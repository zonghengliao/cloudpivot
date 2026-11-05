#!/usr/bin/env bash
# ============================================================================
# C2 演练（隔离副本）：laoliu_test 摘权后的可见面 + 「部门子树过滤」因果对照
# ----------------------------------------------------------------------------
# 为什么要临时探针角色：laoliu_test 的 3 个业务角色里**没有** business:task:read /
# business:project:read（他没有这两个码，摘超管后接口直接 403），
# 所以「子树过滤」这件事必须借一个只含这两个读码的 data_scope=3 探针角色才看得见。
# 探针角色与 biz_task.dept_id 改动全部在副本内，脚本末尾净零还原。
#
# 用法：set -a; . /root/superconv-run2/iso.env; set +a; bash verify_c2_subtree.sh
# 安全：库名必须等于 SUPERCONV_ISO_DB（默认 cloudpivot_superconv_c3），防止指到生产。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ISO_DB="${SUPERCONV_ISO_DB:-cloudpivot_superconv_c3}"
DB_NAME="${SUPERCONV_DB:-${NIK_DATABASE_NAME:-}}"
[ "$DB_NAME" = "$ISO_DB" ] || { echo "✗ 拒跑：SUPERCONV_DB=$DB_NAME ≠ $ISO_DB" >&2; exit 1; }
BASE="${SUPERCONV_BASE:-http://127.0.0.1:18098}"
PORT="${BASE##*:}"
RUN_DIR="${ISO_RUN_DIR:-/root/superconv-run2}"
PASS="${NIK_ADMIN_PASSWORD:?需要 NIK_ADMIN_PASSWORD（副本内口令）}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL=(psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
           -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -X -q -At -v ON_ERROR_STOP=1)
q() { "${PSQL[@]}" -c "$1"; }

FAIL=0
check() { if [ "$2" = "$3" ]; then echo "  [PASS] $1 = $3"; else echo "  [FAIL] $1 期望 $2 实际 $3"; FAIL=$((FAIL+1)); fi; }

restart_iso() {
    local pid
    pid=$(ss -ltnp 2>/dev/null | grep "127.0.0.1:${PORT}" | grep -oP 'pid=\K[0-9]+' | head -1 || true)
    [ -n "$pid" ] && { kill "$pid" 2>/dev/null || true; sleep 2; }
    ( cd "$RUN_DIR" && setsid bash start_iso.sh >> backend.log 2>&1 < /dev/null & )
    for _ in $(seq 1 60); do
        [ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")" = "200" ] && return 0
        sleep 1
    done
    echo "✗ 隔离实例重启后 health 非 200" >&2; exit 1
}

probe() { # probe <label>
    SUPERCONV_BASE="$BASE" python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
        --password "$PASS" --label "$1" | tee "/tmp/c2_$2.json"
}

echo "== C2 演练 · $DB_NAME @ $BASE =="

echo
echo "[1] 目标态（摘权后）真登录：laoliu_test 能看见什么"
bash "$HERE/apply.sh" --revoke >/dev/null 2>&1 || true
# 副本内：用 admin 令牌把 laoliu_test 的口令重置成本地已知值（生产绝不做；生产他 status=0 本就不登录），
# 并清掉锁定/首登改密标记，保证「真登录」这条路可重复。
AT=$(python3 "$HERE/mint_token.py" --username admin)
UID_LT=$("${PSQL[@]}" -c "SELECT id FROM sys_user WHERE username='laoliu_test' AND deleted_at IS NULL")
curl -s -o /dev/null -X PATCH -H "Authorization: Bearer $AT" -H 'Content-Type: application/json' \
     -d "{\"password\":\"$PASS\"}" "$BASE/api/system/user/$UID_LT/password"
"${PSQL[@]}" -c "UPDATE sys_user SET must_change_password=false, is_locked=false, login_fail_count=0
                  WHERE username='laoliu_test' AND deleted_at IS NULL" >/dev/null
restart_iso
probe "after(3 个 3 档角色)" after
check "after: /task/page 被拒" "403" "$(python3 -c "import json;print(json.load(open('/tmp/c2_after.json'))['http']['biz_task'])" 2>/dev/null || echo '?')"

echo
echo "[2] 临时探针角色（data_scope=3 + task/project 读码，仅副本）"
"${PSQL[@]}" <<'SQL'
INSERT INTO sys_role (id, role_code, role_name, description, status, sort_order, remark, data_scope)
VALUES ('5c0e0000-0000-4000-8000-0000000000aa', 'SCOPE_PROBE_TMP', 'C2 范围探针（临时）',
        '只为把 /business/task/page 叫通，验证部门子树过滤；演练结束即删', 1, 99,
        'isolation-only probe role', 3)
ON CONFLICT (id) DO NOTHING;
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), '5c0e0000-0000-4000-8000-0000000000aa', m.id
  FROM sys_menu m WHERE m.auth_code IN ('business:task:read','business:project:read') AND m.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
INSERT INTO sys_user_role (id, user_id, role_id)
SELECT '5c0e0000-0000-4000-8000-0000000000ab', u.id, '5c0e0000-0000-4000-8000-0000000000aa'
  FROM sys_user u WHERE u.username = 'laoliu_test'
ON CONFLICT (user_id, role_id) DO NOTHING;
SQL
q "SELECT '  探针角色权限码 = ' || count(*) FROM sys_role_menu rm JOIN sys_menu m ON m.id=rm.menu_id
    WHERE rm.role_id='5c0e0000-0000-4000-8000-0000000000aa' AND m.auth_code<>''"

TECH=$(q "SELECT id FROM sys_dept WHERE dept_code='TECH' AND deleted_at IS NULL")
FIN=$(q "SELECT id FROM sys_dept WHERE dept_code='FIN' AND deleted_at IS NULL")
TASK=$(q "SELECT id FROM biz_task WHERE dept_id IS NULL AND deleted_at IS NULL ORDER BY created_at LIMIT 1")
echo "  探针任务 = $TASK（dept 依次 NULL → TECH → FIN）"
restart_iso

echo
echo "[3] 反向对照：改 dept_id，看可见行数是否跟着变"
t() { python3 -c "import json;d=json.load(open('/tmp/c2_$1.json'));print(d.get('biz_task'),d.get('biz_project'))" 2>/dev/null || echo "?"; }
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id=NULL WHERE id='$TASK'" >/dev/null
probe "dept_id=NULL（生产历史现状）" null >/dev/null; echo "  dept=NULL → task/project total = $(t null)"
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id='$TECH' WHERE id='$TASK'" >/dev/null
probe "dept_id=TECH" tech >/dev/null; echo "  dept=TECH → task/project total = $(t tech)"
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id='$FIN' WHERE id='$TASK'" >/dev/null
probe "dept_id=FIN" fin >/dev/null; echo "  dept=FIN  → task/project total = $(t fin)"
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id=NULL WHERE id='$TASK'" >/dev/null
probe "dept_id=NULL(还原)" null2 >/dev/null; echo "  dept=NULL(还原) → task/project total = $(t null2)"

check "NULL → 0 行（历史任务 dept_id 全 NULL）" "0 0" "$(t null)"
check "TECH → 该任务可见（1 行）" "1 0" "$(t tech)"
check "FIN → 0 行（子树外不可见）" "0 0" "$(t fin)"

echo
echo "[4] 拆探针（副本净零）"
"${PSQL[@]}" <<'SQL'
DELETE FROM sys_user_role WHERE role_id = '5c0e0000-0000-4000-8000-0000000000aa'
     OR id = '5c0e0000-0000-4000-8000-0000000000ab';
DELETE FROM sys_role_menu WHERE role_id = '5c0e0000-0000-4000-8000-0000000000aa';
DELETE FROM sys_role WHERE id = '5c0e0000-0000-4000-8000-0000000000aa';
SQL
check "probe 残留行" "0" "$(q "SELECT (SELECT count(*) FROM sys_user_role WHERE role_id='5c0e0000-0000-4000-8000-0000000000aa')
   + (SELECT count(*) FROM sys_role_menu WHERE role_id='5c0e0000-0000-4000-8000-0000000000aa')
   + (SELECT count(*) FROM sys_role WHERE id='5c0e0000-0000-4000-8000-0000000000aa')")"
check "biz_task.dept_id 非空（还原后）" "0" "$(q "SELECT count(*) FROM biz_task WHERE dept_id IS NOT NULL")"

echo
echo "== C2 演练结果：FAIL=$FAIL =="
exit "$FAIL"
