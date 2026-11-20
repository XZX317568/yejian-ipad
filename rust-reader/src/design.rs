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
}

pub(crate) fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            paper: Color32::from_rgb(20, 18, 16),
            surface: Color32::from_rgb(28, 25, 22),
            soft: Color32::from_rgb(37, 33, 29),
            ink: Color32::from_rgb(237, 231, 222),
            muted: Color32::from_rgb(161, 152, 142),
            line: Color32::from_rgb(52, 48, 42),
            accent: Color32::from_rgb(224, 123, 96),
            accent_soft: Color32::from_rgb(58, 38, 32),
            on_accent: Color32::from_rgb(28, 25, 22),
            primary: Color32::from_rgb(237, 231, 222),
            on_primary: Color32::from_rgb(28, 25, 22),
            danger: Color32::from_rgb(242, 184, 181),
        }
    } else {
        Palette {
            paper: Color32::from_rgb(247, 244, 238),
            surface: Color32::from_rgb(255, 253, 249),
            soft: Color32::from_rgb(239, 235, 227),
            ink: Color32::from_rgb(31, 27, 23),
            muted: Color32::from_rgb(120, 113, 106),
            line: Color32::from_rgb(229, 223, 213),
            accent: Color32::from_rgb(184, 69, 47),
            accent_soft: Color32::from_rgb(246, 231, 224),
            on_accent: Color32::from_rgb(255, 253, 249),
            primary: Color32::from_rgb(31, 27, 23),
            on_primary: Color32::from_rgb(255, 253, 249),
            danger: Color32::from_rgb(179, 38, 30),
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
    v.window_stroke = Stroke::new(if dark { 1. } else { 0. }, p.line);
    v.window_corner_radius = CornerRadius::same(20);
    v.window_shadow = card_shadow(dark);
    v.menu_corner_radius = CornerRadius::same(12);
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1., p.accent);
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
    v.widgets.active.bg_fill = p.soft;
    v.widgets.active.weak_bg_fill = p.soft;
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

pub(crate) fn toggle(ui: &mut Ui, value: &mut bool, title: &str, description: &str) -> bool {
    let p = palette(ui.visuals().dark_mode);
    let text_width = (ui.available_width() - 68.).max(80.);
    let title = ui.painter().layout(
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
    let height = (title.size().y + description.size().y + 10.).max(56.);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, *value, title.text()));
    if response.clicked() {
        *value = !*value;
    }
    ui.painter().galley(rect.min, title.clone(), p.ink);
    ui.painter().galley(
        rect.min + vec2(0., title.size().y + 4.),
        description,
        p.muted,
    );
    let track = Rect::from_center_size(pos2(rect.right() - 24., rect.center().y), vec2(46., 28.));
    ui.painter()
        .rect_filled(track, 14., if *value { p.accent } else { p.line });
    let x = if *value {
        track.right() - 14.
    } else {
        track.left() + 14.
    };
    ui.painter()
        .circle_filled(pos2(x, track.center().y), 10., p.surface);
    response.clicked()
}
