//! 覆盖层相机：第二台相机单独画一遍，叠在主画面上（第一人称手里的枪、HUD 里的 3D 模型）。
//!
//! 走离屏视图那一套：覆盖层相机按自己的视角、FOV、渲染层把场景画进一张 HDR 纹理，
//! 清成**全透明**、不画天空；屏幕帧画完不透明和半透明之后，按 alpha 把它盖到主画面上，
//! 再一起过后处理。
//!
//! 为什么在后处理**之前**叠：色调映射、曝光、泛光对两部分要一致，不然枪看起来像贴上去的。
//! 代价是依赖深度的效果（雾、景深、SSAO）对覆盖层用的是主画面的深度——枪会被远处的雾染上一点。
//! 要完全不受影响的话把这些效果的遮罩位让开覆盖层的物体。

/// 覆盖层的目标纹理和合成管线。
pub(crate) struct Overlay {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// 覆盖层画好的那张（尺寸跟着屏幕）。
    target: Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)>,
    /// 这一帧画过覆盖层了，屏幕帧要叠上去。叠完清掉。
    pub(crate) ready: bool,
}

const COMPOSITE_WGSL: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> Out {
    let ndc = vec2<f32>(f32((index << 1u) & 2u) * 2.0 - 1.0, f32(index & 2u) * 2.0 - 1.0);
    var out: Out;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(source, source_sampler, in.uv, 0.0);
    // 覆盖层清成 (0,0,0,0)，不透明物体的 alpha 是 1：颜色本来就是预乘过的。
    return vec4<f32>(c.rgb, clamp(c.a, 0.0, 1.0));
}
"#;

impl Overlay {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine overlay composite"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine overlay layout"),
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine overlay pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let premultiplied = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine overlay composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: premultiplied,
                        alpha: premultiplied,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kengine overlay sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            target: None,
            ready: false,
        }
    }

    /// 屏幕尺寸变了：丢掉旧纹理，下次用到时按新尺寸建。
    pub(crate) fn resize(&mut self) {
        self.target = None;
        self.ready = false;
    }

    /// 覆盖层的目标纹理，没有就按 `extent` 建一张。
    pub(crate) fn target(
        &mut self,
        device: &wgpu::Device,
        extent: wgpu::Extent3d,
    ) -> &wgpu::Texture {
        if self
            .target
            .as_ref()
            .is_none_or(|(texture, _, _)| texture.size() != extent)
        {
            let (texture, view) =
                crate::create_view_target(device, extent, "kengine overlay target");
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kengine overlay bind group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.target = Some((texture, view, bind_group));
        }
        &self.target.as_ref().expect("刚建好").0
    }

    /// 把覆盖层盖到 `destination`（主画面的 HDR 目标）上。这一帧没画覆盖层时什么都不做。
    pub(crate) fn composite(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::TextureView,
    ) {
        if !std::mem::take(&mut self.ready) {
            return;
        }
        let Some((_, _, bind_group)) = &self.target else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine overlay composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
