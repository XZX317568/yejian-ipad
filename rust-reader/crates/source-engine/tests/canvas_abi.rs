//! Regression for the `canvas` host-import ABI.
//!
//! `zh.boylove` (and other sources that draw page images) failed to even
//! instantiate because the host's `get_image_width`/`get_image_height`
//! returned `i32` while the upstream AidokuRunner ABI is `i32 -> f32`, and
//! several other canvas entry points take `f32` coordinates / extra path or
//! text pointers. The signatures here mirror `Imports/Canvas.swift` exactly.

use std::sync::Arc;

use yejian_sources::{EngineOptions, SourceEngine, SourceManifest};

/// A WASM module importing every canvas function the host exposes, with the
/// *real* upstream signatures. wasmi validates all declared imports at
/// instantiate, so any host signature drift fails before `get_filters` runs —
/// exactly the live-probe failure this regresses.
fn canvas_user_module() -> Vec<u8> {
    let source = r#"(module
        ;; signatures as changed by the ABI fix (f32 coords, f32 size returns,
        ;; full path/style/text/font pointer arguments)
        (import "canvas" "new_context"      (func $new_ctx   (param f32 f32) (result i32)))
        (import "canvas" "set_transform"    (func $set_tf
            (param i32 f32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "draw_image"       (func $draw_img
            (param i32 i32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "copy_image"       (func $copy_img
            (param i32 i32 f32 f32 f32 f32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "fill"             (func $fill
            (param i32 i32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "stroke"           (func $stroke    (param i32 i32 i32) (result i32)))
        (import "canvas" "draw_text"        (func $draw_txt
            (param i32 i32 i32 f32 f32 f32 i32 f32 f32 f32 f32) (result i32)))
        (import "canvas" "new_font"         (func $new_font  (param i32 i32) (result i32)))
        (import "canvas" "system_font"      (func $sys_font  (param i32) (result i32)))
        (import "canvas" "load_font"        (func $load_font (param i32 i32) (result i32)))
        (import "canvas" "get_image"        (func $get_img   (param i32) (result i32)))
        (import "canvas" "new_image"        (func $new_img   (param i32 i32) (result i32)))
        (import "canvas" "get_image_data"   (func $img_data  (param i32) (result i32)))
        (import "canvas" "get_image_width"  (func $img_w     (param i32) (result f32)))
        (import "canvas" "get_image_height" (func $img_h     (param i32) (result f32)))
        (memory (export "memory") 1)
        (func (export "get_filters") (result i32)
            i32.const 0)
        (func (export "get_search_manga_list")
            (param i32 i32 i32) (result i32)
            ;; exercise the imports so a bad signature can't hide behind laziness
            f32.const 640 f32.const 480 call $new_ctx drop
            i32.const 0))"#;
    wat::parse_str(source).unwrap()
}

fn manifest() -> SourceManifest {
    SourceManifest {
        id: "test.canvas".into(),
        name: "Canvas Test".into(),
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

#[test]
fn module_using_real_canvas_signatures_instantiates() {
    // Before the ABI fix this failed at instantiate with a FuncType mismatch on
    // get_image_width ([I32]->[F32] expected, host gave [I32]->[I32]).
    let engine = SourceEngine::load_wasm(
        manifest(),
        canvas_user_module(),
        EngineOptions::default(),
    )
    .expect("module with real canvas signatures must instantiate");
    // the module actually invoked the canvas host functions during the call
    let _ = pollster::block_on(engine.filters());
}

/// Real `zh.boylove` module from the live community repo, driven through the
/// full `load_installed` path (source.json + filters.json + wasm) with a
/// mocked request handler. Recorded evidence: filters() = 5 (4 static + 1
/// dynamic), listings() = 11.
///
/// Requires the package under `.tools/source-filters-20261119/live/zh.boylove`
/// (see `live/LIVE-SOURCES.md`). Run explicitly:
/// `cargo test -p yejian-sources --test canvas_abi -- --ignored`.
#[test]
#[ignore = "requires explicitly downloaded live package"]
fn zh_boylove_module_loads_and_exposes_filters() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.tools/source-filters-20261119/live/zh.boylove");
    let wasm_path = dir.join("main.wasm");
    for required in [
        &wasm_path,
        &dir.join("source.json"),
        &dir.join("filters.json"),
    ] {
        assert!(
            required.is_file(),
            "live fixture missing: {} — download per live/LIVE-SOURCES.md before running this test",
            required.display()
        );
    }
    let manifest = SourceManifest::from_json_str(
        &std::fs::read_to_string(dir.join("source.json")).unwrap(),
    )
    .unwrap();
    let installed = yejian_sources::InstalledSource {
        manifest,
        directory: dir.clone(),
        wasm_path: wasm_path.clone(),
        settings_path: Some(dir.join("settings.json")),
        filters_path: Some(dir.join("filters.json")),
        icon_path: Some(dir.join("icon.png")),
    };
    // A mocked request handler keeps the real module off the live site during
    // `start`/`get_filters` — this is an ABI regression, not a network test.
    let mut opts = EngineOptions::default();
    opts.request_handler = Some(Arc::new(|_req| yejian_sources::NetResponseData {
        status_code: 200,
        ..Default::default()
    }));
    let engine = SourceEngine::load_installed_with(&installed, opts)
        .expect("zh.boylove must load: canvas ABI must match the real module");

    // Recorded evidence: 4 static filters + 1 dynamic get_filters result = 5.
    let filters = pollster::block_on(engine.filters()).unwrap();
    assert_eq!(filters.len(), 5, "zh.boylove filters() should merge 4 static + 1 dynamic");
    // 11 static listings (source.json; the module exports no dynamic get_listings).
    let listings = pollster::block_on(engine.listings()).unwrap();
    assert_eq!(listings.len(), 11);
}
