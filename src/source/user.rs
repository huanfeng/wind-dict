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

use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::{Dictionary, Entry, Headword, Lookup, Query, UserEntry};
use crate::store::mdx::Mdx;

pub struct UserDictionary {
    name: String,
    /// 加进来时的那个路径。留着是为了设置页能把「设置里的一条路径」与「已经开着的
    /// 这本词典」对上——按下标对会在中间某本打不开时整体错位。
    path: std::path::PathBuf,
    mdx: Mdx,
}

impl UserDictionary {
    pub fn open(path: &Path) -> Result<Self> {
        let mdx = Mdx::open(path)?;
        Ok(Self {
            name: display_name(path, &mdx),
            path: path.to_path_buf(),
            mdx,
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
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
        &self.name
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
                    source: self.name.clone(),
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

    #[test]
    fn 校验会走通解压这条路() {
        assert!(probe(&fixture("v2-encrypted.mdx")).is_ok());
        assert!(probe(&fixture("v1.mdx")).is_ok());
        assert!(probe(&fixture("nope.mdx")).is_err());
    }
}
