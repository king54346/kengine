//! 后处理链：HDR 目标 → Bloom → 色调映射 → 屏幕。
//!
//! 这里只放**固定**的那几步（辉光、色调映射、FXAA）。用户自己写的全屏
//! pass 走 [`crate::postfx`]，插在这条链的前后：
//!
//! ```text
//! 场景 HDR ─→ [HDR 阶段的效果…] ─→ Bloom + 色调映射 ─→ [LDR 阶段的效果…] ─→ 抗锯齿 ─→ 屏幕
//! ```

use crate::tonemap::ToneMapping;
use bytemuck::{Pod, Zeroable};
use std::num::NonZeroU64;

/// 主 pass 的渲染目标格式。
///
/// 必须是浮点格式：PBR 输出的高光远超过 1，8 位归一化格式会在色调映射之前就把它们切掉，
/// Bloom 也就无从提取。
pub(crate) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 辉光链最多几级。第 0 级是半分辨率，每往下一级再减半。
///
/// 六级在 1080p 下最小的一级是 16 像素高，再往下只剩一团均匀的颜色，
/// 加进去只是把整个画面抬亮一点。
const BLOOM_MAX_LEVELS: u32 = 6;

/// 每个 pass 的参数在缓冲里占多少字节。动态偏移要按 256 对齐。
const PARAMS_STRIDE: u64 = 256;

/// 一帧最多要几份参数：提取 1 + 降采样 5 + 升采样 5 + 合成 1。
const PARAMS_SLOTS: u64 = 2 + 2 * BLOOM_MAX_LEVELS as u64;

/// 后处理参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostSettings {
    /// 亮部提取阈值。低于此亮度的像素不参与 Bloom。
    pub bloom_threshold: f32,
    /// Bloom 混合强度。为 0 时相当于关闭 Bloom（整条链都不跑）。
    pub bloom_intensity: f32,
    /// 光晕有多大，`[0, 1]`。
    ///
    /// 辉光是一条降采样链，每一级比上一级宽一倍；这个值决定往回合的时候
    /// 宽的那几级占多少分量。0 是只贴着亮部的一圈，1 是铺开一大片。
    /// 和 three.js `UnrealBloomPass` 的 `radius` 是同一个意思。
    pub bloom_radius: f32,
    /// 只让遮罩的某个通道（0..=3）里的东西发光。`None` 是全场景按阈值。
    ///
    /// 配合 [`Node::post_mask`](kscene::Node::post_mask)：把要发光的物体标上
    /// 遮罩位，这里指定那一位，就是 three.js 的「选择性辉光」。
    /// 被挡住的部分不发光（遮罩里是 0.5，达不到阈值）。
    pub bloom_mask: Option<u8>,
    /// 色调映射算子。
    pub tone_mapping: ToneMapping,
    /// 曝光。色调映射**之前**乘上去的整体倍数，默认 1。
    ///
    /// 这不是「调亮度」的美化开关，而是 HDR 管线里必须有的一环：
    /// 场景的辐射度是有量纲的（一盏 1000 流明的灯就是 1000），
    /// 而色调映射曲线的拐点固定在 1 附近。没有曝光这个自由度的话，
    /// 室内和正午户外两个场景只能二选一地调好看。
    ///
    /// 乘在色调映射之前而不是之后：之后乘等于把已经压好的曲线整体拉伸，
    /// 高光会重新超出 1、又被硬切掉。
    pub exposure: f32,
    /// 抗锯齿。
    pub anti_alias: AntiAlias,
    /// 超采样：每帧把场景画几遍（每遍挪一个亚像素）再平均。1 是关。
    ///
    /// 最干净也最贵的抗锯齿——N 遍就是 N 倍的场景开销。适合静帧截图、
    /// 产品展示这类「帧率不要紧、边缘要干净」的场合。
    /// 开着它时 [`AntiAlias::Taa`] 的抖动让位给它。
    pub ssaa: u32,
    /// 渲染分辨率相对窗口的比例，`[0.25, 1]`（three.js 的 `pass.setResolutionScale`）。
    ///
    /// 场景、阴影以外的离屏目标、后处理都按这个比例的分辨率跑，最后一步放大到窗口尺寸
    /// （怎么放大见 [`upscaling`](Self::upscaling)）；UI 始终是窗口原分辨率。像素着色重的场景拿它换帧率。
    pub render_scale: f32,
    /// 低分辨率画面怎么放大到窗口（`render_scale < 1` 时才有意义）。
    pub upscaling: Upscaling,
    /// 放大后 RCAS 锐化的量：0 最锐，每加 1 锐化减半（FSR1 和 TAAU 共用）。three.js 默认 0.2。
    pub upscale_sharpness: f32,
    /// TAAU 之后要不要再锐化一次（FSR1 的 RCAS 是它自己的一部分，总是做）。
    pub upscale_sharpening: bool,
}

/// 低分辨率渲染之后放大到窗口的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Upscaling {
    /// 双线性：最后一个全屏 pass 按 UV 采样，自然就放大了。最便宜，也最糊。
    #[default]
    Bilinear,
    /// AMD FidelityFX Super Resolution 1：边缘自适应放大（EASU）+ 对比度自适应锐化（RCAS），
    /// 两个全屏 pass。边缘比双线性锐利得多；开着它时 FXAA 让位（FSR 期望输入已经抗过锯齿或者干脆不抗）。
    Fsr1,
    /// 时间性放大：渲染时每帧抖动一个亚像素，在**屏幕分辨率**的历史图上按运动向量累积（顺带抗锯齿，
    /// 内置的 TAA / FXAA 让位）。静止或慢动时最清楚，快速运动时会糊一点。会打开预通道（要运动向量）。
    Taau,
}

impl Default for PostSettings {
    fn default() -> Self {
        Self {
            bloom_threshold: 1.0,
            bloom_intensity: 0.06,
            bloom_radius: 0.5,
            bloom_mask: None,
            tone_mapping: ToneMapping::default(),
            exposure: 1.0,
            anti_alias: AntiAlias::default(),
            ssaa: 1,
            render_scale: 1.0,
            upscaling: Upscaling::Bilinear,
            upscale_sharpness: 0.2,
            upscale_sharpening: true,
        }
    }
}

/// 抗锯齿方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AntiAlias {
    /// 不做。边缘会有锯齿。
    None,
    /// FXAA：纯后处理，一个 pass。
    ///
    /// 代价是细小的文字和纹理细节会略糊。
    #[default]
    Fxaa,
    /// SMAA 风格的形态学抗锯齿：找边、沿边搜出线段的形状、按覆盖率混合。
    ///
    /// 比 FXAA 锐利（不碰不是边的地方），三个 pass。
    /// 见 [`crate::effects::smaa`] 里写明的和原版 SMAA 的差别。
    Smaa,
    /// 时间性抗锯齿：每帧挪一个亚像素，靠运动向量把历史帧对齐后累积。
    ///
    /// 静止画面上效果最好（等价于几十倍超采样），代价是运动时略糊、
    /// 会顺带打开预通道（要运动向量）。
    Taa,
}

/// 后处理 pass 的 uniform，对应 `post.wgsl` 的 `PostParams`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostParams {
    /// x = 阈值，y = 强度，z = 算子编号，w = 曝光
    settings: [f32; 4],
    /// xy = 纹素尺寸，z = 软阈值宽度，w 保留
    texel: [f32; 4],
    /// x = 遮罩通道（<0 不用），y = 升采样权重，z = 归一化系数，w 保留
    extra: [f32; 4],
    padding: [f32; 4],
}

/// FSR1 两个 pass 的 uniform，对应 `fsr.wgsl` 的 `Params`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FsrParams {
    input_size: [f32; 2],
    output_size: [f32; 2],
    sharpness: f32,
    _pad: [f32; 3],
}

/// TAAU pass 的 uniform，对应 `taau.wgsl` 的 `Params`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TaauParams {
    input_size: [f32; 2],
    output_size: [f32; 2],
    jitter: [f32; 2],
    history_valid: f32,
    _pad: f32,
}

/// TAAU 的历史图格式：半精度，累积几十帧也不出色带。
const TAAU_HISTORY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// FXAA pass 的 uniform，对应 `fxaa.wgsl` 的 `Params`。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FxaaParams {
    texel: [f32; 2],
    threshold_min: f32,
    threshold_max: f32,
}

/// 一组尺寸相关的离屏纹理。窗口尺寸变化时整体重建。
struct Targets {
    hdr: wgpu::TextureView,
    /// HDR 纹理本体。
    ///
    /// 视图没法当拷贝的源，而不透明 pass 画完之后要把它整个拷一份出去
    /// 给材质采样（屏幕空间折射）。
    hdr_texture: wgpu::Texture,
    /// 辉光链：一张带 mip 的纹理，每级一个视图。
    bloom_levels: Vec<wgpu::TextureView>,
    /// 每级的像素尺寸。
    bloom_sizes: Vec<(u32, u32)>,
    width: u32,
    height: u32,
}

/// 后处理链。
pub(crate) struct PostProcess {
    settings: PostSettings,
    targets: Targets,

    params_layout: wgpu::BindGroupLayout,
    bloom_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,

    extract_pipeline: wgpu::RenderPipeline,
    down_pipeline: wgpu::RenderPipeline,
    up_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,

    /// 所有 pass 的参数在一块缓冲里，按 [`PARAMS_STRIDE`] 分段、动态偏移寻址。
    ///
    /// 分开写到同一块缓冲的同一个位置不行：`queue.write_buffer` 在提交时
    /// 才生效，同一帧里写两次，两个 pass 读到的都是最后那次。
    params_buffer: wgpu::Buffer,
    /// 降采样与升采样的绑定组：源是链上的某一级，和窗口尺寸一起重建。
    down_bind_groups: Vec<wgpu::BindGroup>,
    up_bind_groups: Vec<wgpu::BindGroup>,
    /// 合成 pass 采辉光链第 0 级。
    bloom_bind_group: wgpu::BindGroup,
    /// 没有遮罩时顶上的 1×1。
    dummy_mask: wgpu::TextureView,

    fxaa_pipeline: wgpu::RenderPipeline,
    fxaa_layout: wgpu::BindGroupLayout,
    fxaa_params: wgpu::Buffer,
    /// FSR1：EASU、RCAS 两条管线（布局和 FXAA 的一样：uniform + 贴图 + 采样器）。
    fsr_easu: wgpu::RenderPipeline,
    fsr_layout: wgpu::BindGroupLayout,
    fsr_rcas: wgpu::RenderPipeline,
    fsr_params: wgpu::Buffer,
    /// EASU 的输出、RCAS 的输入：窗口分辨率的中间图。按需建，尺寸变了重建。
    fsr_intermediate: Option<(wgpu::Texture, wgpu::TextureView)>,
    surface_format: wgpu::TextureFormat,
    /// TAAU：管线、布局、参数，两张轮流读写的历史图（屏幕分辨率）。
    taau_pipeline: wgpu::RenderPipeline,
    taau_layout: wgpu::BindGroupLayout,
    taau_params: wgpu::Buffer,
    taau_history: [Option<(wgpu::Texture, wgpu::TextureView)>; 2],
    /// 这一帧写哪张。
    taau_write: usize,
    /// 上一帧走了 TAAU、历史图可用。
    taau_valid: bool,
    /// 合成 / FXAA 输出的格式（交换链的格式）。
    ldr_format: wgpu::TextureFormat,
}

impl PostProcess {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine post params layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(size_of::<PostParams>() as u64),
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
            ],
        });
        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine post bloom layout"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine post shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("post.wgsl").into()),
        });

        // 提取与模糊只读 group 0；合成还要采样 Bloom，因此多一个 group。
        // 布局分开是必须的：若给模糊管线也声明 group 1，就得绑一张贴图上去，
        // 而那张贴图正是本 pass 的渲染目标，wgpu 会判定用法冲突。
        let simple_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine post simple layout"),
            bind_group_layouts: &[Option::from(&params_layout)],
            immediate_size: 0,
        });
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine post composite layout"),
            bind_group_layouts: &[Option::from(&params_layout), Option::from(&bloom_layout)],
            immediate_size: 0,
        });

        let make_pipeline = |label: &str,
                             entry: &str,
                             format: wgpu::TextureFormat,
                             layout: &wgpu::PipelineLayout,
                             blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("fullscreen_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                // 全屏 pass 不需要深度。
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // 源 + 目标 × 混合常数：往上合的时候，目标那一级先乘上它自己的权重。
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::Constant,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        let extract_pipeline = make_pipeline(
            "kengine bloom extract",
            "bloom_extract_fs",
            HDR_FORMAT,
            &simple_layout,
            wgpu::BlendState::REPLACE,
        );
        let down_pipeline = make_pipeline(
            "kengine bloom down",
            "bloom_down_fs",
            HDR_FORMAT,
            &simple_layout,
            wgpu::BlendState::REPLACE,
        );
        let up_pipeline = make_pipeline(
            "kengine bloom up",
            "bloom_up_fs",
            HDR_FORMAT,
            &simple_layout,
            additive,
        );
        let composite_pipeline = make_pipeline(
            "kengine post composite",
            "composite_fs",
            surface_format,
            &composite_layout,
            wgpu::BlendState::REPLACE,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kengine post sampler"),
            // 采样必须夹边：模糊时会越界取样，重复会把对侧画面卷进来。
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine post params"),
            size: PARAMS_STRIDE * PARAMS_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dummy_mask = crate::postfx::solid_view(
            device,
            queue,
            "kengine post dummy mask",
            wgpu::TextureFormat::Rgba8Unorm,
            &[0, 0, 0, 0],
        );

        // ── FXAA ──
        let fxaa_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine fxaa shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fxaa.wgsl").into()),
        });
        let fxaa_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine fxaa layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<FxaaParams>() as u64),
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
            ],
        });
        let fxaa_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine fxaa pipeline layout"),
            bind_group_layouts: &[Option::from(&fxaa_layout)],
            immediate_size: 0,
        });
        let fxaa_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine fxaa pipeline"),
            layout: Some(&fxaa_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &fxaa_shader,
                entry_point: Some("fxaa_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &fxaa_shader,
                entry_point: Some("fxaa_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
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
        let fsr_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine fsr shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fsr.wgsl").into()),
        });
        // 和 FXAA 同样的三项绑定，只是 uniform 大一些（32 字节），布局得单独建。
        let fsr_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine fsr layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<FsrParams>() as u64),
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
            ],
        });
        let fsr_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine fsr pipeline layout"),
            bind_group_layouts: &[Option::from(&fsr_layout)],
            immediate_size: 0,
        });
        let fsr_pipeline = |entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&fsr_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &fsr_shader,
                    entry_point: Some("fsr_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &fsr_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let taau_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine taau shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("taau.wgsl").into()),
        });
        let taau_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine taau layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<TaauParams>() as u64),
                    },
                    count: None,
                },
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let taau_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine taau pipeline layout"),
            bind_group_layouts: &[Option::from(&taau_layout)],
            immediate_size: 0,
        });
        let taau_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine taau"),
            layout: Some(&taau_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &taau_shader,
                entry_point: Some("taau_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &taau_shader,
                entry_point: Some("taau_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TAAU_HISTORY_FORMAT,
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
        let taau_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine taau params"),
            size: size_of::<TaauParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fsr_easu = fsr_pipeline("easu_fs");
        let fsr_rcas = fsr_pipeline("rcas_fs");
        let fsr_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine fsr params"),
            size: size_of::<FsrParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let fxaa_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine fxaa params"),
            size: size_of::<FxaaParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let targets = create_targets(device, width, height);
        let (down_bind_groups, up_bind_groups, bloom_bind_group) = create_chain_bind_groups(
            device,
            &params_layout,
            &bloom_layout,
            &params_buffer,
            &sampler,
            &dummy_mask,
            &targets,
        );

        Self {
            settings: PostSettings::default(),
            targets,
            params_layout,
            bloom_layout,
            sampler,
            extract_pipeline,
            down_pipeline,
            up_pipeline,
            composite_pipeline,
            params_buffer,
            down_bind_groups,
            up_bind_groups,
            bloom_bind_group,
            dummy_mask,
            fxaa_pipeline,
            fxaa_layout,
            fxaa_params,
            fsr_easu,
            fsr_layout,
            fsr_rcas,
            fsr_params,
            fsr_intermediate: None,
            surface_format,
            taau_pipeline,
            taau_layout,
            taau_params,
            taau_history: [None, None],
            taau_write: 0,
            taau_valid: false,
            ldr_format: surface_format,
        }
    }

    /// HDR 目标的纹理本体，拷贝场景颜色时用。
    pub(crate) fn hdr_texture(&self) -> &wgpu::Texture {
        &self.targets.hdr_texture
    }

    /// 主 pass 应当渲染到的 HDR 目标。
    pub(crate) fn hdr_target(&self) -> &wgpu::TextureView {
        &self.targets.hdr
    }

    /// 合成与抗锯齿输出的格式。
    pub(crate) fn ldr_format(&self) -> wgpu::TextureFormat {
        self.ldr_format
    }

    /// 当前设置。
    pub(crate) fn settings(&self) -> PostSettings {
        self.settings
    }

    /// 修改设置。
    pub(crate) fn set_settings(&mut self, settings: PostSettings) {
        self.settings = settings;
    }

    /// 窗口尺寸变化时重建离屏目标。
    pub(crate) fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width.max(1) == self.targets.width && height.max(1) == self.targets.height {
            return;
        }
        self.targets = create_targets(device, width, height);
        let (down, up, bloom) = create_chain_bind_groups(
            device,
            &self.params_layout,
            &self.bloom_layout,
            &self.params_buffer,
            &self.sampler,
            &self.dummy_mask,
            &self.targets,
        );
        self.down_bind_groups = down;
        self.up_bind_groups = up;
        self.bloom_bind_group = bloom;
    }

    /// Bloom + 色调映射：读 `source`（HDR），写 `target`（LDR 格式）。
    ///
    /// `mask` 是后处理遮罩，只在 [`PostSettings::bloom_mask`] 指定了通道时
    /// 有用；没有就给 `None`。
    pub(crate) fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        mask: Option<&wgpu::TextureView>,
        target: &wgpu::TextureView,
    ) {
        let settings = self.settings;
        let levels = self.targets.bloom_levels.len();
        let bloom_on = settings.bloom_intensity > 0.0 && levels > 0;
        let exposure = if settings.exposure.is_finite() {
            // 负数或 NaN 会让整个画面变成黑屏或花屏，在这里夹住比在
            // 着色器里判断便宜。
            settings.exposure.clamp(0.0, 1000.0)
        } else {
            1.0
        };
        let weights = bloom_level_weights(settings.bloom_radius, levels);
        let normalization: f32 = weights.iter().sum::<f32>().max(1e-4);
        let base = [
            settings.bloom_threshold.max(0.0),
            settings.bloom_intensity.max(0.0),
            settings.tone_mapping.index() as f32,
            exposure,
        ];
        let mask_channel = settings.bloom_mask.map_or(-1.0, |c| f32::from(c.min(3)));

        // 参数表：0 = 提取，1..levels = 降采样，levels..2·levels-1 = 升采样，最后一个 = 合成。
        let mut blob = vec![0u8; (PARAMS_STRIDE * PARAMS_SLOTS) as usize];
        let mut write = |slot: usize, params: PostParams| {
            let start = slot * PARAMS_STRIDE as usize;
            blob[start..start + size_of::<PostParams>()]
                .copy_from_slice(bytemuck::bytes_of(&params));
        };
        let texel_of = |(w, h): (u32, u32)| [1.0 / w.max(1) as f32, 1.0 / h.max(1) as f32];
        let full = (self.targets.width, self.targets.height);
        let [fx, fy] = texel_of(full);
        write(
            0,
            PostParams {
                settings: base,
                texel: [fx, fy, (settings.bloom_threshold * 0.5).max(1e-3), 0.0],
                extra: [mask_channel, 1.0, normalization, 0.0],
                padding: [0.0; 4],
            },
        );
        for level in 1..levels {
            let [tx, ty] = texel_of(self.targets.bloom_sizes[level - 1]);
            write(
                level,
                PostParams {
                    settings: base,
                    texel: [tx, ty, 0.0, 0.0],
                    extra: [-1.0, 1.0, normalization, 0.0],
                    padding: [0.0; 4],
                },
            );
        }
        let last = levels.saturating_sub(1);
        for level in (0..last).rev() {
            // 读第 level+1 级，写第 level 级。最小那一级第一次被读时还没乘过
            // 自己的权重（没有更小的级往它身上合），在着色器输出上补乘。
            let [tx, ty] = texel_of(self.targets.bloom_sizes[level + 1]);
            let source_weight = if level + 1 == last {
                weights[last]
            } else {
                1.0
            };
            write(
                BLOOM_MAX_LEVELS as usize + level,
                PostParams {
                    settings: base,
                    texel: [tx, ty, 0.0, 0.0],
                    extra: [-1.0, source_weight, normalization, 0.0],
                    padding: [0.0; 4],
                },
            );
        }
        let composite_slot = (PARAMS_SLOTS - 1) as usize;
        write(
            composite_slot,
            PostParams {
                settings: [
                    base[0],
                    if bloom_on { base[1] } else { 0.0 },
                    base[2],
                    base[3],
                ],
                texel: [fx, fy, 0.0, 0.0],
                extra: [-1.0, 1.0, normalization, 0.0],
                padding: [0.0; 4],
            },
        );
        queue.write_buffer(&self.params_buffer, 0, &blob);

        let offset = |slot: usize| (slot as u64 * PARAMS_STRIDE) as u32;
        let source_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine post source"),
            layout: &self.params_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.params_buffer,
                        offset: 0,
                        size: NonZeroU64::new(size_of::<PostParams>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(mask.unwrap_or(&self.dummy_mask)),
                },
            ],
        });

        let mut pass = |label: &str,
                        pipeline: &wgpu::RenderPipeline,
                        group: &wgpu::BindGroup,
                        slot: usize,
                        bloom: Option<&wgpu::BindGroup>,
                        target: &wgpu::TextureView,
                        load: wgpu::LoadOp<wgpu::Color>,
                        blend_constant: Option<f32>| {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
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
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, group, &[offset(slot)]);
            if let Some(bloom) = bloom {
                render_pass.set_bind_group(1, bloom, &[]);
            }
            if let Some(weight) = blend_constant {
                let w = f64::from(weight);
                render_pass.set_blend_constant(wgpu::Color {
                    r: w,
                    g: w,
                    b: w,
                    a: w,
                });
            }
            render_pass.draw(0..3, 0..1);
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);

        if bloom_on {
            // HDR → 第 0 级（半分辨率），顺带提取亮部。
            pass(
                "kengine bloom extract",
                &self.extract_pipeline,
                &source_group,
                0,
                None,
                &self.targets.bloom_levels[0],
                clear,
                None,
            );
            // 一级级往下。
            for level in 1..levels {
                pass(
                    "kengine bloom down",
                    &self.down_pipeline,
                    &self.down_bind_groups[level - 1],
                    level,
                    None,
                    &self.targets.bloom_levels[level],
                    clear,
                    None,
                );
            }
            // 再一级级往上：目标那一级原有的内容乘它自己的权重（混合常数），
            // 再加上更宽那一级的帐篷滤波。走到第 0 级时就是 Σ 权重 × 各级。
            for level in (0..last).rev() {
                pass(
                    "kengine bloom up",
                    &self.up_pipeline,
                    &self.up_bind_groups[level],
                    BLOOM_MAX_LEVELS as usize + level,
                    None,
                    &self.targets.bloom_levels[level],
                    wgpu::LoadOp::Load,
                    Some(weights[level]),
                );
            }
        }

        // HDR + 辉光 → LDR。辉光关着时合成那边乘 0，采到的是上一次的残留也无所谓。
        pass(
            "kengine post composite",
            &self.composite_pipeline,
            &source_group,
            composite_slot,
            Some(&self.bloom_bind_group),
            target,
            clear,
            None,
        );
    }

    /// FSR1：读 `source`（LDR，渲染分辨率），放大到 `output` 尺寸写进 `target`。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_fsr(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        output: (u32, u32),
        sharpness: f32,
    ) {
        let (width, height) = (output.0.max(1), output.1.max(1));
        if self
            .fsr_intermediate
            .as_ref()
            .is_none_or(|(texture, _)| texture.width() != width || texture.height() != height)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("kengine fsr intermediate"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.fsr_intermediate = Some((texture, view));
        }
        queue.write_buffer(
            &self.fsr_params,
            0,
            bytemuck::cast_slice(&[FsrParams {
                input_size: [
                    self.targets.width.max(1) as f32,
                    self.targets.height.max(1) as f32,
                ],
                output_size: [width as f32, height as f32],
                sharpness: sharpness.max(0.0),
                _pad: [0.0; 3],
            }]),
        );
        let Some((_, intermediate)) = &self.fsr_intermediate else {
            return;
        };
        for (pipeline, input, output) in [
            (&self.fsr_easu, source, intermediate),
            (&self.fsr_rcas, intermediate, target),
        ] {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kengine fsr bind group"),
                layout: &self.fsr_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.fsr_params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(input),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine fsr"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// 这一帧没走 TAAU：下次再走时历史图作废（隔了几帧的历史对不上）。
    pub(crate) fn skip_taau(&mut self) {
        self.taau_valid = false;
    }

    /// TAAU：本帧低分辨率的 `source`（LDR）+ 运动向量，累积进屏幕分辨率的历史图，再（可选）锐化写进 `target`。
    /// `jitter` 是本帧投影的抖动，单位低分辨率像素。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_taau(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        velocity: &wgpu::TextureView,
        target: &wgpu::TextureView,
        output: (u32, u32),
        jitter: [f32; 2],
        sharpness: Option<f32>,
    ) {
        let (width, height) = (output.0.max(1), output.1.max(1));
        for slot in &mut self.taau_history {
            if slot
                .as_ref()
                .is_none_or(|(texture, _)| texture.width() != width || texture.height() != height)
            {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("kengine taau history"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TAAU_HISTORY_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                *slot = Some((texture, view));
                self.taau_valid = false;
            }
        }
        queue.write_buffer(
            &self.taau_params,
            0,
            bytemuck::cast_slice(&[TaauParams {
                input_size: [
                    self.targets.width.max(1) as f32,
                    self.targets.height.max(1) as f32,
                ],
                output_size: [width as f32, height as f32],
                jitter,
                history_valid: if self.taau_valid { 1.0 } else { 0.0 },
                _pad: 0.0,
            }]),
        );
        let write = self.taau_write;
        let (Some((_, written)), Some((_, previous))) =
            (&self.taau_history[write], &self.taau_history[1 - write])
        else {
            return;
        };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine taau bind group"),
            layout: &self.taau_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.taau_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(previous),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(velocity),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine taau"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: written,
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
            pass.set_pipeline(&self.taau_pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        // 历史图 → 屏幕：RCAS 锐化，或者锐化量取得极大当成原样拷贝。
        queue.write_buffer(
            &self.fsr_params,
            0,
            bytemuck::cast_slice(&[FsrParams {
                input_size: [width as f32, height as f32],
                output_size: [width as f32, height as f32],
                sharpness: sharpness.map_or(64.0, |s| s.max(0.0)),
                _pad: [0.0; 3],
            }]),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine taau sharpen"),
            layout: &self.fsr_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.fsr_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(written),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine taau output"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_pipeline(&self.fsr_rcas);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.taau_write = 1 - write;
        self.taau_valid = true;
    }

    /// FXAA：读 `source`（LDR），写 `target`。
    pub(crate) fn run_fxaa(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        queue.write_buffer(
            &self.fxaa_params,
            0,
            bytemuck::cast_slice(&[FxaaParams {
                texel: [
                    1.0 / self.targets.width.max(1) as f32,
                    1.0 / self.targets.height.max(1) as f32,
                ],
                // 两个阈值缺一不可：只有绝对阈值的话，亮部里很轻微的
                // 渐变也会被当成边缘去糊；只有相对阈值的话，暗部的
                // 噪声会被无限放大。这两个数是 FXAA 3.11 的推荐值。
                threshold_min: 0.0312,
                threshold_max: 0.125,
            }]),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine fxaa bind group"),
            layout: &self.fxaa_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.fxaa_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine fxaa"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        render_pass.set_pipeline(&self.fxaa_pipeline);
        render_pass.set_bind_group(0, &group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

/// 辉光各级的权重，和 three.js `UnrealBloomPass` 同一套：
/// 基准 `f = 1.0, 0.8, 0.6, 0.4, 0.2, …`，按半径在 `f` 和 `1.2 − f` 之间插值。
///
/// 半径 0 时窄的级别占大头（光晕贴着亮部）；半径 1 时反过来。
/// 两头的权重之和相同，所以调半径不改变辉光的总能量。
fn bloom_level_weights(radius: f32, levels: usize) -> Vec<f32> {
    let radius = radius.clamp(0.0, 1.0);
    (0..levels)
        .map(|i| {
            let f = (1.0 - 0.2 * i as f32).max(0.1);
            f + (1.2 - f - f) * radius
        })
        .collect()
}

/// 辉光链的级数：半分辨率起步，每级减半，最小的一级不低于 8 像素。
fn bloom_level_count(width: u32, height: u32) -> u32 {
    let shortest = (width.min(height) / 2).max(1);
    let mut levels = 0;
    let mut size = shortest;
    while levels < BLOOM_MAX_LEVELS && size >= 8 {
        levels += 1;
        size /= 2;
    }
    levels.max(1)
}

fn create_targets(device: &wgpu::Device, width: u32, height: u32) -> Targets {
    let width = width.max(1);
    let height = height.max(1);

    let hdr_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine hdr target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            // 超采样最后要把累积结果拷回来。
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let hdr = hdr_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let levels = bloom_level_count(width, height);
    let bloom_width = (width / 2).max(1);
    let bloom_height = (height / 2).max(1);
    let bloom = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine bloom chain"),
        size: wgpu::Extent3d {
            width: bloom_width,
            height: bloom_height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let bloom_levels = (0..levels)
        .map(|level| {
            bloom.create_view(&wgpu::TextureViewDescriptor {
                label: Some("kengine bloom level"),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    let bloom_sizes = (0..levels)
        .map(|level| {
            (
                (bloom_width >> level).max(1),
                (bloom_height >> level).max(1),
            )
        })
        .collect();

    Targets {
        hdr,
        hdr_texture,
        bloom_levels,
        bloom_sizes,
        width,
        height,
    }
}

/// 链上固定的那些绑定组：降采样读第 i 级、升采样读第 i+1 级、合成读第 0 级。
#[allow(clippy::type_complexity)]
fn create_chain_bind_groups(
    device: &wgpu::Device,
    params_layout: &wgpu::BindGroupLayout,
    bloom_layout: &wgpu::BindGroupLayout,
    params_buffer: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    dummy_mask: &wgpu::TextureView,
    targets: &Targets,
) -> (Vec<wgpu::BindGroup>, Vec<wgpu::BindGroup>, wgpu::BindGroup) {
    let make = |source: &wgpu::TextureView| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine bloom level bind group"),
            layout: params_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: params_buffer,
                        offset: 0,
                        size: NonZeroU64::new(size_of::<PostParams>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(dummy_mask),
                },
            ],
        })
    };
    let levels = targets.bloom_levels.len();
    // 降采样 i（1..levels）读第 i-1 级；升采样 i（0..levels-1）读第 i+1 级。
    let down = (1..levels)
        .map(|i| make(&targets.bloom_levels[i - 1]))
        .collect();
    let up = (0..levels.saturating_sub(1))
        .map(|i| make(&targets.bloom_levels[i + 1]))
        .collect();
    let bloom = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine post bloom bind group"),
        layout: bloom_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&targets.bloom_levels[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    (down, up, bloom)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn params_layout_is_aligned() {
        assert_eq!(size_of::<PostParams>(), 64);
        assert_eq!(size_of::<PostParams>() % 16, 0);
        assert!(size_of::<PostParams>() as u64 <= PARAMS_STRIDE);
    }

    #[test]
    fn hdr_format_is_floating_point() {
        // 必须能存下大于 1 的值，否则高光在色调映射前就被切掉，Bloom 也就无从提取。
        assert_eq!(HDR_FORMAT, wgpu::TextureFormat::Rgba16Float);
    }

    #[test]
    fn default_settings_keep_bloom_subtle() {
        let settings = PostSettings::default();

        // 阈值为 1 表示只有超过「白」的部分才发光。
        assert_eq!(settings.bloom_threshold, 1.0);
        assert!(settings.bloom_intensity > 0.0 && settings.bloom_intensity < 0.5);
        assert_eq!(settings.tone_mapping, ToneMapping::Aces);
        assert_eq!(
            settings.exposure, 1.0,
            "默认曝光必须是 1——它是个倍数，不是偏移"
        );
        assert_eq!(settings.ssaa, 1, "超采样默认关着——它是 N 倍的场景开销");
        assert_eq!(settings.bloom_mask, None);
    }

    #[test]
    fn the_bloom_chain_stops_before_it_becomes_a_single_colour() {
        assert_eq!(bloom_level_count(1920, 1080), 6);
        // 小窗口：半分辨率 60 → 60, 30, 15 三级（下一级 7 < 8）。
        assert_eq!(bloom_level_count(160, 120), 3);
        // 再小也至少一级，不然合成那边没东西可采。
        assert_eq!(bloom_level_count(4, 4), 1);
    }

    #[test]
    fn the_radius_moves_weight_without_changing_the_total() {
        let tight = bloom_level_weights(0.0, 5);
        let wide = bloom_level_weights(1.0, 5);
        assert!((tight.iter().sum::<f32>() - 3.0).abs() < 1e-5);
        assert!((wide.iter().sum::<f32>() - 3.0).abs() < 1e-5);
        assert!(tight[0] > tight[4] && wide[0] < wide[4]);
    }

    #[test]
    fn the_parameter_table_fits_every_pass() {
        // 提取 + 降采样（levels-1）+ 升采样（levels-1）+ 合成，
        // 各自的槽位不能撞：升采样从 BLOOM_MAX_LEVELS 开始编号。
        let levels = BLOOM_MAX_LEVELS as usize;
        let last_down = levels - 1;
        let first_up = BLOOM_MAX_LEVELS as usize;
        let last_up = first_up + levels - 2;
        let composite = (PARAMS_SLOTS - 1) as usize;
        assert!(last_down < first_up);
        assert!(last_up < composite);
    }

    #[test]
    fn the_post_shader_passes_validation() {
        kshader::Shader::from_wgsl(include_str!("post.wgsl")).expect("后处理着色器应当通过校验");
    }
}
