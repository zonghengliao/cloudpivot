#!/usr/bin/env bash
# ============================================================================
# P0-FIX-3B 迁移演练：up → up(幂等) → down(可逆) → up  （V0249）
#
#   bash drill.sh                  # 用 env 里的库（NIK_DATABASE_* / DB_*）
#   DB_NAME=cloudpivot_e2e_iso DB_USER=e2erole DB_PASSWORD=e2eiso_2026 bash drill.sh
#
# 判据（任一不满足即 exit 1）：
#   ① up 后 4 个夹具角色 data_scope=4，且 30_verify.sql 零 FAIL
#   ② 再跑一次 up：只打「已是 4，跳过」，数据零变化（幂等）
#   ③ down 后回到基线 1（快照与演练前**整串相等**）
#   ④ 再 up 一次：快照与第一次 up 后整串相等（可逆 + 复原）
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
UP="$REPO/backend/migrations/business/V0249__qa_fixture_role_scope_self_only.up.sql"
DOWN="$REPO/backend/migrations/business/V0249__qa_fixture_role_scope_self_only.down.sql"
OUT="${OUT_DIR:-/tmp/p0fix3b_drill}"
mkdir -p "$OUT"

DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-${NIK_DATABASE_USER:-alun}}"
DB_NAME="${DB_NAME:-${NIK_DATABASE_NAME:-cloudpivot_e2e_iso}}"
DB_PASSWORD="${DB_PASSWORD:-${NIK_DATABASE_PASSWORD:-alun}}"
export PGPASSWORD="$DB_PASSWORD"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -v ON_ERROR_STOP=1)
FAILED=0

say() { printf '%s\n' "$*"; }
bad() { printf '❌ %s\n' "$*"; FAILED=1; }

say "=============================================================="
say " P0-FIX-3B 演练库：$DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER)"
say " 日志目录：$OUT   时间：$(date '+%F %T %Z')"
say "=============================================================="

snap() { "${PSQL[@]}" -At -F'|' -f "$HERE/20_snapshot.sql" 2>/dev/null; }

verify() {
    local exp=$1 tag=$2
    "${PSQL[@]}" -v exp="$exp" -f "$HERE/30_verify.sql" > "$OUT/verify_${tag}.txt" 2>&1
    local nfail
    nfail=$(grep -c 'FAIL' "$OUT/verify_${tag}.txt" || true)
    if [ "$nfail" -ne 0 ]; then
        bad "30_verify（exp=$exp, $tag）有 $nfail 条 FAIL"
        grep 'FAIL' "$OUT/verify_${tag}.txt"
    else
        say "✅ 30_verify（exp=$exp, $tag）零 FAIL"
    fi
}

# ---- 0. 归位（保证演练可反复跑）：先 down 回到基线 1 -------------------------
say "---- ⓪ 归位：跑一次 down，把库拉回基线 1（这样脚本可反复运行） ----"
"${PSQL[@]}" -f "$DOWN" > "$OUT/up0_normalize.log" 2>&1
grep -E 'NOTICE|ERROR' "$OUT/up0_normalize.log" | sed 's/^/    /'

# ---- 1. 演练前现场 -----------------------------------------------------------
snap > "$OUT/snap_00_before.txt"
say "演练前夹具角色："
"${PSQL[@]}" -At -F'|' -c "select role_code, data_scope from sys_role where deleted_at is null and role_code in ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B') order by 1"
verify 1 "before"

# ---- 1. up ------------------------------------------------------------------
say ""
say "---- ① up ----"
"${PSQL[@]}" -f "$UP" > "$OUT/up1.log" 2>&1
rc=$?
grep -E 'NOTICE|WARNING|ERROR' "$OUT/up1.log" | sed 's/^/    /'
[ $rc -eq 0 ] || bad "up 退出码 $rc"
snap > "$OUT/snap_01_up.txt"
verify 4 "after_up"

# ---- 2. up 再跑一次（幂等） ---------------------------------------------------
say ""
say "---- ② up 再跑一次（幂等） ----"
"${PSQL[@]}" -f "$UP" > "$OUT/up2.log" 2>&1
rc=$?
grep -E 'NOTICE|WARNING|ERROR' "$OUT/up2.log" | sed 's/^/    /'
[ $rc -eq 0 ] || bad "up#2 退出码 $rc"
if grep -q "已是 data_scope=4，跳过（幂等）" "$OUT/up2.log"; then
    say "✅ 第二次 up 只打幂等跳过（$(grep -c '已是 data_scope=4' "$OUT/up2.log") 个角色）"
else
    bad "第二次 up 没有出现幂等跳过提示（可能重复写入）"
fi
snap > "$OUT/snap_02_up2.txt"
if diff -q "$OUT/snap_01_up.txt" "$OUT/snap_02_up2.txt" >/dev/null; then
    say "✅ 幂等：up 两次后快照整串相等"
else
    bad "幂等失败：up 两次后快照不一致"
    diff "$OUT/snap_01_up.txt" "$OUT/snap_02_up2.txt" || true
fi

# ---- 3. down（可逆） ----------------------------------------------------------
say ""
say "---- ③ down ----"
"${PSQL[@]}" -f "$DOWN" > "$OUT/down.log" 2>&1
rc=$?
grep -E 'NOTICE|WARNING|ERROR' "$OUT/down.log" | sed 's/^/    /'
[ $rc -eq 0 ] || bad "down 退出码 $rc"
snap > "$OUT/snap_03_down.txt"
verify 1 "after_down"
if diff -q "$OUT/snap_00_before.txt" "$OUT/snap_03_down.txt" >/dev/null; then
    say "✅ 可逆：down 后快照与演练前整串相等"
else
    bad "可逆失败：down 后与演练前快照不一致"
    diff "$OUT/snap_00_before.txt" "$OUT/snap_03_down.txt" || true
fi

# ---- 4. 再 up（复原） --------------------------------------------------------
say ""
say "---- ④ up 复原 ----"
"${PSQL[@]}" -f "$UP" > "$OUT/up3.log" 2>&1
rc=$?
[ $rc -eq 0 ] || bad "up#3 退出码 $rc"
snap > "$OUT/snap_04_up_final.txt"
verify 4 "final"
if diff -q "$OUT/snap_01_up.txt" "$OUT/snap_04_up_final.txt" >/dev/null; then
    say "✅ 复原：第二次 up 后与第一次 up 后快照整串相等"
else
    bad "复原失败：两次 up 后快照不一致"
    diff "$OUT/snap_01_up.txt" "$OUT/snap_04_up_final.txt" || true
fi

say ""
say "=============================================================="
if [ "$FAILED" -eq 0 ]; then
    say " ✅ 演练全绿：up / up(幂等) / down(可逆) / up(复原)  日志：$OUT"
    exit 0
else
    say " ❌ 演练有失败项，见上文与 $OUT"
    exit 1
fi
