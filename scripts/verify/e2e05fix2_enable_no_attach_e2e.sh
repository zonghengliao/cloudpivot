#!/usr/bin/env bash
# E2E05-FIX-2（卡 t_b1bf7566）真机验收：**无附件也能启用（留痕放行）**
#
# 环境：145 测试机 · cloudpivot_dev 库 · 隔离后端实例（真 token curl）
#
# 背景（E2E-05 演练踩到的死路）：
#   建卡（直接 IN_USE）不查附件；停用后再启用（IDLE→IN_USE）却硬拦
#   「固定资产必须上传发票或验收单」→ 发票未到先建卡的老资产**永远启用不回来**。
#   方案 B（本卡落地）：启用降级为「提示 + 允许 + 审计」，建卡口径不变。
#
# 验收（与卡面 ①~⑤ 一一对应）：
#
#   ① 建卡不传附件 → 正常建成（IN_USE），附件数 0
#   ② 停用 → 无附件启用 → **200**，响应带 `warning`（点名缺口 + 审计码），
#      `operation_logs.operation = ERP_ASSET_ENABLE_NO_ATTACH` 恰好 1 行（含资产编号 / 原因）
#   ③ 前端预检数据源：`GET /attachment/check?at=ACTIVATE` 对无附件资产回 `can_proceed=false`
#      + `missing[].hint`（前端据此弹确认框）；补传后回 `can_proceed=true`
#   ④ 有附件启用 → 200 且 **无** `warning`；也**不**写「无附件启用」审计
#   ⑤ 建卡带附件 → 正常（附件落库 + 卡片在役）
#
# 号段：分类 E2E5F2_60 / 资产名 E2E05FIX2-<RUN_TAG>-* / 部门复用「演练设备部」。
# 每次运行用新的 RUN_TAG（默认时间戳）⇒ 反复跑不会互相干扰（夹具留在库里，见 README）。
#
# 用法：BASE=http://127.0.0.1:8036/api bash scripts/verify/e2e05fix2_enable_no_attach_e2e.sh
# 退出码：0 = 全绿；非 0 = 有 FAIL（明细见 $LOG）
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh

# 允许外部覆盖库名：145 上 `/etc/cloudpivot/cloudpivot-dev.env` 指向的 `cloudpivot_dev`
# 不一定存在（实测 2026-11-09 已被删），隔离验收时用 `DB_NAME=cp_e2e05fix2` 让
# 本脚本与被测实例连同一个库（口令仍取 env 里的 NIK_DATABASE_PASSWORD）。
if [ -n "${DB_NAME:-}" ]; then
  export NIK_DATABASE_NAME="$DB_NAME"
  export PGPASSWORD="$NIK_DATABASE_PASSWORD"
fi

YEAR=${YEAR:-2032}
P1="$YEAR-01"
RUN_TAG=${RUN_TAG:-$(date +%m%d%H%M)}
PREV_YEAR=$((YEAR - 1))
PURCHASE_DATE="$PREV_YEAR-12-10"
CAT_CODE="E2E5F2_60"
NAME_NOATT="E2E05FIX2-$RUN_TAG 无附件资产"
NAME_ATT="E2E05FIX2-$RUN_TAG 建卡带附件"
OP_NOATT="ERP_ASSET_ENABLE_NO_ATTACH"

asset_id_of() { SQ "select id from biz_fixed_asset where asset_name='$1' and deleted_at is null order by created_at desc limit 1"; }
att_cnt()     { SQ "select count(*) from biz_doc_attachment where doc_type='FIXED_ASSET' and doc_id='$1' and deleted_at is null"; }
audit_cnt()   { SQ "select count(*) from operation_logs where target_id='$1' and operation='$OP_NOATT'"; }
audit_field() { SQ "select coalesce(detail->>'$2','') from operation_logs where target_id='$1' and operation='$OP_NOATT' order by created_at limit 1"; }
status_of()   { SQ "select status from biz_fixed_asset where id='$1'"; }
# 启用的 warning 字段（空 = 无 warning；jq 的 .data.warning 缺省为 null）
warn_of()     { printf '%s' "$BODY" | jq -r '.data.warning // ""' 2>/dev/null; }

say "===== F0 登录 + 前置（部门 / 科目 / 期间 $P1 / 分类）====="
login_admin
DEPT1=$(SQ "select id from sys_dept where dept_name='演练设备部' and deleted_at is null limit 1")
if [ -z "$DEPT1" ]; then
  api mk_d1 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E5F2-D1","dept_name":"演练设备部","status":1,"sort_order":92}'
  check "建部门 演练设备部 http" "$HTTP" "200"
  DEPT1=$(data '.data.id')
fi
checkne "部门（演练设备部）" "$DEPT1" ""
ACC_ASSET=$(acc 1601); ACC_ACCUM=$(acc 1602); ACC_EXP=$(acc 5602.01)
checkne "科目 1601/1602/5602.01 齐备" "${#ACC_ASSET}${#ACC_ACCUM}${#ACC_EXP}" "108"
save DEPT1 "$DEPT1"

have=$(SQ "select count(*) from biz_accounting_period where period_code='$P1'")
if [ "$have" != 1 ]; then
  api per_open POST /business/erp/period/batch-open "$T_ADMIN" \
    "{\"from\":\"$P1\",\"to\":\"$P1\",\"remark\":\"E2E05-FIX-2 无附件启用验收\"}"
  check "开账 $P1 http" "$HTTP" "200"
fi
check "验收期间 $P1 已开" "$(SQ "select count(*) from biz_accounting_period where period_code='$P1'")" "1"

CAT=$(SQ "select id from biz_asset_category where category_code='$CAT_CODE' and deleted_at is null limit 1")
if [ -z "$CAT" ]; then
  api mk_cat POST /business/erp/asset-category "$T_ADMIN" \
    "{\"category_code\":\"$CAT_CODE\",\"category_name\":\"E2E05FIX2 60月设备\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"E2E05-FIX-2 验收分类\"}"
  check "建分类 http" "$HTTP" "200"
  check "建分类 code" "$CODE" "0"
  CAT=$(data '.data.id')
fi
checkne "分类 $CAT_CODE" "$CAT" ""
save CAT "$CAT"

mk_asset() { # mk_asset <tag> <name>
  api "$1" POST /business/erp/asset "$T_ADMIN" \
    "{\"asset_name\":\"$2\",\"category_id\":\"$CAT\",\"spec\":\"E2E05FIX2\",\"use_dept_id\":\"$DEPT1\",\"location\":\"演练车间 F2\",\"original_value\":\"12000\",\"tax_amount\":\"0\",\"purchase_date\":\"$PURCHASE_DATE\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"E2E05-FIX-2 无附件启用验收 $RUN_TAG\"}"
  check "建卡 $1 http" "$HTTP" "200"
  check "建卡 $1 code" "$CODE" "0"
}

# ============================================================
say
say "===== ① 建卡不传附件 → 正常建成（口径不变）====="
mk_asset mk_noatt "$NAME_NOATT"
AS_NOATT=$(data '.data.id'); save AS_NOATT "$AS_NOATT"
NO_NOATT=$(SQ "select asset_no from biz_fixed_asset where id='$AS_NOATT'"); save NO_NOATT "$NO_NOATT"
checkne "资产 A 主键" "$AS_NOATT" ""
check "① 建卡不传附件：附件数 0" "$(att_cnt "$AS_NOATT")" "0"
check "① 建卡不传附件：状态 IN_USE（建卡即入役）" "$(status_of "$AS_NOATT")" "IN_USE"
check "① 建卡不传附件：不写「无附件启用」审计" "$(audit_cnt "$AS_NOATT")" "0"

# ============================================================
say
say "===== ③ 前端预检（attachment/check）：无附件 → can_proceed=false ====="
api chk_before GET "/business/attachment/check?doc_type=FIXED_ASSET&doc_id=$AS_NOATT&at=ACTIVATE" "$T_ADMIN"
check "③ check http" "$HTTP" "200"
check "③ 无附件时 can_proceed=false" "$(data '.data.can_proceed')" "false"
CHECK_HINT=$(data '.data.missing[0].hint')
checkne "③ 缺口 hint 非空（前端据此弹确认框）" "$CHECK_HINT" ""

# ============================================================
say
say "===== ② 停用 → 无附件启用 → 允许 + warning + 审计 ====="
api st_idle POST "/business/erp/asset/$AS_NOATT/status" "$T_ADMIN" \
  '{"target_status":"IDLE","reason":"E2E05-FIX-2 验收：先停用，便于验证无附件启用"}'
check "停用 http" "$HTTP" "200"
check "停用后状态" "$(status_of "$AS_NOATT")" "IDLE"
check "停用不写「无附件启用」审计" "$(audit_cnt "$AS_NOATT")" "0"

api st_enable POST "/business/erp/asset/$AS_NOATT/status" "$T_ADMIN" \
  '{"target_status":"IN_USE","reason":"E2E05-FIX-2 验收：发票未到，先恢复使用"}'
check "② 无附件启用 http（改造前是 400）" "$HTTP" "200"
check "② 无附件启用 code" "$CODE" "0"
WARN=$(warn_of)
checkne "② 响应带 warning" "$WARN" ""
case "$WARN" in
  *"发票或验收单"*) ok "② warning 点名缺什么：$WARN" ;;
  *) bad "② warning 未点名缺口：$WARN" ;;
esac
case "$WARN" in
  *"$OP_NOATT"*) ok "② warning 带审计操作码 $OP_NOATT" ;;
  *) bad "② warning 未带审计操作码：$WARN" ;;
esac
check "② 状态真的回到 IN_USE" "$(status_of "$AS_NOATT")" "IN_USE"
check "② 审计恰好 1 行" "$(audit_cnt "$AS_NOATT")" "1"
check "② 审计资产编号" "$(audit_field "$AS_NOATT" asset_no)" "$NO_NOATT"
check "② 审计原因" "$(audit_field "$AS_NOATT" reason)" "E2E05-FIX-2 验收：发票未到，先恢复使用"
check "② 审计 attachments_satisfied=false" "$(audit_field "$AS_NOATT" attachments_satisfied)" "false"
check "② 留痕有 ENABLE 行" \
  "$(SQ "select count(*) from biz_fixed_asset_change where asset_id='$AS_NOATT' and change_type='ENABLE'")" "1"

# ============================================================
say
say "===== ③b 补传附件后：预检 can_proceed=true ====="
ensure_asset_att "$AS_NOATT" fix2
check "附件落库（FIXED_ASSET）" "$(att_cnt "$AS_NOATT")" "1"
api chk_after GET "/business/attachment/check?doc_type=FIXED_ASSET&doc_id=$AS_NOATT&at=ACTIVATE" "$T_ADMIN"
check "③b 有附件时 can_proceed=true" "$(data '.data.can_proceed')" "true"

# ============================================================
say
say "===== ④ 有附件启用 → 正常（无 warning、无审计）====="
api st_idle2 POST "/business/erp/asset/$AS_NOATT/status" "$T_ADMIN" \
  '{"target_status":"IDLE","reason":"E2E05-FIX-2 验收：第二次停用（验证有附件启用）"}'
check "④ 二次停用 http" "$HTTP" "200"
api st_enable2 POST "/business/erp/asset/$AS_NOATT/status" "$T_ADMIN" \
  '{"target_status":"IN_USE","reason":"E2E05-FIX-2 验收：有附件启用"}'
check "④ 有附件启用 http" "$HTTP" "200"
check "④ 有附件启用 code" "$CODE" "0"
check "④ 无 warning（附件齐备不提示）" "$(warn_of)" ""
check "④ 状态 IN_USE" "$(status_of "$AS_NOATT")" "IN_USE"
check "④ 「无附件启用」审计仍是 1 行（有附件启用不记）" "$(audit_cnt "$AS_NOATT")" "1"

# ============================================================
say
say "===== ⑤ 建卡带附件 → 正常 ====="
mk_asset mk_att "$NAME_ATT"
AS_ATT=$(data '.data.id'); save AS_ATT "$AS_ATT"
checkne "资产 B 主键" "$AS_ATT" ""
ensure_asset_att "$AS_ATT" fix2b
check "⑤ 建卡后附件落库" "$(att_cnt "$AS_ATT")" "1"
check "⑤ 建卡带附件：状态 IN_USE" "$(status_of "$AS_ATT")" "IN_USE"
api chk_att GET "/business/attachment/check?doc_type=FIXED_ASSET&doc_id=$AS_ATT&at=ACTIVATE" "$T_ADMIN"
check "⑤ 建卡带附件：can_proceed=true" "$(data '.data.can_proceed')" "true"

# ============================================================
say
say "===== 汇总 ====="
say "夹具：A=$NO_NOATT（无附件启用 ×1 + 有附件启用 ×1） B（建卡带附件）RUN_TAG=$RUN_TAG"
summary
exit "$([ "$FAIL" = 0 ] && echo 0 || echo 1)"
