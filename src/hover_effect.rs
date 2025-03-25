use crate::app_shaders::FRAGMENT_SHADER;
use crate::app_shaders::VERTEX_SHADER;

use crate::core::PickResult;
use crate::graphics::Geometry;
use crate::graphics::Material;
use crate::graphics::Mesh;
use crate::graphics::MeshId;
use crate::graphics::Ribbon;
use crate::graphics::Scene;
use crate::Project;

use geo::TriangulateEarcut;

type Point2 = nalgebra::Point2<f64>;

// TODO: this could hold Scene and Renderer refs because it has the same lifetime
pub struct HoverEffect {
    cell: Option<PickResult>,
    mesh: MeshId,
    ribbon: Ribbon,
}

impl HoverEffect {
    pub fn new(scene: &mut Scene) -> Self {
        let mut outline_material = Material::new(VERTEX_SHADER, FRAGMENT_SHADER);
        outline_material.set_blending(true);
        let outline_material_id = scene.add_material(outline_material);

        let outline_geometry = Geometry::new();
        let outline_geometry_id = scene.add_geometry(outline_geometry);

        let mut outline_mesh = Mesh::new(outline_geometry_id, outline_material_id);
        outline_mesh.visible = false;

        let mesh = scene.add_mesh(outline_mesh);
        let ribbon = Ribbon::new(scene);

        Self {
            cell: None,
            mesh,
            ribbon,
        }
    }

    pub fn has_cell(&self, hit: &PickResult) -> bool {
        self.cell == Some(hit.clone())
    }

    pub fn cell(&self) -> Option<PickResult> {
        self.cell.clone()
    }

    pub fn move_to_back(&mut self, scene: &mut Scene) {
        scene.move_mesh_to_back(self.mesh);
        scene.move_mesh_to_back(self.ribbon.mesh());
    }

    pub fn is_active(&self) -> bool {
        self.cell.is_some()
    }

    pub fn hide(&mut self, scene: &mut Scene) {
        self.cell = None;
        let mesh = scene.get_mesh_mut(&self.mesh).unwrap();
        mesh.visible = false;
        self.ribbon.hide(scene);
    }

    pub fn update(
        &mut self,
        selection: PickResult,
        project: &Project,
        scene: &mut Scene,
        gl: &glow::Context,
    ) {
        self.cell = Some(selection.clone());

        let layer = &project.layers()[selection.layer as usize];
        let polygon = &layer.polygons[selection.polygon];

        let triangles = polygon.earcut_triangles_raw();

        let mut color = layer.color;
        color.w = 1.0;

        let mut points = Vec::new();
        for coord in polygon.exterior().points() {
            points.push(Point2::new(coord.x(), coord.y()));
        }

        self.ribbon.update_geometry(scene, gl, &points);

        let mut geometry = Geometry::new();

        geometry.positions.reserve(3 * triangles.vertices.len() / 2);
        geometry.indices.reserve(triangles.triangle_indices.len());

        for coord in triangles.vertices.chunks(2) {
            geometry.positions.push(coord[0] as f32);
            geometry.positions.push(coord[1] as f32);
            geometry.positions.push(0.0);
        }

        for index in triangles.triangle_indices {
            geometry.indices.push(index as u32);
        }

        let mesh = scene.get_mesh_mut(&self.mesh).unwrap();
        mesh.visible = true;
        mesh.set_vec4("color", color);
        let geometry_id = mesh.geometry_id;
        scene.replace_geometry(gl, geometry_id, geometry);
    }
}
