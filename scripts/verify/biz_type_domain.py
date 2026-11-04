#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
业务类型词表值域巡检（P2-5 / ARCH-3 §5 DC-21）
================================================
只读巡检：把「所有业务类型列」的**实际取值**与词表域对一遍，域外值即 FAIL。

**唯一真相不是本脚本**：值域清单从
`backend/src/business/shared/doc_kind.rs` 里解析 `COLUMN_DOMAINS` + 各 `*_VALUES`
常量（那才是唯一真相）。本脚本只负责「拿着词表去查库」——
**绝不在这里再抄一份白名单**，抄一份就是造第 9 套词表。

用法：
    python3 scripts/verify/biz_type_domain.py                # 全量巡检
    python3 scripts/verify/biz_type_domain.py --json         # JSON（给监控 / CI）
    python3 scripts/verify/biz_type_domain.py --list-domains  # 只打印词表域

连接口径（与 erp_invariants.py 一致）：
    NIK_DATABASE_HOST / NIK_DATABASE_PORT / NIK_DATABASE_USER /
    NIK_DATABASE_NAME / NIK_DATABASE_PASSWORD

退出码：0 = 所有列的值都在域内；1 = 有域外值（列名 / 值 / 行数都会打印）；2 = 环境错误。

设计口径：
  1. 每列独立一条，命中就报「表.列 = 值 × 行数」，不合并（合并不给定位等于没查）；
  2. **软删行不参与判定**（表有 `deleted_at` 就只看 `deleted_at IS NULL`）：
     历史/测试残留不是「在用的数据」，把它们算进来只会让巡检常年红；
  3. **e2e 测试残留单列不判红**：值以 `E2E_` 开头（或 `D2_SMOKE`）的，来自别的卡
     把 e2e 脚本跑在了这个库上 —— 输出里逐个点名 + 计数，但不计红；
     要按严格口径判红加 `--strict`（CI 建议用 `--strict`，人工排查用默认口径）；
  4. NULL 单独报（业务类型列都该有值；`biz_material_serial.source_type` 允许空，
     在词表里登记为 nullable 的不算）；
  5. 只读：脚本里不出现 INSERT / UPDATE / DELETE（`--self-check` 会自证这一点）。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VOCAB = ROOT / "backend/src/business/shared/doc_kind.rs"

# 允许为空的列（其余列出现 NULL 也报警）
#
# `biz_fixed_asset.disposal_type` 是**业务上就该为空**的列：未处置的资产没有处置类型
# （DDL 的 `ck_biz_fa_disposal` 也只要求「处置类型 ∈ SALE/SCRAP/LOSS 或 NULL」）。
# 登记进来不是放宽校验：一旦有值，仍然必须落在三个取值里。
NULLABLE = {("biz_material_serial", "source_type"), ("biz_fixed_asset", "disposal_type")}

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    db=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)

CONST_RE = re.compile(r"const\s+([A-Z_]+)\s*:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];", re.S)
DOMAIN_RE = re.compile(
    r"ColumnDomain\s*\{\s*table:\s*\"([^\"]+)\",\s*column:\s*\"([^\"]+)\","
    r"\s*values:\s*&([A-Z_]+),\s*axis:\s*\"([^\"]+)\",\s*\}",
    re.S,
)
STR_RE = re.compile(r"\"([^\"]*)\"")


def load_domains(path: Path) -> list[dict]:
    """从 doc_kind.rs 解析出 [(table, column, values, axis)]。"""
    text = path.read_text(encoding="utf-8")
    consts: dict[str, list[str]] = {}
    for name, body in CONST_RE.findall(text):
        consts[name] = STR_RE.findall(body)
    domains = []
    for table, column, const, axis in DOMAIN_RE.findall(text):
        if const not in consts:
            raise SystemExit(f"doc_kind.rs 里 {const} 没有定义（词表解析失败）")
        domains.append(
            dict(table=table, column=column, values=consts[const], axis=axis)
        )
    if not domains:
        raise SystemExit("doc_kind.rs 里没解析到 COLUMN_DOMAINS（词表解析失败）")
    return domains


def psql(sql: str) -> list[list[str]]:
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    proc = subprocess.run(
        [
            "psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
            "-d", DB["db"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql,
        ],
        capture_output=True, text=True, env=env, timeout=120,
    )
    if proc.returncode != 0:
        raise SystemExit(f"psql 失败：{proc.stderr.strip()[:400]}")
    rows = []
    for line in proc.stdout.splitlines():
        if line == "":
            continue
        rows.append(line.split("\x1f"))
    return rows


def soft_delete_tables() -> set[str]:
    """有 `deleted_at` 的表（软删行不参与值域判定 —— 那是历史/测试残留，不是在用数据）。"""
    rows = psql(
        "SELECT table_name FROM information_schema.columns "
        "WHERE table_schema='public' AND column_name='deleted_at'"
    )
    return {row[0] for row in rows if row}


def is_test_residue(value: str) -> bool:
    """e2e 跑在生产库留下的类型名（`E2E_*` 前缀 / `D2_SMOKE`）。

    ⚠️ 这不是「业务值的兼容层」：它只用来把**别人 e2e 脚本写进生产库的残留**
    单列出来（输出里逐个点名 + 计数，不静默通过），让词表巡检聚焦业务数据。
    真要在这种库上判红，跑 `--strict`。
    """
    return value.startswith("E2E_") or value == "D2_SMOKE"


def audit(domains: list[dict]) -> list[dict]:
    have_del = soft_delete_tables()
    results = []
    for item in domains:
        table, column = item["table"], item["column"]
        allowed = set(item["values"])
        where = " WHERE deleted_at IS NULL" if table in have_del else ""
        rows = psql(
            f"SELECT COALESCE({column}::text, '<NULL>') AS v, count(*) "
            f"FROM {table}{where} GROUP BY 1 ORDER BY 2 DESC"
        )
        seen = {row[0]: int(row[1]) for row in rows if len(row) >= 2}
        bad = {v: n for v, n in seen.items() if v not in allowed}
        residue = {v: n for v, n in bad.items() if is_test_residue(v)}
        for value in residue:
            bad.pop(value, None)
        if (table, column) in NULLABLE:
            bad.pop("<NULL>", None)
        results.append(
            dict(
                table=table,
                column=column,
                axis=item["axis"],
                allowed=item["values"],
                seen=seen,
                out_of_domain=bad,
                residue=residue,
                soft_deleted_ignored=table in have_del,
                null_count=seen.get("<NULL>", 0),
                null_allowed=(table, column) in NULLABLE,
                ok=not bad,
            )
        )
    return results


def _w(text: str) -> int:
    """显示宽度（全角 / 宽字符占 2 列，用来对齐巡检输出）。"""
    return sum(2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1 for ch in text)


def _pad(text: str, width: int) -> str:
    return text + " " * max(0, width - _w(text))


def render(results: list[dict], strict: bool) -> int:
    if strict:
        for item in results:
            if item["residue"]:
                item["out_of_domain"].update(item["residue"])
                item["ok"] = False
    failed = [item for item in results if not item["ok"]]
    total_rows = sum(sum(item["seen"].values()) for item in results)
    residue_total = sum(sum(item["residue"].values()) for item in results)
    print("=" * 78)
    print(f"业务类型词表值域巡检 —— {len(results)} 列 / {total_rows} 行"
          f"（库 {DB['user']}@{DB['host']}:{DB['port']}/{DB['db']}）")
    print("=" * 78)
    for item in results:
        mark = "PASS" if item["ok"] else "FAIL"
        distinct = len(item["seen"]) - (1 if item["null_count"] else 0)
        head = f"[{mark}] {_pad(item['axis'], 16)}"
        head += _pad(f"{item['table']}.{item['column']}", 40)
        print(f"{head} 取值 {distinct} 种")
        if item["null_count"] and not item["null_allowed"]:
            print(f"       ⚠ NULL × {item['null_count']}（该列不该为空）")
        for value, count in sorted(item["residue"].items(), key=lambda kv: -kv[1]):
            label = "域外值（e2e 测试残留）" if not strict else "域外值"
            print(f"       ⚠ {label} {value!r} × {count}")
        for value, count in sorted(item["out_of_domain"].items(), key=lambda kv: -kv[1]):
            print(f"       ✗ 域外值 {value!r} × {count}")
    print("-" * 78)
    if residue_total and not strict:
        print(f"注：{residue_total} 行是 e2e 脚本写进本库的测试残留（E2E_*/D2_SMOKE），"
              f"已单列不计红；要判红请加 --strict")
    if failed:
        print(f"结果：FAIL（{len(failed)} 列有域外值）")
        return 1
    print("结果：PASS（所有列的值都在 doc_kind.rs 的词表域内）")
    return 0


def self_check() -> int:
    """自证只读：用 AST 找出所有「看起来像 SQL」的字符串，要求它们**都以 SELECT 开头**。

    不按关键字搜全文（文档里举例写 `UPDATE` 也算命中，会误报）；
    也不信任注释 —— 直接看真正会传进 `psql()` 的字面量。
    """
    import ast

    src = Path(__file__).read_text(encoding="utf-8")
    tree = ast.parse(src)
    docstrings = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            doc = ast.get_docstring(node, clean=False)
            if doc is not None:
                docstrings.add(doc)

    verbs = ("SELECT", "INSERT", "UPDATE", "DELETE", "ALTER", "CREATE", "DROP", "TRUNCATE")
    offenders = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            text = node.value
            if text in docstrings:
                continue
            # 单词（如 verbs 元组里的 'INSERT'）不是 SQL，跳过
            if " " not in text.strip():
                continue
            head = text.lstrip().upper()
            if head.startswith(verbs) and not head.startswith("SELECT"):
                offenders.append(text.strip().splitlines()[0][:70])
    if offenders:
        print(f"FAIL：巡检脚本里有非 SELECT 的 SQL：{offenders}")
        return 1
    print("PASS：巡检脚本只读（所有 SQL 字面量都以 SELECT 开头，无 INSERT/UPDATE/DELETE/DDL）")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="业务类型词表值域巡检（只读）")
    parser.add_argument("--json", action="store_true", help="JSON 输出")
    parser.add_argument("--list-domains", action="store_true", help="只打印词表域")
    parser.add_argument("--self-check", action="store_true", help="自证只读")
    parser.add_argument("--strict", action="store_true",
                        help="把 e2e 测试残留（E2E_*/D2_SMOKE）也算域外值判红")
    args = parser.parse_args()

    if not VOCAB.exists():
        print(f"找不到词表源文件：{VOCAB}")
        return 2
    domains = load_domains(VOCAB)

    if args.self_check:
        return self_check()
    if args.list_domains:
        for item in domains:
            print(f"{item['axis']}  {item['table']}.{item['column']} = "
                  f"{item['values']}")
        return 0

    results = audit(domains)
    if args.json:
        print(json.dumps(results, ensure_ascii=False, indent=2))
        if args.strict:
            return 0 if all(item["ok"] and not item["residue"] for item in results) else 1
        return 0 if all(item["ok"] for item in results) else 1
    return render(results, args.strict)


if __name__ == "__main__":
    sys.exit(main())
