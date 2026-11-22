






use egui::*;



use std::collections::HashSet;
use std::collections::VecDeque;


use std::path::Path;
use std::path::PathBuf;










use yejian_sources::*;
use super::*;
use super::engine::request;
use super::downloads::valid_task_id;

pub(super) fn plan_covers(queue: &mut VecDeque<CoverReq>, want: Vec<CoverReq>, skip: &HashSet<String>) {
    let wanted: HashSet<&str> = want.iter().map(|req| req.key.as_str()).collect();
    queue.retain(|req| wanted.contains(req.key.as_str()));
    for req in want {
        if skip.contains(&req.key) || queue.iter().any(|queued| queued.key == req.key) {
            continue;
        }
        queue.push_back(req);
    }
    while queue.len() > COVER_QUEUE_CAP {
        queue.pop_front();
    }
}

pub(super) const COVER_QUEUE_CAP: usize = 24;

pub(super) const COVER_INFLIGHT_CAP: usize = 8;

pub(super) const COVER_TEXTURE_CAP: usize = 120;

pub(super) const COVER_MAX_HEIGHT: u32 = 360;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoverReq {
    pub(super) key: String,
    pub(super) source: String,
    pub(super) url: String,
}

pub(super) struct CoverHttpJob {
    pub(super) key: String,
    pub(super) root: PathBuf,
    pub(super) handler: RequestHandler,
    pub(super) url: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Option<Vec<u8>>,
}

pub(super) struct CoverDone {
    pub(super) key: String,
    pub(super) image: Option<ColorImage>,
}

pub(super) fn follow_cover_path(root: &Path, snapshot: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(snapshot), "非法的封面 id：{snapshot:?}");
    Ok(root.join("follow-covers").join(format!("{snapshot}.img")))
}

pub(super) fn cover_key(source: &str, url: &str) -> String {
    identity_id(source, url, "cover")
}

pub(super) fn source_cover_path(root: &Path, key: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(key), "非法的封面 id：{key:?}");
    Ok(root.join("source-covers").join(format!("{key}.img")))
}

pub(super) fn shrink_cover(image: image::DynamicImage) -> image::DynamicImage {
    if image.width() > 240 || image.height() > COVER_MAX_HEIGHT {
        image.thumbnail(240, COVER_MAX_HEIGHT)
    } else {
        image
    }
}

pub(super) fn store_source_cover(path: &Path, image: &image::DynamicImage) -> anyhow::Result<ColorImage> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("封面路径缺少父目录"))?;
    std::fs::create_dir_all(parent)?;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png)?;
    let part = path.with_extension("part");
    std::fs::write(&part, encoded.get_ref())?;
    std::fs::rename(&part, path)?;
    let rgba = image.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

/// copy skips the network entirely on later launches.
#[cfg(test)]
pub(super) fn fetch_source_cover(
    root: &Path,
    engine: &SourceEngine,
    source: &str,
    url: &str,
) -> anyhow::Result<ColorImage> {
    let key = cover_key(source, url);
    let path = source_cover_path(root, &key)?;
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(image) = image::load_from_memory(&bytes) {
            return store_source_cover(&path, &image);
        }
    }
    let handler = engine.request_handler();
    let bytes = match pollster::block_on(engine.image_request(url, None))? {
        Some(req) => request(&handler, req.url, req.headers, req.body)?,
        None => request(&handler, url.to_string(), vec![], None)?,
    };
    let image = shrink_cover(image::load_from_memory(&bytes)?);
    store_source_cover(&path, &image)
}

/// Returns the cache key and decoded thumbnail for the UI's `cover_ready`.
pub(super) fn fetch_follow_cover(
    root: &Path,
    engine: &SourceEngine,
    manga: &Manga,
) -> anyhow::Result<(String, ColorImage)> {
    let url = manga
        .cover
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no cover url"))?;
    let key = cover_key(&manga.source_key, url);
    let path = source_cover_path(root, &key)?;
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(image) = image::load_from_memory(&bytes) {
            let rgba = image.to_rgba8();
            return Ok((
                key,
                ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    rgba.as_raw(),
                ),
            ));
        }
    }
    let handler = engine.request_handler();
    let bytes = match pollster::block_on(engine.image_request(url, None))? {
        Some(req) => request(&handler, req.url, req.headers, req.body)?,
        None => request(&handler, url.to_string(), vec![], None)?,
    };
    let image = shrink_cover(image::load_from_memory(&bytes)?);
    let color = store_source_cover(&path, &image)?;
    Ok((key, color))
}

impl Sources {
    /// Cover texture for a followed work via the `source-covers` pipeline;
    /// queues a fetch while it is still missing.
    pub(crate) fn work_cover(
        &mut self,
        ctx: &Context,
        source: &str,
        manga: &str,
    ) -> Option<TextureHandle> {
        let index = self.follow_index(source, manga)?;
        let url = self.follow_cover_url(index)?;
        let texture = self.cover_image(ctx, source, &url);
        if texture.is_none() {
            self.plan_covers(vec![CoverReq {
                key: cover_key(source, &url),
                source: source.to_string(),
                url,
            }]);
        }
        texture
    }
    /// has no cover, which also avoids a per-frame file read.
    pub(super)fn follow_cover_url(&mut self, index: usize) -> Option<String> {
        let snapshot = self.follows.get(index)?.snapshot.clone();
        if let Some(url) = self.follow_cover_urls.get(&snapshot) {
            return url.clone();
        }
        let url = manga_snapshot_path(&self.root, &snapshot)
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| Manga::from_postcard_bytes(&bytes).ok())
            .and_then(|manga| manga.cover.filter(|url| !url.is_empty()));
        self.follow_cover_urls.insert(snapshot, url.clone());
        url
    }
    pub(super)fn cover_texture(&mut self, ctx: &Context, snapshot: &str) -> Option<TextureHandle> {
        // Covers saved before the `source-covers` unification stay readable;

        // nothing is ever written under `follow-covers/` any more.

        if let Some(cached) = self.covers.get(snapshot) {
            return cached.clone();
        }
        let texture = follow_cover_path(&self.root, snapshot)
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| image::load_from_memory(&bytes).ok())
            .map(|image| {
                let image = image.thumbnail(120, 160).to_rgba8();
                let size = [image.width() as usize, image.height() as usize];
                ctx.load_texture(
                    format!("follow-cover-{snapshot}"),
                    ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    TextureOptions::LINEAR,
                )
            });
        self.covers.insert(snapshot.to_string(), texture.clone());
        texture
    }
    pub(super)fn cover_image(&mut self, ctx: &Context, source: &str, url: &str) -> Option<TextureHandle> {
        if url.is_empty() {
            return None;
        }
        let key = cover_key(source, url);
        if let Some(texture) = self.cover_textures.get(&key) {
            let texture = texture.clone();
            self.cover_lru.retain(|entry| entry != &key);
            self.cover_lru.push_back(key);
            return Some(texture);
        }
        let image = match self.cover_ready.remove(&key) {
            Some(image) => image.clone(),
            None if self.cover_missing_disk.contains(&key) => None,
            None => {
                let image = source_cover_path(&self.root, &key)
                    .ok()
                    .and_then(|path| std::fs::read(path).ok())
                    .and_then(|bytes| image::load_from_memory(&bytes).ok())
                    .map(|image| {
                        let rgba = image.to_rgba8();
                        ColorImage::from_rgba_unmultiplied(
                            [rgba.width() as usize, rgba.height() as usize],
                            rgba.as_raw(),
                        )
                    });
                if image.is_none() {
                    self.cover_missing_disk.insert(key.clone());
                }
                image
            }
        }?;
        let texture =
            ctx.load_texture(format!("source-cover-{key}"), image, TextureOptions::LINEAR);
        self.cover_textures.insert(key.clone(), texture.clone());
        self.cover_lru.push_back(key);
        while self.cover_textures.len() > COVER_TEXTURE_CAP {
            match self.cover_lru.pop_front() {
                Some(evicted) => {
                    self.cover_textures.remove(&evicted);
                }
                None => break,
            }
        }
        Some(texture)
    }
    pub(super)fn plan_covers(&mut self, want: Vec<CoverReq>) {
        let skip: HashSet<String> = self
            .cover_inflight
            .iter()
            .chain(self.cover_failed.iter())
            .cloned()
            .collect();
        plan_covers(&mut self.cover_queue, want, &skip);
        while self.cover_outstanding < COVER_INFLIGHT_CAP {
            let Some(req) = self.cover_queue.pop_back() else {
                break;
            };
            if self.cover_req_tx.send(req.clone()).is_err() {
                break;
            }
            self.cover_inflight.insert(req.key);
            self.cover_outstanding += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;

    #[test]
    fn cover_queue_tracks_only_the_visible_window() {
        let req = |key: &str| CoverReq {
            key: key.into(),
            source: "s".into(),
            url: format!("https://img/{key}"),
        };
        let keys = |queue: &VecDeque<CoverReq>| {
            queue.iter().map(|req| req.key.clone()).collect::<Vec<_>>()
        };
        let skip = HashSet::from(["in-flight".to_string(), "failed".to_string()]);
        let mut queue = VecDeque::new();
        plan_covers(
            &mut queue,
            vec![req("a"), req("b"), req("in-flight"), req("failed")],
            &skip,
        );
        assert_eq!(
            keys(&queue),
            ["a", "b"],
            "sent and failed covers never queue"
        );
        plan_covers(&mut queue, vec![req("a"), req("c"), req("d")], &skip);
        assert_eq!(
            keys(&queue),
            ["a", "c", "d"],
            "scrolled-away cells drop out"
        );
        let many: Vec<CoverReq> = (0..30).map(|i| req(&format!("k{i}"))).collect();
        plan_covers(&mut queue, many, &skip);
        assert_eq!(queue.len(), COVER_QUEUE_CAP);
        assert_eq!(queue.back().unwrap().key, "k29", "newest requests win");
        assert!(
            !queue.iter().any(|req| req.key == "a"),
            "oldest drops first"
        );
    }
    #[test]
    fn cover_key_is_stable_and_scoped() {
        assert_eq!(cover_key("s", "u"), cover_key("s", "u"));
        assert_ne!(cover_key("a", "u"), cover_key("b", "u"));
        assert_ne!(cover_key("s", "u1"), cover_key("s", "u2"));
        assert!(source_cover_path(Path::new("root"), &cover_key("s", "u")).is_ok());
    }
    #[test]
    fn source_cover_store_writes_atomically_and_reads_back() {
        let root = tempfile::tempdir().unwrap();
        let path = source_cover_path(root.path(), &cover_key("s", "u")).unwrap();
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ));
        let decoded = store_source_cover(&path, &image).unwrap();
        assert_eq!(decoded.size, [8, 8]);
        assert!(path.is_file());
        assert!(!path.with_extension("part").exists());
        assert!(image::load_from_memory(&std::fs::read(&path).unwrap()).is_ok());
    }
    #[test]
    fn cover_fetch_runs_end_to_end_and_caches_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let (base, server) = serve_png(1);
        let engine =
            SourceEngine::load_wasm(source_manifest(), pages_wasm(&[]), EngineOptions::default())
                .unwrap();
        let url = format!("{base}/cover.png");
        let image = fetch_source_cover(root.path(), &engine, "test.source", &url).unwrap();
        assert_eq!(image.size, [4, 4]);
        server.join().unwrap();
        let path = source_cover_path(root.path(), &cover_key("test.source", &url)).unwrap();
        assert!(path.is_file());
        // The second fetch must come from disk: the server only answered once.

        let cached = fetch_source_cover(root.path(), &engine, "test.source", &url).unwrap();
        assert_eq!(cached.size, image.size);
    }
    // -----------------------------------------------------------------
    #[test]
    fn follow_cover_uses_source_cover_key_and_legacy_files_still_read() {
        let root = tempfile::tempdir().unwrap();
        // The same URL must resolve to the same `.img` file as the grid.
        let url = "https://img.example/cover/1.jpg";
        let key = cover_key("source.a", url);
        assert_eq!(key.len(), 16);
        let path = source_cover_path(root.path(), &key).unwrap();
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "source-covers");
        // A pre-unification `follow-covers` file still paints — compat read

        // keeps working while the pipeline only writes to `source-covers/`.
        let mut sources = test_sources(root.path());
        let snapshot = "a1b2c3d4e5f60708".to_string();
        let legacy = follow_cover_path(root.path(), &snapshot).unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 6, image::Rgba([10, 20, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        std::fs::write(&legacy, &png).unwrap();
        let texture = sources.cover_texture(&Context::default(), &snapshot);
        assert!(texture.is_some(), "legacy follow cover still renders");
        assert!(legacy.is_file(), "legacy file is left in place");
    }
    #[test]
    fn follow_cover_url_resolves_from_the_manga_snapshot_once() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut book = manga("source.a", "m");
        book.cover = Some("https://img.example/c.jpg".into());
        sources.chapters = vec![Chapter {
            key: "c1".into(),
            ..Default::default()
        }];
        sources.toggle_follow(&book);
        let url = sources.follow_cover_url(0);
        assert_eq!(url.as_deref(), Some("https://img.example/c.jpg"));
        // Second call comes from the cache, not the file — deleting the

        // snapshot does not change the resolved value.
        let snapshot = sources.follows[0].snapshot.clone();
        std::fs::remove_file(manga_snapshot_path(root.path(), &snapshot).unwrap()).unwrap();
        assert_eq!(
            sources.follow_cover_url(0).as_deref(),
            Some("https://img.example/c.jpg")
        );
        // And the cache key matches what plan_covers would queue.
        assert_eq!(
            cover_key("source.a", "https://img.example/c.jpg"),
            cover_key(&sources.follows[0].source_id, &url.unwrap()),
        );
    }
}
