//! 自带词典：用户自己放进来的 MDX。
//!
//! 与离线词典的关系是**并列**，不是补充也不是兜底：两者都实现
//! [`Dictionary`]，同一个查询会问遍所有词典，各自的结果并排呈现。这与 ADR-0002
//! 拒绝「离线未命中就自动发网络请求」不冲突——那条拒绝的是**跨类**的静默降级
//! （词典 → 译源），而这里全是词典，没有可信度落差需要用户知情。
//!
//! ## 为什么不参与补全
//!
//! [`crate::domain::Wordlist`] 只由离线词典实现，自带词典**刻意不接**。
//!
//! 补全是按词频排序的（ECDICT 的 `frq`），而 MDX 不带任何词频信号。把它的词混进
//! 候选列表，只能追加在末尾或按字典序插入——前者用户看不见，后者会把「打 `app`
//! 首选 `apple`」这个体验拆掉。而实际收益接近于零：随程序分发的英汉库有 340 万词条，
//! 用户那本词典里有而它没有的词，屈指可数。
//!
//! 存储层的 `Mdx::prefix` 仍然留着——将来若要做「只在这本词典里搜」，那是它的用途。
//!
//! ## 为什么是「扫目录」而不是「逐个添加」
//!
//! 词典是用户往一个目录里丢的，不是在设置页里逐个登记的。丢进去就能用、拿走就没了,
//! 这与「装一本词典」是同一件事；逐个登记则要求用户在文件管理器与设置页之间把同一
//! 件事做两遍，而第二遍纯属向程序汇报。设置页因此只剩两件事可做：**换目录**和
//! **开关某一本**。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::{Dictionary, Entry, Headword, Lookup, Query, UserEntry};
use crate::store::mdx::Mdx;

pub struct UserDictionary {
    /// 词典自报的名字（MDX 头部的 Title，为空则退到文件名）。
    base_name: String,
    /// 用户改的名字。`None` = 用 `base_name`。
    ///
    /// **别名与本名分开存**，不是就地改掉 `base_name`：用户清空输入框的意思是
    /// 「恢复默认」，而就地改掉之后本名就找不回来了，只能重开一次文件去读——那让
    /// 一个纯界面动作变成一次磁盘操作，且中途出错就永久丢了这本词典的名字。
    alias: Option<String>,
    /// 稳定键 = 文件名（见 [`key_of`]）。词条、页签、开关全按它对应。
    key: String,
    /// 这本词典的文件路径。留着是为了设置页能把「扫到的一个文件」与「已经开着的
    /// 这本词典」对上——按下标对会在中间某本被关掉或打不开时整体错位。
    path: PathBuf,
    mdx: Mdx,
}

impl UserDictionary {
    pub fn open(path: &Path) -> Result<Self> {
        let mdx = Mdx::open(path)?;
        Ok(Self {
            base_name: display_name(path, &mdx),
            alias: None,
            key: key_of(path),
            path: path.to_path_buf(),
            mdx,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 稳定键（文件名）。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 设定或清除别名。`None`（或全空白）= 恢复本名。
    ///
    /// **只改名，不改键**：改完之后这本词典的开关、页签筛选照旧认得出它是同一本。
    pub fn set_alias(&mut self, alias: Option<&str>) {
        self.alias = alias
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }

    pub fn entry_count(&self) -> u64 {
        self.mdx.meta().entry_count
    }
}

/// 词典的显示名：优先用头部的标题，为空则退到文件名。
///
/// 标题要过一遍 HTML 转换：MDX 的 `Title` 字段是允许带标签的，实测有词典往里塞
/// `<font color=red>`。直接显示会让设置页列出一行标记源码。
fn display_name(path: &Path, mdx: &Mdx) -> String {
    let title: String = crate::html::to_blocks(&mdx.meta().title)
        .iter()
        .map(|b| b.plain())
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "自带词典".to_string())
}

/// 打开一次并试查，确认这个文件真能用作词典。
///
/// 与 `offline::probe_dict` 同样的理由：只看扩展名等于没校验。MDX 里有一半特性我们
/// 不支持（LZO 压缩、正文加密），而那些词典**能打开、能列出词条数**，只在解压正文时
/// 才失败。设置页当场拦住，好过用户以为加好了、查词时得到一堆报错。
/// 词典目录里认哪种文件。
const EXT: &str = "mdx";

/// 递归的层数上限。
///
/// 不是 1 也不是无限：MDX 常常以「一本词典一个文件夹」的形式分发（`.mdx` 加同名的
/// `.mdd` 资源包和一个 `.css`），只扫平铺会把这类整包漏掉；而无限深会在用户把词典
/// 目录指到某个大目录（下载文件夹、整块盘）时变成一次全盘遍历。三层够装下
/// 「词库/英语/牛津/Oxford.mdx」这种归类习惯。
const MAX_DEPTH: usize = 3;

/// 默认的词典目录：用户数据目录下的 `dicts`。
///
/// **不在部署目录内**是硬要求，不是偏好：`dev.ps1` 卸载会
/// `Remove-Item -Recurse -Force` 整个部署目录，而这里放的是用户自己下载的、
/// 动辄几百 MB 的文件。把默认目录放进去，等于让「卸载程序」顺手删掉用户的词库。
/// 这与 ADR-0011 把收藏与历史挪出部署目录是同一条理由。
pub fn default_dir() -> Result<PathBuf> {
    let base = crate::store::userdata::data_dir().map_err(|e| anyhow::anyhow!(e))?;
    Ok(base.join("dicts"))
}

/// 扫出目录下的全部词典，按路径排序。
///
/// 排序是必需的而非整洁癖：目录遍历的次序由文件系统决定，而这个次序会成为查询结果里
/// 各本词典的先后。不排的话，同一批词典在不同机器、甚至同一台机器的不同时刻，
/// 排出来的顺序可能不同。
///
/// 目录不存在、读不动都返回空表而不是报错：一个还没建出来的词典目录是**正常状态**
/// （用户还没往里放东西），不是故障。
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&path, depth + 1, out),
            // 扩展名比较忽略大小写：Windows 的文件系统本就不区分，而词典包里
            // `.MDX` 与 `.mdx` 都见得到。
            Ok(t)
                if t.is_file()
                    && path
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case(EXT)) =>
            {
                out.push(path);
            }
            // 符号链接与其它类型一概不跟：跟软链会绕出词典目录，甚至绕成环。
            _ => {}
        }
    }
}

/// 扫出目录、剔掉关掉的、逐本打开。词典目录到「当前生效的词典」之间的全部逻辑。
///
/// 拎成自由函数而非留在界面层，是为了它能被测：这三步里每一步都可能悄悄出错——扫漏
/// 一层子目录、开关按错了键、打不开的那本把整批带崩——而界面层的 `State` 拖着一份
/// 真实的 SQLite 词库，进不了单元测试。
///
/// 打不开的**跳过**而不是让整批失败：它们是用户的文件，随时可能损坏或用了我们不支持
/// 的压缩方式，而一本坏词典不该连累其余几本。设置页会单独把它标出来（那里会为每本
/// 重开一次并显示失败原因），所以这里的静默是有去处的，不是把错误吞了。
pub fn load(dir: &Path, disabled: &[String]) -> Vec<UserDictionary> {
    scan(dir)
        .into_iter()
        .filter(|p| !disabled.contains(&key_of(p)))
        .filter_map(|p| UserDictionary::open(&p).ok())
        .collect()
}

/// 从路径取出用作开关键的文件名。
pub fn key_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn probe(path: &Path) -> Result<UserDictionary> {
    let d = UserDictionary::open(path)?;
    // 查什么词无所谓，要的是走通「解压 key block + 解压 record block」这条路。
    // 查不到不算失败（这本词典可能确实没有这个词），报错才算。
    let q = Query::new("a").expect("常量查询词非空");
    d.lookup(&q).context("这本词典打得开，但读不出正文")?;
    Ok(d)
}

impl Dictionary for UserDictionary {
    fn name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.base_name)
    }

    /// 查词。不分方向——MDX 不声明自己收的是哪种语言，只能原样问过去。
    ///
    /// 这与 ADR-0003「方向由查询词判定」不矛盾：方向的作用是给随程序分发的两个库
    /// **选路**，而这里只有一份库，无路可选。
    fn lookup(&self, query: &Query) -> Result<Lookup> {
        let hits = self.mdx.lookup(query.text())?;
        if hits.is_empty() {
            return Ok(Lookup::NotFound);
        }
        let entries = hits
            .into_iter()
            .map(|(head, html)| {
                Entry::User(UserEntry {
                    headword: Headword::from_store(head),
                    source: self.name().to_string(),
                    source_key: self.key.clone(),
                    body: crate::html::to_blocks(&html),
                })
            })
            .filter(|e| match e {
                // 空正文的词条不留：MDX 里有一类「只为占位的重定向条目」，转换后什么
                // 都不剩，留着就是给用户一个点开全空的词头。
                Entry::User(u) => !u.body.is_empty(),
                _ => true,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(Lookup::NotFound);
        }
        Ok(Lookup::Found {
            entries,
            // 自带词典没有词形变化数据，落不到原形上，故恒为 false。
            via_base_form: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mdx")
            .join(name)
    }

    #[test]
    fn 查得到就给出富文本词条() {
        let d = UserDictionary::open(&fixture("v2.mdx")).unwrap();
        let q = Query::new("zebra").unwrap();
        let Lookup::Found { entries, .. } = d.lookup(&q).unwrap() else {
            panic!("zebra 应当查得到");
        };
        assert_eq!(entries.len(), 1);
        let Entry::User(u) = &entries[0] else {
            panic!("自带词典只产出 Entry::User");
        };
        assert_eq!(u.headword.as_str(), "zebra");
        assert_eq!(u.source, "wind-dict 测试样本", "出处必须带上");
        assert_eq!(u.body.len(), 2, "两个列表项各成一段");
        assert_eq!(u.body[0].plain(), "striped animal");
        // 词典内部跳转要留住，它是自带词典唯一的导航手段。
        assert!(
            u.body[1]
                .runs
                .iter()
                .any(|r| r.link.as_deref() == Some("AA")),
            "entry:// 跳转丢了：{:?}",
            u.body[1]
        );
    }

    /// 归一化重名的词头一并成为词条——这是 MDX 的性质，不是缺陷，见 ADR-0015。
    #[test]
    fn 重名词头各成一条() {
        let d = UserDictionary::open(&fixture("v2.mdx")).unwrap();
        let q = Query::new("fullhouse").unwrap();
        let Lookup::Found { entries, .. } = d.lookup(&q).unwrap() else {
            panic!("应当查得到");
        };
        let mut heads: Vec<&str> = entries.iter().map(|e| e.headword().as_str()).collect();
        heads.sort();
        assert_eq!(heads, ["Full-House", "fullhouse"]);
    }

    #[test]
    fn 查不到就是未收录() {
        let d = UserDictionary::open(&fixture("v2.mdx")).unwrap();
        let q = Query::new("nonexistent").unwrap();
        assert!(matches!(d.lookup(&q).unwrap(), Lookup::NotFound));
    }

    /// 标题里的标签不能原样显示到设置页去。
    #[test]
    fn 词典名取自标题且去掉标记() {
        let d = UserDictionary::open(&fixture("v2.mdx")).unwrap();
        assert_eq!(d.name(), "wind-dict 测试样本");
        assert!(!d.name().contains('<'));
    }

    /// 目录里混着子目录、非 mdx 文件与大小写不一的扩展名。
    #[test]
    fn 扫目录认得出词典且忽略无关文件() {
        let root = std::env::temp_dir().join(format!("wind-dict-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("英语").join("牛津");
        std::fs::create_dir_all(&nested).unwrap();
        // 一本词典常常是「一个文件夹装 mdx + mdd + css」，故子目录必须扫到。
        std::fs::copy(fixture("v2.mdx"), nested.join("Oxford.MDX")).unwrap();
        std::fs::copy(fixture("v1.mdx"), root.join("a.mdx")).unwrap();
        std::fs::write(root.join("Oxford.mdd"), b"resource").unwrap();
        std::fs::write(root.join("readme.txt"), b"hi").unwrap();

        let got = scan(&root);
        let names: Vec<String> = got.iter().map(|p| key_of(p)).collect();
        assert_eq!(
            names,
            ["a.mdx", "Oxford.MDX"],
            "扫到的应当只有这两本：{got:?}"
        );

        // 超出深度上限的不扫——否则把词典目录指到下载文件夹会变成全盘遍历。
        let deep = root.join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::copy(fixture("v2.mdx"), deep.join("too-deep.mdx")).unwrap();
        assert_eq!(scan(&root).len(), 2, "第四层不该被扫到");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目录还没建出来是**正常状态**（用户还没放东西），不是故障。
    #[test]
    fn 目录不存在时扫出空表() {
        assert!(scan(Path::new(r"Z:\\这个目录不存在\\dicts")).is_empty());
    }

    #[test]
    fn 开关按文件名记() {
        assert_eq!(key_of(Path::new(r"D:\\a\\b\\Oxford.mdx")), "Oxford.mdx");
        assert_eq!(key_of(Path::new("")), "");
    }

    /// 建一个词典目录：`dicts/英语/牛津.mdx` + `dicts/a.mdx`。
    fn 造词典目录(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("wind-dict-load-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("英语");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::copy(fixture("v2.mdx"), sub.join("牛津.mdx")).unwrap();
        std::fs::copy(fixture("v1.mdx"), root.join("a.mdx")).unwrap();
        root
    }

    /// 没有禁用名单时，目录里的全部词典都该加载——**新丢进去的默认就能用**，
    /// 这是「扫目录」相对「逐个添加」的全部意义。
    #[test]
    fn 默认加载目录里的全部词典() {
        let root = 造词典目录("all");
        assert_eq!(load(&root, &[]).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 关掉的按**文件名**认，且只影响那一本。
    #[test]
    fn 关掉的不加载而其余照旧() {
        let root = 造词典目录("off");
        let got = load(&root, &["牛津.mdx".to_string()]);
        assert_eq!(got.len(), 1);
        assert_eq!(key_of(got[0].path()), "a.mdx");

        // 名单里有个根本不存在的名字，不该影响任何一本。
        assert_eq!(load(&root, &["不存在.mdx".to_string()]).len(), 2);
        // 全关掉就一本都不加载。
        let all = ["牛津.mdx".to_string(), "a.mdx".to_string()];
        assert!(load(&root, &all).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 一本坏词典不该连累其余几本。
    #[test]
    fn 打不开的那本被跳过() {
        let root = 造词典目录("broken");
        std::fs::write(root.join("坏的.mdx"), b"not an mdx at all").unwrap();
        assert_eq!(
            scan(&root).len(),
            3,
            "坏的也该被扫到，否则设置页里就看不见它"
        );
        assert_eq!(load(&root, &[]).len(), 2, "但加载时跳过");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 校验会走通解压这条路() {
        assert!(probe(&fixture("v2-encrypted.mdx")).is_ok());
        assert!(probe(&fixture("v1.mdx")).is_ok());
        assert!(probe(&fixture("nope.mdx")).is_err());
    }
}
