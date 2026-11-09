#!/usr/bin/env bash
# E2E05-FIX-3（卡 t_e99f0719）真机验收：动作码 / 只读字段 / 作废处置留痕日期
#
# 环境：145 测试机 · cloudpivot_dev 库 · 隔离后端实例（真 token curl）
#
# 验收（与卡面 ①~③ 一一对应）：
#
#   ① 部门转移 / 使用人变更 → 时间线是「部门转移」「使用人变更」而不是「IN_USE → IN_USE」：
#      · API `GET /asset/{id}` 的 changes 行：change_type = TRANSFER / CUSTODIAN，change_label 有中文名，
#        from_status = to_status = IN_USE（状态确实没变）；前端 changeSummary 已改为只列真正变了的字段
#        （单测见 frontend/tests/unit/fin2-asset.test.ts）；
#      · 数据层 `biz_status_log.action` = TRANSFER / CUSTODIAN（V0365）—— 消费者不必再猜 reason 前缀。
#
#   ② `PUT /asset/{id}` 带 original_value / category_id 且与现值不同 → **400** 明确提示
#      （改造前 serde 静默丢字段：200 + 库里没变）；回传现值（整表单回传）照常 200。
#
#   ③ 作废处置后 `biz_fixed_asset_change`(ENABLE) 的 change_date = **原处置日**（不是今天）：
#      显式传 change_date 时以传的为准。
#
# 号段：资产名 E2E05FIX3-<RUN_TAG>-* / 部门 E2E5F3-D2 / 期间 2030-01（已 OPEN）。
# 每次运行用新的 RUN_TAG（默认时间戳）⇒ 反复跑不会互相干扰（夹具留在库里，见 README）。
#
# 用法：BASE=http://127.0.0.1:8038/api bash scripts/verify/e2e05fix3_p4x3_e2e.sh
# 退出码：0 = 全绿；非 0 = 有 FAIL（明细见 $LOG）
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh

YEAR=2030
PERIOD="${YEAR}-01"
PURCHASE_DATE="${YEAR}-01-05"
DISPOSAL_DATE="${YEAR}-01-15"
RUN_TAG=${RUN_TAG:-$(date +%m%d%H%M)}
CAT_CODE="E2E5F2_60"          # FIX-2 建过的 60 月设备分类（幂等复用）
NAME_A="E2E05FIX3-$RUN_TAG-A"
NAME_B="E2E05FIX3-$RUN_TAG-B"
TODAY=$(date +%F)

asset_id_of() { SQ "select id from biz_fixed_asset where asset_name='$1' and deleted_at is null order by created_at desc limit 1"; }
orig_of()     { SQ "select original_value from biz_fixed_asset where id='$1'"; }
log_actions() { SQ "select coalesce(string_agg(coalesce(action,'NULL'),',' order by created_at),'-') from biz_status_log where entity_type='FIXED_ASSET' and entity_id='$1'::uuid"; }
# 只取业务动作码（action 非 NULL）：状态流转（含建卡 NULL → IN_USE）不在其中
log_moves()   { SQ "select coalesce(string_agg(action,',' order by created_at),'-') from biz_status_log where entity_type='FIXED_ASSET' and entity_id='$1'::uuid and action is not null"; }
log_count()   { SQ "select count(*) from biz_status_log where entity_type='FIXED_ASSET' and entity_id='$1'::uuid"; }
enable_date() { SQ "select string_agg(to_char(change_date,'YYYY-MM-DD'),',') from biz_fixed_asset_change where asset_id='$1' and change_type='ENABLE'"; }
chg_field()   { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

say "===== F0 登录 + 前置（分类 / 部门 / 期间 $PERIOD）====="
login_admin
CAT=$(SQ "select id from biz_asset_category where category_code='$CAT_CODE' and deleted_at is null limit 1")
checkne "分类 $CAT_CODE" "$CAT" ""
DEPT1=$(SQ "select id from sys_dept where dept_name='演练设备部' and deleted_at is null limit 1")
checkne "部门 演练设备部（转出）" "$DEPT1" ""
DEPT2=$(SQ "select id from sys_dept where dept_name='演练设备二部' and deleted_at is null limit 1")
if [ -z "$DEPT2" ]; then
  api mk_d2 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E5F3-D2","dept_name":"演练设备二部","status":1,"sort_order":93}'
  check "建部门 演练设备二部 http" "$HTTP" "200"
  DEPT2=$(data '.data.id')
fi
checkne "部门 演练设备二部（转入）" "$DEPT2" ""
check "期间 $PERIOD 已开" "$(SQ "select status from biz_accounting_period where period_code='$PERIOD'")" "OPEN"
HOLDER=$(SQ "select id from sys_user where username='e2e05_gm'")
checkne "承接人 e2e05_gm" "$HOLDER" ""
save CAT "$CAT"; save DEPT1 "$DEPT1"; save DEPT2 "$DEPT2"; save HOLDER "$HOLDER"

mk_asset() { # mk_asset <tag> <name>
  api "$1" POST /business/erp/asset "$T_ADMIN" \
    "{\"asset_name\":\"$2\",\"category_id\":\"$CAT\",\"spec\":\"E2E05FIX3\",\"use_dept_id\":\"$DEPT1\",\"location\":\"演练车间 F3\",\"original_value\":\"12000\",\"tax_amount\":\"0\",\"purchase_date\":\"$PURCHASE_DATE\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"E2E05-FIX-3 验收 $RUN_TAG\"}"
  check "建卡 $1 http" "$HTTP" "200"
  check "建卡 $1 code" "$CODE" "0"
}

# ============================================================
say
say "===== ①② 夹具 A：改卡 + 部门转移 + 使用人变更 ====="
mk_asset mk_a "$NAME_A"
AS_A=$(data '.data.id'); save AS_A "$AS_A"
checkne "夹具 A 主键" "$AS_A" ""
check "A 原值（建卡）" "$(orig_of "$AS_A")" "12000.0000"
check "A 建卡即入役" "$(SQ "select status from biz_fixed_asset where id='$AS_A'")" "IN_USE"

say
say "----- ② PUT 带 original_value=999999 → 400（改造前是静默 200）-----"
api upd_value PUT "/business/erp/asset/$AS_A" "$T_ADMIN" '{"original_value":"999999"}'
check "② 改原值 http" "$HTTP" "400"
MSG=$(printf '%s' "$BODY" | jq -r '.msg // ""')
case "$MSG" in
  *"原值不可修改"*) ok "② 报文点名「原值不可修改」：$MSG" ;;
  *) bad "② 报文未点名：$MSG" ;;
esac
case "$MSG" in
  *"999999"*) ok "② 报文给出请求值 999999" ;;
  *) bad "② 报文缺请求值：$MSG" ;;
esac
check "② 库里原值一分没动" "$(orig_of "$AS_A")" "12000.0000"
check "② 没有新增状态日志（请求被挡在 UPDATE 之前；只有建卡那条）" "$(log_count "$AS_A")" "1"

say
say "----- ② PUT 带别的 category_id → 400 -----"
api upd_cat PUT "/business/erp/asset/$AS_A" "$T_ADMIN" '{"category_id":"22222222-2222-4222-8222-222222222222"}'
check "② 改分类 http" "$HTTP" "400"
MSG=$(printf '%s' "$BODY" | jq -r '.msg // ""')
case "$MSG" in
  *"资产分类不可修改"*) ok "② 报文点名「资产分类不可修改」：$MSG" ;;
  *) bad "② 报文未点名：$MSG" ;;
esac
check "② 库里分类没动 " "$(SQ "select category_id from biz_fixed_asset where id='$AS_A'")" "$CAT"

say
say "----- ② PUT 整表单回传现值 + 改名 → 200（不误伤）-----"
api upd_ok PUT "/business/erp/asset/$AS_A" "$T_ADMIN" \
  "{\"asset_name\":\"$NAME_A-改名\",\"original_value\":\"12000.0000\",\"category_id\":\"$CAT\",\"use_dept_id\":\"$DEPT1\",\"location\":\"演练车间 F3\"}"
check "② 回传现值 http（不该被拒）" "$HTTP" "200"
check "② 回传现值 code" "$CODE" "0"
check "② 改名生效" "$(SQ "select asset_name from biz_fixed_asset where id='$AS_A'")" "$NAME_A-改名"
check "② 原值仍是 12000" "$(orig_of "$AS_A")" "12000.0000"

say
say "----- ① 部门转移：状态不变，日志带 action=TRANSFER -----"
api transfer POST "/business/erp/asset/$AS_A/transfer" "$T_ADMIN" \
  "{\"to_dept_id\":\"$DEPT2\",\"change_date\":\"$PERIOD-20\",\"reason\":\"E2E05-FIX-3 验收：组织调整\"}"
check "① 转移 http" "$HTTP" "200"
check "① 转移 code" "$CODE" "0"
check "① 转移后状态仍是 IN_USE（from = to 的来源）" "$(SQ "select status from biz_fixed_asset where id='$AS_A'")" "IN_USE"
check "① 转移后使用部门" "$(SQ "select use_dept_id from biz_fixed_asset where id='$AS_A'")" "$DEPT2"
check "① biz_status_log.action = TRANSFER（数据层就分清）" "$(log_moves "$AS_A")" "TRANSFER"
check "① 日志 from/to 都是 IN_USE（状态确实没变，不伪造）" \
  "$(SQ "select string_agg(from_status||'→'||to_status,',') from biz_status_log where entity_type='FIXED_ASSET' and entity_id='$AS_A'::uuid")" "IN_USE→IN_USE"

api detail_a GET "/business/erp/asset/$AS_A" "$T_ADMIN"
check "① 详情 http" "$HTTP" "200"
check "① 时间线首行 change_type" "$(chg_field '.data.changes[0].change_type')" "TRANSFER"
check "① 时间线首行 change_label（前端据此显示「部门转移」）" "$(chg_field '.data.changes[0].change_label')" "部门转移"
check "① 时间线首行 from_status = to_status = IN_USE" \
  "$(printf '%s' "$BODY" | jq -r '.data.changes[0] | (.from_status + "→" + .to_status)')" "IN_USE→IN_USE"
check "① 时间线首行部门变了（真正要显示的内容）" \
  "$(printf '%s' "$BODY" | jq -r '.data.changes[0] | (.from_dept_name + "→" + .to_dept_name) | @text')" \
  "$(SQ "select d1.dept_name||'→'||d2.dept_name from sys_dept d1, sys_dept d2 where d1.id='$DEPT1' and d2.id='$DEPT2'")"

say
say "----- ① 使用人变更：日志带 action=CUSTODIAN -----"
api custodian POST "/business/erp/asset/$AS_A/custodian" "$T_ADMIN" \
  "{\"to_user_id\":\"$HOLDER\",\"change_date\":\"$PERIOD-21\",\"reason\":\"E2E05-FIX-3 验收：离职交接\"}"
check "① 使用人变更 http" "$HTTP" "200"
check "① 使用人变更 code" "$CODE" "0"
check "① 变更后状态仍是 IN_USE" "$(SQ "select status from biz_fixed_asset where id='$AS_A'")" "IN_USE"
check "① 使用人变更日志 action = CUSTODIAN（两条按写入顺序）" "$(log_moves "$AS_A")" "TRANSFER,CUSTODIAN"
check "① 承接人写对" "$(SQ "select custodian_id from biz_fixed_asset where id='$AS_A'")" "$HOLDER"

# ============================================================
say
say "===== ③ 夹具 B：报废处置 → 作废处置，留痕日期 = 原处置日 ====="
mk_asset mk_b "$NAME_B"
AS_B=$(data '.data.id'); save AS_B "$AS_B"
NO_B=$(SQ "select asset_no from biz_fixed_asset where id='$AS_B'"); save NO_B "$NO_B"
checkne "夹具 B 主键" "$AS_B" ""

api dispose POST "/business/erp/asset/$AS_B/dispose" "$T_ADMIN" \
  "{\"disposal_type\":\"SCRAP\",\"disposal_date\":\"$DISPOSAL_DATE\",\"reason\":\"E2E05-FIX-3 验收：B 设备报废\"}"
check "③ 提交处置 http" "$HTTP" "200"
check "③ 提交处置 code" "$CODE" "0"
INST_B=$(SQ "select id from biz_flow_instance where biz_no='$NO_B' and biz_type='ASSET_DISPOSAL' order by created_at desc limit 1")
checkne "③ 审批实例已创建" "$INST_B" ""
TASK_B=$(SQ "select t.id from biz_flow_task t where t.instance_id='$INST_B' and t.status='PENDING' and t.assignee_id=(select id from sys_user where username='e2e05_gm')")
checkne "③ GM 待办（DB）" "$TASK_B" ""
T_GM=$(login e2e05_gm 'Drill@123456' e2e05gm) || exit 1
save T_GM "$T_GM"
api gm_todo GET "/business/flow/task/todo?page=1&page_size=50" "$T_GM"
check "③ GM 待办列表含该任务" "$(data "[.data.list[]|select(.id==\"$TASK_B\")]|length")" "1"
api approve POST "/business/flow/task/$TASK_B/approve" "$T_GM" '{"comment":"E2E05-FIX-3 验收：同意报废"}'
check "③ 审批 http" "$HTTP" "200"
sleep 2
check "③ 卡片 DISPOSED" "$(SQ "select status from biz_fixed_asset where id='$AS_B'")" "DISPOSED"
check "③ 处置日已落库（$DISPOSAL_DATE）" "$(SQ "select to_char(disposal_date,'YYYY-MM-DD') from biz_fixed_asset where id='$AS_B'")" "$DISPOSAL_DATE"

say
say "----- ③ 作废处置（不传 change_date）→ 留痕日期必须是原处置日，不是今天（$TODAY）-----"
api cancel1 POST "/business/erp/asset/$AS_B/dispose/cancel" "$T_ADMIN" '{"reason":"E2E05-FIX-3 验收：处置依据不成立，作废"}'
check "③ 作废处置 http" "$HTTP" "200"
check "③ 作废处置 code" "$CODE" "0"
check "③ 作废后状态回 IN_USE" "$(SQ "select status from biz_fixed_asset where id='$AS_B'")" "IN_USE"
check "③ ENABLE 留痕日期 = 原处置日（改造前是今天）" "$(enable_date "$AS_B")" "$DISPOSAL_DATE"
check "③ 确实不是今天" "$([ "$(enable_date "$AS_B")" = "$TODAY" ] && echo TODAY || echo NOT_TODAY)" "NOT_TODAY"
check "③ 留痕 from→to 仍是 DISPOSED→IN_USE" \
  "$(SQ "select from_status||'→'||to_status from biz_fixed_asset_change where asset_id='$AS_B' and change_type='ENABLE'")" "DISPOSED→IN_USE"

say
say "----- ③b 显式传 change_date → 以传的为准 -----"
api dispose2 POST "/business/erp/asset/$AS_B/dispose" "$T_ADMIN" \
  "{\"disposal_type\":\"SCRAP\",\"disposal_date\":\"$PERIOD-25\",\"reason\":\"E2E05-FIX-3 验收：B 二次报废\"}"
check "③b 二次提交处置 http" "$HTTP" "200"
INST_B2=$(SQ "select id from biz_flow_instance where biz_no='$NO_B' and biz_type='ASSET_DISPOSAL' order by created_at desc limit 1")
TASK_B2=$(SQ "select t.id from biz_flow_task t where t.instance_id='$INST_B2' and t.status='PENDING' and t.assignee_id=(select id from sys_user where username='e2e05_gm')")
checkne "③b 二次审批待办" "$TASK_B2" ""
api approve2 POST "/business/flow/task/$TASK_B2/approve" "$T_GM" '{"comment":"E2E05-FIX-3 验收：同意二次报废"}'
check "③b 审批 http" "$HTTP" "200"
sleep 2
check "③b 卡片再次 DISPOSED" "$(SQ "select status from biz_fixed_asset where id='$AS_B'")" "DISPOSED"
api cancel2 POST "/business/erp/asset/$AS_B/dispose/cancel" "$T_ADMIN" \
  "{\"reason\":\"E2E05-FIX-3 验收：显式指定留痕日期\",\"change_date\":\"${YEAR}-02-03\"}"
check "③b 显式日期作废 http" "$HTTP" "200"
check "③b 显式 change_date 覆盖原处置日" "$(enable_date "$AS_B")" "$DISPOSAL_DATE,${YEAR}-02-03"

# ============================================================
say
say "===== 汇总 ====="
say "夹具：A=$NAME_A-改名（转移 + 使用人变更） B=$NAME_B（作废处置 ×2）RUN_TAG=$RUN_TAG"
summary
exit "$([ "$FAIL" = 0 ] && echo 0 || echo 1)"
