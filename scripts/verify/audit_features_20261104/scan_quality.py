#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUDIT-ALL 手段1b：质量信号扫描（逐条给 file:line，供报告引用）。

输出：
  A 权限码覆盖（每 handler 文件 的 #[alun::permission] 数量 vs pub async fn 数量）
  B 生产代码 panic 风险（非测试模块的 unwrap/expect/panic!）
  C 半截功能标记（未实现/暂不支持/占位/即将上线/stub/未开放/敬请期待）
  D 双真相嫌疑（同一表被多个模块独立写）
  E 前端占位按钮
"""
import os, re, sys, glob, collections

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/opt/cp_audit_head"


def rs_files(paths):
    out = []
    for p in paths:
        full = os.path.join(ROOT, p)
        if os.path.isdir(full):
            out += glob.glob(os.path.join(full, "**", "*.rs"), recursive=True)
        else:
            out += glob.glob(full + ("*" if not p.endswith(".rs") else ""))
    return sorted(set(out))


def feat_files():
    return {
        "task": rs_files(["backend/src/business/task"]),
        "flow": rs_files(["backend/src/business/flow"]),
        "erp": rs_files(["backend/src/business/erp"]),
        "project": rs_files(["backend/src/business/project"]),
        "system": rs_files(["backend/src/system"]),
        "permission+masking": rs_files(["backend/src/business/permission", "backend/src/business/masking"]),
        "collab": rs_files(["backend/src/business/announcement", "backend/src/business/notice"]),
        "storage": rs_files(["backend/src/support/file", "backend/src/business/storage", "backend/src/business/attachment"]),
        "impexp": rs_files(["backend/src/business/impexp"]),
        "help+changelog": rs_files(["backend/src/business/help", "backend/src/business/changelog"]),
        "job": rs_files(["backend/src/support/job"]),
        "shared": rs_files(["backend/src/business/shared"]),
    }


def split_prod_test(txt, fname=""):
    """粗略切出测试部分：从第一个 #[cfg(test)] 起到文件尾；独立的 *_tests.rs 整文件视为测试。"""
    if re.search(r"(^|[_/])(tests?|test_support)[a-z0-9_]*\.rs$", fname):
        return ("", txt)
    i = txt.find("#[cfg(test)]")
    return (txt, "") if i < 0 else (txt[:i], txt[i:])


PERM_RE = re.compile(r"#\[(?:alun|nik)::permission")


print("# AUDIT-ALL 手段1b · 质量信号扫描")
print(f"# repo_root = {ROOT}\n")

files = feat_files()

print("## A 权限码覆盖（handler 级）")
print(f"{'模块':<20} {'handler文件':>10} {'pub fn':>7} {'权限标注':>7} {'覆盖率':>7}")
for k, fs in files.items():
    hs = [f for f in fs if f.endswith("handler.rs") or "handler" in os.path.basename(f)]
    pub = perm = 0
    for f in hs:
        prod, _ = split_prod_test(open(f, encoding="utf-8", errors="replace").read(), f)
        pub += len(re.findall(r"pub\s+async\s+fn\s+\w+", prod))
        perm += len(PERM_RE.findall(prod))
    if hs:
        print(f"{k:<20} {len(hs):>10} {pub:>7} {perm:>7} {(100*perm/max(pub,1)):>6.0f}%")

print("\n## A2 有 handler 文件但零权限标注的文件（潜在越权/漏码）")
for k, fs in files.items():
    for f in fs:
        n = os.path.basename(f)
        if "handler" not in n:
            continue
        txt = open(f, encoding="utf-8", errors="replace").read()
        prod, _ = split_prod_test(txt, f)
        pub = len(re.findall(r"pub\s+async\s+fn\s+\w+", prod))
        perm = len(PERM_RE.findall(prod))
        if pub and perm == 0:
            print(f"  {os.path.relpath(f, ROOT)}: pub fn={pub}, permission=0")

print("\n## B 生产代码 panic 风险（排除 #[cfg(test)] 之后）")
tot = collections.Counter()
hits = []
for k, fs in files.items():
    for f in fs:
        prod, _ = split_prod_test(open(f, encoding="utf-8", errors="replace").read(), f)
        for i, line in enumerate(prod.splitlines(), 1):
            if re.search(r"\.unwrap\(\)|\.expect\(|panic!\(", line):
                tot[k] += 1
                hits.append((k, os.path.relpath(f, ROOT), i, line.strip()[:110]))
for k, v in tot.most_common():
    print(f"  {k:<20} {v}")
print("  —— TOP20 明细 ——")
for k, f, i, s in hits[:20]:
    print(f"  [{k}] {f}:{i}  {s}")
print(f"  —— 合计 {len(hits)} 处 ——")

print("\n## C 半截功能标记")
PAT = r"未实现|暂不支持|暂未实现|占位符?|即将上线|敬请期待|开发中|待实现|not implemented|unimplemented!|todo!\(\)|(TODO|FIXME)\s*[:：]|//\s*(TODO|FIXME)\b|#\s*(TODO|FIXME)\b"
c = collections.Counter()
detail = []
for k, fs in files.items():
    for f in fs:
        for i, line in enumerate(open(f, encoding="utf-8", errors="replace").read().splitlines(), 1):
            if re.search(PAT, line):
                c[k] += 1
                detail.append((k, os.path.relpath(f, ROOT), i, line.strip()[:120]))
for k, v in c.most_common():
    print(f"  {k:<20} {v}")
print(f"  —— 合计 {len(detail)} 处；明细 TOP30 ——")
for k, f, i, s in detail[:30]:
    print(f"  [{k}] {f}:{i}  {s}")

print("\n## D 双真相嫌疑：同一张业务表被多个模块写入（INSERT/UPDATE 命中文件数 > 1）")
tables = ["biz_inventory", "biz_inventory_txn", "biz_inventory_location", "biz_voucher",
          "biz_voucher_entry", "biz_payable", "biz_stock_in", "biz_stock_out",
          "biz_purchase_order", "biz_stock_transfer", "biz_material_serial", "biz_status_log"]
writers = collections.defaultdict(set)
for f in glob.glob(os.path.join(ROOT, "backend/src/**/*.rs"), recursive=True):
    txt = open(f, encoding="utf-8", errors="replace").read()
    rel = os.path.relpath(f, ROOT)
    for m in re.finditer(r"(INSERT\s+INTO|UPDATE)\s+([a-z_]+)", txt, re.I):
        t = m.group(2)
        if t in tables:
            writers[t].add(rel)
for t in tables:
    ws = sorted(writers[t])
    flag = "  <== 多写入点" if len(ws) > 1 else ""
    print(f"  {t:<26} {len(ws):>2} 文件{flag}")
    for w in ws[:8]:
        print(f"      - {w}")

print("\n## E 前端占位/死按钮（TODO/即将上线/敬请期待/未开放/console.log 遗留）")
fes = glob.glob(os.path.join(ROOT, "frontend/src/**/*.vue"), recursive=True) + \
      glob.glob(os.path.join(ROOT, "frontend/src/**/*.ts"), recursive=True)
c2 = collections.Counter()
det2 = []
for f in fes:
    txt = open(f, encoding="utf-8", errors="replace").read()
    for i, line in enumerate(txt.splitlines(), 1):
        if re.search(r"TODO|FIXME|即将上线|敬请期待|未开放|开发中|占位", line):
            c2[os.path.relpath(f, ROOT)] += 1
            det2.append((os.path.relpath(f, ROOT), i, line.strip()[:110]))
print(f"  命中文件 {len(c2)} 个，共 {len(det2)} 处")
for f, i, s in det2[:25]:
    print(f"  {f}:{i}  {s}")
