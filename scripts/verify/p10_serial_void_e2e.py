#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P10 端到端验收：作废「一物一码」收货单 → 序列号被退回（真实 HTTP + 真库）

覆盖（对应 P10 的 cancel_in_tx 第 ② 步 void_serials_of）：
  ① 建单（3 件一物一码，客户端贴号）→ POSTED → 3 个号存活 IN_STOCK、库存 +3
  ② 作废（POSTED → CANCELLED）→ 3 个号全部**软删**（deleted_at 置位，行保留）、库存回基线
  ③ 同号重发：软删后部分唯一索引放行 → 第二张单用**同样 3 个号**能建成并过账
     （证明退回是「真释放」，不是靠物理删解决的）
  ④ 已出库 → 拒绝作废 409：把某个号改成 OUTBOUND 后作废必须 409，文案带
     「不能作废」+ 具体 SN +「请先做退货入库冲销」，且整单回滚（单据仍 POSTED、三账未动）；
     号改回 IN_STOCK 后作废成功
  ⑤ 收尾：夹具零残留（序列号/流水/库位账/成本账/收货单/凭证/应付/物料/供应商/仓库）

跑法（144 上）：
  set -a; . /etc/cloudpivot/cloudpivot.env; set +a
  export P10_BASE=http://127.0.0.1:8023  P10_DB=cloudpivot
  python3 scripts/verify/p10_serial_void_e2e.py
退出码 0 = 全过。脚本幂等：开头按固定夹具 UUID 清上一轮。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P10_BASE", "http://127.0.0.1:8023")
DB = os.environ.get("P10_DB", "cloudpivot")
DBU = os.environ.get("P10_DB_USER", "cloudpivot")
PW = os.environ["NIK_DATABASE_PASSWORD"]
ADMIN_USER = os.environ.get("P10_ADMIN", "admin")
ADMIN_PW = os.environ.get("P10_ADMIN_PW") or os.environ.get("NIK_ADMIN_PASSWORD", "")

UNIT = "e2a10001-0000-4000-8000-000000000001"
SUPPLIER = "e2a10001-0000-4000-8000-000000000002"
WAREHOUSE = "e2a10001-0000-4000-8000-000000000003"
MATERIAL = "e2a10001-0000-4000-8000-000000000011"
SN = ["P10E2E-SN-0001", "P10E2E-SN-0002", "P10E2E-SN-0003"]

PASS = 0
FAIL = 0


def ok(name, extra=""):
    global PASS
    PASS += 1
    print(f"  [PASS] {name}" + (f"（{extra}）" if extra else ""))


def bad(name, detail):
    global FAIL
    FAIL += 1
    print(f"  [FAIL] {name}\n         {detail}")


def check(name, want, got):
    if str(want) == str(got):
        ok(name, got)
        return
    # NUMERIC(18,4) 的 `0` 与 `0.0000` 必须等价：两边都可解析成数就按数值比
    try:
        if abs(float(want) - float(got)) < 1e-9:
            ok(name, got)
            return
    except (TypeError, ValueError):
        pass
    bad(name, f"期望 [{want}] 实际 [{got}]")


def check_num(name, want, got):
    """数值比较（NUMERIC(18,4) 的 0 与 0.0000 必须等价）"""
    try:
        if abs(float(want) - float(got)) < 1e-9:
            ok(name, got)
        else:
            bad(name, f"期望 [{want}] 实际 [{got}]")
    except (TypeError, ValueError):
        bad(name, f"期望 [{want}] 实际 [{got}]（非数值）")


def check_in(name, needle, haystack):
    if needle in (haystack or ""):
        ok(name)
    else:
        bad(name, f"文本里没有 [{needle}]，实际 [{haystack}]")


def psql(sql):
    env = dict(os.environ, PGPASSWORD=PW)
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", DBU, "-d", DB, "-v", "ON_ERROR_STOP=1",
         "-tA", "-F", "\x1f", "-c", sql],
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


def inv_qty():
    return psql1(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{MATERIAL}'::uuid "
                 f"and warehouse_id='{WAREHOUSE}'::uuid")


def alive_serials():
    return psql1(f"select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid "
                 f"and deleted_at is null")


def snap(tag):
    row = psql(f"select "
               f"coalesce((select sum(qty) from biz_inventory where material_id='{MATERIAL}'::uuid "
               f"  and warehouse_id='{WAREHOUSE}'::uuid),0)::text,"
               f"coalesce((select sum(amount) from biz_inventory where material_id='{MATERIAL}'::uuid "
               f"  and warehouse_id='{WAREHOUSE}'::uuid),0)::text,"
               f"coalesce((select sum(qty) from biz_inventory_location where material_id='{MATERIAL}'::uuid "
               f"  and warehouse_id='{WAREHOUSE}'::uuid),0)::text,"
               f"(select count(*) from biz_inventory_txn where material_id='{MATERIAL}'::uuid "
               f"  and warehouse_id='{WAREHOUSE}'::uuid)::text,"
               f"coalesce((select sum(case when txn_type='IN' then qty else -qty end) "
               f"  from biz_inventory_txn where material_id='{MATERIAL}'::uuid "
               f"  and warehouse_id='{WAREHOUSE}'::uuid),0)::text,"
               f"(select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid "
               f"  and deleted_at is null)::text,"
               f"(select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid)::text"
               )[0]
    keys = ["成本账数量", "成本账金额", "库位账数量", "流水条数", "流水净额", "存活序列号", "序列号总行"]
    d = dict(zip(keys, row))
    print(f"\n【{tag}】" + "  ".join(f"{k}={v}" for k, v in d.items()))
    return d


def login():
    pw = ADMIN_PW
    for attempt in range(2):
        st, res = http("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": pw})
        if st != 200 or res.get("code") != 0:
            bad("登录", f"{st} {res}")
            return None
        token = res["data"]["access_token"]
        if not res["data"].get("must_change_password"):
            return token
        new_pw = pw + "_p10new"
        st2, res2 = http("PUT", "/api/system/auth/password",
                         {"old_password": pw, "new_password": new_pw}, token)
        if st2 != 200 or res2.get("code") != 0:
            bad("首登强制改密", f"{st2} {res2}")
            return None
        print(f"  首登强制改密 → 新口令 {new_pw}")
        pw = new_pw
    return None


def cleanup():
    """只删本脚本夹具（固定 UUID 链），单批多语句，顺序照顾外键，幂等。"""
    doc = f"(select id from biz_stock_in where supplier_id='{SUPPLIER}'::uuid)"
    psql(
        f"delete from biz_voucher_entry where voucher_id in "
        f"(select id from biz_voucher where source_id in {doc});"
        f"update biz_stock_in set voucher_id=null, payable_id=null where supplier_id='{SUPPLIER}'::uuid;"
        f"delete from biz_payable where source_id in {doc};"
        f"delete from biz_voucher where source_id in {doc};"
        f"delete from biz_material_serial where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory_txn where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory_location where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_inventory where material_id='{MATERIAL}'::uuid;"
        f"delete from biz_stock_in_item where stock_in_id in {doc};"
        f"delete from biz_stock_in where supplier_id='{SUPPLIER}'::uuid;"
        f"delete from biz_material where id='{MATERIAL}'::uuid;"
        f"delete from biz_supplier where id='{SUPPLIER}'::uuid;"
        f"delete from biz_warehouse where id='{WAREHOUSE}'::uuid;"
        f"delete from biz_unit where id='{UNIT}'::uuid;"
    )


def create(token, serials, remark):
    st, res = http("POST", "/api/business/erp/stock-in", {
        "supplier_id": SUPPLIER, "warehouse_id": WAREHOUSE, "remark": remark,
        "items": [{"material_id": MATERIAL, "qty": str(len(serials)), "price_excl": "10",
                   "tax_rate": "0", "serial_ids": serials}],
    }, token)
    if st != 200 or res.get("code") != 0:
        return None, None, (st, res)
    return res["data"]["header"]["id"], res["data"]["header"]["in_no"], None


def post(token, doc_id, serials):
    return http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                {"status": "POSTED", "serials": [{"line_no": 1, "serial_ids": serials}]}, token)


def cancel(token, doc_id, reason):
    return http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                {"status": "CANCELLED", "reason": reason}, token)


def create_and_post(token, serials, remark):
    doc_id, in_no, err = create(token, serials, remark)
    if err:
        return None, None, err
    st, res = post(token, doc_id, serials)
    if st != 200 or res.get("code") != 0:
        return doc_id, in_no, (st, res)
    return doc_id, in_no, None


def main():
    token = login()
    if not token:
        print("\n==> 汇总：PASS=%d FAIL=%d" % (PASS, FAIL))
        return 1
    ok("登录拿到令牌（admin）")

    print("\n==> 0. 清上一轮夹具 + 造夹具")
    cleanup()
    psql(f"insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) "
         f"values ('{UNIT}','P10-E2E-PCS','个',1,null,1.000000,1,1) on conflict do nothing;"
         f"insert into biz_supplier (id, supplier_code, supplier_name, status) values "
         f"('{SUPPLIER}','P10-E2E-SUP','P10-E2E 供应商',1) on conflict do nothing;"
         f"insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) "
         f"values ('{WAREHOUSE}','P10-E2E-WH','P10-E2E 收货仓','/{WAREHOUSE}/',1,1,1) on conflict do nothing;"
         f"insert into biz_material (id, material_code, material_name, unit_id, status, serial_required) "
         f"values ('{MATERIAL}','P10-E2E-M1','一物一码电机（P10 e2e）','{UNIT}'::uuid,1,true) "
         f"on conflict do nothing")
    check("夹具物料就位（serial_required=true）", 1,
          psql1(f"select count(*) from biz_material where id='{MATERIAL}'::uuid and serial_required=true"))
    base = snap("① 基线（建单前）")

    print("\n==> 1. 建单 + 过账（3 件一物一码，客户端贴号）")
    doc1, no1, err = create_and_post(token, SN, "P10 e2e：作废退序列号")
    if err:
        bad("建单+过账", f"{err[0]} {err[1]}")
        print("\n==> 汇总：PASS=%d FAIL=%d" % (PASS, FAIL))
        return 1
    ok("建单并过账", no1)
    posted = snap("② 过账后")
    check("② 存活序列号 = 3", 3, posted["存活序列号"])
    check("② 成本账数量 = 基线 + 3", f"{float(base['成本账数量']) + 3:.4f}",
          f"{float(posted['成本账数量']):.4f}")
    check("② 库位账数量 = 基线 + 3", f"{float(base['库位账数量']) + 3:.4f}",
          f"{float(posted['库位账数量']):.4f}")
    check("② 3 个号 IN_STOCK 且挂本单",
          ",".join(s + ":IN_STOCK" for s in SN),
          psql1(f"select string_agg(serial_no||':'||status, ',' order by serial_no) "
                f"from biz_material_serial where source_type='PURCHASE' "
                f"and source_id='{doc1}'::uuid and deleted_at is null"))

    print("\n==> 2. 作废（POSTED → CANCELLED）：序列号必须被退回")
    st, res = cancel(token, doc1, "P10 e2e：作废必须退序列号")
    check("作废 HTTP 200", 200, st)
    check("作废业务码 0", 0, res.get("code"))
    voided = snap("③ 作废后")
    check("③ 成本账数量 == 基线", base["成本账数量"], voided["成本账数量"])
    check("③ 成本账金额 == 基线", base["成本账金额"], voided["成本账金额"])
    check("③ 库位账数量 == 基线", base["库位账数量"], voided["库位账数量"])
    check("③ 流水净额 == 基线", base["流水净额"], voided["流水净额"])
    check("③ 存活序列号 = 0（全部退回）", 0, voided["存活序列号"])
    check("③ 序列号总行仍 = 3（软删不物理删）", 3, voided["序列号总行"])
    check("③ 3 个号 deleted_at 置位（行保留、状态仍 IN_STOCK）", 3,
          psql1(f"select count(*) from biz_material_serial where source_id='{doc1}'::uuid "
                f"and deleted_at is not null and status='IN_STOCK'"))
    stock = psql1(f"select coalesce(sum(qty),0) from biz_inventory where material_id='{MATERIAL}'::uuid "
                  f"and warehouse_id='{WAREHOUSE}'::uuid")
    check("③ 不变量 C5：存活在库号 == 库存数量", f"{float(stock):.4f}", f"{float(alive_serials()):.4f}")

    print("\n==> 3. 同号重发（软删后部分唯一索引放行 = 退回是真释放）")
    doc2, no2, err = create_and_post(token, SN, "P10 e2e：同号重发")
    if err:
        bad("同号重发建单+过账", f"{err[0]} {err[1]}")
    else:
        ok("同号重发建单并过账（uk_biz_material_serial_no 部分唯一索引已释放）", no2)
        re = snap("④ 同号重发后")
        check("④ 存活序列号 = 3", 3, re["存活序列号"])
        check("④ 成本账数量 = 基线 + 3", f"{float(base['成本账数量']) + 3:.4f}",
              f"{float(re['成本账数量']):.4f}")
        st, res = cancel(token, doc2, "P10 e2e：对照组，全部在库可作废")
        check("④ 对照组（全部在库）作废 200", 200, st)
        check("④ 对照组作废后存活号 = 0", 0, alive_serials())
        check("④ 对照组作废后成本账 == 基线", base["成本账数量"], inv_qty())

    print("\n==> 4. 已出库 → 拒绝作废 409（SERIAL_SHIPPED 哨兵）")
    doc3, no3, err = create_and_post(token, SN, "P10 e2e：409 分支")
    if err:
        bad("409 分支建单+过账", f"{err[0]} {err[1]}")
    else:
        ok("409 分支建单并过账", no3)
        psql(f"update biz_material_serial set status='OUTBOUND' where source_id='{doc3}'::uuid "
             f"and serial_no='{SN[1]}' and deleted_at is null")
        st, res = cancel(token, doc3, "P10 e2e：已出库应 409")
        check("④ 有一个号已出库 → 作废 409（不是 200/500）", 409, st)
        check_in("④ 409 文案带「不能作废」", "不能作废", res.get("msg"))
        check_in("④ 409 文案点名出库的号 " + SN[1], SN[1], res.get("msg"))
        check_in("④ 409 文案带「请先做退货入库冲销」", "请先做退货入库冲销", res.get("msg"))
        check("④ 409 整单回滚：单据仍 POSTED", "POSTED",
              psql1(f"select status from biz_stock_in where id='{doc3}'::uuid"))
        check("④ 409 整单回滚：成本账仍 = 基线 + 3", f"{float(base['成本账数量']) + 3:.4f}",
              f"{float(inv_qty()):.4f}")
        check("④ 409 整单回滚：3 个号仍全部存活", 3, alive_serials())
        psql(f"update biz_material_serial set status='IN_STOCK' where source_id='{doc3}'::uuid "
             f"and serial_no='{SN[1]}' and deleted_at is null")
        st, res = cancel(token, doc3, "P10 e2e：号改回在库后可作废")
        check("④ 号改回 IN_STOCK 后作废 200", 200, st)
        check("④ 作废后存活号 = 0", 0, alive_serials())

    final = snap("⑤ 收尾前")
    check("⑤ 成本账数量 == 基线", base["成本账数量"], final["成本账数量"])
    check("⑤ 库位账数量 == 基线", base["库位账数量"], final["库位账数量"])
    check("⑤ 流水净额 == 基线", base["流水净额"], final["流水净额"])
    check("⑤ 存活序列号 = 0", 0, final["存活序列号"])

    print("\n==> 5. 清理夹具 + 残留核对")
    cleanup()
    resid = psql(f"select "
                 f"(select count(*) from biz_stock_in where supplier_id='{SUPPLIER}'::uuid),"
                 f"(select count(*) from biz_material_serial where material_id='{MATERIAL}'::uuid),"
                 f"(select count(*) from biz_inventory where material_id='{MATERIAL}'::uuid),"
                 f"(select count(*) from biz_inventory_txn where material_id='{MATERIAL}'::uuid),"
                 f"(select count(*) from biz_voucher where source_no like 'IN-%' "
                 f"  and source_id not in (select id from biz_stock_in)),"
                 f"(select count(*) from biz_material where id='{MATERIAL}'::uuid),"
                 f"(select count(*) from biz_supplier where id='{SUPPLIER}'::uuid),"
                 f"(select count(*) from biz_warehouse where id='{WAREHOUSE}'::uuid)")[0]
    check("清理后残留（收货单/序列号/库存/流水/孤儿凭证/物料/供应商/仓库）",
          "0,0,0,0,0,0,0,0", ",".join(resid))

    print(f"\n==> 汇总：PASS={PASS} FAIL={FAIL}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
