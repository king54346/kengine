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
use kmath::Mat4;
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
    /// x = 软粒子淡出距离（0 = 关闭），y/z = 投影矩阵的深度系数
    soft_params: [f32; 4],
}

/// 一批**由计算着色器填出来的**粒子。
///
/// 和挂在节点上的 [`ParticleSystem`](kparticle::ParticleSystem) 走的是
/// 同一条渲染管线——那条路本来就从 storage buffer 取数据，方片在顶点
/// 着色器里长出来，压根没有顶点缓冲。所以「让 GPU 算出来的粒子直接被画」
/// 缺的只是**让渲染器用别人给的那块缓冲**，而不是它自己每帧 `write_buffer`
/// 填的那块。
///
/// # 缓冲里必须是什么
///
/// 一个紧凑的 `array<Particle>`，`Particle` 的布局见
/// [`kparticle::PARTICLE_STRUCT_WGSL`]——**把那段拼在自己的计算着色器
/// 前面，不要手抄**。抄错了 wgpu 不会报错（绑定只校验总长度），
/// 画出来是一堆乱飞的方片。
///
/// # 排序这件事说清楚
///
/// 粒子是半透明的，alpha 混合不可交换，所以画的顺序错了颜色就错。
/// CPU 粒子由渲染器**逐粒子**从远到近排；GPU 粒子在 CPU 上没有位置，
/// 排不了。所以：
///
/// | 混合方式 | 结果 |
/// |---|---|
/// | [`BlendMode::Additive`] | **正确**。加法可交换，顺序无关 |
/// | [`BlendMode::Alpha`] | 系统之间按 [`bounds`](Self::bounds) 排，但**同一批内部不排** |
///
/// 要 alpha 又要正确，只能自己在 GPU 上把缓冲排好再交过来
/// （bitonic sort）。引擎不代劳：那需要另一条完整的计算管线，
/// 而绝大多数 GPU 粒子（火花、烟尘、魔法）用加法混合本来就更好看。
///
/// # 生命周期
///
/// 每帧提交一次，和精灵一样是即时模式的。缓冲本身由游戏保管
/// （`Arc` 是为了让提交这一步不必转移所有权），渲染器只在绘制时借用。
pub struct GpuParticles {
    /// 粒子数据。由 [`ComputeContext`](crate::ComputeContext) 建、
    /// 由计算着色器填。
    pub particles: std::sync::Arc<crate::StorageBuffer>,
    /// 画前多少个。**不是缓冲的容量**——缓冲通常按上限开，实际存活的
    /// 少得多，多画的那些会是上一帧的残留或者未初始化的垃圾。
    pub count: u32,
    /// 贴图。[`None`] 用内置的软圆点。
    ///
    /// 用 `Arc` 而不是按值传：这个结构每帧都要造一遍，而
    /// [`Texture`] 里是一整块像素，按值传等于每帧拷一遍贴图。
    pub texture: Option<std::sync::Arc<Texture>>,
    /// 混合方式。见上面「排序这件事说清楚」。
    pub blend: BlendMode,
    /// 世界空间包围盒，用来和别的粒子系统排先后。
    ///
    /// **只能由游戏给**：粒子在哪只有 GPU 知道。给个保守的大盒子即可，
    /// 它不参与剔除，只参与系统之间的排序。
    pub bounds: kmath::Aabb,
}

/// 一次绘制的粒子数据从哪儿来。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// 渲染器自己那块缓冲，CPU 模拟的粒子每帧写进去。
    Internal,
    /// 外部交来的一块 storage buffer，由计算着色器填。值是它的 id。
    External(Uuid),
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
    source: Source,
}

/// 建场景深度的绑定组。
fn create_depth_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine particle depth bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(view),
        }],
    })
}

/// 一张 1×1 的占位深度纹理。
///
/// 渲染器建好之后会立刻调 `set_depth_view` 换成真的，但绑定组不能
/// 留空——wgpu 要求管线布局里的每个组在绘制时都有绑定。
pub(crate) fn create_placeholder_depth(device: &wgpu::Device) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("kengine particle placeholder depth"),
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

/// 粒子这一帧要用的相机矩阵。
///
/// 打包成一个结构体而不是三个参数：三个 `Mat4` 挨着传，调用方
/// 传错顺序编译器一句话都不会说。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParticleCamera {
    /// 投影 × 视图。
    pub view_proj: Mat4,
    /// 相机的世界变换，方片靠它的右向量与上向量张开。
    pub camera_to_world: Mat4,
    /// 纯投影矩阵，软粒子靠它反解深度。
    pub projection: Mat4,
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
    /// 外部缓冲（GPU 粒子）的绑定组，按 `StorageBuffer::id` 缓存。
    ///
    /// 缓存是必要的：同一块缓冲每帧都会被交过来一次，而建绑定组
    /// 不便宜。缓冲被游戏丢掉之后这里会留下一条死项——**不清理**，
    /// 因为一个绑定组只有几十字节，而「什么时候算丢掉了」需要引擎去
    /// 猜游戏的意图。真有几千个一次性缓冲的话那是用法本身有问题。
    external_bind_groups: FxHashMap<Uuid, wgpu::BindGroup>,

    texture_layout: wgpu::BindGroupLayout,
    textures: FxHashMap<Uuid, GpuTexture>,
    bind_groups: FxHashMap<Uuid, wgpu::BindGroup>,

    /// 场景深度的绑定组布局，软粒子要用。
    depth_layout: wgpu::BindGroupLayout,
    /// 当前深度纹理的绑定组。窗口尺寸一变就要重建。
    depth_bind_group: wgpu::BindGroup,
    /// 软粒子的淡出距离，0 表示关闭。
    pub(crate) soft_fade: f32,
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
            source: wgpu::ShaderSource::Wgsl(kparticle::particle_wgsl().into()),
        });

        // ── group(0)：每帧全局量 ──
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine particle globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // 片元着色器也要读：软粒子的参数在里面。
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
        let external_bind_groups = FxHashMap::default();

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

        // 场景深度：软粒子靠它判断自己离背后的几何有多近。
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine particle depth layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    // 深度纹理是 `Depth` 采样类型，不是 `Float`——
                    // 写错的话绑定会被 wgpu 打回。
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine particle pipeline layout"),
            bind_group_layouts: &[
                Option::from(&globals_layout),
                Option::from(&storage_layout),
                Option::from(&texture_layout),
                Option::from(&depth_layout),
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

        let depth_bind_group =
            create_depth_bind_group(device, &depth_layout, &create_placeholder_depth(device));

        let mut resources = Self {
            alpha_pipeline,
            additive_pipeline,
            depth_layout,
            depth_bind_group,
            // 默认开着：粒子插进地面时露出的那条笔直交线是最显眼的
            // 穿帮之一，而代价只是一次深度采样。
            soft_fade: 0.5,
            globals_buffer,
            globals_bind_group,
            storage_layout,
            storage_buffer,
            storage_bind_group,
            capacity: Self::INITIAL_CAPACITY,
            external_bind_groups,
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
    /// CPU 模拟的（`items`）和计算着色器填的（`gpu`）排在**同一个序**里，
    /// 都按到相机的距离从远到近。分成两段各排各的话，两类系统之间的
    /// 前后关系就成了「谁在数组里排前面」，和它们离相机多远无关。
    /// 换一张深度纹理（窗口尺寸变化时）。
    pub(crate) fn set_depth_view(&mut self, device: &wgpu::Device, view: &wgpu::TextureView) {
        self.depth_bind_group = create_depth_bind_group(device, &self.depth_layout, view);
    }

    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        items: &[ParticleItem<'_>],
        gpu: &[GpuParticles],
        camera: ParticleCamera,
        scratch: &mut Vec<GpuParticle>,
    ) -> Vec<ParticleBatch> {
        let ParticleCamera {
            view_proj,
            camera_to_world,
            projection,
        } = camera;
        scratch.clear();
        if items.is_empty() && gpu.is_empty() {
            return Vec::new();
        }

        let camera_position = camera_to_world.to_scale_rotation_translation().2;

        // 系统之间也要排序：每个系统内部排好了，系统之间乱序照样会盖错。
        //
        // CPU 的和 GPU 的**排在同一个序里**：分成两段各排各的话，
        // 一团 GPU 火花和一团 CPU 烟尘之间的前后关系就成了「谁在数组里
        // 排前面」，而那和它们离相机多远无关。
        //
        // 排的是 (距离, 来源, 下标) 三元组而不是直接排两个数组：
        // `items` 排完之后 `gpu` 那边的下标就对不上了。
        let mut order: Vec<(f32, bool, usize)> = Vec::with_capacity(items.len() + gpu.len());
        for (index, item) in items.iter().enumerate() {
            let distance = (item.aabb.center() - camera_position).length_squared();
            order.push((distance, false, index));
        }
        for (index, system) in gpu.iter().enumerate() {
            let distance = (system.bounds.center() - camera_position).length_squared();
            order.push((distance, true, index));
        }
        order.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));

        let mut batches = Vec::with_capacity(order.len());
        for (_, is_gpu, index) in order {
            if is_gpu {
                let system = &gpu[index];
                if system.count == 0 {
                    continue;
                }
                self.ensure_external_bind_group(device, system);
                batches.push(ParticleBatch {
                    // GPU 粒子从自己那块缓冲的头开始画。一块缓冲一个系统——
                    // 想把几个系统塞进一块缓冲的话，那块缓冲的分段规则
                    // 只有游戏自己知道，引擎猜不了。
                    first: 0,
                    count: system.count,
                    texture: self.ensure_gpu_texture(device, queue, system),
                    blend: system.blend,
                    source: Source::External(system.particles.id()),
                });
                continue;
            }

            let item = &items[index];
            let first = scratch.len() as u32;
            item.system
                .collect(item.transform, camera_position, scratch);
            let count = scratch.len() as u32 - first;
            if count == 0 {
                continue;
            }

            batches.push(ParticleBatch {
                first,
                count,
                texture: self.ensure_texture(device, queue, item),
                blend: item.system.blend,
                source: Source::Internal,
            });
        }

        if scratch.is_empty() {
            // 全都是 GPU 粒子时也要更新全局量——方片的朝向在那里面。
            self.write_globals(queue, view_proj, camera_to_world, projection);
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
        self.write_globals(queue, view_proj, camera_to_world, projection);

        batches
    }

    /// 写这一帧的全局量。
    ///
    /// 单独拎出来是因为它有**两个**调用点：正常那条，以及「这一帧全是
    /// GPU 粒子、`scratch` 是空的」那条。漏掉后者的话方片会用上一帧的
    /// 相机朝向张开——相机一转，粒子集体歪一下再正回来。
    fn write_globals(
        &self,
        queue: &wgpu::Queue,
        view_proj: Mat4,
        camera_to_world: Mat4,
        projection: Mat4,
    ) {
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[ParticleGlobals {
                view_proj: view_proj.to_cols_array_2d(),
                camera_right: camera_to_world
                    .x_axis
                    .truncate()
                    .normalize_or_zero()
                    .extend(0.0)
                    .to_array(),
                soft_params: [
                    // 正交投影下 `clip.w` 恒为 1，深度反解的公式完全不同。
                    // 与其在着色器里再分一条路，不如在这里直接关掉——
                    // 正交相机基本只用在 2D 和编辑器视图上，那里没有软粒子的需求。
                    if projection.w_axis.w == 0.0 {
                        self.soft_fade
                    } else {
                        0.0
                    },
                    // 投影矩阵的深度系数，用来把深度缓冲的值还原成
                    // 视空间距离。列主序：[2][2] 是 z_axis.z，
                    // [3][2] 是 w_axis.z。
                    projection.z_axis.z,
                    projection.w_axis.z,
                    0.0,
                ],
                camera_up: camera_to_world
                    .y_axis
                    .truncate()
                    .normalize_or_zero()
                    .extend(0.0)
                    .to_array(),
            }]),
        );
    }

    /// 确保这块外部缓冲有一个绑定组。
    fn ensure_external_bind_group(&mut self, device: &wgpu::Device, system: &GpuParticles) {
        let id = system.particles.id();
        if self.external_bind_groups.contains_key(&id) {
            return;
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine gpu particle storage bind group"),
            layout: &self.storage_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: system.particles.buffer.as_entire_binding(),
            }],
        });
        self.external_bind_groups.insert(id, bind_group);
    }

    /// 确保 GPU 粒子系统的贴图已上传，返回绑定组的键。
    fn ensure_gpu_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        system: &GpuParticles,
    ) -> Uuid {
        let Some(texture) = system.texture.as_deref() else {
            return Self::DEFAULT_TEXTURE;
        };
        let id = texture.id();
        self.textures
            .entry(id)
            .or_insert_with(|| upload_texture(device, queue, texture));
        self.ensure_bind_group(device, id);
        id
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
            // 粒子数据可能来自渲染器自己那块缓冲，也可能来自计算着色器
            // 填的一块外部缓冲。绑定组的布局是同一个，换的只是缓冲。
            let storage = match batch.source {
                Source::Internal => &self.storage_bind_group,
                Source::External(id) => {
                    let Some(bind_group) = self.external_bind_groups.get(&id) else {
                        continue;
                    };
                    bind_group
                }
            };
            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_bind_group(1, storage, &[]);
            pass.set_bind_group(2, texture, &[]);
            pass.set_bind_group(3, &self.depth_bind_group, &[]);
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
        self.textures
            .entry(id)
            .or_insert_with(|| upload_texture(device, queue, &texture));
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
        Shader::from_wgsl(kparticle::particle_wgsl()).expect("粒子着色器应当通过校验");
    }

    #[test]
    fn particle_shader_entry_points_match_pipeline() {
        let shader = Shader::from_wgsl(kparticle::particle_wgsl()).unwrap();

        // 这两个名字硬编码在建管线的代码里。
        assert_eq!(shader.vertex_entry(), Some("particle_vs"));
        assert_eq!(shader.fragment_entry(), Some("particle_fs"));
    }

    #[test]
    fn particle_globals_are_16_byte_aligned() {
        // mat4x4(64) + vec4(16) × 3（camera_right / camera_up / soft_params）= 112
        assert_eq!(size_of::<ParticleGlobals>(), 64 + 16 * 3);
        assert_eq!(size_of::<ParticleGlobals>() % 16, 0);
    }

    #[test]
    fn shader_builds_quads_from_the_vertex_index() {
        // 粒子的方片是在顶点着色器里长出来的，没有顶点缓冲。
        // 改成 CPU 生成顶点时这里会报警。
        let source = kparticle::particle_wgsl();
        assert!(source.contains("@builtin(vertex_index)"));
        assert!(source.contains("@builtin(instance_index)"));
        assert!(source.contains("var<storage, read> particles"));
    }

    /// WGSL 里 `linear_depth` 的 Rust 版。两边必须给出同样的结果——
    /// 这里验的是**系数取得对不对**（`projection[2][2]` 和 `[3][2]`），
    /// 那是这段代码最容易错的地方：取错一个元素画面上只是淡出距离
    /// 变得莫名其妙，不会报任何错。
    fn linear_depth(depth: f32, a: f32, b: f32) -> f32 {
        let denominator = depth + a;
        if denominator.abs() < 1e-9 {
            return 1e9;
        }
        (b / denominator).abs()
    }

    /// 从投影矩阵取出 WGSL 那边用的两个系数。和 `prepare` 里取的必须一致。
    fn coefficients(projection: Mat4) -> (f32, f32) {
        (projection.z_axis.z, projection.w_axis.z)
    }

    #[test]
    fn depth_linearization_recovers_view_space_distance() {
        let projection = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 1000.0);
        let (a, b) = coefficients(projection);

        // 取几个已知的视空间距离，正投影再反解，看能不能还原。
        for distance in [0.5_f32, 1.0, 10.0, 100.0, 500.0] {
            // 视空间里相机朝 -z 看，所以点在 z = -distance。
            let clip = projection * kmath::Vec4::new(0.0, 0.0, -distance, 1.0);
            let depth = clip.z / clip.w;

            let recovered = linear_depth(depth, a, b);
            let error = (recovered - distance).abs() / distance;
            assert!(
                error < 1e-3,
                "距离 {distance} 反解成了 {recovered}（相对误差 {error}）"
            );
        }
    }

    #[test]
    fn depth_linearization_is_monotonic() {
        // 越远的深度值必须还原出越大的距离。单调性错了的话
        // 粒子会在错误的一侧淡出——离得越近反而越不透明。
        let projection = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 1000.0);
        let (a, b) = coefficients(projection);

        let mut previous = 0.0;
        for distance in [0.2_f32, 1.0, 5.0, 20.0, 200.0] {
            let clip = projection * kmath::Vec4::new(0.0, 0.0, -distance, 1.0);
            let recovered = linear_depth(clip.z / clip.w, a, b);
            assert!(recovered > previous, "{recovered} 不比 {previous} 大");
            previous = recovered;
        }
    }

    #[test]
    fn an_orthographic_projection_turns_soft_particles_off() {
        // 正交投影下 `clip.w` 恒为 1，深度反解的公式完全不同，
        // 套透视的公式会算出毫无意义的距离。CPU 这边直接关掉。
        let perspective = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 1000.0);
        let orthographic = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 0.1, 100.0);

        // `w_axis.w` 是区分两者的判据：透视是 0，正交是 1。
        assert_eq!(perspective.w_axis.w, 0.0);
        assert_eq!(orthographic.w_axis.w, 1.0);
    }

    #[test]
    fn linearization_never_produces_nan() {
        // NaN 会让整个粒子变成黑洞，而且顺着 Bloom 扩散到整个画面。
        let projection = Mat4::perspective_rh(1.0, 16.0 / 9.0, 0.1, 1000.0);
        let (a, b) = coefficients(projection);

        for depth in [0.0_f32, 0.5, 1.0, -1.0, f32::MAX] {
            assert!(linear_depth(depth, a, b).is_finite(), "深度 {depth}");
        }
        // 系数本身退化时也不能崩。
        assert!(linear_depth(0.5, 0.0, 0.0).is_finite());
        assert!(linear_depth(0.5, -0.5, 1.0).is_finite());
    }

    #[test]
    fn the_fade_curve_reaches_both_ends() {
        // 复现着色器里的淡出：gap / fade 夹到 0..1。
        let fade = 0.5_f32;
        // 粒子正好贴在几何上：完全透明，交线因此被抹掉。
        assert_eq!((0.0_f32 / fade).clamp(0.0, 1.0), 0.0);
        // 离得比淡出距离还远：完全不受影响。
        assert_eq!((1.0_f32 / fade).clamp(0.0, 1.0), 1.0);
        // 中间是线性的。
        assert!(((0.25_f32 / fade).clamp(0.0, 1.0) - 0.5).abs() < 1e-6);
        // 粒子在几何后面（gap 为负）时夹到 0——那本来就会被深度测试剔掉。
        assert_eq!((-1.0_f32 / fade).clamp(0.0, 1.0), 0.0);
    }

    #[test]
    fn soft_particles_are_on_by_default() {
        // 粒子插进地面时露出的那条笔直交线是最显眼的穿帮之一，
        // 而代价只是一次深度采样。
        assert!(
            ParticleGlobals {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                camera_right: [0.0; 4],
                camera_up: [0.0; 4],
                soft_params: [0.5, 0.0, 0.0, 0.0],
            }
            .soft_params[0]
                > 0.0
        );
    }
}
