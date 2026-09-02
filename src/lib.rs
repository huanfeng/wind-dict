//! wind-dict：常驻托盘的桌面词典。
//!
//! 设计的权威出处是两份文档，改代码前先读它们：
//! - `CONTEXT.md` —— 术语表。代码里的命名直接由它推出，含一批**明令禁止**的名字。
//! - `docs/adr/` —— 架构决策记录。其中数条专门用于阻止「看起来更优」的重构。
//!
//! 最容易被误改的三条：
//! 1. **词典与译源不统一抽象**（adr/0008）——可信度与数据形状都不同。
//! 2. **译源绝不自动兜底**（adr/0002）——离线未命中时提示用户，不静默发网络请求。
//! 3. **补全只由离线词典驱动**（`domain::Wordlist`）——查询源永不逐键触发。

/// 应用的**显示名**：窗口标题、自绘标题栏、托盘提示、消息框，四处同一个来源。
///
/// 与「标识名」分开：`wind-dict` 仍是 crate 名、exe 名、部署目录名与自启注册表项名
/// （`autostart::VALUE_NAME`）。那些是**机器读的键**——改动它们会让已部署的实例
/// 认不出自己（旧自启项还指着旧路径，于是开机起两个），而且用户根本看不到。
/// 显示名归显示名，标识名归标识名，混在一起的代价全落在升级路径上。
pub const APP_TITLE: &str = "清风词典";

/// 界面上显示的名字。**dev 构建带后缀**。
///
/// 两个变体常常同时装着（部署目录、用户数据、自启项都是分开的），而窗口标题、任务栏、
/// 托盘提示上它们此前一模一样——手上开着两个窗口时分不出哪个是哪个，改完代码验证时
/// 很容易对着 release 那个看半天。
///
/// 判据与 `userdata::data_dir()`、`autostart::VALUE_NAME` 同源：`debug_assertions`。
pub fn app_title() -> &'static str {
    if cfg!(debug_assertions) {
        concat!("清风词典", " (开发版)")
    } else {
        APP_TITLE
    }
}

pub mod autostart;
pub mod domain;
pub mod html;
pub mod icon;
pub mod settings;
pub mod skin;
pub mod source;
pub mod store;
pub mod ui;
