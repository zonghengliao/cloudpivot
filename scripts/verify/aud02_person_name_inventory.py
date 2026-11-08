#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""AUD-02 全站「人员字段只存 id / 界面显示 UUID」盘点 + 回归门禁

两种用法：

  ① 盘点（给人看，产出证据表）
     python3 scripts/verify/aud02_person_name_inventory.py
         → 打印 markdown 表：文件 · 结构体 · 缺名字的人字段 · 分类
         → 同时写 docs/evidence/20261109-aud02-uuid-audit/inventory.md

  ② 门禁（给 CI / 提交前用）
     python3 scripts/verify/aud02_person_name_inventory.py --check
         → 断言 REQUIRED 里每个「出参 DTO 的人字段」都有配对的 *_name
         → 断言 BAD_PATTERNS 里的「把 id 直接渲染到界面」的写法一个都不剩
         → 退出码 0 = 通过，1 = 有回归

# 为什么要有分类，而不是「所有 *_id 都必须有 *_name」

后端有两类结构体长得一样、语义不一样（`AGENTS.md` 约定 13）：

  * **出参 Res / 列表行**：会序列化给前端 → 人字段**必须**带 `*_name`；
  * **库行 Row / 入参 Req / 筛选 Filter**：不上界面（Row 只做 ORM 中转、Req 是
    前端提交的 id）→ **不该**有 `*_name`（加了反而让人以为能回写姓名）。

所以脚本按「文件名 + 结构体后缀」粗分类，`--check` 只对出参 DTO（`*Res` /
`*Card` / 列表行白名单）施加约束，其余只进盘点表、标注为 N/A。
"""

from __future__ import annotations

import argparse
import os
import re
import sys

BACKEND = os.environ.get("CLOUDPIVOT_BACKEND", "backend/src")
FRONTEND = os.environ.get("CLOUDPIVOT_FRONTEND", "frontend/src")
EVIDENCE_DIR = os.environ.get(
    "CLOUDPIVOT_EVIDENCE", "docs/evidence/20261109-aud02-uuid-audit"
)

# 人员字段：一个字段名能表达「这是个人」就够，不追求穷举
PERSON_FIELD = re.compile(
    r"^(create_by|created_by|update_by|updated_by|owner_id|salesperson_id|"
    r"user_id|operator_id|assignee_id|uploaded_by|creator_id|handler_id|"
    r"approver_id|manager_id|leader_id|drafter_id|applicant_id|reviewer_id|"
    r"initiator_id|assigner_id|previous_owner_id)$"
)

# 出参 DTO 的结构体名后缀（会序列化给前端）
RESP_SUFFIXES = ("Res", "Card", "Row", "Item", "Detail", "Version", "Member")

# 已修复、必须长期保持的「出参 DTO → 人字段 → name 字段」
REQUIRED = [
    ("business/erp/contract_dto.rs", "ContractDetailRes", "owner_id", "owner_name"),
    ("business/erp/contract_dto.rs", "ContractDetailRes", "create_by", "create_by_name"),
    ("business/erp/contract_dto.rs", "ContractDetailRes", "update_by", "update_by_name"),
    ("business/erp/stockin_dto.rs", "StockInRes", "create_by", "create_by_name"),
    ("business/erp/stockout_dto.rs", "StockOutRes", "create_by", "create_by_name"),
    ("business/erp/stocktake_dto.rs", "StockTakeRes", "create_by", "create_by_name"),
    ("business/erp/dto.rs", "InventoryTxnRes", "operator_id", "operator_name"),
    ("business/help/dto.rs", "HelpVersionRes", "create_by", "create_by_name"),
    ("business/mutex/dto.rs", "MutexRes", "create_by", "create_by_name"),
    ("business/storage/dto.rs", "StorageFileRes", "uploaded_by", "uploader_name"),
    ("system/dept/dto.rs", "DeptDetailRes", "leader_id", "leader_name"),
    ("support/file/dto.rs", "FileRes", "uploaded_by", "uploader_name"),
    ("business/attachment/dto.rs", "DocAttachmentItemRes", "uploaded_by", "uploader_name"),
]

# 已知的「界面直接渲染 id」写法（修掉之后一个都不许回来）
BAD_PATTERNS = [
    (
        "views/business/erp/contract/index.vue",
        r"shortId\(detail\.owner_id\)",
        "合同详情「负责人」曾显示截断 UUID",
    ),
    (
        "views/business/help/manage/index.vue",
        r"\{\{\s*version\.create_by\b",
        "帮助文档版本历史曾显示操作人 UUID",
    ),
    (
        "views/business/storage/file/index.vue",
        r"uploaderDisplayName\(row\.uploaded_by",
        "文件列表曾回退渲染上传人 UUID（现在走 uploaderLabelOf）",
    ),
    (
        "views/system/dept/index.vue",
        r"row\.leader_name \|\| row\.leader_id",
        "部门列表「负责人」曾回退渲染 leader_id（UUID）",
    ),
]

STRUCT_RE = re.compile(r"pub struct (\w+)\s*\{")
FIELD_RE = re.compile(r"^\s*pub\s+(\w+)\s*:\s*([^,]+),")
FRONTEND_FIELD_RE = re.compile(r"^\s*(\w+)\??\s*:\s*([^;]+);")
FRONTEND_TYPE_RE = re.compile(r"(?:export\s+)?(?:interface|type)\s+(\w+)\s*(?:=\s*)?\{")
UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)


def structs_of(path: str):
    """解析一个 .rs 文件里的 `pub struct` 及其字段（够用即可，不引第三方解析器）。"""
    lines = open(path, encoding="utf-8").read().split("\n")
    index = 0
    while index < len(lines):
        match = STRUCT_RE.search(lines[index])
        if not match:
            index += 1
            continue
        name = match.group(1)
        fields = []
        cursor = index + 1
        while cursor < len(lines) and "}" not in lines[cursor]:
            field = FIELD_RE.match(lines[cursor])
            if field:
                fields.append(field.group(1))
            cursor += 1
        yield name, fields
        index = cursor


def frontend_types_of(path: str):
    lines = open(path, encoding="utf-8").read().split("\n")
    index = 0
    while index < len(lines):
        match = FRONTEND_TYPE_RE.search(lines[index])
        if not match:
            index += 1
            continue
        name = match.group(1)
        props = []
        cursor = index + 1
        while cursor < len(lines) and not re.match(r"^\}", lines[cursor]):
            prop = FRONTEND_FIELD_RE.match(lines[cursor])
            if prop:
                props.append(prop.group(1))
            cursor += 1
        yield name, props
        index = cursor


def name_candidates(field: str) -> set[str]:
    """一个 id 字段可以接受的 name 字段名。"""
    base = re.sub(r"_(id|by)$", "", field)
    candidates = {f"{base}_name", f"{field}_name"}
    candidates |= {
        "create_by": {"created_by_name"},
        "update_by": {"updated_by_name"},
        "owner_id": {"owner_name"},
        "uploaded_by": {"uploader_name"},
        "assignee_id": {"assignee_name"},
    }.get(field, set())
    return candidates


def classify(path: str, struct: str) -> str:
    """出参 DTO / 库行 / 入参 / 其他。"""
    if struct.endswith("Req") or struct.endswith("Query") or struct.endswith("Filter"):
        return "入参"
    if struct.endswith("Row") and "dto" not in os.path.basename(path):
        return "库行"
    if struct.endswith(RESP_SUFFIXES):
        return "出参"
    return "其他"


def backend_inventory():
    rows = []
    for dirpath, _, files in os.walk(BACKEND):
        for filename in sorted(files):
            if not filename.endswith(".rs"):
                continue
            path = os.path.join(dirpath, filename)
            rel = os.path.relpath(path, BACKEND)
            for struct, fields in structs_of(path):
                person = [f for f in fields if PERSON_FIELD.match(f)]
                if not person:
                    continue
                missing = []
                for field in person:
                    if not (name_candidates(field) & set(fields)):
                        missing.append(field)
                rows.append(
                    {
                        "file": rel,
                        "struct": struct,
                        "person": person,
                        "missing": missing,
                        "kind": classify(path, struct),
                    }
                )
    return rows


def frontend_inventory():
    rows = []
    for dirpath, _, files in os.walk(FRONTEND):
        for filename in sorted(files):
            if not filename.endswith(".ts"):
                continue
            path = os.path.join(dirpath, filename)
            rel = os.path.relpath(path, FRONTEND)
            for name, props in frontend_types_of(path):
                person = [p for p in props if PERSON_FIELD.match(p)]
                if not person:
                    continue
                missing = [p for p in person if not (name_candidates(p) & set(props))]
                rows.append(
                    {
                        "file": rel,
                        "struct": name,
                        "person": person,
                        "missing": missing,
                        "kind": "前端类型",
                    }
                )
    return rows


def render_markdown(rows) -> str:
    out = ["| # | 文件 | 结构体 | 分类 | 人字段 | 缺 name |", "|---|---|---|---|---|---|"]
    for index, row in enumerate(rows, 1):
        out.append(
            "| {i} | `{file}` | `{struct}` | {kind} | {person} | {missing} |".format(
                i=index,
                file=row["file"],
                struct=row["struct"],
                kind=row["kind"],
                person=", ".join(row["person"]),
                missing=(", ".join(row["missing"]) if row["missing"] else "—"),
            )
        )
    return "\n".join(out)


def run_check() -> int:
    fails = []
    print("== 门禁 ①：出参 DTO 的人字段必须带 *_name ==")
    for rel, struct, field, expected in REQUIRED:
        path = os.path.join(BACKEND, rel)
        if not os.path.exists(path):
            fails.append(f"文件不存在：{rel}")
            continue
        found = dict(structs_of(path))
        if struct not in found:
            fails.append(f"{rel}：找不到结构体 {struct}")
            continue
        if expected not in found[struct]:
            fails.append(f"{rel}::{struct}：缺 `{expected}`（{field} 的显示名）")
            continue
        print(f"  [PASS] {struct}.{field} → {expected}")

    print("== 门禁 ②：界面不许直接渲染人员 id ==")
    for rel, pattern, why in BAD_PATTERNS:
        path = os.path.join(FRONTEND, rel)
        if not os.path.exists(path):
            fails.append(f"文件不存在：{rel}")
            continue
        content = open(path, encoding="utf-8").read()
        hits = [m for m in re.finditer(pattern, content)]
        if hits:
            fails.append(f"{rel} 仍命中 `{pattern}`（{why}）")
        else:
            print(f"  [PASS] {rel}：{why} —— 已消灭")

    if fails:
        print("\n== 失败 ==")
        for item in fails:
            print("  [FAIL] " + item)
        return 1
    print(f"\n== 全过：{len(REQUIRED)} 条 DTO 约束 + {len(BAD_PATTERNS)} 条界面约束 ==")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="AUD-02 人员字段盘点 / 门禁")
    parser.add_argument("--check", action="store_true", help="只跑门禁（不写证据文件）")
    args = parser.parse_args()

    if args.check:
        return run_check()

    rows = backend_inventory() + frontend_inventory()
    missing = [r for r in rows if r["missing"]]
    resp_missing = [r for r in missing if r["kind"] == "出参"]

    print(f"扫描到含人员字段的结构体：{len(rows)}")
    print(f"其中缺 name 的：{len(missing)}（其中出参 DTO：{len(resp_missing)}）")
    print()
    print(render_markdown(rows))

    if os.path.isdir(EVIDENCE_DIR):
        target = os.path.join(EVIDENCE_DIR, "inventory.md")
        header = (
            "# AUD-02 盘点表（脚本生成，勿手改）\n\n"
            f"- 含人员字段的结构体：{len(rows)}\n"
            f"- 缺 name：{len(missing)}（出参 DTO {len(resp_missing)}）\n"
            "- 生成命令：`python3 scripts/verify/aud02_person_name_inventory.py`\n\n"
        )
        open(target, "w", encoding="utf-8").write(header + render_markdown(rows) + "\n")
        print(f"\n已写入 {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
