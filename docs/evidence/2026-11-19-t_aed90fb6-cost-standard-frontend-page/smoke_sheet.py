#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-18 标准成本页（期间视图 + 查生效版本）· HTTP 冒烟（真接口 + 真库）。

跑法（实例已起在旁边，库已迁到 V0465，且 `erp.cost.enabled = true`）：

    CS_BASE=http://127.0.0.1:8023 NIK_DATABASE_NAME=cp_m518_smoke \\
      python3 docs/evidence/2026-11-19-t_aed90fb6-cost-standard-frontend-page/smoke_sheet.py

覆盖页面**新两屏**的取数接口（页面不再自己算数，全部落在这两条 GET 上）：

  ① 期间视图 `GET /cost-standard/period-sheet`
     - 期间必填（缺 → 400 / 10001）
     - **只含「已发布 + 启用 + 覆盖期末」的行**：草稿 / 审批中不出现；只覆盖期初到中旬的
       版本也不出现（期末没人覆盖 —— 这正是 `switch_in_period` 存在的前提）
     - 期间内换版 ⇒ 期末那一版一行 + `switch_in_period = true`
     - 单一版本覆盖整期 ⇒ 一行 + `switch_in_period = false`
     - 关键字收窄 / 分页
  ② 查生效版本 `GET /cost-standard/effective`
     - 期间 → 按期末取数；指定日期 → 按该日取数
     - 期间内换版 ⇒ `switch_in_period` + `other_versions` + `note`
     - 没有可取数版本 → 404 + `code=10002`（页面据此补「是不是还没发布」）
     - `period_code` 与 `on_date` 互相矛盾 → 400
  ③ 发布轴（期间视图「提交发布 / 撤回」按钮的同一批接口）
     - 提交 → PENDING；重复提交 / 重复撤回 → 409 + `66111`
     - 审批中改口径 → 409 + `66111`；撤回后可继续改
  ④ 清场：四张卡全删，库内零残留

夹具用**未来期间 2026-12**：卡不会被「已生效过」锁死（`effective_from < 今天`），
所以能改也能删 —— 这是本脚本能清场的前提。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m518_smoke"),
)
BASE = os.environ.get("CS_BASE", "http://127.0.0.1:8023") + "/api"
LOGIN = (os.environ.get("CS_USER", "e2e01_appr"),
         os.environ.get("CS_PASSWORD", "E2e01@Appr123"))

PERIOD = "2026-12"
PERIOD_END = "2026-12-31"
V1_FROM, V1_TO = "2026-12-01", "2026-12-15"
V2_FROM, V2_TO = "2026-12-16", "2026-12-31"
WHOLE_FROM, WHOLE_TO = "2026-12-01", "2026-12-31"

PASS, FAIL = [], []


# ==================== 小工具 ====================

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

    def _send(self, req):
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                return resp.status, json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as err:
            raw = err.read()
            try:
                return err.code, json.loads(raw.decode() or "{}")
            except Exception:  # noqa: BLE001
                return err.code, {"_raw": raw[:200].decode(errors="replace")}

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            BASE + path, data=data, method=method,
            headers={"Content-Type": "application/json",
                     **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        return self._send(req)

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/system/auth/login", {"username": user, "password": pwd})
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = (payload.get("data") or {}).get("access_token", "")
        if not self.token:
            raise SystemExit(f"登录回包没有 token：{payload}")


def data_of(payload) -> dict:
    return (payload or {}).get("data") or {}


def code_of(payload):
    return (payload or {}).get("code")


def msg_of(payload) -> str:
    raw = payload if isinstance(payload, dict) else {}
    text = raw.get("msg") or raw.get("message") or raw.get("_raw") or ""
    return str(text)[:160]


def items_of(payload) -> list:
    return data_of(payload).get("list") or []


# ==================== 夹具 ====================

def finished_materials(limit: int = 3) -> list[tuple[str, str]]:
    rows = q("select id, material_code from biz_material where deleted_at is null and status = 1 "
             f"and material_type = 'FINISHED' order by material_code limit {limit}")
    if len(rows) < limit:
        raise SystemExit(f"需要 {limit} 个 FINISHED 物料，只找到 {len(rows)} 个")
    return [(r[0], r[1]) for r in rows]


def fixture() -> tuple[list[tuple[str, str]], str]:
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    ok("erp.cost.enabled = true（开关读的是 sys_config，改完需重启进程才失效缓存）",
       one("select config_value from sys_config where config_key='erp.cost.enabled'") == "true")

    mats = finished_materials(3)
    raw = q("select id, material_code from biz_material where deleted_at is null and status = 1 "
            "and material_type = 'RAW' order by material_code limit 1")
    if not raw:
        raise SystemExit("没有可用的 RAW 物料（标准 BOM 的耗用物料）")
    comp_id = raw[0][0]

    for mid, _code in mats:
        q(f"delete from biz_cost_standard_item where standard_id in "
          f"(select id from biz_cost_standard where material_id = '{mid}')")
        q(f"delete from biz_cost_standard where material_id = '{mid}'")
    print(f"夹具：{' / '.join(code for _mid, code in mats)} · 耗用 {raw[0][1]}")
    return mats, comp_id


def create_version(client: Client, material_id: str, comp_id: str, frm: str, to: str,
                   rate: str = "30.000000", remark: str = "") -> dict:
    status, payload = client.call("POST", "/business/erp/cost-standard", {
        "material_id": material_id,
        "effective_from": frm,
        "effective_to": to,
        "std_labor_hours": "2.0000",
        "std_labor_rate": rate,
        "std_overhead_cost": "12.000000",
        "items": [{"component_material_id": comp_id, "std_qty": "3.000000", "std_price": "5.000000"}],
        "remark": remark,
    })
    if status != 200:
        raise SystemExit(f"建标准成本失败：{status} {payload}")
    return data_of(payload)


def publish_via_sql(version_id: str) -> None:
    """模拟「财务经理审批通过」。真人审批链属 M5-10 的真库用例，这里只验读路径。"""
    q(f"update biz_cost_standard set publish_state = 'PUBLISHED', status = 1, "
      f"published_at = now(), publish_instance_id = null where id = '{version_id}'")


# ==================== ① 期间视图 ====================

def check_period_sheet(client: Client, mats: list[tuple[str, str]], comp_id: str) -> dict:
    print("\n=== ① 期间视图 GET /cost-standard/period-sheet ===")
    (mat_a, code_a), (mat_b, code_b), _ = mats

    status, payload = client.call("GET", "/business/erp/cost-standard/period-sheet")
    ok("期间必填：缺 period_code → 400 + 10001",
       status == 400 and code_of(payload) == 10001, f"{status} code={code_of(payload)}")

    v1 = create_version(client, mat_a, comp_id, V1_FROM, V1_TO, remark="M518 smoke v1")
    vid1 = v1["head"]["id"]
    ok("新建卡默认是草稿（「提交发布」按钮的起点）",
       v1["head"]["publish_state"] == "DRAFT", v1["head"].get("publish_state_label", ""))
    ok("新建卡金额：料 15 / 工 60 / 费 12 / 总 87（与库 CHECK 同口径）",
       (v1["head"]["std_material_cost"], v1["head"]["std_labor_cost"],
        v1["head"]["std_overhead_cost"], v1["head"]["std_total_cost"])
       == ("15.000000", "60.000000", "12.000000", "87.000000"),
       f"{v1['head']['std_total_cost']}")

    status, payload = client.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    ok("草稿卡不出现在期间视图（矩阵只含已发布）",
       status == 200 and len(items_of(payload)) == 0, f"rows={len(items_of(payload))}")

    status, payload = client.call("POST", f"/business/erp/cost-standard/{vid1}/publish")
    ok("提交发布 → PENDING", status == 200
       and data_of(payload)["head"]["publish_state"] == "PENDING", f"{status} {msg_of(payload)}")
    status, payload = client.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    ok("审批中也不出现在期间视图", status == 200 and len(items_of(payload)) == 0,
       f"rows={len(items_of(payload))}")

    publish_via_sql(vid1)
    status, payload = client.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    ok("只覆盖 12-01~12-15 的版本：期末（12-31）没人覆盖 ⇒ 矩阵仍 0 行（口径 = 期末取数）",
       status == 200 and len(items_of(payload)) == 0, f"rows={len(items_of(payload))}")

    v2 = create_version(client, mat_a, comp_id, V2_FROM, V2_TO, rate="35.000000",
                        remark="M518 smoke v2")
    vid2 = v2["head"]["id"]
    publish_via_sql(vid2)

    status, payload = client.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    rows = items_of(payload)
    row_a = next((r for r in rows if (r.get("head") or {}).get("material_id") == mat_a), {})
    head_a = row_a.get("head") or {}
    ok("期间内换版：该物料只出一行（期末那一版）", status == 200 and len(rows) == 1,
       f"rows={len(rows)}")
    ok("换版行标 switch_in_period = true 且版本号 = v2",
       row_a.get("switch_in_period") is True and head_a.get("version_no") == 2,
       f"switch={row_a.get('switch_in_period')} v={head_a.get('version_no')}")
    ok("换版行金额取 v2 口径（工 70 / 总 97）",
       head_a.get("std_labor_cost") == "70.000000" and head_a.get("std_total_cost") == "97.000000",
       f"{head_a.get('std_labor_cost')} / {head_a.get('std_total_cost')}")

    # 第二个物料：单一版本覆盖整期 ⇒ 一行 + switch=false
    v_b = create_version(client, mat_b, comp_id, WHOLE_FROM, WHOLE_TO, remark="M518 smoke whole")
    vid_b = v_b["head"]["id"]
    publish_via_sql(vid_b)

    status, payload = client.call("GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    rows = items_of(payload)
    row_b = next((r for r in rows if (r.get("head") or {}).get("material_id") == mat_b), {})
    ok("两行（一物料一行）", status == 200 and len(rows) == 2, f"rows={len(rows)}")
    ok("整期单版本 ⇒ switch_in_period = false",
       row_b.get("switch_in_period") is False, str(row_b.get("switch_in_period")))

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}&keyword={code_b}")
    ok("关键字收窄命中（按物料编码）", status == 200 and len(items_of(payload)) == 1,
       f"rows={len(items_of(payload))}")
    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}&keyword=ZZZ-NOPE-518")
    ok("关键字乱码 → 0 行", status == 200 and len(items_of(payload)) == 0,
       f"rows={len(items_of(payload))}")

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}&page=1&page_size=1")
    page = data_of(payload)
    ok("分页生效（page_size=1 回 1 行，total=2）",
       status == 200 and len(page.get("list") or []) == 1 and (page.get("total") or 0) == 2,
       f"list={len(page.get('list') or [])} total={page.get('total')}")
    return {"v1": vid1, "v2": vid2, "v_b": vid_b}


# ==================== ② 查生效版本 ====================

def check_effective(client: Client, mats: list[tuple[str, str]]) -> None:
    print("\n=== ② 查生效版本 GET /cost-standard/effective ===")
    (mat_a, _code_a), (_mat_b, _code_b), (mat_c, code_c) = mats

    status, payload = client.call("GET", f"/business/erp/cost-standard/effective?material_id={mat_a}")
    ok("期间 / 日期都不给 → 400 + 10001", status == 400 and code_of(payload) == 10001,
       f"{status} code={code_of(payload)}")

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/effective?material_id={mat_a}&period_code={PERIOD}")
    res = data_of(payload)
    version = res.get("version") or {}
    ok("按期间查 → 200 且按期末取数", status == 200 and res.get("on_date") == PERIOD_END,
       f"{status} on_date={res.get('on_date')}")
    ok("命中期末那一版 v2（已发布）",
       (version.get("head") or {}).get("version_no") == 2
       and (version.get("head") or {}).get("publish_state") == "PUBLISHED",
       str((version.get("head") or {}).get("version_no")))
    ok("期间内换版：switch_in_period=true + other_versions=[v1]",
       res.get("switch_in_period") is True
       and [(v or {}).get("version_no") for v in (res.get("other_versions") or [])] == [1],
       f"switch={res.get('switch_in_period')}")
    ok("口径说明 note 非空（页面原样展示）", bool(str(res.get("note") or "").strip()),
       str(res.get("note"))[:60])
    items = version.get("items") or []
    ok("详情带标准 BOM 明细（页面表格数据源）",
       len(items) == 1 and items[0].get("std_amount") == "15.000000",
       f"items={len(items)}")

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/effective?material_id={mat_a}&on_date={V1_FROM}")
    res = data_of(payload)
    ok("按日期查（12-01）→ 命中 v1（期内换版前的口径）", status == 200
       and ((res.get("version") or {}).get("head") or {}).get("version_no") == 1,
       f"{status} v={((res.get('version') or {}).get('head') or {}).get('version_no')}")

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/effective?material_id={mat_a}"
               "&period_code=2026-12&on_date=2026-11-20")
    ok("期间与日期互相矛盾（11-20 不在 2026-12 内）→ 400", status == 400,
       f"{status} code={code_of(payload)} {msg_of(payload)[:80]}")

    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/effective?material_id={mat_c}&period_code={PERIOD}")
    ok("没有可取数版本 → 404 + 10002（页面据此补「是不是还没发布」）",
       status == 404 and code_of(payload) == 10002, f"{status} code={code_of(payload)}")
    ok("404 报文带建卡引导（POST /business/erp/cost-standard）",
       "POST /business/erp/cost-standard" in msg_of(payload), msg_of(payload)[:90])
    print(f"      （对照物料 {code_c}：本脚本特意不建卡）")


# ==================== ③ 发布轴 ====================

def check_publish_axis(client: Client, mat_c: str, comp_id: str) -> str:
    print("\n=== ③ 发布轴（期间视图「提交发布 / 撤回」按钮的接口） ===")
    v3 = create_version(client, mat_c, comp_id, WHOLE_FROM, WHOLE_TO, remark="M518 smoke v3")
    vid3 = v3["head"]["id"]

    status, payload = client.call("POST", f"/business/erp/cost-standard/{vid3}/publish")
    ok("草稿 → 提交发布 = PENDING",
       status == 200 and data_of(payload)["head"]["publish_state"] == "PENDING",
       f"{status} {msg_of(payload)}")

    status, payload = client.call("POST", f"/business/erp/cost-standard/{vid3}/publish")
    ok("重复提交 → 409 + 66111", status == 409 and code_of(payload) == 66111,
       f"{status} code={code_of(payload)}")

    status, payload = client.call("PUT", f"/business/erp/cost-standard/{vid3}",
                                  {"std_labor_rate": "99.000000"})
    ok("审批中改口径 → 409 + 66111", status == 409 and code_of(payload) == 66111,
       f"{status} code={code_of(payload)}")

    status, payload = client.call("POST", f"/business/erp/cost-standard/{vid3}/publish-withdraw")
    ok("撤回 → 回到 DRAFT",
       status == 200 and data_of(payload)["head"]["publish_state"] == "DRAFT",
       f"{status} {msg_of(payload)}")

    status, payload = client.call("POST", f"/business/erp/cost-standard/{vid3}/publish-withdraw")
    ok("再撤回 → 409 + 66111", status == 409 and code_of(payload) == 66111,
       f"{status} code={code_of(payload)}")

    status, payload = client.call("PUT", f"/business/erp/cost-standard/{vid3}",
                                  {"std_labor_rate": "99.000000"})
    ok("撤回后草稿可继续改口径", status == 200
       and data_of(payload)["head"]["std_labor_rate"] == "99.000000", f"{status} {msg_of(payload)}")
    return vid3


# ==================== ④ 清场 ====================

def cleanup(client: Client, vids: list[str]) -> None:
    print("\n=== ④ 清场 ===")
    for vid in vids:
        status, payload = client.call("DELETE", f"/business/erp/cost-standard/{vid}")
        ok(f"删除 {vid[:8]}…（未来期间 → 允许）", status == 200, f"{status} {msg_of(payload)}")

    # 接口是**软删**（deleted_at 打标）：列表/取数立刻看不到，行还在 —— 先验这一点
    alive = one("select count(*) from biz_cost_standard where remark like 'M518 smoke%' "
                "and deleted_at is null")
    ok("软删口径：still-alive 行 = 0（列表与取数都看不到）", alive == "0", f"alive={alive}")
    status, payload = client.call(
        "GET", f"/business/erp/cost-standard/period-sheet?period_code={PERIOD}")
    ok("软删后期间视图清零（取数也看不到）",
       status == 200 and len(items_of(payload)) == 0, f"rows={len(items_of(payload))}")

    # 夹具彻底清掉（演练库不留垃圾）：明细 → 卡头
    q("delete from biz_cost_standard_item where standard_id in "
      "(select id from biz_cost_standard where remark like 'M518 smoke%')")
    q("delete from biz_cost_standard where remark like 'M518 smoke%'")
    left = one("select count(*) from biz_cost_standard where remark like 'M518 smoke%'")
    ok("库内零残留（含软删行）", left == "0", f"left={left}")


def main() -> int:
    mats, comp_id = fixture()
    client = Client()
    client.login(*LOGIN)
    print(f"登录 OK（{LOGIN[0]}）")

    vids = check_period_sheet(client, mats, comp_id)
    check_effective(client, mats)
    vid3 = check_publish_axis(client, mats[2][0], comp_id)
    cleanup(client, [vids["v1"], vids["v2"], vids["v_b"], vid3])

    print(f"\n===== {len(PASS)} PASS / {len(FAIL)} FAIL =====")
    for name in FAIL:
        print("  FAIL:", name)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
