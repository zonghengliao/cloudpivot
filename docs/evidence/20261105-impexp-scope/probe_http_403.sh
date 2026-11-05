#!/usr/bin/env bash
# 现状探针：3 个导入导出接口在**当前生产授权**下对三档业务角色是否放行（期望 403）
# 身份：mint_token.py 按库内真值自签（等价真登录，见该文件头部三条边界）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
cd /opt/cloudpivot/scripts/datafix/bizperm

BASE=http://127.0.0.1:8023
EPS=(
  "/api/business/impexp/import/page?page=1&page_size=1"
  "/api/business/impexp/export/page?page=1&page_size=1"
  "/api/business/impexp/def/list"
)
for u in admin laoliu laoliu_test henry.liao; do
  TOK=$(python3 mint_token.py --username "$u" 2>/dev/null) || { echo "$u: 令牌签发失败"; continue; }
  for ep in "${EPS[@]}"; do
    body=$(mktemp)
    code=$(curl -s -o "$body" -w '%{http_code}' -H "Authorization: Bearer $TOK" "$BASE$ep")
    printf '%-12s %-58s -> %s  %s\n' "$u" "$ep" "$code" "$(head -c 140 "$body" | tr -d '\n')"
    rm -f "$body"
  done
  # 上传向导（POST，multipart；只验权限闸，不给文件 ⇒ 403 说明被权限挡住，非 400）
  body=$(mktemp)
  code=$(curl -s -o "$body" -w '%{http_code}' -X POST -H "Authorization: Bearer $TOK" \
              "$BASE/api/business/impexp/import/upload?biz_type=system.dict_item&mode=STRICT")
  printf '%-12s %-58s -> %s  %s\n' "$u" "POST /api/business/impexp/import/upload" "$code" "$(head -c 140 "$body" | tr -d '\n')"
  rm -f "$body"
  echo
done
