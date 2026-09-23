//! 调试线渲染。
//!
//! 整个 pass 只做一件事：把 [`kgizmo::Gizmos`] 攒了一帧的线段顶点传上去画掉。
//! 没有剔除、没有排序、没有合批——线段本来就没有材质可切换，
//! 一次 `draw` 就能把一层全画完。
//!
//! 两条管线只差深度状态：
//!
//! - **深度层**参与深度测试（但不写深度），会被场景挡住；
//! - **覆盖层**深度比较恒真，永远画在最上面。
//!
//! 两条都不写深度：调试线是叠上去的东西，不该影响它之后画的任何像素。

use bytemuck::{Pod, Zeroable};
use kgizmo::{GizmoVertex, Gizmos, Layer, LineSet};
use kmath::Mat4;
use std::{collections::HashMap, num::NonZeroU64};

/// 常驻线段每帧一份矩阵，按 uniform 动态偏移的对齐要求排开。
const MATRIX_STRIDE: u64 = 256;

/// 一个常驻线段集在显存里的那份拷贝。
struct RetainedLines {
    buffer: wgpu::Buffer,
    count: u32,
    /// 最后一次被画是第几帧。长期没人画的缓冲会被回收。
    last_used: u64,
}

/// 本帧一次常驻线段的绘制。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetainedDraw {
    id: u64,
    slot: u32,
    count: u32,
}

/// 调试线 pass 的全局量，对应 `gizmo.wgsl` 的 `Globals`。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GizmoGlobals {
    view_proj: [[f32; 4]; 4],
}

/// 一层的顶点在缓冲里的位置。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GizmoSlice {
    first: u32,
    count: u32,
}

/// 本帧两层各自的绘制范围。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GizmoDraw {
    depth: GizmoSlice,
    overlay: GizmoSlice,
}

impl GizmoDraw {
    /// 本帧一条线都没有。
    pub(crate) fn is_empty(&self) -> bool {
        self.depth.count == 0 && self.overlay.count == 0
    }
}

/// 调试线 pass 所需的一组 GPU 资源。
pub(crate) struct GizmoResources {
    depth_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,

    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,

    vertex_buffer: wgpu::Buffer,
    /// 当前顶点缓冲能容纳的顶点数。
    capacity: u64,

    /// 逐帧复用的暂存区：两层拼成一个缓冲，一次写完。
    scratch: Vec<GizmoVertex>,

    // ── 常驻线段（`kgizmo::LineSet`）──
    retained_pipeline: wgpu::RenderPipeline,
    retained_layout: wgpu::BindGroupLayout,
    matrix_buffer: wgpu::Buffer,
    matrix_bind_group: wgpu::BindGroup,
    /// 矩阵缓冲能放几份。
    matrix_capacity: u64,
    retained: HashMap<u64, RetainedLines>,
    frame: u64,
}

impl GizmoResources {
    /// 初始容量，够画几百根线；不够时翻倍。
    const INITIAL_CAPACITY: u64 = 4096;

    pub(crate) fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kengine gizmo shader"),
            source: wgpu::ShaderSource::Wgsl(kgizmo::GIZMO_WGSL.into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine gizmo globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size_of::<GizmoGlobals>() as u64),
                },
                count: None,
            }],
        });
        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kengine gizmo globals"),
            size: size_of::<GizmoGlobals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kengine gizmo globals bind group"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kengine gizmo pipeline layout"),
            bind_group_layouts: &[Option::from(&globals_layout)],
            immediate_size: 0,
        });

        let depth_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            depth_format,
            wgpu::CompareFunction::LessEqual,
            "depth",
        );
        let overlay_pipeline = create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            color_format,
            depth_format,
            // 恒真 = 关掉深度测试。仍然挂着深度附件，因为同一个 pass 里
            // 的所有管线必须报同一份深度格式。
            wgpu::CompareFunction::Always,
            "overlay",
        );

        // 常驻线段：同一个着色器，但 `Globals` 走动态偏移——每个线段集
        // 一份 `view_proj × model`，着色器完全不用改。
        let retained_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kengine retained lines layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(size_of::<GizmoGlobals>() as u64),
                },
                count: None,
            }],
        });
        let retained_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("kengine retained lines pipeline layout"),
                bind_group_layouts: &[Option::from(&retained_layout)],
                immediate_size: 0,
            });
        let retained_pipeline = create_pipeline(
            device,
            &retained_pipeline_layout,
            &shader,
            color_format,
            depth_format,
            wgpu::CompareFunction::LessEqual,
            "retained",
        );
        let matrix_capacity = 16;
        let matrix_buffer = create_matrix_buffer(device, matrix_capacity);
        let matrix_bind_group = create_matrix_bind_group(device, &retained_layout, &matrix_buffer);

        Self {
            depth_pipeline,
            overlay_pipeline,
            globals_buffer,
            globals_bind_group,
            vertex_buffer: create_vertex_buffer(device, Self::INITIAL_CAPACITY),
            capacity: Self::INITIAL_CAPACITY,
            scratch: Vec::new(),
            retained_pipeline,
            retained_layout,
            matrix_buffer,
            matrix_bind_group,
            matrix_capacity,
            retained: HashMap::new(),
            frame: 0,
        }
    }

    /// 准备本帧的常驻线段：没见过的线段集上传一次，之后每帧只写矩阵。
    pub(crate) fn prepare_retained<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        items: impl Iterator<Item = (&'a LineSet, Mat4)>,
        view_proj: Mat4,
    ) -> Vec<RetainedDraw> {
        use wgpu::util::DeviceExt;
        self.frame += 1;
        let mut draws = Vec::new();
        let mut matrices: Vec<u8> = Vec::new();
        for (lines, model) in items {
            let entry = self
                .retained
                .entry(lines.id())
                .or_insert_with(|| RetainedLines {
                    buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("kengine retained lines"),
                        contents: bytemuck::cast_slice(lines.vertices()),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    count: lines.vertices().len() as u32,
                    last_used: 0,
                });
            entry.last_used = self.frame;
            let slot = draws.len() as u32;
            let globals = GizmoGlobals {
                view_proj: (view_proj * model).to_cols_array_2d(),
            };
            matrices.extend_from_slice(bytemuck::bytes_of(&globals));
            matrices.resize(
                matrices.len() + MATRIX_STRIDE as usize - size_of::<GizmoGlobals>(),
                0,
            );
            draws.push(RetainedDraw {
                id: lines.id(),
                slot,
                count: entry.count,
            });
        }
        // 一秒多没被画的线段集：节点删了、换了几何，显存还给系统。
        let frame = self.frame;
        self.retained.retain(|_, r| frame - r.last_used < 120);

        if draws.is_empty() {
            return draws;
        }
        if draws.len() as u64 > self.matrix_capacity {
            self.matrix_capacity = (draws.len() as u64).next_power_of_two();
            self.matrix_buffer = create_matrix_buffer(device, self.matrix_capacity);
            self.matrix_bind_group =
                create_matrix_bind_group(device, &self.retained_layout, &self.matrix_buffer);
        }
        queue.write_buffer(&self.matrix_buffer, 0, &matrices);
        draws
    }

    /// 画常驻线段。和网格一样参与深度测试，不写深度。
    pub(crate) fn draw_retained(&self, pass: &mut wgpu::RenderPass<'_>, draws: &[RetainedDraw]) {
        if draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.retained_pipeline);
        for draw in draws {
            let Some(lines) = self.retained.get(&draw.id) else {
                continue;
            };
            pass.set_bind_group(
                0,
                &self.matrix_bind_group,
                &[draw.slot * MATRIX_STRIDE as u32],
            );
            pass.set_vertex_buffer(0, lines.buffer.slice(..));
            pass.draw(0..draw.count, 0..1);
        }
    }

    /// 把本帧的线段传上显存，返回两层各自的绘制范围。
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gizmos: &Gizmos,
        view_proj: Mat4,
    ) -> GizmoDraw {
        self.scratch.clear();

        let depth = gizmos.vertices(Layer::Depth);
        let overlay = gizmos.vertices(Layer::Overlay);
        if depth.is_empty() && overlay.is_empty() {
            return GizmoDraw::default();
        }

        // 两层顺次拼进同一个缓冲，各自记住起点——只上传一次，只绑一次。
        self.scratch.extend_from_slice(depth);
        self.scratch.extend_from_slice(overlay);

        let draw = GizmoDraw {
            depth: GizmoSlice {
                first: 0,
                count: depth.len() as u32,
            },
            overlay: GizmoSlice {
                first: depth.len() as u32,
                count: overlay.len() as u32,
            },
        };

        if self.scratch.len() as u64 > self.capacity {
            let capacity = (self.scratch.len() as u64).next_power_of_two();
            self.vertex_buffer = create_vertex_buffer(device, capacity);
            self.capacity = capacity;
        }
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.scratch));
        queue.write_buffer(
            &self.globals_buffer,
            0,
            bytemuck::cast_slice(&[GizmoGlobals {
                view_proj: view_proj.to_cols_array_2d(),
            }]),
        );

        draw
    }

    /// 提交绘制。放在主 pass 的最后——调试线要盖在所有东西上面。
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, draw: &GizmoDraw) {
        if draw.is_empty() {
            return;
        }

        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));

        // 先画会被遮挡的那层，再画覆盖层，顺序和它们的语义一致。
        for (pipeline, slice) in [
            (&self.depth_pipeline, draw.depth),
            (&self.overlay_pipeline, draw.overlay),
        ] {
            if slice.count == 0 {
                continue;
            }
            pass.set_pipeline(pipeline);
            pass.draw(slice.first..slice.first + slice.count, 0..1);
        }
    }
}

fn create_matrix_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine retained line matrices"),
        size: MATRIX_STRIDE * capacity.max(1),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_matrix_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("kengine retained line matrices bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size_of::<GizmoGlobals>() as u64),
            }),
        }],
    })
}

fn create_vertex_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kengine gizmo vertices"),
        size: size_of::<GizmoVertex>() as u64 * capacity.max(1),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    depth_compare: wgpu::CompareFunction,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!("kengine gizmo pipeline ({label})")),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("gizmo_vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<GizmoVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 12,
                        shader_location: 1,
                    },
                ],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("gizmo_fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                // 着色器输出预乘 alpha，于是半透明的调试线也能正确叠加。
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
            topology: wgpu::PrimitiveTopology::LineList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // 线段没有正反面。
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            // 只测不写：调试线是叠加物，不该挡住它之后画的任何东西。
            depth_write_enabled: Option::from(false),
            depth_compare: Option::from(depth_compare),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kgizmo::Color;
    use kmath::Vec3;

    #[test]
    fn shader_compiles_and_exposes_both_entry_points() {
        let module = naga::front::wgsl::parse_str(kgizmo::GIZMO_WGSL).expect("着色器应当能解析");
        let names: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(names.contains(&"gizmo_vs"));
        assert!(names.contains(&"gizmo_fs"));
    }

    #[test]
    fn the_vertex_attribute_offsets_match_the_struct() {
        // 颜色的偏移必须正好等于位置占的字节数，错了就是颜色读到坐标上。
        assert_eq!(size_of::<[f32; 3]>(), 12);
        assert_eq!(size_of::<GizmoVertex>(), 28);
    }

    #[test]
    fn slices_partition_the_buffer_without_overlap() {
        // 这一段逻辑不碰 GPU，可以直接验：两层拼进一个缓冲后
        // 覆盖层的起点必须正好接在深度层末尾，错了就是画串。
        let mut gizmos = Gizmos::new();
        gizmos.set_enabled(true);
        gizmos.line(Vec3::ZERO, Vec3::X, Color::RED);
        gizmos.line(Vec3::ZERO, Vec3::Y, Color::RED);
        gizmos.on_top(|g| g.line(Vec3::ZERO, Vec3::Z, Color::GREEN));

        let depth_count = gizmos.vertices(Layer::Depth).len() as u32;
        let overlay_count = gizmos.vertices(Layer::Overlay).len() as u32;
        assert_eq!(depth_count, 4);
        assert_eq!(overlay_count, 2);

        let draw = GizmoDraw {
            depth: GizmoSlice {
                first: 0,
                count: depth_count,
            },
            overlay: GizmoSlice {
                first: depth_count,
                count: overlay_count,
            },
        };
        assert_eq!(draw.overlay.first, draw.depth.first + draw.depth.count);
        assert!(!draw.is_empty());
    }

    #[test]
    fn an_empty_frame_draws_nothing() {
        assert!(GizmoDraw::default().is_empty());
    }
}
