use super::ribbon_shaders::FRAGMENT_SHADER;
use super::ribbon_shaders::VERTEX_SHADER;
use super::Geometry;
use super::GeometryId;
use super::Material;
use super::Mesh;
use super::MeshId;
use super::Scene;

type Point2d = nalgebra::Point2<f64>;
type Vector2d = nalgebra::Vector2<f64>;
type Vector4f = nalgebra::Vector4<f32>;

pub struct Ribbon {
    mesh: MeshId,
    geometry: GeometryId,
    pub width: f64,
    pub closed: bool,
}

impl Ribbon {
    pub fn new(scene: &mut Scene) -> Self {
        let geometry = Geometry::new();
        let geometry_id = scene.add_geometry(geometry);

        let material = Material::new(VERTEX_SHADER, FRAGMENT_SHADER);
        let material_id = scene.add_material(material);

        let mut mesh = Mesh::new(geometry_id, material_id);
        mesh.visible = false;
        mesh.set_vec4("color", Vector4f::new(1.0, 1.0, 1.0, 1.0));
        let mesh_id = scene.add_mesh(mesh);

        Self {
            mesh: mesh_id,
            geometry: geometry_id,
            width: 5000.0,
            closed: false,
        }
    }

    pub fn hide(&self, scene: &mut Scene) {
        scene.get_mesh_mut(&self.mesh).unwrap().visible = false;
    }

    pub fn show(&self, scene: &mut Scene) {
        scene.get_mesh_mut(&self.mesh).unwrap().visible = true;
    }

    pub fn mesh(&self) -> MeshId {
        self.mesh
    }

    pub fn update_geometry(&self, scene: &mut Scene, gl: &glow::Context, points: &[Point2d]) {
        if points.len() < 2 {
            scene.get_mesh_mut(&self.mesh).unwrap().visible = false;
            return;
        }

        scene.get_mesh_mut(&self.mesh).unwrap().visible = true;

        let mut positions = Vec::new();
        let mut indices = Vec::new();

        // Helper function to add a 3D point to positions
        let add_point = |positions: &mut Vec<f32>, p: Point2d| {
            positions.extend_from_slice(&[p.x as f32, p.y as f32, 0.0]);
        };

        // Helper function to add a triangle to indices
        let add_triangle = |indices: &mut Vec<u32>, a: u32, b: u32, c: u32| {
            indices.extend_from_slice(&[a, b, c]);
        };

        // Calculate mitered points for each segment
        for i in 0..points.len() {
            let prev = if i == 0 {
                if self.closed {
                    points[points.len() - 1]
                } else {
                    points[0]
                }
            } else {
                points[i - 1]
            };

            let curr = points[i];
            let next = if i == points.len() - 1 {
                if self.closed {
                    points[0]
                } else {
                    points[i]
                }
            } else {
                points[i + 1]
            };

            // Calculate direction vectors
            let dir1 = Vector2d::new(curr.x - prev.x, curr.y - prev.y).normalize();
            let dir2 = Vector2d::new(next.x - curr.x, next.y - curr.y).normalize();

            // Calculate miter direction
            let miter_dir = (dir1 + dir2).normalize();
            let miter_length = self.width / (1.0 + dir1.dot(&dir2)).sqrt();

            // Calculate offset points
            let offset1 = Point2d::new(
                curr.x + miter_dir.x * miter_length,
                curr.y + miter_dir.y * miter_length,
            );
            let offset2 = Point2d::new(
                curr.x - miter_dir.x * miter_length,
                curr.y - miter_dir.y * miter_length,
            );

            // Add points and create triangles
            let base_idx = positions.len() as u32 / 3;
            add_point(&mut positions, offset1);
            add_point(&mut positions, offset2);

            if i > 0 {
                add_triangle(&mut indices, base_idx - 2, base_idx, base_idx - 1);
                add_triangle(&mut indices, base_idx - 1, base_idx, base_idx + 1);
            }
        }

        // Create new geometry with the calculated data
        let mut new_geometry = Geometry::new();
        new_geometry.positions = positions;
        new_geometry.indices = indices;

        // Replace the geometry in the scene
        scene.replace_geometry(gl, self.geometry, new_geometry);
    }
}
