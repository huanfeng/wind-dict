# windui 缺陷：拖动区判定不沿父链冒泡，自绘标题栏上的 `clickable()` 容器点不动

> 2026-08-20 · 复现于 wind-ui-rust `main` @ `e05e5ba`（Windows / 无边框窗口）
> 报告方：wind-dict
>
> **已结案** · 上游 `7b6ab36` 当日修掉，wind-dict 侧的绕法已撤。见文末「结案」。

## 症状

无边框窗口的自绘标题栏上放一个 `Element::row().clickable().child(Element::label(…))`
文字入口，**文字上按不下去**——只有文字上下的空隙那几个像素能点。用户的原话是
「只有按键最下面的一点点可以点击生效」。

wind-dict 的实机表现：标题栏右侧「历史」「收藏」「设置」三个入口，指针放在字上按下
不触发 `on_click`，而是开始拖窗。

## 根因

`core.rs` 的 `Tree::drag_hit_at`：

```rust
pub fn drag_hit_at(&self, pos: Point) -> bool {
    let Some(hit) = self.hit_test_for_drag(pos) else { return false };
    if self.get(hit).map(|n| n.widget.focusable()).unwrap_or(false) {
        return false;            // ← 只看命中落定的那个节点自身
    }
    self.ancestor_chain(hit).iter()
        .any(|&id| self.get(id).map(|n| n.window_drag).unwrap_or(false))
}
```

「是不是交互控件」只问**命中落定的那个节点**，而「是不是拖动区」却**沿父链**找。
两侧不对称，于是「容器可点、里面还套着一个子控件」这个组合必然失守：

- `Clickable::focusable()` 是 `true`（`ui/containers.rs:423`）；
- `Label` 不覆写，走 `Widget::focusable()` 的默认值 **`false`**；
- `Label` 是真实控件，`hit_opaque()` 默认 `true`——**命中在它这里就落定了**，
  轮不到外层的 `Clickable`。

于是指针落在文字上时，`drag_hit_at` 看到的是「不可聚焦 + 祖先有 `window_drag`」，
`WM_NCHITTEST` 答 `HTCAPTION`，系统接管这次按下，客户区连 `WM_LBUTTONDOWN` 都收不到。
只有指针落在 `Clickable` **裸露的部分**（label 之外的 padding 与上下空隙）才判为
交互控件。

`interactive_hit_at` 是同一条判据，故同样只认落定节点。

现有测试 `window_drag_hits_caption_not_button` 盖不住这条：它用的是
`Element::window_button`，一个**没有子节点**的叶子控件，命中永远落在它自己身上。

## 复现

`src/ui/mod.rs` 的测试模块里加（`layout` 是该模块现成的辅助函数）：

```rust
#[test]
fn clickable_container_in_drag_region_is_not_caption() {
    // 窗口根照抄自绘标题栏的常见结构：col(标题栏 38 高, 主体 fill)。
    // NullTextEngine 测不出文字尺寸，故给 label 显式尺寸模拟真实测量
    // （12.5px 中文两字 ≈ 25×17）。
    let tree = layout(
        Element::col()
            .fill()
            .child(
                Element::row()
                    .width_match()
                    .height(38)
                    .cross(Align::Stretch)
                    .window_drag()
                    .child(Element::label("wind-dict").width(120).height(38))
                    .child(
                        Element::row()
                            .cross(Align::Center)
                            .padding_xy(11, 0)
                            .height_match()
                            .clickable()
                            .on_click(|_| {})
                            .child(Element::label("历史").width(25).height(17)),
                    ),
            )
            .child(Element::col().fill()),
    );
    // 入口的文字区（label 落在 y 10..27）：应交给 Clickable，而不是拖窗。
    assert!(!tree.drag_hit_at(Point::new(135, 19)), "clickable 容器内的文字不该判为拖动区");
    assert!(tree.interactive_hit_at(Point::new(135, 19)), "该判为交互控件（HTCLIENT）");
    // 品牌文字区仍应可拖。
    assert!(tree.drag_hit_at(Point::new(60, 19)));
}
```

未修时两条断言都失败。实测打印（同一结构，x=135 竖着扫）：

```
y= 2  hit=Clickable  focusable=true   drag=false  interactive=true
y=10  hit=Label      focusable=false  drag=true   interactive=false   ← 点不动
y=19  hit=Label      focusable=false  drag=true   interactive=false   ← 点不动
y=24  hit=Label      focusable=false  drag=true   interactive=false   ← 点不动
y=28  hit=Clickable  focusable=true   drag=false  interactive=true
y=36  hit=Clickable  focusable=true   drag=false  interactive=true
```

再叠上顶部 8px 的窗口缩放边框（`RESIZE_BORDER_LOGICAL`，`interactive` 为假时优先），
可点的就只剩底部那一条——与用户看到的完全一致。

## 建议的修法

**`drag_hit_at` 与 `interactive_hit_at` 都改为沿父链找最近的可交互节点**，而不是只看
落定节点：

```rust
let interactive = self.ancestor_chain(hit).iter()
    .any(|&id| self.get(id).map(|n| n.widget.focusable()).unwrap_or(false));
```

沿父链找到 `window_drag` 之前先遇到可聚焦节点 → 交控件；反之才是拖动区。

这与框架自己在别处的语义是一致的：`on_drop` 与 `context_menu` 都明说「落点命中本节点
**或其子节点**时沿父链冒泡到首个设了回调的节点触发」。指针交互本来就是冒泡的——
`Label` 的 `on_event` 返回 `false`，事件照样冒到 `Clickable` 被消费；只有 `NCHITTEST`
这一侧没跟上，才出现「事件分发认得这次点击，可它压根到不了客户区」。

不必区分 `focusable` 与「有 `on_click`」：`Clickable`、`Button`、输入框等真正想收指针
的控件，`focusable()` 都已经是 `true`。

顺带能修好第二个小毛病：`interactive_at` 一旦沿父链认出交互控件，`handle_nchittest`
第一步就答 `HTCLIENT`，标题栏顶部那 8px 缩放边框也不会再从按钮上咬掉一条——目前只有
`window_button` 这类叶子控件享受得到这个待遇。

## wind-dict 侧的绕法（已撤）

把 `window_drag()` 从整条标题栏挪到品牌块上（`src/ui.rs` 的 `brand`），入口按钮因此
不在 `window_drag` 子树内，父链里找不到拖动区标记，判定自然返回假。品牌块带
`weight(1.0)`，右侧入口之外的整片空白仍归它，拖窗与双击最大化的手感不变。

代价是这条约束**看不见**：日后谁给标题栏行加回 `.window_drag()`，或者把入口挪进任何
一个带该标记的容器，按钮就又点不动了，而且症状（「只有最下面能点」）与原因隔得很远。

## 结案

上游 `7b6ab36`（2026-08-20 同日）把两侧判定合并成一次父链遍历 `Tree::hit_role`，
自内向外**先遇到谁听谁**：可聚焦判交互、`window_drag` 判拖动区。

取「最近的裁决者」而不是上面建议的「链上有没有可聚焦节点」——上游这处改得比提法更准：
`any()` 会把反向嵌套判反（可聚焦容器里再嵌一条拖动区时，内层的声明更具体，该赢）。
提需求时没想到这种嵌套。

wind-dict 侧照此把绕法撤了：`window_drag()` 回到整条标题栏，`brand` 不再背这个标记。
两条约束因此都消失——标题栏怎么摆都行。

**顺带兑现的**：`interactive_hit_at` 也沿父链之后，可点容器整个子树优先判 `HTCLIENT`，
按钮顶部那 8px 不再被缩放带咬掉一条。它的对称代价落在了 wind-dict 身上且是真实的：
三个文字入口贴着窗口顶边，那 141px 宽的一段从此让不出缩放带，从标题栏右侧那截顶边
往上拖拉不动窗口高度。窗口按钮那 138px 一直如此（它本就是可聚焦控件），左侧品牌区与
其余三条边不受影响，故照单接下，记在 `ui::title_bar` 的注释里。真要留缩放带得让容器
躲开边缘（上游给的参照是 `scrollbar::WINDOW_EDGE_INSET`），不值得为它把入口往下挪。
