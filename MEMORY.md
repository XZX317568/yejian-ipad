# 项目接续

> ⚠️ 2026-11-22 校正（本地仓库实测）：本文件后文提到的 `build7` 分支、`a4d87cb`、PR #4 **在本地不存在**（`main` 仅 2 个提交、无 remote）；「剩余工作」7 项**代码已实现于工作区但未入库**。当前状态与待办以 `NEXT.md` 为准，运行规则见 `AGENTS.md` 与 `OPERATIONS.md`。

- 页间 / Yejian 面向安卓手机和 iPad。2026-11-18 用户允许借鉴后改进界面。保留 Wenku8 Flutter v0.5.6 的物理书脊与纸张开合，允许简化操作。产品优先级见 `漫画阅读器研究与产品方案-2026-11-17.md`。
- 主工程为 `rust-reader/`。Rust 自绘界面使用 egui / wgpu，SQLite 管理书库，Aidoku WASM 提供书源。包标识 `app.yejian.reader.rust`。旧 `app/` 和 Isar 数据独立，迁移走备份恢复。
- 0.1.1 的 Android arm64 签名开发包（code 4）、Windows 启动器及说明在 `artifacts/rust/v0.1.1/delivery/`。安卓 JNI 启动问题已修，旧手机实测可进书架；本轮仅更新 iPad。`artifacts/rust/delivery/` 为 0.1.0 及其独立数据。
- 封面进详情，书名下按钮直接续读，返回回到原入口。v3 数据保存长图页内位置并按源与作品关联各章。前台下载队列支持显式暂停、继续、重试入库。2026-11-19 按 iPad 重排书架、详情、书源、历史与设置，控件改为触控尺寸。测试与能力边界见 `IMPLEMENTATION.md`。
- 构建先查物理和提交内存，本地 Cargo 单任务，桌面与安卓构建错峰。入口见 `BUILD.md`。`YEJIAN_IPAD_ONLY=1` 离屏验收覆盖八组 iPad 尺寸和 200% 字体；本轮证据在 `artifacts/rust/ipad-startup-audit/` 与 `.tools/ipad-audit-20261119/`，不接管主屏。
- 公开仓库 `XZX317568/yejian-ipad` 使用标准 macOS runner。2026-11-20 最新交付 **0.1.1/build 6**：`artifacts/rust/sources-round2/run-44700355317/Yejian-iPad/Yejian-0.1.1-6-ipad-unsigned.ipa`（28,691,499字节，arm64/iOS15+/未签名）。PR #2（reader-round1 阅读器第一轮）与 #3（sources-round2 书源浏览详情）已合并进 main（1a4af1d）。模拟器 run44700355317 十四画面通过；本地 193 项测试通过。
- 2026-11-20 build 7 本地全量收口（SWE-2 分三批执行）：process_page_image 书源图片后处理、设置 button/link、追更封面并入 source-covers、后台定时追更（默认1小时、静置零重绘）、Android WebView + Windows 网页验证/站点 UA（web-cookies.json 兼容升级）、AniList + Kitsu 跟踪（send_isolated 隔离通道、私密拦截、进度只增不减）、书架抗锯齿打磨。产物 `artifacts/rust/build7-20261120/`：APK 40,201,618B（versionCode 7）、EXE 106,881,024B；213 项单测 + 四套离屏 GPU 验收全 PASS，日志 `.tools/fusion-20261120/`。build 6 IPA 增大已核实 = 两份全 BMP 阅读字体（Sans 10.25MB + Serif 14.4MB）。待实机：iPad IPA 推送公开仓库、LiveContainer、触控/文件/功耗、拷贝漫画香香腐宅过盾。
- 已完成两轮优化：①阅读器——并发下页（4路）、按作品记忆阅读设置（mode/rtl/裁白边/适应宽度/亮度/双页首页单独，reader-prefs.json，键 source:源ID\x1f漫画ID）、书源viewer提示默认、竖向跨章无缝滚动、私密模式不写在线页码、设置面板安全区。②书源——封面网格结果、异步封面缓存（yejian-covers线程+4路HTTP、磁盘缓存 source-covers/、LRU120、仅加载可见格子）、详情页重做（封面头部、继续阅读、追更改次要按钮、章节行读/下载状态、默认最新在前）、build6、亮度滑条可见。
- 剩余工作（用户可能交给其他AI）：书架观感打磨（书脊保留）；安卓/Win 网页验证登录（现仅 iOS WKWebView）；process_page_image 宿主未实现；书源设置登录/按钮/链接类选项；后台定时追更；追更封面并入新缓存；跟踪服务未迁。实机待验证：LiveContainer 侧载、触控、真实书源封面、功耗帧率；拷贝漫画/香香腐宅真实可用性。验收入口与构建约束见 BUILD.md（单任务 cargo -j1）。
- 2026-11-20 build 7 已导入仓库：分支 `build7`（362d327 原样导入另一助手的本地改动 + a4d87cb 审查修复：安卓 WebView 仅允许 http/https、关闭文件/内容访问，iOS build 6→7），PR #4 未合并。run 44757360489 模拟器与打包通过：`artifacts/rust/build7-20261120/run-44757360489/Yejian-iPad/Yejian-0.1.1-7-ipad-unsigned.ipa`（28,804,511 字节）。214 项测试通过。待实机：AniList 授权码换 token（client_secret 为空，可能只有粘贴 token 可用）、安卓 WebView 过盾、定时追更。网络不稳，gh/git 需重试。
