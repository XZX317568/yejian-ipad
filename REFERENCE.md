# REFERENCE · 主体规格（对齐参考实现）

参考实现：`.tools/wenku-v0.5.6/`（用户认可的那套 Wenku8 Flutter v0.5.6）。三份细则在 `.tools/refspec/`，本页是索引 + 关键常量，改主体前先读这里。

| 细则 | 覆盖 | 体量 |
|---|---|---|
| `.tools/refspec/shelf/SPEC.md` | 书架几何、书脊排版、设计常量、图层与 joint | ~10KB |
| `.tools/refspec/motion/SPEC.md` | ArchiveReveal、ArchivePlaneTransition、滚动物理、时序 | 12.9KB |
| `.tools/refspec/wave/SPEC.md` | 抽出波、三面体渲染、封面飞行网格弯曲 | 18.2KB |
| `.tools/refspec/current/SPEC.md` | 我们 `rust-reader` 现状与五段完成度 | 10.6KB |

## 关键常量（参考实现实际取值）

**书架几何**：`width ≤ 520`；`depthX=0.66`、`depthY=-0.30`；`spine=clamp(w·0.043, 14, 18)`；`pitch=spine+11`；`cover=clamp(w·0.32, 100, 128)`；`H=(cover−20)·166/118+54`；`railY=d·2.5+d²·0.45`（|d|≤7）；抽出 `pull=cover·0.84·w(1.24−0.24w)`。

**抽出波**：`weight=cos²(d/1.48·π/2)`，焦点 ±1.48 槽；origin 位移 `pull·(0.66,−0.30) − 0.6h·smoothstep(w)`。

**封面飞行（唯一真网格弯曲）**：49×17 网格；`scale=300/(300−depth)`；`depthAt = peel·floating·(17 + 7sinπv + 3·breeze·sinπp)`；`lift ≤ 18px`；`roll ≤ 0.045rad`。

**揭示 Reveal**：`Interval(.08–.72)` × `easeOutCubic`；矩形剪口 + `travel(−8,−6)·(1−p)`；`p<.05` 不可命中；纯剪裁，无透明度、无阴影。

**交换面 PlaneTransition**：曲线 `Cubic(.18,.72,.18,1)`；incoming 位移 `(−24·dir, +12)` + 平行四边形斜切 `slant=min(180, .46w)`；outgoing `(+10·dir, −4)` 不剪裁；sheet 自屏底 `h+clearance` 升起、左移 18。

**释放手感**：`v ≥ 50px/s` 才进摩擦段（`dragRate 4.4`）；跑道 `ω=12`、交接速度 `4·pitch`；落位 Hermite `target−Δ·r³`（`landing=3Δ/v`，单调无过冲）；慢速走临界阻尼 spring（`k=144 c=24`）。

**时序**：入场 520ms 错峰 ink（0.35，24 槽）；loading 抬升 1.7s 正弦行波 9px；吸附 = 摩擦 `e`^−4.4t → 三次方落地 → ω12 临界弹簧；飞行 1000/840ms。

**配色**：milk `#FEFDF8`；书脊底 `#F2F0E9`；cap 渐变 `#E3DFD3`→milk；地影 `#8A806B` α 0.10→0；骨条 α `0.075(1−t)`。书脊文字 9px/w600，拉丁整词横排转 90°，CJK 一字一行，≤7 run。

**渲染原则（别搞错）**：书架是**固定斜轴测三面体**——无透视、无每帧光照；顶盖静态渐变 + 0.4px 描边；地面六边形阴影先影书后、固定槽序。**只有封面飞行是真网格弯曲**。

## 外部资料（2026-11-22 检索）

搜索通道：`web_search` 返回 401 失效，改用 ego-browser 驱动 Chromium 检索（已验证可用）。

| 资料 | 内容 | 核实程度 |
|---|---|---|
| [flutter_page_curl](https://pub.dev/packages/flutter_page_curl) | GPU 加速的翻页卷曲，**GLSL fragment shader** 实现，随触摸手势交互，支持 Android/iOS/Linux/macOS/Web/Windows | ✅ 已打开 pub.dev 核实（v0.1.0，7 个月前发布） |
| [Nghi-NV/flutter_page_curl](https://github.com/Nghi-NV/flutter_page_curl) | 上述包的开源实现，含 shader 源码 | ⚠️ 仅检索摘要，未打开 |
| [lostjared/shaders](https://github.com/lostjared/shaders) | 2700+ GLSL fragment shader 合集，可淘卷曲/扭曲类效果 | ⚠️ 仅检索摘要，未打开 |
| [写给前端的 GLSL 指南](https://zhuanlan.zhihu.com) | 8 种高阶 shader 效果拆解 | ⚠️ 仅检索摘要，无具体 URL |

### 外部建议：曲率实现路径（Gemini Neon，2026-11-22，未本地验证）

**结论：选顶点位移网格，不要片元解析卷曲。**

- 片元解析法的致命坑是**反求 UV**：片元着色器做卷曲只适合标准圆柱/圆锥。一旦引入参考实现里的 `sin(πv)`、微风、重力下垂这类非线性形变，从屏幕像素反求纸面 UV **没有闭式解**；硬做迭代难调，且卷曲边缘（silhouette）极易产生锯齿与纹理拉伸。
- iPad GPU 完全富余：49×17 只有 ~833 个顶点，Apple Silicon 上几乎零开销。把 `depthAt` 放进 **Vertex Shader**，用解析偏导算法线传给 Fragment 做光照与软阴影。
- 天然适配 3D 状态机与双面翻页：抽出→摊开→翻页需要统一深度遮挡（Z-buffer）；网格配合 `front_facing` 区分正反面采样不同页，能处理书脊、封面过渡。

> 我（主会话）原先倾向 shader，被这条理由否决：非线性形变下反求 UV 无闭式解。**结论采纳：网格 + 顶点着色器 + 解析法线**。

**对主体的意义（待评估）**：参考实现是 **49×17 网格 + 逐帧顶点位移**做弯曲；上面这类方案是 **在片元着色器里算卷曲与光照**。两者不是参数差异，而是实现路径差异——前者可精确控制、后者可能更省更顺。换路径属于方向性变化，需用户拍板。

## 我们与参考的关系

已经同构的部分：`paper_depth` 公式逐字一致（`motion.rs:51-59`）、`Snap` 阈值 `|v|·pitch≥50` 同源、`rubber_band` 同式（`motion.rs:17-22`）、书脊 `Plane(u,v)` 顶点投影 + `lerp_to_gamma`。

已知差异（见 `current/SPEC.md`）：`ease_out` 用 Material 曲线而非 `easeOutCubic`、`select` 曲线漂移 0.02、scale 取值、摊开槽数 ≤9、地影动态化，以及**飞行阶段多了"有限差分光照"（参考没有）**。
