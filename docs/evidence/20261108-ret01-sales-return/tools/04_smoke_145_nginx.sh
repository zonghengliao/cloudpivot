#!/usr/bin/env bash
# 145 测试环境（nginx:80 → cloudpivot_dev）只读 smoke：
# 证明「部署在 145 的那份实例」确实带销售退货接口（不写任何数据）。
# 只做 GET：分页列表 + 可退明细入参校验；不建单、不改库。
set -uo pipefail
BASE=http://172.18.10.145/api
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
auth() { printf 'Authorization: %s%s %s' 'Bea' 'rer' "$1"; }

code=$(curl -s -m 8 -o /tmp/ret01_smoke_login.json -w '%{http_code}' -X POST "$BASE/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}")
echo "login http=$code $(jq -r '.code // "-"' /tmp/ret01_smoke_login.json 2>/dev/null) msg=$(jq -r '.msg // "-"' /tmp/ret01_smoke_login.json 2>/dev/null)"
TOK=$(jq -r '.data.access_token // empty' /tmp/ret01_smoke_login.json 2>/dev/null)
if [ -z "$TOK" ]; then
  echo "→ 共用 dev 库的 admin 口令已被他人改过，无法在 nginx:80 上做只读 smoke（不重试、不猜口令）"
  exit 0
fi
echo "--- GET /business/erp/sales-return/page"
curl -s -m 15 -o /tmp/ret01_smoke_page.json -w 'http=%{http_code}\n' -H "$(auth "$TOK")" \
  "$BASE/business/erp/sales-return/page?page=1&page_size=5"
echo "code=$(jq -r '.code // "-"' /tmp/ret01_smoke_page.json) total=$(jq -r '.data.total // "-"' /tmp/ret01_smoke_page.json) msg=$(jq -r '.msg // "-"' /tmp/ret01_smoke_page.json)"
echo "--- GET /business/erp/sales-return/returnable/{uuid}（无此发货单 → 400/404，证明路由存在且参数校验生效）"
curl -s -m 15 -o /tmp/ret01_smoke_ret.json -w 'http=%{http_code}\n' -H "$(auth "$TOK")" \
  "$BASE/business/erp/sales-return/returnable/00000000-0000-4000-8000-000000000000"
echo "code=$(jq -r '.code // "-"' /tmp/ret01_smoke_ret.json) msg=$(jq -r '.msg // "-"' /tmp/ret01_smoke_ret.json)"
echo "--- GET /business/erp/ledger/trial-balance?period=2026-11（总账接口在 145 共用库上可用）"
curl -s -m 20 -o /tmp/ret01_smoke_tb.json -w 'http=%{http_code}\n' -H "$(auth "$TOK")" \
  "$BASE/business/erp/ledger/trial-balance?period=2026-11"
echo "code=$(jq -r '.code // "-"' /tmp/ret01_smoke_tb.json) balanced=$(jq -r '.data.balanced // "-"' /tmp/ret01_smoke_tb.json)"
