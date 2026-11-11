#!/usr/bin/env bash
# ============================================================
# VTRACE-6：凭证溯源「单据 → 审批 → 凭证 → 分录 → 跳回原单据」端到端验收
#
# 一条命令跑完整链路（真实 curl + 真实库断言 + 真实前端映射解析）：
#
#   bash scripts/verify/vtrace_e2e.sh                     # 默认跑全部 6 类
#   bash scripts/verify/vtrace_e2e.sh --keep               # 保留本轮夹具（排查用）
#   bash scripts/verify/vtrace_e2e.sh --only PURCHASE      # 只跑某一类（可重复）
#
# ---------------------------------------------------------------------------
# 覆盖的 6 类：卡面词 → 真实 `biz_voucher.source_type`
# ---------------------------------------------------------------------------
# 卡面（VTRACE-6 body）列的 PURCHASE / RETURN / EXPENSE **不是** `biz_voucher.source_type`
# 的取值（它们是 `biz_material_lot.source_type`、成本归集来源等别的轴上的词，对照表见
# `frontend/src/views/business/erp/shared/source-route.ts` 文件头与
# `backend/src/business/shared/doc_kind.rs` 的 `VoucherSourceType`）。本脚本按「业务单据 →
# 凭证来源」的确定映射取最贴近的真实值，**不跳类型**：
#
#   | 卡面词        | 真实 source_type | 业务单据 / 生成路径                              |
#   |--------------|-----------------|-----------------------------------------------|
#   | PURCHASE     | STOCK_IN        | 采购订单 → 审批 → 收货单过账（voucher.rs:1553）   |
#   | RETURN       | STOCK_OUT       | 出库单过账（退货出库 / 领用，同一来源值）          |
#   | EXPENSE      | EXPENSE_REPORT  | 报销单审批通过自动入账（expense_bridge.rs:433）    |
#   | DEPRECIATION | DEPRECIATION    | 折旧批次计提（depreciation_model.rs:456）         |
#   | COST_PERIOD  | COST_PERIOD     | 成本核算结算（cost_settle_voucher.rs:89）        |
#   | MANUAL       | MANUAL          | 手工凭证（无来源单据，source_id 恒为空）           |
#
# 其中 5 类走**真实业务接口**建单并过账（脚本自建主数据夹具，不靠库里既有演示数据）；
# COST_PERIOD 例外：结算前置是「成本计算单」(#27 calc)，需要 BOM / 工艺路线 /
# 人工与制造费用归集一整套夹具（数小时级），故本卡用**显式夹具**（单据走 API 建，
# 凭证按 cost_settle_voucher 的落库形态插入），脚本在证据里把这条标成 `[FIXTURE]`，
# 不冒充真实结算。真实结算链由 COST-B3-6 的真库测试覆盖。
#
# ---------------------------------------------------------------------------
# 每类断言什么（4 条硬指标）
# ---------------------------------------------------------------------------
# 1. 单据真的过账了（状态机在库里是 POSTED / APPROVED）；
# 2. `GET /business/erp/voucher/by-source` 能按 (source_type, source_id) 反查到凭证；
# 3. `GET /business/flow/instance/by-biz` 能反查到审批单（MANUAL / COST_PERIOD 除外）；
# 4. `GET /business/erp/voucher/{id}` 的单头合计 == 分录合计（借贷平衡）；
# 最后统一跑 `frontend/scripts/vtrace-jump-paths.ts`：把每条真实 (source_type,
# source_id, source_no) 喂给 `resolveSourceRoute()`，断言解析出的前端路径 / query 合法。
#
# ---------------------------------------------------------------------------
# 运行前提（隔离实例，别对生产库跑）
# ---------------------------------------------------------------------------
#   createdb -O alun cloudpivot_vtrace6
#   env $(cat /root/vtrace3.env) NIK_DATABASE_NAME=cloudpivot_vtrace6 \
#       backend/target/debug/cloudpivot &        # 自动迁移 + 种子（admin / 权限码 / 科目）
#
# 环境变量（都有默认值，默认对齐上面的隔离实例）：
#   VTRACE_BASE          默认 http://127.0.0.1:18120/api
#   VTRACE_DB_NAME       默认 cloudpivot_vtrace6
#   VTRACE_DB_HOST/USER/PASSWORD  默认 127.0.0.1 / alun / alun
#   VTRACE_ADMIN / VTRACE_ADMIN_PASSWORD  默认 admin / Admin@123456
#   VTRACE_OUT           证据目录，默认 docs/evidence/2026-11-11-voucher-trace
#
# 退出码 = 是否有断言失败（0 = 全过）。失败时日志里直接给出 `[FAIL] STEP n …`，
# 用 `--only <类>` 可单独复跑那一段。
# ============================================================
set -uo pipefail

BASE="${VTRACE_BASE:-http://127.0.0.1:18120/api}"
DBN="${VTRACE_DB_NAME:-cloudpivot_vtrace6}"
DBH="${VTRACE_DB_HOST:-127.0.0.1}"
DBU="${VTRACE_DB_USER:-alun}"
DBPASS="${VTRACE_DB_PASSWORD:-alun}"
ADMIN_USER="${VTRACE_ADMIN:-admin}"
ADMIN_PASS="${VTRACE_ADMIN_PASSWORD:-Admin@123456}"
OUT="${VTRACE_OUT:-docs/evidence/2026-11-11-voucher-trace}"

KEEP=0
ONLY=()
while [ $# -gt 0 ]; do
    case "$1" in
        --keep) KEEP=1 ;;
        --only) ONLY+=("$2"); shift ;;
        --only=*) ONLY+=("${1#*=}") ;;
        -h|--help) sed -n '2,60p' "$0"; exit 0 ;;
        *) echo "未知参数：$1（-h 看用法）" >&2; exit 2 ;;
    esac
    shift
done

# 本轮夹具主数据（带 E6VT 前缀，与其它卡不撞）
UNIT="e6000001-0000-4000-8000-000000000001"
SUPPLIER="e6000001-0000-4000-8000-000000000002"
WAREHOUSE="e6000001-0000-4000-8000-000000000003"
MATERIAL="e6000001-0000-4000-8000-000000000011"
APPROVER_ID="e6000001-0000-4000-8000-000000000101"
APPROVER_USER="vtrace6.approver"

PASS=0
FAIL=0
TOKEN=""
ATOKEN=""
REQ_CODE=0
REQ_BODY=""

# ---------- 断言 ----------
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !(a + 0 == b + 0) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}
assert_contains() {
    case "$1" in
        *"$2"*) ok "$3" ;;
        *) bad "$3：文本里没有 [$2]，实际 [$1]" ;;
    esac
}
assert_true() { if [ "$1" = "true" ]; then ok "$2"; else bad "$2：期望 true 实际 [$1]"; fi; }

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
psql_x() { PGPASSWORD="$DBPASS" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 -qc "$1" 2>&1 | head -3; }

# 发请求：REQ_CODE / REQ_BODY 写回全局（**不要用 $(...) 包**，子 shell 里赋值会丢）
call() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $TOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}
# 审批人身份发请求（内控：制单人不能审自己的单据）
acall() {
    local method="$1" path="$2" body="${3:-}" tmp
    tmp=$(mktemp)
    if [ -n "$body" ]; then
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" -H 'Content-Type: application/json' \
            --data-binary "$body" "$BASE$path")
    else
        REQ_CODE=$(curl -s -o "$tmp" -w '%{http_code}' -X "$method" \
            -H "Authorization: Bearer $ATOKEN" "$BASE$path")
    fi
    REQ_BODY=$(cat "$tmp")
    rm -f "$tmp"
}
jqv() { printf '%s' "$REQ_BODY" | jq -r "$1" 2>/dev/null; }
evidence() { printf '%s\n' "$REQ_BODY" > "$OUT/$1"; }

step() { echo; echo "==> $*"; }
wanted() { [ ${#ONLY[@]} -eq 0 ] && return 0; local k; for k in "${ONLY[@]}"; do [ "$k" = "$1" ] && return 0; done; return 1; }

# 登录（首登改密的库会回 must_change_password=true —— 见下面的处理）
# ⚠️ 登录体写到全局文件：`TOKEN=$(login ...)` 是子 shell，函数里给 REQ_BODY 赋值传不出来。
LOGIN_BODY_FILE=$(mktemp)
login() {
    curl -s -o "$LOGIN_BODY_FILE" -X POST -H 'Content-Type: application/json' \
        --data-binary "{\"username\":\"$1\",\"password\":\"$2\"}" \
        "$BASE/system/auth/login"
    jq -r '.data.access_token // empty' < "$LOGIN_BODY_FILE" 2>/dev/null
}

# 是不是一个 uuid（psql 取不到值时返回的是 `ERROR: ...` 文本，不能当真 id 用）
is_uuid() {
    [[ "${1:-}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]]
}

# 把某张审批单当前 PENDING 的待办批掉（按 instance_id）
approve_pending_of() {
    local instance_id="$1" task_id
    is_uuid "$instance_id" || { REQ_CODE=000; return; }
    task_id=$(psql_q "SELECT id FROM biz_flow_task WHERE instance_id = '$instance_id'::uuid
                       AND status = 'PENDING' ORDER BY created_at LIMIT 1")
    is_uuid "$task_id" || { REQ_CODE=000; return; }
    acall POST "/business/flow/task/$task_id/approve" '{"comment":"VTRACE-6 溯源验收：复核通过"}'
}

mkdir -p "$OUT"
LOG="$OUT/00-run.log"
: > "$LOG"
exec > >(tee -a "$LOG") 2>&1

echo "VTRACE-6 凭证溯源端到端验收"
echo "  base=$BASE  db=$DBN  out=$OUT  $(date '+%F %T %Z')"

# ============================================================
# STEP 0：环境体检 + 登录 + 审批人夹具
# ============================================================
step "STEP 0 环境体检 / 登录 / 审批人夹具（内控：制单人不能自审）"
for bin in curl jq psql; do
    command -v "$bin" >/dev/null || { echo "缺少命令：$bin"; exit 2; }
done
HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "${BASE%/api}/health")
if [ "$HEALTH" != "200" ]; then
    bad "后端探针 ${BASE%/api}/health = $HEALTH（实例没起来？见脚本头『运行前提』）"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"; exit 1
fi
ok "后端探针 200"

# 口令候选：原始口令 → 派生口令（首登改密过的库用后者；重跑自动命中）
TOKEN=""
for CAND in "$ADMIN_PASS" "${ADMIN_PASS}!vt6"; do
    TOKEN=$(login "$ADMIN_USER" "$CAND")
    MUST=$(jq -r '.data.must_change_password // false' < "$LOGIN_BODY_FILE" 2>/dev/null)
    if [ -n "$TOKEN" ]; then
        ADMIN_PASS="$CAND"
        break
    fi
done
if [ -n "$TOKEN" ] && [ "$MUST" = "true" ]; then
    # 空库首启（迁移种子的 admin）处于「首次登录必须先修改初始密码」：
    # 登录能拿到 token，但 session_guard 会把业务接口一律挡成 403 —— 先改密再登一次。
    # 新口令由 VTRACE_ADMIN_PASSWORD 派生，脚本可重跑（第二次直接命中派生口令）。
    NEW_PASS="${ADMIN_PASS}!vt6"
    curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/system/auth/password"
    ADMIN_PASS="$NEW_PASS"
    TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS")
    MUST=$(jq -r '.data.must_change_password // false' < "$LOGIN_BODY_FILE" 2>/dev/null)
    ok "首登改密（$ADMIN_USER 口令已轮换为派生口令，重跑会自动走新口令）"
fi
if [ -z "$TOKEN" ] || [ "$MUST" = "true" ]; then
    bad "登录失败或仍处于待改密状态：$(cat "$LOGIN_BODY_FILE")"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"; exit 1
fi
ok "登录 admin 拿到令牌（$ADMIN_USER）"
cp "$LOGIN_BODY_FILE" "$OUT/01-login.json"

ADMIN_ID=$(psql_q "SELECT id FROM sys_user WHERE username = '$ADMIN_USER' ORDER BY created_at LIMIT 1")
ADMIN_DEPT=$(psql_q "SELECT dept_id FROM sys_user WHERE id = '$ADMIN_ID'::uuid")
[ -n "$ADMIN_DEPT" ] || { bad "admin 没有归属部门，报销流程无法解析审批人"; echo "==> 汇总：PASS=$PASS FAIL=$FAIL"; exit 1; }
DEPT_LEADER_ORIG=$(psql_q "SELECT coalesce(leader_id::text,'') FROM sys_dept WHERE id = '$ADMIN_DEPT'::uuid")

# 审批人夹具：口令哈希抄 admin（这样同一个口令能登进来），角色给齐本链路要用的审批角色
psql_x "INSERT INTO sys_user (id, username, password, real_name, status, dept_id)
        SELECT '$APPROVER_ID'::uuid, '$APPROVER_USER', password, 'VTRACE6 审批人', 1, '$ADMIN_DEPT'::uuid
          FROM sys_user WHERE id = '$ADMIN_ID'::uuid
        ON CONFLICT (id) DO UPDATE SET password = EXCLUDED.password, status = 1, dept_id = EXCLUDED.dept_id" >/dev/null
psql_x "DELETE FROM sys_user_role WHERE user_id = '$APPROVER_ID'::uuid" >/dev/null
psql_x "INSERT INTO sys_user_role (user_id, role_id)
        SELECT '$APPROVER_ID'::uuid, id FROM sys_role
         WHERE role_code IN ('DEPT_LEADER','WAREHOUSE_MANAGER','SUPER_ADMIN','FINANCE_MANAGER')
           AND deleted_at IS NULL ON CONFLICT DO NOTHING" >/dev/null
psql_x "UPDATE sys_dept SET leader_id = '$APPROVER_ID'::uuid WHERE id = '$ADMIN_DEPT'::uuid" >/dev/null
ATOKEN=$(login "$APPROVER_USER" "$ADMIN_PASS")
[ -n "$ATOKEN" ] && ok "审批人夹具登录成功（$APPROVER_USER，直属上级=审批人，制单人=admin）" \
                 || bad "审批人夹具登录失败"

on_exit() {
    psql_x "UPDATE sys_dept SET leader_id = ${DEPT_LEADER_ORIG:+'$DEPT_LEADER_ORIG'::uuid}${DEPT_LEADER_ORIG:-NULL} WHERE id = '$ADMIN_DEPT'::uuid" >/dev/null 2>&1
    psql_x "DELETE FROM sys_user_role WHERE user_id = '$APPROVER_ID'::uuid" >/dev/null 2>&1
    psql_x "DELETE FROM sys_user WHERE id = '$APPROVER_ID'::uuid" >/dev/null 2>&1
    if [ "$KEEP" -eq 1 ]; then
        echo "--keep：保留本轮夹具（主数据 E6VT-* / 单据 / 凭证）。"
    fi
}
trap on_exit EXIT

# ============================================================
# STEP 1  PURCHASE → STOCK_IN：采购单 → 审批 → 收货过账 → 凭证 → 反查 → 全链路
# ============================================================
IN_ID=""; IN_NO=""; IN_VID=""; PO_ID=""; PO_NO=""
if wanted PURCHASE; then
    step "STEP 1 PURCHASE → STOCK_IN（采购单 → 审批 → 收货过账 → 凭证 → 反查 → 跳转）"
    # 主数据夹具（编码带 E6VT 前缀，ON CONFLICT DO NOTHING 让重跑幂等）
    psql_x "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
            VALUES ('$UNIT', 'E6VT-PCS', '个', 1, NULL, 1.000000, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
    psql_x "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
            VALUES ('$SUPPLIER', 'E6VT-SUP', 'VTRACE6 溯源供应商', 1) ON CONFLICT DO NOTHING" >/dev/null
    psql_x "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
            VALUES ('$WAREHOUSE', 'E6VT-WH', 'VTRACE6 溯源仓', '/$WAREHOUSE/', 1, 1, 1) ON CONFLICT DO NOTHING" >/dev/null
    psql_x "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
            VALUES ('$MATERIAL', 'E6VT-M1', '溯源验收物料', '$UNIT'::uuid, 1, false) ON CONFLICT DO NOTHING" >/dev/null
    assert_eq 1 "$(psql_q "SELECT count(*) FROM biz_material WHERE id = '$MATERIAL'::uuid")" "主数据夹具就位（物料 E6VT-M1）"

    call POST '/business/erp/purchase-order' \
        "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",\"remark\":\"VTRACE-6 溯源验收采购\",
          \"items\":[{\"material_id\":\"$MATERIAL\",\"qty\":\"100\",\"price_excl\":\"10\",\"tax_rate\":\"13\"}]}"
    evidence 10-purchase-order-create.json
    assert_eq 200 "$REQ_CODE" "① 建采购订单"
    PO_ID=$(jqv '.data.id // .data.header.id')
    PO_NO=$(jqv '.data.order_no // .data.header.order_no')
    is_uuid "$PO_ID" || PO_ID=$(psql_q "SELECT id FROM biz_purchase_order WHERE supplier_id = '$SUPPLIER'::uuid ORDER BY created_at DESC LIMIT 1")
    [ -n "$PO_NO" ] && [ "$PO_NO" != "null" ] || PO_NO=$(psql_q "SELECT order_no FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")
    ok "采购单 ${PO_NO}（$PO_ID）"

    call PATCH "/business/erp/purchase-order/$PO_ID/status" '{"status":"SUBMITTED"}'
    evidence 11-purchase-order-submit.json
    assert_eq 200 "$REQ_CODE" "② 提交采购单（走审批）"
    assert_eq SUBMITTED "$(jqv '.data.header.status // .data.status')" "采购单停在 SUBMITTED"
    PO_INSTANCE=$(psql_q "SELECT approval_instance_id FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")
    approve_pending_of "$PO_INSTANCE"
    evidence 12-purchase-order-approve.json
    assert_eq 200 "$REQ_CODE" "③ 审批人批掉采购单待办"
    assert_eq APPROVED "$(psql_q "SELECT status FROM biz_purchase_order WHERE id = '$PO_ID'::uuid")" "采购单变 APPROVED"

    ITEM_A=$(psql_q "SELECT id FROM biz_purchase_order_item WHERE order_id = '$PO_ID'::uuid ORDER BY line_no LIMIT 1")
    call POST '/business/erp/stock-in' \
        "{\"purchase_order_id\":\"$PO_ID\",\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
          \"invoice_received\":true,\"remark\":\"VTRACE-6 溯源验收收货\",
          \"items\":[{\"purchase_order_item_id\":\"$ITEM_A\",\"material_id\":\"$MATERIAL\",\"qty\":\"100\",\"price_excl\":\"10\",\"tax_rate\":\"13\"}]}"
    evidence 13-stock-in-create.json
    assert_eq 200 "$REQ_CODE" "④ 从采购单生成收货单"
    IN_ID=$(jqv '.data.header.id')
    IN_NO=$(jqv '.data.header.in_no')
    assert_eq DRAFT "$(jqv '.data.header.status')" "收货单落 DRAFT（不自动过账）"

    call PATCH "/business/erp/stock-in/$IN_ID/status" '{"status":"POSTED"}'
    evidence 14-stock-in-post-submit.json
    assert_eq 200 "$REQ_CODE" "⑤ 收货单提交过账"
    IN_STATUS=$(jqv '.data.header.status')
    if [ "$IN_STATUS" = "PENDING_POST" ]; then
        IN_INSTANCE=$(psql_q "SELECT approval_instance_id FROM biz_stock_in WHERE id = '$IN_ID'::uuid")
        approve_pending_of "$IN_INSTANCE"
        evidence 15-stock-in-approve.json
        assert_eq 200 "$REQ_CODE" "⑤b 仓管主管审批通过（PENDING_POST → POSTED）"
    else
        ok "⑤b 收货单免审直过（状态已 $IN_STATUS，< 1000 元的流程配置）"
    fi
    call GET "/business/erp/stock-in/$IN_ID"
    evidence 16-stock-in-detail.json
    assert_eq POSTED "$(jqv '.data.header.status')" "⑥ 收货单已过账 POSTED"
    IN_VID=$(jqv '.data.header.voucher_id')
    is_uuid "$IN_VID" || IN_VID=$(psql_q "SELECT voucher_id FROM biz_stock_in WHERE id = '$IN_ID'::uuid")
    if is_uuid "$IN_VID"; then ok "⑥b 过账生成了凭证（$IN_VID）"; else bad "过账没生成凭证"; fi

    # ---- 溯源四条硬指标 ----
    call GET "/business/erp/voucher/by-source?source_type=STOCK_IN&source_id=$IN_ID"
    evidence 17-by-source-stock-in.json
    assert_eq 200 "$REQ_CODE" "【溯源①】按来源反查收货单凭证 200"
    assert_eq 1 "$(jqv '.data | length')" "【溯源①】查到 1 张凭证"
    assert_eq "$IN_NO" "$(jqv '.data[0].source_no')" "【溯源①】凭证 source_no = 收货单号 $IN_NO"
    assert_eq "$IN_ID" "$(jqv '.data[0].source_id')" "【溯源①】凭证 source_id = 收货单 id"

    call GET "/business/flow/instance/by-biz?biz_id=$PO_ID"
    evidence 18-flow-by-biz-purchase.json
    assert_eq 200 "$REQ_CODE" "【溯源②】按业务单反查审批单 200"
    assert_eq APPROVED "$(jqv '.data.list[0].status')" "【溯源②】采购单审批已通过（全链路「审批」段有据）"

    call GET "/business/erp/voucher/$IN_VID"
    evidence 19-voucher-detail-stock-in.json
    assert_eq 200 "$REQ_CODE" "【溯源③】凭证详情 200"
    assert_true "$(jqv '.data.balanced')" "【溯源③】凭证借贷平衡（单头合计 == 分录合计）"
    assert_num "$(jqv '.data.entry_total_debit')" "$(jqv '.data.total_debit')" "【溯源③】分录借方合计 == 单头借方"
    assert_eq 3 "$(jqv '.data.entries | length')" "【溯源③】分录 3 条（借 1405 / 借 2221.02 进项税 / 贷 2202.01）"
fi

# ============================================================
# STEP 2  RETURN → STOCK_OUT：出库单过账 → 凭证 → 反查 → 跳转（列表按单号筛）
# ============================================================
OUT_ID=""; OUT_NO=""; OUT_VID=""
if wanted RETURN; then
    step "STEP 2 RETURN → STOCK_OUT（退货/领用出库单过账 → 凭证 → 反查 → 跳转）"
    call POST '/business/erp/stock-out' \
        "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"$WAREHOUSE\",\"reason\":\"VTRACE-6 溯源验收退货出库\",
          \"items\":[{\"material_id\":\"$MATERIAL\",\"qty\":\"3\"}]}"
    evidence 20-stock-out-create.json
    assert_eq 200 "$REQ_CODE" "① 建出库单（领用/退货出库共用出库单）"
    OUT_ID=$(jqv '.data.id')
    OUT_NO=$(jqv '.data.out_no')
    ok "出库单 ${OUT_NO}（$OUT_ID）"
    call PATCH "/business/erp/stock-out/$OUT_ID/status" '{"status":"POSTED"}'
    evidence 21-stock-out-post.json
    assert_eq 200 "$REQ_CODE" "② 出库单过账"
    OUT_STATUS=$(jqv '.data.status')
    if [ "$OUT_STATUS" = "PENDING_POST" ]; then
        OUT_INSTANCE=$(psql_q "SELECT id FROM biz_flow_instance WHERE biz_id = '$OUT_ID'::uuid ORDER BY created_at DESC LIMIT 1")
        approve_pending_of "$OUT_INSTANCE"
        evidence 22-stock-out-approve.json
        assert_eq 200 "$REQ_CODE" "②b 出库单审批通过"
        call GET "/business/erp/stock-out/$OUT_ID"
    fi
    assert_eq POSTED "$(jqv '.data.status')" "③ 出库单已过账 POSTED"
    OUT_VID=$(jqv '.data.voucher_id')
    is_uuid "$OUT_VID" || OUT_VID=$(psql_q "SELECT voucher_id FROM biz_stock_out WHERE id = '$OUT_ID'::uuid")

    call GET "/business/erp/voucher/by-source?source_type=STOCK_OUT&source_id=$OUT_ID"
    evidence 23-by-source-stock-out.json
    assert_eq 200 "$REQ_CODE" "【溯源①】按来源反查出库单凭证 200"
    assert_eq 1 "$(jqv '.data | length')" "【溯源①】查到 1 张凭证"
    assert_eq "$OUT_NO" "$(jqv '.data[0].source_no')" "【溯源①】凭证 source_no = 出库单号 $OUT_NO"
    call GET "/business/erp/voucher/$OUT_VID"
    evidence 24-voucher-detail-stock-out.json
    assert_true "$(jqv '.data.balanced')" "【溯源③】出库凭证借贷平衡"
fi

# ============================================================
# STEP 3  EXPENSE → EXPENSE_REPORT：报销单 → 上传发票 → 审批通过 → 自动入账凭证
# ============================================================
EXP_ID=""; EXP_NO=""; EXP_VID=""
if wanted EXPENSE; then
    step "STEP 3 EXPENSE → EXPENSE_REPORT（报销单 → 发票附件 → 审批通过 → 自动入账）"
    ETYPE=$(psql_q "SELECT id FROM biz_expense_type WHERE deleted_at IS NULL AND type_code = 'TRAVEL' LIMIT 1")
    [ -z "$ETYPE" ] && ETYPE=$(psql_q "SELECT id FROM biz_expense_type WHERE deleted_at IS NULL ORDER BY type_code LIMIT 1")
    [ -z "$ETYPE" ] && { bad "库里没有费用类型（biz_expense_type 空）：先跑 demo_seed 或建一个类型"; }
    call POST '/business/erp/expense' \
        "{\"title\":\"VTRACE-6 溯源验收差旅报销\",\"expense_date\":\"$(date +%F)\",
          \"items\":[{\"expense_type_id\":\"$ETYPE\",\"with_tax_amount\":\"500\",\"tax_rate\":\"0.03\",
                      \"summary\":\"客户走访高铁票\",\"invoice_no\":\"VTRACE6-INV\",\"invoice_type\":\"VAT_NORMAL\"}]}"
    evidence 30-expense-create.json
    assert_eq 200 "$REQ_CODE" "① 建报销单"
    EXP_ID=$(jqv '.data.report.id')
    EXP_NO=$(jqv '.data.report.report_no')
    ok "报销单 ${EXP_NO}（$EXP_ID）"
    # 报送前置：必须至少 1 个发票影像（真上传，1×1 PNG）
    TMPPNG=$(mktemp --suffix=.png)
    printf '\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR' > "$TMPPNG"
    UPLOAD_CODE=$(curl -s -o "$OUT/31-expense-attachment.json" -w '%{http_code}' -X POST \
        -H "Authorization: Bearer $TOKEN" \
        -F doc_type=EXPENSE_REPORT -F doc_id="$EXP_ID" -F file_role=INVOICE_SCAN \
        -F file=@"$TMPPNG" "$BASE/business/attachment/upload")
    rm -f "$TMPPNG"
    assert_eq 200 "$UPLOAD_CODE" "② 上传发票附件（报送前置）"
    call POST "/business/erp/expense/$EXP_ID/submit" '{}'
    evidence 32-expense-submit.json
    assert_eq 200 "$REQ_CODE" "③ 提交报销单（走审批）"
    assert_eq SUBMITTED "$(jqv '.data.report.status')" "报销单停在 SUBMITTED"
    EXP_INSTANCE=$(psql_q "SELECT approval_instance_id FROM biz_expense_report WHERE id = '$EXP_ID'::uuid")
    approve_pending_of "$EXP_INSTANCE"
    evidence 33-expense-approve.json
    assert_eq 200 "$REQ_CODE" "④ 审批人批掉报销待办"
    assert_eq APPROVED "$(psql_q "SELECT status FROM biz_expense_report WHERE id = '$EXP_ID'::uuid")" "报销单变 APPROVED"
    EXP_VID=$(psql_q "SELECT id FROM biz_voucher WHERE source_type = 'EXPENSE_REPORT' AND source_id = '$EXP_ID'::uuid AND is_red = 0 ORDER BY created_at DESC LIMIT 1")
    [ -n "$EXP_VID" ] && ok "④b 审批通过自动生成入账凭证（$EXP_VID）" || bad "审批通过没生成 EXPENSE_REPORT 凭证"

    call GET "/business/erp/voucher/by-source?source_type=EXPENSE_REPORT&source_id=$EXP_ID"
    evidence 34-by-source-expense.json
    assert_eq 200 "$REQ_CODE" "【溯源①】按来源反查报销凭证 200"
    assert_eq 1 "$(jqv '.data | length')" "【溯源①】查到 1 张凭证"
    assert_eq "$EXP_NO" "$(jqv '.data[0].source_no')" "【溯源①】凭证 source_no = 报销单号 $EXP_NO"
    call GET "/business/flow/instance/by-biz?biz_id=$EXP_ID"
    evidence 35-flow-by-biz-expense.json
    assert_eq APPROVED "$(jqv '.data.list[0].status')" "【溯源②】报销单审批已通过"
    call GET "/business/erp/voucher/$EXP_VID"
    evidence 36-voucher-detail-expense.json
    assert_true "$(jqv '.data.balanced')" "【溯源③】报销凭证借贷平衡"
fi

# ============================================================
# STEP 4  DEPRECIATION：建卡（购入凭证）→ 计提 → 折旧凭证 → 反查
# ============================================================
ASSET_ID=""; DEP_BATCH=""; DEP_NO=""; DEP_VID=""
if wanted DEPRECIATION; then
    step "STEP 4 DEPRECIATION（建资产卡 → 次月计提 → 折旧凭证 → 反查 → 跳转）"
    CATEGORY=$(psql_q "SELECT id FROM biz_asset_category WHERE category_code = 'ELEC' LIMIT 1")
    [ -z "$CATEGORY" ] && CATEGORY=$(psql_q "SELECT id FROM biz_asset_category ORDER BY category_code LIMIT 1")
    # 起提月 = 购入月 + 1（折旧政策：当月新增下月计提），期间必须是 OPEN
    NEXT_PERIOD=$(date -d "$(date +%Y-%m-01) +1 month" +%Y-%m)
    psql_x "INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
            VALUES (${NEXT_PERIOD%%-*}, ${NEXT_PERIOD##*-}, '$NEXT_PERIOD',
                    '$NEXT_PERIOD-01'::date, ('$NEXT_PERIOD-01'::date + interval '1 month - 1 day')::date, 'OPEN')
            ON CONFLICT (period_code) DO NOTHING" >/dev/null
    call POST '/business/erp/asset' \
        "{\"asset_name\":\"VTRACE6 溯源验收笔记本\",\"category_id\":\"$CATEGORY\",\"use_dept_id\":\"$ADMIN_DEPT\",
          \"original_value\":\"6000\",\"tax_amount\":\"0\",\"purchase_date\":\"$(date +%F)\",\"source_type\":\"MANUAL\"}"
    evidence 40-asset-create.json
    assert_eq 200 "$REQ_CODE" "① 建资产卡（同时生成 FIXED_ASSET_ACQ 购入凭证）"
    ASSET_ID=$(jqv '.data.id')
    ok "资产卡 $(jqv '.data.no')（$ASSET_ID）"
    call POST '/business/erp/depreciation/run' "{\"period_code\":\"$NEXT_PERIOD\",\"remark\":\"VTRACE-6 溯源验收\"}"
    evidence 41-depreciation-run.json
    assert_eq 200 "$REQ_CODE" "② 计提 $NEXT_PERIOD 折旧"
    OUTCOME=$(jqv '.data.outcome')
    if [ "$OUTCOME" = "POSTED" ]; then
        DEP_BATCH=$(jqv '.data.batch.id')
        DEP_NO=$(jqv '.data.batch.batch_no')
        DEP_VID=$(jqv '.data.batch.voucher_id')
        ok "②b 批次状态 POSTED（$DEP_NO → 凭证 $DEP_VID）"
    else
        # 同一期间只允许一个批次：重跑/并发时拿 EXISTS（或本期无资产拿 SKIPPED）。
        # 溯源断言的目的是「凭证能按批次 id 反查到」，沿用既有批次完全等价。
        DEP_BATCH=$(psql_q "SELECT id FROM biz_depreciation_batch WHERE period_code = '$NEXT_PERIOD' AND voucher_id IS NOT NULL ORDER BY created_at DESC LIMIT 1")
        DEP_NO=$(psql_q "SELECT batch_no FROM biz_depreciation_batch WHERE id = '${DEP_BATCH}'::uuid")
        DEP_VID=$(psql_q "SELECT voucher_id FROM biz_depreciation_batch WHERE id = '${DEP_BATCH}'::uuid")
        if is_uuid "$DEP_BATCH"; then
            ok "②b 期间 $NEXT_PERIOD 已有批次（outcome=$OUTCOME），沿用 $DEP_NO 做溯源断言"
        else
            bad "②b 计提既没生成批次也没找到既有批次（outcome=$OUTCOME，$(jqv '.msg')）"
        fi
    fi

    call GET "/business/erp/voucher/by-source?source_type=DEPRECIATION&source_id=$DEP_BATCH"
    evidence 42-by-source-depreciation.json
    assert_eq 200 "$REQ_CODE" "【溯源①】按来源反查折旧凭证 200"
    assert_eq 1 "$(jqv '.data | length')" "【溯源①】查到 1 张凭证"
    assert_eq "$DEP_BATCH" "$(jqv '.data[0].source_id')" "【溯源①】凭证 source_id = 折旧批次 id（跳转靠它）"
    # 既有落库口径：折旧凭证的 source_no 是**期间码**（不是批次号），source_doc_no 同值。
    # 只影响「来源单据号」这一列的文字（界面会显示 2026-12），不影响按 id 跳转；
    # 已作为发现①写进证据 README，未在本卡改业务代码（改落库值会动历史凭证）。
    assert_eq "$NEXT_PERIOD" "$(jqv '.data[0].source_no')" "【溯源①】折旧凭证 source_no = 期间码 $NEXT_PERIOD（既有口径）"
    call GET "/business/erp/voucher/$DEP_VID"
    evidence 43-voucher-detail-depreciation.json
    assert_true "$(jqv '.data.balanced')" "【溯源③】折旧凭证借贷平衡"
fi

# ============================================================
# STEP 5  COST_PERIOD：[FIXTURE] 结算批次 → 结算凭证（按 cost_settle_voucher 落库形态插入）
# ============================================================
CP_ID=""; CP_NO=""; CP_VID=""
if wanted COST_PERIOD; then
    step "STEP 5 COST_PERIOD [FIXTURE]（结算批次走 API，结算凭证按 cost_settle_voucher 形态落库）"
    THIS_PERIOD=$(date +%Y-%m)
    call POST '/business/erp/cost-period' "{\"period_code\":\"$THIS_PERIOD\",\"remark\":\"VTRACE-6 溯源验收\"}"
    evidence 50-cost-period-create.json
    CP_ID=$(jqv '.data.id')
    CP_NO=$(jqv '.data.batch_no')
    if [ "$REQ_CODE" = "200" ] && is_uuid "$CP_ID"; then
        ok "① 建成本核算批次（真实接口，$CP_NO）"
    else
        # 同一期间只允许一个批次：重跑时 409，沿用既有批次（溯源断言等价）
        CP_ID=$(psql_q "SELECT id FROM biz_cost_period WHERE period_code = '$THIS_PERIOD' ORDER BY created_at DESC LIMIT 1")
        CP_NO=$(psql_q "SELECT batch_no FROM biz_cost_period WHERE id = '$CP_ID'::uuid")
        if is_uuid "$CP_ID"; then
            ok "① 期间 $THIS_PERIOD 已有批次（$REQ_CODE），沿用 $CP_NO"
        else
            bad "① 建成本核算批次失败且找不到既有批次：$REQ_CODE $(jqv '.msg')"
        fi
    fi
    # 结算前置（#27 calc）需要 BOM / 工艺路线 / 人工与制造费用归集整套夹具，
    # 超出本卡范围：这里按 cost_settle_voucher::post_all 的落库形态直接插入
    # 一张结算凭证 + 两条分录（借 6401 主营业务成本 / 贷 1405 库存商品），
    # 只为验证「溯源查询 / 跳转映射」这一段；真实结算生成由 COST-B3-6 真库测试覆盖。
    # 重跑时先把上一轮的夹具凭证清掉（同一批次只留一张，source_no 断言才稳定）。
    psql_x "DELETE FROM biz_voucher_entry WHERE voucher_id IN
              (SELECT id FROM biz_voucher WHERE source_type = 'COST_PERIOD' AND source_id = '$CP_ID'::uuid)" >/dev/null
    psql_x "DELETE FROM biz_voucher WHERE source_type = 'COST_PERIOD' AND source_id = '$CP_ID'::uuid" >/dev/null
    ACC_D=$(psql_q "SELECT id FROM biz_account WHERE account_code = '6401' LIMIT 1")
    ACC_C=$(psql_q "SELECT id FROM biz_account WHERE account_code = '1405' LIMIT 1")
    [ -z "$ACC_C" ] && ACC_C=$(psql_q "SELECT id FROM biz_account WHERE account_code = '1405' LIMIT 1")
    CP_VID=$(psql_q "INSERT INTO biz_voucher
        (voucher_no, period, voucher_date, voucher_type, source_type, source_id, source_no, source_doc_no,
         summary, total_debit, total_credit, entry_count, status, is_red, posted_at)
        VALUES ('CSF-$(date +%H%M%S)', '$THIS_PERIOD', '$THIS_PERIOD-28', 'AUTO', 'COST_PERIOD', '$CP_ID'::uuid,
                '$CP_NO', '$CP_NO', '成本核算结算：完工入库结转（VTRACE-6 夹具）',
                1000.0000, 1000.0000, 2, 'POSTED', 0, now())
        RETURNING id")
    psql_x "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount)
            SELECT '$CP_VID'::uuid, 1, '成本核算结算：完工入库结转', id, account_code, account_name, 1000.0000
              FROM biz_account WHERE id = '$ACC_D'::uuid" >/dev/null
    psql_x "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, credit_amount)
            SELECT '$CP_VID'::uuid, 2, '成本核算结算：完工入库结转', id, account_code, account_name, 1000.0000
              FROM biz_account WHERE id = '$ACC_C'::uuid" >/dev/null

    call GET "/business/erp/voucher/by-source?source_type=COST_PERIOD&source_id=$CP_ID"
    evidence 51-by-source-cost-period.json
    assert_eq 200 "$REQ_CODE" "【溯源①】按来源反查结算凭证 200"
    assert_eq 1 "$(jqv '.data | length')" "【溯源①】查到 1 张凭证"
    assert_eq "$CP_NO" "$(jqv '.data[0].source_no')" "【溯源①】凭证 source_no = 批次号 $CP_NO"
    call GET "/business/erp/voucher/$CP_VID"
    evidence 52-voucher-detail-cost-period.json
    assert_true "$(jqv '.data.balanced')" "【溯源③】结算凭证借贷平衡"
fi

# ============================================================
# STEP 6  MANUAL：手工凭证（无来源单据）→ 制单/审核分离 → 反查为空是设计事实
# ============================================================
MAN_VID=""; MAN_NO=""
if wanted MANUAL; then
    step "STEP 6 MANUAL（手工凭证：source_id 恒空 ⇒ 按来源反查必然为空，属设计）"
    ACC_A=$(psql_q "SELECT id FROM biz_account WHERE account_code = '1002' LIMIT 1")
    ACC_B=$(psql_q "SELECT id FROM biz_account WHERE account_code = '6401' LIMIT 1")
    call POST '/business/erp/voucher' \
        "{\"voucher_date\":\"$(date +%F)\",\"summary\":\"VTRACE-6 溯源验收手工凭证\",
          \"entries\":[{\"account_id\":\"$ACC_A\",\"debit\":\"100\"},{\"account_id\":\"$ACC_B\",\"credit\":\"100\"}]}"
    evidence 60-manual-voucher-create.json
    assert_eq 200 "$REQ_CODE" "① 建手工凭证（DRAFT）"
    MAN_VID=$(jqv '.data.id')
    MAN_NO=$(jqv '.data.voucher_no')
    assert_eq MANUAL "$(jqv '.data.source_type')" "①b 来源类型 MANUAL"
    assert_eq null "$(jqv '.data.source_id')" "①c 手工凭证 source_id 为空（无来源单据）"
    ok "手工凭证 ${MAN_NO}（$MAN_VID）"
    acall POST "/business/erp/voucher/$MAN_VID/audit" '{"remark":"VTRACE-6 审核"}'
    evidence 61-manual-voucher-audit.json
    assert_eq 200 "$REQ_CODE" "② 审批人审核（内控：制单人不能自审）"
    acall POST "/business/erp/voucher/$MAN_VID/post" '{}'
    evidence 62-manual-voucher-post.json
    assert_eq 200 "$REQ_CODE" "③ 记账"
    assert_eq POSTED "$(jqv '.data.status // .data.voucher.status')" "③b 凭证已记账 POSTED"

    call GET "/business/erp/voucher/by-source?source_type=MANUAL&source_id=$MAN_VID"
    evidence 63-by-source-manual.json
    assert_eq 200 "$REQ_CODE" "【溯源①】MANUAL 反查接口可用（不 400）"
    assert_eq 0 "$(jqv '.data | length')" "【溯源①】返回空数组 —— 手工凭证无来源单据，按来源查不到是设计事实"
    call GET "/business/erp/voucher/page?voucher_no=$(printf '%s' "$MAN_NO" | jq -sRr @uri)&page=1&page_size=10"
    evidence 64-voucher-page-manual.json
    assert_eq 1 "$(jqv '.data.total')" "【溯源①'】手工凭证从凭证列表能查到（溯源兜底路径）"
fi

# ============================================================
# STEP 7  跳转路径校验（真数据 → resolveSourceRoute()）
# ============================================================
step "STEP 7 跳转链路校验（把本轮真实 id / 单号喂给 resolveSourceRoute）"
CASES="$OUT/70-jump-cases.json"
jq -n \
  --arg in_id "$IN_ID" --arg in_no "$IN_NO" \
  --arg out_id "$OUT_ID" --arg out_no "$OUT_NO" \
  --arg exp_id "$EXP_ID" --arg exp_no "$EXP_NO" \
  --arg dep_id "$DEP_BATCH" --arg dep_no "$DEP_NO" \
  --arg cp_id "$CP_ID" --arg cp_no "$CP_NO" \
  --arg man_no "$MAN_NO" \
  '{require_mapping: ["STOCK_IN","STOCK_OUT","EXPENSE_REPORT","DEPRECIATION","COST_PERIOD","MANUAL"],
    cases: [
      {key:"PURCHASE", source_type:"STOCK_IN", source_id:$in_id, source_no:$in_no,
       resolvable:($in_id != ""), expect_path:"/business/erp/stock-in",
       expect_query:{in_id:$in_id}, expect_deep_link:"detail"},
      {key:"RETURN", source_type:"STOCK_OUT", source_id:$out_id, source_no:$out_no,
       resolvable:($out_id != ""), expect_path:"/business/erp/stock-out",
       expect_query:{out_no:$out_no}, expect_deep_link:"filter"},
      {key:"EXPENSE", source_type:"EXPENSE_REPORT", source_id:$exp_id, source_no:$exp_no,
       resolvable:($exp_id != ""), expect_path:"/business/erp/expense",
       expect_query:{}, expect_deep_link:"none"},
      {key:"DEPRECIATION", source_type:"DEPRECIATION", source_id:$dep_id, source_no:$dep_no,
       resolvable:($dep_id != ""), expect_path:"/business/erp/depreciation",
       expect_query:{}, expect_deep_link:"none"},
      {key:"COST_PERIOD", source_type:"COST_PERIOD", source_id:$cp_id, source_no:$cp_no,
       resolvable:false, note:"页面/路由未落地（page-missing）⇒ 不给假链接"},
      {key:"MANUAL", source_type:"MANUAL", source_id:null, source_no:$man_no,
       resolvable:false, note:"手工凭证，无来源单据 ⇒ 不可跳"}
    ]}' > "$CASES"
JUMP_LOG="$OUT/71-jump-paths.log"
( cd "$(dirname "$0")/../../frontend" && pnpm exec tsx scripts/vtrace-jump-paths.ts "$(cd "$(dirname "$CASES")" && pwd)/$(basename "$CASES")" ) > "$JUMP_LOG" 2>&1
JUMP_EXIT=$?
cat "$JUMP_LOG"
if [ "$JUMP_EXIT" -eq 0 ]; then
    ok "STEP 7 跳转链路校验全过（详见 $(basename "$JUMP_LOG")）"
else
    bad "STEP 7 跳转链路校验失败（逐条见 $JUMP_LOG）"
fi

# ============================================================
# STEP 8  清理 + 残留核对
# ============================================================
step "STEP 8 清理本轮夹具（--keep 可保留）"
if [ "$KEEP" -eq 0 ]; then
    # 只把**真 uuid** 拼进 IN (...)（取不到值时 psql_q 返回的是 `ERROR: …` 文本）
    uuids_csv() {
        local out="" v
        for v in "$@"; do is_uuid "$v" && out="${out}${out:+,}'$v'"; done
        printf '%s' "$out"
    }
    ACQ_VID=""
    is_uuid "$ASSET_ID" && ACQ_VID=$(psql_q "SELECT id FROM biz_voucher WHERE source_type = 'FIXED_ASSET_ACQ' AND source_id = '$ASSET_ID'::uuid ORDER BY created_at DESC LIMIT 1")
    VIDS=$(uuids_csv "$IN_VID" "$OUT_VID" "$EXP_VID" "$DEP_VID" "$CP_VID" "$MAN_VID" "$ACQ_VID")
    DOCS=$(uuids_csv "$IN_ID" "$PO_ID" "$OUT_ID" "$EXP_ID" "$DEP_BATCH" "$CP_ID" "$ASSET_ID" "$MAN_VID" "$ACQ_VID")
    q() { psql_x "$1" >/dev/null 2>&1 || echo "  （清理未命中：$2）"; }
    qv() { is_uuid "$2" || return 0; q "$1" "$3"; }   # 单 id 版：id 不是 uuid 就直接跳过
    if [ -n "$DOCS" ]; then
        q "DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN ($DOCS))" "flow_task"
        q "DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN ($DOCS))" "flow_node"
        q "DELETE FROM biz_flow_instance WHERE biz_id IN ($DOCS)" "flow_instance"
        q "DELETE FROM biz_status_log WHERE entity_id IN ($DOCS)" "status_log"
    fi
    [ -n "$VIDS" ] && {
        q "DELETE FROM biz_voucher_entry WHERE voucher_id IN ($VIDS)" "voucher_entry"
        q "DELETE FROM biz_voucher WHERE id IN ($VIDS)" "voucher"
    }
    qv "DELETE FROM biz_doc_attachment WHERE doc_type = 'EXPENSE_REPORT' AND doc_id = '$EXP_ID'::uuid" "$EXP_ID" "attachment"
    qv "DELETE FROM biz_expense_payment WHERE report_id = '$EXP_ID'::uuid" "$EXP_ID" "expense_payment"
    qv "DELETE FROM biz_expense_item WHERE report_id = '$EXP_ID'::uuid" "$EXP_ID" "expense_item"
    qv "DELETE FROM biz_expense_report WHERE id = '$EXP_ID'::uuid" "$EXP_ID" "expense_report"
    qv "DELETE FROM biz_payable WHERE source_id = '$IN_ID'::uuid" "$IN_ID" "payable"
    qv "DELETE FROM biz_inventory_txn WHERE doc_id = '$IN_ID'::uuid" "$IN_ID" "inventory_txn(in)"
    qv "DELETE FROM biz_inventory_txn WHERE doc_id = '$OUT_ID'::uuid" "$OUT_ID" "inventory_txn(out)"
    qv "DELETE FROM biz_depreciation WHERE batch_id = '$DEP_BATCH'::uuid" "$DEP_BATCH" "depreciation_rows"
    qv "DELETE FROM biz_depreciation_batch WHERE id = '$DEP_BATCH'::uuid" "$DEP_BATCH" "depreciation_batch"
    qv "DELETE FROM biz_fixed_asset_change WHERE asset_id = '$ASSET_ID'::uuid" "$ASSET_ID" "asset_change"
    qv "DELETE FROM biz_fixed_asset WHERE id = '$ASSET_ID'::uuid" "$ASSET_ID" "asset"
    qv "DELETE FROM biz_cost_period_item WHERE period_id = '$CP_ID'::uuid" "$CP_ID" "cost_period_item"
    qv "DELETE FROM biz_cost_period WHERE id = '$CP_ID'::uuid" "$CP_ID" "cost_period"
    qv "DELETE FROM biz_stock_in_item WHERE stock_in_id = '$IN_ID'::uuid" "$IN_ID" "stock_in_item"
    qv "DELETE FROM biz_stock_in WHERE id = '$IN_ID'::uuid" "$IN_ID" "stock_in"
    qv "DELETE FROM biz_stock_out_item WHERE stock_out_id = '$OUT_ID'::uuid" "$OUT_ID" "stock_out_item"
    qv "DELETE FROM biz_stock_out WHERE id = '$OUT_ID'::uuid" "$OUT_ID" "stock_out"
    qv "DELETE FROM biz_purchase_order_item WHERE order_id = '$PO_ID'::uuid" "$PO_ID" "po_item"
    qv "DELETE FROM biz_purchase_order WHERE id = '$PO_ID'::uuid" "$PO_ID" "purchase_order"
    q "DELETE FROM biz_inventory_location WHERE material_id = '$MATERIAL'::uuid" "inventory_location"
    q "DELETE FROM biz_inventory WHERE material_id = '$MATERIAL'::uuid" "inventory"
    q "DELETE FROM biz_material WHERE id = '$MATERIAL'::uuid" "material"
    q "DELETE FROM biz_warehouse WHERE id = '$WAREHOUSE'::uuid" "warehouse"
    q "DELETE FROM biz_supplier WHERE id = '$SUPPLIER'::uuid" "supplier"
    q "DELETE FROM biz_unit WHERE id = '$UNIT'::uuid" "unit"

    LEFT=$(psql_q "SELECT (SELECT count(*) FROM biz_voucher WHERE id IN (${VIDS:-'00000000-0000-0000-0000-000000000000'::uuid}))
                        + (SELECT count(*) FROM biz_purchase_order WHERE id = '$(is_uuid "$PO_ID" && echo "$PO_ID" || echo 00000000-0000-0000-0000-000000000000)'::uuid)
                        + (SELECT count(*) FROM biz_material WHERE id = '$MATERIAL'::uuid)
                        + (SELECT count(*) FROM biz_flow_instance WHERE biz_id IN (${DOCS:-'00000000-0000-0000-0000-000000000000'::uuid}))")
    assert_eq 0 "$LEFT" "残留核对 = 0（凭证 / 采购单 / 主数据 / 审批单）"
else
    echo "  --keep：跳过清理"
fi

step "汇总"
echo "PASS=$PASS FAIL=$FAIL  （证据目录：$OUT）"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
