#!/usr/bin/env bash
# ============================================================================
# m304_quotation_approval_e2e.sh —— M3-04【M3·销售】报价单：审批流接线 E2E
#
# 卡：t_6bede75a（报价单审批流接线 —— 走 flow，不硬编码）
# 上游：M3-01（V0432 建表）/ M3-02（V0433 权限码 + 状态机）/ M3-03（V0434 转单）
#
# 验的是「一条链路」，全部打**真接口 + 真库 + 真权限 + 真角色**：
#
# ```text
#   空库首启（服务自己跑完 435 条迁移 + bootstrap，V0435 种 quotation_approve 流程）
#        ↓
#   库侧：流程种子形态（PUBLISHED / 5 节点 / 5 边 / 2 审批人 / hash 与行复算一致）
#        ↓
#   夹具：制单员（同时在 SALES_SUPERVISOR 里 → 测内控）/ 销售主管 / 财务经理
#        ↓
#   小额单：提交审批（单据仍 DRAFT）→ 审批中四道闸 → 销售主管批 → **自动发出 SENT**
#        ↓
#   大额单（≥10 万）：销售主管批完仍 DRAFT（等财务）→ 财务批完 → SENT
#        ↓
#   驳回 → 留在 DRAFT → 可改 → 可重提（新实例）
#        ↓
#   作废 → 同一事务撤掉在跑实例（不留孤儿待办）
#        ↓
#   没配流程：409 + 66033 → 跳过理由太短 400 → 持 skip 权限 + 长理由 → 直接发出
#        ↓
#   落库形态：biz_flow_instance / biz_flow_task(SKIPPED 留痕) / biz_status_log
# ```
#
# 用法：
#   bash scripts/verify/m304_quotation_approval_e2e.sh              # 99 开发机 → PG 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m304_quotation_approval_e2e.sh   # 在 145 上跑
#   KEEP_DB=1 …      留库排查（打印库名）
#   SKIP_BUILD=1 …   复用已有 target/debug/cloudpivot（不重编）
#   M304_PORT=18232  换端口（默认 18232）
#
# 约定（AGENTS.md / 卡面）：
#   · 库建在 PG_HOST（99 上默认 145，145 上默认 127.0.0.1）；
#   · 库名含 e2e，脚本自己 DROP；用户统一 cloudpivot；
#   · **空库首启**：不用模板库 —— 服务自己跑完全部迁移 + bootstrap，admin 口令即
#     NIK_ADMIN_PASSWORD（首登强制改密，脚本里走真接口改）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERIFY="$ROOT/scripts/verify"
BIN="$ROOT/backend/target/debug/cloudpivot"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
DB="cp_m304_approval_e2e_$$"
PORT="${M304_PORT:-18232}"
BASE="http://127.0.0.1:$PORT"
ADMIN_PASSWORD="${M304_ADMIN_OLD_PASSWORD:-Admin@123456}"
NEW_ADMIN_PASSWORD="${M304_ADMIN_PASSWORD:-M304@Approve1}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/m304.XXXXXX")"
LOG="$TMP/instance.log"
PASS=0
FAIL=0
INSTANCE_PID=""

ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }

cleanup() {
    if [ -n "$INSTANCE_PID" ]; then
        kill "$INSTANCE_PID" >/dev/null 2>&1 || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$INSTANCE_PID" >/dev/null 2>&1 || break
            sleep 1
        done
        kill -9 "$INSTANCE_PID" >/dev/null 2>&1 || true
    fi
    if [ "$KEEP_DB" = "1" ]; then
        printf '\n  （KEEP_DB=1：演练库保留 → %s；实例日志 %s）\n' "$DB" "$LOG"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT

echo "== M3-04 报价单审批流接线 E2E =="
echo "   PG=${PG_HOST}:${PG_PORT}  演练库=${DB}  服务=${BASE}"
echo

# ---------- 0) 前置 ----------
case "$DB" in
    *drill*|*test*|*e2e*) ok "库名合规（含 e2e）" ;;
    *) bad "库名不含 drill/test/e2e，拒绝执行"; exit 1 ;;
esac
if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    bad "端口 $PORT 已被占用（换 M304_PORT 或先停掉占用进程）"; exit 1
else
    ok "端口 $PORT 空闲"
fi
if [ "$SKIP_BUILD" != "1" ]; then
    echo "  ---- cargo build（debug，确保二进制与当前源码一致）"
    if ( cd "$ROOT/backend" && cargo build >"$TMP/build.log" 2>&1 ); then
        ok "cargo build 成功"
    else
        bad "cargo build 失败（tail: $(tail -3 "$TMP/build.log" | tr '\n' ' ')）"; exit 1
    fi
fi
[ -x "$BIN" ] && ok "二进制在位（$BIN）" || { bad "二进制不存在：$BIN"; exit 1; }

# ---------- 1) 空库首启 ----------
echo
echo "== 1) 空库首启（全部迁移 + bootstrap + V0435 种报价单审批流程）"
dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1 || true
if psql_admin "CREATE DATABASE $DB" >/dev/null 2>&1; then
    ok "建空库 $DB"
else
    bad "建库失败（$(psql_admin "CREATE DATABASE $DB")）"; exit 1
fi

JWT_SECRET="$(openssl rand -hex 32)"
STORAGE_KEY="$(openssl rand -hex 32)"
CONFIG_KEY="$(openssl rand -hex 32)"

(
    cd "$ROOT/backend" || exit 1
    NIK_PROFILE=test \
    NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" \
    NIK_DATABASE_USER="$PG_USER" NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
    NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET="$JWT_SECRET" \
    CLOUDPIVOT_STORAGE_MASTER_KEY="$STORAGE_KEY" \
    X_CONFIG_SECRET_KEY="$CONFIG_KEY" \
    NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    exec "$BIN"
) >"$LOG" 2>&1 &
INSTANCE_PID=$!

READY=0
for _ in $(seq 1 120); do
    sleep 3
    if ! kill -0 "$INSTANCE_PID" >/dev/null 2>&1; then
        bad "服务启动即退出（tail: $(tail -8 "$LOG" | tr '\n' ' ')）"; exit 1
    fi
    if curl -s -m 3 "$BASE/health" | grep -q '"status":"ok"'; then READY=1; break; fi
done
if [ "$READY" = "1" ]; then
    ok "服务就绪（$BASE/health，pid $INSTANCE_PID）"
else
    bad "服务 360 秒未就绪（tail: $(tail -8 "$LOG" | tr '\n' ' ')）"; exit 1
fi
MIGRATIONS="$(psql_q "SELECT count(*) FROM schema_migrations WHERE success")"
MAX_VERSION="$(psql_q "SELECT max(version::int) FROM schema_migrations WHERE success AND version ~ '^[0-9]+$'")"
if [ "${MIGRATIONS:-0}" -ge 360 ]; then
    ok "迁移全部应用（schema_migrations.success = $MIGRATIONS，最大版本 = $MAX_VERSION）"
else
    bad "迁移数异常（success = ${MIGRATIONS:-?}，应 ≥ 360）"
fi
assert_eq "435" "$MAX_VERSION" "最大已应用版本 = 435（本卡 V0435 是最后一号）"

# ---------- 2) V0435 落地形态（SQL 直查） ----------
echo
echo "== 2) V0435 流程种子形态（SQL 直查）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM information_schema.columns
    WHERE table_name='biz_quotation' AND column_name='approval_instance_id'")" \
    "biz_quotation 加出 approval_instance_id（幂等 ADD COLUMN IF NOT EXISTS）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_indexes
    WHERE schemaname='public' AND tablename='biz_quotation' AND indexname='idx_biz_quotation_approval'")" \
    "部分索引 idx_biz_quotation_approval 就位"
assert_eq "quotation_approve/QUOTATION_APPROVE/PUBLISHED/true" "$(psql_q "SELECT flow_code||'/'||biz_type||'/'||status||'/'||is_default
    FROM biz_flow_def WHERE flow_code='quotation_approve' AND deleted_at IS NULL")" \
    "流程定义 quotation_approve 已发布且是默认流程"
assert_eq "5" "$(psql_q "SELECT count(*) FROM biz_flow_node n JOIN biz_flow_def_version v ON v.id=n.version_id
    WHERE v.def_id='20000000-0000-4000-8000-0000000d0431'::uuid")" "节点 5 个"
assert_eq "5" "$(psql_q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_def_version v ON v.id=e.version_id
    WHERE v.def_id='20000000-0000-4000-8000-0000000d0431'::uuid")" "边 5 条"
assert_eq "2" "$(psql_q "SELECT count(*) FROM biz_flow_node_assignee a
    JOIN biz_flow_node n ON n.id=a.node_id JOIN biz_flow_def_version v ON v.id=n.version_id
    WHERE v.def_id='20000000-0000-4000-8000-0000000d0431'::uuid")" "审批人 2 行（销售主管 / 财务经理）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_def_version v ON v.id=e.version_id
    WHERE v.def_id='20000000-0000-4000-8000-0000000d0431'::uuid
      AND e.condition_expr->>'field'='form.amount' AND (e.condition_expr->>'value')::numeric=100000")" \
    "金额分级写在**条件边**上（form.amount < 100000），不是代码 if/else"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_def_version v ON v.id=e.version_id
    WHERE v.def_id='20000000-0000-4000-8000-0000000d0431'::uuid
      AND e.condition_expr->>'field' <> 'form.amount' AND e.condition_expr IS NOT NULL")" \
    "没有别的条件边（阈值只有一个，改阈值改流程定义即可）"

# hash 与行复算一致（口算不得：把行重建快照 → canonical JSON → sha256，与每日自检同源）
if PGPASSWORD="$PGPASSWORD" PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" \
        python3 "$VERIFY/m304_rows_hash.py" "$DB" '20000000-0000-4000-8000-0000000d0431' \
        >"$TMP/hash.log" 2>&1 && grep -q 'drifted = 0' "$TMP/hash.log"; then
    ok "graph_hash 与行复算一致（m304_rows_hash.py drifted = 0）"
else
    bad "graph_hash 与行复算不一致（种出了会被每日自检报 SnapshotEdited 的图）"
    sed 's/^/     /' "$TMP/hash.log" | head -8
fi

# ---------- 3) 夹具：角色 / 账号 ----------
echo
echo "== 3) 夹具（角色 + 三个账号；审批人是真角色成员，不是超管）"
# 审批中心入口：待办列表 business:flow:instance:read / 审批动作 business:flow:approve
psql_q "INSERT INTO sys_role (role_code, role_name, description, status, sort_order)
        VALUES ('M304_APPROVER','M304 审批人','M3-04 验收夹具',1,92) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id FROM sys_role r, sys_menu m
         WHERE r.role_code='M304_APPROVER' AND m.deleted_at IS NULL
           AND m.auth_code IN ('business:flow:approve','business:flow:instance:read','business:flow:read',
                               'business:flow:admin')
        ON CONFLICT DO NOTHING" >/dev/null
# 制单员：报价单全套 + 审批中心入口 + **skip-approval**（⑧ 显式跳过要它）
psql_q "INSERT INTO sys_role (role_code, role_name, description, status, sort_order)
        VALUES ('M304_MAKER','M304 制单员','M3-04 验收夹具',1,93) ON CONFLICT DO NOTHING" >/dev/null
psql_q "INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, m.id FROM sys_role r, sys_menu m
         WHERE r.role_code='M304_MAKER' AND m.deleted_at IS NULL
           AND m.auth_code IN ('business:erp:quotation:list','business:erp:quotation:create',
                               'business:erp:quotation:update','business:erp:quotation:delete',
                               'business:erp:quotation:status','business:erp:quotation:convert',
                               'business:erp:customer:list','business:erp:customer:create',
                               'business:erp:material:list','business:erp:material:create',
                               'business:erp:unit:list',
                               'business:erp:skip-approval',
                               'business:flow:instance:read','business:flow:read')
        ON CONFLICT DO NOTHING" >/dev/null

q() { psql_q "$1" >/dev/null; }
# 账号口令 = admin 的（脚本里 admin 首登会改密，这里取改密后的哈希）
for u in m304_maker m304_sales m304_fin; do
    q "INSERT INTO sys_user (username, password, real_name, must_change_password, status)
       VALUES ('$u', (SELECT password FROM sys_user WHERE username='admin'), '$u', false, 1)
       ON CONFLICT DO NOTHING"
done
# 制单员 → M304_MAKER + **SALES_SUPERVISOR**（故意：让「制单人不可自审」这条闸真的被撞到）
q "INSERT INTO sys_user_role (user_id, role_id)
   SELECT u.id, r.id FROM sys_user u, sys_role r
    WHERE u.username='m304_maker' AND r.role_code IN ('M304_MAKER','SALES_SUPERVISOR','M304_APPROVER')
   ON CONFLICT DO NOTHING"
q "INSERT INTO sys_user_role (user_id, role_id)
   SELECT u.id, r.id FROM sys_user u, sys_role r
    WHERE u.username='m304_sales' AND r.role_code IN ('SALES_SUPERVISOR','M304_APPROVER')
   ON CONFLICT DO NOTHING"
q "INSERT INTO sys_user_role (user_id, role_id)
   SELECT u.id, r.id FROM sys_user u, sys_role r
    WHERE u.username='m304_fin' AND r.role_code IN ('FINANCE_MANAGER','M304_APPROVER')
   ON CONFLICT DO NOTHING"
# 两个审批角色只留夹具成员（否则「派给谁」不可判定 —— 共用库里可能有别的账号）
q "DELETE FROM sys_user_role WHERE role_id IN
     (SELECT id FROM sys_role WHERE role_code IN ('SALES_SUPERVISOR','FINANCE_MANAGER'))
    AND user_id NOT IN (SELECT id FROM sys_user WHERE username IN ('m304_maker','m304_sales','m304_fin'))"

assert_eq "2" "$(psql_q "SELECT count(*) FROM sys_role WHERE role_code IN ('SALES_SUPERVISOR','FINANCE_MANAGER') AND deleted_at IS NULL")" \
    "两个审批角色都在（SALES_SUPERVISOR 由 V0281 种，FINANCE_MANAGER 由 V0191 种，本卡不重复种）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu
    WHERE menu_id IN (SELECT id FROM sys_menu WHERE auth_code LIKE 'business:erp:quotation:%')
      AND role_id IN (SELECT id FROM sys_role WHERE role_code IN ('SALES_SUPERVISOR','FINANCE_MANAGER'))")" \
    "迁移不静默扩权（报价单码没有自动授给审批角色 —— 派人是派待办，不是给单据权限）"

# ---------- 4) 真接口断言 ----------
echo
echo "== 4) 真接口断言（提交 / 闸 / 内控 / 批 / 自动发出 / 驳回 / 作废 / 没配流程）"
if M304_API_BASE="$BASE/api" M304_ADMIN_OLD_PASSWORD="$ADMIN_PASSWORD" \
   M304_ADMIN_PASSWORD="$NEW_ADMIN_PASSWORD" M304_USER_PASSWORD="$NEW_ADMIN_PASSWORD" \
   PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" PGDATABASE="$DB" PGPASSWORD="$PGPASSWORD" \
   python3 "$VERIFY/m304_quotation_approval_e2e.py" >"$TMP/api.log" 2>&1; then
    API_PASS="$(grep -c '^  PASS ' "$TMP/api.log")"
    ok "真接口断言全绿（$API_PASS 条）"
else
    bad "真接口断言有失败（见下方逐条输出）"
fi
grep -E '^  (PASS|FAIL) ' "$TMP/api.log" | sed 's/^/     /'
tail -3 "$TMP/api.log" | grep -E '====' | sed 's/^/     /'
if grep -q '^  FAIL ' "$TMP/api.log"; then
    info "api.log 尾部："
    tail -20 "$TMP/api.log" | sed 's/^/     /'
fi

# ---------- 5) 落库形态 ----------
echo
echo "== 5) 落库形态（实例 / 待办 / 状态日志）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance
    WHERE biz_type='QUOTATION_APPROVE' AND biz_id IS NULL")" \
    "所有报价单审批实例都挂着 biz_id（反查得到单据）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance
    WHERE biz_type='QUOTATION_APPROVE' AND biz_no IS NULL")" \
    "实例都带单号（审计能人读）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_task t JOIN biz_flow_instance i ON i.id=t.instance_id
    WHERE i.biz_type='QUOTATION_APPROVE' AND t.status='PENDING' AND i.status <> 'PENDING'")" \
    "没有「实例已结束但待办还挂着」的孤儿"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_quotation q
    WHERE q.deleted_at IS NULL AND q.approval_instance_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i WHERE i.id = q.approval_instance_id)")" \
    "approval_instance_id 都能反查到实例（弱引用不悬空）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance i
    WHERE i.biz_type='QUOTATION_APPROVE'
      AND NOT EXISTS (SELECT 1 FROM biz_flow_task t JOIN sys_user u ON u.id=t.assignee_id
                       WHERE t.instance_id=i.id AND t.status='SKIPPED' AND u.username='m304_maker')")" \
    "内控留痕：**每一轮**实例都给制单员落了 SKIPPED 行（制单人不可自审，可追溯）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_task t
    WHERE t.status='SKIPPED' AND coalesce(t.comment,'') NOT LIKE '%制单人%'
      AND t.instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_type='QUOTATION_APPROVE')")" \
    "SKIPPED 行都写明原因（没有静默排除）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND to_status='SENT' AND coalesce(reason,'')=''")" \
    "每条「发出」轨迹都有 reason（谁批的 / 为什么跳过）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log
    WHERE entity_type='QUOTATION' AND to_status='SENT' AND reason NOT LIKE '%审批%'
      AND reason NOT LIKE '%跳过%'")" \
    "『发出』的 reason 只有两种来源：审批通过 / 显式跳过（没有第三条暗路）"
info "实例：$(psql_q "SELECT string_agg(status||':'||coalesce(biz_no,'-'), ' / ' ORDER BY created_at)
    FROM biz_flow_instance WHERE biz_type='QUOTATION_APPROVE'")"
info "单据：$(psql_q "SELECT string_agg(quote_no||'='||status, ' / ' ORDER BY quote_no)
    FROM biz_quotation WHERE deleted_at IS NULL")"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_quotation q
    WHERE q.deleted_at IS NULL AND q.status IN ('SENT','ACCEPTED')
      AND NOT EXISTS (SELECT 1 FROM biz_flow_instance i
                       WHERE i.biz_type='QUOTATION_APPROVE' AND i.biz_id=q.id AND i.status='APPROVED')
      AND NOT EXISTS (SELECT 1 FROM biz_status_log l
                       WHERE l.entity_type='QUOTATION' AND l.entity_id=q.id
                         AND l.to_status='SENT' AND coalesce(l.reason,'') LIKE '%跳过审批%')")" \
    "凡已发出的单，要么有 APPROVED 实例，要么有显式跳过的轨迹（不存在两条都没有的）"

# ---------- 6) 菜单结构不变量 ----------
echo
echo "== 6) 菜单结构不变量（menu_invariants.sh / 8 条）"
if bash "$VERIFY/menu_invariants.sh" "$DB" >"$TMP/menu.log" 2>&1; then
    ok "menu_invariants 8 条全绿（本卡不新增菜单行：提交审批复用 quotation:status 码）"
else
    bad "menu_invariants 报红：$(grep -E '✗|FAIL|违规' "$TMP/menu.log" | head -5 | tr '\n' ' ')"
fi

echo
echo "==== M3-04 演练结果：PASS=$PASS FAIL=$FAIL ===="
[ "$FAIL" -eq 0 ] || exit 1
