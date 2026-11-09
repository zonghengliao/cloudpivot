#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""t_9c938b6b 证据：`SALES_RETURN_IN` 该按 DOC 轴还是 NET 轴登记 —— 用判据的牙齿来判。

场景（隔离库，夹具前缀 `E4AX-`）：
    库存 4 件，唯一来源是一条 `SALES_RETURN_IN` 流水；它指向的退货入库单**被撤成 DRAFT**
    （= 单据轴不再解释这 4 件）。这是「登记成 DOC 会不会变成免检牌」的关键场景。

两种登记口径各跑一次全量巡检（`--group 状态`）：
    · 登记 `"SALES_RETURN_IN": "DOC"`（本卡落地的口径） ⇒ E4 必须 🚨：4 件库存没人解释；
    · 登记 `"SALES_RETURN_IN": "NET"`（备选口径）      ⇒ E4 会 ✅：NET 把这 4 件算进
      「其它合法动账净额」，判据被静默放宽 —— 这正是不能选 NET 的理由。

用法（只许打隔离库，脚本自己拒生产库名）：
    NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cloudpivot_e4reg_t9c \
      python3 docs/evidence/20261109-e4b-registry-sales-return/07_axis_net_vs_doc.py
"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
AUDIT = REPO / "scripts/verify/erp_invariants.py"
PREFIX = "E4AX-"
DB = dict(host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
          port=os.environ.get("NIK_DATABASE_PORT", "5432"),
          user=os.environ.get("NIK_DATABASE_USER", "alun"),
          db=os.environ.get("NIK_DATABASE_NAME", ""),
          password=os.environ.get("NIK_DATABASE_PASSWORD", ""))


def psql(sql):
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
                        "-d", DB["db"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql],
                       capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:600])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql):
    rows = psql(sql)
    return rows[0][0] if rows else None


def run_audit(audit):
    env = dict(os.environ, NIK_DATABASE_HOST=DB["host"], NIK_DATABASE_PORT=str(DB["port"]),
               NIK_DATABASE_USER=DB["user"], NIK_DATABASE_NAME=DB["db"],
               NIK_DATABASE_PASSWORD=DB["password"])
    r = subprocess.run([sys.executable, str(audit), "--group", "状态"],
                       capture_output=True, text=True, env=env, timeout=300)
    return r.returncode, r.stdout + r.stderr


def e4_line(out):
    for line in out.split("\n"):
        if "E4 " in line and any(k in line for k in ("✅", "🚨")):
            return line.strip()
    return "(没找到 E4 结果行)"


def cleanup(mat_sql):
    psql(f"delete from biz_inventory_txn where material_id in ({mat_sql})")
    psql(f"delete from biz_inventory where material_id in ({mat_sql})")
    psql(f"delete from biz_inventory_location where material_id in ({mat_sql})")
    psql(f"delete from biz_stock_in_item where stock_in_id in "
         f"(select id from biz_stock_in where in_no like '{PREFIX}%')")
    psql(f"delete from biz_stock_in where in_no like '{PREFIX}%'")
    psql(f"delete from biz_material where material_code like '{PREFIX}%'")
    psql(f"delete from biz_warehouse where warehouse_code like '{PREFIX}%'")


def main():
    db = DB["db"]
    if (not db or db in ("cloudpivot", "cloudpivot_prod") or "prod" in db.lower()
            or not db.startswith("cloudpivot_")):
        print(f"🚨 拒绝执行：NIK_DATABASE_NAME={db!r} 不是隔离库")
        return 2

    mat_sql = f"select id from biz_material where material_code like '{PREFIX}%'"
    cleanup(mat_sql)

    unit = one("select id from biz_unit order by unit_code limit 1")
    psql(f"""insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf)
             values (gen_random_uuid(), '{PREFIX}W1', '{PREFIX}退货仓', '/' || gen_random_uuid() || '/', 1, 1)""")
    wh = one(f"select id from biz_warehouse where warehouse_code = '{PREFIX}W1'")
    psql(f"""insert into biz_material (id, material_code, material_name, unit_id)
             values (gen_random_uuid(), '{PREFIX}M1', '{PREFIX}退货物料', '{unit}')""")
    mt = one(f"select id from biz_material where material_code = '{PREFIX}M1'")

    # 退货入库单：**DRAFT**（单据轴不解释这 4 件）
    psql(f"""insert into biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status,
                                       source_type, source_id, total_qty, total_amount,
                                       total_tax, total_with_tax, is_sourceless)
             values (gen_random_uuid(), '{PREFIX}IN-RET', 'RETURN', '{wh}', current_date,
                     'DRAFT', 'SALES_RETURN', gen_random_uuid(), 4, 40, 0, 40, true)""")
    sin = one(f"select id from biz_stock_in where in_no = '{PREFIX}IN-RET'")
    psql(f"""insert into biz_stock_in_item (id, stock_in_id, line_no, material_id, unit_id,
                                            qty, price, price_incl, amount, tax_amount, with_tax_amount)
             values (gen_random_uuid(), '{sin}', 1, '{mt}', '{unit}', 4, 10, 10, 40, 0, 40)""")
    psql(f"""insert into biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type,
                                            doc_type, doc_id, doc_no, line_no, qty, unit_cost,
                                            amount, period_code, remark)
             values (gen_random_uuid(), '{mt}', '{wh}', 'IN', 'SALES_RETURN_IN', 'STOCK_IN',
                     '{sin}', '{PREFIX}IN-RET', 1, 4, 10, 40, to_char(now(), 'YYYY-MM'),
                     '轴口径对比夹具')""")
    psql(f"""insert into biz_inventory (id, material_id, warehouse_id, qty, avg_cost, amount)
             values (gen_random_uuid(), '{mt}', '{wh}', 4, 10, 40)""")

    print("夹具：库存 4 件，唯一来源 = SALES_RETURN_IN 流水，其退货入库单被撤成 DRAFT")
    code_doc, out_doc = run_audit(AUDIT)
    print(f"\n① 登记 DOC（本卡口径）   E4：{e4_line(out_doc)}")
    print(f"   --group 状态 退出码 = {code_doc}")

    src = AUDIT.read_text(encoding="utf-8")
    patched = src.replace('"SALES_RETURN_IN": "DOC",', '"SALES_RETURN_IN": "NET",')
    assert patched != src, "补丁没打上（登记表里找不到 SALES_RETURN_IN: DOC）"
    with tempfile.TemporaryDirectory() as tmp:
        alt = Path(tmp) / "erp_invariants_net.py"
        alt.write_text(patched, encoding="utf-8")
        code_net, out_net = run_audit(alt)
    print(f"② 登记 NET（备选口径）   E4：{e4_line(out_net)}")
    print(f"   --group 状态 退出码 = {code_net}")

    cleanup(mat_sql)
    left = one(f"select count(*) from biz_material where material_code like '{PREFIX}%'")

    ok_doc = "🚨 [状态] E4 " in out_doc and "4.0000" in out_doc
    ok_net = "✅ [状态] E4 " in out_net
    print("\n" + "=" * 88)
    print(f"{'✅' if ok_doc else '🚨'} DOC 口径：E4 判红并报出多出 4.0000（4 件库存没人解释）")
    print(f"{'✅' if ok_net else '🚨'} NET 口径：E4 判绿 —— NET 把这 4 件算进「其它合法动账净额」，判据被放宽")
    print(f"{'✅' if left == '0' else '🚨'} 夹具已清空（残留 {left} 行）")
    verdict = ok_doc and ok_net and left == "0"
    print("结论：" + ("★ DOC 是对的：登记成 NET 会让「退货入库单不存在」这种真问题静默通过"
                    if verdict else "判据结论不成立，需复查"))
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
