//! The renderer. Edges are static world-space meshes drawn through a camera
//! uniform; everything else is a short list rebuilt each frame for what is
//! on screen (see `paint`).

use bytemuck::{Pod, Zeroable};
use lyon::{
    math::point,
    path::Path,
    tessellation::{BuffersBuilder, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers},
};
use wgpu::util::DeviceExt;

use super::text::{ATLAS, Fonts, GlyphInstance};
use crate::{model::Model, theme::Color};

/// Where the world sits in the view: `screen = world * k + (x, y)`, in
/// points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub k: f32,
    pub x: f32,
    pub y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EdgeVertex {
    /// On the edge's centre line.
    pos: [f32; 2],
    /// Which way to push it out to reach the stroke's side, per unit of
    /// half-width. The width is a uniform, so one mesh serves every zoom.
    normal: [f32; 2],
    edge: u32,
}

/// How one edge is drawn this frame; `layer` 0 is beneath `layer` 1, and
/// any other value is not drawn at all.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, PartialEq)]
pub struct EdgeStyle {
    pub color: [f32; 4],
    pub layer: u32,
    pub _pad: [u32; 3],
}

/// A filled rectangle in screen points.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct RectInstance {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// A vertex of a filled triangle in screen points.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct TriVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

/// What `paint` produced for one frame.
#[derive(Default)]
pub struct Frame {
    pub clear: Option<Color>,
    /// Drawn above the edges and beneath the cards: arrowheads, the
    /// highlighted edge.
    pub tris: Vec<TriVertex>,
    pub rects: Vec<RectInstance>,
    pub glyphs: Vec<GlyphInstance>,
    /// Rectangles above the text: tooltips' grounds.
    pub top_rects: Vec<RectInstance>,
    pub top_glyphs: Vec<GlyphInstance>,
    /// Half the edges' stroke width, in screen points.
    pub edge_half_width: f32,
}

impl Frame {
    pub fn reset(&mut self) {
        self.clear = None;
        self.tris.clear();
        self.rects.clear();
        self.glyphs.clear();
        self.top_rects.clear();
        self.top_glyphs.clear();
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    /// World to clip.
    scale: [f32; 2],
    offset: [f32; 2],
    /// Screen points to clip.
    screen_scale: [f32; 2],
    half_width: f32,
    atlas: f32,
}

const SHADER: &str = r"
struct Globals {
    scale: vec2<f32>,
    offset: vec2<f32>,
    screen_scale: vec2<f32>,
    half_width: f32,
    atlas: f32,
};
struct EdgeStyle {
    color: vec4<f32>,
    layer: u32,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<storage, read> styles: array<EdgeStyle>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) kind: f32,
};

fn screen(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p * g.screen_scale + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

// The instance index is the layer being drawn: an edge on another layer
// collapses to nothing.
@vertex
fn edge_vs(
    @builtin(instance_index) layer: u32,
    @location(0) pos: vec2<f32>,
    @location(1) normal: vec2<f32>,
    @location(2) edge: u32,
) -> Out {
    var out: Out;
    let style = styles[edge];
    let world = pos + normal * g.half_width;
    out.pos = vec4<f32>(world * g.scale + g.offset, 0.0, 1.0);
    if (style.layer != layer) {
        out.pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    }
    out.color = style.color;
    out.uv = vec2<f32>(0.0);
    out.kind = 0.0;
    return out;
}

@vertex
fn tri_vs(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> Out {
    var out: Out;
    out.pos = screen(pos);
    out.color = color;
    out.uv = vec2<f32>(0.0);
    out.kind = 0.0;
    return out;
}

@vertex
fn rect_vs(@builtin(vertex_index) i: u32, @location(0) rect: vec4<f32>, @location(1) color: vec4<f32>) -> Out {
    var out: Out;
    let corner = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    out.pos = screen(rect.xy + rect.zw * corner);
    out.color = color;
    out.uv = vec2<f32>(0.0);
    out.kind = 0.0;
    return out;
}

@vertex
fn glyph_vs(
    @builtin(vertex_index) i: u32,
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: vec4<f32>,
) -> Out {
    var out: Out;
    let corner = vec2<f32>(f32(i & 1u), f32((i >> 1u) & 1u));
    out.pos = screen(rect.xy + rect.zw * corner);
    out.color = color;
    out.kind = kind.x;
    out.uv = (uv.xy + uv.zw * corner) / g.atlas;
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}

// The field is 0.5 on the outline and changes by 1/(2*spread) per field
// texel; `fwidth` says how much of that one screen pixel covers, which is
// the width to fade the edge over.
@fragment
fn glyph_fs(in: Out) -> @location(0) vec4<f32> {
    let d = textureSample(atlas, atlas_sampler, in.uv).r;
    let w = max(fwidth(d), 0.0001);
    // Small glyphs are plain coverage, made at the size they are drawn.
    // CoreText thickens glyphs as it smooths them, and gpui draws through
    // it; a slightly lower threshold and a gamma on coverage put the same
    // weight on the canvas.
    let field = clamp((d - 0.47) / w + 0.5, 0.0, 1.0);
    let a = mix(field, pow(d, 0.78), in.kind) * in.color.a;
    return vec4<f32>(in.color.rgb * a, a);
}
";

const SAMPLES: u32 = 4;

/// The narrowest an edge is drawn, in device pixels.
const EDGE_MIN_DEVICE_PX: f32 = 2.0;

/// The edges flattened to one tolerance.
struct EdgeMesh {
    /// The zoom this mesh is fine enough for, and coarser.
    max_zoom: f32,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// Flattening error allowed on screen, in points.
const CURVE_TOL: f32 = 0.35;

/// Zoom ceilings of the edge meshes. A curve flattened finely enough for
/// the closest zoom is two million triangles for the GitHub schema, nearly
/// all of them smaller than a pixel when the whole graph is in view; each
/// mesh here is flattened for its own ceiling and no finer.
const EDGE_LODS: [f32; 4] = [0.08, 0.3, 1.0, 4.0];

/// A vertex buffer rewritten every frame, grown when a frame outgrows it.
struct Dynamic {
    buffer: wgpu::Buffer,
    capacity: u64,
    label: &'static str,
}

impl Dynamic {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        let capacity = 1 << 16;
        Self { buffer: Self::make(device, label, capacity), capacity, label }
    }

    fn make(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) {
        if bytes.len() as u64 > self.capacity {
            self.capacity = (bytes.len() as u64).next_power_of_two();
            self.buffer = Self::make(device, self.label, self.capacity);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytes);
        }
    }
}

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    msaa: wgpu::TextureView,
    globals: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    atlas: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    styles: wgpu::Buffer,
    style_count: usize,
    edge_pipeline: wgpu::RenderPipeline,
    tri_pipeline: wgpu::RenderPipeline,
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    edges: Vec<EdgeMesh>,
    tris: Dynamic,
    rects: Dynamic,
    glyphs: Dynamic,
    top_rects: Dynamic,
    top_glyphs: Dynamic,
}

impl Gpu {
    /// # Safety
    /// `target` must stay valid for as long as the returned value lives.
    pub unsafe fn new(target: wgpu::SurfaceTargetUnsafe, width: u32, height: u32) -> anyhow::Result<Self> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        // SAFETY: the caller's.
        let surface = unsafe { instance.create_surface_unsafe(target)? };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        let caps = surface.get_capabilities(&adapter);
        // Colours arrive already encoded for the screen, and blend there, as
        // they do in AppKit; an sRGB target would decode them a second time.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: Default::default(),
            width: width.max(1),
            height: height.max(1),
            // The display link already paces frames to the refresh. Letting
            // the layer wait for it as well makes each frame block inside
            // the one before's interval, and a tick is missed whenever the
            // two drift: 7 to 10% of refreshes, measured, at every zoom.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let msaa = msaa_view(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("graph"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let styles = style_buffer(&device, &[EdgeStyle::zeroed()]);
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("graph"),
            entries: &[
                entry(
                    0,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::ShaderStages::VERTEX,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    2,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(3, wgpu::ShaderStages::FRAGMENT, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let bind = bind_group(&device, &layout, &globals, &styles, &atlas_view, &sampler);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("graph"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = |label: &str,
                        vs: &str,
                        fs: &str,
                        stride: usize,
                        step_mode,
                        attributes: &[wgpu::VertexAttribute],
                        topology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers: &[Some(wgpu::VertexBufferLayout { array_stride: stride as u64, step_mode, attributes })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{PrimitiveTopology::*, VertexStepMode::*};
        let edge_pipeline = pipeline(
            "edges",
            "edge_vs",
            "fs",
            std::mem::size_of::<EdgeVertex>(),
            Vertex,
            &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32],
            TriangleList,
        );
        let tri_pipeline = pipeline(
            "triangles",
            "tri_vs",
            "fs",
            std::mem::size_of::<TriVertex>(),
            Vertex,
            &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
            TriangleList,
        );
        let rect_pipeline = pipeline(
            "rects",
            "rect_vs",
            "fs",
            std::mem::size_of::<RectInstance>(),
            Instance,
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
            TriangleStrip,
        );
        let glyph_pipeline = pipeline(
            "glyphs",
            "glyph_vs",
            "glyph_fs",
            std::mem::size_of::<GlyphInstance>(),
            Instance,
            &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4],
            TriangleStrip,
        );
        Ok(Self {
            tris: Dynamic::new(&device, "triangles"),
            rects: Dynamic::new(&device, "rects"),
            glyphs: Dynamic::new(&device, "glyphs"),
            top_rects: Dynamic::new(&device, "top rects"),
            top_glyphs: Dynamic::new(&device, "top glyphs"),
            surface,
            device,
            queue,
            config,
            msaa,
            globals,
            layout,
            bind,
            atlas,
            atlas_view,
            sampler,
            styles,
            style_count: 1,
            edge_pipeline,
            tri_pipeline,
            rect_pipeline,
            glyph_pipeline,
            edges: Vec::new(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width, height) == self.size() {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.msaa = msaa_view(&self.device, &self.config);
    }

    /// Tessellates and uploads a model's edges. Once: nothing here depends
    /// on the camera.
    pub fn set_model(&mut self, model: &Model) {
        let t = std::time::Instant::now();
        let mut tess = StrokeTessellator::new();
        self.edges.clear();
        let mut counts = Vec::new();
        let mut segments = Vec::new();
        for max_zoom in EDGE_LODS {
            let mut geometry: VertexBuffers<EdgeVertex, u32> = VertexBuffers::new();
            // Flattened here, for this ceiling, so lyon only joins lines.
            let options = StrokeOptions::default().with_line_width(2.0).with_tolerance(CURVE_TOL / max_zoom);
            let mut lines = 0;
            for (i, e) in model.edges.iter().enumerate() {
                let polyline = lane_polyline(e, max_zoom);
                lines += polyline.len().saturating_sub(1);
                let mut b = Path::builder();
                b.begin(point(polyline[0][0], polyline[0][1]));
                for p in &polyline[1..] {
                    b.line_to(point(p[0], p[1]));
                }
                b.end(false);
                let path = b.build();
                let _ = tess.tessellate_path(
                    &path,
                    &options,
                    &mut BuffersBuilder::new(&mut geometry, |v: StrokeVertex| EdgeVertex {
                        pos: v.position_on_path().to_array(),
                        normal: v.normal().to_array(),
                        edge: i as u32,
                    }),
                );
            }
            counts.push(geometry.indices.len() / 3);
            segments.push(lines);
            let buffer = |label, contents: &[u8], usage| {
                self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage })
            };
            self.edges.push(EdgeMesh {
                max_zoom,
                vertices: buffer("edge vertices", bytemuck::cast_slice(&geometry.vertices), wgpu::BufferUsages::VERTEX),
                indices: buffer("edge indices", bytemuck::cast_slice(&geometry.indices), wgpu::BufferUsages::INDEX),
                index_count: geometry.indices.len() as u32,
            });
        }
        if std::env::var("GRAVIZ_PERF").is_ok() {
            eprintln!(
                "gpu: {} edges as {segments:?} segments, {counts:?} triangles, built in {:.1}ms",
                model.edges.len(),
                t.elapsed().as_secs_f32() * 1000.0
            );
        }
    }

    /// How each edge is drawn from now on; sent only when it changes.
    pub fn set_edge_styles(&mut self, styles: &[EdgeStyle]) {
        if styles.is_empty() {
            return;
        }
        if styles.len() == self.style_count {
            self.queue.write_buffer(&self.styles, 0, bytemuck::cast_slice(styles));
            return;
        }
        self.styles = style_buffer(&self.device, styles);
        self.style_count = styles.len();
        self.bind = bind_group(&self.device, &self.layout, &self.globals, &self.styles, &self.atlas_view, &self.sampler);
    }

    /// Draws one frame. `scale` is device pixels per point.
    pub fn render(&mut self, cam: Camera, scale: f32, frame: &Frame, fonts: &mut Fonts) {
        let (w, h) = (self.config.width as f32 / scale, self.config.height as f32 / scale);
        let globals = Globals {
            scale: [cam.k * 2.0 / w, -cam.k * 2.0 / h],
            offset: [cam.x * 2.0 / w - 1.0, 1.0 - cam.y * 2.0 / h],
            screen_scale: [2.0 / w, -2.0 / h],
            // At least two device pixels wide: a thinner line straddles a
            // pixel boundary under MSAA and lands as two half-lit pixels,
            // which reads as a faded edge (gpui's rasteriser keeps one fully
            // lit pixel instead).
            half_width: frame.edge_half_width.max(EDGE_MIN_DEVICE_PX / 2.0 / scale) / cam.k,
            atlas: ATLAS as f32,
        };
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        if let Some((lo, hi)) = fonts.dirty.take() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: lo, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &fonts.pixels[(lo * ATLAS) as usize..(hi * ATLAS) as usize],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ATLAS), rows_per_image: None },
                wgpu::Extent3d { width: ATLAS, height: hi - lo, depth_or_array_layers: 1 },
            );
        }
        self.tris.write(&self.device, &self.queue, bytemuck::cast_slice(&frame.tris));
        self.rects.write(&self.device, &self.queue, bytemuck::cast_slice(&frame.rects));
        self.glyphs.write(&self.device, &self.queue, bytemuck::cast_slice(&frame.glyphs));
        self.top_rects.write(&self.device, &self.queue, bytemuck::cast_slice(&frame.top_rects));
        self.top_glyphs.write(&self.device, &self.queue, bytemuck::cast_slice(&frame.top_glyphs));

        let target = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = target.texture.create_view(&Default::default());
        let clear = frame.clear.unwrap_or(Color::BLACK);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("graph"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.msaa,
                    depth_slice: None,
                    resolve_target: Some(&view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear.r as f64,
                            g: clear.g as f64,
                            b: clear.b as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.bind, &[]);
            let mesh = self.edges.iter().find(|m| cam.k <= m.max_zoom).or(self.edges.last());
            // A slice with no edges (an orphan tab of loose types) has
            // empty meshes, which wgpu cannot bind.
            if let Some(edges) = mesh.filter(|m| m.index_count > 0) {
                pass.set_pipeline(&self.edge_pipeline);
                pass.set_vertex_buffer(0, edges.vertices.slice(..));
                pass.set_index_buffer(edges.indices.slice(..), wgpu::IndexFormat::Uint32);
                // Two instances, two layers: receded edges, then lit ones.
                pass.draw_indexed(0..edges.index_count, 0, 0..2);
            }
            let instanced = |pass: &mut wgpu::RenderPass<'_>, pipeline, buffer: &Dynamic, count: usize| {
                if count > 0 {
                    pass.set_pipeline(pipeline);
                    pass.set_vertex_buffer(0, buffer.buffer.slice(..));
                    pass.draw(0..4, 0..count as u32);
                }
            };
            if !frame.tris.is_empty() {
                pass.set_pipeline(&self.tri_pipeline);
                pass.set_vertex_buffer(0, self.tris.buffer.slice(..));
                pass.draw(0..frame.tris.len() as u32, 0..1);
            }
            instanced(&mut pass, &self.rect_pipeline, &self.rects, frame.rects.len());
            instanced(&mut pass, &self.glyph_pipeline, &self.glyphs, frame.glyphs.len());
            instanced(&mut pass, &self.rect_pipeline, &self.top_rects, frame.top_rects.len());
            instanced(&mut pass, &self.glyph_pipeline, &self.top_glyphs, frame.top_glyphs.len());
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(target);
    }
}

fn style_buffer(device: &wgpu::Device, styles: &[EdgeStyle]) -> wgpu::Buffer {
    let stand_in = [EdgeStyle::zeroed()];
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("edge styles"),
        // A storage binding cannot be empty: one unused style stands in.
        contents: bytemuck::cast_slice(if styles.is_empty() { &stand_in } else { styles }),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    })
}

fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    styles: &wgpu::Buffer,
    atlas: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("graph"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: styles.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(atlas) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn msaa_view(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("msaa"),
            size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// How far, in screen pixels at the mesh's zoom ceiling, a dropped vertex
/// may sit from the line drawn in its place (the gpui canvas's `PEN_TOL`).
const PEN_TOL: f32 = 0.3;

/// Segments that keep one cubic within `tol` screen points of its chords at
/// zoom `k` (the gpui canvas's `cubic_steps`). A lane's bend is a tall S
/// half a gap wide; when the box round its control points is thinner than
/// the tolerance on screen, it is one line.
fn cubic_steps(p0: graviz_core::layout::Point, c: &graviz_core::layout::CubicSeg, k: f32, tol: f32) -> usize {
    let w = p0.x.max(c.c1.x).max(c.c2.x).max(c.end.x) - p0.x.min(c.c1.x).min(c.c2.x).min(c.end.x);
    let h = p0.y.max(c.c1.y).max(c.c2.y).max(c.end.y) - p0.y.min(c.c1.y).min(c.c2.y).min(c.end.y);
    if w.min(h) * k <= tol {
        return 1;
    }
    let d0x = (p0.x - 2.0 * c.c1.x + c.c2.x) * k;
    let d0y = (p0.y - 2.0 * c.c1.y + c.c2.y) * k;
    let d1x = (c.c1.x - 2.0 * c.c2.x + c.end.x) * k;
    let d1y = (c.c1.y - 2.0 * c.c2.y + c.end.y) * k;
    let m = (d0x * d0x + d0y * d0y).max(d1x * d1x + d1y * d1y).sqrt();
    ((0.75 * m / tol).sqrt().ceil() as usize).clamp(1, 24)
}

/// An edge as the polyline its mesh for zoom ceiling `k` strokes.
///
/// A lane-routed edge is level runs joined by short bends, and a run that
/// shifts by one lane pitch is three cubics in the world and one straight
/// line on screen at low zoom. Flattening per cubic cannot go below one
/// segment each, so the cubics are flattened here and a sleeve then drops
/// every vertex within `PEN_TOL` of the line kept in its place (the gpui
/// canvas's `Pen`): from the last vertex kept, each candidate narrows a
/// wedge of directions that pass near everything dropped so far.
fn lane_polyline(e: &crate::model::EdgeVisual, k: f32) -> Vec<[f32; 2]> {
    let tol = PEN_TOL / k;
    let mut out = vec![[e.start.x, e.start.y]];
    let mut anchor = (e.start.x, e.start.y);
    let mut pending: Option<(f32, f32)> = None;
    let (mut reach, mut left, mut right) = (0.0f32, (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let cross = |a: (f32, f32), b: (f32, f32)| a.0 * b.1 - a.1 * b.0;
    let mut p0 = e.start;
    for c in &e.curves {
        let n = cubic_steps(p0, c, k, CURVE_TOL);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let p = (a * p0.x + b * c.c1.x + cc * c.c2.x + d * c.end.x, a * p0.y + b * c.c1.y + cc * c.c2.y + d * c.end.y);
            loop {
                let v = (p.0 - anchor.0, p.1 - anchor.1);
                let len = (v.0 * v.0 + v.1 * v.1).sqrt();
                if len < 1e-3 / k {
                    break;
                }
                let perp = (-v.1 / len * tol, v.0 / len * tol);
                let (l, r) = ((v.0 + perp.0, v.1 + perp.1), (v.0 - perp.0, v.1 - perp.1));
                let Some(kept) = pending else {
                    pending = Some(p);
                    (reach, left, right) = (len, l, r);
                    break;
                };
                if len >= reach && cross(v, left) >= 0.0 && cross(right, v) >= 0.0 {
                    pending = Some(p);
                    reach = len;
                    if cross(left, l) < 0.0 {
                        left = l;
                    }
                    if cross(right, r) > 0.0 {
                        right = r;
                    }
                    break;
                }
                out.push([kept.0, kept.1]);
                anchor = kept;
                pending = None;
            }
        }
        p0 = c.end;
    }
    if let Some(kept) = pending {
        out.push([kept.0, kept.1]);
    }
    if out.len() == 1 {
        out.push([p0.x, p0.y]);
    }
    out
}
