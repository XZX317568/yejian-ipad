//! `canvas` WASM host imports (basic raster compositing used by some sources
//! for page image deobfuscation).
//!
//! Function *signatures* mirror AidokuRunner's `Imports/Canvas.swift` exactly —
//! coordinate/dimension arguments are `f32`, and `get_image_width` /
//! `get_image_height` return `f32` (a `Float32` cast of the pixel size, or of
//! the `invalidImagePointer` error code). Matching the real ABI is required
//! for modules that import these to even instantiate: a source that only
//! touches `get_image_width` fails to load when the host returns `i32`.
//!
//! `fill`/`stroke`/`draw_text`/font loading are stubs that return success —
//! real Aidoku sources overwhelmingly only use `draw_image`/`copy_image`/
//! `get_image`/`new_image` for page deobfuscation, same as the Dart engine.

use image::{GenericImage, ImageEncoder, RgbaImage, imageops};
use wasmi::{Caller, Linker};

use super::HostState;
use super::store::StoredValue;

// Canvas result codes, matching AidokuRunner `Canvas.Result`.
const RESULT_SUCCESS: i32 = 0;
const RESULT_INVALID_CONTEXT: i32 = -1;
const RESULT_INVALID_IMAGE_PTR: i32 = -2;
const RESULT_INVALID_IMAGE: i32 = -3;
const RESULT_INVALID_SRC_RECT: i32 = -4;
const RESULT_INVALID_BOUNDS: i32 = -6;
const RESULT_INVALID_DATA: i32 = -11;

/// `get_image_width`/`get_image_height` report errors as a float error code.
fn image_err_f32(code: i32) -> f32 {
    code as f32
}

pub(crate) fn link(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    // new_context(width: f32, height: f32) -> i32
    linker.func_wrap(
        "canvas",
        "new_context",
        |mut caller: Caller<'_, HostState>, width: f32, height: f32| -> i32 {
            if width <= 0.0 || height <= 0.0 || !width.is_finite() || !height.is_finite() {
                return RESULT_INVALID_BOUNDS;
            }
            let img = RgbaImage::new(width as u32, height as u32);
            caller
                .data_mut()
                .store
                .store(StoredValue::CanvasContext(img))
        },
    )?;

    // set_transform(ctx, translate_x, translate_y, scale_x, scale_y, rotate) -> i32
    linker.func_wrap(
        "canvas",
        "set_transform",
        |_: Caller<'_, HostState>,
         _ctx: i32,
         _tx: f32,
         _ty: f32,
         _sx: f32,
         _sy: f32,
         _rot: f32|
         -> i32 { RESULT_SUCCESS },
    )?;

    // draw_image(ctx, img, dst_x, dst_y, dst_w, dst_h) -> i32
    linker.func_wrap(
        "canvas",
        "draw_image",
        |mut caller: Caller<'_, HostState>,
         ctx: i32,
         img: i32,
         dst_x: f32,
         dst_y: f32,
         dst_w: f32,
         dst_h: f32|
         -> i32 { draw_or_copy(&mut caller, ctx, img, None, (dst_x, dst_y, dst_w, dst_h)) },
    )?;

    // copy_image(ctx, img, src_x, src_y, src_w, src_h, dst_x, dst_y, dst_w, dst_h)
    linker.func_wrap(
        "canvas",
        "copy_image",
        |mut caller: Caller<'_, HostState>,
         ctx: i32,
         img: i32,
         src_x: f32,
         src_y: f32,
         src_w: f32,
         src_h: f32,
         dst_x: f32,
         dst_y: f32,
         dst_w: f32,
         dst_h: f32|
         -> i32 {
            draw_or_copy(
                &mut caller,
                ctx,
                img,
                Some((src_x, src_y, src_w, src_h)),
                (dst_x, dst_y, dst_w, dst_h),
            )
        },
    )?;

    // fill(ctx, path_ptr, r, g, b, a) -> i32   (stub: success)
    linker.func_wrap(
        "canvas",
        "fill",
        |_: Caller<'_, HostState>,
         _ctx: i32,
         _path: i32,
         _r: f32,
         _g: f32,
         _b: f32,
         _a: f32|
         -> i32 { RESULT_SUCCESS },
    )?;

    // stroke(ctx, path_ptr, style_ptr) -> i32   (stub: success)
    linker.func_wrap(
        "canvas",
        "stroke",
        |_: Caller<'_, HostState>, _ctx: i32, _path: i32, _style: i32| -> i32 { RESULT_SUCCESS },
    )?;

    // draw_text(ctx, text_ptr, text_len, size, x, y, font_ptr, r, g, b, a) -> i32
    linker.func_wrap(
        "canvas",
        "draw_text",
        |_: Caller<'_, HostState>,
         _ctx: i32,
         _text_ptr: i32,
         _text_len: i32,
         _size: f32,
         _x: f32,
         _y: f32,
         _font: i32,
         _r: f32,
         _g: f32,
         _b: f32,
         _a: f32|
         -> i32 { RESULT_SUCCESS },
    )?;

    // new_font(name_ptr, name_len) -> i32   (stub: a valid descriptor)
    linker.func_wrap(
        "canvas",
        "new_font",
        |_: Caller<'_, HostState>, _name_ptr: i32, _name_len: i32| -> i32 { 1 },
    )?;

    // system_font(weight: i32) -> i32   (stub)
    linker.func_wrap(
        "canvas",
        "system_font",
        |_: Caller<'_, HostState>, _weight: i32| -> i32 { 1 },
    )?;

    // load_font(url_ptr, url_len) -> i32   (stub)
    linker.func_wrap(
        "canvas",
        "load_font",
        |_: Caller<'_, HostState>, _url_ptr: i32, _url_len: i32| -> i32 { 1 },
    )?;

    // get_image(ctx) -> i32 (descriptor for PNG bytes of the canvas)
    linker.func_wrap(
        "canvas",
        "get_image",
        |mut caller: Caller<'_, HostState>, ctx: i32| -> i32 {
            let png = match caller.data().store.get(ctx) {
                Some(StoredValue::CanvasContext(img)) => encode_png(img),
                _ => return RESULT_INVALID_CONTEXT,
            };
            match png {
                Some(bytes) => caller.data_mut().store.store(StoredValue::Bytes(bytes)),
                None => RESULT_INVALID_IMAGE,
            }
        },
    )?;

    // new_image(data_ptr, data_len) -> i32
    linker.func_wrap(
        "canvas",
        "new_image",
        |mut caller: Caller<'_, HostState>, data_ptr: i32, data_len: i32| -> i32 {
            let mem = caller.data().memory.unwrap();
            let bytes = match super::memory::read_bytes(mem, &caller, data_ptr, data_len) {
                Ok(b) => b,
                Err(_) => return RESULT_INVALID_DATA,
            };
            match image::load_from_memory(&bytes) {
                Ok(decoded) => caller
                    .data_mut()
                    .store
                    .store(StoredValue::Image(decoded.to_rgba8())),
                Err(_) => RESULT_INVALID_IMAGE,
            }
        },
    )?;

    // get_image_data(img) -> i32
    linker.func_wrap(
        "canvas",
        "get_image_data",
        |mut caller: Caller<'_, HostState>, img: i32| -> i32 {
            let png = match resolve_image(&mut caller, img) {
                Some(image) => encode_png(&image),
                None => return RESULT_INVALID_IMAGE_PTR,
            };
            match png {
                Some(bytes) => caller.data_mut().store.store(StoredValue::Bytes(bytes)),
                None => RESULT_INVALID_IMAGE,
            }
        },
    )?;

    // get_image_width(img) -> f32
    linker.func_wrap(
        "canvas",
        "get_image_width",
        |mut caller: Caller<'_, HostState>, img: i32| -> f32 {
            resolve_image(&mut caller, img)
                .map(|i| i.width() as f32)
                .unwrap_or_else(|| image_err_f32(RESULT_INVALID_IMAGE_PTR))
        },
    )?;

    // get_image_height(img) -> f32
    linker.func_wrap(
        "canvas",
        "get_image_height",
        |mut caller: Caller<'_, HostState>, img: i32| -> f32 {
            resolve_image(&mut caller, img)
                .map(|i| i.height() as f32)
                .unwrap_or_else(|| image_err_f32(RESULT_INVALID_IMAGE_PTR))
        },
    )?;

    Ok(())
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            img,
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

fn resolve_image(caller: &mut Caller<'_, HostState>, desc: i32) -> Option<RgbaImage> {
    match caller.data().store.get(desc) {
        Some(StoredValue::Image(img)) => Some(img.clone()),
        Some(StoredValue::Bytes(bytes)) => {
            let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
            caller
                .data_mut()
                .store
                .set(desc, StoredValue::Image(decoded.clone()));
            Some(decoded)
        }
        _ => None,
    }
}

fn draw_or_copy(
    caller: &mut Caller<'_, HostState>,
    ctx: i32,
    img_desc: i32,
    src_rect: Option<(f32, f32, f32, f32)>,
    dst: (f32, f32, f32, f32),
) -> i32 {
    let src = match resolve_image(caller, img_desc) {
        Some(i) => i,
        None => return RESULT_INVALID_IMAGE_PTR,
    };
    let (dst_x, dst_y, dst_w, dst_h) = dst;

    let cropped = match src_rect {
        Some((sx, sy, sw, sh)) => {
            let sx = sx.max(0.0) as u32;
            let sy = sy.max(0.0) as u32;
            let sw = (sw as u32).min(src.width().saturating_sub(sx)).max(1);
            let sh = (sh as u32).min(src.height().saturating_sub(sy)).max(1);
            imageops::crop_imm(&src, sx, sy, sw, sh).to_image()
        }
        None => src,
    };

    let resized = if dst_w as u32 != cropped.width() || dst_h as u32 != cropped.height() {
        imageops::resize(
            &cropped,
            dst_w.max(1.0) as u32,
            dst_h.max(1.0) as u32,
            imageops::FilterType::Lanczos3,
        )
    } else {
        cropped
    };

    match caller.data_mut().store.get_mut(ctx) {
        Some(StoredValue::CanvasContext(target)) => {
            let _ = target.copy_from(&resized, dst_x.max(0.0) as u32, dst_y.max(0.0) as u32);
            RESULT_SUCCESS
        }
        _ => RESULT_INVALID_CONTEXT,
    }
}
