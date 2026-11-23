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
    /// 有效比例 = 全局 shelf 比例 × 按槽位缩放，文字与装饰都按它排版。
    scale: f32,
    ink: f32,
    opacity: f32,
    /// 屏幕边缘柔边渐隐系数（0..=1），在 paint 层乘到 opacity 上。
    /// 与 opacity 分开存：contains()/环绕交叉淡入淡出都只看 opacity。
    fade: f32,
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

/// 抽书期间需要在飞行封面上方重绘的邻书（P2-4）：
/// 不只右侧前景，左侧被遮的书也收进来——飞行 mesh 会把它们的剪影裁成
/// 碎块，带 alpha 淡出重绘让被遮书保持完整轮廓。
pub fn foreground(volumes: &[Volume], lane: i32, cover: Plane) -> Vec<Volume> {
    volumes
        .iter()
        .filter(|v| v.lane != lane && v.bounds().intersects(cover.bounds()))
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

/// 按距焦点的槽位数缩小整本书：±1 槽 0.90x、±2 槽 0.82x，
/// 之后每退一槽再乘 0.92（下限 0.55）。边缘书"退到背景"而不只是变暗。
/// 用连续 distance 而不是取整槽号，拖动中途形变保持平滑。
fn slot_shrink(distance: f32) -> f32 {
    let d = distance.abs();
    if d <= 1. {
        1. - 0.10 * d
    } else if d <= 2. {
        0.90 - 0.08 * (d - 1.)
    } else {
        (0.82 * 0.92_f32.powf(d - 2.)).max(0.55)
    }
}

/// 反透视深度压缩：斜轴测投影下厚度方向（书脊 spine 宽 + 封面 depth 向量）
/// 不随距离衰减会显得远端书一样厚。按 1/(1+0.22·|d|) 连续压缩：
/// d=0 → 1.00，±1 → 0.82，±2 → 0.69，±3 → 0.60（渐近但永不归零）。
/// 只压"厚度轴"，书高与书宽(封面面内)不动，远处书更扁、焦点书更立体。
fn depth_squeeze(distance: f32) -> f32 {
    1. / (1. + 0.22 * distance.abs())
}

/// 书靠近 shelf_area 左右边缘 0..band 时按 smoothstep 渐隐，
/// 取代之前被 painter clip 硬裁的生硬边缘。band = min(48, 宽度 5%)。
/// 以书"靠内"的边衡量：从左侧滑入的书在左缘离开左 band 后即全亮。
fn edge_fade(bounds: Rect, area: Rect) -> f32 {
    let band = 48.0_f32.min(area.width() * 0.05).max(1.);
    let left = motion::smooth((bounds.max.x - area.left()) / band);
    let right = motion::smooth((area.right() - bounds.min.x) / band);
    left.min(right)
}

pub fn geometry(area: Rect, count: usize, focus: f32, entrance: f32) -> Vec<Volume> {
    if count == 0 {
        return Vec::new();
    }
    let focus = if count == 1 { 0. } else { focus };
    let scale = shelf_scale(area);
    let width = area.width();
    // 相邻书最多遮掉约 28%：pitch 至少要留出封面可见宽度，否则边缘书只剩一条细缝。
    let pitch_base = lane_pitch_for(area, count);
    let spine = lane_pitch(area) - 11. * scale;
    let cover = (width / scale * 0.32).clamp(100., 190.) * scale;
    let height = (cover - 20. * scale) * 166. / 118. + 54. * scale;
    let base_y = -cover * -0.30 + height * 0.60 + 12. * scale;
    // 按可用高度收口：书放大后（cover 190 * scale 2.6）整摞高度会超过画布，
    // 表现为书的底部被裁掉、上方留白过大。这里等比缩到能完整放下的尺寸。
    let stacked = base_y + height;
    let fit = if stacked > area.height() * 0.98 {
        (area.height() * 0.98) / stacked
    } else {
        1.
    };
    let cover = cover * fit;
    let height = height * fit;
    let base_y = base_y * fit;
    // 相邻书最多遮掉约 28%：pitch 至少要留出封面可见宽度，否则边缘书只剩一条细缝。
    let pitch = pitch_base.max(cover * 0.66 * 0.72);
    // 垂直居中：书群的锚点原本从 area.top 往下挂，底部的展签区会被压住。
    // 这里把剩余高度按 45% 放到上方，让整摞书居中而不是贴底。
    let stacked_now = base_y + height;
    let v_pad = ((area.height() - stacked_now) * 0.45).max(0.);
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
            let shrink = slot_shrink(lane as f32);
            let pull = cover
                * shelf.pull_base
                * weight
                * (shelf.pull_linear - shelf.pull_quad * weight)
                * shrink;
            let footprint = (spine + cover * 0.66) * shrink;
            let cx = start_x + lane as f32 * pitch + (spine + cover * 0.66) / 2.
                - pull * 0.66;
            left = left.min(cx - footprint / 2.);
            right = right.max(cx + footprint / 2.);
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
            let shrink = slot_shrink(distance);
            let weight = motion::weight(distance);
            let pull = cover
                * shelf.pull_base
                * weight
                * (shelf.pull_linear - shelf.pull_quad * weight)
                * shrink;
            let lift = motion::smooth(weight);
            // P2-4：半抬的邻书再补一程抬升（焦点书 lift=1 不受影响）。
            // 被焦点书遮住大半的书只露出下缘一角，读成"漂浮色块"；
            // 抬高一点让被遮的书脊下缘和封面角连贯起来。
            let lift = lift + (1. - lift) * weight * 0.8;
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
            let load = -9. * crest * crest * (1. - ink) * scale * shrink;
            // 对称下沉：|rail| 项让两侧一起矮下去，避免整排"往右下坠"。
            let rail_y = (rail.abs() * shelf.rail_linear * 0.8 + rail * rail * shelf.rail_quad) * scale;
            // 槽位缩放绕"未缩放占地中心"进行：底边对齐 rail、水平居中，
            // 所以缩小只让槽内空隙对称变宽，不会让书错位或互穿。
            let footprint = spine + cover * 0.66;
            let spine_b = spine * shrink;
            let cover_b = cover * shrink;
            let height_b = height * shrink;
            let center_x = start_x + distance * pitch + footprint / 2.;
            let footprint_b = footprint * shrink;
            // floor 沿用原语义：槽位锚点（不含 pull），阴影画在书"应停靠"的位置。
            let floor = pos2(
                center_x - footprint_b / 2.,
                area.top() + v_pad + base_y + height + rail_y,
            );
            let origin = pos2(
                center_x - footprint_b / 2. - pull * 0.66,
                area.top() + v_pad + base_y + pull * 0.30 - height_b * 0.60 * lift + rail_y + load,
            );
            let squeeze = depth_squeeze(distance);
            let front = Plane {
                origin,
                horizontal: vec2(spine_b, spine_b * 0.10) * squeeze,
                vertical: vec2(0., height_b),
            };
            let face = Plane {
                origin: front.point(1., 0.),
                horizontal: vec2(cover_b * 0.66, cover_b * -0.30) * squeeze,
                vertical: vec2(0., height_b),
            };
            let volume = Volume {
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
                thickness: spine_b,
                height: height_b,
                scale: scale * shrink,
                ink,
                opacity,
                fade: 1.,
                lift,
                distance,
                floor,
            };
            Some(Volume {
                fade: edge_fade(volume.bounds(), area),
                ..volume
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
    let fade_dir = vec2(-1., 1.).normalized();
    for volume in volumes {
        let lift = volume.lift;
        let origin = volume.floor + vec2(-4., 5.) * lift * volume.scale;
        let front = volume.front.horizontal;
        let depth = volume.face.horizontal;
        let tail = vec2(-10., 10.) * volume.scale * (1. + 0.55 * lift);
        let ambient = (0.12 - 0.065 * lift) * volume.opacity * volume.fade;
        // 接触阴影并进同一个 mesh（不是独立 Shape，更不是 ellipse——独立椭圆
        // 在斜轴测下会渲染成一排圆点，2026-11-23 实测已回退）。
        // 贴地前缘 = 书底 footprint 的 viewer 侧两条边 p0→p1(书脊底)→p2(封面底)，
        // 沿 fade_dir 扩一条 ≤6pt 的窄带：近边 alpha=1.5×环境（<1.6×上限），
        // 远边回落到环境值，形成"重量锚点"而不是飘着的书。
        let contact = ambient * 1.58;
        let band = 5.5_f32;
        let p0 = origin;
        let p1 = origin + front;
        let p2 = origin + front + depth;
        let p3 = origin + depth;
        let p4 = origin + depth + tail;
        let p5 = origin + tail;
        let b0 = p0 + fade_dir * band;
        let b1 = p1 + fade_dir * band;
        let b2 = p2 + fade_dir * band;
        let mut mesh = Mesh::default();
        // row 0：贴地前缘，最深
        for pos in [p0, p1, p2] {
            mesh.colored_vertex(pos, ink.gamma_multiply(contact));
        }
        // row 1：窄带外缘 + 原多边形尾端，沿用原 (-1,1) 方向的环境梯度
        let far = [b0, b1, b2, p3, p4, p5];
        let bounds = Rect::from_points(&far);
        let half = bounds.size() * vec2(-1., 1.) * 0.5;
        for &pos in &far {
            let factor = ((pos - (bounds.center() - half)).dot(half)
                / (2. * half.length_sq()))
            .clamp(0., 1.);
            mesh.colored_vertex(
                pos,
                ink.gamma_multiply(ambient).lerp_to_gamma(Color32::TRANSPARENT, factor),
            );
        }
        // 窄带：p0p1b0 + p1p2b2 两个四边形
        mesh.add_triangle(0, 1, 4);
        mesh.add_triangle(0, 4, 3);
        mesh.add_triangle(1, 2, 5);
        mesh.add_triangle(1, 5, 4);
        // 远处区域：b0b1b2p3p4p5 扇形（沿用原多边形扇形逻辑）
        for i in 4..8u32 {
            mesh.add_triangle(3, i, i + 1);
        }
        p.add(Shape::mesh(mesh));
    }
}

// Sample the same center crop on the shelf, in flight and on the detail page.
fn cover_uv(texture: &TextureHandle, face: Vec2) -> Rect {
    let scale = texture.size_vec2() / face;
    Rect::from_center_size(pos2(0.5, 0.5), Vec2::splat(scale.min_elem()) / scale)
}

// egui-wgpu uploads every texture with a single mip level and a linear
// sampler, so a 500px cover shrunk into a ~120px oblique shelf face aliases.
// GPU mip generation would need a custom blit pipeline inside the vendored
// renderer; the lightweight stand-in is a CPU mip chain uploaded as extra
// textures, with the level picked per paint from the projected footprint.
/// Pre-scaled tiers of one cover: levels[i] is 1/2^i of the artwork.
pub struct CoverMips {
    levels: Vec<TextureHandle>,
}
impl CoverMips {
    /// [base, /2, /4, ...] textures for image; the base itself is always
    /// levels[0] so callers keep one lookup path for every paint.
    pub fn upload(ctx: &Context, name: &str, image: &ColorImage) -> Self {
        let levels = cover_mip_tiers(image)
            .into_iter()
            .enumerate()
            .map(|(i, tier)| ctx.load_texture(format!("{name}-m{i}"), tier, TextureOptions::LINEAR))
            .collect();
        Self { levels }
    }
    /// GPU memory across every tier, for the cover cache byte budget.
    pub fn bytes(&self) -> usize {
        self.levels
            .iter()
            .map(|level| level.size()[0] * level.size()[1] * 4)
            .sum()
    }
    /// Full-resolution artwork: flight and the detail card want sharpness.
    pub fn base(&self) -> &TextureHandle {
        &self.levels[0]
    }
    /// Deepest tier whose center-crop texels still oversample the on-screen
    /// plane footprint (in points) by more than MIP_PICK_RATIO on either
    /// axis — the isotropic-mip stand-in for the oblique squash. Smaller
    /// picks trade the remaining shimmer for softness.
    pub fn pick(&self, plane: Plane, face: Vec2, ppp: f32) -> &TextureHandle {
        let screen = vec2(
            plane.horizontal.length() * ppp,
            plane.vertical.length() * ppp,
        );
        let mut level = &self.levels[0];
        for next in &self.levels[1..] {
            let scale = next.size_vec2() / face;
            let texels = face * scale.min_elem();
            if texels.x / screen.x.max(1.) > MIP_PICK_RATIO
                || texels.y / screen.y.max(1.) > MIP_PICK_RATIO
            {
                level = next;
            } else {
                break;
            }
        }
        level
    }
}
/// The cover texture for one paint call: mip tier when the cache has one,
/// the plain handle otherwise. face is the un-projected cover size that
/// cover_uv crops against (shelf volumes pass shelf_face(plane)).
pub fn cover_pick<'a>(
    mips: Option<&'a CoverMips>,
    plain: Option<&'a TextureHandle>,
    plane: Plane,
    face: Vec2,
    ppp: f32,
) -> Option<&'a TextureHandle> {
    mips.map(|mips| mips.pick(plane, face, ppp)).or(plain)
}
/// Halve a tier each step until the small edge would drop below 64px, capped
/// at three downscales — enough for shelf faces from ~60px to full screen.
fn cover_mip_tiers(image: &ColorImage) -> Vec<ColorImage> {
    let mut tiers = vec![image.clone()];
    for _ in 0..3 {
        let (w, h) = (tiers.last().unwrap().width(), tiers.last().unwrap().height());
        if w / 2 < 64 || h / 2 < 64 {
            break;
        }
        tiers.push(halve(tiers.last().unwrap()));
    }
    tiers
}
/// 2x box downsample in premultiplied alpha so dark edges do not bleed.
fn halve(image: &ColorImage) -> ColorImage {
    let (w, h) = (image.width(), image.height());
    let (tw, th) = (w / 2, h / 2);
    let mut pixels = Vec::with_capacity(tw * th);
    for y in 0..th {
        for x in 0..tw {
            let mut sum = [0u32; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = image.pixels[(y * 2 + dy) * w + x * 2 + dx];
                    sum[3] += p.a() as u32;
                    sum[0] += p.r() as u32 * p.a() as u32;
                    sum[1] += p.g() as u32 * p.a() as u32;
                    sum[2] += p.b() as u32 * p.a() as u32;
                }
            }
            let a = (sum[3] + 2) / 4;
            // sum stores r*a etc, so un-premultiplying the 2x2 mean only
            // divides by the mean alpha — an extra *255 would clamp to white.
            let channel = |premul: u32| {
                (((premul + 2) / 4 + a / 2) / a.max(1)).min(255) as u8
            };
            pixels.push(Color32::from_rgba_premultiplied(
                channel(sum[0]),
                channel(sum[1]),
                channel(sum[2]),
                a as u8,
            ));
        }
    }
    ColorImage {
        size: [tw, th],
        source_size: vec2(w as f32 / 2., h as f32 / 2.),
        pixels,
    }
}
/// Step to the next tier only while texels still oversample screen pixels by
/// more than this on the more-compressed axis — ceil(lod) mip selection.
const MIP_PICK_RATIO: f32 = 1.5;

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

pub fn shelf_face(plane: Plane) -> Vec2 {
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
    faded.multiply_opacity(volume.opacity * volume.fade);
    let p = &faded;
    let scale = volume.scale;
    // 深度压缩同步到排版基准面：文字与装饰按 unit 向量映射到面上，
    // 不乘 squeeze 会随距离漂出压扁后的轮廓。
    let squeeze = depth_squeeze(volume.distance);
    let face_unit = Plane {
        origin: volume.face.origin,
        horizontal: vec2(0.66, -0.30) * scale * squeeze,
        vertical: Vec2::Y * scale,
    };
    let spine_unit = Plane {
        origin: volume.front.origin,
        horizontal: vec2(1., 0.10) * scale * squeeze,
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
    // 受光棱线：取代整圈 0.8px 闭合描边（闭合描边让书像贴纸）。
    // 光源在右上（depth 向量指向右上）：只描各面的"上缘 + 靠外一侧"——
    // 封面是上缘+右缘，书脊是上缘+外侧左缘；书脊右缘是封面沟槽暗线。
    let ridge_paper = paper.lerp_to_gamma(Color32::WHITE, 0.5);
    let ridge_spine = spine.lerp_to_gamma(Color32::WHITE, 0.5);
    let lit_edges = |p: &Painter, plane: Plane, right: bool, color: Color32| {
        p.line_segment(
            [plane.point(0., 0.), plane.point(1., 0.)],
            Stroke::new(0.9, color),
        );
        let u = if right { 1. } else { 0. };
        p.line_segment(
            [plane.point(u, 0.), plane.point(u, 1.)],
            Stroke::new(0.9, color),
        );
    };
    flat(p, volume.face, paper);
    lit_edges(p, volume.face, true, ridge_paper);
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
    flat(p, volume.front, spine);
    lit_edges(p, volume.front, false, ridge_spine);
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
    // recede, scaled by how far the lane sits from the focus. P2-4 叠加一项
    // 未抬升系数：被焦点书遮住大半的邻书沉得更深，露出的一角不再读成
    // 漂浮的色块。
    let dim = ((volume.distance.abs().min(1.5) / 1.5) * 0.055 + (1. - volume.lift) * 0.055)
        * if dark { 1.45 } else { 1. };
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

/// R1 fallback: egui clips with axis-aligned `Rect`s only — the renderer
/// turns every clip into a scissor rect (egui-wgpu `ScissorRect`), so a
/// `Shape::Path` clip cannot express the reference's slanted trapezoid
/// (archive_motion.dart:338). We slice the reveal quad into horizontal
/// bands and clip each band by a `Rect`; every band's trailing edge uses
/// the conservative (unrevealed-side) boundary so the painted region is
/// always a subset of the true quad — content is never cut short.
/// With `dir = -1` the bands mirror around `rect.center().x`.
/// Minimum band count; the real count adapts so a stair step of the
/// clip edge never exceeds `SPREAD_BAND_STEP_PX` horizontally
/// (`slant / bands <= 8px`). 8 fixed bands left ~slant/8-px steps
/// visible mid-sweep.
const SPREAD_BANDS_MIN: u32 = 8;
/// Horizontal px budget for one stair step of the clip edge.
const SPREAD_BAND_STEP_PX: f32 = 8.;

/// Band count for a reveal of width `w`: at least `SPREAD_BANDS_MIN`,
/// and enough that `slant / bands <= SPREAD_BAND_STEP_PX`.
fn spread_band_count(w: f32) -> u32 {
    let slant = motion::spread_slant(w);
    SPREAD_BANDS_MIN.max((slant / SPREAD_BAND_STEP_PX).ceil() as u32)
}

/// Clip rects approximating `(0,0) -> (edge,0) -> (edge-slant,h) -> (0,h)`
/// at eased progress `p`. `dir < 0` mirrors the sweep. Union of the
/// returned rects approximates the reveal quad; empty when `p == 0`.
pub fn spread_clip(rect: Rect, p: f32, dir: f32) -> Vec<Rect> {
    let w = rect.width();
    let slant = motion::spread_slant(w);
    let edge = motion::spread_edge(w, slant, p);
    let h = rect.height().max(1.);
    let bands_n = spread_band_count(w);
    let mirror = dir < 0.;
    let mut bands = Vec::with_capacity(bands_n as usize);
    for band in 0..bands_n {
        let (top, bottom) = (
            rect.top() + h * band as f32 / bands_n as f32,
            rect.top() + h * (band + 1) as f32 / bands_n as f32,
        );
        // Conservative edge: the true boundary is `edge - slant * y/h`;
        // the smallest value inside a band sits at its lower end.
        let x_edge = edge - slant * (band + 1) as f32 / bands_n as f32;
        if x_edge <= 0. {
            continue;
        }
        let band_rect = if mirror {
            Rect::from_min_max(
                pos2(rect.right() - x_edge.min(w).max(0.), top),
                pos2(rect.right(), bottom),
            )
        } else {
            Rect::from_min_max(pos2(rect.left(), top), pos2(rect.left() + x_edge.min(w), bottom))
        };
        bands.push(band_rect.intersect(rect));
    }
    bands.retain(|b| b.width() > 0.01 && b.height() > 0.01);
    bands
}

/// Draws `draw` once per staircase clip band, shifted by the incoming-plane
/// offset. `p` is the eased spread progress (`motion::spread_progress`).
/// The caller's `painter` keeps its own clip/opacity; each band intersects
/// on top of it, so the sweep never draws outside the widget's clip.
pub fn paint_spread(
    painter: &Painter,
    rect: Rect,
    p: f32,
    dir: f32,
    draw: impl Fn(&Painter, Vec2),
) {
    let (dx, dy) = motion::spread_offset(p);
    let offset = vec2(dx, dy);
    for band in spread_clip(rect, p, dir) {
        let clipped = painter.with_clip_rect(band);
        draw(&clipped, offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cover_mip_tiers_halve_until_sixty_four() {
        let image = ColorImage::filled([520, 700], Color32::WHITE);
        let tiers = cover_mip_tiers(&image);
        assert_eq!(tiers.len(), 4, "520x700 -> 260x350 -> 130x175 -> 65x87");
        assert_eq!(tiers[3].size, [65, 87]);
        // A small cover never produces a degenerate tier.
        let small = cover_mip_tiers(&ColorImage::filled([130, 180], Color32::WHITE));
        assert_eq!(small.len(), 2);
    }
    #[test]
    fn halve_averages_premultiplied_and_stops_bleed() {
        // Top-left 2x2 block mixes opaque and transparent texels: it must
        // average to half-strength red, not a dark smear.
        let mut image = ColorImage::filled([4, 4], Color32::RED);
        for y in 0..2 {
            image.pixels[y * 4] = Color32::TRANSPARENT;
        }
        let down = halve(&image);
        assert_eq!(down.size, [2, 2]);
        let mixed = down.pixels[0];
        assert!((mixed.a() as i32 - 128).abs() <= 1, "alpha {}", mixed.a());
        assert!((mixed.r() as i32 - 255).abs() <= 2, "premult: red stays {}", mixed.r());
        assert!(mixed.g() <= 2);
        let opaque = down.pixels[1];
        assert_eq!(opaque, Color32::RED);
    }
    #[test]
    fn mip_pick_follows_the_projected_footprint() {
        let ctx = Context::default();
        let image = ColorImage::filled([512, 700], Color32::WHITE);
        let mips = CoverMips::upload(&ctx, "cover-t", &image);
        assert_eq!(mips.levels.len(), 4);
        let face = vec2(200., 300.);
        // Huge footprint: base art stays.
        assert!(std::ptr::eq(
            mips.pick(Plane::rect(Rect::from_min_size(Pos2::ZERO, vec2(400., 600.))), face, 1.),
            &mips.levels[0]
        ));
        // ~70x90pt oblique face on a 512x700 cover drops at least two tiers.
        let plane = Plane::rect(Rect::from_min_size(Pos2::ZERO, vec2(70., 90.)));
        let picked = mips.pick(plane, face, 1.);
        assert!(picked.size()[0] <= 256, "picked {:?}", picked.size());
        assert!(picked.size()[0] >= 65);
        assert!(std::ptr::eq(
            cover_pick(Some(&mips), None, plane, face, 1.).unwrap(),
            picked
        ));
    }
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

    #[test]
    fn spread_clip_bands_cover_exactly_the_reveal_quad() {
        let rect = Rect::from_min_size(pos2(50., 20.), vec2(400., 600.));
        assert!(spread_clip(rect, 0., 1.).is_empty(), "p=0 must hide all");
        let full = spread_clip(rect, 1., 1.);
        let area: f32 = full.iter().map(|b| b.width() * b.height()).sum();
        assert!(
            (area - rect.width() * rect.height()).abs() < 1.,
            "p=1 must cover the whole plane, got {area}"
        );
        // Monotonic sweep: union area grows with p and never exceeds the
        // true trapezoid area by more than one band step of slant.
        let mut prev = -1.;
        for i in 0..=20 {
            let p = i as f32 / 20.;
            let bands = spread_clip(rect, p, 1.);
            let covered: f32 = bands.iter().map(|b| b.width() * b.height()).sum();
            assert!(covered >= prev - 0.01, "area shrunk at p={p}");
            prev = covered;
            // Bands tile the rect vertically; touching edges report
            // intersects()=true in epaint, so assert near-zero overlap
            // area instead.
            for pair in bands.windows(2) {
                let overlap = pair[0].intersect(pair[1]);
                assert!(
                    overlap.width() * overlap.height() < 0.01,
                    "bands overlap at p={p}: {:?} vs {:?}",
                    pair[0],
                    pair[1]
                );
            }
            for band in &bands {
                assert!(rect.contains_rect(*band));
            }
        }
    }

    #[test]
    fn spread_clip_step_stays_conservative_and_within_budget() {
        // Adaptive count: enough bands that the stair step never exceeds
        // 8px of horizontal travel, and never fewer than the old fixed 8.
        let slant = motion::spread_slant(1024.);
        let n = spread_band_count(1024.);
        assert!(n >= SPREAD_BANDS_MIN);
        if slant > 0. {
            assert!((slant / n as f32) <= SPREAD_BAND_STEP_PX, "step too coarse");
        }
        // Conservative coverage: every band's drawn right edge must sit
        // inside the true slanted boundary `edge - slant * y/h` — the
        // union never reveals more than the reference quad (no overdraw),
        // and adjacent bands abut so no pixel rows are missed.
        let rect = Rect::from_min_size(pos2(0., 0.), vec2(1024., 900.));
        for i in 1..=10 {
            let p = i as f32 / 10.;
            let edge = motion::spread_edge(rect.width(), slant, p);
            let bands = spread_clip(rect, p, 1.);
            // Bands whose whole step still sits left of the leading edge
            // are dropped (`x_edge <= 0`); count the expected survivors.
            let expected = (0..n)
                .filter(|b| edge - slant * (*b + 1) as f32 / n as f32 > 0.01)
                .count();
            assert_eq!(bands.len(), expected, "band count at p={p}");
            for band in &bands {
                // True boundary at the band's bottom (smallest x inside).
                let true_x = edge - slant * (band.bottom() / rect.height());
                let drawn_x = band.right() - rect.left();
                assert!(
                    drawn_x <= true_x + 0.02,
                    "band overdraws slant at p={p}: drawn={drawn_x} true={true_x}"
                );
            }
        }
    }

    #[test]
    fn spread_clip_dir_minus_one_mirrors_around_center() {
        let rect = Rect::from_min_size(pos2(50., 20.), vec2(400., 600.));
        for i in 1..=10 {
            let p = i as f32 / 10.;
            let left = spread_clip(rect, p, 1.);
            let right = spread_clip(rect, p, -1.);
            // dir=-1 produces the mirror image of dir=+1 around x=w/2:
            // each mirrored band b satisfies b.right = rect.right - a.left
            // and b.left = rect.right - a.right for the matching band a.
            let l_area: f32 = left.iter().map(|b| b.width() * b.height()).sum();
            let r_area: f32 = right.iter().map(|b| b.width() * b.height()).sum();
            // Areas are equal in exact math; f32 cancellation in the
            // mirrored edge formula grows with band count, so allow a bit
            // more than one band-step of absolute error.
            assert!((l_area - r_area).abs() < 0.05, "areas differ at p={p}: {l_area} vs {r_area}");
            let left_from_right: Vec<Rect> = right
                .iter()
                .map(|b| {
                    Rect::from_min_max(
                        pos2(rect.right() - b.right() + rect.left(), b.top()),
                        pos2(rect.right() - b.left() + rect.left(), b.bottom()),
                    )
                })
                .collect();
            assert_eq!(left.len(), left_from_right.len());
            for (a, m) in left.iter().zip(left_from_right.iter()) {
                assert!((a.left() - m.left()).abs() < 0.01, "left edge at p={p}");
                assert!((a.right() - m.right()).abs() < 0.01, "right edge at p={p}");
            }
        }
    }
}
