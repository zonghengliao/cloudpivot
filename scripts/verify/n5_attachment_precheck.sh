#!/bin/bash
# ============================================================================
# N-5 附件预检：8 个必传单据的后端判据实测（真实 token curl）
#
# 这个脚本验的是**前端按钮状态所依赖的那条判据**：`GET /api/business/attachment/check`
# 对 8 类必传单据是否都返回 200 + `{can_proceed, missing[], hint, at_label}`，
# 以及「缺附件 → can_proceed=false」「已有合规附件 → can_proceed=true」两侧都会翻转。
# 前端之所以敢用「按钮 disabled + 红字」而不是自己算，就是因为这里只有一条判据。
#
# 用法（在能访问 145 的机器上跑；需要 /etc/cloudpivot/cloudpivot-dev.env 或等价 env）：
#   bash scripts/verify/n5_attachment_precheck.sh [输出目录]
#
# 依赖：curl / jq / python3 / psql
# 令牌只在本进程内使用，**不写进任何输出文件**（证据可入仓）。
# ============================================================================
set -u

OUT_DIR="${1:-/tmp/n5-attachment-precheck}"
mkdir -p "$OUT_DIR"

if [ -f /etc/cloudpivot/cloudpivot-dev.env ]; then
  # shellcheck disable=SC1091
  set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
fi

BASE="${N5_BASE_URL:-http://127.0.0.1:80}"
ADMIN_USER="${N5_ADMIN_USER:-admin}"
ADMIN_PASS="${N5_ADMIN_PASSWORD:-${NIK_ADMIN_PASSWORD:-}}"
DB_USER="${NIK_DATABASE_USER:-}"; DB_PASS="${NIK_DATABASE_PASSWORD:-}"
DB_NAME="${NIK_DATABASE_NAME:-}"; DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"

FAIL=0
pass() { echo "  [PASS] $*"; }
fail() { echo "  [FAIL] $*"; FAIL=$((FAIL + 1)); }

if [ -z "$ADMIN_PASS" ] || [ -z "$DB_NAME" ]; then
  echo "缺少 NIK_ADMIN_PASSWORD / NIK_DATABASE_* （先 source /etc/cloudpivot/cloudpivot-dev.env）"
  exit 2
fi
export PGPASSWORD="$DB_PASS"
psql_q() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -F'|' -c "$1"; }

echo "==================== 0. 环境 ===================="
echo "BASE=$BASE  DB=$DB_NAME@$DB_HOST:$DB_PORT"
curl -s -o /dev/null -w 'health=%{http_code}\n' "$BASE/health"
psql_q "select doc_type, required_at, allow_exts, coalesce(condition_sql,'-'), remark
          from biz_attachment_rule where deleted_at is null and enabled
         order by doc_type, required_at;" | tee "$OUT_DIR/00_rules.txt"
NUMS=$(wc -l < "$OUT_DIR/00_rules.txt")
[ "$NUMS" -ge 8 ] && pass "规则表启用中 $NUMS 条（8 类单据，INVOICE 拆两条）" || fail "规则表只有 $NUMS 条"

echo "==================== 1. 登录（真 token） ===================="
LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}")
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  echo "登录失败（已脱敏）：$(printf '%s' "$LOGIN" | jq -c 'del(.data.access_token,.data.refresh_token)')"
  exit 1
fi
echo "登录成功：token 长度 ${#TOKEN}（不落盘）"
pass "登录拿到 access_token"

# 每类单据各取一个现存 id（只读；库里没有该类单据时该条记为 SKIP）
declare -A TABLE=(
  [INVOICE]=biz_invoice
  [RECEIPT]=biz_receipt
  [PAYMENT]=biz_payment
  [EXPENSE_REPORT]=biz_expense_report
  [FIXED_ASSET]=biz_fixed_asset
  [STOCK_TAKE]=biz_stock_take
  [PURCHASE_RETURN]=biz_purchase_return
  [SALES_RETURN]=biz_sales_return
)
declare -A DOC_ID=()
declare -A AT=()
echo "==================== 2. 取探针单据（每类一个） ===================="
: > "$OUT_DIR/01_probe_docs.txt"
for DT in "${!TABLE[@]}"; do
  ROW=$(psql_q "select doc_type, required_at from biz_attachment_rule
                 where deleted_at is null and enabled and doc_type = '$DT'
                 order by required_at limit 1;")
  AT[$DT]="${ROW#*|}"
  ID=$(psql_q "select id from ${TABLE[$DT]} where deleted_at is null order by id limit 1;")
  DOC_ID[$DT]="$ID"
  echo "$DT|${AT[$DT]}|${TABLE[$DT]}|${ID:--}" >> "$OUT_DIR/01_probe_docs.txt"
done
sort "$OUT_DIR/01_probe_docs.txt" | tee "$OUT_DIR/01_probe_docs.sorted.txt"
while IFS='|' read -r DT AT_ TBL ID; do
  [ "$ID" = "-" ] && echo "  [SKIP] $DT：${TBL} 里没有存活单据"
done < "$OUT_DIR/01_probe_docs.sorted.txt"

echo "==================== 3. 8 类单据逐一调 /attachment/check ===================="
: > "$OUT_DIR/02_check_all.jsonl"
for DT in INVOICE RECEIPT PAYMENT EXPENSE_REPORT FIXED_ASSET STOCK_TAKE PURCHASE_RETURN SALES_RETURN; do
  ID="${DOC_ID[$DT]}"
  if [ -z "$ID" ] || [ "$ID" = "-" ]; then
    echo "  [SKIP] $DT 没有探针单据"
    continue
  fi
  URL="$BASE/api/business/attachment/check?doc_type=$DT&doc_id=$ID&at=${AT[$DT]}"
  CODE=$(curl -s -o /tmp/n5_check_body.json -w '%{http_code}' \
    -H "Authorization: Bearer $TOKEN" "$URL")
  echo "--- $DT / ${AT[$DT]} / $ID → HTTP $CODE" | tee -a "$OUT_DIR/02_check_all.jsonl"
  jq -c . /tmp/n5_check_body.json | tee -a "$OUT_DIR/02_check_all.jsonl"
  [ "$CODE" = "200" ] || { fail "$DT 预检非 200（$CODE）"; continue; }
  CAN=$(jq -r '.data.can_proceed' /tmp/n5_check_body.json)
  LABEL=$(jq -r '.data.at_label' /tmp/n5_check_body.json)
  case "$LABEL" in
    提交|确认|生效) pass "$DT at_label=$LABEL" ;;
    *) fail "$DT at_label 异常：$LABEL" ;;
  esac
  if [ "$CAN" = "false" ]; then
    MISS=$(jq -r '.data.missing | length' /tmp/n5_check_body.json)
    NEED=$(jq -r '.data.missing[0].need // "-"' /tmp/n5_check_body.json)
    HINT=$(jq -r '.data.hint // "-"' /tmp/n5_check_body.json)
    pass "$DT 缺附件 → can_proceed=false（$MISS 条，need=$NEED，红字「$HINT」）"
  else
    pass "$DT 已有合规附件 → can_proceed=true（按钮应亮）"
  fi
done

echo "==================== 4. need 与规则表 allow_exts 逐字一致 ===================="
python3 - "$OUT_DIR/02_check_all.jsonl" "$OUT_DIR/00_rules.txt" <<'PY'
import json, sys
checks, bad = [], 0
for line in open(sys.argv[1], encoding='utf-8'):
    line = line.strip()
    if not line.startswith('{'):
        continue
    body = json.loads(line)
    data = body.get('data') or {}
    if not data:
        continue
    checks.append(data)
rules = {}
for line in open(sys.argv[2], encoding='utf-8'):
    doc_type, at, exts, _cond, _remark = line.rstrip('\n').split('|')
    rules.setdefault((doc_type, at), set()).add(frozenset(exts.split(',')))
for data in checks:
    key = (data.get('doc_type'), data.get('at'))
    for miss in data.get('missing') or []:
        need = miss.get('need')
        allowed = rules.get(key, set())
        ok = need is None or frozenset(need.split('/')) in allowed
        print(('  [PASS] ' if ok else '  [FAIL] ') + f"{key[0]}/{key[1]} need={need} rule={sorted('/'.join(sorted(e)) for e in allowed)}")
        bad += 0 if ok else 1
print('  比对完成，不一致 %d 条' % bad)
sys.exit(1 if bad else 0)
PY
[ $? -ne 0 ] && FAIL=$((FAIL + 1))

echo "==================== 5. 边界：非法 at / 不存在单据 / 未登录 ===================="
ONE_DT=INVOICE
ONE_ID="${DOC_ID[INVOICE]}"
CODE=$(curl -s -o /tmp/n5_bad_at.json -w '%{http_code}' -H "Authorization: Bearer $TOKEN" \
  "$BASE/api/business/attachment/check?doc_type=$ONE_DT&doc_id=$ONE_ID&at=CONFIRMED")
echo "非法 at → HTTP $CODE $(jq -c '{code,msg}' /tmp/n5_bad_at.json)"
[ "$CODE" = "400" ] && pass "非法 at 400（不静默回落成 SUBMIT）" || fail "非法 at 应 400，实际 $CODE"

CODE=$(curl -s -o /tmp/n5_404.json -w '%{http_code}' -H "Authorization: Bearer $TOKEN" \
  "$BASE/api/business/attachment/check?doc_type=$ONE_DT&doc_id=00000000-0000-4000-8000-000000000000&at=SUBMIT")
echo "不存在单据 → HTTP $CODE $(jq -c '{code,msg}' /tmp/n5_404.json)"
[ "$CODE" = "404" ] && pass "单据不存在 404（前端据此降级放行，不锁死按钮）" || fail "应 404，实际 $CODE"

CODE=$(curl -s -o /dev/null -w '%{http_code}' \
  "$BASE/api/business/attachment/check?doc_type=$ONE_DT&doc_id=$ONE_ID&at=SUBMIT")
echo "未登录 → HTTP $CODE"
[ "$CODE" = "401" ] && pass "未登录 401" || fail "未登录应 401，实际 $CODE"

echo
echo "证据目录：$OUT_DIR"
if [ "$FAIL" -eq 0 ]; then echo "RESULT: PASS"; else echo "RESULT: FAIL ($FAIL)"; fi
exit "$FAIL"
