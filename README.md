# Native text editor

Rust + GPUI Kit 文本编辑器。产品与验收基线见 [交接文档](Rust_GPUI_Editor_Agent_Handoff_v1.0.md)，当前状态见 [IMPLEMENTATION_STATUS](IMPLEMENTATION_STATUS.md)。默认应用与NSH候选实验隔离；本地原生桌面GUI验收目前已覆盖macOS Apple Silicon（含ADR 017/018）与Windows 11 ARM64（Parallels虚拟机，ADR 013/014构建），其余CI目标的构建与无窗口验证不等于完整桌面支持。

产品定位是接近Notepad++的纯文本编辑器：本体优先保证编辑、保存恢复、大文件、多文件、标签页组及单窗口分屏。项目模型和LSP不作为本体目标；未来如需这些能力，通过插件提供。当前已实现标签页组、不同文档的单窗口分屏、主应用**只读**同文档多视口（`schema v3 / format v3`，覆盖分页大文件与图像预览），以及按交接文档§10.1/§14.4 Kit-only回退路线内置的纯Rust无损JSON/JSONC工具与有界只读光栅图像预览；**可编辑（Editable）同文档多视图仍为NO-GO**，NSH TS/TSX插件运行时（`T12`、`T16`）仍暂缓。

## 构建环境

- Rust 1.99.1（`rust-toolchain.toml`）。
- macOS 完整 Xcode、Command Line Tools、Metal Toolchain。`xcode-select -p` 与 `xcrun -f metal` 应可用。
- 初次 Cargo 构建需要已缓存依赖或联网获取；锁文件固定解析结果。应用基础运行不需要 Node/npm。

```sh
cargo build --locked --release -p editor
cargo run --locked --release -p editor -- --data-dir .smoke-data/session
cargo test --locked --workspace
```

首次依赖获取不是应用启动性能；构建成功也不代表 IME、恢复或保存故障验收通过。

## GitHub与多平台构建

源码：[stabey/makeanotepad](https://github.com/stabey/makeanotepad) · [Native CI](https://github.com/stabey/makeanotepad/actions/workflows/ci.yml)

推送`main`、Pull Request及手动触发均执行锁定依赖的测试和release构建。五个目标分别使用原生托管runner：macOS ARM64、macOS Intel、Windows x64、Windows ARM64、Linux x64；不会把ARM模拟x64的结果记作原生x64验收。

在成功运行的 **Artifacts** 中下载对应目标的ZIP，保留14天。产物含`editor`/`editor.exe`、服务CLI、构建身份、SHA-256、目标相关许可证清单及许可文件。它们是开发构建，不是安装包或正式签名/公证版本；系统运行库仍由目标环境提供。

原子安全保存现支持macOS、Linux与Windows（其余未知平台产物仍会携带`EXPERIMENTAL-CANNOT-SAVE.txt`）。其中macOS ARM64与Windows 11 ARM64（ADR 014构建）已完成真实桌面窗口及保存闭环验收（Windows 11含原生`IFileSaveDialog`另存为与帧缓冲截图）；Wave 0提交`91c301c`（ADR 015/016）已在[Native CI 44015994062](https://github.com/stabey/makeanotepad/actions/runs/44015994062)全部6个job一次通过（包含两个Windows目标的PE GUI/CUI子系统校验，产物可用于用户Windows实机测试），ADR 017/018待下一次推送触发CI。Windows版（ADR 015–018）桌面GUI复验、Windows x64裸机、Linux x64与macOS Intel桌面GUI、全平台真实系统输入法候选窗、签名/公证与安装包仍待交付或人工验收。草稿恢复仍不能替代显式保存文件。

CI在五个原生目标上均运行无需窗口的`--help`、`--version`、100文档惰性会话验证及保存/故障注入服务验证（`services_smoke`）。它不执行GUI smoke，不证明真实IME、鼠标交互、GPU兼容或性能指标。具体状态以运行日志及ZIP中的`BUILD_IDENTITY.txt`、`HEADLESS_SMOKE.json`为准。

## 工程结构

- `apps/editor`：Kit 原生窗口、普通编辑适配层、标签与分页查看。
- `crates/editor-services`：不依赖 GPUI 的文件、分页、搜索、保存和恢复服务。
- `spikes/nsh-eval`：独立 headless / Kit Fast + TSX 候选实验；不属于默认 workspace。
- `docs`：架构决定、测试与性能记录。

## 日常使用

默认显示 **Open Editors / Recent Files** 侧栏；最近文件最多30项，列表过滤不打开文件、不预加载未激活标签。侧栏不是项目目录树。

| macOS 快捷键 | 操作 |
|---|---|
| `Cmd+Shift+P` / `F1` | 命令面板 |
| `Cmd+P` | 已打开/最近文件快速切换；输入 `>` 切换命令模式 |
| `Cmd+B` | 显示/隐藏侧栏 |
| `Cmd+Shift+D` / `Cmd+Shift+K` | 复制行 / 删除行 |
| `Alt+↑` / `Alt+↓` | 上移 / 下移当前行或选中行 |
| `Cmd+Alt+↑` / `Cmd+Alt+↓` | 上下添加光标（Windows `Ctrl+Alt+↑/↓`，Linux `Shift+Alt+↑/↓`；亦可由Edit菜单或命令面板调用） |
| `Alt+Shift+拖动` | 矩形/列块选择（Linux亦可`Ctrl+Alt+拖动`） |
| `Cmd+/` | 支持语言的行注释切换 |
| `Tab` / `Shift+Tab` | 原生缩进 / 取消缩进 |
| `Alt+Z` | 自动折行 |

矩形/列选与`AddCursorAbove`/`AddCursorBelow`垂直多光标支持原生多光标编辑与单次撤销并保持`\r\n`边界完整；自定义复制/删除/移动/注释命令仍只处理**活动选区**，会收敛其他光标（因`gpui-base`未公开完整`selections`集合）；原生缩进保留Kit自身的多光标行为。单次行操作跨度/输出限制4 MiB，超限拒绝而非部分修改。JSON、纯文本、CSS、HTML、XML、Markdown不提供伪行注释。

底部状态栏可选择语言、LF/CRLF、UTF-8/BOM、空格/Tab及2/4/8宽度，并切换折行、空白字符显示。语言和显示选项不修改文件；编码/换行变化会标记已保存文件为未保存，并进入恢复保护。只读/分页/图像文档禁用不适用的写入选项。

依据交接文档§10.1与§14.4的Kit-only回退路线，内置提供纯Rust工具而不启用NSH JS运行时：
- **Tools** 菜单与命令面板支持无损 **JSON / JSONC** 校验、格式化与压缩（`Validate` / `Format` / `Minify`；4 MiB与2048深度上限，原样保留大整数、重复键、`-0`、指数、转义、Unicode及JSONC注释与尾随逗号；格式错误时跳转光标提示，改写仅生成`1`次`Undo`并持久化至SQLite）。
- 自动识别常见光栅图像（`PNG`、`JPEG`、`WebP`、`GIF`、`BMP`、`ICO`、`TIFF`）并经由`32 MiB`文件/`8192 px`/`128`帧/`128 MiB`解码RGBA头闸门后以只读预览打开；支持 **View** 菜单与工具栏缩放（`Fit` / `10%–800%`）及全工作区`256 MiB` LRU缓存回收（显式`cx.drop_image`释放GPU纹理）。**`.svg`仍作为可编辑XML文本打开，不作为光栅图像预览。**

最近文件与显示偏好保存在数据目录的 `ui_shell.json`；会话文本仍由独立SQLite恢复库负责。偏好后台合并写入，可见窗格的编辑器即时应用，不可见标签在变为可见时惰性应用。损坏偏好文件在首次替换前保留独立备份，不因启动而直接覆写。

### 标签页组

使用标签栏的 **+ Group**、原生 **Groups** 菜单或命令面板创建组。组支持名称、颜色、折叠和排序；拖动标签可调整组内顺序或移到其他组，拖动组标题可重排组，`Escape`取消拖动。

| macOS 快捷键 | 操作 |
|---|---|
| `Cmd+Alt+G` / `Cmd+Alt+Shift+G` | 当前标签新建组 / 创建空组 |
| `Cmd+Alt+R` / `Cmd+Alt+C` | 重命名当前组 / 切换组颜色 |
| `Cmd+Alt+M` | 折叠或展开当前组 |
| `Cmd+Alt+[` / `Cmd+Alt+]` | 在组内前移 / 后移当前标签 |
| `Cmd+Alt+Shift+[` / `Cmd+Alt+Shift+]` | 将当前标签移到前一组 / 后一组 |
| `Cmd+Alt+U` | 将当前标签移回未分组 |

折叠只隐藏标签，活动文档仍保持打开并在组标题中提示；侧栏和快速切换仍能访问组内成员。**Remove Group (Keep Tabs Open)**只解除分组；**Close All Tabs in Group…**是独立操作，汇总确认后才关闭，取消或保存失败保留整组标签。关闭确认期间暂时冻结编辑与结构操作，避免保存或关闭作用于已经变化的内容。

组名最多128个UTF-8字节，最多128个自定义组；当前窗口最多1024个文档视图。移动和分组不复制文本，不重置撤销历史。组、顺序、选择与视图位置由同一个SQLite恢复worker持久化，不放入`ui_shell.json`。

**File → Recovery Draft Catalog…**可分页查看持久草稿和未接纳的恢复记录。达到1024个视图上限后，可先保留草稿并关闭一个视图，再打开其他记录；**Export Copy…**不占用新视图、不更改原草稿的保存关联。损坏记录明确标为不可访问，不冒称已经修复。

### 单窗口分屏

使用 **Panes** 菜单、工具栏或窗格菜单向左/右/上/下拆分；最多8个窗格，每个窗格有自己的标签栏和当前文档。普通拆分创建空窗格，**Split & Move Tab…**把当前标签移入新窗格，二者都不会复制文档。打开已经存在的文档会定位到它所在的窗格。

| macOS 快捷键 | 操作 |
|---|---|
| `Cmd+\` / `Cmd+Shift+\` | 向右 / 向下拆分 |
| `Cmd+Alt+\` / `Cmd+Alt+Shift+\` | 向左 / 向上拆分 |
| `Cmd+Alt+Shift+L` / `Cmd+Alt+Shift+J` | 向右 / 向下拆分并克隆只读多视口（Win/Linux `Ctrl+Alt+Shift+L/J`） |
| `F6` / `Shift+F6` | 聚焦下一个 / 上一个窗格 |
| `Cmd+Alt+Shift+N` / `Cmd+Alt+Shift+P` | 将当前标签移到下一个 / 上一个窗格 |
| `Cmd+Alt+=` | 均衡窗格大小 |
| `Cmd+Alt+Z` | 仅显示焦点窗格 / 恢复完整布局 |
| `Cmd+Alt+X` | 关闭当前窗格，并合并标签到相邻窗格 |

拖动分隔线调整尺寸，`Escape`取消此次调整；拖动标签到另一窗格中央可移动，到边缘可拆分并移动，到组标题可重新分组。预览不修改正文；无效目标、容量不足或取消不会留下半完成的移动。

对只读文档（`Paged`分页大文件与`Image`光栅图像预览），还可通过上述拆分克隆快捷键或 **Panes → Duplicate Read-Only View in Pane**（及标签右键/窗格菜单）在同一或不同窗格打开同一文档的独立视口，共享单个底层源与缓存预算、独立保存偏移或缩放，且在仍有次视图存活时关闭次视图免弹窗立即移除；**可编辑（Editable）文档同文档多视图仍为NO-GO**。

**Close Pane (Merge Tabs into Adjacent Pane)**不会关闭文档；**Close All Tabs in Active Pane…**才是需要草稿保护确认的关闭操作。点击正文后，输入、保存、搜索和行操作作用于该窗格的当前文档。窗格树、比例、标签位置和选择会恢复，文本实体与撤销历史不会因移动而复制或重置。

交互拆分保证每个窗格至少240×160逻辑像素；窗口太小时显示 **Compact** 提示并暂时只显示焦点窗格，放大后恢复原布局，不删除隐藏窗格。所有窗格共享原有内容、页缓存和索引预算；恢复先加载焦点文档，再逐个加载其他可见文档，预算不足的标签保留并显示原因。

## 数据安全

普通文档的修改与用户文件保存分开：草稿恢复成功不能清除 dirty。恢复文档不自动覆盖磁盘来源。只读分页文件与图像文件不会全文复制进恢复库。

使用 `--data-dir` 可将验证数据与个人会话隔离。卸载二进制不应自动删除恢复数据；手动删除数据目录前先导出需要的未保存草稿。生成的大文件和临时测试会话不提交到版本控制。

旧会话可原子迁移到SQLite `schema v3`与`layout format v3`（原子重建`layout_views`表移除`UNIQUE(view_set, doc)`约束以支持只读同文档多视口）；`v1`/`v2`布局在内存中升级，正常布局写入时才产生新快照。升级前请关闭旧实例并备份整个数据目录；旧版不能读取新布局，保留旧快照不等于支持直接降级。未知未来布局格式与字段在只读预检中拒绝写入；已知格式的损坏布局保留原始记录，再尝试最近有效布局和元数据恢复。

不传 `--data-dir` 时，macOS恢复目录为 `~/Library/Application Support/com.makeaedittool.editor`，数据库文件为 `recovery.sqlite3`。不要将它放到网络共享目录或同步盘。普通编辑器合计内容准入默认256 MiB、最多128个已hydrate编辑器，工作区已解码图像缓冲共享256 MiB LRU上限；这些不是RSS硬上限，撤销历史和布局缓存另占内存。单文件超过64 MiB默认只读分页，不冒充任意大文件可编辑。

原生自动化验证使用隔离目录，报告在正常退出与恢复shutdown确认后写出：

```sh
target/release/editor --data-dir .smoke-data/native-full --smoke=full --smoke-report .smoke-data/native-full.json
```

IME结算、原子撤销及实际保存/关闭/重启恢复另有macOS原生场景；请使用全新隔离目录，已有的`smoke_ime_artifacts`不会被接管或删除：

```sh
target/release/editor --data-dir .smoke-data/ime-check --smoke=ime --smoke-report .smoke-data/ime-check.json
```

本地维护`gpui-base 0.9.0`的**六个源文件**（`state.rs`、`undo_manager.rs`、`element.rs`、`display_map.rs`、`wrap_map.rs`、`text_wrapper.rs`）：涵盖IME结算/历史发布、普通文件Rope接管与顺序行布局、256上限`WindowTextSystem`缓存轮换、视口优先渐进折行与CRLF多光标边界修复；依赖版本和应用层提交屏障不变，新增仅用于新建EditorMode的Rope接管构造器。来源和维护边界见[vendor notice](vendor/gpui-base/NOTICE-IME-PATCH.json)与ADR011–ADR013、ADR016。该补丁不提供共享可编辑多视图，也不等于真实系统输入法候选窗口已验收。

同机ADR016配对release对照（67,108,860 B，warm/unspecified `/tmp`）中：缓存不利的1,048,576行全唯一短行（`short_unique`）首帧就绪时间由7.10秒（7,104.93 ms）降至**344.53 ms**、首帧peak RSS由2324.72 MiB降至**355.93 MiB**（首帧后待折行约1,043,450行），开启`EDITOR_SMOKE_AWAIT_FULL_WRAP=1`完成100%精确折行收敛后全过程peak RSS降至**464.97 MiB均值 / 500.94 MiB中位数**（后台收敛均值**9.28秒**）；常规重复短行首帧就绪由422.11降至321.12 ms。合入ADR 017/018后的最新macOS ARM64本地全量验证通过：工作区**195**个测试（`82 + 27 + 86`）、9个CI打包测试、27个私有行布局测试、11个私有IME历史测试、1个CRLF列选边界测试、`editor-services smoke` 6场景以及8次原生macOS窗口回归（含`usability` + Restart A与`splits` + Restart B），同时Wave 0提交`91c301c`已通过[Native CI 44015994062](https://github.com/stabey/makeanotepad/actions/runs/44015994062)全部6个job；不承诺任意文本形态、冷盘或未测平台达到相同数字，测量边界与原始证据见[性能记录](docs/PERFORMANCE_REPORT.md)、[测试记录](docs/TEST_REPORT.md)与[ADR 017/018回归报告](docs/evidence/adr017-018-native-regressions.json)。

## 候选运行时

```sh
cargo run --locked --release --manifest-path spikes/nsh-eval/Cargo.toml --bin nsh-eval
cargo run --locked --release --manifest-path spikes/nsh-eval/Cargo.toml --features panel --bin nsh-panel-eval
```

结果与上游兼容性问题见 [NSH_EVALUATION](docs/NSH_EVALUATION.md)。默认编辑器不因候选失败改用未验证依赖。

## 同文档多视图实验

这不是主应用功能开关。两个隔离工作区用于验证现有组件的边界，不修改个人会话或生产恢复库；当前运行证据仅来自macOS ARM。

```sh
CARGO_TARGET_DIR="$PWD/target" cargo run --locked --release --manifest-path spikes/shared-text-feasibility/Cargo.toml --bin shared-text-feasibility -- --report /tmp/shared-text-feasibility.json
CARGO_TARGET_DIR="$PWD/target" cargo run --locked --release --manifest-path spikes/multiview-paged/Cargo.toml --bin multiview-paged -- --views 8 --report /tmp/multiview-paged.json
```

普通编辑器诊断的退出码`2`表示实验完成但发现安全断言失败；受维护补丁后的实际运行为exit `0`，66阶段/0失败断言（含此前失败的6项）。旧`source_prediction`仅记录未修改版本的预测，不是补丁后的验收条件。分页实验以一个真实文件源挂载`1/2/4/8`个独立虚拟视口（`--views 8` 66帧、`total_elapsed_ms = 814.08 ms`、14/14校验标志通过且关闭至零后缓存/索引归零）；大于4 GiB的稀疏夹具仅验证偏移正确性，不能作为吞吐成绩。主应用目前已通过`schema v3 / layout format v3`启用**只读**同文档多视口（`Paged`与`Image`），而**可编辑（Editable）同文档多视图仍为NO-GO**；此外，真实系统IME候选窗、Windows（ADR 015–018）及裸机Win x64/Linux/Mac Intel桌面GUI、签名公证/安装包、LSP（`T14`）及NSH/TSX扩展运行时（`T12`、`T16`）仍未交付或未验收，详见[架构决策](docs/ARCHITECTURE_DECISIONS.md)和[测试记录](docs/TEST_REPORT.md)。

## 证据

- [架构决策](docs/ARCHITECTURE_DECISIONS.md)
- [测试记录](docs/TEST_REPORT.md)
- [性能记录](docs/PERFORMANCE_REPORT.md)
- [已知限制](docs/KNOWN_LIMITATIONS.md)
- [主应用依赖许可证清单](docs/DEPENDENCY_LICENSES.csv)是本地macOS解析快照；[隔离NSH候选清单](docs/NSH_DEPENDENCY_LICENSES.csv)仅对应实验。它们包含开发依赖，不等于实际链接清单。每个CI产物另生成目标过滤后的`DEPENDENCY_LICENSES.csv`及`licenses/manifest.json`，并附可获取的上游许可与资源声明。
- 内置资源另有许可证：[Lucide/Feather图标](docs/licenses/gpui-kit-assets-0.9.0-LICENSE-LUCIDE)、[two-face 0.4.5语法与主题声明](docs/licenses/two-face-0.4.5-acknowledgements.md)。后者保留固定上游版本的完整声明，不能只用crate级MIT/Apache标签替代资源许可。

所有尚未实际运行的能力均按未验证处理，不将交接文档目标或上游组件能力当作本项目成绩。
