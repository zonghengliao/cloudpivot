#!/usr/bin/env python3
"""比对 di.v_poly_ref_map 各域 与 Rust 词表 ALL 的覆盖情况（读 git 已提交内容）。

用法: python3 registry_audit.py <repo> <rev>

判等口径：
  * 域 ↔ 枚举：登记表某域的 ref_type 集合 必须 == 该枚举 ALL 的落库值集合。
  * TXN_DOC.PURCHASE_ORDER 是**登记表里有、枚举里没有**的历史遗留值，
    文件里已注明「遗留值（TxnDocType 已无此值，存量直接挂采购单）」⇒ 记 INFO，不算红。
  * TXN_BIZ 不是多态引用域（biz_inventory_txn.biz_type 是「动作轴」，没有 id 列），
    登记表本来就没有这个域 ⇒ 记 N/A，不参与判等。
"""
import re
import subprocess
import sys

repo, rev = sys.argv[1], sys.argv[2]

LEGACY_SQL_ONLY = {("TXN_DOC", "PURCHASE_ORDER")}
NOT_A_POLY_REF_DOMAIN = {"TXN_BIZ"}


def git_show(path):
    return subprocess.run(["git", "-C", repo, "show", f"{rev}:{path}"],
                          capture_output=True, text=True, check=True).stdout


sql = git_show("docs/architecture/snapshot-drift/10_checks.sql")
rs = git_show("backend/src/business/shared/doc_kind.rs")

# ---- 解析登记表：只取 VALUES 块里 ( 'DOMAIN', 'TYPE', 'table'|NULL, ... 的行
m = re.search(r"CREATE OR REPLACE VIEW di\.v_poly_ref_map AS(.*?)\) AS t\(ref_domain", sql, re.S)
rows = {}
for line in m.group(1).splitlines():
    line = line.strip()
    if not line.startswith("('"):
        continue
    mm = re.match(r"\('([A-Z_]+)',\s*'([A-Z_0-9]+)',\s*(NULL|'([a-z_0-9]+)')", line)
    if not mm:
        raise SystemExit(f"UNPARSED: {line}")
    rows.setdefault(mm.group(1), {})[mm.group(2)] = mm.group(4)


def enum_all_and_values(enum_name):
    """返回 [(Member, "落库值")]，来自 impl 里的 ALL + as_str match。"""
    all_m = re.search(
        rf"impl {enum_name} \{{(.*?)\n    pub const ALL: \[{enum_name}; \d+\] = \[(.*?)\n    \];",
        rs, re.S)
    if not all_m:
        return None
    members = [x.strip().rstrip(",").split("::")[-1]
               for x in all_m.group(2).splitlines() if x.strip()]
    arm = dict(re.findall(rf"{enum_name}::(\w+) => \"([A-Z_0-9]+)\"", rs))
    return [(mem, arm[mem]) for mem in members]


DOMAIN_TO_ENUM = {
    "VOUCHER_SOURCE": "VoucherSourceType",
    "TXN_DOC": "TxnDocType",
    "FLOW_BIZ": "FlowBizType",
    "SERIAL_SOURCE": "SerialSourceType",
    "STATUS_ENTITY": "DocKind",
}

print(f"rev={rev}")
print("登记表域: " + ", ".join(f"{d}={len(v)}" for d, v in sorted(rows.items())))
fails = 0
for domain, enum in DOMAIN_TO_ENUM.items():
    pairs = enum_all_and_values(enum)
    if pairs is None:
        print(f"  [SKIP] {domain}: 未找到 {enum}::ALL")
        continue
    rust = {v for _, v in pairs}
    sqlv = set(rows.get(domain, {}))
    missing = [v for _, v in pairs if v not in sqlv]
    extra = sorted(sqlv - rust)
    legacy = [v for v in extra if (domain, v) in LEGACY_SQL_ONLY]
    unexpected_extra = [v for v in extra if v not in legacy]
    ok = not missing and not unexpected_extra
    fails += 0 if ok else 1
    print(f"  [{'PASS' if ok else 'FAIL'}] {domain} ({enum}) SQL={len(sqlv)} Rust={len(rust)}")
    if missing:
        print(f"        Rust 有 / SQL 缺: {missing}")
    if legacy:
        print(f"        SQL 有 / Rust 无（文件已注明的历史遗留值，可接受）: {legacy}")
    if unexpected_extra:
        print(f"        SQL 有 / Rust 无（未登记的意外值）: {unexpected_extra}")

# 非枚举域（手工清单）：只报行数，供人工核对 Rust 侧常量
for domain in ("ATTACHMENT_DOC", "REMINDER_DOC", "FLOW_CC_SOURCE"):
    print(f"  [INFO] {domain} 行数={len(rows.get(domain, {}))}"
          f"（对应 Rust 常量见 doc_kind.rs: ATTACHMENT_DOC_KINDS / REMINDER_DOC_KINDS / AssigneeKind）")
for domain in NOT_A_POLY_REF_DOMAIN:
    print(f"  [N/A ] {domain}: 不是多态引用域（无 id 列），登记表按设计不含该域")

sys.exit(1 if fails else 0)
