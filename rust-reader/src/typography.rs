//! Reading typefaces are static, OFL-licensed instances with broad CJK coverage.
//! Their provenance and conversion are recorded in `licenses/ReadingFonts.md`.
use egui::{Context, FontData, FontDefinitions, FontFamily, FontId};

const BODY_FONT: &str = "reading-sans";
const TITLE_FONT: &str = "reading-serif";
const FALLBACK_FONT: &str = "yejian-cjk";
const TITLE_FAMILY: &str = "reading-title";
const SYSTEM_FONT: &str = "system-native";

/// Probe one native font file. Returns its bytes only when the file exists,
/// reads cleanly, and is a single .ttf/.otf face — never a .ttc collection:
/// ab_glyph (egui 0.37) panics on the `ttcf` tag, so collections and files
/// whose magic is neither 0x00010000 nor "OTTO" are rejected outright.
fn load_native_font(path: &std::path::Path) -> Option<Vec<u8>> {
    if !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let magic = bytes.get(..4)?;
    if magic == b"ttcf" {
        return None;
    }
    if magic != b"OTTO" && magic != [0x00, 0x01, 0x00, 0x00] {
        return None;
    }
    Some(bytes)
}

/// Candidates in priority order: Windows UI face + a broad CJK face.
/// Any that fail probing are silently skipped; the bundled fonts below
/// always remain as the guaranteed fallback chain.
fn native_font_candidates() -> Vec<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        vec![
            std::path::PathBuf::from(r"C:\Windows\Fonts\segoeui.ttf"),
            std::path::PathBuf::from(r"C:\Windows\Fonts\Deng.ttf"),
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

/// Install the fonts only. Existing text sizes and accessibility scaling stay
/// with the app's `text_styles` and `configure_fonts` functions.
pub(crate) fn configure(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let mut native = Vec::new();
    for (index, path) in native_font_candidates().iter().enumerate() {
        if let Some(bytes) = load_native_font(path) {
            fonts.font_data.insert(
                format!("{SYSTEM_FONT}-{index}").into(),
                FontData::from_owned(bytes).into(),
            );
            native.push(format!("{SYSTEM_FONT}-{index}"));
        }
    }
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
    // The bundled reading faces keep the head slots — their exact metrics are
    // load-bearing for layout. Probed native faces ride behind them as an
    // extra guaranteed-fallback chain (still ahead of egui's defaults).
    proportional.insert(0, BODY_FONT.into());
    proportional.push(FALLBACK_FONT.into());
    let tail = proportional.len();
    for (i, name) in native.into_iter().enumerate() {
        proportional.insert(tail + i, name);
    }
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
