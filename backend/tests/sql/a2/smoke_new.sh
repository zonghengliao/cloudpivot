#!/usr/bin/env bash
# 验收 2：新接口闭环 —— 上传 → 列表 → 下载（sha256 一致）→ 软删 → 下载 404。
# 额外覆盖：presign 在本地后端必须 422、complete 对不存在对象必须 404、rebind 重新登记。
#
# 用法（在 172.18.10.144 上）：bash /root/a2-test-run/smoke_new.sh
set -uo pipefail
BASE=http://127.0.0.1:8025/api
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a

PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); printf '  PASS  %-46s %s\n' "$1" "$2";
  else FAIL=$((FAIL + 1)); printf '  FAIL  %-46s got=%s want=%s\n' "$1" "$2" "$3"; fi; }

TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
[ -z "$TOKEN" ] || [ "$TOKEN" = "null" ] && { echo "登录失败"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"
echo "== 新接口闭环 /api/business/storage/* =="

WORK=$(mktemp -d)
printf 'a2-new-api-smoke-%s\n' "$(date +%s%N)" > "$WORK/new.txt"
SRC_SHA=$(sha256sum "$WORK/new.txt" | awk '{print $1}')
SRC_SIZE=$(stat -c%s "$WORK/new.txt")

# 1) 上传
RESP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/upload?category=attachment" \
  -H "$AUTH" -F "file=@$WORK/new.txt;type=text/plain")
CODE=$(echo "$RESP" | tail -1); BODY=$(echo "$RESP" | sed '$d')
check "POST /business/storage/upload" "$CODE" "200"
FID=$(echo "$BODY" | jq -r '.data.id')
echo "        file_id=$FID"
check "  响应含 storage_code" "$(echo "$BODY" | jq -r '.data.storage_code')" "local-default"
check "  响应含 upload_mode=SERVER" "$(echo "$BODY" | jq -r '.data.upload_mode')" "SERVER"
check "  响应含新下载 url" "$(echo "$BODY" | jq -r '.data.url')" "/api/business/storage/download/$FID"
check "  file_size 一致" "$(echo "$BODY" | jq -r '.data.file_size')" "$SRC_SIZE"

# 2) 列表
PAGE=$(curl -s "$BASE/business/storage/file/page?page=1&page_size=10&storage_code=local-default" -H "$AUTH")
check "GET /business/storage/file/page" "$(echo "$PAGE" | jq -r '.code')" "0"
check "  列表命中本次文件" "$(echo "$PAGE" | jq -r --arg id "$FID" '[.data.list[].id] | index($id) != null')" "true"
HASH_Q=$(curl -s "$BASE/business/storage/file/page?file_hash=$SRC_SHA" -H "$AUTH")
check "  按 content hash 过滤命中" "$(echo "$HASH_Q" | jq -r --arg id "$FID" '[.data.list[].id] | index($id) != null')" "true"

# 3) 下载（内容一致）
curl -s -o "$WORK/dl.txt" "$BASE/business/storage/download/$FID" -H "$AUTH"
check "GET /business/storage/download/{id} sha256" "$(sha256sum "$WORK/dl.txt" | awk '{print $1}')" "$SRC_SHA"
HDR=$(curl -s -D - -o /dev/null "$BASE/business/storage/download/$FID" -H "$AUTH")
check "  响应含 Content-Disposition attachment" "$(echo "$HDR" | grep -ci 'content-disposition: attachment')" "1"
check "  响应含 X-Content-Type-Options nosniff" "$(echo "$HDR" | grep -ci 'x-content-type-options: nosniff')" "1"
check "  响应 Content-Length" "$(echo "$HDR" | grep -ci "content-length: $SRC_SIZE")" "1"

# 4) presign 在本地后端必须 422（不支持直传）
PRESIGN=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/presign" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d "{\"category\":\"attachment\",\"file_name\":\"x.txt\",\"file_size\":10}")
check "POST /business/storage/presign 本地后端 422" "$(echo "$PRESIGN" | tail -1)" "422"
check "  错误码 = 60006(直传未开)" "$(echo "$PRESIGN" | sed '$d' | jq -r '.code')" "60006"

# 5) complete 对不存在的对象必须 404（不信任前端声明）
COMPLETE=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/complete" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d "{\"category\":\"attachment\",\"file_key\":\"2026/01/01/nope.txt\",\"file_name\":\"nope.txt\",\"file_size\":3}")
check "POST /business/storage/complete 对象不存在 404" "$(echo "$COMPLETE" | tail -1)" "404"
check "  错误码 = 60007(对象缺失)" "$(echo "$COMPLETE" | sed '$d' | jq -r '.code')" "60007"

# 6) 软删 → 下载 404，物理对象保留（延迟回收）
check "DELETE /business/storage/{id}" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/business/storage/$FID" -H "$AUTH")" "200"
check "  软删后下载 404" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/storage/download/$FID" -H "$AUTH")" "404"
check "  重复删除幂等 200" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/business/storage/$FID" -H "$AUTH")" "200"
check "  记录仍在库（deleted_at 非空）" \
  "$(set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a; PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_test -tAc "select count(*) from sys_file where id='$FID' and deleted_at is not null")" "1"

# 7) rebind：把软删记录重新登记为新记录，下载恢复
REBIND=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/$FID/rebind" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{}')
check "POST /business/storage/{id}/rebind" "$(echo "$REBIND" | tail -1)" "200"
NEWID=$(echo "$REBIND" | sed '$d' | jq -r '.data.id')
check "  新记录 ID 不同" "$([ "$NEWID" != "$FID" ] && echo yes || echo no)" "yes"
check "  新记录 file_key 相同（不复制对象）" \
  "$(echo "$REBIND" | sed '$d' | jq -r '.data.file_key')" "$(curl -s "$BASE/business/storage/file/page?file_hash=$SRC_SHA" -H "$AUTH" | jq -r '.data.list[0].file_key')"
curl -s -o "$WORK/dl2.txt" "$BASE/business/storage/download/$NEWID" -H "$AUTH"
check "  rebind 后下载 sha256 一致" "$(sha256sum "$WORK/dl2.txt" | awk '{print $1}')" "$SRC_SHA"

# 8) 参数分支：非法 ID 400、缺分类 422、超大小 422（avatar 上限 10 MB）
check "GET /business/storage/download/not-a-uuid 400" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/storage/download/not-a-uuid" -H "$AUTH")" "400"
dd if=/dev/zero of="$WORK/big.bin" bs=1M count=11 status=none
check "POST /business/storage/upload 超 avatar 上限 422" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/business/storage/upload?category=avatar" -H "$AUTH" -F "file=@$WORK/big.bin;type=image/png")" "422"
check "POST /business/storage/upload 扩展名不在策略 400" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/business/storage/upload?category=avatar" -H "$AUTH" -F "file=@$WORK/new.txt;type=image/png")" "400"

echo "== 新接口闭环结果：PASS=$PASS FAIL=$FAIL =="
rm -rf "$WORK"
[ "$FAIL" -eq 0 ]
