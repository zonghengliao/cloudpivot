#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P0-FIX-1 铁证：定时任务真的会「到点自己触发」，而且**只触发一次**。

做法：
  1. 读当前 erp_low_stock_scan 的 sys_job_log 条数（before）
  2. 用项目接口 PUT 把它改成「约 70 秒后」触发（cron 秒/分/时 = 目标时刻）
  3. 等到触发时刻 + 40 秒后读条数（after）与新增行的摘要
  4. 统计服务端日志里「业务提醒扫描完成」出现次数 —— 那是**内置注册**那一路的日志，
     如果动态注册和内置注册各跑一次，这里必然 > 0；= 0 说明只有 sys_job 驱动那一轮跑
  5. PUT 把 cron 还原成 0 40 7 * * *
"""
import datetime as dt
import importlib.util
import os
import time

E2E = "/opt/cloudpivot/scripts/verify/p12_business_reminder_e2e.py"
SERVER_LOG = "/root/p0fix1-server.log"

spec = importlib.util.spec_from_file_location("p12", E2E)
p12 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p12)

TARGET = "erp_low_stock_scan"
ORIG = "0 40 7 * * *"
REMARK = ("低库存提醒（启用：每天 15:40 北京时间 = 07:40 UTC 扫描「当前库存 < 安全库存」"
          "的物料×仓库，通知仓管主管；每轮落 sys_job_log）")


def log_count():
    return int(p12.psql(f"SELECT count(*) FROM sys_job_log l JOIN sys_job j ON j.id = l.job_id "
                        f"WHERE j.job_name = '{TARGET}'"))


def grep_count(needle):
    try:
        with open(SERVER_LOG, "r", encoding="utf-8", errors="ignore") as fh:
            return sum(1 for line in fh if needle in line)
    except FileNotFoundError:
        return -1


def put(cron):
    tok = os.environ.get("P0FIX1_TOKEN", "")
    if not tok:
        st, body, tok = p12.login(p12.ADMIN_USER, p12.ADMIN_PASS)
        assert tok, f"登录失败 {st} {body}"
    jid = p12.psql(f"SELECT id FROM sys_job WHERE job_name = '{TARGET}' AND deleted_at IS NULL")
    st, res = p12.call("PUT", f"/api/support/job/{jid}",
                       {"job_name": TARGET, "job_group": "ERP", "cron_expr": cron,
                        "invoke_target": TARGET, "status": 1, "remark": REMARK}, tok)
    return st, res


def main():
    before = log_count()
    fired_at = dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=70)
    cron = f"{fired_at.second} {fired_at.minute} {fired_at.hour} * * *"
    st, res = put(cron)
    print(f"① PUT cron=`{cron}` -> HTTP {st} {res.get('msg')}")
    print(f"   预期触发：UTC {fired_at:%H:%M:%S} / 北京 "
          f"{fired_at + dt.timedelta(hours=8):%H:%M:%S}")
    print(f"② 触发前 {TARGET} 的 sys_job_log 条数 = {before}")

    wait = 110
    print(f"③ 等 {wait}s（不碰任何接口，纯等调度器自己动）...")
    time.sleep(wait)

    after = log_count()
    print(f"④ 触发后条数 = {after}（增量 {after - before}）")
    print("   新增行：")
    print(p12.psql(
        "SELECT l.job_name || ' | status=' || l.status || ' | ' || "
        "to_char(l.started_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI:SS') || ' | ' || "
        "coalesce(l.message,'') FROM sys_job_log l JOIN sys_job j ON j.id = l.job_id "
        f"WHERE j.job_name = '{TARGET}' ORDER BY l.started_at DESC LIMIT 3"))

    builtin_lines = grep_count("业务提醒扫描完成")
    print(f"⑤ 服务端日志里「业务提醒扫描完成」（内置注册那一路）出现 {builtin_lines} 次")
    print("   = 0 ⇒ 这一轮只有 sys_job 驱动的那条在跑，不存在「同一 cron 跑两遍」")

    st, res = put(ORIG)
    print(f"⑥ 还原 cron=`{ORIG}` -> HTTP {st} {res.get('msg')}")
    row = p12.psql("SELECT cron_expr || ' | status=' || status || ' | next=' || "
                   "coalesce(to_char(next_run_at at time zone 'Asia/Shanghai',"
                   "'YYYY-MM-DD HH24:MI:SS'),'-') FROM sys_job "
                   f"WHERE job_name = '{TARGET}' AND deleted_at IS NULL")
    print(f"   当前行：{row}")
    ok = (after - before) == 1 and builtin_lines == 0
    print("\n结论：" + ("✅ 一次且仅一次，到点真触发" if ok else "❌ 不符合预期，见上面数字"))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
