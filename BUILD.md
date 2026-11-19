# 页间本地开发与验收

## Rust 客户端（当前迁移入口）

2026-11-20第二版界面预览在 `artifacts/rust/ui-redesign-v2-20261120/windows/`，双击 `打开新版预览.cmd` 可直接体验带原创样书的新界面，数据存于独立的 `preview-data/`。`打开页间.cmd` 使用旁边的 `data/`，供导入自己的漫画或恢复备份。第一轮预览保留在 `ui-redesign-20261120/`。

既有0.1.1交付在 `artifacts/rust/v0.1.1/delivery/`，Android包为 `Yejian-0.1.1-rust-arm64-debug.apk`。本轮仅生成Windows界面预览，未更新移动端安装包。从旧Windows版迁入书库时，先在旧版设置中创建完整备份，再到新版恢复。

```powershell
cd D:\Documents\aidoku\rust-reader
cargo run --locked -j 1 -- --data-dir .\data
```

后台 GPU 验收使用真实临时书库、实际 UI 输入和离屏渲染，不接管主屏：

```powershell
cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\artifacts\rust\v0.1.1
cargo test --locked --workspace -j 1 -- --test-threads=1
```

Windows 可执行文件单独构建，`cargo build --locked -j 1 --bin yejian` 生成 `target/debug/yejian.exe`。动画与布局取自生产 UI，第二轮录制在 `artifacts/rust/ui-redesign-v2-20261120/书架开合.mp4`，早期版本保留在各自的交付目录。

### 界面更新的定向验收

```powershell
cd D:\Documents\aidoku\rust-reader
$env:YEJIAN_DESIGN_ONLY='1'
cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\artifacts\rust\ui-redesign-v2-20261120
Remove-Item Env:YEJIAN_DESIGN_ONLY
```

该入口输出16张实际GPU截图与 `design-acceptance.json`，覆盖手机、iPad横竖屏、深色、320pt和1194pt的200%字号。真实点击缩略封面并验证阅读的是同一本书，再检查详情、返回、历史续读、主题与WASM筛选；使用原始文字边界及实际裁剪区域检查可见性。书架几何另有边界连续性单测。完整回归、书源筛选和iPad矩阵使用既有入口，输出见该目录下 `regression/`、`filters/`、`ipad/`。`前后对比.png` 与 `界面预览.png` 仅排版实际截图，`书架开合.mp4` 为本轮160帧录制。

### iPad 布局检查与模拟器验收

Windows 上先用同一套 Rust UI 做离屏布局和交互检查，不抢占桌面焦点：

```powershell
cd D:\Documents\aidoku\rust-reader
$env:YEJIAN_IPAD_ONLY='1'
cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\artifacts\rust\ipad-startup-audit\local-final
Remove-Item Env:YEJIAN_IPAD_ONLY
```

档位为 1024×1366、1366×1024、834×1194、1194×834、744×1133、507×1024、320×1024，外加无安全区对照。每组覆盖书架、书源、历史和设置；另测详情、阅读及分屏 200% 大字。书架热区必须达到 44pt，且位于模拟安全区内。`ipad_navigation_stays_inside_native_safe_area` 另以 egui 原生安全区输入复现底部导航遮挡，防止只测桥接层而遗漏平台行为。

公开仓库 `XZX317568/yejian-ipad` 的 `.github/workflows/ipad-sim.yml` 在相关源码推送后运行，也支持手动触发。它在 macOS 上编译并启动真正的 iPad 模拟器：检查进程存活、目标画面的就绪记录、导航安全区及横屏方向，保存截图和启动日志；社区源检查读取真实索引。横屏由独立的 `YejianUITests` 目标用 `XCUIDevice.orientation` 转动设备，书架、详情、阅读均须完成。只有 `sim` 成功，才调用 `ipad.yml` 生成真机目标的未签名 IPA。`tools/capture-ipad-simulator.sh` 是验收入口；失败也上传诊断产物。

2026-11-19 发现旧流程可把已退出应用的 iPad 桌面截图记为成功，因此旧绿色记录不能作为启动证据。修复后的真实模拟器截图在 `artifacts/rust/ipad-startup-audit/`。本地离屏测试不替代 UIKit、Metal 和设备运行；实机触控、文件选择、LiveContainer 启动及功耗另行验证。

验收还要求 XCTest 从正在运行的 Rust UI 获取本次启动的就绪报告，确认封面、阅读图片和原有书本身份。脚本先将模拟器书库改成 build 3 的绝对路径格式，再由新程序升级，验证重新安装后的旧库仍能读取。最终 `verify-ipad-snapshots.py` 核对十个竖屏和四个横屏的 JSON/PNG、导航安全区、旧库升级、社区索引和实际 WASM 筛选结果，成功才生成 `verification.json`。workflow 显式启用 Bash 的 `pipefail`；日志写入成功不能掩盖测试失败。`test-ipad-snapshots.py` 覆盖缺证据、空白阅读、旧报告、错误方向、筛选结果未生效等拒绝场景。

当前 iPad 源码版本为 **0.1.1 / build 5**。直接运行 `ipad.yml` 只编译打包，不表示模拟器已经通过。build 3 的 run `44532550519` 与 `44538203121` 曾因缺少 `pipefail` 出现绿色误报，横屏结果作废。正式交付状态与产物路径以 `IMPLEMENTATION.md` 为准。

书源筛选交互可独立在后台验证。测试书源只在显式 Debug 验收中安装，不进入 Release 包或用户书源目录：

```powershell
cd D:\Documents\aidoku\rust-reader
$env:YEJIAN_FILTERS_ONLY='1'
cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\artifacts\rust\source-filters-build5
Remove-Item Env:YEJIAN_FILTERS_ONLY
```

该流程实际点击热门/最新、加载下一页、题材、应用、取消与重置，验证 WASM 收到条件后的结果；另检查横竖屏、分屏和 200% 字体。`examples/source_probe.rs` 可对指定的独立书源目录运行真实联网检查，分别报告筛选、列表、搜索的结果或失败原因。

`rust-reader/vendor/` 的 eframe / egui-wgpu 0.37.0 覆盖修正了 iOS Metal 画布与输入坐标尺寸不一致的问题：iOS 创建和调整画布均使用完整窗口尺寸，应用布局另外遵守安全区。升级这两项依赖时，应检查 `vendor/README.md` 列出的三个表达式；只在上游已解决尺寸不一致时移除 Cargo patch。

安卓 arm64 包：

```powershell
cd D:\Documents\aidoku
.\tools\build-rust-android.ps1
```

Rust 客户端沿用自己的标识 `app.yejian.reader.rust`。已交付 Android 0.1.1 的 versionCode 为 4，与 Flutter 对照版分开安装；本轮 iPad 修复未重新打包 Android。

有 Mac 时可运行 `bash tools/build-rust-ios.sh --device --release --ipa`。IPA 未签名，需使用已配置的侧载工具；当前没有用户 iPad 上的安装成功证据。步骤见 `rust-reader/platform/ios/README.md`。

Cargo 单任务，Rust 和 Gradle 串行。Android Java 堆为 768 MiB，metaspace 为 384 MiB，worker 为 1。运行前检查物理与可提交内存。0.1.1 / code 4 交付 APK 为 23,818,090 字节。若增量打包留下旧 ZIP 空洞，可先把这一个构建输出 APK 移入工程检查点，再用 `-SkipRust` 重新打包。

## Flutter 对照版本

在 Windows PowerShell 中进入 `D:\Documents\aidoku\app`。本项目固定使用根目录 `tools/flutter.ps1`，它调用项目专用 Flutter 3.50.0 / Dart 3.14.0，并使用 `.tools/` 下的缓存。全局 Flutter 不参与本工程。

## 编译前

查看当前可用内存与其他构建；同机重编译错峰，避免和其他聊天的 Gradle 同时启动。当前入口将 Cargo 并发限为 1，Gradle worker 限为 1、Java 堆限 1024 MB、metaspace 限 512 MB、direct memory 限 256 MB、code cache 限 128 MB、活动处理器数限为 2，使用串行 GC，Kotlin 使用同一进程。以上单项上限不等于进程总内存上限。Windows 的可用虚拟内存也会限制 Dart 编译，物理内存有余量不代表不会耗尽提交空间。

```powershell
Get-CimInstance Win32_OperatingSystem |
  Select-Object FreePhysicalMemory, FreeVirtualMemory
Get-PSDrive -Name D
Get-Process -Name java,dart,rustc,cargo -ErrorAction SilentlyContinue
```

依赖版本保存在 `app/pubspec.lock`。只有缺依赖时运行：

```powershell
..\tools\flutter.ps1 pub get --enforce-lockfile
```

## 安卓

```powershell
..\tools\flutter.ps1 build apk --debug --target-platform android-arm64 --no-pub
```

输出：`app/build/app/outputs/flutter-apk/app-debug.apk`。这是 Android arm64 开发测试包；最终是否已构建成功以 `IMPLEMENTATION.md` 的记录为准。部分依赖包含其他 ABI 的零散原生库，但完整 Flutter/Rust 库只为 arm64 构建，不能据此宣称支持其他 ABI。包名为 `app.yejian.reader`，不替换上游安装。该命令不安装到设备、不发布。功耗和最终流畅度需使用 profile/release 构建在设备上测量。

## 书架预览

```powershell
cd widgetbook
..\..\tools\flutter.ps1 build web --release --no-pub --target lib/shelf_preview.dart
cd ..\..
node tools/preview-server.cjs
```

浏览器打开 `http://127.0.0.1:4173`。已有服务时直接复用，不重复启动。这里复用生产书架与纸张动画组件，封面和阅读记录是明确标注的原创示例；完整应用的书库验证另外使用真实 LibraryScreen 和临时 Isar 数据库完成。

恢复组件目录预览时，将构建目标改为 `lib/main.dart`。同一 web 输出目录一次只保留一个入口。

## 定向检查

在 `app` 目录串行运行：

```powershell
..\tools\flutter.ps1 test --no-pub --concurrency=1 test/core/design/reading_shelf_test.dart
..\tools\flutter.ps1 test --no-pub --concurrency=1 test/modules/library/library_archive_shelf_test.dart test/widgets/mobile_bottom_navigation_mode_test.dart
..\tools\flutter.ps1 test --no-pub --concurrency=1 test/core/design/yejian_library_flow_test.dart
..\tools\flutter.ps1 test --no-pub --concurrency=1 test/modules/manga/reader/reader_progress_persistence_test.dart
..\tools\flutter.ps1 test --no-pub --concurrency=1 test/core/design/design_literals_test.dart test/core/design/accessibility_test.dart
```

视觉测试将三个尺寸的截图写入 `app/build/acceptance`。Windows 字体只在测试时加载，不随应用重新分发。

iPad 的布局可在上述测试和预览中查看；IPA 编译、签名和真机验收需要 macOS/Xcode 与对应设备，Windows 构建不覆盖这部分。
