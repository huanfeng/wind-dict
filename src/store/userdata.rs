//! 用户数据：收藏与历史记录（读写）。
//!
//! 这是存储层**读写的那一半**，与只读词库是**两个独立的 SQLite 文件**——文件边界即
//! 生命周期边界：词库随程序分发、可整体替换升级；用户数据在 `%LOCALAPPDATA%\wind-dict-data`
//! （**不在部署目录内**，否则卸载即丢，见 `main.rs` 的 `userdata_path`），
//! **必须永不丢失**、可单独备份。故本库用 SQLite 默认的持久化（有回滚日志、崩溃安全），
//! 而非词库那种 `journal_mode = OFF`。
//!
//! 「必须永不丢失」及其推出的位置约束见 `docs/adr/0011`。此前这里引 `adr/0006`
//! 是错的：那份 ADR 讲的是常驻进程靠 SQLite 按需读页控制内存，全文与用户数据的
//! 生命周期无关。
//!
//! ## 收藏与历史的语义分野
//!
//! 二者形状相近却是两张表，因为语义相反（术语表）：
//! - **收藏**是**意图**——用户主动标记「我在意这个词」。可带备注；重复收藏不改时间。
//! - **历史**是**事实**——系统被动记录「这个词被查过」。同词重复只更新时间、不新增行。
//!
//! 一个词在历史里不代表用户在意它；故不能用一张表 + 一个「收藏标志」糊弄过去。

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::domain::{Favorite, Headword, HistoryEntry};
use crate::settings::Settings;

/// 建表。两表均以 `word` 为主键——这不是随意选择：
/// - 历史的 `INSERT ... ON CONFLICT(word) DO UPDATE` 靠它实现「重复更新时间戳、不新增行」，
///   直接落实「历史属于查询词这一层」（docs/adr/0002 相关）。
/// - 英文词头是 ASCII、中文词头是 CJK，两者字符串永不碰撞，故一个 `word` 主键可安全
///   容纳两个方向的词头，无需再存方向列（方向可由词头 O(1) 判定，存它是冗余）。
///
/// 不建时间索引：用户数据规模是个人级（几百到几千条），全表扫描 + 排序是微秒级；
/// 索引的写入开销反而不值。规模真涨起来再加。
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS favorites (
    word     TEXT PRIMARY KEY,
    added_at INTEGER NOT NULL,
    note     TEXT
);
CREATE TABLE IF NOT EXISTS history (
    word         TEXT PRIMARY KEY,
    looked_up_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);";

/// 用户数据库。
pub struct UserData {
    conn: Connection,
}

/// 用户数据的可用状态。
///
/// 用具名枚举而非 `Result<UserData, String>`：`Result` 表达的是「刚刚那次操作的
/// 结果」，而这是一个**持续存在的状态**，会被界面反复读取。存成 `Result` 还会招来
/// `?` / `unwrap()` 这类肌肉记忆——而这个值永远不该被传播或解包，不可用是必须被
/// 正面处理的一种常态。
pub enum UserDataState {
    Ready(UserData),
    /// 不可用，并附**原因**：界面要说得出为什么，只说「不可用」等于把排查甩给用户。
    Unavailable(String),
}

impl UserData {
    /// 打开（或创建）用户数据库，建表。读写。
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("打开用户数据库失败：{}", path.display()))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    // ── 收藏（意图）─────────────────────────────────────────

    /// 收藏一个词头。已收藏则为**空操作**——保留原收藏时间，不因再次点击而刷新。
    /// （若要「取消再收藏」刷新时间，先 `remove_favorite` 再 `add_favorite`。）
    pub fn add_favorite(&self, hw: &Headword, at: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO favorites (word, added_at) VALUES (?1, ?2)",
                rusqlite::params![hw.as_str(), at],
            )
            .context("收藏失败")?;
        Ok(())
    }

    pub fn remove_favorite(&self, hw: &Headword) -> Result<()> {
        self.conn
            .execute("DELETE FROM favorites WHERE word = ?1", [hw.as_str()])
            .context("取消收藏失败")?;
        Ok(())
    }

    pub fn is_favorite(&self, hw: &Headword) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM favorites WHERE word = ?1",
            [hw.as_str()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 设置或清除备注（`None` = 清除）。词头不在收藏中则为空操作。
    pub fn set_note(&self, hw: &Headword, note: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE favorites SET note = ?2 WHERE word = ?1",
                rusqlite::params![hw.as_str(), note],
            )
            .context("设置备注失败")?;
        Ok(())
    }

    /// 全部收藏，**按收藏时间倒序**（最近收藏的在前）。
    pub fn favorites(&self) -> Result<Vec<Favorite>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT word, added_at, note FROM favorites ORDER BY added_at DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Favorite {
                headword: Headword::from_store(row.get::<_, String>(0)?),
                added_at: row.get(1)?,
                note: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("读取收藏失败")
    }

    // ── 历史（事实）─────────────────────────────────────────

    /// 记录一次查询。同一词头**只更新时间、不新增行**——历史按词头去重，
    /// 否则高频词会把历史刷屏。
    pub fn record(&self, hw: &Headword, at: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO history (word, looked_up_at) VALUES (?1, ?2)
                 ON CONFLICT(word) DO UPDATE SET looked_up_at = excluded.looked_up_at",
                rusqlite::params![hw.as_str(), at],
            )
            .context("记录历史失败")?;
        Ok(())
    }

    /// 最近的历史，**按查询时间倒序**，最多 `limit` 条。
    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT word, looked_up_at FROM history ORDER BY looked_up_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(HistoryEntry {
                headword: Headword::from_store(row.get::<_, String>(0)?),
                looked_up_at: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("读取历史失败")
    }

    pub fn remove_history(&self, hw: &Headword) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE word = ?1", [hw.as_str()])
            .context("删除历史失败")?;
        Ok(())
    }

    pub fn clear_history(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM history", [])
            .context("清空历史失败")?;
        Ok(())
    }

    /// 读出全部设置。
    ///
    /// 读取失败按「没有这一项」处理，由 `Settings::from_pairs` 退回默认——设置读不到
    /// 不该让程序起不来，与词库缺失时宁可退出是不同刻度的取舍（见 `settings` 模块）。
    pub fn settings(&self) -> Settings {
        let map = self.settings_map().unwrap_or_default();
        Settings::from_pairs(|k| map.get(k).cloned())
    }

    fn settings_map(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (k, v) = r?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// 整份写回。
    ///
    /// **失败必须告知调用方**（返回 `Result`，不吞）：改设置是用户主动表达的意图，
    /// 与历史记录那种被动事实不同——静默失败会让用户以为改好了，下次启动才发现没变。
    /// 这与 `favorites` 的失败处理是同一条原则。
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare_cached("INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")?;
        for (k, v) in s.to_pairs() {
            stmt.execute(rusqlite::params![k, v])
                .with_context(|| format!("保存设置项 {k} 失败"))?;
        }
        Ok(())
    }

    /// 收藏时间查询（内部/测试用）：不在收藏中返回 `None`。
    #[cfg(test)]
    fn favorite_added_at(&self, hw: &Headword) -> Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT added_at FROM favorites WHERE word = ?1",
                [hw.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }
}

/// 用户数据目录，必要时建出来。数据库、崩溃日志、以及自带词典的默认存放处都在这里。
///
/// 位置由两条否定性约束确定——**不在部署目录内**（`dev.ps1` 卸载会
/// `Remove-Item -Recurse -Force` 整个部署目录）、**不在漫游目录内**（登录/注销的
/// 整体同步会拷走写到一半的库）。完整论证与被拒方案见 `docs/adr/0011`。
///
/// 放在库里而不是 `main.rs`：自带词典的默认目录挂在它下面
/// （`source::user::default_dir`），而「用户数据在哪」这件事写两遍就等着漂移——
/// 漂移的后果是卸载一次把用户的词库删了。
pub fn data_dir() -> Result<std::path::PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("环境变量 LOCALAPPDATA 未设置")?;
    // `wind-dict-data` 而非 `wind-dict`：后者是部署目录，卸载时会被整个删除。
    //
    // dev 与 release 分库，与 `dev.ps1` 分离两个部署目录的方式对齐：跑 dev 构建
    // 调试不该往日常使用的历史记录里塞垃圾词；更要紧的是 `SCHEMA` 日后演进（加列、
    // 迁移）时，dev 构建会就地改掉 release 正在用的那个库——现在两边 schema 相同，
    // `CREATE TABLE IF NOT EXISTS` 恰好掩盖了这个风险。
    let dir = std::path::PathBuf::from(base).join(if cfg!(debug_assertions) {
        "wind-dict-data-dev"
    } else {
        "wind-dict-data"
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{}（{e}）", dir.display()))?;
    Ok(dir)
}

/// 当前 Unix 纪元秒。生产代码取时间的唯一入口——存储层的方法都收时间参数以便测试确定，
/// 由调用方在这里取「现在」。
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(s: &str) -> Headword {
        Headword::from_store(s)
    }

    // ── 收藏 ──────────────────────────────────────────────

    #[test]
    fn 收藏与取消() {
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        assert!(!db.is_favorite(&apple).unwrap());
        db.add_favorite(&apple, 1000).unwrap();
        assert!(db.is_favorite(&apple).unwrap());
        db.remove_favorite(&apple).unwrap();
        assert!(!db.is_favorite(&apple).unwrap());
    }

    #[test]
    fn 重复收藏不改时间() {
        // 已收藏则空操作——再次点击「收藏」不该把时间刷成现在。
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        db.add_favorite(&apple, 1000).unwrap();
        db.add_favorite(&apple, 9999).unwrap();
        assert_eq!(db.favorite_added_at(&apple).unwrap(), Some(1000));
        assert_eq!(db.favorites().unwrap().len(), 1, "不该有重复行");
    }

    #[test]
    fn 收藏按时间倒序() {
        let db = UserData::in_memory().unwrap();
        db.add_favorite(&hw("apple"), 1000).unwrap();
        db.add_favorite(&hw("banana"), 3000).unwrap();
        db.add_favorite(&hw("cherry"), 2000).unwrap();
        let words: Vec<_> = db
            .favorites()
            .unwrap()
            .iter()
            .map(|f| f.headword.as_str().to_string())
            .collect();
        assert_eq!(words, vec!["banana", "cherry", "apple"], "最近收藏在前");
    }

    #[test]
    fn 备注设置与清除() {
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        db.add_favorite(&apple, 1000).unwrap();
        assert_eq!(db.favorites().unwrap()[0].note, None);
        db.set_note(&apple, Some("常忘")).unwrap();
        assert_eq!(db.favorites().unwrap()[0].note.as_deref(), Some("常忘"));
        db.set_note(&apple, None).unwrap();
        assert_eq!(db.favorites().unwrap()[0].note, None);
    }

    #[test]
    fn 取消再收藏刷新时间() {
        // 与「重复收藏」相对：显式 remove 后再 add，时间应更新。
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        db.add_favorite(&apple, 1000).unwrap();
        db.remove_favorite(&apple).unwrap();
        db.add_favorite(&apple, 5000).unwrap();
        assert_eq!(db.favorite_added_at(&apple).unwrap(), Some(5000));
    }

    // ── 历史 ──────────────────────────────────────────────

    #[test]
    fn 记录与读取历史() {
        let db = UserData::in_memory().unwrap();
        db.record(&hw("apple"), 1000).unwrap();
        let h = db.history(10).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].headword.as_str(), "apple");
        assert_eq!(h[0].looked_up_at, 1000);
    }

    #[test]
    fn 同词重复查询只更新时间不新增() {
        // 历史按词头去重, 否则高频词会刷屏。
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        db.record(&apple, 1000).unwrap();
        db.record(&apple, 2000).unwrap();
        db.record(&apple, 3000).unwrap();
        let h = db.history(10).unwrap();
        assert_eq!(h.len(), 1, "同一词只占一行");
        assert_eq!(h[0].looked_up_at, 3000, "时间更新为最近一次");
    }

    #[test]
    fn 历史按时间倒序且尊重上限() {
        let db = UserData::in_memory().unwrap();
        db.record(&hw("a"), 1000).unwrap();
        db.record(&hw("b"), 3000).unwrap();
        db.record(&hw("c"), 2000).unwrap();
        let h = db.history(2).unwrap();
        assert_eq!(h.len(), 2, "尊重 limit");
        let words: Vec<_> = h.iter().map(|e| e.headword.as_str()).collect();
        assert_eq!(words, vec!["b", "c"], "最近查询在前");
    }

    #[test]
    fn 删除与清空历史() {
        let db = UserData::in_memory().unwrap();
        db.record(&hw("a"), 1000).unwrap();
        db.record(&hw("b"), 2000).unwrap();
        db.remove_history(&hw("a")).unwrap();
        assert_eq!(db.history(10).unwrap().len(), 1);
        db.clear_history().unwrap();
        assert!(db.history(10).unwrap().is_empty());
    }

    // ── 收藏与历史相互独立 ─────────────────────────────────

    // ── 持久化 ────────────────────────────────────────────

    /// 设置须跨重开存活，且**改一项不影响其余项**——`ON CONFLICT DO UPDATE` 若写错成
    /// `INSERT OR REPLACE` 到整表，或键名拼错，都会在这里暴露。
    #[test]
    fn 设置跨重开持久化且可覆盖() {
        use crate::settings::{HotkeySpec, Settings};
        use crate::skin::{SkinMode, SkinStyle};
        let path = std::env::temp_dir().join(format!("wind_dict_set_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut s = Settings {
            hotkey: "Ctrl+Shift+K".parse().unwrap(),
            autostart: true,
            style: SkinStyle::Focus,
            mode: SkinMode::Dark,
            expand_en: true,
            dict_dir: Some(std::path::PathBuf::from(r"D:\词库")),
            user_dict_dir: Some(std::path::PathBuf::from(r"D:\我的词库")),
            disabled_dicts: vec!["Oxford.mdx".into()],
            dict_names: vec![("cedict".into(), "汉英".into())],
            codetables: false,
            codetable_dirs: vec![std::path::PathBuf::from(r"D:\方案")],
            left_pane_w: 340,
        };
        {
            let db = UserData::open(&path).unwrap();
            db.save_settings(&s).unwrap();
        }
        {
            let db = UserData::open(&path).unwrap();
            assert_eq!(db.settings(), s, "设置须跨重开原样存活");
            // 只改一项后重写，其余项不得被抹掉。
            s.style = SkinStyle::Paper;
            db.save_settings(&s).unwrap();
            let back = db.settings();
            assert_eq!(back.style, SkinStyle::Paper);
            assert_eq!(back.hotkey, "Ctrl+Shift+K".parse::<HotkeySpec>().unwrap());
            assert!(back.autostart, "未改动的项不该被覆盖");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 全新的库读出默认设置，而不是报错。
    #[test]
    fn 新库读出默认设置() {
        let db = UserData::in_memory().unwrap();
        assert_eq!(db.settings(), crate::settings::Settings::default());
    }

    #[test]
    fn 数据跨重开持久化() {
        // 本模块的核心承诺「用户数据必须永不丢失」——内存库证明不了它。
        // 本测试写入、关闭连接、重开、验证存活；能抓「误设 journal_mode=OFF」
        // 或「重开时没建表/没读到」这类会静默丢数据的 bug。
        // 文件名带 pid，避免并发测试进程撞名。
        let path = std::env::temp_dir().join(format!("wind_dict_ud_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);

        {
            let db = UserData::open(&path).unwrap();
            db.add_favorite(&hw("apple"), 1000).unwrap();
            db.set_note(&hw("apple"), Some("测试")).unwrap();
            db.record(&hw("banana"), 2000).unwrap();
        } // 连接在此 drop

        {
            let db = UserData::open(&path).unwrap(); // 重开同一文件
            assert!(db.is_favorite(&hw("apple")).unwrap(), "收藏须跨重开存活");
            assert_eq!(db.favorites().unwrap()[0].note.as_deref(), Some("测试"));
            let h = db.history(10).unwrap();
            assert_eq!(h.len(), 1);
            assert_eq!(h[0].headword.as_str(), "banana");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn 收藏不影响历史历史不影响收藏() {
        // 语义分野的具体体现: 收藏是意图, 历史是事实, 互不牵连。
        let db = UserData::in_memory().unwrap();
        let apple = hw("apple");
        db.add_favorite(&apple, 1000).unwrap();
        assert!(db.history(10).unwrap().is_empty(), "收藏不产生历史");

        let banana = hw("banana");
        db.record(&banana, 2000).unwrap();
        assert!(!db.is_favorite(&banana).unwrap(), "查询不产生收藏");
    }
}
