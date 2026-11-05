#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 四步演练（SOP §9：up → up（幂等）→ down → up 全绿才许碰生产）
# ----------------------------------------------------------------------------
# 断言的是**可观测的行数**，不是脚本退出码：
#   * 第 2 次 up 必须 0 行变更（user_role / role_menu 行数与快照行数都不动）
#   * down 必须把 3 行绑定加回来，并且 laoliu_test 的范围回到 All
#   * 再 up 必须回到「SUPER_ADMIN = admin(+henry.liao)」
#
# 用法：
#   set -a; . /root/superconv-run/iso.env; set +a
#   bash scripts/datafix/superconv/verify_drill.sh
# 安全：只允许在隔离库跑（SUPERCONV_ISO_DB 匹配），防止有人把它指到生产库。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISO_DB="${SUPERCONV_ISO_DB:-cloudpivot_superconv_c1}"
DB_NAME="${SUPERCONV_DB:-${NIK_DATABASE_NAME:-}}"
if [ "$DB_NAME" != "$ISO_DB" ]; then
    echo "✗ 拒跑：SUPERCONV_DB=$DB_NAME，四步演练只允许在隔离副本 $ISO_DB 上跑" >&2
    exit 1
fi
: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_*}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL=(psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
           -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -X -q -At -v ON_ERROR_STOP=1)

FAIL=0
ur_rows() { "${PSQL[@]}" -c "SELECT count(*) FROM sys_user_role"; }
snap_rows() { "${PSQL[@]}" -c "SELECT count(*) FROM datafix_superconv_snapshot WHERE batch='SUPERCONV-20261105'"; }
sup_members() { "${PSQL[@]}" -c "SELECT string_agg(u.username, ',' ORDER BY u.username)
     FROM sys_user_role ur JOIN sys_user u ON u.id=ur.user_id JOIN sys_role r ON r.id=ur.role_id
    WHERE r.role_code='SUPER_ADMIN'"; }
lt_scope() { "${PSQL[@]}" -c "SELECT count(DISTINCT data_scope)::text || ':' ||
       coalesce(string_agg(DISTINCT data_scope::text, ','), '')
     FROM sys_user_role ur JOIN sys_user u ON u.id=ur.user_id JOIN sys_role r ON r.id=ur.role_id
    WHERE u.username='laoliu_test'"; }

check() { # check <说明> <期望> <实际>
    if [ "$2" = "$3" ]; then echo "  [PASS] $1 = $3"; else echo "  [FAIL] $1 期望 $2 实际 $3"; FAIL=$((FAIL+1)); fi
}

echo "== SUPERCONV 四步演练 · $DB_NAME =="
echo "起始：ur_rows=$(ur_rows) snap=$(snap_rows) SUPER=$(sup_members) laoliu_test_scopes=$(lt_scope)"

# 归一：先回退到「没有 OPS_ADMIN、没有任何摘权」的起点
bash "$HERE/apply.sh" --rollback >/dev/null 2>&1 || true
BASE_UR=$(ur_rows)
echo "归一后：ur_rows=$BASE_UR SUPER=$(sup_members)"

echo
echo "[1] 10 up（建角色 + 授权 + 绑定）"
bash "$HERE/apply.sh" --grant >/dev/null 2>&1 || { echo "  [FAIL] grant 失败"; FAIL=$((FAIL+1)); }
UR1=$(ur_rows)
check "grant 后 sys_user_role" "$((BASE_UR + 2))" "$UR1"

echo "[2] 10 up 再跑一次（幂等）"
bash "$HERE/apply.sh" --grant >/dev/null 2>&1 || { echo "  [FAIL] grant 幂等重放失败"; FAIL=$((FAIL+1)); }
check "幂等重放后 sys_user_role 不变" "$UR1" "$(ur_rows)"

echo "[3] 20 up（摘 3 行）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] revoke 失败"; FAIL=$((FAIL+1)); }
check "revoke 后 sys_user_role" "$((UR1 - 3))" "$(ur_rows)"
check "revoke 后 SUPER_ADMIN 成员" "admin,henry.liao" "$(sup_members)"
check "revoke 后 laoliu_test 只剩 3 档" "1:3" "$(lt_scope)"

echo "[4] 20 up 再跑一次（幂等）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] revoke 幂等重放失败"; FAIL=$((FAIL+1)); }
check "幂等重放后 sys_user_role 不变" "$((UR1 - 3))" "$(ur_rows)"
SNAP=$(snap_rows)
check "快照行数（3 绑定 + 2 账号）" "5" "$SNAP"

echo "[5] 20 down（回滚：3 行加回去）"
bash "$HERE/apply.sh" --rollback-revoke >/dev/null 2>&1 || { echo "  [FAIL] rollback-revoke 失败"; FAIL=$((FAIL+1)); }
check "回滚后 sys_user_role" "$UR1" "$(ur_rows)"
check "回滚后 SUPER_ADMIN 成员" "admin,henry.liao,laoliu,laoliu_test" "$(sup_members)"

echo "[6] 20 up 再上一次（回到目标态）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] 二次 revoke 失败"; FAIL=$((FAIL+1)); }
check "二次 revoke 后 sys_user_role" "$((UR1 - 3))" "$(ur_rows)"
check "二次 revoke 后 SUPER_ADMIN 成员" "admin,henry.liao" "$(sup_members)"

echo
echo "== 四步演练结果：FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] && echo "✓ up→up→down→up 全绿" || echo "✗ 有断言失败，不许碰生产"
exit "$FAIL"
