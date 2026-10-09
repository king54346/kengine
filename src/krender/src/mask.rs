//! 后处理遮罩：把 [`Node::post_mask`](kscene::Node::post_mask) 非零的物体
//! 画进一张全屏的 Rgba8 图，给描边、选择性辉光、遮罩合成用。
//!
//! 数值约定和手动深度测试的理由见 `mask.wgsl`。
//!
//! # 什么时候跑
//!
//! 只在后处理**要**的时候（某个效果声明了要遮罩，或者辉光指定了遮罩通道），
//! 而且只画带遮罩位的那几个物体——通常是一两个被选中的东西，
//! 所以这一趟几乎是免费的。

/// 遮罩图的格式。四个通道各一位，值只有 0 / 0.5 / 1 三种，8 位绰绰有余。
pub(crate) const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(crate) struct MaskPass {
    pipeline: wgpu::RenderPipeline,
    skinned_pipeline: wgpu::RenderPipeline,
    depth_layout: wgpu::BindGroupLayout,
    depth_bind_group: wgpu::BindGroup,
    target: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl MaskPass {
    pub(crate) fn new(
        device: &wgpu::Device,
        globals_layout: &wgpu::BindGroupLayout,
        object_layout: &wgpu::BindGroupLayout,
        geometry_prelude: &str,
        depth_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> Self {
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine mask depth layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine mask pipeline layout"),
            bind_group_layouts: &[
                Option::from(globals_layout),
                Option::from(object_layout),
                Option::from(&depth_layout),
            ],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine mask shader"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{geometry_prelude}\n{}", include_str!("mask.wgsl")).into(),
            ),
        });
        let make = |entry: &str, buffers: &[Option<wgpu::VertexBufferLayout<'_>>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("kengine mask pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: MASK_FORMAT,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Max,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Max,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                // 不剔除：被挡住的那部分要画的恰恰可能是背面朝外的那一侧。
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make("vs_main", &[Option::from(crate::vertex_layout())]);
        let skinned_pipeline = make(
            "vs_skinned",
            &[
                Option::from(crate::vertex_layout()),
                Option::from(crate::skin_layout()),
            ],
        );
        let depth_bind_group = create_depth_bind_group(device, &depth_layout, depth_view);
        Self {
            pipeline,
            skinned_pipeline,
            depth_layout,
            depth_bind_group,
            target: create_target(device, width, height),
            width,
            height,
        }
    }

    /// 窗口尺寸变了，或者主 pass 的深度缓冲换了。
    pub(crate) fn resize(
        &mut self,
        device: &wgpu::Device,
        depth_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        self.depth_bind_group = create_depth_bind_group(device, &self.depth_layout, depth_view);
        if self.width != width || self.height != height {
            self.target = create_target(device, width, height);
            self.width = width;
            self.height = height;
        }
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.target
    }

    pub(crate) fn pipeline(&self, skinned: bool) -> &wgpu::RenderPipeline {
        if skinned {
            &self.skinned_pipeline
        } else {
            &self.pipeline
        }
    }

    pub(crate) fn depth_bind_group(&self) -> &wgpu::BindGroup {
        &self.depth_bind_group
    }

    /// 开一个遮罩 pass。遮罩图每帧清零。
    pub(crate) fn begin<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kengine mask pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }
}

fn create_depth_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    depth_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine mask depth bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(depth_view),
        }],
    })
}

fn create_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine post mask"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MASK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

#[cfg(test)]
mod tests {
    use kshader::Shader;

    #[test]
    fn the_mask_shader_compiles_against_the_shared_geometry() {
        let source = format!(
            "{}\n{}\n{}\n{}\n{}",
            klight::LIGHT_WGSL,
            kpbr::PBR_WGSL,
            kpbr::IBL_WGSL,
            crate::geometry_source(),
            include_str!("mask.wgsl"),
        );
        Shader::from_wgsl(source).expect("遮罩着色器应当通过校验");
    }
}
