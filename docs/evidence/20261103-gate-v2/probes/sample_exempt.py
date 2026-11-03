#!/usr/bin/env python3
"""抽检卡 B 的函数级豁免：把 3 个抽样文件里**每一处**登记表查询映射到 enclosing fn，
再看该 fn 上方是否有 SCOPE-EXEMPT 注释（闸② 实际是整文件豁免，所以「函数级」只能靠注释成立）。

另外判：该 fn 是否返回行集（-> Vec<…> / Vec<T> / 带 LIMIT 的列表查询）。
"""
import importlib.util
import os
import re
import sys
from pathlib import Path

SCRIPT = "/opt/cloudpivot/scripts/lint/scope_gate.py"
SRC = Path(os.environ.get("CP_SRC", "/opt/cloudpivot/backend/src"))
TABLES = """sys_user sys_dept sys_import_task sys_export_task sys_notice
sys_file operation_logs audit_logs auth_logs security_logs
biz_task biz_task_comment biz_task_attachment biz_task_worklog
biz_task_member biz_task_dependency biz_task_status_def
biz_project biz_project_member
biz_flow_def biz_flow_def_version biz_flow_node biz_flow_edge biz_flow_node_assignee
biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc""".split()

spec = importlib.util.spec_from_file_location("scope_gate", SCRIPT)
sg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sg)


def enclosing_fn_line(lines, idx):
    """从 idx 往上找最近的 `fn name(` 行。"""
    for i in range(idx, -1, -1):
        m = re.search(r"\bfn\s+([A-Za-z_]\w*)", lines[i])
        if m:
            return m.group(1), i + 1
    return "<top>", 0


def fn_signature(lines, start):
    """把 fn 签名拼起来（可能跨多行），直到 `{` 或 `->…`。"""
    buf = []
    depth = 0
    for i in range(start, min(start + 12, len(lines))):
        buf.append(lines[i].strip())
        depth += lines[i].count("(") - lines[i].count(")")
        if "{" in lines[i] or (depth <= 0 and ";" in lines[i]):
            break
    return " ".join(buf)


def main():
    targets = [
        "business/task/task_model_flow.rs",
        "business/task/task_model_hierarchy.rs",
        "business/task/task_model_schedule.rs",
    ]
    for rel in targets:
        path = SRC / rel
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        print("=" * 100)
        print(f"### {rel}  （{len(lines)} 行）")
        exempt_lines = [i + 1 for i, ln in enumerate(lines) if "SCOPE-EXEMPT" in ln]
        print(f"SCOPE-EXEMPT 注释行：{exempt_lines}   scope:: 出现次数：{text.count('scope::')}")
        print(f"pub fn 数量：{len(re.findall(r'pub (?:async )?fn ', text))}")
        # 每一处查询
        seen = {}
        for i, ln in enumerate(lines):
            stripped = ln.strip()
            if stripped.startswith("//") or stripped.startswith("*"):
                continue
            hits = [t for t in TABLES if f"FROM {t}" in ln]
            if not hits:
                continue
            fname, fline = enclosing_fn_line(lines, i)
            seen.setdefault((fname, fline), {"lines": [], "tables": set()})
            seen[(fname, fline)]["lines"].append(i + 1)
            seen[(fname, fline)]["tables"].update(hits)
        for (fname, fline), info in sorted(seen.items(), key=lambda kv: kv[0][1]):
            sig = fn_signature(lines, fline - 1)
            # 该 fn 之前（同 fn 上方 25 行内 / 到上一个 fn 之间）有没有 SCOPE-EXEMPT
            lo = max(0, fline - 26)
            near = [n for n in exempt_lines if lo <= n <= fline]
            returns_rows = bool(re.search(r"->\s*(?:Result<)?\s*Vec\b", sig)) or "Vec<" in sig.split("->")[-1] if "->" in sig else False
            print(f"\n  fn {fname}  (定义行 {fline})  查询行 {info['lines']}  表 {sorted(info['tables'])}")
            print(f"     豁免注释在其上方 25 行内：{near if near else '【无】'}")
            print(f"     签名：{sig[:200]}")
            print(f"     返回行集(Vec)：{returns_rows}")

        # 反向：有豁免注释但注释下方没有查询的（说明注释与查询不毗邻）
        covered = set()
        for info in seen.values():
            covered.update(info["lines"])
        print(f"\n  查询行合计：{sorted(covered)}")
        for n in exempt_lines:
            nxt = [c for c in sorted(covered) if c > n]
            print(f"   豁免 @{n} → 下一个查询行 {nxt[0] if nxt else '【之后没有查询！】'}"
                  f"  注释：{lines[n-1].strip()[:110]}")


if __name__ == "__main__":
    sys.exit(main())
