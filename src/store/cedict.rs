//! 汉英词库：CC-CEDICT 的解析与只读访问层。
//!
//! 上游是**文本文件**而非 SQLite（与 ECDICT 不同），故本模块含一个构建期的解析器。
//! 格式（<http://cc-cedict.org/wiki/syntax>）：
//!
//! ```text
//! 繁體 简体 [pin1 yin1] /义项一/义项二; 另一种措辞/
//! 皮實 皮实 [pi2 shi5] /(of things) durable/(of people) sturdy; tough/
//! ```
//!
//! - **繁体在前，简体在后**（顺序反了会让整个词库的繁简对调，且不会报错）
//! - 拼音用数字声调，`ü` 写作 `u:`
//! - `/` 分**义项**，`;` 分同一义项内的**措辞**——二者不可混为一谈（见术语表「义项」）
//! - `CL:` 前缀的义项其实是**量词**，不是释义
//! - `#` 开头为注释
//!
//! ## 许可
//!
//! CC-CEDICT 采用 CC BY-SA 协议，与 ECDICT 的 MIT **不同**：由本词库转换而来的
//! 数据文件需保持同协议并署名。本项目代码本身不受影响。详见 docs/adr/0001 的许可讨论。
//!
//! ## 释义为何原样存储
//!
//! 构建期只存原始释义串，义项与量词在**读取时**才解析。理由是解析只发生在用户实际
//! 查看的那一个词条上（查询源永不逐键触发），而序列化 12 万词条要付出编解码成本与
//! 一个新依赖。原样存储亦无损：上游格式演进不会丢信息。

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use super::prefix_upper_bound;
use crate::domain::{Candidate, ChineseEntry, Entry, Headword, Lookup, Query, Sense};

/// 汉英词库的表结构。**构建工具与测试共用此定义**。
///
/// `simplified` **无 UNIQUE 约束**，且这不是疏漏：多音字使一个词头合法地对应多条
/// 词条（`行` 既是 `[hang2]` 又是 `[xing2]`）。加上 UNIQUE 会让建库时静默丢掉其一。
/// 这也是 [`Lookup::Found`] 持有词条**列表**的真正理由——英汉方向永远只回一条，
/// 汉英方向天然可能回多条。
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cedict (
    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL UNIQUE,
    simplified TEXT NOT NULL,
    traditional TEXT NOT NULL,
    pinyin TEXT NOT NULL,
    defs TEXT NOT NULL
);";

/// 索引。`cedict_trad` 让繁体也能被查到——用户可能从繁体文本复制词过来。
/// 词头本身恒为简体（术语表），繁体只是另一条查询入口。
pub const INDEXES: &str = "
CREATE INDEX IF NOT EXISTS cedict_simp ON cedict (simplified);
CREATE INDEX IF NOT EXISTS cedict_trad ON cedict (traditional);";

/// 一行 CC-CEDICT 的原始切分结果，尚未解析释义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLine {
    pub traditional: String,
    pub simplified: String,
    pub pinyin: String,
    /// 原始释义串，不含首尾的 `/`。读取时交由 [`parse_defs`] 解析。
    pub defs_raw: String,
}

/// 解析一行。注释、空行、格式不符者返回 `None`。
///
/// 宽容而非严格：上游有 12 万行，个别畸形行不应让整个构建失败。调用方应统计
/// 跳过的行数——若跳过率异常，说明格式变了，那才是要报警的信号。
pub fn parse_line(line: &str) -> Option<ParsedLine> {
    let line = line.trim();
    // `#! version=1` 这类元数据也以 # 开头，一并跳过。
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // 拼音在方括号内，且**释义在其后**——先按 `[` 切，避免释义里的方括号干扰。
    let (words, rest) = line.split_once('[')?;
    let (pinyin, defs) = rest.split_once(']')?;

    // 繁体在前，简体在后。顺序写反不会报错，只会让整个词库繁简对调——
    // 故此处的顺序由上游格式文档背书，不可凭直觉调整。
    let mut it = words.split_whitespace();
    let traditional = it.next()?.to_string();
    let simplified = it.next()?.to_string();

    let defs = defs.trim();
    let defs_raw = defs.trim_matches('/').trim();
    if defs_raw.is_empty() {
        return None;
    }

    Some(ParsedLine {
        traditional,
        simplified,
        pinyin: pinyin.trim().to_string(),
        defs_raw: defs_raw.to_string(),
    })
}

/// 解析释义串为义项与量词。
///
/// `CL:` 开头的段落是量词而非释义，必须摘出去——把「CL:個|个[ge4]」当作一条英文
/// 释义展示给用户是明显的错误。
pub fn parse_defs(defs_raw: &str) -> (Vec<Sense>, Vec<String>) {
    let mut senses = Vec::new();
    let mut classifiers = Vec::new();

    for part in defs_raw.split('/') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(cl) = part.strip_prefix("CL:") {
            classifiers.extend(parse_classifiers(cl));
            continue;
        }
        // `;` 分隔的是同一义项的不同措辞，不是不同义项。
        let glosses: Vec<String> = part
            .split(';')
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
            .collect();
        if !glosses.is_empty() {
            senses.push(Sense { glosses });
        }
    }

    (senses, classifiers)
}

/// 解析量词段：`座[zuo4],個|个[ge4]` → `["座", "个"]`。
///
/// 取**简体**形式：`個|个` 的竖线左侧是繁体、右侧是简体，与词条本身「词头恒为简体」
/// 保持一致。读音 `[zuo4]` 丢弃——量词只作提示展示，不需要注音。
fn parse_classifiers(s: &str) -> Vec<String> {
    s.split(',')
        .filter_map(|item| {
            // 先去掉读音部分。
            let word = item.split('[').next()?.trim();
            if word.is_empty() {
                return None;
            }
            // `繁|简` → 取简体；无竖线则原样。
            let simp = word.rsplit('|').next()?.trim();
            (!simp.is_empty()).then(|| simp.to_string())
        })
        .collect()
}

/// 由一行构造汉英词条（供测试与读取路径共用）。
pub fn to_entry(p: &ParsedLine) -> Option<ChineseEntry> {
    let (senses, classifiers) = parse_defs(&p.defs_raw);
    // 无义项的词条不该存在——术语表要求「至少一个」义项。
    // 只有量词没有释义的行（若有）在此被丢弃。
    if senses.is_empty() {
        return None;
    }
    Some(ChineseEntry {
        headword: Headword::from_store(&p.simplified),
        traditional: p.traditional.clone(),
        pinyin: p.pinyin.clone(),
        senses,
        classifiers,
    })
}

// ── 只读访问层 ────────────────────────────────────────────────

/// 汉英词库。
pub struct Cedict {
    conn: Connection,
}

impl Cedict {
    /// 以只读方式打开词库。
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("打开汉英词库失败：{}", path.display()))?;
        conn.pragma_update(None, "cache_size", -2000)?;
        conn.pragma_update(None, "journal_mode", "OFF")?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(INDEXES)?;
        Ok(Self { conn })
    }

    /// 查询：简体或繁体精确匹配。
    ///
    /// 返回**多条**词条是常态而非异常：多音字使一个词头对应多个读音与释义。
    /// 界面须能呈现全部，而不是只取第一条——`行[hang2]` 与 `行[xing2]` 谁排前面
    /// 都是错的，用户要的是两个都看到。
    ///
    /// 此处**不做词形还原**：中文不屈折，没有原形可跟。
    pub fn lookup(&self, query: &Query) -> Result<Lookup> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT simplified, traditional, pinyin, defs FROM cedict
             WHERE simplified = ?1 OR traditional = ?1
             ORDER BY id",
        )?;
        let entries: Vec<Entry> = stmt
            .query_map([query.text()], |row| {
                Ok(ParsedLine {
                    simplified: row.get(0)?,
                    traditional: row.get(1)?,
                    pinyin: row.get(2)?,
                    defs_raw: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .filter_map(to_entry)
            .map(Entry::Chinese)
            .collect();

        if entries.is_empty() {
            return Ok(Lookup::NotFound);
        }
        Ok(Lookup::Found {
            entries,
            // 中文无词形变化，此值恒为 false，见术语表「词形变化」。
            via_base_form: false,
        })
    }

    /// 补全：按简体前缀列出候选。
    ///
    /// **排序无法按词频**——CC-CEDICT 不含任何词频信号（ECDICT 有 `frq`/`bnc`，
    /// 这是两个词库的真实差异，不是本层的疏漏）。退而用「短词优先」这个启发式：
    /// 中文里短词通常更常用（`苹果` 之于 `苹果酱`）。这是猜测而非事实，若日后引入
    /// 中文词频数据，此处应改为真正的词频排序。
    ///
    /// 同一简体的多个读音会各占一条候选，故按 `id` 稳定次序去重前先排序。
    pub fn complete(&self, prefix: &str, limit: usize) -> Result<Vec<Candidate>> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return Ok(Vec::new());
        }
        let Some(upper) = prefix_upper_bound(prefix) else {
            return Ok(Vec::new());
        };

        // DISTINCT：多音字在候选列表里只该出现一次——用户还没选词，不必看到读音分歧。
        let mut stmt = self.conn.prepare_cached(
            "SELECT simplified, MIN(defs) FROM cedict
             WHERE simplified >= ?1 AND simplified < ?2
             GROUP BY simplified
             ORDER BY LENGTH(simplified), simplified
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![prefix, upper, limit as i64], |row| {
            let defs: String = row.get(1)?;
            Ok(Candidate {
                headword: Headword::from_store(row.get::<_, String>(0)?),
                preview: first_gloss(&defs),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("汉英补全查询失败")
    }
}

/// 取首个义项的首条措辞，供候选列表的一行预览用。
fn first_gloss(defs_raw: &str) -> Option<String> {
    let (senses, _) = parse_defs(defs_raw);
    senses.first()?.glosses.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 行解析 ────────────────────────────────────────────

    #[test]
    fn 解析官方文档的示例行() {
        let p = parse_line("皮實 皮实 [pi2 shi5] /(of things) durable/(of people) sturdy; tough/")
            .unwrap();
        assert_eq!(p.traditional, "皮實");
        assert_eq!(p.simplified, "皮实");
        assert_eq!(p.pinyin, "pi2 shi5");
        assert_eq!(p.defs_raw, "(of things) durable/(of people) sturdy; tough");
    }

    #[test]
    fn 繁体在前简体在后() {
        // 顺序写反不会报错，只会让整个词库繁简对调——故专门钉死。
        let p = parse_line("蘋果 苹果 [ping2 guo3] /apple/").unwrap();
        assert_eq!(p.traditional, "蘋果", "第一列必须是繁体");
        assert_eq!(p.simplified, "苹果", "第二列必须是简体");
    }

    #[test]
    fn 繁简相同时照存不作推断() {
        let p = parse_line("你好 你好 [ni3 hao3] /hello/hi/").unwrap();
        assert_eq!(p.traditional, "你好");
        assert_eq!(p.simplified, "你好");
    }

    #[test]
    fn 跳过注释与空行() {
        assert!(parse_line("# 这是注释").is_none());
        assert!(parse_line("#! version=1").is_none(), "元数据行也以 # 开头");
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
    }

    #[test]
    fn 跳过畸形行而不panic() {
        // 12 万行里的个别畸形行不该让整个构建失败。
        assert!(parse_line("没有方括号 也没有释义").is_none());
        assert!(parse_line("蘋果 苹果 [ping2 guo3]").is_none(), "无释义");
        assert!(parse_line("蘋果 苹果 [ping2 guo3] //").is_none(), "空释义");
        assert!(parse_line("只有一列 [x] /y/").is_none(), "缺简体列");
    }

    // ── 义项与措辞 ────────────────────────────────────────

    #[test]
    fn 斜杠分义项分号分措辞() {
        // 这是最容易搞混的一条：`;` 不产生新义项。
        let (senses, _) = parse_defs("(of things) durable/(of people) sturdy; tough");
        assert_eq!(senses.len(), 2, "两个义项");
        assert_eq!(senses[0].glosses, vec!["(of things) durable"]);
        assert_eq!(
            senses[1].glosses,
            vec!["(of people) sturdy", "tough"],
            "同一义项的两种措辞，不是两个义项"
        );
    }

    #[test]
    fn 单义项单措辞() {
        let (senses, _) = parse_defs("apple");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].glosses, vec!["apple"]);
    }

    // ── 量词 ──────────────────────────────────────────────

    #[test]
    fn 量词摘出而不混入释义() {
        // 把「CL:個|个[ge4]」当英文释义展示给用户是明显的错误。
        let (senses, cl) = parse_defs("apple/CL:個|个[ge4],顆|颗[ke1]");
        assert_eq!(senses.len(), 1, "量词不算义项");
        assert_eq!(senses[0].glosses, vec!["apple"]);
        assert_eq!(cl, vec!["个", "颗"], "取简体，丢读音");
    }

    #[test]
    fn 量词无繁简差异时原样保留() {
        let (_, cl) = parse_defs("temple/CL:座[zuo4]");
        assert_eq!(cl, vec!["座"]);
    }

    #[test]
    fn 无量词时为空() {
        let (_, cl) = parse_defs("hello/hi");
        assert!(cl.is_empty());
    }

    // ── 构造词条 ──────────────────────────────────────────

    #[test]
    fn 构造完整汉英词条() {
        let p = parse_line("蘋果 苹果 [ping2 guo3] /apple/CL:個|个[ge4],顆|颗[ke1]/").unwrap();
        let e = to_entry(&p).unwrap();
        assert_eq!(e.headword.as_str(), "苹果", "词头恒为简体");
        assert_eq!(e.traditional, "蘋果");
        assert_eq!(e.pinyin, "ping2 guo3");
        assert_eq!(e.senses[0].glosses, vec!["apple"]);
        assert_eq!(e.classifiers, vec!["个", "颗"]);
    }

    #[test]
    fn 只有量词没有释义的行被丢弃() {
        // 术语表要求词条「至少一个」义项。
        let p = parse_line("個 个 [ge4] /CL:個|个[ge4]/").unwrap();
        assert!(to_entry(&p).is_none());
    }

    #[test]
    fn 拼音保留数字声调与u冒号写法() {
        // `ü` 在 CC-CEDICT 中写作 `u:`，原样保留——转换是展示层的事，不是解析层的。
        let p = parse_line("女 女 [nu:3] /female/").unwrap();
        assert_eq!(p.pinyin, "nu:3");
    }

    // ── 以下形状由真实词库（124,732 条）实测发现，非臆造 ──────

    #[test]
    fn 释义中的方括号不干扰拼音切分() {
        // 真实词库中有 18,054 行的释义含方括号（交叉引用语法）。若从右侧找 `[`
        // 或用贪婪匹配取到最后一个 `]`，这 14.5% 的词条会被解析成垃圾——
        // 而全部单元测试仍会绿，因为臆造的示例行里没有方括号。
        let p = parse_line(
            "亞當·斯密 亚当·斯密 [Ya4 dang1 · Si1 mi4] /Adam Smith, author of 國富論|国富论[Guo2 fu4 lun4]/",
        )
        .unwrap();
        assert_eq!(p.pinyin, "Ya4 dang1 · Si1 mi4", "拼音必须取第一对方括号");
        assert!(
            p.defs_raw.contains("國富論|国富论[Guo2 fu4 lun4]"),
            "释义原样保留"
        );
    }

    #[test]
    fn 成语拼音含逗号() {
        // 真实词库中 457 条。成语的中文含逗号，拼音镜像之。
        let p = parse_line(
            "一不做，二不休 一不做，二不休 [yi1 bu4 zuo4 , er4 bu4 xiu1] /in for a penny, in for a pound/",
        )
        .unwrap();
        assert_eq!(p.pinyin, "yi1 bu4 zuo4 , er4 bu4 xiu1");
        assert_eq!(p.simplified, "一不做，二不休");
    }

    #[test]
    fn 人名拼音含间隔号() {
        // 真实词库中 160 条。间隔号分隔外国人名的名与姓。
        let p = parse_line("亨利·哈德遜 亨利·哈德逊 [Heng1 li4 · Ha1 de2 xun4] /Henry Hudson/")
            .unwrap();
        assert_eq!(p.pinyin, "Heng1 li4 · Ha1 de2 xun4");
        assert_eq!(p.simplified, "亨利·哈德逊");
    }

    #[test]
    fn 义项内的逗号不切分措辞() {
        // 只有 `;` 分措辞。`in for a penny, in for a pound` 是**一条**措辞。
        let (senses, _) = parse_defs("in for a penny, in for a pound");
        assert_eq!(senses.len(), 1);
        assert_eq!(senses[0].glosses, vec!["in for a penny, in for a pound"]);
    }

    // ── 存储层 ────────────────────────────────────────────

    fn seed(db: &Cedict, lines: &[&str]) {
        for line in lines {
            let p = parse_line(line).expect("fixture 行必须可解析");
            db.conn
                .execute(
                    "INSERT INTO cedict (simplified, traditional, pinyin, defs)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![p.simplified, p.traditional, p.pinyin, p.defs_raw],
                )
                .unwrap();
        }
    }

    fn q(s: &str) -> Query {
        Query::new(s).unwrap()
    }

    fn zh(entry: &Entry) -> &ChineseEntry {
        match entry {
            Entry::Chinese(e) => e,
            Entry::English(_) => panic!("汉英词库不该产出英汉词条"),
        }
    }

    #[test]
    fn 查简体命中() {
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple/CL:個|个[ge4]/"]);
        let Lookup::Found { entries, .. } = db.lookup(&q("苹果")).unwrap() else {
            panic!("应当命中");
        };
        let e = zh(&entries[0]);
        assert_eq!(e.headword.as_str(), "苹果");
        assert_eq!(e.pinyin, "ping2 guo3");
        assert_eq!(e.classifiers, vec!["个"]);
    }

    #[test]
    fn 查繁体也命中() {
        // 用户可能从繁体文本里复制词过来。词头仍返回简体。
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple/"]);
        let Lookup::Found { entries, .. } = db.lookup(&q("蘋果")).unwrap() else {
            panic!("繁体应当命中");
        };
        assert_eq!(zh(&entries[0]).headword.as_str(), "苹果", "词头恒为简体");
    }

    #[test]
    fn 多音字返回多条词条() {
        // ECDICT 的 word 是 UNIQUE，汉英不是——这是两个词库的结构性差异。
        // 只取第一条是错的：用户要的是两个读音都看到。
        let db = Cedict::in_memory().unwrap();
        seed(
            &db,
            &[
                "行 行 [hang2] /row/line/profession/",
                "行 行 [xing2] /to walk/to go/OK/",
            ],
        );
        let Lookup::Found { entries, .. } = db.lookup(&q("行")).unwrap() else {
            panic!("应当命中");
        };
        assert_eq!(entries.len(), 2, "多音字必须全部返回");
        let pinyins: Vec<_> = entries.iter().map(|e| zh(e).pinyin.as_str()).collect();
        assert_eq!(pinyins, vec!["hang2", "xing2"]);
    }

    #[test]
    fn 中文无词形还原() {
        // 「苹果的过去式」不是有意义的问题，via_base_form 恒为 false。
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple/"]);
        let Lookup::Found { via_base_form, .. } = db.lookup(&q("苹果")).unwrap() else {
            panic!("应当命中");
        };
        assert!(!via_base_form);
    }

    #[test]
    fn 未收录返回未找到() {
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple/"]);
        assert_eq!(db.lookup(&q("鎢鋼")).unwrap(), Lookup::NotFound);
    }

    #[test]
    fn 补全短词优先() {
        // CC-CEDICT 无词频，只能用「短词优先」这个启发式。
        let db = Cedict::in_memory().unwrap();
        seed(
            &db,
            &[
                "蘋果醬 苹果酱 [ping2 guo3 jiang4] /apple jam/",
                "蘋果 苹果 [ping2 guo3] /apple/",
                "蘋果樹 苹果树 [ping2 guo3 shu4] /apple tree/",
            ],
        );
        let got = db.complete("苹果", 10).unwrap();
        let words: Vec<_> = got.iter().map(|c| c.headword.as_str()).collect();
        assert_eq!(words, vec!["苹果", "苹果树", "苹果酱"], "短词在前");
    }

    #[test]
    fn 补全时多音字只占一条() {
        // 用户还没选词，不必在候选列表里看到读音分歧。
        let db = Cedict::in_memory().unwrap();
        seed(
            &db,
            &[
                "行 行 [hang2] /row/line/",
                "行 行 [xing2] /to walk/",
                "行動 行动 [xing2 dong4] /action/",
            ],
        );
        let got = db.complete("行", 10).unwrap();
        let words: Vec<_> = got.iter().map(|c| c.headword.as_str()).collect();
        assert_eq!(words, vec!["行", "行动"], "行 只出现一次");
    }

    #[test]
    fn 补全预览取首义项首措辞() {
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple; malus/fruit/"]);
        let got = db.complete("苹", 10).unwrap();
        assert_eq!(got[0].preview.as_deref(), Some("apple"));
    }

    #[test]
    fn 空前缀返回空() {
        let db = Cedict::in_memory().unwrap();
        seed(&db, &["蘋果 苹果 [ping2 guo3] /apple/"]);
        assert!(db.complete("", 10).unwrap().is_empty());
        assert!(db.complete("   ", 10).unwrap().is_empty());
    }
}
