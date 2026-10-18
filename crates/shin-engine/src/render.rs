//! Mesh upload, materials, WGSL shaders, instanced drawing.
//! Owned by GE-006 (single mesh) and GE-008 (instancing).
//!
//! [`GpuMesh::upload`] copies a CPU-side [`MeshData`] into vertex and index
//! buffers. [`MeshRenderer`] owns the lit pipeline (`render/mesh.wgsl`):
//! a camera, one directional light, a base colour, and an instance buffer
//! of [`InstanceRaw`] model matrices. Every draw is instanced, even when
//! there is only one instance.
//!
//! The pipeline renders into any colour format (use an sRGB one; the shader
//! outputs linear colour) with a [`DEPTH_FORMAT`] depth attachment.
//! [`begin_mesh_pass`] opens a pass with the matching attachments.
//!
//! The renderer has one instance buffer per frame: upload every batch back
//! to back with [`MeshRenderer::upload_instances`], then issue one
//! [`MeshRenderer::draw`] per batch with its index range.
//!
//! ECS integration (GE-008): entities with a [`Transform`] and a
//! [`MeshInstance`] component are drawn by [`render_world`], which groups
//! them by [`MeshHandle`] and [`TextureHandle`] into one instanced draw call
//! per (mesh, texture) pair. Meshes live in the [`MeshRegistry`] and
//! textures in the [`TextureRegistry`] inside the [`RenderState`] resource.
//! An entity with a [`GlobalTransform`] (GE-031: every entity with a
//! transform, once the hierarchy has been propagated) is drawn where that
//! says; otherwise its `Transform` is taken as world space.
//!
//! Textures (GE-025): the shader multiplies a base-colour texture
//! (`@group(1)`: texture at binding 0, the renderer's one sampler at
//! binding 1), sampled at the vertex `uv`, into the material and instance
//! colours. Untextured draws bind a 1×1 white texture, so one pipeline
//! serves both and untextured output is unchanged. Textures are
//! [`TEXTURE_FORMAT`] (sRGB), so samples come back linear.
//!
//! Tools (GE-015): [`register_tools`] adds `mesh.list`, `camera.set`,
//! `light.set`, and `render.frame`.

mod tools;

use std::collections::BTreeMap;
use std::ops::Range;

use wgpu::util::DeviceExt as _;

use crate::asset::{AssetSource, ImageData, MeshData, Vertex};
use crate::ecs::{GlobalTransform, Resources, World};
use crate::gpu::{DEPTH_FORMAT, GpuContext};
use crate::math::{Camera, Mat3, Transform};

pub use tools::register_tools;

/// A mesh living in GPU buffers, ready to draw with [`MeshRenderer`].
pub struct GpuMesh {
    /// [`Vertex`] data (position, normal, uv).
    pub vertex_buffer: wgpu::Buffer,
    /// `u32` triangle-list indices.
    pub index_buffer: wgpu::Buffer,
    /// Number of indices in [`Self::index_buffer`].
    pub index_count: u32,
    /// Number of vertices in [`Self::vertex_buffer`].
    pub vertex_count: u32,
    /// Name of the [`MeshData`] this was uploaded from (may be empty).
    pub name: String,
}

impl GpuMesh {
    /// Uploads `mesh`'s vertices and indices into new GPU buffers.
    pub fn upload(device: &wgpu::Device, mesh: &MeshData) -> Self {
        let label = |kind: &str| format!("mesh '{}' {kind}", mesh.name);
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&label("vertices")),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&label("indices")),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Self {
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            vertex_count: mesh.vertices.len() as u32,
            name: mesh.name.clone(),
        }
    }
}

/// Format of every base-colour texture. glTF base colour is sRGB-encoded,
/// so the GPU decodes samples to linear, matching the shader's colour space.
pub const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The `@group(1)` bind group layout and the one sampler every texture
/// bind group shares. Cheap to clone (wgpu handles are reference counted).
#[derive(Clone)]
struct TextureBinder {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl TextureBinder {
    /// The layout (texture at binding 0, filtering sampler at binding 1)
    /// and a linear, repeating sampler without mipmaps.
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("base colour sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { layout, sampler }
    }

    /// Uploads `image` as a [`TEXTURE_FORMAT`] texture and binds it with
    /// the shared sampler.
    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, image: &ImageData) -> GpuTexture {
        let label = format!("texture '{}'", image.name);
        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some(&label),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEXTURE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &image.rgba,
        );
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&label),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        GpuTexture {
            texture,
            view,
            bind_group,
            name: image.name.clone(),
        }
    }
}

/// A base-colour texture living on the GPU, bound and ready for
/// [`MeshRenderer::draw_textured`].
pub struct GpuTexture {
    /// The [`TEXTURE_FORMAT`] texture, one mip level.
    pub texture: wgpu::Texture,
    /// A view of the whole texture.
    pub view: wgpu::TextureView,
    /// `@group(1)`: [`Self::view`] plus the renderer's sampler.
    bind_group: wgpu::BindGroup,
    /// Name of the [`ImageData`] this was uploaded from (may be empty).
    pub name: String,
}

impl GpuTexture {
    /// Uploads `image` (RGBA8, treated as sRGB) for drawing with
    /// `renderer`.
    ///
    /// # Panics
    ///
    /// If `image.rgba` is not `width * height * 4` bytes, or a dimension is
    /// zero or exceeds the device's limits (wgpu validation).
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &MeshRenderer,
        image: &ImageData,
    ) -> Self {
        renderer.binder.upload(device, queue, image)
    }

    /// Width and height in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.texture.width(), self.texture.height())
    }
}

/// Camera data for the shader (`@group(0) @binding(0)`).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    /// World-to-clip matrix, column-major.
    pub view_proj: [[f32; 4]; 4],
    /// World-space eye position in `xyz`; `w` is unused.
    pub camera_pos: [f32; 4],
}

impl CameraUniform {
    /// The uniform for `cam` rendering into a viewport of `aspect` (width / height).
    pub fn new(cam: &Camera, aspect: f32) -> Self {
        Self {
            view_proj: cam.view_projection(aspect).to_cols_array_2d(),
            camera_pos: cam.transform.translation.extend(1.0).to_array(),
        }
    }
}

/// One directional light plus ambient (`@group(0) @binding(1)`). Linear RGB.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightUniform {
    /// Direction the light travels, from the light toward the scene, in
    /// `xyz` (need not be normalised); `w` is unused. The shader lights a
    /// surface by `N · -direction`, so `[0, -1, 0, 0]` shines straight down.
    pub direction: [f32; 4],
    /// Light colour in `rgb`; `a` is unused.
    pub color: [f32; 4],
    /// Ambient light in `rgb`, added regardless of orientation; `a` is unused.
    pub ambient: [f32; 4],
}

impl Default for LightUniform {
    /// White light shining straight down, with a dim ambient term.
    fn default() -> Self {
        Self {
            direction: [0.0, -1.0, 0.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            ambient: [0.1, 0.1, 0.1, 1.0],
        }
    }
}

/// Material data (`@group(0) @binding(2)`). Only a base colour for now.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUniform {
    base_color: [f32; 4],
}

/// Per-instance data, read from vertex buffer slot 1.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceRaw {
    /// Model (local-to-world) matrix, column-major.
    pub model: [[f32; 4]; 4],
    /// Normal matrix (inverse-transpose of the model's upper 3x3): three
    /// columns, each padded to four floats.
    pub normal_mat: [[f32; 4]; 3],
    /// Linear RGBA colour, multiplied into the material base colour.
    pub color: [f32; 4],
}

/// Instance attributes: model matrix at locations 3..=6, normal matrix at
/// 7..=9, colour at 10. Locations 0..=2 are the [`Vertex`] attributes.
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
    3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4,
    7 => Float32x4, 8 => Float32x4, 9 => Float32x4,
    10 => Float32x4,
];

/// [`Vertex`] attributes: position, normal, uv.
const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

/// Layout of a [`Vertex`] buffer (slot 0).
const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &VERTEX_ATTRIBUTES,
};

impl InstanceRaw {
    /// Instance data for an object placed at `t` with linear RGBA `color`.
    pub fn from_transform(t: &Transform, color: [f32; 4]) -> Self {
        let model = t.to_matrix();
        let normal = Mat3::from_mat4(model).inverse().transpose();
        let col = |v: crate::math::Vec3| v.extend(0.0).to_array();
        Self {
            model: model.to_cols_array_2d(),
            normal_mat: [col(normal.x_axis), col(normal.y_axis), col(normal.z_axis)],
            color,
        }
    }

    /// Vertex buffer layout of an instance buffer (slot 1, stepped per instance).
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: size_of::<InstanceRaw>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &INSTANCE_ATTRIBUTES,
    };
}

/// Draws lit, instanced, optionally textured meshes with one pipeline
/// (plus a translucent variant for [`render_overlay`]).
pub struct MeshRenderer {
    pipeline: wgpu::RenderPipeline,
    /// The same shader alpha-blended, depth-tested without depth writes.
    overlay_pipeline: wgpu::RenderPipeline,
    /// Texture layout and sampler, shared with every [`TextureRegistry`]
    /// made from this renderer.
    binder: TextureBinder,
    /// 1×1 white texture bound by [`Self::draw`].
    white: GpuTexture,
    camera_buf: wgpu::Buffer,
    light_buf: wgpu::Buffer,
    material_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instance_buf: wgpu::Buffer,
    instance_cap: usize,
}

impl MeshRenderer {
    /// Builds the pipeline for colour attachments of `color_format`. The
    /// depth attachment is always [`DEPTH_FORMAT`] (`Depth32Float`).
    ///
    /// Starts with an identity camera matrix, [`LightUniform::default`],
    /// a white base colour, and room for one instance. `queue` uploads the
    /// 1×1 white texture that untextured draws bind.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render/mesh.wgsl").into()),
        });

        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh bind group layout"),
            entries: &[uniform_entry(0), uniform_entry(1), uniform_entry(2)],
        });

        let uniform = |label, contents: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        let camera = CameraUniform {
            view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            camera_pos: [0.0, 0.0, 0.0, 1.0],
        };
        let camera_buf = uniform("camera uniform", bytemuck::bytes_of(&camera));
        let light_buf = uniform(
            "light uniform",
            bytemuck::bytes_of(&LightUniform::default()),
        );
        let material = MaterialUniform {
            base_color: [1.0; 4],
        };
        let material_buf = uniform("material uniform", bytemuck::bytes_of(&material));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mesh bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: light_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: material_buf.as_entire_binding(),
                },
            ],
        });

        let binder = TextureBinder::new(device);
        let white = binder.upload(
            device,
            queue,
            &ImageData {
                name: "white".into(),
                width: 1,
                height: 1,
                rgba: vec![255; 4],
            },
        );

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout), Some(&binder.layout)],
            immediate_size: 0,
        });

        let pipeline = |label, blend, depth_write| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(VERTEX_LAYOUT), Some(InstanceRaw::LAYOUT)],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let overlay_pipeline = pipeline(
            "mesh overlay pipeline",
            wgpu::BlendState::ALPHA_BLENDING,
            false,
        );
        let pipeline = pipeline("mesh pipeline", wgpu::BlendState::REPLACE, true);

        let instance_cap = 1;
        Self {
            pipeline,
            overlay_pipeline,
            binder,
            white,
            camera_buf,
            light_buf,
            material_buf,
            bind_group,
            instance_buf: create_instance_buffer(device, instance_cap),
            instance_cap,
        }
    }

    /// Uploads `cam`'s view-projection (for a viewport of `aspect` =
    /// width / height) and eye position.
    pub fn set_camera(&self, queue: &wgpu::Queue, cam: &Camera, aspect: f32) {
        let uniform = CameraUniform::new(cam, aspect);
        queue.write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&uniform));
    }

    /// Uploads the directional light and ambient term.
    pub fn set_light(&self, queue: &wgpu::Queue, light: &LightUniform) {
        queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(light));
    }

    /// Sets the material base colour (linear RGBA), multiplied with each
    /// instance colour. Defaults to white.
    pub fn set_base_color(&self, queue: &wgpu::Queue, base_color: [f32; 4]) {
        let uniform = MaterialUniform { base_color };
        queue.write_buffer(&self.material_buf, 0, bytemuck::bytes_of(&uniform));
    }

    /// How many instances the instance buffer holds before it must grow.
    pub fn instance_capacity(&self) -> usize {
        self.instance_cap
    }

    /// Uploads every instance drawn this frame into the single instance
    /// buffer, growing it (to the next power of two) if needed.
    ///
    /// Call once per frame, before encoding the pass, with all batches
    /// back to back; then [`Self::draw`] each batch by its index range.
    /// The data is written with `queue.write_buffer`, which takes effect
    /// at the next submit, so a second upload before submitting replaces
    /// the first.
    pub fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[InstanceRaw],
    ) {
        if instances.is_empty() {
            return;
        }
        if instances.len() > self.instance_cap {
            self.instance_cap = instances.len().next_power_of_two();
            self.instance_buf = create_instance_buffer(device, self.instance_cap);
            log::debug!("instance buffer grown to {} instances", self.instance_cap);
        }
        queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
    }

    /// The 1×1 white texture [`Self::draw`] binds.
    pub fn white_texture(&self) -> &GpuTexture {
        &self.white
    }

    /// Draws `mesh` untextured once per instance in `instances`, a range of
    /// indices into the slice last passed to [`Self::upload_instances`].
    /// Several draws with disjoint ranges can share one pass.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, mesh: &GpuMesh, instances: Range<u32>) {
        self.draw_textured(pass, mesh, &self.white, instances);
    }

    /// [`Self::draw`] with `texture` as the base-colour texture. `texture`
    /// must come from this renderer ([`GpuTexture::upload`] or a
    /// [`TextureRegistry`] made from it).
    pub fn draw_textured(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        mesh: &GpuMesh,
        texture: &GpuTexture,
        instances: Range<u32>,
    ) {
        if instances.is_empty() {
            return;
        }
        self.encode(pass, &self.pipeline, mesh, texture, instances);
    }

    /// [`Self::draw`] with the translucent overlay pipeline: alpha-blended
    /// by each instance colour's alpha, depth-tested but not written.
    pub fn draw_overlay(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        mesh: &GpuMesh,
        instances: Range<u32>,
    ) {
        if instances.is_empty() {
            return;
        }
        self.encode(pass, &self.overlay_pipeline, mesh, &self.white, instances);
    }

    /// Records one instanced draw of `mesh` with `pipeline`.
    fn encode(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipeline: &wgpu::RenderPipeline,
        mesh: &GpuMesh,
        texture: &GpuTexture,
        instances: Range<u32>,
    ) {
        debug_assert!(
            instances.end as usize <= self.instance_cap,
            "instance range {instances:?} exceeds the uploaded buffer"
        );
        let stride = size_of::<InstanceRaw>() as wgpu::BufferAddress;
        let bytes = u64::from(instances.start) * stride..u64::from(instances.end) * stride;

        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_bind_group(1, &texture.bind_group, &[]);
        pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
        pass.set_vertex_buffer(1, self.instance_buf.slice(bytes));
        pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..mesh.index_count, 0, 0..instances.len() as u32);
    }
}

fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instance buffer"),
        size: (capacity * size_of::<InstanceRaw>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Begins a render pass suited to [`MeshRenderer`]: colour cleared to
/// `clear` (linear), depth cleared to 1.0, both stored.
pub fn begin_mesh_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    color_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
    clear: wgpu::Color,
) -> wgpu::RenderPass<'e> {
    mesh_pass(
        encoder,
        color_view,
        depth_view,
        wgpu::LoadOp::Clear(clear),
        wgpu::LoadOp::Clear(1.0),
    )
}

/// A pass with one colour and one depth attachment, both stored.
fn mesh_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    color_view: &wgpu::TextureView,
    depth_view: &wgpu::TextureView,
    color_load: wgpu::LoadOp<wgpu::Color>,
    depth_load: wgpu::LoadOp<f32>,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mesh pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: color_load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth_view,
            depth_ops: Some(wgpu::Operations {
                load: depth_load,
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// Identifies a mesh uploaded to a [`MeshRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshHandle(pub u32);

/// Identifies a texture uploaded to a [`TextureRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureHandle(pub u32);

/// Component: draw this entity's [`Transform`] with `mesh`, tinted `color`,
/// optionally with a base-colour `texture`.
///
/// [`render_world`] draws every entity that has both a [`Transform`] and a
/// `MeshInstance`.
#[derive(Clone, Copy, Debug)]
pub struct MeshInstance {
    /// Which mesh to draw.
    pub mesh: MeshHandle,
    /// Linear RGBA colour, multiplied into the material base colour.
    pub color: [f32; 4],
    /// Base-colour texture, multiplied into `color`; `None` draws
    /// untextured (as if with a white texture).
    pub texture: Option<TextureHandle>,
}

/// GPU meshes addressed by [`MeshHandle`]. Meshes are never removed, so a
/// handle stays valid for the registry's lifetime.
///
/// Each entry records the [`AssetSource`] it was loaded from when it came
/// through [`crate::asset::Assets`]; meshes added with [`Self::add`] have
/// none.
#[derive(Default)]
pub struct MeshRegistry {
    meshes: Vec<GpuMesh>,
    /// `sources[i]` is where `meshes[i]` came from, if known.
    sources: Vec<Option<AssetSource>>,
}

impl MeshRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Uploads `data` and returns its handle.
    /// The mesh has no [`AssetSource`]; prefer [`crate::asset::Assets::mesh`]
    /// for anything a scene file may need to refer to.
    pub fn add(&mut self, device: &wgpu::Device, data: &MeshData) -> MeshHandle {
        self.push(GpuMesh::upload(device, data), None)
    }

    /// Uploads `data`, records that it was loaded from `source`, and returns
    /// its handle. Does not deduplicate; [`crate::asset::Assets`] does.
    pub fn add_with_source(
        &mut self,
        device: &wgpu::Device,
        data: &MeshData,
        source: AssetSource,
    ) -> MeshHandle {
        self.push(GpuMesh::upload(device, data), Some(source))
    }

    /// Where the mesh behind `h` was loaded from, if it was added with
    /// [`Self::add_with_source`]. `None` also for unknown handles.
    pub fn source(&self, h: MeshHandle) -> Option<&AssetSource> {
        self.sources.get(h.0 as usize)?.as_ref()
    }

    fn push(&mut self, mesh: GpuMesh, source: Option<AssetSource>) -> MeshHandle {
        let handle = MeshHandle(self.meshes.len() as u32);
        self.meshes.push(mesh);
        self.sources.push(source);
        handle
    }

    /// The mesh behind `h`.
    ///
    /// # Panics
    ///
    /// If `h` did not come from this registry.
    pub fn get(&self, h: MeshHandle) -> &GpuMesh {
        &self.meshes[h.0 as usize]
    }

    /// Every mesh with its handle, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = (MeshHandle, &GpuMesh)> {
        self.meshes
            .iter()
            .enumerate()
            .map(|(i, mesh)| (MeshHandle(i as u32), mesh))
    }

    /// Whether `h` points at a mesh in this registry.
    pub fn contains(&self, h: MeshHandle) -> bool {
        (h.0 as usize) < self.meshes.len()
    }

    /// Number of meshes added so far.
    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    /// Whether no mesh has been added.
    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }
}

/// Base-colour textures addressed by [`TextureHandle`]. Textures are never
/// removed, so a handle stays valid for the registry's lifetime.
///
/// Like [`MeshRegistry`], each entry records the [`AssetSource`] it was
/// loaded from when it came through [`crate::asset::Assets`].
pub struct TextureRegistry {
    /// Layout and sampler of the renderer the textures are drawn with.
    binder: TextureBinder,
    textures: Vec<GpuTexture>,
    /// `sources[i]` is where `textures[i]` came from, if known.
    sources: Vec<Option<AssetSource>>,
}

impl TextureRegistry {
    /// An empty registry whose textures can be drawn by `renderer`.
    pub fn new(renderer: &MeshRenderer) -> Self {
        Self {
            binder: renderer.binder.clone(),
            textures: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Uploads `image` and returns its handle. The texture has no
    /// [`AssetSource`]; prefer [`crate::asset::Assets::texture`].
    pub fn add(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
    ) -> TextureHandle {
        let texture = self.binder.upload(device, queue, image);
        self.push(texture, None)
    }

    /// Uploads `image`, records that it was loaded from `source`, and
    /// returns its handle. Does not deduplicate; [`crate::asset::Assets`]
    /// does.
    pub fn add_with_source(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
        source: AssetSource,
    ) -> TextureHandle {
        let texture = self.binder.upload(device, queue, image);
        self.push(texture, Some(source))
    }

    fn push(&mut self, texture: GpuTexture, source: Option<AssetSource>) -> TextureHandle {
        let handle = TextureHandle(self.textures.len() as u32);
        self.textures.push(texture);
        self.sources.push(source);
        handle
    }

    /// Where the texture behind `h` was loaded from, if it was added with
    /// [`Self::add_with_source`]. `None` also for unknown handles.
    pub fn source(&self, h: TextureHandle) -> Option<&AssetSource> {
        self.sources.get(h.0 as usize)?.as_ref()
    }

    /// The texture behind `h`.
    ///
    /// # Panics
    ///
    /// If `h` did not come from this registry.
    pub fn get(&self, h: TextureHandle) -> &GpuTexture {
        &self.textures[h.0 as usize]
    }

    /// Every texture with its handle, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = (TextureHandle, &GpuTexture)> {
        self.textures
            .iter()
            .enumerate()
            .map(|(i, texture)| (TextureHandle(i as u32), texture))
    }

    /// Whether `h` points at a texture in this registry.
    pub fn contains(&self, h: TextureHandle) -> bool {
        (h.0 as usize) < self.textures.len()
    }

    /// Number of textures added so far.
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// Whether no texture has been added.
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}

/// What one instanced draw shares: a mesh and an optional texture. Ordered
/// so a mesh's untextured batch comes before its textured ones.
type BatchKey = (MeshHandle, Option<TextureHandle>);

/// Resource holding the renderer, the meshes and textures, the light, and
/// the per-batch instance scratch space that [`render_world`] refills every
/// frame.
pub struct RenderState {
    /// The lit mesh pipeline and its instance buffer.
    pub renderer: MeshRenderer,
    /// Meshes that [`MeshInstance::mesh`] handles point into.
    pub registry: MeshRegistry,
    /// Textures that [`MeshInstance::texture`] handles point into.
    pub textures: TextureRegistry,
    /// Light uploaded at the start of every [`render_world`].
    pub light: LightUniform,
    /// Number of `draw` calls (one per distinct (mesh, texture) pair with
    /// at least one instance) made by the last [`render_world`].
    pub last_frame_draw_calls: u32,
    /// Instances per (mesh, texture) pair, in key order. Cleared, not
    /// freed, each frame so the allocations are reused.
    batches: BTreeMap<BatchKey, Vec<InstanceRaw>>,
    /// All batches back to back, as uploaded to the instance buffer.
    instances: Vec<InstanceRaw>,
}

impl RenderState {
    /// A renderer for colour targets of `color_format`, with no meshes or
    /// textures and [`LightUniform::default`].
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let renderer = MeshRenderer::new(device, queue, color_format);
        Self {
            textures: TextureRegistry::new(&renderer),
            renderer,
            registry: MeshRegistry::new(),
            light: LightUniform::default(),
            last_frame_draw_calls: 0,
            batches: BTreeMap::new(),
            instances: Vec::new(),
        }
    }
}

/// Draws every entity with a [`Transform`] and a [`MeshInstance`] as seen
/// by the [`Camera`] resource: one instanced draw call per distinct
/// (mesh, texture) pair, all in one pass. An entity is placed by its
/// [`GlobalTransform`] when it has one, else by its `Transform`.
///
/// `color` and `depth` are loaded, not cleared (the game loop and
/// [`crate::headless::Headless`] clear both before rendering). `aspect` is
/// the target's width / height. Batches are rebuilt from the world on every
/// call, so spawned, moved, and despawned entities show up immediately.
///
/// Entities whose mesh or texture handle is not in its registry are
/// skipped with a warning.
///
/// # Panics
///
/// If the [`RenderState`] or [`Camera`] resource is missing.
pub fn render_world(
    world: &mut World,
    res: &mut Resources,
    ctx: &GpuContext,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    aspect: f32,
) {
    let camera = *res.expect::<Camera>();
    let state = res.expect_mut::<RenderState>();

    // Group instances by (mesh, texture).
    state.batches.values_mut().for_each(Vec::clear);
    let mut unknown = 0;
    for (transform, global, instance) in
        world.query_mut::<(&Transform, Option<&GlobalTransform>, &MeshInstance)>()
    {
        let transform = global.map_or(transform, |global| &global.0);
        let known = state.registry.contains(instance.mesh)
            && instance.texture.is_none_or(|t| state.textures.contains(t));
        if !known {
            unknown += 1;
            continue;
        }
        state
            .batches
            .entry((instance.mesh, instance.texture))
            .or_default()
            .push(InstanceRaw::from_transform(transform, instance.color));
    }
    if unknown > 0 {
        log::warn!("skipped {unknown} entities whose mesh or texture is not in its registry");
    }

    // Lay the batches out back to back, remembering each one's range.
    state.instances.clear();
    let mut draws = Vec::new();
    for (&key, batch) in &state.batches {
        if batch.is_empty() {
            continue;
        }
        let start = state.instances.len() as u32;
        state.instances.extend_from_slice(batch);
        draws.push((key, start..state.instances.len() as u32));
    }

    state.renderer.set_camera(&ctx.queue, &camera, aspect);
    state.renderer.set_light(&ctx.queue, &state.light);
    state
        .renderer
        .upload_instances(&ctx.device, &ctx.queue, &state.instances);

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_world encoder"),
        });
    {
        let mut pass = mesh_pass(
            &mut encoder,
            color,
            depth,
            wgpu::LoadOp::Load,
            wgpu::LoadOp::Load,
        );
        for ((mesh, texture), range) in &draws {
            let mesh = state.registry.get(*mesh);
            let texture = match texture {
                Some(handle) => state.textures.get(*handle),
                None => state.renderer.white_texture(),
            };
            state
                .renderer
                .draw_textured(&mut pass, mesh, texture, range.clone());
        }
    }
    ctx.queue.submit([encoder.finish()]);
    state.last_frame_draw_calls = draws.len() as u32;
}

/// One translucent mesh drawn by [`render_overlay`].
#[derive(Clone, Copy, Debug)]
pub struct OverlayDraw {
    /// The mesh to draw.
    pub mesh: MeshHandle,
    /// Where, in world space.
    pub transform: Transform,
    /// Linear RGBA tint; alpha is the opacity.
    pub color: [f32; 4],
}

/// Draws `draws` as tinted, translucent meshes over what `color` and
/// `depth` already hold (after [`render_world`]), as seen by the
/// [`Camera`] resource: alpha-blended, hidden behind nearer opaque
/// surfaces, and not writing depth. For editor overlays such as collider
/// shapes (GE-034); [`render_world`] never draws them.
///
/// Draws whose mesh is not in the registry are skipped.
///
/// # Panics
///
/// If the [`RenderState`] or [`Camera`] resource is missing.
pub fn render_overlay(
    res: &mut Resources,
    ctx: &GpuContext,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    aspect: f32,
    draws: &[OverlayDraw],
) {
    let camera = *res.expect::<Camera>();
    let state = res.expect_mut::<RenderState>();
    let draws: Vec<&OverlayDraw> = draws
        .iter()
        .filter(|d| state.registry.contains(d.mesh))
        .collect();
    if draws.is_empty() {
        return;
    }
    let instances: Vec<InstanceRaw> = draws
        .iter()
        .map(|d| InstanceRaw::from_transform(&d.transform, d.color))
        .collect();
    state.renderer.set_camera(&ctx.queue, &camera, aspect);
    state.renderer.set_light(&ctx.queue, &state.light);
    state
        .renderer
        .upload_instances(&ctx.device, &ctx.queue, &instances);

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_overlay encoder"),
        });
    {
        let mut pass = mesh_pass(
            &mut encoder,
            color,
            depth,
            wgpu::LoadOp::Load,
            wgpu::LoadOp::Load,
        );
        for (i, draw) in draws.iter().enumerate() {
            let i = i as u32;
            state
                .renderer
                .draw_overlay(&mut pass, state.registry.get(draw.mesh), i..i + 1);
        }
    }
    ctx.queue.submit([encoder.finish()]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::{OFFSCREEN_COLOR_FORMAT, OffscreenTarget};
    use crate::math::{Projection, Quat, Vec3};

    #[test]
    fn layouts_match_struct_sizes() {
        assert_eq!(size_of::<InstanceRaw>(), 128);
        assert_eq!(size_of::<CameraUniform>(), 80);
        assert_eq!(size_of::<LightUniform>(), 48);
        let last = INSTANCE_ATTRIBUTES.last().unwrap();
        assert_eq!(last.offset as usize + 16, size_of::<InstanceRaw>());
        let last = VERTEX_ATTRIBUTES.last().unwrap();
        assert_eq!(last.offset as usize + 8, size_of::<Vertex>());
    }

    #[test]
    fn normal_matrix_undoes_non_uniform_scale() {
        let t = Transform::from_trs(
            Vec3::new(1.0, 2.0, 3.0),
            Quat::from_rotation_y(0.7),
            Vec3::new(2.0, 1.0, 0.5),
        );
        let raw = InstanceRaw::from_transform(&t, [1.0; 4]);
        let n = Mat3::from_cols(
            Vec3::from_slice(&raw.normal_mat[0]),
            Vec3::from_slice(&raw.normal_mat[1]),
            Vec3::from_slice(&raw.normal_mat[2]),
        );
        // A transformed normal stays perpendicular to transformed tangents.
        let model = t.to_matrix();
        let normal = Vec3::new(1.0, 1.0, 0.0).normalize();
        let tangent = Vec3::new(1.0, -1.0, 0.0);
        let dot = (n * normal).dot(model.transform_vector3(tangent));
        assert!(dot.abs() < 1e-5, "{dot}");
        assert!(raw.normal_mat.iter().all(|c| c[3] == 0.0));
    }

    /// Renders `count` cube instances offscreen with `renderer`.
    fn render_instances(
        ctx: &GpuContext,
        target: &OffscreenTarget,
        renderer: &mut MeshRenderer,
        mesh: &GpuMesh,
        count: usize,
    ) {
        let instances: Vec<_> = (0..count)
            .map(|i| {
                let t = Transform::from_translation(Vec3::new(i as f32 * 0.01, 0.0, 0.0));
                InstanceRaw::from_transform(&t, [1.0, 0.5, 0.25, 1.0])
            })
            .collect();
        renderer.upload_instances(&ctx.device, &ctx.queue, &instances);
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = begin_mesh_pass(
                &mut encoder,
                &target.color_view,
                &target.depth_view,
                wgpu::Color::BLACK,
            );
            renderer.draw(&mut pass, mesh, 0..count as u32);
        }
        ctx.queue.submit([encoder.finish()]);
    }

    #[test]
    fn draw_grows_instance_buffer_without_validation_errors() -> anyhow::Result<()> {
        let ctx = GpuContext::new(None)?;
        let target = OffscreenTarget::new(&ctx.device, 32, 32);
        let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let mut renderer = MeshRenderer::new(&ctx.device, &ctx.queue, OFFSCREEN_COLOR_FORMAT);
        let mut camera = Camera {
            transform: Transform::from_translation(Vec3::new(0.0, 2.0, 5.0)),
            projection: Projection::Perspective {
                fov_y_radians: 45f32.to_radians(),
                near: 0.1,
                far: 100.0,
            },
        };
        camera.transform.looking_at(Vec3::ZERO, Vec3::Y);
        renderer.set_camera(&ctx.queue, &camera, 1.0);
        let mesh = GpuMesh::upload(&ctx.device, &crate::asset::cube());

        render_instances(&ctx, &target, &mut renderer, &mesh, 1);
        assert_eq!(renderer.instance_capacity(), 1);
        render_instances(&ctx, &target, &mut renderer, &mesh, 1000);
        assert!(renderer.instance_capacity() >= 1000);

        let image = target.read_pixels(&ctx)?;
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "validation error: {error:?}");
        // The cube really was drawn: the centre is not the black clear colour.
        assert_ne!(image.get_pixel(16, 16).0, [0, 0, 0, 255]);
        Ok(())
    }

    /// GE-034: an overlay blends over the frame by its alpha, and is hidden
    /// behind nearer opaque surfaces.
    #[test]
    fn render_overlay_blends_and_is_depth_tested() -> anyhow::Result<()> {
        let mut engine = crate::headless::Headless::new(32, 32)?;
        let scope = engine
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let mesh = engine
            .resources
            .expect_mut::<RenderState>()
            .registry
            .add(&engine.gpu.device, &crate::asset::cube());
        let overlay = |engine: &mut crate::headless::Headless, alpha: f32| {
            engine.render_to_target();
            let draw = OverlayDraw {
                mesh,
                transform: Transform::default(),
                color: [1.0, 0.0, 0.0, alpha],
            };
            let (color, depth) = (&engine.target.color_view, &engine.target.depth_view);
            render_overlay(&mut engine.resources, &engine.gpu, color, depth, 1.0, &[draw]);
            engine.target.read_pixels(&engine.gpu)
        };
        let opaque = overlay(&mut engine, 1.0)?.get_pixel(16, 16).0;
        let translucent = overlay(&mut engine, 0.5)?.get_pixel(16, 16).0;
        assert!(opaque[0] > 100, "{opaque:?}");
        assert!(
            translucent[0] > 20 && translucent[0] < opaque[0] - 20,
            "half as red: {translucent:?} vs {opaque:?}"
        );
        assert_eq!(translucent[1..3], [0, 0]);

        // A white cube drawn normally in front hides the overlay.
        engine.world.spawn((
            Transform::from_translation(Vec3::new(0.0, 0.5, 2.0)),
            MeshInstance {
                mesh,
                color: [1.0; 4],
                texture: None,
            },
        ));
        let hidden = overlay(&mut engine, 0.5)?.get_pixel(16, 16).0;
        assert!(
            hidden[0] > 50 && hidden[0] == hidden[1] && hidden[1] == hidden[2],
            "the white cube, no red tint: {hidden:?}"
        );
        assert!(pollster::block_on(scope.pop()).is_none());
        Ok(())
    }
}
