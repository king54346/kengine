//! wgpu 渲染器。遍历场景，按材质绘制每个可见的带网格节点。
//!
//! 绑定组划分：
//! - `group(0)`：每帧全局量（视图投影、相机位置、光照）
//! - `group(1)`：每个对象的变换与材质参数，用动态偏移在一个大缓冲里寻址
//! - `group(2)`：材质贴图与采样器，按材质缓存

use crate::scene::{Camera, Frustum, Scene, Vertex};

/// 顶点属性布局。字段顺序必须与 [`Vertex`] 及着色器的 `@location` 一致。
const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x3];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRIBUTES,
    }
}
use bytemuck::{Pod, Zeroable};
use kcore::uuid::Uuid;
use kmaterial::Material;
use kmath::{Mat4, Vec3};
use ktexture::{FilterMode, Texture, TextureFormat, WrapMode};
use std::{collections::HashMap, num::NonZeroU64, sync::Arc};
use wgpu::util::DeviceExt;
use winit::window::Window;

/// 每帧全局量，对应 `shader.wgsl` 的 `Globals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    camera_position: [f32; 4],
    light_direction: [f32; 4],
    light_color: [f32; 4],
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
    _padding: [f32; 2],
}

/// 已上传显存的网格。
struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

/// 本帧一次绘制调用所需的信息。
struct DrawCall {
    mesh_id: Uuid,
    /// 材质贴图绑定组的缓存键。
    texture_key: Uuid,
    uniforms: ObjectUniforms,
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
}

impl RenderStats {
    /// 本帧参与判定的对象总数。
    pub fn total(&self) -> u32 {
        self.drawn + self.culled
    }
}

/// 一帧的绘制结果，供事件循环决定后续动作。
pub(crate) enum RenderOutcome {
    /// 正常绘制完成。
    Ok,
    /// 本帧跳过（窗口被遮挡等），无需处理。
    Skip,
    /// 表面失效，需要重新配置。
    Reconfigure,
    /// 不可恢复的错误，应退出。
    Fatal,
}

pub(crate) struct Renderer {
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
    /// 单个对象 uniform 的跨距，受 GPU 对齐要求约束。
    object_stride: u64,
    /// 当前对象缓冲能容纳的对象数。
    object_capacity: u64,

    texture_layout: wgpu::BindGroupLayout,
    /// 贴图绑定组缓存，键为纹理 id；缺省白纹理用 [`Uuid::nil`]。
    texture_bind_groups: HashMap<Uuid, wgpu::BindGroup>,
    meshes: HashMap<Uuid, GpuMesh>,
    stats: RenderStats,
}

impl Renderer {
    /// 初始容量：够画 256 个物体，不够时自动翻倍。
    const INITIAL_CAPACITY: u64 = 256;

    pub(crate) async fn new(window: Arc<Window>) -> Self {
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine standard shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
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

        // ── group(1)：每个对象，动态偏移 ──
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let object_size = size_of::<ObjectUniforms>() as u64;
        let object_stride = object_size.div_ceil(alignment) * alignment;

        let object_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine object layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(object_size),
                },
                count: None,
            }],
        });

        let (object_buffer, object_bind_group) = Self::create_object_storage(
            &device,
            &object_layout,
            object_stride,
            Self::INITIAL_CAPACITY,
        );

        // ── group(2)：材质贴图 ──
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine texture layout"),
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
            label: Some("kengine pipeline layout"),
            bind_group_layouts: &[
                Option::from(&globals_layout),
                Option::from(&object_layout),
                Option::from(&texture_layout),
            ],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kengine render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Option::from(vertex_layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
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
                depth_write_enabled: Option::from(true),
                depth_compare: Option::from(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

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
            object_stride,
            object_capacity: Self::INITIAL_CAPACITY,
            texture_layout,
            texture_bind_groups: HashMap::new(),
            meshes: HashMap::new(),
            stats: RenderStats::default(),
        };

        // 预置缺省白纹理，让「没有贴图」和「有贴图」走同一条着色器路径。
        renderer.upload_texture(Uuid::nil(), &Texture::white());

        renderer
    }

    pub(crate) fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }

    /// 上一帧的渲染统计。
    pub(crate) fn stats(&self) -> RenderStats {
        self.stats
    }

    pub(crate) fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = Self::create_depth_view(&self.device, &self.config);
    }

    pub(crate) fn render(&mut self, scene: &Scene) -> RenderOutcome {
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

        let lighting = scene.lighting();
        self.queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[Globals {
                view_proj: view_proj.to_cols_array_2d(),
                camera_position: camera_position.extend(1.0).to_array(),
                // 着色器需要的是「指向光源」的方向，与传播方向相反。
                light_direction: (-lighting.direction.normalize_or_zero()).extend(0.0).to_array(),
                light_color: lighting.color.extend(lighting.ambient).to_array(),
            }]),
        );

        // 剔除用的视锥来自本帧的视图投影矩阵。
        let frustum = camera
            .frustum_culling
            .then(|| Frustum::from_view_projection(view_proj));

        // 收集绘制项，顺便把没上传过的网格与贴图传到显存。
        let mut draws = Vec::new();
        let mut stats = RenderStats::default();
        for item in scene.visible_meshes() {
            // 完全在视锥外的物体直接跳过，连显存上传都省了。
            if let Some(frustum) = &frustum
                && !frustum.intersects(&item.aabb)
            {
                stats.culled += 1;
                continue;
            }
            stats.drawn += 1;
            stats.triangles += item.mesh.triangle_count() as u32;

            let mesh = item.mesh;
            if !self.meshes.contains_key(&mesh.id()) {
                let gpu_mesh = GpuMesh {
                    vertex_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine vertex buffer"),
                            contents: bytemuck::cast_slice(mesh.vertices()),
                            usage: wgpu::BufferUsages::VERTEX,
                        },
                    ),
                    index_buffer: self.device.create_buffer_init(
                        &wgpu::util::BufferInitDescriptor {
                            label: Some("kengine index buffer"),
                            contents: bytemuck::cast_slice(mesh.indices()),
                            usage: wgpu::BufferUsages::INDEX,
                        },
                    ),
                    index_count: mesh.index_count(),
                };
                self.meshes.insert(mesh.id(), gpu_mesh);
            }

            let default_material = Material::standard();
            let material = item.material.unwrap_or(&default_material);
            let texture_key = self.ensure_material_texture(material);

            let model = item.transform;
            draws.push(DrawCall {
                mesh_id: mesh.id(),
                texture_key,
                uniforms: ObjectUniforms {
                    model: model.to_cols_array_2d(),
                    // 逆转置，保证非均匀缩放下法线方向仍然正确。
                    normal_matrix: model.inverse().transpose().to_cols_array_2d(),
                    base_color: material.base_color().to_array(),
                    metallic: material.metallic(),
                    roughness: material.roughness(),
                    _padding: [0.0; 2],
                },
            });
        }

        // 对象数超出缓冲容量时翻倍扩容。
        if draws.len() as u64 > self.object_capacity {
            let capacity = (draws.len() as u64).next_power_of_two();
            let (buffer, bind_group) = Self::create_object_storage(
                &self.device,
                &self.object_layout,
                self.object_stride,
                capacity,
            );
            self.object_buffer = buffer;
            self.object_bind_group = bind_group;
            self.object_capacity = capacity;
        }

        self.stats = stats;

        for (index, draw) in draws.iter().enumerate() {
            self.queue.write_buffer(
                &self.object_buffer,
                index as u64 * self.object_stride,
                bytemuck::cast_slice(&[draw.uniforms]),
            );
        }

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                t
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return RenderOutcome::Skip;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return RenderOutcome::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Validation => return RenderOutcome::Fatal,
        };
        let target = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kengine encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kengine render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
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

            for (index, draw) in draws.iter().enumerate() {
                let Some(gpu_mesh) = self.meshes.get(&draw.mesh_id) else {
                    continue;
                };
                let Some(texture_bind_group) = self.texture_bind_groups.get(&draw.texture_key)
                else {
                    continue;
                };

                let offset = (index as u64 * self.object_stride) as u32;
                pass.set_bind_group(1, &self.object_bind_group, &[offset]);
                pass.set_bind_group(2, texture_bind_group, &[]);
                pass.set_vertex_buffer(0, gpu_mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(gpu_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(output);

        RenderOutcome::Ok
    }

    /// 确保材质的贴图已上传，返回贴图绑定组的缓存键。
    ///
    /// 材质没有贴图、或贴图仍在异步加载时，回退到缺省白纹理。
    fn ensure_material_texture(&mut self, material: &Material) -> Uuid {
        let Some(handle) = material.base_color_texture() else {
            return Uuid::nil();
        };
        // 贴图可能还在后台加载，这一帧先用白纹理顶上，加载完自然会切过去。
        let Some(texture) = handle.data_ref() else {
            return Uuid::nil();
        };

        let id = texture.id();
        if !self.texture_bind_groups.contains_key(&id) {
            self.upload_texture(id, &texture);
        }
        id
    }

    fn upload_texture(&mut self, key: Uuid, texture: &Texture) {
        let size = wgpu::Extent3d {
            width: texture.width().max(1),
            height: texture.height().max(1),
            depth_or_array_layers: 1,
        };

        let format = match texture.format() {
            TextureFormat::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Linear => wgpu::TextureFormat::Rgba8Unorm,
        };

        let gpu_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
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

        let sampler_desc = texture.sampler();
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kengine sampler"),
            address_mode_u: convert_wrap(sampler_desc.wrap_u),
            address_mode_v: convert_wrap(sampler_desc.wrap_v),
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: convert_filter(sampler_desc.mag_filter),
            min_filter: convert_filter(sampler_desc.min_filter),
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let view = gpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine texture bind group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        self.texture_bind_groups.insert(key, bind_group);
    }

    fn create_object_storage(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        stride: u64,
        capacity: u64,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine object buffer"),
            size: stride * capacity,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine object bind group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: NonZeroU64::new(size_of::<ObjectUniforms>() as u64),
                }),
            }],
        });

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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        texture.create_view(&wgpu::TextureViewDescriptor::default())
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

    const STANDARD_SHADER: &str = include_str!("shader.wgsl");

    #[test]
    fn standard_shader_passes_validation() {
        // 引擎自带的着色器必须能通过 naga 校验，否则运行时才会在建管线时崩。
        Shader::from_wgsl(STANDARD_SHADER).expect("标准着色器应当通过校验");
    }

    #[test]
    fn shader_entry_points_match_pipeline() {
        let shader = Shader::from_wgsl(STANDARD_SHADER).unwrap();

        // 这两个名字硬编码在建管线的代码里，改了着色器却忘改这里会导致启动崩溃。
        assert_eq!(shader.vertex_entry(), Some("vs_main"));
        assert_eq!(shader.fragment_entry(), Some("fs_main"));
    }

    #[test]
    fn uniform_sizes_match_wgsl_layout() {
        // Globals：mat4x4(64) + vec4(16) × 3 = 112
        assert_eq!(size_of::<Globals>(), 112);
        // ObjectUniforms：mat4x4(64) × 2 + vec4(16) + f32 × 2 + vec2 填充(8) = 160
        assert_eq!(size_of::<ObjectUniforms>(), 160);
    }

    #[test]
    fn uniform_structs_are_16_byte_aligned() {
        // WGSL 的 uniform 地址空间要求结构体按 16 字节对齐，
        // 不满足时 wgpu 会在创建绑定组时报错。
        assert_eq!(size_of::<Globals>() % 16, 0);
        assert_eq!(size_of::<ObjectUniforms>() % 16, 0);
    }
}
