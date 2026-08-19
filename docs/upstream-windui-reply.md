# windui 上游回复：六节九条需求的处理结果

> 2026-08-19 · 上游 wind-ui-rust `main` @ `3ba504c` · 配套 [upstream-keyboard-path.md](upstream-keyboard-path.md)

针对 wind-dict 在 windui 0.13.0 上提出的六节九条（P0 键盘通路 3 条、P1 截图盲区 3 条、
P2 可测性与表达力 3 条）。**七条已落地并推上 main**，一条判定为非缺陷（附复现证据），
一条拆成独立项。

wind-dict 走 `windui = { path = "../wind-ui-rust" }`，拉一下就能用，无需等发版。

先说结论里最需要你们改预期的三条，再说落地内容——**你们提的方案有两处我没照做**，
理由在下面，请先看完再动手。

| 条目 | 结果 |
|---|---|
| P0-1 焦点归属 | ✅ `autofocus()` / `autofocus_select_all()`（三条备选只做了第一条，见下） |
| P0-2 Enter 出口 | ✅ `on_submit()`，两处失实注释已改并加测试锁死 |
| P0-3 候选游标 | ✅ `on_nav_key()`，**含 Tab**，但签名与你们提的不同 |
| P1-1 合成键盘 | ✅ `--type` / `--key` |
| P1-2 指定尺寸 | ✅ `--size W H` |
| P1-3 连点丢一次 | ❌ **非缺陷**，已复现，根因不是丢帧 |
| P2-1 Tray/Hotkey 可测 | ✅ 顺带把整个托盘 API 收成了平台无关声明层 |
| P2-2 句柄可测 | ✅ `ThemeHandle::detached` / `HotkeyHandle::detached` |
| P2-3 按状态着色 | ✅ `fg_role_signal`；`Signal::map` 拆为独立项 |

对应提交（`main`，`d63a262..3ba504c`）：

```
dcd40be feat(app): 运行期句柄的测试构造口 ThemeHandle/HotkeyHandle::detached
1fe21e6 feat(ui): 键盘通路——autofocus / on_submit / on_nav_key，Tab 改为宿主兜底
c832ea8 refactor(tray): 托盘收口为平台无关声明层，回调随之可测
1a1feca feat(app): 截图路径合成键盘 --type/--key 与覆盖尺寸 --size
3ba504c feat(ui): 按状态换色 Element::fg_role_signal(Signal<Role>)
```

---

## 一、三处与你们的提法不同

### 1. Tab 能做，但不是你们设想的方式；`on_nav_key` 的签名因此变了

你们把 Tab 列进 `on_nav_key` 的键集。我第一轮回复说"实现不了"——**那个判断是错的**，
方向找偏了。

问题不在"Tab 归宿主管"，而在它此前是**唯一抢在 `dispatch_key` 之前**处理的键。改成
**兜底**即可：键先给焦点控件，只有控件不消费才轮到焦点导航。这有两个现成对照——同一个
函数里的 Escape 关窗本来就是这个形状；网页里 input `preventDefault()` 掉 Tab 也是同一个
机制。改完 512 个既有测试原样通过（当前没有任何内置控件消费 Tab），所以对你们其余界面
零影响。

代价是签名和你们提的不一样：

```rust
// 你们提的
.on_nav_key(|ctx, key| …)
// 实际是
.on_nav_key(|ctx, ev: KeyEvent| -> bool { … })
```

两处改动都是 Tab 逼出来的：

- **返回 bool**：宿主的焦点导航只在返回 `false` 时才发生。不能替应用决定 Tab 归谁。
- **收 `KeyEvent` 而非 `Key`**：要读 `shift`。**这条请务必照做**——如果你们无脑
  `return true`，会把 Shift+Tab 一起吞掉，那时用户除了鼠标**没有任何办法把焦点移出
  查询框**。惯例是只吞 Tab、放过 Shift+Tab。

`examples/palette.rs` 里演示的是 shell 补全语义（Tab 把当前候选填进输入框），可以直接抄。

**送到的键**：`Up` / `Down`（仅单行）与 `Tab`（单行多行都送）。PageUp/PageDown 见第三节。

### 2. P1-3 不是 bug——已复现，根因不是你们猜的那个

你们要求"先复现，再决定是不是问题"。复现了，结论是**回放逻辑没有问题，不需要改**。

先否掉猜测：`run_offscreen` 每次 `--click` 之后**都**调 `off.frame()`
（`src/platform/mod.rs`），而 `on_pointer` 结尾无条件置 `needs_relayout = true`，该帧必定走
`relayout_if_needed → layout_root → dispatch_reactive_updates`，`host_signal` 的重建与新
布局**在同一帧内完成**。所以"重建后的树需要一帧 layout 而回放不给这一帧"不成立。

我造了两个复现（临时测试，验证后已移除）：

- **布局不变的重建**：点 A → bump 信号整段重建 → 连点 B 两次，**三次全部命中**。
- **布局位移的重建**（重建后多出一条回执消息行，复刻你们 324→369 那个位移）：
  - 用**重建后的真实坐标**点 B → **命中**。
  - 用**旧坐标**点 B → 不命中，但那次点击**被 A 吃掉了**——A 下移 40px 后正好坐在 B
    原来的位置上。

所以现象是真的，但根因是「重建改变了布局，旧坐标现在指向另一个节点」，不是落在弃树上。
`625 369` 那次大概率是位移量算错了，或者消息条的出现还带了别的高度变化。

**真正缺的是可观测性**：`--click` 落在哪个节点上目前无从得知，所以一次不命中会被误读成
丢帧。这条我没有顺手加（不在你们列的九条里），如果确实需要，提一句我加个把命中节点打进
日志的开关。

### 3. `Signal::map` 拆成独立项，先给了 `fg_role_signal`

你们的 P2-3 给了三个备选，我做了第一个（最直接的那个）。`Signal::map` 没做，理由不是
偷懒：windui 的 `Signal` 是**拉取式**的（`get()` 读当前值、`version()` 做变更检测），
没有依赖图。派生信号要么每次读时重算（那 `version()` 怎么合成？下游拿它做变更检测会失
准），要么建依赖图（与现有架构不符）。这是个需要单独想清楚的设计题，绑在这次一起做会
草率。

**一个好消息**：色板本来就是齐的。你们提到 `RichColor` 缺 Success/Warning——对，但
`Role` **已经有** `Role::Success` 和 `Role::Warning`，同源 `palette.success` /
`palette.warning`。所以缺的只是信号版入口，补上就完了。

---

## 二、三处我没做，明说

### 1. 「每次唤起都重新聚焦并全选」没做

`autofocus` 是**一次性**的（语义同 HTML 的 `autofocus`）：节点首次进入焦点环那一帧兑现，
此后焦点归用户，不会每帧粘回去。这条是刻意的——否则"点空白清焦点"会在下一帧被撤销，
与已修好的焦点语义直接冲突。

对你们的实际影响：**进程起来后第一次唤起是好的**（`start_hidden()` 时首帧布局大概率就
发生在第一次 Show 附近，焦点与全选都到位）。但**第二次唤起**时，如果焦点从未离开过查询
框，不会重新触发全选——上次查的词还在，光标在末尾。

真正需要的是一个 per-wake 钩子（`on_show`），而 `WindowOp::Show` 目前纯在平台层落地、
宿主收不到通知，两个平台都要加消费点。那是独立一项，不该塞进这次。**如果这条对你们是
硬需求，单独提，我按一项来做。**

### 2. PageUp/PageDown 送不到 `on_nav_key`

它们**根本不在 `Key` 枚举里**，加进去要动两个平台的键码映射（win32 的 VK 表 + macOS 的
物理键位表，后者是"物理键位编号、与字符值无关"那套，`VK_A == b'A'` 的巧合在那边不成立）。
`on_nav_key` 的签名收整个 `KeyEvent`，正是为了这些键日后加进来时**不必改你们的调用方**。

### 3. `fg_role_signal` 有一处残留约束

换色靠"生效角色进布局签名 → 重排后签名不等 → 升整窗"生效。而重排只在宿主置了
`needs_relayout` 的帧发生，指针 **`Move` 刻意不置**（hover 高频）。

所以：在自定义 `Widget` 的 Move 分支里改**别的节点**的颜色信号不会自动升整窗，需自行
`ctx.mark_dirty()`。按键、点击、菜单、`App::channel` 这些路径都置了，无需额外处理——
你们的用法（设置页保存后改回执颜色）落在安全区内。

---

## 三、落地内容与用法

### P0 键盘通路

```rust
Element::text_input(query, "查词…")
    .leading_icon('\u{1F50D}')
    .autofocus_select_all()            // 焦点有归属，且全选旧内容供覆盖打字
    .on_submit(move |ctx| confirm(ctx))
    .on_nav_key(move |_ctx, ev| match ev.key {
        Key::Down => { cursor.set(cursor.get() + 1); true }
        Key::Up   => { cursor.set(cursor.get().saturating_sub(1)); true }
        Key::Tab if !ev.shift => { accept_completion(); true }   // Shift+Tab 放过！
        _ => false,
    })
```

- `autofocus()` 只聚焦；`autofocus_select_all()` 额外全选（走合成 Ctrl+A 回送控件，
  与右键菜单的复制/粘贴同一条通路）。
- 兑现与 `focusable_order` 取交集，故对话框弹着的那一帧不会把焦点送到遮罩后面。
- 程序性移交**不点亮焦点环**（判据是用户最近一次交互用的什么设备），输入框照常显示光标条。
- **多行输入不触发** `on_submit` 与 ↑↓ 的 `on_nav_key`——那里 Enter 换行、上下移行，
  编辑器的固有语义不该被应用截走。

**`inputs.rs:1730/1735` 那两行注释已改**，并加了一条
`unconsumed_key_does_not_bubble_to_outer_container` 把"按键分发不冒泡"这个事实钉住——
注释若再退回去会当场变红。

顺带说一句：`upstream-keyboard-path.md` 里那段判断（"冒泡这件事不存在"、Escape 兜底的
存在恰好说明当时也发现了没有冒泡）**完全正确**，从 0.8 时代到现在一直成立。错的一直是
windui 自己那两行注释。那份文档除了行号，结论不需要改；现在可以标成已解决。

完整通路示例：`examples/palette.rs`（唤起 → 打字 → ↑↓ 选候选 → Tab 补全 → Enter 确认）。

### P1 截图

```bash
--type <text>   # 逐字符输入，不经平台键码映射，中文可直接打
--key <name>    # Enter/Escape/Tab/Up/Down/Left/Right/Home/End/Backspace/Delete/Space
                # 或单个字符，可加 ctrl+ / shift+ 前缀（--key ctrl+a 全选）
--size W H      # 覆盖窗口尺寸，并把 min_size 的下限一并放开
```

- `--type` / `--key` 与 `--click` 一样可重复，且**按写的顺序混合回放**
  （`--type ab --key Enter --type c` 严格按此序）。
- 走 `handler.on_key`，与真实按键**同一条通路**——焦点裁决、Tab 兜底、控件回调都照常。
- `--size` 把 `min_size` 一并放开是刻意的：你们要测的正是下限处的表现，还受钳制就永远
  截不到那张图。
- `--key` 认不出会打一行提示并跳过，**不猜**——猜错的症状是"截图跟没按一样"，无声无息。

你们 DESIGN.md 那条「原生 UI 改动前，在默认 920x620 和最小 720x480 下为所有皮肤截图」
现在做得到了。补全浮层这类"输入中"状态也终于能进视觉回归：

```bash
cargo run --release --example <你们的示例> -- --screenshot out.png \
    --size 720 480 --type "查" --key Down --key Down
```

### P2 可测性

```rust
// 托盘 / 热键回调（TrayCtx 现在是平台无关类型，两平台同一套语义）
use windui::platform::TrayAction;
assert_eq!(
    windui::testing::run_with_tray_ctx(TrayMenuItem::item("退出", |ctx| ctx.quit())),
    vec![TrayAction::Quit],
);
let acts = windui::testing::run_with_tray_ctx_fn(|ctx| { ctx.notify("已同步", "3 条"); ctx.show_window(); });
assert_eq!(windui::testing::run_with_hotkey_ctx(|ctx| ctx.show_window()), Some(WindowOp::Show));

// 持有运行期句柄的应用状态结构现在造得出来了
struct Settings { theme: ThemeHandle, hotkey: HotkeyHandle, /* … */ }
let s = Settings {
    theme: ThemeHandle::detached(Theme::default()),
    hotkey: HotkeyHandle::detached(),
};
assert_eq!(s.hotkey.pending_ops(), vec![HotkeyOp::Rebind(want)]);
```

> 你们 `808141f` 的提交信息里写得很准："State 持有 ThemeHandle 与 HotkeyHandle，两者在
> windui 里只能从 App 拿、构造面是私有的，下游的测试造不出 State；收信号与 &str 的自由
> 函数则随手可建。" 现在 `State` 本身造得出来了，`write_note` / `note_line_visible` 这类
> 纯为可测而抽的自由函数可以按需收回方法里。
> `pending_ops()` 只读不取走，对真句柄调用也安全（不会把宿主该执行的改绑偷掉）。

**关于 P2-1 的一点补充**：你们说"两个平台各有一份 `TrayCtx`，形状还不一样"——实际情况
比这更大。整个 `Tray` / `TrayMenuItem` **构建器**都是两份完整副本（各约 150 行雷同代码），
`platform/mod.rs` 按 `cfg` 各自 re-export。所以你们此前的跨平台性是**巧合**：两边方法名
恰好一样，类型其实是两个，语义还不同（win32 累积意图、macOS 立即调 OS）。现在收成了
一个声明层，两个后端只保留执行半边。

### P2-3 按状态换色

```rust
let tone = signal(Role::TextMuted);
Element::label_signal(msg).fg_role_signal(tone)
// 保存成功的回调里：tone.set(Role::Success);  失败：tone.set(Role::Danger);
```

优先级 `fg_role_signal` > `fg_role` > `fg`（`.fg(c)` 会清掉信号版）；信号失效时回落而
不是 panic。

> `808141f` 那个变通现在可以收掉。按你们自己的提交信息，它的代价有三层：两个绑同一份
> 文本的 label 叠放、外层 col 要带同一个可见性判定（否则空消息时顶部凭空多出 28px
> spacing）、外加 `note_line_visible` 这个只为可测而抽出来的自由函数。换成
> `fg_role_signal` 之后这三层都不需要了——一个 `Element::label_signal(msg).fg_role_signal(tone)`
> 即可，`Role::Danger` / `Success` / `Warning` 直接当语气用。
>
> 那条"任一时刻至多一行可见"的测试也随之作废——两行叠放这个结构本身没有了，写反导致
> "两行文字重叠着画"或"消息静默丢失"的失败模式一并消失。

---

## 四、行号

你们给的行号我逐条核实过，**当时全部准确**——省了不少时间，谢谢。但这五个提交之后
已全部作废（`app/mod.rs` 的 `on_key` 挪了、`inputs.rs` 的按键分支加了两条、
`platform/win32/tray.rs` 从 545 行降到 327 行）。

下次提需求建议直接给**符号名**而不是行号，双方都省事：

```
EventCtx::request_focus        （原 core.rs:1349）
Tree::dispatch_key             （原 core.rs:2363）
App::screenshot_from_args      （原 app/mod.rs:500）
Element::fg_role               （原 ui/mod.rs:3136）
```

---

## 五、下一步

我这边不打算主动推进的（等你们提）：

1. **`on_show` 钩子** —— 解掉"每次唤起重新聚焦并全选"。要不要做取决于第二次唤起的手感
   对你们有多重要。
2. **`Signal::map` / 派生信号** —— 需要先定"拉取式 Signal 的 version 怎么合成"。
3. **PageUp/PageDown 进 `Key` 枚举** —— 两个平台的键码映射，做起来不难但要两边都验。
4. **`--click` 打印命中节点** —— P1-3 那类误判的真正解药。

破坏性变更照你们说的走 `#[deprecated]`。这轮唯一的签名变更是 `on_nav_key`，但它本身就是
这轮新增、尚未随任何版本发布，所以直接定了签名、没留弃用别名。**如果要调整它，现在是
唯一的窗口期**——发版之后再改就得走弃用流程了。
