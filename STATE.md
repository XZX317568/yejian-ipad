# STATE · 页间当前状态

## 目标（锁死，改它要用户点头）

在 iPad 上做出「像一本真书」的阅读器：书架书脊 → 抽出 → 摊开 → 翻页 → 合上。Rust 自绘（wgpu/egui）。安卓暂缓，源只做 Aidoku，`app/` 冻结。

## 阶段计划（外部评审后修订：先基建，再动形变）

- [~] **阶段 0 · 调参基建**
  - ✅ **参数外置**：`rust-reader/src/params.rs` + `rust-reader/params.json`（`YEJIAN_PARAMS` 可覆盖路径，含 `paper` / `snap` / `weight` / `ease` 四组）。**证据**：同一份已编译产物，改 `paper.base` 17→0 后 `motion/frame-045/060/075.png` 三帧 SHA256 全部不同，且无需重新编译。日志 `.tools/loop-baseline/stage0-params-live.log`
  - ✅ **录制管线**：`tools/record.ps1`（多变体 → 各自 mp4 → `hstack` 并排 `compare.mp4`）。ffmpeg 已验证可用：160 帧 → `artifacts/rust/stage0/body.mp4`（30fps，2.7s @60fps）
  - ✅ **帧数可调**：`YEJIAN_MOTION_FRAMES`（默认 100，录制用 240 → 30fps 播放约 8 秒）
  - ✅ **纸张几何断言已补**：单测 `paper_curve_peak_stays_in_band`（峰值 12–30，默认实测 26.21）+ `paper_curve_is_not_rigid`（剖面跨度 >1.5，实测 4.52）；acceptance 启动闸门同带（`paper lift peak ... outside 12..=30`）
  - ✅ **kill test PASSED**：坏参数（纸全平）→ `Error: paper lift peak 0.00 outside 12..=30`，EXIT=1；正常参数 EXIT=0。日志 `.tools/loop-baseline/stage0-killtest5.log`
  - [done] **阶段 0 收口** → 可进阶段 1
- [~] **阶段 1 · 抽出**：新增 `shelf` 参数段（`pull_base / pull_linear / pull_quad / rail_linear / rail_quad / lift_px / roll_rad / arc_base / arc_pct / arc_max / columns / shade_gain`），已接进 `shelf.rs` 的抽出与飞行绘制。
  - 三个变体：**A 基线**（pull 0.84 / lift 18 / roll 0.045 / arc 12）｜**B 更拔**（1.00 / 22 / 0.06 / 16）｜**C 更克制**（0.70 / 14 / 0.03 / 9）
  - ⚠️ 第一版录制**看不见飞行段**：`h.frame()` 每帧固定推进 1/60s，而飞行只 0.72s ⇒ 只有 ~43 帧，剩下全是静止的详情页。已加 `Harness.dt`，录制时开放段按 1/240 推进（4 倍慢动作）。
  - 裁决素材：`artifacts/rust/record/r1/compare.mp4`（三宫格 8s）与 `contact.png`（3 行 × 4 关键时刻拼图，`tools/contact-sheet.ps1` 生成）
  - ✅ **裁决：A**（2026-11-23，用户在 `preview/` 里用 A/B/C 三个启动器亲手试过后选定）→ 阶段 1 完成
  - 用户同时给出产品决定：**下线「藏书一览」网格，立体书架即主体**（已实现：`ui.rs` 不再绘制网格，网格高度并入 stage；`shelf_focus_book_id()` 补位验收链路）
- [ ] 阶段 2 · 摊开　- [ ] 阶段 3 · 翻页　- [ ] 阶段 4 · 合上　- [ ] 阶段 5 · 常量对齐 + 边缘

每段完成 = 可机查证据 + 用户裁决编号，两者缺一不算。

## 版面决策（2026-11-23，用户睡前授权自主决定；已咨询 Gemini Neon）

> ⚠️ 2026-11-23 02:18 用户反馈：「都没有适配 iPad 界面，太丑了」。已做：宽屏全出血（去圆角卡片、去两侧边距与 1200pt 上限）、头部压缩到 132pt、舞台贴住 shelf_viewport 消除滚动溢出、pager 下移避免与「全部分类」重叠。**仍未解决**：扇形偏右、左右留白不均、左侧导航栏仍是桌面风格、展签位置偏左。审美判断需要用户的眼睛。

- **书架是唯一主角**：藏书一览网格下线 → 右侧信息栏下线 → 舞台占满整屏。
- 信息改为**底部居中「展签」**（书名 34pt / 分类 / 进度 / 继续阅读、详情两个按钮），无卡片边框（Gemini 推荐方案 1）。
- 书按**高度**放大：`shelf_scale` 上界 1.55→2.6，`cover` 上界 128→190pt；1024×1366 下焦点封面约占视口高 45%（Gemini 目标 46–52%）。
- 舞台高度 = `shelf_viewport.height() - top - 16`（短视口 clamp 220），展签高度 = 舞台高 30%（clamp 110–200），按钮锚在展签内，避免被挤出屏幕。

### Gemini 建议里尚未实现的（下一步）

1. 边缘书：±1 槽 0.90x、±2 槽 0.82x 缩放 + 每退一槽 12–15% 环境暗化 + 距屏幕边缘 0–48pt smoothstep 渐隐（现在只有硬边裁切）
2. 重量感：书底 2–4pt 极深接触阴影 + 24–36pt 柔和环境投影 + 一条搁板高光/暗线
3. 反透视错觉：放大后固定斜轴测显"远端更粗"，建议深度压缩到真实厚度 0.6–0.7 倍
4. 高清封面：封面纹理需 mipmap + 各向异性过滤；0.4px 闭合描边改受光棱线高光

## 参考规格（对照 `.tools/wenku-v0.5.6/`；细则原文见 git `9d25566:REFERENCE.md`）

- 书架：`width≤520`；`depthX=0.66 / depthY=−0.30`；`spine=clamp(w·0.043,14,18)`；`pitch=spine+11`；`cover=clamp(w·0.32,100,128)`；`H=(cover−20)·166/118+54`；`railY=d·2.5+d²·0.45`；`pull=cover·0.84·w(1.24−0.24w)`
- 抽出波：`weight=cos²(d/1.48·π/2)`，焦点 ±1.48 槽
- 飞行：49×17 网格；`scale=300/(300−depth)`；`depthAt=peel·floating·(17+7sinπv+3·breeze·sinπp)`；`lift≤18px`；`roll≤0.045rad`
- 揭示：`Interval(.08–.72)×easeOutCubic` + `travel(−8,−6)`
- 交换面：`Cubic(.18,.72,.18,1)`；incoming `(−24·dir,+12)` + `slant=min(180,.46w)`
- 手感：`v≥50px/s` 进摩擦段（dragRate 4.4）；落位 Hermite `target−Δ·r³`；慢速临界阻尼 `k=144 c=24`；飞行 1000/840ms
- **渲染路线（已采纳外部评审）**：顶点位移网格 + Vertex Shader 里算 `depthAt` 与解析法线 → Fragment 做光照与软阴影。**不用**片元反求 UV（非线性形变无闭式解，边缘必锯齿）
- 书脊高级感三要素：压痕沟槽 + 棱线高光（1–2px 暗槽配 0.5px 亮脊）／页口肌理 + 接触 AO ／克制的物理不规则（厚度按页数、参差 1–3px）

## 五段现状（2026-11-22 实测）

书脊 ✅ 完整；抽出 / 摊开 / 翻页 / 合上 ⚠️ 半成品（详见 git `9d25566:NEXT.md`）。

## 环境与通道

| 用途 | 怎么用 |
|---|---|
| 本地离屏 (Windows) | `cd rust-reader` → `$env:YEJIAN_IPAD_ONLY='1'; cargo run --locked -j 1 --features acceptance --bin acceptance -- ..\\artifacts\\rust\\<目录>`，8 档 iPad 矩阵约 45 秒（增量） |
| 云 macOS | `devin ssh 90712f860a83471a97ad0584fbeac1e5 '<cmd>'` —— **不消耗模型额度**；有 Xcode 26.6 + 6 台 iPad 模拟器；磁盘仅 17GB 空闲 |
| 代码同步 | 推 `work/*` 分支到 `XZX317568/yejian-ipad`（PUBLIC），云端 `git clone` |
| 外部顾问 | Gemini Neon（驱动桌面端，截图读回）／SWE-2 子代理（免费，262K 窗口） |
| 每轮收尾 | `tools/loop-check.ps1`（环境闸门 + 锁 + git + 测试 + STATE 证据核对；`-Full` 加跑离屏验收）。**每轮必须 LOOP-CHECK PASS 才能报完成** |
| 裁决录制 | `tools/record.ps1 -Variants <a,b,c> -Out <dir>` → 每变体 mp4 + `compare.mp4`（iPad 1024×1366、8s、30fps） |

## 裁决记录（用户只回编号，永不重复问）

| 轮 | 变体 A / B / C | 选中 | 时间 |
|---|---|---|---|
| 阶段 1 · 抽出 | A 基线 / B 更拔 / C 更克制 | **A**（用户亲眼试玩后选） | 2026-11-23 01:10 |

## 阻塞 / 待核实

- `web_search` 401（key 无效）→ 改用浏览器检索
- 云 macOS 磁盘 17GB，长构建前要清
- 子代理仅 `devin/swe-2` 可用（`deepseek-flash` 在配置里但被会话拒绝）

## 已验证事实（一行一条，附证据路径）

- 2026-11-22 参数外置生效且无需重编译：`paper.base` 17→0 后 frame-045/060/075 哈希全变，日志 `.tools/loop-baseline/stage0-params-live.log`
- 2026-11-22 ffmpeg 管线可用：`artifacts/rust/stage0/body.mp4`（160 帧 / 121KB）
- 2026-11-22 kill test 先 FAILED（纸全平仍 PASS），补断言后 **PASSED**：broken EXIT=1 / good EXIT=0，日志 `.tools/loop-baseline/stage0-killtest5.log`
- 2026-11-23 景深压暗已生效：每退一槽 13%（`shelf.rs` paint_volume），两侧书不再和焦点书抢视线
- 2026-11-23 **负结果**：用 ellipse 另画"接触阴影"在这套斜轴测几何上会变成一排圆色块（像石头），已回退；正确做法是并进地面梯度多边形
- 2026-11-23 全屏书架落地：`ui.rs` 舞台吃满视口、底部展签；`shelf.rs` scale 2.6 / cover 190。iPad 8 档矩阵 PASS（`artifacts/rust/fullshelf2/`），全量 acceptance PASS（`artifacts/rust/fullshelf-full2/`）
- 2026-11-23 录制必须慢动作：`dt=1/240` 时飞行段才铺满 168 帧；固定 1/60 时整段只剩 1–2 帧飞行
- 2026-11-22 全 workspace 测试 EXIT=0（含新增 2 条纸张曲线断言），日志 `.tools/loop-baseline/stage0-tests2.log`

- 2026-11-22 本地 `cargo test -j1`：**220 passed / 0 failed / 3 ignored**，10.8 分钟，日志 `.tools/loop-baseline/cargo-test-20261122-retry3.log`
- 2026-11-22 离屏 iPad 矩阵 8 档 **全 PASS**，构建 45.61s，产物 `artifacts/rust/ipad-polish-round1/`
- 2026-11-22 云 macOS 实测：Darwin arm64 / macOS 26.6.2 / Xcode 26.6 / 12 核 16GB，克隆 `work/ipad-polish-sync` 成功、`cargo metadata` OK
