//! 界面：热键唤起 → 输入 → 实时补全 → 选中查词。
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

use crate::domain::{Candidate, Dictionary, Entry, Lookup, Query, Sense, Wordlist};
use crate::source::offline::OfflineDictionary;

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
                self.entries.set(entries);
            }
        }
    }
}

/// 构建界面。
pub fn build(dict: OfflineDictionary) -> Element {
    let dict = Rc::new(dict);
    let st = Rc::new(State {
        dict: dict.clone(),
        query: signal(String::new()),
        candidates: signal(Vec::new()),
        entries: signal(Vec::new()),
        hint: signal(String::from("输入一个词开始查询")),
    });

    Element::col()
        .fill()
        .bg(Color::hex(0xFFFFFF))
        .padding(16)
        .spacing(10)
        // 补全驱动器：零尺寸、不可见，必须排在候选列表之前（on_update 按注册顺序广播）。
        .child(
            Element::leaf()
                .reactive()
                .widget(Completer {
                    dict: dict.clone(),
                    query: st.query,
                    candidates: st.candidates,
                    last_version: st.query.version(),
                })
                .height(0),
        )
        .child(Element::text_input(st.query, "输入单词或中文…").width_match())
        .child(candidate_list(st.clone()))
        .child(Element::divider())
        .child(result_area(st))
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
                .fg(Color::hex(0x636E72))
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
                    .fg(Color::hex(0x2D3436))
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
                    .fg(Color::hex(0x2D3436))
                    .height(26)
                    .width_match(),
            );
            // 繁体与词头不同才展示——相同时显示两遍是噪音。
            if x.traditional != x.headword.as_str() {
                col = col.child(
                    Element::label(format!("繁体：{}", x.traditional))
                        .fg(Color::hex(0x636E72))
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
                        .fg(Color::hex(0x636E72))
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
