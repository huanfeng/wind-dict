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
use windui::prelude::*;

use crate::domain::{Candidate, Dictionary, Entry, Headword, Lookup, Query, Sense, Wordlist};
use crate::settings::Settings;
use crate::skin::SkinKind;
use crate::source::offline::OfflineDictionary;
use crate::store::userdata::{now_secs, UserDataState};

/// 候选浮层最多列几条。
///
/// 20 条曾是「反正能滚」的产物，但浮层不带滚动条（高度随内容），且**没有键盘游标**
/// （↑↓ 传不进来，见 `docs/upstream-keyboard-path.md`），滚动只能靠鼠标——那还不如
/// 不列。7 条与有道词典的候选条数相当，且 7×38 + `padding(6)` 上下共 278px 的浮层
/// 在 620px 窗口里盖不住结果区太多。补全是缩小范围的工具，不是穷举词表。
///
/// 另一层理由与显示无关：单字母前缀（如 `a`）会命中约 5 万行，且 `ORDER BY frq`
/// 用不上索引（索引在 `(sw, word)` 上），SQLite 必须全排一遍——实测约 20ms。
/// LIMIT 不减少排序量，但它是唯一能钳住内存与渲染开销的地方。
const MAX_CANDIDATES: usize = 7;

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
    /// 候选区重建计数。见 `State::bump_cands`。
    cand_rev: Signal<Vec<u64>>,
    /// 最近一次被「选中」写进查询框的词。见 `State::select`。
    picked: Rc<RefCell<Option<String>>>,
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

        // 选中而来的改词：收起浮层，不再补全。用户已经确定要哪个词了，此刻还列出
        // 一串以它为前缀的别的词，只会盖住刚查出来的结果。
        //
        // 标记存的是**那个词**而非一个 bool，为的是能自校验。本方法的入口是纯版本
        // 比较，而两次 `set` 之间若没有插进一次 layout，它们会**合并成一次**
        // `on_update`（响应式更新在 layout 期派发——`core.rs:424` → `core.rs:370`，
        // 而 layout 挂在 render 上，WM_PAINT 在 Win32 队列里优先级最低，可以被
        // 鼠标消息饿着）。若只是个 bool，合并时它会被后一次（打字）那轮吃掉，
        // 那一击的候选被静默清空。存了词就对不上，抑制自然失效，打字照常补全。
        if self.picked.borrow_mut().take().as_deref() == Some(text.as_str()) {
            self.candidates.set(Vec::new());
            self.reset_cursor();
            return;
        }

        // 补全**永远由离线词典驱动**，与用户选中哪个查询源无关——补全需要词表，
        // 而词表只有词典有（译源没有词库，不知道世上存在哪些词）。见术语表「补全」。
        let list = self
            .dict
            .complete(&text, MAX_CANDIDATES)
            .unwrap_or_default();
        self.candidates.set(list);
        self.reset_cursor();
    }
}

impl Completer {
    /// 游标归零并让候选区重建。两件事必须一起做：游标是构建期读的，只改信号不重建，
    /// 高亮会停在上一批候选的位置上。
    fn reset_cursor(&self) {
        self.cursor.set(0);
        bump(self.cand_rev);
    }
}

/// 侧栏页签：历史记录 / 收藏。
///
/// **不叫「生词本」**——设计稿此处写的是生词本，但术语表明令该词弃用：本项目的收藏
/// 是纯粹的书签语义，不承载掌握程度与复习计划，叫生词本会招来那一整套学习状态。
const TAB_HISTORY: usize = 0;
const TAB_FAVORITES: usize = 1;

/// 侧栏一次最多列出的历史条数。侧栏是「最近查过什么」，不是全量档案。
const SIDE_LIMIT: usize = 100;

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

/// 强调色淡底的不透明度。用于列表选中行、已收藏星标的底。
///
/// 走 `bg_role_alpha` 而非写死颜色：淡底必须随强调色一起变，写死则换肤后底色与强调
/// 色对不上。取 0.14 与设计稿三套皮肤的 `--accent-2` 观感相当。
const ACCENT_SOFT_A: f32 = 0.14;

/// 正文行高倍数。
///
/// 1.7 是中文正文的常用值：CJK 字身方正、笔画密度高，按字体自带行距排出来会显得
/// 拥挤。只施加在**会换行的多行文字**上（释义、义项）——音标、量词这类单行注解
/// 用不上，给了反而平白拉高行盒。
const BODY_LH: f32 = 1.7;

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

/// 侧栏的一行。
#[derive(Clone, PartialEq, Eq)]
struct SideRow {
    headword: Headword,
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
}

/// 监视页签与用户数据变更、重取侧栏列表的响应式控件。
///
/// 与 `Completer` 同构：不绘制任何东西，靠 `on_update` 相位工作，故空闲时不占 CPU。
/// 它同时盯两个信号——页签切换要换数据源，用户数据变更（查询记了历史、收藏增删）
/// 要刷新当前数据源。
struct SideLoader {
    st: Rc<State>,
    last_tab: u64,
    last_rev: u64,
}

impl Widget for SideLoader {
    fn on_update(&mut self, _ctx: &mut EventCtx) {
        let (tab, rev) = (self.st.side_tab.version(), self.st.revision.version());
        if tab == self.last_tab && rev == self.last_rev {
            return;
        }
        self.last_tab = tab;
        self.last_rev = rev;
        self.st.reload_side();
        // 收藏状态变了，结果区的星标也得跟着变——卡片上的 `fav` 是快照，不会自己更新。
        //
        // 只刷星标、不重新分组：词条没变，重跑一遍 `group_by_headword` 是白做的。
        //
        // 代价：`revision` 不区分「收藏变了」和「历史变了」，故每次查询（写历史）也会
        // 白刷一遍星标，多花每词头一次 `is_favorite` 查询。没有拆成两个信号，是因为
        // 词头通常只有一两个，拆分买到的性能不抵它带来的「哪个信号该由谁 bump」的负担。
        self.st.refresh_fav_flags();
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
    key: Signal<String>,
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
        let text = self.key.get();
        let Some(c) = text
            .trim()
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphanumeric())
        else {
            self.st.note_err("热键的主键请填一个字母或数字");
            return;
        };
        self.st.set_hotkey(crate::settings::HotkeySpec {
            ctrl: self.ctrl.get(),
            alt: self.alt.get(),
            shift: self.shift.get(),
            key: c.to_ascii_uppercase(),
        });
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
    /// 用户数据（收藏与历史）。不可用时保留**原因**，由 `unavailable_bar` 展示。
    user: UserDataState,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
    /// 候选列表的键盘游标（下标）。候选非空时恒指向其中一条。
    cursor: Signal<usize>,
    /// 候选区重建计数。游标是构建期读的，改了游标必须让这块整体重来，见 `bump`。
    cand_rev: Signal<Vec<u64>>,
    /// 最近一次被「选中」写进查询框的词。见 `State::select`。
    picked: Rc<RefCell<Option<String>>>,
    /// 结果区按词头分组的卡片。**结果区唯一的数据源**，见 `rebuild_cards`。
    cards: Signal<Vec<Card>>,
    /// 结果区的提示文案（未收录、请输入等）。
    hint: Signal<String>,
    /// 侧栏当前页签。
    side_tab: Signal<usize>,
    /// 侧栏当前列出的行。
    side_rows: Signal<Vec<SideRow>>,
    /// 用户数据的变更计数。收藏增删、历史写入后自增，驱动侧栏与卡片重取。
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
    /// 主列当前页：词典 / 设置。
    page: Signal<usize>,
    /// 召回抽屉是否展开。
    ///
    /// 默认关着（`build` 里初始化为 false）：DESIGN.md 的「Search is home」讲的正是
    /// 这件事——默认可见的界面该服务下一次查询，而不是回顾上一次。
    drawer_open: Signal<bool>,
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
    /// 选中一个词：填进查询框、查询，并**关掉候选浮层**。
    ///
    /// 候选浮层与侧栏行两处都走这里（两个侧栏页签共用同一个 `side_row`）。它们此前
    /// 各自写「`query.set` 然后 `lookup`」，于是都带上了同一个毛病：`query.set`
    /// **无条件** bump 版本（windui `signal.rs:163`，没有任何值比较），`Completer`
    /// 醒来拿新词重算补全，浮层于是又开了——而这次它盖住的正是刚查出来的结果。
    ///
    /// 修法是记下「这次是选中，选的是哪个词」，由 `Completer` 比对后抑制一次补全。
    /// 用 `RefCell` 而非 `Signal`：它不该触发任何重建，只是两段代码之间的一句交代。
    ///
    /// 之所以不能靠「点完把 candidates 清空」了事——清空发生在事件回调里，而响应式
    /// 更新在其后的 layout 期才成批派发（`core.rs:424` → `core.rs:370`），清完立刻
    /// 又被填回去。顺序在这里是决定性的。
    fn select(&self, word: &str) {
        *self.picked.borrow_mut() = Some(word.to_string());
        self.query.set(word.to_string());
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
        if let Some(c) = self.candidate_at_cursor() {
            self.select(c.headword.as_str());
            return;
        }
        let text = self.query.get();
        if !text.trim().is_empty() {
            self.lookup(&text);
        }
    }

    /// 移动候选游标。`down` 为真下移，否则上移。
    ///
    /// **不环绕**。列表最多 7 条、全在眼前，环绕买不到什么；而在顶上按 ↑ 直接跳到末条
    /// 是种惊吓——尤其它同时意味着「再按一下就选中最后那个词」。
    fn move_cursor(&self, down: bool) {
        let n = self.candidates.get().len();
        if n == 0 {
            return;
        }
        let i = self.cursor.get();
        let next = if down {
            (i + 1).min(n - 1)
        } else {
            i.saturating_sub(1)
        };
        if next != i {
            self.cursor.set(next);
            bump(self.cand_rev);
        }
    }

    /// Tab：把游标那条候选填进查询框，**不查询**。
    ///
    /// 这是 shell 的补全语义——Tab 补全词，回车才执行。对词典而言尤其顺：把词补全整了
    /// 再接着改（`make` → `maker`），比先查一次再回来改省一步。
    fn accept_completion(&self) {
        let Some(c) = self.candidate_at_cursor() else {
            return;
        };
        // 不走 `select`：那会连查询一起做掉，且抑制掉后续补全。这里只改字，补全照常
        // 跟上——补完的词往往还要再接着打（`make` 之后接 `r`）。
        self.query.set(c.headword.as_str().to_string());
    }

    /// 游标当前指向的候选。候选为空、或游标越界时为 `None`。
    fn candidate_at_cursor(&self) -> Option<Candidate> {
        self.candidates.get().get(self.cursor.get()).cloned()
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

    /// 执行查询。**只在查询词确定时调用**（选中候选），不逐键触发——见术语表「补全」：
    /// 补全回答「我想拼的是哪个词」，查询回答「这个词什么意思」，是两个动作。
    fn lookup(&self, word: &str) {
        // 上一次操作的消息到此为止：新一次查询开始，那条红字讲的已是别的词。
        self.notice.set(String::new());
        let Some(q) = Query::new(word) else {
            self.rebuild_cards(&[]);
            self.hint.set("输入一个词开始查询".into());
            return;
        };
        match self.dict.lookup(&q) {
            Err(e) => {
                self.rebuild_cards(&[]);
                self.hint.set(format!("词库读取失败：{e}"));
            }
            Ok(Lookup::NotFound) => {
                self.rebuild_cards(&[]);
                // 提示切换到译源，但**绝不自动发起**——见 ADR-0002。
                self.hint.set(format!("离线词典未收录「{}」", q.text()));
            }
            Ok(Lookup::Found {
                entries,
                via_base_form,
            }) => {
                self.hint.set(if via_base_form {
                    // 用户查的是变化形态，显示的是原形词条——不提示会让人困惑。
                    format!("显示的是「{}」的原形词条", q.text())
                } else {
                    String::new()
                });
                self.record_all(&headwords_to_record(&entries));
                self.rebuild_cards(&entries);
            }
        }
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

    /// 落盘当前设置。**失败当场告知**——设置是用户主动表达的意图，静默失败会让人
    /// 以为改好了，下次启动才发现没变。与收藏写入失败同一条原则。
    ///
    /// 返回是否成功，供调用方决定要不要接着做别的（如改注册表）。
    /// 宣告设置有变，令设置页重建。
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

    /// 点标题栏上的召回入口：开抽屉并切到该页签；已经停在这一页则收起。
    ///
    /// 「再点一次收起」这条是刻意的：入口就那一个，若只负责打开，关抽屉就只剩头部
    /// 那个 ×，而人手已经在标题栏上了。
    fn toggle_drawer(&self, tab: usize) {
        if self.drawer_open.get() && self.side_tab.get() == tab {
            self.drawer_open.set(false);
            return;
        }
        self.side_tab.set(tab);
        self.drawer_open.set(true);
    }

    fn bump_settings(&self) {
        bump(self.settings_rev);
    }

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

    /// 换皮肤。
    ///
    /// 目前**只能重启生效**：`ThemeHandle::set` 不重建元素树，而本应用的自绘区域色
    /// （标题栏底、侧栏底、卡片底）在 windui 的 `Role` 里没有对应项，解析不出来。
    /// 完整论证见 ADR-0012。这里如实告知用户，而不是让他点完毫无反应。
    /// 换皮肤。**立即生效**：界面无一处写死颜色，`ThemeHandle::set` 之后下一帧全树
    /// 按新色板重新解析。
    ///
    /// 先换后存：换肤是纯视觉、可随时再换，让用户当场看到结果比「先确保存住」更重要；
    /// 存失败时如实告知「本次有效、重启后回退」，而不是回滚掉一个用户已经看到的变化。
    fn set_skin(&self, kind: SkinKind) {
        self.settings.borrow_mut().skin = kind;
        self.theme.set(kind.skin().theme);
        // 文字与色块靠 `Role` 每帧自己跟上，图标不行——它的颜色在构建期就解析成了具体
        // 色值（见 `crate::icon`）。重建整树是唯一能让图标跟上的办法，理由见 `build`。
        bump(self.skin_rev);
        if self.save_settings() {
            self.note_clear();
        } else {
            self.note_err("皮肤已切换，但未能保存，重启后会回到原来的皮肤");
        }
    }

    /// 改唤起热键。**立即生效**：`HotkeyHandle::set` 下一次消息循环向系统换注册。
    ///
    /// 拦住无修饰键：那会吞掉该字母在**所有程序**里的输入，用户按一下 D 就唤起词典，
    /// 等于没法打字了——而这个错误一旦犯下，用户很难意识到是词典干的。
    fn set_hotkey(&self, spec: crate::settings::HotkeySpec) {
        if !spec.has_modifier() {
            self.note_err("热键至少要带一个 Ctrl / Alt / Shift，否则会吞掉该键在所有程序里的输入");
            return;
        }
        self.settings.borrow_mut().hotkey = spec;
        self.hotkey.set(spec.to_hotkey());
        if self.save_settings() {
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
    fn set_dict_path(&self, is_ec: bool, path: Option<std::path::PathBuf>) {
        // **先校验再落库**。选错文件的代价是致命的：词库打不开时 `main` 直接 exit，
        // 而 release 构建没有控制台（`windows_subsystem = "windows"`），用户看到的是
        // 「双击没反应、托盘不出现、零提示」，且设置存在 %LOCALAPPDATA% 里无从下手。
        // 文件选择器只按扩展名过滤，把汉英库选进英汉槽它照收——非校验不可。
        if let Some(p) = &path {
            if let Err(e) = crate::source::offline::probe_dict(p, is_ec) {
                self.note_err(format!("这个文件不能用作词库：{e:#}"));
                return;
            }
        }
        let has = path.is_some();
        {
            let mut s = self.settings.borrow_mut();
            if is_ec {
                s.ecdict = path;
            } else {
                s.cedict = path;
            }
        }
        if self.save_settings() {
            self.note_ok(if has {
                "词库已更改，重启后生效"
            } else {
                "已恢复默认词库路径，重启后生效"
            });
        }
    }

    /// 从当前页签移除一行：历史页删历史条目，收藏页取消收藏。
    ///
    /// **动作随页签而变**是刻意的：侧栏的 × 意思是「把这一行从我眼前的这个列表里
    /// 去掉」。在历史里那是删记录，在收藏里那是取消收藏——若两处都去删历史，收藏页
    /// 的 × 就会点了没反应。
    ///
    /// 失败当场告知：两者都是用户主动表达的意图，与历史的**被动写入**不同。
    fn remove_side_row(&self, hw: &Headword) {
        let UserDataState::Ready(u) = &self.user else {
            self.notice.set("用户数据未能打开，无法修改".into());
            return;
        };
        let on_favorites = self.side_tab.get() == TAB_FAVORITES;
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

    /// 当前展示的第一个词头，供侧栏标出「你正在看的是这条」。
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

    /// 宣告用户数据有变，驱动侧栏与卡片重取。
    fn bump(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// 重取侧栏列表。数据不可用或读取失败时给出空列表——顶部的警示条已说明原因，
    /// 此处再报一遍是噪音。
    fn reload_side(&self) {
        let UserDataState::Ready(u) = &self.user else {
            self.side_rows.set(Vec::new());
            return;
        };
        let rows = if self.side_tab.get() == TAB_FAVORITES {
            u.favorites().map(|v| {
                v.into_iter()
                    .map(|f| SideRow {
                        headword: f.headword,
                    })
                    .collect()
            })
        } else {
            u.history(SIDE_LIMIT).map(|v| {
                v.into_iter()
                    .map(|h| SideRow {
                        headword: h.headword,
                    })
                    .collect()
            })
        };
        self.side_rows.set(rows.unwrap_or_default());
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

/// 构建界面。
///
/// `user` 不可用时顶部常驻一条警示，说明历史记录失效及其原因（收藏有入口后一并
/// 纳入，见 `unavailable_bar`）。
pub fn build(
    dict: OfflineDictionary,
    user: UserDataState,
    theme: ThemeHandle,
    hotkey: HotkeyHandle,
) -> Element {
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
        user,
        query: signal(String::new()),
        candidates: signal(Vec::new()),
        cursor: signal(0),
        cand_rev: signal(vec![0]),
        picked: Rc::new(RefCell::new(None)),
        cards: signal(Vec::new()),
        hint: signal(String::from("输入一个词开始查询")),
        side_tab: signal(TAB_HISTORY),
        side_rows: signal(Vec::new()),
        revision: signal(0),
        notice: signal(String::new()),
        expanded: ExpandedStates::default(),
        theme,
        hotkey,
        page: signal(PAGE_DICT),
        drawer_open: signal(false),
        settings: RefCell::new(settings),
        settings_note: signal(String::new()),
        settings_note_tone: signal(Role::Danger),
        confirm_clear: signal(false),
        settings_rev: signal(vec![0]),
        skin_rev: signal(vec![0]),
    });
    // 开屏即列出历史：侧栏空着会让人以为功能坏了。
    st.reload_side();

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
    Element::host_signal(st.skin_rev, move |_rev: u64| {
        // `host_signal` 的回调是 `Fn`，会被反复调用，故每次都得拿一份自己的。
        window_root(st.clone(), unavailable.clone())
    })
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
        .child(brand().weight(1.0))
        // 召回与设置三个入口并列在这里。
        //
        // 召回入口此前是右侧一条 44px 常驻 rail 上的两个字形（`↺` `☆`）。撤掉它有两
        // 条独立的理由：
        //
        // 1. **认不出来**。使用者把 `↺` 读成了「刷新」——这不是他看得不够仔细，而是
        //    U+21BA 本来就是通用的循环/重载符号，把它当「历史」用是我们一厢情愿。
        //    图标要么用公认字形，要么就别用；`历史` 二字没有第二种读法。
        // 2. **它吃掉了正文右边的一条**。rail 常驻 44px + 左边框，正文永远够不到窗口
        //    右缘。而正文限宽撤掉之后（见 `EN_DEF_MAX_W`），铺满右侧正是这次重排要的
        //    效果，留一条灰带在那儿等于白撤。
        //
        // 用文字而非图标同样适用于设置：U+2699 在 Windows 上会被 Segoe UI Emoji 接管，
        // 画出来是一个彩色齿轮，与这一屏的单色格调格格不入（变体选择符 U+FE0E 无效，
        // windui 的文本渲染不处理它）。走 SVG 又要 `ImageContent::tint` 定一个具体
        // 颜色，换肤时它不会跟着变——ADR-0012 结案段刚把「界面无一处写死颜色」这条
        // 挣回来。
        .child(recall_entry("历史", TAB_HISTORY, st.clone()))
        .child(recall_entry("收藏", TAB_FAVORITES, st.clone()))
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

/// 标题栏上的一个召回入口：开抽屉并切到该页签；已经停在这一页则收起。
///
/// 「再点一次收起」这条是刻意的：入口就这一个，若它只负责打开，关抽屉就只剩抽屉头部
/// 那个 ×，而人手已经在这儿了。
fn recall_entry(text: &str, tab: usize, st: Rc<State>) -> Element {
    bar_entry(text, move |_ctx| st.toggle_drawer(tab))
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
/// **不画激活态**。抽屉开着时它头部的分段控件已经写明当前停在哪一页，标题栏再标一次
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

/// 主体区域：主列 + 召回抽屉。
///
/// 召回从常驻侧栏改成按需抽屉，依据是 DESIGN.md 的「Search is home / Recall is a
/// drawer」：默认可见的界面该服务**下一次**查询，而 224px 的历史列表是在回顾上一次。
///
/// 抽屉收起时主列独占**整个**宽度——一像素也不留。入口已经移到标题栏（见
/// `recall_entry`），此处不再有 rail，「怎么把抽屉叫出来」仍然一直看得见。
fn body(st: Rc<State>, unavailable: Option<String>) -> Element {
    Element::row()
        .fill()
        .cross(Align::Stretch)
        // 列表驱动器提到这一层，且排在**所有消费者之前**。
        //
        // 它此前挂在侧栏内部，靠「侧栏在主列左边、故先建」才赶得上——`on_update` 按
        // `Element::build` 的前序（即书写顺序）派发，而它除了重载列表还要刷新结果区
        // 卡片的星标，落在主列之后就慢一帧。召回移到右侧之后那个前提不再成立，故提到
        // 这里：位置由「必须先于所有消费者」这条约束决定，不再是左右布局的副产品。
        .child(side_loader(st.clone()))
        .child(pages(st.clone(), unavailable).weight(1.0))
        .child(drawer(st))
}

/// 列表驱动器：零尺寸、不可见，重载召回列表并刷新结果区星标。位置约束见 `body`。
fn side_loader(st: Rc<State>) -> Element {
    Element::leaf()
        .reactive()
        .widget(SideLoader {
            last_tab: st.side_tab.version(),
            last_rev: st.revision.version(),
            st,
        })
        .size(0, 0)
}

/// 召回抽屉：历史 / 收藏，按需展开，挤压主列而非盖住它。
///
/// 挤压而非覆盖：抽屉里点一个词，结果就出现在它左边，两者要同时可见。覆盖式抽屉
/// 得先关掉才能读结果，而「点一个词 → 读 → 再点下一个」正是召回的主要用法。
fn drawer(st: Rc<State>) -> Element {
    let open = st.drawer_open;
    Element::col()
        .width(280)
        .height_match()
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Divider, 1)
        .border_edges(Edges::LEFT)
        .visible_when(move || open.get())
        .child(drawer_head(st.clone()))
        .child(side_list(st).fill().padding_xy(8, 8).weight(1.0))
}

/// 抽屉头部：历史 / 收藏分段 + 关闭。
///
/// **不放收藏计数**。它原先在侧栏底部的设置入口上（「142 词」），那里的理由是「让人
/// 知道这个入口后面有内容」——抽屉一打开列表就在眼前，这个理由没了。而它是构建期
/// 快照，收藏之后不会自己更新，留着就是个会过期的数字。
fn drawer_head(st: Rc<State>) -> Element {
    let open = st.drawer_open;
    Element::row()
        .width_match()
        .height(46)
        .cross(Align::Center)
        .padding_xy(10, 0)
        .spacing(8)
        .border_role(Role::Divider, 1)
        .border_edges(Edges::BOTTOM)
        // 分段控件而非页签：两者都表达单选，但页签的语义是「切换到另一个页面」，
        // 而这里两页是同一个列表的两种来源，切过去人还在抽屉里。
        .child(Element::segmented(vec!["历史", "收藏"], st.side_tab).weight(1.0))
        .child(
            crate::icon::button(crate::icon::CLOSE, 26, Role::TextDisabled)
                .on_click(move |_ctx| open.set(false)),
        )
}

/// 主列：词典页与设置页叠在一起，按 `page` 切换。
///
/// 用叠层而非替换，是为了保住词典页的状态——查询词、候选、结果卡片、滚动位置都在
/// 元素树里，若切页时把整棵子树换掉，从设置页回来会发现结果没了。
fn pages(st: Rc<State>, unavailable: Option<String>) -> Element {
    let (p1, p2) = (st.page, st.page);
    Element::stack()
        .fill()
        .child(
            main_column(st.clone(), unavailable)
                .fill()
                .visible_when(move || p1.get() == PAGE_DICT),
        )
        .child(
            settings_page(st)
                .fill()
                .visible_when(move || p2.get() == PAGE_SETTINGS),
        )
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
/// 视口高才等于真实可见高。`main_column` 里那句「不写 `.fill()`」说的是同一件事。
fn scroll_area(child: Element) -> Element {
    Element::scroll().width_match().weight(1.0).child(child)
}

/// 召回列表。历史与收藏共用一份数据信号，由 `SideLoader` 按当前页签重载。
///
/// 改抽屉时顺带修掉了一处浪费：此前用 `Element::tabs` 把两页都建进树、只给未选中的
/// 挂 `visible_when`，而 `on_update` 的派发只看 `enabled` 不看 `visible`，隐藏那页的
/// 列表照样跟着重建——每次列表变更都是一倍的节点。现在抽屉里只有一个列表，页签切换
/// 只换数据不换结构。
///
/// 传给 `list_signal` 的 key 函数当前是**死参数**：windui 的形参名为 `_key_fn`，
/// 内部做全量重建，没有 keyed diff。传它是为将来上游补齐后自动生效，别据此以为
/// 行有稳定身份（hover / 滚动位置在重建时都会丢）。
fn side_list(st: Rc<State>) -> Element {
    Element::list_signal(
        st.side_rows,
        |r: &SideRow| r.headword.as_str().to_string(),
        move |r: SideRow| side_row(r, st.clone()),
    )
    .fill()
}

/// 侧栏的一行：词头（点击即查）+ 移除按钮。
///
/// 不用 `Element::nav_row`：它自带的 `›` 是「钻入子页」的语义，而这里点一行的动作是
/// 「查这个词」，查完人还在原地。图标与语义不符会让人误以为侧边还有一层。
fn side_row(r: SideRow, st: Rc<State>) -> Element {
    let word = r.headword.as_str().to_string();
    // 选中态标出「你正在看的是这条」——设计稿的圆点与淡底不是纯装饰，它回答了
    // 「我刚点的是哪个」这个问题，尤其在历史列表里滚动之后。
    let active = st.current_headword().as_deref() == Some(word.as_str());
    let (pick_st, del_st) = (st.clone(), st);
    let (pick_word, del_hw) = (word.clone(), r.headword.clone());
    let mut row = Element::row()
        .width_match()
        .height(36)
        .cross(Align::Center)
        .corner(9.0)
        .padding_xy(12, 0)
        .spacing(10)
        .clickable()
        .on_click(move |_ctx| {
            pick_st.select(&pick_word);
        });
    if active {
        row = row.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
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
            .on_click(move |_ctx| del_st.remove_side_row(&del_hw)),
    )
}

/// 主列：查询框 + 补全候选 + 结果。
fn main_column(st: Rc<State>, unavailable: Option<String>) -> Element {
    // 左右 28px。rail 撤掉之后这个值管的是**正文与窗口边缘**的距离，两侧对称——正文
    // 铺满不等于顶到窗框上，那样读起来局促。设计稿此处是 40px，那是在更宽的画布上。
    let mut root = Element::col().fill().padding_xy(28, 16).spacing(12);
    // 不可用时才占这一行：正常情况下不该为一个不会发生的故障留白。
    if let Some(why) = unavailable {
        root = root.child(unavailable_bar(&why));
    }
    root
        // 补全驱动器：零尺寸、不可见，必须排在候选列表之前（on_update 按注册顺序广播）。
        .child(
            Element::leaf()
                .reactive()
                .widget(Completer {
                    cursor: st.cursor,
                    cand_rev: st.cand_rev,
                    dict: st.dict.clone(),
                    query: st.query,
                    candidates: st.candidates,
                    picked: st.picked.clone(),
                    last_version: st.query.version(),
                })
                .height(0),
        )
        // 占位符不用「单词」「搜索」（均为术语表弃用词），但必须保住「中英皆可」
        // 这条信息：查询方向由查询词自动判定，界面上没有方向选择器（ADR-0003），
        // 用户无从知道这个框两种文字都收。也不与结果区提示重复——同一句话在开屏时
        // 同屏出现两遍，占位符就白占了。
        //
        // 设计稿此处的查询框右侧有一组 `Ctrl` `K` 键帽，未照做：本项目的唤起热键是
        // Ctrl+Alt+D（`main.rs`），而窗口内并没有 Ctrl+K 这个键位。画一组按了没用的
        // 键帽比不画更糟。
        .child(query_box(st.clone()))
        .child(notice_bar(st.notice, None))
        // 此处原有一条分隔线。拿掉了：候选区收起后它就紧贴查询框，把主列切成两截，
        // 而它要分隔的两样东西（候选、结果）本就不会同时是空的。区域感交给留白。
        //
        // 结果与候选叠在一起：结果铺满，候选浮在其上、顶端对齐。`Layout::Frame` 用
        // 单个 `align` 同时定横纵（windui `core.rs:arrange_frame`），故候选靠
        // `width_match` 撑满横向、靠默认的 `Align::Start` 贴顶。
        .child(
            Element::stack()
                // 不写 `.fill()`：它的高度分量会立刻被 `weight` 覆盖（`weight` 在竖向
                // 父容器里落到高度维），写了等于留一句死代码。
                .width_match()
                .weight(1.0)
                .child(result_area(st.clone()))
                .child(candidate_panel(st)),
        )
}

/// 设置页。
///
/// 分组与卡片式行照设计稿，但**项目按本项目的能力来**：设计稿的「发音」「例句翻译」
/// 两组没有对应数据源，不画；换来的是设计稿没有的热键、开机自启、词库路径——那才是
/// 一个常驻词典真正要让用户调的东西。
fn settings_page(st: Rc<State>) -> Element {
    let page = st.page;
    let confirm = st.confirm_clear;
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
                    crate::icon::button(crate::icon::BACK, 32, Role::Text).on_click(move |_ctx| {
                        // 离开即撤销确认态：回来时不该还举着「确认清空」等人误触。
                        confirm.set(false);
                        page.set(PAGE_DICT);
                    }),
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
                .child(notice_bar(st.settings_note, Some(st.settings_note_tone)))
                .child(group("外观", skin_cards(st.clone())))
                .child(group("唤起", hotkey_row(st.clone())))
                .child(group("启动", autostart_row(st.clone())))
                .child(group("词库", dict_rows(st.clone())))
                .child(group("释义显示", expand_en_row(st.clone())))
                .child(group("数据", data_rows(st))),
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
            // 「侧栏」这个词在界面上已经没有对应物很久了——先改成抽屉，这次连抽屉的
            // 入口也移到了标题栏。指路的文案必须指得到，否则不如不写。
            Element::label("在「收藏」里逐条取消")
                .font_size(12.5)
                .fg_role(Role::TextMuted),
        ),
    ])
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

/// 皮肤卡片三选一。
fn skin_cards(st: Rc<State>) -> Element {
    let mut row = Element::row()
        .width_match()
        .spacing(14)
        .cross(Align::Stretch);
    for kind in SkinKind::ALL {
        let st = st.clone();
        let current = st.settings.borrow().skin == kind;
        let sw = kind.skin().swatch;
        row = row.child(
            Element::col()
                .weight(1.0)
                .bg_role(Role::Surface)
                .border_role(if current { Role::Accent } else { Role::Border }, 2)
                .corner(12.0)
                .padding(14)
                .spacing(4)
                .clickable()
                .on_click(move |_ctx| st.set_skin(kind))
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
                    Element::label(kind.desc())
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

/// 唤起热键。当前**只读展示**——改键需要框架支持运行时重注册，见下方注释。
fn hotkey_row(st: Rc<State>) -> Element {
    let spec = st.settings.borrow().hotkey;
    let ctrl = signal(spec.ctrl);
    let alt = signal(spec.alt);
    let shift = signal(spec.shift);
    let key = signal(spec.key.to_string());
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
        Some("全局生效，改完立即生效。至少要带一个修饰键"),
        Element::row()
            .cross(Align::Center)
            .spacing(10)
            // 编辑器：零尺寸、不可见，须先于它监视的控件注册。
            .child(editor)
            .child(Element::checkbox("Ctrl", ctrl))
            .child(Element::checkbox("Alt", alt))
            .child(Element::checkbox("Shift", shift))
            .child(
                Element::text_input(key, "键")
                    .width(48)
                    .font_size(13.0)
                    .text_align(Align::Center),
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

/// 词库路径。
fn dict_rows(st: Rc<State>) -> Element {
    card(vec![dict_row(st.clone(), true), dict_row(st, false)])
}

fn dict_row(st: Rc<State>, is_ec: bool) -> Element {
    let (title, cur) = if is_ec {
        ("英汉词库", st.settings.borrow().ecdict.clone())
    } else {
        ("汉英词库", st.settings.borrow().cedict.clone())
    };
    let shown = cur
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（默认：程序同目录）".into());
    let mut right = Element::row().cross(Align::Center).spacing(8);
    // 有自定义路径时才给「恢复默认」——本来就是默认值时这个按钮点了没意义。缺了它，
    // 用户一旦选错就再也回不到默认，只能去手改数据库。
    if cur.is_some() {
        let st2 = st.clone();
        right = right.child(
            Element::button("恢复默认").on_click(move |_ctx| st2.set_dict_path(is_ec, None)),
        );
    }
    right = right.child(Element::button("选择…").on_click(move |ctx| {
        let st = st.clone();
        ctx.request_pick_file(
            PickDialog::new()
                .title(if is_ec {
                    "选择英汉词库"
                } else {
                    "选择汉英词库"
                })
                .filter("SQLite 词库", &["db"]),
            move |picked| {
                if let Some(p) = picked {
                    st.set_dict_path(is_ec, Some(p));
                }
            },
        );
    }));
    row(title, Some(&shown), right)
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
                .on_nav_key(move |_ctx, ev| match ev.key {
                    Key::Down => {
                        nav_st.move_cursor(true);
                        true
                    }
                    Key::Up => {
                        nav_st.move_cursor(false);
                        true
                    }
                    // **Shift+Tab 必须放过**：吞掉它，用户除了鼠标就没有任何办法把焦点
                    // 移出查询框了。只认裸 Tab。
                    Key::Tab if !ev.shift => {
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
                        .on_click(move |_ctx| st.clear_query()),
                )
                // 空框上放一个「清空」按钮没有意义，且它会盖住占位符的尾部。
                .visible_when(move || !query.get().is_empty()),
        )
}

/// 候选浮层：点一条即「确定查询词」，触发查询。
///
/// **浮在结果之上，不占布局流**。此前它是主列 col 里的一段定高区，出现时把整个结果区
/// 下推 160px——用户正读着某词的释义，多打一个字母，正文就跳走了。补全是「我想拼的是
/// 哪个词」的辅助，不该打断「这个词什么意思」的阅读。有道词典也是这么处理的。
///
/// 代价不只是视觉遮挡——被盖住的区域**不可点击**（浮层不透明，本就不该穿透），含首张
/// 卡片的收藏星标与结果区那一段的滚动条命中区；且指针停在浮层上时滚轮无响应，因为
/// `result_area` 的滚动容器是浮层的**兄弟**而非祖先，冒泡到不了它。
///
/// 都可以接受：候选存在的那一刻，用户的注意力本就在选词上，而选中或清空都会立刻收起
/// 浮层。滚轮那条也正是 `MAX_CANDIDATES` 收到 7 的理由之一——列表短到不需要滚。
fn candidate_panel(st: Rc<State>) -> Element {
    let candidates = st.candidates;
    Element::col()
        .width_match()
        // 浮层要**不透明**才盖得住下面的正文。用 Surface 而非 Bg：浮层比底板高一层，
        // 三套皮肤里 surface 都比 bg 略亮（深色皮肤则略浅），正好表达这层高度差。
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(12.0)
        .padding(6)
        // 投影是「浮起来」的唯一视觉依据——没有它，浮层和被盖住的正文会糊成一片。
        .shadow(Shadow::new(0.0, 6.0, 18.0, Color::rgba(0, 0, 0, 38)))
        // `host_signal` 而非 `list_signal`：后者是 `scroll().fill()`，高度只能写死，
        // 于是候选少时浮层是个大半截空着的盒子。`host_signal` 是普通 col，高度随内容
        // ——候选几条，浮层就多高。
        //
        // 它的容器虽是 `col().fill()`（高度 `Match`），但线性布局会把**主轴上的
        // `Match` 降级为 `Wrap`**（`core.rs:547-552`，「避免单个子独占整条主轴」），
        // 故在竖向父容器里高度确实随内容。这是框架写死并带回归测试的规则，不是巧合。
        //
        // 上界全靠 `MAX_CANDIDATES` 收住——浮层自己不设限高。
        // 绑重建计数而非 `candidates` 本身：行要知道自己的下标才能比对键盘游标，而
        // windui 的列表回调只给 item 不给位置。改用「单元素 Vec 当触发器」这个手法
        // （设置页同款，见 `bump`），一次回调里自己 `enumerate` 整张表。
        //
        // 游标一动也走这条重建：高亮底是构建期定的，只改信号不重建，高亮不会动。
        // 候选最多 7 条，重建成本可忽略。
        .child(Element::host_signal(st.cand_rev, move |_rev: u64| {
            let mut col = Element::col().width_match();
            let at = st.cursor.get();
            for (i, c) in st.candidates.get().into_iter().enumerate() {
                col = col.child(candidate_row(c, i == at, st.clone()));
            }
            col
        }))
        // 没有候选时**整块收起**。`visible_when` 会让 `measure` 直接返回 `Size::ZERO`
        // （windui `core.rs` 的 measure 开头就短路），故是真的不占位，不是画成透明。
        .visible_when(move || !candidates.get().is_empty())
}

/// 候选浮层的一行：词头 + 释义摘要。
///
/// 不用 `Element::nav_row`——它是「带 chevron 的钻入行」（windui `ui/nav.rs`），而 `›`
/// 的语义是「进到下一层去」。点候选并不进入任何子页，只是把词填进查询框并查询，人还在
/// 原地。这与 `side_row` 那里拒绝 `nav_row` 是同一条理由，此处此前漏了。
fn candidate_row(c: Candidate, at_cursor: bool, st: Rc<State>) -> Element {
    let word = c.headword.as_str().to_string();
    let pick = word.clone();
    let mut row = Element::row()
        .width_match()
        .height(38)
        .cross(Align::Center)
        .corner(9.0)
        .padding_xy(12, 0)
        .spacing(12)
        .clickable()
        .on_click(move |_ctx| {
            // 选中候选 = 查询词确定 → 此刻才查询源出场，且浮层收起。
            st.select(&pick);
        });
    // 键盘游标所在的那条。与侧栏选中行同一套视觉（强调色淡底），因为回答的是同一个
    // 问题：「回车会选中哪一条」。
    if at_cursor {
        row = row.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
    }
    row = row.child(
        Element::label(word)
            .font_size(14.0)
            .font_weight(if at_cursor { 600 } else { 500 })
            .fg_role(Role::Text)
            // 定宽让释义摘要对齐成一栏。词头长短不一（`make` 与 `makeshift` 差一倍），
            // 不定宽的话每行的摘要起点各不相同，一列候选读下来是锯齿状的——而候选列表
            // 的用法正是**竖着快速扫**，对不齐直接抵消它的价值。
            //
            // 超长词头照常把摘要推开，不截断：认出这是不是我要的那个词，靠的是词头本身。
            .min_width(120),
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

/// 结果区：提示 + 词头卡片。
fn result_area(st: Rc<State>) -> Element {
    let cards = st.cards;
    Element::col()
        .fill()
        .spacing(6)
        .child(
            Element::label_signal(st.hint)
                .fg_role(Role::TextMuted)
                .height(20)
                .width_match(),
        )
        .child(scroll_area(
            Element::host_signal(cards, move |c: Card| card_view(c, st.clone())).width_match(),
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
                Element::label(hw.to_string())
                    .font_size(HEADWORD_SIZE)
                    .font_family(SERIF)
                    .fg_role(Role::Text)
                    .weight(1.0),
            )
            .child(star(c.fav, hw.clone(), st.clone())),
    );
    // 词头区与释义区之间的分隔线，与设计稿一致。两者是不同层次的信息——上面回答
    // 「这是哪个词」，下面回答「它什么意思」，一条线比单纯拉开间距更能说明这件事。
    col = col.child(Element::divider());
    for (i, e) in c.entries.into_iter().enumerate() {
        // 只查表，不新建：本函数跑在重建作用域内，在这里 `signal()` 出来的句柄活不过
        // 下一次重建。信号由 `rebuild_cards` 预先备好，详见 `ExpandedStates`。
        let expanded = st
            .expanded
            .get(&expand_key(&hw, i), st.settings.borrow().expand_en);
        col = col.child(entry_view(e, expanded));
    }
    // 备注排在最后：它是用户**附加**给这个词的东西，不该插进词典自身的内容里打断
    // 「词头 → 音标 → 释义」这条阅读顺序。只在已收藏时出现——没收藏就没有可附着的
    // 书签。
    if c.fav {
        col = col.child(note_field(hw, st));
    }
    col
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
fn entry_view(e: Entry, expanded: Signal<bool>) -> Element {
    match e {
        Entry::English(x) => {
            let mut col = Element::col().spacing(8).width_match();
            // 音标与词性同一行：它们都是「这个词是什么」的元信息，与释义分属两层。
            if x.phonetic.is_some() || x.pos.is_some() {
                let mut meta = Element::row()
                    .cross(Align::Center)
                    .spacing(12)
                    .width_match();
                if let Some(p) = &x.phonetic {
                    meta = meta.child(
                        Element::label(format!("[{p}]"))
                            .font_size(15.0)
                            .fg_role(Role::TextMuted),
                    );
                }
                // 词性用衬线 + 强调色。设计稿此处是斜体，而 windui 没有斜体 API
                // （`font_style`/`italic` 全无），故改由字族与颜色承载这份身份。
                if let Some(pos) = &x.pos {
                    meta = meta.child(
                        Element::label(pos.clone())
                            .font_size(15.0)
                            .font_family(SERIF)
                            .fg_role(Role::Accent),
                    );
                }
                col = col.child(meta);
            }
            // 词形变化：made / making / makes 这些数据一直躺在库里，界面上却一个字
            // 都没有。ADR-0001 当初选 ECDICT，理由之一正是它自带 `exchange`——只用在
            // 查询路径（查 tried 跟随到 try）而不展示，等于这份数据只兑现了一半。
            if !x.inflections.derived.is_empty() {
                col = col.child(inflection_row(&x.inflections.derived));
            }
            // 中文释义按词性分节呈现。整块塞进一个 label 会让 `vt.` `n.` 沦为混在
            // 中文里的普通字符，所有信息挤在同一层次——那正是「看着像一坨文本」的来源。
            if let Some(zh) = &x.zh_definition {
                for g in crate::domain::parse_glosses(zh) {
                    col = col.child(gloss_row(&g));
                }
            }
            // 英英释义默认折叠，用户主动展开才可见——这是刻意的产品决定，非偷懒。
            if let Some(en) = &x.en_definition {
                col = col.child(Element::collapsible(
                    "英英释义",
                    expanded,
                    // 这一段单独限宽，理由见 `EN_DEF_MAX_W`：整屏正文里只有它是成句的
                    // 英文散文，行长失控的风险是真的。
                    Element::label(en.clone())
                        .width_match()
                        .max_width(EN_DEF_MAX_W)
                        .line_height(BODY_LH),
                ));
            }
            col
        }
        Entry::Chinese(x) => {
            let mut col = Element::col().spacing(8).width_match();
            // 拼音是中文词条的「音标」，与英汉分支同一层级，故用同一档字号。
            col = col.child(
                Element::label(format!("[{}]", x.pinyin))
                    .font_size(15.0)
                    .fg_role(Role::TextMuted)
                    .width_match(),
            );
            // 繁体与词头不同才展示——相同时显示两遍是噪音。
            if x.traditional != x.headword.as_str() {
                col = col.child(
                    Element::label(format!("繁体：{}", x.traditional))
                        .fg_role(Role::TextMuted)
                        .height(20)
                        .width_match(),
                );
            }
            // 英文释义按义项分行。`;` 分隔的是同一义项的不同措辞，不另起一行。
            for (i, s) in x.senses.iter().enumerate() {
                col = col.child(
                    Element::label(format!("{}. {}", i + 1, join(s)))
                        .font_size(18.0)
                        .font_weight(500)
                        .line_height(BODY_LH)
                        .width_match(),
                );
            }
            if !x.classifiers.is_empty() {
                col = col.child(
                    Element::label(format!("量词：{}", x.classifiers.join("、")))
                        .fg_role(Role::TextMuted)
                        .height(20)
                        .width_match(),
                );
            }
            col
        }
    }
}

/// 一组同词性的释义：词性胶囊 + 释义正文。
fn gloss_row(g: &crate::domain::Gloss) -> Element {
    let mut row = Element::row().width_match().cross(Align::Start).spacing(10);
    if let Some(pos) = &g.pos {
        row = row.child(pos_chip(pos));
    }
    row.child(
        // 释义之间用顿号而非原文的逗号：中文并列用顿号，且与释义内部可能出现的
        // 逗号区分得开。
        Element::label(g.senses.join("、"))
            .font_size(18.0)
            .font_weight(500)
            .line_height(BODY_LH)
            .weight(1.0),
    )
}

/// 词性胶囊。
///
/// 改成淡底无边框，并与词形变化那排（`inflection_row`）用同一套底色与圆角——两者都是
/// 「挂在释义旁边的小标记」，此前一个描边、一个填底，同屏并列时像两套不相干的控件。
///
/// **定宽**：`vt.` `vi.` `n.` `adj.` 宽度各不相同，不定宽的话每一节的释义正文起始位置
/// 都错开，读下来左边缘是锯齿状的。给一个够装 `adj.` 的下限，释义便对齐成一栏。
///
/// 保留强调色：词性是查词典时的主要扫视目标（「我要的是动词那一条」），它值得一个
/// 与正文不同的颜色。去掉的只是衬线——那份「典籍感」由词头独自承担就够了，散在
/// 小胶囊上只是把界面搅得字族杂乱。
fn pos_chip(pos: &str) -> Element {
    Element::label(pos)
        .font_size(12.5)
        .font_weight(600)
        .fg_role(Role::Accent)
        .bg_role(Role::SurfaceAlt)
        .corner(6.0)
        .padding_xy(8, 4)
        .min_width(42)
        .text_align(Align::Center)
        // 往下挪 4px 与释义首行对齐。
        //
        // 释义带 `BODY_LH`（1.7）行高，18px 的字排在一个约 31px 的行盒里、上下各留一段
        // 空隙；胶囊只有约 23px 高且没有行高加成。父行是 `Align::Start`，两者**顶边**
        // 对齐，于是胶囊的视觉中心比释义首行的高出约 4px——看起来像浮在字的上方。
        // 差值补在这里，而不是去动 `cross`：改成 `Center` 会让多行释义把胶囊拽到段落
        // 正中，那比偏上更糟。
        .margin_xy(0, 4)
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

/// 义项内的多种措辞用 `;` 连回去——它们是同一含义的不同说法，不是不同义项。
fn join(s: &Sense) -> String {
    s.glosses.join("; ")
}

#[cfg(test)]
mod tests {
    use super::{
        expand_key, group_by_headword, headwords_to_record, scroll_area, signal, write_note,
        ExpandedStates, Role,
    };
    use crate::domain::{ChineseEntry, EnglishEntry, Entry, Headword, Inflections, Sense};

    fn 英汉(词头: &str) -> Entry {
        Entry::English(EnglishEntry {
            headword: Headword::from_store(词头),
            phonetic: None,
            zh_definition: None,
            en_definition: None,
            pos: None,
            inflections: Inflections::default(),
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
