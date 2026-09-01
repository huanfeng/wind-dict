//! 界面：热键唤起 → 输入 → 实时补全 → 选中查询。
//!
//! ## 补全为何不轮询
//!
//! `Signal` 没有订阅机制，只有 `version()`。若用 `App::on_interval` 定期比对版本，
//! 会让进程即便闲着也周期性醒来——直接破坏 windui「空闲零 CPU」这条核心指标。
//!
//! 正确做法是挂一个 `reactive` 控件，靠 windui 的 `Widget::on_update` 相位驱动：
//!
//! ```text
//! 打字 → text_input 写 Signal<String> → 请求重绘 → 触发 relayout
//!      → layout 前广播 on_update → Completer 发现 version 变了 → 查补全
//!      → 写候选 Signal → DynList 的 on_update 感知 → 重建候选行
//! ```
//!
//! 整条链只在有输入时才动，空闲时全部静止。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windui::core::{EventCtx, Widget};
use windui::event::{Event, KeyEvent, PointerKind, ShortcutCtx};
use windui::prelude::*;
// 分隔条自绘那条竖线要用（`PaneSplitter::paint`）。它们不在 prelude 里——绝大多数
// 应用不自绘，本项目也只有这一处。
use windui::render::{Canvas, Paint};
// 不在 `prelude` 里：右栏那排页签要的是**只有标签条**的 `TabBar`，而 prelude 只导出
// 连内容区一起打包的 `Element::tabs`。理由见 `dict_tab_bar`。
use windui::ui::containers::{TabBar, TabItem};

use crate::domain::{
    Candidate, CharTier, Dictionary, Entry, Glyph, Headword, Lookup, Query, Sense, UserEntry,
    Wordlist,
};
use crate::settings::Settings;
use crate::skin::{SkinMode, SkinStyle};
use crate::source::offline::OfflineDictionary;
use crate::source::user::UserDictionary;
use crate::store::userdata::{now_secs, UserDataState};

/// 补全候选一次最多列几条。
///
/// 候选从「浮在结果之上的浮层」改成左栏里的常驻可滚列表之后，原先那条「7 条与有道
/// 相当、且 278px 的浮层在 620px 窗口里盖不住结果区太多」的理由整个作废了：列表在
/// 自己那一栏里，滚得动，也不遮任何东西。放宽到 40——够用户往下多扫一屏，也仍是
/// 「缩小范围的工具」而非穷举词表。
///
/// 另一层理由与显示无关，故**不随布局改变**：单字母前缀（如 `a`）会命中约 5 万行，
/// 且 `ORDER BY frq` 用不上索引（索引在 `(sw, word)` 上），SQLite 必须全排一遍——
/// 实测约 20ms。LIMIT 不减少排序量，但它是唯一能钳住内存与渲染开销的地方。所以这里
/// 仍必须是一个具体上界，不能因为「列表能滚」就改成不限。
const MAX_CANDIDATES: usize = 40;

/// 监视查询词、驱动补全的响应式控件。
///
/// 它不绘制任何东西——只是挂在树上，借 `on_update` 相位工作。**必须先于候选列表
/// 构建**：`on_update` 按注册顺序广播（注册即 `Element::build` 的深度优先顺序），
/// 排在列表之后会让候选慢一帧。
struct Completer {
    dict: Rc<OfflineDictionary>,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
    /// 键盘游标。换一批候选就归零——游标指的是「第几条」，而那批候选已经换人了，
    /// 留着旧下标会让高亮停在一个与上次毫不相干的词上。
    cursor: Signal<usize>,
    /// 左栏当前页签。补全一有结果就把左栏拨到候选页，见 `on_update` 末尾。
    left_tab: Signal<usize>,
    /// 上次见到的查询词版本。
    last_version: u64,
}

impl Widget for Completer {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.query.version();
        if v == self.last_version {
            return;
        }
        self.last_version = v;

        let text = self.query.get();

        // 补全**永远由离线词典驱动**，与用户选中哪个查询源无关——补全需要词表，
        // 而词表只有词典有（译源没有词库，不知道世上存在哪些词）。见术语表「补全」。
        let list = self
            .dict
            .complete(&text, MAX_CANDIDATES)
            .unwrap_or_default();
        let empty_query = text.trim().is_empty();
        self.candidates.set(list);
        // 游标归零。**不必再手动触发重建**：`LeftPaneLoader` 盯着 `cursor` 的版本，
        // 而 `Signal::set` 无条件递增版本（windui `signal.rs:163`，不比较新旧值），
        // 故即便游标本来就是 0，这一句也照样把列表叫醒。
        self.cursor.set(0);

        // 左栏跟着**打字**走——注意只跟打字，不跟查询。
        //
        // 本方法是打字的唯一入口（它由 `query` 的版本驱动，而查询词只在用户输入和
        // Tab 补全时才变），故这里拨页签影响不到「查完之后停在哪」：查询不动查询框，
        // 也就叫不醒本方法。这是刻意的，见 `State::select`。
        //
        // 两条规则，其余情形一律**不动**页签（用户手点的选择要留得住）：
        //
        // - 查询词被清空 → 回历史页。「最近查过什么」正是空框时该有的内容，而一个空的
        //   候选页什么也没说。
        // - 否则 → 候选页。注意**没有候选时也切**：那时候选页会写明「没有以 xx 开头
        //   的词」（见 `State::reload_candidates`），这是一句真实的回答；停在历史页则
        //   等于对用户刚敲进去的那串字母不置一词。
        self.left_tab.set(if empty_query {
            LEFT_HISTORY
        } else {
            LEFT_CANDIDATES
        });
    }
}

/// 左栏页签：补全候选 / 历史记录 / 收藏。
///
/// 收藏**不叫「生词本」**——设计稿此处写的是生词本，但术语表明令该词弃用：本项目的
/// 收藏是纯粹的书签语义，不承载掌握程度与复习计划，叫生词本会招来那一整套学习状态。
///
/// 三段共用**一个**列表（见 `LeftRow`），而不是三个列表各挂 `visible_when`：
/// `on_update` 的派发只看 `enabled` 不看 `visible`，隐藏的列表照样跟着重建，
/// 每次数据变更都是三倍的节点。
const LEFT_CANDIDATES: usize = 0;
const LEFT_HISTORY: usize = 1;
const LEFT_FAVORITES: usize = 2;

/// 右栏页签：全部 / 英汉 / 汉英。
///
/// 切的是**词库方向**，不是「两个词典」——离线词典是一个词典，英汉与汉英是它的两个
/// 方向（ADR-0003、术语表「补全」条）。这排页签因此是个**结果筛选器**，不是查询源
/// 选择器：它不改变查询走哪条路（方向由查询词自动判定，界面上没有方向选择器，也不
/// 该有），只决定已经查出来的卡片显示哪些。
///
/// 由此推出一件必须如实呈现的事：一次查询只走一个方向，故**总有一个方向页是空的**
/// （查 `apple` 时「汉英」必空）。空着不吭声会被读成「坏了」，故有 `filter_note`。
/// 一个页签筛的是哪个来源。
///
/// **页签的轴是来源，不是方向。** 方向（英汉 / 汉英）在单次查询里没有区分力：方向
/// 由查询词推出（ADR-0003），查 `hello` 走英汉、查 `你好` 走汉英，于是那两页永远
/// 是「一页有货、一页空着」。有区分力的是「这条出自哪本词典」。
///
/// 内置那两份词库各占一个页签而不是合成一个「离线词典」，是产品决定：用户要能一眼
/// 看出某条释义来自哪一份，而这两份的编纂来源、授权、质量都不同（见 THIRD-PARTY.md）。
#[derive(Clone, PartialEq, Eq)]
enum TabKey {
    /// 全部来源。
    All,
    /// 随程序分发的词库，值是 `offline::ECDICT_KEY` / `CEDICT_KEY`。
    Builtin(&'static str),
    /// 自带词典，值是文件名（`source::user::key_of`）。
    User(String),
}

/// 一个页签：筛什么、叫什么、这次有没有货。
struct TabSpec {
    key: TabKey,
    label: String,
    /// 本次结果里有没有这个来源的词条。绑给 `TabItem::enabled`。
    ///
    /// **用信号而不是每次重建标签条**：结果每查一次就变，而页签集合只在装卸词典或
    /// 改名时才变。重建会丢掉悬停态、并让选中滑块从头落定而不是滑过去。
    on: Signal<bool>,
}

impl TabSpec {
    fn new(key: TabKey, label: String) -> Self {
        Self {
            key,
            label,
            on: signal(true),
        }
    }
}

/// 按页签筛出要显示的卡片。
///
/// **逐条筛，不是整张卡片筛**：一个词头下面常常同时挂着内置词库与自带词典的词条
/// （查 hello 就是），整张留下等于点进「简明英汉字典」还看得见另一本的内容——那样
/// 页签名就是在撒谎；整张丢掉则那个词头会从它确实收录的那一页里消失。
///
/// 筛空的卡片整张丢掉：只剩一个词头、底下什么都没有的卡片没有信息量，而它会让
/// 「这一页里有几条」这个一眼可数的事实变得不可数。
fn filter_cards(all: Vec<Card>, key: &TabKey) -> Vec<Card> {
    all.into_iter()
        .filter_map(|mut c| {
            c.entries.retain(|e| entry_in_tab(e, key));
            (!c.entries.is_empty()).then_some(c)
        })
        .collect()
}

/// 一条词条属不属于某个页签。
///
/// 内置那两份靠**词条的类型**认：`Entry::English` 只可能来自 ECDICT，
/// `Entry::Chinese` 只可能来自 CC-CEDICT——这不是猜，是 `OfflineDictionary::lookup`
/// 按方向选路的直接结果。自带词典靠**稳定键**认，不能靠显示名：名字可以被用户改、
/// 也可能撞车（两个文件的 MDX 标题一模一样是常有的事）。
fn entry_in_tab(e: &Entry, key: &TabKey) -> bool {
    use crate::source::offline::{CEDICT_KEY, ECDICT_KEY};
    match (key, e) {
        (TabKey::All, _) => true,
        (TabKey::Builtin(k), Entry::English(_)) => *k == ECDICT_KEY,
        (TabKey::Builtin(k), Entry::Chinese(_)) => *k == CEDICT_KEY,
        (TabKey::User(k), Entry::User(u)) => *k == u.source_key,
        _ => false,
    }
}

const DICT_ALL: usize = 0;

/// 左栏一行的高度。
///
/// 三个页签的行**必须同高**，见 `left_row`。键盘导航的滚动跟随也依赖这一点：它按
/// `序号 × 行高` 直接算出目标位置，而不是去树里找那一行的节点——列表内容是响应式
/// 重建的，重建时机与键盘事件不同步，按节点找会算在上一批行上。
const ROW_H: i32 = 36;

/// 左栏一次最多列出的历史条数。左栏是「最近查过什么」，不是全量档案。
const RECALL_LIMIT: usize = 100;

/// Ctrl + 字母 的虚拟键码。
///
/// **带修饰键的字母不是 `Key::Char`**：win32 下 Ctrl+L 的 WM_KEYDOWN 不产生 WM_CHAR，
/// 框架据此把它归为「非文字输入」并交出 `Key::Other(VK)`。照着 `Key::Char('l')` 去
/// match 会静默失配——快捷键按下去毫无反应，而代码看着完全合理。windui 自己的
/// Ctrl+C / Ctrl+A（`ui/rich.rs`、`ui/inputs.rs`）也都是按 `Key::Other` 匹配的。
const VK_L: u32 = 0x4C;
const VK_R: u32 = 0x52;
const VK_W: u32 = 0x57;

/// 快捷键一览：`(按键, 作用)`。
///
/// 它同时是**设置页的展示数据**与**这套键位的说明书**，但**不驱动按键处理**——处理在
/// `handle_shortcut` 与各控件的 `on_nav_key` 里，那些地方要 match 语法，表驱动不了。
///
/// 两处因此必须手动保持一致。之所以还是留下这张表：一套记不住的快捷键等于没有，而
/// 让用户去翻源码或猜是更差的选择。改键位时**两边一起改**。
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl + L", "定位到查询框，并全选已有的词"),
    ("Ctrl + R", "重新查一次当前的词"),
    ("Ctrl + ← / →", "沿查询路径后退 / 前进"),
    ("Ctrl + W", "收起窗口（不退出，热键可再唤起）"),
    ("Esc", "设置页：返回词典；词典页：收起窗口"),
    ("Tab", "焦点：查询框 → 列表 → 页签"),
    ("↑ / ↓", "在左栏列表里移动，右栏实时跟随"),
    ("→", "把选中的候选填进查询框"),
    ("Enter", "查询选中的词，并记入历史"),
    ("Ctrl + C", "复制右栏选中的文字"),
];

/// 标题栏上导航按钮的边长。两枚箭头与它们的居中都按这个值算，见 [`nav_buttons`]。
const NAV_BTN: i32 = 26;

/// 分隔条宽度。
///
/// 视觉上只有中间 1px 是线，其余 5px 是**命中余量**——一条 1px 的线用鼠标几乎抓不住。
/// 6px 是能稳稳抓住、又不至于在两栏之间劈出一道明显缝隙的下限。
const SPLITTER_W: i32 = 6;

/// 右栏的最小可读宽度。
///
/// 拖分隔条时用它反推左栏的上限：无论窗口多窄、用户往右拖多狠，右栏都得留得下一个
/// 30px 的词头加一行释义。没有这条，左栏可以被拖到把释义挤成一列单字。
const RIGHT_MIN_W: i32 = 380;

/// 主列当前显示哪一页。
const PAGE_DICT: usize = 0;
const PAGE_SETTINGS: usize = 1;

/// 词头与词性用的衬线字族。
///
/// 词典的专业观感很大程度来自衬线体——正文用无衬线、词头用衬线是纸质词典的惯例。
/// 取 Georgia 是因为它随 Windows 分发、必然存在，且字面宽、小字号也清晰。
///
/// 中文词头它没有字形，会由系统回退到默认中文字体（无衬线）。这是刻意接受的：
/// Windows 自带的中文衬线只有宋体，大字号下笔画细弱、观感陈旧，回退反而更好。
const SERIF: &str = "Georgia";

/// 词头字号。
///
/// 从 42 收到 30。42 是配那版「右边挂一个 42px 描边星标方块」的词头行定的，两者互相
/// 撑着，看久了整块像一张招牌而不是一个词条标题。星标收到 32、去掉描边之后（见
/// `star`），词头再占 42 就成了孤立的大字。30 仍是全屏最大的字号，主角地位不变。
///
/// 另一层理由与设备有关：本项目的日常使用环境是 200% 缩放的屏幕，42 逻辑像素在那儿
/// 落成 84 物理像素——纸质词典里没有这么大的词头。定字号时只看 1x 截图会漏掉这件事。
const HEADWORD_SIZE: f32 = 30.0;

/// 查询框高度。输入框与叠在其上的清除按钮共用（见 `query_box`）。
///
/// 从 50 收到 42。50 那版的理由是「查询框是这一屏的主控件，给足分量」，但那句话只在
/// **开屏**成立：查完之后主角是词条，而一条 50px 高、横贯整屏的输入框仍是画面最重的
/// 元素，把注意力钉在一个已经用完的控件上。42 仍明显高于普通输入框（windui 默认 32），
/// 开屏时的分量够用，读词条时不再压场。
const QUERY_H: i32 = 42;

/// 强调色淡底的不透明度。用于「你正在看的是这条」的列表行、已收藏星标的底。
///
/// 走 `bg_role_alpha` 而非写死颜色：淡底必须随强调色一起变，写死则换肤后底色与强调
/// 色对不上。取 0.14 与设计稿三套皮肤的 `--accent-2` 观感相当。
const ACCENT_SOFT_A: f32 = 0.14;

/// 键盘游标那条候选的淡底，比 `ACCENT_SOFT_A` 再淡一档。
///
/// 两档而非一档，是因为左栏同时要回答**两个**问题，而它们并不总是同一条：
///
/// - 「我正在看哪个词」——`active`，用满档淡底加粗体。
/// - 「回车会选中哪一条」——`at_cursor`，用这档半淡底。
///
/// 鼠标路径下两者恒重合（点候选会把游标一并挪过去，见 `State::pick_candidate`），
/// 只有用 ↑↓ 预选时才分开——而那正是需要区分的时刻：用户在拿游标比划，眼睛还看着
/// 当前那条的释义。此前只有游标一档，点了别的词之后高亮仍停在原处，指的是一个
/// 早已不在看的词。
const CURSOR_SOFT_A: f32 = 0.06;

/// 正文行高倍数。
///
/// 1.7 是中文正文的常用值：CJK 字身方正、笔画密度高，按字体自带行距排出来会显得
/// 拥挤。只施加在**会换行的多行文字**上（释义、义项）——音标、量词这类单行注解
/// 用不上，给了反而平白拉高行盒。
const BODY_LH: f32 = 1.7;

/// 富文本里各段的样式名。
///
/// 集中成常量而非散着写字面量：`RichDoc::style` 注册的名字与 `Para::styled` 引用的
/// 名字必须一字不差，写错了不会报错——那一段只是**静默退回控件默认样式**，表现为
/// 「某一行莫名其妙变成了正文色」，很难看出是打错了字。
const SPAN_PHONETIC: &str = "phonetic";
const SPAN_POS: &str = "pos";
const SPAN_BODY: &str = "body";
const SPAN_NOTE: &str = "note";
const SPAN_INDEX: &str = "index";
const SPAN_HEADWORD: &str = "headword";

/// 释义正文与词性标记之间的悬挂缩进。
///
/// 词性在段首，释义跟在其后；释义换行时续行缩进到这个位置，与首行的释义文字对齐，
/// 而不是绕回段首顶着词性下面。
///
/// 它取代了 `pos_chip` 当初那个 `min_width(42)` 定宽：定宽在「词性与释义是并排两列」
/// 的行布局里才成立，而富文本里两者在**同一段**内（这正是能连续选中的原因），列的
/// 概念不复存在，对齐只能靠悬挂缩进。
const GLOSS_HANGING: i32 = 52;

/// 英英释义的最大宽度。
///
/// 这里曾是一条 `BODY_MAX_W = 640` 的**整段正文**限宽，已经撤掉：920 宽的窗口配 640
/// 的正文，右侧空出近三百像素，收藏星标孤零零地挂在 640 处离窗口右缘还有一大截，
/// 整个画面左重右空。「限宽护住行长」这条道理没错，但它只对**长句成段**的文字成立，
/// 而这一屏的正文九成是中文释义的顿号并列（「制造、安排、创造、构成…」）——那是词条
/// 列表，不是段落，一行排满反而少占两行、密度更高。
///
/// 真正需要它的只有英英释义：那是成句的英文散文，且默认折叠、展开才出现。故限宽从
/// 「所有正文」收缩到「只有这一段」，两边的道理都保住了。
const EN_DEF_MAX_W: i32 = 720;

/// 查询导航路径：走过的词，按**浏览顺序**排列。
///
/// 与「历史记录」不是一回事，别合并：历史是按时间倒序、去重的**档案**（术语表里它是
/// 「系统被动记录的事实」），回答「我这些天查过什么」；这里是一条**路径**，回答「我
/// 刚才是从哪一步走到这儿的」，语义与浏览器的前进/后退完全一致——同一个词在路径上可以
/// 出现多次，而回退之后再查新词会把前面那段截断。
///
/// 抽成独立类型而非几个散在 `State` 上的字段，是为了能单测：它的三条规则（不重复压、
/// 回退后截断、边界不越界）都是纯逻辑，而 `State` 拖着一个真实的 SQLite 词典，进不了
/// 单元测试。
#[derive(Default)]
struct NavPath {
    path: Vec<String>,
    /// 当前停在第几步。`path` 为空时无意义。
    pos: usize,
}

impl NavPath {
    /// 走到一个新词。
    fn push(&mut self, word: &str) {
        // 已经停在这个词上就不重复压：连点同一行、或回车查一个正看着的词，都不该在
        // 路径上堆出一串一模一样的台阶。
        if self.path.get(self.pos).map(String::as_str) == Some(word) {
            return;
        }
        // 从当前位置截断——浏览器语义：后退几步之后再查新词，原先那段前进路径就作废了。
        if !self.path.is_empty() {
            self.path.truncate(self.pos + 1);
        }
        self.path.push(word.to_string());
        self.pos = self.path.len() - 1;
    }

    /// 沿路径走一步，返回走到的词。走不动时为 `None`。
    fn go(&mut self, forward: bool) -> Option<String> {
        if !self.can_go(forward) {
            return None;
        }
        self.pos = if forward { self.pos + 1 } else { self.pos - 1 };
        self.path.get(self.pos).cloned()
    }

    /// 该方向上还有没有下一步。供按钮决定要不要置灰。
    fn can_go(&self, forward: bool) -> bool {
        if forward {
            self.pos + 1 < self.path.len()
        } else {
            self.pos > 0
        }
    }
}

/// 左栏列表的一行。三个页签共用**一份**数据信号（`State::left_rows`），由
/// `LeftPaneLoader` 按当前页签填充。
///
/// 高亮态（候选的键盘游标、召回行的「你正在看的是这条」）编进**数据**而非在构建期
/// 现读，是因为二者都是构建期求值的视觉：只改信号不重建，高亮不会动。编进数据之后
/// 游标一动就是一次数据变更，列表自然重建——原先专为此设的 `cand_rev` 重建计数
/// 因此可以整个删掉。
#[derive(Clone)]
enum LeftRow {
    /// 一条补全候选：点它即「确定查询词」，触发查询。
    ///
    /// `at_cursor`（回车会选这条）与 `active`（正在看这条）是两件事，见
    /// `CURSOR_SOFT_A`。
    Candidate {
        cand: Candidate,
        at_cursor: bool,
        active: bool,
    },
    /// 一条召回记录（历史或收藏）：点它即查这个词。
    Recall {
        headword: Headword,
        at_cursor: bool,
        active: bool,
    },
}

/// 结果区的一张词头卡片。
///
/// 以**词头**而非词条为单位，因为收藏的单位是词头：一个繁体查询词可能命中简体列
/// 不同的多行（`餘` → `余` / `馀`），此时整页一个星标无法表达用户要收藏哪一个。
/// 而同一词头的多音字（`行` 的 hang2 / xing2）是多条词条、一个词头，只该有一个星标。
/// 常见的单词头情形下，它看起来与设计稿的单星标完全一致。
#[derive(Clone)]
struct Card {
    headword: Headword,
    /// 是否已收藏。取值时刻即卡片构建时刻，由 `revision` 驱动重建保持新鲜。
    fav: bool,
    entries: Vec<Entry>,
    /// 字形。仅当词头**恰好一个字**时才有——「苹果的部首」不是有意义的问题。
    ///
    /// 挂在卡片上而不是挂在词条上：`行` 有三条词条（hang2/heng2/xing2）却只有一副
    /// 字形，放进 [`Entry`] 会存三份、也会画三遍。见 `domain::Glyph` 的文档。
    glyph: Option<Glyph>,
}

/// `LeftPaneLoader` 上一轮见到的各信号版本。
///
/// 拆成具名结构而非一串 `u64` 字段，是因为它有两个取值时刻（进门比对、出门记账），
/// 两处必须取同一组信号——散成五个字段就等着漏掉其中一个。
#[derive(Clone, Copy, PartialEq, Eq)]
struct LeftInputs {
    tab: u64,
    cands: u64,
    cursor: u64,
    rev: u64,
    cards: u64,
}

impl LeftInputs {
    fn of(st: &State) -> Self {
        Self {
            tab: st.left_tab.version(),
            cands: st.candidates.version(),
            cursor: st.cursor.version(),
            rev: st.revision.version(),
            cards: st.cards.version(),
        }
    }
}

/// 监视左栏的全部输入、重算左栏行的响应式控件。
///
/// 与 `Completer` 同构：不绘制任何东西，靠 `on_update` 相位工作，故空闲时不占 CPU。
///
/// 它盯五个信号，因为左栏的一行长什么样确实取决于这五样东西：页签（换数据源）、
/// 候选与键盘游标（候选行及其高亮）、用户数据变更（历史/收藏增删）、当前卡片
/// （召回行的「你正在看的是这条」）。
struct LeftPaneLoader {
    st: Rc<State>,
    last: LeftInputs,
}

impl Widget for LeftPaneLoader {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let now = LeftInputs::of(&self.st);
        if now == self.last {
            return;
        }
        // 收藏状态变了，结果区的星标也得跟着变——卡片上的 `fav` 是快照，不会自己更新。
        //
        // 只刷星标、不重新分组：词条没变，重跑一遍 `group_by_headword` 是白做的。
        //
        // 代价：`revision` 不区分「收藏变了」和「历史变了」，故每次查询（写历史）也会
        // 白刷一遍星标，多花每词头一次 `is_favorite` 查询。没有拆成两个信号，是因为
        // 词头通常只有一两个，拆分买到的性能不抵它带来的「哪个信号该由谁 bump」的负担。
        //
        // **只在 `revision` 变了时刷**，不是每轮都刷。这不是省一次查询那么简单：
        // `refresh_fav_flags` 无条件 `cards.set`，而本控件又盯着 `cards`——每轮都刷
        // 就是每轮都把自己叫醒，进程再也回不到空闲，windui「空闲零 CPU」这条核心
        // 指标当场作废。
        if now.rev != self.last.rev {
            self.st.refresh_fav_flags();
        }
        // 换了页签就把行游标归零。下标指的是「第几行」，而换页签那批行已经换人了，
        // 留着旧下标会让游标停在一个与刚才毫不相干的词上——候选页第 12 条与历史页
        // 第 12 条之间没有任何关系。
        //
        // 在 `reload_left` **之前**归零，否则这一轮铺出来的行仍按旧下标标高亮。
        if now.tab != self.last.tab {
            self.st.cursor.set(0);
        }
        self.st.reload_left();
        // **刷完再记账**，把自己刚造成的那次 `cards` 写入也算进去。若记上面那个 `now`，
        // 下一轮又会看到「cards 变了」，同样是一个永不停歇的循环。
        self.last = LeftInputs::of(&self.st);
    }
}

/// 让左栏列表能拿键盘焦点、并用 ↑↓ 走行的控件。
///
/// ## 为什么需要一个控件
///
/// 列表的行各自是一个 `Clickable`，若靠它们自己拿焦点，Tab 会**逐行**走过去——四十条
/// 候选就是四十下 Tab，而用户要的是「一下进列表，然后上下走」。这正是 WAI-ARIA 的
/// roving tabindex：整块只占一个焦点位，内部移动交给方向键。windui 的 `TabBar` 出于
/// 同样的理由也是整条一个控件。
///
/// 它挂在列表**外层的 col** 上，不是滚动容器本身——`Element::scroll()` 的滚动条拖动
/// 逻辑住在它默认挂的 `ScrollWidget` 里，顶掉它就会重演「滚动条看得见、抓不住」那个
/// 缺陷（见 `left_list`）。
///
/// ## 滚动跟随为什么按行高硬算
///
/// 游标移到视口外时列表要跟着滚。正统做法是拿到那一行的节点、调 `scroll_into_view`，
/// 但行是响应式重建的：`on_update` 派发到本控件时，列表内容的重建（`DynList`，注册在
/// 更内层）还没跑，此刻去树里找「第 i 行」找到的是上一批行。
///
/// 按 `序号 × ROW_H` 直接算就绕开了这个时序——代价是三个页签的行必须同高，而那本来
/// 就是硬约束（见 `left_row`）。
struct ListKeyNav {
    st: Rc<State>,
    /// 上次见到的游标版本。
    last_cursor: u64,
    /// 上次见到的「要焦点」请求版本。见 `State::focus_list`。
    last_focus_req: u64,
}

impl ListKeyNav {
    /// 把游标那一行滚进视口。已经在视口里就不动——否则每按一次键列表都会跳一下。
    fn scroll_into_view(&self, ctx: &mut EventCtx) {
        let i = self.st.cursor.get() as i32;
        let me = ctx.id();
        let tree = ctx.tree_mut();
        // 本控件的唯一子节点就是那个滚动容器（见 `left_list`）。
        let Some(scroll) = tree.get(me).and_then(|n| n.children.first().copied()) else {
            return;
        };
        let Some(n) = tree.get(scroll) else {
            return;
        };
        let (view_h, cur) = (n.bounds.h, n.scroll_y);
        if view_h <= 0 {
            return;
        }
        let (top, bottom) = (i * ROW_H, i * ROW_H + ROW_H);
        let next = if top < cur {
            top
        } else if bottom > cur + view_h {
            bottom - view_h
        } else {
            return;
        };
        tree.set_scroll_y(scroll, next.max(0));
    }
}

impl Widget for ListKeyNav {
    fn focusable(&self) -> bool {
        true
    }

    fn on_update(&mut self, ctx: &mut EventCtx) {
        // 有人点了行 → 替自己把键盘焦点要过来。理由见 `State::focus_list`。
        let f = self.st.focus_list.version();
        if f != self.last_focus_req {
            self.last_focus_req = f;
            ctx.request_focus();
        }
        let v = self.st.cursor.version();
        if v == self.last_cursor {
            return;
        }
        self.last_cursor = v;
        // 跟随**不看焦点在哪**：在查询框里按 ↑↓ 同样要把列表滚到那一行上，否则用户
        // 打着字往下翻，高亮早就跑到视口外面去了。
        self.scroll_into_view(ctx);
    }

    fn on_event(&mut self, _ctx: &mut EventCtx, ev: &Event) -> bool {
        let Event::Key(k) = ev else {
            return false;
        };
        if !k.pressed {
            return false;
        }
        match k.key {
            Key::Down => {
                self.st.move_cursor(true);
                true
            }
            Key::Up => {
                self.st.move_cursor(false);
                true
            }
            // 回车在这里与在查询框里是同一件事：确定要游标那一行，且**记历史**。
            Key::Enter => {
                self.st.submit();
                true
            }
            // Tab / Shift+Tab 一律放过，让焦点继续走。
            _ => false,
        }
    }

    // **不画焦点环**。曾经画过一圈 2px 的强调色边框，撤掉了：那一圈框住的是整个列表
    // 区域，在一块本就被淡底、圆点、分隔线填满的栏里再套一个大框，读起来像是「这块
    // 区域出错了」而不是「焦点在这儿」。
    //
    // 焦点的可见性由**游标行的高亮**承担——它一直在，且按方向键时会动，那比一圈静止
    // 的边框更能说明「键盘现在管着这里」。
}

/// 两栏之间那条可拖动的分隔条。
///
/// ## 拖动中就重排，不是松手才重排
///
/// windui 的宽度是**构建期**定的——没有 `width_signal` 这种东西（对照 `fg_role_signal`
/// 确实有），所以每改一次栏宽就得重建左栏那棵子树。
///
/// 曾经因此走过「拖动中只平移分隔条、松手才落定」的路子，撤掉了：拖分栏是一个靠**眼睛
/// 反馈**收敛的动作，看不到内容跟着变，就只能松手看一眼、不合适再拖一次。当初担心的两
/// 项代价实测都不成立——
///
/// - **滚动位置不会丢**：滚动状态归外层 `Element::scroll()` 自带的 `ScrollWidget`，
///   重建的只是它内部那批行（见 `left_list`）。
/// - **重建量很小**：左栏就一个查询框、一个分段控件和至多 `MAX_CANDIDATES` 行。
///
/// 写库仍留到松手（`PointerKind::Up`）：拖动途中的每个中间值都存一次，等于把用户
/// 拖过的每一帧都往 SQLite 里写一遍，而其中只有最后那个值有意义。
///
/// ## 为什么要指针捕获
///
/// 分隔条只有 6px 宽，鼠标稍快一点就跑到它外面去了。`ctx.capture()` 之后事件不再按
/// 命中派发、而是直送本节点，拖动才不会在半路断掉。`Up` 里**必须**释放，否则整个窗口
/// 的点击都会继续被这条 6px 的线吃掉。
struct PaneSplitter {
    st: Rc<State>,
    /// 拖动中：`(按下时的指针 x, 按下时的左栏宽)`。`None` = 没在拖。
    drag: Option<(i32, i32)>,
    /// 指针是否悬在条上。只影响那条线的粗细与颜色。
    hover: bool,
}

impl PaneSplitter {
    /// 拖到当前指针位置对应的左栏宽度。
    fn target_w(&self, ctx: &mut EventCtx, x: i32) -> Option<i32> {
        let (x0, w0) = self.drag?;
        Some(clamp_left_w(w0 + x - x0, root_width(ctx)))
    }
}

impl Widget for PaneSplitter {
    /// 中间一条竖线；悬停或拖动时加粗并转强调色。
    ///
    /// 自绘而非用 `bg_role` 铺底：这条节点有 6px 宽（命中余量），整条铺色会在两栏之间
    /// 画出一道明显的灰带。只画中间 1px，看起来才是一条分隔线。
    fn paint(
        &self,
        bounds: Rect,
        _content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let active = self.hover || self.drag.is_some();
        let (w, role) = if active {
            (2.0, Role::Accent)
        } else {
            (1.0, Role::Divider)
        };
        // 走 `icon::role_color` 而非写死色值：换肤时这条线必须跟着变，而 `Canvas` 收的
        // 是具体 `Color`，角色只能在这里当场解析一次。
        let paint = Paint::fill(crate::icon::role_color(role));
        // 线画在**左边缘**，不是 6px 的中点。
        //
        // 画在中点时它离左栏的底色边界还有 2.5px，看起来像是「一条没对齐的线浮在两栏
        // 中间」——左栏底色（SurfaceAlt）在 bounds.x 就断了，线却在 bounds.x+2.5。
        // 贴着左边缘画，它才读作「左栏的右边界」。命中余量仍是整条 6px，只是全部落在
        // 线的右侧——那半边是右栏的留白，本来就没有内容，占着不碍事。
        canvas.fill_rect(bounds.x as f32, bounds.y as f32, w, bounds.h as f32, &paint);
    }

    /// 左右调整箭头。
    ///
    /// 这个形状是为本控件在 windui 补的（`CursorShape::SizeWE`）——此前只有
    /// Arrow / Hand / Text，只能退而用手型，而手型说的是「这里能点」，与「这里能左右
    /// 拖」指向两种不同的操作。
    fn cursor(&self) -> CursorShape {
        CursorShape::SizeWE
    }

    fn on_event(&mut self, ctx: &mut EventCtx, ev: &Event) -> bool {
        let Event::Pointer(p) = ev else {
            return false;
        };
        match p.kind {
            // 悬停只改视觉，**不消费**：把 Enter/Leave 吞掉会打断框架自己的 hover 记账。
            PointerKind::Enter => {
                self.hover = true;
                ctx.mark_dirty();
                false
            }
            PointerKind::Leave => {
                self.hover = false;
                ctx.mark_dirty();
                false
            }
            PointerKind::Down => {
                self.drag = Some((p.pos.x, self.st.settings.borrow().left_pane_w));
                ctx.capture();
                ctx.mark_dirty();
                true
            }
            PointerKind::Move => {
                let Some(w) = self.target_w(ctx, p.pos.x) else {
                    return false;
                };
                // 当场改宽度、当场重建：两栏跟着手一起动。**不写库**——写库留到松手。
                self.st.set_left_w(w);
                true
            }
            PointerKind::Up => {
                if self.drag.take().is_none() {
                    return false;
                }
                ctx.release_capture();
                // **不按松手坐标重算宽度**，只存盘。
                //
                // 捕获被系统收走时（Alt+Tab、弹出原生模态、另一个窗口抢走鼠标），
                // windui 会给捕获节点合成一个 `(-1000000, -1000000)` 的 Up 让它收尾。
                // 按那个坐标重算，`clamp_left_w` 会把左栏钳到下限——于是一次被打断的
                // 拖动会把栏宽定死在 200px，还写进设置库跨重启留着。
                //
                // 宽度在拖动途中每次 `Move` 都已经落进设置了，这里要做的本来就只有
                // 存盘那一下。
                self.st.save_left_w();
                true
            }
            _ => false,
        }
    }
}

/// 监视右栏页签与卡片、重算「当前页签下该显示哪些卡片」的响应式控件。
///
/// 为什么要派生一个信号，而不在构建期就地过滤：结果区是 `host_signal(cards, …)`，
/// 逐卡片映射，拿不到「整批筛完之后是不是空的」这个信息——而空态提示恰恰只要它。
///
/// **必须排在 `LeftPaneLoader` 之后**：后者刷星标即写 `cards`，本控件排在前面，
/// 那次写入要等下一帧才被看见，星标慢一帧。
struct CardFilter {
    st: Rc<State>,
    last_tab: u64,
    last_cards: u64,
}

impl Widget for CardFilter {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let (tab, cards) = (self.st.dict_tab.version(), self.st.cards.version());
        if tab == self.last_tab && cards == self.last_cards {
            return;
        }
        self.last_tab = tab;
        self.last_cards = cards;
        self.st.refilter_cards();
    }
}

/// 监视「给词典改名」输入框、落库的响应式控件。与 [`NoteSaver`] 同构，理由相同：
/// windui 的 `text_input` 没有提交/失焦回调，只能盯变化。
///
/// **不重建设置页**：重建会把正在输入的这个框连同它的焦点一起换掉，用户打第二个字
/// 时就发现光标没了。改名影响的是页签，而页签在另一棵子树上（`tabs_rev`）。
struct DictNameSaver {
    st: Rc<State>,
    key: String,
    text: Signal<String>,
    last_version: u64,
}

impl Widget for DictNameSaver {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.text.version();
        if v == self.last_version {
            return;
        }
        self.last_version = v;
        self.st.set_dict_name(&self.key, &self.text.get());
    }
}

/// 监视备注输入、落库的响应式控件。
///
/// 与 `Completer` 同构，靠 `on_update` 相位工作。之所以需要它，是因为 windui 的
/// `text_input` 没有提交/失焦回调——没有「什么时候算改完了」这个信号，只能盯变化。
///
/// **逐次变更即写库**：本地 SQLite 的一次小写入是微秒级，而「攒着等提交」在没有提交
/// 事件的前提下必然要引入定时器，那会破坏 windui「空闲零 CPU」这条指标。代价是用户
/// 每敲一个字就写一次库，这是清楚的取舍，不是疏忽。
struct NoteSaver {
    st: Rc<State>,
    headword: Headword,
    text: Signal<String>,
    last_version: u64,
}

impl Widget for NoteSaver {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.text.version();
        if v == self.last_version {
            return;
        }
        self.last_version = v;
        self.st.set_note(&self.headword, self.text.get());
    }
}

/// 监视设置开关、把变更落到实处的响应式控件。
///
/// **为什么不用 `Element::switch(..).on_toggle(..)`**：windui 的 `Switch` 没有实现
/// `Widget::take_click`，而该 trait 方法的默认实现是空的——挂上去的回调被静默吞掉，
/// 开关照样滑动、动画照样正常，但什么都不会发生。这个坑已上报框架。
///
/// 改为盯信号版本：无论用户怎么让开关翻转，这里都收得到。落实失败时**把开关拨回
/// 原位**——开关显示「开」而注册表没写，比报个错更误导人。
struct SettingToggle {
    st: Rc<State>,
    on: Signal<bool>,
    last_version: u64,
    /// 落实这次变更，返回是否成功。
    apply: fn(&State, bool) -> bool,
}

impl Widget for SettingToggle {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.on.version();
        if v == self.last_version {
            return;
        }
        self.last_version = v;
        let want = self.on.get();
        if !(self.apply)(&self.st, want) {
            self.on.set(!want);
            // 回拨自己也会推高版本，须同步记下，否则下一帧会把回拨当成新的用户操作，
            // 来回翻转停不下来。
            self.last_version = self.on.version();
        }
    }
}

/// 监视热键编辑、即时改绑的响应式控件。
///
/// **为什么不做「按下新组合键」式的捕获**：windui 的 `KeyEvent` 只带 `ctrl` 与
/// `shift`，没有 `alt`——而本项目的默认热键正是 Ctrl+Alt+D，捕获式界面根本认不出它。
/// 勾选修饰键 + 填字母这条路虽朴素，却能如实表达全部可用组合，且不必处理「按下
/// Escape 算取消还是算热键」这类捕获特有的歧义。
struct HotkeyEditor {
    st: Rc<State>,
    ctrl: Signal<bool>,
    alt: Signal<bool>,
    shift: Signal<bool>,
    /// 主键在 `HotkeyKey::all()` 里的下标。
    key: Signal<usize>,
    last: (u64, u64, u64, u64),
}

impl Widget for HotkeyEditor {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let now = (
            self.ctrl.version(),
            self.alt.version(),
            self.shift.version(),
            self.key.version(),
        );
        if now == self.last {
            return;
        }
        self.last = now;
        // 主键从固定表里按下标取，不再解析用户手打的字符串：下拉框只列得出合法项，
        // 「填了个看不懂的键」这条错误路径整个消失了。
        let all = crate::settings::HotkeyKey::all();
        let Some(&key) = all.get(self.key.get()) else {
            return;
        };
        self.st.set_hotkey(crate::settings::HotkeySpec {
            ctrl: self.ctrl.get(),
            alt: self.alt.get(),
            shift: self.shift.get(),
            key,
        });
    }
}

/// 把设置里的回执转成 Toast。
///
/// 零尺寸、挂在**顶层**（`body`）而不是设置页里：设置页每次 `bump_settings` 整体重建，
/// 挂在里头的话，每一条「改完就重建」的回执（换目录、刷新、拨开关）都会连同监听器一起
/// 被换掉——新监听器带着新的 `last`，那条刚写下的消息于是永远发不出去。
///
/// 盯**信号版本**而非内容：连着两次同样的回执（例如连点两下刷新）内容一模一样，比内容
/// 就只弹一条，而用户点了两下，看不到第二条反馈会以为按钮坏了。
struct ToastSink {
    note: Signal<String>,
    tone: Signal<Role>,
    last: u64,
}

impl Widget for ToastSink {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        let now = self.note.version();
        if now == self.last {
            return;
        }
        self.last = now;
        let text = self.note.get();
        // 空串是 `note_clear` 用来收起消息的，不是一条消息。
        if text.is_empty() {
            return;
        }
        match self.tone.get() {
            Role::Danger => ctx.toast_err(text),
            _ => ctx.toast_ok(text),
        }
    }
}

/// 让绑在这个信号上的 `host_signal` 整体重建一次。
///
/// windui 的 `host_signal` 收 `Signal<Vec<T>>`、对**每个元素**调一次构建回调，故拿一个
/// **单元素** Vec 装计数即可表达「整体重建一次」。设置页与候选区共用这个手法：两处都有
/// 构建期求值的东西（选中环、词库路径 / 候选高亮），信号一改就得整块重来。
fn bump(rev: Signal<Vec<u64>>) {
    let n = rev.get().first().copied().unwrap_or(0);
    rev.set(vec![n.wrapping_add(1)]);
}

/// 写一条消息条：语气与文本**同写**。
///
/// 语气就是文字角色本身（`Role::Success` / `Role::Danger`），不再另立一个 `Tone` 枚举
/// 转译——windui 的 `Role` 已经有这两个语义色，中间加一层只是把同一件事说两遍。
///
/// 抽成自由函数是为了能测：`State` 持有 `ThemeHandle` 与 `HotkeyHandle`，此前两者的
/// 构造面是私有的、下游造不出 `State`。上游已补 `detached` 构造口，这层可以按需收回
/// 方法里；暂留是因为 `State` 还持有 `OfflineDictionary`，那要一份真词库文件。
///
/// 先写语气再写文本不是随意的：文本是可见性的开关（空串 = 收起），反过来写会有一瞬
/// 消息条已按新文本显示、语气还停在上一条上。同一次事件内两次写入之间不会插进绘制，
/// 故今天看不出差别——但这个顺序不需要依赖那个前提。
fn write_note(text: Signal<String>, tone: Signal<Role>, role: Role, s: impl Into<String>) {
    tone.set(role);
    text.set(s.into());
}

/// 界面状态。
struct State {
    dict: Rc<OfflineDictionary>,
    /// 自带词典：用户放进来的 MDX，按设置里的顺序依次问过去。
    ///
    /// 只装**打开成功**的那些。设置页列的是 `Settings::user_dicts` 里的路径，两者
    /// 对不上的即为「当前打不开」——文件被移走、被换成不支持的格式，都归这一类。
    /// 让打不开的词典留在设置里而不是被悄悄剔除，用户才有机会看见并处理它。
    ///
    /// `RefCell` 是为了设置页增删词典：那不需要重建界面树，只需下一次查询问到新的那本。
    user_dicts: RefCell<Vec<UserDictionary>>,
    /// 用户数据（收藏与历史）。不可用时保留**原因**，由 `unavailable_bar` 展示。
    user: UserDataState,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
    /// **左栏的行游标**（下标），三个页签共用。
    ///
    /// 它此前只服务候选页。扩到三段是键盘导航的前提：焦点落在列表上按 ↑↓ 时，用户
    /// 不关心自己停在哪个页签——「上一条 / 下一条」对候选、历史、收藏是同一个动作。
    ///
    /// 切页签时归零（见 `LeftPaneLoader`）：下标指的是「第几行」，而换了页签那批行
    /// 已经换人了，留着旧下标会让游标停在一个与上次毫不相干的词上。
    cursor: Signal<usize>,
    /// 一次查询分组出来的**全部**卡片，未经右栏页签筛选。
    ///
    /// 它是查询结果的**真相**，但不是结果区读的那个信号——结果区读 `visible_cards`。
    /// 两者的关系是单向派生（`refilter_cards`），故不存在「两份数据要同时更新」这种
    /// 迟早会漏掉一处的要求。
    cards: Signal<Vec<Card>>,
    /// 经右栏页签筛选后**当前该显示**的卡片。结果区唯一的数据源，由 `CardFilter` 派生。
    visible_cards: Signal<Vec<Card>>,
    /// 结果区的提示文案（未收录、请输入等）。
    hint: Signal<String>,
    /// 右栏页签筛完之后的空态文案。空串 = 无需提示，见 `refilter_cards`。
    filter_note: Signal<String>,
    /// 右栏当前页签：全部 / 英汉 / 汉英。
    /// 托盘图标的运行期句柄。改热键之后要把提示改掉——那句话里写着热键。
    tray: windui::platform::TrayHandle,
    dict_tab: Signal<usize>,
    /// 页签集合。随装卸词典与改名而变，随查询**不变**（变的只是各自的可用态）。
    tabs: RefCell<Vec<TabSpec>>,
    /// 页签集合变了，标签条整体重建一次。
    tabs_rev: Signal<Vec<u64>>,
    /// 左栏当前页签：候选 / 历史 / 收藏。
    left_tab: Signal<usize>,
    /// 左栏当前列出的行。三个页签共用，由 `LeftPaneLoader` 按页签填充。
    left_rows: Signal<Vec<LeftRow>>,
    /// 左栏列表为空时的文案。空串 = 列表非空，无需提示。
    left_note: Signal<String>,
    /// 查询导航路径。见 [`NavPath`]。
    nav: RefCell<NavPath>,
    /// 导航按钮的重建计数：两枚按钮的可用与否是构建期算的，走一步就得重建。
    nav_rev: Signal<Vec<u64>>,
    /// 「把键盘焦点交给左栏列表」的请求计数。
    ///
    /// 用计数器而非布尔：`Signal::set` 无条件递增版本，连点两次同一行也各是一次请求，
    /// 而布尔翻不动就丢了第二次。
    ///
    /// 之所以要绕这一手：`EventCtx::request_focus` 只能把焦点给**自己**，而点击是行
    /// 自己消费的（行是 `Clickable`），它没法替父容器要焦点。让行 bump 一个信号、
    /// 由 `ListKeyNav` 在 `on_update` 里替自己要，是唯一不改上游的路径。
    focus_list: Signal<Vec<u64>>,
    /// 用户数据的变更计数。收藏增删、历史写入后自增，驱动左栏与卡片重取。
    ///
    /// 用计数而非直接刷新，是因为写入点（`record_all`、`toggle_favorite`）与读取点
    /// （侧栏、卡片）互不相识——让它们共享一个「有东西变了」的信号，比让写入方
    /// 逐一通知读取方更不容易漏。
    revision: Signal<u64>,
    /// 需要**当场**告知用户的消息（目前只有收藏写入失败）。空串 = 无。
    notice: Signal<String>,
    /// 可折叠区（当前只有「英英释义」）的展开态。
    expanded: ExpandedStates,
    /// 主题句柄：换肤即 `set` 一份新 `Theme`，下一帧全树跟随。
    ///
    /// 界面里**没有一处写死颜色**，全部走 `Role` / `RoleAlpha`，故换肤不需要重建
    /// 元素树——这正是 ADR-0012 当初判断做不到的那件事，其症结其实不在框架，而在
    /// 我们把本可用角色表达的颜色写成了具体色值。
    theme: ThemeHandle,
    /// 热键句柄：改键即 `HotkeyHandle::set`，下一次消息循环生效。
    hotkey: HotkeyHandle,
    /// 内容区当前页：词典 / 设置。
    page: Signal<usize>,
    /// 左栏重建计数。栏宽是构建期读的（windui 没有 `width_signal`），改了必须重建，
    /// 见 `PaneSplitter`。
    pane_rev: Signal<Vec<u64>>,
    /// 当前设置。界面上的各个控件绑到它的分量信号，改动经 `save_settings` 落库。
    settings: RefCell<Settings>,
    /// 设置页的即时反馈（保存失败、需重启生效等）。空串 = 无。
    settings_note: Signal<String>,
    /// 上一条 `settings_note` 的文字角色。初值取 `Danger` 是保守选择：新写入若漏了
    /// 语气，一条被染红的成功回执比一条被染绿的失败提示要好收场。
    settings_note_tone: Signal<Role>,
    /// 「清空历史」是否已进入确认态。
    ///
    /// 用两步确认而非弹模态框：清空不可撤销，但为它拉起一个系统模态框在常驻小工具上
    /// 过重；而「点一次变成『确认清空』，再点才真清」既拦得住误触，又不打断心流。
    confirm_clear: Signal<bool>,
    /// 换肤重建计数。图标的颜色是构建期解析的，换肤后必须重建整树才会跟上，见 `build`。
    skin_rev: Signal<Vec<u64>>,
    /// 系统此刻是否偏好暗色。
    ///
    /// 缓存而非每次现查：`SkinMode::System` 下每一次解析配色都要用到它，而查它要读
    /// 注册表。真正让它必须是**可变**的，是运行期跟随——用户在系统设置里切了暗色，
    /// windui 会回调 `on_system_theme_changed`（见 `build` 的返回值），那时把这一位
    /// 更新掉，配色就跟着翻过去了。
    ///
    /// 用 `Cell` 而非信号：它不该自己驱动重建。翻转它之后要做的事与用户手动换风格
    /// 完全一样（`apply_skin`），走同一条路才不会两边漂移。
    system_dark: std::cell::Cell<bool>,
    /// 设置页的重建计数。
    ///
    /// 设置页上有若干**构建时求值**的显示——皮肤卡片的选中环、词库路径文字。它们
    /// 不是控件自带的状态，改了设置若不重建就会停在旧值上（选中环留在原来那张卡片，
    /// 是最显眼的一处）。故设置一变就整页重建，而不是逐处想办法局部刷新：设置页的
    /// 重建成本可以忽略，而「有的刷了有的没刷」是很难查的那种不一致。
    settings_rev: Signal<Vec<u64>>,
}

/// 展开态的键：词头 + 该词头下的词条序号（多音字一个词头、多条词条，各自开合）。
///
/// 抽成函数是因为它有**两个**调用方——预建（`State::rebuild_cards`）与构建期取用
/// （`card_view`），两处拼不出同一个键，预建就等于没做，而症状是下面那个 panic。
fn expand_key(hw: &Headword, i: usize) -> String {
    format!("{hw}#{i}")
}

/// 可折叠区的展开态集合，按键长期持有。
///
/// **必须活在元素树之外**。结果区的卡片会因收藏状态变化而整棵重建
/// （`refresh_fav_flags` 写 `cards` → `host_signal` 全量重建），若像从前那样在
/// `entry_view` 里就地 `signal(false)`，每次重建都新造一个信号、展开态归零——
/// 用户看到的是「展开英英释义后点一下收藏，它自己收起来了」。
///
/// 这类 bug 换个控件是治不好的：无论用 `Element::collapsible` 还是富文本的折叠区，
/// 只要展开态跟着元素树生灭，重建就会抹掉它。状态得比树活得久。
///
/// ## 信号本身也必须在重建之外创建（血的教训）
///
/// 「活在树之外」还有一半：光把**句柄**存在树外不够，`signal()` 那一下**在哪儿调**
/// 同样决定生死。windui 的 `host_signal` 用一个 `SignalScope` 圈住 `build_fn`，重建时
/// 先整批回收上一轮在其中创建的信号（`ui/mod.rs::host_signal`）——那是对的，否则每重建
/// 一次就永久漏一代。但它意味着：**在构建期调 `signal()` 拿到的句柄，只活到下一次
/// 重建**。
///
/// 从前 `get` 是「没有就地建一个并存进表里」，而它的调用点在 `card_view`，也就是
/// 重建作用域**内**。于是表里存的是一个到期就作废的句柄，下一次重建取回它、
/// `Element::collapsible` 一读就 panic：「signal 句柄已失效：槽位已被回收」。而 panic
/// 落在 Win32 窗口过程里跨 C ABI 不能展开，运行时直接 abort——进程凭空消失，
/// 事件查看器只留一条 `0xc0000409`。
///
/// 实机复现：查一个**带英英释义**的词（输入 `misc` → 候选 → 回车）。`rebuild_cards`
/// 写一次 `cards` 触发重建 #1，随后 `SideLoader` 的 `refresh_fav_flags` 又写一次，
/// 重建 #2 取回失效句柄。没有英英释义的词根本不读那个信号（`entry_view` 里 `collapsible`
/// 才读），所以不崩——这就是它看起来偶发、追了两天没头绪的原因。
///
/// 故信号一律由 [`prepare`](Self::prepare) 在**事件回调里**建好，`get` 只查表。
///
/// 单独成一个类型而非直接摊在 `State` 里，是为了能脱开词库单独验证——它的正确性
/// 全在「同一个键必须拿到同一个、且不会失效的信号」这一条上，而那与词典数据无关。
#[derive(Default)]
struct ExpandedStates(RefCell<HashMap<String, Signal<bool>>>);

impl ExpandedStates {
    /// 为这批键备好信号。**只能在元素树重建之外调用**（事件回调里），理由见类型头注释。
    ///
    /// 已有的键不动：`or_insert_with` 而非 `insert`——重查同一个词不该把用户手动展开的
    /// 那条又合上。这也正是 `default_open` 只对**尚未出现过**的键生效的地方：用户折叠过
    /// 的，不该因为改了设置又被展开。
    fn prepare(&self, keys: impl IntoIterator<Item = String>, default_open: bool) {
        let mut map = self.0.borrow_mut();
        for k in keys {
            map.entry(k).or_insert_with(|| signal(default_open));
        }
    }

    /// 取某个键的展开态。**同一键永远返回同一个信号**。
    ///
    /// 未命中时就地新建，但**不入表**：这条是退路，走到这里说明 `prepare` 漏了一个键。
    /// 新建的句柄同样活不过下一次重建，可它没进表、不会被第二次取用，故只会让这一条
    /// 折叠区的展开态归零，不会 panic。dev 构建下当场断言，免得这条退路把漏建悄悄
    /// 变成「偶尔自己收起来」那种最难查的毛病。
    fn get(&self, key: &str, default_open: bool) -> Signal<bool> {
        if let Some(s) = self.0.borrow().get(key) {
            return *s;
        }
        debug_assert!(
            false,
            "展开态 `{key}` 未预建：`rebuild_cards` 与 `card_view` 的键对不上了"
        );
        signal(default_open)
    }
}

impl State {
    /// 点左栏的一行：把游标挪到它身上、把键盘焦点交给列表，然后查询。
    ///
    /// **交焦点**是为了鼠标与键盘能接上：用户点了一行之后，多半接着想用 ↑↓ 继续看
    /// 相邻的词——若焦点还留在查询框（或哪儿都不在），方向键就落不到列表上。
    ///
    /// **挪游标**同样不是装饰，理由见 [`pick_candidate`](Self::pick_candidate)；
    /// 按词头回查下标的取舍也一并写在那里。
    fn focus_row(&self, word: &str) {
        bump(self.focus_list);
        if let Some(i) = self
            .left_rows
            .get()
            .iter()
            .position(|r| self.row_matches(r, word))
        {
            self.cursor.set(i);
        }
        self.lookup(word);
    }

    /// 某一行是否就是这个词头。
    fn row_matches(&self, r: &LeftRow, word: &str) -> bool {
        match r {
            LeftRow::Candidate { cand, .. } => cand.headword.as_str() == word,
            LeftRow::Recall { headword, .. } => headword.as_str() == word,
        }
    }

    /// 点一条候选：把键盘游标挪到它身上，然后查询。
    ///
    /// 挪游标不是可有可无的装饰。游标回答「回车会选中哪一条」，而用户刚用鼠标选定了
    /// 一条——此刻若游标还停在原处，按回车会跳去查另一个词，这是实打实的错误动作。
    /// 顺带也让两档高亮在鼠标路径下重合，见 `CURSOR_SOFT_A`。
    ///
    /// 按词头回查下标而不是把下标编进行数据：候选最多 `MAX_CANDIDATES` 条，一次线性
    /// 查找的代价可以忽略，而多一个字段就多一处要与列表顺序保持同步的东西。
    fn pick_candidate(&self, word: &str) {
        // 点候选同样把焦点交给列表，理由见 `focus_row`。
        bump(self.focus_list);
        if let Some(i) = self
            .candidates
            .get()
            .iter()
            .position(|c| c.headword.as_str() == word)
        {
            self.cursor.set(i);
        }
        self.lookup(word);
    }

    /// 选中一个词：**只查询**，不碰查询框，也不动左栏。
    ///
    /// 左栏三个页签的行、以及回车（`submit`）都走这里。
    ///
    /// **不写查询框**是这个方法最要紧的一条，它换来的是「点一条看一条」：候选原样留在
    /// 左边，可以接着点下一条来回比对，而不是每点一次列表就换一批内容。查一个词并不
    /// 意味着用户想改自己刚打的那串字——恰恰相反，他多半正想拿这串字继续挑。
    ///
    /// 这同时消掉了一整套机制。此前 `select` 会 `query.set(word)`，而 `query.set`
    /// **无条件** bump 版本（windui `signal.rs:163`，没有任何值比较），于是 `Completer`
    /// 醒来拿新词重算补全，候选被换成「以这个词为前缀的一串别的词」。为压住它，代码里
    /// 曾有一个 `picked: RefCell<Option<String>>` 标记外加一段自校验注释——那整套东西
    /// 的存在理由只是「select 会改查询框」。不改了，它们就一起没了。
    ///
    /// 想主动把词填进查询框仍有明确的入口：Tab（`accept_completion`）。那是 shell 的
    /// 补全语义，用户按下它就是在说「把这个词接着编辑」。
    fn select(&self, word: &str) {
        self.lookup(word);
    }

    /// 回车：确定查询词。
    ///
    /// 有候选就取游标那条，没有就直接查输入框里的字。两者都是术语表说的「查询词确定
    /// 下来」——它明确列了「回车、选中候选、切换查询源」三种，回车在列。
    ///
    /// 空输入不查：`lookup` 对空串会走 `Query::new` 的 None 分支，把结果区清成
    /// 「输入一个词开始查询」，等于用户按一下回车就把正在读的词条弄没了。
    fn submit(&self) {
        // 游标那一行优先——三个页签一视同仁：候选页回车查那条候选，历史页回车查那条
        // 历史。此处**记历史**（走 `select` → `lookup`），与 ↑↓ 的 `preview` 分开：
        // 回车是用户表达「就是它」的那一下。
        if let Some(w) = self.row_at_cursor() {
            self.select(&w);
            return;
        }
        let text = self.query.get();
        if !text.trim().is_empty() {
            self.lookup(&text);
        }
    }

    /// 移动左栏行游标并**当场查出那一行**。`down` 为真下移，否则上移。
    ///
    /// 移动即查，是「右栏跟着上下键实时变」这件事的全部实现——但它走 `preview`，
    /// 不写历史，理由见那个方法。
    ///
    /// **不环绕**。在顶上按 ↑ 直接跳到末条是种惊吓——尤其它同时意味着「再按一下就
    /// 选中最后那个词」。候选放宽到 40 条、列表滚得动之后这条更成立了：环绕会把视口
    /// 从头甩到尾，而用户按 ↑ 的意图从来不是「去最后一条」。
    fn move_cursor(&self, down: bool) {
        // 按**左栏当前列出的行**算边界，不再只看候选：历史页与收藏页同样要能用 ↑↓ 走。
        let n = self.left_rows.get().len();
        if n == 0 {
            return;
        }
        let i = self.cursor.get();
        let next = if down {
            (i + 1).min(n - 1)
        } else {
            i.saturating_sub(1)
        };
        if next == i {
            return;
        }
        // 只改信号、不手动触发重建：高亮编在 `LeftRow` 的数据里（见该类型的注释），
        // 而 `LeftPaneLoader` 盯着 `cursor` 的版本，重建自然跟上。
        self.cursor.set(next);
        if let Some(w) = self.row_word(next) {
            self.preview(&w);
        }
    }

    /// 第 `i` 行对应的词头。越界或列表为空时为 `None`。
    fn row_word(&self, i: usize) -> Option<String> {
        self.left_rows.get().get(i).map(|r| match r {
            LeftRow::Candidate { cand, .. } => cand.headword.as_str().to_string(),
            LeftRow::Recall { headword, .. } => headword.as_str().to_string(),
        })
    }

    /// 游标当前指向的那一行的词头。
    fn row_at_cursor(&self) -> Option<String> {
        self.row_word(self.cursor.get())
    }

    /// 是否该由 → 接受补全。
    ///
    /// 只在**当前输入是游标那条候选的严格前缀**时才算数，且只在候选页。这是个近似——
    /// 正确的判据是「光标在行尾」（fish / zsh 的 autosuggestion 就是这么判的），而
    /// windui 的 `on_nav_key` 只给按键、给不到光标位置。
    ///
    /// 这个近似能**自我恢复**，所以可以接受：补全一次之后输入就等于候选、不再是严格
    /// 前缀，→ 随即放行、正常移动光标。真正会误伤的只有「输入恰是某候选的前缀、且用户
    /// 正想把光标往回移」这一种，而那时按一下 ← 就回来了。
    ///
    /// **前缀比对不分大小写**：补全查询走的是 `stardict_3` 索引上的 `COLLATE NOCASE`
    /// （见 `store::ecdict::complete`），输入 `Ap` 完全可能拿到候选 `apple`。按字节
    /// 严格比的话这里返回 false，→ 就成了「有时补全、有时移光标」——同一个键在看起来
    /// 一样的情形下行为不同，比它干脆不工作更难用。
    fn should_accept_completion(&self) -> bool {
        if self.left_tab.get() != LEFT_CANDIDATES {
            return false;
        }
        let Some(w) = self.row_at_cursor() else {
            return false;
        };
        let q = self.query.get();
        // 只降 ASCII 大小写：候选与查询词的大小写差异只可能来自英文（中文没有大小写，
        // `to_lowercase` 却要为它走一遍 Unicode 全表映射并分配）。
        !q.is_empty()
            && w.len() > q.len()
            && w.as_bytes()[..q.len()].eq_ignore_ascii_case(q.as_bytes())
    }

    /// →：把游标那条候选填进查询框，**不查询**。
    ///
    /// 这是 shell 的补全语义——补全词，回车才执行。对词典而言尤其顺：把词补全整了再
    /// 接着改（`make` → `maker`），比先查一次再回来改省一步。
    ///
    /// 此前绑在 Tab 上。让出 Tab 是为了把它还给**焦点导航**——从查询框跳到左栏列表
    /// 需要一个键，而 Tab 正是所有 Windows 程序里的那个键；再占着它，用户就没有任何
    /// 办法用键盘走出输入框。→ 是补全的常见替代键位（fish / zsh 同款）。
    fn accept_completion(&self) {
        let Some(w) = self.row_at_cursor() else {
            return;
        };
        // 不走 `select`：那会连查询一起做掉。这里只改字，补全照常跟上——补完的词往往
        // 还要再接着打（`make` 之后接 `r`）。
        self.query.set(w);
    }

    /// 清空查询框，回到开屏状态。
    ///
    /// 常驻词典最高频的一个动作是「唤起 → 查另一个词」，而热键唤起时上次的词还在框里
    /// （窗口只是被隐藏，状态原样保留）。在没有「唤起即聚焦并全选」之前
    /// （见 `docs/upstream-keyboard-path.md`），用户得先把光标挪进去再全选删除——
    /// 一个按钮省掉的正是这一串。
    ///
    /// 连结果一起清而不只清输入框：留着上一个词的词条、顶上却是个空框，这个组合不表达
    /// 任何状态。`candidates` 不必手动清——`complete("")` 走 `Query::new` 的 None 分支
    /// 返回空表（`source/offline.rs:83`），`Completer` 醒来自会清空。
    fn clear_query(&self) {
        self.query.set(String::new());
        self.rebuild_cards(&[]);
        self.hint.set("输入一个词开始查询".into());
        self.notice.set(String::new());
    }

    /// 执行查询并记入历史。用户**确定**要这个词时走这条（回车、点一行）。
    fn lookup(&self, word: &str) {
        self.lookup_inner(word, true);
    }

    /// 执行查询但**不记历史**。键盘 ↑↓ 扫过一行时走这条。
    ///
    /// 不记历史是承重的，不是优化：按住 ↓ 划过二十条候选会写下二十条历史记录，把
    /// 「最近查过什么」冲成一串用户根本没看的词——而历史一旦被冲掉就找不回来了。
    ///
    /// 术语表把历史定义为「系统被动记录的**事实**」，那个事实是「用户查过这个词」。
    /// 用方向键扫过去不构成这个事实，停下来读才算——所以记历史的时机是回车与点击，
    /// 不是游标移动。
    fn preview(&self, word: &str) {
        self.lookup_inner(word, false);
    }

    /// 查询的实现。`record` 决定是否记入历史，两个入口的差别只有这一处。
    ///
    /// **查询的四条路径都必须经由这里**——见 `rebuild_cards` 的说明。
    fn lookup_inner(&self, word: &str, record: bool) {
        // 上一次操作的消息到此为止：新一次查询开始，那条红字讲的已是别的词。
        self.notice.set(String::new());
        let Some(q) = Query::new(word) else {
            self.rebuild_cards(&[]);
            self.hint.set("输入一个词开始查询".into());
            return;
        };
        // 随程序分发的词典与用户自带的词典**都问一遍**，结果并排呈现。
        //
        // 这不违反 ADR-0002 的「绝不自动兜底」：那条挡的是**跨类**降级（词典未命中就
        // 静默发网络请求给译源），因为二者可信度与隐私代价都不同。这里全是词典，
        // 没有落差需要用户知情，而每条自带词典的词条都带着出处（`UserEntry::source`）。
        let mut entries = Vec::new();
        let mut via_base_form = false;
        let mut err = None;
        match self.dict.lookup(&q) {
            Err(e) => err = Some(e),
            Ok(Lookup::NotFound) => {}
            Ok(Lookup::Found {
                entries: found,
                via_base_form: v,
            }) => {
                entries = found;
                via_base_form = v;
            }
        }
        // 顺序即优先级：随程序分发的词典在前。它是我们挑过的，自带词典的来源与质量
        // 我们一无所知，把后者顶到前面等于替用户做了一个我们没有依据做的判断。
        let builtin_len = entries.len();

        let mut broken: Vec<String> = Vec::new();
        for d in self.user_dicts.borrow().iter() {
            match d.lookup(&q) {
                Ok(Lookup::Found { entries: found, .. }) => entries.extend(found),
                Ok(Lookup::NotFound) => {}
                // 一本词典读坏了不该让整次查询失败：其余词典的结果照样有用。
                // 但也不能咽下去——用户得知道自己加的那本正在失灵。
                Err(_) => broken.push(d.name().to_string()),
            }
        }
        if !broken.is_empty() {
            self.note_err(format!("自带词典读取失败：{}", broken.join("、")));
        }

        if entries.is_empty() {
            self.rebuild_cards(&[]);
            self.hint.set(match err {
                Some(e) => format!("词库读取失败：{e}"),
                // 提示切换到译源，但**绝不自动发起**——见 ADR-0002。
                None => format!("离线词典未收录「{}」", q.text()),
            });
            return;
        }

        self.hint.set(if via_base_form {
            // 用户查的是变化形态，显示的是原形词条——不提示会让人困惑。
            format!("显示的是「{}」的原形词条", q.text())
        } else {
            String::new()
        });
        if record {
            // 只记随程序分发的词典给出的词头；它一无所获时才退而记自带词典的。
            //
            // 不合起来记：MDX 的词头按归一化后的形式重名（查 `fullhouse` 会同时命中
            // `Full-House` 与 `fullhouse`，见 ADR-0015），全记下来会让一次查询在历史里
            // 留下两三行只差标点的记录。历史是给人翻的。
            let source = if builtin_len > 0 {
                &entries[..builtin_len]
            } else {
                &entries[..]
            };
            self.record_all(&headwords_to_record(source));
            // 只有「确定要这个词」的查询才进导航路径，与记历史同一时机。
            // ↑↓ 扫过去的那些不进——否则按住方向键划过二十条，后退键就得
            // 按二十下才退得回来，而用户心里那一步只有一步。
            self.push_nav(word);
        }
        self.rebuild_cards(&entries);
    }

    /// 按词头把词条分组成卡片，并取各词头当下的收藏状态。
    ///
    /// **查询的四条路径都必须经由这里**，一无所获与失败传空表——否则「未收录」的提示
    /// 下面会挂着上一个词的卡片，且不会自愈：未命中不写历史、不 `bump()`，
    /// `SideLoader` 版本没变直接早退，没人来收拾。这条路径并不罕见：收藏与历史独立于
    /// 词库存活（ADR-0011），换一版词库后点侧栏里旧词库写下的词头，走的正是未命中分支。
    ///
    /// `cards` 是结果区**唯一**的数据源。这里曾另有一个 `entries: Signal<Vec<Entry>>`
    /// 与之并存，两者必须同时更新——而「必须同时」这种要求迟早会被漏掉一处（当初就
    /// 漏在那三条失败路径上）。既然卡片里本就装着全部词条，那个信号纯属冗余，删掉它
    /// 之后两份数据失同步这件事就无从发生了。
    fn rebuild_cards(&self, entries: &[Entry]) {
        let cards: Vec<Card> = group_by_headword(entries)
            .into_iter()
            .map(|(hw, entries)| Card {
                fav: self.is_favorite(&hw),
                glyph: self.glyph_of(&hw),
                headword: hw,
                entries,
            })
            .collect();
        // 展开态的信号必须在这里建，**不能等到 `card_view` 里**——那是重建作用域内，
        // 建出来的句柄下次重建就作废，取回来一读整个进程 abort。原委见 `ExpandedStates`。
        //
        // 位置在 `cards.set` 之前：这一行才是重建的触发点，先备好再放它走。
        let default_open = self.settings.borrow().expand_en;
        self.expanded.prepare(
            cards
                .iter()
                .flat_map(|c| (0..c.entries.len()).map(|i| expand_key(&c.headword, i))),
            default_open,
        );
        self.cards.set(cards);
    }

    /// 只刷新已有卡片的收藏状态，不动分组与词条。
    fn refresh_fav_flags(&self) {
        let cards = self
            .cards
            .get()
            .into_iter()
            .map(|c| Card {
                fav: self.is_favorite(&c.headword),
                ..c
            })
            .collect();
        self.cards.set(cards);
    }

    /// 词头的字形。词头不是**单个字**时没有字形，不是「查不到」。
    ///
    /// 判据写成「恰好一个 char」而非 `len() == 1`：后者是字节数，任何一个汉字都过不了。
    fn glyph_of(&self, hw: &Headword) -> Option<Glyph> {
        let mut it = hw.as_str().chars();
        match (it.next(), it.next()) {
            (Some(ch), None) => self.dict.glyph(ch),
            _ => None,
        }
    }

    /// 词头是否已收藏。读不到时按「未收藏」呈现——星标必须画成某个样子，而空心星
    /// 至少不会让用户以为收藏已经存在。真正的失败告知发生在写入路径。
    fn is_favorite(&self, hw: &Headword) -> bool {
        let UserDataState::Ready(u) = &self.user else {
            return false;
        };
        u.is_favorite(hw).unwrap_or(false)
    }

    /// 切换收藏。
    ///
    /// **失败必须当场告知**，与 `record_all` 的静默策略正好相反——这不是不一致，而是
    /// 术语表那条分界的直接后果：历史是系统被动记录的**事实**，用户没要求它发生；
    /// 收藏是用户主动表达的**意图**，点了就是要求它发生。让一次失败的收藏看起来成功了，
    /// 用户会在需要时发现词不见了，而那时已无从追溯。
    fn toggle_favorite(&self, hw: &Headword) {
        let UserDataState::Ready(u) = &self.user else {
            self.notice.set("收藏不可用：用户数据未能打开".into());
            return;
        };
        // 以库中的真实状态为准，而非卡片上那个可能已经过时的 `fav`。
        let now_fav = match u.is_favorite(hw) {
            Ok(b) => b,
            Err(e) => {
                self.notice.set(format!("收藏状态读取失败：{e}"));
                return;
            }
        };
        let r = if now_fav {
            u.remove_favorite(hw)
        } else {
            u.add_favorite(hw, now_secs())
        };
        match r {
            Ok(()) => {
                self.notice.set(String::new());
                self.bump();
            }
            Err(e) => self.notice.set(format!(
                "{}「{hw}」失败：{e}",
                if now_fav { "取消收藏" } else { "收藏" }
            )),
        }
    }

    /// 报一条成功回执。
    fn note_ok(&self, text: impl Into<String>) {
        write_note(
            self.settings_note,
            self.settings_note_tone,
            Role::Success,
            text,
        );
    }

    /// 报一条失败/拒绝消息。
    fn note_err(&self, text: impl Into<String>) {
        write_note(
            self.settings_note,
            self.settings_note_tone,
            Role::Danger,
            text,
        );
    }

    /// 收起消息条。不动语气——空串本就不显示，留着上一条的语气无人可见，而多写一次
    /// 信号会多触发一轮重绘。
    fn note_clear(&self) {
        self.settings_note.set(String::new());
    }

    /// 宣告设置有变，令设置页重建。
    fn bump_settings(&self) {
        bump(self.settings_rev);
    }

    /// 落盘当前设置。**失败当场告知**——设置是用户主动表达的意图，静默失败会让人
    /// 以为改好了，下次启动才发现没变。与收藏写入失败同一条原则。
    ///
    /// 返回是否成功，供调用方决定要不要接着做别的（如改注册表）。
    fn save_settings(&self) -> bool {
        let UserDataState::Ready(u) = &self.user else {
            self.note_err("设置无法保存：用户数据未能打开");
            return false;
        };
        // 先落成一句再 match：`match` 的 scrutinee 里的 `Ref` 会活到整个 match 结束，
        // 而分支里的 `bump_settings()` 会触发重建。今天重建只 `borrow()`，但只要将来
        // 有人在重建路径上写一次 `borrow_mut()`，就是运行期 panic 且现场极难读。
        let saved = u.save_settings(&self.settings.borrow());
        match saved {
            Ok(()) => {
                self.bump_settings();
                true
            }
            Err(e) => {
                self.note_err(format!("保存设置失败：{e}"));
                false
            }
        }
    }

    /// 改左栏宽度并重建左栏。**不写库**——拖动途中每帧都会调它。
    ///
    /// **没变就整个跳过**。`Signal::set` 无条件递增版本，不比较新旧值，所以指针在同一
    /// 像素上抖动时若照走一遍，就是一串白重建；而「点一下分隔条没拖动」恰恰是最常见的
    /// 误触。
    fn set_left_w(&self, w: i32) {
        if self.settings.borrow().left_pane_w == w {
            return;
        }
        self.settings.borrow_mut().left_pane_w = w;
        bump(self.pane_rev);
    }

    /// 把当前栏宽存进库。拖动松手时调一次。
    ///
    /// 与 `set_left_w` 分开，是因为两者的频率差着数量级：宽度每帧都可能变，而值得
    /// 存盘的只有用户松手时那一个。合在一起就是把拖过的每一帧都写一遍 SQLite。
    ///
    /// 先改后存，与 `set_skin` 同一取向：栏宽是纯布局、可随时再拖，让用户当场看到结果
    /// 比「先确保存住」更重要；存失败时如实告知本次有效、重启回退。
    fn save_left_w(&self) {
        if self.save_settings() {
            self.note_clear();
        } else {
            self.note_err("栏宽已调整，但未能保存，重启后会回到原来的宽度");
        }
    }

    /// 离开设置页。**离开设置页只有这一个出口。**
    ///
    /// 它做的不止是切页：还要撤销「确认清空」那个待发状态。清空历史是两步确认的
    /// （先「清空」变成「确认清空」，再点一次才真清），而副标题许诺的正是「切走本页
    /// 取消」。
    ///
    /// 抽成一个方法，是因为出口不止一个——返回按钮之外还有 Esc，而 Esc 那条是后加的，
    /// 当时只切了页。于是「点清空 → 按 Esc 离开 → 下次进设置页」这条路上，按钮仍举着
    /// 「确认清空」等人误触，一下就抹掉全部历史，而那份数据不可再生（ADR-0011）。
    /// 两处各写一遍必然还会漏第三处。
    fn leave_settings(&self) {
        self.confirm_clear.set(false);
        self.page.set(PAGE_DICT);
    }

    /// 把左栏宽度恢复成默认值并存盘。
    fn reset_left_w(&self) {
        self.set_left_w(crate::settings::LEFT_PANE_W_DEFAULT);
        self.save_left_w();
    }

    /// 系统此刻是否偏好暗色（见 `State::system_dark`）。
    fn system_dark(&self) -> bool {
        self.system_dark.get()
    }

    /// 当前设置解析出来的明暗档。`SkinMode::System` 时看系统那一位。
    fn dark(&self) -> bool {
        self.settings.borrow().mode.is_dark(self.system_dark())
    }

    /// 把当前的风格 × 明暗兑现到界面上。**换配色只有这一条路。**
    ///
    /// 三个入口都汇到这里：改风格、改明暗、系统外观变了。分开写的话，早晚有一个入口
    /// 漏掉重建那一步——而漏掉的表现是「文字变了色、图标没变」，看起来像渲染出了问题，
    /// 不像是少调了一个函数。
    ///
    /// **立即生效**：正文与色块靠 `Role` 每帧自己跟上；图标不行——它的颜色在构建期就
    /// 解析成了具体色值（见 `crate::icon`），只能靠重建整树跟上，理由见 `build`。
    fn apply_skin(&self) {
        let (style, dark) = (self.settings.borrow().style, self.dark());
        self.theme.set(style.theme(dark));
        bump(self.skin_rev);
    }

    /// 换风格。
    ///
    /// 先换后存：换配色是纯视觉、可随时再换，让用户当场看到结果比「先确保存住」更
    /// 重要；存失败时如实告知「本次有效、重启后回退」，而不是回滚掉一个用户已经看到
    /// 的变化。
    fn set_style(&self, style: SkinStyle) {
        self.settings.borrow_mut().style = style;
        self.apply_skin();
        self.save_skin("配色已切换");
    }

    /// 换明暗档（含「跟随系统」）。
    fn set_mode(&self, mode: SkinMode) {
        self.settings.borrow_mut().mode = mode;
        self.apply_skin();
        self.save_skin("明暗已切换");
    }

    /// 系统的外观偏好变了。只在 `SkinMode::System` 下真正换色。
    ///
    /// 不在别的档下换色，但**照样记下这一位**：用户可能过一会儿才切到「跟随系统」，
    /// 那时得用上最新的值，而不是启动时读到的那个。
    fn system_theme_changed(&self, dark: bool) {
        if self.system_dark.replace(dark) == dark {
            return;
        }
        if self.settings.borrow().mode == SkinMode::System {
            self.apply_skin();
        }
    }

    fn save_skin(&self, what: &str) {
        if self.save_settings() {
            self.note_clear();
        } else {
            self.note_err(format!("{what}，但未能保存，重启后会回到原来的配色"));
        }
    }

    /// 改唤起热键。**立即生效**：`HotkeyHandle::set` 下一次消息循环向系统换注册。
    ///
    /// 拦住无修饰键：那会吞掉该字母在**所有程序**里的输入，用户按一下 D 就唤起词典，
    /// 等于没法打字了——而这个错误一旦犯下，用户很难意识到是词典干的。
    fn set_hotkey(&self, spec: crate::settings::HotkeySpec) {
        if !spec.is_safe() {
            self.note_err(
                "字母或数字作主键时至少要带一个 Ctrl / Alt / Shift，否则会吞掉该键在所有程序里的输入；F1–F12 可以单独用",
            );
            return;
        }
        self.settings.borrow_mut().hotkey = spec;
        self.hotkey.set(spec.to_hotkey());
        if self.save_settings() {
            // 托盘提示里写着热键。不跟着改的话，用户改完热键去托盘上一悬停，看到的
            // 是**旧的那个组合**——一句由程序自己给出、且明确是错的信息。
            self.tray.set_tooltip(tray_tip(&spec));
            self.note_ok(format!("唤起热键已改为 {spec}"));
        }
    }

    /// 开关开机自启。
    ///
    /// 先写注册表再落库：注册表才是**真相**（用户可能在别处删掉自启项），库里存的
    /// 只是界面初值。反过来先落库的话，注册表写失败时库里就留下了一个假状态。
    fn set_autostart(&self, on: bool) -> bool {
        if let Err(e) = crate::autostart::set(on) {
            self.note_err(format!("设置开机启动失败：{e}"));
            return false;
        }
        self.settings.borrow_mut().autostart = on;
        if !self.save_settings() {
            // 注册表已改、库没写上。真实状态仍是对的（`autostart_now` 以注册表为准），
            // 只是这次没能记进库里——如实报出，不谎称成功。
            return false;
        }
        self.note_clear();
        true
    }

    /// 开机自启的**真实**状态：以注册表为准，读不到才退回库里存的值。
    ///
    /// 注册表才是权威——用户可能在任务管理器或 msconfig 里禁掉自启，那时库里仍是
    /// `true`，若拿库值做开关初值，开关会一直显示开着而实际早已关闭。
    fn autostart_now(&self) -> bool {
        crate::autostart::is_enabled().unwrap_or_else(|_| self.settings.borrow().autostart)
    }

    /// 英英释义是否默认展开。**立即生效**——它只影响此后新建卡片的初始展开态。
    fn set_expand_en(&self, on: bool) -> bool {
        self.settings.borrow_mut().expand_en = on;
        if !self.save_settings() {
            return false;
        }
        self.note_clear();
        true
    }

    /// 换词库路径。只能重启生效：词库连接在 `main` 里打开后交给了界面，运行期换库
    /// 意味着重建整条查询链路，而那点收益抵不上它带来的状态一致性问题。
    fn set_dict_dir(&self, dir: Option<std::path::PathBuf>) {
        // **先校验再落库**。选错目录的代价是致命的：词库打不开时 `main` 直接退出，
        // 而 release 构建没有控制台（`windows_subsystem = "windows"`），用户看到的是
        // 「双击没反应、托盘不出现、零提示」。
        //
        // 光看文件名在不在不够：把汉英库改名成 `ecdict.db` 放进去，只看名字的检查会
        // 照收，而两个库的表结构完全不同。`check_dir` 会真开一次并试查。
        if let Some(p) = &dir {
            let st = crate::source::offline::check_dir(p);
            if !st.usable() {
                self.note_err(format!(
                    "这个目录不能用作词库目录：{}",
                    st.missing().join("；")
                ));
                return;
            }
        }
        let has = dir.is_some();
        self.settings.borrow_mut().dict_dir = dir;
        if self.save_settings() {
            self.note_ok(if has {
                "词库目录已更改，重启后生效"
            } else {
                "已恢复默认词库目录，重启后生效"
            });
            self.bump_settings();
        }
    }

    /// 当前生效的词典目录：设置里指定的，否则默认目录。
    ///
    /// 每次现算而不是开机存一份：用户可以改目录，而存一份就要在改动时同步两处。
    fn user_dict_dir(&self) -> Option<std::path::PathBuf> {
        self.settings
            .borrow()
            .user_dict_dir
            .clone()
            .or_else(|| crate::source::user::default_dir().ok())
    }

    /// 换词典目录。`None` = 恢复默认。
    ///
    /// 与换词库路径（`set_dict_path`）不同，这个**当场生效**不用重启：那两个库的
    /// 连接在 `main` 里打开后就交给了界面，而自带词典由 `State` 自己持有。
    fn set_user_dict_dir(&self, dir: Option<std::path::PathBuf>) {
        self.settings.borrow_mut().user_dict_dir = dir;
        if self.save_settings() {
            self.reload_user_dicts();
            let n = self.user_dicts.borrow().len();
            self.note_ok(format!("词典目录已更改，扫到 {n} 本"));
            self.bump_settings();
        }
    }

    /// 开关一本自带词典。`file` 是文件名，见 `Settings::disabled_dicts`。返回是否落实。
    ///
    /// **不触发设置页重建**：这一条是从 widget 的 `on_update` 里调进来的，而重建会把
    /// 调用者所在的那棵子树连同它读的信号一起回收。开关本身已经把状态画出来了，
    /// 那一行的其余文字（词典名、词条数）描述的是这个文件，与开关与否无关。
    fn toggle_user_dict(&self, file: &str, on: bool) -> bool {
        {
            let mut s = self.settings.borrow_mut();
            s.disabled_dicts.retain(|x| x != file);
            if !on {
                s.disabled_dicts.push(file.to_string());
            }
        }
        if !self.save_settings() {
            return false;
        }
        self.reload_user_dicts();
        true
    }

    /// 重扫词典目录并打开启用的那些。
    ///
    /// 整个重来而不是增量增删：目录才是唯一的真相——用户可能在程序开着的时候往里
    /// 拖了一本、或者删掉一本，增量维护的那份列表根本不知道。重开一本实测 2 ms
    /// 量级（索引只有几十 KB），而这条路径只在开机与用户动设置时才走。
    fn reload_user_dicts(&self) {
        let Some(dir) = self.user_dict_dir() else {
            self.user_dicts.borrow_mut().clear();
            return;
        };
        let disabled = self.settings.borrow().disabled_dicts.clone();
        *self.user_dicts.borrow_mut() = crate::source::user::load(&dir, &disabled);
        self.apply_dict_aliases();
    }

    /// 从当前页签移除一行：历史页删历史条目，收藏页取消收藏。
    ///
    /// **动作随页签而变**是刻意的：召回行的 × 意思是「把这一行从我眼前的这个列表里
    /// 去掉」。在历史里那是删记录，在收藏里那是取消收藏——若两处都去删历史，收藏页
    /// 的 × 就会点了没反应。
    ///
    /// 失败当场告知：两者都是用户主动表达的意图，与历史的**被动写入**不同。
    fn remove_recall_row(&self, hw: &Headword) {
        let UserDataState::Ready(u) = &self.user else {
            self.notice.set("用户数据未能打开，无法修改".into());
            return;
        };
        let on_favorites = self.left_tab.get() == LEFT_FAVORITES;
        let r = if on_favorites {
            u.remove_favorite(hw)
        } else {
            u.remove_history(hw)
        };
        match r {
            Ok(()) => {
                self.notice.set(String::new());
                self.bump();
            }
            Err(e) => self.notice.set(format!(
                "{}「{hw}」失败：{e}",
                if on_favorites {
                    "取消收藏"
                } else {
                    "删除历史"
                }
            )),
        }
    }

    /// 清空全部历史记录。破坏性且不可撤销，故调用方须先做二次确认。
    fn clear_history(&self) {
        let UserDataState::Ready(u) = &self.user else {
            self.note_err("用户数据未能打开，无法清空");
            return;
        };
        match u.clear_history() {
            Ok(()) => {
                self.note_ok("历史记录已清空");
                self.bump();
            }
            Err(e) => self.note_err(format!("清空历史失败：{e}")),
        }
    }

    /// 当前展示的第一个词头，供左栏的召回行标出「你正在看的是这条」。
    fn current_headword(&self) -> Option<String> {
        self.cards
            .get()
            .first()
            .map(|c| c.headword.as_str().to_string())
    }

    /// 历史与收藏的条数，供设置页如实显示「要清掉多少」。
    fn counts(&self) -> (usize, usize) {
        let UserDataState::Ready(u) = &self.user else {
            return (0, 0);
        };
        (
            u.history(usize::MAX).map(|v| v.len()).unwrap_or(0),
            u.favorites().map(|v| v.len()).unwrap_or(0),
        )
    }

    /// 写收藏备注。空串视作清除备注。
    ///
    /// **不 `bump()`**：备注变更不影响侧栏列表与卡片分组，而 `bump` 会触发整轮重建，
    /// 逐字敲备注时那意味着每个字符重建一次结果区——输入框会被连同重建掉，焦点与
    /// 光标位置随之丢失，根本没法打字。
    fn set_note(&self, hw: &Headword, note: String) {
        let UserDataState::Ready(u) = &self.user else {
            return;
        };
        let arg = if note.trim().is_empty() {
            None
        } else {
            Some(note.trim())
        };
        if let Err(e) = u.set_note(hw, arg) {
            self.notice.set(format!("保存备注失败：{e}"));
        }
    }

    /// 取某词头的收藏备注。未收藏或无备注时为空串。
    fn note_of(&self, hw: &Headword) -> String {
        let UserDataState::Ready(u) = &self.user else {
            return String::new();
        };
        u.favorites()
            .ok()
            .and_then(|v| v.into_iter().find(|f| &f.headword == hw))
            .and_then(|f| f.note)
            .unwrap_or_default()
    }

    /// 宣告用户数据有变，驱动左栏与卡片重取。
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// 把一个词压进导航路径。
    fn push_nav(&self, word: &str) {
        self.nav.borrow_mut().push(word);
        bump(self.nav_rev);
    }

    /// 重查当前正在看的那个词。
    ///
    /// 走 `preview`（不记历史、不压导航路径）：刷新不是一次新的查询，用户只是想让词条
    /// 重新读一遍库——换过词库文件之后尤其有用。没有词在看时什么也不做，而不是把结果区
    /// 清空：那会把「刷新」变成「清屏」，是两件不同的事。
    fn refresh(&self) {
        if let Some(w) = self.current_headword() {
            self.preview(&w);
        }
    }

    /// 沿导航路径走一步。`forward` 为真前进，否则后退。
    ///
    /// 走到的词用 `preview` 查（不记历史、不再压回路径）：后退不是一次新的查询，而是
    /// 回到一个已经发生过的位置——浏览器的后退同样不会在历史里新增一条。
    ///
    /// 借用必须在 `preview` **之前**放掉：那条路径会一路走到 `rebuild_cards`，将来若有
    /// 谁在重建里读一次导航状态，留着 `borrow_mut` 就是运行期 panic，且现场极难读。
    fn go_nav(&self, forward: bool) {
        let word = self.nav.borrow_mut().go(forward);
        let Some(word) = word else {
            return;
        };
        self.preview(&word);
        bump(self.nav_rev);
    }

    /// 路径上还有没有可后退 / 可前进的一步。供按钮决定要不要置灰。
    fn can_go(&self, forward: bool) -> bool {
        self.nav.borrow().can_go(forward)
    }

    /// 按当前页签重算左栏的行与空态文案。
    ///
    /// 三个页签走同一个出口（`left_rows`），故切页签只换数据、不换结构——这正是三段
    /// 共用一个列表要买的东西，见 `LEFT_CANDIDATES` 一族常量的注释。
    ///
    /// 用户数据不可用或读取失败时给出空列表——顶部的警示条已说明原因，此处再报一遍
    /// 是噪音。
    /// 行数变了之后把键盘游标收回表内。**每次铺完行都要调。**
    ///
    /// 游标存的是「第几行」，而行是会变少的：在收藏页逐条点 × 取消收藏，行数一路往下
    /// 掉，游标却原地不动。`move_cursor` 只钳**新**值（`(i+1).min(n-1)`），钳不住已经
    /// 越界的旧值——↓ 一步就跳回表内，↑ 却是 `saturating_sub(1)`，从 5 走到 4 仍在表外，
    /// 得连按几下才回得来。这期间没有任何一行带高亮，`row_at_cursor()` 返回 None，
    /// 按回车会落到「查输入框里的字」那条兜底上，查的是另一个词。
    ///
    /// 只在值真的越界时写：`Signal::set` 无条件涨版本，而 `ListKeyNav` 盯着 `cursor`
    /// 做滚动跟随——每次铺行都写一次，列表就会在用户没碰键盘时自己跳。
    fn clamp_cursor(&self, n: usize) {
        let max = n.saturating_sub(1);
        if self.cursor.get() > max {
            self.cursor.set(max);
        }
    }

    fn reload_left(&self) {
        if self.left_tab.get() == LEFT_CANDIDATES {
            self.reload_candidates();
            return;
        }
        self.reload_recall();
    }

    /// 候选页：把当前候选连同键盘游标、当前查看的词铺成行。
    fn reload_candidates(&self) {
        let cands = self.candidates.get();
        // 先收游标再读它：候选可能刚变少（见 `clamp_cursor`）。
        self.clamp_cursor(cands.len());
        let at = self.cursor.get();
        // 「正在看的是哪个词」对候选行与召回行是同一个问题，故用同一个来源。
        let current = self.current_headword();
        let rows: Vec<LeftRow> = cands
            .into_iter()
            .enumerate()
            .map(|(i, cand)| LeftRow::Candidate {
                active: current.as_deref() == Some(cand.headword.as_str()),
                cand,
                at_cursor: i == at,
            })
            .collect();
        // 空态分两种，说的不是一回事：没打字是「还没开始」，打了字没候选是「词典里
        // 确实没有以它开头的词」。合成一句话会让后者读起来像前者，用户会以为自己
        // 没打进去。
        self.left_note.set(if !rows.is_empty() {
            String::new()
        } else {
            let q = self.query.get();
            let q = q.trim();
            if q.is_empty() {
                "输入一个词开始补全".into()
            } else {
                format!("没有以「{q}」开头的词")
            }
        });
        self.left_rows.set(rows);
    }

    /// 历史页 / 收藏页：从用户数据取词头，并标出当前正在看的那条。
    fn reload_recall(&self) {
        let on_favorites = self.left_tab.get() == LEFT_FAVORITES;
        let UserDataState::Ready(u) = &self.user else {
            self.clamp_cursor(0);
            self.left_rows.set(Vec::new());
            self.left_note.set("用户数据未能打开".into());
            return;
        };
        let hws = if on_favorites {
            u.favorites()
                .map(|v| v.into_iter().map(|f| f.headword).collect::<Vec<_>>())
        } else {
            u.history(RECALL_LIMIT)
                .map(|v| v.into_iter().map(|h| h.headword).collect())
        };
        let hws = hws.unwrap_or_default();
        let current = self.current_headword();
        // 先收游标再读它：逐条取消收藏会让行数一路往下掉（见 `clamp_cursor`）。
        self.clamp_cursor(hws.len());
        let at = self.cursor.get();
        let rows: Vec<LeftRow> = hws
            .into_iter()
            .enumerate()
            .map(|(i, headword)| {
                let active = current.as_deref() == Some(headword.as_str());
                LeftRow::Recall {
                    headword,
                    at_cursor: i == at,
                    active,
                }
            })
            .collect();
        self.left_note.set(if !rows.is_empty() {
            String::new()
        } else if on_favorites {
            "还没有收藏任何词".into()
        } else {
            "还没有查询记录".into()
        });
        self.left_rows.set(rows);
    }

    /// 按右栏页签筛出该显示的卡片，并给出筛空时的说明。
    ///
    /// 「筛空」是这里唯一需要解释的状态：一次查询只走一个方向，故查 `apple` 时
    /// 「汉英」页必然一条也没有。那**不是**故障，但空白一片看起来像故障，所以必须
    /// 有一句话——而 `hint` 讲的是查询本身（未收录、经原形命中），不能拿来讲页签。
    fn refilter_cards(&self) {
        let all = self.cards.get();

        // 先更新每个页签的可用态。判据是**全部结果**，与此刻停在哪一页无关——
        // 拿筛过的结果去判，会让除当前页之外的所有页签一起变灰。
        //
        // 一条结果都没有时**全部保持可用**：那是「还没查」或「这个词没收录」，不是
        // 「这本词典没有它」。全灰会让开屏那一眼看着像所有词典都坏了。
        let idle = all.is_empty();
        for t in self.tabs.borrow().iter() {
            let any = idle
                || matches!(t.key, TabKey::All)
                || all
                    .iter()
                    .any(|c| c.entries.iter().any(|e| entry_in_tab(e, &t.key)));
            if t.on.get() != any {
                t.on.set(any);
            }
        }

        let (key, label) = {
            let tabs = self.tabs.borrow();
            match tabs.get(self.dict_tab.get()) {
                Some(t) => (t.key.clone(), t.label.clone()),
                // 越界不该发生（下标由 `TabBar` 写，取值受标签数约束），但「筛空了」
                // 比 panic 更难查，故这里宁可退回全收。
                None => (TabKey::All, String::new()),
            }
        };

        let had_any = !all.is_empty();
        let kept = filter_cards(all, &key);
        self.filter_note.set(if had_any && kept.is_empty() {
            format!("本次查询在「{label}」里没有词条。")
        } else {
            String::new()
        });
        self.visible_cards.set(kept);
    }

    /// 重建页签集合：全部 + 内置两份 + 每本自带词典。
    ///
    /// 只在**页签集合本身**变化时调（装卸词典、改名、开关某本），不在每次查询时调——
    /// 查询只改各页签的可用态，那走信号（见 [`TabSpec::on`]）。
    fn rebuild_tabs(&self) {
        use crate::source::offline::{CEDICT_KEY, CEDICT_NAME, ECDICT_KEY, ECDICT_NAME};
        let mut v = {
            let s = self.settings.borrow();
            vec![
                TabSpec::new(TabKey::All, "全部".into()),
                TabSpec::new(
                    TabKey::Builtin(ECDICT_KEY),
                    s.dict_name(ECDICT_KEY, ECDICT_NAME),
                ),
                TabSpec::new(
                    TabKey::Builtin(CEDICT_KEY),
                    s.dict_name(CEDICT_KEY, CEDICT_NAME),
                ),
            ]
        };
        for d in self.user_dicts.borrow().iter() {
            v.push(TabSpec::new(
                TabKey::User(d.key().to_string()),
                d.name().to_string(),
            ));
        }
        // 词典少了的话当前下标可能落到外面。回「全部」而不是钳到末项：钳过去等于把
        // 用户默默扔进另一本词典的结果里，而他并没有选它。
        if self.dict_tab.get() >= v.len() {
            self.dict_tab.set(DICT_ALL);
        }
        *self.tabs.borrow_mut() = v;
        bump(self.tabs_rev);
        self.refilter_cards();
    }

    /// 把设置里的自定义名字贴到已打开的词典上。
    fn apply_dict_aliases(&self) {
        let names = self.settings.borrow().dict_names.clone();
        for d in self.user_dicts.borrow_mut().iter_mut() {
            let alias = names.iter().find(|(k, _)| k == d.key()).map(|(_, v)| v);
            d.set_alias(alias.map(String::as_str));
        }
    }

    /// 给某个来源改名。空名字 = 恢复默认。
    fn set_dict_name(&self, key: &str, name: &str) {
        self.settings.borrow_mut().set_dict_name(key, name);
        if self.save_settings() {
            self.apply_dict_aliases();
            self.rebuild_tabs();
        }
    }

    /// 把一次查询命中的全部词头记入历史记录。
    ///
    /// **整批共用一个时刻**：一次查询就是一个时间点。若逐条各取 `now_secs()`，恰好
    /// 跨秒时同一次查询的几个词头会拿到不同时间，历史按时间倒序展示的相对次序便与
    /// 界面呈现次序对不上——那正是 `headwords_to_record` 费力保序要避免的事。
    ///
    /// 写入失败**静默忽略**：历史是系统被动记录的事实，用户并未要求它发生，为它
    /// 打断查询或弹错都不成比例。用户数据整体不可用这一情况另有交代——那是启动时
    /// 就已知的状态，由界面如实展示，不在这条偶发写入路径上处理。
    ///
    /// **此静默策略仅适用于历史。收藏是用户主动表达的意图**：点了收藏就是要求它
    /// 发生，写入失败必须告知用户，绝不可把这里的 `let _ =` 复制过去。
    fn record_all(&self, hws: &[Headword]) {
        let UserDataState::Ready(u) = &self.user else {
            return;
        };
        let at = now_secs();
        for hw in hws {
            let _ = u.record(hw, at);
        }
        // 历史变了，侧栏该重取。即便个别写入失败也照样宣告——成功的那些仍需呈现，
        // 而全失败时重取会拿回一份相同的列表——注意这**不是**零代价：`Signal::set`
        // 无条件递增版本（不比较新旧值），故列表照样全量重建、照样多一帧重绘。
        // 规模是个人级的几百条，这个代价可以接受，但别以为它不存在。
        self.bump();
    }
}

/// 把左栏宽度钳进合法区间。
///
/// 两层上限，缺一不可：
///
/// - `settings` 里那对 MIN/MAX 是与窗口无关的**硬边界**，防止设置库被手改成荒唐值。
/// - `root_w - RIGHT_MIN_W` 是跟着窗口走的那层，保证右栏永远留得下能读的宽度。
///
/// `root_w <= 0` 时只施加硬边界：那说明还没有布局过（首帧之前），此时按一个不存在的
/// 窗口宽去算上限，只会把用户的设置无端改小。
fn clamp_left_w(w: i32, root_w: i32) -> i32 {
    let w = w.clamp(
        crate::settings::LEFT_PANE_W_MIN,
        crate::settings::LEFT_PANE_W_MAX,
    );
    if root_w <= 0 {
        return w;
    }
    // `max` 兜住极窄窗口：那时两个下限打架，让左栏保住它的下限、右栏被挤，
    // 总好过左栏被压到列不出一个词头。
    let cap = (root_w - RIGHT_MIN_W).max(crate::settings::LEFT_PANE_W_MIN);
    w.min(cap)
}

/// 当前窗口宽度（根节点的宽）。拿不到时返回 0，由 `clamp_left_w` 按「还没布局过」处理。
fn root_width(ctx: &mut EventCtx) -> i32 {
    let tree = ctx.tree_mut();
    let root = tree.root;
    root.and_then(|r| tree.get(r))
        .map(|n| n.bounds.w)
        .unwrap_or(0)
}

/// 把词条按词头分组，组的顺序与组内顺序都保持原样。
///
/// 分组的单位是词头而非词条，因为**收藏的单位是词头**：多音字（`行` 的 hang2 /
/// xing2）是两条词条、一个词头，只该有一个星标；而 `餘` 命中的 `余` / `馀` 是两个
/// 词头，必须能分别收藏。
///
/// 与 `headwords_to_record` 共用同一套「去重保序」语义，故直接借它定序——两者若
/// 各自实现，历史记录的顺序与界面呈现的顺序就会有机会漂移。
fn group_by_headword(entries: &[Entry]) -> Vec<(Headword, Vec<Entry>)> {
    headwords_to_record(entries)
        .into_iter()
        .map(|hw| {
            let group = entries
                .iter()
                .filter(|e| e.headword() == &hw)
                .cloned()
                .collect();
            (hw, group)
        })
        .collect()
}

/// 一次命中中应当进入历史记录的词头：**全部**，去重且保序。
///
/// 进历史的是**词头**而非查询词：查 `tried` 命中 `try` 时记的是 `try`。术语表把
/// 历史定义为「查询过的词头序列」，而词头是词典中真实存在的那个词；`record` 收
/// `&Headword` 的签名已从类型上强制了这一点。
///
/// 取全部而非第一条：汉英词库按 `WHERE simplified = ?1 OR traditional = ?1` 查询，
/// 一个繁体查询词可能命中多行、而那些行的**简体列并不相同**——如 `餘` 同时命中
/// `余` 与 `馀` 两个词头。界面本就把它们全部呈现（见 `cedict.rs` 的「谁排前面都是
/// 错的」），历史只记第一条会与用户所见对不上。何况第一条取的是建库插入顺序，
/// 不含任何「主要词头」语义。
///
/// 去重保序而非用集合：同一词头的多音字（`行[hang2]`/`行[xing2]`）会返回多条词条
/// 但词头相同，只该记一次；而记录顺序应与界面呈现顺序一致，排序会打乱它。
///
/// 本函数**看不出**这些词条是直接命中还是经词形变化跟随原形得来的——到这一层
/// `entries` 里的词头已经是原形（`ecdict.rs` 早已把原形词条换了进来）。故「查
/// `tries` 记 `try`」这条语义由 `store::ecdict` 的 `变化形态无释义时跟随到原形`
/// 负责验证，此处无从、也不该重复断言。
fn headwords_to_record(entries: &[Entry]) -> Vec<Headword> {
    let mut out: Vec<Headword> = Vec::new();
    for e in entries {
        let hw = e.headword();
        if !out.iter().any(|seen| seen == hw) {
            out.push(hw.clone());
        }
    }
    out
}

/// `build` 的产物：界面树，加上一份窗口级快捷键处理器。
///
/// 打包成一个结构而非让 `main` 各取一次，是因为两者共享同一个 `State`——分成两个函数
/// 就得把 `State` 或它的构造过程暴露出去，而它是这个模块的全部内部机制。
pub struct Ui {
    pub root: Element,
    /// 交给 [`windui::app::App::on_shortcut`]。
    pub shortcut: ShortcutFn,
    /// 交给 [`windui::app::App::on_system_theme_changed`]。
    ///
    /// 只在 `SkinMode::System` 下真正换色，但任何时候都记下系统那一位——用户可能过
    /// 一会儿才切到「跟随系统」，那时该用最新的值。
    pub system_theme: SystemThemeFn,
}

/// 窗口级快捷键处理器。抽成别名只为让 [`Ui`] 的字段读得下去。
pub type ShortcutFn = Box<dyn FnMut(&mut ShortcutCtx, KeyEvent) -> bool>;

/// 系统外观变化的处理器。同上。
pub type SystemThemeFn = Box<dyn FnMut(&mut EventCtx, bool)>;

/// 构建界面。
///
/// `user` 不可用时顶部常驻一条警示，说明历史记录失效及其原因（收藏有入口后一并
/// 纳入，见 `unavailable_bar`）。
pub fn build(
    dict: OfflineDictionary,
    user: UserDataState,
    theme: ThemeHandle,
    hotkey: HotkeyHandle,
    tray: windui::platform::TrayHandle,
    // 系统此刻是否偏好暗色。由 `main` 查好传进来而非在此现查：它是 OS 状态，
    // 而本模块的其余部分只碰自己的状态。
    system_dark: bool,
) -> Ui {
    let dict = Rc::new(dict);
    let unavailable = match &user {
        UserDataState::Ready(_) => None,
        UserDataState::Unavailable(why) => Some(why.clone()),
    };
    // 设置读不到时退回默认（`Settings::from_pairs` 保证），不让程序起不来。
    let settings = match &user {
        UserDataState::Ready(u) => u.settings(),
        UserDataState::Unavailable(_) => Settings::default(),
    };
    let st = Rc::new(State {
        dict: dict.clone(),
        // 先建空的，`State` 造好之后再扫——扫描要读设置里的目录与开关，那是
        // `State` 的方法。
        user_dicts: RefCell::new(Vec::new()),
        user,
        query: signal(String::new()),
        candidates: signal(Vec::new()),
        cursor: signal(0),
        cards: signal(Vec::new()),
        visible_cards: signal(Vec::new()),
        hint: signal(String::from("输入一个词开始查询")),
        filter_note: signal(String::new()),
        tray,
        dict_tab: signal(DICT_ALL),
        tabs: RefCell::new(Vec::new()),
        tabs_rev: signal(vec![0]),
        // 开屏停在历史页：DESIGN.md 的「Search is home / Recall is a drawer」讲的是
        // 别拿历史当主导航，不是别让人看见它。左栏那 280px 在开屏时**没有别的内容**
        // 可放——候选页此刻是空的——而「最近查过什么」正好是下一次查询的起点。
        left_tab: signal(LEFT_HISTORY),
        left_rows: signal(Vec::new()),
        left_note: signal(String::new()),
        nav: RefCell::new(NavPath::default()),
        nav_rev: signal(vec![0]),
        focus_list: signal(vec![0]),
        revision: signal(0),
        notice: signal(String::new()),
        expanded: ExpandedStates::default(),
        theme,
        hotkey,
        page: signal(PAGE_DICT),
        pane_rev: signal(vec![0]),
        settings: RefCell::new(settings),
        settings_note: signal(String::new()),
        settings_note_tone: signal(Role::Danger),
        confirm_clear: signal(false),
        settings_rev: signal(vec![0]),
        skin_rev: signal(vec![0]),
        system_dark: std::cell::Cell::new(system_dark),
    });
    // 扫词典目录。放在 `State` 造好之后，是因为扫描要读设置里的目录与开关，
    // 那两样都得先有 `State` 才拿得到。
    st.reload_user_dicts();
    // 页签集合依赖上面扫到的那批词典，故必须排在它后面。
    st.rebuild_tabs();
    // 开屏即把左栏填上：驱动器要到第一次 layout 才跑，在那之前列表是空的，
    // 而开屏那一眼正好落在这里。
    st.reload_left();

    // 整棵树挂在 `skin_rev` 上：换肤时重建一次。
    //
    // **为什么又要重建了**。ADR-0012 结案时的结论是「界面无一处写死颜色，全部走 `Role`，
    // 故换肤只需 `ThemeHandle::set`，下一帧全树按新色板重新解析」——那条对**文字与色块**
    // 至今成立。图标不成立：windui 的 `ImageContent::tint` 收 `Color` 而非 `Role`，
    // 且按钮画图标时用的是图标自带的 tint、不套用按钮前景色，故图标的颜色只能在**构建期**
    // 解析（见 `crate::icon` 的模块头）。构建期取的值不会自己更新，只能重建。
    //
    // 取舍：要么图标换肤后颜色不跟（深色皮肤下一个深灰的 × 等于看不见），要么换肤重建
    // 一次。选后者。换肤是用户主动、低频的动作，而重建的代价这里几乎为零——查询词、
    // 候选、结果卡片、展开态全都活在元素树**之外**的信号里（这正是 `ExpandedStates`
    // 当初被搬出树的理由），重建后原样还在。会丢的只有滚动位置与 hover 态。
    //
    // 只包一层、不逐处细分：漏掉任何一处含图标的子树，就会出现「有的跟了有的没跟」，
    // 那是最难查的一类不一致。整树重建换来的是「不可能漏」。
    let sc_st = st.clone();
    let sys_st = st.clone();
    let root = Element::host_signal(st.skin_rev, move |_rev: u64| {
        // `host_signal` 的回调是 `Fn`，会被反复调用，故每次都得拿一份自己的。
        window_root(st.clone(), unavailable.clone())
    });
    Ui {
        root,
        shortcut: Box::new(move |ctx, ev| handle_shortcut(&sc_st, ctx, ev)),
        system_theme: Box::new(move |_ctx, dark| sys_st.system_theme_changed(dark)),
    }
}

/// 窗口级快捷键。返回 `true` = 已处理。
///
/// 这里只放**与焦点无关**的键：无论用户此刻停在查询框、列表还是右栏正文上，它们都该
/// 生效。与焦点强相关的（↑↓ 走行、→ 接受补全、Enter 查询）留在各自控件的 `on_nav_key`
/// 里——那些键在不同控件上本就该有不同含义，塞进这里反而要在每个分支里回头判断焦点。
///
/// windui 保证这条回调**排在焦点控件之后**：输入框正在打字时，字符键先被它吃掉，轮不
/// 到这里。所以下面每一条都必须带修饰键或是功能键，裸字符键放进来会截胡打字。
///
/// 键位见 `SHORTCUTS`——那张表是给用户看的说明书，改这里也要改它。
fn handle_shortcut(st: &Rc<State>, ctx: &mut ShortcutCtx, ev: KeyEvent) -> bool {
    if !ev.pressed {
        return false;
    }
    match ev.key {
        // Ctrl+L：回到查询框并全选。浏览器地址栏的键位，是这类「跳回主输入框」最通用
        // 的一个。走 windui 的 autofocus 通路，与热键唤起窗口做的是同一件事。
        Key::Other(VK_L) if ev.ctrl => {
            ctx.focus_main_input();
            true
        }
        // Ctrl+R：重查当前词。
        Key::Other(VK_R) if ev.ctrl => {
            st.refresh();
            true
        }
        // Ctrl+W：收起窗口。走关闭决策链，本项目 `hide_on_close` 会把它落成隐藏——
        // 常驻工具的 Ctrl+W 该是「收起」而不是「退出」，进程还要留着等热键。
        Key::Other(VK_W) if ev.ctrl => {
            ctx.request_close();
            true
        }
        Key::Left if ev.ctrl => {
            st.go_nav(false);
            true
        }
        Key::Right if ev.ctrl => {
            st.go_nav(true);
            true
        }
        // Esc 在设置页是**返回**，不是关窗。
        //
        // 这正是 `on_shortcut` 必须排在框架的 Escape 兜底之前的理由：兜底会直接把窗口
        // 收掉，而用户在设置页按 Esc 想的是「退出这一页」。返回 true 把这一键吃掉，
        // 兜底就轮不到了；不在设置页时返回 false 放行，Esc 照旧收起窗口（ADR-0007）。
        Key::Escape if st.page.get() == PAGE_SETTINGS => {
            st.leave_settings();
            true
        }
        _ => false,
    }
}

/// 窗口根：标题栏 + 分隔线 + 主体。每次换肤重建一次，见 `build`。
fn window_root(st: Rc<State>, unavailable: Option<String>) -> Element {
    // 无系统标题栏：整窗都是客户区，故顶部这条标题栏由我们自己画（见 `title_bar`）。
    Element::col()
        .fill()
        .bg_role(Role::Bg)
        .child(title_bar(st.clone()))
        .child(Element::divider())
        .child(body(st, unavailable).weight(1.0))
}

/// 自定义标题栏：应用标识 + 窗口按钮。
///
/// 整条 `window_drag()` 可拖动窗口；落在窗口按钮与三个文字入口上不拖、正常点击——
/// windui 沿父链自内向外找最近的裁决者，先遇到可聚焦控件就交给它（`Tree::hit_role`）。
///
/// 曾有一版把拖动区缩到品牌块上，那是上游只看命中落定节点时的绕法，已随
/// wind-ui-rust `7b6ab36` 撤回，见 `bar_entry`。
///
/// 附带一条真实的取舍：三个入口贴着窗口顶边，而可聚焦子树整体优先判 `HTCLIENT`，
/// 于是那 141px 宽的一段**让不出顶部 8px 的缩放带**——从标题栏右侧那截顶边往上拖
/// 拉不动窗口高度。窗口按钮那 138px 本来就是这样（它一直是可聚焦控件），左侧品牌区
/// 与其余三条边不受影响，够用。
fn title_bar(st: Rc<State>) -> Element {
    Element::row()
        .width_match()
        .height(38)
        .cross(Align::Stretch)
        // 标题栏底走 `SurfaceAlt` 而非皮肤里那个具体色：三套皮肤的 `titlebar` 恰好
        // 都等于 `surface_alt`，用角色表达之后换肤能自动跟随，不必重建元素树。
        .bg_role(Role::SurfaceAlt)
        .window_drag()
        .child(brand())
        // 前进 / 后退紧跟应用标识，摆在**左上角**——浏览器、文件管理器、macOS 词典
        // 都在这个位置，是这两枚箭头唯一不需要解释的落点。
        //
        // 没有放进右栏（那里离释义更近、看着更「就近」）：`TabBar` 的高度在交叉轴上
        // 会失控，与它并排或叠放都会把那一屏排坏，详见 `right_pane`。标题栏是定高的
        // 38px，不吃这个亏。
        .child(
            Element::row()
                .cross(Align::Center)
                .padding_edges(Insets::new(4, 0, 0, 0))
                .child(nav_buttons(st.clone())),
        )
        // 弹簧：把设置顶到最右。此前这个作用由 `brand().weight(1.0)` 兼任，中间插进
        // 导航按钮之后就得单拎出来——否则品牌块会把按钮一路推到窗口中间。
        .child(Element::leaf().weight(1.0))
        // 标题栏右侧只剩设置一个入口了。
        //
        // 「历史」「收藏」两个入口随召回抽屉一起撤掉——它们现在是左栏那个分段控件的
        // 两段，与列表挨在一起。入口摆在离它要打开的东西最近的地方，比摆在标题栏上
        // 再拉开一个抽屉少一步，也少一个「这两个字会打开什么」的疑问。
        //
        // 用文字而非图标：U+2699 在 Windows 上会被 Segoe UI Emoji 接管，画出来是一个
        // 彩色齿轮，与这一屏的单色格调格格不入（变体选择符 U+FE0E 无效，windui 的文本
        // 渲染不处理它）。走 SVG 又要 `ImageContent::tint` 定一个具体颜色，换肤时它
        // 不会跟着变——ADR-0012 结案段刚把「界面无一处写死颜色」这条挣回来。
        .child(settings_entry(st))
        // 窗口按钮的宽度（46px，与设计一致）、图标形状与 hover 色均由 windui 硬编码，
        // 只有图标色可调。框架的 `BTN_H = 32` 在这里不生效——本行 `cross(Stretch)`
        // 会把按钮拉到 38 高。
        //
        // hover 红沿用框架的 `0xE81123` 而非设计稿的 `#E5484D`：两者并列可辨（前者
        // 饱和度明显更高），但关闭键的 hover 红是 Windows 的系统惯例色，跟随系统比
        // 跟随设计稿更对——这不是「差别太小懒得改」。
        .child(Element::window_button(WindowButtonKind::Minimize).fg_role(Role::TextMuted))
        .child(Element::window_button(WindowButtonKind::Maximize).fg_role(Role::TextMuted))
        .child(Element::window_button(WindowButtonKind::Close).fg_role(Role::TextMuted))
}

/// 标题栏上的设置入口。
fn settings_entry(st: Rc<State>) -> Element {
    let page = st.page;
    bar_entry("设置", move |_ctx| page.set(PAGE_SETTINGS))
}

/// 标题栏上的一个文字入口。
///
/// 落在 `window_drag` 区域内仍可点：windui 沿父链自内向外找最近的裁决者，命中虽落在
/// 里面那个 `label` 上（不可聚焦），外层这个 `Clickable` 先于标题栏的 `window_drag`
/// 被遇到，于是判交互、不拖窗。
///
/// 这条**曾经不成立**，实机表现是「只有按钮最下面一条能点」：那时判定只看命中落定的
/// 节点自身，`Label::focusable()` 为 false 就当成了标题栏空白，`WM_NCHITTEST` 答
/// `HTCAPTION`，客户区连 `WM_LBUTTONDOWN` 都收不到。上游 `7b6ab36` 已把两侧判定统一
/// 成一次父链遍历（`Tree::hit_role`），经过与结案记在 `docs/upstream-drag-hit-bubbles.md`。
///
/// **不画激活态**。左栏底部那个分段控件已经写明当前停在哪一页，标题栏再标一次
/// 是重复；而 windui 的 `bg_role` 是构建期定死的，要让它跟着信号变得多叠一个节点——
/// 为一份冗余信息付这个代价不值。
fn bar_entry(text: &str, on_click: impl FnMut(&mut EventCtx) + 'static) -> Element {
    Element::row()
        .cross(Align::Center)
        .padding_xy(11, 0)
        .height_match()
        .clickable()
        .on_click(on_click)
        .child(
            Element::label(text.to_string())
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        )
}

/// 标题栏左侧的应用标识：图标 + 名称 + 能力副标题。
fn brand() -> Element {
    Element::row()
        .cross(Align::Center)
        .spacing(9)
        .padding_xy(14, 0)
        .child(
            // 应用标识。**与托盘、任务栏是同一份产物**（`scripts/gen-icon.ps1`），
            // 三处必然一致。
            //
            // 此前是「`Role::Accent` 圆角块 + 一个 12px 的『词』字」，靠容器
            // `cross(Center)` + `text_align(Center)` 凑居中——而实机上它明显偏左上：
            // 汉字的字面框比 GDI/DirectWrite 的行盒窄且不对称，两个方向的居中都是按
            // **行盒**算的，字面自然落不到正中。这类偏移调不出来，因为可调的量
            // （字号、padding）都不是它的成因。
            //
            // 换成图片就没有「让字居中」这个问题了——居中在设计期由绘制脚本一次性
            // 解决，运行期只是贴一张图。
            crate::icon::app(20),
        )
        .child(
            Element::label(crate::APP_TITLE)
                .font_size(12.5)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(
            // 「英汉 · 汉英」说的是**查询方向**，不是两个词典——术语表禁止的是
            // 「汉英词典」这个组合，方向名本身合法（见 `domain::Direction`）。
            // 此处紧跟应用名，读作能力描述。
            //
            // 用 `text_muted` 而非设计稿此处的 `--faint`：faint 对三套皮肤的标题栏
            // 底都只有 ~2:1 对比度（远低于 AA 的 4.5），而这行是界面上**唯一**告知
            // 用户「中英两个方向都收」的地方——ADR-0003 拿掉了方向选择器，信息只剩
            // 这一处。装饰性文字可以淡，承载唯一信息的文字不可以。muted 也仍未达
            // 4.5（3.2/3.2/5.2），但那是设计稿整体的色阶问题，不该在这里独自解决。
            Element::label("英汉 · 汉英")
                .font_size(12.0)
                .fg_role(Role::TextMuted),
        )
}

/// 主体区域：两栏词典页与设置页，外加两个零尺寸驱动器与一条通栏消息条。
///
/// 布局是左右两栏——左边输入与列表，右边页签与释义，一如 macOS 自带的词典。上一版
/// 是「主列 + 按需抽屉」，抽屉在此撤掉：两栏之外再挂一个 280px 的抽屉，在 720px 的
/// 最小窗口里会挤成三栏；而历史、收藏与补全候选本就同形（都是一列词头），合进左栏
/// 那一个列表比另开一栏更省地方，也更好找。
///
/// DESIGN.md 的「Search is home / Recall is a drawer」并没有因此作废：它反对的是把
/// 历史当主导航，而这里的主导航仍是左栏顶上那个查询框——历史只是它下面那个列表在
/// 没有候选时的默认内容。
fn body(st: Rc<State>, unavailable: Option<String>) -> Element {
    let (p1, p2) = (st.page, st.page);
    let mut root = Element::col()
        .fill()
        // 三个驱动器都提到这一层，且排在**所有消费者之前**——`on_update` 按
        // `Element::build` 的前序（即书写顺序）派发，排在消费者之后就慢一帧。
        //
        // 它们**彼此之间的顺序也是承重的**，因为后一个消费前一个刚写下的信号：
        //
        // 1. `Completer` 产出候选、并把左栏拨到候选页；
        // 2. `LeftPaneLoader` 据此铺出左栏的行；
        // 3. `CardFilter` 据 `LeftPaneLoader` 刷星标时写下的 `cards` 派生出可见卡片。
        //
        // 补全驱动器此前挂在左栏内部（`left_pane`），排在 `LeftPaneLoader` **之后**，
        // 于是候选与页签的更新总要等下一帧才被铺成行。症状很具体：打完字**立刻**按 ↓，
        // 游标作用在上一批行上——左栏看着是候选，实际还是历史，于是按一下 ↓ 查出来的
        // 是历史里的某个词。实机上打字与按键之间通常隔着好几帧，所以不容易撞见，但
        // 「打完就按」恰恰是熟练用户的常态。
        .child(completer(st.clone()))
        .child(left_loader(st.clone()))
        .child(card_filter(st.clone()))
        // 回执 → Toast。与上面三个不同，它不产出任何信号，故不参与那条顺序链。
        .child(
            Element::leaf()
                .reactive()
                .widget(ToastSink {
                    note: st.settings_note,
                    tone: st.settings_note_tone,
                    last: st.settings_note.version(),
                })
                .size(0, 0),
        );
    // 不可用时才占这一行：正常情况下不该为一个不会发生的故障留白。
    if let Some(why) = unavailable {
        root = root.child(
            Element::row()
                .width_match()
                .padding_xy(14, 10)
                .child(unavailable_bar(&why)),
        );
    }
    root
        // 词典页与设置页叠在一起，按 `page` 切换。
        //
        // 用叠层而非替换，是为了保住词典页的状态——查询词、候选、结果卡片、滚动位置
        // 都在元素树里，若切页时把整棵子树换掉，从设置页回来会发现结果没了。
        //
        // 设置页盖住的是**整个**两栏区，不只右栏：它自带页头与分组卡片，是一屏独立的
        // 内容；只盖右栏会让左边留着一列与设置毫不相干的词，读起来像是「这些设置属于
        // 那个词」。
        .child(
            Element::stack()
                // 不写 `.fill()`：高度分量会立刻被 `weight` 覆盖（`weight` 在竖向父
                // 容器里落到高度维），写了等于留一句死代码。
                .width_match()
                .weight(1.0)
                .child(
                    dict_page(st.clone())
                        .fill()
                        .visible_when(move || p1.get() == PAGE_DICT),
                )
                .child(
                    settings_page(st.clone())
                        .fill()
                        .visible_when(move || p2.get() == PAGE_SETTINGS),
                ),
        )
        // 消息条通栏摆在最底下。它此前在主列的查询框下方，那时只有一列，摆哪都一样；
        // 两栏之后不行了——消息的来源横跨两栏（左栏删记录、右栏点收藏），摆进任一栏
        // 都会出现「在这边操作、消息却在那边」。
        .child(notice_row(st.notice))
}

/// 通栏消息条。空串时整行连同内边距一起收起——`visible_when` 让 `measure` 直接返回
/// `Size::ZERO`（windui `core.rs` 的 measure 开头就短路），是真的不占位，不是画成透明。
fn notice_row(notice: Signal<String>) -> Element {
    Element::row()
        .width_match()
        .padding_xy(14, 8)
        .border_role(Role::Divider, 1)
        .border_edges(Edges::TOP)
        .visible_when(move || !notice.get().is_empty())
        .child(notice_bar(notice, None))
}

/// 补全驱动器：零尺寸、不可见，监视查询词、产出候选。位置约束见 `body`。
fn completer(st: Rc<State>) -> Element {
    Element::leaf()
        .reactive()
        .widget(Completer {
            cursor: st.cursor,
            left_tab: st.left_tab,
            dict: st.dict.clone(),
            query: st.query,
            candidates: st.candidates,
            last_version: st.query.version(),
        })
        .size(0, 0)
}

/// 左栏驱动器：零尺寸、不可见，重算左栏行并刷新结果区星标。位置约束见 `body`。
fn left_loader(st: Rc<State>) -> Element {
    Element::leaf()
        .reactive()
        .widget(LeftPaneLoader {
            last: LeftInputs::of(&st),
            st,
        })
        .size(0, 0)
}

/// 卡片筛选驱动器：零尺寸、不可见，按右栏页签派生 `visible_cards`。位置约束见 `body`。
fn card_filter(st: Rc<State>) -> Element {
    Element::leaf()
        .reactive()
        .widget(CardFilter {
            last_tab: st.dict_tab.version(),
            last_cards: st.cards.version(),
            st,
        })
        .size(0, 0)
}

/// 词典页：左栏（输入 + 列表）+ 可拖的分隔条 + 右栏（页签 + 释义）。
fn dict_page(st: Rc<State>) -> Element {
    let pane_st = st.clone();
    Element::row()
        .fill()
        .cross(Align::Stretch)
        // **只把左栏包进重建作用域**，右栏留在外面。改栏宽本该只改变宽度，而右栏那边
        // 装着用户正读到一半的释义——滚动位置、将来的文本选区都活在元素树里，跟着重建
        // 一次就没了。右栏的宽度靠 `weight` 自己跟上，不需要重建。
        .child(Element::host_signal(st.pane_rev, move |_rev: u64| {
            left_pane(pane_st.clone())
        }))
        .child(splitter(st.clone()))
        .child(right_pane(st).weight(1.0))
}

/// 两栏之间那条可拖动的分隔条。行为与取舍见 `PaneSplitter`。
fn splitter(st: Rc<State>) -> Element {
    Element::leaf()
        .width(SPLITTER_W)
        .height_match()
        .widget(PaneSplitter {
            st,
            drag: None,
            hover: false,
        })
}

/// 竖向容器里的滚动区：**高度靠 `weight` 拿，不能用 `.fill()`**。
///
/// 这不是风格偏好，是 windui 的测量规则决定的。竖向父容器里，子节点主轴（高度）上的
/// `Match` 会被降级成 `Wrap`（`core.rs:measure_linear` 的「主轴上的 Match 降级为 Wrap」），
/// 随后按 `at_most(整条主轴)` 解析——注意是**整条**，不是扣掉固定高兄弟之后的剩余。
/// 于是内容超长的滚动区拿到的视口高等于整个父容器高，arrange 时从兄弟下方排起，
/// 视口底边落到父容器**之外**，超出的那截被裁掉。
///
/// 症状是「滚到底还差一截看不见」，且差值恰好等于上方兄弟占的高度——设置页差一个
/// 56px 页头，结果区差 26px（20px 提示行 + 6px 间距）。滚动条也够不到底，因为
/// `max_scroll = content_h - 视口高` 里的视口高多算了那一截。
///
/// `weight` 走的是第二遍按剩余空间瓜分的路径（`MeasureSpec::exactly(portion)`），
/// 视口高才等于真实可见高。`body` 里那句「不写 `.fill()`」说的是同一件事。
fn scroll_area(child: Element) -> Element {
    Element::scroll().width_match().weight(1.0).child(child)
}

/// 左栏：查询框 + 三段页签 + 一份列表。
fn left_pane(st: Rc<State>) -> Element {
    // 构建期读一次。宽度在 windui 里是构建期量，改了要重建——这正是 `pane_rev` 存在
    // 的理由，见 `dict_page`。
    let w = st.settings.borrow().left_pane_w;
    Element::col()
        .width(w)
        .height_match()
        // 左栏底比正文底暗一档，两栏的分界不全靠那条竖线撑着。三套皮肤的 `surface_alt`
        // 都与 `bg` 拉开了这一档，用角色表达之后换肤自动跟随。
        //
        // 右边框已撤：那条线现在由分隔条自己画（`PaneSplitter::paint`），它要能随悬停
        // 加粗变色，画在这里就跟不了手。
        .bg_role(Role::SurfaceAlt)
        // 右侧只留 2px：那一边紧挨着分隔条，10px 的对称内边距会在列表与分隔条之间
        // 撑出一道明显的空沟。左侧仍要 10——它是正文与窗口边缘的距离。
        .padding_edges(Insets::new(10, 12, 2, 12))
        .spacing(9)
        // 补全驱动器**不在这里**——它提到了 `body` 顶层，必须先于 `LeftPaneLoader`
        // 跑，理由见那里。挂在左栏内部会让候选慢一帧铺出来。
        //
        // 占位符不用「单词」「搜索」（均为术语表弃用词），但必须保住「中英皆可」这条
        // 信息：查询方向由查询词自动判定，界面上没有方向选择器（ADR-0003），用户无从
        // 知道这个框两种文字都收。
        //
        // 设计稿此处的查询框右侧有一组 `Ctrl` `K` 键帽，未照做：本项目的唤起热键是
        // Ctrl+Alt+D（`main.rs`），而窗口内并没有 Ctrl+K 这个键位。画一组按了没用的
        // 键帽比不画更糟。
        .child(query_box(st.clone()))
        .child(left_note_line(st.left_note))
        .child(left_list(st.clone()))
        // 分段控件摆在**列表下方**，不是查询框与列表之间。
        //
        // 理由是 Tab 顺序：焦点从查询框出来该直接落到列表上——那是用户按 Tab 时想去
        // 的地方（「移到列表进行操作」）。夹在中间时它必然先吃掉一次 Tab，而它切的是
        // 列表的**数据来源**，属于「先决定看什么，再在里面走」——真要用键盘切来源，
        // 从列表再 Tab 一下就到，顺序反而更顺。
        //
        // 底部横条也是侧栏筛选器的常见位置（Finder 的路径栏、邮件客户端的过滤条都在
        // 这一档），视觉上不会被读成「列表的标题」。
        //
        // 用分段控件而非页签：两者都表达单选，但右栏顶上已经有一排真页签了，同屏两排
        // 页签会让人以为它们是同一套导航的两级。分段的语义是「同一个列表的几种来源」，
        // 正是这里的实情——切过去人还在左栏。
        // 第一段叫「查询」而不是「候选」：这一段列的是当前输入能查到的词，用户读它
        // 时想的是「我要查哪个」。「候选」是**补全**这个动作的内部说法（术语表里它是
        // 一条正式术语），摆到界面上会让人以为那是另一种东西。
        .child(Element::segmented(vec!["查询", "历史", "收藏"], st.left_tab).width_match())
}

/// 左栏列表的空态文案。列表非空时整行收起。
///
/// 摆在列表**上方**而非下方：列表拿 `weight` 占尽剩余高度，即便一行都没有也仍是那么
/// 高，文案摆在它下面会被压到栏底——离用户刚敲字的地方隔着一整片空白，读起来不像是
/// 在回答刚才那次输入。
fn left_note_line(note: Signal<String>) -> Element {
    Element::label_signal(note)
        .font_size(12.5)
        .fg_role(Role::TextMuted)
        .width_match()
        .padding_xy(12, 4)
        .visible_when(move || !note.get().is_empty())
}

/// 左栏列表。三个页签共用一份数据信号，由 `LeftPaneLoader` 按当前页签重算。
///
/// **不用 `Element::list_signal`**，尽管它看起来正是为这件事准备的。它内部是
/// `Self::scroll()` 之后再 `set_widget(DynList)`——而 `Element::scroll()` 的滚动条
/// 拖动逻辑就住在它默认挂的那个 `ScrollWidget` 里，被 `DynList` 顶掉之后，滚动条
/// **看得见、抓不住**：画得出来（绘制只看 `content_h > 视口高`），按下去却没有任何
/// 东西处理这次拖动。滚轮不受影响（它走 `Tree::scroll_target`，只认 `Layout::Scroll`，
/// 与 widget 无关），所以症状是「滚轮能滚、拖不动」这种半瘫。
///
/// 改成 `scroll_area(host_signal(…))`：外层是完整的 `Element::scroll()`（`ScrollWidget`
/// 原封不动），里层 `host_signal` 是普通 col 挂 `DynList`，只管按信号重建行。两个职责
/// 各归各位。结果区（`result_area`）一直是这么写的，那边的滚动条从来没坏过。
///
/// 高度靠 `weight` 拿、不能用 `.fill()`——理由见 `scroll_area`。
fn left_list(st: Rc<State>) -> Element {
    let nav = ListKeyNav {
        last_cursor: st.cursor.version(),
        last_focus_req: st.focus_list.version(),
        st: st.clone(),
    };
    // 外面这层 col 只为挂 `ListKeyNav`：焦点与方向键归它，滚动仍归里面那个
    // `scroll_area` 自带的 `ScrollWidget`。两个职责各归各位，理由见两者的注释。
    Element::col()
        .width_match()
        .weight(1.0)
        .reactive()
        .widget(nav)
        .child(
            scroll_area(
                Element::host_signal(st.left_rows, move |r: LeftRow| left_row(r, st.clone()))
                    .width_match(),
            )
            // **只在右侧**留出滚动条的地盘。滚动条画在滚动容器的全矩形边缘，而内容排
            // 在 padding 内——不留这一档，候选行的释义摘要就被压在滚动条底下（一列
            // 半透明的灰条盖着字，正是它最该被读到的位置）。
            //
            // 用非对称 padding 而非 `padding_xy`：后者会让左边跟着白缩 10px，而左边
            // 并没有滚动条。
            .padding_edges(Insets::new(0, 0, 10, 0)),
        )
}

/// 左栏的一行：按类型分派。
///
/// 两种行**同高**（36），故切页签时列表不跳。这不是巧合而是约束：三段共用一个列表，
/// 高度不一致会让「切一下页签」看起来像「列表整个换了个东西」。
fn left_row(r: LeftRow, st: Rc<State>) -> Element {
    match r {
        LeftRow::Candidate {
            cand,
            at_cursor,
            active,
        } => candidate_row(cand, at_cursor, active, st),
        LeftRow::Recall {
            headword,
            at_cursor,
            active,
        } => recall_row(headword, at_cursor, active, st),
    }
}

/// 左栏的一条召回记录：词头（点击即查）+ 移除按钮。
///
/// 不用 `Element::nav_row`：它自带的 `›` 是「钻入子页」的语义，而这里点一行的动作是
/// 「查这个词」，查完人还在原地。图标与语义不符会让人误以为侧边还有一层。
///
/// `active`（「你正在看的是这条」）由调用方**在数据里**给出，不在这里现读——理由见
/// `LeftRow`。
fn recall_row(hw: Headword, at_cursor: bool, active: bool, st: Rc<State>) -> Element {
    let word = hw.as_str().to_string();
    let (pick_st, del_st) = (st.clone(), st);
    let (pick_word, del_hw) = (word.clone(), hw);
    let mut row = Element::row()
        .width_match()
        .height(36)
        .cross(Align::Center)
        .corner(9.0)
        .padding_xy(12, 0)
        .spacing(10)
        .clickable()
        // **退出 Tab 环**。行是 `Clickable`，默认可聚焦——不摘掉的话 Tab 会逐行走过
        // 四十条候选，而整块列表只该占**一个**焦点位（roving tabindex，见
        // `ListKeyNav`）：进来一次，之后交给 ↑↓。
        //
        // 鼠标点击不受影响：可聚焦与可点击是两件事。
        .focusable(false)
        .on_click(move |_ctx| {
            // 游标跟着鼠标走，理由同 `State::pick_candidate`：不挪的话，点完一行再按
            // 回车会跳去查另一个词。
            pick_st.focus_row(&pick_word);
        });
    // 两档淡底，与候选行同一套：满档是「你正在看的是这条」，半档是「回车会选中
    // 这一条」。设计稿的圆点与淡底不是纯装饰，它回答了「我刚点的是哪个」这个问题，
    // 尤其在历史列表里滚动之后。
    if active {
        row = row.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
    } else if at_cursor {
        row = row.bg_role_alpha(Role::Accent, CURSOR_SOFT_A);
    }
    row.child(
        // 圆点：6px，选中时用强调色。
        Element::leaf().size(6, 6).corner(3.0).bg_role(if active {
            Role::Accent
        } else {
            Role::TextDisabled
        }),
    )
    .child(
        Element::label(word)
            .font_size(14.0)
            .font_weight(if active { 600 } else { 500 })
            .fg_role(if active { Role::Text } else { Role::TextMuted })
            .weight(1.0),
    )
    .child(
        crate::icon::button(crate::icon::CLOSE, 24, Role::TextDisabled)
            .on_click(move |_ctx| del_st.remove_recall_row(&del_hw)),
    )
}

/// 右栏：方向页签 + 释义。
fn right_pane(st: Rc<State>) -> Element {
    Element::col()
        .fill()
        // 顶上只有页签。前进/后退挪到了标题栏——`TabBar` 的高度是 `Match`，与任何
        // 东西并排或叠放都会掉进交叉轴的坑：Match 在交叉轴上意味着「撑满父容器」，
        // 而父容器高度又随内容，两边互相等，最后解析成一大截，页签直接掉到半屏高的
        // 位置、结果区被挤没。它**只有独占竖向容器的一行**时才是对的——那时 Match
        // 落在主轴上被降级为 Wrap（`core.rs` 的「主轴上的 Match 降级为 Wrap」），
        // 高度才是那条标签条本身。
        .child(dict_tab_bar(st.clone()))
        .child(result_area(st).weight(1.0))
}

/// 前进 / 后退两枚按钮。落点在标题栏左上角，理由见 [`title_bar`] 里那段注释
/// （曾经打算摆在右栏方向页签旁边，`TabBar` 的交叉轴高度失控让那条路走不通）。
///
/// 包在 `host_signal` 里：两枚按钮的可用与否是构建期算的（图标颜色尤其——它在构建期
/// 就解析成了具体色值，见 `crate::icon`），走一步就得重建。重建量是两个节点。
fn nav_buttons(st: Rc<State>) -> Element {
    Element::host_signal(st.nav_rev, move |_rev: u64| {
        let (back, fwd) = (st.clone(), st.clone());
        Element::row()
            .cross(Align::Center)
            .spacing(2)
            .child(nav_button(
                crate::icon::BACK,
                st.can_go(false),
                move |_ctx| back.go_nav(false),
            ))
            .child(nav_button(
                crate::icon::FORWARD,
                st.can_go(true),
                move |_ctx| fwd.go_nav(true),
            ))
    })
    // **必须显式给高度**，否则两枚箭头会贴着标题栏顶边排，与旁边的「设置」不在一条
    // 视觉中线上。
    //
    // `host_signal` 的容器是 `col().fill()`，高度是 `Match`——在标题栏那个横向容器里
    // 那是**交叉轴**，Match 意味着撑满整条 38px；而按钮行在这个 col 内部的纵向**主轴**
    // 上是 Wrap，于是只占 26px 并贴顶。外层的 `cross(Center)` 对此无能为力：它要居中的
    // 那个子节点本身已经撑满了，居中等于没动。
    //
    // 给它按钮的自然高度，`cross(Center)` 才有东西可居中。这与 `right_pane` 那段
    // 「TabBar 不能与任何东西并排」是同一条规则的两次现形。
    .height(NAV_BTN)
}

/// 一枚导航按钮。走不动时置灰。
///
/// 置灰只改颜色、**不摘掉回调**：`go_nav` 自己有边界检查，点了什么也不会发生。少一条
/// 「两处都要判、漏一处就点出越界」的耦合。
fn nav_button(
    icon: &'static [u8],
    enabled: bool,
    on_click: impl FnMut(&mut EventCtx) + 'static,
) -> Element {
    let role = if enabled {
        Role::TextMuted
    } else {
        Role::TextDisabled
    };
    crate::icon::button(icon, NAV_BTN, role).on_click(on_click)
}

/// 右栏顶部那排方向页签。
///
/// 直接搭一条 `TabBar`，而不用 `Element::tabs`：后者会把**每一页**都建进树、只给未
/// 选中的挂 `visible_when`，而 `on_update` 的派发只看 `enabled` 不看 `visible`——
/// 三个页面就是三份结果区跟着重建，每次查询都是三倍的节点。撤掉召回抽屉时刚修掉的
/// 正是同一处浪费。
///
/// 而这排页签切的本来也不是「三个页面」，是同一个结果区的三种筛法（见 `DICT_ALL`
/// 一族常量）——一份内容区才是它的实情，`TabBar` 只负责那条标签条。
///
/// 条高与贯穿基线由 `TabBar` 自己按主题决定，故此处不设固定高、也不另加分隔线。
fn dict_tab_bar(st: Rc<State>) -> Element {
    Element::host_signal(st.tabs_rev, move |_rev: u64| {
        let items: Vec<TabItem> = st
            .tabs
            .borrow()
            .iter()
            // 本次没命中的置灰而不是摘掉：留在原位，位置就是稳定的，用户「总是点
            // 第三个」这条肌肉记忆才成立；摘掉则每查一个词后面所有页签整体左移，
            // 看着像换了一条标签条。
            .map(|t| TabItem::new(t.label.clone()).enabled(t.on))
            .collect();
        Element::leaf()
            .widget(TabBar::new(items, st.dict_tab))
            .width_match()
    })
}

/// 设置页。
///
/// 分组与卡片式行照设计稿，但**项目按本项目的能力来**：设计稿的「发音」「例句翻译」
/// 两组没有对应数据源，不画；换来的是设计稿没有的热键、开机自启、词库路径——那才是
/// 一个常驻词典真正要让用户调的东西。
fn settings_page(st: Rc<State>) -> Element {
    let back_st = st.clone();
    Element::col()
        .fill()
        .child(
            // 页头：返回 + 标题。
            Element::row()
                .width_match()
                .height(56)
                .cross(Align::Center)
                .padding_xy(22, 0)
                .spacing(12)
                .border_role(Role::Divider, 1)
                .border_edges(Edges::BOTTOM)
                .child(
                    crate::icon::button(crate::icon::BACK, 32, Role::Text)
                        .on_click(move |_ctx| back_st.leave_settings()),
                )
                .child(
                    Element::label("设置")
                        .font_size(22.0)
                        .font_family(SERIF)
                        .fg_role(Role::Text),
                ),
        )
        .child(scroll_area(Element::host_signal(
            st.settings_rev,
            move |_rev: u64| settings_body(st.clone()),
        )))
}

/// 设置页正文。每次 `settings_rev` 变动整体重建，故其中的构建时求值（选中环、
/// 词库路径）总是新鲜的。
fn settings_body(st: Rc<State>) -> Element {
    Element::col()
        .width_match()
        // 内容居中。此前主列左边有 224px 的侧栏顶着，620 的限宽靠左也还平衡；召回移到
        // 右侧抽屉之后主列宽了 200 多，再靠左就是一边贴边、一边空一大片。
        .cross(Align::Center)
        .child(
            Element::col()
                .width_match()
                .max_width(620)
                .padding_xy(40, 26)
                .spacing(28)
                // 回执不再占正文里的一行：它现在走顶部的 Toast（见 `ToastSink`）。
                // 两处都报等于同一句话说两遍，而设置页那条还会把它下面的所有分组
                // 往下顶一截——一条三秒后就该消失的消息，不该让页面跳一下。
                .child(group("外观", appearance_group(st.clone())))
                .child(group("布局", pane_w_row(st.clone())))
                .child(group("唤起", hotkey_row(st.clone())))
                .child(group("启动", autostart_row(st.clone())))
                .child(group("词库", dict_rows(st.clone())))
                .child(group("自带词典", user_dict_rows(st.clone())))
                .child(group("释义显示", expand_en_row(st.clone())))
                .child(group("数据", data_rows(st)))
                // 快捷键一览沉到底部：它是**说明书**，不是设置——读一次就记住了，
                // 而上面每一项都是要反复来改的。七行键位摆在第四组，等于让每个来改
                // 词库目录的人先滚过一屏自己早就知道的东西。
                .child(group("快捷键", shortcut_rows()))
                .child(group("关于", about_rows())),
        )
}

/// 数据管理：条数如实展示 + 清空历史。
///
/// **只清历史、不清收藏**：历史是系统被动记录的事实，攒多了是噪音，批量清理是正当
/// 需求；收藏是用户一条条主动标记的意图，批量抹掉的破坏性完全不同量级，且没有对应
/// 的存储层接口——真要做也该是另一套更谨慎的交互，不是并排放一个同样的按钮。
fn data_rows(st: Rc<State>) -> Element {
    let (hist, favs) = st.counts();
    let confirming = st.confirm_clear.get();
    let st2 = st.clone();
    let clear_btn = if confirming {
        Element::button("确认清空")
            .on_click(move |_ctx| {
                st2.clear_history();
                st2.confirm_clear.set(false);
                st2.bump_settings();
            })
            .fg_role(Role::Danger)
    } else {
        Element::button("清空").on_click(move |_ctx| {
            st2.confirm_clear.set(true);
            st2.bump_settings();
        })
    };
    let sub = if confirming {
        "此操作不可撤销。再点一次确认，或切走本页取消".to_string()
    } else {
        format!("已记录 {hist} 条")
    };
    card(vec![
        row("历史记录", Some(&sub), clear_btn),
        row(
            "收藏",
            Some(&format!("已收藏 {favs} 条")),
            // 指路的文案必须指得到，否则不如不写。这句话先后指过「侧栏」和「抽屉」，
            // 两者在界面上都已经没有对应物；收藏现在是左栏那个分段控件的第三段。
            Element::label("在「收藏」里逐条取消")
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        ),
    ])
}

/// 托盘图标的悬停提示。
///
/// **建托盘时与改热键时共用这一处**：两边各写一句 `format!` 的话，用户改一次热键，
/// 提示的措辞就会跟着变一次——而那两句本该是同一句话的不同时刻。
pub fn tray_tip(hotkey: &crate::settings::HotkeySpec) -> String {
    format!("{} — {hotkey} 查询", crate::APP_TITLE)
}

/// 项目主页。关于页与包内 README 都指这里。
const REPO_URL: &str = "https://github.com/huanfeng/wind-dict";

/// 关于。
///
/// 版本号取 `CARGO_PKG_VERSION`——与 exe 的文件属性（`build.rs`）、发布包的包名
/// （`scripts/release.ps1`）同一个来源。抄一份常量在这里就等于允许「界面说 0.1.0、
/// 文件属性说 0.2.0」，而那种不一致没有任何报错，只会让用户报的 bug 对不上版本。
fn about_rows() -> Element {
    card(vec![
        row(
            crate::APP_TITLE,
            Some(&format!("版本 {}", env!("CARGO_PKG_VERSION"))),
            Element::col(),
        ),
        row(
            "项目主页",
            Some(REPO_URL),
            Element::button("打开").on_click(|_| open_url(REPO_URL)),
        ),
        row(
            "许可",
            Some("程序代码 MIT OR Apache-2.0；三份词库各有其上游与协议，见随程序分发的 THIRD-PARTY.md"),
            Element::col(),
        ),
    ])
}

/// 用默认浏览器打开一个网址。
///
/// 走 `explorer` 而不是 `cmd /C start`：后者会闪一下控制台窗口（本程序是 GUI 子系统，
/// 平时根本没有控制台），而 explorer 是 GUI 程序，把未知协议交回 shell 的效果一样。
/// 与 `reveal` 同一条路子。
///
/// 只喂常量（[`REPO_URL`]）。这个函数**不接受**任何来自词典内容或用户输入的字符串——
/// 自带词典的正文里就有链接，把它们直接递给 shell 是另一回事，要先有白名单。
fn open_url(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

/// 快捷键一览。**只读**：这一栏是说明书，不是改键的地方。
///
/// 不做成可改的：全局唤起热键值得让用户改（它要和别的软件抢一个组合），而窗口内的
/// 快捷键没有这个冲突——它们只在本窗口有焦点时生效，改了反而丢掉「Ctrl+L 回到搜索框」
/// 这类跨软件通用的肌肉记忆。真有人要改，那是另一套（带冲突检测的）交互。
///
/// 数据取自 `SHORTCUTS`，那张表同时是这套键位的说明书，见它的注释。
fn shortcut_rows() -> Element {
    card(
        SHORTCUTS
            .iter()
            .map(|(keys, what)| {
                Element::row()
                    .width_match()
                    .cross(Align::Center)
                    .padding_xy(14, 11)
                    .spacing(12)
                    .child(
                        Element::label(*what)
                            .font_size(13.5)
                            .fg_role(Role::Text)
                            .weight(1.0),
                    )
                    .child(
                        // 键位用淡底小胶囊，与释义里的词性标记同一套视觉语言——都是
                        // 「一小段需要与正文区分开的记号」。
                        Element::label(*keys)
                            .font_size(12.5)
                            .font_weight(500)
                            .fg_role(Role::TextMuted)
                            .bg_role(Role::SurfaceAlt)
                            .corner(6.0)
                            .padding_xy(9, 4),
                    )
            })
            .collect(),
    )
}

/// 布局设置：左栏宽度的当前值与重置。
///
/// 这里**只给重置，不给数值输入**。栏宽的正确值是「看着舒服」，那件事拖一下分隔条
/// 当场就能判断，而填一个数字要来回试；此处真正需要的是一条退路——拖坏了、或换了
/// 台屏幕之后回到一个已知可用的值。
fn pane_w_row(st: Rc<State>) -> Element {
    let w = st.settings.borrow().left_pane_w;
    let reset_st = st.clone();
    card(vec![row(
        "左栏宽度",
        Some(&format!(
            "当前 {w}px。拖两栏之间的分隔线即可调整，默认 {}px",
            crate::settings::LEFT_PANE_W_DEFAULT
        )),
        Element::button("重置").on_click(move |_ctx| {
            reset_st.reset_left_w();
            // 本行显示的是构建期取的数值，不重建就还停在旧值上——与皮肤卡片的选中环
            // 是同一类问题，见 `State::settings_rev`。
            reset_st.bump_settings();
        }),
    )])
}

/// 一个设置分组：小标题 + 内容卡片。
fn group(title: &str, body: Element) -> Element {
    Element::col()
        .width_match()
        .spacing(12)
        .child(
            Element::label(title)
                .font_size(12.0)
                .font_weight(700)
                .fg_role(Role::TextMuted),
        )
        .child(body)
}

/// 卡片：设置行的容器。
fn card(rows: Vec<Element>) -> Element {
    let mut col = Element::col()
        .width_match()
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0);
    for (i, r) in rows.into_iter().enumerate() {
        // 行间分隔线交给上边框，末行不画——比在行之间插色块少一层节点。
        let r = if i == 0 {
            r
        } else {
            r.border_role(Role::Divider, 1).border_edges(Edges::TOP)
        };
        col = col.child(r);
    }
    col
}

/// 一行设置：左标题（可带副标题）+ 右控件。
fn row(title: &str, subtitle: Option<&str>, control: Element) -> Element {
    let mut left = Element::col().spacing(2).weight(1.0).child(
        Element::label(title)
            .font_size(14.0)
            .font_weight(500)
            .fg_role(Role::Text)
            .width_match(),
    );
    if let Some(sub) = subtitle {
        left = left.child(
            Element::label(sub)
                .font_size(12.0)
                .fg_role(Role::TextMuted)
                .width_match(),
        );
    }
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(18, 14)
        .spacing(12)
        .child(left)
        .child(control)
}

/// 外观分组：风格卡片 + 亮暗分段。
///
/// 两个控件而非一排六张卡片。六张卡片要求用户在「哪一族颜色」和「亮还是暗」两件事上
/// 同时做决定，而它们本就是正交的——拆成两个控件之后，换个明暗不必重新挑一遍风格。
/// 六张卡片还表达不了「跟随系统」：那不是第七套配色，是把明暗这一维交给系统。
fn appearance_group(st: Rc<State>) -> Element {
    Element::col()
        .width_match()
        .child(mode_row(st.clone()))
        .child(Element::divider())
        .child(
            Element::col()
                .width_match()
                .padding_xy(18, 14)
                .child(style_cards(st)),
        )
}

/// 亮 / 暗 / 跟随系统。
///
/// 排在风格卡片**之前**：它决定卡片上的预览色块画成什么样（`SkinStyle::swatch` 收
/// `dark`），先选明暗再挑风格，读起来才是因果顺序。
fn mode_row(st: Rc<State>) -> Element {
    let cur = st.settings.borrow().mode;
    let sel = signal(SkinMode::ALL.iter().position(|&m| m == cur).unwrap_or(0));
    let seg_st = st.clone();
    Element::row()
        .width_match()
        .cross(Align::Center)
        .padding_xy(18, 14)
        .spacing(12)
        .child(
            Element::col()
                .weight(1.0)
                .child(
                    Element::label("明暗")
                        .font_size(14.0)
                        .fg_role(Role::Text)
                        .width_match(),
                )
                .child(
                    Element::label(if cur == SkinMode::System {
                        "跟随系统的「应用模式」，改了当场跟上"
                    } else {
                        "不随系统变化"
                    })
                    .font_size(12.0)
                    .fg_role(Role::TextMuted)
                    .width_match(),
                ),
        )
        .child(
            Element::leaf()
                .reactive()
                .widget(ModeWatcher {
                    st: seg_st,
                    sel,
                    last: sel.version(),
                })
                .size(0, 0),
        )
        .child(
            Element::segmented(
                SkinMode::ALL.iter().map(|m| m.label()).collect::<Vec<_>>(),
                sel,
            )
            .width(220),
        )
}

/// 把分段控件的选择回写成设置。
///
/// 与 `SettingToggle` 同构：`Element::segmented` 只认信号，而设置住在 `RefCell` 里；
/// 中间要一个驱动器把信号的变化搬过去。不能在构建期读一次了事——那时用户还没点。
struct ModeWatcher {
    st: Rc<State>,
    sel: Signal<usize>,
    last: u64,
}

impl Widget for ModeWatcher {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.sel.version();
        if v == self.last {
            return;
        }
        self.last = v;
        let Some(&m) = SkinMode::ALL.get(self.sel.get()) else {
            return;
        };
        if self.st.settings.borrow().mode != m {
            self.st.set_mode(m);
        }
    }
}

/// 风格卡片三选一。预览色块按**当前明暗档**画，点下去得到的就是看到的那套。
fn style_cards(st: Rc<State>) -> Element {
    let dark = st.dark();
    let mut row = Element::row()
        .width_match()
        .spacing(14)
        .cross(Align::Stretch);
    for kind in SkinStyle::ALL {
        let st = st.clone();
        let current = st.settings.borrow().style == kind;
        let sw = kind.swatch(dark);
        row = row.child(
            Element::col()
                .weight(1.0)
                .bg_role(Role::Surface)
                .border_role(if current { Role::Accent } else { Role::Border }, 2)
                .corner(12.0)
                .padding(14)
                .spacing(4)
                .clickable()
                .on_click(move |_ctx| st.set_style(kind))
                .child(
                    // 三色预览块。
                    // 每块都描边：浅色皮肤的底色块与卡片底同为近白，不描边就看不见，
                    // 三色预览只剩两色。
                    Element::row()
                        .spacing(5)
                        .height(18)
                        .child(swatch_chip(sw[0]))
                        .child(swatch_chip(sw[1]))
                        .child(swatch_chip(sw[2])),
                )
                .child(
                    Element::label(kind.name())
                        .font_size(14.0)
                        .font_weight(600)
                        .fg_role(Role::Text),
                )
                .child(
                    Element::label(kind.desc(dark))
                        .font_size(12.0)
                        .fg_role(Role::TextMuted),
                ),
        );
    }
    row
}

/// 皮肤预览色块。
fn swatch_chip(c: Color) -> Element {
    Element::leaf()
        .size(22, 22)
        .corner(6.0)
        .bg(c)
        .border_role(Role::Border, 1)
}

/// 唤起热键：三个修饰键复选框 + 主键下拉，改完当场重注册。
///
/// 这里曾长期是**只读展示**，因为改键要框架支持运行期重注册；`HotkeyHandle::rebind`
/// 补上之后才活过来，改动落在 [`HotkeyEditor`] 里。
fn hotkey_row(st: Rc<State>) -> Element {
    let spec = st.settings.borrow().hotkey;
    let ctrl = signal(spec.ctrl);
    let alt = signal(spec.alt);
    let shift = signal(spec.shift);
    let key = signal(spec.key.index());
    let key_options: Vec<String> = crate::settings::HotkeyKey::all()
        .into_iter()
        .map(|k| k.to_string())
        .collect();
    let editor = Element::leaf()
        .reactive()
        .widget(HotkeyEditor {
            st,
            ctrl,
            alt,
            shift,
            key,
            last: (
                ctrl.version(),
                alt.version(),
                shift.version(),
                key.version(),
            ),
        })
        .size(0, 0);
    card(vec![row(
        "唤起热键",
        Some("全局生效，改完立即生效。F1–F12 可单独使用；字母与数字须配修饰键"),
        Element::row()
            .cross(Align::Center)
            .spacing(10)
            // 编辑器：零尺寸、不可见，须先于它监视的控件注册。
            .child(editor)
            .child(Element::checkbox("Ctrl", ctrl))
            .child(Element::checkbox("Alt", alt))
            .child(Element::checkbox("Shift", shift))
            .child(
                // 主键用下拉而非文本框。
                //
                // 文本框收的是**字符**，而 F1–F12 根本不产生字符：用户按 F1 时框里毫无
                // 反应（`TextInput` 只把 Up/Down/Tab/PageUp/PageDown 转给 `on_nav_key`，
                // `Key::Other` 到不了应用），只能手打「F1」两个字母——一个没人猜得到的
                // 用法。做成按键捕获则要自绘一个控件（框架没有现成的），代价远超收益。
                //
                // 下拉框把这件事变回选择题：能选的都合法，选不到的都不合法，也没有
                // 「填错字」这条错误路径。
                Element::dropdown(key_options, key)
                    .width(96)
                    .font_size(13.0),
            ),
    )])
}

/// 开机自启。
fn autostart_row(st: Rc<State>) -> Element {
    // 初值取**注册表**而非库：用户可能在别处禁掉了自启。
    toggle_row(
        st,
        "开机时启动",
        "登录后自动在托盘常驻，等待热键唤起",
        |st| st.autostart_now(),
        |st, v| st.set_autostart(v),
    )
}

/// 释义显示。
fn expand_en_row(st: Rc<State>) -> Element {
    toggle_row(
        st,
        "默认展开英英释义",
        "关闭时英英释义仍在，只是需要点开",
        |st| st.settings.borrow().expand_en,
        |st, v| st.set_expand_en(v),
    )
}

/// 一行开关设置：初值由 `init` 取，翻转由 `apply` 落实（失败则自动拨回）。
fn toggle_row(
    st: Rc<State>,
    title: &str,
    subtitle: &str,
    init: fn(&State) -> bool,
    apply: fn(&State, bool) -> bool,
) -> Element {
    let on = signal(init(&st));
    card(vec![row(
        title,
        Some(subtitle),
        Element::row()
            .cross(Align::Center)
            // 监听器：零尺寸、不可见，须先于开关注册（on_update 按注册顺序广播）。
            .child(
                Element::leaf()
                    .reactive()
                    .widget(SettingToggle {
                        st,
                        on,
                        last_version: on.version(),
                        apply,
                    })
                    .size(0, 0),
            )
            .child(Element::switch(on)),
    )])
}

/// 词库目录：随程序分发的三份库住在哪。
///
/// **一行，不是三行**。三份库永远住在一起、随同一次部署整体替换；给它们各配一个
/// 文件选择器，等于把一种从不发生的状态（英汉指新版、汉英指旧版）摆到用户面前。
/// 字形库此前更是**连设置项都没有**，靠「在英汉库旁边找」这条看不见的约定定位——
/// 同一件事两套规则，其中一套还是隐式的。
fn dict_rows(st: Rc<State>) -> Element {
    use crate::source::offline;
    let configured = st.settings.borrow().dict_dir.clone();
    // 显示的是**此刻真正在用的**那个目录，不是设置里写着的那个。二者可能不同：
    // 设置里的目录失效时 `main` 会回退到程序同目录（否则用户会被锁在启动失败里）。
    // 只显示设置值的话，用户会对着一个正确的路径纳闷为什么查的还是旧库。
    let in_use = match &configured {
        Some(d) if offline::check_dir(d).usable() => d.clone(),
        _ => offline::exe_dir(),
    };
    let fell_back = configured.is_some() && configured.as_ref() != Some(&in_use);

    let status = offline::check_dir(&in_use);
    let sub = if fell_back {
        format!("设置的目录当前不可用，正在用：{}", in_use.display())
    } else {
        in_use.display().to_string()
    };

    let mut right = Element::row().cross(Align::Center).spacing(8);
    // 有自定义目录时才给「恢复默认」——本来就是默认值时这个按钮点了没意义。缺了它，
    // 用户一旦选错就再也回不到默认，只能去手改数据库。
    if configured.is_some() {
        let st2 = st.clone();
        right = right.child(Element::button("恢复默认").on_click(move |_| st2.set_dict_dir(None)));
    }
    let d = in_use.clone();
    right = right.child(Element::button("打开").on_click(move |_| reveal(&d)));
    let st3 = st.clone();
    right = right.child(Element::button("更改…").on_click(move |ctx| {
        let st = st3.clone();
        ctx.request_pick_folder(
            PickDialog::new().title("选择词库目录"),
            move |picked| {
                if let Some(p) = picked {
                    st.set_dict_dir(Some(p));
                }
            },
        );
    }));

    // 两份词库各占一行而不是合成「已装词库」一行：它们现在各有一个页签、各有一个
    // 可改的名字，那一行里塞不下，也说不清哪个大小对应哪个名字。
    card(vec![
        row("词库目录", Some(&sub), right),
        builtin_dict_row(
            st.clone(),
            offline::ECDICT_KEY,
            offline::ECDICT_NAME,
            "英汉",
            offline::ECDICT_FILE,
            &status.ecdict,
        ),
        builtin_dict_row(
            st,
            offline::CEDICT_KEY,
            offline::CEDICT_NAME,
            "汉英",
            offline::CEDICT_FILE,
            &status.cedict,
        ),
        // 字形库**没有改名框**：它不出词条，也就没有页签，名字无处可显示。多一个
        // 改了看不见效果的输入框只会让人以为坏了。
        row("字形库", Some(&glyph_line(&status)), Element::col()),
    ])
}

/// 一份内置词库的设置行：当前名字 + 方向/文件/大小 + 改名框。
fn builtin_dict_row(
    st: Rc<State>,
    key: &'static str,
    default_name: &str,
    dir_label: &str,
    file: &str,
    stat: &Result<u64, String>,
) -> Element {
    let sub = match stat {
        Ok(n) => format!("{dir_label} · {file} · {}", mb(*n)),
        // 打不开必须说出原因：否则用户只看到一行名字，无从判断是文件没了还是坏了。
        Err(e) => format!("{dir_label} · {file} · 打不开：{e}"),
    };
    dict_name_row(st, key, default_name, &sub, Element::col())
}

/// 字形库那一行的说明。它可缺，故说法与另外两份不同：不在只是少一行部首笔画，
/// 不是故障。
fn glyph_line(st: &crate::source::offline::DirStatus) -> String {
    use crate::source::offline::UNIHAN_FILE;
    match st.unihan {
        Some(n) => format!("部首、笔画、繁简 · {UNIHAN_FILE} · {}", mb(n)),
        None => format!("无 {UNIHAN_FILE}（不显示部首笔画）"),
    }
}

/// 词库大小。
///
/// 报**文件大小**而不是词条数：`SELECT count(*)` 在这几个库上实测各要 170–210 ms，
/// 而设置页每次换肤、每次拨开关都要重建，加起来半秒的卡顿换不来什么——用户分辨不出
/// 770,611 与 700,000 的差别，却一眼看得出 169 MB 与 0 字节的差别。
fn mb(n: u64) -> String {
    format!("{:.0} MB", n as f64 / 1_048_576.0)
}

/// 一行「某本词典」：当前显示名 + 说明 + 改名框（+ 右侧附加控件）。
///
/// 输入框里**留空表示用默认名**，占位符就是那个默认名——而不是把默认名填进去。
/// 填进去的话，「我没改过」与「我把它改成了跟默认一样」在存储里就分不开，用户也
/// 没有一个明确的「恢复默认」动作可做（清空即恢复，见 `Settings::set_dict_name`）。
fn dict_name_row(
    st: Rc<State>,
    key: &str,
    default_name: &str,
    sub: &str,
    extra: Element,
) -> Element {
    let (custom, title) = {
        let s = st.settings.borrow();
        (s.dict_name(key, ""), s.dict_name(key, default_name))
    };
    let text = signal(custom);
    Element::row().width_match().cross(Align::Center).child(row(
        &title,
        Some(sub),
        Element::row()
            .cross(Align::Center)
            .spacing(8)
            // 监听器须先于输入框注册：`on_update` 按注册顺序广播。
            .child(
                Element::leaf()
                    .reactive()
                    .widget(DictNameSaver {
                        st,
                        key: key.to_string(),
                        text,
                        last_version: text.version(),
                    })
                    .size(0, 0),
            )
            .child(
                Element::text_input(text, default_name)
                    .font_size(13.0)
                    .width(150),
            )
            .child(extra),
    ))
}

/// 打开一个目录（资源管理器）。
///
/// 设置页给这个入口，是因为「把词典放进去」这件事只能在文件管理器里做——而让用户
/// 照着一行灰字自己去粘贴路径，是把程序知道的事推给用户重做一遍。
fn reveal(dir: &std::path::Path) {
    // 目录可能还不存在（用户从没放过词典）。先建出来再打开，否则资源管理器会弹一个
    // 「找不到」——而那正是用户此刻最需要的那个目录。
    let _ = std::fs::create_dir_all(dir);
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
}

/// 千位分隔。词条数动辄七位（实测一本 3,402,564），不分节根本读不出量级。
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 自带词典：一行目录 + 扫到的每本一行开关。
///
/// 没有「添加」按钮，这是刻意的：添加一本词典 = 把文件放进这个目录。设置页能做的
/// 只有换目录和开关某一本——凡是用户已经在文件管理器里做过的事，不该再让他来这里
/// 向程序汇报一遍。
fn user_dict_rows(st: Rc<State>) -> Element {
    let dir = st.user_dict_dir();
    let custom = st.settings.borrow().user_dict_dir.is_some();
    let disabled = st.settings.borrow().disabled_dicts.clone();

    let shown = match &dir {
        Some(d) => d.display().to_string(),
        None => "无法确定（LOCALAPPDATA 未设置）".to_string(),
    };
    let mut right = Element::row().cross(Align::Center).spacing(8);
    // 只在用过自定义目录时才给「恢复默认」：本来就是默认值时这个按钮点了没意义。
    if custom {
        let st2 = st.clone();
        right =
            right.child(Element::button("恢复默认").on_click(move |_| st2.set_user_dict_dir(None)));
    }
    // 刷新：目录才是唯一的真相，而它随时可能在程序外面被改动（往里拖一本、删一本）。
    // 没有这个按钮时，用户唯一能想到的办法是重启程序——而「换个目录」和「改个开关」
    // 都会顺带重扫，唯独「我刚往里放了一本」没有对应的动作，这是个说不通的空缺。
    let st4 = st.clone();
    right = right.child(Element::button("刷新").on_click(move |_| {
        st4.reload_user_dicts();
        let n = st4.user_dicts.borrow().len();
        st4.note_ok(format!("已重新扫描词典目录，可用 {n} 本"));
        // 重建设置页：那几行词典名与词条数是构建期扫出来的，不重建就还停在旧的一批上
        // ——而用户点刷新，要的正是看见新的那一批。
        st4.bump_settings();
    }));
    if let Some(d) = dir.clone() {
        right = right.child(Element::button("打开").on_click(move |_| reveal(&d)));
    }
    let st3 = st.clone();
    right = right.child(Element::button("更改…").on_click(move |ctx| {
        let st = st3.clone();
        ctx.request_pick_folder(
            PickDialog::new().title("选择词典目录"),
            move |picked| {
                if let Some(p) = picked {
                    st.set_user_dict_dir(Some(p));
                }
            },
        );
    }));

    let mut rows = vec![row("词典目录", Some(&shown), right)];

    let files = dir
        .as_deref()
        .map(crate::source::user::scan)
        .unwrap_or_default();
    if files.is_empty() {
        rows.push(row(
            "还没有词典",
            Some("把 .mdx 文件放进上面这个目录（可以带子目录），回到这里就能看到"),
            Element::col(),
        ));
        return card(rows);
    }

    // 逐本打开一次，为的是拿到词典名与词条数——**关掉的也开**：不开就只能显示一个
    // 文件名，而用户此刻要判断的正是「这本是不是我想开的那本」。一本约 3 ms，
    // 且只在设置页打开时才走这一遭。
    for p in &files {
        let file = crate::source::user::key_of(p);
        let (title, sub) = match crate::source::user::probe(p) {
            Ok(d) => (
                d.name().to_string(),
                format!("{} 词条 · {file}", thousands(d.entry_count())),
            ),
            // 打不开必须**说出原因**：否则用户只看到一个拨不动的开关，无从判断是
            // 文件坏了，还是我们不支持它用的压缩方式。
            Err(e) => (file.clone(), format!("打不开：{e:#}")),
        };
        let on = signal(!disabled.contains(&file));
        let toggle = Element::row()
            .cross(Align::Center)
            // 监听器须先于开关注册（on_update 按注册顺序广播），且必须盯信号而非
            // 挂 `on_toggle`——后者被 windui 静默吞掉，见 `SettingToggle`。
            .child(
                Element::leaf()
                    .reactive()
                    .widget(DictToggle {
                        st: st.clone(),
                        file: file.clone(),
                        on,
                        last_version: on.version(),
                    })
                    .size(0, 0),
            )
            .child(Element::switch(on));
        // 键是文件名，与页签、开关用的是同一个（见 `source::user::key_of`）。
        rows.push(dict_name_row(st.clone(), &file, &title, &sub, toggle));
    }
    card(rows)
}

/// 监视某一本自带词典的开关。
///
/// 与 `SettingToggle` 分开而不是把它泛化：那个持有的是 `fn` 指针（无捕获），而这里
/// 必须带上「是哪一本」。为一处需要闭包的用法把另一处改成 `Box<dyn FnMut>`，是让
/// 已经好用的代码替新代码付账。
struct DictToggle {
    st: Rc<State>,
    file: String,
    on: Signal<bool>,
    last_version: u64,
}

impl Widget for DictToggle {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let v = self.on.version();
        if v == self.last_version {
            return;
        }
        self.last_version = v;
        let want = self.on.get();
        if !self.st.toggle_user_dict(&self.file, want) {
            self.on.set(!want);
            // 回拨自己也会推高版本，须同步记下，否则下一帧会把回拨当成新的用户操作，
            // 来回翻转停不下来。
            self.last_version = self.on.version();
        }
    }
}

/// 需当场告知的消息条。空串时零高度、不占位。
///
/// 与 `unavailable_bar` 分开：那条讲的是启动时就已知的**持续状态**，这条讲的是
/// 刚刚那一次操作的**结果**。混在一起会让用户分不清「一直不能用」和「这次没成」。
///
/// `tone` 为 `None` 时恒按失败着色——词典页那条消息条只报失败（收藏写不进、
/// 删除失败），没有成功回执要报，给它配一个恒为 `Danger` 的信号纯属多余。
///
/// 曾经为了「成功绿、失败红」在这里叠过两个绑同一份文本的 label，各带一个可见性判定，
/// 外层容器还要再带一个（否则空消息时它仍占父容器一份 spacing）。上游补上
/// `fg_role_signal` 之后那三层一起收掉了——颜色现在跟着信号走，一个 label 就够。
fn notice_bar(notice: Signal<String>, tone: Option<Signal<Role>>) -> Element {
    let mut line = Element::label_signal(notice)
        .font_size(13.0)
        .width_match()
        .visible_when(move || !notice.get().is_empty());
    line = match tone {
        Some(t) => line.fg_role_signal(t),
        None => line.fg_role(Role::Danger),
    };
    line
}

/// 用户数据不可用时的警示条。
///
/// 宁可占一行位置也要摆在明面上：静默失效会让用户以为记录发生了，而实际什么都
/// 没写入。附上原因是因为只说「不可用」等于把排查责任丢给用户。
///
/// **只反映启动时的状态**：`UserDataState` 在 `build` 时求值一次即固定。若数据库
/// 在运行期才变得不可用（磁盘满、文件被删），本条不会出现——历史按
/// `State::record_all` 的静默策略处理；而收藏一旦有入口，其写入失败必须**当场**
/// 告知，不能指望这条。
///
/// 文案暂只提历史记录：收藏尚无界面入口，提一个用户还够不着的能力只会让人困惑。
///
/// **警示性由边框承载，文字用正文色**。琥珀色文字（此前的 `0xB54708`）压在淡黄底上
/// 只有 5.2:1 且底色写死——深色皮肤下会变成黑底里一整块近白卡片。改用同色系淡底的
/// `Intent::badge_colors` 也不行：它拿同一个色既当底又当字，三套皮肤实测只有
/// 2.9–3.9:1，够不到 AA 的 4.5。
///
/// 故把「严重性」与「可读性」拆到两个通道：文字走 `Text` 拿满对比度，严重性交给
/// `Danger` 边框。这同时满足 WCAG 1.4.1——彩色文字对色觉障碍用户等同普通文字，
/// 而边框加「不可用」这三个字是冗余编码，不依赖颜色也能读懂。
fn unavailable_bar(why: &str) -> Element {
    Element::label(format!("历史记录不可用：{why}"))
        .font_size(13.0)
        .fg_role(Role::Text)
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Danger, 1)
        .corner(6.0)
        .padding(8)
        .width_match()
}

/// 查询框 + 右端的清除按钮。
///
/// 按钮**叠在输入框上**而不是并排：并排会把输入框挤窄，且按钮出现/消失时输入框宽度
/// 跟着变，光标位置跳一下。叠放则输入框始终定宽。
///
/// `Layout::Frame` 用单个 `align` 同时定横纵（windui `core.rs:arrange_frame`），故
/// 按钮做成与输入框**等高**（`QUERY_H`）的行，`Align::End` 在纵向偏移为零，横向贴右
/// ——「右端居中」就是这么凑出来的，不是框架直接支持的对齐方式。两处高度必须一起改，
/// 故抽成常量：写死成两个 50 时，改一处另一处就悄悄错位了。
fn query_box(st: Rc<State>) -> Element {
    let query = st.query;
    let (submit_st, nav_st) = (st.clone(), st.clone());
    Element::stack()
        .width_match()
        .child(
            Element::text_input(st.query, "输入中文或英文…")
                .width_match()
                .height(QUERY_H)
                .corner(10.0)
                .font_size(15.0)
                // 唤起后焦点落在这里，并全选旧内容——常驻词典最高频的动作是「唤起 →
                // 查另一个词」，而窗口只是被隐藏、上次的词原样还在框里。全选让下一个词
                // 直接覆盖打上去，不用先删。
                //
                // **每次唤起都重新兑现**：托盘点击 / 全局热键 / `WindowOp::Show` 造成的
                // 隐藏→可见跃迁，上游都会重新 arm 一次（windui `on_window_shown`），
                // 应用侧不需要挂 `on_show`。
                //
                // 这里此前写着「只在进程起来后第一次唤起兑现，第二次唤起焦点还停在原处」
                // ——那条限制上游已经解掉了（`675d6d5`），注释一并作废。
                .autofocus_select_all()
                .on_submit(move |_ctx| submit_st.submit())
                // 焦点留在查询框时的键位。**Tab 一律放过**——它是所有 Windows 程序里
                // 「把焦点交出去」的那个键，占着它，用户就没有任何办法用键盘走进左栏
                // 列表。补全因此改绑 →，见 `State::accept_completion`。
                .on_nav_key(move |_ctx, ev| match ev.key {
                    Key::Down => {
                        nav_st.move_cursor(true);
                        true
                    }
                    Key::Up => {
                        nav_st.move_cursor(false);
                        true
                    }
                    // 带 Ctrl 的方向键一律放行，交给窗口级快捷键（`handle_shortcut`
                    // 里的前进/后退）。**必须排在下面那条裸 → 之前**——match 自上而下，
                    // 裸 → 那条不看修饰键，排在前面会把 Ctrl+→ 一并吃掉；而在这里
                    // 「吃掉」意味着 `on_nav_key` 返回 true，窗口级那层就再也收不到它。
                    Key::Left | Key::Right if ev.ctrl => false,
                    // 只在补全确实会改变输入时吞掉 →，否则放行让光标正常移动。
                    // 这个判据是个能自我恢复的近似，理由见 `should_accept_completion`。
                    Key::Right if nav_st.should_accept_completion() => {
                        nav_st.accept_completion();
                        true
                    }
                    _ => false,
                }),
        )
        .child(
            Element::row()
                .height(QUERY_H)
                .cross(Align::Center)
                .padding_xy(10, 0)
                .align(Align::End)
                .child(
                    crate::icon::button(crate::icon::CLOSE, 26, Role::TextDisabled)
                        // **退出 Tab 环**。它是查询框的附属物，不是一站独立的目的地：
                        // 用户按 Tab 是想离开输入框去列表，中间横着一个「清空」既挡路、
                        // 又危险——焦点停在它上面时按空格/回车就把刚打的词清了。
                        //
                        // 鼠标照常可点，清空也仍有键盘路径（选中全部再删）。这不是把
                        // 功能拿掉，是把它从**键盘导航的主路**上挪开。
                        .focusable(false)
                        .on_click(move |_ctx| st.clear_query()),
                )
                // 空框上放一个「清空」按钮没有意义，且它会盖住占位符的尾部。
                .visible_when(move || !query.get().is_empty()),
        )
}

/// 左栏的一条补全候选：词头 + 释义摘要。
///
/// 不用 `Element::nav_row`——它是「带 chevron 的钻入行」（windui `ui/nav.rs`），而 `›`
/// 的语义是「进到下一层去」。点候选并不进入任何子页，只是把词填进查询框并查询，人还在
/// 原地。这与 `recall_row` 那里拒绝 `nav_row` 是同一条理由。
fn candidate_row(c: Candidate, at_cursor: bool, active: bool, st: Rc<State>) -> Element {
    let word = c.headword.as_str().to_string();
    let pick = word.clone();
    let mut row = Element::row()
        .width_match()
        // 与召回行同高，理由见 `left_row`。
        .height(36)
        .cross(Align::Center)
        .corner(9.0)
        .padding_xy(12, 0)
        .spacing(10)
        .clickable()
        // **退出 Tab 环**。行是 `Clickable`，默认可聚焦——不摘掉的话 Tab 会逐行走过
        // 四十条候选，而整块列表只该占**一个**焦点位（roving tabindex，见
        // `ListKeyNav`）：进来一次，之后交给 ↑↓。
        //
        // 鼠标点击不受影响：可聚焦与可点击是两件事。
        .focusable(false)
        .on_click(move |_ctx| {
            // 选中候选 = 查询词确定 → 此刻才查询源出场。游标一并挪过来，见
            // `State::pick_candidate`。
            st.pick_candidate(&pick);
        });
    // 两档淡底，回答两个不同的问题；同时成立时取满档（「正在看」压过「将要选」）。
    // 取值与层次的理由见 `CURSOR_SOFT_A`。
    if active {
        row = row.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
    } else if at_cursor {
        row = row.bg_role_alpha(Role::Accent, CURSOR_SOFT_A);
    }
    row = row.child(
        Element::label(word)
            // 加粗只给 `active`：它标的是「正在看这条」，与召回行一致。游标那条靠淡底
            // 就够——两处都加粗会让一列候选出现两个同样重的词，反而看不出主次。
            .font_size(14.0)
            .font_weight(if active { 600 } else { 500 })
            .fg_role(Role::Text)
            // 定宽让释义摘要对齐成一栏。词头长短不一（`make` 与 `makeshift` 差一倍），
            // 不定宽的话每行的摘要起点各不相同，一列候选读下来是锯齿状的——而候选列表
            // 的用法正是**竖着快速扫**，对不齐直接抵消它的价值。
            //
            // 从 120 收到 88：浮层年代这一行有整个主列的宽度，如今它在 280px 的左栏
            // 里，扣掉内边距与间距只剩 236——留 120 给词头，摘要就只剩百来像素，
            // 「一眼认出是不是我要的那个词」这件事做不成了。88 放得下六七个字母／三个
            // 汉字，覆盖绝大多数词头，超长的照常把摘要推开、不截断：认出这是不是我要
            // 的那个词，最终靠的是词头本身。
            .min_width(88),
    );
    // 释义摘要单行截断：它是判断「是不是我要的那个词」的依据，不是正文。让它换行会把
    // 行高撑开，一屏就列不下几条了——而候选列表的价值恰恰在于一眼扫过多条。
    if let Some(p) = c.preview {
        row = row.child(
            Element::label(p)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        );
    }
    row
}

/// 结果区：提示 + 页签空态 + 词头卡片。
///
/// 吃的是 `visible_cards` 而非 `cards`——前者是后者经右栏页签筛过的派生信号，见
/// `State::refilter_cards`。
/// 正文与栏边缘的距离。设计稿此处是 40px，那是在更宽的画布上。
///
/// **挂在滚动区内部那一层，不挂在外层容器上**。挂外层时整块滚动区跟着缩进 28px，
/// 滚动条于是悬在离窗口右边缘 28px 的地方——看着像浮在正文里的一条，而不是这一栏的
/// 边界，且滚动条与正文之间那道空沟怎么看都像画错了。设置页一直是对的
/// （`settings_body` 把 padding 放在滚动区**里面**），结果区是唯一的例外。
const RESULT_PAD_X: i32 = 28;

fn result_area(st: Rc<State>) -> Element {
    let (cards, fnote) = (st.visible_cards, st.filter_note);
    Element::col()
        .fill()
        .spacing(6)
        .child(
            Element::label_signal(st.hint)
                .fg_role(Role::TextMuted)
                .height(20)
                .width_match()
                .padding_edges(Insets::new(RESULT_PAD_X, 14, RESULT_PAD_X, 0)),
        )
        // 页签筛空时的说明，与 `hint` 各占一行、各说各的：`hint` 讲的是查询本身
        // （未收录、经原形命中），这一行讲的是「你现在停在哪个页签」。塞进同一个信号
        // 会让两种状态互相覆盖——而它们完全可能同时成立（查到了词、但停在空的方向页）。
        .child(
            Element::label_signal(fnote)
                .font_size(13.0)
                .fg_role(Role::TextMuted)
                .width_match()
                .padding_edges(Insets::new(RESULT_PAD_X, 0, RESULT_PAD_X, 0))
                .visible_when(move || !fnote.get().is_empty()),
        )
        .child(scroll_area(
            Element::host_signal(cards, move |c: Card| card_view(c, st.clone()))
                .width_match()
                .padding_edges(Insets::new(RESULT_PAD_X, 0, RESULT_PAD_X, 14)),
        ))
}

/// 一张词头卡片：大字词头 + 收藏星标 + 该词头下的全部词条。
fn card_view(c: Card, st: Rc<State>) -> Element {
    let hw = c.headword.clone();
    // 卡片之间留出一个身位，靠间距而非分隔线区分——多个词头时才看得出边界。
    let mut col = Element::col().spacing(10).width_match().padding_xy(0, 10);
    col = col.child(
        Element::row()
            // 改回居中对齐。此前是顶对齐，理由是「42px 的星标方块与 42px 的词头居中
            // 会掉到词头视觉重心之下」——词头收到 30、星标收到 32 之后这条不再成立，
            // 两者高度相当，居中就是对的。
            .cross(Align::Center)
            .width_match()
            .spacing(14)
            .child(
                // 词头也走富文本，为的是它能被选中复制——查完一个词顺手复制词头是
                // 常见动作，而它此前是唯一一处「看得见、选不中」的正文。
                //
                // 不与下方释义合成一篇：星标要与词头**同一行**并排，而富文本里放不进
                // 一个按钮。代价是选区不能从词头一路拖到释义，可以接受。
                Element::rich(headword_doc(&hw))
                    .copy_menu(true)
                    // 同 `selectable`：不进焦点环，Ctrl+C 就复制不了词头。
                    .focusable(true)
                    .weight(1.0),
            )
            .child(star(c.fav, hw.clone(), st.clone())),
    );
    // 词头区与释义区之间的分隔线，与设计稿一致。两者是不同层次的信息——上面回答
    // 「这是哪个词」，下面回答「它什么意思」，一条线比单纯拉开间距更能说明这件事。
    col = col.child(Element::divider());
    // 字形在释义之前：它回答「这是个什么字」，与上方词头同属「这是哪个词」那一层，
    // 排在释义之后就与阅读顺序拧着了。
    if let Some(g) = &c.glyph {
        col = col.child(glyph_row(g));
    }
    for (i, e) in c.entries.into_iter().enumerate() {
        // 只查表，不新建：本函数跑在重建作用域内，在这里 `signal()` 出来的句柄活不过
        // 下一次重建。信号由 `rebuild_cards` 预先备好，详见 `ExpandedStates`。
        let expanded = st
            .expanded
            .get(&expand_key(&hw, i), st.settings.borrow().expand_en);
        col = col.child(entry_view(e, expanded, st.clone()));
    }
    // 备注排在最后：它是用户**附加**给这个词的东西，不该插进词典自身的内容里打断
    // 「词头 → 音标 → 释义」这条阅读顺序。只在已收藏时出现——没收藏就没有可附着的
    // 书签。
    if c.fav {
        col = col.child(note_field(hw, st));
    }
    col
}

/// 字形一行：读音、部首、笔画、字级。
///
/// 画在**卡片**上而非词条里，理由见 [`Card::glyph`]：`行` 有三条词条却只有一副字形，
/// 放进 `chinese_doc` 就会重复三遍。数据模型里避开的重复，在渲染层重新引入等于白避。
///
/// 用普通标签与徽章而非富文本：它是元信息，不是释义。DESIGN.md 把「能选中」这条留给
/// 正文，而「部首 讠」单独被复制出来是没有上下文的碎片——同星级、考纲徽章的处置。
///
/// 读音用普通话调号（`yǔ`）而非 CC-CEDICT 的数字调（`yu3`），且是**字**的读音全集；
/// 每条词条各自的拼音仍归词条。二者不重复：一个答「这个字怎么念」，一个答
/// 「这条词条读哪个音」。
fn glyph_row(g: &Glyph) -> Element {
    // 组间距 16：读音与检字数据各自内部都用 `·` 分隔，靠太近时两组会连成一条长链，
    // 读的人分不出哪几个是读音。颜色差做了一半的活，间距补另一半。
    let mut row = Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(16);

    // 读音排最前且不弱化：整行里只有它是用户念得出来的东西，其余都是检字信息。
    if !g.readings.is_empty() {
        row = row.child(
            Element::label(g.readings.join(" · "))
                .font_size(13.5)
                .fg_role(Role::Text),
        );
    }

    let mut parts = vec![format!("部首 {}", g.radical)];
    // 部外笔画为负时略去。那 32 个字比自身部首还少一笔，Unihan 记负数是诚实的，
    // 但「部外 -1 画」没人看得懂，而总笔画已经把该说的说完了。
    if g.extra_strokes >= 0 {
        parts.push(format!("部外 {} 画", g.extra_strokes));
    }
    parts.push(format!("共 {} 画", g.total_strokes));
    row = row.child(
        Element::label(parts.join("  ·  "))
            .font_size(12.5)
            .fg_role(Role::TextMuted),
    );

    // 字级做成徽章而非行内文字，与英文词的考纲徽章同一套语言：它标的是「这个字有多
    // 常用」这一身份，与左边那串检字数据不是一类，混进同一串 `·` 会被读丢。
    // 一级字用强调色——3500 个常用字是这套分级里唯一对普通读者有意义的那档。
    if let Some(t) = g.tier {
        let first = t == CharTier::Level1;
        row = row.child(grading_chip(
            t.label(),
            if first { Role::Accent } else { Role::TextMuted },
            if first { 600 } else { 400 },
        ));
    }
    row
}

/// 词头：全屏最大的那几个字。
fn headword_doc(hw: &Headword) -> RichDoc {
    RichDoc::new()
        .style(
            SPAN_HEADWORD,
            SpanStyle::new()
                .size(HEADWORD_SIZE)
                .family(SERIF)
                .fg(RichColor::Text),
        )
        .para(Para::new().styled(SPAN_HEADWORD, hw.as_str().to_string()))
}

/// 收藏备注输入框。
fn note_field(hw: Headword, st: Rc<State>) -> Element {
    let text = signal(st.note_of(&hw));
    Element::row()
        .width_match()
        .cross(Align::Center)
        .spacing(8)
        // 保存器：零尺寸、不可见，须先于输入框注册（on_update 按注册顺序广播）。
        .child(
            Element::leaf()
                .reactive()
                .widget(NoteSaver {
                    st,
                    headword: hw,
                    text,
                    last_version: text.version(),
                })
                .size(0, 0),
        )
        .child(
            // 限宽：备注是一句自己写给自己的短话，不是正文。正文限宽撤掉之后它会跟着
            // 横贯整屏，一个 1800px 宽的输入框在暗示「这里该写很多字」——与它的用途
            // 相反。这是撤限宽时唯一需要单独兜住的控件。
            Element::text_input(text, "备注…")
                .font_size(13.0)
                .width_match()
                .max_width(520)
                .weight(1.0),
        )
}

/// 收藏星标。实心 = 已收藏。
fn star(fav: bool, hw: Headword, st: Rc<State>) -> Element {
    // 32×32，**未收藏时不画边框**。
    //
    // 此前是 42×42 且两态都描边，于是它在整屏上比词头还抢眼——一个淡蓝描边的圆角方块
    // 压在词头旁边，眼睛先看到的是按钮而不是词。收藏确实是这一屏唯一的写操作，需要
    // 一个明确的可点区域，但「明确」由尺寸（32px 已远超指尖/指针的命中要求）和位置
    // （词头行最右）负责，不必再叠一圈边框。
    //
    // 已收藏时才上淡底 + 强调色实心星：那时它承载状态，值得被看见；未收藏时它只是
    // 一个待命的动作，空心星足够。
    //
    // 星形走 SVG 而非 `★`/`☆` 字形：后者在 Windows 上被 emoji 字体接管，画出来带彩色
    // 描边且**在方块里明显偏左上**——那不是对齐没调好，是字面框与行盒对不上，调不出来。
    // 详见 `crate::icon`。
    // 用 `stack` 而非 `row`：线性容器只有交叉轴对齐（`cross`），没有主轴对齐，18px 的
    // 图标在 32px 的行里只能靠算 padding 顶到中间——那是把「居中」写成了一个减法，
    // 图标尺寸一改就错位。`Layout::Frame` 的 `align` 同时定横纵（windui
    // `core.rs:arrange_frame`），`Align::Center` 就是两轴都居中。
    let mut btn = Element::stack()
        .size(32, 32)
        .corner(9.0)
        .clickable()
        .on_click(move |_ctx| st.toggle_favorite(&hw));
    if fav {
        btn = btn.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
    }
    btn.child(
        crate::icon::view(
            if fav {
                crate::icon::STAR_FILLED
            } else {
                crate::icon::STAR
            },
            18,
            if fav {
                Role::Accent
            } else {
                Role::TextDisabled
            },
        )
        .align(Align::Center),
    )
}

/// 词条视图。
///
/// 两类词条**形状不同**（英汉有音标与词形变化，汉英有拼音、繁体与量词，且中文不屈折），
/// 故必须 match 两个分支——这正是 ADR-0009 拆分词条类型所买到的：编译器不允许
/// 「给中文词条读词形变化」这类无意义的访问。
///
/// 颜色一律走 `Role` 而非具体色值。此处原先写的 `0x2D3436` / `0x636E72` 恰好就是
/// windui `Palette::default()` 的 `text` / `text_muted`——它们从来不是选定的颜色，
/// 只是照默认主题抄下来的常量。皮肤成为变量的那一刻，这种抄写就从冗余变成了错误：
/// 深色皮肤下 `0x2D3436` 压在 `0x17191C` 上只有 1.39:1，等于看不见。
///
/// **不含词头**——词头由 `card_view` 统一呈现在卡片头部，此处重复一遍是噪音；
/// 多音字（一个词头、多条词条）尤其明显，会把同一个词连打好几遍。
fn entry_view(e: Entry, expanded: Signal<bool>, st: Rc<State>) -> Element {
    match e {
        Entry::English(x) => {
            let mut col = Element::col().spacing(8).width_match();
            // 音标与词性同一行：它们都是「这个词是什么」的元信息，与释义分属两层。
            if x.phonetic.is_some() || x.pos.is_some() {
                col = col.child(selectable(en_meta_doc(&x)));
            }
            // 分级徽章紧跟音标：两者都在回答「这是个什么词」，而释义回答的是
            // 「它什么意思」。放到词形变化之后会把这两段元信息劈开。
            //
            // **它与词形变化都留在富文本之外**，仍是 Element。两者是**徽章**不是正文：
            // 星级是重复的星形、考试标签是一组彩色小块，它们的意义在形状与颜色里，
            // 拖选出来只会得到一串没有上下文的短词（「牛津 3 CET4」）。可选中的是
            // 释义，不是装饰。
            if let Some(r) = grading_row(&x.grading) {
                col = col.child(r);
            }
            // 词形变化：made / making / makes 这些数据一直躺在库里，界面上却一个字
            // 都没有。ADR-0001 当初选 ECDICT，理由之一正是它自带 `exchange`——只用在
            // 查询路径（查 tried 跟随到 try）而不展示，等于这份数据只兑现了一半。
            if !x.inflections.derived.is_empty() {
                col = col.child(inflection_row(&x.inflections.derived));
            }
            // 中文释义整块是**一段富文本**，各词性各成一 Para。整块而非逐行，正是为了
            // 能从第一条释义一直拖选到最后一条——「复制这个词的全部释义」是查词典最
            // 常见的一个动作，逐行切成独立控件就永远只能一条条来。
            if let Some(zh) = &x.zh_definition {
                let glosses = crate::domain::parse_glosses(zh);
                if !glosses.is_empty() {
                    col = col.child(selectable(zh_def_doc(&glosses)));
                }
            }
            // 英英释义默认折叠，用户主动展开才可见——这是刻意的产品决定，非偷懒。
            //
            // 用 `Element::collapsible` 而不是 `RichDoc::section`：后者的信号语义是
            // **collapsed**（true = 收起），而本项目的 `ExpandedStates` 存的是
            // expanded（true = 展开）。同一个信号不可能两种读法，而翻转语义要动
            // `ExpandedStates` 及其那一串「展开态活过重建」的测试——为省一层嵌套去动
            // 那块，不划算。
            if let Some(en) = &x.en_definition {
                col = col.child(Element::collapsible(
                    "英英释义",
                    expanded,
                    // 这一段单独限宽，理由见 `EN_DEF_MAX_W`：整屏正文里只有它是成句的
                    // 英文散文，行长失控的风险是真的。
                    selectable(en_def_doc(en)).max_width(EN_DEF_MAX_W),
                ));
            }
            col
        }
        // 汉英词条整条就是一段富文本——它没有徽章类内容（CC-CEDICT 只有繁简、拼音、
        // 释义、量词四样，全是文字），故拼音到量词可以一气选到底。
        Entry::Chinese(x) => selectable(chinese_doc(&x)).width_match(),
        // 自带词典：出处一行 + 一整段富文本。
        //
        // 出处**必须画出来**，且在正文之上。随程序分发的两个库是我们挑过的，自带词典
        // 是用户放进来的、来源与质量我们一无所知；两者以相同样式并排出现时，用户没有
        // 任何线索区分。这与 ADR-0008 要求标注「由 AI 生成」是同一条理由的另一面。
        Entry::User(x) => Element::col()
            .spacing(6)
            .width_match()
            .child(
                Element::label(x.source.clone())
                    .font_size(12.0)
                    .fg_role(Role::TextMuted),
            )
            .child(
                selectable(user_doc(&x))
                    // 词典内部的交叉引用（「参见 X」）点了就跳过去——这是自带词典
                    // 唯一的导航手段，它的正文里没有别的可操作元素。
                    .on_span_click(move |_, id| st.select(id)),
            ),
    }
}

/// 自带词典正文里每级列表的缩进。
const USER_INDENT: i32 = 20;

/// 自带词典的正文：段落 + 行内粗体 / 斜体 / 跳转。
///
/// 这三个样式位就是 CSS 被剥掉之后**全部**幸存的语义（见 `crate::html`）。斜体尤其
/// 承重：例句、语体标注（*informal*）、拉丁学名都只剩它了。
fn user_doc(x: &UserEntry) -> RichDoc {
    let mut doc = RichDoc::new();
    for (i, b) in x.body.iter().enumerate() {
        let mut para = Para::new();
        for r in &b.runs {
            let mut style = SpanStyle::new().size(15.0);
            if r.bold {
                style = style.weight(700);
            }
            if r.italic {
                style = style.italic();
            }
            match &r.link {
                Some(target) => {
                    para = para.span_id(
                        target.clone(),
                        r.text.clone(),
                        style.fg(RichColor::Accent).underline(),
                    )
                }
                None => para = para.span(r.text.clone(), style),
            }
        }
        if b.indent > 0 {
            para = para.indent(i32::from(b.indent) * USER_INDENT);
        }
        if i > 0 {
            para = para.spacing_before(6);
        }
        doc = doc.para(para);
    }
    doc
}

/// 把一篇富文本装成可选中、可复制的控件。
///
/// 三处词条内容都经由这里，为的是「能选中」这件事不会漏在某一段上——那种漏法在界面上
/// 看不出来，只有用户去拖选时才发现某一段拖不动。
///
/// `copy_menu(true)` 开右键菜单（复制选区 / 全选）。仅靠 Ctrl+C 不够：这一屏没有别的
/// 右键菜单，用户在释义上点右键期待的就是复制。
fn selectable(doc: RichDoc) -> Element {
    Element::rich(doc)
        .copy_menu(true)
        // **强制进焦点环**，否则 Ctrl+C 到不了它。
        //
        // windui 的 `RichText::focusable()` 只在文档含可折叠 Section 时为真（纯静态
        // 文本不占焦点位，这个默认对绝大多数应用是对的）。而它的 `on_event` 里
        // Ctrl+C / Ctrl+A 的处理是齐全的——键盘事件只发给焦点节点，于是那段代码在
        // 词典正文上永远跑不到：能用鼠标划选、能用右键菜单复制，独独 Ctrl+C 没反应。
        //
        // 代价是 Tab 会经过右栏的每一段正文。可以接受：它们在左栏之后，不挡「输入框
        // → 列表」那条主路；而「Tab 到一段正文、Ctrl+C 复制它」本身是合理的键盘路径。
        .focusable(true)
        .width_match()
        .line_height(BODY_LH)
}

/// 英汉词条的元信息：音标 + 词性。
fn en_meta_doc(x: &crate::domain::EnglishEntry) -> RichDoc {
    let mut para = Para::new();
    if let Some(p) = &x.phonetic {
        para = para.styled(SPAN_PHONETIC, format!("[{p}]"));
    }
    // 词性用衬线 + 强调色。设计稿此处是斜体，而 windui 没有斜体 API
    // （`font_style`/`italic` 全无），故改由字族与颜色承载这份身份。
    if let Some(pos) = &x.pos {
        if x.phonetic.is_some() {
            para = para.text("  ");
        }
        para = para.span(
            pos.clone(),
            SpanStyle::new()
                .size(15.0)
                .family(SERIF)
                .fg(RichColor::Accent),
        );
    }
    RichDoc::new()
        .style(
            SPAN_PHONETIC,
            SpanStyle::new().size(15.0).fg(RichColor::Muted),
        )
        .para(para)
}

/// 英汉词条的中文释义：每个词性一段，悬挂缩进对齐。
fn zh_def_doc(glosses: &[crate::domain::Gloss]) -> RichDoc {
    let mut doc = RichDoc::new()
        // 词性胶囊。`chip()` 的底与圆角由 rich 主题给，故这里只定字号、字重与颜色——
        // 保留强调色是因为词性是查词典时的主要扫视目标（「我要的是动词那一条」）。
        .style(
            SPAN_POS,
            SpanStyle::new()
                .size(12.5)
                .weight(600)
                .fg(RichColor::Accent)
                .chip(),
        )
        .style(SPAN_BODY, SpanStyle::new().size(18.0).weight(500));
    for (i, g) in glosses.iter().enumerate() {
        let mut para = Para::new();
        if let Some(pos) = &g.pos {
            para = para.styled(SPAN_POS, pos.clone()).text(" ");
        }
        // 释义之间用顿号而非原文的逗号：中文并列用顿号，且与释义内部可能出现的
        // 逗号区分得开。
        para = para
            .styled(SPAN_BODY, g.senses.join("、"))
            .hanging(GLOSS_HANGING);
        // 段间距只加在第二段起：首段上面已经有容器的 spacing，再加一次就多出一截。
        if i > 0 {
            para = para.spacing_before(8);
        }
        doc = doc.para(para);
    }
    doc
}

/// 英英释义：一段成句的英文散文。
fn en_def_doc(en: &str) -> RichDoc {
    RichDoc::new()
        .style(SPAN_BODY, SpanStyle::new().size(15.0))
        .para(Para::new().styled(SPAN_BODY, en.to_string()))
}

/// 汉英词条的全部内容：拼音、繁体、义项、量词。
fn chinese_doc(x: &crate::domain::ChineseEntry) -> RichDoc {
    let mut doc = RichDoc::new()
        // 拼音是中文词条的「音标」，与英汉分支同一层级，故用同一档字号。
        .style(
            SPAN_PHONETIC,
            SpanStyle::new().size(15.0).fg(RichColor::Muted),
        )
        .style(SPAN_NOTE, SpanStyle::new().size(13.0).fg(RichColor::Muted))
        .style(SPAN_INDEX, SpanStyle::new().size(18.0).fg(RichColor::Muted))
        .style(SPAN_BODY, SpanStyle::new().size(18.0).weight(500))
        .para(Para::new().styled(SPAN_PHONETIC, format!("[{}]", x.pinyin)));
    // 繁体与词头不同才展示——相同时显示两遍是噪音。
    if x.traditional != x.headword.as_str() {
        doc = doc.para(
            Para::new()
                .styled(SPAN_NOTE, format!("繁体：{}", x.traditional))
                .spacing_before(6),
        );
    }
    // 英文释义按义项分段。`;` 分隔的是同一义项的不同措辞，不另起一段。
    //
    // 序号单独一个 span：它是界面加的编号、不是词典原文，用弱化色与释义拉开一档，
    // 拖选复制时也仍在文本里——那正是用户想要的（「1. May」比孤零零一个 May 好读）。
    for (i, s) in x.senses.iter().enumerate() {
        doc = doc.para(
            Para::new()
                .styled(SPAN_INDEX, format!("{}. ", i + 1))
                .styled(SPAN_BODY, join(s))
                .hanging(24)
                .spacing_before(if i == 0 { 10 } else { 6 }),
        );
    }
    if !x.classifiers.is_empty() {
        doc = doc.para(
            Para::new()
                .styled(SPAN_NOTE, format!("量词：{}", x.classifiers.join("、")))
                .spacing_before(8),
        );
    }
    doc
}

/// 词形变化一排。
fn inflection_row(derived: &[(crate::domain::InflectionKind, Headword)]) -> Element {
    let mut row = Element::row().width_match().cross(Align::Center).spacing(8);
    for (kind, hw) in derived {
        row = row.child(
            Element::row()
                .cross(Align::Center)
                .spacing(5)
                .bg_role(Role::SurfaceAlt)
                .corner(6.0)
                .padding_xy(9, 4)
                .child(
                    Element::label(kind.label())
                        .font_size(11.5)
                        .fg_role(Role::TextMuted),
                )
                .child(
                    Element::label(hw.to_string())
                        .font_size(13.0)
                        .font_weight(500)
                        .fg_role(Role::Text),
                ),
        );
    }
    row
}

/// 词汇分级一排：柯林斯星级、牛津核心词、考试大纲、词频排名。
///
/// 这些数据此前压根没进词库——构建期就被丢掉了（见 docs/adr/0010 的修订）。补回来
/// 是因为它们回答的是查词时的**第二个问题：这词要紧吗**。柯林斯星级与牛津核心说的是
/// 重要度，考试大纲说的是「什么阶段该会」，词频给的是客观排名。
///
/// 与 [`inflection_row`] 共用同一套底色、圆角与内边距：两者都是「挂在词条上的小标记」，
/// 同屏并列时必须看起来属于同一类东西，否则界面像拼了两套不相干的控件。
///
/// **一个徽章都没有时返回 `None`，判空与渲染因此是同一段代码。** 不能让调用方拿
/// [`Grading::is_empty`](crate::domain::Grading::is_empty) 当闸门：那是「有没有数据」，
/// 而这里问的是「画不画得出东西」，两者在 `bnc` 上就分家了——它存进了库却**刻意不
/// 渲染**（理由见下方 `frq` 那段）。只有 bnc 有值的词并不罕见（两个语料库覆盖面不同），
/// 而那样的词会通过数据闸门、拿到一个零子节点的 row，在词条上留下一道 8px 的空隙。
fn grading_row(g: &crate::domain::Grading) -> Option<Element> {
    let mut row = Element::row().width_match().cross(Align::Center).spacing(8);
    // 数子节点而不是重新判一遍字段：判据只有一处，下面加一个新徽章时不必记得回来改。
    let mut n_badges = 0;

    // 星级用实心星重复而非写数字：五颗星一眼可数，数字还要在心里换算一次。
    //
    // `min(5)` 不是多余的防御。词库可以被**外部文件整体替换**（设置页支持指定词库
    // 路径，且本项目的 schema 已对齐上游格式，第三方库都能装进来），那些库里的
    // collins 是否守在 1–5 内，不由我们说了算——不钳住就可能画出一屏的星。
    if let Some(n) = g.collins {
        n_badges += 1;
        row = row.child(
            Element::row()
                .cross(Align::Center)
                .spacing(5)
                .bg_role(Role::SurfaceAlt)
                .corner(6.0)
                .padding_xy(9, 4)
                .child(
                    Element::label("柯林斯")
                        .font_size(11.5)
                        .fg_role(Role::TextMuted),
                )
                .child(
                    Element::label("★".repeat(n.min(5) as usize))
                        .font_size(12.0)
                        .fg_role(Role::Accent),
                ),
        );
    }

    // 牛津三千是个是非题、没有等级，故只在为真时出现。用强调色与考试大纲拉开层级：
    // 它标的是「核心词汇」这一身份，比「考四级会考到」更根本。
    if g.oxford {
        n_badges += 1;
        row = row.child(grading_chip("牛津核心", Role::Accent, 600));
    }

    // 大纲标签已在领域层按学习阶段排好序（`ExamTag::rank`），此处照序渲染即可——
    // 库里存的是录入顺序，直接铺出来读起来没有递进感。
    for t in &g.tags {
        n_badges += 1;
        row = row.child(grading_chip(t.label(), Role::TextMuted, 500));
    }

    // 只显示当代语料库词频（`frq`），不并列 BNC：两个都画就是两串没有单位的数字并排，
    // 读者无从分辨谁是谁。BNC 已经存进库里（[`crate::domain::Grading::bnc`]），要用时
    // 随时可取——它统计的是数百年间的英文资料，读旧书时比 `frq` 更有参考价值。
    if let Some(f) = g.frq {
        n_badges += 1;
        row = row.child(
            Element::label(format!("词频 {f}"))
                .font_size(11.5)
                .fg_role(Role::TextDisabled),
        );
    }

    (n_badges > 0).then_some(row)
}

/// 分级徽章：淡底小标签。
fn grading_chip(text: &str, fg: Role, weight: u16) -> Element {
    Element::label(text)
        .font_size(11.5)
        .font_weight(weight)
        .fg_role(fg)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .padding_xy(9, 4)
}

/// 义项内的多种措辞用 `;` 连回去——它们是同一含义的不同说法，不是不同义项。
fn join(s: &Sense) -> String {
    s.glosses.join("; ")
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_left_w, entry_in_tab, expand_key, filter_cards, grading_row, group_by_headword,
        headwords_to_record, scroll_area, signal, write_note, Card, ExpandedStates, NavPath, Role,
        TabKey, RIGHT_MIN_W,
    };
    use crate::domain::{
        ChineseEntry, EnglishEntry, Entry, ExamTag, Grading, Headword, Inflections, Sense,
    };

    // ── 分级徽章 ──────────────────────────────────────────

    /// 只有 `bnc` 的词条**画不出任何徽章**，那一行整个不该建。
    ///
    /// `bnc` 存进了库却刻意不渲染（两串没有单位的数字并排读者分不清谁是谁），而
    /// `Grading::is_empty` 把它算作「有数据」。拿那个当闸门，只有 bnc 的词就会拿到一个
    /// 零子节点的 row，在词条上留下一道 8px 的空隙——ECDICT 里 bnc 有值而 frq 为 NULL
    /// 的词并不罕见，两个语料库的覆盖面本就不同。
    #[test]
    fn 只有词频语料库排名时不建徽章行() {
        let g = Grading {
            bnc: Some(8906),
            ..Default::default()
        };
        assert!(!g.is_empty(), "前提：按数据算它不是空的");
        assert!(
            grading_row(&g).is_none(),
            "但一个徽章都画不出，那一行不该建"
        );
        assert!(grading_row(&Grading::default()).is_none(), "全空同理");
    }

    /// 四种徽章各自单独出现时都要建出那一行——判空与渲染同源，漏一种就是少一行。
    #[test]
    fn 任一徽章有值就建徽章行() {
        for g in [
            Grading {
                collins: Some(5),
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
                frq: Some(524),
                ..Default::default()
            },
        ] {
            assert!(grading_row(&g).is_some(), "有徽章可画：{g:?}");
        }
    }

    fn 英汉(词头: &str) -> Entry {
        Entry::English(EnglishEntry {
            headword: Headword::from_store(词头),
            phonetic: None,
            zh_definition: None,
            en_definition: None,
            pos: None,
            inflections: Inflections::default(),
            grading: Grading::default(),
        })
    }

    fn 汉英(词头: &str, 拼音: &str) -> Entry {
        Entry::Chinese(ChineseEntry {
            headword: Headword::from_store(词头),
            traditional: String::new(),
            pinyin: 拼音.into(),
            senses: vec![Sense {
                glosses: vec!["x".into()],
            }],
            classifiers: Vec::new(),
        })
    }

    fn 词头们(entries: &[Entry]) -> Vec<String> {
        headwords_to_record(entries)
            .iter()
            .map(|h| h.as_str().to_string())
            .collect()
    }

    #[test]
    fn 一无所获不记录任何词头() {
        assert!(headwords_to_record(&[]).is_empty());
    }

    #[test]
    fn 单个词条记其词头() {
        assert_eq!(词头们(&[英汉("try")]), vec!["try"]);
    }

    /// 繁体查询词可命中简体列不同的多行（`餘` → `余` / `馀`），必须**全部**记录。
    /// 只记第一条会让历史与用户所见的词条列表对不上，且「第一条」仅是建库插入顺序。
    #[test]
    fn 多个不同词头全部记录且保序() {
        let e = [汉英("余", "yu2"), 汉英("馀", "Yu2"), 汉英("馀", "yu2")];
        assert_eq!(词头们(&e), vec!["余", "馀"]);
    }

    /// 多音字返回多条词条但词头相同（`行` 的 hang2 / xing2），只该记一次。
    #[test]
    fn 同一词头去重() {
        let e = [汉英("行", "hang2"), 汉英("行", "xing2")];
        assert_eq!(词头们(&e), vec!["行"]);
    }

    fn 分组形状(entries: &[Entry]) -> Vec<(String, usize)> {
        group_by_headword(entries)
            .into_iter()
            .map(|(hw, g)| (hw.as_str().to_string(), g.len()))
            .collect()
    }

    /// 多音字合成**一**张卡片（一个词头 = 一个星标），两条词条都在组内。
    #[test]
    fn 多音字归入同一张卡片() {
        let e = [汉英("行", "hang2"), 汉英("行", "xing2")];
        assert_eq!(分组形状(&e), vec![("行".into(), 2)]);
    }

    /// 不同词头各成一张卡片，才能分别收藏（`餘` → `余` / `馀`）。
    #[test]
    fn 不同词头各成一张卡片() {
        let e = [汉英("余", "yu2"), 汉英("馀", "yu2")];
        assert_eq!(分组形状(&e), vec![("余".into(), 1), ("馀".into(), 1)]);
    }

    /// 组的顺序须与 `headwords_to_record` 一致，否则界面呈现顺序会与历史记录顺序漂移。
    #[test]
    fn 分组顺序与记录顺序一致() {
        let e = [汉英("馀", "yu2"), 汉英("余", "yu2"), 汉英("馀", "Yu2")];
        let 记录: Vec<String> = 词头们(&e);
        let 分组: Vec<String> = 分组形状(&e).into_iter().map(|(w, _)| w).collect();
        assert_eq!(分组, 记录);
        assert_eq!(分组, vec!["馀", "余"]);
    }

    /// 展开态的默认值来自设置，但**只对尚未出现过的键生效**——用户手动折叠过的
    /// 词条，不该因为后来改了「默认展开」而被强行展开。
    #[test]
    fn 默认展开只对新键生效() {
        let states = ExpandedStates::default();
        // 先以「默认折叠」建键，再手动展开。
        states.prepare(["a#0".into()], false);
        states.get("a#0", false).set(true);
        // 此后即便再以 default_open=false 备一次，已有的键也保持原状。
        states.prepare(["a#0".into()], false);
        assert!(states.get("a#0", false).get(), "已存在的键不受默认值影响");
        // 新键才吃默认值。
        states.prepare(["b#0".into()], true);
        assert!(states.get("b#0", true).get(), "新键应按默认值展开");
    }

    /// 一无所获时没有卡片——空列表而非一张空卡片。
    #[test]
    fn 一无所获没有卡片() {
        assert!(group_by_headword(&[]).is_empty());
    }

    /// 展开态的**唯一**要害：同一个键必须拿到同一个信号。
    ///
    /// 从前展开态是在 `entry_view` 里就地 `signal(false)` 的，卡片一重建就换新信号、
    /// 展开态归零——表现为「展开英英释义后点一下收藏，它自己收起来了」。本测试钉住
    /// 的正是那次重建之后仍能取回同一个信号。
    #[test]
    fn 同一键取回同一个展开态() {
        let states = ExpandedStates::default();
        states.prepare(["make#0".into()], false);
        let a = states.get("make#0", false);
        a.set(true);
        // 模拟卡片重建：重新取一次。
        let b = states.get("make#0", false);
        assert!(b.get(), "重建后展开态应当保持");
    }

    /// **崩溃回归**（2026-08-20 实机：输入 `misc` → 候选 → 回车 → 进程 abort）。
    ///
    /// 上面那条测试模拟的「重建」只有一半——真实的重建还包含**回收上一轮的构建期
    /// 信号**：windui 的 `host_signal` 用一个 `SignalScope` 圈住 `build_fn`，重建时
    /// 先整批 dispose。缺了这一半，就漏掉了真正的失败模式：信号若在重建作用域**内**
    /// 创建，表里存的句柄到期作废，下一次重建取回它、`collapsible` 一读就
    /// 「signal 句柄已失效」，而这条 panic 落在 Win32 窗口过程里不能展开，直接 abort。
    ///
    /// 故本测试必须用真的 `SignalScope` 包住取用，并在作用域外 `prepare`——那正是
    /// `rebuild_cards` 与 `card_view` 各自所处的位置。
    #[test]
    fn 展开态活过带回收的重建() {
        use windui::signal::SignalScope;
        let states = ExpandedStates::default();
        // 事件回调（`rebuild_cards`）：在任何重建作用域之外备好信号。
        states.prepare(["misc#0".into()], false);

        // 重建 #1：宿主在自己的作用域里构建卡片，其中取用展开态。
        let mut scope = SignalScope::new();
        let first = scope.collect(|| states.get("misc#0", false));
        first.set(true);

        // 重建 #2：宿主先回收上一轮的构建期信号，再造新的一批。
        scope.dispose();
        let mut next = SignalScope::new();
        let again = next.collect(|| states.get("misc#0", false));

        assert!(
            again.get(),
            "展开态该活过重建，且句柄不该随作用域一起被回收"
        );
    }

    /// 不同词条各自开合，互不影响（多音字一个词头、多条词条）。
    #[test]
    fn 不同键的展开态互不影响() {
        let states = ExpandedStates::default();
        states.prepare(["行#0".into(), "行#1".into()], false);
        states.get("行#0", false).set(true);
        assert!(states.get("行#0", false).get());
        assert!(!states.get("行#1", false).get(), "另一条词条不该被带着展开");
    }

    /// 预建与构建期取用**必须拼出同一个键**，否则预建等于没做，而症状是上面那条
    /// abort。两处都走 `expand_key`，这里钉住它的形状。
    #[test]
    fn 展开态的键带词条序号() {
        let hw = Headword::from_store("行");
        assert_eq!(expand_key(&hw, 0), "行#0");
        assert_eq!(expand_key(&hw, 1), "行#1");
    }

    // ── 设置页消息条 ──────────────────────────────────────────────────

    /// 语气与文本必须同写。漏写语气的症状是「历史记录已清空」沿用上一条失败消息的
    /// 红色，看着像操作没成。
    ///
    /// 两行叠放那套结构已随 `fg_role_signal` 收掉，「任一时刻至多一行可见」那三条
    /// 测试连同它一起作废——写反导致两行重叠或消息静默丢失的失败模式不存在了。
    #[test]
    fn 写消息条时语气跟着文本走() {
        let text = signal(String::new());
        let tone = signal(Role::Danger);

        write_note(text, tone, Role::Success, "历史记录已清空");
        assert_eq!(text.get(), "历史记录已清空");
        assert_eq!(tone.get(), Role::Success, "成功回执该是 Success 角色");

        // 反向也要跟得上：上一条是成功，这一条失败，语气不能停在绿色上。
        write_note(text, tone, Role::Danger, "清空历史失败：库已锁定");
        assert_eq!(text.get(), "清空历史失败：库已锁定");
        assert_eq!(tone.get(), Role::Danger, "失败消息该是 Danger 角色");
    }

    fn 自带(w: &str) -> Entry {
        自带来自(w, "牛津.mdx")
    }

    fn 自带来自(w: &str, key: &str) -> Entry {
        Entry::User(crate::domain::UserEntry {
            headword: Headword::from_store(w),
            source: "某本自带词典".into(),
            source_key: key.into(),
            body: vec![crate::domain::TextBlock {
                indent: 0,
                runs: vec![crate::domain::TextRun {
                    text: "释义".into(),
                    ..Default::default()
                }],
            }],
        })
    }

    fn 卡片(entries: Vec<Entry>) -> Card {
        Card {
            headword: entries[0].headword().clone(),
            fav: false,
            entries,
            // 这些测试考的是页签筛选，与字形无关。字形要靠打开字形库才拿得到，
            // 而单测里没有库文件——留空正是「字形库缺席」那条路径的样子。
            glyph: None,
        }
    }

    fn 内置(key: &'static str) -> TabKey {
        TabKey::Builtin(key)
    }

    #[test]
    fn 全部页收下所有来源() {
        for e in [
            英汉("apple"),
            汉英("苹果", "ping2 guo3"),
            自带("serendipity"),
        ] {
            assert!(entry_in_tab(&e, &TabKey::All));
        }
    }

    /// 内置两份各归各的页，且**自带词典的词条不进内置页**。
    ///
    /// 这是页签从「方向」改成「来源」之后最要紧的一条：此前自带词典的英文词条会
    /// 落进「英汉」页，因为那时判的是词头的方向。现在「简明英汉字典」这一页只该有
    /// 简明英汉字典的东西——否则页签名就是在撒谎。
    #[test]
    fn 内置两份各归各页() {
        use crate::source::offline::{CEDICT_KEY, ECDICT_KEY};
        let en = 英汉("apple");
        let zh = 汉英("苹果", "ping2 guo3");
        let user = 自带("apple");
        assert!(entry_in_tab(&en, &内置(ECDICT_KEY)));
        assert!(!entry_in_tab(&zh, &内置(ECDICT_KEY)));
        assert!(!entry_in_tab(&user, &内置(ECDICT_KEY)), "自带的不算内置的");
        assert!(entry_in_tab(&zh, &内置(CEDICT_KEY)));
        assert!(!entry_in_tab(&en, &内置(CEDICT_KEY)));
        assert!(!entry_in_tab(&user, &内置(CEDICT_KEY)));
    }

    /// 混装卡片按页签**拆开**：内置页只留内置的词条，自带页只留那一本的。
    ///
    /// 这是「逐条筛而不是整张筛」的全部理由。查 hello 时一个词头下面既有内置英汉库
    /// 的词条、又有自带词典的——整张留下，点进「简明英汉字典」还看得见另一本的内容；
    /// 整张丢掉，这个词头就会从它确实收录的那一页里消失。
    #[test]
    fn 混装卡片按来源拆开() {
        use crate::source::offline::ECDICT_KEY;
        let c = 卡片(vec![英汉("apple"), 自带来自("apple", "牛津.mdx")]);
        let all = filter_cards(vec![c.clone()], &TabKey::All);
        assert_eq!(all[0].entries.len(), 2, "全部页两条都在");

        let ec = filter_cards(vec![c.clone()], &内置(ECDICT_KEY));
        assert_eq!(ec.len(), 1, "词头还在");
        assert_eq!(ec[0].entries.len(), 1);
        assert!(matches!(ec[0].entries[0], Entry::English(_)));

        let ox = filter_cards(vec![c], &TabKey::User("牛津.mdx".into()));
        assert_eq!(ox[0].entries.len(), 1);
        assert!(matches!(ox[0].entries[0], Entry::User(_)));
    }

    /// 筛空的卡片整张丢掉，不留一个光秃秃的词头。
    #[test]
    fn 筛空的卡片不留词头() {
        use crate::source::offline::CEDICT_KEY;
        let c = 卡片(vec![英汉("apple")]);
        assert!(filter_cards(vec![c], &内置(CEDICT_KEY)).is_empty());
    }

    /// 自带词典按**稳定键**认，不按显示名。
    ///
    /// 两本词典的 MDX 标题一模一样是常有的事，且用户随时能改名；拿名字当依据会把
    /// 两本的词条混进同一页，而用户看到的是「这一页里混着别本的东西」。
    #[test]
    fn 自带词典按文件名分页() {
        let a = 自带来自("apple", "牛津.mdx");
        let b = 自带来自("apple", "柯林斯.mdx");
        let 牛津 = TabKey::User("牛津.mdx".into());
        assert!(entry_in_tab(&a, &牛津));
        assert!(!entry_in_tab(&b, &牛津), "同名不同文件必须分开");
        assert!(!entry_in_tab(&英汉("apple"), &牛津), "内置的不进自带页");
    }

    fn 走过(词们: &[&str]) -> NavPath {
        let mut p = NavPath::default();
        for w in 词们 {
            p.push(w);
        }
        p
    }

    #[test]
    fn 空路径哪边都走不动() {
        let p = NavPath::default();
        assert!(!p.can_go(false));
        assert!(!p.can_go(true));
    }

    #[test]
    fn 查了一个词仍无处可退() {
        let p = 走过(&["apple"]);
        assert!(!p.can_go(false), "只走过一步，脚下就是起点");
        assert!(!p.can_go(true));
    }

    #[test]
    fn 后退回到上一个词再前进回来() {
        let mut p = 走过(&["apple", "banana", "cherry"]);
        assert_eq!(p.go(false).as_deref(), Some("banana"));
        assert_eq!(p.go(false).as_deref(), Some("apple"));
        assert!(!p.can_go(false), "退到起点就不能再退");
        assert_eq!(p.go(true).as_deref(), Some("banana"));
        assert_eq!(p.go(true).as_deref(), Some("cherry"));
        assert!(!p.can_go(true), "走到末端就不能再进");
    }

    /// 连查同一个词不该在路径上堆台阶——否则按后退会「原地不动」好几下。
    #[test]
    fn 重复查同一个词不堆台阶() {
        let p = 走过(&["apple", "apple", "apple"]);
        assert!(!p.can_go(false), "三次都是同一个词，路径上只该有一步");
    }

    /// 浏览器语义：回退之后再查新词，原先那段前进路径作废。
    #[test]
    fn 回退后查新词截断前进路径() {
        let mut p = 走过(&["apple", "banana", "cherry"]);
        p.go(false); // 退到 banana
        assert!(p.can_go(true), "前提：此刻 cherry 还在前面");
        p.push("durian");
        assert!(!p.can_go(true), "查了新词，cherry 那段该没了");
        assert_eq!(
            p.go(false).as_deref(),
            Some("banana"),
            "后退仍回得到 banana"
        );
    }

    /// 退回去之后**原地**再查同一个词：不该截断，也不该新增。
    ///
    /// 这一条守的是 `push` 里那两步的**顺序**——先判重、再截断。反过来的话，
    /// 「后退到 banana 再点一次 banana」会把 cherry 截掉，而用户什么也没改变。
    #[test]
    fn 后退后重查当前词不动路径() {
        let mut p = 走过(&["apple", "banana", "cherry"]);
        p.go(false); // 退到 banana
        p.push("banana");
        assert!(p.can_go(true), "原地重查不该截断前进路径");
        assert_eq!(p.go(true).as_deref(), Some("cherry"));
    }

    /// 栏宽的硬边界：设置库被手改成荒唐值时也不能让界面不可用。
    #[test]
    fn 栏宽钳在硬边界内() {
        use crate::settings::{LEFT_PANE_W_MAX, LEFT_PANE_W_MIN};
        // root_w = 0 表示「还没布局过」，此时只施加硬边界，不按一个不存在的窗口宽算。
        assert_eq!(clamp_left_w(5, 0), LEFT_PANE_W_MIN);
        assert_eq!(clamp_left_w(5000, 0), LEFT_PANE_W_MAX);
        assert_eq!(clamp_left_w(300, 0), 300, "区间内的值原样通过");
    }

    /// 无论怎么拖，右栏都得留得下能读的宽度。
    #[test]
    fn 栏宽给右栏留足可读宽度() {
        const 窗口宽: i32 = 900;
        // 往右拖到底：左栏最多吃到 窗口宽 - RIGHT_MIN_W。
        assert_eq!(clamp_left_w(9999, 窗口宽), 窗口宽 - RIGHT_MIN_W);
        // 再往右一点也不行——这条断言守的是「拖到边界就停住」，不是某个具体数值。
        assert!(
            clamp_left_w(9999, 窗口宽) + RIGHT_MIN_W <= 窗口宽,
            "左栏加右栏下限不该超过窗口"
        );
    }

    /// 极窄窗口下两个下限打架时，左栏保住自己的下限。
    ///
    /// 720 是 `main.rs` 的窗口宽下限，此处特意取一个比它更窄的值：窗口尺寸下限是
    /// 靠平台约束保证的，而这个函数不该依赖那个保证才不出错。
    #[test]
    fn 极窄窗口下左栏保住下限() {
        use crate::settings::LEFT_PANE_W_MIN;
        // 400 的窗口减去 RIGHT_MIN_W 已是负数，cap 会低于下限。
        assert_eq!(clamp_left_w(300, 400), LEFT_PANE_W_MIN);
        assert_eq!(clamp_left_w(50, 400), LEFT_PANE_W_MIN);
    }

    /// 左栏那份列表与设置页正文同构：`scroll_area` 装在竖向容器里靠 `weight` 拿高度，
    /// 故同一个坑它也踩得到。
    ///
    /// 守的是结果：查询框、分段控件、空态行占掉固定高之后，列表的视口高必须**等于
    /// 剩余**，底边落在栏内。若哪天误把 `weight` 写成 `.fill()`，视口会按整条栏高
    /// 解析、底边越出栏外，症状是「左栏滚到底还差一截看不见」，差值恰好等于上方那些
    /// 固定高兄弟之和。详见 `scroll_area` 的文档注释。
    ///
    /// 骨架照抄 `left_list` 的 `scroll_area(host_signal(…))`，**不是** `list_signal`
    /// ——后者内部 `set_widget(DynList)` 会把 `Element::scroll()` 默认挂的
    /// `ScrollWidget` 顶掉，滚动条画得出来却抓不住（滚轮仍可用，因为它走
    /// `Tree::scroll_target`，只认 `Layout::Scroll`）。左栏一度就是这么写的，
    /// 表现为「历史列表拖不动」。这条测试守不住那个缺陷（widget 类型在 `Tree` 层
    /// 查不到），但骨架保持一致，至少读到这里的人不会再照着 `list_signal` 抄回去。
    #[test]
    fn 左栏列表的视口不越出栏底() {
        use windui::core::Tree;
        use windui::prelude::{signal, Element, Size};
        use windui::text::NullTextEngine;

        const 栏高: i32 = 400;
        const 头部高: i32 = 90;
        const 行高: i32 = 36;
        const 行数: i32 = 30;

        let rows = signal((0..行数).map(|i| format!("词{i}")).collect::<Vec<_>>());
        // 左栏骨架：一块固定高的头部（查询框 + 分段控件）+ 靠 weight 拿高度的滚动列表。
        let 栏 = Element::col()
            .fill()
            .child(Element::leaf().width_match().height(头部高))
            .child(scroll_area(
                Element::host_signal(rows, move |s: String| {
                    Element::label(s).width_match().height(行高)
                })
                .width_match(),
            ));

        let mut tree = Tree::new();
        let root = 栏.build(&mut tree);
        tree.root = Some(root);
        tree.layout_root(
            Size::new(crate::settings::LEFT_PANE_W_DEFAULT, 栏高),
            &mut NullTextEngine,
        );

        let 列表 = tree.get(root).unwrap().children[1];
        assert_eq!(
            tree.get(列表).unwrap().bounds.h,
            栏高 - 头部高,
            "视口高该是扣掉头部后的剩余；等于栏高说明主轴 Match 又拿了整条"
        );
        assert_eq!(
            tree.get(列表).unwrap().bounds.bottom(),
            栏高,
            "视口底边不该越出栏底"
        );

        let (_, 最大滚动) = tree.scroll_range(列表).expect("scroll_area 应是滚动容器");
        assert_eq!(
            最大滚动,
            行高 * 行数 - (栏高 - 头部高),
            "可滚动量按真实视口高算；视口多算一截，这里就少一截"
        );
    }

    /// 回归：设置页滚到底仍有一截正文看不见，差值恰好是页头的高度。
    ///
    /// 病因与规则写在 `scroll_area` 的文档注释里，这里守的是它的**结果**——竖向容器中
    /// 滚动区的视口必须落在父容器内，且高度等于扣掉固定高兄弟之后的剩余。
    ///
    /// 曾经的写法是 `Element::scroll().fill()`。看着合理（"填满剩余空间"），实际上
    /// 主轴上的 `Match` 被降级为 `Wrap` 后按整条父容器高解析，下面三条断言会各差一个
    /// 页头：视口高 300 而非 244、底边 356 越出窗口、末块滚到底仍有 56px 在窗外。
    #[test]
    fn 滚动区的视口不越出父容器底边() {
        use windui::core::Tree;
        use windui::prelude::{Element, Size};
        use windui::text::NullTextEngine;

        const 窗口高: i32 = 300;
        const 页头高: i32 = 56;
        const 块高: i32 = 200;
        const 块数: i32 = 3;

        // 内容必须比视口高，否则滚不动也就测不出越界。
        let mut 正文 = Element::col().width_match();
        for _ in 0..块数 {
            正文 = 正文.child(Element::leaf().width_match().height(块高));
        }
        // 设置页的骨架：固定高页头 + 滚动正文。
        let 页 = Element::col()
            .fill()
            .child(Element::leaf().width_match().height(页头高))
            .child(scroll_area(正文));

        let mut tree = Tree::new();
        let root = 页.build(&mut tree);
        tree.root = Some(root);
        let 布局 = |t: &mut Tree| t.layout_root(Size::new(400, 窗口高), &mut NullTextEngine);
        布局(&mut tree);

        // `Node::bounds` 是**相对父节点**的（绝对原点由 `arrange` 另行维护），
        // 判断"有没有越出窗口"必须沿父链累加，否则拿深层节点的局部 y 当绝对值用。
        fn 绝对底边(tree: &Tree, id: windui::core::NodeId) -> i32 {
            let n = tree.get(id).unwrap();
            let mut y = n.bounds.bottom();
            let mut 上级 = n.parent;
            while let Some(p) = 上级 {
                let pn = tree.get(p).unwrap();
                y += pn.bounds.y;
                上级 = pn.parent;
            }
            y
        }

        let 滚动区 = tree.get(root).unwrap().children[1];
        assert_eq!(
            tree.get(滚动区).unwrap().bounds.h,
            窗口高 - 页头高,
            "视口高该是扣掉页头后的剩余；等于窗口高说明主轴 Match 又拿了整条"
        );
        assert_eq!(绝对底边(&tree, 滚动区), 窗口高, "视口底边不该越出窗口");

        // 滚到底：末块的底边正好落在窗口内，一像素也不该被裁到窗外。
        let (_, 最大滚动) = tree.scroll_range(滚动区).expect("scroll_area 应是滚动容器");
        assert_eq!(
            最大滚动,
            块高 * 块数 - (窗口高 - 页头高),
            "可滚动量按真实视口高算；视口多算一截，这里就少一截"
        );
        assert!(tree.set_scroll_y(滚动区, 最大滚动));
        布局(&mut tree);

        let 正文节点 = tree.get(滚动区).unwrap().children[0];
        let 末块 = *tree.get(正文节点).unwrap().children.last().unwrap();
        assert_eq!(
            绝对底边(&tree, 末块),
            窗口高,
            "滚到底时正文末尾该贴着窗口底边，而不是停在窗外"
        );
    }
}
