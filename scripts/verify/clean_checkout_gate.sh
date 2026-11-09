#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# clean_checkout_gate.sh — 干净检出闸（L3）：证明「干净检出不会红」
#
# 卡 t_2746ca53 ②「禁止本机依赖」。评审 5.7 原文：
#   「单测依赖 gitignored 私有配置，干净检出必红」
#
# 为什么需要这道闸（而不是只在 startup_guard.rs 里修一次）：
#   `.gitignore` 里有两类东西是**本机私有**的 —— `backend/config/config-dev.toml`、
#   `backend/config/config-test.toml`、`/etc/cloudpivot/*.env`、`frontend/node_modules`。
#   只要有一条测试断言「这些文件存在 / 内容是某个值」，那么**只在开发机上绿**，
#   CI、新人克隆、`git archive` 出来的发布包全是红的 —— 而开发机上永远看不见这个红。
#   E2E-14 已经把 `startup_guard.rs` 改成「私有文件在就断言它，不在就断言 *.example.toml」，
#   但**没有任何门禁守住这条性质**：下一条 `assert!(Path::new("config/config-dev.toml").exists())`
#   就能悄悄把「干净检出必红」带回来。本闸就是守这条性质的机械判据。
#
# 判据（真做一遍，不模拟）：
#   ① `git archive HEAD | tar -x -C $TMP` —— 干净检出 = **只含 git 跟踪的文件**；
#   ② 断言私有配置确实**不在**（在 = .gitignore 漏了 / 有人 -f 塞进去了 ⇒ 红）；
#   ③ 断言 `config-{dev,test}.example.toml` 模板确实**在**（模板被删，测试的兜底分支就失效 ⇒ 红）；
#   ④ 取已编译好的 lib 单测二进制（`cargo test --lib --no-run`，复用主 target，不重复编译），
#      把 **CWD 指到干净检出**再跑全量 —— 配置路径是相对 CWD 的 `config/`，
#      于是测试读到的是干净树里的配置面，与本机私有文件彻底隔离；
#   ⑤ 断言 0 failed 且 passed 数不低于地板值（防「过滤写错 → 0 条用例 → 假绿」）。
#
# 为什么这么快（实测 ≈15 s，其中单测 5.5 s）：复用主 target 的测试二进制，只花 extract 的时间。
# 干净树无库 ⇒ 真库用例按设计 SKIP（那部分由 `check.sh` 闸⓪ 在真库上管，不是本闸的职责）。
#
# 用法：
#   bash scripts/verify/clean_checkout_gate.sh              # 正常判红/判绿
#   bash scripts/verify/clean_checkout_gate.sh --self-test  # 判据自检（红/绿语料，秒级，不编译）
#   bash scripts/verify/clean_checkout_gate.sh --keep       # 保留临时目录便于排查
#   CLEAN_HEAD=<ref>  bash …            # 换基线（默认 HEAD）
#   CLEAN_TEST_FLOOR=<n> bash …         # 用例数地板（默认 1000）
#   CLEAN_TMPDIR=<dir>   bash …         # 临时目录根（默认 ${TMPDIR:-/tmp}；需 ≥1 GB 空闲）
#
# 退出码：0 = 干净检出可跑（绿）；1 = 干净检出必红 / 检出不干净；2 = 环境问题（跑不起来）
# ═══════════════════════════════════════════════════════════════════════════
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

HEAD_REF="${CLEAN_HEAD:-HEAD}"
TEST_FLOOR="${CLEAN_TEST_FLOOR:-1000}"
KEEP=0
SELF_TEST=0
for arg in "$@"; do
    case "$arg" in
        --keep)      KEEP=1 ;;
        --self-test) SELF_TEST=1 ;;
        -h|--help)   sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数：$arg（支持 --self-test / --keep / --help）" >&2; exit 2 ;;
    esac
done

# 私有文件 / 模板清单（与 .gitignore 第 24–28 行同口径）
PRIVATE_FILES=(
    "backend/config/config-dev.toml"
    "backend/config/config-test.toml"
    ".env"
    "backend/.env"
)
TEMPLATE_FILES=(
    "backend/config/config-dev.example.toml"
    "backend/config/config-test.example.toml"
    "backend/config/config.toml"
    "backend/config/config-prod.toml"
)

say()  { echo "==> $*"; }
ok()   { echo "  ✓ $*"; }
bad()  { echo "  ✗ $*" >&2; }
info() { echo "  ℹ $*"; }

# ══════════════════════════════════════════════════════════════════════════
# 判据 ①②③ 的纯文件系统部分，抽成函数 —— 自检语料直接复用它（保证「自检的就是判据本身」）
#   参数：<干净树根>，返回 0=干净，非 0=不干净（并打印原因）
# ══════════════════════════════════════════════════════════════════════════
check_tree_surface() {
    local tree="$1" rc=0 f
    for f in "${PRIVATE_FILES[@]}"; do
        if [ -e "$tree/$f" ]; then
            bad "干净检出里出现了私有配置：$f（.gitignore 漏了，或被人 git add -f 塞进去了）"
            rc=1
        fi
    done
    for f in "${TEMPLATE_FILES[@]}"; do
        if [ ! -f "$tree/$f" ]; then
            bad "干净检出里缺少模板：$f（模板不在了，测试的「私有文件缺失」兜底分支会失效）"
            rc=1
        fi
    done
    # node_modules / 私有 env 不该出现在 git 跟踪面（前者只是体积，后者是安全问题）
    if [ -e "$tree/frontend/node_modules" ]; then
        bad "干净检出里出现了 frontend/node_modules（.gitignore 漏了？）"
        rc=1
    fi
    return "$rc"
}

# ══════════════════════════════════════════════════════════════════════════
# --self-test：用构造的红/绿语料证明判据本身有效（不编译、不联网、秒级）
# ══════════════════════════════════════════════════════════════════════════
if [ "$SELF_TEST" = "1" ]; then
    say "干净检出闸 · 判据自检（红/绿语料）"
    st_tmp="$(mktemp -d "${CLEAN_TMPDIR:-${TMPDIR:-/tmp}}/cleanco-selftest.XXXXXX")" || exit 2
    trap 'rm -rf "$st_tmp"' EXIT
    pass=0; fail=0
    mk_fixture() {   # 造一个「只含 tracking 面」的绿语料
        local d="$1"
        mkdir -p "$d/backend/config"
        for f in "${TEMPLATE_FILES[@]}"; do : > "$d/$f"; done
    }
    expect() {      # expect <绿|红> <说明> <命令…>
        local want="$1" label="$2"; shift 2
        if "$@" >/dev/null 2>&1; then got=绿; else got=红; fi
        if [ "$got" = "$want" ]; then pass=$((pass + 1)); echo "  ✓ [$want] $label"
        else fail=$((fail + 1)); echo "  ✘ 期望[$want] 实际[$got]：$label" >&2; fi
    }
    # 绿语料
    mk_fixture "$st_tmp/green"
    expect 绿 "只含跟踪面 → 判绿"            check_tree_surface "$st_tmp/green"
    # 红语料 ①：私有配置混进检出
    mk_fixture "$st_tmp/red_private"
    : > "$st_tmp/red_private/backend/config/config-dev.toml"
    expect 红 "混入 config-dev.toml → 判红"   check_tree_surface "$st_tmp/red_private"
    # 红语料 ②：缺 example 模板
    mk_fixture "$st_tmp/red_no_tpl"
    rm -f "$st_tmp/red_no_tpl/backend/config/config-test.example.toml"
    expect 红 "缺模板 example → 判红"        check_tree_surface "$st_tmp/red_no_tpl"
    # 红语料 ③：node_modules 混进来
    mk_fixture "$st_tmp/red_nm"
    mkdir -p "$st_tmp/red_nm/frontend/node_modules"
    expect 红 "混入 node_modules → 判红"     check_tree_surface "$st_tmp/red_nm"
    # 红语料 ④：.env 混进来
    mk_fixture "$st_tmp/red_env"
    : > "$st_tmp/red_env/.env"
    expect 红 "混入 .env → 判红"             check_tree_surface "$st_tmp/red_env"

    echo ""
    if [ "$fail" -ne 0 ]; then
        echo "✘ 干净检出闸自检未通过：$pass 通过 / $fail 失败（判据被改坏了？）" >&2
        exit 1
    fi
    echo "✓ 干净检出闸自检通过（$pass/$pass）"
    exit 0
fi

# ══════════════════════════════════════════════════════════════════════════
# 主流程
# ══════════════════════════════════════════════════════════════════════════
cd "$ROOT" || { bad "进不了仓库根 $ROOT"; exit 2; }

sha="$(git rev-parse --short "$HEAD_REF" 2>/dev/null)" || { bad "取不到 $HEAD_REF"; exit 2; }
say "干净检出闸：基线 $HEAD_REF ($sha)"

# 编译面一致性提示：本闸跑的是「主 target 里已编好的二进制（= 工作树）」＋「HEAD 的配置面」。
# 工作树有未提交的后端改动时，两者不是同一份源码 —— 不判红（提交前的正常状态），但要如实说。
if [ -n "$(git status --porcelain -- backend/src backend/Cargo.toml 2>/dev/null)" ]; then
    info "工作树有未提交的后端改动：本闸验证的是「工作树代码 + HEAD 的配置面」（提交后二者一致）"
fi

TMPROOT="${CLEAN_TMPDIR:-${TMPDIR:-/tmp}}"
# 空间预检：git archive 约 250 MB，解压后约 400 MB（实测 2026-11-10）
need_kb=$((900 * 1024))
avail_kb="$(df -Pk "$TMPROOT" 2>/dev/null | awk 'NR==2 {print $4}')"
if [ -n "${avail_kb:-}" ] && [ "$avail_kb" -lt "$need_kb" ]; then
    bad "临时目录所在分区空间不足：$TMPROOT 只剩 $((avail_kb / 1024)) MB，需要 ≥900 MB"
    echo "    用 CLEAN_TMPDIR=<另一个盘上的目录> 重跑" >&2
    exit 2
fi

CLEAN_TREE="$(mktemp -d "$TMPROOT/cleanco.XXXXXX")" || { bad "mktemp 失败（$TMPROOT）"; exit 2; }
cleanup() {
    if [ "$KEEP" = "1" ]; then echo "  ℹ --keep：临时目录保留在 $CLEAN_TREE" >&2
    else rm -rf "$CLEAN_TREE"; fi
}
trap cleanup EXIT

say "① 提取干净检出（git archive $HEAD_REF → $CLEAN_TREE）"
if ! git archive "$HEAD_REF" | tar -x -C "$CLEAN_TREE"; then
    bad "git archive / tar 失败"; exit 2
fi
n_files="$(find "$CLEAN_TREE" -type f | wc -l | tr -d ' ')"
ok "提取完成：$n_files 个文件（只含 git 跟踪面）"

say "② 断言检出确实干净（私有配置不得混进来）"
if ! check_tree_surface "$CLEAN_TREE"; then
    bad "检出不干净 —— 「干净检出」这个前提本身不成立，先修 .gitignore / 别 git add -f"
    exit 1
fi
ok "无私有配置（$(printf '%s ' "${PRIVATE_FILES[@]}")），模板齐全"

say "③ 取 lib 单测二进制（复用主 target，不重复编译）"
if ! command -v cargo >/dev/null 2>&1; then
    bad "找不到 cargo：本闸需要先能编译测试二进制"; exit 2
fi
build_log="$(mktemp "$TMPROOT/cleanco-build.XXXXXX.log")"
( cd "$ROOT/backend" && cargo test --lib --no-run ) > "$build_log" 2>&1
build_rc=$?
TEST_BIN="$(sed -n 's/.*Executable unittests src\/lib\.rs (\(.*\))$/\1/p' "$build_log" | tail -1)"
BIN_ABS=""
case "$TEST_BIN" in
    /*) BIN_ABS="$TEST_BIN" ;;
    "") BIN_ABS="" ;;
    *)  BIN_ABS="$ROOT/backend/$TEST_BIN" ;;
esac
if [ "$build_rc" -ne 0 ] || [ -z "$BIN_ABS" ] || [ ! -x "$BIN_ABS" ]; then
    bad "拿不到 lib 单测二进制（cargo test --lib --no-run 退出码 $build_rc）："
    tail -15 "$build_log" | sed 's/^/    /' >&2
    echo "    日志：$build_log" >&2
    exit 2
fi
ok "二进制：$BIN_ABS"

say "④ 在干净检出的 CWD 上跑全量 lib 单测（配置面 = 干净树）"
run_log="$(mktemp "$TMPROOT/cleanco-run.XXXXXX.log")"
( cd "$CLEAN_TREE/backend" && "$BIN_ABS" --test-threads=1 ) > "$run_log" 2>&1
run_rc=$?
result_line="$(grep -E '^test result:' "$run_log" | tail -1)"
[ -n "$result_line" ] && echo "  $result_line"

passed="$(printf '%s' "$result_line" | sed -n 's/.*ok\. \([0-9][0-9]*\) passed.*/\1/p')"
failed="$(printf '%s' "$result_line" | sed -n 's/.*\([0-9][0-9]*\) failed.*/\1/p')"
passed="${passed:-0}"; failed="${failed:-0}"

if [ "$run_rc" -ne 0 ] || [ "$failed" -ne 0 ]; then
    bad "干净检出任有 $failed 条单测失败（这正是「干净检出必红」）"
    grep -E '^test .* FAILED|^---- .* stdout' "$run_log" | head -20 | sed 's/^/    /' >&2
    echo "    完整日志：$run_log（--keep 可保留干净树排查）" >&2
    echo "    修法：把测试改成「私有文件在就断言它，不在就断言 *.example.toml」（范本：backend/src/shared/startup_guard.rs）" >&2
    exit 1
fi

if [ "$passed" -lt "$TEST_FLOOR" ]; then
    bad "只跑了 $passed 条用例（地板 $TEST_FLOOR）→ 过滤/二进制不对，这是假绿"
    exit 1
fi

ok "干净检出全绿：$passed passed / 0 failed（地板 $TEST_FLOOR）"
rm -f "$build_log" "$run_log"
echo "✓ 干净检出闸通过（基线 $sha）"
exit 0
