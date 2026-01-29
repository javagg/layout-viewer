use bevy_ecs::entity::Entity;
use bevy_ecs::query::QueryState;
use bevy_ecs::world::World;
use geo::Contains;
use rstar::RTree;
use rstar::RTreeObject;

use crate::core::components::Hovered;
use crate::core::components::Layer;
use crate::core::components::LayerMaterial;
use crate::core::components::ShapeInstance;
use crate::core::hover_effect::HoverEffect;
use crate::core::hover_effect::HoverParams;
use crate::core::layer_proxy::LayerProxy;
use crate::core::rtree::RTreeItem;
use crate::graphics::bounds::BoundingBox;
use crate::graphics::camera::Camera;
use crate::graphics::geometry::Geometry;
use crate::graphics::material::BlendMode;
use crate::graphics::material::Material;
use crate::graphics::mesh::Mesh;
use crate::graphics::renderer::Renderer;
use crate::graphics::vectors::*;
use crate::graphics::viewport::Viewport;

/// Encapsulates high-level application logic common to all platforms.
pub struct AppController {
    window_size: (u32, u32),
    renderer: Renderer,
    camera: Camera,
    world: World,
    queries: QueryBundle,
    is_dragging: bool,
    last_mouse_pos: Option<(u32, u32)>,
    zoom_speed: f64,
    needs_render: bool,
    hover_effect: HoverEffect,
    rtree: RTree<RTreeItem>,
    pinch_state: Option<PinchState>,
}

#[derive(Clone, Copy)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn inverse(&self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
    pub fn is_dark(&self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// All coordinates and distances are in world space.
struct PinchState {
    start_center: Vector2d,
    start_distance: f64,
    start_camera_position: Point3d,
    start_camera_width: f64,
}

impl AppController {
    pub fn new(renderer: Renderer, physical_width: u32, physical_height: u32) -> Self {
        let camera = Camera::new(Point3d::new(0.0, 0.0, 0.0), 128.0, 128.0, -1.0, 1.0);

        let mut world = World::new();

        let hover_effect = HoverEffect::new(&mut world);

        let queries = QueryBundle::new(&mut world);

        Self {
            window_size: (physical_width, physical_height),
            renderer,
            camera,
            world,
            queries,
            is_dragging: false,
            last_mouse_pos: None,
            zoom_speed: 0.05,
            needs_render: true,
            hover_effect,
            rtree: RTree::new(),
            pinch_state: None,
        }
    }

    pub fn set_world(&mut self, mut world: World) {
        if world.id() == self.world.id() {
            return;
        }

        self.hover_effect = HoverEffect::new(&mut world);
        self.hover_effect.set_render_order(&mut world, 9999);
        self.renderer.on_new_world(&mut world);
        self.world = world;
        self.queries.update(&mut self.world);

        let mut world_bounds = BoundingBox::new();
        for layer in self.queries.layers.iter_mut(&mut self.world) {
            world_bounds.encompass(&layer.world_bounds);
        }

        self.camera.fit_to_bounds(self.window_size, world_bounds);

        self.render();

        let mut rtree_items = Vec::new();
        for (entity, shape_instance) in self.queries.shapes.iter(&self.world) {
            rtree_items.push(RTreeItem {
                shape_instance: entity,
                aabb: shape_instance.world_polygon.envelope(),
            });
        }
        self.rtree = RTree::bulk_load(rtree_items);
    }

    pub fn handle_mouse_press(&mut self, x: u32, y: u32) {
        if self.pinch_state.is_some() {
            return;
        }
        self.is_dragging = true;
        self.last_mouse_pos = Some((x, y));
    }

    pub fn handle_mouse_release(&mut self) {
        self.pinch_state = None;
        self.is_dragging = false;
        self.last_mouse_pos = None;
    }

    pub fn handle_pinch_start(&mut self, distance: f64, center: Vector2u) {
        let (center_x, center_y) = self.screen_to_world(center.x, center.y);
        self.pinch_state = Some(PinchState {
            start_center: Vector2d::new(center_x, center_y),
            start_distance: distance,
            start_camera_position: self.camera.position,
            start_camera_width: self.camera.width,
        });
    }

    pub fn handle_pinch_zoom(&mut self, distance: f64, center: Vector2u) {
        let Some(pinch_state) = &self.pinch_state else {
            return;
        };

        let aspect = self.camera.height / self.camera.width;

        // Reset the camera; use the original coordinate system for all
        // subsequent calculations to prevent jitter.
        self.camera.position.x = pinch_state.start_camera_position.x;
        self.camera.position.y = pinch_state.start_camera_position.y;
        self.camera.width = pinch_state.start_camera_width;
        self.camera.height = self.camera.width * aspect;

        // Convert current center to world space
        let (center_x, center_y) = self.screen_to_world(center.x, center.y);

        // Calculate zoom factor based on the change in distance
        let zoom_factor = pinch_state.start_distance / distance;

        // Update camera size (zoom)
        let new_width = pinch_state.start_camera_width * zoom_factor;

        // Adjust camera position to keep pinch center stable
        let dx = pinch_state.start_center.x - center_x;
        let dy = pinch_state.start_center.y - center_y;

        self.camera.position.x = pinch_state.start_camera_position.x + dx;
        self.camera.position.y = pinch_state.start_camera_position.y + dy;
        self.camera.width = new_width;
        self.camera.height = new_width * aspect;

        self.render();
    }

    pub fn handle_pinch_release(&mut self) {
        self.pinch_state = None;
    }

    pub fn handle_mouse_move(&mut self, x: u32, y: u32) {
        if self.pinch_state.is_some() {
            return;
        }
        if self.is_dragging {
            if let Some((last_x, last_y)) = self.last_mouse_pos {
                let p1 = self.screen_to_world(x, y);
                let p0 = self.screen_to_world(last_x, last_y);
                let dx = p1.0 - p0.0;
                let dy = p1.1 - p0.1;

                let mut pos = self.camera.position;
                pos.x -= dx;
                pos.y -= dy;
                self.camera.position = pos;
                self.render();
            }
            self.last_mouse_pos = Some((x, y));
        }

        // Convert screen coordinates to world space
        let (world_x, world_y) = self.screen_to_world(x, y);

        // Find the single entity that has the Hovered component, if it exists.
        let hovered_entity = self
            .world
            .query::<(Entity, &Hovered)>()
            .single(&self.world)
            .ok()
            .map(|(entity, _)| entity)
            .unwrap_or(Entity::PLACEHOLDER);

        if let Some(hit) = self.pick_cell(world_x, world_y) {
            if hit.shape_instance != hovered_entity {
                if hovered_entity != Entity::PLACEHOLDER {
                    self.world.entity_mut(hovered_entity).remove::<Hovered>();
                }
                self.world.entity_mut(hit.shape_instance).insert(Hovered);
                self.hover_effect.show(HoverParams {
                    shape_instance: hit.shape_instance,
                    world: &mut self.world,
                    gl: self.renderer.gl(),
                });
                self.render();
            }
        } else if hovered_entity != Entity::PLACEHOLDER {
            self.hover_effect.hide(&mut self.world);
            self.world.entity_mut(hovered_entity).remove::<Hovered>();
            self.render();
        }
    }

    pub fn handle_mouse_wheel(&mut self, x: u32, y: u32, delta: f64) {
        if self.pinch_state.is_some() {
            return;
        }
        // Ignore very small deltas that might be touchpad bounce
        const MIN_DELTA: f64 = 0.01;
        if delta.abs() < MIN_DELTA {
            return;
        }

        // Convert screen coordinates to world space before zoom
        let (world_x, world_y) = self.screen_to_world(x, y);

        // Calculate zoom factor (positive delta = zoom in, negative = zoom out)
        // Clamp delta to avoid extreme zoom changes
        let clamped_delta = delta.clamp(-1.0, 1.0);
        let zoom_factor = if clamped_delta > 0.0 {
            1.0 - self.zoom_speed
        } else {
            1.0 + self.zoom_speed
        };

        // Update camera size (zoom)
        self.camera.width *= zoom_factor;
        self.camera.height *= zoom_factor;

        // Convert the same screen coordinates to world space after zoom
        let (new_world_x, new_world_y) = self.screen_to_world(x, y);

        // Adjust camera position to keep cursor point stable
        self.camera.position.x += world_x - new_world_x;
        self.camera.position.y += world_y - new_world_y;

        self.render();
    }

    pub fn handle_mouse_leave(&mut self) {
        self.is_dragging = false;

        let hovered_entity = self
            .world
            .query::<(Entity, &Hovered)>()
            .single(&self.world)
            .ok()
            .map(|(entity, _)| entity)
            .unwrap_or(Entity::PLACEHOLDER);

        if hovered_entity != Entity::PLACEHOLDER {
            self.hover_effect.hide(&mut self.world);
            self.world.entity_mut(hovered_entity).remove::<Hovered>();
            self.render();
        }
    }

    /// Requests a render to occur during the next tick.
    pub fn render(&mut self) {
        self.needs_render = true;
    }

    /// Unconditionally called every 16 ms, returns "true" if the framebuffer
    /// was refreshed.
    pub fn tick(&mut self) -> bool {
        if !self.needs_render {
            return false;
        }

        let width = 5.0 * self.camera.width / self.window_size.0 as f64;
        self.hover_effect
            .update_stroke_width(width, &mut self.world, self.renderer.gl());

        self.renderer.render(&mut self.world, &self.camera);
        self.renderer.check_gl_error("Scene render");
        self.needs_render = false;
        true // Frame was rendered
    }

    pub fn resize(&mut self, physical_width: u32, physical_height: u32) {
        self.window_size = (physical_width, physical_height);
        self.renderer.set_viewport(Viewport {
            left: 0.0,
            top: 0.0,
            width: physical_width as f64,
            height: physical_height as f64,
        });
        let window_aspect = physical_width as f64 / physical_height as f64;
        self.camera.height = self.camera.width / window_aspect;

        self.renderer.render(&mut self.world, &self.camera);
        self.renderer.check_gl_error("Scene render");
    }

    pub fn destroy(&mut self) {
        for mut geo in self.queries.geometries.iter_mut(&mut self.world) {
            geo.destroy(self.renderer.gl());
        }

        for mut mat in self.queries.materials.iter_mut(&mut self.world) {
            mat.destroy(self.renderer.gl());
        }

        // TODO: despawn the entities too
    }

    pub fn apply_theme(&mut self, theme: &Theme) {
        let mut count = 0;
        for layer in self.queries.layers.iter(&self.world) {
            if !layer.shape_instances.is_empty() {
                count += 1;
            }
        }
        let alpha = 1.0 / (count as f32);

        let mut layer_meshes = Vec::new();
        for (_, mut layer) in self.queries.mut_layers.iter_mut(&mut self.world) {
            layer.color.w = alpha;
            layer_meshes.push((layer.mesh, layer.visible, layer.color.w));
        }

        for (mesh, visible, alpha) in layer_meshes {
            self.update_layer_mesh(mesh, visible, alpha);
        }

        let material = self.queries.layer_material.single_mut(&mut self.world);

        match theme {
            Theme::Light => {
                self.renderer.set_clear_color(1.0, 1.0, 1.0, 1.0);
                if let Ok((mut material, _)) = material {
                    material.set_blending(BlendMode::Subtractive);
                }
            }
            Theme::Dark => {
                self.renderer.set_clear_color(0.0, 0.0, 0.0, 1.0);
                if let Ok((mut material, _)) = material {
                    material.set_blending(BlendMode::Additive);
                }
            }
        }

        self.render();
    }

    fn update_layer_mesh(&mut self, mesh: Entity, visible: bool, alpha: f32) {
        let color = Vector4f::new(alpha, alpha, alpha, 1.0);
        let mut mesh = self.world.get_mut::<Mesh>(mesh).unwrap();
        mesh.set_vec4("color", color);
        mesh.visible = visible;
    }

    pub fn create_layer_proxies(&mut self) -> Vec<LayerProxy> {
        let mut layer_proxies = Vec::new();
        for (entity, layer) in self.queries.mut_layers.iter(&self.world) {
            layer_proxies.push(LayerProxy::from_layer(entity, layer));
        }
        layer_proxies
    }

    pub fn update_layer(&mut self, layer_proxy: LayerProxy) {
        let mut layer = self
            .queries
            .mut_layers
            .get_mut(&mut self.world, layer_proxy.entity)
            .unwrap()
            .1;
        layer_proxy.to_layer(&mut layer);

        let mesh = layer.mesh;
        let visible = layer.visible;
        let alpha = layer.color.w;

        self.update_layer_mesh(mesh, visible, alpha);
    }

    fn pick_cell(&self, x: f64, y: f64) -> Option<RTreeItem> {
        let point = geo::Point::new(x, y);
        let items = self.rtree.locate_all_at_point(&point);
        let mut result: Option<RTreeItem> = None;
        let mut result_layer_index = -i16::MAX;

        // Of all items whose AABB overlaps the query point, pick the one with
        // the highest layer index, but only if its layer is visible, and if its
        // polygon actually contains the point.

        for item in items {
            let shape_instance = self
                .world
                .get::<ShapeInstance>(item.shape_instance)
                .unwrap();

            if shape_instance.layer_index < result_layer_index {
                continue;
            }

            let layer = self.world.get::<Layer>(shape_instance.layer).unwrap();

            if !layer.visible {
                continue;
            }

            if !shape_instance.world_polygon.contains(&point) {
                continue;
            }

            result = Some(item.clone());
            result_layer_index = shape_instance.layer_index;
        }
        result
    }

    fn screen_to_world(&self, screen_x: u32, screen_y: u32) -> (f64, f64) {
        let ndc_x = (screen_x as f64 / self.window_size.0 as f64) * 2.0 - 1.0;
        let ndc_y = -((screen_y as f64 / self.window_size.1 as f64) * 2.0 - 1.0);
        let world = self.camera.unproject(Point3d::new(ndc_x, ndc_y, 0.0));
        (world.x, world.y)
    }
}

impl Drop for AppController {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Bundles all query objects used by the AppController
struct QueryBundle {
    mut_layers: QueryState<(Entity, &'static mut Layer)>,
    layers: QueryState<&'static Layer>,
    shapes: QueryState<(Entity, &'static ShapeInstance)>,
    geometries: QueryState<&'static mut Geometry>,
    materials: QueryState<&'static mut Material>,
    layer_material: QueryState<(&'static mut Material, &'static LayerMaterial)>,
}

impl QueryBundle {
    fn new(world: &mut World) -> Self {
        Self {
            mut_layers: QueryState::new(world),
            layers: QueryState::new(world),
            shapes: QueryState::new(world),
            geometries: QueryState::new(world),
            materials: QueryState::new(world),
            layer_material: QueryState::new(world),
        }
    }

    fn update(&mut self, world: &mut World) {
        *self = Self::new(world);
    }
}
