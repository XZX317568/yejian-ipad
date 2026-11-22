import Foundation

// Winit's iOS backend calls UIApplicationMain itself. The Xcode executable
// therefore enters through Rust, and Rust starts the eframe/wgpu event loop.
//
// Direct reference on purpose: the Rust code is a static library (`libyejian.a`),
// and only a link-time reference makes the linker pull the Rust object files
// into the executable. `dlsym` would leave the symbol unresolved at link time,
// so the archive member could be dropped (or the lookup simply fail) and the
// app would launch without Rust.
@_silgen_name("yejian_start_app")
func yejian_start_app()

NSLog("Yejian main: calling Rust")
yejian_start_app()
NSLog("Yejian main: Rust returned")
