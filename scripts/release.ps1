# wind-dict 发布脚本：构建 → 组装 → **验证** → 压包。
#
# 用法:
#   .\scripts\release.ps1                          # 全量
#   .\scripts\release.ps1 -ExpectVersion v0.2.0    # CI: 与 tag 不符即失败
#   .\scripts\release.ps1 -SkipBuild -NoSmoke      # 只重打包 (改 README/许可 时)
#   .\scripts\release.ps1 -Repo owner/wind-dict    # 顺带出一份 scoop manifest
#
# 产物 → artifacts\release\ (不入库):
#   wind-dict-<版本>-x64.zip           exe + 三份词库 + 许可 + 说明 + 一本示例 MDX
#   wind-dict-<版本>-x64.zip.sha256    sha256sum 格式, scoop / 校验用
#   wind-dict-<版本>-smoke.png         冒烟那一步的截图 (不进包, 是"这个包长这样"的存照)
#   wind-dict.json                     scoop manifest (给了 -Repo 才生成)
#
# ## 为什么这不只是一句 Compress-Archive
#
# 发布包真正的失败方式**不是"文件少了"**——那种一眼看得见。是这些:
#
#   - 打进去的是 debug 那个 exe    → 慢十倍, 且它读写的是另一个用户数据目录
#   - 词库是上次 gen-data 的半成品 → 装上能启动, 查什么都"未收录"
#   - 版本号与 git tag 对不上      → 用户报的 bug 指不到源码
#   - 从有未提交改动的树打的包     → 同上, 且根本无法重现
#
# 四种全都要等用户装上之后才暴露, 而那时已经无从查起。故本脚本在**压包之前**:
#
#   1. 版本、git 提交、工作树是否干净, 对不上就不打;
#   2. 拿 offline_probe / mdx_probe 真去查一次**暂存目录里的那几份文件**——不是查
#      .cache 里的原件, 复制这一步本身就可能出错;
#   3. 用**暂存目录里的那个 exe** 跑一次离屏截图。这是唯一覆盖"exe 与词库放在一起
#      能不能起来"的检查: 它走完真实的启动路径 (resolve_dict_dir → check_dir →
#      建窗口 → 渲染), 而这条路上任何一处失败在 release 下都是**静默**的
#      (windows_subsystem="windows", 没有控制台)。
#
# ## 为什么全量一个包
#
# 程序 ~2MB, 词库 ~190MB, 合成一个包意味着每个补丁版都要重下 190MB 一模一样的词库。
# 仍然这么定, 是因为"下载 → 解压 → 双击"必须是一条直线: 分包会让用户在拿到程序之后
# 还差一步才能用, 而那一步失败时的表现是"程序打不开"(见 ADR-0016)。
# 哪天词库真的大到扛不住, 再切分包 —— 那时组装这一步 (Copy-Staging) 原样可用。

param(
    # 输出目录。默认 artifacts\release\。
    [string]$Out,
    # 期望的版本号 (CI 传 git tag, 前缀 v 会被去掉)。与 Cargo.toml 不符即失败。
    [string]$ExpectVersion,
    # 允许从有未提交改动的工作树打包。
    [switch]$AllowDirty,
    # 复用 target\release 里已有的 exe, 不重新 cargo build。
    [switch]$SkipBuild,
    # 跳过启动冒烟 (无桌面会话的 CI runner 上用)。
    [switch]$NoSmoke,
    # GitHub 仓库 owner/name。给了才生成 scoop manifest —— URL 得有个准数, 猜不得。
    [string]$Repo
)

$ErrorActionPreference = "Stop"
. "$PSScriptRoot\common.ps1"

$Arch   = "x64"
$OutDir = if ($Out) { $Out } else { "$ArtifactDir\release" }
$DevPs1 = "$ScriptDir\dev.ps1"

# ---------- 版本 ----------
# 版本号只有一个来源: Cargo.toml 的 [package] 段。exe 的资源版本也从这里来
# (CARGO_PKG_VERSION → build.rs), 故包名与文件属性天然一致, 不存在第二份。
#
# 限定在 [package] 段内是必须的: [dependencies] 里满是 version = "0.32"。
function Get-CargoVersion {
    $inPkg = $false
    foreach ($line in Get-Content "$Root\Cargo.toml" -Encoding UTF8) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPkg = ($Matches[1] -eq 'package'); continue }
        if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    return $null
}

# git 提交与工作树状态。不在 git 仓库里也能打包 (返回空提交号), 只是包里没有出处。
function Get-GitInfo {
    $info = @{ Commit = ""; Dirty = $false; Known = $false }
    git -C $Root rev-parse --git-dir *> $null
    if ($LASTEXITCODE -ne 0) { return $info }
    $info.Commit = (git -C $Root rev-parse --short HEAD).Trim()
    $info.Dirty  = [bool]((git -C $Root status --porcelain) -join "")
    $info.Known  = $true
    return $info
}

# path 依赖各自的 git 状态。
#
# 拒绝脏工作树, 图的是"这个包由哪次提交产生"有个准数。而 windui 是 **path 依赖**
# (../wind-ui-rust) —— 它占了这个 exe 的一大半, 却完全不在本仓库的 git status 里。
# 只管本仓库等于把那半边的可重现性凭空丢了: 同一个 wind-dict 提交, 隔壁改一行,
# 打出来就是另一个程序, 而包上任何地方都看不出区别。
#
# 从 Cargo.toml 现扫而不是把 ../wind-ui-rust 写死: 日后再添一个 path 依赖时,
# 写死的那种会**静默**漏掉它 —— 正是这里要防的那类事。
function Get-PathDeps {
    $deps = @()
    foreach ($line in Get-Content "$Root\Cargo.toml" -Encoding UTF8) {
        if ($line -notmatch 'path\s*=\s*"([^"]+)"') { continue }
        $raw = Join-Path $Root $Matches[1]
        if (-not (Test-Path $raw)) { continue }
        $full = (Resolve-Path $raw).Path
        $d = @{ Name = (Split-Path $full -Leaf); Path = $full; Commit = ""; Dirty = $false; Known = $false }
        git -C $full rev-parse --git-dir *> $null
        if ($LASTEXITCODE -eq 0) {
            $d.Commit = (git -C $full rev-parse --short HEAD).Trim()
            $d.Dirty  = [bool]((git -C $full status --porcelain) -join "")
            $d.Known  = $true
        }
        $deps += $d
    }
    return $deps
}

# ---------- 准备:词库与示例词典 ----------
# 两者都由 dev.ps1 负责取得 (那里有下载、重试、增量构建的全套逻辑)。这里只判断
# "在不在", 缺了就把活交回去 —— 免得同一件事在两个脚本里各写一遍然后慢慢漂移。
function Ensure-Payload {
    foreach ($f in @("ecdict.db", "cedict.db", "unihan.db")) {
        if (-not (Test-Path "$DictDir\$f")) {
            Warn "词库缺 $f, 交给 dev.ps1 gd 生成 (首次要数分钟)..."
            & $DevPs1 gd
            if ($LASTEXITCODE -ne 0) { ErrMsg "gen-data 失败"; return $false }
            break
        }
    }
    # 字形库可缺 —— 但那是**运行时**的宽容 (少一行部首笔画), 不是发布时的。发出去的
    # 包缺了它, 每个用户都少这项功能且无从补救, 故这里当作硬性缺失。
    foreach ($f in @("ecdict.db", "cedict.db", "unihan.db")) {
        if (-not (Test-Path "$DictDir\$f")) { ErrMsg "词库仍缺: $f"; return $false }
    }
    if (-not (Get-ChildItem $MdxDir -Filter *.mdx -ErrorAction SilentlyContinue)) {
        Warn "示例词典缺失, 交给 dev.ps1 gm 下载 (~70MB)..."
        & $DevPs1 gm
        if ($LASTEXITCODE -ne 0) { ErrMsg "下载示例 MDX 失败"; return $false }
    }
    return $true
}

# ---------- 组装 ----------
function Copy-Staging ([string]$stage, [string]$version, [hashtable]$git, [array]$deps) {
    # 暂存目录是刻意留到下次发布的（排障时第一件事就是翻"到底打进去了什么"），
    # 于是很容易有人正拿它里头的 exe 在跑 —— 直接 Remove-Item 会甩出一句原始的
    # "another process" 报错，指着 cedict.db，而真正要说的是"先把那个程序关掉"。
    if (Test-Path $stage) {
        $live = Get-Process -Name "wind-dict" -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -like "$stage\*" }
        if ($live) {
            ErrMsg "暂存目录里的 wind-dict.exe 正开着 (PID $($live.Id -join ', '))，先关掉它再打包"
            exit 1
        }
        Remove-Item -Recurse -Force $stage
    }
    New-Item -ItemType Directory -Path $stage, "$stage\dicts-example" -Force | Out-Null

    Copy-Item "$Root\target\release\wind-dict.exe" "$stage\wind-dict.exe" -Force
    foreach ($f in @("ecdict.db", "cedict.db", "unihan.db")) {
        Copy-Item "$DictDir\$f" "$stage\$f" -Force
    }
    # 许可文件必须进包: cedict.db 源自 CC-CEDICT, 按 CC BY-SA 4.0 分发, 署名是**分发
    # 时**的义务, 不是仓库里放一份就算履行了。unihan.db 的 Unicode License 同理,
    # 它要求"随数据保留声明"。
    Copy-Item "$Root\THIRD-PARTY.md" "$stage\THIRD-PARTY.md" -Force

    $mdx = Get-ChildItem $MdxDir -Filter *.mdx | Select-Object -First 1
    Copy-Item $mdx.FullName "$stage\dicts-example\ecdict-headless.mdx" -Force

    Write-Readme "$stage\README.txt" $version $git $deps
}

# 包里的 README。**生成而不是入库一份**: 版本号、构建日期、提交号都在这里, 入库
# 就等于把它们抄成第二份, 而抄错时没有任何报错, 只是包里写着一个陈旧的号码。
function Write-Readme ([string]$path, [string]$version, [hashtable]$git, [array]$deps) {
    $stamp = (Get-Date).ToString("yyyy-MM-dd")
    $from  = if ($git.Known) { "提交 $($git.Commit)" } else { "(非 git 工作树)" }
    # path 依赖的提交也写进去: 少了它, "这个包从哪来的"只答对了一半。
    foreach ($d in $deps) {
        if ($d.Known) { $from += " · $($d.Name) $($d.Commit)" }
    }
    $text = @"
清风词典 wind-dict $version ($Arch)
构建于 $stamp · $from

绿色应用：解压到任意目录，双击 wind-dict.exe 即可使用。
默认唤起热键 Ctrl+Alt+X（可在设置里改）。关闭窗口只是收起，程序留在托盘等热键。

── 这个目录里有什么 ────────────────────────────────
  wind-dict.exe   程序
  ecdict.db       英汉词库
  cedict.db       汉英词库
  unihan.db       字形库（部首、笔画、繁简）
  THIRD-PARTY.md  三份词库各自的上游与许可

  这三份 .db 就是「词库目录」，默认取 exe 同目录，所以别把 exe 单独挪走。
  真要分开放，在 设置 → 词库 → 词库目录 里指向它们所在的目录。

── 你的数据不在这里 ────────────────────────────────
  收藏、历史、设置存在   %LOCALAPPDATA%\wind-dict-data\
  这是刻意的：删掉本目录（升级、卸载）碰不到它们。要彻底清除请另外删那个目录。

  若你在设置里打开过「开机自启」，卸载时请先在设置里关掉，再删目录——否则注册表里
  会留下一条指向已删除文件的自启项。

── 用户词典（可选）────────────────────────────────
  dicts-example\ecdict-headless.mdx 是一本示例 MDX，用来演示「把词典丢进目录就能查」。

  把它（或你自己的 .mdx）复制到：
      %LOCALAPPDATA%\wind-dict-data\dicts\
  找不到那个目录就在 设置 → 词典 里点「打开」，它会直接把那个目录开出来。
  丢进去就能用——程序自动扫到，在 设置 → 词典 里逐本开关。这个目录同样不随卸载删除。

  这本示例与内置英汉库同源且无音标，只为让你一眼看出用户词典那一段确实生效了；
  有更合用的词典就换掉它。

── 许可 ──────────────────────────────────────────
  程序代码 MIT OR Apache-2.0。
  三份词库各有其上游与协议，见 THIRD-PARTY.md——其中 cedict.db 源自 CC-CEDICT，
  按 CC BY-SA 4.0 分发。
"@
    # CRLF：包里的 .txt 多半被记事本或压缩软件的预览打开，那些地方对 LF 的处理
    # 至今仍不一致。BOM 同理，免得中文被当成 ANSI。
    $text = $text -replace "`r?`n", "`r`n"
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding $true))
}

# ---------- 验证 ----------
# 探的是**暂存目录里的副本**, 不是 .cache 里的原件: 复制这一步本身就可能漏、可能
# 截断, 而"原件是好的"不能证明"包里那份是好的"。
function Test-Payload ([string]$stage) {
    Say "`n[verify] 词库 —— offline_probe 真查一次暂存目录"
    Push-Location $Root
    try {
        cargo run --release --quiet --example offline_probe -- $stage | Out-Null
        if ($LASTEXITCODE -ne 0) { ErrMsg "暂存目录里的词库打不开或表结构不符"; return $false }
        Say "[verify] 示例词典 —— mdx_probe"
        cargo run --release --quiet --example mdx_probe -- "$stage\dicts-example\ecdict-headless.mdx" | Out-Null
        if ($LASTEXITCODE -ne 0) { ErrMsg "示例 MDX 读不出来"; return $false }
    } finally { Pop-Location }
    Gray "  - 词库与示例词典均可查"
    return $true
}

# 启动冒烟: 用包里那个 exe 跑一次离屏截图。
#
# 三点必须这么做:
#   - **不传词库路径**。程序默认取 exe 同目录, 这正是用户解压后的样子; 传参数反而
#     绕开了要验的那条路 (何况 main 见到任何 -- 开关就不认位置参数了)。
#   - **沙箱 LOCALAPPDATA**。截图模式会读写设置与历史; 打个包顺手清空自己的历史
#     是不能接受的。data_dir() 读的就是这个环境变量, 改它即可隔离。
#   - **必须有超时**。词库不可用时 main 走 fatal() → 弹 MessageBox → 无人点确定,
#     进程就永远挂在那里。CI 上这会变成一次静默的卡死。
function Test-Smoke ([string]$stage, [string]$shot) {
    Say "`n[verify] 启动冒烟 —— 用包里的 exe 跑一次离屏截图"
    if (Test-Path $shot) { Remove-Item $shot -Force }
    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("wind-dict-smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Path $sandbox -Force | Out-Null
    $saved = $env:LOCALAPPDATA
    $env:LOCALAPPDATA = $sandbox
    try {
        $p = Start-Process -FilePath "$stage\wind-dict.exe" -ArgumentList "--screenshot", "`"$shot`"" -PassThru
        if (-not $p.WaitForExit(90000)) {
            $p.Kill()
            ErrMsg "启动 90 秒没退出 —— 多半是弹了错误框在等人点确定 (词库不可用?)"
            return $false
        }
        if ($p.ExitCode -ne 0) { ErrMsg "退出码 $($p.ExitCode)"; return $false }
        if (-not (Test-Path $shot)) { ErrMsg "没出图: $shot"; return $false }
        return (Test-Shot $shot)
    } finally {
        $env:LOCALAPPDATA = $saved
        Remove-Item -Recurse -Force $sandbox -ErrorAction SilentlyContinue
    }
}

# 截图不是一张空窗口。
#
# **不按文件大小判**。本项目的空状态大片纯色, 一张完全正确的 920x620 截图只有
# 15 KB —— 头一版按 20KB 设阈值, 当场把一个好产物打了回来。一个会误伤正确结果的
# 判据比没有判据坏: 它教人去调阈值, 而不是去看那张图。
#
# 判据换成取样点上有多少种颜色: 渲染器起不来时窗口是纯白或纯黑 (1~2 种), 真画出来
# 就有底色、文字、强调色、边框 (实测 21 种)。
function Test-Shot ([string]$shot) {
    $n = (Get-Item $shot).Length
    if ($n -lt 2KB) { ErrMsg "截图只有 $n 字节, 不像一张图"; return $false }
    # System.Drawing 是 Windows 专有的; 拿不到就退回"出了图就算过", 并说出来 ——
    # 少验一层胜过让整条发布线卡在一个可选的检查上。
    try { Add-Type -AssemblyName System.Drawing -ErrorAction Stop }
    catch { Warn "  - System.Drawing 不可用, 本次只验到「出了图」($(Size-Of $shot))"; return $true }
    $bmp = [System.Drawing.Bitmap]::FromFile((Resolve-Path $shot).Path)
    try {
        if ($bmp.Width -lt 800 -or $bmp.Height -lt 500) {
            ErrMsg "截图 $($bmp.Width)x$($bmp.Height), 窗口没按 920x620 建起来"; return $false
        }
        $seen = @{}
        for ($y = 4; $y -lt $bmp.Height; $y += 17) {
            for ($x = 4; $x -lt $bmp.Width; $x += 17) { $seen[$bmp.GetPixel($x, $y).ToArgb()] = 1 }
        }
        if ($seen.Count -lt 6) { ErrMsg "整张图只有 $($seen.Count) 种颜色, 界面是空的"; return $false }
        Gray "  - 起得来、开得了词库、画得出界面 ($($bmp.Width)x$($bmp.Height), 取样 $($seen.Count) 色)"
        return $true
    } finally { $bmp.Dispose() }
}

# ---------- 压包 ----------
function New-Zip ([string]$stage, [string]$zip) {
    if (Test-Path $zip) { Remove-Item $zip -Force }
    try { Add-Type -AssemblyName System.IO.Compression.FileSystem -ErrorAction SilentlyContinue } catch {}
    Say "`n[pack] 压缩 (词库压得动, 但 ~190MB 要等一会)..."
    $t = [Diagnostics.Stopwatch]::StartNew()
    # 走 .NET 而不是 Compress-Archive: 后者在这个量级上慢到数分钟, 且历史上有过
    # 大文件相关的坑。includeBaseDirectory=$true 让包里带一层顶层目录, 解压不会
    # 把一堆文件铺满当前目录 (scoop 那边对应 extract_dir)。
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $stage, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    $t.Stop()
    Gray "  - $(Size-Of $zip)  用时 $([int]$t.Elapsed.TotalSeconds)s"
}

# 回读包里的条目, 与暂存目录逐条比对。
#
# 验的是**包**而不是暂存目录: 前面所有检查针对的都是磁盘上那堆文件, 而用户拿到的
# 是这个 zip。压缩这一步漏一个文件不会报错, 只会让某台机器上的程序打不开词库。
function Test-Zip ([string]$stage, [string]$zip, [string]$rootName) {
    $want = Get-ChildItem $stage -Recurse -File |
        ForEach-Object { "$rootName/" + $_.FullName.Substring($stage.Length + 1).Replace("\", "/") } |
        Sort-Object
    $a = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try { $got = $a.Entries | ForEach-Object { $_.FullName } | Sort-Object } finally { $a.Dispose() }
    $diff = Compare-Object $want $got
    if ($diff) {
        ErrMsg "包内条目与暂存目录不一致:"
        $diff | ForEach-Object { ErrMsg "  $($_.SideIndicator) $($_.InputObject)" }
        return $false
    }
    Gray "  - 包内 $($got.Count) 个条目与暂存目录逐条对上"
    return $true
}

function New-Checksum ([string]$zip) {
    $h = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    # sha256sum 的格式: 哈希, 两个空格, 文件名。这样 Linux 侧的 `sha256sum -c` 能直接用。
    [System.IO.File]::WriteAllText("$zip.sha256", "$h  $(Split-Path $zip -Leaf)`n")
    return $h
}

# ---------- scoop ----------
# 只在给了 -Repo 时生成: manifest 的核心是那个下载 URL, 而 URL 猜不得 —— 猜错的
# manifest 比没有 manifest 坏, 它会装出一个 404 而不是一句"还没配"。
function New-ScoopManifest ([string]$repo, [string]$version, [string]$rootName, [string]$zipName, [string]$hash, [string]$path) {
    $m = [ordered]@{
        version      = $version
        description  = "常驻托盘的桌面词典：全局热键唤起，离线词典为主"
        homepage     = "https://github.com/$repo"
        license      = "MIT OR Apache-2.0"
        architecture = [ordered]@{
            "64bit" = [ordered]@{
                url  = "https://github.com/$repo/releases/download/v$version/$zipName"
                hash = $hash
            }
        }
        extract_dir  = $rootName
        shortcuts    = @(, @("wind-dict.exe", "清风词典"))
        # persist 是空的, 而这不是漏写: 收藏/历史/设置都在 %LOCALAPPDATA%\wind-dict-data\,
        # 本就在 scoop 的 app 目录之外 (ADR-0011)。scoop update / uninstall 动不到它们,
        # 故没有什么需要 persist 保住。
        persist      = @()
        notes        = @(
            "词库随包附带, 无需另外下载。",
            "示例词典: 把 dicts-example\ecdict-headless.mdx 复制到 %LOCALAPPDATA%\wind-dict-data\dicts\ 即可在 设置->词典 里启用。",
            "收藏与历史存在 %LOCALAPPDATA%\wind-dict-data\, 卸载不会删除。"
        )
        checkver     = [ordered]@{ github = "https://github.com/$repo" }
        autoupdate   = [ordered]@{
            architecture = [ordered]@{
                "64bit" = [ordered]@{
                    url = "https://github.com/$repo/releases/download/v`$version/wind-dict-`$version-$Arch.zip"
                }
            }
            extract_dir = "wind-dict-`$version-$Arch"
        }
    }
    $json = ($m | ConvertTo-Json -Depth 8) -replace "`r?`n", "`r`n"
    [System.IO.File]::WriteAllText($path, $json + "`r`n", (New-Object System.Text.UTF8Encoding $false))
}

# ---------- 主流程 ----------
$version = Get-CargoVersion
if (-not $version) { ErrMsg "从 Cargo.toml 的 [package] 段里读不出 version"; exit 1 }

if ($ExpectVersion) {
    $want = $ExpectVersion.TrimStart("v", "V")
    if ($want -ne $version) {
        # CI 上 tag 与 Cargo.toml 对不上, 通常是"改完版本忘了提交"或"打错了 tag"。
        # 两种都必须当场停: 发出去之后, 用户报的 bug 就指不回任何一个提交了。
        ErrMsg "版本对不上: git tag 说 $want, Cargo.toml 说 $version"
        exit 1
    }
}

$git = Get-GitInfo
if ($git.Known) {
    if ($git.Dirty -and -not $AllowDirty) {
        ErrMsg "工作树有未提交的改动, 拒绝打包 (确要如此加 -AllowDirty)"
        git -C $Root status --short
        exit 1
    }
    if ($git.Dirty) { Warn "工作树不干净, 这个包无法由 $($git.Commit) 重现" }
} else {
    Warn "不在 git 工作树里, 包内 README 不会记录出处"
}

$deps = @(Get-PathDeps)
foreach ($d in $deps) {
    if (-not $d.Known) { Warn "路径依赖 $($d.Name) 不在 git 工作树里, 无从记录它的版本"; continue }
    if ($d.Dirty -and -not $AllowDirty) {
        ErrMsg "路径依赖 $($d.Name) 有未提交的改动, 拒绝打包 (确要如此加 -AllowDirty)"
        git -C $d.Path status --short
        exit 1
    }
    if ($d.Dirty) { Warn "路径依赖 $($d.Name) 不干净, 这个包无法由 $($d.Commit) 重现" }
}

# windui.ref 必须与真正参与构建的那份 windui 对上。
#
# 那个文件是 CI 检出 windui 的依据, 也是"这个包由哪两个提交产生"里的另一半。对不上
# 意味着:本机打出来的包与 CI 按同一个 tag 打出来的**不是同一个程序**, 而包内 README
# 上写着的那个 windui 提交号会指向一份并非用来构建它的代码 —— 一个看不出错的错误。
#
# 与 dev.ps1 里同名的检查不同, 这里是**硬失败**: 日常开发容得下两边不同步, 发布容不下。
# CI 上这一条永远成立(windui 就是按 pin 检出的), 除非有人用 WINDUI_REF 覆盖了它 ——
# 那种情况下更要拦。
$pin = Get-PinnedWindui
if ($pin) {
    foreach ($d in $deps) {
        if (-not $d.Known -or ($d.Name -notlike "wind-ui*")) { continue }
        $head = Get-LocalWindui $d.Path
        if ($head -and $head -ne $pin) {
            ErrMsg "windui.ref 与实际参与构建的 windui 对不上:"
            ErrMsg "  windui.ref  $($pin.Substring(0, 7))"
            ErrMsg "  $($d.Path)  $($head.Substring(0, 7))"
            Gray  "  先把 windui 推上去, 再 .\scripts\dev.ps1 pin, 然后提交 windui.ref"
            exit 1
        }
    }
} else {
    Warn "windui.ref 读不出提交号, 无从核对 path 依赖的版本"
}

$name     = "wind-dict-$version-$Arch"
$stage    = "$OutDir\staging\$name"
$zip      = "$OutDir\$name.zip"
$shot     = "$OutDir\wind-dict-$version-smoke.png"

Say "`n========== 发布 $name =========="
Gray "  版本 $version   提交 $(if ($git.Known) { $git.Commit } else { '-' })   输出 $OutDir"
foreach ($d in $deps) { Gray "  路径依赖 $($d.Name) $(if ($d.Known) { $d.Commit } else { '(非 git)' })" }

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
if (-not (Ensure-Payload)) { exit 1 }

if ($SkipBuild) {
    if (-not (Test-Path "$Root\target\release\wind-dict.exe")) {
        ErrMsg "-SkipBuild 但 target\release\wind-dict.exe 不存在"; exit 1
    }
    Warn "跳过构建, 复用 target\release\wind-dict.exe"
} else {
    Say "`n[build] cargo build --release"
    Push-Location $Root
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { ErrMsg "构建失败"; exit 1 }
    } finally { Pop-Location }
}

Say "`n[stage] 组装 → $stage"
Copy-Staging $stage $version $git $deps
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    Gray ("  - {0,-42} {1}" -f $_.FullName.Substring($stage.Length + 1), (Size-Of $_.FullName))
}

if (-not (Test-Payload $stage)) { exit 1 }
if ($NoSmoke) { Warn "`n跳过启动冒烟 (-NoSmoke)" } elseif (-not (Test-Smoke $stage $shot)) { exit 1 }

New-Zip $stage $zip
if (-not (Test-Zip $stage $zip $name)) { exit 1 }
$hash = New-Checksum $zip

if ($Repo) {
    New-ScoopManifest $Repo $version $name (Split-Path $zip -Leaf) $hash "$OutDir\wind-dict.json"
    Gray "  - scoop manifest → $OutDir\wind-dict.json"
} else {
    Gray "  - 未给 -Repo, 不生成 scoop manifest (URL 猜不得)"
}

# 暂存目录留着不删: 发布出问题时, 第一件事就是翻"到底打进去了什么"。它在
# artifacts\ 下, 已被 .gitignore 挡住, 且下次发布会整个重建。
Say "`n========== 完成 =========="
Write-Host "  $zip"
Write-Host "  $(Size-Of $zip)"
Write-Host "  sha256  $hash"
if (-not $NoSmoke) { Write-Host "  截图    $shot" }
Write-Host ""
Gray "  上传到 GitHub Release:"
Gray "    gh release create v$version `"$zip`" `"$zip.sha256`" --title `"v$version`" --notes-file <说明>"
