#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P0-FIX-1 探针：在隔离实例上做「真到点触发」与夹具清场。

用法（环境变量见下）：
  P12_BASE=http://127.0.0.1:18111 P12_DB_NAME=cloudpivot_p0fix1 python3 p0fix1_probe.py <cmd>

cmd：
  cleanup   清掉 p12 e2e 夹具（造出来的低库存/应付/合同会让账账不变量假红）
  schedule  把 erp_low_stock_scan 的 cron 改成「约 90 秒后」-> 证明调度器真的会到点触发
  restore   把 cron 还原成 0 40 7 * * *
  trigger   手动触发 4 个提醒（各落一条 sys_job_log）
  show      打印 4 行配置 + 近 10 条执行日志
"""
import datetime as dt
import importlib.util
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
E2E = "/opt/cloudpivot/scripts/verify/p12_business_reminder_e2e.py"

spec = importlib.util.spec_from_file_location("p12", E2E)
p12 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p12)

TARGET = "erp_low_stock_scan"
ORIG_CRON = "0 40 7 * * *"
ORIG_REMARK = ("低库存提醒（启用：每天 15:40 北京时间 = 07:40 UTC 扫描「当前库存 < 安全库存」"
               "的物料×仓库，通知仓管主管；每轮落 sys_job_log）")


def job_row(name=TARGET):
    return p12.psql(
        "SELECT id || '|' || cron_expr || '|' || status || '|' || "
        "coalesce(to_char(next_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'),'-') "
        f"FROM sys_job WHERE job_name = '{name}' AND deleted_at IS NULL")


def token():
    st, body, tok = p12.login(p12.ADMIN_USER, p12.ADMIN_PASS)
    assert tok, f"登录失败 {st} {body}"
    return tok


def cmd_cleanup():
    p12.cleanup()
    print("已清场（p12_* 夹具 / 通知 / 台账已删）")


def cmd_schedule():
    tok = token()
    jid = job_row().split("|")[0]
    fire_at = dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=90)
    cron = f"{fire_at.second} {fire_at.minute} {fire_at.hour} * * *"
    st, res = p12.call("PUT", f"/api/support/job/{jid}",
                       {"job_name": TARGET, "job_group": "ERP", "cron_expr": cron,
                        "invoke_target": TARGET, "status": 1, "remark": ORIG_REMARK}, tok)
    print(f"PUT cron={cron} -> {st} {res}")
    print(f"预期触发时刻（UTC）{fire_at:%Y-%m-%d %H:%M:%S} / 北京 "
          f"{fire_at + dt.timedelta(hours=8):%H:%M:%S}")


def cmd_restore():
    tok = token()
    jid = job_row().split("|")[0]
    st, res = p12.call("PUT", f"/api/support/job/{jid}",
                       {"job_name": TARGET, "job_group": "ERP", "cron_expr": ORIG_CRON,
                        "invoke_target": TARGET, "status": 1, "remark": ORIG_REMARK}, tok)
    print(f"PUT cron={ORIG_CRON} -> {st} {res}")
    print("当前行:", job_row())


def cmd_trigger():
    tok = token()
    rows = p12.psql("SELECT job_name || '|' || id FROM sys_job WHERE deleted_at IS NULL "
                    "AND job_name LIKE 'erp\\_%\\_scan' ORDER BY job_name").splitlines()
    for row in rows:
        name, jid = row.split("|")
        before = int(p12.psql(f"SELECT count(*) FROM sys_job_log WHERE job_id = '{jid}'"))
        st, res = p12.call("POST", f"/api/support/job/{jid}/trigger", None, tok)
        after = int(p12.psql(f"SELECT count(*) FROM sys_job_log WHERE job_id = '{jid}'"))
        msg = p12.psql(f"SELECT COALESCE(message,'') FROM sys_job_log WHERE job_id = '{jid}' "
                       "ORDER BY started_at DESC LIMIT 1")
        print(f"{name}: http={st} log {before}->{after}  {msg}")


def cmd_show():
    print("== sys_job（4 个提醒）")
    out = subprocess.run(
        ["psql", "-h", p12.DB["host"], "-p", p12.DB["port"], "-U", p12.DB["user"],
         "-d", p12.DB["name"], "-X", "-q", "-tAF", " | ",
         "-c", "SELECT job_name, job_group, cron_expr, invoke_target, status::text, "
               "to_char(last_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'), "
               "to_char(next_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS') "
               "FROM sys_job WHERE deleted_at IS NULL AND job_name LIKE 'erp\\_%\\_scan' "
               "ORDER BY job_name"],
        capture_output=True, text=True, env=dict(os.environ, PGPASSWORD=p12.DB["password"]))
    print(out.stdout.strip())
    print("\n== sys_job_log（近 10 条）")
    out = subprocess.run(
        ["psql", "-h", p12.DB["host"], "-p", p12.DB["port"], "-U", p12.DB["user"],
         "-d", p12.DB["name"], "-X", "-q", "-tAF", " | ",
         "-c", "SELECT job_name, status::text, "
               "to_char(started_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'), "
               "duration_ms::text, COALESCE(message,'') "
               "FROM sys_job_log ORDER BY started_at DESC LIMIT 10"],
        capture_output=True, text=True, env=dict(os.environ, PGPASSWORD=p12.DB["password"]))
    print(out.stdout.strip())


CMDS = {"cleanup": cmd_cleanup, "schedule": cmd_schedule, "restore": cmd_restore,
        "trigger": cmd_trigger, "show": cmd_show}

if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "show"
    CMDS[cmd]()
