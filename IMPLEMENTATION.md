# 阅读器实施记录

## 当前目标

页间 / Yejian 面向 iPad 和安卓手机。用户于 2026-11-17 选择 Rust，11 月 18 日确认界面可按阅读体验改进。`rust-reader/` 用 egui / wgpu 绘制 Wenku8 Flutter **v0.5.6** 风格的物理书架和纸张开合。选书、看详情、直接续读是三个独立动作。原 Flutter 版本位于 `app/`。

## 第二轮界面改版（2026-11-20，本地预览）

第一轮未达到用户对界面质量的要求，本轮调整页面结构与字体。物理书架放在紧凑阅读区域内，书名、进度、阅读和「详情」相邻；下方新增真实藏书缩略浏览，点缩略封面会选中该书，再直接阅读。平板高竖屏适度放大主书架与缩略封面，横屏保持紧凑；手机短横屏使用侧栏，200%字号下仍可直接进入阅读。

设置移除大块嵌套卡片，采用稳定的分组行、主题样式预览和全宽字号滑块。书源宽屏采用已安装列表与添加区域两栏，窄屏自然上下排；浏览、章节、下载与筛选用轻分隔组织。正文使用 Noto Sans SC 400，书名与页面标题使用 Noto Serif SC 600，保留 Droid 缺字后备。两份静态衍生字体及OFL、来源、转换说明随源码和交付包保留。

交付在 `artifacts/rust/ui-redesign-v2-20261120/windows/`。`打开新版预览.cmd` 使用独立五本样书，`打开页间.cmd` 使用该目录自己的个人书库。Windows EXE 为104,930,816字节；源码版本仍为0.1.1。第一轮目录保留；本轮未重打IPA/APK。

第二轮证据独立位于 `.tools/ui-redesign-v2-20261120/` 与 `artifacts/rust/ui-redesign-v2-20261120/`：

- `unit-final.log`：57项客户端单测通过。
- `design-final.log`、`design-acceptance.json`：16张生产UI截图及实际点击通过，新增缩略选书→读取同一本书检查；深浅主题、历史续读、WASM筛选与200%字号检查通过。
- `regression-final.log`：导入、物理书架开合、单本拖动、中断飞行、手机横屏直接续读、单页/双页/连续阅读、页内位置恢复、章节与队列恢复通过；静止后无持续动画或重绘。
- `filters-final.log`：真实WASM列表、分页、条件提交、取消、重置及200%筛选通过。
- `ipad-final.log`：八组尺寸、模拟安全区、44pt主要热区、详情/阅读与大字布局通过；41张输出为Windows离屏生产UI截图。
- `windows-build.log`：最终Windows预览构建；`windows-smoke.json`记录交付EXE的实际启动、五本样书/五张封面就绪与正常关闭。

实际查看并修正了横屏缩略区裁切、主题第三选项意外换行、手机横屏阅读入口出屏、大字筛选标题过大及分类区与阅读区域相接的问题。返回书架后的滚动条悬停淡出在有界等待内结束，仍保留停帧断言。`前后对比.png`、`界面预览.png`、`验收画面.png` 由实际截图排版；`书架开合.mp4`由本轮160张生产UI采样帧生成。指定SWE-2的任务封装失败状态沿用下方记录，本轮未声称SWE执行成功。

## 第一轮界面更新（2026-11-20，本地预览）

书架、详情、书源、筛选、阅读足迹和设置统一为暖纸白、墨色文字与青绿操作色，深色模式使用对应配色。页面名称和选中导航更明确；手机使用底栏，宽屏使用侧栏。书架突出当前书名、进度和阅读按钮，2–9本的小书库在静止时每本仅绘制一次，切换边缘交接淡化，保留封面开合。设置按外观、阅读、书库与备份分组，双端续读折叠；桌面文件路径收进“文件位置”，移动端继续使用系统文件选择。

复查修正了小书库焦点跨半格时整排横跳、320pt大字分类重叠、横屏200%字号书架与阅读按钮重叠，以及历史页说明出屏。共用布局判定、连续几何和按实际文字高度排版处理这些问题。书源筛选仍使用原请求状态与独立草稿，设置仍写入原有键。

本地交付位于 `artifacts/rust/ui-redesign-20261120/windows/`：双击 `打开新版预览.cmd` 会在独立的 `preview-data/` 创建五本原创图形样书；`打开页间.cmd` 使用该目录自己的 `data/`，可导入个人漫画或恢复备份。`Yejian.exe` 为 Windows 开发预览，80,189,440字节，源码版本仍为0.1.1。本轮未重打IPA/APK，iPad现有交付仍为下文build5。

验证记录在 `.tools/ui-redesign-20261120/`，截图与动画在 `artifacts/rust/ui-redesign-20261120/`：

- `unit-final.log`：客户端57项单测通过，包含小书库静止身份、循环淡化和边界连续性。
- `design-final.log`、`design-acceptance.json`：16张生产UI GPU截图；真实点击详情、翻页、历史续读、深浅主题及WASM筛选，检查200%字号、44pt主要热区和安全区。全部画面已查看。
- `regression-final.log`：导入、封面开合、拖动、单页/双页、连续阅读跨页和页内位置恢复、章节与下载恢复、静置停帧通过。
- `filters-final.log`：源列表与分页、真实WASM筛选结果、取消、重置、范围条件及大字布局通过。
- `ipad-final.log`：八组iPad/分屏/无安全区对照及详情、阅读和大字布局通过；这是Windows离屏生产UI检查，新增界面尚未运行iOS模拟器或真机。
- `windows-build.log`、`windows-smoke.json`：Windows包编译成功，实际启动交付EXE后五本样书和五张封面就绪，主窗口正常关闭。

`界面预览.png` 为原始截图排版，`书架开合.mp4` 来自真实UI的160帧录制；书与书源均为明确的本地验收示例。用户指定的SWE-2已尝试启动，但被平台任务封装错误阻断，没有执行本轮改版；实现、验收与独立审查由主代理及可用子代理完成。

## 书源筛选与列表补全（2026-11-20，build 5）

此前书源搜索始终传空筛选，静态 `filters.json` 未读取，源自带列表也未显示。本轮接入静态与动态筛选、源提供的列表、文字/排序/三态条件/单选/多选排除/说明/数值范围；按源的默认值显示，未改动的默认值不额外提交。筛选用独立草稿，应用才重搜，取消保留原条件，重置回到默认。iPad 宽屏双列，窄分屏单列，动作按钮固定在表单顶部。

搜索、列表、详情和错误回复均带请求序号，旧请求不能替换新条件的结果。下一页使用已提交的关键词与条件，失败仍可重试；新条件回到第一页及顶部。按源保留本次会话的条件。静态文件损坏只阻止筛选，不阻断已有搜索；动态选项请求失败时保留可读静态选项并显示失败原因。`hidesFiltersWhileSearching` 按源协议处理。

本地证据在 `.tools/source-filters-20261119/` 与 `artifacts/rust/source-filters-build5/`。SWE-2 实现引擎并经主模型退回修正默认值、损坏文件边界及实际参数测试；主模型负责源 UI、请求状态、真实点击验收。引擎30项和WASM夹具4项通过，随后补测静态列表在动态请求失败时仍可浏览；客户端55项通过。生产离屏GPU已实际完成列表翻页、题材/完结/排序方向/多选改变WASM结果、取消、重置、横竖屏/分屏/200%字体。MangaDex真实13项筛选页也已渲染并查看。canvas ABI两项通过，其中真实模块明确手动启用，常规测试不会假装联网执行。

真实社区抽查：MangaDex v15 读出13项筛选，带真实条件搜索返回20条；MangaBall v1 读出10项筛选，但搜索返回上游HTTP500；拷贝漫画 v24 合并得到4项筛选（含动态题材）和3个列表，搜索要求JS/网页验证。香香腐宅v13原先无法实例化：canvas宽高返回类型及坐标签名不匹配；现已对齐官方f32 ABI，实际模块可加载、合并得到5项筛选和11个列表（该项使用无网络响应夹具，未证明网站搜索）。既有字体/路径绘制占位实现仍有限制，不把接口能加载等同于完整图片处理。此记录区分元数据能读取与搜索实际可用。

最终联网结果在 `.tools/source-filters-20261119/live-results.json`：香香腐宅实际网络调用失败，11个静态列表可读取；UI会保留可解析的4个静态筛选并显示动态请求失败。源码 `f9c05a1acceb34c3d319714193647a399caea1a2` 已推送到既定公开仓库，run `44563594979` 的模拟器关卡通过。`artifacts/rust/source-filters-build5/run-44563594979/verification.json` 核验十个竖屏和四个横屏画面、真实WASM筛选结果、176条社区源、旧库升级与容器变更后内容可读。筛选页横屏及结果页截图已人工查看，安全区与表单正常。第一次package因GitHub无法分配托管runner而取消；仅重试package后，job `135721150088` 成功，复用同一源码的有效模拟器结果。

已交付 **0.1.1 / build 5**：`artifacts/rust/v0.1.1/ipad/Yejian-0.1.1-5-ipad-unsigned.ipa`，**13,791,557 字节**，arm64 / iPhoneOS / iOS15+ / 设备族[1,2] / 主程序0755 / 未签名。包内版本、标识及权限核验见 `.tools/source-filters-20261119/package-check.json`。应用标识保持 `app.yejian.reader.rust`；按已有LiveContainer更新路线保留数据容器，更新前导出完整备份。build4及以前文件仍保留。本次不重打Android，未声称用户iPad真机安装、触控或功耗已验收。

## iPad 启动与适配复查（2026-11-19）

实际复查推翻了“绿色 Actions 就代表 iPad 可用”的结论。旧脚本只要求截图命令成功，应用已退出时也会拍到系统桌面。新增日志的 run `44514736194` 捕获到明确启动错误：`max_inter_stage_shader_variables` 请求 16，但模拟器 Metal 只支持 15。eframe 在该错误路径调用 `exit(0)`，所以进程的零退出码也不能作为成功证据。

iOS 图形设备改用适合 2D 渲染的 downlevel limits，并保留适配器实际分辨率及缓冲区对齐能力。修复后的 run `44517045277` 首次留下书架、详情、阅读、历史、书源、设置六个应用画面，且阅读页确认图片已加载；证据在 `artifacts/rust/ipad-startup-audit/run-44517045277/`。人工查看发现底栏仍受系统横条遮挡，因此随后把布局边界改为 egui 原生安全区与桥接安全区的交集，补充直接输入原生安全区的回归测试。

本轮还修正了字体在首帧被桌面默认值覆盖、社区源输入行在窄分屏溢出、200% 字体标题与内容相碰、字号滑块溢出的问题。普通控件最小 44pt，正文 17pt，分类和历史继续按钮扩大；主题及阅读方式允许换行。iPad 导入使用系统文件选择，备份使用系统分享，不再展示电脑路径输入。

- `.tools/ipad-audit-20261119/ipad-targeted-tests.log`：Metal 限制兼容、原生安全区导航两项通过。
- `.tools/ipad-audit-20261119/ipad-layout-final.log`：八组尺寸的书架、书源、历史、设置，竖横屏详情与阅读，半屏和三分之一分屏 200% 字体通过。截图在 `artifacts/rust/ipad-startup-audit/local-final/`。
- `.tools/ipad-audit-20261119/ui-regression.log`：导入、开合、直接续读、双页、连续跨页、页内位置恢复、章节与下载队列、静置停帧通过。
- `.tools/ipad-audit-20261119/live-community.log`：真实社区索引 176 条，安装并加载 MangaBall 成功。此条是 Windows 网络检查；模拟器联网结果另列。

run `44520003076` 已在模拟器联网读取到社区索引 **176 条**，但横屏请求没有发出，检查按预期失败。它还证明，仅修正导航坐标不足以解决裁切：egui-winit 用整个窗口尺寸计算坐标，eframe/egui-wgpu 却用扣除安全区后的尺寸配置 Metal 画布。本轮在 `rust-reader/vendor/` 保留两份带原许可证的 0.37.0 依赖，只改 iOS 的三个画布尺寸表达式，让初始创建、恢复和调整尺寸都使用 `outer_size()`；应用布局仍避开系统安全区，其他平台维持原行为。

历史记录将退出归因于“替换 winit delegate”，此判断已撤回。当前依赖 winit 0.30.13 的 `UIApplicationMain` 不传 application delegate，通过通知接收生命周期；本轮日志确认的启动失败原因为上面的 Metal limits。文档桥继续保留已有宿主 delegate。

run `44523387780`（commit `384598c`）的实际截图确认画布修正有效，底栏完整显示在系统横条之上，社区索引仍为 176 条。该轮按预期阻止打包：iPad 多任务窗口模式明确拒绝应用内 `requestGeometryUpdate`，错误为 “The current windowing mode does not allow for programmatic changes to interface orientation.” 随后删除应用内的测试旋转代码，改用 XCTest 的 `XCUIDevice.orientation` 模拟转动设备；独立测试目标不进入 Release 应用。

run `44532550519`（commit `27124b5`）的“全部验收通过”结论已撤回。GitHub 将 sim 与 package 两个 job 标绿，但 `capture.log` 实际包含横屏 `shelf.json` 缺失的 `FileNotFoundError`：外层 workflow 缺少 `pipefail`，`tee` 的成功掩盖了截图脚本的失败。竖屏六页、阅读图片及社区索引 176 条仍有有效证据；旧 XCTest 仅证明窗口旋转，固定等待后截图没有证明横屏漫画加载完成。证据在 `artifacts/rust/ipad-startup-audit/run-44532550519/`。本轮 build 4 已补齐失败阻断、每次启动独立确认、横屏真实内容与存放目录变化后的书库恢复检查。

本轮 **0.1.1 / build 4**（commit `f0303c051fac8a9d0ec42351535e5632c9df290a`）已通过 run `44545119356` 的 sim 和 package。修复两类实际问题：书库保存绝对位置，iOS 重装换容器后找不到档案与封面；已完成下载也会保留旧 CBZ 位置。书库升级为 schema 4，以库内相对位置保存受管文件，兼容旧绝对值与 Windows 相对根，保留阅读进度、时间、分类、章节和隐藏状态。新版本打开后，旧版不能直接打开 schema 4；回退应使用更新前完整备份。

SWE-2 负责 reader-core，主模型复核后退回修正旧库测试、相对根兼容和返回值合同，再整合下载路径和验收关卡。`.tools/ipad-audit-20261119/relocation-prefailure.log` 记录修前四项失败；`relocation-final.log` 记录修后54项通过；`sources-relocation-tests.log` 记录29项通过。`build4-local-gpu.log` 与 `artifacts/rust/ipad-startup-audit/build4-local/` 为本轮八组尺寸和大字布局的离屏验收，实际合成图已查看。`evidence-gate-tests.log` 的8项测试和 `pipeline-failure-test.log` 验证缺失内容会失败、日志管道不再吞退出码。真实模拟器的完整证据在 `artifacts/rust/ipad-startup-audit/run-44545119356/`：`verification.json` 确认七个竖屏和三个横屏就绪、横屏阅读已加载、社区索引176条、旧书库schema3→4升级。`legacy-library.json` 与 `relocated-library.json` 证明数据容器UUID确实改变而五个book ID保持不变。已人工查看横屏书架、详情、阅读和社区源截图：封面恢复彩色图、阅读页有实际图片、控件处于系统安全区。

build 4 交付：`artifacts/rust/v0.1.1/ipad/Yejian-0.1.1-4-ipad-unsigned.ipa`，**13,706,347 字节**。`.tools/ipad-audit-20261119/build4-package-check.json` 记录包内验证：`app.yejian.reader.rust`、0.1.1 / build 4、iPhoneOS、设备族[1,2]、最低iOS15.0、arm64 Mach-O、主程序可执行权限0755、无签名或描述文件。更新先从旧版导出完整备份，再由已配置的 LiveContainer 沿用原数据容器导入新版。此前交付文件保留，安装说明已同步。

此前交付为 **0.1.1 / build 3**：`artifacts/rust/v0.1.1/ipad/Yejian-0.1.1-3-ipad-unsigned.ipa`（13,702,953 字节），其横屏内容验收尚不成立。已核对包内 `app.yejian.reader.rust`、版本 0.1.1 / build 3、iPhoneOS、设备族含 iPad、最低 iOS 15.0、arm64 Mach-O 主程序、无签名。旧 build 2 保留在同目录，不含本轮修复。仓库经实时 API 确认为公开；源码与验证流程提交在该仓库，未发布商店版本。

用户 iPad 上的 LiveContainer 签名与启动、文件选择/分享、真实分屏手势、功耗和帧率仍缺少实机证据。需要 JS、网页人机验证或图片后处理的源、后台自动追更和跟踪服务仍为功能缺口。

## Rust 0.1.1 交付（2026-11-18）

安装包、Windows 启动器、可选的三本原创示例和使用说明位于 `artifacts/rust/v0.1.1/delivery/`。书架动画位于 `artifacts/rust/v0.1.1/shelf-motion.webm`。旧版 Windows 书库可用完整备份迁入新版。

- **书架与阅读**　点中央封面看详情和已下载章节，点书名下的按钮开始或继续阅读。从书架直接续读后，返回仍停在原来的书和搜索结果。单本书架固定居中。短横屏将封面和可操作的信息左右排布，200% 大字也能直接进入阅读。
- **章节与进度**　同源、同作品的章节合为一本。未读作品从最早的已下载章节开始，有进度的作品回到最后阅读的一章。连续滚动保留跨页后多出的距离，v3 书库记录页码和图内比例。独立 SQLite 连接提交进度，紧接着刷新、备份、恢复或正常退出时会等待先前的保存操作。保存失败时可在界面重试。
- **下载恢复**　前台队列持久化完整的书源漫画与章节参数。每页通过图片解码检查后才算下载成功。继续任务时复用已完成的页，归档已生成但入库中断时可直接重试入库。测试用本地 HTTP 和 WASM 源下载两页，随后导入最终 CBZ、关联章节并用阅读器打开两页。
- **界面实操**　生产程序在离屏 GPU 上接受点击、滑动、滚轮及返回输入，覆盖手机竖横屏、200% 大字、平板、被中途打断的封面飞行、单本搜索、长图旋转与重开。9000 像素长图可逐段滚动。静置书架的 `repaint_delay` 为 `Duration::MAX`。证据在 `.tools/rust-v011-landscape-gpu.log` 和版本目录的截图。动画取自该次运行的 160 张采样帧。
- **iPad 界面（2026-11-19）**　按 iPad 优先重排大屏：横屏书架放大到 820×640 并抬升缩放上限，书架与右侧信息列共用视觉中线；竖屏与分屏把书架放在上方、标题和按钮紧随其下；书脊在左右边缘淡出，不再硬切。详情页横屏改为大封面加独立文字栏，竖屏封面加大并整体居中。书源、历史、设置改为居中的 760 宽阅读栏。全局控件改为触控尺寸（30 交互高度、12×8 按钮内边距、正文 15、标题 22）。离屏验收新增 1366×1024、1024×1366、507×1024 三组书架/详情与 1366×1024 的书源、历史、设置截图，全部通过；文件为 `artifacts/rust/v0.1.1/ipad-{landscape,portrait,split}-*.png` 与 `ipad-landscape-{sources,history,settings}.png`。
- **安卓启动修复**　真机首启在 `JNI GetObjectClass` 处崩溃：`ndk-context` 给的是 `Application`，旧代码却按 Activity 取类，找不到 `inboxDir()`。改为经 Application 的 ClassLoader 解析 `app.yejian.reader.RustActivity` 并缓存全局引用。手机实测可进入书架，版本 0.1.1 / code 3；当前交付包为 code 4（含 iPad 界面改动）。APK 与旧版同一调试证书，可覆盖安装并保留书库。
- **社区源**　书源页新增“社区源”按钮，一键填入 `https://aidoku-community.github.io/sources/index.min.json` 并读取。Windows 上的真实仓库检查（`crates/source-engine/tests/live_repository.rs`，需 `YEJIAN_LIVE_REPO=1`）取得 176 条索引，安装并加载 `multi.mangaball` 成功。设备端的手动导入结果仍待复测。
- **回归证据**　`.tools/rust-v011-audit-tests.log` 记录本次复查中的 44 项测试。随后的保存顺序和未读章节顺序修改分别通过 `.tools/rust-v011-shelf-position.log`、`.tools/rust-v011-shelf-chapter.log` 的定向测试。v3 升级、备份与章节排序结果在 `.tools/rust-v011-reading-core.log` 和 `.tools/rust-v011-chapter-order.log`。
- **包与设备**　安卓交付包为 23,818,090 字节，`app.yejian.reader.rust`，0.1.1 / code 4，minSdk 24，targetSdk 36，与旧包同一调试证书。交付件通过 APK v2 签名检查，唯一原生库是 arm64 `libyejian.so`，其四个加载段均为 16 KiB 对齐。启动类 `app.yejian.reader.RustActivity` 显式加载该库，两条 Java 回调和 NativeActivity 入口均已导出。code 2 的原始交付件保留在 `.tools/checkpoints/delivery-code2.apk`。Windows 交付 EXE 在独立后台桌面启动、导入六页示例并正常退出，见 `.tools/rust-v011-delivery-windows-smoke.log`。编译日志为 `.tools/rust-v011-delivery-{android,windows}.log` 与 `.tools/rust-fix-android.log`。

iPad 工程版本为 0.1.1 / 2。2026-11-19 在用户授权下，源码上传到私有仓库 `XZX317568/yejian-ipad`，由 GitHub Actions 的 macOS 15 / Xcode 16.4 / Rust 1.97.0 完成 Release 真机编译，最新未签名安装包为 `artifacts/rust/v0.1.1/ipad/Yejian-0.1.1-2-ipad-unsigned.ipa`（13,701,753 字节，含 iPad 界面改动与社区源按钮）。实测该包 `app.yejian.reader.rust`、0.1.1 / 2、`iPhoneOS`、设备族 [1,2]、minOS 15.0，主程序为 arm64 Mach-O，含 `Assets.car`，没有 `_CodeSignature`，需由 LiveContainer 一类工具在设备上签名。首轮构建失败于脚本里 `$TRIPLE` 后紧跟中文被 bash 当成变量名，改成分隔写法后成功；日志在 `.tools/ipad-run1.log` 与仓库 Actions 页面。iPad 启动、触控、分屏、帧率和耗电仍未在设备上验证。交付时未发现已连接安卓设备或无线调试服务。需要 JS、网页人机验证或图片后处理的书源，以及后台自动追更、跟踪服务、自动发现和持久配对，仍是功能缺口。

## Rust 0.1.0 历史基线（2026-11-17）

- 新客户端的包标识为 `app.yejian.reader.rust`，与旧版并装；不直接改写原 Isar 库。桌面默认数据目录和移动端私有目录独立，支持通过备份迁入。
- 界面、书架网格、动画、书库、阅读、网络和源引擎使用 Rust。Android Java / iOS Swift 只桥接文件选择、分享、安全区、返回与启动；Rust 包不携带 Flutter/Dart/WebView 界面运行库。现有 Rust 同步宿主通过 `app/rust/sync_host` 路径复用。
- 新入口包括真实 SQLite 书库、CBZ/ZIP、CBT/TAR、CBR/RAR 和图片目录导入，归档按页读取，进度、分类、多选移出、搜索、历史、备份及恢复。阅读支持单页、平板双页、右向左、缩放和纵向单页滚动；长条图片分成不超过 2048 px 高的 GPU 纹理，快速翻页取消过期解码，换书清掉旧页纹理。
- Aidoku WASM 引擎脱离 FRB/Dart，每源独立线程；UI 接通仓库安装、搜索分页、详情/章节和下载为 CBZ 后阅读。切源清掉旧结果，下一页失败不推进页码，重复结果去重。未下载完的 `.part` 仅清理本次创建的文件，完整文件改名失败则保留恢复副本。
- 书架按原参数移植为有纹理的 GPU 网格；保持 .66 / -.30 投影、520 / 420 / 1000 / 840 ms 时间及惯性吸附。窗口按附近书本生成，静止时停止请求动画帧。
- 进度新增独立 `progress_at` 时间，v1→v2 数据库迁移保留旧的真实阅读时间；新设备导入的第 0 页不能覆盖旧进度，主动重读第 1 页仍能同步。备份恢复后阅读设置立即应用。私密阅读不写入新历史/进度。
- 局域网手动配对通过复用的双向 TLS 宿主交换进度。只同步两端都有的同一内容文件，缺书、越界页均明确报告。Android 的系统手势返回与三键返回均已接线，弹窗优先关闭，然后阅读→详情→书架，根页面才退出。
- 最终 Android arm64 开发测试包：`artifacts/rust/delivery/Yejian-0.1.0-rust-arm64-debug.apk`；原始输出 `rust-reader/platform/android/app/build/outputs/apk/debug/app-debug.apk`。2026-11-17 **17:03** 最终打包成功，23,547,626 bytes（约 22.46 MiB），版本 `0.1.0` / code `1`，minSdk 24 / targetSdk 36，启动类 `app.yejian.reader.RustActivity`，APK v2 签名验证通过。唯一运行库为 arm64 `libyejian.so`。构建日志 `.tools/rust-android-final.log`、最终打包日志 `.tools/rust-android-final-package.log`。
- iPad 的 Rust 静态入口、Xcode 工程、文件导入/导出桥、安全区、旋转/分屏配置与 AppIcon 已接好。2026-11-19 的云端 macOS 构建已证明真机目标能编译、链接并生成 IPA；签名和真机运行仍无证据。
- 迁移前工程文档恢复点：`.tools/checkpoints/before-rust-migration/`。旧 APK 保持原路径。

### 0.1.0 的验证与当时的能力边界

- 核心检查 39 项通过（3 项单元 + 36 项书库流程）：自然顺序、坏档案隔离、真实 RAR 字节、页码回退、私密、原生/旧备份、进度合并、旧库升级；日志 `.tools/rust-core-final.log`。
- UI/工作线程/同步 13 项通过：千本几何范围、动画停止、弹窗返回顺序、换书纹理回收、长条分块、恢复后即时设置、来源隔离/分页、下载失败文件保护及两套真实书库经本机双向 TLS 同步；日志 `.tools/rust-ui-sync-final-tests.log`。
- 原生源引擎 19 项及 1 个文档编译检查通过：WASM 搜索、编码、燃料预算、坏源、人机验证错误、HTTP 状态与体积上限、TLS 根证书配置、仓库格式和路径检查；仍适用的日志 `.tools/rust-tests-final.log`。测试使用本机 HTTP / WASM 夹具，未冒充外网源的真机验收。
- 生产 UI 在独立离屏 GPU 目标运行，使用真实临时 SQLite 库和原创 CBZ 夹具，实际发送点击、拖动、搜索和 BrowserBack 输入。导入→切书→打开→翻页→重读数据库→返回，浅/深色、1024×768 平板、375×768 大字、右向左双页均通过。日志 `.tools/rust-ui-acceptance-final.log`，截图/动画在 `artifacts/rust/`，详情见 `acceptance-result.txt`。
- 静置检查读取 egui 实际 `repaint_delay`：为 `Duration::MAX`，没有持续申请帧；这只证明该路径按需重绘，不代表设备功耗已经降低。`shelf-motion.webm` 由 60 fps 采样帧生成，不是设备实测帧率。
- 2026-11-17 检查 `adb devices` 与 `adb mdns services` 均未发现设备。APK 未安装，Android 的触摸/键盘/文件选择器/网络/系统返回和真正双设备同步仍需可用安卓设备；iPad 需 macOS/Xcode、签名与设备。当前源码、桌面 GPU 行为、Android 编译与签名检查分别成立，不能互相替代。
- Rust 版尚未完整覆盖旧应用：JS/Cloudflare 网页验证、需图片后处理的源、动态源设置 UI、追更/批量后台下载与断点续传、跟踪服务等仍未迁入；章节目前下载为独立本地书。纵向模式为单页纵向滚动并切页，尚非整章无缝长卷。
- 进度以文件内容 ID 匹配，依赖设备时钟，尚无自动发现/持久配对、删除同步或独立分类时间；单独修改分类不会立刻同步。旧 Mangayomi 备份只能迁入当前设备仍能访问的本地档案，缺失档案逐条报告并保留原始 JSON；不直接读取旧 Isar。
- 书架已按参考几何和曲线实现并完成渲染对照，尚未做两台设备上与 Wenku8 原版的逐帧视觉/手感比较，不宣称完全一致或“历史最强”已经验证。

## Flutter 对照版本（迁移前已验证）

- 研究文档已覆盖 14 个项目、9 个项目的一手用户反馈、24 类痛点、65 个一手来源；本轮不再重复广泛调研。
- `app/` 为 Mangayomi v0.9.96 的公开源码快照，源码基线提交 `39381ba`。项目 SDK 为 Flutter 3.50.0 / Dart 3.14.0，构建入口与资源限制见 `BUILD.md`。
- 参考书架代码固定到 Wenku8 v0.5.6，下载原文在 `.tools/wenku-v0.5.6`。原始许可证和 NOTICE 保存在 Shiori 下并接入应用许可证页面；改编记录见 `app/packages/shiori/WENKU8_ADAPTATION.md`。
- 正式原型是现有应用中的真实 LibraryScreen，直接读取 Isar 收藏、历史和设置。web 入口为同组件动画预览，使用明确标注的原创示例数据，支持鼠标/触摸拖动、搜索、主题切换和打开/返回详情。
- 2026-11-17 **14:36** 已成功生成含最终导航字体修复的 Android arm64 开发包（217,165,096 bytes，约 207 MiB）。最终包位置为 `app/build/app/outputs/flutter-apk/app-debug.apk`；构建日志 `.tools/final-shelf-apk-low-memory.log`，用时 253.6 秒。
- 最终 APK 已核验：名称「页间」、包名 `app.yejian.reader`、activity `app.yejian.reader.MainActivity`、版本 `0.9.96+129`、minSdk 24，包含 arm64 Flutter/Rust/Isar 原生库及三份 Wenku8 许可资产。完整运行库只为 arm64 构建；其他 ABI 的零散依赖库不代表支持其他 ABI。
- 尚未安装到安卓或 iPad 设备。iPad 未签名 IPA 已由私有仓库的 GitHub Actions 产出，未签名、未上架、未做真机验收。上传范围仅限用户授权的私有仓库 `XZX317568/yejian-ipad`；没有公开代码、没有收费服务调用。

## 已实现的书架行为

- 真正的物理书脊、书顶和封面投影。保留原版 `.66 / -.30` 投影、最大 520 px 书架宽度、半径 1.48 的连续抽取曲线。
- 520 ms 入场、420 ms 点选切书、惯性滑行后准确吸附；打开 1000 ms、返回 840 ms 的封面弯曲飞行。点非中心书先居中，再点中心书打开。
- 手指滑动、前后按钮、循环切书、重排后保持选中身份、旋转/窗口缩放、减少动态效果。静置不启动循环动画。
- 只创建屏幕附近的书本节点，只解码邻近封面。1000 本夹具验证实际节点少于 45，循环边界不丢中心书、不改打开对象。
- 完整书库的搜索、筛选、分类、下拉刷新、长按多选及封面勾选接回原功能；书架模式不再重复叠一个“继续阅读”书架。旧网格/列表可继续选择。
- 新装默认书架，已有显示偏好不改；持久化枚举 0–3 保持，新增 shelf=4，JSON/Isar 映射已接通。
- 手机顶部标题与玻璃胶囊导航，iPad 窄侧栏；内容宽度达到 880 px 时书架与信息并排，窄分屏和大字模式改为上下排列。胶囊标签继承应用字体，修复中文标签在视觉测试中显示方框的问题。

## 已完成的可靠性修复

- ZIP/CBZ/TAR/CBT 按页解压，CRC 校验、`.part` 临时写入和原子替换；好坏档案混合导入按文件隔离。页缓存有容量目标和近期页面保护。RAR 仍走整章 Rust 解压。
- 同步处理长 URL、异常行诊断、UTF-8 分批、独立实体游标和精确删除确认；失败不推进水位。冲突合并仍依赖设备时钟，未实现完整操作日志和撤销恢复。
- 重新阅读时保存每次可见页；同步章节页码优先于旧设备位置；更新在事务中读最新行；私密阅读不落盘。
- 首帧显示启动界面，初始化失败进入可见恢复页，可重试、复制路径与报告。数据库失败仅关闭句柄；初始化仍运行时发生异步异常，不允许并发重试。
- 新装跟随系统主题、默认漫画、关闭 Discord 和上游自动更新；保留上游 OAuth 回调 scheme 以维持登录，和上游并装时仍可能发生回调争抢。

## 当前证据与待完成

已确认：

- `reading_shelf_test.dart` 最新 9 项通过：选书/重排、空库、单本、200% 大字、快速切书、缩放、双向滑动吸附、静置、1000 本循环窗口、Hero 往返及数值连续性。日志 `.tools/shelf-window-retest.log`。
- 真实书库与选择集成 6 项通过：最近阅读排序、多选勾选/取消，以及 390×844、1024×768、375×768（200%字）搜索与渲染。日志 `.tools/library-final-visual.log`。
- 最终字体修复后，三种尺寸真实书库渲染/搜索与两种底部导航模式共 5 项通过，日志 `.tools/final-label-render.log`。三张最新截图位于 `app/build/acceptance/{phone-library,tablet-library,split-large-text-library}.png`，均已实际查看，中文标签正常。
- 阅读页码 4 项、新装身份/设置 8 项通过，见 `.tools/shelf-reader-regressions.log`。该次运行中的多选测试夹具失败已在上述 6 项集成检查中修复并重验。
- 启动/恢复/新装/引导 15 项通过，由对应代理执行并交回；主会话验收记录与构建不将其视为设备验证。
- 样式规则和图标标签 7 项通过。最终定向静态分析 12 个路径，无错误、警告或提示，日志 `.tools/final-shelf-analyze.log`。
- 最终 web release 已构建，日志 `.tools/final-shelf-web.log`，入口 `http://127.0.0.1:4173`。独立后台浏览器实际检查手机/平板、浅色/深色、拖动与按钮切书、搜索空结果、说明弹窗，以及打开/返回详情；浏览器错误输出为空。
- 最终动画为 `artifacts/shelf-motion-final.webm`，按 60 fps 录制，包含切书、封面打开与完整归位；截图为 `artifacts/preview-{phone,tablet,dark}.png`。录制参数不代表设备能稳定达到 60 fps。web 使用原创示例记录；真实书库布局以上述 LibraryScreen 截图为准。

本轮遇到的环境问题：

- D 盘曾耗尽；失败产物删除被自动审批拒绝后，没有绕过删除。改用对项目 Rust 构建目录的可逆 NTFS 压缩，保留全部文件并释放约 4 GB。
- Windows 可提交内存紧张，Flutter 测试两次在 JIT/加载阶段报 Out of memory，后续单独重跑已通过。第一次最终 APK 增量构建期间可提交余量降至约 200 MB，并报 Guava 类加载/Android 插件配置失败；未对该失败武断归因。停止本工程 daemon 后回收内存，改用 1 GB Java 堆、串行 GC、单 worker 重试成功，完成后已请求停止本工程 daemon。历史失败日志 `.tools/final-shelf-apk.log`，成功日志 `.tools/final-shelf-apk-low-memory.log`。

以上 Flutter 对照版本在本机可完成的源码、组件交互、视觉、web 预览及 Android arm64 开发包验收已完成。IPA、安卓安装启动、双设备同步、设备帧率/手感、功耗与原版逐帧同机对照尚未验证；需要各自可用设备与 iOS 构建签名环境。当前开发入口以本文顶部 Rust 结果为准。

## 技术路线讨论

用户于 2026-11-17 明确要求迁移 Rust 界面，已取代此前维持 Flutter 的技术选择。Slint 与 egui 的实际接口已检查；本次采用 egui / wgpu，以直接表达斜投影封面和纸张变形网格。Rust 迁移不等于已经证明更省电；耗电需在同设备、同数据、同画质、相同亮度与帧率下比较正式构建。
