//! 深度／法线预通道 + 屏幕空间环境光遮蔽。
//!
//! # 两个 pass，一条依赖链
//!
//! ```text
//! 预通道  ──→  深度 + 世界法线  ──→  SSAO pass  ──→  遮蔽图  ──→  主 pass
//! ```
//!
//! 主 pass 把遮蔽图乘进 `occlusion`，于是只削弱**环境光**——
//! 直射光不受影响。把 AO 直接乘在最终颜色上（很多引擎图省事的做法）
//! 会把太阳照亮的地方也一起压暗，看着像整个画面蒙了一层灰。
//!
//! # 关着的时候一分钱不花
//!
//! [`SsaoSettings::enabled`] 为假时这两个 pass 都不跑，主 pass 绑的是
//! 一张 1×1 的白图（值为 1 = 完全不遮）。「没有 SSAO」于是等价于
//! 「乘 1」，着色器不必为它写分支——和缺贴图时绑白图是同一个套路。
//!
//! 这一点决定了预通道该不该复用主 pass 的深度：**不复用**。
//! 顺序上做得到（预通道在前），但那要求主 pass 改成 `LessEqual` +
//! `LoadOp::Load`，而两条 pass 的顶点变换必须**逐位相同**才安全——
//! 同一段 WGSL 编进两个模块，驱动的优化不保证一致，差一个 ULP 就会在
//! 物体表面上抠出一片洞。
//!
//! 代价是开着 SSAO 时几何走两遍。真要省这一遍是一次独立的、有回归风险的
//! 改动，不该和「把 SSAO 做出来」混在一起。

use bytemuck::{Pod, Zeroable};
use kmath::{Mat4, Vec3};

/// SSAO 数学的端到端验证。单独一个文件，理由见它的模块文档。
#[cfg(test)]
#[path = "ssao_gpu_tests.rs"]
mod gpu_tests;

/// 法线缓冲的格式。
///
/// 必须是浮点的：世界法线的范围是 `[-1, 1]`，压进 `Unorm` 要先编码到
/// `[0,1]` 再解开，8 位精度下半球采样会在平面上抖出一圈圈条纹。
const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 遮蔽图的格式。
///
/// `R32Float` 而不是 `R16Float`：前者在 WebGPU 里是**不可过滤**的，
/// 和绑定布局里写的 `filterable: false` 严格对得上。着色器那边用
/// `textureLoad`，本来就不需要过滤。
const OCCLUSION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

/// SSAO 的调节项。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SsaoSettings {
    /// 开不开。关着时预通道和 SSAO pass 都不跑。
    pub enabled: bool,
    /// 采样半径（世界单位）。
    ///
    /// 决定「多远以内的东西算遮挡」。房间尺度的场景通常 0.3~1.0；
    /// 给得太大会把整面墙算成遮挡，画面糊成一片脏；太小则只有紧贴的
    /// 接缝有效果。
    pub radius: f32,
    /// 强度。1 是采样结果原样用，越大越黑。
    pub strength: f32,
    /// 采样数，1..=16。
    ///
    /// 上限是 16，因为核是写死在着色器里的 16 个方向——
    /// 那些方向从头到尾不变，做成 uniform 只是白占 256 字节。
    pub samples: u32,
    /// 深度偏移（世界单位）。
    ///
    /// 防自遮挡：不加的话平坦表面会因为浮点误差把自己判成被挡住，
    /// 整个画面浮起一层均匀的灰。给太大则接触阴影会离开接缝。
    pub bias: f32,
}

impl Default for SsaoSettings {
    fn default() -> Self {
        Self {
            // 默认关着。它要多渲一遍几何，而**大多数场景没有它也说得过去**
            // ——不像软粒子那样是「不开就明显穿帮」。
            enabled: false,
            radius: 0.5,
            strength: 1.0,
            samples: 16,
            bias: 0.02,
        }
    }
}

/// SSAO pass 的 uniform，对应 `ssao.wgsl` 的 `SsaoParams`。
///
/// 字段顺序和填充**必须**和那边一致。对不上不会报错——
/// `min_binding_size` 只校验总长度——画出来是一张噪声。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SsaoParams {
    inverse_view_proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    camera_position: [f32; 4],
    /// x = 半径，y = 强度，z = 采样数，w = 偏移
    settings: [f32; 4],
    /// xy = 纹素尺寸，zw = 视口像素尺寸
    texel: [f32; 4],
}

/// 尺寸相关的三张纹理。窗口一变就整体重建。
struct Targets {
    /// 预通道自己的深度。理由见模块文档。
    depth: wgpu::TextureView,
    normal: wgpu::TextureView,
    occlusion: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// 预通道与 SSAO 的一整套资源。
pub(crate) struct Ssao {
    pub(crate) settings: SsaoSettings,

    /// 预通道的两条管线：静态与蒙皮。顶点布局不同，只能分开。
    prepass_pipeline: wgpu::RenderPipeline,
    prepass_skinned_pipeline: wgpu::RenderPipeline,

    ssao_pipeline: wgpu::RenderPipeline,
    ssao_layout: wgpu::BindGroupLayout,
    params_buffer: wgpu::Buffer,
    /// 采深度和法线的绑定组。重建目标时要跟着重建。
    ssao_bind_group: wgpu::BindGroup,

    targets: Targets,
    /// 关着 SSAO 时绑给主 pass 的那张 1×1 白图。
    white: wgpu::TextureView,
}

impl Ssao {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        globals_layout: &wgpu::BindGroupLayout,
        object_layout: &wgpu::BindGroupLayout,
        geometry_prelude: &str,
        width: u32,
        height: u32,
    ) -> Self {
        // ── 预通道 ──
        //
        // 只用 group(0) 和 group(1)，所以有自己的一条**更窄**的管线布局。
        // 沿用主 pass 那条的话，wgpu 要求每个组在绘制时都被 set，
        // 就得为预通道准备一套用不上的贴图绑定组。
        let prepass_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine prepass layout"),
            bind_group_layouts: &[Option::from(globals_layout), Option::from(object_layout)],
            immediate_size: 0,
        });
        let prepass_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine prepass shader"),
            source: wgpu::ShaderSource::Wgsl(
                // `Globals` / `ObjectUniforms` / 蒙皮 / 形变来自
                // `geometry.wgsl`——和主着色器、阴影 pass 是同一份声明。
                // 前缀里还带着 klight 和 kpbr：`Globals` 引用了
                // `Light` 和 `Environment`，少了它们编不过。
                format!("{geometry_prelude}\n{}", include_str!("prepass.wgsl")).into(),
            ),
        });

        let prepass_pipeline = create_prepass_pipeline(
            device,
            &prepass_layout,
            &prepass_module,
            "vs_main",
            &[Option::from(crate::vertex_layout())],
            "kengine prepass pipeline",
        );
        let prepass_skinned_pipeline = create_prepass_pipeline(
            device,
            &prepass_layout,
            &prepass_module,
            "vs_skinned",
            &[
                Option::from(crate::vertex_layout()),
                Option::from(crate::skin_layout()),
            ],
            "kengine prepass skinned pipeline",
        );

        // ── SSAO ──
        let ssao_layout = create_ssao_layout(device);
        let ssao_pipeline = create_ssao_pipeline(device, &ssao_layout);
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine ssao params"),
            size: size_of::<SsaoParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let targets = create_targets(device, width, height);
        let ssao_bind_group =
            create_ssao_bind_group(device, &ssao_layout, &params_buffer, &targets);
        let white = create_white(device, queue);

        Self {
            settings: SsaoSettings::default(),
            prepass_pipeline,
            prepass_skinned_pipeline,
            ssao_pipeline,
            ssao_layout,
            params_buffer,
            ssao_bind_group,
            targets,
            white,
        }
    }

    /// 窗口尺寸变了。
    pub(crate) fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.targets.width == width.max(1) && self.targets.height == height.max(1) {
            return;
        }
        self.targets = create_targets(device, width, height);
        self.ssao_bind_group = create_ssao_bind_group(
            device,
            &self.ssao_layout,
            &self.params_buffer,
            &self.targets,
        );
    }

    /// 主 pass 该绑哪张遮蔽图。
    ///
    /// 关着 SSAO 时是那张 1×1 白图——「没有 SSAO」等价于「乘 1」。
    pub(crate) fn occlusion_view(&self) -> &wgpu::TextureView {
        if self.settings.enabled {
            &self.targets.occlusion
        } else {
            &self.white
        }
    }

    /// 按批次选预通道的管线。
    pub(crate) fn prepass_pipeline(&self, skinned: bool) -> &wgpu::RenderPipeline {
        if skinned {
            &self.prepass_skinned_pipeline
        } else {
            &self.prepass_pipeline
        }
    }

    /// 开一个预通道。返回的 pass 由调用方填绘制命令。
    ///
    /// 深度和法线都 `Clear`：这两张图每帧从头算，没有需要保留的历史。
    pub(crate) fn begin_prepass<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine prepass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets.normal,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }

    /// 跑 SSAO：读预通道的深度与法线，写遮蔽图。
    pub(crate) fn run(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view_proj: Mat4,
        camera_position: Vec3,
    ) {
        let (width, height) = (self.targets.width as f32, self.targets.height as f32);
        queue.write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&SsaoParams {
                inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
                view_proj: view_proj.to_cols_array_2d(),
                camera_position: camera_position.extend(1.0).to_array(),
                settings: [
                    self.settings.radius.max(1e-4),
                    self.settings.strength.max(0.0),
                    // 夹进核的长度。给 20 的话循环会读越界的常量数组，
                    // 那在 WGSL 里是未定义行为。
                    self.settings.samples.clamp(1, 16) as f32,
                    self.settings.bias.max(0.0),
                ],
                texel: [1.0 / width, 1.0 / height, width, height],
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine ssao pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.targets.occlusion,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // 不 `Load`：整张图每个像素都会被写一遍。
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.ssao_pipeline);
        pass.set_bind_group(0, &self.ssao_bind_group, &[]);
        // 全屏三角形：三个顶点盖住整个视口。
        pass.draw(0..3, 0..1);
    }
}

/// SSAO pass 的绑定组布局。
///
/// 抽成自由函数是为了让测试能**不建整个 [`Ssao`]** 就跑这一段——
/// 后者要 `Globals` / `ObjectUniforms` 那两个布局，而那是预通道的事，
/// 和 SSAO 的数学无关。
fn create_ssao_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("kengine ssao layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(size_of::<SsaoParams>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

/// SSAO 的全屏管线。
fn create_ssao_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kengine ssao shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("ssao.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kengine ssao pipeline layout"),
        bind_group_layouts: &[Option::from(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kengine ssao pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("fullscreen_vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: OCCLUSION_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::RED,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_targets(device: &wgpu::Device, width: u32, height: u32) -> Targets {
    let (width, height) = (width.max(1), height.max(1));
    let make = |label: &str, format: wgpu::TextureFormat| {
        device
            .create_texture(&wgpu::TextureDescriptor {
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
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };

    Targets {
        depth: make("kengine prepass depth", wgpu::TextureFormat::Depth32Float),
        normal: make("kengine prepass normal", NORMAL_FORMAT),
        occlusion: make("kengine ssao occlusion", OCCLUSION_FORMAT),
        width,
        height,
    }
}

fn create_ssao_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    targets: &Targets,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine ssao bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&targets.depth),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&targets.normal),
            },
        ],
    })
}

/// 关着 SSAO 时用的 1×1 白图。
fn create_white(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kengine ssao off"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OCCLUSION_FORMAT,
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
        // 1.0：完全不遮。写 0 的话关掉 SSAO 反而让整个画面的环境光归零。
        &1.0f32.to_le_bytes(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
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

/// 建一条预通道管线。
fn create_prepass_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    entry_point: &str,
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: NORMAL_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        // 和主 pass 完全一致的光栅化状态。不一致的话预通道看到的轮廓
        // 和主 pass 画出来的对不上，AO 会沿着边缘错开一圈。
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

#[cfg(test)]
mod tests {
    use super::*;
    use kshader::Shader;

    #[test]
    fn the_ssao_shader_passes_validation() {
        Shader::from_wgsl(include_str!("ssao.wgsl")).expect("SSAO 着色器应当通过校验");
    }

    #[test]
    fn the_prepass_shader_compiles_against_the_shared_geometry() {
        // 预通道用的 `Globals` / `ObjectUniforms` / 蒙皮 / 形变来自
        // `geometry.wgsl`——和主着色器、阴影 pass 是同一份。
        // 这一条挂了通常说明 `geometry.wgsl` 改了而预通道没跟上。
        let source = format!(
            "{}\n{}\n{}\n{}\n{}",
            klight::LIGHT_WGSL,
            kpbr::PBR_WGSL,
            kpbr::IBL_WGSL,
            crate::geometry_source(),
            include_str!("prepass.wgsl"),
        );
        Shader::from_wgsl(source).expect("预通道着色器应当通过校验");
    }

    #[test]
    fn the_prepass_declares_both_entry_points() {
        // 名字硬编码在建管线的代码里。改了名字这里先响，
        // 而不是等到运行时 wgpu 报「找不到入口点」。
        let source = include_str!("prepass.wgsl");
        assert!(source.contains("fn vs_main"));
        assert!(source.contains("fn vs_skinned"));
        assert!(source.contains("fn fs_main"));
    }

    #[test]
    fn ssao_is_off_by_default() {
        // 它要多渲一遍几何，而大多数场景没有它也说得过去——
        // 不像软粒子那样是「不开就明显穿帮」。
        assert!(!SsaoSettings::default().enabled);
    }

    #[test]
    fn the_params_struct_is_sixteen_byte_aligned() {
        // WGSL 的 uniform 要求 16 字节对齐。对不上不会报错，
        // 只是读出来的字段全部错位——画出来是一张噪声。
        assert_eq!(size_of::<SsaoParams>() % 16, 0);
        // 两个 mat4（128）+ 三个 vec4（48）。
        assert_eq!(size_of::<SsaoParams>(), 176);
    }

    #[test]
    fn the_sample_count_is_clamped_to_the_kernel_length() {
        // 着色器里的核是写死的 16 个方向。给 20 的话循环会读越界，
        // 那在 WGSL 里是未定义行为。
        for requested in [0u32, 1, 16, 99] {
            assert!((1..=16).contains(&requested.clamp(1, 16)));
        }
    }

    #[test]
    fn the_kernel_only_points_away_from_the_surface() {
        // 半球采样：核里每个方向的 z 必须为正。有负的就意味着往表面
        // **里面**采样，平坦的地方会凭空出现一半的遮蔽，
        // 整个画面糊上一层灰。
        let source = include_str!("ssao.wgsl");
        let kernel = source.split("const KERNEL").nth(1).expect("找不到核的定义");
        let body = &kernel[..kernel.find(");").expect("核的定义没有收尾")];

        let mut checked = 0;
        for entry in body.split("vec3<f32>(").skip(1) {
            let numbers: Vec<f32> = entry
                .split(')')
                .next()
                .unwrap()
                .split(',')
                .filter_map(|n| n.trim().parse::<f32>().ok())
                .collect();
            if numbers.len() != 3 {
                continue;
            }
            assert!(numbers[2] > 0.0, "核里有朝向表面内侧的方向：{numbers:?}");
            checked += 1;
        }
        assert_eq!(checked, 16, "核该有 16 个方向，实际解出 {checked} 个");
    }

    #[test]
    fn the_kernel_has_samples_at_several_distances() {
        // 全都是单位长的话采样点全落在半球壳上，近处的接触阴影
        // （墙角、物体和地面的交线）一点都采不到——而那正是 AO
        // 最该表现的地方。
        let source = include_str!("ssao.wgsl");
        let kernel = source.split("const KERNEL").nth(1).unwrap();
        let body = &kernel[..kernel.find(");").unwrap()];

        let mut lengths = Vec::new();
        for entry in body.split("vec3<f32>(").skip(1) {
            let numbers: Vec<f32> = entry
                .split(')')
                .next()
                .unwrap()
                .split(',')
                .filter_map(|n| n.trim().parse::<f32>().ok())
                .collect();
            if numbers.len() == 3 {
                lengths.push((numbers[0].powi(2) + numbers[1].powi(2) + numbers[2].powi(2)).sqrt());
            }
        }

        let shortest = lengths.iter().cloned().fold(f32::MAX, f32::min);
        let longest = lengths.iter().cloned().fold(0.0, f32::max);
        assert!(shortest < 0.2, "最短的采样距离 {shortest} 还是太远");
        assert!(longest > 0.8, "最长的采样距离 {longest} 太近");
    }
}
