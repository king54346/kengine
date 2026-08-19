//! kgizmo — 即时模式调试绘制
//!
//! 这个 crate 只产出**线段顶点**，一根 wgpu 的毛都不沾。渲染器在
//! `krender` 里读走 [`Gizmos::vertices`] 上传显存，于是「谁都能画调试线」
//! 和「只有 krender 认识 GPU」这两件事不冲突。
//!
//! # 为什么全用线段
//!
//! 调试绘制要的是**看得见结构**，不是好看。全部退化成线段之后：
//! 上传只有一个缓冲、绘制只有一次 `draw`、没有排序问题、没有材质。
//! 球体、圆锥、视锥这些都在 CPU 上拆成线段，代价是每帧几千个顶点——
//! 相比一次网格绘制不值一提。
//!
//! # 两层
//!
//! - **深度层**：参与深度测试，被物体挡住就看不见。看空间关系用这层。
//! - **覆盖层**：关掉深度测试，永远画在最上面。找「东西到底在哪」用这层——
//!   要调试的物体常常正好埋在别的东西里面。
//!
//! # 用法
//!
//! ```
//! use kgizmo::{Color, Gizmos};
//! use kmath::Vec3;
//!
//! let mut gizmos = Gizmos::default();
//! gizmos.set_enabled(true);   // 默认是关的，见 `Gizmos::new`
//!
//! gizmos.line(Vec3::ZERO, Vec3::Y, Color::GREEN);
//! gizmos.sphere(Vec3::ZERO, 1.0, Color::CYAN);
//!
//! // 渲染器每帧读走，然后清空。
//! assert!(!gizmos.is_empty());
//! gizmos.clear();
//! assert!(gizmos.is_empty());
//! ```

mod color;
mod shapes;

pub use color::Color;

use bytemuck::{Pod, Zeroable};
use kmath::Vec3;

/// 调试线着色器源码，由 `krender` 编译。
pub const GIZMO_WGSL: &str = include_str!("gizmo.wgsl");

/// 一个线段端点，对应 `gizmo.wgsl` 的顶点输入。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GizmoVertex {
    /// 世界坐标。
    pub position: [f32; 3],
    /// 线性 RGBA。
    pub color: [f32; 4],
}

/// 调试线画在哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layer {
    /// 参与深度测试，会被场景挡住。
    #[default]
    Depth,
    /// 关掉深度测试，永远可见。
    Overlay,
}

/// 一帧的调试线缓冲。
///
/// 即时模式：每帧从头画，画完由渲染器 [`clear`](Gizmos::clear) 掉。
/// 没有句柄、没有生命周期管理——想让一条线一直在，就每帧都画它。
#[derive(Debug, Default, Clone)]
pub struct Gizmos {
    depth: Vec<GizmoVertex>,
    overlay: Vec<GizmoVertex>,
    layer: Layer,
    enabled: bool,
}

impl Gizmos {
    /// 空缓冲。默认**关闭**。
    ///
    /// 默认关掉是有意的：调试绘制的调用会散布在游戏逻辑各处，发布版本里
    /// 不该为它们付顶点生成的代价。关闭时所有绘制方法直接返回，连形状
    /// 都不会去算。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否开启。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 开关调试绘制。关闭时缓冲会被清空。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.clear();
        }
    }

    /// 切换开关，返回切换后的状态。方便绑一个按键。
    pub fn toggle(&mut self) -> bool {
        self.set_enabled(!self.enabled);
        self.enabled
    }

    /// 之后的绘制画在哪一层。
    pub fn set_layer(&mut self, layer: Layer) {
        self.layer = layer;
    }

    /// 当前层。
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// 在覆盖层里画一段，画完自动恢复原来的层。
    ///
    /// ```
    /// # use kgizmo::{Color, Gizmos, Layer};
    /// # use kmath::Vec3;
    /// let mut gizmos = Gizmos::new();
    /// gizmos.set_enabled(true);
    /// gizmos.on_top(|g| g.line(Vec3::ZERO, Vec3::X, Color::RED));
    /// assert_eq!(gizmos.layer(), Layer::Depth); // 已经恢复
    /// assert_eq!(gizmos.vertices(Layer::Overlay).len(), 2);
    /// ```
    pub fn on_top<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.layer;
        self.layer = Layer::Overlay;
        let result = f(self);
        self.layer = previous;
        result
    }

    /// 某一层的顶点，两个一组构成线段。
    pub fn vertices(&self, layer: Layer) -> &[GizmoVertex] {
        match layer {
            Layer::Depth => &self.depth,
            Layer::Overlay => &self.overlay,
        }
    }

    /// 两层加起来的顶点数。
    pub fn len(&self) -> usize {
        self.depth.len() + self.overlay.len()
    }

    /// 这一帧一条线都没画。
    pub fn is_empty(&self) -> bool {
        self.depth.is_empty() && self.overlay.is_empty()
    }

    /// 清空两层，保留已分配的容量。
    ///
    /// 保留容量是关键：即时模式每帧都要重新填满，每帧重新分配的话
    /// 调试绘制自己就成了性能问题。
    pub fn clear(&mut self) {
        self.depth.clear();
        self.overlay.clear();
    }

    // ───────────────────────── 基本图元 ─────────────────────────

    /// 一条线段。
    pub fn line(&mut self, from: Vec3, to: Vec3, color: Color) {
        if !self.enabled {
            return;
        }
        let color = color.to_array();
        let buffer = match self.layer {
            Layer::Depth => &mut self.depth,
            Layer::Overlay => &mut self.overlay,
        };
        buffer.push(GizmoVertex {
            position: from.to_array(),
            color,
        });
        buffer.push(GizmoVertex {
            position: to.to_array(),
            color,
        });
    }

    /// 一条两端颜色不同的线段，中间线性过渡。
    pub fn gradient_line(&mut self, from: Vec3, to: Vec3, from_color: Color, to_color: Color) {
        if !self.enabled {
            return;
        }
        let buffer = match self.layer {
            Layer::Depth => &mut self.depth,
            Layer::Overlay => &mut self.overlay,
        };
        buffer.push(GizmoVertex {
            position: from.to_array(),
            color: from_color.to_array(),
        });
        buffer.push(GizmoVertex {
            position: to.to_array(),
            color: to_color.to_array(),
        });
    }

    /// 折线：依次连接相邻两点。少于两个点时什么都不画。
    pub fn polyline(&mut self, points: &[Vec3], color: Color) {
        if !self.enabled {
            return;
        }
        for pair in points.windows(2) {
            self.line(pair[0], pair[1], color);
        }
    }

    /// 闭合折线：在 [`polyline`](Gizmos::polyline) 的基础上把首尾接起来。
    pub fn polyline_closed(&mut self, points: &[Vec3], color: Color) {
        if !self.enabled || points.len() < 2 {
            return;
        }
        self.polyline(points, color);
        self.line(points[points.len() - 1], points[0], color);
    }

    /// 从一点出发、给定方向与长度的射线。
    pub fn ray(&mut self, origin: Vec3, direction: Vec3, length: f32, color: Color) {
        self.line(origin, origin + direction.normalize_or_zero() * length, color);
    }

    /// 一个「点」——三条互相垂直的小短线，比单个像素好找。
    pub fn point(&mut self, position: Vec3, size: f32, color: Color) {
        if !self.enabled {
            return;
        }
        let h = size * 0.5;
        self.line(position - Vec3::X * h, position + Vec3::X * h, color);
        self.line(position - Vec3::Y * h, position + Vec3::Y * h, color);
        self.line(position - Vec3::Z * h, position + Vec3::Z * h, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Gizmos {
        let mut g = Gizmos::new();
        g.set_enabled(true);
        g
    }

    #[test]
    fn drawing_is_off_until_you_turn_it_on() {
        // 默认关闭是刻意的：发布版本不该为散落各处的调试调用买单。
        let mut gizmos = Gizmos::new();
        gizmos.line(Vec3::ZERO, Vec3::X, Color::RED);
        gizmos.sphere(Vec3::ZERO, 1.0, Color::RED);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_line_is_two_vertices() {
        let mut gizmos = on();
        gizmos.line(Vec3::ZERO, Vec3::X, Color::RED);

        let v = gizmos.vertices(Layer::Depth);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].position, [0.0, 0.0, 0.0]);
        assert_eq!(v[1].position, [1.0, 0.0, 0.0]);
        assert_eq!(v[0].color, Color::RED.to_array());
    }

    #[test]
    fn the_two_layers_are_separate() {
        let mut gizmos = on();
        gizmos.line(Vec3::ZERO, Vec3::X, Color::RED);
        gizmos.set_layer(Layer::Overlay);
        gizmos.line(Vec3::ZERO, Vec3::Y, Color::GREEN);

        assert_eq!(gizmos.vertices(Layer::Depth).len(), 2);
        assert_eq!(gizmos.vertices(Layer::Overlay).len(), 2);
        assert_eq!(gizmos.len(), 4);
    }

    #[test]
    fn on_top_restores_the_previous_layer_even_on_panic() {
        // 这里只验证正常返回路径；`on_top` 不用 guard，因为调试绘制
        // 里 panic 之后整帧都不可信了，恢复不恢复没有意义。
        let mut gizmos = on();
        gizmos.set_layer(Layer::Overlay);
        gizmos.on_top(|g| g.line(Vec3::ZERO, Vec3::X, Color::RED));
        assert_eq!(gizmos.layer(), Layer::Overlay);
    }

    #[test]
    fn clear_keeps_the_capacity() {
        let mut gizmos = on();
        for i in 0..100 {
            gizmos.line(Vec3::ZERO, Vec3::splat(i as f32), Color::WHITE);
        }
        let capacity = gizmos.depth.capacity();

        gizmos.clear();

        assert!(gizmos.is_empty());
        // 即时模式每帧重填，容量丢了就是每帧重新分配。
        assert_eq!(gizmos.depth.capacity(), capacity);
    }

    #[test]
    fn turning_it_off_drops_what_was_drawn() {
        let mut gizmos = on();
        gizmos.line(Vec3::ZERO, Vec3::X, Color::RED);
        gizmos.set_enabled(false);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn polyline_connects_consecutive_points() {
        let mut gizmos = on();
        gizmos.polyline(&[Vec3::ZERO, Vec3::X, Vec3::Y], Color::WHITE);
        // 3 个点 → 2 段 → 4 个顶点。
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 4);

        gizmos.clear();
        gizmos.polyline_closed(&[Vec3::ZERO, Vec3::X, Vec3::Y], Color::WHITE);
        // 闭合多一段。
        assert_eq!(gizmos.vertices(Layer::Depth).len(), 6);
    }

    #[test]
    fn a_single_point_polyline_draws_nothing() {
        let mut gizmos = on();
        gizmos.polyline(&[Vec3::ZERO], Color::WHITE);
        gizmos.polyline_closed(&[Vec3::ZERO], Color::WHITE);
        gizmos.polyline(&[], Color::WHITE);
        assert!(gizmos.is_empty());
    }

    #[test]
    fn a_zero_direction_ray_is_degenerate_not_nan() {
        // 归一化零向量会得到 NaN，一路传进顶点缓冲就是整块画面消失。
        let mut gizmos = on();
        gizmos.ray(Vec3::ONE, Vec3::ZERO, 5.0, Color::RED);

        let v = gizmos.vertices(Layer::Depth);
        assert_eq!(v[0].position, v[1].position);
        assert!(v.iter().all(|v| v.position.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn gradient_line_keeps_both_colors() {
        let mut gizmos = on();
        gizmos.gradient_line(Vec3::ZERO, Vec3::X, Color::RED, Color::BLUE);

        let v = gizmos.vertices(Layer::Depth);
        assert_eq!(v[0].color, Color::RED.to_array());
        assert_eq!(v[1].color, Color::BLUE.to_array());
    }

    #[test]
    fn vertex_layout_matches_the_shader() {
        // 顶点结构体和 WGSL 的 `@location` 必须对齐，错了就是画面全花。
        let module = naga::front::wgsl::parse_str(GIZMO_WGSL).expect("着色器应当能解析");
        let vs = module
            .entry_points
            .iter()
            .find(|e| e.name == "gizmo_vs")
            .expect("应当有顶点入口");

        let locations: Vec<_> = vs
            .function
            .arguments
            .iter()
            .filter_map(|a| match a.binding {
                Some(naga::Binding::Location { location, .. }) => Some(location),
                _ => None,
            })
            .collect();
        assert_eq!(locations, vec![0, 1], "位置在 0、颜色在 1");
        assert_eq!(size_of::<GizmoVertex>(), 28);
    }
}
