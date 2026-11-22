# 页间 Rust 平台桥

这里只放 Android / iOS 的**外壳**和 Rust 侧的小桥接库，不包含主界面。主界面、书架和阅读器仍在 `rust-reader/src`。

## 主 Cargo 接入

`rust-reader/Cargo.toml` 需要把本目录加进 workspace 和依赖：

```toml
[workspace]
members = ["crates/reader-core", "crates/source-engine", "platform"]

[dependencies]
yejian-platform = { path = "platform" }
```

主 crate 已经同时声明了 `rlib`、`cdylib`、`staticlib`，因此 Android 会生成 `libyejian.so`，iOS 会生成 `libyejian.a`。包名和 `[lib] name` 保持 `yejian`，不要改成别的名字，Android 清单和 iOS 链接参数都按这个名字写好了。

## 入口

Android 由主模型实现 `android_main`，eframe 0.37 需要把 `AndroidApp` 放进 `NativeOptions`。
注意签名：`android-activity` 的胶水代码是在 Rust 内部调用这个符号的（winit 0.30 官方示例同样如此），
所以**用 Rust ABI 的普通 `fn`，不要加 `extern "C"`**；`AndroidApp` 不是 C 结构体。

依据：本机 `.tools/inspect/android-activity-0.6.1/src/native_activity/glue.rs:663` 里是
`extern "Rust" { pub fn android_main(app: AndroidApp); }`，`src/lib.rs` 也写明
“This must be an exported, unmangled, `"Rust"` ABI function”。

```rust
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    yejian_platform::init();
    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "页间",
        options,
        Box::new(|cc| {
            // Copies finish on a Java worker thread; repaint from there.
            let ctx = cc.egui_ctx.clone();
            yejian_platform::set_event_waker(move || ctx.request_repaint());
            Ok(Box::new(App::new(cc)))
        }),
    );
}
```

iOS 由主模型导出 `yejian_start_app`；`main.swift` 用 `@_silgen_name("yejian_start_app")` 直接引用它
（静态库只有被链接期引用时才会拉进可执行文件，不能用 `dlsym` 代替）。winit 自己调用 `UIApplicationMain`，所以 Swift 侧不能先调用 `UIApplicationMain`：

```rust
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn yejian_start_app() {
    yejian_platform::init();
    let _ = eframe::run_native(
        "页间",
        eframe::NativeOptions::default(),
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    );
}
```

`run()` 在 iOS 上不会返回（事件循环接管进程），`yejian_start_app` 里直接调用即可，不需要返回码。

## UI 里的接入方式

`src/ui.rs` 直接调用 `yejian_platform::*`，不需要再加 `src/platform.rs` 或 `mod platform;`。
批次处理：`batch.kind == PickKind::Backup` → `Command::Restore`，否则 → `import_files(batch.paths())`。

## 从文件管理器“用页间打开”

`RustActivity` 注册了 `ACTION_VIEW`（`content://` 与 `file://`，覆盖 zip/cbz/rar/cbr/cbt/tar/gz/json 和 `application/octet-stream`）。
冷启动走 `onCreate` 里的 `getIntent()`，运行中走 `onNewIntent`（`launchMode=singleTask`）。两条路径和选择器完全一样：
复制到 `filesDir/inbox` 后入队一个普通批次（真实 requestId），`kind` 按 MIME/扩展名判定——JSON 记为备份，其余按漫画归档；
纯 `.zip` 备份与 cbz 无法从元数据区分，会记为漫画，用户仍可在设置页手动恢复。

## 返回键（Back）

Android 的返回不再由系统直接关闭应用：`RustActivity` 只记录请求（`nativeBackPressed` → Rust 原子标志 + 唤醒），
是否退出由 UI 决定。接口：

```rust
pub fn take_back_requested() -> bool;   // 取走一次请求；原子交换，可每帧调用
pub fn finish() -> Result<(), PlatformError>; // 仅在根页面调用；Android = Activity.finish()，iOS 返回 Unsupported
```

两条投递路径，必须一起接：

1. **手势返回 / 预测性返回**（API 33+，target 36 默认开启；清单已写
   `android:enableOnBackInvokedCallback="true"`）：走 `OnBackInvokedDispatcher`，不会产生按键事件，
   由 `take_back_requested()` 送达。
2. **3 键导航**：`KEYCODE_BACK` 先进入 native 输入队列，winit 把按键标成 handled，Java 的
   `onBackPressed` 不会触发，所以 UI 还要把 `egui::Key::BrowserBack`（以及桌面/Escape）当成返回。

UI 里每帧取一次即可：

```rust
let back = yejian_platform::take_back_requested()
    || ctx.input(|i| i.key_pressed(egui::Key::BrowserBack) || i.key_pressed(egui::Key::Escape));
if back {
    if self.close_top_dialog() {
        // 弹窗优先关闭
    } else if self.screen != Screen::Library {
        self.go_back(); // reader → detail → library
    } else {
        // 根页面：只有这里才允许退出
        #[cfg(target_os = "android")]
        let _ = yejian_platform::finish();
    }
}
```

因为注册了 `OnBackInvokedCallback`，系统在根页面不会自动退出应用——这正是要做 `finish()` 的原因。

## 稳定接口

```rust
pub fn init();
pub fn pick_comics() -> Result<i64, PlatformError>;
pub fn pick_backup() -> Result<i64, PlatformError>;
pub fn poll_import() -> Result<Option<ImportBatch>, PlatformError>;
pub fn import_pending() -> Result<bool, PlatformError>;
pub fn set_event_waker(waker: impl Fn() + Send + Sync + 'static);
pub fn share_file(path: &Path) -> Result<(), PlatformError>;
pub fn export_file(path: &Path) -> Result<(), PlatformError>;
pub fn inbox_dir() -> Result<PathBuf, PlatformError>;
pub fn app_data_dir() -> Result<PathBuf, PlatformError>;
pub fn safe_insets() -> Result<Insets, PlatformError>;
```

- `pick_comics()` 立即返回请求号；选择结果和复制在后台完成。
- `pick_backup()` 只允许选 `zip`/`json`，返回的批次 `kind == PickKind::Backup`；`pick_comics()` 的批次是 `PickKind::Comics`。两个选择器同一时间只能开一个。
- 启动时调用一次 `set_event_waker(|| ctx.request_repaint())`，复制完成后外壳主动唤醒界面。装好 waker 后，Android 的 `poll_import()` 在空闲帧只是一次原子读（不会产生 JNI 调用），所以每帧调它也不会造成持续 tick；没有 waker 时才需要退回“仅在 `import_pending()` 为真时每 250 ms 查一次”。
- `poll_import()` 拿到 `Some(batch)` 后：`batch.kind == Comics` 就把 `batch.paths()` 交给归档导入；`Backup` 就取第一个文件交给 `Command::Restore`。`request_id == 0` 表示 iOS 启动时从 `Documents/Inbox` 迁入的文件。
- `ImportBatch::status` 是 `Completed`、`Partial`、`Canceled`、`Error`；`failed` 里保留每个失败文件的显示名和原因。
- `share_file(path)` 打开系统分享面板（Android `ACTION_SEND` + 自带只读 `FileShareProvider`，iOS `UIActivityViewController`）；`export_file(path)` 是“另存为”（Android `ACTION_CREATE_DOCUMENT` 选目录后流式写入，iOS 等同分享面板里的“存储到文件”）。备份生成后两者之一即可导出，都不需要存储权限。
- `inbox_dir()` 是稳定的 app 私有目录。Android 为 `filesDir/inbox`，iOS 为 `Application Support/Yejian/inbox`。
- `safe_insets()` 返回物理像素。Android 在 Java 里读 `WindowInsets`；iOS 在 Swift 里读 `safeAreaInsets × screen.scale`。
- 非移动端调用会返回 `PlatformError::Unsupported`；桌面主界面可以完全忽略这些接口。

## 平台目录

- `android/`：最小 Gradle 工程、`RustActivity`、清单、`gradlew`。applicationId 为 `app.yejian.reader.rust`，与旧 Flutter 应用并装。
- `ios/`：Xcode 工程、Swift 文档桥、`Info.plist`、`Yejian/Assets.xcassets`（AppIcon 共 18 个尺寸槽位，同一套书本线条，`tools/generate-appicon.ps1` 可重生成）。Bundle id 同为 `app.yejian.reader.rust`。
- `../tools/build-rust-android.ps1`：cargo-ndk + Gradle 打包入口，不安装、不发布。
- `../tools/build-rust-ios.sh`：在 macOS 生成静态库并放到 Xcode 期望的目录。

## 明确的缺口

- 没有商店截图/宣传素材，也没有自定义启动画面（iOS 用 `UILaunchScreen` 空字典的系统底色）。图标两端都已就绪：Android 矢量/自适应，iOS `Assets.xcassets/AppIcon.appiconset` 的 18 个不透明 PNG 槽位，图案是同一套书本线条。
- Android 已注册 `ACTION_VIEW`（用页间打开），但没有 Share Extension 式的“分享到页间”目标。
- iOS 没有 Share Extension/App Group；需要开发者账号和签名才能补。冷启动的“拷贝到页间”由 `Documents/Inbox` 扫描覆盖，运行中的文档打开由动态 delegate 覆盖。
- 本机没有 macOS/Xcode，也没有连接 Android 设备，因此 iOS 工程和移动端运行都还没有真机验收。
