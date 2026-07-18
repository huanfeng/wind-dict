//! wind-dict：常驻托盘的桌面词典。
//!
//! 设计的权威出处是 `CONTEXT.md`（术语表）与 `docs/adr/`（架构决策），改代码前先读。
//!
//! 本文件只负责组装：词库 → 离线词典 → 界面 → 常驻外壳（热键 / 托盘 / 显隐）。

// 常驻托盘应用不该带控制台窗口。debug 期保留，便于看 panic 与日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use windui::prelude::*;

use wind_dict::source::offline::OfflineDictionary;
use wind_dict::ui;

/// 唤起热键：Ctrl+Alt+D。
///
/// 键位尚未做成可配置——这是已知的开放问题（热键是全局独占资源，撞键时用户
/// 目前只能改代码）。
const HOTKEY_CHAR: char = 'D';

fn main() {
    let (ecdict, cedict) = dict_paths();

    // 词库缺失是致命的：离线词典是本工具的主体，没有它整个程序没有意义。
    // 与其起一个查什么都「未收录」的空壳（那是在撒谎——术语表里「一无所获」的意思是
    // 词典确实没有这个词，不是「我没词库」），不如直接失败。
    let dict = match OfflineDictionary::open(&ecdict, &cedict) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("wind-dict：无法打开词库\n{e:?}\n");
            eprintln!("英汉词库：{}", ecdict.display());
            eprintln!("汉英词库：{}", cedict.display());
            eprintln!("\n构建词库：");
            eprintln!("  cargo run --release --example build_ecdict -- ecdict.csv ecdict.db");
            eprintln!("  cargo run --release --example build_cedict -- cedict_ts.u8 cedict.db");
            std::process::exit(1);
        }
    };

    let tray = Tray::new()
        .tooltip(format!("wind-dict — Ctrl+Alt+{HOTKEY_CHAR} 查词"))
        .icon_rgba(16, 16, &icon())
        .on_left_click(|ctx| ctx.show_window())
        .on_double_click(|ctx| ctx.show_window())
        .menu(vec![
            TrayMenuItem::item("查词", |ctx| ctx.show_window()),
            TrayMenuItem::separator(),
            // 唯一的退出途径：窗口关闭与 ESC 都只是收起来（hide_on_close）。
            TrayMenuItem::item("退出", |ctx| ctx.quit()),
        ]);

    App::new("wind-dict", 460, 560)
        .min_size(360, 400)
        .tray(tray)
        // 常驻：启动不闪窗口，关闭只收起，进程始终活着等热键。见 ADR-0006。
        .start_hidden()
        .hide_on_close()
        .hotkey(Hotkey::new(Key::Char(HOTKEY_CHAR)).ctrl().alt(), |ctx| {
            ctx.show_window()
        })
        .screenshot_from_args()
        .content(ui::build(dict))
        .run();
}

/// 词库路径：命令行参数优先，否则取 exe 同目录。
///
/// exe 同目录而非工作目录：常驻工具从托盘/热键启动时，工作目录是什么完全不可控。
fn dict_paths() -> (PathBuf, PathBuf) {
    let mut args = std::env::args().skip(1).filter(|a| !a.starts_with("--"));
    if let (Some(a), Some(b)) = (args.next(), args.next()) {
        return (PathBuf::from(a), PathBuf::from(b));
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default();
    (dir.join("ecdict.db"), dir.join("cedict.db"))
}

/// 托盘图标：16×16 纯色 RGBA8（占位，免捆绑资源文件）。
fn icon() -> Vec<u8> {
    [0x4C, 0x8B, 0xF5, 0xFF].repeat(16 * 16)
}
