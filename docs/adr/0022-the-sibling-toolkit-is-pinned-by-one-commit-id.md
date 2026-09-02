# 兄弟仓库 windui 靠一个提交号钉住，而不是靠运气

`Cargo.toml` 里 windui 是 **path 依赖**（`path = "../wind-ui-rust"`），两个仓库各自
独立提交、独立推送。仓库根的 **`windui.ref`** 记着 CI 该检出哪一个 windui 提交。

## 起因：CI 的成败取决于两次 push 的先后

第一次 CI 是红的，挂在找不到 `SelectionScope`、`TabItem::enabled`、
`platform::TrayHandle`——全是当时还躺在本机、没推上去的 windui 代码。

这不是「忘了推」这一次的疏忽。工作流是先抓 windui 默认分支的 HEAD 再编，于是
「先推 wind-dict、后推 windui」必红，而那个先后不在任何人的检查清单上。更麻烦的是
本机那份 `../wind-ui-rust` 是一个**可变的工作副本**：同一个 wind-dict 提交，隔壁改
一行就编出另一个程序，而以前没有任何地方记下这次构建用的是哪一版。

`windui.ref` 就是那个「哪一版」。CI 与 release 都按它检出；`release.ps1` 把它与实际
参与构建的那份 windui 核对，对不上即拒绝打包——包内 `README.txt` 上那行
「提交 xxx · wind-ui-rust yyy」是排障唯一的锚点，它必须**确有其事**。

## 为什么不改成 git 依赖

`windui = { git = "...", rev = "..." }` 能让 Cargo.lock 原生完成钉住，看起来更正统。
不这么做，是因为**两个仓库是一起改的**：改 windui 的一个 API、立刻回 wind-dict 编一次，
这是日常。换成 git 依赖，每改一行 windui 都要 push 一次、再更新一次 lock，一个几秒的
循环变成几分钟。

Cargo 的 `[patch]` 可以把 git 依赖就地换回本地路径，那是「正统 + 不牺牲开发循环」的
组合，但它要求每个开发者各自配一份覆盖，而覆盖没配对时的表现是**静默地编了另一份代码**
——正是这条 ADR 要消灭的那类事。一个仓库里人人都看得见的文本文件，比一份人人各配一份
的本地覆盖更难出错。

## 为什么不用 git submodule

submodule 的 gitlink 本身就是钉子，且随每次提交一起版本化，这点比文本文件强。

代价是目录结构：submodule 只能落在 `wind-dict/wind-ui-rust`，而 `Cargo.toml`、
`scripts/*.ps1`、以及「两个编辑器窗口并排开着两个仓库」的日常习惯，全都建立在**兄弟
目录**这个前提上。为一个只有 CI 会读的钉子去改动整个本地布局，不划算。

## 谁来维护它

| 时机 | 行为 | 为什么是这个强度 |
|---|---|---|
| `dev.ps1 pin` | 写入当前 windui HEAD；**拒绝写未推送的提交** | 写进去的表现是下次 CI 在 checkout 那一步就红，而错误信息（`could not find <sha>`）离真正的原因隔着一层 |
| `dev.ps1 ci` | 与本机 HEAD 不一致时**警告** | 本地同时改两个仓库是常态；拦下来只会逼人绕过检查 |
| `release.ps1` | 同一件事**硬失败** | 发布包必须能由两个确定的提交重现 |

仓库变量 `WINDUI_REF` 可临时覆盖（调 CI 用）。覆盖之后 `release.ps1` 的核对会失败——
那是对的：用一个临时覆盖打出来的包，说不清是对着哪版编的。

## 顺带钉住的其他几处

- **cache 键带上 windui 提交号**。`target\` 里躺着的是对着某一版 windui 编出来的产物，
  换了 windui 却复用它，轻则白编一遍，重则拿到一份说不清对着哪版编的增量结果。
  Cargo.lock 管不到这件事——path 依赖在 lock 里只有一个版本号，没有提交号。
- **cargo 加 `--locked`**。Cargo.lock 已入库就该由它说了算；少了这个开关，cargo 会为了
  让构建过去而**就地改写** lock，于是 CI 验的是一份仓库里并不存在的依赖组合。

## 发布前先跑一遍 CI

`release.yml` 用 `workflow_call` 复用 `ci.yml`，作为发布的前置门。

tag 未必打在被 CI 跑过的提交上：可以打在几天前的某个提交上，也可以打在一个只改了
版本号、还没推过的提交上。到那时才发现编不过，是这条流水线里最贵的一种失败——词库
已经下完、包已经打了一半。

复用时 `concurrency` 的组名里**必须带 `github.workflow`**：`workflow_call` 下被调用方
看到的 `github.ref` 仍是调用方那个，只按 ref 分组会让「推一次 main」与「在 main 上试跑
发布」落进同一组，`cancel-in-progress` 让它们互相掐掉。

## 试跑：走完全程但不建 Release

`release.yml` 由非 tag 触发（手动 `workflow_dispatch`）时，走完一模一样的
构建 → 组装 → 验证 → 压包，但不建 Release、不动任何 tag，产物作为构建物留 5 天。

这条路存在的理由：这条流水线上真正没把握的是打包与冒烟那几步——runner 上能不能建窗口、
能不能渲染——而它们不该等到第一次真发布时才第一次跑。

**发不发布由 `github.ref` 是不是 tag 决定，不由某个输入开关决定。** 开关会被忘在打开的
位置上，而「这是不是一个 tag」没有中间状态。
