#!/usr/bin/env python3
"""门禁 G5：`changelog::record(_in)` 的每个调用点都必须在 `transaction(` 的词法范围内。

依据 `docs/批次C-技术架构.md` §4.12.4 的 G5 行：

    G5 | 每个 `record(` 调用点必须在 `transaction(` 的词法范围内
       | （这是唯一能自动化的「同事务」证据）

## 这个检查能证什么、不能证什么（诚实说明）

能证：

1. 调用点**在**某个 `.transaction(|tx| async move { ... })` 闭包体内
   （不是函数体顶层、不是 `tokio::spawn`、不是定时任务里裸调）；
2. 传给 `record(_in)` 的**第一个实参**里有 `tx`（`&tx` / `tx.as_ref()` 均可）,
   并且**没有** `db()` / `global()` —— 后者是「开第二个事务」的唯一入口。

不能证：运行时 `tx` 到底是不是业务事务本身（那要靠 `tests/batchC_c7_rollback.rs`
的 `0|1|0` 真库断言兜底）。所以本脚本与 G2、T-CL-01 三者必须同时存在。

## 用法

    python3 scripts/lint/changelog_in_tx.py            # 默认扫 backend/src
    python3 scripts/lint/changelog_in_tx.py --root .   # 指定仓库根
    python3 scripts/lint/changelog_in_tx.py --list     # 顺带列出每个调用点的判定

退出码：0 = 全部合规；1 = 有违例；2 = 用法/环境错误。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# 调用点的函数名。`record` 与 `record_in` 都要查：前者是底层实现，
# 后者是「事务闭包内」的封装，两者都是 handler 的直接调用对象。
CALL_RE = re.compile(r"(?<![A-Za-z0-9_])(?:changelog::)?(record|record_in)\s*\(")
# 函数定义（`async fn record_in(`）不是调用点。
DEF_RE = re.compile(r"\bfn\s+record(_in)?\s*$")
# 事务闭包的「头」：`.transaction(|tx| async move {` / `.transaction(move |tx| {`
TX_HEAD_RE = re.compile(
    r"transaction\s*\(\s*(?:move\s*)?\|[^|]*\|(?:\s*async\s*move)?\s*$"
)
# 允许出现在第一个实参里的「事务句柄」写法。
TX_ARG_RE = re.compile(r"\btx\b")
# 模块内部把 executor 原样透传的写法（只对 business/changelog/ 生效）。
PASS_THROUGH_ARG_RE = re.compile(r"^&?ex\b")
# 绝对不允许出现在第一个实参里的写法（第二个事务的入口）。
FORBIDDEN_ARG = ("db()", "global()", "try_db()")


def mask(source: str) -> str:
    """把注释与字符串字面量替换成等长空格，保留偏移。

    为什么必须 mask：本仓库的注释里大量出现 `db()`、`record(`、
    `transaction(|tx| ...)`（都是**讲道理**用的），不 mask 会把文档当成代码判红。
    等长替换保证报错的行号列号仍然指向真实文件位置。
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
        # 字符字面量（'x' / '\n'）：只处理「两三个字符就闭合」的形态，
        # 生命周期标注 'a 原样保留（它后面没有紧跟的闭引号）。
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


def block_kinds(masked: str) -> list[tuple[int, int, str]]:
    """扫描 `{}` 配平，返回 [(开括号偏移, 闭括号偏移, 类型)]，类型 ∈ {tx, other}。"""
    stack: list[int] = []
    blocks: list[tuple[int, int, str]] = []
    for index, ch in enumerate(masked):
        if ch == "{":
            stack.append(index)
        elif ch == "}" and stack:
            open_at = stack.pop()
            head = masked[max(0, open_at - 200) : open_at]
            kind = "tx" if TX_HEAD_RE.search(head) else "other"
            blocks.append((open_at, index, kind))
    # 未闭合的 `{`（文件被截断）按 other 处理，保证调用点仍能定位
    for open_at in stack:
        blocks.append((open_at, len(masked), "other"))
    return blocks


def enclosing(blocks: list[tuple[int, int, str]], pos: int) -> list[tuple[int, int, str]]:
    """包含 `pos` 的所有块，按「内 → 外」排序。"""
    hits = [b for b in blocks if b[0] < pos < b[1]]
    hits.sort(key=lambda b: b[0], reverse=True)
    return hits


def has_begin_handle(masked: str, scope: list[tuple[int, int, str]], pos: int) -> bool:
    """调用点之前，**同一个函数体内**是否出现过 `begin()`（显式事务句柄）。

    为什么需要这条：`Db::begin() -> Transaction` 是框架提供的另一种事务 API
    （`record_then_rollback` 用它，因为闭包式 `transaction()` 拿不到「事务内计数」）。
    它同样是真事务，所以 `let tx = conn.begin().await?; … record(&tx, …)` 应判绿；
    判据限定在**最外层包含块**（即函数体）内、且必须早于调用点，避免把
    「另一个函数里 begin 过」也算进来。
    """
    if not scope:
        return False
    outermost = scope[-1]
    window = masked[max(outermost[0], 0) : pos]
    return ".begin()" in window


def first_argument(masked: str, call_open_paren: int) -> str:
    """取调用的第一个实参原文（按顶层逗号切）。"""
    depth = 0
    start = call_open_paren + 1
    index = call_open_paren
    while index < len(masked):
        ch = masked[index]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth == 0:
                return masked[start:index].strip()
        elif ch == "," and depth == 1:
            return masked[start:index].strip()
        index += 1
    return masked[start:].strip()


def check_file(path: Path, repo_root: Path, verbose: bool) -> tuple[list[str], int]:
    """返回 (违例列表, 本文件里判定过的调用点数)。"""
    source = path.read_text(encoding="utf-8")
    masked = mask(source)
    blocks = block_kinds(masked)
    problems: list[str] = []
    sites = 0
    rel = path.relative_to(repo_root)
    inside_module = "business/changelog/" in str(rel).replace("\\", "/")
    for match in CALL_RE.finditer(masked):
        name = match.group(1)
        # 调用的 `(` 在 match 末尾
        paren = match.end() - 1
        # 函数定义不是调用点：`pub async fn record(` / `fn record_in(`
        head = masked[max(0, match.start() - 24) : match.start()]
        if DEF_RE.search(head) or re.search(r"\bfn\s+$", head):
            continue
        line = masked.count("\n", 0, match.start()) + 1
        sites += 1
        scope = enclosing(blocks, match.start())
        in_tx = any(kind == "tx" for _, _, kind in scope)
        arg = first_argument(masked, paren)
        arg_bad = [token for token in FORBIDDEN_ARG if token in arg]
        arg_has_tx = bool(TX_ARG_RE.search(arg))
        # 显式事务对象形态：`let tx = conn.begin().await?; … record(&tx, …)`
        # 闭包里没有 `transaction(` 字样，但句柄同样来自 `begin()`，语义等价。
        explicit_tx = arg_has_tx and has_begin_handle(masked, scope, match.start())
        # 模块内部透传：`record_in` 把收到的 executor（`ex`）原样交给 `record`。
        # 这只在 business/changelog/ 内部成立（模块里没有 db()，G2 兜底）。
        internal_pass = inside_module and bool(PASS_THROUGH_ARG_RE.search(arg))

        if in_tx:
            if arg_bad:
                problems.append(
                    f"{rel}:{line}: {name}({arg[:60]}) 第一个实参含 {'/'.join(arg_bad)}"
                    "——这会开第二个事务，回滚时留下假日志"
                )
            elif not arg_has_tx:
                problems.append(
                    f"{rel}:{line}: {name}({arg[:60]}) 第一个实参里看不到 tx，"
                    "无法证明是同事务执行器"
                )
            elif verbose:
                print(f"  ok  {rel}:{line} {name}(...) 在 transaction 闭包内")
        elif explicit_tx:
            if arg_bad:
                problems.append(
                    f"{rel}:{line}: {name}({arg[:60]}) 第一个实参含 {'/'.join(arg_bad)}"
                )
            elif verbose:
                print(f"  ok  {rel}:{line} {name}(...) 使用 begin() 创建的显式事务句柄")
        elif internal_pass:
            if verbose:
                print(f"  ok  {rel}:{line} {name}({arg[:60]}) 模块内部透传 executor")
        else:
            problems.append(
                f"{rel}:{line}: {name}(...) 不在任何 transaction(|tx| ...) 闭包内"
                "（也不在 begin() 创建的显式事务句柄负责的范围里）"
            )
    return problems, sites


def main() -> int:
    parser = argparse.ArgumentParser(description="门禁 G5：record 调用点必须在事务范围内")
    parser.add_argument("--root", default=None, help="仓库根目录（默认由脚本位置推断）")
    parser.add_argument("--src", default=None, help="源码目录（默认 <root>/backend/src）")
    parser.add_argument("--list", action="store_true", help="列出每个调用点的判定")
    args = parser.parse_args()

    repo_root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parents[2]
    src = Path(args.src).resolve() if args.src else repo_root / "backend" / "src"
    if not src.is_dir():
        print(f"G5 跳过/失败：找不到源码目录 {src}", file=sys.stderr)
        return 2

    files = sorted(src.rglob("*.rs"))
    if not files:
        print(f"G5 跳过/失败：{src} 下没有 .rs 文件", file=sys.stderr)
        return 2

    problems: list[str] = []
    checked = 0
    call_sites = 0
    for path in files:
        found, sites = check_file(path, repo_root, args.list)
        problems.extend(found)
        call_sites += sites
        checked += 1

    if problems:
        print(f"G5 FAIL：{len(problems)} 个调用点不合规（扫了 {checked} 个文件）")
        for item in problems:
            print(f"  {item}")
        print("  修法：把 record(_in) 放进 db().transaction(|tx| async move { … }) 里，第一个实参传 &tx")
        return 1
    print(f"G5 PASS：{call_sites} 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 {checked} 个文件）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
