# Android 外壳

这是一个独立的、最小的 `NativeActivity` 工程，不引用、不修改旧的 `app/android` Flutter 工程。它复用项目 `.tools/gradle` 缓存和同一套 Gradle 9.3.1 / AGP 9.1.1 版本。

## 标识

- applicationId：`app.yejian.reader.rust`
- Java 类：`app.yejian.reader.RustActivity`
- Native library：`libyejian.so`（`android.app.lib_name=yejian`）
- minSdk 24 / targetSdk 36 / compileSdk 36

独立 applicationId 让新 Rust 客户端与现有 `app.yejian.reader` 并装，不会覆盖旧应用的 Isar 数据。

## 构建

从仓库根目录：

```powershell
.\tools\build-rust-android.ps1
.\tools\build-rust-android.ps1 -Profile release
```

脚本只负责构建和打包，不安装到设备、不发布。默认使用 arm64-v8a；其他 ABI 用 `-Abi` 指定。Gradle 使用 `.tools/gradle`，`-Xmx768m`、单 worker、`--no-daemon`。

APK 输出在 `app/build/outputs/apk/{debug,release}/`。

## Java → Rust 契约

`RustActivity` 提供以下 **public static** 方法：

| 方法 | 签名 | 说明 |
| --- | --- | --- |
| `pickComics` | `(I)J` | 打开 `ACTION_OPEN_DOCUMENT` 多选；`0`=漫画归档，`1`=zip/json 备份。返回请求号，`-1` 表示当前不可用或已有选择器 |
| `pollImport` | `()Ljava/lang/String;` | 取出下一个已完成批次 JSON；空闲返回 `null` |
| `isImporting` | `()Z` | 选择器打开中或仍在复制时为 `true`；Rust 只用它在导入期间按 250 ms 轮询 |
| `inboxDir` | `()Ljava/lang/String;` | 创建并返回 `filesDir/inbox` |
| `appDataDir` | `()Ljava/lang/String;` | 返回 `filesDir` |
| `safeInsetsPx` | `()Ljava/lang/String;` | 返回 `left,top,right,bottom` 物理像素 |
| `shareFile` | `(Ljava/lang/String;)Z` | 把 app 内文件交给系统分享/保存面板；`false` 表示文件不存在或 Activity 未就绪 |
| `exportFile` | `(Ljava/lang/String;)Z` | 打开 `ACTION_CREATE_DOCUMENT`“另存为”，用户选目录后后台写入；`false` 表示文件不存在或 Activity 未就绪 |
| `finishApp` | `()V` | Rust 在根页面请求退出时调用；内部在 UI 线程 `finish()` |

Rust 侧通过 `ndk-context` 拿到 Activity 的 class（避免 native 线程用系统 ClassLoader 找不到应用类），再调用这些静态方法。主 crate 不需要写任何 JNI 导出。

反方向只有一个导出：`RustActivity.enqueueResult` 在批次入队后调用 native 方法
`Java_app_yejian_reader_RustActivity_nativeImportFinished`（由 `yejian-platform` 提供），
用来唤醒 egui 的 `request_repaint`。符号缺失时 Java 捕获 `UnsatisfiedLinkError`，退回轮询。
返回键另有 `Java_app_yejian_reader_RustActivity_nativeBackPressed`：Java 只置位“请求过返回”并唤醒，
不会自己 `finish()`。

批次 JSON：

```json
{
  "requestId": 1,
  "kind": "comics",
  "status": "ok",
  "files": [{"name": "Saga.cbz", "path": "/data/user/0/app.yejian.reader.rust/files/inbox/Saga.cbz"}],
  "failed": [],
  "message": null
}
```

`kind` 为 `comics` 或 `backup`；`status` 为 `ok`、`partial`、`canceled`、`error`。`message` 只在错误或说明时出现。

## 导入行为

- 只使用 SAF（`ACTION_OPEN_DOCUMENT` + `EXTRA_ALLOW_MULTIPLE`），不申请 `READ_EXTERNAL_STORAGE`、`MANAGE_EXTERNAL_STORAGE` 或任何“所有文件”权限。
- 同样接受 `ACTION_VIEW`（文件管理器/邮件附件“用页间打开”）：冷启动在 `onCreate` 读 `getIntent()`，运行中在 `onNewIntent`（`singleTask`）处理，走同一条复制与入队路径。
- MIME 过滤覆盖 zip/cbz/rar/cbr/tar/cbt/gzip 和 `application/octet-stream`，避免部分文件管理器把漫画归档标成未知类型。
- 每个文件先写到同目录 `.import-<uuid>.part`，`fsync` 后 `renameTo` 最终名，完成后再入队；重名自动加 ` (2)`。
- 复制在单线程后台 executor 完成，不阻塞 UI 线程。
- 选择器取消、单个文件失败、全部失败都会产生一个批次，不会静默消失。
- 备份选择模式只列 zip/json；`kind=backup` 让 Rust 把文件交给 `Command::Restore`。
- `ACTION_VIEW` 的 `kind` 由 MIME 和扩展名判定：JSON 记为备份，其余记为漫画；纯 `.zip` 备份无法与 cbz 从元数据区分，会记为漫画。

## 轮询与唤醒

- Java 在 `enqueueResult` 里调用 native `Java_app_yejian_reader_RustActivity_nativeImportFinished`，Rust 侧置位“队列可能有数据”并唤醒 egui。
- 装了 waker 之后，Rust 的 `poll_import()` 在空闲帧只做一次原子读；没有 waker 时退回 `isImporting()` + 250 ms 轮询。

## 图标与名称

- 桌面名称来自 `strings.xml` 的 `app_name`（页间）。
- 图标是原创矢量：`res/mipmap-anydpi/ic_launcher.xml`（API 24–25 整幅）、`res/mipmap-anydpi-v26/ic_launcher.xml`（自适应，背景 `@color/ic_launcher_background` + `@drawable/ic_launcher_foreground`），墨色纸面 + 米色书页 + 金色书签。

## 导出备份

`shareFile(path)` 先把文件复制到 `cacheDir/share/<名字>`，再用 `ACTION_SEND` +
`FileShareProvider`（authority `app.yejian.reader.rust.files`，只读、只暴露这一个目录）
打开系统分享面板。`exportFile(path)` 走 `ACTION_CREATE_DOCUMENT`，用户选好目录后由后台线程
流式写入目标 URI，这是 Android 上真正可靠的“保存到 Downloads/云盘”。两者都不需要任何存储权限，
也不会暴露整个 `filesDir`。

清单只加了 `INTERNET`（书源查询和文件下载），没有任何存储权限。

## 生命周期与界面

- `RustActivity` 继承 `NativeActivity`，Rust 的 `android_main` 负责窗口、生命周期和绘制。
- 返回键：API 33+ 注册 `OnBackInvokedCallback`（清单 `android:enableOnBackInvokedCallback="true"`），
  API 24–32 覆盖 `onBackPressed()`；两者都只记录请求，等 Rust 决定“应用内后退”还是“根页面退出”。
  3 键导航的 `KEYCODE_BACK` 会被 winit 当成 `BrowserBack` 消费，UI 侧要一并判断。
- `configChanges` 包含旋转、窗口尺寸、深浅色、密度和导航变化，`screenOrientation` 不锁定；旋转不会重建 Activity，也不会锁死方向。
- 主题是 `Theme.DeviceDefault.NoActionBar`，保留系统栏，便于 Rust 读取并使用 insets。
- `onDestroy` 清空静态 Activity 引用、取消进行中的请求标记；导入线程是 daemon，进程销毁时随进程结束。

## 当前缺口

- 没有启动画面（只有桌面图标）；图标是矢量/自适应，商店素材需要另做 PNG。
- 冷启动时系统可能重复投递同一个 `ACTION_VIEW`（例如进程被回收后重建），同一文件会再复制一份并加 ` (2)`。
- 本机没有连接设备，也没有在这里启动 Gradle；APK 打包和真机行为由主模型统一构建后验收。
