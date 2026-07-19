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

/// 界面状态。
struct State {
    dict: Rc<OfflineDictionary>,
    /// 用户数据（收藏与历史）。不可用时保留**原因**，由 `unavailable_bar` 展示
    /// ——目前只展示历史记录失效，收藏有入口后一并纳入。
    user: UserDataState,
    query: Signal<String>,
    candidates: Signal<Vec<Candidate>>,
    /// 当前展示的词条。空 = 尚未查询或一无所获。
    entries: Signal<Vec<Entry>>,
    /// 结果区的提示文案（未收录、请输入等）。
    hint: Signal<String>,
}

impl State {
    /// 执行查询。**只在查询词确定时调用**（选中候选），不逐键触发——见术语表「补全」：
    /// 补全回答「我想拼的是哪个词」，查询回答「这个词什么意思」，是两个动作。
    fn lookup(&self, word: &str) {
        let Some(q) = Query::new(word) else {
            self.entries.set(Vec::new());
            self.hint.set("输入一个词开始查询".into());
            return;
        };
        match self.dict.lookup(&q) {
            Err(e) => {
                self.entries.set(Vec::new());
                self.hint.set(format!("词库读取失败：{e}"));
            }
            Ok(Lookup::NotFound) => {
                self.entries.set(Vec::new());
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
                self.entries.set(entries);
            }
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
        entries: signal(Vec::new()),
        hint: signal(String::from("输入一个词开始查询")),
    });

    // 无系统标题栏：整窗都是客户区，故顶部这条标题栏由我们自己画（见 `title_bar`）。
    Element::col()
        .fill()
        .bg(skin.theme.palette.bg)
        .child(title_bar(&skin))
        .child(Element::divider())
        .child(body(st, unavailable).weight(1.0))
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

/// 主体区域。
///
/// 底色由 `build` 的根容器统一铺，此处不再铺一遍——同一块区域填两次纯色是白费。
fn body(st: Rc<State>, unavailable: Option<String>) -> Element {
    let mut root = Element::col().fill().padding(16).spacing(10);
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
        .child(Element::text_input(st.query, "输入中文或英文…").width_match())
        .child(candidate_list(st.clone()))
        .child(Element::divider())
        .child(result_area(st))
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
}

/// 结果区：提示 + 词条。
fn result_area(st: Rc<State>) -> Element {
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
                .child(Element::host_signal(st.entries, entry_view)),
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
fn entry_view(e: Entry) -> Element {
    match e {
        Entry::English(x) => {
            let mut col = Element::col().spacing(4).width_match().padding(8);
            let head = match &x.phonetic {
                Some(p) => format!("{}  [{}]", x.headword, p),
                None => x.headword.to_string(),
            };
            col = col.child(
                Element::label(head)
                    .font_size(18.0)
                    .fg_role(Role::Text)
                    .height(26)
                    .width_match(),
            );
            // 中文释义为主，默认展示（ECDICT 的 translation 字段）。
            if let Some(zh) = &x.zh_definition {
                col = col.child(Element::label(zh.clone()).width_match());
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
            let mut col = Element::col().spacing(4).width_match().padding(8);
            col = col.child(
                Element::label(format!("{}  [{}]", x.headword, x.pinyin))
                    .font_size(18.0)
                    .fg_role(Role::Text)
                    .height(26)
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
                col = col.child(Element::label(format!("{}. {}", i + 1, join(s))).width_match());
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
    use super::headwords_to_record;
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
}
