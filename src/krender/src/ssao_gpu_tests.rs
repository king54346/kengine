//! SSAO 的**数学**验证：喂一组人造的深度／法线，读回遮蔽图。
//!
//! 这一层必须真跑一遍。SSAO 是「装了但不生效」的重灾区——半球方向反了、
//! 深度反解错了、范围检查写反了，任何一条都会让整张图恒等于 1
//! （完全没效果）或者恒等于 0（全黑），而且**一句错都不报**。
//!
//! 不建整个 [`Ssao`](super::Ssao)：那要 `Globals` / `ObjectUniforms`
//! 两个布局，那是预通道的事，和这里要验的数学无关。所以
//! `create_ssao_layout` / `create_ssao_pipeline` 被抽成了自由函数。

use super::*;

/// 画布边长。64 × 4 = 256 字节一行，正好是 GPU 的行对齐要求，
/// 读回时不用抠填充。
const SIZE: u32 = 64;

/// 共用整个测试进程那一台设备。每条测试各开一台的话，
/// 并发析构会间歇性地把进程带走——理由见
/// [`ComputeContext::shared`](crate::ComputeContext)。
fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
    let shared = crate::ComputeContext::shared_headless()?;
    Some((shared.device().clone(), shared.queue().clone()))
}

/// 一个正交相机，正对 -Z 看一个 `[-1,1]²` 的平面。
///
/// 用正交而不是透视：正交下「深度 → 世界坐标」是线性的，人造数据好构造。
/// 这里要验的是遮蔽本身，不是投影。
fn camera() -> (Mat4, Vec3) {
    let eye = Vec3::new(0.0, 0.0, 3.0);
    let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
    let projection = Mat4::orthographic_rh(-1.0, 1.0, -1.0, 1.0, 0.1, 10.0);
    (projection * view, eye)
}

/// 造一张深度图和一张法线图，跑一遍 SSAO，读回遮蔽值。
///
/// `depth_at` 给每个像素的深度（0 = 近平面，1 = 远平面），
/// `normal_at` 给世界法线。没有可用显卡时返回 [`None`]，调用方跳过。
fn run(
    settings: SsaoSettings,
    depth_at: impl Fn(u32, u32) -> f32,
    normal_at: impl Fn(u32, u32) -> [f32; 3],
) -> Option<Vec<f32>> {
    let (device, queue) = headless()?;

    let depth_data: Vec<f32> = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| depth_at(x, y))
        .collect();
    let normal_data: Vec<[f32; 4]> = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| {
            let n = normal_at(x, y);
            [n[0], n[1], n[2], 1.0]
        })
        .collect();

    let depth_view = write_depth(&device, &queue, &depth_data);
    let normal_view = write_normal(&device, &queue, &normal_data);

    let layout = create_ssao_layout(&device);
    let pipeline = create_ssao_pipeline(&device, &layout);
    let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: size_of::<SsaoParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let (view_proj, camera_position) = camera();
    let size = SIZE as f32;
    queue.write_buffer(
        &params_buffer,
        0,
        bytemuck::bytes_of(&SsaoParams {
            inverse_view_proj: view_proj.inverse().to_cols_array_2d(),
            view_proj: view_proj.to_cols_array_2d(),
            camera_position: camera_position.extend(1.0).to_array(),
            settings: [
                settings.radius,
                settings.strength,
                settings.samples.clamp(1, 16) as f32,
                settings.bias,
            ],
            texel: [1.0 / size, 1.0 / size, size, size],
        }),
    );

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&normal_view),
            },
        ],
    });

    let occlusion = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OCCLUSION_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let occlusion_view = occlusion.create_view(&Default::default());

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &occlusion_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (SIZE * SIZE * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &occlusion,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 4),
                rows_per_image: Some(SIZE),
            },
        },
        extent(),
    );
    queue.submit(Some(encoder.finish()));

    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = sender.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    receiver.recv().ok()?.ok()?;
    let bytes = staging.slice(..).get_mapped_range().ok()?.to_vec();
    staging.unmap();

    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

fn extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: SIZE,
        height: SIZE,
        depth_or_array_layers: 1,
    }
}

/// 把一组深度值画进一张深度纹理。
///
/// 深度格式**不能** `write_texture`，只能画进去。所以先把数据放进一张
/// `R32Float`，再用一个全屏三角形逐像素采它、写 `frag_depth`。
fn write_depth(device: &wgpu::Device, queue: &wgpu::Queue, data: &[f32]) -> wgpu::TextureView {
    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(data),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        extent(),
    );

    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&Default::default());

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(DEPTH_BLIT_WGSL.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&source.create_view(&Default::default())),
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Option::from(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
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
            targets: &[],
        }),
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Option::from(true),
            // `Always`：这一趟是在**灌数据**，不是在画场景。
            depth_compare: Option::from(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
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
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));

    depth_view
}

const DEPTH_BLIT_WGSL: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u) * 2.0 - 1.0;
    let y = 1.0 - f32(index & 2u) * 2.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) position: vec4<f32>) -> @builtin(frag_depth) f32 {
    return textureLoad(source, vec2<i32>(position.xy), 0).r;
}
"#;

/// 把一组法线写进一张 `Rgba16Float` 纹理。
fn write_normal(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    data: &[[f32; 4]],
) -> wgpu::TextureView {
    let halves: Vec<u16> = data
        .iter()
        .flat_map(|p| p.iter().map(|v| half_from_f32(*v)))
        .collect();

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: NORMAL_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&halves),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 8),
            rows_per_image: Some(SIZE),
        },
        extent(),
    );
    texture.create_view(&Default::default())
}

/// `f32` → IEEE 半精度。只求够用：法线在 `[-1,1]` 里，不会溢出。
fn half_from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = ((bits >> 13) & 0x3ff) as u16;
    if exponent <= 0 {
        return sign;
    }
    sign | ((exponent as u16) << 10) | mantissa
}

/// 一面正对相机的墙：整张图同一个深度，法线朝 +Z。
fn flat_wall() -> (impl Fn(u32, u32) -> f32, impl Fn(u32, u32) -> [f32; 3]) {
    (|_, _| 0.5, |_, _| [0.0, 0.0, 1.0])
}

/// 左半边凸出来、右半边凹进去的一级台阶。
fn step_edge() -> (impl Fn(u32, u32) -> f32, impl Fn(u32, u32) -> [f32; 3]) {
    (
        |x: u32, _| if x < SIZE / 2 { 0.40 } else { 0.55 },
        |_, _| [0.0, 0.0, 1.0],
    )
}

fn on() -> SsaoSettings {
    SsaoSettings {
        enabled: true,
        ..Default::default()
    }
}

#[test]
fn a_flat_wall_is_not_occluded() {
    // 最要紧的一条。平坦表面必须**几乎不遮**——半球方向里混进朝向表面
    // 内侧的、或者偏移给小了，整个画面会浮起一层均匀的灰，
    // 而且看着还挺「有效果」。
    let (depth, normal) = flat_wall();
    let Some(values) = run(on(), depth, normal) else {
        return;
    };

    let center = values[(SIZE / 2 * SIZE + SIZE / 2) as usize];
    assert!(
        center > 0.9,
        "平坦的墙被判成遮蔽了 {:.3}（返回值 1 = 完全不遮）",
        1.0 - center
    );
}

#[test]
fn the_sky_is_never_occluded() {
    // 深度是清除值的地方没有几何。不特判的话反解出来的位置在无穷远，
    // 采样全落空，结果是一片随机噪声。
    let Some(values) = run(on(), |_, _| 1.0, |_, _| [0.0, 0.0, 1.0]) else {
        return;
    };

    assert!(
        values.iter().all(|v| *v >= 0.999),
        "天空处出现了遮蔽，最暗 {:.3}",
        values.iter().cloned().fold(1.0f32, f32::min)
    );
}

#[test]
fn a_step_casts_occlusion_on_the_lower_side() {
    // 真正要验的：有高低差的地方，**凹的那一侧**变暗。
    let (depth, normal) = step_edge();
    let Some(values) = run(
        SsaoSettings {
            radius: 0.6,
            ..on()
        },
        depth,
        normal,
    ) else {
        return;
    };

    let at = |x: u32, y: u32| values[(y * SIZE + x) as usize];
    let row = SIZE / 2;
    let near_step = at(SIZE / 2 + 2, row);
    let far_from_step = at(SIZE - 3, row);

    assert!(
        near_step < far_from_step - 0.02,
        "台阶旁边没有变暗：贴着 {near_step:.3}，远处 {far_from_step:.3}"
    );
}

#[test]
fn strength_zero_disables_the_effect() {
    // 强度是**乘**在遮蔽量上的。写成加法或者位置放错的话，调到 0 也关不掉
    // ——那种「关不掉的效果」很难查。
    let (depth, normal) = step_edge();
    let Some(values) = run(
        SsaoSettings {
            strength: 0.0,
            radius: 0.6,
            ..on()
        },
        depth,
        normal,
    ) else {
        return;
    };

    assert!(
        values.iter().all(|v| *v >= 0.999),
        "强度为 0 时还有遮蔽，最暗 {:.3}",
        values.iter().cloned().fold(1.0f32, f32::min)
    );
}

#[test]
fn stronger_settings_darken_more() {
    // 单调性。参数调大反而变亮的话，说明某处符号反了。
    let sample = |strength: f32| -> Option<f32> {
        let (depth, normal) = step_edge();
        let values = run(
            SsaoSettings {
                strength,
                radius: 0.6,
                ..on()
            },
            depth,
            normal,
        )?;
        Some(values[(SIZE / 2 * SIZE + SIZE / 2 + 2) as usize])
    };

    let (Some(weak), Some(strong)) = (sample(0.5), sample(2.0)) else {
        return;
    };
    assert!(
        strong < weak,
        "强度从 0.5 调到 2.0 反而变亮了：{weak:.3} → {strong:.3}"
    );
}
