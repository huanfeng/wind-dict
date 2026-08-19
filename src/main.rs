//! wind-dict：常驻托盘的桌面词典。
//!
//! 设计的权威出处是 `CONTEXT.md`（术语表）与 `docs/adr/`（架构决策），改代码前先读。
//!
//! 本文件只负责组装：词库 → 离线词典 → 界面 → 常驻外壳（热键 / 托盘 / 显隐）。

// 常驻托盘应用不该带控制台窗口。debug 期保留，便于看 panic 与日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use windui::prelude::*;

use wind_dict::settings::Settings;
use wind_dict::source::offline::OfflineDictionary;
use wind_dict::store::userdata::{UserData, UserDataState};
use wind_dict::ui;

fn main() {
    // 用户数据先开：设置存在其中，而词库路径、热键、皮肤都要由设置决定。
    //
    // 用户数据打不开**不致命**：查询是主体功能，收藏与历史是其增益，为后者让整个
    // 工具起不来是不成比例的。但也**不静默降级**——不可用的原因带进界面如实告知
    // （见 `ui::build`）。设置在这种情况下退回默认，程序照常可用。
    let user = open_userdata();
    let settings = match &user {
        UserDataState::Ready(u) => u.settings(),
        UserDataState::Unavailable(_) => Settings::default(),
    };

    let (ecdict, cedict) = dict_paths(&settings);

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
        .tooltip(format!("wind-dict — {} 查询", settings.hotkey))
        .icon_rgba(16, 16, &icon())
        .on_left_click(|ctx| ctx.show_window())
        .on_double_click(|ctx| ctx.show_window())
        .menu(vec![
            TrayMenuItem::item("查询", |ctx| ctx.show_window()),
            TrayMenuItem::separator(),
            // 唯一的退出途径：窗口关闭与 ESC 都只是收起来（hide_on_close）。
            TrayMenuItem::item("退出", |ctx| ctx.quit()),
        ]);

    // 皮肤取自设置，**运行期可换**：界面里没有一处写死颜色，全部走主题角色，故
    // `ThemeHandle::set` 之后下一帧全树跟随，不必重建元素树。ADR-0012 当初判断
    // 「接不上」，症结其实不在框架，而在我们把本可用角色表达的颜色写成了具体色值。
    let skin = settings.skin.skin();

    // 窗口 920 宽：召回改成按需抽屉之后，常驻开销只剩 44px 的 rail，主列平时拿到
    // 876px——远超设计稿正文限宽 640px，正文因此靠 `max_width` 自己收着，不再是被
    // 侧栏挤出来的宽度。抽屉展开时主列 596px，仍装得下限宽后的正文。
    let mut app = App::new("wind-dict", 920, 620)
        // 下限按**抽屉展开态**定：720 时展开还剩 396px 给主列，勉强够读；再窄就该让
        // 抽屉改为盖住主列而不是挤它（尚未实现），而不是让用户拖到一个不可用的尺寸。
        .min_size(720, 480)
        // 无系统标题栏：标题栏由 `ui::title_bar` 自绘，才能与整体配色一致。
        .frameless()
        // GPU 渲染（Direct2D）。本项目不以「后台内存尽可能小」为目标——见 ADR-0006
        // 的修订，需求已改为优先响应与渲染质量。
        //
        // 取 `Auto` 而非 `Gpu`：无 GPU、RDP 远程会话、离屏截图等情形前者自动回退软
        // 渲染，后者是**报错终止**——那条语义是给排障用的（静默回退会让人拿两张软渲染
        // 的截图得出「软硬一致」），交付给用户的构建不该带。
        .renderer(Renderer::Auto)
        .theme(skin.theme.clone())
        .tray(tray)
        // 常驻：启动不闪窗口，关闭只收起，进程始终活着等热键。见 ADR-0006。
        .start_hidden()
        .hide_on_close()
        .screenshot_from_args();

    // 两个句柄都必须在 `content` **之前**取到——界面要拿着它们才能即时换肤、改键。
    let theme = app.theme_handle();
    let hotkey = app.hotkey_handle(settings.hotkey.to_hotkey(), |ctx| ctx.show_window());

    app.content(ui::build(dict, user, theme, hotkey)).run();
}

/// 打开用户数据库：`%LOCALAPPDATA%\wind-dict-data\userdata.db`。
///
/// 位置由两条否定性约束确定——**不在部署目录内**（`dev.ps1` 卸载会
/// `Remove-Item -Recurse -Force` 整个部署目录）、**不在漫游目录内**（登录/注销的
/// 整体同步会拷走写到一半的库）。完整论证与被拒方案见 `docs/adr/0011`。
fn open_userdata() -> UserDataState {
    let path = match userdata_path() {
        Ok(p) => p,
        Err(why) => return UserDataState::Unavailable(why),
    };
    match UserData::open(&path) {
        Ok(u) => UserDataState::Ready(u),
        // `{e:#}` 保留 anyhow 的完整因果链，只取最外层会丢掉真正的原因。
        Err(e) => UserDataState::Unavailable(format!("{e:#}")),
    }
}

/// 用户数据库路径，必要时建目录。
fn userdata_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("环境变量 LOCALAPPDATA 未设置")?;
    // `wind-dict-data` 而非 `wind-dict`：后者是部署目录，卸载时会被整个删除。
    //
    // dev 与 release 分库，与 `dev.ps1` 分离两个部署目录的方式对齐：跑 dev 构建
    // 调试不该往日常使用的历史记录里塞垃圾词；更要紧的是 `SCHEMA` 日后演进（加列、
    // 迁移）时，dev 构建会就地改掉 release 正在用的那个库——现在两边 schema 相同，
    // `CREATE TABLE IF NOT EXISTS` 恰好掩盖了这个风险。
    let dir = PathBuf::from(base).join(if cfg!(debug_assertions) {
        "wind-dict-data-dev"
    } else {
        "wind-dict-data"
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{}（{e}）", dir.display()))?;
    Ok(dir.join("userdata.db"))
}

/// 词库路径。优先级：命令行参数 > 设置 > exe 同目录。
///
/// 命令行压过设置，是因为它是**当次显式指定**的——开发时用 `.cache/dict/` 里的库
/// 调试，不该被用户设置里的路径顶掉，也不该反过来把设置改坏。
///
/// exe 同目录而非工作目录：常驻工具从托盘/热键启动时，工作目录是什么完全不可控。
fn dict_paths(settings: &Settings) -> (PathBuf, PathBuf) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(pair) = positional_dicts(&args) {
        return pair;
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default();
    let ec = settings
        .ecdict
        .clone()
        .unwrap_or_else(|| dir.join("ecdict.db"));
    let ce = settings
        .cedict
        .clone()
        .unwrap_or_else(|| dir.join("cedict.db"));
    (ec, ce)
}

/// 命令行里那对词库路径。**命令行含任何开关时一律不认**，返回 `None`。
///
/// 此前是「滤掉 `--` 开头的项、剩下的头两个当词库」，而开关的**取值**长得与位置参数
/// 一模一样：`--screenshot out.png --click 28 596` 剩下的是 `out.png`、`28`、`596`，
/// 于是程序去把 `out.png` 当 SQLite 库打开，报「打开英汉词库失败：out.png」——症状与
/// 起因隔了十万八千里，是最难认的那种。
///
/// 修法不采「逐个跳过带值开关」：那要求 wind-dict 记住上游有哪些开关、各带几个值
/// （windui 的 `--click` 还可重复出现），上游每加一个开关这里就悄悄失效一次，而失效
/// 的表现又是上面那条谜语。改判「有开关就整体不认」则与上游的开关表无关。
///
/// 两种用法本就不会同时出现：位置参数是开发期临时指定词库（`wind-dict a.db b.db`），
/// 开关全都走截图与调试路径。
fn positional_dicts(args: &[String]) -> Option<(PathBuf, PathBuf)> {
    if args.iter().any(|a| a.starts_with("--")) {
        return None;
    }
    match args {
        [a, b, ..] => Some((PathBuf::from(a), PathBuf::from(b))),
        _ => None,
    }
}

/// 托盘图标：16×16 纯色 RGBA8（占位，免捆绑资源文件）。
fn icon() -> Vec<u8> {
    [0x4C, 0x8B, 0xF5, 0xFF].repeat(16 * 16)
}

#[cfg(test)]
mod tests {
    use super::positional_dicts;

    fn 参数(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 两个位置参数即一对词库() {
        let got = positional_dicts(&参数(&["ec.db", "ce.db"]));
        let (ec, ce) = got.expect("两个位置参数该认");
        assert_eq!(ec.to_str(), Some("ec.db"));
        assert_eq!(ce.to_str(), Some("ce.db"));
    }

    /// 只给一个不成对——宁可回退默认，也不拿它当英汉库、汉英库悬空。
    #[test]
    fn 单个位置参数不成对() {
        assert!(positional_dicts(&参数(&["ec.db"])).is_none());
    }

    /// 这条是本函数存在的理由：截图开关的取值此前会被当成词库路径。
    #[test]
    fn 带开关时不认位置参数() {
        assert!(positional_dicts(&参数(&["--screenshot", "out.png"])).is_none());
        assert!(
            positional_dicts(&参数(&["--screenshot", "out.png", "--click", "28", "596"])).is_none(),
            "--click 的坐标不该被当成词库路径"
        );
    }
}
