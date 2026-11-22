//! Projected volumes use the same vertices for painting and hit testing.
use crate::motion;
use egui::{
    epaint::{Mesh, Vertex},
    *,
};

#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub origin: Pos2,
    pub horizontal: Vec2,
    pub vertical: Vec2,
}
impl Plane {
    pub fn point(self, u: f32, v: f32) -> Pos2 {
        self.origin + self.horizontal * u + self.vertical * v
    }
    pub fn rect(rect: Rect) -> Self {
        Self {
            origin: rect.min,
            horizontal: vec2(rect.width(), 0.),
            vertical: vec2(0., rect.height()),
        }
    }
    pub fn sub(self, left: f32, top: f32, width: f32, height: f32) -> Self {
        Self {
            origin: self.point(left, top),
            horizontal: self.horizontal * width,
            vertical: self.vertical * height,
        }
    }
    pub fn bounds(self) -> Rect {
        Rect::from_points(&[
            self.origin,
            self.point(1., 0.),
            self.point(1., 1.),
            self.point(0., 1.),
        ])
    }
    pub fn contains(self, p: Pos2) -> bool {
        let d = p - self.origin;
        let determinant = self.horizontal.x * self.vertical.y - self.horizontal.y * self.vertical.x;
        if determinant.abs() < 0.01 {
            return false;
        }
        let u = (d.x * self.vertical.y - d.y * self.vertical.x) / determinant;
        let v = (self.horizontal.x * d.y - self.horizontal.y * d.x) / determinant;
        (0. ..=1.).contains(&u) && (0. ..=1.).contains(&v)
    }
}

#[derive(Clone)]
pub struct Volume {
    pub lane: i32,
    pub index: usize,
    pub front: Plane,
    pub face: Plane,
    pub cover: Plane,
    pub cap: [Pos2; 4],
    pub thickness: f32,
    pub height: f32,
    scale: f32,
    ink: f32,
    opacity: f32,
    lift: f32,
    distance: f32,
    floor: Pos2,
}
impl Volume {
    pub fn bounds(&self) -> Rect {
        self.front
            .bounds()
            .union(self.face.bounds())
            .union(Rect::from_points(&self.cap))
    }
    pub fn contains(&self, pos: Pos2) -> bool {
        if self.opacity < 0.1 {
            return false;
        }
        self.front.contains(pos)
            || self.face.contains(pos)
            || Plane {
                origin: self.cap[0],
                horizontal: self.cap[1] - self.cap[0],
                vertical: self.cap[3] - self.cap[0],
            }
            .contains(pos)
    }
}

pub fn foreground(volumes: &[Volume], lane: i32, cover: Plane) -> Vec<Volume> {
    volumes
        .iter()
        .filter(|v| v.lane > lane && v.bounds().intersects(cover.bounds()))
        .cloned()
        .collect()
}

fn shelf_scale(area: Rect) -> f32 {
    // 上界提到 2.6：网格下线后书架独占整屏，书必须按高度长大，
    // 焦点封面目标约为视口高的 46–52%（Gemini 建议）。
    (area.height() / 416.).clamp(0.2, 2.6)
}

pub fn lane_pitch(area: Rect) -> f32 {
    let scale = shelf_scale(area);
    ((area.width() / scale * 0.043).clamp(14., 18.) + 11.) * scale
}

pub fn lane_pitch_for(area: Rect, count: usize) -> f32 {
    let base = lane_pitch(area);
    if (2..=9).contains(&count) {
        (area.width() / (count as f32 + 3.5)).clamp(base, base * 2.)
    } else {
        base
    }
}

pub fn geometry(area: Rect, count: usize, focus: f32, entrance: f32) -> Vec<Volume> {
    if count == 0 {
        return Vec::new();
    }
    let focus = if count == 1 { 0. } else { focus };
    let scale = shelf_scale(area);
    let width = area.width();
    let pitch = lane_pitch_for(area, count);
    let spine = lane_pitch(area) - 11. * scale;
    let cover = (width / scale * 0.32).clamp(100., 190.) * scale;
    let height = (cover - 20. * scale) * 166. / 118. + 54. * scale;
    let base_y = -cover * -0.30 + height * 0.60 + 12. * scale;
    let mut start_x = area.center().x - (spine + cover * 0.66) / 2. + cover * 0.84 * 0.66;
    let first = -(count as i32 / 2);
    let last = first + count as i32 - 1;
    if count <= 9 {
        // Centre the resting arrangement once. Changing the currently
        // rendered lane window must never move all the other books.
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        for lane in first..=last {
            let shelf = crate::params::params().shelf.clone();
            let weight = motion::weight(lane as f32);
            let x = start_x + lane as f32 * pitch
                - cover * shelf.pull_base * weight * (shelf.pull_linear - shelf.pull_quad * weight) * 0.66;
            left = left.min(x);
            right = right.max(x + spine + cover * 0.66);
        }
        start_x += area.center().x - (left + right) / 2.;
    }
    let edge =
        ((width / 2. + spine + cover * 0.66 + cover * 0.84 * 0.66) / pitch).ceil() as i32 + 2;
    // floor is deliberate: the wrapping window changes at integral lane seams.
    let center = focus.floor() as i32;
    // A small collection should look like five books, not twenty repeated
    // spines. The lane identities still wrap, so next/previous keeps cycling.
    let lanes = if count == 1 {
        0..=0
    } else if count <= 9 {
        center + first..=center + last + 1
    } else {
        center - edge..=center + edge
    };
    lanes
        .filter_map(|lane| {
            let distance = lane as f32 - focus;
            let opacity = if (2..=9).contains(&count) {
                (distance - first as f32 + 1.)
                    .clamp(0., 1.)
                    .min((last as f32 + 1. - distance).clamp(0., 1.))
            } else {
                1.
            };
            if opacity <= 0.00001 {
                return None;
            }
            let shelf = crate::params::params().shelf.clone();
            let weight = motion::weight(distance);
            let pull = cover * shelf.pull_base * weight * (shelf.pull_linear - shelf.pull_quad * weight);
            let lift = motion::smooth(weight);
            let rail = distance.clamp(-7., 7.);
            let start = ((distance + 8.) / 24.).clamp(0., 0.65);
            let ink = motion::cubic(
                ((entrance - start) / 0.35).clamp(0., 1.),
                0.215,
                0.61,
                0.355,
                1.,
            );
            let phase = entrance * motion::ENTRANCE as f32 / 1.7 - distance * 0.075;
            let crest = (1. + (phase * std::f32::consts::TAU).sin()) * 0.5;
            let load = -9. * crest * crest * (1. - ink) * scale;
            let rail_y = (rail * shelf.rail_linear + rail * rail * shelf.rail_quad) * scale;
            let floor = pos2(
                start_x + distance * pitch,
                area.top() + base_y + height + rail_y,
            );
            let origin = pos2(
                start_x + distance * pitch - pull * 0.66,
                area.top() + base_y + pull * 0.30 - height * 0.60 * lift + rail_y + load,
            );
            let front = Plane {
                origin,
                horizontal: vec2(spine, spine * 0.10),
                vertical: vec2(0., height),
            };
            let face = Plane {
                origin: front.point(1., 0.),
                horizontal: vec2(cover * 0.66, cover * -0.30),
                vertical: vec2(0., height),
            };
            Some(Volume {
                lane,
                index: lane.rem_euclid(count as i32) as usize,
                front,
                face,
                cover: face.sub(
                    10. * scale / cover,
                    30. * scale / height,
                    1. - 20. * scale / cover,
                    1. - 54. * scale / height,
                ),
                cap: [
                    origin,
                    front.point(1., 0.),
                    face.point(1., 0.),
                    origin + face.horizontal,
                ],
                thickness: spine,
                height,
                scale,
                ink,
                opacity,
                lift,
                distance,
                floor,
            })
        })
        .collect()
}

fn polygon(p: &Painter, points: Vec<Pos2>, fill: Color32, stroke: Stroke) {
    p.add(Shape::convex_polygon(points, fill, stroke));
}
fn flat(p: &Painter, plane: Plane, color: Color32) {
    polygon(
        p,
        vec![
            plane.point(0., 0.),
            plane.point(1., 0.),
            plane.point(1., 1.),
            plane.point(0., 1.),
        ],
        color,
        Stroke::NONE,
    );
}
fn flat_stroked(p: &Painter, plane: Plane, color: Color32, stroke: Stroke) {
    polygon(
        p,
        vec![
            plane.point(0., 0.),
            plane.point(1., 0.),
            plane.point(1., 1.),
            plane.point(0., 1.),
        ],
        color,
        stroke,
    );
}

fn ellipse(p: &Painter, center: Pos2, radius: Vec2, fill: Color32) {
    let points: Vec<Pos2> = (0..24)
        .map(|i| {
            let angle = i as f32 / 24. * std::f32::consts::TAU;
            center + vec2(radius.x * angle.cos(), radius.y * angle.sin())
        })
        .collect();
    polygon(p, points, fill, Stroke::NONE);
}

fn gradient(p: &Painter, points: &[Pos2], direction: Vec2, first: Color32, last: Color32) {
    let bounds = Rect::from_points(points);
    let half = bounds.size() * direction * 0.5;
    let mut mesh = Mesh::default();
    for &pos in points {
        let factor =
            ((pos - (bounds.center() - half)).dot(half) / (2. * half.length_sq())).clamp(0., 1.);
        mesh.colored_vertex(pos, first.lerp_to_gamma(last, factor));
    }
    for i in 2..points.len() as u32 {
        mesh.add_triangle(0, i - 1, i);
    }
    p.add(Shape::mesh(mesh));
}

pub fn paint_floor(p: &Painter, volumes: &[Volume], dark: bool) {
    let ink = if dark {
        Color32::from_rgb(254, 253, 248)
    } else {
        Color32::from_rgb(138, 128, 107)
    };
    for volume in volumes {
        let lift = volume.lift;
        let origin = volume.floor + vec2(-4., 5.) * lift * volume.scale;
        let front = volume.front.horizontal;
        let depth = volume.face.horizontal;
        let tail = vec2(-10., 10.) * volume.scale * (1. + 0.55 * lift);
        gradient(
            p,
            &[
                origin,
                origin + front,
                origin + front + depth,
                origin + depth,
                origin + depth + tail,
                origin + tail,
            ],
            vec2(-1., 1.),
            ink.gamma_multiply((0.12 - 0.065 * lift) * volume.opacity),
            Color32::TRANSPARENT,
        );
        // 已排除：用 ellipse 画"接触阴影"在这套斜轴测几何上会变成一排圆形色块，
        // 看起来像石头而不是影子（2026-11-23 实测，见 STATE 已验证事实）。
        // 正确做法是把接触阴影并进上面那张地面梯度多边形，而不是另画椭圆。
    }
}

// Sample the same center crop on the shelf, in flight and on the detail page.
fn cover_uv(texture: &TextureHandle, face: Vec2) -> Rect {
    let scale = texture.size_vec2() / face;
    Rect::from_center_size(pos2(0.5, 0.5), Vec2::splat(scale.min_elem()) / scale)
}

fn cover_rows(face: Vec2) -> Vec<f32> {
    let mut rows: Vec<_> = (0..=32).map(|i| i as f32 / 32.).collect();
    for i in 1..=8 {
        let y = 8. / face.y * (1. - (i as f32 * std::f32::consts::FRAC_PI_2 / 8.).cos());
        rows.extend([y, 1. - y]);
    }
    rows.sort_by(f32::total_cmp);
    rows.dedup();
    rows
}

fn corner_inset(face: Vec2, v: f32) -> f32 {
    // Fold to the nearer edge on a dyadic grid: mirrored `v` snap to the same
    // representable value, so the cover mesh stays exactly symmetric.
    let edge = ((v.min(1. - v)) * 4096.).round() / 4096.;
    let corner = (edge * face.y / 8.).min(1.);
    (8. / face.x) * (1. - (1. - (1. - corner).powi(2)).max(0.).sqrt())
}

fn cover_mesh(
    texture: &TextureHandle,
    face: Vec2,
    rows: &[f32],
    columns: u32,
    point: impl Fn(f32, f32) -> Pos2,
) -> Mesh {
    let uv = cover_uv(texture, face);
    let mut mesh = Mesh::with_texture(texture.id());
    mesh.vertices.reserve(rows.len() * columns as usize);
    for &v in rows {
        let inset = corner_inset(face, v);
        for col in 0..columns {
            let u = inset + (1. - 2. * inset) * col as f32 / (columns - 1) as f32;
            mesh.vertices.push(Vertex {
                pos: point(u, v),
                uv: uv.lerp_inside(vec2(u, v)),
                color: Color32::WHITE,
            });
        }
    }
    for row in 0..rows.len() as u32 - 1 {
        for col in 0..columns - 1 {
            let a = row * columns + col;
            mesh.indices
                .extend([a, a + columns, a + 1, a + 1, a + columns, a + columns + 1]);
        }
    }
    mesh
}

fn shelf_face(plane: Plane) -> Vec2 {
    vec2(plane.horizontal.x / 0.66, plane.vertical.y)
}

pub fn paint_art(p: &Painter, plane: Plane, face: Vec2, texture: &TextureHandle, paper: Color32) {
    paint_art_alpha(p, plane, face, texture, paper, 1.);
}

/// Same rounded center crop as `paint_art`, with the artwork tinted by
/// `alpha` so covers can fade in over the paper backing placeholder.
pub fn paint_art_alpha(
    p: &Painter,
    plane: Plane,
    face: Vec2,
    texture: &TextureHandle,
    paper: Color32,
    alpha: f32,
) {
    // A flat panel needs only corner rows and two columns.
    let mut rows = vec![0., 1.];
    for i in 1..=8 {
        let y = 8. / face.y * (1. - (i as f32 * std::f32::consts::FRAC_PI_2 / 8.).cos());
        rows.extend([y, 1. - y]);
    }
    rows.sort_by(f32::total_cmp);
    let mut mesh = cover_mesh(texture, face, &rows, 2, |u, v| plane.point(u, v));
    let tint = Color32::from_rgba_unmultiplied(
        255,
        255,
        255,
        (alpha.clamp(0., 1.) * 255.).round() as u8,
    );
    for vertex in &mut mesh.vertices {
        vertex.color = tint;
    }
    paint_paper_mesh(p, mesh, paper);
}

fn paint_paper_mesh(p: &Painter, artwork: Mesh, paper: Color32) {
    let mut backing = Mesh {
        indices: artwork.indices.clone(),
        ..Default::default()
    };
    for vertex in &artwork.vertices {
        backing.colored_vertex(vertex.pos, paper);
    }
    p.add(Shape::mesh(backing));
    p.add(Shape::mesh(artwork));
}

/// Text is transformed along with its physical face, including atlas UVs.
fn affine_text(
    p: &Painter,
    plane: Plane,
    pos: Pos2,
    text: &str,
    size: f32,
    color: Color32,
    mono: bool,
) {
    let font = if mono {
        FontId::monospace(size)
    } else {
        FontId::proportional(size)
    };
    let galley = p.layout_no_wrap(text.into(), font, color);
    let atlas = p.ctx().fonts(|f| f.font_image_size());
    for row in &galley.rows {
        let mut mesh = row.visuals.mesh.clone();
        for vertex in &mut mesh.vertices {
            let local = vertex.pos + row.pos.to_vec2() + pos.to_vec2();
            vertex.pos = plane.point(local.x, local.y);
            vertex.uv.x /= atlas[0] as f32;
            vertex.uv.y /= atlas[1] as f32;
        }
        p.add(Shape::mesh(mesh));
    }
}

pub fn paint_volume(
    p: &Painter,
    volume: &Volume,
    title: &str,
    cover: Option<&TextureHandle>,
    dark: bool,
    hide_cover: bool,
    selected: bool,
) {
    let mut faded = p.clone();
    faded.multiply_opacity(volume.opacity);
    let p = &faded;
    let scale = volume.scale;
    let face_unit = Plane {
        origin: volume.face.origin,
        horizontal: vec2(0.66, -0.30) * scale,
        vertical: Vec2::Y * scale,
    };
    let spine_unit = Plane {
        origin: volume.front.origin,
        horizontal: vec2(1., 0.10) * scale,
        vertical: Vec2::Y * scale,
    };
    let ink = if dark {
        Color32::from_gray(238)
    } else {
        Color32::from_rgb(33, 44, 50)
    };
    let muted = if dark {
        Color32::from_gray(168)
    } else {
        Color32::from_rgb(99, 111, 119)
    };
    let paper = if dark {
        Color32::from_gray(36)
    } else {
        Color32::from_rgb(254, 253, 248)
    };
    let spine = if dark {
        Color32::from_gray(45)
    } else {
        Color32::from_rgb(242, 240, 233)
    };
    // A hairline edge along the slanted paper and spine planes lets egui's
    // anti-aliased stroke hide the pixel stairs `Stroke::NONE` left behind.
    let edge_line = if dark {
        Color32::from_gray(88).gamma_multiply(0.55)
    } else {
        Color32::from_rgb(198, 190, 172).gamma_multiply(0.9)
    };
    // 景深压暗：每退一槽 13%，把注意力压回焦点书（Gemini 建议 12–15%）。
    // 书放大后如果没有这层，两侧书会和焦点书抢视线。
    let dim = 1. - 0.13 * volume.distance.abs().min(3.);
    let paper = paper.gamma_multiply(dim);
    let spine = spine.gamma_multiply(dim);
    let edge_line = edge_line.gamma_multiply(dim);
    flat_stroked(p, volume.face, paper, Stroke::new(0.8, edge_line));
    let mut print = p.clone();
    print.multiply_opacity(volume.ink);
    if !hide_cover {
        if let Some(texture) = cover {
            paint_art(
                &print,
                volume.cover,
                shelf_face(volume.cover),
                texture,
                paper,
            );
            // The cover art ends flush with the face; a soft frame keeps the
            // printed edge crisp against the paper underneath.
            let frame = volume.cover;
            polygon(
                p,
                vec![
                    frame.point(0., 0.),
                    frame.point(1., 0.),
                    frame.point(1., 1.),
                    frame.point(0., 1.),
                ],
                Color32::TRANSPARENT,
                Stroke::new(0.75, edge_line),
            );
        } else {
            flat(&print, volume.cover, ink);
            let letters: String = title.chars().take(6).collect();
            affine_text(
                &print,
                Plane {
                    origin: volume.cover.point(0.08, 0.36),
                    ..face_unit
                },
                Pos2::ZERO,
                &letters,
                10.,
                paper,
                false,
            );
        }
    }
    // Below ~0.72 the engraved index would render as ≈4px noise, so tiny
    // shelves swap both numbers for one quiet tick instead.
    let tiny = scale < 0.72;
    if tiny {
        let a = face_unit.point(0.08, 0.07);
        let b = face_unit.point(0.30, 0.07);
        p.line_segment([a, b], Stroke::new(1., muted.gamma_multiply(0.6)));
    } else {
        affine_text(
            &print,
            face_unit,
            pos2(10., 9.),
            &format!("NO.{:03}", volume.index + 1),
            8.,
            muted,
            true,
        );
    }
    let edge = if dark {
        Color32::from_gray(60)
    } else {
        Color32::from_rgb(227, 223, 211)
    };
    gradient(
        p,
        &volume.cap,
        Vec2::X,
        edge,
        if dark { spine } else { paper },
    );
    p.add(Shape::closed_line(
        volume.cap.to_vec(),
        Stroke::new(0.4, muted.gamma_multiply(0.24)),
    ));
    if !tiny {
        // Ultra-fine page-edge striations inside the top cap suggest the
        // stacked paper leaves instead of a blank extruded box.
        let striation = muted.gamma_multiply(if dark { 0.16 } else { 0.18 });
        let depth = volume.cap[3] - volume.cap[0];
        for f in [0.28, 0.52, 0.76] {
            let a = volume.cap[0] + (volume.cap[1] - volume.cap[0]) * f;
            p.line_segment([a + depth * 0.07, a + depth * 0.93], Stroke::new(0.4, striation));
        }
    }
    flat_stroked(p, volume.front, spine, Stroke::new(0.8, edge_line));
    // A whisper of a gutter line where the cover board meets the spine gives
    // each volume its own silhouette against the neighbour's white edge.
    p.line_segment(
        [volume.front.point(1., 0.), volume.front.point(1., 1.)],
        Stroke::new(0.9, muted.gamma_multiply(if dark { 0.30 } else { 0.42 })),
    );
    if !tiny {
        affine_text(
            &print,
            spine_unit,
            pos2(2., 9.),
            &format!("{:02}", volume.index + 1),
            8.,
            muted,
            true,
        );
    }
    let chars: Vec<char> = title.chars().take(9).collect();
    // Keep the vertical title clear of the bottom ring on tight heights.
    let top = (volume.height / scale - chars.len() as f32 * 11. - 14.) / 2.;
    for (i, ch) in chars.iter().enumerate() {
        affine_text(
            &print,
            spine_unit,
            pos2((volume.thickness / scale - 9.) / 2., top + i as f32 * 11.),
            &ch.to_string(),
            9.,
            ink,
            false,
        );
    }
    // Neighbor contact AO: a narrow dark-to-transparent strip along the
    // gutter edge of the cover face gives the board joint natural depth.
    let gutter = volume.face.sub(0., 0., 0.08, 1.);
    gradient(
        p,
        &[
            gutter.point(0., 0.),
            gutter.point(1., 0.),
            gutter.point(1., 1.),
            gutter.point(0., 1.),
        ],
        Vec2::X,
        Color32::from_black_alpha(if dark { 56 } else { 26 }),
        Color32::TRANSPARENT,
    );
    // Gentle focus dimming keeps the centered book crisp while neighbors
    // recede, scaled by how far the lane sits from the focus.
    let dim = (volume.distance.abs().min(1.5) / 1.5) * if dark { 0.08 } else { 0.055 };
    if dim > 0.005 {
        let shade = Color32::from_black_alpha((dim * 255.).round() as u8);
        flat(p, volume.face, shade);
        flat(p, volume.front, shade);
    }
    p.circle_stroke(volume.front.point(0.5, 0.94), 2.2, Stroke::new(0.6, muted));
    if selected {
        p.circle_filled(
            volume.front.point(0.5, 0.94),
            3.,
            if dark {
                Color32::from_rgb(156, 199, 177)
            } else {
                Color32::from_rgb(44, 90, 70)
            },
        );
    }
}

/// A single GPU mesh peels, flies, and settles without rasterizing frames.
pub fn paint_flight(
    p: &Painter,
    from: Plane,
    to: Plane,
    t: f32,
    texture: &TextureHandle,
    dark: bool,
) {
    let (start, end) = (from.bounds(), to.bounds());
    let travel = motion::travel(t);
    let depart = motion::cubic((t - 0.02) / 0.32, 0.645, 0.045, 0.355, 1.);
    let contact = motion::cubic((t - 0.58) / 0.42, 0.645, 0.045, 0.355, 1.);
    let envelope = 1. - motion::smooth((t - 0.42) / 0.16);
    let shelf = crate::params::params().shelf.clone();
    let lift = shelf.lift_px * motion::smooth(t / 0.18) * envelope;
    let roll = -shelf.roll_rad * (t * std::f32::consts::PI).sin() * envelope;
    let (sin, cos) = roll.sin_cos();
    let arc = (shelf.arc_base + start.center().distance(end.center()) * shelf.arc_pct).min(shelf.arc_max)
        * (travel * std::f32::consts::PI).sin();
    let center = start.center().lerp(end.center(), travel) - vec2(0., arc + lift);
    let bounds = start.size() + (end.size() - start.size()) * travel;
    let face = shelf_face(from) + (end.size() - shelf_face(from)) * travel;
    // A two-layer soft drop shadow tracks the page: it fades in on takeoff
    // and dissolves as the paper lands into its resting shadow.
    let elevation = ((lift + arc) / 48.).clamp(0., 1.) * (1. - contact * 0.7);
    if elevation > 0.001 {
        let ground = center + vec2(5., 7.) * elevation + vec2(0., (arc + lift) * 0.85);
        let radius = vec2(bounds.x * 0.55, bounds.y * 0.30) * (0.8 + 0.5 * elevation);
        let gain = if dark { 1.35 } else { 1. };
        ellipse(
            p,
            ground,
            radius * 1.4,
            Color32::from_black_alpha((elevation * 24. * gain).round() as u8),
        );
        ellipse(
            p,
            ground,
            radius,
            Color32::from_black_alpha((elevation * 42. * gain).round() as u8),
        );
    }
    let columns = shelf.columns.max(3) as u32;
    let rows = cover_rows(face);
    let mut mesh = cover_mesh(texture, face, &rows, columns, |u, v| {
        let scale = 300. / (300. - motion::paper_depth(t, u, v));
        let (u, v) = (0.5 + (u - 0.5) * scale, 0.5 + (v - 0.5) * scale);
        let begin_uv = (from.point(u, v) - start.min) / start.size();
        let flat = vec2(u, v);
        let end_uv = (to.point(u, v) - end.min) / end.size();
        let free = begin_uv + (flat - begin_uv) * depart;
        let local = (free + (end_uv - free) * contact - Vec2::splat(0.5)) * bounds;
        center + vec2(local.x * cos - local.y * sin, local.x * sin + local.y * cos)
    });
    // Directional lighting from finite differences of the paper depth map:
    // ripple highlights and crease shading at zero shader cost.
    for (i, vertex) in mesh.vertices.iter_mut().enumerate() {
        let v = rows[i / columns as usize];
        let inset = corner_inset(face, v);
        let u = inset + (1. - 2. * inset) * (i % columns as usize) as f32 / (columns - 1) as f32;
        let depth = motion::paper_depth(t, u, v);
        let shade = motion::paper_depth(t, u + 0.03, v + 0.03) - depth + depth / 28.;
        let brightness = (1. - shade * shelf.shade_gain).clamp(0.91, 1.04);
        let channel = (brightness * 255.).round().clamp(0., 255.) as u8;
        vertex.color = Color32::from_rgb(channel, channel, channel);
    }
    paint_paper_mesh(
        p,
        mesh,
        if dark {
            Color32::from_gray(36)
        } else {
            Color32::from_rgb(254, 253, 248)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thousand_books_have_bounded_geometry_and_exact_hit_planes() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(520., 450.));
        for f in [0., 999.9, 1000., -0.1, -1000.] {
            let volumes = geometry(rect, 1000, f, 1.);
            assert!(volumes.len() < 45);
            let selected = volumes.iter().find(|v| v.lane == f.round() as i32).unwrap();
            assert_eq!(selected.index, (f.round() as i32).rem_euclid(1000) as usize);
            assert!(selected.cover.contains(selected.cover.point(0.5, 0.5)));
        }
    }
    #[test]
    fn small_collections_show_each_book_once_at_rest_and_crossfade_the_wrap() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(740., 560.));
        for count in 2..=9 {
            for focus in [-3., -1., 0., 1., count as f32 + 1.] {
                let volumes = geometry(rect, count, focus, 1.);
                assert_eq!(volumes.len(), count);
                let identities: std::collections::HashSet<_> =
                    volumes.iter().map(|v| v.index).collect();
                assert_eq!(identities.len(), count);
                assert!(volumes.iter().all(|v| v.opacity == 1.));
            }
            for focus in [-2.8, -0.6, 0.1, 0.8, count as f32 + 0.7] {
                let volumes = geometry(rect, count, focus, 1.);
                assert_eq!(volumes.len(), count + 1);
                for index in 0..count {
                    let opacity: f32 = volumes
                        .iter()
                        .filter(|v| v.index == index)
                        .map(|v| v.opacity)
                        .sum();
                    assert!((opacity - 1.).abs() < 0.0001);
                }
                let selected = volumes
                    .iter()
                    .find(|v| v.lane == focus.round() as i32)
                    .unwrap();
                assert_eq!(
                    selected.index,
                    (focus.round() as i32).rem_euclid(count as i32) as usize
                );
                assert!(selected.contains(selected.cover.point(0.5, 0.5)));
            }
        }
    }
    #[test]
    fn small_collection_geometry_is_continuous_at_selection_and_wrap_boundaries() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(740., 560.));
        for count in 2..=9 {
            for seam in [-1., -0.5, 0., 0.5, 1., count as f32] {
                let before = geometry(rect, count, seam - 0.0001, 1.);
                let after = geometry(rect, count, seam + 0.0001, 1.);
                for old in &before {
                    if let Some(new) = after.iter().find(|v| v.lane == old.lane) {
                        assert_eq!(old.index, new.index);
                        for (a, b) in [
                            (old.front.origin, new.front.origin),
                            (old.cover.point(0.5, 0.5), new.cover.point(0.5, 0.5)),
                        ] {
                            assert!(
                                a.distance(b) < 0.1,
                                "count={count}, seam={seam}, lane={}",
                                old.lane
                            );
                        }
                        assert!((old.opacity - new.opacity).abs() < 0.001);
                    } else {
                        assert!(old.opacity < 0.001);
                    }
                }
                for new in &after {
                    if !before.iter().any(|v| v.lane == new.lane) {
                        assert!(new.opacity < 0.001);
                    }
                }
            }
        }
    }
    #[test]
    fn volumes_expose_lift_distance_and_cover_corners_stay_inside_the_crop() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(740., 560.));
        let volumes = geometry(rect, 9, 2., 1.);
        for volume in &volumes {
            assert!((0. ..=1.).contains(&volume.lift));
            assert!((volume.distance - (volume.lane as f32 - 2.)).abs() < 0.0001);
            if volume.distance.abs() >= 1.48 {
                assert_eq!(volume.lift, 0.);
            }
        }
        let peak = volumes.iter().map(|v| v.lift).fold(0.0_f32, f32::max);
        assert!((peak - 1.).abs() < 0.001);
        let face = vec2(118., 166.);
        assert_eq!(corner_inset(face, 0.5), 0.);
        assert_eq!(corner_inset(face, 0.03), corner_inset(face, 0.97));
        assert!((0. ..0.5).contains(&corner_inset(face, 0.)));
    }
}
