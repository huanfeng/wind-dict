//! 拿一本真实 MDX 走完整条路：直读 → 样式表展开 → 转成文本块。
//!
//! ```text
//! cargo run --release --example mdx_probe -- <词典.mdx> [词...]
//! ```
//!
//! 断言证明性质，这个证明观感——顺带把开库与查词的真实代价打出来，那是「运行时直读」
//! 这个决定唯一站得住的依据。

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("用法：mdx_probe <词典.mdx> [词...]");
        std::process::exit(2);
    };
    let words: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let words = if words.is_empty() {
        vec!["abandon".into()]
    } else {
        words
    };

    let size = std::fs::metadata(&path)?.len();
    let t0 = std::time::Instant::now();
    let d = wind_dict::store::mdx::Mdx::open(&path)?;
    let open_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let m = d.meta();
    println!(
        "{}  ——  {:.1} MB · 引擎 v{} · {} 词条",
        if m.title.is_empty() { "(无标题)" } else { &m.title },
        size as f64 / 1_048_576.0,
        m.version,
        m.entry_count
    );
    println!("开库耗时 {open_ms:.1} ms");

    for w in &words {
        let t0 = std::time::Instant::now();
        let hits = d.lookup(w)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!("\n══ {w}  ——  {} 条，{ms:.2} ms", hits.len());
        if hits.is_empty() {
            println!("   未收录");
        }
        for (head, html) in hits {
            println!("── {head}   （正文 {} 字节）", html.len());
            for b in wind_dict::html::to_blocks(&html) {
                let mut line = "    ".repeat(b.indent as usize);
                for r in &b.runs {
                    let t = if r.italic {
                        format!("/{}/", r.text)
                    } else if r.bold {
                        format!("*{}*", r.text)
                    } else {
                        r.text.clone()
                    };
                    line.push_str(&t);
                    if let Some(l) = &r.link {
                        line.push_str(&format!("[→{l}]"));
                    }
                }
                println!("│ {}", line.trim_end());
            }
        }
        let done = d.prefix(w, 8)?;
        if !done.is_empty() {
            println!("   补全：{}", done.join(" · "));
        }
    }
    Ok(())
}
