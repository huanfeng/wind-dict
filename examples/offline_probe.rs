//! 离线词典端到端验证：一个词典、两份词库、方向自动判定。
//!
//! ```bash
//! cargo run --release --example offline_probe -- ecdict.db cedict.db
//! ```

use std::time::Instant;

use wind_dict::domain::{Dictionary, Entry, Lookup, Query, Wordlist};
use wind_dict::source::offline::OfflineDictionary;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(ec), Some(ce)) = (args.next(), args.next()) else {
        eprintln!("用法：cargo run --release --example offline_probe -- <ecdict.db> <cedict.db>");
        std::process::exit(2);
    };

    let t = Instant::now();
    // 字形库对本工具无所谓：它探的是查询与补全，字形不参与。给个不存在的路径，
    // `open` 会当作没有——这正是那个「可缺」设计要保证的行为。
    let dict = OfflineDictionary::open(
        std::path::Path::new(&ec),
        std::path::Path::new(&ce),
        std::path::Path::new("unihan.db"),
    )?;
    println!("打开「{}」：{:?}\n", dict.name(), t.elapsed());

    // 方向由查询词自动判定——同一个入口，用户从不选方向。
    println!("=== 查询（方向自动判定） ===");
    for w in [
        "apple",
        "苹果",
        "serendipity",
        "行",
        "tried",
        "你好",
        "苹果酱",
    ] {
        let q = Query::new(w).unwrap();
        let t = Instant::now();
        let r = dict.lookup(&q)?;
        let dt = t.elapsed();
        let dir = format!("{:?}", q.direction());
        match r {
            Lookup::NotFound => println!("  {w:<12} [{dir:<7}] 未收录  ({dt:?})"),
            Lookup::Found { entries, .. } => {
                println!("  {w:<12} [{dir:<7}] {} 条  ({dt:?})", entries.len());
                for e in entries.iter().take(3) {
                    match e {
                        Entry::English(x) => {
                            let zh = x.zh_definition.as_deref().unwrap_or("(无中文释义)");
                            let line: String =
                                zh.lines().next().unwrap_or("").chars().take(36).collect();
                            println!(
                                "      音标[{}] {line}",
                                x.phonetic.as_deref().unwrap_or("-")
                            );
                        }
                        Entry::Chinese(x) => {
                            let g: Vec<_> = x
                                .senses
                                .iter()
                                .take(3)
                                .map(|s| s.glosses.join("; "))
                                .collect();
                            let cl = if x.classifiers.is_empty() {
                                String::new()
                            } else {
                                format!("  量词={:?}", x.classifiers)
                            };
                            println!(
                                "      拼音[{}] 繁[{}] {}{cl}",
                                x.pinyin,
                                x.traditional,
                                g.join(" / ")
                            );
                        }
                        // 本例只探随程序分发的两个库，走不到自带词典这一支。
                        Entry::User(_) => unreachable!("离线词典不产出自带词典的词条"),
                    }
                }
            }
        }
    }

    // 补全同样自动判方向，但两个方向的排序依据不同。
    println!("\n=== 补全（英汉按词频 / 汉英按词长） ===");
    for p in ["app", "苹", "ser", "行"] {
        let t = Instant::now();
        let cands = dict.complete(p, 5)?;
        println!("  {p:<5} ({:?})", t.elapsed());
        for c in &cands {
            let prev: String = c
                .preview
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(26)
                .collect();
            println!("      {:<12} {prev}", c.headword.as_str());
        }
    }

    if std::env::args().any(|a| a == "--hold") {
        println!("\n驻留中（供内存采样）…");
        std::thread::sleep(std::time::Duration::from_secs(6));
    }
    Ok(())
}
