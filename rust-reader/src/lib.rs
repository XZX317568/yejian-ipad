mod motion;
mod shelf;
mod worker;
mod ui;
mod sources;
mod sync;
mod vertical;
pub use ui::ReaderApp;

use std::path::PathBuf;

pub fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("yejian-cjk".into(),egui::FontData::from_static(include_bytes!("../assets/DroidSansFallback.ttf")).into());
    for family in [egui::FontFamily::Proportional,egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("yejian-cjk".into());
    }
    ctx.set_fonts(fonts);
}

pub fn run() -> eframe::Result {
    yejian_platform::init();
    let args: Vec<String> = std::env::args().collect();
    let root = args.iter().position(|x|x=="--data-dir").and_then(|i|args.get(i+1)).map(PathBuf::from)
        .unwrap_or_else(default_data_dir);
    let imports: Vec<PathBuf> = args.iter().position(|x|x=="--import").map(|i|args[i+1..].iter().map(PathBuf::from).collect()).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("页间").with_inner_size([1024.,768.]).with_min_inner_size([320.,480.]).with_app_id("app.yejian.reader.rust"),
        run_and_return: !cfg!(target_os="ios"),
        ..Default::default()
    };
    eframe::run_native("页间", options, Box::new(move|cc| {
        configure_fonts(&cc.egui_ctx);
        let wake=cc.egui_ctx.clone();
        yejian_platform::set_event_waker(move||wake.request_repaint());
        let mut app = ReaderApp::new(root,cc.egui_ctx.clone());
        app.import_files(imports);
        Ok(Box::new(app))
    }))
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
    if let Err(error)=run(){log::error!("页间启动失败：{error}");}
}
