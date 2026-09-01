//! 计算着色器：让 GPU 干渲染以外的活。
//!
//! 粒子模拟、图像处理、剔除、物理求解——这些都是「同一段代码跑几万遍、
//! 每遍处理一小块数据」，正是 GPU 擅长而 CPU 吃力的形状。
//!
//! # 三样东西
//!
//! | | 是什么 |
//! |---|---|
//! | [`ComputePipeline`] | 一段编译好的计算着色器 |
//! | [`StorageBuffer`] | GPU 上一块可读写的数据 |
//! | [`ComputeContext::dispatch`] | 派 GPU 跑一遍 |
//!
//! ```ignore
//! let gpu = ComputeContext::from_renderer(&renderer);
//!
//! let pipeline = gpu.create_pipeline(&shader)?;
//! let buffer = gpu.create_buffer("data", bytemuck::cast_slice(&values));
//!
//! gpu.dispatch(&pipeline, &[&buffer], [values.len().div_ceil(64) as u32, 1, 1]);
//!
//! let result = gpu.read(&buffer)?;
//! ```
//!
//! # 从哪儿拿设备
//!
//! 两条路，区别只有一个但很要紧：**缓冲能不能直接给渲染用**。
//!
//! - [`from_renderer`](ComputeContext::from_renderer)：和渲染器**共用**同一台
//!   设备。算出来的东西可以直接拿去画（将来的 GPU 粒子走这条）。
//! - [`headless`](ComputeContext::headless)：自己开一台，不需要窗口。
//!   命令行工具、烘焙、测试用。它的缓冲和渲染器那边**互不相通**——
//!   两台设备之间没有共享内存这回事。
//!
//! # 绑定的约定
//!
//! 所有缓冲绑在 **`@group(0)`**，`@binding` 按传进 [`dispatch`](ComputeContext::dispatch)
//! 的**顺序**取 0、1、2……
//!
//! ```wgsl
//! @group(0) @binding(0) var<storage, read_write> data: array<f32>;
//!
//! @compute @workgroup_size(64)
//! fn main(@builtin(global_invocation_id) id: vec3<u32>) {
//!     data[id.x] = data[id.x] * 2.0;
//! }
//! ```
//!
//! 布局是 wgpu 从着色器**推导**出来的，不用手写——代价是这套接口只支持
//! 单个绑定组。要 uniform、贴图、多组绑定的话得另开一条更啰嗦的路，
//! 目前没有那个需求。
//!
//! # 读回是同步的，而且很慢
//!
//! [`read`](ComputeContext::read) 会**等 GPU 干完**再把数据搬回内存。
//! 一次往返轻则几毫秒，用在每帧的热路径上会把帧率拖垮。
//!
//! 它的正当用途是：调试、烘焙、以及把计算结果交给游戏逻辑（比如 GPU 剔除
//! 出来的可见列表）。**纯粹在 GPU 上流转的数据不该读回来**——粒子算完直接
//! 拿去画就行了。

use crate::Renderer;
use kshader::{Shader, ShaderStage};

/// 建计算管线时出的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeError {
    /// 着色器里没有 `@compute` 入口。
    NoComputeEntry,
    /// 着色器源码没通过校验。
    Invalid(String),
}

impl std::fmt::Display for ComputeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoComputeEntry => {
                write!(f, "着色器里没有 @compute 入口点")
            }
            Self::Invalid(message) => write!(f, "计算着色器无效：{message}"),
        }
    }
}

impl std::error::Error for ComputeError {}

/// 跑计算着色器要用的一台设备。
///
/// 见模块文档里「从哪儿拿设备」那一节。
#[derive(Debug, Clone)]
pub struct ComputeContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// 一段编译好的计算着色器。
///
/// 建一次反复用——建管线要编译着色器，每帧建一条会明显拖慢。
#[derive(Debug)]
pub struct ComputePipeline {
    pipeline: wgpu::ComputePipeline,
    /// 从着色器推导出来的 `@group(0)` 布局。
    layout: wgpu::BindGroupLayout,
}

/// GPU 上一块可读写的数据。
#[derive(Debug)]
pub struct StorageBuffer {
    buffer: wgpu::Buffer,
    size: u64,
}

impl StorageBuffer {
    /// 字节数。
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl ComputeContext {
    /// 和渲染器共用同一台设备。
    ///
    /// 算出来的缓冲可以直接被渲染管线拿去用——这是它和
    /// [`headless`](Self::headless) 的唯一区别，但很要紧。
    pub fn from_renderer(renderer: &Renderer) -> Self {
        // wgpu 的 `Device` / `Queue` 内部是 `Arc`，克隆只是加一次引用计数，
        // 不是真的开第二台设备。
        Self {
            device: renderer.device.clone(),
            queue: renderer.queue.clone(),
        }
    }

    /// 自己开一台不需要窗口的设备。
    ///
    /// 机器上没有可用的 GPU 适配器时返回 [`None`]——无头 CI、纯软件渲染
    /// 环境都可能这样，那时调用方通常该跳过而不是崩。
    pub fn headless() -> Option<Self> {
        pollster::block_on(Self::headless_async())
    }

    /// [`headless`](Self::headless) 的异步版本。
    pub async fn headless_async() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kengine compute device"),
                ..Default::default()
            })
            .await
            .ok()?;

        Some(Self { device, queue })
    }

    /// 编译一条计算管线。
    ///
    /// 入口点从着色器里反射，所以 WGSL 那边叫什么名字都行。
    pub fn create_pipeline(&self, shader: &Shader) -> Result<ComputePipeline, ComputeError> {
        let entry = shader
            .entry_point(ShaderStage::Compute)
            .ok_or(ComputeError::NoComputeEntry)?
            .to_string();

        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kengine compute shader"),
                source: wgpu::ShaderSource::Wgsl(shader.source().into()),
            });

        // 着色器上设的 `override` 取值。计算着色器尤其用得上——
        // `@workgroup_size` 可以直接写成 `override`，一份源码适配不同硬件。
        let constants = shader.constant_overrides();

        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("kengine compute pipeline"),
                // `None` 让 wgpu 从着色器推导布局。手写一份的话，
                // 着色器改一个绑定就要同步改这里，而对不上时的报错
                // 很难对应回源码。
                layout: None,
                module: &module,
                entry_point: Some(&entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                cache: None,
            });

        let layout = pipeline.get_bind_group_layout(0);
        Ok(ComputePipeline { pipeline, layout })
    }

    /// 用现成的数据建一块 storage buffer。
    pub fn create_buffer(&self, label: &str, contents: &[u8]) -> StorageBuffer {
        use wgpu::util::DeviceExt;

        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                // 三个用途缺一不可：STORAGE 给着色器读写，
                // COPY_SRC 给读回，COPY_DST 给 `write_buffer` 覆写。
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            });

        StorageBuffer {
            size: contents.len() as u64,
            buffer,
        }
    }

    /// 建一块清零的 storage buffer。
    ///
    /// 用来接计算结果——不必先在 CPU 上准备一份同样大的零数组。
    pub fn create_buffer_zeroed(&self, label: &str, size: u64) -> StorageBuffer {
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        StorageBuffer { buffer, size }
    }

    /// 覆写一块 storage buffer 的内容。
    ///
    /// 数据比缓冲大时什么都不做——截断着写会得到一份半新半旧的数据，
    /// 那种错在结果里几乎看不出来。
    pub fn write(&self, buffer: &StorageBuffer, contents: &[u8]) -> bool {
        if contents.len() as u64 > buffer.size {
            klog::error!(
                "写入 {} 字节，但缓冲只有 {} 字节",
                contents.len(),
                buffer.size
            );
            return false;
        }
        self.queue.write_buffer(&buffer.buffer, 0, contents);
        true
    }

    /// 派 GPU 跑一遍。
    ///
    /// `bindings` 里的缓冲按顺序绑到 `@group(0)` 的 `@binding(0)`、`(1)`……
    ///
    /// `workgroups` 是**工作组数**，不是线程数：着色器里
    /// `@workgroup_size(64)` 配 `[10, 1, 1]` 会跑 640 个线程。这两个数
    /// 分开的理由是硬件——一个工作组内的线程共享缓存、能互相同步，
    /// 跨组则不能。
    ///
    /// 提交之后**不等它跑完**。要拿结果就去
    /// [`read`](Self::read)，那个会等。
    pub fn dispatch(
        &self,
        pipeline: &ComputePipeline,
        bindings: &[&StorageBuffer],
        workgroups: [u32; 3],
    ) {
        // 三个维度里有 0 的话一个线程都不会跑。这多半是「元素个数除以
        // 工作组大小」时忘了向上取整，静默什么都不做最难查。
        if workgroups.iter().any(|&n| n == 0) {
            klog::warn!("dispatch 的工作组数含 0：{workgroups:?}，什么都不会执行");
            return;
        }

        let entries: Vec<wgpu::BindGroupEntry> = bindings
            .iter()
            .enumerate()
            .map(|(index, buffer)| wgpu::BindGroupEntry {
                binding: index as u32,
                resource: buffer.buffer.as_entire_binding(),
            })
            .collect();

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine compute bindings"),
            layout: &pipeline.layout,
            entries: &entries,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kengine compute encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("kengine compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// 把一块 storage buffer 读回内存。**会等 GPU 干完**。
    ///
    /// 见模块文档：一次往返轻则几毫秒，别放在每帧的热路径上。
    ///
    /// 映射失败时返回 [`None`]——设备丢失、缓冲已被销毁都会走到这里。
    pub fn read(&self, buffer: &StorageBuffer) -> Option<Vec<u8>> {
        if buffer.size == 0 {
            return Some(Vec::new());
        }

        // storage buffer 本身不能直接映射到内存（那两个用途在多数后端上
        // 互斥），所以要先拷进一块专门用来映射的缓冲。
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine readback staging"),
            size: buffer.size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kengine readback encoder"),
            });
        encoder.copy_buffer_to_buffer(&buffer.buffer, 0, &staging, 0, buffer.size);
        self.queue.submit(Some(encoder.finish()));

        let (sender, receiver) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // 发送失败说明接收端已经没了，那时这个结果也没人要了。
                let _ = sender.send(result);
            });

        // 必须 poll 到底：映射的回调是在 poll 里跑的，不 poll 就永远等不到。
        if self.device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
            klog::error!("等待 GPU 时设备出错");
            return None;
        }

        match receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                klog::error!("缓冲映射失败：{error}");
                return None;
            }
            Err(_) => {
                klog::error!("缓冲映射的回调没有到达");
                return None;
            }
        }

        let data = match staging.slice(..).get_mapped_range() {
            Ok(view) => view.to_vec(),
            Err(error) => {
                klog::error!("取映射区间失败：{error}");
                staging.unmap();
                return None;
            }
        };
        // 解映射之后 staging 会被丢弃。不解的话 wgpu 会在销毁时报错。
        staging.unmap();
        Some(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一台不要窗口的渲染设备。
    ///
    /// 拿不到就返回 [`None`]——CI 上通常没有 GPU，那种环境下这些测试
    /// 应当跳过而不是红。本地开发一定会真的跑到。
    fn headless() -> Option<ComputeContext> {
        ComputeContext::headless()
    }

    /// 把字节按 `f32` 解出来。
    fn as_floats(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    const DOUBLE: &str = r#"
        @group(0) @binding(0) var<storage, read_write> data: array<f32>;

        @compute @workgroup_size(64)
        fn main(@builtin(global_invocation_id) id: vec3<u32>) {
            if (id.x >= arrayLength(&data)) { return; }
            data[id.x] = data[id.x] * 2.0;
        }
    "#;

    #[test]
    fn a_compute_shader_actually_computes() {
        // 这条是整个模块的验收：数据进 GPU、算一遍、读回来，结果对得上。
        // 光看「管线建出来了」说明不了任何事。
        let Some(renderer) = headless() else {
            return;
        };

        let input: Vec<f32> = (0..256).map(|i| i as f32).collect();
        let shader = Shader::from_wgsl(DOUBLE).expect("着色器该通过校验");
        let pipeline = renderer.create_pipeline(&shader).expect("管线该建得出来");

        let buffer = renderer.create_buffer("input", bytemuck::cast_slice(&input));
        renderer.dispatch(&pipeline, &[&buffer], [4, 1, 1]);

        let output = as_floats(&renderer.read(&buffer).expect("该读得回来"));

        assert_eq!(output.len(), input.len());
        for (index, (before, after)) in input.iter().zip(&output).enumerate() {
            assert_eq!(*after, before * 2.0, "第 {index} 个元素算错了");
        }
    }

    #[test]
    fn a_zeroed_buffer_starts_empty_and_can_be_filled() {
        let Some(renderer) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            @group(0) @binding(0) var<storage, read_write> data: array<u32>;

            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                if (id.x >= arrayLength(&data)) { return; }
                data[id.x] = id.x;
            }
            "#,
        )
        .unwrap();
        let pipeline = renderer.create_pipeline(&shader).unwrap();

        let buffer = renderer.create_buffer_zeroed("out", 64 * 4);
        assert!(
            renderer.read(&buffer).unwrap().iter().all(|&b| b == 0),
            "新建的缓冲该是全零"
        );

        renderer.dispatch(&pipeline, &[&buffer], [1, 1, 1]);

        let output = renderer.read(&buffer).unwrap();
        let values: Vec<u32> = output
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        assert_eq!(values[0], 0);
        assert_eq!(values[63], 63, "每个线程该写自己那一格");
    }

    #[test]
    fn two_buffers_bind_in_order() {
        // 绑定顺序是这套接口唯一的约定，必须钉死。
        let Some(renderer) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            // 注意 `target` 是 WGSL 的保留字，不能拿来当变量名——
            // kshader 的预校验会在加载期就拦下，不必等到 GPU 编译。
            @group(0) @binding(0) var<storage, read> source: array<f32>;
            @group(0) @binding(1) var<storage, read_write> sink: array<f32>;

            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                if (id.x >= arrayLength(&sink)) { return; }
                sink[id.x] = source[id.x] + 100.0;
            }
            "#,
        )
        .unwrap();
        let pipeline = renderer.create_pipeline(&shader).unwrap();

        let source: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let input = renderer.create_buffer("source", bytemuck::cast_slice(&source));
        let output = renderer.create_buffer_zeroed("sink", 64 * 4);

        renderer.dispatch(&pipeline, &[&input, &output], [1, 1, 1]);

        let result = as_floats(&renderer.read(&output).unwrap());
        assert_eq!(result[0], 100.0);
        assert_eq!(result[63], 163.0);
    }

    #[test]
    fn writing_a_buffer_replaces_its_contents() {
        let Some(renderer) = headless() else {
            return;
        };

        let buffer = renderer.create_buffer("data", &[1u8, 2, 3, 4]);
        assert!(renderer.write(&buffer, &[9u8, 9, 9, 9]));

        assert_eq!(renderer.read(&buffer).unwrap(), vec![9, 9, 9, 9]);
    }

    #[test]
    fn writing_more_than_fits_is_refused() {
        // 截断着写会得到一份半新半旧的数据，那种错在结果里几乎看不出来。
        let Some(renderer) = headless() else {
            return;
        };

        let buffer = renderer.create_buffer("data", &[0u8; 4]);
        assert!(!renderer.write(&buffer, &[1u8; 8]));

        assert_eq!(renderer.read(&buffer).unwrap(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_shader_without_a_compute_entry_is_refused() {
        let Some(renderer) = headless() else {
            return;
        };

        // 一份只有顶点/片元入口的着色器。
        let shader = Shader::from_wgsl(
            r#"
            @vertex fn vs() -> @builtin(position) vec4<f32> {
                return vec4<f32>(0.0, 0.0, 0.0, 1.0);
            }
            @fragment fn fs() -> @location(0) vec4<f32> {
                return vec4<f32>(1.0);
            }
            "#,
        )
        .unwrap();

        assert_eq!(
            renderer.create_pipeline(&shader).err(),
            Some(ComputeError::NoComputeEntry)
        );
    }

    #[test]
    fn a_zero_workgroup_dispatch_does_nothing_instead_of_hanging() {
        // 「元素个数除以工作组大小」忘了向上取整时会得到 0，
        // 静默什么都不做是最难查的那种。这里至少留一条日志。
        let Some(renderer) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(DOUBLE).unwrap();
        let pipeline = renderer.create_pipeline(&shader).unwrap();
        let input: Vec<f32> = vec![1.0; 64];
        let buffer = renderer.create_buffer("data", bytemuck::cast_slice(&input));

        renderer.dispatch(&pipeline, &[&buffer], [0, 1, 1]);

        let output = as_floats(&renderer.read(&buffer).unwrap());
        assert!(output.iter().all(|&v| v == 1.0), "不该有任何元素被改动");
    }
}
