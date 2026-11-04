#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
e2e 夹具残留检查（只读）
========================

用途：查「批量生成零成本夹具」有没有残留在库里 —— 这类行会让
`erp_invariants.py` 的 **B1（BLOCKER）** 与 **C3（WARN）** 同时变红，
P10（10:54）与 P11-followup（11:34 复发）两次是同一形态。

判据（四道同时成立才算夹具，与 `scripts/datafix/p10/20_residue_report.up.sql` 一致）：
  ① 该物料至少有 1 条 `biz_inventory_txn` 满足 `biz_type='SERIAL_IN' AND doc_type='SERIAL_BATCH'`
     （只有 `POST /business/erp/serial/batch-generate` 写这个组合，见 serial_model.rs:802）；
  ② 没有业务来源：`biz_stock_in_item` / `biz_purchase_order_item` 都没有它的行；
  ③ 该物料存在序列号，且**没有任何一条带真实来源**：`source_type` 为空 / `'PURCHASE'` 且
     `source_id IS NULL`（收货过账会回填 `source_id`，所以这条能区分「批量生成」与「真收货」）；
     ⚠️ 判据不能写成「等于 'PURCHASE'」：batch-generate 在请求不带 `source_type` 时落**空串**
     （`serial_model.rs` 的 `params.source_type.unwrap_or_default()`），2026-10-31 隔离实例
     复跑 11:34 走查实测就是空串形态 —— 只认 `'PURCHASE'` 会把整批夹具漏掉（假绿）。
  ④ 库存零价值：该物料所有 `biz_inventory` 行的 `amount` 与 `avg_cost` 都是 0。

ADR-20261031 落地后（批量生成 = 无来源入库，必录成本 + 写两本账）：
新形态**不再命中** ④ —— 这是对的，有成本就是有账载来源，撤销要走红冲/盘点，别用 P10 清。
语义说明与双向自测见 `scripts/verify/README.md`「语义变化」一节。

⚠️ 已知盲区（2026-10-31 16:17 prod 实测，判据未改）：第 ③ 道把 `source_type='PRODUCE'`
且 `source_id IS NULL` 当成「有真实来源」⇒ 这种形态的零成本夹具会被**漏检**。
真正的硬判据是 `source_id IS NULL`（没有来源单据）；`source_type` 非空不代表有来源单。
组合闸门仍有效：`e2e-postrun-check.sh` 的第 ③ 步（不变量）会把这种形态抓住。

用法：
    python3 scripts/verify/e2e_fixture_residue.py            # 人看
    python3 scripts/verify/e2e_fixture_residue.py --json     # 机器看（监控 / e2e 后置检查）
    NIK_DATABASE_NAME=cloudpivot_p11f python3 ...            # 换库（隔离库 / 演练库）

连接口径与 `erp_invariants.py` 相同：只认 `NIK_DATABASE_*`（记得 `set -a; . /etc/cloudpivot/cloudpivot.env`）。

退出码：有残留 → 1；干净 → 0；连库失败 → 2（不要把它当成「干净」）。

    ⚠ 本脚本**不装** db_guard：它是**只读**巡检，而且 e2e 打生产时必须能对生产库跑一次
      （`frontend/e2e/target-guard.mjs` 的 POSTRUN_STEPS 就要求在生产上跑它）。
      「禁止打生产」的守卫在写数据的脚本上，只读审计不加闸。
"""
import argparse
import json
import os
import subprocess
import sys

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)

# 判据 SQL：与 20_residue_report.up.sql 的 CTE 逐条对应（改动必须同改两处，e2e 里有一条交叉断言）
SQL = """
WITH cand AS (
    SELECT m.id AS material_id, m.material_code
      FROM biz_material m
     WHERE m.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM biz_inventory_txn t
                    WHERE t.material_id = m.id
                      AND t.biz_type = 'SERIAL_IN' AND t.doc_type = 'SERIAL_BATCH')
       AND NOT EXISTS (SELECT 1 FROM biz_stock_in_item si WHERE si.material_id = m.id)
       AND NOT EXISTS (SELECT 1 FROM biz_purchase_order_item pi WHERE pi.material_id = m.id)
       AND EXISTS (SELECT 1 FROM biz_material_serial s WHERE s.material_id = m.id)
       AND NOT EXISTS (SELECT 1 FROM biz_material_serial s
                        WHERE s.material_id = m.id
                          AND ((s.source_type IS NOT NULL AND s.source_type NOT IN ('', 'PURCHASE'))
                               OR s.source_id IS NOT NULL))
       AND NOT EXISTS (SELECT 1 FROM biz_inventory i
                        WHERE i.material_id = m.id AND (i.amount <> 0 OR i.avg_cost <> 0))
)
SELECT c.material_code,
       (SELECT count(*) FROM biz_inventory i WHERE i.material_id = c.material_id),
       coalesce((SELECT sum(i.qty) FROM biz_inventory i WHERE i.material_id = c.material_id), 0),
       (SELECT count(*) FROM biz_inventory_txn t WHERE t.material_id = c.material_id),
       (SELECT count(*) FROM biz_material_serial s
         WHERE s.material_id = c.material_id AND s.deleted_at IS NULL),
       (SELECT count(*) FROM biz_material_serial s WHERE s.material_id = c.material_id)
  FROM cand c
 ORDER BY c.material_code
"""


def psql_rows(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    cmd = ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
           "-d", DB["name"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql]
    run = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=60)
    if run.returncode != 0:
        raise RuntimeError(run.stderr.strip()[:400] or "psql 执行失败")
    rows = []
    for line in run.stdout.strip().split("\n"):
        if line:
            rows.append(line.split("\x1f"))
    return rows


def main():
    ap = argparse.ArgumentParser(description="e2e 夹具残留检查（只读）")
    ap.add_argument("--json", action="store_true", help="输出 JSON（监控 / 后置检查用）")
    args = ap.parse_args()

    try:
        rows = psql_rows(SQL)
    except (RuntimeError, subprocess.TimeoutExpired) as exc:
        if args.json:
            print(json.dumps({"ok": False, "error": str(exc), "db": DB["name"]}, ensure_ascii=False))
        else:
            print(f"✗ 连库/查询失败（{DB['name']}@{DB['host']}:{DB['port']}）：{exc}")
            print("  提示：先 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a`")
        return 2

    hits = [
        dict(material_code=r[0], inv_rows=int(r[1]), inv_qty=str(r[2]),
             txn_rows=int(r[3]), serial_alive=int(r[4]), serial_rows=int(r[5]))
        for r in rows
    ]

    if args.json:
        print(json.dumps({"ok": not hits, "db": DB["name"], "hits": hits}, ensure_ascii=False))
        return 1 if hits else 0

    print("=" * 84)
    print("e2e 夹具残留检查（批量生成零成本夹具）")
    print(f"库: {DB['name']}@{DB['host']}:{DB['port']}")
    print("=" * 84)
    if not hits:
        print("  ✅ 无残留：没有「无单据、零价值、批量生成」的物料 —— B1/C3 不会被这类夹具打红")
        return 0

    for h in hits:
        print(f"  ❌ 命中 {h['material_code']}："
              f"库存 {h['inv_rows']} 行 / 数量 {h['inv_qty']}；流水 {h['txn_rows']} 条；"
              f"存活序列号 {h['serial_alive']}（全部 {h['serial_rows']} 条）")
    print("-" * 84)
    print(f"  结论：{len(hits)} 个物料是「批量生成」夹具残留 ⇒ erp_invariants 的 B1（BLOCKER）/ C3（WARN）会红")
    print("  处置（带备份，幂等）：")
    print("    1) bash scripts/datafix/p10/00_backup.sh")
    print("    2) bash scripts/datafix/p10/apply.sh --check          # 先看")
    print("    3) P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh   # 再清（快照可回滚）")
    print("  复发源头：e2e / 手工点「批量生成」不要打 prod（8023）；见 docs/datafix-20261031-p11-m-w1a-004-recur.md")
    return 1


if __name__ == "__main__":
    sys.exit(main())
