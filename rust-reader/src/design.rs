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
    pub primary: Color32,
    pub on_primary: Color32,
    pub danger: Color32,
    pub page: Color32,
    pub spine: Color32,
    pub edge: Color32,
    pub floor: Color32,
}

/// The selectable colour schemes; 纸墨 stays the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scheme {
    Ink,
    Mist,
    Sepia,
    Mono,
    Sakura,
    Dusk,
}

impl Scheme {
    pub(crate) const ALL: [Scheme; 6] = [
        Scheme::Ink,
        Scheme::Mist,
        Scheme::Sepia,
        Scheme::Mono,
        Scheme::Sakura,
        Scheme::Dusk,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Scheme::Ink => "ink",
            Scheme::Mist => "mist",
            Scheme::Sepia => "sepia",
            Scheme::Mono => "mono",
            Scheme::Sakura => "sakura",
            Scheme::Dusk => "dusk",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Scheme::Ink => "纸墨",
            Scheme::Mist => "雾蓝",
            Scheme::Sepia => "茶韵",
            Scheme::Mono => "素白",
            Scheme::Sakura => "樱粉",
            Scheme::Dusk => "黛紫",
        }
    }

    pub(crate) fn from_key(key: &str) -> Scheme {
        Scheme::ALL
            .into_iter()
            .find(|scheme| scheme.key() == key)
            .unwrap_or(Scheme::Ink)
    }
}

thread_local! {
    static CURRENT: std::cell::Cell<Scheme> = const { std::cell::Cell::new(Scheme::Ink) };
}

pub(crate) fn set_scheme(scheme: Scheme) {
    CURRENT.with(|current| current.set(scheme));
}

pub(crate) fn scheme() -> Scheme {
    CURRENT.with(|current| current.get())
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The sixteen roles, in order: paper, surface, soft, ink, muted, line,
/// accent, accent_soft, on_accent, primary, on_primary, danger, page, spine,
/// edge, floor.
fn roles(values: [[u8; 3]; 16]) -> Palette {
    let [paper, surface, soft, ink, muted, line, accent, accent_soft, on_accent, primary, on_primary, danger, page, spine, edge, floor] =
        values;
    Palette {
        paper: rgb(paper[0], paper[1], paper[2]),
        surface: rgb(surface[0], surface[1], surface[2]),
        soft: rgb(soft[0], soft[1], soft[2]),
        ink: rgb(ink[0], ink[1], ink[2]),
        muted: rgb(muted[0], muted[1], muted[2]),
        line: rgb(line[0], line[1], line[2]),
        accent: rgb(accent[0], accent[1], accent[2]),
        accent_soft: rgb(accent_soft[0], accent_soft[1], accent_soft[2]),
        on_accent: rgb(on_accent[0], on_accent[1], on_accent[2]),
        primary: rgb(primary[0], primary[1], primary[2]),
        on_primary: rgb(on_primary[0], on_primary[1], on_primary[2]),
        danger: rgb(danger[0], danger[1], danger[2]),
        page: rgb(page[0], page[1], page[2]),
        spine: rgb(spine[0], spine[1], spine[2]),
        edge: rgb(edge[0], edge[1], edge[2]),
        floor: rgb(floor[0], floor[1], floor[2]),
    }
}

pub(crate) fn palette_of(scheme: Scheme, dark: bool) -> Palette {
    let values = match (scheme, dark) {
        (Scheme::Ink, false) => roles([
            [247, 244, 238],
            [255, 253, 249],
            [239, 235, 227],
            [31, 27, 23],
            [115, 108, 101],
            [229, 223, 213],
            [184, 69, 47],
            [246, 231, 224],
            [255, 253, 249],
            [31, 27, 23],
            [255, 253, 249],
            [179, 38, 30],
            [255, 253, 249],
            [244, 240, 231],
            [227, 223, 211],
            [138, 128, 107],
        ]),
        (Scheme::Ink, true) => roles([
            [20, 18, 16],
            [28, 25, 22],
            [37, 33, 29],
            [237, 231, 222],
            [161, 152, 142],
            [52, 48, 42],
            [224, 123, 96],
            [58, 38, 32],
            [28, 25, 22],
            [237, 231, 222],
            [28, 25, 22],
            [242, 184, 181],
            [40, 37, 33],
            [48, 43, 38],
            [62, 56, 50],
            [254, 253, 248],
        ]),
        (Scheme::Mist, false) => roles([
            [244, 246, 249],
            [255, 255, 255],
            [233, 237, 242],
            [26, 31, 43],
            [100, 110, 129],
            [221, 226, 234],
            [61, 90, 152],
            [227, 233, 245],
            [255, 255, 255],
            [26, 31, 43],
            [255, 255, 255],
            [179, 38, 30],
            [255, 255, 255],
            [238, 241, 245],
            [217, 222, 230],
            [122, 132, 150],
        ]),
        (Scheme::Mist, true) => roles([
            [17, 20, 25],
            [24, 28, 35],
            [33, 38, 47],
            [230, 234, 240],
            [149, 157, 171],
            [44, 50, 60],
            [142, 168, 224],
            [38, 48, 74],
            [24, 28, 35],
            [230, 234, 240],
            [24, 28, 35],
            [242, 184, 181],
            [35, 40, 50],
            [42, 48, 59],
            [57, 64, 77],
            [230, 234, 240],
        ]),
        (Scheme::Sepia, false) => roles([
            [242, 232, 213],
            [250, 243, 230],
            [232, 220, 196],
            [59, 47, 34],
            [117, 100, 77],
            [221, 207, 180],
            [154, 91, 42],
            [238, 220, 196],
            [250, 243, 230],
            [59, 47, 34],
            [250, 243, 230],
            [168, 50, 31],
            [251, 245, 234],
            [239, 228, 207],
            [220, 203, 172],
            [142, 122, 94],
        ]),
        (Scheme::Sepia, true) => roles([
            [28, 23, 18],
            [36, 30, 23],
            [47, 39, 30],
            [233, 220, 198],
            [168, 151, 127],
            [58, 49, 38],
            [217, 160, 102],
            [63, 47, 31],
            [36, 30, 23],
            [233, 220, 198],
            [36, 30, 23],
            [242, 184, 181],
            [46, 38, 29],
            [54, 45, 34],
            [74, 62, 48],
            [233, 220, 198],
        ]),
        (Scheme::Mono, false) => roles([
            [245, 245, 244],
            [255, 255, 255],
            [235, 235, 234],
            [22, 22, 22],
            [110, 110, 108],
            [224, 224, 222],
            [43, 43, 43],
            [230, 230, 228],
            [255, 255, 255],
            [22, 22, 22],
            [255, 255, 255],
            [179, 38, 30],
            [255, 255, 255],
            [240, 240, 239],
            [218, 218, 216],
            [124, 124, 122],
        ]),
        (Scheme::Mono, true) => roles([
            [0, 0, 0],
            [17, 17, 17],
            [28, 28, 28],
            [237, 237, 237],
            [154, 154, 154],
            [42, 42, 42],
            [237, 237, 237],
            [38, 38, 38],
            [17, 17, 17],
            [237, 237, 237],
            [17, 17, 17],
            [242, 184, 181],
            [30, 30, 30],
            [38, 38, 38],
            [54, 54, 54],
            [237, 237, 237],
        ]),
        (Scheme::Sakura, false) => roles([
            [250, 244, 244],
            [255, 252, 252],
            [242, 232, 233],
            [42, 31, 34],
            [125, 105, 114],
            [235, 220, 223],
            [192, 71, 110],
            [248, 227, 234],
            [255, 252, 252],
            [42, 31, 34],
            [255, 252, 252],
            [179, 38, 30],
            [255, 252, 252],
            [246, 236, 238],
            [230, 212, 216],
            [147, 124, 133],
        ]),
        (Scheme::Sakura, true) => roles([
            [23, 18, 20],
            [32, 26, 28],
            [43, 35, 38],
            [240, 228, 231],
            [174, 154, 161],
            [58, 48, 52],
            [238, 143, 174],
            [68, 38, 47],
            [32, 26, 28],
            [240, 228, 231],
            [32, 26, 28],
            [242, 184, 181],
            [42, 34, 37],
            [50, 41, 44],
            [69, 57, 61],
            [240, 228, 231],
        ]),
        (Scheme::Dusk, false) => roles([
            [245, 244, 248],
            [255, 255, 255],
            [235, 233, 241],
            [31, 28, 42],
            [110, 105, 129],
            [223, 220, 232],
            [102, 80, 164],
            [233, 228, 246],
            [255, 255, 255],
            [31, 28, 42],
            [255, 255, 255],
            [179, 38, 30],
            [255, 255, 255],
            [240, 238, 245],
            [218, 214, 228],
            [130, 124, 150],
        ]),
        (Scheme::Dusk, true) => roles([
            [19, 18, 25],
            [27, 26, 35],
            [37, 35, 47],
            [232, 229, 240],
            [160, 155, 179],
            [50, 47, 62],
            [185, 166, 240],
            [49, 42, 74],
            [27, 26, 35],
            [232, 229, 240],
            [27, 26, 35],
            [242, 184, 181],
            [38, 36, 47],
            [45, 43, 56],
            [60, 57, 73],
            [232, 229, 240],
        ]),
    };
    values
}

pub(crate) fn palette(dark: bool) -> Palette {
    palette_of(scheme(), dark)
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
    v.window_stroke = Stroke::new(if dark { 1. } else { 0. }, p.line);
    v.window_corner_radius = CornerRadius::same(20);
    v.window_shadow = card_shadow(dark);
    v.menu_corner_radius = CornerRadius::same(12);
    // egui shares `selection` between selected text and selected widgets; the
    // warm `soft` fill keeps selected chips/tabs ink-on-grey instead of pink.
    v.selection.bg_fill = p.soft;
    v.selection.stroke = Stroke::new(1., p.ink);
    v.hyperlink_color = p.accent;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1., p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1., p.ink);
    for widget in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.bg_fill = p.surface;
        widget.weak_bg_fill = Color32::TRANSPARENT;
        widget.bg_stroke = Stroke::new(1., p.line);
        widget.fg_stroke = Stroke::new(1.5, p.ink);
        widget.corner_radius = CornerRadius::same(10);
        widget.expansion = 0.;
    }
    v.widgets.hovered.bg_fill = p.soft;
    v.widgets.hovered.weak_bg_fill = p.soft;
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

/// Cards and modals float on a soft shadow in light mode and keep a 1px
/// outline in dark mode where a shadow would be invisible.
pub(crate) fn card_shadow(dark: bool) -> Shadow {
    if dark {
        Shadow::NONE
    } else {
        Shadow {
            offset: [0, 2],
            blur: 12,
            spread: 0,
            color: Color32::from_black_alpha(20),
        }
    }
}

pub(crate) fn card_stroke(dark: bool) -> Stroke {
    if dark {
        Stroke::new(1., palette(dark).line)
    } else {
        Stroke::NONE
    }
}

pub(crate) fn surface(ui: &Ui) -> Frame {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    Frame::new()
        .fill(p.surface)
        .corner_radius(16)
        .inner_margin(20)
        .stroke(card_stroke(dark))
        .shadow(card_shadow(dark))
}

/// Result of [`modal`]: whether the dimmed backdrop was clicked (dismiss)
/// and where the card landed so callers can record a hitbox.
pub(crate) struct Modal {
    pub(crate) scrim_clicked: bool,
    pub(crate) card_rect: Rect,
}

/// Opaque scrim + centered opaque card shared by every modal. The scrim
/// swallows the tap so nothing beneath is clickable; `fade_in(false)` plus
/// the surface fill keep the card opaque from the first frame.
pub(crate) fn modal(ctx: &Context, key: &str, body: impl FnOnce(&mut Ui)) -> Modal {
    let screen = ctx.content_rect();
    let scrim = Area::new(Id::unique(format!("{key}-scrim")))
        .order(Order::Foreground)
        .fade_in(false)
        .interactable(true)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0., Color32::from_black_alpha(96));
            ui.allocate_rect(screen, Sense::click())
        });
    let mut card_rect = Rect::NOTHING;
    Area::new(Id::unique(key))
        .order(Order::Foreground)
        .fade_in(false)
        .interactable(true)
        .movable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            let frame = surface(ui).show(ui, |ui| body(ui));
            card_rect = frame.response.rect;
        });
    Modal {
        scrim_clicked: scrim.inner.clicked(),
        card_rect,
    }
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
    ui.painter().rect_filled(rect, 12., p.surface);
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
    let pv = palette(preview_dark);
    ui.painter().rect_filled(preview, 5., pv.paper);
    if kind == "system" {
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(preview.center().x, preview.top()), preview.max),
            5.,
            palette(true).paper,
        );
    }
    let book = Rect::from_min_size(preview.min + vec2(8., 7.), vec2(22., 34.));
    ui.painter().rect_filled(book, 2., pv.accent);
    for (offset, fraction) in [(12., 0.60), (21., 0.42), (31., 0.50)] {
        ui.painter().line_segment(
            [
                pos2(book.right() + 8., preview.top() + offset),
                pos2(
                    book.right() + 8. + (preview.width() - 48.).max(0.) * fraction,
                    preview.top() + offset,
                ),
            ],
            Stroke::new(2., pv.muted),
        );
    }
    ui.painter().galley(
        pos2(rect.center().x - text.size().x / 2., rect.top() + 68.),
        text,
        p.ink,
    );
    response.clicked()
}

/// A colour-scheme card: three book spines and three swatches preview the
/// scheme's paper, ink and accent in the current light/dark mode.
pub(crate) fn scheme_choice(ui: &mut Ui, scheme: Scheme, selected: bool, width: f32) -> bool {
    let p = palette(ui.visuals().dark_mode);
    let pv = palette_of(scheme, ui.visuals().dark_mode);
    let font = TextStyle::Button.resolve(ui.style());
    let name = scheme.name();
    let text = ui.painter().layout(name.into(), font, p.ink, width - 16.);
    let height = 78. + text.size().y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, selected, name));
    ui.painter().rect_filled(rect, 12., p.surface);
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
    ui.painter().rect_filled(preview, 5., pv.paper);
    let spine_w = 10.;
    for (i, fill) in [pv.spine, pv.accent, pv.page].iter().enumerate() {
        let spine = Rect::from_min_size(
            preview.min + vec2(8. + i as f32 * (spine_w + 3.), 7.),
            vec2(spine_w, 34.),
        );
        ui.painter().rect_filled(spine, 2., *fill);
        ui.painter().rect_stroke(
            spine,
            2.,
            Stroke::new(1., pv.line),
            StrokeKind::Inside,
        );
    }
    for (i, fill) in [pv.paper, pv.ink, pv.accent].iter().enumerate() {
        let center = pos2(
            preview.left() + 8. + 3. * (spine_w + 3.) + 14. + i as f32 * 20.,
            preview.top() + 24.,
        );
        ui.painter().circle_filled(center, 7., *fill);
        ui.painter().circle_stroke(center, 7., Stroke::new(1., pv.line));
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
    ui.add_space(4.);
    ui.add(
        Label::new(
            RichText::new(title)
                .size(ui.text_style_height(&TextStyle::Body) * 17. / 15.)
                .strong()
                .color(p.ink),
        )
        .wrap(),
    );
    ui.add_space(8.);
}

pub(crate) fn primary(label: &str, dark: bool) -> Button<'_> {
    let p = palette(dark);
    Button::new(RichText::new(label).color(p.on_primary))
        .fill(p.primary)
        .corner_radius(12)
        .min_size(vec2(88., 48.))
        .wrap()
}

/// Destructive confirm action — danger fill, on_primary stays light on
/// the dark red in light mode and dark on the pale red in dark mode.
pub(crate) fn danger(label: &str, dark: bool) -> Button<'_> {
    let p = palette(dark);
    Button::new(RichText::new(label).color(p.on_primary))
        .fill(p.danger)
        .corner_radius(12)
        .min_size(vec2(88., 48.))
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
        .stroke(Stroke::new(1., p.line))
        .corner_radius(10)
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
            mesh.colored_vertex(strip.left_top(), top_color);
            mesh.colored_vertex(strip.right_top(), top_color);
            mesh.colored_vertex(strip.right_bottom(), bottom_color);
            mesh.colored_vertex(strip.left_bottom(), bottom_color);
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
        "bookmark" => {
            p.line_segment([c + vec2(-5., -7.), c + vec2(-5., 7.)], stroke);
            p.line_segment([c + vec2(5., -7.), c + vec2(5., 7.)], stroke);
            p.line_segment([c + vec2(-5., -7.), c + vec2(5., -7.)], stroke);
            p.line_segment([c + vec2(-5., 7.), c + vec2(0., 3.)], stroke);
            p.line_segment([c + vec2(5., 7.), c + vec2(0., 3.)], stroke);
        }
        "bookmark-on" => {
            let pts = vec![
                c + vec2(-5., -7.),
                c + vec2(5., -7.),
                c + vec2(5., 7.),
                c + vec2(0., 3.),
                c + vec2(-5., 7.),
            ];
            p.add(Shape::convex_polygon(pts, color, stroke));
        }
        "share" => {
            // Open box with an arrow rising out of it.
            p.line_segment([c + vec2(-7., -1.), c + vec2(-7., 7.)], stroke);
            p.line_segment([c + vec2(-7., 7.), c + vec2(7., 7.)], stroke);
            p.line_segment([c + vec2(7., 7.), c + vec2(7., -1.)], stroke);
            p.line_segment([c + vec2(0., 4.), c + vec2(0., -7.)], stroke);
            p.line_segment([c + vec2(0., -7.), c + vec2(-4., -3.)], stroke);
            p.line_segment([c + vec2(0., -7.), c + vec2(4., -3.)], stroke);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color32) -> f32 {
        fn channel(c: u8) -> f32 {
            let c = c as f32 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (hi, lo) = {
            let (la, lb) = (luminance(a), luminance(b));
            (la.max(lb), la.min(lb))
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn every_scheme_keeps_text_and_controls_legible() {
        for scheme in Scheme::ALL {
            for dark in [false, true] {
                let p = palette_of(scheme, dark);
                let mode = if dark { "dark" } else { "light" };
                for (a, b, min, label) in [
                    (p.ink, p.paper, 7., "ink on paper"),
                    (p.ink, p.surface, 7., "ink on surface"),
                    (p.muted, p.surface, 4.5, "muted on surface"),
                    (p.muted, p.paper, 4.5, "muted on paper"),
                    (p.on_primary, p.primary, 4.5, "on_primary on primary"),
                    (p.on_primary, p.danger, 4.5, "on_primary on danger"),
                    (p.accent, p.surface, 3., "accent on surface"),
                    (p.accent, p.paper, 3., "accent on paper"),
                ] {
                    let ratio = contrast(a, b);
                    assert!(
                        ratio >= min,
                        "{} {mode}: {label} contrast {ratio:.2} below {min}",
                        scheme.key()
                    );
                }
            }
        }
    }

    #[test]
    fn scheme_keys_round_trip_and_unknown_falls_back_to_ink() {
        for scheme in Scheme::ALL {
            assert_eq!(Scheme::from_key(scheme.key()), scheme);
        }
        assert_eq!(Scheme::from_key("does-not-exist"), Scheme::Ink);
    }
}
