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

use crate::skin::SkinKind;

/// 唤起热键的按键组合。
///
/// 用自己的类型而非直接存 windui 的 `Hotkey`：后者是框架的注册用类型，既不能
/// 序列化也不便比较，而热键要写进数据库、要在界面上显示、要能被解析回来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// 主键。只支持单个字母/数字——热键是全局注册，功能键与符号在各语言键盘上
    /// 的位置差异太大，限制在字母数字内可避免「在别人机器上按不出来」。
    pub key: char,
}

impl Default for HotkeySpec {
    /// Ctrl+Alt+D。
    fn default() -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: false,
            key: 'D',
        }
    }
}

impl HotkeySpec {
    /// 是否带了至少一个修饰键。
    ///
    /// 无修饰的全局热键会**吞掉该字母在所有程序中的输入**——用户按一下 D 就唤起
    /// 词典，等于没法打字了。
    ///
    /// 当前设置页的热键行是只读展示，没有输入入口，故此方法尚无调用点；**将来开放
    /// 改键时必须用它拦住**。之所以现在就写下，是因为那时容易只想着「解析成功即可」。
    pub fn has_modifier(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// 转成 windui 的注册用类型。
    ///
    /// 放在这里而非 `main`：改键时界面也要用它（`ui::State::set_hotkey` 走
    /// `HotkeyHandle::rebind`），两处各写一遍必然漂移。
    pub fn to_hotkey(self) -> windui::event::Hotkey {
        let mut hk = windui::event::Hotkey::new(windui::event::Key::Char(self.key));
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
            key: '\0',
        };
        let mut seen_key = false;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => spec.ctrl = true,
                "alt" => spec.alt = true,
                "shift" => spec.shift = true,
                other => {
                    let mut chars = other.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) if c.is_ascii_alphanumeric() => {
                            spec.key = c.to_ascii_uppercase();
                            seen_key = true;
                        }
                        _ => return Err(format!("无法识别的按键：{part}")),
                    }
                }
            }
        }
        if !seen_key {
            return Err(format!("热键缺少主键：{s}"));
        }
        Ok(spec)
    }
}

/// 一份完整设置。字段均有默认值，故读不到任何一项时也能给出可用的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// 唤起热键。
    pub hotkey: HotkeySpec,
    /// 是否开机自启。
    pub autostart: bool,
    /// 皮肤。
    pub skin: SkinKind,
    /// 词条是否默认展开英英释义。
    ///
    /// 只影响**默认**展开与否，不影响该区是否存在——英英释义始终可展开，这是
    /// 刻意的产品决定（见 `ui::entry_view`）。
    pub expand_en: bool,
    /// 词库路径覆盖。`None` = 沿用命令行参数或 exe 同目录的默认查找。
    pub ecdict: Option<PathBuf>,
    pub cedict: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: HotkeySpec::default(),
            autostart: false,
            skin: SkinKind::Light,
            expand_en: false,
            ecdict: None,
            cedict: None,
        }
    }
}

/// 设置在数据库里的键名。集中在此，避免字符串散落各处拼错。
pub mod keys {
    pub const HOTKEY: &str = "hotkey";
    pub const AUTOSTART: &str = "autostart";
    pub const SKIN: &str = "skin";
    pub const EXPAND_EN: &str = "expand_en";
    pub const ECDICT: &str = "ecdict";
    pub const CEDICT: &str = "cedict";
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
            skin: get(keys::SKIN)
                .and_then(|s| skin_from_str(&s))
                .unwrap_or(d.skin),
            expand_en: get(keys::EXPAND_EN)
                .map(|s| s == "1")
                .unwrap_or(d.expand_en),
            ecdict: get(keys::ECDICT)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            cedict: get(keys::CEDICT)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        }
    }

    /// 展开为键值对，供逐条写库。
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            (keys::HOTKEY, self.hotkey.to_string()),
            (keys::AUTOSTART, bool_str(self.autostart).into()),
            (keys::SKIN, skin_str(self.skin).into()),
            (keys::EXPAND_EN, bool_str(self.expand_en).into()),
            (keys::ECDICT, path_str(&self.ecdict)),
            (keys::CEDICT, path_str(&self.cedict)),
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

/// 皮肤的存储表示。用稳定的短名而非序号——序号会在 `SkinKind` 增删成员时错位，
/// 让用户的皮肤莫名其妙变成另一套。
fn skin_str(k: SkinKind) -> &'static str {
    match k {
        SkinKind::Light => "light",
        SkinKind::Paper => "paper",
        SkinKind::Dark => "dark",
    }
}

fn skin_from_str(s: &str) -> Option<SkinKind> {
    match s {
        "light" => Some(SkinKind::Light),
        "paper" => Some(SkinKind::Paper),
        "dark" => Some(SkinKind::Dark),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn 热键往返一致() {
        for s in ["Ctrl+Alt+D", "Ctrl+K", "Alt+Shift+9", "Ctrl+Alt+Shift+Q"] {
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
        assert!("Ctrl+F1".parse::<HotkeySpec>().is_err(), "功能键不支持");
        assert!("".parse::<HotkeySpec>().is_err());
    }

    /// 无修饰键的全局热键会吞掉该字母在所有程序中的输入，界面须拦住。
    #[test]
    fn 无修饰键可被识别出来() {
        let bare: HotkeySpec = "D".parse().unwrap();
        assert!(!bare.has_modifier());
        assert!(HotkeySpec::default().has_modifier());
    }

    #[test]
    fn 设置往返一致() {
        let s = Settings {
            hotkey: "Ctrl+Shift+K".parse().unwrap(),
            autostart: true,
            skin: SkinKind::Dark,
            expand_en: true,
            ecdict: Some(PathBuf::from(r"D:\a\ec.db")),
            cedict: None,
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
            keys::SKIN => Some("neon".to_string()),
            _ => None,
        };
        let s = Settings::from_pairs(bad);
        assert_eq!(s.hotkey, HotkeySpec::default());
        assert_eq!(s.skin, SkinKind::Light);
    }

    /// 皮肤按稳定短名存储：`SkinKind` 增删成员时用户的选择不会错位到另一套。
    #[test]
    fn 皮肤按名字存而非序号() {
        for k in SkinKind::ALL {
            assert_eq!(skin_from_str(skin_str(k)), Some(k));
        }
        assert_eq!(skin_str(SkinKind::Paper), "paper");
    }
}
