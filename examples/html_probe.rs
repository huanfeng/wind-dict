//! 把真实牛津 MDX 正文的转换结果打出来。断言证明性质，这个证明观感。
fn main() {
    let html = r#"<link rel="stylesheet"href="ODE.css"type="text/css"><div class="Od3"><div class="k0i"><div class="h1s"><h2 class="z2h">@</h2></div><div><div><div class="k0z"><span class="nvt"><span class="xno">symbol</span></span><div class="se2"><div class="u2n"><div class="ysl"><a id="@__2"></a><span class="vkq">1</span><span class="aw5">At (used to indicate cost or rate per unit):</span> <span class="xxn"><em class="xv4">30 <a href="entry://dictionary#dictionary__2">dictionaries</a> @ £29.99 <a href="entry://each#each__6">each</a></em></span></div></div><div class="ewq"><div class="ysl"><a id="@__3"></a><span class="vkq">1.1</span> <i class="rnr">informal</i> <span class="aw5">At (in any sense):</span> <span class="xxn"><em class="xv4">he dealt fairly well with tough issues thrown @ him</em></span></div></div></div></div></div></div></div></div>"#;
    println!("原文 {} 字节 →", html.len());
    for b in wind_dict::html::to_blocks(html) {
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
