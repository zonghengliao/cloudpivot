#!/usr/bin/env bash
# E2E-05 资产域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e05_asset_{a,b,c}.sh source。状态落 /tmp/e2e05/state.sh，报文落 /tmp/e2e05/raw/，
# 断言逐条追加 /tmp/e2e05/asserts.log（可跨脚本累计）。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e05}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
mkdir -p "$RAW"; touch "$LOG"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
SQ()   { PSQLQ -c "$1" 2>&1 | tr '\n' ' ' | sed 's/  */ /g; s/ *$//'; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf 'PASS\t%s\n' "$*" >> "$LOG"; say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL\t%s\n' "$*" >> "$LOG"; say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne() { if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
chknum() { # 金额比较（容差 0.005）
  if awk -v a="${2:-x}" -v b="${3:-x}" 'BEGIN{ if (a=="x"||b=="x") exit 1; d=a-b; if(d<0)d=-d; exit !(d<0.005) }'; then
    ok "$1 = $2（期望 $3）"
  else bad "$1 = $2（期望 $3）"; fi
}
chkle() { # 小于等于
  if awk -v a="${2:-x}" -v b="${3:-x}" 'BEGIN{ if (a=="x"||b=="x") exit 1; exit !(a<=b+0.00001) }'; then
    ok "$1 = $2 ≤ $3"
  else bad "$1 = $2（应 ≤ $3）"; fi
}
summary() { say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 300; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  if [ "$HTTP" = 401 ] && [ -n "$token" ]; then
    local fresh
    fresh=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
    if [ -n "$fresh" ]; then
      T_ADMIN=$fresh; save T_ADMIN "$fresh"
      local -a retry=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path" --oauth2-bearer "$fresh")
      [ -n "$body" ] && retry+=(-H 'Content-Type: application/json' -d "$body")
      HTTP=$(curl "${retry[@]}")
    fi
  fi
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}

login() { # login <user> <pass> <tag> → 打印 token（成功 CODE=0）
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'
  else say "FATAL: 登录失败 $1 http=$HTTP $(show)" >&2; return 1; fi
}

login_admin() {
  T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1
  save T_ADMIN "$T_ADMIN"
  say "  · admin 登录成功（token 长度 ${#T_ADMIN}）"
}

login_gm() {
  T_GM=$(login gm_drill 'Drill@123456' gm) || exit 1
  save T_GM "$T_GM"
  say "  · gm_drill（GENERAL_MANAGER，处置审批人）登录成功（token 长度 ${#T_GM}）"
}

# 取科目 id：acc <code> → uuid
acc() { SQ "select id from biz_account where account_code='$1' and deleted_at is null"; }

# 上传附件（multipart）：upfile <tag> <doc_type> <doc_id> <file_role> <local_path>
upfile() {
  local name=$1 dt=$2 did=$3 role=$4 path=$5 out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$T_ADMIN" -F "doc_type=$dt" -F "doc_id=$did" \
    -F "file_role=$role" -F "remark=E2E-05 演练附件" -F "file=@$path")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}

# 固定资产「启用（ACTIVATE）」前置：分类 FIXED_ASSET 要求至少 1 个发票/验收单
ensure_asset_att() { # ensure_asset_att <asset_id> <tag>
  local n
  n=$(SQ "select count(*) from biz_doc_attachment where doc_type='FIXED_ASSET' and doc_id='$1' and deleted_at is null")
  if [ "$n" -gt 0 ]; then ok "资产附件已存在（$2）"; return 0; fi
  [ -f "$WORK/drill-invoice.pdf" ] || printf '%%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$WORK/drill-invoice.pdf"
  upfile "att_$2" FIXED_ASSET "$1" INVOICE_SCAN "$WORK/drill-invoice.pdf"
  check "上传附件 $2 http" "$HTTP" "200"
  check "上传附件 $2 code" "$CODE" "0"
  check "附件落库（FIXED_ASSET / INVOICE_SCAN）" \
    "$(SQ "select count(*) from biz_doc_attachment where doc_type='FIXED_ASSET' and doc_id='$1' and file_role='INVOICE_SCAN' and deleted_at is null")" "1"
}

# 期初前置：演练期间 2027-12 ~ 2028-06（幂等）
ensure_periods() {
  local from=2027-12 to=2028-06 have
  have=$(SQ "select count(*) from biz_accounting_period where period_code between '$from' and '$to'")
  say "  · 演练期间 $from..$to 现有 $have 期"
  if [ "$have" = 7 ]; then ok "演练期间齐备（7 期）"; return 0; fi
  api per_batch POST /business/erp/period/batch-open "$T_ADMIN" "{\"from\":\"$from\",\"to\":\"$to\",\"remark\":\"E2E-05 资产域演练期间\"}"
  check "批量开账 http" "$HTTP" "200"
  check "批量开账 code" "$CODE" "0"
  have=$(SQ "select count(*) from biz_accounting_period where period_code between '$from' and '$to'")
  check "演练期间数" "$have" "7"
}
