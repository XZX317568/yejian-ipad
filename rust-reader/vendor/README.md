# iOS drawable size patch

`eframe 0.37.0` and `egui-wgpu 0.37.0` are copied from their published crates.
Their upstream project is `emilk/egui`; both are licensed MIT OR Apache-2.0.
The two license texts in this directory apply to both copies.

Only three expressions differ from the published Rust sources:

- `egui-wgpu/src/winit.rs`: both surface creation paths use `outer_size()` on iOS.
- `eframe/src/native/wgpu_integration.rs`: native surface resize uses the same size.

On iOS, winit 0.30.13 `inner_size()` subtracts UIKit safe-area insets, whereas
`egui-winit::screen_size_in_pixels()` deliberately uses `outer_size()`. The Metal
layer covers the whole window. Using the smaller surface with full-window UI
coordinates stretched and clipped the bottom navigation, even while egui's
hitboxes were inside its reported safe area.

Android and desktop expressions remain `inner_size()`. The app's own layout
still avoids `Context::content_rect()`; this patch changes drawable resolution,
not touch target spacing. Review these three changes when updating egui/winit,
and remove the Cargo overrides once upstream uses consistent iOS dimensions.

Evidence before the patch: `artifacts/rust/ipad-startup-audit/run-44520003076/`.
