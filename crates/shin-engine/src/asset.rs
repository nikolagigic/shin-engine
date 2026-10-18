//! glTF and texture loading into CPU-side structs. Owned by GE-005.
//!
//! Loading produces plain Rust data; the renderer (GE-006) uploads
//! [`MeshData`] and [`ImageData`] to the GPU. The one exception is the
//! [`Assets`] resource (GE-019), which maps stable [`AssetSource`] names
//! (`builtin://cube`, `assets/x.gltf#2`) to uploaded meshes.
//!
//! Loading flattens a glTF document:
//!
//! - every triangle primitive becomes one [`MeshData`] (other primitive
//!   modes are skipped with a warning);
//! - every node becomes one [`NodeData`] per primitive of its mesh (or one
//!   [`NodeData`] with `mesh: None` if it has no mesh), carrying its world
//!   transform (parent × local, baked);
//! - indices are widened to `u32`, or generated as `0..n` when absent;
//! - missing normals are replaced by flat per-triangle normals, which
//!   duplicates vertices so each triangle owns its three corners.
//!
//! Tools (GE-015, GE-019): [`register_tools`] adds `asset.load`,
//! `scene.load_gltf`, and `mesh.builtin`, which all upload through
//! [`Assets`] into the renderer's mesh registry; and (GE-032) `asset.list`,
//! which returns what [`scan`] finds in the project.

mod registry;
mod scan;
mod tools;

use std::f32::consts::PI;
use std::path::Path;

use anyhow::Context;
use glam::{Mat4, Vec3};

use crate::math::Transform;

pub use registry::{AssetSource, Assets};
pub use scan::{AssetEntry, AssetKind, EntrySource, SCANNED_DIRS, scan};
pub use tools::register_tools;
pub(crate) use tools::with_assets;

/// One mesh vertex as the renderer consumes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Object-space unit normal.
    pub normal: [f32; 3],
    /// Texture coordinate (glTF convention: origin top-left, v down).
    pub uv: [f32; 2],
}

/// An indexed triangle list.
#[derive(Clone, Debug)]
pub struct MeshData {
    /// Name of the glTF mesh this primitive came from (may be empty).
    pub name: String,
    /// Vertex data.
    pub vertices: Vec<Vertex>,
    /// Triangle list indices into `vertices`, three per triangle.
    pub indices: Vec<u32>,
    /// Index into [`SceneData::materials`], if the primitive has a material.
    pub material: Option<usize>,
}

/// The base-colour part of a glTF PBR material.
#[derive(Clone, Debug)]
pub struct MaterialData {
    /// Material name (may be empty).
    pub name: String,
    /// Linear RGBA base colour factor; `[1, 1, 1, 1]` when unspecified.
    pub base_color: [f32; 4],
    /// Index into [`SceneData::images`] of the base colour texture, if any.
    pub base_color_texture: Option<usize>,
}

/// A decoded image, always 8-bit RGBA.
#[derive(Clone, Debug)]
pub struct ImageData {
    /// Image name, or the file stem for images loaded from disk.
    pub name: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Everything loaded from one glTF file, flattened.
#[derive(Clone, Debug, Default)]
pub struct SceneData {
    /// One entry per glTF triangle primitive.
    pub meshes: Vec<MeshData>,
    /// Materials, in glTF order.
    pub materials: Vec<MaterialData>,
    /// Images, in glTF order, decoded to RGBA8.
    pub images: Vec<ImageData>,
    /// Nodes with their world transforms baked in.
    pub nodes: Vec<NodeData>,
}

/// A placed instance of a mesh (or an empty node).
#[derive(Clone, Debug)]
pub struct NodeData {
    /// Node name (may be empty).
    pub name: String,
    /// Index into [`SceneData::meshes`], if the node draws something.
    pub mesh: Option<usize>,
    /// World transform: the product of all ancestor transforms and this
    /// node's local transform.
    pub transform: Transform,
}

/// Loads a glTF 2.0 file (`.gltf` with embedded or sidecar buffers, or
/// `.glb`) and its images.
///
/// The error message contains `path` if anything fails.
pub fn load_gltf(path: impl AsRef<Path>) -> anyhow::Result<SceneData> {
    let path = path.as_ref();
    let import =
        gltf::import(path).with_context(|| format!("failed to load glTF {}", path.display()))?;
    scene_from_import(import).with_context(|| format!("invalid glTF {}", path.display()))
}

/// Loads a PNG or JPEG file and converts it to RGBA8.
///
/// The error message contains `path` if anything fails.
pub fn load_image(path: impl AsRef<Path>) -> anyhow::Result<ImageData> {
    let path = path.as_ref();
    let image = image::open(path)
        .with_context(|| format!("failed to load image {}", path.display()))?
        .to_rgba8();
    Ok(ImageData {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

/// Unit cube centred at the origin (extent -0.5..0.5): 24 vertices (four per
/// face, so each face has its own normal and UVs) and 36 indices,
/// counter-clockwise viewed from outside.
pub fn cube() -> MeshData {
    // Each face: outward normal `n` and in-plane axes `u`, `v` with u × v = n,
    // so corners listed (-u,-v), (+u,-v), (+u,+v), (-u,+v) are CCW from outside.
    let faces = [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ];
    let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];

    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (n, u, v) in faces {
        let base = vertices.len() as u32;
        for ((a, b), uv) in corners.into_iter().zip(uvs) {
            vertices.push(Vertex {
                position: (0.5 * (n + a * u + b * v)).to_array(),
                normal: n.to_array(),
                uv,
            });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    MeshData {
        name: "cube".into(),
        vertices,
        indices,
        material: None,
    }
}

/// Unit-radius UV sphere centred at the origin, counter-clockwise viewed from
/// outside.
///
/// `segments` divides the equator (clamped to at least 3) and `rings`
/// divides pole to pole (clamped to at least 2). Each ring has
/// `segments + 1` vertices so the UV seam is not shared; degenerate
/// triangles at the poles are omitted.
pub fn uv_sphere(segments: u32, rings: u32) -> MeshData {
    let segments = segments.max(3);
    let rings = rings.max(2);
    let mut vertices = Vec::with_capacity(((rings + 1) * (segments + 1)) as usize);
    for r in 0..=rings {
        // Polar angle from +Y.
        let phi = PI * r as f32 / rings as f32;
        for s in 0..=segments {
            // Azimuth from +Z towards +X.
            let theta = 2.0 * PI * s as f32 / segments as f32;
            let p = Vec3::new(phi.sin() * theta.sin(), phi.cos(), phi.sin() * theta.cos());
            vertices.push(Vertex {
                position: p.to_array(),
                normal: p.to_array(),
                uv: [s as f32 / segments as f32, r as f32 / rings as f32],
            });
        }
    }

    let stride = segments + 1;
    let mut indices = Vec::new();
    for r in 0..rings {
        for s in 0..segments {
            let a = r * stride + s; // top-left
            let b = a + stride; // bottom-left
            let c = b + 1; // bottom-right
            let d = a + 1; // top-right
            if r != rings - 1 {
                indices.extend([a, b, c]);
            }
            if r != 0 {
                indices.extend([a, c, d]);
            }
        }
    }
    MeshData {
        name: "uv_sphere".into(),
        vertices,
        indices,
        material: None,
    }
}

/// Converts an imported glTF document into [`SceneData`]. Separate from
/// [`load_gltf`] so tests can import from in-memory bytes.
fn scene_from_import(
    (document, buffers, images): (
        gltf::Document,
        Vec<gltf::buffer::Data>,
        Vec<gltf::image::Data>,
    ),
) -> anyhow::Result<SceneData> {
    let mut scene = SceneData {
        images: document
            .images()
            .zip(images)
            .map(|(image, data)| convert_image(image.name().unwrap_or_default(), data))
            .collect::<anyhow::Result<_>>()?,
        materials: document.materials().map(convert_material).collect(),
        ..SceneData::default()
    };

    // glTF mesh index -> indices into `scene.meshes` (one per primitive).
    let mut mesh_primitives = Vec::with_capacity(document.meshes().len());
    for mesh in document.meshes() {
        let name = mesh.name().unwrap_or_default();
        let mut ids = Vec::new();
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                log::warn!("skipping {:?} primitive in mesh {name:?}", primitive.mode());
                continue;
            }
            ids.push(scene.meshes.len());
            scene
                .meshes
                .push(convert_primitive(name, &primitive, &buffers)?);
        }
        mesh_primitives.push(ids);
    }

    // Roots are nodes that are nobody's child; this covers every node
    // whether or not it is listed in a glTF scene.
    let mut is_child = vec![false; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            is_child[child.index()] = true;
        }
    }
    for root in document.nodes().filter(|n| !is_child[n.index()]) {
        add_node(&mut scene.nodes, &mesh_primitives, &root, Mat4::IDENTITY);
    }
    Ok(scene)
}

/// Appends `node` and its descendants with world transforms baked.
fn add_node(
    out: &mut Vec<NodeData>,
    mesh_primitives: &[Vec<usize>],
    node: &gltf::Node,
    parent: Mat4,
) {
    let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
    let (scale, rotation, translation) = world.to_scale_rotation_translation();
    let transform = Transform {
        translation,
        rotation,
        scale,
    };
    let name = node.name().unwrap_or_default().to_string();
    let meshes = node.mesh().map(|m| &mesh_primitives[m.index()][..]);
    match meshes {
        Some(ids) if !ids.is_empty() => {
            for &id in ids {
                out.push(NodeData {
                    name: name.clone(),
                    mesh: Some(id),
                    transform,
                });
            }
        }
        _ => out.push(NodeData {
            name,
            mesh: None,
            transform,
        }),
    }
    for child in node.children() {
        add_node(out, mesh_primitives, &child, world);
    }
}

fn convert_primitive(
    name: &str,
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
) -> anyhow::Result<MeshData> {
    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|d| &d.0[..]));
    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .with_context(|| format!("mesh {name:?} has a primitive without positions"))?
        .collect();
    let mut uvs = reader
        .read_tex_coords(0)
        .map(|t| t.into_f32().collect::<Vec<_>>())
        .unwrap_or_default();
    uvs.resize(positions.len(), [0.0, 0.0]);
    let indices: Vec<u32> = match reader.read_indices() {
        Some(indices) => indices.into_u32().collect(),
        None => (0..positions.len() as u32).collect(),
    };
    if let Some(&bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
        anyhow::bail!("mesh {name:?} has out-of-range index {bad}");
    }

    let (vertices, indices) = match reader.read_normals() {
        Some(normals) => {
            let vertices = positions
                .iter()
                .zip(normals)
                .zip(&uvs)
                .map(|((&position, normal), &uv)| Vertex {
                    position,
                    normal,
                    uv,
                })
                .collect();
            (vertices, indices)
        }
        None => flat_shaded(&positions, &uvs, &indices),
    };
    Ok(MeshData {
        name: name.to_string(),
        vertices,
        indices,
        material: primitive.material().index(),
    })
}

/// Duplicates vertices so every triangle has its own corners, each carrying
/// the triangle's face normal. Returns the new vertices and indices `0..n`.
fn flat_shaded(
    positions: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(indices.len());
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = [0, 1, 2].map(|k| Vec3::from(positions[tri[k] as usize]));
        let normal = (b - a).cross(c - a).normalize_or_zero().to_array();
        for &i in tri {
            vertices.push(Vertex {
                position: positions[i as usize],
                normal,
                uv: uvs[i as usize],
            });
        }
    }
    let indices = (0..vertices.len() as u32).collect();
    (vertices, indices)
}

fn convert_material(material: gltf::Material) -> MaterialData {
    let pbr = material.pbr_metallic_roughness();
    MaterialData {
        name: material.name().unwrap_or_default().to_string(),
        base_color: pbr.base_color_factor(),
        base_color_texture: pbr
            .base_color_texture()
            .map(|t| t.texture().source().index()),
    }
}

/// Converts any pixel format the glTF importer produces to RGBA8. Missing
/// colour channels are filled from red (grey images), missing alpha is 255.
fn convert_image(name: &str, data: gltf::image::Data) -> anyhow::Result<ImageData> {
    use gltf::image::Format;
    // (channels, bytes per channel)
    let (channels, channel_bytes) = match data.format {
        Format::R8 => (1, 1),
        Format::R8G8 => (2, 1),
        Format::R8G8B8 => (3, 1),
        Format::R8G8B8A8 => (4, 1),
        Format::R16 => (1, 2),
        Format::R16G16 => (2, 2),
        Format::R16G16B16 => (3, 2),
        Format::R16G16B16A16 => (4, 2),
        Format::R32G32B32FLOAT => (3, 4),
        Format::R32G32B32A32FLOAT => (4, 4),
    };
    let channel = |bytes: &[u8]| -> u8 {
        match bytes.len() {
            1 => bytes[0],
            2 => (u16::from_ne_bytes([bytes[0], bytes[1]]) >> 8) as u8,
            _ => {
                let f = f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                (f.clamp(0.0, 1.0) * 255.0).round() as u8
            }
        }
    };
    let pixel_count = data.width as usize * data.height as usize;
    anyhow::ensure!(
        data.pixels.len() == pixel_count * channels * channel_bytes,
        "image {name:?} has {} bytes, expected {}",
        data.pixels.len(),
        pixel_count * channels * channel_bytes
    );
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel in data.pixels.chunks_exact(channels * channel_bytes) {
        let c: Vec<u8> = pixel.chunks_exact(channel_bytes).map(channel).collect();
        rgba.extend(match channels {
            1 => [c[0], c[0], c[0], 255],
            2 => [c[0], c[0], c[0], c[1]],
            3 => [c[0], c[1], c[2], 255],
            _ => [c[0], c[1], c[2], c[3]],
        });
    }
    Ok(ImageData {
        name: name.to_string(),
        width: data.width,
        height: data.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_ccw_from_outside(mesh: &MeshData) {
        for tri in mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = [0, 1, 2].map(|k| Vec3::from(mesh.vertices[tri[k] as usize].position));
            let face = (b - a).cross(c - a);
            for &i in tri {
                let n = Vec3::from(mesh.vertices[i as usize].normal);
                assert!(
                    face.dot(n) > 0.0,
                    "triangle {tri:?} is not CCW from outside"
                );
            }
        }
    }

    #[test]
    fn cube_has_24_vertices_and_36_indices() {
        let cube = cube();
        assert_eq!(cube.vertices.len(), 24);
        assert_eq!(cube.indices.len(), 36);
    }

    #[test]
    fn cube_normals_are_axis_aligned() {
        for v in cube().vertices {
            let n = Vec3::from(v.normal);
            let abs = n.abs();
            assert_eq!(
                abs.max_element(),
                1.0,
                "normal {n} is not unit axis-aligned"
            );
            assert_eq!(abs.element_sum(), 1.0, "normal {n} is not axis-aligned");
            // The vertex lies on the face the normal points out of.
            assert_eq!(Vec3::from(v.position).dot(n), 0.5);
        }
    }

    #[test]
    fn cube_winding_is_ccw_from_outside() {
        assert_ccw_from_outside(&cube());
    }

    #[test]
    fn uv_sphere_vertices_are_on_unit_sphere_with_radial_normals() {
        let sphere = uv_sphere(16, 8);
        assert_eq!(sphere.vertices.len(), 17 * 9);
        for v in &sphere.vertices {
            let p = Vec3::from(v.position);
            assert!(
                (p.length() - 1.0).abs() <= 1e-5,
                "{p} is off the unit sphere"
            );
            assert!(Vec3::from(v.normal).abs_diff_eq(p.normalize(), 1e-6));
        }
    }

    #[test]
    fn uv_sphere_winding_is_ccw_from_outside() {
        let sphere = uv_sphere(16, 8);
        // 16 * 8 quads, minus one triangle per quad on each polar ring.
        assert_eq!(sphere.indices.len(), 3 * (2 * 16 * 8 - 2 * 16));
        assert_ccw_from_outside(&sphere);
    }

    fn import_json(json: &str) -> SceneData {
        scene_from_import(gltf::import_slice(json.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn child_translation_is_scaled_by_parent() {
        let scene = import_json(
            r#"{
                "asset": { "version": "2.0" },
                "scenes": [{ "nodes": [0] }],
                "nodes": [
                    { "name": "parent", "scale": [2, 2, 2], "children": [1] },
                    { "name": "child", "translation": [1, 2, 3] }
                ]
            }"#,
        );
        assert_eq!(scene.nodes.len(), 2);
        let child = scene.nodes.iter().find(|n| n.name == "child").unwrap();
        assert!(
            child
                .transform
                .translation
                .abs_diff_eq(Vec3::new(2.0, 4.0, 6.0), 1e-6)
        );
        assert!(child.transform.scale.abs_diff_eq(Vec3::splat(2.0), 1e-6));
    }

    #[test]
    fn missing_normals_become_flat_and_missing_indices_are_generated() {
        // One CCW triangle in the XY plane, positions only, no indices.
        let bytes: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64_encode(&bytes)
        );
        let scene = import_json(&format!(
            r#"{{
                "asset": {{ "version": "2.0" }},
                "buffers": [{{ "byteLength": 36, "uri": "{uri}" }}],
                "bufferViews": [{{ "buffer": 0, "byteLength": 36 }}],
                "accessors": [{{
                    "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                    "min": [0, 0, 0], "max": [1, 1, 0]
                }}],
                "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0 }} }}] }}],
                "nodes": [{{ "mesh": 0 }}]
            }}"#
        ));
        let mesh = &scene.meshes[0];
        assert_eq!(mesh.indices, [0, 1, 2]);
        for v in &mesh.vertices {
            assert_eq!(v.normal, [0.0, 0.0, 1.0]);
        }
        assert_eq!(scene.nodes[0].mesh, Some(0));
        assert_eq!(scene.nodes[0].transform, Transform::IDENTITY);
    }

    /// Minimal base64 encoder so the test can build a data URI without
    /// adding a dependency.
    fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            for k in 0..4 {
                if k <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * k) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn rgb_and_grey_images_expand_to_rgba() {
        let grey = gltf::image::Data {
            pixels: vec![10, 20],
            format: gltf::image::Format::R8,
            width: 2,
            height: 1,
        };
        assert_eq!(
            convert_image("g", grey).unwrap().rgba,
            [10, 10, 10, 255, 20, 20, 20, 255]
        );
        let rgb = gltf::image::Data {
            pixels: vec![1, 2, 3],
            format: gltf::image::Format::R8G8B8,
            width: 1,
            height: 1,
        };
        assert_eq!(convert_image("c", rgb).unwrap().rgba, [1, 2, 3, 255]);
    }
}
