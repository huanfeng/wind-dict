//! 开机自启：读写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`。
//!
//! ## 为什么是 HKCU 而不是 HKLM
//!
//! `HKLM` 的 Run 键对**所有用户**生效，且写它需要管理员权限。一个词典工具没有理由
//! 索要提权，也没有理由替别的用户做决定。`HKCU` 只影响当前用户、无需提权。
//!
//! ## 为什么不用「启动」文件夹
//!
//! 往 `%APPDATA%\...\Startup` 放快捷方式是另一条常见路径，但快捷方式是 `.lnk`，
//! 得走 COM（`IShellLink`）才能正确生成——比读写一个注册表字符串复杂得多，且用户
//! 手动删掉快捷方式后我们无从得知状态。注册表项读回来就是真相。
//!
//! ## 非 Windows 平台
//!
//! 本项目当前仅 Windows（ADR-0005）。其余平台给出「不支持」而非假装成功——
//! 静默返回 `Ok` 会让设置界面显示一个永远打不开的开关。

/// 注册表值名。改名会导致旧的自启项变成孤儿（用户卸载后仍留在注册表里），故此值
/// **一经发布不得更改**。
const VALUE_NAME: &str = "wind-dict";

/// 开机自启时附加的命令行开关：**带它才收进托盘，不带就正常显示窗口**。
///
/// 起因是「双击图标什么也不发生」。此前 `main` 无条件 `start_hidden()`，因为常驻工具
/// 开机时不该弹窗——但那个理由只对**开机自启**成立。用户手动双击 exe 时同样不弹窗，
/// 表现就是「点了没反应」，除非他知道要去按热键或找托盘图标。
///
/// 判据放在命令行而不是别处，是因为只有它能区分这两种启动：自启项由本程序自己写入
/// （`set`），我们完全掌握它带什么参数；用户手动运行则不会带。环境变量、父进程名
/// 之类都是间接推断，且用户手动跑一次带参数的命令时会失灵。
pub const TRAY_ARG: &str = "--tray";

#[cfg(windows)]
mod imp {
    use super::{TRAY_ARG, VALUE_NAME};
    use anyhow::{bail, Context, Result};
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// 打开 Run 键。`write` 决定申请读写还是只读权限——只读失败通常意味着注册表
    /// 被策略锁定，此时应如实报错而非假装成功。
    fn open_run(write: bool) -> Result<HKEY> {
        let mut key = HKEY::default();
        let access = if write {
            KEY_WRITE | KEY_READ
        } else {
            KEY_READ
        };
        let path = HSTRING::from(RUN_KEY);
        unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                0,
                access,
                &mut key,
            )
            .ok()
            .context("打开注册表 Run 键失败")?;
        }
        Ok(key)
    }

    /// 自启项该写入的完整命令行：`"<exe 路径>" --tray`。
    ///
    /// **路径必须带引号**：含空格时（`C:\Program Files\...`）不加引号会被解析成
    /// 「运行 C:\Program，参数 Files\...」，开机时静默失败，而用户只会看到「自启没生效」。
    ///
    /// 末尾的 `--tray` 见 [`super::TRAY_ARG`]：开机自启要收进托盘，手动运行要显示窗口。
    pub fn command() -> Result<String> {
        let exe = std::env::current_exe().context("取当前程序路径失败")?;
        Ok(format!("\"{}\" {}", exe.display(), TRAY_ARG))
    }

    /// 读回 Run 键里我们那一项的值。不存在返回 `None`（正常状态，不是错误）。
    fn read_value() -> Result<Option<String>> {
        let key = open_run(false)?;
        let name = HSTRING::from(VALUE_NAME);
        // 两趟：先问长度，再按长度取。REG_SZ 的长度以**字节**计，宽字符故 /2。
        let mut size: u32 = 0;
        let r = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut size as *mut u32),
            )
        };
        if r.is_err() {
            unsafe { RegCloseKey(key).ok().ok() };
            return Ok(None);
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let r = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut size as *mut u32),
            )
        };
        unsafe { RegCloseKey(key).ok().ok() };
        if r.is_err() {
            return Ok(None);
        }
        // REG_SZ 存的字符串**可能**带结尾 NUL，也可能不带（写入方决定）。两种都要收。
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(Some(String::from_utf16_lossy(&buf[..end])))
    }

    /// 自启已开启、但记的命令行不是现在该写的那条时，就地改写。返回是否改写过。
    ///
    /// 两种情况会走到这里，都是真实的：
    /// 1. **旧版本写的值没有 `--tray`**。不修的话，老用户升级后开机会突然弹出窗口——
    ///    而他当初打开这个开关要的正是「安静地待在托盘里」。
    /// 2. **程序被挪了地方**。`dev.ps1` 部署到别处、或用户手动搬目录之后，注册表里
    ///    还指着旧路径，开机自启静默失效。这个缺陷此前一直在，只是没人发现。
    ///
    /// **只在已开启时修**：关着的时候什么都不做，绝不因为「路径对不上」就替用户打开
    /// 自启——那是在替他做决定。
    pub fn repair_if_stale() -> Result<bool> {
        let Some(current) = read_value()? else {
            return Ok(false);
        };
        let want = command()?;
        if current == want {
            return Ok(false);
        }
        set(true)?;
        Ok(true)
    }

    pub fn is_enabled() -> Result<bool> {
        // 值不存在是正常状态（未开启自启），不是错误——`read_value` 已按这条返回 `None`。
        Ok(read_value()?.is_some())
    }

    pub fn set(on: bool) -> Result<()> {
        // 先取命令行再开键：`command()` 的 `?` 若在开键之后早退，句柄就漏了。
        let value = if on {
            Some(HSTRING::from(command()?))
        } else {
            None
        };
        let key = open_run(true)?;
        let name = HSTRING::from(VALUE_NAME);
        let r = if let Some(value) = value {
            // 长度含结尾的 NUL，且以**字节**计——REG_SZ 是宽字符，故 ×2。
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    value.as_ptr() as *const u8,
                    (value.len() + 1) * std::mem::size_of::<u16>(),
                )
            };
            unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes)) }
        } else {
            unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }
        };
        unsafe { RegCloseKey(key).ok().ok() };
        // 关闭时「值本就不存在」视作成功——用户要的结果已经达成。但**只放行这一种**：
        // 若是被组策略拒绝（ACCESS_DENIED），那是真失败，吞掉它就成了静默降级。
        if !on && r == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if r.is_err() {
            // 用系统消息而非裸错误码：用户看到「拒绝访问」比看到 `WIN32_ERROR(5)` 有用。
            bail!("写注册表失败：{}", r.to_hresult().message());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub fn is_enabled() -> Result<bool> {
        Ok(false)
    }

    pub fn set(_on: bool) -> Result<()> {
        bail!("当前平台不支持开机自启")
    }

    /// 没有自启项可修——非 Windows 上本模块整体不支持自启（见模块头）。
    pub fn repair_if_stale() -> Result<bool> {
        Ok(false)
    }
}

/// 当前是否已设置开机自启。
pub fn is_enabled() -> anyhow::Result<bool> {
    imp::is_enabled()
}

/// 开启/关闭开机自启。
///
/// **失败必须上报**：这是用户主动表达的意图，静默失败会让开关看着开了、下次开机
/// 才发现没生效。与收藏写入失败的处理是同一条原则。
pub fn set(on: bool) -> anyhow::Result<()> {
    imp::set(on)
}

/// 自启已开启但记的命令行过时（缺 [`TRAY_ARG`]、或程序已挪位）时就地改写。
///
/// 返回是否改写过。启动时调一次即可，见 `main`。
pub fn repair_if_stale() -> anyhow::Result<bool> {
    imp::repair_if_stale()
}
