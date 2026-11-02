#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""module_graph.py —— 模块级勾稽关系静态扫描器（ARCH-3）

一句话：把「谁和谁有数据往来」从源码里量出来，让「孤岛」「断链」变成可复现的数字，
而不是架构师拍脑袋的印象。

扫描做什么
----------
1. 从 migrations 抓 `CREATE TABLE` → 得到「系统里真的有哪些表」（事实清单）。
2. 从后端 Rust 源码抽 **SQL 字符串字面量**（跨行、含 `\` 续行），在字面量内部：
   · 统计每张表被 `FROM` / `JOIN` / `INSERT INTO` / `UPDATE` 引用的次数；
   · 抽取 `JOIN <表> <别名> ON <条件>` → 得到**字段级**关联条件；
   · 抽取同一条 SQL 里共同出现的表对 → 表级勾稽边（这就是「模块 A 和模块 B 有没有往来」）。
3. 每条引用带上「文件 → 模块」归属，产出：
   · 模块 × 模块 访问矩阵
   · 表级勾稽边（带 ON 条件原文）
   · 孤岛候选，分两档：`dead`（代码零引用 + 无页面）/ `suspicious`（有代码用但无跨表同现）
4. 可选 `--db`：读外键与行数（**只读** SELECT）。

用法
----
    python3 scripts/verify/module_graph.py                    # 文本报告
    python3 scripts/verify/module_graph.py --db cloudpivot_test
    python3 scripts/verify/module_graph.py --format mermaid   # 画依赖图
    python3 scripts/verify/module_graph.py --format tables    # 表级明细（被谁引用/和谁同现）
    python3 scripts/verify/module_graph.py --format json
    python3 scripts/verify/module_graph.py --check            # 存在「死表」即退出码 1

输出稳定：表清单来自 migrations、边来自 SQL 字面量，改代码后重跑数字会跟着变。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

# ==========================================================================
# 一、归属规则：表 → 模块 / 子模块
# ==========================================================================
# 顺序敏感，长前缀在前。业务语义归属，不看建表语句写在哪个 migration 文件。
OWNER_RULES: list[tuple[str, str, str]] = [
    # (表名前缀, 一级模块, 二级子模块)
    ("biz_task_worklog", "task", "worklog"),
    ("biz_task_comment", "task", "comment"),
    ("biz_task_attachment", "task", "attachment"),
    ("biz_task_dependency", "task", "dependency"),
    ("biz_task_member", "task", "member"),
    ("biz_task_template", "task", "template"),
    ("biz_task_ai_token", "task", "ai"),
    ("biz_task", "task", "task"),
    ("biz_project_member", "project", "member"),
    ("biz_project", "project", "project"),
    ("biz_flow_", "flow", "flow"),
    ("sys_notice", "notice", "notice"),
    ("sys_announcement", "announcement", "announcement"),
    ("sys_help_", "help", "help"),
    ("sys_change_log", "changelog", "changelog"),
    ("sys_file", "storage", "file"),
    ("sys_storage_", "storage", "storage"),
    ("sys_import_task", "impexp", "import"),
    ("sys_export_task", "impexp", "export"),
    ("sys_ai_agent", "aiagent", "agent"),
    ("sys_field_mask", "masking", "masking"),
    ("sys_role_mutex", "mutex", "mutex"),
    ("sys_role_dept", "permission", "scope"),
    ("sys_user_preference", "sys", "preference"),
    ("sys_preference_def", "sys", "preference"),
    ("sys_user", "sys", "user"),
    ("sys_role", "sys", "role"),
    ("sys_menu", "sys", "menu"),
    ("sys_dept", "sys", "dept"),
    ("sys_dict_", "sys", "dict"),
    ("sys_token_blacklist", "sys", "auth"),
    ("sys_config", "support", "config"),
    ("sys_job", "support", "job"),
    # ---- ERP 二级子模块（顺序：具体在前）----
    ("biz_contract", "erp", "contract"),
    ("biz_purchase_order", "erp", "purchase"),
    ("biz_stock_in", "erp", "stockin"),
    ("biz_stock_transfer", "erp", "transfer"),
    ("biz_inventory_txn", "erp", "inventory"),
    ("biz_inventory_location", "erp", "inventory"),
    ("biz_inventory", "erp", "inventory"),
    ("biz_material_serial", "erp", "inventory"),
    ("biz_voucher_rule", "erp", "finance"),
    ("biz_voucher_entry", "erp", "finance"),
    ("biz_voucher", "erp", "finance"),
    ("biz_payable", "erp", "finance"),
    ("biz_payment", "erp", "finance"),
    ("biz_report_item", "erp", "report"),
    ("biz_cashflow_item", "erp", "report"),
    ("biz_opening_balance", "erp", "opening"),
    ("biz_opening_inventory", "erp", "opening"),
    ("biz_accounting_period", "erp", "period"),
    ("biz_seq", "erp", "period"),
    ("biz_status_log", "erp", "audit"),
    ("biz_supplier", "erp", "master"),
    ("biz_material", "erp", "master"),
    ("biz_warehouse", "erp", "master"),
    ("biz_location", "erp", "master"),
    ("biz_unit", "erp", "master"),
    ("biz_account", "erp", "master"),
]

EXTRA_OWNER: dict[str, tuple[str, str]] = {
    "operation_logs": ("support", "log"),
    "audit_logs": ("support", "log"),
    "auth_logs": ("support", "log"),
    "security_logs": ("support", "log"),
    "password_change_logs": ("support", "log"),
    "schema_migrations": ("_framework", "migration"),
}

MODULE_CN: dict[str, str] = {
    "task": "工作任务",
    "project": "项目",
    "flow": "审批流",
    "erp": "ERP",
    "notice": "消息通知",
    "announcement": "公告",
    "help": "帮助中心",
    "changelog": "变更日志",
    "storage": "文件存储",
    "impexp": "导入导出",
    "aiagent": "AI 用户",
    "masking": "字段脱敏",
    "mutex": "角色互斥",
    "permission": "数据权限",
    "sys": "系统管理",
    "support": "支撑底座",
    "shared": "数据权限基座",
    "middleware": "中间件",
    "monitor": "服务器监控",
    "_framework": "框架自建",
}

SUBMODULE_CN: dict[str, str] = {
    "erp/master": "ERP·主数据",
    "erp/contract": "ERP·合同",
    "erp/purchase": "ERP·采购",
    "erp/stockin": "ERP·收货",
    "erp/inventory": "ERP·库存",
    "erp/transfer": "ERP·调拨",
    "erp/finance": "ERP·财务",
    "erp/report": "ERP·报表",
    "erp/period": "ERP·期间",
    "erp/opening": "ERP·期初",
    "erp/audit": "ERP·状态流水",
}

BACKEND_SCAN_DIRS = [
    "backend/src/business",
    "backend/src/system",
    "backend/src/support",
    "backend/src/middleware",
    "backend/src/task",
    "backend/src/shared",
]
BACKEND_ROOT_FILES = ["backend/src/bootstrap.rs", "backend/src/lib.rs"]
FRONTEND_SCAN_DIRS = ["frontend/src/api", "frontend/src/views"]

CREATE_TABLE_RE = re.compile(
    r"CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:public\.)?([a-z][a-z0-9_]*)", re.IGNORECASE
)
# Rust 字符串字面量（允许跨行与 \" 转义）；只取不含未转义引号的朴素字面量
RUST_STR_RE = re.compile(r'"((?:[^"\\]|\\.)*)"', re.DOTALL)
SQL_HINT_RE = re.compile(r"\b(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM)\b", re.IGNORECASE)
FROM_JOIN_RE = re.compile(
    r"\b(FROM|JOIN|INSERT\s+INTO|UPDATE)\s+((?:public\.)?[a-z][a-z0-9_]*)(?:\s+(?:AS\s+)?([a-z][a-z0-9_]*))?",
    re.IGNORECASE,
)
JOIN_ON_RE = re.compile(
    r"JOIN\s+(?:public\.)?([a-z][a-z0-9_]*)\s+(?:AS\s+)?([a-z][a-z0-9_]*)?\s+ON\s+([^\n]+)",
    re.IGNORECASE,
)
# 表名常量化：`const ITEM_TABLE: &str = "biz_report_item"`（配置驱动模块的普遍写法）
CONST_TABLE_RE = re.compile(
    r'const\s+([A-Z][A-Z0-9_]*)\s*:\s*&str\s*=\s*"([a-z][a-z0-9_]*)"'
)
# ORM 模型声明的表名（只用 Model API、不写裸 SQL 的表）
ORM_TABLE_RE = re.compile(r'#\[table\(\s*"([a-z][a-z0-9_]*)"\s*\)\]')
# 正则回溯时会命中的 SQL 关键字（防止把 `CREATE TABLE IF NOT EXISTS %I …` 解析成表名 `if`）
RESERVED_WORDS = {"if", "not", "exists", "public", "only", "table"}


def owner_of(table: str) -> tuple[str, str]:
    if table in EXTRA_OWNER:
        return EXTRA_OWNER[table]
    for prefix, mod, sub in OWNER_RULES:
        if table.startswith(prefix):
            return mod, sub
    return "_unknown", "_unknown"


def module_of_path(rel: str) -> str:
    parts = rel.split("/")
    if len(parts) >= 4 and parts[2] == "business":
        return parts[3]
    if len(parts) >= 4 and parts[2] == "system":
        return "sys"
    if len(parts) >= 4 and parts[2] == "support":
        return "support"
    if len(parts) >= 3 and parts[2] in ("middleware", "task", "shared"):
        return parts[2]
    return "_root"


# ==========================================================================
# 二、数据结构
# ==========================================================================
@dataclass
class TableRef:
    table: str
    kind: str            # FROM / JOIN / INSERT / UPDATE
    module: str
    file: str
    line: int
    alias: str | None = None


@dataclass
class JoinEdge:
    left_hint: str       # 同一 SQL 里出现过的其它表（逗号连接）
    right: str           # 被 JOIN 的表
    cond: str            # ON 条件原文（已压空白）
    module: str
    file: str
    line: int


@dataclass
class Scan:
    tables: list[str] = field(default_factory=list)
    refs: list[TableRef] = field(default_factory=list)
    joins: list[JoinEdge] = field(default_factory=list)
    cooccur: Counter = field(default_factory=Counter)      # (a,b) 排序后的表对
    cooccur_sites: dict = field(default_factory=lambda: defaultdict(list))
    frontend_mentions: Counter = field(default_factory=Counter)
    files: int = 0
    lines: int = 0
    sql_literals: int = 0
    const_resolved: int = 0                              # 通过 const 表名还原出的引用数
    orm_refs: int = 0                                    # `#[table(...)]` ORM 模型声明数


# ==========================================================================
# 三、扫描实现
# ==========================================================================
def discover_tables(repo: Path) -> list[str]:
    tables: set[str] = set()
    for p in (repo / "migrations").rglob("*.up.sql") if (repo / "migrations").exists() else (repo / "backend" / "migrations").rglob("*.up.sql"):
        text = p.read_text(encoding="utf-8", errors="ignore")
        # 先剥 `--` 行注释：注释里出现的 `CREATE TABLE biz_[a-z_]+` 之类示例会造出幽灵表（实测 biz_）
        text = re.sub(r"--[^\n]*", "", text)
        for m in CREATE_TABLE_RE.finditer(text):
            name = m.group(1).lower()
            # 跳过 SQL 关键字与动态命名占位（`format('CREATE TABLE IF NOT EXISTS %I PARTITION OF …')`
            # 会让正则在回溯时把 `IF` 当成表名，实测造出幽灵表 `if`）
            if name in RESERVED_WORDS or name.endswith("_"):
                continue
            tables.add(name)
    return sorted(tables)


def iter_backend_files(repo: Path):
    for d in BACKEND_SCAN_DIRS:
        base = repo / d
        if base.exists():
            for p in sorted(base.rglob("*.rs")):
                yield p, str(p.relative_to(repo))
    for f in BACKEND_ROOT_FILES:
        p = repo / f
        if p.exists():
            yield p, f


def scan_backend(repo: Path, scan: Scan) -> None:
    valid = set(scan.tables)
    for path, rel in iter_backend_files(repo):
        mod = module_of_path(rel)
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        scan.files += 1
        scan.lines += text.count("\n") + 1

        # 表名常量化（`const ITEM_TABLE: &str = "biz_report_item"` + `format!("... FROM {ITEM_TABLE}")`）
        # 不处理的话，配置驱动型模块会被误判成孤岛（实测 report_item / cashflow_item 就是这种写法）
        const_tables = {
            name: tbl
            for name, tbl in CONST_TABLE_RE.findall(text)
            if tbl in valid
        }

        # ORM 模型声明：`#[table("security_logs")]` —— 这类表只走 Model API，SQL 字面量里看不到，
        # 不统计会全部误判成「死表」（实测 security_logs / password_change_logs 就是这样）
        for om in ORM_TABLE_RE.finditer(text):
            tbl = om.group(1).lower()
            if tbl not in valid:
                continue
            line = text.count("\n", 0, om.start()) + 1
            scan.refs.append(TableRef(tbl, "ORM", mod, rel, line, None))
            scan.orm_refs += 1

        for m in RUST_STR_RE.finditer(text):
            literal = m.group(1)
            if len(literal) < 12 or not SQL_HINT_RE.search(literal):
                continue
            scan.sql_literals += 1
            base_line = text.count("\n", 0, m.start()) + 1

            resolved = literal
            for name, tbl in const_tables.items():
                token = "{" + name + "}"
                if token in resolved:
                    resolved = resolved.replace(token, tbl)
                    scan.const_resolved += 1

            present: list[str] = []
            for fm in FROM_JOIN_RE.finditer(resolved):
                kw = fm.group(1).upper().replace("  ", " ")
                tbl = fm.group(2).lower().removeprefix("public.")
                alias = fm.group(3)
                if tbl not in valid:
                    continue
                kind = "JOIN" if kw == "JOIN" else ("INSERT" if kw.startswith("INSERT") else kw)
                line = base_line + literal.count("\n", 0, fm.start()) 
                scan.refs.append(TableRef(tbl, kind, mod, rel, line, alias))
                present.append(tbl)

            # 同一 SQL 内的表对（去重后两两组合）
            uniq = sorted(set(present))
            for i in range(len(uniq)):
                for j in range(i + 1, len(uniq)):
                    key = (uniq[i], uniq[j])
                    scan.cooccur[key] += 1
                    scan.cooccur_sites[key].append(f"{rel}:{base_line}")

            for jm in JOIN_ON_RE.finditer(resolved):
                tbl = jm.group(1).lower().removeprefix("public.")
                if tbl not in valid:
                    continue
                cond = re.sub(r"\s+", " ", jm.group(3)).strip().rstrip("\\").strip()
                left = [t for t in sorted(set(present)) if t != tbl]
                scan.joins.append(
                    JoinEdge(
                        left_hint="+".join(left[:3]) or "?",
                        right=tbl,
                        cond=cond[:160],
                        module=mod,
                        file=rel,
                        line=base_line + literal.count("\n", 0, jm.start()),
                    )
                )


def scan_frontend(repo: Path, scan: Scan) -> None:
    """前端只用来判断「这张表对应的功能有没有页面」，不算数据访问边。"""
    valid = set(scan.tables)
    pat = re.compile(r"\b(" + "|".join(re.escape(t) for t in valid) + r")\b") if valid else None
    if not pat:
        return
    for d in FRONTEND_SCAN_DIRS:
        base = repo / d
        if not base.exists():
            continue
        for p in sorted(base.rglob("*")):
            if p.suffix not in (".ts", ".vue", ".js"):
                continue
            try:
                text = p.read_text(encoding="utf-8", errors="ignore")
            except OSError:
                continue
            scan.files += 1
            scan.lines += text.count("\n") + 1
            for m in pat.finditer(text):
                scan.frontend_mentions[m.group(1)] += 1


def psql(db: str, sql: str) -> list[list[str]]:
    env = dict(os.environ)
    env.setdefault("PGPASSWORD", os.environ.get("NIK_DATABASE_PASSWORD", "alun"))
    cmd = [
        "psql",
        "-h", os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", os.environ.get("NIK_DATABASE_PORT", "5432"),
        "-U", os.environ.get("NIK_DATABASE_USER", "alun"),
        "-d", db, "-Atc", sql, "-F", "|",
    ]
    out = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip())
    return [line.split("|") for line in out.stdout.strip().splitlines() if line]


def load_db_facts(db: str) -> dict:
    fks = psql(
        db,
        """
        SELECT tc.table_name, kcu.column_name, ccu.table_name, ccu.column_name, rc.delete_rule
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema
        JOIN information_schema.constraint_column_usage ccu
          ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema
        JOIN information_schema.referential_constraints rc
          ON rc.constraint_name = tc.constraint_name
        WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = 'public'
        ORDER BY 1, 2
        """,
    )
    rows = {r[0]: int(r[1]) for r in psql(db, "SELECT relname, n_live_tup FROM pg_stat_user_tables")}
    cols: dict[str, list[str]] = defaultdict(list)
    for r in psql(
        db,
        "SELECT table_name, column_name FROM information_schema.columns "
        "WHERE table_schema='public' ORDER BY table_name, ordinal_position",
    ):
        cols[r[0]].append(r[1])
    return {"fks": fks, "rows": rows, "cols": dict(cols)}


# ==========================================================================
# 四、分析
# ==========================================================================
def analyse(scan: Scan, db: dict | None) -> dict:
    own: dict[str, list[str]] = defaultdict(list)          # module -> tables
    sub_own: dict[str, list[str]] = defaultdict(list)      # "mod/sub" -> tables
    for t in scan.tables:
        m, s = owner_of(t)
        own[m].append(t)
        sub_own[f"{m}/{s}"].append(t)

    # 每张表的引用：谁在 SQL 里碰过它
    by_table: dict[str, Counter] = defaultdict(Counter)
    kind_by_table: dict[str, Counter] = defaultdict(Counter)
    for r in scan.refs:
        by_table[r.table][r.module] += 1
        kind_by_table[r.table][r.kind] += 1

    # 模块矩阵（消费者模块 → 生产者模块），以文件模块为消费者
    matrix: dict[tuple[str, str], dict] = {}
    for r in scan.refs:
        owner_mod, _ = owner_of(r.table)
        if r.module == owner_mod or r.module.startswith("_"):
            continue
        slot = matrix.setdefault((r.module, owner_mod), {"refs": 0, "tables": Counter()})
        slot["refs"] += 1
        slot["tables"][r.table] += 1

    # 子模块矩阵（以表所属子模块为单位）
    sub_matrix: dict[tuple[str, str], dict] = {}
    for (a, b), n in scan.cooccur.items():
        sa = "/".join(owner_of(a))
        sb = "/".join(owner_of(b))
        if sa == sb:
            continue
        key = (sa, sb) if sa < sb else (sb, sa)
        slot = sub_matrix.setdefault(key, {"pairs": 0, "detail": Counter()})
        slot["pairs"] += n
        slot["detail"][f"{a}~{b}"] += n

    fk_in: Counter = Counter()
    fk_out: Counter = Counter()
    if db:
        for child, _c, parent, _p, _rule in db["fks"]:
            fk_out[child] += 1
            fk_in[parent] += 1

    # 孤岛判定
    # 注意：本扫描只能看到「同一 SQL 里同现」；配置驱动型模块（如报表项→科目映射）是
    # 「分别读表 + 应用层组装」，物理上没有 JOIN，不能因此判死刑 → 分两档：
    #   dead       = 代码零引用且无页面（真该删/该合）
    #   suspicious = 有代码用，但没有跨表同现（需人工定性：孤岛 / 合理独立 / 断链）
    islands = []
    for t in scan.tables:
        mod, sub = owner_of(t)
        if mod.startswith("_"):
            continue
        external_mods = {m: n for m, n in by_table.get(t, Counter()).items() if m != mod}
        co = [k for k in scan.cooccur if t in k]
        fk = fk_in[t] + fk_out[t] if db else 0
        self_refs = by_table.get(t, Counter()).get(mod, 0)
        if not co and not external_mods and fk == 0:
            if self_refs == 0 and scan.frontend_mentions.get(t, 0) == 0:
                verdict, severity = "死表（代码零引用、无页面）", "dead"
            else:
                verdict, severity = "无跨表同现（只在本模块被单独读取，关联在应用层完成）", "suspicious"
        elif not co and not external_mods:
            verdict, severity = "自产自销（只有外键、没有跨模块 SQL 关联）", "suspicious"
        elif not co:
            verdict, severity = "只被跨模块裸引用（无 JOIN、无同现）", "suspicious"
        else:
            continue
        islands.append(
            {
                "table": t,
                "module": mod,
                "sub": sub,
                "verdict": verdict,
                "severity": severity,
                "self_refs": self_refs,
                "ext_refs": sum(external_mods.values()),
                "cooccur_edges": len(co),
                "fk_in": fk_in[t] if db else None,
                "fk_out": fk_out[t] if db else None,
                "rows": db["rows"].get(t) if db else None,
                "cols": db["cols"].get(t, []) if db else [],
                "frontend": scan.frontend_mentions.get(t, 0),
            }
        )

    dead = [t for t in scan.tables if not by_table.get(t) and not scan.frontend_mentions.get(t)]

    # 表级明细（供 --format tables 与文档生成；也把「谁和我同现」按对聚合）
    co_with: dict[str, Counter] = defaultdict(Counter)
    for (x, y), n in scan.cooccur.items():
        co_with[x][y] += n
        co_with[y][x] += n
    table_detail = []
    for t in scan.tables:
        mod, sub = owner_of(t)
        refs_by_mod = by_table.get(t, Counter())
        table_detail.append(
            {
                "table": t,
                "module": mod,
                "sub": sub,
                "self_refs": refs_by_mod.get(mod, 0),
                "ext_refs": sum(n for m, n in refs_by_mod.items() if m != mod),
                "ref_by": {m: n for m, n in refs_by_mod.items()},
                "kinds": dict(kind_by_table.get(t, Counter())),
                "cooccur_with": dict(co_with.get(t, Counter()).most_common()),
                "fk_in": fk_in[t] if db else None,
                "fk_out": fk_out[t] if db else None,
                "rows": db["rows"].get(t) if db else None,
                "cols": db["cols"].get(t, []) if db else [],
                "frontend": scan.frontend_mentions.get(t, 0),
            }
        )
    table_detail.sort(key=lambda x: (x["module"], x["sub"], -x["self_refs"], x["table"]))

    return {
        "own": {k: sorted(v) for k, v in own.items()},
        "sub_own": {k: sorted(v) for k, v in sub_own.items()},
        "by_table": by_table,
        "kind_by_table": kind_by_table,
        "matrix": matrix,
        "sub_matrix": sub_matrix,
        "islands": sorted(islands, key=lambda x: (x["module"], x["sub"], x["table"])),
        "dead": dead,
        "fk_in": fk_in,
        "fk_out": fk_out,
        "table_detail": table_detail,
    }


# ==========================================================================
# 五、输出
# ==========================================================================
def fmt_text(scan: Scan, a: dict, db: dict | None) -> str:
    L: list[str] = []
    w = L.append
    w("=" * 84)
    w("模块级勾稽关系扫描 · module_graph.py（只读静态扫描 + 可选 DB 事实）")
    w("=" * 84)
    w(f"扫描文件 {scan.files} 个 / {scan.lines} 行；含 SQL 的字符串字面量 {scan.sql_literals} 条")
    w(f"表 {len(scan.tables)} 张；一级模块 {len([m for m in a['own'] if not m.startswith('_')])} 个；"
      f"二级子模块 {len([s for s in a['sub_own'] if not s.startswith('_')])} 个")
    w("")

    w("─" * 84)
    w("[1] 模块 × 表：自引用 vs 跨模块引用")
    w("─" * 84)
    w(f"{'模块':<12}{'表数':>5}{'本模块 SQL 引用':>16}{'跨模块 SQL 引用':>16}{'被 JOIN 次数':>14}")
    for m in sorted(a["own"]):
        if m.startswith("_"):
            continue
        ts = a["own"][m]
        self_refs = ext_refs = joins = 0
        for t in ts:
            self_refs += a["by_table"].get(t, Counter()).get(m, 0)
            ext_refs += sum(n for k, n in a["by_table"].get(t, Counter()).items() if k != m)
            joins += a["kind_by_table"].get(t, Counter()).get("JOIN", 0)
        w(f"{m:<12}{len(ts):>5}{self_refs:>16}{ext_refs:>16}{joins:>14}")
    w("")

    w("─" * 84)
    w("[2] 一级模块访问矩阵（消费方 → 生产方，值=SQL 引用数）")
    w("─" * 84)
    for (c, p), info in sorted(a["matrix"].items(), key=lambda kv: -kv[1]["refs"]):
        tbls = ", ".join(f"{t}×{n}" for t, n in info["tables"].most_common(6))
        w(f"{c:<12} -> {p:<12} {info['refs']:>4} 处  {tbls}")
    w("")

    w("─" * 84)
    w("[3] 二级子模块勾稽强度（同一 SQL 里共同出现的表对数量）")
    w("─" * 84)
    for (x, y), info in sorted(a["sub_matrix"].items(), key=lambda kv: -kv[1]["pairs"]):
        detail = ", ".join(f"{k}×{v}" for k, v in info["detail"].most_common(4))
        w(f"{x:<18} <-> {y:<18} {info['pairs']:>4}  {detail}")
    w("")

    w("─" * 84)
    w("[4] 表级勾稽边（JOIN ... ON 原文，最多列 60 条）")
    w("─" * 84)
    seen = set()
    n = 0
    for e in scan.joins:
        key = (e.right, e.cond[:60])
        if key in seen:
            continue
        seen.add(key)
        n += 1
        if n > 60:
            break
        w(f"JOIN {e.right:<28} ON {e.cond}")
        w(f"    位于 {e.module:<10} {e.file}:{e.line}")
    w("")

    w("─" * 84)
    w("[5] 孤岛候选（无同现、无跨模块引用、无外键）")
    w("─" * 84)
    if not a["islands"]:
        w("（无）")
    for i in a["islands"]:
        w(
            f"{i['module']:<10}/{i['sub']:<12}{i['table']:<30} self={i['self_refs']:<4}"
            f" ext={i['ext_refs']:<3} fk_in={i['fk_in']} fk_out={i['fk_out']} "
            f"rows={i['rows']} fe={i['frontend']}"
        )
        w(f"      判决依据：{i['verdict']}")
        if i["cols"]:
            cols = [c for c in i["cols"] if c.endswith("_id") or c in ("biz_type", "biz_id", "source_type", "source_id")]
            if cols:
                w(f"      可疑关联列（未建外键）：{', '.join(cols)}")
    w("")

    if db:
        w("─" * 84)
        w("[6] 外键密度：ERP 表 0 外键清单（数据层无约束，全靠代码自律）")
        w("─" * 84)
        erp_tables = a["own"].get("erp", [])
        no_fk = [t for t in erp_tables if a["fk_in"][t] == 0 and a["fk_out"][t] == 0]
        w(f"ERP 表 {len(erp_tables)} 张，其中 0 外键 {len(no_fk)} 张：")
        for t in no_fk:
            w(f"  - {t:<30} rows={db['rows'].get(t, '-')}")
        w("")
        w(f"全库外键 {len(db['fks'])} 条，入边最多的表：")
        for t, k in a["fk_in"].most_common(12):
            w(f"  {t:<30} in={k}")
    return "\n".join(L)


def fmt_tables(scan: Scan, a: dict) -> str:
    """表级明细：每张表被谁引用、和谁同现、外键/行数/页面。给审计文档当证据表用。"""
    L: list[str] = []
    w = L.append
    w(f"{'表':<30}{'模块/子模块':<26}{'自引':>5}{'跨模块':>7}{'FK入':>5}{'FK出':>5}{'行数':>8}{'前端':>5}")
    for d in a["table_detail"]:
        fk_in = "-" if d["fk_in"] is None else d["fk_in"]
        fk_out = "-" if d["fk_out"] is None else d["fk_out"]
        rows = "-" if d["rows"] is None else d["rows"]
        w(f"{d['table']:<30}{d['module'] + '/' + d['sub']:<26}{d['self_refs']:>5}{d['ext_refs']:>7}"
          f"{str(fk_in):>5}{str(fk_out):>5}{str(rows):>8}{d['frontend']:>5}")
        co = ", ".join(f"{t}×{n}" for t, n in list(d["cooccur_with"].items())[:6])
        w(f"    同现：{co or '（无）'}")
        w(f"    引用方：{', '.join(f'{m}×{n}' for m, n in sorted(d['ref_by'].items(), key=lambda kv: -kv[1])) or '（无）'}")
    return "\n".join(L)


def fmt_mermaid(scan: Scan, a: dict) -> str:
    L = ["```mermaid", "flowchart LR"]
    for m in sorted(a["own"]):
        if m.startswith("_"):
            continue
        L.append(f'  {m}["{MODULE_CN.get(m, m)}<br/>{len(a["own"][m])} 表"]')
    for (c, p), info in sorted(a["matrix"].items(), key=lambda kv: -kv[1]["refs"]):
        if info["refs"] < 2:
            continue
        style = "" if info["refs"] >= 10 else ":::weak"
        L.append(f"  {c} -->|\"{info['refs']}\"| {p}{style}")
    L.append("  classDef weak stroke-dasharray: 4 4;")
    L.append("```")
    return "\n".join(L)


def main() -> int:
    ap = argparse.ArgumentParser(description="模块级勾稽关系扫描（只读）")
    default_repo = Path(__file__).resolve().parent.parent.parent
    ap.add_argument("--repo", default=str(default_repo))
    ap.add_argument("--format", choices=["text", "mermaid", "json", "tables"], default="text")
    ap.add_argument("--db", default=None, help="库名；给出时额外读外键/行数/列（只读 SELECT）")
    ap.add_argument("--check", action="store_true", help="存在真孤岛时退出码 1")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    scan = Scan(tables=discover_tables(repo))
    scan_frontend(repo, scan)   # 先扫前端，为「有没有页面」提供口径
    scan_backend(repo, scan)

    db = None
    if args.db:
        try:
            db = load_db_facts(args.db)
        except Exception as e:  # noqa: BLE001
            print(f"[warn] 数据库不可用，跳过 FK/行数：{e}", file=sys.stderr)

    a = analyse(scan, db)

    if args.format == "text":
        print(fmt_text(scan, a, db))
    elif args.format == "mermaid":
        print(fmt_mermaid(scan, a))
    elif args.format == "tables":
        print(fmt_tables(scan, a))
    else:
        print(
            json.dumps(
                {
                    "modules": a["own"],
                    "sub_modules": a["sub_own"],
                    "matrix": [
                        {"consumer": c, "producer": p, "refs": v["refs"], "tables": dict(v["tables"])}
                        for (c, p), v in sorted(a["matrix"].items())
                    ],
                    "sub_matrix": [
                        {"a": x, "b": y, "pairs": v["pairs"], "detail": dict(v["detail"])}
                        for (x, y), v in sorted(a["sub_matrix"].items())
                    ],
                    "islands": a["islands"],
                    "dead_tables": a["dead"],
                    "table_detail": a["table_detail"],
                    "stats": {
                        "files": scan.files,
                        "lines": scan.lines,
                        "sql_literals": scan.sql_literals,
                        "tables": len(scan.tables),
                        "const_resolved": scan.const_resolved,
                    },
                },
                ensure_ascii=False,
                indent=2,
            )
        )

    if args.check:
        return 1 if any(i.get("severity") == "dead" for i in a["islands"]) else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
