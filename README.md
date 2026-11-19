# 页间 iPad

Rust 漫画阅读器，保留物理书架和纸张开合动画，优先适配 iPad。界面由 egui / wgpu 绘制，SQLite 保存书库，支持本地漫画导入与 Aidoku 社区源。

在 Actions 中运行「页间 iPad 模拟器截图」。流程先检查实际启动、主要页面、联网社区索引与设备横屏，成功后自动生成供已配置的 LiveContainer 使用的未签名 IPA。相关源码推送也会触发同一流程。

当前版本为 0.1.1 / build 4。先导出完整备份，再沿用已有数据容器更新。已验证旧书库升级、数据容器迁移后的封面及阅读内容；回退旧版需用更新前备份。构建和平台说明见 `BUILD.md` 与 `rust-reader/platform/ios/README.md`；最终验证记录见 `IMPLEMENTATION.md`。

公开源码按 GPL-3.0 发布。模拟器结果不能代替具体 iPad 上的侧载、触控、文件分享和耗电验证。
