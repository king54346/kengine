//! GPU 逐实例剔除 + 间接绘制。
//!
//! CPU 按节点剔视锥（实例化节点整组算一个），批次照旧在 CPU 上建；这里在那之后再按**实例**剔一遍：
//! 一个计算 pass 对每个（槽, 视图）判可见，把看得见的槽压紧写进那个视图的一段，同时累加间接绘制
//! 参数里的实例数。之后各个 pass 不再 `draw_indexed`，改成 `draw_indexed_indirect`。
//!
//! # 视图和段
//!
//! 视图 0 是相机，1.. 是阴影的各层。各个 pass 绑的槽缓冲（`instance_slots`）就是压紧结果，
//! 一个视图占一段、每段 `槽总数` 个槽；批次在段里的起点和原始槽数组里一样，所以
//! `first_instance = 视图 × 槽总数 + batch.first`，`index_count` / `first_index` 也都是 CPU 预填的，
//! GPU 只写 `instance_count`。着色器拿 `instance_index` 查槽，和不剔除时一模一样，一行都不用改。
//!
//! 阴影的每一层是自己的一段、自己的一组参数，所以一次 dispatch（y 维是视图）就把相机和所有阴影层
//! 都剔完了，不用 N + 1 次。
//!
//! # 顺序
//!
//! 原子加法压紧不保序。不透明的、阴影里的都无所谓；相机视图里的半透明批次要按远近画，
//! 它们不剔除、原样拷到同一个位置，照旧直接绘制（`CullBatch::ORDERED`）。
//!
//! # 前提
//!
//! `first_instance` 不为 0 的间接绘制要 `Features::INDIRECT_FIRST_INSTANCE`（WebGPU 的可选特性
//! `indirect-first-instance`）。适配器没有的话不建这一套，渲染器照旧走 CPU 那条路。

use bytemuck::{Pod, Zeroable};

/// 槽不属于任何批次（不会出现，防御用）。
pub(crate) const NO_BATCH: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CullParams {
    /// x = 槽总数，y = 每个视图的批次数，z = 视图数
    counts: [u32; 4],
}

/// 剔除表里的一个批次，对应 `cull.wgsl` 的 `CullBatch`。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq, Eq)]
pub(crate) struct CullBatch {
    pub first: u32,
    pub count: u32,
    pub flags: u32,
    pub _pad: u32,
}

impl CullBatch {
    /// 相机视图里保持原顺序（半透明）：原样拷过去、不剔除，照旧直接绘制。
    pub const ORDERED: u32 = 1;
}

/// 一个视图，对应 `cull.wgsl` 的 `CullView`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct CullView {
    pub matrix: [[f32; 4]; 4],
    /// x = 阴影图边长，y = 投影尺寸下限（纹素，0 = 不按尺寸剔）
    pub params: [f32; 4],
}

/// 一个对象的包围盒，对应 `cull.wgsl` 的 `ObjectBounds`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct ObjectBounds {
    pub model: [[f32; 4]; 4],
    /// 网格的局部包围盒；`local_min[3] = 1`：不准（蒙皮、形变），不按实例测、退回整组判。
    pub local_min: [f32; 4],
    pub local_max: [f32; 4],
    /// 世界包围盒；`world_min[3] = 0`：不投影。
    pub world_min: [f32; 4],
    pub world_max: [f32; 4],
}

/// 间接绘制参数，布局同 `wgpu::util::DrawIndexedIndirectArgs`。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq, Eq)]
pub(crate) struct DrawArgs {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
}

/// 一帧剔除要的全部输入（CPU 上拼好的）。
pub(crate) struct CullInput<'a> {
    pub slots: &'a [[u32; 2]],
    /// 每个槽属于剔除表里的哪个批次。
    pub slot_batch: &'a [u32],
    pub batches: &'a [CullBatch],
    pub views: &'a [CullView],
    pub bounds: &'a [ObjectBounds],
    /// `视图 × 批次数 + 批次` 排好的初始参数（实例数是 0，半透明的相机视图是原数）。
    pub args: &'a [DrawArgs],
}

/// 按需翻倍的存储缓冲。
struct Storage {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl Storage {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Self {
            buffer: create(device, label, usage, 256),
            label,
            usage,
        }
    }

    /// 写 `data`，放不下就换一块翻倍的。
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u8]) {
        if data.len() as u64 > self.buffer.size() {
            let size = (data.len() as u64).next_power_of_two();
            self.buffer = create(device, self.label, self.usage, size);
        }
        if !data.is_empty() {
            queue.write_buffer(&self.buffer, 0, data);
        }
    }
}

fn create(
    device: &wgpu::Device,
    label: &'static str,
    usage: wgpu::BufferUsages,
    size: u64,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(256),
        usage,
        mapped_at_creation: false,
    })
}

/// 剔除 pass 的管线和它的几块缓冲。
pub(crate) struct GpuCull {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    source: Storage,
    slot_batch: Storage,
    batches: Storage,
    views: Storage,
    bounds: Storage,
    args: Storage,
    /// 本帧的 (槽总数, 每个视图的批次数, 视图数)。
    counts: (u32, u32, u32),
}

impl GpuCull {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine cull layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, true),
                storage(6, true),
                storage(7, false),
                storage(8, false),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine cull shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cull.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine cull pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("kengine cull pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("cull"),
            compilation_options: Default::default(),
            cache: None,
        });
        let input = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        Self {
            pipeline,
            layout,
            params: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kengine cull params"),
                size: size_of::<CullParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            source: Storage::new(device, "kengine cull source slots", input),
            slot_batch: Storage::new(device, "kengine cull slot batches", input),
            batches: Storage::new(device, "kengine cull batches", input),
            views: Storage::new(device, "kengine cull views", input),
            bounds: Storage::new(device, "kengine cull bounds", input),
            args: Storage::new(
                device,
                "kengine indirect args",
                // COPY_SRC：测试里读回实例数核对剔除。
                input | wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_SRC,
            ),
            counts: (0, 0, 0),
        }
    }

    /// 把这一帧的输入写进显存（`queue.write_buffer`，在本帧提交的命令之前生效）。
    pub(crate) fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, input: &CullInput) {
        self.counts = (
            input.slots.len() as u32,
            input.batches.len() as u32,
            input.views.len() as u32,
        );
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&CullParams {
                counts: [self.counts.0, self.counts.1, self.counts.2, 0],
            }),
        );
        self.source
            .write(device, queue, bytemuck::cast_slice(input.slots));
        self.slot_batch
            .write(device, queue, bytemuck::cast_slice(input.slot_batch));
        self.batches
            .write(device, queue, bytemuck::cast_slice(input.batches));
        self.views
            .write(device, queue, bytemuck::cast_slice(input.views));
        self.bounds
            .write(device, queue, bytemuck::cast_slice(input.bounds));
        self.args
            .write(device, queue, bytemuck::cast_slice(input.args));
    }

    /// 编码剔除 pass。`culled` 是各个 pass 绑的槽缓冲（容量至少 槽总数 × 视图数），
    /// `instances` 是实例数据缓冲。
    pub(crate) fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        culled: &wgpu::Buffer,
        instances: &wgpu::Buffer,
    ) {
        let (slots, _, views) = self.counts;
        if slots == 0 || views == 0 {
            return;
        }
        fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        // 每帧现建：缓冲随时可能翻倍换掉，绑定组只是个描述符，建一个很便宜。
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine cull bind group"),
            layout: &self.layout,
            entries: &[
                entry(0, &self.params),
                entry(1, &self.source.buffer),
                entry(2, &self.slot_batch.buffer),
                entry(3, &self.batches.buffer),
                entry(4, &self.views.buffer),
                entry(5, &self.bounds.buffer),
                entry(6, instances),
                entry(7, culled),
                entry(8, &self.args.buffer),
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("kengine cull pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(slots.div_ceil(64), views, 1);
    }

    /// 间接参数缓冲，和第 `view` 个视图里第 `batch` 个批次那组参数的字节偏移。
    pub(crate) fn args_for(&self, view: u32, batch: u32) -> (&wgpu::Buffer, u64) {
        let index = view as u64 * self.counts.1 as u64 + batch as u64;
        (&self.args.buffer, index * size_of::<DrawArgs>() as u64)
    }
}

/// 一个阴影层：光空间矩阵和阴影图边长。
pub(crate) struct ShadowView {
    pub matrix: kmath::Mat4,
    pub resolution: u32,
}

impl crate::Renderer {
    /// 本帧要不要走 GPU 剔除；要的话给批次编上剔除表下标、把输入写进显存，返回视图数（相机 + 阴影层）。
    /// 不走返回 `None`，什么都不动。
    ///
    /// `lists` 是 不透明、半透明、遮罩 三组批次，按这个顺序拼成剔除表——阴影的批次是
    /// 「不透明 + 半透明」，下标正好接得上。
    pub(crate) fn prepare_gpu_cull(
        &mut self,
        frame: &crate::FrameInstances<'_>,
        lists: [&mut [crate::Batch]; 3],
        camera: kmath::Mat4,
        shadows: &[ShadowView],
        min_shadow_texels: f32,
    ) -> Option<u32> {
        self.cull_active = false;
        if !self.gpu_culling() || frame.slots.is_empty() {
            return None;
        }
        let slot_count = frame.slots.len() as u32;
        let mut table = Vec::new();
        let mut ranges = Vec::new();
        let mut slot_batch = vec![NO_BATCH; frame.slots.len()];
        for (list, ordered) in lists.into_iter().zip([false, true, false]) {
            for batch in list.iter_mut() {
                batch.cull = table.len() as u32;
                let span = batch.first as usize..(batch.first + batch.count) as usize;
                slot_batch[span].fill(batch.cull);
                table.push(CullBatch {
                    first: batch.first,
                    count: batch.count,
                    flags: if ordered { CullBatch::ORDERED } else { 0 },
                    _pad: 0,
                });
                ranges.push(batch.index_range);
            }
        }

        let views: Vec<CullView> = std::iter::once(CullView {
            matrix: camera.to_cols_array_2d(),
            params: [0.0; 4],
        })
        .chain(shadows.iter().map(|shadow| CullView {
            matrix: shadow.matrix.to_cols_array_2d(),
            params: [shadow.resolution as f32, min_shadow_texels, 0.0, 0.0],
        }))
        .collect();

        let corner = |v: kmath::Vec3, flag: f32| [v.x, v.y, v.z, flag];
        let bounds: Vec<ObjectBounds> = frame
            .objects
            .iter()
            .zip(&frame.bounds)
            .zip(&frame.local_bounds)
            .map(|((object, world), local)| ObjectBounds {
                model: object.model,
                local_min: corner(local.min, if local.is_empty() { 1.0 } else { 0.0 }),
                local_max: corner(local.max, 0.0),
                world_min: corner(world.min, if world.is_empty() { 0.0 } else { 1.0 }),
                world_max: corner(world.max, 0.0),
            })
            .collect();

        let mut args = Vec::with_capacity(views.len() * table.len());
        for view in 0..views.len() as u32 {
            for (batch, range) in table.iter().zip(&ranges) {
                let ordered = view == 0 && batch.flags & CullBatch::ORDERED != 0;
                args.push(DrawArgs {
                    index_count: range.1,
                    instance_count: if ordered { batch.count } else { 0 },
                    first_index: range.0,
                    base_vertex: 0,
                    first_instance: view * slot_count + batch.first,
                });
            }
        }

        let input = CullInput {
            slots: &frame.slots,
            slot_batch: &slot_batch,
            batches: &table,
            views: &views,
            bounds: &bounds,
            args: &args,
        };
        let cull = self.gpu_cull.as_mut()?;
        cull.upload(&self.device, &self.queue, &input);
        self.cull_active = true;
        Some(views.len() as u32)
    }

    /// 读回上一帧剔除完的间接参数（同步等 GPU，测试用）：`[视图][批次]` 的实例数。
    #[cfg(test)]
    pub(crate) fn read_cull_counts(&self) -> Option<Vec<Vec<u32>>> {
        let cull = self.gpu_cull.as_ref().filter(|_| self.cull_active)?;
        let (_, batches, views) = cull.counts;
        let size = (batches * views) as u64 * size_of::<DrawArgs>() as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine cull readback"),
            size: size.max(4),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&cull.args.buffer, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        let data = slice.get_mapped_range().ok()?;
        let args: &[DrawArgs] = bytemuck::cast_slice(&data[..size as usize]);
        Some(
            args.chunks(batches.max(1) as usize)
                .map(|view| view.iter().map(|a| a.instance_count).collect())
                .collect(),
        )
    }

    /// 这一批在第 `view` 个视图里的间接参数；没开 GPU 剔除或这批没进表时为 `None`（直接绘制）。
    pub(crate) fn indirect_args(
        &self,
        batch: &crate::Batch,
        view: u32,
    ) -> Option<(&wgpu::Buffer, u64)> {
        if !self.cull_active || batch.cull == NO_BATCH {
            return None;
        }
        self.gpu_cull
            .as_ref()
            .map(|cull| cull.args_for(view, batch.cull))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cull_shader_passes_validation() {
        kshader::Shader::from_wgsl(include_str!("cull.wgsl")).expect("剔除着色器应当通过校验");
    }

    /// CPU / WGSL 结构体大小对不上不报错，只出垃圾。
    #[test]
    fn struct_sizes_match_the_shader() {
        assert_eq!(size_of::<CullBatch>(), 16);
        assert_eq!(size_of::<CullView>(), 80);
        assert_eq!(size_of::<ObjectBounds>(), 128);
        assert_eq!(size_of::<DrawArgs>(), 20);
        assert_eq!(size_of::<CullParams>(), 16);
        assert_eq!(size_of::<kscene::Instance>(), 96);
    }
}
