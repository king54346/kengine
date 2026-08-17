//! 后处理链：HDR 目标 → Bloom → 色调映射 → 屏幕。

use crate::tonemap::ToneMapping;
use bytemuck::{Pod, Zeroable};
use std::num::NonZeroU64;

/// 主 pass 的渲染目标格式。
///
/// 必须是浮点格式：PBR 输出的高光远超过 1，8 位归一化格式会在色调映射之前就把它们切掉，
/// Bloom 也就无从提取。
pub(crate) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bloom 相对主目标的降采样倍数。半分辨率在质量与开销之间比较平衡。
const BLOOM_DOWNSCALE: u32 = 2;

/// 后处理参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostSettings {
    /// 亮部提取阈值。低于此亮度的像素不参与 Bloom。
    pub bloom_threshold: f32,
    /// Bloom 混合强度。为 0 时相当于关闭 Bloom。
    pub bloom_intensity: f32,
    /// 色调映射算子。
    pub tone_mapping: ToneMapping,
}

impl Default for PostSettings {
    fn default() -> Self {
        Self {
            bloom_threshold: 1.0,
            bloom_intensity: 0.06,
            tone_mapping: ToneMapping::default(),
        }
    }
}

/// 后处理 pass 的 uniform，对应 `post.wgsl` 的 `PostParams`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostParams {
    /// x = 阈值，y = 强度，z = 算子编号，w 保留
    settings: [f32; 4],
    /// xy = 纹素尺寸，zw = 模糊方向
    texel: [f32; 4],
}

/// 一组尺寸相关的离屏纹理。窗口尺寸变化时整体重建。
struct Targets {
    hdr: wgpu::TextureView,
    /// 两张半分辨率缓冲，模糊时来回乒乓。
    bloom: [wgpu::TextureView; 2],
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
    blur_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,

    /// 每个 pass 一份参数缓冲：提取、横向模糊、纵向模糊、合成。
    params_buffers: [wgpu::Buffer; 4],
    /// 与上面一一对应的绑定组，在重建目标时一并刷新。
    bind_groups: Vec<wgpu::BindGroup>,
    /// 合成 pass 用到的 Bloom 采样绑定组。
    bloom_bind_group: Option<wgpu::BindGroup>,
}

impl PostProcess {
    pub(crate) fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine post params layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(size_of::<PostParams>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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

        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine post bloom layout"),
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
            ],
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
                             layout: &wgpu::PipelineLayout| {
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
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                // 全屏 pass 不需要深度。
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let extract_pipeline = make_pipeline(
            "kengine bloom extract",
            "bloom_extract_fs",
            HDR_FORMAT,
            &simple_layout,
        );
        let blur_pipeline = make_pipeline(
            "kengine bloom blur",
            "bloom_blur_fs",
            HDR_FORMAT,
            &simple_layout,
        );
        let composite_pipeline = make_pipeline(
            "kengine post composite",
            "composite_fs",
            surface_format,
            &composite_layout,
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

        // 四个 pass 各一份参数：提取、横向模糊、纵向模糊、合成。
        let params_buffers = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kengine post params"),
                size: size_of::<PostParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        let targets = create_targets(device, width, height);

        let mut post = Self {
            settings: PostSettings::default(),
            targets,
            params_layout,
            bloom_layout,
            sampler,
            extract_pipeline,
            blur_pipeline,
            composite_pipeline,
            params_buffers,
            bind_groups: Vec::new(),
            bloom_bind_group: None,
        };
        post.rebuild_bind_groups(device);
        post
    }

    /// 主 pass 应当渲染到的 HDR 目标。
    pub(crate) fn hdr_target(&self) -> &wgpu::TextureView {
        &self.targets.hdr
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
        if width == self.targets.width && height == self.targets.height {
            return;
        }
        self.targets = create_targets(device, width, height);
        self.rebuild_bind_groups(device);
    }

    fn rebuild_bind_groups(&mut self, device: &wgpu::Device) {
        let make = |buffer: &wgpu::Buffer, texture: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kengine post bind group"),
                layout: &self.params_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(texture),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };

        self.bind_groups = vec![
            // 提取：读 HDR，写 bloom[0]
            make(&self.params_buffers[0], &self.targets.hdr),
            // 横向模糊：读 bloom[0]，写 bloom[1]
            make(&self.params_buffers[1], &self.targets.bloom[0]),
            // 纵向模糊：读 bloom[1]，写 bloom[0]
            make(&self.params_buffers[2], &self.targets.bloom[1]),
            // 合成：读 HDR（Bloom 走 group 1）
            make(&self.params_buffers[3], &self.targets.hdr),
        ];

        let make_bloom = |texture: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kengine post bloom bind group"),
                layout: &self.bloom_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(texture),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            })
        };

        // 纵向模糊的结果落在 bloom[0]，合成时采样它。
        self.bloom_bind_group = Some(make_bloom(&self.targets.bloom[0]));
    }

    /// 执行整条后处理链，把结果写到 `output`。
    pub(crate) fn run(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
    ) {
        let bloom_width = (self.targets.width / BLOOM_DOWNSCALE).max(1) as f32;
        let bloom_height = (self.targets.height / BLOOM_DOWNSCALE).max(1) as f32;
        let operator = self.settings.tone_mapping.index() as f32;

        let base = [
            self.settings.bloom_threshold,
            self.settings.bloom_intensity,
            operator,
            0.0,
        ];

        // 四个 pass 的参数：提取、横向模糊、纵向模糊、合成。
        let params = [
            PostParams {
                settings: base,
                texel: [0.0, 0.0, 0.0, 0.0],
            },
            PostParams {
                settings: base,
                texel: [1.0 / bloom_width, 1.0 / bloom_height, 1.0, 0.0],
            },
            PostParams {
                settings: base,
                texel: [1.0 / bloom_width, 1.0 / bloom_height, 0.0, 1.0],
            },
            PostParams {
                settings: base,
                texel: [0.0, 0.0, 0.0, 0.0],
            },
        ];
        for (buffer, value) in self.params_buffers.iter().zip(params) {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&[value]));
        }

        let Some(bloom_bind_group) = &self.bloom_bind_group else {
            return;
        };

        let mut pass = |label: &str,
                        pipeline: &wgpu::RenderPipeline,
                        bind_group: &wgpu::BindGroup,
                        bloom: Option<&wgpu::BindGroup>,
                        target: &wgpu::TextureView| {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
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
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, bind_group, &[]);
            if let Some(bloom) = bloom {
                render_pass.set_bind_group(1, bloom, &[]);
            }
            render_pass.draw(0..3, 0..1);
        };

        // HDR → 亮部（bloom[0]）
        pass(
            "kengine bloom extract",
            &self.extract_pipeline,
            &self.bind_groups[0],
            None,
            &self.targets.bloom[0],
        );
        // 横向模糊：bloom[0] → bloom[1]
        pass(
            "kengine bloom blur h",
            &self.blur_pipeline,
            &self.bind_groups[1],
            None,
            &self.targets.bloom[1],
        );
        // 纵向模糊：bloom[1] → bloom[0]
        pass(
            "kengine bloom blur v",
            &self.blur_pipeline,
            &self.bind_groups[2],
            None,
            &self.targets.bloom[0],
        );
        // HDR + bloom[0] → 屏幕
        pass(
            "kengine post composite",
            &self.composite_pipeline,
            &self.bind_groups[3],
            Some(bloom_bind_group),
            output,
        );
    }
}

fn create_targets(device: &wgpu::Device, width: u32, height: u32) -> Targets {
    let width = width.max(1);
    let height = height.max(1);

    let make = |label: &str, w: u32, h: u32| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };

    let bloom_width = width / BLOOM_DOWNSCALE;
    let bloom_height = height / BLOOM_DOWNSCALE;

    Targets {
        hdr: make("kengine hdr target", width, height),
        bloom: [
            make("kengine bloom a", bloom_width, bloom_height),
            make("kengine bloom b", bloom_width, bloom_height),
        ],
        width,
        height,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn params_layout_is_aligned() {
        assert_eq!(size_of::<PostParams>(), 32);
        assert_eq!(size_of::<PostParams>() % 16, 0);
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
    }
}
