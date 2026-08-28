//! 构建期工具：Unihan 数据文件 → 字形库（SQLite）。
//!
//! ```bash
//! # 先解压 https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip
//! cargo run --release --example build_unihan -- <解压目录> unihan.db
//! ```
//!
//! ## 为什么扫整个目录而不按文件名取字段
//!
//! `kRSUnicode` 现在住在 `Unihan_IRGSources.txt`，而不是名字最像的
//! `Unihan_RadicalStrokeCounts.txt`——Unicode 把它搬过一次。按文件名去取，下一次
//! 重排就会**静默少读**（文件还在，字段不在了），产出一个条目变少却完全不报错的库。
//! 扫全目录的代价只是多读几 MB 文本，换掉的是一整类无声失败。
//!
//! ## 部首字形怎么定
//!
//! 分两条路，因为没有一条对两种形都成立：
//!
//! - **基本形**查 `store::unihan::KANGXI_RADICALS`（源自 Unicode 康熙部首区）。
//!   不从数据推：「部首外笔画为 0 的字即该部首」这条规律会挑中低码位的罕见异体字，
//!   部首 102 得到 `曱` 而非 `田`，182 得到 `凬` 而非 `風`。
//! - **简化形**只能从数据推。Unicode 的「CJK 部首补充区」对这些字符没有给出 NFKC
//!   归一（实测 113 个字符里仅 1 个有），查无可查。所幸实测 32 个简化形中 30 个
//!   只有唯一候选，剩下 2 个用「基本区最小码位」也挑对了。
//!
//! 固化的康熙表会不会与上游漂移？会，所以建库前先用数据反验一遍，对不上就当场失败。
//!
//! ## 许可
//!
//! Unihan 为 **Unicode License V3**：可用、可改、可再分发、可商用，唯一义务是随
//! 数据或文档保留版权与许可声明——见本项目 `THIRD-PARTY.md`。与 ECDICT 的 MIT 同级，
//! 不引入 CC-CEDICT 那样的传染性条款。

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use wind_dict::store::unihan::{
    parse_rs, radical_char, verify_kangxi_table, RadicalStroke, SCHEMA,
};

/// 从一份 Unihan 文本目录里收集所需字段。
///
/// 返回 `字段名 → (码位 → 值)`。行格式为 `U+XXXX\t字段\t值`，`#` 开头是注释。
fn collect(
    dir: &std::path::Path,
    wanted: &[&str],
) -> Result<HashMap<String, HashMap<u32, String>>> {
    let mut out: HashMap<String, HashMap<u32, String>> = wanted
        .iter()
        .map(|k| ((*k).to_string(), HashMap::new()))
        .collect();

    let mut files = 0u32;
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("读取目录失败：{}", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "txt") {
            continue;
        }
        files += 1;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取失败：{}", path.display()))?;
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(cp), Some(key), Some(val)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Some(slot) = out.get_mut(key) else {
                continue;
            };
            let Some(cp) = cp
                .strip_prefix("U+")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
            else {
                continue;
            };
            slot.insert(cp, val.to_string());
        }
    }
    if files == 0 {
        bail!(
            "{} 里没有任何 .txt——需要解压后的 Unihan 目录",
            dir.display()
        );
    }
    Ok(out)
}

/// 把 `U+XXXX U+YYYY` 形式的变体字段解析成字符串，并剔除自指。
///
/// Unihan 允许一个字把自己列为自己的变体（既是简体也是繁体时）。原样存下来，界面
/// 就会显示「简体：行」——一句正确但无用的废话。
fn variants(field: Option<&String>, self_cp: u32) -> String {
    let Some(field) = field else {
        return String::new();
    };
    field
        .split_whitespace()
        .filter_map(|t| t.strip_prefix("U+"))
        .filter_map(|h| u32::from_str_radix(h, 16).ok())
        .filter(|cp| *cp != self_cp)
        .filter_map(char::from_u32)
        .collect()
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(dst)) = (args.next(), args.next()) else {
        eprintln!("用法：cargo run --release --example build_unihan -- <Unihan解压目录> <out.db>");
        std::process::exit(2);
    };
    let dir = std::path::Path::new(&src);

    println!("读取 Unihan 数据…");
    let data = collect(
        dir,
        &[
            "kRSUnicode",
            "kTotalStrokes",
            "kSimplifiedVariant",
            "kTraditionalVariant",
        ],
    )?;
    let rs_raw = &data["kRSUnicode"];
    let strokes_raw = &data["kTotalStrokes"];
    if rs_raw.is_empty() {
        bail!("没读到任何 kRSUnicode——目录不对，或 Unicode 又把字段挪走了");
    }
    println!(
        "  kRSUnicode {} 字，kTotalStrokes {} 字",
        rs_raw.len(),
        strokes_raw.len()
    );

    // 解析一遍，后面反验与推导都用这份。
    let parsed: HashMap<u32, RadicalStroke> = rs_raw
        .iter()
        .filter_map(|(cp, v)| parse_rs(v).map(|rs| (*cp, rs)))
        .collect();
    if parsed.len() != rs_raw.len() {
        bail!(
            "有 {} 条 kRSUnicode 解析不了——格式变了，先看 store::unihan::parse_rs",
            rs_raw.len() - parsed.len()
        );
    }

    // ── 反验固化的康熙表 ───────────────────────────────────────
    let lookup = |ch: char| parsed.get(&(ch as u32)).copied();
    let bad = verify_kangxi_table(lookup);
    if !bad.is_empty() {
        bail!(
            "康熙部首表与 Unihan 数据不一致，部首号 {bad:?}。\n\
             表在 store::unihan::KANGXI_RADICALS。不修正就建库，产出的每个部首都可能是错的。"
        );
    }
    println!("  康熙 214 部首表已通过数据反验");

    // ── 推导简化部首形 ───────────────────────────────────────
    // 部首外笔画为 0 的字，本身就是那个部首形。
    let mut zero: BTreeMap<(u8, u8), Vec<u32>> = BTreeMap::new();
    for (cp, rs) in &parsed {
        if rs.extra_strokes == 0 {
            zero.entry((rs.radical_no, rs.simplified_level))
                .or_default()
                .push(*cp);
        }
    }
    let mut simplified_form: HashMap<(u8, u8), char> = HashMap::new();
    for ((no, level), cps) in &zero {
        if *level == 0 {
            continue; // 基本形查表，不推导
        }
        // 优先取 CJK 统一表意文字基本区里码位最小的：简化部首形都是常用字，
        // 落在基本区；扩展区的候选是罕见异体。
        let pick = cps
            .iter()
            .filter(|cp| (0x4E00..=0x9FFF).contains(*cp))
            .min()
            .or_else(|| cps.iter().min());
        if let Some(c) = pick.and_then(|cp| char::from_u32(*cp)) {
            simplified_form.insert((*no, *level), c);
        }
    }
    println!("  推导出 {} 个简化部首形", simplified_form.len());

    // ── 建库 ───────────────────────────────────────────────
    let _ = std::fs::remove_file(&dst);
    let mut conn = Connection::open(&dst).context("创建字形库失败")?;
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;

    let tx = conn.transaction()?;
    let (mut ok, mut no_strokes, mut no_radical) = (0u32, 0u32, 0u32);
    {
        let mut stmt = tx.prepare(
            "INSERT INTO glyph (ch, radical, radical_no, extra_strokes, total_strokes, simplified, traditional)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (cp, rs) in &parsed {
            let Some(ch) = char::from_u32(*cp) else {
                continue;
            };
            // 总笔画缺失就跳过：只有部首而没有笔画数的记录，界面显示出来是半截的。
            let Some(total) = strokes_raw
                .get(cp)
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u8>().ok())
            else {
                no_strokes += 1;
                continue;
            };
            let Some(radical) = radical_char(
                rs.radical_no,
                (rs.simplified_level > 0)
                    .then(|| {
                        simplified_form
                            .get(&(rs.radical_no, rs.simplified_level))
                            .copied()
                    })
                    .flatten(),
            ) else {
                no_radical += 1;
                continue;
            };
            stmt.execute(rusqlite::params![
                ch.to_string(),
                radical.to_string(),
                rs.radical_no,
                rs.extra_strokes,
                total,
                variants(data["kSimplifiedVariant"].get(cp), *cp),
                variants(data["kTraditionalVariant"].get(cp), *cp),
            ])?;
            ok += 1;
            if ok % 20_000 == 0 {
                print!("\r已插入 {ok} 字…");
                let _ = std::io::stdout().flush();
            }
        }
    }
    tx.commit()?;
    conn.execute_batch("VACUUM;")?;

    let size = std::fs::metadata(&dst)?.len() as f64 / 1024.0 / 1024.0;
    println!("\r字形库建成：{ok} 字，{size:.1}MB → {dst}");
    if no_strokes > 0 {
        println!("  跳过 {no_strokes} 字（无总笔画）");
    }
    if no_radical > 0 {
        println!("  跳过 {no_radical} 字（部首形无法解析）");
    }
    Ok(())
}
