//! 离线词典：无需网络、随程序分发的那个词典。
//!
//! 它是**一个词典**，背后挂**两份词库**（英汉的 ECDICT + 汉英的 CC-CEDICT）——
//! 词典与词库是多对多关系，见术语表。用户认知里只有「离线词典」这一个东西；
//! 「英汉」「汉英」不是两个词典，只是同一个词典的两个方向（docs/adr/0003）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::{Candidate, Dictionary, Direction, Glyph, Lookup, Query, Wordlist};
use crate::store::cedict::Cedict;
use crate::store::ecdict::Ecdict;
use crate::store::unihan::Unihan;

/// 离线词典。
pub struct OfflineDictionary {
    /// 英汉方向的词库。
    ecdict: Ecdict,
    /// 汉英方向的词库。
    cedict: Cedict,
    /// 字形库。**可缺**，理由见 [`OfflineDictionary::open`]。
    unihan: Option<Unihan>,
}

impl OfflineDictionary {
    /// 从一个**目录**打开三份库，文件名是约定好的（[`ECDICT_FILE`] 等）。
    ///
    /// 收目录而非三条路径：三份库永远住在一起、随同一次部署整体替换。收三条路径等于
    /// 允许一种从不发生、却处处要防的状态——英汉指向新版、汉英还指着旧版。
    ///
    /// 两份词库缺一不可：缺了任一份，对应方向的查询就只能谎称「未收录」——而术语表里
    /// 「一无所获」的意思是**词典确实没有这个词**，不是「我没能力查」。与其在运行时
    /// 撒谎，不如在这里失败。
    ///
    /// 字形库则**允许缺席**，且这不是双重标准。上面那条理由是「缺了会让程序说谎」，
    /// 它不迁移到字形：少一份字形，界面只是不显示部首笔画，没有任何一句话变成假的。
    /// 让一个纯增益的数据有权阻止程序启动，代价是所有尚未重建数据的部署直接打不开。
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self {
            ecdict: Ecdict::open(&dir.join(ECDICT_FILE))?,
            cedict: Cedict::open(&dir.join(CEDICT_FILE))?,
            // 打不开就当没有。这里吞掉错误是刻意的——理由见本方法上面那段。
            unihan: Unihan::open(&dir.join(UNIHAN_FILE)).ok(),
        })
    }

    /// 查一个字的字形。字形库缺席、或该字未收录时返回 `None`。
    ///
    /// 只接受单个 `char`：字形是**字**的属性，「苹果的部首」不是有意义的问题。
    /// 这一条在类型上挡住，而不是靠调用方自觉。
    pub fn glyph(&self, ch: char) -> Option<Glyph> {
        self.unihan.as_ref()?.get(ch).ok().flatten()
    }
}

// ── 词库目录 ──────────────────────────────────────────────

/// 三份库的文件名。**部署脚本与程序共用这套名字**，改名要两边一起改。
pub const ECDICT_FILE: &str = "ecdict.db";
pub const CEDICT_FILE: &str = "cedict.db";
pub const UNIHAN_FILE: &str = "unihan.db";

/// exe 所在目录，也就是默认的词库目录。
///
/// **不是工作目录**：常驻工具从托盘或热键启动时，工作目录是什么完全不可控。
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default()
}

/// 一个目录里三份库各自的状况。字节数用于让用户认出「是不是完整的那一份」。
pub struct DirStatus {
    pub ecdict: Result<u64, String>,
    pub cedict: Result<u64, String>,
    /// 字形库可缺，故没有失败态——不在就是不在，界面少显示一行部首笔画而已。
    pub unihan: Option<u64>,
}

impl DirStatus {
    /// 两份**必需**的库都在且表结构对得上。
    pub fn usable(&self) -> bool {
        self.ecdict.is_ok() && self.cedict.is_ok()
    }

    /// 缺什么，用于报错与设置页提示。齐备时返回空表。
    pub fn missing(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Err(e) = &self.ecdict {
            out.push(format!("英汉词库（{ECDICT_FILE}）：{e}"));
        }
        if let Err(e) = &self.cedict {
            out.push(format!("汉英词库（{CEDICT_FILE}）：{e}"));
        }
        out
    }
}

/// 检查一个目录能否用作词库目录。
///
/// **真开一次并试查**，不只看文件在不在：文件名对不代表内容对——把汉英库改名成
/// `ecdict.db` 放进去，只看名字的检查会照收，而两个库的表结构完全不同。
///
/// 之所以值得为此多写一段：词库不可用在 release 下是**静默致命**的（`main` 直接
/// 退出，而无控制台构建看不到任何输出），用户只会看到「双击了没反应」。这个函数是
/// 设置页当场拦住、以及启动时回退到默认目录的共同依据。
pub fn check_dir(dir: &Path) -> DirStatus {
    fn size(p: &Path) -> u64 {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }
    let ec = dir.join(ECDICT_FILE);
    let ce = dir.join(CEDICT_FILE);
    let un = dir.join(UNIHAN_FILE);

    let ecdict = (|| -> Result<u64> {
        let d = Ecdict::open(&ec)?;
        // 试查一次。查不到不代表库坏（可能只是词条少），但**报错**说明表结构不对。
        let q = Query::new("the").expect("常量查询词非空");
        d.lookup(&q).context("表结构不符")?;
        Ok(size(&ec))
    })()
    .map_err(|e| format!("{e:#}"));

    let cedict = (|| -> Result<u64> {
        let d = Cedict::open(&ce)?;
        let q = Query::new("的").expect("常量查询词非空");
        d.lookup(&q).context("表结构不符")?;
        Ok(size(&ce))
    })()
    .map_err(|e| format!("{e:#}"));

    DirStatus {
        ecdict,
        cedict,
        unihan: Unihan::open(&un).ok().map(|_| size(&un)),
    }
}

impl Dictionary for OfflineDictionary {
    fn name(&self) -> &str {
        "离线词典"
    }

    /// 按查询方向路由到对应词库。
    ///
    /// 方向已由查询词自动判定（docs/adr/0003），此处只是分发——**不存在**让用户
    /// 选方向的入口，也不该有。
    fn lookup(&self, query: &Query) -> Result<Lookup> {
        match query.direction() {
            Direction::EnToZh => self.ecdict.lookup(query),
            Direction::ZhToEn => self.cedict.lookup(query),
        }
    }
}

impl Wordlist for OfflineDictionary {
    /// 补全同样按方向路由。
    ///
    /// 复用 [`Query`] 来判方向而非另写一套判定：前缀本就是「打了一半的查询词」，
    /// 判定规则必须与查询完全一致——否则会出现「补全按中文找、查询按英文查」这种
    /// 自相矛盾的行为。
    ///
    /// 注意两个方向的排序依据**不同**：英汉按词频（ECDICT 有 `frq`），汉英按词长
    /// （CC-CEDICT 无任何词频信号）。这是两份词库的真实差异，不是缺陷。
    fn complete(&self, prefix: &str, limit: usize) -> Result<Vec<Candidate>> {
        let Some(q) = Query::new(prefix) else {
            return Ok(Vec::new());
        };
        match q.direction() {
            Direction::EnToZh => self.ecdict.complete(q.text(), limit),
            Direction::ZhToEn => self.cedict.complete(q.text(), limit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_dir, CEDICT_FILE, ECDICT_FILE};

    /// 造一个目录，把 `src` 复制成目录里的 `name`。
    fn 放(dir: &std::path::Path, name: &str, src: &str) {
        std::fs::copy(src, dir.join(name)).unwrap();
    }

    fn 临时目录(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wd-dictdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn 有词库() -> bool {
        std::path::Path::new(".cache/dict/ecdict.db").exists()
            && std::path::Path::new(".cache/dict/cedict.db").exists()
    }

    #[test]
    fn 齐备的目录可用() {
        if !有词库() {
            eprintln!("跳过：本机没有词库文件");
            return;
        }
        let st = check_dir(std::path::Path::new(".cache/dict"));
        assert!(st.usable(), "缺：{:?}", st.missing());
        assert!(st.ecdict.as_ref().is_ok_and(|n| *n > 0), "该报出字节数");
        assert!(st.missing().is_empty());
    }

    /// 名字对、内容不对，必须被拦住。
    ///
    /// 这是 `check_dir` 真开一次而不是只看文件在不在的**唯一理由**：把汉英库改名成
    /// `ecdict.db`，只看名字的检查会照收，而两个库的表结构完全不同。而词库设错在
    /// release 下是静默致命的——`main` 直接退出，无控制台构建看不到任何输出，
    /// 用户只看到程序打不开。
    #[test]
    fn 名字对但内容不对的被拦住() {
        if !有词库() {
            eprintln!("跳过：本机没有词库文件");
            return;
        }
        let d = 临时目录("swapped");
        // 两份库对调着放：文件名齐全，内容全错。
        放(&d, ECDICT_FILE, ".cache/dict/cedict.db");
        放(&d, CEDICT_FILE, ".cache/dict/ecdict.db");
        let st = check_dir(&d);
        assert!(!st.usable(), "对调的两份库必须被拦住");
        assert_eq!(st.missing().len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 缺一份就报哪一份() {
        if !有词库() {
            eprintln!("跳过：本机没有词库文件");
            return;
        }
        let d = 临时目录("partial");
        放(&d, ECDICT_FILE, ".cache/dict/ecdict.db");
        let st = check_dir(&d);
        assert!(!st.usable());
        let missing = st.missing();
        assert_eq!(missing.len(), 1, "只缺汉英，不该连英汉一起报");
        assert!(missing[0].contains(CEDICT_FILE), "{missing:?}");
        // 字形库可缺，且它的缺席不影响可用性判定。
        assert!(st.unihan.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 空目录、以及塞了个不是 SQLite 的文件，都要被拦住而不是恐慌。
    #[test]
    fn 空目录与假文件都被拦住() {
        let d = 临时目录("junk");
        assert!(!check_dir(&d).usable(), "空目录");
        std::fs::write(d.join(ECDICT_FILE), b"not a database").unwrap();
        std::fs::write(d.join(CEDICT_FILE), b"not a database").unwrap();
        assert!(!check_dir(&d).usable(), "假文件");
        let _ = std::fs::remove_dir_all(&d);
    }
}
