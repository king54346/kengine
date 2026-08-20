//! 精灵排序与批处理。
//!
//! # 2D 为什么必须排序
//!
//! 精灵是半透明的，而 alpha 混合**不可交换**：先画角色再画背景，
//! 背景会盖在角色上。3D 靠深度缓冲解决遮挡，2D 不行——精灵全在同一个
//! 平面上，深度值一样。所以顺序必须由 CPU 定。
//!
//! # 三级排序键
//!
//! 1. **层**（`layer`）：背景 / 地面 / 角色 / 特效 / UI，由设计决定；
//! 2. **层内的 Y**：同一层里，越靠下的越晚画（近处遮住远处）——
//!    这是俯视角 2D 游戏的通行做法；
//! 3. **纹理**：前两者相同的精灵按纹理聚在一起，能少断几次批。
//!
//! 第三级只在前两级**完全相同**时才生效。让纹理优先于 Y 的话，
//! 一个角色会被它自己的影子盖住。
//!
//! # 批只在相邻同纹理时合并
//!
//! 排序之后**不能**再把同纹理的精灵拉到一起——那会破坏刚排好的顺序。
//! 所以是「扫一遍，相邻且同纹理就并进上一批」。

use bytemuck::{Pod, Zeroable};
use kcore::uuid::Uuid;
use kmath::{Vec2, Vec4};

use crate::SpriteRegion;

/// 上传给 GPU 的一个精灵，对应 `sprite2d.wgsl` 的 `Sprite`。
///
/// 字段顺序与对齐必须和着色器一致——错一位就是满屏乱码，
/// 而且不会有任何报错。有一条测试盯着大小。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GpuSprite {
    /// 左下角的世界坐标。
    pub position: [f32; 2],
    /// 世界尺寸。
    pub size: [f32; 2],
    /// 图集区域 `[u0, v0, u1, v1]`。
    pub uv: [f32; 4],
    /// 顶点色。
    pub color: [f32; 4],
    /// 绕中心旋转的弧度。
    pub rotation: f32,
    /// 补齐到 64 字节（16 的整数倍，WGSL 存储缓冲的要求）。
    ///
    /// 着色器那边写的是**三个 `f32`** 而不是一个 `vec3`：
    /// WGSL 的 `vec3` 按 16 字节对齐，用它会把结构体撑到 80。
    pub _padding: [f32; 3],
}

impl From<&SpriteInstance> for GpuSprite {
    fn from(instance: &SpriteInstance) -> Self {
        Self {
            position: instance.position.to_array(),
            size: instance.size.to_array(),
            uv: [
                instance.region.min.x,
                instance.region.min.y,
                instance.region.max.x,
                instance.region.max.y,
            ],
            color: instance.color.to_array(),
            rotation: instance.rotation,
            _padding: [0.0; 3],
        }
    }
}

/// 一个待绘制的精灵实例。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteInstance {
    /// 世界位置（左下角）。
    pub position: Vec2,
    /// 世界尺寸。
    pub size: Vec2,
    /// 取图集的哪一格。
    pub region: SpriteRegion,
    /// 顶点色。
    pub color: Vec4,
    /// 用哪张纹理。换纹理就要断批。
    pub texture: Uuid,
    /// 排序层。小的先画（在下面）。
    pub layer: i32,
    /// 绕中心旋转的弧度。
    pub rotation: f32,
}

impl SpriteInstance {
    /// 一个最简单的实例。
    pub fn new(position: Vec2, size: Vec2, texture: Uuid) -> Self {
        Self {
            position,
            size,
            region: SpriteRegion::FULL,
            color: Vec4::ONE,
            texture,
            layer: 0,
            rotation: 0.0,
        }
    }

    /// 换一格。
    pub fn with_region(mut self, region: SpriteRegion) -> Self {
        self.region = region;
        self
    }

    /// 换个层。
    pub fn with_layer(mut self, layer: i32) -> Self {
        self.layer = layer;
        self
    }

    /// 染个色。
    pub fn with_color(mut self, color: Vec4) -> Self {
        self.color = color;
        self
    }

    /// 转个角度。
    pub fn with_rotation(mut self, radians: f32) -> Self {
        self.rotation = radians;
        self
    }

    /// 排序用的 Y。取的是**底边**而不是中心：
    /// 一高一矮两个角色并排站着时，脚在同一条线上才算「一样远」。
    fn sort_y(&self) -> f32 {
        self.position.y
    }
}

/// 一批能一次画完的精灵。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// 在排好序的实例数组里的起点。
    pub first: usize,
    /// 数量。
    pub count: usize,
    /// 这一批用的纹理。
    pub texture: Uuid,
}

/// 层内的排序方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// 按 Y 从大到小：越靠下的越晚画，近处遮住远处。俯视角 2D 的通行做法。
    #[default]
    YDescending,
    /// 不排，保持提交顺序。
    ///
    /// 适合调用方自己知道该怎么排的场合（比如已经按逻辑顺序生成好了）。
    /// 排序不是免费的：几万个精灵每帧排一次是实打实的开销。
    Insertion,
}

/// 把一组精灵排好序并分批。
///
/// **就地排序**：返回的批次下标指向排序之后的 `instances`。
pub fn sort_and_batch(instances: &mut [SpriteInstance], mode: SortMode) -> Vec<Batch> {
    match mode {
        SortMode::YDescending => {
            // 稳定排序：键完全相同的两个精灵要保持提交顺序，
            // 否则同一帧画两次会闪。
            instances.sort_by(|a, b| {
                a.layer
                    .cmp(&b.layer)
                    // Y 大的先画（在后面/上方），Y 小的后画（在前面/下方）。
                    .then_with(|| b.sort_y().total_cmp(&a.sort_y()))
                    // 只有前两级完全相同才按纹理聚。让纹理优先于 Y 的话，
                    // 一个角色会被它自己的影子盖住。
                    .then_with(|| a.texture.cmp(&b.texture))
            });
        }
        SortMode::Insertion => {
            instances.sort_by_key(|s| s.layer);
        }
    }

    batch(instances)
}

/// 把相邻的同纹理精灵并成一批。
///
/// **只并相邻的**。把不相邻的同纹理精灵拉到一起会破坏刚排好的顺序，
/// 半透明的东西就会盖错。
fn batch(instances: &[SpriteInstance]) -> Vec<Batch> {
    let mut batches: Vec<Batch> = Vec::new();
    for (index, instance) in instances.iter().enumerate() {
        match batches.last_mut() {
            Some(last) if last.texture == instance.texture => last.count += 1,
            _ => batches.push(Batch {
                first: index,
                count: 1,
                texture: instance.texture,
            }),
        }
    }
    batches
}

/// 把一个实例展开成四个角的世界坐标，顺序是**左下、右下、右上、左上**。
///
/// 旋转绕矩形中心。绕左下角转的话，改朝向会让精灵整个甩出去。
pub fn corners(instance: &SpriteInstance) -> [Vec2; 4] {
    let half = instance.size * 0.5;
    let center = instance.position + half;
    let local = [
        Vec2::new(-half.x, -half.y),
        Vec2::new(half.x, -half.y),
        Vec2::new(half.x, half.y),
        Vec2::new(-half.x, half.y),
    ];

    if instance.rotation == 0.0 {
        return local.map(|p| center + p);
    }
    let (sin, cos) = instance.rotation.sin_cos();
    local.map(|p| center + Vec2::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos))
}

/// 剔除掉完全在视野外的精灵。
///
/// `view` 是世界矩形 `(左下, 右上)`。旋转过的精灵按它的包围圆判定——
/// 按未旋转的矩形判的话，转 45° 的精灵在边缘会被误剔。
pub fn cull(instances: &[SpriteInstance], view: (Vec2, Vec2)) -> Vec<SpriteInstance> {
    let (lo, hi) = view;
    instances
        .iter()
        .filter(|instance| {
            let half = instance.size * 0.5;
            let center = instance.position + half;
            // 旋转之后包围盒会变大，用外接圆的半径兜底。
            let radius = if instance.rotation == 0.0 {
                half
            } else {
                Vec2::splat(half.length())
            };
            center.x + radius.x >= lo.x
                && center.x - radius.x <= hi.x
                && center.y + radius.y >= lo.y
                && center.y - radius.y <= hi.y
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texture(seed: u8) -> Uuid {
        Uuid::from_bytes([seed; 16])
    }

    fn at(x: f32, y: f32, texture: Uuid) -> SpriteInstance {
        SpriteInstance::new(Vec2::new(x, y), Vec2::ONE, texture)
    }

    #[test]
    fn layers_are_drawn_bottom_up() {
        let mut sprites = vec![
            at(0.0, 0.0, texture(1)).with_layer(5),
            at(0.0, 0.0, texture(1)).with_layer(-2),
            at(0.0, 0.0, texture(1)).with_layer(0),
        ];
        sort_and_batch(&mut sprites, SortMode::YDescending);
        let layers: Vec<i32> = sprites.iter().map(|s| s.layer).collect();
        assert_eq!(layers, vec![-2, 0, 5]);
    }

    #[test]
    fn lower_sprites_are_drawn_later_within_a_layer() {
        // 俯视角 2D 的通行做法：越靠下的越近，该盖住上面的。
        let mut sprites = vec![
            at(0.0, 10.0, texture(1)),
            at(0.0, 30.0, texture(1)),
            at(0.0, 20.0, texture(1)),
        ];
        sort_and_batch(&mut sprites, SortMode::YDescending);
        let ys: Vec<f32> = sprites.iter().map(|s| s.position.y).collect();
        assert_eq!(ys, vec![30.0, 20.0, 10.0]);
    }

    #[test]
    fn the_layer_beats_the_y_position() {
        // 层是设计定的，Y 只在层内起作用。反过来的话，
        // 背景里一个靠下的物件会盖住角色。
        let mut sprites = vec![
            at(0.0, 0.0, texture(1)).with_layer(1),   // 上层，但很靠下
            at(0.0, 100.0, texture(1)).with_layer(0), // 下层，但很靠上
        ];
        sort_and_batch(&mut sprites, SortMode::YDescending);
        assert_eq!(sprites[0].layer, 0, "下层的必须先画");
    }

    #[test]
    fn the_texture_only_breaks_ties() {
        // 让纹理优先于 Y 的话，一个角色会被它自己的影子盖住。
        let mut sprites = vec![
            at(0.0, 10.0, texture(9)),
            at(0.0, 20.0, texture(1)),
            at(0.0, 15.0, texture(9)),
        ];
        sort_and_batch(&mut sprites, SortMode::YDescending);
        let ys: Vec<f32> = sprites.iter().map(|s| s.position.y).collect();
        assert_eq!(ys, vec![20.0, 15.0, 10.0], "纹理不该打乱 Y 的顺序");
    }

    #[test]
    fn identical_keys_keep_the_submission_order() {
        // 不稳定的话同一帧画两次会闪。
        let mut sprites: Vec<SpriteInstance> =
            (0..20).map(|i| at(i as f32, 0.0, texture(1))).collect();
        sort_and_batch(&mut sprites, SortMode::YDescending);
        let xs: Vec<f32> = sprites.iter().map(|s| s.position.x).collect();
        assert_eq!(xs, (0..20).map(|i| i as f32).collect::<Vec<_>>());
    }

    #[test]
    fn adjacent_same_texture_sprites_share_a_batch() {
        let mut sprites = vec![
            at(0.0, 30.0, texture(1)),
            at(0.0, 20.0, texture(1)),
            at(0.0, 10.0, texture(1)),
        ];
        let batches = sort_and_batch(&mut sprites, SortMode::YDescending);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].count, 3);
    }

    #[test]
    fn a_texture_change_breaks_the_batch() {
        let mut sprites = vec![
            at(0.0, 30.0, texture(1)),
            at(0.0, 20.0, texture(2)),
            at(0.0, 10.0, texture(1)),
        ];
        let batches = sort_and_batch(&mut sprites, SortMode::YDescending);
        assert_eq!(batches.len(), 3, "夹在中间的另一张纹理要断成三批");
    }

    #[test]
    fn batching_never_reorders_sprites() {
        // 把不相邻的同纹理精灵拉到一起会破坏刚排好的顺序，
        // 半透明的东西就会盖错。
        let mut sprites = vec![
            at(0.0, 30.0, texture(1)),
            at(0.0, 20.0, texture(2)),
            at(0.0, 10.0, texture(1)),
        ];
        sort_and_batch(&mut sprites, SortMode::YDescending);
        let ys: Vec<f32> = sprites.iter().map(|s| s.position.y).collect();
        assert_eq!(ys, vec![30.0, 20.0, 10.0]);
    }

    #[test]
    fn batches_cover_every_sprite_exactly_once() {
        let mut sprites: Vec<SpriteInstance> = (0..50)
            .map(|i| at(0.0, i as f32, texture((i % 4) as u8)))
            .collect();
        let batches = sort_and_batch(&mut sprites, SortMode::YDescending);

        let mut cursor = 0;
        for batch in &batches {
            assert_eq!(batch.first, cursor, "批次之间有缝或者重叠");
            cursor += batch.count;
        }
        assert_eq!(cursor, sprites.len());
    }

    #[test]
    fn insertion_mode_only_sorts_by_layer() {
        // 排序不是免费的：几万个精灵每帧排一次是实打实的开销。
        let mut sprites = vec![
            at(0.0, 10.0, texture(1)).with_layer(1),
            at(0.0, 30.0, texture(1)).with_layer(0),
            at(0.0, 20.0, texture(1)).with_layer(0),
        ];
        sort_and_batch(&mut sprites, SortMode::Insertion);
        let ys: Vec<f32> = sprites.iter().map(|s| s.position.y).collect();
        assert_eq!(ys, vec![30.0, 20.0, 10.0], "层内该保持提交顺序");
    }

    #[test]
    fn an_empty_input_produces_no_batches() {
        let mut sprites: Vec<SpriteInstance> = Vec::new();
        assert!(sort_and_batch(&mut sprites, SortMode::YDescending).is_empty());
    }

    #[test]
    fn corners_go_counter_clockwise_from_bottom_left() {
        let instance = SpriteInstance::new(Vec2::new(10.0, 20.0), Vec2::new(4.0, 6.0), texture(1));
        let c = corners(&instance);
        assert_eq!(c[0], Vec2::new(10.0, 20.0));
        assert_eq!(c[1], Vec2::new(14.0, 20.0));
        assert_eq!(c[2], Vec2::new(14.0, 26.0));
        assert_eq!(c[3], Vec2::new(10.0, 26.0));
    }

    #[test]
    fn rotation_pivots_on_the_centre() {
        // 绕左下角转的话，改朝向会让精灵整个甩出去。
        let instance = SpriteInstance::new(Vec2::ZERO, Vec2::splat(2.0), texture(1))
            .with_rotation(std::f32::consts::FRAC_PI_2);
        let c = corners(&instance);
        let centre = c.iter().fold(Vec2::ZERO, |a, b| a + *b) / 4.0;
        assert!((centre - Vec2::ONE).length() < 1e-5, "中心跑了：{centre:?}");
    }

    #[test]
    fn rotation_preserves_the_size() {
        let instance =
            SpriteInstance::new(Vec2::ZERO, Vec2::new(4.0, 2.0), texture(1)).with_rotation(0.7);
        let c = corners(&instance);
        assert!(((c[1] - c[0]).length() - 4.0).abs() < 1e-4);
        assert!(((c[2] - c[1]).length() - 2.0).abs() < 1e-4);
    }

    #[test]
    fn culling_keeps_what_overlaps_the_view() {
        let sprites = vec![
            at(0.0, 0.0, texture(1)),
            at(100.0, 100.0, texture(1)),
            at(-0.5, -0.5, texture(1)),
        ];
        let visible = cull(&sprites, (Vec2::ZERO, Vec2::splat(10.0)));
        assert_eq!(visible.len(), 2, "边上压着的那个也该留下");
    }

    #[test]
    fn a_rotated_sprite_is_not_culled_by_its_unrotated_box() {
        // 按未旋转的矩形判的话，转 45° 的精灵在边缘会被误剔——
        // 屏幕边上的东西会突然消失。
        let sprite = SpriteInstance::new(Vec2::new(-0.7, 0.0), Vec2::splat(1.0), texture(1))
            .with_rotation(std::f32::consts::FRAC_PI_4);
        let visible = cull(&[sprite], (Vec2::ZERO, Vec2::splat(10.0)));
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn the_gpu_layout_matches_the_shader() {
        // 字段顺序或对齐错一位就是满屏乱码，而且不报任何错。
        assert_eq!(size_of::<GpuSprite>(), 64);
        // WGSL 的存储缓冲按 16 字节对齐。
        assert_eq!(size_of::<GpuSprite>() % 16, 0);

        let module = naga::front::wgsl::parse_str(crate::SPRITE2D_WGSL).expect("着色器应当能解析");
        let names: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"sprite_vs"));
        assert!(names.contains(&"sprite_fs"));
    }

    #[test]
    fn converting_to_gpu_keeps_the_region() {
        let instance = at(3.0, 4.0, texture(1))
            .with_region(SpriteRegion::new(
                Vec2::new(0.25, 0.5),
                Vec2::new(0.75, 1.0),
            ))
            .with_rotation(1.5);
        let gpu = GpuSprite::from(&instance);

        assert_eq!(gpu.position, [3.0, 4.0]);
        assert_eq!(gpu.uv, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(gpu.rotation, 1.5);
    }

    #[test]
    fn culling_drops_everything_when_the_view_is_elsewhere() {
        let sprites: Vec<SpriteInstance> = (0..10).map(|i| at(i as f32, 0.0, texture(1))).collect();
        let visible = cull(&sprites, (Vec2::splat(1000.0), Vec2::splat(1100.0)));
        assert!(visible.is_empty());
    }
}
