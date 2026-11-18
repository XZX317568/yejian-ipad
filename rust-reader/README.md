# 页间 Rust 客户端

在页间书架上，点中央封面看详情和章节，点书名下的「开始阅读」或「继续阅读」直接进入漫画。直接续读会回到上次阅读的章节、页码和长图内的位置，返回键回到书架。下载的同源同作品合为一本，新书按章节顺序打开。

Android arm64 开发包和 Windows 启动器在 `../artifacts/rust/v0.1.1/delivery/`。书架、纸张动画和阅读状态都在 Rust 中实现，egui / wgpu 负责交互与绘制，SQLite 保存书库。旧 Flutter 应用位于 `../app`，两版使用各自的应用标识和数据目录，旧数据通过备份迁入。

```powershell
cd D:\Documents\aidoku\rust-reader
cargo run --locked -j 1 -- --data-dir .\data
```

构建、后台 GPU 验收和 iPad 工程入口见 `../BUILD.md`。真机验证状态、书源支持范围和本次修复记录见 `../IMPLEMENTATION.md`。

原书架来自 Wenku8 Flutter v0.5.6 的几何和动效实现，版权及 GPL 条款在 `licenses/`。本客户端使用 GPL-3.0-only；复用的其他库保留其原许可证。字体为上游 Droid Sans Fallback，Apache-2.0。

