//! 可编程后处理：自己写的全屏 pass，串成一条链。
//!
//! 固定的那几步（辉光、色调映射、FXAA）在 [`crate::post`]；这里是让游戏
//! 自己往链上挂东西的地方。three.js 的 `RenderPipeline` + TSL 节点、
//! bevy 的 `ViewNode` 都是干这个的。
//!
//! # 一个效果长什么样
//!
//! ```ignore
//! let vignette = PostEffect::new("vignette", PostStage::Ldr, r#"
//!     @fragment
//!     fn main(in: PostVertex) -> @location(0) vec4<f32> {
//!         let color = sample_input(in.uv);
//!         let d = distance(in.uv, vec2<f32>(0.5));
//!         return vec4<f32>(color.rgb * (1.0 - d * param_amount().x), 1.0);
//!     }
//! "#)
//! .param("amount", 0.8)
//! .pass("main", PassOutput::Out);
//!
//! ctx.post_effects.push(vignette);
//! // 之后随时：
//! ctx.post_effects.get_mut("vignette").unwrap().set("amount", 1.2);
//! ```
//!
//! 引擎在效果的 WGSL 前面拼上 `postfx_prelude.wgsl`：顶点着色器、所有
//! 绑定（输入、深度、法线、运动向量、遮罩、历史、暂存图、用户贴图、
//! 离屏视图）和一堆常用函数，外加每个参数一个 `param_<名字>()`。
//!
//! # 两个阶段
//!
//! | | [`PostStage::Hdr`] | [`PostStage::Ldr`] |
//! |---|---|---|
//! | 在哪 | 色调映射之前 | 色调映射之后、抗锯齿之前 |
//! | 输入 | 线性 HDR，值可以远大于 1 | 显示空间，`[0,1]` |
//! | 适合 | 景深、运动模糊、SSR、体积光——和物理量打交道的 | 调色 LUT、描边、像素化、复古滤镜——和「最终画面」打交道的 |
//!
//! # 多 pass
//!
//! 一个效果可以有好几个 pass，中间结果放在最多四张**暂存图**里
//! （`t0`…`t3`，各自可以是分辨率的几分之一）。最后一个 pass 写
//! [`PassOutput::Out`]，那是交给链上下一环的结果。
//!
//! # 历史
//!
//! [`PostEffect::history`] 让效果拿到自己**上一帧**的输出（或某张暂存图）——
//! 残影、TAA、时间性降噪都靠它。刚开始或窗口变了大小之后没有历史，
//! `history_valid()` 返回假。

use crate::GpuTexture;
use bytemuck::{Pod, Zeroable};
use fxhash::{FxHashMap, FxHashSet};
use kcore::uuid::Uuid;
use kmath::{Vec2, Vec3, Vec4};
use ktexture::Texture;
use std::num::NonZeroU64;
use std::sync::Arc;

/// 每个效果最多几个参数（`vec4`）。
pub const MAX_PARAMS: usize = 16;
/// 每个效果最多几张暂存图。
pub const MAX_SCRATCH: usize = 4;
/// 每个效果最多几张用户贴图。
pub const MAX_USER_TEXTURES: usize = 2;

/// 效果挂在链上的哪一段。见[模块文档](self)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostStage {
    /// 色调映射之前，线性 HDR。
    Hdr,
    /// 色调映射之后，显示空间。
    Ldr,
}

/// 效果要引擎额外准备的输入。
///
/// 每一样都有代价（多渲一遍几何、多一个 pass），所以要**声明**了才给；
/// 没声明的那几个绑的是全零的占位图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PostInputs(u32);

impl PostInputs {
    /// 什么都不要。深度总是给的——它本来就在，不花钱。
    pub const NONE: Self = Self(0);
    /// 世界法线 + 金属度（预通道）。
    pub const NORMAL: Self = Self(1);
    /// 运动向量（预通道）。
    pub const VELOCITY: Self = Self(2);
    /// 基础色 + 粗糙度（预通道）。
    pub const MATERIAL: Self = Self(4);
    /// 后处理遮罩（遮罩 pass）。
    pub const MASK: Self = Self(8);
    /// 每帧把投影挪一个亚像素。时间性累积的效果要它，否则历史帧和本帧
    /// 采的是同一批位置，累积多少帧都还是那一个样本。
    pub const JITTER: Self = Self(16);

    /// 两组合起来。
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// 是否包含 `other` 的全部。
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// 要不要跑预通道。
    pub const fn needs_prepass(self) -> bool {
        self.0 & (Self::NORMAL.0 | Self::VELOCITY.0 | Self::MATERIAL.0) != 0
    }
}

impl std::ops::BitOr for PostInputs {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for PostInputs {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// 一个 pass 写到哪儿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassOutput {
    /// 暂存图 `t0`…`t3`。
    Scratch(u8),
    /// 这个效果的结果，交给链上的下一环。
    Out,
}

/// 能当参数的值：一律存成 `vec4`，不够四个分量的补零。
pub trait IntoParam {
    /// 转成 `vec4`。
    fn into_param(self) -> [f32; 4];
}

impl IntoParam for f32 {
    fn into_param(self) -> [f32; 4] {
        [self, 0.0, 0.0, 0.0]
    }
}
impl IntoParam for i32 {
    fn into_param(self) -> [f32; 4] {
        [self as f32, 0.0, 0.0, 0.0]
    }
}
impl IntoParam for u32 {
    fn into_param(self) -> [f32; 4] {
        [self as f32, 0.0, 0.0, 0.0]
    }
}
impl IntoParam for bool {
    fn into_param(self) -> [f32; 4] {
        [if self { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]
    }
}
impl IntoParam for Vec2 {
    fn into_param(self) -> [f32; 4] {
        [self.x, self.y, 0.0, 0.0]
    }
}
impl IntoParam for Vec3 {
    fn into_param(self) -> [f32; 4] {
        [self.x, self.y, self.z, 0.0]
    }
}
impl IntoParam for Vec4 {
    fn into_param(self) -> [f32; 4] {
        self.to_array()
    }
}
impl IntoParam for [f32; 2] {
    fn into_param(self) -> [f32; 4] {
        [self[0], self[1], 0.0, 0.0]
    }
}
impl IntoParam for [f32; 3] {
    fn into_param(self) -> [f32; 4] {
        [self[0], self[1], self[2], 0.0]
    }
}
impl IntoParam for [f32; 4] {
    fn into_param(self) -> [f32; 4] {
        self
    }
}

/// 用户贴图槽里放什么。
#[derive(Debug, Clone)]
pub enum PostTexture {
    /// 一张图。
    Image(Texture),
    /// 离屏相机 `slot` 这一帧画出来的东西。
    View(u8),
}

/// 一个 pass：片元入口名 + 输出目标。
#[derive(Debug, Clone)]
struct PostPass {
    entry: String,
    output: PassOutput,
}

/// 一个后处理效果。见[模块文档](self)。
#[derive(Debug, Clone)]
pub struct PostEffect {
    /// GPU 那边认它用的身份：管线、暂存图、历史都按这个缓存。
    ///
    /// 每次 [`new`](Self::new) 都是新的——同名的两个效果也是两份状态。
    id: Uuid,
    name: String,
    /// 开不开。关着的效果不跑，但它的 GPU 状态（历史）留着。
    pub enabled: bool,
    stage: PostStage,
    source: Arc<str>,
    passes: Vec<PostPass>,
    /// 各暂存图相对输出分辨率的比例。
    scratch: Vec<f32>,
    history: Option<PassOutput>,
    inputs: PostInputs,
    params: [[f32; 4]; MAX_PARAMS],
    param_names: Vec<String>,
    textures: [Option<PostTexture>; MAX_USER_TEXTURES],
}

impl PostEffect {
    /// 新建一个效果。`source` 是片元入口所在的 WGSL，引擎会在前面拼上前缀。
    pub fn new(name: impl Into<String>, stage: PostStage, source: impl Into<Arc<str>>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            enabled: true,
            stage,
            source: source.into(),
            passes: Vec::new(),
            scratch: Vec::new(),
            history: None,
            inputs: PostInputs::NONE,
            params: [[0.0; 4]; MAX_PARAMS],
            param_names: Vec::new(),
            textures: [None, None],
        }
    }

    /// 声明一张暂存图，`scale` 是相对输出分辨率的比例（1 = 全分辨率，
    /// 0.5 = 半分辨率）。按声明顺序编号为 `t0`、`t1`…
    pub fn scratch(mut self, scale: f32) -> Self {
        if self.scratch.len() < MAX_SCRATCH {
            self.scratch.push(scale.clamp(1.0 / 64.0, 4.0));
        } else {
            klog::warn!(
                "后处理效果「{}」的暂存图超过 {MAX_SCRATCH} 张，多的忽略",
                self.name
            );
        }
        self
    }

    /// 加一个 pass。按加入的顺序跑。
    pub fn pass(mut self, entry: impl Into<String>, output: PassOutput) -> Self {
        self.passes.push(PostPass {
            entry: entry.into(),
            output,
        });
        self
    }

    /// 让效果拿到上一帧的某个结果（`history_texture`）。
    pub fn history(mut self, source: PassOutput) -> Self {
        self.history = Some(source);
        self
    }

    /// 声明需要的额外输入。见 [`PostInputs`]。
    pub fn inputs(mut self, inputs: PostInputs) -> Self {
        self.inputs |= inputs;
        self
    }

    /// 声明一个参数并给初值。WGSL 里用 `param_<名字>()` 读，得到 `vec4<f32>`。
    ///
    /// **只能在构建时声明**：名字决定了生成的 WGSL，管线编好之后再加
    /// 名字是加不进去的。之后改值用 [`set`](Self::set)。
    pub fn param(mut self, name: impl Into<String>, value: impl IntoParam) -> Self {
        let name = name.into();
        if !is_identifier(&name) {
            klog::warn!("后处理参数名「{name}」不是合法的 WGSL 标识符，忽略");
            return self;
        }
        match self.param_names.iter().position(|n| *n == name) {
            Some(index) => self.params[index] = value.into_param(),
            None if self.param_names.len() < MAX_PARAMS => {
                self.params[self.param_names.len()] = value.into_param();
                self.param_names.push(name);
            }
            None => klog::warn!("后处理效果「{}」的参数超过 {MAX_PARAMS} 个", self.name),
        }
        self
    }

    /// 给用户贴图槽 `slot`（0 或 1）一张图。WGSL 里是 `user0` / `user1`。
    pub fn texture(mut self, slot: usize, texture: Texture) -> Self {
        self.set_texture(slot, Some(texture));
        self
    }

    /// 把离屏相机 `view` 的画面放进用户贴图槽 `slot`（WGSL 里的 `user0` / `user1`）。
    pub fn view(mut self, slot: usize, view: u8) -> Self {
        if let Some(entry) = self.textures.get_mut(slot) {
            *entry = Some(PostTexture::View(view));
        }
        self
    }

    /// 换用户贴图。
    pub fn set_texture(&mut self, slot: usize, texture: Option<Texture>) {
        if let Some(entry) = self.textures.get_mut(slot) {
            *entry = texture.map(PostTexture::Image);
        }
    }

    /// 改一个参数的值。名字没声明过就忽略并告警一次。
    pub fn set(&mut self, name: &str, value: impl IntoParam) {
        match self.param_names.iter().position(|n| n == name) {
            Some(index) => self.params[index] = value.into_param(),
            None => klog::once!(klog::warn!(
                "后处理效果「{}」没有叫「{name}」的参数（参数要在构建时用 `param` 声明）",
                self.name
            )),
        }
    }

    /// 读一个参数的值。
    pub fn get(&self, name: &str) -> Option<[f32; 4]> {
        self.param_names
            .iter()
            .position(|n| n == name)
            .map(|index| self.params[index])
    }

    /// 名字。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 所在阶段。
    pub fn stage(&self) -> PostStage {
        self.stage
    }

    /// 声明的额外输入。
    pub fn required_inputs(&self) -> PostInputs {
        self.inputs
    }

    /// GPU 状态的身份。
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// 各 pass 的入口名。测试用。
    #[cfg(test)]
    pub(crate) fn debug_entries(&self) -> Vec<String> {
        self.passes.iter().map(|pass| pass.entry.clone()).collect()
    }

    /// 拼好前缀之后的完整 WGSL。管线就是用它编的——出错时拿它去对行号。
    pub fn full_source(&self) -> String {
        let mut accessors = String::new();
        for (index, name) in self.param_names.iter().enumerate() {
            accessors.push_str(&format!(
                "fn param_{name}() -> vec4<f32> {{ return params[{index}]; }}\n"
            ));
        }
        format!(
            "{}\n{}\n{}",
            include_str!("postfx_prelude.wgsl"),
            accessors,
            self.source
        )
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 一串效果，按顺序跑。
///
/// 纯数据：放在 `Context` 里给插件改，渲染器每帧照着它执行。
#[derive(Debug, Clone, Default)]
pub struct PostStack {
    effects: Vec<PostEffect>,
}

impl PostStack {
    /// 空的。
    pub fn new() -> Self {
        Self::default()
    }

    /// 加到末尾，返回它的可变引用。
    pub fn push(&mut self, effect: PostEffect) -> &mut PostEffect {
        self.effects.push(effect);
        self.effects.last_mut().expect("刚放进去的")
    }

    /// 插到第 `index` 个位置。
    pub fn insert(&mut self, index: usize, effect: PostEffect) {
        let index = index.min(self.effects.len());
        self.effects.insert(index, effect);
    }

    /// 按名字找（第一个）。
    pub fn get(&self, name: &str) -> Option<&PostEffect> {
        self.effects.iter().find(|e| e.name == name)
    }

    /// 按名字找（第一个），可变。
    pub fn get_mut(&mut self, name: &str) -> Option<&mut PostEffect> {
        self.effects.iter_mut().find(|e| e.name == name)
    }

    /// 按名字删（第一个）。
    pub fn remove(&mut self, name: &str) -> Option<PostEffect> {
        let index = self.effects.iter().position(|e| e.name == name)?;
        Some(self.effects.remove(index))
    }

    /// 开关一个效果。找不到时返回假。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        match self.get_mut(name) {
            Some(effect) => {
                effect.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.effects.clear();
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// 效果个数。
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// 遍历。
    pub fn iter(&self) -> impl Iterator<Item = &PostEffect> {
        self.effects.iter()
    }

    /// 可变遍历。
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut PostEffect> {
        self.effects.iter_mut()
    }

    /// 所有**开着**的效果合起来要哪些输入。
    pub fn required_inputs(&self) -> PostInputs {
        self.effects
            .iter()
            .filter(|e| e.enabled)
            .fold(PostInputs::NONE, |acc, e| acc | e.inputs)
    }
}

// ─────────────────────────── 渲染器那一侧 ───────────────────────────

/// 每帧所有效果共用的 uniform，对应前缀里的 `PostFrame`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct PostFrame {
    pub(crate) view_proj: [[f32; 4]; 4],
    pub(crate) inverse_view_proj: [[f32; 4]; 4],
    pub(crate) clip_view_proj: [[f32; 4]; 4],
    pub(crate) prev_view_proj: [[f32; 4]; 4],
    pub(crate) view: [[f32; 4]; 4],
    pub(crate) projection: [[f32; 4]; 4],
    pub(crate) inverse_projection: [[f32; 4]; 4],
    pub(crate) camera_position: [f32; 4],
    pub(crate) resolution: [f32; 4],
    pub(crate) time: [f32; 4],
    pub(crate) camera: [f32; 4],
    pub(crate) jitter: [f32; 4],
    pub(crate) light: [f32; 4],
    pub(crate) light_color: [f32; 4],
    pub(crate) light_view_proj: [[[f32; 4]; 4]; klight::cascade::MAX_SHADOW_LAYERS],
    pub(crate) cascade_splits: [f32; 4],
    pub(crate) shadow_params: [f32; 4],
}

/// 每个效果自己的小 uniform，对应前缀里的 `EffectInfo`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EffectInfo {
    state: [f32; 4],
}

/// 这一帧效果能读到的东西。没有的给 `None`，绑占位图。
pub(crate) struct FrameInputs<'a> {
    pub(crate) depth: &'a wgpu::TextureView,
    pub(crate) normal: Option<&'a wgpu::TextureView>,
    pub(crate) velocity: Option<&'a wgpu::TextureView>,
    pub(crate) material: Option<&'a wgpu::TextureView>,
    pub(crate) mask: Option<&'a wgpu::TextureView>,
    pub(crate) ao: Option<&'a wgpu::TextureView>,
    pub(crate) scene: &'a wgpu::TextureView,
    pub(crate) views: [Option<&'a wgpu::TextureView>; 2],
    /// 级联阴影图（整个数组）。
    pub(crate) shadow: &'a wgpu::TextureView,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Target {
    fn new(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width,
            height,
        }
    }
}

/// 一个效果在 GPU 上的家当。
struct EffectState {
    params: wgpu::Buffer,
    info: wgpu::Buffer,
    scratch: Vec<Target>,
    history: Option<Target>,
    /// 跑了几帧（重建目标后清零）。
    frames: u32,
    /// 历史纹理里有没有东西。
    history_valid: bool,
    /// 最近一次被用到是第几帧，用来回收删掉的效果。
    last_used: u64,
    /// 建目标时的输出尺寸与格式；变了就重建。
    size: (u32, u32),
    format: wgpu::TextureFormat,
}

/// 一段链的乒乓缓冲。
struct Chain {
    targets: [Target; 2],
    format: wgpu::TextureFormat,
}

/// 渲染器持有的后处理执行器。
pub(crate) struct PostFx {
    layout0: wgpu::BindGroupLayout,
    layout1: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    frame_buffer: wgpu::Buffer,
    linear_sampler: wgpu::Sampler,
    nearest_sampler: wgpu::Sampler,
    repeat_sampler: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
    /// 缺省输入用的占位图。
    black: wgpu::TextureView,
    black_unfilterable: wgpu::TextureView,
    white_unfilterable: wgpu::TextureView,
    black_depth: wgpu::TextureView,
    modules: FxHashMap<Uuid, wgpu::ShaderModule>,
    pipelines: FxHashMap<(Uuid, usize, wgpu::TextureFormat), wgpu::RenderPipeline>,
    failed: FxHashSet<Uuid>,
    states: FxHashMap<Uuid, EffectState>,
    user_textures: FxHashMap<Uuid, GpuTexture>,
    hdr_chain: Option<Chain>,
    ldr_chain: Option<Chain>,
    width: u32,
    height: u32,
    frame: u64,
    /// 拷贝一张图到另一张（格式可以不同）的管线，按目标格式缓存。
    blit_layout: wgpu::BindGroupLayout,
    blit_pipeline_layout: wgpu::PipelineLayout,
    blit_module: wgpu::ShaderModule,
    blit_pipelines: FxHashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
}

/// 做一张 1×1 的纯色图。
pub(crate) fn solid_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    bytes: &[u8],
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
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
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes.len() as u32),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct BlitVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn blit_vs(@builtin(vertex_index) index: u32) -> BlitVertex {
    let ndc = vec2<f32>(f32((index << 1u) & 2u) * 2.0 - 1.0, f32(index & 2u) * 2.0 - 1.0);
    var out: BlitVertex;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

@fragment
fn blit_fs(in: BlitVertex) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, source_sampler, in.uv, 0.0);
}
"#;

impl PostFx {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32) -> Self {
        let texture = |binding: u32, filterable: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform = |binding: u32, size: u64| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        };
        let sampler = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine postfx frame layout"),
            entries: &[
                uniform(0, size_of::<PostFrame>() as u64),
                uniform(1, (MAX_PARAMS * 16) as u64),
                texture(2, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                texture(4, true),
                texture(5, true),
                texture(6, true),
                texture(7, true),
                // 遮蔽图是 Rg32Float，不可过滤。
                texture(8, false),
                texture(9, true),
                texture(10, true),
                sampler(11),
                sampler(12),
                sampler(13),
                uniform(14, size_of::<EffectInfo>() as u64),
                wgpu::BindGroupLayoutEntry {
                    binding: 15,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 16,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let layout1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine postfx effect layout"),
            entries: &(0..6).map(|b| texture(b, true)).collect::<Vec<_>>(),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine postfx pipeline layout"),
            bind_group_layouts: &[Option::from(&layout0), Option::from(&layout1)],
            immediate_size: 0,
        });
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine postfx frame"),
            size: size_of::<PostFrame>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let make_sampler = |filter: wgpu::FilterMode, address: wgpu::AddressMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("kengine postfx sampler"),
                address_mode_u: address,
                address_mode_v: address,
                address_mode_w: address,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine blit layout"),
            entries: &[texture(0, true), sampler(1)],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine blit pipeline layout"),
            bind_group_layouts: &[Option::from(&blit_layout)],
            immediate_size: 0,
        });
        let blit_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });

        Self {
            layout0,
            layout1,
            pipeline_layout,
            frame_buffer,
            linear_sampler: make_sampler(wgpu::FilterMode::Linear, wgpu::AddressMode::ClampToEdge),
            nearest_sampler: make_sampler(
                wgpu::FilterMode::Nearest,
                wgpu::AddressMode::ClampToEdge,
            ),
            repeat_sampler: make_sampler(wgpu::FilterMode::Linear, wgpu::AddressMode::Repeat),
            shadow_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("kengine postfx shadow sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
            black: solid_view(
                device,
                queue,
                "kengine postfx black",
                wgpu::TextureFormat::Rgba8Unorm,
                &[0, 0, 0, 0],
            ),
            black_unfilterable: solid_view(
                device,
                queue,
                "kengine postfx black rg32",
                wgpu::TextureFormat::Rg32Float,
                bytemuck::cast_slice(&[0.0f32, 0.0]),
            ),
            white_unfilterable: solid_view(
                device,
                queue,
                "kengine postfx white rg32",
                wgpu::TextureFormat::Rg32Float,
                bytemuck::cast_slice(&[1.0f32, 1.0]),
            ),
            black_depth: create_depth_placeholder(device),
            modules: FxHashMap::default(),
            pipelines: FxHashMap::default(),
            failed: FxHashSet::default(),
            states: FxHashMap::default(),
            user_textures: FxHashMap::default(),
            hdr_chain: None,
            ldr_chain: None,
            width: width.max(1),
            height: height.max(1),
            frame: 0,
            blit_layout,
            blit_pipeline_layout,
            blit_module,
            blit_pipelines: FxHashMap::default(),
        }
    }

    /// 窗口尺寸变了：链和所有效果的目标下一次用到时重建。
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if (width.max(1), height.max(1)) == (self.width, self.height) {
            return;
        }
        self.width = width.max(1);
        self.height = height.max(1);
        self.hdr_chain = None;
        self.ldr_chain = None;
        // 效果的目标按尺寸检查，在 `ensure_state` 里重建。
    }

    /// 写本帧的公共 uniform。每帧（屏幕那一次）写一遍。
    pub(crate) fn write_frame(&mut self, queue: &wgpu::Queue, frame: &PostFrame) {
        self.frame += 1;
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(frame));
    }

    /// 编一个效果的某个 pass。失败的效果记下来，不每帧重试。
    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        effect: &PostEffect,
        pass: usize,
        format: wgpu::TextureFormat,
    ) -> bool {
        let key = (effect.id, pass, format);
        if self.pipelines.contains_key(&key) {
            return true;
        }
        if self.failed.contains(&effect.id) {
            return false;
        }
        if !self.modules.contains_key(&effect.id) {
            let source = effect.full_source();
            // 先过一遍 naga：wgpu 遇到坏的 WGSL 会直接 panic（或者进
            // 错误回调、之后整台设备不可用），而一个手滑写错的效果不该
            // 让整个游戏崩掉。
            if let Err(error) = kshader::Shader::from_wgsl(source.clone()) {
                klog::error!("后处理效果「{}」编译失败：{error}", effect.name);
                self.failed.insert(effect.id);
                return false;
            }
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&effect.name),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            self.modules.insert(effect.id, module);
        }
        let module = &self.modules[&effect.id];
        let entry = &effect.passes[pass].entry;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&effect.name),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("post_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(key, pipeline);
        true
    }

    /// 保证效果的缓冲与目标齐全、尺寸对得上。
    fn ensure_state(
        &mut self,
        device: &wgpu::Device,
        effect: &PostEffect,
        format: wgpu::TextureFormat,
    ) {
        let size = (self.width, self.height);
        let stale = self
            .states
            .get(&effect.id)
            .is_none_or(|state| state.size != size || state.format != format);
        if !stale {
            return;
        }
        let scratch = effect
            .scratch
            .iter()
            .map(|scale| {
                Target::new(
                    device,
                    "kengine postfx scratch",
                    (size.0 as f32 * scale).round() as u32,
                    (size.1 as f32 * scale).round() as u32,
                    crate::post::HDR_FORMAT,
                )
            })
            .collect::<Vec<_>>();
        let history = effect.history.map(|source| match source {
            PassOutput::Out => {
                Target::new(device, "kengine postfx history", size.0, size.1, format)
            }
            PassOutput::Scratch(index) => {
                let (w, h) = scratch
                    .get(index as usize)
                    .map_or(size, |target| (target.width, target.height));
                Target::new(
                    device,
                    "kengine postfx history",
                    w,
                    h,
                    crate::post::HDR_FORMAT,
                )
            }
        });
        let (params, info) = match self.states.remove(&effect.id) {
            Some(old) => (old.params, old.info),
            None => (
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("kengine postfx params"),
                    size: (MAX_PARAMS * 16) as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("kengine postfx info"),
                    size: size_of::<EffectInfo>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            ),
        };
        self.states.insert(
            effect.id,
            EffectState {
                params,
                info,
                scratch,
                history,
                frames: 0,
                history_valid: false,
                last_used: self.frame,
                size,
                format,
            },
        );
    }

    pub(crate) fn ensure_chain(
        &mut self,
        device: &wgpu::Device,
        stage: PostStage,
        format: wgpu::TextureFormat,
    ) {
        let slot = match stage {
            PostStage::Hdr => &mut self.hdr_chain,
            PostStage::Ldr => &mut self.ldr_chain,
        };
        if slot.as_ref().is_some_and(|chain| chain.format == format) {
            return;
        }
        let make = || {
            Target::new(
                device,
                "kengine postfx chain",
                self.width,
                self.height,
                format,
            )
        };
        *slot = Some(Chain {
            targets: [make(), make()],
            format,
        });
    }

    /// 某一段链上第 `index` 张的纹理本体（截图要拷它）。
    pub(crate) fn chain_texture(&self, stage: PostStage, index: usize) -> Option<&wgpu::Texture> {
        let chain = match stage {
            PostStage::Hdr => self.hdr_chain.as_ref(),
            PostStage::Ldr => self.ldr_chain.as_ref(),
        }?;
        Some(&chain.targets[index & 1].texture)
    }

    /// 某一段链上第 `index` 张的视图。
    pub(crate) fn chain_view(&self, stage: PostStage, index: usize) -> Option<&wgpu::TextureView> {
        let chain = match stage {
            PostStage::Hdr => self.hdr_chain.as_ref(),
            PostStage::Ldr => self.ldr_chain.as_ref(),
        }?;
        Some(&chain.targets[index & 1].view)
    }

    fn user_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) {
        self.user_textures
            .entry(texture.id())
            .or_insert_with(|| crate::upload_texture(device, queue, texture));
    }

    /// 跑一段链上的所有效果。
    ///
    /// `input` 是这一段的起点；`input_in_chain` 为 `Some(i)` 时表示起点就是
    /// 链上第 `i` 张（LDR 段：合成已经写进了链的第 0 张）。
    ///
    /// 返回最后一个效果写到了链上的哪一张；一个都没跑时返回 `None`，
    /// 调用方接着用 `input`。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effects: &[&PostEffect],
        stage: PostStage,
        format: wgpu::TextureFormat,
        input: Option<&wgpu::TextureView>,
        input_in_chain: Option<usize>,
        inputs: &FrameInputs<'_>,
    ) -> Option<usize> {
        let runnable: Vec<&PostEffect> = effects
            .iter()
            .copied()
            .filter(|effect| effect.enabled && effect.stage == stage && !effect.passes.is_empty())
            .collect();
        if runnable.is_empty() {
            return None;
        }
        self.ensure_chain(device, stage, format);

        // 先把所有要的东西（管线、目标、贴图）备齐，再开始录命令——
        // 录命令时要借用 self 里的视图，那之后就不能再改 self 了。
        let mut ready = Vec::with_capacity(runnable.len());
        for effect in runnable {
            let mut ok = true;
            for pass in 0..effect.passes.len() {
                let pass_format = match effect.passes[pass].output {
                    PassOutput::Out => format,
                    PassOutput::Scratch(_) => crate::post::HDR_FORMAT,
                };
                ok &= self.pipeline(device, effect, pass, pass_format);
            }
            if !ok {
                continue;
            }
            self.ensure_state(device, effect, format);
            for texture in effect.textures.iter().flatten() {
                if let PostTexture::Image(texture) = texture {
                    self.user_texture(device, queue, texture);
                }
            }
            ready.push(effect);
        }
        if ready.is_empty() {
            return None;
        }

        // 当前输入在链上的哪一张（None = 外部的 `input`）。
        let mut current = input_in_chain;
        for effect in ready {
            let output = match current {
                Some(index) => 1 - (index & 1),
                None => 0,
            };
            self.run_effect(
                device, queue, encoder, effect, stage, format, input, current, output, inputs,
            );
            current = Some(output);
        }
        // 顺手回收删掉的效果：一段时间没用到的状态放掉。
        let frame = self.frame;
        self.states
            .retain(|_, state| frame.saturating_sub(state.last_used) < 120);
        current
    }

    #[allow(clippy::too_many_arguments)]
    fn run_effect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        effect: &PostEffect,
        stage: PostStage,
        format: wgpu::TextureFormat,
        external_input: Option<&wgpu::TextureView>,
        input_index: Option<usize>,
        output_index: usize,
        inputs: &FrameInputs<'_>,
    ) {
        let frame = self.frame;
        {
            let state = self.states.get_mut(&effect.id).expect("ensure_state 建过");
            state.last_used = frame;
            queue.write_buffer(&state.params, 0, bytemuck::cast_slice(&effect.params));
            queue.write_buffer(
                &state.info,
                0,
                bytemuck::bytes_of(&EffectInfo {
                    state: [
                        state.frames as f32,
                        if state.history_valid { 1.0 } else { 0.0 },
                        0.0,
                        0.0,
                    ],
                }),
            );
        }

        let chain = match stage {
            PostStage::Hdr => self.hdr_chain.as_ref(),
            PostStage::Ldr => self.ldr_chain.as_ref(),
        }
        .expect("ensure_chain 建过");
        let state = &self.states[&effect.id];
        let input_view = match input_index {
            Some(index) => &chain.targets[index & 1].view,
            None => external_input.unwrap_or(inputs.scene),
        };
        let output_target = &chain.targets[output_index & 1];

        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine postfx frame group"),
            layout: &self.layout0,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: state.params.as_entire_binding(),
                },
                tex(2, input_view),
                tex(3, inputs.depth),
                tex(4, inputs.normal.unwrap_or(&self.black)),
                tex(5, inputs.velocity.unwrap_or(&self.black)),
                tex(6, inputs.mask.unwrap_or(&self.black)),
                tex(7, state.history.as_ref().map_or(&self.black, |t| &t.view)),
                tex(8, inputs.ao.unwrap_or(&self.white_unfilterable)),
                tex(9, inputs.material.unwrap_or(&self.black)),
                tex(10, inputs.scene),
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::Sampler(&self.nearest_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::Sampler(&self.repeat_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: state.info.as_entire_binding(),
                },
                tex(15, inputs.shadow),
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
            ],
        });
        let _ = &self.black_unfilterable;
        let _ = &self.black_depth;

        for (pass_index, pass) in effect.passes.iter().enumerate() {
            let pass_format = match pass.output {
                PassOutput::Out => format,
                PassOutput::Scratch(_) => crate::post::HDR_FORMAT,
            };
            let Some(pipeline) = self.pipelines.get(&(effect.id, pass_index, pass_format)) else {
                continue;
            };
            let target_view = match pass.output {
                PassOutput::Out => &output_target.view,
                PassOutput::Scratch(index) => match state.scratch.get(index as usize) {
                    Some(target) => &target.view,
                    None => {
                        klog::once!(klog::warn!(
                            "后处理效果「{}」写了没声明的暂存图 t{index}",
                            effect.name
                        ));
                        continue;
                    }
                },
            };
            let scratch_view = |index: usize| -> &wgpu::TextureView {
                if pass.output == PassOutput::Scratch(index as u8) {
                    // 正在写的那张不能同时读。
                    return &self.black;
                }
                state.scratch.get(index).map_or(&self.black, |t| &t.view)
            };
            let user = |slot: usize| -> &wgpu::TextureView {
                match &effect.textures[slot] {
                    Some(PostTexture::Image(texture)) => self
                        .user_textures
                        .get(&texture.id())
                        .map_or(&self.black, |gpu| &gpu.view),
                    Some(PostTexture::View(view)) => inputs
                        .views
                        .get(usize::from(*view))
                        .copied()
                        .flatten()
                        .unwrap_or(&self.black),
                    None => &self.black,
                }
            };
            let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kengine postfx effect group"),
                layout: &self.layout1,
                entries: &[
                    tex(0, scratch_view(0)),
                    tex(1, scratch_view(1)),
                    tex(2, scratch_view(2)),
                    tex(3, scratch_view(3)),
                    tex(4, user(0)),
                    tex(5, user(1)),
                ],
            });
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&effect.name),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, &group0, &[]);
            render_pass.set_bind_group(1, &group1, &[]);
            render_pass.draw(0..3, 0..1);
        }

        // 历史：把这一帧的结果拷一份，下一帧读。
        let mut copied = false;
        if let (Some(source), Some(history)) = (effect.history, state.history.as_ref()) {
            let source_target = match source {
                PassOutput::Out => Some(output_target),
                PassOutput::Scratch(index) => state.scratch.get(index as usize),
            };
            if let Some(source_target) = source_target
                && source_target.width == history.width
                && source_target.height == history.height
                && source_target.texture.format() == history.texture.format()
            {
                encoder.copy_texture_to_texture(
                    source_target.texture.as_image_copy(),
                    history.texture.as_image_copy(),
                    wgpu::Extent3d {
                        width: history.width,
                        height: history.height,
                        depth_or_array_layers: 1,
                    },
                );
                copied = true;
            }
        }
        let state = self.states.get_mut(&effect.id).expect("上面还在");
        state.frames = state.frames.saturating_add(1);
        state.history_valid |= copied;
    }

    /// 先把 [`blit`](Self::blit) 要的管线建好。
    ///
    /// 分成两步是为了借用：拷贝的源经常就是这里面的一张链上的图，
    /// 那时 `blit` 只能拿 `&self`。
    pub(crate) fn prepare_blit(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        additive: bool,
    ) {
        self.blit_pipelines
            .entry((format, additive))
            .or_insert_with(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("kengine blit"),
                    layout: Some(&self.blit_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &self.blit_module,
                        entry_point: Some("blit_vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &self.blit_module,
                        entry_point: Some("blit_fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: additive.then_some(wgpu::BlendState {
                                color: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::Constant,
                                    dst_factor: wgpu::BlendFactor::One,
                                    operation: wgpu::BlendOperation::Add,
                                },
                                alpha: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::Constant,
                                    dst_factor: wgpu::BlendFactor::One,
                                    operation: wgpu::BlendOperation::Add,
                                },
                            }),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            });
    }

    /// 把 `source` 原样画到 `target` 上（格式可以不同）。先调 [`prepare_blit`](Self::prepare_blit)。
    ///
    /// `accumulate` 给了 `(权重, 是否第一遍)` 的话按「源 × 权重 + 目标」混合——
    /// 超采样累积用；第一遍先清零。
    pub(crate) fn blit(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        accumulate: Option<(f64, bool)>,
    ) {
        let Some(pipeline) = self.blit_pipelines.get(&(format, accumulate.is_some())) else {
            klog::once!(klog::warn!("拷贝管线没有预先建好（漏调了 prepare_blit）"));
            return;
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine blit group"),
            layout: &self.blit_layout,
            entries: &[
                tex(0, source),
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                },
            ],
        });
        let load = match accumulate {
            Some((_, false)) => wgpu::LoadOp::Load,
            _ => wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        if let Some((weight, _)) = accumulate {
            pass.set_blend_constant(wgpu::Color {
                r: weight,
                g: weight,
                b: weight,
                a: weight,
            });
        }
        pass.draw(0..3, 0..1);
    }
}

fn tex(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn create_depth_placeholder(device: &wgpu::Device) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine postfx depth placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prelude_passes_validation_on_its_own() {
        let effect = PostEffect::new(
            "empty",
            PostStage::Hdr,
            "@fragment fn main(in: PostVertex) -> @location(0) vec4<f32> { return sample_input(in.uv); }",
        );
        kshader::Shader::from_wgsl(effect.full_source()).expect("前缀 + 最简效果应当通过校验");
    }

    #[test]
    fn declared_params_get_an_accessor() {
        let effect = PostEffect::new(
            "tint",
            PostStage::Ldr,
            "@fragment fn main(in: PostVertex) -> @location(0) vec4<f32> { return sample_input(in.uv) * param_tint(); }",
        )
        .param("tint", Vec3::new(1.0, 0.5, 0.25));
        kshader::Shader::from_wgsl(effect.full_source()).expect("参数访问函数应当生成出来");
        assert_eq!(effect.get("tint"), Some([1.0, 0.5, 0.25, 0.0]));
    }

    #[test]
    fn set_only_touches_declared_params() {
        let mut effect = PostEffect::new("a", PostStage::Hdr, "").param("x", 1.0);
        effect.set("x", 2.0);
        effect.set("y", 3.0);
        assert_eq!(effect.get("x"), Some([2.0, 0.0, 0.0, 0.0]));
        assert_eq!(effect.get("y"), None);
    }

    #[test]
    fn bad_param_names_are_rejected() {
        let effect = PostEffect::new("a", PostStage::Hdr, "")
            .param("not valid", 1.0)
            .param("1x", 1.0);
        assert!(effect.param_names.is_empty());
    }

    #[test]
    fn the_frame_uniform_is_sixteen_byte_aligned() {
        assert_eq!(size_of::<PostFrame>() % 16, 0);
        assert_eq!(size_of::<PostFrame>(), 7 * 64 + 7 * 16 + 6 * 64 + 2 * 16);
    }

    #[test]
    fn inputs_combine() {
        let inputs = PostInputs::NORMAL | PostInputs::MASK;
        assert!(inputs.contains(PostInputs::NORMAL));
        assert!(inputs.contains(PostInputs::MASK));
        assert!(!inputs.contains(PostInputs::VELOCITY));
        assert!(inputs.needs_prepass());
        assert!(!PostInputs::MASK.needs_prepass());
    }

    #[test]
    fn the_stack_finds_effects_by_name() {
        let mut stack = PostStack::new();
        stack.push(PostEffect::new("a", PostStage::Hdr, "").inputs(PostInputs::VELOCITY));
        stack.push(PostEffect::new("b", PostStage::Ldr, "").inputs(PostInputs::MASK));
        assert!(stack.get("b").is_some());
        assert_eq!(
            stack.required_inputs(),
            PostInputs::VELOCITY | PostInputs::MASK
        );
        stack.set_enabled("a", false);
        assert_eq!(stack.required_inputs(), PostInputs::MASK);
        assert!(stack.remove("a").is_some());
        assert_eq!(stack.len(), 1);
    }
}
