//! 配色：三种**风格** × 亮/暗两档。
//!
//! ## 为何不是「主题」
//!
//! windui 已有 `Theme` 这个名字，指的是驱动控件默认视觉的那份色板与度量。本模块
//! 产出的正是它——但产出**哪一份**由两个正交的选择决定，故对外的单位不叫主题：
//! 叫主题的话，代码里会出现两个 Theme 互相打架。
//!
//! ## 风格与明暗是两件事
//!
//! [`SkinStyle`] 管「用哪一族颜色」（中性 / 暖纸 / 冷灰，各配一个色相），
//! [`SkinMode`] 管「亮还是暗」。此前二者是揉在一起的三个枚举值（简约明亮 / 典雅
//! 纸感 / 深色专注），于是「我喜欢纸感，但晚上想要暗的」这个再普通不过的诉求无处
//! 表达——三选一里根本没有那一项。拆开之后是 3 × 2 = 6 套，而设置页仍只是两个控件。
//!
//! [`SkinMode::System`] 让明暗跟随系统。这对常驻工具尤其要紧：一次会话可能横跨
//! 日出日落，而进程一直不重启。
//!
//! ## 取色方法
//!
//! 底色、文字、边框等，亮色三套中的**简约·亮**与**纸感·亮**直接取自设计稿的 CSS
//! 变量（`Dictionary.dc.html` 的 THEMES），**专注·暗**同样如此。其余三套（简约·暗、
//! 纸感·暗、专注·亮）设计稿没有，按同一套方法在 oklch 空间新配：沿用同族色相，
//! 明度阶梯对齐已有的那三套，逐项核过 WCAG 对比度（见文件末的测试）。
//!
//! 强调色一律在 oklch 里定值再换算成 sRGB——windui 只吃 sRGB，而 `oklch()` 是设计稿
//! 的书写方式。这里曾图省事拿设计稿的 `THEME_SWATCH` 顶替，那是错的：`THEME_SWATCH`
//! 是设置页装饰色块的预览值，不是 `--accent` 的换算结果，二者可辨地不同（深色皮肤
//! 尤甚），且会让白字掉到 4.17:1、够不到 AA。
//!
//! hover / active 两态设计稿**没有**，是本模块派生的：在 oklch 空间把 L 各 ±0.07，
//! 色相与彩度不动——这样六套配色的交互反馈强度一致，而在 sRGB 里直接加减亮度做不到
//! （同样的增量在不同色相上观感差别很大）。
//!
//! ## 亮暗两档的强调色为什么不同值
//!
//! 同一风格在亮暗下用**同一色相、不同明度**：亮色档取 L≈0.52（偏暗，配白字），
//! 暗色档取 L≈0.70–0.82（偏亮，配深字）。不能两档共用一个值——把亮色档那个 L=0.55
//! 的蓝放到深灰底上，强调色与底的对比只有 3.5:1，按钮糊成一团；反过来把亮档的底
//! 配上 L=0.82 的青绿，白字压不住它。
//!
//! 代价是 `on_accent` 在暗色档要翻成深色。这不是妥协而是必然：一个在深底上够亮的
//! 强调色，本身就亮到白字站不住。

use windui::prelude::*;
use windui::theme::{Metrics, Palette, Theme};

/// 配色风格：决定用哪一族颜色，与亮暗无关。
///
/// 三种取向，各由「中性色的冷暖」与「强调色的色相」共同定义——两者必须一起换，
/// 只换强调色的话，三种风格在暗色档下会像是同一套配色配了三个按钮颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinStyle {
    /// 简约：中性底 · 蓝。亮档偏暖白，暗档是彩度为零的纯中性灰。
    Plain,
    /// 纸感：暖色底 · 赭。暖色相贯穿全套，亮档是纸白，暗档是深褐。
    Paper,
    /// 专注：冷灰底 · 青绿。
    Focus,
}

impl SkinStyle {
    /// 设置页列出的全部风格，顺序即展示顺序。
    pub const ALL: [SkinStyle; 3] = [SkinStyle::Plain, SkinStyle::Paper, SkinStyle::Focus];

    /// 设置页卡片的标题。
    pub fn name(self) -> &'static str {
        match self {
            SkinStyle::Plain => "简约",
            SkinStyle::Paper => "纸感",
            SkinStyle::Focus => "专注",
        }
    }

    /// 设置页卡片的副标题：底色 · 强调色。
    ///
    /// 随明暗变：暗色档下写「暖白」是骗人的，而副标题的全部作用就是让用户在点下去
    /// 之前知道会得到什么。
    pub fn desc(self, dark: bool) -> &'static str {
        match (self, dark) {
            (SkinStyle::Plain, false) => "暖白 · 蓝",
            (SkinStyle::Plain, true) => "中性深灰 · 蓝",
            (SkinStyle::Paper, false) => "暖纸色 · 赭",
            (SkinStyle::Paper, true) => "深褐 · 赭",
            (SkinStyle::Focus, false) => "浅灰 · 青绿",
            (SkinStyle::Focus, true) => "深灰 · 青绿",
        }
    }

    /// 这一风格在指定明暗档下的 `Theme`，交给 [`windui::app::ThemeHandle`]。
    pub fn theme(self, dark: bool) -> Theme {
        let palette = match (self, dark) {
            (SkinStyle::Plain, false) => plain_light(),
            (SkinStyle::Plain, true) => plain_dark(),
            (SkinStyle::Paper, false) => paper_light(),
            (SkinStyle::Paper, true) => paper_dark(),
            (SkinStyle::Focus, false) => focus_light(),
            (SkinStyle::Focus, true) => focus_dark(),
        };
        Theme {
            palette,
            metrics: metrics(),
            ..Theme::default()
        }
    }

    /// 设置页卡片上那三个预览色块：底色、强调色、正文色。
    ///
    /// **直接取自色板本身**，不是另一份手写的预览值。此前它是设计稿里一份独立的
    /// `THEME_SWATCH`，于是预览与实际可辨地对不上——而色块的全部职责就是预告实际
    /// 效果。取这三项，是因为它们正是一眼能认出一套配色的三样东西。
    pub fn swatch(self, dark: bool) -> [Color; 3] {
        let p = self.theme(dark).palette;
        [p.bg, p.accent, p.text]
    }
}

/// 亮 / 暗 / 跟随系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinMode {
    Light,
    Dark,
    /// 跟随系统的「应用模式」设置，并在用户改动它时当场跟上。
    System,
}

impl SkinMode {
    /// 设置页分段控件的顺序：亮 / 暗 / 跟随系统。
    ///
    /// 「跟随系统」排在最后而非最前，尽管它多半是最佳默认：前两项是**直接的**选择，
    /// 第三项是把选择权交出去，读起来是「或者，让系统决定」。分段控件从左往右读，
    /// 这个次序才顺。
    pub const ALL: [SkinMode; 3] = [SkinMode::Light, SkinMode::Dark, SkinMode::System];

    pub fn label(self) -> &'static str {
        match self {
            SkinMode::Light => "亮",
            SkinMode::Dark => "暗",
            SkinMode::System => "跟随系统",
        }
    }

    /// 最终该用暗色吗。`system_dark` 由调用方查系统得来
    /// （[`windui::event::system_prefers_dark`]）。
    ///
    /// 系统那一位由外部传入而不在这里查：本模块除此之外只是一堆常量，查不查得到
    /// 系统偏好与配色本身无关。传进来也让这个判定可测——查系统的版本测不了。
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            SkinMode::Light => false,
            SkinMode::Dark => true,
            SkinMode::System => system_dark,
        }
    }
}

/// 六套配色共用的度量。
///
/// 圆角比 windui 默认（4/6/10）更大一档：设计稿的键帽 5-6px、列表行 8-9px、
/// 卡片与查询框 11-12px，整体比框架默认更圆。
fn metrics() -> Metrics {
    Metrics {
        corner_sm: 6.0,
        corner_md: 8.0,
        corner_lg: 12.0,
        font_sm: 12.5,
        font_md: 14.0,
        font_lg: 16.0,
        ..Metrics::default()
    }
}

/// 深色档的分隔线与描边：半透明白。
///
/// 叠在不同深浅的面上都成立，写死实色则会在 bg / surface / surface_alt 之间露出接缝。
const DARK_BORDER: Color = Color::rgba(255, 255, 255, 23);
const DARK_DIVIDER: Color = Color::rgba(255, 255, 255, 18);

/// 简约·亮：暖白 · 蓝。设计稿的 `light` 主题。
fn plain_light() -> Palette {
    Palette {
        // oklch(0.55 0.16 264)。白字于其上 4.99:1，达 AA。
        accent: Color::hex(0x406BCE),
        accent_hover: Color::hex(0x5381E6),
        accent_active: Color::hex(0x2D56B6),
        on_accent: Color::WHITE,
        bg: Color::hex(0xFBFBF9),
        surface: Color::hex(0xFFFFFF),
        surface_alt: Color::hex(0xF6F5F1),
        text: Color::hex(0x1B1A17),
        text_muted: Color::hex(0x8A887F),
        text_disabled: Color::hex(0xB3B1A6),
        border: Color::hex(0xE6E4DD),
        track: Color::hex(0xE6E4DD),
        placeholder: Color::hex(0xB3B1A6),
        divider: Color::hex(0xEEEDE8),
        ..Palette::default()
    }
}

/// 简约·暗：中性深灰 · 蓝。
///
/// 中性色的彩度取 **0**——这是它与专注·暗的分野：那套是偏蓝绿的冷灰
/// （`oklch(0.213 0.007 258)`），这套一点色偏都不带。两套暗色配色若只差一个强调色，
/// 在设置页上就成了「同一套配色的两个按钮颜色」，风格这个维度也就白分了。
fn plain_dark() -> Palette {
    Palette {
        // oklch(0.70 0.13 264)。与亮档同色相，明度提高一档：L=0.55 那个蓝放到深灰底上
        // 与底只有 3.5:1，按钮会糊成一团。
        accent: Color::hex(0x759CEF),
        accent_hover: Color::hex(0x8AB2FF),
        accent_active: Color::hex(0x6086D8),
        // 深底上够亮的强调色，白字必然站不住（仅 2.71:1）。深色前景得 6.72:1。
        on_accent: Color::hex(0x0D1528),
        bg: Color::hex(0x191919),
        surface: Color::hex(0x222222),
        surface_alt: Color::hex(0x1D1D1D),
        text: Color::hex(0xF3F3F3),
        text_muted: Color::hex(0x929292),
        text_disabled: Color::hex(0x5B5B5B),
        border: DARK_BORDER,
        track: DARK_BORDER,
        placeholder: Color::hex(0x5B5B5B),
        divider: DARK_DIVIDER,
        ..Palette::default()
    }
}

/// 纸感·亮：暖纸色 · 赭。设计稿的 `paper` 主题。
fn paper_light() -> Palette {
    Palette {
        // oklch(0.52 0.11 55)。白字于其上 5.74:1，达 AA。
        accent: Color::hex(0x985521),
        accent_hover: Color::hex(0xAF6A37),
        accent_active: Color::hex(0x824103),
        on_accent: Color::WHITE,
        bg: Color::hex(0xF7F2E7),
        surface: Color::hex(0xFAF6EC),
        surface_alt: Color::hex(0xECE5D6),
        text: Color::hex(0x33291C),
        text_muted: Color::hex(0x8A7D68),
        text_disabled: Color::hex(0xB3A482),
        border: Color::hex(0xDDD3BF),
        track: Color::hex(0xDDD3BF),
        placeholder: Color::hex(0xB3A482),
        divider: Color::hex(0xE3D9C4),
        ..Palette::default()
    }
}

/// 纸感·暗：深褐 · 赭。
///
/// 暖色相贯穿全套（中性色也带 0.014–0.020 的彩度），而不是「深灰配一个暖强调色」。
/// 纸感这个风格的身份就在底色的暖上——底色一中性，它与简约·暗就只剩强调色之别了。
fn paper_dark() -> Palette {
    Palette {
        // oklch(0.72 0.11 55)。与亮档同色相，明度提高一档，理由同 `plain_dark`。
        accent: Color::hex(0xDA915F),
        accent_hover: Color::hex(0xF1A774),
        accent_active: Color::hex(0xC27C4A),
        // 白字于其上仅 2.56:1；深褐前景得 7.08:1。
        on_accent: Color::hex(0x241104),
        bg: Color::hex(0x221D17),
        surface: Color::hex(0x2E2820),
        surface_alt: Color::hex(0x28221B),
        text: Color::hex(0xF2EEE6),
        text_muted: Color::hex(0x999185),
        text_disabled: Color::hex(0x645C51),
        border: DARK_BORDER,
        track: DARK_BORDER,
        placeholder: Color::hex(0x645C51),
        divider: DARK_DIVIDER,
        ..Palette::default()
    }
}

/// 专注·亮：浅灰 · 青绿。
///
/// 冷中性底（色相 220、彩度 0.003–0.012），与简约·亮那套偏暖的白拉开。强调色沿用
/// 专注·暗的青绿色相 172，明度压到 0.52 才让白字够得着 AA（5.23:1）。
fn focus_light() -> Palette {
    Palette {
        // oklch(0.52 0.10 172)。
        accent: Color::hex(0x047B62),
        accent_hover: Color::hex(0x2B9076),
        accent_active: Color::hex(0x00664F),
        on_accent: Color::WHITE,
        bg: Color::hex(0xF6F9FA),
        surface: Color::hex(0xFFFFFF),
        surface_alt: Color::hex(0xEEF1F3),
        text: Color::hex(0x161B1E),
        text_muted: Color::hex(0x7E868B),
        text_disabled: Color::hex(0xA8AFB3),
        border: Color::hex(0xDBE1E2),
        track: Color::hex(0xDBE1E2),
        placeholder: Color::hex(0xA8AFB3),
        divider: Color::hex(0xE5EAEB),
        ..Palette::default()
    }
}

/// 专注·暗：深灰 · 青绿。设计稿的 `dark` 主题。
fn focus_dark() -> Palette {
    Palette {
        // oklch(0.82 0.12 172)。
        accent: Color::hex(0x65DDBB),
        accent_hover: Color::hex(0x7DF5D2),
        accent_active: Color::hex(0x4BC6A5),
        // 白字于其上仅 1.66:1，改用深色前景得 9.95:1。
        on_accent: Color::hex(0x10221C),
        bg: Color::hex(0x17191C),
        surface: Color::hex(0x1E2125),
        surface_alt: Color::hex(0x1B1E22),
        text: Color::hex(0xF2F3F4),
        text_muted: Color::hex(0x8B9097),
        text_disabled: Color::hex(0x565B62),
        border: DARK_BORDER,
        track: DARK_BORDER,
        placeholder: Color::hex(0x565B62),
        divider: DARK_DIVIDER,
        ..Palette::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相对亮度（WCAG 2.x）。
    fn luminance(c: Color) -> f32 {
        fn lin(v: u8) -> f32 {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    /// WCAG 对比度。两色均须不透明——本模块只对不透明项断言（半透明的
    /// `border`/`divider` 是装饰线，不承载文字）。
    fn contrast(a: Color, b: Color) -> f32 {
        assert_eq!(a.a, 255, "对比度只对不透明色有意义");
        assert_eq!(b.a, 255, "对比度只对不透明色有意义");
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn all() -> Vec<(SkinStyle, bool, Palette)> {
        let mut v = Vec::new();
        for s in SkinStyle::ALL {
            for dark in [false, true] {
                v.push((s, dark, s.theme(dark).palette));
            }
        }
        v
    }

    /// 按钮上的字必须看得清。**这是六套配色里最容易出事的一项**——强调色是唯一
    /// 一个「亮暗两档取值不同」的角色，而它一变，压在上面的字就得跟着重新选。
    ///
    /// 4.5 是 WCAG AA 对正文的门槛。按钮文字通常不大，不能按大字的 3.0 放宽。
    #[test]
    fn 强调色上的文字达到_aa() {
        for (s, dark, p) in all() {
            let c = contrast(p.on_accent, p.accent);
            assert!(
                c >= 4.5,
                "{:?}/{} 的 on_accent 对比度仅 {c:.2}:1，够不到 AA 的 4.5",
                s,
                if dark { "暗" } else { "亮" }
            );
        }
    }

    /// 正文与次级文字在两种底（窗口底、面）上都要够读。
    ///
    /// 两种底都查，是因为它们并不总是同一档亮度：`surface` 在亮色档比 `bg` 更白、
    /// 在暗色档却比 `bg` 更浅——正文在其中一种上成立不代表另一种也成立。
    #[test]
    fn 正文在两种底上都达到_aa() {
        for (s, dark, p) in all() {
            for (名, 底) in [
                ("bg", p.bg),
                ("surface", p.surface),
                ("surface_alt", p.surface_alt),
            ] {
                let c = contrast(p.text, 底);
                assert!(
                    c >= 7.0,
                    "{:?}/{} 的正文在 {名} 上仅 {c:.2}:1，低于既有配色 12.6 起的水准",
                    s,
                    if dark { "暗" } else { "亮" }
                );
            }
        }
    }

    /// 强调色本身要能从底色里跳出来，否则按钮与普通面一个样。
    ///
    /// 3.0 是 WCAG 对**非文字**元素（控件边界、状态指示）的门槛。这条守的正是
    /// 「亮暗两档不能共用一个强调色」：把亮档那个 L=0.55 的蓝放到深灰底上是 3.5:1，
    /// 勉强够；再深一点就掉下去了。
    #[test]
    fn 强调色与底色的对比够作非文字元素() {
        for (s, dark, p) in all() {
            let c = contrast(p.accent, p.bg);
            assert!(
                c >= 3.0,
                "{:?}/{} 的强调色对底色仅 {c:.2}:1，按钮会糊进背景",
                s,
                if dark { "暗" } else { "亮" }
            );
        }
    }

    /// 暗色档的底色确实比亮色档暗——防的是把某一套的两档配串了。
    ///
    /// 这种错在编译期与运行期都不会响：两套颜色都是合法的色板，界面照样画得出来，
    /// 只是用户点「暗」得到一片白。
    #[test]
    fn 每种风格的暗档都真的比亮档暗() {
        for s in SkinStyle::ALL {
            let (l, d) = (s.theme(false).palette, s.theme(true).palette);
            assert!(
                luminance(d.bg) < luminance(l.bg),
                "{s:?} 的暗档底色不比亮档暗"
            );
            assert!(
                luminance(d.text) > luminance(l.text),
                "{s:?} 的暗档正文不比亮档亮"
            );
        }
    }

    /// 三种风格必须彼此可辨，否则「风格」这个维度形同虚设。
    ///
    /// 逐档比：同为暗色档的三套若底色相同，设置页上就成了「同一套配色的三个按钮
    /// 颜色」。强调色本就各异，故只查底色。
    #[test]
    fn 同一明暗档下三种风格的底色互不相同() {
        for dark in [false, true] {
            let bgs: Vec<_> = SkinStyle::ALL
                .iter()
                .map(|s| s.theme(dark).palette.bg)
                .collect();
            for i in 0..bgs.len() {
                for j in i + 1..bgs.len() {
                    assert_ne!(
                        (bgs[i].r, bgs[i].g, bgs[i].b),
                        (bgs[j].r, bgs[j].g, bgs[j].b),
                        "{:?} 档下 {:?} 与 {:?} 的底色相同",
                        dark,
                        SkinStyle::ALL[i],
                        SkinStyle::ALL[j]
                    );
                }
            }
        }
    }

    /// 预览色块取自色板本身，故必然与实际一致——这条钉住的是「不许再有第二份色值」。
    #[test]
    fn 预览色块取自色板本身() {
        for (s, dark, p) in all() {
            assert_eq!(s.swatch(dark), [p.bg, p.accent, p.text]);
        }
    }

    #[test]
    fn 跟随系统时明暗由系统那一位决定() {
        assert!(!SkinMode::Light.is_dark(true), "选了亮就不该被系统翻过去");
        assert!(SkinMode::Dark.is_dark(false), "选了暗同理");
        assert!(SkinMode::System.is_dark(true));
        assert!(!SkinMode::System.is_dark(false));
    }
}
