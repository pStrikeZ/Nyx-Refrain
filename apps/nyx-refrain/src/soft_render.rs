//! CPU rendering of egui meshes. Colors stay in egui's premultiplied sRGBA space;
//! the opaque output is softbuffer's 0RGB format. No platform APIs are used here.

use std::collections::HashMap;

use egui::epaint::{ClippedPrimitive, Primitive, Vertex};
use egui::{Color32, ColorImage, ImageData, Pos2, Rect, TextureId, TexturesDelta, Vec2};

#[derive(Default)]
pub struct Renderer {
    textures: HashMap<TextureId, ColorImage>,
}

impl Renderer {
    /// Apply before drawing, including frames whose window is hidden.
    pub fn update_textures(&mut self, delta: &mut TexturesDelta) {
        for (id, changes) in delta.set.drain() {
            for change in changes {
                let ImageData::Color(image) = &change.image;
                if let Some([x, y]) = change.pos {
                    let texture = self
                        .textures
                        .get_mut(&id)
                        .expect("texture patch before allocation");
                    for row in 0..image.size[1] {
                        let start = (y + row) * texture.size[0] + x;
                        texture.pixels[start..start + image.size[0]].copy_from_slice(
                            &image.pixels[row * image.size[0]..(row + 1) * image.size[0]],
                        );
                    }
                } else {
                    self.textures.insert(id, (**image).clone());
                }
            }
        }
    }

    /// egui requires frees to happen after the frame using these textures.
    pub fn free_textures(&mut self, delta: &mut TexturesDelta) {
        for id in delta.free.drain() {
            self.textures.remove(&id);
        }
    }

    pub fn paint(
        &self,
        primitives: &[ClippedPrimitive],
        pixels_per_point: f32,
        size: [usize; 2],
        background: Color32,
        buffer: &mut [u32],
    ) {
        buffer.fill(pack(background.to_array().map(f32::from)));
        for clipped in primitives {
            // This UI uses only meshes; GPU paint callbacks have no CPU equivalent.
            let Primitive::Mesh(mesh) = &clipped.primitive else {
                continue;
            };
            let texture = &self.textures[&mesh.texture_id];
            let clip = clipped.clip_rect * pixels_per_point;
            for indices in mesh.indices.chunks_exact(3) {
                let vertices = [
                    mesh.vertices[indices[0] as usize],
                    mesh.vertices[indices[1] as usize],
                    mesh.vertices[indices[2] as usize],
                ];
                triangle(vertices, texture, clip, pixels_per_point, size, buffer);
            }
        }
    }
}

fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// Half-open coverage gives shared triangle edges exactly one owner (no alpha seams).
fn top_left(a: Pos2, b: Pos2) -> bool {
    b.y < a.y || (b.y == a.y && b.x > a.x)
}

fn triangle(
    mut v: [Vertex; 3],
    texture: &ColorImage,
    clip: Rect,
    scale: f32,
    size: [usize; 2],
    buffer: &mut [u32],
) {
    for vertex in &mut v {
        vertex.pos *= scale;
    }
    let mut area = edge(v[0].pos, v[1].pos, v[2].pos);
    if area == 0.0 {
        return;
    }
    if area < 0.0 {
        v.swap(1, 2);
        area = -area;
    }
    let bounds = Rect::from_points(&v.map(|v| v.pos))
        .intersect(clip)
        .intersect(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(size[0] as f32, size[1] as f32),
        ));
    let min = (bounds.min - Vec2::splat(0.5)).ceil();
    let max = (bounds.max - Vec2::splat(0.5)).ceil();
    let edges = [
        (v[1].pos, v[2].pos),
        (v[2].pos, v[0].pos),
        (v[0].pos, v[1].pos),
    ];
    let inclusive = edges.map(|(a, b)| top_left(a, b));
    let colors = v.map(|v| v.color.to_array().map(f32::from));
    for y in min.y.max(0.0) as usize..max.y.max(0.0) as usize {
        for x in min.x.max(0.0) as usize..max.x.max(0.0) as usize {
            let p = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            let weights = edges.map(|(a, b)| edge(a, b, p));
            if weights
                .iter()
                .zip(inclusive)
                .any(|(&w, inc)| w < 0.0 || (w == 0.0 && !inc))
            {
                continue;
            }
            let weights = weights.map(|w| w / area);
            let uv = v[0].uv.to_vec2() * weights[0]
                + v[1].uv.to_vec2() * weights[1]
                + v[2].uv.to_vec2() * weights[2];
            let texel = sample(texture, uv);
            let source = std::array::from_fn(|c| {
                (colors[0][c] * weights[0] + colors[1][c] * weights[1] + colors[2][c] * weights[2])
                    * texel[c]
                    / 255.0
            });
            let pixel = &mut buffer[y * size[0] + x];
            *pixel = blend(source, *pixel);
        }
    }
}

/// Bilinear filtering, with texel centers at (n + 0.5) / size and clamp-to-edge.
fn sample(texture: &ColorImage, uv: Vec2) -> [f32; 4] {
    let [w, h] = texture.size;
    let x = (uv.x * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
    let y = (uv.y * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x.fract(), y.fract());
    let taps = [
        texture[(x0, y0)],
        texture[(x1, y0)],
        texture[(x0, y1)],
        texture[(x1, y1)],
    ]
    .map(|c| c.to_array().map(f32::from));
    std::array::from_fn(|c| {
        (taps[0][c] * (1.0 - fx) + taps[1][c] * fx) * (1.0 - fy)
            + (taps[2][c] * (1.0 - fx) + taps[3][c] * fx) * fy
    })
}

fn pack(c: [f32; 4]) -> u32 {
    let c = c.map(|v| v.round().clamp(0.0, 255.0) as u32);
    (c[0] << 16) | (c[1] << 8) | c[2]
}

fn blend(source: [f32; 4], dest: u32) -> u32 {
    let dest = [(dest >> 16) & 255, (dest >> 8) & 255, dest & 255, 255];
    pack(std::array::from_fn(|c| {
        source[c] + dest[c] as f32 * (1.0 - source[3] / 255.0)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{ImageDelta, Mesh};

    fn rectangle(color: Color32, clip_rect: Rect) -> ClippedPrimitive {
        let mut mesh = Mesh::default();
        mesh.add_colored_rect(Rect::from_min_max(Pos2::ZERO, egui::pos2(2.0, 2.0)), color);
        ClippedPrimitive {
            clip_rect,
            primitive: Primitive::Mesh(mesh),
        }
    }

    #[test]
    fn fractional_scale_clip_and_shared_edges() {
        let mut renderer = Renderer::default();
        let mut delta = TexturesDelta {
            set: [(
                TextureId::default(),
                ImageDelta::full(
                    ColorImage::filled([1, 1], Color32::WHITE),
                    egui::TextureOptions::LINEAR,
                ),
            )]
            .into_iter()
            .map(|(id, image)| (id, [image].into_iter().collect()))
            .collect(),
            free: Default::default(),
        };
        renderer.update_textures(&mut delta);
        let primitive = rectangle(
            Color32::from_rgba_premultiplied(128, 0, 0, 128),
            Rect::from_min_max(Pos2::ZERO, egui::pos2(4.0 / 3.0, 2.0)),
        );
        let mut buffer = [0; 16];
        renderer.paint(&[primitive], 1.5, [4, 4], Color32::BLUE, &mut buffer);
        assert_eq!(
            buffer,
            [
                0x80007f, 0x80007f, 0xff, 0xff, 0x80007f, 0x80007f, 0xff, 0xff, 0x80007f, 0x80007f,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff
            ]
        );
    }

    #[test]
    fn solid_triangle_and_winding() {
        let texture = ColorImage::filled([1, 1], Color32::WHITE);
        let vertex = |x, y| Vertex {
            pos: egui::pos2(x, y),
            uv: Pos2::ZERO,
            color: Color32::GREEN,
        };
        let v = [vertex(0.0, 0.0), vertex(3.0, 0.0), vertex(0.0, 3.0)];
        let mut a = [0; 9];
        let mut b = a;
        triangle(v, &texture, Rect::EVERYTHING, 1.0, [3, 3], &mut a);
        triangle(
            [v[2], v[1], v[0]],
            &texture,
            Rect::EVERYTHING,
            1.0,
            [3, 3],
            &mut b,
        );
        assert_eq!(a, b);
        assert_eq!(a, [0x00ff00, 0x00ff00, 0, 0x00ff00, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn texture_patch_sampling_and_free() {
        let id = TextureId::Managed(7);
        let mut renderer = Renderer::default();
        renderer.update_textures(&mut TexturesDelta {
            set: [(
                id,
                ImageDelta::full(
                    ColorImage::filled([2, 1], Color32::RED),
                    egui::TextureOptions::LINEAR,
                ),
            )]
            .into_iter()
            .map(|(id, image)| (id, [image].into_iter().collect()))
            .collect(),
            free: Default::default(),
        });
        renderer.update_textures(&mut TexturesDelta {
            set: [(
                id,
                ImageDelta::partial(
                    [1, 0],
                    ColorImage::filled([1, 1], Color32::BLUE),
                    egui::TextureOptions::LINEAR,
                ),
            )]
            .into_iter()
            .map(|(id, image)| (id, [image].into_iter().collect()))
            .collect(),
            free: Default::default(),
        });
        let image = &renderer.textures[&id];
        assert_eq!(
            sample(image, egui::vec2(0.25, 0.5)),
            [255.0, 0.0, 0.0, 255.0]
        );
        assert_eq!(
            sample(image, egui::vec2(0.5, 0.5)),
            [127.5, 0.0, 127.5, 255.0]
        );
        assert_eq!(
            sample(image, egui::vec2(2.0, 0.5)),
            [0.0, 0.0, 255.0, 255.0]
        );
        renderer.free_textures(&mut TexturesDelta {
            free: [id].into_iter().collect(),
            set: Default::default(),
        });
        assert!(renderer.textures.is_empty());
    }

    #[test]
    fn premultiplied_blend() {
        assert_eq!(blend([128.0, 0.0, 0.0, 128.0], 0x0000ff), 0x80007f);
        assert_eq!(blend([0.0; 4], 0x123456), 0x123456);
        assert_eq!(blend([20.0, 30.0, 40.0, 255.0], 0xffffff), 0x141e28);
    }
}
