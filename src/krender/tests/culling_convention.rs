//! 图元的三角形绕序和渲染器的背面剔除规则对不对得上。
//!
//! # 为什么要真的渲一遍
//!
//! 「CCW 是正面」这句话在纸上讨论不出结果：WebGPU 判定绕序用的是
//! **帧缓冲坐标**（y 向下），而投影矩阵里可能还有一次 y 翻转。两次翻转
//! 抵消与否，靠推理很容易得出相反的结论——本文件的存在就是因为
//! 推理失败过。
//!
//! 所以这里真的开一台无头设备、真的画一遍、真的把像素读回来。
//! 画不出来的那个面就是被剔掉了。
//!
//! # 症状长什么样
//!
//! 绕序反了的面**不会报任何错**，只是从外面看不见——于是闭合模型
//! 变成能看穿的空壳，而立方体这种每个面朝向都不同的图元会显示成
//! 「只剩几个面」。

use kcamera::Camera;
use kmath::{Mat4, Vec3};
use kmesh::Mesh;

/// 离屏画布的边长。只看正中心一个像素，不需要大。
const SIZE: u32 = 64;

/// 把一个网格从 `eye` 朝原点画一遍，返回正中心那个像素上画的是哪个面。
///
/// 返回值是那个面的**顶点法线**。背景清成透明黑，中心什么都没画到时
/// 返回 `Some(None)`。
fn face_at_center(mesh: &Mesh, eye: Vec3) -> Option<Option<Vec3>> {
    let (device, queue) = headless()?;

    // ── 顶点缓冲：位置 + 法线 ──
    //
    // 法线是必须的。只画个纯色的话，闭合网格从任何方向看中心像素都被
    // 盖住——正面被剔掉时，**背后那个面的内侧**顶上来了。
    // 把法线画成颜色才分得清画的是哪个面。
    let data: Vec<[f32; 3]> = mesh
        .vertices()
        .iter()
        .flat_map(|v| [v.position, v.normal])
        .collect();
    let vertices = create_buffer(
        &device,
        bytemuck::cast_slice(&data),
        wgpu::BufferUsages::VERTEX,
    );
    let indices = create_buffer(
        &device,
        bytemuck::cast_slice(mesh.indices()),
        wgpu::BufferUsages::INDEX,
    );

    // ── 相机：和引擎里用的是同一套 ──
    //
    // 直接手写一个矩阵是不行的：要验的正是「引擎那套矩阵配上引擎那套
    // 剔除设置会怎样」，任何一处换成自己写的都会让结论失效。
    let camera = Camera::perspective(45.0);
    let view_proj = camera.projection_matrix(1.0) * Mat4::look_at_rh(eye, Vec3::ZERO, up_for(eye));
    let uniform = create_buffer(
        &device,
        bytemuck::cast_slice(&view_proj.to_cols_array()),
        wgpu::BufferUsages::UNIFORM,
    );

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            r#"
            @group(0) @binding(0) var<uniform> view_proj: mat4x4<f32>;

            struct Out {
                @builtin(position) clip: vec4<f32>,
                @location(0) normal: vec3<f32>,
            };

            @vertex
            fn vs(
                @location(0) position: vec3<f32>,
                @location(1) normal: vec3<f32>,
            ) -> Out {
                var out: Out;
                out.clip = view_proj * vec4<f32>(position, 1.0);
                out.normal = normal;
                return out;
            }

            @fragment
            fn fs(in: Out) -> @location(0) vec4<f32> {
                // 法线编码成颜色：[-1,1] 映到 [0,1]。
                return vec4<f32>(normalize(in.normal) * 0.5 + 0.5, 1.0);
            }
            "#
            .into(),
        ),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            }),
        ),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 24,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 12,
                        shader_location: 1,
                    },
                ],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        // ── 这三行是本测试的全部意义所在 ──
        // 必须和 `create_standard_pipeline` 里的完全一致，否则验的就不是
        // 引擎的行为了。
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        // 闭合网格必须开深度测试：没有的话后画的三角形会盖住先画的，
        // 读回来的是「最后画的那个面」而不是「最靠前的那个面」。
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&Default::default());

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // 清成全 0：alpha 非零就说明有三角形盖到了。
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
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
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.indices().len() as u32, 0, 0..1);
    }

    // 每行按 256 字节对齐，64 × 4 = 256 正好，不用抠填充。
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (SIZE * SIZE * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
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
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (sender, receiver) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = sender.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    receiver.recv().ok()?.ok()?;

    let data = staging.slice(..).get_mapped_range().ok()?.to_vec();
    staging.unmap();

    let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    if data[center + 3] == 0 {
        return Some(None);
    }
    let decode = |byte: u8| byte as f32 / 255.0 * 2.0 - 1.0;
    Some(Some(Vec3::new(
        decode(data[center]),
        decode(data[center + 1]),
        decode(data[center + 2]),
    )))
}

/// 相机在正上方时 `look_at` 的 up 不能还是 Y，那样会退化。
fn up_for(eye: Vec3) -> Vec3 {
    if eye.x.abs() < 1e-4 && eye.z.abs() < 1e-4 {
        Vec3::Z
    } else {
        Vec3::Y
    }
}

fn create_buffer(
    device: &wgpu::Device,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents,
        usage,
    })
}

/// 没有可用适配器就返回 [`None`]，调用方跳过。
///
/// 借的是**整个测试进程共用**的那一台设备，不是自己开一台。
/// 每条测试各开一台的话，libtest 并行跑完之后几台设备同时析构，
/// 在 Windows 上会间歇性地以 `STATUS_STACK_BUFFER_OVERRUN` 把进程带走——
/// 而且是在「5 passed」之后崩的，测试报告全绿、退出码却是失败。
/// 详见 `ComputeContext::shared_headless` 的文档。
fn headless() -> Option<(&'static wgpu::Device, &'static wgpu::Queue)> {
    let gpu = krender::ComputeContext::shared_headless()?;
    Some((gpu.device(), gpu.queue()))
}

/// 六个方向各看一眼，报告每个方向上「画在最前面的那个面朝哪」。
///
/// 正常的闭合网格应当处处满足：**看到的面朝着相机**，也就是它的法线
/// 和视线方向的点积为正（法线 · 从表面指向相机的方向 > 0）。
///
/// 朝反了就说明那个方向的正面被剔掉了，顶上来的是背后那个面的内侧。
fn survey(mesh: &Mesh) -> Option<Vec<(&'static str, String)>> {
    let mut report = Vec::new();
    for (name, eye) in [
        ("+X", Vec3::new(3.0, 0.0, 0.0)),
        ("-X", Vec3::new(-3.0, 0.0, 0.0)),
        ("+Y", Vec3::new(0.0, 3.0, 0.0)),
        ("-Y", Vec3::new(0.0, -3.0, 0.0)),
        ("+Z", Vec3::new(0.0, 0.0, 3.0)),
        ("-Z", Vec3::new(0.0, 0.0, -3.0)),
    ] {
        let toward_camera = eye.normalize();
        let verdict = match face_at_center(mesh, eye)? {
            None => "什么都没画到".to_string(),
            Some(normal) => {
                let facing = normal.dot(toward_camera);
                if facing > 0.1 {
                    "正常".to_string()
                } else {
                    format!("看到的面法线是 {normal:?}，背对着相机（点积 {facing:.2}）")
                }
            }
        };
        report.push((name, verdict));
    }
    Some(report)
}

/// 六个方向全部正常时通过，否则把出问题的方向全列出来。
fn assert_all_directions_are_sane(name: &str, mesh: &Mesh) {
    let Some(report) = survey(mesh) else {
        return; // 没有 GPU，跳过
    };
    let bad: Vec<String> = report
        .iter()
        .filter(|(_, verdict)| verdict != "正常")
        .map(|(direction, verdict)| format!("  从 {direction} 看：{verdict}"))
        .collect();

    assert!(
        bad.is_empty(),
        "{name} 有 {} 个方向的绕序反了：
{}",
        bad.len(),
        bad.join(
            "
"
        )
    );
}

#[test]
fn the_ground_plane_is_visible_from_above() {
    // 这一条钉住**约定本身**。地面在每个例子里都看得见，所以它是
    // 已知正确的那个样本：`plane` 的绕序是什么，正面就是什么。
    //
    // 它挂了的话，不是 `plane` 坏了，是渲染器的 `front_face` /
    // `cull_mode` 被人改了——那会让所有图元一起翻面。
    let Some(face) = face_at_center(&Mesh::plane(1.0), Vec3::new(0.0, 3.0, 0.0)) else {
        return;
    };
    assert!(face.is_some(), "地面从上方看不见了 —— 剔除规则被改过");
}

#[test]
fn the_ground_plane_is_culled_from_below() {
    // 反面也要验：只验正面的话，「剔除根本没开」也会让上一条通过。
    let Some(face) = face_at_center(&Mesh::plane(1.0), Vec3::new(0.0, -3.0, 0.0)) else {
        return;
    };
    assert!(face.is_none(), "从下面也看得见地面 —— 背面剔除没生效");
}

#[test]
fn a_cube_shows_the_face_that_points_at_the_camera() {
    // 立方体每个面朝向都不同，是最容易把绕序问题暴露出来的图元：
    // 绕序反了的那个面被剔掉之后，顶上来的是**对面那个面的内侧**，
    // 于是画面上那一块的法线背对相机、光照全黑。
    assert_all_directions_are_sane("立方体", &Mesh::cube());
}

#[test]
fn a_sphere_shows_the_near_hemisphere() {
    // 球被翻面之后**轮廓完全不变**（看到的是远侧半球），肉眼极难发现。
    // 但那半球的法线背对相机，光照和深度都是错的。
    assert_all_directions_are_sane("球", &Mesh::sphere(16, 24));
}

#[test]
fn a_cylinder_shows_its_near_side_and_both_caps() {
    assert_all_directions_are_sane("圆柱", &Mesh::cylinder(24));
}
