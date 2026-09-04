//! 构建期工具：CC-CEDICT 文本 → 汉英词库（SQLite）。
//!
//! ```bash
//! cargo run --release --example build_cedict -- cedict_ts.u8 cedict.db
//! ```
//!
//! ## 与英汉词库的两处结构性差异
//!
//! 1. **简体列不是 UNIQUE**。多音字使同一词头对应多条词条：`行` 既是 `[hang2]`
//!    又是 `[xing2]`。这不是脏数据，是中文的事实——故查询返回的是词条**列表**。
//!
//! 2. **没有词频数据**。CC-CEDICT 不含任何词频信号（ECDICT 有 `frq`/`bnc`），
//!    因此汉英补全无法按词频排序，只能用「短词优先」这个启发式。见 `store::cedict`。
//!
//! ## 许可
//!
//! CC-CEDICT 为 **CC BY-SA 4.0**（由词库文件头部实测确认）。本工具的产物是其衍生
//! 数据，须保持同协议并署名；本项目代码本身不受影响（MIT）。

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use wind_dict::store::cedict::{parse_line, INDEXES, SCHEMA};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(dst)) = (args.next(), args.next()) else {
        eprintln!("用法：cargo run --release --example build_cedict -- <cedict.txt> <out.db>");
        std::process::exit(2);
    };

    let _ = std::fs::remove_file(&dst);
    let mut conn = Connection::open(&dst).context("创建词库失败")?;
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;

    let text = std::fs::read_to_string(&src).context("读取词库文本失败")?;
    let tx = conn.transaction()?;

    let (mut ok, mut skipped) = (0u32, 0u32);
    {
        let mut stmt = tx.prepare(
            "INSERT INTO cedict (simplified, traditional, pinyin, defs) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for line in text.lines() {
            match parse_line(line) {
                None => {
                    // 注释与空行也走这里。跳过率异常才是信号，见 examples/cedict_stats.rs。
                    skipped += 1;
                }
                Some(p) => {
                    stmt.execute(rusqlite::params![
                        p.simplified,
                        p.traditional,
                        p.pinyin,
                        p.defs_raw
                    ])?;
                    ok += 1;
                    if ok % 50_000 == 0 {
                        print!("\r已插入 {ok} 条…");
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }
    }
    tx.commit()?;
    println!("\r插入完成：{ok} 条，跳过 {skipped} 行（含注释）");

    println!("建索引中…");
    conn.execute_batch(INDEXES)?;
    println!("VACUUM 中…");
    conn.execute_batch("VACUUM;")?;

    // 多音字实测：验证「简体非 UNIQUE」这个 schema 决定确有必要。
    let dupes: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT simplified FROM cedict GROUP BY simplified HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    println!("多词条词头：{dupes} 个（若为 0，说明 UNIQUE 约束本可加——实测说话）");
    drop(conn);

    let size = std::fs::metadata(Path::new(&dst))?.len();
    println!("\n词库：{dst}");
    println!(
        "体积：{:.1} MB（{size} 字节）",
        size as f64 / 1024.0 / 1024.0
    );
    Ok(())
}
