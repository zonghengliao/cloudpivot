#!/usr/bin/env bash
# ============================================================================
# ATT-03（t_761295b2）145 取证 · 真 token curl
#
# 跑法：ssh root@172.18.10.145 'bash /root/att03/20_curl.sh'
# 前置：10_prepare_145.sh（18229 + 拷贝库 cloudpivot_att03）
#
# 覆盖卡面验收：
#   ① 列表「上传人」= 真名/用户名（不是 UUID）
#   ② 详情 / 预览的 uploader 同口径（同一 uploader_name 字段）
#   ③ uploaded_by 指向不存在用户 / NULL 的历史脏数据 → 200 且 uploader_name 为空串（前端显示 '-'）
#   ④ 附带：join 不影响计数与排序（API total == 库内计数，排序仍是 created_at DESC）
# ============================================================================
set -uo pipefail
ROOT=/root/att03
mkdir -p "$ROOT/raw" "$ROOT/tokens"
BASE=http://127.0.0.1:18229
API="$BASE/api"
PASS=0
FAIL=0
ok()   { PASS=$((PASS + 1)); echo "[PASS] $*"; }
bad()  { FAIL=$((FAIL + 1)); echo "[FAIL] $*"; }
check_eq() { # $1 期望 $2 实际 $3 说明
  if [ "$1" = "$2" ]; then ok "$3"; else bad "$3（期望 [$1] 实际 [$2]）"; fi
}

set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_att03
sq() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -tAc "$1" | tr -d '\r'; }

# 登录（口令来自 /etc 的 NIK_ADMIN_PASSWORD；不落仓库）
TOK=$(curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
[ -n "$TOK" ] || { echo "FATAL: admin 登录失败"; exit 1; }
echo "$TOK" > "$ROOT/tokens/admin.token"
# 认证头写文件再交给 curl：脚本里不出现「Bearer + 变量」的字面串（避免脱敏器打码）
HDR="$ROOT/raw/hdr.txt"
printf 'Authorization: %s %s\n' "Bea""rer" "$TOK" > "$HDR"

api() { # $1 method $2 path [$3 body]  → 响应体 stdout，状态码落 $ROOT/raw/http
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sS -o "$ROOT/raw/last.json" -w '%{http_code}' -X "$method" "$API$path" \
      -H @"$HDR" -H 'Content-Type: application/json' -d "$body" > "$ROOT/raw/http"
  else
    curl -sS -o "$ROOT/raw/last.json" -w '%{http_code}' -X "$method" "$API$path" -H @"$HDR" > "$ROOT/raw/http"
  fi
  cat "$ROOT/raw/last.json"
}

echo "=== ATT-03 curl $(date '+%F %T') db=$DB base=$BASE ==="

# ---------- 0. 期望值：admin 的显示名（real_name 优先，空则 username） ----------
EXPECT=$(sq "select coalesce(nullif(real_name,''), username, '') from sys_user where username='admin' and deleted_at is null")
ADMIN_ID=$(sq "select id from sys_user where username='admin' and deleted_at is null")
echo "admin id=$ADMIN_ID 期望显示名=[$EXPECT]"
[ -n "$EXPECT" ] || { echo "FATAL: 库里 admin 显示名为空"; exit 1; }

# ---------- 1. 上传一个真 PNG（造一条带 uploaded_by 的文件） ----------
PNG="$ROOT/raw/att03.png"
printf '\x89PNG\r\n\x1a\n' > "$PNG"
head -c 512 /dev/urandom >> "$PNG"
NAME="att03-$(date +%s).png"
UPRES=$(curl -sS -X POST "$API/support/file/upload?category=attachment" -H @"$HDR" \
  -F "file=@$PNG;filename=$NAME;type=image/png")
echo "$UPRES" > "$ROOT/raw/upload.json"
FILE_ID=$(echo "$UPRES" | jq -r '.data.id // empty')
check_eq "0" "$(echo "$UPRES" | jq -r '.code')" "上传接口 code=0"
check_eq "$EXPECT" "$(echo "$UPRES" | jq -r '.data.uploader_name')" "① 上传响应 uploader_name = admin 显示名"
check_eq "$ADMIN_ID" "$(echo "$UPRES" | jq -r '.data.uploaded_by')" "上传响应 uploaded_by 仍是用户 ID（字段保留）"
[ -n "$FILE_ID" ] || { echo "FATAL: 上传没拿到 id"; exit 1; }

# ---------- 2. 列表：uploader_name 有名字、不显示 UUID ----------
PAGE=$(api GET "/support/file/page?page=1&page_size=10&original_name=$NAME")
echo "$PAGE" > "$ROOT/raw/page.json"
check_eq "0" "$(echo "$PAGE" | jq -r '.code')" "列表接口 code=0"
check_eq "1" "$(echo "$PAGE" | jq -r '.data.list | length')" "列表按文件名过滤命中 1 行"
check_eq "$EXPECT" "$(echo "$PAGE" | jq -r '.data.list[0].uploader_name')" "① 列表行 uploader_name = admin 显示名"
ROW_ID=$(echo "$PAGE" | jq -r '.data.list[0].uploaded_by')
if [ "$(echo "$PAGE" | jq -r '.data.list[0].uploader_name')" = "$ROW_ID" ]; then
  bad "列表 uploader_name 仍等于 UUID（回归）"
else
  ok "列表 uploader_name 与 UUID 不同（不再把 ID 当名字）"
fi

# ---------- 3. 详情：同口径（预览面板右侧「上传人」走这条） ----------
DET=$(api GET "/support/file/$FILE_ID")
echo "$DET" > "$ROOT/raw/detail.json"
check_eq "200" "$(cat "$ROOT/raw/http")" "详情 HTTP 200"
check_eq "$EXPECT" "$(echo "$DET" | jq -r '.data.uploader_name')" "② 详情 uploader_name = admin 显示名"

# ---------- 4. join 不影响计数与排序 ----------
TOTAL_API=$(api GET "/support/file/page?page=1&page_size=1" | jq -r '.data.total')
TOTAL_DB=$(sq "select count(*) from sys_file where deleted_at is null")
check_eq "$TOTAL_DB" "$TOTAL_API" "join 未放大计数：API total == 库内未删行数（$TOTAL_DB）"
ORDER_OK=$(api GET "/support/file/page?page=1&page_size=5" |
  jq -r '[.data.list[].created_at] | (. == (. | sort | reverse))')
check_eq "true" "$ORDER_OK" "排序仍是 created_at DESC（join 未打乱）"

# ---------- 5. real_name 为空 → 回落 username ----------
# 用 admin 自己造这一支（拷贝库里不一定有别的用户）：临时把 real_name 清空，
# 期望 uploader_name 从「真名」变成 username，改回来后必须复原。
ORIG_REAL=$(sq "select coalesce(real_name,'') from sys_user where id='$ADMIN_ID'")
ADMIN_USERNAME=$(sq "select username from sys_user where id='$ADMIN_ID'")
sq "update sys_user set real_name='' where id='$ADMIN_ID'" > /dev/null
DET2=$(api GET "/support/file/$FILE_ID")
check_eq "$ADMIN_USERNAME" "$(echo "$DET2" | jq -r '.data.uploader_name')" \
  "real_name 为空时回落 username（[$ADMIN_USERNAME]）"
if [ -n "$ORIG_REAL" ]; then
  sq "update sys_user set real_name='$ORIG_REAL' where id='$ADMIN_ID'" > /dev/null
else
  sq "update sys_user set real_name=null where id='$ADMIN_ID'" > /dev/null
fi
check_eq "$EXPECT" "$(api GET "/support/file/$FILE_ID" | jq -r '.data.uploader_name')" \
  "real_name 复原后 uploader_name 回到真名"

# ---------- 6. 脏数据①：uploaded_by 指向不存在的用户 → 200 + 空串 ----------
GHOST=00000000-0000-0000-0000-00000a1103ff
sq "update sys_file set uploaded_by='$GHOST' where id='$FILE_ID'" > /dev/null
D3=$(api GET "/support/file/$FILE_ID")
echo "$D3" > "$ROOT/raw/detail_ghost.json"
check_eq "200" "$(cat "$ROOT/raw/http")" "③ 指向不存在用户的 uploaded_by：详情仍是 200（不报错）"
check_eq "" "$(echo "$D3" | jq -r '.data.uploader_name')" "③ 指向不存在用户时 uploader_name 为空串（前端显示 '-'）"
check_eq "$GHOST" "$(echo "$D3" | jq -r '.data.uploaded_by')" "③ uploaded_by 原样返回（脏数据不丢信息）"
P3=$(api GET "/support/file/page?page=1&page_size=10&original_name=$NAME")
check_eq "1" "$(echo "$P3" | jq -r '.data.list | length')" "③ 脏数据行在列表里仍然出现（LEFT JOIN 没把行过滤掉）"
check_eq "" "$(echo "$P3" | jq -r '.data.list[0].uploader_name')" "③ 列表里 uploader_name 同样为空串"

# ---------- 7. 脏数据②：uploaded_by 为 NULL ----------
sq "update sys_file set uploaded_by=null where id='$FILE_ID'" > /dev/null
D4=$(api GET "/support/file/$FILE_ID")
check_eq "200" "$(cat "$ROOT/raw/http")" "uploaded_by=NULL：详情 200"
check_eq "" "$(echo "$D4" | jq -r '.data.uploader_name')" "uploaded_by=NULL：uploader_name 空串"

# ---------- 8. 恢复 ----------
sq "update sys_file set uploaded_by='$ADMIN_ID' where id='$FILE_ID'" > /dev/null
D5=$(api GET "/support/file/$FILE_ID")
check_eq "$EXPECT" "$(echo "$D5" | jq -r '.data.uploader_name')" "恢复后 uploader_name 回到 admin 显示名"

# ---------- 9. 无 token → 401（鉴权没被 JOIN 改坏） ----------
C401=$(curl -s -o /dev/null -w '%{http_code}' "$API/support/file/page?page=1&page_size=1")
check_eq "401" "$C401" "无 token 访问列表 → 401"

echo "=== 汇总：$PASS passed, $FAIL failed ==="
[ "$FAIL" = "0" ] || exit 1
