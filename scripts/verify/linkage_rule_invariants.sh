#!/usr/bin/env bash
# =============================================================================
# linkage_rule_invariants.sh —— 跨单据联动规则的**覆盖率不变量**（E2E-07 · FIX-2）
#
# 验什么（4 条断言，任何一条有输出即 FAIL、退出码 1）
#
#   ① wired 触发点必须至少 1 条 enabled 规则
#      —— 「接线了但零规则 = 这个触发点的联动能力不存在」（E2E-07 的 CONTRACT、
#      FIX-3 的 FIXED_ASSET、TST-01 的 PURCHASE_RETURN 都是这个病）。
#   ② enabled 规则不能挂在**未接线**的触发点上
#      —— 配得上、能启用、永远不触发（P1-FIX-5C 那个坑）。未接线实体见
#      `linkage::spec::TRIGGER_EXEMPT`（销售链那批：SO / SHIPMENT / RECEIVABLE …）。
#   ③ enabled 规则的 (触发点, 状态) 必须是该触发点真实存在的状态
#      —— 状态写错（改名 / 手抖 / 迁移漏改）的规则永远等不到触发。
#      状态表逐字对应 `spec::TRIGGER_SPECS[].status_labels`，由单测
#      `tests_presets_coverage::invariants_script_status_table_matches_spec` 双向钉住。
#   ④ enabled 规则的动作必须**已接管**，且不是该实体结构性被禁的动作
#      —— 占位动作（`implemented = false`）配得上但执行时 Skipped；
#      结构性被禁（合同出凭证 / 凭证生凭证 / 审批实例落账）配置期就该 400，
#      库里出现只可能是绕过 API 的脏行，运行时一律被排除 = 配了不跑。
#
# 用法
#   bash scripts/verify/linkage_rule_invariants.sh                  # 读环境变量 / dev env 文件
#   bash scripts/verify/linkage_rule_invariants.sh <db>             # 指定库名
#   NIK_DATABASE_NAME=cloudpivot_dev bash scripts/verify/linkage_rule_invariants.sh
#
# 退出码：0 = 4 条全绿；1 = 至少一条红（逐条点名）；2 = 连不上库 / 表不存在。
#
# ⚠ 不要拿它当「迁移有没有跑」的检查：表不存在是 rc=2（不能说全绿）。
# =============================================================================
set -uo pipefail

ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
if [ -f "$ENV_FILE" ]; then
  # shellcheck disable=SC1090
  set -a; . "$ENV_FILE"; set +a
fi
DB_ARG="${1:-}"
[ -n "$DB_ARG" ] && export NIK_DATABASE_NAME="$DB_ARG"

: "${NIK_DATABASE_HOST:=127.0.0.1}"
: "${NIK_DATABASE_PORT:=5432}"
: "${NIK_DATABASE_USER:=alun}"
: "${NIK_DATABASE_NAME:=alun}"
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-$NIK_DATABASE_USER}"

PSQL=(psql -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" -U "$NIK_DATABASE_USER" \
      -d "$NIK_DATABASE_NAME" -X -q -tA -F'|')

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }

# ---- 表在不在（不在 = rc 2，不能算绿） -------------------------------------
if ! "${PSQL[@]}" -c "SELECT 1 FROM biz_flow_linkage_rule LIMIT 1" >/dev/null 2>&1; then
  echo "❌ 表 biz_flow_linkage_rule 不存在或连不上库（$NIK_DATABASE_USER@$NIK_DATABASE_HOST:$NIK_DATABASE_PORT/$NIK_DATABASE_NAME）"
  exit 2
fi

echo "== 联动规则不变量（库 $NIK_DATABASE_NAME）=="

# ---- 覆盖表（验收用；不是断言） --------------------------------------------
echo "  -- 触发点规则覆盖 --"
"${PSQL[@]}" -c "
SELECT t.entity || ' | rules=' || COALESCE(r.total, 0) || ' | enabled=' || COALESCE(r.alive, 0)
  FROM (VALUES
    ('STOCK_IN'), ('STOCK_OUT'), ('STOCK_TRANSFER'), ('PURCHASE_ORDER'),
    ('PAYABLE'), ('VOUCHER'), ('ACCOUNTING_PERIOD'), ('ACCOUNTING_YEAR'),
    ('CONTRACT'), ('STOCK_TAKE'), ('FIXED_ASSET'), ('DEPRECIATION'),
    ('EXPENSE_REPORT'), ('PURCHASE_RETURN'), ('TASK'), ('FLOW_INSTANCE')
  ) t(entity)
  LEFT JOIN (
    SELECT trigger_entity,
           count(*) AS total,
           count(*) FILTER (WHERE enabled AND deleted_at IS NULL) AS alive
      FROM biz_flow_linkage_rule GROUP BY trigger_entity
  ) r ON r.trigger_entity = t.entity
 ORDER BY COALESCE(r.alive, 0), t.entity;" | sed 's/^/     /'

# ---- ① wired ⇒ ≥1 enabled 规则 ---------------------------------------------
# BEGIN WIRED_TRIGGERS
WIRED_TRIGGERS="
STOCK_IN
STOCK_OUT
STOCK_TRANSFER
PURCHASE_ORDER
PAYABLE
VOUCHER
ACCOUNTING_PERIOD
ACCOUNTING_YEAR
CONTRACT
STOCK_TAKE
FIXED_ASSET
DEPRECIATION
EXPENSE_REPORT
PURCHASE_RETURN
TASK
FLOW_INSTANCE
"
# END WIRED_TRIGGERS

VALUES_LIST=$(printf "('%s')," $WIRED_TRIGGERS | sed 's/,$//')
OUT=$("${PSQL[@]}" -c "
SELECT t.entity
  FROM (VALUES $VALUES_LIST) t(entity)
 WHERE NOT EXISTS (
   SELECT 1 FROM biz_flow_linkage_rule r
    WHERE r.trigger_entity = t.entity AND r.enabled AND r.deleted_at IS NULL)
 ORDER BY t.entity;")
if [ -z "$OUT" ]; then
  ok "① 每个 wired 触发点都有 ≥1 条 enabled 规则"
else
  bad "① 以下 wired 触发点**没有** enabled 规则（接线了但能力不存在）：$(echo "$OUT" | tr '\n' ' ')"
fi

# ---- ② 规则不能挂在未接线触发点上 ------------------------------------------
OUT=$("${PSQL[@]}" -c "
SELECT DISTINCT r.trigger_entity
  FROM biz_flow_linkage_rule r
 WHERE r.deleted_at IS NULL AND r.enabled
   AND r.trigger_entity NOT IN ($(printf "'%s'," $WIRED_TRIGGERS | sed 's/,$//'))
 ORDER BY 1;")
if [ -z "$OUT" ]; then
  ok "② enabled 规则全部挂在已接线的触发点上"
else
  bad "② 以下规则挂在**未接线**触发点上（配了永远不触发）：$(echo "$OUT" | tr '\n' ' ')"
fi

# ---- ③ (触发点, 状态) 必须是真实状态 ---------------------------------------
# BEGIN STATUS_TABLE
STATUS_PAIRS="
STOCK_IN|DRAFT
STOCK_IN|PENDING_POST
STOCK_IN|POSTED
STOCK_IN|PENDING_CANCEL
STOCK_IN|CANCELLED
STOCK_OUT|DRAFT
STOCK_OUT|PENDING_POST
STOCK_OUT|POSTED
STOCK_OUT|CANCELLED
STOCK_TRANSFER|DRAFT
STOCK_TRANSFER|PENDING
STOCK_TRANSFER|APPROVED
STOCK_TRANSFER|IN_TRANSIT
STOCK_TRANSFER|DONE
STOCK_TRANSFER|CANCELLED
PURCHASE_ORDER|DRAFT
PURCHASE_ORDER|SUBMITTED
PURCHASE_ORDER|APPROVED
PURCHASE_ORDER|PARTIAL_RECEIVED
PURCHASE_ORDER|RECEIVED
PURCHASE_ORDER|CLOSED
PURCHASE_ORDER|CANCELLED
PAYABLE|OPEN
PAYABLE|PARTIAL_PAID
PAYABLE|SETTLED
PAYABLE|PENDING_CANCEL
PAYABLE|CANCELLED
VOUCHER|DRAFT
VOUCHER|AUDITED
VOUCHER|POSTED
ACCOUNTING_PERIOD|NOT_OPENED
ACCOUNTING_PERIOD|OPEN
ACCOUNTING_PERIOD|CLOSED
ACCOUNTING_YEAR|NOT_CLOSED
ACCOUNTING_YEAR|CLOSED
CONTRACT|DRAFT
CONTRACT|PENDING_APPROVAL
CONTRACT|SIGNED
CONTRACT|ACTIVE
CONTRACT|COMPLETED
CONTRACT|TERMINATED
STOCK_TAKE|DRAFT
STOCK_TAKE|PENDING_POST
STOCK_TAKE|POSTED
STOCK_TAKE|CANCELLED
FIXED_ASSET|IN_USE
FIXED_ASSET|IDLE
FIXED_ASSET|DISPOSED
DEPRECIATION|CALCULATED
DEPRECIATION|POSTED
DEPRECIATION|REVERSED
DEPRECIATION|SKIPPED
EXPENSE_REPORT|DRAFT
EXPENSE_REPORT|SUBMITTED
EXPENSE_REPORT|APPROVED
EXPENSE_REPORT|PAID
EXPENSE_REPORT|CANCELLED
PURCHASE_RETURN|DRAFT
PURCHASE_RETURN|SUBMITTED
PURCHASE_RETURN|APPROVED
PURCHASE_RETURN|CONFIRMED
PURCHASE_RETURN|CANCELLED
TASK|TODO
TASK|DOING
TASK|REVIEW
TASK|DONE
TASK|CANCELLED
FLOW_INSTANCE|APPROVED
FLOW_INSTANCE|REJECTED
FLOW_INSTANCE|WITHDRAWN
"
# END STATUS_TABLE

STATUS_VALUES=$(echo "$STATUS_PAIRS" | grep -v '^$' | awk -F'|' '{printf "('\''%s'\'', '\''%s'\''),", $1, $2}' | sed 's/,$//')
OUT=$("${PSQL[@]}" -c "
SELECT r.trigger_entity || '/' || r.trigger_status
  FROM biz_flow_linkage_rule r
 WHERE r.deleted_at IS NULL AND r.enabled
   AND NOT EXISTS (
     SELECT 1 FROM (VALUES $STATUS_VALUES) v(entity, status)
      WHERE v.entity = r.trigger_entity AND v.status = r.trigger_status)
 ORDER BY 1;")
if [ -z "$OUT" ]; then
  ok "③ enabled 规则的状态全部是该触发点的真实状态"
else
  bad "③ 以下规则的状态在触发点状态集里不存在（永远不触发）：$(echo "$OUT" | tr '\n' ' ')"
fi

# ---- ④ 动作必须已接管、且不是结构性被禁 --------------------------------
# BEGIN IMPLEMENTED_ACTIONS
IMPLEMENTED_ACTIONS="
SEND_NOTICE
GENERATE_VOUCHER
UPDATE_RECEIVED_QTY
CREATE_TASK
"
# END IMPLEMENTED_ACTIONS

# BEGIN FORBIDDEN_PAIRS
FORBIDDEN_PAIRS="
CONTRACT|GENERATE_VOUCHER
VOUCHER|GENERATE_VOUCHER
FLOW_INSTANCE|WRITE_INVENTORY
FLOW_INSTANCE|WRITE_INVENTORY_TXN
FLOW_INSTANCE|UPDATE_COST
FLOW_INSTANCE|GENERATE_VOUCHER
FLOW_INSTANCE|REVERSE_VOUCHER
FLOW_INSTANCE|UPDATE_RECEIVED_QTY
FLOW_INSTANCE|UPDATE_PAYABLE_BALANCE
FLOW_INSTANCE|UPDATE_DOC_STATUS
FLOW_INSTANCE|LOCK_DOCUMENT
"
# END FORBIDDEN_PAIRS

FORBIDDEN_VALUES=$(echo "$FORBIDDEN_PAIRS" | grep -v '^$' | awk -F'|' '{printf "('\''%s'\'', '\''%s'\''),", $1, $2}' | sed 's/,$//')
IMPLEMENTED_VALUES=$(printf "'%s'," $IMPLEMENTED_ACTIONS | sed 's/,$//')
OUT=$("${PSQL[@]}" -c "
SELECT r.trigger_entity || '/' || r.trigger_status || '/' || r.action_code || ' (' ||
       CASE WHEN r.action_code NOT IN ($IMPLEMENTED_VALUES) THEN '占位动作' ELSE '结构性被禁' END || ')'
  FROM biz_flow_linkage_rule r
 WHERE r.deleted_at IS NULL AND r.enabled
   AND (r.action_code NOT IN ($IMPLEMENTED_VALUES)
        OR EXISTS (SELECT 1 FROM (VALUES $FORBIDDEN_VALUES) f(entity, action)
                    WHERE f.entity = r.trigger_entity AND f.action = r.action_code))
 ORDER BY 1;")
if [ -z "$OUT" ]; then
  ok "④ enabled 规则的动作全部已接管、且不是结构性被禁的"
else
  bad "④ 以下规则配了也不会跑：$(echo "$OUT" | tr '\n' ' ')"
fi

echo
echo "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
