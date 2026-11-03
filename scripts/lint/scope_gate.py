#!/usr/bin/env python3
"""门禁「闸②」（口径 v2）：登记表的查询必须接入 `scope::` 或写明 `SCOPE-EXEMPT` 理由。

判定面 = **会进生产编译的代码**（口径 v2 · D1）：

1. 父模块以 `#[cfg(test)]` 声明的模块文件 → 整文件跳过。支持三种挂载写法：
   - `#[cfg(test)] mod x;`（含 `pub mod` / `pub(crate) mod`）；
   - `#[cfg(test)] #[path = "xxx_tests.rs"] mod tests;`；
   - 同目录 `mod.rs` / `dir.rs` 里以 `mod tests;` 挂载的 `tests.rs`；
   模块文件解析按 **Rust 语义**（口径 v2 硬化 R2）：父文件是 `mod.rs` / `lib.rs` / `main.rs`
   时子模块落 `dir/x.rs`，父文件是 `foo.rs` 时落 `foo/x.rs`（**不是** `foo 的父目录/x.rs`）。
2. 文件内 `#[cfg(test)] mod xxx { … }` 区域跳过（含 `#[cfg(all(test, …))]`），按 `{}` 配平定位。
   跳过是**字符级**的（口径 v2 硬化 R3）：同一行里写在测试区之前/之后的生产代码仍然照判，
   不会因为「这一行沾了测试区」就整行放过。
3. 跳过可审计：每次运行打印「扫描生产面 N / 跳过测试面 M」；`--list-skipped` 打印清单
   （带声明位置 `<声明文件>:<行>`，便于逐条复核）。
4. 防假绿硬断言之一：`REQUIRED_PRODUCTION` 里的已知生产文件必须落在生产面里，
   解析器一旦把它们误判成测试面 → 立刻 FAIL（而不是静默变绿）。
5. 防假绿硬断言之二（口径 v2 硬化 R1）：**被跳过的文件清单本身也是硬断言**。
   实测集合必须与 `SKIP_MANIFEST` 逐条一致，多一个少一个都 FAIL ——
   否则一句 `#[cfg(test)] #[path = "../task/生产文件.rs"] mod ghost;` 就能把任意生产文件
   藏进测试面（复核 T5b 的复现路径）。新增/删除测试面文件时必须同步改常量（评审可见）。

其余判定与改写前的 `scripts/check.sh` **逐条一致**：

- 整行注释不算代码行（`//`、`*` 开头）；`FROM <表>` 与 `scope::` 都只在代码行上判
  （注释里举例的 SQL / 「参考 scope::」的说明不算数 —— 后者会造成假绿）；
- `SCOPE-EXEMPT` 沿**整文件**搜索（标记本身就是注释）；
- 表名按**前缀**命中（`FROM biz_task` 同时命中 `biz_task_comment` 等），按文件聚合。

用法：

    python3 scripts/lint/scope_gate.py --root .              # 判定（0 通过 / 1 失败）
    python3 scripts/lint/scope_gate.py --root . --list-skipped
    python3 scripts/lint/scope_gate.py --self-test           # 负例自测（口径 v2 · D6）

退出码：0 = 全部通过；1 = 有失败（含防假绿断言失败）；2 = 用法/环境错误。
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from collections.abc import Collection
from pathlib import Path
from typing import NamedTuple

# 登记表清单：与 scripts/check.sh 的 BATCHB_SCOPED_TABLES 逐字一致（28 张）
TABLES = """
sys_user sys_dept sys_import_task sys_export_task sys_notice
sys_file operation_logs audit_logs auth_logs security_logs
biz_task biz_task_comment biz_task_attachment biz_task_worklog
biz_task_member biz_task_dependency biz_task_status_def
biz_project biz_project_member
biz_flow_def biz_flow_def_version biz_flow_node biz_flow_edge biz_flow_node_assignee
biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc
""".split()

# 扫描范围硬断言（设计修正 A · 修正 2 第②条）：这些目录必须在范围内且真有 .rs
SCAN_DIRS = ("business/task", "business", "system", "support", "shared")

# 防假绿硬断言（口径 v2 · D1.4）：这几个是**已知生产文件**（分别接入 scope::paginate /
# scope::filter_for / 是 scope 实现本身）。解析器一旦把它们判成测试面，闸② 必须红。
REQUIRED_PRODUCTION = (
    "business/task/task_model_query.rs",
    "business/project/model.rs",
    "business/shared/scope.rs",
)

# 跳过清单硬断言（口径 v2 硬化 R1）：被判定为测试面、因而不参与闸② 判定的文件，**逐条登记**。
#
# 为什么需要它：`REQUIRED_PRODUCTION` 只钉住了 3 个已知生产文件，其余 300+ 生产面文件都可以被
# 一句 `#[cfg(test)] #[path = "../task/生产文件.rs"] mod ghost_copy;` 藏进测试面（复核 T5b 实测
# 红变绿）。所以「跳过集合」本身必须是一个显式常量：实测集合与它不一致 → FAIL。
#
# 维护口径：新增/删除测试面文件时同步改这里（多一条少一条都算 FAIL，报错会直接给出可粘贴的行）。
# 这是刻意的摩擦 —— 跳过权限的每一次扩张都要在评审里被看见。
SKIP_MANIFEST = frozenset(
    """
business/attachment/tests.rs
business/erp/cancel_cascade_tests.rs
business/erp/contract_approval_tests.rs
business/erp/fixture_hygiene.rs
business/erp/model_tests.rs
business/erp/period_items_tests.rs
business/erp/period_remind_tests.rs
business/erp/period_test_support.rs
business/erp/pflow3b_tests.rs
business/erp/reminder_tests.rs
business/erp/stockin_handler_tests_a4.rs
business/erp/stockin_handler_tests_cancel.rs
business/erp/stockin_handler_tests_d4_purchase.rs
business/erp/stockin_handler_tests_d9_copy.rs
business/erp/stockin_handler_tests_dc03.rs
business/erp/stockin_handler_tests_fixtures.rs
business/erp/stockin_handler_tests_helpers.rs
business/erp/stockin_handler_tests_location.rs
business/erp/stockin_handler_tests_p10_locked.rs
business/erp/stockin_handler_tests_p16.rs
business/erp/stockin_handler_tests_pflow3a.rs
business/erp/stockin_handler_tests_posting.rs
business/erp/stockin_handler_tests_update.rs
business/erp/stockin_tests.rs
business/erp/stockout_tests.rs
business/erp/stocktake_tests.rs
business/erp/transfer_tests.rs
business/erp/year_end_items_tests.rs
business/erp/year_end_remind_tests.rs
business/erp/year_end_test_support.rs
business/flow/edge_admin_tests.rs
business/flow/linkage/tests.rs
business/flow/test_support.rs
business/impexp/builtin/master_data/db_tests.rs
business/masking/tests.rs
business/permission/tests.rs
business/task/task_model_cascade_cancel_tests.rs
business/task/task_model_cascade_tests.rs
business/task/task_model_depth_tests.rs
business/task/task_model_display_file_name_tests.rs
business/task/task_model_repair_tests.rs
business/task/task_model_root_only_tests.rs
business/task/task_model_stats_tests.rs
business/task/task_model_statuses_filter_tests.rs
business/task/task_model_tests.rs
system/authz_tests.rs
system/dept/tests.rs
system/menu/permission_audit_tests.rs
system/menu/tests.rs
system/role/tests.rs
system/session_tests.rs
system/tests_support.rs
system/user/tests.rs
""".split()
)

CFG_ATTR_RE = re.compile(r"#\[\s*cfg\s*\(")
PATH_ATTR_RE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
MOD_RE = re.compile(r"(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*([;{])")
COMMENT_LINE_RE = re.compile(r"^[ \t]*(?://|\*)")


class Decl(NamedTuple):
    name: str
    path_attr: str | None
    line: int


def mask(source: str) -> str:
    """把注释与字符串字面量替换成等长空格，保留偏移（配平 `{}` 时不会被字符串里的括号带偏）。

    与 `scripts/lint/changelog_in_tx.py` 的 `mask()` 同款（这里不 import，保持脚本独立可单跑）。
    """
    out = list(source)
    i = 0
    n = len(source)
    while i < n:
        ch = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        # 行注释
        if ch == "/" and nxt == "/":
            while i < n and source[i] != "\n":
                out[i] = " "
                i += 1
            continue
        # 块注释（Rust 允许嵌套）
        if ch == "/" and nxt == "*":
            depth = 1
            out[i] = out[i + 1] = " "
            i += 2
            while i < n and depth > 0:
                if source[i] == "/" and i + 1 < n and source[i + 1] == "*":
                    depth += 1
                    out[i] = out[i + 1] = " "
                    i += 2
                    continue
                if source[i] == "*" and i + 1 < n and source[i + 1] == "/":
                    depth -= 1
                    out[i] = out[i + 1] = " "
                    i += 2
                    continue
                if source[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        # 原始字符串 r"..." / r#"..."# / br#"..."#
        if ch in "rb" and re.match(r'(?:b?r|rb)#*"', source[i : i + 8]):
            prefix = re.match(r'(?:b?r|rb)(#*)"', source[i : i + 8])
            if prefix:
                hashes = prefix.group(1)
                end_marker = '"' + hashes
                start = i
                i += prefix.end()
                while i < n:
                    if source.startswith(end_marker, i):
                        i += len(end_marker)
                        break
                    i += 1
                for k in range(start, min(i, n)):
                    if source[k] != "\n":
                        out[k] = " "
                continue
        # 普通字符串
        if ch == '"':
            out[i] = " "
            i += 1
            while i < n:
                if source[i] == "\\":
                    if source[i] != "\n":
                        out[i] = " "
                    if i + 1 < n:
                        if source[i + 1] != "\n":
                            out[i + 1] = " "
                        i += 2
                        continue
                if source[i] == '"':
                    out[i] = " "
                    i += 1
                    break
                if source[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        # 字符字面量（'x' / '\n'）；生命周期标注 'a 原样保留
        if ch == "'":
            match = re.match(r"'(\\.|[^\\'])'", source[i : i + 6])
            if match:
                for k in range(match.end()):
                    if source[i + k] != "\n":
                        out[i + k] = " "
                i += match.end()
                continue
        i += 1
    return "".join(out)


def match_balanced(text: str, open_index: int, opener: str = "(", closer: str = ")") -> int | None:
    """返回与 `text[open_index]` 配对的闭括号**之后**的偏移；未闭合返回 None。"""
    if open_index >= len(text) or text[open_index] != opener:
        return None
    depth = 0
    i = open_index
    while i < len(text):
        ch = text[i]
        if ch == opener:
            depth += 1
        elif ch == closer:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return None


def split_top_level(text: str) -> list[str]:
    parts: list[str] = []
    cur: list[str] = []
    depth = 0
    for ch in text:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    if cur:
        parts.append("".join(cur))
    return parts


def is_test_only(cfg_content: str) -> bool:
    """`test` / `all(test, …)` → 只在测试编译；`any(test, …)` / `not(test)` → 可能进生产，不跳。"""
    content = cfg_content.strip()
    if content == "test":
        return True
    match = re.match(r"all\s*\((.*)\)$", content, re.S)
    if match:
        return any(part.strip() == "test" for part in split_top_level(match.group(1)))
    return False


def parse_module_decls(source: str, masked: str) -> tuple[list[Decl], list[tuple[int, int]]]:
    """扫出文件内的 `#[cfg(test)]` 模块声明与内联区。

    返回 (decls, inline_regions)，inline_regions 是 0-based 的 (起始偏移, 结束偏移)，
    覆盖从 `#[cfg(test)]` 属性开头到模块体 `{}` 收尾之后 —— 字符级而不是行级（口径 v2 硬化 R3），
    这样同一行里写在测试区之外的生产代码不会被整行吞掉。
    """
    decls: list[Decl] = []
    regions: list[tuple[int, int]] = []
    i = 0
    n = len(masked)
    while i < n:
        match = CFG_ATTR_RE.search(masked, i)
        if not match:
            break
        content_end = match_balanced(masked, match.end() - 1)
        if content_end is None:
            break
        content = masked[match.end() : content_end - 1]
        i = content_end
        if not is_test_only(content):
            continue
        # `#[cfg(...)]` 的属性闭括号
        j = content_end
        while j < n and masked[j] in " \t\r\n":
            j += 1
        if j >= n or masked[j] != "]":
            continue
        j += 1
        # 属性链：`#[path = "..."]` 等（从原文取，masked 里的字符串已被抹掉）
        path_attr: str | None = None
        while True:
            k = j
            while k < n and masked[k] in " \t\r\n":
                k += 1
            if not masked.startswith("#[", k):
                break
            attr_end = match_balanced(masked, k + 1, "[", "]")
            if attr_end is None:
                break
            raw = source[k:attr_end]
            path_match = PATH_ATTR_RE.search(raw)
            if path_match:
                path_attr = path_match.group(1)
            j = attr_end
        k = j
        while k < n and masked[k] in " \t\r\n":
            k += 1
        mod_match = MOD_RE.match(masked, k)
        if not mod_match:
            i = j
            continue
        name, terminator = mod_match.group(1), mod_match.group(2)
        line = masked.count("\n", 0, mod_match.start()) + 1
        if terminator == ";":
            decls.append(Decl(name, path_attr, line))
            i = mod_match.end()
        else:
            brace_end = match_balanced(masked, mod_match.end() - 1, "{", "}")
            if brace_end is None:
                # 花括号没配平（文件被截断/写坏）：保守处理 —— 不放行任何内联区，
                # 后面所有代码都按生产面判（宁可报红，不许静默变绿）。
                break
            regions.append((match.start(), brace_end))
            i = brace_end
    return decls, regions


def rust_parent_dir(parent: Path) -> Path:
    """父**文件**对应的「子模块目录」，按 Rust 模块语义算（口径 v2 硬化 R2）。

    - `dir/mod.rs` / `src/lib.rs` / `src/main.rs`：子模块落在 `dir/`（= parent.parent）；
    - `foo.rs`（非 mod.rs 的普通模块文件）：Rust 里 `foo.rs` 的子模块落在 `foo/x.rs`，
      所以目录是 `parent.parent / parent.stem`。

    旧实现一律用 `parent.parent`，于是 `foo.rs` 里的 `#[cfg(test)] mod x;` 会指向
    `父目录/x.rs` —— 那是**另一个模块**的文件（合法 Rust 里的别的模块），
    结果把生产文件误判成测试面（T4 复现：`task/mod.rs` 的 `mod zz_c;` 被
    `task/model.rs` 的 `#[cfg(test)] mod zz_c;` 连坐跳过）。
    """
    if parent.name in ("mod.rs", "lib.rs", "main.rs"):
        return parent.parent
    return parent.parent / parent.stem


def child_module_paths(parent: Path, name: str, path_attr: str | None) -> list[Path]:
    """按 Rust 模块语义把 `mod name;` 解析成候选文件路径。

    普通声明：父文件是 `dir/mod.rs` / `src/lib.rs` / `src/main.rs` 时子模块落 `dir/name.rs`
    或 `dir/name/mod.rs`；父文件是 `foo.rs` 时落 `foo/name.rs` 或 `foo/name/mod.rs`。
    `#[path = "..."]` 的相对基准是**声明所在文件的那个目录**（Rust 参考：非内联 mod 的
    path 属性相对源文件所在目录），即 `parent.parent`。
    """
    if path_attr:
        return [parent.parent / path_attr]
    base = rust_parent_dir(parent)
    return [base / f"{name}.rs", base / name / "mod.rs"]


class SourceFile(NamedTuple):
    path: Path
    text: str
    masked: str
    regions: list[tuple[int, int]]
    decls: list[Decl]


def load_files(src: Path) -> dict[Path, SourceFile]:
    files: dict[Path, SourceFile] = {}
    for path in sorted(src.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        masked = mask(text)
        decls, regions = parse_module_decls(text, masked)
        files[path.resolve()] = SourceFile(path, text, masked, regions, decls)
    return files


def collect_test_surface(files: dict[Path, SourceFile]) -> dict[Path, str]:
    """返回 {被跳过的文件 → 声明位置（相对路径:行）}。"""
    skipped: dict[Path, str] = {}
    for resolved, info in files.items():
        for decl in info.decls:
            for candidate in child_module_paths(resolved, decl.name, decl.path_attr):
                target = candidate.resolve()
                if target in files and target not in skipped:
                    skipped[target] = f"{info.path}:{decl.line}"
    return skipped


def code_lines(info: SourceFile) -> list[str]:
    """去掉整行注释与内联 `#[cfg(test)] mod` 区域后的代码行。

    测试区按**字符偏移**抹成等长空格，而不是整行丢弃（口径 v2 硬化 R3）：生产代码与
    `#[cfg(test)] mod` 写在同一行时（复核 U2：`pub fn q() { … FROM sys_notice … } #[cfg(test)]
    mod tests { … }`）仍然照判，不会因为「这一行沾了测试区」就整行放过。
    """
    out: list[str] = []
    offset = 0
    for line in info.text.split("\n"):
        start = offset
        offset += len(line) + 1
        if COMMENT_LINE_RE.match(line):
            continue
        visible = line
        for region_start, region_end in info.regions:
            lo = max(region_start, start)
            hi = min(region_end, start + len(line))
            if lo < hi:
                visible = visible[: lo - start] + " " * (hi - lo) + visible[hi - start :]
        out.append(visible)
    return out


class Report(NamedTuple):
    failures: dict[str, list[str]]  # 相对 src 的路径 → 命中的表（排序去重）
    skipped: dict[str, str]  # 相对 src 的路径 → 声明位置
    scanned: int
    coverage_errors: list[str]
    guard_errors: list[str]
    skip_manifest_errors: list[str] = []

    @property
    def ok(self) -> bool:
        return not (self.failures or self.coverage_errors or self.guard_errors or self.skip_manifest_errors)


def run_gate(src: Path, expect_skipped: Collection[str] | None = None) -> Report:
    """跑一次闸② 判定。

    `expect_skipped` = 跳过清单硬断言（R1）：传 `SKIP_MANIFEST` 时，实测跳过集合必须与它
    逐条一致；自测用的小型 fixture 传各自的期望集合，传 None 表示不做清单断言。
    """
    files = load_files(src)
    if not files:
        raise FileNotFoundError(f"{src} 下没有 .rs 文件")

    coverage_errors: list[str] = []
    for rel in SCAN_DIRS:
        directory = src / rel
        if not directory.is_dir():
            coverage_errors.append(f"扫描范围缺少目录 backend/src/{rel}（闸② 会漏掉该目录下的查询）")
            continue
        if not any(directory.rglob("*.rs")):
            coverage_errors.append(f"扫描范围 backend/src/{rel} 下没有 .rs 文件（目录搬走或未纳入版本管理）")

    skipped = collect_test_surface(files)
    production = {p: info for p, info in files.items() if p not in skipped}

    # 防假绿硬断言（D1.4）：已知生产文件必须落在生产面
    guard_errors: list[str] = []
    production_rel = {info.path.relative_to(src).as_posix() for info in production.values()}
    for rel in REQUIRED_PRODUCTION:
        target = (src / rel).resolve()
        if not target.is_file():
            guard_errors.append(f"防假绿断言：已知生产文件 backend/src/{rel} 不存在（扫描根/目录结构异常）")
        elif target in skipped:
            guard_errors.append(
                f"防假绿断言：已知生产文件 backend/src/{rel} 被判成了测试面"
                f"（声明位置 {skipped[target]}）—— 解析器误判，闸② 不许据此变绿"
            )
        elif rel not in production_rel:
            guard_errors.append(f"防假绿断言：已知生产文件 backend/src/{rel} 不在生产面扫描结果里")

    failures: dict[str, list[str]] = {}
    skipped_rel = {info.path.relative_to(src).as_posix(): why for p, why in skipped.items() for info in [files[p]]}

    # 跳过清单硬断言（R1）：实测跳过集合必须与登记的清单逐条一致，多一个少一个都 FAIL。
    skip_manifest_errors: list[str] = []
    if expect_skipped is not None:
        expected = set(expect_skipped)
        observed = set(skipped_rel)
        for rel in sorted(observed - expected):
            skip_manifest_errors.append(
                f"跳过清单断言：backend/src/{rel} 被判成了测试面，但 SKIP_MANIFEST 里没有登记"
                f"（声明位置 {skipped_rel[rel]}）—— 这正是「一句 #[cfg(test)] 声明把生产文件藏起来」"
                f'的形态；确认它确实是测试面后，把这一行补进 scripts/lint/scope_gate.py："{rel}",'
            )
        for rel in sorted(expected - observed):
            skip_manifest_errors.append(
                f"跳过清单断言：SKIP_MANIFEST 登记了 backend/src/{rel}，但本次解析没有跳过它"
                f"（文件被删/改名，或声明被去掉）—— 请从 SKIP_MANIFEST 删掉这一行：\"{rel}\","
            )

    for info in production.values():
        lines = code_lines(info)
        hits = [table for table in TABLES if any(f"FROM {table}" in line for line in lines)]
        if not hits:
            continue
        # 与 check.sh 一致：SCOPE-EXEMPT 沿整文件搜；scope:: 只看代码行
        if "SCOPE-EXEMPT" in info.text:
            continue
        if any("scope::" in line for line in lines):
            continue
        # 表名按字典序输出（沿用原 check.sh 的 `sort -u` 口径，便于和改写前的输出逐行对照）
        failures[info.path.relative_to(src).as_posix()] = sorted(set(hits))

    return Report(
        failures,
        dict(sorted(skipped_rel.items())),
        len(production),
        coverage_errors,
        guard_errors,
        skip_manifest_errors,
    )


def print_report(report: Report, list_skipped: bool, baseline: int | None) -> None:
    print(
        f"闸② 判定面（v2）：扫描生产面 {report.scanned} 个文件 / 跳过测试面 {len(report.skipped)} 个文件"
    )
    print(
        "    跳过依据：父模块 `#[cfg(test)]` 声明（mod x; / #[path=...] mod x;）"
        "与文件内 `#[cfg(test)] mod {…}` 内联区（均不进生产编译）"
    )
    if report.coverage_errors:
        for item in report.coverage_errors:
            print(f"  FAIL：{item}")
    else:
        print("  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}")
    if list_skipped:
        print(f"被跳过的测试面文件（{len(report.skipped)} 个，含声明位置）：")
        for rel, why in report.skipped.items():
            print(f"  {rel}  ← {why}")
    for rel, tables in sorted(report.failures.items()):
        print(f"    FAIL: {rel} 查询 {' '.join(tables)} 未接入 scope:: 且无 SCOPE-EXEMPT")
    if report.guard_errors:
        for item in report.guard_errors:
            print(f"  FAIL：{item}")
    if report.skip_manifest_errors:
        for item in report.skip_manifest_errors:
            print(f"  FAIL：{item}")
    else:
        print(f"  PASS：跳过清单与 SKIP_MANIFEST 逐条一致（{len(report.skipped)} 条；多一个少一个都不放行）")
    if report.failures:
        tail = (
            f"（按文件计；B0 实测基线 {baseline} 处是 file×table 口径，仅作历史参考）"
            if baseline is not None
            else "（按文件计）"
        )
        print(f"  FAIL：{len(report.failures)} 个文件{tail}")
        print("  失败文件清单（file (tables)，供逐文件裁定核对）：")
        for rel, tables in sorted(report.failures.items()):
            print(f"    {rel} ({', '.join(tables)})")
    elif not (report.coverage_errors or report.guard_errors or report.skip_manifest_errors):
        print("  PASS：0 处")


# ---------------------------------------------------------------------------
# 负例自测（口径 v2 · D6）：在临时目录里造样例代码，走**同一个** run_gate 判定
# ---------------------------------------------------------------------------

BASE_FIXTURE = {
    "business/mod.rs": "pub mod task;\npub mod project;\npub mod erp;\n",
    "business/task/mod.rs": (
        "mod task_model_query;\n"
        "mod task_model_write;\n"
        "mod inline_tests_only;\n"
        "#[cfg(test)]\n"
        "mod task_model_write_tests;\n"
        "#[cfg(test)]\n"
        '#[path = "task_model_path_tests.rs"]\n'
        "mod path_mounted_tests;\n"
    ),
    # 已知生产文件（防假绿断言要求存在且在生产面）
    "business/task/task_model_query.rs": (
        "pub async fn page() -> () {\n"
        "    let _ = scope::paginate(TARGET, &filters);\n"
        '    let _sql = "SELECT * FROM biz_task WHERE 1=1";\n'
        "}\n"
    ),
    "business/project/model.rs": (
        "pub async fn get() -> () {\n"
        "    let _ = scope::filter_for(TARGET);\n"
        '    let _sql = "SELECT * FROM biz_project";\n'
        "}\n"
    ),
    "business/shared/scope.rs": "pub fn paginate() -> () {}\n",
    "business/shared/mod.rs": "pub mod scope;\n",
    # ① 生产面真实漏接 → 必须报红
    "business/task/task_model_write.rs": (
        "pub async fn next_no() -> () {\n"
        '    let _sql = "SELECT MAX(task_no) FROM biz_task";\n'
        "    // SELECT * FROM sys_user  ← 整行注释，不算代码行\n"
        "}\n"
    ),
    # ② #[cfg(test)] 声明的整文件 → 跳过（并进 --list-skipped 清单）
    "business/task/task_model_write_tests.rs": (
        'fn q() { let _sql = "SELECT * FROM biz_task"; }\n'
    ),
    # ②' #[cfg(test)] #[path = "..."] 挂载 → 跳过
    "business/task/task_model_path_tests.rs": (
        'fn q() { let _sql = "DELETE FROM biz_task_member"; }\n'
    ),
    # ③ 文件内 #[cfg(test)] 内联区 → 跳过（外层生产代码无漏接）
    "business/task/inline_tests_only.rs": (
        "pub fn plain() -> i32 { 1 }\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    fn q() {\n"
        '        let _sql = "SELECT count(*) FROM sys_user";\n'
        "    }\n"
        "}\n"
    ),
    # ④ 生产面写 SCOPE-EXEMPT → 放行（原有语义）
    "business/erp/mod.rs": "pub mod exempt;\n",
    "business/erp/exempt.rs": (
        "// SCOPE-EXEMPT: 后台扫描，不产出用户可见行集（权限在 handler.rs:12 校验）\n"
        "pub fn scan() { let _sql = \"SELECT * FROM sys_notice\"; }\n"
    ),
    # ⑤ 注释里的 scope:: 不能造成假绿：该文件没有真实 scope::，必须报红
    "business/erp/comment_only_scope.rs": (
        "pub fn q() {\n"
        "    // 参考 scope::ScopeKind 的说明，但本文件并没有接入\n"
        '    let _sql = "SELECT * FROM biz_task_comment";\n'
        "}\n"
    ),
    "system/mod.rs": "pub mod dept;\n",
    "system/dept/model.rs": "pub fn ok() {}\n",
    "support/mod.rs": "pub fn ok() {}\n",
    "shared/mod.rs": "pub fn ok() {}\n",
}

# 自测场景：期望的失败集合（相对 src 的路径）与必须跳过的文件
SELF_TEST_CASES = (
    (
        "① 生产面真实漏接 → 报红",
        {},
        {"business/task/task_model_write.rs", "business/erp/comment_only_scope.rs"},
        {"business/task/task_model_write_tests.rs", "business/task/task_model_path_tests.rs"},
    ),
    (
        "② #[cfg(test)] 声明的整文件（mod x; 与 #[path] 两种）→ 跳过且进清单",
        {},
        None,
        {"business/task/task_model_write_tests.rs", "business/task/task_model_path_tests.rs"},
    ),
    (
        "③ 内联 #[cfg(test)] mod 区 → 跳过（文件本身不报红）",
        {},
        None,
        set(),
    ),
    (
        "④ 防假绿断言：把已知生产文件声明成 #[cfg(test)] → 必须 FAIL",
        {"business/task/mod.rs": "#[cfg(test)]\nmod task_model_query;\nmod task_model_write;\n"},
        None,
        None,
    ),
)


def write_fixture(root: Path, overrides: dict[str, str]) -> None:
    for rel, content in {**BASE_FIXTURE, **overrides}.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")


# 自测 fixture 自己的「跳过清单」（对应 BASE_FIXTURE 里两条 #[cfg(test)] 声明）
FIXTURE_SKIP_MANIFEST = frozenset(
    {
        "business/task/task_model_write_tests.rs",
        "business/task/task_model_path_tests.rs",
    }
)


def self_test() -> int:
    base = Path(tempfile.mkdtemp(prefix="scope_gate_selftest_"))
    print(f"闸② 负例自测（临时目录 {base}）")
    failures = 0

    root = base / "main"
    write_fixture(root, {})
    # src 根 = fixture 根（其下就是 business/ system/ support/ shared/）
    report = run_gate(root, FIXTURE_SKIP_MANIFEST)

    def check(label: str, condition: bool, detail: str = "") -> None:
        nonlocal failures
        print(f"  {'PASS' if condition else 'FAIL'}  {label}{('  ' + detail) if detail else ''}")
        if not condition:
            failures += 1

    check(
        "① 生产面漏接 task_model_write.rs 报红",
        "business/task/task_model_write.rs" in report.failures,
        f"failures={sorted(report.failures)}",
    )
    check(
        "② #[cfg(test)] mod x; 挂载的 tests 文件被跳过（不在 failures）",
        "business/task/task_model_write_tests.rs" in report.skipped
        and "business/task/task_model_write_tests.rs" not in report.failures,
    )
    check(
        "②' #[cfg(test)] #[path=...] 挂载的 tests 文件被跳过",
        "business/task/task_model_path_tests.rs" in report.skipped
        and "business/task/task_model_path_tests.rs" not in report.failures,
    )
    check(
        "③ 内联 #[cfg(test)] mod 区的漏接被跳过（inline_tests_only.rs 不报红）",
        "business/task/inline_tests_only.rs" not in report.failures,
    )
    check(
        "④ SCOPE-EXEMPT 生产文件放行（business/erp/exempt.rs）",
        "business/erp/exempt.rs" not in report.failures,
    )
    check(
        "⑤ 注释里的 scope:: 不造成假绿（comment_only_scope.rs 报红）",
        "business/erp/comment_only_scope.rs" in report.failures,
    )
    check(
        "⑥ 内联注释行里的 FROM 不算代码行（task_model_write.rs 只报 biz_task）",
        report.failures.get("business/task/task_model_write.rs") == ["biz_task"],
        f"实际 {report.failures.get('business/task/task_model_write.rs')}",
    )
    check(
        "⑦ 扫描/跳过计数可审计",
        report.scanned > 0 and len(report.skipped) == 2,
        f"scanned={report.scanned} skipped={len(report.skipped)}",
    )
    check(
        "⑦' 跳过清单断言：实测跳过集合 == 登记清单 → 不报错（正常场景不许误伤）",
        report.skip_manifest_errors == [],
        "; ".join(report.skip_manifest_errors) or "skip_manifest_errors 为空（正确）",
    )

    # ④ 防假绿断言：把 REQUIRED_PRODUCTION 之一声明成 #[cfg(test)] → run_gate 必须报 guard 错
    root2 = base / "guard"
    write_fixture(
        root2,
        {
            "business/task/mod.rs": (
                "#[cfg(test)]\nmod task_model_query;\n"
                "mod task_model_write;\nmod inline_tests_only;\n"
            )
        },
    )
    guard_report = run_gate(root2)
    check(
        "⑧ 防假绿断言：已知生产文件被误判成测试面 → FAIL（guard_errors 非空）",
        bool(guard_report.guard_errors) and not guard_report.ok,
        "; ".join(guard_report.guard_errors) or "guard_errors 为空（假绿！）",
    )

    # 覆盖范围断言：删掉 system/ 目录 → coverage_errors 必须非空
    root3 = base / "coverage"
    write_fixture(root3, {})
    for path in (root3 / "system").rglob("*.rs"):
        path.unlink()
    for path in (root3 / "system").rglob("*"):
        if path.is_dir():
            path.rmdir()
    (root3 / "system").rmdir()
    coverage_report = run_gate(root3)
    check(
        "⑨ 扫描范围断言：缺 system/ 目录 → coverage_errors 非空",
        bool(coverage_report.coverage_errors),
        "; ".join(coverage_report.coverage_errors) or "coverage_errors 为空（假绿！）",
    )

    # --- R1 硬化：一句 #[cfg(test)] #[path=...] 把别的生产文件挂成测试面 → 跳过清单断言必须 FAIL ---
    root4 = base / "r1_ghost_path"
    write_fixture(root4, {})
    ghost_target = root4 / "business/task/zz_d.rs"
    ghost_target.write_text(
        'pub fn q() {\n    let _sql = "SELECT * FROM sys_notice";\n}\n', encoding="utf-8"
    )
    task_mod = root4 / "business/task/mod.rs"
    task_mod.write_text(task_mod.read_text(encoding="utf-8") + "\nmod zz_d;\n", encoding="utf-8")
    gate_file = root4 / "business/erp/approval_gate.rs"
    gate_file.write_text(
        'pub fn ok() {}\n\n#[cfg(test)]\n#[path = "../task/zz_d.rs"]\nmod ghost_copy;\n',
        encoding="utf-8",
    )
    # 反事实：不做清单断言时，这条幽灵声明确实能让闸② 变绿（证明断言不是摆设）
    ghost_unguarded = run_gate(root4, None)
    ghost_report = run_gate(root4, FIXTURE_SKIP_MANIFEST)
    check(
        "⑩ 跳过清单断言：幽灵 #[path] 把生产文件藏进测试面 → FAIL（反事实对照：旧行为会变绿）",
        bool(ghost_report.skip_manifest_errors)
        and not ghost_report.ok
        and "business/task/zz_d.rs" not in ghost_unguarded.failures,
        "; ".join(ghost_report.skip_manifest_errors) or "skip_manifest_errors 为空（假绿！）",
    )

    # --- R2 硬化：非 mod.rs 父文件的 #[cfg(test)] mod 子模块不许连坐跳过同目录的生产文件 ---
    root5 = base / "r2_child_path"
    write_fixture(root5, {})
    (root5 / "business/task/zz_child.rs").write_text(
        'pub fn q() {\n    let _sql = "SELECT * FROM sys_notice";\n}\n', encoding="utf-8"
    )
    task_mod = root5 / "business/task/mod.rs"
    task_mod.write_text(task_mod.read_text(encoding="utf-8") + "\nmod zz_child;\n", encoding="utf-8")
    # task/model.rs 的 `#[cfg(test)] mod zz_child;` 按 Rust 语义是 task::model::zz_child
    # → business/task/model/zz_child.rs，与生产文件 business/task/zz_child.rs 无关
    (root5 / "business/task/model.rs").write_text(
        "#[cfg(test)]\nmod zz_child;\n", encoding="utf-8"
    )
    path_report = run_gate(root5, FIXTURE_SKIP_MANIFEST)
    check(
        "⑪ 子模块路径按 Rust 语义：foo.rs 的 #[cfg(test)] mod 不连坐跳过 foo/x.rs（R2）",
        "business/task/zz_child.rs" in path_report.failures
        and "business/task/zz_child.rs" not in path_report.skipped
        and path_report.skip_manifest_errors == [],
        f"failures={sorted(path_report.failures)} skipped={sorted(path_report.skipped)}",
    )

    # --- R3 硬化：生产查询与 #[cfg(test)] mod 写在同一行 → 生产代码仍要判 ---
    root6 = base / "r3_same_line"
    write_fixture(root6, {})
    (root6 / "business/task/zz_same_line.rs").write_text(
        'pub fn q() { let _sql = "SELECT * FROM sys_notice"; } #[cfg(test)] mod tests { fn t() {} }\n',
        encoding="utf-8",
    )
    task_mod = root6 / "business/task/mod.rs"
    task_mod.write_text(task_mod.read_text(encoding="utf-8") + "\nmod zz_same_line;\n", encoding="utf-8")
    same_line_report = run_gate(root6, FIXTURE_SKIP_MANIFEST)
    check(
        "⑫ 字符级跳过：同一行里的生产查询不被 #[cfg(test)] mod 整行吞掉（R3）",
        "business/task/zz_same_line.rs" in same_line_report.failures,
        f"failures={sorted(same_line_report.failures)}",
    )

    print(
        f"闸② 自测结论：{'全部通过' if failures == 0 else f'{failures} 项不符合期望'}"
        f"（临时目录保留供复核：{base}）"
    )
    return 0 if failures == 0 else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="门禁 闸②：登记表查询必须有 scope:: 或 SCOPE-EXEMPT")
    parser.add_argument("--root", default=None, help="仓库根（默认由脚本位置推断）")
    parser.add_argument("--src", default=None, help="源码目录（默认 <root>/backend/src）")
    parser.add_argument("--list-skipped", action="store_true", help="打印被跳过的测试面文件清单")
    parser.add_argument("--baseline", type=int, default=None, help="历史基线数字（仅用于提示文案）")
    parser.add_argument("--self-test", action="store_true", help="负例自测（D6）")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    repo_root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parents[2]
    src = Path(args.src).resolve() if args.src else repo_root / "backend" / "src"
    if not src.is_dir():
        print(f"闸② 跳过/失败：找不到源码目录 {src}", file=sys.stderr)
        return 2

    try:
        report = run_gate(src, SKIP_MANIFEST)
    except FileNotFoundError as error:
        print(f"闸② 跳过/失败：{error}", file=sys.stderr)
        return 2

    print_report(report, args.list_skipped, args.baseline)
    return 0 if report.ok else 1


if __name__ == "__main__":
    sys.exit(main())
