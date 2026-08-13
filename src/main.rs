mod task;

use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::camera::rh::{proj::directx::perspective, view::look_at_mat4};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 3],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[rustfmt::skip]
const VERTICES: &[Vertex] = &[
    // +X face (red)
    Vertex { position: [ 0.5, -0.5, -0.5], color: [1.0, 0.0, 0.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], color: [1.0, 0.0, 0.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], color: [1.0, 0.0, 0.0] },
    Vertex { position: [ 0.5, -0.5,  0.5], color: [1.0, 0.0, 0.0] },
    // -X face (cyan)
    Vertex { position: [-0.5, -0.5,  0.5], color: [0.0, 1.0, 1.0] },
    Vertex { position: [-0.5,  0.5,  0.5], color: [0.0, 1.0, 1.0] },
    Vertex { position: [-0.5,  0.5, -0.5], color: [0.0, 1.0, 1.0] },
    Vertex { position: [-0.5, -0.5, -0.5], color: [0.0, 1.0, 1.0] },
    // +Y face (green)
    Vertex { position: [-0.5,  0.5, -0.5], color: [0.0, 1.0, 0.0] },
    Vertex { position: [-0.5,  0.5,  0.5], color: [0.0, 1.0, 0.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], color: [0.0, 1.0, 0.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], color: [0.0, 1.0, 0.0] },
    // -Y face (magenta)
    Vertex { position: [-0.5, -0.5,  0.5], color: [1.0, 0.0, 1.0] },
    Vertex { position: [-0.5, -0.5, -0.5], color: [1.0, 0.0, 1.0] },
    Vertex { position: [ 0.5, -0.5, -0.5], color: [1.0, 0.0, 1.0] },
    Vertex { position: [ 0.5, -0.5,  0.5], color: [1.0, 0.0, 1.0] },
    // +Z face (blue)
    Vertex { position: [-0.5, -0.5,  0.5], color: [0.0, 0.0, 1.0] },
    Vertex { position: [ 0.5, -0.5,  0.5], color: [0.0, 0.0, 1.0] },
    Vertex { position: [ 0.5,  0.5,  0.5], color: [0.0, 0.0, 1.0] },
    Vertex { position: [-0.5,  0.5,  0.5], color: [0.0, 0.0, 1.0] },
    // -Z face (yellow)
    Vertex { position: [ 0.5, -0.5, -0.5], color: [1.0, 1.0, 0.0] },
    Vertex { position: [-0.5, -0.5, -0.5], color: [1.0, 1.0, 0.0] },
    Vertex { position: [-0.5,  0.5, -0.5], color: [1.0, 1.0, 0.0] },
    Vertex { position: [ 0.5,  0.5, -0.5], color: [1.0, 1.0, 0.0] },
];

#[rustfmt::skip]
const INDICES: &[u16] = &[
    0, 1, 2, 0, 2, 3,       // +X
    4, 5, 6, 4, 6, 7,       // -X
    8, 9, 10, 8, 10, 11,    // +Y
    12, 13, 14, 12, 14, 15, // -Y
    16, 17, 18, 16, 18, 19, // +Z
    20, 21, 22, 20, 22, 23, // -Z
];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
}

struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    render_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    num_indices: u32,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    depth_view: wgpu::TextureView,
    start_time: Instant,
    window: Arc<Window>,
}

impl State {
    async fn new(window: Arc<Window>) -> Self {
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
                label: Some("device"),
                ..Default::default()
            })
            .await
            .unwrap();

        let mut config = surface.get_default_config(&adapter, size.width, size.height).unwrap();
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let depth_view = Self::create_depth_view(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertex buffer"),
            contents: bytemuck::cast_slice(VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("index buffer"),
            contents: bytemuck::cast_slice(INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform buffer"),
            size: std::mem::size_of::<Uniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform bind group layout"),
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

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline layout"),
            bind_group_layouts: &[Option::from(&bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Option::from(Vertex::layout())],
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

        Self {
            surface,
            device,
            queue,
            config,
            size,
            render_pipeline,
            vertex_buffer,
            index_buffer,
            num_indices: INDICES.len() as u32,
            uniform_buffer,
            uniform_bind_group,
            depth_view,
            start_time: Instant::now(),
            window,
        }
    }

    fn create_depth_view(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth texture"),
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

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.depth_view = Self::create_depth_view(&self.device, &self.config);
    }

    fn update(&mut self) {
        let t = self.start_time.elapsed().as_secs_f32();

        let model = Mat4::from_rotation_y(t) * Mat4::from_rotation_x(t * 0.6);
        let view = look_at_mat4(Vec3::new(0.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y);
        let aspect = self.config.width as f32 / self.config.height as f32;
        let proj = perspective(45f32.to_radians(), aspect, 0.1, 100.0);

        let mvp = proj * view * model;
        let uniforms = Uniforms {
            mvp: mvp.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
    }

    fn render(&mut self) -> RenderOutcome {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return RenderOutcome::Skip;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return RenderOutcome::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Validation => return RenderOutcome::Fatal,
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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

            render_pass.set_pipeline(&self.render_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.num_indices, 0, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(output);

        RenderOutcome::Ok
    }
}

enum RenderOutcome {
    Ok,
    Skip,
    Reconfigure,
    Fatal,
}

#[derive(Default)]
struct App {
    state: Option<State>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window_attributes = Window::default_attributes().with_title("wgpu cube");
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        self.state = Some(pollster::block_on(State::new(window)));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(new_size) => state.resize(new_size),
            WindowEvent::RedrawRequested => {
                state.update();
                match state.render() {
                    RenderOutcome::Ok | RenderOutcome::Skip => {}
                    RenderOutcome::Reconfigure => state.resize(state.size),
                    RenderOutcome::Fatal => event_loop.exit(),
                }
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop.run_app(&mut app).unwrap();
}
