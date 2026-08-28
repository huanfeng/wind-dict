//! 字形库：汉字的部首、笔画与繁简对应，源自 Unicode 官方的 Unihan 数据库。
//!
//! ## 为什么单独一个库文件
//!
//! 与 `ecdict.db` / `cedict.db` 分开，理由与那两者彼此分开是同一条：**上游不同、
//! 协议不同、升级节奏不同**。Unihan 是 Unicode License（实质等同 MIT），CC-CEDICT
//! 是 CC BY-SA——两份数据一旦合进同一个文件，署名义务就再也拆不开了。Unicode 每年
//! 发一版，届时替换这一个文件即可，碰都不碰另外两个。
//!
//! ## 数据来源与三个坑
//!
//! 字段取自 `Unihan_IRGSources.txt`。**不是** `Unihan_RadicalStrokeCounts.txt`——
//! 名字最像的那个文件如今只剩 Adobe 的字段，`kRSUnicode` 不在里面。这一条只能靠
//! 实测发现，照着字段名猜文件必然扑空。
//!
//! `kRSUnicode` 的三个边界情况，天真的解析器每个都会栽（数量为实测值）：
//!
//! | 情况 | 数量 | 样本 |
//! |---|---|---|
//! | 撇号后缀（简化部首形） | 3,877 | `196'.5`（鸟部）、`212'''.5` |
//! | **负**的部首外笔画 | 45 | `125.-1`、`125.-2` |
//! | 多值（空格分隔多种分析） | 448 | `1.2 75.-1` |
//!
//! 撇号绝不能忽略：常用简体字**全部**走撇号形式（语 `149'`、钱 `167'`、门 `169'`、
//! 鸟 `196'`），忽略它会让这 3,877 个字显示错误的部首——`语` 的部首会变成「言」。
//! 负笔画则意味着该字段必须有符号：`u8` 会在那 45 个字上静默回绕成 255。
//!
//! ## 许可
//!
//! Unihan 采用 **Unicode License V3**：可用、可改、可再分发、可商用，唯一义务是随
//! 数据或文档保留版权与许可声明。与 ECDICT 的 MIT 同级，不引入 CC-CEDICT 那样的
//! 传染性条款。

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::domain::{CharTier, Glyph};

/// 字形库的表结构。**构建工具与测试共用此定义**。
///
/// `ch` 是主键，且这与汉英库恰成对照：那里 `simplified` 无 UNIQUE，因为多音字让
/// 一个词头合法地对应多条词条；这里不存在「同一个字两副字形」，故主键是诚实的。
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS glyph (
    ch            TEXT    PRIMARY KEY NOT NULL,
    radical       TEXT    NOT NULL,
    radical_no    INTEGER NOT NULL,
    extra_strokes INTEGER NOT NULL,
    total_strokes INTEGER NOT NULL,
    simplified    TEXT    NOT NULL DEFAULT '',
    traditional   TEXT    NOT NULL DEFAULT '',
    readings      TEXT    NOT NULL DEFAULT '',
    tier          INTEGER
);";

/// 康熙 214 部首的字形，按部首号 1–214 顺序排列。
///
/// 取自 Unicode 的**康熙部首区** `U+2F00..=U+2FD5`（该区按序即部首 1–214）逐个
/// NFKC 归一得到的汉字。之所以固化成字面量而非运行时归一：归一要拉进一个
/// Unicode 规范化依赖，而这 214 个部首自 1716 年定谱以来不曾变过。
///
/// 固化的风险是它可能与数据漂移，故构建工具**必须**调用 [`verify_kangxi_table`]
/// 拿 Unihan 数据反验一遍——写错了会当场让建库失败，而不是静默产出错部首。
///
/// 不用「部首外笔画为 0 的字即该部首」这条规律直接从数据推：它对简化形成立，对
/// 基本形却会挑中低码位的罕见异体字（部首 102 会得到 `曱` 而非 `田`，182 会得到
/// `凬` 而非 `風`）。
pub const KANGXI_RADICALS: &str = "一丨丶丿乙亅二亠人儿入八冂冖冫几凵刀力勹匕匚匸十卜卩厂厶又口囗土士夂夊夕大女子宀寸小尢尸屮山巛工己巾干幺广廴廾弋弓彐彡彳心戈戶手支攴文斗斤方无日曰月木欠止歹殳毋比毛氏气水火爪父爻爿片牙牛犬玄玉瓜瓦甘生用田疋疒癶白皮皿目矛矢石示禸禾穴立竹米糸缶网羊羽老而耒耳聿肉臣自至臼舌舛舟艮色艸虍虫血行衣襾見角言谷豆豕豸貝赤走足身車辛辰辵邑酉釆里金長門阜隶隹雨靑非面革韋韭音頁風飛食首香馬骨高髟鬥鬯鬲鬼魚鳥鹵鹿麥麻黃黍黑黹黽鼎鼓鼠鼻齊齒龍龜龠";

/// 一条 `kRSUnicode` 的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadicalStroke {
    /// 康熙部首号，1–214。
    pub radical_no: u8,
    /// 撇号数：0 = 康熙基本形，1 = 中国简化形，2 及以上 = 日本新字体。
    ///
    /// 保留层级而非压成布尔，是因为 `212'`（龙）、`212''`（竜）、`212'''` 是三个
    /// 不同的字形，压平会把它们混作一谈。
    pub simplified_level: u8,
    /// 部首外笔画，可为负。
    pub extra_strokes: i8,
}

/// 解析 `kRSUnicode` 的值，形如 `144.0`、`196'.5`、`125.-1`。
///
/// 字段可能是空格分隔的**多个**分析（实测 448 字），此处只取第一个：那是 Unihan
/// 列出的主分析。全都留下会让一个字有两个部首，界面无从选择。
pub fn parse_rs(field: &str) -> Option<RadicalStroke> {
    let first = field.split_whitespace().next()?;
    let (head, tail) = first.split_once('.')?;
    let apostrophes = head.bytes().filter(|b| *b == b'\'').count();
    let radical_no: u8 = head.trim_end_matches('\'').parse().ok()?;
    if !(1..=214).contains(&radical_no) {
        return None;
    }
    Some(RadicalStroke {
        radical_no,
        simplified_level: u8::try_from(apostrophes).ok()?,
        extra_strokes: tail.parse().ok()?,
    })
}

/// 从 `kXHC1983` / `kTGHZ2013` / `kMandarin` 的一个值里取出读音列表。
///
/// 前两者的每项形如 `0442.080:háng`，冒号前是原字典的页码位置；`kMandarin` 则是
/// 光秃秃的 `xíng`。页码**可以逗号分隔多个**（实测 143 条，如 `049.010,049.020:chǒu`），
/// 故按最后一个冒号切，不按第一个——按第一个会把 `049.020:chǒu` 整段当成读音。
///
/// 顺带去重：同一个读音在不同页出现是可能的，展示两遍是噪音。
pub fn parse_readings(field: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in field.split_whitespace() {
        let py = tok.rsplit_once(':').map_or(tok, |(_, p)| p);
        if !py.is_empty() && !out.iter().any(|x| x == py) {
            out.push(py.to_string());
        }
    }
    out
}

/// 从 `kHanyuPinlu` 里取「读音 → 出现次数」，用于给读音排序。
///
/// 值形如 `xíng(2943) háng(218)`。它只覆盖 3,799 字，故仅作排序依据、不作读音来源：
/// 拿它当来源会让绝大多数字一个读音都没有。
pub fn parse_pinlu(field: &str) -> Vec<(String, u32)> {
    field
        .split_whitespace()
        .filter_map(|tok| {
            let (py, rest) = tok.split_once('(')?;
            let n = rest.strip_suffix(')')?.parse().ok()?;
            Some((py.to_string(), n))
        })
        .collect()
}

/// 拿 Unihan 数据反验 [`KANGXI_RADICALS`]：每个部首字自身的 `kRSUnicode` 必须是
/// 「该部首号 + 基本形 + 0 笔」。
///
/// `rs` 传入一个「字 → 其 kRSUnicode 解析结果」的查询闭包。返回不一致的部首号列表，
/// 空表示表与数据吻合。构建工具据此在建库前失败——固化的表一旦与上游对不上，
/// 产出的每一个部首都是错的，而运行时没有任何东西会察觉。
pub fn verify_kangxi_table(rs: impl Fn(char) -> Option<RadicalStroke>) -> Vec<u8> {
    let mut bad = Vec::new();
    for (i, ch) in KANGXI_RADICALS.chars().enumerate() {
        let no = u8::try_from(i + 1).expect("部首号不超过 214");
        let ok = matches!(
            rs(ch),
            Some(RadicalStroke {
                radical_no,
                simplified_level: 0,
                extra_strokes: 0,
            }) if radical_no == no
        );
        if !ok {
            bad.push(no);
        }
    }
    bad
}

/// 部首号 + 撇号层级 → 部首字形。
///
/// 基本形直接查 [`KANGXI_RADICALS`]；简化形无法查表——Unicode 的「CJK 部首补充区」
/// 对这些字符**没有**给出 NFKC 归一（实测 113 个字符里仅 1 个有），故简化形只能由
/// 构建期从数据推导后传进来。推导规则见 `examples/build_unihan.rs`。
pub fn radical_char(radical_no: u8, simplified: Option<char>) -> Option<char> {
    match simplified {
        Some(c) => Some(c),
        None => KANGXI_RADICALS
            .chars()
            .nth(usize::from(radical_no).checked_sub(1)?),
    }
}

/// 只读的字形库。
pub struct Unihan {
    conn: Connection,
}

impl Unihan {
    /// 打开字形库。
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("打开字形库失败：{}", path.display()))?;
        // 字形库仅约 4MB，且每次查询最多取一行（主键命中）。给 2MB 缓存足够把
        // 索引页留在内存里，再多就是白占——同 docs/adr/0006 对两个词库的取舍。
        conn.pragma_update(None, "cache_size", -2000)?;
        conn.pragma_update(None, "journal_mode", "OFF")?;
        Ok(Self { conn })
    }

    /// 查一个字的字形。未收录返回 `None`。
    ///
    /// 只接受单个 `char`，不接受字符串：字形是**字**的属性，「苹果的部首」不是一个
    /// 有意义的问题，故让它在类型上就无法表达（同 [`Glyph`] 的文档）。
    pub fn get(&self, ch: char) -> Result<Option<Glyph>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT radical, radical_no, extra_strokes, total_strokes, simplified, traditional,
                    readings, tier
             FROM glyph WHERE ch = ?1",
        )?;
        let mut rows = stmt.query([ch.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let radical: String = row.get(0)?;
        Ok(Some(Glyph {
            ch,
            radical: radical.chars().next().context("部首列为空")?,
            radical_no: row.get(1)?,
            extra_strokes: row.get(2)?,
            total_strokes: row.get(3)?,
            simplified: row.get::<_, String>(4)?.chars().collect(),
            traditional: row.get::<_, String>(5)?.chars().collect(),
            // 空格分隔。构建期已排好序、去过重，读取时不再加工。
            readings: row
                .get::<_, String>(6)?
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            // 库里存的是整数，来路未必可信，故经 `from_level` 过一道而非直接转型。
            tier: row.get::<_, Option<u8>>(7)?.and_then(CharTier::from_level),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 康熙表恰好二百一十四个部首() {
        assert_eq!(KANGXI_RADICALS.chars().count(), 214);
        // 部首互不相同——重复即抄漏了一个，而长度检查抓不到这种错。
        let mut uniq: Vec<char> = KANGXI_RADICALS.chars().collect();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 214, "康熙表内有重复部首");
    }

    #[test]
    fn 康熙表抽样对得上() {
        let at = |n: usize| KANGXI_RADICALS.chars().nth(n - 1).unwrap();
        assert_eq!(at(1), '一');
        assert_eq!(at(144), '行');
        // 以下三个是「从数据推导基本形」那条路会挑错的位置，钉死它们。
        assert_eq!(at(90), '爿', "会被误挑成 丬");
        assert_eq!(at(102), '田', "会被误挑成 曱");
        assert_eq!(at(182), '風', "会被误挑成 凬");
        assert_eq!(at(214), '龠');
    }

    #[test]
    fn 解析基本形() {
        assert_eq!(
            parse_rs("144.0"),
            Some(RadicalStroke {
                radical_no: 144,
                simplified_level: 0,
                extra_strokes: 0
            })
        );
    }

    #[test]
    fn 解析简化形的撇号层级() {
        assert_eq!(parse_rs("196'.5").unwrap().simplified_level, 1);
        assert_eq!(parse_rs("212''.5").unwrap().simplified_level, 2);
        assert_eq!(parse_rs("212'''.5").unwrap().simplified_level, 3);
        // 撇号不能被算进部首号，否则解析直接失败或串号。
        assert_eq!(parse_rs("196'.5").unwrap().radical_no, 196);
    }

    /// 45 个字的部首外笔画是负的。用 `u8` 存会静默回绕成 255，界面显示「255 画」。
    #[test]
    fn 部首外笔画可以为负() {
        assert_eq!(parse_rs("125.-1").unwrap().extra_strokes, -1);
        assert_eq!(parse_rs("125.-2").unwrap().extra_strokes, -2);
    }

    /// 448 个字有多个部首分析，取第一个。全留会让一个字有两个部首。
    #[test]
    fn 多值只取第一个分析() {
        let rs = parse_rs("1.2 75.-1").unwrap();
        assert_eq!(rs.radical_no, 1);
        assert_eq!(rs.extra_strokes, 2);
    }

    #[test]
    fn 拒绝越界与畸形的部首号() {
        assert!(parse_rs("0.3").is_none());
        assert!(parse_rs("215.0").is_none());
        assert!(parse_rs("144").is_none(), "缺小数点");
        assert!(parse_rs("").is_none());
    }

    #[test]
    fn 部首字形按层级取() {
        assert_eq!(radical_char(144, None), Some('行'));
        assert_eq!(radical_char(149, Some('讠')), Some('讠'));
        assert_eq!(radical_char(0, None), None);
        assert_eq!(radical_char(215, None), None);
    }

    #[test]
    fn 读音要剥掉页码前缀() {
        assert_eq!(
            parse_readings("0442.080:háng 1290.030:xíng"),
            ["háng", "xíng"]
        );
        // kMandarin 没有页码，裸读音也得认。
        assert_eq!(parse_readings("xíng"), ["xíng"]);
        assert!(parse_readings("").is_empty());
    }

    /// 143 条记录的页码是逗号分隔的多个。按**第一个**冒号切会把
    /// `049.020:chǒu` 整段当成读音，得到 `049.020:chǒu` 这种垃圾。
    #[test]
    fn 逗号分隔的多页码不影响取读音() {
        assert_eq!(parse_readings("049.010,049.020:chǒu"), ["chǒu"]);
        assert_eq!(
            parse_readings("0758.081,0758.091:ma 0770.150:me 1340.041:yāo"),
            ["ma", "me", "yāo"]
        );
    }

    #[test]
    fn 同一读音只留一个() {
        assert_eq!(
            parse_readings("001.010:yī 002.020:yī 003.030:èr"),
            ["yī", "èr"]
        );
    }

    #[test]
    fn 读音频次解析得出() {
        assert_eq!(
            parse_pinlu("xíng(2943) háng(218)"),
            [("xíng".to_string(), 2943), ("háng".to_string(), 218)]
        );
        // 畸形项跳过而不是整条报废——少一个排序依据，不该连累其余读音。
        assert_eq!(
            parse_pinlu("xíng(abc) háng(218)"),
            [("háng".to_string(), 218)]
        );
        assert!(parse_pinlu("").is_empty());
    }

    #[test]
    fn 字级只认一到三() {
        assert_eq!(CharTier::from_level(1), Some(CharTier::Level1));
        assert_eq!(CharTier::from_level(3), Some(CharTier::Level3));
        assert_eq!(CharTier::from_level(0), None);
        assert_eq!(CharTier::from_level(4), None);
        assert_eq!(CharTier::Level1.label(), "一级字");
        assert_eq!(CharTier::Level2.level(), 2);
    }

    /// 拿**真实建出来的库**跑一遍。
    ///
    /// 上面那些测的都是纯函数，证明不了「建库工具写进去的东西读得回来」——解析对、
    /// 表结构对，中间任一步接错列序，单测照样全绿。本机没有库时跳过（同
    /// `source::offline` 里那条），故它在 CI 上是弱的，在开发机上是真的。
    #[test]
    fn 真实库里的字形读得回来() {
        let path = std::path::Path::new(".cache/dict/unihan.db");
        if !path.exists() {
            eprintln!("跳过：本机没有 .cache/dict/unihan.db（跑 scripts/dev.ps1 gd 生成）");
            return;
        }
        let db = Unihan::open(path).expect("字形库打得开");

        // 简化部首 + 繁体对应。这个字同时钉住三件事：撇号形被解成 讠 而非 言、
        // 部外笔画与总笔画没有串列、繁简变体存下来了。
        let g = db.get('语').unwrap().expect("语 应当收录");
        assert_eq!(g.radical, '讠', "简化部首形没解出来");
        assert_eq!(g.radical_no, 149);
        assert_eq!((g.extra_strokes, g.total_strokes), (7, 9));
        assert_eq!(g.traditional, vec!['語']);
        assert!(g.simplified.is_empty(), "本字已是简体，不该再指向简体");

        // 基本部首，且部首自身即该字。
        let g = db.get('行').unwrap().expect("行 应当收录");
        assert_eq!(
            (g.radical, g.radical_no, g.extra_strokes, g.total_strokes),
            ('行', 144, 0, 6)
        );

        // 这两个是「从数据推基本形」那条错路会挑错的位置，用真实库再钉一遍。
        assert_eq!(db.get('田').unwrap().unwrap().radical, '田');
        assert_eq!(db.get('爿').unwrap().unwrap().radical, '爿');

        // 读音：普通话调号、大陆标准、按常用度排序。
        // `行` 的 xíng 在频率词典里 2943 次、háng 218 次，故 xíng 必须在前——
        // 若退回原字典页码序，出来的是 háng 打头，那是按拼音字母排的，与常用度无关。
        let g = db.get('行').unwrap().unwrap();
        assert_eq!(g.readings.first().map(String::as_str), Some("xíng"));
        assert!(g.readings.iter().any(|r| r == "háng"));

        // 取 kXHC1983 而非 kTGHZ2013 的验证点：后者对 `语` 只给 yǔ，而 CC-CEDICT
        // 在同一张卡片下列着 yu3 与 yu4 两条词条。少给一个音，卡片就自相矛盾。
        assert_eq!(db.get('语').unwrap().unwrap().readings, ["yǔ", "yù"]);

        // ü 必须是调号形式，不是 v 也不是 u:。
        assert_eq!(db.get('女').unwrap().unwrap().readings, ["nǚ"]);

        // 字级：一级 3500 常用字。
        assert_eq!(
            db.get('好').unwrap().unwrap().tier,
            Some(crate::domain::CharTier::Level1)
        );
        // 48 画的生僻字不在《通用规范汉字表》里，没有字级——这是 None 而非报错。
        assert_eq!(db.get('龘').unwrap().unwrap().tier, None);

        // 未收录的字返回 None，不是报错——字形缺失是常态，不是故障。
        assert!(db.get('A').unwrap().is_none());
    }

    /// 反验器必须真的会报错，否则它在构建期只是个摆设。
    #[test]
    fn 反验器能抓出表与数据不一致() {
        let 全对 = |ch: char| {
            KANGXI_RADICALS
                .chars()
                .position(|c| c == ch)
                .map(|i| RadicalStroke {
                    radical_no: u8::try_from(i + 1).unwrap(),
                    simplified_level: 0,
                    extra_strokes: 0,
                })
        };
        assert!(verify_kangxi_table(全对).is_empty());

        // 把部首 144 的数据改成别的号，反验器应当只报这一个。
        let 错一个 = |ch: char| {
            let mut rs = 全对(ch)?;
            if ch == '行' {
                rs.radical_no = 9;
            }
            Some(rs)
        };
        assert_eq!(verify_kangxi_table(错一个), vec![144]);

        // 数据里查不到的部首也算不一致，不能当作「没意见」放过。
        assert_eq!(verify_kangxi_table(|_| None).len(), 214);
    }
}
