#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-08 标准成本版本表 · HTTP 冒烟（真接口 + 真库，逐条断言）。

跑法（实例已起在旁边）：
    NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m508_e2e_full \
    COST_STD_BASE=http://127.0.0.1:18244 \
      python3 docs/evidence/2026-11-19-t_bda3bb6b-cost-standard-version/smoke_cost_standard.py

覆盖（每条都打 PASS/FAIL，最后 summary + 退出码）：
  ① 建版：三要素算数 / 版本号 / 物料实时 join / 标准 BOM
  ② 列表（status / period / keyword 三种筛选）
  ③ 详情
  ④ 「某物料某期间生效版本」：期间末口径 / 区间外 404 / 参数缺失 400
  ⑤ 期间 × 物料 矩阵
  ⑥ 生效期重叠 409 + 66108；端点相接放行
  ⑦ 未生效版本可改（工时改了金额跟着变）
  ⑧ 已生效版本改口径 409 + 66109；改停止日放行；删 409 + 66110
  ⑨ 失效 / 生效闭环（失效释放窗口 → 同期新版可建 → 再启用旧版被拒）
  ⑩ 未生效版本可删、删后 404；非法 id 404；矩阵缺期间 400

数值字段一律按**字符串**传（rust_decimal 默认不开 serde-float：JSON 浮点会被拒）。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import date, timedelta

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m508_e2e_full"),
)
BASE = os.environ.get("COST_STD_BASE", "http://127.0.0.1:18244") + "/api"
ADMIN = ("admin", os.environ.get("COST_STD_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M508SM{int(time.time()) % 100000}"

PASS, FAIL = [], []


def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"], "-d", DB["name"],
         "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:600]}\n---\n{sql[:400]}")
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


def ok(name: str, cond: bool, detail: str = "") -> None:
    (PASS if cond else FAIL).append(name)
    print(f"[{'PASS' if cond else 'FAIL'}] {name}{('  · ' + detail) if detail else ''}")


class Client:
    def __init__(self) -> None:
        self.token = ""

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(BASE + path, data=data, method=method,
                                     headers={"Content-Type": "application/json",
                                              **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status, json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode()
            try:
                return exc.code, json.loads(raw or "{}")
            except json.JSONDecodeError:
                return exc.code, {"raw": raw[:300]}

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login", {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = (payload.get("data") or {}).get("access_token", "")
        if not self.token:
            raise SystemExit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def err_of(payload) -> str:
    p = payload or {}
    return f"code={p.get('code')} msg={(p.get('msg') or p.get('message') or '')[:120]}"


def month_window(d: date) -> tuple[str, date, date]:
    code = f"{d.year:04d}-{d.month:02d}"
    start = date(d.year, d.month, 1)
    nxt = date(d.year + (1 if d.month == 12 else 0), 1 if d.month == 12 else d.month + 1, 1)
    return code, start, nxt - timedelta(days=1)


def setup() -> dict:
    """打开模块开关 + 造三个物料（成品 + 两个耗用）。返回物料 id。"""
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    ok("erp.cost.enabled 已在库里为 true", one("select config_value from sys_config where config_key='erp.cost.enabled'") == "true")
    cli = Client()
    cli.login(*ADMIN)
    status, payload = cli.call("POST", "/support/config/reload")
    ok("参数重载 200（模块开关立刻生效）", status == 200, err_of(payload))

    # 清上一次的夹具（重跑幂等）
    q(f"delete from biz_cost_standard where remark like '{TAG}%'")
    q(f"delete from biz_material where material_code like '{TAG}%'")

    ids = {}
    for kind, mtype in (("FG", "FINISHED"), ("RA", "RAW"), ("RB", "RAW")):
        q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                       material_type, status, remark)
              select gen_random_uuid(), '{TAG}-{kind}', '{TAG} 夹具 {kind}', 'M5-08',
                     (select id from biz_unit where deleted_at is null order by unit_code limit 1),
                     '{mtype}', 1, '{TAG}'""")
        ids[kind] = one(f"select id from biz_material where material_code = '{TAG}-{kind}'")
    ok("三个夹具物料已建（成品 + 两个耗用）", all(ids.values()), str(ids))

    # 清掉该物料上可能残留的标准成本（按物料 id，绝不扫全库）
    for mid in ids.values():
        q(f"delete from biz_cost_standard where material_id = '{mid}'::uuid")
    return {"cli": cli, **ids}


def create_req(material: str, period: str, items: list, *, from_date: str | None = None,
               to_date: str | None = None, hours: str = "2.5", rate: str = "31.111111",
               overhead: str = "12.345678", remark: str = "") -> dict:
    body = {
        "material_id": material,
        "std_labor_hours": hours,
        "std_labor_rate": rate,
        "std_overhead_cost": overhead,
        "items": items,
        "remark": f"{TAG} {remark}".strip(),
    }
    if period:
        body["period_code"] = period
    if from_date:
        body["effective_from"] = from_date
    if to_date:
        body["effective_to"] = to_date
    return body


def main() -> int:
    fx = setup()
    cli: Client = fx["cli"]
    fg, ra, rb = fx["FG"], fx["RA"], fx["RB"]
    today = date.today()
    period_a, a_start, a_end = month_window(today + timedelta(days=30))
    period_b, b_start, b_end = month_window(today + timedelta(days=62))
    period_past, p_start, p_end = month_window(today - timedelta(days=40))
    items = [
        {"component_material_id": ra, "std_qty": "3.000000", "std_price": "5.500000"},
        {"component_material_id": rb, "std_qty": "2.000000", "std_price": "0.333333"},
    ]

    # ---------- ① 建版 ----------
    status, payload = cli.call("POST", "/business/erp/cost-standard", create_req(fg, period_a, items, remark="第一版"))
    head = data_of(payload).get("head") or {}
    ver_a = head.get("id", "")
    ok("① 建版 200", status == 200, err_of(payload))
    ok("① 版本号从 1 起", head.get("version_no") == 1, str(head.get("version_no")))
    ok("① 料 = Σ明细（17.166666）", head.get("std_material_cost") == "17.166666", str(head.get("std_material_cost")))
    ok("① 工 = round(工时×费率,6)（77.777778）", head.get("std_labor_cost") == "77.777778", str(head.get("std_labor_cost")))
    ok("① 总 = 料+工+费（107.290122）", head.get("std_total_cost") == "107.290122", str(head.get("std_total_cost")))
    ok("① 状态默认生效", head.get("status") == 1 and head.get("status_label") == "生效")
    ok("① 生效区间落整期", head.get("effective_from") == a_start.isoformat() and head.get("effective_to") == a_end.isoformat(),
       f"{head.get('effective_from')} ~ {head.get('effective_to')}")
    ok("① 明细 2 行且带耗用物料编码",
       len(data_of(payload).get("items") or []) == 2
       and (data_of(payload)["items"][0].get("component_material_code") or "").startswith(TAG),
       str((data_of(payload).get("items") or [{}])[0].get("component_material_code")))
    ok("① 物料编码/名称实时 join", (head.get("material_code") or "").startswith(TAG) and bool(head.get("material_name")))

    # ---------- ② 列表 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-standard?material_id={fg}&status=1")
    ok("② 列表（物料 + 状态筛选）total=1", status == 200 and data_of(payload).get("total") == 1, err_of(payload))
    status, payload = cli.call("GET", f"/business/erp/cost-standard?material_id={fg}&period_code={period_a}")
    ok("② 列表（期间筛选）命中 1 条", data_of(payload).get("total") == 1, err_of(payload))
    status, payload = cli.call("GET", f"/business/erp/cost-standard?keyword={TAG}-FG")
    ok("② 列表（关键字=物料编码）命中", data_of(payload).get("total") == 1, err_of(payload))
    status, payload = cli.call("GET", f"/business/erp/cost-standard?material_id={fg}&period_code=2019-01")
    ok("② 列表（无关期间）0 条", data_of(payload).get("total") == 0, err_of(payload))

    # ---------- ③ 详情 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-standard/{ver_a}")
    detail = data_of(payload)
    ok("③ 详情 200 且明细 2 行", status == 200 and len(detail.get("items") or []) == 2, err_of(payload))
    ok("③ 明细金额 = round(用量×单价,6)",
       (detail.get("items") or [{}])[0].get("std_amount") == "16.500000",
       str((detail.get("items") or [{}])[0].get("std_amount")))

    # ---------- ④ 期间生效版本 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-standard/effective?material_id={fg}&period_code={period_a}")
    eff = data_of(payload)
    ok("④ 期间取数 200", status == 200, err_of(payload))
    ok("④ 取期末口径", eff.get("on_date") == a_end.isoformat(), str(eff.get("on_date")))
    ok("④ 命中刚建的版本", (eff.get("version") or {}).get("head", {}).get("id") == ver_a)
    ok("④ 整期一个版本 ⇒ switch_in_period=false", eff.get("switch_in_period") is False)
    status, payload = cli.call("GET", f"/business/erp/cost-standard/effective?material_id={fg}&on_date={a_start.isoformat()}")
    ok("④ 按日取数（期初）命中", status == 200 and data_of(payload).get("version", {}).get("head", {}).get("id") == ver_a)
    status, payload = cli.call("GET", f"/business/erp/cost-standard/effective?material_id={fg}&on_date={(a_start - timedelta(days=1)).isoformat()}")
    ok("④ 区间外 404", status == 404, f"{status} {err_of(payload)}")
    status, payload = cli.call("GET", f"/business/erp/cost-standard/effective?material_id={fg}")
    ok("④ 缺期间与日期 400/10001", status == 400 and payload.get("code") == 10001, f"{status} {err_of(payload)}")

    # ---------- ⑤ 期间矩阵 ----------
    status, payload = cli.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={period_a}&keyword={TAG}-FG")
    sheet = data_of(payload)
    ok("⑤ 矩阵 200 且只命中本物料 1 行", status == 200 and sheet.get("total") == 1, err_of(payload))
    row = (sheet.get("list") or [{}])[0]
    ok("⑤ 矩阵取期末生效版本（v1）", (row.get("head") or {}).get("version_no") == 1)
    ok("⑤ 整期一版 ⇒ switch_in_period=false", row.get("switch_in_period") is False)

    # ---------- ⑥ 重叠 / 相邻 ----------
    status, payload = cli.call("POST", "/business/erp/cost-standard",
                               create_req(fg, "", [], from_date=(a_start + timedelta(days=10)).isoformat(),
                                          to_date=a_end.isoformat(), remark="重叠探针"))
    ok("⑥ 同期间重叠建版 409 + 66108", status == 409 and payload.get("code") == 66108, f"{status} {err_of(payload)}")
    status, payload = cli.call("POST", "/business/erp/cost-standard", create_req(fg, period_b, [], remark="第二版"))
    ver_b = (data_of(payload).get("head") or {}).get("id", "")
    ok("⑥ 端点相接（下一期间）放行 200", status == 200, err_of(payload))
    ok("⑥ 版本号递增到 2", (data_of(payload).get("head") or {}).get("version_no") == 2)
    status, payload = cli.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={period_b}&keyword={TAG}-FG")
    ok("⑥ 矩阵在下一期间只出 v2", ((data_of(payload).get("list") or [{}])[0].get("head") or {}).get("version_no") == 2)

    # ---------- ⑦ 未生效版本可改 ----------
    status, payload = cli.call("PUT", f"/business/erp/cost-standard/{ver_b}", {"std_labor_hours": "3"})
    upd = data_of(payload)
    ok("⑦ 改未生效版本工时 200", status == 200, err_of(payload))
    ok("⑦ 人工成本跟着重算（3×31.111111=93.333333）", (upd.get("head") or {}).get("std_labor_cost") == "93.333333",
       str((upd.get("head") or {}).get("std_labor_cost")))
    status, payload = cli.call("PUT", f"/business/erp/cost-standard/{ver_b}",
                               {"items": [{"component_material_id": ra, "std_qty": "5.000000", "std_price": "3.000000"}]})
    ok("⑦ 整表替换标准 BOM（料=15.000000）",
       (data_of(payload).get("head") or {}).get("std_material_cost") == "15.000000"
       and len(data_of(payload).get("items") or []) == 1,
       str((data_of(payload).get("head") or {}).get("std_material_cost")))

    # ---------- ⑧ 已生效锁改锁删 ----------
    status, payload = cli.call("POST", "/business/erp/cost-standard", create_req(fg, period_past, [], remark="过去版"))
    ver_past = (data_of(payload).get("head") or {}).get("id", "")
    ok("⑧ 建一个已生效过的版本 200", status == 200, err_of(payload))
    status, payload = cli.call("PUT", f"/business/erp/cost-standard/{ver_past}", {"std_labor_rate": "40"})
    ok("⑧ 已生效版本改工资率 409 + 66109", status == 409 and payload.get("code") == 66109, f"{status} {err_of(payload)}")
    status, payload = cli.call("PUT", f"/business/erp/cost-standard/{ver_past}",
                               {"effective_to": p_end.isoformat(), "remark": f"{TAG} 收口"})
    ok("⑧ 已生效版本改停止日/备注放行 200", status == 200, err_of(payload))
    status, payload = cli.call("DELETE", f"/business/erp/cost-standard/{ver_past}")
    ok("⑧ 已生效版本删除 409 + 66110", status == 409 and payload.get("code") == 66110, f"{status} {err_of(payload)}")

    # ---------- ⑨ 失效 / 生效闭环 ----------
    status, payload = cli.call("POST", f"/business/erp/cost-standard/{ver_a}/status",
                               {"status": 0, "reason": "标准过时，换新版"})
    res = data_of(payload)
    ok("⑨ 失效 200 且状态变失效", status == 200 and (res.get("version") or {}).get("head", {}).get("status") == 0, err_of(payload))
    ok("⑨ 失效给出后果警告", bool(res.get("warnings")), str((res.get("warnings") or [""])[0])[:80])
    ok("⑨ 失效原因留痕进备注", "[失效" in ((res.get("version") or {}).get("head", {}).get("remark") or ""))
    status, payload = cli.call("GET", f"/business/erp/cost-standard/effective?material_id={fg}&period_code={period_a}")
    ok("⑨ 失效后该期间没有生效版本（404）", status == 404, f"{status} {err_of(payload)}")
    status, payload = cli.call("POST", f"/business/erp/cost-standard/{ver_a}/status", {"status": 1})
    ok("⑨ 重新启用 200", status == 200, err_of(payload))
    status, payload = cli.call("POST", "/business/erp/cost-standard", create_req(fg, period_a, [], remark="同期新版"))
    ok("⑨ 窗口已被 v1 占回 ⇒ 同期新版 409", status == 409 and payload.get("code") == 66108, f"{status} {err_of(payload)}")
    cli.call("POST", f"/business/erp/cost-standard/{ver_a}/status", {"status": 0, "reason": "再让位"})
    status, payload = cli.call("POST", "/business/erp/cost-standard", create_req(fg, period_a, [], remark="让位后新版"))
    ver_new = (data_of(payload).get("head") or {}).get("id", "")
    ok("⑨ 失效释放窗口 ⇒ 同期新版 200（版本号 4）", status == 200 and (data_of(payload).get("head") or {}).get("version_no") == 4,
       err_of(payload))
    status, payload = cli.call("POST", f"/business/erp/cost-standard/{ver_a}/status", {"status": 1})
    ok("⑨ 再启用旧版与新版重叠 ⇒ 409 + 66108", status == 409 and payload.get("code") == 66108, f"{status} {err_of(payload)}")

    # ---------- ⑩ 删除 / 404 / 400 ----------
    status, payload = cli.call("DELETE", f"/business/erp/cost-standard/{ver_new}")
    ok("⑩ 未生效版本可删 200", status == 200, err_of(payload))
    status, payload = cli.call("GET", f"/business/erp/cost-standard/{ver_new}")
    ok("⑩ 删后详情 404", status == 404, f"{status} {err_of(payload)}")
    status, payload = cli.call("GET", "/business/erp/cost-standard/11111111-1111-4111-8111-111111111111")
    ok("⑩ 不存在的 id 404", status == 404, f"{status}")
    status, payload = cli.call("GET", "/business/erp/cost-standard/period-sheet")
    ok("⑩ 矩阵缺期间 400/10001", status == 400 and payload.get("code") == 10001, f"{status} {err_of(payload)}")

    # ---------- 清场 + 库层核对 ----------
    for mid in (fg, ra, rb):
        q(f"delete from biz_cost_standard where material_id = '{mid}'::uuid")
    q(f"delete from biz_material where material_code like '{TAG}%'")
    left = one(f"select count(*) from biz_cost_standard where remark like '{TAG}%'")
    ok("清场零残留（biz_cost_standard 按 remark 前缀）", left == "0", left)

    print(f"\n===== {len(PASS)} PASS / {len(FAIL)} FAIL =====")
    if FAIL:
        for name in FAIL:
            print("  FAILED:", name)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
