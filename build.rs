//! 构建期：把应用图标与版本信息编进 exe 的 Win32 资源段。
//!
//! ## 为什么必须有这一步
//!
//! 图标进 exe 只有资源段这一条路——`include_bytes!` 那种把字节编进 `.rdata` 的做法
//! 只能给我们自己画（托盘就是这么走的），而**任务栏、Alt-Tab、资源管理器**的图标
//! 全部由系统去 PE 的资源表里取，它读不到 `.rdata` 里的任何东西。
//!
//! windui 建窗口类时按名字 `MAINICON` 取组图标，取不到再退到数字序号 1，两者都取不到
//! 就是 `HICON::default()`——即**空白**（见 `platform/win32/mod.rs` 的
//! `register_window_class`）。此前本项目没有 build.rs，`assets/wind-dict.ico` 生成
//! 出来就一直躺在库里没人用，任务栏于是一直是空白。
//!
//! ## 为什么 .rc 是这里生成而不是入库一份
//!
//! 版本号要跟着 `Cargo.toml` 走。入库一份手写 .rc 就等于把版本号抄成两份，而「两处
//! 各自维护的同一个事实」迟早会对不上——且对不上时没有任何报错，只是文件属性里显示
//! 一个陈旧的号码。这里从 `CARGO_PKG_VERSION` 现算，不存在第二份。
//!
//! ## 为什么写成 UTF-16LE
//!
//! rc.exe 对无 BOM 的文件按**系统 ANSI 代码页**解释，中文在简体中文机器上恰好能过
//! （GBK），换到别的 locale 就是乱码——一个「在我机器上是好的」的经典陷阱。带 BOM 的
//! UTF-16LE 是 rc.exe 唯一无歧义的输入，与 locale 无关。

fn main() {
    // 非 Windows 目标上没有资源段这回事，整段跳过（本项目目前只面向 Windows，
    // 但让 `cargo check --target ...` 不至于报一个莫名其妙的错是便宜的）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("assets").join("wind-dict.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");

    // 图标缺失就跳过而不是失败：`assets/wind-dict.ico` 由 `scripts/gen-icon.ps1` 生成
    // 且已入库，正常不会缺。但「图标没了」该让人得到一个说得清的告警，而不是让整个
    // 构建挂在 rc.exe 一句 "cannot open file" 上。
    if !ico.exists() {
        println!(
            "cargo:warning=未找到 {}，本次构建的 exe 将没有图标（跑 scripts/gen-icon.ps1 生成）",
            ico.display()
        );
        return;
    }

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("wind-dict.rc");
    std::fs::write(&out, utf16le(&rc_source(&ico))).expect("写入 .rc 失败");

    embed_resource::compile(&out, embed_resource::NONE)
        // `manifest_optional`：本项目的 .rc 里**没有**应用程序清单——DPI 感知由 windui
        // 运行期调 `SetProcessDpiAwarenessContext` 解决，塞一份清单进来反而多一处会和
        // 它打架的声明。故资源编译器缺席时只是少个图标，不该让构建失败。
        .manifest_optional()
        .expect("编译资源失败");
}

/// 生成 .rc 源码。
fn rc_source(ico: &std::path::Path) -> String {
    // `MAINICON`：不带引号的标识符在 .rc 里就是**字符串名**（数字才是序号）。这个名字
    // 不是随便取的——windui 优先按它查找，见本文件头部。
    //
    // 路径里的反斜杠要转义：.rc 的字符串字面量沿用 C 的转义规则，`C:\assets\x.ico`
    // 里的 `\a` 会被当成响铃符。
    let ico = ico.display().to_string().replace('\\', "\\\\");

    // "0.1.0" → (0, 1, 0)。Win32 的版本是四段，第四段留给构建号，我们没有，填 0。
    let mut v = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // 预发布后缀（`0.2.0-rc1`）不是数字，先切掉。
    if let Some(i) = v.find(['-', '+']) {
        v.truncate(i);
    }
    let mut it = v.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    );

    format!(
        r#"#include <winver.h>

MAINICON ICON "{ico}"

VS_VERSION_INFO VERSIONINFO
FILEVERSION    {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS         VOS_NT_WINDOWS32
FILETYPE       VFT_APP
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        /* 0804 = 简体中文，04B0 = Unicode。界面是中文的，属性页也该是。 */
        BLOCK "080404B0"
        BEGIN
            VALUE "FileDescription",  "清风词典"
            VALUE "ProductName",      "清风词典"
            VALUE "FileVersion",      "{a}.{b}.{c}.0"
            VALUE "ProductVersion",   "{a}.{b}.{c}.0"
            VALUE "OriginalFilename", "wind-dict.exe"
            VALUE "InternalName",     "wind-dict"
            VALUE "LegalCopyright",   "MIT OR Apache-2.0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        /* 必须与上面的 BLOCK 名一致，否则属性页读不出任何字段。 */
        VALUE "Translation", 0x0804, 1200
    END
END
"#
    )
}

/// UTF-8 → 带 BOM 的 UTF-16LE 字节。理由见本文件头部。
fn utf16le(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE]; // BOM
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}
