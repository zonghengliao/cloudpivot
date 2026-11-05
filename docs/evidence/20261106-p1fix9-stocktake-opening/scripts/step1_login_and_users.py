import json, sys
sys.path.insert(0, '/root/st9-run')
from apilib import Api

OUT = []


def log(title, payload):
    line = f"### {title}\n{json.dumps(payload, ensure_ascii=False, indent=2) if not isinstance(payload, str) else payload}"
    print(line)
    OUT.append(line)


a = Api()
a.login("admin", "***REDACTED***")
# 首次登录强制改密（产品红线）：走真实接口，不碰库
r = a.call("PUT", "/system/auth/password", {"old_password": "***REDACTED***", "new_password": "${ST9_ADMIN_PASSWORD}"})
log("1) admin 首登改密", {"http": r["_http"], "ok": r["_ok"], "resp": r.get("_error") or "OK"})

a2 = Api()
tok = a2.login("admin", "${ST9_ADMIN_PASSWORD}")
log("2) admin 用新口令登录", {"must_change_password": tok.get("must_change_password"), "user": tok.get("user")})
a = a2

roles = a.ok("GET", "/system/role/list")
role_rows = roles if isinstance(roles, list) else roles.get("list") or roles.get("rows") or roles
log("3) 角色清单（截取）", [{"role_code": x.get("role_code"), "id": x.get("id"), "role_name": x.get("role_name")} for x in role_rows][:12])
wm = [x for x in role_rows if x.get("role_code") == "WAREHOUSE_MANAGER"]
log("4) WAREHOUSE_MANAGER", wm)

# 期初/盘点需要的会计期间
periods = a.ok("GET", "/business/erp/period/page?page=1&page_size=30")
rows = periods.get("list") or periods.get("rows") or periods.get("records") or []
log("5) 期间（2026 年）", [{"period": x.get("period") or x.get("period_code"), "status": x.get("status")} for x in rows if str(x.get("period") or x.get("period_code")).startswith("2026-")][:14])

# 第二个用户：仓管主管（盘点审批人，必须 ≠ 制单人）
created = a.ok("POST", "/system/user", {
    "username": "st_wh",
    "password": "${ST9_WH_PASSWORD}",
    "real_name": "盘点仓管主管",
    "status": 1,
    "role_ids": [wm[0]["id"]] if wm else [],
})
log("6) 建审批用户 st_wh", created)

with open("/root/st9-run/step1.txt", "w") as fh:
    fh.write("\n".join(OUT) + "\n")
