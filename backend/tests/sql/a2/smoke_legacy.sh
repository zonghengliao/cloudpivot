#!/usr/bin/env bash
# 验收 1：旧接口零回归 —— `/support/file/*` 逐个真打（upload / upload_batch /
# check_duplicate / page / detail / delete / download），断言状态码与内容哈希。
#
# 用法（在 172.18.10.144 上）：bash /root/a2-test-run/smoke_legacy.sh
set -uo pipefail
# 注意：后端 `[router].prefix = "/api"`，宏内路径不含 /api，对外 URL 必须带前缀。
BASE=http://127.0.0.1:8025/api
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a

PASS=0
FAIL=0
check() { # name actual expected
  if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); printf '  PASS  %-42s %s\n' "$1" "$2";
  else FAIL=$((FAIL + 1)); printf '  FAIL  %-42s got=%s want=%s\n' "$1" "$2" "$3"; fi
}

TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then echo "登录失败，终止"; exit 1; fi
AUTH="Authorization: Bearer $TOKEN"
echo "== 旧接口回归（profile=test / cloudpivot_test） =="

WORK=$(mktemp -d)
printf 'legacy-smoke-%s\n' "$(date +%s%N)" > "$WORK/legacy.txt"
SRC_SHA=$(sha256sum "$WORK/legacy.txt" | awk '{print $1}')
SRC_SIZE=$(stat -c%s "$WORK/legacy.txt")
printf 'batch-a\n' > "$WORK/a.txt"
printf 'batch-b\n' > "$WORK/b.txt"

# 1) upload
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/support/file/upload?category=attachment" \
  -H "$AUTH" -F "file=@$WORK/legacy.txt;type=text/plain")
CODE=$(echo "$RESP" | tail -1); BODY=$(echo "$RESP" | sed '$d')
check "POST /support/file/upload" "$CODE" "200"
FID=$(echo "$BODY" | jq -r '.data.id')
URL=$(echo "$BODY" | jq -r '.data.url')
echo "        file_id=$FID url=$URL"
check "  upload 响应字段齐全" \
  "$(echo "$BODY" | jq -r '[.data | has("id"), has("file_name"), has("original_name"), has("file_key"), has("file_size"), has("mime_type"), has("extension"), has("category"), has("status"), has("uploaded_by"), has("created_at"), has("url")] | all')" "true"
check "  upload url 指向旧下载接口" "$(echo "$URL" | sed 's|/api/support/file/download/.*|ok|')" "ok"
check "  upload file_size" "$(echo "$BODY" | jq -r '.data.file_size')" "$SRC_SIZE"

# 2) check-duplicate（A1 之前就已存在的行为：命中返回同一记录）
DUP=$(curl -s "$BASE/support/file/check-duplicate?file_hash=$SRC_SHA" -H "$AUTH")
check "GET /support/file/check-duplicate 命中" "$(echo "$DUP" | jq -r '.data.duplicated')" "true"
check "  duplicated.file.id 相同" "$(echo "$DUP" | jq -r '.data.file.id')" "$FID"
MISS=$(curl -s "$BASE/support/file/check-duplicate?file_hash=deadbeef" -H "$AUTH")
check "GET /support/file/check-duplicate 未命中" "$(echo "$MISS" | jq -r '.data.duplicated')" "false"
check "GET /support/file/check-duplicate 空参 400" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/support/file/check-duplicate?file_hash=" -H "$AUTH")" "400"

# 3) page（响应结构 list/total/page/page_size 不能变）
PAGE=$(curl -s "$BASE/support/file/page?page=1&page_size=5&category=attachment" -H "$AUTH")
check "GET /support/file/page 状态" "$(echo "$PAGE" | jq -r '.code')" "0"
check "  page 字段齐全" \
  "$(echo "$PAGE" | jq -r '[(.data|has("list")),(.data|has("total")),(.data|has("page")),(.data|has("page_size"))] | all')" "true"
check "  本记录出现在第一页" "$(echo "$PAGE" | jq -r --arg id "$FID" '[.data.list[].id] | index($id) != null')" "true"

# 4) detail
DETAIL=$(curl -s "$BASE/support/file/$FID" -H "$AUTH")
check "GET /support/file/{id}" "$(echo "$DETAIL" | jq -r '.code')" "0"
check "  detail.id 一致" "$(echo "$DETAIL" | jq -r '.data.id')" "$FID"

# 5) download（内容必须逐字节一致）
curl -s -o "$WORK/dl.txt" "$BASE/support/file/download/$FID" -H "$AUTH"
DL_SHA=$(sha256sum "$WORK/dl.txt" | awk '{print $1}')
check "GET /support/file/download/{id} sha256" "$DL_SHA" "$SRC_SHA"
check "GET /support/file/download/{id} 大小" "$(stat -c%s "$WORK/dl.txt")" "$SRC_SIZE"

# 6) upload/batch（多文件，顺序返回）
BATCH=$(curl -s -X POST "$BASE/support/file/upload/batch?category=attachment" -H "$AUTH" \
  -F "file=@$WORK/a.txt;type=text/plain" -F "file=@$WORK/b.txt;type=text/plain")
check "POST /support/file/upload/batch 数量" "$(echo "$BATCH" | jq -r '.data | length')" "2"
check "  batch 顺序 = 提交顺序" \
  "$(echo "$BATCH" | jq -r '[.data[].original_name] | join(",")')" "a.txt,b.txt"

# 7) 类型白名单仍然生效（旧行为：.exe 400）
printf 'MZ' > "$WORK/bad.exe"
check "POST /support/file/upload .exe 被拒" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/support/file/upload" -H "$AUTH" -F "file=@$WORK/bad.exe")" "400"

# 8) delete → 再下载 404
check "DELETE /support/file/{id}" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/support/file/$FID" -H "$AUTH")" "200"
check "  删除后 GET /support/file/{id} 404" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/support/file/$FID" -H "$AUTH")" "404"
check "  删除后下载 404" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/support/file/download/$FID" -H "$AUTH")" "404"

echo "== 旧接口回归结果：PASS=$PASS FAIL=$FAIL =="
rm -rf "$WORK"
[ "$FAIL" -eq 0 ]
