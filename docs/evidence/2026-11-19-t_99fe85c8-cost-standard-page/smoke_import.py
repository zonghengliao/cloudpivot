#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-09 标准成本维护页 · HTTP 冒烟（真接口 + 真库，逐条断言）。

跑法（实例已起在旁边）：
    NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m509_mig \
    COST_STD_BASE=http://127.0.0.1:18259 \
      python3 docs/evidence/2026-11-19-t_99fe85c8-cost-standard-page/smoke_import.py

覆盖（每条打 PASS/FAIL，最后 summary + 退出码）：
  ① 模板下载：200 + xlsx（PK 魔数 / 体积）
  ② 批量导入成功：2 组 3 行 → 2 版；金额与 M5-08 口径一致；明细行号与物料编码
  ③ 重复导入：整批拒绝（期间已被占用），库里不多行
  ④ 坏行（物料不存在）：好的一组也不入库（全有或全无）
  ⑤ 组内卡头不一致：整批拒绝，点名「标准工时」列
  ⑥ 明细半填（填了耗用物料、单价留空）：整批拒绝，点名「标准单价」列
  ⑦ 结构错：非 xlsx → 4xx；缺 file 字段 → 400
  ⑧ 维护页 CRUD（页面按钮的同一批接口）：建 / 改 / 失效 / 生效 / 删
  ⑨ 已生效版本禁删（409 + 66110）
  ⑩ 清场零残留

本脚本**只用标准库**（urllib + zipfile）：宿主机没有 openpyxl，所以自己写一份最小 xlsx
（inline string 单元格，sheet 名「数据」—— 与后端 `SheetReader` 的取表口径一致）。
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
import zipfile
from datetime import date, timedelta

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "172.18.10.145"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "fbb1263691ff75c31ec99eaf258138b4"),
    name=os.environ.get("NIK_DATABASE_NAME", "cp_m509_mig"),
)
BASE = os.environ.get("COST_STD_BASE", "http://127.0.0.1:18259") + "/api"
ADMIN = ("admin", os.environ.get("COST_STD_ADMIN_PASSWORD", "Admin@123456"))
TAG = f"M509SM{int(time.time()) % 100000}"

# 9 列表头（与后端 `cost_standard_import::COLUMNS` 逐字一致；必填列不带 `*` 也能匹配）
HEADERS = [
    "物料编码",
    "期间",
    "标准工时",
    "标准工资率",
    "标准制造费用",
    "耗用物料编码",
    "标准用量",
    "标准单价",
    "备注",
]

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


def month_window(day: date) -> tuple[str, str, str]:
    start = day.replace(day=1)
    nxt = (start + timedelta(days=32)).replace(day=1)
    return start.strftime("%Y-%m"), start.isoformat(), (nxt - timedelta(days=1)).isoformat()


def _col(index: int) -> str:
    name = ""
    index += 1
    while index:
        index, rem = divmod(index - 1, 26)
        name = chr(65 + rem) + name
    return name


def build_xlsx(rows: list[list[str]], sheet_name: str = "数据") -> bytes:
    """最小可用 xlsx（inlineStr 单元格）。calamine 读它和读真实模板一样。"""
    def esc(text: str) -> str:
        return (text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
                    .replace('"', "&quot;"))

    body = [f'<row r="1">']
    for col, head in enumerate(HEADERS):
        body.append(f'<c r="{_col(col)}1" t="inlineStr"><is><t>{esc(head)}</t></is></c>')
    body.append("</row>")
    for row_index, row in enumerate(rows, start=2):
        body.append(f'<row r="{row_index}">')
        for col, value in enumerate(row):
            if value == "":
                continue
            body.append(f'<c r="{_col(col)}{row_index}" t="inlineStr"><is><t>{esc(value)}</t></is></c>')
        body.append("</row>")

    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        '<sheetData>' + "".join(body) + "</sheetData></worksheet>"
    )
    workbook = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        f'<sheets><sheet name="{esc(sheet_name)}" sheetId="1" r:id="rId1"/></sheets></workbook>'
    )
    rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
        'Target="worksheets/sheet1.xml"/></Relationships>'
    )
    root_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" '
        'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
        'Target="xl/workbook.xml"/></Relationships>'
    )
    content_types = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" '
        'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" '
        'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        "</Types>"
    )
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", content_types)
        zf.writestr("_rels/.rels", root_rels)
        zf.writestr("xl/workbook.xml", workbook)
        zf.writestr("xl/_rels/workbook.xml.rels", rels)
        zf.writestr("xl/worksheets/sheet1.xml", sheet)
    return buffer.getvalue()


# ==================== HTTP ====================

class Client:
    def __init__(self) -> None:
        self.token = ""

    def _send(self, req: urllib.request.Request):
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                raw = resp.read()
                ctype = resp.headers.get("Content-Type", "")
                if "application/json" in ctype:
                    return resp.status, json.loads(raw.decode() or "{}"), raw
                return resp.status, {"_raw_len": len(raw)}, raw
        except urllib.error.HTTPError as err:
            raw = err.read()
            try:
                payload = json.loads(raw.decode() or "{}")
            except Exception:  # noqa: BLE001
                payload = {"_raw": raw[:200].decode(errors="replace")}
            return err.code, payload, raw

    def call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            BASE + path, data=data, method=method,
            headers={"Content-Type": "application/json",
                     **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        status, payload, raw = self._send(req)
        return status, payload

    def raw(self, method: str, path: str):
        req = urllib.request.Request(
            BASE + path, method=method,
            headers={"Authorization": f"Bearer {self.token}"} if self.token else {})
        return self._send(req)

    def upload(self, path: str, file_bytes: bytes, filename: str = "标准成本.xlsx", field: str = "file"):
        boundary = f"----m509{uuid.uuid4().hex}"
        parts = []
        parts.append(f"--{boundary}\r\n".encode())
        parts.append(
            f'Content-Disposition: form-data; name="{field}"; filename="{filename}"\r\n'.encode())
        parts.append(b"Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n")
        parts.append(file_bytes)
        parts.append(f"\r\n--{boundary}--\r\n".encode())
        data = b"".join(parts)
        req = urllib.request.Request(
            BASE + path, data=data, method="POST",
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}",
                     **({"Authorization": f"Bearer {self.token}"} if self.token else {})})
        status, payload, raw = self._send(req)
        return status, payload

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
    if not isinstance(payload, dict):
        return str(payload)[:200]
    return f"code={payload.get('code')} msg={payload.get('msg')}"


# ==================== 夹具 ====================

def setup() -> dict:
    q("update sys_user set must_change_password = false where username = 'admin'")
    q("update sys_config set config_value = 'true' where config_key = 'erp.cost.enabled'")
    ok("erp.cost.enabled 已置 true", one("select config_value from sys_config where config_key='erp.cost.enabled'") == "true")
    cli = Client()
    cli.login(*ADMIN)
    status, payload = cli.call("POST", "/support/config/reload")
    ok("参数重载 200（模块开关立刻生效）", status == 200, err_of(payload))

    q(f"delete from biz_cost_standard where remark like '{TAG}%'")
    q(f"delete from biz_material where material_code like '{TAG}%'")

    ids = {}
    for kind, mtype in (("FG", "FINISHED"), ("FS", "FINISHED"), ("FG2", "FINISHED"),
                        ("FG3", "FINISHED"), ("RA", "RAW"), ("RB", "RAW")):
        q(f"""insert into biz_material (id, material_code, material_name, spec, unit_id,
                                       material_type, status, remark)
              select gen_random_uuid(), '{TAG}-{kind}', '{TAG} 夹具 {kind}', 'M5-09',
                     (select id from biz_unit where deleted_at is null order by unit_code limit 1),
                     '{mtype}', 1, '{TAG}'""")
        ids[kind] = one(f"select id from biz_material where material_code = '{TAG}-{kind}'")
    ok("六个夹具物料已建（4 成品 + 2 耗用）", all(ids.values()), f"{len([v for v in ids.values() if v])} 个")
    for mid in ids.values():
        if mid:
            q(f"delete from biz_cost_standard where material_id = '{mid}'::uuid")
    return {"cli": cli, **ids}


def row(material: str, period: str, component: str = "", qty: str = "", price: str = "",
        hours: str = "2.5", rate: str = "31.111111", overhead: str = "12.345678",
        remark: str = "") -> list[str]:
    return [material, period, hours, rate, overhead, component, qty, price, f"{TAG} {remark}".strip()]


def main() -> int:
    fx = setup()
    cli: Client = fx["cli"]
    fg, fs, fg2, fg3, ra, rb = (fx["FG"], fx["FS"], fx["FG2"], fx["FG3"], fx["RA"], fx["RB"])
    today = date.today()
    p1, p1_start, p1_end = month_window(today + timedelta(days=30))
    p2, p2_start, p2_end = month_window(today + timedelta(days=62))
    code = lambda key: f"{TAG}-{key}"  # noqa: E731

    # ---------- ① 模板下载 ----------
    status, payload, raw = cli.raw("GET", "/business/erp/cost-standard/import-template")
    ok("① 模板下载 200", status == 200, err_of(payload))
    ok("① 模板是 xlsx（PK 魔数）", raw[:2] == b"PK", raw[:4].hex())
    ok("① 模板体积合理（>3KB，含三个 sheet）", len(raw) > 3000, f"{len(raw)} B")

    # ---------- ② 批量导入成功：2 组 3 行 ----------
    rows = [
        row(code("FG"), p1, code("RA"), "3", "5.5", remark="导入第1行"),
        row(code("FG"), p1, code("RB"), "2", "0.333333", remark="导入第1行"),
        row(code("FS"), p1, remark="无BOM的一版"),
    ]
    status, payload = cli.upload("/business/erp/cost-standard/import", build_xlsx(rows))
    res = data_of(payload)
    ok("② 导入 200", status == 200, err_of(payload))
    ok("② 3 行全部入库", res.get("success") == 3 and res.get("failed") == 0, json.dumps(res, ensure_ascii=False)[:200])
    ok("② 新建 2 版（两组）", res.get("versions_created") == 2, str(res.get("versions_created")))
    ok("② 无逐行错误", res.get("rows") == [], str(res.get("rows"))[:200])

    status, payload = cli.call("GET", f"/business/erp/cost-standard?material_id={fg}")
    page = data_of(payload)
    head = (page.get("list") or [{}])[0]
    imported_id = head.get("id", "")
    ok("② 列表能查到 FG 这一版", page.get("total") == 1, str(page.get("total")))
    ok("② 版本号从 1 起", head.get("version_no") == 1, str(head.get("version_no")))
    ok("② 料 = Σ明细（17.166666）", head.get("std_material_cost") == "17.166666", str(head.get("std_material_cost")))
    ok("② 工 = round(工时×费率,6)（77.777778）", head.get("std_labor_cost") == "77.777778", str(head.get("std_labor_cost")))
    ok("② 总 = 料+工+费（107.290122）", head.get("std_total_cost") == "107.290122", str(head.get("std_total_cost")))
    ok("② 生效区间 = 整期", head.get("effective_from") == p1_start and head.get("effective_to") == p1_end,
       f"{head.get('effective_from')} ~ {head.get('effective_to')}")
    ok("② 状态默认生效", head.get("status") == 1)
    ok("② 明细 2 行", head.get("items_count") == 2, str(head.get("items_count")))
    ok("② 备注落库（带 TAG）", (head.get("remark") or "").startswith(TAG), str(head.get("remark")))

    status, payload = cli.call("GET", f"/business/erp/cost-standard/{head.get('id')}")
    detail = data_of(payload)
    items = detail.get("items") or []
    ok("② 详情明细 2 行且行号 1/2", [it.get("line_no") for it in items] == [1, 2], str([it.get("line_no") for it in items]))
    ok("② 明细带耗用物料编码", [it.get("component_material_code") for it in items] == [code("RA"), code("RB")],
       str([it.get("component_material_code") for it in items]))
    ok("② 明细金额（16.500000 / 0.666666）",
       [it.get("std_amount") for it in items] == ["16.500000", "0.666666"],
       str([it.get("std_amount") for it in items]))

    head_fs = one(f"select count(*) from biz_cost_standard where material_id='{fs}'::uuid and deleted_at is null")
    ok("② 无 BOM 的组也建出来了", head_fs == "1", head_fs)

    # ---------- ③ 重复导入：整批拒绝 ----------
    status, payload = cli.upload("/business/erp/cost-standard/import", build_xlsx(rows))
    res = data_of(payload)
    ok("③ 重复导入 200（业务拒绝不是 HTTP 错）", status == 200, err_of(payload))
    ok("③ 一条都没入库", res.get("success") == 0 and res.get("versions_created") == 0, str(res.get("success")))
    ok("③ 三行都被点名（整批拒绝）", res.get("failed") == 3, str(res.get("failed")))
    msgs = " ".join(issue.get("message", "") for issue in res.get("rows") or [])
    ok("③ 报文指出「已经有启用版本」", "已经有启用版本" in msgs, msgs[:160])
    ok("③ 报文给出三条出路", "三条出路" in msgs, msgs[:80])
    ok("③ 库里仍只有 1 版", one(f"select count(*) from biz_cost_standard where material_id='{fg}'::uuid and deleted_at is null") == "1")

    # ---------- ④ 坏行：好的一组也不入库 ----------
    bad_rows = [
        row(code("FG3"), p1, remark="好的一组"),
        row(f"{TAG}-NOT-EXIST", p1, remark="坏的一组"),
    ]
    status, payload = cli.upload("/business/erp/cost-standard/import", build_xlsx(bad_rows))
    res = data_of(payload)
    ok("④ 坏行导入：success=0", res.get("success") == 0, str(res.get("success")))
    ok("④ 只报第 3 行有错", [issue.get("line") for issue in res.get("rows") or []] == [3],
       str([issue.get("line") for issue in res.get("rows") or []]))
    ok("④ 列名 = 物料编码", (res.get("rows") or [{}])[0].get("column") == "物料编码",
       str((res.get("rows") or [{}])[0].get("column")))
    ok("④ 报文说「不存在」", "不存在" in ((res.get("rows") or [{}])[0].get("message") or ""), "")
    ok("④ 好的一组也一条没入库（全有或全无）",
       one(f"select count(*) from biz_cost_standard where material_id='{fg3}'::uuid and deleted_at is null") == "0")

    # ---------- ⑤ 组内卡头不一致 ----------
    mismatch = [
        row(code("FG3"), p1, code("RA"), "1", "2", rate="31.111111", remark="卡头A"),
        row(code("FG3"), p1, code("RB"), "1", "2", rate="50", remark="卡头A"),
    ]
    status, payload = cli.upload("/business/erp/cost-standard/import", build_xlsx(mismatch))
    res = data_of(payload)
    ok("⑤ 卡头不一致：整批拒", res.get("success") == 0 and res.get("failed") == 1, str(res.get("failed")))
    ok("⑤ 列名 = 标准工时", (res.get("rows") or [{}])[0].get("column") == "标准工时",
       str((res.get("rows") or [{}])[0].get("column")))
    ok("⑤ 报文说「必须逐字一致」", "必须逐字一致" in ((res.get("rows") or [{}])[0].get("message") or ""), "")
    ok("⑤ 库里零行", one(f"select count(*) from biz_cost_standard where material_id='{fg3}'::uuid and deleted_at is null") == "0")

    # ---------- ⑥ 明细半填 ----------
    half = [row(code("FG3"), p1, code("RA"), "1", "", remark="单价留空")]
    status, payload = cli.upload("/business/erp/cost-standard/import", build_xlsx(half))
    res = data_of(payload)
    ok("⑥ 明细半填：整批拒", res.get("success") == 0 and res.get("failed") == 1, str(res.get("failed")))
    ok("⑥ 列名 = 标准单价", (res.get("rows") or [{}])[0].get("column") == "标准单价",
       str((res.get("rows") or [{}])[0].get("column")))

    # ---------- ⑦ 结构错 ----------
    status, payload = cli.upload("/business/erp/cost-standard/import", b"this is not a xlsx at all")
    ok("⑦ 非 xlsx → 4xx", 400 <= status < 500, f"{status} {err_of(payload)}")
    status, payload = cli.upload("/business/erp/cost-standard/import", b"", field="wrong_field")
    ok("⑦ 缺 file 字段 → 400", status == 400, f"{status} {err_of(payload)}")

    # ---------- ⑧ 维护页 CRUD（页面按钮的同一批接口） ----------
    body = {
        "material_id": fg2,
        "period_code": p2,
        "std_labor_hours": "1.5",
        "std_labor_rate": "20",
        "std_overhead_cost": "5",
        "items": [{"component_material_id": ra, "std_qty": "2", "std_price": "3"}],
        "remark": f"{TAG} 页面新建",
    }
    status, payload = cli.call("POST", "/business/erp/cost-standard", body)
    created = data_of(payload).get("head") or {}
    ok("⑧ 页面「新增版本」200", status == 200, err_of(payload))
    ok("⑧ 建出来的料 = 6 / 工 = 30 / 总 = 41",
       (created.get("std_material_cost"), created.get("std_labor_cost"), created.get("std_total_cost"))
       == ("6.000000", "30.000000", "41.000000"),
       f"{created.get('std_material_cost')}/{created.get('std_labor_cost')}/{created.get('std_total_cost')}")
    new_id = created.get("id", "")

    status, payload = cli.call("PUT", f"/business/erp/cost-standard/{new_id}",
                              {"std_labor_rate": "25", "remark": f"{TAG} 页面改过"})
    updated = data_of(payload).get("head") or {}
    ok("⑧ 页面「编辑」200（工 30 → 37.5）", status == 200 and updated.get("std_labor_cost") == "37.500000",
       f"{status} {updated.get('std_labor_cost')}")

    status, payload = cli.call("POST", f"/business/erp/cost-standard/{new_id}/status",
                              {"status": 0, "reason": "页面失效"})
    ok("⑧ 页面「失效」200", status == 200, err_of(payload))
    ok("⑧ 失效后带 warnings（界面要提示后果）", len(data_of(payload).get("warnings") or []) > 0,
       str(data_of(payload).get("warnings"))[:160])
    ok("⑧ 失效留痕进备注", "失效" in ((data_of(payload).get("version") or {}).get("head") or {}).get("remark", ""),
       str(((data_of(payload).get("version") or {}).get("head") or {}).get("remark"))[:120])

    status, payload = cli.call("POST", f"/business/erp/cost-standard/{new_id}/status", {"status": 1})
    ok("⑧ 页面「再生效」200", status == 200, err_of(payload))

    status, payload = cli.call("DELETE", f"/business/erp/cost-standard/{new_id}")
    ok("⑧ 页面「删除」未生效过的版本 200", status == 200, err_of(payload))
    status, payload = cli.call("GET", f"/business/erp/cost-standard/{new_id}")
    ok("⑧ 删后详情 404", status == 404, f"{status} {err_of(payload)}")

    # ---------- ⑨ 已生效版本禁删（66110） ----------
    # 「已生效过」的判据是 `effective_from < 今天`，所以要用**过去期间**的版本才算数；
    # 导入那一版落在将来（下个月），本来就允许删（它不是任何历史差异的凭据）。
    p_past, past_start, past_end = month_window(today - timedelta(days=40))
    status, payload = cli.call("POST", "/business/erp/cost-standard", {
        "material_id": fg3,
        "period_code": p_past,
        "items": [],
        "remark": f"{TAG} 过去期间",
    })
    past = data_of(payload).get("head") or {}
    ok("⑨ 过去期间的版本建得出来（已生效过）", status == 200 and past.get("effective_from") == past_start,
       f"{status} {past.get('effective_from')}")
    status, payload = cli.call("DELETE", f"/business/erp/cost-standard/{past.get('id')}")
    ok("⑨ 删已生效过的版本 → 409 + 66110",
       status == 409 and payload.get("code") == 66110, f"{status} {err_of(payload)}")
    ok("⑨ 报文指向「改成失效」这条正路", "失效" in (payload.get("msg") or ""), (payload.get("msg") or "")[:120])
    ok("⑨ 那一版还在（没被删）",
       one(f"select count(*) from biz_cost_standard where material_id='{fg3}'::uuid and deleted_at is null") == "1")

    # 将来期间的版本可以删（不是历史凭据）
    status, payload = cli.call("DELETE", f"/business/erp/cost-standard/{imported_id}")
    ok("⑨ 删将来期间的版本 200（未生效过不算历史凭据）", status == 200, f"{status} {err_of(payload)}")

    # ---------- ⑩ 清场 ----------
    for mid in (fg, fs, fg2, fg3, ra, rb):
        q(f"delete from biz_cost_standard where material_id = '{mid}'::uuid")
    q(f"delete from biz_material where material_code like '{TAG}%'")
    left = one(f"select count(*) from biz_cost_standard where remark like '{TAG}%'")
    ok("⑩ 清场零残留", left == "0", left)

    print()
    print(f"summary: PASS={len(PASS)} FAIL={len(FAIL)}")
    if FAIL:
        print("失败项：")
        for name in FAIL:
            print(f"  - {name}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
