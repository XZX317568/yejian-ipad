# yejian-sources

页间阅读器的原生 Aidoku 源码引擎。它是 Mangayomi v0.9.96 `app/rust/src/aidoku_wasm`
的独立 Rust 提取版：不依赖 Flutter/Dart，网络由 `reqwest` 直接执行，每个源独占一个
后台线程，公共 API 是标准 `Pin<Box<dyn Future<Output = T> + Send>>` 异步。

旧 `app/` 中的任何文件都没有改动；来源和 Apache-2.0 许可证见 `NOTICE` 与 `LICENSE`。

## 一、给 UI 的接口

```rust
use std::sync::Arc;
use yejian_sources::{
    EngineOptions, NetRequestData, PageContent, SourceCatalog, SourceEngine, SourceInstaller,
};

// 1. 本地源目录（一个源一个子目录，布局与解包后的 .aix 相同）
let catalog = SourceCatalog::new(app_data_dir.join("sources"));
let installed = catalog.list()?;                 // Vec<InstalledSource>，坏目录自动跳过
let one = catalog.get("source.id")?;             // 精确错误：缺文件/元数据/未安装
let manifest = &one.manifest;                    // name / version / languages / urls / listings

// 2. 从 Aidoku 源仓库 URL 浏览并安装（不内置任何源）
let installer = SourceInstaller::new(SourceCatalog::new(app_data_dir.join("sources")))?;
let index = installer.fetch_index("https://example.test/index.json")?;
for source in &index.sources {                   // id / name / version / icon_url / languages
    println!("{} v{}", source.name, source.version);
}
let installed = installer.install(&index, "source.id")?;  // 下载 .aix → 校验 → 原子替换

// 注意：SourceCatalog / SourceInstaller 是同步阻塞调用（文件与网络），
// UI 应放到后台线程执行；SourceEngine 的方法才是可 await 的异步接口。

// 3. 每个源一个引擎；用 Arc 共享给 UI，所有方法都可以并发 await
let engine = Arc::new(SourceEngine::load_installed(&installed)?);
// 或 SourceEngine::load(catalog, "source.id") / SourceEngine::load_wasm(manifest, bytes, options)

let features = engine.features();                // 哪些导出真实存在
let listings = engine.listings().await?;         // 静态 + 动态书单
let page1 = engine.browse(&listings[0].id, 1).await?;   // MangaPageResult { entries, has_next_page }
let results = engine.search("葬送的芙莉莲").await?;       // 等价 search_page(query, 1, vec![])

// 4. 详情 / 章节 / 页面
let manga = engine.manga_details(&results.entries[0].key).await?;
let chapters = engine.chapters(&manga.key).await?;
let pages = engine.pages(&chapters[0].key).await?;        // 需要先 chapters() 过
// 重启后从本地数据库恢复的下载任务：把持久化的完整对象传进来
let pages = engine.pages_with(manga.clone(), chapters[0].clone()).await?;

// 5. 图片下载：保留源的 Cookie 会话和请求头
for page in pages {
    match page.content {
        PageContent::Url { url, context } => {
            let resolved = engine.image_request(&url, context).await?; // Option<ImageRequest>
            let request = resolved.unwrap_or_else(|| yejian_sources::ImageRequest {
                url: url.clone(),
                headers: Vec::new(),
                body: None,
            });
            let response = engine.http_request(NetRequestData {
                method: "GET".into(),
                url: request.url,
                headers: request.headers,
                body: request.body,
                timeout_ms: 60_000,
            }).await?;
            if response.status_code == 200 {
                let bytes = response.body;
            }
        }
        PageContent::Image { image_data, .. } => { /* 源已经内联图片字节 */ }
        PageContent::Text(text) => { /* 文字页 */ }
        PageContent::ZipFile { url, file_path } => { /* 下载整章压缩包后按 file_path 取页 */ }
    }
}
```

下载恢复缓存按章节页列表指纹复用：URL/上下文、zip 路径、文本长度或内联图长度
变化时会重新抓取该章。URL 查询参数变化按新列表处理，不做通用 `stripQuery`
猜测。内联图片即使 `image_ref` 和长度都相同，也会比较实际字节；不同时只重写
该页。

下载任务保存的是搜索时拿到的完整 `Manga`/`Chapter`：`Manga::to_postcard_bytes`
与 `Chapter::to_postcard_bytes` 输出与 `get_page_list` 入参完全相同的 postcard
字节，`from_postcard_bytes` 用于重启后还原。作品快照按漫画存一份，多个章节任务
共享，不在任务 JSON 里重复章节列表。只有未记录快照的旧队列才按 key/title/url
回退；新任务记录的快照若缺失、损坏、id 非法或与任务不匹配会明确失败并提示回到
作品详情重新加入，绝不把半截对象发给源。

### 方法一览

| 调用 | 真实 Aidoku 接口 | 说明 |
| --- | --- | --- |
| `search(query)` | `get_search_manga_list(query, 1, [])` | 普通搜索 |
| `search_page(query, page, filters)` | `get_search_manga_list` | 带筛选/翻页 |
| `browse(listing_id, page)` | `get_manga_list` | 静态或动态书单 |
| `manga_details(id)` | `get_manga_update(details=true, chapters=false)` | 与缓存合并 |
| `chapters(id)` | `get_manga_update(details=false, chapters=true)` | 返回章节并写入缓存 |
| `pages(chapter_id)` | `get_page_list` | 依赖 `chapters()` 缓存 |
| `pages_for(manga_id, chapter_id)` | `get_page_list` | 只有 key 时的兜底 |
| `pages_with(manga, chapter)` | `get_page_list` | 下载恢复推荐 |
| `listings()` | `get_listings` + 静态 `source.json` | 去重 |
| `filters()` | `get_filters` | 仅动态筛选（见边界） |
| `base_url()` | `get_base_url` | 可选 |
| `image_request(url, ctx)` | `get_image_request` | 可选；无实现返回 `None` |
| `http_request(request)` | 原生网络 | 用源自己的 Cookie/请求头下载 |
| `deep_link(url)` | `handle_deep_link` | 可选深链 |
| `basic_login(key, user, pass)` | `handle_basic_login` | 可选账号登录 |
| `web_login(key, cookies)` | `handle_web_login` | 可选网页登录回填 Cookie |
| `migrate_key(kind, manga, chapter)` | `handle_key_migration` | 可选 key 迁移 |
| `set_fuel_per_call(n)` / `fuel_per_call()` | 本地 | 每次调用的 WASM fuel 预算；`0` 表示不限 |

`Manga.key` / `Chapter.key` 就是 `manga_details`、`chapters`、`pages` 使用的 id。
`MangaPageResult.entries[*].source_key` 会自动填好。

### 选项

```rust
let engine = SourceEngine::load_installed_with(&installed, EngineOptions {
    user_agent: Some("...".into()),
    request_handler: Some(Arc::new(|request| { /* 系统代理/DoH */ })),
    print_handler: Some(Arc::new(|line| log::debug!("{line}"))),
    partial_result_handler: None,
});
```

默认 UA 与旧 Dart 栈的源抓取 UA 一致。`request_handler` 是给平台层接系统代理、
DoH 或测试桩用的；返回 `NetResponseData` 时必须保留真实状态码和 `error` 含义。

## 二、错误语义（UI 必须区分）

| 变体 | 含义 / UI 动作 |
| --- | --- |
| `WebVerificationRequired { detail }` | 源需要 JS/WebView 或站点返回了人机验证页。**不要显示成空结果**，应提供“打开网页验证/换源”。 |
| `Network(_)` | DNS/TLS/超时/非法 URL，可重试。 |
| `Load(_)` | `.aix`/`.wasm`/元数据损坏，不能重试。 |
| `UnknownChapter(id)` | 还没调用 `chapters()`，先加载章节。 |
| `Unimplemented(_)` | 源没有导出这个功能（例如没有 `get_image_request`）。 |
| `EngineStopped` / `EnginePanicked(_)` | 源线程已退出；重新 `load`。 |
| `FuelExhausted { limit }` | 源单次调用超过 WASM 指令预算（通常是死循环）；提示用户换源或重试。 |
| `Io(_)` | 安装目录读写失败。 |

HTTP 4xx/5xx **不是** `Network` 错误：`http_request` 返回 `Ok(NetResponseData)`，
其中 `status_code` 和 `headers` 是真实值，`error` 只表示传输失败（此时状态码为 0）。

## 三、安装目录与安全

```text
<root>/<source-id>/
  source.json    # 必需，Aidoku 元数据（支持 info 包裹和扁平两种写法）
  main.wasm      # 必需，必须有 \0asm 魔数
  settings.json  # 可选，静态设置默认值会注入 defaults
  filters.json   # 可选，当前仅保留
  icon.png       # 可选
```

- `.aix` 大小上限 64 MiB，单条目 64 MiB，解压总量 128 MiB。
- `source-id` 只允许 `A-Z a-z 0-9 . _ -`，拒绝 `..` 和路径分隔符。
- 更新先写临时目录，全部校验通过后再原子替换；失败保留旧版本。
- 不内置、不推荐、不自动安装任何第三方源；只安装用户给出的仓库/包。

## 四、可靠性设计与未迁移边界

已迁移并可用：浏览、搜索、详情、章节、页面、动态书单/筛选、图片请求、
基础登录/网页 Cookie 注入、深链、key 迁移（只要源导出对应函数）。

明确边界：

- **不执行 JavaScript/WebView**。`js.context_eval` 等返回失败并升级成
  `WebVerificationRequired`；Cloudflare 等 403/429/503 验证页也会被识别。
- `get_home`、动态 `get_settings`、`process_page_image` 仍是未接线状态（与旧原生引擎一致）。
- 静态 `filters.json` 尚未解析成 UI 模型；`filters()` 只返回动态筛选。
- 每个源严格串行执行。每次调用有 WASM fuel 预算，默认 10 亿（约数秒），
  `SourceEngine::set_fuel_per_call(n)` 可调整、`0` 关闭；超限返回 `FuelExhausted`，
  只影响该源，不会让死循环永久占用 CPU。宿主网络时间不计入 fuel，仍受超时约束。
- 单次 HTTP 响应 body 上限 64 MiB（流式读取，超限拒绝）；超限按传输失败返回
  （`status_code = 0` + `error`），避免单页图片耗尽内存。
- 缓存上限：256 本漫画、8192 个章节；超出按插入顺序淘汰。
- 全局 `defaults` 存储是进程级、按 `source-id` 命名空间隔离；与旧 Dart 行为一致。
  当前只注入 `settings.json` 默认值，尚未提供 UI 修改设置/持久化的公共接口。
- HTTP 客户端每个源一个 Cookie 会话；重定向最多 10 次；超时默认 60 秒、上限 300 秒。
- TLS 使用内置 Mozilla webpki 根证书（TLS 1.3/1.2，启用证书链与域名校验），
  不依赖 `rustls-platform-verifier`，因此在无 JVM/Kotlin 的 Android NativeActivity
  构建中也能正常访问 HTTPS；代价是不使用系统额外安装的企业/用户根证书。
- 平台代理/DoH 尚未内置，用 `EngineOptions::request_handler` 接入。

## 五、测试

测试不访问外网、不使用付费/私有服务：

- `catalog`：安装/列出/删除、缺 wasm、坏元数据、路径穿越。
- `http`：本机临时 HTTP 服务，验证状态码、重复响应头、正文、传输错误、64 MiB 上限，
  以及自定义 TLS 配置确实装载 webpki 根证书与 h2/http1.1 ALPN。
- `repo`：Aidoku 新/旧索引、相对 URL、`index_v2` 重定向、坏 JSON。
- `engine`：用 `wat` 构造真实 WASM 模块，验证 postcard 往返、原解释器可复用、
  后台线程异步搜索、坏模块加载错误、JS 源返回 `WebVerificationRequired`、
  死循环触发 `FuelExhausted`。

本 crate 不自行启动完整重编译；由主模型统一串行执行 `cargo test -p yejian-sources`。
