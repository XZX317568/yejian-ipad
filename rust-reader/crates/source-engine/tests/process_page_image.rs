//! Regression for the `process_page_image` host path.
//!
//! The fixture module mirrors the Aidoku ABI: `process_page_image(response,
//! context)` receives a postcard `Response` whose `image` field is a stored
//! image descriptor, draws it onto a canvas and returns a new image ref in a
//! postcard `ImageRef`. The host must hand back real PNG bytes that the page
//! pipeline can decode — and must pass the page's `context` map through.

use std::sync::Arc;

use yejian_sources::{EngineOptions, SourceEngine, SourceManifest};

/// A 2x1 RGBA image: left red, right green.
fn fixture_png() -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    let image = image::RgbaImage::from_fn(2, 1, |x, _| {
        if x == 0 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 255, 0, 255])
        }
    });
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    png.into_inner()
}

/// Module that scales the page image into a 1x1 canvas (cropping the left
/// half) and returns the canvas as the new image ref.
fn crop_module() -> Vec<u8> {
    let source = r#"(module
        (import "std" "buffer_len" (func $len (param i32) (result i32)))
        (import "std" "read_buffer" (func $read (param i32 i32 i32) (result i32)))
        (import "canvas" "new_context" (func $ctx (param f32 f32) (result i32)))
        (import "canvas" "copy_image" (func $copy
            (param i32 i32 f32 f32 f32 f32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "get_image" (func $getimg (param i32) (result i32)))
        (memory (export "memory") 1)
        ;; scratch buffer for the response postcard + the result block
        (func (export "process_page_image") (param $response i32) (param $context i32) (result i32)
            (local $len i32) (local $img i32) (local $c i32)
            (local.set $len (call $len (local.get $response)))
            (if (i32.eqz (local.get $len)) (then (return (i32.const -1))))
            ;; read the postcard into scratch at 0
            (drop (call $read (local.get $response) (i32.const 0) (local.get $len)))
            ;; image ref is the last field: zigzag varint at byte len-1 / 2
            (local.set $img (i32.shr_u
                (i32.load8_u (i32.sub (local.get $len) (i32.const 1)))
                (i32.const 1)))
            ;; 1x1 context holding just the left source pixel
            (local.set $c (call $ctx (f32.const 1) (f32.const 1)))
            (drop (call $copy (local.get $c) (local.get $img)
                (f32.const 0) (f32.const 0) (f32.const 1) (f32.const 1)
                (f32.const 0) (f32.const 0) (f32.const 1) (f32.const 1)))
            ;; result ref = canvas png bytes descriptor
            (local.set $img (call $getimg (local.get $c)))
            ;; build the result block at 1024: [len+8][4 pad][zigzag(img)]
            (i32.store (i32.const 1024) (i32.const 12)) ;; len prefix = data+8
            (i32.store (i32.const 1028) (i32.const 0))   ;; pad
            (i32.store8 (i32.const 1032) (i32.shl (local.get $img) (i32.const 1)))
            (i32.const 1024)))"#;
    wat::parse_str(source).unwrap()
}

/// A module that traps when the context arg is `-1`, proving context
/// plumbing reached the call.
fn context_module() -> Vec<u8> {
    let source = r#"(module
        (import "std" "buffer_len" (func $len (param i32) (result i32)))
        (import "std" "read_buffer" (func $read (param i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "process_page_image") (param $response i32) (param $context i32) (result i32)
            (local $len i32) (local $img i32)
            ;; trap when the host forgot the context
            (if (i32.lt_s (local.get $context) (i32.const 0)) (then unreachable))
            ;; context buffer should decode to at least one pair => len > 1
            (if (i32.le_s (call $len (local.get $context)) (i32.const 1)) (then unreachable))
            ;; copy image ref out of the response tail (same trick as crop_module)
            (local.set $len (call $len (local.get $response)))
            (drop (call $read (local.get $response) (i32.const 0) (local.get $len)))
            (local.set $img (i32.shr_u
                (i32.load8_u (i32.sub (local.get $len) (i32.const 1)))
                (i32.const 1)))
            (i32.store (i32.const 1024) (i32.const 12))
            (i32.store (i32.const 1028) (i32.const 0))
            (i32.store8 (i32.const 1032) (i32.shl (local.get $img) (i32.const 1)))
            (i32.const 1024)))"#;
    wat::parse_str(source).unwrap()
}

/// A module whose `process_page_image` returns `-6` (canvas error) — the
/// caller must surface it as a real error instead of a valid image.
fn error_module() -> Vec<u8> {
    let source = r#"(module
        (memory (export "memory") 1)
        (func (export "process_page_image") (param i32 i32) (result i32)
            i32.const -6))"#;
    wat::parse_str(source).unwrap()
}

fn manifest(id: &str) -> SourceManifest {
    SourceManifest {
        id: id.into(),
        name: "Process Test".into(),
        version: 1,
        alt_names: Vec::new(),
        languages: vec!["en".into()],
        urls: vec!["https://example.test".into()],
        content_rating: 0,
        listings: Vec::new(),
        allows_base_url_select: false,
        hides_filters_while_searching: false,
    }
}

fn no_network() -> yejian_sources::RequestHandler {
    Arc::new(|_req| yejian_sources::NetResponseData {
        status_code: 200,
        ..Default::default()
    })
}

fn engine(id: &str, wasm: Vec<u8>) -> SourceEngine {
    let mut opts = EngineOptions::default();
    opts.request_handler = Some(no_network());
    SourceEngine::load_wasm(manifest(id), wasm, opts).unwrap()
}

#[test]
fn process_page_image_transforms_the_downloaded_bytes() {
    let engine = engine("test.process", crop_module());
    assert!(engine.features().processes_pages);
    let input = fixture_png();
    let out = pollster::block_on(engine.process_page_image(input.clone(), None))
        .unwrap()
        .expect("source processes pages");
    assert_ne!(out, input, "processed output must differ from the input");
    let decoded = image::load_from_memory(&out)
        .expect("output is a valid image")
        .to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (1, 1));
    // Left half was red → the single output pixel is red-dominant.
    let pixel = decoded.get_pixel(0, 0);
    assert!(pixel[0] > pixel[1], "expected red-dominant pixel, got {pixel:?}");
}

#[test]
fn page_context_reaches_the_module() {
    let engine = engine("test.context", context_module());
    let ctx = vec![
        ("page".to_string(), "3".to_string()),
        ("seed".to_string(), "abc".to_string()),
    ];
    let out = pollster::block_on(engine.process_page_image(fixture_png(), Some(ctx)))
        .unwrap()
        .expect("context reached");
    assert!(image::load_from_memory(&out).is_ok());
}

#[test]
fn source_errors_propagate_instead_of_silent_pass_through() {
    let engine = engine("test.err", error_module());
    let error =
        pollster::block_on(engine.process_page_image(fixture_png(), None)).unwrap_err();
    assert!(
        matches!(error, yejian_sources::SourceError::Message(ref m) if m.contains("canvas")),
        "{error}"
    );
}

#[test]
fn non_processing_sources_return_none_at_zero_cost() {
    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
            (func (export "get_search_manga_list") (param i32 i32 i32) (result i32) i32.const 0))"#,
    )
    .unwrap();
    let engine = engine("test.plain", wasm);
    assert!(!engine.features().processes_pages);
    let input = fixture_png();
    assert!(
        pollster::block_on(engine.process_page_image(input, None))
            .unwrap()
            .is_none()
    );
}

// -----------------------------------------------------------------------
// handle_notification
// -----------------------------------------------------------------------

/// A module that echoes the notification length as its return code and
/// records nothing — the caller only needs the round-trip not to trap.
fn notification_module() -> Vec<u8> {
    let source = r#"(module
        (import "std" "buffer_len" (func $len (param i32) (result i32)))
        (import "std" "read_buffer" (func $read (param i32 i32 i32) (result i32)))
        (import "std" "destroy" (func $destroy (param i32)))
        (memory (export "memory") 1)
        ;; read the string and stash its first byte so a second export can
        ;; verify the notification really arrived
        (func (export "handle_notification") (param $note i32) (result i32)
            (local $len i32)
            (local.set $len (call $len (local.get $note)))
            (if (i32.lt_s (local.get $len) (i32.const 1)) (then (return (i32.const -1))))
            (drop (call $read (local.get $note) (i32.const 0) (local.get $len)))
            (i32.store8 (i32.const 2048) (i32.load8_u (i32.const 0)))
            (i32.const 0))
        (func (export "last_note") (result i32)
            (i32.load8_u (i32.const 2048))))"#;
    wat::parse_str(source).unwrap()
}

#[test]
fn handle_notification_reaches_the_export() {
    let engine = engine("test.notify", notification_module());
    assert!(engine.features().handles_notifications);
    pollster::block_on(engine.handle_notification("logout".into())).unwrap();
}

#[test]
fn missing_notification_export_is_unimplemented() {
    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
            (func (export "get_search_manga_list") (param i32 i32 i32) (result i32) i32.const 0))"#,
    )
    .unwrap();
    let engine = engine("test.nonotify", wasm);
    assert!(!engine.features().handles_notifications);
    let error =
        pollster::block_on(engine.handle_notification("x".into())).unwrap_err();
    assert!(
        matches!(error, yejian_sources::SourceError::Unimplemented(_)),
        "{error}"
    );
}
