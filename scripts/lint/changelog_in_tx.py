#!/usr/bin/env python3
"""门禁 G5（口径 v2）：`changelog::record(_in)` 的每个调用点都必须有**事务证据**。

依据 `docs/批次C-技术架构.md` §4.12.4 的 G5 行 + `docs/门禁口径-v2-20261103.md` 的 D4/D5。

## 判定面（D4 = 只认 changelog 写入入口）

1. 只匹配限定名 `changelog::record(` / `changelog::record_in(`；
2. 保留 `business/changelog/` 模块内部对 `record` / `record_in` 的未限定调用
   （模块内没有 `db()`，由 G2 兜底）；
3. 忽略方法调用（匹配位置前一个非空字符是 `.`，如缓存闸门的 `TREE_CACHE_GATE.record(true)`）；
4. 忽略同文件本地 `fn record(` 遮蔽下的裸调用（如 `business/flow/linkage/engine.rs`），
   但**同文件里真实的 `changelog::record` 仍要查**；
5. 注释 / 字符串照旧用 `mask()` 处理。

## 事务证据（D5 = 看类型，不看名字）

- 调用点在 `db().transaction(|tx| async move { … })` 闭包体内，且首参含 `tx` → 合规；
- 首参是 `&Transaction` 类型的标识符（**裸标识符**，如 `changelog::record(ex, …)`，且该标识符在
  最近 enclosing `fn` 的形参表里声明为 `&Transaction` / `&mut Transaction` / `Transaction`）→ 合规；
- `let tx = conn.begin().await?` 之后的 `record(&tx, …)` → 合规（显式事务句柄）；
- `business/changelog/` 模块内部把 executor 原样透传（`record_in` → `record`）→ 合规；
- `db()` / `global()` / `try_db()` 出现在首参 → **硬拒**（那是第二个事务入口）。

> 为什么类型证据只对**裸标识符**成立：`tokio::spawn(async { changelog::record(&tx, …) })` 里的
> `&tx` 哪怕类型是 `&Transaction`，也可能在事务结束之后才执行 —— 这种「句柄越过事务边界」的写法
> 必须继续报红（D6 负例②）。裸标识符 `ex` 只可能是「当前函数拿到的那个事务句柄」，才是 D5 要放行的形态。

## 这个检查能证什么、不能证什么（诚实说明）

能证：调用点有**静态可读的事务证据**（上面的五种之一）。
不能证：运行时 `tx` 到底是不是业务事务本身（那靠 `tests/batchC_c7_rollback.rs` 的 `0|1|0`
真库断言兜底）。所以本脚本与 G2、T-CL-01 三者必须同时存在。

## 用法

    python3 scripts/lint/changelog_in_tx.py              # 默认扫 backend/src
    python3 scripts/lint/changelog_in_tx.py --root .     # 指定仓库根
    python3 scripts/lint/changelog_in_tx.py --list       # 顺带列出每个调用点的判定
    python3 scripts/lint/changelog_in_tx.py --self-test  # 负例自测（口径 v2 · D6）

退出码：0 = 全部合规；1 = 有违例 / 判定面疑似被改坏；2 = 用法/环境错误。
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path
from typing import NamedTuple

# 判定面：限定名调用（D4.1）
QUALIFIED_CALL_RE = re.compile(r"(?<![A-Za-z0-9_])changelog\s*::\s*(record|record_in)\s*\(")
# 模块内部裸调（D4.2）：前面不能是标识符字符、`.`（方法调用）、`:`（`Self::record`）
BARE_CALL_RE = re.compile(r"(?<![A-Za-z0-9_.:])(record|record_in)\s*\(")
# 本地同名函数定义（`fn record(` / `async fn record_in(`）：D4.4 的遮蔽来源
LOCAL_RECORD_DEF_RE = re.compile(r"\bfn\s+record(_in)?\s*\(")
# 事务闭包的「头」：`.transaction(|tx| async move {` / `.transaction(move |tx| {`
TX_HEAD_RE = re.compile(r"transaction\s*\(\s*(?:move\s*)?\|[^|]*\|(?:\s*async\s*move)?\s*$")
# 允许出现在第一个实参里的「事务句柄」写法（形参名叫 tx）
TX_ARG_RE = re.compile(r"\btx\b")
# 模块内部把 executor 原样透传的写法（只对 business/changelog/ 生效）
PASS_THROUGH_ARG_RE = re.compile(r"^&?ex\b")
# 绝对不允许出现在第一个实参里的写法（第二个事务的入口）
FORBIDDEN_ARG = ("db()", "global()", "try_db()")
# D5 的类型证据只认**裸标识符**首参（`&tx` 这类仍按事务上下文判定）
IDENT_ARG_RE = re.compile(r"^[A-Za-z_]\w*$")
# 形参类型里的 Transaction（允许 `&` / `&mut` / 模块路径前缀）
TRANSACTION_TYPE_RE = re.compile(r"(?:^|[\s&])(?:mut\s+)?(?:[A-Za-z_]\w*\s*::\s*)*Transaction\b")
FN_DEF_RE = re.compile(r"\bfn\s+([A-Za-z_]\w*)\s*(?:<[^{;(]*>)?\s*\(")
# 真实调用的下限（口径 v2 验收 2：N ≥ 27）。正则被改坏 → 0 命中假绿 → 直接判 FAIL
MIN_CALL_SITES = 27


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


def parse_params(params_text: str) -> dict[str, str]:
    """解析形参表 → {形参名: 类型原文}（`&self` / 模式参数会被跳过）。"""
    params: dict[str, str] = {}
    for part in split_top_level(params_text):
        item = part.strip()
        if not item:
            continue
        item = re.sub(r"#\[[^\]]*\]", " ", item).strip()
        item = re.sub(r"^mut\s+", "", item)
        if ":" not in item:
            continue  # `&self` / `self`
        name, _, typ = item.partition(":")
        name = name.strip()
        if re.fullmatch(r"[A-Za-z_]\w*", name):
            params[name] = typ.strip()
    return params


class FnInfo(NamedTuple):
    name: str
    body_start: int
    body_end: int
    params: dict[str, str]


def parse_functions(masked: str) -> list[FnInfo]:
    """扫出所有带函数体的 `fn`（函数体范围 + 形参表），供 D5 的「最近 enclosing fn」使用。"""
    out: list[FnInfo] = []
    for match in FN_DEF_RE.finditer(masked):
        paren_open = match.end() - 1
        paren_end = match_balanced(masked, paren_open)
        if paren_end is None:
            continue
        params = parse_params(masked[paren_open + 1 : paren_end - 1])
        # 从形参表之后找函数体 `{`（先遇到 `;` 说明是 trait 声明，没有函数体）
        depth = 0
        index = paren_end
        body_start: int | None = None
        while index < len(masked):
            ch = masked[index]
            if ch in "([":
                depth += 1
            elif ch in ")]":
                depth -= 1
            elif ch == "{" and depth == 0:
                body_start = index
                break
            elif ch == ";" and depth == 0:
                break
            index += 1
        if body_start is None:
            continue
        body_end = match_balanced(masked, body_start, "{", "}") or len(masked)
        out.append(FnInfo(match.group(1), body_start, body_end, params))
    return out


def enclosing_fn(functions: list[FnInfo], pos: int) -> FnInfo | None:
    """包含 `pos` 的最内层 `fn`。"""
    best: FnInfo | None = None
    for info in functions:
        if info.body_start < pos < info.body_end:
            if best is None or info.body_start > best.body_start:
                best = info
    return best


class Site(NamedTuple):
    name: str
    line: int
    arg: str
    kind: str  # qualified / bare
    verdict: str  # tx_closure / typed_transaction / begin_handle / module_passthrough
    detail: str


def is_definition(masked: str, pos: int) -> bool:
    """`fn record(` / `async fn record_in(` 是定义，不是调用点。"""
    return bool(re.search(r"\bfn\s+$", masked[max(0, pos - 16) : pos]))


def check_file(path: Path, repo_root: Path, verbose: bool) -> tuple[list[str], int, list[str]]:
    """返回 (违例列表, 本文件判定过的调用点数, 提示信息)。"""
    source = path.read_text(encoding="utf-8")
    masked = mask(source)
    blocks = block_kinds(masked)
    functions = parse_functions(masked)
    rel = path.relative_to(repo_root)
    rel_posix = rel.as_posix()
    inside_module = "business/changelog/" in rel_posix
    local_record_def = bool(LOCAL_RECORD_DEF_RE.search(masked))

    # 判定面（D4）：限定名 + 模块内裸调（裸调对本地 fn record 遮蔽的情况另行提示，不计入）
    raw_sites: list[tuple[int, str, str]] = []
    for match in QUALIFIED_CALL_RE.finditer(masked):
        if is_definition(masked, match.start()):
            continue
        raw_sites.append((match.start(), match.group(1), "qualified"))
    if inside_module:
        for match in BARE_CALL_RE.finditer(masked):
            if is_definition(masked, match.start()):
                continue
            raw_sites.append((match.start(), match.group(1), "bare"))
    raw_sites.sort()

    notes: list[str] = []
    # 可审计：非 changelog 模块里的裸调不在判定面（D4.1），若该文件没有本地 fn record 遮蔽
    # 提醒一句「可能是 use changelog::record 导入的裸调，请改成限定名」——只提示，不判红。
    if not inside_module:
        for match in BARE_CALL_RE.finditer(masked):
            if is_definition(masked, match.start()):
                continue
            line = masked.count("\n", 0, match.start()) + 1
            if local_record_def:
                if verbose:
                    print(f"  skip {rel}:{line} 裸调 {match.group(1)}(…)（本地 fn record 遮蔽，D4.4）")
            else:
                notes.append(
                    f"{rel}:{line}: 裸调 {match.group(1)}(…) 不在判定面（D4 只认限定名），"
                    "且本文件没有本地 fn record —— 若是 use 导入的裸调，请改写成 changelog::record"
                )

    problems: list[str] = []
    sites = 0
    for pos, name, kind in raw_sites:
        paren = masked.index("(", pos + len(name))
        line = masked.count("\n", 0, pos) + 1
        sites += 1
        arg = first_argument(masked, paren)
        scope = enclosing(blocks, pos)
        in_tx = any(kind_ == "tx" for _, _, kind_ in scope)
        arg_bad = [token for token in FORBIDDEN_ARG if token in arg]
        arg_has_tx = bool(TX_ARG_RE.search(arg))
        # 显式事务对象形态：`let tx = conn.begin().await?; … record(&tx, …)`
        explicit_tx = arg_has_tx and has_begin_handle(masked, scope, pos)
        # D5：裸标识符 + 最近 enclosing fn 的形参声明为 Transaction 类型
        typed_transaction = False
        typed_detail = ""
        if IDENT_ARG_RE.fullmatch(arg):
            info = enclosing_fn(functions, pos)
            if info is not None and arg in info.params:
                param_type = info.params[arg]
                if TRANSACTION_TYPE_RE.search(param_type):
                    typed_transaction = True
                    typed_detail = f"{info.name}({arg}: {param_type})"
        # 模块内部透传：`record_in` 把收到的 executor（`ex`）原样交给 `record`
        module_passthrough = inside_module and bool(PASS_THROUGH_ARG_RE.search(arg))

        if arg_bad:
            problems.append(
                f"{rel}:{line}: {name}({arg[:60]}) 第一个实参含 {'/'.join(arg_bad)}"
                "——这会开第二个事务，回滚时留下假日志"
            )
        elif in_tx and (arg_has_tx or typed_transaction):
            if verbose:
                print(f"  ok  {rel}:{line} {name}(…) 在 transaction 闭包内")
        elif typed_transaction:
            # D5：类型级证据（比「变量名必须叫 tx」更强）
            if verbose:
                print(f"  ok  {rel}:{line} {name}({arg}) 形参声明为 Transaction：{typed_detail}")
        elif explicit_tx:
            if verbose:
                print(f"  ok  {rel}:{line} {name}(…) 使用 begin() 创建的显式事务句柄")
        elif module_passthrough:
            if verbose:
                print(f"  ok  {rel}:{line} {name}({arg[:60]}) 模块内部透传 executor")
        elif in_tx:
            problems.append(
                f"{rel}:{line}: {name}({arg[:60]}) 第一个实参里看不到 tx / Transaction 类型，"
                "无法证明是同事务执行器"
            )
        else:
            problems.append(
                f"{rel}:{line}: {name}({arg[:60]}) 不在任何 transaction(|tx| ...) 闭包内，"
                "首参也不是 Transaction 类型的形参"
                "（也不在 begin() 创建的显式事务句柄负责的范围里）"
            )
    return problems, sites, notes


def coverage_ok(call_sites: int) -> bool:
    """防假绿：判定面太小说明正则/过滤被改坏了（0 命中假绿与漏报同样不可接受）。"""
    return call_sites >= MIN_CALL_SITES


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="门禁 G5：record 调用点必须有事务证据")
    parser.add_argument("--root", default=None, help="仓库根目录（默认由脚本位置推断）")
    parser.add_argument("--src", default=None, help="源码目录（默认 <root>/backend/src）")
    parser.add_argument("--list", action="store_true", help="列出每个调用点的判定")
    parser.add_argument("--self-test", action="store_true", help="负例自测（口径 v2 · D6）")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

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
    notes: list[str] = []
    checked = 0
    call_sites = 0
    for path in files:
        found, sites, file_notes = check_file(path, repo_root, args.list)
        problems.extend(found)
        notes.extend(file_notes)
        call_sites += sites
        checked += 1

    for note in notes:
        print(f"  提示：{note}")

    if not coverage_ok(call_sites):
        print(
            f"G5 FAIL：只检查到 {call_sites} 个调用点（口径要求 ≥ {MIN_CALL_SITES}）"
            "—— 判定面/正则可能被改坏了，0 命中假绿同样判 FAIL"
        )
        return 1

    if problems:
        print(f"G5 FAIL：{len(problems)} 个调用点不合规（检查了 {call_sites} 个调用点，扫了 {checked} 个文件）")
        for item in problems:
            print(f"  {item}")
        print(
            "  修法：把 changelog::record(_in) 放进 db().transaction(|tx| async move { … }) 里、第一个实参传 &tx；"
            "或让首参是形参声明为 &Transaction 的事务句柄"
        )
        return 1
    print(
        f"G5 PASS：检查了 {call_sites} 个调用点（changelog::record / changelog::record_in），"
        f"全部有事务证据（扫了 {checked} 个文件）"
    )
    return 0


# ---------------------------------------------------------------------------
# 负例自测（口径 v2 · D6）：在临时目录写样例代码，走**同一个** check_file 判定
# ---------------------------------------------------------------------------

SELF_TEST_CASES: tuple[dict, ...] = (
    {
        "name": "① 函数顶层 changelog::record(db(), …) → 报红",
        "file": "backend/src/case_top_level_db.rs",
        "source": """
use crate::db::Db;

pub async fn record_something(target: ChangeTarget<'_>) -> Result<(), ApiError> {
    changelog::record(db(), &ctx, target, input).await?;
    Ok(())
}
""",
        "problems": 1,
        "sites": 1,
        "problem_contains": "db()",
    },
    {
        "name": "② tokio::spawn(async { changelog::record(&tx, …) }) → 报红（不在 transaction 闭包内）",
        "file": "backend/src/case_spawn.rs",
        "source": """
pub async fn spawn_task(tx: &Transaction, ctx: &ChangeCtx) -> Result<(), ApiError> {
    tokio::spawn(async move {
        changelog::record(&tx, ctx, target, input).await;
    });
    Ok(())
}
""",
        "problems": 1,
        "sites": 1,
        "problem_contains": "不在任何 transaction",
    },
    {
        "name": "③ transaction(|tx| async move { changelog::record(db(), …) }) → 报红（第二个事务入口）",
        "file": "backend/src/case_tx_db.rs",
        "source": """
pub async fn run(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    db.transaction(|tx| async move {
        changelog::record(db(), ctx, target, input).await
    })
    .await
}
""",
        "problems": 1,
        "sites": 1,
        "problem_contains": "db()",
    },
    {
        "name": "④ 本地 fn record 遮蔽 + 裸调不报红，同文件 changelog::record 在事务外仍报红",
        "file": "backend/src/case_shadow.rs",
        "source": """
fn record(rule: &str, outcome: &str) -> u8 {
    1
}

pub async fn run(conn: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    record("rule", "ok");
    record("rule2", "ok");
    changelog::record(conn, ctx, target, input).await?;
    Ok(())
}
""",
        "problems": 1,
        "sites": 1,
        "problem_line": 8,  # changelog::record(conn, …) 所在行；两个裸调 record(…) 在 6/7 行不得报红
    },
    {
        "name": "⑤ fn f(ex: &Transaction, …) 里 changelog::record(ex, …) → 判绿（D5 类型证据）",
        "file": "backend/src/case_typed_param.rs",
        "source": """
pub async fn cancel_tasks(ex: &Transaction, ctx: &ChangeCtx) -> Result<(), ApiError> {
    for victim in &victims {
        changelog::record(ex, ctx, target, input).await?;
    }
    Ok(())
}
""",
        "problems": 0,
        "sites": 1,
    },
    {
        "name": "⑥ 缓存闸门方法调用 GATE.record(true) 不计入判定面（28 处误报的回归防护）",
        "file": "backend/src/case_method_calls.rs",
        "source": """
pub fn bump(gate: &InvalidationGate) {
    TREE_CACHE_GATE.record(true);
    GATE.record(false);
    invalidation.record(true);
}
""",
        "problems": 0,
        "sites": 0,
    },
    {
        "name": "⑦ changelog 模块内部未限定调用 record(ex, …) 保留在判定面且透传判绿",
        "file": "backend/src/business/changelog/mod.rs",
        "source": """
pub async fn record(ex: &impl SqlExecutor, ctx: &ChangeCtx) -> ApiError {}

pub async fn record_in(ex: &impl SqlExecutor, ctx: &ChangeCtx) -> ApiError {
    record(ex, ctx, target, input).await.map_err(|error| error)
}
""",
        "problems": 0,
        "sites": 1,
    },
    {
        "name": "⑧ 注释里的 changelog::record(db(), …) 不判红（mask 生效）",
        "file": "backend/src/case_comment.rs",
        "source": """
//! 不要写 changelog::record(db(), …)（这是文档，不是代码）
// changelog::record(db(), &ctx, target, input);
pub fn ok() {}
""",
        "problems": 0,
        "sites": 0,
    },
    {
        "name": "⑨ 事务闭包内 &tx 判绿（原有合规形态不能回退）",
        "file": "backend/src/case_in_tx.rs",
        "source": """
pub async fn run(db: &Db, ctx: &ChangeCtx) -> Result<(), ApiError> {
    db.transaction(|tx| async move {
        changelog::record(&tx, ctx, target, input).await
    })
    .await
}
""",
        "problems": 0,
        "sites": 1,
    },
)


def self_test() -> int:
    base = Path(tempfile.mkdtemp(prefix="g5_selftest_"))
    print(f"G5 负例自测（临时目录 {base}）")
    failures = 0

    def check(label: str, condition: bool, detail: str = "") -> None:
        nonlocal failures
        print(f"  {'PASS' if condition else 'FAIL'}  {label}{('  ' + detail) if detail else ''}")
        if not condition:
            failures += 1

    for case in SELF_TEST_CASES:
        path = base / case["file"]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(case["source"].lstrip("\n"), encoding="utf-8")
        problems, sites, _notes = check_file(path, base, False)
        ok = len(problems) == case["problems"] and sites == case["sites"]
        if ok and case.get("problem_contains"):
            ok = any(case["problem_contains"] in item for item in problems)
        if ok and case.get("problem_line"):
            ok = any(f":{case['problem_line']}:" in item for item in problems)
        detail = f"problems={len(problems)} sites={sites}"
        if problems:
            detail += " | " + problems[0][:100]
        check(case["name"], ok, detail)

    # 防假绿下限：判定面被改坏（0 命中 / 少于 27）必须判 FAIL
    check("⑩ 防假绿：调用点数 0 → coverage_ok=False", not coverage_ok(0))
    check(f"⑪ 防假绿：调用点数 {MIN_CALL_SITES - 1} → coverage_ok=False", not coverage_ok(MIN_CALL_SITES - 1))
    check(f"⑫ 防假绿：调用点数 {MIN_CALL_SITES} → coverage_ok=True", coverage_ok(MIN_CALL_SITES))

    print(
        f"G5 自测结论：{'全部通过' if failures == 0 else f'{failures} 项不符合期望'}"
        f"（临时目录保留供复核：{base}）"
    )
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
