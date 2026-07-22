//! 临时验证：开机自启的注册表读写真的落地了吗。
//!
//! 注册表这类东西「编译通过」与「写对了」相距甚远——长度算错会截断路径、漏引号会
//! 在含空格的路径下静默失败。故必须实测：开 → 读回 → 关 → 再读回。

fn main() -> anyhow::Result<()> {
    let before = wind_dict::autostart::is_enabled()?;
    println!("初始状态：{before}");

    wind_dict::autostart::set(true)?;
    let on = wind_dict::autostart::is_enabled()?;
    println!("设为开启后读回：{on}");
    assert!(on, "开启后应读回 true");

    wind_dict::autostart::set(false)?;
    let off = wind_dict::autostart::is_enabled()?;
    println!("设为关闭后读回：{off}");
    assert!(!off, "关闭后应读回 false");

    // 复原到初始状态，别把开发机的设置改坏。
    wind_dict::autostart::set(before)?;
    println!("已复原到初始状态：{}", wind_dict::autostart::is_enabled()?);
    println!("✅ 注册表读写往返正确");
    Ok(())
}
