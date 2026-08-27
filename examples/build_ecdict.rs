//! 构建期工具：`ecdict.csv` → 自建精简英汉词库（SQLite）。
//!
//! 为何自建而不用官方的 `ecdict-sqlite-28.zip`（206.7MB 压缩 / 811.9MB 解压，实测），
//! 见 docs/adr/0010。产出的表结构**逐列对齐上游** `stardict.py`，故官方发布与任何
//! 同格式的第三方词库都可直接替换本工具的产物。
//!
//! ```bash
//! cargo run --release --example build_ecdict -- ecdict.csv ecdict.db
//! ```
//!
//! ## 本工具处理的两个 CSV 陷阱
//!
//! 1. **CSV 里没有 `sw` 列**。该列（stripped word：只留字母数字、转小写）是上游
//!    `stardict.py` 建库时算出来的，补全的性能全靠它和 `(sw, word)` 索引。
//!    自建就必须自己算。
//!
//! 2. **换行是字面的 `\n` 两个字符**，不是真换行。原样入库会让补全预览把整条释义
//!    糊成一行。此处在构建期一次性转成真换行——运行时便无需关心。

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

// schema 与 sw 的定义均由 store::ecdict 提供，构建期与运行期**共用同一份**。
//
// 各自实现一份就等着静默漂移：sw 的算法若两处分歧，建库时算的值与查询时算的前缀
// 对不上，补全查不到东西且不报错；schema 若两处分歧，测试测的就是个不发货的表结构。
use wind_dict::store::ecdict::{stripped_word, INDEXES, SCHEMA};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(dst)) = (args.next(), args.next()) else {
        eprintln!("用法：cargo run --release --example build_ecdict -- <ecdict.csv> <out.db>");
        std::process::exit(2);
    };

    // 重建而非增量：词库是可整体替换的只读资产，没有「迁移」这回事。
    let _ = std::fs::remove_file(&dst);

    let mut conn = Connection::open(&dst).context("创建词库失败")?;
    conn.execute_batch(SCHEMA)?;
    // 构建期一次性写入，无并发、失败即重来 —— 关掉同步与日志换速度。
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;

    let mut rdr = csv::Reader::from_path(&src).context("打开 CSV 失败")?;
    let tx = conn.transaction()?;

    let (mut ok, mut skipped, mut empty_sw) = (0u32, 0u32, 0u32);
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO stardict
             (word, sw, phonetic, definition, translation, pos,
              collins, oxford, tag, bnc, frq, exchange, detail, audio)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;

        for (i, rec) in rdr.records().enumerate() {
            let rec = rec.with_context(|| format!("CSV 第 {} 行解析失败", i + 2))?;
            let get = |n: usize| rec.get(n).unwrap_or("").trim();

            let word = get(0);
            if word.is_empty() {
                skipped += 1;
                continue;
            }

            // sw 自算 —— CSV 里没有这一列。
            let sw = stripped_word(word);
            if sw.is_empty() {
                // 纯符号词头（如 `'`）算不出 sw。它们无法被前缀补全命中，
                // 但仍可被精确查询，故照常入库。
                empty_sw += 1;
            }

            // 下标对应 CSV 表头：word,phonetic,definition,translation,pos,collins,
            // oxford,tag,bnc,frq,exchange,detail,audio。硬编码下标可行，是因为 ECDICT
            // 标准版与 ultimate 版的表头**逐列一致**（已实测核对），下标不会漂移。
            stmt.execute(rusqlite::params![
                word,
                sw,
                none_if_empty(get(1)),
                none_if_empty(&unescape_newlines(get(2))),
                none_if_empty(&unescape_newlines(get(3))),
                none_if_empty(get(4)),
                parse_flag(get(5)),
                parse_flag(get(6)),
                none_if_empty(get(7)),
                parse_rank(get(8)),
                parse_rank(get(9)),
                none_if_empty(get(10)),
                none_if_empty(&unescape_newlines(get(11))),
                none_if_empty(get(12)),
            ])?;
            ok += 1;

            if ok % 100_000 == 0 {
                print!("\r已插入 {ok} 条…");
                let _ = std::io::stdout().flush();
            }
        }
    }
    tx.commit()?;

    println!("\r插入完成：{ok} 条，跳过 {skipped} 条，无 sw {empty_sw} 条");

    println!("建索引中…");
    conn.execute_batch(INDEXES)?;

    // VACUUM 回收插入过程产生的碎片页。词库是只读资产，值得为体积做这一次整理。
    println!("VACUUM 中…");
    conn.execute_batch("VACUUM;")?;
    drop(conn);

    let size = std::fs::metadata(Path::new(&dst))?.len();
    println!("\n词库：{dst}");
    println!(
        "体积：{:.1} MB（{size} 字节）",
        size as f64 / 1024.0 / 1024.0
    );
    Ok(())
}

/// 字面的 `\n` → 真换行。
///
/// ECDICT 的 CSV 把释义中的换行存为反斜杠加 n 两个字符。不转换的话，
/// `str::lines()` 切不开，补全预览会把整条释义糊成一行。
fn unescape_newlines(s: &str) -> String {
    s.replace("\\n", "\n")
}

fn none_if_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// 词频排名：ECDICT 用 `0` 与空字符串表示「未知」，统一映射为 NULL。
///
/// 若把 0 原样存进去，`ORDER BY frq` 会把它排在真实高频词（排名 1、2、3…）**之前**
/// ——0 比任何排名都小。故未知必须是 NULL，并在查询侧显式后置。
fn parse_rank(s: &str) -> Option<i64> {
    match s.parse::<i64>() {
        Ok(n) if n > 0 => Some(n),
        _ => None,
    }
}

/// 柯林斯星级与牛津标记：空字符串与非法值一律作 `0`。
///
/// 存 `0` 而非 NULL 是**刻意与上游对齐**——`stardict.py` 给这两列的声明就是
/// `INTEGER DEFAULT(0)`。词库要能与官方发布互换，值域就得一致；读取侧
/// （`store::ecdict::rank_u8`）也据此把 `0` 与 NULL 一同当作「未评级」。
///
/// 这与 `parse_rank` 把 `0` 归一成 NULL 的做法相反，但两者并不矛盾：`bnc`/`frq`
/// 上游声明的正是 `DEFAULT(NULL)`，且排序需要 NULL 才能被显式后置（见 `parse_rank`）。
/// 每一列各随各的上游声明，这正是「对齐格式」的含义。
fn parse_flag(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(0).max(0)
}
