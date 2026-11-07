#!/usr/bin/env bash
# ============================================================================
# FIX-1 验收 ③：**已有 demo_lead 的环境**（模拟 145）再开种子 → 不重复建
#
# 步骤：
#   1. 在一个只跑过迁移的空库上，用 API 手工建一个 `demo_lead`（模拟 145 上
#      历史演练留下的账号：口令不同、真名不同、没有角色）
#   2. 把 NIK_SEED_DEMO_USERS 打开重启
#   3. 断言：created=8 / existed 含 demo_lead / 该账号的口令与真名**没被覆盖**
#      / 用户总数 = 1(admin) + 1(老 demo_lead) + 8(新建) = 10（不多不少）
# ============================================================================
set -uo pipefail
BASE="http://127.0.0.1:8036"
API="$BASE/api"
HTTP=""

login() {
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r '.data.access_token // empty'
}
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}"
  local out
  if [ -n "$body" ]; then
    out=$(curl -sS -o /tmp/_b3.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    out=$(curl -sS -o /tmp/_b3.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$out"; cat /tmp/_b3.json
}

echo "===== 1. admin 登录 + 改密 ====="
INIT=$(grep -m1 ^NIK_ADMIN_PASSWORD= /root/fix1/env3 | cut -d= -f2-)
T=$(login admin "$INIT")
curl -sS -o /dev/null -X PUT "$API/system/auth/password" -H "Authorization: Bearer $T" \
  -H 'Content-Type: application/json' -d "{\"old_password\":\"$INIT\",\"new_password\":\"Admin@Fix1c\"}"
T=$(login admin "Admin@Fix1c")
echo "admin token_len=${#T}"

echo
echo "===== 2. 手工建一个「历史遗留」的 demo_lead（口令 Legacy@2026 / 真名 老演练主管）====="
api POST /system/user "$T" \
  '{"username":"demo_lead","password":"Legacy@2026","real_name":"老演练主管","status":1,"role_ids":[]}' > /tmp/_u.json
echo "create user http=$HTTP $(cat /tmp/_u.json)"
LEGACY_ID=$(curl -sS "$API/system/user/page?page=1&page_size=20&username=demo_lead" -H "Authorization: Bearer $T" | jq -r '.data.list[0].id')
echo "legacy demo_lead id=$LEGACY_ID"

echo
echo "===== 3. 手工建的角色绑定数（应为 0）====="
echo "roles before seed: $(curl -sS "$API/system/user/$LEGACY_ID/roles" -H "Authorization: Bearer $T")"

echo
echo "===== 4. 停实例 → 打开 NIK_SEED_DEMO_USERS → 重启 ====="
pkill -f cloudpivot-fix1
sleep 3
sed -i 's/^NIK_SEED_DEMO_USERS=.*/NIK_SEED_DEMO_USERS=true/' /root/fix1/env3
cd /root/fix1 && (./fix1_start_instance.sh /root/fix1/env3 > /root/fix1/run3b.log 2>&1 &)
sleep 25
echo "----- 启动日志里的种子结果 -----"
grep -o '演示账号种子完成.*' /root/fix1/run3b.log | head -1

echo
echo "===== 5. 断言：老 demo_lead 没被覆盖（还是 Legacy@2026 / 老演练主管）====="
LT=$(login demo_lead "Legacy@2026")
echo "login demo_lead with ORIGINAL password -> token_len=${#LT}（>0 = 口令没被覆盖）"
ME=$(curl -sS "$API/system/auth/me" -H "Authorization: Bearer $LT" 2>/dev/null)
echo "me: $(echo "$ME" | jq -c '{username:.data.username, real_name:.data.real_name, must_change_password:.data.must_change_password}' 2>/dev/null)"
echo "roles now: $(curl -sS "$API/system/user/$LEGACY_ID/roles" -H "Authorization: Bearer $T")"

echo
echo "===== 6. 用户总数（应为 10：admin + 老 demo_lead + 新建 8）====="
curl -sS "$API/system/user/page?page=1&page_size=100" -H "Authorization: Bearer $T" |
  jq -c '{total:.data.total, users:[.data.list[].username]}'
