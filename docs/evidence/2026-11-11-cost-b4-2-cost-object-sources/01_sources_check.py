#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""COST-B4-2 · 成本对象 ↔ 出库/入库反查 的真库验收（ARCH-9 §8.1）。

对着**真实例 + 真库**跑，判据分三层，互不引用：

| 层 | 断言什么 | 数据来源 |
|---|---|---|
| 报文自平 | 合计 = 各行现算相加 | 接口自己的 `issues[] / receipts[]` |
| 报文 ↔ 库 | 接口给的数 = SQL 直接查明细的数 | `biz_stock_out_item` / `biz_stock_in_item` / `biz_cost_collect` / `biz_cost_period_item` |
| 5 条判据 | 两条链真的对得上（料已归集、货已入库且金额 = 结转成本） | 同上，但**独立复算**，不读 `checks[]` 的结论 |

用法（脚本只读，不写业务表）：

    BASE=http://127.0.0.1:18142 DB=cost_b42_e2e OBJ_NO=E2E-CB-0001 \
        python3 01_sources_check.py | tee 02_sources_check.out

退出码 0 = 全绿；非 0 = 有断言失败（打印期望 vs 实际）。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from decimal import Decimal

BASE = os.environ.get("BASE", "http://127.0.0.1:18142") + "/api"
DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
    name=os.environ.get("DB", "cost_b42_e2e"),
)
ADMIN = ("admin", "Admin@123456")
OBJ_NO = os.environ.get("OBJ_NO", "E2E-CB-0001")
TOL = Decimal("0.0001")

PASSED: list[str] = []
FAILED: list[str] = []


# ══════════════════════════════════════════════════════════════════
# 库（只读）
# ══════════════════════════════════════════════════════════════════
def q(sql: str) -> list[list[str]]:
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["name"], "-v", "ON_ERROR_STOP=1", "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"SQL 失败：{r.stderr.strip()[:800]}\n---\n{sql[:400]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


# ══════════════════════════════════════════════════════════════════
# 断言
# ══════════════════════════════════════════════════════════════════
def ok(name: str, cond: bool, expect: str = "", actual: str = "") -> bool:
    if cond:
        PASSED.append(name)
        print(f"  ✅ {name}" + (f"（{actual}）" if actual else ""))
        return True
    FAILED.append(name)
    print(f"  🚨 [FAIL] {name}")
    if expect or actual:
        print(f"       期望：{expect or '—'}\n       实际：{actual or '—'}")
    return False


def dec(value) -> Decimal:
    return Decimal(str(value or "0"))


def eq_dec(name: str, expect, actual) -> bool:
    return ok(name, abs(dec(expect) - dec(actual)) <= TOL, str(expect), str(actual))


def section(title: str) -> None:
    print(f"\n{'=' * 84}\n{title}\n{'=' * 84}")


# ══════════════════════════════════════════════════════════════════
# HTTP
# ══════════════════════════════════════════════════════════════════
class Client:
    def __init__(self) -> None:
        self.token = ""

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(BASE + path, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return resp.status, json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as err:
            raw = err.read().decode()
            try:
                return err.code, json.loads(raw)
            except Exception:
                return err.code, {"raw": raw}

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login",
                                   {"username": user, "password": pwd})
        if status != 200:
            sys.exit(f"登录失败（{user}）：{status} {payload}")
        data = payload.get("data") or {}
        self.token = data.get("access_token") or data.get("token") or ""
        if not self.token:
            sys.exit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def main() -> int:
    c = Client()

    section("① 登录 + 找到演练成本对象")
    c.login(*ADMIN)
    ok("admin 登录成功", bool(c.token))

    status, payload = c.call(
        "GET", f"/business/erp/cost-object?page=1&page_size=50&keyword={OBJ_NO}")
    ok("列表接口 200", status == 200, "200", str(status))
    rows = data_of(payload).get("list") or data_of(payload).get("records") or []
    target = next((r for r in rows if r.get("object_no") == OBJ_NO), None)
    if not target:
        ok(f"列表能按对象号筛到 {OBJ_NO}", False, OBJ_NO,
           str([r.get("object_no") for r in rows][:5]))
        print("\n找不到对象 ⇒ 后面的断言无法进行")
        return 1
    obj_id = target["id"]
    ok(f"列表能按对象号筛到 {OBJ_NO}", True, OBJ_NO, obj_id)

    section("② GET /cost-object/{id}/sources 报文形状")
    status, payload = c.call("GET", f"/business/erp/cost-object/{obj_id}/sources")
    ok("反查接口 200", status == 200, "200",
       f"{status} {payload.get('msg') if isinstance(payload, dict) else payload}")
    res = data_of(payload)
    for field in ["object_id", "object_no", "product_name", "status", "completed_qty",
                  "wip_qty", "issues", "issue_totals", "issue_posted_totals", "receipts",
                  "receipt_totals", "receipt_cancelled_count", "checks", "all_ok"]:
        ok(f"报文有字段 {field}", field in res)
    ok("object_id 与列表一致", res.get("object_id") == obj_id, obj_id, str(res.get("object_id")))
    ok("object_no 与列表一致", res.get("object_no") == OBJ_NO, OBJ_NO, str(res.get("object_no")))

    issues = res.get("issues") or []
    receipts = res.get("receipts") or []

    section("③ 领料链（这个批的料从哪来）")
    ok("领料单至少 1 张", len(issues) >= 1, ">=1", str(len(issues)))
    for row in issues:
        ok(f"领料行 {row.get('doc_no')} 有 doc_no / doc_date / status",
           bool(row.get("doc_no")) and bool(row.get("doc_date")) and bool(row.get("status")))
        # 跳转参数必须真能落到原单据（原表里查得到这张单）
        hit = one(f"select out_no from biz_stock_out where out_no = '{row['doc_no']}' "
                  f"and deleted_at is null")
        ok(f"领料行 {row.get('doc_no')} 在 biz_stock_out 里真实存在（可跳转）",
           hit == row["doc_no"], row["doc_no"], hit or "(库里没有)")
        ok(f"领料行 {row.get('doc_no')} 的 id 是合法 uuid（前端 router 用）",
           str(uuid.UUID(row["id"])) == row["id"])

    # 报文自平：合计 = 各行相加（脚本自己加，不读后端合计）
    all_qty = sum((dec(r["qty"]) for r in issues), Decimal("0"))
    all_amt = sum((dec(r["amount"]) for r in issues), Decimal("0"))
    posted_qty = sum((dec(r["qty"]) for r in issues if r["status"] == "POSTED"), Decimal("0"))
    posted_amt = sum((dec(r["amount"]) for r in issues if r["status"] == "POSTED"), Decimal("0"))
    eq_dec("issue_totals.qty = 各领料行相加", all_qty, res["issue_totals"]["qty"])
    eq_dec("issue_totals.amount = 各领料行相加", all_amt, res["issue_totals"]["amount"])
    eq_dec("issue_posted_totals.qty = 各 POSTED 行相加", posted_qty,
           res["issue_posted_totals"]["qty"])
    eq_dec("issue_posted_totals.amount = 各 POSTED 行相加", posted_amt,
           res["issue_posted_totals"]["amount"])
    ok("issue_totals.doc_count = 行数",
       res["issue_totals"].get("doc_count") == len(issues),
       str(len(issues)), str(res["issue_totals"].get("doc_count")))

    # 报文 ↔ 库：明细逐行汇总
    sql_issue = one(f"""select coalesce(sum(qty),0)||'|'||coalesce(sum(amount),0)
                          from biz_stock_out_item i
                          join biz_stock_out h on h.id = i.stock_out_id
                         where h.cost_object_id = '{obj_id}' and h.deleted_at is null
                           and h.status = 'POSTED'""") or "0|0"
    sql_qty, sql_amt = sql_issue.split("|")
    eq_dec("已过账领料数量 = 库里 biz_stock_out_item 汇总", sql_qty,
           res["issue_posted_totals"]["qty"])
    eq_dec("已过账领料金额 = 库里 biz_stock_out_item 汇总", sql_amt,
           res["issue_posted_totals"]["amount"])
    sql_lines = one(f"""select count(*) from biz_stock_out_item i
                          join biz_stock_out h on h.id = i.stock_out_id
                         where h.cost_object_id = '{obj_id}' and h.deleted_at is null""")
    ok("issue_totals.line_count = 库里明细行数",
       res["issue_totals"].get("line_count") == int(sql_lines),
       sql_lines, str(res["issue_totals"].get("line_count")))

    # 行级归集标记：collected_qty/amount 与 biz_cost_collect 实际一致
    for row in issues:
        sql_collected = one(f"""select coalesce(sum(c.qty),0)||'|'||coalesce(sum(c.amount),0)
                                  from biz_cost_collect c
                                  join biz_stock_out_item i on i.id = c.source_id
                                  join biz_stock_out h on h.id = i.stock_out_id
                                 where h.out_no = '{row['doc_no']}' and c.deleted_at is null
                                   and c.element = 'MATERIAL' and c.source_type = 'STOCK_OUT'""") or "0|0"
        cq, ca = sql_collected.split("|")
        eq_dec(f"领料行 {row['doc_no']} collected_qty = 库里归集数量", cq, row["collected_qty"])
        eq_dec(f"领料行 {row['doc_no']} collected_amount = 库里归集金额", ca, row["collected_amount"])
        full = row["status"] == "POSTED" \
            and abs(dec(row["qty"]) - dec(row["collected_qty"])) <= TOL \
            and abs(dec(row["amount"]) - dec(row["collected_amount"])) <= TOL
        ok(f"领料行 {row['doc_no']} collected 标记与「足额归集」判定一致",
           row["collected"] == full, str(full), str(row["collected"]))

    section("④ 完工入库链（这个批的货去哪了）")
    ok("完工入库单至少 2 张（1 张有效 + 1 张红冲作废）", len(receipts) >= 2,
       ">=2", str(len(receipts)))
    ok("receipt_cancelled_count = 报文里 CANCELLED 行数",
       res["receipt_cancelled_count"] == sum(1 for r in receipts if r["status"] == "CANCELLED"),
       str(sum(1 for r in receipts if r["status"] == "CANCELLED")),
       str(res["receipt_cancelled_count"]))
    for row in receipts:
        hit = one(f"select in_no from biz_stock_in where in_no = '{row['doc_no']}' "
                  f"and in_type = 'PRODUCTION' and deleted_at is null")
        ok(f"入库行 {row.get('doc_no')} 在 biz_stock_in（in_type=PRODUCTION）里真实存在",
           hit == row["doc_no"], row["doc_no"], hit or "(库里没有)")
        if row["status"] == "POSTED":
            ok(f"入库行 {row['doc_no']} 带结算批次号（页面显示「哪一批结算的」）",
               bool(row.get("batch_no")), "非空", str(row.get("batch_no")))

    rec_qty = sum((dec(r["qty"]) for r in receipts if r["status"] == "POSTED"), Decimal("0"))
    rec_amt = sum((dec(r["amount"]) for r in receipts if r["status"] == "POSTED"), Decimal("0"))
    eq_dec("receipt_totals.qty = 各 POSTED 入库行相加", rec_qty, res["receipt_totals"]["qty"])
    eq_dec("receipt_totals.amount = 各 POSTED 入库行相加", rec_amt,
           res["receipt_totals"]["amount"])

    sql_rec = one(f"""select coalesce(sum(qty),0)||'|'||coalesce(sum(amount),0)
                        from biz_stock_in_item i
                        join biz_stock_in h on h.id = i.stock_in_id
                       where h.cost_object_id = '{obj_id}' and h.deleted_at is null
                         and h.in_type = 'PRODUCTION' and h.status = 'POSTED'""") or "0|0"
    rq, ra = sql_rec.split("|")
    eq_dec("入库数量 = 库里 biz_stock_in_item 汇总", rq, res["receipt_totals"]["qty"])
    eq_dec("入库金额 = 库里 biz_stock_in_item 汇总", ra, res["receipt_totals"]["amount"])

    section("⑤ 5 条对平判据（独立复算，不读 checks 的结论）")
    checks = res.get("checks") or []
    ok("判据 5 条", len(checks) == 5, "5", str(len(checks)))
    ok("keys 与设计一一对应",
       [ck["key"] for ck in checks] == ["issue_collected_amount", "issue_collected_qty",
                                        "receipt_settled_qty", "receipt_settled_amount",
                                        "issue_all_collected"],
       "issue_collected_amount/issue_collected_qty/receipt_settled_qty/receipt_settled_amount/issue_all_collected",
       ",".join(ck["key"] for ck in checks))

    sql_issues_posted = one(f"""select coalesce(sum(i.qty),0)||'|'||coalesce(sum(i.amount),0)
                                  from biz_stock_out_item i
                                  join biz_stock_out h on h.id = i.stock_out_id
                                 where h.cost_object_id = '{obj_id}' and h.deleted_at is null
                                   and h.status = 'POSTED'""") or "0|0"
    sql_collect = one(f"""select coalesce(sum(qty),0)||'|'||coalesce(sum(amount),0)
                            from biz_cost_collect
                           where cost_object_id = '{obj_id}' and element = 'MATERIAL'
                             and source_type = 'STOCK_OUT' and deleted_at is null""") or "0|0"
    sql_settled = one(f"""select coalesce(sum(i.completed_qty),0)||'|'||coalesce(sum(i.finished_cost),0)
                            from biz_cost_period_item i
                            join biz_cost_period p on p.id = i.period_id
                           where i.cost_object_id = '{obj_id}' and i.status = 'SETTLED'
                             and p.deleted_at is null""") or "0|0"

    ip_qty, ip_amt = sql_issues_posted.split("|")
    co_qty, co_amt = sql_collect.split("|")
    se_qty, se_amt = sql_settled.split("|")

    by_key = {ck["key"]: ck for ck in checks}
    eq_dec("判据① 左（已过账领料金额）= 库", ip_amt, by_key["issue_collected_amount"]["left"])
    eq_dec("判据① 右（归集料行金额）= 库", co_amt, by_key["issue_collected_amount"]["right"])
    eq_dec("判据② 左（已过账领料数量）= 库", ip_qty, by_key["issue_collected_qty"]["left"])
    eq_dec("判据② 右（归集料行数量）= 库", co_qty, by_key["issue_collected_qty"]["right"])
    eq_dec("判据③ 左（POSTED 入库数量）= 库", rq, by_key["receipt_settled_qty"]["left"])
    eq_dec("判据③ 右（已结算计算单完工数量）= 库", se_qty, by_key["receipt_settled_qty"]["right"])
    eq_dec("判据④ 左（POSTED 入库金额）= 库", ra, by_key["receipt_settled_amount"]["left"])
    eq_dec("判据④ 右（已结算计算单完工转出）= 库", se_amt,
           by_key["receipt_settled_amount"]["right"])

    ok("判据① 领料金额 = 归集直接材料（真平）",
       abs(dec(ip_amt) - dec(co_amt)) <= TOL, ip_amt, co_amt)
    ok("判据③ 完工入库数量 = 计算单累计完工（真平）",
       abs(dec(rq) - dec(se_qty)) <= TOL, se_qty, rq)
    ok("判据④ 入库金额 = 完工转出成本（真平）",
       abs(dec(ra) - dec(se_amt)) <= TOL, se_amt, ra)

    # ⑤ 已过账领料都已归集：未足额归集的张数应为 0
    bad = 0
    for row in issues:
        if row["status"] != "POSTED":
            continue
        if abs(dec(row["qty"]) - dec(row["collected_qty"])) > TOL or \
                abs(dec(row["amount"]) - dec(row["collected_amount"])) > TOL:
            bad += 1
    ok("判据⑤ 未（足额）归集的领料单数 = 0", bad == 0, "0", str(bad))
    ok("报文 all_ok = true 且 5 条 ok 全 true",
       res["all_ok"] is True and all(ck["ok"] for ck in checks),
       "true", f"all_ok={res['all_ok']} red={[ck['key'] for ck in checks if not ck['ok']]}")

    section("⑥ 边界：不存在 / 非法 id")
    bogus = str(uuid.uuid4())
    status, payload = c.call("GET", f"/business/erp/cost-object/{bogus}/sources")
    ok("不存在的对象 ⇒ 404", status == 404, "404", f"{status} {payload.get('msg') or ''}")
    status, payload = c.call("GET", "/business/erp/cost-object/not-a-uuid/sources")
    ok("非 uuid ⇒ 400（不是 500 也不是 200）", status == 400, "400",
       f"{status} {payload.get('msg') or ''}")

    section("⑦ 同口径交叉：详情接口与反查接口的对象头一致")
    status, payload = c.call("GET", f"/business/erp/cost-object/{obj_id}")
    detail = data_of(payload)
    ok("详情接口 200", status == 200, "200", str(status))
    ok("详情与反查的 status 一致", detail.get("status") == res.get("status"),
       str(detail.get("status")), str(res.get("status")))
    ok("详情与反查的 completed_qty 一致",
       dec(detail.get("completed_qty")) == dec(res.get("completed_qty")),
       str(detail.get("completed_qty")), str(res.get("completed_qty")))

    print(f"\n{'=' * 84}")
    print(f"总计：{len(PASSED)} 条通过 / {len(FAILED)} 条失败")
    if FAILED:
        for name in FAILED:
            print(f"  🚨 {name}")
        return 1
    print("✅ 全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
