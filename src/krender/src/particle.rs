//! 粒子渲染。
//!
//! 粒子是**半透明**的，这决定了它必须单独走一段流程：
//!
//! - 画在所有不透明物体与天空之后——半透明不写深度，先画就会被后画的东西盖掉；
//! - 从远到近画——alpha 混合不可交换，顺序错了颜色就错了；
//! - 方片在顶点着色器里长出来——CPU 每帧只上传粒子数组，不重建顶点缓冲。

use crate::{GpuTexture, upload_texture};
use bytemuck::{Pod, Zeroable};
use fxhash::FxHashMap;
use kcore::uuid::Uuid;
use kmath::{Mat4, Vec3};
use kparticle::{BlendMode, GpuParticle};
use kscene::ParticleItem;
use ktexture::Texture;
use std::num::NonZeroU64;

/// 粒子 pass 的全局量，对应 `particle.wgsl` 的 `ParticleGlobals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ParticleGlobals {
    view_proj: [[f32; 4]; 4],
    camera_right: [f32; 4],
    camera_up: [f32; 4],
}

/// 一个粒子系统对应的一次绘制。
///
/// 不做跨系统的合批：相邻两个系统的贴图和混合方式往往不同，
/// 而且它们之间还隔着「从远到近」的顺序要求，合并了就画错了。
pub(crate) struct ParticleBatch {
    first: u32,
    count: u32,
    texture: Uuid,
    blend: BlendMode,
}

/// 粒子 pass 所需的一组 GPU 资源。
pub(crate) struct ParticleResources {
    alpha_pipeline: wgpu::RenderPipeline,
    additive_pipeline: wgpu::RenderPipeline,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    storage_layout: wgpu::BindGroupLayout,
    storage_buffer: wgpu::Buffer,
    storage_bind_group: wgpu::BindGroup,
    /// 当前粒子缓冲能容纳的粒子数。
    capacity: u64,

    texture_layout: wgpu::BindGroupLayout,
    textures: FxHashMap<Uuid, GpuTexture>,
    bind_groups: FxHashMap<Uuid, wgpu::BindGroup>,
}

impl ParticleResources {
    /// 初始容量。粒子数超过时翻倍扩容。
    const INITIAL_CAPACITY: u64 = 1024;

    /// 没指定贴图的粒子用的内置软圆点的键。
    const DEFAULT_TEXTURE: Uuid = Uuid::nil();

    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine particle shader"),
            source: wgpu::ShaderSource::Wgsl(kparticle::PARTICLE_WGSL.into()),
        });

        // ── group(0)：每帧全局量 ──
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine particle globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<ParticleGlobals>() as u64),
                },
                count: None,
            }],
        });
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine particle globals"),
            size: size_of::<ParticleGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine particle globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        // ── group(1)：粒子数组 ──
        let storage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine particle storage layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<GpuParticle>() as u64),
                },
                count: None,
            }],
        });
        let (storage_buffer, storage_bind_group) =
            create_storage(device, &storage_layout, Self::INITIAL_CAPACITY);

        // ── group(2)：贴图 ──
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine particle texture layout"),
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
            label: Some("kengine particle pipeline layout"),
            bind_group_layouts: &[
                Option::from(&globals_layout),
                Option::from(&storage_layout),
                Option::from(&texture_layout),
            ],
            immediate_size: 0,
        });

        // 两条管线只差混合状态：着色器输出的是预乘 alpha 的颜色，
        // 于是「半透明」与「相加」共用同一个片元着色器。
        let alpha_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            depth_format,
            wgpu::BlendState {
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
            },
            "alpha",
        );
        let additive_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            depth_format,
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
            "additive",
        );

        let mut resources = Self {
            alpha_pipeline,
            additive_pipeline,
            globals_buffer,
            globals_bind_group,
            storage_layout,
            storage_buffer,
            storage_bind_group,
            capacity: Self::INITIAL_CAPACITY,
            texture_layout,
            textures: FxHashMap::default(),
            bind_groups: FxHashMap::default(),
        };

        // 内置软圆点：没有美术资源时也能看到像样的粒子，
        // 而不是一堆边缘生硬的方块。
        let dot = upload_texture(device, queue, &Texture::soft_circle(64, 1.6));
        resources.textures.insert(Self::DEFAULT_TEXTURE, dot);
        resources.ensure_bind_group(device, Self::DEFAULT_TEXTURE);
        resources
    }

    /// 收集本帧所有粒子并上传显存，返回按绘制顺序排好的批次。
    ///
    /// `items` 会被按到相机的距离**从远到近**重排。
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        items: &mut [ParticleItem<'_>],
        view_proj: Mat4,
        camera_to_world: Mat4,
        scratch: &mut Vec<GpuParticle>,
    ) -> Vec<ParticleBatch> {
        scratch.clear();
        if items.is_empty() {
            return Vec::new();
        }

        let camera_position = camera_to_world.to_scale_rotation_translation().2;

        // 系统之间也要排序：每个系统内部排好了，系统之间乱序照样会盖错。
        items.sort_unstable_by(|a, b| {
            let a = (a.aabb.center() - camera_position).length_squared();
            let b = (b.aabb.center() - camera_position).length_squared();
            b.total_cmp(&a)
        });

        let mut batches = Vec::with_capacity(items.len());
        for item in items.iter() {
            let first = scratch.len() as u32;
            item.system.collect(item.transform, camera_position, scratch);
            let count = scratch.len() as u32 - first;
            if count == 0 {
                continue;
            }

            batches.push(ParticleBatch {
                first,
                count,
                texture: self.ensure_texture(device, queue, item),
                blend: item.system.blend,
            });
        }

        if scratch.is_empty() {
            return batches;
        }

        if scratch.len() as u64 > self.capacity {
            let capacity = (scratch.len() as u64).next_power_of_two();
            let (buffer, bind_group) = create_storage(device, &self.storage_layout, capacity);
            self.storage_buffer = buffer;
            self.storage_bind_group = bind_group;
            self.capacity = capacity;
        }
        queue.write_buffer(&self.storage_buffer, 0, bytemuck::cast_slice(scratch));

        // 方片沿相机的右向量与上向量张开，于是永远正对镜头。
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[ParticleGlobals {
                view_proj: view_proj.to_cols_array_2d(),
                camera_right: Vec3::from(camera_to_world.x_axis.truncate())
                    .normalize_or_zero()
                    .extend(0.0)
                    .to_array(),
                camera_up: Vec3::from(camera_to_world.y_axis.truncate())
                    .normalize_or_zero()
                    .extend(0.0)
                    .to_array(),
            }]),
        );

        batches
    }

    /// 提交绘制。必须在不透明物体与天空**之后**调用。
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batches: &[ParticleBatch]) {
        for batch in batches {
            let Some(texture) = self.bind_groups.get(&batch.texture) else {
                continue;
            };

            pass.set_pipeline(match batch.blend {
                BlendMode::Alpha => &self.alpha_pipeline,
                BlendMode::Additive => &self.additive_pipeline,
            });
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.storage_bind_group, &[]);
            pass.set_bind_group(2, texture, &[]);
            // 六个顶点拼一个方片，几何在顶点着色器里长出来，不需要顶点缓冲。
            pass.draw(0..6, batch.first..batch.first + batch.count);
        }
    }

    /// 确保粒子系统的贴图已上传，返回绑定组的键。
    ///
    /// 贴图还在异步加载时先用内置圆点顶上，加载完成后自然换上。
    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        item: &ParticleItem<'_>,
    ) -> Uuid {
        let Some(texture) = item.system.texture.as_ref().and_then(|t| t.data_ref()) else {
            return Self::DEFAULT_TEXTURE;
        };

        let id = texture.id();
        if !self.textures.contains_key(&id) {
            let uploaded = upload_texture(device, queue, &texture);
            self.textures.insert(id, uploaded);
        }
        self.ensure_bind_group(device, id);
        id
    }

    fn ensure_bind_group(&mut self, device: &wgpu::Device, id: Uuid) {
        if self.bind_groups.contains_key(&id) {
            return;
        }
        let Some(texture) = self.textures.get(&id) else {
            return;
        };

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine particle texture bind group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
            ],
        });
        self.bind_groups.insert(id, bind_group);
    }
}

fn create_storage(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    capacity: u64,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine particle buffer"),
        size: size_of::<GpuParticle>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine particle bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    (buffer, bind_group)
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("kengine particle pipeline ({label})")),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("particle_vs"),
            compilation_options: Default::default(),
            // 顶点数据全在存储缓冲里，没有顶点缓冲。
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("particle_fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // 方片正反都要看得见：粒子绕视线轴旋转时绕序会翻转。
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            // 只测试不写入：粒子之间靠 CPU 排序决定前后，
            // 写深度反而会让同一团里靠后的粒子被自己人挡掉。
            depth_write_enabled: Option::from(false),
            depth_compare: Option::from(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use kshader::Shader;

    #[test]
    fn particle_shader_passes_validation() {
        Shader::from_wgsl(kparticle::PARTICLE_WGSL).expect("粒子着色器应当通过校验");
    }

    #[test]
    fn particle_shader_entry_points_match_pipeline() {
        let shader = Shader::from_wgsl(kparticle::PARTICLE_WGSL).unwrap();

        // 这两个名字硬编码在建管线的代码里。
        assert_eq!(shader.vertex_entry(), Some("particle_vs"));
        assert_eq!(shader.fragment_entry(), Some("particle_fs"));
    }

    #[test]
    fn particle_globals_are_16_byte_aligned() {
        // mat4x4(64) + vec4(16) × 2 = 96
        assert_eq!(size_of::<ParticleGlobals>(), 96);
        assert_eq!(size_of::<ParticleGlobals>() % 16, 0);
    }

    #[test]
    fn shader_builds_quads_from_the_vertex_index() {
        // 粒子的方片是在顶点着色器里长出来的，没有顶点缓冲。
        // 改成 CPU 生成顶点时这里会报警。
        assert!(kparticle::PARTICLE_WGSL.contains("@builtin(vertex_index)"));
        assert!(kparticle::PARTICLE_WGSL.contains("@builtin(instance_index)"));
        assert!(kparticle::PARTICLE_WGSL.contains("var<storage, read> particles"));
    }
}
