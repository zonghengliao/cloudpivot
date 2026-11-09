#!/usr/bin/env bash
# E2E-02 销售域演练公共库（145 cloudpivot_dev，真后端 :8033）
# 被 e2e02_sales_a.sh / _b.sh / _c.sh source。状态落 /tmp/e2e02/state.sh，报文落 /tmp/e2e02/raw/
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e02}; RAW=$WORK/raw; STATE=$WORK/state.sh
mkdir -p "$RAW"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
num()  { printf '%s' "${1:-0}"; }
summary(){ say; say "== PASS=$PASS FAIL=$FAIL"; [ "$FAIL" = 0 ] || say "== 有失败项，退出码 1"; [ "$FAIL" = 0 ]; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 40 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}
data() { printf '%s' "$BODY" | jq -r "$1"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1=$2"; }
login() { # login <user> <pass> <tag>
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'; else
    say "FATAL: 登录失败 $1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)" >&2; return 1; fi
}
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null; }
keys() { printf '%s' "$BODY" | jq -r '(.data|type) as $t | if $t=="object" then (.data|keys|join(",")) else $t end' 2>/dev/null; }

# 最小 PDF / PNG 夹具
mkpdf() {
  local f=$WORK/e2e02-doc.pdf
  printf '%%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 20 100 Td (E2E-02 doc) Tj ET\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$f"
  printf '%s' "$f"
}
mkpng() {
  local f=$WORK/e2e02-doc.png
  printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8AAAwAB/AF+4wAAAABJRU5ErkJggg==' | base64 -d > "$f"
  printf '%s' "$f"
}
attach_upload() { # attach_upload <name> <doc_type> <doc_id> <file> <ct>
  local name=$1 dtype=$2 did=$3 file=$4 ct=${5:-application/pdf} out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$T_ADMIN" \
    -F "doc_type=$dtype" -F "doc_id=$did" -F "file_role=INVOICE_SCAN" -F "file=@$file;type=$ct")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}

approve_instance() { # approve_instance <token> <instance_id> <tag>
  local t=$1 iid=$2 tag=$3 n=0 tid node
  while [ $n -lt 8 ]; do
    api "${tag}_todo$n" GET "/business/flow/task/todo?page=1&page_size=200" "$t"
    [ "$CODE" = 0 ] || { say "  ⚠ $tag 待办查询 code=$CODE"; return 1; }
    tid=$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)
    [ -z "$tid" ] && return 0
    node=$(data ".data.list[]? | select(.id==\"$tid\") | .current_node_name")
    api "${tag}_ap$n" POST "/business/flow/task/$tid/approve" "$t" '{"comment":"E2E-02 演练审批通过"}'
    if [ "$CODE" = 0 ]; then ok "审批通过节点 [$node]"; else bad "审批节点 [$node] 失败 http=$HTTP $(show)"; return 1; fi
    n=$((n+1))
  done
}
flow_state() { PSQLQ -c "select status from biz_flow_instance where id='$1';"; }
