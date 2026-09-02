//! 定位清风输入法（WindInput）放方案的地方。
//!
//! 本项目与它是兄弟项目，码表反查用的就是它的方案数据（见 `source::codetable`）。这里
//! 只负责回答「那些数据在哪」，不碰数据本身。
//!
//! ## 两处目录，缺一不可
//!
//! - **安装目录**里的 `data\schemas`——随程序分发的方案（五笔）在这。
//! - **用户数据目录**里的 `schemas`——用户自己放的方案（虎码、小鹤之类）在这。
//!
//! 后者的位置由 `%LOCALAPPDATA%\<产品>\datadir.conf` 里那一行指定，**可以不在默认
//! 位置**：本机上它就被搬到了 `D:\UserData\输入法数据\WindInput`。所以「猜安装目录下的
//! 某个子目录」这条路一开始就是错的，必须去问那个文件。
//!
//! ## 按「那里有没有东西」认，不按名字认
//!
//! 卸载项的键名是产品显示名（本机上是「清风输入法」与「清风输入法 (开发版)」），它会随
//! 品牌、语言、版本变；而「这个目录下有没有 `data\schemas`」是个客观事实。故遍历卸载项、
//! 用后者判定——同时装了正式版与开发版时两个都能找到，各自成为一处来源。
//!
//! ## 找不到不是错误
//!
//! 用户可能根本没装 WindInput，或装在这套探测覆盖不到的地方。那时返回空列表，由设置页
//! 提供手动指定——本项目是独立的词典，码表反查是增益而不是前提。

use std::path::{Path, PathBuf};

/// 一处放着输入方案的目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDir {
    pub path: PathBuf,
    /// 这一处是怎么来的，给设置页显示，让用户看得懂扫的是哪儿。
    pub origin: String,
}

/// 探测机器上所有放着输入方案的目录。
///
/// 结果去重且保序：安装目录在前（内置方案），用户数据目录在后（自己放的方案）。同名
/// 方案后者覆盖前者是调用方的事，这里只负责给出位置。
pub fn schema_dirs() -> Vec<SchemaDir> {
    let mut out: Vec<SchemaDir> = Vec::new();
    let mut push = |path: PathBuf, origin: String| {
        if path.is_dir() && !out.iter().any(|d| d.path == path) {
            out.push(SchemaDir { path, origin });
        }
    };
    for dir in imp::install_dirs() {
        let schemas = dir.join("data").join("schemas");
        push(schemas, format!("清风输入法 · {}", dir.display()));
    }
    for (conf_owner, data) in data_dirs() {
        let schemas = data.join("schemas");
        push(schemas, format!("{conf_owner} · {}", data.display()));
    }
    out
}

/// 从 `%LOCALAPPDATA%\<产品>\datadir.conf` 读出用户数据目录。
///
/// 返回 `(产品目录名, 数据目录)`。那个文件里就一行路径，别无他物。
fn data_dirs() -> Vec<(String, PathBuf)> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(PathBuf::from(local)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let conf = e.path().join("datadir.conf");
        if !conf.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&conf) else {
            continue;
        };
        let p = PathBuf::from(text.trim());
        // 目录不存在就跳过：datadir.conf 可能指向一个已经被删掉或拔掉的盘。
        if p.as_os_str().is_empty() || !p.is_dir() {
            continue;
        }
        let owner = e.file_name().to_string_lossy().into_owned();
        out.push((owner, p));
    }
    out.sort();
    out
}

/// 一个目录看起来像不像方案目录。
///
/// 判据是「里头有 `*.schema.toml`」——手动指定时用它给出即时反馈，免得用户填了个路径
/// 却只在下次查询时才发现选错了。
pub fn looks_like_schema_dir(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".schema.toml"))
}

#[cfg(windows)]
mod imp {
    use super::PathBuf;
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ,
    };

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

    /// 遍历卸载项，找出 `InstallLocation` 下确实有 `data\schemas` 的那些。
    pub fn install_dirs() -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            scan_uninstall(root, &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    fn scan_uninstall(root: HKEY, out: &mut Vec<PathBuf>) {
        let path = HSTRING::from(UNINSTALL);
        let mut key = HKEY::default();
        let opened =
            unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut key) }.is_ok();
        if !opened {
            return;
        }
        // 注册表项名上限 255 个字符（MSDN），+1 留给结尾 NUL。
        let mut name = [0u16; 256];
        let mut i = 0u32;
        loop {
            let mut len = name.len() as u32;
            let r = unsafe {
                RegEnumKeyExW(
                    key,
                    i,
                    PWSTR(name.as_mut_ptr()),
                    &mut len,
                    None,
                    PWSTR::null(),
                    None,
                    None,
                )
            };
            if r.is_err() {
                break;
            }
            i += 1;
            let sub = String::from_utf16_lossy(&name[..len as usize]);
            if let Some(loc) = read_install_location(key, &sub) {
                let dir = PathBuf::from(loc.trim());
                // 判据是「那里有方案数据」，不是名字——见模块头。
                if dir.join("data").join("schemas").is_dir() {
                    out.push(dir);
                }
            }
        }
        unsafe { RegCloseKey(key).ok().ok() };
    }

    /// 读一个卸载项的 `InstallLocation`。值不存在是常态（多数卸载项都不写它）。
    fn read_install_location(parent: HKEY, sub: &str) -> Option<String> {
        let sub_h = HSTRING::from(sub);
        let mut k = HKEY::default();
        if unsafe { RegOpenKeyExW(parent, PCWSTR(sub_h.as_ptr()), 0, KEY_READ, &mut k) }.is_err() {
            return None;
        }
        let value = HSTRING::from("InstallLocation");
        // 两趟：先问长度，再按长度取。长度以**字节**计，宽字符故 /2。
        let mut size: u32 = 0;
        let r = unsafe {
            RegQueryValueExW(
                k,
                PCWSTR(value.as_ptr()),
                None,
                None,
                None,
                Some(&mut size as *mut u32),
            )
        };
        if r.is_err() || size == 0 {
            unsafe { RegCloseKey(k).ok().ok() };
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let r = unsafe {
            RegQueryValueExW(
                k,
                PCWSTR(value.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut size as *mut u32),
            )
        };
        unsafe { RegCloseKey(k).ok().ok() };
        if r.is_err() {
            return None;
        }
        // REG_SZ 可能带也可能不带结尾 NUL，两种都要收。
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.trim().is_empty()).then_some(s)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PathBuf;

    /// 本项目当前仅 Windows（ADR-0005）。别的平台上没有可探测的安装。
    pub fn install_dirs() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 有_schema_文件的目录才算方案目录() {
        let dir = std::env::temp_dir().join(format!("wd-wi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!looks_like_schema_dir(&dir), "空目录不算");
        std::fs::write(dir.join("readme.txt"), "x").unwrap();
        assert!(!looks_like_schema_dir(&dir), "有别的文件也不算");
        std::fs::write(dir.join("wubi86.schema.toml"), "x").unwrap();
        assert!(looks_like_schema_dir(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 不存在的目录也答得出来() {
        assert!(!looks_like_schema_dir(Path::new(r"Z:\没有这个盘\x")));
    }

    /// 探测本身不该 panic，装没装 WindInput 都一样。
    #[test]
    fn 探测在任何机器上都能跑完() {
        let dirs = schema_dirs();
        for d in &dirs {
            assert!(d.path.is_dir(), "给出的目录必须真实存在：{:?}", d.path);
            assert!(!d.origin.is_empty(), "每一处都要说清是哪来的");
        }
    }
}
