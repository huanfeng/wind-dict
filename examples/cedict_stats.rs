//! 拿真实的 CC-CEDICT 文件验证解析器，并报告统计。
//!
//! 单元测试用的是照文档编的行；真实的 12 万行里一定有想不到的形状。本程序的价值
//! 在于**跳过率**：若远高于注释行数，说明解析器（或上游格式）出了问题。
//!
//! ```bash
//! cargo run --example cedict_stats -- path/to/cedict_ts.u8
//! ```

use std::collections::BTreeMap;

use wind_dict::store::cedict::{parse_line, to_entry};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("用法：cargo run --example cedict_stats -- <cedict.txt>");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).expect("读取词库文件失败");

    let (mut comments, mut blank, mut malformed, mut no_sense) = (0u32, 0u32, 0u32, 0u32);
    let mut entries = Vec::new();
    // 收集被判为畸形的行，供人工核对——跳过率高时要能看到究竟跳了什么。
    let mut samples = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            blank += 1;
            continue;
        }
        if t.starts_with('#') {
            comments += 1;
            continue;
        }
        match parse_line(line) {
            None => {
                malformed += 1;
                if samples.len() < 5 {
                    samples.push(line.to_string());
                }
            }
            Some(p) => match to_entry(&p) {
                None => no_sense += 1,
                Some(e) => entries.push(e),
            },
        }
    }

    let total = text.lines().count();
    println!("总行数        {total}");
    println!("注释          {comments}");
    println!("空行          {blank}");
    println!("畸形（跳过）  {malformed}");
    println!("无义项（跳过）{no_sense}");
    println!("成功词条      {}", entries.len());

    if !samples.is_empty() {
        println!("\n畸形行样本：");
        for s in &samples {
            println!("  {s}");
        }
    }

    // 义项与量词分布：验证「/ 分义项、; 分措辞」确实解析出了多义项词条。
    let multi_sense = entries.iter().filter(|e| e.senses.len() > 1).count();
    let multi_gloss = entries
        .iter()
        .filter(|e| e.senses.iter().any(|s| s.glosses.len() > 1))
        .count();
    let with_cl = entries.iter().filter(|e| !e.classifiers.is_empty()).count();
    println!("\n多义项词条    {multi_sense}");
    println!("含多措辞义项  {multi_gloss}");
    println!("带量词词条    {with_cl}");

    // 繁简：多少词条繁简写法不同。
    let diff = entries
        .iter()
        .filter(|e| e.traditional != e.headword.as_str())
        .count();
    println!("繁简不同      {diff}");

    // 拼音里出现的非常规字符——用于发现解析器没料到的形状。
    let mut odd: BTreeMap<char, u32> = BTreeMap::new();
    for e in &entries {
        for c in e.pinyin.chars() {
            if !c.is_ascii_alphanumeric() && c != ' ' && c != ':' {
                *odd.entry(c).or_default() += 1;
            }
        }
    }
    if !odd.is_empty() {
        println!("\n拼音中的非常规字符（前 10）：");
        for (c, n) in odd.iter().take(10) {
            println!("  {c:?} × {n}");
        }
    }

    // 抽样几条，肉眼核对。
    println!("\n抽样：");
    for e in entries.iter().filter(|e| !e.classifiers.is_empty()).take(3) {
        println!(
            "  {} ({}) [{}] {:?} 量词={:?}",
            e.headword,
            e.traditional,
            e.pinyin,
            e.senses
                .iter()
                .map(|s| s.glosses.join("; "))
                .collect::<Vec<_>>(),
            e.classifiers
        );
    }
}
