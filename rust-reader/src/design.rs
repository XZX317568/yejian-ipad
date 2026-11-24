//! Shared surfaces and controls for the reading workspace.
use egui::*;

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub paper: Color32,
    pub surface: Color32,
    pub soft: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub line: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
}

/// Light accent triplets for the A/B/C judgement variants.
/// YEJIAN_ACCENT_VARIANT = A (amber, default) | B (spine brown) | C (ink).
/// Parsed once and cached: palette() is called for every surface each frame,
/// so a per-call env read + String allocation was pure waste (L-S1-10).
fn accent_variant() -> (u8, u8, u8, u8, u8, u8, u8, u8, u8) {
    static VARIANT: std::sync::OnceLock<(u8, u8, u8, u8, u8, u8, u8, u8, u8)> =
        std::sync::OnceLock::new();
    *VARIANT.get_or_init(|| {
        match std::env::var("YEJIAN_ACCENT_VARIANT")
            .unwrap_or_else(|_| "A".into())
            .trim()
            .to_ascii_uppercase()
            .as_str()
        {
            "B" => (168, 93, 40, 244, 231, 218, 255, 253, 249),
            "C" => (44, 38, 33, 235, 230, 222, 255, 253, 249),
            // Variant A deepened to #B0621A: warm amber-brown keeps the
            // Apple Books glow and lifts on_accent white text past WCAG AA
            // (>=4.6:1, previously ~3.2:1 at 200,125,47 — L-S1-07).
            _ => (176, 98, 26, 247, 234, 217, 255, 253, 249),
        }
    })
}

pub(crate) fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            paper: Color32::from_rgb(20, 19, 18),
            surface: Color32::from_rgb(31, 29, 27),
            soft: Color32::from_rgb(42, 39, 36),
            ink: Color32::from_rgb(245, 242, 235),
            muted: Color32::from_rgb(163, 158, 147),
            line: Color32::from_rgb(51, 47, 43),
            accent: Color32::from_rgb(224, 155, 79),
            accent_soft: Color32::from_rgb(59, 46, 30),
            on_accent: Color32::from_rgb(24, 19, 13),
        }
    } else {
        let (ar, ag, ab, sr, sg, sb, on_r, on_g, on_b) = accent_variant();
        Palette {
            paper: Color32::from_rgb(250, 247, 242),
            surface: Color32::from_rgb(255, 255, 255),
            soft: Color32::from_rgb(242, 237, 228),
            ink: Color32::from_rgb(28, 25, 23),
            muted: Color32::from_rgb(120, 113, 108),
            line: Color32::from_rgb(231, 226, 216),
            accent: Color32::from_rgb(ar, ag, ab),
            accent_soft: Color32::from_rgb(sr, sg, sb),
            on_accent: Color32::from_rgb(on_r, on_g, on_b),
        }
    }
}

pub(crate) fn visuals(dark: bool) -> Visuals {
    let p = palette(dark);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.override_text_color = Some(p.ink);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.paper;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.soft;
    v.window_stroke = Stroke::new(1., p.line);
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(12);
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1., p.accent);
    v.hyperlink_color = p.accent;
    v.widgets.noninteractive.bg_stroke = Stroke::new(0.75, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1., p.ink);
    for widget in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.bg_fill = p.surface;
        widget.weak_bg_fill = Color32::TRANSPARENT;
        widget.bg_stroke = Stroke::new(0.75, p.line);
        widget.fg_stroke = Stroke::new(1.5, p.ink);
        widget.corner_radius = CornerRadius::same(11);
        widget.expansion = 0.;
    }
    v.widgets.hovered.bg_fill = p.accent_soft;
    v.widgets.hovered.weak_bg_fill = p.accent_soft;
    // Gentle press distinction: a shade deeper than the hover wash.
    let pressed = if dark {
        p.accent_soft.gamma_multiply(1.2)
    } else {
        p.accent_soft.gamma_multiply(0.88)
    };
    v.widgets.active.bg_fill = pressed;
    v.widgets.active.weak_bg_fill = pressed;
    v.widgets.active.bg_stroke = Stroke::new(1., p.accent.linear_multiply(0.6));
    v.widgets.active.fg_stroke.color = p.accent;
    v
}

pub(crate) fn surface(ui: &Ui) -> Frame {
    let p = palette(ui.visuals().dark_mode);
    Frame::new()
        .fill(p.surface)
        .corner_radius(14)
        .inner_margin(20)
        .stroke(Stroke::new(1., p.line.gamma_multiply(0.55)))
}

/// A theme choice shows what the page will look like before it is selected.
pub(crate) fn theme_choice(
    ui: &mut Ui,
    name: &str,
    kind: &str,
    selected: bool,
    width: f32,
) -> bool {
    let p = palette(ui.visuals().dark_mode);
    let font = TextStyle::Button.resolve(ui.style());
    let text = ui.painter().layout(name.into(), font, p.ink, width - 16.);
    let height = 78. + text.size().y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, selected, name));
    ui.painter()
        .rect_filled(rect, 12., if selected { p.accent_soft } else { p.surface });
    ui.painter().rect_stroke(
        rect,
        12.,
        Stroke::new(
            if selected { 1.5 } else { 1. },
            if selected { p.accent } else { p.line },
        ),
        StrokeKind::Inside,
    );
    let preview = Rect::from_min_max(
        rect.min + vec2(12., 11.),
        pos2(rect.right() - 12., rect.top() + 59.),
    );
    let preview_dark = kind == "dark";
    // Warm-charcoal night / warm paper — the old green-tinged swatches are gone.
    let bg = if preview_dark {
        Color32::from_rgb(32, 29, 26)
    } else {
        Color32::from_rgb(244, 240, 231)
    };
    ui.painter().rect_filled(preview, 5., bg);
    if kind == "system" {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(preview.center().x, preview.top()), preview.max),
            5.,
            Color32::from_rgb(32, 29, 26),
        );
    }
    let book = Rect::from_min_size(preview.min + vec2(8., 7.), vec2(22., 34.));
    ui.painter()
        .rect_filled(book, 2., Color32::from_rgb(196, 142, 78));
    for (offset, fraction) in [(12., 0.60), (21., 0.42), (31., 0.50)] {
        ui.painter().line_segment(
            [
                pos2(book.right() + 8., preview.top() + offset),
                pos2(
                    book.right() + 8. + (preview.width() - 48.).max(0.) * fraction,
                    preview.top() + offset,
                ),
            ],
            Stroke::new(
                2.,
                if preview_dark {
                    Color32::from_gray(140)
                } else {
                    Color32::from_gray(158)
                },
            ),
        );
    }
    ui.painter().galley(
        pos2(rect.center().x - text.size().x / 2., rect.top() + 68.),
        text,
        p.ink,
    );
    response.clicked()
}

pub(crate) fn section(ui: &mut Ui, title: &str) {
    let p = palette(ui.visuals().dark_mode);
    ui.add(
        Label::new(
            RichText::new(title)
                .size(ui.text_style_height(&TextStyle::Body))
                .strong()
                .color(p.ink),
        )
        .wrap(),
    );
    ui.add_space(8.);
}

pub(crate) fn primary(label: &str, dark: bool) -> Button<'_> {
    let p = palette(dark);
    Button::new(RichText::new(label).color(p.on_accent))
        .fill(p.accent)
        .corner_radius(12)
        .min_size(vec2(44., 48.))
        .wrap()
}

/// Toggle with a 140ms knob and track transition (instant when `reduced`).
pub(crate) fn toggle_animated(
    ui: &mut Ui,
    value: &mut bool,
    title: &str,
    description: &str,
    reduced: bool,
) -> bool {
    let p = palette(ui.visuals().dark_mode);
    let text_width = (ui.available_width() - 68.).max(80.);
    let title_galley = ui.painter().layout(
        title.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        p.ink,
        text_width,
    );
    let description = ui.painter().layout(
        description.to_owned(),
        TextStyle::Small.resolve(ui.style()),
        p.muted,
        text_width,
    );
    let height = (title_galley.size().y + description.size().y + 10.).max(56.);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(Role::CheckBox, true, *value, title_galley.text())
    });
    if response.clicked() {
        *value = !*value;
    }
    ui.painter().galley(rect.min, title_galley.clone(), p.ink);
    ui.painter().galley(
        rect.min + vec2(0., title_galley.size().y + 4.),
        description,
        p.muted,
    );
    let anim = ui.ctx().animate_bool_with_time(
        ui.make_persistent_id(("yejian-toggle", title)),
        *value,
        if reduced { 0. } else { 0.14 },
    );
    let track = Rect::from_center_size(pos2(rect.right() - 24., rect.center().y), vec2(46., 28.));
    ui.painter()
        .rect_filled(track, 14., mix(p.line, p.accent, anim));
    let x = track.left() + 14. + (track.width() - 28.) * anim;
    ui.painter()
        .circle_filled(pos2(x, track.center().y), 10., mix(p.surface, p.on_accent, anim));
    response.clicked()
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0., 1.);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Warm paper ambient + directional drop shadow: three translucent rounded
/// rects scaled by `elevation` (1.0 cards/nav, 2.0 modals/cover). No shaders.
pub(crate) fn soft_shadow(p: &Painter, rect: Rect, radius: f32, elevation: f32, dark: bool) {
    let elevation = elevation.max(0.);
    if elevation <= 0. {
        return;
    }
    // (spread, drop, alpha) — all per unit of elevation.
    for (spread, drop, alpha) in [
        (3.2, 0.6, if dark { 0.10 } else { 0.07 }),
        (1.8, 1.4, if dark { 0.18 } else { 0.10 }),
        (0.9, 2.4, if dark { 0.30 } else { 0.16 }),
    ] {
        let grow = spread * elevation;
        let layer = rect
            .expand2(vec2(grow, grow))
            .translate(vec2(0., drop * elevation));
        let color = if dark {
            Color32::from_black_alpha((alpha * 255.) as u8)
        } else {
            Color32::from_rgba_unmultiplied(92, 76, 58, (alpha * 255.) as u8)
        };
        p.rect_filled(layer, radius + grow, color);
    }
}

/// Unified light-bordered secondary action button.
pub(crate) fn secondary<'a>(label: &'a str, dark: bool) -> Button<'a> {
    let p = palette(dark);
    Button::new(RichText::new(label).color(p.ink))
        .fill(p.soft)
        .stroke(Stroke::new(0.75, p.line))
        .corner_radius(11)
        .min_size(vec2(44., 44.))
        .wrap()
}

/// Gentle 0.97 shrink around center for touch press feedback
/// (at most 2pt inset per side).
pub(crate) fn press_rect(rect: Rect, pressed: bool) -> Rect {
    if !pressed {
        return rect;
    }
    let inset = vec2(
        (rect.width() * 0.015).min(2.),
        (rect.height() * 0.015).min(2.),
    );
    Rect::from_min_max(rect.min + inset, rect.max - inset)
}

/// 20pt alpha-to-solid gradient strips fading scrolled content into `bg`.
pub(crate) fn paint_scroll_fade(
    p: &Painter,
    rect: Rect,
    bg: Color32,
    fade_top: bool,
    fade_bottom: bool,
) {
    const FADE: f32 = 20.;
    let strip = |top: f32, bottom: f32, top_color: Color32, bottom_color: Color32| {
        let strip = Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), bottom));
        if strip.is_positive() {
            let mut mesh = Mesh::default();
            mesh.vertices.extend([
                epaint::Vertex::untextured(strip.left_top(), top_color),
                epaint::Vertex::untextured(strip.right_top(), top_color),
                epaint::Vertex::untextured(strip.right_bottom(), bottom_color),
                epaint::Vertex::untextured(strip.left_bottom(), bottom_color),
            ]);
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            p.add(Shape::mesh(mesh));
        }
    };
    if fade_top {
        strip(
            rect.top(),
            (rect.top() + FADE).min(rect.bottom()),
            bg.linear_multiply(0.),
            bg,
        );
    }
    if fade_bottom {
        strip(
            (rect.bottom() - FADE).max(rect.top()),
            rect.bottom(),
            bg,
            bg.linear_multiply(0.),
        );
    }
}

pub(crate) struct SliderOutcome {
    pub changed: bool,
    pub dragging: bool,
    pub rect: Rect,
    pub thumb_x: f32,
}

/// Touch-sized custom slider: 44pt hit area, 4pt rounded track, accent fill
/// (from the left in LTR, the right in RTL), press/drag anywhere to set.
pub(crate) fn slider(
    ui: &mut Ui,
    id: impl AsIdSalt,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
    rtl: bool,
    dark: bool,
) -> SliderOutcome {
    let p = palette(dark);
    let (rect, _allocated) = ui.allocate_exact_size(vec2(width.max(80.), 44.), Sense::hover());
    let response = ui.interact(rect, ui.make_persistent_id(id), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::slider(true, *value as f64, ""));

    let (min, max) = (*range.start(), *range.end());
    let span = (max - min).max(f32::EPSILON);
    let track = Rect::from_center_size(rect.center(), vec2((rect.width() - 28.).max(4.), 4.));

    let mut changed = false;
    if (response.dragged() || response.is_pointer_button_down_on())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let raw = if rtl {
            track.right() - pointer.x
        } else {
            pointer.x - track.left()
        };
        let next = (min + (raw / track.width()).clamp(0., 1.) * (max - min))
            .clamp(min.min(max), min.max(max));
        if next != *value {
            *value = next;
            changed = true;
        }
    }

    let t = ((*value - min) / span).clamp(0., 1.);
    let t_visual = if rtl { 1. - t } else { t };
    let thumb_x = track.left() + track.width() * t_visual;
    let cy = rect.center().y;

    let track_fill = if dark {
        Color32::from_gray(72)
    } else {
        p.line
    };
    ui.painter().rect_filled(track, 2., track_fill);
    let active = if rtl {
        Rect::from_min_max(pos2(thumb_x, track.top()), track.max)
    } else {
        Rect::from_min_max(track.min, pos2(thumb_x, track.bottom()))
    };
    if active.width() > 0.5 {
        ui.painter().rect_filled(active, 2., p.accent);
    }
    let engaged =
        response.hovered() || response.dragged() || response.is_pointer_button_down_on();
    let r = if engaged { 11. } else { 9. };
    let thumb = pos2(thumb_x, cy);
    ui.painter().circle_filled(
        thumb + vec2(0., 1.2),
        r + 0.8,
        Color32::from_black_alpha(if dark { 80 } else { 28 }),
    );
    ui.painter().circle_filled(thumb, r, p.surface);
    ui.painter()
        .circle_stroke(thumb, r - 0.7, Stroke::new(2., p.accent));
    if engaged {
        ui.painter().circle_filled(thumb, 3., p.accent);
    }
    SliderOutcome {
        changed,
        dragging: response.dragged(),
        rect,
        thumb_x,
    }
}

/// Small geometric glyphs drawn centered on `center` at ~17pt.
pub(crate) fn draw_icon(p: &Painter, center: Pos2, kind: &str, color: Color32, active: bool) {
    let c = center;
    let stroke = Stroke::new(if active { 1.8 } else { 1.5 }, color);
    let tint = color.linear_multiply(0.16);
    match kind {
        "library" => {
            let book = Rect::from_center_size(c, vec2(13., 16.));
            if active {
                p.rect_filled(book, 2., tint);
            }
            p.rect_stroke(book, 2., stroke, StrokeKind::Inside);
            let spine_x = book.left() + 3.6;
            p.line_segment(
                [
                    pos2(spine_x, book.top() + 0.8),
                    pos2(spine_x, book.bottom() - 0.8),
                ],
                Stroke::new(1.2, color),
            );
            let r_top = book.top() + 0.6;
            let r_right = book.right() - 2.4;
            p.add(Shape::convex_polygon(
                vec![
                    pos2(r_right - 2.8, r_top),
                    pos2(r_right, r_top),
                    pos2(r_right, r_top + 5.4),
                    pos2(r_right - 1.4, r_top + 4.),
                    pos2(r_right - 2.8, r_top + 5.4),
                ],
                color,
                Stroke::NONE,
            ));
        }
        "search" => {
            let ring = c + vec2(-1.4, -1.4);
            let r = 5.2;
            p.circle_stroke(ring, r, stroke);
            let d = vec2(1., 1.).normalized();
            p.line_segment(
                [ring + d * (r + 0.4), ring + d * (r + 4.)],
                Stroke::new(if active { 2.2 } else { 1.8 }, color),
            );
        }
        "history" => {
            if active {
                p.circle_filled(c, 7., tint);
            }
            p.circle_stroke(c, 7., stroke);
            let pts: Vec<Pos2> = (0..=5)
                .map(|i| {
                    let a = (140. + i as f32 * 22.).to_radians();
                    c + vec2(a.cos(), a.sin()) * 8.8
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(1.1, color.linear_multiply(0.6))));
            p.line_segment([c, c + vec2(0., -3.8)], Stroke::new(1.6, color));
            p.line_segment([c, c + vec2(2.8, 1.2)], Stroke::new(1.6, color));
            p.circle_filled(c, 1.1, color);
        }
        "settings" => {
            // Three horizontal sliders — unmistakable at 16-20pt.
            for (i, dy) in [-4.6, 0., 4.6].into_iter().enumerate() {
                let y = c.y + dy;
                p.line_segment(
                    [pos2(c.x - 7., y), pos2(c.x + 7., y)],
                    Stroke::new(1.4, color.linear_multiply(0.85)),
                );
                let knob = pos2(c.x + [-2.6, 2.4, -0.6][i], y);
                if active {
                    p.circle_filled(knob, 2.4, color);
                } else {
                    p.circle_stroke(knob, 2.4, Stroke::new(1.5, color));
                }
            }
        }
        "plus" => {
            p.line_segment(
                [c + vec2(-6., 0.), c + vec2(6., 0.)],
                Stroke::new(1.8, color),
            );
            p.line_segment(
                [c + vec2(0., -6.), c + vec2(0., 6.)],
                Stroke::new(1.8, color),
            );
        }
        "back" => {
            p.line_segment(
                [c + vec2(5., 0.), c + vec2(-5.5, 0.)],
                Stroke::new(1.8, color),
            );
            p.add(Shape::line(
                vec![c + vec2(-1., -4.2), c + vec2(-5.5, 0.), c + vec2(-1., 4.2)],
                Stroke::new(1.8, color),
            ));
        }
        "prev" => {
            p.add(Shape::line(
                vec![c + vec2(3.2, -5.4), c + vec2(-3.2, 0.), c + vec2(3.2, 5.4)],
                stroke,
            ));
        }
        "next" => {
            p.add(Shape::line(
                vec![c + vec2(-3.2, -5.4), c + vec2(3.2, 0.), c + vec2(-3.2, 5.4)],
                stroke,
            ));
        }
        "open" => {
            // Open book: two pages leaning on a shared spine.
            let spine_top = c + vec2(0., -3.4);
            let spine_bottom = c + vec2(0., 4.6);
            for s in [-1., 1.] {
                p.add(Shape::line(
                    vec![
                        spine_top,
                        c + vec2(6.6 * s, -5.8),
                        c + vec2(6.6 * s, 2.4),
                        spine_bottom,
                    ],
                    stroke,
                ));
            }
        }
        "chapters" => {
            for i in 0..3 {
                let y = c.y - 4.6 + i as f32 * 4.6;
                p.circle_filled(pos2(c.x - 5.6, y), 1.1, color);
                p.line_segment(
                    [pos2(c.x - 3., y), pos2(c.x + 6., y)],
                    Stroke::new(1.5, color),
                );
            }
        }
        "sun" => {
            if active {
                p.circle_filled(c, 4.2, tint);
            }
            p.circle_stroke(c, 4.2, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([c + d * 6.2, c + d * 8.2], Stroke::new(1.5, color));
            }
        }
        "paged" => {
            let page = Rect::from_center_size(c, vec2(11., 15.));
            if active {
                p.rect_filled(page, 1.5, tint);
            }
            p.rect_stroke(page, 1.5, stroke, StrokeKind::Inside);
        }
        "spread" => {
            for s in [-1., 1.] {
                let page = Rect::from_center_size(c + vec2(3.6 * s, 0.), vec2(6.4, 15.));
                if active {
                    p.rect_filled(page, 1.2, tint);
                }
                p.rect_stroke(page, 1.2, stroke, StrokeKind::Inside);
            }
        }
        "vertical" => {
            let page = Rect::from_center_size(c, vec2(11., 15.));
            if active {
                p.rect_filled(page, 1.5, tint);
            }
            p.rect_stroke(page, 1.5, stroke, StrokeKind::Inside);
            p.add(Shape::line(
                vec![
                    pos2(c.x - 2.4, c.y - 0.6),
                    pos2(c.x, c.y - 3.8),
                    pos2(c.x + 2.4, c.y - 0.6),
                ],
                Stroke::new(1.5, color),
            ));
            p.add(Shape::line(
                vec![
                    pos2(c.x - 2.4, c.y + 0.6),
                    pos2(c.x, c.y + 3.8),
                    pos2(c.x + 2.4, c.y + 0.6),
                ],
                Stroke::new(1.5, color),
            ));
        }
        "download" => {
            p.line_segment(
                [c + vec2(0., -6.), c + vec2(0., 2.2)],
                Stroke::new(1.8, color),
            );
            p.add(Shape::line(
                vec![c + vec2(-3.6, -1.), c + vec2(0., 2.6), c + vec2(3.6, -1.)],
                Stroke::new(1.8, color),
            ));
            p.line_segment(
                [pos2(c.x - 5.6, c.y + 6.), pos2(c.x + 5.6, c.y + 6.)],
                Stroke::new(1.8, color),
            );
        }
        "check" => {
            p.add(Shape::line(
                vec![c + vec2(-5.6, 0.6), c + vec2(-1.6, 4.4), c + vec2(6., -4.2)],
                Stroke::new(2., color),
            ));
        }
        _ => {}
    }
}

/// 统一字阶角色：Apple Books 原生排版基准（点），乘 font_scale 缩放。
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum TypeRole {
    LargeTitle,
    Title,
    Headline,
    Body,
    Subhead,
    Caption,
}

/// 字阶尺寸。LargeTitle 在大字号上温和阻尼（>1.35 后缓升），
/// 避免 200% 动态字级把标题顶出安全区（硬上限 42pt）。
#[allow(dead_code)]
pub(crate) fn type_size(font_scale: f32, role: TypeRole) -> f32 {
    let base = match role {
        TypeRole::LargeTitle => 30.,
        TypeRole::Title => 24.,
        TypeRole::Headline => 20.,
        TypeRole::Body => 17.,
        TypeRole::Subhead => 14.,
        TypeRole::Caption => 12.,
    };
    if matches!(role, TypeRole::LargeTitle) && font_scale > 1.35 {
        return (30. * (1.35 + (font_scale - 1.35) * 0.4)).min(42.);
    }
    base * font_scale
}

/// 原生毛玻璃质感浮层：接触影 + 环境影 + 半透明底 + 顶部微高光。
/// 供导航栏、阅读器工具条等浮层统一复用（S2/S4 挂接点）。
#[allow(dead_code)]
pub(crate) fn translucent_bar(
    painter: &egui::Painter,
    rect: egui::Rect,
    p: Palette,
    dark: bool,
    radius: u8,
) {
    // 双层柔和阴影：近身接触影 + 较大环境影。
    soft_shadow(painter, rect, radius as f32, 0.7, dark);
    soft_shadow(
        painter,
        rect.shrink(2.),
        radius as f32,
        0.4,
        dark,
    );
    let fill = if dark {
        p.surface.linear_multiply(224. / 255.)
    } else {
        p.surface.linear_multiply(232. / 255.)
    };
    painter.rect_filled(rect, radius as f32, fill);
    // 1px 顶部/内缘微高光描边，模拟毛玻璃板的分层边缘。
    painter.rect_stroke(
        rect.shrink(0.5),
        radius as f32,
        Stroke::new(1., p.ink.linear_multiply(if dark { 0.06 } else { 0.10 })),
        StrokeKind::Inside,
    );
    painter.line_segment(
        [rect.left_top() + vec2(radius as f32, 0.5), rect.right_top() + vec2(-(radius as f32), 0.5)],
        Stroke::new(1., Color32::WHITE.linear_multiply(if dark { 0.05 } else { 0.45 })),
    );
}

/// iOS 设置页式分组卡片：14pt 圆角、surface 底、1px line 细描边、16pt 内边距。
#[allow(dead_code)]
pub(crate) fn grouped_card<R>(
    ui: &mut egui::Ui,
    p: Palette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    Frame::new()
        .fill(p.surface)
        .corner_radius(14)
        .inner_margin(16)
        .stroke(Stroke::new(1., p.line))
        .show(ui, add_contents)
}
