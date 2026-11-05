#!/usr/bin/env bash
# ======================================================================
# P16 验证脚本：收货单作废审批（六本账复核）
#
# 依据卡面 t_f1d0564f §三「验收（真机）」。
#
# 它做什么（六段，每段一条命令能单独复跑）：
#
#   ① 结构与种子自检   biz_stock_in.approval_instance_id / ck_biz_si_status 4 态 /
#                     ck_biz_si_pending_cancel / 种子流程 stock_in_cancel（PUBLISHED + 默认）
#   ② 单测（16 条 P16 + P9 的 12 条作废回归）  走真库（ERP_DB_URL）
#   ③ 六本账回滚实证   选一张真实作废过的收货单，逐本账核对「已归零 / 红字凭证 1 张 /
#                     反向流水 1 条 / 序列号全软删 / 应付 CANCELLED」
#   ④ 反向：孤儿实例   审批还在跑、单据已不处于作废审批中 → 必须被回收（P11 教训）
#   ⑤ 不变量套件       全绿，BLOCKER 0
# ⑥ 前端类型检查     vue-tsc --noEmit（新状态 / 新接口不引入类型错误）
#
# ⚠️ 走查数据的坑（本卡实测）：②段的单测会按夹具供应商（`P2B1-SUP`）清掉收货单，
#    如果真机走查也用那个供应商，单据被清走而它留下的库存/流水会成为孤儿行，
#    让 ⑤段的不变量出 B2/B4/E4。**走查请用别的供应商（如 `SUP-0001`）**。
#    同理：本脚本自己不动数据，③段只是「读最近一张审批通过作废的单」。
#
# 用法：
#
#   P16_DB_URL='postgres://user:pw@127.0.0.1:5432/cloudpivot_p16' \
#     bash scripts/verify/p16_stockin_cancel_approval.sh
#
#   bash scripts/verify/p16_stockin_cancel_approval.sh --skip-frontend   # 只跑后端
#
# 环境变量：
#   P16_DB_URL      收货单所在的库（**必须是已应用 V0174 的库**）。缺省时依次回退
#                   ERP_DB_URL → NIK_DATABASE_* 拼出来的串；都没有就**跳过**真库段（退出码 2）。
#   ERP_DB_URL      单测用的库（缺省 = P16_DB_URL）
#   P16_PSQL        psql 可执行文件（默认 psql）
#
# 退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足（没库 / 没 cargo）。
# ======================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PSQL="${P16_PSQL:-psql}"
SKIP_FE=0
for arg in "$@"; do
  case "$arg" in
    --skip-frontend) SKIP_FE=1 ;;
    -h|--help) sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
  esac
done

FAIL=0
pass() { printf '  \033[32m✅\033[0m %s\n' "$1"; }
fail() { printf '  \033[31m❌\033[0m %s\n' "$1"; FAIL=1; }
skip() { printf '  \033[33m⏭\033[0m %s\n' "$1"; }
head1() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# ---------- 0) 连库 ----------
DB_URL="${P16_DB_URL:-${ERP_DB_URL:-}}"
if [ -z "$DB_URL" ] && [ -n "${NIK_DATABASE_USER:-}" ]; then
  DB_URL="postgres://${NIK_DATABASE_USER}:${NIK_DATABASE_PASSWORD:-}@${NIK_DATABASE_HOST:-127.0.0.1}:${NIK_DATABASE_PORT:-5432}/${NIK_DATABASE_NAME:-cloudpivot}"
fi
if [ -z "$DB_URL" ]; then
  echo "SKIP: 未提供 P16_DB_URL / ERP_DB_URL / NIK_DATABASE_*，无法连库（真库段全部跳过）" >&2
  exit 2
fi
if ! "$PSQL" "$DB_URL" -X -tAc 'select 1' >/dev/null 2>&1; then
  echo "SKIP: 连不上库（$DB_URL），真库段全部跳过" >&2
  exit 2
fi

q() { "$PSQL" "$DB_URL" -X -tAc "$1" 2>/dev/null; }
qp() { "$PSQL" "$DB_URL" -X -tAc "$1" 2>/dev/null | tr -d '[:space:]'; }

echo "库：$(q 'select current_database()')"
echo "仓：$ROOT"

# ---------- ① 结构与种子 ----------
head1 "① 结构与种子自检（V0174）"

col=$(qp "select count(*) from information_schema.columns
           where table_name='biz_stock_in' and column_name='approval_instance_id'")
[ "$col" = "1" ] && pass "biz_stock_in.approval_instance_id 已存在" || fail "缺 approval_instance_id 列（V0174 未应用）"

ck=$(q "select pg_get_constraintdef(oid) from pg_constraint
         where conname='ck_biz_si_status' and conrelid='biz_stock_in'::regclass")
case "$ck" in
  *PENDING_CANCEL*) pass "ck_biz_si_status 已放开 PENDING_CANCEL" ;;
  *) fail "ck_biz_si_status 未放开 PENDING_CANCEL（新状态第一笔写入就会 23514）" ;;
esac

ck2=$(qp "select count(*) from pg_constraint
           where conname='ck_biz_si_pending_cancel' and conrelid='biz_stock_in'::regclass")
[ "$ck2" = "1" ] && pass "ck_biz_si_pending_cancel 存在（审批中 ⟺ 有实例）" || fail "缺 ck_biz_si_pending_cancel"

seed=$(q "select d.status || '|' || d.is_default::text || '|' || v.version_status || '|' || v.node_count || '|' || v.edge_count
            from biz_flow_def d join biz_flow_def_version v on v.id = d.current_version_id
           where d.flow_code='stock_in_cancel' and d.deleted_at is null")
if [ -z "$seed" ]; then
  fail "种子流程 stock_in_cancel 不存在"
elif [ "$seed" = "PUBLISHED|true|PUBLISHED|3|2" ]; then
  pass "种子流程 stock_in_cancel = PUBLISHED/默认/3 节点 2 边"
else
  fail "种子流程结构不对：$seed（期望 PUBLISHED|true|PUBLISHED|3|2）"
fi

forbid=$(q "select n.self_approval || '|' || n.assignee_type || '|' || n.empty_assignee
              from biz_flow_node n join biz_flow_def d on d.current_version_id = n.version_id
             where d.flow_code='stock_in_cancel' and n.node_type='APPROVAL'")
if [ "$forbid" = "FORBID|ROLE|ERROR" ]; then
  pass "审批节点 = ROLE + 空审批人报错（不兜底超管）+ **禁止自审**（内控，P1-FIX-1）"
else
  fail "审批节点策略不对：$forbid（期望 FORBID|ROLE|ERROR）"
fi

wh_role=$(q "select coalesce(string_agg(r.role_code, ','), '')
               from biz_flow_node_assignee a
               join biz_flow_node n on n.id = a.node_id
               join biz_flow_def_version v on v.id = n.version_id
               join biz_flow_def d on d.id = v.def_id
               join sys_role r on r.id = a.ref_id and r.deleted_at is null
              where d.flow_code='stock_in_cancel' and v.is_current and n.node_type='APPROVAL'")
if [ "$wh_role" = "WAREHOUSE_MANAGER" ]; then
  pass "作废复核岗位 = 仓管主管（WAREHOUSE_MANAGER，P1-FIX-1：不再指超管）"
else
  fail "作废复核岗位绑定不对：$wh_role（期望 WAREHOUSE_MANAGER）"
fi

# 状态分布（给人看：审批中 / 已作废各多少）
q "select '状态分布：' || string_agg(status || '=' || cnt, '  ') from
     (select status, count(*)::text as cnt from biz_stock_in where deleted_at is null group by status) t" | sed 's/^/    /'

# ---------- ② 单测 ----------
head1 "② 单测（真库）"
if command -v cargo >/dev/null 2>&1; then
  ( cd "$ROOT/backend" && ERP_DB_URL="${ERP_DB_URL:-$DB_URL}" \
      cargo test --lib business::erp::stockin -- --test-threads=1 2>&1 | tail -4 ) \
    | grep -q "test result: ok" \
    && pass "business::erp::stockin 全绿（含 P16 16 条 + P9 作废回归 12 条）" \
    || fail "单测有失败（重跑：cd backend && ERP_DB_URL=… cargo test --lib business::erp::stockin -- --test-threads=1）"
else
  skip "没装 cargo，单测段跳过"
fi

# ---------- ③ 六本账回滚实证 ----------
head1 "③ 六本账回滚实证（取最近一张审批通过作废的单）"
DOC=$(q "select id from biz_stock_in where status='CANCELLED' and approval_instance_id is null
          order by cancelled_at desc nulls last limit 1")
if [ -z "$DOC" ]; then
  skip "库里还没有「作废审批通过」的收货单（先按卡面 §三 走一遍 ①–④）"
else
  echo "    单据：$(q "select in_no || ' / ' || status from biz_stock_in where id='$DOC'")"
  # ① 库存：本单物料在本仓的数量应为回滚后的值；这里只验证「反向流水 / 红字 / 应付 / 序列号」
  txn=$(qp "select count(*) from biz_inventory_txn where doc_type='STOCK_IN' and doc_id='$DOC' and biz_type='PURCHASE_IN_CANCEL'")
  [ "$txn" -ge 1 ] && pass "反向流水 PURCHASE_IN_CANCEL = $txn 条" || fail "没有反向流水（库存账没退）"

  red=$(qp "select count(*) from biz_voucher where source_id='$DOC' and is_red=1")
  [ "$red" -ge 1 ] && pass "红字凭证 = $red 张" || fail "没有红字凭证"

  pay=$(q "select coalesce(string_agg(status::text, ','), '(无应付单)') from biz_payable where source_id='$DOC'")
  case "$pay" in
    *OPEN*|*PARTIAL_PAID*) fail "应付单未联动作废：$pay" ;;
    *) pass "应付单状态：$pay" ;;
  esac

  alive=$(qp "select count(*) from biz_material_serial where source_id='$DOC' and source_type='PURCHASE' and deleted_at is null")
  if [ "$alive" = "0" ]; then
    pass "序列号全部软删（在库 0 件）"
  else
    fail "还有 $alive 个序列号没退（一物一码幽灵号）"
  fi

  # 成本账 / 库位账：本单涉及的 (物料, 仓) 上，作废后不应残留「本单数量」
  neg=$(qp "select count(*) from biz_inventory_location il
              join biz_stock_in_item i on i.material_id = il.material_id and i.location_id = il.location_id
             where i.stock_in_id='$DOC' and il.warehouse_id = (select warehouse_id from biz_stock_in where id='$DOC') and il.qty < 0")
  [ "$neg" = "0" ] && pass "库位账无负数（没有冲成负库存）" || fail "库位账出现 $neg 行负数"
fi

# ---------- ④ 反向：孤儿实例 ----------
head1 "④ 反向自检：孤儿作废审批实例（P11 教训）"
orphan=$(qp "select count(*)
               from biz_flow_instance i
               left join biz_stock_in s on s.id = i.biz_id
              where i.biz_type='STOCK_IN_CANCEL' and i.status='PENDING' and i.deleted_at is null
                and i.biz_id is not null
                and (s.id is null or s.deleted_at is not null or s.status <> 'PENDING_CANCEL')")
if [ "$orphan" = "0" ]; then
  pass "孤儿实例 0 条（定时任务 stockin_cancel_reconcile 每 10 分钟扫一遍）"
else
  fail "存在 $orphan 条孤儿作废审批实例（单据已删 / 已非作废审批中，审批却还在跑）"
fi

pending_all=$(qp "select count(*) from biz_flow_instance where biz_type='STOCK_IN_CANCEL' and status='PENDING'")
running_docs=$(qp "select count(*) from biz_stock_in where status='PENDING_CANCEL' and deleted_at is null")
if [ "$pending_all" = "$running_docs" ]; then
  pass "在跑实例数（$pending_all）= 作废审批中单据数（$running_docs）"
else
  fail "在跑实例数（$pending_all）≠ 作废审批中单据数（$running_docs）：审批与单据脱节"
fi

# ---------- ⑤ 不变量 ----------
head1 "⑤ ERP 不变量套件"
INV_LOG=$(mktemp)
( cd "$ROOT" && NIK_DATABASE_HOST="$(echo "$DB_URL" | sed -E 's#^postgres://[^:]+:[^@]*@([^:]+):([0-9]+)/.*#\1#')" \
  NIK_DATABASE_PORT="$(echo "$DB_URL" | sed -E 's#^postgres://[^:]+:[^@]*@[^:]+:([0-9]+)/.*#\1#')" \
  NIK_DATABASE_USER="$(echo "$DB_URL" | sed -E 's#^postgres://([^:]+):.*#\1#')" \
  NIK_DATABASE_PASSWORD="$(echo "$DB_URL" | sed -E 's#^postgres://[^:]+:([^@]*)@.*#\1#')" \
  NIK_DATABASE_NAME="$(echo "$DB_URL" | sed -E 's#^postgres://[^@]+@[^/]+/(.*)$#\1#')" \
  timeout 300 python3 scripts/verify/erp_invariants.py >"$INV_LOG" 2>&1 )
grep -E "结果：|需要人看|BLOCKER" "$INV_LOG" | tail -4 | sed 's/^/    /'
if grep -q "BLOCKER 0" "$INV_LOG"; then
  pass "不变量 BLOCKER 0"
else
  fail "不变量出现 BLOCKER（详见 $INV_LOG）"
fi
PASSED=$(grep -oE "结果：[0-9]+/[0-9]+ 通过" "$INV_LOG" | grep -oE "[0-9]+/" | tr -d '/')
if [ -n "$PASSED" ] && [ "$PASSED" -ge 32 ]; then
  pass "不变量通过数 $PASSED（≥32）"
else
  fail "不变量通过数 $PASSED（<32，详见 $INV_LOG）"
fi

# ---------- ⑥ 前端 ----------
head1 "⑥ 前端类型检查"
if [ "$SKIP_FE" = "1" ]; then
  skip "--skip-frontend"
elif [ -d "$ROOT/frontend/node_modules" ] && command -v pnpm >/dev/null 2>&1; then
  ( cd "$ROOT/frontend" && pnpm typecheck >/tmp/p16_typecheck.log 2>&1 ) \
    && pass "vue-tsc --noEmit 通过" || { fail "typecheck 失败（/tmp/p16_typecheck.log）"; tail -5 /tmp/p16_typecheck.log | sed 's/^/    /'; }
else
  skip "前端依赖未安装 / 没有 pnpm"
fi

head1 "结果"
if [ "$FAIL" = "0" ]; then
  printf '  \033[32m★ P16 全部通过\033[0m\n'
  exit 0
fi
printf '  \033[31m✗ 有断言不通过（见上）\033[0m\n'
exit 1
