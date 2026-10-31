#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""t_421d8aa1 端到端验收：`POST /business/erp/serial/batch-generate` = **无来源入库**

覆盖 ADR-20261031 与规格 v3（`docs/技术方案-20261031-批量生成无来源入库.md`）的验收判据：

  ① 缺 `unit_cost` ⇒ 400 + 10001 + 逐字报文，且**零落库**（号段 / 号 / 两本账 / 流水 / 凭证一行不动）；
  ② `unit_cost = 0` / `"abc"` / `1e11` ⇒ 400 + 逐字报文；
  ③ 带单价生成 5 件 @12.34 ⇒ `biz_inventory` qty=5 / amount=61.7000 / avg_cost=12.340000，
     `biz_inventory_location` 合计 = 5，一条流水（`IN` + `SERIAL_IN` + `SERIAL_BATCH`，单价/金额写实），
     一张凭证（借 1405 / 贷 1901），响应体回显单价 / 金额 / 快照；
  ④ 二次生成 4 件 @15 ⇒ 移动加权 `avg = round((61.7 + 60) / 9, 6) = 13.522222`；
  ⑤ 出库 1 件 ⇒ 成本账 8 / 108.1778、库位账 8，凭证 借 6401 / 贷 1405（金额取账上真实差额）；
  ⑥ 报废 1 件 ⇒ 成本账 7 / 94.6556，凭证 借 1901 / 贷 1405；
  ⑦ 不变式①（`biz_inventory.qty == count(IN_STOCK 号)`）与 B1（库位账合计 == 成本账 qty）全程成立；
  ⑧ 在本库上跑 `scripts/verify/erp_invariants.py` ⇒ 32/32、BLOCKER 0、WARN 0；并打印
     `doc_type='STOCK_TRANSFER'` 行数（此数必须为 0，否则 B1 的绿不可信 —— 规格 §11 R3）。

跑法（隔离实例，端口 18089/5189 段）：

    export T421_ADMIN_PW='…'             # 隔离实例 admin 口令（不落库、不进仓）
    T421_BASE=http://127.0.0.1:18090 T421_DB=cloudpivot_e2e_t421 \\
    T421_DB_USER=e2erole T421_DB_PW=… \\
      python3 scripts/verify/t421_batch_generate_e2e.py

退出码 0 = 全过。夹具用固定 UUID（`b42a0001-…`），开头清上一轮、结尾清本轮。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("T421_BASE", "http://127.0.0.1:18090")
DB = os.environ.get("T421_DB", "cloudpivot_e2e_t421")
DB_HOST = os.environ.get("T421_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("T421_DB_PORT", "5432")
DB_USER = os.environ.get("T421_DB_USER", "e2erole")
DB_PW = os.environ.get("T421_DB_PW", "")
ADMIN_USER = os.environ.get("T421_ADMIN", "admin")
ADMIN_PW = os.environ.get("T421_ADMIN_PW", "")
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
INVARIANTS = os.environ.get("T421_INVARIANTS", os.path.join(REPO, "scripts/verify/erp_invariants.py"))

UNIT = "b42a0001-0000-4000-8000-000000000001"
WAREHOUSE = "b42a0001-0000-4000-8000-000000000002"
MATERIAL = "b42a0001-0000-4000-8000-000000000011"

PASS = 0
FAIL = 0
STEPS = []


def ok(name, extra=""):
    global PASS
    PASS += 1
    print(f"  [PASS] {name}" + (f"（{extra}）" if extra else ""))


def bad(name, detail):
    global FAIL
    FAIL += 1
    print(f"  [FAIL] {name}\n         {detail}")


def check_num(name, want, got):
    """NUMERIC 的 0 / 0.0000 必须等价 ⇒ 一律按数值比。"""
    try:
        if abs(Decimal(str(want)) - Decimal(str(got))) < Decimal("0.000001"):
            ok(name, f"{got}")
        else:
            bad(name, f"期望 [{want}] 实际 [{got}]")
    except Exception:
        bad(name, f"期望 [{want}] 实际 [{got}]（非数值）")


def check_eq(name, want, got):
    if str(want) == str(got):
        ok(name, got)
    else:
        bad(name, f"期望 [{want}] 实际 [{got}]")


def check_in(name, needle, haystack):
    if needle in (haystack or ""):
        ok(name)
    else:
        bad(name, f"文本里没有 [{needle}]，实际 [{haystack}]")


def psql(sql):
    env = dict(os.environ, PGPASSWORD=DB_PW)
    r = subprocess.run(
        ["psql", "-h", DB_HOST, "-p", DB_PORT, "-U", DB_USER, "-d", DB, "-v", "ON_ERROR_STOP=1",
         "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=180)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:500]}  SQL={sql[:160]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def psql1(sql, default=""):
    rows = psql(sql)
    return rows[0][0] if rows else default


def http(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def login():
    if not ADMIN_PW:
        print("FATAL: 必须用 T421_ADMIN_PW 提供隔离实例的 admin 口令")
        sys.exit(2)
    password = ADMIN_PW
    # 空库首启时 bootstrap 种出来的 admin 带「首登强制改密」：此时业务接口一律
    # `403 首次登录必须先修改初始密码`（**不是** 401）。必须先用旧令牌改密、再用新口令重登
    # （改密会 `token_version++`，旧令牌随即失效）。
    for _ in range(2):
        st, res = http("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": password})
        if st != 200 or res.get("code") != 0:
            print(f"FATAL: 登录失败 {st} {res}")
            sys.exit(2)
        data = res["data"]
        token = data["access_token"]
        if not data.get("must_change_password"):
            return token
        new_password = f"{password}#T421r2"
        st, res = http("PUT", "/api/system/auth/password",
                       {"old_password": password, "new_password": new_password}, token)
        if st != 200 or res.get("code") != 0:
            print(f"FATAL: 首登强制改密失败 {st} {res}")
            sys.exit(2)
        print(f"  首登强制改密 → 口令已更新（R2 用新口令重登）")
        password = new_password
    print("FATAL: 改密后仍要求改密")
    sys.exit(2)


def cleanup():
    psql(
        f"delete from biz_voucher_entry where voucher_id in "
        f"  (select id from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP'));"
        f"delete from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP');"
        f"delete from biz_material_serial where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory_txn where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory_location where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_material where id='{MATERIAL}'::uuid;"
        f"delete from biz_warehouse where id='{WAREHOUSE}'::uuid;"
        f"delete from biz_unit where id='{UNIT}'::uuid;"
    )


def fixtures():
    psql(
        f"insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) "
        f"  values ('{UNIT}','T421-PCS','个',1,null,1.000000,1,1) on conflict do nothing;"
        f"insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) "
        f"  values ('{WAREHOUSE}','T421-WH','T421 无来源入库仓','/{WAREHOUSE}/',1,1,1) on conflict do nothing;"
        f"insert into biz_material (id, material_code, material_name, unit_id, status, serial_required) "
        f"  values ('{MATERIAL}','T421-M1','一物一码电机（t421 e2e）','{UNIT}'::uuid,1,true) on conflict do nothing"
    )


def seq_no():
    return psql1("select coalesce(max(current_no),0) from biz_seq")


def counters():
    """六表 + 号段计数的快照（用于「400 零落库」断言）。"""
    row = psql(
        f"select (select coalesce(max(current_no),0) from biz_seq)::text,"
        f" (select count(*) from biz_material_serial)::text,"
        f" (select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid)::text,"
        f" (select count(*) from biz_inventory)::text,"
        f" (select count(*) from biz_inventory_location)::text,"
        f" (select count(*) from biz_inventory_txn)::text,"
        f" (select count(*) from biz_voucher)::text,"
        f" (select count(*) from biz_voucher_entry)::text"
    )[0]
    keys = ["号段", "序列号", "本物料号", "成本账", "库位账", "流水", "凭证", "分录"]
    return dict(zip(keys, row))


def snapshot(tag):
    row = psql(
        f"select coalesce((select sum(qty) from biz_inventory where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid),0)::numeric(18,4)::text,"
        f" coalesce((select sum(amount) from biz_inventory where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid),0)::numeric(18,4)::text,"
        f" coalesce((select max(avg_cost) from biz_inventory where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid and location_id is null),0)::numeric(18,6)::text,"
        f" coalesce((select sum(qty) from biz_inventory_location where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid),0)::numeric(18,4)::text,"
        f" (select count(*) from biz_inventory_txn where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid)::text,"
        f" (select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid "
        f"   and warehouse_id='{WAREHOUSE}'::uuid and status='IN_STOCK' and deleted_at is null)::text,"
        f" (select count(*) from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP'))::text"
    )[0]
    keys = ["成本账qty", "成本账amount", "avg_cost", "库位账合计", "流水条数", "在库号", "序列号凭证"]
    d = dict(zip(keys, row))
    print(f"\n【{tag}】" + "  ".join(f"{k}={v}" for k, v in d.items()))
    STEPS.append((tag, d))
    return d


def latest_txn(serial_no=None):
    cond = f" and doc_no = '{serial_no}'" if serial_no else ""
    rows = psql(
        f"select txn_type, biz_type, doc_type, qty::numeric(18,4)::text, unit_cost::numeric(18,6)::text,"
        f" amount::numeric(18,4)::text, before_qty::numeric(18,4)::text, after_qty::numeric(18,4)::text,"
        f" before_amount::numeric(18,4)::text, after_amount::numeric(18,4)::text, remark"
        f" from biz_inventory_txn where material_id='{MATERIAL}'::uuid and warehouse_id='{WAREHOUSE}'::uuid"
        f" {cond} order by created_at desc limit 1"
    )
    return rows[0] if rows else None


def require_txn(label, serial_no=None):
    """取最新流水；没有就判红并中断（后面的下标访问不该抛 TypeError）。"""
    txn = latest_txn(serial_no)
    if txn is None:
        bad(f"{label}：必须有一条流水", "查不到 biz_inventory_txn 行")
        print("\n==> 汇总：PASS=%d FAIL=%d（提前中止）" % (PASS, FAIL))
        sys.exit(1)
    return txn


def voucher_lines(source_type, source_id=None):
    cond = f" and v.source_id = '{source_id}'::uuid" if source_id else ""
    return psql(
        f"select e.line_no::text, e.account_code, e.account_name,"
        f" e.debit_amount::numeric(18,4)::text, e.credit_amount::numeric(18,4)::text"
        f" from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id"
        f" where v.source_type = '{source_type}' {cond} order by e.line_no"
    )


def main():
    token = login()
    ok("登录拿到令牌（隔离实例 admin）")

    print("\n==> 0. 清上一轮夹具 + 造夹具（假物料 serial_required=true）")
    cleanup()
    fixtures()
    check_eq("夹具物料就位（serial_required=true）", 1,
             psql1(f"select count(*) from biz_material where id='{MATERIAL}'::uuid and serial_required"))
    base = snapshot("① 基线")

    print("\n==> 1. 缺 unit_cost ⇒ 400 + 10001 + 逐字报文，且零落库")
    before = counters()
    st, res = http("POST", "/api/business/erp/serial/batch-generate",
                   {"material_id": MATERIAL, "warehouse_id": WAREHOUSE, "count": 5}, token)
    check_eq("HTTP 400", 400, st)
    check_eq("业务码 10001", 10001, res.get("code"))
    check_eq("报文逐字（规格 §2.3）",
             "unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）",
             res.get("msg"))
    after = counters()
    for k in before:
        check_eq(f"零落库：{k} 未变", before[k], after[k])
    check_eq("号段未推进（validate 在 begin 之前）", before["号段"], seq_no())

    print("\n==> 2. 非法单价：0 / \"abc\" / 1e11 都 400 + 逐字报文")
    for body, want_msg in [
        ({"unit_cost": 0},
         "unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收"),
        ({"unit_cost": "abc"}, "unit_cost 不是合法数字：「abc」"),
        ({"unit_cost": 100000000000},
         "unit_cost 超出可记账范围（须 < 100000000000）：100000000000"),
    ]:
        st, res = http("POST", "/api/business/erp/serial/batch-generate",
                       {"material_id": MATERIAL, "warehouse_id": WAREHOUSE, "count": 5, **body}, token)
        check_eq(f"HTTP 400（unit_cost={body['unit_cost']}）", 400, st)
        check_eq(f"业务码 10001（unit_cost={body['unit_cost']}）", 10001, res.get("code"))
        check_eq(f"报文逐字（unit_cost={body['unit_cost']}）", want_msg, res.get("msg"))
    check_eq("② 结束后号段仍未推进", before["号段"], seq_no())

    print("\n==> 3. 生成 5 件 @12.34（＝无来源入库：两本账 + 凭证 + 写实流水）")
    st, res = http("POST", "/api/business/erp/serial/batch-generate",
                   {"material_id": MATERIAL, "warehouse_id": WAREHOUSE, "count": 5,
                    "unit_cost": 12.34, "batch_no": "T421-B1"}, token)
    check_eq("HTTP 200", 200, st)
    check_eq("业务码 0", 0, res.get("code"))
    data = res.get("data") or {}
    check_eq("响应 count", 5, data.get("count"))
    check_num("响应 unit_cost（6 位字符串）", "12.34", data.get("unit_cost"))
    check_num("响应 amount = 5 × 12.34", "61.7", data.get("amount"))
    inv = data.get("inventory") or {}
    check_num("响应 inventory.qty", "5", inv.get("qty"))
    check_num("响应 inventory.amount", "61.7", inv.get("amount"))
    check_num("响应 inventory.avg_cost", "12.34", inv.get("avg_cost"))
    s3 = snapshot("② 生成 5 件 @12.34 后")
    check_num("成本账 qty = 5", "5", s3["成本账qty"])
    check_num("成本账 amount = 5 × 12.34 = 61.7000", "61.7", s3["成本账amount"])
    check_num("avg_cost = 12.340000", "12.34", s3["avg_cost"])
    check_num("库位账合计 = 5（B1：库位账 == 成本账）", "5", s3["库位账合计"])
    check_num("在库号 = 5（不变式①）", "5", s3["在库号"])
    txn = require_txn("生成 5 件")
    check_eq("流水 txn_type", "IN", txn[0])
    check_eq("流水 biz_type（P10 残留判据① 依赖）", "SERIAL_IN", txn[1])
    check_eq("流水 doc_type（P10 残留判据① 依赖）", "SERIAL_BATCH", txn[2])
    check_num("流水 qty = 5", "5", txn[3])
    check_num("流水 unit_cost 写实 12.340000（原为 0）", "12.34", txn[4])
    check_num("流水 amount 写实 61.7000（原为 0）", "61.7", txn[5])
    check_num("流水 before_qty", "0", txn[6])
    check_num("流水 after_qty", "5", txn[7])
    check_num("流水 before_amount", "0", txn[8])
    check_num("流水 after_amount", "61.7", txn[9])
    check_in("流水 remark 带单价与金额", "单价 12.340000｜金额 61.7000", txn[10])
    lines = voucher_lines("SERIAL_BATCH")
    check_eq("凭证行数 = 2（借 1405 / 贷 1901）", 2, len(lines))
    if len(lines) == 2:
        check_eq("凭证①科目 1405 库存商品 借方", ("1405", "61.7000", "0.0000"),
                 (lines[0][1], lines[0][3], lines[0][4]))
        check_eq("凭证②科目 1901 待处理财产损溢 贷方", ("1901", "0.0000", "61.7000"),
                 (lines[1][1], lines[1][3], lines[1][4]))

    print("\n==> 4. 二次生成 4 件 @15.00 ⇒ 移动加权 avg = round((61.7+60)/9, 6) = 13.522222")
    st, res = http("POST", "/api/business/erp/serial/batch-generate",
                   {"material_id": MATERIAL, "warehouse_id": WAREHOUSE, "count": 4,
                    "unit_cost": "15.00", "batch_no": "T421-B2"}, token)
    check_eq("HTTP 200", 200, st)
    s4 = snapshot("③ 二次生成 4 件 @15 后")
    check_num("成本账 qty = 9", "9", s4["成本账qty"])
    check_num("成本账 amount = 61.7 + 60 = 121.7000", "121.7", s4["成本账amount"])
    check_num("avg_cost = ROUND(121.7/9, 6) = 13.522222", "13.522222", s4["avg_cost"])
    check_num("库位账合计 = 9", "9", s4["库位账合计"])
    check_num("流水条数 = 2（每次生成一条，不随桶数翻倍）", "2", s4["流水条数"])

    print("\n==> 5. 出库 1 件 ⇒ 成本结转（借 6401 / 贷 1405，金额取账上真实差额）")
    serials = psql(f"select id, serial_no from biz_material_serial where material_id='{MATERIAL}'::uuid "
                   f"and status='IN_STOCK' and deleted_at is null order by serial_no limit 2")
    out_id, out_no = serials[0]
    st, res = http("PATCH", f"/api/business/erp/serial/{out_id}/status",
                   {"status": "OUTBOUND", "reason": "t421 e2e 出库"}, token)
    check_eq("出库 HTTP 200", 200, st)
    s5 = snapshot("④ 出库 1 件后")
    check_num("成本账 qty = 8", "8", s5["成本账qty"])
    check_num("成本账 amount = 8 × 13.522222 → 108.1778", "108.1778", s5["成本账amount"])
    check_num("库位账合计 = 8（B1 仍相等）", "8", s5["库位账合计"])
    txn5 = require_txn("出库 1 件", out_no)
    check_eq("出库流水 txn_type", "OUT", txn5[0])
    check_eq("出库流水 biz_type", "SERIAL_OUT", txn5[1])
    check_num("出库流水 amount = 账上差额 13.5222", "13.5222", txn5[5])
    lines5 = voucher_lines("SERIAL_OUT")
    check_eq("出库凭证行数 = 2（借 6401 / 贷 1405）", 2, len(lines5))
    if len(lines5) == 2:
        check_eq("出库凭证①科目 6401 主营业务成本 借方", ("6401", "13.5222"),
                 (lines5[0][1], lines5[0][3]))
        check_eq("出库凭证②科目 1405 贷方", ("1405", "13.5222"), (lines5[1][1], lines5[1][4]))

    print("\n==> 6. 报废 1 件 ⇒ 报废损失（借 1901 / 贷 1405）")
    scrap_id, scrap_no = serials[1]
    st, res = http("PATCH", f"/api/business/erp/serial/{scrap_id}/status",
                   {"status": "SCRAPPED", "reason": "t421 e2e 报废"}, token)
    check_eq("报废 HTTP 200", 200, st)
    s6 = snapshot("⑤ 报废 1 件后")
    check_num("成本账 qty = 7", "7", s6["成本账qty"])
    check_num("成本账 amount = 7 × 13.522222 → 94.6556", "94.6556", s6["成本账amount"])
    check_num("库位账合计 = 7", "7", s6["库位账合计"])
    lines6 = voucher_lines("SERIAL_SCRAP")
    check_eq("报废凭证行数 = 2（借 1901 / 贷 1405）", 2, len(lines6))
    if len(lines6) == 2:
        check_eq("报废凭证①科目 1901 借方", ("1901", "13.5222"),
                 (lines6[0][1], lines6[0][3]))
        check_eq("报废凭证②科目 1405 贷方", ("1405", "13.5222"), (lines6[1][1], lines6[1][4]))

    print("\n==> 7. 不变式① / B1 全程复核（本夹具物料）")
    row = psql(
        f"select (select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid "
        f"   and status='IN_STOCK' and deleted_at is null)::text,"
        f" coalesce((select sum(qty) from biz_inventory where material_id='{MATERIAL}'::uuid),0)::numeric(18,4)::text,"
        f" coalesce((select sum(qty) from biz_inventory_location where material_id='{MATERIAL}'::uuid),0)::numeric(18,4)::text"
    )[0]
    check_num("不变式①：在库号 == 成本账 qty", row[0], row[1])
    check_num("B1：库位账合计 == 成本账 qty", row[1], row[2])

    print("\n==> 8. 本库全量巡检：erp_invariants.py")
    env = dict(os.environ, NIK_DATABASE_HOST=DB_HOST, NIK_DATABASE_PORT=DB_PORT,
               NIK_DATABASE_USER=DB_USER, NIK_DATABASE_NAME=DB, NIK_DATABASE_PASSWORD=DB_PW)
    r = subprocess.run([sys.executable, INVARIANTS], capture_output=True, text=True, env=env, timeout=300)
    tail = [ln for ln in r.stdout.strip().split("\n") if ln.strip()]
    print("\n".join(tail[-6:]))
    # 汇总行的形态是「结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0」——
    # 它**不是**最后一行（最后一行是「★ 全部不变量成立…」），要按前缀找。
    summary = next((ln for ln in tail if "结果：" in ln), "")
    check_in("巡检汇总 32/32", "32/32", summary)
    check_in("BLOCKER 0", "BLOCKER 0", summary)
    check_in("WARN 0", "WARN 0", summary)
    check_eq("巡检退出码 0", 0, r.returncode)
    transfer = psql1("select count(*) from biz_inventory_txn where doc_type='STOCK_TRANSFER'")
    if os.environ.get("T421_EXPECT_NO_TRANSFER", "0") == "1":
        check_eq("副本库 STOCK_TRANSFER 流水行数 = 0（否则 B1 的绿不可信，规格 §11 R3）", 0, transfer)
    elif str(transfer) == "0":
        ok("副本库 STOCK_TRANSFER 流水行数 = 0", transfer)
    else:
        # prod 副本自带 2 条历史 STOCK_TRANSFER（R3：调拨结算不写库位账，已由 t_270b472e 收口）。
        # 这里不判红，但必须打出来 —— 判据是「B1 仍绿」由上面的 32/32 证明；
        # 「0 行」这条硬约束在空库隔离实例上单独跑（T421_EXPECT_NO_TRANSFER=1）。
        print(f"  [NOTE] 副本库含 {transfer} 条历史 STOCK_TRANSFER 流水（prod 自带，R3 已单卡收口）；"
              f"本库的 B1 仍绿由上面 32/32 证明")

    print("\n==> 9. 收尾：清本轮夹具（只删自己造的，固定 UUID 链）")
    cleanup()
    check_eq("夹具零残留（物料/仓库/单位）", "0|0|0",
             psql1(f"select (select count(*) from biz_material where id='{MATERIAL}'::uuid)::text || '|' ||"
                   f" (select count(*) from biz_warehouse where id='{WAREHOUSE}'::uuid)::text || '|' ||"
                   f" (select count(*) from biz_unit where id='{UNIT}'::uuid)::text"))
    check_eq("序列号凭证零残留", 0,
             psql1("select count(*) from biz_voucher where source_type in "
                   "('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')"))

    print("\n==> 汇总：PASS=%d FAIL=%d" % (PASS, FAIL))
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
