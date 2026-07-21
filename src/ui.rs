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

use std::rc::Rc;

use windui::core::{EventCtx, Widget};
use windui::prelude::*;

use crate::domain::{Candidate, Dictionary, Entry, Headword, Lookup, Query, Sense, Wordlist};
use crate::skin::Skin;
use crate::source::offline::OfflineDictionary;
use crate::store::userdata::{now_secs, UserDataState};

/// 补全候选数量上限。
///
/// 单字母前缀（如 `a`）会命中约 5 万行，且 `ORDER BY frq` 用不上索引（索引在
/// `(sw, word)` 上），SQLite 必须全排一遍——实测约 20ms。LIMIT 不减少排序量，
/// 但它是唯一能钳住内存与渲染开销的地方。
const MAX_CANDIDATES: usize = 20;

/// 监视查询词、驱动补全的响应式控件。
///
/// 它不绘制任何东西——只是挂在树上，借 `on_update` 相位工作。**必须先于候选列表
/// 构建**：`on_update` 按注册顺序广播（注册即 `Element::build` 的深度优先顺序），
/// 排在列表之后会让候选慢一帧。
struct Completer {
    dict: Rc<OfflineDictionary>,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
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

        // 补全**永远由离线词典驱动**，与用户选中哪个查询源无关——补全需要词表，
        // 而词表只有词典有（译源没有词库，不知道世上存在哪些词）。见术语表「补全」。
        let text = self.query.get();
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

/// 词头与词性用的衬线字族。
///
/// 词典的专业观感很大程度来自衬线体——正文用无衬线、词头用衬线是纸质词典的惯例。
/// 取 Georgia 是因为它随 Windows 分发、必然存在，且字面宽、小字号也清晰。
///
/// 中文词头它没有字形，会由系统回退到默认中文字体（无衬线）。这是刻意接受的：
/// Windows 自带的中文衬线只有宋体，大字号下笔画细弱、观感陈旧，回退反而更好。
const SERIF: &str = "Georgia";

/// 词头字号。比正文大一个数量级——词头是这一屏的主角，其余都是它的注解。
const HEADWORD_SIZE: f32 = 38.0;

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

/// 界面状态。
struct State {
    dict: Rc<OfflineDictionary>,
    /// 用户数据（收藏与历史）。不可用时保留**原因**，由 `unavailable_bar` 展示。
    user: UserDataState,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
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
}

impl State {
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
pub fn build(dict: OfflineDictionary, user: UserDataState, skin: Skin) -> Element {
    let dict = Rc::new(dict);
    let unavailable = match &user {
        UserDataState::Ready(_) => None,
        UserDataState::Unavailable(why) => Some(why.clone()),
    };
    let st = Rc::new(State {
        dict: dict.clone(),
        user,
        query: signal(String::new()),
        candidates: signal(Vec::new()),
        cards: signal(Vec::new()),
        hint: signal(String::from("输入一个词开始查询")),
        side_tab: signal(TAB_HISTORY),
        side_rows: signal(Vec::new()),
        revision: signal(0),
        notice: signal(String::new()),
    });
    // 开屏即列出历史：侧栏空着会让人以为功能坏了。
    st.reload_side();

    // 无系统标题栏：整窗都是客户区，故顶部这条标题栏由我们自己画（见 `title_bar`）。
    Element::col()
        .fill()
        .bg(skin.theme.palette.bg)
        .child(title_bar(&skin))
        .child(Element::divider())
        .child(body(st, unavailable, &skin).weight(1.0))
}

/// 自定义标题栏：应用标识 + 窗口按钮。
///
/// 整条 `window_drag()` 可拖动窗口；落在窗口按钮上不拖、正常点击（windui 按「命中
/// 可聚焦控件则不拖」处理）。
fn title_bar(skin: &Skin) -> Element {
    Element::row()
        .width_match()
        .height(38)
        .cross(Align::Stretch)
        .bg(skin.titlebar)
        .window_drag()
        .child(brand(skin).weight(1.0))
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
fn brand(skin: &Skin) -> Element {
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
                .fg(skin.text2),
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
fn body(st: Rc<State>, unavailable: Option<String>, skin: &Skin) -> Element {
    Element::row()
        .fill()
        .cross(Align::Stretch)
        .child(sidebar(st.clone(), skin))
        .child(vrule(skin.theme.palette.divider))
        .child(main_column(st, unavailable).weight(1.0))
}

/// 竖直细线。windui 的 `Element::divider()` 只有横向一种，竖向需自己拼。
fn vrule(color: Color) -> Element {
    Element::leaf().width(1).height_match().bg(color)
}

/// 侧栏：历史 / 收藏两个页签 + 列表。
///
/// 设计稿的侧栏底部还有一个「设置」入口，此处**刻意不做**——设置页是下一阶段的东西，
/// 现在放个按钮上去，点了没有任何反应。宁可先没有入口，也不放一个骗人的。
fn sidebar(st: Rc<State>, skin: &Skin) -> Element {
    Element::col()
        .width(224)
        .height_match()
        .bg(skin.panel)
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
                    ("收藏", side_list(st)),
                ],
            )
            .fill()
            .padding_xy(8, 8),
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
        move |r: SideRow| {
            let st = st.clone();
            let word = r.headword.as_str().to_string();
            Element::nav_row(word.clone()).on_click(move |_ctx| {
                st.query.set(word.clone());
                st.lookup(&word);
            })
        },
    )
    .fill()
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
        .child(Element::text_input(st.query, "输入中文或英文…").width_match())
        .child(notice_bar(st.notice))
        .child(candidate_list(st.clone()))
        // 此处原有一条分隔线。拿掉了：候选区收起后它就紧贴查询框，把主列切成两截，
        // 而它要分隔的两样东西（候选、结果）本就不会同时是空的。区域感交给留白。
        .child(result_area(st))
}

/// 需当场告知的消息条。空串时零高度、不占位。
///
/// 与 `unavailable_bar` 分开：那条讲的是启动时就已知的**持续状态**，这条讲的是
/// 刚刚那一次操作的**结果**。混在一起会让用户分不清「一直不能用」和「这次没成」。
fn notice_bar(notice: Signal<String>) -> Element {
    Element::label_rc(notice)
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

/// 候选列表：点一条即「确定查询词」，触发查询。
fn candidate_list(st: Rc<State>) -> Element {
    let candidates = st.candidates;
    Element::list_signal(
        st.candidates,
        |c: &Candidate| c.headword.as_str().to_string(),
        move |c: Candidate| {
            let st = st.clone();
            let word = c.headword.as_str().to_string();
            let label = match &c.preview {
                Some(p) => format!("{}   {}", c.headword, p),
                None => c.headword.to_string(),
            };
            Element::nav_row(label).on_click(move |_ctx| {
                // 选中候选 = 查询词确定 → 此刻才查询源出场。
                st.query.set(word.clone());
                st.lookup(&word);
            })
        },
    )
    .height(160)
    .width_match()
    // 没有候选时**整块收起**，而不是留一条 160px 的空白带。
    //
    // `visible_when` 会让 `measure` 直接返回 `Size::ZERO`（windui `core.rs` 的
    // measure 开头就短路），故这里是真的不占位，不是画成透明。开屏时这一条空白
    // 曾占去窗口近四成高度，是界面显得空荡的主要来源。
    .visible_when(move || !candidates.get().is_empty())
}

/// 结果区：提示 + 词头卡片。
fn result_area(st: Rc<State>) -> Element {
    let cards = st.cards;
    Element::col()
        .fill()
        .spacing(6)
        .child(
            Element::label_rc(st.hint)
                .fg_role(Role::TextMuted)
                .height(20)
                .width_match(),
        )
        .child(
            Element::scroll()
                .fill()
                .child(Element::host_signal(cards, move |c: Card| {
                    card_view(c, st.clone())
                })),
        )
}

/// 一张词头卡片：大字词头 + 收藏星标 + 该词头下的全部词条。
fn card_view(c: Card, st: Rc<State>) -> Element {
    let hw = c.headword.clone();
    // 卡片之间留出一个身位，靠间距而非分隔线区分——多个词头时才看得出边界。
    let mut col = Element::col().spacing(10).width_match().padding_xy(0, 10);
    col = col.child(
        Element::row()
            .cross(Align::Center)
            .width_match()
            .child(
                Element::label(hw.to_string())
                    .font_size(HEADWORD_SIZE)
                    .font_family(SERIF)
                    .fg_role(Role::Text)
                    .weight(1.0),
            )
            .child(star(c.fav, hw, st)),
    );
    for e in c.entries {
        col = col.child(entry_view(e));
    }
    col
}

/// 收藏星标。实心 = 已收藏。
fn star(fav: bool, hw: Headword, st: Rc<State>) -> Element {
    Element::icon_button(if fav { "★" } else { "☆" })
        .fg_role(if fav { Role::Accent } else { Role::TextMuted })
        .on_click(move |_ctx| st.toggle_favorite(&hw))
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
fn entry_view(e: Entry) -> Element {
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
            // 中文释义为主，默认展示（ECDICT 的 translation 字段）。放大一档：它是
            // 用户查词时真正要读的那行字，不该与音标、量词等注解同等大小。
            if let Some(zh) = &x.zh_definition {
                col = col.child(Element::label(zh.clone()).font_size(16.0).width_match());
            }
            // 英英释义默认折叠，用户主动展开才可见——这是刻意的产品决定，非偷懒。
            if let Some(en) = &x.en_definition {
                col = col.child(Element::collapsible(
                    "英英释义",
                    signal(false),
                    Element::label(en.clone()).width_match(),
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
                        .font_size(16.0)
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

/// 义项内的多种措辞用 `;` 连回去——它们是同一含义的不同说法，不是不同义项。
fn join(s: &Sense) -> String {
    s.glosses.join("; ")
}

#[cfg(test)]
mod tests {
    use super::{group_by_headword, headwords_to_record};
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

    /// 一无所获时没有卡片——空列表而非一张空卡片。
    #[test]
    fn 一无所获没有卡片() {
        assert!(group_by_headword(&[]).is_empty());
    }
}
