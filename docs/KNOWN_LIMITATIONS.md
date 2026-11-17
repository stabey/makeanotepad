# 当前限制

以实际运行记录为准；交接文档的目标不是通过声明。

## 平台与分发

- 本地原生桌面GUI验收环境目前已覆盖macOS Apple Silicon（含ADR 017/018）与Windows 11 ARM64（Parallels Desktop虚拟机，WDDM显卡2.0x缩放；其中Windows桌面实跑与帧缓冲截图仅基于ADR 013/014构建）。Wave 0提交`91c301c`已通过[GitHub Actions Native CI 44015994062](https://github.com/stabey/makeanotepad/actions/runs/44015994062)全部6个job（含Windows PE GUI/CUI子系统门禁，已可用于用户Windows实机测试），但ADR 015–018重建产物的Windows桌面GUI复验尚未执行，ADR 017/018尚待下一次推送触发CI。Windows x64裸机、Linux x64及macOS Intel仍仅具CI编译、工作区测试与无窗口`services_smoke`证据，不等于完整桌面GUI或性能基线。
- 尚无签名、公证、安装包或自动更新交付。macOS源码构建需要Rust及完整Xcode/Metal；Windows/Linux构建依赖见CI平台准备脚本，开发ZIP不自动安装系统运行库。Windows版`editor.exe`已在代码、打包校验与Wave 0 CI产物中完成ADR 015改造（启用`windows_subsystem = "windows"`并以管道安全`AttachConsole(ATTACH_PARENT_PROCESS)`保留已有标准句柄，`package_artifact.py`强制校验`editor.exe` PE GUI子系统`0x0002`与`editor-services.exe` CUI子系统`0x0003`，同时在状态栏、Quick Switcher、`"Saved {}"`提示与恢复目录接入Fail-closed `\\?\`路径展示剥离而不改写规范`PathBuf`）；独立启动无黑框与界面路径展示的Windows桌面实机复验仍待实际运行确认。
- 原子安全保存支持macOS、Linux与Windows（其余未知目标显式返回`Unsupported`）。Windows 11 ARM64桌面已实测`Ctrl+Shift+S`调起原生COM `IFileSaveDialog`完成NTFS保存闭环，但尚未穷尽网络盘/UNC/UAC受限目录等所有系统文件对话框边界，真实中文输入法候选窗仍未验收，仍是未签名开发构建而非正式发行版。
- NSH保留独立实验，不进入默认编辑器；真实IME、完整焦点/DPI/主题同步、正式SDK（`T12`）、离线TS/TSX扩展包（`T16`）与LSP（`T14`）门槛未通过或未交付。JSON/JSONC工具（`T13`）与光栅图像预览（`T15`）已依据交接文档§10.1与§14.4 Kit-only回退路线在主应用以纯Rust内置实现，但不宣称交付JS插件运行时或`T12`/`T16`。

## 文件与预算

- 默认普通可编辑文件上限64 MiB；超过阈值以及超长行等输入走只读分页。不是任意大文件编辑器；大文件原位替换与写回不提供。
- 普通编辑器合计内容准入256 MiB、最多128个已hydrate实体；仅未修改且无撤销历史的文件标签可被回收。内容预算不是RSS硬上限，撤销与排版缓存额外占用。
- UTF-8、UTF-8 BOM、统一LF/CRLF为本轮保真范围；混合换行、二进制或不支持编码不按可编辑文本静默改写。
- 分页视口64 KiB、共享页缓存64 MiB、共享稀疏索引预算16 MiB。长行分段显示，不先整行shaping；稀疏索引未覆盖处行号可能未知。
- 普通正则/大小写折叠搜索限16 MiB；结果上限10,000，达到上限明确标记截断。分页仅支持流式字面量，不支持任意跨文件正则。
- 原子多编辑：同内容多位置走原生多光标；不同替换内容的跨度与输出受4 MiB预算约束，超限拒绝且不部分修改。
- 内置JSON/JSONC工具（Validate/Format/Minify）：仅作用于可编辑文本槽位（只读/分页/图像视图不改写正文），受`MAX_JSON_TOOL_BYTES = 4 MiB`与`MAX_JSON_DEPTH = 2048`约束，超限直接拒绝。
- 只读光栅图像预览：仅按魔数与有界头探测支持`PNG`、`JPEG`、`WebP`、`GIF`、`BMP`、`ICO`、`TIFF`，单图受`32 MiB`文件、`8192 px`宽高、`128`帧与`128 MiB`估算解码RGBA约束；超限在读全图前进入`OverBudget`，损坏图像头安全回退为`Paged`查看器。工作区已解码图像缓冲共享`256 MiB` LRU上限（非可见标签按激活顺序驱逐并以`cx.drop_image`释放）。**`.svg`继续作为可编辑XML文本打开，不渲染为光栅图像预览。**

## 日常编辑界面

- 侧栏目前管理打开文档和最近文件，不是项目目录树；跨目录搜索未实现。
- 已实现标签组、最多8个不同文档窗格，以及**只读文档（`Paged`与`Image`）同文档多视口**（共享单源与缓存预算，独立偏移或缩放，次视图无弹窗快速关闭）；**可编辑（`Editable`）同文档多视图仍为NO-GO（ADR 009）**，重启或重载若遇到`Editable`/已有checkpoint草稿的重复视图会自动折叠为单视图。上限仍为128个自定义组加未分组、1024个视图、128个已hydrate编辑器，不按窗格复制预算。窗口空间不足会暂时只显示焦点窗格，逻辑布局保留。
- 矩形/列选（`Alt+Shift`拖拽；Linux亦可`Ctrl+Alt`）与垂直多光标（`AddCursorAbove`/`AddCursorBelow`，macOS为`Cmd+Alt+↑/↓`、Windows为`Ctrl+Alt+↑/↓`、Linux为`Shift+Alt+↑/↓`，亦可经Edit菜单与命令面板调用）已修复CRLF边界（`cursor_boundary(..., Bias::Left)`），并通过原生`--smoke=usability`及私有单测；但自定义复制/删除/移动/注释（`DuplicateLine`/`DeleteLine`/`MoveLineUp`/`MoveLineDown`/`ToggleComment`）仍只操作活动选区并收拢其他光标（因`gpui-base`未公开完整`selections`集合），不将这些自定义行命令宣传为多光标批量编辑。
- 行命令最多50,000行、4 MiB跨度/输出；只提供已知`//`、`#`、`--`行注释，不为JSON等格式插入非法注释。
- 视图偏好写入独立于草稿恢复。已有偏好flush尚未结束时的后续写入错误，可能到下一次flush或退出才显示；错误保持可见于持久化屏障，不影响正文恢复确认。
- 此前按应用窗口捕获失败：`screencapture -l`返回无法创建图像，ScreenCaptureKit返回`-3811`，可枚举显示器数为0；这些观察不足以确定物理显示器或锁屏根因。本轮未补像素级呈现验收。真实AppKit窗口内程序化smoke不能替代物理鼠标、系统Option键、真实IME与屏幕端到端验收；外部SIGTERM仅用于清理，不是应用正常退出确认。
- 未修改Kit 0.9.0的unmark、空标记取消和IME Undo/Redo事件边界缺陷已有有界补丁；原始6个失败断言、实际工作区保存/关闭及SQLite重启恢复已通过。受维护`vendor/gpui-base`原生源文件范围现为**6个文件**（`state.rs`、`undo_manager.rs`、`element.rs`、`display_map.rs`、`wrap_map.rs`、`text_wrapper.rs`，统一记录于`vendor/gpui-base/NOTICE-IME-PATCH.json`），覆盖IME历史/结算、Rope接管与顺序行布局、256上限`WindowTextSystem`缓存轮换、视口优先渐进折行与CRLF多光标边界修复，未修改应用层提交/身份屏障。真实系统输入法候选窗口、重转换及平台回调顺序仍未验收，不能从合成原生回调外推。

## 恢复与保存

- SQLite草稿恢复不等于用户文件已保存；恢复确认不能清除dirty。
- 单草稿超过64 MiB后恢复保护降级，需显式另存抢救；退出必须提示，不能宣称新修改已自动保护。
- 损坏单条元数据不阻断其他草稿；原始记录保留。恢复目录可分页访问超出1024视图容量的记录，显示不可读条目；不可读数据不保证能打开或导出，不以空文本伪装恢复成功。
- 升级前关闭旧实例并备份完整数据目录；不能直接用旧版本降级打开。现已原子迁移至SQLite `schema v3`（重建`layout_views`移除`UNIQUE(view_set, doc)`约束，支持只读同文档多视口）与`layout format v3`；`v1`/`v2`布局在内存升级，正常写入后保存新快照。未知未来布局或字段在只读预检中拒绝写入，已知格式的损坏布局保留原始快照并回退。次视图存活时拒绝`Discard`。当前视图容量仍为1024，恢复目录不等于同时加载全部草稿。
- macOS、Linux与Windows安全保存均拒绝硬链接目标与只读目标，解析现有符号链接（悬空符号链接显式失败），区分已发布与完全同步结果。Linux替换仅保留POSIX权限位（`0o7777`），不保留ACL、xattr或非所属组owner；Windows通过`FlushFileBuffers`与`MoveFileExW(MOVEFILE_WRITE_THROUGH)`发布并保留基础文件属性（`HIDDEN`/`SYSTEM`/`ARCHIVE`/`NOT_CONTENT_INDEXED`），不支持非特权父目录刷盘、不跨替换保留NTFS替代数据流（ADS）或自定义DACL，外部无`FILE_SHARE_DELETE`占用会拒绝替换并保留原文件与草稿；FAT/exFAT等无稳定文件ID卷在`Prepared`崩溃对账时保守失败闭合。外部写入检查与发布之间仍存在竞争窗口。
- 进程故障注入、事务测试及F_FULLFSYNC不等于真实断电验证；没有断电实验结论。

## 测量边界

- 既有大文件/服务实验数据位于外置USB SSD，不是内置NVMe；本轮本地构建IME及纯打开对照夹具位于独立`/tmp`目录，CI产物再验收使用独立`/Volumes/macport/Caches/tmp`目录。均未清除OS缓存，标为warm/unspecified。服务读取延迟不是GUI首屏，输入提交耗时不是屏幕端到端延迟。
- 64 GiB完整文件扫描、16文件共享缓存、100草稿惰性恢复已有服务层证据；原生主应用的最终运行状态见[测试报告](TEST_REPORT.md)与[ADR 017/018回归报告](evidence/adr017-018-native-regressions.json)，不从服务测试外推。
- Unicode键盘事件注入不替代真实中文输入法预编辑、候选窗口及组合提交验收。
- 普通文件内存与加载延迟在缓存不利形态下已大幅收敛但仍有清晰边界。ADR 016同夹具macOS ARM64配对release对照（67,108,860 B，warm/unspecified `/tmp`）中：1,048,576行全唯一短行（`short_unique`）在默认`--smoke=startup`下的首帧就绪时间由7.10秒（7,104.93 ms）降至**344.53 ms**、首帧peak RSS由2324.72 MiB降至**355.93 MiB**（首帧后待折行约1,043,450行）；在`EDITOR_SMOKE_AWAIT_FULL_WRAP=1`驱动100%精确折行收敛后，总后台收敛时间仍为**9.28秒**（9,283.03 ms均值，因未近似字形宽度），但全程peak RSS降至**464.97 MiB均值 / 500.94 MiB中位数**（最大515.31 MiB）。ADR 017/018合入后未重跑该64 MiB折行配对矩阵，亦未采集Windows/Linux桌面新性能样本；后台未证实完成draw的记录不用于稳定内存结论；本轮仍无稳定绘制后的长期多文档堆或泄漏结论，本机ScreenCaptureKit枚举显示器为0（错误码`-3811`），外部窗口像素截图仍未验证。证据见[性能报告](PERFORMANCE_REPORT.md)与[测试报告](TEST_REPORT.md)。
