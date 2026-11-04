#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUDIT-ALL 手段1：代码扫描。输出每功能的规模/结构/风险指标。

用法：python3 scan_code.py [repo_root] > 01_code_scan.txt
口径说明：
  · LOC = 物理行数（wc -l），非逻辑行
  · tests = 含 #[cfg(test)] 的文件数；testfns = #[test] / #[tokio::test] 出现次数
  · risk = TODO/FIXME/unimplemented!/todo!() 次数（硬编码/半截功能嫌疑）
  · unwrap = .unwrap() 次数（panic 风险，排除 .unwrap_or*）
  · perm = #[alun::permission] 标注数（handler 权限码覆盖）
  · rawsql = 含 "SELECT" 的字符串字面量行数（SQL 拼接风险）
"""
import os, re, sys, subprocess, json

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/opt/cp_audit_head"

FEATURES = [
    # (编号, 名称, [后端路径], [前端路径], [文档 glob 关键词])
    (1, "工作任务管理", ["backend/src/business/task"], ["frontend/src/views/business/task", "frontend/src/api/business"], ["任务", "task"]),
    (2, "审批流引擎", ["backend/src/business/flow"], ["frontend/src/views/business/flow", "frontend/src/api/business/flow"], ["工作流", "流程", "flow"]),
    (3, "采购管理", ["backend/src/business/erp/contract_handler.rs", "backend/src/business/erp/contract_model.rs", "backend/src/business/erp/contract_dto.rs", "backend/src/business/erp/contract_bridge.rs", "backend/src/business/erp/contract_approval_tests.rs", "backend/src/business/erp/purchase_handler.rs", "backend/src/business/erp/purchase_model.rs", "backend/src/business/erp/purchase_dto.rs", "backend/src/business/erp/purchase_bridge.rs", "backend/src/business/erp/purchase_credit.rs"], ["frontend/src/views/business/erp"], ["采购", "合同"]),
    (4, "收货管理", ["backend/src/business/erp/stockin*.rs"], ["frontend/src/views/business/erp"], ["收货", "入库"]),
    (5, "出库管理", ["backend/src/business/erp/stockout*.rs"], ["frontend/src/views/business/erp"], ["出库", "领用"]),
    (6, "库存管理", ["backend/src/business/erp/model_inventory.rs", "backend/src/business/erp/inventory_audit.rs", "backend/src/business/erp/opening.rs", "backend/src/business/erp/serial_model.rs", "backend/src/business/erp/serial_handler.rs", "backend/src/business/erp/serial_dto.rs", "backend/src/business/erp/stocktake*.rs", "backend/src/business/erp/batch.rs"], ["frontend/src/views/business/erp"], ["库存", "序列号", "期初"]),
    (7, "调拨管理", ["backend/src/business/erp/transfer*.rs"], ["frontend/src/views/business/erp"], ["调拨"]),
    (8, "财务管理", ["backend/src/business/erp/voucher*.rs", "backend/src/business/erp/payable*.rs"], ["frontend/src/views/business/erp"], ["凭证", "应付", "付款"]),
    (9, "财务报表", ["backend/src/business/erp/report.rs", "backend/src/business/erp/income_statement.rs", "backend/src/business/erp/cash_flow.rs", "backend/src/business/erp/downloads"], ["frontend/src/views/business/erp"], ["报表", "利润表", "现金流量"]),
    (10, "会计期间/年结", ["backend/src/business/erp/period*.rs", "backend/src/business/erp/year_end*.rs"], ["frontend/src/views/business/erp"], ["期间", "年结", "结账"]),
    (11, "总账/辅助账", ["backend/src/business/erp/ledger.rs", "backend/src/business/erp/aux_tests.rs"], ["frontend/src/views/business/erp"], ["总账", "明细账", "辅助核算"]),
    (12, "基础数据", ["backend/src/business/erp/model_master.rs", "backend/src/business/erp/unit_guard.rs"], ["frontend/src/views/business/erp"], ["物料", "供应商", "仓库", "科目"]),
    (13, "项目管理", ["backend/src/business/project"], ["frontend/src/views/business/project"], ["项目"]),
    (14, "系统管理", ["backend/src/system"], ["frontend/src/views/system"], ["系统管理", "权限"]),
    (15, "数据权限", ["backend/src/business/permission", "backend/src/business/masking", "backend/src/business/shared/scope.rs"], ["frontend/src/views/system/data-scope", "frontend/src/views/system/masking"], ["数据权限", "脱敏"]),
    (16, "协作", ["backend/src/business/announcement", "backend/src/business/notice"], ["frontend/src/views/business/announcement", "frontend/src/views/business/notice"], ["公告", "站内信", "通知"]),
    (17, "文件存储", ["backend/src/support/file", "backend/src/business/storage", "backend/src/business/attachment"], ["frontend/src/views/support/file", "frontend/src/views/business/storage"], ["文件", "存储", "附件"]),
    (18, "导入导出", ["backend/src/business/impexp"], ["frontend/src/views/business/impexp", "frontend/src/api/business/impexp"], ["导入", "导出"]),
    (19, "帮助中心/变更日志", ["backend/src/business/help", "backend/src/business/changelog"], ["frontend/src/views/business/help", "frontend/src/views/system/changelog"], ["帮助", "变更日志"]),
    (20, "定时任务", ["backend/src/support/job"], ["frontend/src/views/support/job"], ["定时任务", "cron"]),
    (21, "流程模板库", ["backend/src/business/flow"], ["frontend/src/views/business/flow"], ["模板"]),
    (22, "状态机配置化", ["backend/src/business/shared/state_machine.rs"], ["frontend/src/views/business/erp", "frontend/src/views/system"], ["状态机"]),
    (23, "跨单据联动白名单", ["backend/src/business/flow/linkage"], ["frontend/src/views/business/flow"], ["联动"]),
    (24, "巡检体系", ["scripts/verify/erp_invariants.py", "scripts/verify/snapshot_audit.py", "docs/architecture/snapshot-drift"], ["frontend/src/views/business/erp"], ["巡检", "不变量", "快照"]),
    (25, "审批进度显示", ["backend/src/business/erp/approval_gate.rs"], ["frontend/src/views/business/erp", "frontend/src/views/business/flow"], ["审批进度"]),
    (26, "结账检查清单", ["backend/src/business/erp/period_items*.rs", "backend/src/business/erp/period_owner.rs", "backend/src/business/erp/period_remind*.rs"], ["frontend/src/views/business/erp"], ["结账检查", "检查清单"]),
]


def expand(pats):
    out = []
    for p in pats:
        full = os.path.join(ROOT, p)
        import glob as g
        hits = sorted(g.glob(full)) if any(c in p for c in "*?[") else ([full] if os.path.exists(full) else [])
        for h in hits:
            if os.path.isdir(h):
                for dirpath, _dirs, files in os.walk(h):
                    for f in files:
                        if f.endswith((".rs", ".py", ".sh", ".sql", ".ts", ".vue", ".mjs", ".md")):
                            out.append(os.path.join(dirpath, f))
            elif os.path.isfile(h):
                out.append(h)
    return sorted(set(out))


def metrics(files):
    loc = 0
    per_file = []
    tests_files = testfns = risk = unwrap = perm = rawsql = 0
    for f in files:
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                txt = fh.read()
        except OSError:
            continue
        n = txt.count("\n") + (0 if txt.endswith("\n") else 1)
        loc += n
        per_file.append((n, os.path.relpath(f, ROOT)))
        if "#[cfg(test)]" in txt:
            tests_files += 1
        testfns += len(re.findall(r"#\[(?:tokio::)?test\]", txt))
        risk += len(re.findall(r"\bTODO\b|\bFIXME\b|unimplemented!|todo!\(\)", txt))
        unwrap += len(re.findall(r"\.unwrap\(\)", txt))
        perm += len(re.findall(r"#\[alun::permission", txt))
        if f.endswith(".rs"):
            rawsql += len(re.findall(r'"[^"]*\bSELECT\b', txt))
    per_file.sort(reverse=True)
    return dict(loc=loc, files=len(files), tests_files=tests_files, testfns=testfns,
                risk=risk, unwrap=unwrap, perm=perm, rawsql=rawsql,
                maxfile=per_file[0] if per_file else (0, "-"), top=per_file[:5])


def doc_hits(names, keys):
    hits = []
    for dirpath, _d, files in os.walk(os.path.join(ROOT, "docs")):
        if "node_modules" in dirpath:
            continue
        for f in files:
            if not f.endswith(".md"):
                continue
            rel = os.path.relpath(os.path.join(dirpath, f), ROOT)
            if any(k in f for k in keys):
                hits.append(rel)
    return sorted(hits)


print(f"# AUDIT-ALL 手段1 · 代码扫描原始输出")
print(f"# repo_root = {ROOT}")
print(f"# commit    = {subprocess.run(['git','-C',ROOT,'rev-parse','HEAD'],capture_output=True,text=True).stdout.strip()}")
print()
hdr = f"{'#':>3} {'功能':<16} {'文件':>4} {'LOC':>7} {'测试文件':>6} {'测试函数':>6} {'风险标记':>6} {'unwrap':>6} {'权限码':>5} {'裸SQL':>5}  {'最大文件':<52} 文档"
print(hdr)
print("-" * len(hdr))
rows = []
for no, name, be, fe, keys in FEATURES:
    files = expand(be) + expand(fe)
    m = metrics(files)
    docs = doc_hits(name, keys)
    rows.append((no, name, m, docs))
    mv = f"{m['maxfile'][1]} ({m['maxfile'][0]})"
    print(f"{no:>3} {name:<16} {m['files']:>4} {m['loc']:>7} {m['tests_files']:>6} {m['testfns']:>6} "
          f"{m['risk']:>6} {m['unwrap']:>6} {m['perm']:>5} {m['rawsql']:>5}  {mv:<52} {len(docs)}")

print()
print("## 每功能 TOP5 最大文件")
for no, name, m, docs in rows:
    print(f"\n### {no} {name}")
    for n, p in m["top"]:
        print(f"  {n:>6}  {p}")

print()
print("## 文档命中明细")
for no, name, m, docs in rows:
    print(f"\n### {no} {name}  ({len(docs)} 篇)")
    for d in docs[:12]:
        print(f"  - {d}")

print()
print("## 全仓基础数据")
def sh(c):
    return subprocess.run(c, shell=True, capture_output=True, text=True, cwd=ROOT).stdout.strip()

print("backend/src 总行数        :", sh('find backend/src -name "*.rs" | xargs cat | wc -l'))
print("backend .rs 文件数        :", sh('find backend/src -name "*.rs" | wc -l'))
print("backend 最大 10 文件      :")
print(sh('find backend/src -name "*.rs" | xargs wc -l | sort -rn | sed -n "2,12p"'))
print("frontend/src 总行数       :", sh('find frontend/src -name "*.vue" -o -name "*.ts" | xargs cat | wc -l'))
print("frontend 最大 10 文件     :")
print(sh('find frontend/src \\( -name "*.vue" -o -name "*.ts" \\) | xargs wc -l | sort -rn | sed -n "2,12p"'))
print("迁移文件数                :", sh('ls backend/migrations/*/V*.up.sql | wc -l'))
print("迁移最大版本              :", sh('ls backend/migrations/*/V*.up.sql | sed "s/.*\\/V\\([0-9]*\\).*/\\1/" | sort -n | tail -1'))
print("全仓 TODO/FIXME/unimplemented：")
print(sh('grep -rn "TODO\\|FIXME\\|unimplemented!\\|todo!()" backend/src --include="*.rs" | wc -l'))
print("全仓 .unwrap() 数量        :", sh('grep -rn "\\.unwrap()" backend/src --include="*.rs" | wc -l'))
print("全仓 #[alun::permission]   :", sh('grep -rn "#\\[alun::permission" backend/src --include="*.rs" | wc -l'))
