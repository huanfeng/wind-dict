# wind-dict 开发脚本
#
# 用法:
#   .\scripts\dev.ps1            # 交互式菜单
#   .\scripts\dev.ps1 <命令>...  # 直调, 可连续: .\scripts\dev.ps1 gd b p
#
# wind-dict 是绿色单目录应用: 产物 = wind-dict.exe + 两个词库 (ecdict.db / cedict.db)
#                                     + 一个字形库 (unihan.db, 可缺)。
# 词库由 examples/build_ecdict|build_cedict 从下载的 ECDICT(CSV) / CC-CEDICT(txt) 构建,
# 太大不入库 (见 .gitignore), 由 gd 生成到 .cache/dict/。
#
# ── 命名规则: release 是基准, dev 在同一个命令**前面加一个 d** ──────────────────
#
# 日常做的事绝大多数是 release; dev 构建只在要带断言跑一跑时才用。此前两者的命令各起
# 各的名 (1/b, p/pd, i/id), 记的是两套东西, 且 "1" 这种名字与它做的事毫无关系。
#
#   b    / db     构建 → build\ / build_dev\  (exe + 三份词库, 内容即部署内容)
#   run  / drun   构建并直接运行 (不部署、不碰注册表; 跑的是 build[_dev]\ 里那个)
#   p    / dp     覆盖式部署 → 目标目录 + 开机自启
#   i    / di     全新部署并启动: 构建 → 停实例 → 清空目录 → 装 → 起
#                 (要看最新改动就用它; p 是覆盖式的, 留得下上一版多出来的文件)
#   u    / du     卸载 (删目录 + 移除自启; 用户数据不动, 见 ADR-0011)
#
# ── 与 release/dev 无关的 ────────────────────────────────────────────────────
#
#   gd            生成词库: 下载源 + 构建 ecdict.db / cedict.db / unihan.db → .cache\dict\
#   gm            下载一本示例 MDX (~70MB), 供手动测试「自带词典」; 部署时自动装进词典目录
#   rel           发布: 构建 + 组装 + 验证 + 压成带版本的 zip → artifacts\release\
#                 (要传参数就直接调 scripts\release.ps1, 见那个文件的头部)
#   k=check  l=clippy  t=test  f=fmt  fc=fmt-check  ci(=fc+l+t)  clean
#
# 部署目标 (默认 %LOCALAPPDATA%\wind-dict, 免管理员; 在 scripts\deploy.local.ps1 覆盖):
#   DeployDirRelease = ...\wind-dict
#   DeployDirDev     = ...\wind-dict-dev

param(
    [Parameter(Position = 0, ValueFromRemainingArguments)] [string[]]$Commands = @()
)

$ErrorActionPreference = "Stop"

# 路径 ($Root/$DictDir/$MdxDir/...) 与输出辅助 (Say/Warn/ErrMsg/Gray) 在 common.ps1,
# 与 release.ps1 共用 —— 那些路径是**同一个事实**, 抄成两份就等着哪天悄悄漂移。
. "$PSScriptRoot\common.ps1"

# ---------- 词库源 ----------
$EcdictUrl = "https://raw.githubusercontent.com/skywind3000/ECDICT/master/ecdict.csv"
$CedictUrl = "https://www.mdbg.net/chinese/export/cedict/cedict_1_0_ts_utf-8_mdbg.txt.gz"
# 字形源。Unihan 是 Unicode 官方数据, latest 随标准每年更新。
$UnihanUrl = "https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip"
# 《通用规范汉字表》(国务院 国发〔2013〕23 号) 的三级字表。
# 该字表属《著作权法》第五条所指行政性质文件, 不适用著作权法, 故可自由使用。
$TghzBase = "https://raw.githubusercontent.com/shengdoushi/common-standard-chinese-characters-table/master"

# 示例自带词典 (MDX)。用于手动测试"把词典丢进目录就能查"这条路, 不随产品分发。
#
# 取 ECDICT 自己发布的 MDX 而非别的: 它是本仓库唯一能自动下载的、授权明确 (MIT/CC)
# 的 MDX。选 headless (无音标) 版是刻意的 —— 它与随程序分发的 ecdict.db 内容同源,
# 若取带音标的版本, 两者显示出来几乎一模一样, 反而看不出自带词典那一段是不是真的
# 走通了; 无音标版一眼能分辨。
$ExampleMdxUrl = "https://github.com/skywind3000/ECDICT/releases/download/1.0.28/ecdict-mdx-headless-28.zip"

# ---------- 部署目标 (可在 deploy.local.ps1 覆盖) ----------
#
# 注意: Uninstall 会 Remove-Item -Recurse -Force 整个目录, 故这里只放随程序分发、
# 可整体替换的东西 (exe + 词库)。用户数据 (收藏/历史) 必须永不丢失, 因此另存于
# %LOCALAPPDATA%\wind-dict-data\ , 不在此目录内 —— 见 docs/adr/0011。
#
# 改这两个路径前先读那份 ADR: 若把部署目录指到用户数据目录上, 卸载即数据丢失。
$DeployDirRelease = "$env:LOCALAPPDATA\wind-dict"
$DeployDirDev     = "$env:LOCALAPPDATA\wind-dict-dev"
$deployCfg = "$ScriptDir\deploy.local.ps1"
if (Test-Path $deployCfg) { . $deployCfg }

# ---------- profile 的派生值 ----------
#
# release 是基准, dev 只是同一件事的另一个变体: 产物目录、部署目录、自启项名字全由它
# 派生。这三个判断此前散在 Deploy / Uninstall / Install-Fresh 里各写一遍 —— 加一个变体
# 要改三处, 而漏掉一处的表现是「部署 dev 却动了 release 的自启项」这类查不清的故障。
function Out-For      ([string]$profile) { if ($profile -eq "dev") { $BuildDevDir } else { $BuildDir } }
function DeployDirFor ([string]$profile) { if ($profile -eq "dev") { $DeployDirDev } else { $DeployDirRelease } }
# 自启项的名字。**必须与 src/autostart.rs 的 VALUE_NAME 一致**: 程序启动时会拿自己算出的
# 那条命令行去比对同名值, 对不上就就地改写 (repair_if_stale)。两边取的都是「dev 加 d 后缀」
# 这个约定 —— 那边按 cfg!(debug_assertions) 分, 这边按 profile 分, 结论相同。
function AutoNameFor  ([string]$profile) { if ($profile -eq "dev") { "wind-dict-dev" } else { "wind-dict" } }
# 构建这个 profile 该敲哪个命令, 供「产物不存在」时的提示引用。
function BuildCmdFor  ([string]$profile) { if ($profile -eq "dev") { "db" } else { "b" } }

# ---------- 词库: 下载 ----------
# 已存在则跳过 (下载是幂等的一次性动作; 删 .cache\src 可强制重下)。
function Get-File ([string]$url, [string]$dst, [string]$desc = "") {
    if (Test-Path $dst) { Gray "[skip] $(Split-Path $dst -Leaf) 已存在"; return $true }
    New-Item -ItemType Directory -Path (Split-Path $dst -Parent) -Force | Out-Null
    Gray "[get ] $(Split-Path $dst -Leaf) $desc"
    $old = $ProgressPreference; $ProgressPreference = "SilentlyContinue"
    try {
        for ($i = 1; $i -le 3; $i++) {
            try { Invoke-WebRequest -Uri $url -OutFile $dst -UseBasicParsing -TimeoutSec 300; return $true }
            catch {
                if (Test-Path $dst) { Remove-Item $dst -Force -ErrorAction SilentlyContinue }
                if ($i -eq 3) { ErrMsg "下载失败 ($i/3): $url`n  $($_.Exception.Message)"; return $false }
                Warn "下载重试 ($i/3): $(Split-Path $dst -Leaf)"; Start-Sleep -Seconds 2
            }
        }
    } finally { $ProgressPreference = $old }
    return $false
}

# $src 是否比 $dst 新 (源更新则需重建)。PowerShell 无 bash 的 -nt, 用 LastWriteTime 比较。
function Test-Newer ([string]$src, [string]$dst) {
    (Get-Item $src).LastWriteTime -gt (Get-Item $dst).LastWriteTime
}

# gzip 解压 (CC-CEDICT 以 .gz 分发; 避免依赖外部 gzip 工具)。
function Expand-Gzip ([string]$src, [string]$dst) {
    $in  = [System.IO.File]::OpenRead($src)
    $out = [System.IO.File]::Create($dst)
    $gz  = New-Object System.IO.Compression.GzipStream($in, [System.IO.Compression.CompressionMode]::Decompress)
    try { $gz.CopyTo($out) } finally { $gz.Dispose(); $out.Dispose(); $in.Dispose() }
}

# ---------- 词库: gen-data ----------
# 下载 ECDICT / CC-CEDICT 源, 用 build_ecdict / build_cedict 构建 .db → .cache\dict\。
# .db 已存在且源未更新则跳过 (构建 ecdict.db 需几分钟)。
function Do-GenData {
    New-Item -ItemType Directory -Path $SrcDir, $DictDir -Force | Out-Null
    Say "`n下载词库源 → $SrcDir"
    $ecdictCsv = "$SrcDir\ecdict.csv"
    $cedictGz  = "$SrcDir\cedict.txt.gz"
    $cedictTxt = "$SrcDir\cedict.txt"
    if (-not (Get-File $EcdictUrl $ecdictCsv "英汉 (~63MB)")) { return $false }
    if (-not (Get-File $CedictUrl $cedictGz  "汉英 (~4MB gz)")) { return $false }
    if (-not (Test-Path $cedictTxt)) { Gray "[gz  ] 解压 cedict.txt.gz"; Expand-Gzip $cedictGz $cedictTxt }

    # Unihan 是 zip 里的一组 .txt, 解压到子目录 (build_unihan 扫整个目录取字段)。
    $unihanZip = "$SrcDir\Unihan.zip"
    $unihanDir = "$SrcDir\unihan"
    if (-not (Get-File $UnihanUrl $unihanZip "字形 (~8MB zip)")) { return $false }
    if ((-not (Test-Path $unihanDir)) -or (Test-Newer $unihanZip $unihanDir)) {
        Gray "[zip ] 解压 Unihan.zip"
        if (Test-Path $unihanDir) { Remove-Item -Recurse -Force $unihanDir }
        Expand-Archive -Path $unihanZip -DestinationPath $unihanDir -Force
    }

    # 字级表: 三个小文本, 缺了 build_unihan 会照建, 只是没有「一级字」这类标记。
    $tghzDir = "$SrcDir\tghz"
    New-Item -ItemType Directory -Path $tghzDir -Force | Out-Null
    foreach ($lv in 1..3) {
        $f = "$tghzDir\level-$lv.txt"
        if (-not (Test-Path $f)) { Get-File "$TghzBase/level-$lv.txt" $f "通用规范汉字表 $lv 级" | Out-Null }
    }

    $ecdictDb = "$DictDir\ecdict.db"
    $cedictDb = "$DictDir\cedict.db"
    $unihanDb = "$DictDir\unihan.db"
    Push-Location $Root
    try {
        if ((-not (Test-Path $ecdictDb)) -or (Test-Newer $ecdictCsv $ecdictDb)) {
            Say "`n构建英汉词库 (~77 万词条, 数分钟)..."
            cargo run --release --example build_ecdict -- $ecdictCsv $ecdictDb
            if ($LASTEXITCODE -ne 0) { ErrMsg "build_ecdict 失败!"; return $false }
        } else { Gray "[skip] ecdict.db 已最新" }

        if ((-not (Test-Path $cedictDb)) -or (Test-Newer $cedictTxt $cedictDb)) {
            Say "`n构建汉英词库 (~12 万词条)..."
            cargo run --release --example build_cedict -- $cedictTxt $cedictDb
            if ($LASTEXITCODE -ne 0) { ErrMsg "build_cedict 失败!"; return $false }
        } else { Gray "[skip] cedict.db 已最新" }

        if ((-not (Test-Path $unihanDb)) -or (Test-Newer $unihanZip $unihanDb)) {
            Say "`n构建字形库 (~10 万字)..."
            cargo run --release --example build_unihan -- $unihanDir $unihanDb $tghzDir
            if ($LASTEXITCODE -ne 0) { ErrMsg "build_unihan 失败!"; return $false }
        } else { Gray "[skip] unihan.db 已最新" }
    } finally { Pop-Location }
    Say "`ngen-data 完成 → $DictDir"
    return $true
}

# ---------- 自带词典目录 ----------
#
# 自带词典 (用户自己放的 MDX) 的目录 —— 不是"词库目录"(那是 exe 同目录的三份 .db)。
# 与 src/store/userdata.rs 的 data_dir() 必须一致: %LOCALAPPDATA%\wind-dict-data[-dev]\dicts。
# 这个目录**不在部署目录内**, 故 u/du 卸载碰不到它 —— 里头是用户自己下载的词典,
# 动辄几百 MB, 卸载程序顺手删掉是不能接受的 (同 ADR-0011)。
function UserDataDir ([string]$profile) {
    $data = if ($profile -eq "dev") { "wind-dict-data-dev" } else { "wind-dict-data" }
    return "$env:LOCALAPPDATA\$data"
}

function UserDictDir ([string]$profile) { return (Join-Path (UserDataDir $profile) "dicts") }

# 下载示例 MDX → .cache\mdx\。仅供手动测试, 不参与构建, 不随产品分发。
function Do-GetMdx {
    New-Item -ItemType Directory -Path $SrcDir, $MdxDir -Force | Out-Null
    if (Get-ChildItem $MdxDir -Filter *.mdx -ErrorAction SilentlyContinue) {
        Gray "[skip] 示例词典已存在 → $MdxDir"; return $true
    }
    $zip = "$SrcDir\ecdict-mdx.zip"
    if (-not (Get-File $ExampleMdxUrl $zip "示例 MDX (~70MB)")) { return $false }

    # zip 里的文件名是 GBK 编码的中文, Expand-Archive 解出来是乱码 —— 解到临时目录
    # 再按"目录里唯一的那个 .mdx"重命名, 不去猜它原本叫什么。
    $tmp = "$MdxDir\_unzip"
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Gray "[zip ] 解压示例词典"
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $found = Get-ChildItem $tmp -Recurse -File | Where-Object { $_.Extension -ieq ".mdx" } | Select-Object -First 1
    if (-not $found) { ErrMsg "压缩包里没有 .mdx"; Remove-Item -Recurse -Force $tmp; return $false }
    Move-Item $found.FullName "$MdxDir\ecdict-example.mdx" -Force
    Remove-Item -Recurse -Force $tmp
    Say "示例词典就绪 → $MdxDir\ecdict-example.mdx"
    return $true
}

# 把示例词典装进词典目录。已有任何 .mdx 就不动 —— 那是用户的东西。
function Install-ExampleMdx ([string]$profile) {
    $dst = UserDictDir $profile
    New-Item -ItemType Directory -Path $dst -Force | Out-Null
    if (Get-ChildItem $dst -Recurse -Filter *.mdx -ErrorAction SilentlyContinue) {
        Gray "  - 词典目录已有词典, 不动 ($dst)"; return
    }
    $src = Get-ChildItem $MdxDir -Filter *.mdx -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $src) {
        Gray "  - 词典目录为空; 跑 'gm' 可下载一本示例词典用于测试"
        Gray "    $dst"
        return
    }
    Copy-Item $src.FullName (Join-Path $dst $src.Name) -Force
    Gray "  - 示例词典 → $dst\$($src.Name)"
}

# 确保词库存在; 缺则触发 gen-data。
function Ensure-Dict {
    if ((Test-Path "$DictDir\ecdict.db") -and (Test-Path "$DictDir\cedict.db")) { return $true }
    Warn "词库缺失, 先执行 gen-data..."
    return (Do-GenData)
}

# ---------- 构建 ----------
# exe + 词库组装到 build[_dev]\; 内容即部署内容。
# dev 变体走默认 debug (未优化 + 断言), release 走优化。两者都是 GUI 子系统 (无控制台):
# dev 构建也会被 dp 部署成常驻程序, 弹黑窗口是产品缺陷不是开发便利。panic 看
# %LOCALAPPDATA%\wind-dict-data-dev\panic.log。
function Build-App ([string]$profile = "release") {
    $outdir = Out-For $profile
    if (-not (Ensure-Dict)) { return $false }

    $targetSub = if ($profile -eq "dev") { "debug" } else { "release" }
    Say "`n[build] wind-dict ($profile)..."
    Push-Location $Root
    try {
        if ($profile -eq "dev") { cargo build } else { cargo build --release }
        if ($LASTEXITCODE -ne 0) { ErrMsg "构建失败!"; return $false }
    } finally { Pop-Location }

    $exe = "$Root\target\$targetSub\wind-dict.exe"
    if (-not (Test-Path $exe)) { ErrMsg "未找到产物: $exe"; return $false }

    if (Test-Path $outdir) { Remove-Item -Recurse -Force $outdir }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    Copy-Item $exe "$outdir\wind-dict.exe" -Force
    Copy-Item "$DictDir\ecdict.db" "$outdir\ecdict.db" -Force
    Copy-Item "$DictDir\cedict.db" "$outdir\cedict.db" -Force
    # 字形库允许缺席 (OfflineDictionary::open 打不开就当没有), 故不因它失败。
    if (Test-Path "$DictDir\unihan.db") {
        Copy-Item "$DictDir\unihan.db" "$outdir\unihan.db" -Force
    } else { Warn "unihan.db 缺失, 本次产物没有部首笔画 (跑 gd 可补上)" }

    $sz = [math]::Round((Get-Item "$outdir\wind-dict.exe").Length / 1MB, 2)
    Gray "已组装 → $outdir  (exe ${sz}MB + ecdict.db + cedict.db + unihan.db)"
    return $true
}

# 构建并直接跑 build[_dev]\ 里那个, **不部署**: 不复制到目标目录、不写自启项。
#
# 与 i/di 的分工: 这条是「编完看一眼」, 那条是「装上用」。跑的是产物目录里的 exe, 故
# 用户数据仍按 profile 分 (dev 走 wind-dict-data-dev), 不会污染日常使用的历史与收藏。
function Do-Run ([string]$profile = "release") {
    if (-not (Build-App $profile)) { return $false }
    $exe = "$(Out-For $profile)\wind-dict.exe"
    Say "`n[run] $exe"
    & $exe
    return $true
}

# ---------- 部署 (绿色: 复制目录 + 开机自启; 免管理员) ----------
function Set-AutoStart ([string]$dir, [string]$name) {
    $exe = Join-Path $dir "wind-dict.exe"
    try {
        # 末尾的 --tray 必须带: 程序按它区分"开机自启"与"用户双击图标"——不带就会
        # 显示窗口 (见 src/autostart.rs 的 TRAY_ARG)。这里与 autostart::command()
        # 写的是同一条命令行, 两边必须一致, 否则程序每次启动都会把它改回去
        # (repair_if_stale)。
        Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -Value "`"$exe`" --tray" -Force
        Gray "  - 已配置开机自启 ($name)"
    } catch { Warn "  - 配置开机自启失败: $($_.Exception.Message)" }
}

function Remove-AutoStart ([string]$name) {
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -ErrorAction SilentlyContinue
    Gray "  - 已移除开机自启 ($name)"
}

# ---------- 停实例 / 清目录 / 起实例 ----------
#
# 停掉某个目录下运行的实例, 并**等它真的退出**。
#
# 按 exe 路径过滤而不是照进程名一律杀: 用户很可能同时开着 release 那个常驻实例, 或者
# 一个从 build_dev\ 直接 run 出来的 —— 部署 dev 没有理由把它们一起带走。
#
# 等待用 WaitForExit 而不是睡一个固定的毫秒数: Stop-Process 返回时进程未必已经消失
# (它只是发出终止请求), 紧接着删目录就会撞上「文件正由另一进程使用」。固定睡眠要么
# 白等要么不够, 而不够的那次会以一次莫名其妙的删除失败呈现 —— 那时人会去调那个数字,
# 不会去想句柄还没放开。
function Stop-App ([string]$dir) {
    $procs = @(Get-Process -Name "wind-dict" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path -like "$dir\*" })
    if ($procs.Count -eq 0) { return $true }
    $ok = $true
    foreach ($proc in $procs) {
        Gray "  - 停止运行中的实例 (PID $($proc.Id))"
        # 强杀而非 CloseMainWindow: 本程序收到 WM_CLOSE 只是把窗口藏起来继续常驻
        # (托盘工具都这样), 温和退出在这里根本不会退。
        try { Stop-Process -Id $proc.Id -Force -ErrorAction Stop }
        catch { Warn "    杀不掉: $($_.Exception.Message)"; $ok = $false; continue }
        try {
            if (-not $proc.WaitForExit(5000)) { ErrMsg "    PID $($proc.Id) 5 秒内没有退出"; $ok = $false }
        } catch { }  # 进程已消失时拿不到句柄, 那正是我们要的结果
    }
    return $ok
}

# 这个目录能不能整个删掉。
#
# $DeployDirRelease / $DeployDirDev 可被 scripts\deploy.local.ps1 覆盖成任意路径, 而
# 下一步要跑的是 Remove-Item -Recurse -Force。一个写错的本地配置就足以删掉用户在别处
# 的东西, 而且是无声的。故先证明这确实是我们装出来的目录, 再动手。
function Test-Wipeable ([string]$dir, [string]$profile) {
    $d = $dir.TrimEnd('\', '/')
    if (-not $d) { ErrMsg "部署目录为空, 拒绝清理"; return $false }

    # 用户数据必须活得比部署久 (ADR-0011)。部署目录若等于数据目录、或把数据目录套在
    # 自己里头, 清理下去就是把收藏和历史一起删了 —— 这种事只会发生一次, 而那一次没有
    # 撤销。u/du 走 Uninstall 时同样该拦, 故这道闸放在两条路都经过的地方。
    $data = (UserDataDir $profile).TrimEnd('\', '/')
    if ($d -ieq $data -or $data.StartsWith("$d\", [StringComparison]::OrdinalIgnoreCase)) {
        ErrMsg "部署目录套着用户数据目录, 拒绝清理:"
        ErrMsg "  部署 $d"
        ErrMsg "  数据 $data"
        return $false
    }

    if (-not (Test-Path $d)) { return $true }
    if (@(Get-ChildItem -LiteralPath $d -Force).Count -eq 0) { return $true }
    # 目录里有东西, 那就必须认得出是我们的。空目录放行是刻意的: 上次部署中途失败
    # 留下一个空壳很常见, 为它停下来只是添堵。
    if (-not (Test-Path (Join-Path $d "wind-dict.exe"))) {
        ErrMsg "目录里没有 wind-dict.exe, 不像是部署目录, 拒绝清理: $d"
        Gray  "  (部署目录在 scripts\deploy.local.ps1 里配; 确实要用它就先手动清空)"
        return $false
    }
    return $true
}

# 整个删掉部署目录。调用前必须先过 Test-Wipeable。
#
# 「整个删」而不是逐个覆盖那几个文件名: 覆盖式部署清不掉上一版多出来的文件 —— 哪天
# 不再分发 unihan.db, 旧的那份会永远留在目录里, 程序照样把它读进来。于是手上跑的是一个
# 混着两个版本残骸的目录, 而从外面完全看不出来。
function Clear-DeployDir ([string]$dir) {
    if (-not (Test-Path $dir)) { return $true }
    # 反病毒、资源管理器的预览窗格、索引服务都可能在进程退出后再攥着句柄一小会儿。
    # 第一次失败就报错, 会把「其实等两百毫秒就好」变成一次假故障。
    for ($i = 1; $i -le 5; $i++) {
        try {
            Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction Stop
            Gray "  - 已清空 $dir"
            return $true
        } catch {
            if ($i -eq 5) {
                ErrMsg "清理失败: $($_.Exception.Message)"
                ErrMsg "  多半还有句柄攥着这个目录里的文件 (另开着一个实例? 资源管理器停在里面?)"
                return $false
            }
            Start-Sleep -Milliseconds (200 * $i)
        }
    }
    return $false
}

# 起一个新实例。
#
# **不带 --tray**: 那个开关是给开机自启用的, 带上就直接收进托盘、什么也不显示
# (见 src/autostart.rs 的 TRAY_ARG)。这条命令的用途是「装完立刻看看新东西」, 起来
# 不给窗口等于没起。
#
# 工作目录设成部署目录只是为了贴近双击图标的样子; 词库按 exe 所在目录找
# (main.rs 的 resolve_dict_dir → offline::exe_dir), 与 cwd 无关。
function Start-App ([string]$dir) {
    $exe = Join-Path $dir "wind-dict.exe"
    if (-not (Test-Path $exe)) { ErrMsg "没有可启动的 exe: $exe"; return $false }
    Start-Process -FilePath $exe -WorkingDirectory $dir
    Gray "  - 已启动 $exe"
    return $true
}

function Deploy ([string]$profile = "release") {
    $outdir    = Out-For $profile
    $targetDir = DeployDirFor $profile
    $autoName  = AutoNameFor $profile
    if (-not (Test-Path "$outdir\wind-dict.exe")) {
        ErrMsg "无 $outdir 产物; 请先 '$(BuildCmdFor $profile)' 构建。"; return $false
    }
    Say "`n========== 部署 ($profile) → $targetDir =========="
    # 先停掉运行中的实例, 让出文件锁 (常驻工具多半开着)。
    if (-not (Stop-App $targetDir)) { ErrMsg "实例没停下来, 复制多半会撞上文件锁"; return $false }

    New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
    foreach ($f in @("wind-dict.exe", "ecdict.db", "cedict.db", "unihan.db")) {
        # unihan.db 可缺: 它是纯增益数据, 少了只是不显示部首笔画。
        if (-not (Test-Path "$outdir\$f")) { Gray "  - $f (跳过, 不存在)"; continue }
        Copy-Item "$outdir\$f" "$targetDir\$f" -Force
        Gray "  - $f"
    }
    Set-AutoStart $targetDir $autoName
    # 自带词典不进部署目录: 它属于用户数据那一侧, 卸载不该删 (见 Dict-Dir)。
    Install-ExampleMdx $profile
    Say "`n部署完成. 启动: $targetDir\wind-dict.exe"
    return $true
}

function Uninstall ([string]$profile = "release") {
    $targetDir = DeployDirFor $profile
    $autoName  = AutoNameFor $profile
    Say "`n========== 卸载 ($profile) =========="
    # 拦一道再删: 卸载与全新部署删的是同一个目录, 那条红线 (别把用户数据卷进来)
    # 对两者一样成立。
    if (-not (Test-Wipeable $targetDir $profile)) { return $false }
    if (-not (Stop-App $targetDir)) { return $false }
    Remove-AutoStart $autoName
    if (-not (Clear-DeployDir $targetDir)) { return $false }
    Say "`n卸载完成. 用户数据 (收藏/历史/自带词典) 仍在 $(UserDataDir $profile)"
    return $true
}

# ---------- 全新部署并启动 ----------
#
# 一条命令跑完「构建 → 停实例 → 清空目录 → 装 → 起」, 供改完代码立刻上手试。
#
# 与 p/dp 的区别只在**清空**与**启动**这两步。分成两个命令而不是给 Deploy 加开关,
# 是因为 p 还有一个正当用法: 目标机上只想换掉 exe 和词库, 不动目录里别的东西。
function Install-Fresh ([string]$profile = "release") {
    $targetDir = DeployDirFor $profile
    Say "`n========== 全新部署 ($profile) → $targetDir =========="
    # 先验目标合法再构建: 构建 release 要几分钟, 让人等完了才说「这个目录我不敢删」
    # 是浪费的。
    if (-not (Test-Wipeable $targetDir $profile)) { return $false }
    if (-not (Build-App $profile)) { return $false }
    if (-not (Stop-App $targetDir)) { return $false }
    if (-not (Clear-DeployDir $targetDir)) { return $false }
    # 复制、自启、示例词典全部复用 Deploy —— 部署内容是同一件事, 抄成两份就等着
    # 哪天只有一边记得加新文件。
    if (-not (Deploy $profile)) { return $false }
    return (Start-App $targetDir)
}

# ---------- 发布 ----------
# 只是把 release.ps1 挂进菜单; 逻辑全在那边 (版本校验、组装、三道验证、压包、校验和)。
# 要传参数 (-ExpectVersion / -Repo / -SkipBuild) 就直接调那个脚本。
function Do-Release {
    # 先归零: release.ps1 顺利跑完时不会调 exit, $LASTEXITCODE 会留着它内部最后一条
    # 原生命令 (cargo) 的值。不归零就等于拿一个陈旧的数当成败判据。
    $global:LASTEXITCODE = 0
    & "$ScriptDir\release.ps1"
    if ($LASTEXITCODE -ne 0) { ErrMsg "发布失败"; return $false }
    return $true
}

# ---------- 代码质量 ----------
function Do-Check    { Say "`ncargo check...";       Push-Location $Root; try { cargo check }                    finally { Pop-Location } }
function Do-Clippy   { Say "`ncargo clippy...";      Push-Location $Root; try { cargo clippy --all-targets }     finally { Pop-Location } }
function Do-Test     { Say "`ncargo test...";        Push-Location $Root; try { cargo test }                     finally { Pop-Location } }
function Do-Fmt      { Say "`ncargo fmt...";         Push-Location $Root; try { cargo fmt }                      finally { Pop-Location } }
function Do-FmtCheck { Say "`ncargo fmt --check..."; Push-Location $Root; try { cargo fmt --all -- --check }     finally { Pop-Location } }
function Do-Clean    { Say "`ncargo clean...";       Push-Location $Root; try { cargo clean }                    finally { Pop-Location } }

function Do-Ci {
    Push-Location $Root
    try {
        Do-FmtCheck; if ($LASTEXITCODE -ne 0) { ErrMsg "fmt 检查失败!"; return $false }
        Do-Clippy;   if ($LASTEXITCODE -ne 0) { ErrMsg "clippy 失败!"; return $false }
        Do-Test;     if ($LASTEXITCODE -ne 0) { ErrMsg "test 失败!";   return $false }
    } finally { Pop-Location }
    Say "`nCI 全部通过 ✓"; return $true
}

# ---------- 命令分发 ----------
function Invoke-Command ([string]$cmd) {
    switch ($cmd) {
        # release 在前、dev 在后, 成对排列 —— 命名规则见文件头部。
        "b"                  { return (Build-App "release") }
        "db"                 { return (Build-App "dev") }
        "run"                { return (Do-Run "release") }
        "drun"               { return (Do-Run "dev") }
        "p"                  { return (Deploy "release") }
        "dp"                 { return (Deploy "dev") }
        "i"                  { return (Install-Fresh "release") }
        "di"                 { return (Install-Fresh "dev") }
        "u"                  { return (Uninstall "release") }
        "du"                 { return (Uninstall "dev") }
        "gd"                 { return (Do-GenData) }
        "gm"                 { return (Do-GetMdx) }
        { $_ -in "rel", "release" } { return (Do-Release) }
        { $_ -in "k", "check" }     { Do-Check;    return $true }
        { $_ -in "l", "clippy" }    { Do-Clippy;   return $true }
        { $_ -in "t", "test" }      { Do-Test;     return $true }
        { $_ -in "f", "fmt" }       { Do-Fmt;      return $true }
        { $_ -in "fc", "fmt-check" }{ Do-FmtCheck; return $true }
        "ci"                        { return (Do-Ci) }
        "clean"                     { Do-Clean;    return $true }
        default { ErrMsg "未知命令: $cmd"; return $false }
    }
}

function Show-Menu {
    Write-Host ""
    Say "===== wind-dict 开发菜单 ====="
    Gray "  release 是基准; dev 在同一个命令前面加一个 d"
    Write-Host ""
    Write-Host "              release          dev"
    Write-Host "  构建        b                db        → build\ / build_dev\"
    Write-Host "  运行        run              drun      构建并直接跑, 不部署"
    Write-Host "  部署        p                dp        覆盖式"
    Write-Host "              i                di        全新部署并启动"
    Write-Host "  卸载        u                du        用户数据不动"
    Write-Host ""
    Write-Host "  数据      gd  生成词库 (下载源 + 构建 .db)"
    Write-Host "            gm  下载示例 MDX (自带词典手动测试用)"
    Write-Host "  发布      rel 打成带版本的 zip → artifacts\release\ (构建+验证+压包)"
    Write-Host "  质量      k check   l clippy   t test   f fmt   fc fmt-check   ci   clean"
    Write-Host "            q 退出"
    Write-Host ""
    $sel = Read-Host "请选择"
    return $sel
}

# ---------- 主入口 ----------
if ($Commands.Count -eq 0) {
    # 交互菜单: 循环直到退出。
    while ($true) {
        $sel = Show-Menu
        if ($sel -in "q", "quit", "exit", "") { break }
        try { Invoke-Command $sel | Out-Null } catch { ErrMsg $_.Exception.Message }
    }
} else {
    # 命令行直调: 连续命令, 任一失败即停 (对齐参考: 前者失败则后者不执行)。
    foreach ($c in $Commands) {
        $ok = Invoke-Command $c
        if ($ok -eq $false) { exit 1 }
    }
}
