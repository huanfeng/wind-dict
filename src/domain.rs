//! 领域模型：CONTEXT.md 的可执行形式。
//!
//! 本模块的类型与 CONTEXT.md 的术语一一对应，命名规则直接由术语表推出：
//!
//! | 术语 | 类型 / 标识符 | 术语表禁止的命名 |
//! |------|--------------|-----------------|
//! | 词条 | [`Entry`] | item |
//! | 词头 | [`Headword`] | word, term, query |
//! | 中文释义 | `Entry::zh_definition` | translation（与译源的产出撞名） |
//! | 英英释义 | `Entry::en_definition` | definition（未指明中英，有歧义） |
//! | 词形变化 | [`Inflections`] | exchange |
//! | 原形 | `base_form` | lemma, stem |
//! | 查询词 | [`Query`] | input, keyword |
//! | 查询方向 | [`Direction`] | language |
//! | 查询源 | [`Source`] | provider, backend |
//! | 词典 | [`Dictionary`] | — |
//! | 译源 | [`TranslationSource`] | AI 词典, 在线词典 |
//!
//! 术语表把「词典」与「译源」定义为两类**不同**的东西（docs/adr/0008），因此它们是两个
//! 互不继承的 trait，而非一个统一抽象——这是刻意的，不要「重构」掉。

use std::fmt;

// ── 词头与词条 ────────────────────────────────────────────────

/// 词头：词典中真实存在的那个词，是词条的唯一标识。
///
/// 与 [`Query`] 的分界见术语表：查询词是用户打进去的字符串（可能拼错），
/// 词头是词典里确实收录的词。二者类型不同，防止在代码里被悄悄混用。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Headword(String);

impl Headword {
    /// 仅词库读取路径可构造：词头的存在性由词库背书，不由调用方声称。
    pub(crate) fn from_store(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Headword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 词形变化：同一词头的屈折形态集合。
///
/// 数据源自 ECDICT 的 `exchange` 字段（如 `d:tried/p:tried/i:trying/3:tries`）。
/// 它的存在使本项目**不需要词干提取算法**——这是选择 ECDICT 的额外收益，
/// 也是一个不应被「优化」掉的依赖，见 docs/adr/0001。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inflections {
    /// 本词头的原形。`None` 表示它自己就是原形。
    ///
    /// 术语表禁止称其为 lemma / 词根 / 词干：`tried` 的原形是 `try`，
    /// 而「词根」是构词法概念，本项目不涉及。
    pub base_form: Option<Headword>,
    /// 由本词头（作为原形）派生出的全部形态。
    pub derived: Vec<Headword>,
}

/// 词条：词典中描述一个词的完整信息单元。分两类且仅两类。
///
/// 不合并为「字段并集 + 大量 Option」的统一类型（docs/adr/0009）：两类词条的形状
/// 本就不同，合并会让 `苹果` 的「过去式」、`apple` 的「繁体」变成能编译的代码。
/// 拆开之后，这类无意义访问**根本过不了编译**——这正是拆分要买的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// 英汉词条，源自 ECDICT。
    English(EnglishEntry),
    /// 汉英词条，源自 CC-CEDICT。
    Chinese(ChineseEntry),
}

impl Entry {
    /// 两类词条唯一的共性：都有词头。
    pub fn headword(&self) -> &Headword {
        match self {
            Entry::English(e) => &e.headword,
            Entry::Chinese(e) => &e.headword,
        }
    }
}

/// 英汉词条：英文词头 + 中文释义。
///
/// 中文释义与英英释义是**两个独立并存的字段**，不是同一内容的两种语言版本——
/// 术语表把单独的「释义」列为歧义词，故此处不存在名为 `definition` 的字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnglishEntry {
    pub headword: Headword,
    /// 音标（IPA）。ECDICT 的 `phonetic` 字段，可能缺失。
    ///
    /// 术语表禁止用「音标」指代拼音：音标专指英文的 IPA。
    pub phonetic: Option<String>,
    /// 中文释义。默认展示给用户的那个。ECDICT 的 `translation` 字段。
    pub zh_definition: Option<String>,
    /// 英英释义。默认折叠，用户主动展开才可见。ECDICT 的 `definition` 字段。
    pub en_definition: Option<String>,
    /// 词性。ECDICT 的 `pos` 字段。
    pub pos: Option<String>,
    /// 词形变化。**英汉词条专有**——中文不屈折，故 [`ChineseEntry`] 上没有这个字段。
    pub inflections: Inflections,
}

/// 汉英词条：中文词头 + 英文释义。
///
/// 此处**没有** `inflections` 字段，且这不是遗漏：「苹果的过去式」不是一个有意义的
/// 问题。中文不存在屈折变化，故该字段在此类词条上无处安放——见术语表「词形变化」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChineseEntry {
    /// 词头恒为**简体**。繁体写法另存于 `traditional`。
    pub headword: Headword,
    /// 繁体写法。与词头相同时亦照存——是否相同由数据说了算，不由本层推断。
    pub traditional: String,
    /// 拼音，带声调。CC-CEDICT 用数字声调（`ping2 guo3`），`ü` 写作 `u:`。
    ///
    /// 术语表禁止称其为「音标」。
    pub pinyin: String,
    /// 英文释义，按义项切分。**至少一个**——无义项的词条不该被构造出来。
    ///
    /// 注意这是「中文词头的英文解释」，**不是**英英释义（后者是英文词头的英文解释）。
    pub senses: Vec<Sense>,
    /// 量词（如 `苹果` 配 `个`、`颗`）。**汉英词条专有**。
    pub classifiers: Vec<String>,
}

/// 义项：词头的一个独立含义。
///
/// 一个义项可有多种措辞（CC-CEDICT 中以 `;` 分隔），它们是同一含义的不同说法，
/// 而非不同含义——如 `(of people) sturdy; tough` 是**一个**义项的两种措辞。
/// 术语表禁止把义项与措辞混作一谈。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sense {
    pub glosses: Vec<String>,
}

// ── 查询词与方向 ──────────────────────────────────────────────

/// 查询方向：一次查询是英→中还是中→英。
///
/// 方向是**查询词的属性**，不是查询源的维度：不存在「汉英词典」，只存在离线词典
/// 在收到中文查询词时所走的汉英方向。见 docs/adr/0003。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 英→中：查 ECDICT 词库。
    EnToZh,
    /// 中→英：查 CC-CEDICT 词库。
    ZhToEn,
}

/// 查询词：用户当前输入的、待查询的那个词。
///
/// 全局唯一，为所有查询源共享——切换查询源不改变查询词，只改变由谁来解释它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    text: String,
    direction: Direction,
}

impl Query {
    /// 由用户输入构造，方向自动判定。空白输入返回 `None`——空查询不是查询。
    pub fn new(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Self {
            direction: Direction::detect(text),
            text: text.to_string(),
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }
}

impl Direction {
    /// 由查询词判定方向：含任一 CJK 汉字即视为中文，否则视为英文。
    ///
    /// 「含任一汉字即中文」是一条**刻意粗糙**的规则，它决定了几个边界情况：
    /// - `iPhone 手机` → 中文（含汉字）
    /// - `123` / `!!!` → 英文（无汉字，走 ECDICT，查无结果）
    ///
    /// docs/adr/0003 已记录：混杂输入的默认规则尚未定案。此处先取「有汉字即中文」，
    /// 理由是中文用户在英文词中夹汉字的场景，远比反之罕见。
    fn detect(text: &str) -> Direction {
        if text.chars().any(is_cjk_ideograph) {
            Direction::ZhToEn
        } else {
            Direction::EnToZh
        }
    }
}

/// 是否为 CJK 汉字。
///
/// 只认汉字本体，**不认**中文标点（`，。！`）与全角字符：
/// 输入 `hello，world` 应当走英汉方向，一个中文逗号不该把它变成中文查询。
fn is_cjk_ideograph(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF     // CJK 统一表意文字
        | 0x3400..=0x4DBF   // 扩展 A
        | 0xF900..=0xFAFF   // 兼容表意文字
    )
}

// ── 查询源 ────────────────────────────────────────────────────

/// 词典的查询结果。
///
/// 「一无所获」是**词典专有**的状态：词典不会编造它没有的东西。译源没有这个状态——
/// 它对任何输入都会给出些什么，哪怕是错的。这正是二者不共用一个 trait 的原因之一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// 命中。`via_base_form` 记录是否经词形变化落到原形上（如查 `tried` 得到 `try`），
    /// 界面需据此提示「显示的是 try 的词条」，否则用户会困惑。
    Found {
        entries: Vec<Entry>,
        via_base_form: bool,
    },
    /// 未收录。此时界面提示用户可切换到译源——但**绝不自动切**，见 docs/adr/0002。
    NotFound,
}

/// 译源的产出：一段文本，没有音标、词性、词形变化，也没有「收录与否」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub text: String,
    /// 该文本是否由 AI 生成。为 `true` 时界面**必须**显式标注「由 AI 生成，可能有误」。
    ///
    /// 这不是可选的润色：词典的结果有出处，AI 的没有。若二者以相同样式呈现，
    /// 用户没有任何线索区分「查到的」与「编的」。见 docs/adr/0008。
    pub generated: bool,
}

/// 词典：查出词条的查询源。结果有确定出处，查不到就是查不到。
///
/// 注意此 trait **没有补全方法**。补全需要一份词表，而不是每个词典都有词表
/// （远程词典就没有）。补全是离线词典的专属能力，见 [`Wordlist`]。
pub trait Dictionary {
    /// 用户可见的名字。
    fn name(&self) -> &str;

    fn lookup(&self, query: &Query) -> anyhow::Result<Lookup>;
}

/// 译源：产出译文或生成文本的查询源。结果算/生成出来，无出处。
pub trait TranslationSource {
    fn name(&self) -> &str;

    /// 译源直接吞下查询词原样，不做「解析为词头」这层工作——它不知道存在哪些词。
    fn translate(&self, query: &Query) -> anyhow::Result<Translation>;
}

/// 查询源：词典与译源的并集，且**仅有这两类**。
///
/// 这里用 enum 而非统一 trait，是 docs/adr/0008 的直接落实：二者可信度不同、
/// 数据形状不同，统一抽象必然把词条压扁成纯文本，牺牲离线词典的全部结构优势。
pub enum Source {
    Dictionary(Box<dyn Dictionary>),
    Translation(Box<dyn TranslationSource>),
}

impl Source {
    pub fn name(&self) -> &str {
        match self {
            Source::Dictionary(d) => d.name(),
            Source::Translation(t) => t.name(),
        }
    }
}

// ── 补全 ──────────────────────────────────────────────────────

/// 补全候选：一个词头，加上给用户的一行预览。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub headword: Headword,
    /// 一行中文释义预览，便于用户在候选列表里直接认出要的那个。
    pub preview: Option<String>,
}

/// 词表：能够按前缀列出候选词头的能力。
///
/// **补全永远由离线词典驱动，与当前查询源无关**——因为补全需要一份词表，而词表只有
/// 词典有：译源没有词库，它根本不知道世上存在哪些词。即便用户选中的是 AI 译源，
/// 打字时的候选词依然来自离线词典。
///
/// 由此还推出一条硬性规则：**查询源永不被逐键触发**。只有当查询词确定下来
/// （回车、选中候选、切换查询源）时，当前查询源才被调用。否则打 `serendipity`
/// 会变成 11 次 AI 请求。
pub trait Wordlist {
    /// 按前缀列出候选，**按词频排序**（高频优先），而非字典序。
    ///
    /// 词频来自 ECDICT 的 `frq` / `bnc` 字段。这让打 `app` 的首选是 `apple`
    /// 而不是 `appalachia`——体验差别巨大，而代价只是一个 ORDER BY。
    fn complete(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<Candidate>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空查询词不构成查询() {
        assert!(Query::new("").is_none());
        assert!(Query::new("   ").is_none());
    }

    #[test]
    fn 查询词自动去除首尾空白() {
        let q = Query::new("  apple  ").unwrap();
        assert_eq!(q.text(), "apple");
    }

    #[test]
    fn 纯英文走英汉方向() {
        assert_eq!(Query::new("apple").unwrap().direction(), Direction::EnToZh);
    }

    #[test]
    fn 含汉字走汉英方向() {
        assert_eq!(Query::new("苹果").unwrap().direction(), Direction::ZhToEn);
    }

    #[test]
    fn 中英混杂含汉字则视为中文() {
        // docs/adr/0003 记录的待定规则，此处先取「有汉字即中文」。
        assert_eq!(
            Query::new("iPhone 手机").unwrap().direction(),
            Direction::ZhToEn
        );
    }

    #[test]
    fn 中文标点不改变方向() {
        // 一个中文逗号不该把英文查询变成中文查询。
        assert_eq!(
            Query::new("hello，world").unwrap().direction(),
            Direction::EnToZh
        );
    }

    #[test]
    fn 数字与符号走英汉方向() {
        assert_eq!(Query::new("123").unwrap().direction(), Direction::EnToZh);
        assert_eq!(Query::new("!!!").unwrap().direction(), Direction::EnToZh);
    }
}
