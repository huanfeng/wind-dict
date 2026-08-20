# 生成应用图标：圆角方块底 + 白色「词」字。
#
# 用法: .\scripts\gen-icon.ps1
# 产物 (提交入库, 运行期直接 include_bytes!):
#   assets/app-icon-64.png   标题栏品牌块用 (64px 源, 20 逻辑 px 处显示; 200% DPI 下 40 物理 px)
#   assets/tray-32.rgba      托盘用的裸 RGBA (Tray::icon_rgba 只吃裸字节, 不解 PNG)
#   assets/wind-dict.ico     多尺寸图标, 供打包工具按 MAINICON 注入 exe 资源
#
# ## 为什么是脚本生成而不是手绘 SVG
#
# windui 的 SVG 光栅化 (resvg) **默认不渲染文字** —— `svg-text` feature 默认关, 开了要拉进
# 整个字体栈并在首次解码时扫系统字体。而本图标的主体恰恰就是一个汉字。把「词」转成矢量
# 路径也可以, 但那要嵌一份字形轮廓, 改字体就得重转, 而它换来的只是超高 DPI 下的锐利度 ——
# 一个 20px 的品牌块不值这个复杂度。
#
# 故: 设计期用真实字体渲染成位图, 产物入库。图标是**不会随主题变的品牌标识**, 位图正合适。
#
# ## 为什么图标不跟随皮肤
#
# 底色写死取明亮皮肤的 accent (#406BCE)。托盘图标与任务栏图标本就无从跟随应用内主题,
# 标题栏那个若单独跟着皮肤变, 同一个标识在三处会是三种颜色。品牌标识保持恒定是对的。

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$Root   = Split-Path $PSScriptRoot -Parent
$Assets = Join-Path $Root "assets"
New-Item -ItemType Directory -Path $Assets -Force | Out-Null

# 明亮皮肤的 accent (src/skin.rs: Color::hex(0x406BCE))。
$Bg   = [System.Drawing.Color]::FromArgb(255, 0x40, 0x6B, 0xCE)
$Fg   = [System.Drawing.Color]::White
$Char = "词"

# 一张 $size 见方的图标位图。
function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # 圆角方块底。半径取边长的 22%, 与 Windows 11 应用图标的观感接近。
    $r = [int][Math]::Round($size * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($size - $d, 0, $d, $d, 270, 90)
    $path.AddArc($size - $d, $size - $d, $d, $d, 0, 90)
    $path.AddArc(0, $size - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $brush = New-Object System.Drawing.SolidBrush($Bg)
    $g.FillPath($brush, $path)

    # 「词」。字号取边长的 55%: 汉字字面接近方形, 再大就顶到圆角上。
    # 用 StringFormat 的居中而非自己算坐标 —— GDI+ 的行盒含 ascent/descent, 手算必偏。
    $fontSize = [float]($size * 0.55)
    $family = $null
    foreach ($n in @("Microsoft YaHei UI", "Microsoft YaHei", "SimHei")) {
        try { $family = New-Object System.Drawing.FontFamily($n); break } catch { }
    }
    if ($null -eq $family) { $family = [System.Drawing.FontFamily]::GenericSansSerif }
    $font = New-Object System.Drawing.Font($family, $fontSize, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = [System.Drawing.StringAlignment]::Center
    $fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
    $fgBrush = New-Object System.Drawing.SolidBrush($Fg)
    # 视觉下沉修正: 汉字的字面框比行盒窄, GDI+ 按行盒居中会略偏上。
    $rect = New-Object System.Drawing.RectangleF(0, ($size * 0.02), $size, $size)
    $g.DrawString($Char, $font, $fgBrush, $rect, $fmt)

    $g.Dispose(); $brush.Dispose(); $fgBrush.Dispose(); $font.Dispose(); $path.Dispose(); $fmt.Dispose()
    return $bmp
}

function Save-Png([System.Drawing.Bitmap]$bmp, [string]$path) {
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Host "  - $([System.IO.Path]::GetFileName($path))  ($($bmp.Width)x$($bmp.Height))"
}

# ---- 标题栏用 PNG ----
$b64 = New-IconBitmap 64
Save-Png $b64 (Join-Path $Assets "app-icon-64.png")

# ---- 托盘用裸 RGBA ----
# Tray::icon_rgba 收的是**非预乘 RGBA8**, 逐像素小端序 R,G,B,A。
# Bitmap 内部是 BGRA 预乘…… 实为 Format32bppArgb (非预乘) 且字节序 B,G,R,A, 故只需换 R/B。
$b32 = New-IconBitmap 32
$data = $b32.LockBits(
    (New-Object System.Drawing.Rectangle(0, 0, 32, 32)),
    [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
    [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$len = 32 * 32 * 4
$buf = New-Object byte[] $len
[System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $buf, 0, $len)
$b32.UnlockBits($data)
for ($i = 0; $i -lt $len; $i += 4) { $t = $buf[$i]; $buf[$i] = $buf[$i + 2]; $buf[$i + 2] = $t }
[System.IO.File]::WriteAllBytes((Join-Path $Assets "tray-32.rgba"), $buf)
Write-Host "  - tray-32.rgba  (32x32 RGBA, $len 字节)"

# ---- 多尺寸 ICO ----
# 手写 ICO 容器: ICONDIR + N×ICONDIRENTRY + N 段 PNG。Vista 起 ICO 内允许直接放 PNG,
# 免去 BMP 的 XOR/AND 掩码那套。宽高字节写 0 表示 256。
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($s in $sizes) {
    $bmp = New-IconBitmap $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    $ms.Dispose(); $bmp.Dispose()
}
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $w.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $w.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$i].Length)
    $w.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $Assets "wind-dict.ico"), $out.ToArray())
$w.Dispose(); $out.Dispose()
Write-Host "  - wind-dict.ico  ($($sizes -join '/') 七个尺寸)"

$b64.Dispose(); $b32.Dispose()
Write-Host "`n图标已生成 -> $Assets"
