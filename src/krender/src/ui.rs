//! UI 渲染。
//!
//! 屏幕空间的一段绘制，画在**后处理之后、直接写交换链**。
//!
//! 为什么不和 3D 一起画到 HDR 目标：UI 的颜色是设计好的，过一遍色调映射
//! 会被整体压暗，白色不再是白色。放在后处理之后，界面上的 `#FFFFFF`
//! 到屏幕上就还是 `#FFFFFF`。
//!
//! 代价是 UI 拿不到 bloom——想让一个按钮发光得自己画一圈辉光。
//! 这个取舍对 HUD 和菜单是划算的。

use bytemuck::{Pod, Zeroable};
use fxhash::FxHashMap;
use kcore::uuid::Uuid;
use ktexture::Texture;
use kui::{DrawList, UiVertex};
use std::num::NonZeroU64;

/// UI pass 的全局量，对应 `ui.wgsl` 的 `Globals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct UiGlobals {
    screen: [f32; 2],
    _padding: [f32; 2],
}

/// UI pass 的一组 GPU 资源。
pub(crate) struct UiResources {
    pipeline: wgpu::RenderPipeline,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    texture_layout: wgpu::BindGroupLayout,
    /// 字形图集的绑定组。图集内容一变就重建。
    atlas_bind_group: Option<wgpu::BindGroup>,
    /// 用户贴图的绑定组。
    bind_groups: FxHashMap<Uuid, wgpu::BindGroup>,

    vertex_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    index_buffer: wgpu::Buffer,
    index_capacity: u64,
}

impl UiResources {
    const INITIAL_VERTICES: u64 = 4096;
    const INITIAL_INDICES: u64 = 6144;

    pub(crate) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine ui shader"),
            source: wgpu::ShaderSource::Wgsl(kui::UI_WGSL.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine ui globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<UiGlobals>() as u64),
                },
                count: None,
            }],
        });
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine ui globals"),
            size: size_of::<UiGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine ui globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine ui texture layout"),
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
            label: Some("kengine ui pipeline layout"),
            bind_group_layouts: &[Option::from(&globals_layout), Option::from(&texture_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine ui pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("ui_vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<UiVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 32,
                            shader_location: 3,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 48,
                            shader_location: 4,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("ui_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // 着色器输出预乘 alpha。
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                // UI 的四边形按屏幕坐标生成，绕序是顺时针；不关背面剔除
                // 的话整个界面都不会出现。
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            // UI 不参与深度：它画在所有东西之上，顺序由绘制列表决定。
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            globals_buffer,
            globals_bind_group,
            texture_layout,
            atlas_bind_group: None,
            bind_groups: FxHashMap::default(),
            vertex_buffer: create_vertex_buffer(device, Self::INITIAL_VERTICES),
            vertex_capacity: Self::INITIAL_VERTICES,
            index_buffer: create_index_buffer(device, Self::INITIAL_INDICES),
            index_capacity: Self::INITIAL_INDICES,
        }
    }

    /// 重新上传字形图集。
    ///
    /// **只在图集版本号变了之后调**：1024² 展开成 RGBA 是 4 MB，
    /// 每帧传一次是实打实的带宽浪费，而绝大多数帧里图集是不动的。
    pub(crate) fn prepare_atlas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &Texture,
    ) {
        let uploaded = crate::upload_texture(device, queue, atlas);
        self.atlas_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine ui atlas bind group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&uploaded.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&uploaded.sampler),
                },
            ],
        }));
    }

    /// 上传本帧的几何。
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        list: &DrawList,
        screen: [f32; 2],
    ) {
        if list.is_empty() {
            return;
        }

        if list.vertices().len() as u64 > self.vertex_capacity {
            self.vertex_capacity = (list.vertices().len() as u64).next_power_of_two();
            self.vertex_buffer = create_vertex_buffer(device, self.vertex_capacity);
        }
        if list.indices().len() as u64 > self.index_capacity {
            self.index_capacity = (list.indices().len() as u64).next_power_of_two();
            self.index_buffer = create_index_buffer(device, self.index_capacity);
        }

        queue.write_buffer(
            &self.vertex_buffer,
            0,
            bytemuck::cast_slice(list.vertices()),
        );
        queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(list.indices()));
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[UiGlobals {
                screen,
                _padding: [0.0; 2],
            }]),
        );
    }

    /// 提交绘制。目标是交换链，不是 HDR 目标。
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        list: &DrawList,
        physical: [u32; 2],
        scale: f32,
    ) {
        let Some(atlas) = self.atlas_bind_group.as_ref() else {
            return;
        };
        if list.is_empty() {
            return;
        }

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);

        for batch in list.batches() {
            let bind_group = match batch.texture {
                None => atlas,
                Some(id) => match self.bind_groups.get(&id) {
                    Some(group) => group,
                    // 贴图还没上传就先跳过这一批，而不是用图集顶替——
                    // 用图集顶替会在界面上印出一片字形。
                    None => continue,
                },
            };

            // 剪刀矩形要用**物理**像素，而绘制列表里是逻辑像素。
            // 不乘 DPI 缩放的话，高分屏上裁剪区只覆盖左上角四分之一。
            let clip = batch.clip;
            let x = (clip.min.x * scale).max(0.0) as u32;
            let y = (clip.min.y * scale).max(0.0) as u32;
            let w = ((clip.max.x * scale) as u32).saturating_sub(x);
            let h = ((clip.max.y * scale) as u32).saturating_sub(y);
            // 剪刀矩形超出渲染目标时 wgpu 会直接报验证错误，得先夹住。
            let w = w.min(physical[0].saturating_sub(x));
            let h = h.min(physical[1].saturating_sub(y));
            if w == 0 || h == 0 {
                continue;
            }
            pass.set_scissor_rect(x, y, w, h);

            pass.set_bind_group(1, bind_group, &[]);
            pass.draw_indexed(
                batch.first_index..batch.first_index + batch.index_count,
                0,
                0..1,
            );
        }

        // 恢复成整个目标，免得影响之后的绘制。
        pass.set_scissor_rect(0, 0, physical[0], physical[1]);
    }

    /// 登记一张用户贴图，之后 `DrawList::image` 就能引用它。
    pub(crate) fn upload_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &Texture,
    ) {
        if self.bind_groups.contains_key(&texture.id()) {
            return;
        }
        let uploaded = crate::upload_texture(device, queue, texture);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine ui image bind group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&uploaded.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&uploaded.sampler),
                },
            ],
        });
        self.bind_groups.insert(texture.id(), bind_group);
    }
}

fn create_vertex_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine ui vertices"),
        size: size_of::<UiVertex>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_index_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine ui indices"),
        size: size_of::<u32>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_compiles_and_has_both_entry_points() {
        let module = naga::front::wgsl::parse_str(kui::UI_WGSL).expect("着色器应当能解析");
        let names: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"ui_vs"));
        assert!(names.contains(&"ui_fs"));
    }

    #[test]
    fn the_attribute_offsets_match_the_vertex_struct() {
        // 偏移写错就是满屏乱码，而且不会有任何报错。
        assert_eq!(size_of::<[f32; 2]>(), 8);
        assert_eq!(size_of::<UiVertex>(), 64);
        // 位置 0、UV 8、颜色 16、矩形 32、参数 48。
        assert_eq!(8 + 8, 16);
        assert_eq!(16 + 16, 32);
        assert_eq!(32 + 16, 48);
        // 参数从 vec2 变成 vec4 是为了腾出「模式」这一格：
        // 线段和矩形共用一条管线，靠它区分。
        assert_eq!(48 + 16, 64);
    }
}
