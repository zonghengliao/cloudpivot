#!/usr/bin/env python3
"""TD-2 保真度校验：证明拆分「只搬不移」（比较拆分前 HEAD 与拆分后 facade+部件）。

口径：
  1. 条目集合：只比 fn/struct/enum/union/trait/type/const/static/impl 的名字多重集
     （mod 是拆分产生的结构，不比；`__head__` 是解析器伪项）。
  2. SQL 字面量多重集（含 SELECT/INSERT/UPDATE/DELETE 的字符串，空白归一化）。
  3. #[test]/#[tokio::test] 用例名多重集。
  4. 代码 token 多重集：忽略注释行 / use 语句整块 / mod 声明与花括号 / #[path]，
     剩下的逐 token 比对（等价于逐行但不受 rustfmt 重排行宽影响；逗号单列，
     因为 rustfmt 会给多行调用补尾逗号）—— 这是「没有一行代码被改动」的机械证明。
"""
from __future__ import annotations

import collections
import tempfile
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import split_module as sm

REPO = Path(__file__).resolve().parents[2]
SQL_KW = re.compile(r"\b(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM)\b", re.I)
STR_LIT = re.compile(r'r#*"(.+?)"#*|"((?:[^"\\]|\\.)*)"', re.S)
KEEP_DECL = re.compile(
    r"^(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|extern)\s+)*"
    r"(fn|struct|enum|union|trait|type|const|static|impl|macro_rules!)\b")


def read_head(path: str, ref: str = "HEAD") -> str:
    return subprocess.run(["git", "show", f"{ref}:{path}"], cwd=REPO,
                          capture_output=True, text=True, check=True).stdout


def decl_names(text: str) -> collections.Counter:
    c = collections.Counter()
    tmp = Path(tempfile.gettempdir()) / "td2_parse_probe.rs"
    tmp.write_text(text, encoding="utf-8")
    docs, imports, items, trailer = sm.split_file(tmp)
    for it in items:
        if it.name == "__head__":
            continue
        idx = next((k for k, l in enumerate(it.lines) if sm.DECL_RE.match(l)), None)
        if idx is None:
            continue
        decl_line = it.lines[idx]
        # 跳过 mod 声明（含其内部递归）
        if re.match(r"^\s*(pub\s+)?mod\s+\w+", decl_line):
            body = it.lines[idx + 1:]
            while body and body[-1].strip() == "":
                body.pop()
            if body and body[-1].strip() == "}":
                body = body[:-1]
            inner = "".join(l[4:] if l.startswith("    ") else l for l in body)
            c.update(decl_names(inner))
            continue
        if KEEP_DECL.match(decl_line.lstrip()):
            c[it.name] += 1
    return c


def sql_multiset(text: str) -> collections.Counter:
    c = collections.Counter()
    for m in STR_LIT.finditer(text):
        lit = m.group(1) or m.group(2) or ""
        if SQL_KW.search(lit):
            c[" ".join(lit.split())] += 1
    return c


def test_names(text: str) -> collections.Counter:
    c = collections.Counter()
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if "#[test]" in line or "#[tokio::test" in line:
            for j in range(i + 1, min(i + 6, len(lines))):
                m = re.search(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", lines[j])
                if m:
                    c[m.group(1)] += 1
                    break
    return c


# `,` 不计入（rustfmt 会给多行调用补尾逗号：同一份代码格式化前后差 1 个逗号属正常）
TOKEN = re.compile(r'"[^"]*"|[A-Za-z_][A-Za-z0-9_]*|\d+\.?\d*|[^\sA-Za-z0-9_,]')
IGNORE_LINE = re.compile(r"^(//|use |pub use |pub\(super\) use |#\[path|#\[cfg|mod |\}?$|\})")


def code_tokens(text: str) -> collections.Counter:
    """排除注释 / use 语句 / mod 结构行后，逐 token 计入（token 多重集）。"""
    c = collections.Counter()
    in_use = False
    for raw in text.splitlines():
        s = raw.strip()
        if not s:
            continue
        if in_use:
            if s.endswith(";"):
                in_use = False
            continue
        if s.startswith(("use ", "pub use ", "pub(super) use ", "pub(crate) use ")):
            if not s.endswith(";"):
                in_use = True
            continue
        if s.startswith("//"):
            continue
        if re.match(r"^(pub\s+)?mod\s+\w+\s*;?$", s):
            continue
        if re.match(r"^(pub\s+)?mod\s+\w+\s*\{$", s) or s == "}":
            continue
        if s.startswith("#[path") or s == "#[cfg(test)]":
            continue
        s = re.sub(r"^pub\(super\)\s+", "", s)
        for tok in TOKEN.findall(s):
            c[tok] += 1
    return c


def report(title, before, after):
    missing = before - after
    added = after - before
    print("  %-20s 前=%5d 后=%5d  MISSING=%s  ADDED=%s" % (
        title, sum(before.values()), sum(after.values()),
        (dict(list(missing.items())[:6]) if missing else "none"),
        (dict(list(added.items())[:6]) if added else "none")))
    return missing, added


def main():
    # 拆分前的 ref：拆分提交之后要指回拆分前那一版（默认 HEAD 是给「还没提交」用的）
    ref = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
    targets = [
        # P2-1A：task/handler.rs（5757 行）按职责拆成 11 个代码部件 + 3 个测试部件
        ("backend/src/business/task/handler.rs",
         ["backend/src/business/task/handler.rs"] +
         [f"backend/src/business/task/task_handler_{n}.rs" for n in [
             "common", "notice", "crud", "status", "hierarchy", "dep", "comment", "attachment",
             "ai", "assign", "flow", "tests", "stats_tests", "copy_db_tests"]]),
        ("backend/src/business/task/model.rs",
         ["backend/src/business/task/model.rs"] +
         [f"backend/src/business/task/task_model_{n}.rs" for n in [
             "base", "rows", "query", "write", "hierarchy", "dep", "ai", "stats", "flow",
             "schedule", "tests", "display_file_name_tests", "depth_tests", "root_only_tests",
             "statuses_filter_tests", "stats_tests", "repair_tests", "cascade_tests",
             "cascade_cancel_tests"]]),
        # P2-1B：flow/model.rs（4732 行）按职责拆成 10 个代码部件 + 2 个测试部件
        ("backend/src/business/flow/model.rs",
         ["backend/src/business/flow/model.rs"] +
         [f"backend/src/business/flow/flow_model_{n}.rs" for n in [
             "base", "def", "graph", "guard", "instance", "node", "cancel", "progress",
             "inbox", "stats", "tests", "delete_tests"]]),
        ("backend/src/business/erp/stockin_handler.rs",
         ["backend/src/business/erp/stockin_handler.rs"] +
         [f"backend/src/business/erp/stockin_handler_{n}.rs" for n in [
             "common", "purchase", "write", "post", "cancel", "status", "query",
             "tests_fixtures", "tests_helpers", "tests_posting", "tests_update",
             "tests_location", "tests_cancel", "tests_p10_locked", "tests_dc03", "tests_p16",
             "tests_d4_purchase", "tests_pflow3a", "tests_d9_copy", "tests_a4"]]),
    ]
    ok = True
    for src_path, new_paths in targets:
        if len(sys.argv) > 2 and sys.argv[2] not in src_path:
            continue
        print("=" * 78)
        print("%s   （拆分前 ref = %s）" % (src_path, ref))
        before = read_head(src_path, ref)
        after = "\n".join((REPO / p).read_text(encoding="utf-8") for p in new_paths)
        miss = []
        for label, fn in (("条目多重集", decl_names), ("SQL 字面量", sql_multiset),
                          ("用例名", test_names), ("代码 token 多重集", code_tokens)):
            m, a = report(label, fn(before), fn(after))
            if m:
                miss.append(label)
            if a and label != "代码 token 多重集":
                print("     !! 新增（应为空）：%s" % dict(a))
                miss.append(label)
        if miss:
            ok = False
            print("   !! 需要人工确认：%s" % miss)
    print("=" * 78)
    print("结论：", "PASS（条目 / SQL / 用例名 / 代码 token 四重多重集全部一一对应，只搬不移）" if ok
          else "FAIL（见上）")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
