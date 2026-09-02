# wind-dict 脚本的公共部分：路径与输出。由 dev.ps1 / release.ps1 点源。
#
# 只放**两个脚本都要用**的东西。这个文件一旦变成杂物间，下一个人就得先读完它
# 才敢改 dev.ps1——那时它带来的麻烦就超过它省下的重复了。
#
# ## 编码必须是带 BOM 的 UTF-8
#
# Windows PowerShell 5.1 对无 BOM 的文件按**系统 ANSI 代码页**解释，本文件的中文
# 注释在非简中 locale 上会解成乱码，而失败的样子是一个指向随机行号的语法错误——
# 一个极费时的假故障。dev.ps1 此前正是如此（本次一并补上 BOM）。

# ---------- 路径 ----------
# 层级: <repo>\scripts\*.ps1
$ScriptDir   = $PSScriptRoot
$Root        = Split-Path $ScriptDir -Parent
$CacheDir    = "$Root\.cache"          # 下载源 + 构建的 .db (不入库)
$SrcDir      = "$CacheDir\src"         # 下载的 ecdict.csv / cedict.txt / Unihan.zip
$DictDir     = "$CacheDir\dict"        # 构建的 ecdict.db / cedict.db / unihan.db
$MdxDir      = "$CacheDir\mdx"         # 下载的示例 .mdx (不入库)
$BuildDir    = "$Root\build"           # release 产物 (= 部署内容)
$BuildDevDir = "$Root\build_dev"       # dev 产物
$ArtifactDir = "$Root\artifacts"       # 截图、发布包等 (不入库)
$PinFile     = "$Root\windui.ref"      # windui 的提交号 (CI 按它检出)

# ---------- path 依赖的提交号 ----------
# windui.ref 里第一个非注释非空行就是那个提交号。为什么需要它、谁来维护，
# 都写在那个文件自己的注释里——这里只负责读，不重复一遍理由。
#
# 读不到就返回 $null（文件被删、或只剩注释）：调用方各自决定这算警告还是硬失败。
function Get-PinnedWindui {
    if (-not (Test-Path $PinFile)) { return $null }
    foreach ($line in Get-Content $PinFile -Encoding UTF8) {
        $t = $line.Trim()
        if ($t -and -not $t.StartsWith("#")) { return $t }
    }
    return $null
}

# 本机 ../wind-ui-rust 当前的提交号（完整 40 位）。不在 git 工作树里则返回 $null。
function Get-LocalWindui ([string]$path) {
    if (-not (Test-Path $path)) { return $null }
    git -C $path rev-parse --git-dir *> $null
    if ($LASTEXITCODE -ne 0) { return $null }
    return (git -C $path rev-parse HEAD).Trim()
}

# ---------- 原生命令的失败方式 ----------
# PowerShell 7.3 起 $PSNativeCommandUseErrorActionPreference 会让"退出码非零的原生
# 命令"在 ErrorActionPreference=Stop 下**抛异常**。本仓库两个脚本都是先跑 cargo、
# 再看 $LASTEXITCODE 自己报错——那条开关一旦为真，就永远走不到那句检查，用户看到的
# 是一段 PowerShell 的异常堆栈而不是"构建失败!"。
#
# 当前 pwsh 默认为 $false，但这是**默认值**，不是保证；显式钉死，免得哪天升级 pwsh
# 后两个脚本一起换一种失败方式。
$PSNativeCommandUseErrorActionPreference = $false

# ---------- 输出辅助 ----------
function Say    ([string]$m) { Write-Host $m -ForegroundColor Green }
function Warn   ([string]$m) { Write-Host $m -ForegroundColor Yellow }
function ErrMsg ([string]$m) { Write-Host $m -ForegroundColor Red }
function Gray   ([string]$m) { Write-Host $m -ForegroundColor DarkGray }

# 人类可读的字节数。发布清单里全用它，免得同一份文件在两处报出两种单位。
function Size-Of ([string]$path) {
    $n = (Get-Item $path).Length
    if ($n -ge 1MB) { return "{0:N1} MB" -f ($n / 1MB) }
    if ($n -ge 1KB) { return "{0:N0} KB" -f ($n / 1KB) }
    return "$n B"
}
