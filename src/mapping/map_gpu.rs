//! GPU map: base map tiles and trip lines in one pass. Geometry uploads once per tile or
//! line change; panning, zooming and rotation only rewrite small uniforms.
//!
//! Fills use stencil-then-cover: each polygon ring's triangle fan counts winding into
//! stencil, then one screen cover per style paints where count is non-zero. 4x MSAA
//! smooths fill edges; attachments are transient, so tile GPUs keep them on chip.
use super::{
    camera::{Camera, TileId},
    labels::system_font,
    labels_gpu::{self, Atlas},
    style::{BACKGROUND, Paint, STYLES},
    vector_tiles::{Tile, corner},
};
use std::{collections::HashMap, ops::Range, sync::Arc};
use zgui::surface::wgpu;

const SAMPLES: u32 = 4;
const STENCIL: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;
/// Draw uniform: offset (2 f32), width, dashed, colour (4 f32), cover rectangle (4 f32).
const DRAW_SIZE: u64 = 48;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineStyle {
    pub rgba: [u8; 4],
    /// CSS px.
    pub width: f32,
    pub dashed: bool,
}

/// Trip and area lines. Coordinates are projected metres relative to `origin`; runs of
/// equal style draw together.
#[derive(Default, PartialEq)]
pub struct Lines {
    pub origin: [f64; 2],
    pub segments: Vec<[f32; 4]>,
    pub runs: Vec<(LineStyle, Range<u32>)>,
}

impl Lines {
    /// Adds one segment; `a`, `b` are absolute projected metres.
    pub fn push(&mut self, a: [f64; 2], b: [f64; 2], style: LineStyle) {
        if self.segments.is_empty() {
            self.origin = a;
        }
        let rel = |p: [f64; 2]| {
            [
                (p[0] - self.origin[0]) as f32,
                (p[1] - self.origin[1]) as f32,
            ]
        };
        let (a, b) = (rel(a), rel(b));
        let i = self.segments.len() as u32;
        self.segments.push([a[0], a[1], b[0], b[1]]);
        match self.runs.last_mut() {
            Some((s, range)) if *s == style => range.end = i + 1,
            _ => self.runs.push((style, i..i + 1)),
        }
    }
}

/// What one frame draws. Base map is drawn (with given dark flag) when `base` is set;
/// otherwise output is transparent apart from lines.
#[derive(Default)]
pub struct Scene {
    pub tiles: Vec<(TileId, Arc<Tile>)>,
    pub lines: Arc<Lines>,
    pub base: Option<bool>,
}

/// Same geometry and settings, compared by identity: tiles are large.
impl PartialEq for Scene {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && Arc::ptr_eq(&self.lines, &other.lines)
            && self.tiles.len() == other.tiles.len()
            && self
                .tiles
                .iter()
                .zip(&other.tiles)
                .all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1))
    }
}

/// One output frame: target and its size, camera, size in CSS px, device px per CSS px.
pub struct Frame<'a> {
    pub output: &'a wgpu::TextureView,
    pub extent: [u32; 2],
    pub camera: Camera,
    pub css: [f64; 2],
    pub ratio: f32,
}

struct Buffers {
    tile: Arc<Tile>,
    fills: Option<wgpu::Buffer>,
    segments: Option<wgpu::Buffer>,
}

enum Cmd<'a> {
    Fill(&'a wgpu::Buffer, Range<u32>),
    Cover,
    Line(&'a wgpu::Buffer, Range<u32>),
    /// Glyph instances, halo pass then text pass.
    Text(u32),
}

pub struct Renderer {
    fill: wgpu::RenderPipeline,
    cover: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    text: wgpu::RenderPipeline,
    atlas: Atlas,
    /// Glyph instances, rewritten per frame, with capacity in bytes.
    glyphs: Option<(u64, wgpu::Buffer)>,
    view: wgpu::Buffer,
    view_bind: wgpu::BindGroup,
    draw_layout: wgpu::BindGroupLayout,
    /// Per-draw uniforms at `stride`, with capacity in draws.
    draws: Option<(u64, wgpu::Buffer, wgpu::BindGroup)>,
    stride: u64,
    tiles: HashMap<usize, Buffers>,
    lines: Option<(Arc<Lines>, wgpu::Buffer)>,
    targets: Option<([u32; 2], wgpu::TextureView, wgpu::TextureView)>,
}

fn bytes<const N: usize>(v: &[[f32; N]]) -> Vec<u8> {
    v.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect()
}

fn uniform_layout(device: &wgpu::Device, dynamic: bool) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(if dynamic { DRAW_SIZE } else { 16 }),
            },
            count: None,
        }],
    })
}

fn stencil(compare: wgpu::CompareFunction, pass: wgpu::StencilOperation) -> wgpu::StencilFaceState {
    wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: pass,
    }
}

impl Renderer {
    pub fn new(device: &wgpu::Device) -> Self {
        let view = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("map.view"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view_layout = uniform_layout(device, false);
        let draw_layout = uniform_layout(device, true);
        let view_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &view_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("map"),
            source: wgpu::ShaderSource::Wgsl(include_str!("map.wgsl").into()),
        });
        let atlas_layout = Atlas::layout(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&view_layout), Some(&draw_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let pipeline = |label: &str,
                        vs: &str,
                        buffers: &[wgpu::VertexBufferLayout<'_>],
                        write: wgpu::ColorWrites,
                        front: wgpu::StencilFaceState,
                        back: wgpu::StencilFaceState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(match vs {
                        "vs_line" => "fs_line",
                        "vs_text" => "fs_text",
                        _ => "fs_fill",
                    }),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: write,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: STENCIL,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState {
                        front,
                        back,
                        read_mask: 0xff,
                        write_mask: 0xff,
                    },
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: SAMPLES,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{CompareFunction as C, StencilOperation as S};
        let fill = pipeline(
            "map.fill",
            "vs_fill",
            &[wgpu::VertexBufferLayout {
                array_stride: 8,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2],
            }],
            wgpu::ColorWrites::empty(),
            stencil(C::Always, S::IncrementWrap),
            stencil(C::Always, S::DecrementWrap),
        );
        let cover = pipeline(
            "map.cover",
            "vs_cover",
            &[],
            wgpu::ColorWrites::ALL,
            stencil(C::NotEqual, S::Zero),
            stencil(C::NotEqual, S::Zero),
        );
        let line = pipeline(
            "map.line",
            "vs_line",
            &[wgpu::VertexBufferLayout {
                array_stride: 16,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
            }],
            wgpu::ColorWrites::ALL,
            stencil(C::Always, S::Keep),
            stencil(C::Always, S::Keep),
        );
        let text = pipeline(
            "map.text",
            "vs_text",
            &[wgpu::VertexBufferLayout {
                array_stride: labels_gpu::INSTANCE,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2,
                    4 => Float32x4, 5 => Unorm8x4, 6 => Unorm8x4
                ],
            }],
            wgpu::ColorWrites::ALL,
            stencil(C::Always, S::Keep),
            stencil(C::Always, S::Keep),
        );
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        Self {
            fill,
            cover,
            line,
            text,
            atlas: Atlas::new(device, &atlas_layout),
            glyphs: None,
            view,
            view_bind,
            draw_layout,
            draws: None,
            stride: DRAW_SIZE.div_ceil(align) * align,
            tiles: HashMap::new(),
            lines: None,
            targets: None,
        }
    }

    fn vertices(device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> Option<wgpu::Buffer> {
        (!bytes.is_empty()).then(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("map.geometry"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytes);
            buffer
        })
    }

    /// Uploads geometry new to this scene and frees geometry it no longer uses.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene) {
        self.tiles
            .retain(|_, b| scene.tiles.iter().any(|(_, t)| Arc::ptr_eq(t, &b.tile)));
        for (_, tile) in &scene.tiles {
            self.tiles
                .entry(Arc::as_ptr(tile) as usize)
                .or_insert_with(|| Buffers {
                    tile: tile.clone(),
                    fills: Self::vertices(device, queue, &bytes(&tile.fills)),
                    segments: Self::vertices(device, queue, &bytes(&tile.segments)),
                });
        }
        if self
            .lines
            .as_ref()
            .is_none_or(|(have, _)| !Arc::ptr_eq(have, &scene.lines))
        {
            self.lines = Self::vertices(device, queue, &bytes(&scene.lines.segments))
                .map(|b| (scene.lines.clone(), b));
        }
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        frame: Frame<'_>,
    ) {
        let Frame {
            output,
            extent,
            camera,
            css,
            ratio,
        } = frame;
        self.upload(device, queue, scene);
        if self
            .targets
            .as_ref()
            .is_none_or(|(size, ..)| *size != extent)
        {
            let target = |format| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("map.msaa"),
                        size: wgpu::Extent3d {
                            width: extent[0],
                            height: extent[1],
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: SAMPLES,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TRANSIENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            self.targets = Some((
                extent,
                target(wgpu::TextureFormat::Rgba8Unorm),
                target(STENCIL),
            ));
        }

        // Styles at nearest integer zoom, widths scaled with zoom between: matches CPU tiles.
        let level = camera.zoom.round().clamp(0.0, 255.0);
        let zoom = level as u8;
        let grow = 2f64.powf(camera.zoom - level) as f32;
        let mut uniforms: Vec<u8> = Vec::new();
        let mut cmds: Vec<(u32, Cmd<'_>)> = Vec::new();
        let stride = self.stride as usize;
        let mut push =
            |offset: [f64; 2], width: f32, dashed: bool, rgba: [u8; 4], rect: [f32; 4]| {
                let at = uniforms.len();
                let v = [
                    offset[0] as f32,
                    offset[1] as f32,
                    width,
                    f32::from(u8::from(dashed)),
                ]
                .into_iter()
                .chain(rgba.map(|c| f32::from(c) / 255.0))
                .chain(rect);
                for f in v {
                    uniforms.extend(f.to_ne_bytes());
                }
                uniforms.resize(at + stride, 0);
                (at / stride) as u32
            };
        let rel = |o: [f64; 2]| [o[0] - camera.center[0], o[1] - camera.center[1]];
        let mut clear = wgpu::Color::TRANSPARENT;
        let mut text_draws = None;
        if let Some(dark) = scene.base {
            let [r, g, b, _] = BACKGROUND.rgba(dark, zoom).map(|c| f64::from(c) / 255.0);
            clear = wgpu::Color { r, g, b, a: 1.0 };
            for (slot, style) in STYLES.iter().enumerate() {
                if !style.visible(zoom) {
                    continue;
                }
                let rgba = style.rgba(dark, zoom);
                let mut covers = Vec::new();
                for (id, tile) in &scene.tiles {
                    let buffers = &self.tiles[&(Arc::as_ptr(tile) as usize)];
                    let offset = rel(corner(*id));
                    match style.paint {
                        Paint::Fill { .. } => {
                            if let (Some(buffer), Some((_, r, bounds))) =
                                (&buffers.fills, tile.fill_runs.iter().find(|r| r.0 == slot))
                            {
                                cmds.push((
                                    push(offset, 0.0, false, rgba, [0.0; 4]),
                                    Cmd::Fill(buffer, r.clone()),
                                ));
                                covers.push((offset, *bounds));
                            }
                        }
                        Paint::Line(_) => {
                            if let (Some(buffer), Some((_, r))) = (
                                &buffers.segments,
                                tile.line_runs.iter().find(|r| r.0 == slot),
                            ) {
                                let width = style.width(zoom).unwrap_or(1.0) as f32 * grow;
                                cmds.push((
                                    push(offset, width, false, rgba, [0.0; 4]),
                                    Cmd::Line(buffer, r.clone()),
                                ));
                            }
                        }
                    }
                }
                // Cover only where this style has geometry: most styles are sparse.
                for (offset, bounds) in covers {
                    cmds.push((push(offset, 0.0, false, rgba, bounds), Cmd::Cover));
                }
            }
        }
        if let Some(dark) = scene.base
            && let Some(font) = system_font()
        {
            let instances = self
                .atlas
                .place(queue, font, &scene.tiles, camera, css, ratio, dark);
            if !instances.is_empty() {
                let size = instances.len() as u64;
                if self.glyphs.as_ref().is_none_or(|(cap, _)| *cap < size) {
                    let cap = size.next_power_of_two();
                    self.glyphs = Some((
                        cap,
                        device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("map.glyphs"),
                            size: cap,
                            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        }),
                    ));
                }
                queue.write_buffer(&self.glyphs.as_ref().unwrap().1, 0, &instances);
                let count = (size / labels_gpu::INSTANCE) as u32;
                text_draws = Some((
                    count,
                    [
                        push([0.0; 2], 0.0, false, [0; 4], [0.0; 4]),
                        push([0.0; 2], 0.0, true, [0; 4], [0.0; 4]),
                    ],
                ));
            }
        }
        if let Some((_, buffer)) = &self.lines {
            let offset = rel(scene.lines.origin);
            for (style, range) in &scene.lines.runs {
                let draw = push(offset, style.width, style.dashed, style.rgba, [0.0; 4]);
                cmds.push((draw, Cmd::Line(buffer, range.clone())));
            }
        }
        // Labels above trip lines, as image tiles were.
        if let Some((count, passes)) = text_draws {
            for draw in passes {
                cmds.push((draw, Cmd::Text(count)));
            }
        }

        let view: Vec<u8> = [camera.scale() as f32, ratio, css[0] as f32, css[1] as f32]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
        queue.write_buffer(&self.view, 0, &view);
        let count = (uniforms.len() / stride).max(1) as u64;
        if self.draws.as_ref().is_none_or(|(cap, ..)| *cap < count) {
            let cap = count.next_power_of_two().max(64);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("map.draws"),
                size: cap * self.stride,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.draw_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(DRAW_SIZE),
                    }),
                }],
            });
            self.draws = Some((cap, buffer, bind));
        }
        let (_, draws, draw_bind) = self.draws.as_ref().unwrap();
        if !uniforms.is_empty() {
            queue.write_buffer(draws, 0, &uniforms);
        }
        let (_, color, stencil) = self.targets.as_ref().unwrap();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("map"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: Some(output),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: stencil,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Discard,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.view_bind, &[]);
        pass.set_bind_group(2, &self.atlas.bind, &[]);
        for (draw, cmd) in cmds {
            pass.set_bind_group(1, draw_bind, &[draw * self.stride as u32]);
            match cmd {
                Cmd::Fill(buffer, range) => {
                    pass.set_pipeline(&self.fill);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(range, 0..1);
                }
                Cmd::Cover => {
                    pass.set_pipeline(&self.cover);
                    pass.draw(0..6, 0..1);
                }
                Cmd::Text(count) => {
                    pass.set_pipeline(&self.text);
                    pass.set_vertex_buffer(0, self.glyphs.as_ref().unwrap().1.slice(..));
                    pass.draw(0..6, 0..count);
                }
                Cmd::Line(buffer, range) => {
                    pass.set_pipeline(&self.line);
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(0..6, range);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mapping::camera::{TILE, WORLD};

    #[test]
    fn segments_pack_relative_to_first_point_in_style_runs() {
        let solid = LineStyle {
            rgba: [1, 2, 3, 4],
            width: 5.0,
            dashed: false,
        };
        let dashed = LineStyle {
            dashed: true,
            ..solid
        };
        let mut lines = Lines::default();
        lines.push([1e6, 2e6], [1e6 + 5.0, 2e6], solid);
        lines.push([1e6 + 5.0, 2e6], [1e6 + 9.5, 2e6 + 1.0], solid);
        lines.push([1e6, 2e6], [1e6, 2e6 + 1.0], dashed);
        assert_eq!(lines.origin, [1e6, 2e6]);
        assert_eq!(lines.segments[1], [5.0, 0.0, 9.5, 1.0]);
        assert_eq!(lines.runs, vec![(solid, 0..2), (dashed, 2..3)]);
    }

    /// Minimal executor: wgpu's native futures complete after a device poll.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::yield_now();
        }
    }

    /// Renders `scene` at `camera` into w x h RGBA pixels (1 device px per CSS px).
    pub fn render(scene: &Scene, camera: Camera, w: u32, h: u32) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::default();
        let Ok(adapter) = block_on(instance.request_adapter(&Default::default())) else {
            eprintln!("no GPU adapter; skipping");
            return None;
        };
        let (device, queue) = block_on(adapter.request_device(&Default::default())).unwrap();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut renderer = Renderer::new(&device);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            Frame {
                output: &texture.create_view(&Default::default()),
                extent: [w, h],
                camera,
                css: [f64::from(w), f64::from(h)],
                ratio: 1.0,
            },
        );
        let row = (w * 4).next_multiple_of(256);
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        read.slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = read.slice(..).get_mapped_range();
        Some(
            mapped
                .chunks(row as usize)
                .flat_map(|r| &r[..(w * 4) as usize])
                .copied()
                .collect(),
        )
    }

    #[test]
    fn gpu_draws_capsules_and_dashes() {
        let (w, h) = (64u32, 32u32);
        // One CSS px per metre, centred on origin.
        let camera = Camera {
            center: [0.0, 0.0],
            zoom: (WORLD / TILE).log2(),
        };
        let mut lines = Lines::default();
        let style = |rgba, width, dashed| LineStyle {
            rgba,
            width,
            dashed,
        };
        lines.push(
            [-20.0, 0.0],
            [20.0, 0.0],
            style([255, 0, 0, 255], 6.0, false),
        );
        lines.push(
            [-30.0, -10.0],
            [30.0, -10.0],
            style([0, 0, 255, 255], 2.0, true),
        );
        let scene = Scene {
            lines: Arc::new(lines),
            ..Default::default()
        };
        let Some(pixels) = render(&scene, camera, w, h) else {
            return;
        };
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        // Solid line: x 12..52 at y 16, 6 px wide, round ends.
        assert_eq!(px(32, 16), [255, 0, 0, 255], "line centre");
        assert_eq!(px(32, 14), [255, 0, 0, 255], "inside width");
        assert_eq!(px(32, 9)[3], 0, "outside width");
        assert_eq!(px(5, 16)[3], 0, "beyond round end");
        assert!(px(10, 16)[3] > 0, "round end cap");
        // Dashed: starts x 2 at y 26; 5 px on, 4 off.
        assert!(px(4, 26)[2] > 200, "dash on");
        assert_eq!(px(9, 26)[3], 0, "dash gap");
        assert!(px(13, 26)[2] > 200, "next dash");
    }
}
