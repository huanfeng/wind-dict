//! 三套配色皮肤。
//!
//! ## 为何不是「主题」
//!
//! windui 已有 `Theme` 这个名字，指的是驱动控件默认视觉的那份色板与度量。本项目
//! 需要的比它多——设计里有若干位置是自绘的（标题栏底、侧边栏底、选中行淡底），
//! `Palette` 没有对应字段。若也叫「主题」，代码里就会出现两个 Theme 互相打架。
//!
//! 故本模块的单位叫**皮肤**：一套皮肤 = 一份交给 windui 的 `Theme` + 一组界面
//! 自绘要用的额外色。
//!
//! ## 三套皮肤的取色
//!
//! 底色、文字、边框等**直接取自设计稿的 CSS 变量**（`Dictionary.dc.html` 的 THEMES）。
//!
//! `--accent` 是例外：设计稿用 `oklch()` 写，而 windui 只吃 sRGB，必须换算。这里
//! 曾图省事拿设计稿的 `THEME_SWATCH` 顶替——那是错的，`THEME_SWATCH` 是设置页三个
//! 装饰小色块的预览值，不是 `--accent` 的换算结果。二者可辨地不同，深色皮肤尤甚
//! （`oklch(0.82 …)` 换算得 `#65DDBB`，而 swatch 是明显更深的 `#2ECA9E`），且明亮
//! 皮肤因此让白字掉到 4.17:1，够不到 AA。现按 OKLab 矩阵实算取值。
//!
//! `--accent` 的 hover / active 两态设计稿**没有**，是本模块自行派生的：在 oklch 空间
//! 把 L 各 ±0.07，色相与彩度不动——这样三套皮肤的交互反馈强度一致，而在 sRGB 里直接
//! 加减亮度做不到（同样的增量在不同色相上观感差别很大）。`accent_border` 明亮皮肤
//! 同样是 oklch 换算所得，其余两套设计稿给的就是 hex。

use windui::prelude::*;
use windui::theme::{Metrics, Palette, Theme};

/// 皮肤种类。持久化与设置页都按它标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinKind {
    /// 简约明亮：中性暖白 · 蓝。
    Light,
    /// 典雅纸感：暖纸色 · 赭。
    Paper,
    /// 深色专注：深灰 · 青绿。
    Dark,
}

impl SkinKind {
    /// 设置页列出的全部皮肤，顺序即展示顺序。
    pub const ALL: [SkinKind; 3] = [SkinKind::Light, SkinKind::Paper, SkinKind::Dark];

    /// 设置页卡片的标题。
    pub fn name(self) -> &'static str {
        match self {
            SkinKind::Light => "简约明亮",
            SkinKind::Paper => "典雅纸感",
            SkinKind::Dark => "深色专注",
        }
    }

    /// 设置页卡片的副标题：底色 · 强调色。
    pub fn desc(self) -> &'static str {
        match self {
            SkinKind::Light => "中性暖白 · 蓝",
            SkinKind::Paper => "暖纸色 · 赭",
            SkinKind::Dark => "深灰 · 青绿",
        }
    }

    pub fn skin(self) -> Skin {
        match self {
            SkinKind::Light => Skin::light(),
            SkinKind::Paper => Skin::paper(),
            SkinKind::Dark => Skin::dark(),
        }
    }
}

/// 一套皮肤。
///
/// `theme` 交给 windui 驱动控件默认视觉；其余字段是界面自绘处要用的颜色——
/// 它们在 `Palette` 里没有对应字段，不是遗漏而是层次不同：`Palette` 描述的是
/// 「控件」的通用角色，而这些描述的是本应用**特定区域**的身份。
///
/// 代价是这些字段**没有对应的 `Role`**，因而无法随 `ThemeHandle::set` 换肤跟随。
/// 这正是运行时换肤当前做不到的原因之一，见 ADR-0012。
#[derive(Clone)]
pub struct Skin {
    pub theme: Theme,
    /// 标题栏底色。
    pub titlebar: Color,
    /// 侧边栏底色。比窗口底 `bg` 略深，用于把左栏与主区分开。
    pub panel: Color,
    /// 次级文字：比正文浅、比 `text_muted` 深。用于标题栏应用名、列表项等。
    pub text2: Color,
    /// 强调色淡底：列表选中行、勾选态的背景。
    pub accent_soft: Color,
    /// 强调色描边：淡底之上的边框。
    pub accent_border: Color,
    /// 卡片底色。
    pub card: Color,
    /// 三色预览块，用于设置页的皮肤卡片。
    ///
    /// 取自设计稿的 `THEME_SWATCH`，且**只**能用在这里——它就是为这三个装饰色块
    /// 定义的，不是 `--accent` 等真实变量的等价值（见模块头）。
    pub swatch: [Color; 3],
}

/// 三套皮肤共用的度量。
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

impl Skin {
    pub fn light() -> Self {
        // oklch(0.55 0.16 264)。白字于其上 4.99:1，达 AA。
        let accent = Color::hex(0x406BCE);
        Self {
            theme: Theme {
                palette: Palette {
                    accent,
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
                },
                metrics: metrics(),
                ..Theme::default()
            },
            titlebar: Color::hex(0xF6F5F1),
            panel: Color::hex(0xF6F5F1),
            text2: Color::hex(0x3F3D37),
            accent_soft: Color::hex(0xECEBFF),
            // oklch(0.85 0.05 264)。另两套皮肤设计稿直接给的是 hex，无需换算。
            accent_border: Color::hex(0xBDCEEF),
            card: Color::hex(0xF6F5F1),
            swatch: [
                Color::hex(0xFBFBF9),
                Color::hex(0x5B6CFF),
                Color::hex(0x1B1A17),
            ],
        }
    }

    pub fn paper() -> Self {
        // oklch(0.52 0.11 55)。白字于其上 5.74:1，达 AA。
        let accent = Color::hex(0x985521);
        Self {
            theme: Theme {
                palette: Palette {
                    accent,
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
                },
                metrics: metrics(),
                ..Theme::default()
            },
            titlebar: Color::hex(0xECE5D6),
            panel: Color::hex(0xECE5D6),
            text2: Color::hex(0x5C5040),
            accent_soft: Color::hex(0xEFE4D0),
            accent_border: Color::hex(0xE0CFA8),
            card: Color::hex(0xEFE8D8),
            swatch: [
                Color::hex(0xF7F2E7),
                Color::hex(0xA5603F),
                Color::hex(0x33291C),
            ],
        }
    }

    pub fn dark() -> Self {
        // oklch(0.82 0.12 172)。
        let accent = Color::hex(0x65DDBB);
        Self {
            theme: Theme {
                palette: Palette {
                    accent,
                    accent_hover: Color::hex(0x7DF5D2),
                    accent_active: Color::hex(0x4BC6A5),
                    // 深色皮肤的强调色是亮青绿，白字压不住——白字仅 1.66:1，改用深色
                    // 前景得 9.95:1。这是三套皮肤里唯一需要偏离白色 `on_accent` 的。
                    on_accent: Color::hex(0x10221C),
                    bg: Color::hex(0x17191C),
                    surface: Color::hex(0x1E2125),
                    surface_alt: Color::hex(0x1B1E22),
                    text: Color::hex(0xF2F3F4),
                    text_muted: Color::hex(0x8B9097),
                    text_disabled: Color::hex(0x565B62),
                    // 深色下的分隔与描边用半透明白：叠在不同深浅的面上都成立，
                    // 写死实色则会在 bg / surface / surface_alt 之间露出接缝。
                    border: Color::rgba(255, 255, 255, 23),
                    track: Color::rgba(255, 255, 255, 23),
                    placeholder: Color::hex(0x565B62),
                    divider: Color::rgba(255, 255, 255, 18),
                    ..Palette::default()
                },
                metrics: metrics(),
                ..Theme::default()
            },
            titlebar: Color::hex(0x1B1E22),
            panel: Color::hex(0x1B1E22),
            text2: Color::hex(0xD5D8DB),
            accent_soft: Color::rgba(46, 202, 158, 36),
            accent_border: Color::rgba(46, 202, 158, 77),
            card: Color::hex(0x1E2125),
            swatch: [
                Color::hex(0x1B1E22),
                Color::hex(0x2ECA9E),
                Color::hex(0xF2F3F4),
            ],
        }
    }
}
