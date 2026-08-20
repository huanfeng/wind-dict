//! 图标：SVG 资源 + 按主题角色着色。
//!
//! ## 为什么不用字形
//!
//! 界面此前用 `★` `☆` `×` `←` 这些字符当图标。它们在 Windows 上会被 Segoe UI Emoji /
//! Segoe UI Symbol 接管——渲染出来带彩色描边（真机截图上肉眼可见的红蓝边），且字面框
//! 与 UI 字体的行盒对不上，于是**在按钮里既不居中、放大后还发虚**。这不是调字号能救的：
//! 问题出在字体回退，我们对回退到哪个字体没有发言权。
//!
//! ## 着色为什么要「解析成具体颜色」
//!
//! windui 的 `ImageContent::tint` 收的是 `Color` 而**不是** `Role`（`ui/image.rs`），
//! 按钮绘制图标时走 `paint_into` 用图标自带的 tint，不会套用按钮的前景色
//! （`ui/mod.rs` 的 `Button::paint`）。故图标无法像 `fg_role` 那样每帧跟随主题。
//!
//! 这正是 ADR-0012 结案时判定「图标接不上换肤」的那条，至今成立。本模块的对策是
//! **构建期解析**：`Role::resolve(&theme::current())` 取当下的具体色，换肤时由
//! `ui::build` 那层整树重建来刷新（见 `ui::State::set_skin` 与 `skin_rev`）。
//!
//! 这里**没有写死任何颜色**——写的仍是角色，只是解析时机从每帧提前到了构建期。
//!
//! ## 光栅结果为什么必须常驻（这条不是优化，是正确性）
//!
//! windui 的 Direct2D 后端把上传到 GPU 的位图缓存在 `HashMap<usize, ID2D1Bitmap1>` 里，
//! **键是 `Rc<Pixmap>` 的指针地址**（`platform/win32/d2d.rs` 的 `image_cache` 与
//! `render/image.rs` 的 `Image::cache_id`）。地址只在对象活着时唯一：一张图被丢弃后，
//! 分配器完全可能把同一个地址交给下一张图，而缓存里那条旧记录还在——于是**新图画出来
//! 是旧图**。
//!
//! 这不是理论风险，是实测到的：整树重建换肤之后，标题栏的应用标识位置画出了上一帧
//! 设置页那个返回箭头。结果卡片每次查询都重建（`refresh_fav_flags`），收藏星标同样在
//! 这条路上。
//!
//! 对策是**预先光栅化并永久持有**：每个 `(资源, 尺寸, 颜色)` 只光栅一次，`Image` 存进
//! 下面这张表再也不释放，交给界面的是它的 `clone`（克隆的是 `Rc`，地址相同）。地址
//! 既然永不回收，就不可能被别人复用，上游那条缓存于是恒为正确。顺带也省掉了每次重建
//! 都重解一遍 SVG。
//!
//! 代价是这张表只增不减。上界很小且封闭：资源 5 个 × 用到的尺寸 4 档 × 皮肤 3 套的
//! 若干角色色，几十条，每条至多百来 KB。
//!
//! **因此不能用 `ImageContent::from_svg_bytes(bytes, None)`**——那条是 DPI 感知的，
//! 它在 paint 期按物理尺寸**重新光栅**，每次都产出新的 `Image`，正好回到上面那个坑里。
//! 换来的清晰度由 `RASTER_SCALE` 补偿。

use std::cell::RefCell;
use std::collections::HashMap;

use windui::prelude::*;
use windui::theme;

/// 空心星：未收藏。
pub const STAR: &[u8] = include_bytes!("../assets/icons/star.svg");
/// 实心星：已收藏。与 `STAR` 同一条路径，只是填充，故两态切换时星形不跳。
pub const STAR_FILLED: &[u8] = include_bytes!("../assets/icons/star-filled.svg");
/// 叉：清除查询词、移除召回行、关闭抽屉。
pub const CLOSE: &[u8] = include_bytes!("../assets/icons/x.svg");
/// 左箭头：从设置页返回。
pub const BACK: &[u8] = include_bytes!("../assets/icons/arrow-left.svg");

/// 应用标识：圆角方块底 + 白色「词」。由 `scripts/gen-icon.ps1` 生成。
///
/// 位图而非 SVG，因为它的主体是一个汉字，而 windui 的 resvg 默认不渲染 SVG 内的文字
/// （`svg-text` feature 默认关）。理由与取舍详见那个脚本的头注释。
pub const APP: &[u8] = include_bytes!("../assets/app-icon-64.png");

/// 托盘图标的裸 RGBA（32×32）。`Tray::icon_rgba` 只吃裸字节，不解 PNG。
pub const TRAY_RGBA: &[u8] = include_bytes!("../assets/tray-32.rgba");
/// 托盘图标边长。
pub const TRAY_SIZE: u32 = 32;

/// 光栅化倍率：按「逻辑尺寸 × 3」出图。
///
/// 3 而不是 2：日常使用是 200% 缩放（→ 恰好 1.5× 下采样，细描边仍锐利），而 300% 的
/// 屏幕上是 1:1。写 2 的话 300% 屏上就要放大，矢量图标最不该出现的就是被放大。
/// 再往上（400%）会有轻微放大，那是可接受的退化——不为一个罕见档位把常见档位的
/// 内存翻倍。
const RASTER_SCALE: i32 = 3;

thread_local! {
    /// `(资源字节的地址, 逻辑尺寸, 着色 RGBA)` → 已光栅化并着好色的图。
    ///
    /// **只增不减，且必须如此**——理由见模块头「光栅结果为什么必须常驻」。
    /// 谁要给它加淘汰，先读完那一节：淘汰即释放，释放即地址可回收，那正是缺陷的入口。
    static RASTER: RefCell<HashMap<(usize, i32, u32), Image>> = RefCell::new(HashMap::new());
}

/// 角色在**当前**主题下的具体颜色。构建期取值，换肤后需重建才会更新。
pub fn role_color(role: Role) -> Color {
    role.resolve(&theme::current())
}

/// 把颜色压成一个可做 `HashMap` 键的整数。
fn color_key(c: Color) -> u32 {
    u32::from_be_bytes([c.r, c.g, c.b, c.a])
}

/// 取（必要时光栅化并缓存）一张着好色的图。资源解不开时返回 `None`。
fn raster(bytes: &'static [u8], size: i32, role: Role) -> Option<Image> {
    let color = role_color(role);
    let key = (bytes.as_ptr() as usize, size, color_key(color));
    // 先只读地查一次并**立刻结束借用**：下面的解码不该在持有借用时进行。
    if let Some(img) = RASTER.with(|m| m.borrow().get(&key).cloned()) {
        return Some(img);
    }
    // SVG 按倍率光栅；PNG（应用标识）没有矢量源，按原尺寸解出来交给 `Fit` 缩放。
    let raw = if bytes.starts_with(b"\x89PNG") {
        Image::from_png_bytes(bytes).ok()?
    } else {
        Image::from_svg_bytes(bytes, Some((size * RASTER_SCALE).max(1) as u32)).ok()?
    };
    // 应用标识是多色位图，重着色会把它抹成单色——只有单色图标才该走 tint。
    let img = if bytes.starts_with(b"\x89PNG") {
        raw
    } else {
        raw.tinted(color)
    };
    RASTER.with(|m| m.borrow_mut().insert(key, img.clone()));
    Some(img)
}

/// 一枚按角色着色的图标，`size` 见方。不可点——要可点的用 [`button`]。
pub fn view(bytes: &'static [u8], size: i32, role: Role) -> Element {
    Element::image_content(ImageContent::new(raster(bytes, size, role))).size(size, size)
}

/// 应用标识（多色位图，不着色）。
pub fn app(size: i32) -> Element {
    // 角色随便给一个——PNG 分支不着色，`role_color` 的结果只进缓存键。给 `Text` 是为了
    // 让同一张图在不同皮肤下各占一条缓存，而不是让键在皮肤间意外重合。
    Element::image_content(ImageContent::new(raster(APP, size, Role::Text))).size(size, size)
}

/// 一枚按角色着色的图标按钮：自带 hover / press 圆底、手型光标与键盘激活。
pub fn button(bytes: &'static [u8], size: i32, role: Role) -> Element {
    Element::icon_button_content(ImageContent::new(raster(bytes, size, role))).size(size, size)
}
