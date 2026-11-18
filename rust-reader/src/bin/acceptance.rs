//! Runs the production UI on an offscreen GPU target, with real pointer input
//! and a real temporary SQLite library. No desktop focus is taken.
use anyhow::{Context as _, Result};
use egui::*;
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use yejian::ReaderApp;

struct Harness {
    ctx: Context,
    app: ReaderApp,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    now: f64,
    size: [u32; 2],
    output: PathBuf,
    repaint_delay: Duration,
    texts: Vec<(String, Rect)>,
}
impl Harness {
    fn new(root: PathBuf, output: PathBuf) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        println!("Offscreen adapter: {}", adapter.get_info().name);
        let renderer = Renderer::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            RendererOptions::default(),
        );
        let ctx = Context::default();
        yejian::configure_fonts(&ctx);
        let app = ReaderApp::new(root, ctx.clone());
        Ok(Self {
            ctx,
            app,
            device,
            queue,
            renderer,
            now: 0.,
            size: [390, 844],
            output,
            repaint_delay: Duration::ZERO,
            texts: Vec::new(),
        })
    }
    fn frame(&mut self, events: Vec<Event>, name: Option<&str>) -> Result<()> {
        self.now += 1. / 60.;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(self.size[0] as f32, self.size[1] as f32),
            )),
            time: Some(self.now),
            events,
            ..Default::default()
        };
        let mut output = self.ctx.run_ui(input, |ui| self.app.draw(ui));
        self.texts.clear();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, clipped.clip_rect, &mut self.texts);
        }
        self.repaint_delay = output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let descriptor = ScreenDescriptor {
            size_in_pixels: self.size,
            pixels_per_point: output.pixels_per_point,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yejian-acceptance"),
            size: wgpu::Extent3d {
                width: self.size[0],
                height: self.size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let commands = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &jobs,
            &descriptor,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("acceptance"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &jobs, &descriptor);
        }
        let stride = (self.size[0] * 4).div_ceil(256) * 256;
        let buffer = name.map(|_| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture"),
                size: (stride * self.size[1]) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        if let Some(buffer) = &buffer {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: self.size[0],
                    height: self.size[1],
                    depth_or_array_layers: 1,
                },
            );
        }
        self.queue
            .submit(commands.into_iter().chain([encoder.finish()]));
        if let (Some(buffer), Some(name)) = (buffer, name) {
            let (tx, rx) = std::sync::mpsc::channel();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.device.poll(wgpu::PollType::wait_indefinitely())?;
            rx.recv()??;
            let mapped = buffer.slice(..).get_mapped_range()?;
            let mut pixels = Vec::with_capacity((self.size[0] * self.size[1] * 4) as usize);
            for row in mapped.chunks(stride as usize) {
                pixels.extend_from_slice(&row[..self.size[0] as usize * 4]);
            }
            image::save_buffer(
                self.output.join(format!("{name}.png")),
                &pixels,
                self.size[0],
                self.size[1],
                image::ColorType::Rgba8,
            )?;
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        output.textures_delta.clear();
        Ok(())
    }
    fn frames(&mut self, n: usize) -> Result<()> {
        for _ in 0..n {
            self.frame(vec![], None)?;
        }
        Ok(())
    }
    fn click(&mut self, id: &str) -> Result<()> {
        let pos = self
            .app
            .hitboxes
            .get(id)
            .with_context(|| format!("missing control {id}"))?
            .center();
        self.click_pos(pos)
    }
    fn click_text(&mut self, label: &str) -> Result<()> {
        let rect = self
            .texts
            .iter()
            .find(|(text, _)| text == label)
            .with_context(|| format!("visible control {label} is missing"))?
            .1;
        self.click_pos(rect.center())
    }
    fn read_from_beginning(&mut self) -> Result<()> {
        if self.app.hitboxes.contains_key("read-start") {
            self.click("read-start")
        } else {
            self.click("read")
        }
    }
    fn click_pos(&mut self, pos: Pos2) -> Result<()> {
        self.frame(
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            None,
        )?;
        self.frame(
            vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            None,
        )
    }
    fn key(&mut self, key: Key) -> Result<()> {
        self.frame(
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            None,
        )?;
        self.frame(
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            None,
        )
    }
    fn drag(&mut self, start: Pos2, end: Pos2) -> Result<()> {
        self.frame(
            vec![
                Event::PointerMoved(start),
                Event::PointerButton {
                    pos: start,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            None,
        )?;
        for step in 1..=12 {
            self.frame(
                vec![Event::PointerMoved(start.lerp(end, step as f32 / 12.))],
                None,
            )?;
        }
        // Hold the final position before release: this checks seam handling
        // without adding a fling to the test's precisely measured distance.
        self.frames(20)?;
        self.frame(
            vec![Event::PointerButton {
                pos: end,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            None,
        )?;
        self.frames(5)
    }
    fn wait(&mut self, predicate: impl Fn(&ReaderApp) -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(25);
        while !predicate(&self.app) {
            anyhow::ensure!(
                Instant::now() < deadline,
                "UI operation timed out; screen {}",
                self.app.screen_name()
            );
            self.frame(vec![], None)?;
            std::thread::sleep(Duration::from_millis(15));
        }
        Ok(())
    }
}

fn collect_text(shape: &Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
    match shape {
        Shape::Text(text) => {
            let rect = text.visual_bounding_rect().intersect(clip);
            if rect.is_positive() {
                out.push((text.galley.job.text.clone(), rect));
            }
        }
        Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, clip, out);
            }
        }
        _ => {}
    }
}

fn fixtures(root: &Path) -> Result<Vec<PathBuf>> {
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
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
        for page in 0..6 {
            let mut image =
                image::RgbImage::new(640, if index == 0 && page == 2 { 9000 } else { 960 });
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
                    if page > 0 && x > 35 && x < 605 && (y % 280 < 8 || x % 280 < 7) {
                        *pixel = image::Rgb([232, 235, 226]);
                    }
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
fn main() -> Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../artifacts/rust"));
    std::fs::create_dir_all(&output)?;
    std::fs::create_dir_all(output.join("motion"))?;
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let data = output.join(format!("acceptance-{run}"));
    let paths = fixtures(&data.join("fixtures"))?;
    let mut h = Harness::new(data.join("library"), output.clone())?;
    h.wait(ReaderApp::ready)?;
    h.frame(vec![], Some("empty-phone"))?;
    h.app.import_files(paths);
    h.wait(|a| a.book_count() == 5)?;
    h.frames(50)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.now += 6.;
    h.frames(3)?;
    h.frame(vec![], Some("phone-shelf"))?;
    anyhow::ensure!(h.app.rendered_book_nodes < 45);
    anyhow::ensure!(h.texts.iter().any(|(text, _)| text == "开始阅读"));
    h.click("shelf-next")?;
    for i in 0..35 {
        h.frame(vec![], Some(&format!("motion/frame-{i:03}")))?;
    }
    h.wait(ReaderApp::covers_loaded)?;
    h.frame(vec![], Some("phone-selected"))?;
    h.click("shelf-center")?;
    for i in 0..65 {
        let name = format!("motion/frame-{:03}", i + 35);
        h.frame(vec![], Some(&name))?;
        if [4, 12, 24, 36, 48, 60].contains(&i) {
            std::fs::copy(
                output.join(format!("{name}.png")),
                output.join(format!("open-{i:02}.png")),
            )?;
        }
    }
    anyhow::ensure!(h.app.screen_name() == "detail");
    h.frame(vec![], Some("detail"))?;
    h.click("read")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], Some("reader-page-1"))?;
    h.click("reader-next")?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(h.app.current_page() == 1);
    h.frame(vec![], Some("reader-page-2"))?;
    // Swipe uses accumulated drag distance, not the zero release-frame delta.
    let start = pos2(290., 420.);
    h.frame(
        vec![
            Event::PointerMoved(start),
            Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        None,
    )?;
    for x in [265., 235., 205., 175., 145., 115.] {
        h.frame(vec![Event::PointerMoved(pos2(x, 420.))], None)?;
    }
    h.frame(
        vec![Event::PointerButton {
            pos: pos2(115., 420.),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        None,
    )?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(h.app.current_page() == 2, "swipe failed to turn a page");
    h.click("reader-back")?;
    h.frames(3)?;
    h.click("back")?;
    for i in 0..60 {
        h.frame(vec![], Some(&format!("motion/frame-{:03}", i + 100)))?;
    }
    h.frame(vec![], Some("return-shelf"))?;
    h.frames(5)?;
    anyhow::ensure!(!h.app.frame_is_animating, "shelf keeps animating at rest");
    anyhow::ensure!(
        h.repaint_delay >= Duration::from_secs(1),
        "idle shelf still schedules rapid repaint: {:?}",
        h.repaint_delay
    );
    let idle_delay = h.repaint_delay;
    h.size = [1024, 768];
    h.frames(5)?;
    h.frame(vec![], Some("tablet-shelf"))?;
    // iPad window sizes: 11" landscape and portrait, plus a narrow Split View.
    for (width, height, name) in [
        (1366, 1024, "ipad-landscape-shelf"),
        (1024, 1366, "ipad-portrait-shelf"),
        (507, 1024, "ipad-split-shelf"),
    ] {
        h.size = [width, height];
        h.frames(6)?;
        h.frame(vec![], Some(name))?;
    }
    h.size = [1366, 1024];
    h.frames(6)?;
    h.click("open-book")?;
    h.frames(65)?;
    eprintln!("ipad landscape after open-book: {}", h.app.screen_name());
    h.frame(vec![], Some("ipad-landscape-detail"))?;
    h.click("back")?;
    h.frames(90)?;
    h.size = [1024, 1366];
    h.frames(6)?;
    h.click("open-book")?;
    h.frames(65)?;
    eprintln!("ipad portrait after open-book: {}", h.app.screen_name());
    h.frame(vec![], Some("ipad-portrait-detail"))?;
    h.click("back")?;
    h.frames(90)?;
    h.size = [1024, 768];
    h.frames(4)?;
    h.click("nav-history")?;
    h.frames(3)?;
    h.frame(vec![], Some("history"))?;
    h.click("nav-settings")?;
    h.frames(3)?;
    h.frame(vec![], Some("settings"))?;
    h.size = [1366, 1024];
    h.frames(6)?;
    h.click("nav-search")?;
    h.frames(4)?;
    h.frame(vec![], Some("ipad-landscape-sources"))?;
    h.click("nav-history")?;
    h.frames(4)?;
    h.frame(vec![], Some("ipad-landscape-history"))?;
    h.click("nav-settings")?;
    h.frames(4)?;
    h.frame(vec![], Some("ipad-landscape-settings"))?;
    h.click("nav-library")?;
    h.frames(4)?;
    h.size = [1024, 768];
    h.frames(4)?;
    // Reopen the real database from disk; in-memory state is insufficient.
    let reopened = yejian_core::Library::open(&data.join("library"))?;
    anyhow::ensure!(
        reopened
            .list_books("")?
            .iter()
            .any(|b| b.current_page == 2 && b.last_read_at.is_some()),
        "page did not persist"
    );
    reopened.set_setting("theme", "dark")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [390, 844];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.frame(vec![], Some("phone-dark"))?;
    h.click("open-book")?;
    h.frames(65)?;
    h.click("read")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], Some("reader-dark"))?;
    h.key(Key::BrowserBack)?;
    anyhow::ensure!(
        h.app.screen_name() == "detail",
        "system back did not return from reader"
    );
    h.key(Key::BrowserBack)?;
    h.frames(60)?;
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "system back did not return to shelf"
    );
    reopened.set_setting("reading-mode", "spread")?;
    reopened.set_setting("reading-direction", "rtl")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [1024, 768];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.click("open-book")?;
    h.frames(65)?;
    h.read_from_beginning()?;
    h.wait(ReaderApp::page_loaded)?;
    // Prefetch the second page before checking the two-page arrangement.
    h.frames(10)?;
    std::thread::sleep(Duration::from_millis(120));
    h.frames(3)?;
    h.frame(vec![], Some("reader-rtl-spread"))?;
    let right_start = pos2(180., 380.);
    h.frame(
        vec![
            Event::PointerMoved(right_start),
            Event::PointerButton {
                pos: right_start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        None,
    )?;
    for x in [210., 250., 290., 330.] {
        h.frame(vec![Event::PointerMoved(pos2(x, 380.))], None)?;
    }
    h.frame(
        vec![Event::PointerButton {
            pos: pos2(330., 380.),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        None,
    )?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_page() == 2,
        "right-to-left spread swipe did not advance by two pages"
    );
    h.key(Key::BrowserBack)?;
    h.key(Key::BrowserBack)?;
    h.frames(60)?;
    h.frame(vec![], Some("tablet-return-system-back"))?;
    reopened.set_setting("font-scale", "2")?;
    reopened.set_setting("theme", "light")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [375, 768];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.frame(vec![], Some("split-large-text"))?;
    let shortcut = h.app.hitboxes["shelf-read"];
    anyhow::ensure!(shortcut.bottom() < h.app.hitboxes["nav-library"].top());
    anyhow::ensure!(!shortcut.intersects(h.app.hitboxes["shelf-prev"]));
    // Search through the real field, including an empty result.
    h.click("search")?;
    h.frames(2)?;
    h.click("search-field")?;
    h.frame(vec![Event::Text("不存在的漫画".into())], None)?;
    h.frame(vec![], Some("search-empty"))?;
    h.click("clear-search")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("open-book"),
        "clear search did not restore the shelf"
    );
    // Searching down to one book must cancel movement and leave its cover
    // tappable. Exercise both routes at 200% text with the search still active.
    let unread = reopened.list_books("")?.into_iter().find(|b| b.last_read_at.is_none())
        .context("expected an unread fixture")?;
    h.click("search")?;
    h.frames(2)?;
    h.click("search-field")?;
    h.frame(vec![Event::Text(unread.title.clone())], None)?;
    h.frames(3)?;
    anyhow::ensure!(h.app.shelf_book_count() == 1 && !h.app.hitboxes.contains_key("shelf-next"));
    let center = h.app.hitboxes["shelf-center"].center();
    h.frame(vec![Event::PointerMoved(center), Event::MouseWheel {
        unit: MouseWheelUnit::Point, delta: vec2(90., 0.), phase: TouchPhase::Move,
        source: MouseWheelSource::Wheel, modifiers: Modifiers::NONE,
    }], None)?;
    h.drag(center, center - vec2(85., 0.))?;
    anyhow::ensure!(center.distance(h.app.hitboxes["shelf-center"].center()) < 0.1);
    h.frame(vec![], Some("single-large-text"))?;
    h.click("shelf-center")?;
    h.frames(16)?;
    anyhow::ensure!(h.app.screen_name() == "detail" && h.app.frame_is_animating);
    h.key(Key::BrowserBack)?;
    h.frames(60)?;
    anyhow::ensure!(h.app.screen_name() == "library" && h.app.shelf_book_count() == 1);
    h.click_text("开始阅读")?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(h.app.current_book_id() == Some(unread.id.as_str()) && h.app.current_page() == 0);
    h.click("reader-back")?;
    h.frames(4)?;
    anyhow::ensure!(h.app.screen_name() == "library" && h.app.shelf_book_count() == 1);
    h.size = [844, 390];
    h.frames(3)?;
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(844., 390.));
    anyhow::ensure!(screen.contains_rect(h.app.hitboxes["shelf-read"]));
    h.frame(vec![], Some("phone-landscape-shelf"))?;
    h.click("shelf-center")?;
    h.frames(65)?;
    h.frame(vec![], Some("phone-landscape-detail"))?;
    anyhow::ensure!(screen.contains_rect(h.app.hitboxes["read"]));
    h.click_text("继续阅读 · 第 1 页")?;
    h.wait(ReaderApp::page_loaded)?;
    h.key(Key::BrowserBack)?;
    h.key(Key::BrowserBack)?;
    h.frames(60)?;
    anyhow::ensure!(h.app.screen_name() == "library");
    // Variable-height continuous reading stores a page-relative anchor.
    reopened.set_setting("font-scale", "1")?;
    reopened.set_setting("reading-mode", "vertical")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [390, 844];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.click("open-book")?;
    h.frames(65)?;
    h.read_from_beginning()?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(15)?;
    std::thread::sleep(Duration::from_millis(100));
    h.frames(3)?;
    h.drag(pos2(190., 650.), pos2(190., 300.))?;
    let first = h
        .app
        .hitboxes
        .get("vertical-page-0")
        .context("first continuous page is not visible")?;
    let second = h
        .app
        .hitboxes
        .get("vertical-page-1")
        .context("next continuous page is not visible")?;
    anyhow::ensure!(
        (first.bottom() - second.top()).abs() < 0.1,
        "continuous pages have a gap or overlap"
    );
    h.frame(vec![], Some("reader-continuous-seam"))?;
    h.drag(pos2(190., 650.), pos2(190., 300.))?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_page() == 1,
        "continuous scrolling reset or skipped a page"
    );
    let fraction = h.app.current_fraction();
    anyhow::ensure!(
        fraction > 0.05 && fraction < 0.7,
        "scroll remainder was lost: {fraction}"
    );
    let book_id = h.app.current_book_id().unwrap().to_string();
    h.size = [768, 1024];
    h.frames(5)?;
    anyhow::ensure!(
        (h.app.current_fraction() - fraction).abs() < 0.002,
        "resizing lost the page-relative anchor"
    );
    h.frame(vec![], Some("reader-continuous-resized"))?;
    h.key(Key::BrowserBack)?;
    h.wait(ReaderApp::position_saved)?;
    let saved = reopened.get_book(&book_id)?;
    anyhow::ensure!(
        saved.current_page == 1 && (saved.page_fraction - fraction).abs() < 0.002,
        "page fraction was not persisted"
    );
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [390, 844];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.frame(vec![], Some("shelf-resume"))?;
    h.click_text("继续阅读")?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(book_id.as_str())
            && h.app.current_page() == 1 && (h.app.current_fraction() - fraction).abs() < 0.002,
        "one-tap resume lost the book or its within-page position"
    );
    h.wait(|app| app.rendered_page_nodes >= 2)?;
    h.frame(vec![], Some("reader-continuous-resumed"))?;
    h.frames(10)?;
    anyhow::ensure!(
        !h.app.frame_is_animating && h.repaint_delay >= Duration::from_secs(1),
        "continuous reader keeps repainting at rest"
    );
    h.key(Key::BrowserBack)?;
    anyhow::ensure!(h.app.screen_name() == "library", "back from the shortcut opened details");

    // Explicit source identities group downloaded chapters; another source's
    // same title stays a separate work. Only downloaded chapters are traversed.
    let books = reopened.list_books("")?;
    let one = books.iter().find(|b| b.title == "山海之间").unwrap();
    let two = books.iter().find(|b| b.title == "深蓝来信").unwrap();
    let foreign = books.iter().find(|b| b.title == "小镇回声").unwrap();
    for (book, source, n) in [
        (one, "fixture-a", 1.),
        (two, "fixture-a", 2.),
        (foreign, "fixture-b", 1.),
    ] {
        reopened.link_chapter(
            &book.id,
            &yejian_core::ChapterMetadata {
                source_id: source.into(),
                manga_id: "shared-title".into(),
                manga_title: "星河之间".into(),
                chapter_id: format!("chapter-{n}"),
                chapter_title: format!("第 {n} 话"),
                chapter_number: Some(n),
                volume_number: None,
            },
        )?;
    }
    reopened.set_setting("reading-mode", "paged")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    anyhow::ensure!(
        h.app.shelf_book_count() == 4,
        "chapters are duplicated in the shelf or sources were merged"
    );
    h.click("search")?;
    h.frames(2)?;
    h.click("search-field")?;
    h.frame(vec![Event::Text("山海之间".into())], None)?;
    h.frames(4)?;
    h.click("open-book")?;
    h.frames(65)?;
    h.read_from_beginning()?;
    h.wait(ReaderApp::page_loaded)?;
    for _ in 0..6 {
        h.click("reader-next")?;
        h.wait(ReaderApp::page_loaded)?;
    }
    anyhow::ensure!(
        h.app.current_book_id() == Some(two.id.as_str()) && h.app.current_page() == 0,
        "end-of-chapter did not continue into the next downloaded chapter"
    );
    h.frame(vec![], Some("reader-next-chapter"))?;
    h.click("reader-chapters")?;
    h.frames(12)?;
    h.frame(vec![], Some("chapter-picker"))?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key(&format!("chapter-{}", one.id))
            && !h
                .app
                .hitboxes
                .contains_key(&format!("chapter-{}", foreign.id)),
        "chapter picker mixes another source"
    );
    h.key(Key::BrowserBack)?;
    anyhow::ensure!(
        h.app.screen_name() == "reader",
        "back from chapter picker exited reading"
    );
    h.key(Key::BrowserBack)?;
    h.key(Key::BrowserBack)?;
    h.frames(60)?;
    h.frame(vec![], Some("grouped-shelf"))?;
    reopened.set_setting("reading-mode", "vertical")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.click("search")?;
    h.frames(2)?;
    h.click("search-field")?;
    h.frame(vec![Event::Text("山海之间".into())], None)?;
    h.frames(4)?;
    h.click("open-book")?;
    h.frames(65)?;
    h.read_from_beginning()?;
    h.wait(ReaderApp::page_loaded)?;
    h.click("reader-next")?;
    h.wait(ReaderApp::page_loaded)?;
    h.click("reader-next")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app
            .hitboxes
            .get("vertical-page-2")
            .is_some_and(|rect| rect.height() > 4000.),
        "long strip was reduced to a thumbnail"
    );
    h.drag(pos2(190., 680.), pos2(190., 200.))?;
    anyhow::ensure!(
        h.app.current_page() == 2 && h.app.current_fraction() > 0.02,
        "long strip did not scroll within its page"
    );
    h.frame(vec![], Some("reader-long-strip"))?;
    h.frames(10)?;
    anyhow::ensure!(
        !h.app.frame_is_animating && h.repaint_delay >= Duration::from_secs(1),
        "long-strip reader repaints at rest"
    );
    let closing_id = h.app.current_book_id().unwrap().to_string();
    let closing_fraction = h.app.current_fraction();
    eframe::App::on_exit(&mut h.app);
    let closing_saved = reopened.get_book(&closing_id)?;
    anyhow::ensure!(
        closing_saved.current_page == 2
            && (closing_saved.page_fraction - closing_fraction).abs() < 0.0001,
        "normal exit returned before the final reading position was committed"
    );
    // A persisted, interrupted queue is visible without repeating a search.
    // These records are labelled fixture data; the queue execution itself is
    // covered by the real WASM/HTTP and recovery tests in sources.rs.
    let task = |id: &str, chapter: &str, status: &str| {
        serde_json::json!({
            "id":id,"source_id":"preview-source","manga_id":"preview-story",
            "manga_title":"下载恢复示例","chapter_id":chapter,"chapter_title":format!("第 {chapter} 话"),
            "status":status,"pages_done":2,"pages_total":6,
            "error":if status=="Failed" {Some("示例：网络中断，完整的2页已经保留")}else{None},
        })
    };
    let store = serde_json::json!({"tasks":[task("0123456789abcdef","1","Paused"),task("1123456789abcdef","2","Failed"),task("2123456789abcdef","3","Queued")]});
    std::fs::write(
        data.join("library/download-tasks.json"),
        serde_json::to_vec(&store)?,
    )?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.frames(20)?;
    h.click("nav-search")?;
    h.frames(6)?;
    h.frame(vec![], Some("download-recovery-queue"))?;
    reopened.set_setting("font-scale", "2")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [375, 768];
    h.wait(ReaderApp::ready)?;
    h.frames(20)?;
    h.click("nav-search")?;
    h.frames(6)?;
    h.frame(vec![], Some("download-recovery-large-text"))?;
    anyhow::ensure!(
        h.texts.iter().any(|(text, _)| text == "继续"),
        "large-text queue has no visible continue control"
    );
    reopened.set_setting("font-scale", "1")?;
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.size = [390, 844];
    h.wait(ReaderApp::ready)?;
    h.frames(20)?;
    h.click("nav-search")?;
    h.frames(6)?;
    anyhow::ensure!(
        h.texts.iter().any(|(text, _)| text == "继续"),
        "restarted download queue has no visible continue control"
    );
    h.click_text("继续")?;
    h.frames(8)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let store: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.join("library/download-tasks.json"))?)?;
        if store["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == "0123456789abcdef" && task["status"] == "Failed")
        {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "queue continue control did not execute the task"
        );
        h.frames(2)?;
        std::thread::sleep(Duration::from_millis(15));
    }
    h.frame(vec![], Some("download-recovery-retry"))?;
    println!(
        "PASS: import, cover open/return, single-book drag and interrupted flight, one-tap start/resume/back at phone and large text, paged/spread, continuous seams and fractional restore, grouped chapters, recoverable queue, idle stops. Captures at {}",
        output.display()
    );
    std::fs::write(
        output.join("acceptance-result.txt"),
        format!(
            "PASS\nLibrary: {}\nBook count: {}\nNodes: {}\nIdle animation: {}\nIdle repaint delay: {:?}\n",
            data.display(),
            h.app.book_count(),
            h.app.rendered_book_nodes,
            h.app.frame_is_animating,
            idle_delay
        ),
    )?;
    Ok(())
}
