//! Map labels on GPU: candidates from visible vector tiles placed each frame in screen space
//! (one collision pass across all tiles), glyphs from an atlas rasterized at exact device
//! size, so text stays crisp at every zoom. Atlas red is glyph coverage, green its halo.
use super::{
    camera::{Camera, TileId},
    labels::Candidate,
    style::rgba,
    vector_tiles::{Tile, corner},
};
use ab_glyph::{Font, FontVec, GlyphId, ScaleFont, point};
use std::{collections::HashMap, sync::Arc};
use zgui::surface::wgpu;

const ATLAS: u32 = 1024;
/// Halo reach beyond glyph outline, CSS px (CPU tiles stroke 2.5 px wide).
const HALO: f32 = 1.25;
/// Bytes per glyph instance: anchor, local, size, rotation, uv rect (f32s), two colours.
pub const INSTANCE: u64 = 56;

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 4],
    /// Box top-left from pen on baseline, device px.
    offset: [f32; 2],
    /// Device px.
    size: [f32; 2],
}

pub struct Atlas {
    texture: wgpu::Texture,
    pub bind: wgpu::BindGroup,
    /// (glyph, px x 8, ratio x 8) -> glyph; None for blank glyphs.
    glyphs: HashMap<(u16, u32, u32), Option<Glyph>>,
    /// Shelf packing: cursor x, y, row height.
    shelf: [u32; 3],
}

struct Full;

impl Atlas {
    pub fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
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
        })
    }

    pub fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("map.glyphs"),
            size: wgpu::Extent3d {
                width: ATLAS,
                height: ATLAS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Self {
            texture,
            bind,
            glyphs: HashMap::new(),
            shelf: [0; 3],
        }
    }

    /// Glyph `id` at `px` device px, rasterizing it on first use.
    fn glyph(
        &mut self,
        queue: &wgpu::Queue,
        font: &FontVec,
        id: GlyphId,
        px: f32,
        ratio: f32,
    ) -> Result<Option<Glyph>, Full> {
        let key = (id.0, (px * 8.0) as u32, (ratio * 8.0) as u32);
        if let Some(g) = self.glyphs.get(&key) {
            return Ok(*g);
        }
        let Some(outline) = font.outline_glyph(id.with_scale_and_position(px, point(0.0, 0.0)))
        else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        let bounds = outline.px_bounds();
        let reach = HALO * ratio;
        let pad = reach.ceil() as u32 + 1;
        let (gw, gh) = (bounds.width() as u32, bounds.height() as u32);
        let (w, h) = (gw + 2 * pad, gh + 2 * pad);
        let mut coverage = vec![0f32; (w * h) as usize];
        outline.draw(|x, y, c| {
            coverage[((y + pad) * w + x + pad) as usize] = c;
        });
        // Halo: coverage dilated by `reach`, with one-pixel soft edge.
        let r = reach.ceil() as i32 + 1;
        let mut texels = vec![0u8; (w * h * 2) as usize];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let mut halo = 0f32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (sx, sy) = (x + dx, y + dy);
                        if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                            continue;
                        }
                        let near = (reach + 0.5 - (dx as f32).hypot(dy as f32)).clamp(0.0, 1.0);
                        halo = halo.max(coverage[(sy as u32 * w + sx as u32) as usize] * near);
                    }
                }
                let i = (y as u32 * w + x as u32) as usize;
                texels[i * 2] = (coverage[i] * 255.0).round() as u8;
                texels[i * 2 + 1] = (halo * 255.0).round() as u8;
            }
        }
        let [mut sx, mut sy, mut row] = self.shelf;
        if sx + w > ATLAS {
            (sx, sy, row) = (0, sy + row, 0);
        }
        if sy + h > ATLAS {
            return Err(Full);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: sx, y: sy, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &texels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 2),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.shelf = [sx + w, sy, row.max(h)];
        let a = ATLAS as f32;
        let glyph = Glyph {
            uv: [
                sx as f32 / a,
                sy as f32 / a,
                (sx + w) as f32 / a,
                (sy + h) as f32 / a,
            ],
            offset: [bounds.min.x - pad as f32, bounds.min.y - pad as f32],
            size: [w as f32, h as f32],
        };
        self.glyphs.insert(key, Some(glyph));
        Ok(Some(glyph))
    }

    /// Glyph instances for labels of `tiles` in view. `css` is output size, `ratio`
    /// device px per CSS px.
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &mut self,
        queue: &wgpu::Queue,
        font: &FontVec,
        tiles: &[(TileId, Arc<Tile>)],
        camera: Camera,
        css: [f64; 2],
        ratio: f32,
        dark: bool,
    ) -> Vec<u8> {
        match self.try_place(queue, font, tiles, camera, css, ratio, dark) {
            Ok(v) => v,
            // Atlas full: start over with only glyphs this view needs.
            Err(Full) => {
                self.glyphs.clear();
                self.shelf = [0; 3];
                self.try_place(queue, font, tiles, camera, css, ratio, dark)
                    .unwrap_or_default()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn try_place(
        &mut self,
        queue: &wgpu::Queue,
        font: &FontVec,
        tiles: &[(TileId, Arc<Tile>)],
        camera: Camera,
        css: [f64; 2],
        ratio: f32,
        dark: bool,
    ) -> Result<Vec<u8>, Full> {
        let zoom = camera.zoom.round();
        // Text size eased with fractional zoom: grows and shrinks smoothly with a pinch.
        let grow = super::style::eased(super::style::LABEL_SCALE, camera.zoom);
        let scale = camera.scale();
        let [w, h] = css;
        let mut all: Vec<(&Candidate, f64, f64)> = Vec::new();
        for (id, tile) in tiles {
            let [x0, y0] = corner(*id);
            for c in &tile.labels {
                let x = (x0 + c.at.0 - camera.center[0]) * scale + w / 2.0;
                let y = h / 2.0 - (y0 - c.at.1 - camera.center[1]) * scale;
                if zoom >= c.min_zoom && (-w..2.0 * w).contains(&x) && (-h..2.0 * h).contains(&y) {
                    all.push((c, x, y));
                }
            }
        }
        all.sort_by(|a, b| {
            a.0.priority
                .cmp(&b.0.priority)
                .then(b.0.room.unwrap_or(0.0).total_cmp(&a.0.room.unwrap_or(0.0)))
        });
        let mut placed: Vec<[f64; 4]> = Vec::new();
        let mut roads: Vec<(&str, f64, f64)> = Vec::new();
        let mut out = Vec::new();
        for (c, x, y) in all {
            // One label per road name nearby.
            if c.room.is_some()
                && roads
                    .iter()
                    .any(|r| r.0 == c.text && (r.1 - x).hypot(r.2 - y) < 300.0)
            {
                continue;
            }
            let size = c.size * grow;
            // Glyphs rasterized at whole device px (atlas reuse), quads scaled by `fit` to
            // exact size.
            let exact = size as f32 * ratio;
            let px = exact.round().max(1.0);
            let fit = exact / px;
            let scaled = font.as_scaled(px);
            let mut glyphs = Vec::new();
            let mut pen = 0.0;
            let mut previous = None;
            let mut missing = false;
            for ch in c.text.chars() {
                let id = font.glyph_id(ch);
                if id.0 == 0 && !ch.is_whitespace() {
                    missing = true;
                    break;
                }
                if let Some(p) = previous {
                    pen += scaled.kern(p, id);
                }
                glyphs.push((id, pen));
                pen += scaled.h_advance(id);
                previous = Some(id);
            }
            let advance = pen;
            let width = f64::from(pen * fit / ratio);
            if missing || c.room.is_some_and(|room| room * scale < width + 8.0) {
                continue;
            }
            // Rotated text box (baseline 0.35 size below centre) with 3 px margin.
            let (sin, cos) = c.at.2.sin_cos();
            let (hw, top, bottom) = (width / 2.0 + 3.0, -0.75 * size - 3.0, 0.6 * size + 3.0);
            let corners = [(-hw, top), (hw, top), (-hw, bottom), (hw, bottom)]
                .map(|(lx, ly)| (x + cos * lx - sin * ly, y + sin * lx + cos * ly));
            let bounds = corners
                .iter()
                .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| {
                    [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]
                });
            if bounds[2] < 0.0 || bounds[0] > w || bounds[3] < 0.0 || bounds[1] > h {
                continue;
            }
            if placed.iter().any(|p| {
                p[0] < bounds[2] && bounds[0] < p[2] && p[1] < bounds[3] && bounds[1] < p[3]
            }) {
                continue;
            }
            placed.push(bounds);
            if c.room.is_some() {
                roads.push((&c.text, x, y));
            }
            let (text, halo) = c.colors(dark);
            let colors = [rgba(text), rgba(halo)];
            let baseline = 0.35 * px;
            for (id, pen) in glyphs {
                let Some(g) = self.glyph(queue, font, id, px, ratio)? else {
                    continue;
                };
                // Box top-left in label frame, device px from label centre.
                let local = [pen + g.offset[0] - advance / 2.0, baseline + g.offset[1]];
                let k = fit / ratio;
                let (mut anchor, mut local) = ([x as f32, y as f32], [local[0] * k, local[1] * k]);
                // Horizontal text lands on whole device pixels: crisp, like CPU tiles.
                if c.at.2 == 0.0 {
                    anchor = [
                        ((anchor[0] + local[0]) * ratio).round() / ratio,
                        ((anchor[1] + local[1]) * ratio).round() / ratio,
                    ];
                    local = [0.0, 0.0];
                }
                let floats = [
                    anchor[0],
                    anchor[1],
                    local[0],
                    local[1],
                    g.size[0] * k,
                    g.size[1] * k,
                    cos as f32,
                    sin as f32,
                ]
                .into_iter()
                .chain(g.uv);
                for f in floats {
                    out.extend(f.to_ne_bytes());
                }
                out.extend(colors.concat());
            }
        }
        Ok(out)
    }
}
