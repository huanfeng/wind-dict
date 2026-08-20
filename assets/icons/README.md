# 图标来源与许可

`star.svg` / `x.svg` / `arrow-left.svg` 取自 [Lucide](https://lucide.dev)
（`lucide-icons/lucide`，`main` 分支的 `icons/` 目录）。

- **Lucide** — ISC License，Copyright (c) 2026 Lucide Icons and Contributors
- `x` 与 `arrow-left` 派生自 [Feather](https://feathericons.com) —
  MIT License，Copyright (c) 2013-present Cole Bemis

`star-filled.svg` 是本项目从 `star.svg` 改的：**同一条路径**，`fill` 由 `none` 改为实心。
用同一条路径而非另找一个实心星图标，是为了让收藏 / 未收藏两态的星形轮廓严丝合缝——
换一个图标的话，点一下收藏星星的形状会跳一下。实心变体去掉了 `stroke`：Lucide 的
描边是 2px + `stroke-linejoin="round"`，填充之后那圈圆角描边会把五个角磨钝，实心星
看起来发面。空心星保留描边（那本来就是它的全部）。

## 就地改了什么

1. `stroke="currentColor"` → `stroke="#000000"`。resvg 会把 `currentColor` 解析成黑色，
   结果一样，但写死是自证的：颜色**不由 SVG 决定**，运行期由 `icon::tinted` 按主题角色
   重新着色（`Image::tinted` 只换 RGB、保留 alpha 覆盖度，故源色是什么都不影响结果）。
2. 压掉了缩进换行。纯粹是为了 `include_bytes!` 进二进制时少几十字节。

## 为什么不用字形

`★`（U+2605）这类符号在 Windows 上会被 Segoe UI Emoji / Segoe UI Symbol 接管，
渲染出来带彩色描边，且字面框与 UI 字体不一致——在按钮里既不居中、放大后还发虚。
本项目此前正是这么画的，实机上一眼可见。

## 更新方式

```
curl -sS -o star.svg https://raw.githubusercontent.com/lucide-icons/lucide/main/icons/star.svg
```
取回后按上面两条就地改一遍，并重新生成 `star-filled.svg`。
