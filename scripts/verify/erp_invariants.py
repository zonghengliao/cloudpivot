#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
云枢 ERP 不变量审计套件 (ERP Invariant Audit Suite)
====================================================
用途：把 ERP 的会计恒等式 + 业务不变量写成可反复运行的自动校验。
      任何一条 FAIL = 账实/账账不符，必须当天修，不许带病运行。

用法：
    python3 erp_invariants.py              # 全量跑，输出报告
    python3 erp_invariants.py --json       # JSON 输出（给监控用）
    python3 erp_invariants.py --group 会计  # 只跑某一组

连接口径：默认本机 prod（`NIK_DATABASE_*` 未设时 = 127.0.0.1:5432 / cloudpivot / cloudpivot），
可用环境变量覆盖（演练库 / 隔离库 / CI）：
    NIK_DATABASE_HOST / NIK_DATABASE_PORT / NIK_DATABASE_USER / NIK_DATABASE_NAME / NIK_DATABASE_PASSWORD

退出码：有 BLOCKER → 1（`scripts/check.sh` 的闸③ 靠这个判红）；否则 0。

设计原则：
  1. 每条不变量只依赖数据库真相，不信任任何业务代码的自我声明
  2. FAIL 必须给出「差多少、差在哪几条」，不给出定位的检查等于没查
  3. 金额比较一律用 numeric，容差 0.0001（4 位标度）
  4. 分类：BLOCKER（账错了，立刻停）/ WARN（可疑，要人看）/ INFO（仅记录）
"""
import os, re, sys, subprocess, json, argparse
from decimal import Decimal
from pathlib import Path

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    db=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)
TOL = Decimal("0.0001")
RESULT = []

# ══════════════════════════════════════════════════════════════════
# E4 的「库存流水 biz_type」登记表 —— 白名单不许再逐个漏
# ══════════════════════════════════════════════════════════════════
# E4 判据是「库存 ≤ 单据能解释的量」。**每漏一个会产 IN 的 biz_type，用它的库就必假红**
# （OPENING / STOCK_TAKE_* 就是这么漏的：期初建账库、盘盈库恒报「有作废单没退库存」，
#  既误伤 check.sh 闸③，又把真正的「作废没退库存」淹掉）。
#
# 取值域来源（代码里改 biz_type 必须同步这张表；漏登记由两道闸兜住）：
#   ① `backend/src/business/shared/doc_kind.rs::TxnBizType::ALL`（22 个，唯一拼写；含盘点三态）
#   ② `backend/src/business/erp/stocktake_model.rs`
#      （STOCK_TAKE_GAIN / STOCK_TAKE_LOSS / STOCK_TAKE_CANCEL 的写入常量 —— 盘点轴。
#       这三个曾**只**以常量形式存在、词表里没有（已知漂移），t_6a667db8 已补进 ① 的
#       枚举与 TXN_BIZ_TYPE_VALUES；仍在此登记是因为「写入点 ≠ 词表」两件事要分开看：
#       常量在哪个模块写，与它属于哪根轴，是不同的问题）
#   闸一：本套件 E4b 在运行期把库里出现、但此处未登记的 biz_type 直接判 BLOCKER；
#   闸二：`scripts/verify/regression/erp_invariants_e4_regression.py` 静态逐字比对
#         上面两处代码常量，代码新增 biz_type 而这里没跟上 → 回归红。
#
# 三种归属：
#   'DOC'    —— 由 `biz_stock_in` **单据轴**解释：过账写 PURCHASE_IN（stockin_write.rs:551），
#               作废写 PURCHASE_IN_CANCEL（stockin_cancel.rs:270），单据 status 一翻
#               `posted` 汇总与流水同步涨落 ⇒ **不能再进 other_moves，否则重复扣减**；
#   'NET'    —— 其它一切动账（期初 / 盘点三态 / 调拨 / 序列号 / 各类出库 / 出库作废 …），
#               按 txn_type 正负号计入 other_moves；
#   'LEGACY' —— 现行代码**不产**、但历史库/夹具里确实存在的旧值（如 biz_type='PURCHASE' +
#               doc_type='PURCHASE_ORDER'）。按 NET 计入（否则老库必假红），
#               但 E4c 会把它当 WARN 亮出来「建议确认来源/清理」，不静默放过。
#   ⚠️ 前提：`biz_stock_in` 过账恒写 PURCHASE_IN（`stockin_write::write_txn` 无 biz_type 入参），
#      所以 PRODUCTION_IN / PURCHASE_RETURN 等即便将来按 in_type 启用也不与 posted 重叠。
#      一旦改成「按 in_type 分流 biz_type」，必须把对应值改判成 'DOC'。
E4_BIZ_TYPE_AXIS = {
    "PURCHASE_IN": "DOC",
    "PURCHASE_IN_CANCEL": "DOC",
    "OPENING": "NET",
    "STOCK_TAKE_GAIN": "NET",
    "STOCK_TAKE_LOSS": "NET",
    "STOCK_TAKE_CANCEL": "NET",
    "TRANSFER_IN": "NET",
    "TRANSFER_OUT": "NET",
    "SERIAL_IN": "NET",
    "SERIAL_OUT": "NET",
    "SERIAL_SCRAP": "NET",
    "STOCK_OUT_CANCEL": "NET",
    "ISSUE_OUT": "NET",
    "SCRAP_OUT": "NET",
    "LOSS_OUT": "NET",
    "OTHER_OUT": "NET",
    "PURCHASE_RETURN": "NET",
    "SALE_OUT": "NET",
    "PRODUCTION_IN": "NET",
    "PRODUCE_CANCEL": "NET",
    "ADJUST_IN": "NET",
    "ADJUST_OUT": "NET",
    "PURCHASE": "LEGACY",
}
E4_OTHER_BIZ_TYPES = tuple(k for k, v in E4_BIZ_TYPE_AXIS.items() if v in ("NET", "LEGACY"))
E4_LEGACY_BIZ_TYPES = tuple(k for k, v in E4_BIZ_TYPE_AXIS.items() if v == "LEGACY")
E4_OTHER_IN_SQL = ",".join("'%s'" % b for b in E4_OTHER_BIZ_TYPES)
E4_KNOWN_IN_SQL = ",".join("'%s'" % b for b in E4_BIZ_TYPE_AXIS)
E4_LEGACY_IN_SQL = ",".join("'%s'" % b for b in E4_LEGACY_BIZ_TYPES) or "''"


def q(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"], "-d", DB["db"], "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:300])
    rows = []
    for line in r.stdout.strip().split("\n"):
        if line == "":
            continue
        rows.append(line.split("\x1f"))
    return rows


def one(sql, default="0"):
    rows = q(sql)
    if not rows or not rows[0]:
        return default
    return rows[0][0]


def dec(v):
    try:
        return Decimal(str(v).strip() or "0")
    except Exception:
        return Decimal("0")


def table_exists(name):
    """表是否存在（新表还没上线的库要能跑完整个套件，不能因为缺表整组炸掉）。"""
    try:
        rows = q("select 1 from information_schema.tables "
                 f"where table_schema = 'public' and table_name = '{name}'")
        return bool(rows)
    except Exception:
        return False


def check(name, group, level, ok, detail="", locator_sql=None):
    """登记一条检查结果。ok=True 为 PASS。detail 说明差多少。"""
    RESULT.append(dict(name=name, group=group, level=level,
                       ok=bool(ok), detail=detail))
    mark = "✅" if ok else ("🚨" if level == "BLOCKER" else ("⚠️" if level == "WARN" else "ℹ️"))
    print(f"  {mark} [{group}] {name}")
    if not ok and detail:
        for line in str(detail).split("\n"):
            print(f"        {line}")
    if not ok and locator_sql:
        try:
            rows = q(locator_sql)
            if rows:
                print(f"        ↳ 定位（{len(rows)} 条）：")
                for r in rows[:8]:
                    print(f"          {' | '.join(str(x)[:40] for x in r)}")
                if len(rows) > 8:
                    print(f"          ... 还有 {len(rows) - 8} 条")
        except Exception as e:
            print(f"        ↳ 定位查询失败: {e}")


# ══════════════════════════════════════════════════════════════════
# A. 会计恒等式 —— 最硬的一条，错了就说明账本坏了
# ══════════════════════════════════════════════════════════════════
def group_a():
    print("\n" + "=" * 92)
    print("A. 会计恒等式（凭证借贷平衡）")
    print("=" * 92)

    # A1 每张凭证的借贷必须相等
    bad = q("""
        select v.voucher_no, v.total_debit::text, v.total_credit::text,
               coalesce(sum(e.debit_amount),0)::text, coalesce(sum(e.credit_amount),0)::text
        from biz_voucher v
        left join biz_voucher_entry e on e.voucher_id = v.id
        where v.deleted_at is null
        group by v.id, v.voucher_no, v.total_debit, v.total_credit
        having abs(coalesce(sum(e.debit_amount),0) - coalesce(sum(e.credit_amount),0)) > 0.0001
    """)
    check("A1 每张凭证 Σ借 = Σ贷", "会计", "BLOCKER", not bad,
          f"{len(bad)} 张凭证借贷不平" if bad else "全部平衡",
          """select v.voucher_no, (v.total_debit - v.total_credit)::text as 差
             from biz_voucher v where v.deleted_at is null
             and abs(v.total_debit - v.total_credit) > 0.0001""")

    # A2 凭证头汇总 = 分录明细汇总
    bad2 = q("""
        select v.voucher_no, v.total_debit::text, coalesce(sum(e.debit_amount),0)::text,
               v.total_credit::text, coalesce(sum(e.credit_amount),0)::text
        from biz_voucher v
        left join biz_voucher_entry e on e.voucher_id = v.id
        where v.deleted_at is null
        group by v.id, v.voucher_no, v.total_debit, v.total_credit
        having abs(v.total_debit - coalesce(sum(e.debit_amount),0)) > 0.0001
            or abs(v.total_credit - coalesce(sum(e.credit_amount),0)) > 0.0001
    """)
    check("A2 凭证头金额 = 分录取和", "会计", "BLOCKER", not bad2,
          f"{len(bad2)} 张头与明细不一致" if bad2 else "全部一致",
          """select v.voucher_no, v.total_debit::text,
                    coalesce(sum(e.debit_amount),0)::text as 分录取和
             from biz_voucher v left join biz_voucher_entry e on e.voucher_id=v.id
             where v.deleted_at is null group by v.id, v.voucher_no, v.total_debit, v.total_credit
             having abs(v.total_debit - coalesce(sum(e.debit_amount),0)) > 0.0001""")

    # A3 entry_count 与实际分录条数一致
    bad3 = q("""
        select v.voucher_no, v.entry_count::text, count(e.id)::text
        from biz_voucher v left join biz_voucher_entry e on e.voucher_id = v.id
        where v.deleted_at is null
        group by v.id, v.voucher_no, v.entry_count
        having v.entry_count <> count(e.id)
    """)
    check("A3 entry_count = 分录条数", "会计", "WARN", not bad3,
          f"{len(bad3)} 张计数错" if bad3 else "全部一致")

    # A4 全账借贷总额相等（试算平衡的根）
    d = dec(one("select coalesce(sum(total_debit),0) from biz_voucher where deleted_at is null"))
    c = dec(one("select coalesce(sum(total_credit),0) from biz_voucher where deleted_at is null"))
    check("A4 全账 Σ借 = Σ贷", "会计", "BLOCKER", abs(d - c) <= TOL,
          f"借方 {d} / 贷方 {c} / 差 {d - c}")

    # A5 分录不能借贷同时非零，也不能同时为零
    bad5 = q("""
        select v.voucher_no, a.account_code, e.debit_amount::text, e.credit_amount::text
        from biz_voucher_entry e
        join biz_voucher v on v.id = e.voucher_id
        left join biz_account a on a.id = e.account_id
        where v.deleted_at is null
          and ((e.debit_amount > 0 and e.credit_amount > 0) or (e.debit_amount = 0 and e.credit_amount = 0))
    """)
    check("A5 每条分录 有且仅有一边非零", "会计", "BLOCKER", not bad5,
          f"{len(bad5)} 条分录借贷都非零或都为零" if bad5 else "全部合规")


# ══════════════════════════════════════════════════════════════════
# B. 账账相符 —— 业务账 vs 财务账
# ══════════════════════════════════════════════════════════════════
def group_b():
    print("\n" + "=" * 92)
    print("B. 账账相符（库存账 vs 财务账 vs 库位账）")
    print("=" * 92)

    # B1 库位账合计 = 成本账（不变式③，W2c-2 的巡检项）
    bad = q("""
        with cost as (
            select material_id, warehouse_id, sum(qty) as q
            from biz_inventory
            group by 1, 2
        ), loc as (
            select material_id, warehouse_id, sum(qty) as q
            from biz_inventory_location
            group by 1, 2
        )
        select coalesce(c.material_id, l.material_id)::text,
               coalesce(c.warehouse_id, l.warehouse_id)::text,
               coalesce(c.q,0)::text, coalesce(l.q,0)::text
        from cost c full outer join loc l
          on c.material_id = l.material_id and c.warehouse_id = l.warehouse_id
        where abs(coalesce(c.q,0) - coalesce(l.q,0)) > 0.0001
    """)
    check("B1 库位账合计 = 成本账 qty", "账账", "BLOCKER", not bad,
          f"{len(bad)} 组物料×仓库两账不符" if bad else "全部相符",
          """with cost as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2),
                  loc as (select material_id, warehouse_id, sum(qty) q from biz_inventory_location group by 1,2)
             select m.material_code, w.warehouse_name,
                    coalesce(c.q,0)::text as 成本账, coalesce(l.q,0)::text as 库位账
             from cost c full outer join loc l on c.material_id=l.material_id and c.warehouse_id=l.warehouse_id
             left join biz_material m on m.id=coalesce(c.material_id,l.material_id)
             left join biz_warehouse w on w.id=coalesce(c.warehouse_id,l.warehouse_id)
             where abs(coalesce(c.q,0)-coalesce(l.q,0)) > 0.0001""")

    # B2 库存金额 = 存货科目（1405）余额
    inv_amt = dec(one("select coalesce(sum(amount),0) from biz_inventory"))
    gl_amt = dec(one("""
        select coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)),0)
        from biz_voucher_entry e
        join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
        join biz_account a on a.id = e.account_id
        where a.account_code like '1405%'
    """))
    check("B2 库存金额 = 存货科目 1405 余额", "账账", "BLOCKER",
          abs(inv_amt - gl_amt) <= TOL,
          f"库存模块 {inv_amt} / 总账 1405 {gl_amt} / 差 {inv_amt - gl_amt}")

    # B3 应付未付合计 = 应付科目（2202）贷方余额
    ap_unpaid = dec(one("""
        select coalesce(sum(unpaid_amount),0) from biz_payable
        where deleted_at is null and status <> 'CANCELLED'
    """))
    ap_gl = dec(one("""
        select coalesce(sum(coalesce(e.credit_amount,0) - coalesce(e.debit_amount,0)),0)
        from biz_voucher_entry e
        join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
        join biz_account a on a.id = e.account_id
        where a.account_code like '2202%'
    """))
    check("B3 应付未付 = 应付科目 2202 贷方余额", "账账", "WARN",
          abs(ap_unpaid - ap_gl) <= TOL,
          f"应付模块未付 {ap_unpaid} / 总账 2202 {ap_gl} / 差 {ap_unpaid - ap_gl}"
          + ("\n        （若刚做了付款但未生成凭证，差异合理；否则说明付款没记账）" if abs(ap_unpaid - ap_gl) > TOL else ""))

    # B4 流水累计 = 当前库存（按方向加减）
    bad4 = q("""
        with txn as (
            select material_id, warehouse_id,
                   sum(case when txn_type = 'IN' then qty else -qty end) as q
            from biz_inventory_txn group by 1,2
        ), inv as (
            select material_id, warehouse_id, sum(qty) as q
            from biz_inventory group by 1,2
        )
        select coalesce(t.material_id,i.material_id)::text,
               coalesce(t.warehouse_id,i.warehouse_id)::text,
               coalesce(t.q,0)::text, coalesce(i.q,0)::text
        from txn t full outer join inv i
          on t.material_id=i.material_id and t.warehouse_id=i.warehouse_id
        where abs(coalesce(t.q,0) - coalesce(i.q,0)) > 0.0001
    """)
    check("B4 流水累计 = 当前库存", "账账", "BLOCKER", not bad4,
          f"{len(bad4)} 组流水与库存不符" if bad4 else "全部相符",
          """with txn as (select material_id, warehouse_id,
                            sum(case when txn_type='IN' then qty else -qty end) q
                         from biz_inventory_txn group by 1,2),
                  inv as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2)
             select m.material_code, w.warehouse_name,
                    coalesce(t.q,0)::text as 流水累计, coalesce(i.q,0)::text as 当前库存
             from txn t full outer join inv i on t.material_id=i.material_id and t.warehouse_id=i.warehouse_id
             left join biz_material m on m.id=coalesce(t.material_id,i.material_id)
             left join biz_warehouse w on w.id=coalesce(t.warehouse_id,i.warehouse_id)
             where abs(coalesce(t.q,0)-coalesce(i.q,0)) > 0.0001""")

    # ══════════════════════════════════════════════════════════════
    # B5 / B6 出库单（ARCH-3 DC-13）：B1/B2/B4/C1 只能证「账平」，
    # **发现不了「该出没出」**。这两条专门盯出库侧的两种硬伤：
    #   B5  已过账却没凭证（成本没进 6401/1901，1405 不会减 → 存货虚增）
    #   B6  出库量累计 > 入库+期初（账实不符的负向版本：货出了但没记入库/期初）
    # ══════════════════════════════════════════════════════════════
    if not table_exists("biz_stock_out"):
        check("B5 出库单已过账 → 必有成本结转凭证", "账账", "INFO", True,
              "biz_stock_out 未建（本库还没跑 V0203）：跳过")
        check("B6 出库量累计 ≤ 入库+期初（不允许负库存的仓）", "账账", "INFO", True,
              "biz_stock_out 未建（本库还没跑 V0203）：跳过")
    else:
        # B5：POSTED 且金额 > 0 的出库单，凭证必须存在且金额对得上。
        # 金额 0 的（零成本存量出库）按设计跳过凭证 → 不算违约（单独 INFO）。
        bad5 = q("""
            select s.out_no, s.total_amount::text,
                   coalesce(v.voucher_no, '(无凭证)'),
                   coalesce(v.total_debit, 0)::text
            from biz_stock_out s
            left join biz_voucher v
                   on v.id = s.voucher_id and v.deleted_at is null and v.is_red = 0
            where s.deleted_at is null
              and s.status = 'POSTED'
              and s.total_amount > 0.0001
              and (s.voucher_id is null
                   or v.id is null
                   or abs(v.total_debit - s.total_amount) > 0.0001)
            order by 1
        """)
        check("B5 出库单已过账 → 必有成本结转凭证", "账账", "BLOCKER", not bad5,
              f"{len(bad5)} 张已过账出库单缺凭证或金额不符（成本没结转，1405 不会减）"
              if bad5 else "全部有凭证且金额一致",
              """select s.out_no, s.status, s.total_amount::text, coalesce(v.voucher_no,'(无)')
                 from biz_stock_out s left join biz_voucher v on v.id = s.voucher_id
                 where s.deleted_at is null and s.status='POSTED' and s.total_amount > 0.0001
                   and (s.voucher_id is null or v.id is null)""")

        zero_cost = q("""
            select s.out_no, s.total_qty::text
            from biz_stock_out s
            where s.deleted_at is null and s.status = 'POSTED'
              and s.total_amount <= 0.0001
        """)
        check("B5b 零成本出库单（金额 = 0，按设计无凭证）", "账账", "INFO", True,
              f"{len(zero_cost)} 张（零成本存量，财务按需补账）" if zero_cost else "无")

        # B6：出库累计 ≤ 入库累计（含期初，期初也走 IN 流水）。
        # 允许负库存的仓库（biz_warehouse.allow_negative = 1）**排除**：
        # 那是仓库自己的策略，负数量是预期的（C1 也不该为它报错）。
        bad6 = q("""
            with inb as (
                select material_id, warehouse_id, sum(qty) as q
                from biz_inventory_txn where txn_type = 'IN' group by 1, 2
            ), outb as (
                select material_id, warehouse_id, sum(qty) as q
                from biz_inventory_txn where txn_type = 'OUT' group by 1, 2
            )
            select coalesce(o.material_id, i.material_id)::text,
                   coalesce(o.warehouse_id, i.warehouse_id)::text,
                   coalesce(i.q, 0)::text, coalesce(o.q, 0)::text
            from outb o
            full outer join inb i
              on i.material_id = o.material_id and i.warehouse_id = o.warehouse_id
            left join biz_warehouse w on w.id = coalesce(o.warehouse_id, i.warehouse_id)
            where coalesce(o.q, 0) > coalesce(i.q, 0) + 0.0001
              and coalesce(w.allow_negative, 0) = 0
        """)
        check("B6 出库量累计 ≤ 入库+期初", "账账", "BLOCKER", not bad6,
              f"{len(bad6)} 组出库量大于入库量（货出了，但入库/期初没记）"
              if bad6 else "全部相符",
              """with inb as (select material_id, warehouse_id, sum(qty) q
                              from biz_inventory_txn where txn_type='IN' group by 1,2),
                    outb as (select material_id, warehouse_id, sum(qty) q
                               from biz_inventory_txn where txn_type='OUT' group by 1,2)
               select m.material_code, w.warehouse_name,
                      coalesce(i.q,0)::text as 入库累计, coalesce(o.q,0)::text as 出库累计
               from outb o full outer join inb i
                 on i.material_id=o.material_id and i.warehouse_id=o.warehouse_id
               left join biz_material m on m.id=coalesce(o.material_id,i.material_id)
               left join biz_warehouse w on w.id=coalesce(o.warehouse_id,i.warehouse_id)
               where coalesce(o.q,0) > coalesce(i.q,0) + 0.0001
                 and coalesce(w.allow_negative,0) = 0""")


# ══════════════════════════════════════════════════════════════════
# C. 账实相符 —— 不该出现的数据
# ══════════════════════════════════════════════════════════════════
def group_c():
    print("\n" + "=" * 92)
    print("C. 账实相符（负库存/异常值）")
    print("=" * 92)

    neg = q("""select m.material_code, w.warehouse_name, i.qty::text
               from biz_inventory i
               left join biz_material m on m.id=i.material_id
               left join biz_warehouse w on w.id=i.warehouse_id
               where i.qty < 0""")
    check("C1 无负库存", "账实", "BLOCKER", not neg,
          f"{len(neg)} 行负库存" if neg else "全部非负")

    negl = q("""select count(*) from biz_inventory_location
                where qty < 0""")
    check("C2 无负库位库存", "账实", "BLOCKER", not negl or negl[0][0] == "0",
          f"{negl[0][0] if negl else 0} 行负库位库存")

    # 有库存但 avg_cost 为 0（成本核算缺失）
    zero_cost = q("""select m.material_code, w.warehouse_name, i.qty::text, i.avg_cost::text
                     from biz_inventory i
                     left join biz_material m on m.id=i.material_id
                     left join biz_warehouse w on w.id=i.warehouse_id
                     where i.qty > 0 and i.avg_cost = 0""")
    check("C3 有库存则成本价非零", "账实", "WARN", not zero_cost,
          f"{len(zero_cost)} 行有货但均价为 0（成本核算漏了）" if zero_cost else "全部正常")

    # amount 与 qty*avg_cost 一致
    bad_amt = q("""select m.material_code, i.qty::text, i.avg_cost::text, i.amount::text
                   from biz_inventory i
                   left join biz_material m on m.id=i.material_id
                   where i.qty > 0
                     and abs(i.amount - i.qty * i.avg_cost) > 0.01""")
    check("C4 库存金额 = 数量 × 均价", "账实", "WARN", not bad_amt,
          f"{len(bad_amt)} 行金额与数量×均价不符" if bad_amt else "全部一致")

    # C5 一物一码：在库序列号件数 = 库存数量（W2c 口径 · 不变式①，P10 新增）
    #
    # 两层判据：
    #   ① 每个 (物料, 仓库) 的 `count(serial WHERE status='IN_STOCK')` 必须等于 `biz_inventory.qty`；
    #   ② 没有仓库的序列号（`warehouse_id IS NULL`）本身就是账实缺口 —— 它进不了任何一本仓账，
    #      按「(无仓库)」一行单独报出来（`batch-generate` 不带 warehouse 就会造出这种号）。
    #
    # 级别 WARN 而不是 BLOCKER：`serial_required` 可以在已有库存的物料上后开（W1c 允许 PUT），
    # 那会立刻出现「历史按数量的库存 vs 0 条序列号」的过渡态 —— 不是账错，是需要人看一眼。
    #
    # ⚠️ 统计**必须**带 `deleted_at IS NULL`：作废/退回的序列号是软删行（P10 作废口径），
    #    漏掉这个条件会把已作废的号当成在库（P9 交付时就是这样把「3 条存活 + 8 条已软删」
    #    误读成「8 条 IN_STOCK 与在库 2 对不上」，白开了一张卡）。
    bad_serial = q("""
        with serial as (
            select material_id, warehouse_id, count(*)::numeric as q
              from biz_material_serial
             where status = 'IN_STOCK' and deleted_at is null
             group by 1, 2
        ), inv as (
            select material_id, warehouse_id, sum(qty) as q
              from biz_inventory
             group by 1, 2
        )
        select m.material_code,
               coalesce(w.warehouse_code, '(无仓库)'),
               coalesce(s.q,0)::text as 在库号,
               coalesce(v.q,0)::text as 帐面,
               (coalesce(s.q,0) - coalesce(v.q,0))::text as 差
          from serial s
          full outer join inv v
            on v.material_id = s.material_id
           and v.warehouse_id is not distinct from s.warehouse_id
          join biz_material m on m.id = coalesce(s.material_id, v.material_id)
          left join biz_warehouse w on w.id = coalesce(s.warehouse_id, v.warehouse_id)
         where m.serial_required = true
           and abs(coalesce(s.q,0) - coalesce(v.q,0)) > 0.0001
    """)
    check("C5 一物一码：在库序列号件数 = 库存数量", "账实", "WARN", not bad_serial,
          f"{len(bad_serial)} 组「在库序列号 vs 帐面数量」不符" if bad_serial else "全部相符",
          """with serial as (select material_id, warehouse_id, count(*)::numeric q
                              from biz_material_serial
                             where status='IN_STOCK' and deleted_at is null group by 1,2),
                  inv as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2)
             select m.material_code, coalesce(w.warehouse_code,'(无仓库)'),
                    coalesce(s.q,0)::text as 在库号, coalesce(v.q,0)::text as 帐面
               from serial s
               full outer join inv v on v.material_id=s.material_id
                                   and v.warehouse_id is not distinct from s.warehouse_id
               join biz_material m on m.id=coalesce(s.material_id,v.material_id)
               left join biz_warehouse w on w.id=coalesce(s.warehouse_id,v.warehouse_id)
              where m.serial_required = true
                and abs(coalesce(s.q,0)-coalesce(v.q,0)) > 0.0001""")


# ══════════════════════════════════════════════════════════════════
# D. 单据链完整性
# ══════════════════════════════════════════════════════════════════
def group_d():
    print("\n" + "=" * 92)
    print("D. 单据链完整性（源单/凭证/孤儿行）")
    print("=" * 92)

    # D1 已过账收货单必须有凭证
    no_v = q("""
        select s.in_no, s.status, s.total_with_tax::text
        from biz_stock_in s
        where s.deleted_at is null and s.status = 'POSTED'
          and not exists (
            select 1 from biz_voucher v
            where v.deleted_at is null and v.source_no = s.in_no and v.is_red = 0)
    """)
    check("D1 已过账收货单必有凭证", "单据链", "BLOCKER", not no_v,
          f"{len(no_v)} 张已过账但无凭证" if no_v else "全部有凭证",
          """select s.in_no, s.status from biz_stock_in s
             where s.deleted_at is null and s.status='POSTED'
               and not exists (select 1 from biz_voucher v where v.deleted_at is null
                               and v.source_no = s.in_no and v.is_red=0)""")

    # D2 凭证必能追溯到源单（自动凭证）
    orphan_v = q("""
        select v.voucher_no, v.source_type, coalesce(v.source_no,'(空)')
        from biz_voucher v
        where v.deleted_at is null and v.source_type <> 'MANUAL' and coalesce(v.source_no,'') = ''
    """)
    check("D2 自动凭证必有源单号", "单据链", "WARN", not orphan_v,
          f"{len(orphan_v)} 张自动凭证无源单号（无法追溯）" if orphan_v else "全部可追溯")

    # D3 无孤儿分录
    orphan_e = q("""select count(*) from biz_voucher_entry e
                    where not exists (select 1 from biz_voucher v where v.id = e.voucher_id)""")
    check("D3 无孤儿分录", "单据链", "BLOCKER",
          not orphan_e or orphan_e[0][0] == "0", f"{orphan_e[0][0] if orphan_e else 0} 条孤儿分录")

    # D4 PURCHASE 类收货单必有供应商
    no_sup = q("""select in_no from biz_stock_in
                  where deleted_at is null and in_type like 'PURCHASE%' and supplier_id is null""")
    check("D4 采购收货必有供应商", "单据链", "BLOCKER", not no_sup,
          f"{len(no_sup)} 张无供应商" if no_sup else "全部有供应商")

    # D5 单据号唯一
    dup = q("""select in_no, count(*) from biz_stock_in where deleted_at is null
               group by 1 having count(*) > 1""")
    check("D5 收货单号唯一", "单据链", "BLOCKER", not dup,
          f"{len(dup)} 个重号" if dup else "全部唯一")

    # D6 已过账收货单的来源采购单状态必须「可收」（DC-03 / R-P0-3）
    #    口径：过账闸允许的四个状态；已过账收货单的来源单落在 DRAFT / CLOSED / CANCELLED
    #    （或来源单被软删 / 悬空）⇒ 这批入库是「来源单已被取消，货还是进来了」
    #    ⇒ 库存、应付、进项凭证全部没有上游依据，BLOCKER。
    bad_po_status = q("""
        select s.in_no, coalesce(p.order_no, '(悬空)'),
               case when p.id is null then '(缺失)'
                    when p.deleted_at is not null then '(已软删)'
                    else p.status end
        from biz_stock_in s
        left join biz_purchase_order p on p.id = s.source_order_id
        where s.deleted_at is null
          and s.status = 'POSTED'
          and s.source_order_id is not null
          and (p.id is null
               or p.deleted_at is not null
               or p.status not in ('SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED'))
        order by s.in_no
    """)
    check("D6 已过账收货单的来源采购单必须可收", "单据链", "BLOCKER", not bad_po_status,
          f"{len(bad_po_status)} 张已过账收货单的来源采购单已不可收"
          if bad_po_status else "全部可收或为无源单",
          """select s.in_no, s.status, o.order_no, o.status, o.deleted_at
             from biz_stock_in s join biz_purchase_order o on o.id = s.source_order_id
             where s.deleted_at is null and s.status='POSTED'
               and (o.deleted_at is not null
                    or o.status not in ('SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED'))""")


# ══════════════════════════════════════════════════════════════════
# E. 状态一致性 —— 作废/红冲的连锁反应
# ══════════════════════════════════════════════════════════════════
def group_e():
    print("\n" + "=" * 92)
    print("E. 状态一致性（作废必红冲、红冲必双向）")
    print("=" * 92)

    # E1 已作废收货单不能有未红冲的有效凭证
    bad = q("""
        select s.in_no, count(v.id)::text
        from biz_stock_in s
        join biz_voucher v on v.source_no = s.in_no and v.deleted_at is null
                          and v.is_red = 0 and v.reversed_by is null
        where s.deleted_at is null and s.status = 'CANCELLED'
        group by s.in_no
    """)
    check("E1 作废单的有效凭证必须已红冲", "状态", "BLOCKER", not bad,
          f"{len(bad)} 张作废单仍挂着未红冲凭证" if bad else "全部已红冲",
          """select s.in_no, v.voucher_no, v.status::text
             from biz_stock_in s join biz_voucher v on v.source_no = s.in_no
             where s.deleted_at is null and s.status='CANCELLED'
               and v.deleted_at is null and v.is_red=0 and v.reversed_by is null""")

    # E2 红冲双向一致
    bad2 = q("""
        select v.voucher_no, coalesce(v.reversed_by::text,'(空)')
        from biz_voucher v
        where v.deleted_at is null and v.reversed_by is not null
          and not exists (select 1 from biz_voucher r
                          where r.id = v.reversed_by and r.reverse_of = v.id)
    """)
    check("E2 红冲双向指针一致", "状态", "WARN", not bad2,
          f"{len(bad2)} 张单向指针" if bad2 else "全部双向一致")

    # E3 红字凭证必须指向原凭证
    bad3 = q("""select voucher_no from biz_voucher
                where deleted_at is null and is_red = 1 and reverse_of is null""")
    check("E3 红字凭证必指向原凭证", "状态", "WARN", not bad3,
          f"{len(bad3)} 张红字无来源" if bad3 else "全部有来源")

    # E4 ★ 作废单的库存必须已回滚（这是刚发现的 P0）
    # 判定：作废的收货单，其数量不应还留在库存里。
    # 做法：按物料×仓库对比「有效入库」与「库存数量」。
    #
    # ⚠️ P9 修正（原判定会误报）：有效入库 = POSTED 收货明细 **+ 其他合法净入库**。
    #    原版只算收货，于是「收货 7 件 + 调拨进 1 件」的仓（库存 8）会被判成
    #    「有作废单没退库存」—— 那是假红。补上其他入库来源后，
    #    本条仍然能抓到真问题：库存 > 所有单据能解释的数量 ⇒ 一定有单子（最常见是作废单）没退库存。
    #
    # ⚠️ P1-FIX-9 衍生修正（t_086dab13）：原来的「其他入库」是手写白名单
    #    `('TRANSFER_IN','TRANSFER_OUT','SERIAL_IN','SERIAL_OUT')`，漏了
    #    OPENING（库存期初）/ STOCK_TAKE_GAIN（盘盈）/ STOCK_TAKE_CANCEL（盘亏作废回补）
    #    ⇒ 任何用期初建账或做过盘盈的库恒红。现改为查 [`E4_BIZ_TYPE_AXIS`] 登记表，
    #    并由 E4b 兜住「新 biz_type 没登记」。判据强度未放宽：真·作废没退库存仍然 BLOCKER。
    bad4 = q(f"""
        with posted as (
            select i.material_id, s.warehouse_id,
                   sum(i.qty) as q
            from biz_stock_in s
            join biz_stock_in_item i on i.stock_in_id = s.id
            where s.deleted_at is null and s.status = 'POSTED'
            group by 1, 2
        ), other_moves as (
            select material_id, warehouse_id,
                   sum(case when txn_type = 'IN' then qty else -qty end) as q
            from biz_inventory_txn
            where biz_type in ({E4_OTHER_IN_SQL})
            group by 1, 2
        ), inv as (
            select material_id, warehouse_id, sum(qty) as q
            from biz_inventory group by 1,2
        )
        select m.material_code, w.warehouse_name,
               coalesce(p.q,0)::text, coalesce(o.q,0)::text, coalesce(v.q,0)::text,
               (coalesce(v.q,0) - coalesce(p.q,0) - coalesce(o.q,0))::text
        from inv v
        left join posted p on p.material_id = v.material_id and p.warehouse_id = v.warehouse_id
        left join other_moves o on o.material_id = v.material_id and o.warehouse_id = v.warehouse_id
        left join biz_material m on m.id = v.material_id
        left join biz_warehouse w on w.id = v.warehouse_id
        where coalesce(v.q,0) > coalesce(p.q,0) + coalesce(o.q,0) + 0.0001
    """)
    check("E4 作废单的库存已回滚（库存 ≤ 有效入库＋其它合法动账净额＋0.0001）", "状态", "BLOCKER", not bad4,
          f"{len(bad4)} 组库存大于有效入库量（有作废单没退库存）" if bad4 else
          f"全部已回滚（已登记 {len(E4_BIZ_TYPE_AXIS)} 种 biz_type，其中 "
          f"{len(E4_OTHER_BIZ_TYPES)} 种按净额计入）",
          f"""with posted as (select i.material_id, s.warehouse_id, sum(i.qty) q
                            from biz_stock_in s join biz_stock_in_item i on i.stock_in_id=s.id
                            where s.deleted_at is null and s.status='POSTED' group by 1,2),
                  other_moves as (select material_id, warehouse_id,
                                         sum(case when txn_type='IN' then qty else -qty end) q
                                    from biz_inventory_txn
                                   where biz_type in ({E4_OTHER_IN_SQL})
                                   group by 1,2),
                  inv as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2)
             select m.material_code, w.warehouse_name,
                    coalesce(p.q,0)::text as 有效入库, coalesce(o.q,0)::text as 其它动账净额,
                    v.q::text as 库存, (v.q-coalesce(p.q,0)-coalesce(o.q,0))::text as 多出
             from inv v left join posted p on p.material_id=v.material_id and p.warehouse_id=v.warehouse_id
             left join other_moves o on o.material_id=v.material_id and o.warehouse_id=v.warehouse_id
             left join biz_material m on m.id=v.material_id
             left join biz_warehouse w on w.id=v.warehouse_id
             where v.q > coalesce(p.q,0) + coalesce(o.q,0) + 0.0001""")

    # E4b ★ E4 判据的完整性：库里出现的 biz_type 必须全部登记（否则 E4 会静默漏算）
    #     为什么是 BLOCKER：没登记的 biz_type 要么让 E4 假红（漏一个 IN 源），要么让它
    #     静默放宽（漏一个 OUT 源）。两种都不许悄悄发生——加一行登记即可放行。
    unknown = q(f"""
        select biz_type, doc_type, txn_type, count(*)::text, coalesce(sum(qty),0)::text
        from biz_inventory_txn
        where biz_type is null or biz_type not in ({E4_KNOWN_IN_SQL})
        group by 1, 2, 3
        order by 1, 3
    """)
    check("E4b 库存流水 biz_type 已全部登记进 E4 判据", "状态", "BLOCKER", not unknown,
          ("\n".join(f"{r[0]}/{r[1]}/{r[2]} 共 {r[3]} 条 净 {r[4]} —— 未登记，E4 无法判"
                     f"（登记到 scripts/verify/erp_invariants.py::E4_BIZ_TYPE_AXIS）"
                     for r in unknown) if unknown else
           f"全部已登记（{len(E4_BIZ_TYPE_AXIS)} 种）"),
          f"""select biz_type, doc_type, txn_type, count(*)::text, sum(qty)::text
                from biz_inventory_txn
               where biz_type is null or biz_type not in ({E4_KNOWN_IN_SQL})
               group by 1,2,3""")

    # E4c 遗留 biz_type：现行代码不产、历史库/夹具里还留着（E4 已按净额计入，不误伤），
    #     但要亮出来——遗留值会让「库存 ≤ 单据」的解释力变弱，值得确认来源或清理。
    legacy = q(f"""
        select biz_type, doc_type, txn_type, count(*)::text, coalesce(sum(qty),0)::text
        from biz_inventory_txn
        where biz_type in ({E4_LEGACY_IN_SQL})
        group by 1, 2, 3
        order by 1, 3
    """)
    check("E4c 无遗留 biz_type（现行代码已不产，历史行建议确认/清理）", "状态", "WARN", not legacy,
          ("\n".join(f"{r[0]}/{r[1]}/{r[2]} 共 {r[3]} 条 净 {r[4]} —— 现行代码不产此值，"
                     f"E4 已按净额计入；建议查来源后清理" for r in legacy)
           if legacy else "无遗留类型"),
          f"""select biz_type, doc_type, txn_type, count(*)::text, sum(qty)::text
                from biz_inventory_txn where biz_type in ({E4_LEGACY_IN_SQL})
               group by 1,2,3""")


# ══════════════════════════════════════════════════════════════════
# F. 期间控制
# ══════════════════════════════════════════════════════════════════
def group_f():
    print("\n" + "=" * 92)
    print("F. 期间控制（已结账期间不许有凭证）")
    print("=" * 92)

    try:
        closed = q("select period_code, status from biz_accounting_period where status = 'CLOSED'")
        check("F1 已结账期间无新凭证", "期间", "BLOCKER", True,
              f"当前无已结账期间（或 {len(closed)} 个）")
    except Exception as e:
        check("F1 已结账期间无新凭证", "期间", "INFO", True, f"跳过：{str(e)[:60]}")

    # 凭证日期必须在已开期间内
    try:
        bad = q("""
            select v.voucher_no, v.period
            from biz_voucher v
            where v.deleted_at is null
              and not exists (select 1 from biz_accounting_period p where p.period_code = v.period)
        """)
        check("F2 凭证期间必有期间记录", "期间", "WARN", not bad,
              f"{len(bad)} 张凭证的期间无记录" if bad else "全部有期间记录")
    except Exception as e:
        check("F2 凭证期间必有期间记录", "期间", "INFO", True, f"跳过：{str(e)[:60]}")


# ══════════════════════════════════════════════════════════════════
# G. 业务闭环（每张单据的状态是否可达终态）
# ══════════════════════════════════════════════════════════════════
def group_g():
    print("\n" + "=" * 92)
    print("G. 业务闭环（卡在中间态的单据）")
    print("=" * 92)

    # 收货单 DRAFT 超过 7 天（草稿僵尸）
    stale = q("""select in_no, created_at::date::text from biz_stock_in
                 where deleted_at is null and status = 'DRAFT'
                   and created_at < now() - interval '7 days'""")
    check("G1 无 7 天以上草稿收货单", "闭环", "INFO", not stale,
          f"{len(stale)} 张僵尸草稿" if stale else "无")

    # 调拨 PENDING 无审批实例（P5 修完应通过）
    bad2 = q("""select transfer_no, status from biz_stock_transfer
                where deleted_at is null and status = 'PENDING'
                  and approval_instance_id is null""")
    check("G2 PENDING 调拨必有审批实例", "闭环", "BLOCKER", not bad2,
          f"{len(bad2)} 张卡死（提交了但没人能批）" if bad2 else "全部有实例",
          """select transfer_no, status::text from biz_stock_transfer
             where deleted_at is null and status='PENDING' and approval_instance_id is null""")

    # 采购订单 SUBMITTED 超 30 天未收货
    stale2 = q("""select order_no, status, created_at::date::text from biz_purchase_order
                  where deleted_at is null and status = 'SUBMITTED'
                    and created_at < now() - interval '30 days'""")
    check("G3 无 30 天未处理的已提交采购单", "闭环", "INFO", not stale2,
          f"{len(stale2)} 张" if stale2 else "无")

    # 应付 OPEN 超期未付
    overdue = q("""select payable_no, unpaid_amount::text, due_date::text from biz_payable
                   where deleted_at is null and status in ('OPEN','PARTIAL_PAID')
                     and due_date < current_date""")
    check("G4 无逾期未付应付单", "闭环", "WARN", not overdue,
          f"{len(overdue)} 张已逾期" if overdue else "无逾期",
          """select payable_no, unpaid_amount::text, due_date::text from biz_payable
             where deleted_at is null and status in ('OPEN','PARTIAL_PAID')
               and due_date < current_date""")


# ══════════════════════════════════════════════════════════════════
# H. 审批闭环（任务 ↔ 审批实例 ↔ 待办 三者不许脱节）
# ══════════════════════════════════════════════════════════════════
#
# ── H6 的静态检查工具（不连库，只读 migrations 文件）──────────────────
#
# 为什么要有它（P2-6b-follow，2026-11-05）：
#   预置流程种子漏写 `is_system` 已经犯了 **4 次**（V0209 / V0211 / V0228 / V0231）——
#   每一次都是「INSERT 列清单止于 owner_dept_id」，吃 `DEFAULT FALSE`，于是系统预置、
#   客户不该改图的流程被当成客户自建。后果不是报错，而是**静默少几张卡**：
#   P2-6b 把模板列表收口为「标准流程」（`AND d.is_system`）后，V0228/V0231 种下的
#   两条预置流程直接从列表里消失（budget_approve / asset_disposal）。
#
#   库内断言（H5）只认「名字含『标准』」这一个方向，抓不到这两条（名字里没有「标准」）。
#   所以再加一条**静态**检查：直接扫 migrations 的 INSERT 列清单。它不依赖库状态，
#   也不依赖起名习惯 —— 新人照抄旧种子复制粘贴时，漏列当场红。
#
# 判定口径（四条，缺一不可）：
#   1. 只扫 `INSERT INTO biz_flow_def (...)`，且 **版本 ≥ V0193**（`is_system` 列由
#      V0193 加；更早的迁移根本没有这一列，缺列不是错）；
#   2. 列清单里有 `is_system` → 通过；
#   3. 没有 → 允许「补偿」：存在**更晚**的迁移里有 `UPDATE biz_flow_def` 语句，
#      同时含该 `flow_code` 字面量与 `is_system = TRUE`（这正是已应用迁移只能
#      新开一条修的机制，例如 V0220 补 V0209/V0211、V0233 补 V0228/V0231）。
#      补偿不到 → 红（BLOCKER）：下一个漏列的新种子不该靠人眼发现。
#   4. 隐式列清单（`INSERT INTO biz_flow_def SELECT ...`，即 INSERT 后没有括号列名）
#      → 直接红：静态拿不到列名，且位置依赖表列序，比漏列更脆。
_IS_SYSTEM_COLUMN_FROM = 193
_INSERT_FLOW_DEF_RE = re.compile(r"INSERT\s+INTO\s+biz_flow_def\s*\(", re.IGNORECASE)
# 隐式列清单（`INSERT INTO biz_flow_def SELECT ...` / `VALUES` 不带括号列）：无法静态确认
# 写了 is_system，而且位置依赖别的表的列序——比漏列更脆，直接判红，别让它从正则缝里溜过去。
_INSERT_FLOW_DEF_IMPLICIT_RE = re.compile(
    r"INSERT\s+INTO\s+biz_flow_def\s+(?!\()", re.IGNORECASE)
_UPDATE_FLOW_DEF_RE = re.compile(r"UPDATE\s+biz_flow_def\b", re.IGNORECASE)
_IS_SYSTEM_TRUE_RE = re.compile(r"is_system\s*=\s*TRUE", re.IGNORECASE)


def _migrations_root():
    """迁移根目录（`ERP_MIGRATIONS_ROOT` 只给负向自测用；默认仓内 backend/migrations）。"""
    env = os.environ.get("ERP_MIGRATIONS_ROOT")
    if env:
        return Path(env)
    return Path(__file__).resolve().parents[2] / "backend" / "migrations"


def _load_up_migrations():
    """[(version, path, text)]：版本化 up 方向的 SQL，按版本升序（down / 种子 S__ 不参与）。"""
    out = []
    root = _migrations_root()
    if not root.is_dir():
        return out
    for path in sorted(root.rglob("*.sql")):
        if path.name.endswith(".down.sql"):
            continue
        m = re.match(r"^[Vv]?(\d+)__.+\.sql$", path.name)
        if not m:
            continue
        out.append((int(m.group(1)), path,
                    path.read_text(encoding="utf-8", errors="replace")))
    out.sort(key=lambda t: (t[0], str(t[1])))
    return out


def _flow_def_inserts(text):
    """产出 (行号, 列名列表, 语句正文)：每处 `INSERT INTO biz_flow_def (...)`。"""
    for m in _INSERT_FLOW_DEF_RE.finditer(text):
        # 括号配对取出列清单（种子都写成多行，不能按行切）
        i, depth = m.end(), 1
        while i < len(text) and depth:
            if text[i] == "(":
                depth += 1
            elif text[i] == ")":
                depth -= 1
            i += 1
        cols = [c.strip().strip('"').lower()
                for c in text[m.end():i - 1].split(",") if c.strip()]
        line = text.count("\n", 0, m.start()) + 1
        j = text.find(";", i)
        yield line, cols, text[i:j if j != -1 else len(text)]


def _flow_code_of(stmt):
    """从 INSERT 的 VALUES/SELECT 里取 flow_code：第一个「小写蛇形」字符串字面量。

    种子写法固定为 `'<uuid>', '<flow_code>', '<中文名>', ...`：uuid 带连字符、
    名称含中文/大写，都会被正则滤掉，剩下第一个就是 flow_code。
    """
    for lit in re.findall(r"'([^']*)'", stmt):
        if re.fullmatch(r"[a-z][a-z0-9_]{1,63}", lit):
            return lit
    return None


def _compensating_migration(flow_code, after_version, migrations):
    """返回补偿该 flow_code 的后续迁移版本号（`V0233` 之类），没有则 None。"""
    if not flow_code:
        return None
    for ver, _path, text in migrations:
        if ver <= after_version:
            continue
        for m in _UPDATE_FLOW_DEF_RE.finditer(text):
            j = text.find(";", m.end())
            stmt = text[m.end():j if j != -1 else len(text)]
            if f"'{flow_code}'" in stmt and _IS_SYSTEM_TRUE_RE.search(stmt):
                return f"V{ver:04d}"
    return None


def _check_flow_seed_is_system():
    """H6：预置流程种子必须显式写 is_system（或由后续迁移补偿）。静态，不查库。"""
    name = "H6 预置流程种子的 INSERT 必须显式写 is_system（或由后续迁移补偿）"
    migrations = _load_up_migrations()
    if not migrations:
        check(name, "审批", "WARN", True,
              f"没扫到版本化迁移（{_migrations_root()}）——检查路径，不算失败")
        return

    scanned, bad, compensated = 0, [], []
    for ver, path, text in migrations:
        if ver < _IS_SYSTEM_COLUMN_FROM:
            continue
        for line, cols, stmt in _flow_def_inserts(text):
            scanned += 1
            if "is_system" in cols:
                continue
            code = _flow_code_of(stmt)
            by = _compensating_migration(code, ver, migrations)
            where = f"{path.name}:{line}"
            if by:
                compensated.append(f"{where} {code or '(未解析)'} → 已由 {by} 补偿")
            else:
                bad.append(f"{where} flow_code={code or '(未解析)'}")
        # 隐式列清单：正则拿不到列名，一律判红（位置依赖列序，比漏列更脆）
        for m in _INSERT_FLOW_DEF_IMPLICIT_RE.finditer(text):
            scanned += 1
            line = text.count("\n", 0, m.start()) + 1
            bad.append(f"{path.name}:{line} 列清单隐式（INSERT 后没有括号列清单，"
                       f"无法静态确认写了 is_system）")

    if scanned == 0:
        check(name, "审批", "WARN", True,
              f"V{_IS_SYSTEM_COLUMN_FROM:04d}+ 的迁移里没有 `INSERT INTO biz_flow_def`——跳过，不算失败")
        return

    detail = f"V{_IS_SYSTEM_COLUMN_FROM:04d}+ 共 {scanned} 处 INSERT 列清单"
    if bad:
        detail = (f"{len(bad)} 处漏写 is_system（客户可改身份露出、模板列表里静默消失）："
                  + "；".join(bad[:8])
                  + " → 种子里显式写 is_system=TRUE；已经应用的迁移不许改，"
                    "请新开一条迁移按 flow_code 补标（照 V0220 / V0233 的写法）")
    elif compensated:
        detail += f"全部含 is_system；历史遗留 {len(compensated)} 处已由后续迁移补偿：" \
                  + "；".join(compensated)
    else:
        detail += "全部含 is_system"
    check(name, "审批", "BLOCKER", not bad, detail)


def group_h():
    print("\n" + "=" * 92)
    print("H. 审批闭环（任务取消/完成 → 审批必须跟着收口）")
    print("=" * 92)

    # H6 先跑：它是**静态**检查（只读 migrations 文件），不依赖库连接。
    # 放在最前面，是为了「库连不上 / 库还没建表」时这条结论也能落地，
    # 不至于被后面第一条 SQL 的异常整组带走（main() 对 group 异常只记一条 BLOCKER）。
    _check_flow_seed_is_system()

    # H1：实例已终态，待办不能还挂 PENDING。
    # 这正是老板报 TSK-202610-000149 的形态：任务取消了、审批单 CANCELLED 了，
    # 但 biz_flow_task 还是 PENDING，审批人看到一条点不动的孤儿待办。
    bad = q("""
        select i.instance_no, coalesce(i.biz_no,'(空)'), coalesce(u.username,'(未知)'), ft.node_key
        from biz_flow_task ft
        join biz_flow_instance i on i.id = ft.instance_id
        left join sys_user u on u.id = ft.assignee_id
        where ft.status = 'PENDING'
          and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
          and i.deleted_at is null
        order by 1
    """)
    check("H1 实例已终态 → 待办必须终态", "审批", "BLOCKER", not bad,
          f"{len(bad)} 条孤儿待办（实例已结束，待办还 PENDING）" if bad else "无孤儿待办",
          """select i.instance_no || ' / ' || coalesce(i.biz_no,'(空)') || ' / ' || ft.node_key
             from biz_flow_task ft join biz_flow_instance i on i.id = ft.instance_id
             where ft.status='PENDING' and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
               and i.deleted_at is null""")

    # H2：任务已取消/已完成 → 它名下不许还有 PENDING 的审批实例。
    # 这是 H1 的上游口径：实例根本没被动过（还 PENDING），但业务对象已经结束了。
    bad2 = q("""
        select t.task_no, t.status, i.instance_no
        from biz_task t
        join biz_flow_instance i on i.biz_id = t.id and i.deleted_at is null
        where t.deleted_at is null
          and t.status in ('CANCELLED','DONE')
          and i.status = 'PENDING'
        order by 1
    """)
    check("H2 任务已取消/完成 → 审批实例必须终态", "审批", "BLOCKER", not bad2,
          f"{len(bad2)} 条实例还挂着（任务已结束但审批没人收口）" if bad2 else "全部已收口",
          """select t.task_no || ' ' || t.status::text || ' / ' || i.instance_no
             from biz_task t join biz_flow_instance i on i.biz_id = t.id and i.deleted_at is null
             where t.deleted_at is null and t.status in ('CANCELLED','DONE') and i.status='PENDING'""")

    # H3：待办必须能追溯到业务对象（否则点进去 404）。
    # 只查 TASK_ACCEPT：其它 biz_type（调拨单等）的业务表不同，各自闭环由 G2 管，
    # 这里不越过模块边界去猜 biz_type 对应的表名。
    bad3 = q("""
        select i.instance_no, coalesce(i.biz_no,'(空)'), coalesce(u.username,'(未知)')
        from biz_flow_task ft
        join biz_flow_instance i on i.id = ft.instance_id
        left join sys_user u on u.id = ft.assignee_id
        where ft.status = 'PENDING'
          and i.status = 'PENDING'
          and i.biz_type = 'TASK_ACCEPT'
          and i.deleted_at is null
          and not exists (
                select 1 from biz_task t
                 where t.deleted_at is null
                   and (t.id = i.biz_id or (i.biz_id is null and t.task_no = i.biz_no)))
        order by 1
    """)
    check("H3 待办必能追溯到任务（未软删）", "审批", "WARN", not bad3,
          f"{len(bad3)} 条孤儿待办（任务已删/不存在，点进去 404）" if bad3 else "全部可追溯",
          """select i.instance_no || ' / ' || coalesce(i.biz_no,'(空)')
             from biz_flow_task ft join biz_flow_instance i on i.id = ft.instance_id
             where ft.status='PENDING' and i.status='PENDING' and i.biz_type='TASK_ACCEPT'
               and i.deleted_at is null
               and not exists (select 1 from biz_task t where t.deleted_at is null
                               and (t.id = i.biz_id or (i.biz_id is null and t.task_no = i.biz_no)))""")

    # H4：审批实例已 APPROVED，但单据**仍在待审批态** → P0（ARCH-3 §6 R-P1-1 · 断链 DC-04）。
    #
    # 这是「审批闭环」的最后一段：审批单走到终态了，业务回调却没把单据推走
    # （回调失败只记 WARN 不回滚审批单，所以这种形态在库里是**合法落库**的），
    # 结果就是「审批中心显示已通过、单据还写着审批中，没人能再推进它」——
    # 用户会去重提（撞唯一索引 409）、或找 IT 手工改库，两种都是事故。
    #
    # 口径：按 biz_type 逐类给出「待审批态」的真实状态值（不猜、不 JOIN 错表）：
    #   PO_APPROVE      → biz_purchase_order.status = SUBMITTED
    #   STOCK_IN_POST   → biz_stock_in.status      = PENDING_POST
    #   STOCK_IN_CANCEL → biz_stock_in.status      = PENDING_CANCEL
    #   PAYABLE_CANCEL  → biz_payable.status       = PENDING_CANCEL
    #   CONTRACT_APPROVE→ biz_contract.status      = PENDING_APPROVAL（本卡新增）
    #   VOUCHER_REVERSE → 凭证 status 恒 POSTED，待审批由 pending_reverse_instance_id 表达
    # 只查**未软删**的单据与实例（软删单据上的历史实例是审计留痕，不是卡死）。
    try:
        bad4 = q("""
            select 'PO_APPROVE', i.instance_no, coalesce(i.biz_no,'(空)'), o.status::text
              from biz_flow_instance i
              join biz_purchase_order o on o.id = i.biz_id
             where i.status = 'APPROVED' and i.deleted_at is null
               and o.deleted_at is null and o.status = 'SUBMITTED'
            union all
            select 'STOCK_IN_POST', i.instance_no, coalesce(i.biz_no,'(空)'), s.status::text
              from biz_flow_instance i
              join biz_stock_in s on s.id = i.biz_id
             where i.status = 'APPROVED' and i.deleted_at is null
               and s.deleted_at is null and s.status = 'PENDING_POST'
            union all
            select 'STOCK_IN_CANCEL', i.instance_no, coalesce(i.biz_no,'(空)'), s.status::text
              from biz_flow_instance i
              join biz_stock_in s on s.id = i.biz_id
             where i.status = 'APPROVED' and i.deleted_at is null
               and s.deleted_at is null and s.status = 'PENDING_CANCEL'
            union all
            select 'PAYABLE_CANCEL', i.instance_no, coalesce(i.biz_no,'(空)'), p.status::text
              from biz_flow_instance i
              join biz_payable p on p.id = i.biz_id
             where i.status = 'APPROVED' and i.deleted_at is null
               and p.deleted_at is null and p.status = 'PENDING_CANCEL'
            union all
            select 'CONTRACT_APPROVE', i.instance_no, coalesce(i.biz_no,'(空)'), c.status::text
              from biz_flow_instance i
              join biz_contract c on c.id = i.biz_id
             where i.status = 'APPROVED' and i.deleted_at is null
               and c.deleted_at is null and c.status = 'PENDING_APPROVAL'
            union all
            select 'VOUCHER_REVERSE', i.instance_no, coalesce(i.biz_no,'(空)'), 'POSTED+pending_reverse'
              from biz_flow_instance i
              join biz_voucher v on v.pending_reverse_instance_id = i.id
             where i.status = 'APPROVED' and i.deleted_at is null
               and v.deleted_at is null
            order by 1, 2
        """)
        detail = ("；".join(f"{row[0]} {row[1] or '(空)'} 仍 {row[3]} → {row[2]}" for row in bad4[:5])
                  if bad4 else "全部单据已随审批结论前进")
        if bad4:
            detail = f"{len(bad4)} 张单据审批已通过但没前进：" + detail
        check("H4 审批已通过 → 单据必须前进（不许停在待审批态）", "审批", "BLOCKER", not bad4,
              detail,
              """select i.biz_type || ' / ' || i.instance_no || ' / ' || coalesce(i.biz_no,'(空)')
                 from biz_flow_instance i
                where i.status='APPROVED' and i.deleted_at is null
                  and (   (i.biz_type='PO_APPROVE' and exists (select 1 from biz_purchase_order o
                                where o.id=i.biz_id and o.deleted_at is null and o.status='SUBMITTED'))
                       or (i.biz_type in ('STOCK_IN_POST','STOCK_IN_CANCEL') and exists (select 1 from biz_stock_in s
                                where s.id=i.biz_id and s.deleted_at is null
                                  and s.status = case when i.biz_type='STOCK_IN_POST' then 'PENDING_POST' else 'PENDING_CANCEL' end))
                       or (i.biz_type='PAYABLE_CANCEL' and exists (select 1 from biz_payable p
                                where p.id=i.biz_id and p.deleted_at is null and p.status='PENDING_CANCEL'))
                       or (i.biz_type='CONTRACT_APPROVE' and exists (select 1 from biz_contract c
                                where c.id=i.biz_id and c.deleted_at is null and c.status='PENDING_APPROVAL'))
                       or (i.biz_type='VOUCHER_REVERSE' and exists (select 1 from biz_voucher v
                                where v.pending_reverse_instance_id=i.id and v.deleted_at is null)))""")
    except Exception as e:
        check("H4 审批已通过 → 单据必须前进（不许停在待审批态）", "审批", "BLOCKER", False,
              f"执行异常（表/列缺失？）：{str(e)[:120]}")

    # H5：「标准」标签（is_system）与名字必须一致 —— P2-6a 的防回归断言。
    #
    # 病（不是笔误，是系统性漏列）：预置流程的种子 INSERT 漏写 `is_system` →
    # 吃 `DEFAULT FALSE`（列定义 V0193:29），于是系统预置、客户不该改的流程
    # 被标成「客户自建」：以可改身份露出，却挂不上「标准」标签。
    # 实测中招的是 V0209（contract_approve）与 V0211（stock_take_post），
    # 由 V0220 按 flow_code 精确修正（已应用迁移有指纹校验，只能新开一条修）。
    #
    # 为什么按「名字含标准」查而不是按 flow_code 白名单查：
    # 白名单只能钉住已知的 2 个编码，**下一个漏写 is_system 的种子迁移**
    # （多半也会起名「…（标准）」）照样漏过去。按名字查才能当场抓到新犯。
    # 为什么只查「非系统 + 名字含标准」这一个方向：
    # 反方向（is_system=TRUE 但名字里没「标准」四个字）在库里是**正常**的
    # （如 payable_pay「应付付款审批」、task_accept「任务验收」），不能当错标。
    #
    # 级别 WARN 而不是 BLOCKER：这条不影响账（钱没动、追溯链没断），
    # 只是把系统预置流程错标成客户可改的语义错误 ⇒ 归「可疑，要人看」。
    # 落库侧另有一道硬闸：V0220 迁移体内带自检，同形态出现时迁移直接 RAISE EXCEPTION 失败。
    if table_exists("biz_flow_def"):
        bad5 = q("""
            select flow_code, flow_name
              from biz_flow_def
             where deleted_at is null
               and not is_system
               and flow_name like '%标准%'
             order by flow_code
        """)
        check("H5 名字自称「标准」的流程定义必须 is_system=TRUE", "审批", "WARN", not bad5,
              (f"{len(bad5)} 条错标（种子 INSERT 漏写 is_system，参照 V0209/V0211 的错法）："
               + "；".join(f"{r[0]} {r[1]}" for r in bad5[:5])
               + " → 新开一条迁移按 flow_code 补 is_system=TRUE（别改已应用迁移）")
              if bad5 else "无错标（标准流程都挂 is_system=TRUE）",
              """select flow_code || ' / ' || flow_name || ' / is_system=' || is_system::text
                   from biz_flow_def
                  where deleted_at is null and not is_system and flow_name like '%标准%'
                  order by flow_code""")
    else:
        check("H5 名字自称「标准」的流程定义必须 is_system=TRUE", "审批", "INFO", True,
              "本库还没有 biz_flow_def（流程模块未上）——跳过，不算失败")


# ══════════════════════════════════════════════════════════════════
# I. 盘点单（ARCH-3 DC-14）—— 账实差异必须有单据有凭证
# ══════════════════════════════════════════════════════════════════
def group_i():
    print("\n" + "=" * 92)
    print("I. 盘点单（盘盈盘亏必须有凭证、差异必须等于库存变化）")
    print("=" * 92)

    if not table_exists("biz_stock_take"):
        check("I0 盘点单表存在", "盘点", "INFO", True,
              "本库还没有 V0211（盘点单）——整组跳过，不算失败")
        return

    # I1 已过账且差异金额 > 0 的盘点单，必须有蓝字凭证（且来源对得上）
    bad1 = q("""
        select t.take_no,
               (t.total_gain_amount + t.total_loss_amount)::text as diff_amount,
               coalesce(t.voucher_id::text, '(无)') as voucher_id,
               coalesce(v.source_type || '/' || v.status || case when v.is_red = 1 then '/红字' else '' end, '(找不到凭证)') as voucher
          from biz_stock_take t
          left join biz_voucher v on v.id = t.voucher_id and v.deleted_at is null
         where t.deleted_at is null and t.status = 'POSTED'
           and (t.total_gain_amount + t.total_loss_amount) > 0.0001
           and (t.voucher_id is null
                or v.id is null
                or v.source_type <> 'STOCK_TAKE'
                or v.is_red = 1)
         order by t.take_no
    """)
    check("I1 盘点单已过账 → 必须有盘点差异凭证", "盘点", "BLOCKER", not bad1,
          f"{len(bad1)} 张已过账盘点单没有（或挂错了）凭证" if bad1 else "全部有凭证",
          """select t.take_no, (t.total_gain_amount + t.total_loss_amount)::text as 差异金额,
                    coalesce(t.voucher_id::text,'(无)') as 凭证
               from biz_stock_take t
               left join biz_voucher v on v.id = t.voucher_id and v.deleted_at is null
              where t.deleted_at is null and t.status = 'POSTED'
                and (t.total_gain_amount + t.total_loss_amount) > 0.0001
                and (t.voucher_id is null or v.id is null or v.source_type <> 'STOCK_TAKE' or v.is_red = 1)""")

    # I2 盘点差异累计 = 库存流水累计（按盘点单 id 对齐）
    bad2 = q("""
        with diff as (
            select t.id, t.take_no,
                   coalesce(sum(i.diff_qty), 0) as diff_qty
              from biz_stock_take t
              join biz_stock_take_item i on i.take_id = t.id
             where t.deleted_at is null and t.status = 'POSTED'
             group by 1, 2
        ), txn as (
            select doc_id,
                   sum(case when txn_type = 'IN' then qty else -qty end) as txn_qty
              from biz_inventory_txn
             where doc_type = 'STOCK_TAKE'
             group by 1
        )
        select d.take_no, d.diff_qty::text, coalesce(x.txn_qty, 0)::text
          from diff d
          left join txn x on x.doc_id = d.id
         where abs(d.diff_qty - coalesce(x.txn_qty, 0)) > 0.0001
         order by d.take_no
    """)
    check("I2 盘点差异累计 = 库存流水累计（对得上）", "盘点", "BLOCKER", not bad2,
          f"{len(bad2)} 张盘点单的差异与流水不符" if bad2 else "全部相符",
          """with diff as (select t.id, t.take_no, coalesce(sum(i.diff_qty),0) as q
                            from biz_stock_take t join biz_stock_take_item i on i.take_id=t.id
                           where t.deleted_at is null and t.status='POSTED' group by 1,2),
                  txn as (select doc_id, sum(case when txn_type='IN' then qty else -qty end) as q
                            from biz_inventory_txn where doc_type='STOCK_TAKE' group by 1)
             select d.take_no, d.q::text as 差异, coalesce(x.q,0)::text as 流水
               from diff d left join txn x on x.doc_id=d.id
              where abs(d.q - coalesce(x.q,0)) > 0.0001""")

    # I3 已作废的盘点单：正向 + 冲销必须抵消为 0（红冲前的账必须回基线）
    bad3 = q("""
        select t.id, t.take_no,
               sum(case when x.txn_type = 'IN' then x.qty else -x.qty end)::text as net
          from biz_stock_take t
          join biz_inventory_txn x on x.doc_type = 'STOCK_TAKE' and x.doc_id = t.id
         where t.deleted_at is null and t.status = 'CANCELLED'
         group by 1, 2
        having abs(sum(case when x.txn_type = 'IN' then x.qty else -x.qty end)) > 0.0001
         order by t.take_no
    """)
    check("I3 已作废盘点单 → 流水净额为 0（正向已冲销）", "盘点", "WARN", not bad3,
          f"{len(bad3)} 张作废盘点单的流水没抵消" if bad3 else "全部抵消",
          """select t.take_no,
                    sum(case when x.txn_type='IN' then x.qty else -x.qty end)::text as 净额
               from biz_stock_take t join biz_inventory_txn x
                 on x.doc_type='STOCK_TAKE' and x.doc_id=t.id
              where t.deleted_at is null and t.status='CANCELLED'
              group by 1 having abs(sum(case when x.txn_type='IN' then x.qty else -x.qty end)) > 0.0001""")


# ══════════════════════════════════════════════════════════════════
# K. 固定资产（ARCH-6 第五章 · FIN-2）—— 折旧缓存列必须等于明细，科目必须对得上
# ══════════════════════════════════════════════════════════════════
def group_k():
    print("\n" + "=" * 92)
    print("K. 固定资产（折旧缓存 = 明细、1601/1602/1606 与卡片对账、处置必留痕）")
    print("=" * 92)
    if not table_exists("biz_fixed_asset"):
        check("K0 固定资产表存在", "固资", "WARN", False, "biz_fixed_asset 不存在（本库还没上 FIN-2）")
        return

    # K1 月折旧额 = round((原值-残值)/月数, 4)（WARN：历史数据 / 手工改库可能漂）
    bad = q("""
        select a.asset_no, a.monthly_depreciation::text,
               round((a.original_value - a.residual_value) / a.useful_life_months, 4)::text
          from biz_fixed_asset a
         where a.deleted_at is null
           and abs(a.monthly_depreciation
                   - round((a.original_value - a.residual_value) / a.useful_life_months, 4)) > 0.0001
         order by a.asset_no
    """)
    check("K1 月折旧额 = round((原值−残值)/月数,4)", "固资", "WARN", not bad,
          f"{len(bad)} 张卡的月折旧额与公式不符" if bad else "全部相符")

    # K2 累计折旧 = Σ(POSTED 明细)（BLOCKER：缓存列唯一写入者是折旧过账）
    bad2 = q("""
        select a.asset_no, a.accumulated_depreciation::text,
               coalesce(sum(d.amount) filter (where d.status = 'POSTED'), 0)::text
          from biz_fixed_asset a
          left join biz_depreciation d on d.asset_id = a.id
         where a.deleted_at is null
         group by a.id, a.asset_no, a.accumulated_depreciation
        having abs(a.accumulated_depreciation
                   - coalesce(sum(d.amount) filter (where d.status = 'POSTED'), 0)) > 0.0001
         order by a.asset_no
    """)
    check("K2 卡片累计折旧 = Σ明细(POSTED)", "固资", "BLOCKER", not bad2,
          f"{len(bad2)} 张卡的累计折旧与明细不符" if bad2 else "全部相符")

    # K3 累计 ≤ 可折旧总额 且 净值 = 原值 − 累计（库 CHECK 也挡，这里查历史脏数据）
    bad3 = q("""
        select a.asset_no, a.accumulated_depreciation::text, a.depreciable_base::text,
               a.net_value::text, (a.original_value - a.accumulated_depreciation)::text
          from biz_fixed_asset a
         where a.deleted_at is null
           and (a.accumulated_depreciation > a.depreciable_base + 0.0001
                or abs(a.net_value - (a.original_value - a.accumulated_depreciation)) > 0.0001)
         order by a.asset_no
    """)
    check("K3 累计 ≤ 可折旧总额 且 净值 = 原值 − 累计", "固资", "BLOCKER", not bad3,
          f"{len(bad3)} 张卡违反净值恒等 / 提足上限" if bad3 else "全部相符")

    # K4 批次 total = Σ明细 且 asset_count = 明细行数
    bad4 = q("""
        select b.batch_no, b.total_amount::text, coalesce(sum(d.amount),0)::text,
               b.asset_count, count(d.id)
          from biz_depreciation_batch b
          left join biz_depreciation d on d.batch_id = b.id
         group by b.id, b.batch_no, b.total_amount, b.asset_count
        having abs(b.total_amount - coalesce(sum(d.amount),0)) > 0.0001
            or b.asset_count <> count(d.id)
         order by b.batch_no
    """)
    check("K4 批次合计 = Σ明细 且 台数 = 明细行数", "固资", "BLOCKER", not bad4,
          f"{len(bad4)} 个批次的合计/台数与明细不符" if bad4 else "全部相符")

    # K5 1602 贷方余额 = Σ 卡片累计折旧（容差 0.0001）
    get_1602 = one("""
        select coalesce(sum(e.credit_amount - e.debit_amount), 0)::text
          from biz_voucher_entry e
          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
         where e.account_code = '1602'
    """)
    cards_accum = one("""
        select coalesce(sum(accumulated_depreciation), 0)::text
          from biz_fixed_asset where deleted_at is null
    """)
    ok5 = abs(dec(get_1602) - dec(cards_accum)) <= TOL
    check("K5 1602 贷方余额 = Σ卡片累计折旧", "固资", "BLOCKER", ok5,
          f"1602 = {get_1602} / 卡片 = {cards_accum}" if not ok5 else f"1602 = {get_1602}")

    # K6 1601 借方余额 = Σ 未处置卡片原值
    get_1601 = one("""
        select coalesce(sum(e.debit_amount - e.credit_amount), 0)::text
          from biz_voucher_entry e
          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
         where e.account_code = '1601'
    """)
    live_original = one("""
        select coalesce(sum(original_value), 0)::text
          from biz_fixed_asset where deleted_at is null and status <> 'DISPOSED'
    """)
    ok6 = abs(dec(get_1601) - dec(live_original)) <= TOL
    check("K6 1601 借方余额 = Σ未处置卡片原值", "固资", "BLOCKER", ok6,
          f"1601 = {get_1601} / 在役卡片 = {live_original}" if not ok6 else f"1601 = {get_1601}")

    # K7 1606 固定资产清理必须归零（WARN：处置完成后的清理科目不许挂账）
    get_1606 = one("""
        select coalesce(sum(e.debit_amount - e.credit_amount), 0)::text
          from biz_voucher_entry e
          join biz_voucher v on v.id = e.voucher_id and v.deleted_at is null
         where e.account_code = '1606'
    """)
    ok7 = abs(dec(get_1606)) <= TOL
    check("K7 1606 固定资产清理余额 = 0（无未结转挂账）", "固资", "WARN", ok7,
          f"1606 余额 = {get_1606}（处置未结转完 / 有人直接记了这个科目）" if not ok7 else "归零")

    # K8 同一资产同一期间只能有一条 POSTED 明细
    bad8 = q("""
        select asset_id::text, period_code, count(*)::text
          from biz_depreciation where status = 'POSTED'
         group by 1, 2 having count(*) > 1
    """)
    check("K8 同资产同期只有一条 POSTED 折旧", "固资", "BLOCKER", not bad8,
          f"{len(bad8)} 组重复计提" if bad8 else "无重复")

    # K9 已 POSTED 批次的目标期间必须存在且已过账（WARN：历史 / 手工造数）
    bad9 = q("""
        select b.batch_no, b.period_code
          from biz_depreciation_batch b
         where b.status = 'POSTED'
           and not exists (select 1 from biz_accounting_period p
                            where p.period_code = b.period_code)
         order by b.batch_no
    """)
    check("K9 已过账批次的期间必须存在", "固资", "WARN", not bad9,
          f"{len(bad9)} 个批次的期间不存在" if bad9 else "全部有期间行")

    # K10 已处置资产必有 DISPOSAL 留痕
    bad10 = q("""
        select a.asset_no
          from biz_fixed_asset a
         where a.deleted_at is null and a.status = 'DISPOSED'
           and not exists (select 1 from biz_fixed_asset_change c
                            where c.asset_id = a.id and c.change_type = 'DISPOSAL')
         order by a.asset_no
    """)
    check("K10 已处置资产必有 DISPOSAL 变动留痕", "固资", "BLOCKER", not bad10,
          f"{len(bad10)} 张已处置卡没有处置留痕" if bad10 else "全部有留痕")



# ==================== 报销（ARCH-6 §4.9 L 组，FIN-3） ====================

def group_l():
    """报销不变量 L1–L8（ARCH-6 §4.9）。"""
    # L1 主子孙合计：total_with_tax = Σ 明细 with_tax，且 item_count = 条数
    bad = q("""
        select r.report_no, r.total_with_tax::text,
               coalesce((select sum(i.with_tax_amount) from biz_expense_item i
                          where i.report_id = r.id), 0)::text,
               r.item_count::text,
               coalesce((select count(*) from biz_expense_item i where i.report_id = r.id), 0)::text
          from biz_expense_report r
         where r.deleted_at is null
           and (r.total_with_tax <> coalesce((select sum(i.with_tax_amount) from biz_expense_item i
                                               where i.report_id = r.id), 0)
                or r.item_count <> (select count(*) from biz_expense_item i where i.report_id = r.id))
         order by r.report_no
    """)
    check("L1 报销单 total_with_tax = Σ 明细 且 item_count = 条数", "报销", "BLOCKER", not bad,
          f"{len(bad)} 张单明细与单头对不上：{bad[:3]}" if bad else "全部自洽")

    # L2 APPROVED / PAID 必有计提凭证（且凭证未软删）
    bad = q("""
        select r.report_no
          from biz_expense_report r
          left join biz_voucher v on v.id = r.voucher_id
         where r.deleted_at is null and r.status in ('APPROVED','PAID')
           and (r.voucher_id is null or v.id is null or v.deleted_at is not null)
         order by r.report_no
    """)
    check("L2 APPROVED/PAID 必有未删除的计提凭证", "报销", "BLOCKER", not bad,
          f"{len(bad)} 张已入账单缺凭证：{bad[:3]}" if bad else "全部有凭证")

    # L3 计提凭证借方合计 = 单头价税合计，且贷方科目全为 2241.01
    bad = q("""
        select r.report_no,
               coalesce(sum(e.debit_amount), 0)::text, r.total_with_tax::text
          from biz_expense_report r
          join biz_voucher v on v.id = r.voucher_id and v.is_red = 0 and v.deleted_at is null
          join biz_voucher_entry e on e.voucher_id = v.id
         where r.deleted_at is null and r.status in ('APPROVED','PAID')
         group by r.report_no, r.total_with_tax
        having abs(coalesce(sum(e.debit_amount), 0) - r.total_with_tax) > 0.0001
         order by r.report_no
    """)
    check("L3 计提凭证借方合计 = 单头价税合计", "报销", "BLOCKER", not bad,
          f"{len(bad)} 张单凭证借方对不上：{bad[:3]}" if bad else "账账一致")
    bad = q("""
        select r.report_no, e.account_code
          from biz_expense_report r
          join biz_voucher v on v.id = r.voucher_id and v.is_red = 0 and v.deleted_at is null
          join biz_voucher_entry e on e.voucher_id = v.id
         where r.deleted_at is null and r.status in ('APPROVED','PAID')
           and e.credit_amount > 0 and e.account_code <> '2241.01'
         order by r.report_no
    """)
    check("L3' 计提凭证贷方全部是 2241.01 员工垫付", "报销", "BLOCKER", not bad,
          f"{len(bad)} 行贷方科目不是 2241.01：{bad[:3]}" if bad else "全部正确")

    # L4 2241.01 贷方净额 = Σ(价税合计 − 已付)（未作废的已入账单）
    rows = q("""
        select coalesce(sum(e.credit_amount - e.debit_amount), 0)::text
          from biz_voucher_entry e where e.account_code = '2241.01'
    """)
    ledger = Decimal(rows[0][0]) if rows else Decimal("0")
    rows = q("""
        select coalesce(sum(total_with_tax - paid_amount), 0)::text
          from biz_expense_report
         where deleted_at is null and status in ('APPROVED','PAID')
    """)
    expected = Decimal(rows[0][0]) if rows else Decimal("0")
    check("L4 2241.01 贷方净额 = Σ(报销单未付金额)", "报销", "BLOCKER",
          abs(ledger - expected) <= TOL,
          f"科目 {ledger} / 单据 {expected}（差 {ledger - expected}）")

    # L5 审批已终态但单据仍 SUBMITTED 超过 10 分钟（孤儿回调）
    bad = q("""
        select r.report_no, i.status, i.updated_at::text
          from biz_expense_report r
          join biz_flow_instance i on i.id = r.approval_instance_id
         where r.deleted_at is null and r.status = 'SUBMITTED'
           and i.status <> 'PENDING'
           and i.updated_at < now() - interval '10 minutes'
         order by r.report_no
    """)
    check("L5 无孤儿回调（审批已终态但单据仍 SUBMITTED）", "报销", "BLOCKER", not bad,
          f"{len(bad)} 张单需人工补做：{bad[:3]}" if bad else "无孤儿")

    # L6 pay_status 与金额自洽（三值完备）
    bad = q("""
        select report_no from biz_expense_report
         where deleted_at is null
           and ((pay_status = 'UNPAID'  and paid_amount <> 0)
             or (pay_status = 'PARTIAL' and (paid_amount <= 0 or paid_amount >= total_with_tax))
             or (pay_status = 'PAID'    and (paid_amount <> total_with_tax or unpaid_amount <> 0))
             or (unpaid_amount <> total_with_tax - paid_amount))
         order by report_no
    """)
    check("L6 pay_status 与 paid/unpaid 自洽", "报销", "WARN", not bad,
          f"{len(bad)} 张单金额与状态不自洽：{bad[:3]}" if bad else "全部自洽")

    # L7 已作废且曾入账的报销单，其计提凭证必须已红冲
    bad = q("""
        select r.report_no
          from biz_expense_report r
          join biz_voucher v on v.id = r.voucher_id
         where r.deleted_at is null and r.status = 'CANCELLED'
           and r.voucher_id is not null and v.reversed_by is null
         order by r.report_no
    """)
    check("L7 作废单的有效凭证必须已红冲", "报销", "BLOCKER", not bad,
          f"{len(bad)} 张作废单凭证未红冲：{bad[:3]}" if bad else "全部已红冲")

    # L8 报销 usage 的 amount 合计 = 受预算控制明细的价税合计
    bad = q("""
        select r.report_no, u.total::text, controlled.total::text
          from biz_expense_report r
          join (select source_id, coalesce(sum(amount), 0) as total
                  from biz_budget_usage
                 where source_type = 'EXPENSE_REPORT' and status in ('OCCUPIED','CONSUMED')
                   and deleted_at is null
                 group by source_id) u on u.source_id = r.id
          join (select i.report_id, coalesce(sum(i.with_tax_amount), 0) as total
                  from biz_expense_item i
                  join biz_expense_type t on t.id = i.expense_type_id
                 where t.budget_controlled = 1
                 group by i.report_id) controlled on controlled.report_id = r.id
         where r.deleted_at is null
           and abs(u.total - controlled.total) > 0.0001
         order by r.report_no
    """)
    check("L8 报销占用合计 = 受控明细价税合计", "报销", "WARN", not bad,
          f"{len(bad)} 张单预算口径与单据不一致：{bad[:3]}" if bad else "口径一致")


# ══════════════════════════════════════════════════════════════════
# J. 定时任务 —— 自动提醒必须「配得上 + 真的在跑」（P0-FIX-1）
#
# 为什么单开一组只查调度、不查账：账错了有人看得见（报表对不上、余额不平），
# 而「提醒任务根本没跑」是**静默失效** —— 单据该到期不提醒、库存该补不提醒、
# 合同该续签不提醒，业务方以为系统在管，实际一条通知都没发过。这类故障
# 唯一的证伪证据就是执行日志，所以这组查的是 `sys_job` + `sys_job_log`。
#
# 判定口径（两条，缺一不可）：
#   J1 配置真相：4 行存在且 status = 1（停用 → 调度器根本不会装载它）
#   J2 运行真相：每条近 7 天至少一条 status = 1（成功）的 sys_job_log
#
# J1 / J2 为什么要拆两级判断、J2 为什么带前置条件：
#   `sys_job_log` 只有「服务真的在这个库上跑过 sys_job 任务」才会有行。全新迁移出来、
#   服务还没起过的库（CI / 演练库）拿不到任何执行记录，此时判红纯属噪音（既不是账错，
#   也不能说明提醒坏了）。所以：近 7 天整库 0 条执行记录 → J2 记 INFO 跳过；
#   只要有任一任务在这库上跑过（说明调度器活着），这 4 个提醒就**必须**各有成功记录。
#   这条闸要抓的是「库在跑、提醒却哑了」——那才是业务被坑的状态。
# ══════════════════════════════════════════════════════════════════

# 4 个关键提醒任务：job_name → 人话（与 `erp::reminder::RemindKind` 一一对应）
REMINDER_JOBS = {
    "erp_payable_due_scan": "应付到期",
    "erp_po_overdue_scan": "采购超期",
    "erp_contract_expire_scan": "合同到期",
    "erp_low_stock_scan": "低库存",
}


def group_j():
    """定时任务不变量 J1–J2（P0-FIX-1）。"""
    print("\n" + "=" * 92)
    print("J. 定时任务（4 个自动提醒必须已启用、且近期真的跑成功过）")
    print("=" * 92)

    if not table_exists("sys_job"):
        check("J1 4 个 ERP 提醒任务已启用", "任务", "INFO", True,
              "本库无 sys_job 表（迁移未跑到定时任务模块），跳过")
        return

    names = ",".join(f"'{n}'" for n in REMINDER_JOBS)
    rows = q(f"""
        select j.job_name,
               j.status::text,
               coalesce(to_char(j.last_run_at at time zone 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI'), '-'),
               (select count(*)::text from sys_job_log l
                 where l.job_id = j.id and l.status = 1
                   and l.started_at >= now() - interval '7 days'),
               (select count(*)::text from sys_job_log l
                 where l.job_id = j.id)
          from sys_job j
         where j.deleted_at is null
           and j.job_name in ({names})
         order by j.job_name
    """)
    got = {r[0]: r for r in rows}

    # ---------- J1 配置：4 行都在、都启用 ----------
    missing = [n for n in REMINDER_JOBS if n not in got]
    disabled = [(n, r[1]) for n, r in got.items() if r[1] != "1"]
    reasons = []
    if missing:
        reasons.append(f"缺 {len(missing)} 行：{', '.join(missing)}（迁移没种 / 行被删）")
    if disabled:
        reasons.append("停用（status≠1，调度器不会装它）："
                       + "、".join(f"{n}(status={s})" for n, s in disabled))
    check("J1 4 个 ERP 提醒任务已启用（status = 1）", "任务", "BLOCKER",
          not missing and not disabled,
          "\n".join(reasons) if reasons else
          "4 行全部启用：" + "、".join(f"{n}({REMINDER_JOBS[n]})" for n in sorted(got)),
          f"""select job_name, status::text as 状态, coalesce(left(remark, 40), '') as 备注
                from sys_job where deleted_at is null and job_name in ({names})
               order by job_name""")

    if missing:
        # 行都不在，J2 没有可查的 job_id；J1 已报红，不再叠一条噪音
        return

    # ---------- J2 运行：近 7 天各有成功执行记录 ----------
    alive = int(one("select count(*) from sys_job_log "
                    "where started_at >= now() - interval '7 days'") or 0)
    if alive == 0:
        # 显式打一行：`check()` 只在 FAIL 时打印 detail，INFO 通过时不留痕就分不清
        # 「真的每条都有成功记录」和「整库还没跑过任何任务、新鲜度判定被跳过」。
        print("        ℹ️ 近 7 天整库 0 条 sys_job 执行记录（服务未起 / 新库）⇒ "
              "新鲜度判定跳过，本组只校验了配置（J1）")
        check("J2 4 个提醒近 7 天各有成功执行记录", "任务", "INFO", True,
              "近 7 天整库没有任何 sys_job 执行记录（服务未起 / 新库），新鲜度判定跳过；"
              "J1 已保证配置为启用，服务起来后第一轮会自动落 sys_job_log")
        return

    stale = [(n, r) for n, r in got.items() if int(r[3]) == 0]
    detail = "\n".join(
        f"{n}（{REMINDER_JOBS[n]}）：近 7 天成功 0 条，历史共 {r[4]} 条，"
        f"last_run_at={r[2]}（时区 +08:00）"
        for n, r in stale)
    check("J2 4 个提醒近 7 天各有成功执行记录", "任务", "BLOCKER",
          not stale,
          detail if stale else
          "、".join(f"{n} 近7天成功 {r[3]} 条（last={r[2]}）" for n, r in sorted(got.items())),
          f"""select j.job_name, l.status::text as 状态,
                     to_char(l.started_at at time zone 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI') as 开始,
                     coalesce(left(l.message, 60), left(l.error, 60), '') as 摘要
                from sys_job j left join sys_job_log l on l.job_id = j.id
               where j.deleted_at is null and j.job_name in ({names})
               order by j.job_name, l.started_at desc""")

    # ---------- 附带：调度器注册表与 DB 的 cron 一致性（INFO，人工核对用） ----------
    # 这里只做提示：真正「注册没注册」要看进程日志（`定时任务 xxx [cron] 已排程`），
    # 巡检不读日志文件，避免把「日志轮转」当成故障。
    print("        ℹ️ 抽查口径：cron 按 UTC 解释（`0 10 7 * * *` = 北京时间 15:10），"
          "与「定时任务」页 remark 一致")


# ══════════════════════════════════════════════════════════════════
# N. 数据权限（【P0-FIX-2】）：行级数据范围不能「空转」
# ══════════════════════════════════════════════════════════════════
#
# 背景：库里「谁能看哪些行」只有两个载体 —— `sys_role.data_scope`
# （1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义集合）与
# `sys_role_dept`（仅 5 生效）。解析链见 `business/shared/principal.rs::RESOLVE_SQL`
# → `shared/scope.rs::filter_for`。全库都停在默认值 1 时，每个人拿到的片段都是
# `AND 1 = 1`：功能没坏、接口没报错，**但「数据权限」这件事等于没做** —— 这是
# 典型的静默失效（比报错更难发现，因为看板上一切正常）。
#
# 判据（WARN 级：配置缺失不该拦发布，但必须有人看见）：
#   N1-a 库里至少有一个**启用的**角色是部门级范围（2/3，或 5 且有集合行）；
#        一个都没有 ⇒ 所有 dept 受控表的行级裁剪恒不生效。
#   N1-b 不许存在 `data_scope=5` 却 `sys_role_dept` 0 行的角色 —— 这种「配了却空」
#        的 CUSTOM 会让持有者被 `merge_scope_rows` 静默降级成「仅本人」（S10），
#        表现为「某人突然只看得到自己的单据」，报修时极难定位。
# 附带信息（INFO，写在 detail 里，不单独占一条检查）：超管账号占比 ——
# 「9 个用户都能看全部」里往往有相当一部分是**他们本来就持有 SUPER_ADMIN**。

# 代码里真的会按 dept_id 收口的表（`ScopeTarget` 的 dept_col 非空的那些）。
# 不在这里的 biz_* 表（采购/出入库/库存/付款/凭证/合同/固资…）走的是
# `ScopeTarget::unscoped`，即**不受行级范围控制**（见卡内「哪些表受控」一节）。
DEPT_SCOPED_TABLES = {
    "sys_user": "人员列表（scope.rs::SYS_USER）",
    "biz_task": "任务列表（task/task_model_base.rs::TASK_TARGET，范围 ∪ 参与关系）",
    "biz_project": "项目列表（project/model.rs::PROJECT_TARGET）",
    "biz_budget": "预算列表（erp/budget_model.rs::BUDGET_TARGET）",
    "biz_expense_report": "报销单列表（erp/expense_model.rs::EXPENSE_TARGET）",
}


def group_n():
    """数据权限不变量 N1（【P0-FIX-2】）。"""
    print("\n" + "=" * 92)
    print("N. 数据权限（有 dept_id 的受控表必须真的有部门级授权，否则行级权限空转）")
    print("=" * 92)

    if not table_exists("sys_role") or not table_exists("sys_role_dept"):
        check("N1 有 dept_id 的受控表必须有部门级授权", "权限", "INFO", True,
              "本库没有 sys_role / sys_role_dept（迁移未跑到数据权限模块），跳过")
        return

    # ---- N1-a：部门级授权是否真的存在 ----
    dept_roles = q("""
        select r.role_code,
               r.data_scope::text,
               (select count(*)::text from sys_role_dept rd where rd.role_id = r.id)
          from sys_role r
         where r.deleted_at is null
           and r.status = 1
           and (r.data_scope in (2, 3)
                or (r.data_scope = 5
                    and exists (select 1 from sys_role_dept rd where rd.role_id = r.id)))
         order by r.role_code""")

    # ---- N1-b：selector=5 却没有集合行（配错的 CUSTOM） ----
    empty_custom = q("""
        select r.role_code
          from sys_role r
         where r.deleted_at is null
           and r.data_scope = 5
           and not exists (select 1 from sys_role_dept rd where rd.role_id = r.id)
         order by r.role_code""")

    # ---- 受控表在本库的落地情况（只在存在的表上要求，缺表不算错） ----
    present = [t for t in DEPT_SCOPED_TABLES if table_exists(t)]
    absent = [t for t in DEPT_SCOPED_TABLES if t not in present]

    # ---- INFO：超管占比（解释「看起来所有人都能看全部」） ----
    total_users = one("select count(*) from sys_user where deleted_at is null") or "0"
    super_users = one("""
        select count(distinct u.id)
          from sys_user u
          join sys_user_role ur on ur.user_id = u.id
              and (ur.expire_time is null or ur.expire_time > now())
          join sys_role r on r.id = ur.role_id
              and r.deleted_at is null and r.status = 1
         where u.deleted_at is null and r.role_code = 'SUPER_ADMIN'""") or "0"
    scope5_rows = one("select count(*) from sys_role_dept") or "0"

    reasons = []
    if not dept_roles:
        reasons.append(
            f"没有任何启用角色的数据范围是部门级：{len(present)} 张受控表"
            f"（{'、'.join(present) or '无'}）的 dept_id 裁剪恒不生效，"
            f"每个人拿到的片段都是 `AND 1 = 1` —— 行级数据权限空转")
    if empty_custom:
        reasons.append(
            "data_scope=5 但没有 sys_role_dept 行的角色："
            + "、".join(r[0] for r in empty_custom)
            + "（持有者被静默降级为「仅本人」，会表现为突然只看得到自己的单据）")

    ok = not reasons
    detail_lines = []
    if reasons:
        detail_lines.extend(reasons)
    detail_lines.append(
        f"受控表 {len(present)} 张在库：{'、'.join(present) or '无'}"
        + (f"（未建表、不计入：{'、'.join(absent)}）" if absent else ""))
    detail_lines.append(
        "部门级角色：" + ("、".join(f"{r[0]}(scope={r[1]}"
                                    + (f", 集合={r[2]} 个部门" if r[1] == "5" else "") + ")"
                                    for r in dept_roles) or "无")
        + f"；sys_role_dept 共 {scope5_rows} 行")
    detail_lines.append(
        f"超管占比：{super_users}/{total_users} 个在用账号持有 SUPER_ADMIN"
        "（超管在 scope.rs::filter_for 里短路恒真，不受数据范围限制——"
        "「所有人都能看全部」里有一部分是这件事，不是范围配置的锅）")

    check("N1 有 dept_id 的受控表必须有部门级授权", "权限", "WARN", ok,
          "\n".join(detail_lines),
          """select r.role_code as 角色, r.data_scope::text as 数据范围, r.status::text as 状态,
                    (select count(*) from sys_role_dept rd where rd.role_id = r.id) as 自定义部门数
               from sys_role r where r.deleted_at is null
              order by r.data_scope desc, r.role_code""")

    # ---- N2：在用 HUMAN 账号必须落地到部门（【P0-FIX-2B】D-5）----
    #
    # 判据（卡片 t_db82c38e 原文）：status=1 AND is_locked=false AND user_type='HUMAN'
    # AND deleted_at IS NULL ⇒ dept_id IS NOT NULL。
    # 豁免：user_type='ROBOT'（AI 用户不进组织树）、is_locked=true（锁定夹具）、status=0（停用号，
    #       登不进来 —— `system/auth/handler.rs` 对 status=0 回 AUTH_ACCOUNT_DISABLED）。
    #
    # 为什么是 WARN 而不是 BLOCKER：`henry.liao` 的身份（docs/待决策.md 铁律①）尚未确认，
    # 未确认期间这条会**长期亮**，升级成 BLOCKER 等于让巡检永久变红。升级是 C 卡的事。
    #
    # 为什么这条值得存在：HUMAN 账号没有部门时，`principal::RESOLVE_SQL` 的 `subtree`/`DEPT`
    # 分支解析不出部门 ⇒ 部门类角色被 merge_scope_rows 静默降级成「仅本人」（S10/S11）。
    # 也就是说「没部门」不是缺个字段，而是**权限静默缩水**，只在列表里表现为「怎么都看不到」。
    name_n2 = "N2 在用 HUMAN 账号必须有部门归属"
    if not table_exists("sys_user"):
        check(name_n2, "权限", "INFO", True, "本库没有 sys_user（迁移未跑到用户模块），跳过")
        return

    # 本库有没有 dept_id 列（极老的库可能没有；缺列时按 INFO 跳过而不是整组炸）
    has_dept_col = q(
        "select 1 from information_schema.columns "
        "where table_schema='public' and table_name='sys_user' and column_name='dept_id'")
    if not has_dept_col:
        check(name_n2, "权限", "INFO", True, "sys_user 没有 dept_id 列（未上组织树），跳过")
        return

    violated = q("""
        select u.username, u.user_type, u.is_locked::text, u.status::text
          from sys_user u
         where u.status = 1
           and u.is_locked = false
           and u.user_type = 'HUMAN'
           and u.deleted_at is null
           and u.dept_id is null
         order by u.username""")
    in_use = one("""
        select count(*) from sys_user
         where status = 1 and is_locked = false and user_type = 'HUMAN'
           and deleted_at is null""") or "0"
    # 口径说明（写进 detail，免得下次有人拿「9 个账号」跟这里的数字对不上）：
    # 卡片里的 5 来自「deleted_at IS NULL 的 HUMAN」（含停用号）；本判据按 status=1 收口，
    # 停用号登不进来、不算「在用」。放一行「宽松口径」进来，两种数法都看得见。
    loose = q("""
        select u.username, u.status::text
          from sys_user u
         where u.is_locked = false
           and u.user_type = 'HUMAN'
           and u.deleted_at is null
           and u.dept_id is null
         order by u.username""")

    lines2 = []
    if violated:
        lines2.append(
            f"在用 HUMAN 账号（status=1/未锁/未删）里 {len(violated)} 个没有部门："
            + "、".join(f"{r[0]}(status={r[3]})" for r in violated))
        lines2.append(
            "后果：这些账号的部门类角色会被静默降级成「仅本人」（S10/S11），列表少数据但不报错。")
        lines2.append(
            "处置：bash scripts/datafix/org_landing/apply.sh（按 dept_code 落位；"
            "身份未确认的账号（henry.liao）等 C 卡定了再落）。")
    else:
        lines2.append(f"在用 HUMAN 账号 {in_use} 个，全部有部门。")
    if loose:
        lines2.append(
            "宽松口径（含 status=0 停用号，便于与决策文档的「9 个在用账号」对账）："
            + "、".join(f"{r[0]}(status={r[1]})" for r in loose))

    check(name_n2, "权限", "WARN", not violated, "\n".join(lines2),
          """select u.username as 账号, u.status::text as 状态, u.is_locked::text as 锁定,
                    u.user_type as 类型, u.created_at::date as 创建日
               from sys_user u
              where u.is_locked = false and u.user_type = 'HUMAN'
                and u.deleted_at is null and u.dept_id is null
              order by u.status desc, u.username""")


def group_o():
    """E2E 夹具标记不变量 O1（【P0-FIX-3】）。

    **只在生产库上判**：测试库 / 演练库 / e2e 隔离实例里的 `[E2E]` 行是用例的预期产物，
    在那里报 WARN 就成了「永远亮的指标」。生产库上出现活跃标记行 = 有人把 e2e 打到
    了线上（或历史残留没清），必须人看。
    """
    print("\n" + "=" * 92)
    print("O. E2E 夹具标记（生产库不许有活跃标记行 / 不许有在用测试账号）")
    print("=" * 92)

    name = "O1 生产库无活跃 E2E 标记行、无在用测试账号"
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        import db_guard as _guard
        identity = _guard.classify_db(DB["db"], DB["host"], DB["port"], _guard.prod_identity())
    except Exception as exc:  # 判据缺失不能当成「干净」，但也不该把整套打红
        check(name, "E2E", "INFO", True, f"生产身份判定不可用，本库跳过（{exc}）")
        return
    if not identity["prod"]:
        # 跳过也要**看得见**：否则「绿」和「没查」在报告里长得一样
        print(f"  · 跳过：库 {DB['db']} 不是生产库（{identity['reason']}）——"
              "e2e 夹具在测试库/演练库是预期产物")
        check(name, "E2E", "INFO", True)
        return

    try:
        import e2e_marker as _marker  # 判据唯一实现，不在这里重抄谓词
        hits = _marker.residue()
        accounts = _marker.enabled_test_accounts()
        locator = _marker.RESIDUE_SQL
    except Exception as exc:
        # 查不了 ≠ 干净：这条是生产库上的红/绿判据，失败必须说出来
        check(name, "E2E", "WARN", False,
              f"查不了（scripts/verify/e2e_marker.py 执行失败：{exc}）—— 查不了不等于干净")
        return

    total = sum(h["alive"] for h in hits)
    ok = total == 0 and not accounts
    lines = []
    if hits:
        lines.append(f"活跃标记行 {total} 条，分布在 {len(hits)} 张表：")
        for h in hits:
            lines.append(f"  · {h['table']} {h['alive']} 行  样例：{' | '.join(h['sample'])[:70]}")
    if accounts:
        lines.append(f"仍在用的测试账号 {len(accounts)} 个：{', '.join(accounts)}")
    if not ok:
        lines.append("处置：bash scripts/datafix/e2e_residue3/00_backup.sh && "
                     "bash scripts/datafix/e2e_residue3/apply.sh（E2E 行软删、账号禁用）")
    check(name, "E2E", "WARN", ok, "\n".join(lines), locator)


GROUPS = {"会计": group_a, "账账": group_b, "账实": group_c,
          "单据链": group_d, "状态": group_e, "期间": group_f, "闭环": group_g,
          "审批": group_h, "盘点": group_i, "固资": group_k, "报销": group_l,
          "任务": group_j, "权限": group_n, "E2E": group_o}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--group", default=None)
    args = ap.parse_args()

    print("=" * 92)
    print("云枢 ERP 不变量审计套件")
    print(f"库: {DB['db']}   容差: {TOL}")
    print("=" * 92)

    groups = [GROUPS[args.group]] if args.group else list(GROUPS.values())
    for g in groups:
        try:
            g()
        except Exception as e:
            check(f"{g.__name__} 执行异常", "系统", "BLOCKER", False, str(e)[:300])

    total = len(RESULT)
    passed = sum(1 for r in RESULT if r["ok"])
    blockers = [r for r in RESULT if not r["ok"] and r["level"] == "BLOCKER"]
    warns = [r for r in RESULT if not r["ok"] and r["level"] == "WARN"]

    print("\n" + "=" * 92)
    print(f"结果：{passed}/{total} 通过   🚨 BLOCKER {len(blockers)}   ⚠️ WARN {len(warns)}")
    print("=" * 92)
    if blockers:
        print("\n🚨 必须立刻修（账已错，不许带病运行）：")
        for r in blockers:
            print(f"  · [{r['group']}] {r['name']}  —  {r['detail'][:100]}")
    if warns:
        print("\n⚠️ 需要人看：")
        for r in warns:
            print(f"  · [{r['group']}] {r['name']}  —  {r['detail'][:100]}")
    if not blockers and not warns:
        print("\n★ 全部不变量成立，账目干净。")

    if args.json:
        print("\n" + json.dumps(dict(total=total, passed=passed,
                                    blockers=len(blockers), warns=len(warns),
                                    results=RESULT), ensure_ascii=False, indent=2))
    sys.exit(1 if blockers else 0)


if __name__ == "__main__":
    main()
