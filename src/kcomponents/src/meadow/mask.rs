//! 野花遮罩图集：三种花并排画在一张 480 × 256 的图上（three-stylized 的 `flowerShapes.ts`
//! 是拿 canvas 2D 画的，这里自己光栅化同样的形状）。
//!
//! r = 花瓣、g = 茎叶、b = 花心、a = 覆盖。三个颜色通道是**互斥**的（每个像素只属于最后画上去的那一层），
//! 着色器拿它们当权重混三种颜色。每像素 4 × 4 超采样抗锯齿；rgb 不预乘（边上的像素颜色是满的，只有 a 小）。

use kmath::Vec2;

pub const TILE_WIDTH: u32 = 160;
pub const HEIGHT: u32 = 256;
pub const VARIANTS: u32 = 3;
const SUPERSAMPLE: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    Petal = 1,
    Foliage = 2,
    Centre = 3,
}

enum Shape {
    /// 圆头圆角的折线。
    Stroke { points: Vec<Vec2>, width: f32 },
    /// 叶子：从 `origin` 沿 `angle` 伸出 `length`，两边各是一段二次贝塞尔（控制点在 0.35 L、±0.18 L）。
    Leaf {
        origin: Vec2,
        angle: f32,
        length: f32,
    },
    /// 旋转过的椭圆。
    Ellipse {
        center: Vec2,
        radii: Vec2,
        rotation: f32,
    },
}

/// 把点转到以 `origin` 为原点、旋转 `angle` 的局部坐标（canvas 的 `translate` + `rotate` 的逆）。
fn to_local(p: Vec2, origin: Vec2, angle: f32) -> Vec2 {
    let d = p - origin;
    let (s, c) = angle.sin_cos();
    Vec2::new(d.x * c + d.y * s, -d.x * s + d.y * c)
}

fn rotate(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c)
}

impl Shape {
    fn bounds(&self) -> (Vec2, Vec2) {
        match self {
            Shape::Stroke { points, width } => {
                let lo = points.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(*p));
                let hi = points.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(*p));
                (lo - *width * 0.5, hi + *width * 0.5)
            }
            Shape::Leaf { origin, length, .. } => (*origin - *length, *origin + *length),
            Shape::Ellipse { center, radii, .. } => {
                let r = radii.max_element();
                (*center - r, *center + r)
            }
        }
    }

    fn contains(&self, p: Vec2) -> bool {
        match self {
            Shape::Stroke { points, width } => {
                let r = width * 0.5;
                points.windows(2).any(|s| {
                    let (a, b) = (s[0], s[1]);
                    let ab = b - a;
                    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
                    (a + ab * t).distance_squared(p) <= r * r
                })
            }
            Shape::Leaf {
                origin,
                angle,
                length,
            } => {
                let local = to_local(p, *origin, *angle);
                let l = *length;
                if local.x < 0.0 || local.x > l {
                    return false;
                }
                // x(s) = 0.7 L s + 0.3 L s²（单调），反解 s；半宽 = |y(s)| = 0.36 L s (1 − s)。
                let s = (-0.7 + (0.49 + 1.2 * local.x / l).sqrt()) / 0.6;
                local.y.abs() <= 0.36 * l * s * (1.0 - s)
            }
            Shape::Ellipse {
                center,
                radii,
                rotation,
            } => {
                let local = to_local(p, *center, *rotation) / *radii;
                local.length_squared() <= 1.0
            }
        }
    }
}

struct Canvas {
    shapes: Vec<(Shape, Channel)>,
}

impl Canvas {
    fn stem(&mut self, points: &[(f32, f32)], width: f32) {
        let points = points.iter().map(|&(x, y)| Vec2::new(x, y)).collect();
        self.shapes
            .push((Shape::Stroke { points, width }, Channel::Foliage));
    }

    fn leaf(&mut self, x: f32, y: f32, angle: f32, length: f32) {
        self.shapes.push((
            Shape::Leaf {
                origin: Vec2::new(x, y),
                angle,
                length,
            },
            Channel::Foliage,
        ));
    }

    fn ellipse(&mut self, channel: Channel, center: Vec2, radii: Vec2, rotation: f32) {
        self.shapes.push((
            Shape::Ellipse {
                center,
                radii,
                rotation,
            },
            channel,
        ));
    }

    fn petals(&mut self, x: f32, y: f32, radius: f32, count: u32) {
        let origin = Vec2::new(x, y);
        for index in 0..count {
            let angle = index as f32 / count as f32 * std::f32::consts::TAU;
            let center = origin + rotate(Vec2::new(0.0, -radius * 0.62), angle);
            self.ellipse(
                Channel::Petal,
                center,
                Vec2::new(radius * 0.28, radius * 0.58),
                angle,
            );
        }
        self.ellipse(Channel::Centre, origin, Vec2::splat(radius * 0.26), 0.0);
    }

    fn daisy(&mut self, x: f32) {
        let stem = x + 81.0;
        self.stem(
            &[(stem, 248.0), (stem - 3.0, 170.0), (stem + 3.0, 101.0)],
            7.0,
        );
        self.leaf(stem - 2.0, 185.0, std::f32::consts::PI * 0.78, 52.0);
        self.leaf(stem + 1.0, 151.0, -std::f32::consts::PI * 0.66, 45.0);
        self.petals(stem + 3.0, 72.0, 45.0, 9);
    }

    fn spike(&mut self, x: f32) {
        let stem = x + 79.0;
        self.stem(
            &[(stem, 249.0), (stem - 2.0, 172.0), (stem + 5.0, 58.0)],
            7.0,
        );
        self.leaf(stem - 2.0, 184.0, std::f32::consts::PI * 0.84, 51.0);
        self.leaf(stem + 1.0, 154.0, -std::f32::consts::PI * 0.75, 38.0);
        for index in 0..9 {
            let y = 118.0 - index as f32 * 9.0;
            let width = 22.0 - index as f32 * 1.2;
            let even = index % 2 == 0;
            self.ellipse(
                Channel::Petal,
                Vec2::new(stem + if even { -6.0 } else { 6.0 }, y),
                Vec2::new(width * 0.52, 8.0),
                if even { -0.38 } else { 0.38 },
            );
        }
        self.ellipse(
            Channel::Centre,
            Vec2::new(stem + 4.0, 50.0),
            Vec2::new(6.0, 10.0),
            0.0,
        );
    }

    fn branching(&mut self, x: f32) {
        let stem = x + 73.0;
        self.stem(
            &[
                (stem, 249.0),
                (stem - 3.0, 185.0),
                (stem + 8.0, 133.0),
                (stem + 24.0, 68.0),
            ],
            6.0,
        );
        self.stem(
            &[
                (stem + 2.0, 184.0),
                (stem - 31.0, 139.0),
                (stem - 43.0, 108.0),
            ],
            5.0,
        );
        self.stem(&[(stem + 9.0, 139.0), (stem + 43.0, 113.0)], 5.0);
        self.leaf(stem - 1.0, 197.0, std::f32::consts::PI * 0.83, 47.0);
        self.leaf(stem + 4.0, 162.0, -std::f32::consts::PI * 0.64, 43.0);
        self.leaf(stem + 14.0, 139.0, std::f32::consts::PI * 0.64, 35.0);
        self.petals(stem + 24.0, 55.0, 31.0, 8);
        self.petals(stem - 45.0, 98.0, 23.0, 7);
        self.petals(stem + 49.0, 108.0, 21.0, 7);
    }
}

/// 画出图集，返回 RGBA8（行优先，第 0 行是顶端）。
pub fn rasterize() -> (u32, u32, Vec<u8>) {
    let mut canvas = Canvas { shapes: Vec::new() };
    canvas.daisy(0.0);
    canvas.spike(TILE_WIDTH as f32);
    canvas.branching(TILE_WIDTH as f32 * 2.0);

    let width = TILE_WIDTH * VARIANTS;
    let (sw, sh) = (width * SUPERSAMPLE, HEIGHT * SUPERSAMPLE);
    // 每个子样本记最后盖上去的那一层。
    let mut labels = vec![0u8; (sw * sh) as usize];
    let scale = 1.0 / SUPERSAMPLE as f32;
    for (shape, channel) in &canvas.shapes {
        let (lo, hi) = shape.bounds();
        let x0 = ((lo.x / scale).floor().max(0.0) as u32).min(sw);
        let y0 = ((lo.y / scale).floor().max(0.0) as u32).min(sh);
        let x1 = ((hi.x / scale).ceil().max(0.0) as u32).min(sw);
        let y1 = ((hi.y / scale).ceil().max(0.0) as u32).min(sh);
        for sy in y0..y1 {
            for sx in x0..x1 {
                let p = Vec2::new((sx as f32 + 0.5) * scale, (sy as f32 + 0.5) * scale);
                if shape.contains(p) {
                    labels[(sy * sw + sx) as usize] = *channel as u8;
                }
            }
        }
    }
    let mut pixels = vec![0u8; (width * HEIGHT * 4) as usize];
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for y in 0..HEIGHT {
        for x in 0..width {
            let mut counts = [0u32; 4];
            for dy in 0..SUPERSAMPLE {
                for dx in 0..SUPERSAMPLE {
                    let label =
                        labels[((y * SUPERSAMPLE + dy) * sw + x * SUPERSAMPLE + dx) as usize];
                    counts[label as usize] += 1;
                }
            }
            let covered = counts[1] + counts[2] + counts[3];
            let at = ((y * width + x) * 4) as usize;
            if covered > 0 {
                for channel in 0..3 {
                    pixels[at + channel] =
                        (counts[channel + 1] as f32 / covered as f32 * 255.0).round() as u8;
                }
                pixels[at + 3] = (covered as f32 / samples * 255.0).round() as u8;
            }
        }
    }
    (width, HEIGHT, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_atlas_has_three_flowers_with_stems_petals_and_centres() {
        let (width, height, pixels) = rasterize();
        assert_eq!((width, height), (480, 256));
        let at = |x: u32, y: u32| &pixels[((y * width + x) * 4) as usize..][..4];
        // 雏菊：花心在 (84, 72)，花瓣在它正上方约 28 像素处，茎在 (81, 230)。
        assert_eq!(at(84, 72), [0, 0, 255, 255]);
        assert_eq!(at(84, 44), [255, 0, 0, 255]);
        assert_eq!(at(81, 230), [0, 255, 0, 255]);
        // 角上是空的。
        assert_eq!(at(2, 2)[3], 0);
        // 每一格都画了东西。
        for tile in 0..VARIANTS {
            let covered = (0..height)
                .flat_map(|y| (tile * TILE_WIDTH..(tile + 1) * TILE_WIDTH).map(move |x| (x, y)))
                .filter(|&(x, y)| at(x, y)[3] > 128)
                .count();
            assert!(covered > 2000, "第 {tile} 种花只有 {covered} 个像素");
        }
    }
}
