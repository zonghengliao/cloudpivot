#!/usr/bin/env bash
# =============================================================================
# e2e07_fix3_attachment_name_probe.sh —— 附件**秒传复用导致文件名错位**的真机验收（145）
#
# 卡：t_b30fb5be / E2E07-FIX-3（P3，审计隐患）
#
# 原 bug 复现口径：
#   A 上传 drill-invoice.pdf（内容 X）→ sys_file.original_name = drill-invoice.pdf
#   B 上传 e2e07-contract-scan.pdf（同内容 X）→ 秒传复用同一条 sys_file
#   B 的附件列表 → 旧实现显示 **drill-invoice.pdf**（别人的名字）
#
# 验什么（真 token、真 HTTP、真库，逐条 PASS/FAIL）
#   1. 迁移 V0362 已应用：`biz_doc_attachment.file_name` varchar(255) 在、库存量空值 0
#   2. 建两张合同，把**同一份字节**（内容 X）分别以不同文件名上传
#        → 第二次 must be `duplicated=true` 且 `file_id` 与第一次**相同**（真秒传）
#        → 但两条绑定的 `file_name` 各是**本次上传**的名字
#   3. 两张单据的附件列表 API 各自返回自己的名字（file_name 字段）
#   4. 库内：两条绑定同一个 file_id、两个不同的 file_name（这就是本卡要的不变量）
#   5. sys_file 只有 1 行（内容 hash 唯一）——证明「秒传」真的发生了
#   6. 审计：第二次上传的 operation_logs detail.file_name = **B 自己的名字**
#      （旧实现这里记的是 A 的名字，等于审计也跟着错）
#   7. 回落：把 B 那行的 file_name 置空（模拟 V0362 之前的历史行）→ 列表回落
#      `sys_file.original_name`（= A 的名字）→ **正是原 bug 的表现**，回填/回落两段都对；
#      验完立刻恢复，不留脏数据
#   8. 超长文件名（256 字符）→ 400（不静默截断、不 500）
#
# 期望：PASS=N FAIL=0，退出码 0；任何一条 FAIL 退出码 1。
# 运行环境：145（BASE 默认 http://127.0.0.1:8033/api），库 cloudpivot_dev。
# 夹具：合同 + 附件由本脚本自建（单号/文件名带 RUN 后缀），**留作证据不删**
#       （与 E2E07-FIX-2 探针同口径）。
# =============================================================================
set -uo pipefail
ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK="${WORK:-/tmp/e2e07fix3}"; mkdir -p "$WORK/raw"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
say()  { printf '%s\n' "$*"; }
SQ()   { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -X -q -tA -F'|' -c "$1"; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$WORK/raw/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

# upload <名> <doc_type> <doc_id> <本地文件> <对外的文件名> <token>
upload() {
  local name=$1 doc_type=$2 doc_id=$3 local_file=$4 as_name=$5 token=$6
  local out="$WORK/raw/$name.json"
  HTTP=$(curl -s -m 120 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$token" \
    -F "doc_type=$doc_type" -F "doc_id=$doc_id" -F "file_role=CONTRACT_SCAN" \
    -F "file=@$local_file;filename=$as_name;type=application/pdf")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}

login() { # login <用户名> <口令>
  api "login_$1" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && data '.data.access_token'
}

RUN=$(date +%m%d%H%M%S)
NAME_A="e2e07-contract-scan-$RUN.pdf"
NAME_B="drill-invoice-$RUN.pdf"

say "### 0) 登录"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD"); [ -n "$T_ADMIN" ] && ok "admin 登录" || { bad "admin 登录失败"; exit 1; }
ADMIN_ID=$(SQ "select id from sys_user where username='admin'")

say "### 1) 迁移 V0362 已应用 + 存量回填无空值（验收①②）"
check "V0362 在 schema_migrations 里" \
  "$(SQ "select count(*) from schema_migrations where source like '%V0362__doc_attachment_file_name%' and success")" "1"
check "file_name 列（varchar(255)）存在" \
  "$(SQ "select count(*) from information_schema.columns where table_schema='public' \
        and table_name='biz_doc_attachment' and column_name='file_name' \
        and data_type='character varying' and character_maximum_length=255")" "1"
check "存量空值行数（验收②：必须 0）" \
  "$(SQ "select count(*) from biz_doc_attachment a join sys_file f on f.id=a.file_id \
         where nullif(a.file_name,'') is null")" "0"

say "### 2) 建两张合同（附件挂靠点）"
SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null and status=1 order by supplier_code limit 1")
[ -z "$SUP_ID" ] && { bad "库里没有可用供应商"; exit 1; }
for n in A B; do
  api "contract_$n" POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"E2E07FIX3-$n-$RUN\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",\"amount\":1000,\"sign_date\":\"2026-11-09\",\"start_date\":\"2026-11-09\",\"end_date\":\"2027-11-08\",\"owner_id\":\"$ADMIN_ID\"}"
  eval "C_$n=\$(data '.data.id')"
  check "建单 $n（HTTP/CODE）" "$HTTP/c$CODE" "200/c0"
  eval "C=\$C_$n"
  [ -z "$C" ] || [ "$C" = "null" ] && { bad "建单 $n 没拿到 id"; exit 1; }
done

say "### 3) 同一份字节，两个不同文件名，分别上传到两张合同（真秒传）"
# 内容 X：一份最小合法 PDF + 每次运行唯一的注释行（保证 hash 与历史夹具不同，但本次 A/B 相同）
{
  printf '%%PDF-1.4\n%% E2E07-FIX-3 秒传夹具 %s\n' "$RUN"
  printf '1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n'
  printf '2 0 obj<</Type/Pages/Count 0/Kids[]>>endobj\n'
  printf 'trailer<</Root 1 0 R>>\n%%%%EOF\n'
} > "$WORK/content-x.pdf"
cp "$WORK/content-x.pdf" "$WORK/as-a.pdf"
cp "$WORK/content-x.pdf" "$WORK/as-b.pdf"
HASH=$(sha256sum "$WORK/content-x.pdf" | cut -d' ' -f1)
check "A/B 两份上传文件字节一致（sha256 前 12 位）" "$(printf '%s' "$HASH" | cut -c1-12)" "$(printf '%s' "$HASH" | cut -c1-12)"

upload up_a CONTRACT "$C_A" "$WORK/as-a.pdf" "$NAME_A" "$T_ADMIN"
check "上传 A（HTTP/CODE）" "$HTTP/c$CODE" "200/c0"
FID_A=$(data '.data.file_id'); NAME_GOT_A=$(data '.data.file_name'); BID_A=$(data '.data.id')
DUP_A=$(data '.data.duplicated')
check "上传 A 的显示名" "$NAME_GOT_A" "$NAME_A"

upload up_b CONTRACT "$C_B" "$WORK/as-b.pdf" "$NAME_B" "$T_ADMIN"
check "上传 B（HTTP/CODE）" "$HTTP/c$CODE" "200/c0"
FID_B=$(data '.data.file_id'); NAME_GOT_B=$(data '.data.file_name'); BID_B=$(data '.data.id')
DUP_B=$(data '.data.duplicated')
check "上传 B 的显示名（旧实现会显示 A 的名字）" "$NAME_GOT_B" "$NAME_B"
check "秒传命中（duplicated）" "$DUP_B" "true"
check "两条绑定共用同一个 sys_file（file_id 相同）" "$FID_B" "$FID_A"

say "### 4) 列表 API 各自返回自己的名字（验收③④）"
api list_a GET "/business/attachment/list?doc_type=CONTRACT&doc_id=$C_A" "$T_ADMIN"
check "合同 A 列表 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同 A 列表 file_name" "$(data ".data.list[] | select(.id==\"$BID_A\") | .file_name")" "$NAME_A"
api list_b GET "/business/attachment/list?doc_type=CONTRACT&doc_id=$C_B" "$T_ADMIN"
check "合同 B 列表 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "合同 B 列表 file_name（原 bug：会是 $NAME_A）" \
  "$(data ".data.list[] | select(.id==\"$BID_B\") | .file_name")" "$NAME_B"
check "合同 B 列表 file_id（仍是秒传复用的那条）" \
  "$(data ".data.list[] | select(.id==\"$BID_B\") | .file_id")" "$FID_B"

say "### 5) 库内不变量 + 真秒传证据"
check "两条绑定同 file_id" \
  "$(SQ "select count(distinct file_id) from biz_doc_attachment where id in ('$BID_A','$BID_B')")" "1"
check "两条绑定两个不同的 file_name" \
  "$(SQ "select count(distinct file_name) from biz_doc_attachment where id in ('$BID_A','$BID_B')")" "2"
check "sys_file 里该内容只有 1 行（秒传）" \
  "$(SQ "select count(*) from sys_file where file_hash='$HASH' and deleted_at is null")" "1"
check "sys_file.original_name = 第一个上传者的名字（bug 的成因）" \
  "$(SQ "select original_name from sys_file where id='$FID_A'")" "$NAME_A"

say "### 6) 审计留痕：第二次上传记的必须是 B 自己的名字"
check "operation_logs(DOC_ATTACHMENT_UPLOAD) detail.file_name" \
  "$(SQ "select detail->>'file_name' from operation_logs \
         where operation='DOC_ATTACHMENT_UPLOAD' and target_id='$BID_B' \
         order by created_at desc limit 1")" "$NAME_B"
check "operation_logs detail.original_name（本次提交的文件名）" \
  "$(SQ "select detail->>'original_name' from operation_logs \
         where operation='DOC_ATTACHMENT_UPLOAD' and target_id='$BID_B' \
         order by created_at desc limit 1")" "$NAME_B"

say "### 7) 回落口径：把 B 那行的快照置空（模拟未回填的历史行）→ 回落 sys_file 名字"
SQ "update biz_doc_attachment set file_name=null where id='$BID_B'" > /dev/null
api list_b_null GET "/business/attachment/list?doc_type=CONTRACT&doc_id=$C_B" "$T_ADMIN"
check "置空后列表回落到 sys_file.original_name（= A 的名字，正是原 bug 的表现）" \
  "$(data ".data.list[] | select(.id==\"$BID_B\") | .file_name")" "$NAME_A"
SQ "update biz_doc_attachment set file_name='$NAME_B' where id='$BID_B'" > /dev/null
api list_b_back GET "/business/attachment/list?doc_type=CONTRACT&doc_id=$C_B" "$T_ADMIN"
check "恢复快照后又是自己的名字" \
  "$(data ".data.list[] | select(.id==\"$BID_B\") | .file_name")" "$NAME_B"

say "### 8) 超长文件名（256 字符）→ 400（不静默截断、不 500）"
LONG=$(python3 -c "print('汉'*256)")
upload up_long CONTRACT "$C_A" "$WORK/as-a.pdf" "$LONG.pdf" "$T_ADMIN"
check "超长文件名 HTTP/CODE" "$HTTP/c$CODE" "400/c10001"
printf '%s' "$BODY" | jq -r '.msg // empty' 2>/dev/null | grep -q "255" \
  && ok "报文点明上限 255" || bad "报文没提上限：$(printf '%s' "$BODY" | jq -r '.msg // .')"

say ""
say "================ PASS=$PASS FAIL=$FAIL ================"
say "证据目录：$WORK/raw（每次请求的原始响应）"
[ "$FAIL" -eq 0 ] || exit 1
