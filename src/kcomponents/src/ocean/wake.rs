//! 尾迹：一块跟着某处走的世界空间网格，存两样东西——
//!
//! - **白沫**：物体划过的地方画上去，慢慢消散。
//! - **水面起伏**：一个小的波动方程。物体把水往下压、往两边推，压出来的凹坑以波的形式传开——
//!   船后面拖出 V 字形的尾浪，浮标上下晃荡时一圈圈地往外扩。几个物体的波直接叠加，
//!   两道尾迹相交处自然出现干涉花纹，不用专门处理。
//!
//! 网格中心按整纹素跟着目标挪（内容同步平移），所以船开多远尾迹都跟得上，
//! 而已经画上去的白沫、传开的波留在原来的世界位置上。
//!
//! 纹理：r = 白沫（0–1），g = 起伏高度（`height / HEIGHT_RANGE * 0.5 + 0.5`）。

use kmath::{Vec2, Vec3};
use ktexture::{FilterMode, Sampler, Texture, TextureFormat, WrapMode};

/// 起伏高度的编码范围（米）：±1.5 米以外截断。
pub const HEIGHT_RANGE: f32 = 1.5;

/// 尾迹图。
pub struct WakeMap {
    size: usize,
    /// 覆盖的世界边长（米）。
    extent: f32,
    /// 图中心对应的世界 xz（总是纹素的整数倍）。
    center: Vec2,
    values: Vec<f32>,
    /// 波动方程的这一步和上一步（米）。
    height: Vec<f32>,
    previous: Vec<f32>,
    /// 还没走完的模拟时间（按定长子步推进）。
    pending: f32,
    texture: Texture,
    /// 白沫消散速度（1/秒）。
    pub decay: f32,
    /// 尾浪传播速度（米/秒）。
    pub wave_speed: f32,
    /// 尾浪每秒衰减掉的比例（0–1）。
    pub wave_damping: f32,
}

/// 波动方程的子步长（秒）。网格 1–2 米、波速几米每秒时远在稳定条件以内。
const SUBSTEP: f32 = 1.0 / 60.0;

impl WakeMap {
    pub fn new(size: usize, extent: f32) -> Self {
        let texture = Texture::new(
            size as u32,
            size as u32,
            encode(&vec![0.0; size * size], &vec![0.0; size * size]),
        )
        .with_format(TextureFormat::Linear)
        .with_sampler(Sampler {
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            wrap_u: WrapMode::ClampToEdge,
            wrap_v: WrapMode::ClampToEdge,
            ..Default::default()
        });
        Self {
            size,
            extent,
            center: Vec2::ZERO,
            values: vec![0.0; size * size],
            height: vec![0.0; size * size],
            previous: vec![0.0; size * size],
            pending: 0.0,
            texture,
            decay: 0.2,
            // 比船慢：船速超过波速时，船压出来的波来不及跑到前面，在身后叠成 V 字（和音爆的马赫锥一个道理）。
            wave_speed: 2.4,
            wave_damping: 0.22,
        }
    }

    pub fn texture(&self) -> &Texture {
        &self.texture
    }

    pub fn center(&self) -> Vec2 {
        self.center
    }

    pub fn extent(&self) -> f32 {
        self.extent
    }

    fn texel(&self) -> f32 {
        self.extent / self.size as f32
    }

    /// 世界坐标 → 网格坐标（纹素为单位，纹素中心在 .5）。
    fn local(&self, position: Vec2) -> Vec2 {
        (position - self.center) / self.texel() + Vec2::splat(self.size as f32 / 2.0)
    }

    /// 把图的中心挪到 `target` 附近（按整纹素），已有的白沫和波留在原世界位置。
    pub fn follow(&mut self, target: Vec3) {
        let texel = self.texel();
        let snapped = Vec2::new(
            (target.x / texel).round() * texel,
            (target.z / texel).round() * texel,
        );
        let shift_x = ((snapped.x - self.center.x) / texel).round() as i64;
        let shift_z = ((snapped.y - self.center.y) / texel).round() as i64;
        if shift_x == 0 && shift_z == 0 {
            return;
        }
        let n = self.size as i64;
        let shift = |grid: &[f32]| {
            let mut moved = vec![0.0; grid.len()];
            for z in 0..n {
                for x in 0..n {
                    let (sx, sz) = (x + shift_x, z + shift_z);
                    if (0..n).contains(&sx) && (0..n).contains(&sz) {
                        moved[(z * n + x) as usize] = grid[(sz * n + sx) as usize];
                    }
                }
            }
            moved
        };
        self.values = shift(&self.values);
        self.height = shift(&self.height);
        self.previous = shift(&self.previous);
        self.center = snapped;
    }

    /// 对半径 `radius` 米内的每个纹素调 `f(下标, 1 - (距离/半径)²)`。
    fn for_disc(&mut self, position: Vec3, radius: f32, mut f: impl FnMut(&mut Self, usize, f32)) {
        let n = self.size as i64;
        let local = self.local(Vec2::new(position.x, position.z));
        let r = (radius / self.texel()).max(0.75);
        let (x0, x1) = ((local.x - r).floor() as i64, (local.x + r).ceil() as i64);
        let (z0, z1) = ((local.y - r).floor() as i64, (local.y + r).ceil() as i64);
        for z in z0.max(0)..=z1.min(n - 1) {
            for x in x0.max(0)..=x1.min(n - 1) {
                let d = Vec2::new(x as f32 + 0.5, z as f32 + 0.5).distance(local) / r;
                if d < 1.0 {
                    f(self, (z * n + x) as usize, 1.0 - d * d);
                }
            }
        }
    }

    /// 在世界坐标 `position` 画一个半径 `radius` 米的白沫圆斑，强度 0–1。
    pub fn stamp(&mut self, position: Vec3, radius: f32, strength: f32) {
        self.for_disc(position, radius, |wake, index, falloff| {
            let value = &mut wake.values[index];
            *value = value.max(strength * falloff);
        });
    }

    /// 把半径 `radius` 米内的水面往下压 `depth` 米（负数是往上顶）。
    ///
    /// 压一下就会传开：每帧在移动的物体下面压，就拖出一道尾浪；上下晃的物体就晃出一圈圈波纹。
    pub fn push(&mut self, position: Vec3, radius: f32, depth: f32) {
        self.for_disc(position, radius, |wake, index, falloff| {
            let target = -depth * falloff;
            // 往目标高度拉过去，而不是直接加：物体停着不动时不会越压越深。
            let h = &mut wake.height[index];
            *h += (target - *h) * 0.5;
        });
    }

    /// 物体的尾迹发生器：每帧对每个想留尾迹的物体调一次。
    ///
    /// `velocity` 是物体的速度（米/秒）。水平速度越快、上下晃得越猛，压出的波越大、白沫越多；
    /// 静止的物体什么也不留。
    pub fn emit(&mut self, position: Vec3, velocity: Vec3, radius: f32) {
        let horizontal = Vec2::new(velocity.x, velocity.z).length();
        let vertical = velocity.y.abs();
        let speed = horizontal + vertical * 1.5;
        if speed < 0.05 {
            return;
        }
        let depth = (horizontal * 0.14 + vertical * 0.2).min(1.2) * (radius / 3.0).clamp(0.25, 1.0);
        self.push(position, radius, depth);
        let foam = ((horizontal - 0.8) * 0.18 + (vertical - 0.4) * 0.35).clamp(0.0, 1.0);
        if foam > 0.0 {
            self.stamp(position, radius * 1.15, foam);
        }
    }

    /// 推进：白沫消散、波动方程走几步，刷新纹理。
    pub fn update(&mut self, dt: f32) {
        let keep = (-self.decay * dt).exp();
        for value in &mut self.values {
            *value *= keep;
        }
        self.pending = (self.pending + dt).min(SUBSTEP * 4.0);
        while self.pending >= SUBSTEP {
            self.pending -= SUBSTEP;
            self.step(SUBSTEP);
        }
        self.texture = self.texture.with_pixels(encode(&self.values, &self.height));
    }

    /// 波动方程一步（显式蛙跳）：h' = 2h − h₋ + (c·dt/dx)² ∇²h，再乘一点阻尼。边界固定为 0。
    fn step(&mut self, dt: f32) {
        let n = self.size;
        let courant = (self.wave_speed * dt / self.texel()).min(0.7);
        let k = courant * courant;
        let damping = (1.0 - self.wave_damping).max(0.0).powf(dt);
        let mut next = vec![0.0; n * n];
        for z in 1..n - 1 {
            for x in 1..n - 1 {
                let i = z * n + x;
                let h = self.height[i];
                let laplacian = self.height[i - 1]
                    + self.height[i + 1]
                    + self.height[i - n]
                    + self.height[i + n]
                    - 4.0 * h;
                next[i] = ((2.0 * h - self.previous[i] + k * laplacian) * damping)
                    .clamp(-HEIGHT_RANGE, HEIGHT_RANGE);
            }
        }
        self.previous = std::mem::replace(&mut self.height, next);
    }

    /// 某点的尾迹起伏（米，双线性插值）。图外是 0。
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let n = self.size as i64;
        let local = self.local(Vec2::new(x, z)) - Vec2::splat(0.5);
        let (x0, z0) = (local.x.floor() as i64, local.y.floor() as i64);
        let (tx, tz) = (local.x - x0 as f32, local.y - z0 as f32);
        let at = |x: i64, z: i64| {
            if (0..n).contains(&x) && (0..n).contains(&z) {
                self.height[(z * n + x) as usize]
            } else {
                0.0
            }
        };
        let top = at(x0, z0) * (1.0 - tx) + at(x0 + 1, z0) * tx;
        let bottom = at(x0, z0 + 1) * (1.0 - tx) + at(x0 + 1, z0 + 1) * tx;
        top * (1.0 - tz) + bottom * tz
    }

    /// 某点的泡沫值（测试和调试用）。
    pub fn value_at(&self, position: Vec3) -> f32 {
        let local = self.local(Vec2::new(position.x, position.z));
        let (x, z) = (local.x.floor() as i64, local.y.floor() as i64);
        let n = self.size as i64;
        if (0..n).contains(&x) && (0..n).contains(&z) {
            self.values[(z * n + x) as usize]
        } else {
            0.0
        }
    }
}

fn encode(foam: &[f32], height: &[f32]) -> Vec<u8> {
    let mut data = vec![0u8; foam.len() * 4];
    for (index, (value, h)) in foam.iter().zip(height).enumerate() {
        data[index * 4] = (value.clamp(0.0, 1.0) * 255.0) as u8;
        data[index * 4 + 1] =
            ((h / HEIGHT_RANGE * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
        data[index * 4 + 3] = 255;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_stay_put_in_the_world_when_the_map_follows() {
        let mut wake = WakeMap::new(64, 64.0);
        let spot = Vec3::new(3.0, 0.0, -2.0);
        wake.stamp(spot, 2.0, 1.0);
        assert!(wake.value_at(spot) > 0.8);
        wake.follow(Vec3::new(10.0, 0.0, 5.0));
        assert!(
            wake.value_at(spot) > 0.8,
            "跟着挪之后，白沫还在原来的世界位置"
        );
        wake.update(15.0);
        assert!(wake.value_at(spot) < 0.1, "十五秒后基本散了");
    }

    #[test]
    fn a_push_spreads_out_as_a_ring_and_dies_down() {
        let mut wake = WakeMap::new(96, 96.0);
        let center = Vec3::ZERO;
        wake.push(center, 2.0, 0.6);
        assert!(wake.height_at(0.0, 0.0) < -0.2, "压下去了");
        for _ in 0..90 {
            wake.update(1.0 / 60.0);
        }
        // 一秒半之后波（2.4 米/秒）传到了离中心三四米外。
        let ring = (0..360).step_by(30).map(|a| {
            let a = (a as f32).to_radians();
            wake.height_at(a.cos() * 3.5, a.sin() * 3.5).abs()
        });
        assert!(ring.fold(0.0, f32::max) > 0.005, "波没有传开");
        for _ in 0..2400 {
            wake.update(1.0 / 60.0);
        }
        assert!(
            wake.height_at(3.5, 0.0).abs() < 0.01,
            "四十秒后应该平静下来"
        );
    }

    #[test]
    fn a_moving_emitter_leaves_a_trail_behind_it() {
        let mut wake = WakeMap::new(128, 128.0);
        let velocity = Vec3::new(6.0, 0.0, 0.0);
        let mut position = Vec3::new(-30.0, 0.0, 0.0);
        for _ in 0..300 {
            wake.emit(position, velocity, 2.5);
            wake.update(1.0 / 60.0);
            position += velocity / 60.0;
        }
        // 船从 -30 开到 0：身后（-15 附近）有白沫、水面在动；船头前方（+15）还是平的。
        assert!(
            wake.value_at(Vec3::new(-6.0, 0.0, 0.0)) > 0.2,
            "身后应该有白沫"
        );
        let behind = (-20..-5)
            .map(|x| wake.height_at(x as f32, 3.0).abs())
            .fold(0.0, f32::max);
        let ahead = (8..20)
            .map(|x| wake.height_at(x as f32, 3.0).abs())
            .fold(0.0, f32::max);
        assert!(
            behind > 0.02 && behind > ahead * 3.0,
            "尾浪应该在身后：身后 {behind}，前方 {ahead}"
        );
    }
}
