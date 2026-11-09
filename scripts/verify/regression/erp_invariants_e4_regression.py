#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
erp_invariants.py **E4 / E4b 回归**（t_086dab13）
================================================
E4「作废单的库存已回滚」原来手写白名单只认 TRANSFER_IN/OUT + SERIAL_IN/OUT，
漏掉 OPENING（库存期初）/ STOCK_TAKE_GAIN（盘盈）/ STOCK_TAKE_CANCEL（盘亏作废回补）
⇒ 任何用期初建账或做过盘盈的库必假红。本脚本守三件事：

  ① 静态漂移（不需要数据库）—— 三层真相链，每层单独断言：
     **写入点常量/字面量 ⊆ 词表（`doc_kind.rs::TXN_BIZ_TYPE_VALUES`）⊆ E4 登记表
     （`scripts/verify/erp_invariants.py::E4_BIZ_TYPE_AXIS`）**，外加前端
     `doc-kinds.ts::TXN_BIZ_TYPES` 镜像逐字同序。
     写入点靠**内容探测**（出现 `inventory_writer::insert_txn` / `TxnRecord {` 即算），
     不维护模块清单 —— 旧版用清单，于是 `sales_return_posting.rs` 的
     `SALES_RETURN_IN` 一直被漏扫（t_9c938b6b 的真 BLOCKER）。本脚本还内置两条
     负样本自检（注入词表外字面量 / 摘掉登记表一行），证明这道闸有牙齿。
  ② 正样本：在隔离库造「收货 50（单据轴）＋库存期初 100＋盘盈 5＋盘亏 8 并作废回补 8」
     的完整流水链，断言 E4 仍**绿**（修前这条必红，见 docs/evidence 里的 before 输出）。
  ③ 负样本（判据强度不许放宽）：
       · 删掉那条 STOCK_TAKE_CANCEL 回补流水 ⇒ E4 **必须红**，且定位到夹具物料、多出恰好 8；
       · 插一条未登记的 biz_type 流水        ⇒ E4b **必须红**（否则 E4 会静默漏算）。
       · 销售退货回库（SALES_RETURN_IN）的入库单被撤成 DRAFT ⇒ E4 **必须红**
         —— 这条盯的是「SALES_RETURN_IN 归 DOC 轴」这个判定：登记成 NET 时会一直绿（判据被放宽）。
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


# ── 闸①：静态漂移（写入点字面量 ⊆ 词表 ⊆ E4 登记表）──────────────────────
# 三层，每一层都单独断言（顺序就是真相链）：
#
#     写入点常量/字面量  ⊆  `TXN_BIZ_TYPE_VALUES`（词表，doc_kind.rs）  ⊆  `E4_BIZ_TYPE_AXIS`
#
# 为什么这条闸以前漏了 `SALES_RETURN_IN`（t_9c938b6b，真 BLOCKER）：
#   ① 写入点模块是**手写清单**（旧 `E4_TXN_WRITER_MODULES`），`sales_return_posting.rs`
#      不在里面 ⇒ 它的 `pub const BIZ_TYPE_SALES_RETURN_IN = "SALES_RETURN_IN"` 从来没被扫到；
#   ② 只比「枚举 + 清单里的常量」↔ 登记表，**缺了「写入点 ⊆ 词表」这一环** —— 当写入点
#      用一个词表里根本没有的值时，枚举侧与登记表侧同时缺这一个值，集合相等发现不了。
# 现在：写入点靠**内容探测**（出现 `inventory_writer::insert_txn` / `TxnRecord {` 即算写入点），
# 新模块天生被覆盖；三层各自比对，任一层漏一个值立刻红。
#
# 数量判据**不写死**：期望集合从 `TXN_BIZ_TYPE_VALUES` 解析（唯一真相），
# 枚举解析结果必须与之逐字一致 —— 否则一旦词表扩容（19 → 22 → 23）这条闸会拿旧数字误判。
TXN_WRITE_MARKERS = ("insert_txn(", "TxnRecord {")
# 探测本身要有牙齿：这两处是动账写入的主干（唯一写入口 + 收货过账）。一旦探测规则失效
# （比如把 insert_txn 改名），命中集会自动缩水 —— 那绝不能表现为「全绿」，必须红。
TXN_WRITE_ANCHORS = ("erp/inventory_writer.rs", "erp/stockin_write.rs")
BIZ_TS_MIRROR = REPO / "frontend" / "src" / "api" / "business" / "erp" / "doc-kinds.ts"

# 写入点里的 biz_type 字面量：`pub const [X_]BIZ_TYPE[X_]: &str = "LIT"`
# （同时吃 `purchase_return_posting` 那种 `pub const TXN_BIZ_TYPE`），
# 以及直接写在 `TxnRecord {` 里的 `biz_type: "LIT"`。
BIZ_CONST_RE = re.compile(r'pub const [A-Z0-9_]*BIZ_TYPE[A-Z0-9_]*\s*:\s*&str\s*=\s*"([A-Z][A-Z0-9_]*)"')
BIZ_INLINE_RE = re.compile(r'biz_type:\s*"([A-Z][A-Z0-9_]*)"')


def discover_txn_writers(business_root):
    """动账写入点模块：出现 `insert_txn(` / `TxnRecord {` 的 .rs —— 内容探测，不维护清单。"""
    found = {}
    for path in sorted(business_root.rglob("*.rs")):
        text = path.read_text(encoding="utf-8", errors="ignore")
        if any(marker in text for marker in TXN_WRITE_MARKERS):
            found[path] = text
    return found


def scan_biz_type_layers(business_root, audit_path, doc_kind_path, ts_mirror, extra_modules=None):
    """把三层真相解析出来（**纯读**：不打印、不记 FAILS —— 闸①和它的自检共用这一份逻辑）。

    `extra_modules` = {文件名: 源码文本}，自检用它在内存里**注入**一个写入点模块，
    模拟「新模块写了词表里没有的字面量」，不必改仓库里的任何文件。
    """
    out = {"problems": [], "writers": {}, "write_literals": {}, "registered": set(),
           "registered_axes": {}, "vocabulary": [], "enum_values": set()}

    src = audit_path.read_text(encoding="utf-8")
    m = re.search(r"^E4_BIZ_TYPE_AXIS = \{(.*?)^\}", src, re.M | re.S)
    if not m:
        out["problems"].append("登记表 E4_BIZ_TYPE_AXIS 不可解析")
    else:
        body = m.group(1)
        out["registered"] = set(re.findall(r'"([A-Z0-9_]+)"\s*:', body))
        out["registered_axes"] = dict(re.findall(r'"([A-Z0-9_]+)"\s*:\s*"([A-Z]+)"', body))

    doc_kind = doc_kind_path.read_text(encoding="utf-8")
    impl = re.search(r"^impl TxnBizType \{(.*?)^\}", doc_kind, re.M | re.S)
    as_str = (re.search(r"pub const fn as_str\(self\) -> &'static str \{(.*?)\n    \}\n", impl.group(1), re.S)
              if impl else None)
    if as_str:
        out["enum_values"] = set(re.findall(r'=>\s*"([A-Z0-9_]+)"', as_str.group(1)))
    else:
        out["problems"].append("doc_kind.rs 的 TxnBizType::as_str() 不可解析")

    domain = re.search(r"const TXN_BIZ_TYPE_VALUES: \[&str; (\d+)\] = \[(.*?)\n\];", doc_kind, re.S)
    if domain:
        out["vocabulary"] = re.findall(r'"([A-Z0-9_]+)"', domain.group(2))
        out["vocabulary_declared"] = int(domain.group(1))
    else:
        out["problems"].append("doc_kind.rs 的 TXN_BIZ_TYPE_VALUES 不可解析")

    writers = discover_txn_writers(business_root)
    for name, text in (extra_modules or {}).items():
        writers[Path(name)] = text
    out["writers"] = writers
    for path, text in writers.items():
        for lit in BIZ_CONST_RE.findall(text) + BIZ_INLINE_RE.findall(text):
            out["write_literals"].setdefault(lit, []).append(str(path))

    vocab = set(out["vocabulary"])
    registered = out["registered"]
    out["not_in_vocabulary"] = sorted(set(out["write_literals"]) - vocab)
    out["not_in_registry"] = sorted(vocab - registered)
    # 冗余但独立的一条：写入点直接对登记表（链路 写入点 ⊆ 词表 ⊆ 登记表 的合成结论）。
    # 单独断言的价值是报错更直白：写入点用了一个**谁都不认识**的值时，
    # 只看「词表 ⊆ 登记表」是看不出来的（两侧同时缺同一个值）。
    out["sites_not_in_registry"] = sorted(set(out["write_literals"]) - registered)
    legacy = {k for k, v in out["registered_axes"].items() if v == "LEGACY"}
    out["registry_extras_not_legacy"] = sorted((registered - vocab) - legacy)

    if ts_mirror and Path(ts_mirror).exists():
        ts = Path(ts_mirror).read_text(encoding="utf-8")
        mirror = re.search(r"export const TXN_BIZ_TYPES = \[(.*?)\] as const;", ts, re.S)
        out["ts_mirror"] = re.findall(r"'([A-Z0-9_]+)'", mirror.group(1)) if mirror else []
    else:
        out["ts_mirror"] = []
    return out


def static_registry_gate():
    print("\n[闸①] 三层静态漂移：写入点字面量 ⊆ 词表 ⊆ E4 登记表")
    r = scan_biz_type_layers(REPO / "backend/src/business", AUDIT,
                             REPO / "backend/src/business/shared/doc_kind.rs", BIZ_TS_MIRROR)
    for problem in r["problems"]:
        free_of(False, problem)
    if r["problems"]:
        return

    registered, vocab, enum_vals = r["registered"], set(r["vocabulary"]), r["enum_values"]
    free_of(len(registered) > 0, f"登记表已解析（{len(registered)} 种）", str(sorted(registered)))
    free_of(len(vocab) > 0, f"词表 TXN_BIZ_TYPE_VALUES 已解析（{len(vocab)} 种）",
            str(sorted(vocab)))
    free_of(len(vocab) == r.get("vocabulary_declared"),
            f"词表声明长度与条目数一致（声明 {r.get('vocabulary_declared')} / 实际 {len(vocab)}）")
    free_of(enum_vals == vocab,
            f"TxnBizType::as_str() 与词表逐字一致（枚举 {len(enum_vals)} / 词表 {len(vocab)}）",
            f"枚举独有 {sorted(enum_vals - vocab)} / 词表独有 {sorted(vocab - enum_vals)}")

    # ── 写入点探测（内容探测 + 锚点自检）──
    writer_names = {p.relative_to(REPO / "backend/src/business").as_posix()
                    for p in r["writers"] if str(p).startswith(str(REPO / "backend/src/business"))}
    print(f"        ℹ️ 探测到 {len(writer_names)} 个动账写入点模块：{sorted(writer_names)}")
    free_of(len(writer_names) >= 5, f"动账写入点模块探测到 {len(writer_names)} 个（≥5）",
            str(sorted(writer_names)))
    missing_anchor = [a for a in TXN_WRITE_ANCHORS if a not in writer_names]
    free_of(not missing_anchor, "写入点探测锚点在场（探测规则没失效）",
            f"锚点缺失 {missing_anchor} —— 探测规则（{TXN_WRITE_MARKERS}）可能失效，"
            "不许按「扫不到 = 没问题」放行")

    # ── 层一 → 层二：写入点字面量 ⊆ 词表 ──
    detail_w = ""
    if r["not_in_vocabulary"]:
        detail_w = ("写入点用了词表里没有的值：" +
                    "；".join(f"{k} <- {r['write_literals'][k]}" for k in r["not_in_vocabulary"]) +
                    " —— 补进 doc_kind.rs::TxnBizType + TXN_BIZ_TYPE_VALUES")
    free_of(not r["not_in_vocabulary"],
            f"写入点 biz_type 字面量全部在词表里（{len(r['write_literals'])} 个）", detail_w)

    # ── 层二 → 层三：词表 ⊆ E4 登记表 ──
    free_of(not r["not_in_registry"], "词表里的 biz_type 全部已登记进 E4 判据",
            ("未登记：" + ", ".join(r["not_in_registry"]) + f" —— 请登记到 {REGISTRY_OWNER}")
            if r["not_in_registry"] else "")

    # ── 合成结论（冗余断言，报错更直白）：写入点字面量 ⊆ 登记表 ──
    free_of(not r["sites_not_in_registry"], "写入点的 biz_type 全部已登记进 E4 判据",
            ("写入点的值既不在词表也不在登记表：" + ", ".join(r["sites_not_in_registry"]) +
             f" —— 先补 doc_kind.rs 词表，再登记到 {REGISTRY_OWNER}")
            if r["sites_not_in_registry"] else "")

    # ── 层三反向：登记表不许留「词表里没有、又不是 LEGACY」的孤儿值 ──
    free_of(not r["registry_extras_not_legacy"], "登记表没有来历不明的多余值（词表外只允许 LEGACY）",
            f"既不在词表、也没标 LEGACY：{r['registry_extras_not_legacy']}")

    # ── 前端镜像：doc-kinds.ts 的 TXN_BIZ_TYPES 与词表逐字同序 ──
    free_of(r["ts_mirror"] == r["vocabulary"],
            f"前端 doc-kinds.ts 镜像与词表逐字同序（{len(r['ts_mirror'])} 项）",
            f"前端独有 {sorted(set(r['ts_mirror']) - vocab)} / 词表独有前端的差集 "
            f"{sorted(vocab - set(r['ts_mirror']))}（顺序也要一致）")

    print(f"        ℹ️ 写入点字面量 {len(r['write_literals'])} 个：{sorted(r['write_literals'])}")


def static_gate_self_test():
    """闸① 的两条负样本 —— 证明它**真的**能抓到 t_9c938b6b 那种漏法。

    A. 内存里注入一个「写入点写了词表里没有的字面量」的模块 ⇒ 层一/层二必须同时红并点名；
       旧闸（手写模块清单）对这个注入判绿，所以这条自检就是回归卡本身。
    B. 拿掉登记表里的一行（OPENING）⇒ 「词表 ⊆ 登记表」必须红并点名 OPENING。
    """
    import tempfile

    business_root = REPO / "backend/src/business"
    doc_kind = business_root / "shared/doc_kind.rs"
    fake = ('fn write() {\n    inventory_writer::insert_txn(...);\n}\n'
            'pub const BIZ_TYPE_E4REG_GATE_FAKE: &str = "E4REG_GATE_FAKE";\n')
    r = scan_biz_type_layers(business_root, AUDIT, doc_kind, BIZ_TS_MIRROR,
                             extra_modules={"<selftest>/fake_writer.rs": fake})
    free_of("E4REG_GATE_FAKE" in r["not_in_vocabulary"],
            "闸①自检 A1：写入点用了词表外字面量 → 「写入点 ⊆ 词表」判红",
            f"实际 not_in_vocabulary={r['not_in_vocabulary']}")
    free_of("E4REG_GATE_FAKE" in r["sites_not_in_registry"],
            "闸①自检 A2：同一个值 → 「写入点 ⊆ 登记表」判红（旧闸在这里判绿）",
            f"实际 sites_not_in_registry={r['sites_not_in_registry']}")

    src = AUDIT.read_text(encoding="utf-8").replace('    "OPENING": "NET",\n', "")
    with tempfile.TemporaryDirectory() as tmp:
        audit_copy = Path(tmp) / "erp_invariants_no_opening.py"
        audit_copy.write_text(src, encoding="utf-8")
        r2 = scan_biz_type_layers(business_root, audit_copy, doc_kind, BIZ_TS_MIRROR)
    free_of("OPENING" in r2["not_in_registry"],
            "闸①自检 B：登记表少一行（OPENING）→ 「词表 ⊆ 登记表」判红并点名",
            f"实际 not_in_registry={r2['not_in_registry']}")


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
    # 成本账先删（biz_inventory.location_id → biz_inventory_location.id），再删库位账。
    # 漏删库位账 = 每跑一次留一行孤儿（t_45bda7ad 在 E2E-08 上的同类事故），别省这一步。
    psql(f"delete from biz_inventory_location where material_id in ({mat})")
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
        static_gate_self_test()
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
    static_gate_self_test()

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

    print("\n[闸⑥] 销售退货入库（SALES_RETURN_IN 归 **DOC 轴**）的判据牙齿："
          "\n         单据在 ⇒ E4 绿；单据被撤成 DRAFT ⇒ E4 必须红（若误登记成 NET，这条会一直是绿）")
    psql(f"""insert into biz_stock_in
                 (id, in_no, in_type, warehouse_id, in_date, status, source_type, source_id,
                  total_qty, total_amount, total_tax, total_with_tax, is_sourceless)
             values (gen_random_uuid(), '{PREFIX}IN-RET', 'RETURN', '{wh}', current_date,
                     'POSTED', 'SALES_RETURN', gen_random_uuid(),
                     4, 40, 0, 40, true)""")
    sin_ret = one(f"select id from biz_stock_in where in_no = '{PREFIX}IN-RET'")
    psql(f"""insert into biz_stock_in_item
                 (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl,
                  amount, tax_amount, with_tax_amount)
             values (gen_random_uuid(), '{sin_ret}', 1, '{mt}',
                     (select unit_id from biz_material where id = '{mt}'), 4, 10, 10, 40, 0, 40)""")
    add_txn(mt, wh, "SALES_RETURN_IN", "STOCK_IN", "IN", 4, sin_ret, f"{PREFIX}IN-RET", "销售退货回库")
    psql(f"update biz_inventory set qty = 159, amount = 1590 where material_id = '{mt}'")

    code, out = run_audit()
    print("  ── 审计输出（E4 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "结果：", "🚨", PREFIX)):
            print("  " + line.rstrip())
    free_of("✅ [状态] E4 " in out, "退货回库有 POSTED 入库单时 E4 判绿（DOC 轴能解释这批数量）",
            e4_line(out))
    free_of("✅ [状态] E4b " in out, "SALES_RETURN_IN 已登记（E4b 绿，不再是「未登记」）", e4b_line(out))

    # 负样本：把退货入库单撤成 DRAFT（= 单据轴不再解释这 4 件）
    psql(f"update biz_stock_in set status = 'DRAFT' where in_no = '{PREFIX}IN-RET'")
    code, out = run_audit()
    print("  ── 审计输出（E4 相关）──")
    for line in out.split("\n"):
        if any(k in line for k in ("E4 ", "E4b ", "结果：", "🚨", "↳ 定位", PREFIX)):
            print("  " + line.rstrip())
    free_of("🚨 [状态] E4 " in out, "退货入库单被撤成 DRAFT ⇒ E4 判红（4 件库存没人解释）", e4_line(out))
    free_of("✅ [状态] E4b " in out,
            "同一场景 E4b 仍绿 —— 证明红是 E4 判据抓的，不是「未登记」旁路兜的", e4b_line(out))
    free_of(f"{PREFIX}M1" in out and "4.0000" in out, "E4 点得出差在哪（物料 + 差额 4）",
            "\n".join(l for l in out.split("\n") if PREFIX in l or "多出" in l))

    # 复原：撤掉退货夹具，库存回到 155（后续收尾按 155 清理）
    psql(f"delete from biz_inventory_txn where doc_no = '{PREFIX}IN-RET'")
    psql(f"delete from biz_stock_in_item where stock_in_id = '{sin_ret}'")
    psql(f"delete from biz_stock_in where id = '{sin_ret}'")
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
