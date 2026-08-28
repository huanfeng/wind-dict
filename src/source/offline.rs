//! 离线词典：无需网络、随程序分发的那个词典。
//!
//! 它是**一个词典**，背后挂**两份词库**（英汉的 ECDICT + 汉英的 CC-CEDICT）——
//! 词典与词库是多对多关系，见术语表。用户认知里只有「离线词典」这一个东西；
//! 「英汉」「汉英」不是两个词典，只是同一个词典的两个方向（docs/adr/0003）。

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
    /// 打开两份词库。
    ///
    /// 二者缺一不可：缺了任一份，对应方向的查询就只能谎称「未收录」——而术语表里
    /// 「一无所获」的意思是**词典确实没有这个词**，不是「我没能力查」。与其在运行时
    /// 撒谎，不如在这里失败。
    ///
    /// 字形库则**允许缺席**，且这不是双重标准。上面那条理由是「缺了会让程序说谎」，
    /// 它不迁移到字形：少一份字形，界面只是不显示部首笔画，没有任何一句话变成假的。
    /// 让一个纯增益的数据有权阻止程序启动，代价是所有尚未重建数据的部署直接打不开。
    pub fn open(
        ecdict_path: &std::path::Path,
        cedict_path: &std::path::Path,
        unihan_path: &std::path::Path,
    ) -> Result<Self> {
        Ok(Self {
            ecdict: Ecdict::open(ecdict_path)?,
            cedict: Cedict::open(cedict_path)?,
            // 打不开就当没有。这里吞掉错误是刻意的——它与「词库路径设错」不同，
            // 用户没有配置过字形库，也就无从「设错」。
            unihan: Unihan::open(unihan_path).ok(),
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

/// 校验一个文件能否用作**指定方向**的词库。
///
/// 只看扩展名等于没校验：文件选择器按 `.db` 过滤，把汉英库选进英汉槽它照收，而两个
/// 库的表结构完全不同——真正打开一次并试查一个词，才知道选对没有。
///
/// 之所以值得为此多写一段：词库设错在 release 下是**静默致命**的（`main` 直接 exit，
/// 而无控制台构建看不到任何输出），用户只会看到程序打不开。宁可在设置页当场拦住。
pub fn probe_dict(path: &std::path::Path, is_ec: bool) -> Result<()> {
    if is_ec {
        let d = Ecdict::open(path)?;
        // 试查一次。查不到不代表库坏（可能只是词条少），但**报错**说明表结构不对。
        let q = Query::new("the").expect("常量查询词非空");
        d.lookup(&q).context("该文件不是英汉词库（表结构不符）")?;
    } else {
        let d = Cedict::open(path)?;
        let q = Query::new("的").expect("常量查询词非空");
        d.lookup(&q).context("该文件不是汉英词库（表结构不符）")?;
    }
    Ok(())
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
    use super::probe_dict;

    /// 把汉英库选进英汉槽必须被拦住。
    ///
    /// 这是 `probe_dict` 存在的唯一理由——文件选择器只按 `.db` 过滤，两个库都叫
    /// `.db`，选反了它照收。而词库设错在 release 下是静默致命的：`main` 直接 exit，
    /// 无控制台构建看不到任何输出，用户只看到程序打不开。
    #[test]
    fn 方向选反的词库被拦住() {
        let ec = std::path::Path::new(".cache/dict/ecdict.db");
        let ce = std::path::Path::new(".cache/dict/cedict.db");
        if !ec.exists() || !ce.exists() {
            eprintln!("跳过：本机没有词库文件");
            return;
        }
        assert!(probe_dict(ec, true).is_ok(), "英汉库放英汉槽应通过");
        assert!(probe_dict(ce, false).is_ok(), "汉英库放汉英槽应通过");
        assert!(probe_dict(ce, true).is_err(), "汉英库放进英汉槽必须被拦住");
        assert!(probe_dict(ec, false).is_err(), "英汉库放进汉英槽必须被拦住");
    }

    /// 随便一个不是 SQLite 的文件也要被拦住。
    #[test]
    fn 非词库文件被拦住() {
        let p = std::env::temp_dir().join(format!("wd_notdb_{}.db", std::process::id()));
        std::fs::write(&p, b"this is not a database").unwrap();
        assert!(probe_dict(&p, true).is_err());
        assert!(probe_dict(&p, false).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
