#!/usr/bin/env bash
# ============================================================
# ERP-R3 生产真机验收（**在生产机 144 上跑**，读 /etc/cloudpivot/cloudpivot.env）
#
#   bash /tmp/erpr3_prod_verify.sh
#
# 只做**只读**动作：GET 报表 / 导出 / 查库计数；不写任何业务数据。
# 断言的是「报表自己成立 + 与资产负债表互证」，不硬编码金额（生产数据会变）：
#   · 期末余额 = 期初 + 净增加                  （报表自身加减）
#   · 三活动净额之和 = 净增加额                  （分区汇总）
#   · 期末余额 = 资产负债表「货币资金」（同一期间） （跨表勾稽）
#   · 期末余额 = 总账现金类科目余额               （第三方视角）
#   · warnings 与 unclassified_count 必须成对出现（未分类不许静默）
# ============================================================
set -uo pipefail
BASE="http://127.0.0.1:8023/api"
set -a
. /etc/cloudpivot/cloudpivot.env
set +a
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); echo "[FAIL] $*"; }
expect() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi; }

PSQLA() { PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -Atc "$1" 2>&1; }
jq_get() {
    python3 -c '
import json,sys
node=json.loads(sys.argv[1])
for p in sys.argv[2].split("."):
    if p=="": continue
    node = node[int(p)] if isinstance(node,list) else node.get(p)
    if node is None: print("<null>"); raise SystemExit
print("true" if node is True else "false" if node is False else (json.dumps(node,ensure_ascii=False) if isinstance(node,(dict,list)) else node))
' "$1" "$2"
}
num_eq() { awk -v a="$1" -v b="$2" 'BEGIN{exit !(a+0==b+0)}'; }
expect_num() { if num_eq "$2" "$3"; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi; }

echo "== 库 $NIK_DATABASE_NAME @ 127.0.0.1"
echo "== 凭证 $(PSQLA "select count(*) from biz_voucher where deleted_at is null and status='POSTED'") 张；期间 $(PSQLA "select string_agg(period_code, ',' order by period_code) from biz_accounting_period")"

# ---------- 1. 迁移 / 配置落库 ----------
expect "分类行 12 条（V0185）" "12" "$(PSQLA "select count(*) from biz_cashflow_item where is_active=1")"
expect "迁移记录 success" "t" "$(PSQLA "select success from schema_migrations where name='erp_cashflow'")"
expect "权限节点 business:erp:report:cash-flow" "1" \
    "$(PSQLA "select count(*) from sys_menu where auth_code='business:erp:report:cash-flow' and menu_type=2 and is_visible and component='business/erp/report/cash-flow/index'")"
expect "非现金科目全部有分类（1012 按设计除外）" "0" \
    "$(PSQLA "select count(*) from biz_account c where c.deleted_at is null and left(c.account_code,4) not in ('1001','1002') and c.account_code <> '1012' and not exists (select 1 from biz_cashflow_item i cross join lateral unnest(string_to_array(i.account_pattern, ',')) as ca(code) where i.is_active=1 and (c.account_code = btrim(ca.code) or c.account_code like btrim(ca.code) || '.%'))")"

# ---------- 2. 登录 ----------
TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("data",{}).get("access_token",""))' 2>/dev/null)
[ ${#TOKEN} -gt 20 ] && ok "admin 登录（令牌 ${#TOKEN} 字节）" || { bad "登录失败"; exit 1; }
AUTH="Authorization: Bearer ${TOKEN}"

# ---------- 3. 现金流量表（现有数据出表） ----------
PERIOD=$(PSQLA "select period_code from biz_accounting_period order by period_code desc limit 1")
CF=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=$PERIOD")
expect "现有期间 $PERIOD 出表 code" "0" "$(jq_get "$CF" "code")"
expect "period 回显" "$PERIOD" "$(jq_get "$CF" "data.period")"
expect "简化版标记" "true" "$(jq_get "$CF" "data.simplified")"
expect "本期列名" "本期金额" "$(jq_get "$CF" "data.current_label")"
expect "累计列名" "本年累计" "$(jq_get "$CF" "data.ytd_label")"

OPEN=$(jq_get "$CF" "data.opening_balance.current")
NET=$(jq_get "$CF" "data.net_increase.current")
CLOSE=$(jq_get "$CF" "data.closing_balance.current")
BS=$(jq_get "$CF" "data.check.balance_sheet_cash")
LEDGER=$(jq_get "$CF" "data.check.closing_by_ledger")
OP_SUM=$(python3 -c '
import json,sys
d=json.loads(sys.argv[1])["data"]
print(sum(float(d[k]["net"]["current"]) for k in ("operating","investing","financing","other")))
' "$CF")
expect_num "期末余额 = 期初 + 净增加" "$(awk -v a="$OPEN" -v b="$NET" 'BEGIN{printf "%.4f", a+b}')" "$CLOSE"
expect_num "三活动净额之和 = 净增加额" "$OP_SUM" "$NET"
expect_num "期末余额 = 资产负债表「货币资金」" "$BS" "$CLOSE"
expect_num "期末余额 = 总账现金类科目余额" "$LEDGER" "$CLOSE"
expect "勾稽 balanced = true" "true" "$(jq_get "$CF" "data.check.balanced")"
expect_num "勾稽 diff = 0" "0" "$(jq_get "$CF" "data.check.diff")"
expect "diff_text" "0.00" "$(jq_get "$CF" "data.check.diff_text")"

# warnings 与未分类笔数必须成对（硬红线 4：未分类不许静默）
WARN_N=$(python3 -c 'import json,sys; print(len(json.loads(sys.argv[1])["data"]["warnings"]))' "$CF")
UNMAP_N=$(jq_get "$CF" "data.check.unclassified_count")
echo "== warnings（$WARN_N 条）: $(jq_get "$CF" "data.warnings")"
echo "== 未分类 ${UNMAP_N} 笔（流入 $(jq_get "$CF" "data.check.unclassified_inflow") / 流出 $(jq_get "$CF" "data.check.unclassified_outflow")）"
UNMAP_SAID=$(python3 -c '
import json,sys
d=json.loads(sys.argv[1])["data"]
print("yes" if any("未能分类" in w for w in d["warnings"]) else "no")
' "$CF")
if [ "$UNMAP_N" = "0" ]; then
    expect "未分类 0 笔时不许出现「未能分类」告警" "no" "$UNMAP_SAID"
else
    expect "未分类 $UNMAP_N 笔 → warnings 必须点名「未能分类」" "yes" "$UNMAP_SAID"
fi
if [ "$WARN_N" -gt 0 ]; then
    ok "生产库有 $WARN_N 条告警（未分类/口径差异必须可见，不静默）"
else
    ok "生产库当前无告警（现金分录全部可分类）"
fi

# ---------- 4. 跨表互证：资产负债表的货币资金 ----------
BSJ=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=$PERIOD")
expect "资产负债表 code" "0" "$(jq_get "$BSJ" "code")"
BS_AMT=$(python3 -c '
import json,sys
d=json.loads(sys.argv[1])["data"]
for r in d["assets"]:
    if r["item_code"]=="MONETARY_FUNDS":
        print(r["amount"]); raise SystemExit
print("<missing>")
' "$BSJ")
expect_num "资产负债表接口「货币资金」= 现金流量表勾稽值" "$BS_AMT" "$BS"

# ---------- 5. 菜单树 ----------
MENU=$(curl -s -H "$AUTH" "$BASE/system/menu/tree")
expect "菜单树 code" "0" "$(jq_get "$MENU" "code")"
case "$MENU" in
    *现金流量表*) ok "菜单树含「现金流量表」节点（侧边栏点得出来）" ;;
    *) bad "菜单树里没有「现金流量表」" ;;
esac

# ---------- 6. 导出 ----------
curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow/export?period=$PERIOD" \
    -o /tmp/erpr3_prod_cf.xlsx -D /tmp/erpr3_prod_cf.hdr
CT=$(grep -i '^content-type' /tmp/erpr3_prod_cf.hdr | tr -d '\r')
case "$CT" in
    *spreadsheetml*) ok "导出 Content-Type = xlsx" ;;
    *) bad "导出 Content-Type 不对：$CT" ;;
esac
if [ "$(head -c 2 /tmp/erpr3_prod_cf.xlsx)" = "PK" ]; then
    ok "导出合法 xlsx（PK 魔数，$(stat -c %s /tmp/erpr3_prod_cf.xlsx) 字节）"
else
    bad "导出文件不是 xlsx"
fi
grep -qi 'attachment' /tmp/erpr3_prod_cf.hdr && ok "导出带 attachment 头" || bad "导出缺 attachment 头"

# ---------- 7. 反例 ----------
C1=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-13")
expect "非法期间 2026-13 → 400" "400" "$C1"
C2=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2027-06")
expect "空月份没开账 2027-06 → 400" "400" "$C2"
C3=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2027-06")
expect "空月份 业务码 = 66010" "66010" "$(jq_get "$C3" "code")"
C4=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/business/erp/report/cash-flow?period=$PERIOD")
case "$C4" in
    401|403) ok "无令牌 → $C4" ;;
    *) bad "无令牌应为 401/403，实际 $C4" ;;
esac

# ---------- 8. 前端 dist ----------
if grep -rl "现金流量表" /opt/cloudpivot/frontend/dist/assets/*.js >/dev/null 2>&1; then
    ok "前端 dist 含「现金流量表」页面（点菜单出得来）"
else
    bad "前端 dist 里找不到现金流量表页面"
fi

echo ""
echo "=========================================="
echo "  PASS=$PASS  FAIL=$FAIL"
echo "=========================================="
[ "$FAIL" -eq 0 ] || exit 1
