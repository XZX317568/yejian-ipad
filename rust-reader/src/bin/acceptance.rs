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
    /// 每帧推进的秒数；录制慢动作时调小，让飞行段在视频里占足够帧数。
    dt: f64,
    size: [u32; 2],
    output: PathBuf,
    repaint_delay: Duration,
    texts: Vec<(String, Rect)>,
    painted_text: Vec<PaintedText>,
    painted_images: Vec<PaintedImage>,
    /// 请求的原生像素倍率（注入 RawInput.viewport）；<=1 走默认 1×。
    ppp: f32,
    /// 上一帧 egui 实际输出的 pixels_per_point，用来断言真实 2×。
    rendered_ppp: f32,
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
            dt: 1. / 60.,
            size: [390, 844],
            output,
            repaint_delay: Duration::ZERO,
            texts: Vec::new(),
            painted_text: Vec::new(),
            painted_images: Vec::new(),
            ppp: 1.,
            rendered_ppp: 1.,
        })
    }
    fn frame(&mut self, events: Vec<Event>, name: Option<&str>) -> Result<()> {
        self.now += self.dt;
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(self.size[0] as f32, self.size[1] as f32),
            )),
            time: Some(self.now),
            events,
            ..Default::default()
        };
        if self.ppp > 1. {
            input
                .viewports
                .entry(input.viewport_id)
                .or_default()
                .native_pixels_per_point = Some(self.ppp);
        }
        let mut output = self.ctx.run_ui(input, |ui| self.app.draw(ui));
        self.rendered_ppp = output.pixels_per_point;
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
        let phys = [
            (self.size[0] as f32 * output.pixels_per_point).round() as u32,
            (self.size[1] as f32 * output.pixels_per_point).round() as u32,
        ];
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let descriptor = ScreenDescriptor {
            size_in_pixels: phys,
            pixels_per_point: output.pixels_per_point,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("yejian-acceptance"),
            size: wgpu::Extent3d {
                width: phys[0],
                height: phys[1],
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
        let stride = (phys[0] * 4).div_ceil(256) * 256;
        let buffer = name.map(|_| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture"),
                size: (stride * phys[1]) as u64,
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
                    width: phys[0],
                    height: phys[1],
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
            let mut pixels = Vec::with_capacity((phys[0] * phys[1] * 4) as usize);
            for row in mapped.chunks(stride as usize) {
                pixels.extend_from_slice(&row[..phys[0] as usize * 4]);
            }
            image::save_buffer(
                self.output.join(format!("{name}.png")),
                &pixels,
                phys[0],
                phys[1],
                image::ColorType::Rgba8,
            )?;
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        output.textures_delta.clear();
        Ok(())
    }
    /// 读取已落盘 PNG 的 RGBA 像素（宽 = h.size[0]）。
    fn captured_pixels(&self, name: &str) -> Result<Vec<u8>> {
        Ok(image::open(self.output.join(format!("{name}.png")))
            .with_context(|| format!("missing capture {name}"))?
            .to_rgba8()
            .into_raw())
    }
    /// 两帧在 [skip_top, rows-exclude_bottom) 行带内逐字节比较；跳过交互
    /// chrome（顶栏 hover、底栏滑块）——ghost 不复制这些。
    fn captured_equal(&self, a: &str, b: &str, skip_top: usize, exclude_bottom: usize) -> Result<bool> {
        let (pa, pb) = (self.captured_pixels(a)?, self.captured_pixels(b)?);
        let stride = self.size[0] as usize * 4;
        let from = (skip_top * stride).min(pa.len()).min(pb.len());
        let to = self.size[1] as usize - exclude_bottom;
        let to = (pa.len().min(pb.len()) / stride).min(to) * stride;
        Ok(from <= to && pa[from..to] == pb[from..to])
    }
    /// 行 y 上在 [x0,x1) 里最右一个非纸底像素列（|luma-bg|>5），无则 x0。
    fn right_edge(&self, name: &str, y: usize, x0: usize, x1: usize, bg: f32) -> Result<f32> {
        let px = self.captured_pixels(name)?;
        let w = self.size[0] as usize;
        for x in (x0..x1).rev() {
            let i = (y * w + x) * 4;
            let luma = (px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) as f32 / 3.;
            if (luma - bg).abs() > 5. {
                return Ok(x as f32);
            }
        }
        Ok(x0 as f32)
    }
    /// 一行 [x0,x1) 像素的平均亮度（0..255），摊开像素断言用。
    fn row_luma(&self, name: &str, y: usize, x0: usize, x1: usize) -> Result<f32> {
        let px = self.captured_pixels(name)?;
        let w = self.size[0] as usize;
        let mut sum = 0u64;
        for x in x0..x1 {
            let i = (y * w + x) * 4;
            sum += (px[i] as u64 + px[i + 1] as u64 + px[i + 2] as u64) / 3;
        }
        Ok(sum as f32 / (x1 - x0).max(1) as f32)
    }
    /// 合上中点帧采样：页区下方边距行，bands 内阅读器黑（~19）、
    /// bands 外详情页纸白（~248）。
    fn close_mid_samples(&self, name: &str) -> Result<(u8, u8)> {
        let px = self.captured_pixels(name)?;
        let (w, y) = (self.size[0] as usize, self.size[1] as usize - 180);
        let at = |x: usize| -> u8 {
            let i = (y * w + x) * 4;
            ((px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) / 3) as u8
        };
        Ok((at(w / 12), at(w * 5 / 6)))
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

/// 生成等尺寸 CBZ 夹具。titles 决定书名（去重键），palette_offset 给整套
/// 调色板移位——同名不同版或不同名同字节都会被 import 按内容 hash 归并。
fn fixtures(root: &Path, titles: &[&str], palette_offset: u8) -> Result<Vec<PathBuf>> {
    use std::io::Write;
    std::fs::create_dir_all(root)?;
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
                image::RgbImage::new(640, if index == 0 && page == 2 && titles.len() == 5 { 9000 } else { 960 });
            for (y, row) in image.enumerate_rows_mut() {
                for (x, _, pixel) in row {
                    let base = colors[index % 5].map(|c| c.wrapping_add(palette_offset * 61));
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

/// 原生 2× 下阅读器 tile 拼接取证：双色长页跨 2048px 瓦片缝，页内墨线远
/// 离接缝供自检「画内线 ≠ 拼缝线」；夹具只写本路隔离数据目录。
fn seam_fixture(path: &Path) -> Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().context("dir")?)?;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path)?);
    let mut img = image::RgbImage::new(640, 3000);
    for (y, row) in img.enumerate_rows_mut() {
        for (_, _, p) in row {
            let ink = (1995..1999).contains(&y) || (2068..2074).contains(&y);
            *p = image::Rgb(if ink { [218, 216, 204] } else if y < 2048 { [182, 95, 74] } else { [104, 88, 121] });
        }
    }
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)?;
    zip.start_file("001.png", zip::write::SimpleFileOptions::default())?;
    zip.write_all(&png.into_inner())?;
    zip.finish()?;
    Ok(path.to_path_buf())
}
/// 逐物理行扫 tile 相接处：记录吸附前逻辑边界与吸附后相接物理坐标，
/// 断言连接处不露画布底色/白底；墨线自检行不得被误报成缝。
fn seam_check(h: &Harness, name: &str, presnap: Rect) -> Result<serde_json::Value> {
    const A: [u8; 3] = [182, 95, 74];
    const B: [u8; 3] = [104, 88, 121];
    const INK: [u8; 3] = [218, 216, 204];
    let ppp = h.rendered_ppp;
    let shot = image::open(h.output.join(format!("{name}.png")))?.to_rgba8();
    let (w, px) = (shot.width() as usize, shot.into_raw());
    let at = |x: usize, y: usize| { let i = (y * w + x) * 4; [px[i], px[i + 1], px[i + 2]] };
    let near = |p: [u8; 3], c: [u8; 3], t: i32| p.iter().zip(c.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= t);
    let mut tiles: Vec<Rect> = h.painted_images.iter().map(|i| i.bounds)
        .filter(|r| presnap.expand(0.6).contains_rect(*r) && r.height() > 30.).collect();
    tiles.sort_by(|a, b| a.top().partial_cmp(&b.top()).unwrap());
    tiles.dedup_by(|a, b| a == b);
    anyhow::ensure!(tiles.len() >= 2, "{name}: expected tiled page, got {tiles:?}");
    let (x0, x1) = ((presnap.left() * ppp + 8.).round() as usize, (presnap.right() * ppp - 8.).round() as usize);
    let cols = x1 - x0;
    let fr = |v: f32| (v * ppp).fract().abs();
    let mut joins = Vec::new();
    for (i, t) in tiles.iter().enumerate().skip(1) {
        let edge = t.top();
        let adj = tiles[i - 1].bottom() - edge;
        anyhow::ensure!(adj.abs() < 0.25, "{name}: tiles not adjacent ({adj:.4}pt) {tiles:?}");
        let row = (edge * ppp).round() as usize;
        let mut leak = (0usize, 0usize);
        for row in row.saturating_sub(1)..=row + 1 {
            for x in x0..x1 {
                let p = at(x, row);
                if near(p, [0, 0, 0], 10)
                    || near(p, [26, 24, 21], 10)
                    || p.iter().all(|&c| c >= 235)
                {
                    leak.0 += 1;
                }
                else if !near(p, A, 40) && !near(p, B, 40) { leak.1 += 1; }
            }
        }
        anyhow::ensure!(leak.0 == 0 && leak.1 * 4 <= cols * 3,
            "{name}: tile join leaks canvas/blend at {edge:.2}pt row {row}px: {leak:?}");
        let mid = (x0 + x1) / 2;
        anyhow::ensure!(near(at(mid, row - 2), A, 40) && near(at(mid, row + 2), B, 40),
            "{name}: join row {row} not A->B: {:?}/{:?}", at(mid, row - 2), at(mid, row + 2));
        joins.push(serde_json::json!({"edge_pt": edge, "edge_px": edge * ppp, "adj_gap_pt": adj, "row_px": row, "leak": leak}));
    }
    let ink = [1997., 2071.].iter().filter(|sy| {
        let i = ((*sy / 2048.) as usize).min(tiles.len() - 1);
        let (base, span) = (i as f32 * 2048., (3000. - i as f32 * 2048.).min(2048.));
        let r = ((tiles[i].top() + (*sy - base) / span * tiles[i].height()) * ppp).round() as usize;
        (x0..x1).filter(|&x| near(at(x, r), INK, 40)).count() * 2 > cols
    }).count();
    Ok(serde_json::json!({"png": format!("{name}.png"), "presnap_pt": [presnap.left(), presnap.top(), presnap.right(), presnap.bottom()],
        "presnap_px_frac": [fr(presnap.left()), fr(presnap.top()), fr(presnap.right()), fr(presnap.bottom())],
        "tiles_px": tiles.iter().map(|t| [t.left() * ppp, t.top() * ppp, t.right() * ppp, t.bottom() * ppp]).collect::<Vec<_>>(),
        "cols_px": [x0, x1], "joins": joins, "ink_lines_found": ink}))
}
/// 真实 2×（native ppp 注入）下阅读器分片拼接实渲取证；只走真实绘制入口。
fn seam_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    h.ppp = 2.;
    h.size = [821, 1181]; // 奇数宽 → 页矩形吸附后边缘落在 .5 物理像素上
    h.app.import_files(vec![seam_fixture(&library_root.join("seam-fixture").join("接缝量规.cbz"))?]);
    h.wait(|a| a.book_count() == 6)?;
    h.wait(ReaderApp::covers_loaded)?;
    for _ in 0..6 {
        if h.app.shelf_focus_book_title().as_deref() == Some("接缝量规") { break; }
        h.click("shelf-next")?;
        h.frames(30)?;
    }
    anyhow::ensure!(h.app.shelf_focus_book_title().as_deref() == Some("接缝量规"), "seam: fixture not focused");
    h.click("open-book")?;
    h.wait(|a| a.screen_name() == "detail")?;
    h.frames(70)?;
    h.click("read")?;
    h.wait(ReaderApp::page_loaded)?;
    for _ in 0..90 {
        if !h.app.frame_is_animating { break; }
        h.frames(1)?;
    }
    h.frame(vec![], Some("seam2x-paged"))?;
    anyhow::ensure!(h.rendered_ppp == 2., "seam: ppp requested 2, rendered {}", h.rendered_ppp);
    let rect = *h.app.hitboxes.get("reader-image").context("seam: no reader-image")?;
    let paged = seam_check(h, "seam2x-paged", rect)?;
    h.click("reader-settings")?;
    h.frames(5)?;
    h.click("reader-mode-vertical")?;
    h.frames(5)?;
    h.click("reader-options-close")?;
    h.frames(6)?;
    h.wait(ReaderApp::page_loaded)?;
    let strip = |h: &Harness| h.app.hitboxes
        .get(&format!("vertical-page-{}", h.app.current_page()))
        .copied().context("seam: no vertical page");
    let mut vertical = Vec::new();
    for take in 0..2 {
        if take > 0 {
            let c = pos2(h.size[0] as f32 / 2., h.size[1] as f32 / 2.);
            h.drag(c, c - vec2(0., 333.))?;
        }
        for _ in 0..8 {
            let rect = strip(h)?;
            let seam_y = rect.top() + rect.height() * 2048. / 3000.;
            if (160. ..h.size[1] as f32 - 240.).contains(&seam_y) { break; }
            let c = pos2(h.size[0] as f32 / 2., h.size[1] as f32 / 2.);
            h.drag(c, c + vec2(0., if seam_y > 940. { -560. } else { 560. }))?;
            h.frames(30)?;
        }
        let name = format!("seam2x-vertical-{take}");
        h.frame(vec![], Some(&name))?;
        vertical.push(seam_check(h, &name, strip(h)?)?);
    }
    std::fs::write(h.output.join("seam2x.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "requested_ppp": 2., "rendered_ppp": h.rendered_ppp, "logical_size_pt": h.size,
        "physical_buffer_px": [h.size[0] as f32 * 2., h.size[1] as f32 * 2.],
        "paged": paged, "vertical": vertical }))?)?;
    println!("PASS: reader tile joins pixel-tight at native 2x (seam2x.json)");
    Ok(())
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
    for _ in 0..6 {
        h.frame(
            vec![
                Event::PointerMoved(pos2(600., 400.)),
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

/// Clicks a label that may sit below the filter sheet's fold. `click_text`
/// only sees painted (unclipped) text, so long sheets need a few wheel ticks
/// before the target scrolls into view.
fn click_text_scrolled(h: &mut Harness, label: &str) -> Result<()> {
    for _ in 0..6 {
        if h.texts.iter().any(|(text, _)| text == label) {
            return h.click_text(label);
        }
        h.frame(
            vec![
                Event::PointerMoved(pos2(600., 500.)),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -240.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(2)?;
    }
    h.click_text(label)
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
        anyhow::ensure!(
            h.painted_text.iter().any(|text| {
                text.text == label
                    && rect.expand(0.5).contains_rect(text.bounds)
                    && text.clip.expand(0.5).contains_rect(rect)
            }),
            "design: {id} / {label} is clipped or detached from its touch target"
        );
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
    // 藏书一览网格已于 2026-11-23 下线（书架即主体）。改用书架焦点书验证
    // 「点哪本开哪本」：焦点书 → 详情 → 阅读，三处身份必须一致。
    h.click("shelf-next")?;
    h.frames(40)?;
    let focus_id = h
        .app
        .shelf_focus_book_id()
        .context("design: shelf focus book id is missing")?;
    // 焦点书标题直接取自数据层：旧"第一条高 >20 的文本"会命中「书架」标题。
    let focus_title = h
        .app
        .shelf_focus_book_title()
        .context("design: shelf focus book title is missing")?;
    h.click("shelf-center")?;
    // 60 帧（1s）：覆盖 motion::OPEN 0.72s 飞行 + detail_text_alpha 0.92
    // 淡入——B 参数下 40 帧时文字列仍 0 透明度，按钮被 set_opacity 禁用。
    h.frames(60)?;
    // 点封面进的是 Detail 屏幕：current_book_id 只认 Reader，身份核对
    // 走 detail_book_id + screen_name（84ce320 起此断言即恒假，本轮暴露）。
    anyhow::ensure!(
        h.app.screen_name() == "detail" && h.app.detail_book_id() == Some(focus_id.as_str()),
        "design: tapping the focused cover opened a different book (wanted {focus_id}, got {} {:?})",
        h.app.screen_name(),
        h.app.detail_book_id()
    );
    anyhow::ensure!(
        h.painted_text.iter().any(|text| text.text == focus_title),
        "design: detail page does not show the focused title {focus_title}"
    );
    h.read_from_beginning()?;
    h.wait(ReaderApp::page_loaded)?;
    anyhow::ensure!(
        h.app.current_book_id() == Some(focus_id.as_str()),
        "design: reading after detail opened a different book"
    );
    h.frame(vec![], None)?;
    h.click("reader-back")?;
    h.frames(5)?;
    // reader-back 回到进入点（Detail），再退一次才回书架。
    h.click("back")?;
    h.frames(60)?;
    h.click("shelf-next")?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    anyhow::ensure!(
        h.app.rendered_book_nodes < 45,
        "design: unbounded shelf rendering"
    );

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

    // S3 证据：chapter_links 按 book_id 去重——「查看全部」页脚要 ≥7 本入链
    // 的书：补 4 本夹具副本到 9 册，除焦点书与已读书外 7 本链成一部 7 话作品。
    // 导入会重置焦点——先记下当前焦点（未读、未链）再导入。
    let keep_ids = [
        focus_id.clone(),
        h.app.shelf_focus_book_id().context("design: lost shelf focus")?,
    ];
    let extra = fixtures(
        &h.output.join("series-fixtures"),
        &["合集卷一", "合集卷二", "合集卷三", "合集卷四"],
        1,
    )?;
    h.app.import_files(extra[..4].to_vec());
    h.wait(|app| app.book_count() == 9)?;
    let library = yejian_core::Library::open(library_root)?;
    let books = library.list_books("")?;
    let series: Vec<_> = books
        .iter()
        .filter(|book| !keep_ids.contains(&book.id))
        .take(7)
        .collect();
    anyhow::ensure!(series.len() == 7, "design: not enough books left for the series fixture");
    let series_ids: Vec<String> = series.iter().map(|book| book.id.clone()).collect();
    let meta = |n: f64| yejian_core::ChapterMetadata {
        source_id: "design.series".into(),
        manga_id: "design-series".into(),
        manga_title: "七话合集".into(),
        chapter_id: format!("design-ch{n:.0}"),
        chapter_title: format!("第 {n:.0} 话"),
        chapter_number: Some(n),
        volume_number: None,
    };
    for (idx, book) in series.iter().enumerate() {
        library.link_chapter(&book.id, &meta(idx as f64 + 1.))?;
    }
    h.app = ReaderApp::new(library_root.to_path_buf(), h.ctx.clone());
    h.wait(ReaderApp::ready)?;
    h.frames(40)?;
    h.wait(ReaderApp::covers_loaded)?;
    for _ in 0..4 {
        if h.app
            .shelf_focus_book_id()
            .is_some_and(|id| series_ids.contains(&id))
        {
            break;
        }
        h.click("shelf-next")?;
        h.frames(12)?;
    }
    anyhow::ensure!(
        h.app
            .shelf_focus_book_id()
            .is_some_and(|id| series_ids.contains(&id)),
        "design: could not focus the series volume"
    );
    h.click("open-book")?;
    h.frames(65)?;
    anyhow::ensure!(
        h.app.screen_name() == "detail"
            && h.app
                .detail_book_id()
                .is_some_and(|id| series_ids.iter().any(|s| s == id)),
        "design: series cover did not open its detail"
    );
    anyhow::ensure!(
        h.app.hitboxes.keys().filter(|k| k.starts_with("dchapter-")).count() == 6
            && h.app.hitboxes.contains_key("detail-chapters-all"),
        "design: inline chapter card is missing its rows or footer: {:?}",
        h.app.hitboxes.keys().collect::<Vec<_>>()
    );
    design_capture(h, "design-detail-chapters", &mut captures)?;
    // task-24 补证：834pt 横屏下卡片头/尾必有一端出屏（内容 ≈373pt > 可用窗），
    // 切到 1024×1366 竖幅把整卡收进首屏——前 6 话与「查看全部」同屏完整可读。
    // 若仍差一点则小步滚动补齐；拍完回到 1194×834 再走原 back 链路。
    h.size = [1024, 1366];
    h.frames(12)?;
    let wanted = |h: &Harness| {
        let fully = |name: &str| {
            h.painted_text.iter().any(|t| {
                t.text == name
                    && design_safe_rect(h).expand(0.5).contains_rect(t.bounds)
                    && t.clip.expand(0.5).contains_rect(t.bounds)
            })
        };
        fully("查看全部 7 话")
            && fully("章节 · 共 7 话")
            && (1..=6).all(|n| fully(&format!("第 {n} 话")))
    };
    // 滚轮指针必须落在正文滚动区（cover.bottom() 以下），否则事件落空。
    for _ in 0..8 {
        if wanted(h) {
            break;
        }
        h.frame(
            vec![
                Event::PointerMoved(pos2(600., 1150.)),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: vec2(0., -80.),
                    source: MouseWheelSource::Wheel,
                    modifiers: Modifiers::NONE,
                    phase: TouchPhase::Move,
                },
            ],
            None,
        )?;
        h.frames(4)?;
    }
    anyhow::ensure!(
        wanted(h),
        "design: cannot fit all 7 chapter rows plus the footer on one canvas"
    );
    design_capture(h, "design-detail-chapters-full", &mut captures)?;
    h.size = [1194, 834];
    h.frames(12)?;
    h.click("back")?;
    h.frames(60)?;
    // 焦点还回未链接的未读书：后续 iPad 段照旧拿到单卷详情。
    for _ in 0..4 {
        if h.app.shelf_focus_book_id().as_deref() == Some(keep_ids[1].as_str()) {
            break;
        }
        h.click("shelf-next")?;
        h.frames(12)?;
    }
    anyhow::ensure!(
        h.app.shelf_focus_book_id().as_deref() == Some(keep_ids[1].as_str()),
        "design: could not restore focus to the single-volume book"
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
    h.frames(14)?;
    let resume = format!("history-{read_id}");
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
    h.frames(14)?;
    for label in ["跟随系统", "浅色", "深色"] {
        anyhow::ensure!(
            design_label_visible(h, label),
            "design: theme choice {label} is clipped"
        );
    }
    design_capture(h, "design-settings", &mut captures)?;
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
    click_text_scrolled(h, "冒险")?;
    h.click_text("应用筛选")?;
    h.wait(|app| app.source_browser_report()["results"][0] == "筛选:冒险题材")?;
    h.frames(5)?;
    design_capture(h, "design-filtered-results", &mut captures)?;

    // Reopen the real saved settings, preserving the source and reading
    // history. Only the temporary acceptance library is changed.
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
    h.click("shelf-read")?;
    h.wait(ReaderApp::page_loaded)?;
    h.frame(vec![], None)?;
    h.click("reader-back")?;
    h.frames(5)?;
    anyhow::ensure!(
        h.app.screen_name() == "library",
        "design: direct reading lost its shelf return"
    );
    h.click("nav-history")?;
    h.frames(14)?;
    design_capture(h, "design-large-text-history", &mut captures)?;
    anyhow::ensure!(
        design_label_visible(h, "最近读过的故事，都在这里。"),
        "design: 320pt/200 percent history introduction is clipped"
    );
    design_touch(h, &resume, Some("继续"))?;
    let introduction = h
        .painted_text
        .iter()
        .find(|text| text.text == "最近读过的故事，都在这里。")
        .context("design: history introduction is missing")?;
    let visible_covers: Vec<_> = h
        .painted_images
        .iter()
        .map(|image| image.bounds.intersect(image.clip))
        .filter(|rect| rect.is_positive())
        .collect();
    anyhow::ensure!(
        !visible_covers.is_empty()
            && visible_covers
                .iter()
                .all(|rect| rect.top() >= introduction.bounds.bottom()),
        "design: large-text history rows overlap the wrapped introduction"
    );
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
    h.frames(14)?;
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
    library.set_setting("font-scale", "1")?;
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
    click_text_scrolled(h, "冒险")?;
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
    click_text_scrolled(h, "恋爱")?;
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
    click_text_scrolled(h, "✓ 热血")?;
    h.frames(2)?;
    click_text_scrolled(h, "− 热血")?;
    h.frames(2)?;
    click_text_scrolled(h, "治愈")?;
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
    set_insets_override(None);
    println!(
        "PASS: source listing + pagination + real WASM filtered result + cancel/reset + iPad/200 percent filters"
    );
    Ok(())
}

/// spec §7 像素断言：spread-mid 帧上采样斜边两侧竖条，并量边界斜率。
/// 斜扫期间 flight mesh 已隐藏，未揭示区 = 纸底色（~248）。
/// dir=+1：揭示边界 x_edge(y) = edge - slant·frac(y)，conservative band
/// 用 band 下缘——即边界是 staircase，逐 band 右跳 slant/bands。
/// 两个采样行落在封面卡艺术区（y∈[335,775]），避开圆角顶/底与标题行；
/// 标题行（y≈825）只断言未揭示侧=底色（R2：文字 alpha≈0 的穿帮锚点）。
/// Detail cover card on iPad portrait (ui::detail_cover_rect): rows are
/// derived from the card so both the mid-frame capture and the check itself
/// follow layout retunes. Returns (left, top, width, height).
fn detail_card_rect(w: f32, hgt: f32) -> (f32, f32, f32, f32) {
    let card_w = (hgt * 0.25).clamp(250., 320.);
    let card_h = card_w * 166. / 118.;
    let block = card_h + 220.;
    let card_top = 24. + 84. + (hgt - 84. - 24. - block).max(0.) * 0.40;
    let card_left = w / 2. - card_w / 2.;
    (card_left, card_top, card_w, card_h)
}

fn spread_mid_check(h: &Harness, p: f32) -> Result<()> {
    let area_top = 24.;
    let w = h.size[0] as f32;
    let hgt = h.size[1] as f32 - 24. - 20.;
    let slant = yejian::motion_spread_slant(w);
    let edge = yejian::motion_spread_edge(w, slant, p);
    let (dx, dy) = yejian::motion_spread_offset(p);
    anyhow::ensure!(
        dx < 0.,
        "spread_mid_check assumes dir=+1 (dx={dx}) — generalize before flipping spread.dir"
    );
    let (card_left, card_top, card_w, card_h) = detail_card_rect(w, hgt);
    // The visible copy is the shifted incoming plane: art rect is the card
    // translated by (dx, dy).
    let art_left = card_left + dx;
    let art_right = card_left + card_w + dx;
    let art_top = card_top + dy;
    let bands = 8f32.max((slant / 8.).ceil());
    // Conservative per-band edge in screen x for an absolute y row.
    let front = |y: f32| -> f32 {
        let frac = ((y - area_top) / hgt).clamp(0., 1.);
        edge - slant * (frac * bands).ceil() / bands
    };
    let bg = h.row_luma("spread-mid", 700, 930, 1000)?;
    // Two rows inside the art, well clear of the rounded top/bottom corners.
    let mut edges = Vec::new();
    for y in [art_top + card_h * 0.18, art_top + card_h * 0.82] {
        let f = front(y);
        anyhow::ensure!(
            f > art_left + 60. && f < art_right - 10.,
            "spread-mid: predicted front {f:.0}@y{y} outside card art [{art_left:.0},{art_right:.0}] — p={p} not mid-sweep"
        );
        // Revealed strip: well inside the sweep and inside the art — must
        // differ from paper (art + stroke + shadow).
        let rev_x = ((f - 50.) as usize).max(art_left as usize + 5);
        let rev = h.row_luma("spread-mid", y as usize, rev_x, rev_x + 24)?;
        anyhow::ensure!(
            (rev - bg).abs() > 12.,
            "spread-mid: revealed strip x={rev_x}..{} luma {rev:.0} ≈ paper {bg:.0} — incoming copy missing under the front",
            rev_x + 24
        );
        // Unrevealed strip: right of the staircase edge — must be clean paper.
        let unrev_x = (f + 30.) as usize;
        let unrev = h.row_luma("spread-mid", y as usize, unrev_x, unrev_x + 24)?;
        anyhow::ensure!(
            (unrev - bg).abs() <= 8.,
            "spread-mid: unrevealed strip x={unrev_x}..{} luma {unrev:.0} ≠ paper {bg:.0} — leaked content or wrong boundary",
            unrev_x + 24
        );
        // Pixel-measured edge: rightmost non-paper column in this row, inside art.
        let measured = h.right_edge("spread-mid", y as usize, art_left as usize, (f + 30.) as usize, bg)?;
        edges.push((y, measured));
    }
    // Slant check: measured boundary slope vs formula slant/h, ±15%.
    let ((y1, x1), (y2, x2)) = (edges[0], edges[1]);
    let slope = (x2 - x1).abs() / (y2 - y1);
    let expected = slant / hgt;
    anyhow::ensure!(
        (slope - expected).abs() / expected <= 0.15,
        "spread-mid: boundary slope {slope:.3} vs expected {expected:.3} (±15%) — slant angle wrong"
    );
    // R2 anchor: the text column is unclipped. Its right half sits in the
    // unrevealed region at p≈0.5; alpha≈0 must keep it at paper luma.
    let text_x = (front(825.) + 20.) as usize;
    let text = h.row_luma("spread-mid", 825, text_x, text_x + 40)?;
    anyhow::ensure!(
        (text - bg).abs() <= 8.,
        "spread-mid: text luma {text:.0} ≠ paper {bg:.0} at x={text_x} — unclipped text column ghosts into the unrevealed region"
    );
    println!("spread-mid: p={p:.3} edge={edge:.0} slope={slope:.3} bg={bg:.0} PASS");
    Ok(())
}

fn ipad_pass(h: &mut Harness, library_root: &Path) -> Result<()> {
    use yejian_platform::{Insets, set_insets_override};
    // iPad window sizes in points (one harness pixel per point), plus the
    // narrow Split View widths and a no-inset control.
    let profiles: [([u32; 2], i32, i32, &str); 10] = [
        ([1024, 1366], 24, 20, "ipad-pro-129-portrait"),
        ([1366, 1024], 24, 20, "ipad-pro-129-landscape"),
        ([834, 1194], 24, 20, "ipad-11-portrait"),
        ([1194, 834], 24, 20, "ipad-11-landscape"),
        ([820, 1180], 24, 20, "ipad-air5-portrait"),
        ([1180, 820], 24, 20, "ipad-air5-landscape"),
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
        for (tab, label) in [
            ("search", "sources"),
            ("settings", "settings"),
            ("history", "history"),
        ] {
            h.click(&format!("nav-{tab}"))?;
            // 14 frames let the nav pill's 0.2s tween settle, so captures
            // never show the highlight straddling two tabs (P1-4).
            h.frames(14)?;
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
        if name == "portrait" {
            // 摊开中点帧（spec §7 像素断言）：斜扫前缘扫到卡艺横向中心的帧。
            // r2=B 参数下 p 轴已平移（start .35 / shift_x 40），裸 p>=0.5 时
            // 前缘已越过卡片右缘，故以卡艺几何定位中点而非固定进度阈值。
            // 斜扫期间 flight mesh 隐藏，未揭示区 = 纸底色。
            let w = size[0] as f32;
            let hgt = size[1] as f32 - 24. - 20.;
            let slant = yejian::motion_spread_slant(w);
            let bands = 8f32.max((slant / 8.).ceil());
            let (card_left, card_top, card_w, card_h) = detail_card_rect(w, hgt);
            let mid_y = card_top + card_h * 0.5;
            let mut mid = false;
            let mut mid_p = 0f32;
            for i in 1..=45usize {
                let p = yejian::motion_spread_progress(
                    (i as f64 * h.dt / yejian::motion_open_seconds()).min(1.) as f32,
                );
                // incoming 平面位移随 p 收敛，卡艺中心跟着 dx 左移。
                let (dx, dy) = yejian::motion_spread_offset(p);
                let edge = yejian::motion_spread_edge(w, slant, p);
                let frac = ((mid_y + dy - 24.) / hgt).clamp(0., 1.);
                let front = edge - slant * (frac * bands).ceil() / bands;
                let capture = !mid && front >= card_left + card_w * 0.5 + dx;
                if capture {
                    mid = true;
                    mid_p = p;
                }
                h.frame(vec![], capture.then_some("spread-mid"))?;
            }
            anyhow::ensure!(mid, "spread sweep never crossed the card centre");
            h.frames(20)?;
            spread_mid_check(h, mid_p)?;
        } else {
            h.frames(65)?;
        }
        h.frame(vec![], Some(&format!("ipad-pro-129-{name}-detail")))?;
        h.click("read")?;
        h.wait(ReaderApp::page_loaded)?;
        h.frames(6)?;
        h.frame(vec![], Some(&format!("ipad-pro-129-{name}-reader")))?;
        if name == "portrait" {
            // 阶段 3 flip evidence: hold a 0.5-width drag so the turn-edge
            // depth ridge is visible, then release into the committed turn.
            let grab = pos2(size[0] as f32 - 80., size[1] as f32 / 2.);
            h.frame(
                vec![
                    Event::PointerMoved(grab),
                    Event::PointerButton {
                        pos: grab,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Modifiers::NONE,
                    },
                ],
                None,
            )?;
            let mut held = grab;
            for _ in 0..8 {
                held.x -= 56.;
                h.frame(vec![Event::PointerMoved(held)], None)?;
            }
            h.frame(
                vec![Event::PointerMoved(held - vec2(56., 0.))],
                Some("ipad-pro-129-portrait-flip-mid"),
            )?;
            h.frame(
                vec![Event::PointerButton {
                    pos: held - vec2(56., 0.),
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                None,
            )?;
            h.wait(ReaderApp::page_loaded)?;
            h.frames(4)?;
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
        if name == "portrait" {
            // 合上中点帧（p 首个 <=0.55 的 exit 帧）：半屏阅读器、半屏详情。
            h.frame(vec![], Some("close-pre"))?;
            let back_pos = h
                .app
                .hitboxes
                .get("reader-back")
                .with_context(|| "reader-back control missing")?
                .center();
            let btn = |pressed: bool| Event::PointerButton {
                pos: back_pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            h.frame(vec![Event::PointerMoved(back_pos), btn(true)], None)?;
            h.frame(vec![btn(false)], Some("close-p1"))?;
            let exit_s = yejian::motion_close_seconds();
            let mut mid = false;
            for i in 1..=24usize {
                let p = yejian::motion_close_p(((i as f64) * h.dt / exit_s.max(0.001)).min(1.) as f32);
                h.frame(vec![], (!mid && p <= 0.55).then(|| { mid = true; "close-mid" }))?;
            }
        } else {
            h.click("reader-back")?;
            h.frames(24)?;
        }
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
            h.frames(14)?;
            h.frame(
                vec![],
                Some(&format!("ipad-split-{name}-{label}-large-text")),
            )?;
            // The nav pill must not touch the bar edge (>=6pt gap).
            let pill = h.app.hitboxes["nav-pill"];
            anyhow::ensure!(
                pill.left() >= 6. && size[0] as f32 - pill.right() >= 6.,
                "{name}: nav pill too close to bar edge: {pill:?}"
            );
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
    set_insets_override(None);
    println!(
        "PASS: iPad profiles rendered with real UI at 1024x1366, 1366x1024, 1194x834, 820x1180, 1180x820, 744x1133, 507x1024 and 320x1024"
    );
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
    h.key(Key::BrowserBack)?;
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
        h.click(&format!("history-{}", two.id))?;
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

/// 裁决素材专用：iPad 竖屏下的「抽出 → 摊开 → 合上」连续帧。
/// 默认全量 pass 的 motion 帧是手机尺寸（390×844），对 iPad 打磨没有参考价值。
fn record_pass(h: &mut Harness) -> Result<()> {
    h.size = [1024, 1366];
    h.frames(8)?;
    let motion_frames: usize = std::env::var("YEJIAN_MOTION_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n >= 300)
        .unwrap_or(310);
    // 帧分配：书架滚动 ≤60 帧（60fps 实速）→ 摊开飞行（慢动作）→ 阅读器段
    //（稳定 → 拖拽翻页 → 稳定 → reader-back 合上，全程 1/120 二倍慢动作）。
    // motion::OPEN ≈ 0.72s：1/180 推进 ×131 帧 ≈ 0.73s 恰好覆盖全程，且
    // spread-mid 落在与 1/240 录制相同的 t≈0.70s 中点斜扫帧。
    // 阅读器段 = hold 14 + 拖拽翻页 9 + 翻页补间 24 + hold 14 + 合上 ≥57；
    // 合上要覆盖最长 exit_ms=420ms（1/120 下 ≈50.4 帧），总帧数低于 ~300
    // 时余量不足，所以 YEJIAN_MOTION_FRAMES <300 一律回退默认 310。
    let scroll_n = (motion_frames / 4).min(60);
    // 阅读器段（翻页 + 合上）保底 118 帧；摊开段吃剩余帧数，下限 131。
    let open_n = (motion_frames - scroll_n - 118).max(131);
    let reader_n = motion_frames - scroll_n - open_n;
    std::fs::create_dir_all(h.output.join("motion"))?;
    let at = |i: usize| format!("motion/frame-{i:03}");
    h.click("shelf-next")?;
    for i in 0..scroll_n {
        h.frame(vec![], Some(&at(i)))?;
    }
    h.wait(ReaderApp::covers_loaded)?;
    h.click("shelf-center")?;
    h.dt = 1. / 180.;
    for i in 0..open_n {
        // 留一份命名证明帧，但必须另存——R3 直接把帧改名，motion 序号断在
        // frame-193，ffmpeg 只编码到 192 帧。
        let name = at(i + scroll_n);
        h.frame(vec![], Some(&name))?;
        if i == 91 {
            std::fs::copy(
                h.output.join(format!("{name}.png")),
                h.output.join("spread-mid.png"),
            )?;
        }
    }
    h.dt = 1. / 60.;
    // 摊开收尽后进阅读器（entry peel 不录——帧数留给稳定态）；等页面与
    // 翻页补间全走完拿到合上基线，再录 ~30 帧阅读器稳定段。
    h.wait(|a| a.screen_name() == "detail")?;
    h.click("read")?;
    h.dt = 1. / 120.;
    h.wait(ReaderApp::page_loaded)?;
    for _ in 0..90 {
        if !h.app.frame_is_animating {
            break;
        }
        h.frames(1)?;
    }
    anyhow::ensure!(
        !h.app.frame_is_animating,
        "record: reader still animating before close"
    );
    // ~14 帧稳定阅读器（2 倍慢放），让裁决能看清 entry 收尾后的静止页。
    let hold1_n = 14.min(reader_n / 8);
    let mut next = scroll_n + open_n;
    for i in 0..hold1_n {
        h.frame(vec![], Some(&at(next + i)))?;
    }
    next += hold1_n;
    // 翻页段：手动拖拽右缘向左 ~9 帧（drag 跟手阶段 flip_depth 隆起带全程
    // 可见），释放帧记为 turn 首帧 → 提交 next_page → 补间 0.18s。
    // 触发方式沿用全量验收的 flip-mid 证据手法（grab 右缘逐帧 PointerMoved），
    // 不用 tap：tap 无法让隆起带拖在手指下慢放。
    let before_page = h.app.current_page();
    let canvas = h
        .app
        .hitboxes
        .get("reader-canvas")
        .copied()
        .with_context(|| "record: reader-canvas control missing")?;
    let grab = pos2(canvas.right() - 24., canvas.center().y);
    // 按下帧不录（与 close 释放帧同理，release 帧才是段首帧）。
    h.frame(
        vec![
            Event::PointerMoved(grab),
            Event::PointerButton {
                pos: grab,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        None,
    )?;
    let mut held = grab;
    for i in 0..8 {
        held.x -= 56.;
        h.frame(vec![Event::PointerMoved(held)], Some(&at(next + i)))?;
    }
    next += 8;
    h.frame(
        vec![Event::PointerButton {
            pos: held,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Some(&at(next)),
    )?;
    next += 1;
    anyhow::ensure!(
        h.app.current_page() > before_page,
        "record: drag did not commit a page turn ({before_page} -> {})",
        h.app.current_page()
    );
    // 翻页补间 turn_ms=180ms：1/120 推进约 21.6 帧，录 24 帧覆盖收束。
    let turn_n = 24.min(reader_n / 4);
    for i in 0..turn_n {
        h.frame(vec![], Some(&at(next + i)))?;
    }
    next += turn_n;
    // 等补间/翻页余波彻底收尽，再录 ~14 帧稳定页，然后进入合上段。
    h.wait(ReaderApp::page_loaded)?;
    for _ in 0..90 {
        if !h.app.frame_is_animating {
            break;
        }
        h.frames(1)?;
    }
    anyhow::ensure!(
        !h.app.frame_is_animating,
        "record: reader still animating after turn"
    );
    let hold2_n = 14.min(reader_n / 8);
    for i in 0..hold2_n {
        h.frame(vec![], Some(&at(next + i)))?;
    }
    next += hold2_n;
    // back() fires on pointer release: the release frame is the first
    // peel-exit frame (p=1), so press goes unrecorded and release opens
    // the close segment — same hand-off the full pass names close-p1.
    let back_pos = h
        .app
        .hitboxes
        .get("reader-back")
        .with_context(|| "record: reader-back control missing")?
        .center();
    h.frame(
        vec![
            Event::PointerMoved(back_pos),
            Event::PointerButton {
                pos: back_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        None,
    )?;
    h.frame(
        vec![Event::PointerButton {
            pos: back_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Some(&at(next)),
    )?;
    next += 1;
    anyhow::ensure!(
        h.app.reader_exit_active(),
        "record: reader-back armed no close exit"
    );
    // 剩余 ~57 帧 ×1/120 ≈ 0.48s，覆盖最长的 exit_ms(420) 加少量详情稳定。
    let close_anim_n = motion_frames - next;
    for i in 0..close_anim_n {
        h.frame(vec![], Some(&at(next + i)))?;
    }
    anyhow::ensure!(
        !h.app.reader_exit_active(),
        "record: close exit did not finish"
    );
    anyhow::ensure!(
        h.app.screen_name() == "detail",
        "record: close did not land back on detail"
    );
    h.dt = 1. / 60.;
    println!(
        "RECORD: {motion_frames} frames at 1024x1366 (scroll {scroll_n} / open {open_n} / reader {reader_n}, holds {hold1_n}+{hold2_n}, turn {turn_n}, close {close_anim_n})"
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
    let paths = fixtures(
        &data.join("fixtures"),
        &["山海之间", "深蓝来信", "雨后列车", "小镇回声", "月光旅人"],
        0,
    )?;
    let mut h = Harness::new(data.join("library"), output.clone())?;
    // 阶段 0 闸门：先确认纸张真的在弯。没有这条，把曲率参数改坏也能全绿。
    {
        let mut peak = 0f32;
        for i in 0..=80 {
            let t = i as f32 / 80.;
            for u in [0., 0.25, 0.5, 0.75, 1.] {
                for v in [0., 0.25, 0.5, 0.75, 1.] {
                    peak = peak.max(yejian::motion_paper_depth(t, u, v));
                }
            }
        }
        anyhow::ensure!(
            (12.0..=30.0).contains(&peak),
            "paper lift peak {peak:.2} outside 12..=30 — params.json 被改坏或曲率回归"
        );
        println!("paper lift peak: {peak:.2}");
    }
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
    if std::env::var_os("YEJIAN_FILTERS_ONLY").is_some() {
        return filters_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_IPAD_ONLY").is_some() {
        return ipad_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_DESIGN_ONLY").is_some() {
        return design_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_DAILY_ONLY").is_some() {
        return daily_pass(&mut h, &data.join("library"));
    }
    if std::env::var_os("YEJIAN_RECORD_ONLY").is_some() {
        return record_pass(&mut h);
    }
    if std::env::var_os("YEJIAN_SEAM_ONLY").is_some() {
        return seam_pass(&mut h, &data.join("library"));
    }
    h.frame(vec![], Some("phone-shelf"))?;
    anyhow::ensure!(h.app.rendered_book_nodes < 45);
    anyhow::ensure!(h.texts.iter().any(|(text, _)| text == "开始阅读"));
    // 录制长度由 YEJIAN_MOTION_FRAMES 控制（默认 100 帧 ≈ 2.7s@60fps）。
    // 裁决用的对比视频设 240 帧，30fps 播放约 8 秒。
    let motion_frames: usize = std::env::var("YEJIAN_MOTION_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n >= 100)
        .unwrap_or(100);
    let scroll_n = motion_frames * 35 / 100;
    let open_n = motion_frames - scroll_n;
    h.click("shelf-next")?;
    for i in 0..scroll_n {
        h.frame(vec![], Some(&format!("motion/frame-{i:03}")))?;
    }
    h.wait(ReaderApp::covers_loaded)?;
    h.frame(vec![], Some("phone-selected"))?;
    h.click("shelf-center")?;
    for i in 0..open_n {
        let name = format!("motion/frame-{:03}", i + scroll_n);
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
        let name = x > 144.5 && x < 145.5;
        h.frame(
            vec![Event::PointerMoved(pos2(x, 420.))],
            name.then_some("reader-flip-mid"),
        )?;
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
    // 阶段4「合上」：先等翻页补间走完拿到静态基线帧，再发 back。
    // close-p1（release 帧=p=1 叠加层首帧）页区应逐字节等于基线；
    // close-commit（exit 收尽的下一帧）应等于 close-end 直切帧；
    // close-mid 采样 p 首个 <=0.55 的帧，bands 内黑、bands 外纸白。
    for _ in 0..90 {
        if !h.app.frame_is_animating { break; }
        h.frames(1)?;
    }
    anyhow::ensure!(!h.app.frame_is_animating, "reader still animating at close baseline");
    h.frame(vec![], Some("close-pre"))?;
    // back() fires mid-frame on pointer release, so the release frame is
    // the first overlay frame (p=1); name it by releasing manually.
    let back_pos = h
        .app
        .hitboxes
        .get("reader-back")
        .with_context(|| "reader-back control missing")?
        .center();
    h.frame(
        vec![
            Event::PointerMoved(back_pos),
            Event::PointerButton { pos: back_pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
        ],
        None,
    )?;
    h.frame(
        vec![Event::PointerButton { pos: back_pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }],
        Some("close-p1"),
    )?;
    anyhow::ensure!(h.app.reader_exit_active(), "reader-back armed no close exit");
    anyhow::ensure!(
        h.captured_equal("close-p1", "close-pre", 56, 80)?,
        "exit first frame differs from the static reader frame",
    );
    let exit_s = yejian::motion_close_seconds();
    let (mut mid_done, mut commit_done) = (false, false);
    for i in 1..=40usize {
        let p = yejian::motion_close_p(((i as f64) * h.dt / exit_s.max(0.001)).min(1.) as f32);
        let name = if !mid_done && p <= 0.55 {
            mid_done = true;
            Some("close-mid")
        } else if !commit_done && p <= 0.002 {
            commit_done = true;
            Some("close-commit")
        } else {
            None
        };
        h.frame(vec![], name)?;
        if commit_done && !h.app.reader_exit_active() { break; }
    }
    h.frame(vec![], Some("close-end"))?;
    anyhow::ensure!(!h.app.reader_exit_active(), "close exit did not finish");
    anyhow::ensure!(mid_done && commit_done, "close frames not captured");
    anyhow::ensure!(
        h.captured_equal("close-commit", "close-end", 0, 0)?,
        "exit last frame differs from the direct-cut target frame",
    );
    let (dark_side, paper_side) = h.close_mid_samples("close-mid")?;
    anyhow::ensure!(
        dark_side < 90 && paper_side > 140,
        "mid-exit frame lacks the two-screen split: dark={dark_side} paper={paper_side}",
    );
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
        (820, 1180, "ipad-air5-portrait-shelf"),
        (1180, 820, "ipad-air5-landscape-shelf"),
    ] {
        h.size = [width, height];
        h.frames(6)?;
        h.frame(vec![], Some(name))?;
        if name.starts_with("ipad-air5") {
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(width as f32, height as f32));
            let open = *h
                .app
                .hitboxes
                .get("open-book")
                .with_context(|| format!("{name}: open-book action is missing"))?;
            anyhow::ensure!(
                screen.contains_rect(open),
                "{name}: open-book action is outside the {width}x{height} window"
            );
            h.click("open-book")?;
            h.frames(65)?;
            anyhow::ensure!(
                h.app.screen_name() == "detail",
                "{name}: open-book did not reach the detail screen"
            );
            let read = *h
                .app
                .hitboxes
                .get("read")
                .with_context(|| format!("{name}: detail read action is missing"))?;
            anyhow::ensure!(
                screen.contains_rect(read),
                "{name}: primary reader action leaves the {width}x{height} window"
            );
            let detail = name.replace("-shelf", "-detail");
            h.frame(vec![], Some(detail.as_str()))?;
            h.click("back")?;
            h.frames(90)?;
        }
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
    h.frames(14)?;
    h.frame(vec![], Some("history"))?;
    h.click("nav-settings")?;
    h.frames(14)?;
    h.frame(vec![], Some("settings"))?;
    h.size = [1366, 1024];
    h.frames(6)?;
    h.click("nav-search")?;
    h.frames(14)?;
    h.frame(vec![], Some("ipad-landscape-sources"))?;
    h.click("nav-history")?;
    h.frames(14)?;
    h.frame(vec![], Some("ipad-landscape-history"))?;
    h.click("nav-settings")?;
    h.frames(14)?;
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
