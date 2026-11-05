#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""组织落位真机验收（【P0-FIX-2B-A】t_db82c38e · A5）。

在**隔离实例**上验证三件事（全部走真实 HTTP + 真实库）：

  ① 部门隔离：TECH / FIN 各 1 人 + 各 1 张任务，互相看不到对方的任务；
  ② 反向对照（因果）：把某人的 dept_id 改到别部门 → 立刻看得到别部门的任务、看不到原部门的
     —— 证明「可见性确实由 dept_id 驱动」，落位落错会**立刻**体现为越权/失权；
  ③ 建单快照：`biz_task.dept_id` = 负责人所在部门（验收判据 A5 的第三句）。

部门用真实 `dept_code`（TECH / FIN）解析 uuid，**不硬编码 uuid**。

用法（隔离实例，db_guard 会拒绝生产库）：
    LANDBASE=http://127.0.0.1:18095 LAND_DB_NAME=cloudpivot_orgland \\
    LAND_DB_USER=alun LAND_DB_PASSWORD=alun python3 scripts/verify/org_landing_e2e.py

退出码：0 全过 / 1 有失败 / 3 打到生产库被守卫拒绝。
"""

import os
import sys

_os, _sys = os, sys          # db_guard 的 require_non_prod 用 _os/_sys 命名，保持一份别名

_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402

_require_non_prod(script=_os.path.basename(__file__))

import json  # noqa: E402
import subprocess  # noqa: E402
import urllib.error  # noqa: E402
import urllib.request  # noqa: E402

BASE = os.environ.get("LANDBASE", "http://127.0.0.1:18095")
DB = dict(
    host=os.environ.get("LAND_DB_HOST", "127.0.0.1"),
    port=os.environ.get("LAND_DB_PORT", "5432"),
    user=os.environ.get("LAND_DB_USER", "alun"),
    name=os.environ.get("LAND_DB_NAME", "cloudpivot_orgland"),
    password=os.environ.get("LAND_DB_PASSWORD", "alun"),
)
ADMIN_USER = os.environ.get("LAND_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("LAND_ADMIN_PASS", "Admin@123456")
NEW_PASS = os.environ.get("LAND_NEW_PASS", "Land@2026!")

USER_TECH, USER_FIN = "u_tech_land", "u_fin_land"
TASK_TECH = "LAND-TECH 技术部任务（本部门可见）"
TASK_FIN = "LAND-FIN 财务部任务（他部门不可见）"
ROLE_CODE = "DEPT_LEADER"   # data_scope=3（本部门及以下），部门级范围的代表

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


def call(method, path, body=None, token=None):
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
    for candidate in dict.fromkeys([password, ADMIN_PASS, NEW_PASS]):
        status, body = call("POST", "/api/system/auth/login",
                            {"username": username, "password": candidate})
        token = (body.get("data") or {}).get("access_token") or ""
        if not token:
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
    raise SystemExit(f"[FATAL] {username} 登录失败（口令未知：请先在隔离库重置）")


def dept_payload(code):
    """读一个部门的当前值，拼成 PUT 需要的载荷（原样写回 = 只触发「部门写」的副作用）。"""
    row = psql("select dept_code, dept_name, coalesce(parent_id::text,''), coalesce(phone,''), "
               "coalesce(email,''), status, sort_order "
               f"from sys_dept where dept_code='{code}' and deleted_at is null").split("|")
    return dict(dept_code=row[0], dept_name=row[1], parent_id=(row[2] or None),
                phone=row[3], email=row[4], status=int(row[5]), sort_order=int(row[6]))


def bump_scope_epoch(token, code="TECH"):
    """触发范围缓存失效（`system/dept/handler.rs` 在部门写后 `scope_guard::bump_epoch()`）。

    我们改的是 `sys_user.dept_id`（直改库），生产里改人的部门走同一套 handler；
    直改库不会自动 bump epoch，所以这里**照生产机制显式 bump 一次** ——
    这也正是 A6 要求 `INCR perm:user_scope_epoch` 的原因。
    """
    dept_id = one(f"select id from sys_dept where dept_code='{code}' and deleted_at is null")
    st, body = call("PUT", f"/api/system/dept/{dept_id}", dept_payload(code), token)
    if st != 200:
        raise SystemExit(f"[FATAL] 触发范围缓存失效失败（PUT dept {code}）：{st} {body}")


def cleanup():
    for stmt in [
        f"DELETE FROM biz_task WHERE title LIKE 'LAND-%'",
        "DELETE FROM sys_user_role WHERE user_id IN "
        f"(SELECT id FROM sys_user WHERE username IN ('{USER_TECH}','{USER_FIN}'))",
        f"DELETE FROM sys_user WHERE username IN ('{USER_TECH}','{USER_FIN}')",
    ]:
        psql(stmt)


def ensure_user(token, username, dept_id):
    existed = one(f"select id from sys_user where username = '{username}' and deleted_at is null")
    if existed:
        return existed
    rid = one(f"select id from sys_role where role_code = '{ROLE_CODE}' and deleted_at is null")
    if not rid:
        raise SystemExit(f"[FATAL] 角色 {ROLE_CODE} 不存在")
    st, body = call("POST", "/api/system/user",
                    {"username": username, "password": NEW_PASS, "real_name": username,
                     "dept_id": dept_id, "status": 1, "role_ids": [rid]}, token)
    if st not in (200, 201):
        raise SystemExit(f"[FATAL] 建用户 {username} 失败：{st} {body}")
    return (body.get("data") or {})["id"]


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
    print(f"【P0-FIX-2B-A】组织落位真机验收 —— {BASE} / 库 {DB['name']}")
    print("=" * 92)
    cleanup()

    tech_id = one("select id from sys_dept where dept_code='TECH' and deleted_at is null")
    fin_id = one("select id from sys_dept where dept_code='FIN' and deleted_at is null")
    check(bool(tech_id) and bool(fin_id), "① 真实部门 TECH / FIN 按 dept_code 解析到 uuid",
          f"TECH={tech_id or '缺失'} FIN={fin_id or '缺失'}")
    if not tech_id or not fin_id:
        print("\n结果：部门树不全，无法继续（先确认隔离库是 prod 副本）")
        return 1

    admin_token, pass_ = login(ADMIN_USER, ADMIN_PASS)
    print(f"[PASS] admin 登录成功（口令 {pass_}）")

    tech_uid = ensure_user(admin_token, USER_TECH, tech_id)
    fin_uid = ensure_user(admin_token, USER_FIN, fin_id)
    check(bool(tech_uid) and bool(fin_uid), "① TECH / FIN 各 1 个人就绪")

    t_tech = create_task(admin_token, TASK_TECH, tech_uid)
    t_fin = create_task(admin_token, TASK_FIN, fin_uid)

    # ③ 建单快照：biz_task.dept_id = 负责人所在部门
    snap_tech = one(f"select coalesce(d.dept_code,'NULL') from biz_task t join sys_dept d on d.id=t.dept_id "
                    f"where t.id='{t_tech}'")
    snap_fin = one(f"select coalesce(d.dept_code,'NULL') from biz_task t join sys_dept d on d.id=t.dept_id "
                   f"where t.id='{t_fin}'")
    check(snap_tech == "TECH" and snap_fin == "FIN",
          "③ 建单 biz_task.dept_id = 负责人所在部门（A5 第三句）",
          f"TECH 任务快照={snap_tech}（期望 TECH）、FIN 任务快照={snap_fin}（期望 FIN）")

    tok_tech, _ = login(USER_TECH, NEW_PASS)
    tok_fin, _ = login(USER_FIN, NEW_PASS)

    # ① 隔离
    tech_sees = visible_titles(tok_tech)
    fin_sees = visible_titles(tok_fin)
    ok1 = TASK_TECH in tech_sees and TASK_FIN not in tech_sees
    ok2 = TASK_FIN in fin_sees and TASK_TECH not in fin_sees
    check(ok1, "① TECH 账号只看到本部门任务（看不到 FIN 的）", f"看到：{tech_sees}")
    check(ok2, "① FIN 账号只看到本部门任务（看不到 TECH 的）", f"看到：{fin_sees}")

    # ② 反向对照（因果）：把 TECH 这个人的 dept_id 改到 FIN → 立刻看得到 FIN 的任务
    #    注意：他自己那张 TECH 任务**仍可见**是正确的 —— 任务可见性是「范围 ∪ 参与关系」
    #    （`task_model_base.rs::VISIBLE_OR_TAIL`），他是那张单的负责人，属并集里的第二支。
    #    因果判据因此落在「原先看不到的 FIN 任务，现在看得到了」这一句上。
    psql(f"update sys_user set dept_id='{fin_id}' where id='{tech_uid}'")
    bump_scope_epoch(admin_token)
    tok_tech2, _ = login(USER_TECH, NEW_PASS)
    after_move = visible_titles(tok_tech2)
    ok3 = TASK_FIN in after_move
    check(ok3, "② 反向对照：dept_id 改到 FIN + 范围缓存失效 → 原先看不到的 FIN 任务立刻可见"
               "（自己负责的 TECH 单仍可见，属「范围 ∪ 参与关系」并集语义）",
          f"看到：{after_move}")

    # 还原
    psql(f"update sys_user set dept_id='{tech_id}' where id='{tech_uid}'")
    bump_scope_epoch(admin_token)
    tok_tech3, _ = login(USER_TECH, NEW_PASS)
    after_back = visible_titles(tok_tech3)
    ok4 = TASK_TECH in after_back and TASK_FIN not in after_back
    check(ok4, "② 还原 dept_id=TECH → 又只看得到 TECH 的任务（因果双向成立）",
          f"看到：{after_back}")

    cleanup()
    left = psql("select count(*) from biz_task where title like 'LAND-%'") + "/" + \
        psql(f"select count(*) from sys_user where username in ('{USER_TECH}','{USER_FIN}')")
    check(left == "0/0", "④ 夹具已清理（任务 0 / 账号 0）", f"残留 {left}")

    print()
    print("=" * 92)
    print(f"结果：{len(PASSED)}/{len(PASSED) + len(FAILED)} 通过")
    print("=" * 92)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
