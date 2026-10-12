# ncdurs

`ncdurs` 是 [ncdu](https://dev.yorhel.nl/ncdu)（C 版）的 Rust 重实现，目标是跨平台的磁盘占用浏览工具。

代码位于本仓库的 `rust/` 子目录，与 C 版源码、Autotools 构建完全独立：`cargo` 只针对 `rust/`，C 版仍用 `./configure && make`，两者互不影响。

> 当前状态：**P0 + 扫描体验**。可以扫描目录（带进度页、可随时中止）、交互浏览、排序、切换大小口径；数值与 C 版一致，扫描性能也与 C 版同一量级（见「性能对比」）。
> 已通过与 C 版的自动化对比（`scripts/compare.sh`：数值、边界用例、性能）以及 4 个单元测试。
> 删除、刷新、排除规则、导出/导入等功能尚未实现，见文末「待实现」。

## 构建与运行

需要 Rust 1.85 或更高版本（crate 使用 edition 2024，开发环境为 1.96）。

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
-h, --help            显示帮助
-V, --version         显示版本
```

`PATH` 缺省为当前目录。路径不存在或不是目录时报错退出（退出码 1）。

### 按键

| 按键 | 作用 |
| --- | --- |
| `↑` / `k`、`↓` / `j` | 上下移动光标 |
| `PgUp` / `PgDn` | 翻页 |
| `g` / `G`（或 Home/End） | 跳到首项 / 末项 |
| `→` / `l` / `Enter` | 进入选中的目录 |
| `←` / `u` / `Backspace` | 返回上级目录 |
| `s` | 切换排序列：size → name → items |
| `d` | 反转排序方向 |
| `a` | 在磁盘占用 / 表观大小之间切换 |
| `q` / `Esc` / `Ctrl-C` | 退出 |

扫描期间会显示进度页（当前路径、已扫条目数与已扫大小），此时按 `q` 或 `Esc` 可随时中止扫描。

### 两种大小口径

- **磁盘占用（Disk usage）**：`st_blocks * 512`，文件实际占用的磁盘空间（按文件系统块分配）。**默认口径**，与 `du` 默认行为一致。想知道"删掉它能腾出多少空间"看这个。
- **表观大小（Apparent size）**：`st_size`，文件内容的逻辑长度，即 `ls -l` 显示的大小。

两者差异来源：块对齐（1 字节文件也占 4 KiB）、稀疏文件（apparent 1 GiB / disk 0 B）、压缩文件系统（disk 可能小于 apparent）等。按 `a` 键或启动时加 `-a` 切换，底部汇总行会同时列出两种口径。

## 与原版 ncdu（C 版 1.22）的对比

下表以原版的 `ncdu.1` 手册与 `src/help.c` 为准逐项核对（「部分」= 能力等价但接口/平台有差异）。整体进度：约 59 项中已实现 8 项、部分实现 5 项、未实现 46 项。

### 数值一致性

- 对同一目录，`Disk usage` / `Apparent size` / `Items` 三项与 `./ncdu` 完全一致（已在含硬链接的测试目录上对照验证）：

  ```text
  C  版 ./ncdu ：Total disk usage:  10.0 MiB   Apparent size:  10.0 MiB   Items: 5
  ncdurs      ：Disk usage: 10.0 MiB          Apparent size: 10.0 MiB   Items: 5
  ```

- 硬链接只计一次；符号链接不跟随；读不了/被拒绝的条目显示为 `[error]` 并继续扫描（均在扫描结束时汇总提示）。
- 尺寸口径、目录自身块计入父目录等语义与 C 版 `src/dir_scan.c` / `src/dir_mem.c` 保持一致。

### 命令行选项

| 选项（原版） | ncdurs | 备注 |
| --- | --- | --- |
| `-h, --help` | 已实现 | |
| `-v, -V, --version` | 部分 | 有 `-V` / `--version`，无 `-v` |
| `-f FILE`（导入扫描结果） | 未实现 | 计划与 C 版 JSON 格式互通 |
| `-o FILE`（导出为 JSON） | 未实现 | 同上 |
| `-e, --extended, --no-extended` | 未实现 | 当前不保存 uid/gid/mtime |
| `--ignore-config` | 未实现 | 尚无配置文件 |
| `-x, --one-file-system` / `--cross-file-system` | 未实现 | |
| `--exclude PATTERN` / `-X, --exclude-from FILE` | 未实现 | |
| `--exclude-caches` / `--include-caches` | 未实现 | |
| `-L, --follow-symlinks` / `--no-follow-symlinks` | 未实现 | 行为等同原版默认（不跟随） |
| `--exclude-kernfs` / `--include-kernfs` | 未实现 | |
| `--exclude-firmlinks` / `--follow-firmlinks`（macOS） | 未实现 | |
| `-0` / `-1` / `-2`（UI 级别） | 未实现 | 只有全屏界面 |
| `-q, --slow-ui-updates` / `--fast-ui-updates` | 未实现 | 扫描期刷新间隔固定 50 ms |
| `--enable-shell` / `--disable-shell` | 未实现 | 无 shell 功能 |
| `--enable-delete` / `--disable-delete` | 未实现 | 无删除功能 |
| `--enable-refresh` / `--disable-refresh` | 未实现 | 无刷新功能 |
| `-r` / `-rr`（只读） | 未实现 | 当前没有任何破坏性操作 |
| `--si` / `--no-si` | 未实现 | 固定二进制单位（KiB / MiB / ...） |
| `-a, --apparent-size` / `--disk-usage` | 部分 | 默认即磁盘占用，有 `-a`；没有 `--disk-usage` 显式开关 |
| `--show-hidden` / `--hide-hidden` | 未实现 | |
| `--show-itemcount` / `--hide-itemcount` | 未实现 | 无 itemcount 列（汇总在底栏） |
| `--show-mtime` / `--hide-mtime` | 未实现 | 依赖 `-e` |
| `--show-graph` / `--hide-graph` | 未实现 | 图形列始终显示 |
| `--show-percent` / `--hide-percent` | 未实现 | 百分比始终显示 |
| `--graph-style hash\|half-block\|eighth-block` | 未实现 | 固定 `#` 风格、固定 10 格宽 |
| `--sort COLUMN` | 未实现 | 界面内 `s` 键可切换排序列 |
| `--enable-natsort` / `--disable-natsort` | 未实现 | 现为小写字符串比较（原版默认开启自然序） |
| `--group-directories-first` / `--no-group-directories-first` | 未实现 | |
| `--confirm-quit` / `--no-confirm-quit` | 未实现 | 退出无二次确认 |
| `--confirm-delete` / `--no-confirm-delete` | 未实现 | 无删除功能 |
| `--color off\|dark\|dark-bg` | 未实现 | 只有选中行反色高亮 |

### 界面按键

| 按键（原版） | 功能 | ncdurs |
| --- | --- | --- |
| `↑`/`k`、`↓`/`j` | 移动光标 | 已实现（另有 PgUp / PgDn、g / G、Home / End） |
| `→`/`Enter` | 进入目录 | 已实现（另有 `l`） |
| `←`/`<`/`h` | 返回上级 | 部分：`←` / `u` / `Backspace`（原版用 `<` / `h`） |
| `n` / `s` / `C` | 按名称 / 大小 / items 排序 | 部分：一个 `s` 键循环切换（size → name → items），`d` 反向 |
| `M` | 按 mtime 排序 | 未实现（依赖 `-e`） |
| `t` | 排序时目录优先 | 未实现 |
| `g` | 显示百分比 / 图形 | 未实现（两者始终显示） |
| `a` | 切换表观大小 / 磁盘占用 | 已实现 |
| `c` | 切换子项计数列 | 未实现（底栏始终显示 Items） |
| `m` | 切换 mtime 列 | 未实现 |
| `e` | 显示/隐藏 隐藏与排除项 | 未实现 |
| `i` | 显示选中项信息 | 未实现 |
| `r` | 重算当前目录 | 未实现 |
| `b` | 在当前目录开 shell | 未实现 |
| `d` | 删除选中项 | 未实现 |
| `?` | 帮助页 | 未实现 |
| `q` | 退出 | 已实现（另有 `Esc` / `Ctrl-C`） |
| `q`（扫描期间） | 中止扫描 | 已实现 |

### 其他特性

| 特性 | 原版 | ncdurs |
| --- | --- | --- |
| 扫描进度页（当前路径 / 条目数 / 已扫大小 / 可中止） | 支持 | 已实现 |
| 硬链接去重 | 支持（并标记 `H`） | 支持（Unix；Windows 暂缺，且无标记） |
| 条目状态标记（`!` `.` `<` `e` `>` `@` `^` `H` `F`） | 9 种，显示在名称前 | 仅错误条目附加 `[error]` 文本 |
| 配置文件（`/etc/ncdu.conf`、`~/.config/ncdu/config`） | 支持 | 未实现 |
| 环境变量 `NCDU_SHELL`、`NCDU_LEVEL` | 支持 | 未实现（无 shell 功能） |
| 导出 / 导入 JSON | 支持 | 未实现 |
| 显示列（图 / 百分比 / itemcount / mtime） | 可自由开关 | 固定：大小 + 图形 + 百分比 |
| 颜色主题 | 3 种（off / dark / dark-bg） | 未实现 |
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
| 参考：C 版 `ncdu -o- /usr`（不建树，纯扫描 + 序列化） | 4.6 s | 未实现导出 |

两版处于同一量级，ncdurs 略优。ncdurs 侧的手段是名字集中存放（name arena）+ 容量预分配 + 降低扫描期渲染频率，把同一场景的 CPU 时间从 3.60 s 压到 1.74 s、RSS 从 72 MiB 压到 53 MiB。内核侧 ncdurs 反而更贵一些：Rust 标准库使用 `statx` 而不是 `lstat`（strace 实测约 2 万次 `statx`），这部分开销被用户态分配的减少抵消了。

## 跨平台

| 平台 | "磁盘占用"取值方式 | 硬链接去重 | 状态 |
| --- | --- | --- | --- |
| Linux / macOS / *BSD | `st_blocks * 512` | `(st_dev, st_ino)` + `st_nlink` | 本机（Linux）已验证运行 |
| Windows | `GetCompressedFileSizeW`（实际分配大小，兼容稀疏/压缩文件） | **暂未实现** | 通过 `cargo check`，未在真机运行 |

所有平台差异集中在 `src/platform.rs`（三个函数：`disk_usage` / `file_id` / `nlink`）。Windows 侧 `std` 的 `file_index()` / `number_of_links()` 仍不稳定（rust#63010），正确识别硬链接需要为每个文件打开句柄，代价较大，因此 Windows 上同一文件的多个硬链接会被重复计数（行为等同 `dir /s`）。

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

单元测试（大小向上累加、硬链接只计一次、分步扫描与一次性扫描等价、中止/结束都恢复工作目录）：

```bash
cd rust
cargo test
```

与原版 ncdu 的对比脚本（数值 + 边界用例 + 性能，带 PASS/FAIL 断言）：

```bash
cd rust && ./scripts/compare.sh            # 默认对比 /usr
./scripts/compare.sh ~/some/dir --runs 3   # 指定目录、性能跑 3 轮
./scripts/compare.sh --skip-perf           # 只做数值与边界用例
```

脚本会依次做：`du` 与两个版本的 disk usage 三方对照、apparent size 与 items 的两版对照、自动生成含硬链接/稀疏文件/符号链接/无权限目录的用例树再对照一次、最后测量「扫描到浏览界面出现」的耗时（min/avg）。全部通过时退出码 0。

交叉编译检查（只做类型检查，不需要目标平台链接器）：

```bash
rustup target add x86_64-pc-windows-msvc x86_64-pc-windows-gnu \
    x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-freebsd
cargo check --target x86_64-pc-windows-msvc
```

## 已知限制与待实现

现状：

- 扫描性能与 C 版同一量级（见上表），内存占用略低；
- 遍历使用 `chdir`（进程全局状态），同一时间请只运行一个扫描实例；
- 图形列固定 10 格宽，不随终端宽度自适应；无颜色主题。

待实现（按优先级）：

1. 删除文件（`d`）、打开 shell（`b`）、刷新重扫（`r`）；
2. `--exclude PATTERN`、`-x`（不跨文件系统）、`--exclude-caches`；
3. 导出 / 导入 JSON（`-o` / `-f`，与 C 版格式互通）；
4. 扩展信息（`-e`：uid/gid/mtime）与按 mtime 排序；
5. 排序补齐：自然序（natsort，C 版默认开启）、目录优先；
6. 颜色主题（`--color off|dark|dark-bg`）、帮助页（`?`）、SI 单位（`--si`）；
7. Windows 硬链接去重、Windows 隐藏/系统属性过滤；
8. （可选）内核调用优化：用 `openat`/`fstatat` 或 libc 直调替代 std 的 `statx`。

## 许可

与上游 ncdu 相同，见仓库根目录的 `COPYING`（MIT）。
