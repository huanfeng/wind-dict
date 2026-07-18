# 缺失的后台工具能力在 windui 上游实现，不在本项目内绕过

wind-dict 需要五项 windui 当前不具备的能力：全局热键、窗口失焦事件、`EventCtx` 上的窗口显隐、关闭拦截时转为隐藏、启动即隐藏。这些能力**一项都不是词典特有的**——任何热键唤起类常驻工具（启动器、剪贴板管理器、截图工具）都需要它们。因此它们在 windui 中正统实现，而非在 wind-dict 里用 Win32 hack 绕过。

## 已验证的缺口

| 能力 | 证据 | MVP 阻塞 |
|------|------|----------|
| 全局热键 | `RegisterHotKey` 全仓库零命中 | **是** |
| 普通回调中显隐窗口 | `show_window`/`hide_window` 仅存在于 `platform/*/tray.rs`；`EventCtx`（`core.rs:968`）无此方法 | **是** |
| 关闭拦截转隐藏 | `on_close_request(f: FnMut() -> bool)` 签名不带 ctx，拦截后无法隐藏 | **是** |
| 启动即隐藏 | `platform/win32/mod.rs:612` 无条件 `ShowWindow(hwnd, SW_SHOW)`，无配置项 | 是（体验瑕疵） |
| 窗口失焦事件 | `WM_ACTIVATE` / `WM_KILLFOCUS` 零命中；`AppHandler` trait 无对应方法 | **否** |

> 窗口失焦事件不阻塞 MVP：窗口消失策略已定为「ESC 隐藏、失焦不隐藏」（见 ADR-0007），该能力仅在未来实现划词浮窗时才需要。列在此处是因为它同属「后台工具」这一能力缺口，一并设计更合理。

## 被拒绝的方案

**在 wind-dict 内用 Win32 绕过**：独立线程跑私有消息循环注册热键（`WM_HOTKEY` 投递到线程队列，不必进 windui 的循环），并用 `FindWindowW` 按类名抓 HWND 后直接 `ShowWindow` + `SetForegroundWindow`。此路技术上大概率可行，且更快见到成品。拒绝的理由是：这些能力对 windui 是**真实的缺失拼图**而非本项目的特殊需求，把它们沉淀在应用层意味着下一个工具还要再 hack 一遍；而「先 hack 再回流」的承诺在实践中极易被无限推迟。

## 后果

- **wind-dict 的进度被 windui 的开发阻塞。** 这是本决定最主要的代价，且必须正视：词典逻辑本身反而不是关键路径。
- 新增 API 必须同时提供 **macOS 实现**，否则 windui README 承诺的「一份代码，两个平台」即被打破。尽管 wind-dict 当前仅 Windows（见 ADR-0005 平台范围），windui 作为通用库不能只补一半。macOS 全局热键需 Carbon `RegisterEventHotKey` 或 `CGEventTap`（后者需用户授予辅助功能权限），工作量显著大于 Windows 侧。

  > **2026-07 偏离记录**：上述 macOS 义务**暂未履行**。实现时决定先只做 Windows，macOS 侧以编译期报错占位。
  >
  > 理由：开发环境为 Windows，macOS 代码**无法编译、无法运行、无法验证**。而 windui 的 `AGENTS.md` §5 明令「只能真机验证的特性…别声称『已验证』」。交付一份看着像对的、实则从未编译过的 Carbon 代码，比明确的 `compile_error!` 更危险——前者会让人以为该平台可用。
  >
  > 代价：windui 的双平台承诺在此能力上暂时中断，须在 Mac 环境补齐后方可发布。这是**已知的技术债，不是遗漏**。
- windui 需要新增一类概念：**没有可见窗口也能存活的应用**。这可能触及事件循环与生命周期的既有假设（如「窗口销毁即退出」），影响面需在动手前评估。
