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
    /// Ctrl+Alt+X。
    ///
    /// 改默认值**不影响已经用起来的人**：热键存在设置里，只有从没改过的新装机器才会
    /// 拿到这个值。
    fn default() -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: false,
            key: HotkeyKey::Char('X'),
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
    /// 随程序分发的三份词库所在的目录。`None` = exe 同目录。
    ///
    /// **一个目录，不是三条路径**。三份库（英汉、汉英、字形）永远住在一起、随同一次
    /// 部署整体替换，让它们各配一条路径，等于允许一种从不发生、却处处要防的状态：
    /// 英汉指向新版、汉英还指着旧版。
    ///
    /// 这也把字形库一并收编了：它此前**没有设置项**，靠「在英汉库旁边找」这条隐式
    /// 约定定位——同一件事有两套规则，其中一套还是不可见的。
    pub dict_dir: Option<PathBuf>,
    /// 用户词典所在的目录。`None` = 用默认目录（见 `source::user::default_dir`）。
    ///
    /// 存**一个目录**而非一串文件路径：词典是用户往里丢的，不是逐个登记的。丢进去
    /// 就能用、拿走就没了，这与「装一本词典」是同一件事；逐个登记则要求用户在文件
    /// 管理器与设置页之间把同一件事做两遍。
    ///
    /// 目录可改而非写死，是因为词库动辄几百 MB 到几 GB，用户多半有自己的存放地方
    /// （另一块盘、同步目录）。但默认值必须落在**卸载删不到**的地方——见
    /// `source::user::default_dir` 与 ADR-0011。
    pub user_dict_dir: Option<PathBuf>,
    /// 被用户关掉的用户词典，存**文件名**。
    ///
    /// 存「关掉的」而不是「开着的」，是这套设计的关键一处：新丢进目录的词典必须
    /// **默认可用**——否则「放进去就能用」就退化成「放进去再来设置页开一下」，
    /// 与手动逐个添加没有区别。
    ///
    /// 按文件名而非完整路径，是为了用户换目录（把词库从 C 盘挪到 D 盘）之后这些
    /// 开关还认得出是同一本词典。
    pub disabled_dicts: Vec<String>,
    /// 各来源的自定义显示名：`(稳定键, 名字)`。
    ///
    /// 存**键**而非下标或路径：内置那两份的键是 `ecdict` / `cedict`，用户词典的键是
    /// 文件名——换目录、装卸别的词典都不会让这份映射错位。
    ///
    /// 只存改过的那些。没有条目就用默认名，故「恢复默认」等同于删掉这一条，不需要
    /// 另存一个「是否用了默认名」的标志——那种标志迟早会与实际的名字对不上。
    pub dict_names: Vec<(String, String)>,
    /// 词典顺序：来源的**稳定键**，从前到后。
    ///
    /// 一份统一的名单，内置词库、用户词典、码表方案都在里面——用户眼里它们都是「一本
    /// 词典」，分三处各排各的等于让他在三个地方拼出一个顺序。
    ///
    /// 只存用户排过的那些，**不在名单里的排在末尾**（保持各自的自然序）：新装一本词典
    /// 时它出现在最后，而不是插进用户精心排好的序列中间某处。
    pub dict_order: Vec<String>,
    /// 是否启用码表反查（由字查它在某个输入方案里的编码与拆分）。
    ///
    /// 默认**开**：数据来自机器上已装的清风输入法，探测不到就什么也不会出现，开着没有
    /// 副作用；而默认关意味着装了兄弟软件的用户还得先来设置页找一遍才知道有这功能。
    pub codetables: bool,
    /// 查词组时要不要逐字列出编码。
    ///
    /// 默认**开**：关掉的话查任何多字词，码表那一页都是空的，用户多半会以为功能坏了。
    /// 但逐字铺开确实会在释义中间夹三五行编码，嫌乱的人可以关——那时只有单字给编码。
    pub code_multi_char: bool,
    /// 手动指定的方案目录，自动探测之外的补充。
    ///
    /// 探测走的是注册表里的安装位置与 `datadir.conf`（见 `source::windinput`），覆盖不到
    /// 便携版、解压即用、或装在别处的情形——那时用户自己指一下。存**目录**而不是单个
    /// 方案文件，与用户词典目录同一条理由：丢进去就能用。
    pub codetable_dirs: Vec<PathBuf>,
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
            dict_dir: None,
            user_dict_dir: None,
            disabled_dicts: Vec::new(),
            dict_names: Vec::new(),
            dict_order: Vec::new(),
            codetables: true,
            code_multi_char: true,
            codetable_dirs: Vec::new(),
            left_pane_w: LEFT_PANE_W_DEFAULT,
        }
    }
}

/// 设置在数据库里的键名。集中在此，避免字符串散落各处拼错。
impl Settings {
    /// 某个来源此刻的显示名。没改过就是 `default`。
    pub fn dict_name(&self, key: &str, default: &str) -> String {
        self.dict_names
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| default.to_string())
    }

    /// 改名。空名字视同**恢复默认**——直接删掉这一条，而不是存一个空串。
    ///
    /// 存空串的话，`dict_name` 就得再判一次「空的算不算数」，而那条判断迟早会有
    /// 一处漏掉；不存则「有没有这一条」本身就是答案。
    pub fn set_dict_name(&mut self, key: &str, name: &str) {
        let name = name.trim();
        self.dict_names.retain(|(k, _)| k != key);
        if !name.is_empty() {
            self.dict_names.push((key.to_string(), name.to_string()));
        }
    }
}

pub mod keys {
    pub const HOTKEY: &str = "hotkey";
    pub const AUTOSTART: &str = "autostart";
    pub const SKIN_STYLE: &str = "skin_style";
    pub const SKIN_MODE: &str = "skin_mode";
    /// 旧版的皮肤键，**只读不写**。见 `super::legacy`。
    pub const LEGACY_SKIN: &str = "skin";
    pub const EXPAND_EN: &str = "expand_en";
    pub const DICT_DIR: &str = "dict_dir";
    /// 旧版按文件存的词库路径。**只读不写**，仅用于迁移到 [`DICT_DIR`]。
    pub const LEGACY_ECDICT: &str = "ecdict";
    pub const USER_DICT_DIR: &str = "user_dict_dir";
    pub const DISABLED_DICTS: &str = "disabled_dicts";
    pub const DICT_NAMES: &str = "dict_names";
    pub const DICT_ORDER: &str = "dict_order";
    pub const CODETABLES: &str = "codetables";
    pub const CODE_MULTI_CHAR: &str = "code_multi_char";
    pub const CODETABLE_DIRS: &str = "codetable_dirs";
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
            dict_dir: get(keys::DICT_DIR)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                // 旧版按文件存路径。取英汉库所在的目录迁移过来——三份库本就同处一处，
                // 故这个推断在旧数据上必然成立。不迁移的话，设置过词库路径的用户会在
                // 升级后被静默拽回默认目录。
                .or_else(|| {
                    get(keys::LEGACY_ECDICT)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .and_then(|p| p.parent().map(PathBuf::from))
                }),
            user_dict_dir: get(keys::USER_DICT_DIR)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            dict_names: get(keys::DICT_NAMES)
                .map(|s| pairs_of(&s))
                .unwrap_or_else(|| d.dict_names.clone()),
            disabled_dicts: get(keys::DISABLED_DICTS)
                .map(|s| lines_of(&s))
                .unwrap_or_default(),
            dict_order: get(keys::DICT_ORDER)
                .map(|s| lines_of(&s))
                .unwrap_or_default(),
            codetables: get(keys::CODETABLES)
                .map(|s| s == "1")
                .unwrap_or(d.codetables),
            code_multi_char: get(keys::CODE_MULTI_CHAR)
                .map(|s| s == "1")
                .unwrap_or(d.code_multi_char),
            codetable_dirs: get(keys::CODETABLE_DIRS)
                .map(|s| lines_of(&s).into_iter().map(PathBuf::from).collect())
                .unwrap_or_default(),
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
            (keys::DICT_DIR, path_str(&self.dict_dir)),
            (keys::USER_DICT_DIR, path_str(&self.user_dict_dir)),
            (keys::DISABLED_DICTS, self.disabled_dicts.join("\n")),
            (
                keys::DICT_NAMES,
                self.dict_names
                    .iter()
                    .map(|(k, v)| format!("{k}\t{v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (keys::DICT_ORDER, self.dict_order.join("\n")),
            (keys::CODETABLES, bool_str(self.codetables).into()),
            (keys::CODE_MULTI_CHAR, bool_str(self.code_multi_char).into()),
            (
                keys::CODETABLE_DIRS,
                self.codetable_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    ),
            ),
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

/// 一串文件名存成一个值：换行分隔。
///
/// Windows 的文件名不允许含换行（`<>:"/\|?*` 与控制字符都被文件系统挡在外面），
/// 故这个分隔符不会与内容冲突——用分号或逗号就会，那两个在文件名里合法。
fn lines_of(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 解析「键 TAB 名字」逐行的映射。
///
/// 分隔符取制表符而非等号或冒号：键里有文件名（`朗文=当代.mdx` 完全合法），而
/// Windows 的文件名不允许含制表符与换行，故这个分隔符不会与内容冲突。
///
/// 一行拆不出两段就整行丢掉，不猜。设置库可能被手改或被旧版本写坏，而一个半截的
/// 映射会表现为「某本词典的名字莫名其妙变成一串键」——比它干脆不生效更难查。
fn pairs_of(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
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

    /// 旧版按**文件**存词库路径（`ecdict` / `cedict` 两个键）。取英汉库所在的目录
    /// 迁移过来——三份库本就同处一处，故这个推断在旧数据上必然成立。
    ///
    /// 改名与恢复默认。
    ///
    /// 「清空即恢复默认」是这套设计的关键一处：没有它，用户改错了名字就只能再编一个
    /// 名字，回不到我们给的那个。而它成立的前提是**空名字不落库**——落一个空串的话，
    /// `dict_name` 就得再判一次「空的算不算数」，那条判断迟早有一处会漏。
    #[test]
    fn 词典改名与恢复默认() {
        let mut s = Settings::default();
        assert_eq!(
            s.dict_name("ecdict", "简明英汉字典"),
            "简明英汉字典",
            "没改过就是默认名"
        );

        s.set_dict_name("ecdict", "我的英汉");
        assert_eq!(s.dict_name("ecdict", "简明英汉字典"), "我的英汉");
        assert_eq!(s.dict_names.len(), 1);

        s.set_dict_name("ecdict", "又改一次");
        assert_eq!(s.dict_names.len(), 1, "改第二次不该多出一条");
        assert_eq!(s.dict_name("ecdict", "简明英汉字典"), "又改一次");

        s.set_dict_name("ecdict", "   ");
        assert!(s.dict_names.is_empty(), "全空白等同清空，不落库");
        assert_eq!(s.dict_name("ecdict", "简明英汉字典"), "简明英汉字典");
    }

    /// 半截的映射整行丢掉，不猜。
    ///
    /// 设置库可能被手改或被旧版本写坏，而一个猜出来的映射会表现为「某本词典的名字
    /// 莫名其妙变成一串键」——比它干脆不生效更难查。
    #[test]
    fn 词典名映射只认完整的行() {
        let got = pairs_of(
            "ecdict	英汉
没有分隔符
	cedict少了键
cedict	
 a 	 b ",
        );
        assert_eq!(
            got,
            vec![
                ("ecdict".to_string(), "英汉".to_string()),
                ("a".to_string(), "b".to_string()),
            ],
        );
    }

    /// 不迁移的话，设置过词库路径的用户升级后会被静默拽回程序同目录：查得到词、
    /// 一切正常，只是用的不是他指定的那份库。这种「没坏但不对」最难被发现。
    #[test]
    fn 旧版的词库文件路径迁移成目录() {
        let 读 = |m: &HashMap<String, String>| {
            let m = m.clone();
            Settings::from_pairs(move |k| m.get(k).cloned())
        };

        let legacy = HashMap::from([(
            keys::LEGACY_ECDICT.to_string(),
            r"D:\dict\ecdict.db".to_string(),
        )]);
        assert_eq!(读(&legacy).dict_dir, Some(PathBuf::from(r"D:\dict")));

        // 新键存在时以新键为准，不被旧键顶掉。
        let both = HashMap::from([
            (
                keys::LEGACY_ECDICT.to_string(),
                r"D:\dict\ecdict.db".to_string(),
            ),
            (keys::DICT_DIR.to_string(), r"E:\新目录".to_string()),
        ]);
        assert_eq!(读(&both).dict_dir, Some(PathBuf::from(r"E:\新目录")));

        // 两个都没有就是没设置过，走默认（程序同目录）。
        assert_eq!(读(&HashMap::new()).dict_dir, None);
        // 空串等同没设置——旧版把 `None` 也写成空串存了进去。
        let empty = HashMap::from([(keys::LEGACY_ECDICT.to_string(), String::new())]);
        assert_eq!(读(&empty).dict_dir, None);
    }

    #[test]
    fn 设置往返一致() {
        let s = Settings {
            hotkey: "Ctrl+Shift+K".parse().unwrap(),
            autostart: true,
            style: SkinStyle::Focus,
            mode: SkinMode::Dark,
            expand_en: true,
            dict_dir: Some(PathBuf::from(r"D:\a\dict")),
            user_dict_dir: Some(PathBuf::from(r"E:\我的词库")),
            // 两条而非一条：一条存不出分隔符有没有用对。
            disabled_dicts: vec!["Oxford.mdx".into(), "朗文 当代.mdx".into()],
            // 两条，且其中一条的键带空格：分隔符用的是制表符，验它不会被空格搅乱。
            dict_names: vec![
                ("ecdict".into(), "我的英汉".into()),
                ("朗文 当代.mdx".into(), "朗文".into()),
            ],
            // 三条，验顺序本身也往返得回来（而不只是集合相等）。
            dict_order: vec!["cedict".into(), "ecdict".into(), "wubi86".into()],
            // 取非默认值（默认为 true）：等于默认时兜底分支也能让断言过，就验不出往返。
            codetables: false,
            // 同样取非默认值（默认为 true）。
            code_multi_char: false,
            // 两条，且其中一条带空格与中文：目录列表用换行分隔，验它不被路径里的空格搅乱。
            codetable_dirs: vec![
                std::path::PathBuf::from(r"D:\方案"),
                std::path::PathBuf::from(r"E:\my schemas"),
            ],
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
