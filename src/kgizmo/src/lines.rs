//! 常驻线段集：挂在场景节点上、跟着节点变换、只上传一次的线段。
//!
//! # 和 [`Gizmos`](crate::Gizmos) 的区别
//!
//! `Gizmos` 是即时模式：每帧重画、每帧整个传上显存。画几百根调试线
//! 没问题，但 GCode 的打印路径动辄十几万段、BVH 骨架要跟着动画走、
//! CAD 模型的边线要随物体一起移动——这些东西**几何不变，只有变换在变**，
//! 每帧重传十几 MB 的顶点纯属浪费。
//!
//! `LineSet` 反过来：顶点是不可变的共享数据（[`Arc`]），渲染器按 id
//! 缓存它的顶点缓冲，之后每帧只换一个 `view_proj × model` 矩阵。
//! 画法和调试线一样（同一个着色器、线段拓扑、不写深度），所以不用
//! 担心它和网格互相遮挡出怪样子。
//!
//! 线宽恒为 1 像素——WebGPU 的线段拓扑不支持别的线宽，three.js 的
//! `LineBasicMaterial.linewidth` 在 WebGL 里同样无效。

use crate::{Color, GizmoVertex};
use kmath::Vec3;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 一组常驻线段（每两个顶点一段）。克隆只增加引用计数。
#[derive(Debug, Clone)]
pub struct LineSet {
    id: u64,
    vertices: Arc<[GizmoVertex]>,
    min: Vec3,
    max: Vec3,
}

impl LineSet {
    /// 用成对的顶点建一个线段集。顶点数为奇数时丢掉最后一个。
    pub fn new(mut vertices: Vec<GizmoVertex>) -> Self {
        vertices.truncate(vertices.len() / 2 * 2);
        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for v in &vertices {
            let p = Vec3::from_array(v.position);
            min = min.min(p);
            max = max.max(p);
        }
        if vertices.is_empty() {
            (min, max) = (Vec3::ZERO, Vec3::ZERO);
        }
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            vertices: vertices.into(),
            min,
            max,
        }
    }

    /// 从一串线段建。
    pub fn from_segments(segments: impl IntoIterator<Item = (Vec3, Vec3, Color)>) -> Self {
        let mut builder = LineSetBuilder::default();
        for (a, b, color) in segments {
            builder.line(a, b, color);
        }
        builder.build()
    }

    /// 唯一 id。渲染器用它做显存缓存的键：几何不可变，id 相同就是同一份数据。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 全部顶点。
    pub fn vertices(&self) -> &[GizmoVertex] {
        &self.vertices
    }

    /// 线段数。
    pub fn segment_count(&self) -> usize {
        self.vertices.len() / 2
    }

    /// 一段都没有。
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// 局部空间的包围盒 `(最小, 最大)`。
    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.min, self.max)
    }
}

/// 逐段攒线段的构建器。
#[derive(Debug, Clone, Default)]
pub struct LineSetBuilder {
    vertices: Vec<GizmoVertex>,
}

impl LineSetBuilder {
    /// 预留 `segments` 段的容量。
    pub fn with_capacity(segments: usize) -> Self {
        Self {
            vertices: Vec::with_capacity(segments * 2),
        }
    }

    /// 加一段。
    pub fn line(&mut self, a: Vec3, b: Vec3, color: Color) -> &mut Self {
        self.gradient(a, b, color, color)
    }

    /// 加一段两端颜色不同的线。
    pub fn gradient(&mut self, a: Vec3, b: Vec3, from: Color, to: Color) -> &mut Self {
        self.vertices.push(GizmoVertex {
            position: a.to_array(),
            color: [from.r, from.g, from.b, from.a],
        });
        self.vertices.push(GizmoVertex {
            position: b.to_array(),
            color: [to.r, to.g, to.b, to.a],
        });
        self
    }

    /// 加一条折线。
    pub fn polyline(&mut self, points: &[Vec3], color: Color) -> &mut Self {
        for pair in points.windows(2) {
            self.line(pair[0], pair[1], color);
        }
        self
    }

    /// 已攒的线段数。
    pub fn segment_count(&self) -> usize {
        self.vertices.len() / 2
    }

    /// 建成线段集。
    pub fn build(self) -> LineSet {
        LineSet::new(self.vertices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_and_ids() {
        let a = LineSet::from_segments([(Vec3::ZERO, Vec3::new(1.0, 2.0, -3.0), Color::RED)]);
        let b = a.clone();
        assert_eq!(a.id(), b.id(), "克隆是同一份数据");
        assert_ne!(a.id(), LineSet::new(Vec::new()).id());
        assert_eq!(a.bounds(), (Vec3::new(0.0, 0.0, -3.0), Vec3::new(1.0, 2.0, 0.0)));
        assert_eq!(a.segment_count(), 1);
    }

    #[test]
    fn polyline_makes_n_minus_one_segments() {
        let mut builder = LineSetBuilder::default();
        builder.polyline(&[Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z], Color::WHITE);
        assert_eq!(builder.build().segment_count(), 3);
    }
}
