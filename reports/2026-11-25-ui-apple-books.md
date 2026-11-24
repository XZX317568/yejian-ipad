# 团队汇总 · ui-apple-books

## 结论

主目标：部分达成（6/7）

S0–S5 全段实现+独立验收通过、S6 终验六路退出码全 0、A/B/C 三变体 310 帧端到端录像与 12 屏离线审阅页已交付且实际打开复验通过；唯一缺口是用户视觉签收（A/B/C 编号+时间未回），且页面有 2 项发布缺陷未签。技术验收可机查、可复跑；全部画面为 Windows 离屏 wgpu 渲染，不是 iPad 真机。

## 谁干了什么

| 成员 | 交付物 | 状态 | 产物指针 |
|---|---|---|---|
| impl-s0 | S0 基线与参数同步、裁决表 r2–r4 | 完成 | rust-reader/params.json；worklog-impl-s0.md |
| impl-s1 | 暖白设计系统 token + A/B/C 强调色入口 | 完成 | rust-reader/src/design.rs；worklog-impl-s1 |
| impl-s2a | 书脊投影去花屏 | 完成 | rust-reader/src/shelf.rs；worklog-impl-s2a |
| impl-s2b | 书架/浮层导航按设计系统重做 | 完成 | rust-reader/src/ui.rs；worklog-impl-s2b |
| impl-s3 | 详情页重做+内联章节 | 完成 | rust-reader/src/ui.rs；worklog-impl-s3 |
| impl-s4 | 阅读器 chrome + 对比度修正 | 完成 | rust-reader/src/ui.rs；worklog-impl-s4 |
| impl-s5a/b/c | 书源/浏览历史/设置卡片化 | 完成 | sources.rs / ui.rs；worklog-impl-s5* |
| skill-native (task-38) | 书源页头按宽度分层（frontend-design） | 完成 | sources.rs +61/−23（49e4ca4）；worklog-skill-native.md |
| verify-s0…s5c 各轮 | 各段独立验收 | 完成 | .scratch/ui-apple-books/worklog-verify-*.md |
| verify-s6 / verify-s6-r2 | S6 六路终验 + 2× 专项 + A/B/C 录像 | 完成 | .tools/verify-ui-v2-final/**；worklog-verify-s6(-r2).md |
| preview-shell (task-39/37) | 离线审阅页模板+装配+十二屏包 | 完成 | preview/ui-v2-review/{index.html,data.js,compare.mp4}；.scratch/ui-apple-books/preview-stage/ |
| review-offline (task-44) | 页面实际打开复验 | 完成（结论含 FAIL 项） | .tools/verify-ui-v2-offline/r2/**；worklog-review-offline-r2.md |
| verify-s6-r2 (task-48/41) | 证据底稿 + 本汇总 | 完成 | worklog-digest-input.md / worklog-release-notes.md / 本文件 |

## 验收证据

（复跑者均为独立验收成员，非实现者自检；工作目录 D:\Documents\aidoku）

- `cargo test --locked --workspace -j 1 -- --test-threads=1` → EXIT=0，268 passed/0 failed/3 ignored — verify-s6 复跑；.tools/verify-ui-v2-final/logs/{exit-test.txt,test.log}
- `cargo run --locked -j 1 --features acceptance --bin acceptance -- <out>\full` → EXIT=0，全量 PASS（import/开合/拖拽/翻页/接缝/分组章节/队列/idle）— verify-s6；logs/{exit-full.txt,full.log}
- `YEJIAN_DESIGN_ONLY=1 … -- <out>\design` → EXIT=0 — verify-s6；logs/{exit-design.txt,design.log}，out/design/design-acceptance.json
- `YEJIAN_DAILY_ONLY=1 … -- <out>\daily` → EXIT=0 — verify-s6；logs/{exit-daily.txt,daily.log}，out/daily/daily-acceptance.json
- `YEJIAN_FILTERS_ONLY=1 … -- <out>\filters` → EXIT=0 — verify-s6；logs/{exit-filters.txt,filters.log}
- `YEJIAN_IPAD_ONLY=1 … -- <out>\ipad` → EXIT=0，10 profiles PASS（820×1180…320×1024），spread-mid PASS — verify-s6；logs/{exit-ipad.txt,ipad.log}
- `YEJIAN_SEAM_ONLY=1 … -- <out>\seam` → EXIT=0，rendered_ppp=2.0，paged+vertical 三帧 join leak=[0,0] — verify-s6 复跑、verify-s6-r2 独立复算（seam-independent.json）；out/seam/seam2x.json
- `YEJIAN_RECORD_ONLY=1 YEJIAN_MOTION_FRAMES=310 YEJIAN_ACCENT_VARIANT={A,B,C} cargo run --locked -j 1 --features acceptance --bin acceptance -- <out>` → EXIT=0 ×3，各 310 帧@1024×1366 — verify-s6-r2 实测；rec/{A,B,C}/、exit-record-*.txt
- 变体像素核对：frame-000 CTA A=(170,94,24)/B=(168,93,40)/C=(44,38,33) 与 design.rs 逐字节一致 — verify-s6-r2；.tools/verify-ui-v2-final/check-accent.py
- 并排片：ffprobe compare.mp4 → 3072×1366、310 帧、10.33s — verify-s6-r2；rec/compare.mp4 + check-t{0.5,3.2,6.5,9.8}.png
- 页面复验（浏览器）：`ego-browser nodejs -e "await import('.../r2/round-0N.mjs')"` file:// 直开 → 12/12 场景 loaded、键盘/F/侧栏生效、视频 paused→播放→seek 全过 — review-offline（独立）；.tools/verify-ui-v2-offline/r2/**
- task-38 diff 范围：`git show 49e4ca4 --stat` → 仅 sources.rs +61/−23；before/after 320pt 孤字修复与宽版无退化 — verify-s6-r2 看图核实；crop-{old,new}-*-top.png

## 偏离与未验证

- **A/B/C 强调色待用户裁决**：compare.mp4 已就绪，编号+时间未回；done 机械门槛未满足，任何变体不得记为已选。
- **页面 2 项发布缺陷未签**（task-44 实测 FAIL）：①meta 行把 ~380KB data-URI 渲染成不可断行文本，整页 scrollWidth≈2.7M px（525/1440 两档均横溢）；②「打开原图」弹 about:blank（data: 顶层导航被 Chromium 拦截）。方向已获 Lead 接受，修复未做、未复验。
- **弯曲翻页相位逐像素证伪未做**：录像与抽帧肉眼连贯，但相位级逐像素证伪未执行，列为未实测。
- **iPad 真机未跑**：本轮全链 Windows 离屏（NVIDIA RTX 5060）；云 macOS 模拟器证据属历史轮次。
- L-S1-08/09 字体排版项、L-S4-01..03 chrome 细节、L-01..06 杂项沿用遗留清单，未在本轮重验。

### 未验证清单

- 用户：A/B/C 变体视觉签收（缺：编号+时间）
- task-47：十二屏宽度及核心操作独立复核（缺：独立 PASS 结论；页面横溢/原图空白修复后需重验）
- 全员：iPad 真机安装/运行（缺：真机证据）；弯曲翻页相位逐像素证伪（缺：方法）
- L-S1-08/09、L-S4-01..03、L-01..06：见遗留清单（缺：各自「后面怎么查」栏证据）

## 死路与决策

- 试过 drawtext 烧录 A/B/C 角标 → ffmpeg fontconfig 崩溃（exit -1073741819）→ 改 GDI+ 预打标帧再编码（label-frames.ps1）。
- 试过 record.ps1 默认输出 → 会写 artifacts/（禁区）→ 改 record-one.ps1 输出 .tools/verify-ui-v2-final/rec。
- 不可逆决策：UI 重构方向 = Apple Books 暖白原生风 × 实体书脊融合、技术路线 = Rust egui/wgpu 自绘（2026-11-24 用户定，背景：网页三稿路线作废；推翻条件：用户改判）。
- 不可逆决策：用户方向来源核对——frontend-design 官方 SKILL.md（github.com/anthropics/skills）已核实 HTTP 200 并落地「hero=真实物/结构即信息」两取舍（worklog-preview-shell.md R2）。
- 用户决定：下线「藏书一览」网格，立体书架为唯一主体（2026-11-23）。

## 未决与下一步

- 第一件：用户看 rec/compare.mp4（或 preview/ui-v2-review/index.html 内嵌视频）回 A/B/C 编号 —— done 前置。
- task-47：页面横溢（meta 渲染 src）与「打开原图」修复后独立复验 525/1440。
- iPad 真机走查（拷贝漫画日常用之前）。
- 遗留：13 条（L-01..06、L-S1-03/04/08/09、L-S4-01..03），清单 .scratch/ui-apple-books/leftovers.md；其中 L-S1-01/02/05/06/07/10、L-S2b-01/02 已在后续轮次标注解决，实际未结 = 上列 13 条。
