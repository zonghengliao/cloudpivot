#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 四步演练（SOP §9：up → up（幂等）→ down → up 全绿才许碰生产）
# ----------------------------------------------------------------------------
# 断言的是**可观测的行数/形状**，不是脚本退出码：
#   * 第 2 次 --grant / --revoke 必须 0 行变更
#   * --rollback（20.down → 10.down → 15.down → 05.down）必须把 4 行超管绑定加回、
#     5 行业务角色绑定删掉、9 行目录授权删掉、henry.liao.dept_id 回到 NULL
#   * 再 --grant + --revoke 必须回到目标态
#
# 用法：
#   set -a; . /root/superconv-run2/iso.env; set +a
#   bash scripts/datafix/superconv/verify_drill.sh
# 安全：只允许在隔离库跑（库名必须等于 SUPERCONV_ISO_DB），防止有人把它指到生产库。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISO_DB="${SUPERCONV_ISO_DB:-cloudpivot_superconv_c3}"
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
q() { "${PSQL[@]}" -c "$1"; }
ur_rows() { q "SELECT count(*) FROM sys_user_role"; }
rm_rows() { q "SELECT count(*) FROM sys_role_menu"; }
sup_members() { q "SELECT coalesce(string_agg(u.username, ',' ORDER BY u.username),'(无)')
     FROM sys_user_role ur JOIN sys_user u ON u.id=ur.user_id JOIN sys_role r ON r.id=ur.role_id
    WHERE r.role_code='SUPER_ADMIN'"; }
henry_dept() { q "SELECT coalesce(d.dept_code,'NULL') FROM sys_user u LEFT JOIN sys_dept d ON d.id=u.dept_id
    WHERE u.username='henry.liao'"; }
laoliu_roles() { q "SELECT count(*) FROM sys_user_role ur JOIN sys_user u ON u.id=ur.user_id
    WHERE u.username='laoliu'"; }
snap() { q "SELECT coalesce(string_agg(batch||'='||n, ',' ORDER BY batch),'(无)') FROM (
     SELECT batch, count(*) n FROM datafix_superconv_snapshot GROUP BY batch) t"; }

check() { # check <说明> <期望> <实际>
    if [ "$2" = "$3" ]; then echo "  [PASS] $1 = $3"; else echo "  [FAIL] $1 期望 $2 实际 $3"; FAIL=$((FAIL+1)); fi
}

echo "== SUPERCONV 四步演练 · $DB_NAME =="
bash "$HERE/apply.sh" --rollback >/dev/null 2>&1 || true
BASE_UR=$(ur_rows); BASE_RM=$(rm_rows)
echo "归一（全回退）后：sys_user_role=$BASE_UR sys_role_menu=$BASE_RM SUPER=$(sup_members) henry.dept=$(henry_dept)"
check "归一后超管成员" "admin,henry.liao,laoliu,laoliu_test" "$(sup_members)"
check "归一后 henry.liao 无部门" "NULL" "$(henry_dept)"

echo
echo "[1] --grant（05 落位 + 15 补 9 行目录 + 10 绑 5 个业务角色）"
bash "$HERE/apply.sh" --grant >/dev/null 2>&1 || { echo "  [FAIL] grant 失败"; FAIL=$((FAIL+1)); }
check "grant 后 sys_user_role（+5）" "$((BASE_UR + 5))" "$(ur_rows)"
check "grant 后 sys_role_menu（+9）"  "$((BASE_RM + 9))" "$(rm_rows)"
check "grant 后 laoliu 角色数（5 业务 + 仍是超管）" "6" "$(laoliu_roles)"
check "grant 后 henry.liao 部门" "HQ" "$(henry_dept)"
check "grant 后超管成员未变" "admin,henry.liao,laoliu,laoliu_test" "$(sup_members)"

echo
echo "[2] --grant 再跑一次（幂等）"
bash "$HERE/apply.sh" --grant >/dev/null 2>&1 || { echo "  [FAIL] grant 幂等重放失败"; FAIL=$((FAIL+1)); }
check "幂等重放后 sys_user_role 不变" "$((BASE_UR + 5))" "$(ur_rows)"
check "幂等重放后 sys_role_menu 不变" "$((BASE_RM + 9))" "$(rm_rows)"

echo
echo "[3] --revoke（摘 4 行）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] revoke 失败"; FAIL=$((FAIL+1)); }
check "revoke 后 sys_user_role（-4）" "$((BASE_UR + 1))" "$(ur_rows)"
check "revoke 后超管成员" "admin" "$(sup_members)"
check "revoke 后 laoliu 角色数" "5" "$(laoliu_roles)"

echo
echo "[4] --revoke 再跑一次（幂等）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] revoke 幂等重放失败"; FAIL=$((FAIL+1)); }
check "幂等重放后 sys_user_role 不变" "$((BASE_UR + 1))" "$(ur_rows)"
check "阶段 2 快照（4 绑定 + 3 账号）" "7" "$(q "SELECT count(*) FROM datafix_superconv_snapshot WHERE batch='SUPERCONV-20261105'")"
check "阶段 1 快照（5 业务角色绑定）" "5" "$(q "SELECT count(*) FROM datafix_superconv_snapshot WHERE batch='SUPERCONV-20261105-ROLES'")"
# DIRS 批次会随 down/up 往返累积历史行（down 故意保留快照留档），所以只断言「≥ 本次 9 行」
[ "$(q "SELECT count(*) FROM datafix_superconv_snapshot WHERE batch='SUPERCONV-20261105-DIRS'")" -ge 9 ] \
    && echo "  [PASS] 阶段 1.5 快照 ≥ 9 行" || { echo "  [FAIL] 阶段 1.5 快照不足 9 行"; FAIL=$((FAIL+1)); }

echo
echo "[5] --rollback-revoke（20.down：4 行超管绑定加回）"
bash "$HERE/apply.sh" --rollback-revoke >/dev/null 2>&1 || { echo "  [FAIL] rollback-revoke 失败"; FAIL=$((FAIL+1)); }
check "回滚阶段 2 后 sys_user_role" "$((BASE_UR + 5))" "$(ur_rows)"
check "回滚阶段 2 后超管成员" "admin,henry.liao,laoliu,laoliu_test" "$(sup_members)"

echo
echo "[6] 再 --revoke（回到目标态）"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || { echo "  [FAIL] 二次 revoke 失败"; FAIL=$((FAIL+1)); }
check "二次 revoke 后 sys_user_role" "$((BASE_UR + 1))" "$(ur_rows)"
check "二次 revoke 后超管成员" "admin" "$(sup_members)"

echo
echo "[7] --rollback 全退（20.down → 10.down → 15.down → 05.down）"
bash "$HERE/apply.sh" --rollback >/dev/null 2>&1 || { echo "  [FAIL] 全回退失败"; FAIL=$((FAIL+1)); }
check "全回退后 sys_user_role" "$BASE_UR" "$(ur_rows)"
check "全回退后 sys_role_menu" "$BASE_RM" "$(rm_rows)"
check "全回退后超管成员" "admin,henry.liao,laoliu,laoliu_test" "$(sup_members)"
check "全回退后 henry.liao 无部门" "NULL" "$(henry_dept)"
check "全回退后 laoliu 角色数" "1" "$(laoliu_roles)"

echo
echo "== 四步演练结果：FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] && echo "✓ up→up→down→up→down 全绿" || echo "✗ 有断言失败，不许碰生产"
exit "$FAIL"
