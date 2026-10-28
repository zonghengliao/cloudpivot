#!/usr/bin/env bash
# V0040 在真实 PostgreSQL 上的约束断言（批次 A · A1 验收第 2 条）
# 用法：bash verify_v0040.sh <dbname>
# 说明：全部在事务内插入非法数据 / 或在断言后立即清理，不污染业务数据。
set -uo pipefail

DB="${1:-cloudpivot_test}"
PSQL="psql -h 127.0.0.1 -U ${ALUN_DATABASE_USER} -d ${DB} -v ON_ERROR_STOP=0 -tA"
PASS=0
FAIL=0

# expect_fail <编号> <说明> <期望错误片段> <SQL>
expect_fail() {
  local id="$1" desc="$2" needle="$3" sql="$4"
  local out
  out=$($PSQL -c "$sql" 2>&1)
  if echo "$out" | grep -q "$needle"; then
    echo "PASS $id $desc  → 命中 $needle"
    PASS=$((PASS + 1))
  else
    echo "FAIL $id $desc  → 期望包含 [$needle]，实际：$out"
    FAIL=$((FAIL + 1))
  fi
}

# expect_ok <编号> <说明> <SQL>
expect_ok() {
  local id="$1" desc="$2" sql="$3"
  local out
  out=$($PSQL -c "$sql" 2>&1)
  if echo "$out" | grep -qiE "error|错误"; then
    echo "FAIL $id $desc  → 期望成功，实际：$out"
    FAIL=$((FAIL + 1))
  else
    echo "PASS $id $desc"
    PASS=$((PASS + 1))
  fi
}

echo "==== 目标库：${DB} ===="

# 清理上一次残留
$PSQL -c "DELETE FROM sys_storage_config WHERE config_code LIKE 'chk-%'" >/dev/null 2>&1
$PSQL -c "DELETE FROM sys_storage_policy WHERE category LIKE 'chk-%'" >/dev/null 2>&1

LOCAL_ROW="INSERT INTO sys_storage_config (config_code, config_name, backend_type, is_default, base_path) VALUES ('chk-local-a','检查用本地A','LOCAL',TRUE,'')"
S3_FULL="INSERT INTO sys_storage_config (config_code, config_name, backend_type, endpoint, bucket, ak_cipher, ak_nonce, sk_cipher, sk_nonce) VALUES ('chk-s3-a','检查用S3','S3','http://minio.local:9000','mybucket','ct','n','ct','n')"

expect_ok   T0  "插入第一个默认本地后端（合法）" "$LOCAL_ROW"
expect_fail T1  "插入第二个默认后端（应被唯一索引拦住）" "uk_sys_storage_config_default" \
  "INSERT INTO sys_storage_config (config_code, config_name, backend_type, is_default, base_path) VALUES ('chk-local-b','检查用本地B','LOCAL',TRUE,'')"
expect_fail T2  "S3 后端缺 AK/SK 密文（应被 ck_remote 拦住）" "ck_sys_storage_config_remote" \
  "INSERT INTO sys_storage_config (config_code, config_name, backend_type, endpoint, bucket) VALUES ('chk-s3-nokey','缺密钥','S3','http://minio.local:9000','mybucket')"
expect_ok   T2b "S3 后端带全密文（合法）" "${S3_FULL/chk-s3-a/chk-s3-ok}"
expect_fail T3  "本地后端携带密钥（应被 ck_remote 拦住）" "ck_sys_storage_config_remote" \
  "INSERT INTO sys_storage_config (config_code, config_name, backend_type, ak_cipher, ak_nonce, sk_cipher, sk_nonce) VALUES ('chk-local-key','本地带密钥','LOCAL','ct','n','ct','n')"
expect_fail T4  "backend_type='OSS'（应被 ck_backend 拦住）" "ck_sys_storage_config_backend" \
  "INSERT INTO sys_storage_config (config_code, config_name, backend_type) VALUES ('chk-oss','非法类型','OSS')"
expect_fail T4b "config_code='Chk_Bad'（应被 ck_code 拦住）" "ck_sys_storage_config_code" \
  "INSERT INTO sys_storage_config (config_code, config_name, backend_type) VALUES ('Chk_Bad','非法编码','LOCAL')"
expect_fail T5  "策略 max_size_mb=0（应被 ck_size 拦住）" "ck_sys_storage_policy_size" \
  "INSERT INTO sys_storage_policy (category, category_name, max_size_mb) VALUES ('chk-p1','检查','0')"
expect_fail T5b "策略扩展名带点 '.png,.jpg'（应被 ck_ext 拦住）" "ck_sys_storage_policy_ext" \
  "INSERT INTO sys_storage_policy (category, category_name, allowed_extensions) VALUES ('chk-p2','检查','.png,.jpg')"
expect_fail T5c "策略 TTL 越界 30 秒（应被 ck_ttl 拦住）" "ck_sys_storage_policy_ttl" \
  "INSERT INTO sys_storage_policy (category, category_name, presign_ttl_secs) VALUES ('chk-p3','检查',30)"
expect_ok   T5d "策略合法值（jpg,png / 900 秒）" \
  "INSERT INTO sys_storage_policy (category, category_name, max_size_mb, allowed_extensions, presign_ttl_secs) VALUES ('chk-p-ok','检查','50','jpg,png',900)"

# sys_file 扩展列断言
expect_fail T6  "同 file_hash 第二条有效记录（应被 uk_sys_file_hash 拦住）" "uk_sys_file_hash" \
  "WITH a AS (INSERT INTO sys_file (file_name, file_key, file_size, file_hash) VALUES ('a.txt','chk/a.txt',1,'deadbeef') RETURNING id) INSERT INTO sys_file (file_name, file_key, file_size, file_hash) SELECT 'b.txt','chk/b.txt',1,'deadbeef' FROM a"
expect_ok   T6b "软删后同 hash 可重新入库" \
  "UPDATE sys_file SET deleted_at = now() WHERE file_hash = 'deadbeef'; INSERT INTO sys_file (file_name, file_key, file_size, file_hash) VALUES ('c.txt','chk/c.txt',1,'deadbeef')"
expect_fail T6c "upload_mode='FTP'（应被 ck_sys_file_upload_mode 拦住）" "ck_sys_file_upload_mode" \
  "INSERT INTO sys_file (file_name, file_key, file_size, upload_mode) VALUES ('d.txt','chk/d.txt',1,'FTP')"
expect_ok   T6d "storage_code 列可写入后端编码" \
  "INSERT INTO sys_file (file_name, file_key, file_size, storage_code) VALUES ('e.txt','chk/e.txt',1,'chk-local-a')"

# 清理
$PSQL -c "DELETE FROM sys_file WHERE file_key LIKE 'chk/%'" >/dev/null 2>&1
$PSQL -c "DELETE FROM sys_storage_config WHERE config_code LIKE 'chk-%'" >/dev/null 2>&1
$PSQL -c "DELETE FROM sys_storage_policy WHERE category LIKE 'chk-%'" >/dev/null 2>&1

echo "==== 结果：PASS=$PASS FAIL=$FAIL ===="
[ "$FAIL" -eq 0 ]
