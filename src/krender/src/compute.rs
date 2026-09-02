//! 计算着色器：让 GPU 干渲染以外的活。
//!
//! 粒子模拟、图像处理、剔除、物理求解——这些都是「同一段代码跑几万遍、
//! 每遍处理一小块数据」，正是 GPU 擅长而 CPU 吃力的形状。
//!
//! # 四样东西
//!
//! | | 是什么 |
//! |---|---|
//! | [`ComputePipeline`] | 一段编译好的计算着色器 |
//! | [`StorageBuffer`] | GPU 上一块可读写的数据，按一维下标寻址 |
//! | [`StorageTexture`] | GPU 上一张可写的纹理，按二维坐标寻址 |
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
//! 所有资源绑在 **`@group(0)`**，`@binding` 按传进 [`dispatch`](ComputeContext::dispatch)
//! 的**顺序**取 0、1、2……缓冲与纹理混着用时走
//! [`dispatch_with`](ComputeContext::dispatch_with)。
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
use kcore::uuid::Uuid;
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
    /// 同一个 crate 里的渲染代码要拿它去建绑定组（GPU 粒子那条路）。
    pub(crate) buffer: wgpu::Buffer,
    size: u64,
    /// 缓存绑定组用的键。
    ///
    /// 建绑定组不便宜，而同一块缓冲每帧都会被交过来一次，
    /// 所以渲染器按这个 id 缓存。wgpu 的 `Buffer` 自己没有稳定可比的
    /// 标识，只能自己发一个。
    id: Uuid,
}

impl StorageBuffer {
    /// 字节数。
    pub fn size(&self) -> u64 {
        self.size
    }

    /// 这块缓冲的标识。渲染器按它缓存绑定组。
    pub fn id(&self) -> Uuid {
        self.id
    }
}

/// 存储纹理的像素格式。
///
/// 只列了三种，因为**能当存储纹理写的格式是有限的**：不是所有格式都
/// 支持 `textureStore`，而这三种在所有后端上都保证可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageFormat {
    /// 每像素一个 `u32`。计数、标号、生命游戏的格子这类整数数据用它。
    R32Uint,
    /// 每像素 4 个 `f32`。要精度就用它，代价是 16 字节一像素。
    Rgba32Float,
    /// 每像素 4 个字节，归一化到 `[0, 1]`。就是普通的 RGBA 图。
    ///
    /// 注意是**线性**而非 sRGB：sRGB 格式不能当存储纹理写。
    Rgba8Unorm,
}

impl StorageFormat {
    /// 对应的 wgpu 格式。
    fn to_wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::R32Uint => wgpu::TextureFormat::R32Uint,
            Self::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        }
    }

    /// 一个像素占几个字节。
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::R32Uint => 4,
            Self::Rgba32Float => 16,
            Self::Rgba8Unorm => 4,
        }
    }

    /// WGSL 里写这个格式时用的名字。
    ///
    /// 着色器侧要写成 `texture_storage_2d<r32uint, write>`，名字必须一致。
    pub fn wgsl_name(self) -> &'static str {
        match self {
            Self::R32Uint => "r32uint",
            Self::Rgba32Float => "rgba32float",
            Self::Rgba8Unorm => "rgba8unorm",
        }
    }
}

/// GPU 上一张可写的纹理。
///
/// 和 [`StorageBuffer`] 的区别是**寻址方式**：缓冲按一维下标，纹理按
/// 二维坐标。图像处理、格子模拟这类天然是二维的活儿用纹理，代码里
/// 不必反复算 `y * width + x`，而且硬件的缓存局部性也按二维来。
#[derive(Debug)]
pub struct StorageTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: StorageFormat,
}

impl StorageTexture {
    /// 宽度（像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 高度（像素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 像素格式。
    pub fn format(&self) -> StorageFormat {
        self.format
    }
}

/// 一条 dispatch 要绑的一样东西。
///
/// 缓冲和纹理混着绑时用它；全是缓冲的话
/// [`dispatch`](ComputeContext::dispatch) 更省事。
#[derive(Debug, Clone, Copy)]
pub enum Binding<'a> {
    /// 一块 storage buffer。
    Buffer(&'a StorageBuffer),
    /// 一张 storage texture。
    Texture(&'a StorageTexture),
}

impl<'a> From<&'a StorageBuffer> for Binding<'a> {
    fn from(buffer: &'a StorageBuffer) -> Self {
        Self::Buffer(buffer)
    }
}

impl<'a> From<&'a StorageTexture> for Binding<'a> {
    fn from(texture: &'a StorageTexture) -> Self {
        Self::Texture(texture)
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
            id: Uuid::new_v4(),
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

        StorageBuffer {
            buffer,
            size,
            id: Uuid::new_v4(),
        }
    }

    /// 建一张清零的 storage texture。
    ///
    /// 着色器侧的声明必须和 `format` 对得上：
    ///
    /// ```wgsl
    /// @group(0) @binding(0) var image: texture_storage_2d<rgba8unorm, write>;
    /// ```
    ///
    /// 名字见 [`StorageFormat::wgsl_name`]。写错的话 wgpu 会在建绑定组时
    /// 报「格式不匹配」。
    ///
    /// # 为什么只能写不能读
    ///
    /// 同一张纹理既读又写（`read_write`）在 WebGPU 里是要单独开特性的，
    /// 而且不是所有后端都支持。真要「读上一代、写下一代」，就像
    /// `compute_game_of_life` 那样开两张轮换——那本来也是更正确的做法，
    /// 同一张纹理边读边写会让结果取决于线程调度顺序。
    ///
    /// 宽高会被夹到至少 1：零尺寸的纹理建不出来。
    pub fn create_storage_texture(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: StorageFormat,
    ) -> StorageTexture {
        let (width, height) = (width.max(1), height.max(1));

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.to_wgpu(),
            // STORAGE_BINDING 给着色器写，COPY_SRC 给读回，
            // COPY_DST 让它能被清零和被上传。
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        StorageTexture {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            texture,
            width,
            height,
            format,
        }
    }

    /// 把一张 storage texture 读回内存，逐行紧密排列（每行 `width * 像素字节`）。
    ///
    /// 和 [`read`](Self::read) 一样**会等 GPU 干完**，同样别放在热路径上。
    ///
    /// # 行对齐这件事
    ///
    /// GPU 拷纹理到缓冲时，每行的起始偏移必须是 256 字节的倍数。所以这里
    /// 先按对齐后的行宽拷进 staging，再把每行的有效部分抠出来紧密排好。
    /// 不做这一步的话，宽度不是 64 像素倍数的图读回来会**逐行错位**——
    /// 画面上看着像斜切，很容易误判成着色器写错了坐标。
    pub fn read_texture(&self, texture: &StorageTexture) -> Option<Vec<u8>> {
        let bytes_per_pixel = texture.format.bytes_per_pixel();
        let unpadded_row = texture.width * bytes_per_pixel;
        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(ALIGN) * ALIGN;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine texture readback staging"),
            size: (padded_row * texture.height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kengine texture readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(texture.height),
                },
            },
            wgpu::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let padded = self.map_and_read(&staging)?;

        // 把每行的有效部分抠出来，丢掉行尾的填充。
        let mut data = Vec::with_capacity((unpadded_row * texture.height) as usize);
        for row in 0..texture.height {
            let start = (row * padded_row) as usize;
            data.extend_from_slice(&padded[start..start + unpadded_row as usize]);
        }
        Some(data)
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
        let bindings: Vec<Binding<'_>> = bindings.iter().map(|b| Binding::Buffer(b)).collect();
        self.dispatch_with(pipeline, &bindings, workgroups);
    }

    /// 派 GPU 跑一遍，绑定可以是缓冲也可以是纹理。
    ///
    /// 除了绑定的种类，和 [`dispatch`](Self::dispatch) 完全一样。
    ///
    /// ```ignore
    /// gpu.dispatch_with(
    ///     &pipeline,
    ///     &[Binding::Buffer(&counts), Binding::Texture(&image)],
    ///     [width.div_ceil(8), height.div_ceil(8), 1],
    /// );
    /// ```
    pub fn dispatch_with(
        &self,
        pipeline: &ComputePipeline,
        bindings: &[Binding<'_>],
        workgroups: [u32; 3],
    ) {
        // 三个维度里有 0 的话一个线程都不会跑。这多半是「元素个数除以
        // 工作组大小」时忘了向上取整，静默什么都不做最难查。
        if workgroups.contains(&0) {
            klog::warn!("dispatch 的工作组数含 0：{workgroups:?}，什么都不会执行");
            return;
        }

        let entries: Vec<wgpu::BindGroupEntry> = bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| wgpu::BindGroupEntry {
                binding: index as u32,
                resource: match binding {
                    Binding::Buffer(buffer) => buffer.buffer.as_entire_binding(),
                    Binding::Texture(texture) => wgpu::BindingResource::TextureView(&texture.view),
                },
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

        self.map_and_read(&staging)
    }

    /// 等 GPU 干完，把一块可映射的缓冲整个搬进内存。
    ///
    /// [`read`](Self::read) 和 [`read_texture`](Self::read_texture) 的公共尾巴。
    fn map_and_read(&self, staging: &wgpu::Buffer) -> Option<Vec<u8>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // 发送失败说明接收端已经没了，那时这个结果也没人要了。
                let _ = sender.send(result);
            });

        // 必须 poll 到底：映射的回调是在 poll 里跑的，不 poll 就永远等不到。
        if self
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
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

    // ── 存储纹理 ──

    #[test]
    fn a_compute_shader_can_write_a_storage_texture() {
        // 纹理这条路和缓冲那条是两套绑定类型，得单独验一遍真的算对了。
        let Some(gpu) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            @group(0) @binding(0) var image: texture_storage_2d<rgba8unorm, write>;

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                let size = textureDimensions(image);
                if (id.x >= size.x || id.y >= size.y) { return; }
                // 横向红、纵向绿，读回来一眼能看出有没有转置或错行。
                textureStore(image, vec2<i32>(id.xy), vec4<f32>(
                    f32(id.x) / f32(size.x - 1u),
                    f32(id.y) / f32(size.y - 1u),
                    0.0,
                    1.0,
                ));
            }
            "#,
        )
        .unwrap();
        let pipeline = gpu.create_pipeline(&shader).unwrap();

        let image = gpu.create_storage_texture("image", 16, 16, StorageFormat::Rgba8Unorm);
        gpu.dispatch_with(&pipeline, &[Binding::Texture(&image)], [2, 2, 1]);

        let pixels = gpu.read_texture(&image).expect("该读得回来");

        assert_eq!(pixels.len(), 16 * 16 * 4);
        let at = |x: usize, y: usize| &pixels[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert_eq!(at(0, 0), [0, 0, 0, 255]);
        assert_eq!(at(15, 0), [255, 0, 0, 255]);
        assert_eq!(at(0, 15), [0, 255, 0, 255]);
    }

    #[test]
    fn rows_come_back_tightly_packed_even_when_the_width_needs_padding() {
        // 一行 5 像素 × 4 字节 = 20 字节，而 GPU 要求每行按 256 字节对齐。
        // 不把填充抠掉的话，读回来的图会逐行错位——看着像斜切，
        // 很容易误判成着色器写错了坐标。
        let Some(gpu) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            @group(0) @binding(0) var image: texture_storage_2d<r32uint, write>;

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                let size = textureDimensions(image);
                if (id.x >= size.x || id.y >= size.y) { return; }
                textureStore(image, vec2<i32>(id.xy), vec4<u32>(id.y * size.x + id.x, 0u, 0u, 0u));
            }
            "#,
        )
        .unwrap();
        let pipeline = gpu.create_pipeline(&shader).unwrap();

        let image = gpu.create_storage_texture("counter", 5, 3, StorageFormat::R32Uint);
        gpu.dispatch_with(&pipeline, &[Binding::Texture(&image)], [1, 1, 1]);

        let bytes = gpu.read_texture(&image).unwrap();
        assert_eq!(bytes.len(), 5 * 3 * 4, "行填充没有被抠掉");

        let values: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(values, (0..15).collect::<Vec<u32>>());
    }

    #[test]
    fn buffers_and_textures_can_be_bound_side_by_side() {
        // 顺序就是 @binding 的号，混着绑时尤其要钉死。
        let Some(gpu) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            @group(0) @binding(0) var<storage, read> palette: array<f32>;
            @group(0) @binding(1) var image: texture_storage_2d<rgba8unorm, write>;

            @compute @workgroup_size(4, 4)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                let size = textureDimensions(image);
                if (id.x >= size.x || id.y >= size.y) { return; }
                textureStore(image, vec2<i32>(id.xy), vec4<f32>(palette[id.x], 0.0, 0.0, 1.0));
            }
            "#,
        )
        .unwrap();
        let pipeline = gpu.create_pipeline(&shader).unwrap();

        let palette = gpu.create_buffer("palette", bytemuck::cast_slice(&[0.0f32, 1.0, 0.0, 1.0]));
        let image = gpu.create_storage_texture("image", 4, 4, StorageFormat::Rgba8Unorm);

        gpu.dispatch_with(
            &pipeline,
            &[Binding::Buffer(&palette), Binding::Texture(&image)],
            [1, 1, 1],
        );

        let pixels = gpu.read_texture(&image).unwrap();
        assert_eq!(&pixels[0..4], [0, 0, 0, 255]);
        assert_eq!(&pixels[4..8], [255, 0, 0, 255]);
    }

    #[test]
    fn a_fresh_storage_texture_starts_black() {
        let Some(gpu) = headless() else {
            return;
        };

        let image = gpu.create_storage_texture("blank", 4, 4, StorageFormat::Rgba8Unorm);
        assert!(gpu.read_texture(&image).unwrap().iter().all(|&b| b == 0));
    }

    #[test]
    fn a_zero_sized_texture_is_clamped_instead_of_failing() {
        // 尺寸从窗口大小算出来时，最小化的那一帧会是 0。
        let Some(gpu) = headless() else {
            return;
        };

        let image = gpu.create_storage_texture("tiny", 0, 0, StorageFormat::R32Uint);
        assert_eq!((image.width(), image.height()), (1, 1));
    }

    #[test]
    fn the_wgsl_format_names_match_what_the_shader_must_write() {
        // Rust 侧建纹理、WGSL 侧声明格式，两边对不上时 wgpu 报的是
        // 「绑定格式不匹配」，指不回这两个名字里的哪一个错了。
        for (format, name) in [
            (StorageFormat::R32Uint, "r32uint"),
            (StorageFormat::Rgba32Float, "rgba32float"),
            (StorageFormat::Rgba8Unorm, "rgba8unorm"),
        ] {
            assert_eq!(format.wgsl_name(), name);
        }
    }

    #[test]
    fn an_override_constant_actually_reaches_the_driver() {
        // 管线常量这条路唯一能自动验证的地方：算出来的数不一样。
        // 渲染管线走的是同一份 `PipelineCompilationOptions`，
        // 但那边只能靠肉眼看画面。
        let Some(gpu) = headless() else {
            return;
        };

        const SOURCE: &str = r#"
            override FACTOR: f32 = 1.0;

            @group(0) @binding(0) var<storage, read_write> data: array<f32>;

            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                if (id.x >= arrayLength(&data)) { return; }
                data[id.x] = data[id.x] * FACTOR;
            }
        "#;

        let run = |factor: Option<f64>| -> Vec<f32> {
            let mut shader = Shader::from_wgsl(SOURCE).unwrap();
            if let Some(factor) = factor {
                shader = shader.with_constant("FACTOR", factor);
            }
            let pipeline = gpu.create_pipeline(&shader).unwrap();
            let buffer = gpu.create_buffer("data", bytemuck::cast_slice(&[1.0f32; 64]));
            gpu.dispatch(&pipeline, &[&buffer], [1, 1, 1]);
            as_floats(&gpu.read(&buffer).unwrap())
        };

        assert_eq!(run(None)[0], 1.0, "没设常量时该用声明处的默认值");
        assert_eq!(run(Some(7.0))[0], 7.0, "设了常量却没生效");
        assert_eq!(run(Some(0.25))[0], 0.25);
    }

    #[test]
    fn a_constant_can_be_derived_from_another_constant() {
        // `override B = 1.0 / A;` 只覆盖 A，B 要在建管线时跟着算出来。
        // 这是 `override` 比 uniform 多出来的那点东西——编译期就定下了值。
        let Some(gpu) = headless() else {
            return;
        };

        let shader = Shader::from_wgsl(
            r#"
            override LEVELS: f32 = 4.0;
            override STEP: f32 = 1.0 / LEVELS;

            @group(0) @binding(0) var<storage, read_write> data: array<f32>;

            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                if (id.x >= arrayLength(&data)) { return; }
                data[id.x] = STEP;
            }
            "#,
        )
        .unwrap()
        .with_constant("LEVELS", 8.0);

        let pipeline = gpu.create_pipeline(&shader).unwrap();
        let buffer = gpu.create_buffer_zeroed("data", 64 * 4);
        gpu.dispatch(&pipeline, &[&buffer], [1, 1, 1]);

        assert_eq!(as_floats(&gpu.read(&buffer).unwrap())[0], 0.125);
    }
}
