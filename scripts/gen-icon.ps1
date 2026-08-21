# 生成应用图标：圆角方块底 + 白色「词」字。
#
# 用法: .\scripts\gen-icon.ps1
# 产物 (提交入库, 运行期直接 include_bytes!):
#   assets/app-icon-40.png   标题栏品牌块用 (40px 源, 20 逻辑 px 处显示; 200% DPI 下 1:1)
#   assets/tray-32.rgba      托盘用的裸 RGBA (Tray::icon_rgba 只吃裸字节, 不解 PNG)
#   assets/wind-dict.ico     多尺寸图标, 供打包工具按 MAINICON 注入 exe 资源
#
# ## 为什么是脚本生成而不是手绘 SVG
#
# windui 的 SVG 光栅化 (resvg) **默认不渲染文字** —— `svg-text` feature 默认关, 开了要拉进
# 整个字体栈并在首次解码时扫系统字体。而本图标的主体恰恰就是一个汉字。
#
# 把「词」转成矢量路径 (导出字形轮廓) 能绕开这条。这里曾写着「它换来的只是超高 DPI 下
# 的锐利度」—— **那句话是错的**, 实测推翻了它: windui 持有 SVG 源时会在绘制期按
# `canvas.dpi_scale()` 光栅到物理尺寸, 换来的是**任何 DPI 下都 1:1**, 而 1:1 与否正是
# 锐利度的全部 (见下一节的逐档对比)。
#
# 结论仍是不做, 但理由换了: 代价除了「嵌字形轮廓、改字体就得重转」, 还要动 src/icon.rs
# 那层预光栅缓存 —— 它交给 windui 的是已光栅好的位图, 矢量路径根本走不到。而 40px 源
# 在 200% 下已经是 1:1, 剩下的只有 >200% 那几档。
#
# 故: 设计期用真实字体渲染成位图, 产物入库。图标是**不会随主题变的品牌标识**, 位图正合适。
#
# ## 标题栏那张为什么是 40px (曾是 64px)
#
# 它在界面里占 20 **逻辑** px, 物理像素随 DPI 走: 100% 是 20, 200% 是 40。而
# `icon::app` 走的是 `raster()` 的 PNG 分支 —— PNG 没有矢量源, 按原尺寸解码后交给
# 渲染层缩放 (见 src/icon.rs)。旁边那些 SVG 图标按 `size * RASTER_SCALE` 出图,
# 恰好绕开了采样问题, 唯独这张位图没人管。
#
# **锐利只有 1:1 一条路**。逐档比过 (80px 降到 40 / 40px 直出 1:1), 差别不是细微:
# 只要经过降采样, 笔画边缘就带一圈灰 —— 源图按 80 格网做的 hinting, 降到 40 之后
# 网格全错位, hinting 的好处刚好被抵消。1:1 那版笔画干净见边。
#
# 40 = 20 × 2, 即 **200% DPI 下 1:1**。各档的实际比例:
#   100% → 2:1 降采样    150% → 1.33:1 降采样    200% → 1:1 (最优)
#   250% → 放大 1.25x    300% → 放大 1.5x
#
# 取 40 而不是 80, 是拿 >200% 的退化换 ≤200% 的锐利: 40px 源在 100/150/200 三档上
# **每一档都严格优于 80px 源**, 只有 250% 以上才反过来 (位图放大没有补救)。日常
# 使用是 200%, 与 src/icon.rs 里 RASTER_SCALE 的取向一致。
#
# 要彻底摆脱这个取舍, 得让标识走矢量: windui 持有 SVG 源时会在**绘制期**按
# `canvas.dpi_scale()` 光栅到物理尺寸, 任何 DPI 都 1:1 (见 windui `ui/image.rs`
# 的 `paint_into`)。但 src/icon.rs 出于另一条正确性理由自己接管了光栅缓存, 交给
# windui 的是已光栅好的位图, 那条路径用不上 —— 见该文件模块头。
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

$FontSize = 0.68   # 字号占边长的比例, 见 New-IconBitmap 里的说明

# 字体只查一次: 每档尺寸都重查一遍纯属浪费, 且「不同尺寸可能落到不同字体」是个
# 谁也不想调试的故障模式。
$Family = $null
foreach ($n in @("Microsoft YaHei UI", "Microsoft YaHei", "SimHei")) {
    try { $Family = New-Object System.Drawing.FontFamily($n); break } catch { }
}
if ($null -eq $Family) { $Family = [System.Drawing.FontFamily]::GenericSansSerif }

# 「词」在给定字号下的**字面框**(相对行盒原点)。
#
# 为什么不用 StringFormat 的居中: 那个居中是按**行盒**算的, 而行盒含 ascent/descent
# 的留白, 汉字字面比它窄且不对称 —— 两个方向都会偏。原先靠 `$size * 0.02` 的经验
# 偏移去凑, 那是在给一个算错的中心打补丁, 尺寸一变补丁就失准。
#
# GraphicsPath 给的是字形轮廓的真实包围盒, 由它算出的平移量对任何尺寸都成立。
function Get-GlyphBounds([float]$em) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddString($Char, $Family, 0, $em, (New-Object System.Drawing.PointF(0, 0)),
                 [System.Drawing.StringFormat]::GenericTypographic)
    $bb = $p.GetBounds()
    $p.Dispose()
    return $bb
}

# 一张 $size 见方的图标位图。
function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    # GridFit 保留字体 hinting, 把笔画钉到像素网格上 —— 这是「锐利」的来源。
    #
    # 试过的替代路线, 都更糊, 别再走一遍:
    #   - 4x 超采样后降采样: 边缘更**平滑**, 但平滑不等于锐利, 它把笔画边界摊到了
    #     相邻像素上, 中小尺寸肉眼可见地发灰。
    #   - GraphicsPath + FillPath: 无 hinting, 同样发灰。
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # 圆角方块底。半径取边长的 22%, 与 Windows 11 应用图标的观感接近。
    #
    # **路径右/下边界取 size-1 而不是 size**, 这一条不是微调:
    #
    # GDI+ 默认的 PixelOffsetMode 把像素中心放在**整数**坐标上 —— 即最后一行像素的中心
    # 是 size-1, 不是 size-0.5。原先路径画到 size, 于是最后一行整个落在弧的内侧, 底部
    # 和右侧的圆角被吃掉一整行 (32px 档实测: 顶部首行不透明 21px, 底部末行 27px, 倒数
    # 两行还是满宽 32)。表现出来就是上圆角完好、下圆角是平的, 看着像「图标下方被裁了
    # 一截」。
    #
    # 试过内缩半像素 (0.5 .. size-0.5), 那是按「像素中心在 x+0.5」的约定算的, 与这里
    # 的 PixelOffsetMode 对不上 —— 结果反而变成顶部退了一整行、底部照旧满。要么改
    # PixelOffsetMode 要么按它的约定取边界, 这里取后者: 少一处全局状态。
    $r = [float]($size * 0.22)
    $d = $r * 2
    $x0 = [float]0
    $y0 = [float]0
    $x1 = [float]($size - 1)
    $y1 = [float]($size - 1)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($x0, $y0, $d, $d, 180, 90)
    $path.AddArc(($x1 - $d), $y0, $d, $d, 270, 90)
    $path.AddArc(($x1 - $d), ($y1 - $d), $d, $d, 0, 90)
    $path.AddArc($x0, ($y1 - $d), $d, $d, 90, 90)
    $path.CloseFigure()
    $brush = New-Object System.Drawing.SolidBrush($Bg)
    $g.FillPath($brush, $path)

    # 「词」。字号取边长的 68%。
    #
    # 曾是 55%, 主体在方块里偏小、四周留白过多。逐档比过 0.62 / 0.68 / 0.74:
    # 0.74 时字已顶到圆角上, 0.68 是「尽量大且四周仍有呼吸」的那一档。
    #
    # 注意字号大小与清晰度无关 —— 16px 档把字号从 0.55 提到 0.68 反而更糊。清晰度的
    # 瓶颈是采样(每条笔画摊到几个像素), 不是字号。放大是为了观感, 不是为了锐利。
    $em = [float]($size * $FontSize)
    $bb = Get-GlyphBounds $em
    $dx = [float](($size - $bb.Width) / 2 - $bb.X)
    $dy = [float](($size - $bb.Height) / 2 - $bb.Y)
    $font = New-Object System.Drawing.Font($Family, $em, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $fgBrush = New-Object System.Drawing.SolidBrush($Fg)
    # GenericTypographic: 默认的 GenericDefault 会在字串两侧加额外留白, 那会让上面
    # 精确算出的 $dx 白算。
    $g.DrawString($Char, $font, $fgBrush, (New-Object System.Drawing.PointF($dx, $dy)),
                  [System.Drawing.StringFormat]::GenericTypographic)

    $g.Dispose(); $brush.Dispose(); $fgBrush.Dispose(); $font.Dispose(); $path.Dispose()
    return $bmp
}

function Save-Png([System.Drawing.Bitmap]$bmp, [string]$path) {
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Host "  - $([System.IO.Path]::GetFileName($path))  ($($bmp.Width)x$($bmp.Height))"
}

# ---- 标题栏用 PNG ----
$b40 = New-IconBitmap 40
Save-Png $b40 (Join-Path $Assets "app-icon-40.png")

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

$b40.Dispose(); $b32.Dispose()
Write-Host "`n图标已生成 -> $Assets"
