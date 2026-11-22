//! Reading typefaces are static, OFL-licensed instances with broad CJK coverage.
//! Their provenance and conversion are recorded in `licenses/ReadingFonts.md`.
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};

const BODY_FONT: &str = "reading-sans";
const TITLE_FONT: &str = "reading-serif";
const FALLBACK_FONT: &str = "yejian-cjk";
const TITLE_FAMILY: &str = "reading-title";

/// Install the fonts only. Existing text sizes and accessibility scaling stay
/// with the app's `text_styles` and `configure_fonts` functions.
pub(crate) fn configure(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (
            BODY_FONT,
            include_bytes!("../assets/YejianReadingSans-Regular.ttf").as_slice(),
        ),
        (
            TITLE_FONT,
            include_bytes!("../assets/YejianReadingSerif-Semibold.ttf").as_slice(),
        ),
        (
            FALLBACK_FONT,
            include_bytes!("../assets/DroidSansFallback.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
    }

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, BODY_FONT.into());
    proportional.push(FALLBACK_FONT.into());
    let mut titles = vec![TITLE_FONT.into()];
    titles.extend(proportional.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(TITLE_FAMILY.into()), titles);

    // Preserve egui's fixed-width Latin face; CJK uses the reading face when
    // the monospace fonts do not have the requested character.
    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.extend([BODY_FONT.into(), FALLBACK_FONT.into()]);
    ctx.set_fonts(fonts);
}

pub(crate) fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(TITLE_FAMILY.into()))
}
