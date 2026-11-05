#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 验收 C2 的数据范围演练（**只许在隔离副本上跑**）
# ----------------------------------------------------------------------------
# 生产上 laoliu_test 是 status=0（登录在密码校验前就被 403 AUTH_ACCOUNT_DISABLED 拒），
# 所以「真机登录看他能看什么」只能在 prod dump 的副本上做。本脚本就是那套演练：
#
#   1) rollback-revoke → 真登录 → 记 before（他是超管，任务/项目/用户都能看）
#   2) apply-revoke    → 真登录 → 记 after （他没有 business:task:read ⇒ 403）
#   3) 临时建一个 data_scope=3 的探针角色（只含 business:task:read + business:project:read）绑给 laoliu_test，
#      让「任务/项目列表」这两个接口**可调用**，从而把「部门子树过滤」这件事露出来；
#   4) 反向对照：挑一条任务 dept_id 置 TECH → 他能看到；改 FIN → 立刻看不到；再还原 NULL
#   5) 拆掉探针角色与绑定（副本里净零）
#
# 安全闸：DB 名不匹配 SUPERCONV_ISO_DB（默认 cloudpivot_superconv_c1）直接退出 1 ——
#         防止有人把 FORCE 环境指到生产库上跑（本脚本会 UPDATE biz_task.dept_id）。
#
# 用法：
#   set -a; . /root/superconv-run/iso.env; set +a
#   NIK_ADMIN_PASSWORD=*** bash scripts/datafix/superconv/verify_c2_scope.sh
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

ISO_DB="${SUPERCONV_ISO_DB:-cloudpivot_superconv_c1}"
DB_NAME="${SUPERCONV_DB:-${NIK_DATABASE_NAME:-}}"
if [ "$DB_NAME" != "$ISO_DB" ]; then
    echo "✗ 拒跑：SUPERCONV_DB=$DB_NAME，本脚本只允许在隔离副本 $ISO_DB 上跑" >&2
    exit 1
fi
: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_*}"
: "${NIK_ADMIN_PASSWORD:?需要 NIK_ADMIN_PASSWORD（副本里 laoliu_test 的口令已被设成 admin 的哈希，见 README）}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${SUPERCONV_BASE:-http://127.0.0.1:18097}"
ISO_PORT="${BASE##*:}"
ISO_RUN_DIR="${ISO_RUN_DIR:-/root/superconv-run}"

PSQL=(psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
           -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -X -q -v ON_ERROR_STOP=1)

q() { "${PSQL[@]}" -At -F'|' -c "$1"; }

echo "== C2 演练 · $DB_NAME @ $BASE =="

# ---------------------------------------------------------------- 1) before
echo
echo "[1] 回退摘权 → laoliu_test 恢复超管 → 真登录看可见行数"
bash "$HERE/apply.sh" --rollback-revoke >/dev/null
# 隔离实例的权限缓存是**进程内**的（启动日志 `cache_type=local`，不是 Redis），
# 300s 内会返回改绑前的旧权限码 → 每次改完绑定都重启一次，保证读到的是库里的真值。
restart_iso() {
    local pid
    pid=$(ss -ltnp 2>/dev/null | grep "127.0.0.1:${ISO_PORT}" | grep -oP 'pid=\K[0-9]+' | head -1 || true)
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; sleep 2; fi
    ( cd "$ISO_RUN_DIR" && setsid bash start_iso.sh >> backend.log 2>&1 < /dev/null & )
    for _ in $(seq 1 60); do
        [ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")" = "200" ] && return 0
        sleep 1
    done
    echo "✗ 隔离实例重启后 health 非 200" >&2; exit 1
}
restart_iso
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "before(超管)" | tee /tmp/c2_before.json

# ---------------------------------------------------------------- 2) after
echo
echo "[2] 重新摘权 → 同一账号再登录"
bash "$HERE/apply.sh" --revoke >/dev/null
restart_iso
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "after(摘超管)" | tee /tmp/c2_after.json || true

# ------------------------------------------- 3) 探针角色：只补一个任务列表权限码
echo
echo "[3] 临时探针角色（data_scope=3 + business:task:list，仅副本内，尾步拆除）"
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
restart_iso
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "after+task:list(TECH 子树, 历史任务 dept_id 全 NULL)" | tee /tmp/c2_subtree0.json

# ------------------------------------------------------- 4) 反向对照（dept_id 因果）
TECH=$(q "SELECT id FROM sys_dept WHERE dept_code='TECH' AND deleted_at IS NULL")
FIN=$(q "SELECT id FROM sys_dept WHERE dept_code='FIN' AND deleted_at IS NULL")
TASK=$(q "SELECT id FROM biz_task WHERE dept_id IS NULL AND deleted_at IS NULL ORDER BY created_at LIMIT 1")
echo
echo "[4] 反向对照：任务 $TASK 的 dept_id 依次置 TECH → FIN → 还原 NULL"
echo -n "    dept_id=TECH  : "
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id='$TECH' WHERE id='$TASK'" >/dev/null
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "dept_id=TECH" | tee /tmp/c2_tech.json
echo -n "    dept_id=FIN   : "
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id='$FIN' WHERE id='$TASK'" >/dev/null
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "dept_id=FIN" | tee /tmp/c2_fin.json
echo -n "    dept_id=NULL  : "
"${PSQL[@]}" -c "UPDATE biz_task SET dept_id=NULL WHERE id='$TASK'" >/dev/null
SUPERCONV_BASE=$BASE python3 "$HERE/probe_scope_c2.py" --username laoliu_test \
    --password "$NIK_ADMIN_PASSWORD" --label "dept_id=NULL(还原)" | tee /tmp/c2_null.json

# ------------------------------------------------------------ 5) 拆探针（净零）
echo
echo "[5] 拆探针角色 / 绑定（副本净零）"
"${PSQL[@]}" <<'SQL'
DELETE FROM sys_user_role WHERE role_id = '5c0e0000-0000-4000-8000-0000000000aa'
     OR id = '5c0e0000-0000-4000-8000-0000000000ab';
DELETE FROM sys_role_menu WHERE role_id = '5c0e0000-0000-4000-8000-0000000000aa';
DELETE FROM sys_role WHERE id = '5c0e0000-0000-4000-8000-0000000000aa';
SQL
q "SELECT '  残留 probe 行 = ' || (SELECT count(*) FROM sys_user_role WHERE role_id='5c0e0000-0000-4000-8000-0000000000aa')
     + (SELECT count(*) FROM sys_role_menu WHERE role_id='5c0e0000-0000-4000-8000-0000000000aa')
     + (SELECT count(*) FROM sys_role WHERE id='5c0e0000-0000-4000-8000-0000000000aa')"
q "SELECT '  biz_task 仍为 NULL dept 的行数 = ' || count(*) FROM biz_task WHERE dept_id IS NULL"
echo "== C2 演练结束 =="
