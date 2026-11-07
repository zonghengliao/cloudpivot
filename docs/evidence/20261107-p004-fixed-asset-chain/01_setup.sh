#!/usr/bin/env bash
# ① 演练前置：登录 admin、建演练部门、建总经理审批账号 gm_drill
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh

echo "===== 登录 admin ====="
ADMIN=$(login admin "$ADMIN_PASS")
echo "admin token len=${#ADMIN}"
[ -n "$ADMIN" ] || { echo "FATAL: admin 登录失败"; exit 1; }
echo "$ADMIN" > "$EV/raw/admin.token"

echo "===== A1 建部门（资产必须挂在部门上；dev 库 sys_dept 原为空）====="
api POST "/system/dept" "$ADMIN" '{"dept_code":"DRILL_ASSET","dept_name":"演练设备部","status":1,"sort_order":90}' > "$EV/raw/a1_dept.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/a1_dept.json")"
DEPT=$(jq -r '.data.id // empty' "$EV/raw/a1_dept.json")
if [ -z "$DEPT" ]; then
  # 已存在则查（幂等重跑）
  API_GET=$(api GET "/system/dept/tree" "$ADMIN")
  DEPT=$(echo "$API_GET" | jq -r '[..|objects|select(.dept_code?=="DRILL_ASSET")][0].id // empty')
fi
echo "dept_id=$DEPT"
echo "$DEPT" > "$EV/raw/dept.id"

echo "===== A2 建审批人 gm_drill（GENERAL_MANAGER：处置流程 n_gm 节点只派这个角色）====="
api POST "/system/user" "$ADMIN" "{\"username\":\"gm_drill\",\"password\":\"Drill@123456\",\"real_name\":\"演练总经理\",\"dept_id\":\"$DEPT\",\"status\":1,\"role_ids\":[\"20000000-0000-4000-8000-0000000f0003\"]}" > "$EV/raw/a2_user.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/a2_user.json")"
GMID=$(jq -r '.data.id // empty' "$EV/raw/a2_user.json")
echo "gm_drill id=$GMID"
echo "$GMID" > "$EV/raw/gm.id"

echo "===== A3 gm_drill 登录 ====="
GM=$(login gm_drill 'Drill@123456')
echo "gm_drill token len=${#GM}"
echo "$GM" > "$EV/raw/gm.token"
