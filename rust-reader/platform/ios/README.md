# iOS 外壳

`Yejian.xcodeproj` 是一个最小的 iOS 应用工程：Swift 入口调用 Rust 导出的 `yejian_start_app()`，winit 自己调用 `UIApplicationMain`，eframe/wgpu 使用 Metal 绘制。Bundle id 为 `app.yejian.reader.rust`。

## 只有 Windows，怎样装到 iPad？

GitHub Actions 可以临时提供 Mac 来编译。项目里的 `.github/workflows/ipad.yml` 需要手动运行。私有仓库会消耗账号套餐内的计算和产物存储额度，运行前核对剩余额度与支出上限。

1. 将构建所需源码和工作流放入自己的私有仓库默认分支后，在 **Actions → 页间 iPad 安装包 → Run workflow** 发起一次构建。
2. 构建成功后下载 **Yejian-iPad**，解压下载的 ZIP，里面是 `Yejian-0.1.1-2-ipad-unsigned.ipa`。构建使用 iPad 真机目标，检查 arm64 架构、应用标识和可执行文件，再保存产物。
3. 已经使用 LiveContainer 的 iPad 可以把 IPA 存进“文件”，打开 LiveContainer，点右上角 **＋** 选取 IPA，再点“页间”启动。首次运行时可先导入一本 CBZ，翻页后退出再打开，确认阅读位置能恢复。

流程已在 Windows 上准备，首次 macOS 编译和 iPad 实机运行仍待验证。

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

`rusqlite` 用的是 `bundled` SQLite（自己编译 `sqlite3.c`），不需要也不应该加 `-lsqlite3`，否则可能链接到系统里更旧的那份。主 Cargo 精简 feature 时要确认 wgpu 留着 `metal`，否则运行时找不到适配器（eframe 默认会带上，别手写成只剩 `dx12`）。

`Yejian/main.swift` 直接引用 `rust-reader/src/lib.rs` 导出的 `yejian_start_app()`，使 Xcode 在链接时从 Rust 静态库中拉入启动代码。这个入口调用 `run()`，初始化文档桥并启动阅读器。winit 的事件循环负责调用 `UIApplicationMain`。

## 文档打开与导入

`Info.plist` 声明了 cbz/cbr/cbt、zip、rar、tar 文档类型，并设置了 `UIFileSharingEnabled`、`LSSupportsOpeningDocumentsInPlace`；另外有 `NSLocalNetworkUsageDescription`（局域网阅读进度同步的授权文案）和 `NSAppTransportSecurity/NSAllowsArbitraryLoads`（用户书源可能是 http）。文件可以：

1. 在应用内点“导入” → `UIDocumentPickerViewController` 多选（`asCopy: true`）；
2. 在“文件”里选择“拷贝到页间” → iOS 把文件放进 `Documents/Inbox`，下次启动时桥接层自动迁入 Application Support/inbox；
3. 应用运行中通过“用页间打开”收到 URL → 动态安装的 `UIApplicationDelegate` 捕获并复制。

复制同样先写 `.import-<uuid>.part`，再原子 move 到最终文件名，重名加 ` (2)`。

`Documents/Inbox` 的扫描/搬移在后台串行队列里跑，不阻塞首帧；`NSFileProtection` 与安全区桥不需要额外配置。
`org.yejian.cbz/cbr/cbt/rar` 用 `UTExportedTypeDeclarations` 声明（本应用自己定义的类型），`LSItemContentTypes` 同时引用系统自带的 zip/tar UTI。

## Rust ABI

`DocumentBridge.swift` 导出：

| C 符号 | 说明 |
| --- | --- |
| `yejian_ios_init` | 延迟到启动完成后准备目录、安装 delegate、扫描 `Documents/Inbox` |
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
- **冷启动 open-in-place**：没有 delegate 的启动参数，无法在 `didFinishLaunching` 之前接管 URL。通过“拷贝到页间”产生的 `Documents/Inbox` 文件会在启动时扫描；运行中的 open-in 由动态 delegate 处理。
- 没有商店截图/宣传图，也没有自定义启动画面图片：启动屏用 `Info.plist` 的 `UILaunchScreen` 空字典（系统给的纯色底）。App 图标只在 asset catalog 里，不带 App Store 宣传素材。
- 本机是 Windows，没有 macOS/Xcode，也没有 iOS 设备；图标 PNG、`Contents.json` 和 `pbxproj` 引用都在本机生成并检查过，但**这里没有编译、签名、归档或真机运行**，iPad 上 Swift/IPA 是否可用仍未验证。
