#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-7 账务巡检落库 / 看板探针（HTTP + DB 双侧对账）。

跑法（先起隔离实例，见 scripts/verify/p27_instance.sh up）：
    BASE_URL=http://127.0.0.1:18095 \
    TEST_USER=admin TEST_PASS='P27Admin@123456' \
    NIK_DATABASE_NAME=cloudpivot_p27_inv NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
    python3 scripts/verify/p27_invariant_store_probe.py

探的是什么（每条都是「接口说一套、库里是另一套」的对照）：
    A 组  表结构 / 种入的 sys_job 行
    B 组  POST /run 两次：返回体 vs sys_invariant_result 落库行逐字段对账
    C 组  GET /overview：latest / trend / failures 与库内真相一致
    D 组  GET /page 与 GET /{id}：分页口径与明细完整性

只写 sys_job 之外的业务表？不 —— 本探针只调「运行巡检」接口（它写 sys_invariant_result），
不改任何 ERP 业务数据。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:18095")
USER = os.environ.get("TEST_USER", "admin")
PASS = os.environ.get("TEST_PASS", "P27Admin@123456")

DB = {
    "host": os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    "port": os.environ.get("NIK_DATABASE_PORT", "5432"),
    "user": os.environ.get("NIK_DATABASE_USER", "alun"),
    "name": os.environ.get("NIK_DATABASE_NAME", "cloudpivot_p27_inv"),
    "password": os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
}

CST = timezone(timedelta(hours=8))
RESULTS = []


def rec(tag, ok, desc, evidence=""):
    RESULTS.append(ok)
    print(f"{tag:<6} {'OK' if ok else 'FAIL':<6} {desc}{(' | ' + evidence) if evidence else ''}")
    return ok


def psql(sql):
    """跑一条 SQL，返回行（每行是字段列表）。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = DB["password"]
    proc = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["name"], "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip()[:400])
    return [line.split("\x1f") for line in proc.stdout.strip().split("\n") if line != ""]


def http(method, path, token=None, body=None):
    data = json.dumps(body, ensure_ascii=False).encode() if body is not None else None
    req = urllib.request.Request(BASE_URL + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=320) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        return exc.code, json.loads(exc.read().decode() or "{}")


def login():
    status, body = http("POST", "/api/system/auth/login",
                        body={"username": USER, "password": PASS})
    if status != 200 or body.get("code") != 0:
        print(f"登录失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:300]}")
        sys.exit(2)
    data = body["data"]
    return data["access_token"]


def ensure_ready(token):
    """空库首次登录会被强制改密（middleware.session_guard 拦截），这里把它走完。

    这是**产品既有安全策略**，不是本卡的改动；探针只是顺着它走，
    顺便证明「改密后看板接口才可用」这条链路是通的。
    """
    global PASS
    status, body = http("GET", "/api/support/invariant/overview", token=token)
    if status != 403 or "首次登录" not in (body.get("msg") or ""):
        return token
    new_pass = PASS + "_p27"
    status, body = http(
        "PUT", "/api/system/auth/password", token=token,
        body={"old_password": PASS, "new_password": new_pass},
    )
    if status != 200 or body.get("code") != 0:
        print(f"首次改密失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:300]}")
        sys.exit(2)
    print(f"… 空库首次登录：已按产品策略改密（{USER} 的口令已更新为 6+ 位新口令）")
    PASS = new_pass
    return login()



def main():
    token = ensure_ready(login())
    print(f"基线：库 {DB['name']} @ {DB['host']}:{DB['port']}，实例 {BASE_URL}\n")

    # ---------- A 组：表结构 + 种入的 sys_job 行 ----------
    cols = psql(
        "select column_name, data_type, is_nullable from information_schema.columns "
        "where table_name = 'sys_invariant_result' order by ordinal_position"
    )
    names = [c[0] for c in cols]
    want_cols = {
        "id", "run_at", "trigger_type", "status", "total", "passed", "failed",
        "blockers", "warns", "duration_ms", "database_name", "script_path",
        "detail", "failures", "error", "created_at",
    }
    rec("A1", want_cols.issubset(set(names)), "sys_invariant_result 表存在且列齐全",
        f"列数={len(names)}")

    idx = psql(
        "select indexname from pg_indexes where tablename = 'sys_invariant_result' order by indexname"
    )
    idx_names = [i[0] for i in idx]
    rec("A2", "idx_invariant_run_at" in idx_names and "idx_invariant_not_ok" in idx_names,
        "run_at 倒序索引 + 非 OK 部分索引存在", ",".join(idx_names))

    cks = psql(
        "select conname from pg_constraint where conrelid = 'sys_invariant_result'::regclass "
        "and contype = 'c' order by conname"
    )
    ck_names = [c[0] for c in cks]
    rec("A3", "ck_invariant_status" in ck_names and "ck_invariant_trigger" in ck_names,
        "status / trigger_type 的 CHECK 约束存在", ",".join(ck_names))

    job = psql(
        "select job_name, cron_expr, invoke_target, status from sys_job "
        "where job_name = 'invariant_audit' and deleted_at is null"
    )
    ok_job = bool(job) and job[0][1] == "0 20 2 * * *" and job[0][2] == "invariant_audit"
    rec("A4", ok_job, "sys_job 已种入 invariant_audit（可见行，供查看/手动执行）",
        json.dumps(job, ensure_ascii=False))

    before = int(psql("select count(*) from sys_invariant_result")[0][0])

    # ---------- B 组：POST /run 两轮，返回体 vs 落库行 ----------
    runs = []
    for round_no in (1, 2):
        status, body = http("POST", "/api/support/invariant/run", token=token)
        code = body.get("code")
        data = body.get("data") or {}
        rec(f"B{round_no}a", status == 200 and code == 0,
            f"第 {round_no} 轮 POST /support/invariant/run 返回 200",
            f"HTTP {status} code={code} msg={body.get('msg')}")
        rec(f"B{round_no}b", data.get("status") in ("OK", "FAIL") and (data.get("total") or 0) > 0,
            f"第 {round_no} 轮结论是账务结论（OK/FAIL）且检查条数 > 0",
            f"status={data.get('status')} total={data.get('total')} passed={data.get('passed')} "
            f"blockers={data.get('blockers')} warns={data.get('warns')} duration={data.get('duration_ms')}ms")
        rec(f"B{round_no}c", (data.get("trigger_type") == "MANUAL"),
            f"第 {round_no} 轮 trigger_type=MANUAL", str(data.get("trigger_type")))
        runs.append(data)

    after = int(psql("select count(*) from sys_invariant_result")[0][0])
    rec("B3", after == before + 2, "两轮运行各落一行（count +2）", f"{before} -> {after}")

    db_rows = psql(
        "select id::text, status, total, passed, failed, blockers, warns, trigger_type, "
        "database_name, script_path, (detail is not null), (failures is not null) "
        "from sys_invariant_result order by run_at desc limit 2"
    )
    latest_db = dict(zip(
        ["id", "status", "total", "passed", "failed", "blockers", "warns",
         "trigger_type", "database_name", "script_path", "has_detail", "has_failures"],
        db_rows[0],
    ))
    rec("B4", latest_db["id"] == runs[-1]["id"],
        "库里最新一行的 id 与第二轮返回体一致", f"{latest_db['id']}")
    rec("B5",
        int(latest_db["total"]) == runs[-1]["total"]
        and int(latest_db["passed"]) == runs[-1]["passed"]
        and int(latest_db["failed"]) == runs[-1]["failed"],
        "库里最新一行的 total/passed/failed 与返回体一致",
        f"db={latest_db['total']}/{latest_db['passed']}/{latest_db['failed']} "
        f"api={runs[-1]['total']}/{runs[-1]['passed']}/{runs[-1]['failed']}")
    rec("B6", latest_db["has_detail"] == "t" and latest_db["has_failures"] == "t",
        "明细与失败项两个 JSONB 列都已写入", f"detail={latest_db['has_detail']} failures={latest_db['has_failures']}")
    rec("B7", runs[0]["id"] != runs[1]["id"], "两轮是两条独立记录（id 不同）",
        f"{runs[0]['id']} vs {runs[1]['id']}")

    # ---------- C 组：overview 与库内真相 ----------
    status, body = http("GET", "/api/support/invariant/overview?days=30", token=token)
    ov = body.get("data") or {}
    rec("C1", status == 200 and body.get("code") == 0, "GET /overview 返回 200")
    rec("C2", (ov.get("latest") or {}).get("id") == runs[-1]["id"],
        "overview.latest 指向最近一次运行", str((ov.get("latest") or {}).get("id")))
    rec("C3", (ov.get("failures") or []) and len(ov["failures"]) == runs[-1]["failed"]
        or (runs[-1]["failed"] == 0 and not ov.get("failures")),
        "当前失败项条数 == 最近一次 failed",
        f"failures={len(ov.get('failures') or [])} failed={runs[-1]['failed']}")
    trend = ov.get("trend") or []
    today = datetime.now(CST).strftime("%Y-%m-%d")
    rec("C4", bool(trend) and trend[-1]["date"] == today,
        "趋势最后一点是今天（北京时间切日）",
        f"最后一点={trend[-1]['date'] if trend else '无'} 今天={today}")
    rec("C5", bool(trend) and trend[-1]["runs"] >= 2,
        "今天的趋势点累计到两轮运行", f"runs={trend[-1]['runs'] if trend else 0}")
    rec("C6", all(p["failed"] == 0 or p["status"] != "OK" for p in trend),
        "趋势口径自洽：失败数为 0 的点不会标成非 OK")
    rec("C7", (ov.get("runs_in_window") or 0) >= 2, "窗口内运行次数 >= 2",
        str(ov.get("runs_in_window")))
    rec("C8", ov.get("schedule", {}).get("cron") == "0 20 2 * * *",
        "看板展示的调度口径 = 每天 02:20", json.dumps(ov.get("schedule"), ensure_ascii=False))

    # ---------- D 组：分页与详情 ----------
    status, body = http("GET", "/api/support/invariant/page?page=1&page_size=10", token=token)
    page_data = body.get("data") or {}
    rows = page_data.get("list") or []
    rec("D1", status == 200 and (page_data.get("total") or 0) >= 2,
        "GET /page 返回分页", f"total={page_data.get('total')} 本页={len(rows)}")
    rec("D2", bool(rows) and rows[0]["id"] == runs[-1]["id"], "分页按 run_at 倒序（首行是最新一次）")
    rec("D3", "detail" not in rows[0] if rows else False,
        "分页列表不带全量明细（只带计数）", f"字段={sorted(rows[0].keys()) if rows else '—'}")

    status, body = http("GET", f"/api/support/invariant/page?page=1&page_size=10&status=ERROR", token=token)
    err_rows = (body.get("data") or {}).get("list") or []
    rec("D4", all(r["status"] == "ERROR" for r in err_rows), "status 过滤生效",
        f"ERROR 行 {len(err_rows)} 条")

    status, body = http("GET", f"/api/support/invariant/{runs[-1]['id']}", token=token)
    detail = body.get("data") or {}
    rec("D5", status == 200 and len(detail.get("detail") or []) == runs[-1]["total"],
        "详情返回全量检查明细（条数 == total）",
        f"detail={len(detail.get('detail') or [])} total={runs[-1]['total']}")
    rec("D6", len(detail.get("failures") or []) == runs[-1]["failed"],
        "详情里的失败项条数 == failed",
        f"failures={len(detail.get('failures') or [])} failed={runs[-1]['failed']}")
    rec("D7", all(set(["name", "group", "level", "ok", "detail"]).issubset(item.keys())
                  for item in (detail.get("detail") or [])[:5]),
        "检查项字段与脚本输出同构（name/group/level/ok/detail）")

    status, body = http("GET", "/api/support/invariant/not-a-uuid", token=token)
    rec("D8", status == 400, "非法 id 返回 400（不是 500）", f"HTTP {status}")

    # ---------- 汇总 ----------
    total = len(RESULTS)
    failed = RESULTS.count(False)
    print("\n" + "=" * 78)
    print(f"结果：{total - failed}/{total} 通过   FAIL={failed}")
    print("=" * 78)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
