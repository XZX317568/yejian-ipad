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
    painted_text: Vec<PaintedText>,
    painted_images: Vec<PaintedImage>,
}
struct PaintedText {
    text: String,
    bounds: Rect,
    clip: Rect,
}
struct PaintedImage {
    bounds: Rect,
    clip: Rect,
}
impl Harness {
    fn new(root: PathBuf, output: PathBuf) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        // Request the adapter's full limits so decoded pages wider than the
        // 2048 downlevel default fit (real devices expose 8192+).
        let required_limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                required_limits,
                ..Default::default()
            },
        ))?;
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
            painted_text: Vec::new(),
            painted_images: Vec::new(),
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
            max_texture_side: Some(self.device.limits().max_texture_dimension_2d as usize),
            ..Default::default()
        };
        let mut output = self.ctx.run_ui(input, |ui| self.app.draw(ui));
        self.texts.clear();
        self.painted_text.clear();
        self.painted_images.clear();
        for clipped in &output.shapes {
            collect_text(
                &clipped.shape,
                clipped.clip_rect,
                &mut self.texts,
                &mut self.painted_text,
            );
            collect_images(&clipped.shape, clipped.clip_rect, &mut self.painted_images);
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

/// Minimal fake Bangumi API for the tracker-bangumi capture: serves
/// `/v0/me` and 404s user collections (nothing collected yet).
fn fake_bangumi() -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = vec![0u8; 64 * 1024];
        let mut request = Vec::new();
        let read = stream.read(&mut buffer).unwrap_or(0);
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request).to_string();
        let head = text.split("\r\n\r\n").next().unwrap_or("");
        let line = head.lines().next().unwrap_or("");
        let (status, reply) = if line.starts_with("GET /v0/me") {
            (
                "200",
                r#"{"username":"tester","nickname":"测试者","avatar":{"large":""}}"#,
            )
        } else if line.starts_with("POST /v0/users/-/collections/") {
            ("200", r#"{"id":99,"type":3,"ep_status":0}"#)
        } else if line.starts_with("POST /v0/search/subjects") {
            (
                "200",
                r#"{"data":[{"id":99,"name":"作品A","name_cn":"作品A","volumes":120,"images":{"large":""}}]}"#,
            )
        } else {
            ("404", r#"{"title":"Not Found"}"#)
        };
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    (format!("http://{addr}"), handle)
}

/// Minimal in-process WebDAV server: `MKCOL`, `GET` + `ETag`, `PUT` with
/// `If-None-Match`/`If-Match`. Files live in the shared map so the test can
/// seed a remote snapshot before the app syncs (a real two-root merge).
fn fake_webdav() -> (
    String,
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (Vec<u8>, String)>>>,
    std::thread::JoinHandle<()>,
) {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let files: Arc<Mutex<HashMap<String, (Vec<u8>, String)>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let store = files.clone();
    let handle = std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = Vec::new();
        let mut buffer = vec![0u8; 64 * 1024];
        // Read the head, then however many body bytes Content-Length asks for.
        loop {
            let read = stream.read(&mut buffer).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(pos) = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
            {
                let head = String::from_utf8_lossy(&request[..pos]).to_string();
                let want: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= pos + 4 + want {
                    break;
                }
            }
        }
        let head_end = request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(request.len());
        let head = String::from_utf8_lossy(&request[..head_end]).to_string();
        let body = request[head_end + 4.min(request.len() - head_end).min(4)..].to_vec();
        let line = head.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/").to_string();
        let header = |name: &str| {
            head.lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix(&format!("{}:", name.to_ascii_lowercase()))
                        .map(|v| v.trim().to_string())
                })
        };
        let mut files = store.lock().unwrap();
        let next_etag = || format!("\"{}\"", files.len() + 1);
        let (status, etag, reply): (&str, Option<String>, Vec<u8>) = match method {
            "MKCOL" => ("201", None, Vec::new()),
            "GET" => match files.get(&path) {
                Some((body, etag)) => ("200", Some(etag.clone()), body.clone()),
                None => ("404", None, b"not found".to_vec()),
            },
            "PUT" => {
                let exists = files.contains_key(&path);
                if header("if-none-match").as_deref() == Some("*") && exists {
                    ("412", None, Vec::new())
                } else if let Some(want) = header("if-match") {
                    let have = files.get(&path).map(|(_, tag)| tag.clone());
                    if have.as_deref() != Some(want.as_str()) {
                        ("412", None, Vec::new())
                    } else {
                        let etag = next_etag();
                        files.insert(path, (body.clone(), etag.clone()));
                        ("204", Some(etag), Vec::new())
                    }
                } else {
                    let etag = next_etag();
                    files.insert(path, (body.clone(), etag.clone()));
                    (if exists { "204" } else { "201" }, Some(etag), Vec::new())
                }
            }
            _ => ("405", None, Vec::new()),
        };
        drop(files);
        let mut response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
            reply.len()
        );
        if let Some(etag) = etag {
            response.push_str(&format!("ETag: {etag}\r\n"));
        }
        response.push_str("\r\n");
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(&reply);
    });
    (format!("http://{addr}/"), files, handle)
}

fn collect_text(
    shape: &Shape,
    clip: Rect,
    out: &mut Vec<(String, Rect)>,
    painted: &mut Vec<PaintedText>,
) {
    match shape {
        Shape::Text(text) => {
            let bounds = text.visual_bounding_rect();
            let rect = bounds.intersect(clip);
            if rect.is_positive() {
                out.push((text.galley.job.text.clone(), rect));
                painted.push(PaintedText {
                    text: text.galley.job.text.clone(),
                    bounds,
                    clip,
                });
            }
        }
        Shape::Vec(shapes) => {
            for shape in shapes {
                collect_text(shape, clip, out, painted);
            }
        }
        _ => {}
    }
}

fn collect_images(shape: &Shape, clip: Rect, out: &mut Vec<PaintedImage>) {
    match shape {
        Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => {
            let bounds = Rect::from_points(
                &mesh
                    .vertices
                    .iter()
                    .map(|vertex| vertex.pos)
                    .collect::<Vec<_>>(),
            );
            if bounds.intersect(clip).is_positive() {
                out.push(PaintedImage { bounds, clip });
            }
        }
        Shape::Vec(shapes) => {
            for shape in shapes {
                collect_images(shape, clip, out);
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

fn design_safe_rect(h: &Harness) -> Rect {
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(h.size[0] as f32, h.size[1] as f32));
    let insets = yejian_platform::safe_insets().unwrap_or_default();
    let scale = h.ctx.pixels_per_point();
    screen
        .intersect(h.ctx.content_rect())
        .intersect(Rect::from_min_max(
            screen.min + vec2(insets.left as f32 / scale, insets.top as f32 / scale),
            screen.max - vec2(insets.right as f32 / scale, insets.bottom as f32 / scale),
        ))
}

/// A painted label can have a positive intersection while most of the button
/// is outside its scroll viewport. Check its original bounds against the
/// actual painter clip as well as the platform safe area.
fn design_label_visible(h: &Harness, label: &str) -> bool {
    let safe = design_safe_rect(h).expand(0.5);
    h.painted_text.iter().any(|text| {
        text.text == label
            && safe.contains_rect(text.bounds)
            && text.clip.expand(0.5).contains_rect(text.bounds)
    })
}

/// Wheels the pointer over the scrollable page so checks can reach controls
/// above or below the fold (the online detail header is taller than a phone
/// or landscape viewport).
fn scroll_page(h: &mut Harness, delta: f32) -> Result<()> {
    let center = pos2(h.size[0] as f32 / 2., (h.size[1] as f32 / 2.).min(400.));
    for _ in 0..6 {
        h.frame(
            vec![
                Event::PointerMoved(center),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., delta),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(3)?;
    }
    Ok(())
}

/// Scroll the page until hitbox `id` is fully inside its scroll clip and
/// its `label` caption is painted within it — the same condition
/// design_touch asserts. Used for rows the stats card can push anywhere
/// below the fold.
fn scroll_until_row_visible(h: &mut Harness, id: &str, label: &str) -> Result<()> {
    let mut last_center: Option<f32> = None;
    for _ in 0..60 {
        let rect = h.app.hitboxes.get(id).copied();
        if let Some(rect) = rect {
            let visible = h.painted_text.iter().any(|text| {
                text.text == label
                    && rect.expand(0.5).contains_rect(text.bounds)
                    && text.clip.expand(0.5).contains_rect(rect)
            });
            if visible {
                return Ok(());
            }
            last_center = Some(rect.center().y);
            // Negative delta scrolls down (rows move up) and vice versa.
            // Use the row's own scroll clip to pick the direction: if the
            // bottom is cut, scroll down; if the top is cut, scroll up.
            let clip = h
                .painted_text
                .iter()
                .find(|text| rect.intersects(text.bounds))
                .map(|text| text.clip)
                .unwrap_or_else(|| design_safe_rect(h));
            let delta = if rect.bottom() > clip.bottom() + 1. {
                -30.
            } else if rect.top() < clip.top() - 1. {
                30.
            } else if rect.center().y > clip.center().y {
                -30.
            } else {
                30.
            };
            scroll_page(h, delta)?;
        } else {
            let down = last_center.map(|y| y > design_safe_rect(h).center().y).unwrap_or(true);
            scroll_page(h, if down { -30. } else { 30. })?;
        }
    }
    Ok(())
}

fn design_touch(h: &Harness, id: &str, label: Option<&str>) -> Result<()> {
    let rect = *h
        .app
        .hitboxes
        .get(id)
        .with_context(|| format!("design: missing {id}"))?;
    anyhow::ensure!(
        rect.width() >= 43.5 && rect.height() >= 43.5,
        "design: {id} touch target is only {rect:?}"
    );
    anyhow::ensure!(
        design_safe_rect(h).expand(0.5).contains_rect(rect),
        "design: {id} is outside the safe area: {rect:?}"
    );
    if let Some(label) = label {
        if !h.painted_text.iter().any(|text| {
            text.text == label
                && rect.expand(0.5).contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(rect)
        }) {
            let candidates: Vec<String> = h
                .painted_text
                .iter()
                .filter(|t| t.text.contains("阅读") || t.text == label || t.bounds.intersects(rect))
                .map(|t| format!("{:?}@{:?} clip{:?}", t.text, t.bounds, t.clip))
                .collect();
            anyhow::bail!(
                "design: {id} / {label} is clipped or detached from its touch target {rect:?}; candidates: {candidates:?}"
            );
        }
    }
    Ok(())
}

fn design_capture(
    h: &mut Harness,
    name: &str,
    captures: &mut Vec<serde_json::Value>,
) -> Result<()> {
    h.frame(vec![], Some(name))?;
    for id in [
        "nav-library",
        "nav-search",
        "nav-history",
        "nav-settings",
        "reader-back",
    ] {
        if h.app.hitboxes.contains_key(id) {
            design_touch(h, id, None)?;
        }
    }
    let coordinates = |rect: Rect| [rect.left(), rect.top(), rect.right(), rect.bottom()];
    captures.push(serde_json::json!({
        "image": format!("{name}.png"),
        "size": h.size,
        "screen": h.app.screen_name(),
        "safe_rect": coordinates(design_safe_rect(h)),
        "hitboxes": h.app.hitboxes.iter().map(|(id, rect)| (id.clone(), coordinates(*rect)))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "painted_text": h.painted_text.iter().map(|text| serde_json::json!({
            "text": text.text,
            "bounds": coordinates(text.bounds),
            "clip": coordinates(text.clip),
            "fully_visible": design_safe_rect(h).expand(0.5).contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds),
        })).collect::<Vec<_>>(),
        "painted_images": h.painted_images.iter().map(|image| serde_json::json!({
            "bounds": coordinates(image.bounds),
            "clip": coordinates(image.clip),
        })).collect::<Vec<_>>(),
    }));
    Ok(())
}

/// Asserts a dialog card painted its surface fully opaque in `png`:
/// a pixel inside the top-left padding must equal the light surface.
fn assert_opaque_card(h: &Harness, png: &str, hitbox: &str) -> Result<()> {
    let rect = h.app.hitboxes[hitbox];
    let shot = image::open(h.output.join(png))?.to_rgba8();
    let x = ((rect.left() + 12.) as u32).min(shot.width() - 1);
    let y = ((rect.top() + 12.) as u32).min(shot.height() - 1);
    let px = shot.get_pixel(x, y);
    let surface = [255u8, 253, 249];
    anyhow::ensure!(
        (0..3).all(|i| (px[i] as i32 - surface[i] as i32).abs() <= 8),
        "{png}: card is translucent at {x},{y}: {px:?} (expected {surface:?})"
    );
    Ok(())
}

fn design_read_clear_of_shelf(h: &Harness) -> Result<()> {
    let action = *h
        .app
        .hitboxes
        .get("shelf-read")
        .context("design: shelf read action is missing")?;
    let safe = design_safe_rect(h);
    for image in &h.painted_images {
        // On the shelf, cover meshes share its actual painter clip. This
        // catches a button covering the book stage even between two covers,
        // without reconstructing or importing the production layout formula.
        let stage = image.clip.intersect(safe);
        anyhow::ensure!(
            !action.intersect(stage).is_positive(),
            "design: reading action overlaps the visible shelf stage: action {action:?}, stage {stage:?}"
        );
    }
    Ok(())
}

fn design_reveal_read(h: &mut Harness) -> Result<()> {
    for attempt in 0..7 {
        design_read_clear_of_shelf(h)?;
        if design_touch(h, "shelf-read", Some("继续阅读")).is_ok()
            || design_touch(h, "shelf-read", Some("开始阅读")).is_ok()
        {
            return Ok(());
        }
        anyhow::ensure!(
            attempt < 6,
            "design: shelf reading action cannot be reached at 200 percent"
        );
        // Near the edge of the scroll column, outside the book's own drag
        // surface. This follows the same scroll path a finger/wheel uses.
        h.frame(
            vec![
                Event::PointerMoved(pos2(h.size[0] as f32 - 18., h.size[1] as f32 * 0.67)),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -180.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(6)?;
    }
    unreachable!()
}

/// A small visual review set with real library, pointer and WASM behavior.
/// This supplements the existing reader and iPad suites; it is not a device
/// frame-rate, UIKit or file-picker test.
fn design_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    struct ResetInsets;
    impl Drop for ResetInsets {
        fn drop(&mut self) {
            set_insets_override(None);
        }
    }
    let _reset = ResetInsets;
    let mut captures = Vec::new();
    set_insets_override(Some(Insets {
        left: 0,
        top: 24,
        right: 0,
        bottom: 20,
    }));
    h.size = [390, 844];
    h.frames(6)?;
    design_touch(h, "shelf-read", Some("开始阅读"))?;
    design_capture(h, "design-phone-shelf", &mut captures)?;
    let initial_title = h
        .painted_text
        .iter()
        .find(|text| text.text == "月光旅人")
        .context("design: initial selected book title is missing")?;
    anyhow::ensure!(
        initial_title.bounds.height() > 20.,
        "design: expected selected title above the thumbnails"
    );
    // The thumbnail strip starts collapsed: a taller stage and no targets.
    anyhow::ensure!(
        !h.app
            .hitboxes
            .keys()
            .any(|id| id.starts_with("shelf-thumbnail-")),
        "design: thumbnail strip should start collapsed"
    );
    let collapsed_stage = h.app.shelf_stage_height();
    h.click("shelf-grid-toggle")?;
    h.frames(6)?;
    anyhow::ensure!(
        h.app.shelf_stage_height() < collapsed_stage,
        "design: collapsed stage {} did not exceed expanded {}",
        collapsed_stage,
        h.app.shelf_stage_height()
    );
    // On narrow widths the expanded strip sits below the fold now that the
    // header has a second row; scroll the library column down to reach it.
    for _ in 0..6 {
        if h
            .app
            .hitboxes
            .keys()
            .any(|id| id.starts_with("shelf-thumbnail-"))
        {
            break;
        }
        h.frame(
            vec![
                Event::PointerMoved(pos2(h.size[0] as f32 - 18., h.size[1] as f32 * 0.67)),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -180.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(4)?;
    }
    design_capture(h, "design-phone-shelf-expanded", &mut captures)?;
    let thumbnail = h
        .app
        .hitboxes
        .iter()
        .filter(|(id, _)| id.starts_with("shelf-thumbnail-"))
        .min_by(|(_, a), (_, b)| a.left().total_cmp(&b.left()))
        .map(|(id, _)| id.clone())
        .context("design: visible thumbnail tap target is missing")?;
    // Move away, then select the first actual thumbnail with pointer events.
    h.click("shelf-next")?;
    h.frames(40)?;
    design_touch(h, &thumbnail, None)?;
    h.click(&thumbnail)?;
    h.frames(40)?;
    anyhow::ensure!(
        h.painted_text.iter().any(|text| text.text == "月光旅人"
            && text.bounds.height() > 20.
            && text.bounds.top() < 550.),
        "design: thumbnail tap did not select its book in the reading stage"
    );
    h.click("shelf-read")?;
    h.wait(ReaderApp::page_loaded)?;
    let expected_id = thumbnail.trim_start_matches("shelf-thumbnail-");
    anyhow::ensure!(
        h.app.current_book_id() == Some(expected_id),
        "design: reading after thumbnail selection opened a different book"
    );
    h.frame(vec![], None)?;
    h.click("reader-back")?;
    h.frames(5)?;
    h.click("shelf-next")?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    anyhow::ensure!(
        h.app.rendered_book_nodes < 45,
        "design: unbounded shelf rendering"
    );
    // Collapse again so the remaining captures show the default shelf.
    h.click("shelf-grid-toggle")?;
    h.frames(6)?;

    // The sort menu and the book-swap animation on the collapsed shelf.
    h.click("shelf-sort")?;
    h.frames(4)?;
    design_capture(h, "shelf-sort-menu", &mut captures)?;
    for id in ["shelf-sort-recent", "shelf-sort-progress", "shelf-sort-added"] {
        anyhow::ensure!(
            h.app.hitboxes.contains_key(id),
            "design: sort menu row {id} is missing"
        );
    }
    // The popover must be an opaque card: nothing on the shelf may show
    // through it. Sample a pixel in the panel padding above the first row.
    {
        let row = h.app.hitboxes["shelf-sort-progress"];
        let shot = image::open(h.output.join("shelf-sort-menu.png"))?.to_rgba8();
        let x = (row.center().x as u32).min(shot.width() - 1);
        let y = ((row.top() - 10.).max(0.) as u32).min(shot.height() - 1);
        let px = shot.get_pixel(x, y);
        let surface = [255u8, 253, 249];
        anyhow::ensure!(
            (0..3).all(|i| (px[i] as i32 - surface[i] as i32).abs() <= 8),
            "design: sort popover is translucent at {x},{y}: {px:?} (expected {surface:?})"
        );
    }
    for (first, row, name) in [
        (true, "shelf-sort-added", "added"),
        (false, "shelf-sort-progress", "progress"),
        (false, "shelf-sort-recent", "recent"),
    ] {
        if !first {
            h.click("shelf-sort")?;
            h.frames(4)?;
        }
        h.click(row)?;
        if name == "added" {
            h.frames(16)?;
            design_capture(h, "shelf-sort-moving", &mut captures)?;
            h.frames(30)?;
        } else {
            h.frames(46)?;
        }
        // The tap landed on the popover row, not a shelf book beneath it.
        anyhow::ensure!(
            h.app.shelf_sort_key() == name,
            "design: selecting {name} from the popover did not change the sort"
        );
        design_capture(h, &format!("shelf-sort-{name}"), &mut captures)?;
        anyhow::ensure!(
            h.app.focus_index() == 0,
            "design: focus should land on the first book of the {name} order"
        );
        let expected = h.app.focused_book_title();
        anyhow::ensure!(
            h.painted_text
                .iter()
                .any(|text| text.text == expected && text.bounds.height() > 20.),
            "design: central book after {name} sorting is not the first of the new order ({expected})"
        );
    }
    // Sorting lands on the first book, which is the one read above; move on
    // so the next stage shows an unread book's 开始阅读 again.
    h.click("shelf-next")?;
    h.frames(40)?;

    h.size = [834, 1194];
    h.frames(6)?;
    design_touch(h, "shelf-read", Some("开始阅读"))?;
    design_capture(h, "design-ipad-portrait-shelf", &mut captures)?;
    h.size = [1194, 834];
    h.frames(6)?;
    design_touch(h, "shelf-read", Some("开始阅读"))?;
    design_capture(h, "design-ipad-landscape-shelf", &mut captures)?;
    h.click("open-book")?;
    h.frames(65)?;
    anyhow::ensure!(
        h.app.screen_name() == "detail",
        "design: cover did not open details"
    );
    design_touch(h, "read", None)?;
    design_capture(h, "design-detail", &mut captures)?;
    h.click("read")?;
    h.wait(ReaderApp::page_loaded)?;
    // A cached page can be ready during the click's release frame, before
    // the new screen has painted its controls. Render the resulting screen
    // once; missing reader controls still fail the following real click.
    h.frame(vec![], None)?;
    h.click("reader-next")?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_page() == 1,
        "design: next page did not advance"
    );
    let read_id = h
        .app
        .current_book_id()
        .context("design: reader has no book")?
        .to_owned();
    design_capture(h, "design-reader", &mut captures)?;
    h.click("reader-back")?;
    h.wait(ReaderApp::position_saved)?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.screen_name() == "detail",
        "design: reader lost its entry point"
    );
    h.click("back")?;
    h.frames(60)?;
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "design: details did not return"
    );
    // Scrollbar hover fades can start when the cover flight gives control
    // back to the scroll column. They must settle within this bounded tail.
    for _ in 0..30 {
        if !h.app.frame_is_animating && h.repaint_delay >= Duration::from_secs(1) {
            break;
        }
        h.frame(vec![], None)?;
    }
    anyhow::ensure!(
        !h.app.frame_is_animating && h.repaint_delay >= Duration::from_secs(1),
        "design: settled shelf is repainting continuously: {:?}, animating={}, causes={:?}",
        h.repaint_delay,
        h.app.frame_is_animating,
        h.ctx.repaint_causes()
    );
    let idle_delay = h.repaint_delay;
    let library = yejian_core::Library::open(library_root)?;
    let persisted = library.get_book(&read_id)?;
    anyhow::ensure!(
        persisted.current_page == 1 && persisted.last_read_at.is_some(),
        "design: reading progress/history did not persist"
    );

    h.size = [834, 1194];
    h.frames(5)?;
    h.click("nav-history")?;
    h.frames(4)?;
    let resume = format!("history-{read_id}");
    // The stats card sits on top now; scroll until the resume row's
    // touch target is fully inside its scroll clip before tapping it
    // (the same condition design_touch asserts).
    scroll_until_row_visible(h, &resume, "继续")?;
    design_touch(h, &resume, None)?;
    design_capture(h, "design-history", &mut captures)?;
    h.click(&resume)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(read_id.as_str()) && h.app.current_page() == 1,
        "design: history resumed the wrong book/page"
    );
    h.click("reader-back")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.screen_name() == "library" && h.app.hitboxes.contains_key(&resume),
        "design: history reading did not return directly to its history entry"
    );
    h.click("nav-settings")?;
    h.frames(5)?;
    for label in ["跟随系统", "浅色", "深色"] {
        anyhow::ensure!(
            design_label_visible(h, label),
            "design: theme choice {label} is clipped"
        );
    }
    design_capture(h, "design-settings", &mut captures)?;
    // Round 3: the 存储 group reports managed bytes with confirmed cleanups.
    h.wait(ReaderApp::storage_ready)?;
    // Sweep the settings scroll both ways in steps smaller than the
    // viewport so every row passes fully through the visible window.
    for _ in 0..10 {
        if design_label_visible(h, "书架漫画") && design_label_visible(h, "封面缓存") {
            break;
        }
        scroll_page(h, -400.)?;
    }
    for _ in 0..10 {
        if design_label_visible(h, "书架漫画") && design_label_visible(h, "封面缓存") {
            break;
        }
        scroll_page(h, 400.)?;
    }
    anyhow::ensure!(
        design_label_visible(h, "书架漫画") && design_label_visible(h, "封面缓存"),
        "design: 存储 group rows are missing"
    );
    design_capture(h, "settings-storage", &mut captures)?;
    // Every cleanup asks first; the confirm card is fully opaque.
    h.click("storage-removed-clean")?;
    h.frames(4)?;
    design_capture(h, "settings-storage-confirm", &mut captures)?;
    assert_opaque_card(h, "settings-storage-confirm.png", "storage-confirm-card")?;
    h.click_text("取消")?;
    h.frames(3)?;
    scroll_page(h, 1400.)?;
    h.frames(3)?;
    h.click_text("深色")?;
    h.frames(5)?;
    h.click("nav-library")?;
    h.size = [390, 844];
    h.frames(8)?;
    design_capture(h, "design-dark-phone", &mut captures)?;
    h.click("nav-settings")?;
    h.frames(5)?;
    h.click_text("浅色")?;
    h.frames(5)?;

    // Seed the existing explicitly-labelled WASM source, then enter it via
    // the production "my sources" screen before changing a real filter.
    h.size = [1194, 834];
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    h.frames(5)?;
    h.click_text("‹ 我的书源")?;
    h.frames(5)?;
    anyhow::ensure!(
        h.texts.iter().any(|(text, _)| text.contains("筛选验收源")),
        "design: installed source is missing from my sources"
    );
    design_capture(h, "design-sources", &mut captures)?;
    let source_button = h
        .texts
        .iter()
        .find(|(text, _)| text.contains("筛选验收源"))
        .context("design: source button is missing")?
        .0
        .clone();
    h.click_text(&source_button)?;
    h.wait(ReaderApp::source_browser_ready)?;
    h.frames(5)?;
    h.click_text("筛选与排序")?;
    h.frames(5)?;
    for label in ["应用筛选", "重置", "取消"] {
        anyhow::ensure!(design_label_visible(h, label), "design: {label} is clipped");
    }
    design_capture(h, "design-filters", &mut captures)?;
    h.click_text("冒险")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "筛选:冒险题材")?;
    h.frames(5)?;
    design_capture(h, "design-filtered-results", &mut captures)?;

    // Round 2: one working fixture source and one corrupt fixture source
    // produce a success group and a failure group in installed order.
    h.app.prepare_global_search_acceptance()?;
    h.wait(ReaderApp::global_search_done)?;
    h.frames(5)?;
    {
        let groups = h.app.source_browser_report()["groups"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        anyhow::ensure!(
            groups.iter().any(|group| {
                group["source"] == "local.filter-acceptance"
                    && group["state"]
                        .as_str()
                        .is_some_and(|state| state.starts_with("done:"))
            }),
            "global: the filter fixture group did not finish with results"
        );
        anyhow::ensure!(
            groups.iter().any(|group| {
                group["source"] == "local.broken-acceptance"
                    && group["state"]
                        .as_str()
                        .is_some_and(|state| state.starts_with("failed:"))
            }),
            "global: the broken fixture group did not report its failure"
        );
    }
    design_capture(h, "design-global-search", &mut captures)?;
    // The scope control and search action stay fully on screen.
    let screen =
        Rect::from_min_size(Pos2::ZERO, vec2(h.size[0] as f32, h.size[1] as f32));
    for id in ["scope-current", "scope-global", "source-search-go"] {
        anyhow::ensure!(
            screen.contains_rect(h.app.hitboxes[id]),
            "global: {id} {:?} overflows {screen:?}",
            h.app.hitboxes[id]
        );
    }

    // Round A: search history chips sit under the empty field; clearing
    // asks through an opaque card.
    h.app.seed_search_history(vec!["星海".into(), "猎人".into()]);
    h.frames(4)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("history-0")
            && h.app.hitboxes.contains_key("history-clear"),
        "history: keyword chips did not render under the empty search field"
    );
    design_capture(h, "search-history", &mut captures)?;
    h.click("history-clear")?;
    h.frames(4)?;
    design_capture(h, "search-history-confirm", &mut captures)?;
    assert_opaque_card(h, "search-history-confirm.png", "history-clear-card")?;
    h.click("history-clear-go")?;
    h.frames(3)?;
    anyhow::ensure!(
        !h.app.hitboxes.contains_key("history-0"),
        "history: 清空 left keyword chips behind"
    );

    // Round A: the shelf header is a scrollable tab strip ending in 管理.
    h.click("nav-library")?;
    h.frames(5)?;
    h.app.seed_categories(vec!["热血".into(), "悬疑".into()]);
    h.frames(4)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("category-tabs-manage")
            && h.app.hitboxes.keys().any(|id| id.starts_with("category-tab-0")),
        "category: the shelf tab strip did not render"
    );
    design_capture(h, "shelf-category-tabs", &mut captures)?;
    h.click("category-tabs-manage")?;
    h.frames(4)?;
    design_capture(h, "category-manage", &mut captures)?;
    assert_opaque_card(h, "category-manage.png", "category-manage-card")?;
    // Every row shows all four slots; only the enabled moves register
    // hitboxes — first row can only move down, last only up.
    anyhow::ensure!(
        h.app.hitboxes.contains_key("category-down-0")
            && !h.app.hitboxes.contains_key("category-up-0"),
        "category: first row must offer 下移 only"
    );
    anyhow::ensure!(
        h.app.hitboxes.contains_key("category-up-1")
            && !h.app.hitboxes.contains_key("category-down-1"),
        "category: last row must offer 上移 only"
    );
    h.click("category-down-0")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.category_order() == ["悬疑", "热血"],
        "category: 下移 did not reorder {:?}",
        h.app.category_order()
    );
    h.click("category-up-1")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.category_order() == ["热血", "悬疑"],
        "category: 上移 did not restore {:?}",
        h.app.category_order()
    );
    h.click_text("关闭")?;
    h.frames(3)?;
    let book_id = h
        .app
        .first_book_id()
        .context("category: pick needs a library book")?;
    h.app.pick_categories(vec![book_id]);
    h.frames(4)?;
    design_capture(h, "category-pick", &mut captures)?;
    assert_opaque_card(h, "category-pick.png", "category-pick-card")?;
    h.click_text("取消")?;
    h.frames(3)?;

    // Round B: the 追更 settings group exposes the auto-download mode with
    // its mobile-data note.
    h.click("nav-settings")?;
    h.frames(4)?;
    // Sweep the full settings page in small steps (each scroll_page call is
    // six wheel events, so large deltas jump past whole sections).
    for _ in 0..24 {
        if design_label_visible(h, "自动下载新章节（会使用移动网络）") {
            break;
        }
        scroll_page(h, -120.)?;
    }
    for _ in 0..24 {
        if design_label_visible(h, "自动下载新章节（会使用移动网络）") {
            break;
        }
        scroll_page(h, 120.)?;
    }
    anyhow::ensure!(
        design_label_visible(h, "自动下载新章节（会使用移动网络）"),
        "roundB: 自动下载新章节 row is missing from settings"
    );
    design_capture(h, "settings-auto-download", &mut captures)?;

    // Round B: a followed work with queued auto-downloads reports
    // "自动下载中 N 章" in 我的追更.
    h.app.prepare_auto_follow_acceptance();
    h.frames(4)?;
    anyhow::ensure!(
        h.painted_text
            .iter()
            .any(|text| text.text.contains("自动下载中 2 章")),
        "roundB: 自动下载中 row is missing from 我的追更"
    );
    design_capture(h, "follows-auto-downloading", &mut captures)?;

    // Round B: the 换源 panel — grouped candidates, one expanded with its
    // chapter count, the opaque confirm card, then the new detail page.
    h.app.prepare_migrate_acceptance("candidates");
    h.frames(4)?;
    anyhow::ensure!(
        h.painted_text
            .iter()
            .any(|text| text.text.contains("共 3 话")),
        "roundB: candidate expansion did not render"
    );
    design_capture(h, "migrate-candidates", &mut captures)?;
    assert_opaque_card(h, "migrate-candidates.png", "migrate-card")?;
    h.app.prepare_migrate_acceptance("confirm");
    h.frames(4)?;
    anyhow::ensure!(
        design_label_visible(h, "同时删除旧书源已下载的章节"),
        "roundB: migrate confirm did not render"
    );
    design_capture(h, "migrate-confirm", &mut captures)?;
    assert_opaque_card(h, "migrate-confirm.png", "migrate-card")?;
    h.app.prepare_migrate_acceptance("done");
    h.frames(5)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("source-detail-read"),
        "roundB: migration did not land on the new source's detail page"
    );
    design_capture(h, "migrate-done", &mut captures)?;

    // Round D: 阅读足迹 — grouped local+online rows under the stats card,
    // then 书源管理 — update badge, 测试 result, opaque uninstall confirm
    // and the repository language/name filter.
    h.app.prepare_history_acceptance();
    h.frames(6)?;
    // The ScrollArea offset persists in egui memory across screens;
    // return to the top so the stats card is in view.
    for _ in 0..24 {
        if design_label_visible(h, "阅读统计") {
            break;
        }
        scroll_page(h, 300.)?;
    }
    anyhow::ensure!(
        design_label_visible(h, "阅读统计")
            && design_label_visible(h, "连续阅读")
            && design_label_visible(h, "本月读完")
            && design_label_visible(h, "今天"),
        "roundD: the stats card did not render: {:?}",
        h.texts.iter().map(|(text, _)| text).collect::<Vec<_>>()
    );
    anyhow::ensure!(
        h.app.hitboxes.contains_key("history-search")
            && h.app.hitboxes.contains_key("history-clear"),
        "roundD: history search/clear controls are missing"
    );
    design_capture(h, "history-stats", &mut captures)?;
    // The group sections beyond 今天 sit below the first viewport; scroll
    // the page until 昨天/更早 render.
    for _ in 0..24 {
        if design_label_visible(h, "更早") {
            break;
        }
        scroll_page(h, -300.)?;
    }
    design_capture(h, "history-grouped", &mut captures)?;
    anyhow::ensure!(
        design_label_visible(h, "更早")
            && (design_label_visible(h, "昨天") || design_label_visible(h, "本周")),
        "roundD: history groups did not render: {:?}",
        h.texts.iter().map(|(text, _)| text).collect::<Vec<_>>()
    );

    h.app.prepare_sources_manage_acceptance();
    h.frames(6)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("source-uninstall-local.manage-a")
            && h.app.hitboxes.contains_key("source-test-local.manage-b")
            && h.app.hitboxes.contains_key("source-update-local.manage-a"),
        "roundD: 我的书源 action slots are missing"
    );
    design_capture(h, "sources-manage", &mut captures)?;
    // The manage-a/manage-b rows sit below the fold; scroll until both the
    // update badge and the finished 测试 result are painted.
    for _ in 0..24 {
        let badge = h
            .texts
            .iter()
            .any(|(text, _)| text.contains("有更新 v3"));
        let result = h
            .texts
            .iter()
            .any(|(text, _)| text.contains("可用 · 820 ms"));
        if badge && result {
            break;
        }
        scroll_page(h, -200.)?;
    }
    anyhow::ensure!(
        h.texts
            .iter()
            .any(|(text, _)| text.contains("可用 · 820 ms")),
        "roundD: the 测试 result did not render"
    );
    anyhow::ensure!(
        h.texts
            .iter()
            .any(|(text, _)| text.contains("有更新 v3")),
        "roundD: the 有更新 badge did not render"
    );
    design_capture(h, "sources-test", &mut captures)?;

    // Scroll past 我的书源 to the repository list for the filter capture.
    for _ in 0..24 {
        if h.app.hitboxes.contains_key("repo-lang-中文") {
            break;
        }
        scroll_page(h, -160.)?;
    }
    anyhow::ensure!(
        h.app.hitboxes.contains_key("repo-lang-中文"),
        "roundD: repository language chips are missing"
    );
    h.click("repo-lang-中文")?;
    h.frames(4)?;
    anyhow::ensure!(
        !design_label_visible(h, "さくら文庫")
            && design_label_visible(h, "纸间图源 · 中文"),
        "roundD: the 中文 language chip did not filter the repository list"
    );
    design_capture(h, "repo-filter", &mut captures)?;

    h.app.open_uninstall_acceptance();
    h.frames(4)?;
    anyhow::ensure!(
        design_label_visible(h, "已下载的章节保留；这个书源上的追更会停止更新，并标为「书源已卸载」。"),
        "roundD: the uninstall warning did not render"
    );
    design_capture(h, "source-uninstall-confirm", &mut captures)?;
    assert_opaque_card(h, "source-uninstall-confirm.png", "source-uninstall-card")?;
    h.click_text("取消")?;
    h.frames(3)?;

    // Round B: Bangumi login + binding against a local fake API, then the
    // 进度跟踪 settings row shows the account.
    let (bangumi_base, _bangumi_server) = fake_bangumi();
    h.app.set_bangumi_endpoint(h.ctx.clone(), bangumi_base);
    let key = "source:local.filter-acceptance\u{1f}fx-pop-1".to_string();
    h.app.tracker_command(yejian::trackers::TrackCommand::BangumiToken(
        "acceptance-token".into(),
    ));
    let report_key = key.clone();
    h.wait(move |app| {
        app.tracker_report(&report_key)["bangumi"].is_string()
    })?;
    h.app.tracker_command(yejian::trackers::TrackCommand::Bind {
        tracker: yejian::trackers::TrackerKind::Bangumi,
        key: key.clone(),
        pick: yejian::trackers::TrackSearch {
            media_id: "99".into(),
            title: "作品A".into(),
            cover: String::new(),
            total_chapters: 120,
            status: String::new(),
        },
        progress: 0,
    });
    h.wait(move |app| app.tracker_report(&key)["bound"] == true)?;
    h.click("nav-settings")?;
    h.frames(4)?;
    let bangumi_row = |h: &Harness| {
        let safe = design_safe_rect(h).expand(0.5);
        h.painted_text.iter().any(|text| {
            text.text.starts_with("Bangumi ·")
                && safe.contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds)
        })
    };
    for _ in 0..24 {
        if bangumi_row(h) {
            break;
        }
        scroll_page(h, -120.)?;
    }
    for _ in 0..24 {
        if bangumi_row(h) {
            break;
        }
        scroll_page(h, 120.)?;
    }
    anyhow::ensure!(
        bangumi_row(h),
        "roundB: the Bangumi tracker row is missing from settings"
    );
    design_capture(h, "tracker-bangumi", &mut captures)?;

    // Round C: app lock — desktop fakes the platform support + auth, then
    // the privacy toggle, delay picker and opaque lock screen are exercised.
    h.app.set_app_lock_fake(true, Some(Ok(())));
    let lock_row = |h: &Harness| {
        let safe = design_safe_rect(h).expand(0.5);
        h.painted_text.iter().any(|text| {
            text.text.contains("应用锁")
                && safe.contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds)
        })
    };
    for _ in 0..24 {
        if lock_row(h) {
            break;
        }
        scroll_page(h, -120.)?;
    }
    for _ in 0..24 {
        if lock_row(h) {
            break;
        }
        scroll_page(h, 120.)?;
    }
    anyhow::ensure!(lock_row(h), "roundC: the 隐私 app-lock row is missing");
    h.click_text("应用锁（面容 ID / 触控 ID / 设备密码）")?;
    h.wait(|app| app.app_lock_enabled())?;
    h.frames(3)?;
    anyhow::ensure!(
        lock_row(h),
        "roundC: the enabled app-lock row is clipped at this layout"
    );
    design_capture(h, "app-lock-settings", &mut captures)?;

    // A denied auto-prompt keeps the screen locked with the retry note.
    h.app.set_app_lock_fake(true, Some(Err("denied".into())));
    h.app.debug_lock();
    h.frames(4)?;
    anyhow::ensure!(h.app.app_lock_locked(), "roundC: debug lock did not engage");
    design_capture(h, "app-lock", &mut captures)?;
    // The whole screen is the lock card: a pixel well inside it must be the
    // opaque paper surface, not whatever the shelf painted underneath.
    {
        let shot = image::open(h.output.join("app-lock.png"))?.to_rgba8();
        let px = shot.get_pixel(shot.width() / 5, shot.height() / 5);
        let surface = [255u8, 253, 249];
        anyhow::ensure!(
            (0..3).all(|i| (px[i] as i32 - surface[i] as i32).abs() <= 8),
            "app-lock.png: lock screen is translucent: {px:?}"
        );
    }
    // While locked, a tap that would land on a shelf hitbox must go nowhere
    // (the nav row sits beneath the lock screen at the same coordinates).
    let nav_pos = h
        .app
        .hitboxes
        .get("nav-library")
        .context("roundC: nav hitbox missing under the lock")?
        .center();
    h.click_pos(nav_pos)?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.app_lock_locked() && h.app.screen_name() == "library",
        "roundC: a tap reached through the lock screen"
    );
    h.app.set_app_lock_fake(true, Some(Ok(())));
    h.click("app-lock-unlock")?;
    h.wait(|app| !app.app_lock_locked())?;
    h.frames(3)?;
    h.click("nav-library")?;
    h.frames(3)?;

    // Round D3: 新章节通知 — desktop fakes a granted permission, the 追更
    // settings toggle is switched on and the enabled row is captured.
    yejian_platform::set_notification_fake(Some(true));
    h.click("nav-settings")?;
    h.frames(4)?;
    let notify_row = |h: &Harness| {
        let safe = design_safe_rect(h).expand(0.5);
        h.painted_text.iter().any(|text| {
            text.text.contains("新章节通知")
                && safe.contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds)
        })
    };
    for _ in 0..24 {
        if notify_row(h) {
            break;
        }
        scroll_page(h, -120.)?;
    }
    for _ in 0..24 {
        if notify_row(h) {
            break;
        }
        scroll_page(h, 120.)?;
    }
    anyhow::ensure!(notify_row(h), "roundD3: the 新章节通知 row is missing");
    h.click_text("新章节通知")?;
    h.wait(|app| app.notify_updates_enabled())?;
    h.frames(3)?;
    design_capture(h, "settings-notify", &mut captures)?;

    // Round D4: 云同步（WebDAV）— seed a remote snapshot written by
    // "device-b" on a fake server, run a real two-root merge, then capture
    // the settings section with the 上次同步 status line.
    let (dav_url, dav_files, _dav) = fake_webdav();
    {
        let remote = serde_json::json!({
            "stats": {
                "device-b\u{1f}2001-01-01": {
                    "updated_at": 1,
                    "value": {"seconds": 5, "pages": 2, "chapters": 0}
                }
            }
        });
        dav_files.lock().unwrap().insert(
            "/yejian/yejian-sync.json".into(),
            (serde_json::to_vec(&remote)?, "\"1\"".into()),
        );
    }
    h.app.prepare_webdav_acceptance(dav_url);
    h.app.cloud_sync_once();
    h.wait(|app| app.cloud_synced())?;
    // The remote "device-b" day must have merged into the local stats
    // store — proof the two-root merge ran and was applied by owners.
    h.wait(|app| app.stats_day_seconds("2001-01-01", "device-b") == Some(5))?;
    let webdav_row = |h: &Harness| {
        let safe = design_safe_rect(h).expand(0.5);
        h.painted_text.iter().any(|text| {
            text.text.contains("云同步")
                && safe.contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds)
        })
    };
    for _ in 0..24 {
        if webdav_row(h) {
            break;
        }
        scroll_page(h, -120.)?;
    }
    for _ in 0..24 {
        if webdav_row(h) {
            break;
        }
        scroll_page(h, 120.)?;
    }
    anyhow::ensure!(webdav_row(h), "roundD4: the 云同步 section is missing");
    design_capture(h, "settings-webdav", &mut captures)?;
    let synced_row = |h: &Harness| {
        let safe = design_safe_rect(h).expand(0.5);
        h.painted_text.iter().any(|text| {
            text.text.contains("上次同步")
                && safe.contains_rect(text.bounds)
                && text.clip.expand(0.5).contains_rect(text.bounds)
        })
    };
    for _ in 0..24 {
        if synced_row(h) {
            break;
        }
        scroll_page(h, -120.)?;
    }
    anyhow::ensure!(synced_row(h), "roundD4: the 上次同步 status did not render");
    design_capture(h, "webdav-synced", &mut captures)?;

    // E3: 关于 → 导出诊断日志 produces a share-staged zip with no secrets.
    scroll_until_row_visible(h, "diagnostics-export", "导出诊断日志")?;
    design_capture(h, "diagnostics-export", &mut captures)?;
    h.click("diagnostics-export")?;
    h.frames(3)?;
    h.wait(|app| {
        app.last_shared()
            .map(|p| p.extension().is_some_and(|e| e == "zip"))
            .unwrap_or(false)
    })?;
    let staged = h.app.last_shared().unwrap().to_path_buf();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&staged)?)?;
    let entries: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    anyhow::ensure!(
        entries.iter().any(|n| n == "device.txt") && entries.iter().any(|n| n == "summary.txt"),
        "diagnostics zip lacks device/summary: {entries:?}"
    );
    anyhow::ensure!(
        !entries.iter().any(|n| n.ends_with(".json")),
        "diagnostics zip ships a raw state file: {entries:?}"
    );
    for secret in ["trackers.json", "webdav.json", "web-cookies.json"] {
        anyhow::ensure!(
            !entries.iter().any(|n| n.contains(secret)),
            "diagnostics zip leaks {secret}: {entries:?}"
        );
    }

    // E2: the 性能信息 overlay paints decode/frame/texture counters.
    scroll_until_row_visible(h, "diagnostics-export", "导出诊断日志")?;
    h.click_text("性能信息")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.painted_text.iter().any(|t| t.text.contains("ms") || t.text.contains("纹理") || t.text.contains("帧")),
        "perf-overlay: overlay counters not painted"
    );
    design_capture(h, "perf-overlay", &mut captures)?;
    h.click_text("性能信息")?;
    h.frames(2)?;

    // E3 notices: crash / oom exit assessment and a supervised restart.
    h.app.fake_exit_notice(true);
    h.frames(4)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("crash-export"),
        "crash-notice: 导出日志 affordance missing"
    );
    design_capture(h, "crash-notice", &mut captures)?;
    h.app.clear_notice();
    h.app.fake_exit_notice(false);
    h.frames(4)?;
    design_capture(h, "oom-notice", &mut captures)?;
    h.app.clear_notice();
    h.app.fake_worker_recovery();
    h.frames(4)?;
    anyhow::ensure!(
        h.painted_text.iter().any(|t| t.text.contains("已自动恢复")),
        "worker-recovered: recovery toast not painted"
    );
    design_capture(h, "worker-recovered", &mut captures)?;
    h.app.clear_notice();

    // Reopen the real saved settings, preserving the source and reading
    // history. Only the temporary acceptance library is changed. Preview
    // follows/tasks must not be persisted into the real library on exit.
    h.app.clear_acceptance_previews();
    eframe::App::on_exit(&mut h.app);
    library.set_setting("font-scale", "2")?;
    library.set_setting("theme", "light")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.size = [1194, 834];
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    // Keep the initial frame on disk even if the overlap assertion rejects
    // it. Scrolling may reveal an action, but must not conceal an overlay
    // already present on the initial 200 percent landscape layout.
    h.frame(vec![], Some("design-large-text-landscape-shelf"))?;
    anyhow::ensure!(
        !h.painted_images.is_empty(),
        "design: landscape shelf has no rendered covers"
    );
    design_read_clear_of_shelf(h)?;
    design_reveal_read(h)?;
    design_capture(h, "design-large-text-landscape-shelf", &mut captures)?;
    h.click("shelf-read")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    h.click("reader-back")?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "design: landscape large-text direct reading lost its shelf return"
    );

    h.size = [320, 1024];
    h.frames(8)?;
    design_reveal_read(h)?;
    design_capture(h, "design-large-text-shelf", &mut captures)?;
    for attempt in 0..5 {
        h.click("shelf-read")?;
        h.frames(4)?;
        if h.app.screen_name() == "reader" {
            break;
        }
        anyhow::ensure!(attempt < 4, "design: 320pt shelf-read click did not open");
    }
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    h.click("reader-back")?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "design: direct reading lost its shelf return"
    );
    h.app.prepare_history_acceptance();
    h.click("nav-history")?;
    h.frames(5)?;
    // The ScrollArea offset persists in egui memory across the recreated
    // app; return to the top so the stats card and rows are visible.
    for _ in 0..24 {
        if design_label_visible(h, "阅读统计") {
            break;
        }
        scroll_page(h, 300.)?;
    }
    design_capture(h, "design-large-text-history", &mut captures)?;
    design_capture(h, "history-stats-320", &mut captures)?;
    anyhow::ensure!(
        design_label_visible(h, "本地和网络，最近读过的故事都在这里。")
            && design_label_visible(h, "阅读统计"),
        "design: 320pt/200 percent history introduction is clipped"
    );
    // Covers in the groups below must never overlap the wrapped
    // introduction (checked here while the top of the page is visible).
    let introduction = h
        .painted_text
        .iter()
        .find(|text| text.text == "本地和网络，最近读过的故事都在这里。")
        .context("design: history introduction is missing")?;
    let visible_covers: Vec<_> = h
        .painted_images
        .iter()
        .map(|image| image.bounds.intersect(image.clip))
        .filter(|rect| rect.is_positive())
        .collect();
    // With the stats card on top there may be no cover in the first
    // viewport; every cover that is painted must still sit below the intro.
    anyhow::ensure!(
        visible_covers
            .iter()
            .all(|rect| rect.top() >= introduction.bounds.bottom()),
        "design: large-text history rows overlap the wrapped introduction {:?} covers {:?}",
        introduction.bounds,
        visible_covers
    );
    // The resume row can sit anywhere below the stats card; scroll
    // adaptively until its touch target is fully inside the safe area.

    // scroll_page issues six wheel events per call — keep the delta small
    // or the row overshoots the safe area in both directions.
    scroll_until_row_visible(h, &resume, "继续")?;
    design_touch(h, &resume, Some("继续"))?;
    // 移除 is a real quiet button with a ≥44pt hit area, not a text link.
    let hide = h
        .app
        .hitboxes
        .keys()
        .find(|key| {
            key.starts_with("history-hide-")
                && h.app.hitboxes[*key].intersects(h.app.hitboxes[&resume])
        })
        .cloned()
        .context("design: 移除 button is missing on the resume row")?;
    design_touch(h, &hide, None)?;
    h.click(&resume)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(read_id.as_str()) && h.app.current_page() == 1,
        "design: 320pt/200 percent history resumed the wrong book/page"
    );
    h.click("reader-back")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.screen_name() == "library" && h.app.hitboxes.contains_key(&resume),
        "design: large-text history reading did not return directly to history"
    );
    h.click("nav-settings")?;
    h.frames(5)?;
    // The settings ScrollArea offset persists in egui memory, so make sure
    // the page is back at the top before asserting the theme row.
    for _ in 0..24 {
        if design_label_visible(h, "跟随系统")
            && design_label_visible(h, "浅色")
            && design_label_visible(h, "深色")
        {
            break;
        }
        scroll_page(h, 120.)?;
    }
    for label in ["跟随系统", "浅色", "深色"] {
        anyhow::ensure!(
            design_label_visible(h, label),
            "design: 200 percent theme choice {label} is clipped"
        );
    }
    design_capture(h, "design-large-text-settings", &mut captures)?;
    h.app.prepare_source_acceptance("filters")?;
    h.wait(|app| {
        app.source_browser_report()["filter_editor"] == true && app.source_browser_ready()
    })?;
    h.frames(5)?;
    for label in ["应用筛选", "重置", "取消"] {
        anyhow::ensure!(
            design_label_visible(h, label),
            "design: 200 percent filter action {label} is clipped"
        );
    }
    design_capture(h, "design-large-text-filters", &mut captures)?;
    h.click_text("取消")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.source_browser_report()["filter_editor"] == false,
        "design: large-text filter cancel did not leave the form"
    );
    h.app.prepare_global_search_acceptance()?;
    h.wait(ReaderApp::global_search_done)?;
    h.frames(5)?;
    design_capture(h, "design-large-text-global-search", &mut captures)?;
    let screen =
        Rect::from_min_size(Pos2::ZERO, vec2(h.size[0] as f32, h.size[1] as f32));
    for id in ["scope-current", "scope-global", "source-search-go"] {
        anyhow::ensure!(
            screen.contains_rect(h.app.hitboxes[id]),
            "global 200 percent: {id} {:?} overflows {screen:?}",
            h.app.hitboxes[id]
        );
    }
    library.set_setting("font-scale", "1")?;
    online_shelf_pass(h, library_root, &mut captures)?;
    let report = serde_json::json!({
        "result": "PASS",
        "library": library_root,
        "fixtures": "Original five-book CBZ fixtures and the existing labelled local WASM filter source",
        "checks": [
            "phone and iPad shelf actions, safe navigation, 44pt touch targets",
            "actual thumbnail tap selects its book and starts reading that same book",
            "cover opens detail; page advances; both back routes preserve entry point",
            "saved page and history read from SQLite; history resumes the same page",
            "light/dark choices clicked in production settings",
            "installed source entered through its UI; WASM receives the adventure filter",
            "1194x834 at 200 percent: reading action reaches reader without overlapping the actual rendered shelf clip",
            "320pt at 200 percent: history introduction fully visible and separated from rows; history resumes saved page",
            "320pt split view at 200 percent: reading action reachable, settings/filter labels unclipped",
            "settled shelf stops requesting animation frames"
        ],
        "idle_repaint_delay": format!("{idle_delay:?}"),
        "captures": captures,
        "scope": "Local offscreen production UI; device-specific UIKit/Metal/touch/file-picker behavior is not exercised",
    });
    std::fs::write(
        h.output.join("design-acceptance.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "PASS: design review interactions and {} captures at {}",
        captures.len(),
        h.output.display()
    );
    Ok(())
}

/// Round 1 online shelf: a followed work with no local chapters appears on
/// the shelf, a fresh chapter badges its locally-linked book, and the shelf
/// resume opens the online detail page and enters the reader by itself.
fn online_shelf_pass(
    h: &mut Harness,
    library_root: &Path,
    captures: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let library = yejian_core::Library::open(library_root)?;
    let books = library.list_books("")?;
    let linked = books
        .iter()
        .find(|book| book.title == "山海之间")
        .context("online: first fixture is missing")?;
    library.link_chapter(
        &linked.id,
        &yejian_core::ChapterMetadata {
            source_id: "local.filter-acceptance".into(),
            manga_id: "fx-pop-1".into(),
            manga_title: "热门-第1页".into(),
            chapter_id: "daily-1".into(),
            chapter_title: "第 1 话".into(),
            chapter_number: Some(1.),
            volume_number: None,
        },
    )?;
    h.size = [834, 1194];
    h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    // Follow the linked work and a second work with no local chapters.
    for (listing, title) in [(None, "热门-第1页"), (Some("最新"), "最新-第1页")] {
        if let Some(listing) = listing {
            h.click_text(listing)?;
            h.wait(|app| app.source_browser_report()["results"][0] == title)?;
        }
        h.click("source-result-0")?;
        h.wait(|app| {
            app.hitboxes.contains_key("source-detail-follow")
                && app.source_browser_report()["busy"] == false
        })?;
        h.click("source-detail-follow")?;
        h.frames(3)?;
        anyhow::ensure!(
            h.painted_text
                .iter()
                .any(|text| text.text == "已在书架 · 移出"),
            "online: 《{title}》 did not land on the shelf"
        );
        h.click("source-detail-back")?;
        h.frames(3)?;
    }
    h.click("nav-library")?;
    h.wait(ReaderApp::covers_loaded)?;
    h.frames(6)?;
    anyhow::ensure!(
        h.app.visible_count() == 6,
        "online: followed work did not synthesise a shelf entry"
    );
    design_capture(h, "shelf-online", captures)?;
    // A check that finds a fresh chapter badges the locally-linked book.
    let follows_path = library_root.join("follows.json");
    let mut store: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&follows_path)?)?;
    for follow in store["follows"]
        .as_array_mut()
        .context("online: follows store is corrupt")?
    {
        if follow["manga_id"] == "fx-pop-1" {
            follow["known"] = serde_json::json!(["daily-1"]);
            follow["fresh"] = serde_json::json!(["daily-2"]);
        }
    }
    std::fs::write(&follows_path, serde_json::to_vec(&store)?)?;
    h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.frames(8)?;
    // Centre the linked book — its spine carries the manga title — so the
    // fresh-chapter badge is visible.
    for _ in 0..6 {
        if h.app.focused_book_title() == "热门-第1页" {
            break;
        }
        h.click("shelf-prev")?;
        h.wait(|app| !app.frame_is_animating)?;
    }
    anyhow::ensure!(
        h.app.focused_book_title() == "热门-第1页",
        "online: shelf could not focus the linked book"
    );
    design_capture(h, "shelf-online-badge", captures)?;
    // Focus the online entry and resume it.
    for _ in 0..6 {
        if h.app.focused_book_title() == "最新-第1页" {
            break;
        }
        h.click("shelf-next")?;
        h.wait(|app| !app.frame_is_animating)?;
    }
    for _ in 0..6 {
        if h.app.focused_book_title() == "最新-第1页" {
            break;
        }
        h.click("shelf-prev")?;
        h.wait(|app| !app.frame_is_animating)?;
    }
    anyhow::ensure!(
        h.app.focused_book_title() == "最新-第1页",
        "online: shelf could not focus the followed work"
    );
    h.click("shelf-read")?;
    // The fixture exports no get_page_list, so the stream stays pending;
    // entering the online reader is the observable outcome.
    h.wait(|app| app.screen_name() == "reader")?;
    h.frames(4)?;
    design_capture(h, "shelf-online-reader", captures)?;
    h.click("reader-back")?;
    h.frames(6)?;
    Ok(())
}

/// Renders the production UI at iPad point sizes with iPad safe-area insets.
///
/// The GPU path is identical to the one the shells use, so layout can be
/// checked on a desktop machine without a Mac, a simulator, or CI minutes.
fn filters_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    set_insets_override(Some(Insets {
        left: 0,
        top: 24,
        right: 0,
        bottom: 20,
    }));
    h.size = [1366, 1024];
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    h.frames(4)?;
    h.frame(vec![], Some("source-browse-landscape"))?;
    anyhow::ensure!(
        h.texts.iter().any(|(text, _)| text == "热门-第1页"),
        "default listing was not executed"
    );
    // Once the last grid row is visible the next page loads on its own; the
    // button below remains as the manual path when it does not trigger.
    h.wait(|app| {
        app.source_browser_report()["results"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|value| value == "热门-第2页"))
    })?;
    h.click_text("最新")?;
    h.wait(|app| {
        app.source_browser_report()["results"]
            .as_array()
            .is_some_and(|entries| entries.len() == 1 && entries[0] == "最新-第1页")
    })?;
    h.click_text("筛选与排序")?;
    h.frames(4)?;
    h.frame(vec![], Some("source-filters-landscape"))?;
    h.click_text("冒险")?;
    h.click_text("应用筛选")?;
    h.wait(|app| {
        app.source_browser_report()["results"]
            .as_array()
            .is_some_and(|entries| entries.len() == 1 && entries[0] == "筛选:冒险题材")
    })?;
    h.frames(4)?;
    h.frame(vec![], Some("source-filtered-result"))?;
    // Cancelling a changed draft preserves the submitted filter.
    h.click_text("筛选（1）")?;
    h.frames(3)?;
    h.click_text("恋爱")?;
    h.click_text("取消")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.source_browser_report()["results"][0] == "筛选:冒险题材",
        "cancel changed results"
    );
    // Reset must return to the source defaults, not submit all visible defaults.
    h.click_text("筛选（1）")?;
    h.frames(3)?;
    h.click_text("重置")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "未筛选结果")?;
    // Check and sort widgets must preserve numeric meanings and direction.
    h.frames(3)?;
    h.click_text("筛选与排序")?;
    h.frames(3)?;
    h.click_text("包含")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "筛选:已完结(含)")?;
    h.frames(3)?;
    h.click_text("筛选（1）")?;
    h.frames(3)?;
    h.click_text("重置")?;
    h.frames(2)?;
    h.click_text("最新")?;
    h.click_text("升序")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "筛选:最新升序")?;
    h.frames(3)?;
    h.click_text("筛选（1）")?;
    h.frames(3)?;
    h.click_text("重置")?;
    h.frames(2)?;
    // Default 热血 is removed by cycling include -> exclude -> neutral.
    h.click_text("✓ 热血")?;
    h.frames(2)?;
    h.click_text("− 热血")?;
    h.frames(2)?;
    h.click_text("治愈")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "筛选:含治愈")?;
    h.frames(3)?;
    h.click_text("筛选（1）")?;
    h.frames(3)?;
    h.click_text("重置")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "未筛选结果")?;
    h.frames(3)?;
    h.click_text("筛选与排序")?;
    h.frames(3)?;
    for (size, label) in [
        ([834, 1194], "portrait"),
        ([1194, 834], "landscape-11"),
        ([507, 1024], "split-half"),
        ([320, 1024], "split-third"),
    ] {
        h.size = size;
        h.frames(5)?;
        h.frame(vec![], Some(&format!("source-filters-{label}")))?;
        for control in ["应用筛选", "重置", "取消"] {
            anyhow::ensure!(
                h.texts.iter().any(|(text, rect)| text == control
                    && rect.left() >= 0.
                    && rect.right() <= size[0] as f32),
                "{label}: {control} offscreen"
            );
        }
    }
    // A tall form must really scroll and its range input must reject bad data.
    h.size = [507, 1024];
    h.frames(4)?;
    for _ in 0..8 {
        h.frame(
            vec![
                Event::PointerMoved(pos2(350., 700.)),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -380.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(5)?;
    }
    h.frame(vec![], Some("source-filters-range"))?;
    anyhow::ensure!(
        h.texts.iter().any(|(text, _)| text == "最大值"),
        "range filter cannot be reached"
    );
    // Recreate at 200 percent font size, then inspect the actual draft UI.
    let library = yejian_core::Library::open(library_root)?;
    library.set_setting("font-scale", "2")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.app.prepare_source_acceptance("filters")?;
    h.wait(ReaderApp::source_browser_ready)?;
    for (size, label) in [([507, 1024], "half"), ([320, 1024], "third")] {
        h.size = size;
        h.frames(5)?;
        h.frame(vec![], Some(&format!("source-filters-large-{label}")))?;
        for control in ["应用筛选", "重置", "取消"] {
            anyhow::ensure!(
                h.texts.iter().any(|(text, _)| text == control),
                "200 percent {label}: {control} missing"
            );
        }
    }
    // The result grid still fits a narrow split at 200 percent text.
    h.click_text("取消")?;
    h.size = [320, 1024];
    h.frames(5)?;
    h.frame(vec![], Some("source-grid-320-large-text"))?;
    // Recreate at 100 percent for the cover grid and detail captures.
    library.set_setting("font-scale", "1")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    for (size, name) in [
        ([1366, 1024], "source-grid-landscape"),
        ([1024, 1366], "source-grid-portrait"),
    ] {
        h.size = size;
        h.frames(5)?;
        h.frame(vec![], Some(name))?;
    }
    h.click_text("热门-第1页")?;
    h.wait(|app| app.source_browser_report()["busy"] == false)?;
    h.size = [1366, 1024];
    h.frames(5)?;
    h.frame(vec![], Some("source-detail-wide"))?;
    h.size = [507, 1024];
    h.frames(5)?;
    h.frame(vec![], Some("source-detail-narrow"))?;
    // Dark-theme sweep of the grid and detail pages.
    library.set_setting("theme", "dark")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    h.size = [1024, 1366];
    h.frames(5)?;
    h.frame(vec![], Some("source-grid-portrait-dark"))?;
    h.click_text("热门-第1页")?;
    h.wait(|app| app.source_browser_report()["busy"] == false)?;
    h.size = [1366, 1024];
    h.frames(5)?;
    h.frame(vec![], Some("source-detail-wide-dark"))?;
    library.set_setting("theme", "system")?;
    set_insets_override(None);
    println!(
        "PASS: source listing + pagination + real WASM filtered result + cancel/reset + iPad/200 percent filters"
    );
    Ok(())
}

fn ipad_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    // iPad window sizes in points (one harness pixel per point), plus the
    // narrow Split View widths and a no-inset control.
    let profiles: [([u32; 2], i32, i32, &str); 8] = [
        ([1024, 1366], 24, 20, "ipad-pro-129-portrait"),
        ([1366, 1024], 24, 20, "ipad-pro-129-landscape"),
        ([834, 1194], 24, 20, "ipad-11-portrait"),
        ([1194, 834], 24, 20, "ipad-11-landscape"),
        ([744, 1133], 24, 20, "ipad-mini-portrait"),
        ([507, 1024], 24, 20, "ipad-split-half"),
        ([320, 1024], 24, 20, "ipad-split-third"),
        ([1024, 1366], 0, 0, "ipad-pro-129-portrait-no-insets"),
    ];
    for (size, top, bottom, name) in profiles {
        set_insets_override(Some(Insets {
            left: 0,
            top,
            right: 0,
            bottom,
        }));
        h.size = size;
        h.frames(8)?;
        h.frame(vec![], Some(name))?;
        if size[0] >= 880 {
            anyhow::ensure!(
                h.app.hitboxes.contains_key("nav-library"),
                "{name}: tablet rail missing at {} pt wide",
                size[0]
            );
        }
        if top > 0 {
            let bleed: Vec<String> = h
                .app
                .hitboxes
                .iter()
                .filter(|(_, rect)| rect.min.y < top as f32 - 0.5)
                .map(|(id, rect)| format!("{id}={rect:?}"))
                .collect();
            anyhow::ensure!(
                bleed.is_empty(),
                "{name}: {} controls painted inside the {top} pt status-bar inset: {}",
                bleed.len(),
                bleed.join(", ")
            );
        }
        let low: Vec<String> = h
            .app
            .hitboxes
            .iter()
            .filter(|(_, rect)| rect.max.y > size[1] as f32 - bottom as f32 + 0.5)
            .map(|(id, rect)| format!("{id}={rect:?}"))
            .collect();
        anyhow::ensure!(
            low.is_empty(),
            "{name}: {} controls painted inside the {bottom} pt home-indicator inset: {}",
            low.len(),
            low.join(", ")
        );
        let small: Vec<String> = h
            .app
            .hitboxes
            .iter()
            .filter(|(_, rect)| rect.width() < 44. || rect.height() < 44.)
            .map(|(id, rect)| format!("{id}={}x{}", rect.width().round(), rect.height().round()))
            .collect();
        anyhow::ensure!(
            small.is_empty(),
            "{name}: undersized controls: {}",
            small.join(", ")
        );
        println!(
            "ipad {name}: {}x{} pt, insets top {top} bottom {bottom}",
            size[0], size[1]
        );
        if name == "ipad-11-portrait" {
            anyhow::ensure!(
                h.app.shelf_books_fraction() >= 0.9,
                "{name}: collapsed books rect is {:.2} of the stage width, expected the stacked arrangement",
                h.app.shelf_books_fraction()
            );
        }
        for (tab, label) in [
            ("search", "sources"),
            ("settings", "settings"),
            ("history", "history"),
        ] {
            h.click(&format!("nav-{tab}"))?;
            h.frames(4)?;
            h.frame(vec![], Some(&format!("{name}-{label}")))?;
            if tab == "search" {
                for control in ["读取", "社区源"] {
                    anyhow::ensure!(
                        h.texts.iter().any(|(text, _)| text == control),
                        "{name}: source control {control} is clipped"
                    );
                }
            }
        }
        h.click("nav-library")?;
        h.frames(4)?;
    }
    set_insets_override(Some(Insets {
        left: 0,
        top: 24,
        right: 0,
        bottom: 20,
    }));
    for (size, name) in [([1024, 1366], "portrait"), ([1366, 1024], "landscape")] {
        h.size = size;
        h.frames(10)?;
        h.click("open-book")?;
        h.frames(65)?;
        h.frame(vec![], Some(&format!("ipad-pro-129-{name}-detail")))?;
        h.click("read")?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(6)?;
        h.frame(vec![], Some(&format!("ipad-pro-129-{name}-reader")))?;
        if name == "portrait" {
            h.click("reader-settings")?;
            h.frames(6)?;
            h.frame(vec![], Some("ipad-pro-129-portrait-reader-settings"))?;
            for control in [
                "上下滚动",
                "翻页方向",
                "裁剪白边",
                "适应宽度",
                "亮度",
                "双页首页单独",
                "设为所有作品的默认",
            ] {
                anyhow::ensure!(
                    h.texts.iter().any(|(text, _)| text.contains(control)),
                    "portrait: reader settings control {control} is clipped"
                );
            }
            h.click("reader-options-close")?;
            h.frames(4)?;
        }
        h.click("reader-back")?;
        h.frames(24)?;
        h.click("back")?;
        h.frames(45)?;
    }
    let library = yejian_core::Library::open(library_root)?;
    library.set_setting("font-scale", "2")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    for (size, name) in [([507, 1024], "half"), ([320, 1024], "third")] {
        h.size = size;
        h.frames(5)?;
        for (tab, label) in [("search", "sources"), ("settings", "settings")] {
            h.click(&format!("nav-{tab}"))?;
            h.frames(4)?;
            h.frame(
                vec![],
                Some(&format!("ipad-split-{name}-{label}-large-text")),
            )?;
            if tab == "search" {
                for control in ["读取", "社区源"] {
                    anyhow::ensure!(
                        h.texts.iter().any(|(text, _)| text == control),
                        "200 percent {name}: {control} is clipped"
                    );
                }
            }
        }
    }
    library.set_setting("font-scale", "1")?;
    // Dark-theme sweep on the iPad 11 portrait profile.
    library.set_setting("theme", "dark")?;
    h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.wait(ReaderApp::covers_loaded)?;
    h.size = [834, 1194];
    h.frames(8)?;
    h.frame(vec![], Some("ipad-11-portrait-dark"))?;
    for (tab, label) in [
        ("search", "sources"),
        ("settings", "settings"),
        ("history", "history"),
    ] {
        h.click(&format!("nav-{tab}"))?;
        h.frames(4)?;
        h.frame(vec![], Some(&format!("ipad-11-portrait-dark-{label}")))?;
    }
    h.click("nav-library")?;
    h.frames(4)?;
    library.set_setting("theme", "system")?;
    set_insets_override(None);
    println!(
        "PASS: iPad profiles rendered with real UI at 1024x1366, 1366x1024, 1194x834, 744x1133, 507x1024 and 320x1024"
    );
    Ok(())
}

/// One iPad-11 portrait sweep per colour scheme: shelf in light and dark,
/// plus a settings capture with that scheme selected in the picker.
fn schemes_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    set_insets_override(Some(Insets {
        left: 0,
        top: 24,
        right: 0,
        bottom: 20,
    }));
    let library = yejian_core::Library::open(library_root)?;
    for key in ["ink", "mist", "sepia", "mono", "sakura", "dusk"] {
        library.set_setting("palette", key)?;
        for theme in ["light", "dark"] {
            library.set_setting("theme", theme)?;
            h.app = ReaderApp::new(library_root.into(), h.ctx.clone());
            h.wait(ReaderApp::ready)?;
            h.wait(ReaderApp::covers_loaded)?;
            h.size = [834, 1194];
            h.frames(8)?;
            h.frame(vec![], Some(&format!("shelf-{key}-{theme}")))?;
            if theme == "light" {
                h.click("nav-settings")?;
                h.frames(4)?;
                h.frame(vec![], Some(&format!("settings-{key}")))?;
                h.click("nav-library")?;
                h.frames(4)?;
            }
        }
    }
    library.set_setting("theme", "system")?;
    library.set_setting("palette", "ink")?;
    set_insets_override(None);
    println!("PASS: six colour schemes on iPad 11 portrait, light and dark");
    Ok(())
}

/// Exercise gestures and interruptions that a sequence of toolbar clicks
/// misses: double taps, constrained panning, modal input and real return paths.
fn daily_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    struct ResetInsets;
    impl Drop for ResetInsets {
        fn drop(&mut self) {
            set_insets_override(None);
        }
    }
    let _reset = ResetInsets;
    set_insets_override(Some(Insets {
        left: 0,
        top: 24,
        right: 0,
        bottom: 20,
    }));
    let library = yejian_core::Library::open(library_root)?;
    let books = library.list_books("")?;
    let one = books
        .iter()
        .find(|book| book.title == "山海之间")
        .context("daily: first fixture is missing")?;
    let two = books
        .iter()
        .find(|book| book.title == "深蓝来信")
        .context("daily: second fixture is missing")?;
    for (book, number) in [(one, 1.), (two, 2.)] {
        library.link_chapter(
            &book.id,
            &yejian_core::ChapterMetadata {
                source_id: "local.filter-acceptance".into(),
                manga_id: "fx-pop-1".into(),
                manga_title: "热门-第1页".into(),
                chapter_id: format!("daily-{number}"),
                chapter_title: format!("第 {number} 话"),
                chapter_number: Some(number),
                volume_number: None,
            },
        )?;
    }
    library.save_progress(&one.id, 1, false)?;
    library.set_setting("reading-mode", "paged")?;
    library.set_setting("reading-direction", "ltr")?;
    library.set_setting("font-scale", "1")?;
    library.set_setting("reduce-motion", "true")?;
    h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
    h.size = [390, 844];
    h.wait(ReaderApp::ready)?;
    h.frames(8)?;
    h.click("nav-history")?;
    h.frames(5)?;
    let resume = format!("history-{}", one.id);
    let mut captures = Vec::new();
    // The stats card can push the resume row below the fold.
    scroll_until_row_visible(h, &resume, "继续")?;
    design_touch(h, &resume, None)?;
    h.click(&resume)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str()) && h.app.current_page() == 1,
        "daily: history resumed a different book or page"
    );
    h.click("reader-back")?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library" && h.app.hitboxes.contains_key(&resume),
        "daily: returning from history reading detoured through details"
    );
    design_capture(h, "daily-history-return", &mut captures)?;
    h.click(&resume)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(3)?;

    let normal_image = *h
        .app
        .hitboxes
        .get("reader-image")
        .context("daily: loaded page did not expose its painted image")?;
    let canvas = h.app.hitboxes["reader-canvas"];
    let edge = pos2(canvas.right() - 24., canvas.center().y);
    // Both pointer clicks are inside the platform double-click interval.
    // Waiting afterwards also catches a stale delayed first-click action.
    h.click_pos(edge)?;
    h.frames(4)?;
    h.click_pos(edge)?;
    h.frames(30)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str()) && h.app.current_page() == 1,
        "daily: edge double-tap also turned the page"
    );
    let enlarged = h.app.hitboxes["reader-image"];
    anyhow::ensure!(
        enlarged.width() > normal_image.width() * 1.8,
        "daily: double-tap did not enlarge the actual page"
    );
    h.drag(canvas.center(), canvas.center() + vec2(2000., 1800.))?;
    let panned = h.app.hitboxes["reader-image"];
    let canvas = h.app.hitboxes["reader-canvas"];
    let axis_is_bounded = |low: f32, high: f32, view_low: f32, view_high: f32| {
        if high - low > view_high - view_low {
            low <= view_low + 1. && high >= view_high - 1.
        } else {
            ((low + high) - (view_low + view_high)).abs() < 2.
        }
    };
    anyhow::ensure!(
        axis_is_bounded(panned.left(), panned.right(), canvas.left(), canvas.right())
            && axis_is_bounded(panned.top(), panned.bottom(), canvas.top(), canvas.bottom()),
        "daily: zoomed page can be dragged away from the canvas: {panned:?} / {canvas:?}"
    );
    anyhow::ensure!(
        h.painted_images
            .iter()
            .any(|image| image.bounds.intersect(image.clip).contains(canvas.center())),
        "daily: constrained page is not actually painted at the canvas center"
    );
    design_capture(h, "daily-zoom-pan-boundary", &mut captures)?;
    h.frames(30)?;
    let edge = pos2(canvas.right() - 24., canvas.center().y);
    h.click_pos(edge)?;
    h.frames(4)?;
    h.click_pos(edge)?;
    h.frames(30)?;
    let restored = h.app.hitboxes["reader-image"];
    anyhow::ensure!(
        h.app.current_page() == 1
            && (restored.width() - normal_image.width()).abs() < 1.
            && restored.center().distance(normal_image.center()) < 1.,
        "daily: double-tap reset did not restore the page without navigation"
    );
    design_capture(h, "daily-zoom-reset", &mut captures)?;

    // These taps are close in time but far enough apart to be two separate
    // clicks. A delayed single-click implementation must preserve both.
    let canvas = h.app.hitboxes["reader-canvas"];
    h.click_pos(pos2(canvas.right() - 24., canvas.center().y))?;
    h.frames(4)?;
    h.click_pos(pos2(canvas.right() - 59., canvas.center().y))?;
    h.frames(30)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str()) && h.app.current_page() == 3,
        "daily: two distinct rapid edge taps did not each turn a page"
    );
    h.click("reader-prev")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(2)?;
    h.click("reader-prev")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.current_page() == 1,
        "daily: previous-page controls did not restore the test position"
    );

    h.click("reader-settings")?;
    h.frames(4)?;
    for key in [Key::ArrowRight, Key::PageDown, Key::Space] {
        h.key(key)?;
    }
    let wheel = |position: Pos2| {
        vec![
            Event::PointerMoved(position),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(0., -550.),
                source: MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
                phase: TouchPhase::Move,
            },
        ]
    };
    h.frame(wheel(canvas.center()), None)?;
    h.frames(20)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str()) && h.app.current_page() == 1,
        "daily: settings popup let keyboard input navigate the page behind it"
    );
    h.click("reader-mode-vertical")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app
            .hitboxes
            .contains_key(&format!("vertical-page-{}", h.app.current_page())),
        "daily: choosing continuous mode did not change the rendered reader"
    );
    let popup_page = h.app.current_page();
    let popup_fraction = h.app.current_fraction();
    h.key(Key::PageDown)?;
    h.key(Key::Space)?;
    h.frame(wheel(canvas.center()), None)?;
    h.frames(20)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str())
            && h.app.current_page() == popup_page
            && (h.app.current_fraction() - popup_fraction).abs() < 0.0001,
        "daily: settings popup let keys/wheel scroll the long page behind it"
    );
    h.click("reader-direction")?;
    h.frames(3)?;
    h.click("reader-mode-paged")?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("reader-image"),
        "daily: choosing paged mode did not restore the page renderer"
    );
    h.click("reader-options-close")?;
    h.frames(8)?;
    anyhow::ensure!(
        h.app.screen_name() == "reader"
            && h.app.current_book_id() == Some(one.id.as_str())
            && h.app.current_page() == 1,
        "daily: changing reading mode/direction lost the current book or position"
    );
    for (key, expected) in [
        (Key::PageDown, 2),
        (Key::Space, 3),
        (Key::PageUp, 2),
        (Key::ArrowRight, 1),
        (Key::ArrowLeft, 2),
    ] {
        h.key(key)?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(2)?;
        anyhow::ensure!(
            h.app.current_page() == expected,
            "daily: RTL {key:?} reached page {}, expected {expected}",
            h.app.current_page()
        );
    }
    design_capture(h, "daily-rtl-reader", &mut captures)?;
    h.click("reader-chapters")?;
    h.frames(5)?;
    let before_dialog = h.app.current_page();
    for key in [Key::ArrowRight, Key::Space, Key::PageDown] {
        h.key(key)?;
    }
    h.frame(wheel(canvas.center()), None)?;
    h.frames(20)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(one.id.as_str())
            && h.app.current_page() == before_dialog,
        "daily: chapter picker allowed navigation behind the popup"
    );
    design_capture(h, "daily-chapter-picker", &mut captures)?;
    // Round 3: manage mode multi-selects chapters and confirms deletion —
    // cancel here keeps the fixture books for the remaining captures.
    h.click("chapter-manage")?;
    h.frames(4)?;
    h.click(&format!("chapter-check-{}", two.id))?;
    h.frames(3)?;
    design_capture(h, "chapters-delete", &mut captures)?;
    h.click("chapter-delete")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.hitboxes.contains_key("storage-confirm-go"),
        "daily: deleting a chapter did not ask for confirmation"
    );
    design_capture(h, "chapters-delete-confirm", &mut captures)?;
    assert_opaque_card(h, "chapters-delete-confirm.png", "storage-confirm-card")?;
    h.click_text("取消")?;
    h.frames(3)?;
    h.click_text("关闭")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.screen_name() == "reader",
        "daily: closing chapter picker left the reader"
    );
    h.click("reader-settings")?;
    h.frames(3)?;
    h.click("reader-mode-vertical")?;
    h.frames(3)?;
    h.click("reader-options-close")?;
    h.frames(4)?;
    for _ in 0..one.page_count {
        if h.app.current_page() + 1 == one.page_count {
            break;
        }
        h.click("reader-next")?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(2)?;
    }
    anyhow::ensure!(
        h.app.current_page() + 1 == one.page_count,
        "daily: next-page controls did not reach the final chapter page"
    );
    let canvas = h.app.hitboxes["reader-canvas"];
    design_capture(h, "daily-continuous-chapter-end", &mut captures)?;
    // Scrolling past the last page flows straight into the next chapter.
    for _ in 0..10 {
        if h.app.current_book_id() == Some(two.id.as_str()) {
            break;
        }
        h.frame(wheel(canvas.center()), None)?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(4)?;
    }
    anyhow::ensure!(
        h.app.current_book_id() == Some(two.id.as_str()),
        "daily: scrolling past the chapter end did not continue into the next chapter"
    );
    design_capture(h, "daily-continuous-next-chapter", &mut captures)?;
    h.click("reader-back")?;
    h.wait(ReaderApp::position_saved)?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library"
            && h.app.hitboxes.contains_key(&format!("history-{}", two.id)),
        "daily: crossing chapters discarded the history return destination"
    );

    for (name, size, font_scale) in [
        ("phone", [390, 844], "1"),
        ("ipad-landscape", [1194, 834], "1"),
        ("large-text", [320, 1024], "2"),
    ] {
        eframe::App::on_exit(&mut h.app);
        library.set_setting("font-scale", font_scale)?;
        h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
        h.size = size;
        h.wait(ReaderApp::ready)?;
        h.frames(8)?;
        h.click("nav-history")?;
        h.frames(5)?;
        let row = format!("history-{}", two.id);
        scroll_until_row_visible(h, &row, "继续")?;
        h.click(&row)?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(3)?;
        h.click("reader-settings")?;
        h.frames(6)?;
        // Save rejected layouts too, so a failed gate leaves an actionable
        // picture rather than only a missing-control error.
        h.frame(vec![], Some(&format!("daily-options-{name}")))?;
        for id in [
            "reader-mode-paged",
            "reader-mode-spread",
            "reader-mode-vertical",
            "reader-direction",
            "reader-options-close",
        ] {
            design_touch(h, id, None)
                .with_context(|| format!("daily: reading options at {name}"))?;
            let rect = h.app.hitboxes[id];
            anyhow::ensure!(
                h.painted_text.iter().any(|text| {
                    rect.intersects(text.bounds)
                        && design_safe_rect(h).expand(0.5).contains_rect(text.bounds)
                        && text.clip.expand(0.5).contains_rect(text.bounds)
                }),
                "daily: {id} has no fully readable label at {name}"
            );
        }
        design_capture(h, &format!("daily-options-{name}"), &mut captures)?;
        h.click("reader-options-close")?;
        h.frames(3)?;
        h.click("reader-back")?;
        h.frames(4)?;
    }
    library.set_setting("font-scale", "1")?;
    eframe::App::on_exit(&mut h.app);
    h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
    h.size = [1194, 834];
    h.wait(ReaderApp::ready)?;
    h.frames(6)?;
    h.app.prepare_source_acceptance("browse")?;
    h.wait(ReaderApp::source_browser_ready)?;
    h.frames(4)?;
    h.click_text("热门-第1页")?;
    h.wait(|app| app.source_browser_report()["busy"] == false)?;
    h.frames(5)?;
    h.frame(vec![], Some("daily-source-detail"))?;
    // The detail header is tall; scroll the chapter rows fully into view.
    scroll_page(h, -320.)?;
    let first_read = "第 1 话";
    let second_read = "第 2 话";
    for label in [first_read, second_read] {
        anyhow::ensure!(
            design_label_visible(h, label),
            "daily: source detail did not recognize the locally linked chapter: {label}"
        );
    }
    anyhow::ensure!(
        h.texts.iter().filter(|(text, _)| text == "已下载").count() >= 2,
        "daily: locally linked chapters did not show their downloaded state"
    );
    let chapter_y = |h: &Harness, label: &str| -> Result<f32> {
        h.texts
            .iter()
            .find(|(text, _)| text == label)
            .map(|(_, rect)| rect.center().y)
            .with_context(|| format!("daily: chapter label is missing: {label}"))
    };
    h.click_text("从头开始")?;
    h.frames(3)?;
    anyhow::ensure!(
        chapter_y(h, first_read)? < chapter_y(h, second_read)?,
        "daily: chapter order did not start from the first chapter"
    );
    h.click_text("最新在前")?;
    h.frames(3)?;
    anyhow::ensure!(
        chapter_y(h, second_read)? < chapter_y(h, first_read)?,
        "daily: newest-first action did not reverse the chapter order"
    );
    design_capture(h, "daily-source-chapter-order", &mut captures)?;
    h.click_text("查找章节名称或编号")?;
    h.frame(vec![Event::Text("第 2".into())], None)?;
    h.frames(4)?;
    anyhow::ensure!(
        design_label_visible(h, second_read)
            && !h.texts.iter().any(|(text, _)| text == first_read),
        "daily: chapter search did not narrow the actual chapter list"
    );
    h.click_text(second_read)?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(two.id.as_str()),
        "daily: source chapter read action opened a different local archive"
    );
    h.click("reader-next")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frames(3)?;
    anyhow::ensure!(
        h.app.current_page() == 1,
        "daily: source-opened local chapter did not support reading"
    );
    design_capture(h, "daily-source-opened-reader", &mut captures)?;
    h.click("reader-back")?;
    h.wait(ReaderApp::position_saved)?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library"
            && h.app.source_browser_report()["source"] == "local.filter-acceptance",
        "daily: source reading did not return to the source detail"
    );
    // Scroll position survived the round trip; verify both ends of the page.
    scroll_page(h, 320.)?;
    anyhow::ensure!(
        design_label_visible(h, "返回搜索结果")
            && h.texts.iter().any(|(text, _)| text == "第 2"),
        "daily: source reading lost the detail page or chapter search"
    );
    scroll_page(h, -320.)?;
    anyhow::ensure!(
        design_label_visible(h, second_read)
            && !h.texts.iter().any(|(text, _)| text == first_read),
        "daily: source reading did not return to the same filtered chapter list"
    );
    design_capture(h, "daily-source-reader-return", &mut captures)?;
    h.click_text("清除查找")?;
    h.frames(3)?;
    // Restoring two rows grew the page; scroll so both names are on screen.
    scroll_page(h, -320.)?;
    anyhow::ensure!(
        chapter_y(h, second_read)? < chapter_y(h, first_read)?,
        "daily: returning from source reading lost the chapter sort choice"
    );
    std::fs::write(
        h.output.join("daily-acceptance.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "result": "PASS",
            "library": library_root,
            "checks": [
                "history resumes and returns directly",
                "edge double-tap zooms without turning or delayed navigation",
                "two spatially distinct rapid edge taps each advance one page",
                "zoomed pan stays bounded and reset recenters",
                "reader options change mode and direction in place",
                "settings and chapter popup block reader keyboard and wheel input",
                "RTL reverses arrows but preserves PageDown/Space/PageUp meaning",
                "continuous chapter-end action opens the next downloaded chapter",
                "cross-chapter return preserves history",
                "phone, iPad landscape and 320pt 200% option controls stay readable and touch-sized",
                "real WASM source chapter order and search affect the visible list",
                "downloaded source chapter opens the matching archive and returns to the same source detail/search/sort"
            ],
            "captures": captures,
        }))?,
    )?;
    println!(
        "PASS: daily reading gestures, modal isolation, RTL shortcuts, history return, chapter continuation and reading options. Captures at {}",
        h.output.display()
    );
    Ok(())
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
    let real_source = std::env::var_os("YEJIAN_REAL_FILTERS").map(PathBuf::from);
    if let Some(source) = &real_source {
        let target = data
            .join("library/sources")
            .join(source.file_name().context("source name")?);
        std::fs::create_dir_all(&target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), target.join(entry.file_name()))?;
            }
        }
    }
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
    if let Some(source) = real_source {
        h.size = [1366, 1024];
        h.app.prepare_installed_source_acceptance(
            source
                .file_name()
                .and_then(|s| s.to_str())
                .context("source id")?,
        )?;
        h.wait(|app| {
            app.source_browser_report()["loaded"] == true
                && !app.source_browser_report()["busy"]
                    .as_bool()
                    .unwrap_or(true)
        })?;
        h.frames(5)?;
        // A blocked remote search must not hide the successfully loaded filters.
        if h.app.source_browser_report()["filter_editor"] != true {
            h.click_text("筛选与排序")?;
            h.frames(3)?;
        }
        h.frame(vec![], Some("real-source-filters-landscape"))?;
        h.size = [834, 1194];
        h.frames(4)?;
        h.frame(vec![], Some("real-source-filters-portrait"))?;
        println!("REAL SOURCE: {}", h.app.source_browser_report());
        return Ok(());
    }
    if std::env::var_os("YEJIAN_BENCH").is_some() {
        return bench_pass(&output);
    }
    if std::env::var_os("YEJIAN_FILTERS_ONLY").is_some() {
        return filters_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_IPAD_ONLY").is_some() {
        return ipad_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_DESIGN_ONLY").is_some() {
        return design_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_SCHEMES_ONLY").is_some() {
        return schemes_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_DAILY_ONLY").is_some() {
        return daily_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_ONLINE_ONLY").is_some() {
        let mut captures = Vec::new();
        return online_shelf_pass(&mut h, &data.join("library"), &mut captures);
    }
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
    let unread = reopened
        .list_books("")?
        .into_iter()
        .find(|b| b.last_read_at.is_none())
        .context("expected an unread fixture")?;
    h.click("search")?;
    h.frames(2)?;
    h.click("search-field")?;
    h.frame(vec![Event::Text(unread.title.clone())], None)?;
    h.frames(3)?;
    anyhow::ensure!(h.app.shelf_book_count() == 1 && !h.app.hitboxes.contains_key("shelf-next"));
    let center = h.app.hitboxes["shelf-center"].center();
    h.frame(
        vec![
            Event::PointerMoved(center),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(90., 0.),
                phase: TouchPhase::Move,
                source: MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            },
        ],
        None,
    )?;
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
    anyhow::ensure!(
        h.app.current_book_id() == Some(unread.id.as_str()) && h.app.current_page() == 0
    );
    h.click("reader-back")?;
    h.frames(4)?;
    anyhow::ensure!(h.app.screen_name() == "library" && h.app.shelf_book_count() == 1);
    h.size = [844, 390];
    h.frames(3)?;
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(844., 390.));
    anyhow::ensure!(
        screen.contains_rect(h.app.hitboxes["shelf-read"]),
        "landscape shelf-read {:?} outside {screen:?}",
        h.app.hitboxes["shelf-read"]
    );
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
            && h.app.current_page() == 1
            && (h.app.current_fraction() - fraction).abs() < 0.002,
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
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "back from the shortcut opened details"
    );

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
        if h.app.chapter_end() {
            break;
        }
        h.click("reader-next")?;
        h.wait(ReaderApp::page_loaded)?;
    }
    anyhow::ensure!(
        h.app.chapter_end() && h.app.current_book_id() == Some(one.id.as_str()),
        "last page did not show the chapter end card"
    );
    // The end card sits past the last page; progress stays on it.
    h.frame(vec![], Some("reader-chapter-end"))?;
    h.click("chapter-end-continue")?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(two.id.as_str()) && h.app.current_page() == 0,
        "end-of-chapter did not continue into the next downloaded chapter"
    );
    h.frame(vec![], Some("reader-next-chapter"))?;
    // Time/battery pill shows only while the controls are hidden. Taps
    // resolve after the double-click delay, so let them fire first.
    h.click("reader-canvas")?;
    h.frames(20)?;
    h.frame(vec![], Some("reader-status"))?;
    h.click("reader-canvas")?;
    h.frames(20)?;
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
    // Round A: top-bar bookmark + share on a downloaded chapter.
    if !h.app.hitboxes.contains_key("reader-bookmark") {
        h.click("reader-canvas")?;
        h.frames(20)?;
    }
    h.click("reader-bookmark")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.bookmark_count() == 1,
        "reader: the top-bar bookmark toggle did not record a bookmark"
    );
    h.click("reader-chapters")?;
    h.frames(8)?;
    h.click("dialog-tab-书签")?;
    h.frames(4)?;
    h.frame(vec![], Some("reader-bookmarks"))?;
    h.click_text("关闭")?;
    h.frames(4)?;
    anyhow::ensure!(
        h.app.share_ready_now(),
        "share: a downloaded page must be shareable"
    );
    h.frame(vec![], Some("reader-share-enabled"))?;
    h.click("reader-share")?;
    for _ in 0..40 {
        h.frame(vec![], None)?;
        if h.app.last_shared().is_some() {
            break;
        }
    }
    let staged = h
        .app
        .last_shared()
        .context("share: the worker did not stage the page")?
        .to_path_buf();
    anyhow::ensure!(
        staged.exists() && staged.parent().is_some_and(|dir| dir.ends_with("share")),
        "share: staged file missing or outside share/"
    );
    // An online chapter whose bytes have not arrived keeps share disabled.
    h.app.prepare_share_stream();
    h.frames(10)?;
    if !h.app.hitboxes.contains_key("reader-share") {
        h.click("reader-canvas")?;
        h.frames(20)?;
    }
    anyhow::ensure!(
        !h.app.share_ready_now(),
        "share: must stay disabled until the page bytes exist"
    );
    h.frame(vec![], Some("reader-share-disabled"))?;
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

    // The vertical strip draws a labelled separator where one chapter hands
    // off to the next — scroll the first chapter down until it appears.
    h.app = ReaderApp::new(data.join("library"), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.frames(30)?;
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
    // The paged run above stored per-work prefs; switch this work to the
    // strip through the reader options like a user would.
    h.click("reader-settings")?;
    h.frames(3)?;
    h.click("reader-mode-vertical")?;
    h.frames(3)?;
    h.click("reader-options-close")?;
    h.frames(4)?;
    let separator = |app: &ReaderApp| {
        app.hitboxes
            .keys()
            .any(|key| key.starts_with("vertical-separator-"))
    };
    // Page to the last page of the first chapter, then ease over the seam:
    // each step waits for the strip to load before scrolling on.
    for _ in 0..one.page_count {
        if h.app.current_page() + 1 == one.page_count {
            break;
        }
        h.click("reader-next")?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(2)?;
    }
    let canvas = h.app.hitboxes["reader-canvas"];
    let wheel = || {
        vec![
            Event::PointerMoved(canvas.center()),
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(0., -220.),
                source: MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
                phase: TouchPhase::Move,
            },
        ]
    };
    let mut crossed = false;
    for _ in 0..300 {
        if separator(&h.app) {
            crossed = true;
            break;
        }
        h.frame(wheel(), None)?;
        h.wait(ReaderApp::page_loaded)?;
    }
    anyhow::ensure!(
        crossed,
        "vertical strip never showed a chapter separator (book={:?} page={} end={} keys={:?})",
        h.app.current_book_id(),
        h.app.current_page(),
        h.app.chapter_end(),
        h.app.hitboxes.keys().collect::<Vec<_>>()
    );
    h.frame(vec![], Some("reader-vertical-separator"))?;

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

/// Deterministic xorshift64 used only for bench fixture generation.
struct BenchRng(u64);
impl BenchRng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Renders one synthetic manga page: a light base, horizontal "screen tone"
/// lines and per-pixel noise, so JPEG decode cost is realistic rather than a
/// flat-color image that encodes to nothing.
fn bench_page(width: u32, height: u32, seed: u64) -> Result<Vec<u8>> {
    let mut rng = BenchRng(seed);
    let mut image = image::RgbImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let line = y % 6 < 2 || x % 7 == 0;
        let noise = (rng.next() % 40) as u8;
        let base = if line { 96u8 } else { 236u8 };
        let v = base.saturating_sub(noise / 3);
        *pixel = image::Rgb([v, v.saturating_sub(noise / 5), v.saturating_sub(noise / 4)]);
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image)?;
    Ok(out.into_inner())
}

/// Builds `name.cbz` with `count` JPEG pages of `width` x `height` each.
fn bench_cbz(root: &Path, name: &str, width: u32, height: u32, count: usize) -> Result<PathBuf> {
    use std::io::Write;
    let path = root.join(format!("{name}.cbz"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    for page in 0..count {
        let bytes = bench_page(width, height, 0x9e3779b97f4a7c15 + page as u64)?;
        zip.start_file(
            format!("{:03}.jpg", page + 1),
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(path)
}

fn percentile_of(values: &mut Vec<f64>, p: f64) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return 0.;
    }
    // Ceil so small samples (5 spread turns) still pick the tail, not a
    // mid-range value.
    values[(((values.len() - 1) as f64 * p).ceil() as usize).min(values.len() - 1)]
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    percentile_of(&mut v, 0.5)
}

/// Book ids discovered during the import phase.
struct BenchBooks {
    paged: String,
    strip: String,
    hires: String,
}

/// Imports `cbz` and returns the id of the book that appeared.
fn bench_import(h: &mut Harness, known: &mut Vec<String>, cbz: PathBuf) -> Result<String> {
    let before = h.app.shelf_book_count();
    h.app.import_files(vec![cbz]);
    let deadline = Instant::now() + Duration::from_secs(60);
    while h.app.shelf_book_count() == before {
        anyhow::ensure!(Instant::now() < deadline, "bench import stalled");
        h.frames(2)?;
        std::thread::sleep(Duration::from_millis(15));
    }
    let id = (0..h.app.shelf_book_count())
        .filter_map(|i| h.app.shelf_book_id(i))
        .find(|id| !known.contains(id))
        .context("bench import produced no new book")?;
    known.push(id.clone());
    Ok(id)
}

/// Imports the three fixtures on a throwaway harness, then drops it so the
/// measured cold start begins from an already-populated library.
fn bench_prepare(fixtures: &Path, library: &Path, output: &Path) -> Result<BenchBooks> {
    let mut h = Harness::new(library.to_path_buf(), output.to_path_buf())?;
    h.ctx.set_pixels_per_point(2.);
    let mut known = Vec::new();
    let books = BenchBooks {
        paged: bench_import(&mut h, &mut known, fixtures.join("bench-paged.cbz"))?,
        strip: bench_import(&mut h, &mut known, fixtures.join("bench-strip.cbz"))?,
        hires: bench_import(&mut h, &mut known, fixtures.join("bench-hires.cbz"))?,
    };
    h.wait(|a| a.covers_loaded())?;
    h.frames(3)?;
    Ok(books)
}

/// open_to_first_page_ms: wall clock from issuing the open command to the
/// first painted frame where the current page's texture is loaded.
/// page_turn_wait_ms: wall clock from issuing a turn to the first painted
/// frame where the new page's texture is loaded; `turns` sequential turns
/// are issued 400 ms apart and reported as mean/p95 by the caller.
fn bench_open_and_turns(
    h: &mut Harness,
    book: &str,
    turns: usize,
) -> Result<(f64, Vec<f64>)> {
    let open_began = Instant::now();
    h.app.open_book(book);
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    let open_ms = open_began.elapsed().as_secs_f64() * 1000.;
    let mut waits = Vec::new();
    for turn in 1..=turns {
        let slot = Instant::now();
        let before = h.app.current_page();
        h.click("reader-next")?;
        let deadline = Instant::now() + Duration::from_secs(25);
        while !(h.app.current_page() > before && h.app.page_loaded()) {
            anyhow::ensure!(
                Instant::now() < deadline,
                "bench turn {before} timed out at page {} (screen {})",
                h.app.current_page(),
                h.app.screen_name()
            );
            h.frame(vec![], None)?;
            std::thread::sleep(Duration::from_millis(15));
        }
        waits.push(slot.elapsed().as_secs_f64() * 1000.);
        if turn < turns {
            let elapsed = slot.elapsed();
            if elapsed < Duration::from_millis(400) {
                std::thread::sleep(Duration::from_millis(400) - elapsed);
            }
        }
    }
    Ok((open_ms, waits))
}

/// strip_blank_frames: frames during a scripted fast scroll — 50 pt of
/// wheel delta per frame paced to one frame every 16.7 ms of wall clock
/// (≈3000 pt/s) across the whole strip — in which any visible page region
/// painted without a texture. Bounded by a 180 s limit.
fn bench_strip_scroll(h: &mut Harness) -> Result<usize> {
    let center = pos2(h.size[0] as f32 / 2., h.size[1] as f32 / 2.);
    let frame_pace = Duration::from_secs_f64(1. / 60.);
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut blanks = 0usize;
    loop {
        let slot = Instant::now();
        h.frame(
            vec![
                Event::PointerMoved(center),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -50.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        if yejian::perf::take_blank_frame() {
            blanks += 1;
        }
        if h.app.current_fraction() >= 0.999 {
            return Ok(blanks);
        }
        anyhow::ensure!(Instant::now() < deadline, "bench strip scroll timed out");
        if let Some(rest) = frame_pace.checked_sub(slot.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Metrics for one fixture phase. peak_texture_bytes is the maximum
/// resident page+cover texture bytes during the phase (sum of live tile
/// w*h*4), with the peak reset to live bytes when the phase starts.
fn bench_phase(peak: u64, open_ms: f64, waits: &mut Vec<f64>) -> serde_json::Value {
    serde_json::json!({
        "open_to_first_page_ms": open_ms,
        "page_turn_wait_ms_mean": waits.iter().sum::<f64>() / waits.len() as f64,
        "page_turn_wait_ms_p95": percentile_of(waits, 0.95),
        "peak_texture_bytes": peak,
    })
}

/// One bench iteration: cold start on a prepared library, then the paged,
/// hires and strip phases (hires only, in spread mode, when `spread_only`).
fn bench_iteration(
    fixtures: &Path,
    library: &Path,
    output: &Path,
    size: [u32; 2],
    spread_only: bool,
) -> Result<serde_json::Value> {
    let books = bench_prepare(fixtures, library, output)?;
    yejian::perf::reset();
    // cold_start_to_shelf_ms: harness construction (process-level app +
    // worker startup) to the first painted frame where the shelf shows its
    // books with covers loaded.
    let start = Instant::now();
    let mut h = Harness::new(library.to_path_buf(), output.to_path_buf())?;
    h.ctx.set_pixels_per_point(2.);
    h.size = size;
    h.wait(|a| a.shelf_book_count() >= 3 && a.covers_loaded())?;
    h.frame(vec![], None)?;
    let cold_start_to_shelf_ms = start.elapsed().as_secs_f64() * 1000.;

    let mut phases = serde_json::Map::new();
    if !spread_only {
        yejian::perf::reset_peak_to_live();
        let (open_ms, mut waits) = bench_open_and_turns(&mut h, &books.paged, 20)?;
        phases.insert(
            "paged".into(),
            bench_phase(yejian::perf::snapshot().peak_bytes, open_ms, &mut waits),
        );
        h.app.leave_reader();
        h.frames(3)?;
    }

    yejian::perf::reset_peak_to_live();
    if spread_only {
        h.app.set_mode("spread");
    }
    // 12-page fixture: 8 paged turns fit; spread advances two pages per
    // turn, so 5 turns reach the last spread without overshooting.
    let (open_ms, mut waits) = bench_open_and_turns(&mut h, &books.hires, if spread_only { 5 } else { 8 })?;
    phases.insert(
        "hires".into(),
        bench_phase(yejian::perf::snapshot().peak_bytes, open_ms, &mut waits),
    );
    h.app.leave_reader();
    h.frames(3)?;

    if !spread_only {
        h.app.set_mode("vertical");
        yejian::perf::reset_peak_to_live();
        h.app.open_book(&books.strip);
        {
            let deadline = Instant::now() + Duration::from_secs(180);
            while !(h.app.is_vertical() && h.app.page_loaded()) {
                anyhow::ensure!(Instant::now() < deadline, "bench strip open timed out");
                h.frame(vec![], None)?;
                std::thread::sleep(Duration::from_millis(15));
            }
        }
        h.frames(3)?;
        yejian::perf::reset_peak_to_live();
        let strip_blank_frames = bench_strip_scroll(&mut h)?;
        phases.insert(
            "strip".into(),
            serde_json::json!({
                "strip_blank_frames": strip_blank_frames,
                "peak_texture_bytes": yejian::perf::snapshot().peak_bytes,
            }),
        );
        h.app.leave_reader();
        h.app.set_mode("paged");
    }

    let snapshot = yejian::perf::snapshot();
    let mut run = serde_json::json!({
        "cold_start_to_shelf_ms": cold_start_to_shelf_ms,
        "decode_p50_ms": snapshot.decode_p50_ms,
        "decode_p95_ms": snapshot.decode_p95_ms,
        "queue_p95_ms": snapshot.queue_p95_ms,
        "frame_p95_ms": snapshot.frame_p95_ms,
    });
    for (key, value) in phases {
        run[key] = value;
    }
    Ok(run)
}

/// Median of `key` across `runs` (missing keys are skipped).
fn bench_median(runs: &[serde_json::Value], key: &str) -> f64 {
    let values: Vec<f64> = runs
        .iter()
        .filter_map(|run| run[key].as_f64())
        .collect();
    median(&values)
}

/// `YEJIAN_BENCH=1`: runs the perf bench 3x per viewport and writes
/// `bench.json` (median per metric) into the output dir.
/// `YEJIAN_BENCH_PAGE_BUDGET` (bytes) overrides the page-texture budget.
fn bench_pass(output: &Path) -> Result<()> {
    let fixtures = output.join("bench-fixtures");
    std::fs::create_dir_all(&fixtures)?;
    // (a) 40-page 1500x2200 manga; (b) 20-page 800x12000 vertical strip;
    // (c) 12-page 2480x3508 high-res scan (triggers display-sized decode).
    if !fixtures.join("bench-paged.cbz").exists() {
        println!("bench: generating paged fixture (this takes a moment)");
        bench_cbz(&fixtures, "bench-paged", 1500, 2200, 40)?;
    }
    if !fixtures.join("bench-strip.cbz").exists() {
        println!("bench: generating strip fixture");
        bench_cbz(&fixtures, "bench-strip", 800, 12000, 20)?;
    }
    if !fixtures.join("bench-hires.cbz").exists() {
        println!("bench: generating hires fixture");
        bench_cbz(&fixtures, "bench-hires", 2480, 3508, 12)?;
    }
    if let Ok(bytes) = std::env::var("YEJIAN_BENCH_PAGE_BUDGET") {
        let bytes: u64 = bytes.parse().context("YEJIAN_BENCH_PAGE_BUDGET")?;
        yejian::perf::set_page_budget(Some(bytes));
        println!("bench: page budget override {bytes} bytes");
    }
    const TOP_KEYS: &[&str] = &[
        "cold_start_to_shelf_ms",
        "decode_p50_ms",
        "decode_p95_ms",
        "queue_p95_ms",
        "frame_p95_ms",
    ];
    const PHASE_KEYS: &[&str] = &[
        "open_to_first_page_ms",
        "page_turn_wait_ms_mean",
        "page_turn_wait_ms_p95",
        "peak_texture_bytes",
    ];
    const STRIP_KEYS: &[&str] = &["strip_blank_frames", "peak_texture_bytes"];
    let mut report = serde_json::Map::new();
    for (label, size, spread_only) in [
        ("ipad-11", [834, 1194], false),
        ("ipad-pro-129", [1024, 1366], false),
        ("ipad-pro-129-landscape-spread", [1366, 1024], true),
    ] {
        let mut runs = Vec::new();
        for iteration in 0..3 {
            let library = output.join(format!("bench-lib-{label}-{iteration}"));
            let _ = std::fs::remove_dir_all(&library);
            let run = bench_iteration(&fixtures, &library, output, size, spread_only)?;
            println!("bench {label} run {iteration}: {run}");
            runs.push(run);
        }
        let mut med = serde_json::Map::new();
        for key in TOP_KEYS {
            med.insert((*key).into(), serde_json::json!(bench_median(&runs, key)));
        }
        for (phase, keys) in [
            ("paged", PHASE_KEYS),
            ("hires", PHASE_KEYS),
            ("strip", STRIP_KEYS),
        ] {
            if runs.iter().all(|run| run.get(phase).is_some()) {
                let phase_runs: Vec<serde_json::Value> =
                    runs.iter().map(|run| run[phase].clone()).collect();
                let mut phase_med = serde_json::Map::new();
                for key in keys {
                    phase_med
                        .insert((*key).into(), serde_json::json!(bench_median(&phase_runs, key)));
                }
                med.insert(phase.into(), serde_json::Value::Object(phase_med));
            }
        }
        report.insert(label.into(), serde_json::Value::Object(med));
    }
    std::fs::write(
        output.join("bench.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(report))?,
    )?;
    println!("BENCH: {}", output.join("bench.json").display());
    Ok(())
}
