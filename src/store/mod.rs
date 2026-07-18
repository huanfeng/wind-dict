//! 存储层：词库（只读）与用户数据（读写）。
//!
//! 二者是**两个独立的 SQLite 文件**，靠 `ATTACH` 跨库查询：
//!
//! - **词库**随程序分发，只读，可整体替换升级（删旧文件放新文件即可）。
//! - **用户数据**（收藏、历史记录）在 AppData，读写，必须永不丢失、可单独备份。
//!
//! 这个文件边界即是**生命周期边界**。放同一个文件里，「升级词库」就会变成危险的
//! 原地迁移或数据搬运；分开之后，升级词库碰都不碰用户数据。

pub mod cedict;
pub mod ecdict;

/// 前缀查询的半开区间上界：把末位字符 +1，得到 `[prefix, upper)`。
///
/// 用范围查询而非 `LIKE 'x%'`：范围确定性地走索引，而 `LIKE` 能否被优化成范围扫描
/// 取决于列的 collation 与编译期设置。英汉（`sw` 列，ASCII）与汉英（简体列，CJK）
/// 两处共用此实现——同一套逻辑写两遍就等着漂移。
///
/// 返回 `None` 表示无法构造上界（空串，或末位已是 Unicode 上限）。
pub(crate) fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last = chars.pop()?;
    // char::from_u32 对代理区（0xD800–0xDFFF）返回 None，故 `\u{D7FF}` 之后需跳过。
    // 实际输入是 [a-z0-9] 或 CJK，碰不到这个边界，但不能因此就 unwrap。
    let next = char::from_u32(last as u32 + 1)?;
    chars.push(next);
    Some(chars.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::prefix_upper_bound;

    #[test]
    fn 上界为末位加一() {
        assert_eq!(prefix_upper_bound("app").as_deref(), Some("apq"));
        assert_eq!(prefix_upper_bound("z").as_deref(), Some("{"));
        assert_eq!(prefix_upper_bound("").as_deref(), None);
    }

    /// 断言 `[prefix, upper)` 这个半开区间的**契约**，而非某个具体码位。
    ///
    /// 早先此处写的是 `assert_eq!(prefix_upper_bound("苹果"), Some("苹柂"))`——
    /// 一个靠心算 Unicode 邻居猜出来的字面量，而且猜错了（正解是 `苹枝`）。
    /// 那种断言只证明作者的算术，不证明函数的契约。
    fn 区间契约成立(prefix: &str, inside: &[&str], outside: &[&str]) {
        let upper = prefix_upper_bound(prefix).expect("非空前缀必有上界");
        assert!(
            prefix < upper.as_str(),
            "上界须大于前缀：{prefix} < {upper}"
        );
        for s in inside {
            assert!(
                *s >= prefix && *s < upper.as_str(),
                "{s} 以 {prefix} 开头，必须落在 [{prefix}, {upper}) 内"
            );
        }
        for s in outside {
            assert!(
                *s < prefix || *s >= upper.as_str(),
                "{s} 不以 {prefix} 开头，必须落在 [{prefix}, {upper}) 外"
            );
        }
    }

    #[test]
    fn 区间覆盖英文前缀() {
        区间契约成立("app", &["app", "apple", "appzzz"], &["apq", "ap", "banana"]);
    }

    #[test]
    fn 区间覆盖汉字前缀() {
        // 汉英补全的前缀是 CJK，同一实现必须对它成立。
        区间契约成立(
            "苹果",
            &["苹果", "苹果树", "苹果酱"],
            &["苹", "香蕉", "苹菓"],
        );
    }
}
