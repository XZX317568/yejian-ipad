# STATE · 页间当前状态

## 目标（锁死，改它要用户点头）

在 iPad 上做出「像一本真书」的阅读器：书架书脊 → 抽出 → 摊开 → 翻页 → 合上。Rust 自绘（wgpu/egui）。安卓暂缓，源只做 Aidoku，`app/` 冻结。

## 阶段计划（外部评审后修订：先基建，再动形变）

- [ ] **阶段 0 · 调参基建**（不做完不开工）
  - 动画常量 + WGSL 外置到 TOML/JSON，离屏渲染器直读 → 调参周期从 11 分钟压到 ~5 秒
  - 端到端录制脚本（全流程，未实现段线性插值占位）
  - **kill test**：故意把 `pull`×2、`slant`=0，验收必须至少一处 FAIL；否则先修验收
- [ ] 阶段 1 · 抽出　- [ ] 阶段 2 · 摊开　- [ ] 阶段 3 · 翻页　- [ ] 阶段 4 · 合上　- [ ] 阶段 5 · 常量对齐 + 边缘

每段完成 = 可机查证据 + 用户裁决编号，两者缺一不算。

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

## 裁决记录（用户只回编号，永不重复问）

| 轮 | 变体 A / B / C | 选中 | 时间 |
|---|---|---|---|
| — | — | — | — |

## 阻塞 / 待核实

- `web_search` 401（key 无效）→ 改用浏览器检索
- 云 macOS 磁盘 17GB，长构建前要清
- 子代理仅 `devin/swe-2` 可用（`deepseek-flash` 在配置里但被会话拒绝）

## 已验证事实（一行一条，附证据路径）

- 2026-11-22 本地 `cargo test -j1`：**220 passed / 0 failed / 3 ignored**，10.8 分钟，日志 `.tools/loop-baseline/cargo-test-20261122-retry3.log`
- 2026-11-22 离屏 iPad 矩阵 8 档 **全 PASS**，构建 45.61s，产物 `artifacts/rust/ipad-polish-round1/`
- 2026-11-22 云 macOS 实测：Darwin arm64 / macOS 26.6.2 / Xcode 26.6 / 12 核 16GB，克隆 `work/ipad-polish-sync` 成功、`cargo metadata` OK
