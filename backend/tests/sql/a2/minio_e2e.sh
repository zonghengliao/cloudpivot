#!/usr/bin/env bash
# 验收 4：S3 真读写 —— 本地 MinIO 全链路：
#   建后端配置 → 测试连接 → 预签名直传(PUT) → 完成登记 → 302 下载(sha256 一致) →
#   服务端上传 12 MB（走分片）→ 下载 sha256 一致 → 软删 → 下载 404。
#
# 环境：MinIO 跑在 172.18.10.99:9000（bucket=cloudpivot-test），后端在 144。
# 前置：测试后端进程必须以 CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST=172.18.10.99 启动
#       （私网地址的唯一放行通道，见 store::ensure_endpoint_allowed 文档）。
# 用法：bash /root/a2-test-run/minio_e2e.sh <minio_ak> <minio_sk>
set -uo pipefail
BASE=http://127.0.0.1:8025/api
MINIO_ENDPOINT=${MINIO_ENDPOINT:-http://172.18.10.99:9000}
MINIO_BUCKET=${MINIO_BUCKET:-cloudpivot-test}
AK=${1:?需要 MinIO AccessKey}
SK=${2:?需要 MinIO SecretKey}
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a

PASS=0; FAIL=0
check() { if [ "$2" = "$3" ]; then PASS=$((PASS + 1)); printf '  PASS  %-48s %s\n' "$1" "$2";
  else FAIL=$((FAIL + 1)); printf '  FAIL  %-48s got=%s want=%s\n' "$1" "$2" "$3"; fi; }

TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ALUN_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
AUTH="Authorization: Bearer $TOKEN"
echo "== S3（MinIO $MINIO_ENDPOINT / bucket=$MINIO_BUCKET）全链路 =="

# ---------- 0. 清理旧配置（幂等重跑） ----------
OLD=$(curl -s "$BASE/business/storage/config/page?page=1&page_size=50&backend_type=S3" -H "$AUTH" \
  | jq -r '.data.list[]? | select(.config_code=="minio-test") | .id')
if [ -n "$OLD" ]; then
  curl -s -o /dev/null -X DELETE "$BASE/business/storage/config/$OLD" -H "$AUTH"
  echo "  cleanup: 删除旧 minio-test 配置 $OLD"
fi

# ---------- 0. SSRF 防护仍然生效（白名单只放行被显式列出的主机） ----------
GUARD=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/config" -H "$AUTH" \
  -H 'Content-Type: application/json' -d "{
    \"config_code\":\"ssrf-guard\",\"config_name\":\"SSRF 探针\",\"backend_type\":\"S3\",
    \"endpoint\":\"http://169.254.169.254/latest/meta-data\",\"bucket\":\"x\",
    \"region\":\"us-east-1\",\"ak\":\"$AK\",\"sk\":\"$SK\",\"status\":1}")
check "元数据端点 endpoint 被 SSRF 防护拒绝" "$(echo "$GUARD" | tail -1)" "400"
check "  错误码 = 60009(ENDPOINT_FORBIDDEN)" "$(echo "$GUARD" | sed '$d' | jq -r '.code')" "60009"

# ---------- 1. 建 S3 后端配置 ----------
CREATE=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/config" -H "$AUTH" \
  -H 'Content-Type: application/json' -d "{
    \"config_code\":\"minio-test\",\"config_name\":\"MinIO 测试桶\",
    \"backend_type\":\"S3\",\"endpoint\":\"$MINIO_ENDPOINT\",\"bucket\":\"$MINIO_BUCKET\",
    \"region\":\"us-east-1\",\"base_path\":\"a2\",\"use_tls\":false,\"path_style\":true,
    \"ak\":\"$AK\",\"sk\":\"$SK\",\"status\":1}")
check "POST /business/storage/config 建 S3 后端" "$(echo "$CREATE" | tail -1)" "200"
CFG_JSON=$(echo "$CREATE" | sed '$d')
CFG_ID=$(echo "$CFG_JSON" | jq -r '.data.id')
check "  AK 只回掩码（不含明文）" "$(echo "$CFG_JSON" | jq -r '.data.ak_masked' | grep -c '\*\*\*\*')" "1"
check "  响应体不含明文 SecretKey" "$(echo "$CFG_JSON" | grep -c "$SK")" "0"

# ---------- 2. 测试连接（真发一次已签名 HEAD bucket） ----------
TEST=$(curl -s -X POST "$BASE/business/storage/config/$CFG_ID/test" -H "$AUTH")
check "POST /business/storage/config/{id}/test healthy" "$(echo "$TEST" | jq -r '.data.healthy')" "true"
echo "        latency_ms=$(echo "$TEST" | jq -r '.data.latency_ms') message=$(echo "$TEST" | jq -r '.data.message')"

# ---------- 3. 设为默认后端（策略未指定 storage_code → 走默认） ----------
check "POST /business/storage/config/{id}/default" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/business/storage/config/$CFG_ID/default" -H "$AUTH")" "200"

WORK=$(mktemp -d)
STAMP=$(date +%s%N)
printf 'minio-direct-upload-%s\n' "$STAMP" > "$WORK/direct.txt"
SRC_SHA=$(sha256sum "$WORK/direct.txt" | awk '{print $1}')
SRC_SIZE=$(stat -c%s "$WORK/direct.txt")

# ---------- 4. 预签名直传 ----------
PRESIGN=$(curl -s -X POST "$BASE/business/storage/presign" -H "$AUTH" \
  -H 'Content-Type: application/json' -d "{
    \"category\":\"attachment\",\"file_name\":\"direct-$STAMP.txt\",
    \"file_size\":$SRC_SIZE,\"file_hash\":\"$SRC_SHA\",\"mime_type\":\"text/plain\"}")
UPLOAD_URL=$(echo "$PRESIGN" | jq -r '.data.upload_url')
FILE_KEY=$(echo "$PRESIGN" | jq -r '.data.file_key')
check "POST /business/storage/presign 返回 storage_code" "$(echo "$PRESIGN" | jq -r '.data.storage_code')" "minio-test"
check "  未命中秒传" "$(echo "$PRESIGN" | jq -r '.data.duplicated')" "false"
echo "        upload_url=${UPLOAD_URL:0:80}...（已签名，$(( ${#UPLOAD_URL} )) 字节）"

# 浏览器直传：curl 直接 PUT 到 MinIO（**不带**任何 Authorization）
PUT_CODE=$(curl -s -o "$WORK/put.out" -w '%{http_code}' -X PUT "$UPLOAD_URL" \
  -H 'Content-Type: text/plain' --data-binary "@$WORK/direct.txt")
check "PUT 预签名 URL 直传 MinIO" "$PUT_CODE" "200"

# 前端拿到 200 后调 complete 登记
COMPLETE=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/complete" -H "$AUTH" \
  -H 'Content-Type: application/json' -d "{
    \"category\":\"attachment\",\"file_key\":\"$FILE_KEY\",\"file_name\":\"direct-$STAMP.txt\",
    \"file_size\":$SRC_SIZE,\"file_hash\":\"$SRC_SHA\",\"mime_type\":\"text/plain\"}")
check "POST /business/storage/complete 登记" "$(echo "$COMPLETE" | tail -1)" "200"
DIRECT_ID=$(echo "$COMPLETE" | sed '$d' | jq -r '.data.id')
check "  upload_mode=DIRECT" "$(echo "$COMPLETE" | sed '$d' | jq -r '.data.upload_mode')" "DIRECT"
check "  storage_code=minio-test" "$(echo "$COMPLETE" | sed '$d' | jq -r '.data.storage_code')" "minio-test"

# ---------- 5. 下载（S3 → 302 预签名 URL） ----------
REDIR=$(curl -s -D - -o /dev/null "$BASE/business/storage/download/$DIRECT_ID" -H "$AUTH")
check "GET download S3 后端 302" "$(echo "$REDIR" | head -1 | grep -c ' 302')" "1"
LOCATION=$(echo "$REDIR" | grep -i '^location:' | tr -d '\r' | sed 's/^[Ll]ocation: //')
echo "        Location=${LOCATION:0:70}..."
curl -sL -o "$WORK/dl.txt" "$BASE/business/storage/download/$DIRECT_ID" -H "$AUTH"
check "  302 后内容 sha256 一致" "$(sha256sum "$WORK/dl.txt" | awk '{print $1}')" "$SRC_SHA"

# ---------- 6. 服务端上传 12 MB（> 8 MB ⇒ 分片上传） ----------
dd if=/dev/urandom of="$WORK/big.mp4" bs=1M count=12 status=none
BIG_SHA=$(sha256sum "$WORK/big.mp4" | awk '{print $1}')
UP=$(curl -s -w '\n%{http_code}' -X POST "$BASE/business/storage/upload?category=attachment" \
  -H "$AUTH" -F "file=@$WORK/big.mp4;type=application/octet-stream")
check "POST /business/storage/upload 12 MB 到 S3（分片）" "$(echo "$UP" | tail -1)" "200"
BIG_ID=$(echo "$UP" | sed '$d' | jq -r '.data.id')
check "  big storage_code=minio-test" "$(echo "$UP" | sed '$d' | jq -r '.data.storage_code')" "minio-test"
curl -sL -o "$WORK/big.dl" "$BASE/business/storage/download/$BIG_ID" -H "$AUTH"
check "  12 MB 下载 sha256 一致（分片合并正确）" "$(sha256sum "$WORK/big.dl" | awk '{print $1}')" "$BIG_SHA"

# ---------- 7. 软删 → 下载 404 ----------
check "DELETE /business/storage/{id}（S3 记录）" \
  "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/business/storage/$DIRECT_ID" -H "$AUTH")" "200"
check "  软删后下载 404" \
  "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/storage/download/$DIRECT_ID" -H "$AUTH")" "404"

# ---------- 8. 收尾：默认后端切回本地（避免影响其它验证） ----------
LOCAL_ID=$(curl -s "$BASE/business/storage/config/page?page=1&page_size=50" -H "$AUTH" \
  | jq -r '.data.list[]? | select(.config_code=="local-default") | .id')
if [ -n "$LOCAL_ID" ]; then
  curl -s -o /dev/null -X POST "$BASE/business/storage/config/$LOCAL_ID/default" -H "$AUTH"
  echo "  cleanup: 默认后端切回 local-default"
fi
echo "        minio-test 配置保留（id=$CFG_ID）供复用"

echo "== S3 全链路结果：PASS=$PASS FAIL=$FAIL =="
rm -rf "$WORK"
[ "$FAIL" -eq 0 ]
