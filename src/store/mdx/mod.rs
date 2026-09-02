//! MDX（MDict）词典的**运行时直读**：不导入、不转换，用户把文件放进来就能查。
//!
//! ## 为什么不是「导入成 SQLite」
//!
//! 起初的设计是一次性导入。那个结论来自 `readmdict` 这个 crate 的接口——它只有
//! `items()`，一次把整本词典读进内存——而**不是来自格式本身**。实测推翻了它：
//!
//! ```text
//! ecdict-mdx-headless-28.mdx   69.7 MB   3,402,564 词条
//! 开库读入（= 常驻索引）  0.09 MB      全文件的 0.14%
//! 查一个词             读盘 ~30 KB   瞬时解压峰值 64 KB
//! ```
//!
//! MDX 的索引是**两级**的：文件头附近只记每个 key block 的首词，逐词表压在块里。
//! 查词 = 二分定位块 → 解压那一个块 → 定位 record block → 解压那一个块。两次
//! 64 KB 的解压就是全部代价，与词典大小无关。这正是 docs/adr/0006 要的形状：
//! 常驻内存是 O(块数) 而非 O(词条数)。
//!
//! ## 键序：不复刻它就会静默漏词
//!
//! MDX 的键按「小写 + 去掉 ASCII 标点空白」排序，由文件头的 `KeyCaseSensitive` 与
//! `StripKey` 声明。这不是可以近似的细节——在上述真实词典上实测：
//!
//! | 比较函数 | 340 万键中的逆序对 | 随机 3000 键的命中率 |
//! |---|---|---|
//! | 仅 `lower()` | 510,108 | 2468 / 3000 |
//! | mdict-js 文档里的字符集 | 1,861（漏了 `&`） | —— |
//! | 全部 ASCII 标点空白 | **0** | **3000 / 3000** |
//!
//! 漏掉的 17.7% 不会报错，只会显示「未收录」——用户以为词典里没有这个词。
//!
//! 归一化必然产生重名（实测 199,037 组，最大一组 15 个：`full-house` 与 `fullhouse`
//! 是两个不同词头）。故 [`Mdx::lookup`] 返回**一组**结果而非一条，且等值组可能跨越
//! 块边界，二分定位后必须顺带扫下一块。
//!
//! ## 与 ADR-0001 的关系
//!
//! ADR-0001 拒绝 MDX 时写下的否决条件是「除非 windui 获得富文本渲染能力」。该条件
//! 现已**部分**满足：windui 有了 `RichDoc` 与斜体，本 crate 有了 [`crate::html`]。
//! 但富文本 ≠ HTML+CSS 渲染器，正文的层级信息仍会丢失（详见 `crate::html` 的说明）。
//! 所以用户词典是**补充**，不取代随程序分发的结构化词库。
//!
//! ## 尚未支持的两件事
//!
//! - **LZO 压缩块**（多见于 v1.2 老词典）。不写的理由不是难：是**没有测试向量**。
//!   LZO1X 解压是个指针状态机，写错了不崩溃，只产出错乱的字节。本机装不上
//!   `python-lzo`（无预编译轮子），拿不到可信的对照数据，故宁可明确报错。
//! - **正文加密**（`Encrypted` 含 1 位，需注册码）。这个不打算支持。
//!
//! 两者都在打开时就明确报错，不会等到查词才给出难以归因的空结果。

mod ripemd128;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

use ripemd128::ripemd128;

/// 头部声明的正文编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Le,
    /// Windows 代码页（936 = GBK，950 = Big5）。老词典常用。
    CodePage(u32),
}

impl Encoding {
    /// 词条与正文的终止符宽度。UTF-16 里单个 `\0` 字节随处可见，必须按 2 字节对齐找。
    fn term_width(self) -> usize {
        if self == Encoding::Utf16Le {
            2
        } else {
            1
        }
    }
}

/// 头部元数据里本模块用得上的部分。
#[derive(Debug, Clone)]
pub struct Meta {
    pub title: String,
    /// 词典自述，是 HTML 片段。交给 [`crate::html::to_blocks`] 才能显示。
    pub description: String,
    pub entry_count: u64,
    /// 引擎版本。2.0 起所有长度字段从 4 字节变 8 字节，是整个格式最大的分叉点。
    pub version: f32,
    encoding: Encoding,
    case_sensitive: bool,
    strip_key: bool,
}

/// key block 的索引项。**只存首词**——末词对定位没有额外作用（块内扫描自会终止），
/// 存了就是让每本词典多占一倍索引内存。
struct KeyBlock {
    first_norm: String,
    file_off: u64,
    comp_size: u64,
}

/// record block 的索引项。`dec_off` 是它在「全部块解压后首尾相接」这个虚拟空间里的
/// 起点——词条记录的偏移就是按那个空间给的，故必须两套偏移都留着。
struct RecBlock {
    file_off: u64,
    comp_size: u64,
    dec_off: u64,
    dec_size: u64,
}

pub struct Mdx {
    file: RefCell<File>,
    meta: Meta,
    keys: Vec<KeyBlock>,
    recs: Vec<RecBlock>,
    /// `` `N` `` 标记到 (前置, 后置) 的映射，见 [`Mdx::apply_styles`]。
    styles: HashMap<String, (String, String)>,
    /// 单槽缓存。一个词的多个义项常落在同一个 record block 里，重复解压 64 KB
    /// 纯属浪费；而多槽 LRU 对这个访问模式没有额外收益。
    cache: RefCell<Option<(usize, Vec<u8>)>>,
}

impl Mdx {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f =
            File::open(path).with_context(|| format!("打不开 MDX 文件：{}", path.display()))?;
        let (meta, styles, encrypted, mut pos) = read_header(&mut f)?;

        ensure!(
            encrypted & 1 == 0,
            "这本词典的正文是加密的（需要 MDict 注册码），本程序不支持"
        );

        let wide = meta.version >= 2.0;
        let nw = if wide { 8 } else { 4 };

        // ── 词条索引区 ──
        let head = read_at(&mut f, pos, nw * if wide { 5 } else { 4 })?;
        let (info_size, blocks_size) = if wide {
            pos += 40 + 4; // 5 个 u64，再跳过一个 adler32 校验和
            (num(&head, 3, wide), num(&head, 4, wide))
        } else {
            pos += 16;
            (num(&head, 2, wide), num(&head, 3, wide))
        };
        let mut meta = meta;
        meta.entry_count = num(&head, 1, wide);

        let info = read_at(&mut f, pos, info_size as usize)?;
        pos += info_size;
        let keys = parse_key_index(&info, &meta, wide, encrypted, pos)?;
        pos += blocks_size;

        // ── 正文区 ──
        let head = read_at(&mut f, pos, nw * 4)?;
        pos += nw as u64 * 4;
        let n_rec = num(&head, 0, wide);
        let rinfo_size = num(&head, 2, wide);
        let rinfo = read_at(&mut f, pos, rinfo_size as usize)?;
        pos += rinfo_size;

        let mut recs = Vec::with_capacity(n_rec as usize);
        let (mut off_c, mut off_d) = (0u64, 0u64);
        for i in 0..n_rec as usize {
            ensure!(
                (i * 2 + 2) * nw <= rinfo.len(),
                "正文块索引被截断：声称 {n_rec} 块，实际只有 {} 字节",
                rinfo.len()
            );
            let comp_size = num(&rinfo, i * 2, wide);
            let dec_size = num(&rinfo, i * 2 + 1, wide);
            recs.push(RecBlock {
                file_off: pos + off_c,
                comp_size,
                dec_off: off_d,
                dec_size,
            });
            off_c += comp_size;
            off_d += dec_size;
        }

        Ok(Self {
            file: RefCell::new(f),
            meta,
            keys,
            recs,
            styles,
            cache: RefCell::new(None),
        })
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// 查词。返回**全部**归一化后与之相等的条目：`(词头, HTML 正文)`。
    ///
    /// 返回多条不是异常：`full-house` 与 `fullhouse` 归一化后相同，MDict 自己也把
    /// 它们一并列出。词头是原样的，正文已做过样式表替换。
    pub fn lookup(&self, query: &str) -> Result<Vec<(String, String)>> {
        let qn = self.normalize(query);
        if qn.is_empty() || self.keys.is_empty() {
            return Ok(Vec::new());
        }
        let start = self.block_for(&qn);
        let mut hits = Vec::new();
        // 等值组可能压在块边界上，故定位到的块之后还要看一块。
        for bi in start..(start + 2).min(self.keys.len()) {
            for (key, off) in self.scan_keys(bi)? {
                if self.normalize(&key) == qn {
                    hits.push((key, off));
                }
            }
        }
        hits.into_iter()
            .map(|(key, off)| Ok((key, self.record(off)?)))
            .collect()
    }

    /// 前缀补全。返回原样词头，按词典自身的顺序，至多 `limit` 条。
    pub fn prefix(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let pn = self.normalize(prefix);
        if pn.is_empty() || self.keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut bi = self.block_for(&pn);
        while bi < self.keys.len() && out.len() < limit {
            // 整块扫完再决定要不要继续，而不是一遇到越界的词就中断：块**之间**有序是
            // 格式保证的，块**内**的次序则由造词典的工具决定，不该被依赖。
            let mut past = false;
            for (key, _) in self.scan_keys(bi)? {
                let kn = self.normalize(&key);
                if kn.starts_with(&pn) {
                    if out.len() < limit {
                        out.push(key);
                    }
                } else if kn.as_str() > pn.as_str() {
                    past = true;
                }
            }
            if past {
                break;
            }
            bi += 1;
        }
        Ok(out)
    }

    /// 归一化：把查询词与词头拉到同一把尺子上。规则由文件头声明，不是本程序的偏好。
    fn normalize(&self, s: &str) -> String {
        normalize_with(&self.meta, s)
    }

    /// 二分出「首词 ≤ 目标」的最后一个块。目标比首个块的首词还小时返回 0，
    /// 后续扫描自然查无此词。
    fn block_for(&self, norm: &str) -> usize {
        let (mut lo, mut hi) = (0usize, self.keys.len() - 1);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.keys[mid].first_norm.as_str() <= norm {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }

    /// 解压一个 key block，取出其中的 `(词头, 正文偏移)`。
    fn scan_keys(&self, bi: usize) -> Result<Vec<(String, u64)>> {
        let b = &self.keys[bi];
        let raw = read_at(
            &mut self.file.borrow_mut(),
            b.file_off,
            b.comp_size as usize,
        )?;
        let buf = decompress(&raw, b.file_off)?;
        let wide = self.meta.version >= 2.0;
        let nw = if wide { 8 } else { 4 };
        let tw = self.meta.encoding.term_width();

        let mut out = Vec::new();
        let mut p = 0usize;
        while p + nw <= buf.len() {
            let off = num(&buf[p..], 0, wide);
            p += nw;
            let end = find_term(&buf[p..], tw).unwrap_or(buf.len() - p);
            out.push((self.decode(&buf[p..p + end]), off));
            p += end + tw;
        }
        Ok(out)
    }

    /// 取一条正文：定位到含该偏移的 record block，只解压那一块。
    fn record(&self, off: u64) -> Result<String> {
        let bi = self
            .recs
            .binary_search_by(|b| {
                if off < b.dec_off {
                    std::cmp::Ordering::Greater
                } else if off >= b.dec_off + b.dec_size {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .map_err(|_| anyhow::anyhow!("词条偏移 {off} 落在所有正文块之外，文件可能损坏"))?;

        let mut cache = self.cache.borrow_mut();
        if !matches!(&*cache, Some((i, _)) if *i == bi) {
            let b = &self.recs[bi];
            let raw = read_at(
                &mut self.file.borrow_mut(),
                b.file_off,
                b.comp_size as usize,
            )?;
            *cache = Some((bi, decompress(&raw, b.file_off)?));
        }
        let buf = &cache.as_ref().expect("刚填过").1;

        let start = (off - self.recs[bi].dec_off) as usize;
        ensure!(start <= buf.len(), "词条偏移越过了所在正文块，文件可能损坏");
        let tw = self.meta.encoding.term_width();
        let end = start + find_term(&buf[start..], tw).unwrap_or(buf.len() - start);
        Ok(self.apply_styles(&self.decode(&buf[start..end])))
    }

    fn decode(&self, bytes: &[u8]) -> String {
        decode_with(self.meta.encoding, bytes)
    }

    /// 展开正文里的 `` `N` `` 样式标记。
    ///
    /// MDict 把重复的 HTML 片段抽进头部的 `StyleSheet`，正文只留编号——一种在 CSS
    /// 之外的自带语义通路。**不展开就会看到满屏反引号数字**。规则是：标记之后、下一个
    /// 标记（或结尾）之前的那段文字，被该编号的前后置片段夹住。
    fn apply_styles(&self, txt: &str) -> String {
        apply_styles(&self.styles, txt)
    }
}

/// 见 [`Mdx::apply_styles`]。拆成自由函数是为了能直接对着字符串测——它是本模块里
/// 唯一一段纯字符串变换的逻辑，没理由非得先造出一本词典才能验证。
fn apply_styles(styles: &HashMap<String, (String, String)>, txt: &str) -> String {
    if styles.is_empty() || !txt.contains('`') {
        return txt.to_string();
    }
    // 首个标记之前的文字不属于任何样式，原样输出。
    let Some((at, id, after)) = find_marker(txt) else {
        return txt.to_string();
    };
    let mut out = String::with_capacity(txt.len());
    out.push_str(&txt[..at]);
    let (mut id, mut rest) = (id, after);
    loop {
        let (seg, next) = match find_marker(rest) {
            Some((a, nid, tail)) => (&rest[..a], Some((nid, tail))),
            None => (rest, None),
        };
        let (open, close) = styles
            .get(&id)
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .unwrap_or(("", ""));
        out.push_str(open);
        // MDict 的约定：段落以换行结尾时，闭合片段要挪到换行之前。
        if let Some(stripped) = seg.strip_suffix('\n') {
            out.push_str(stripped.trim_end());
            out.push_str(close);
            out.push_str("\r\n");
        } else {
            out.push_str(seg);
            out.push_str(close);
        }
        match next {
            Some((nid, tail)) => (id, rest) = (nid, tail),
            None => break,
        }
    }
    out
}

/// 找下一个 `` `数字` `` 标记，返回 (起始下标, 编号, 标记之后的切片)。
///
/// 必须验证结尾那个反引号：正文里单独出现的反引号（英文里的省略号、代码片段）
/// 不是标记，误判会把后面整段吃掉。
fn find_marker(s: &str) -> Option<(usize, String, &str)> {
    let mut from = 0usize;
    while let Some(rel) = s[from..].find('`') {
        let at = from + rel;
        let rest = &s[at + 1..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && rest[digits..].starts_with('`') {
            return Some((at, rest[..digits].to_string(), &rest[digits + 1..]));
        }
        from = at + 1;
    }
    None
}

// ── 归一化 ────────────────────────────────────────────────

/// `StripKey` 要去掉的字符：ASCII 的空白与标点，**仅此而已**。
///
/// 边界是实测钉死的，两侧都试过：mdict-js 文档里的集合少了 `&`，在 340 万键上留下
/// 1,861 处逆序；而「非字母数字全去」会连重音字母一起吃掉（`Nīl` → `Nl`），留下
/// 6,795 处。取当前这一集时逆序为 0。
fn is_stripped(c: char) -> bool {
    c.is_ascii_whitespace() || c.is_ascii_punctuation()
}

// ── 二进制读取 ────────────────────────────────────────────

/// 按版本取第 `i` 个长度字段。2.0 起是 8 字节，之前是 4 字节，全部大端。
fn num(buf: &[u8], i: usize, wide: bool) -> u64 {
    if wide {
        let mut b = [0u8; 8];
        b.copy_from_slice(&buf[i * 8..i * 8 + 8]);
        u64::from_be_bytes(b)
    } else {
        let mut b = [0u8; 4];
        b.copy_from_slice(&buf[i * 4..i * 4 + 4]);
        u32::from_be_bytes(b) as u64
    }
}

fn read_at(f: &mut File, off: u64, len: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(off))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)
        .with_context(|| format!("读取 {len} 字节 @ {off} 失败，文件可能被截断"))?;
    Ok(buf)
}

/// 找终止符。`tw == 2` 时必须按 2 字节对齐找双零——UTF-16 正文里单个 `\0` 到处都是。
fn find_term(buf: &[u8], tw: usize) -> Option<usize> {
    if tw == 1 {
        buf.iter().position(|b| *b == 0)
    } else {
        (0..buf.len().saturating_sub(1))
            .step_by(2)
            .find(|&i| buf[i] == 0 && buf[i + 1] == 0)
    }
}

/// 块头是 4 字节压缩类型 + 4 字节 adler32，其后才是数据。
///
/// 不另行校验 adler32：zlib 流自带一份，`miniz_oxide` 解压时已经验过；类型 0 的裸块
/// 没有可校验的东西。多写一份 adler32 实现，换来的只是对同一件事的第二次确认。
fn decompress(blob: &[u8], at: u64) -> Result<Vec<u8>> {
    ensure!(blob.len() >= 8, "压缩块 @ {at} 不足 8 字节");
    let kind = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    let body = &blob[8..];
    match kind {
        0 => Ok(body.to_vec()),
        1 => bail!(
            "这本词典用 LZO 压缩（多见于 2.0 之前的老词典），本程序暂不支持——\
             缺的是可信的测试数据，不是实现难度"
        ),
        2 => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(body)
                .read_to_end(&mut out)
                .with_context(|| format!("解压块 @ {at} 失败"))?;
            Ok(out)
        }
        k => bail!("压缩类型 {k} 未知（块 @ {at}），文件可能损坏"),
    }
}

/// MDX 的键索引「加密」：从块内第 4–8 字节派生密钥后逐字节换位异或。
///
/// 密钥的原料就在文件里，所以这不是保密手段，是防呆——防的是用文本编辑器直接
/// 翻词表。照做即可，不必也不该把它当安全边界。
fn deobfuscate(blob: &[u8]) -> Vec<u8> {
    let mut seed = [0u8; 8];
    seed[..4].copy_from_slice(&blob[4..8]);
    seed[4..].copy_from_slice(&0x3695u32.to_le_bytes());
    let key = ripemd128(&seed);

    let mut out = blob[..8].to_vec();
    let mut prev = 0x36u8;
    for (i, &b) in blob[8..].iter().enumerate() {
        let t = b.rotate_left(4) ^ prev ^ (i as u8) ^ key[i % key.len()];
        prev = b;
        out.push(t);
    }
    out
}

// ── 头部 ──────────────────────────────────────────────────

/// 上限纯为防御损坏文件：真实词典的头部是几 KB 量级，读到一个荒谬的长度就该停手，
/// 而不是先分配几 GB 再失败。
const MAX_HEADER: u32 = 4 * 1024 * 1024;

type Header = (Meta, HashMap<String, (String, String)>, u32, u64);

fn read_header(f: &mut File) -> Result<Header> {
    let mut len = [0u8; 4];
    f.read_exact(&mut len).context("读不到头部长度")?;
    let hlen = u32::from_be_bytes(len);
    ensure!(
        hlen > 0 && hlen <= MAX_HEADER,
        "头部长度 {hlen} 不合理，这多半不是一个 MDX 文件"
    );
    let mut buf = vec![0u8; hlen as usize];
    f.read_exact(&mut buf).context("头部被截断")?;
    let mut _ck = [0u8; 4];
    f.read_exact(&mut _ck).context("头部校验和缺失")?;

    let xml = utf16le(&buf);
    let a = attributes(&xml);

    let version: f32 = a
        .get("GeneratedByEngineVersion")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.2);
    ensure!(
        (1.0..=3.0).contains(&version),
        "引擎版本 {version} 超出已知范围"
    );

    let enc_name = a.get("Encoding").map(String::as_str).unwrap_or("UTF-8");
    let encoding = match enc_name.to_ascii_uppercase().replace('-', "").as_str() {
        "" | "UTF8" => Encoding::Utf8,
        "UTF16" | "UTF16LE" => Encoding::Utf16Le,
        "GBK" | "GB2312" | "GB18030" => Encoding::CodePage(936),
        "BIG5" | "BIG5HKSCS" => Encoding::CodePage(950),
        other => bail!("未知的正文编码 {other}"),
    };

    // `Encrypted` 是位掩码，但老文件里写 Yes/No。1 位 = 正文加密，2 位 = 键索引加扰。
    let encrypted = match a.get("Encrypted").map(String::as_str).unwrap_or("0") {
        "Yes" | "yes" => 1,
        "No" | "no" | "" => 0,
        s => s.parse().unwrap_or(0),
    };

    let meta = Meta {
        title: a.get("Title").cloned().unwrap_or_default(),
        description: a.get("Description").cloned().unwrap_or_default(),
        entry_count: 0, // 词条区读到之后再填
        version,
        encoding,
        // 两个属性都是「缺省即宽松」：MDict 自己的默认就是不区分大小写、要去标点。
        case_sensitive: yes(a.get("KeyCaseSensitive")),
        strip_key: a.get("StripKey").is_none_or(|v| yes(Some(v))),
    };
    let styles = parse_stylesheet(a.get("StyleSheet").map(String::as_str).unwrap_or(""));
    let pos = 4 + hlen as u64 + 4;
    Ok((meta, styles, encrypted, pos))
}

fn yes(v: Option<&String>) -> bool {
    matches!(v.map(|s| s.as_str()), Some("Yes") | Some("yes") | Some("1"))
}

/// 从 `<Dictionary a="1" b="2"/>` 里取出属性。
///
/// 不引 XML 解析器：头部永远是**单个自闭合元素**，没有子节点、没有命名空间，
/// 属性值里也不会出现裸引号（真值里的 `<`、`&` 都是转义过的）。为这个形状引一个
/// 通用解析器，是拿依赖换一个用不上的通用性。
fn attributes(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let b = xml.as_bytes();
    let mut i = 0usize;
    while let Some(eq) = xml[i..].find("=\"") {
        let eq = i + eq;
        // 往回收拢属性名。
        let mut s = eq;
        while s > 0 && (b[s - 1].is_ascii_alphanumeric() || b[s - 1] == b'_') {
            s -= 1;
        }
        let val_start = eq + 2;
        let Some(end) = xml[val_start..].find('"') else {
            break;
        };
        let end = val_start + end;
        if s < eq {
            out.insert(
                xml[s..eq].to_string(),
                crate::html::unescape(&xml[val_start..end]),
            );
        }
        i = end + 1;
    }
    out
}

/// 样式表是「编号 / 前置 / 后置」三行一组的纯文本。
fn parse_stylesheet(raw: &str) -> HashMap<String, (String, String)> {
    let lines: Vec<&str> = raw.lines().collect();
    lines
        .chunks(3)
        .filter(|c| c.len() == 3 && !c[0].trim().is_empty())
        .map(|c| {
            (
                c[0].trim().to_string(),
                (c[1].to_string(), c[2].to_string()),
            )
        })
        .collect()
}

// ── 词条索引 ──────────────────────────────────────────────

fn parse_key_index(
    info: &[u8],
    meta: &Meta,
    wide: bool,
    encrypted: u32,
    blocks_start: u64,
) -> Result<Vec<KeyBlock>> {
    // v2.0 的索引整体压缩，且可能加扰；v1.2 是裸的。
    let plain: Vec<u8> = if wide {
        ensure!(info.len() >= 8, "词条索引区过短");
        let src = if encrypted & 2 != 0 {
            deobfuscate(info)
        } else {
            info.to_vec()
        };
        decompress(&src, 0).context("词条索引解压失败（若本词典加扰，说明解扰环节出错）")?
    } else {
        info.to_vec()
    };

    let nw = if wide { 8 } else { 4 };
    // 长度字段的宽度、以及词后有无终止符，都随版本变。
    let (lw, term) = if wide {
        (2usize, 1usize)
    } else {
        (1usize, 0usize)
    };
    let cw = meta.encoding.term_width(); // UTF-16 时字符数要乘 2

    let mut out = Vec::new();
    let mut p = 0usize;
    let mut off = 0u64;
    while p < plain.len() {
        ensure!(p + nw <= plain.len(), "词条索引在块计数处截断");
        p += nw; // 本块词条数，定位用不上

        let word = |p: &mut usize| -> Result<String> {
            ensure!(*p + lw <= plain.len(), "词条索引在长度字段处截断");
            let n = if lw == 2 {
                u16::from_be_bytes([plain[*p], plain[*p + 1]]) as usize
            } else {
                plain[*p] as usize
            };
            *p += lw;
            let bytes = n * cw;
            ensure!(*p + bytes <= plain.len(), "词条索引在词头处截断");
            let s = decode_with(meta.encoding, &plain[*p..*p + bytes]);
            *p += bytes + term * cw;
            Ok(s)
        };
        let first = word(&mut p)?;
        let _last = word(&mut p)?;

        ensure!(p + nw * 2 <= plain.len(), "词条索引在块大小处截断");
        let comp_size = num(&plain[p..], 0, wide);
        p += nw * 2; // 压缩大小 + 解压大小，后者定位用不上

        out.push(KeyBlock {
            first_norm: normalize_with(meta, &first),
            file_off: blocks_start + off,
            comp_size,
        });
        off += comp_size;
    }
    ensure!(!out.is_empty(), "词典里一个词条块也没有");
    Ok(out)
}

/// 归一化的唯一实现。拎成自由函数是因为开库阶段还没有 `Mdx`——而同一套规则写两遍
/// 就等着漂移，那正是 `super::prefix_upper_bound` 已经记过一次的教训。
fn normalize_with(meta: &Meta, s: &str) -> String {
    let lowered;
    let s = if meta.case_sensitive {
        s
    } else {
        lowered = s.to_lowercase();
        &lowered
    };
    if !meta.strip_key {
        return s.to_string();
    }
    s.chars().filter(|c| !is_stripped(*c)).collect()
}

fn decode_with(enc: Encoding, bytes: &[u8]) -> String {
    match enc {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Utf16Le => utf16le(bytes),
        Encoding::CodePage(cp) => decode_code_page(cp, bytes),
    }
}

/// 尾部落单的半个码元直接丢弃：`as_chunks` 只给整对，余数在 `.1` 里，我们不要。
fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// 老词典的 GBK / Big5 正文。
///
/// 走系统的 `MultiByteToWideChar` 而非引入编码表 crate：这些代码页的映射表是
/// Windows 自带的，而本项目已经是 Windows 优先（docs/adr/0005）且已依赖 `windows`
/// crate。引一份几百 KB 的静态表进二进制，去做一件系统已经做好的事，不划算。
#[cfg(windows)]
fn decode_code_page(cp: u32, bytes: &[u8]) -> String {
    use windows::Win32::Globalization::MultiByteToWideChar;
    if bytes.is_empty() {
        return String::new();
    }
    // SAFETY: 传入的是本进程持有的切片，长度如实给出；先探长度再按长度分配。
    let n = unsafe { MultiByteToWideChar(cp, Default::default(), bytes, None) };
    if n <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut buf = vec![0u16; n as usize];
    let n = unsafe { MultiByteToWideChar(cp, Default::default(), bytes, Some(&mut buf)) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

#[cfg(not(windows))]
fn decode_code_page(_cp: u32, bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests;
