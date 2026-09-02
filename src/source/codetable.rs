//! 码表反查：由字查它在某个输入方案里的编码与字根拆分。
//!
//! 数据来自清风输入法（WindInput）的**拆字表**——一份 `字<TAB>字根<TAB>编码` 的 TSV：
//!
//! ```text
//! # 五笔86拆字数据库  格式: 字符<TAB>字根序列（空格分隔）<TAB>五笔86编码
//! 的<TAB>白 勺 丶 ⺊<TAB>rqyy
//! ```
//!
//! ## 为什么按「方案」发现而不是写死五笔
//!
//! WindInput 的方案是**自描述**的：`<方案>.schema.toml` 里的 `[engine.chaizi]` 段自带
//! 拆字表路径、字根字体路径与字体家族名。照这个格式扫目录，虎码、小鹤这类第三方方案
//! 放进去就自动出现，不需要本项目认识它们——这正是「支持扩展」该有的样子。写死五笔的话，
//! 每多一个方案就要改一次代码。
//!
//! ## 字根是私用区字符
//!
//! 五笔的字根序列落在 Unicode 私用区（`U+E000` 起），必须配 `HeiTiZiGen.ttf` 才显示得
//! 出来，否则是一排方框。装了 WindInput 的机器上这个字体已经在系统字体库里（安装器会
//! 装它），故优先按 `font_family` 用；方案自带的 `font_path` 作为退路。
//!
//! 用汉字部件当字根的方案（虎码等）没有这个问题，它们的 `font_family` 为空即可。
//!
//! ## 只做「由字查码」
//!
//! 反查在输入法语境里就是这个方向。反过来「由码查字」需要另一套倒排索引，且与英文查询
//! 天然打架——`stand` 既是英文词也是一串合法五笔码，界面无从判断用户要哪个。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::{CharCode, CodeEntry, Dictionary, Entry, Headword, Lookup, Query};

/// 一个方案在磁盘上的描述，由 `<方案>.schema.toml` 解析而来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRef {
    /// 稳定键：方案 id（`[schema] id`），缺失时退回文件名。改名映射按它存。
    pub key: String,
    /// 方案名（`[schema] name`），缺失时同 key。
    pub name: String,
    /// 拆字表的绝对路径。
    pub table: PathBuf,
    /// 字根字体家族名。空 = 字根是普通汉字，不需要特殊字体。
    pub font_family: Option<String>,
    /// 字根字体文件。系统没装那个家族时的退路。
    pub font_path: Option<PathBuf>,
}

/// 一个字的编码与拆分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeInfo {
    roots: String,
    code: String,
}

/// 一张码表。
pub struct CodeTable {
    key: String,
    base_name: String,
    /// 用户改的名字（`Settings::dict_names`）。与自带词典同一套机制。
    alias: Option<String>,
    font_family: Option<String>,
    /// 查词组时要不要逐字列出（`Settings::code_multi_char`）。
    multi_char: bool,
    map: HashMap<char, CodeInfo>,
}

impl CodeTable {
    /// 从一份拆字表建表。
    ///
    /// 整表载入内存：五笔那份是 27536 行、约 700 KB，解析后含 HashMap 开销不到 2 MB，
    /// 而换来的是查询期零 IO。相比之下按需读盘要为每个字做一次随机读，而用户每敲一个字
    /// 都会查一次。
    pub fn load(schema: &SchemaRef) -> Result<Self> {
        let raw = std::fs::read_to_string(&schema.table)
            .with_context(|| format!("读不了拆字表：{}", schema.table.display()))?;
        let mut map = HashMap::new();
        for line in raw.lines() {
            // 首行是格式说明，且第三方表也可能带注释。
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(ch), Some(roots)) = (it.next(), it.next()) else {
                continue;
            };
            // 编码列可缺：有的表只给拆分。缺了照样收——「这个字怎么拆」本身就有用。
            let code = it.next().unwrap_or("").trim().to_string();
            let mut chars = ch.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                // 词条行（多字）不收：拆字表讲的是**单字**怎么拆，多字词的编码由方案的
                // 造词规则算出来，不在这份数据里。
                continue;
            };
            map.insert(
                c,
                CodeInfo {
                    roots: roots.trim().to_string(),
                    code,
                },
            );
        }
        anyhow::ensure!(!map.is_empty(), "拆字表里一个字也没有");
        Ok(Self {
            key: schema.key.clone(),
            base_name: schema.name.clone(),
            alias: None,
            font_family: schema.font_family.clone(),
            multi_char: true,
            map,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// 字根字体家族名，供界面渲染字根那一段。
    pub fn font_family(&self) -> Option<&str> {
        self.font_family.as_deref()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 查词组时是否逐字列出。
    pub fn set_multi_char(&mut self, on: bool) {
        self.multi_char = on;
    }

    /// 改名。空名字 = 恢复默认，与自带词典同一套语义。
    pub fn set_alias(&mut self, alias: Option<&str>) {
        self.alias = alias.filter(|s| !s.trim().is_empty()).map(str::to_string);
    }
}

impl Dictionary for CodeTable {
    fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.base_name)
    }

    /// 逐字给出编码与拆分。
    ///
    /// 多字词也收：查「输入法」时逐字列出三个字的编码，那正是想不起某个字怎么打时要看的
    /// 东西。词整体的编码由方案的造词规则算出，不在拆字表里，故不编造。
    fn lookup(&self, query: &Query) -> Result<Lookup> {
        let chars: Vec<CharCode> = query
            .text()
            .chars()
            .filter_map(|c| {
                let info = self.map.get(&c)?;
                Some(CharCode {
                    ch: c,
                    code: info.code.clone(),
                    roots: info.roots.clone(),
                })
            })
            .collect();
        // 判据是**命中的字数**而不是查询词的长度：查「的x」只命中一个字，那仍是一次
        // 单字反查，不该被「词组」这条规则挡掉。
        if chars.is_empty() || (!self.multi_char && chars.len() > 1) {
            return Ok(Lookup::NotFound);
        }
        Ok(Lookup::Found {
            entries: vec![Entry::Code(CodeEntry {
                headword: Headword::from_store(query.text().to_string()),
                source: self.name().to_string(),
                source_key: self.key.clone(),
                font_family: self.font_family.clone(),
                chars,
            })],
            // 码表按字直查，没有词形变化这回事。
            via_base_form: false,
        })
    }
}

// ── 方案发现 ────────────────────────────────────────────────────────────────

/// 扫一个 `schemas/` 目录，找出所有带拆字表的方案。
///
/// 只认 `*.schema.toml`，且必须有 `[engine.chaizi] db_path`——没有拆字表的方案（拼音、
/// 双拼、英文）对本项目没有意义，它们回答不了「这个字怎么拆」。
///
/// 路径按 WindInput 的约定解析：`db_path` 相对于 **schemas 目录本身**
/// （`wubi86/wubi86_chaizi.txt` → `<schemas>/wubi86/wubi86_chaizi.txt`）。
pub fn discover(schemas_dir: &Path) -> Vec<SchemaRef> {
    let Ok(rd) = std::fs::read_dir(schemas_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if !path.to_string_lossy().ends_with(".schema.toml") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(r) = parse_schema(&src, &path, schemas_dir) else {
            continue;
        };
        out.push(r);
    }
    // 文件序取决于文件系统，按名字排一遍，页签次序才是稳定的。
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// 从一份 schema.toml 里取出本项目关心的那几个字段。没有拆字表就返回 `None`。
fn parse_schema(src: &str, file: &Path, schemas_dir: &Path) -> Option<SchemaRef> {
    let db = toml_get(src, "engine.chaizi", "db_path")?;
    let table = schemas_dir.join(&db);
    let stem = file
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.trim_end_matches(".schema.toml"))
        .unwrap_or("")
        .to_string();
    let key = toml_get(src, "schema", "id").unwrap_or_else(|| stem.clone());
    let name = toml_get(src, "schema", "name").unwrap_or_else(|| key.clone());
    Some(SchemaRef {
        key,
        name,
        table,
        font_family: toml_get(src, "engine.chaizi", "font_family").filter(|s| !s.is_empty()),
        font_path: toml_get(src, "engine.chaizi", "font_path")
            .filter(|s| !s.is_empty())
            .map(|p| schemas_dir.join(p)),
    })
}

/// 从 TOML 文本里取一个字符串值。
///
/// **只认** `[section]` 行与 `key = "value"`（单双引号皆可，行内 `#` 之后是注释）。
/// 不引 `toml` + `serde`：这里要读的只有四个字符串，而那套依赖会把 serde 整个拖进一个
/// 以体积为卖点的二进制——与自己写 ripemd128、键序归一化是同一笔账（见 `src/store/mdx`）。
///
/// 代价是多行字符串、数组、行内表读不出值。那时该键**当作缺失**（方案被跳过），而不是
/// 解析出一个错的路径——把一条读不懂的配置当成读懂了，比读不懂危险得多。
fn toml_get(src: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = String::new();
    for line in src.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // `[[dictionaries]]` 这类数组表的名字会剩一层方括号，正好与我们要的段名不等。
            cur = name.trim().to_string();
            continue;
        }
        if cur != section {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        return Some(unquote(v.trim()));
    }
    None
}

/// 剥掉 TOML 标量值的引号与行尾注释。
fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(rest) = v.strip_prefix(q) {
            // 引号内的 `#` 不是注释，故先找配对引号。
            if let Some(end) = rest.find(q) {
                return rest[..end].to_string();
            }
        }
    }
    // 裸值：截到注释为止。
    v.split('#').next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
[schema]
id = "wubi86"
name = "五笔"
description = "五笔86版输入方案"

[engine]
type = "codetable"

[engine.chaizi]
db_path = "wubi86/wubi86_chaizi.txt"
font_path = "wubi86/HeiTiZiGen.ttf"
font_family = "黑体字根"

[[dictionaries]]
id = "wubi86_main"
path = "wubi86/wubi86_jidian.dict.yaml"
"#;

    #[test]
    fn 取得出方案里那几个字段() {
        assert_eq!(toml_get(SCHEMA, "schema", "id").as_deref(), Some("wubi86"));
        assert_eq!(toml_get(SCHEMA, "schema", "name").as_deref(), Some("五笔"));
        assert_eq!(
            toml_get(SCHEMA, "engine.chaizi", "font_family").as_deref(),
            Some("黑体字根")
        );
    }

    /// 段名必须整段相等，不能只看前缀。
    ///
    /// `[engine]` 与 `[engine.chaizi]` 是两个段，而 `type = "codetable"` 在前者里。
    /// 按前缀匹配的话，`engine.chaizi` 会把 `[engine]` 段的键也读进来。
    #[test]
    fn 段名不按前缀匹配() {
        assert_eq!(toml_get(SCHEMA, "engine.chaizi", "type"), None);
        assert_eq!(
            toml_get(SCHEMA, "engine", "type").as_deref(),
            Some("codetable")
        );
    }

    /// `[[dictionaries]]` 这类数组表不该被当成 `[dictionaries]`。
    #[test]
    fn 数组表不冒充普通段() {
        assert_eq!(toml_get(SCHEMA, "dictionaries", "id"), None);
    }

    #[test]
    fn 引号与行尾注释都剥得掉() {
        assert_eq!(unquote(r#""值"  # 注释"#), "值");
        assert_eq!(unquote("'单引号'"), "单引号");
        assert_eq!(unquote("裸值 # 注释"), "裸值");
        // 引号内的 # 不是注释——路径里带 # 是合法的。
        assert_eq!(unquote(r##""a#b""##), "a#b");
    }

    /// 没有拆字表的方案（拼音、双拼）直接跳过：它们回答不了「这个字怎么拆」。
    #[test]
    fn 没有拆字表的方案不算数() {
        let pinyin = "[schema]\nid = \"pinyin\"\nname = \"全拼\"\n";
        assert!(parse_schema(pinyin, Path::new("pinyin.schema.toml"), Path::new(".")).is_none());
    }

    #[test]
    fn 方案的路径按_schemas_目录解析() {
        let dir = Path::new(r"C:\App\data\schemas");
        let r = parse_schema(SCHEMA, &dir.join("wubi86.schema.toml"), dir).unwrap();
        assert_eq!(r.key, "wubi86");
        assert_eq!(r.name, "五笔");
        assert_eq!(r.table, dir.join("wubi86/wubi86_chaizi.txt"));
        assert_eq!(r.font_family.as_deref(), Some("黑体字根"));
        assert_eq!(r.font_path, Some(dir.join("wubi86/HeiTiZiGen.ttf")));
    }

    /// 方案 id 缺失时退回文件名，不能让整个方案因此消失。
    #[test]
    fn 缺_id_时用文件名当键() {
        let s = "[engine.chaizi]\ndb_path = \"t.txt\"\n";
        let r = parse_schema(s, Path::new("huma.schema.toml"), Path::new(".")).unwrap();
        assert_eq!(r.key, "huma");
        assert_eq!(r.name, "huma", "名字也退回同一个值，而不是空着");
    }

    /// 用一份内容临时建一张表。
    ///
    /// 文件名必须**每次都不同**：测试是并行跑的，共用一个路径会让几个用例互相覆盖
    /// 对方写的内容——那种失败看起来像解析出错，其实是文件被别人改了。
    fn 表(rows: &str) -> CodeTable {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("wd-ct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("t{}.txt", N.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&p, rows).unwrap();
        CodeTable::load(&SchemaRef {
            key: "t".into(),
            name: "测试码表".into(),
            table: p,
            font_family: None,
            font_path: None,
        })
        .unwrap()
    }

    #[test]
    fn 载入拆字表并按字查得到() {
        let t = 表("# 头行注释\n的\t白 勺\trqyy\n一\t一\tggll\n");
        assert_eq!(t.len(), 2);
        let q = Query::new("的").unwrap();
        let Lookup::Found { entries, .. } = t.lookup(&q).unwrap() else {
            panic!("该查得到");
        };
        let Entry::Code(e) = &entries[0] else {
            panic!("该是码表词条")
        };
        assert_eq!(e.chars.len(), 1);
        assert_eq!(e.chars[0].code, "rqyy");
        assert_eq!(e.chars[0].roots, "白 勺");
    }

    /// 多字词逐字给：想不起某个字怎么打时，要看的正是这个。
    #[test]
    fn 多字词逐字列出() {
        let t = 表("的\t白 勺\trqyy\n一\t一\tggll\n");
        let q = Query::new("的一").unwrap();
        let Lookup::Found { entries, .. } = t.lookup(&q).unwrap() else {
            panic!("该查得到");
        };
        let Entry::Code(e) = &entries[0] else {
            panic!()
        };
        assert_eq!(e.chars.len(), 2);
        assert_eq!(e.chars[1].ch, '一');
    }

    /// 表里没有的字直接略过，不占位。
    #[test]
    fn 未收录的字不出现在结果里() {
        let t = 表("的\t白 勺\trqyy\n");
        let q = Query::new("的x").unwrap();
        let Lookup::Found { entries, .. } = t.lookup(&q).unwrap() else {
            panic!()
        };
        let Entry::Code(e) = &entries[0] else {
            panic!()
        };
        assert_eq!(e.chars.len(), 1, "拉丁字母不在拆字表里，不该占一行");
    }

    #[test]
    fn 一个字都没命中就是未收录() {
        let t = 表("的\t白 勺\trqyy\n");
        let q = Query::new("hello").unwrap();
        assert!(matches!(t.lookup(&q).unwrap(), Lookup::NotFound));
    }

    /// 多字行不收：拆字表讲的是单字怎么拆，词的编码由造词规则算，不在这份数据里。
    #[test]
    fn 多字行不进表() {
        let t = 表("的\t白 勺\trqyy\n输入法\t x \tabcd\n");
        assert_eq!(t.len(), 1);
    }

    /// 缺编码列的表照收——「这个字怎么拆」本身就有用。
    #[test]
    fn 只有拆分没有编码也收() {
        let t = 表("的\t白 勺\n");
        assert_eq!(t.len(), 1);
        let q = Query::new("的").unwrap();
        let Lookup::Found { entries, .. } = t.lookup(&q).unwrap() else {
            panic!()
        };
        let Entry::Code(e) = &entries[0] else {
            panic!()
        };
        assert_eq!(e.chars[0].code, "");
        assert_eq!(e.chars[0].roots, "白 勺");
    }

    /// 关掉「词组逐字」后，多字词不再给结果；单字照旧。
    #[test]
    fn 词组逐字可以关掉() {
        let mut t = 表("的	白 勺	rqyy
一	一	ggll
");
        t.set_multi_char(false);
        assert!(matches!(
            t.lookup(&Query::new("的一").unwrap()).unwrap(),
            Lookup::NotFound
        ));
        assert!(matches!(
            t.lookup(&Query::new("的").unwrap()).unwrap(),
            Lookup::Found { .. }
        ));
    }

    /// 只命中一个字的多字查询仍算单字反查。
    #[test]
    fn 关掉之后半命中的查询照样给() {
        let mut t = 表("的	白 勺	rqyy
");
        t.set_multi_char(false);
        assert!(
            matches!(
                t.lookup(&Query::new("的x").unwrap()).unwrap(),
                Lookup::Found { .. }
            ),
            "「的x」只命中一个字，那仍是一次单字反查"
        );
    }

    #[test]
    fn 改名与恢复默认() {
        let mut t = 表("的\t白 勺\trqyy\n");
        assert_eq!(t.name(), "测试码表");
        t.set_alias(Some("五笔86"));
        assert_eq!(t.name(), "五笔86");
        t.set_alias(Some("   "));
        assert_eq!(t.name(), "测试码表", "空白名字视同没改");
        t.set_alias(Some("虎码"));
        t.set_alias(None);
        assert_eq!(t.name(), "测试码表");
    }
}
