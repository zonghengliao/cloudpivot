#!/usr/bin/env bash
# ============================================================================
# FIX-3 验收（145 测试机 · 隔离实例 8041 · 全新库 cloudpivot_fix3）
#
# 卡面验收逐条对拍：
#   ① FIXED_ASSET 停用 / 启用 / 处置 → 各产生通知（sys_notice）
#   ② 新库跑完全集迁移 → biz_flow_linkage_rule 有 3 条 FIXED_ASSET 规则
#   ③ 未计提当月折旧 → 处置 409 + 66048（提示先计提）
#   ④ 已计提 → 处置成功，处置凭证按含当月折旧的累计额落账
#   ⑤ 回归：未处置资产的折旧照常
#   ⑥ 幂等：up.sql 重跑两次 → 规则仍 3 条（自检 NOTICE 不炸）
#
# 只跑 145，不碰 144 生产。DB 与端口都可覆盖：DB=xxx PORT=yyyy bash verify145.sh
# ============================================================================
set -uo pipefail

DB="${DB:-cloudpivot_fix3}"
PORT="${PORT:-8041}"
BASE="http://127.0.0.1:${PORT}"
API="$BASE/api"
ENVF=/etc/cloudpivot/cloudpivot-dev.env
OUT="${OUT:-/tmp/fix3/out}"
mkdir -p "$OUT"

ADMIN_PASS_INIT="$(grep -m1 ^NIK_ADMIN_PASSWORD "$ENVF" | cut -d= -f2)"
ADMIN_PASS_NEW='Admin@123456'
export PGPASSWORD="$(grep -m1 ^NIK_DATABASE_PASSWORD "$ENVF" | cut -d= -f2)"

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  ❌ $*"; }
check() { # $1=实际 $2=期望 $3=说明
  if [ "$1" = "$2" ]; then ok "$3（$1）"; else bad "$3：期望 [$2] 实际 [$1]"; fi
}
check_ne() {
  if [ "$1" != "$2" ]; then ok "$3（$1）"; else bad "$3：不该等于 [$2]"; fi
}

psqlq() { psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -t -A -c "$1" 2>&1; }

# api METHOD PATH TOKEN [BODY] → stdout=响应体，$HTTP=状态码
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o "$OUT/_body.json" -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o "$OUT/_body.json" -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat "$OUT/_body.json"
}

login() { # $1=user $2=pass → token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" > "$OUT/_login.json"
  jq -r '.data.access_token // empty' "$OUT/_login.json"
}

echo "=============================================================="
echo "FIX-3 验收 · DB=$DB PORT=$PORT · $(date '+%F %T')"
echo "=============================================================="

# ---------------------------------------------------------------- 0 实例
echo
echo "=== 0 隔离实例 ==="
HEALTH=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/health" || true)
if [ "$HEALTH" != "200" ]; then
  echo "  health=$HEALTH，实例没起（本脚本假设实例已由外部拉起）"
  exit 1
fi
ok "实例健康 /health=200（$BASE）"

MIG=$(psqlq "select count(*) from schema_migrations")
echo "  schema_migrations=$MIG"

# ---------------------------------------------------------------- ② 预置规则
echo
echo "=== ② 新库的 FIXED_ASSET 联动规则（V0323） ==="
RULES=$(psqlq "select trigger_status||'|'||enabled||'|'||(action_params->>'mode')
                 from biz_flow_linkage_rule
                where trigger_entity='FIXED_ASSET' and action_code='SEND_NOTICE' and deleted_at is null
                order by trigger_status")
echo "$RULES" | sed 's/^/  /'
N_RULES=$(printf '%s\n' "$RULES" | grep -c . )
check "$N_RULES" "3" "FIXED_ASSET 有 3 条 SEND_NOTICE 规则"
check "$(printf '%s\n' "$RULES" | grep -c '|true|TAKEOVER')" "3" "3 条全部 enabled + TAKEOVER"
check "$(printf '%s\n' "$RULES" | cut -d'|' -f1 | tr '\n' ',' )" "DISPOSED,IDLE,IN_USE," "触发状态覆盖 DISPOSED / IDLE / IN_USE"

# ---------------------------------------------------------------- ⑥ 幂等
echo
echo "=== ⑥ 迁移幂等（up.sql 连跑两次） ==="
UP=/opt/cloudpivot/backend/migrations/business/V0323__asset_linkage_rules.up.sql
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -v ON_ERROR_STOP=1 -f "$UP" > "$OUT/idem1.log" 2>&1
E1=$?
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -v ON_ERROR_STOP=1 -f "$UP" > "$OUT/idem2.log" 2>&1
E2=$?
N_AFTER=$(psqlq "select count(*) from biz_flow_linkage_rule where trigger_entity='FIXED_ASSET' and deleted_at is null")
check "$E1/$E2" "0/0" "up.sql 连跑两次不报错（自检块通过）"
check "$N_AFTER" "3" "重跑后规则仍是 3 条（ON CONFLICT DO NOTHING，不生出第二份）"
grep -o "V0323 完成.*" "$OUT/idem2.log" | head -2 | sed 's/^/  /'

# ---------------------------------------------------------------- 登录
echo
echo "=== 准备：登录 + 组织/分类/资产 ==="
ADMIN=$(login admin "$ADMIN_PASS_INIT")
if [ -z "$ADMIN" ]; then
  # 复用旧库时 admin 口令可能已被改过：退回默认口令再试一次（新库不会走到这里）
  ADMIN=$(login admin "$ADMIN_PASS_NEW")
fi
if [ -z "$ADMIN" ]; then
  bad "admin 登录失败"; exit 1
fi
MUST=$(jq -r '.data.must_change_password // false' "$OUT/_login.json")
if [ "$MUST" = "true" ]; then
  api PUT "/system/auth/password" "$ADMIN" "{\"old_password\":\"$ADMIN_PASS_INIT\",\"new_password\":\"$ADMIN_PASS_NEW\"}" > "$OUT/pwd.json"
  echo "  首登改密 http=$HTTP"
  ADMIN=$(login admin "$ADMIN_PASS_NEW")
fi
[ -n "$ADMIN" ] || { bad "改密后 admin 登录失败"; exit 1; }
ok "admin 已登录（token len=${#ADMIN}）"

DEPT=$(psqlq "select id from sys_dept where dept_code='FIX3_ASSET' limit 1")
if [ -z "$DEPT" ]; then
  api POST "/system/dept" "$ADMIN" '{"dept_code":"FIX3_ASSET","dept_name":"FIX3 资产部","status":1,"sort_order":91}' > "$OUT/dept.json"
  DEPT=$(jq -r '.data.id // empty' "$OUT/dept.json")
fi
[ -n "$DEPT" ] || { bad "建部门失败"; cat "$OUT/dept.json"; exit 1; }
ok "部门 dept_id=$DEPT"

# 审批人（GENERAL_MANAGER）：处置流程 n_gm 只派这个角色
GMID=$(psqlq "select id from sys_user where username='gm_fix3' limit 1")
if [ -z "$GMID" ]; then
  api POST "/system/user" "$ADMIN" "{\"username\":\"gm_fix3\",\"password\":\"Drill@123456\",\"real_name\":\"FIX3 总经理\",\"dept_id\":\"$DEPT\",\"status\":1,\"role_ids\":[\"20000000-0000-4000-8000-0000000f0003\"]}" > "$OUT/gm.json"
  GMID=$(jq -r '.data.id // empty' "$OUT/gm.json")
fi
GM=$(login gm_fix3 'Drill@123456')
if [ -z "$GM" ]; then
  # 首登改密同样处理
  api PUT "/system/auth/password" "$(login gm_fix3 'Drill@123456')" '{"old_password":"Drill@123456","new_password":"Drill@654321"}' > "$OUT/gmpwd.json"
  GM=$(login gm_fix3 'Drill@654321')
fi
[ -n "$GM" ] || { bad "gm_fix3 登录失败"; cat "$OUT/gm.json"; exit 1; }
ok "审批人 gm_fix3 已登录（id=$GMID）"

ACC_ASSET=$(psqlq "select id from biz_account where account_code='1601' and deleted_at is null limit 1")   # 1601 固定资产
ACC_ACCUM=$(psqlq "select id from biz_account where account_code='1602' and deleted_at is null limit 1")   # 1602 累计折旧
ACC_EXP=$(psqlq "select id from biz_account where account_code='5602.01' and deleted_at is null limit 1") # 5602.01 管理费用-折旧费
[ -n "$ACC_ASSET" ] && [ -n "$ACC_ACCUM" ] && [ -n "$ACC_EXP" ] || { bad "科目 1601/1602/5602.01 缺失"; exit 1; }
ok "科目：1601=$ACC_ASSET / 1602=$ACC_ACCUM / 5602.01=$ACC_EXP"

CAT=$(psqlq "select id from biz_asset_category where category_code='FIX3_EQ' and deleted_at is null limit 1")
if [ -z "$CAT" ]; then
  api POST "/business/erp/asset-category" "$ADMIN" "{\"category_code\":\"FIX3_EQ\",\"category_name\":\"FIX3 设备（60月）\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"FIX-3 验收分类\"}" > "$OUT/cat.json"
  CAT=$(jq -r '.data.id // empty' "$OUT/cat.json")
fi
[ -n "$CAT" ] || { bad "建分类失败"; cat "$OUT/cat.json"; exit 1; }
ok "分类 FIX3_EQ=$CAT（60 月 / 残值 5%）"

# 资产 A：2026-10-05 购入 → 起提 2026-11-01（月折旧 1900）
api POST "/business/erp/asset" "$ADMIN" "{\"asset_name\":\"FIX3 设备A\",\"category_id\":\"$CAT\",\"spec\":\"FIX3-A\",\"use_dept_id\":\"$DEPT\",\"original_value\":\"120000\",\"tax_amount\":\"0\",\"purchase_date\":\"2026-10-05\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"FIX-3 验收资产A\"}" > "$OUT/assetA.json"
A=$(jq -r '.data.id // empty' "$OUT/assetA.json")
[ -n "$A" ] || { bad "建卡 A 失败"; cat "$OUT/assetA.json"; exit 1; }
ok "资产 A=$A（$(psqlq "select asset_no||' 月折旧 '||monthly_depreciation||' 起提 '||start_depreciation_date from biz_fixed_asset where id='$A'::uuid")）"

# 资产 B：同月购入，用来验 ⑤ 回归（未处置照提）+ ① 停用/启用通知
api POST "/business/erp/asset" "$ADMIN" "{\"asset_name\":\"FIX3 设备B\",\"category_id\":\"$CAT\",\"spec\":\"FIX3-B\",\"use_dept_id\":\"$DEPT\",\"original_value\":\"60000\",\"tax_amount\":\"0\",\"purchase_date\":\"2026-10-06\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"FIX-3 验收资产B\"}" > "$OUT/assetB.json"
B=$(jq -r '.data.id // empty' "$OUT/assetB.json")
[ -n "$B" ] || { bad "建卡 B 失败"; cat "$OUT/assetB.json"; exit 1; }
ok "资产 B=$B"

# 资产 C：2026-11-05 购入 → 起提 2026-12-01（当月购入当月处置的边界用例）
api POST "/business/erp/asset" "$ADMIN" "{\"asset_name\":\"FIX3 设备C\",\"category_id\":\"$CAT\",\"spec\":\"FIX3-C\",\"use_dept_id\":\"$DEPT\",\"original_value\":\"1000\",\"tax_amount\":\"0\",\"purchase_date\":\"2026-11-05\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"FIX-3 验收资产C（当月购入）\"}" > "$OUT/assetC.json"
C=$(jq -r '.data.id // empty' "$OUT/assetC.json")
[ -n "$C" ] || { bad "建卡 C 失败"; cat "$OUT/assetC.json"; exit 1; }
ok "资产 C=$C（起提日 $(psqlq "select start_depreciation_date from biz_fixed_asset where id='$C'::uuid")）"

# ---------------------------------------------------------------- ① 停用/启用通知
echo
echo "=== ①-a 停用 / 启用 → 各产生一条 FIXED_ASSET 通知 ==="
N0=$(psqlq "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$B'")
api POST "/business/erp/asset/$B/status" "$ADMIN" '{"target_status":"IDLE","reason":"FIX3 验收停用"}' > "$OUT/b_idle.json"
check "$HTTP" "200" "停用 HTTP=200"
api POST "/business/erp/asset/$B/status" "$ADMIN" '{"target_status":"IN_USE","reason":"FIX3 验收启用"}' > "$OUT/b_inuse.json"
check "$HTTP" "200" "启用 HTTP=200"
sleep 1
N1=$(psqlq "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$B'")
echo "  通知条数 $N0 → $N1"
check "$N1" "2" "停用 + 启用各发一条通知（无规则时会是 0）"
psqlq "select title||' | '||idempotency_key from sys_notice where biz_type='FIXED_ASSET' and biz_id='$B' order by created_at" | sed 's/^/  /'

# ---------------------------------------------------------------- ③ 处置闸 409
echo
echo "=== ③ 未计提当月（2026-11）折旧 → 处置 409 66048 ==="
psqlq "select 'period 2026-11 status='||status from biz_accounting_period where period_code='2026-11'"
api POST "/business/erp/asset/$A/dispose" "$ADMIN" '{"disposal_type":"SALE","disposal_date":"2026-11-20","reason":"FIX3 验收处置","sale_price":"100000","output_tax":"13000"}' > "$OUT/a_dispose_blocked.json"
GATE_CODE=$(jq -r '.code // empty' "$OUT/a_dispose_blocked.json")
GATE_MSG=$(jq -r '.message // .msg // empty' "$OUT/a_dispose_blocked.json")
echo "  http=$HTTP code=$GATE_CODE"
echo "  msg=$GATE_MSG"
check "$HTTP" "409" "未计提当月折旧 → 处置被拒绝（409）"
check "$GATE_CODE" "66048" "错误码 66048（ERP_ASSET_DEPRECIATION_PENDING）"
if printf '%s' "$GATE_MSG" | grep -q "2026-11"; then ok "报文点名处置当月 2026-11"; else bad "报文没点名期间：$GATE_MSG"; fi
if printf '%s' "$GATE_MSG" | grep -q "计提"; then ok "报文指路「先计提」"; else bad "报文没给「先计提」指引"; fi
N_APPROVAL=$(psqlq "select count(*) from biz_flow_instance where biz_type='ASSET_DISPOSAL' and biz_id='$A'::uuid")
check "$N_APPROVAL" "0" "被拦时**没有**起审批实例（失败在提交前）"

echo
echo "=== ③-b 当月购入当月处置（资产 C）不被闸门误拦 ==="
api POST "/business/erp/asset/$C/dispose" "$ADMIN" '{"disposal_type":"SCRAP","disposal_date":"2026-11-21","reason":"FIX3 当月购入当月报废"}' > "$OUT/c_dispose.json"
C_CODE=$(jq -r '.code // empty' "$OUT/c_dispose.json")
echo "  http=$HTTP code=$C_CODE msg=$(jq -r '.message // .msg // empty' "$OUT/c_dispose.json")"
check_ne "$C_CODE" "66048" "当月购入当月处置不被折旧闸拦（起提日 2026-12-01，本月本来不提）"

# ---------------------------------------------------------------- ④ 计提后处置
echo
echo "=== ④ 先计提 2026-11 折旧 → 再处置 A ==="
api POST "/business/erp/depreciation/run" "$ADMIN" '{"period_code":"2026-11"}' > "$OUT/dep_run.json"
echo "  http=$HTTP $(jq -c '{code,msg,data}' "$OUT/dep_run.json")"
check "$HTTP" "200" "2026-11 折旧计提 HTTP=200"
BATCH_ST=$(psqlq "select status||' / '||asset_count||' 台 / '||total_amount from biz_depreciation_batch where period_code='2026-11'")
echo "  批次：$BATCH_ST"
case "$BATCH_ST" in POSTED*) ok "批次已过账（POSTED）";; *) bad "批次未过账：$BATCH_ST";; esac

echo
echo "=== ⑤ 回归：未处置资产（B）的折旧照常 ==="
B_AMT=$(psqlq "select amount from biz_depreciation where asset_id='$B'::uuid and period_code='2026-11' and status='POSTED'")
A_AMT=$(psqlq "select amount from biz_depreciation where asset_id='$A'::uuid and period_code='2026-11' and status='POSTED'")
check "$A_AMT" "1900.0000" "资产 A 2026-11 折旧明细 1900（处置前照提）"
check "$B_AMT" "950.0000" "资产 B（未处置）2026-11 折旧照常 950"
C_AMT=$(psqlq "select count(*) from biz_depreciation where asset_id='$C'::uuid and period_code='2026-11'")
check "$C_AMT" "0" "资产 C 当月购入当月不提（明细 0 行）"

echo
echo "=== ④ 处置 A（已计提当月） ==="
ACC_BEFORE=$(psqlq "select accumulated_depreciation from biz_fixed_asset where id='$A'::uuid")
echo "  处置前累计折旧=$ACC_BEFORE（含 2026-11 的 1900）"
api POST "/business/erp/asset/$A/dispose" "$ADMIN" '{"disposal_type":"SALE","disposal_date":"2026-11-20","reason":"FIX3 验收处置","sale_price":"100000","output_tax":"13000"}' > "$OUT/a_dispose_ok.json"
check "$HTTP" "200" "已计提当月折旧 → 处置提交 200"
INST=$(psqlq "select instance_no from biz_flow_instance where biz_type='ASSET_DISPOSAL' and biz_id='$A'::uuid order by created_at desc limit 1")
echo "  审批实例=$INST"
[ -n "$INST" ] && ok "已起处置审批实例" || bad "没有起审批实例"

TASK=$(psqlq "select t.id from biz_flow_task t join biz_flow_instance i on i.id = t.instance_id
               where i.biz_no = (select asset_no from biz_fixed_asset where id='$A'::uuid)
                 and t.status = 'PENDING' order by t.created_at desc limit 1")
echo "  待办 id=$TASK（按实例 biz_no 精确取，不依赖 todo 接口的分页/过滤口径）"
[ -n "$TASK" ] || { bad "资产 A 的处置没有生成待办"; }
if [ -n "$TASK" ]; then
  # 顺手把 todo 接口的原始响应留证（gm_fix3 视角）
  curl -sS "$API/business/flow/task/todo?page=1&page_size=20" -H "Authorization: Bearer $GM" > "$OUT/gm_todo.json"
  echo "  gm_fix3 todo: $(jq -c '{code,total:.data.total,rows:(.data.list|length?)}' "$OUT/gm_todo.json" 2>/dev/null)"
  api POST "/business/flow/task/$TASK/approve" "$GM" '{"comment":"FIX3 验收：同意处置"}' > "$OUT/a_approve.json"
  check "$HTTP" "200" "总经理审批通过 HTTP=200"
  sleep 3
fi

A_ST=$(psqlq "select status||'|'||coalesce(disposal_type,'')||'|'||coalesce(disposal_date::text,'')||'|'||accumulated_depreciation from biz_fixed_asset where id='$A'::uuid")
echo "  卡片终态：$A_ST"
check "$(printf '%s' "$A_ST" | cut -d'|' -f1)" "DISPOSED" "卡片已处置"
check "$(printf '%s' "$A_ST" | cut -d'|' -f4)" "1900.0000" "处置时累计折旧含当月 1900（不再是 0 / 漏提）"

echo "  处置凭证："
psqlq "select v.voucher_no||' | '||e.account_code||' 借'||e.debit_amount||' 贷'||e.credit_amount
         from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
        where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='$A'::uuid
        order by v.voucher_no, e.line_no" | sed 's/^/    /'
BAL=$(psqlq "select count(*) from (select v.id from biz_voucher v where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='$A'::uuid and v.total_debit<>v.total_credit) t")
check "$BAL" "0" "处置凭证全部借贷平衡"
ACC_V=$(psqlq "select coalesce(sum(e.debit_amount),0) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id join biz_account a on a.id=e.account_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='$A'::uuid and a.account_code='1602'")
check "$ACC_V" "1900.0000" "凭证借记 1602 累计折旧 = 1900（当月 1900 已进处置分录，不再漏提）"
CLR=$(psqlq "select coalesce(sum(e.debit_amount - e.credit_amount),0) from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id join biz_account a on a.id=e.account_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='$A'::uuid and a.account_code='1606'")
check "$CLR" "0.0000" "清理科目 1606 净额归零（K7）"
NET=$(psqlq "select net_value from biz_fixed_asset where id='$A'::uuid")
echo "  处置时净值=$NET（120000 − 1900 = 118100，与 1606 借方一致才说明当月折旧进了处置）"

echo
echo "=== ①-b 处置 → 产生 DISPOSED 通知 ==="
ND=$(psqlq "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$A'")
echo "  资产 A 通知条数=$ND"
psqlq "select title||' | '||idempotency_key from sys_notice where biz_type='FIXED_ASSET' and biz_id='$A' order by created_at" | sed 's/^/  /'
[ "$ND" -ge 1 ] && ok "处置发了通知（无预置规则时会是 0）" || bad "处置没有产生通知"

echo
echo "=== 附：迁移版本 / 规则表最终态 ==="
psqlq "select 'schema_migrations='||count(*)||' 最大='||max(version::int) from schema_migrations"
psqlq "select trigger_entity||'/'||trigger_status||' '||name from biz_flow_linkage_rule where deleted_at is null order by trigger_entity, trigger_status" | sed 's/^/  /'

echo
echo "=============================================================="
echo "SUMMARY  PASS=$PASS  FAIL=$FAIL"
echo "=============================================================="
[ "$FAIL" -eq 0 ] || exit 1
