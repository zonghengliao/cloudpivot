#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""D2 真机验收：跨单据联动白名单（第 1 步只读 + 第 2 步 SEND_NOTICE 接管）。

跑法（隔离实例已在跑）：

    bash scripts/verify/d2_instance.sh start
    python3 scripts/verify/d2_linkage_e2e.py

它要证明的事（每条都有 `[PASS]/[FAIL]` 与原始输出）：

  A. 迁移到位：biz_flow_linkage_rule 存在、CHECK 生效、max(version)=205；
  B. **没有规则时行为逐字不变**：任务 TODO→DOING 仍然只发 1 条旧逻辑通知；
  C. 影子模式（SHADOW）：仍然 1 条通知（旧逻辑发），但日志里出现
     「联动影子对比」且 matched=true —— 「并行跑一段、对比一致」的机器证据；
  D. 接管模式（TAKEOVER）：只有 1 条通知，且日志写明「已接管 / 旧逻辑本次不发」
     —— 证明通知确实由配置驱动发出、没有双发；
  E. 接管发出来的通知与旧逻辑那条**内容模板一致**（把任务名/编号替换后逐字相同）；
  F. 配置被写脏（参数非法）时**退回旧逻辑**：通知照发、流程不中断、日志有 ERROR；
  G. 收尾：删掉规则后表回到 0 行（配置可删可复活）。

退出码：0 = 全 PASS；1 = 有 FAIL。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))

import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("D2_BASE", "http://127.0.0.1:18095")
DB = os.environ.get("D2_DB", "cloudpivot_d2")
LOG = os.environ.get("D2_LOG", "/root/d2-e2e-server.log")
PW_FILE = "/root/d2-admin-password.txt"
PREFIX = "D2E2E-"

PASS = 0
FAIL = 0
TOKEN = ""


def ok(label, detail=""):
    global PASS
    PASS += 1
    print(f"[PASS] {label}" + (f"  —— {detail}" if detail else ""))


def bad(label, detail=""):
    global FAIL
    FAIL += 1
    print(f"[FAIL] {label}" + (f"  —— {detail}" if detail else ""))


def expect(cond, label, detail=""):
    if cond:
        ok(label, detail)
    else:
        bad(label, detail)


def psql(sql):
    """用应用账号直连（su - postgres 读不到 /root 下的文件，且 owner 会错）。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = "alun"
    proc = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-At", "-c", sql],
        capture_output=True,
        text=True,
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"psql 失败：{proc.stderr.strip()}（SQL: {sql}）")
    return proc.stdout.strip()


def _json(text):
    """把响应体解析成 dict（非 JSON 时返回 {raw: …}，便于把错误原文带进断言）。"""
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw": text}


def http(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, _json(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, _json(err.read().decode())


def login():
    """登录（首登强制改密 + 登录失败计数都要兜住，重跑才不会 401）。"""
    candidates = []
    if os.environ.get("D2_ADMIN_PASSWORD"):
        candidates.append(os.environ["D2_ADMIN_PASSWORD"])
    if os.path.exists(PW_FILE):
        candidates.append(open(PW_FILE).read().strip())
    candidates += ["Admin@123456D2!", "Admin@123456"]
    psql("UPDATE sys_user SET login_fail_count = 0, is_locked = false WHERE username = 'admin'")

    for password in candidates:
        code, body = http("POST", "/api/system/auth/login", {"username": "admin", "password": password})
        if code != 200:
            continue
        data = body.get("data") or {}
        token = data.get("access_token")
        if not token:
            continue
        if data.get("must_change_password"):
            new_password = "Admin@123456D2!"
            code, body = http(
                "PUT",
                "/api/system/auth/password",
                {"old_password": password, "new_password": new_password},
                token=token,
            )
            if code != 200:
                raise RuntimeError(f"改密失败：{code} {body}")
            with open(PW_FILE, "w") as fh:
                fh.write(new_password)
            code, body = http(
                "POST", "/api/system/auth/login", {"username": "admin", "password": new_password}
            )
            data = body.get("data") or {}
            token = data.get("access_token")
            if not token:
                raise RuntimeError(f"改密后重登失败：{code} {body}")
        with open(PW_FILE, "w") as fh:
            fh.write(password if not data.get("must_change_password") else "Admin@123456D2!")
        return token
    raise RuntimeError("admin 登录失败（候选口令全部不通）")


def ensure_owner_user():
    """造一个「不是操作人」的负责人：任务通知会剔除操作人自己，
    只有负责人 ≠ 操作人时才会真的产生通知（这是既有行为，不是本卡引入的）。"""
    username = "d2e2e_owner"
    code, body = http(
        "POST",
        "/api/system/user",
        {"username": username, "password": "Owner@123456", "real_name": "D2 负责人", "status": 1},
        token=TOKEN,
    )
    if code in (200, 201):
        return (body.get("data") or {}).get("id") or psql(
            f"SELECT id FROM sys_user WHERE username = '{username}'"
        )
    if code == 409:
        return psql(f"SELECT id FROM sys_user WHERE username = '{username}'")
    raise RuntimeError(f"建负责人失败：{code} {body}")


def create_task(title, owner_id):
    body = {"title": title}
    if owner_id:
        body["owner_id"] = owner_id
    code, resp = http("POST", "/api/business/task", body, token=TOKEN)
    if code not in (200, 201):
        raise RuntimeError(f"建任务失败：{code} {resp}")
    data = resp.get("data") or {}
    return data["id"], data["task_no"]


def to_doing(task_id):
    code, body = http("PATCH", f"/api/business/task/{task_id}/status", {"status": "DOING"}, token=TOKEN)
    if code != 200:
        raise RuntimeError(f"改状态失败：{code} {body}")
    return body


def notice_of(task_id):
    """按 biz_id 取通知（sys_notice.biz_id 是 varchar）。"""
    rows = psql(
        "SELECT title || '~' || content || '~' || coalesce(biz_type, '') || '~' || receiver_count::text "
        f"FROM sys_notice WHERE biz_id = '{task_id}' AND deleted_at IS NULL"
    )
    if not rows:
        return []
    out = []
    for line in rows.splitlines():
        parts = line.split("~")
        out.append(
            {"title": parts[0], "content": parts[1], "biz_type": parts[2], "receivers": parts[3]}
        )
    return out


def upsert_rule(mode):
    """写规则（走 SQL 直插：配置 HTTP 入口不在本卡范围，卡面要求「能存能查」即可）。"""
    psql(
        "INSERT INTO biz_flow_linkage_rule "
        "(id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq, "
        " created_at, updated_at) "
        "VALUES (gen_random_uuid(), 'D2E2E-SEND-NOTICE', 'TASK', 'DOING', 'SEND_NOTICE', "
        f"'{{\"mode\": \"{mode}\"}}'::jsonb, true, 10, now(), now()) "
        "ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE "
        "SET action_params = EXCLUDED.action_params, enabled = true, seq = EXCLUDED.seq, "
        "    updated_at = now(), deleted_at = NULL"
    )


def drop_rule():
    psql("DELETE FROM biz_flow_linkage_rule WHERE name = 'D2E2E-SEND-NOTICE'")


def rule_rows():
    return psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE deleted_at IS NULL")


ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def log_tail_since(offset):
    """读日志增量。

    ⚠️ tracing 默认给日志加 ANSI 颜色，终端里肉眼看不出但**子串匹配会失败**
    （`matched=true` 实际是 `matched=\\x1b[32mtrue`）——必须先剥掉颜色码。
    """
    if not os.path.exists(LOG):
        return ""
    with open(LOG, "r", errors="replace") as fh:
        fh.seek(offset)
        return ANSI_RE.sub("", fh.read())


def log_size():
    return os.path.getsize(LOG) if os.path.exists(LOG) else 0


def norm_content(content, title, task_no):
    """把任务名/编号替换掉，用来比较「两条通知是不是同一套模板」。"""
    text = content.replace(title, "<TITLE>").replace(task_no, "<NO>")
    return re.sub(r"由 \S+ 从", "由 <OP> 从", text)


def main():
    global TOKEN

    print("== D2 跨单据联动白名单 · 真机验收 ==")
    # ---------- A. 迁移与表结构 ----------
    exists = psql(
        "SELECT count(*) FROM pg_tables WHERE tablename = 'biz_flow_linkage_rule'"
    )
    expect(exists == "1", "A1 表 biz_flow_linkage_rule 已建", f"pg_tables={exists}")
    version = psql("SELECT max(version::int) FROM schema_migrations")
    expect(int(version) >= 205, "A2 迁移已应用到 V0205", f"max(version)={version}")
    checks = psql(
        "SELECT string_agg(conname, ',' ORDER BY conname) FROM pg_constraint "
        "WHERE conrelid = 'biz_flow_linkage_rule'::regclass AND contype = 'c'"
    )
    for name in ("ck_flow_linkage_action_code", "ck_flow_linkage_no_sql_key", "ck_flow_linkage_params_object"):
        expect(name in checks, f"A3 约束存在：{name}", checks)
    expect(rule_rows() == "0", "A4 初始 0 条规则（空表 = 行为不变）", f"rules={rule_rows()}")

    TOKEN = login()
    ok("A5 admin 登录成功")
    owner_id = ensure_owner_user()
    expect(bool(owner_id), "A6 造出「非操作人」的负责人（任务通知会剔除操作人自己）", str(owner_id))

    drop_rule()
    # ---------- B. 没有规则：行为逐字不变 ----------
    t1_id, t1_no = create_task(PREFIX + "基线-无规则", owner_id)
    to_doing(t1_id)
    n1 = notice_of(t1_id)
    expect(len(n1) == 1, "B1 无规则时仍发 1 条旧逻辑通知", f"rows={len(n1)}")
    if n1:
        expect(n1[0]["title"] == "任务状态变更", "B2 标题 = 任务状态变更（旧逻辑文案）", n1[0]["title"])
        expect(n1[0]["biz_type"] == "task", "B3 biz_type = task", n1[0]["biz_type"])

    # ---------- C. 影子模式：并行对比，旧逻辑照发 ----------
    upsert_rule("SHADOW")
    expect(rule_rows() == "1", "C1 规则已写入（能存）")
    saved = psql(
        "SELECT trigger_entity || '/' || trigger_status || '/' || action_code || '/' || action_params::text "
        "FROM biz_flow_linkage_rule WHERE name = 'D2E2E-SEND-NOTICE'"
    )
    expect("TASK/DOING/SEND_NOTICE" in saved and "SHADOW" in saved, "C2 规则能读回来", saved)

    offset = log_size()
    t2_id, t2_no = create_task(PREFIX + "影子-对比", owner_id)
    to_doing(t2_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    n2 = notice_of(t2_id)
    expect(len(n2) == 1, "C3 影子模式仍只有 1 条通知（不双发）", f"rows={len(n2)}")
    expect("联动影子对比" in log, "C4 日志出现「联动影子对比」")
    expect("matched=true" in log, "C5 影子对比结果一致（matched=true）", _matched_line(log))

    # ---------- D. 接管模式：配置发、旧逻辑不发 ----------
    upsert_rule("TAKEOVER")
    offset = log_size()
    t3_id, t3_no = create_task(PREFIX + "接管-配置发", owner_id)
    to_doing(t3_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    n3 = notice_of(t3_id)
    expect(len(n3) == 1, "D1 接管后恰好 1 条通知（旧逻辑没有双发）", f"rows={len(n3)}")
    expect("联动已接管通知：配置发出" in log, "D2 日志写明「配置发出」")
    expect("任务状态通知已由联动配置接管" in log, "D3 日志写明旧逻辑本次不发")
    if n3 and n1:
        expect(n3[0]["title"] == n1[0]["title"], "D4 接管通知标题与旧逻辑一致", n3[0]["title"])
        same_template = norm_content(n3[0]["content"], PREFIX + "接管-配置发", t3_no) == norm_content(
            n1[0]["content"], PREFIX + "基线-无规则", t1_no
        )
        expect(same_template, "D5 接管通知正文与旧逻辑同一套模板", n3[0]["content"])
        expect(n3[0]["biz_type"] == n1[0]["biz_type"], "D6 biz_type 一致")
        expect(n3[0]["receivers"] == n1[0]["receivers"], "D7 接收人数一致", n3[0]["receivers"])

    # ---------- E. 配置写脏 → 退回旧逻辑（通知不丢） ----------
    psql(
        "UPDATE biz_flow_linkage_rule SET action_params = '{\"mode\": \"TAKEOVER_XXX\"}'::jsonb "
        "WHERE name = 'D2E2E-SEND-NOTICE'"
    )
    offset = log_size()
    t4_id, t4_no = create_task(PREFIX + "脏配置-退回旧逻辑", owner_id)
    to_doing(t4_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    n4 = notice_of(t4_id)
    expect(len(n4) == 1, "E1 配置非法时仍发 1 条通知（退回旧逻辑，不丢）", f"rows={len(n4)}")
    if n4:
        expect(n4[0]["title"] == "任务状态变更", "E2 退回旧逻辑后文案不变", n4[0]["title"])
    expect("参数非法" in log, "E3 运行阶段打了 ERROR 日志（含「参数非法」）")
    expect("联动已接管通知" not in log, "E4 非法配置没有被执行（没有接管日志）")

    # ---------- F. 收尾：删规则回 0 行 ----------
    drop_rule()
    expect(rule_rows() == "0", "F1 删除规则后回到 0 行", f"rules={rule_rows()}")

    print()
    print(f"==> 汇总：PASS={PASS} FAIL={FAIL}")
    if FAIL:
        sys.exit(1)


def _matched_line(log):
    for line in log.splitlines():
        if "联动影子对比" in line:
            return line.strip()[:300]
    return ""


if __name__ == "__main__":
    main()
