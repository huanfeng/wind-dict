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
    install_panic_log();
    // 自启项若还是旧格式（没有 `--tray`）或指着旧路径，就地改写。静默忽略失败：
    // 这是一次**修复**，不是用户此刻要求的动作，为它弹错不成比例；真要改自启，
    // 设置页那个开关会如实报错。
    let _ = wind_dict::autostart::repair_if_stale();
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
        // 常驻：关闭只收起，进程始终活着等热键。见 ADR-0006。
        .hide_on_close()
        .screenshot_from_args();

    // **只有开机自启才收进托盘**，手动运行照常显示窗口。
    //
    // 此前 `start_hidden()` 是无条件的，依据是 ADR-0006 的「常驻工具启动不该闪窗口」。
    // 那条只对**开机自启**成立：开机时弹一个词典窗口是打扰。但同一段代码也管住了用户
    // **双击图标**的情形，而那时他刚刚亲手表达了「我要用它」——却什么也没发生。除非
    // 他知道去按热键或翻托盘，否则这个程序看起来就是坏的。
    //
    // 判据是命令行开关（见 `autostart::TRAY_ARG`），由自启项自己带上。
    if launched_for_tray() {
        app = app.start_hidden();
    }

    // 两个句柄都必须在 `content` **之前**取到——界面要拿着它们才能即时换肤、改键。
    let theme = app.theme_handle();
    let hotkey = app.hotkey_handle(settings.hotkey.to_hotkey(), |ctx| ctx.show_window());

    app.content(ui::build(dict, user, theme, hotkey)).run();
}

/// 把 panic 追加写到 `<用户数据目录>\panic.log`，写完仍交回默认钩子。
///
/// **没有它时用户看到的是什么**：release 构建带 `windows_subsystem = "windows"`
/// （无控制台）且 `panic = "abort"`，一次 panic 的全部表现就是**进程凭空消失**——
/// 没有窗口、没有提示、没有退出码可看。而 panic 若发生在 Win32 的窗口过程里，
/// 它甚至不是「Rust 崩溃」的样子：跨 C ABI 不能展开，运行时直接 `__fastfail`，
/// 事件查看器里只留下一条 `0xc0000409`，故障模块是 exe 自身、偏移落在 panic 机制
/// 内部——既指不出出错的代码，也读不到 panic 的消息。
///
/// 落一份文件把这条信息链接回来：消息 + `file:line` 是排障真正需要的东西，而它在
/// panic 发生的那一刻本来就在手上，只是没人写下来。
///
/// 写在用户数据目录而非部署目录：后者会被 `dev.ps1` 卸载时整个删除（ADR-0011），
/// 而崩溃日志的价值恰恰在于**卸载重装之后还在**。
///
/// 一切失败都静默忽略：日志写不下去是排障能力的损失，不是产品故障，为它再 panic
/// 一次（在 panic 钩子里！）只会把现场彻底毁掉。
fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(dir) = userdata_dir() {
            let where_ = match info.location() {
                Some(l) => format!("{}:{}:{}", l.file(), l.line(), l.column()),
                None => "位置未知".to_string(),
            };
            // payload 取两种最常见的形状；其余类型的 panic 载荷本就无法转成文字。
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "（无消息）".to_string());
            append_panic_log(&dir, &where_, &msg);
        }
        // 交回默认钩子：dev 构建有控制台，那条输出仍然是最快的反馈。
        prev(info);
    }));
}

/// 往 `dir/panic.log` 追加一条记录。失败静默——理由见 `install_panic_log`。
///
/// **追加而非覆盖**：偶发崩溃的价值在于攒够几条才看得出共性，写一次盖一次等于只留
/// 最后一次。
///
/// 从钩子里抽出来是为了能测：`PanicHookInfo` 在下游造不出来，混在闭包里这段写盘逻辑
/// 就只能靠「真崩一次」来验证——而它恰恰是崩溃时唯一还在跑的代码，不能靠运气。
fn append_panic_log(dir: &std::path::Path, location: &str, msg: &str) {
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("panic.log"))
    else {
        return;
    };
    let _ = writeln!(
        f,
        "---- wind-dict panic ----\n构建: {}\n位置: {location}\n消息: {msg}\n",
        if cfg!(debug_assertions) {
            "dev"
        } else {
            "release"
        },
    );
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
    Ok(userdata_dir()?.join("userdata.db"))
}

/// 用户数据目录，必要时建出来。库与崩溃日志都落在这里。
fn userdata_dir() -> Result<PathBuf, String> {
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
    Ok(dir)
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

/// 本次启动是否该直接收进托盘。见 `autostart::TRAY_ARG`。
fn launched_for_tray() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    wants_tray(&args)
}

/// 参数里是否带托盘开关。
///
/// 与 `launched_for_tray` 分开是为了能测：直接读 `std::env::args()` 的函数在单测里
/// 没法喂参数，而「带不带这个开关」正是决定用户双击图标后有没有窗口的那一下。
fn wants_tray(args: &[String]) -> bool {
    args.iter().any(|a| a == wind_dict::autostart::TRAY_ARG)
}

/// 托盘图标：16×16 纯色 RGBA8（占位，免捆绑资源文件）。
fn icon() -> Vec<u8> {
    [0x4C, 0x8B, 0xF5, 0xFF].repeat(16 * 16)
}

#[cfg(test)]
mod tests {
    use super::{append_panic_log, positional_dicts, wants_tray};

    fn 参数(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 崩溃日志是**排查偶发崩溃的唯一线索**：release 无控制台，panic 落在窗口过程里
    /// 时连 Rust 的错误消息都到不了任何地方。若这段写盘悄悄失灵，下一次崩溃仍然什么
    /// 都留不下——而那正是它存在的全部意义。故它必须有测试。
    #[test]
    fn 崩溃日志写得下位置与消息() {
        let dir = std::env::temp_dir().join(format!("wind-dict-panic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        append_panic_log(&dir, "src/ui.rs:42:7", "候选下标越界");

        let got = std::fs::read_to_string(dir.join("panic.log")).expect("日志该已写出");
        assert!(got.contains("src/ui.rs:42:7"), "位置该在日志里：{got}");
        assert!(got.contains("候选下标越界"), "消息该在日志里：{got}");

        // 第二次崩溃不该把第一次冲掉——偶发问题要攒够几条才看得出共性。
        append_panic_log(&dir, "src/ui.rs:99:1", "第二次");
        let got = std::fs::read_to_string(dir.join("panic.log")).expect("日志该仍在");
        assert!(got.contains("候选下标越界"), "首条不该被覆盖：{got}");
        assert!(got.contains("第二次"), "次条该已追加：{got}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录不存在时**不得 panic**：这段代码是在 panic 钩子里跑的，它自己再 panic
    /// 会把现场彻底毁掉（双重 panic 直接 abort，连默认钩子的输出都没有）。
    #[test]
    fn 日志写不下去也不会再崩一次() {
        append_panic_log(
            std::path::Path::new("Z:/绝不存在的盘/也不存在的目录"),
            "src/x.rs:1:1",
            "无处可写",
        );
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

    /// 自启项现在会带 `--tray`（见 `autostart::TRAY_ARG`）。它是无值开关，本不该影响
    /// 位置参数，但「有开关就整体不认」这条把它也一并挡了——挡对了：开机自启的命令行
    /// 里本就没有词库路径，认与不认结果相同，而放宽规则去逐个识别开关正是那条注释
    /// 拒绝的做法。这里把它钉住，免得日后有人为了 `--tray` 去动那条规则。
    #[test]
    fn 托盘开关不被当成词库路径() {
        assert!(positional_dicts(&参数(&[wind_dict::autostart::TRAY_ARG])).is_none());
        assert!(
            positional_dicts(&参数(&["ec.db", "ce.db", wind_dict::autostart::TRAY_ARG])).is_none(),
            "混入开关时整体不认，位置参数也不该生效"
        );
    }

    /// `--tray` 在与不在，决定的是「收进托盘」还是「显示窗口」。这条判定错了，用户
    /// 双击图标会毫无反应（此前正是如此），或者开机时被弹一个窗口。
    #[test]
    fn 仅带托盘开关时才收进托盘() {
        assert!(!wants_tray(&参数(&[])), "手动双击（无参数）应显示窗口");
        assert!(
            wants_tray(&参数(&[wind_dict::autostart::TRAY_ARG])),
            "自启（带开关）应收进托盘"
        );
        assert!(!wants_tray(&参数(&["--traylike"])), "不该按前缀匹配");
        assert!(
            wants_tray(&参数(&["ec.db", "ce.db", "--tray"])),
            "开关出现在任意位置都算"
        );
    }
}
