# 页间 / Yejian

Rust 漫画阅读器，保留物理书架和纸张开合动画，优先适配 iPad，也包含 Android 和 Windows 入口。界面由 egui / wgpu 绘制，SQLite 保存书库，支持本地漫画导入与 Aidoku 社区源。

2026-11-20 源码更新包含第二轮界面改版、阅读字体、藏书缩略浏览，以及阅读手势、章节续读和书库恢复相关修复。Android 包装工程和构建脚本也已收入仓库。字体及复用组件的许可证保存在 `rust-reader/licenses/` 和 `rust-reader/vendor/`。

在仓库根目录运行 Windows 客户端：

```powershell
cd rust-reader
cargo run --locked -j 1 -- --data-dir .\data
```

构建 Android arm64 开发包：

```powershell
.\tools\build-rust-android.ps1
```

iPad 在 Actions 中运行「页间 iPad 模拟器截图」。流程先检查实际启动、主要页面、联网社区索引与设备横屏，成功后自动生成供已配置的 LiveContainer 使用的未签名 IPA。相关源码推送也会触发同一流程；请在对应提交的运行记录中确认结果。

源码版本仍为 0.1.1 / iPad build 5。此前 run 44563594979 已通过14个iPad模拟器画面及筛选结果检查，并生成未签名 IPA；该安装包早于本次界面和阅读修复。书源支持静态/动态筛选、排序、多选排除、数值范围及源列表，取消筛选保留原条件。

先导出完整备份，再沿用已有数据容器更新。已验证旧书库升级、数据容器迁移后的封面及阅读内容；回退旧版需用更新前备份。需要JavaScript或网页验证的源仍有限制。构建和平台说明见 `BUILD.md` 与 `rust-reader/platform/ios/README.md`；最终验证记录见 `IMPLEMENTATION.md`。

此仓库包含当前 Rust 客户端及其依赖的 `app/rust/sync_host`。开发记录中的旧 Flutter 对照工程、本地预览、测试截图、安装包和个人书库不随源码上传。

公开源码按 GPL-3.0 发布。模拟器结果不能代替具体 iPad 上的侧载、触控、文件分享和耗电验证。
