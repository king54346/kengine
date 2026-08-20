//! 2D 精灵渲染。
//!
//! 画在主 pass 里、天空之后、粒子之前——精灵是半透明的，要在不透明
//! 物体之后画；但它又该被粒子盖住（粒子通常是特效）。
//!
//! # 一批一次绘制
//!
//! 几何在顶点着色器里长出来，CPU 每帧只上传实例数组。同一张纹理的
//! 相邻精灵合成一批，一批一次 `draw`。3D 那条路是逐精灵绑材质、
//! 逐精灵提交，几万个精灵时瓶颈全在提交上。
//!
//! # 顺序由 CPU 定
//!
//! 精灵不写深度也不测深度：它们全在同一个平面上，深度值一样，
//! 深度缓冲帮不上忙。正确的前后关系完全靠 `ksprite::sort_and_batch`
//! 排出来的顺序——所以这里**绝不能**重排批次。

use bytemuck::{Pod, Zeroable};
use fxhash::FxHashMap;
use kcore::uuid::Uuid;
use kmath::Mat4;
use ksprite::{Batch, GpuSprite};
use ktexture::Texture;
use std::num::NonZeroU64;

/// 2D pass 的全局量，对应 `sprite2d.wgsl` 的 `Globals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SpriteGlobals {
    view_proj: [[f32; 4]; 4],
}

/// 2D pass 的一组 GPU 资源。
pub(crate) struct SpriteResources {
    pipeline: wgpu::RenderPipeline,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    storage_layout: wgpu::BindGroupLayout,
    storage_buffer: wgpu::Buffer,
    storage_bind_group: wgpu::BindGroup,
    capacity: u64,

    texture_layout: wgpu::BindGroupLayout,
    bind_groups: FxHashMap<Uuid, wgpu::BindGroup>,

    /// 逐帧复用的上传暂存区。
    scratch: Vec<GpuSprite>,
}

impl SpriteResources {
    const INITIAL_CAPACITY: u64 = 2048;

    pub(crate) fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine sprite2d shader"),
            source: wgpu::ShaderSource::Wgsl(ksprite::SPRITE2D_WGSL.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine sprite2d globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<SpriteGlobals>() as u64),
                },
                count: None,
            }],
        });
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine sprite2d globals"),
            size: size_of::<SpriteGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine sprite2d globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let storage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine sprite2d storage layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<GpuSprite>() as u64),
                },
                count: None,
            }],
        });
        let (storage_buffer, storage_bind_group) =
            create_storage(device, &storage_layout, Self::INITIAL_CAPACITY);

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine sprite2d texture layout"),
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
            label: Some("kengine sprite2d pipeline layout"),
            bind_group_layouts: &[
                Option::from(&globals_layout),
                Option::from(&storage_layout),
                Option::from(&texture_layout),
            ],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine sprite2d pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("sprite_vs"),
                compilation_options: Default::default(),
                // 顶点数据全在存储缓冲里，没有顶点缓冲。
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("sprite_fs"),
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
                // 精灵正反都要看得见：绕 Z 轴转过 180° 之后绕序会翻。
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                // 既不写也不测深度：精灵全在同一个平面上，深度值一样，
                // 深度缓冲帮不上忙。前后关系完全靠 CPU 排出来的顺序。
                depth_write_enabled: Option::from(false),
                depth_compare: Option::from(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            globals_buffer,
            globals_bind_group,
            storage_layout,
            storage_buffer,
            storage_bind_group,
            capacity: Self::INITIAL_CAPACITY,
            texture_layout,
            bind_groups: FxHashMap::default(),
            scratch: Vec::new(),
        }
    }

    /// 上传本帧的精灵。
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sprites: &[ksprite::SpriteInstance],
        view_proj: Mat4,
    ) {
        self.scratch.clear();
        self.scratch.extend(sprites.iter().map(GpuSprite::from));
        if self.scratch.is_empty() {
            return;
        }

        if self.scratch.len() as u64 > self.capacity {
            let capacity = (self.scratch.len() as u64).next_power_of_two();
            let (buffer, bind_group) = create_storage(device, &self.storage_layout, capacity);
            self.storage_buffer = buffer;
            self.storage_bind_group = bind_group;
            self.capacity = capacity;
        }
        queue.write_buffer(&self.storage_buffer, 0, bytemuck::cast_slice(&self.scratch));
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[SpriteGlobals {
                view_proj: view_proj.to_cols_array_2d(),
            }]),
        );
    }

    /// 提交绘制。
    ///
    /// **必须按 `batches` 给的顺序画**：那是 CPU 排好的前后关系，
    /// 重排就会让半透明的东西盖错。
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batches: &[Batch]) {
        if batches.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_bind_group(1, &self.storage_bind_group, &[]);

        for batch in batches {
            // 贴图还没上传就跳过这一批，而不是用别的顶替——
            // 顶替会在画面上印出完全不相干的图。
            let Some(texture) = self.bind_groups.get(&batch.texture) else {
                continue;
            };
            pass.set_bind_group(2, texture, &[]);
            // 六个顶点拼一个方片，几何在顶点着色器里长出来。
            let first = batch.first as u32;
            pass.draw(0..6, first..first + batch.count as u32);
        }
    }

    /// 登记一张精灵贴图。
    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) {
        if self.bind_groups.contains_key(&texture.id()) {
            return;
        }
        let uploaded = crate::upload_texture(device, queue, texture);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine sprite2d texture bind group"),
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

fn create_storage(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    capacity: u64,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine sprite2d buffer"),
        size: size_of::<GpuSprite>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine sprite2d bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    (buffer, bind_group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_compiles_and_has_both_entry_points() {
        let module =
            naga::front::wgsl::parse_str(ksprite::SPRITE2D_WGSL).expect("着色器应当能解析");
        let names: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"sprite_vs"));
        assert!(names.contains(&"sprite_fs"));
    }

    #[test]
    fn the_instance_struct_is_storage_buffer_aligned() {
        // WGSL 的存储缓冲要求 16 字节对齐。不对齐的话每个实例都会
        // 读到上一个的尾巴，满屏乱码。
        assert_eq!(size_of::<GpuSprite>() % 16, 0);
        assert_eq!(size_of::<GpuSprite>(), 64);
    }
}
