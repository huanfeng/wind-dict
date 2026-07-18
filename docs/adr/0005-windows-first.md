# 仅优先支持 Windows，尽管 windui 是跨平台库

本项目建立在跨平台 GUI 库 windui（Windows + macOS）之上，却只优先交付 Windows。这个反差需要解释，否则会被误读为疏漏。

原因是**全局热键**——本项目唯一的入口。Windows 侧是 `RegisterHotKey`，几十行 Win32，无需任何用户授权。macOS 侧则是完全不同量级的问题：Carbon `RegisterEventHotKey` 或 `CGEventTap`，后者要求用户手动前往「系统设置 → 隐私与安全性 → 辅助功能」授权，随之而来的是引导 UI、权限被撤销后的降级路径、公证签名等一整套工程与产品问题。

## 后果

- 控件树、布局、查询逻辑、存储全部平台无关，天然可移植。**日后补 macOS 只需填「热键」与「窗口显隐」这两条平台缝**——这正是选择 windui 的收益兑现之处。
- 本决定**不豁免 windui**：按 ADR-0004，加进 windui 的新 API 仍须同时提供 macOS 实现。「仅 Windows」是 wind-dict 的交付范围，不是 windui 的实现范围。
