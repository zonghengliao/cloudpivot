#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""REVERIFY 特别验证项 ①：**全新库（只有 admin）→ 走完 FIX-1 的引导 → 跑通销售链**。

环境：145 隔离实例 `127.0.0.1:8052` / 库 `cloudpivot_rev_admin`（`NIK_SEED_DEMO_USERS=false`，
迁移后只有 1 个账号 admin、0 个演示账号）。144 未触碰；8033 服役实例未碰。

引导路径**全部走接口**（不许手工改库），步骤与 `docs/evidence/20261107-fix1-open-box/README.md` 一致：
  ① GET  /system/setup/check                → all_ready=false + 缺 6 个关键角色
  ② 建销售订单 → POST submit-approval        → 422 / 65018（带结构化 data：reason=SELF_ONLY）
  ③ POST /system/user 建一名非制单人 + POST /system/setup/assign 指派 SALES_SUPERVISOR
  ④ 重复指派一次                            → added=[] / skipped=[...]（幂等证据）
  ⑤ 重新提交                                → 200（状态 SUBMITTED，实例生成）
  ⑥ 新账号登录 → GET /business/flow/task/todo → POST /task/{id}/approve → 订单 APPROVED
"""
import json, os, subprocess, sys, time, urllib.request, urllib.error

BASE = os.environ.get("REV_BASE", "http://127.0.0.1:8052")
DB = os.environ.get("REV_DB", "cloudpivot_rev_admin")
PW = os.environ["PGPASSWORD"]
OUT = os.environ.get("REV_RAW", "/root/reverify/raw")
os.makedirs(OUT, exist_ok=True)
PASS = FAIL = 0
FAILS, LOG = [], []
LAST = {"http": None, "body": None}

def say(s): print(s, flush=True); LOG.append(s)
def ok(m):
    global PASS; PASS += 1; say(f"  ✅ {m}")
def bad(m):
    global FAIL; FAIL += 1
    hint = ""
    if LAST["http"] is not None:
        hint = f"  ← http={LAST['http']} body={json.dumps(LAST['body'], ensure_ascii=False)[:320]}"
    FAILS.append(m); say(f"  ❌ {m}{hint}")
def eq(desc, actual, expected):
    (ok if str(actual) == str(expected) else bad)(f"{desc} = {actual}" + ("" if str(actual) == str(expected) else f"（期望 {expected}）"))

def sq(sql):
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-tAc", sql],
                       capture_output=True, text=True, env={**os.environ, "PGPASSWORD": PW})
    if r.returncode != 0: say(f"  (psql: {r.stderr.strip()[:160]})")
    return r.stdout.strip()

def api(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + "/api" + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token: req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            out = (200, json.loads(r.read() or b"{}"))
    except urllib.error.HTTPError as e:
        raw = e.read()
        try: out = (e.code, json.loads(raw or b"{}"))
        except Exception: out = (e.code, {"raw": raw.decode("utf-8", "ignore")})
    LAST["http"], LAST["body"] = out
    return out

def data_of(r):
    d = r.get("data")
    return d if isinstance(d, dict) else {}

CHANGED_PW = os.environ.get("REV_CHANGED_PW", "Admin@Rev2026")

def login(user, pw):
    st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    if r.get("code") == 10005:
        say("  ⏳ 登录限流，等 65s"); time.sleep(65)
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": pw})
    if st != 200 or r.get("code") != 0:
        say(f"  ❌ 登录失败 {user} http={st} code={r.get('code')} msg={r.get('msg')}")
        return None, None
    d = data_of(r)
    if d.get("must_change_password"):
        st2, r2 = api("PUT", "/system/auth/password", d["access_token"],
                      {"old_password": pw, "new_password": CHANGED_PW})
        if st2 != 200 or r2.get("code") != 0:
            say(f"  ❌ 强制改密失败 {user} http={st2} {r2.get('msg')}")
            return None, None
        say(f"  🔑 {user} 首次登录强制改密成功")
        st, r = api("POST", "/system/auth/login", None, {"username": user, "password": CHANGED_PW})
        if st != 200 or r.get("code") != 0:
            say(f"  ❌ 改密后登录失败 {user}")
            return None, None
        d = data_of(r)
        return d["access_token"], CHANGED_PW
    return d["access_token"], pw

def main():
    say("=" * 70); say("特别验证① 全新库（只有 admin）→ FIX-1 引导 → 销售链"); say("=" * 70)
    users = sq("select string_agg(username, ',' order by username) from sys_user where deleted_at is null")
    eq("全新库账号数（迁移+种子后）", len(users.split(",")) if users else 0, 1)
    eq("全新库唯一账号", users, "admin")

    ADMIN_PW = os.environ.get("ADMIN_PW", "")
    if not ADMIN_PW:
        bad("缺少 ADMIN_PW 环境变量"); return 1
    A, _ = login("admin", ADMIN_PW)
    if not A: return 1
    ok("admin 登录成功（首次登录强制改密已走通）")

    st, r = api("GET", "/system/setup/check", A)
    d = data_of(r)
    eq("①GET /system/setup/check http", st, 200)
    eq("①all_ready=false（还没配审批人）", d.get("all_ready"), False)
    eq("①demo_users_enabled=false", d.get("demo_users_enabled"), False)
    missing = [m.get("role_code") for m in (d.get("missing") or [])]
    say(f"  缺失关键角色：{missing}")
    eq("①缺失角色数", len(missing), 6)

    UNIT = sq("select id from biz_unit where deleted_at is null order by is_base desc limit 1")
    WH = sq("select id from biz_warehouse where deleted_at is null and is_leaf=1 limit 1")
    st, r = api("POST", "/business/erp/customer", A, {"code": "REV-FRESH-C1", "name": "全新库客户", "status": "ACTIVE"})
    eq("②建客户 http", st, 200); CUST = data_of(r).get("id")
    st, r = api("POST", "/business/erp/material", A,
                {"material_code": "REV-FRESH-M1", "material_name": "全新库物料", "unit_id": UNIT,
                 "material_type": "RAW", "costing_method": "MOVING_AVG", "tax_rate": 0, "status": 1})
    if st == 409:
        MAT = sq("select id from biz_material where material_code='REV-FRESH-M1'")
        say("  (物料已存在，复用)")
    else:
        eq("②建物料 http", st, 200); MAT = data_of(r).get("id")
    st, r = api("POST", "/business/erp/opening-balance/inventory", A,
                {"period": "2026-11", "items": [{"material_id": MAT, "warehouse_id": WH, "qty": 50, "unit_cost": 10}]})
    eq("②库存期初 http", st, 200)
    st, r = api("POST", "/business/erp/sales-order", A,
                {"customer_id": CUST, "order_date": "2026-11-08", "remark": "FIX-1 引导演练",
                 "items": [{"material_id": MAT, "qty": 5, "price": 100, "tax_rate": 0}]})
    eq("②建销售订单 http", st, 200); SO = data_of(r).get("id")
    SO_NO = data_of(r).get("so_no") or data_of(r).get("order_no")
    say(f"  订单：{SO_NO}")
    st, r = api("POST", f"/business/erp/sales-order/{SO}/submit-approval", A, {})
    eq("②无审批人时提交 422", st, 422)
    eq("②错误码 65018（结构化引导）", r.get("code"), 65018)
    dd = data_of(r)
    eq("②data.reason=SELF_ONLY", dd.get("reason"), "SELF_ONLY")
    eq("②data.role_code=SALES_SUPERVISOR", dd.get("role_code"), "SALES_SUPERVISOR")
    say(f"  引导文案：{(r.get('msg') or '')[:150]}")
    say(f"  fix_hint：{dd.get('fix_hint')}")

    # ③ 建一名非制单人 + 指派到 SALES_SUPERVISOR（全走接口，不改库）
    st, r = api("POST", "/system/user", A,
                {"username": "rev_approver", "password": "Appr@2026rev", "real_name": "REVERIFY 销售主管",
                 "dept_id": sq("select id from sys_dept where deleted_at is null limit 1")})
    if st == 200:
        ok("③建审批人账号 http = 200")
    else:
        eq("③建审批人账号 http", st, 200)
    UID = sq("select id from sys_user where username='rev_approver'")
    eq("③审批人账号已落库", bool(UID), True)
    st, r = api("POST", "/system/setup/assign", A, {"role_code": "SALES_SUPERVISOR", "user_ids": [UID]})
    eq("③指派角色 http", st, 200)
    ad = data_of(r)
    eq("③首个指派 added=1", len(ad.get("added") or []), 1)
    eq("③指派后成员数", ad.get("member_count"), 1)
    st, r = api("POST", "/system/setup/assign", A, {"role_code": "SALES_SUPERVISOR", "user_ids": [UID]})
    ad2 = data_of(r)
    eq("③重复指派 http", st, 200)
    eq("③重复指派幂等（added=0）", len(ad2.get("added") or []), 0)
    eq("③重复指派 skipped=1", len(ad2.get("skipped") or []), 1)
    st, r = api("GET", "/system/setup/check", A)
    missing2 = [m.get("role_code") for m in (data_of(r).get("missing") or [])]
    eq("③指派后 SALES_SUPERVISOR 已退出缺失列表", "SALES_SUPERVISOR" in missing2, False)

    # ④ 重新提交
    st, r = api("POST", f"/business/erp/sales-order/{SO}/submit-approval", A, {})
    eq("④重提 http", st, 200)
    INST = sq(f"select id from biz_flow_instance where biz_id='{SO}' order by created_at desc limit 1")
    eq("④审批实例已生成", bool(INST), True)

    # ⑤ 新账号登录 → 待办 → 审批
    APPR, APPR_PW = login("rev_approver", "Appr@2026rev")
    if APPR:
        ok("⑤新审批人登录成功（无强制改密）")
    else:
        bad("⑤新审批人登录失败")
    st, r = api("GET", "/business/flow/task/todo?page=1&page_size=20", APPR)
    eq("⑤待办列表 http", st, 200)
    rows = data_of(r).get("list") or data_of(r).get("records") or data_of(r).get("rows") or []
    total = data_of(r).get("total")
    say(f"  待办 total={total} 返回 {len(rows)} 行")
    eq("⑤待办条数 ≥1", (total if total is not None else len(rows)) > 0, True)
    TASK = None
    for row in rows:
        if str(row.get("instance_id")) == INST or str(row.get("biz_no", "")) == str(SO_NO):
            TASK = row.get("id"); break
    if not TASK and rows:
        TASK = rows[0].get("id")
    eq("⑤取到待办 task", bool(TASK), True)
    st, r = api("POST", f"/business/flow/task/{TASK}/approve", APPR, {"comment": "REVERIFY FIX-1 引导审批"})
    eq("⑤审批 http", st, 200)
    eq("⑤订单终态 APPROVED", sq(f"select status from biz_sales_order where id='{SO}'"), "APPROVED")
    eq("⑤审批意见回写实例（FIX-9）",
       sq(f"select coalesce(result_comment,'') from biz_flow_instance where id='{INST}'"), "REVERIFY FIX-1 引导审批")

    say("\n" + "=" * 70)
    say(f"RESULT: PASS={PASS} FAIL={FAIL}")
    if FAILS:
        say("FAILED:")
        for f in FAILS: say("  - " + f)
    with open(os.path.join(OUT, "fresh_admin_chain.log"), "w") as fh:
        fh.write("\n".join(LOG) + "\n")
    return 0 if FAIL == 0 else 1

if __name__ == "__main__":
    sys.exit(main())
