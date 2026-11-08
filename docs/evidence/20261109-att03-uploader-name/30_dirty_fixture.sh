#!/usr/bin/env bash
# ============================================================================
# ATT-03（t_761295b2）145 取证 · 造「历史脏数据」夹具
#
# 场景：`sys_file.uploaded_by` 指向一个**不存在的用户**（真实历史上的删号残留）。
# 没有任何接口能造这种数据（也不该有），只能在隔离实例的拷贝库里 UPDATE。
# 造完后把 id / 原始文件名打到 stdout，交给 e2e（E2E_DIRTY_FILE_ID）与 curl 复核。
# ============================================================================
set -uo pipefail
ROOT=/root/att03
BASE=http://127.0.0.1:18229
API="$BASE/api"

set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_att03
sq() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -tAc "$1" | tr -d '\r'; }

TOK=$(curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
[ -n "$TOK" ] || { echo "FATAL: admin 登录失败" >&2; exit 1; }
HDR="$ROOT/raw/dirty_hdr.txt"
printf 'Authorization: %s %s\n' "Bea""rer" "$TOK" > "$HDR"

PNG="$ROOT/raw/att03-dirty.png"
printf '\x89PNG\r\n\x1a\n' > "$PNG"
head -c 512 /dev/urandom >> "$PNG"
NAME="att03-dirty-$(date +%s).png"
RES=$(curl -sS -X POST "$API/support/file/upload?category=attachment" -H @"$HDR" \
  -F "file=@$PNG;filename=$NAME;type=image/png")
ID=$(echo "$RES" | jq -r '.data.id // empty')
[ -n "$ID" ] || { echo "FATAL: 上传失败：$RES" >&2; exit 1; }
echo "上传响应 uploader_name=[$(echo "$RES" | jq -r '.data.uploader_name')]" >&2

GHOST=00000000-0000-0000-0000-00000a1103ff
sq "update sys_file set uploaded_by='$GHOST' where id='$ID'" > /dev/null
echo "改脏后 uploader_name=[$(curl -sS "$API/support/file/$ID" -H @"$HDR" | jq -r '.data.uploader_name')]（应为空）" >&2

echo "DIRTY_ID=$ID"
echo "DIRTY_NAME=$NAME"
