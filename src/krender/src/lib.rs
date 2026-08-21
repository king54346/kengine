//! krender —— wgpu 渲染后端。
//!
//! 遍历场景，剔除不可见对象，按材质绘制。
//! 这是整个引擎里**唯一**依赖 wgpu 的 crate。
//!
//! 绑定组划分：
//! - `group(0)`：每帧全局量（视图投影、相机位置、光照）
//! - `group(1)`：每个对象的变换与材质参数，用动态偏移在一个大缓冲里寻址
//! - `group(2)`：材质贴图与采样器，按材质缓存

#[cfg(test)]
mod cascade_batch_tests;
mod gizmo;
mod particle;
mod post;
mod sprite2d;
mod tonemap;
mod ui;

pub use post::PostSettings;
pub use tonemap::ToneMapping;

use gizmo::GizmoResources;
use kcamera::{Camera, Frustum};
use klight::{GpuLight, MAX_LIGHTS, shadow::ShadowSettings};
use kmesh::{MorphDelta, SkinVertex, Vertex};
use kparticle::GpuParticle;
use kscene::Scene;
use kui::Ui;
use particle::ParticleResources;
pub use post::AntiAlias;
use post::PostProcess;
use sprite2d::SpriteResources;
use ui::UiResources;

/// 顶点属性布局。字段顺序必须与 [`Vertex`] 及着色器的 `@location` 一致。
const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x3,
    2 => Float32x2,
    3 => Float32x3,
    4 => Float32x4,
];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRIBUTES,
    }
}

/// 蒙皮属性的布局。关节号用 `Uint16x4`——骨架不会有六万根骨头，
/// 用 u32 只是白白让每个顶点多背 8 个字节。
const SKIN_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
    5 => Uint16x4,
    6 => Float32x4,
];

fn skin_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<SkinVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &SKIN_ATTRIBUTES,
    }
}
use bytemuck::{Pod, Zeroable};
use fxhash::FxHashMap;
use kcore::uuid::Uuid;
use kmaterial::Material;
use kmath::{Mat4, Vec3};
use kpbr::GpuEnvironment;
use ktexture::{FilterMode, Texture, TextureFormat, WrapMode};
use std::{num::NonZeroU64, sync::Arc, time::Instant};
use wgpu::util::DeviceExt;
use winit::window::Window;

/// 每帧全局量，对应 `shader.wgsl` 的 `Globals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    camera_position: [f32; 4],
    /// rgb = 环境光贡献
    ambient: [f32; 4],
    /// x = 生效光源数，其余为对齐填充
    light_count: [u32; 4],
    /// 各级级联的光空间矩阵。用不满的级填单位阵。
    light_view_proj: [[[f32; 4]; 4]; klight::cascade::MAX_CASCADES],
    /// x/y/z = 前三级的远距离，w = 实际级数。
    cascade_splits: [f32; 4],
    /// x = 深度偏移，y = 法线偏移，z = 阴影贴图边长，w = 是否启用
    shadow_params: [f32; 4],
    /// x = 预滤波环境图的 mip 数（0 表示没有 HDR），其余保留
    ibl_params: [f32; 4],
    environment: GpuEnvironment,
    lights: [GpuLight; MAX_LIGHTS],
}

/// 阴影深度 pass 的全局量，对应 `shadow.wgsl` 的 `ShadowGlobals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowGlobals {
    light_view_proj: [[f32; 4]; 4],
    params: [f32; 4],
}

/// 阴影深度 pass 的每对象数据。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowObject {
    model: [[f32; 4]; 4],
    /// x = 骨骼矩阵起点，其余为对齐填充。深度 pass 也要蒙皮，
    /// 否则角色动起来了，影子还保持绑定姿态。
    skin: [u32; 4],
}

/// 天空 pass 的全局量，对应 `sky.wgsl` 的 `SkyGlobals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyGlobals {
    inverse_view_proj: [[f32; 4]; 4],
    camera_position: [f32; 4],
    /// x = 预滤波环境图的 mip 数（0 表示没有 HDR），其余保留
    ibl_params: [f32; 4],
    environment: GpuEnvironment,
}

/// 每个对象的数据，对应 `shader.wgsl` 的 `ObjectUniforms`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ObjectUniforms {
    model: [[f32; 4]; 4],
    normal_matrix: [[f32; 4]; 4],
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    normal_scale: f32,
    occlusion_strength: f32,
    emissive: [f32; 4],
    /// x = 本实例的骨骼矩阵在全局数组里的起点，其余为对齐填充。
    ///
    /// 蒙皮实例各有一套骨骼矩阵，但它们拼在同一个缓冲里，
    /// 靠这个偏移各取各的——于是同一个蒙皮网格的多个实例仍然能合批。
    skin: [u32; 4],
    /// 纹理坐标变换：xy = 缩放，zw = 偏移。
    ///
    /// 精灵图集靠它从一张大图里取出一格：整张图的 UV 是 0..1，
    /// 缩放到格子大小、再偏移到格子位置，就等于「只采样这一格」。
    uv_transform: [f32; 4],
    /// xyz = 反射探针的采集点，w = 预滤波纹理数组的层号。
    ///
    /// w = 0 表示这个对象没有探针管，用第 0 层（全局环境）。
    probe_position: [f32; 4],
    /// xyz = 视差盒最小角，w = 是否做视差校正（>0.5 为是）。
    probe_min: [f32; 4],
    /// xyz = 视差盒最大角，w = 反射强度。
    probe_max: [f32; 4],
}

/// 从材质里取出纹理坐标变换：`[缩放x, 缩放y, 偏移x, 偏移y]`。
///
/// 没设过就是恒等变换 `[1, 1, 0, 0]`——普通模型完全不受这套机制影响。
/// 类型不对（比如有人把 `uv_scale` 设成了 `Float`）也退回恒等，
/// 而不是把 UV 变成一堆垃圾值：一处写错不该让整个模型的贴图全乱。
fn uv_transform_of(material: &kmaterial::Material) -> [f32; 4] {
    fn vec2(material: &kmaterial::Material, name: &str, fallback: kmath::Vec2) -> kmath::Vec2 {
        match material.get(name) {
            Some(kmaterial::MaterialValue::Vec2(v)) => *v,
            _ => fallback,
        }
    }

    let scale = vec2(material, kpbr::standard::UV_SCALE, kmath::Vec2::ONE);
    let offset = vec2(material, kpbr::standard::UV_OFFSET, kmath::Vec2::ZERO);
    [scale.x, scale.y, offset.x, offset.y]
}

/// 已上传显存的网格。
struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    /// 蒙皮属性，作为第二个顶点缓冲。静态网格没有。
    skin_buffer: Option<wgpu::Buffer>,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    /// 本网格的形变增量在全局形变缓冲中的起点。
    morph_offset: u32,
    /// 形变目标数量，0 表示没有形变。
    morph_count: u32,
}

/// 本帧一个待绘制对象。
struct DrawCall {
    mesh_id: Uuid,
    /// 材质贴图绑定组的缓存键（五张贴图 id 的组合）。
    texture_key: [Uuid; 5],
    /// 是否走蒙皮管线。蒙皮与静态的顶点布局不同，不能混在一批里。
    skinned: bool,
    /// 到相机的距离平方，半透明物体按它从远到近排序。
    ///
    /// 存平方而不是距离：只用来比大小，开方是白花的。
    depth: f32,
    /// 世界空间包围盒，阴影的逐级剔除要用。
    aabb: kmath::Aabb,
    uniforms: ObjectUniforms,
}

/// 一批网格与贴图都相同、可以合并成一次绘制的对象。
///
/// 每个对象的变换与材质参数各不相同没关系——那些数据在存储缓冲里，
/// 着色器按 `instance_index` 取，一次 `draw_indexed` 就能画完整批。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Batch {
    mesh_id: Uuid,
    texture_key: [Uuid; 5],
    /// 是否走蒙皮管线。
    skinned: bool,
    /// 本批第一个实例在存储缓冲中的下标。
    first: u32,
    /// 实例数量。
    count: u32,
}

/// 一帧的渲染统计。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    /// 实际提交绘制的对象数。
    pub drawn: u32,
    /// 被视锥剔除掉的对象数。
    pub culled: u32,
    /// 实际绘制的三角形数。
    pub triangles: u32,
    /// 实际提交的绘制调用数。批处理生效时会明显小于 [`drawn`](Self::drawn)。
    pub draw_calls: u32,
    /// 本帧绘制的粒子数。
    pub particles: u32,
    /// 本帧的调试线顶点数，两个一组构成一条线段。
    pub gizmo_vertices: u32,
    /// 本帧的 UI 顶点数。
    pub ui_vertices: u32,
    /// 本帧走 2D 批处理管线的精灵数。
    pub sprites: u32,
    /// 阴影 pass 提交的绘制调用数（所有级联加起来）。
    ///
    /// 和 [`draw_calls`](Self::draw_calls) 对比能看出逐级剔除的效果：
    /// 不剔除的话它约等于 `draw_calls × 级数`。
    pub shadow_draw_calls: u32,
    /// 视锥剔除耗时（微秒）。
    pub cull_micros: u32,
    /// CPU 端准备一帧的总耗时（微秒）：剔除 + 收集 + 分批 + 上传。
    pub prepare_micros: u32,
}

impl RenderStats {
    /// 本帧参与判定的对象总数。
    pub fn total(&self) -> u32 {
        self.drawn + self.culled
    }

    /// 平均每次绘制调用画了多少个对象。批处理没生效时是 1。
    pub fn instances_per_draw(&self) -> f32 {
        if self.draw_calls == 0 {
            0.0
        } else {
            self.drawn as f32 / self.draw_calls as f32
        }
    }
}

/// 把绘制项按「网格 + 贴图」分批，并按批次顺序输出实例数组。
///
/// 排序的是下标而不是绘制项本身：`DrawCall` 有两百多字节，
/// 上万个对象直接排序光是搬运数据就很可观，排下标只搬 4 字节。
///
/// 排序会打乱原本的提交顺序。不透明物体有深度测试兜底，顺序无所谓；
/// 将来加半透明时，那部分必须单独走一条按深度排序的路径。
fn build_batches(
    draws: &[DrawCall],
    instances: &mut Vec<ObjectUniforms>,
    bounds: &mut Vec<kmath::Aabb>,
) -> Vec<Batch> {
    build_batches_into(draws, instances, bounds, true)
}

/// 按每个实例的包围盒，把一批拆成「本级要画的连续段」。
///
/// 实例下标是全局的、和主 pass 共用一个数组，所以**不能重排也不能压缩**——
/// 只能把批次切成若干段，跳过被剔掉的那些。
///
/// 同一批里的实例通常在空间上是散开的（同一个网格的多个实例），
/// 所以切出来的段数可能不少；但每段仍是一次实例化绘制，
/// 比逐个提交好得多。
fn cascade_batches(
    batches: &[Batch],
    bounds: &[kmath::Aabb],
    matrix: kmath::Mat4,
    resolution: u32,
    min_texels: f32,
) -> Vec<Batch> {
    let mut out: Vec<Batch> = Vec::with_capacity(batches.len());
    for batch in batches {
        let mut run: Option<Batch> = None;
        for offset in 0..batch.count {
            let index = (batch.first + offset) as usize;
            let visible = bounds.get(index).is_some_and(|aabb| {
                klight::cascade::shadow_visibility(matrix, *aabb, resolution, min_texels)
            });

            match (visible, run.as_mut()) {
                // 接着上一段。
                (true, Some(current)) => current.count += 1,
                // 开一段新的。
                (true, None) => {
                    run = Some(Batch {
                        first: batch.first + offset,
                        count: 1,
                        ..*batch
                    })
                }
                // 断了，收尾。
                (false, Some(_)) => out.push(run.take().expect("刚判过是 Some")),
                (false, None) => {}
            }
        }
        if let Some(current) = run {
            out.push(current);
        }
    }
    out
}

/// 半透明物体的批次。
///
/// 和不透明的关键区别：**顺序不能动**。不透明物体按网格和贴图重排能把
/// 绘制调用降到最少，反正谁先画结果都一样；半透明物体一旦重排，
/// 混合结果就错了——远处的东西画在近处的上面。
///
/// 所以这里先按距离从远到近排，再**只合并相邻的**同网格同贴图项。
/// 合并率会低很多，但那是正确性的代价。
fn build_transparent_batches(
    draws: &mut [DrawCall],
    instances: &mut Vec<ObjectUniforms>,
    bounds: &mut Vec<kmath::Aabb>,
) -> Vec<Batch> {
    // 从远到近。`total_cmp` 而不是 `partial_cmp().unwrap()`：
    // 退化的变换会算出 NaN 距离，unwrap 会直接崩掉整帧。
    draws.sort_by(|a, b| b.depth.total_cmp(&a.depth));
    build_batches_into(draws, instances, bounds, false)
}

/// `reorder` 为真时按网格/贴图重排以最大化合并；为假时保持传入顺序。
fn build_batches_into(
    draws: &[DrawCall],
    instances: &mut Vec<ObjectUniforms>,
    bounds: &mut Vec<kmath::Aabb>,
    reorder: bool,
) -> Vec<Batch> {
    if !reorder {
        let mut batches: Vec<Batch> = Vec::new();
        for draw in draws {
            instances.push(draw.uniforms);
            // 和 `instances` 一一对齐：阴影逐级剔除按实例下标回查它。
            bounds.push(draw.aabb);
            match batches.last_mut() {
                Some(last)
                    if last.mesh_id == draw.mesh_id
                        && last.texture_key == draw.texture_key
                        && last.skinned == draw.skinned =>
                {
                    last.count += 1;
                }
                _ => batches.push(Batch {
                    mesh_id: draw.mesh_id,
                    texture_key: draw.texture_key,
                    skinned: draw.skinned,
                    first: instances.len() as u32 - 1,
                    count: 1,
                }),
            }
        }
        return batches;
    }
    build_opaque_batches(draws, instances, bounds)
}

fn build_opaque_batches(
    draws: &[DrawCall],
    instances: &mut Vec<ObjectUniforms>,
    bounds: &mut Vec<kmath::Aabb>,
) -> Vec<Batch> {
    let mut order: Vec<u32> = (0..draws.len() as u32).collect();
    order.sort_unstable_by(|&a, &b| {
        let (a, b) = (&draws[a as usize], &draws[b as usize]);
        // 蒙皮排在最前：它决定用哪条管线，比网格和贴图更「贵」，
        // 先按它分开能把管线切换降到一次。
        a.skinned
            .cmp(&b.skinned)
            .then_with(|| a.mesh_id.cmp(&b.mesh_id))
            .then_with(|| a.texture_key.cmp(&b.texture_key))
    });

    instances.reserve(draws.len());

    let mut batches: Vec<Batch> = Vec::new();
    for &index in &order {
        let draw = &draws[index as usize];
        instances.push(draw.uniforms);
        bounds.push(draw.aabb);
        match batches.last_mut() {
            // 排序保证同一批的对象连续出现，所以只用跟上一批比。
            Some(last)
                if last.mesh_id == draw.mesh_id
                    && last.texture_key == draw.texture_key
                    && last.skinned == draw.skinned =>
            {
                last.count += 1;
            }
            _ => batches.push(Batch {
                mesh_id: draw.mesh_id,
                texture_key: draw.texture_key,
                skinned: draw.skinned,
                first: instances.len() as u32 - 1,
                count: 1,
            }),
        }
    }
    batches
}

/// 一帧的绘制结果，供事件循环决定后续动作。
pub enum RenderOutcome {
    /// 正常绘制完成。
    Ok,
    /// 本帧跳过（窗口被遮挡等），无需处理。
    Skip,
    /// 表面失效，需要重新配置。
    Reconfigure,
    /// 不可恢复的错误，应退出。
    Fatal,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    pipeline: wgpu::RenderPipeline,
    depth_view: wgpu::TextureView,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    object_layout: wgpu::BindGroupLayout,
    object_buffer: wgpu::Buffer,
    object_bind_group: wgpu::BindGroup,
    /// 当前对象缓冲能容纳的实例数。
    object_capacity: u64,
    /// 蒙皮管线。顶点布局多一路，只能单独开一条。
    skinned_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    skinned_transparent_pipeline: wgpu::RenderPipeline,
    /// 所有蒙皮实例的骨骼矩阵，拼在一个缓冲里。
    joint_buffer: wgpu::Buffer,
    joint_capacity: u64,
    /// 逐帧复用的骨骼矩阵暂存区。
    joint_scratch: Vec<[[f32; 4]; 4]>,
    /// 所有带形变的网格的增量，按顶点优先排列，只增不减。
    morph_buffer: wgpu::Buffer,
    morph_capacity: u64,
    /// 已经写进形变缓冲的元素数，新网格从这里往后追加。
    morph_used: u64,
    /// 每实例的形变权重，逐帧重写。
    morph_weight_buffer: wgpu::Buffer,
    morph_weight_capacity: u64,
    /// 逐帧复用的权重暂存区。
    morph_weight_scratch: Vec<f32>,

    /// 环境 BRDF 查找表，启动时在 CPU 上算好后上传。
    brdf_bind_group: wgpu::BindGroup,

    /// 阴影相关资源。
    shadow: ShadowResources,
    /// 后处理链：主 pass 先画到 HDR 目标，再经 Bloom 与色调映射输出到屏幕。
    post: PostProcess,
    /// 粒子 pass 的资源。
    particles: ParticleResources,
    /// 逐帧复用的粒子暂存区。
    particle_scratch: Vec<GpuParticle>,
    /// 调试线 pass 的资源。
    gizmos: GizmoResources,
    /// 2D 精灵 pass 的资源。
    sprites: SpriteResources,
    /// 逐帧复用的精灵暂存区。
    sprite_scratch: Vec<kscene::SpriteInstance>,
    /// UI pass 的资源。
    ui: UiResources,
    /// 上一次上传的字形图集版本号。
    ui_atlas_version: u64,
    /// 预滤波环境图的 mip 数。0 表示没有 HDR，着色器会退回程序化天空。
    environment_mips: usize,
    /// 上一次上传的环境图版本号。
    environment_version: u64,
    /// 重建 group(3) 绑定组时要用。
    brdf_layout: wgpu::BindGroupLayout,
    /// 重建天空绑定组时要用。
    sky_layout: wgpu::BindGroupLayout,

    sky_pipeline: wgpu::RenderPipeline,
    sky_buffer: wgpu::Buffer,
    sky_bind_group: wgpu::BindGroup,

    texture_layout: wgpu::BindGroupLayout,
    /// 已上传的单张贴图，键为 [`ktexture::Texture::id`]。
    gpu_textures: FxHashMap<Uuid, GpuTexture>,
    /// 材质贴图绑定组，键是五张贴图 id 的组合。
    ///
    /// 用组合而非材质 id 作键，是为了让异步加载中的贴图就绪后自动换上——
    /// 贴图 id 一变，键就变，会重新建一个绑定组。
    material_bind_groups: FxHashMap<[Uuid; 5], wgpu::BindGroup>,
    /// 材质缺某张贴图时顶上的中性贴图。
    default_textures: DefaultTextures,
    meshes: FxHashMap<Uuid, GpuMesh>,
    stats: RenderStats,
}

/// 一张已上传的贴图连同它自己的采样器。
///
/// 采样设置属于贴图而非材质——像素风贴图要最近邻，普通贴图要线性，
/// 用一个共用采样器会让这些设置全部失效。
pub(crate) struct GpuTexture {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// 材质缺贴图时使用的中性贴图。
///
/// 让着色器保持单一代码路径：与其在着色器里分支判断"有没有贴图"，
/// 不如绑一张不改变结果的贴图上去。
struct DefaultTextures {
    /// 全白：基础色、金属度粗糙度、遮蔽、自发光的中性值。
    white: GpuTexture,
    /// (0.5, 0.5, 1.0)：切线空间里指向正上方，即"不扰动"。
    flat_normal: GpuTexture,
}

/// 每级级联的全局量在缓冲里占多大一段。
///
/// `ShadowGlobals` 只有 80 字节，但动态偏移必须是
/// `min_uniform_buffer_offset_alignment` 的倍数——各家硬件普遍是 256，
/// WebGPU 的下限保证也是 256，直接按它对齐最省事。
const SHADOW_GLOBALS_STRIDE: u64 = 256;

/// 阴影 pass 所需的一组 GPU 资源。
struct ShadowResources {
    settings: ShadowSettings,
    /// 级联参数。
    cascades: klight::cascade::CascadeSettings,
    pipeline: wgpu::RenderPipeline,
    /// 整个数组的视图，给主着色器采样。
    depth_view: wgpu::TextureView,
    /// 每层一个视图，渲染时当深度附件。
    layer_views: Vec<wgpu::TextureView>,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    object_buffer: wgpu::Buffer,
    object_bind_group: wgpu::BindGroup,
    object_capacity: u64,
    /// 蒙皮深度管线。
    skinned_pipeline: wgpu::RenderPipeline,
    joint_buffer: wgpu::Buffer,
    joint_capacity: u64,
    morph_buffer: wgpu::Buffer,
    morph_capacity: u64,
    morph_weight_buffer: wgpu::Buffer,
    morph_weight_capacity: u64,
}

impl Renderer {
    /// 初始容量：够画 256 个物体，不够时自动翻倍。
    const INITIAL_CAPACITY: u64 = 256;
    /// 骨骼矩阵的初始容量，够放几个中等骨架。
    const INITIAL_JOINTS: u64 = 256;
    /// 形变增量的初始容量。一上来就给大一点：一个带形变的网格
    /// 动辄就是「顶点数 × 目标数」个元素。
    const INITIAL_MORPH: u64 = 4096;

    /// 创建渲染器并配置交换链。
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance.create_surface(window.clone()).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kengine device"),
                ..Default::default()
            })
            .await
            .unwrap();

        let mut config = surface
            .get_default_config(&adapter, size.width, size.height)
            .unwrap();
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let depth_view = Self::create_depth_view(&device, &config);

        // 交换链非 sRGB 时硬件不会做线性→sRGB 转换，画面会明显偏暗。
        if !config.format.is_srgb() {
            klog::warn!("交换链格式 {:?} 不是 sRGB，画面可能偏暗", config.format);
        }

        // PBR 的 BRDF 函数由 kpbr 提供，拼在标准着色器前面一起编译。
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine standard shader"),
            source: wgpu::ShaderSource::Wgsl(standard_shader_source().into()),
        });

        // ── group(0)：每帧全局量 ──
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<Globals>() as u64),
                },
                count: None,
            }],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine globals buffer"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        // ── group(1)：每个实例一份的变换与材质参数 ──
        // 用存储缓冲而非「uniform + 动态偏移」：后者每个对象都要重新绑一次绑定组，
        // 也就注定了一个对象一次绘制调用；存储缓冲让着色器自己按实例号取数据，
        // 同网格同贴图的对象因此能合并成一次绘制。
        let object_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine object layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<ObjectUniforms>() as u64),
                    },
                    count: None,
                },
                // 骨骼矩阵与对象数据同组：绑定组上限是 4，已经用满了
                // （全局 / 对象 / 材质贴图 / BRDF+阴影），只能挤进来。
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<[[f32; 4]; 4]>() as u64),
                    },
                    count: None,
                },
                // 形变增量与形变权重。同样只能挤在 group(1) 里：绑定组上限是 4。
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<MorphDelta>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<f32>() as u64),
                    },
                    count: None,
                },
            ],
        });

        let joint_buffer = create_joint_storage(&device, Self::INITIAL_JOINTS);
        let morph_buffer = create_morph_storage(&device, Self::INITIAL_MORPH);
        let morph_weight_buffer = create_morph_weight_storage(&device, Self::INITIAL_CAPACITY);
        let (object_buffer, object_bind_group) = Self::create_object_storage(
            &device,
            &object_layout,
            Self::INITIAL_CAPACITY,
            &joint_buffer,
            &morph_buffer,
            &morph_weight_buffer,
        );

        // ── group(2)：材质贴图 ──
        // 材质贴图：基础色 / 法线 / 金属度粗糙度 / 遮蔽 / 自发光，共用一个采样器。
        let mut texture_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ];
        for binding in 2..=5 {
            texture_entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
        }
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine texture layout"),
            entries: &texture_entries,
        });

        // ── group(3)：环境 BRDF 查找表 ──
        let brdf_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine brdf layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        // 级联：每级一层。
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                // 预滤波的 HDR 环境图。没有 HDR 时绑一张 1×1 的占位——
                // wgpu 不允许绑定组留空，而着色器靠 `ibl_params.x` 跳过采样。
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        // 数组维度：第 0 层是全局环境，之后每个反射探针一层。
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // 阴影贴图与 BRDF LUT 同属 group(3)，需要等阴影资源建好后一起绑定。

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine pipeline layout"),
            bind_group_layouts: &[
                Option::from(&globals_layout),
                Option::from(&object_layout),
                Option::from(&texture_layout),
                Option::from(&brdf_layout),
            ],
            immediate_size: 0,
        });

        // 静态与蒙皮各一条管线：两者的顶点布局不同（蒙皮多一路顶点缓冲），
        // 而顶点布局是管线状态的一部分，没法在一条管线里切换。
        let pipeline = create_standard_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_main",
            &[Option::from(vertex_layout())],
            "kengine render pipeline",
            kmaterial::BlendMode::Opaque,
        );
        let skinned_pipeline = create_standard_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_skinned",
            &[Option::from(vertex_layout()), Option::from(skin_layout())],
            "kengine skinned pipeline",
            kmaterial::BlendMode::Opaque,
        );
        // 半透明版本：只有混合方式和深度写入不同，着色器完全一样。
        let transparent_pipeline = create_standard_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_main",
            &[Option::from(vertex_layout())],
            "kengine transparent pipeline",
            kmaterial::BlendMode::Alpha,
        );
        let skinned_transparent_pipeline = create_standard_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_skinned",
            &[Option::from(vertex_layout()), Option::from(skin_layout())],
            "kengine skinned transparent pipeline",
            kmaterial::BlendMode::Alpha,
        );

        // ── 阴影 pass ──
        let shadow = create_shadow_resources(&device, ShadowSettings::default());

        // ── 天空 pass ──
        // 还没有 HDR 时先绑一张 1×1 的占位；`set_environment_hdr` 之后
        // 会重建这些绑定组。主 pass 与天空 pass 共用它。
        let placeholder_environment = create_placeholder_environment(&device);

        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine sky shader"),
            source: wgpu::ShaderSource::Wgsl(sky_shader_source().into()),
        });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine sky layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<SkyGlobals>() as u64),
                    },
                    count: None,
                },
                // 和主 pass 共用同一张预滤波环境图。
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        // 数组维度：第 0 层全局环境，之后每个反射探针一层。
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sky_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine sky buffer"),
            size: size_of::<SkyGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sky_bind_group =
            create_sky_bind_group(&device, &sky_layout, &sky_buffer, &placeholder_environment);
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine sky pipeline layout"),
            bind_group_layouts: &[Option::from(&sky_layout)],
            immediate_size: 0,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine sky pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("sky_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("sky_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    // 天空与物体画在同一张 HDR 目标上。
                    format: post::HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // 全屏三角形不做剔除，绕序无所谓。
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                // 天空不写深度，只填充没有物体的地方。
                depth_write_enabled: Option::from(false),
                depth_compare: Option::from(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let default_textures = DefaultTextures {
            white: upload_texture(&device, &queue, &Texture::white()),
            flat_normal: upload_texture(
                &device,
                &queue,
                // 法线贴图是数据不是颜色，必须走线性格式。
                &Texture::solid(1, 1, [128, 128, 255, 255])
                    .with_format(ktexture::TextureFormat::Linear),
            ),
        };

        let brdf_bind_group = create_brdf_lut(
            &device,
            &queue,
            &brdf_layout,
            &shadow.depth_view,
            &placeholder_environment,
        );
        let post = PostProcess::new(&device, config.width, config.height, config.format);
        // 粒子画在主 pass 里，因此目标格式与深度格式都要与主 pass 一致。
        let particles = ParticleResources::new(
            &device,
            &queue,
            post::HDR_FORMAT,
            wgpu::TextureFormat::Depth32Float,
        );
        // 调试线同样画在主 pass 里，格式必须一致。
        let gizmos =
            GizmoResources::new(&device, post::HDR_FORMAT, wgpu::TextureFormat::Depth32Float);
        // 2D 精灵画在主 pass 里，格式与主 pass 一致。
        let sprite_resources =
            SpriteResources::new(&device, post::HDR_FORMAT, wgpu::TextureFormat::Depth32Float);
        // UI 画在后处理**之后**，目标是交换链，所以用交换链的格式。
        let ui_resources = UiResources::new(&device, config.format);

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            size,
            pipeline,
            depth_view,
            globals_buffer,
            globals_bind_group,
            object_layout,
            object_buffer,
            object_bind_group,
            object_capacity: Self::INITIAL_CAPACITY,
            skinned_pipeline,
            transparent_pipeline,
            skinned_transparent_pipeline,
            joint_buffer,
            joint_capacity: Self::INITIAL_JOINTS,
            joint_scratch: Vec::new(),
            morph_buffer,
            morph_capacity: Self::INITIAL_MORPH,
            morph_used: 0,
            morph_weight_buffer,
            morph_weight_capacity: Self::INITIAL_CAPACITY,
            morph_weight_scratch: Vec::new(),
            brdf_bind_group,
            shadow,
            post,
            particles,
            particle_scratch: Vec::new(),
            gizmos,
            sprites: sprite_resources,
            sprite_scratch: Vec::new(),
            ui: ui_resources,
            ui_atlas_version: u64::MAX,
            environment_mips: 0,
            environment_version: 0,
            brdf_layout,
            sky_layout,
            sky_pipeline,
            sky_buffer,
            sky_bind_group,
            texture_layout,
            gpu_textures: FxHashMap::default(),
            material_bind_groups: FxHashMap::default(),
            default_textures,
            meshes: FxHashMap::default(),
            stats: RenderStats::default(),
        };

        // 粒子建的时候只有一张 1×1 占位深度，这里换成真的。
        renderer
            .particles
            .set_depth_view(&renderer.device, &renderer.depth_view);
        renderer
    }

    /// 登记一张给 UI 用的贴图。
    ///
    /// `DrawList::image` 只带一个 id，渲染器得先见过这张贴图才画得出来；
    /// 没登记过的批次会被**跳过**（而不是用字形图集顶替——那会在界面上
    /// 印出一片字形）。
    pub fn register_ui_texture(&mut self, texture: &ktexture::Texture) {
        self.ui.upload_image(&self.device, &self.queue, texture);
    }

    /// 当前渲染目标尺寸。
    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    /// 上一帧的渲染统计。
    /// 后处理设置。
    /// 软粒子的淡出距离（世界单位），0 表示关闭。
    ///
    /// 粒子是个方片，插进地面时会露出一条**笔直的交线**——一眼就能看出
    /// 它是张纸。开着之后粒子越接近背后的几何就越透明，交线被抹掉。
    ///
    /// 调大让过渡更柔和，但整团粒子会整体变淡。烟雾用 0.5~2 米，
    /// 火花这类本来就该有硬边的可以关掉。
    ///
    /// **正交相机下自动失效**——深度反解的公式只对透视投影成立。
    pub fn soft_particle_fade(&self) -> f32 {
        self.particles.soft_fade
    }

    /// 设置软粒子的淡出距离。
    pub fn set_soft_particle_fade(&mut self, distance: f32) {
        self.particles.soft_fade = distance.max(0.0);
    }

    pub fn post_settings(&self) -> PostSettings {
        self.post.settings()
    }

    /// 修改后处理设置。
    pub fn set_post_settings(&mut self, settings: PostSettings) {
        self.post.set_settings(settings);
    }

    /// 上一帧的渲染统计。
    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// 重新配置交换链与深度缓冲。
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = Self::create_depth_view(&self.device, &self.config);
        // 软粒子采样的就是这张纹理。不换绑定组的话，改窗口大小之后
        // 粒子会按旧尺寸的深度去淡出——表现为淡出边界整体错位。
        self.particles
            .set_depth_view(&self.device, &self.depth_view);
        self.post
            .resize(&self.device, new_size.width, new_size.height);
    }

    /// 绘制一帧。
    pub fn render(&mut self, scene: &Scene, ui: &Ui) -> RenderOutcome {
        // 相机：取场景里第一个启用的；没有就用一个看向原点的默认视角。
        let (camera_to_world, camera) = scene.active_camera().unwrap_or_else(|| {
            let eye = Vec3::new(0.0, 1.5, 3.0);
            (
                Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y).inverse(),
                Camera::default(),
            )
        });

        let view = camera_to_world.inverse();
        let camera_position = camera_to_world.to_scale_rotation_translation().2;
        let aspect = self.config.width as f32 / self.config.height.max(1) as f32;
        let view_proj = camera.projection_matrix(aspect) * view;

        // 收集光源，超出容量的部分丢弃并告警。
        //
        // 投射阴影的光源必须占据 index 0——着色器只对首个光源做阴影判定，
        // 顺序错了会导致阴影套在错误的光源上。
        let shadow_caster = scene.shadow_caster();
        let mut lights = [GpuLight::default(); MAX_LIGHTS];
        let mut light_count = 0usize;

        if let Some((light, transform)) = shadow_caster {
            lights[0] = light.to_gpu(transform);
            light_count = 1;
        }

        let mut caster_skipped = false;
        for (light, transform) in scene.visible_lights() {
            // 跳过已放在首位的那一盏；后续同样标记了投影的光源按普通光源处理。
            if light.cast_shadows && shadow_caster.is_some() && !caster_skipped {
                caster_skipped = true;
                continue;
            }
            if light_count >= MAX_LIGHTS {
                klog::once!(klog::warn!("场景光源超过上限 {MAX_LIGHTS}，多余的已被忽略"));
                break;
            }
            lights[light_count] = light.to_gpu(transform);
            light_count += 1;
        }

        // ── HDR 环境图 ──
        // 只在版本号变了时重传：一条 256×128 的 mip 链是几兆的浮点数据，
        // 每帧重传纯属浪费，而它只在换环境图时才变。
        if scene.environment_version() != self.environment_version {
            self.environment_version = scene.environment_version();
            let probe_levels: Vec<&[kpbr::prefilter::PrefilteredLevel]> = scene
                .reflection_probes()
                .iter()
                .map(|entry| entry.levels.as_slice())
                .collect();
            let uploaded = scene.prefiltered_environment().and_then(|levels| {
                upload_prefiltered_environment(&self.device, &self.queue, levels, &probe_levels)
                    .map(|view| (view, levels.len()))
            });

            let (view, mips) = match uploaded {
                Some((view, mips)) => (view, mips),
                // 换回程序化天空：绑占位图，着色器靠 `ibl_params.x == 0`
                // 跳过采样。
                None => (create_placeholder_environment(&self.device), 0),
            };
            self.environment_mips = mips;
            self.brdf_bind_group = create_brdf_lut(
                &self.device,
                &self.queue,
                &self.brdf_layout,
                &self.shadow.depth_view,
                &view,
            );
            // 天空 pass 也要跟着换：不换的话反射来自新 HDR、
            // 天上还是旧的那张，两者对不上。
            self.sky_bind_group =
                create_sky_bind_group(&self.device, &self.sky_layout, &self.sky_buffer, &view);
        }

        // ── 级联阴影 ──
        // 把视锥按距离切段，每段一张阴影图。近处那段覆盖的世界范围小，
        // 同样分辨率下纹素密度高一个数量级。
        let cascades = match shadow_caster {
            Some((light, transform)) => klight::cascade::compute(
                view_proj,
                light.direction(transform),
                scene.visible_bounds(),
                self.shadow.cascades,
            ),
            None => Vec::new(),
        };

        let mut light_view_proj =
            [Mat4::IDENTITY.to_cols_array_2d(); klight::cascade::MAX_CASCADES];
        // 切分距离交给着色器选级联。用不满的级填一个极大值，
        // 免得着色器选到没渲染过的层——那是一片未初始化的噪点。
        let mut cascade_splits = [f32::MAX; 4];
        for (index, cascade) in cascades.iter().enumerate() {
            light_view_proj[index] = cascade.matrix.to_cols_array_2d();
            if index < 3 {
                cascade_splits[index] = cascade.far;
            }
        }
        cascade_splits[3] = cascades.len() as f32;
        let shadow_enabled = !cascades.is_empty() && light_count > 0;
        let settings = self.shadow.settings;

        self.queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[Globals {
                view_proj: view_proj.to_cols_array_2d(),
                camera_position: camera_position.extend(1.0).to_array(),
                ambient: [0.0; 4],
                light_count: [light_count as u32, 0, 0, 0],
                light_view_proj,
                cascade_splits,
                ibl_params: [self.environment_mips as f32, 0.0, 0.0, 0.0],
                shadow_params: [
                    settings.depth_bias,
                    settings.normal_bias,
                    settings.resolution.max(256) as f32,
                    if shadow_enabled { 1.0 } else { 0.0 },
                ],
                environment: scene.environment().to_gpu(),
                lights,
            }]),
        );

        // ── 剔除 ──
        // 视锥来自本帧的视图投影矩阵；实际判定由场景图的 BVH 完成，
        // 对象多时它还会自动切到并行分片。
        let prepare_start = Instant::now();
        let frustum = camera
            .frustum_culling
            .then(|| Frustum::from_view_projection(view_proj));

        let visible: Vec<_> = match &frustum {
            Some(frustum) => scene.cull(frustum),
            None => scene.visible_meshes().collect(),
        };
        let cull_micros = prepare_start.elapsed().as_micros() as u32;

        let mut stats = RenderStats {
            drawn: visible.len() as u32,
            culled: (scene.drawable_count() - visible.len()) as u32,
            cull_micros,
            ..RenderStats::default()
        };

        // 收集绘制项，顺便把没上传过的网格与贴图传到显存。
        // 标准材质建一次就够：它内部是带 String 键的哈希表，
        // 放在循环里等于每个对象都重新分配一遍。
        let default_material = Material::standard();
        // 探针参数拿出来一份：`select` 要一个连续切片，而场景里
        // 存的是带像素的条目。
        let probe_params: Vec<kpbr::probe::ReflectionProbe> = scene
            .reflection_probes()
            .iter()
            .map(|entry| entry.probe)
            .collect();
        let mut draws = Vec::with_capacity(visible.len());
        // 半透明的单独收：它们要按距离排序，混不进不透明的批次里。
        let mut transparent_draws: Vec<DrawCall> = Vec::new();
        // 所有蒙皮实例的骨骼矩阵拼进同一个数组，各实例记下自己的起点。
        let mut joints = std::mem::take(&mut self.joint_scratch);
        joints.clear();
        let mut morph_weights = std::mem::take(&mut self.morph_weight_scratch);
        morph_weights.clear();
        for item in visible {
            stats.triangles += item.mesh.triangle_count() as u32;

            let mesh = item.mesh;
            if !self.meshes.contains_key(&mesh.id()) {
                // 形变增量是随网格一次性上传的静态数据，追加到全局缓冲末尾。
                let (morph_offset, morph_count) = self.upload_morph_targets(mesh);
                let gpu_mesh = GpuMesh {
                    vertex_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine vertex buffer"),
                            contents: bytemuck::cast_slice(mesh.vertices()),
                            usage: wgpu::BufferUsages::VERTEX,
                        },
                    ),
                    // 蒙皮属性单独一路顶点缓冲，静态网格没有这一路。
                    skin_buffer: mesh.skin().map(|skin| {
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("kengine skin buffer"),
                                contents: bytemuck::cast_slice(skin),
                                usage: wgpu::BufferUsages::VERTEX,
                            })
                    }),
                    index_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine index buffer"),
                            contents: bytemuck::cast_slice(mesh.indices()),
                            usage: wgpu::BufferUsages::INDEX,
                        },
                    ),
                    index_count: mesh.index_count(),
                    morph_offset,
                    morph_count,
                };
                self.meshes.insert(mesh.id(), gpu_mesh);
            }

            let material = item.material.unwrap_or(&default_material);
            let texture_key = self.ensure_material_textures(material);

            // 形变权重逐实例写进权重缓冲：同一个网格的两个实例可以有不同的表情。
            let morph = self
                .meshes
                .get(&mesh.id())
                .map(|gpu| (gpu.morph_offset, gpu.morph_count))
                .unwrap_or((0, 0));
            let weight_offset = morph_weights.len() as u32;
            if morph.1 > 0 {
                morph_weights.extend(
                    (0..morph.1 as usize)
                        .map(|index| item.morph_weights.get(index).copied().unwrap_or(0.0)),
                );
            }

            // 只有网格自己也带蒙皮属性时才走蒙皮管线：
            // 骨架挂在没有蒙皮顶点的网格上是导入出的错，按静态画至少不会崩。
            let skin_offset = match item.skin.filter(|_| mesh.is_skinned()) {
                Some(matrices) => {
                    let offset = joints.len() as u32;
                    joints.extend(matrices.iter().map(|m| m.to_cols_array_2d()));
                    Some(offset)
                }
                None => None,
            };

            // 逐对象选探针，用包围盒中心。横跨两个房间的大物体只能
            // 用一个探针——前向渲染的常规取舍，办法是把大物体拆开。
            let (probe_position, probe_min, probe_max) =
                match kpbr::probe::select(&probe_params, item.aabb.center()) {
                    Some(index) => {
                        let probe = &probe_params[index];
                        (
                            // 层号 +1：第 0 层是全局环境。
                            probe.position.extend((index + 1) as f32).to_array(),
                            probe
                                .bounds
                                .min
                                .extend(if probe.parallax { 1.0 } else { 0.0 })
                                .to_array(),
                            probe.bounds.max.extend(probe.intensity).to_array(),
                        )
                    }
                    // 没探针管它：层号 0（全局环境）、不做视差、强度 1。
                    None => ([0.0; 4], [0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]),
                };

            let model = item.transform;
            // 用包围盒中心而不是变换的平移：蒙皮网格的变换是单位阵，
            // 拿平移排序的话所有角色都会被当成在原点。
            let depth = (item.aabb.center() - camera_position).length_squared();
            let target = if material.blend_mode() == kmaterial::BlendMode::Alpha {
                &mut transparent_draws
            } else {
                &mut draws
            };
            target.push(DrawCall {
                mesh_id: mesh.id(),
                texture_key,
                skinned: skin_offset.is_some(),
                depth,
                aabb: item.aabb,
                uniforms: ObjectUniforms {
                    model: model.to_cols_array_2d(),
                    // 逆转置，保证非均匀缩放下法线方向仍然正确。
                    normal_matrix: model.inverse().transpose().to_cols_array_2d(),
                    base_color: material.base_color().to_array(),
                    metallic: material.metallic(),
                    roughness: material.roughness(),
                    // 没挂法线贴图时置 0，着色器据此完全跳过切线空间计算。
                    normal_scale: if material.get(kpbr::standard::NORMAL_TEXTURE).is_some() {
                        1.0
                    } else {
                        0.0
                    },
                    occlusion_strength: material
                        .get(kpbr::standard::OCCLUSION)
                        .and_then(kmaterial::MaterialValue::as_float)
                        .unwrap_or(1.0),
                    emissive: material
                        .get(kpbr::standard::EMISSIVE)
                        .and_then(kmaterial::MaterialValue::as_vec3)
                        .unwrap_or(Vec3::ZERO)
                        .extend(0.0)
                        .to_array(),
                    skin: [skin_offset.unwrap_or(0), morph.0, morph.1, weight_offset],
                    uv_transform: uv_transform_of(material),
                    probe_position,
                    probe_min,
                    probe_max,
                },
            });
        }

        // ── 批处理：同网格同贴图的对象合并成一次绘制 ──
        let mut instances = Vec::new();
        // 和 `instances` 一一对齐的包围盒，阴影逐级剔除按实例下标回查它。
        let mut instance_bounds = Vec::new();
        let batches = build_batches(&draws, &mut instances, &mut instance_bounds);
        // 半透明的批次接在不透明的后面，共用同一个实例数组——
        // 实例下标是全局的，两边分开建数组的话下标会撞。
        let transparent_batches =
            build_transparent_batches(&mut transparent_draws, &mut instances, &mut instance_bounds);
        stats.draw_calls = (batches.len() + transparent_batches.len()) as u32;
        let total_draws = draws.len() + transparent_draws.len();

        // 骨骼矩阵超出容量时翻倍。它排在对象缓冲之前，
        // 因为对象绑定组引用了骨骼缓冲，换了缓冲就得重建绑定组。
        let joint_grew = joints.len() as u64 > self.joint_capacity;
        if joint_grew {
            let capacity = (joints.len() as u64).next_power_of_two();
            self.joint_buffer = create_joint_storage(&self.device, capacity);
            self.joint_capacity = capacity;
        }

        // 对象数超出缓冲容量时翻倍扩容。
        if total_draws as u64 > self.object_capacity {
            let capacity = (total_draws as u64).next_power_of_two();
            let (buffer, bind_group) = Self::create_object_storage(
                &self.device,
                &self.object_layout,
                capacity,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
            );
            self.object_buffer = buffer;
            self.object_bind_group = bind_group;
            self.object_capacity = capacity;
        } else if joint_grew {
            // 对象缓冲没换但骨骼缓冲换了，绑定组仍然指着旧的，得重建。
            self.object_bind_group = create_object_bind_group(
                &self.device,
                &self.object_layout,
                &self.object_buffer,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
            );
        }

        if !joints.is_empty() {
            self.queue
                .write_buffer(&self.joint_buffer, 0, bytemuck::cast_slice(&joints));
        }

        // 形变权重每帧重写；缓冲不够就翻倍，并重建引用它的绑定组。
        if morph_weights.len() as u64 > self.morph_weight_capacity {
            let capacity = (morph_weights.len() as u64).next_power_of_two();
            self.morph_weight_buffer = create_morph_weight_storage(&self.device, capacity);
            self.morph_weight_capacity = capacity;
            self.object_bind_group = create_object_bind_group(
                &self.device,
                &self.object_layout,
                &self.object_buffer,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
            );
        }
        if !morph_weights.is_empty() {
            self.queue.write_buffer(
                &self.morph_weight_buffer,
                0,
                bytemuck::cast_slice(&morph_weights),
            );
        }

        // 一次写完整个数组。逐对象写在上万实例时，光是写入调用本身就很可观。
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.object_buffer, 0, bytemuck::cast_slice(&instances));
        }

        self.queue.write_buffer(
            &self.sky_buffer,
            0,
            bytemuck::cast_slice(&[SkyGlobals {
                inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
                ibl_params: [self.environment_mips as f32, 0.0, 0.0, 0.0],
                camera_position: camera_position.extend(1.0).to_array(),
                environment: scene.environment().to_gpu(),
            }]),
        );

        // ── 粒子：收集、排序、上传 ──
        // 半透明，所以既不进 BVH 也不参与批处理，单独走一条路。
        let mut particle_items = scene.visible_particles(frustum.as_ref());
        let mut scratch = std::mem::take(&mut self.particle_scratch);
        let particle_batches = self.particles.prepare(
            &self.device,
            &self.queue,
            &mut particle_items,
            particle::ParticleCamera {
                view_proj,
                camera_to_world,
                projection: camera.projection_matrix(aspect),
            },
            &mut scratch,
        );
        stats.particles = scratch.len() as u32;
        stats.draw_calls += particle_batches.len() as u32;
        self.particle_scratch = scratch;

        // ── 2D 精灵：排序、合批、上传 ──
        // 先把新登记的贴图传上去。`upload` 内部会跳过已经见过的，
        // 所以每帧扫一遍很便宜。
        for texture in scene.sprite_textures() {
            self.sprites.upload(&self.device, &self.queue, texture);
        }
        // 排序必须在 CPU 上做：精灵全在同一平面，深度缓冲帮不上忙。
        let mut sprite_scratch = std::mem::take(&mut self.sprite_scratch);
        sprite_scratch.clear();
        sprite_scratch.extend_from_slice(scene.sprites());
        let sprite_batches =
            ksprite::sort_and_batch(&mut sprite_scratch, ksprite::SortMode::YDescending);
        self.sprites
            .prepare(&self.device, &self.queue, &sprite_scratch, view_proj);
        stats.sprites = sprite_scratch.len() as u32;
        stats.draw_calls += sprite_batches.len() as u32;
        self.sprite_scratch = sprite_scratch;

        // ── UI：几何与图集 ──
        // 图集只在版本号变了之后才重传：1024² 展开成 RGBA 是 4 MB，
        // 而绝大多数帧里图集是不动的。
        let ui_list = ui.draw_list();
        if ui.atlas_version() != self.ui_atlas_version {
            let texture = ui.atlas_texture();
            self.ui.prepare_atlas(&self.device, &self.queue, &texture);
            self.ui_atlas_version = ui.atlas_version();
        }
        self.ui.prepare(
            &self.device,
            &self.queue,
            ui_list,
            [ui.screen().x, ui.screen().y],
        );
        stats.ui_vertices = ui_list.vertices().len() as u32;
        stats.draw_calls += ui_list.batches().len() as u32;

        // ── 调试线：整帧攒下来的线段一次传上去 ──
        let gizmo_draw = self
            .gizmos
            .prepare(&self.device, &self.queue, scene.gizmos(), view_proj);
        stats.gizmo_vertices = scene.gizmos().len() as u32;

        // 统计在取交换链纹理之前定格：那一步会因垂直同步而阻塞，
        // 算进来的话读到的就是显示器刷新率，不是 CPU 的准备耗时。
        stats.prepare_micros = prepare_start.elapsed().as_micros() as u32;
        self.stats = stats;
        let joint_count = joints.len();
        self.joint_scratch = joints;
        let morph_weight_count = morph_weights.len();
        self.morph_weight_scratch = morph_weights;

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return RenderOutcome::Skip;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return RenderOutcome::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Validation => return RenderOutcome::Fatal,
        };
        let surface_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // 主 pass 与天空都画到 HDR 离屏目标，后处理链再输出到屏幕。
        let target = self.post.hdr_target();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kengine encoder"),
            });

        // ── 阴影深度 pass ──
        // 从光源视角把所有可见物体画一遍，只写深度。
        if shadow_enabled {
            // 深度 pass 有自己的一份骨骼矩阵缓冲：它与主 pass 分属不同的绑定组布局，
            // 共用一个缓冲反而要多传一层引用。数据是同一份，写两遍。
            let shadow_joints_grew = joint_count as u64 > self.shadow.joint_capacity;
            if shadow_joints_grew {
                let capacity = (joint_count as u64).next_power_of_two();
                self.shadow.joint_buffer = create_joint_storage(&self.device, capacity);
                self.shadow.joint_capacity = capacity;
            }
            // 深度 pass 有自己的一份形变权重缓冲，数据同主 pass。
            let shadow_weights_grew = morph_weight_count as u64 > self.shadow.morph_weight_capacity;
            if shadow_weights_grew {
                let capacity = (morph_weight_count as u64).next_power_of_two();
                self.shadow.morph_weight_buffer =
                    create_morph_weight_storage(&self.device, capacity);
                self.shadow.morph_weight_capacity = capacity;
            }
            // 形变增量是静态数据，主 pass 那边可能已经扩过容，这里跟上。
            let shadow_morph_stale = self.shadow.morph_capacity != self.morph_capacity;
            if shadow_morph_stale {
                self.shadow.morph_buffer = create_morph_storage(&self.device, self.morph_capacity);
                self.shadow.morph_capacity = self.morph_capacity;
            }

            if draws.len() as u64 > self.shadow.object_capacity
                || shadow_joints_grew
                || shadow_weights_grew
                || shadow_morph_stale
            {
                let capacity = (draws.len() as u64)
                    .next_power_of_two()
                    .max(self.shadow.object_capacity);
                let (buffer, bind_group) = create_shadow_object_storage(
                    &self.device,
                    &self.shadow.object_layout,
                    capacity,
                    &self.shadow.joint_buffer,
                    &self.shadow.morph_buffer,
                    &self.shadow.morph_weight_buffer,
                );
                self.shadow.object_buffer = buffer;
                self.shadow.object_bind_group = bind_group;
                self.shadow.object_capacity = capacity;
            }
            if joint_count > 0 {
                self.queue.write_buffer(
                    &self.shadow.joint_buffer,
                    0,
                    bytemuck::cast_slice(&self.joint_scratch),
                );
            }
            if morph_weight_count > 0 {
                self.queue.write_buffer(
                    &self.shadow.morph_weight_buffer,
                    0,
                    bytemuck::cast_slice(&self.morph_weight_scratch),
                );
            }
            // 形变增量只在网格新上传时变，用一次拷贝把主 pass 的那份同步过来。
            if self.morph_used > 0 {
                encoder.copy_buffer_to_buffer(
                    &self.morph_buffer,
                    0,
                    &self.shadow.morph_buffer,
                    0,
                    self.morph_used * size_of::<MorphDelta>() as u64,
                );
            }

            // 深度 pass 只要模型矩阵，实例顺序与主 pass 完全一致。
            let shadow_objects: Vec<ShadowObject> = instances
                .iter()
                .map(|instance| ShadowObject {
                    model: instance.model,
                    skin: instance.skin,
                })
                .collect();
            if !shadow_objects.is_empty() {
                self.queue.write_buffer(
                    &self.shadow.object_buffer,
                    0,
                    bytemuck::cast_slice(&shadow_objects),
                );
            }

            // 每级级联跑一遍：一次 render pass 只能挂一层当深度附件。
            //
            // 这曾经是级联最主要的代价——N 级就是 N 次**完整**的场景遍历。
            // 现在每级先剔一遍：范围外的不画，投影小于两个纹素的也不画
            // （小物件在几百米外投的影子还不到一个像素）。
            // 所有级联的全局量**一次写完**，各占一段。
            //
            // 见 `has_dynamic_offset` 那里的注释：分开写会被 wgpu 的
            // 写入时序合并成最后一次。
            {
                let mut blob = vec![0u8; SHADOW_GLOBALS_STRIDE as usize * cascades.len().max(1)];
                for (index, cascade) in cascades.iter().enumerate() {
                    let globals = ShadowGlobals {
                        light_view_proj: cascade.matrix.to_cols_array_2d(),
                        params: [
                            settings.depth_bias,
                            settings.normal_bias,
                            settings.resolution.max(256) as f32,
                            1.0,
                        ],
                    };
                    let start = index * SHADOW_GLOBALS_STRIDE as usize;
                    blob[start..start + size_of::<ShadowGlobals>()]
                        .copy_from_slice(bytemuck::bytes_of(&globals));
                }
                self.queue
                    .write_buffer(&self.shadow.globals_buffer, 0, &blob);
            }

            for (index, cascade) in cascades.iter().enumerate() {
                let cascade_batches = cascade_batches(
                    &batches,
                    &instance_bounds,
                    cascade.matrix,
                    settings.resolution.max(256),
                    settings.min_shadow_texels,
                );
                // 写 `self.stats` 而不是本地的 `stats`：后者在阴影 pass
                // 之前就已经定格并搬进 self 了，改它不会被任何人读到。
                self.stats.shadow_draw_calls += cascade_batches.len() as u32;

                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("kengine shadow pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow.layer_views[index],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });

                // 动态偏移选中本级那一段。
                pass.set_bind_group(
                    0,
                    &self.shadow.globals_bind_group,
                    &[index as u32 * SHADOW_GLOBALS_STRIDE as u32],
                );
                pass.set_bind_group(1, &self.shadow.object_bind_group, &[]);

                // 深度 pass 与贴图无关，本可以按网格合并得更狠，
                // 但沿用主 pass 的分批能保证两边的实例下标一一对应。
                let mut current_skinned = None;
                for batch in &cascade_batches {
                    let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                        continue;
                    };
                    // 批次已按蒙皮与否排过序，管线最多切换一次。
                    if current_skinned != Some(batch.skinned) {
                        pass.set_pipeline(if batch.skinned {
                            &self.shadow.skinned_pipeline
                        } else {
                            &self.shadow.pipeline
                        });
                        current_skinned = Some(batch.skinned);
                    }

                    pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                    if batch.skinned {
                        let Some(skin) = gpu_mesh.skin_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, skin.slice(..));
                    }
                    pass.set_index_buffer(
                        gpu_mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(
                        0..gpu_mesh.index_count,
                        0,
                        batch.first..batch.first + batch.count,
                    );
                }
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            // 整个实例数组绑一次就够，着色器按实例号自己寻址。
            pass.set_bind_group(1, &self.object_bind_group, &[]);
            pass.set_bind_group(3, &self.brdf_bind_group, &[]);

            let mut current_skinned = Some(false);
            for batch in &batches {
                let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                    continue;
                };
                let Some(texture_bind_group) = self.material_bind_groups.get(&batch.texture_key)
                else {
                    continue;
                };

                if current_skinned != Some(batch.skinned) {
                    pass.set_pipeline(if batch.skinned {
                        &self.skinned_pipeline
                    } else {
                        &self.pipeline
                    });
                    // 换管线不影响已绑定的组，它们的布局是同一个。
                    current_skinned = Some(batch.skinned);
                }

                pass.set_bind_group(2, texture_bind_group, &[]);
                pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                if batch.skinned {
                    let Some(skin) = gpu_mesh.skin_buffer.as_ref() else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, skin.slice(..));
                }
                pass.set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                // 实例范围的起点即 `@builtin(instance_index)` 的起始值。
                pass.draw_indexed(
                    0..gpu_mesh.index_count,
                    0,
                    batch.first..batch.first + batch.count,
                );
            }

            // 天空放在最后画：此时深度缓冲已填好，只有空白像素能通过 LessEqual 测试，
            // 被物体挡住的部分直接被剔除，省下大片无用的着色。
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.sky_bind_group, &[]);
            pass.draw(0..3, 0..1);

            // ── 半透明 ──
            //
            // 必须在天空之后：半透明物体要和它背后的东西混合，而天空
            // 就是最远的那个「背后」。放在天空之前的话，天空会把已经
            // 混好的颜色覆盖掉——因为天空的深度是 1，而半透明不写深度，
            // 挡不住它。
            if !transparent_batches.is_empty() {
                pass.set_bind_group(0, &self.globals_bind_group, &[]);
                let mut current = None;
                for batch in &transparent_batches {
                    let Some(gpu_mesh) = self.meshes.get(&batch.mesh_id) else {
                        continue;
                    };
                    let Some(texture_bind_group) =
                        self.material_bind_groups.get(&batch.texture_key)
                    else {
                        continue;
                    };
                    if current != Some(batch.skinned) {
                        pass.set_pipeline(if batch.skinned {
                            &self.skinned_transparent_pipeline
                        } else {
                            &self.transparent_pipeline
                        });
                        current = Some(batch.skinned);
                    }
                    pass.set_bind_group(2, texture_bind_group, &[]);
                    pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                    if batch.skinned {
                        let Some(skin) = gpu_mesh.skin_buffer.as_ref() else {
                            continue;
                        };
                        pass.set_vertex_buffer(1, skin.slice(..));
                    }
                    pass.set_index_buffer(
                        gpu_mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(
                        0..gpu_mesh.index_count,
                        0,
                        batch.first..batch.first + batch.count,
                    );
                }
            }

            // 2D 精灵画在天空之后、粒子之前：精灵半透明，要在不透明物体
            // 之后画；但它又该被粒子盖住（粒子通常是特效）。
            self.sprites.draw(&mut pass, &sprite_batches);
        }

        // ── 粒子与调试线：只读深度的第二个 pass ──
        //
        // 拆出来是为了**软粒子**：它要把深度缓冲当纹理采样，而深度缓冲
        // 此刻正挂在管线上当附件。WebGPU 允许「只读深度附件 + 同一张
        // 纹理当采样源」，但前提是这个 pass 里没人写深度。
        //
        // 粒子和调试线本来就都只测不写（见各自管线的注释），所以拆出来
        // 不改变任何绘制结果，代价只是多一次颜色附件的 load/store。
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine overlay pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.post.hdr_target(),
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // 接着上一个 pass 画，不能清。
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    // `None` 表示**只读**。这一条就是软粒子能成立的原因：
                    // 只读之后同一张深度纹理才能同时当采样源。
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // 粒子在前：它们半透明且不写深度，任何在它们之后画的不透明物体
            // 都会把它们盖掉——包括天空。
            self.particles.draw(&mut pass, &particle_batches);

            // 调试线放在最后：它要盖在所有东西上面，而且不写深度，
            // 所以画在哪一步都不会影响别人，唯独顺序决定了它自己可不可见。
            self.gizmos.draw(&mut pass, &gizmo_draw);
        }

        // 后处理：Bloom + 色调映射，最终写入交换链。
        self.post.run(&self.queue, &mut encoder, &surface_view);

        // ── UI ──
        // 画在后处理之后：UI 的颜色是设计好的，过一遍色调映射会被整体压暗，
        // 白色不再是白色。代价是 UI 拿不到 bloom。
        if !ui_list.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine ui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // 保留后处理的输出，UI 叠在上面。
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.ui.draw(
                &mut pass,
                ui_list,
                [self.config.width, self.config.height],
                ui.scale(),
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(output);

        RenderOutcome::Ok
    }

    /// 把一个网格的形变增量追加到全局缓冲，返回（起点, 目标数）。
    ///
    /// 排列成**顶点优先**：`[顶点0的所有目标][顶点1的所有目标]…`。
    /// 着色器读一个顶点的全部形变时只碰一段连续内存；
    /// 反过来按目标优先排的话，每个目标都要跳一次整段顶点数据。
    fn upload_morph_targets(&mut self, mesh: &kmesh::Mesh) -> (u32, u32) {
        let targets = mesh.morph_targets();
        if targets.is_empty() {
            return (0, 0);
        }

        let deltas = pack_morph_deltas(mesh);
        let offset = self.morph_used;
        let required = offset + deltas.len() as u64;
        if required > self.morph_capacity {
            // 已经上传的增量还得留着（别的网格在用），所以扩容要把旧数据搬过去。
            let capacity = required.next_power_of_two();
            let buffer = create_morph_storage(&self.device, capacity);
            if self.morph_used > 0 {
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("kengine morph grow"),
                        });
                encoder.copy_buffer_to_buffer(
                    &self.morph_buffer,
                    0,
                    &buffer,
                    0,
                    self.morph_used * size_of::<MorphDelta>() as u64,
                );
                self.queue.submit(std::iter::once(encoder.finish()));
            }
            self.morph_buffer = buffer;
            self.morph_capacity = capacity;
            self.object_bind_group = create_object_bind_group(
                &self.device,
                &self.object_layout,
                &self.object_buffer,
                &self.joint_buffer,
                &self.morph_buffer,
                &self.morph_weight_buffer,
            );
        }

        self.queue.write_buffer(
            &self.morph_buffer,
            offset * size_of::<MorphDelta>() as u64,
            bytemuck::cast_slice(&deltas),
        );
        self.morph_used = required;

        (offset as u32, targets.len() as u32)
    }

    /// 确保材质用到的贴图都已上传，返回绑定组缓存键。
    ///
    /// 贴图仍在异步加载时先用中性贴图顶上；加载完成后 id 变化会让键变化，
    /// 下一帧自然换成真正的贴图。
    fn ensure_material_textures(&mut self, material: &Material) -> [Uuid; 5] {
        const SLOTS: [&str; 5] = [
            kmaterial::standard::BASE_COLOR_TEXTURE,
            kpbr::standard::NORMAL_TEXTURE,
            kpbr::standard::METALLIC_ROUGHNESS_TEXTURE,
            kpbr::standard::OCCLUSION_TEXTURE,
            kpbr::standard::EMISSIVE_TEXTURE,
        ];

        let mut key = [Uuid::nil(); 5];
        for (slot, name) in SLOTS.iter().enumerate() {
            let Some(handle) = material
                .get(name)
                .and_then(kmaterial::MaterialValue::as_texture)
            else {
                continue;
            };
            let Some(texture) = handle.data_ref() else {
                continue;
            };

            let id = texture.id();
            if !self.gpu_textures.contains_key(&id) {
                let uploaded = upload_texture(&self.device, &self.queue, &texture);
                self.gpu_textures.insert(id, uploaded);
            }
            key[slot] = id;
        }

        if !self.material_bind_groups.contains_key(&key) {
            let bind_group = self.create_material_bind_group(&key);
            self.material_bind_groups.insert(key, bind_group);
        }

        key
    }

    fn create_material_bind_group(&self, key: &[Uuid; 5]) -> wgpu::BindGroup {
        // 第二个槽位是法线贴图，缺失时要用「不扰动」的中性法线而非白色。
        let texture_for = |slot: usize| -> &GpuTexture {
            self.gpu_textures.get(&key[slot]).unwrap_or({
                if slot == 1 {
                    &self.default_textures.flat_normal
                } else {
                    &self.default_textures.white
                }
            })
        };

        // 采样器取自基础色贴图——一个材质的各张贴图共享同一套 UV，
        // 平铺与过滤方式理应一致。
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&texture_for(0).view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&texture_for(0).sampler),
            },
        ];
        for slot in 1..5 {
            entries.push(wgpu::BindGroupEntry {
                binding: slot as u32 + 1,
                resource: wgpu::BindingResource::TextureView(&texture_for(slot).view),
            });
        }

        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine material bind group"),
            layout: &self.texture_layout,
            entries: &entries,
        })
    }

    fn create_object_storage(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        capacity: u64,
        joints: &wgpu::Buffer,
        morphs: &wgpu::Buffer,
        morph_weights: &wgpu::Buffer,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine object buffer"),
            size: size_of::<ObjectUniforms>() as u64 * capacity.max(1),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group =
            create_object_bind_group(device, layout, &buffer, joints, morphs, morph_weights);
        (buffer, bind_group)
    }

    fn create_depth_view(
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
    ) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine depth texture"),
            size: wgpu::Extent3d {
                width: config.width.max(1),
                height: config.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            // 软粒子要把它当纹理采样。只写 RENDER_ATTACHMENT 的话
            // 建绑定组时会被 wgpu 打回。
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }
}

/// 建一条标准着色管线。静态与蒙皮只差入口函数与顶点布局。
fn create_standard_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    entry_point: &str,
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    label: &str,
    blend_mode: kmaterial::BlendMode,
) -> wgpu::RenderPipeline {
    let transparent = blend_mode == kmaterial::BlendMode::Alpha;
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                // 主 pass 画到 HDR 离屏目标，不是直接画到屏幕。
                format: post::HDR_FORMAT,
                blend: Some(if transparent {
                    wgpu::BlendState::ALPHA_BLENDING
                } else {
                    wgpu::BlendState::REPLACE
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            // 半透明不写深度：写了的话先画的半透明物体会把后画的挡掉，
            // 透过玻璃就看不见玻璃后面的玻璃了。仍然要**测试**深度，
            // 不然半透明物体会画在挡着它的墙前面。
            depth_write_enabled: Option::from(!transparent),
            depth_compare: Option::from(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// 建骨骼矩阵缓冲。容量至少为 1——空缓冲绑不上去。
fn create_joint_storage(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine joint buffer"),
        size: size_of::<[[f32; 4]; 4]>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// 把对象数组与骨骼矩阵数组绑进同一个绑定组。
fn create_object_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    objects: &wgpu::Buffer,
    joints: &wgpu::Buffer,
    morphs: &wgpu::Buffer,
    morph_weights: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine object bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                // 绑定整个缓冲：着色器侧是变长数组，实例号即下标。
                resource: objects.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: joints.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: morphs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: morph_weights.as_entire_binding(),
            },
        ],
    })
}

/// 把网格的形变增量排成**顶点优先**的一维数组：
/// `[顶点0的目标0, 顶点0的目标1, …, 顶点1的目标0, …]`。
///
/// 着色器按 `起点 + 顶点号 × 目标数 + 目标号` 寻址，读一个顶点的全部形变
/// 只碰一段连续内存；反过来按目标优先排的话，每多一个目标就要跳一次整段顶点数据。
fn pack_morph_deltas(mesh: &kmesh::Mesh) -> Vec<MorphDelta> {
    let targets = mesh.morph_targets();
    let vertex_count = mesh.vertices().len();

    let mut deltas = Vec::with_capacity(vertex_count * targets.len());
    for vertex in 0..vertex_count {
        for target in targets {
            deltas.push(target.deltas()[vertex]);
        }
    }
    deltas
}

/// 建形变增量缓冲。
fn create_morph_storage(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine morph buffer"),
        size: size_of::<MorphDelta>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            // 扩容时要把已有的数据搬过去：形变增量是随网格一次性上传的，
            // 重新收集一遍就得回头去问每个网格要数据。
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// 建形变权重缓冲。
fn create_morph_weight_storage(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine morph weight buffer"),
        size: size_of::<f32>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// 标准着色器的完整源码：kpbr 的 BRDF 函数 + 引擎自己的顶点/片元入口。
/// 拼接顺序有讲究：klight 定义了 `Light` 结构，`Globals` 里要用，必须排在最前。
/// 标准着色器的完整源码。
///
/// 拼接顺序有讲究：klight 定义 `Light`、kpbr 的 IBL 定义 `Environment`，
/// 两者都被 `Globals` 引用，必须排在标准着色器之前。
fn standard_shader_source() -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        klight::LIGHT_WGSL,
        kpbr::PBR_WGSL,
        kpbr::IBL_WGSL,
        shadow_sampling_source(),
        include_str!("shader.wgsl")
    )
}

/// 阴影深度 pass 的着色器（含自己的绑定声明）。
fn shadow_shader_source() -> &'static str {
    include_str!("shadow_pass.wgsl")
}

/// 阴影采样函数。纯函数、无绑定声明，可拼进主着色器。
fn shadow_sampling_source() -> &'static str {
    include_str!("shadow_sample.wgsl")
}

/// 建立阴影贴图、深度管线与相关绑定组。
fn create_shadow_resources(device: &wgpu::Device, settings: ShadowSettings) -> ShadowResources {
    let resolution = settings.resolution.max(256);

    // 每级级联一层。分成独立的纹理数组而不是一张大图切格子：
    // 切格子的话相邻级的纹素会在边界处互相渗色，采样到隔壁级的深度，
    // 表现为级联交界处一圈错误的阴影。
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine shadow map"),
        size: wgpu::Extent3d {
            width: resolution,
            height: resolution,
            depth_or_array_layers: klight::cascade::MAX_CASCADES as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    // 采样用的是整个数组的视图。
    let depth_view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    // 渲染时每层一个视图：一次 pass 只能挂一层当深度附件。
    let layer_views: Vec<wgpu::TextureView> = (0..klight::cascade::MAX_CASCADES as u32)
        .map(|layer| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("kengine shadow layer"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        })
        .collect();

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kengine shadow shader"),
        source: wgpu::ShaderSource::Wgsl(shadow_shader_source().into()),
    });

    let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kengine shadow globals layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                // 每级级联一段，靠动态偏移选。
                //
                // 不能每级各写一次同一段缓冲然后各开一个 pass——
                // `Queue::write_buffer` 的写入是在**提交的命令之前**统一
                // 执行的（wgpu 文档原话），所以那样写的话三个 pass
                // 会全部读到最后一次写入的值，三级级联渲染出一模一样的
                // 深度图。这个 bug 不报任何错，只表现为近处的阴影错位。
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(size_of::<ShadowGlobals>() as u64),
            },
            count: None,
        }],
    });
    let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine shadow globals"),
        size: SHADOW_GLOBALS_STRIDE * klight::cascade::MAX_CASCADES as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine shadow globals bind group"),
        layout: &globals_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &globals_buffer,
                offset: 0,
                // 绑定的是一段，不是整个缓冲——动态偏移在此基础上再加。
                size: NonZeroU64::new(size_of::<ShadowGlobals>() as u64),
            }),
        }],
    });

    let object_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kengine shadow object layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<ShadowObject>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<[[f32; 4]; 4]>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<MorphDelta>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<f32>() as u64),
                },
                count: None,
            },
        ],
    });
    let joint_buffer = create_joint_storage(device, Renderer::INITIAL_JOINTS);
    let morph_buffer = create_morph_storage(device, Renderer::INITIAL_MORPH);
    let morph_weight_buffer = create_morph_weight_storage(device, Renderer::INITIAL_CAPACITY);
    let (object_buffer, object_bind_group) = create_shadow_object_storage(
        device,
        &object_layout,
        Renderer::INITIAL_CAPACITY,
        &joint_buffer,
        &morph_buffer,
        &morph_weight_buffer,
    );

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kengine shadow pipeline layout"),
        bind_group_layouts: &[Option::from(&globals_layout), Option::from(&object_layout)],
        immediate_size: 0,
    });

    let pipeline = create_shadow_pipeline(
        device,
        &pipeline_layout,
        &shader,
        "shadow_vs",
        &[Option::from(vertex_layout())],
        "kengine shadow pipeline",
    );

    let skinned_pipeline = create_shadow_pipeline(
        device,
        &pipeline_layout,
        &shader,
        "shadow_skinned_vs",
        &[Option::from(vertex_layout()), Option::from(skin_layout())],
        "kengine skinned shadow pipeline",
    );

    ShadowResources {
        settings,
        cascades: klight::cascade::CascadeSettings::default(),
        pipeline,
        depth_view,
        layer_views,
        skinned_pipeline,
        joint_buffer,
        joint_capacity: Renderer::INITIAL_JOINTS,
        morph_buffer,
        morph_capacity: Renderer::INITIAL_MORPH,
        morph_weight_buffer,
        morph_weight_capacity: Renderer::INITIAL_CAPACITY,
        globals_buffer,
        globals_bind_group,
        object_layout,
        object_buffer,
        object_bind_group,
        object_capacity: Renderer::INITIAL_CAPACITY,
    }
}

/// 建一条深度 pass 管线。静态与蒙皮只差入口函数与顶点布局。
fn create_shadow_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    entry_point: &str,
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            buffers,
        },
        // 深度 pass 不需要片元着色器。
        fragment: None,
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // 两面都画。
            //
            // 「只画背面」是减少自阴影条纹的经典手法，但它**只对闭合网格
            // 成立**：地形、地面平面、单面的墙都是一层三角形，正面朝着光，
            // 剔掉正面就等于整个物体不投影——实测地形的阴影图全是清除值。
            //
            // 自阴影条纹改由 `depth_bias` 与 `normal_bias` 处理。
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Option::from(true),
            depth_compare: Option::from(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_shadow_object_storage(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    capacity: u64,
    joints: &wgpu::Buffer,
    morphs: &wgpu::Buffer,
    morph_weights: &wgpu::Buffer,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine shadow object buffer"),
        size: size_of::<ShadowObject>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine shadow object bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: joints.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: morphs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: morph_weights.as_entire_binding(),
            },
        ],
    });
    (buffer, bind_group)
}

/// 天空 pass 的着色器，同样需要 IBL 里的 `Environment` 结构与天空函数。
fn sky_shader_source() -> String {
    format!("{}\n{}", kpbr::IBL_WGSL, include_str!("sky.wgsl"))
}

/// 生成环境 BRDF 查找表并上传。
///
/// 值域在 [0, 1]，8 位精度足够，且保证在所有后端上都可过滤。
#[allow(clippy::too_many_arguments)]
fn create_brdf_lut(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    shadow_view: &wgpu::TextureView,
    environment_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    const SIZE: u32 = 64;

    let lut = kpbr::ibl::brdf_lut(SIZE);
    let mut pixels = Vec::with_capacity(lut.len() * 4);
    for value in &lut {
        pixels.push((value.x.clamp(0.0, 1.0) * 255.0) as u8);
        pixels.push((value.y.clamp(0.0, 1.0) * 255.0) as u8);
        pixels.push(0);
        pixels.push(255);
    }

    let size = wgpu::Extent3d {
        width: SIZE,
        height: SIZE,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine brdf lut"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // LUT 是数据不是颜色，必须用线性格式，走 sRGB 会把数值扭曲。
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * SIZE),
            rows_per_image: Some(SIZE),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // 环境图的采样器：水平要**重复**（全景图左右连续），
    // 垂直夹取（两极），并开三线性以便在 mip 之间平滑过渡。
    let environment_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kengine environment sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        // 不开的话按粗糙度选 mip 时会在级与级之间跳变，
        // 表现为粗糙度渐变的表面上出现一圈圈台阶。
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kengine brdf sampler"),
        // 查找表必须夹边，重复采样会让掠射角的值绕回去。
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // 阴影贴图用比较采样器：硬件直接返回「通过深度测试的比例」，
    // 配合线性过滤即可得到 2×2 的免费 PCF。
    let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kengine shadow sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        compare: Some(wgpu::CompareFunction::LessEqual),
        ..Default::default()
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine brdf bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(shadow_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&shadow_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(environment_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&environment_sampler),
            },
        ],
    })
}

/// 建天空 pass 的绑定组。
fn create_sky_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    environment_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    // 天空的采样器和主 pass 那个是同一套设置：水平重复、垂直夹取。
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kengine sky environment sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine sky bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(environment_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    })
}

/// 一张 1×1 的黑色占位环境图。
///
/// wgpu 不允许绑定组留空，而没有 HDR 时那两个绑定点也得有东西。
/// 着色器靠 `ibl_params.x == 0` 跳过采样，所以内容是什么无所谓。
fn create_placeholder_environment(device: &wgpu::Device) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine placeholder environment"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor {
            // 着色器声明的是 texture_2d_array，默认视图会推断成 D2，
            // 绑定时会被拒绝。
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
}

/// 把预滤波的 mip 链传上显存。
///
/// 每一级正好是上一级的一半（`prefilter` 保证了这件事），
/// 于是可以直接当纹理的 mip 链用——着色器按粗糙度选级，
/// 硬件的三线性过滤顺便把相邻两级插好。
fn upload_prefiltered_environment(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    levels: &[kpbr::prefilter::PrefilteredLevel],
    probes: &[&[kpbr::prefilter::PrefilteredLevel]],
) -> Option<wgpu::TextureView> {
    let base = levels.first()?;
    // 第 0 层是全局环境，之后每个反射探针一层。
    //
    // 纹理数组要求**每层完全等大**——分辨率和 mip 级数都得一致。
    // 这就是为什么 `Scene::add_reflection_probe` 强制沿用全局环境的
    // 预滤波设置：这里没法给某一层单独换尺寸。
    let layers = 1 + probes.len();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine prefiltered environment"),
        size: wgpu::Extent3d {
            width: base.width as u32,
            height: base.height as u32,
            depth_or_array_layers: layers as u32,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // 必须是浮点格式：HDR 的值可以远大于 1，
        // 用 8 位会把所有高光压成纯白，镜面反射全丢。
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    // 每层的 mip 链依次写入。层号 0 是全局环境。
    for (layer, source) in std::iter::once(levels)
        .chain(probes.iter().copied())
        .enumerate()
    {
        for (index, level) in source.iter().enumerate() {
            // 尺寸对不上的层直接跳过：写进去会被 wgpu 拒绝，
            // 而留一层没写的话那个探针会采样出未初始化的内存。
            // 跳过至少让它退化成黑色，而不是花屏。
            let expected_width = (base.width >> index).max(1);
            let expected_height = (base.height >> index).max(1);
            if index >= levels.len()
                || level.width != expected_width
                || level.height != expected_height
            {
                klog::warn!(
                    "反射探针第 {layer} 层的 mip {index} 尺寸是 {}×{}，                     期望 {expected_width}×{expected_height}——跳过",
                    level.width,
                    level.height
                );
                continue;
            }

            // CPU 侧是紧凑的 RGB，GPU 要 RGBA 半精度。
            let mut texels: Vec<u8> = Vec::with_capacity(level.width * level.height * 8);
            for pixel in level.pixels.chunks_exact(3) {
                for channel in [pixel[0], pixel[1], pixel[2], 1.0] {
                    texels.extend_from_slice(&half_from_f32(channel).to_le_bytes());
                }
            }

            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: index as u32,
                    // z 是数组层号，不是深度——2D 数组纹理就是这么寻址的。
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &texels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(level.width as u32 * 8),
                    rows_per_image: Some(level.height as u32),
                },
                wgpu::Extent3d {
                    width: level.width as u32,
                    height: level.height as u32,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    Some(texture.create_view(&wgpu::TextureViewDescriptor {
        // 必须显式指定 D2Array：默认视图对只有一层的纹理会推断成 D2，
        // 而着色器声明的是 texture_2d_array，绑定时会被拒绝。
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    }))
}

/// `f32` 转 IEEE754 半精度。
///
/// 手写而不是拉一个 crate：只用在环境图上传这一处，而 `half` 会把
/// 整个依赖树拉进来。溢出饱和到 inf 而不是回绕——HDR 里确实有
/// 超出半精度范围的太阳，回绕会让它变成黑点。
fn half_from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;

    if exponent >= 0x1f {
        // 溢出或本来就是 inf/NaN。
        return sign | 0x7c00;
    }
    if exponent <= 0 {
        // 下溢：直接归零。半精度的非规格化数在这里不值得处理——
        // 环境图里那么暗的值对光照没有贡献。
        return sign;
    }
    sign | ((exponent as u16) << 10) | ((mantissa >> 13) as u16)
}

/// 上传一张贴图，连同按其采样设置建好的采样器一起返回。
pub(crate) fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &Texture,
) -> GpuTexture {
    let size = wgpu::Extent3d {
        width: texture.width().max(1),
        height: texture.height().max(1),
        depth_or_array_layers: 1,
    };

    let format = match texture.format() {
        TextureFormat::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        TextureFormat::Linear => wgpu::TextureFormat::Rgba8Unorm,
    };

    let gpu_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &gpu_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        texture.data(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * size.width),
            rows_per_image: Some(size.height),
        },
        size,
    );

    let descriptor = texture.sampler();
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("kengine texture sampler"),
        address_mode_u: convert_wrap(descriptor.wrap_u),
        address_mode_v: convert_wrap(descriptor.wrap_v),
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: convert_filter(descriptor.mag_filter),
        min_filter: convert_filter(descriptor.min_filter),
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    GpuTexture {
        view: gpu_texture.create_view(&wgpu::TextureViewDescriptor::default()),
        sampler,
    }
}

fn convert_filter(filter: FilterMode) -> wgpu::FilterMode {
    match filter {
        FilterMode::Nearest => wgpu::FilterMode::Nearest,
        FilterMode::Linear => wgpu::FilterMode::Linear,
    }
}

fn convert_wrap(wrap: WrapMode) -> wgpu::AddressMode {
    match wrap {
        WrapMode::Repeat => wgpu::AddressMode::Repeat,
        WrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        WrapMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use kshader::Shader;

    fn standard_shader() -> String {
        standard_shader_source()
    }

    #[test]
    fn standard_shader_passes_validation() {
        // 引擎自带的着色器必须能通过 naga 校验，否则运行时才会在建管线时崩。
        Shader::from_wgsl(standard_shader()).expect("标准着色器应当通过校验");
    }

    #[test]
    fn shader_entry_points_match_pipeline() {
        let shader = Shader::from_wgsl(standard_shader()).unwrap();

        // 这两个名字硬编码在建管线的代码里，改了着色器却忘改这里会导致启动崩溃。
        assert_eq!(shader.vertex_entry(), Some("vs_main"));
        assert_eq!(shader.fragment_entry(), Some("fs_main"));
    }

    #[test]
    fn a_plain_material_gets_an_identity_uv_transform() {
        // 普通模型完全不该受精灵那套 UV 变换影响。
        assert_eq!(
            uv_transform_of(&kmaterial::Material::standard()),
            [1.0, 1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn atlas_parameters_reach_the_uv_transform() {
        let material = kmaterial::Material::standard()
            .with(kpbr::standard::UV_SCALE, kmath::Vec2::new(0.25, 0.5))
            .with(kpbr::standard::UV_OFFSET, kmath::Vec2::new(0.75, 0.5));

        assert_eq!(uv_transform_of(&material), [0.25, 0.5, 0.75, 0.5]);
    }

    #[test]
    fn a_wrongly_typed_uv_parameter_falls_back_to_identity() {
        // 一处写错不该让整个模型的贴图全乱。
        let material = kmaterial::Material::standard().with(kpbr::standard::UV_SCALE, 0.5f32);

        assert_eq!(uv_transform_of(&material), [1.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn only_the_missing_half_falls_back() {
        let material = kmaterial::Material::standard()
            .with(kpbr::standard::UV_OFFSET, kmath::Vec2::splat(0.25));

        assert_eq!(uv_transform_of(&material), [1.0, 1.0, 0.25, 0.25]);
    }

    #[test]
    fn uniform_sizes_match_wgsl_layout() {
        // Globals：view_proj(64) + vec4 × 3 + 级联矩阵(64 × 4)
        //          + 切分距离(16) + 阴影参数(16) + IBL 参数(16)
        //          + 环境(224) + 光源数组(64 × 16)
        assert_eq!(
            size_of::<Globals>(),
            64 + 16 * 3
                + 64 * klight::cascade::MAX_CASCADES
                + 16 * 3
                + size_of::<GpuEnvironment>()
                + 64 * MAX_LIGHTS
        );
        assert_eq!(size_of::<Globals>() % 16, 0);
        // ObjectUniforms：mat4x4(64) × 2 + base_color(16) + f32 × 4 + emissive(16)
        //                 + 骨骼偏移(16) + UV 变换(16)
        //                 + 探针 vec4 × 3(48) = 256。
        // 四个 f32 恰好凑满 16 字节，emissive 才能落在 vec4 要求的对齐边界上。
        assert_eq!(
            size_of::<ObjectUniforms>(),
            64 * 2 + 16 * 3 + 16 * 2 + 16 * 3
        );
        assert_eq!(size_of::<ObjectUniforms>() % 16, 0);
    }

    #[test]
    fn post_shader_passes_validation() {
        Shader::from_wgsl(include_str!("post.wgsl")).expect("后处理着色器应当通过校验");
    }

    #[test]
    fn post_shader_exposes_every_entry_point() {
        // 这四个名字硬编码在建管线的代码里。
        let source = include_str!("post.wgsl");
        for entry in [
            "fullscreen_vs",
            "bloom_extract_fs",
            "bloom_blur_fs",
            "composite_fs",
        ] {
            assert!(source.contains(entry), "后处理着色器缺少入口 {entry}");
        }
    }

    #[test]
    fn shadow_shaders_pass_validation() {
        // 深度 pass 与采样函数分属两个文件：前者自带绑定声明，
        // 后者是纯函数、要拼进主着色器，混在一起会与主着色器的绑定冲突。
        Shader::from_wgsl(shadow_shader_source()).expect("阴影深度着色器应当通过校验");
    }

    #[test]
    fn shadow_pass_entry_point_matches_pipeline() {
        let shader = Shader::from_wgsl(shadow_shader_source()).unwrap();

        assert_eq!(shader.vertex_entry(), Some("shadow_vs"));
        // 深度 pass 不需要片元着色器。
        assert_eq!(shader.fragment_entry(), None);
    }

    #[test]
    fn shadow_uniform_layouts_are_aligned() {
        assert_eq!(size_of::<ShadowGlobals>(), 80);
        assert_eq!(size_of::<ShadowGlobals>() % 16, 0);
        assert_eq!(size_of::<ShadowObject>() % 16, 0);
    }

    #[test]
    fn sky_shader_passes_validation() {
        Shader::from_wgsl(sky_shader_source()).expect("天空着色器应当通过校验");
    }

    #[test]
    fn sky_shader_entry_points_match_pipeline() {
        let shader = Shader::from_wgsl(sky_shader_source()).unwrap();

        assert_eq!(shader.vertex_entry(), Some("sky_vs"));
        assert_eq!(shader.fragment_entry(), Some("sky_fs"));
    }

    /// 造一个只有网格与贴图键有意义的绘制项。
    fn draw(mesh: u128, texture: u128) -> DrawCall {
        DrawCall {
            mesh_id: Uuid::from_u128(mesh),
            texture_key: [Uuid::from_u128(texture); 5],
            skinned: false,
            depth: 0.0,
            aabb: kmath::Aabb::new(kmath::Vec3::ZERO, kmath::Vec3::ONE),
            uniforms: ObjectUniforms::zeroed(),
        }
    }

    /// 同上，但带一个到相机的距离，用来验半透明排序。
    fn draw_at(mesh: u128, texture: u128, depth: f32) -> DrawCall {
        DrawCall {
            depth,
            ..draw(mesh, texture)
        }
    }

    /// 同上，但走蒙皮管线。
    fn skinned_draw(mesh: u128, texture: u128) -> DrawCall {
        DrawCall {
            skinned: true,
            ..draw(mesh, texture)
        }
    }

    /// 跑一遍半透明分批，返回批次和每批的第一个实例的距离。
    fn transparent_batch(mut draws: Vec<DrawCall>) -> Vec<f32> {
        let mut instances = Vec::new();
        let batches = build_transparent_batches(&mut draws, &mut instances, &mut Vec::new());
        // 排序后的顺序体现在 draws 上，按批次的 first 反查。
        batches
            .iter()
            .map(|b| draws[b.first as usize].depth)
            .collect()
    }

    #[test]
    fn transparent_draws_are_sorted_back_to_front() {
        // 重排半透明物体会让远处的东西画在近处的上面。
        let order = transparent_batch(vec![
            draw_at(1, 1, 5.0),
            draw_at(2, 2, 100.0),
            draw_at(3, 3, 50.0),
        ]);
        assert_eq!(order, vec![100.0, 50.0, 5.0]);
    }

    #[test]
    fn transparent_batches_only_merge_adjacent_items() {
        // 同网格同贴图但距离上被别的物体隔开时，不能合并——
        // 合并等于把中间那个的绘制顺序挪了位置。
        let mut draws = vec![
            draw_at(1, 1, 100.0),
            draw_at(2, 2, 50.0),
            draw_at(1, 1, 10.0),
        ];
        let mut instances = Vec::new();
        let batches = build_transparent_batches(&mut draws, &mut instances, &mut Vec::new());
        assert_eq!(batches.len(), 3, "隔着一个物体的两项被错误合并了");

        // 相邻的同类项仍然要合并。
        let mut adjacent = vec![
            draw_at(1, 1, 100.0),
            draw_at(1, 1, 90.0),
            draw_at(2, 2, 10.0),
        ];
        instances.clear();
        let batches = build_transparent_batches(&mut adjacent, &mut instances, &mut Vec::new());
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].count, 2);
    }

    #[test]
    fn nan_depth_does_not_panic() {
        // 退化的变换会算出 NaN 距离。`partial_cmp().unwrap()` 会崩掉整帧。
        let mut draws = vec![
            draw_at(1, 1, f32::NAN),
            draw_at(2, 2, 5.0),
            draw_at(3, 3, f32::NAN),
        ];
        let mut instances = Vec::new();
        let batches = build_transparent_batches(&mut draws, &mut instances, &mut Vec::new());
        assert_eq!(batches.len(), 3);
        assert_eq!(instances.len(), 3);
    }

    #[test]
    fn transparent_instances_append_after_opaque_ones() {
        // 实例下标是全局的：两边各建一个数组的话下标会撞，
        // 半透明物体会用上不透明物体的变换矩阵。
        let opaque = [draw(1, 1), draw(2, 2)];
        let mut instances = Vec::new();
        let opaque_batches = build_batches(&opaque, &mut instances, &mut Vec::new());
        let mut transparent = vec![draw_at(3, 3, 10.0), draw_at(4, 4, 20.0)];
        let transparent_batches =
            build_transparent_batches(&mut transparent, &mut instances, &mut Vec::new());

        assert_eq!(instances.len(), 4);
        // 半透明的批次全部指向后两个槽位。
        for batch in &transparent_batches {
            assert!(
                batch.first >= 2,
                "半透明批次的起点 {} 撞进了不透明区",
                batch.first
            );
        }
        for batch in &opaque_batches {
            assert!(batch.first < 2);
        }
    }

    /// 跑一遍分批，返回批次与按批次排好的实例数组。
    fn batch(draws: &[DrawCall]) -> (Vec<Batch>, Vec<ObjectUniforms>) {
        let mut instances = Vec::new();
        let batches = build_batches(draws, &mut instances, &mut Vec::new());
        (batches, instances)
    }

    #[test]
    fn identical_objects_merge_into_one_batch() {
        let (batches, instances) = batch(&[draw(1, 1), draw(1, 1), draw(1, 1)]);

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].first, 0);
        assert_eq!(batches[0].count, 3);
        assert_eq!(instances.len(), 3);
    }

    #[test]
    fn different_meshes_or_textures_split_batches() {
        // 网格不同、贴图不同都不能合并——两者都要重新绑定。
        let (batches, _) = batch(&[draw(1, 1), draw(2, 1), draw(1, 2)]);

        assert_eq!(batches.len(), 3);
        assert!(batches.iter().all(|batch| batch.count == 1));
    }

    #[test]
    fn scattered_objects_are_gathered_into_batches() {
        // 交错提交的同类对象，排序后应当聚成两批。
        let (batches, _) = batch(&[draw(1, 1), draw(2, 2), draw(1, 1), draw(2, 2), draw(1, 1)]);

        assert_eq!(batches.len(), 2);
        assert_eq!(batches.iter().map(|b| b.count).sum::<u32>(), 5);
    }

    #[test]
    fn batches_cover_every_object_exactly_once() {
        let draws: Vec<DrawCall> = (0..64).map(|i| draw(i % 5, i % 3)).collect();

        let (batches, instances) = batch(&draws);

        // 批次必须首尾相接地覆盖整个实例数组：漏掉的对象不会被画，
        // 重叠的区间会把别人的变换套到自己头上。
        let mut next = 0;
        for batch in &batches {
            assert_eq!(batch.first, next);
            next += batch.count;
        }
        assert_eq!(next as usize, draws.len());
        assert_eq!(instances.len(), draws.len());
    }

    #[test]
    fn batching_does_not_lose_per_object_data() {
        // 同一批里各对象的变换互不相同，合并绘制不能把它们抹平。
        let draws: Vec<DrawCall> = (0..4)
            .map(|i| {
                let mut d = draw(1, 1);
                d.uniforms.model[3][0] = i as f32;
                d
            })
            .collect();

        let (batches, instances) = batch(&draws);

        assert_eq!(batches.len(), 1);
        let mut offsets: Vec<f32> = instances.iter().map(|i| i.model[3][0]).collect();
        offsets.sort_by(f32::total_cmp);
        assert_eq!(offsets, vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn instances_follow_batch_order() {
        // 实例数组必须按批次重排：着色器是拿实例号当下标去取数据的，
        // 顺序对不上，物体就会套上别人的变换。
        let mut a = draw(2, 1);
        a.uniforms.metallic = 1.0;
        let mut b = draw(1, 1);
        b.uniforms.metallic = 2.0;

        let (batches, instances) = batch(&[a, b]);

        assert_eq!(batches.len(), 2);
        for batch in &batches {
            let expected = if batch.mesh_id == Uuid::from_u128(1) {
                2.0
            } else {
                1.0
            };
            assert_eq!(instances[batch.first as usize].metallic, expected);
        }
    }

    #[test]
    fn empty_frame_produces_no_batches() {
        let (batches, instances) = batch(&[]);

        assert!(batches.is_empty());
        assert!(instances.is_empty());
    }

    #[test]
    fn stats_report_batching_effectiveness() {
        let stats = RenderStats {
            drawn: 100,
            draw_calls: 4,
            ..RenderStats::default()
        };

        assert_eq!(stats.instances_per_draw(), 25.0);
        // 一帧什么都没画时不能除出 NaN。
        assert_eq!(RenderStats::default().instances_per_draw(), 0.0);
    }

    #[test]
    fn skinned_and_static_objects_never_share_a_batch() {
        // 两者的顶点布局不同，共用一批就会用错管线。
        let (batches, _) = batch(&[draw(1, 1), skinned_draw(1, 1), draw(1, 1)]);

        assert_eq!(batches.len(), 2);
        // 排序把静态排在前、蒙皮排在后，管线因此最多切换一次。
        assert!(!batches[0].skinned && batches[0].count == 2);
        assert!(batches[1].skinned && batches[1].count == 1);
    }

    #[test]
    fn skinned_instances_of_the_same_mesh_still_batch() {
        // 每个蒙皮实例有自己的一套骨骼矩阵，但矩阵在同一个缓冲里、
        // 各自记着偏移，所以同网格的多个角色仍然能一次画完。
        let (batches, _) = batch(&[skinned_draw(1, 1), skinned_draw(1, 1)]);

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].count, 2);
    }

    #[test]
    fn shadow_object_layout_matches_the_shader() {
        // model(64) + 骨骼偏移(16)
        assert_eq!(size_of::<ShadowObject>(), 80);
        assert_eq!(size_of::<ShadowObject>() % 16, 0);
    }

    #[test]
    fn shaders_expose_the_skinning_entry_points() {
        // 这两个名字硬编码在建管线的代码里。
        let standard = Shader::from_wgsl(standard_shader()).unwrap();
        assert!(standard_shader().contains("fn vs_skinned"));
        assert!(standard_shader().contains("var<storage, read> joint_matrices"));
        let _ = standard;

        let shadow = shadow_shader_source();
        assert!(shadow.contains("fn shadow_skinned_vs"));
        assert!(shadow.contains("var<storage, read> shadow_joints"));
    }

    #[test]
    fn skin_vertex_layout_matches_the_mesh_data() {
        // 顶点缓冲的跨距必须与 kmesh 的结构体一致，否则读到的是错位的数据。
        assert_eq!(
            skin_layout().array_stride,
            size_of::<SkinVertex>() as wgpu::BufferAddress
        );
        // 蒙皮属性接在标准顶点的 5 个位置之后。
        assert_eq!(skin_layout().attributes[0].shader_location, 5);
        assert_eq!(skin_layout().attributes[1].shader_location, 6);
    }

    #[test]
    fn morph_deltas_are_packed_vertex_major() {
        use kmesh::{MorphTarget, Vertex};

        // 两个顶点、两个形变目标，增量用可辨认的数值填。
        let vertices = vec![Vertex::default(); 2];
        let mesh = kmesh::Mesh::new(vertices, vec![0, 1, 0]).with_morph_targets(
            vec![
                MorphTarget::new(
                    "a",
                    vec![
                        MorphDelta {
                            position: [1.0, 0.0, 0.0],
                            ..Default::default()
                        },
                        MorphDelta {
                            position: [2.0, 0.0, 0.0],
                            ..Default::default()
                        },
                    ],
                ),
                MorphTarget::new(
                    "b",
                    vec![
                        MorphDelta {
                            position: [10.0, 0.0, 0.0],
                            ..Default::default()
                        },
                        MorphDelta {
                            position: [20.0, 0.0, 0.0],
                            ..Default::default()
                        },
                    ],
                ),
            ],
            vec![0.0, 0.0],
        );

        let packed = pack_morph_deltas(&mesh);

        // 顶点优先：同一顶点的两个目标相邻，着色器才能一段连续内存读完。
        assert_eq!(packed.len(), 4);
        assert_eq!(packed[0].position[0], 1.0); // 顶点 0 / 目标 a
        assert_eq!(packed[1].position[0], 10.0); // 顶点 0 / 目标 b
        assert_eq!(packed[2].position[0], 2.0); // 顶点 1 / 目标 a
        assert_eq!(packed[3].position[0], 20.0); // 顶点 1 / 目标 b
    }

    #[test]
    fn packing_a_mesh_without_morph_targets_is_empty() {
        assert!(pack_morph_deltas(&kmesh::Mesh::cube()).is_empty());
    }

    #[test]
    fn shaders_apply_morph_targets() {
        // 形变在顶点着色器里叠加，两条主管线与两条深度管线都要有。
        let standard = standard_shader();
        assert!(standard.contains("fn apply_morph"));
        assert!(standard.contains("var<storage, read> morph_deltas"));
        assert!(standard.contains("var<storage, read> morph_weights"));
        // 形变要按顶点号取增量，缺了这个 builtin 就只能整块网格一起变形。
        assert!(standard.contains("@builtin(vertex_index)"));

        let shadow = shadow_shader_source();
        assert!(shadow.contains("fn shadow_morph_position"));
        assert!(shadow.contains("@builtin(vertex_index)"));
    }

    #[test]
    fn morph_delta_matches_the_shader_layout() {
        // WGSL 侧是两个 vec3 各补齐到 16 字节，CPU 侧必须一致。
        assert_eq!(size_of::<MorphDelta>(), 32);
        assert_eq!(size_of::<MorphDelta>() % 16, 0);
    }

    #[test]
    fn shaders_index_objects_by_instance() {
        // 实例化的关键：着色器必须按实例号取每个对象的数据。
        // 改回逐对象绑定的写法时，这里会立刻报警。
        let standard = standard_shader();
        assert!(standard.contains("var<storage, read> objects"));
        assert!(standard.contains("@builtin(instance_index)"));

        let shadow = shadow_shader_source();
        assert!(shadow.contains("var<storage, read> shadow_objects"));
        assert!(shadow.contains("@builtin(instance_index)"));
    }

    #[test]
    fn uniform_structs_are_16_byte_aligned() {
        // WGSL 的 uniform 地址空间要求结构体按 16 字节对齐，
        // 不满足时 wgpu 会在创建绑定组时报错。
        assert_eq!(size_of::<Globals>() % 16, 0);
        assert_eq!(size_of::<ObjectUniforms>() % 16, 0);
    }

    #[test]
    fn shadow_globals_fit_in_their_stride() {
        // 每级级联在缓冲里占一段，动态偏移选中其中一段。
        // 结构体涨过 stride 的话，后一级会把前一级的数据盖掉。
        assert!(
            size_of::<ShadowGlobals>() as u64 <= SHADOW_GLOBALS_STRIDE,
            "ShadowGlobals 有 {} 字节，超过了 {SHADOW_GLOBALS_STRIDE} 的步长",
            size_of::<ShadowGlobals>()
        );
    }

    #[test]
    fn the_shadow_globals_stride_satisfies_the_alignment_floor() {
        // 动态偏移必须是 `min_uniform_buffer_offset_alignment` 的倍数。
        // WebGPU 保证这个下限不超过 256，所以按 256 对齐在哪儿都成立。
        assert_eq!(SHADOW_GLOBALS_STRIDE % 256, 0);
    }

    #[test]
    fn every_cascade_gets_a_distinct_offset() {
        // 这条测试记录的是一个真实的 bug：原来的写法是每级各写一次
        // 同一段缓冲、各开一个 pass。`Queue::write_buffer` 的写入是在
        // **提交的命令之前**统一执行的，所以三个 pass 全读到最后一次
        // 写入的值——三级级联渲染出逐字节相同的深度图。
        //
        // 不报任何错，只表现为近处的阴影错位。实测确认过：修复前
        // 三层 `layers[0] == layers[i]` 全为真，修复后为假。
        let offsets: Vec<u64> = (0..klight::cascade::MAX_CASCADES)
            .map(|i| i as u64 * SHADOW_GLOBALS_STRIDE)
            .collect();

        for pair in offsets.windows(2) {
            assert_ne!(pair[0], pair[1]);
            assert!(
                pair[1] - pair[0] >= size_of::<ShadowGlobals>() as u64,
                "两级的偏移间距放不下一个 ShadowGlobals"
            );
        }
        // 最后一级也要落在缓冲里。
        let buffer_size = SHADOW_GLOBALS_STRIDE * klight::cascade::MAX_CASCADES as u64;
        assert!(offsets.last().unwrap() + size_of::<ShadowGlobals>() as u64 <= buffer_size);
    }

    #[test]
    fn packing_cascade_globals_lands_at_the_right_offsets() {
        // 复现 `render` 里那段打包逻辑，验证每级的数据落在自己那一段。
        let matrices = [
            Mat4::IDENTITY,
            Mat4::from_scale(Vec3::splat(2.0)),
            Mat4::ZERO,
        ];
        let mut blob = vec![0u8; SHADOW_GLOBALS_STRIDE as usize * matrices.len()];
        for (index, matrix) in matrices.iter().enumerate() {
            let globals = ShadowGlobals {
                light_view_proj: matrix.to_cols_array_2d(),
                params: [index as f32, 0.0, 0.0, 1.0],
            };
            let start = index * SHADOW_GLOBALS_STRIDE as usize;
            blob[start..start + size_of::<ShadowGlobals>()]
                .copy_from_slice(bytemuck::bytes_of(&globals));
        }

        for (index, matrix) in matrices.iter().enumerate() {
            let start = index * SHADOW_GLOBALS_STRIDE as usize;
            let read: &ShadowGlobals =
                bytemuck::from_bytes(&blob[start..start + size_of::<ShadowGlobals>()]);
            assert_eq!(read.light_view_proj, matrix.to_cols_array_2d());
            assert_eq!(
                read.params[0], index as f32,
                "第 {index} 级读到了别人的数据"
            );
        }
    }
}
