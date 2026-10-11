# ncdurs

`ncdurs` 是 [ncdu](https://dev.yorhel.nl/ncdu)（C 版）的 Rust 重实现，目标是跨平台的磁盘占用浏览工具。

代码位于本仓库的 `rust/` 子目录，与 C 版源码、Autotools 构建完全独立：`cargo` 只针对 `rust/`，C 版仍用 `./configure && make`，两者互不影响。

> 当前状态：**初版（P0）**。可以扫描目录、交互浏览、排序、切换大小口径，数值与 C 版一致。
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

### 两种大小口径

- **磁盘占用（Disk usage）**：`st_blocks * 512`，文件实际占用的磁盘空间（按文件系统块分配）。**默认口径**，与 `du` 默认行为一致。想知道"删掉它能腾出多少空间"看这个。
- **表观大小（Apparent size）**：`st_size`，文件内容的逻辑长度，即 `ls -l` 显示的大小。

两者差异来源：块对齐（1 字节文件也占 4 KiB）、稀疏文件（apparent 1 GiB / disk 0 B）、压缩文件系统（disk 可能小于 apparent）等。按 `a` 键或启动时加 `-a` 切换，底部汇总行会同时列出两种口径。

## 与 C 版的关系

- 对同一目录，`Disk usage` / `Apparent size` / `Items` 三项与 `./ncdu` 完全一致（已在含硬链接的测试目录上对照验证）：

  ```text
  C  版 ./ncdu ：Total disk usage:  10.0 MiB   Apparent size:  10.0 MiB   Items: 5
  ncdurs      ：Disk usage: 10.0 MiB          Apparent size: 10.0 MiB   Items: 5
  ```

- 硬链接只计一次；符号链接不跟随；读不了/被拒绝的条目显示为 `[error]` 并继续扫描（均在扫描结束时汇总提示）。
- 尺寸口径、目录自身块计入父目录等语义与 C 版 `src/dir_scan.c` / `src/dir_mem.c` 保持一致。

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
- 扫描是单趟后序递归：节点先入树（只带自身块），子树扫完后再回填汇总值，因此任意目录节点的 `size` 天然包含全部后代；
- 扫描与浏览完全分离，中间只通过这棵树传递数据 —— 这也是当前"扫描期间无进度、不可中断"的原因。

## 测试

```bash
cd rust
cargo test          # 单元测试：大小向上累加、硬链接只计一次
```

交叉编译检查（只做类型检查，不需要目标平台链接器）：

```bash
rustup target add x86_64-pc-windows-msvc x86_64-pc-windows-gnu \
    x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-freebsd
cargo check --target x86_64-pc-windows-msvc
```

## 已知限制与待实现

现状（初版固有）：

- 扫描期间只有一行 `Scanning ...`，**无进度、不可中断**；扫大目录（如 `/`）需要等待；
- 扫描速度慢于 C 版与 `du`：逐项用绝对路径 `lstat`、每层拼接路径，未做 chdir 式相对路径或并行优化（实测 `/usr` 约 50 万条目，本机 17.5s，`du` 7.6s）；
- 图形列固定 10 格宽，不随终端宽度自适应；无颜色主题。

待实现（按优先级）：

1. 扫描进度显示 + 扫描中按 `q` 中断；
2. 扫描性能（相对路径/chdir 或并行扫描）；
3. 删除文件（`d`）、打开 shell（`b`）、刷新重扫（`r`）；
4. `--exclude PATTERN`、`-x`（不跨文件系统）、`--exclude-caches`；
5. 导出 / 导入 JSON（`-o` / `-f`，与 C 版格式互通）；
6. 扩展信息（`-e`：uid/gid/mtime）与按 mtime 排序；
7. 颜色主题（`--color off|dark|dark-bg`）、帮助页（`?`）、SI 单位（`--si`）；
8. Windows 硬链接去重、Windows 隐藏/系统属性过滤。

## 许可

与上游 ncdu 相同，见仓库根目录的 `COPYING`（MIT）。
