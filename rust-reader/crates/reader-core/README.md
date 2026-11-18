# yejian-core

页间 Rust 客户端的真实本地书库。这个 crate 只处理数据：把用户选择的漫画复制进书库、建立按页索引、保存阅读进度、读写设置、导出/恢复整库备份、同步双端进度，并从旧 Mangayomi JSON/ZIP 备份迁移本地漫画元数据。它不打开旧 Isar 数据库，也不依赖 Flutter。

`Library` 内部是 `Mutex<rusqlite::Connection>`；一个实例可以 `Send + Sync` 交给后台导入线程和 UI 共用。同一根目录也可以再打开一个 `Library`，由 SQLite WAL 和 busy timeout 协调。

## 接口

```rust
use std::path::Path;
use yejian_core::{Book, ChapterLink, ChapterMetadata, Library, SUPPORTED_FORMATS};

let library = Library::open(root)?;                // 建库并迁移 schema
let books: Vec<Book> = library.list_books("")?;    // 标题/作者/分类搜索
let book = library.import(Path::new("volume.cbz"))?;
let page: Vec<u8> = library.page_bytes(&book.id, 0)?;
library.save_progress(&book.id, 0, false)?;             // 页顶，等价于 fraction = 0
library.save_position(&book.id, 0, 0.42, false)?;       // 页内 42% 处（长条无缝滚动）
library.set_category(&book.id, "收藏")?;
library.remove_from_library(&book.id)?;

// 章节身份来自源，不靠标题猜测；一本书只有一个章节关联
let metadata = ChapterMetadata {
    source_id: "source".into(),
    manga_id: "manga".into(),
    manga_title: "作品名".into(),
    chapter_id: "chapter-12".into(),
    chapter_title: "第12话".into(),
    chapter_number: Some(12.0),
    volume_number: Some(2.0),
};
library.link_chapter(&book.id, &metadata)?;
let links: Vec<ChapterLink> = library.list_chapter_links()?;

library.set_setting("reading_mode", "vertical")?;
let mode = library.get_setting("reading_mode")?;   // Option<String>

let report = library.export_backup(Path::new("library.backup"))?;
let restored = library.import_backup(Path::new("library.backup"))?;
let legacy = library.import_legacy_mangayomi_backup(Path::new("mangayomi.backup"))?;

// 统一恢复入口：按内容自动判断原生备份 / 旧 Mangayomi JSON / 旧 Mangayomi ZIP
let summary = library.restore(Path::new("whatever.backup"))?;
summary.imported_count();
summary.updated_count();
summary.skipped_count();
summary.failed_count();

// 双端续读：只传进度/分类，不传漫画文件
let records = library.export_progress()?;
let report = library.apply_progress(&records)?;
report.applied_count();
report.skipped_count();
report.unchanged_count();
```

`SUPPORTED_FORMATS` 公开当前可导入和读取的扩展名（CBZ/ZIP/CBT/TAR/CBR/RAR/PNG/JPEG/WebP），UI 可以直接用它生成文件选择器过滤条件。

`backup()` 是 `export_backup()` 的短别名。`export_backup()` 返回 `BackupReport`，包含导出数量和因档案缺失跳过的书，`is_complete()` 表示是否全部导出。

`import_backup()` **只接受原生 YeJian 备份 ZIP**。文件不是原生格式时直接报错，并提示改用 `restore()`，不会把旧 JSON/ZIP 当成成功恢复。它返回 `BackupImportReport`：`imported_count()`、`updated_count()`、`skipped_count()`、`failed_count()`，以及逐本的跳过/失败原因。

`restore()` 是 UI 选择备份后的统一入口，按文件内容判断来源：

- 原生 YeJian 备份 ZIP → 恢复书、封面、进度和设置
- Mangayomi JSON 或含 `*.backup.db`/`*.json` 的 ZIP → 迁移旧数据
- `.tachibk`、损坏或未知格式 → 明确报错

返回 `RestoreSummary`，逐项列出 `imported`、`updated`、`skipped`、`failures`，并用 `source` 标明识别出的备份类型。原生备份还会返回 `links_restored` 和 `link_failures`（章节关联恢复数量与冲突原因）；`is_complete()` / `is_fully_restored()` 也要求 `link_failures` 为空，章节关联没恢复成功不会被算成“全部恢复”。旧备份中本地档案已不存在的书会进入 `skipped`，带上原路径和原因，不会让整个恢复假报成功。

## 行为约定

- `Book.current_page` 和 `page_bytes(book_id, page)` 都是 **0 基**。旧 Mangayomi 的 `lastPageRead` 是 1 基，迁移时会减一。
- `Book.page_fraction` 是页顶相对整张图高度的位置，取值 `0.0..=1.0`；长条无缝滚动用它精确复原页内位置。`save_progress()` 保持原合同，等价于 `save_position(..., 0.0, ...)`。
- `save_position()`/`save_progress(..., private = true)` 只校验书、页码和比例，不写入任何内容；私密阅读不改变已保存页、`page_fraction` 和 `last_read_at`。
- 有限但越界的比例会被夹到 `0.0`/`1.0`（滚动到底部的浮点误差不应报错）；`NaN` 和无穷大直接报错。数据库里的比例始终有限且在范围内。
- 进度允许回退，重新打开库后仍保持最后保存的页。
- `remove_from_library()` 只把书标记为隐藏，不删除书库里的档案副本，也不删除用户原文件。再次导入相同内容会恢复那本书并保留进度和分类。
- 书的 `id` 是复制内容（文件或图片目录）的 SHA-256 十六进制值。重复导入同一内容不会新建书，也不会丢进度。
- `import()` 一次处理一个文件/目录。批量导入由 UI 逐个调用；一个文件失败不会回滚其他文件。
- `page_bytes()` 返回编码后的 PNG/JPEG/WebP 字节，不在这里解码整页。

## 章节关联

`link_chapter(book_id, &ChapterMetadata)` 把一本书显式绑定到源章节：

```rust
pub struct ChapterMetadata {
    pub source_id: String,
    pub manga_id: String,
    pub manga_title: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub chapter_number: Option<f64>,   // 保留 2.5 这类番外小数
    pub volume_number: Option<f64>,
}
pub struct ChapterLink { pub book_id: String, pub metadata: ChapterMetadata }
```

- `source_id`、`manga_id`、`chapter_id` 去掉首尾空白后必须非空；其余字段原样保存（只去首尾空白）。
- 一个 `book_id` 只有一个关联；重复关联同一本书会更新它自己的记录（用于重新下载后纠正），不会产生重复行。
- `(source_id, manga_id, chapter_id)` 唯一。若该章节已属于**另一本书**，调用直接报错并指出已有 `book_id`，不覆盖、不合并、不丢数据。不同源的同名作品不会互相合并。
- `list_chapter_links()` 只返回可见书的关联，按源 → 作品标题（自然顺序）→ 卷号 → 章节号排序。缺卷号的章节归入本作品**首个明确卷组**（最小卷号）；本作品完全没有任何卷号时，所有章节同属一个无卷组。同一组内有编号章节在前、按数值升序，`2.5` 这类小数落在 2 和 3 之间；缺编号的章节排在编号章节之后，按 `chapter_title` 自然顺序稳定兜底。
- 隐藏（`remove_from_library`）的书不出现在列表里，但关联行保留；再次导入同一内容会恢复这本书和它的关联。
- 关联不进入 `list_books()` 的含义，也不进入双端进度同步；只有原生备份会携带它。

## 导入与页面

支持：

- `.cbz`、`.zip`
- `.cbt`、`.tar`
- `.cbr`、`.rar`（RAR5/RAR4/RAR1.3；加密、分卷 RAR 明确报错）
- 图片目录（可含子目录）

ZIP 条目按自然顺序排列，`2.png` 在 `10.png` 前。ZIP 页会核对条目名、解压大小和 CRC32；TAR 页会核对条目名和大小；目录页会核对相对路径、文件大小并拒绝符号链接。所有归档条目名都按数据处理，抽取备份时使用逐段安全检查，不会把 `..` 或绝对路径当输出路径。

RAR/CBR 只解码到内存，从不按归档条目名写文件，所以不存在路径穿越落盘。页面按自然顺序排列；解压会核对条目名、解压大小，并在 RAR 提供 CRC32 时校验。当前不支持密码、加密头和分卷 RAR。

限制：

- 单页编码字节上限 64 MiB
- 封面源图上限 32 MiB
- 封面解码像素上限 120,000,000
- 归档/目录条目上限 20,000
- 备份单条目上限 8 GiB
- RAR 单页上限 64 MiB
- RAR 整本缓存上限 128 MiB / 2,048 页；超过上限时自动退化为逐页有界读取，不会无限占用内存

封面用 `image` 解码后缩到最长 480×720 的 JPEG，写入 `covers/`。封面优先取文件名含 `cover` 的页，取不到再按自然顺序尝试前面的页；单页损坏时仍可能用后面的页生成封面，而读取损坏页会返回带原因的 `anyhow::Error`。

## 书库目录

```text
<root>/
  library.sqlite3
  archives/
    <sha256>.cbz
    <sha256>/...          # 图片目录导入
  covers/
    <sha256>.jpg
```

导入先写 `archives/.import-*.tmp`，校验并生成封面后再原子改名到最终路径；失败会清掉这次创建的临时文件和未入册副本，不碰源文件。

## 备份格式

`export_backup()` 写一个 ZIP：

```text
manifest.json
archives/<book-id>.<ext>       # 或 archives/<book-id>/<relative>
covers/<book-id>.jpg
```

`manifest.json` 带 `format = "yejian-library-backup"`、版本、书元数据、阅读状态（含 `page_fraction`）、章节关联（`links`）和全部设置。`page_fraction` 与 `links` 是后加的可选字段：旧备份没有它们时仍可恢复。恢复时每本书独立处理，坏条目进入 `failures`，其他书继续；已有书只有在备份更新时间不旧于本地时才覆盖阅读状态，否则进入 `skipped` 并保留本地进度。设置按备份内容恢复。

旧备份没有 `page_fraction`（而不是显式 0）时：如果备份页与本地页相同，保留本地已有的页内位置；如果页不同，位置落在该页页顶。这样用旧备份恢复不会把用户较新的页内阅读位置冲掉。章节关联只在目标书没有关联、或关联的是同一章节时写入；如果目标书已经关联到另一章节，会保留本地关联并把原因放进 `link_failures`/`summary.link_failures`，不会静默改写。

## 旧 Mangayomi 备份迁移

`import_legacy_mangayomi_backup()` 读取：

- 旧 Mangayomi `.backup` ZIP 内的 `*.backup.db` JSON
- 直接给出的 Mangayomi JSON 文件

原始 JSON 会原样存入 `legacy_backups.raw_json`，每本漫画或章节的原始记录和导入结果存入 `legacy_records`；旧 `settings` 额外保存在设置键 `legacy.mangayomi.settings_json`。迁移只导入在**当前设备上仍可读**的本地档案路径，找不到的档案（整本或其中某一话）记录为 `missing_archives`，导入失败记录为 `failures`，不会丢弃原始备份数据。`restore()` 会把 `missing_archives` 放进 `skipped`，把 `failures` 放进 `failures`。

不支持直接读取 Isar 数据库、Tachiyomi/Mihon `.tachibk`、加密 ZIP 或未知格式；这些情况返回明确错误。

## 双端进度同步

`export_progress()` 返回 `Vec<ProgressRecord>`：`book_id`（内容 SHA-256）、`current_page`、`page_fraction`、`last_read_at`、`updated_at`、`category`、`removed`。这里 `updated_at` 是阅读状态时间（真正翻页保存的时刻），不是导入时间或分类编辑时间；刚导入、从未阅读的书不会导出第 0 页记录。只包含当前可见且已阅读过的书；私密阅读不写库，所以不会出现在导出中。记录只包含阅读状态和分类，不包含漫画文件、章节关联或路径。本轮尚未为“从未阅读书的纯分类修改”单独同步。

`apply_progress()` 在同一事务里合并远端记录：

- 只有远端阅读状态时间严格新于本地时才覆盖，保留远端传入的时间戳；新设备导入时写入的“导入时间”不会覆盖旧设备的历史进度。
- 页面允许回退；同一条同步批次里重复记录按时间戳取最新。
- `page_fraction` 随记录一起同步；旧客户端没有该字段时按 `0.0` 解码，远端非法值（`NaN`/无穷）按 `0.0` 应用，不阻塞整批合并。
- 找不到的书、越界页会进入 `skipped`，不会被当成“第 0 页同步成功”。
- 没有阅读状态（第 0 页且没有 `last_read_at`）的记录会进入 `skipped`，不会被当成一次真实阅读。
- `removed = true` 的记录会被跳过；没有可靠 tombstone 之前不传播删除，也不会隐藏本地书。

## 验证

测试位于 `tests/library.rs` 和 `tests/reading_v3.rs`，覆盖自然排序、重复导入、进度回退与重开、私密进度、混合损坏档案、损坏页/缺档案错误、新设备未读导入不覆盖旧进度、明确回读可覆盖、旧 schema 升级保留原阅读时间、原生备份恢复与部分报告、恢复后档案字节一致、被篡改备份不产生脏书、统一 `restore()` 自动识别、旧 Mangayomi JSON/ZIP 迁移、缺失旧档案、非原生文件不假报成功、CBR/RAR 真实页面读取、CBR 备份恢复、进度同步时间/重复/缺书，以及不支持格式。`reading_v3.rs` 另覆盖 v1/v2 库升级 `page_fraction = 0`、同页不同 `page_fraction` 持久与重开、私密不写、0/1/越界夹取、`NaN` 拒绝、进度/备份往返、旧备份无 offset 不冲掉本地位置、章节关联唯一身份/隐藏/重复关联/跨源同名不合并、番外 `2.5` 排序与缺编号标题兜底、备份链接冲突时保留本地关联且完成判定为假；`chapter.rs` 的单元测试覆盖缺卷号归入首个明确卷组、全无卷号单一分组和小数章节排序。RAR 测试用 `rars` 写入器生成 RAR1.3、RAR4 和 RAR5 夹具，不依赖外部 RAR 文件；加密、分卷、压缩（非 store）RAR 与真实设备上的 RAR 兼容性未在本 crate 的夹具中覆盖，读取路径由上游 `rars` 负责。按主模型约定，本子任务不运行 `cargo test`；构建与测试由主模型统一执行。

## 第三方许可

RAR/CBR 读取使用 `rars` 0.9.x（当前锁定 0.9.4，MIT OR Apache-2.0，https://github.com/bitplane/rars）。本 crate 整体仍按 GPL-3.0-only 发布；复用的库保留其原许可证，详见 `THIRD_PARTY.md` 和仓库 `licenses/`。
