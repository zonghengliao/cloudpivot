#!/usr/bin/env bash
# V0201 成本法收敛：真机 HTTP 验收（隔离实例 18097 / 库 cp_v0201_iso）
set -u
BASE=http://127.0.0.1:18097/api
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d cp_v0201_iso -tAc"

echo "=== ① 迁移落库核对 ==="
$PSQL "select version||' '||name from schema_migrations where kind='versioned' and version='201'"
$PSQL "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_material_costing'"

echo
echo "=== ② 登录 ==="
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(printf '%s' "$LOGIN" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("data",{}).get("access_token",""))')
if [ -z "$TOKEN" ]; then echo "登录失败: $LOGIN"; exit 1; fi
echo "token 长度=${#TOKEN}"

# 首登强制改密（session_guard 403）：走改密接口，然后用新口令重登
curl -s -m 10 -o /tmp/v0201_chpwd.json -w 'change-password HTTP=%{http_code}\n' -X PUT "$BASE/system/auth/password" \
  -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
  -d '{"old_password":"Admin@123456","new_password":"Admin@1234567"}'
cat /tmp/v0201_chpwd.json; echo
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@1234567"}')
TOKEN=$(printf '%s' "$LOGIN" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("data",{}).get("access_token",""))')
if [ -z "$TOKEN" ]; then echo "改密后登录失败: $LOGIN"; exit 1; fi
echo "重登成功，token 长度=${#TOKEN}"

UNIT=$($PSQL "select id from biz_unit where is_base=1 and status=1 order by unit_code limit 1")
echo "unit_id=$UNIT"

post_material () { # $1=code $2=costing $3=名字
  curl -s -m 10 -o /tmp/v0201_resp.json -w '%{http_code}' -X POST "$BASE/business/erp/material" \
    -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
    -d "{\"material_code\":\"$1\",\"material_name\":\"$3\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"costing_method\":\"$2\"}"
}

echo
echo "=== ③ 提交 FIFO（期望 400 + 指定文案） ==="
CODE=$(post_material V0201-E2E-FIFO FIFO '成本法验收-FIFO')
echo "HTTP=$CODE"
cat /tmp/v0201_resp.json; echo

echo
echo "=== ④ 提交 STANDARD（期望 400 + 指定文案） ==="
CODE=$(post_material V0201-E2E-STD STANDARD '成本法验收-STANDARD')
echo "HTTP=$CODE"
cat /tmp/v0201_resp.json; echo

echo
echo "=== ⑤ 提交 MOVING_AVG（期望 200 + 落库） ==="
CODE=$(post_material V0201-E2E-OK MOVING_AVG '成本法验收-移动加权')
echo "HTTP=$CODE"
cat /tmp/v0201_resp.json | head -c 600; echo
MID=$(python3 -c 'import json;d=json.load(open("/tmp/v0201_resp.json"));print(d.get("data",{}).get("id",""))')
echo "新物料 id=$MID"
$PSQL "select material_code||' => '||costing_method from biz_material where material_code='V0201-E2E-OK'"

echo
echo "=== ⑥ 编辑同一条改成 FIFO（期望 400） ==="
CODE=$(curl -s -m 10 -o /tmp/v0201_resp2.json -w '%{http_code}' -X PUT "$BASE/business/erp/material/$MID" \
  -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" -d '{"costing_method":"FIFO"}')
echo "HTTP=$CODE"
cat /tmp/v0201_resp2.json; echo

echo
echo "=== ⑦ 编辑其它字段（不带 costing_method，期望 200 且成本法不变） ==="
CODE=$(curl -s -m 10 -o /tmp/v0201_resp3.json -w '%{http_code}' -X PUT "$BASE/business/erp/material/$MID" \
  -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" -d '{"material_name":"成本法验收-移动加权-改名"}')
echo "HTTP=$CODE"
$PSQL "select material_name||' / '||costing_method from biz_material where id='$MID'"

echo
echo "=== ⑧ 绕过接口直接写库（DB CHECK 兜底，期望被拒） ==="
$PSQL "update biz_material set costing_method='FIFO' where id='$MID'" 2>&1 | head -2
$PSQL "select material_code||' => '||costing_method from biz_material where id='$MID'"

echo
echo "=== ⑨ 收尾：删掉验收物料（软删接口无 → 直接删行，隔离库专用） ==="
$PSQL "delete from biz_material where material_code like 'V0201-E2E-%'" && echo "验收行已清理"
