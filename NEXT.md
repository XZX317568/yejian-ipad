# NEXT · 自主迭代待办

规则见 `AGENTS.md`。状态只有 `todo` / `doing` / `done` / `blocked`；一次只做一条，做完写证据路径。

## 已核实的现状（2026-11-22，4 个 swe-2 只读盘点）

- **`main` 只有 2 个提交**（`39381ba` 上游基线、`d9c99ae` Rust 阅读器），**没有 remote、没有 `build7` 分支**；MEMORY.md 里提到的 `build7`/`a4d87cb`/PR #4 在本地不存在。
- **工作区有 118 项未提交改动** → 当前工作区是这批工作的唯一副本。任何自主迭代之前必须先入库，否则没有回滚点。
- MEMORY.md 末尾「剩余工作」7 项**全部已经实现**（在工作区里，未入库）：书架书脊 `src/shelf.rs:125,279`；安卓/桌面网页登录 `platform/android/.../RustActivity.java:21,385` + `src/sources.rs:4734-4762`；`process_page_image` `src/sources.rs:6169`；登录/按钮/链接设置项 `src/sources.rs:1727-1728`；后台追更 `src/sources.rs:1549,4315-4354`；追更封面入 source-covers `src/sources.rs:1927-1972,3121,8670`；跟踪服务 `src/trackers.rs:1,20,280` + `ui.rs:5160,5204`。→ 这份清单是**过时的**，真实缺口是「验收 + 入库」，不是「实现」。
- 垃圾文件：根 `NUL`(4232B)、`rust-reader/NUL`(126B，会破坏 rg 对目录的搜索)、`rust-reader/D tmpcheck.json`(340KB)、`Documentsaidoku.toolsshelf-selected.png`(67KB)。体积：`.tools/` 19.8GB（yejian-build20 8.0G、gradle 7.2G、flutter-source 1.8G、pub-cache 1.5G）、`rust-reader/target-main-check/` 184MB、`.tmp-sim/` 28.7MB、`artifacts/` 121.6MB（只读）。
- `.gitignore` 缺：`.tmp-sim/`、`NUL`、`**/target-main-check/`、`**/__pycache__/`。

## 每轮必跑的验证（来源 BUILD.md + 源码核对，均已确认有效）

1. `cargo test --locked --workspace -j 1 -- --test-threads=1` — 增量 1–3 分钟，全量编译 10 分钟+。
2. `cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\artifacts\rust\v0.1.1` — 完整 GPU 离屏验收。
3. 定向：`$env:YEJIAN_DESIGN_ONLY='1'` / `YEJIAN_IPAD_ONLY` / `YEJIAN_FILTERS_ONLY` 三选一后跑同一 acceptance 命令；分发逻辑在 `src/bin/acceptance.rs:1851-1859`。

## 待办

### P0 先做（不做完不许开始改代码）
- [done] **盘点真实状态** — 见上方「已核实的现状」。
- [todo] **清垃圾 + 补 .gitignore**：删上面 4 个垃圾文件（`NUL` 用 `\\?\` 路径删）、补忽略项。验收：`git status` 里不再出现这些名字，`rg` 可正常搜 `rust-reader`。
- [todo] **检查点入库**（`app/` 已冻结，不再分组细修）：① `archive: 冻结 Flutter 旧壳 app/` 一条提交，原样收进 101 项改动，之后任何轮次不许动 `app/`；② `reader: 入库当前 Rust 阅读器` 一条；③ `docs: 迭代规则与产品定义` 一条。验收：`git status` 干净、`git log --oneline` 每行可读、`git status app/` 为空。
- [todo] **`tools/loop-check.ps1`**：一条命令跑「cargo test -j1 + 相关 acceptance」，输出 PASS/FAIL 与日志路径，失败非零退出。必须内置：① `TMP`/`TEMP` 重定向（否则假 `LNK1104`）② 4 条收尾断言（`git status`/`git log -1`/退出码/acceptance JSON）③ `-audit`：复核上一轮 `done` 的三个证据 + `OPERATIONS.md` 五节的文档断言 ④ `-smoke`：端到端冒烟（启动→书架非空→打开一本→渲染出页→截图非空）。验收：可重复运行，失败非零退出。
- [todo] **锁与 diff 预算落到脚本**：`.tools/loop.lock` 建/删/检测 + 本轮 diff 行数与文件数统计（超 300 行或 10 文件即 FAIL）。验收：故意超预算时脚本报 FAIL。

### 主体（物理书脊与纸张开合 · 最高优先级 · 目标端 = iPad）

**五段实测状态（2026-11-22，`.tools/refspec/current/SPEC.md` 10.6KB）**

| 段 | 状态 | 说明 |
|---|---|---|
| 书脊 | ✅ 完整 | `Plane(u,v)` 顶点投影 + `lerp_to_gamma` 顶点色，无 shader；front 微斜 `(spine, spine·0.10)`，face 收 `(cover·0.66, −0.30·cover)` |
| 抽出 | ⚠️ 半成品 | `ShelfMotion` 有 focus(lane) + 0.42s 过冲 tween + Snap，缺参考的 `weight=cos²` 侧书下沉与 origin 拔出位移 |
| 摊开 | ⚠️ 半成品 | 缺 `ArchivePlaneTransition`（平行四边形斜切交换面，`Cubic(.18,.72,.18,1)`） |
| 翻页 | ⚠️ 半成品 | `PageSlide` 越界 `limit=max(w·0.35,24)`；缺 `ArchiveReveal`（`Interval(.08–.72)`×easeOutCubic + `travel(-8,-6)`） |
| 合上 | ⚠️ 半成品 | 飞行三曲线 travel/depart/contact + 17 列 mesh 有，回位欠缺 |

**已确认同构**：`paper_depth = peel·floating·(17+7·sin(vπ)+3·breeze·sin(tπ))` 与参考逐字一致（`motion.rs:51-59`）；`Snap` 阈值 `|v|·pitch≥50` 等于参考的 50px/s，`coast=ln(|v|/4)/4.4` 同源；`rubber_band = sign·limit·(1−1/(1+|d|/limit))`（`motion.rs:17-22`）。**唯一额外项**：飞行阶段有"有限差分光照"，参考没有——留/删待用户决定。

- [done] **参考规格书 `REFERENCE.md`**：四份细则在 `.tools/refspec/{shelf,motion,wave,current}/SPEC.md`（共 51.7KB），关键常量与公式汇总进 `REFERENCE.md`。
- [done] **差距清单**：见下方六轮计划，按对"像真书"的影响排序。

**主体六轮计划（一轮一项，每轮出 8–12 秒新旧并排录制，用户裁决后写回本文件）**

1. **抽出**：补 `weight=cos²(d/1.48·π/2)` 的侧书下沉（`railY=d·2.5+d²·0.45`）与 origin 拔出位移 `pull=cover·0.84·w(1.24−0.24w)`。现状只有 focus+0.42s 过冲 tween。
2. **摊开**：补 `ArchivePlaneTransition`——`Cubic(.18,.72,.18,1)`、incoming `(−24·dir,+12)` + 斜切 `slant=min(180,.46w)`、outgoing `(+10·dir,−4)`、sheet 自屏底升起左移 18。**这是"纸张开合"的核心一段。**
3. **翻页**：补 `ArchiveReveal`——`Interval(.08–.72)×easeOutCubic`、剪口 + `travel(−8,−6)·(1−p)`、`p<.05` 不可命中、纯剪裁无阴影。
4. **合上/回位**：飞行三曲线已有（travel/depart/contact + 17 列 mesh），补回位段收敛与 landed 状态。
5. **常量对齐**：`ease_out` 改 `easeOutCubic`、修 `select` 曲线 0.02 漂移、对齐 `scale` 取值、摊开槽数、地影。**另需用户决定**：飞行阶段的"有限差分光照"（参考没有）留还是删。
6. **边缘**：clamp 硬边 vs 循环池 `edgeSlots=⌈(w/2+footprint+pull·0.66)/pitch⌉+2`，按需补。

每轮开跑前：`git status` 必须干净（P0 检查点入库已完成），并按 `OPERATIONS.md` 检查磁盘/内存闸门。
- [todo] **主体五段逐段打磨**（书脊 / 抽出 / 摊开 / 翻页 / 合上，一段一轮）：每轮附 8–12 秒离屏录制（新旧并排），由用户裁决；裁决写回本文件，后续轮次必须遵守。
- [todo] **帧率与手感**：主体动作在 60fps 下不掉帧；桌面离屏测不出的项，写清"需要用户在真机确认什么"。

### 配套（围绕主体，不得抢优先级）

- [todo] **7 项逐条验收**：按上面证据位置，为每一项补一条可机器判定的测试或截图断言；缺哪补哪。验收：`cargo test` 覆盖这 7 个符号所在模块。
- [todo] **Aidoku 源兼容打磨**（当前范围内唯一的源生态；其他生态暂缓）：把 Aidoku WASM 的加载、筛选、搜索、详情、翻页后处理做到稳定，逐源验证。验收：`examples/source_probe` 的真实源结果 + 单测。
- [todo] **iPad 实机待验（安卓暂缓）**：LiveContainer 侧载、触控尺寸与 44pt 热区、真实书源封面、功耗与帧率、拷贝漫画/香香腐宅过盾、AniList 授权码换 token。写清「需要用户在 iPad 上做哪一步」即止。
- [todo] **iPad 尺寸矩阵基线**：`YEJIAN_IPAD_ONLY=1` 跑 1024×1366 / 1366×1024 / 834×1194 / 1194×834 / 744×1133 / 507×1024 / 320×1024 + 200% 字号，产出截图与 JSON，作为打磨前的对照基线。

### 工程侧
- [todo] **磁盘清理**：`.tools/` 里 8.0G+7.2G+1.8G+1.5G 的构建缓存、`target-main-check/` 184MB、`.tmp-sim/` 28.7MB；先出清单再删，`.tools/` 下的日志保留。

## 需要用户决定（AI 不自行拍板）

- **弯曲实现路径**：继续沿用参考的 49×17 网格顶点位移，还是评估 shader（片元着色器算卷曲+光照）方案？证据见 `REFERENCE.md` 的「外部资料」。这不是参数差异，是路线差异，且会影响后续五轮怎么做。
- **飞行阶段的"有限差分光照"**：参考实现没有，是自创项。留（更像纸弯）还是删（更贴参考）？
- **搜索通道**：`web_search` 401 失效，需要修 key 或确认改用浏览器检索。

- 是否重建 git 历史/加 remote（`XZX317568/yejian-ipad`）：涉及推送，等用户点头。

## 轮次日志
- 2026-11-22 第 0 轮：4 个 swe-2 并发只读盘点（测试基线 / build7 差集 / 卫生 / 验收入口），产出上方「已核实的现状」。
- 2026-11-22 基线结果：**220 passed / 0 failed / 3 ignored，EXIT=0，耗时 ≈10.8 分钟**，日志 `.tools/loop-baseline/cargo-test-20261122-retry3.log`。
  ⚠️ 踩坑：`lib.exe` 合并 AWS-LC 静态库时会往 `%TMP%`（本机为 `D:\tmp`）写 `lnk{GUID}.tmp` 中转文件，被外部清理程序秒删 → 连续 `LNK1104`（EXIT=101）。**解法：跑构建前把 `TMP`/`TEMP` 指向 `rust-reader` 外的稳定目录**（如 `.tools/loop-baseline/tmp`）。`loop-check.ps1` 必须内置这一步，否则每轮都会假失败。
