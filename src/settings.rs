//! 用户设置：唤起热键、开机启动、词库路径、皮肤、释义显示。
//!
//! ## 设置与用户数据的分界
//!
//! 二者同住一个 SQLite 文件（`userdata.db`），但**语义不同，失去的代价也不同**：
//! 收藏与历史是用户攒出来的、丢了不可复得（ADR-0011 因此把它挪出部署目录）；设置
//! 是几个偏好，丢了重设一遍即可。放同一个库只是省一个文件句柄，不代表它们同级——
//! 任何「顺手把设置也备份/迁移」的逻辑都该分开对待。
//!
//! ## 为什么是纯数据 + 显式加载
//!
//! `Settings` 不持有数据库连接，加载与保存都由调用方显式发起。这样它可以脱开
//! SQLite 单测（解析、默认值、往返一致性都是纯逻辑），而那正是这个模块最容易出错
//! 的地方——热键的字符串表示尤其。

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::skin::{SkinMode, SkinStyle};

/// 热键的主键。
///
/// 分两类而非统一成一个 `char`，是因为**它们的安全前提不同**：字母数字键单独注册会
/// 吞掉该字符在所有程序里的输入（按一下 D 就唤起词典，等于没法打字了），功能键则不
/// 参与文字输入，单独用是安全的。这个差别决定了 [`HotkeySpec::is_safe`] 的判定，
/// 把它编进类型里，就不会有人在某条新路径上忘了区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyKey {
    /// 字母或数字。**必须搭配至少一个修饰键**。
    Char(char),
    /// 功能键 F1–F12。可以单独作热键。
    Func(u8),
}

impl fmt::Display for HotkeyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyKey::Char(c) => write!(f, "{c}"),
            HotkeyKey::Func(n) => write!(f, "F{n}"),
        }
    }
}

impl FromStr for HotkeyKey {
    type Err = String;

    /// 解析主键。`F1`–`F12` 认功能键，单个字母数字认字符键。
    ///
    /// **先试功能键**：`F` 本身也是合法的字符主键（Ctrl+Alt+F），若先按单字符解析，
    /// `F1` 会被截成 `F`，用户设的 F1 会悄悄变成一个完全不同的热键。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix(['F', 'f']) {
            if !rest.is_empty() {
                return match rest.parse::<u8>() {
                    Ok(n) if (1..=12).contains(&n) => Ok(HotkeyKey::Func(n)),
                    _ => Err(format!("功能键只支持 F1–F12：{s}")),
                };
            }
        }
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => {
                Ok(HotkeyKey::Char(c.to_ascii_uppercase()))
            }
            _ => Err(format!("无法识别的按键：{s}")),
        }
    }
}

impl HotkeyKey {
    /// 全部可选主键，顺序固定：`F1`–`F12`、`A`–`Z`、`0`–`9`。
    ///
    /// 功能键排在最前是因为它们是**唯一能单独作热键**的一档（见
    /// [`HotkeySpec::is_safe`]），把它们放在列表开头，用户一拉开就看得见这条可能性。
    pub fn all() -> Vec<HotkeyKey> {
        let f = (1..=12).map(HotkeyKey::Func);
        let letters = (b'A'..=b'Z').map(|c| HotkeyKey::Char(c as char));
        let digits = (b'0'..=b'9').map(|c| HotkeyKey::Char(c as char));
        f.chain(letters).chain(digits).collect()
    }

    /// 本键在 [`all`](Self::all) 里的下标。不在表内时回退到 0——那只可能来自一个手改
    /// 过的设置库，让下拉框停在首项，好过 panic 或显示一个空白选项。
    pub fn index(self) -> usize {
        HotkeyKey::all()
            .iter()
            .position(|&k| k == self)
            .unwrap_or(0)
    }
}

/// 唤起热键的按键组合。
///
/// 用自己的类型而非直接存 windui 的 `Hotkey`：后者是框架的注册用类型，既不能
/// 序列化也不便比较，而热键要写进数据库、要在界面上显示、要能被解析回来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// 主键：单个字母/数字，或 F1–F12。
    ///
    /// 不收符号键：热键是全局注册，而符号在各语言键盘上的位置差异太大，容易「在别人
    /// 机器上按不出来」。功能键没有这个问题——F1–F12 在所有键盘布局上都在同一处。
    pub key: HotkeyKey,
}

impl Default for HotkeySpec {
    /// Ctrl+Alt+D。
    fn default() -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: false,
            key: HotkeyKey::Char('D'),
        }
    }
}

impl HotkeySpec {
    /// 这个组合是否可以安全地注册为全局热键。
    ///
    /// 无修饰的**字母数字**热键会吞掉该字符在所有程序中的输入——用户按一下 D 就唤起
    /// 词典，等于没法打字了，而且这个错误一旦犯下，用户很难意识到是词典干的。
    ///
    /// 功能键不在此列：F1–F12 不参与文字输入，单独作热键是常规做法（且比三键组合好
    /// 按）。这条区分正是主键分成两类的理由，见 [`HotkeyKey`]。
    pub fn is_safe(self) -> bool {
        match self.key {
            HotkeyKey::Func(_) => true,
            HotkeyKey::Char(_) => self.ctrl || self.alt || self.shift,
        }
    }

    /// 转成 windui 的注册用类型。
    ///
    /// 放在这里而非 `main`：改键时界面也要用它（`ui::State::set_hotkey` 走
    /// `HotkeyHandle::rebind`），两处各写一遍必然漂移。
    pub fn to_hotkey(self) -> windui::event::Hotkey {
        let key = match self.key {
            HotkeyKey::Char(c) => windui::event::Key::Char(c),
            // VK_F1 = 0x70，F1–F12 连续。走 `Key::Other` 而不必在框架里新增变体：
            // windui 已把它定义为「跨平台对齐的虚拟键码」并直接放行给 RegisterHotKey
            // （见 `platform/win32/hotkey.rs` 的 `vk_of`）。
            HotkeyKey::Func(n) => windui::event::Key::Other(0x6F + n as u32),
        };
        let mut hk = windui::event::Hotkey::new(key);
        if self.ctrl {
            hk = hk.ctrl();
        }
        if self.alt {
            hk = hk.alt();
        }
        if self.shift {
            hk = hk.shift();
        }
        hk
    }
}

impl fmt::Display for HotkeySpec {
    /// `Ctrl+Alt+D`。顺序固定为 Ctrl → Alt → Shift → 主键，与 Windows 的书写惯例
    /// 一致，也保证同一组合只有一种字符串形式（否则往返解析会漂移）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for HotkeySpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut spec = HotkeySpec {
            ctrl: false,
            alt: false,
            shift: false,
            key: HotkeyKey::Char('\0'),
        };
        let mut seen_key = false;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => spec.ctrl = true,
                "alt" => spec.alt = true,
                "shift" => spec.shift = true,
                _ => {
                    spec.key = part.parse()?;
                    seen_key = true;
                }
            }
        }
        if !seen_key {
            return Err(format!("热键缺少主键：{s}"));
        }
        Ok(spec)
    }
}

/// 左栏默认宽度。
///
/// 窗口最小宽 720（`main.rs`），280 之后右栏还剩 440——放得下 30px 的词头与成行的
/// 释义。这也是「重置」按钮回到的那个值。
pub const LEFT_PANE_W_DEFAULT: i32 = 280;

/// 左栏宽度下限。再窄，候选行的释义摘要就只剩两三个字，而「一眼认出是不是我要的那个
/// 词」正是靠它。
pub const LEFT_PANE_W_MIN: i32 = 200;

/// 左栏宽度上限。
///
/// 这是与窗口无关的**硬上限**，防止设置库被手改成一个荒唐的值。真正管用的上限跟着
/// 窗口走（见 `ui::clamp_left_w`）——它保证右栏无论如何都留得下能读的宽度，而这条
/// 只保证「即便在一块超宽屏上，左栏也不该比一本书还宽」。
pub const LEFT_PANE_W_MAX: i32 = 560;

/// 一份完整设置。字段均有默认值，故读不到任何一项时也能给出可用的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// 唤起热键。
    pub hotkey: HotkeySpec,
    /// 是否开机自启。
    pub autostart: bool,
    /// 配色风格（用哪一族颜色）。与 `mode` 正交，见 [`crate::skin`]。
    pub style: SkinStyle,
    /// 亮 / 暗 / 跟随系统。
    pub mode: SkinMode,
    /// 词条是否默认展开英英释义。
    ///
    /// 只影响**默认**展开与否，不影响该区是否存在——英英释义始终可展开，这是
    /// 刻意的产品决定（见 `ui::entry_view`）。
    pub expand_en: bool,
    /// 词库路径覆盖。`None` = 沿用命令行参数或 exe 同目录的默认查找。
    pub ecdict: Option<PathBuf>,
    pub cedict: Option<PathBuf>,
    /// 左栏宽度（逻辑 px）。
    ///
    /// 存**像素**而非左右比例，是因为左栏装的是一列词头：它需要的宽度由「一个词头加
    /// 一行释义摘要要多宽」决定，与窗口多宽无关。按比例存的话，窗口一拉宽，左栏就跟着
    /// 长出一截谁也用不上的空白，而真正该变宽的是读释义的右栏。Finder、macOS 词典
    /// 的侧栏都是这么处理的。
    pub left_pane_w: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: HotkeySpec::default(),
            autostart: false,
            style: SkinStyle::Plain,
            mode: SkinMode::System,
            expand_en: false,
            ecdict: None,
            cedict: None,
            left_pane_w: LEFT_PANE_W_DEFAULT,
        }
    }
}

/// 设置在数据库里的键名。集中在此，避免字符串散落各处拼错。
pub mod keys {
    pub const HOTKEY: &str = "hotkey";
    pub const AUTOSTART: &str = "autostart";
    pub const SKIN_STYLE: &str = "skin_style";
    pub const SKIN_MODE: &str = "skin_mode";
    /// 旧版的皮肤键，**只读不写**。见 `super::legacy`。
    pub const LEGACY_SKIN: &str = "skin";
    pub const EXPAND_EN: &str = "expand_en";
    pub const ECDICT: &str = "ecdict";
    pub const CEDICT: &str = "cedict";
    pub const LEFT_PANE_W: &str = "left_pane_w";
}

impl Settings {
    /// 从键值对还原。**无法识别的值一律退回默认**，不报错。
    ///
    /// 理由是设置库可能被手改、被旧版本写坏、或是将来新增枚举项后回退旧版本。这些
    /// 情形下让程序起不来是不成比例的——设置丢了重设一遍即可，而拒绝启动等于把
    /// 一个小问题升级成故障。这与词库缺失时宁可退出的取舍不同，因为代价不同。
    pub fn from_pairs(get: impl Fn(&str) -> Option<String>) -> Self {
        let d = Settings::default();
        Settings {
            hotkey: get(keys::HOTKEY)
                .and_then(|s| s.parse().ok())
                .unwrap_or(d.hotkey),
            autostart: get(keys::AUTOSTART)
                .map(|s| s == "1")
                .unwrap_or(d.autostart),
            // 风格与明暗各自独立解析，任一项读不到就退回默认——包括从旧版的
            // 单一 `skin` 键迁移过来的那一半（见 `skin_from_legacy`）。
            style: get(keys::SKIN_STYLE)
                .and_then(|s| style_from_str(&s))
                .or_else(|| legacy(&get).map(|(st, _)| st))
                .unwrap_or(d.style),
            mode: get(keys::SKIN_MODE)
                .and_then(|s| mode_from_str(&s))
                .or_else(|| legacy(&get).map(|(_, m)| m))
                .unwrap_or(d.mode),
            expand_en: get(keys::EXPAND_EN)
                .map(|s| s == "1")
                .unwrap_or(d.expand_en),
            ecdict: get(keys::ECDICT)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            cedict: get(keys::CEDICT)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            // 读回来就钳一次。库里的值可能是手改的、也可能是旧版本在别的窗口尺寸下
            // 写的，而一个 5px 或 5000px 的左栏会让界面直接不可用——那属于「读不到
            // 就退回默认」这条策略要挡住的同一类事故。
            left_pane_w: get(keys::LEFT_PANE_W)
                .and_then(|s| s.parse::<i32>().ok())
                .map(|w| w.clamp(LEFT_PANE_W_MIN, LEFT_PANE_W_MAX))
                .unwrap_or(d.left_pane_w),
        }
    }

    /// 展开为键值对，供逐条写库。
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            (keys::HOTKEY, self.hotkey.to_string()),
            (keys::AUTOSTART, bool_str(self.autostart).into()),
            (keys::SKIN_STYLE, style_str(self.style).into()),
            (keys::SKIN_MODE, mode_str(self.mode).into()),
            (keys::EXPAND_EN, bool_str(self.expand_en).into()),
            (keys::ECDICT, path_str(&self.ecdict)),
            (keys::CEDICT, path_str(&self.cedict)),
            (keys::LEFT_PANE_W, self.left_pane_w.to_string()),
        ]
    }
}

fn bool_str(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

fn path_str(p: &Option<PathBuf>) -> String {
    p.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// 风格的存储表示。用稳定的短名而非序号——序号会在枚举增删成员时错位，让用户的
/// 配色莫名其妙变成另一套。
fn style_str(k: SkinStyle) -> &'static str {
    match k {
        SkinStyle::Plain => "plain",
        SkinStyle::Paper => "paper",
        SkinStyle::Focus => "focus",
    }
}

fn style_from_str(s: &str) -> Option<SkinStyle> {
    match s {
        "plain" => Some(SkinStyle::Plain),
        "paper" => Some(SkinStyle::Paper),
        "focus" => Some(SkinStyle::Focus),
        _ => None,
    }
}

fn mode_str(m: SkinMode) -> &'static str {
    match m {
        SkinMode::Light => "light",
        SkinMode::Dark => "dark",
        SkinMode::System => "system",
    }
}

fn mode_from_str(s: &str) -> Option<SkinMode> {
    match s {
        "light" => Some(SkinMode::Light),
        "dark" => Some(SkinMode::Dark),
        "system" => Some(SkinMode::System),
        _ => None,
    }
}

/// 把旧版那个单一的 `skin` 键拆成风格 + 明暗。
///
/// 旧版三选一（`light`/`paper`/`dark`）把风格与明暗揉在了一起，新版拆开之后每一项
/// 都有唯一对应：`light` 就是简约的亮档，`dark` 就是专注的暗档。
///
/// **必须迁移而不是让它退回默认**：用户数据要跨部署存活（ADR-0011），而一个用了半年
/// 深色的人升级后被扔回浅色，是这条原则最直观的反例。旧键只读不写——新版存的是两个
/// 新键，那条旧记录留在库里无人问津，也无害。
fn legacy(get: &impl Fn(&str) -> Option<String>) -> Option<(SkinStyle, SkinMode)> {
    match get(keys::LEGACY_SKIN)?.as_str() {
        "light" => Some((SkinStyle::Plain, SkinMode::Light)),
        "paper" => Some((SkinStyle::Paper, SkinMode::Light)),
        "dark" => Some((SkinStyle::Focus, SkinMode::Dark)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn 热键往返一致() {
        for s in [
            "Ctrl+Alt+D",
            "Ctrl+K",
            "Alt+Shift+9",
            "Ctrl+Alt+Shift+Q",
            // 功能键：单用与带修饰键都要能原样往返。
            "F1",
            "F12",
            "Ctrl+F5",
        ] {
            let spec: HotkeySpec = s.parse().unwrap();
            assert_eq!(spec.to_string(), s, "往返后字符串应不变");
        }
    }

    /// 修饰键顺序与大小写不影响解析，但输出**只有一种形式**——否则同一组合会在库里
    /// 存出多种写法，比较与去重都会失准。
    #[test]
    fn 解析容错但输出规范() {
        let a: HotkeySpec = "alt+ctrl+d".parse().unwrap();
        let b: HotkeySpec = "Ctrl+Alt+D".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a.to_string(), "Ctrl+Alt+D");
    }

    #[test]
    fn 缺少主键或按键非法时报错() {
        assert!("Ctrl+Alt".parse::<HotkeySpec>().is_err());
        assert!("".parse::<HotkeySpec>().is_err());
        assert!("F0".parse::<HotkeySpec>().is_err(), "功能键从 F1 起");
        assert!("F13".parse::<HotkeySpec>().is_err(), "功能键到 F12 止");
        assert!("Ctrl+回".parse::<HotkeySpec>().is_err(), "非 ASCII 主键");
    }

    /// `F` 既是合法的字符主键、又是功能键的前缀，解析顺序不能把 `F1` 截成 `F`。
    #[test]
    fn 单个_f_是字符主键而_f1_是功能键() {
        assert_eq!(
            "Ctrl+F".parse::<HotkeySpec>().unwrap().key,
            HotkeyKey::Char('F')
        );
        assert_eq!("F1".parse::<HotkeySpec>().unwrap().key, HotkeyKey::Func(1));
    }

    /// 无修饰的**字母数字**热键会吞掉该字符在所有程序中的输入，界面须拦住；
    /// 而功能键不参与文字输入，单用是安全的——这条区分正是主键分两类的理由。
    #[test]
    fn 只有字母数字主键才要求修饰键() {
        let bare: HotkeySpec = "D".parse().unwrap();
        assert!(!bare.is_safe(), "裸字母会吞掉打字");
        assert!(HotkeySpec::default().is_safe());
        assert!(
            "F1".parse::<HotkeySpec>().unwrap().is_safe(),
            "功能键可单用"
        );
        assert!("Ctrl+F1".parse::<HotkeySpec>().unwrap().is_safe());
    }

    /// 下拉框列出的每一项都要能原样选回来——`index` 与 `all` 一旦错位，用户选的是
    /// F5、存下来的却是别的键，而这种错位在界面上看不出来（下拉照样显示他选的那项）。
    #[test]
    fn 主键表与下标互为反函数() {
        let all = HotkeyKey::all();
        assert_eq!(all.len(), 12 + 26 + 10, "F1–F12 + A–Z + 0–9");
        for (i, k) in all.iter().enumerate() {
            assert_eq!(k.index(), i, "{k} 的下标应是 {i}");
        }
    }

    /// 功能键排在最前，用户拉开下拉就看得见「可以单独用一个 F 键」这条可能性。
    #[test]
    fn 功能键排在主键表最前() {
        let all = HotkeyKey::all();
        assert_eq!(all[0], HotkeyKey::Func(1));
        assert_eq!(all[11], HotkeyKey::Func(12));
        assert_eq!(all[12], HotkeyKey::Char('A'));
    }

    /// F1–F12 必须落在 VK_F1..=VK_F12（0x70..=0x7B）——注册全局热键靠的就是这个码，
    /// 算错一位就会绑到别的键上，而那是运行期才看得见的错误。
    #[test]
    fn 功能键映射到正确的虚拟键码() {
        use windui::event::Key;
        for (n, vk) in [(1u8, 0x70u32), (5, 0x74), (12, 0x7B)] {
            let spec: HotkeySpec = format!("F{n}").parse().unwrap();
            assert_eq!(
                spec.to_hotkey().key,
                Key::Other(vk),
                "F{n} 应映射到 {vk:#x}"
            );
        }
    }

    #[test]
    fn 设置往返一致() {
        let s = Settings {
            hotkey: "Ctrl+Shift+K".parse().unwrap(),
            autostart: true,
            style: SkinStyle::Focus,
            mode: SkinMode::Dark,
            expand_en: true,
            ecdict: Some(PathBuf::from(r"D:\a\ec.db")),
            cedict: None,
            // 特意取非默认值：等于默认时，`from_pairs` 的兜底分支也能让断言通过，
            // 那就验不出这一项到底有没有真的往返。
            left_pane_w: 340,
        };
        let map: HashMap<String, String> = s
            .to_pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let back = Settings::from_pairs(|k| map.get(k).cloned());
        assert_eq!(back, s);
    }

    /// 空库读出的是默认设置，而非报错或空壳。
    #[test]
    fn 空库给出默认设置() {
        assert_eq!(Settings::from_pairs(|_| None), Settings::default());
    }

    /// 值被手改坏或来自更新的版本时**退回默认**，不让程序起不来。
    #[test]
    fn 无法识别的值退回默认() {
        let bad = |k: &str| match k {
            keys::HOTKEY => Some("这不是热键".to_string()),
            keys::SKIN_STYLE => Some("neon".to_string()),
            keys::SKIN_MODE => Some("sepia".to_string()),
            // 旧键也给个坏值：迁移那条兜底同样不许让程序起不来。
            keys::LEGACY_SKIN => Some("neon".to_string()),
            _ => None,
        };
        let s = Settings::from_pairs(bad);
        assert_eq!(s.hotkey, HotkeySpec::default());
        assert_eq!(s.style, SkinStyle::Plain);
        assert_eq!(s.mode, SkinMode::System);
    }

    /// 配色按稳定短名存储：枚举增删成员时用户的选择不会错位到另一套。
    #[test]
    fn 配色按名字存而非序号() {
        for k in SkinStyle::ALL {
            assert_eq!(style_from_str(style_str(k)), Some(k));
        }
        for m in SkinMode::ALL {
            assert_eq!(mode_from_str(mode_str(m)), Some(m));
        }
        assert_eq!(style_str(SkinStyle::Paper), "paper");
    }

    /// 旧版的单一 `skin` 键要能迁移成风格 + 明暗。
    ///
    /// 用户数据跨部署存活是硬约束（ADR-0011）。没有这条迁移，一个用了半年深色的人
    /// 升级后会被扔回浅色——而那看起来像是设置丢了，不像是升级。
    #[test]
    fn 旧版皮肤键迁移为风格加明暗() {
        for (旧, 风格, 明暗) in [
            ("light", SkinStyle::Plain, SkinMode::Light),
            ("paper", SkinStyle::Paper, SkinMode::Light),
            ("dark", SkinStyle::Focus, SkinMode::Dark),
        ] {
            let s = Settings::from_pairs(|k| (k == keys::LEGACY_SKIN).then(|| 旧.to_string()));
            assert_eq!(s.style, 风格, "旧值 {旧} 的风格");
            assert_eq!(s.mode, 明暗, "旧值 {旧} 的明暗");
        }
    }

    /// 新键在场时压过旧键：迁移只在新键缺席时兜底，否则用户改过的设置会被一条
    /// 早该退休的旧记录顶回去。
    #[test]
    fn 新键优先于旧版皮肤键() {
        let s = Settings::from_pairs(|k| match k {
            keys::LEGACY_SKIN => Some("dark".to_string()),
            keys::SKIN_STYLE => Some("paper".to_string()),
            keys::SKIN_MODE => Some("system".to_string()),
            _ => None,
        });
        assert_eq!(s.style, SkinStyle::Paper);
        assert_eq!(s.mode, SkinMode::System);
    }

    /// 旧键写不进库：`to_pairs` 只产出新键，否则每次存盘都把那条旧记录重新写活，
    /// 迁移就永远收不了尾。
    #[test]
    fn 旧版皮肤键不再写入() {
        let 键: Vec<&str> = Settings::default()
            .to_pairs()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(!键.contains(&keys::LEGACY_SKIN), "旧键不该再被写出：{键:?}");
        assert!(键.contains(&keys::SKIN_STYLE));
        assert!(键.contains(&keys::SKIN_MODE));
    }
}
