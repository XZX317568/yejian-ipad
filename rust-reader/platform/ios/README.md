# iOS 外壳

`Yejian.xcodeproj` 是一个最小的 iOS 应用工程：Swift 入口调用 Rust 导出的 `yejian_start_app()`，winit 自己调用 `UIApplicationMain`，eframe/wgpu 使用 Metal 绘制。Bundle id 为 `app.yejian.reader.rust`。

## 只有 Windows，怎样装到 iPad？

公开仓库的 GitHub Actions 提供 macOS 构建环境，Windows 本机无需安装 Xcode。当前流程先运行 iPad 模拟器检查，通过后才生成 IPA。

1. 在 **Actions → 页间 iPad 模拟器截图 → Run workflow** 发起验收。相关源码推送也会自动触发。
2. `sim` 和 `package / ipad` 都成功，且 **Yejian-iPad-Sim** 内有通过的 `verification.json` 后，下载 **Yejian-iPad**，解压获得 `Yejian-0.1.1-4-ipad-unsigned.ipa`。它是 iPad 真机目标的 arm64 程序。
3. 已配置好 LiveContainer 的 iPad 可将 IPA 存进“文件”，从 LiveContainer 选择此包。更新时沿用现有应用和数据容器，避免删除旧书库。设备上的签名、安装和启动结果需要在该 iPad 上确认。

2026-11-19 已在 iPad 模拟器运行六个生产画面。旧流程曾出现退出后拍到桌面、以及横屏检查失败被日志管道掩盖的问题，对应绿色记录已作废。当前关卡要求每次启动独立的内容就绪报告，横屏必须实际加载封面和阅读图片，并验证旧库升级。最新交付和验证结果见工程根目录 `IMPLEMENTATION.md`。

## 在 macOS 的工程根目录构建

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
bash tools/build-rust-ios.sh --device
# 或者：
bash tools/build-rust-ios.sh --sim
```

脚本会把 `libyejian.a` 与 Rust 生成的系统库链接参数 `native-static-libs.rsp` 一起放到 `rust-reader/platform/ios/Libs/iphoneos/` 或 `Libs/iphonesimulator/`。Xcode 按当前设备或模拟器选择对应目录。
脚本会识别 `CARGO_TARGET_DIR`（如果设置了就按它找产物），默认仍是 `rust-reader/target`。

### 构建 iPad 版实际需要的环境

- macOS 13+ 与 Xcode 15+（工程 `objectVersion = 56`、`IPHONEOS_DEPLOYMENT_TARGET = 15.0`）。
- macOS 上的 Rust 工具链，以及 `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`（Intel Mac 用 `x86_64-apple-ios`）。
- 从 Xcode 直接安装到真机时，在 Signing & Capabilities 里选择自己的 Apple Developer Team。导出供 LiveContainer 使用的 IPA 可以省去这一步。
- 先跑 `bash tools/build-rust-ios.sh --device`（真机）或 `--sim`（模拟器），否则 `-lyejian` 会链接失败——这是有意的，缺库必须报错而不是静默少一个入口。

`bash tools/build-rust-ios.sh --device --ipa` 会编译并在 `artifacts/rust/ipad/` 生成 IPA。只做编译检查可用 `--xcodebuild`。

## 图标

`Yejian/Assets.xcassets/AppIcon.appiconset` 已随工程提供：18 个 PNG 槽位（iPhone 20/29/40/60pt 的 @2x、@3x，iPad 20/29/40/76pt 的 @1x、@2x 加 83.5pt @2x，以及 App Store 用的 1024pt），全部是不透明 24bpp PNG。App Store 校验不接受带 alpha 通道的图标，圆角也由系统裁切，所以图里不带圆角、不带透明。

图案和 Android 图标是同一套矢量：墨色纸面 `#14161C`、米色双页 `#F4EBD8` / `#EADFC5`、书脊 `#C7B998`、40% 墨色文字线 `#668E8474`、金色书签 `#D9A93C`；几何逐点取自 `platform/android/app/src/main/res/mipmap-anydpi/ic_launcher.xml` 的 108×108 视口，只在 iOS 上把整本书居中收到图标宽度约 58%（iOS 的图标没有自适应图标那套 72dp 安全区）。

重新生成（Windows PowerShell，或装了 `pwsh` 的 macOS；只用 System.Drawing/GDI+，不需要 ImageMagick）：

```powershell
cd rust-reader/platform/ios
pwsh -File tools/generate-appicon.ps1 -Preview   # -Preview 另存 .tools/ios-appicon-preview.png
```

`Info.plist` 里写了 `CFBundleIconName = AppIcon`，两套 target 配置都有 `ASSETCATALOG_COMPILER_APPICON_NAME = AppIcon`，`Assets.xcassets` 已放进 `PBXResourcesBuildPhase`，Xcode 会把它编成 `Assets.car`。改设计只改 `platform/ios/tools/generate-appicon.ps1` 里的几何（和 Android 矢量保持同步）再重跑。

## 链接的系统框架

Xcode 链接 `-lyejian`、界面所需的系统框架，以及 `rustc --print native-static-libs` 为当前目标列出的系统库。静态库中的 Rust 代码和 Swift 文档桥在这一步连接。

`rusqlite` 用的是 `bundled` SQLite，不另加 `-lsqlite3`。主 Cargo 精简 feature 时保留 wgpu 的 `metal`。iOS 用 `downlevel_defaults()` 加适配器实际分辨率和对齐限制：模拟器只支持 15 个 inter-stage shader variables，WebGPU 默认请求 16 会在首帧之前失败；egui 绘制只需其中两个。

`Yejian/main.swift` 直接引用 `rust-reader/src/lib.rs` 导出的 `yejian_start_app()`，使 Xcode 在链接时从 Rust 静态库中拉入启动代码。这个入口调用 `run()`，初始化文档桥并启动阅读器。winit 的事件循环负责调用 `UIApplicationMain`。

## 文档打开与导入

`Info.plist` 声明了 cbz/cbr/cbt、zip、rar、tar 文档类型，并设置了 `UIFileSharingEnabled`、`LSSupportsOpeningDocumentsInPlace`；另外有 `NSLocalNetworkUsageDescription`（局域网阅读进度同步的授权文案）和 `NSAppTransportSecurity/NSAllowsArbitraryLoads`（用户书源可能是 http）。文件可以：

1. 在应用内点“导入” → `UIDocumentPickerViewController` 多选（`asCopy: true`）；
2. 在“文件”里选择“拷贝到页间” → iOS 把文件放进 `Documents/Inbox`，下次启动时桥接层自动迁入 Application Support/inbox；
应用运行中从外部直接送入 URL 的路径目前没有可靠验收；优先使用应用内文件选择。winit 0.30.13 通过系统通知接收生命周期事件，不会注册自己的 application delegate。桥接层只在该位置为空时安装文档回调；若侧载宿主已有 delegate，则保留宿主。

复制同样先写 `.import-<uuid>.part`，再原子 move 到最终文件名，重名加 ` (2)`。

`Documents/Inbox` 的扫描/搬移在后台串行队列里跑，不阻塞首帧；`NSFileProtection` 与安全区桥不需要额外配置。
`org.yejian.cbz/cbr/cbt/rar` 用 `UTExportedTypeDeclarations` 声明（本应用自己定义的类型），`LSItemContentTypes` 同时引用系统自带的 zip/tar UTI。

## Rust ABI

`DocumentBridge.swift` 导出：

| C 符号 | 说明 |
| --- | --- |
| `yejian_ios_init` | 延迟到启动完成后准备目录并扫描 `Documents/Inbox`，保留已有宿主 delegate |
| `yejian_ios_pick_documents` | 参数 `0`=漫画归档、`1`=zip/json 备份；返回请求号，`-1` 表示当前没有可展示的控制器 |
| `yejian_ios_poll_import` | 返回 `strdup` 的 JSON，空闲返回 NULL |
| `yejian_ios_free_string` | 释放上面的字符串 |
| `yejian_ios_inbox_dir` | Application Support/Yejian/inbox |
| `yejian_ios_app_data_dir` | Application Support/Yejian |
| `yejian_ios_safe_insets` | `left,top,right,bottom` 物理像素 |
| `yejian_ios_set_waker` | 注册复制完成回调，Swift 在主队列调用它唤醒 Rust |
| `yejian_ios_share_file` | 用 `UIActivityViewController` 导出文件；`0` 成功、`-1` 无控制器、`-2` 文件不存在 |

JSON 里多了 `"kind": "comics" | "backup"`，启动时从 `Documents/Inbox` 迁入的批次固定为 `comics`。

所有 UIKit 调用都必须在主线程；winit 的 iOS 事件循环本来就在主线程。桥接层对非主线程调用做了 `DispatchQueue.main.sync` 兜底。

## 能力边界

- **没有 Share Extension**。真正的“分享到页间”（从其他 App 的分享面板直接进入）需要第二个 target、App Group、entitlement 和开发者账号签名；本工程没有伪造这一能力。
- **外部 open-in-place**：没有可靠的 URL 事件转发；通过“拷贝到页间”产生的 `Documents/Inbox` 文件会在下次启动时扫描，应用内文件选择走独立回调。
- 没有商店截图/宣传图，也没有自定义启动画面图片：启动屏用 `Info.plist` 的 `UILaunchScreen` 空字典（系统给的纯色底）。App 图标只在 asset catalog 里，不带 App Store 宣传素材。
- 模拟器运行能验证 UIKit / Metal 启动和布局，不能代替用户 iPad 的签名、LiveContainer 兼容、真实分屏手势、帧率与耗电测量。现有产物均未包含用户签名。
