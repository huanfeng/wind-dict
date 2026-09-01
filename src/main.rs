//! wind-dict：常驻托盘的桌面词典。
//!
//! 设计的权威出处是 `CONTEXT.md`（术语表）与 `docs/adr/`（架构决策），改代码前先读。
//!
//! 本文件只负责组装：词库 → 离线词典 → 界面 → 常驻外壳（热键 / 托盘 / 显隐）。

// 常驻托盘应用不该带控制台窗口——**dev 构建也一样**。
//
// 此前这里是 `cfg_attr(not(debug_assertions), ...)`，只给 release 摘掉控制台，理由是
// 「debug 期保留，便于看 panic 与日志」。但 dev 构建也是拿来**用**的（`dev.ps1 pd`
// 把它部署到 `%LOCALAPPDATA%\wind-dict-dev` 常驻），于是一个托盘词典每次开机都先弹
// 一个黑窗口杵在任务栏上——这不是「开发态的小代价」，这是产品缺陷。
//
// 摘掉之后 stderr 确实无处可去了，两条替代路径都已就位：
//   - panic → `<用户数据目录>\panic.log`（见 `install_panic_log`，本就是为 release
//     无控制台的情形写的）；
//   - 启动期致命错误 → 消息框（见 `fatal`），比控制台还醒目，且用户也看得见。
#![windows_subsystem = "windows"]

use std::path::PathBuf;

use windui::prelude::*;

use wind_dict::icon;
use wind_dict::settings::Settings;
use wind_dict::source::offline::{self, OfflineDictionary};
use wind_dict::store::userdata::{UserData, UserDataState};
use wind_dict::ui;
use wind_dict::APP_TITLE;

/// 单实例标识。
///
/// dev 与 release 分开，与 `autostart::VALUE_NAME`、`userdata::data_dir()` 同一套判据：
/// 两个变体是两个程序，让 dev 构建顶掉正在用的 release 窗口没有道理。
const SINGLE_ID: &str = if cfg!(debug_assertions) {
    "wind-dict-dev"
} else {
    "wind-dict"
};

fn main() {
    // 单实例闸门放在**最前面**，先于 panic 日志与自启修复。
    //
    // 二次实例的全部使命是把 argv 递给首实例然后死掉。让它走完启动流程的话，
    // `repair_if_stale` 会顺手改写自启项、160 MB 的词库会被再打开一次——白做，而且
    // 带副作用。`App::run` 内部本来也仲裁一次，但那已经在这些事之后了。
    //
    // **离屏截图除外**：那条路不建窗口、不常驻，是验证手段而不是第二个实例。拦掉它
    // 就等于「只要有实例开着就永远截不了图」，而截图是本项目验证界面的主要手段。
    // windui 自己在 `run` 里也是先处理截图、再仲裁，这里与它一致。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !wants_screenshot(&args)
        && windui::claim_instance(SINGLE_ID) == windui::InstanceRole::Handoff
    {
        return;
    }
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

    let dict_dir = resolve_dict_dir(&settings);

    // 词库缺失是致命的：离线词典是本工具的主体，没有它整个程序没有意义。
    // 与其起一个查什么都「未收录」的空壳（那是在撒谎——术语表里「一无所获」的意思是
    // 词典确实没有这个词，不是「我没词库」），不如直接失败。
    //
    // 但**报得出是缺哪一份**：此前只说「无法打开词库」再附一段 anyhow 的因果链，
    // 用户看到的是一屏读不懂的东西，而他真正需要知道的只有「哪个文件不在」。
    let status = offline::check_dir(&dict_dir);
    if !status.usable() {
        fatal(&format!(
            "词库不可用。\n\n词库目录：{}\n\n{}\n\n构建词库：\n  cargo run --release --example build_ecdict -- ecdict.csv ecdict.db\n  cargo run --release --example build_cedict -- cedict_ts.u8 cedict.db",
            dict_dir.display(),
            status.missing().join("\n"),
        ));
    }
    let dict = match OfflineDictionary::open(&dict_dir) {
        Ok(d) => d,
        // `check_dir` 刚刚才开过同样这几份库，走到这里说明文件正在被替换。
        Err(e) => fatal(&format!(
            "词库刚才还是好的，现在打不开了——是不是正在被替换？\n\n{}\n\n{e:#}",
            dict_dir.display()
        )),
    };

    let tray = Tray::new()
        .tooltip(wind_dict::ui::tray_tip(&settings.hotkey))
        // 托盘图标与标题栏、任务栏是同一份产物（`scripts/gen-icon.ps1`）。此前这里是
        // 一块 16×16 的纯蓝方块占位——托盘里一排图标中它是唯一认不出是什么的那个。
        .icon_rgba(icon::TRAY_SIZE, icon::TRAY_SIZE, icon::TRAY_RGBA)
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
    // 启动时读一次系统偏好；之后的变化由 `on_system_theme_changed` 跟（见下）。
    let system_dark = windui::event::system_prefers_dark();
    let theme = settings.style.theme(settings.mode.is_dark(system_dark));

    // 窗口 920 宽：界面是左右两栏（`ui::dict_page`），左栏定宽 280，故释义那一栏拿到
    // 640——正文限宽已撤（见 `ui::EN_DEF_MAX_W`），一行排满正好是舒适的阅读宽度。
    let mut app = App::new(APP_TITLE, 920, 620)
        // 下限 720：扣掉左栏那 280 还剩 440 给释义，勉强够读。再窄就该让左栏改为可收起
        // 而不是继续压缩释义（尚未实现），而不是让用户拖到一个不可用的尺寸。
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
        .theme(theme.clone())
        .tray(tray)
        // 常驻：关闭只收起，进程始终活着等热键。见 ADR-0006。
        .hide_on_close()
        .screenshot_from_args()
        // 单实例。`app_id` 必须与上面 `claim_instance` 传的**完全一致**：不一致的话
        // 本进程会撞上自己已经持有的那把锁、把自己误判成二次实例，然后把 argv 转发
        // 给自己，窗口就永不出现了（windui `claim_instance` 的文档写了这一条）。
        //
        // 回调空着即可：平台层在它返回后就会显示并前置主窗口，而那正是「用户又双击了
        // 一次图标」该有的结果——本程序常驻托盘，窗口多半正藏着，只置前是不够的。
        .single_instance(SINGLE_ID, |_argv| {});

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
    let theme_handle = app.theme_handle();
    // 热键**切换**显隐，不是只管唤起。
    //
    // 「按一下出来、再按一下收回去」是常驻工具热键的通常语义（Listary、uTools 皆然），
    // 而此前只有 `show_window`：窗口已经在眼前时再按一下毫无反应，用户只能去点关闭。
    //
    // 可见性取自 `window_state()` 而非应用侧自己记的标志——隐藏可以发生在框架内部
    // （ESC 关窗、`hide_on_close` 的关闭按钮），应用收不到通知，自建标志迟早对不上。
    // 这个字段是为本项目在 windui 补的（`WindowState::visible`），一并补的还有热键
    // 派发前刷新一次快照：热键是唯一能在窗口隐藏、毫无消息往来时抵达的输入，不刷新
    // 就会读到窗口被藏起来之前的陈值。
    let hotkey = app.hotkey_handle(settings.hotkey.to_hotkey(), |ctx| {
        if window_state().visible {
            ctx.hide_window();
        } else {
            ctx.show_window();
        }
    });

    // 托盘句柄要在 `content` 之前取到，与主题、热键两个句柄同理。
    let tray_handle = app.tray_handle();
    let ui = ui::build(dict, user, theme_handle, hotkey, tray_handle, system_dark);
    app.on_shortcut(ui.shortcut)
        // 运行期跟随系统亮暗。常驻工具尤其需要：一次会话可能横跨日出日落，而进程
        // 一直不重启——只在启动时读一次的话，用户在系统里切了暗色得把词典重开。
        .on_system_theme_changed(ui.system_theme)
        .content(ui.root)
        .run();
}

/// 报告一条**启动期致命错误**后退出。
///
/// 无控制台的 GUI 程序里，`eprintln!` + `exit(1)` 对用户等同于**什么也没发生**——
/// 双击图标，窗口没出来，没有任何提示。词库缺失恰恰是最可能被真实用户撞上的那条
/// （绿色分发时漏拷 `.db`），它必须说话。
///
/// 仍保留 `eprintln!`：从终端跑 `cargo run` 时 Rust 会把标准流接到父控制台上，
/// 那行输出在开发态照样看得到，且不必等人点掉对话框。
fn fatal(msg: &str) -> ! {
    eprintln!("{APP_TITLE}：{msg}");
    #[cfg(windows)]
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        // 此刻还没有任何窗口（词库在建窗口之前打开），故属主传空。
        MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from(APP_TITLE),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1)
}

/// 把 panic 追加写到 `<用户数据目录>\panic.log`，写完仍交回默认钩子。
///
/// **没有它时用户看到的是什么**：两档构建都带 `windows_subsystem = "windows"`
/// （无控制台），release 还叠了 `panic = "abort"`，一次 panic 的全部表现就是
/// **进程凭空消失**——
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
        // 交回默认钩子：从终端跑 `cargo run` 时标准流接在父控制台上，那条输出仍是最快的
        // 反馈；双击启动时它无处可去，日志才是唯一线索——两条路都留着。
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

/// 用户数据目录。实现在库里（`store::userdata::data_dir`）——自带词典的默认目录
/// 也要落在它下面，而「这个目录在哪」写两遍就等着漂移。
fn userdata_dir() -> Result<PathBuf, String> {
    wind_dict::store::userdata::data_dir()
}

/// 词库目录。优先级：命令行参数 > 设置 > exe 同目录。
///
/// 命令行压过设置，是因为它是**当次显式指定**的——开发时用 `.cache/dict/` 里的库
/// 调试，不该被用户设置里的目录顶掉，也不该反过来把设置改坏。
///
/// **设置里的目录不可用时回退 exe 同目录**，而不是直接判死。设置存的是路径，而路径会
/// 失效（换盘、挪目录、清理下载文件夹）；一旦失效，程序在启动阶段就退出，而修改设置的
/// 入口**在程序里面**——用户进不去，也就改不回来，只剩手改 SQLite 一条路。一个能把
/// 自己锁死的设置项不该存在。设置页会把这个回退如实标出来，不是悄悄换掉。
fn resolve_dict_dir(settings: &Settings) -> PathBuf {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(dir) = positional_dict_dir(&args) {
        return dir;
    }
    match &settings.dict_dir {
        Some(d) if offline::check_dir(d).usable() => d.clone(),
        _ => offline::exe_dir(),
    }
}

/// 命令行里那个词库目录。**命令行含任何开关时一律不认**，返回 `None`。
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
/// 两种用法本就不会同时出现：位置参数是开发期临时指定词库目录
/// （`wind-dict .cache\dict`），开关全都走截图与调试路径。
fn positional_dict_dir(args: &[String]) -> Option<PathBuf> {
    if args.iter().any(|a| a.starts_with("--")) {
        return None;
    }
    args.first().map(PathBuf::from)
}

/// 本次启动是不是离屏截图。见 `main` 里那道单实例闸门。
///
/// 判据比 windui 的宽——它还要求 `--screenshot` 后面跟得上一个路径。宁可宽：判宽了
/// 只是多起一个实例（`App::run` 里那道仲裁还会再拦一次），判窄了却是截图永远拿不到，
/// 而那种失败看上去像「程序起不来」，要查很久才会想到单实例头上。
fn wants_screenshot(args: &[String]) -> bool {
    args.iter().any(|a| a == "--screenshot")
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

#[cfg(test)]
mod tests {
    use super::{append_panic_log, positional_dict_dir, wants_screenshot, wants_tray};

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
    fn 位置参数即词库目录() {
        let got = positional_dict_dir(&参数(&[r".cache\dict"]));
        assert_eq!(got.expect("该认").to_str(), Some(r".cache\dict"));
        assert!(positional_dict_dir(&参数(&[])).is_none());
    }

    /// 这条是本函数存在的理由：截图开关的取值此前会被当成词库路径。
    #[test]
    fn 带开关时不认位置参数() {
        assert!(positional_dict_dir(&参数(&["--screenshot", "out.png"])).is_none());
        assert!(
            positional_dict_dir(&参数(&["--screenshot", "out.png", "--click", "28", "596"]))
                .is_none(),
            "--click 的坐标不该被当成词库路径"
        );
    }

    /// 自启项现在会带 `--tray`（见 `autostart::TRAY_ARG`）。它是无值开关，本不该影响
    /// 位置参数，但「有开关就整体不认」这条把它也一并挡了——挡对了：开机自启的命令行
    /// 里本就没有词库路径，认与不认结果相同，而放宽规则去逐个识别开关正是那条注释
    /// 拒绝的做法。这里把它钉住，免得日后有人为了 `--tray` 去动那条规则。
    #[test]
    fn 托盘开关不被当成词库路径() {
        assert!(positional_dict_dir(&参数(&[wind_dict::autostart::TRAY_ARG])).is_none());
        assert!(
            positional_dict_dir(&参数(&[r".cache\dict", wind_dict::autostart::TRAY_ARG])).is_none(),
            "混入开关时整体不认，位置参数也不该生效"
        );
    }

    /// 离屏截图必须绕开单实例闸门。
    ///
    /// 判错的后果不对称：判宽了只是多起一个实例（`App::run` 里那道仲裁会再拦一次），
    /// 判窄了则是「只要有实例开着就永远截不了图」——而截图是本项目验证界面的主要手段，
    /// 那种失败还看不出跟单实例有关。
    #[test]
    fn 截图模式绕开单实例闸门() {
        assert!(wants_screenshot(&参数(&["--screenshot", "out.png"])));
        assert!(
            wants_screenshot(&参数(&["--screenshot", "out.png", "--size", "920", "620"])),
            "截图参数后面还挂着别的开关时同样成立"
        );
        assert!(!wants_screenshot(&参数(&[])), "手动双击不是截图");
        assert!(
            !wants_screenshot(&参数(&[wind_dict::autostart::TRAY_ARG])),
            "开机自启不是截图"
        );
        assert!(
            !wants_screenshot(&参数(&["--screenshots"])),
            "不该按前缀匹配，与 wants_tray 同理"
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
            wants_tray(&参数(&[r".cache\dict", "--tray"])),
            "开关出现在任意位置都算"
        );
    }
}
