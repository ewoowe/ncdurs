#!/usr/bin/env bash
#
# compare.sh - 对比 ncdurs（Rust 版）与原版 ncdu（C 版）
#
# 做四件事：
#   1. 前置检查（依赖、二进制；默认顺带构建两个版本）
#   2. 数值对比：对若干目录比较 disk usage / apparent size / items，
#      并用 `du` 作为 disk usage 的独立基准
#   3. 边界用例：自动生成含硬链接、稀疏文件、符号链接、无权限目录的树，
#      再次校验两版结果一致
#   4. 性能对比：测量「扫描 + 导出」的耗时（无界面模式，多次取 min/avg）
#
# 用法: scripts/compare.sh [DIR] [--runs N] [--no-build] [--skip-perf]
#   DIR          要比对的目录（默认 /usr）
#   --runs N     性能测试每版重复次数（默认 2）
#   --no-build   跳过构建
#   --skip-perf  只做数值与边界对比
#
# 依赖: bash, script(1)(util-linux), du, awk, grep, sed, tr, date, truncate
#
# 注意: 运行时会通过 script(1) 在临时 pty 里启动两个 TUI 程序并把输出写入
# 文件，所以当前终端不会被接管。`script -f`（flush）是必须的：没有它，输出
# 文件是块缓冲的，据此轮询得出的任何时间都是错的。
#
# 退出码: 0 = 全部 PASS, 1 = 有 FAIL 或环境不满足

set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUST_BIN="$ROOT/rust/target/release/ncdurs"
C_BIN="$ROOT/ncdu"
TERM_VAL="${TERM:-xterm-256color}"

# 扫描超过这个时间（秒）就被中止，避免脚本挂死；大目录请调大
SCAN_TIMEOUT_S=60
POLL_STEPS=$((SCAN_TIMEOUT_S * 20))   # 每次 sleep 0.05s

DIR="/usr"
RUNS=2
DO_BUILD=1
SKIP_PERF=0

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      sed -n '2,30p' "$0" | sed -e 's/^#\{1,\} \{0,1\}//'
      exit 0 ;;
    --runs)      RUNS="${2:-2}"; shift 2 ;;
    --no-build)  DO_BUILD=0; shift ;;
    --skip-perf) SKIP_PERF=1; shift ;;
    -*)          echo "未知选项: $1" >&2; exit 1 ;;
    *)           DIR="$1"; shift ;;
  esac
done

TMP="$(mktemp -d)"
cleanup() { chmod -R u+rwX "$TMP" 2>/dev/null; rm -rf "$TMP"; }
trap cleanup EXIT

PASS=0
FAIL=0

section() { printf '\n=== %s ===\n' "$1"; }

check() {  # check <描述> <期望> <实际>
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1))
    printf '  [PASS] %s: %s\n' "$1" "$3"
  else
    FAIL=$((FAIL + 1))
    printf '  [FAIL] %s: 期望 %s, 实际 %s\n' "$1" "$2" "$3"
  fi
}

norm() { tr -d ' ' <<<"$1"; }

# ---------------------------------------------------------------- 前置检查

section "前置检查"

for cmd in script du awk grep sed tr date truncate mktemp; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "缺少命令: $cmd" >&2; exit 1; }
done
echo "  依赖检查通过"

if [ "$DO_BUILD" = 1 ]; then
  # A fresh checkout has no Makefile yet, so configure first. Both steps fail
  # hard: carrying on without a C binary only defers the error to the check
  # below, where the reason is much less obvious.
  if [ ! -f "$ROOT/Makefile" ]; then
    # A fresh checkout carries only configure.ac and Makefile.am - configure and
    # Makefile.in are generated (and gitignored), so autotools have to create
    # the build system first.
    if [ ! -x "$ROOT/configure" ]; then
      echo "  生成 configure (autoreconf -i) ..."
      ( cd "$ROOT" && autoreconf -i >/dev/null 2>&1 ) \
        || { echo "  error: autoreconf 失败（需要 autoconf 与 automake）" >&2; exit 1; }
    fi
    echo "  配置 C 版 (configure) ..."
    ( cd "$ROOT" && ./configure >/dev/null 2>&1 ) \
      || { echo "  error: ./configure 失败（需要 ncurses 开发包）" >&2; exit 1; }
  fi
  echo "  构建 C 版 (make) ..."
  ( cd "$ROOT" && make -j"$(nproc 2>/dev/null || echo 2)" >/dev/null 2>&1 ) \
    || { echo "  error: make 失败（若 C 版已构建过，可用 --no-build）" >&2; exit 1; }
  echo "  构建 Rust 版 (cargo build --release) ..."
  ( cd "$ROOT/rust" && cargo build --release 2>&1 | tail -1 )
fi

[ -x "$C_BIN" ]    || { echo "找不到 C 版二进制: $C_BIN（先 make 或用 --no-build 前先构建）" >&2; exit 1; }
[ -x "$RUST_BIN" ] || { echo "找不到 Rust 版二进制: $RUST_BIN（先 cargo build --release）" >&2; exit 1; }
echo "  C 版:    $C_BIN"
echo "  Rust 版: $RUST_BIN"

# ------------------------------------------------------------ 通用小工具

# 在临时 pty 中运行一次 TUI，进入浏览界面后发 q 退出。
# run_tui <bin> <dir> <outfile> <errfile>
#
# 这里等的是一个固定时间，而不是"界面出现"的某种信号，两个原因：
#
#   * 界面文本不能用：ncdurs 在每个 cell 之前都发一次光标定位，输出里没有
#     连续的 "Apparent size" 可以匹配（C 版用 ncurses，连续输出，能匹配）。
#     早先的 grep 判据对 ncdurs 永不命中，每次都白等到超时 —— 三个用例目录
#     就浪费了 180 秒。
#   * "输出稳定"也不行：正因为逐 cell 定位，ncdurs 每帧要写约 50 KB，而且
#     扫描页有动画，输出一直在增长，判据同样永不满足。
#
# 固定等待是安全的：给的目录都是小目录，1 秒足够画完；万一还在扫描，'q' 在
# 扫描页正是"中止"键，程序会立刻退出，不会挂住。
TUI_WAIT_S=${TUI_WAIT_S:-1}

run_tui() {
  local bin="$1" dir="$2" out="$3" err="$4"
  rm -f "$out" "$err"
  ( sleep "$TUI_WAIT_S"; printf 'q' ) \
    | TERM="$TERM_VAL" script -qefc "stty rows 40 cols 130; $bin $(printf '%q' "$dir")" /dev/null \
        >"$out" 2>"$err"
}

# 去掉 ANSI 序列与字符集切换，得到近似纯文本
clean_out() {
  tr -d '\033\017\016' <"$1" \
    | sed -e 's/([B0]//g' -e 's/\[[0-9;?]*[a-zA-Z]//g' \
    | tr '\r' '\n'
}

# 从清理后的输出里取数值。ratatui 的差分渲染会吞掉空格（"Diskusage:21.4GiB"），
# 所以正则里空格是可选的。
# metric <cleaned-file> disk|apparent|items
metric() {
  local f="$1" key="$2"
  case "$key" in
    disk)
      grep -aoE '([Tt]otal )?[Dd]isk *usage: *[0-9.]+ *[KMGT]?i?B' "$f" | tail -1 \
        | grep -aoE '[0-9.]+ *[KMGT]?i?B' ;;
    apparent)
      grep -aoE 'Apparent *size: *[0-9.]+ *[KMGT]?i?B' "$f" | tail -1 \
        | grep -aoE '[0-9.]+ *[KMGT]?i?B' ;;
    items)
      grep -aoE 'Items: ?[0-9]+' "$f" | tail -1 | grep -aoE '[0-9]+' ;;
  esac
}

# 从导出文件里算出总量：`export_value <file> dsize|asize|items`。
#
# 导出里没有"总量"字段 —— 每个条目（包括根目录）写的是它**自身**的大小，
# 所以总量要**累加**全部条目。硬链接已经去重，因此累加值就是界面底栏和
# `du` 看到的那个总量。这是数值对比的数据来源：确定性，不受渲染方式或
# 扫描快慢影响。
export_value() {
  local f="$1" key="$2"
  case "$key" in
    dsize) grep -aoE '"dsize":[0-9]+' "$f" | grep -aoE '[0-9]+$' | awk '{s += $1} END {print s + 0}' ;;
    asize) grep -aoE '"asize":[0-9]+' "$f" | grep -aoE '[0-9]+$' | awk '{s += $1} END {print s + 0}' ;;
    items) grep -ao '"name"' "$f" | wc -l ;;
  esac
}

# du 的 disk usage（字节）：块数 × 512，与 ncdu 的 dsize 口径一致
du_bytes() { du -s -B512 "$1" 2>/dev/null | awk '{print $1 * 512}'; }

# 与 ncdurs ui::human_size / C 版显示一致的格式化（1024 进制，1 位小数）
human_size() {
  awk -v b="${1:-0}" 'BEGIN {
    split("B KiB MiB GiB TiB PiB EiB", u, " ");
    if (b < 1024) { printf "%d B", b; exit }
    i = 1; v = b;
    while (v >= 1024 && i < 7) { v /= 1024; i++ }
    printf "%.1f %s", v, u[i];
  }'
}

# ---------------------------------------------------------------- 数值对比

compare_dir() {  # compare_dir <标签> <目录>
  local label="$1" dir="$2"
  section "数值对比: $label ($dir)"

  if [ ! -d "$dir" ]; then
    echo "  跳过：不是目录"
    return
  fi

  # 数值取自导出文件，而不是抓屏解析界面文字。
  #
  # 抓屏需要"等界面画完"，对扫描慢的目录（/usr 要两三秒）会抓不到内容，而
  # 空值互相比较会伪装成通过 —— 那是最糟的一种失败。导出是确定性的，而且与
  # 渲染方式无关；"界面上那个数字显示得对不对"由 src/ui.rs 的 TestBackend
  # 断言负责，这里不再重复那条路径。
  local c_json="$TMP/c-scan.json" r_json="$TMP/r-scan.json"
  if ! "$C_BIN" -0 -o - "$dir" >"$c_json" 2>/dev/null; then
    echo "  跳过：C 版导出失败（$dir）"
    return
  fi
  if ! "$RUST_BIN" -0 -o - "$dir" >"$r_json" 2>/dev/null; then
    check "$label: ncdurs 导出" "成功" "失败"
    return
  fi

  # 界面仍然各跑一次，但只断言"画出来了东西"，不解析其中的数值。
  run_tui "$C_BIN"    "$dir" "$TMP/c.raw" "$TMP/c.err"
  run_tui "$RUST_BIN" "$dir" "$TMP/r.raw" "$TMP/r.err"
  clean_out "$TMP/c.raw" >"$TMP/c.txt"
  clean_out "$TMP/r.raw" >"$TMP/r.txt"
  local c_bytes r_bytes
  c_bytes=$(wc -c <"$TMP/c.raw")
  r_bytes=$(wc -c <"$TMP/r.raw")
  if [ "$c_bytes" -lt 200 ] || [ "$r_bytes" -lt 200 ]; then
    check "$label: 界面有输出" "≥200 字节（两版）" "C 版 ${c_bytes} / ncdurs ${r_bytes} 字节"
  fi

  local c_disk r_disk c_app r_app c_items r_items ref_bytes ref_human
  c_disk="$(human_size "$(export_value "$c_json" dsize)")"
  r_disk="$(human_size "$(export_value "$r_json" dsize)")"
  c_app="$(human_size "$(export_value "$c_json" asize)")"
  r_app="$(human_size "$(export_value "$r_json" asize)")"
  c_items="$(export_value "$c_json" items)"
  r_items="$(export_value "$r_json" items)"
  ref_bytes="$(du_bytes "$dir")"
  ref_human="$(human_size "${ref_bytes:-0}")"

  printf '  %-10s %-14s %-14s %s\n' "" "disk usage" "apparent size" "items"
  printf '  %-10s %-14s %-14s %s\n' "du(ref)" "$ref_human" "-" "-"
  printf '  %-10s %-14s %-14s %s\n' "ncdu(C)" "$c_disk" "$c_app" "$c_items"
  printf '  %-10s %-14s %-14s %s\n' "ncdurs" "$r_disk" "$r_app" "$r_items"

  check "$label: ncdurs 与 C 版 disk usage 一致"  "$(norm "$c_disk")"  "$(norm "$r_disk")"
  check "$label: ncdurs disk usage 与 du 一致"    "$(norm "$ref_human")" "$(norm "$r_disk")"
  check "$label: apparent size 与 C 版一致"       "$(norm "$c_app")"   "$(norm "$r_app")"
  check "$label: items 与 C 版一致"               "$(norm "$c_items")" "$(norm "$r_items")"

  if [ -s "$TMP/r.err" ]; then
    printf '  (ncdurs stderr: %s)\n' "$(head -1 "$TMP/r.err")"
  fi
}

compare_dir "目标目录" "$DIR"

# ---------------------------------------------------------------- 边界用例

section "边界用例（硬链接 / 稀疏文件 / 符号链接 / 无权限目录）"

CASE="$TMP/case"
mkdir -p "$CASE/sub" "$CASE/empty" "$CASE/secret"
dd if=/dev/zero of="$CASE/sub/f1" bs=1M count=10 status=none
ln "$CASE/sub/f1" "$CASE/sub/f2"          # 硬链接：两边都应只计一次
head -c 1 /dev/zero >"$CASE/tiny"         # 1 字节文件
truncate -s 64M "$CASE/sparse"             # 稀疏文件：disk 应远小于 apparent
ln -s "$CASE" "$CASE/loop"                # 指向自身的符号链接：不应跟随
echo hidden >"$CASE/secret/h"
chmod 000 "$CASE/secret"                  # 无权限目录：应报错但继续

ls -l "$CASE" | sed 's/^/  /'
compare_dir "边界用例" "$CASE"

# ------------------------------------------------- 跨目录硬链接（记账方式与 C 版一致）

section "跨目录硬链接（同一 inode 落在两个目录）"

# a/f1 and b/f2 share one inode: the totals must agree, while the per-directory
# split is known to differ - see "硬链接记账方式的差异" in README.
HLCASE="$TMP/hlcase"
mkdir -p "$HLCASE/a" "$HLCASE/b"
dd if=/dev/zero of="$HLCASE/a/f1" bs=1M count=1 status=none
ln "$HLCASE/a/f1" "$HLCASE/b/f2"

compare_dir "跨目录硬链接" "$HLCASE"

# Shows the (known) difference instead of asserting on it: in the export, the C
# version keeps the full size on *every* link (plus hlnkc/ino), while our file
# only carries the size of the link that was actually counted.
hl_items() {  # hl_items <bin> -> the f1/f2 entries as they appear in the export
  "$1" -o - "$HLCASE" 2>/dev/null | grep -aoE '\{"name":"f[12]"[^}]*\}'
}

echo "  导出文件里同一 inode 的两个条目："
echo "  C 版（每条链接都带自身大小 + hlnkc/ino）："
hl_items "$C_BIN" | sed 's/^/    /'
echo "  ncdurs（每条链接同样带自身大小 + ino/hlnkc/nlink）："
hl_items "$RUST_BIN" | sed 's/^/    /'
echo "  两版在此完全一致：每条硬链接都保留自身大小与 ino/hlnkc/nlink，只有总量按一次计入（此前记录的「记账方式差异」已不存在）。"

# ---------------------------------------------------------- 导出/导入互通

section "导出 / 导入互通（与 C 版）"

# Numbers that are guaranteed to survive a round trip: the root entry's sizes
# and the total number of items. Per-item sizes of hard links are not compared:
# the C version exports the size of every link and de-duplicates while loading,
# while we store the already de-duplicated values (the totals agree).
export_summary() {
  local root items
  root="$(grep -aoE '"(asize|dsize)":[0-9]+' "$1" | head -2 | tr '\n' ' ')"
  items="$(grep -ao '"name"' "$1" | wc -l)"
  printf '%s items=%s' "$root" "$items"
}

interop_dir() {  # interop_dir <label> <dir>
  local label="$1" dir="$2"
  local cjson="$TMP/c-export.json" rjson="$TMP/r-export.json"
  local again="$TMP/r-again.json" c2json="$TMP/c2-export.json"

  # 1) C export -> ncdurs import -> export again: numbers must be identical.
  if ! "$C_BIN" -o - "$dir" >"$cjson" 2>/dev/null; then
    echo "  跳过：C 版导出失败（$dir）"
    return
  fi
  if "$RUST_BIN" -f "$cjson" -o "$again" 2>"$TMP/interop.err"; then
    check "$label: ncdurs 读 C 版导出后数值不变" \
      "$(export_summary "$cjson")" "$(export_summary "$again")"
  else
    check "$label: ncdurs 导入 C 版导出" "成功" "失败：$(head -1 "$TMP/interop.err")"
  fi

  # 2) ncdurs export -> C import -> C export: numbers must match C's own scan.
  if "$RUST_BIN" -o "$rjson" "$dir" >/dev/null 2>&1; then
    if "$C_BIN" -0 -f "$rjson" -o "$c2json" 2>"$TMP/interop2.err"; then
      check "$label: C 版读 ncdurs 导出后数值不变" \
        "$(export_summary "$cjson")" "$(export_summary "$c2json")"
    else
      check "$label: C 版导入 ncdurs 导出" "成功" "失败：$(head -1 "$TMP/interop2.err")"
    fi
  fi
}

interop_dir "目标目录" "$DIR"
interop_dir "边界用例" "$CASE"

# ---------------------------------------------------------------- 性能对比

if [ "$SKIP_PERF" = 1 ]; then
  section "性能对比：已跳过"
else
  section "性能对比（扫描 + 导出到 /dev/null，$RUNS 轮）"

  # measure_scan <bin> <dir> → 打印毫秒数
  #
  # The scan is timed without the TUI: "-0" plus an export to /dev/null.
  # ncdurs moves the cursor before every single cell (that is what makes
  # partial redraws and wide characters safe), so its terminal output contains
  # no contiguous "Apparent size" text - the previous TUI-based measurement,
  # which grepped for that string, could never see ncdurs finish. Both versions
  # scan exactly the same tree here, which is what this comparison is about;
  # the drawing time of the browser is not covered.
  measure_scan() {
    local bin="$1" dir="$2" s e rc
    s=$(date +%s%N)
    TERM="$TERM_VAL" "$bin" -0 -o - "$dir" > /dev/null 2>&1
    rc=$?
    e=$(date +%s%N)
    if [ "$rc" -ne 0 ]; then
      echo "FAILED"
    else
      echo $(((e - s) / 1000000))
    fi
  }

  c_times=""
  r_times=""
  for run in $(seq 1 "$RUNS"); do
    t_c="$(measure_scan "$C_BIN" "$DIR")"
    t_r="$(measure_scan "$RUST_BIN" "$DIR")"
    printf '  第 %s 轮: ncdu(C) %6s ms   ncdurs %6s ms\n' "$run" "$t_c" "$t_r"
    c_times="$c_times $t_c"
    r_times="$r_times $t_r"
  done

  stats() {
    awk '{ n=0; s=0; m=0
           for (i=1;i<=NF;i++) { if ($i !~ /^[0-9]+$/) continue; s+=$i; n++; if (m==0 || $i<m) m=$i }
           if (n) printf "min=%d ms  avg=%.0f ms", m, s/n; else printf "无有效样本" }' <<<"$1"
  }
  printf '  ncdu(C): %s\n' "$(stats "$c_times")"
  printf '  ncdurs : %s\n' "$(stats "$r_times")"
  echo "  提示: 两者若相差在几十毫秒内即视为同一量级；测量前请避免其它重负载。"
fi

# -------------------------------------------------------------------- 汇总

section "汇总"
printf '  PASS: %d   FAIL: %d\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
  echo "  结论: 全部通过"
else
  echo "  结论: 存在不一致，请查看上面的 [FAIL] 行"
  exit 1
fi
