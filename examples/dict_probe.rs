//! 拿真实词库端到端验证离线查询路径。
//!
//! 单元测试跑的是手写 fixture——它们只能证明实现与我的假设自洽，证明不了假设为真。
//! 本程序拿 77 万条真数据跑同样的路径，验证的是**假设本身**。
//!
//! ```bash
//! cargo run --release --example dict_probe -- ecdict.db
//! ```

use std::time::Instant;

use wind_dict::domain::{Entry, Lookup, Query};
use wind_dict::store::ecdict::Ecdict;

fn main() -> anyhow::Result<()> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("用法：cargo run --release --example dict_probe -- <ecdict.db>");
        std::process::exit(2);
    };

    let t = Instant::now();
    let db = Ecdict::open(std::path::Path::new(&path))?;
    println!("打开词库：{:?}\n", t.elapsed());

    // ── 查询 ──────────────────────────────────────────────
    println!("=== 查询 ===");
    for w in [
        "apple",
        "serendipity",
        "tried",
        "tries",
        "better",
        "running",
    ] {
        let q = Query::new(w).unwrap();
        let t = Instant::now();
        let r = db.lookup(&q)?;
        let dt = t.elapsed();
        match r {
            Lookup::NotFound => println!("  {w:<12} 未收录  ({dt:?})"),
            Lookup::Found {
                entries,
                via_base_form,
            } => {
                let Entry::English(e) = &entries[0] else {
                    unreachable!("英汉词库只产出英汉词条")
                };
                let zh = e.zh_definition.as_deref().unwrap_or("(无中文释义)");
                let first = zh.lines().next().unwrap_or("");
                println!(
                    "  {w:<12} → {:<10} {first}{}  ({dt:?})",
                    e.headword,
                    if via_base_form { "  [经原形]" } else { "" }
                );
            }
        }
    }

    // ── 补全 ──────────────────────────────────────────────
    println!("\n=== 补全（按词频） ===");
    for p in ["app", "ser", "wor"] {
        let t = Instant::now();
        let cands = db.complete(p, 5)?;
        let dt = t.elapsed();
        println!("  {p:<5} ({dt:?})");
        for c in &cands {
            let prev = c.preview.as_deref().unwrap_or("");
            let prev: String = prev.chars().take(28).collect();
            println!("      {:<16} {prev}", c.headword);
        }
    }

    // ── 逐键补全的真实开销 ────────────────────────────────
    // 用户打 "serendipity" 是 11 次按键。这是「实时补全」这个决定的真实账单。
    println!("\n=== 逐键补全累计（模拟输入 serendipity） ===");
    let word = "serendipity";
    let t = Instant::now();
    for i in 1..=word.len() {
        db.complete(&word[..i], 20)?;
    }
    let total = t.elapsed();
    println!(
        "  {} 次按键共 {total:?}（均 {:?}/次）",
        word.len(),
        total / word.len() as u32
    );

    // ── 最坏情况：单字母前缀 ──────────────────────────────
    // 打第一个字母时候选集最大（几万行），这是补全的性能下限。
    println!("\n=== 最坏情况（单字母前缀） ===");
    for p in ["a", "s", "e"] {
        let t = Instant::now();
        let n = db.complete(p, 20)?.len();
        println!("  {p} → {n} 条候选  ({:?})", t.elapsed());
    }

    // 供外部采样内存：进程需存活足够久才测得到峰值工作集。
    // 这模拟的正是常驻态——查完词、窗口隐藏、进程继续活着。
    if std::env::args().any(|a| a == "--hold") {
        println!("\n驻留中（供内存采样）…");
        std::thread::sleep(std::time::Duration::from_secs(6));
    }

    Ok(())
}
