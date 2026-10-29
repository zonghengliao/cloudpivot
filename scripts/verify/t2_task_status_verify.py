#!/usr/bin/env python3
"""T2 接口走查（真服务 + 真库 + 真登录）：逐条打印证据。

用法（在 172.18.10.144 上，验证库 `cloudpivot_t2v` 已迁到 V0108）：

    set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
    export T2_BASE=http://127.0.0.1:18091/api
    python3 scripts/verify/t2_task_status_verify.py

前置：服务已起（本脚本**不**起停服务，方便对照日志），夹具已灌
（`t2_task_status_fixture.sql`）。

为什么用 Python 而不是 T1 的 bash+curl：本卡有 40+ 条断言要同时校验
「HTTP 码 + 业务码 + 落库值（psql）+ 评论 / 变更日志 / 通知行」，
bash 里四层引号嵌套（curl → jq → psql -c）几乎不可维护。

退出码：0 = 全部 PASS；1 = 有 FAIL。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("T2_BASE", "http://127.0.0.1:18091/api")
DB = os.environ.get("T2_DB", "cloudpivot_t2v")
PW = os.environ.get("ALUN_ADMIN_PASSWORD", "")
PG = {
    "host": os.environ.get("ALUN_DATABASE_HOST", "127.0.0.1"),
    "port": os.environ.get("ALUN_DATABASE_PORT", "5432"),
    "user": os.environ.get("ALUN_DATABASE_USER", "cloudpivot"),
    "password": os.environ.get("ALUN_DATABASE_PASSWORD", ""),
}

FAIL = 0
PASSED = 0


# ==================== 基础工具 ====================


def hdr(text: str) -> None:
    print()
    print("=" * 78)
    print(text)
    print("=" * 78)


def check(label: str, expected, actual) -> bool:
    global FAIL, PASSED
    ok = expected == actual
    if ok:
        PASSED += 1
        print(f"  PASS  {label}：{actual}")
    else:
        FAIL += 1
        print(f"  FAIL  {label}：期望 {expected}，实际 {actual}")
    return ok


def req(method: str, path: str, token: str | None = None, body: dict | None = None):
    """返回 (HTTP 状态码, 解析后的 JSON dict)。"""
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def psql(sql: str) -> str:
    env = dict(os.environ)
    env["PGPASSWORD"] = PG["password"]
    command = [
        "psql",
        "-h", PG["host"],
        "-p", PG["port"],
        "-U", PG["user"],
        "-d", DB,
        "-Atc",
        sql,
    ]
    result = subprocess.run(command, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        print("  [psql 失败] " + result.stderr.strip())
        return ""
    return result.stdout.strip()


def login(username: str) -> str:
    status, payload = req(
        "POST", "/system/auth/login", None, {"username": username, "password": PW}
    )
    if status != 200:
        print(f"  登录失败 {username}: {status} {payload}")
        return ""
    return payload["data"]["access_token"]


def code_of(payload: dict):
    return payload.get("code")


def data_of(payload: dict):
    return payload.get("data") or {}


# ==================== 业务动作 ====================


def create_task(token: str, title: str, owner_id: str | None = None, **extra):
    body = {"title": title, **extra}
    if owner_id:
        body["owner_id"] = owner_id
    status, payload = req("POST", "/business/task", token, body)
    if status != 200:
        raise SystemExit(f"建任务失败：{status} {payload}")
    return payload["data"]["id"]


def patch_status(token: str, task_id: str, to: str, reason: str | None = None):
    body = {"to": to}
    if reason is not None:
        body["reason"] = reason
    return req("PATCH", f"/business/task/{task_id}/status", token, body)


def patch_progress(token: str, task_id: str, progress: int):
    return req("PATCH", f"/business/task/{task_id}/progress", token, {"progress": progress})


def detail(token: str, task_id: str):
    status, payload = req("GET", f"/business/task/{task_id}", token)
    if status != 200:
        raise SystemExit(f"取详情失败：{status} {payload}")
    return payload["data"]


def subtask(token: str, parent_id: str, title: str, hours: float | None = None):
    body = {"title": title}
    if hours is not None:
        body["estimate_hours"] = hours
    status, payload = req("POST", f"/business/task/{parent_id}/subtask", token, body)
    if status != 200:
        raise SystemExit(f"建子任务失败：{status} {payload}")
    return payload["data"]["id"]


def user_id(name: str) -> str:
    return psql(f"SELECT id FROM sys_user WHERE username = '{name}' AND deleted_at IS NULL")


# ==================== 走查 ====================


def main() -> int:
    hdr("0) 登录（admin + 4 个夹具账号，密码哈希与 admin 相同）")
    tok_admin = login("admin")
    tok_owner = login("t2_owner")
    tok_maker = login("t2_maker")
    tok_other = login("t2_other")
    tok_collab = login("t2_collab")
    print(
        f"  admin/owner/maker/other/collab token 长度 = {len(tok_admin)} / {len(tok_owner)} / "
        f"{len(tok_maker)} / {len(tok_other)} / {len(tok_collab)}"
    )
    if not all([tok_admin, tok_owner, tok_maker, tok_other, tok_collab]):
        print("  登录不全，停止")
        return 1
    owner_id = user_id("t2_owner")
    maker_id = user_id("t2_maker")
    collab_id = user_id("t2_collab")
    print(f"  t2_owner={owner_id}  t2_maker={maker_id}  t2_collab={collab_id}")

    # ---------------- 1. 合法流转全走一遍 ----------------
    hdr("1) 合法流转全走一遍：TODO→DOING→REVIEW→DONE（need_review=true）")
    t1 = create_task(tok_maker, "T2走查-合法链", owner_id, need_review=True)
    print(f"  task={t1}  creator=t2_maker  owner=t2_owner  need_review=true")
    d = detail(tok_maker, t1)
    check("初始状态", "TODO", d["status"])
    check("初始进度", 0, d["progress"])

    status, payload = patch_status(tok_owner, t1, "DOING")
    check("TODO→DOING 由负责人做 HTTP", 200, status)
    check("  落库状态", "DOING", data_of(payload).get("status"))
    check("  进度联动 max(0,10)", 10, data_of(payload).get("progress"))

    status, payload = patch_status(tok_owner, t1, "REVIEW")
    check("DOING→REVIEW 由负责人做 HTTP", 200, status)
    check("  落库状态", "REVIEW", data_of(payload).get("status"))
    check("  进度拉满", 100, data_of(payload).get("progress"))

    status, payload = patch_status(tok_maker, t1, "DONE")
    check("REVIEW→DONE 由创建人（验收人）做 HTTP", 200, status)
    check("  落库状态", "DONE", data_of(payload).get("status"))
    check("  done_at 有值", True, bool(data_of(payload).get("done_at")))

    comments = psql(
        "SELECT coalesce(string_agg(comment_type || '|' || content, E'\\n' ORDER BY created_at), '') "
        f"FROM biz_task_comment WHERE task_id = '{t1}'"
    )
    print("  评论（biz_task_comment，按时间）：")
    for line in comments.splitlines():
        print("    " + line)
    check("STATUS_CHANGE 评论条数", 3, int(psql(
        f"SELECT count(*) FROM biz_task_comment WHERE task_id = '{t1}' AND comment_type = 'STATUS_CHANGE'"
    )))
    check(
        "每条评论都带 from→to",
        True,
        all("→" in line for line in comments.splitlines() if line.startswith("STATUS_CHANGE")),
    )

    logs = psql(
        "SELECT action || '|' || array_to_string(changed_fields, ',') || '|' || diff::text "
        f"FROM sys_change_log WHERE resource = 'biz_task' AND resource_id = '{t1}' ORDER BY created_at"
    )
    print("  变更日志（sys_change_log）：")
    for line in logs.splitlines():
        print("    " + line[:200])
    check("变更日志条数", 3, int(psql(
        f"SELECT count(*) FROM sys_change_log WHERE resource = 'biz_task' AND resource_id = '{t1}'"
    )))
    check("变更日志动作", "STATUS", psql(
        f"SELECT DISTINCT action FROM sys_change_log WHERE resource = 'biz_task' AND resource_id = '{t1}'"
    ))
    check(
        "变更日志 diff 里有 status 字段",
        True,
        "status" in psql(
            f"SELECT diff::text FROM sys_change_log WHERE resource = 'biz_task' "
            f"AND resource_id = '{t1}' ORDER BY created_at LIMIT 1"
        ),
    )
    check(
        "变更日志 diff 里 from 是 TODO、to 是 DOING",
        True,
        '"TODO"' in psql(
            f"SELECT diff::text FROM sys_change_log WHERE resource = 'biz_task' "
            f"AND resource_id = '{t1}' ORDER BY created_at LIMIT 1"
        ),
    )

    # ---------------- 2. 非法流转全被拒 ----------------
    hdr("2) 非法流转全被拒（跳级 / 终态直取 / 同状态 / 无验收权）")
    t2 = create_task(tok_maker, "T2走查-跳级", owner_id)
    status, payload = patch_status(tok_owner, t2, "DONE")
    check("TODO→DONE（跳级）HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))
    print(f"    文案：{payload.get('msg')}")

    status, payload = patch_status(tok_maker, t1, "CANCELLED")
    check("DONE→CANCELLED（终态直取）HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))

    t3 = create_task(tok_maker, "T2走查-验收权", owner_id, need_review=True)
    patch_status(tok_owner, t3, "DOING")
    patch_status(tok_owner, t3, "REVIEW")
    status, payload = patch_status(tok_owner, t3, "DONE")
    check("REVIEW→DONE 由负责人自己做（无验收权）HTTP", 403, status)
    check("  业务码", 63011, code_of(payload))
    print(f"    文案：{payload.get('msg')}")

    status, payload = patch_status(tok_owner, t3, "REVIEW")
    check("REVIEW→REVIEW（同状态）HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))

    status, payload = patch_status(tok_maker, t3, "TODO")
    check("REVIEW→TODO（回退到待开始）HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))

    # ---------------- 3. 打回必须填原因 ----------------
    hdr("3) 打回（REVIEW→DOING）必须填原因且 ≥5 字")
    status, payload = patch_status(tok_maker, t3, "DOING")
    check("不填 reason HTTP", 400, status)
    check("  业务码", 63013, code_of(payload))
    print(f"    文案：{payload.get('msg')}")
    status, payload = patch_status(tok_maker, t3, "DOING", "不行")
    check("reason 只有 2 个字 HTTP", 400, status)
    check("  业务码", 63013, code_of(payload))
    print(f"    文案：{payload.get('msg')}")
    reason = "测试没跑通，返工补单测"
    status, payload = patch_status(tok_maker, t3, "DOING", reason)
    check("reason 合法（≥5 字）HTTP", 200, status)
    check("  落库状态", "DOING", data_of(payload).get("status"))
    check("  打回后进度归 10", 10, data_of(payload).get("progress"))
    check("  reject_reason 落库", reason, psql(
        f"SELECT coalesce(reject_reason, '') FROM biz_task WHERE id = '{t3}'"
    ))
    check("  reason 进评论", True, reason in psql(
        f"SELECT coalesce(string_agg(content, ' '), '') FROM biz_task_comment WHERE task_id = '{t3}'"
    ))

    # ---------------- 4. need_review=false 可直接 DONE ----------------
    hdr("4) need_review=false 时 DOING→DONE 允许；true 时被拒")
    t4 = create_task(tok_maker, "T2走查-免验收", owner_id, need_review=False)
    patch_status(tok_owner, t4, "DOING")
    status, payload = patch_status(tok_owner, t4, "DONE")
    check("need_review=false：DOING→DONE HTTP", 200, status)
    check("  落库状态", "DONE", data_of(payload).get("status"))

    t5 = create_task(tok_maker, "T2走查-需验收", owner_id, need_review=True)
    patch_status(tok_owner, t5, "DOING")
    status, payload = patch_status(tok_owner, t5, "DONE")
    check("need_review=true：DOING→DONE HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))
    print(f"    文案：{payload.get('msg')}")
    status, payload = patch_status(tok_owner, t5, "REVIEW")
    check("need_review=true：DOING→REVIEW HTTP", 200, status)

    # ---------------- 5. done_at / 重开 ----------------
    hdr("5) DONE 后 done_at 有值；重开（DONE→DOING）后清空")
    check("DONE 后 done_at 非空", "true", psql(
        f"SELECT (done_at IS NOT NULL)::text FROM biz_task WHERE id = '{t4}'"
    ))
    status, payload = patch_status(tok_maker, t4, "DOING")
    check("DONE→DOING 由创建人重开 HTTP", 200, status)
    check("  重开后 done_at 清空", "false", psql(
        f"SELECT (done_at IS NOT NULL)::text FROM biz_task WHERE id = '{t4}'"
    ))
    check("  重开后 progress 归 10", 10, data_of(payload).get("progress"))

    # 负责人不能自己重开（DONE→DOING 只认创建人 / 管理员）
    t4b = create_task(tok_maker, "T2走查-重开权限", owner_id, need_review=False)
    patch_status(tok_owner, t4b, "DOING")
    patch_status(tok_owner, t4b, "DONE")
    status, payload = patch_status(tok_owner, t4b, "DOING")
    check("DONE→DOING 由负责人自己做（负责人不在重开白名单）HTTP", 403, status)
    check("  业务码", 63011, code_of(payload))

    # 同状态自环：确切的 409 + 63010
    status, payload = patch_status(tok_owner, t4, "DOING")
    check("DOING→DOING（同状态）HTTP", 409, status)
    check("  业务码", 63010, code_of(payload))

    # ---------------- 6. 无子任务：手动进度 + 状态联动 ----------------
    hdr("6) 无子任务：手动 0→50→100；DOING 时设 5 → 自动 10")
    t6 = create_task(tok_maker, "T2走查-手动进度", owner_id)
    for value in (0, 50, 100):
        status, payload = patch_progress(tok_owner, t6, value)
        check(f"手动设 {value} HTTP", 200, status)
        check(f"  回执 progress", value, data_of(payload).get("progress"))
    t7 = create_task(tok_maker, "T2走查-联动", owner_id)
    patch_status(tok_owner, t7, "DOING")
    status, payload = patch_progress(tok_owner, t7, 5)
    check("DOING 时设 5 HTTP", 200, status)
    check("  实际落库（联动抬到 10）", 10, data_of(payload).get("progress"))
    check("  回执原值 requested", 5, data_of(payload).get("requested"))
    status, payload = patch_progress(tok_owner, t7, 101)
    check("进度 101（越界）HTTP", 400, status)
    check("  业务码", 10001, code_of(payload))

    # ---------------- 7. 有子任务：工时加权 80% ----------------
    hdr("7) 有子任务：1h/2h/7h，进度 0/50/100 → 期望 (1×0 + 2×50 + 7×100)/10 = 80")
    p7 = create_task(tok_maker, "T2走查-加权父")
    c7a = subtask(tok_maker, p7, "T2走查-加权子A", 1)
    c7b = subtask(tok_maker, p7, "T2走查-加权子B", 2)
    c7c = subtask(tok_maker, p7, "T2走查-加权子C", 7)
    print(f"  父={p7}  子={c7a}(1h) {c7b}(2h) {c7c}(7h)")
    patch_progress(tok_maker, c7b, 50)
    status, payload = patch_progress(tok_maker, c7c, 100)
    check("子任务设进度 HTTP", 200, status)
    parent_progress = psql(f"SELECT progress FROM biz_task WHERE id = '{p7}'")
    check("父任务汇总值", "80", parent_progress)
    print("  计算：(1×0 + 2×50 + 7×100) / 10 = 800 / 10 = 80.0 → 80")
    print("  父任务重算评论：")
    for line in psql(
        f"SELECT content FROM biz_task_comment WHERE task_id = '{p7}' AND comment_type = 'PROGRESS_AUTO' ORDER BY created_at"
    ).splitlines():
        print("    " + line)

    # ---------------- 8. 工时全空 → 等权平均 ----------------
    hdr("8) 工时全空 → 等权平均：(0 + 50 + 100) / 3 = 50")
    p8 = create_task(tok_maker, "T2走查-等权父")
    c8a = subtask(tok_maker, p8, "T2走查-等权子A")
    c8b = subtask(tok_maker, p8, "T2走查-等权子B")
    c8c = subtask(tok_maker, p8, "T2走查-等权子C")
    patch_progress(tok_maker, c8b, 50)
    patch_progress(tok_maker, c8c, 100)
    check("父任务等权汇总值", "50", psql(f"SELECT progress FROM biz_task WHERE id = '{p8}'"))
    check("子任务进度 0 的仍是 0", "0", psql(f"SELECT progress FROM biz_task WHERE id = '{c8a}'"))

    # ---------------- 9. 父任务手改进度 → 409 ----------------
    hdr("9) 父任务手改进度 → 409 + 63012（进度由子任务汇总）")
    status, payload = patch_progress(tok_maker, p7, 10)
    check("HTTP", 409, status)
    check("业务码", 63012, code_of(payload))
    print(f"  文案：{payload.get('msg')}")
    check("父任务进度未被改动", "80", psql(f"SELECT progress FROM biz_task WHERE id = '{p7}'"))

    # ---------------- 10. 子任务全 DONE → 父 100，状态不变 ----------------
    hdr("10) 子任务全 DONE → 父 progress=100 但 status 不变（架构 D5）")
    p10 = create_task(tok_maker, "T2走查-全完成父")
    d10a = subtask(tok_maker, p10, "T2走查-全完成子A", 1)
    d10b = subtask(tok_maker, p10, "T2走查-全完成子B", 3)
    for child in (d10a, d10b):
        patch_status(tok_maker, child, "DOING")
        status, _ = patch_status(tok_maker, child, "DONE")
        check(f"子任务 {child[:8]} 推到 DONE", 200, status)
    row = psql(f"SELECT progress || '|' || status FROM biz_task WHERE id = '{p10}'")
    check("父任务 progress|status", "100|TODO", row)

    # ---------------- 11. 三级嵌套 A→B→C ----------------
    hdr("11) 三级嵌套 A→B→C：C 完成 → B 重算 100 → A 重算 50")
    a_id = "7e000000-0000-0000-0000-000000000001"
    b_id = "7e000000-0000-0000-0000-000000000002"
    c_id = "7e000000-0000-0000-0000-000000000003"
    print("  结构（夹具 SQL 造，业务 API 限 2 层）：A(2h 子B + 2h 子D) → B → C")
    print("  完成前：" + psql(
        f"SELECT string_agg(task_no || '=' || progress || '%/' || status, '  ' ORDER BY task_no) "
        f"FROM biz_task WHERE id IN ('{a_id}','{b_id}','{c_id}')"
    ))
    status, payload = patch_status(tok_maker, c_id, "DOING")
    check("C：TODO→DOING HTTP", 200, status)
    check("  C 的父任务（B）被重算", b_id, data_of(payload).get("parent_progress", [{}])[0].get("id"))
    status, payload = patch_status(tok_maker, c_id, "DONE")
    check("C：DOING→DONE HTTP", 200, status)
    chain = data_of(payload).get("parent_progress") or []
    print("  本次重算链（由近及远）：")
    for item in chain:
        print(
            f"    {item['id']}  {item['previous']}% → {item['progress']}%"
            f"（{item['mode']}，all_done={item['all_done']}）"
        )
    check("B 重算到 100", "100", psql(f"SELECT progress FROM biz_task WHERE id = '{b_id}'"))
    check("A 重算到 50（(2×100 + 2×0)/4）", "50", psql(f"SELECT progress FROM biz_task WHERE id = '{a_id}'"))
    check("A 的状态没有自动变（仍是 DOING）", "DOING", psql(f"SELECT status FROM biz_task WHERE id = '{a_id}'"))

    # ---------------- 12. 可见性仍然生效 ----------------
    hdr("12) 无关用户改状态 → 404 + 63001（不暴露「存在但无权」）")
    t12 = create_task(tok_maker, "T2走查-可见性", owner_id)
    status, payload = patch_status(tok_other, t12, "DOING")
    check("HTTP", 404, status)
    check("业务码", 63001, code_of(payload))
    print(f"  文案：{payload.get('msg')}")
    status, payload = patch_progress(tok_other, t12, 50)
    check("无关用户改进度 HTTP", 404, status)
    check("业务码", 63001, code_of(payload))

    # ---------------- 13. 跨部门协作人 ----------------
    hdr("13) 跨部门协作人：看得见（200）但推不动（403）")
    psql(
        "INSERT INTO biz_task_member (task_id, user_id, role, created_at) VALUES "
        f"('{t12}', '{collab_id}', 'COLLABORATOR', now()) ON CONFLICT DO NOTHING"
    )
    status, payload = req("GET", f"/business/task/{t12}", tok_collab)
    check("协作人看详情 HTTP", 200, status)
    check("  协作人能看见", t12, data_of(payload).get("id"))
    status, payload = patch_status(tok_collab, t12, "DOING")
    check("协作人改状态 HTTP", 403, status)
    check("  业务码（状态流转专用码）", 63011, code_of(payload))
    print(f"    文案：{payload.get('msg')}")
    status, payload = patch_progress(tok_collab, t12, 50)
    check("协作人改进度 HTTP", 403, status)
    check("  业务码（T1 的编辑类无权限码）", 63002, code_of(payload))
    status, payload = req("PUT", f"/business/task/{t12}", tok_collab, {"title": "改标题"})
    check("协作人编辑任务 HTTP（T1 口径）", 403, status)
    check("  业务码", 63002, code_of(payload))
    # 负责人推一次普通状态变更 → 用来验证「尊重 notify.inbox」的那一档
    status, _ = patch_status(tok_owner, t12, "DOING")
    check("负责人把 t12 推到 DOING（普通状态变更）HTTP", 200, status)

    # ---------------- 14. 子任务接口 + 权限 ----------------
    hdr("14) 子任务接口：建 / 列 / 继承 / 深度限制")
    p14 = create_task(tok_maker, "T2走查-子任务接口", priority="P1", due_date="2026-12-31", need_review=True)
    child = subtask(tok_maker, p14, "T2走查-继承子", 4)
    child_detail = detail(tok_maker, child)
    check("子任务继承父任务优先级", "P1", child_detail["priority"])
    check("子任务继承父任务截止日", "2026-12-31", child_detail["due_date"])
    check("子任务继承 need_review", True, child_detail["need_review"])
    check("子任务父指向正确", p14, child_detail["parent_id"])
    status, payload = req("GET", f"/business/task/{p14}/subtasks", tok_maker)
    check("子任务列表 HTTP", 200, status)
    listing = data_of(payload)
    check("  列表条数", 1, len(listing.get("list") or []))
    check("  进度已锁", True, listing.get("progress_locked"))
    check("  父任务进度 = 子任务（唯一 0h→等权）", 0, listing.get("progress"))
    # 第 3 层：父任务已是子任务 → 拒绝（T1 的 2 层限制）
    status, payload = req("POST", f"/business/task/{child}/subtask", tok_maker, {"title": "T2走查-第四层"})
    check("给子任务再加子任务（第 3 层）HTTP", 400, status)
    check("  业务码", 63005, code_of(payload))
    print(f"    文案：{payload.get('msg')}")
    # 终态父任务不能加子任务
    patch_status(tok_maker, p14, "DOING")
    check("父任务推进到 DOING（有子任务时进度不动）", "0", psql(f"SELECT progress FROM biz_task WHERE id = '{p14}'"))

    # ---------------- 15. 通知（修正 8 分级） ----------------
    hdr("15) 通知分级：待验收 / 被打回（强制）；其它状态变更（尊重偏好）")
    for task_id, label in ((t3, "被打回 t3"), (t5, "需验收 t5"), (t12, "普通变更 t12")):
        rows = psql(
            "SELECT n.title || ' → ' || r.user_id FROM sys_notice n "
            "JOIN sys_notice_receiver r ON r.notice_id = n.id "
            f"WHERE n.biz_id = '{task_id}' ORDER BY n.created_at"
        )
        print(f"  {label}（{task_id[:8]}）通知：")
        if not rows:
            print("    （无）")
        for line in rows.splitlines():
            print("    " + line.strip())
    check("被打回通知到负责人", True, owner_id in psql(
        "SELECT string_agg(r.user_id::text, ',') FROM sys_notice n "
        "JOIN sys_notice_receiver r ON r.notice_id = n.id "
        f"WHERE n.biz_id = '{t3}' AND n.title = '任务被打回'"
    ))
    check("待验收通知到创建人", True, maker_id in psql(
        "SELECT string_agg(r.user_id::text, ',') FROM sys_notice n "
        "JOIN sys_notice_receiver r ON r.notice_id = n.id "
        f"WHERE n.biz_id = '{t5}' AND n.title = '任务待你验收'"
    ))

    # ---------------- 16. status_semantic 保持 NULL ----------------
    hdr("16) P1 扩展位：status_semantic 全程保持 NULL")
    check("有值的行数（应为 0）", "0", psql(
        "SELECT count(*) FROM biz_task WHERE status_semantic IS NOT NULL"
    ))
    check("comment_type 分布", True, psql(
        "SELECT count(*) > 0 FROM biz_task_comment WHERE comment_type = 'PROGRESS_AUTO'"
    ) == "t")

    # ---------------- 汇总 ----------------
    hdr("汇总")
    print(f"  PASS = {PASSED}   FAIL = {FAIL}")
    if FAIL:
        print("  有断言失败 —— 见上面 FAIL 行")
        return 1
    print("  全部断言通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
