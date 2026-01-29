use anyhow::anyhow;
use anyhow::Result;
use bevy_ecs::world::World;
use bytemuck::Pod;
use bytemuck::Zeroable;
use geo::Contains;
use geo::TriangulateEarcut;
use rstar::RTree;
use rstar::RTreeObject;
use std::num::NonZeroU32;
use std::num::NonZeroU64;
use std::time::Duration;
use std::time::Instant;
use winit::dpi::PhysicalPosition;
use winit::event::Event;
use winit::event::WindowEvent;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::window::Window;
use winit::window::WindowBuilder;

use crate::core::app_controller::Theme;
use crate::core::components::Layer;
use crate::core::components::ShapeInstance;
use crate::core::rtree::RTreeItem;
use crate::graphics::bounds::BoundingBox;
use crate::graphics::camera::Camera;
use crate::graphics::geometry::Geometry;
use crate::graphics::material::Material;
use crate::graphics::mesh::Mesh;

const INITIAL_WINDOW_WIDTH: u32 = 800;
const INITIAL_WINDOW_HEIGHT: u32 = 600;

const MAX_DRAWS_PER_FRAME: usize = 4096;

fn apply_theme_to_world(world: &mut World, theme: Theme) {
    let mut non_empty_layers = 0usize;
    for layer in world.query::<&Layer>().iter(world) {
        if !layer.shape_instances.is_empty() {
            non_empty_layers += 1;
        }
    }
    let alpha = if non_empty_layers == 0 {
        1.0
    } else {
        1.0 / (non_empty_layers as f32)
    };

    // Light: 画黑色（带 alpha），Dark: 画白色（带 alpha）
    let base_rgb = match theme {
        Theme::Light => [0.0, 0.0, 0.0],
        Theme::Dark => [1.0, 1.0, 1.0],
    };

    // 先收集要改的 mesh，避免同时可变/不可变借用 world。
    let mut layer_meshes: Vec<(bevy_ecs::entity::Entity, bool)> = Vec::new();
    for layer in world.query::<&Layer>().iter(world) {
        layer_meshes.push((layer.mesh, layer.visible));
    }

    for (mesh_entity, visible) in layer_meshes {
        if let Some(mut mesh) = world.get_mut::<Mesh>(mesh_entity) {
            mesh.visible = visible;
            mesh.set_vec4(
                "color",
                nalgebra::Vector4::new(base_rgb[0], base_rgb[1], base_rgb[2], alpha),
            );
        }
    }
}

fn build_fill_geometry(polygon: &crate::graphics::vectors::Polygon) -> (Vec<f32>, Vec<u32>) {
    let triangles = polygon.earcut_triangles_raw();

    let mut positions: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    positions.reserve(3 * triangles.vertices.len().saturating_div(2));
    indices.reserve(triangles.triangle_indices.len());

    for coord in triangles.vertices.chunks(2) {
        positions.push(coord[0] as f32);
        positions.push(coord[1] as f32);
        positions.push(0.0);
    }
    for index in triangles.triangle_indices {
        indices.push(index as u32);
    }
    (positions, indices)
}

fn build_ribbon_geometry(
    spine: &[crate::graphics::vectors::Point2d],
    width: f64,
    closed: bool,
) -> (Vec<f32>, Vec<u32>) {
    use crate::graphics::vectors::Point2d;
    use crate::graphics::vectors::Vector2d;

    if spine.len() < 2 {
        return (Vec::new(), Vec::new());
    }

    let points = spine;
    let mut positions: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let add_point = |positions: &mut Vec<f32>, p: Point2d| {
        positions.extend_from_slice(&[p.x as f32, p.y as f32, 0.0]);
    };

    let add_triangle = |indices: &mut Vec<u32>, a: u32, b: u32, c: u32| {
        indices.extend_from_slice(&[a, b, c]);
    };

    let count = if closed {
        points.len().saturating_sub(1)
    } else {
        points.len()
    };
    if count < 2 {
        return (Vec::new(), Vec::new());
    }

    let upper = if closed { count + 1 } else { count };

    for i in 0..upper {
        let prev = points[(i + count - 1) % count];
        let curr = points[i % count];
        let next = points[(i + 1) % count];

        let mut dir1 = (curr - prev).normalize();
        let mut dir2 = (next - curr).normalize();

        if !closed && i == 0 {
            dir1 = dir2;
        }
        if !closed && i == count - 1 {
            dir2 = dir1;
        }

        let normal = Vector2d::new(-dir1.y, dir1.x);

        let miter_dir = (dir1 + dir2).normalize();
        let miter_dir = Vector2d::new(-miter_dir.y, miter_dir.x);

        // 防止极端尖角导致过大 miter
        let denom = normal.dot(&miter_dir).abs().max(1e-6);
        let miter_length = 0.5 * width / denom;

        let base = (positions.len() / 3) as u32;
        add_point(&mut positions, curr + miter_dir * miter_length);
        add_point(&mut positions, curr - miter_dir * miter_length);
        if i > 0 {
            add_triangle(&mut indices, base - 2, base, base - 1);
            add_triangle(&mut indices, base - 1, base, base + 1);
        }
    }

    (positions, indices)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DrawUniform {
    model: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    projection: [[f32; 4]; 4],
    color: [f32; 4],
}

fn mat4_to_cols_array(m: &nalgebra::Matrix4<f32>) -> [[f32; 4]; 4] {
    let s = m.as_slice();
    [
        [s[0], s[1], s[2], s[3]],
        [s[4], s[5], s[6], s[7]],
        [s[8], s[9], s[10], s[11]],
        [s[12], s[13], s[14], s[15]],
    ]
}

struct WgpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    clear_color: wgpu::Color,

    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniform_stride: u64,

    camera: Camera,

    // Interaction
    is_dragging: bool,
    last_mouse_pos: Option<(u32, u32)>,
    current_cursor_pos: Option<PhysicalPosition<f64>>,
    zoom_speed: f64,

    // Hover overlay (fill + stroke)
    theme: Theme,
    rtree: RTree<RTreeItem>,
    hovered_shape: Option<bevy_ecs::entity::Entity>,
    hover_spine: Vec<crate::graphics::vectors::Point2d>,
    hover_fill_mesh: bevy_ecs::entity::Entity,
    hover_fill_geometry: bevy_ecs::entity::Entity,
    hover_stroke_mesh: bevy_ecs::entity::Entity,
    hover_stroke_geometry: bevy_ecs::entity::Entity,
    hover_stroke_width: f64,
}

impl WgpuState {
    async fn new(window: &Window, theme: Theme, world: &mut World) -> Result<Self> {
        let size = window.inner_size();

        let clear_color = match theme {
            Theme::Light => wgpu::Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            Theme::Dark => wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // `wgpu::Surface` 的生命周期受 window 约束。这里用 `transmute` 把生命周期延长到 'static，
        // 前提是我们保证 window 在 event loop 生命周期内不被 drop（确实如此：window 被 move 进闭包）。
        let surface = instance
            .create_surface(window)
            .map_err(|e| anyhow!("Failed to create wgpu surface: {e}"))?;
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("No suitable GPU adapters found"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("layout-viewer device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("Failed to request device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            caps.present_modes[0]
        };

        let alpha_mode = caps.alpha_modes[0];

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let mut camera = Camera::new(
            crate::graphics::vectors::Point3d::new(0.0, 0.0, 0.0),
            128.0,
            128.0,
            -1.0,
            1.0,
        );

        let mut world_bounds = BoundingBox::new();
        for layer in world.query::<&Layer>().iter(world) {
            world_bounds.encompass(&layer.world_bounds);
        }
        if !world_bounds.is_empty() {
            camera.fit_to_bounds((size.width.max(1), size.height.max(1)), world_bounds);
        } else {
            let aspect = size.width.max(1) as f64 / size.height.max(1) as f64;
            camera.height = camera.width / aspect;
        }

        // Build R-tree once (for hover picking)
        let mut rtree_items: Vec<RTreeItem> = Vec::new();
        for (entity, shape_instance) in world
            .query::<(bevy_ecs::entity::Entity, &ShapeInstance)>()
            .iter(world)
        {
            rtree_items.push(RTreeItem {
                shape_instance: entity,
                aabb: shape_instance.world_polygon.envelope(),
            });
        }
        let rtree = RTree::bulk_load(rtree_items);

        // Create hover overlay meshes (rendered on top)
        let hover_fill_geometry = world.spawn(Geometry::new()).id();
        let hover_stroke_geometry = world.spawn(Geometry::new()).id();
        let hover_material = world.spawn(Material::default()).id();

        let mut fill_mesh = Mesh::new(hover_fill_geometry, hover_material);
        fill_mesh.visible = false;
        fill_mesh.render_order = 9_999;
        let hover_fill_mesh = world.spawn(fill_mesh).id();

        let mut stroke_mesh = Mesh::new(hover_stroke_geometry, hover_material);
        stroke_mesh.visible = false;
        stroke_mesh.render_order = 10_000;
        let hover_stroke_mesh = world.spawn(stroke_mesh).id();

        let hover_stroke_width = 5.0 * camera.width / (size.width.max(1) as f64);

        let shader_source = r#"
struct Uniforms {
    model: mat4x4<f32>,
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> u: Uniforms;

struct VSOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> VSOut {
    var out: VSOut;
    out.pos = u.projection * u.view * u.model * vec4<f32>(position, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return u.color;
}
"#;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layout-viewer wgpu shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let uniform_size = std::mem::size_of::<DrawUniform>() as u64;
        let align = device.limits().min_uniform_buffer_offset_alignment as u64;
        let uniform_stride = ((uniform_size + align - 1) / align) * align;

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layout-viewer uniform buffer"),
            size: uniform_stride * (MAX_DRAWS_PER_FRAME as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // 不设置 min_binding_size：避免不同后端/平台对该字段的严格程度差异。
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layout-viewer bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layout-viewer bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: NonZeroU64::new(uniform_size),
                }),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layout-viewer pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layout-viewer pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            clear_color,

            pipeline,
            bind_group,
            uniform_buffer,
            uniform_stride,

            camera,

            is_dragging: false,
            last_mouse_pos: None,
            current_cursor_pos: None,
            zoom_speed: 0.05,

            theme,
            rtree,
            hovered_shape: None,
            hover_spine: Vec::new(),
            hover_fill_mesh,
            hover_fill_geometry,
            hover_stroke_mesh,
            hover_stroke_geometry,
            hover_stroke_width,
        })
    }

    fn set_hover_visible(&mut self, world: &mut World, visible: bool) {
        if let Some(mut mesh) = world.get_mut::<Mesh>(self.hover_fill_mesh) {
            mesh.visible = visible;
        }
        if let Some(mut mesh) = world.get_mut::<Mesh>(self.hover_stroke_mesh) {
            mesh.visible = visible;
        }
    }

    fn refresh_hover_stroke_width_if_needed(&mut self, world: &mut World) {
        if self.hover_spine.is_empty() {
            return;
        }

        let desired = 5.0 * self.camera.width / (self.config.width.max(1) as f64);
        if (desired - self.hover_stroke_width).abs() < 1e-6 {
            return;
        }
        self.hover_stroke_width = desired;

        let (positions, indices) =
            build_ribbon_geometry(&self.hover_spine, self.hover_stroke_width, true);
        if let Some(mut geo) = world.get_mut::<Geometry>(self.hover_stroke_geometry) {
            geo.positions = positions;
            geo.indices = indices;
        }
    }

    fn pick_shape_at_world(&self, world: &World, x: f64, y: f64) -> Option<bevy_ecs::entity::Entity> {
        let point = geo::Point::new(x, y);
        let items = self.rtree.locate_all_at_point(&point);

        let mut result: Option<bevy_ecs::entity::Entity> = None;
        let mut result_layer_index = -i16::MAX;

        for item in items {
            let Some(shape_instance) = world.get::<ShapeInstance>(item.shape_instance) else {
                continue;
            };

            if shape_instance.layer_index < result_layer_index {
                continue;
            }

            let Some(layer) = world.get::<Layer>(shape_instance.layer) else {
                continue;
            };
            if !layer.visible {
                continue;
            }

            if !shape_instance.world_polygon.contains(&point) {
                continue;
            }

            result = Some(item.shape_instance);
            result_layer_index = shape_instance.layer_index;
        }

        result
    }

    fn update_hover_at_screen(&mut self, world: &mut World, x: u32, y: u32) {
        let (wx, wy) = self.screen_to_world(x, y);
        let hit = self.pick_shape_at_world(world, wx, wy);

        if hit == self.hovered_shape {
            return;
        }

        self.hovered_shape = hit;

        let Some(hit) = hit else {
            self.hover_spine.clear();
            self.set_hover_visible(world, false);
            return;
        };

        let Some(shape_instance) = world.get::<ShapeInstance>(hit) else {
            self.hover_spine.clear();
            self.set_hover_visible(world, false);
            return;
        };

        // 先在不可变借用阶段把数据算出来，避免后续 get_mut 的借用冲突。
        let (fill_positions, fill_indices, spine) = {
            let (fill_positions, fill_indices) = build_fill_geometry(&shape_instance.world_polygon);
            let mut spine: Vec<crate::graphics::vectors::Point2d> = Vec::new();
            for coord in shape_instance.world_polygon.exterior().points() {
                spine.push(crate::graphics::vectors::Point2d::new(coord.x(), coord.y()));
            }
            (fill_positions, fill_indices, spine)
        };

        // Fill geometry
        if let Some(mut geo) = world.get_mut::<Geometry>(self.hover_fill_geometry) {
            geo.positions = fill_positions;
            geo.indices = fill_indices;
        }

        // Stroke geometry
        self.hover_spine = spine;
        let (stroke_positions, stroke_indices) =
            build_ribbon_geometry(&self.hover_spine, self.hover_stroke_width, true);
        if let Some(mut geo) = world.get_mut::<Geometry>(self.hover_stroke_geometry) {
            geo.positions = stroke_positions;
            geo.indices = stroke_indices;
        }

        // Colors
        let (fill_rgb, fill_a) = match self.theme {
            Theme::Light => ([0.0, 0.0, 0.0], 0.12),
            Theme::Dark => ([1.0, 1.0, 1.0], 0.12),
        };
        let stroke_color = match self.theme {
            Theme::Light => [0.0, 0.4, 0.6, 0.9],
            Theme::Dark => [0.0, 0.6, 1.0, 0.9],
        };

        if let Some(mut mesh) = world.get_mut::<Mesh>(self.hover_fill_mesh) {
            mesh.set_vec4(
                "color",
                nalgebra::Vector4::new(fill_rgb[0], fill_rgb[1], fill_rgb[2], fill_a),
            );
            mesh.visible = true;
        }
        if let Some(mut mesh) = world.get_mut::<Mesh>(self.hover_stroke_mesh) {
            mesh.set_vec4(
                "color",
                nalgebra::Vector4::new(
                    stroke_color[0],
                    stroke_color[1],
                    stroke_color[2],
                    stroke_color[3],
                ),
            );
            mesh.visible = true;
        }
    }

    fn screen_to_world(&self, screen_x: u32, screen_y: u32) -> (f64, f64) {
        let w = self.config.width.max(1) as f64;
        let h = self.config.height.max(1) as f64;
        let ndc_x = (screen_x as f64 / w) * 2.0 - 1.0;
        let ndc_y = -((screen_y as f64 / h) * 2.0 - 1.0);
        let world = self
            .camera
            .unproject(crate::graphics::vectors::Point3d::new(ndc_x, ndc_y, 0.0));
        (world.x, world.y)
    }

    fn handle_mouse_press(&mut self, x: u32, y: u32) {
        self.is_dragging = true;
        self.last_mouse_pos = Some((x, y));
    }

    fn handle_mouse_release(&mut self) {
        self.is_dragging = false;
        self.last_mouse_pos = None;
    }

    fn handle_mouse_move(&mut self, x: u32, y: u32) {
        if !self.is_dragging {
            return;
        }

        if let Some((last_x, last_y)) = self.last_mouse_pos {
            let p1 = self.screen_to_world(x, y);
            let p0 = self.screen_to_world(last_x, last_y);
            let dx = p1.0 - p0.0;
            let dy = p1.1 - p0.1;

            let mut pos = self.camera.position;
            pos.x -= dx;
            pos.y -= dy;
            self.camera.position = pos;
        }

        self.last_mouse_pos = Some((x, y));
    }

    fn handle_mouse_wheel(&mut self, x: u32, y: u32, delta_y: f64) {
        // Ignore very small deltas that might be touchpad bounce
        const MIN_DELTA: f64 = 0.01;
        if delta_y.abs() < MIN_DELTA {
            return;
        }

        // Convert screen coordinates to world space before zoom
        let (world_x, world_y) = self.screen_to_world(x, y);

        // Clamp delta to avoid extreme zoom changes
        let clamped = delta_y.clamp(-1.0, 1.0);
        let zoom_factor = if clamped > 0.0 {
            1.0 - self.zoom_speed
        } else {
            1.0 + self.zoom_speed
        };

        self.camera.width *= zoom_factor;
        self.camera.height *= zoom_factor;

        // Convert the same screen coordinates to world space after zoom
        let (new_world_x, new_world_y) = self.screen_to_world(x, y);

        // Adjust camera position to keep cursor point stable
        self.camera.position.x += world_x - new_world_x;
        self.camera.position.y += world_y - new_world_y;
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);

        let aspect = self.config.width as f64 / self.config.height as f64;
        self.camera.height = self.camera.width / aspect;
    }

    fn render(&mut self, world: &mut World) -> Result<()> {
        // 缩放/resize 后，hover 线宽需要跟随更新
        self.refresh_hover_stroke_width_if_needed(world);
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(anyhow!("wgpu: out of memory"));
            }
            Err(e) => {
                // Timeout / Outdated 等：下一帧重试即可
                log::warn!("wgpu surface error: {e:?}");
                return Ok(());
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("layout-viewer wgpu encoder"),
            });

        let projection = self.camera.get_projection_matrix().cast::<f32>();
        let view_matrix = self.camera.get_view_matrix().cast::<f32>();

        // 收集所有可见 mesh，并按 render_order 排序。
        let mut meshes: Vec<(i32, nalgebra::Matrix4<f32>, [f32; 4], bevy_ecs::entity::Entity)> =
            Vec::new();
        for (_entity, mesh) in world.query::<(bevy_ecs::entity::Entity, &Mesh)>().iter(world) {
            if !mesh.visible {
                continue;
            }
            let color = mesh
                .get_vec4("color")
                .map(|c| [c.x, c.y, c.z, c.w])
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);

            meshes.push((mesh.render_order, mesh.matrix, color, mesh.geometry));
        }
        meshes.sort_by_key(|(order, _, _, _)| *order);

        struct DrawGpu {
            vertex_buffer: wgpu::Buffer,
            index_buffer: wgpu::Buffer,
            index_count: u32,
            uniform_offset: u32,
        }

        let mut draws: Vec<DrawGpu> = Vec::new();
        for (i, (_order, model_matrix, color, geometry_entity)) in meshes.iter().enumerate() {
            if i >= MAX_DRAWS_PER_FRAME {
                log::warn!(
                    "wgpu: exceeded MAX_DRAWS_PER_FRAME ({}), dropping remaining draws",
                    MAX_DRAWS_PER_FRAME
                );
                break;
            }

            let Some(geometry) = world.get::<Geometry>(*geometry_entity) else {
                continue;
            };

            if geometry.positions.is_empty() || geometry.indices.is_empty() {
                continue;
            }

            let uniform = DrawUniform {
                model: mat4_to_cols_array(model_matrix),
                view: mat4_to_cols_array(&view_matrix),
                projection: mat4_to_cols_array(&projection),
                color: *color,
            };

            let offset = (i as u64) * self.uniform_stride;
            self.queue
                .write_buffer(&self.uniform_buffer, offset, bytemuck::bytes_of(&uniform));

            let vb_size = (geometry.positions.len() * std::mem::size_of::<f32>()) as u64;
            let vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("layout-viewer vertex buffer"),
                size: vb_size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(
                &vertex_buffer,
                0,
                bytemuck::cast_slice(geometry.positions.as_slice()),
            );

            let ib_size = (geometry.indices.len() * std::mem::size_of::<u32>()) as u64;
            let index_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("layout-viewer index buffer"),
                size: ib_size,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(
                &index_buffer,
                0,
                bytemuck::cast_slice(geometry.indices.as_slice()),
            );

            draws.push(DrawGpu {
                vertex_buffer,
                index_buffer,
                index_count: geometry.indices.len() as u32,
                uniform_offset: offset as u32,
            });
        }

        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("layout-viewer render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            rp.set_pipeline(&self.pipeline);

            for draw in &draws {
                rp.set_bind_group(0, &self.bind_group, &[draw.uniform_offset]);
                rp.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
                rp.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                rp.draw_indexed(0..draw.index_count, 0, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

pub fn spawn_wgpu_window(world: World, theme: Theme) -> Result<()> {
    let mut world = world;

    apply_theme_to_world(&mut world, theme);

    let event_loop = EventLoop::new()?;
    let window = WindowBuilder::new()
        .with_title("Layout Viewer (wgpu)")
        .with_inner_size(winit::dpi::LogicalSize::new(
            INITIAL_WINDOW_WIDTH,
            INITIAL_WINDOW_HEIGHT,
        ))
        .build(&event_loop)?;

    let mut state = pollster::block_on(WgpuState::new(&window, theme, &mut world))?;

    let mut next_tick = Instant::now();
    let tick_interval = Duration::from_millis(16);

    let _ = event_loop.run(move |event, window_target| {
        if let Some(next_tick_time) = next_tick.checked_add(tick_interval) {
            window_target.set_control_flow(ControlFlow::WaitUntil(next_tick_time));
        }

        match event {
            Event::AboutToWait => {
                let now = Instant::now();
                if now >= next_tick {
                    window.request_redraw();
                    next_tick = now + tick_interval;
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    window_target.exit();
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    use winit::keyboard::KeyCode;
                    use winit::keyboard::PhysicalKey;
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if code == KeyCode::Escape || code == KeyCode::KeyQ {
                            window_target.exit();
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    state.current_cursor_pos = Some(position);
                    state.handle_mouse_move(position.x as u32, position.y as u32);
                    state.update_hover_at_screen(&mut world, position.x as u32, position.y as u32);
                    if state.is_dragging {
                        window.request_redraw();
                    } else {
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseInput {
                    state: button_state,
                    button,
                    ..
                } => {
                    use winit::event::MouseButton;
                    if button == MouseButton::Left {
                        match button_state {
                            winit::event::ElementState::Pressed => {
                                if let Some(pos) = state.current_cursor_pos {
                                    state.handle_mouse_press(pos.x as u32, pos.y as u32);
                                }
                            }
                            winit::event::ElementState::Released => {
                                state.handle_mouse_release();
                            }
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if let Some(pos) = state.current_cursor_pos {
                        let delta_y = match delta {
                            winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64,
                            winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y,
                        };
                        state.handle_mouse_wheel(pos.x as u32, pos.y as u32, delta_y);
                        state.update_hover_at_screen(&mut world, pos.x as u32, pos.y as u32);
                        window.request_redraw();
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    state.hovered_shape = None;
                    state.hover_spine.clear();
                    state.set_hover_visible(&mut world, false);
                    window.request_redraw();
                }
                WindowEvent::Resized(size) => {
                    // wgpu 要求宽高非 0；winit 最小化时会给 0
                    let width = NonZeroU32::new(size.width);
                    let height = NonZeroU32::new(size.height);
                    if width.is_some() && height.is_some() {
                        state.resize(size);
                        if let Some(pos) = state.current_cursor_pos {
                            state.update_hover_at_screen(
                                &mut world,
                                pos.x as u32,
                                pos.y as u32,
                            );
                        }
                        window.request_redraw();
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Err(err) = state.render(&mut world) {
                        log::error!("wgpu render failed: {err:#}");
                        window_target.exit();
                    }
                }
                _ => (),
            },
            _ => (),
        }
    });

    Ok(())
}
