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

#[cfg(windows)]
mod imp {
    use super::VALUE_NAME;
    use anyhow::{bail, Context, Result};
    use windows::core::{HSTRING, PCWSTR};
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

    /// 当前 exe 路径，带引号。
    ///
    /// **必须带引号**：路径含空格时（`C:\Program Files\...`）不加引号会被解析成
    /// 「运行 C:\Program，参数 Files\...」，开机时静默失败，而用户只会看到「自启没生效」。
    fn quoted_exe() -> Result<String> {
        let exe = std::env::current_exe().context("取当前程序路径失败")?;
        Ok(format!("\"{}\"", exe.display()))
    }

    pub fn is_enabled() -> Result<bool> {
        let key = open_run(false)?;
        let name = HSTRING::from(VALUE_NAME);
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
        unsafe { RegCloseKey(key).ok().ok() };
        // 值不存在是正常状态（未开启自启），不是错误。
        Ok(r.is_ok())
    }

    pub fn set(on: bool) -> Result<()> {
        let key = open_run(true)?;
        let name = HSTRING::from(VALUE_NAME);
        let r = if on {
            let value = HSTRING::from(quoted_exe()?);
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
        // 关闭自启时值本就不存在，视作成功——用户要的结果已经达成。
        if !on && r.is_err() {
            return Ok(());
        }
        if r.is_err() {
            bail!("写注册表失败（{:?}）", r);
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
