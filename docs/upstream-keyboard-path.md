# windui 需求：键盘通路

给 wind-ui-rust 的需求。起因是 wind-dict 真机试用后的结论——**界面能用但不好用，根不在视觉，在于查词全程必须过鼠标**。

这不是词典特有的需求。任何"唤起 → 输入 → 从候选里选一个 → 确认"的界面（命令面板、快速打开、地址栏、搜索框）都是同一条通路，而 windui 当前一条也走不通。

## 现状：四步操作里两步是鼠标

wind-dict 的核心动作是查一个词：

| 步骤 | 有道词典 | wind-dict 现状 |
|---|---|---|
| 唤起 | 热键 | 热键 ✅ |
| 开始打字 | 直接打 | **鼠标点一下输入框** ❌ |
| 从候选里挑 | ↑↓ | **无** ❌ |
| 确认查询 | Enter | **鼠标点候选行** ❌ |

## 三条缺口

### 1. 窗口显示后无法把焦点给到某个控件

`WindowOp::Show`（`core.rs:1193`）只置可见性。配合 `start_hidden()`，进程起来时 `App.focus == None`，热键唤起后第一次按键无处可去。

应用层绕不过去：`ctx.request_focus()`（`core.rs:1059`）写的是 `self.out.focus = Some(self.self_id)`——**只能把焦点给自己**，且只在该控件自己的事件回调里可用。没有"让某个别的节点获得焦点"的表达方式，也没有声明式的初始焦点。

**期望**：以下任一即可
- `Element::autofocus()` —— 声明式，构建时标记，树建成后宿主把焦点给它
- `EventCtx::focus(node)` / 一个可从应用层持有的 `FocusHandle`
- `WindowOp::Show` 时若 `focus == None`，自动落到 `focusable_order()` 的首项

第三个最省事，但**不够**：wind-dict 唤起时希望焦点落在查询框并**全选已有内容**（下次直接覆盖打字，不用先删）。所以还需要一条"聚焦并全选"的语义，或者 `TextInput` 暴露 `select_all()`。

### 2. 单行输入的 Enter 没有出口

`inputs.rs:1703` 对 `Key::Enter` 加了 `if self.is_multiline()` 守卫，注释是：

> 多行：Enter 插入换行。单行不处理（**冒泡，留给默认行为**）。

**冒泡这件事不存在。** `dispatch_key`（`core.rs:1816-1832）的全部实现是：

```rust
pub fn dispatch_key(&mut self, ev: KeyEvent, focus: Option<NodeId>) -> DispatchResult {
    let mut res = DispatchResult::default();
    if let Some(f) = focus {
        let (consumed, o) = self.call_on_event(f, &Event::Key(ev));
        // …搬运 outcome 字段…
    }
    res
}
```

只有一次 `call_on_event`，没有 `ancestor_chain` 循环。对比同文件的 `dispatch_files`（`core.rs:1837`）——那个是真冒泡，`for id in self.ancestor_chain(target)` 写得明明白白。

所以"不消费"的实际后果不是"传给外层"，而是**事件消失**。唯一的例外是 Escape，宿主对它有硬编码兜底（`app.rs:2254`：`if !res.consumed && ev.key == Key::Escape && self.resolve_close()`）——这条兜底的存在恰好说明当时也发现了没有冒泡，但只给 Escape 打了补丁。

**这行注释比缺口本身更危险**：它描述的是作者以为的架构。下游读到它会去写"外层容器挂 Widget 接 Enter"的实现——编译通过、逻辑正确、永远不触发。

**期望**：以下任一
- `Element::on_submit(f)` —— 单行 `TextInput` 专属修饰符，Enter 时触发
- 让 `dispatch_key` 真冒泡（沿 `ancestor_chain`，首个消费者截止），并**修掉那行注释**

倾向前者：真冒泡会让所有容器都可能截获按键，是更大的语义变更，而 `on_submit` 是这个场景的直接表达。

### 3. 候选列表没有键盘游标

依赖 2。有了 Enter 之后还差：↑↓ 在候选间移动、当前项高亮、Enter 确认当前项。

wind-dict 的候选列表是 `Element::list_signal` + 自绘行，**选中态可以我们自己画**（用 `Signal<usize>` 存游标即可）。真正缺的只是 ↑↓ 这两个键能传到某处——即输入框聚焦时，↑↓ 不该被 `TextInput` 吞掉也不该消失。

`inputs.rs:1708-1712` 对 `Key::Up/Down` 同样加了 `if self.is_multiline()` 守卫，注释同样写着"单行不消费（冒泡）"。同一个问题。

**期望**：`on_submit` 若能扩展成一组"输入框未处理的导航键"回调最好，例如
```rust
Element::text_input(sig, "…")
    .on_submit(|ctx| …)
    .on_nav_key(|ctx, key| …)   // Up / Down / PageUp / PageDown / Tab
```
或者干脆开放冒泡，由应用层在祖先节点上接。

## 优先级

| # | 缺口 | 没有它的后果 |
|---|---|---|
| 1 | 初始焦点 | **热键唤起后打不了字**——常驻工具最致命的一条 |
| 2 | Enter 提交 | 确认查询只能鼠标点 |
| 3 | ↑↓ 游标 | 候选只能鼠标点 |

1 独立，可单独做。2 是 3 的前提。

## 附：wind-dict 自己能做的部分

不占用上游工作量，此处只作说明，避免重复投入：候选浮层化（`Element::stack`）、候选行去掉 `nav_row` 的 chevron、输入框清除按钮、查询前进/后退、唤起时的查询词策略——这些都在应用层做，已在推进。

## 2026-08-19 结案：三条全部落地并已接通

上游一轮做完，本文的三条需求**全部解决**，wind-dict 侧也已接上（`80b9b26`）。
完整回复见 [upstream-windui-reply.md](upstream-windui-reply.md)。

| # | 缺口 | 上游给的 |
|---|---|---|
| 1 | 初始焦点 | `Element::autofocus()` / `autofocus_select_all()` |
| 2 | Enter 提交 | `Element::on_submit()` |
| 3 | ↑↓ 游标 | `Element::on_nav_key()`，**含 Tab** |

**本文的结论当时全部正确，只有行号已作废**（上游那五个提交动过 `app/mod.rs` 与
`inputs.rs`）。尤其「冒泡这件事不存在」那段——上游确认从 0.8 时代到 0.13 一直成立，
错的自始至终是 windui 自己那两行注释，现已改掉并加了一条测试把「按键分发不冒泡」这个
事实钉住，注释若再退回去会当场变红。

三处与本文提法不同，接线时按上游的来：

- **`on_nav_key` 收 `KeyEvent` 而非 `Key`，且返回 `bool`**。Tab 从「抢在分发之前」改成
  「分发之后兜底」才做得成，宿主的焦点导航只在回调返回 `false` 时发生。因此
  **Shift+Tab 必须放过**——无脑 `return true` 会把它一起吞掉，那时用户除了鼠标没有
  任何办法把焦点移出查询框。
- **`autofocus` 是一次性的**（语义同 HTML），不是每次唤起都重新聚焦。故「唤起即全选」
  只在进程起来后第一次唤起成立；第二次唤起若焦点从未离开过查询框，上次查的词还在、
  光标在末尾。要每次唤起都全选得有个 per-wake 钩子（`on_show`），上游说明确要就单独提。
- **PageUp/PageDown** 当时不在 `Key` 枚举里，上游随后在 `8219b58` 补上了。

下次提需求给**符号名**而不是行号——行号在对方改完自己的代码后必然作废。
