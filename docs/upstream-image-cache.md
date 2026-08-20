# windui 缺陷：GPU 位图缓存按指针地址索引，图被回收后会串图

> 2026-08-20 · 复现于 wind-ui-rust `main` @ `e05e5ba`（Windows / Direct2D 后端）
> 报告方：wind-dict

## 症状

界面上某个图片控件画出来的是**另一张**图——通常是上一帧刚被销毁的那张。

wind-dict 的实机表现：换肤（整树重建）之后回到词典页，标题栏的应用标识位置画出了
设置页那个返回箭头，而且是**旧皮肤的颜色**。截图见本仓库 `artifacts/`（若已清理，
按下面的复现步骤可重出）。

## 根因

`platform/win32/d2d.rs`：

```rust
/// 图片位图缓存：键 = `Image::cache_id()`（底层 `Rc<Pixmap>` 指针）
image_cache: HashMap<usize, ID2D1Bitmap1>,
```

`render/image.rs`：

```rust
pub(crate) fn cache_id(&self) -> usize {
    std::rc::Rc::as_ptr(&self.pixmap) as usize
}
```

**指针地址只在对象活着时唯一。** 一张 `Image` 被丢弃后，分配器完全可以把同一个地址
交给下一次分配——而 `image_cache` 里以那个地址为键的条目**还在**，于是新图查缓存
直接命中了旧图的 GPU 位图。

这不是「缓存没失效」那么轻：它的表现是**画错内容**，且错的那张往往语义上毫不相干，
排查时几乎不会有人想到是缓存。

`image_cache.len() > 64` 时整体 `clear()` 那条不解决问题——它只是让串图变得更随机。

## 复现

条件是「在同一帧内销毁一批 `Image` 又新建一批」，这在响应式重建里是常态：

1. 一个用 `host_signal` / `list_signal` 驱动的子树，里面有若干 `Element::image_svg`
   或 `image_content`（各自 `tint` 成不同颜色）。
2. 触发一次重建（信号变更），使旧的 `ImageContent` 全部析构、新的全部构造。
3. 观察若干轮之后，某个图标画出了另一个图标的图形（且是旧的着色）。

wind-dict 侧最稳的一条：设置页换肤 → 返回词典页。换肤会重建整棵树（我们必须这么做，
理由见下面「附」），一次性析构 + 新建全部图标，命中率很高。

## 建议的修法

按可靠性排序：

1. **给 `Image` 一个进程内唯一且不复用的 id**。构造时从一个单调递增的计数器取号，
   `cache_id()` 返回它。改动面小，且把「唯一性」从分配器行为变成了自己的不变量。
2. **缓存持弱引用并校验**：值里连同 `Weak<Pixmap>` 一起存，命中时 `upgrade()` 成功
   且与当前 `Rc` 同一对象才算命中。比 1 复杂，好处是能顺带做淘汰。
3. 最省事但只是缓解：`Image` 析构时从缓存里移除自己——需要 `Image` 拿到 canvas，
   跨层了，不推荐。

`shadow_cache` 用的是 `ShadowKey`（值语义），不受影响；`solid`/`grad_cache` 同理。
问题只在 `image_cache` 这一处。

## 顺带：`ImageContent::tint` 只收 `Color`，接不上换肤

不是缺陷，是能力缺口，但两件事在 wind-dict 这里是连着的。

`ImageContent::tint(Color)` 与 `Element::tint(Color)` 都只收具体颜色，而
`Button::paint` 画图标走 `icon.paint_into(...)`，用的是图标自带的 tint，**不套用
按钮的 `fg`**。于是图标无法像 `fg_role` / `fg_role_signal` 那样每帧跟随主题。

下游只能在**构建期**把 `Role` 解析成 `Color`，再靠「换肤时重建整棵树」来刷新——
而正是这次整树重建把上面那个串图缺陷放大成了必现。两件事因此是同一份代价。

**期望**：`ImageContent::tint_role(Role)` / `Element::tint_role(Role)`，paint 期按
当前主题解析。有了它，下游换肤就不必重建元素树了（ADR-0012 的结论也能恢复完整）。

## 附：wind-dict 侧当前的绕法

预先光栅化每个 `(资源, 尺寸, 颜色)` 并**永久持有** `Image`（`src/icon.rs`）：地址永不
回收，上游那条缓存于是恒为正确。代价是放弃了 `from_svg_bytes(bytes, None)` 的 DPI
感知重光栅（那条每次 paint 都产出新 `Image`，正好踩在缺陷上），改为按固定倍率出图。

上游修掉之后这层可以撤掉，DPI 感知那条路也能重新用起来。
