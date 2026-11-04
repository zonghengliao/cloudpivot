#!/usr/bin/env bash
# FIN-2-FE 真机全链路（接口层）—— 隔离实例 18025 / 库 cloudpivot_fin2asset
# 链：建卡 → 看月折旧/净值 → 提交处置 → fin2gm 审批 → 作废处置 → 跑折旧 → 红冲 → 重提
set -uo pipefail
BASE=http://127.0.0.1:18025/api
OUT=${OUT:-/opt/cloudpivot/docs/evidence/20261105-fin2-fe/raw}
mkdir -p "$OUT"
FAIL=0

say() { printf '\n=== %s ===\n' "$*"; }

login() {
  curl -s -X POST "$BASE/system/auth/login" -H 'content-type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}"
}

say "① 登录 admin / fin2gm"
login admin 'Admin@123456' > "$OUT/00-login-admin.json"
A=$(jq -r '.data.access_token // empty' "$OUT/00-login-admin.json")
login fin2gm 'Admin@123456' > "$OUT/00-login-fin2gm.json"
G=$(jq -r '.data.access_token // empty' "$OUT/00-login-fin2gm.json")
echo "admin token: ${#A} chars / fin2gm token: ${#G} chars"
[ -n "$A" ] && [ -n "$G" ] || { echo "登录失败"; jq . "$OUT/00-login-admin.json" "$OUT/00-login-fin2gm.json"; exit 1; }
AH="Authorization: Bearer $A"
GH="Authorization: Bearer $G"

say "② 资产分类下拉（5 条种子）"
curl -s "$BASE/business/erp/asset-category/list" -H "$AH" > "$OUT/01-category-list.json"
jq -r '.data | "分类条数=\(length)", (.[] | "  \(.category_code) \(.category_name) \(.useful_life_months)月 残值率\(.salvage_rate)")' "$OUT/01-category-list.json"
CAT=$(jq -r '.data[] | select(.category_code=="ELEC") | .id' "$OUT/01-category-list.json")
echo "选分类 ELEC=$CAT"

say "③ 现有台账（取部门 + 建卡前快照）"
curl -s "$BASE/business/erp/asset/page?page=1&page_size=5" -H "$AH" > "$OUT/02-asset-page-before.json"
jq -r '.data | "台账条数=\(.total)", (.list[] | "  \(.asset_no) \(.asset_name) \(.status) 原值\(.original_value) 月折旧\(.monthly_depreciation)")' "$OUT/02-asset-page-before.json"
DEPT=$(jq -r '.data.list[0].use_dept_id // empty' "$OUT/02-asset-page-before.json")
DEPTNAME=$(jq -r '.data.list[0].use_dept_name // empty' "$OUT/02-asset-page-before.json")
echo "部门=$DEPTNAME ($DEPT)"

say "④ 建卡（原值 100000 / 现付 / 购入 2026-11-10 → 起提 2026-12-01）"
curl -s -X POST "$BASE/business/erp/asset" -H "$AH" -H 'content-type: application/json' \
  -d "{\"asset_name\":\"FIN-2-FE 真机验收显示器\",\"category_id\":\"$CAT\",\"spec\":\"FE-14inch\",\"use_dept_id\":\"$DEPT\",\"location\":\"A座3层\",\"original_value\":\"100000.0000\",\"tax_amount\":\"0.0000\",\"purchase_date\":\"2026-11-10\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CASH\",\"remark\":\"【FIN-2-FE】真机验收\"}" \
  > "$OUT/03-asset-create.json"
jq -c . "$OUT/03-asset-create.json"
AID=$(jq -r '.data.id // empty' "$OUT/03-asset-create.json")
ANO=$(jq -r '.data.no // empty' "$OUT/03-asset-create.json")
echo "资产 id=$AID 编号=$ANO"
[ -n "$AID" ] || { echo "建卡失败"; exit 1; }

say "⑤ 详情：月折旧 / 起提日 / 已提月数 / 剩余可提（明细四细节 #1 #4 的数据源）"
curl -s "$BASE/business/erp/asset/$AID" -H "$AH" > "$OUT/04-asset-detail.json"
jq -r '.data as $d | $d.asset | "编号\(.asset_no) 状态\(.status)(\(.status_label)) 原值\(.original_value) 残值\(.residual_value) 可折旧\(.depreciable_base) 月折旧\(.monthly_depreciation)", "起提日\(.start_depreciation_date) 已提\(.depreciation_months_done)月 累计\(.accumulated_depreciation) 净值\(.net_value)", "剩余可提 \($d.months_left) 月 / \($d.amount_left) 元  凭证=\(.voucher_id // "—") 来源\(.source_type)"' "$OUT/04-asset-detail.json"

say "⑥ 提交处置（SALE 出售 / 售价 10000 / 销项税 1300）"
curl -s -X POST "$BASE/business/erp/asset/$AID/dispose" -H "$AH" -H 'content-type: application/json' \
  -d '{"disposal_type":"SALE","disposal_date":"2026-11-05","reason":"【FIN-2-FE】验收：资产出售处置","sale_price":"10000.0000","output_tax":"1300.0000"}' \
  > "$OUT/05-asset-dispose.json"
jq -c '{code,msg,status:.data.asset.status,pending:.data.pending_disposal}' "$OUT/05-asset-dispose.json"

say "⑦ 审批前资产仍在役（审批通过才动账）"
curl -s "$BASE/business/erp/asset/$AID" -H "$AH" | jq -r '.data.asset | "状态=\(.status)(\(.status_label)) 处置类型=\(.disposal_type // "—")"'

say "⑧ fin2gm 待办（按 form_data.asset_no 精确定位）"
curl -s "$BASE/business/flow/task/todo?page=1&page_size=20" -H "$GH" > "$OUT/06-gm-todo.json"
jq -r '.data | "待办总数=\(.total)", (.list[] | "  \(.id) \(.title) \(.current_node_name) \(.status)")' "$OUT/06-gm-todo.json"
FID=$(jq -r --arg no "$ANO" '.data.list[] | select((.form_data.asset_no // "") == $no) | .id' "$OUT/06-gm-todo.json" | head -1)
echo "审批任务 id=$FID"

if [ -n "$FID" ]; then
  say "⑨ fin2gm 审批通过"
  curl -s -X POST "$BASE/business/flow/task/$FID/approve" -H "$GH" -H 'content-type: application/json' \
    -d '{"comment":"【FIN-2-FE】验收：同意处置"}' > "$OUT/07-gm-approve.json"
  jq -c '{code,msg,status:.data.status,instance_status:.data.instance_status}' "$OUT/07-gm-approve.json"
  sleep 2
  say "⑩ 审批后：资产 DISPOSED + 处置三列 + 处置凭证 + 变动时间线"
  curl -s "$BASE/business/erp/asset/$AID" -H "$AH" > "$OUT/08-asset-after-approve.json"
  jq -r '.data.asset | "状态=\(.status)(\(.status_label)) 处置=\(.disposal_type // "—")/\(.disposal_date // "—") 净值=\(.net_value) 处置凭证=\(.disposal_voucher_id // "—")"' "$OUT/08-asset-after-approve.json"
  jq -r '.data.changes[]? | "  变动 \(.change_type) \(.change_date) \(.reason // "")"' "$OUT/08-asset-after-approve.json"
else
  echo "!! 没找到 fin2gm 的处置待办"; FAIL=1
fi

say "⑪ 作废处置（admin）"
curl -s -X POST "$BASE/business/erp/asset/$AID/dispose/cancel" -H "$AH" -H 'content-type: application/json' \
  -d '{"reason":"【FIN-2-FE】验收：撤销本次处置"}' > "$OUT/09-asset-dispose-cancel.json"
jq -c '{code,msg,status:.data.asset.status,disposal_type:.data.asset.disposal_type,disposal_date:.data.asset.disposal_date}' "$OUT/09-asset-dispose-cancel.json"

say "⑫ 计提 2026-12"
curl -s -X POST "$BASE/business/erp/depreciation/run" -H "$AH" -H 'content-type: application/json' \
  -d '{"period_code":"2026-12"}' > "$OUT/10-dep-run.json"
jq -c '{code,msg,outcome:.data.outcome,batch_no:.data.batch.batch_no,batch_id:.data.batch.id,period:.data.batch.period_code,status:.data.batch.status,total:.data.batch.total_amount,asset_count:.data.batch.asset_count,voucher:.data.batch.voucher_id,message:.data.message}' "$OUT/10-dep-run.json"
BID=$(jq -r '.data.batch.id // empty' "$OUT/10-dep-run.json")

say "⑬ 批次分页"
curl -s "$BASE/business/erp/depreciation/page?page=1&page_size=10" -H "$AH" > "$OUT/11-dep-page.json"
jq -r '.data | "批次数=\(.total)", (.list[] | "  \(.period_code) \(.status) 合计\(.total_amount) 台数\(.asset_count) 批次\(.batch_no)")' "$OUT/11-dep-page.json"
[ -n "$BID" ] || BID=$(jq -r '.data.list[0].id // empty' "$OUT/11-dep-page.json")

say "⑭ 批次明细"
curl -s "$BASE/business/erp/depreciation/$BID" -H "$AH" > "$OUT/12-dep-detail.json"
jq -r '.data | "批次 \(.batch.period_code) \(.batch.status) 合计\(.batch.total_amount) 凭证\(.batch.voucher_id // "—")", "明细 \(.details|length) 条", (.details[] | "  \(.asset_no) \(.asset_name) 部门\(.use_dept_name) 金额\(.amount) 状态\(.status)")' "$OUT/12-dep-detail.json"

say "⑮ 卡片回写"
curl -s "$BASE/business/erp/asset/$AID" -H "$AH" > "$OUT/13-asset-after-dep.json"
jq -r '.data as $d | $d.asset | "累计折旧=\(.accumulated_depreciation) 净值=\(.net_value) 已提\(.depreciation_months_done)月 剩余可提 \($d.months_left) 月 / \($d.amount_left) 元"' "$OUT/13-asset-after-dep.json"

say "⑯ 整批红冲"
curl -s -X POST "$BASE/business/erp/depreciation/$BID/reverse" -H "$AH" -H 'content-type: application/json' \
  -d '{"reason":"【FIN-2-FE】验收：整批红冲"}' > "$OUT/14-dep-reverse.json"
jq -r '.data.batch | "批次 \(.period_code) \(.status) 合计\(.total_amount) 红字凭证\(.reverse_voucher_id // "—")"' "$OUT/14-dep-reverse.json"
jq -r '.data.details[]? | "  明细 \(.asset_no) \(.status)"' "$OUT/14-dep-reverse.json"

say "⑰ 红冲后回滚 + 重跑同期间（REVERSED_RERUN）"
curl -s "$BASE/business/erp/asset/$AID" -H "$AH" | jq -r '.data.asset | "累计折旧=\(.accumulated_depreciation) 净值=\(.net_value) 已提\(.depreciation_months_done)月"'
curl -s -X POST "$BASE/business/erp/depreciation/run" -H "$AH" -H 'content-type: application/json' \
  -d '{"period_code":"2026-12"}' > "$OUT/15-dep-rerun.json"
jq -c '{code,msg,outcome:.data.outcome,total:.data.batch.total_amount,batch_no:.data.batch.batch_no,status:.data.batch.status}' "$OUT/15-dep-rerun.json"

say "⑱ 错误码回归"
curl -s -X POST "$BASE/business/erp/depreciation/$BID/reverse" -H "$AH" -H 'content-type: application/json' -d '{"reason":"x"}' > "$OUT/16-err-reverse-short.json"
jq -c '{code,msg}' "$OUT/16-err-reverse-short.json"
curl -s -X POST "$BASE/business/erp/asset/$AID/dispose" -H "$AH" -H 'content-type: application/json' \
  -d '{"disposal_type":"NOPE","disposal_date":"2026-11-05","reason":"非法类型"}' > "$OUT/17-err-disposal-type.json"
jq -c '{code,msg}' "$OUT/17-err-disposal-type.json"

say "⑲ 结算快照"
jq -r '.data.asset | "最终 \(.asset_no) 状态=\(.status)(\(.status_label)) 累计=\(.accumulated_depreciation) 净值=\(.net_value)"' "$OUT/13-asset-after-dep.json"
echo
echo "FAIL=$FAIL  ASSET=$ANO  BATCH=$BID  证据目录=$OUT"
