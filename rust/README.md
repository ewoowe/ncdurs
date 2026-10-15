# ncdurs

`ncdurs` 是 [ncdu](https://dev.yorhel.nl/ncdu)（C 版）的 Rust 重实现，目标是跨平台的磁盘占用浏览工具。

代码位于本仓库的 `rust/` 子目录，与 C 版源码、Autotools 构建完全独立：`cargo` 只针对 `rust/`，C 版仍用 `./configure && make`，两者互不影响。

> 当前状态：**功能已完整** —— 手册的选项表 59 项与九种条目状态标记全部实现，
> 界面与数值均与 C 版对齐，扫描性能同量级（`/usr`、50 万条目上约 1.09 倍）。
>
> **支持**：目录扫描（进度页、可中止）、交互浏览（五列 + 四个排序列 + 目录优先 +
> 自然序 + 隐藏过滤）、删除 / 重扫 / 开 shell、与 C 版格式互通的导出导入、排除规则
> （模式 / 缓存标记 / 文件系统边界 / Linux 伪文件系统）、扩展信息（`-e`）、配置文件、
> 四档颜色主题（输出用与 ncurses 相同的经典 8 色码）。
>
> **验证**：`scripts/compare.sh` 的 16 项 C 版对照断言 + 33 个单元测试；CI 在
> Linux / macOS / Windows 三平台跑 fmt、clippy（`-D warnings`）、测试与 MSRV 检查。
>
> **待办**：仅剩可选项，见「[已知限制与待实现](#已知限制与待实现)」。

## 目录

- [构建与运行](#构建与运行)
- [用法](#用法)
- [与原版 ncdu 的对比](#与原版-ncdu-的对比)
- [跨平台](#跨平台)
- [项目结构](#项目结构)
- [测试](#测试)
- [已知限制与待实现](#已知限制与待实现)
- [许可](#许可)

## 构建与运行

需要 Rust 1.88 或更高版本（crate 使用 edition 2024，并用到 edition 2024 稳定下来的 let-chains；开发环境为 1.96）。

```bash
cd rust
cargo build --release
./target/release/ncdurs /path/to/dir
```

Windows（PowerShell）：

```powershell
cd rust
cargo build --release
.\target\release\ncdurs.exe C:\Users
```

## 用法

```text
ncdurs [OPTIONS] [PATH]

-a, --apparent-size   显示表观大小（apparent size）而不是磁盘占用
-o, --export FILE     扫描并把结果以 JSON 写入 FILE（- 表示标准输出）
-f, --import FILE     读取用 -o 生成的文件并浏览（- 表示标准输入）
    --color SCHEME    颜色主题：auto（默认）/ off / dark / dark-bg
    --exclude PATTERN 匹配的条目不计入统计（可重复指定）
-X, --exclude-from FILE 从文件读取排除模式，每行一个
-x, --one-file-system 不跨文件系统
    --exclude-caches  跳过含 CACHEDIR.TAG 的目录
-e, --extended       收集 uid/gid/mode/mtime（启用 mtime 列与 m/M 键）
-h, --help            显示帮助
-V, --version         显示版本
```

`PATH` 缺省为当前目录。路径不存在或不是目录时报错退出（退出码 1）。`-f` 与路径互斥，`-f` 加 `-o` 可组合做格式转换；`-o` 在标准输出被重定向（管道/文件）时不启动界面，可直接用于脚本。

导出格式与 C 版双向互通：

```bash
ncdurs -o - /usr | ncdu -f -        # ncdurs 扫描，C 版浏览
ncdu -o - /usr | ncdurs -f -        # C 版扫描，ncdurs 浏览
ncdurs -f old.json -o new.json      # 导入后再导出
```

### 按键

| 按键 | 作用 |
| --- | --- |
| `↑` / `k`、`↓` / `j` | 上下移动光标 |
| `PgUp` / `PgDn` | 翻页 |
| `g` / `G`（或 Home/End） | 跳到首项 / 末项 |
| `→` / `l` / `Enter` | 进入选中的目录 |
| `←` / `u` / `Backspace` | 返回上级目录 |
| `n` / `s` / `C` | 按名称 / 大小 / items 排序（再按一次反向） |
| `a` | 在磁盘占用 / 表观大小之间切换 |
| `d` | 删除选中项（弹出 `y`/`n` 确认） |
| `r` | 重扫当前目录 |
| `b` | 在当前目录打开 shell（`NCDU_SHELL` → `$SHELL`），退出后回到界面 |
| `q` / `Esc` / `Ctrl-C` | 退出 |

扫描期间会显示进度页（当前路径、已扫条目数与已扫大小），此时按 `q` 或 `Esc` 可随时中止扫描。

### 两种大小口径

- **磁盘占用（Disk usage）**：`st_blocks * 512`，文件实际占用的磁盘空间（按文件系统块分配）。**默认口径**，与 `du` 默认行为一致。想知道"删掉它能腾出多少空间"看这个。
- **表观大小（Apparent size）**：`st_size`，文件内容的逻辑长度，即 `ls -l` 显示的大小。

两者差异来源：块对齐（1 字节文件也占 4 KiB）、稀疏文件（apparent 1 GiB / disk 0 B）、压缩文件系统（disk 可能小于 apparent）等。按 `a` 键或启动时加 `-a` 切换，底部汇总行会同时列出两种口径。

## 与原版 ncdu 的对比

下表的基准是原版 **ncdu 1.22**（仓库里的 C 实现）。

下表以原版的 `ncdu.1` 手册与 `src/help.c` 为准逐项核对（「部分」= 能力等价但接口/平台有差异）。整体进度：约 59 项**全部实现** —— 原版 `ncdu.1` 手册里的选项与按键都已可用（其中 macOS 专有的 `--exclude-firmlinks` 只在 macOS 上产生实际效果，其他平台上是无操作）。

### 数值一致性

- 对同一目录，`Disk usage` / `Apparent size` / `Items` 三项与 `./ncdu` 完全一致（已在含硬链接的测试目录上对照验证）：

  ```text
  C  版 ./ncdu ：Total disk usage:  10.0 MiB   Apparent size:  10.0 MiB   Items: 5
  ncdurs      ：Disk usage: 10.0 MiB          Apparent size: 10.0 MiB   Items: 5
  ```

- 硬链接只计一次；符号链接不跟随；读不了/被拒绝的条目显示为 `[error]` 并继续扫描（均在扫描结束时汇总提示）。
- 尺寸口径、目录自身块计入父目录等语义与 C 版 `src/dir_scan.c` / `src/dir_mem.c` 保持一致。

### 硬链接记账方式（与 C 版逐字节对齐）

同一个 inode 的多个硬链接：**每条链接都保留自己的大小** —— 列表里各显示各的，导出条目也各带各的 `asize`/`dsize` 与 `ino`/`hlnkc`/`nlink`；而**总量只按一次计入**，重复的那条不计入父目录的累加（`Node.counted`），界面在它旁边标 `H`。

这与 C 版的机制一致：去重不是靠"把重复链接的大小写成 0"，而是靠 `ino`/`hlnkc`/`nlink` 让读取方知道该计几次。跨目录硬链接、`H` 标记，以及「C 版导出 → ncdurs 导入 → 再导出」的逐条保真都已对齐；实测见 `scripts/compare.sh` 的「跨目录硬链接」一段。

我们的导出文件与 C 版**逐字节一致**：同一目录下 `ncdurs -o` 与 `ncdu -o -` 的输出，去掉 `progname`/`progver`/`timestamp` 与 `dev` 后 diff 为空（`dev` 是我们有意不保存的字段）。符号链接等「非普通文件」的 `notreg`、`read_error`、`excluded` 的三种取值（`pattern`/`otherfs`/`kernfs`）、硬链接的 `ino`/`hlnkc`/`nlink`、扩展信息（`-e` 的 `uid`/`gid`/`mode`/`mtime`）、字符串转义、零值字段省略都已对齐（验证方法见「测试」一节）。

### 命令行选项

| 选项（原版） | ncdurs | 备注 |
| --- | --- | --- |
| `-h, --help` | 已实现 | |
| `-v, -V, --version` | 已实现 | `-v` 与 `-V` / `--version` 是同义词（`main.c:434`） |
| `-f FILE`（导入扫描结果） | 已实现 | 与 C 版 JSON 互通；`-` 表示 stdin；导入后禁用删除/刷新/shell |
| `-o FILE`（导出为 JSON） | 已实现 | 与 C 版 JSON 互通；`-` 表示 stdout，重定向时不启界面 |
| `-e, --extended, --no-extended` | 已实现 | 保存 uid/gid/mode/mtime，导出字段与 C 版逐字节一致；信息框改用 `Mode`/`UID`/`GID`/`Last modified` |
| `--ignore-config` | 已实现 | 跳过下面两个配置文件 |
| `-x, --one-file-system` / `--cross-file-system` | 已实现 | 目录标 `>`，不深入且不计入 |
| `--exclude PATTERN` / `-X, --exclude-from FILE` | 已实现 | 匹配整条路径与它的每个后缀（`fnmatch` 语义，`*` 跨 `/`）；命中的目录标 `<` 且不深入 |
| `--exclude-caches` / `--include-caches` | 已实现 | 识别 `CACHEDIR.TAG` 的官方签名 |
| `-L, --follow-symlinks` / `--no-follow-symlinks` | 已实现 | 指向**文件**的链接按目标计数（且不再是 `@`）；指向**目录**的链接永不跟随 —— 与 C 版逐字节一致 |
| `--exclude-kernfs` / `--include-kernfs` | 已实现 | Linux：用 `statfs` 的 `f_type` 比对 12 个伪文件系统 magic（proc/sys/cgroup/cgroup2/debug/devpts/binfmt/bpf/pstore/security/selinux/trace）；不入内、不计入、标 `^`，导出 `"excluded":"kernfs"`（实测与 C 版逐字段一致） |
| `--exclude-firmlinks` / `--follow-firmlinks`（macOS） | 已实现 | 用 `getattrlist(ATTR_CMNEXT_NOFIRMLINKPATH)` 返回的路径与传入路径比较来判断；命中时不进入、标 `F`，但**保留自身大小**（`dir_scan.c:133` 的归零列表不含 `FF_FRMLNK`）；默认**跟随**（`main.c:45`），所以只有显式开启才出现 `F` |
| `-0` / `-1` / `-2`（UI 级别） | 已实现 | `-0` 扫描期间完全不绘制（实测：默认输出 5161 字节 vs `-0` 的 36 字节）；`-1` 输出纯文本进度行到 **stderr**（路径 + 文件数 + 累计尺寸，`\r` 覆盖同一行，所以 stdout 仍是干净的 JSON），`-2` 走全屏界面 |
| `-q, --slow-ui-updates` / `--fast-ui-updates` | 已实现 | 刷新间隔 2 s ↔ 50 ms |
| `--enable-shell` / `--disable-shell` | 已实现 | 门控 `b` 键，禁用时弹 `Shell feature disabled.` |
| `--enable-delete` / `--disable-delete` | 已实现 | 门控 `d` 键，禁用时弹 `Deletion feature disabled.` |
| `--enable-refresh` / `--disable-refresh` | 已实现 | 门控 `r` 键，禁用时弹 `Directory refresh feature disabled.` |
| `-r` / `-rr`（只读） | 已实现 | `-r` = `--disable-delete`；`-rr` 再加 `--disable-shell`（同手册）；导入的树本就只读 |
| `--si` / `--no-si` | 已实现 | 十进制（kB/MB）与二进制（KiB/MiB）单位切换 |
| `-a, --apparent-size` / `--disk-usage` | 已实现 | 两个方向都有显式开关（默认磁盘占用） |
| `--show-hidden` / `--hide-hidden` | 已实现 | 隐藏点文件、`~` 结尾的备份文件与被排除项（`e` 键同）；只影响列表，不影响尺寸 |
| `--show-itemcount` / `--hide-itemcount` | 已实现 | 7 列宽的 itemcount 列，`c` 键同；格式照 `browse_draw_items`（千位分隔 / k / M / `> 1B`） |
| `--show-mtime` / `--hide-mtime` | 已实现 | 依赖 `-e`；`m` 键同样可切换 |
| `--show-graph` / `--hide-graph` | 已实现 | 与 `--show-percent` 共同构成 0..3 的 `graph` 值（bit 0 = 条形、bit 1 = 百分比），`g` 键循环 |
| `--show-percent` / `--hide-percent` | 已实现 | 百分比的基准是**当前目录总量**，与条形的"列表最大值"基准不同（同 C 版） |
| `--graph-style hash\|half-block\|eighth-block` | 已实现 | 字符表逐字照抄 `browser.c:139`（hash `#` / half-block `▌█` / eighth-block `▏▎▍▌▋▊▉█`）；另外也接受原版那个拼错的 `eigth-block` |
| `--sort COLUMN` | 已实现 | 取值 `disk-usage`（默认）/ `name` / `apparent-size` / `itemcount` / `mtime`，可加 `-asc` / `-desc` 后缀；`apparent-size` 同时切换大小口径 |
| `--enable-natsort` / `--disable-natsort` | 已实现 | 关闭后退化为逐字节比较（自然序照 `deps/strnatcmp.c`，默认开启） |
| `--group-directories-first` / `--no-group-directories-first` | 已实现 | 与 `t` 键等价 |
| `--confirm-quit` / `--no-confirm-quit` | 已实现 | 弹出 `ncdu confirm quit` 窗口（`Really quit? (y/N)`），照 `src/quit.c` |
| `--confirm-delete` / `--no-confirm-delete` | 已实现 | 控制删除确认（含"don't ask me again"），与 `d` 键同一状态 |
| `--color off\|dark\|dark-bg` | 已实现 | 三套主题逐槽照抄 `src/util.h` 的 `UI_COLORS`；另加原版没有的 `auto` 并作为默认（`NO_COLOR` 已设置、或 `TERM` 为空 / `dumb` 时回退 `off`，否则用 `dark`） |

### 界面按键

| 按键（原版） | 功能 | ncdurs |
| --- | --- | --- |
| `↑`/`k`、`↓`/`j` | 移动光标 | 已实现（另有 PgUp / PgDn、g / G、Home / End） |
| `→`/`Enter` | 进入目录 | 已实现（另有 `l`） |
| `←`/`<`/`h` | 返回上级 | 部分：`←` / `u` / `Backspace`（原版用 `<` / `h`） |
| `n` / `s` / `C` | 按名称 / 大小 / items 排序 | 已实现（同键再按一次反向） |
| `M` | 按 mtime 排序 | 已实现（`-e` 下可用，比较链 `mtime → size → name → items`） |
| `t` | 排序时目录优先 | 已实现（判断在降序反转之前，所以升序降序都是目录在前） |
| `g` | 显示百分比 / 图形 | 已实现（0..3 循环：无 / 条形 / 百分比 / 两者） |
| `c` | 显示 itemcount 列 | 已实现 |
| `m` | 显示 mtime 列（需 `-e`） | 已实现 |
| `e` | 显示 / 隐藏隐藏项 | 已实现（点文件、`~` 备份、被排除项；不影响尺寸） |
| `a` | 切换表观大小 / 磁盘占用 | 已实现 |
| `i` | 显示选中项信息 | 已实现（`-e` 时含 `Mode`/`UID`/`GID`/`Last modified`） |
| `r` | 重算当前目录 | 已实现（重扫后替换该目录的子树） |
| `b` | 在当前目录开 shell | 已实现（`NCDU_SHELL` → `$SHELL` → `/bin/sh`，经 shell 解析，可带参数） |
| `d` | 删除选中项 | 已实现（先弹 `y`/`n` 确认；目录按递归删除） |
| `i` | 显示选中项信息 | 已实现（`Name`/`Path`/`Type`/`Disk usage`/`Apparent size`，再按 `i` 关闭） |
| `?` | 帮助页 | 已实现（三页 `1:Keys` / `2:Format` / `3:About`，方向键与 PgUp / PgDn 滚动） |
| `q` | 退出 | 已实现（另有 `Esc` / `Ctrl-C`） |
| `q`（扫描期间） | 中止扫描 | 已实现 |

### 其他特性

| 特性 | 原版 | ncdurs |
| --- | --- | --- |
| 扫描进度页（当前路径 / 条目数 / 已扫大小 / 可中止） | 支持 | 已实现 |
| 硬链接去重 | 支持（并标记 `H`） | 已实现：Unix 用 `st_ino`/`st_nlink`，Windows 用 `GetFileInformationByHandle` 取文件索引与链接数（每个条目一次句柄，结果复用） |
| 条目状态标记（`!` `.` `<` `e` `>` `@` `^` `H` `F`） | 9 种，显示在名称前 | **九种全部实现**：`!`（自己读取失败）、`.`（**后代**中有目录读取失败，尺寸可能不准）、`<`（被排除）、`>`（其它文件系统）、`^`（Linux 伪文件系统）、`H`（同一文件已计过，即重复的硬链接）、`F`（firmlink，仅 macOS）、`e`（空目录）、`@`（非普通文件） |
| 配置文件（`/etc/ncdu.conf`、`$XDG_CONFIG_HOME/ncdu/config` 或 `~/.config/ncdu/config`） | 支持 | 已实现：每行一个选项（可含值，如 `--exclude .git`）、`#` 注释、空行忽略、行首 `@` 表示该行允许文件不存在（手册的例子 `@--exclude-from ~/.ncduexcludes`）；**先系统级后用户级**，且**命令行参数优先于配置**。仓库里带了一份示例：`ncdu.conf.example`（全部注释掉，直接拷过去不改变任何行为）。**这份文件是本实现额外提供的** —— 原版没有示例文件，它只在 `ncdu.1` 的 CONFIGURATION FILE 段里内联了几行例子 |
| 环境变量 `NCDU_SHELL`、`NCDU_LEVEL` | 支持 | 已实现（`b` 使用；`NCDU_LEVEL` 每次会话递增一次） |
| 导出 / 导入 JSON | 支持 | 已实现（与 C 版双向互通，见「用法」） |
| 显示列（图 / 百分比 / itemcount / mtime） | 可自由开关 | 已实现（`--show-*` 开关与 `c`/`m`/`g` 键） |
| 颜色主题 | 3 种（off / dark / dark-bg） | 已实现（`--color`，默认 `auto`：终端支持彩色就用 `dark`，否则回退 `off`；数值/目录/图形/标记各按其色槽着色） |
| 跨平台 | Linux / macOS / *BSD（不支持 Windows） | Linux / macOS / *BSD / Windows（Windows 尚未真机验证） |

### 性能对比

同机实测（Linux，`/usr` 约 50 万条目）。耗时口径是「扫描完成、浏览界面出现」：在 pty 中用 `script -qfec` 运行并以 `grep` 轮询输出（`-f` 让每次写入立即 flush —— 少了它，块缓冲会让轮询读到虚假的慢数据）；内存与 CPU 取自 `/usr/bin/time -v`。

| 项目 | C 版 ncdu 1.22 | ncdurs |
| --- | --- | --- |
| `/usr` 扫描到可浏览（两次取值） | 2.36 s / 2.31 s | 2.29 s / 1.93 s |
| CPU 时间（User + System） | 0.72 + 1.61 = 2.33 s | 0.49 + 1.25 = 1.74 s |
| 峰值内存 RSS | 55 MiB | 53 MiB |
| 二进制体积（release） | 281 KiB | 1.01 MiB（strip 后 810 KiB） |
| 参考：`du -s --block-size=512 /usr` | 7.6 s | — |
| 参考：C 版 `ncdu -o- /usr`（不建树，纯扫描 + 序列化） | 4.6 s | 已支持 `-o`，未单独计时 |

两版处于同一量级，ncdurs 略优。ncdurs 侧的手段是名字集中存放（name arena）+ 容量预分配 + 降低扫描期渲染频率，把同一场景的 CPU 时间从 3.60 s 压到 1.74 s、RSS 从 72 MiB 压到 53 MiB。内核侧 ncdurs 反而更贵一些：Rust 标准库使用 `statx` 而不是 `lstat`（strace 实测约 2 万次 `statx`），这部分开销被用户态分配的减少抵消了。

## 跨平台

| 平台 | "磁盘占用"取值方式 | 硬链接去重 | 状态 |
| --- | --- | --- | --- |
| Linux / macOS / *BSD | `st_blocks * 512` | `(st_dev, st_ino)` + `st_nlink` | 本机（Linux）已验证运行 |
| Windows | `GetCompressedFileSizeW`（实际分配大小，兼容稀疏/压缩文件）；失败时回退到逻辑大小（目录走这条） | 卷序列号 + `GetFileInformationByHandle` 的文件索引与链接数 | 已实现；`cargo check` 与 CI 覆盖，未在真机长期运行 |

所有平台差异都集中在 `src/platform.rs`：`disk_usage`（磁盘占用）、`file_info`（设备号 / inode / 链接数，供硬链接去重使用）、`ext_info`（`-e` 的 uid/gid/mode/mtime）、`is_firmlink`（macOS）。Windows 侧 `std` 仍不暴露文件索引与链接数（rust#63010），所以 `file_info` 走 `CreateFileW` + `GetFileInformationByHandle`，**每个条目开一次句柄**（同一次扫描里结果会复用，不会重复开）。

终端界面基于 `ratatui` + `crossterm`，Windows 的 ConHost / Windows Terminal 原生支持，无额外系统依赖。

## 项目结构

| 文件 | 职责 | 对应 C 版 |
| --- | --- | --- |
| `src/main.rs` | 参数解析、终端初始化/恢复、事件循环 | `src/main.c` |
| `src/scan.rs` | 递归扫描、大小汇总、`Tree` / `Node` 数据结构 | `src/dir_scan.c` + `src/dir_mem.c` |
| `src/platform.rs` | 平台差异：占用大小、文件标识、链接数 | （C 版内联在各处） |
| `src/app.rs` | 浏览状态：当前目录、排序、光标、按键处理 | `src/dirlist.c` |
| `src/ui.rs` | ratatui 绘制：状态栏 / 路径栏 / 列表 / 汇总行 | `src/browser.c` |

设计要点（细节见源码注释）：

- 整棵树存在 `Tree { nodes: Vec<Node> }` 里，父子关系用下标表示（arena），渲染时只读、无借用冲突；
- 所有条目名集中存放在 `Tree.names` 这一个字符串里，`Node` 只保存 `(start, len)`；每个条目因此不再需要单独的 `String`，配合节点容量预分配，把同一场景的 CPU 时间从 3.6 s 压到 1.7 s、RSS 从 72 MiB 压到 53 MiB；
- 扫描是「显式栈 + 分批推进」的迭代遍历（`Scanner::step()`），每批只处理有限条条目，因此扫描期间能持续重绘进度页并响应按键；节点入树时只带自身块，目录处理完后再回填汇总值，所以任意目录节点的 `size` 天然包含全部后代；
- 遍历用 `chdir` + 相对文件名（与 C 版一致），避免为每个条目解析绝对路径；`chdir` 是进程全局状态，因此同一时间只允许一个扫描，程序会在扫描结束或中止时恢复原工作目录；
- 扫描与浏览完全分离，中间只通过这棵树传递数据。

## 测试

单元测试 33 个：

- **扫描**：大小向上累加、硬链接只计一次且每条保留自身大小、非普通文件的 `notreg`、排除规则不计入且不深入、`-L` 只跟随指向文件的链接、分步与一次性扫描等价、中止/结束都恢复工作目录、删除子树后祖先扣减、重扫后子树替换一致、刷新拾取新文件；
- **导出 / 导入**：导出格式逐字段、往返一致、转义与代理对、`excluded` 的三种取值、硬链接字段、小数形式的 `mtime`、非法输入；
- **其他**：glob 匹配、`CACHEDIR.TAG` 签名、命令行展开（`-1xo-` / `-Xpatterns` / `--sort=name`）、配置文件解析、`NCDU_LEVEL` 语义；
- **界面渲染**（`TestBackend` 断言 buffer）：状态行与底栏的底色铺满整行、最大条目的图形条满格、mtime 列落在图形与名称之间。

```bash
cd rust
cargo test
```

与原版 ncdu 的对比脚本（数值 + 边界用例 + 双向导出/导入互通 + 性能，带 PASS/FAIL 断言）：

```bash
cd rust
bash scripts/compare.sh            # 默认对比 /usr
bash scripts/compare.sh ~/some/dir --runs 3   # 指定目录、性能跑 3 轮
bash scripts/compare.sh --skip-perf           # 跳过性能测量
```

脚本会依次做：`du` 与两个版本的 disk usage 三方对照、apparent size 与 items 的两版对照、自动生成含硬链接/稀疏文件/符号链接/无权限目录的用例树再对照一次、与 C 版的双向导出/导入一致性检查（`ncdu -o` → `ncdurs -f` → 再导出、以及反向，比对根尺寸与条目数）、最后测量「扫描到浏览界面出现」的耗时（min/avg）。全部通过时退出码 0。

### CI

`.github/workflows/rust.yml`（GitHub Actions；Gitea/Forgejo Actions 读同一路径）跑三个 job：

| job | 内容 |
| --- | --- |
| `test` | Linux / macOS / Windows 三平台矩阵：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`、`cargo build --release` |
| `interop` | Ubuntu：装 `libncurses-dev` 与 autotools，让 `compare.sh` 自行构建 C 版并在一个小目录上跑全部断言（`--skip-perf`，因为 runner 的计时噪声太大）。**C 版的构建产物有缓存**（生成的构建系统 + `src/*.o`，key 覆盖源码），源码没变时不会重编 |
| `msrv` | 用 1.88.0（声明的最低版本）编译并跑测试，防止不知不觉用上新语法 |

只在 `rust/**` 或工作流文件本身变化时触发，C 版的改动不会打扰它。本地等价的一条命令：

```bash
cd rust
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
```

### 发布二进制

推一个 tag 会触发 `.github/workflows/release.yml`，构建三平台二进制并附到对应 Release：

```bash
git tag rust-v0.2.0
git push ewoowe rust-v0.2.0
```

`rust-v` 前缀是为了与上游 ncdu 的 tag 区分开。产物（`strip` 已开启，每个约 1 MB）：

```
ncdurs-0.2.0-x86_64-unknown-linux-gnu.tar.gz
ncdurs-0.2.0-aarch64-apple-darwin.tar.gz
ncdurs-0.2.0-x86_64-pc-windows-msvc.zip
```

每个包里是 `ncdurs`（Windows 为 `ncdurs.exe`）与 `README.md`。手动触发该工作流（Actions → release → Run workflow）只构建归档并作为 workflow artifact 上传，**不会**发 Release。

两点说明：Linux 产物是动态链接 glibc 的，目标机器 glibc 太旧时自行 `cargo build --release`；Windows 的硬链接去重尚未实现（见上文），扫码结果会比 Linux 略大。

### 导出文件逐字节对照

想确认"两边导出的 JSON 是不是一模一样"，用规范化后的 diff（元数据与 `dev` 字段先去掉；`dev` 我们暂不保存）：

```bash
cd /media/wangcheng/WorkSpace/VSCode/ncdu-official/ncdu
./ncdu -o - /some/dir > /tmp/c.json
./rust/target/release/ncdurs -o /tmp/r.json /some/dir
norm() { sed -E 's/"progname":"[^"]*"/"progname":"X"/; s/"progver":"[^"]*"/"progver":"X"/; s/"timestamp":[0-9]+/"timestamp":0/; s/,"dev":[0-9]+//' "$1"; }
diff <(norm /tmp/c.json) <(norm /tmp/r.json) && echo '逐字节一致'
```

应当无差异（含硬链接条目）。另一个坑：**别用刚创建的文件做这种对照** —— `dd` 写完的文件 `st_blocks` 可能稍后才稳定，两版读到的会是不同时刻的快照。

### 界面渲染的验证

界面细节（列位置、颜色、底色范围、图形条格数）用下面两种方法核对，都不依赖人眼盯着屏幕：

**1. 渲染像素级断言（`TestBackend`）** —— 适合"某个位置应该是什么"的确定性检查：

```rust
use ratatui::backend::TestBackend;
let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
terminal.draw(|f| draw(f, &mut app)).unwrap();
let buffer = terminal.backend().buffer();
// 状态行铺到最后一列了吗？图形条的那一行满格了吗？
assert_eq!(buffer.cell((79, 0)).unwrap().style().bg, app.colors.hd.bg);
```

`src/ui.rs` 里现有两个这样的测试：**状态行与底栏的每一列是否都带标头底色**（对应 C 版的 `mvhline` 清整行）、**最大条目那一行的图形条是否全满**（对应按 `dirlist_maxs` 缩放）。它们都是改动前必然失败的回归保护。

**2. 同尺寸抓两版首帧后精确计数** —— 适合与 C 版对照"格数、字符数"这类量：

```bash
for v in ./ncdu ./rust/target/release/ncdurs; do
  { sleep 1.7; printf 'q'; sleep 0.3; } | timeout 9 env TERM=xterm-256color script -qefc \
    "stty rows 28 cols 110; $v /some/dir" /dev/null > /tmp/$(basename "$v").raw
done
python3 -c "
import re
for name in ['/tmp/ncdu.raw', '/tmp/ncdurs.raw']:
    t = re.sub(r'\x1b\[[0-9;?]*[a-zA-Z]', '', open(name, encoding='utf8', errors='replace').read())
    for line in t.replace('\r', '\n').split('\n'):
        if 'rust' in line and '[' in line:
            m = re.search(r'\[(#*)( *)\]', line)
            if m:
                print(name, '# 数', len(m.group(1)), '空格数', len(m.group(2)))
                break
"
```

三个要点，都踩过：

- **必须看首帧**：两边都是差分渲染，没变化的行（路径行、列表）不会重绘，流式输出里"看不到" —— 曾据此误判路径行只在子目录显示；
- **给 C 版留点时间**：扫描完成后它的首帧可能还是 `No items to display`，列表稍后才填充；
- **一定套 `timeout`**：帮助页、信息框这类窗口是**模态**的（`q` 只关闭窗口、不退出程序，与原版一致），脚本少发一次按键就会一直挂着。

交叉编译检查（只做类型检查，不需要目标平台链接器）：

```bash
rustup target add x86_64-pc-windows-msvc x86_64-pc-windows-gnu \
    x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-freebsd
cargo check --target x86_64-pc-windows-msvc
```

## 已知限制与待实现

现状：

- 扫描性能与 C 版同一量级（`/usr`、50 万条目上约 1.09 倍，见上表），内存占用略低；
- 遍历使用 `chdir`（进程全局状态），同一时间请只运行一个扫描实例；
- 删除与刷新只在内存树里「摘链 / 替换子树」，被移除的旧节点留在 arena 中成为不可达垃圾（不回收），反复大量操作会让内存缓慢增长；
- 删除是直接 `unlink` / 递归 `rmdir`，没有回收站语义，确认后立即生效；

- 颜色输出与 C 版**逐字节一致**：颜色写成经典 8 色 SGR（`33`/`46`/`42`…）而不是 crossterm 默认的 256 色语法（`38;5;3`）—— 见 `src/backend.rs`。这一点很重要：256 色表的前 16 项**是否**映射到终端自己的调色板由终端决定，某些主题会按固定色表渲染 `38;5;3`，于是 dark 主题看起来与原版不同；用经典色码则始终跟随终端配色；
- 界面已按原版对齐（列布局、底栏、扫描页/消息/确认框的居中窗口、`..` 项、帮助页、信息框），但有几个标记还画不出来：`<`（被排除项）、`>`（其它文件系统）、`^`（Linux 伪文件系统）、`F`（firmlink）需要对应的扫描选项，`H`（硬链接已计过）与 `.`（后代读取失败）都已实现，现在 9 种里已实现 8 种。

待实现（按优先级）：

1. （可选）arena 垃圾回收（compact），回收删除/刷新留下的不可达节点。

**已评估、不再列入待办（2026-10-15）**：内核调用优化 —— 即"用 `openat`/`fstatat` 替代 std 的路径解析"。`strace -c` 显示我们的 `statx` 调用次数与 C 版的 `newfstatat` **逐次相同**（`/usr/lib` 上 70169 对 70168），其余调用（`getdents64` / `chdir` / `openat` / `fstat`）也一一对应，所以这条路上没有可优化空间。当时观察到的约 32% 差距实际来自**导出到 stdout 时没有缓冲**（每个字段一次 `write`，共 70169 次，而 C 版走 stdio 只有 976 次），已改为 `BufWriter`（降到 912 次，syscall 总数与 C 版持平），差距随之降到约 9%；余下的部分在用户态的分配与数据结构，收益有限、改动风险不小，暂不做。

## 许可

与上游 ncdu 相同，见仓库根目录的 `COPYING`（MIT）。
