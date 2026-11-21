//! Core WASM execution engine.
//!
//! Instantiates one Aidoku `.wasm` module with `wasmi`, links every host
//! namespace, and exposes the exported functions real sources implement.
//! A module is single-threaded by design; [`crate::SourceEngine`] owns it on a
//! dedicated worker thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use wasmi::{Config, Engine, Instance, Linker, Memory, Module, Store, TrapCode, Val};
use image::ImageEncoder;

use crate::error::SourceError;
use crate::http::{NetRequestData, NetResponseData};

use super::host_defaults;
use super::host_net;
use super::models::*;
use super::postcard::{PostcardReader, PostcardWriter};
use super::store::{GlobalStore, StoredValue};
use super::{host_canvas, host_env, host_html, host_js, host_std};

pub(crate) type RequestHandler =
    Arc<dyn Fn(NetRequestData) -> NetResponseData + Send + Sync + 'static>;

pub(crate) struct HostState {
    pub store: GlobalStore,
    pub memory: Option<Memory>,
    pub source_key: String,
    pub print_handler: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub partial_result_handler: Option<Arc<dyn Fn(Vec<u8>) + Send + Sync>>,
    pub request_handler: RequestHandler,
    /// Set by any `js` import that would evaluate code or drive a web view.
    pub js_requested: bool,
    /// Set when a failing HTTP response carries a known anti-bot marker.
    pub challenge_suspected: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DetectedFeatures {
    pub provides_listings: bool,
    pub provides_home: bool,
    pub dynamic_filters: bool,
    pub dynamic_settings: bool,
    pub dynamic_listings: bool,
    pub processes_pages: bool,
    pub processes_covers: bool,
    pub provides_image_requests: bool,
    pub provides_page_descriptions: bool,
    pub provides_alternate_covers: bool,
    pub provides_base_url: bool,
    pub handles_notifications: bool,
    pub handles_deep_links: bool,
    pub handles_basic_login: bool,
    pub handles_web_login: bool,
    pub handles_migration: bool,
}

impl From<DetectedFeatures> for SourceFeatures {
    fn from(f: DetectedFeatures) -> Self {
        SourceFeatures {
            provides_listings: f.provides_listings,
            provides_home: f.provides_home,
            dynamic_filters: f.dynamic_filters,
            dynamic_settings: f.dynamic_settings,
            dynamic_listings: f.dynamic_listings,
            processes_pages: f.processes_pages,
            processes_covers: f.processes_covers,
            provides_image_requests: f.provides_image_requests,
            provides_page_descriptions: f.provides_page_descriptions,
            provides_alternate_covers: f.provides_alternate_covers,
            provides_base_url: f.provides_base_url,
            handles_notifications: f.handles_notifications,
            handles_deep_links: f.handles_deep_links,
            handles_basic_login: f.handles_basic_login,
            handles_web_login: f.handles_web_login,
            handles_migration: f.handles_migration,
        }
    }
}

pub(crate) struct AidokuInterpreter {
    store: Store<HostState>,
    instance: Instance,
    memory: Memory,
    fuel_per_call: Arc<AtomicU64>,
    pub features: DetectedFeatures,
}

pub(crate) type IResult<T> = Result<T, SourceError>;

impl AidokuInterpreter {
    pub fn create(
        bytes: &[u8],
        source_key: String,
        request_handler: RequestHandler,
        print_handler: Option<Arc<dyn Fn(String) + Send + Sync>>,
        partial_result_handler: Option<Arc<dyn Fn(Vec<u8>) + Send + Sync>>,
        fuel_per_call: Arc<AtomicU64>,
    ) -> Result<Self, SourceError> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes)
            .map_err(|e| SourceError::Load(format!("invalid WebAssembly module: {e}")))?;

        let host_state = HostState {
            store: GlobalStore::new(),
            memory: None,
            source_key,
            print_handler,
            partial_result_handler,
            request_handler,
            js_requested: false,
            challenge_suspected: false,
        };
        let mut store = Store::new(&engine, host_state);
        let limit = fuel_limit(&fuel_per_call);
        store
            .set_fuel(limit)
            .map_err(|e| SourceError::Load(format!("enabling WebAssembly fuel metering: {e}")))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        host_std::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking std imports: {e}")))?;
        host_net::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking net imports: {e}")))?;
        host_html::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking html imports: {e}")))?;
        host_defaults::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking defaults imports: {e}")))?;
        host_env::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking env imports: {e}")))?;
        host_canvas::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking canvas imports: {e}")))?;
        host_js::link(&mut linker)
            .map_err(|e| SourceError::Load(format!("linking js imports: {e}")))?;

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| load_error("instantiating source module", e, limit))?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| SourceError::Load("WASM module does not export linear memory".into()))?;
        store.data_mut().memory = Some(memory);

        let has = |name: &str| instance.get_func(&store, name).is_some();
        let features = DetectedFeatures {
            provides_listings: has("get_manga_list"),
            provides_home: has("get_home"),
            dynamic_filters: has("get_filters"),
            dynamic_settings: has("get_settings"),
            dynamic_listings: has("get_listings"),
            processes_pages: has("process_page_image"),
            processes_covers: has("process_cover_image"),
            provides_image_requests: has("get_image_request"),
            provides_page_descriptions: has("get_page_description"),
            provides_alternate_covers: has("get_alternate_covers"),
            provides_base_url: has("get_base_url"),
            handles_notifications: has("handle_notification"),
            handles_deep_links: has("handle_deep_link"),
            handles_basic_login: has("handle_basic_login"),
            handles_web_login: has("handle_web_login"),
            handles_migration: has("handle_key_migration"),
        };

        let mut interpreter = Self {
            store,
            instance,
            memory,
            fuel_per_call,
            features,
        };
        interpreter.start().map_err(|error| match error {
            SourceError::FuelExhausted { limit } => SourceError::FuelExhausted { limit },
            other => {
                if interpreter.store.data().js_requested {
                    SourceError::web_verification(
                        "the source start function needs JavaScript/WebView execution",
                    )
                } else {
                    SourceError::Load(format!("source start failed: {other}"))
                }
            }
        })?;
        Ok(interpreter)
    }

    pub fn source_key(&self) -> &str {
        &self.store.data().source_key
    }

    /// Executes a request through the same transport/cookie session the
    /// source uses. Must be called on the source worker thread.
    pub fn request(&self, request: NetRequestData) -> NetResponseData {
        (self.store.data().request_handler)(request)
    }

    fn start(&mut self) -> IResult<()> {
        if let Some(func) = self.instance.get_func(&mut self.store, "start") {
            func.call(&mut self.store, &[], &mut [])
                .map_err(|e| call_error("WASM trap in start", e, self.fuel_limit()))?;
        }
        Ok(())
    }

    fn begin_call(&mut self) {
        let _ = self.store.set_fuel(self.fuel_limit());
        let data = self.store.data_mut();
        data.js_requested = false;
        data.challenge_suspected = false;
    }

    fn fuel_limit(&self) -> u64 {
        fuel_limit(&self.fuel_per_call)
    }

    /// Turns a failed or suspiciously empty result into an explicit
    /// verification error when the source asked for JS/webview work or the
    /// site served an anti-bot page.
    fn guard<T>(&mut self, result: IResult<T>, empty: impl Fn(&T) -> bool) -> IResult<T> {
        let js = self.store.data().js_requested;
        let challenge = self.store.data().challenge_suspected;
        if !js && !challenge {
            return result;
        }
        match result {
            Err(SourceError::Unimplemented(_)) => result,
            Err(SourceError::FuelExhausted { .. }) => result,
            Err(_) => Err(Self::verification_error(js, challenge)),
            Ok(value) if empty(&value) => Err(Self::verification_error(js, challenge)),
            other => other,
        }
    }

    fn verification_error(js: bool, challenge: bool) -> SourceError {
        let detail = match (js, challenge) {
            (true, true) => {
                "the source needs JavaScript/WebView execution and the site served a verification challenge"
            }
            (true, false) => {
                "the source needs JavaScript/WebView execution, which the native engine does not provide"
            }
            (false, true) => "the site returned an anti-bot verification page instead of content",
            (false, false) => "web verification required",
        };
        SourceError::web_verification(detail)
    }

    fn store_bytes(&mut self, bytes: Vec<u8>) -> i32 {
        self.store.data_mut().store.store(StoredValue::Bytes(bytes))
    }

    fn destroy(&mut self, desc: i32) {
        self.store.data_mut().store.remove(desc);
    }

    fn call_raw(&mut self, name: &str, args: &[i32]) -> IResult<i32> {
        let func = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| SourceError::Unimplemented(name.to_string()))?;
        let params: Vec<Val> = args.iter().map(|a| Val::I32(*a)).collect();
        let mut results = [Val::I32(0)];
        func.call(&mut self.store, &params, &mut results)
            .map_err(|e| call_error(format!("WASM trap in {name}"), e, self.fuel_limit()))?;
        match results[0] {
            Val::I32(v) => Ok(v),
            _ => Err(SourceError::Deserialize(format!(
                "{name} returned a non-i32 value"
            ))),
        }
    }

    fn handle_result(&mut self, result: i32) -> IResult<Vec<u8>> {
        if result < 0 {
            return Err(SourceError::from_code(result, None));
        }
        let pointer = result;
        let length = super::memory::read_u32(self.memory, &self.store, pointer)
            .map_err(|e| SourceError::Deserialize(e.to_string()))?;
        if length == 0xFFFF_FFFF {
            let str_len = super::memory::read_u32(self.memory, &self.store, pointer + 8)
                .map_err(|e| SourceError::Deserialize(e.to_string()))?
                .wrapping_sub(12);
            let msg =
                super::memory::read_string(self.memory, &self.store, pointer + 12, str_len as i32)
                    .unwrap_or_default();
            self.free_result(pointer);
            return Err(SourceError::Message(msg));
        }
        let data =
            super::memory::read_bytes(self.memory, &self.store, pointer + 8, length as i32 - 8)
                .map_err(|e| SourceError::Deserialize(e.to_string()))?;
        self.free_result(pointer);
        Ok(data)
    }

    fn free_result(&mut self, pointer: i32) {
        if let Some(func) = self.instance.get_func(&mut self.store, "free_result") {
            let _ = func.call(&mut self.store, &[Val::I32(pointer)], &mut []);
        }
    }

    // -----------------------------------------------------------------
    // High level API
    // -----------------------------------------------------------------

    pub fn get_manga_list(&mut self, listing: &Listing, page: i32) -> IResult<MangaPageResult> {
        self.begin_call();
        let mut w = PostcardWriter::new();
        listing.to_postcard(&mut w);
        let listing_desc = self.store_bytes(w.into_bytes());
        let result = self.call_raw("get_manga_list", &[listing_desc, page]);
        self.destroy(listing_desc);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                MangaPageResult::from_postcard(&mut r)
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |page| page.entries.is_empty())
    }

    pub fn get_search_manga_list(
        &mut self,
        query: Option<&str>,
        page: i32,
        filters: &[FilterValue],
    ) -> IResult<MangaPageResult> {
        self.begin_call();
        let query_desc = self.store_bytes(query.unwrap_or("").as_bytes().to_vec());
        let mut fw = PostcardWriter::new();
        fw.write_list(filters, |w, f| f.to_postcard(w));
        let filters_desc = self.store_bytes(fw.into_bytes());
        let result = self.call_raw("get_search_manga_list", &[query_desc, page, filters_desc]);
        self.destroy(query_desc);
        self.destroy(filters_desc);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                MangaPageResult::from_postcard(&mut r)
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |page| page.entries.is_empty())
    }

    pub fn get_manga_update(
        &mut self,
        manga: &Manga,
        needs_details: bool,
        needs_chapters: bool,
    ) -> IResult<Manga> {
        self.begin_call();
        let mut w = PostcardWriter::new();
        manga.to_postcard(&mut w);
        let manga_desc = self.store_bytes(w.into_bytes());
        let result = self.call_raw(
            "get_manga_update",
            &[manga_desc, needs_details as i32, needs_chapters as i32],
        );
        self.destroy(manga_desc);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                Manga::from_postcard(&mut r).map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |manga| {
            (needs_details && manga.key.is_empty() && manga.title.is_empty())
                || (needs_chapters && manga.chapters.as_ref().is_none_or(|c| c.is_empty()))
        })
    }

    pub fn get_page_list(&mut self, manga: &Manga, chapter: &Chapter) -> IResult<Vec<Page>> {
        self.begin_call();
        let mut mw = PostcardWriter::new();
        manga.to_postcard(&mut mw);
        let manga_desc = self.store_bytes(mw.into_bytes());
        let mut cw = PostcardWriter::new();
        chapter.to_postcard(&mut cw);
        let chapter_desc = self.store_bytes(cw.into_bytes());
        let result = self.call_raw("get_page_list", &[manga_desc, chapter_desc]);
        self.destroy(manga_desc);
        self.destroy(chapter_desc);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                let store_ref = &self.store.data().store;
                r.read_list(|r| Page::from_postcard(r, store_ref))
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |pages| pages.is_empty())
    }

    pub fn get_filters(&mut self) -> IResult<Vec<Filter>> {
        self.begin_call();
        let result = self.call_raw("get_filters", &[]);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                r.read_list(Filter::from_postcard)
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |filters| filters.is_empty())
    }

    pub fn get_listings(&mut self) -> IResult<Vec<Listing>> {
        self.begin_call();
        let result = self.call_raw("get_listings", &[]);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                r.read_list(Listing::from_postcard)
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |listings| listings.is_empty())
    }

    pub fn get_base_url(&mut self) -> IResult<Option<String>> {
        self.begin_call();
        let result = self.call_raw("get_base_url", &[]);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                r.read_option(|r| r.read_string())
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |value| value.is_none())
    }

    pub fn handle_deep_link(&mut self, url: &str) -> IResult<Option<DeepLinkResult>> {
        self.begin_call();
        let url_desc = self.store_bytes(url.as_bytes().to_vec());
        let result = self.call_raw("handle_deep_link", &[url_desc]);
        self.destroy(url_desc);
        let result = match result {
            Ok(code) if code < 0 => Ok(None),
            Ok(code) => self.handle_result(code).and_then(|data| {
                let mut r = PostcardReader::new(&data);
                DeepLinkResult::from_postcard(&mut r)
                    .map(Some)
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            }),
            Err(e) => Err(e),
        };
        self.guard(result, |value| value.is_none())
    }

    pub fn handle_basic_login(
        &mut self,
        key: &str,
        username: &str,
        password: &str,
    ) -> IResult<bool> {
        self.begin_call();
        let key_desc = self.store_bytes(key.as_bytes().to_vec());
        let user_desc = self.store_bytes(username.as_bytes().to_vec());
        let pass_desc = self.store_bytes(password.as_bytes().to_vec());
        let result = self.call_raw("handle_basic_login", &[key_desc, user_desc, pass_desc]);
        self.destroy(key_desc);
        self.destroy(user_desc);
        self.destroy(pass_desc);
        let result = result.map(|code| code == 1);
        if matches!(result, Ok(false))
            && (self.store.data().js_requested || self.store.data().challenge_suspected)
        {
            return Err(Self::verification_error(
                self.store.data().js_requested,
                self.store.data().challenge_suspected,
            ));
        }
        result
    }

    pub fn handle_web_login(&mut self, key: &str, cookies: &[(String, String)]) -> IResult<bool> {
        self.begin_call();
        let key_desc = self.store_bytes(key.as_bytes().to_vec());
        let mut w = PostcardWriter::new();
        w.write_map(cookies, |w, k| w.write_string(k), |w, v| w.write_string(v));
        let cookies_desc = self.store_bytes(w.into_bytes());
        let result = self.call_raw("handle_web_login", &[key_desc, cookies_desc]);
        self.destroy(key_desc);
        self.destroy(cookies_desc);
        result.map(|code| code == 1)
    }

    pub fn handle_migration(
        &mut self,
        kind: i32,
        manga_key: &str,
        chapter_key: Option<&str>,
    ) -> IResult<String> {
        self.begin_call();
        let manga_desc = self.store_bytes(manga_key.as_bytes().to_vec());
        let chapter_desc = self.store_bytes(chapter_key.unwrap_or("").as_bytes().to_vec());
        let result = self.call_raw("handle_key_migration", &[kind, manga_desc, chapter_desc]);
        self.destroy(manga_desc);
        self.destroy(chapter_desc);
        let result = result
            .and_then(|code| self.handle_result(code))
            .and_then(|data| {
                let mut r = PostcardReader::new(&data);
                r.read_string()
                    .map_err(|e| SourceError::Deserialize(e.to_string()))
            });
        self.guard(result, |value| value.is_empty())
    }

    pub fn get_image_request(
        &mut self,
        url: &str,
        context: Option<&HashMap<String, String>>,
    ) -> IResult<Option<ImageRequest>> {
        if !self.features.provides_image_requests {
            return Ok(None);
        }
        self.begin_call();
        let url_desc = self.store_bytes({
            let mut w = PostcardWriter::new();
            w.write_string(url);
            w.into_bytes()
        });
        let ctx_desc = match context {
            Some(ctx) => {
                let mut w = PostcardWriter::new();
                let entries: Vec<(String, String)> =
                    ctx.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                w.write_map(&entries, |w, k| w.write_string(k), |w, v| w.write_string(v));
                self.store_bytes(w.into_bytes())
            }
            None => -1,
        };
        let result = self.call_raw("get_image_request", &[url_desc, ctx_desc]);
        self.destroy(url_desc);
        if ctx_desc >= 0 {
            self.destroy(ctx_desc);
        }
        let result = match result {
            Ok(code) if code < 0 => Ok(None),
            Ok(code) => self.handle_result(code).and_then(|data| {
                let mut r = PostcardReader::new(&data);
                let rid = r
                    .read_i32()
                    .map_err(|e| SourceError::Deserialize(e.to_string()))?;
                let found = match self.store.data().store.get(rid) {
                    Some(StoredValue::NetRequest(req)) => {
                        Some((req.url.clone(), req.headers.clone(), req.body.clone()))
                    }
                    _ => None,
                };
                if let Some((request_url, headers, body)) = found {
                    self.store.data_mut().store.remove(rid);
                    Ok(Some(ImageRequest {
                        url: request_url.unwrap_or_else(|| url.to_string()),
                        headers,
                        body,
                    }))
                } else {
                    Ok(None)
                }
            }),
            Err(e) => Err(e),
        };
        self.guard(result, |value| value.is_none())
    }

    /// Runs one downloaded page image through the source's
    /// `process_page_image` export, used by obfuscated-image sources.
    ///
    /// `bytes` is the raw response body; `context` is the page's optional
    /// `PageContext` (the Aidoku ABI passes the postcard map as the second
    /// argument, `-1` when absent). The returned [`AidokuResponse`] hands the
    /// module a `StoredValue::Image` descriptor to work on, and the result
    /// descriptor decodes back to a `PlatformImage` — re-encoded here as PNG
    /// bytes ready for the page store.
    pub fn process_page_image(
        &mut self,
        bytes: &[u8],
        context: Option<&[(String, String)]>,
    ) -> IResult<Vec<u8>> {
        self.process_image("process_page_image", bytes, context)
    }

    /// Cover variant of [`Self::process_page_image`]; the export takes the
    /// response only, with no page context argument.
    pub fn process_cover_image(&mut self, bytes: &[u8]) -> IResult<Vec<u8>> {
        self.process_image("process_cover_image", bytes, None)
    }

    fn process_image(
        &mut self,
        export: &str,
        bytes: &[u8],
        context: Option<&[(String, String)]>,
    ) -> IResult<Vec<u8>> {
        self.begin_call();
        let decoded = image::load_from_memory(bytes)
            .map_err(|e| SourceError::Deserialize(format!("page image is not decodable: {e}")))?
            .to_rgba8();
        let image_ref = self.store.data_mut().store.store(StoredValue::Image(decoded));

        let response = AidokuResponse {
            code: 200,
            headers: Vec::new(),
            request: AidokuRequest {
                url: None,
                headers: Vec::new(),
            },
            image: image_ref,
        };
        let mut w = PostcardWriter::new();
        response.to_postcard(&mut w);
        let response_desc = self.store_bytes(w.into_bytes());

        let context_desc = if export == "process_page_image" {
            match context {
                Some(pairs) => {
                    let mut w = PostcardWriter::new();
                    w.write_map(pairs, |w, k| w.write_string(k), |w, v| w.write_string(v));
                    self.store_bytes(w.into_bytes())
                }
                None => -1,
            }
        } else {
            -1
        };

        let args = if export == "process_page_image" {
            &[response_desc, context_desc][..]
        } else {
            &[response_desc][..]
        };
        let result = self.call_raw(export, args);
        self.destroy(response_desc);
        if context_desc >= 0 {
            self.destroy(context_desc);
        }

        let mut output_ref = None;
        let result = result.and_then(|code| {
            self.handle_result(code).and_then(|data| {
                let mut r = PostcardReader::new(&data);
                let result_ref = r
                    .read_i32()
                    .map_err(|e| SourceError::Deserialize(e.to_string()))?;
                if result_ref != image_ref {
                    output_ref = Some(result_ref);
                }
                let image = match self.store.data().store.get(result_ref) {
                    Some(StoredValue::Image(image)) => image.clone(),
                    Some(StoredValue::Bytes(bytes)) => image::load_from_memory(bytes)
                        .map_err(|e| {
                            SourceError::Deserialize(format!(
                                "{export} returned undecodable image bytes: {e}"
                            ))
                        })?
                        .to_rgba8(),
                    _ => {
                        return Err(SourceError::Deserialize(format!(
                            "{export} did not return a stored image (ref {result_ref})"
                        )));
                    }
                };
                let mut encoded = std::io::Cursor::new(Vec::new());
                image::codecs::png::PngEncoder::new(&mut encoded)
                    .write_image(
                        image.as_raw(),
                        image.width(),
                        image.height(),
                        image::ExtendedColorType::Rgba8,
                    )
                    .map_err(|e| {
                        SourceError::Deserialize(format!("encoding processed page image: {e}"))
                    })?;
                Ok(encoded.into_inner())
            })
        });
        // The input image and any returned image are released on every exit,
        // including source errors, traps and undecodable results.
        let store = &mut self.store.data_mut().store;
        store.remove(image_ref);
        if let Some(output_ref) = output_ref {
            store.remove(output_ref);
        }
        self.guard(result, |bytes| bytes.is_empty())
    }

    /// Delivers a `settings.json` `notification` string to the source's
    /// `handle_notification` export (button taps, page refreshes).
    pub fn handle_notification(&mut self, notification: &str) -> IResult<()> {
        self.begin_call();
        let desc = self.store_bytes(notification.as_bytes().to_vec());
        let result = self.call_raw("handle_notification", &[desc]);
        self.destroy(desc);
        result.map(|_| ())
    }

    pub fn dispose(&mut self) {
        self.store.data_mut().store.clear();
    }
}

/// `0` means "no practical limit"; every other value is the per-call budget.
fn fuel_limit(fuel: &AtomicU64) -> u64 {
    match fuel.load(Ordering::Relaxed) {
        0 => u64::MAX,
        value => value,
    }
}

fn call_error(context: impl Into<String>, error: wasmi::Error, limit: u64) -> SourceError {
    if is_out_of_fuel(&error) {
        SourceError::FuelExhausted { limit }
    } else {
        SourceError::Message(format!("{}: {error}", context.into()))
    }
}

fn load_error(context: &str, error: wasmi::Error, limit: u64) -> SourceError {
    if is_out_of_fuel(&error) {
        SourceError::FuelExhausted { limit }
    } else {
        SourceError::Load(format!("{context}: {error}"))
    }
}

fn is_out_of_fuel(error: &wasmi::Error) -> bool {
    error.as_trap_code() == Some(TrapCode::OutOfFuel)
        || error
            .to_string()
            .to_ascii_lowercase()
            .contains("out of fuel")
}
