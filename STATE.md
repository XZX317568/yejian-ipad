# STATE · 页间当前状态

## 目标（锁死，改它要用户点头）

在 iPad 上做出「像一本真书」的阅读器：书架书脊 → 抽出 → 摊开 → 翻页 → 合上。Rust 自绘（wgpu/egui）。安卓暂缓，源只做 Aidoku，`app/` 冻结。

## 阶段计划（外部评审后修订：先基建，再动形变）

> **当前阶段（2026-11-24 起）**：UI 重构 S0–S6（Apple Books 暖白原生风 × 实体书脊融合，深耕 Rust egui/wgpu），详见 `工程清单.md`；下方阶段 0–5 为已完成历史。

- ✅ **阶段 0 · 调参基建**
  - ✅ **参数外置**：`rust-reader/src/params.rs` + `rust-reader/params.json`（`YEJIAN_PARAMS` 可覆盖路径，含 `paper` / `snap` / `weight` / `ease` 四组）。**证据**：同一份已编译产物，改 `paper.base` 17→0 后 `motion/frame-045/060/075.png` 三帧 SHA256 全部不同，且无需重新编译。日志 `.tools/loop-baseline/stage0-params-live.log`
  - ✅ **录制管线**：`tools/record.ps1`（多变体 → 各自 mp4 → `hstack` 并排 `compare.mp4`）。ffmpeg 已验证可用：160 帧 → `artifacts/rust/stage0/body.mp4`（30fps，2.7s @60fps）
  - ✅ **帧数可调**：`YEJIAN_MOTION_FRAMES`（默认 100，录制用 240 → 30fps 播放约 8 秒）
  - ✅ **纸张几何断言已补**：单测 `paper_curve_peak_stays_in_band`（峰值 12–30，默认实测 26.21）+ `paper_curve_is_not_rigid`（剖面跨度 >1.5，实测 4.52）；acceptance 启动闸门同带（`paper lift peak ... outside 12..=30`）
  - ✅ **kill test PASSED**：坏参数（纸全平）→ `Error: paper lift peak 0.00 outside 12..=30`，EXIT=1；正常参数 EXIT=0。日志 `.tools/loop-baseline/stage0-killtest5.log`
  - [done] **阶段 0 收口** → 可进阶段 1
- ✅ **阶段 1 · 抽出**：新增 `shelf` 参数段（`pull_base / pull_linear / pull_quad / rail_linear / rail_quad / lift_px / roll_rad / arc_base / arc_pct / arc_max / columns / shade_gain`），已接进 `shelf.rs` 的抽出与飞行绘制。
  - 三个变体：**A 基线**（pull 0.84 / lift 18 / roll 0.045 / arc 12）｜**B 更拔**（1.00 / 22 / 0.06 / 16）｜**C 更克制**（0.70 / 14 / 0.03 / 9）
  - ⚠️ 第一版录制**看不见飞行段**：`h.frame()` 每帧固定推进 1/60s，而飞行只 0.72s ⇒ 只有 ~43 帧，剩下全是静止的详情页。已加 `Harness.dt`，录制时开放段按 1/240 推进（4 倍慢动作）。
  - 裁决素材（2026-11-23 三次重录=P1-6立体化后，310 帧全链）：r2/r3/r4 各有 compare.mp4 与 {A,B,C}/variant-*.mp4；摊开段书保持立体 morph（非平面卡片）
- **2026-11-23 裁决结果：r2=r3=r4=B（纸感全套）**——常量已落进 params.rs：spread slant_pct .46→.30 / start .45→.35 / shift_x 24→40；flip turn_ms 180→240 / commit_vx 500→350 / depth 22→34 / band .35→.50 / edge_shade .10→.16；close exit_ms 320→420 / under_alpha .5→.3 / travel 580→680。完整 acceptance EXIT=0 不回归
  - 裁决素材：`artifacts/rust/record/r1/compare.mp4`（三宫格 8s）与 `contact.png`（3 行 × 4 关键时刻拼图，`tools/contact-sheet.ps1` 生成）
  - ✅ **裁决：A**（2026-11-23，用户在 `preview/` 里用 A/B/C 三个启动器亲手试过后选定）→ 阶段 1 完成
- ✅ **阶段 2 · 摊开**：核心链路已入库（`0f43700`，斜扫 clip 揭示 + spread 参数组）；三变体已录 `artifacts/rust/record/r2/compare.mp4`；preview 已加 试-r2-A/B/C 启动器 → **2026-11-23 用户裁决：B（翻页感纸面）**
- ✅ **阶段 4 · 合上**：实现已入库（reader_exit 镜像 entry，Option F）；三变体已录 `artifacts/rust/record/r4/compare.mp4` → **2026-11-23 用户裁决：B（书感）**
- ✅ **阶段 3 · 翻页**：核心链路已入库（`64381e0`，页图 mesh 化 + 列向 depth 隆起 + 详情→阅读器 entry）；三变体已录 `artifacts/rust/record/r3/compare.mp4` + 中点帧 `record/r3-flip/{A,B,C}/flip-*.png`；preview 已加 试-r3-A/B/C 启动器 → **2026-11-23 用户裁决：B（纸感）**
  - 用户同时给出产品决定：**下线「藏书一览」网格，立体书架即主体**（已实现：`ui.rs` 不再绘制网格，网格高度并入 stage；`shelf_focus_book_id()` 补位验收链路）
- ✅ 阶段 2 · 摊开（B）　- ✅ 阶段 3 · 翻页（B）　- ✅ 阶段 4 · 合上（B）　- ✅ 阶段 5 · 常量对齐（B 参数已入库）

每段完成 = 可机查证据 + 用户裁决编号，两者缺一不算。

## 版面决策（2026-11-23，用户睡前授权自主决定；已咨询 Gemini Neon）

> ⚠️ 2026-11-23 02:18 用户反馈：「都没有适配 iPad 界面，太丑了」。
>
> **量具踩坑（勿重犯）**：`tools/analyze-layout.ps1` 按"高饱和像素"找包围盒时，会把底部绿色的「开始阅读」按钮算成书，
> 于是得出"书压在屏幕底部（B=14）"的错误结论。**要先排除展签区域再量**，否则会去修一个不存在的问题。已做：宽屏全出血（去圆角卡片、去两侧边距与 1200pt 上限）、头部压缩到 132pt、舞台贴住 shelf_viewport 消除滚动溢出、pager 下移避免与「全部分类」重叠。**仍未解决**：扇形偏右、左右留白不均、左侧导航栏仍是桌面风格、展签位置偏左。审美判断需要用户的眼睛。

- **书架是唯一主角**：藏书一览网格下线 → 右侧信息栏下线 → 舞台占满整屏。
- 信息改为**底部居中「展签」**（书名 34pt / 分类 / 进度 / 继续阅读、详情两个按钮），无卡片边框（Gemini 推荐方案 1）。
- 书按**高度**放大：`shelf_scale` 上界 1.55→2.6，`cover` 上界 128→190pt；1024×1366 下焦点封面约占视口高 45%（Gemini 目标 46–52%）。
- 舞台高度 = `shelf_viewport.height() - top - 16`（短视口 clamp 220），展签高度 = 舞台高 30%（clamp 110–200），按钮锚在展签内，避免被挤出屏幕。

### iPad 版面体检结论（详见 `.tools/ui-audit-20261123.md`）

> ✅ 已修完并推送：展签居中+横屏让位（2ca30f0）、竖屏底部标签栏+width>height 侧栏判定+横屏窄栏（f546d2e）。用户反馈的"没适配 iPad"核心根因是 `side_navigation()` 的 `width>=1000` 把 1024 竖屏误判成横屏桌面布局。
> ⚠️ 目标曾因 6 轮上限 blocked → 放宽 24 轮 → **用户要求 10000 轮**（2026-11-23 03:01）。
> ✅ 2026-11-23 03:05 构建与验收迁往云 macOS 盒（12 核 arm64）：`acceptance` 全量 1m03s 编译、iPad 8 档矩阵 PASS（Metal 离屏，工件在盒子 `~/repos/yejian/artifacts/rust/mac-check1`）。iOS 模拟器链路齐备：`tools/build-rust-ios.sh --sim` + xcodebuild + `tools/capture-ipad-simulator.sh`（CI 同款），6 台 iPad 模拟器可用；真机 IPA 走 `--device --ipa` → LiveContainer。
> ⚠️ zsh 注意：`===`/未引号变量会被展开；`PID=\$!` 不会展开（用 pgrep 确认）。

两个独立 agent（ui-critic / nav-spec）给出的顺序：①展签内部左偏 ②竖屏该改底部标签栏（1024 竖屏仍走桌面侧栏，且 `side_navigation()` 用 width≥1000 判断会误判）③横屏 books rect 未扣展签带导致压字 ④边缘书柔边渐隐。本轮已修：书群垂直居中、舞台再让 104pt、pitch 下限、rail 对称下沉。

### Gemini 建议里尚未实现的（下一步）

1. 边缘书 ✅（767cae8）：48pt smoothstep 渐隐 + 半宽钳制 + 焦点豁免（Volume.fade 已贯通）
2. 重量感 ✅（51f8a68）：三层阴影=接触3.5pt内缘0.34+环境26-34pt+搁板线1.2px
3. 反透视 ✅（de2a1f6）：depth_squeeze 分段线性 0→1/±2→0.70/≥±3→0.65
4. 高清封面 ✅（d657882 已有）：CPU 预缩放 3 档 box-filter 已有

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
| 云 macOS | `devin ssh 90712f860a83471a97ad0584fbeac1e5 '<cmd>'` —— **不消耗模型额度**；有 Xcode 26.6 + 6 台 iPad 模拟器。**会话已于 2026-11-24 ended**（r5 真机 check 后 SSH `Session has ended`，raw log 未取回）；不再把「17GB 可用」当现状 |
| 代码同步 | 推 `work/*` 分支到 `XZX317568/yejian-ipad`（PUBLIC），云端 `git clone` |
| 外部顾问 | Gemini Neon（驱动桌面端，截图读回）／SWE-2 子代理（免费，262K 窗口） |
| 每轮收尾 | `tools/loop-check.ps1`（环境闸门 + 锁 + git + 测试 + STATE 证据核对；`-Full` 加跑离屏验收）。**每轮必须 LOOP-CHECK PASS 才能报完成** |
| 裁决录制 | `tools/record.ps1 -Variants <a,b,c> -Out <dir>` → 每变体 mp4 + `compare.mp4`（iPad 1024×1366、8s、30fps） |

## 裁决记录（用户只回编号，永不重复问）

| 轮 | 变体 A / B / C | 选中 | 时间 |
|---|---|---|---|
| 阶段 1 · 抽出 | A 基线 / B 更拔 / C 更克制 | **A**（用户亲眼试玩后选） | 2026-11-23 01:10 |
| 阶段 2 · 摊开 | A / B / C 三变体（compare.mp4 + variant-*.mp4） | **B**（翻页感纸面） | 2026-11-23 |
| 阶段 3 · 翻页 | A / B / C 三变体（compare.mp4 + variant-*.mp4） | **B**（纸感） | 2026-11-23 |
| 阶段 4 · 合上 | A / B / C 三变体（compare.mp4 + variant-*.mp4） | **B**（书感） | 2026-11-23 |
| S6 · 暖白强调色 | A 琥珀(170,94,24) / B 书脊棕(168,93,40) / C 墨黑(44,38,33)（.tools/verify-ui-v2-final/rec/compare.mp4） | **A**（琥珀，浅色 #AA5E18；与 `design.rs:36` 默认分支及录像 frame-000 CTA 像素一致） | 2026-11-25 12:14:13 +08:00（Asia/Shanghai，**系统记录收到裁决的时间**，非客户端发送时间/毫秒） |

## S6 终验快照（2026-11-25，候选 49e4ca4，verify-s6/-r2 独立验收）

- 单测 268/0/3 EXIT=0 + 六路（full/design/daily/filters/ipad/seam-2×）EXIT=0；回执 .tools/verify-ui-v2-final/logs/（cmd+exit+log+JSON），二进制 SHA256 CA42474504EE8C1C302B6815BA684960DAD00C46F867673CBBB4E6C392932C4E
- 2× 专项 rendered_ppp=2.0、三帧 join leak=[0,0]；A/B/C 各 310 帧@1024×1366 端到端（书架→摊开→翻页→合书），compare.mp4 3072×1366/10.33s；变体像素逐字节核对属实 —— 全部 Windows 离屏 wgpu（RTX 5060），非真机
- 十二屏审阅页 preview/ui-v2-review/（index.html + data.js 12 场景 + compare.mp4）；入口 `file:///D:/Documents/aidoku/preview/ui-v2-review/index.html`（双击即开，无服务器）。task-44 **当时**实际打开复验：12 屏/键盘/播放/暂停/seek PASS，但 ①meta 渲染 src 致整页横溢（scrollWidth≈2.7Mpx）②「打开原图」about:blank —— 两项发布缺陷当时未签（**历史快照**，e10c58a 6/7 即基于此）
- task-46 修复两项缺陷（index.html 7,025 B，SHA256 5449656D…B119），task-47 独立复验 **PASS**：526 与 1440 两档 12/12 屏 documentElement.scrollWidth == clientWidth（请求 525 实测 526，+1px）；「打开原图」真实点击 820×1180 与 O 键 1366×1024 均 1:1 非空白、关闭回原场景；键盘/切图/页内播放 t=1.231、暂停 t=2.035、seek 8s 全过；证据 .tools/verify-ui-v2-offline/width/{summary.json,525-sweep.json,1440-sweep.json,525-interactions.json,width-*.png}、worklog-review-width.md
- 汇总：reports/2026-11-25-ui-apple-books.md（技术交付 **7/7**；用户已于 2026-11-25 12:14:13 +08:00 选定强调色 **A 琥珀 #AA5E18**；e10c58a 的「部分达成 6/7」为历史快照；iPad 真机未测、弯曲翻页相位逐像素证伪未做仍为未验证边界，选色不等于真机验收）；遗留 13 条见 .scratch/ui-apple-books/leftovers.md

## 阻塞 / 待核实

- `web_search` 401（key 无效）→ 改用浏览器检索
- 云 macOS 会话已 ended（2026-11-24）；旧「磁盘 17GB，长构建前要清」不再是当前环境事实，需要时重开会话再核
- 子代理仅 `devin/swe-2` 可用（`deepseek-flash` 在配置里但被会话拒绝）

## 已验证事实（一行一条，附证据路径）

- 2026-11-24 **真实界面评审页（侧栏）**：`preview/design-20261123/review-app.html`（12 张 Air 5 真机/验收截图：书架/详情竖横 + 阅读/历史/书源/设置/筛选/书源浏览/社区/分屏，图片内嵌，**不是 A/B/C 设计稿**）；点「标注」→点画面落点→写改法→自动写 `app-review-notes.json`，服务端 `/api/app-notes`；ego 机检 `.tools/design-20261124/app-review-v2-check.json` **12/12 true**（525 紧凑/1440 两栏/保存/重置/切屏；http 与 file:// 双上下文都能 POST 落盘），截图 `.tools/design-20261124/review-app-v2-525.png`；交互原型热点（书架→详情→阅读→翻页→底栏）机检 `.tools/design-20261124/app-interactive-check.json` **12/12 true**，交互截图 `.tools/design-20261124/review-app-interact-mode-525.png`；原生窗口无法嵌进侧栏（实测失焦即最小化、会抢鼠标），侧栏用交互原型；**visualApproved=false，待用户确认要看哪些真实界面**。
- 2026-11-24 **侧栏评审页升级为真交互（v2，按宽度布局 + 三档缩放）**：`preview/design-20261123/review-app.html` 3,019,242 B，SHA256 `3C405CCDA5F900FCFF25D84A2859A39E53F5B231CF08278AAC4F9C8C3F1550AC`；由 `review-app.tpl.html`（9,229 B，SHA256 `50331670EBE9A4E20B1097D0CB37A1FF04DD68AAF315859ED9100D62184DB12D`）+ 生成脚本产出，**两次重建逐字节一致**（副本沙箱重建，证据 `.tools/design-20261124/verify2-reproducibility.json`）；交互逻辑独立成 `review-app-interact.js`（30,292 B，SHA256 `600873EB67CC3FC17D75514E9D79AF1E74454FDF25C7DF14B785FA41BCE9A65E`）。能力：单屏按容器宽度渲染（525 视口实测 481×692，比例 1.4387），三档缩放 适应宽度 / 100%(820px) / 200%(1640px)，任何档位 `document.scrollWidth == 视口宽`，200% 在容器内横竖滚；坐标换算回 820×1180 逻辑坐标（200% 平移到右下角点按 → 99.9%/99.9%；未平移点可视内容右边缘 → 29.8%）；纵向滚动 / 阅读页横向拖拽翻页 / 长按标注三者互不误触发，**CDP 触摸三项补测通过**；工具条删除歧义开关，改为常显提示。**独立证伪第二轮 PASS**（判据 10 条全 PASS；实现者自测只作参考、不作验收结论）：证据 `.tools/design-20261124/verify2-recon-525.json`、`verify2-525-interactions.json`、`verify2-1440.json`、`verify2-reproducibility.json`、`verify2-525-fit.png`、`verify2-525-100.png`、`verify2-525-200.png`、`verify2-525-interactions.png`、`verify2-1440-fit.png`。**入口**：`preview/评审-侧边栏.cmd` 已升 `?v=5`，**旧侧栏标签页必须刷新一次**；说明见 `preview/说明.md`。**未通过/未做**：`visualApproved=false`；用户尚未试新版（`app-review-notes.json` 143 B / SHA256 `3E160E19961B755A8E39A84F5DEAEE128786F6F7F05A4FA77B8F2348ECA844F7`，`notes: []`，无 `last` 脏字段，证伪者核过开始/结束一致，快照 `.tools/design-20261124/verify2-notes-before.json`）；fit 档 13px 设计字实测约 7.6px（实现者自报 9.4px，口径矛盾未核实）；工具条覆盖画面顶部 44px（fit 高 692 的 6.4%），是否接受待产品裁决；200% 下桌面鼠标不能拖拽平移（触摸/滚动条可用）；阅读页 3 页为同一张截图重复 + 页码 HUD（本轮未复测，沿用上一轮证伪结论）；刷新后回书架，只恢复标注不恢复上次屏（本轮未复测，沿用上一轮结论）；真实 iPad/Safari 触屏未测。
- 2026-11-24 **设计评审页 v1（静态工具，visualApproved=false）**：`preview/design-20261123/review.html`（340px 侧栏 + 820×1180 画布；标注模式点画面落点，app.js 经 postMessage 回传命中元素；localStorage + 导出/导入/复制 JSON）；ego-browser 机检 `.tools/design-20261124/review-check.json` **24/24 true**（1440×900 与 1024×768；草稿 50/49.9；命中 `.intro`；切屏隐藏/清单保留/回跳/删除/回归 A/B/C 全过；截图 `.tools/design-20261124/review-page.png`，仅布局自检不作视觉结论）；**待用户试用记 1 条**；侧栏模式（2026-11-24 追加）：`serve.mjs` 本地服务 + `评审-侧边栏.cmd`，右侧栏「浏览器面板」打开 `http://127.0.0.1:8765/review.html`，标注后自动写 `review-notes.json`（无需导出），侧栏 525×1125 机检 `.tools/design-20261124/sidebar-check.json` **11/11 true**，`05-review-check.json` 回归 **24/24 true**；独立证伪 `.tools/design-20261124/verifier-report.json` **84/84 true**（05 重跑 24/24、07 54/54、08 6/6，0 失败）；P3 已知：已有 pin 的 44px 框会截走附近点击、坏 localStorage 静默忽略、Ctrl+Enter 无落点无反馈；本轮未动 Rust、未跑完整 acceptance。
- 2026-11-24 **QuickJS 运行器 + Air 5 完整验收（4.2 / 3.0）**：workspace `cargo test --locked --workspace -j 1 -- --test-threads=1` → **266 tests + 1 doctest = 267 passed / 0 failed / 3 ignored**（`.tools/quickjs-20261124/workspace-air5-final-r2.log`；根全链 job `pwsh-91b0bc2c2f3b4c9d` exit 0，日志本身无 exit 行）。默认**完整** acceptance（所有 YEJIAN_* 已清）EXIT0 → `artifacts/rust/quickjs-air5-20261124-full`，日志 `acceptance-air5-full-r2.log`（含 open-book→detail 竖/横断言、PASS 行、`acceptance-result.txt: PASS`；4 张 Air 5 截图实测 820×1180 / 1180×820）。`YEJIAN_IPAD_ONLY` 补充 10 profiles EXIT0（`acceptance-air5-ipad-supplement-r2.log`，输出 `...-ipad`）**不能当 full 替代**。机器可读：`.tools/quickjs-20261124/{acceptance,verification}.json`（手工 summary，非 bin 自动 JSON）。
- 2026-11-24 **拷贝漫画实时（QuickJS 后，源 v24）**：更新時間 50 / 熱門 50 / 全新上架 60 均 has_next=true，筛选 50；搜索「海贼王」12 → 详情 → 403 章 → 13 页 → 首图 HTTP 200 549062B（`copymanga-air5-final-r2.log`，JSON 原样在 `verification.json`）。title 仍全 null，但 `chapter_label_unavailable_count=0`：显示名走 title→有限话号→有限卷号→key 兜底；纯卷 105 个且卷号 distinct 1..105（**旧审查「同名卷」推断错**）。新下载与旧 Failed/Queued 从详情重新加入 + fresh snapshot 统一显示、保留进度；已 Done + CBZ 原名不改、未迁移用户数据。
- 2026-11-24 **iOS 目标检查（不是 App/真机测试）**：模拟器 `cargo check` 取 `check-ios-r2.log:76` 第一条 **SIM_EXIT=0**（同日志第二条 device 段不用）。真机目标证据 = 工具 job `pwsh-0bc804fd9d234034`：`cargo check --locked -p yejian-sources -j1 --target aarch64-apple-ios`，SDKROOT=iPhoneOS26.5.sdk，**DEVICE_CHECK_EXIT=0**（job completed / exit 0，耗时 25.2s）。raw log `/Users/devin/captures/quickjs-20261124/check-device-real-r5.log` **未取回**（随后 SSH `Session has ended`），故 `.tools/quickjs-20261124/ios-device-check-receipt.json` 是工具输出转录、不含编译器日志正文；范围仅 yejian-sources crate check，**无新 IPA / App 链接 / 部署 / 真机运行**。
- 2026-11-24 **设计比较页（2.2）**：`preview/design-20261123/compare.html` 已建三列微缩（A/B/C 各 820×1180）+ 四屏切换 + 每列全尺寸入口；`compare-review.json` **21/21 checks true**，`dom-review.json` 12 屏 DOM **10/10 true** 且切换交互 pass；**visualApproved=false**、截图工具两次失败后已停止，Windows 字体≠iPad，A/C 书脊 3D 投影命中最小 40.7px 有风险 → 不得声称触点全 ≥44 或视觉通过；A/B/C 待用户裁决。
- 2026-11-23 **原生输入收口（4.3）+设计初稿验证轮（2.2）**：本地 `cargo test --locked --workspace -j1 -- --test-threads=1` EXIT0，248 passed / 0 failed / 3 ignored + doctest 1 passed（`.tools/input-20261123/final-tests-recovered.log`）；完整 acceptance `cargo run --locked -j1 --features acceptance --bin acceptance -- ../artifacts/rust/input-20261123-final` EXIT0（`.tools/input-20261123/final-acceptance-recovered.log`，产物 `artifacts/rust/input-20261123-final/`）；Mac Rust(aarch64-apple-ios-sim) 与 Xcode build-for-testing 成功（`rust-remote.log` / `xcode-remote.log`）；模拟器输入实测 iPad Air 11″ M4 (820×1180) 持久目录 `/Users/devin/captures/input-20261123-2312`：**2/2 passed 0 skip**（`.tools/input-20261123/final/{summary.json,input-tests.log,pinyin-candidate.png,verification.json}`）。test06 真实逐键拼音 `h`→`a`→`i`→候选条出现「海」→键盘 Search 提交→重开弹窗回填；test05 覆盖 取消×2、Unicode「海贼王」直输、弹窗内 Search 提交。**未覆盖**：真实书源网络端到端与真机；修的是书源搜索原生 UITextField（fixture），非全局 IME。设计预览浏览器侧已确认 C 筛选仅海贼王、翻页更新文本与页码、A/B/C 切换生效，但截图接口超时、三方向未做最终视觉通过
- 2026-11-23 用户重申：禁止 AI 自设 goal / 自封完成，只记录事实与证据
- 2026-11-23 **Air 5 尺寸模拟器体检（工程清单 1.3）**：云 Mac iPad Air 11-inch (M4) 模拟器 DDB1A224（820×1180，同 Air 5），代码 7ae8705；竖 10 + 横 4 画面就绪、IPadRotationTests 4/4、verify-ipad-snapshots PASS（capture 脚本在 XCTest 后被手动中断，relocated-library.json 按脚本同逻辑手工补生成后再核验）。截图 `artifacts/体检-20261123-sim/`。**拷贝漫画 v24（同日更正）**：WASM 导入 js::context_create/context_eval，引擎 host_js.rs 全为桩；但书源源码只在 HTML 列表/筛选页用 `JsContext.eval("JSON.stringify(...)")`。本机网络临时 e2e 探针（examples/tmp_copymanga_e2e.rs，用后已删）实测：搜「海贼王」12 条 → 详情 OK → 403 话 → 13 页、首图 HTTP 200 549062B；「全新上架」60 条；「更新時間」「熱門」报 web verification required。早先「搜索必失败」系 source_probe 只测空关键词+筛选导致的误判。章节 title 全为 None 待查。**中文输入推断**：winit 0.30.13 iOS WinitView 仅实现 UIKeyInput（insertText/deleteBackward），无 UITextInput marked text → 拼音选字大概率不可用，待真机核实
- 2026-11-23 **最新 IPA（工程清单 0.2）**：Actions run 45126414677（ipad.yml，ref work/ipad-polish-sync @7ae8705，代码同 6273962）成功 7m42s → `artifacts/rust/ipad-20261123-run45126414677/Yejian-iPad/Yejian-0.1.1-7-ipad-unsigned.ipa`（28,826,778 字节，SHA256 e536cc24…fcefc3）；Info.plist 实查 app.yejian.reader.rust / 0.1.1 / build 7——**与 11-20 的 build 7 同号**，新旧只能靠画面区分。云 Mac `devin ssh` 本次报未登录，改走 Actions
- 2026-11-22 参数外置生效且无需重编译：`paper.base` 17→0 后 frame-045/060/075 哈希全变，日志 `.tools/loop-baseline/stage0-params-live.log`
- 2026-11-22 ffmpeg 管线可用：`artifacts/rust/stage0/body.mp4`（160 帧 / 121KB）
- 2026-11-22 kill test 先 FAILED（纸全平仍 PASS），补断言后 **PASSED**：broken EXIT=1 / good EXIT=0，日志 `.tools/loop-baseline/stage0-killtest5.log`
- 2026-11-23 **iPad 模拟器真机链路打通**：macOS 盒跑通 CI 同款 build-rust-ios.sh --sim → xcodebuild → capture-ipad-simulator.sh → IPadRotationTests **4/4 PASS**；shelf.png 证实竖屏底部标签栏+全屏书架在真模拟器生效（截图拉回到 artifacts/rust/sim-pulled/，经 devin forward + python http.server）。verify-ipad-snapshots.py 崩在缺 relocated-library.json（覆盖率缺口，非 bug）
- 2026-11-23 **模拟器第三轮全链 PASS**（含 flip+verify 修复）：ipad-sim3 capture 完整跑完 CAPTURE_EXIT=0、legacy-library.json 就位、verify-ipad-snapshots.py PASS（上一轮 pipe2 的"缺 JSON 崩"已修复+根因是 capture 中断非 verify 错）。IPadRotationTests 4/4
- 2026-11-23 **深度打磨 R3**（b9687f2）：边缘渐隐带+三层接触阴影+反透视0.65基线全部落地；最终录影 `artifacts/rust/record/polished/variant-base.mp4`（310帧全链，B参数+全部质感项）
- 2026-11-23 **B 参数云端终验**（b-params2）：xcodebuild arm64 + 旋转测试 4/4 PASS + verify-ipad-snapshots PASS；iPad Pro 13" 真模拟器横屏书群展示成立
- 2026-11-23 **回归修复+证伪完成**（4908714）：P2-1 空态居中把清除搜索按钮压出视口→修；验收第5条第三方证伪确认 P0-1/P1-6/回归修复全部生效
- 2026-11-23 **P1-6 详情封面立体化**（4213bd2）：复用 Volume 几何+morph 跟 spread_progress；详情页书脊/页口/收分全回——评审 R2 **15 条全部清零**
- 2026-11-23 **P0-1 横屏书群**（7477920）：焦点书+28%、群宽×1.9、群心右移6%w；评审 R2 全部消化（P0×3 全修、P1×6 修5、P2×6 全修，剩 P1-6 详情封面立体化待裁决）
- 2026-11-23 **P2四项**（6fdbdf5）：空状态居中、token 框露底、舞台加深4%、遮挡剪影保完整；评审队列清零（P0-1/P1-1/P1-6 三个大改留裁决后）
- 2026-11-23 **P1一致性四项**（00d4e30）：分屏舞台改满幅直角带、辅助页统一 page_column 左缘、激活态统一胶囊、分页器统一锚舞台右上；剩 P1-1 展签对齐+P1-6 详情封面平面化两个大改待裁决
- 2026-11-23 **设计评审R2**（.tools/design-review-round2.md）：P0×3/P1×6/P2×6；P0-2线穿按钮+P0-3按钮出血已修（366443d，agent-p0fix 截图验证）；P0-1 横屏书群权重不足属布局改动待裁决后动
- 2026-11-23 **摊开真bug修复** 943b0cb：斜扫之前被 flight mesh 压在底下从未可见（draw() 扫描期隐藏 mesh）；文字列在未揭示纸面鬼影（detail_text_alpha 改前沿联动）；spec§7 像素断言落地（斜率 0.139 vs 理论 0.1366 ±15%）
- 2026-11-23 **边缘余项**：fd11ce0 斜扫带 8→自适应 23 段（撕口台阶细化见 agent-bands/close-mid-compare.png）+ 320pt 导航 pill 左右边距 ≥8pt
- 2026-11-23 **阶段5常量对齐完成**（.tools/const-align-audit.md）：80 键全核，62 一致 + 18 有意偏离 + **0 漂移**；spring_c/select 曲线/close_travel 三个疑似点全部证伪
- 2026-11-23 **封面 mipmap**（d657882）：egui sampler 无真 mip，改 CPU 预缩放 ≤3 档 box-filter + cover_pick 按足迹选档；焦点书 0 像素变化、远端封面边缘变柔；isotropic 档对斜向拉伸轴偏软是已知取舍
- 2026-11-23 **阶段4合上规格**（.tools/stage4-close-spec.md）：参考无专属合上实现，coverReturn=840ms 只是 Cupertino pop 倒放；detail→shelf flight.returning 已完整；唯一缺口 reader→detail 的 reader_exit 镜像（spread_clip 收缩 bands 复用）
- 2026-11-23 **阶段3翻页规格**（.tools/stage3-flip-spec.md）：参考里**无 page-curl**，转场是 Cupertino 平推；方案 = PageSlide 平移轴不动 + 页图 mesh 化加列向 depth 隆起（PaperFlightPose.depthAt 行向→列向）。flip 参数组待实现，R4 守卫 placeholder 平移
- 2026-11-23 verify-ipad-snapshots.py 修复 f898b8d：缺 legacy/relocated JSON 降级 WARNING 跳过（真凶是 capture 中断被 tee 吞 exit code）
- 2026-11-23 **模拟器第二轮全链 PASS**（含摊开）：ipad-sim2 出 8 画面（shelf/detail/detail-landscape/reader/history/browse/community/filters/filters-landscape），IPadRotationTests 4/4，TEST EXECUTE SUCCEEDED；detail 完成态布局全居中正确。截图路径 artifacts/rust/ipad-sim2（盒子）/ sim-pulled/detail-sim2.png（本地）
- 2026-11-23 **摊开语义纠正**（.tools/stage2-spread-spec.md）：参考 `slant=min(180,w·0.46)` 是**裁剪路径斜扫边**不是面片剪切；incoming/outgoing 在参考里是死代码，唯一活用法是底部 sheet。摊开 = 斜边扫入 clip 揭示，接在 flight 的 contact 段（t≥0.58）后
- 2026-11-23 书本尺寸实测：`cover = clamp(w/scale*0.32,100,190)*scale`，iPad 竖屏约 **328pt 宽 / 528pt 高**（不是以为的 460）；`base_y+height ≈ 974`，画布 1202，所以垂直并未被裁
- 2026-11-23 景深压暗已生效：每退一槽 13%（`shelf.rs` paint_volume），两侧书不再和焦点书抢视线
- 2026-11-23 **负结果**：用 ellipse 另画"接触阴影"在这套斜轴测几何上会变成一排圆色块（像石头），已回退；正确做法是并进地面梯度多边形
- 2026-11-23 全屏书架落地：`ui.rs` 舞台吃满视口、底部展签；`shelf.rs` scale 2.6 / cover 190。iPad 8 档矩阵 PASS（`artifacts/rust/fullshelf2/`），全量 acceptance PASS（`artifacts/rust/fullshelf-full2/`）
- 2026-11-23 录制必须慢动作：`dt=1/240` 时飞行段才铺满 168 帧；固定 1/60 时整段只剩 1–2 帧飞行
- 2026-11-22 全 workspace 测试 EXIT=0（含新增 2 条纸张曲线断言），日志 `.tools/loop-baseline/stage0-tests2.log`

- 2026-11-22 本地 `cargo test -j1`：**220 passed / 0 failed / 3 ignored**，10.8 分钟，日志 `.tools/loop-baseline/cargo-test-20261122-retry3.log`
- 2026-11-22 离屏 iPad 矩阵 8 档 **全 PASS**，构建 45.61s，产物 `artifacts/rust/ipad-polish-round1/`
- 2026-11-22 云 macOS 实测：Darwin arm64 / macOS 26.6.2 / Xcode 26.6 / 12 核 16GB，克隆 `work/ipad-polish-sync` 成功、`cargo metadata` OK