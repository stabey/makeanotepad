# 实施状态

## 基线与推进顺序

以 `Rust_GPUI_Editor_Agent_Handoff_v1.0.md` 为产品契约。当前从空代码仓库启动，不能把上游能力算作本项目交付。

1. 审查依赖与唯一文本权威，完成 Kit 原生编辑路径。
2. 并行实现纯 Rust 有界分页/检索实验与 UI；共享接口先经 design-review 评审。
3. 接入安全保存及草稿恢复，验证错误与退出边界。
4. release 下测首屏、普通编辑、多文件、搜索、取消及恢复；保留正确性约束。
5. 评估 NSH；未通过前不进入默认关键路径。继续推进交接文档 P1/P2，扩展、语言和平台交付按原验收矩阵追踪。

## 环境观察

- macOS Apple Silicon，Mac16,10，32 GiB 内存。
- Rust/Cargo 1.99.1；Xcode 路径 `/Applications/Xcode.app/Contents/Developer`。
- 工作卷可用空间约 1.4 TiB（实施开始时）。
- Parallels Windows 11为ARM64（验收完成后已恢复`suspended`状态），不作为Windows x64裸机性能基线；此前基于ADR 013/014构建已完成真实Windows 11 ARM64桌面GUI、快捷键、原生`IFileSaveDialog`另存为闭环、NTFS 5阶段崩溃保存及虚拟机帧缓冲截图验收。Wave 0提交`91c301c`（ADR 015/016）已通过五目标原生CI（含Windows PE GUI/CUI子系统校验，可供用户进行Windows实机测试），但ADR 015–018重建二进制的Windows桌面GUI与物理IME仍待实机复验。Windows x64、Linux x64与macOS Intel仅完成GitHub原生CI编译、测试、原子保存与进程故障门禁，尚未完成桌面GUI与性能验收。
- 用户提及的 `edtor-review` 不在已安装 agent 列表；本次由项目指定 `design-review` 承担独立设计讨论，未假称联系到不存在的 agent。

## 当前交付

- 默认原生应用`target/release/editor`可在本机运行；Kit-only、无NSH默认依赖，零`Cargo.toml`/`Cargo.lock`新增依赖。最新**ADR 017**（SQLite `schema v3`、`layout format v3`、主应用只读同文档多视口与次视图快速关闭）与**ADR 018**（Kit-only 纯 Rust 无损 JSON/JSONC 工具及有界只读光栅图像预览）叠加ADR 015/016在macOS ARM64本地全量通过：`cargo fmt --all -- --check`、`spikes/multiview-paged` fmt检查、`test_package_artifact.py` **9/9**、`cargo test --locked --workspace` **195/195**（`editor` lib 82 + `editor-services` lib 27 + `services` integration 86）、`vendor/gpui-base`私有测试（`ime_pin_` **11/11**、`text_wrapper::` **27/27**、`test_crlf_columnar` **1/1**）、`editor-services smoke`（6场景）、**8次原生窗口回归**（`startup`、`full`、`usability` 含`columnar_multicursor_single_undo_verified=true` / `json_lossless_and_undo_verified=true` / `image_viewer_budget_and_release_verified=true` + Restart A、`groups`、`splits` + Restart B 3窗格3视图含1共享Paged+1 Editable、`ime` 3规格×192次提交）以及两个隔离实验（`shared-text-feasibility` 66阶段/0失败、`multiview-paged --views 8` 66帧/14/14标志），正常退出均观察到durable ACK；产物SHA-256见[`docs/evidence/adr017-018-native-regressions.json`](docs/evidence/adr017-018-native-regressions.json)。
- 公开仓库为[stabey/makeanotepad](https://github.com/stabey/makeanotepad)。Wave 0合入ADR 015/016的提交`91c301c`（`91c301c86d9bbaaacfc334e68e95072647107497`）在[五目标原生CI（44015994062）](https://github.com/stabey/makeanotepad/actions/runs/44015994062)全部6个job一次通过（`fmt`、`aarch64-apple-darwin`、`x86_64-apple-darwin`、`x86_64-pc-windows-msvc`、`aarch64-pc-windows-msvc`、`x86_64-unknown-linux-gnu`，含Windows PE GUI/CUI子系统打包门禁，已备好Windows测试产物；ADR 017/018本地定稿待下一次推送触发CI）。
- 普通文档：唯一EditorState权威、版本化原子编辑、同步程序化提交与原生事件匹配、NoOp、后台普通搜索/替换、高亮、主题、保存与草稿恢复。格式仅元数据修改也进入dirty/恢复保护，保存期间再改格式不会被旧保存确认抹掉。
- 日常界面与内置纯Rust工具：默认打开文档/最近文件侧栏、命令面板、快速切换、语言/编码/换行/缩进选择、折行与空白显示、矩形列选（`Alt+Shift`拖拽；Linux亦可`Ctrl+Alt`）与垂直多光标（`AddCursorAbove`/`AddCursorBelow`经快捷键、Edit菜单及命令面板调用，CRLF边界已验证）、活动选区行编辑/注释、偏好持久化。依据交接文档§10.1与§14.4 Kit-only回退路径，内置纯Rust无损JSON/JSONC校验/格式化/压缩（`Tools`菜单与命令面板，4 MiB/深度2048上限、单步Undo、错误位置跳转）与有界只读光栅图像预览（PNG/JPEG/WebP/GIF/BMP/ICO/TIFF魔数与头闸门、`Fit`/`10%–800%`缩放、256 MiB工作区LRU + `cx.drop_image` GPU释放；**SVG仍作为可编辑XML文本打开**）。自定义行命令仍只作用于活动选区，侧栏不是项目目录树。
- 标签组、不同文档分屏与只读同文档多视口：最多8窗格，四方向拆分、标签跨窗格拖放、尺寸调整/均分、焦点切换、关闭合并、仅显示焦点窗格与重启恢复已实现。基于SQLite `schema v3`与`layout format v3`，主应用现已支持**只读**文档（`Paged`与`Image`）同文档多视口（`SplitAndCloneActiveViewRight` `Cmd/Ctrl+Alt+Shift+L`、`SplitAndCloneActiveViewDown` `Cmd/Ctrl+Alt+Shift+J`、`CloneActiveReadOnlyView`），共享同一底层文档/分页源与缓存预算、支持独立字节偏移或缩放、次视图快速关闭免弹窗及重启恢复（`splits` -> Restart B已验证）；同文档**Editable**多视图仍保持**NO-GO**（ADR 009）。
- 大文件：有界ReadAt、共享页缓存、稀疏索引、虚拟化视图、长行分段、64位字节导航、可取消流式搜索与主应用只读多视口共享源。1/10/64 GiB非稀疏日志及长行/密集行已实测，64 GiB在真实窗口显示。
- 多文件：16文件并发索引与共享缓存实验；分屏交付时100/1024标签×1/2/4/8窗格恢复均只加载可见文档，1024标签8窗格正文占用557,056 bytes；预算限制1个编辑器时优先焦点窗格，不按窗格复制预算。已有1000草稿整组取消/保留关闭验证保留。数字和严格测量边界见 [性能报告](docs/PERFORMANCE_REPORT.md)，本轮未重跑该矩阵。
- 同文档多视图的固定Kit公开API门槛（Editable）仍为NO-GO，不采用双缓冲同步。受维护`vendor/gpui-base`原生补丁保持**6个文件**（`state.rs`、`undo_manager.rs`、`element.rs`、`display_map.rs`、`wrap_map.rs`、`text_wrapper.rs`），保留单编辑器IME结算与历史修复（`shared-text-feasibility` 66阶段/0失败）、渐进折行与CRLF多光标边界修复；主应用已启用只读共享源多视口，可编辑共享视图仍保持隔离研究，见[ADR 009、ADR 011、ADR 016、ADR 017](docs/ARCHITECTURE_DECISIONS.md)及[测试报告](docs/TEST_REPORT.md)。
- 普通加载、首次折行与渐进折行收敛：ADR012（接管Rope与顺序行布局）、ADR013（消除双SumTree重叠、单行容纳快路径、`WrapScratch`复用）与ADR016（256上限`WindowTextSystem`缓存轮换、视口优先渐进折行与滚动锚点补偿、非活动标签瞬时缓存释放）保持生效；本轮未重跑ADR 016 64 MiB配对矩阵（最新同机67,108,860 B基准见[性能报告](docs/PERFORMANCE_REPORT.md)：`short_unique`首帧344.53 ms / 355.93 MiB、全量收敛464.97 MiB均值 / 9.28秒）。
- NSH候选已实际构建运行并保留截图，但需要上游兼容补丁，真实IME、主题同步、完整SDK等门槛未通过。决定**暂缓进入默认主线**，不是将TS/TSX插件系统记为完成。
- 元数据排序及面板焦点问题已有修复与复审，失败过程和运行证据保留。此前外部键鼠及截图不可用的限制，本轮未补充外部交互验收；原生窗口内派发不能替代物理HID或像素证明。范围见 [已知限制](docs/KNOWN_LIMITATIONS.md)，不是完整Notepad++替代品交付。

## 交接任务对应状态

| 任务 | 当前状态 |
|---|---|
| T01 仓库/工具链/记录 | 已建立；锁文件、运行说明、许可证与报告齐全 |
| T02 原生窗口 | macOS Apple Silicon（含ADR 017/018）与Windows 11 ARM64（Parallels桌面，ADR 014版本）实际运行通过；Wave 0 CI（`91c301c`）已备好Windows PE GUI产物，ADR 015–018 Windows桌面复验、Windows x64裸机、Linux及macOS Intel桌面GUI未验证 |
| T03 混合候选 | 已完成候选实验与回退决策；NSH采用门槛未通过 |
| T04 文档适配/候选收敛 | 本轮事务及版本场景通过，默认Kit-only |
| T05 打开/编码/保存 | UTF-8/BOM/LF/CRLF、安全保存与冲突核心场景通过；原生系统文件面板边界未穷尽 |
| T06 草稿/会话 | 正常退出恢复、空白草稿、100标签惰性启动与`schema v3` / `layout format v3`迁移/重启通过（13标签Restart A与3窗格Restart B通过） |
| T07 故障边界 | 服务进程中断及保存阶段注入通过；真实掉电未验证 |
| T08 编辑工具/搜索 | 搜索替换、事务、高亮、命令面板、快速切换、视图配置、活动选区行编辑，以及`Alt+Shift`矩形列选/`AddCursorAbove`/`AddCursorBelow` CRLF多光标单次撤销已通过`--smoke=usability`；自定义行操作仍限活动选区，真实系统IME候选窗未验收 |
| T09 分页/长行 | 服务、实际原生视图与主应用**只读同文档多视口**（`schema v3 / format v3`，共享单分页源、独立偏移与次视图快速关闭）已验证；不是大文件可编辑，且Editable同文档多视图仍为NO-GO |
| T10 流式搜索 | 跨块、64位末尾、限额与取消已验证 |
| T11 索引/源变化 | 渐进索引预算、追加/截断/替换失效服务场景通过 |
| T12 SDK/NSH生命周期 | 暂缓；NSH TS/TSX宿主未并入主线（内置JSON/图像按§10.1/§14.4由Kit-only纯Rust交付，不冒充JS插件运行时） |
| T13 JSON扩展 | **按交接文档§10.1/§14.4 Kit-only回退路线以纯Rust内置交付**：`json_tools.rs`无损RFC 8259 + JSONC校验/格式化/压缩（4 MiB/深度2048、后台错误定位、单步Undo与SQLite恢复已通过`--smoke=usability`）；NSH TS插件形式仍暂缓 |
| T14 LSP | 不再作为本体目标；未来可由插件提供，当前未实施 |
| T15 图片查看器 | **按交接文档§10.1/§14.4 Kit-only回退路线以纯Rust内置交付**：支持PNG/JPEG/WebP/GIF/BMP/ICO/TIFF有界头探测（32 MiB/8192 px/128帧/128 MiB RGBA）、256 MiB工作区LRU与`cx.drop_image`释放、`Fit`/`10%–800%`每视图缩放及只读多视口独立缩放（已通过`--smoke=usability`）；SVG保留为可编辑XML文本 |
| T16 离线扩展包 | 暂缓；已有依赖/资源许可清单及内置Rust能力不等于T16扩展包或TS插件交付 |
| T17 性能收敛 | 已有同机真实实验与原生提交基线；ADR 016已实现百万唯一短行首帧344.53 ms及全量收敛464.97 MiB均值RSS；物理屏幕首帧、真实IME、新功能跨平台桌面性能分布未完成 |
| T18 两平台发布 | Wave 0提交`91c301c`（ADR 015/016）已通过五目标CI（`44015994062`，含Windows PE GUI/CUI校验，待用户Windows实测）；ADR 017/018待下次推送CI；Windows桌面复验、Windows x64裸机/Linux/Mac Intel桌面GUI、签名/公证及安装包未交付 |

## 后续推进顺序

1. 公开仓库与Wave 0（`91c301c`）五目标CI开发构建已通过（含Windows PE GUI/CUI子系统门禁）；后续保持构建、CLI、GUI和性能验收分开记录。
2. 本体保持纯编辑器定位，不引入项目模型或LSP。标签组、不同文档分屏、只读同文档多视口（`schema v3 / format v3`）、内置纯Rust无损JSON/JSONC与有界图像预览（ADR 017/018）、单编辑器IME修复、普通加载与视口优先渐进折行（ADR 012/013/016）、三平台原子安全保存（ADR 014）、Windows GUI子系统与路径展示剥离（ADR 015）及T08列选/垂直多光标已实现并在macOS ARM64完成回归验证。
3. 下一阶段重点：推送ADR 017/018触发五目标CI，并在Windows桌面实测新构建（验证无黑框启动、CLI重定向、`\\?\`显示剥离以及ADR 017/018桌面行为）；可编辑同文档多视图在固定`gpui-base`上仍为NO-GO；补齐物理系统输入法候选窗、Windows x64裸机/Linux/macOS Intel桌面GUI、签名公证与安装包交付。
4. NSH/SDK（T12、T16）仍暂缓于`spikes/nsh-eval`，须先通过兼容、IME、主题和焦点门槛，不绑定主编辑路径。平台构建产物不等于签名、公证、安装包或完整交互验收。

## 性能实验原则

ReadAt 为安全基线；对比读取块大小、预读与共享缓存、搜索算法和索引稀疏度。mmap 仅在外部截断风险可隔离的实验中评估，不默认用于可变文件。以真实生成的非稀疏数据测吞吐；稀疏文件只用于偏移正确性。分别记录热缓存和未确认缓存状态，不把重启进程称为冷盘。覆盖 100 标签、多个大文件、长行、密集短行、Unicode、取消和资源释放。目标值与实测值分开。

生成的大文件、测试草稿数据库、临时自动化脚本与验证保活进程已清理；源码、release可执行文件、复现实验CLI及 `docs/evidence` 保留。默认用户数据目录未用于本轮验证。
