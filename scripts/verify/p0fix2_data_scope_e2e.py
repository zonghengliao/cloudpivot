#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""【P0-FIX-2】数据权限真机验收：配置真的生效了吗？

跑在隔离实例上（默认 127.0.0.1:18092 / 库 cloudpivot_p0fix2），全部走真实 HTTP：

  ① 夹具：两个部门（P2-财务部 / P2-销售部）+ 两个部门账号
     （p2_fin 持 FINANCE_MANAGER，p2_sal 持 DEPT_LEADER）
  ② 配置面：`PUT /api/business/permission/scope/role/{id}` 把 FINANCE_MANAGER 配成
     data_scope=5 + 自定义集合 {P2-财务部}，回读确认（含 custom_depts 明细）
  ③ 建单：admin 分别给两个部门的账号建任务 → 断言 `biz_task.dept_id` 落了
     「负责人所在部门」快照（否则部门裁剪无从谈起）
  ④ 交叉验证（本卡验收核心）：
       p2_fin 看不到 p2_sal 的任务；p2_sal 看不到 p2_fin 的任务
  ⑤ 反向对照（证明因果，不是"恰好"）：
       把 FINANCE_MANAGER 的 data_scope 改回 1（全部）→ p2_fin **立刻**能看到对方任务
       （= 修复前的空转状态）→ 改回 5 → 又看不到
  ⑥ 没把管理员限死：admin（SUPER_ADMIN）两种配置下都看得到两个部门的任务；
     SUPER_ADMIN 的 data_scope 仍是 1
  ⑦ 库里落地：sys_role_dept 有行、角色 data_scope 与接口回读一致

用法：
    python3 scripts/verify/p0fix2_data_scope_e2e.py
退出码：0 全过 / 1 有失败 / 3 打到生产库被守卫拒绝。
"""

# ---- 环境守卫：e2e 不许打到生产库 ----
import os as _os
import sys as _sys

_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402

_require_non_prod(script=_os.path.basename(__file__))

import json  # noqa: E402
import os  # noqa: E402
import subprocess  # noqa: E402
import sys  # noqa: E402
import urllib.error  # noqa: E402
import urllib.request  # noqa: E402

BASE = os.environ.get("P2_BASE", "http://127.0.0.1:18092")
DB = dict(
    host=os.environ.get("P2_DB_HOST", "127.0.0.1"),
    port=os.environ.get("P2_DB_PORT", "5432"),
    user=os.environ.get("P2_DB_USER", "alun"),
    name=os.environ.get("P2_DB_NAME", "cloudpivot_p0fix2"),
    password=os.environ.get("P2_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("P2_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P2_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("P2_NEW_PASS", "P2@Passw0rd!")   # 首登强制改密后的口令

DEPT_FIN = ("P2FIN", "P2-财务部")
DEPT_SAL = ("P2SAL", "P2-销售部")
USER_FIN, USER_SAL = "p2_fin", "p2_sal"
TASK_FIN = "P2-A 财务部任务（本部门的单据）"
TASK_SAL = "P2-B 销售部任务（别部门的单据）"

PASSED, FAILED = [], []


def check(ok, name, detail=""):
    (PASSED if ok else FAILED).append(name)
    print(("[PASS] " if ok else "[FAIL] ") + name + ("" if ok else f" — {detail}"))
    return ok


def psql(sql):
    args = ["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
            "-X", "-q", "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", sql]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB["password"]), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def one(sql, default=""):
    val = psql(sql)
    return val.split("|")[0] if val else default


def call(method, path, body=None, token=None) -> "tuple[int, dict]":
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username, password):
    """登录（兼容「首登强制改密」与「上一轮已改过口令」两种情况）。"""
    last = None
    for candidate in dict.fromkeys([password, ADMIN_PASS, NEW_PASS]):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": candidate})
        token = (body.get("data") or {}).get("access_token") or ""
        if not token:
            last = (status, body)
            continue
        if (body.get("data") or {}).get("must_change_password"):
            st, resp = call("PUT", "/api/system/auth/password",
                            {"old_password": candidate, "new_password": NEW_PASS}, token)
            if st != 200:
                raise SystemExit(f"[FATAL] {username} 改初始口令失败：{st} {resp}")
            status, body = call("POST", "/api/system/auth/login",
                                {"username": username, "password": NEW_PASS})
            token = (body.get("data") or {}).get("access_token") or ""
            if not token:
                raise SystemExit(f"[FATAL] {username} 改密后重登失败：{status} {body}")
            return token, NEW_PASS
        return token, candidate
    raise SystemExit(f"[FATAL] {username} 登录失败（试过 {password} / ADMIN / NEW）：{last}")


# ==================== 夹具 ====================

def cleanup():
    for stmt in [
        "DELETE FROM biz_task WHERE title LIKE 'P2-%'",
        "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'p2\\_%')",
        "DELETE FROM sys_user WHERE username LIKE 'p2\\_%'",
        "DELETE FROM sys_role_dept WHERE dept_id IN (SELECT id FROM sys_dept WHERE dept_code LIKE 'P2%')",
        "DELETE FROM sys_dept WHERE dept_code LIKE 'P2%'",
    ]:
        psql(stmt)


def ensure_dept(token, code, name):
    existed = one(f"select id from sys_dept where dept_code = '{code}' and deleted_at is null")
    if existed:
        return existed, False
    st, body = call("POST", "/api/system/dept",
                    {"dept_code": code, "dept_name": name, "parent_id": None,
                     "status": 1, "sort_order": 90}, token)
    if st not in (200, 201):
        raise SystemExit(f"[FATAL] 建部门 {code} 失败：{st} {body}")
    return (body.get("data") or {})["id"], True


def ensure_user(token, username, dept_id, role_codes):
    existed = one(f"select id from sys_user where username = '{username}' and deleted_at is null")
    if existed:
        return existed, False
    role_ids = []
    for code in role_codes:
        rid = one(f"select id from sys_role where role_code = '{code}' and deleted_at is null")
        if not rid:
            raise SystemExit(f"[FATAL] 角色 {code} 不存在（迁移没种上？）")
        role_ids.append(rid)
    st, body = call("POST", "/api/system/user",
                    {"username": username, "password": NEW_PASS, "real_name": username,
                     "dept_id": dept_id, "status": 1, "role_ids": role_ids}, token)
    if st not in (200, 201):
        raise SystemExit(f"[FATAL] 建用户 {username} 失败：{st} {body}")
    return (body.get("data") or {})["id"], True


def role_id(code):
    rid = one(f"select id from sys_role where role_code = '{code}' and deleted_at is null")
    if not rid:
        raise SystemExit(f"[FATAL] 角色 {code} 不存在")
    return rid


def set_scope(token, code, data_scope, depts=None):
    payload = {"data_scope": data_scope}
    if depts is not None:
        payload["custom_dept_ids"] = depts
    return call("PUT", f"/api/business/permission/scope/role/{role_id(code)}", payload, token)


def create_task(token, title, owner_id):
    st, body = call("POST", "/api/business/task", {"title": title, "owner_id": owner_id}, token)
    if st not in (200, 201):
        raise SystemExit(f"[FATAL] 建任务 {title} 失败：{st} {body}")
    return (body.get("data") or {}).get("id", "")


def visible_titles(token):
    st, body = call("GET", "/api/business/task/page?page=1&page_size=100", None, token)
    if st != 200:
        raise SystemExit(f"[FATAL] 任务列表失败：{st} {body}")
    rows = (body.get("data") or {}).get("list") or []
    return [r.get("title", "") for r in rows]


def main():
    print("=" * 92)
    print("【P0-FIX-2】数据权限真机验收 —— 目标", BASE, "/ 库", DB["name"])
    print("=" * 92)

    cleanup()

    admin_token, _ = login(ADMIN_USER, ADMIN_PASS)
    admin_id = one(f"select id from sys_user where username = '{ADMIN_USER}'")
    check(bool(admin_token), "admin 登录成功")

    # ---------- ① 夹具 ----------
    fin_dept_id, _ = ensure_dept(admin_token, *DEPT_FIN)
    sal_dept_id, _ = ensure_dept(admin_token, *DEPT_SAL)
    check(bool(fin_dept_id) and bool(sal_dept_id),
          "① 两个部门就绪（P2-财务部 / P2-销售部）", f"{fin_dept_id} / {sal_dept_id}")

    fin_uid, _ = ensure_user(admin_token, USER_FIN, fin_dept_id, ["FINANCE_MANAGER"])
    sal_uid, _ = ensure_user(admin_token, USER_SAL, sal_dept_id, ["DEPT_LEADER"])
    check(bool(fin_uid) and bool(sal_uid),
          f"① 两个部门账号就绪（{USER_FIN}@财务部 / {USER_SAL}@销售部）", f"{fin_uid} / {sal_uid}")

    # ---------- ② 配置面：CUSTOM 集合 ----------
    st, body = set_scope(admin_token, "FINANCE_MANAGER", 5, [fin_dept_id])
    data = body.get("data") or {}
    ok = st == 200 and data.get("data_scope") == 5 and fin_dept_id in (data.get("custom_dept_ids") or [])
    check(ok, "② PUT 把 FINANCE_MANAGER 配成 data_scope=5 + 集合{P2-财务部}",
          f"HTTP {st} / {json.dumps(data, ensure_ascii=False)[:300]}")

    st, body = call("GET", f"/api/business/permission/scope/role/{role_id('FINANCE_MANAGER')}",
                    None, admin_token)
    data = body.get("data") or {}
    check(st == 200 and data.get("data_scope_code") == "CUSTOM"
          and any(d.get("dept_code") == DEPT_FIN[0] for d in (data.get("custom_depts") or [])),
          "② 回读：接口报 CUSTOM + custom_depts 含 P2-财务部",
          f"HTTP {st} / {json.dumps(data, ensure_ascii=False)[:300]}")

    dl_scope = one("select data_scope from sys_role where role_code = 'DEPT_LEADER' "
                   "and deleted_at is null")
    check(dl_scope == "3", "② DEPT_LEADER 是 3（本部门及以下，V0247 授权）", f"实际 {dl_scope}")

    # ---------- ③ 建单 + dept_id 快照 ----------
    create_task(admin_token, TASK_FIN, fin_uid)
    create_task(admin_token, TASK_SAL, sal_uid)
    snap_fin = one(f"select coalesce(dept_id::text,'-') from biz_task where title = '{TASK_FIN}'")
    snap_sal = one(f"select coalesce(dept_id::text,'-') from biz_task where title = '{TASK_SAL}'")
    check(snap_fin == fin_dept_id and snap_sal == sal_dept_id,
          "③ 建单落了「负责人所在部门」快照（biz_task.dept_id）",
          f"财务部任务={snap_fin}（期望 {fin_dept_id}）/ 销售部任务={snap_sal}（期望 {sal_dept_id}）")

    # ---------- ④ 交叉验证（核心） ----------
    fin_token, fin_pass = login(USER_FIN, NEW_PASS)
    sal_token, sal_pass = login(USER_SAL, NEW_PASS)

    fin_view = visible_titles(fin_token)
    sal_view = visible_titles(sal_token)
    check(TASK_FIN in fin_view and TASK_SAL not in fin_view,
          "④ 财务部账号看得到本部门任务、看不到销售部任务", f"可见 {fin_view}")
    check(TASK_SAL in sal_view and TASK_FIN not in sal_view,
          "④ 销售部账号看得到本部门任务、看不到财务部任务", f"可见 {sal_view}")

    # ---------- ⑤ 反向对照：改回 1（空转）→ 立刻可见 ----------
    set_scope(admin_token, "FINANCE_MANAGER", 1)
    fin_view_all = visible_titles(fin_token)
    check(TASK_SAL in fin_view_all,
          "⑤ 对照：FINANCE_MANAGER 改回 data_scope=1（修复前的空转状态）→ 立刻能看到别部门任务",
          f"可见 {fin_view_all}")

    set_scope(admin_token, "FINANCE_MANAGER", 5, [fin_dept_id])
    fin_view_back = visible_titles(fin_token)
    check(TASK_SAL not in fin_view_back and TASK_FIN in fin_view_back,
          "⑤ 对照：再改回 data_scope=5 → 别部门任务重新不可见（因果成立）",
          f"可见 {fin_view_back}")

    # ---------- ⑥ 没把管理员限死 ----------
    admin_view = visible_titles(admin_token)
    check(TASK_FIN in admin_view and TASK_SAL in admin_view,
          "⑥ SUPER_ADMIN 仍能看到两个部门的任务（没被限死）", f"可见 {admin_view}")
    super_scope = one("select data_scope from sys_role where role_code = 'SUPER_ADMIN' "
                      "and deleted_at is null")
    check(super_scope == "1", "⑥ SUPER_ADMIN 的 data_scope 保持 1", f"实际 {super_scope}")

    # ---------- ⑦ 库里落地 ----------
    rows = psql("select r.role_code, r.data_scope, count(rd.id) "
                "from sys_role r left join sys_role_dept rd on rd.role_id = r.id "
                "where r.role_code in ('FINANCE_MANAGER','DEPT_LEADER','SUPER_ADMIN') "
                "and r.deleted_at is null group by 1,2 order by 1")
    fm = [r for r in rows.splitlines() if r.startswith("FINANCE_MANAGER")]
    check(bool(fm) and fm[0].split("|")[1] == "5" and int(fm[0].split("|")[2]) >= 1,
          "⑦ 库里 sys_role_dept 有行、FINANCE_MANAGER data_scope=5", rows.replace("\n", " ; "))

    total_rows = one("select count(*) from sys_role_dept")
    check(int(total_rows) > 0, "⑦ sys_role_dept 不再是 0 行", f"实际 {total_rows} 行")

    # ---------- 收尾：还原配置 + 清夹具 ----------
    set_scope(admin_token, "FINANCE_MANAGER", 1, [])
    cleanup()
    residue = one("select count(*) from biz_task where title like 'P2-%'")
    check(residue == "0", "⑧ 夹具已清理（残留 0 行）", f"残留 {residue}")

    print("\n" + "=" * 92)
    print(f"结果：{len(PASSED)}/{len(PASSED) + len(FAILED)} 通过")
    if FAILED:
        print("失败项：")
        for name in FAILED:
            print("  · " + name)
    print("=" * 92)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    try:
        _sys.exit(main())
    except RuntimeError as error:
        print(f"[FATAL] {error}", file=_sys.stderr)
        _sys.exit(1)
