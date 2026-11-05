#!/usr/bin/env bash
# 对照探针：三档账号（laoliu_test，部门 TECH，data_scope=3）在**业务列表页**上
# 现在就能看到多少条 ERP 单据 —— 用来量化「导出到底有没有多给范围」。
# 与 prod/03 的导出全量（stock_in 14 / payable 12 / voucher 21）逐项比对。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
cd /opt/cloudpivot/scripts/datafix/bizperm
BASE=http://127.0.0.1:8023
EPS=(
  "/api/business/erp/stock-in/page?page=1&page_size=5"
  "/api/business/erp/payable/page?page=1&page_size=5"
  "/api/business/erp/voucher/page?page=1&page_size=5"
)
for u in laoliu_test admin; do
  TOK=$(python3 mint_token.py --username "$u" 2>/dev/null) || { echo "$u: 令牌签发失败"; continue; }
  for ep in "${EPS[@]}"; do
    body=$(mktemp)
    code=$(curl -s -o "$body" -w '%{http_code}' -H "Authorization: Bearer $TOK" "$BASE$ep")
    total=$(python3 - "$body" <<'PY'
import json,sys
try:
    d=json.load(open(sys.argv[1]))
    print(d.get("data",{}).get("total"))
except Exception:
    print("n/a")
PY
)
    printf '%-12s %-52s -> %s  total=%s\n' "$u" "$ep" "$code" "$total"
    rm -f "$body"
  done
  echo
done
