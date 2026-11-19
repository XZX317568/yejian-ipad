mod motion;
mod shelf;
mod worker;
mod ui;
mod sources;
mod source_filters;
mod sync;
mod vertical;
#[cfg(target_os = "ios")]
mod ios_diagnostics;
pub use ui::ReaderApp;

use std::path::PathBuf;

pub fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("yejian-cjk".into(),egui::FontData::from_static(include_bytes!("../assets/DroidSansFallback.ttf")).into());
    for family in [egui::FontFamily::Proportional,egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("yejian-cjk".into());
    }
    ctx.set_fonts(fonts);
    // Touch-sized controls and comfortable line spacing. The default egui
    // metrics are desktop-mouse sized and read as a debug panel on iPad.
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(10., 9.);
            style.spacing.button_padding = egui::vec2(12., 8.);
            style.spacing.interact_size = egui::vec2(44., 44.);
            style.spacing.icon_width = 22.;
            style.spacing.icon_width_inner = 12.;
            style.spacing.icon_spacing = 8.;
            style.spacing.slider_width = 180.;
            style.text_styles = text_styles(1.);
        });
    }
}

fn text_styles(scale: f32) -> std::collections::BTreeMap<egui::TextStyle, egui::FontId> {
    let mut fonts = egui::Style::default().text_styles;
    for (style, size) in [
        (egui::TextStyle::Body, 17.),
        (egui::TextStyle::Button, 16.),
        (egui::TextStyle::Heading, 23.),
        (egui::TextStyle::Small, 13.),
        (egui::TextStyle::Monospace, 15.),
    ] {
        let font = fonts.get_mut(&style).expect("standard text style");
        font.size = size * scale;
    }
    fonts
}

pub fn run() -> eframe::Result {
    yejian_platform::init();
    log::debug!("platform initialized");
    let args: Vec<String> = std::env::args().collect();
    let root = args.iter().position(|x|x=="--data-dir").and_then(|i|args.get(i+1)).map(PathBuf::from)
        .unwrap_or_else(default_data_dir);
    let imports: Vec<PathBuf> = args.iter().position(|x|x=="--import").map(|i|args[i+1..].iter().map(PathBuf::from).collect()).unwrap_or_default();
    // Simulator-only UI snapshots. Release builds never see these flags.
    #[cfg(debug_assertions)]
    let seed_demo = args.iter().any(|x| x == "--seed-demo");
    #[cfg(debug_assertions)]
    let snapshot = args
        .iter()
        .position(|x| x == "--ui-snapshot")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("页间").with_inner_size([1024.,768.]).with_min_inner_size([320.,480.]).with_app_id("app.yejian.reader.rust"),
        run_and_return: !cfg!(target_os="ios"),
        #[cfg(target_os = "ios")]
        wgpu_options: ios_renderer_configuration(),
        ..Default::default()
    };
    log::info!("starting native renderer");
    eframe::run_native("页间", options, Box::new(move|cc| {
        log::info!("renderer ready; creating library UI");
        configure_fonts(&cc.egui_ctx);
        let wake=cc.egui_ctx.clone();
        yejian_platform::set_event_waker(move||wake.request_repaint());
        #[cfg(debug_assertions)]
        let demo_root = root.join("demo");
        let mut app = ReaderApp::new(root,cc.egui_ctx.clone());
        app.import_files(imports);
        #[cfg(debug_assertions)]
        {
            if seed_demo
                && let Ok(paths) = demo_archives(&demo_root)
            {
                app.import_files(paths);
            }
            if let Some(name) = snapshot {
                app.request_snapshot(name);
            }
        }
        Ok(Box::new(app))
    }))
}

#[cfg(target_os = "ios")]
fn ios_renderer_configuration() -> eframe::WgpuConfiguration {
    let mut config = eframe::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.device_descriptor = std::sync::Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
            label: Some("Yejian iPad renderer"),
            required_limits: ios_render_limits(&adapter.limits()),
            ..Default::default()
        });
    }
    config
}

#[cfg(any(target_os = "ios", test))]
fn ios_render_limits(supported: &eframe::wgpu::Limits) -> eframe::wgpu::Limits {
    // Metal on the iPad simulator supports 15 inter-stage variables, while
    // WebGPU's default asks for 16. egui's shader only uses two. The native
    // downlevel baseline covers our 2D renderer without demanding unused
    // WebGPU features, and preserves the adapter's actual display resolution.
    eframe::wgpu::Limits::downlevel_defaults()
        .using_resolution(supported.clone())
        .using_alignment(supported.clone())
}

#[cfg(test)]
mod renderer_tests {
    #[test]
    fn ipad_limits_accept_metal_downlevel_and_retina_surfaces() {
        let supported = eframe::wgpu::Limits {
            max_texture_dimension_2d: 8192,
            ..eframe::wgpu::Limits::downlevel_defaults()
        };
        let requested = super::ios_render_limits(&supported);
        assert!(requested.check_limits(&supported));
        assert_eq!(requested.max_inter_stage_shader_variables, 15);
        assert_eq!(requested.max_texture_dimension_2d, 8192);
    }
}

/// Five small original archives for simulator screenshots.
#[cfg(debug_assertions)]
fn demo_archives(root: &std::path::Path) -> anyhow::Result<Vec<PathBuf>> {
    use std::io::Write;
    std::fs::create_dir_all(root)?;
    let titles = ["山海之间", "深蓝来信", "雨后列车", "小镇回声", "月光旅人"];
    let colors = [
        [52u8, 87, 83],
        [40, 75, 108],
        [152, 118, 77],
        [104, 88, 121],
        [182, 95, 74],
    ];
    let mut paths = Vec::new();
    for (index, title) in titles.iter().enumerate() {
        let path = root.join(format!("{title}.cbz"));
        if path.is_file() {
            paths.push(path);
            continue;
        }
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
        for page in 0..6 {
            let mut image = image::RgbImage::new(640, 960);
            for (y, row) in image.enumerate_rows_mut() {
                for (x, _, pixel) in row {
                    let base = colors[index];
                    let sun = (x as i32 - 380).pow(2) + (y as i32 - 225).pow(2) < 155 * 155;
                    let wave = y as f32 > 400. + (x as f32 / 110.).sin() * 65. + page as f32 * 12.;
                    *pixel = image::Rgb(if sun {
                        [223, 224, 207]
                    } else if wave {
                        [base[0] / 2, base[1] / 2, base[2] / 2]
                    } else {
                        base
                    });
                }
            }
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png)?;
            zip.start_file(
                format!("{:03}.png", page + 1),
                zip::write::SimpleFileOptions::default(),
            )?;
            zip.write_all(&bytes.into_inner())?;
        }
        zip.finish()?;
        paths.push(path);
    }
    Ok(paths)
}

pub fn default_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("YEJIAN_DATA_DIR") { return path.into(); }
    #[cfg(target_os="windows")]
    if let Some(path) = std::env::var_os("LOCALAPPDATA") { return PathBuf::from(path).join("Yejian"); }
    #[cfg(target_os="ios")]
    if let Some(path) = std::env::var_os("HOME") { return PathBuf::from(path).join("Documents").join("Yejian"); }
    PathBuf::from("data")
}

#[cfg(target_os="android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));
    yejian_platform::init();
    let Some(data)=app.internal_data_path() else {log::error!("Android app data directory unavailable");return;};
    let root = data.join("library");
    let options = eframe::NativeOptions { android_app:Some(app), ..Default::default() };
    if let Err(e) = eframe::run_native("页间",options,Box::new(move|cc|{
        configure_fonts(&cc.egui_ctx);
        let wake=cc.egui_ctx.clone();
        yejian_platform::set_event_waker(move||wake.request_repaint());
        Ok(Box::new(ReaderApp::new(root,cc.egui_ctx.clone())))
    })) { log::error!("页间启动失败：{e}"); }
}

#[cfg(target_os="ios")]
#[unsafe(no_mangle)]
pub extern "C" fn yejian_start_app() {
    ios_diagnostics::init();
    log::info!("yejian_start_app: enter");
    match run() {
        Ok(()) => log::info!("yejian_start_app: run returned"),
        Err(error) => log::error!("yejian_start_app: {error}"),
    }
    log::logger().flush();
}
