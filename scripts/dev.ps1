# wind-dict 开发脚本
#
# 用法:
#   .\scripts\dev.ps1            # 交互式菜单
#   .\scripts\dev.ps1 <命令>...  # 直调, 可连续: .\scripts\dev.ps1 gd 1 p
#
# wind-dict 是绿色单目录应用: 产物 = wind-dict.exe + 两个词库 (ecdict.db / cedict.db)。
# 词库由 examples/build_ecdict|build_cedict 从下载的 ECDICT(CSV) / CC-CEDICT(txt) 构建,
# 太大不入库 (见 .gitignore), 由 gen-data 生成到 .cache/dict/。
#
# 命令:
#   b / d        Dev 构建 → build_dev/ (exe + 词库软复制)
#   1 / r        Release 构建 → build/
#   run          Dev 构建并运行
#   gd           gen-data: 下载词库源 + 构建 ecdict.db / cedict.db → .cache/dict/
#   p / pd       部署 release / dev → 目标目录 (复制 + 可选开机自启)
#   u / ud       卸载 release / dev (删目录 + 移除自启)
#   k=check  l=clippy  t=test  f=fmt  fc=fmt-check  ci(=fc+l+t)  clean
#
# 部署目标 (默认 %LOCALAPPDATA%\wind-dict, 免管理员; 在 scripts\deploy.local.ps1 覆盖):
#   DeployDirRelease = ...\wind-dict
#   DeployDirDev     = ...\wind-dict-dev

param(
    [Parameter(Position = 0, ValueFromRemainingArguments)] [string[]]$Commands = @()
)

$ErrorActionPreference = "Stop"

# ---------- 路径 ----------
# 层级: <repo>\scripts\dev.ps1
$ScriptDir   = $PSScriptRoot
$Root        = Split-Path $ScriptDir -Parent
$CacheDir    = "$Root\.cache"          # 下载源 + 构建的 .db (不入库)
$SrcDir      = "$CacheDir\src"         # 下载的 ecdict.csv / cedict.txt
$DictDir     = "$CacheDir\dict"        # 构建的 ecdict.db / cedict.db
$BuildDir    = "$Root\build"           # release 产物 (= 部署内容)
$BuildDevDir = "$Root\build_dev"       # dev 产物

# 词库源。
$EcdictUrl = "https://raw.githubusercontent.com/skywind3000/ECDICT/master/ecdict.csv"
$CedictUrl = "https://www.mdbg.net/chinese/export/cedict/cedict_1_0_ts_utf-8_mdbg.txt.gz"

# ---------- 部署目标 (可在 deploy.local.ps1 覆盖) ----------
$DeployDirRelease = "$env:LOCALAPPDATA\wind-dict"
$DeployDirDev     = "$env:LOCALAPPDATA\wind-dict-dev"
$deployCfg = "$ScriptDir\deploy.local.ps1"
if (Test-Path $deployCfg) { . $deployCfg }

# ---------- 输出辅助 ----------
function Say    ([string]$m) { Write-Host $m -ForegroundColor Green }
function Warn   ([string]$m) { Write-Host $m -ForegroundColor Yellow }
function ErrMsg ([string]$m) { Write-Host $m -ForegroundColor Red }
function Gray   ([string]$m) { Write-Host $m -ForegroundColor DarkGray }

function Out-For ([string]$profile) { if ($profile -eq "dev") { $BuildDevDir } else { $BuildDir } }

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

    $ecdictDb = "$DictDir\ecdict.db"
    $cedictDb = "$DictDir\cedict.db"
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
    } finally { Pop-Location }
    Say "`ngen-data 完成 → $DictDir"
    return $true
}

# 确保词库存在; 缺则触发 gen-data。
function Ensure-Dict {
    if ((Test-Path "$DictDir\ecdict.db") -and (Test-Path "$DictDir\cedict.db")) { return $true }
    Warn "词库缺失, 先执行 gen-data..."
    return (Do-GenData)
}

# ---------- 构建 ----------
# exe + 词库组装到 build[_dev]\; 内容即部署内容。
# dev 变体走默认 debug (保留控制台窗口看 panic); release 走优化 + 无控制台。
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

    $sz = [math]::Round((Get-Item "$outdir\wind-dict.exe").Length / 1MB, 2)
    Gray "已组装 → $outdir  (exe ${sz}MB + ecdict.db + cedict.db)"
    return $true
}

function Do-Run {
    if (-not (Build-App "dev")) { return $false }
    Say "`n[run] $BuildDevDir\wind-dict.exe"
    & "$BuildDevDir\wind-dict.exe"
    return $true
}

# ---------- 部署 (绿色: 复制目录 + 开机自启; 免管理员) ----------
function Set-AutoStart ([string]$dir, [string]$name) {
    $exe = Join-Path $dir "wind-dict.exe"
    try {
        Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -Value "`"$exe`"" -Force
        Gray "  - 已配置开机自启 ($name)"
    } catch { Warn "  - 配置开机自启失败: $($_.Exception.Message)" }
}

function Remove-AutoStart ([string]$name) {
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -ErrorAction SilentlyContinue
    Gray "  - 已移除开机自启 ($name)"
}

function Deploy ([string]$profile = "release") {
    $outdir    = Out-For $profile
    $targetDir = if ($profile -eq "dev") { $DeployDirDev } else { $DeployDirRelease }
    $autoName  = if ($profile -eq "dev") { "wind-dict-dev" } else { "wind-dict" }
    if (-not (Test-Path "$outdir\wind-dict.exe")) {
        ErrMsg "无 $outdir 产物; 请先 '$(if($profile -eq 'dev'){'d'}else{'1'})' 构建。"; return $false
    }
    Say "`n========== 部署 ($profile) → $targetDir =========="
    # 先杀掉运行中的实例, 让出文件锁 (常驻工具多半开着)。
    Get-Process -Name "wind-dict" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$targetDir\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 400

    New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
    foreach ($f in @("wind-dict.exe", "ecdict.db", "cedict.db")) {
        Copy-Item "$outdir\$f" "$targetDir\$f" -Force
        Gray "  - $f"
    }
    Set-AutoStart $targetDir $autoName
    Say "`n部署完成. 启动: $targetDir\wind-dict.exe"
    return $true
}

function Uninstall ([string]$profile = "release") {
    $targetDir = if ($profile -eq "dev") { $DeployDirDev } else { $DeployDirRelease }
    $autoName  = if ($profile -eq "dev") { "wind-dict-dev" } else { "wind-dict" }
    Say "`n========== 卸载 ($profile) =========="
    Get-Process -Name "wind-dict" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$targetDir\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 400
    Remove-AutoStart $autoName
    if (Test-Path $targetDir) { Remove-Item -Recurse -Force $targetDir; Gray "  - 已删除 $targetDir" }
    Say "`n卸载完成."
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
        { $_ -in "b", "d" }  { return (Build-App "dev") }
        { $_ -in "1", "r" }  { return (Build-App "release") }
        "run"                { return (Do-Run) }
        "gd"                 { return (Do-GenData) }
        "p"                  { return (Deploy "release") }
        "pd"                 { return (Deploy "dev") }
        "u"                  { return (Uninstall "release") }
        "ud"                 { return (Uninstall "dev") }
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
    Write-Host "  构建/运行:"
    Write-Host "    b   Dev 构建 → build_dev/       1   Release 构建 → build/"
    Write-Host "    run Dev 构建并运行              gd  生成词库 (下载 + 构建 .db)"
    Write-Host "  部署:"
    Write-Host "    p   部署 release                pd  部署 dev"
    Write-Host "    u   卸载 release                ud  卸载 dev"
    Write-Host "  质量:"
    Write-Host "    k check   l clippy   t test   f fmt   fc fmt-check   ci   clean"
    Write-Host "    q 退出"
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
