//! 测试样本在 `tests/fixtures/mdx/`，由 `writemdict` 生成，覆盖四条分叉：引擎版本
//! （2.0 的 8 字节字段 vs 1.2 的 4 字节）、键索引加扰、压缩类型、正文编码。
//!
//! 样本里的词头是挑过的，不是随手写的：`AA` 与 `A. & A.` 归一化后同为 `aa`，
//! `Full-House` 与 `fullhouse` 同为 `fullhouse`。这两组撑起「查一个词要返回一组结果」
//! 这条契约——用普通词头是测不出来的。

use super::*;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mdx")
        .join(name)
}

fn open(name: &str) -> Mdx {
    Mdx::open(&fixture(name)).unwrap_or_else(|e| panic!("{name} 打不开：{e:#}"))
}

/// 查一个词，只取词头，排序后便于断言。
fn heads(d: &Mdx, q: &str) -> Vec<String> {
    let mut v: Vec<String> = d.lookup(q).unwrap().into_iter().map(|(k, _)| k).collect();
    v.sort();
    v
}

// ── 四条格式分叉 ──────────────────────────────────────────

#[test]
fn 二点零版读得出词条() {
    let d = open("v2.mdx");
    assert_eq!(d.meta().version, 2.0);
    assert_eq!(d.meta().entry_count, 5);
    let (_, body) = &d.lookup("zebra").unwrap()[0];
    assert!(body.contains("striped animal"), "正文不对：{body}");
}

/// 1.2 版的每个长度字段都是 4 字节而非 8，且词头之后没有终止符。任一处错位都会让
/// 整份索引变成乱码——这个用例是那条分支唯一的守卫。
#[test]
fn 一点二版的四字节字段() {
    let d = open("v1.mdx");
    assert_eq!(d.meta().version, 1.2);
    assert_eq!(heads(&d, "zebra"), ["zebra"]);
}

/// 加扰用 ripemd128 从文件里的 4 个字节派生密钥。解错了不会报错，只会让索引解压失败
/// 或（更糟）解出看似合理的垃圾，故必须有真实样本反验。
#[test]
fn 加扰的键索引能解开() {
    let d = open("v2-encrypted.mdx");
    assert_eq!(heads(&d, "zebra"), ["zebra"]);
    assert_eq!(heads(&d, "aa"), ["A. & A.", "AA"]);
}

#[test]
fn 未压缩的块也读得动() {
    let d = open("v2-uncompressed.mdx");
    assert_eq!(heads(&d, "zebra"), ["zebra"]);
}

#[cfg(windows)]
#[test]
fn gbk正文解得回中文() {
    let d = open("v2-gbk.mdx");
    let (head, body) = &d.lookup("苹果").unwrap()[0];
    assert_eq!(head, "苹果");
    assert!(body.contains("一种水果"), "正文不对：{body}");
}

// ── 键的归一化 ────────────────────────────────────────────

/// 归一化后相等的词头必须**一并**返回。少返回不是「精确」，是丢词条。
#[test]
fn 重名的词头一并返回() {
    let d = open("v2.mdx");
    assert_eq!(heads(&d, "aa"), ["A. & A.", "AA"]);
    assert_eq!(heads(&d, "fullhouse"), ["Full-House", "fullhouse"]);
}

/// 大小写与标点在比较时都不参与。这不是宽容，是复刻词典自身的排序规则——
/// 用别的规则去二分，落点就是错的。
#[test]
fn 大小写与标点不参与比较() {
    let d = open("v2.mdx");
    for q in ["Full-House", "full house", "FULLHOUSE", "f.u.l.l.h.o.u.s.e"] {
        assert_eq!(heads(&d, q), ["Full-House", "fullhouse"], "查 {q} 时不对");
    }
    for q in ["AA", "aa", "a.&.a."] {
        assert_eq!(heads(&d, q), ["A. & A.", "AA"], "查 {q} 时不对");
    }
}

#[test]
fn 查不到时返回空而不是报错() {
    let d = open("v2.mdx");
    assert!(d.lookup("这个词不存在").unwrap().is_empty());
    assert!(d.lookup("").unwrap().is_empty());
    assert!(
        d.lookup("   ").unwrap().is_empty(),
        "全是被剥掉的字符，等同空查询"
    );
}

#[test]
fn 前缀补全按归一化后的形式匹配() {
    let d = open("v2.mdx");
    let mut got = d.prefix("full", 10).unwrap();
    got.sort();
    assert_eq!(got, ["Full-House", "fullhouse"]);

    // 前缀本身带标点，同样要能匹配。
    let mut got = d.prefix("Full-", 10).unwrap();
    got.sort();
    assert_eq!(got, ["Full-House", "fullhouse"]);

    assert_eq!(d.prefix("full", 1).unwrap().len(), 1, "limit 要生效");
    assert!(d.prefix("qqq", 10).unwrap().is_empty());
}

// ── 归一化规则本身 ────────────────────────────────────────

#[test]
fn 剥离的正好是ascii标点与空白() {
    for c in [' ', '\t', '-', '.', '&', '/', '\'', '(', '_', '+'] {
        assert!(is_stripped(c), "{c:?} 应被剥离");
    }
    // 重音字母与汉字必须留下：把它们剥掉会让键序整体错乱（实测 6,795 处逆序）。
    for c in ['a', '0', 'ī', 'é', '苹', '　'] {
        assert!(!is_stripped(c), "{c:?} 不该被剥离");
    }
}

// ── 头部解析 ──────────────────────────────────────────────

#[test]
fn 属性按名取值且解转义() {
    let a =
        attributes(r#"<Dictionary Title="Test" Encoding="UTF-8" Description="a &lt;b&gt; c"/>"#);
    assert_eq!(a.get("Title").unwrap(), "Test");
    assert_eq!(a.get("Encoding").unwrap(), "UTF-8");
    assert_eq!(a.get("Description").unwrap(), "a <b> c");
    assert!(!a.contains_key("Nope"));
}

/// 属性值里出现 `="` 不该把扫描带偏——真实词典的 `Description` 里满是转义过的 HTML。
#[test]
fn 属性值里的等号引号不干扰扫描() {
    let a = attributes(r#"<D A="x &lt;p class=&quot;y&quot;&gt;z" B="2"/>"#);
    assert_eq!(a.get("A").unwrap(), r#"x <p class="y">z"#);
    assert_eq!(a.get("B").unwrap(), "2");
}

#[test]
fn 样式表按三行一组解析() {
    let s = parse_stylesheet("1\n<b>\n</b>\n2\n<i>\n</i>\n");
    assert_eq!(s.len(), 2);
    assert_eq!(s["1"], ("<b>".into(), "</b>".into()));
    assert_eq!(s["2"], ("<i>".into(), "</i>".into()));
    assert!(parse_stylesheet("").is_empty());
}

/// 空的后置片段是合法的：实测 ECDICT 的样式 2 就是「前置 `</br>`，后置为空」。
#[test]
fn 样式的后置片段可以为空() {
    let s = parse_stylesheet("2\n</br>\n\n");
    assert_eq!(s["2"], ("</br>".into(), String::new()));
}

// ── 样式标记展开 ──────────────────────────────────────────

fn sheet() -> HashMap<String, (String, String)> {
    parse_stylesheet("1\n<b>\n</b>\n2\n</br>\n\n4\n<font color=gray>\n</font>\n")
}

#[test]
fn 标记之后的那段被夹住() {
    assert_eq!(apply_styles(&sheet(), "头`1`身"), "头<b>身</b>");
}

/// 相邻标记之间没有文字时，前一个样式包住空串——ECDICT 里 `` `2``4` `` 就是这么用的，
/// 效果是插入一个 `</br>` 再开始灰色。丢掉这个空段就少了一个换行。
#[test]
fn 相邻标记之间的空段也要展开() {
    assert_eq!(
        apply_styles(&sheet(), "释义`2``4`(标注)"),
        "释义</br><font color=gray>(标注)</font>"
    );
}

#[test]
fn 未定义的编号原样跳过而不是丢字() {
    assert_eq!(apply_styles(&sheet(), "a`9`b"), "ab");
}

/// 正文里孤立的反引号（代码片段、省略号）不是标记。误判会把它之后的文字整段吃掉。
#[test]
fn 孤立的反引号不当作标记() {
    for s in ["a`b", "a``b", "a`x`b", "`", "a`12b"] {
        assert_eq!(apply_styles(&sheet(), s), s, "{s:?} 不该被改动");
    }
}

#[test]
fn 没有样式表时原样返回() {
    assert_eq!(apply_styles(&HashMap::new(), "a`1`b"), "a`1`b");
}

// ── 底层细节 ──────────────────────────────────────────────

/// UTF-16 正文里单个 `\0` 字节随处可见（所有 ASCII 字符都带一个），只能按 2 字节
/// 对齐找双零。用单字节规则会在第一个英文字母处就把词条截断。
#[test]
fn utf16的终止符按双字节对齐找() {
    let buf = b"a\0b\0\0\0c\0";
    assert_eq!(find_term(buf, 2), Some(4));
    assert_eq!(find_term(buf, 1), Some(1));
    assert_eq!(find_term(b"abc", 1), None);
    // 双零跨越了对齐边界就不算：`\0` 在奇数位是某个字符的高字节。
    assert_eq!(find_term(b"a\0\0b", 2), None);
}

#[test]
fn 长度字段随版本变宽() {
    let b = [0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 9];
    assert_eq!(num(&b, 0, true), 7);
    assert_eq!(num(&b, 1, false), 7);
    assert_eq!(num(&b, 2, false), 9);
}

#[test]
fn 损坏的输入报错而不是恐慌() {
    let dir = std::env::temp_dir().join("wind-dict-mdx-test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("broken.mdx");

    std::fs::write(&p, b"").unwrap();
    assert!(Mdx::open(&p).is_err(), "空文件");

    // 头部长度写了个天文数字：必须在分配之前就拒绝。
    std::fs::write(&p, [0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]).unwrap();
    assert!(Mdx::open(&p).is_err(), "荒谬的头部长度");

    std::fs::write(&p, b"\x00\x00\x00\x08not-utf16-xml\x00\x00\x00\x00").unwrap();
    assert!(Mdx::open(&p).is_err(), "头部不是合法 XML");
}

/// LZO 与正文加密都是**明确不支持**，不是「碰巧读不出来」。报错必须发生在打开阶段，
/// 让用户知道是词典用了不支持的特性，而不是以为自己的词查不到。
#[test]
fn 不支持的特性给出可归因的错误() {
    let e = decompress(&[1, 0, 0, 0, 0, 0, 0, 0], 0)
        .unwrap_err()
        .to_string();
    assert!(e.contains("LZO"), "错误信息应点名 LZO：{e}");
    let e = decompress(&[9, 0, 0, 0, 0, 0, 0, 0], 0)
        .unwrap_err()
        .to_string();
    assert!(e.contains("9"), "未知压缩类型应报出类型号：{e}");
}

// ── 真实词典 ──────────────────────────────────────────────

/// 样本是造出来的，只有几个词、一个块。块**之间**的二分、等值组跨块、几十 MB 的
/// 索引——这些只有真词典能验。放一份到 `.cache/dict/test.mdx` 即启用（例如
/// ECDICT release 里的 `ecdict-mdx-headless-28.zip`）。缺了就跳过，与字形库同一套办法。
#[test]
fn 真实词典查得动() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/dict/test.mdx");
    let Ok(d) = Mdx::open(&p) else {
        eprintln!("跳过：{} 不存在", p.display());
        return;
    };
    assert!(d.meta().entry_count > 1000, "这不像一本完整词典");
    let hits = d.lookup("abandon").unwrap();
    assert!(!hits.is_empty(), "常用词查不到，多半是键序归一化错了");
    assert!(!hits[0].1.is_empty());
    assert!(!d.prefix("aband", 5).unwrap().is_empty());
}
