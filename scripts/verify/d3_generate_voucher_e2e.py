#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""D3 真机验收：接管财务联动 GENERATE_VOUCHER（先抽公共函数 → SHADOW 并行对比 → TAKEOVER）。

跑法（隔离实例已在跑）：

    bash scripts/verify/d3_instance.sh start
    python3 scripts/verify/d3_generate_voucher_e2e.py

它要证明的事（每条都有 `[PASS]/[FAIL]` 与原始输出）：

  A. 环境：表/迁移到位、规则表初始 0 行、`GENERATE_VOUCHER` 已接管（配置写得进去）；
  B. **没有规则时行为逐字不变**：过账仍由旧路径生成 1 张凭证（基线快照）；
  C. **SHADOW 并行对比**：配置路径在 SAVEPOINT 里真跑一遍并回滚（库里不多凭证），
     与旧路径的凭证逐字段对比 `matched=true diffs=[]`，连凭证号都相同；
  D. **TAKEOVER 接管**：凭证由配置生成、旧路径不再生成（恰好 1 张），
     逐字段与基线一致（≥8 项断言）；
  E. 配置写脏（mode=TAKEOVER_XXX 直插库）→ ERROR + 跳过，凭证仍由旧逻辑发（不漏记）；
  F. 模板停用（template_code 指向不存在的 rule_code）→ 跳过 + 旧逻辑照发；
  G. **关键动作失败**（期间已结账）→ 整笔回滚：单据仍 DRAFT、无凭证、无应付、无流水；
  H. 收尾：删规则后表回到 0 行。

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
from decimal import Decimal

BASE = os.environ.get("D3_BASE", "http://127.0.0.1:18096")
DB = os.environ.get("D3_DB", "cloudpivot_d3")
LOG = os.environ.get("D3_LOG", "/root/d3-e2e-server.log")
PW_FILE = "/root/d3-admin-password.txt"
PREFIX = "D3E2E-"

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
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("PGPASSWORD", "alun")
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
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, _json(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, _json(err.read().decode())


def login():
    candidates = []
    if os.environ.get("D3_ADMIN_PASSWORD"):
        candidates.append(os.environ["D3_ADMIN_PASSWORD"])
    if os.path.exists(PW_FILE):
        candidates.append(open(PW_FILE).read().strip())
    candidates += ["Admin@123456D3!", "Admin@123456"]
    psql("UPDATE sys_user SET login_fail_count = 0, is_locked = false WHERE username = 'admin'")

    for password in candidates:
        code, body = http(
            "POST", "/api/system/auth/login", {"username": "admin", "password": password}
        )
        if code != 200:
            continue
        data = body.get("data") or {}
        token = data.get("access_token")
        if not token:
            continue
        if data.get("must_change_password"):
            new_password = "Admin@123456D3!"
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
                "POST",
                "/api/system/auth/login",
                {"username": "admin", "password": new_password},
            )
            token = (body.get("data") or {}).get("access_token")
            if not token:
                raise RuntimeError(f"改密后重登失败：{code} {body}")
        with open(PW_FILE, "w") as fh:
            fh.write(password if not data.get("must_change_password") else "Admin@123456D3!")
        return token
    raise RuntimeError("admin 登录失败（候选口令全部不通）")


# ---------------- 主数据（迁移种好的标准数据，按编码取） ----------------


def master_ids():
    material = psql("SELECT id FROM biz_material WHERE material_code = 'M-W1A-001' LIMIT 1")
    warehouse = psql("SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH01' LIMIT 1")
    supplier = psql("SELECT id FROM biz_supplier WHERE supplier_code = 'SUP-0001' LIMIT 1")
    if not (material and warehouse and supplier):
        raise RuntimeError(f"标准主数据缺失：{material=} {warehouse=} {supplier=}")
    return material, warehouse, supplier


MATERIAL, WAREHOUSE, SUPPLIER = "", "", ""


def create_stock_in(tag, in_date=None):
    """建一张收货单（1 行：10 @ 10 元，13% → 100 + 13 = 113）。"""
    body = {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "is_sourceless": True,
        "remark": PREFIX + tag,
        "invoice_received": False,
        "items": [
            {
                "material_id": MATERIAL,
                "qty": "10",
                "price_excl": "10",
                "tax_rate": "13",
            }
        ],
    }
    if in_date:
        body["in_date"] = in_date
    code, resp = http("POST", "/api/business/erp/stock-in", body, token=TOKEN)
    if code not in (200, 201):
        raise RuntimeError(f"建收货单失败：{code} {resp}")
    data = resp.get("data") or {}
    header = data.get("header") or data
    return header["id"], header["in_no"]


def post_stock_in(doc_id):
    code, body = http(
        "PATCH", f"/api/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"}, token=TOKEN
    )
    return code, body


def create_stock_in_multi(tag, items):
    """建一张**多物料**收货单（P2-1b：按去重物料拆分录）。items = [(material_id, qty, price, rate)]。"""
    body = {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "is_sourceless": True,
        "remark": PREFIX + tag,
        "invoice_received": False,
        "items": [
            {"material_id": m, "qty": q, "price_excl": p, "tax_rate": r} for m, q, p, r in items
        ],
    }
    code, resp = http("POST", "/api/business/erp/stock-in", body, token=TOKEN)
    if code not in (200, 201):
        raise RuntimeError(f"建多物料收货单失败：{code} {resp}")
    data = resp.get("data") or {}
    header = data.get("header") or data
    return header["id"], header["in_no"]


# ---------------- 凭证快照（逐字段） ----------------

VOUCHER_SQL = """
SELECT coalesce(string_agg(
    v.voucher_word || '|' || v.voucher_no || '|' || v.voucher_type || '|' || v.source_type || '|' ||
    v.period || '|' || v.voucher_date || '|' || v.total_debit || '|' || v.total_credit || '|' ||
    v.entry_count || '|' || v.is_red || '|' || v.status || '|' || v.summary, E'\\n' ORDER BY v.created_at), '')
FROM biz_voucher v
WHERE v.source_type = 'STOCK_IN' AND v.source_id = '{doc}' AND v.deleted_at IS NULL
"""

ENTRY_SQL = """
SELECT coalesce(string_agg(
    e.line_no || ':' || e.account_code || ':' || e.debit_amount || ':' || e.credit_amount
    || ':' || coalesce(e.aux_material_id::text, '-') || ':' || coalesce(e.aux_warehouse_id::text, '-')
    || ':' || coalesce(e.aux_supplier_id::text, '-'), ',' ORDER BY e.line_no), '')
FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
WHERE v.source_type = 'STOCK_IN' AND v.source_id = '{doc}'
"""


def voucher_rows(doc_id):
    out = []
    for line in psql(VOUCHER_SQL.format(doc=doc_id)).splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        out.append(
            {
                "voucher_word": parts[0],
                "voucher_no": parts[1],
                "voucher_type": parts[2],
                "source_type": parts[3],
                "period": parts[4],
                "voucher_date": parts[5],
                "total_debit": parts[6],
                "total_credit": parts[7],
                "entry_count": parts[8],
                "is_red": parts[9],
                "status": parts[10],
                "summary": "|".join(parts[11:]),
            }
        )
    return out


def entries_of(doc_id):
    raw = psql(ENTRY_SQL.format(doc=doc_id))
    return [item for item in raw.split(",") if item]


def voucher_count(doc_id):
    return int(psql(f"SELECT count(*) FROM biz_voucher WHERE source_id = '{doc_id}'") or 0)


# ---------------- 规则 ----------------


def upsert_rule(name, params, entity="STOCK_IN", status="POSTED"):
    psql(
        "INSERT INTO biz_flow_linkage_rule "
        "(id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq, "
        " created_at, updated_at) "
        f"VALUES (gen_random_uuid(), '{name}', '{entity}', '{status}', 'GENERATE_VOUCHER', "
        f"'{json.dumps(params)}'::jsonb, true, 10, now(), now()) "
        "ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE "
        "SET action_params = EXCLUDED.action_params, enabled = true, name = EXCLUDED.name, "
        "    updated_at = now(), deleted_at = NULL"
    )


def drop_rules():
    psql("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'D3E2E-%'")


def rule_rows():
    return int(psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE deleted_at IS NULL") or 0)


ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def log_size():
    return os.path.getsize(LOG) if os.path.exists(LOG) else 0


def log_tail_since(offset):
    """读日志增量（**必须剥 ANSI**：`matched=true` 实际是 `matched=\\x1b[32mtrue`）。"""
    if not os.path.exists(LOG):
        return ""
    with open(LOG, "r", errors="replace") as fh:
        fh.seek(offset)
        return ANSI_RE.sub("", fh.read())


def line_with(log, needle):
    for line in log.splitlines():
        if needle in line:
            return line.strip()
    return ""


def field(line, key):
    match = re.search(rf"\b{key}=(\"[^\"]*\"|\S+)", line)
    return match.group(1).strip('"') if match else ""


def reset_business_data():
    """把隔离库的业务数据清空（本脚本可重复跑，且跑完后能在同一个库上跑不变量）。

    ⚠️ 有三条硬护栏，别在别的地方抄这段：
    1. **只允许**库名以 `cloudpivot_d3` 开头（本卡的隔离库）；
    2. 只 TRUNCATE 业务表，不碰 sys_user / 主数据 / 迁移历史 / 规则配置；
    3. 期间表按「2026 全年 OPEN」补齐（凭证期间必须有期间记录，否则不变量 F2 会红）。

    为什么必须清干净：上一轮留下的「已过账单据」如果只删凭证不删库存，
    库存模块与总账 1405 会立刻对不上（B2 变 BLOCKER）——那是夹具脏，不是账错。
    """
    if not DB.startswith("cloudpivot_d3"):
        raise RuntimeError(f"拒绝在非隔离库上跑：DB={DB}（必须 cloudpivot_d3*）")
    psql(
        "TRUNCATE biz_voucher_entry, biz_voucher, biz_payment, biz_payable, "
        "biz_inventory_txn, biz_inventory_location, biz_inventory, biz_material_serial, "
        "biz_status_log, biz_flow_task, biz_flow_instance, biz_flow_linkage_rule, "
        "biz_stock_in CASCADE"
    )
    for month in range(1, 13):
        start = f"2026-{month:02d}-01"
        # 月末用日期运算算（别在 Python 里手写 30/31）
        end = (
            f"2026-{month + 1:02d}-01" if month < 12 else "2027-01-01"
        )
        psql(
            "INSERT INTO biz_accounting_period "
            "(period_year, period_month, period_code, start_date, end_date, status) "
            f"VALUES (2026, {month}, '2026-{month:02d}', '{start}', "
            f"('{end}'::date - 1), 'OPEN') "
            "ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'"
        )


def main():
    global TOKEN, MATERIAL, WAREHOUSE, SUPPLIER

    print("== D3 财务联动 GENERATE_VOUCHER 接管 · 真机验收 ==")

    # ---------- A. 环境 ----------
    exists = psql("SELECT count(*) FROM pg_tables WHERE tablename = 'biz_flow_linkage_rule'")
    expect(exists == "1", "A1 表 biz_flow_linkage_rule 已建", f"pg_tables={exists}")
    version = psql("SELECT max(version::int) FROM schema_migrations")
    expect(int(version) >= 205, "A2 迁移已应用到 V0205（本卡不改 schema）", f"max(version)={version}")

    TOKEN = login()
    ok("A3 admin 登录成功")
    MATERIAL, WAREHOUSE, SUPPLIER = master_ids()
    ok("A4 标准主数据就位（物料 / 仓库 / 供应商）")

    drop_rules()
    reset_business_data()
    expect(rule_rows() == 0, "A5 初始 0 条规则（空表 = 行为不变）", f"rules={rule_rows()}")

    # ---------- B. 基线：无规则 → 旧路径生成 1 张凭证 ----------
    b_id, b_no = create_stock_in("基线-无规则")
    code, body = post_stock_in(b_id)
    expect(code == 200, "B1 无规则时过账成功（旧路径）", f"code={code} {body.get('msg', '')}")
    b_rows = voucher_rows(b_id)
    expect(len(b_rows) == 1, "B2 旧路径生成恰好 1 张凭证", f"rows={len(b_rows)}")
    base = b_rows[0] if b_rows else {}
    expect(base.get("entry_count") == "3", "B3 采购入库凭证 3 行（存货 / 进项税 / 应付）",
           base.get("entry_count", ""))
    expect(base.get("total_debit") == base.get("total_credit") == "113.0000",
           "B4 借贷平衡且 = 价税合计 113", f"{base.get('total_debit')}/{base.get('total_credit')}")
    expect(base.get("period", "").startswith("2026-"), "B5 会计期间取自凭证日期", base.get("period", ""))
    base_entries = entries_of(b_id)
    expect(len(base_entries) == 3, "B6 分录 3 行落库", str(base_entries))

    # ---------- C. SHADOW：并行对比 ----------
    upsert_rule("D3E2E-GEN-VOUCHER", {"biz_type": "PURCHASE_IN", "mode": "SHADOW"})
    expect(rule_rows() == 1, "C1 SHADOW 规则已写入（能存）")
    saved = psql(
        "SELECT trigger_entity || '/' || trigger_status || '/' || action_code || '/' || action_params::text "
        "FROM biz_flow_linkage_rule WHERE name = 'D3E2E-GEN-VOUCHER'"
    )
    expect("STOCK_IN/POSTED/GENERATE_VOUCHER" in saved, "C2 规则能读回来", saved)

    c_id, c_no = create_stock_in("影子-对比")
    offset = log_size()
    code, body = post_stock_in(c_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code == 200, "C3 影子模式过账成功", f"code={code} {body.get('msg', '')}")
    expect(voucher_count(c_id) == 1, "C4 影子模式仍只有 1 张凭证（试算已回滚，不双发）",
           f"count={voucher_count(c_id)}")
    expect("联动凭证影子试算完成" in log, "C5 日志出现「影子试算完成（已回滚）」")
    shadow_line = line_with(log, "联动凭证影子对比")
    expect(bool(shadow_line), "C6 日志出现「联动凭证影子对比」", shadow_line[:160])
    expect("matched=true" in log, "C7 逐字段对比一致（matched=true）", field(shadow_line, "matched"))
    expect("diffs=[]" in log.replace(" ", ""), "C8 差异列表为空（diffs=[]）",
           field(shadow_line, "diffs"))
    expect(
        field(shadow_line, "configured_voucher_no") == field(shadow_line, "legacy_voucher_no")
        and field(shadow_line, "legacy_voucher_no") != "",
        "C9 影子凭证号 == 旧路径凭证号（同一张单、同一个号段位置）",
        f"{field(shadow_line, 'configured_voucher_no')} / {field(shadow_line, 'legacy_voucher_no')}",
    )
    c_rows = voucher_rows(c_id)
    c_row = c_rows[0] if c_rows else {}
    expect(c_row.get("voucher_no") == field(shadow_line, "legacy_voucher_no"),
           "C10 库里那张凭证就是对比用的旧路径凭证", c_row.get("voucher_no", ""))
    expect(field(shadow_line, "field_count") == "15", "C11 对比覆盖 15 个字段（≥8 项）",
           field(shadow_line, "field_count"))
    expect(c_row.get("total_debit") == base.get("total_debit")
           and c_row.get("entry_count") == base.get("entry_count"),
           "C12 SHADOW 阶段业务行为不变（金额 / 行数与基线一致）",
           f"{c_row.get('total_debit')} / {c_row.get('entry_count')}")

    # ---------- C+. 多物料单的 SHADOW 一致性（P2-1b：拆分只写在共享 builder 里） ----------
    # 规则仍是 SHADOW：配置路径与旧路径各按「逐物料拆分录」跑一遍，必须逐行一致 ——
    # 这条断言就是「谁在 handler 里另写一套拆分录，D3 立刻红」的护栏。
    material2 = psql("SELECT id FROM biz_material WHERE material_code = 'M-W1A-002' LIMIT 1")
    expect(bool(material2), "C13 第二个物料就位（多物料用例的夹具）", material2)
    cm_id, cm_no = create_stock_in_multi(
        "影子-多物料",
        [(MATERIAL, "10", "10", "13"), (material2, "20", "5", "13")],
    )
    offset = log_size()
    code, body = post_stock_in(cm_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code == 200, "C14 多物料单过账成功（SHADOW）", f"code={code} {body.get('msg', '')}")
    cm_rows = voucher_rows(cm_id)
    cm_row = cm_rows[0] if cm_rows else {}
    cm_entries = entries_of(cm_id)
    header_total = psql(f"SELECT total_amount FROM biz_stock_in WHERE id = '{cm_id}'")
    expect(cm_row.get("entry_count") == "4", "C15 多物料含税 ⇒ 凭证 4 行（M + 2）",
           cm_row.get("entry_count", ""))
    expect(len(cm_entries) == 4, "C16 分录 4 行落库", str(cm_entries))
    inv = [e for e in cm_entries if e.split(":")[1] == "1405"]
    expect(len(inv) == 2, "C17 存货行 2 条（每个去重物料一行）", str(inv))
    expect([e.split(":")[4] for e in inv] == [MATERIAL, material2],
           "C18 存货行逐行带物料且按 material_id 升序", str([e.split(":")[4] for e in inv]))
    inv_sum = sum(Decimal(e.split(":")[2]) for e in inv)
    expect(str(inv_sum) == str(Decimal(header_total)), "C19 Σ 1405 分录 = 单头 total_amount",
           f"{inv_sum} vs {header_total}")
    cm_shadow = line_with(log, "联动凭证影子对比")
    expect(bool(cm_shadow) and field(cm_shadow, "doc_no") == cm_no,
           "C20 多物料单产生了影子对比日志", cm_shadow[:190])
    expect(field(cm_shadow, "matched") == "true" and field(cm_shadow, "diffs") == "[]",
           "C21 多物料：配置路径 vs 旧路径逐行一致（matched=true diffs=[]）",
           f"matched={field(cm_shadow, 'matched')} diffs={field(cm_shadow, 'diffs')}")
    expect(field(cm_shadow, "configured_voucher_no") == field(cm_shadow, "legacy_voucher_no")
           and field(cm_shadow, "legacy_voucher_no") != "",
           "C22 多物料：两条路的凭证号也相同（同一号段位置）",
           f"{field(cm_shadow, 'configured_voucher_no')} / {field(cm_shadow, 'legacy_voucher_no')}")

    # ---------- D. TAKEOVER：配置接管 ----------
    upsert_rule("D3E2E-GEN-VOUCHER", {"biz_type": "PURCHASE_IN", "mode": "TAKEOVER"})
    d_id, d_no = create_stock_in("接管-配置发")
    offset = log_size()
    code, body = post_stock_in(d_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code == 200, "D1 接管后过账成功", f"code={code} {body.get('msg', '')}")
    d_rows = voucher_rows(d_id)
    expect(len(d_rows) == 1, "D2 接管后恰好 1 张凭证（旧路径没有双发）", f"rows={len(d_rows)}")
    taken = line_with(log, "凭证已由联动配置接管")
    expect(bool(taken), "D3 日志写明「收货过账的凭证已由联动配置接管」", taken[:160])
    expect("联动已接管凭证：配置生成" in log, "D4 日志写明「配置生成（旧逻辑本次不再生成）」")
    d_row = d_rows[0] if d_rows else {}
    d_entries = entries_of(d_id)

    # 逐字段对比（换成「同内容单据」后应逐字一致；只有凭证号 / 单据号 / 摘要里的单号会变）
    def norm(text, doc_no):
        return text.replace(doc_no, "<NO>").replace(" ", "")

    expect(d_row.get("voucher_word") == base.get("voucher_word"), "D5 凭证字一致",
           d_row.get("voucher_word", ""))
    expect(
        re.fullmatch(r"PZ-\d{8}-\d{6}", d_row.get("voucher_no", "")) is not None,
        "D6 凭证号仍是 PZ-yyyyMMdd-NNNNNN 号段", d_row.get("voucher_no", ""),
    )
    expect(d_row.get("total_debit") == base.get("total_debit"), "D7 借方合计一致",
           d_row.get("total_debit", ""))
    expect(d_row.get("total_credit") == base.get("total_credit"), "D8 贷方合计一致",
           d_row.get("total_credit", ""))
    expect(d_row.get("entry_count") == base.get("entry_count"), "D9 分录行数一致",
           d_row.get("entry_count", ""))
    expect(d_row.get("period") == base.get("period"), "D10 会计期间一致", d_row.get("period", ""))
    expect(d_row.get("source_type") == base.get("source_type") == "STOCK_IN", "D11 关联单据类型一致",
           d_row.get("source_type", ""))
    expect(
        psql(f"SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id = '{d_id}'") == "1",
        "D12 凭证关联的正是这张收货单",
        d_id,
    )
    expect([e.split(":")[1] for e in d_entries] == [e.split(":")[1] for e in base_entries],
           "D13 逐行科目编码一致（借存货 / 借进项税 / 贷应付）", str([e.split(":")[1] for e in d_entries]))
    expect([e.split(":")[2] for e in d_entries] == [e.split(":")[2] for e in base_entries],
           "D14 逐行借方金额一致", str([e.split(":")[2] for e in d_entries]))
    expect([e.split(":")[3] for e in d_entries] == [e.split(":")[3] for e in base_entries],
           "D15 逐行贷方金额一致", str([e.split(":")[3] for e in d_entries]))
    expect(
        norm(d_row.get("summary", ""), d_no) == norm(base.get("summary", ""), b_no),
        "D16 摘要同一套模板（采购入库 <单号> 价税合计 113）",
        f"{d_row.get('summary')} vs {base.get('summary')}",
    )
    expect(d_row.get("status") == "POSTED" and d_row.get("is_red") == "0",
           "D17 接管生成的凭证状态 POSTED、非红字", f"{d_row.get('status')}/{d_row.get('is_red')}")

    # ---------- E. 配置写脏 → 跳过 + 旧逻辑照发 ----------
    upsert_rule("D3E2E-GEN-VOUCHER", {"biz_type": "PURCHASE_IN", "mode": "TAKEOVER_XXX"})
    e_id, e_no = create_stock_in("脏配置")
    offset = log_size()
    code, body = post_stock_in(e_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code == 200, "E1 配置写脏时过账仍成功（不被配置搞死）", f"code={code} {body.get('msg', '')}")
    expect(voucher_count(e_id) == 1, "E2 凭证仍由旧逻辑发出（不漏记）", f"count={voucher_count(e_id)}")
    expect("参数非法" in log, "E3 日志 ERROR 点名参数非法（mode 只允许 SHADOW / TAKEOVER）")

    # ---------- F. 模板停用 → 跳过 + 旧逻辑照发 ----------
    upsert_rule(
        "D3E2E-GEN-VOUCHER",
        {"biz_type": "PURCHASE_IN", "template_code": "NO_SUCH_TEMPLATE", "mode": "TAKEOVER"},
    )
    f_id, f_no = create_stock_in("模板停用")
    offset = log_size()
    code, body = post_stock_in(f_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code == 200, "F1 模板停用时过账仍成功", f"code={code} {body.get('msg', '')}")
    expect(voucher_count(f_id) == 1, "F2 凭证仍由旧逻辑发出", f"count={voucher_count(f_id)}")
    expect("未启用" in log, "F3 日志点名「模板未启用」（target 不在 → 跳过）")

    # ---------- G. 关键动作失败（期间已结账）→ 整笔回滚 ----------
    # 专用期间：2031-03 钉成 CLOSED（不碰真实月份）
    psql(
        "INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status) "
        "VALUES (2031, 3, '2031-03', '2031-03-01', '2031-03-31', 'CLOSED') "
        "ON CONFLICT (period_code) DO UPDATE SET status = 'CLOSED'"
    )
    # 对照组（G0）：**没有规则**时同一张单也会因为期间已结账失败——
    # 先记下它落在什么状态，后面才能证明「接管后失败」没有把单据留成别的样子。
    drop_rules()
    g0_id, g0_no = create_stock_in("期间已结账-对照", in_date="2031-03-15")
    code0, body0 = post_stock_in(g0_id)
    g0_status = psql(f"SELECT status FROM biz_stock_in WHERE id = '{g0_id}'")
    expect(
        code0 != 200 and g0_status != "POSTED" and voucher_count(g0_id) == 0,
        "G0 对照组：无规则时期间已结账同样失败、同样没凭证（既有行为，非 D3 引入）",
        f"code={code0} status={g0_status}",
    )

    upsert_rule("D3E2E-GEN-VOUCHER", {"biz_type": "PURCHASE_IN", "mode": "TAKEOVER"})
    g_id, g_no = create_stock_in("期间已结账", in_date="2031-03-15")
    offset = log_size()
    code, body = post_stock_in(g_id)
    time.sleep(0.5)
    log = log_tail_since(offset)
    expect(code != 200, "G1 期间已结账 → 过账失败（关键动作失败）", f"code={code}")
    expect("已结账" in (body.get("msg") or ""), "G2 错误文案点名「已结账」", (body.get("msg") or "")[:120])
    expect("整笔回滚" in log, "G3 日志写明关键动作失败 → 整笔回滚")
    g_status = psql(f"SELECT status FROM biz_stock_in WHERE id = '{g_id}'")
    g_posted_at = psql(f"SELECT coalesce(posted_at::text, '') FROM biz_stock_in WHERE id = '{g_id}'")
    expect(
        g_status == g0_status and g_posted_at == "",
        "G4 接管后失败与对照组落同一个状态（没有把单据留成别的样子）、posted_at 仍为空",
        f"status={g_status}（对照 {g0_status}） posted_at='{g_posted_at}'",
    )
    expect(voucher_count(g_id) == 0, "G5 没有留下任何凭证（不许半张）", f"count={voucher_count(g_id)}")
    payable = psql(f"SELECT count(*) FROM biz_payable WHERE source_id = '{g_id}'")
    expect(payable == "0", "G6 没有留下应付单", f"count={payable}")
    txn = psql(f"SELECT count(*) FROM biz_inventory_txn WHERE doc_id = '{g_id}'")
    expect(txn == "0", "G7 没有留下库存流水（库存没动）", f"count={txn}")
    expect(psql(f"SELECT voucher_id IS NULL FROM biz_stock_in WHERE id = '{g_id}'") == "t",
           "G8 收货单的 voucher_id 仍为空")
    psql("DELETE FROM biz_accounting_period WHERE period_code = '2031-03'")

    # ---------- H. 收尾 ----------
    drop_rules()
    expect(rule_rows() == 0, "H1 删规则后表回到 0 行（配置可删可复活）", f"rules={rule_rows()}")

    print(f"==> 汇总：PASS={PASS} FAIL={FAIL}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:  # noqa: BLE001 —— 验收脚本要把异常当 FAIL 打出来
        print(f"[FAIL] 脚本异常：{error}")
        print(f"==> 汇总：PASS={PASS} FAIL={FAIL + 1}")
        sys.exit(1)
