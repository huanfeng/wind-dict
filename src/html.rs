//! HTML 片段 → 结构化文本块。供自带词典（MDX）的正文使用。
//!
//! ## 这不是 HTML 渲染器
//!
//! docs/adr/0001 拒绝的是**渲染器**：HTML 解析 + 盒模型 + 内联排版 + CSS 级联，
//! 工作量超过词典本体。本模块是一个**有界的标签映射器**：块级标签变段落边界、
//! `<b>`/`<em>` 变样式位、其余丢弃。**CSS 一行都不碰**——碰了就是在重蹈那份 ADR。
//!
//! 代价是诚实的：语义会丢。实测一份真实的牛津 MDX，62 个 class 名**全部**是
//! `ysl`/`aw5`/`k0z` 这样的 3–4 位哈希串，「哪段是释义、哪段是例句」只存在于外部
//! CSS 里。没有 CSS 就分不出来，这不是本模块偷懒，是这个格式的性质。
//!
//! ## 唯一幸存的语义信号
//!
//! 剥掉 CSS 之后，`<em>` / `<i>` 是**唯一**还能表达语义的标签——例句、语体标注
//! （*informal*）、拉丁学名都靠它。所以斜体不是可选的润色，是这条路上仅存的结构。
//! （windui 的斜体正是为此而加。）
//!
//! ## 为什么必须尊重块级标签
//!
//! 直接剥标签会把一切黏成一坨，实测真实牛津词条长这样：
//!
//! ```text
//! @symbol1At (used to indicate cost or rate per unit): 30 dictionaries @ £29.99 each1.1 informal At...
//! ```
//!
//! 把 `div`/`p`/`li`/`br` 当作段落边界之后才成为可读的东西。这一条是整个转换里
//! 最要紧的：丢样式只是难看，丢边界是不可读。
//!
//! ## 丢掉 CSS 的确切代价
//!
//! 同一条词条转换后是：
//!
//! ```text
//! @
//! symbol
//! 1At (used to indicate cost or rate per unit): 30 dictionaries @ £29.99 each
//! 1.1 informal At (in any sense): he dealt fairly well with tough issues thrown @ him
//! ```
//!
//! 注意 `1At`——义项号与释义黏着。这**忠实于原文**（`<span>1</span><span>At…`
//! 之间本就没有空白，间距由 CSS 的 `margin` 给），不是转换的 bug。没有级联就补不出
//! 这个空隙，除非去猜「哪个 span 是义项号」，而 class 名是哈希串，猜不了。
//!
//! 记在这里是为了别把它当缺陷去「修」：能修的只有引入 CSS，那正是 ADR-0001 拒绝的。

/// 一段文字里的一截，样式一致。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// `entry://词` 形式的跳转目标（已去掉协议前缀）。其余链接不留——外部 URL
    /// 在一个离线词典里点了也没有意义。
    pub link: Option<String>,
}

/// 一个段落。`indent` 是列表嵌套层级，0 为不缩进。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextBlock {
    pub indent: u8,
    pub runs: Vec<TextRun>,
}

impl TextBlock {
    /// 本段的纯文字。判空与测试用。
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// 会切断段落的块级标签。
///
/// `br` 也在内：它虽是空元素，作用却是段落边界，这正是它该在的语义位置。
const BLOCK: &[&str] = &[
    "div",
    "p",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "td",
    "th",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "dl",
    "dt",
    "dd",
    "hr",
    "section",
    "article",
];

/// 整块内容都要丢弃的标签——不只是标签本身，连同它包住的文字。
///
/// `script` 与 `style` 的内容是代码不是正文。实测那份牛津 MDX 里有 1,358 个
/// `<script>`：把它们的内容当文字渲染出来，用户会在释义中间看到一段 JavaScript。
const DROP_CONTENT: &[&str] = &["script", "style", "head"];

/// 解析出的一个词法单元。
enum Token<'a> {
    Text(&'a str),
    Open { name: String, attrs: &'a str },
    Close(String),
}

/// 逐个吐出词法单元。不建树——建树是渲染器的活，这里只要线性的段落。
struct Lexer<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let b = self.s.as_bytes();
        if self.i >= b.len() {
            return None;
        }
        if b[self.i] != b'<' {
            let start = self.i;
            while self.i < b.len() && b[self.i] != b'<' {
                self.i += 1;
            }
            return Some(Token::Text(&self.s[start..self.i]));
        }
        // 走到配对的 '>'。找不到就说明尾部截断了，把剩下的当文字，不丢内容。
        let Some(end) = self.s[self.i..].find('>').map(|k| self.i + k) else {
            let rest = &self.s[self.i..];
            self.i = b.len();
            return Some(Token::Text(rest));
        };
        let inner = &self.s[self.i + 1..end];
        self.i = end + 1;
        // 注释与 <!DOCTYPE> 之类，整个跳过。
        if inner.starts_with('!') {
            return self.next();
        }
        let (closing, inner) = match inner.strip_prefix('/') {
            Some(r) => (true, r),
            None => (false, inner),
        };
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            return self.next();
        }
        Some(if closing {
            Token::Close(name)
        } else {
            Token::Open {
                name,
                attrs: &inner[name_end..],
            }
        })
    }
}

/// 取属性值。只认 `name="value"` 与 `name='value'` 两种写法。
///
/// 不认无引号的裸值：MDX 的 HTML 是机器生成的，实测一律带引号；为一个不出现的
/// 情况写解析分支，等于给自己多留一条没测过的路。
fn attr(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(k) = lower[from..].find(name) {
        let at = from + k;
        let rest = attrs[at + name.len()..].trim_start();
        let rest = match rest.strip_prefix('=') {
            Some(r) => r.trim_start(),
            None => {
                from = at + name.len();
                continue;
            }
        };
        let q = rest.chars().next()?;
        if q == '"' || q == '\'' {
            let body = &rest[1..];
            let end = body.find(q)?;
            return Some(body[..end].to_string());
        }
        from = at + name.len();
    }
    None
}

/// 解实体引用。只解常见的那几个 + 数字引用。
///
/// 不引入 HTML 实体全表（2,231 项）：词典正文里出现的实体高度集中，而那张表会让
/// 二进制多出几十 KB——这个项目以体积为卖点（docs/adr/0006）。未识别的实体**原样
/// 保留**，不吞掉：显示成 `&hellip;` 虽丑，总好过静默消失。
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..semi];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" | "#160" => Some('\u{a0}'),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                // 认不出就把整个引用原样留下，别吞。
                out.push_str(&tail[..=semi]);
                rest = &tail[semi + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 把一段 HTML 转成结构化文本块。
pub fn to_blocks(html: &str) -> Vec<TextBlock> {
    let mut blocks: Vec<TextBlock> = Vec::new();
    let mut cur = TextBlock::default();
    // 三个计数器而非布尔：标签会嵌套，`<b><b>x</b>y</b>` 里的 y 仍该是粗体。
    // 用布尔的话内层的结束标签会把外层一起关掉。
    let (mut bold, mut italic, mut list_depth) = (0u32, 0u32, 0u32);
    let mut link: Option<String> = None;
    let mut drop_until: Option<String> = None;

    let flush = |cur: &mut TextBlock, blocks: &mut Vec<TextBlock>, depth: u32| {
        // 全是空白的段落不留：7 层 div 嵌套会产生大量空段，逐个画出来是一屏空行。
        if !cur.plain().trim().is_empty() {
            cur.runs.retain(|r| !r.text.is_empty());
            cur.indent = depth.min(u8::MAX as u32) as u8;
            blocks.push(std::mem::take(cur));
        } else {
            cur.runs.clear();
        }
    };

    for tok in (Lexer { s: html, i: 0 }) {
        // 丢弃模式：吞掉一切直到对应的结束标签。
        if let Some(name) = &drop_until {
            if let Token::Close(c) = &tok {
                if c == name {
                    drop_until = None;
                }
            }
            continue;
        }
        match tok {
            Token::Text(t) => {
                let t = unescape(t);
                // 空白折叠成单个空格。HTML 的换行与缩进在这里是噪音，照搬会让每段
                // 文字前后带一堆空白，在 RichDoc 里是看得见的。
                let mut norm = String::with_capacity(t.len());
                let mut sp = cur.plain().is_empty();
                for c in t.chars() {
                    if c.is_whitespace() {
                        if !sp {
                            norm.push(' ');
                            sp = true;
                        }
                    } else {
                        norm.push(c);
                        sp = false;
                    }
                }
                if !norm.is_empty() {
                    cur.runs.push(TextRun {
                        text: norm,
                        bold: bold > 0,
                        italic: italic > 0,
                        link: link.clone(),
                    });
                }
            }
            Token::Open { name, attrs } => {
                if DROP_CONTENT.contains(&name.as_str()) {
                    drop_until = Some(name);
                    continue;
                }
                match name.as_str() {
                    "b" | "strong" => bold += 1,
                    "i" | "em" | "cite" | "var" => italic += 1,
                    "a" => {
                        // 只留内部跳转。外部 URL 在离线词典里点了没有意义，
                        // 而把它渲染成可点的东西是在承诺做不到的事。
                        link = attr(attrs, "href")
                            .and_then(|h| h.strip_prefix("entry://").map(str::to_string))
                            .map(|t| {
                                // `entry://apple#apple__2` 的锚点部分不是词头。
                                t.split('#').next().unwrap_or(&t).to_string()
                            })
                            .filter(|t| !t.is_empty());
                    }
                    "ul" | "ol" => {
                        flush(&mut cur, &mut blocks, list_depth);
                        list_depth += 1;
                    }
                    n if BLOCK.contains(&n) => flush(&mut cur, &mut blocks, list_depth),
                    _ => {}
                }
            }
            Token::Close(name) => match name.as_str() {
                "b" | "strong" => bold = bold.saturating_sub(1),
                "i" | "em" | "cite" | "var" => italic = italic.saturating_sub(1),
                "a" => link = None,
                "ul" | "ol" => {
                    flush(&mut cur, &mut blocks, list_depth);
                    list_depth = list_depth.saturating_sub(1);
                }
                n if BLOCK.contains(&n) => flush(&mut cur, &mut blocks, list_depth),
                _ => {}
            },
        }
    }
    flush(&mut cur, &mut blocks, list_depth);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(html: &str) -> Vec<String> {
        to_blocks(html).iter().map(|b| b.plain()).collect()
    }

    /// 整个转换里最要紧的一条：块级标签必须切断段落。
    ///
    /// 不切的话真实牛津词条会变成
    /// `@symbol1At (used to indicate...)30 dictionaries @ £29.99 each1.1 informal At...`
    /// ——丢样式只是难看，丢边界是不可读。
    #[test]
    fn 块级标签切断段落() {
        assert_eq!(
            plain("<div>一</div><div>二</div>"),
            vec!["一".to_string(), "二".to_string()]
        );
        assert_eq!(plain("甲<br>乙"), vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(
            plain("<p>a</p><h2>b</h2><li>c</li>"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    /// 7 层 div 嵌套会产生大量空段，逐个画出来是一屏空行。
    #[test]
    fn 空段落不留() {
        assert_eq!(plain("<div><div><div>x</div></div></div>"), vec!["x"]);
        assert!(plain("<div></div><div>  </div>").is_empty());
    }

    #[test]
    fn 粗体与斜体标出来() {
        let b = to_blocks("<b>粗</b>常<em>斜</em>");
        assert_eq!(b.len(), 1);
        let r = &b[0].runs;
        assert_eq!(r.len(), 3);
        assert!(r[0].bold && !r[0].italic);
        assert!(!r[1].bold && !r[1].italic);
        assert!(r[2].italic && !r[2].bold);
    }

    /// 标签会嵌套。用布尔而非计数器的话，内层的 `</b>` 会把外层一起关掉，
    /// 于是 `y` 变成非粗体。
    #[test]
    fn 嵌套的样式标签不会被内层关掉() {
        let b = to_blocks("<b>x<b>z</b>y</b>");
        assert!(b[0].runs.iter().all(|r| r.bold), "内层结束标签关掉了外层");
    }

    /// 未配对的结束标签不能让计数器回绕（`0 - 1` 在 u32 上是天文数字）。
    #[test]
    fn 多余的结束标签不会回绕() {
        let b = to_blocks("</b></i>文字");
        assert_eq!(b.len(), 1);
        assert!(!b[0].runs[0].bold && !b[0].runs[0].italic);
    }

    #[test]
    fn 内部跳转留下外部链接丢弃() {
        let b = to_blocks(r#"<a href="entry://pear">梨</a>"#);
        assert_eq!(b[0].runs[0].link.as_deref(), Some("pear"));
        // 锚点不是词头的一部分。
        let b = to_blocks(r#"<a href="entry://apple#apple__2">苹果</a>"#);
        assert_eq!(b[0].runs[0].link.as_deref(), Some("apple"));
        // 外部 URL 在离线词典里点了没意义，不做成可点的。
        let b = to_blocks(r#"<a href="https://example.com">外</a>"#);
        assert_eq!(b[0].runs[0].link, None);
    }

    /// 实测那份牛津 MDX 里有 1,358 个 `<script>`。把内容当文字渲染出来，
    /// 用户会在释义中间看到一段 JavaScript。
    #[test]
    fn 脚本与样式的内容整块丢弃() {
        assert_eq!(
            plain("<div>正文</div><script>alert(1)</script>"),
            vec!["正文"]
        );
        assert_eq!(plain("<style>.a{color:red}</style><p>x</p>"), vec!["x"]);
    }

    #[test]
    fn 图片与外链样式表不留痕迹() {
        assert_eq!(
            plain(r#"<link rel="stylesheet"href="t.css"><div>x</div><img src="a.png">"#),
            vec!["x"]
        );
    }

    #[test]
    fn 实体引用解得开() {
        assert_eq!(plain("a&amp;b&lt;c&gt;d"), vec!["a&b<c>d"]);
        assert_eq!(plain("&#65;&#x42;"), vec!["AB"]);
        // 认不出的原样保留，不吞——显示成 `&hellip;` 虽丑，总好过静默消失。
        assert_eq!(plain("x&zzz;y"), vec!["x&zzz;y"]);
    }

    #[test]
    fn 空白折叠成单空格() {
        assert_eq!(plain("<div>  a   \n\t  b  </div>"), vec!["a b "]);
    }

    #[test]
    fn 列表带缩进() {
        let b = to_blocks("<ul><li>一</li><li>二</li></ul>");
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|x| x.indent == 1), "列表项应当缩进一级");
    }

    /// 尾部被截断的标签不该吞掉内容。
    #[test]
    fn 未闭合的标签不丢内容() {
        assert_eq!(plain("正文<div"), vec!["正文<div"]);
    }

    /// 拿**真实的牛津 MDX 正文**跑一遍。这段是从 Oxford Dictionary of English 3/e
    /// 的记录块里原样解出来的，class 名全是哈希串，样式在外部 ODE.css 里。
    #[test]
    fn 真实牛津词条转得出可读结构() {
        let html = r#"<link rel="stylesheet"href="ODE.css"type="text/css"><div class="Od3"><div class="k0i"><div class="h1s"><h2 class="z2h">@</h2></div><div><div><div class="k0z"><span class="nvt"><span class="xno">symbol</span></span><div class="se2"><div class="u2n"><div class="ysl"><a id="@__2"></a><span class="vkq">1</span><span class="aw5">At (used to indicate cost or rate per unit):</span> <span class="xxn"><em class="xv4">30 <a href="entry://dictionary#dictionary__2">dictionaries</a> @ £29.99 <a href="entry://each#each__6">each</a></em></span></div></div><div class="ewq"><div class="ysl"><a id="@__3"></a><span class="vkq">1.1</span> <i class="rnr">informal</i> <span class="aw5">At (in any sense):</span></div></div></div></div></div></div></div></div>"#;
        let b = to_blocks(html);
        let lines: Vec<String> = b.iter().map(|x| x.plain()).collect();

        // 词头、词性、义项号+释义、例句、次义项，各成一段——不再黏成一坨。
        assert!(lines.iter().any(|l| l.trim() == "@"), "词头没单独成段");
        assert!(lines.iter().any(|l| l.trim() == "symbol"), "词性没单独成段");
        assert!(
            lines.iter().any(|l| l.contains("At (used to indicate")),
            "释义丢了"
        );
        assert!(
            !lines.iter().any(|l| l.contains("symbol1At")),
            "词性与义项号黏在了一起——块级边界没生效"
        );

        // 例句靠斜体幸存下来：剥掉 CSS 后它是唯一的语义信号。
        let 斜 = b
            .iter()
            .flat_map(|x| &x.runs)
            .filter(|r| r.italic)
            .map(|r| r.text.as_str())
            .collect::<String>();
        assert!(斜.contains("30"), "例句没被标成斜体");
        assert!(斜.contains("informal"), "语体标注没被标成斜体");

        // 交叉引用可点，且指向词头而非带锚点的串。
        let 链: Vec<&str> = b
            .iter()
            .flat_map(|x| &x.runs)
            .filter_map(|r| r.link.as_deref())
            .collect();
        assert!(
            链.contains(&"dictionary") && 链.contains(&"each"),
            "跳转丢了：{链:?}"
        );

        // 外链样式表与空的锚点 `<a id=...>` 不该留下任何可见痕迹。
        assert!(!lines.iter().any(|l| l.contains("ODE.css")));
    }
}
