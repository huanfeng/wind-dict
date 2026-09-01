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
    /// 由本词头（作为原形）派生出的全部形态，**带形态种类**。
    ///
    /// 种类不可丢：界面要说的是「made 是过去式」，而不只是「还有个词叫 made」。
    /// 此处曾是 `Vec<Headword>`，把种类扔在了解析阶段——数据在库里、代码里却看不见。
    pub derived: Vec<(InflectionKind, Headword)>,
}

/// 词形变化的种类。
///
/// 只列 ECDICT `exchange` 字段实际提供的这几种。不做成开放的字符串：种类是**有限
/// 且已知**的，用枚举才能让界面穷尽处理，也才能给出中文标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflectionKind {
    /// 过去式（`p`）。
    Past,
    /// 过去分词（`d`）。
    PastParticiple,
    /// 现在分词（`i`）。
    Present,
    /// 第三人称单数（`3`）。
    ThirdPerson,
    /// 复数（`s`）。
    Plural,
    /// 比较级（`r`）。
    Comparative,
    /// 最高级（`t`）。
    Superlative,
}

impl InflectionKind {
    /// ECDICT `exchange` 的码。
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "p" => Self::Past,
            "d" => Self::PastParticiple,
            "i" => Self::Present,
            "3" => Self::ThirdPerson,
            "s" => Self::Plural,
            "r" => Self::Comparative,
            "t" => Self::Superlative,
            _ => return None,
        })
    }

    /// 界面上的中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Past => "过去式",
            Self::PastParticiple => "过去分词",
            Self::Present => "现在分词",
            Self::ThirdPerson => "第三人称",
            Self::Plural => "复数",
            Self::Comparative => "比较级",
            Self::Superlative => "最高级",
        }
    }
}

/// 考试大纲标记。ECDICT `tag` 字段的取值，以空格分隔。
///
/// 与 [`InflectionKind`] 同理不做成开放字符串：取值是**有限且已知**的八种，用枚举
/// 才能让界面穷尽处理并给出中文标签。未知码直接丢弃——把 `tag` 里的意外取值原样
/// 渲染成徽章，界面上会冒出没人认识的字母。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExamTag {
    /// 中考（`zk`）。
    Zhongkao,
    /// 高考（`gk`）。
    Gaokao,
    /// 四级（`cet4`）。
    Cet4,
    /// 六级（`cet6`）。
    Cet6,
    /// 考研（`ky`）。
    Kaoyan,
    /// 托福（`toefl`）。
    Toefl,
    /// 雅思（`ielts`）。
    Ielts,
    /// GRE（`gre`）。
    Gre,
}

impl ExamTag {
    /// ECDICT `tag` 字段的码。
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "zk" => Self::Zhongkao,
            "gk" => Self::Gaokao,
            "cet4" => Self::Cet4,
            "cet6" => Self::Cet6,
            "ky" => Self::Kaoyan,
            "toefl" => Self::Toefl,
            "ielts" => Self::Ielts,
            "gre" => Self::Gre,
            _ => return None,
        })
    }

    /// 界面上的中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zhongkao => "中考",
            Self::Gaokao => "高考",
            Self::Cet4 => "四级",
            Self::Cet6 => "六级",
            Self::Kaoyan => "考研",
            Self::Toefl => "托福",
            Self::Ielts => "雅思",
            Self::Gre => "GRE",
        }
    }

    /// 学习阶段由浅入深的次序。
    ///
    /// 库里 `tag` 的字符串顺序是录入顺序（`support` 存的是 `gk cet4 cet6 ky ielts`），
    /// 并不保证递进。界面上并排展示时，乱序的难度标签读起来没有意义，故排序而非
    /// 原样呈现。
    fn rank(self) -> u8 {
        match self {
            Self::Zhongkao => 0,
            Self::Gaokao => 1,
            Self::Cet4 => 2,
            Self::Cet6 => 3,
            Self::Kaoyan => 4,
            Self::Toefl => 5,
            Self::Ielts => 6,
            Self::Gre => 7,
        }
    }
}

/// 词汇分级：这个词有多重要、归属哪些考试大纲。
///
/// 五项分别来自 ECDICT 的 `collins` / `oxford` / `tag` / `bnc` / `frq`。聚合成一个
/// 结构而非平铺进 [`EnglishEntry`]，理由与 [`Inflections`] 相同：它们回答的是同一个
/// 问题（「这词值不值得记」），且**汉英词条上一项都没有**——CC-CEDICT 不带任何词频
/// 与分级信号。聚合后这份差异在类型上一目了然。
///
/// 这些字段此前被构建期丢弃（见 docs/adr/0010 的修订）。全库覆盖率虽低
/// （`collins` 1.8%、`oxford` 0.4%、`tag` 1.9%），但它们**只标常用词**——恰恰是
/// 用户真正会查的那批。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grading {
    /// 柯林斯星级，1–5。`None` = 未评级。
    pub collins: Option<u8>,
    /// 是否属于牛津三千核心词汇。
    pub oxford: bool,
    /// 所属考试大纲，已按学习阶段由浅入深排序。
    pub tags: Vec<ExamTag>,
    /// 英国国家语料库词频排名。越小越高频，`None` = 未进榜。
    ///
    /// 与 `frq` 并存不是冗余：BNC 统计数百年来的英文资料，当代语料库只统计近 20 年。
    /// `quay`（码头）在当代语料库排两万开外，在 BNC 却排第 8906——读旧书时它是高频词。
    pub bnc: Option<u32>,
    /// 当代语料库词频排名。越小越高频，`None` = 未进榜。
    pub frq: Option<u32>,
}

impl Grading {
    /// 是否没有任何可展示的分级信息。
    ///
    /// 界面据此决定要不要画那一行徽章——全空时连容器都不该建，否则词条上会多出
    /// 一道没有内容的间距。
    pub fn is_empty(&self) -> bool {
        self.collins.is_none()
            && !self.oxford
            && self.tags.is_empty()
            && self.bnc.is_none()
            && self.frq.is_none()
    }

    /// 解析 ECDICT 的 `tag` 字段：空格分隔的码，未知码丢弃，结果按阶段排序去重。
    pub fn parse_tags(tag: &str) -> Vec<ExamTag> {
        let mut v: Vec<ExamTag> = tag
            .split_whitespace()
            .filter_map(ExamTag::from_code)
            .collect();
        v.sort_by_key(|t| t.rank());
        v.dedup();
        v
    }
}

/// 一组同词性的释义。
///
/// ECDICT 的 `translation` 字段本身是有结构的——每行「词性 + 该词性下的释义」，如
/// `vt. 制造, 安排, 创造`。把整块字符串直接渲染成一个标签，等于把这份结构丢了：
/// 词性变成混在中文里的普通字符，所有信息挤在同一个视觉层次。
///
/// 抽样 2000 个高频词实测，**82% 的释义行带词性前缀**，值得解析。剩下 18% 没有前缀
/// 的行照样保留——`pos` 为 `None`、整行进 `senses`，不因解析不出而丢内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gloss {
    /// 词性缩写，如 `vt.` / `n.`。`None` = 该行没有可识别的词性前缀。
    pub pos: Option<String>,
    /// 该词性下的各个释义。
    pub senses: Vec<String>,
}

/// 把 ECDICT 的 `translation` 解析成按词性分组的释义。
///
/// **解析失败不丢内容**是这个函数的第一要务：词典的本分是把词库里有的东西呈现出来，
/// 为了排版好看而吞掉一行释义，是本末倒置。故任何无法识别的行都原样进 `senses`。
///
/// 放在领域层而非 `store::ecdict`：解析结果（词性 + 义项）是**领域形状**，与汉英词条
/// 的 `Sense` 同级；而且它是纯函数，脱开数据库就能测。将来界面换用富文本控件时，
/// 渲染那一层会重写，这一层不动。
pub fn parse_glosses(translation: &str) -> Vec<Gloss> {
    translation
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match split_pos(line) {
            Some((pos, rest)) => Gloss {
                pos: Some(pos.to_string()),
                senses: split_senses(rest),
            },
            None => Gloss {
                pos: None,
                senses: split_senses(line),
            },
        })
        .collect()
}

/// 切出行首的词性前缀。要求是「若干 ASCII 字母 + 点 + 空白」，如 `vt. `。
///
/// 限定 ASCII 字母是为了不误伤中文：中文释义里出现「甲.」这类写法时不该被当成词性。
fn split_pos(line: &str) -> Option<(&str, &str)> {
    let dot = line.find('.')?;
    let (head, tail) = line.split_at(dot + 1);
    let word = &head[..head.len() - 1];
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    // 点后必须跟空白，否则 `U.S.A` 这类会被切成词性。
    if !tail.starts_with(char::is_whitespace) {
        return None;
    }
    Some((head, tail.trim_start()))
}

/// 按逗号切分同词性下的多个释义。中英文逗号都切——词库两种都在用。
fn split_senses(s: &str) -> Vec<String> {
    s.split([',', '，'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

// ── 富文本 ────────────────────────────────────────────────
//
// 自带词典（MDX）的正文没有字段，只有**带样式的段落**。这套模型放在领域层而不是
// `crate::html`，是因为它描述的是「词典正文长什么样」，与 HTML 无关——HTML 只是它
// 众多可能来源里的一个，`html::to_blocks` 是转换器，不是定义者。反过来让领域类型
// 依赖 `html`，会把「解析格式」这件事钉进领域模型。

/// 一段文字里的一截，样式一致。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// 指向另一个词头的跳转目标。**只留词典内部的跳转**——外部 URL 在一个离线词典里
    /// 点了也没有意义，留着只会给用户一个按不动的链接。
    pub link: Option<String>,
}

/// 一个段落。`indent` 是列表嵌套层级，0 为不缩进。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextBlock {
    pub indent: u8,
    pub runs: Vec<TextRun>,
}

impl TextBlock {
    /// 本段的纯文字。判空与测试用。
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

// ── 词条 ──────────────────────────────────────────────────

/// 词条：词典中描述一个词的完整信息单元。分三类，且**每一类都有自己真实的形状**。
///
/// 不合并为「字段并集 + 大量 Option」的统一类型（docs/adr/0009）：几类词条的形状
/// 本就不同，合并会让 `苹果` 的「过去式」、`apple` 的「繁体」变成能编译的代码。
/// 拆开之后，这类无意义访问**根本过不了编译**——这正是拆分要买的东西。
///
/// ADR-0009 原文写的是「分两类且仅两类」。[`Entry::User`] 是后来加的第三类，这**不是**
/// 对那份决定的违反而是它的应用：自带词典的正文既没有音标也没有词性，只有带样式的
/// 段落，硬塞进前两类中的任何一个都要新增一批永远为 `None` 的字段——那正是 ADR-0009
/// 要挡的事。判据是「形状是否真的不同」，不是「变体数量」。
///
/// 与 ADR-0013 拒绝把字形做成第三类词条也不冲突：那次的理由是**字形没有释义**，
/// 让它与真词条并列会让「查到了」这句话在两种情况下含义不同。自带词典有释义，
/// 查到它就是查到了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// 英汉词条，源自 ECDICT。
    English(EnglishEntry),
    /// 汉英词条，源自 CC-CEDICT。
    Chinese(ChineseEntry),
    /// 自带词典的词条，源自用户放进来的 MDX。
    User(UserEntry),
}

impl Entry {
    /// 三类词条唯一的共性：都有词头。
    pub fn headword(&self) -> &Headword {
        match self {
            Entry::English(e) => &e.headword,
            Entry::Chinese(e) => &e.headword,
            Entry::User(e) => &e.headword,
        }
    }
}

/// 自带词典的词条：词头 + 一段富文本，没有任何字段化信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEntry {
    pub headword: Headword,
    /// 出处词典的名字。
    ///
    /// **必须显示给用户**，且这不是装饰。随程序分发的两个词库是我们挑过的，自带词典
    /// 是用户自己放进来的、来源与质量我们一无所知。两者以相同样式并排出现时，用户
    /// 没有任何线索区分。这与 ADR-0008 要求标注「由 AI 生成」是同一条理由的另一面。
    pub source: String,
    /// 出处词典的**稳定键**（文件名），与 `source` 那个显示名分开。
    ///
    /// 页签按来源筛词条，筛的依据必须是键：显示名可以被用户改、也可能撞车（两个
    /// 文件的 MDX 标题一模一样是常有的事），拿它当依据会把两本词典的词条混进
    /// 同一页，且用户一改名字筛选就失灵。
    pub source_key: String,
    pub body: Vec<TextBlock>,
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
    /// 词汇分级。**英汉词条专有**——CC-CEDICT 不带词频与大纲信号，见 [`Grading`]。
    pub grading: Grading,
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

// ── 字形 ────────────────────────────────────────────

/// 字形属性：一个**汉字**的部首、笔画与繁简对应。源自 Unihan。
///
/// 这不是第三类词条，故不进 [`Entry`]：`行` 有三个读音、三条汉英词条，却只有一副
/// 字形（部首 行、总 6 画）。挂到 [`ChineseEntry`] 上要存三份，且暗示「字形随读音变」
/// ——它不变；反过来 `苹果` 是两个字，没有单一字形可言。
///
/// 字形属于**字**，词条属于**词**，故独立成查。这与 docs/adr/0009 拒绝合并两类词条
/// 是同一个论证的另一面：那次拒绝的是「把两种形状塞进一个类型」，这次拒绝的是
/// 「把字的属性塞进词的记录」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyph {
    /// 字本身。
    pub ch: char,
    /// 部首字形，**已解出简化形**——`语` 的部首是 `讠` 而非 `言`。
    ///
    /// 简体常用字几乎全部使用简化部首形（实测 3,877 字），若退回康熙基本形，
    /// 界面上大批常用字的部首都会是错的。
    pub radical: char,
    /// 康熙部首号，1–214。`radical` 只够显示，要按部首归类得靠这个号。
    pub radical_no: u8,
    /// 部首外笔画。**可为负**（实测 45 字，如 `125.-1`）——字比其部首本身还少一笔时
    /// Unihan 记负数。故此处是 `i8` 而非 `u8`：后者会在那 45 个字上静默回绕成 255。
    pub extra_strokes: i8,
    /// 总笔画。
    pub total_strokes: u8,
    /// 对应的简体写法。本字已是简体时为空。可能多于一个。
    pub simplified: Vec<char>,
    /// 对应的繁体写法。本字已是繁体时为空。可能多于一个。
    pub traditional: Vec<char>,
    /// 普通话读音，带调号（`yǔ`），按常用度排序。**大陆标准**。
    ///
    /// 取自 Unihan 的 `kXHC1983`（《现代汉语词典》）而非更新的 `kTGHZ2013`
    /// （《通用规范汉字字典》，2013），理由来自界面而非数据：后者对 `语` 只给 `yǔ`，
    /// 而 CC-CEDICT 在同一张卡片下列着 `[yu3]` 与 `[yu4]` 两条词条。字形行的读音若
    /// 少于下面列出的词条，卡片就在自相矛盾。详见 docs/adr/0014。
    ///
    /// 与 [`ChineseEntry::pinyin`] 不重复也不冲突：那是**词条**的读音（每条一个，
    /// 数字调），这是**字**的读音全集（带调号）。同 `Glyph` 与 `Entry` 的分工。
    pub readings: Vec<String>,
    /// 《通用规范汉字表》的级别。`None` = 不在这 8105 字之内。
    pub tier: Option<CharTier>,
}

/// 《通用规范汉字表》（国务院 国发〔2013〕23 号）的字级。
///
/// 这是**大陆官方**对「哪些字重要」的回答，作用等同于 [`Grading`] 之于英文词——
/// 一级 3500 字是义务教育与日常出版的常用字，三级多为姓氏、地名、科技术语用字。
///
/// 该字表属《著作权法》第五条所指「国家机关的……行政性质的文件」，不适用著作权法，
/// 故可自由使用。这一点与其载体仓库挂什么许可无关：没有人能对政府规范性文件主张权利。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CharTier {
    /// 一级字，3500 个。
    Level1,
    /// 二级字，3000 个。
    Level2,
    /// 三级字，1605 个。
    Level3,
}

impl CharTier {
    /// 由级号构造。1/2/3 之外返回 `None`——库里存的是整数，不能假定它一定合法。
    pub fn from_level(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Level1),
            2 => Some(Self::Level2),
            3 => Some(Self::Level3),
            _ => None,
        }
    }

    /// 级号，1–3。
    pub fn level(self) -> u8 {
        match self {
            Self::Level1 => 1,
            Self::Level2 => 2,
            Self::Level3 => 3,
        }
    }

    /// 界面用的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Level1 => "一级字",
            Self::Level2 => "二级字",
            Self::Level3 => "三级字",
        }
    }
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

// ── 收藏与历史 ────────────────────────────────────────────────

/// 收藏项：一个被用户主动标记的词头，附收藏时间与可选备注。
///
/// 收藏是**意图**——用户表达「我在意这个词」。它是纯粹的书签，**不承载**掌握程度、
/// 复习次数、复习计划等学习状态（本项目不存在「生词本」概念，见术语表「收藏」）。
/// 故这里只有 `note` 一个自由字段，没有任何学习进度字段——这是刻意的边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    pub headword: Headword,
    /// 收藏时间（Unix 纪元秒）。
    pub added_at: i64,
    /// 用户备注。书签语义下唯一的附加信息。
    pub note: Option<String>,
}

/// 历史项：一个查过的词头，附最近一次查询的时间。
///
/// 历史是**事实**——系统被动记录「这个词被查过」，不代表用户在意它（与收藏的分界，
/// 见术语表）。同一词头重复查询只**更新时间**、不新增条目，故这里没有「查询次数」
/// 之类的字段：历史回答「我最近查了什么」，不做统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub headword: Headword,
    /// 最近一次查询的时间（Unix 纪元秒）。
    pub looked_up_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 释义按词性分组() {
        let g = parse_glosses("vt. 制造, 安排\nvi. 开始, 前进\nn. 构造");
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].pos.as_deref(), Some("vt."));
        assert_eq!(g[0].senses, vec!["制造", "安排"]);
        assert_eq!(g[2].pos.as_deref(), Some("n."));
        assert_eq!(g[2].senses, vec!["构造"]);
    }

    /// **解析不出也不能丢内容**。词典的本分是把词库里有的东西呈现出来，为了排版
    /// 好看而吞掉一行释义是本末倒置。实测约 18% 的行没有词性前缀。
    #[test]
    fn 无词性前缀的行原样保留() {
        let g = parse_glosses("这一行没有词性前缀");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].pos, None);
        assert_eq!(g[0].senses, vec!["这一行没有词性前缀"]);
    }

    /// 中英文逗号都要切——词库里两种都在用。
    #[test]
    fn 中英文逗号都切分() {
        let g = parse_glosses("n. 苹果，苹果树, 苹果公司");
        assert_eq!(g[0].senses, vec!["苹果", "苹果树", "苹果公司"]);
    }

    /// 点后没有空白的不算词性前缀，否则 `U.S.A` 会被切成「U.」+ 「S.A」。
    #[test]
    fn 缩写不被误判为词性() {
        let g = parse_glosses("U.S.A 美国");
        assert_eq!(g[0].pos, None, "缩写不该被当成词性");
        assert_eq!(g[0].senses, vec!["U.S.A 美国"]);
    }

    /// 中文的「甲.」之类不该被当成词性——限定 ASCII 字母。
    #[test]
    fn 中文不被误判为词性() {
        let g = parse_glosses("甲. 某种解释");
        assert_eq!(g[0].pos, None);
    }

    #[test]
    fn 空串与空行不产出条目() {
        assert!(parse_glosses("").is_empty());
        assert!(parse_glosses("\n\n  \n").is_empty());
    }

    /// 形态种类必须随词保留，界面才说得出「made 是过去式」。
    #[test]
    fn 形态种类可还原为中文标签() {
        assert_eq!(InflectionKind::from_code("p").unwrap().label(), "过去式");
        assert_eq!(InflectionKind::from_code("3").unwrap().label(), "第三人称");
        assert_eq!(InflectionKind::from_code("1"), None, "变换类型标记不是形态");
        assert_eq!(InflectionKind::from_code("0"), None, "原形另行处理");
    }

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

    // ── 词汇分级 ──────────────────────────────────────────

    /// 库里的 `tag` 是**录入顺序**，不保证由浅入深。
    ///
    /// 断言的字面量取自真实数据：`support` 在 ECDICT 里存的正是 `gk cet4 cet6 ky ielts`。
    #[test]
    fn 大纲标签按学习阶段排序而非录入顺序() {
        assert_eq!(
            Grading::parse_tags("gk cet4 cet6 ky ielts"),
            vec![
                ExamTag::Gaokao,
                ExamTag::Cet4,
                ExamTag::Cet6,
                ExamTag::Kaoyan,
                ExamTag::Ielts
            ]
        );
        // 完全乱序的输入同样归位。
        assert_eq!(
            Grading::parse_tags("gre zk cet4"),
            vec![ExamTag::Zhongkao, ExamTag::Cet4, ExamTag::Gre]
        );
    }

    /// 未知码丢弃，不渲染成徽章。
    ///
    /// 词库可被外部文件整体替换，`tag` 里出现什么不由我们说了算。原样渲染的话，
    /// 界面上会冒出一排没人认识的字母。
    #[test]
    fn 未知大纲码被丢弃() {
        assert_eq!(
            Grading::parse_tags("cet4 xyz 中文 cet6"),
            vec![ExamTag::Cet4, ExamTag::Cet6]
        );
        assert!(Grading::parse_tags("").is_empty());
        assert!(Grading::parse_tags("   ").is_empty());
    }

    #[test]
    fn 重复大纲码只留一个() {
        assert_eq!(Grading::parse_tags("cet4 cet4 cet4"), vec![ExamTag::Cet4]);
    }

    /// `is_empty` 决定界面画不画那一行徽章，故它必须对「只有一项有值」保持敏感。
    #[test]
    fn 分级只要有一项有值就不算空() {
        assert!(Grading::default().is_empty());
        for g in [
            Grading {
                collins: Some(1),
                ..Default::default()
            },
            Grading {
                oxford: true,
                ..Default::default()
            },
            Grading {
                tags: vec![ExamTag::Cet4],
                ..Default::default()
            },
            Grading {
                bnc: Some(1),
                ..Default::default()
            },
            Grading {
                frq: Some(1),
                ..Default::default()
            },
        ] {
            assert!(!g.is_empty(), "有值的分级不该被判为空：{g:?}");
        }
    }
}
