//! 英汉词库：自建精简 ECDICT 库的只读访问层。
//!
//! 本层是**只读**的：词库随程序分发、可整体替换升级，用户数据在另一个库里，
//! 二者靠 `ATTACH` 跨库查询。这个文件边界即是生命周期边界。
//!
//! 词库由 `examples/build_ecdict.rs` 从 `ecdict.csv` 构建，**不用 ECDICT 官方的
//! SQLite 发布**（207MB 压缩 / 600MB+ 解压），见 docs/adr/0010。

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, Row};

use crate::domain::{Candidate, EnglishEntry, Entry, Headword, Inflections, Lookup, Query};

/// 自建英汉词库的表结构。**构建工具与测试共用此定义**。
///
/// 这是发货的 schema，不是 ECDICT 上游 `stardict.py` 的。相对上游丢弃的列
/// （见 docs/adr/0010）：`detail`（JSON 扩展）、`audio`（官方未实现）——本项目无
/// 用途且是体积大头；`collins`/`oxford`/`tag`——当前无功能依赖。
///
/// 表名沿用 `stardict` 以便与上游工具链对照。保留 `bnc`/`frq`：补全按词频排序全靠
/// 它们。保留 `sw`：前缀补全的索引列，值由 [`stripped_word`] 在构建期算出。
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS stardict (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL UNIQUE,
    word VARCHAR(64) COLLATE NOCASE NOT NULL UNIQUE,
    sw VARCHAR(64) COLLATE NOCASE NOT NULL,
    phonetic VARCHAR(64),
    definition TEXT,
    translation TEXT,
    pos VARCHAR(16),
    bnc INTEGER DEFAULT(NULL),
    frq INTEGER DEFAULT(NULL),
    exchange TEXT
);";

/// 索引。构建期应在**灌完数据后**再建——先建索引再逐行插入，等于每插一行维护一次
/// B 树，77 万行下慢一个数量级。
///
/// `stardict_3 (sw, word)` 是补全的命脉，见 `complete` 与其查询计划测试。
pub const INDEXES: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS stardict_2 ON stardict (word);
CREATE INDEX IF NOT EXISTS stardict_3 ON stardict (sw, word COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS sd_1 ON stardict (word COLLATE NOCASE);";

/// 英汉词库。
pub struct Ecdict {
    conn: Connection,
}

/// 查询词条时取的列。`detail` 与 `audio` 不取：
/// `detail` 是 JSON 扩展、`audio` 官方尚未实现，二者在本项目均无用途，
/// 且是词库体积的大头之一（见 docs/adr/0001 的后果）。
const ENTRY_COLS: &str = "word, phonetic, definition, translation, pos, exchange";

impl Ecdict {
    /// 以只读方式打开词库。
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("打开英汉词库失败：{}", path.display()))?;
        Self::tune(&conn)?;
        Ok(Self { conn })
    }

    /// 内存库（仅测试用）。
    ///
    /// 用的是 [`SCHEMA`] 与 [`INDEXES`]——即**实际发货的表结构**，而非 ECDICT 上游的。
    /// 测试跑在一个不发货的 schema 上等于没测；索引也必须建，否则查询计划测试无意义。
    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(INDEXES)?;
        Ok(Self { conn })
    }

    /// 内存参数。
    ///
    /// 驻留内存与 76 万词条的词库总量脱钩，靠的是 SQLite 按需读页——这一点不变
    /// （docs/adr/0006）。变的是缓存上限：项目已不再以「后台内存尽可能小」为目标，
    /// 故把页缓存从 2MB 放宽到 32MB，让补全的热页留在内存里。
    ///
    /// 补全的最坏情况是单字母前缀命中约 5 万行且 `ORDER BY frq` 用不上索引，
    /// SQLite 必须全排一遍。缓存放宽买到的正是这条路径上的重复代价——**页留住了，
    /// 第二次起不必重读**。上限仍是显式钳死的，不是不设防。
    fn tune(conn: &Connection) -> Result<()> {
        // 负值 = KB。-32000 即上限 32MB 页缓存（约为英汉词库体积的 20%）。
        conn.pragma_update(None, "cache_size", -32000)?;
        // 只读库无需回滚日志。
        conn.pragma_update(None, "journal_mode", "OFF")?;
        Ok(())
    }

    /// 按词头精确查（大小写不敏感——`word` 列声明为 COLLATE NOCASE）。
    fn exact(&self, word: &str) -> Result<Option<EnglishEntry>> {
        let sql = format!("SELECT {ENTRY_COLS} FROM stardict WHERE word = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([word])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_entry(row)?)),
            None => Ok(None),
        }
    }

    /// 查询：先精确匹配，命中后视情况跟随词形变化落到原形。
    ///
    /// 本词库只产出**英汉词条**——它是离线词典在英汉方向上的供给。
    pub fn lookup(&self, query: &Query) -> Result<Lookup> {
        let Some(entry) = self.exact(query.text())? else {
            return Ok(Lookup::NotFound);
        };

        // 词条自身有中文释义 → 直接给它，不跟随原形。
        // 例：`tried` 在 ECDICT 中自带「try 的过去式」，这已是用户要的答案。
        if entry.zh_definition.is_some() {
            return Ok(Lookup::Found {
                entries: vec![Entry::English(entry)],
                via_base_form: false,
            });
        }

        // 自身无释义但指向原形 → 跟随。这是词形还原真正起作用的路径：
        // 变化形态的行往往是「空壳」，释义只挂在原形上。
        if let Some(base) = &entry.inflections.base_form {
            if let Some(base_entry) = self.exact(base.as_str())? {
                return Ok(Lookup::Found {
                    entries: vec![Entry::English(base_entry)],
                    via_base_form: true,
                });
            }
        }

        // 既无释义也无原形可跟 —— 词条存在但空。仍视为命中，由界面呈现「查无释义」，
        // 而非谎称未收录：它确实在词库里。
        Ok(Lookup::Found {
            entries: vec![Entry::English(entry)],
            via_base_form: false,
        })
    }

    /// 补全：按前缀列出候选，**词频高的在前**。
    ///
    /// 两个不显然的点：
    ///
    /// 1. 走 `sw` 列而非 `word`。`sw` 是 ECDICT 预先算好的「只留字母数字的小写形式」，
    ///    且索引 `stardict_3` 正是建在 `(sw, word)` 上——词库作者就是为前缀匹配备的它。
    ///    用 `word LIKE 'app%'` 会绕开这个索引。
    ///
    /// 2. `frq` 可为 NULL（schema 的 `DEFAULT(NULL)`），而 SQLite 的 ASC 排序把
    ///    **NULL 排在最前**。若直接 `ORDER BY frq`，词频未知的生僻词会被顶到候选首位，
    ///    与「高频优先」恰好相反。故先按「频次是否已知」分组，再按频次升序
    ///    （ECDICT 的 frq 是排名：越小越高频）。
    pub fn complete(&self, prefix: &str, limit: usize) -> Result<Vec<Candidate>> {
        let sw = stripped_word(prefix);
        if sw.is_empty() {
            return Ok(Vec::new());
        }
        let Some(upper) = prefix_upper_bound(&sw) else {
            return Ok(Vec::new());
        };

        // 用半开区间 [sw, upper) 而非 LIKE：范围查询确定性地走 stardict_3 索引，
        // 而 LIKE 在 COLLATE NOCASE 列上能否被优化成范围扫描取决于编译期设置。
        let sql = "
            SELECT word, translation FROM stardict
            WHERE sw >= ?1 AND sw < ?2
            ORDER BY (frq IS NULL OR frq = 0), frq, sw
            LIMIT ?3";
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(rusqlite::params![sw, upper, limit as i64], |row| {
            Ok(Candidate {
                headword: Headword::from_store(row.get::<_, String>(0)?),
                preview: row.get::<_, Option<String>>(1)?.map(|t| first_line(&t)),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("补全查询失败")
    }
}

/// 把一行映射为英汉词条。
fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<EnglishEntry> {
    let exchange: Option<String> = row.get(5)?;
    Ok(EnglishEntry {
        headword: Headword::from_store(row.get::<_, String>(0)?),
        phonetic: row.get(1)?,
        // 列序对应 ENTRY_COLS。ECDICT 的 definition 是英英、translation 是中文——
        // 二者是词条上并存的两个字段，不是同一内容的两种语言，故绝不可互换。
        en_definition: row.get(2)?,
        zh_definition: row.get(3)?,
        pos: row.get(4)?,
        inflections: exchange.as_deref().map(parse_exchange).unwrap_or_default(),
    })
}

/// 解析 ECDICT 的 `exchange` 字段。
///
/// 格式为 `码:值` 以 `/` 分隔，如 `try` → `p:tried/d:tried/i:trying/3:tries`，
/// 而 `tried` → `0:try/1:p`。码含义：
///
/// | 码 | 含义 | | 码 | 含义 |
/// |---|---|---|---|---|
/// | `p` | 过去式 | | `s` | 复数 |
/// | `d` | 过去分词 | | `r` | 比较级 |
/// | `i` | 现在分词 | | `t` | 最高级 |
/// | `3` | 第三人称单数 | | `0` | **原形** |
/// | `1` | 原形的变换类型 | | | |
///
/// 只有 `0`（原形）与派生形态被采纳；`1` 是原形的变换类型标记（值形如 `p`、`d`），
/// 不是一个词，故丢弃——把它当词头会造出 `Headword("p")` 这种垃圾。
fn parse_exchange(s: &str) -> Inflections {
    let mut out = Inflections::default();
    for part in s.split('/') {
        let Some((code, value)) = part.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match code {
            "0" => out.base_form = Some(Headword::from_store(value)),
            "p" | "d" | "i" | "3" | "r" | "t" | "s" => {
                out.derived.push(Headword::from_store(value));
            }
            // "1" 及未知码：丢弃。
            _ => {}
        }
    }
    out
}

/// 归一化为 ECDICT `sw` 列的形式：只留字母与数字，转小写。
///
/// **构建期与运行期必须用同一个定义**：`sw` 的值在建库时算好并落盘，查询时按同样的
/// 规则算前缀去比对。两处若有分歧，补全会静默失效（查不到，但不报错）。故此函数
/// 公开给构建工具（`examples/build_ecdict.rs`）复用，而非各自实现一份。
///
/// 改动本函数即意味着**存量词库全部作废**，必须重建。
pub fn stripped_word(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 半开区间的上界：把末位字符 +1。
///
/// 输入恒为 `stripped_word` 的产物（只含 `[a-z0-9]`），故末位不会是 `char::MAX`，
/// 直接 +1 安全。`'z'` → `'{'`（ASCII 0x7B）虽非字母，但作为排序上界完全正确。
fn prefix_upper_bound(sw: &str) -> Option<String> {
    let mut chars: Vec<char> = sw.chars().collect();
    let last = chars.pop()?;
    let next = char::from_u32(last as u32 + 1)?;
    chars.push(next);
    Some(chars.into_iter().collect())
}

/// 取首行，供候选列表的一行预览用。
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixture 行：(词头, 中文释义, exchange, 词频排名)。
    /// 不叫 `Row`——那个名字已被 `rusqlite::Row` 占用。
    type Fixture<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<i64>);

    fn seed(db: &Ecdict, rows: &[Fixture<'_>]) {
        for (word, translation, exchange, frq) in rows {
            db.conn
                .execute(
                    "INSERT INTO stardict (word, sw, translation, exchange, frq)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![word, stripped_word(word), translation, exchange, frq],
                )
                .unwrap();
        }
    }

    fn q(s: &str) -> Query {
        Query::new(s).unwrap()
    }

    /// 解包为英汉词条。本词库只产出英汉词条，拿到别的就是 bug。
    fn en(entry: &Entry) -> &EnglishEntry {
        match entry {
            Entry::English(e) => e,
            Entry::Chinese(_) => panic!("英汉词库不该产出汉英词条"),
        }
    }

    // ── 词形还原 ──────────────────────────────────────────

    #[test]
    fn 变化形态自带释义时直接返回自身() {
        let db = Ecdict::in_memory().unwrap();
        seed(
            &db,
            &[(
                "tried",
                Some("v. 尝试( try的过去式 )"),
                Some("0:try/1:p"),
                None,
            )],
        );
        let Lookup::Found {
            entries,
            via_base_form,
        } = db.lookup(&q("tried")).unwrap()
        else {
            panic!("应当命中");
        };
        assert_eq!(en(&entries[0]).headword.as_str(), "tried");
        assert!(!via_base_form, "自身有释义，不应跟随原形");
    }

    #[test]
    fn 变化形态无释义时跟随到原形() {
        let db = Ecdict::in_memory().unwrap();
        seed(
            &db,
            &[
                ("tries", None, Some("0:try/1:3"), None),
                ("try", Some("v. 尝试"), Some("p:tried/3:tries"), Some(500)),
            ],
        );
        let Lookup::Found {
            entries,
            via_base_form,
        } = db.lookup(&q("tries")).unwrap()
        else {
            panic!("应当命中");
        };
        assert_eq!(en(&entries[0]).headword.as_str(), "try");
        assert_eq!(en(&entries[0]).zh_definition.as_deref(), Some("v. 尝试"));
        assert!(via_base_form, "界面需据此提示「显示的是 try 的词条」");
    }

    #[test]
    fn 空壳词条且原形缺失时仍视为命中() {
        // 词条确实在词库里，只是没释义——不可谎称未收录。
        let db = Ecdict::in_memory().unwrap();
        seed(&db, &[("weirdo", None, Some("0:nonexistent"), None)]);
        assert!(matches!(
            db.lookup(&q("weirdo")).unwrap(),
            Lookup::Found { .. }
        ));
    }

    #[test]
    fn 未收录返回未找到() {
        let db = Ecdict::in_memory().unwrap();
        seed(&db, &[("apple", Some("n. 苹果"), None, Some(1000))]);
        assert_eq!(db.lookup(&q("zzzzz")).unwrap(), Lookup::NotFound);
    }

    #[test]
    fn 查询大小写不敏感() {
        let db = Ecdict::in_memory().unwrap();
        seed(&db, &[("Apple", Some("n. 苹果"), None, Some(1000))]);
        assert!(matches!(
            db.lookup(&q("APPLE")).unwrap(),
            Lookup::Found { .. }
        ));
    }

    // ── exchange 解析 ─────────────────────────────────────

    #[test]
    fn 解析原形与派生形态() {
        let inf = parse_exchange("p:tried/d:tried/i:trying/3:tries");
        assert!(inf.base_form.is_none(), "try 自己就是原形");
        assert_eq!(inf.derived.len(), 4);
    }

    #[test]
    fn 解析变化形态指向原形() {
        let inf = parse_exchange("0:try/1:p");
        assert_eq!(inf.base_form.as_ref().unwrap().as_str(), "try");
        assert!(inf.derived.is_empty(), "码 1 是变换类型标记，不是词");
    }

    #[test]
    fn 忽略畸形与未知码() {
        let inf = parse_exchange("garbage/9:x/0:/p:ran");
        assert!(inf.base_form.is_none(), "0 的值为空应被丢弃");
        assert_eq!(inf.derived, vec![Headword::from_store("ran")]);
    }

    // ── 补全 ──────────────────────────────────────────────

    #[test]
    fn 补全按词频排序而非字典序() {
        let db = Ecdict::in_memory().unwrap();
        seed(
            &db,
            &[
                ("appalachia", Some("n. 阿巴拉契亚"), None, Some(90000)),
                ("apple", Some("n. 苹果"), None, Some(1000)),
                ("applique", Some("n. 贴花"), None, Some(50000)),
            ],
        );
        let got = db.complete("app", 10).unwrap();
        let words: Vec<_> = got.iter().map(|c| c.headword.as_str()).collect();
        assert_eq!(
            words,
            vec!["apple", "applique", "appalachia"],
            "高频词必须在前——字典序会让 appalachia 居首"
        );
    }

    #[test]
    fn 词频未知的词排在已知词之后() {
        // schema 中 frq DEFAULT(NULL)，而 SQLite 的 ASC 把 NULL 排最前。
        // 若直接 ORDER BY frq，生僻词会顶到候选首位——与「高频优先」恰好相反。
        let db = Ecdict::in_memory().unwrap();
        seed(
            &db,
            &[
                ("appulse", Some("n. 接近"), None, None), // 词频未知
                ("apply", Some("v. 申请"), None, Some(800)),
                ("appose", Some("v. 并置"), None, Some(0)), // 0 亦视为未知
            ],
        );
        let got = db.complete("app", 10).unwrap();
        assert_eq!(
            got[0].headword.as_str(),
            "apply",
            "唯一词频已知的词必须居首"
        );
    }

    #[test]
    fn 补全走归一化前缀() {
        let db = Ecdict::in_memory().unwrap();
        seed(&db, &[("New York", Some("n. 纽约"), None, Some(100))]);
        // 用户输入含空格与大写，sw 列存的是 "newyork"。
        let got = db.complete("New Yo", 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].headword.as_str(), "New York");
    }

    #[test]
    fn 补全尊重数量上限() {
        let db = Ecdict::in_memory().unwrap();
        let rows: Vec<_> = (0..50)
            .map(|i| (format!("app{i:02}"), format!("n. 第{i}个")))
            .collect();
        for (w, t) in &rows {
            db.conn
                .execute(
                    "INSERT INTO stardict (word, sw, translation) VALUES (?1, ?2, ?3)",
                    rusqlite::params![w, stripped_word(w), t],
                )
                .unwrap();
        }
        assert_eq!(db.complete("app", 20).unwrap().len(), 20);
    }

    #[test]
    fn 前缀不含字母数字时返回空() {
        let db = Ecdict::in_memory().unwrap();
        seed(&db, &[("apple", Some("n. 苹果"), None, Some(1))]);
        assert!(db.complete("!!!", 10).unwrap().is_empty());
        assert!(db.complete("", 10).unwrap().is_empty());
    }

    #[test]
    fn 补全预览只取首行() {
        let db = Ecdict::in_memory().unwrap();
        seed(
            &db,
            &[(
                "apple",
                Some("n. 苹果\nn. 苹果树\nn. 苹果公司"),
                None,
                Some(1),
            )],
        );
        let got = db.complete("app", 10).unwrap();
        assert_eq!(got[0].preview.as_deref(), Some("n. 苹果"));
    }

    // ── 前缀上界 ──────────────────────────────────────────

    #[test]
    fn 前缀上界为末位加一() {
        assert_eq!(prefix_upper_bound("app").as_deref(), Some("apq"));
        assert_eq!(prefix_upper_bound("z").as_deref(), Some("{"));
        assert_eq!(prefix_upper_bound(""), None);
    }

    // ── 查询计划 ──────────────────────────────────────────

    #[test]
    fn 补全走_sw_索引而非全表扫描() {
        // 76 万词条上全表扫描 = 每次按键卡顿。这条断言把「补全用了 ECDICT 预建的
        // stardict_3(sw, word) 索引」从注释里的说法变成可执行的证据。
        // 若有人把范围查询「优化」成 LIKE，或改了 ORDER BY 导致索引失效，此处会红。
        let db = Ecdict::in_memory().unwrap();
        let plan: Vec<String> = db
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT word, translation FROM stardict
                 WHERE sw >= ?1 AND sw < ?2
                 ORDER BY (frq IS NULL OR frq = 0), frq, sw
                 LIMIT ?3",
            )
            .unwrap()
            // EXPLAIN QUERY PLAN 仍需绑定占位符；值不影响所选计划。
            .query_map(rusqlite::params!["app", "apq", 20i64], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        let plan = plan.join("\n");
        assert!(
            plan.contains("stardict_3"),
            "补全必须走 stardict_3 索引，实际计划：\n{plan}"
        );
        assert!(
            !plan.contains("SCAN stardict"),
            "补全不得全表扫描，实际计划：\n{plan}"
        );
    }
}
