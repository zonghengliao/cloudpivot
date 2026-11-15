#!/usr/bin/env bash
# ============================================================================
# test_seed_gate.sh —— P2-62「数据准备脚本化」门禁（秒级，不编译、不连库）
#
# 为什么需要它：`ensure_*` 夹具只有**一份实现**才有价值。改前是 41 个文件各自一份
# `fn uniq()`（4 种实现）、13 个文件各自 10 行 `INSERT INTO biz_material`。人是很健忘的，
# 下一次加用例时「顺手抄一段 INSERT」几乎必然发生 —— 本门禁就是那道闸：
#
#   ① 编码/年度工具的实现只有一份：除 `test_seed.rs` 外，`fn uniq(` / `fn tail_code(`
#      / `fn fx_suffix_code(` / `fn next_year(` 的函数体必须转发到 `test_seed::`；
#   ② `test_seed.rs` 的导出名单齐全（删掉任何一个 `ensure_*` 都会红）；
#   ③ 测试代码里主数据 `INSERT INTO` 不超过**登记预算**（只许降、不许升）。
#
# 用法：
#   bash scripts/verify/test_seed_gate.sh              # 跑门禁
#   bash scripts/verify/test_seed_gate.sh --selftest   # 反证：造一个「本地 uniq 实现」的文件，必须 exit=1
#
# 退出码：0 = 全过；1 = 有违规（明细在 stderr）；2 = 前置缺失（源码路径不对）
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC="$ROOT/backend/app/src"
SEED="$SRC/test_seed.rs"

die() { echo "==> test_seed_gate 未通过：$*" >&2; exit 1; }

# --- ① 转发检查 -------------------------------------------------------------
# 打印 src 下所有「本地实现」（函数体 4 行内没出现 test_seed::）的 文件:行:函数名
forward_violations() {
    local src="$1"
    local files
    files=$(grep -rlE 'fn (uniq|tail_code|fx_suffix_code|next_year)\(' "$src" --include=*.rs 2>/dev/null \
            | grep -v '/test_seed\.rs$' || true)
    [ -z "$files" ] && return 0
    # shellcheck disable=SC2086
    awk '
        function flush() {
            if (pending != "") {
                if (body !~ /test_seed::/) printf "%s:%d:%s\n", fname, lineno, pending
                pending = ""
            }
        }
        {
            # 先把「上一行起的函数体」继续累积（最多 4 行，够装下转发体）
            if (pending != "") { body = body $0; seen++; if (seen >= 4) flush() }
            # 再判断本行是不是又一份本地实现（注释里的不算）
            if ($0 ~ /fn (uniq|tail_code|fx_suffix_code|next_year)\(/ && $0 !~ /^[[:space:]]*(\/\/|\*)/) {
                pending = $0; lineno = FNR; fname = FILENAME; body = ""; seen = 0
            }
        }
        END { flush() }
    ' $files
}

# --- ③ 主数据 INSERT 预算 ---------------------------------------------------
# 键 = 表名，值 = 上限（= 2026-11-15 P2-62 实测值）。**只会往下改**：
# 每迁一批用例到 ensure_*，就把对应数字下调；不许上调（上调等于承认回潮）。
declare -A BUDGET=(
    [sys_dept]=8  [sys_user]=29 [sys_user_role]=15 [sys_role]=7
    [biz_unit]=9  [biz_material]=12 [biz_warehouse]=11 [biz_account]=6
    [biz_supplier]=7 [biz_customer]=1 [biz_accounting_period]=2 [biz_location]=2
)

master_insert_count() {
    local table="$1"
    grep -rhoE "INSERT INTO ${table}\b" \
        --include=*_tests.rs --include=*_test.rs --include=*_support.rs \
        "$SRC" 2>/dev/null | wc -l | tr -d ' '
}

selftest() {
    local tmp
    tmp=$(mktemp -d)
    mkdir -p "$tmp/src"
    cat > "$tmp/src/fake_tests.rs" <<'RS'
fn uniq() -> String {
    let now = 1;
    format!("{now}")
}
RS
    local out
    out=$(forward_violations "$tmp/src")
    rm -rf "$tmp"
    if [ -n "$out" ]; then
        echo "✓ 反证成立：本地 uniq 实现被门禁抓到（exit=1 语义）"
        echo "   $out"
        exit 1        # 反证脚本本身就是「应当失败」，返回 1 让调用方看到门禁有效
    fi
    echo "✗ 反证失败：伪造的本地实现没被门禁抓到 —— 门禁逻辑坏了" >&2
    exit 0
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
fi

[ -d "$SRC" ] || { echo "源码目录不存在：$SRC" >&2; exit 2; }
[ -f "$SEED" ] || die "缺少 $SEED（统一夹具模块被删了？）"

echo "==> ① 编码/年度工具只有一份实现（转发到 crate::test_seed）"
viol=$(forward_violations "$SRC")
if [ -n "$viol" ]; then
    echo "$viol" >&2
    die "上面这些文件又自己实现了一遍 uniq/tail_code/next_year —— 改成 4 行转发：
        fn uniq() -> String { crate::test_seed::uniq_suffix() }"
fi
echo "    ✓ $(grep -rlE 'fn (uniq|tail_code|fx_suffix_code|next_year)\(' "$SRC" --include=*.rs | grep -vc '/test_seed\.rs$') 处本地工具函数全部转发"

echo "==> ② test_seed 导出名单齐全"
missing=0
use_block=$(sed -n '/^pub use /,/^};/p' "$SEED")
for name in uniq_suffix uniq_u64 tail_code last_chars year_seq period_code \
            ensure_unit ensure_material ensure_material_kind ensure_warehouse \
            ensure_account ensure_supplier ensure_customer ensure_accounting_period \
            ensure_location sweep_once ensure_any_dept ensure_dept ensure_user \
            ensure_two_users ensure_role_member ensure_approver_roles fixture_uuid; do
    # 两种合法形态：① 本文件 `pub fn name(` / `pub async fn name(`；
    #              ② `pub use …::{…}` 块里的裸名（底座 sys_* 夹具的实现留在 kernel crate）
    if ! grep -qE "^pub (async )?fn ${name}\(" "$SEED" \
       && ! printf '%s\n' "$use_block" | grep -qE "(^|[^A-Za-z0-9_])${name}([^A-Za-z0-9_]|$)"; then
        echo "    缺少导出：$name" >&2
        missing=$((missing + 1))
    fi
done
[ "$missing" -eq 0 ] || die "test_seed.rs 少了 $missing 个导出（用例会编译不过或被迫回去手写 INSERT）"
echo "    ✓ 23 个导出齐全"

echo "==> ③ 测试代码主数据 INSERT 不超预算（只许降）"
over=0
for table in "${!BUDGET[@]}"; do
    n=$(master_insert_count "$table")
    limit=${BUDGET[$table]}
    if [ "$n" -gt "$limit" ]; then
        echo "    ✗ $table：$n 处 > 预算 $limit" >&2
        over=$((over + 1))
    fi
done
[ "$over" -eq 0 ] || die "有 $over 张主数据表的手写 INSERT 回潮了：新用例请用 crate::test_seed::ensure_*（见 docs/architecture/P2-62-数据准备脚本化.md）"
total=0
for table in "${!BUDGET[@]}"; do total=$((total + $(master_insert_count "$table"))); done
echo "    ✓ 主数据手写 INSERT 合计 $total 处（预算 $total，改一次下调一次）"
echo "==> test_seed_gate 全过（① 转发 / ② 导出 / ③ 预算）"
