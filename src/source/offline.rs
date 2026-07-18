//! 离线词典：无需网络、随程序分发的那个词典。
//!
//! 它是**一个词典**，背后挂**两份词库**（英汉的 ECDICT + 汉英的 CC-CEDICT）——
//! 词典与词库是多对多关系，见术语表。用户认知里只有「离线词典」这一个东西；
//! 「英汉」「汉英」不是两个词典，只是同一个词典的两个方向（docs/adr/0003）。

use anyhow::Result;

use crate::domain::{Candidate, Dictionary, Direction, Lookup, Query, Wordlist};
use crate::store::cedict::Cedict;
use crate::store::ecdict::Ecdict;

/// 离线词典。
pub struct OfflineDictionary {
    /// 英汉方向的词库。
    ecdict: Ecdict,
    /// 汉英方向的词库。
    cedict: Cedict,
}

impl OfflineDictionary {
    /// 打开两份词库。
    ///
    /// 二者缺一不可：缺了任一份，对应方向的查询就只能谎称「未收录」——而术语表里
    /// 「一无所获」的意思是**词典确实没有这个词**，不是「我没能力查」。与其在运行时
    /// 撒谎，不如在这里失败。
    pub fn open(ecdict_path: &std::path::Path, cedict_path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            ecdict: Ecdict::open(ecdict_path)?,
            cedict: Cedict::open(cedict_path)?,
        })
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
