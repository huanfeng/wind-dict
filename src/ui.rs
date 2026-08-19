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
            return;
        }

        // 补全**永远由离线词典驱动**，与用户选中哪个查询源无关——补全需要词表，
        // 而词表只有词典有（译源没有词库，不知道世上存在哪些词）。见术语表「补全」。
        let list = self
            .dict
            .complete(&text, MAX_CANDIDATES)
            .unwrap_or_default();
        self.candidates.set(list);
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

/// 词头字号。比正文大一个数量级——词头是这一屏的主角，其余都是它的注解。
const HEADWORD_SIZE: f32 = 42.0;

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

/// 正文最大宽度。
///
/// 行太长时，眼睛从行尾回到下一行行首容易串行，长段落尤其明显。窗口拉宽后主列
/// 会一直变宽，故需要一个上界把正文收在舒适的行长内——多出来的宽度宁可留白。
const BODY_MAX_W: i32 = 640;

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
            self.st
                .settings_note
                .set("热键的主键请填一个字母或数字".into());
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

/// 界面状态。
struct State {
    dict: Rc<OfflineDictionary>,
    /// 用户数据（收藏与历史）。不可用时保留**原因**，由 `unavailable_bar` 展示。
    user: UserDataState,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
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
    /// 热键句柄：改键即 `rebind`，下一次消息循环生效。
    hotkey: HotkeyHandle,
    /// 主列当前页：词典 / 设置。
    page: Signal<usize>,
    /// 当前设置。界面上的各个控件绑到它的分量信号，改动经 `save_settings` 落库。
    settings: RefCell<Settings>,
    /// 设置页的即时反馈（保存失败、需重启生效等）。空串 = 无。
    settings_note: Signal<String>,
    /// 「清空历史」是否已进入确认态。
    ///
    /// 用两步确认而非弹模态框：清空不可撤销，但为它拉起一个系统模态框在常驻小工具上
    /// 过重；而「点一次变成『确认清空』，再点才真清」既拦得住误触，又不打断心流。
    confirm_clear: Signal<bool>,
    /// 设置页的重建计数。
    ///
    /// 设置页上有若干**构建时求值**的显示——皮肤卡片的选中环、词库路径文字。它们
    /// 不是控件自带的状态，改了设置若不重建就会停在旧值上（选中环留在原来那张卡片，
    /// 是最显眼的一处）。故设置一变就整页重建，而不是逐处想办法局部刷新：设置页的
    /// 重建成本可以忽略，而「有的刷了有的没刷」是很难查的那种不一致。
    settings_rev: Signal<Vec<u64>>,
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
/// 单独成一个类型而非直接摊在 `State` 里，是为了能脱开词库单独验证——它的正确性
/// 全在「同一个键必须拿到同一个信号」这一条上，而那与词典数据无关。
#[derive(Default)]
struct ExpandedStates(RefCell<HashMap<String, Signal<bool>>>);

impl ExpandedStates {
    /// 取某个键的展开态，没有则按 `default_open` 新建。**同一键永远返回同一个信号**。
    ///
    /// 默认值由调用方给（来自设置里的「默认展开英英释义」），而不是写死 false——
    /// 但只对**尚未出现过**的键生效：用户手动折叠过的，不该因为改了设置又被展开。
    fn get(&self, key: &str, default_open: bool) -> Signal<bool> {
        if let Some(s) = self.0.borrow().get(key) {
            return *s;
        }
        let s = signal(default_open);
        self.0.borrow_mut().insert(key.to_string(), s);
        s
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
        let cards = group_by_headword(entries)
            .into_iter()
            .map(|(hw, entries)| Card {
                fav: self.is_favorite(&hw),
                headword: hw,
                entries,
            })
            .collect();
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
    fn bump_settings(&self) {
        let n = self.settings_rev.get().first().copied().unwrap_or(0);
        self.settings_rev.set(vec![n.wrapping_add(1)]);
    }

    fn save_settings(&self) -> bool {
        let UserDataState::Ready(u) = &self.user else {
            self.settings_note
                .set("设置无法保存：用户数据未能打开".into());
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
                self.settings_note.set(format!("保存设置失败：{e}"));
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
        if self.save_settings() {
            self.settings_note.set(String::new());
        } else {
            self.settings_note
                .set("皮肤已切换，但未能保存，重启后会回到原来的皮肤".into());
        }
    }

    /// 改唤起热键。**立即生效**：`rebind` 下一次消息循环向系统换注册。
    ///
    /// 拦住无修饰键：那会吞掉该字母在**所有程序**里的输入，用户按一下 D 就唤起词典，
    /// 等于没法打字了——而这个错误一旦犯下，用户很难意识到是词典干的。
    fn set_hotkey(&self, spec: crate::settings::HotkeySpec) {
        if !spec.has_modifier() {
            self.settings_note.set(
                "热键至少要带一个 Ctrl / Alt / Shift，否则会吞掉该键在所有程序里的输入".into(),
            );
            return;
        }
        self.settings.borrow_mut().hotkey = spec;
        self.hotkey.set(spec.to_hotkey());
        if self.save_settings() {
            self.settings_note.set(format!("唤起热键已改为 {spec}"));
        }
    }

    /// 开关开机自启。
    ///
    /// 先写注册表再落库：注册表才是**真相**（用户可能在别处删掉自启项），库里存的
    /// 只是界面初值。反过来先落库的话，注册表写失败时库里就留下了一个假状态。
    fn set_autostart(&self, on: bool) -> bool {
        if let Err(e) = crate::autostart::set(on) {
            self.settings_note.set(format!("设置开机启动失败：{e}"));
            return false;
        }
        self.settings.borrow_mut().autostart = on;
        if !self.save_settings() {
            // 注册表已改、库没写上。真实状态仍是对的（`autostart_now` 以注册表为准），
            // 只是这次没能记进库里——如实报出，不谎称成功。
            return false;
        }
        self.settings_note.set(String::new());
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
        self.settings_note.set(String::new());
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
                self.settings_note
                    .set(format!("这个文件不能用作词库：{e:#}"));
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
            self.settings_note.set(if has {
                "词库已更改，重启后生效".into()
            } else {
                "已恢复默认词库路径，重启后生效".into()
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
            self.settings_note.set("用户数据未能打开，无法清空".into());
            return;
        };
        match u.clear_history() {
            Ok(()) => {
                self.settings_note.set("历史记录已清空".into());
                self.bump();
            }
            Err(e) => self.settings_note.set(format!("清空历史失败：{e}")),
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
        settings: RefCell::new(settings),
        settings_note: signal(String::new()),
        confirm_clear: signal(false),
        settings_rev: signal(vec![0]),
    });
    // 开屏即列出历史：侧栏空着会让人以为功能坏了。
    st.reload_side();

    // 无系统标题栏：整窗都是客户区，故顶部这条标题栏由我们自己画（见 `title_bar`）。
    Element::col()
        .fill()
        .bg_role(Role::Bg)
        .child(title_bar())
        .child(Element::divider())
        .child(body(st, unavailable).weight(1.0))
}

/// 自定义标题栏：应用标识 + 窗口按钮。
///
/// 整条 `window_drag()` 可拖动窗口；落在窗口按钮上不拖、正常点击（windui 按「命中
/// 可聚焦控件则不拖」处理）。
fn title_bar() -> Element {
    Element::row()
        .width_match()
        .height(38)
        .cross(Align::Stretch)
        // 标题栏底走 `SurfaceAlt` 而非皮肤里那个具体色：三套皮肤的 `titlebar` 恰好
        // 都等于 `surface_alt`，用角色表达之后换肤能自动跟随，不必重建元素树。
        .bg_role(Role::SurfaceAlt)
        .window_drag()
        .child(brand().weight(1.0))
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

/// 标题栏左侧的应用标识：图标 + 名称 + 能力副标题。
fn brand() -> Element {
    Element::row()
        .cross(Align::Center)
        .spacing(9)
        .padding_xy(14, 0)
        .child(
            // 图标：强调色圆角块 + 一个「词」字。用汉字而非拉丁字母首字母，
            // 因为这是个中英双向的词典，汉字比 "W" 更说明它是什么。
            //
            // 居中靠**外层容器**而非 `text_align`：后者只管水平（见 windui
            // `Element::text_align` 的文档），单靠它会让 12px 的字顶在 20px 块的
            // 上沿。这是 windui 自己 `badge_intent` 的写法。
            Element::row()
                .cross(Align::Center)
                .size(20, 20)
                .bg_role(Role::Accent)
                .corner(5.0)
                .child(
                    Element::label("词")
                        .font_size(12.0)
                        .font_weight(600)
                        .fg_role(Role::OnAccent)
                        .width_match()
                        .text_align(Align::Center),
                ),
        )
        .child(
            Element::label("wind-dict")
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

/// 主体区域：左侧栏 + 右主列。
///
/// 底色由 `build` 的根容器统一铺，此处不再铺一遍——同一块区域填两次纯色是白费。
fn body(st: Rc<State>, unavailable: Option<String>) -> Element {
    Element::row()
        .fill()
        .cross(Align::Stretch)
        // 栏间那条线是侧栏**自己的右边框**，不再是一个独立节点：单边边框不参与
        // 布局，而 1px 色块要占一列，容器一改间距就得跟着调。
        .child(sidebar(st.clone()))
        .child(pages(st, unavailable).weight(1.0))
}

/// 侧栏：历史 / 收藏两个页签 + 列表。
///
/// 设计稿的侧栏底部还有一个「设置」入口，此处**刻意不做**——设置页是下一阶段的东西，
/// 现在放个按钮上去，点了没有任何反应。宁可先没有入口，也不放一个骗人的。
fn sidebar(st: Rc<State>) -> Element {
    Element::col()
        .width(224)
        .height_match()
        .bg_role(Role::SurfaceAlt)
        .border_role(Role::Divider, 1)
        .border_edges(Edges::RIGHT)
        // 列表驱动器：零尺寸、不可见，须先于列表注册（on_update 按注册顺序广播，
        // 而注册顺序是 `Element::build` 的深度优先前序，即书写顺序）。
        //
        // 还有一层**跨子树**的依赖：`SideLoader` 也会重建结果区的卡片，故它必须先于
        // 主列的 `cards` 列表注册，否则卡片会慢一帧。当前成立仅仅因为 `body` 里侧栏
        // 排在主列之前——**若把侧栏挪到右边，这里会静默退化**，届时需把驱动器提到
        // `body` 层级，而不是靠左右顺序碰巧对。
        .child(
            Element::leaf()
                .reactive()
                .widget(SideLoader {
                    last_tab: st.side_tab.version(),
                    last_rev: st.revision.version(),
                    st: st.clone(),
                })
                .height(0),
        )
        .child(
            Element::tabs(
                st.side_tab,
                vec![
                    ("历史", side_list(st.clone())),
                    // 术语表弃用「生词本」，见 `TAB_HISTORY` 处的说明。
                    ("收藏", side_list(st.clone())),
                ],
            )
            .fill()
            .padding_xy(8, 8)
            .weight(1.0),
        )
        // 设置入口。此前刻意不做，因为「点了没地方去」；现在设置页有了，它才该出现。
        .child(settings_entry(st))
}

/// 侧栏底部的设置入口。
fn settings_entry(st: Rc<State>) -> Element {
    let page = st.page;
    Element::row()
        .width_match()
        .height(46)
        .cross(Align::Center)
        .padding_xy(16, 0)
        .spacing(10)
        .border_role(Role::Divider, 1)
        .border_edges(Edges::TOP)
        .clickable()
        .on_click(move |_ctx| page.set(PAGE_SETTINGS))
        .child(
            Element::label("设置")
                .font_size(13.0)
                .font_weight(500)
                .fg_role(Role::Text)
                .weight(1.0),
        )
        // 收藏计数：设计稿此处是「142 词」。它不只是装饰——收藏是慢慢攒起来的，
        // 一个数字能让人知道自己攒了多少，也顺带说明这个入口后面有内容。
        .child(
            Element::label(format!("{} 词", st.counts().1))
                .font_size(12.0)
                .fg_role(Role::TextDisabled),
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

/// 侧栏列表。两个页签共用一份数据信号。
///
/// 这样不会串，但**理由不是「只有一个页签存在」**：`Element::tabs` 把两页都建进树，
/// 只给未选中的挂 `visible_when`；而 `on_update` 的派发只看 `enabled` 不看 `visible`，
/// 所以隐藏那一页的列表照样在重建。不串的真实理由是两页绑同一信号、内容本就相同。
///
/// 代价是每次列表变更有一倍的节点重建。可接受，但若将来两个页签要显示不同内容，
/// 必须先解决这件事——那时「共用一份信号」这个前提本身就没了。
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
        Element::icon_button("×")
            .fg_role(Role::TextDisabled)
            .on_click(move |_ctx| del_st.remove_side_row(&del_hw)),
    )
}

/// 主列：查询框 + 补全候选 + 结果。
fn main_column(st: Rc<State>, unavailable: Option<String>) -> Element {
    // 左右 28px 而非 16：查询框与词条正文都靠这份留白与侧栏拉开距离。设计稿此处是
    // 40px，但那是在更宽的画布上；920px 窗口减去侧栏后按 28 收，观感相当。
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
        // 50px 高、12 圆角：查询框是这一屏的主控件，与设计稿一致地给足分量。
        .child(query_box(st.clone()))
        .child(notice_bar(st.notice))
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
                    Element::row()
                        .size(32, 32)
                        .cross(Align::Center)
                        .corner(8.0)
                        .clickable()
                        .on_click(move |_ctx| {
                            // 离开即撤销确认态：回来时不该还举着「确认清空」等人误触。
                            confirm.set(false);
                            page.set(PAGE_DICT);
                        })
                        .child(
                            Element::label("←")
                                .width_match()
                                .text_align(Align::Center)
                                .font_size(18.0)
                                .fg_role(Role::Text),
                        ),
                )
                .child(
                    Element::label("设置")
                        .font_size(22.0)
                        .font_family(SERIF)
                        .fg_role(Role::Text),
                ),
        )
        .child(
            Element::scroll()
                .fill()
                .child(Element::host_signal(st.settings_rev, move |_rev: u64| {
                    settings_body(st.clone())
                })),
        )
}

/// 设置页正文。每次 `settings_rev` 变动整体重建，故其中的构建时求值（选中环、
/// 词库路径）总是新鲜的。
fn settings_body(st: Rc<State>) -> Element {
    Element::col()
        .width_match()
        .max_width(620)
        .padding_xy(40, 26)
        .spacing(28)
        .child(notice_bar(st.settings_note))
        .child(group("外观", skin_cards(st.clone())))
        .child(group("唤起", hotkey_row(st.clone())))
        .child(group("启动", autostart_row(st.clone())))
        .child(group("词库", dict_rows(st.clone())))
        .child(group("释义显示", expand_en_row(st.clone())))
        .child(group("数据", data_rows(st)))
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
            Element::label("在侧栏逐条取消")
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
fn notice_bar(notice: Signal<String>) -> Element {
    Element::label_signal(notice)
        .font_size(13.0)
        .fg_role(Role::Danger)
        .width_match()
        .visible_when(move || !notice.get().is_empty())
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
/// 按钮做成与输入框等高（50px）的行，`Align::End` 在纵向偏移为零，横向贴右——
/// 「右端居中」就是这么凑出来的，不是框架直接支持的对齐方式。
fn query_box(st: Rc<State>) -> Element {
    let query = st.query;
    Element::stack()
        .width_match()
        .child(
            Element::text_input(st.query, "输入中文或英文…")
                .width_match()
                .height(50)
                .corner(12.0)
                .font_size(16.0),
        )
        .child(
            Element::row()
                .height(50)
                .cross(Align::Center)
                .padding_xy(12, 0)
                .align(Align::End)
                .child(
                    Element::icon_button("×")
                        .fg_role(Role::TextDisabled)
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
        .child(Element::host_signal(st.candidates, move |c: Candidate| {
            candidate_row(c, st.clone())
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
fn candidate_row(c: Candidate, st: Rc<State>) -> Element {
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
        })
        .child(
            Element::label(word)
                .font_size(14.0)
                .font_weight(500)
                .fg_role(Role::Text),
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
        .child(
            Element::scroll().fill().child(
                Element::host_signal(cards, move |c: Card| card_view(c, st.clone()))
                    .width_match()
                    // 正文限宽：窗口拉得再宽，行长也收在可读范围内，多出来的
                    // 宽度留白。限宽在测量前生效，故释义是**在 640 内换行**，
                    // 不是排完再裁。
                    .max_width(BODY_MAX_W),
            ),
        )
}

/// 一张词头卡片：大字词头 + 收藏星标 + 该词头下的全部词条。
fn card_view(c: Card, st: Rc<State>) -> Element {
    let hw = c.headword.clone();
    // 卡片之间留出一个身位，靠间距而非分隔线区分——多个词头时才看得出边界。
    let mut col = Element::col().spacing(10).width_match().padding_xy(0, 10);
    col = col.child(
        Element::row()
            // 顶对齐而非居中：星标是 42px 方块，与 42px 的词头居中对齐会让它掉到
            // 词头视觉重心之下（词头有下伸部，实际占位高于字面）。
            .cross(Align::Start)
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
        // 键带序号：同一词头下可能有多条词条（多音字），各自的折叠区应能分别开合。
        let expanded = st
            .expanded
            .get(&format!("{hw}#{i}"), st.settings.borrow().expand_en);
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
            Element::text_input(text, "备注…")
                .font_size(13.0)
                .width_match()
                .weight(1.0),
        )
}

/// 收藏星标。实心 = 已收藏。
fn star(fav: bool, hw: Headword, st: Rc<State>) -> Element {
    // 42×42 的带边框方块，而非一个裸图标：收藏是这一屏唯一的写操作，给它一个明确的
    // 可点区域。已收藏时填淡底 + 强调色实心星，未收藏是空心星，两态一眼可辨。
    let mut btn = Element::row()
        .size(42, 42)
        .cross(Align::Center)
        .corner(11.0)
        .border_role(if fav { Role::Accent } else { Role::Border }, 1)
        .clickable()
        .on_click(move |_ctx| st.toggle_favorite(&hw));
    if fav {
        btn = btn.bg_role_alpha(Role::Accent, ACCENT_SOFT_A);
    }
    btn.child(
        Element::label(if fav { "★" } else { "☆" })
            .font_size(19.0)
            .width_match()
            .text_align(Align::Center)
            .fg_role(if fav {
                Role::Accent
            } else {
                Role::TextDisabled
            }),
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
                    Element::label(en.clone())
                        .width_match()
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
fn pos_chip(pos: &str) -> Element {
    Element::label(pos)
        .font_size(12.5)
        .font_weight(600)
        .font_family(SERIF)
        .fg_role(Role::Accent)
        .border_role(Role::Border, 1)
        .corner(6.0)
        .padding_xy(8, 3)
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
    use super::{group_by_headword, headwords_to_record, ExpandedStates};
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
        states.get("a#0", false).set(true);
        // 此后即便传入 default_open=false，已有的键也保持原状。
        assert!(states.get("a#0", false).get(), "已存在的键不受默认值影响");
        // 新键才吃默认值。
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
        let a = states.get("make#0", false);
        a.set(true);
        // 模拟卡片重建：重新取一次。
        let b = states.get("make#0", false);
        assert!(b.get(), "重建后展开态应当保持");
    }

    /// 不同词条各自开合，互不影响（多音字一个词头、多条词条）。
    #[test]
    fn 不同键的展开态互不影响() {
        let states = ExpandedStates::default();
        states.get("行#0", false).set(true);
        assert!(states.get("行#0", false).get());
        assert!(!states.get("行#1", false).get(), "另一条词条不该被带着展开");
    }
}
