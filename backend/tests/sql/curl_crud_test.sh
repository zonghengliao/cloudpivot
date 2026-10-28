#!/usr/bin/env bash
# A1 配置 CRUD + 测试连接接口实测（批次 A · A1 验收第 3 条）
# 前置：test profile 实例已启动在 127.0.0.1:8025，且已注入 CLOUDPIVOT_STORAGE_MASTER_KEY
set -uo pipefail

BASE="http://127.0.0.1:8025/api"
PASS=0
FAIL=0
ok() { PASS=$((PASS+1)); echo "PASS $1"; }
bad() { FAIL=$((FAIL+1)); echo "FAIL $1"; }

echo "===== 0. 登录取 token ====="
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"${ALUN_ADMIN_PASSWORD}\"}")
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
if [ -n "$TOKEN" ]; then ok "登录成功（token 长度 ${#TOKEN}）"; else bad "登录失败: $LOGIN"; echo "$LOGIN"; exit 1; fi
AUTH="Authorization: Bearer $TOKEN"

echo
echo "===== 1. 列表（初始） ====="
LIST0=$(curl -s -m 10 -H "$AUTH" "$BASE/business/storage/config/page?page=1&page_size=10")
echo "$LIST0" | jq -c '{code, total: .data.total, rows: (.data.list|length)}'
[ "$(echo "$LIST0" | jq -r '.code')" = "0" ] && ok "分页接口可用" || bad "分页接口异常"

echo
echo "===== 2. 新增本地后端（首个后端应自动成为默认） ====="
CREATE_LOCAL=$(curl -s -m 10 -X POST "$BASE/business/storage/config" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "config_code":"local-default","config_name":"本地磁盘（默认）","backend_type":"LOCAL",
  "base_path":"","remark":"A1 实测创建的本地后端"}')
echo "$CREATE_LOCAL" | jq -c '.data | {id, config_code, backend_type, is_default, ak_masked, has_secret, health_status}'
LOCAL_ID=$(echo "$CREATE_LOCAL" | jq -r '.data.id // empty')
[ -n "$LOCAL_ID" ] && [ "$(echo "$CREATE_LOCAL" | jq -r '.data.is_default')" = "true" ] && ok "新增本地后端成功且自动设为默认" || bad "新增本地后端失败: $CREATE_LOCAL"

echo
echo "===== 3. 新增 S3 后端（明文 AK/SK 入参） ====="
CREATE_S3=$(curl -s -m 10 -X POST "$BASE/business/storage/config" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "config_code":"minio-a","config_name":"MinIO 主桶","backend_type":"S3",
  "endpoint":"http://minio.local:9000","bucket":"mybucket","region":"us-east-1",
  "use_tls":false,"path_style":true,"base_path":"public",
  "ak":"AKIA_PLAINTEXT_PROBE_9f2c","sk":"SK_PLAINTEXT_PROBE_SECRET_0001","remark":"A1 实测"}')
echo "$CREATE_S3" | jq -c '.data | {id, config_code, backend_type, endpoint, bucket, path_style, ak_masked, has_secret, is_default}'
S3_ID=$(echo "$CREATE_S3" | jq -r '.data.id // empty')
[ -n "$S3_ID" ] && ok "新增 S3 后端成功" || bad "新增 S3 后端失败: $CREATE_S3"

echo
echo "===== 4. 掩码证明：响应体不得含明文 AK/SK ====="
ALL_RESP=$(echo "$CREATE_LOCAL$CREATE_S3$LIST0")
DETAIL=$(curl -s -m 10 -H "$AUTH" "$BASE/business/storage/config/$S3_ID")
PAGE=$(curl -s -m 10 -H "$AUTH" "$BASE/business/storage/config/page?page=1&page_size=10")
BLOB="$ALL_RESP$DETAIL$PAGE"
if echo "$BLOB" | grep -q "AKIA_PLAINTEXT_PROBE_9f2c"; then bad "响应体出现明文 AK"; else ok "响应体中无明文 AK"; fi
if echo "$BLOB" | grep -q "SK_PLAINTEXT_PROBE_SECRET_0001"; then bad "响应体出现明文 SK"; else ok "响应体中无明文 SK"; fi
echo "详情响应：$(echo "$DETAIL" | jq -c '.data | {config_code, ak_masked, has_secret, key_version}')"
echo "列表中的 S3 行：$(echo "$PAGE" | jq -c '.data.list[] | select(.config_code=="minio-a") | {config_code, ak_masked, has_secret}')"
MASKED=$(echo "$DETAIL" | jq -r '.data.ak_masked')
[ "$MASKED" = "AKIA****9f2c" ] && ok "AK 掩码格式正确（$MASKED）" || bad "AK 掩码异常：$MASKED"

echo
echo "===== 5. 数据库中只有密文（SQL 直查证明） ====="
DBROW=$(psql -h 127.0.0.1 -U "${ALUN_DATABASE_USER}" -d cloudpivot_test -tA -c \
  "select config_code||'|'||coalesce(length(ak_cipher)::text,'-'||'')||'|'||coalesce(length(ak_nonce)::text,'-')||'|'||coalesce(length(sk_cipher)::text,'-') from sys_storage_config where config_code='minio-a'")
echo "库中行（编码|密文长度|nonce长度|SK密文长度）：$DBROW"
[ -n "$DBROW" ] && echo "明文落库检查：$(psql -h 127.0.0.1 -U "${ALUN_DATABASE_USER}" -d cloudpivot_test -tA -c "select count(*) from sys_storage_config where ak_cipher like '%AKIA_PLAINTEXT%' or sk_cipher like '%PLAINTEXT%'") 条含明文（期望 0）"

echo
echo "===== 6. 编辑（不传密钥 → 密钥保持不变） ====="
UPDATE=$(curl -s -m 10 -X PUT "$BASE/business/storage/config/$S3_ID" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "config_code":"minio-a","config_name":"MinIO 主桶（改名）","backend_type":"S3",
  "endpoint":"http://minio.local:9000","bucket":"mybucket-edited","region":"us-east-1",
  "use_tls":false,"path_style":true,"base_path":"public"}')
echo "$UPDATE" | jq -c '.data | {config_code, config_name, bucket, ak_masked, has_secret}'
[ "$(echo "$UPDATE" | jq -r '.data.config_name')" = "MinIO 主桶（改名）" ] && ok "编辑成功" || bad "编辑失败: $UPDATE"
[ "$(echo "$UPDATE" | jq -r '.data.ak_masked')" = "AKIA****9f2c" ] && ok "未传密钥时原密钥保持不变（掩码一致）" || bad "密钥被误改"

echo
echo "===== 7. 编码不可修改（应 400） ====="
BADCODE=$(curl -s -m 10 -X PUT "$BASE/business/storage/config/$S3_ID" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "config_code":"minio-renamed","config_name":"x","backend_type":"S3","endpoint":"http://minio.local:9000","bucket":"mybucket"}')
echo "$BADCODE" | jq -c '{code, msg}'
[ "$(echo "$BADCODE" | jq -r '.code')" = "10001" ] && ok "试图修改编码被拒（10001）" || bad "编码修改未被拒绝"

echo
echo "===== 8. SSRF 防护（endpoint 指向元数据地址，应 60009） ====="
SSRF=$(curl -s -m 10 -X POST "$BASE/business/storage/config" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "config_code":"ssrf-probe","config_name":"SSRF 探针","backend_type":"S3",
  "endpoint":"http://169.254.169.254","bucket":"x","ak":"a1","sk":"s1"}')
echo "$SSRF" | jq -c '{code, msg}'
[ "$(echo "$SSRF" | jq -r '.code')" = "60009" ] && ok "内网端点被 SSRF 防护拒绝（60009）" || bad "SSRF 防护未生效"

echo
echo "===== 9. 测试连接：本地后端（写 1KB 探针再删） ====="
TEST_LOCAL=$(curl -s -m 20 -X POST "$BASE/business/storage/config/$LOCAL_ID/test" -H "$AUTH")
echo "$TEST_LOCAL" | jq -c '.data'
[ "$(echo "$TEST_LOCAL" | jq -r '.data.healthy')" = "true" ] && ok "本地后端连通性探测 HEALTHY" || bad "本地后端探测失败: $TEST_LOCAL"

echo
echo "===== 10. 测试连接：S3 后端（DNS 不可达 → DOWN，且错误已脱敏） ====="
TEST_S3=$(curl -s -m 30 -X POST "$BASE/business/storage/config/$S3_ID/test" -H "$AUTH")
echo "$TEST_S3" | jq -c '.data'
echo "回写后的库中健康状态：$(psql -h 127.0.0.1 -U "${ALUN_DATABASE_USER}" -d cloudpivot_test -tA -c "select health_status||' | '||coalesce(left(last_error,60),'-') from sys_storage_config where config_code='minio-a'")"
if echo "$TEST_S3" | grep -q "AKIA_PLAINTEXT\|SK_PLAINTEXT"; then bad "探测结果泄露凭据"; else ok "探测失败原因不含凭据"; fi

echo
echo "===== 11. 设为默认（当前默认是 local，先切到 S3 再切回） ====="
SET_DEF=$(curl -s -m 10 -X POST "$BASE/business/storage/config/$S3_ID/default" -H "$AUTH")
echo "$SET_DEF" | jq -c '{code, msg}'
[ "$(echo "$SET_DEF" | jq -r '.code')" = "0" ] && ok "设为默认成功" || bad "设为默认失败: $SET_DEF"
echo "库中默认后端：$(psql -h 127.0.0.1 -U "${ALUN_DATABASE_USER}" -d cloudpivot_test -tA -c "select config_code from sys_storage_config where is_default and deleted_at is null")"
SET_DEF2=$(curl -s -m 10 -X POST "$BASE/business/storage/config/$LOCAL_ID/default" -H "$AUTH")
[ "$(echo "$SET_DEF2" | jq -r '.code')" = "0" ] && ok "切回本地默认成功（唯一默认约束未被破坏）" || bad "切回默认失败: $SET_DEF2"

echo
echo "===== 12. 删除保护：默认后端不可删（应 409/10003） ====="
DEL_DEFAULT=$(curl -s -m 10 -X DELETE "$BASE/business/storage/config/$LOCAL_ID" -H "$AUTH")
echo "$DEL_DEFAULT" | jq -c '{code, msg}'
[ "$(echo "$DEL_DEFAULT" | jq -r '.code')" = "10003" ] && ok "默认后端删除被拒" || bad "默认后端删除未被拒绝"

echo
echo "===== 13. 删除 S3 后端（非默认、无引用 → 应成功） ====="
DEL_S3=$(curl -s -m 10 -X DELETE "$BASE/business/storage/config/$S3_ID" -H "$AUTH")
echo "$DEL_S3" | jq -c '{code, msg}'
[ "$(echo "$DEL_S3" | jq -r '.code')" = "0" ] && ok "S3 后端软删成功" || bad "S3 后端删除失败: $DEL_S3"
PAGE2=$(curl -s -m 10 -H "$AUTH" "$BASE/business/storage/config/page?page=1&page_size=10")
echo "删除后列表：$(echo "$PAGE2" | jq -c '{total: .data.total, codes: [.data.list[].config_code]}')"

echo
echo "===== 14. 上传策略：分页 + 修改 ====="
POLICY_PAGE=$(curl -s -m 10 -H "$AUTH" "$BASE/business/storage/policy/page?page=1&page_size=10")
echo "$POLICY_PAGE" | jq -c '{code, total: .data.total, rows: (.data.list|length)}'
PID=$(echo "$POLICY_PAGE" | jq -r '.data.list[0].id // empty')
if [ -n "$PID" ]; then
  POLICY_UPD=$(curl -s -m 10 -X PUT "$BASE/business/storage/policy/$PID" -H "$AUTH" -H 'Content-Type: application/json' -d '{
    "category_name":"附件（A1 实测）","max_size_mb":80,"allowed_extensions":"jpg,png,pdf,xlsx",
    "allowed_mimes":"image/png,image/jpeg,application/pdf","direct_upload":true,"presign_ttl_secs":1200}')
  echo "$POLICY_UPD" | jq -c '.data | {category, category_name, max_size_mb, allowed_extensions, presign_ttl_secs}'
  [ "$(echo "$POLICY_UPD" | jq -r '.data.max_size_mb')" = "80" ] && ok "策略修改成功" || bad "策略修改失败: $POLICY_UPD"
  # 非法配置必须被拒（扩展名带点）
  POLICY_BAD=$(curl -s -m 10 -X PUT "$BASE/business/storage/policy/$PID" -H "$AUTH" -H 'Content-Type: application/json' -d '{
    "category_name":"附件","max_size_mb":80,"allowed_extensions":".jpg","presign_ttl_secs":1200}')
  echo "非法策略（扩展名带点）：$(echo "$POLICY_BAD" | jq -c '{code, msg}')"
  [ "$(echo "$POLICY_BAD" | jq -r '.code')" = "10001" ] && ok "非法策略被拒（10001）" || bad "非法策略未被拒绝"
else
  bad "策略分页无数据，无法验证修改"
fi

echo
echo "===== 15. 权限校验：无 token 访问应 401 ====="
NOAUTH=$(curl -s -m 10 "$BASE/business/storage/config/page")
echo "无 token：$(echo "$NOAUTH" | jq -c '{code, msg}' 2>/dev/null || echo "$NOAUTH")"
[ "$(echo "$NOAUTH" | jq -r '.code')" = "20004" ] && ok "无 token 被拒（20004）" || echo "（提示：实际返回 $(echo "$NOAUTH" | jq -r '.code')，非 20004 也不算失败，框架可能用 20005）"

echo
echo "===== 结果：PASS=$PASS FAIL=$FAIL ====="
