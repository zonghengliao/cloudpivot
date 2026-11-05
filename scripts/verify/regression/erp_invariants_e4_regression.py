#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
erp_invariants.py **E4 / E4b 回归**（t_086dab13）
================================================
E4「作废单的库存已回滚」原来手写白名单只认 TRANSFER_IN/OUT + SERIAL_IN/OUT，
漏掉 OPENING（库存期初）/ STOCK_TAKE_GAIN（盘盈）/ STOCK_TAKE_CANCEL（盘亏作废回补）
⇒ 任何用期初建账或做过盘盈的库必假红。本脚本守三件事：

  ① 静态漂移（不需要数据库）：代码里出现的每一个 biz_type 字面量都必须登记进
     `scripts/verify/erp_invariants.py::E4_BIZ_TYPE_AXIS`。代码新增 biz_type、
     登记表没跟上 → 本脚本直接红（这就是「别再逐个漏」的闸）。
  ② 正样本：在隔离库造「收货 50（单据轴）＋库存期初 100＋盘盈 5＋盘亏 8 并作废回补 8」
     的完整流水链，断言 E4 仍**绿**（修前这条必红，见 docs/evidence 里的 before 输出）。
  ③ 负样本（判据强度不许放宽）：
       · 删掉那条 STOCK_TAKE_CANCEL 回补流水 ⇒ E4 **必须红**，且定位到夹具物料、多出恰好 8；
       · 插一条未登记的 biz_type 流水        ⇒ E4b **必须红**（否则 E4 会静默漏算）。
  ④ 遗留值：老库里的 biz_type='PURCHASE'/doc_type='PURCHASE_ORDER' 不许让 E4 误伤，
     但 E4c 必须 WARN 亮出来（不静默放过历史数据）。

用法（**只许打隔离库**，脚本自己会拒生产库名）：
    NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
    NIK_DATABASE_NAME=cloudpivot_e4reg_t086 \
    python3 scripts/verify/regression/erp_invariants_e4_regression.py

退出码：0 = 全部断言通过；1 = 有断言失败（断言的失败原因逐条打印）。
夹具全部带 `E4REG-` 前缀，跑完/跑前都会清理，可反复跑。
"""
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
AUDIT = REPO / "scripts" / "verify" / "erp_invariants.py"
REGISTRY_OWNER = "scripts/verify/erp_invariants.py::E4_BIZ_TYPE_AXIS"

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    db=os.environ.get("NIK_DATABASE_NAME", ""),
    password=os.environ.get("NIK_DATABASE_PASSWORD", ""),
)
PREFIX = "E4REG-"

FAILS = []
CHECKS = []


def free_of(cond, name, detail=""):
    CHECKS.append((bool(cond), name, detail))
    print(f"  {'✅' if cond else '🚨'} {name}")
    if not cond:
        if detail:
            for line in str(detail).split("\n"):
                print(f"        {line}")
        FAILS.append(name)


# ── psql 薄封装（与 erp_invariants.py 同一连接口径）─────────────────────────
def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB["password"]
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", db or DB["db"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True, env=env, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:600])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line != ""]


def one(sql):
    rows = psql(sql)
    return rows[0][0] if rows else None


# ── 闸①：静态漂移（代码 biz_type 字面量 ⊆ 登记表）──────────────────────────
# 动账 biz_type 的取值域只有两处来源（写入唯一入口是 inventory_writer::insert_txn）：
#   ① doc_kind.rs `impl TxnBizType` 的 as_str() 落库字面量
#      + 同文件 `TXN_BIZ_TYPE_VALUES`（域清单，两者逐字同序，由
#        `cargo test --lib business::shared::doc_kind::domain_consts_match_enums` 盯着）
#   ② 读写库存流水的 ERP 模块里的 `pub const BIZ_TYPE_*: &str = "LITERAL"`
#      （盘点三态曾只以常量形式存在于 stocktake_model.rs，已补进 ① 的枚举词表，见 t_6a667db8）
#
# 数量判据**不写死**：期望集合从 `TXN_BIZ_TYPE_VALUES` 解析（唯一真相），
# 枚举解析结果必须与之逐字一致 —— 否则一旦词表扩容（如 19 → 22）这条闸会拿旧数字误判。
E4_TXN_WRITER_MODULES = (
    "stockin_base.rs", "stockin_write.rs", "stockin_cancel.rs", "opening.rs",
    "transfer_settle.rs", "serial_model.rs", "stockout_base.rs", "stockout_model.rs",
    "stocktake_model.rs",
)


def static_registry_gate():
    print("\n[闸①] 静态漂移：代码里出现过的 biz_type 必须全部登记进 E4 判据")
    src = AUDIT.read_text(encoding="utf-8")
    m = re.search(r"^E4_BIZ_TYPE_AXIS = \{(.*?)^\}", src, re.M | re.S)
    if not m:
        free_of(False, "登记表 E4_BIZ_TYPE_AXIS 可解析", "在 erp_invariants.py 里找不到该字典")
        return
    registered = set(re.findall(r'"([A-Z0-9_]+)"\s*:', m.group(1)))
    free_of(len(registered) >= 22, f"登记表已解析（{len(registered)} 种）", str(sorted(registered)))

    # 来源 ①：doc_kind.rs 里 `impl TxnBizType` 块内的 as_str() 字面量
    doc_kind = (REPO / "backend/src/business/shared/doc_kind.rs").read_text(encoding="utf-8")
    impl = re.search(r"^impl TxnBizType \{(.*?)^\}", doc_kind, re.M | re.S)
    free_of(bool(impl), "doc_kind.rs 的 impl TxnBizType 块可解析")
    body = re.search(r"pub const fn as_str\(self\) -> &'static str \{(.*?)\n    \}\n",
                     impl.group(1), re.S) if impl else None
    free_of(bool(body), "TxnBizType::as_str() 可解析")
    enum_vals = set(re.findall(r'=>\s*"([A-Z0-9_]+)"', body.group(1))) if body else set()
    # 期望集合 = `TXN_BIZ_TYPE_VALUES`（doc_kind.rs 的域清单，唯一真相）。不写死数字：
    # 词表扩容（19→22 补盘点三态）时，这条闸只看「枚举 ↔ 域清单是否一致」，不拿旧数字误判。
    domain = re.search(r"const TXN_BIZ_TYPE_VALUES: \[&str; (\d+)\] = \[(.*?)\n\];", doc_kind, re.S)
    free_of(bool(domain), "doc_kind.rs 的 TXN_BIZ_TYPE_VALUES 可解析")
    domain_vals = set(re.findall(r'"([A-Z0-9_]+)"', domain.group(2))) if domain else set()
    free_of(bool(domain_vals),
            f"TXN_BIZ_TYPE_VALUES 解析到 {len(domain_vals)} 个 biz_type",
            str(sorted(domain_vals)))
    free_of(enum_vals == domain_vals,
            f"TxnBizType::as_str() 与 TXN_BIZ_TYPE_VALUES 一致（枚举 {len(enum_vals)} / 域 {len(domain_vals)}）",
            f"枚举独有 {sorted(enum_vals - domain_vals)} / 域独有 {sorted(domain_vals - enum_vals)}")

    # 来源 ②：动账写入模块里的 BIZ_TYPE_* 常量字面量
    const_vals = set()
    for name in E4_TXN_WRITER_MODULES:
        rs = REPO / "backend/src/business/erp" / name
        const_vals |= set(re.findall(r'pub const BIZ_TYPE[A-Z_]*:\s*&str\s*=\s*"([A-Z][A-Z0-9_]*)"',
                                     rs.read_text(encoding="utf-8")))

    code_vals = enum_vals | const_vals
    missing = sorted(code_vals - registered)
    free_of(bool(code_vals), f"代码侧共解析到 {len(code_vals)} 个 biz_type"
                             f"（枚举 {len(enum_vals)} + 模块常量 {len(const_vals)}）")
    free_of(not missing, "代码里的 biz_type 全部已登记",
            ("未登记：" + ", ".join(missing) + f" —— 请登记到 {REGISTRY_OWNER}")
            if missing else "")
    only_const = sorted(const_vals - enum_vals)
    print(f"        ℹ️ 只出现在模块常量、不在 TxnBizType 枚举里的：{only_const or '无'}"
          "（E4 曾因漏登记 OPENING/STOCK_TAKE_* 假红）")


# ── 审计脚本调用 ────────────────────────────────────────────────────────────
def run_audit():
    """跑 `--group 状态`（E1..E4b），返回 (退出码, 合并输出)。"""
    env = dict(os.environ)
    env.update(NIK_DATABASE_HOST=DB["host"], NIK_DATABASE_PORT=str(DB["port"]),
               NIK_DATABASE_USER=DB["user"], NIK_DATABASE_NAME=DB["db"],
               NIK_DATABASE_PASSWORD=DB["password"])
    r = subprocess.run([sys.executable, str(AUDIT), "--group", "状态"],
                       capture_output=True, text=True, env=env, cwd=str(REPO), timeout=300)
    return r.returncode, r.stdout + r.stderr


def e4_line(out):
    for line in out.split("\n"):
        if "E4 " in line and ("✅" in line or "🚨" in line or "⚠️" in line):
            return line.strip()
    return "(未找到 E4 结果行)"


def e4b_line(out):
    for line in out.split("\n"):
        if "E4b " in line and ("✅" in line or "🚨" in line or "⚠️" in line):
            return line.strip()
    return "(未找到 E4b 结果行)"


def e4c_line(out):
    for line in out.split("\n"):
        if "E4c " in line and ("✅" in line or "🚨" in line or "⚠️" in line):
            return line.strip()
    return "(未找到 E4c 结果行)"


# ── 夹具 ────────────────────────────────────────────────────────────────────
def cleanup():
    mat = f"select id from biz_material where material_code like '{PREFIX}%'"
    psql(f"delete from biz_inventory where material_id in ({mat})")
    psql(f"delete from biz_inventory_txn where doc_no like '{PREFIX}%' or material_id in ({mat})")
    psql("delete from biz_stock_in_item where stock_in_id in "
         f"(select id from biz_stock_in where in_no like '{PREFIX}%')")
    psql(f"delete from biz_stock_in where in_no like '{PREFIX}%'")
    psql(f"delete from biz_material where material_code like '{PREFIX}%'")
    psql(f"delete from biz_warehouse where warehouse_code like '{PREFIX}%'")


def add_txn(mt, wh, biz, dtype, ttype, qty, doc_id, doc_no, remark):
    """写一条流水（数值写实：unit_cost=10，amount=qty*10）。"""
    psql(f"""insert into biz_inventory_txn
                 (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id,
                  doc_no, line_no, qty, unit_cost, amount, period_code, remark)
             values (gen_random_uuid(), '{mt}', '{wh}', '{ttype}', '{biz}', '{dtype}',
                     '{doc_id}', '{doc_no}', 1, {qty}, 10, {float(qty) * 10},
                     to_char(now(), 'YYYY-MM'), '{remark}')""")


def seed():
    """造一条真实形态的库存链（数值刻意好认）：

        收货过账       PURCHASE_IN        IN  50   ← 单据轴（biz_stock_in POSTED）
        库存期初       OPENING            IN 100   ← 修前漏（假红来源 1）
        盘盈过账       STOCK_TAKE_GAIN    IN   5   ← 修前漏（假红来源 2）
        盘亏过账       STOCK_TAKE_LOSS    OUT  8
        盘亏单作废回补 STOCK_TAKE_CANCEL  IN   8   ← 修前漏（假红来源 3）
        ⇒ 库存 = 50 + 100 + 5 - 8 + 8 = 155
    """
    unit = one("select id from biz_unit order by unit_code limit 1")
    if not unit:
        raise RuntimeError("库里没有 biz_unit 行，无法造夹具（请用有基础资料的隔离库）")
    psql(f"""insert into biz_warehouse
                 (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf)
             values (gen_random_uuid(), '{PREFIX}W1', '{PREFIX}E4 回归仓',
                     '/' || gen_random_uuid() || '/', 1, 1)""")
    wh = one(f"select id from biz_warehouse where warehouse_code = '{PREFIX}W1'")
    psql(f"""insert into biz_material (id, material_code, material_name, unit_id)
             values (gen_random_uuid(), '{PREFIX}M1', '{PREFIX}E4 回归物料', '{unit}')""")
    mt = one(f"select id from biz_material where material_code = '{PREFIX}M1'")

    sup = one("select id from biz_supplier where deleted_at is null order by id limit 1")
    in_type, sup_sql = ("PURCHASE", f"'{sup}'") if sup else ("OTHER", "null")
    psql(f"""insert into biz_stock_in
                 (id, in_no, in_type, warehouse_id, supplier_id, in_date, status,
                  total_qty, total_amount, total_tax, total_with_tax, is_sourceless)
             values (gen_random_uuid(), '{PREFIX}IN-1', '{in_type}', '{wh}', {sup_sql},
                     current_date, 'POSTED', 50, 500, 0, 500, true)""")
    sin = one(f"select id from biz_stock_in where in_no = '{PREFIX}IN-1'")
    psql(f"""insert into biz_stock_in_item
                 (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl,
                  amount, tax_amount, with_tax_amount)
             values (gen_random_uuid(), '{sin}', 1, '{mt}', '{unit}', 50, 10, 10,
                     500, 0, 500)""")

    def txn(biz, dtype, ttype, qty, doc_id, doc_no, remark):
        add_txn(mt, wh, biz, dtype, ttype, qty, doc_id, doc_no, remark)

    txn("PURCHASE_IN", "STOCK_IN", "IN", 50, sin, f"{PREFIX}IN-1", "收货过账（单据轴）")
    txn("OPENING", "OPENING", "IN", 100, sin, f"{PREFIX}OPEN-1", "库存期初")
    txn("STOCK_TAKE_GAIN", "STOCK_TAKE", "IN", 5, sin, f"{PREFIX}ST-GAIN", "盘盈过账")
    txn("STOCK_TAKE_LOSS", "STOCK_TAKE", "OUT", 8, sin, f"{PREFIX}ST-LOSS", "盘亏过账")
    txn("STOCK_TAKE_CANCEL", "STOCK_TAKE", "IN", 8, sin, f"{PREFIX}ST-CANCEL", "盘亏单作废回补")

    psql(f"""insert into biz_inventory
                 (id, material_id, warehouse_id, qty, avg_cost, amount)
             values (gen_random_uuid(), '{mt}', '{wh}', 155, 10, 1550)""")
    return wh, mt


# ── 主流程 ──────────────────────────────────────────────────────────────────
def main():
    import argparse
    ap = argparse.ArgumentParser(description="E4/E4b 回归（t_086dab13）")
    ap.add_argument("--static-only", action="store_true",
                    help="只跑闸①（代码 biz_type ⊆ 登记表），不连库/不改数据（可进 pre-commit）")
    args = ap.parse_args()

    db = DB["db"]
    print("=" * 92)
    print("erp_invariants E4/E4b 回归（t_086dab13）")
    print(f"库: {db or '(未指定)'} @ {DB['host']}:{DB['port']}   审计脚本: {AUDIT}")
    print("=" * 92)

    if args.static_only:
        static_registry_gate()
        bad = [name for ok, name, _ in CHECKS if not ok]
        print(f"\n闸①：{len(CHECKS) - len(bad)}/{len(CHECKS)} 断言通过"
              + (f"   🚨 失败 {len(bad)} 条" if bad else "   ★ 全绿"))
        return 1 if bad else 0

    if (not db or db in ("cloudpivot", "cloudpivot_prod", "postgres")
            or "prod" in db.lower() or not db.startswith("cloudpivot_")):
        print(f"\n🚨 拒绝执行：NIK_DATABASE_NAME={db!r} 不是隔离库。"
              "\n   本脚本会改写库存轴数据，只许指向隔离/演练库（cloudpivot_* 且不含 prod）。"
              "\n   （只想查静态漂移请加 --static-only，不需要连库）")
        return 2

    static_registry_gate()

    print("\n[闸②] 正样本：期初 + 盘盈 + 盘亏作废 的库，E4 必须绿")
    cleanup()
    wh, mt = seed()
    print(f"  夹具：物料 {PREFIX}M1({mt}) 仓库 {PREFIX}W1({wh}) 库存 155"
          " = 收货50 + 期初100 + 盘盈5 - 盘亏8 + 作废回补8")
    code, out = run_audit()
    print("  ── 审计输出（E4 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "结果：", "🚨", "↳ 定位")):
            print("  " + line.rstrip())
    free_of("✅ [状态] E4 " in out, "E4 在期初/盘盈/盘亏作废库上判绿", e4_line(out))
    free_of("✅ [状态] E4b " in out, "E4b（biz_type 登记完整性）判绿", e4b_line(out))
    free_of("✅ [状态] E4c " in out, "E4c（无遗留 biz_type）判绿", e4c_line(out))
    free_of(code == 0, "组「状态」BLOCKER 0（退出码 0）", f"退出码 {code}\n{out[-600:]}")

    print("\n[闸③] 负样本 A：删掉 STOCK_TAKE_CANCEL 回补流水 ⇒ E4 必须红（判据不许放宽）")
    psql(f"delete from biz_inventory_txn where doc_no = '{PREFIX}ST-CANCEL'")
    code, out = run_audit()
    print("  ── 审计输出（E4 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "结果：", "🚨", "↳ 定位", PREFIX)):
            print("  " + line.rstrip())
    free_of("🚨 [状态] E4 " in out, "E4 对「作废没退库存」判红", e4_line(out))
    free_of(code != 0, "组「状态」退出码非 0（闸③ 会拦住）", f"退出码 {code}")
    free_of(f"{PREFIX}M1" in out, "E4 定位到夹具物料（给得出差在哪）",
            out[-800:] if f"{PREFIX}M1" not in out else "")
    free_of("8.0000" in out, "E4 报出的差额恰好 = 被删掉的回补量 8",
            "\n".join(l for l in out.split("\n") if PREFIX in l or "多出" in l or "定位" in l))
    # 复原回补流水，让后续闸门从「绿」状态出发（证明红是负样本造成的，不是残留）
    sin = one(f"select id from biz_stock_in where in_no = '{PREFIX}IN-1'")
    add_txn(mt, wh, "STOCK_TAKE_CANCEL", "STOCK_TAKE", "IN", 8, sin,
            f"{PREFIX}ST-CANCEL", "盘亏单作废回补")

    print("\n[闸④] 负样本 B：插一条未登记的 biz_type ⇒ E4b 必须红（防 E4 静默漏算）")
    psql(f"""insert into biz_inventory_txn
                 (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no,
                  line_no, qty, period_code, remark)
             values (gen_random_uuid(), '{mt}', '{wh}', 'IN', 'E4REG_UNKNOWN_TYPE', 'MYSTERY',
                     gen_random_uuid(), '{PREFIX}UNKNOWN-1', 1, 3,
                     to_char(now(), 'YYYY-MM'), '未登记类型负样本')""")
    code, out = run_audit()
    print("  ── 审计输出（E4b 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "结果：", "🚨", "未登记", "↳ 定位")):
            print("  " + line.rstrip())
    free_of("🚨 [状态] E4b " in out, "E4b 对未登记 biz_type 判红", e4b_line(out))
    free_of("E4REG_UNKNOWN_TYPE" in out, "E4b 报出未登记的类型名（可运维）", out[-800:])
    psql(f"delete from biz_inventory_txn where doc_no = '{PREFIX}UNKNOWN-1'")

    print("\n[闸⑤] 遗留值：老库里存在 biz_type='PURCHASE'/doc_type='PURCHASE_ORDER'"
          " ⇒ E4 不许误伤，E4c 必须 WARN 亮出来")
    psql(f"""insert into biz_inventory_txn
                 (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no,
                  line_no, qty, unit_cost, amount, period_code, remark)
             values (gen_random_uuid(), '{mt}', '{wh}', 'IN', 'PURCHASE', 'PURCHASE_ORDER',
                     gen_random_uuid(), '{PREFIX}LEGACY-1', 1, 5, 10, 50,
                     to_char(now(), 'YYYY-MM'), '遗留值（现行代码不产）')""")
    psql(f"update biz_inventory set qty = 160, amount = 1600 where material_id = '{mt}'")
    code, out = run_audit()
    print("  ── 审计输出（E4 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "E4c ", "结果：", "🚨", "⚠️", PREFIX)):
            print("  " + line.rstrip())
    free_of("✅ [状态] E4 " in out, "E4 对遗留值不误伤（仍绿）", e4_line(out))
    free_of("✅ [状态] E4b " in out, "E4b 仍绿（遗留值已登记，不是「未知」）", e4b_line(out))
    free_of("⚠️ [状态] E4c " in out, "E4c 对遗留值出 WARN（不静默放过）", e4c_line(out))
    psql(f"delete from biz_inventory_txn where doc_no = '{PREFIX}LEGACY-1'")
    psql(f"update biz_inventory set qty = 155, amount = 1550 where material_id = '{mt}'")

    print("\n[收尾] 清理夹具并复核回到绿")
    cleanup()
    code, out = run_audit()
    free_of("✅ [状态] E4 " in out and "✅ [状态] E4b " in out and "✅ [状态] E4c " in out,
            "清理后 E4/E4b/E4c 回到绿",
            " / ".join([e4_line(out), e4b_line(out), e4c_line(out)]))
    left = one(f"select count(*) from biz_material where material_code like '{PREFIX}%'")
    free_of(left == "0", "夹具已清空（无残留）", f"残留 biz_material {left} 行")

    print("\n" + "=" * 92)
    bad = [name for ok, name, _ in CHECKS if not ok]
    print(f"结果：{len(CHECKS) - len(bad)}/{len(CHECKS)} 断言通过"
          + (f"   🚨 失败 {len(bad)} 条" if bad else "   ★ 全绿"))
    if bad:
        for name in bad:
            print(f"  · {name}")
    print("=" * 92)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
