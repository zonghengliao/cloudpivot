#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""库存三本账「唯一写入口」闸（P2-5A 收口 / P2-5B 门禁加固）。

判据：`backend/src` 下**非测试**代码里，除了
`backend/src/business/erp/inventory_writer.rs` 之外，任何文件都不许出现对
`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 的写语句
（`INSERT INTO` / `UPDATE` / `DELETE FROM` / `TRUNCATE` / `COPY`）。

为什么要有这道闸：收口前 `biz_inventory` 有 13 个文件直写、`biz_inventory_location`
13 个、`biz_inventory_txn` 8 个 —— 同一件事在 N 个文件里各写一遍 SQL，任何一处的
`ROUND` 位数 / 夹 0 / 归零口径漂移都不会立刻报错，只在巡检里变成「账不对」。
唯一写入口 + 静态闸，才能保证「口径只有一份」。

判据必须扛住「换个写法就漏过」（P2-5B 实测漏过的三种，均已封）：
  * 跨行 SQL：`UPDATE\\n    biz_inventory SET …`（旧实现逐行匹配 → 漏）；
  * schema 限定：`UPDATE public.biz_inventory …` / `UPDATE {schema}.biz_inventory`（漏）；
  * 加引号标识符：`UPDATE "biz_inventory" …`（漏）。
另外 `#[cfg(test)]` 的豁免按**花括号配对**只跳过测试项本身，不再是「从这里到文件末尾
全豁免」（旧实现下，往测试模块后面追加一个直写函数就能悄悄蒙过去）。

豁免（都是**故意的**，不是漏网）：
  * 测试文件（按**明确命名**判定，见 TEST_NAME_RE）：夹具必须能**直写脏数据**来造
    「台账被人手工改过」这类场景（例如 `UPDATE biz_inventory SET qty = 999` 去验巡检
    能抓），走写入口就造不出来了；
  * 文件内 `#[cfg(test)]` 标记的模块 / 函数（连同其花括号体）；
  * 注释（`//…`、`/* … */`）：文档里提到 SQL 不算写入。
注意：**文件名只是"恰好包含 test 子串"不再豁免**（`latest_stock.rs`、`attestation.rs`
这类会被正常扫描）—— 否则改个文件名就能绕过闸门。

用法：
    python3 scripts/verify/inventory_write_gate.py            # 明细 + 退出码
    python3 scripts/verify/inventory_write_gate.py --json     # 给 CI 解析
    python3 scripts/verify/inventory_write_gate.py --self-test # 闸门自检（红/绿语料）
退出码：0 = 干净；1 = 有越界写入（或自检不通过）。
"""
import argparse
import bisect
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "backend" / "src"
WRITER = SRC / "business" / "erp" / "inventory_writer.rs"

# 写法：<写关键字> [schema.] 表名，表名可选双引号。
# 注意：表名捕获组**只能出现一次** —— 早先按分支各写一个 `_TABLE` 会让 group(1) 只在
# INSERT 分支上有值（其余分支返回 None），所以关键字收敛成一个非捕获前缀。
# `(?!\w)` 收尾用于排除 `biz_inventory_po` 这类非目标表。
WRITE_RE = re.compile(
    r"(?is)\b(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM|COPY|TRUNCATE(?:\s+TABLE)?)\s+"
    r'(?:(?:[A-Za-z_][\w$]*|"[^"]*"|\{[^}]*\})\s*\.\s*)*'
    r'"?(?P<table>biz_inventory(?:_location|_txn)?)"?'
    r"(?!\w)"
)

# `#[cfg(test)]` / `#[cfg(all(test, …))]`
CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\(\s*(?:all\(\s*)?test\b")

# 明确的测试文件命名（整串匹配文件名）。**不用** "test 子串" 判据：见模块 docstring。
TEST_NAME_RE = re.compile(
    r"^(?:"
    r"tests?\.rs"
    r"|tests_support\.rs"
    r"|test_.*\.rs"
    r"|.*_tests?\.rs"
    r"|.*_tests?_.*\.rs"
    r"|.*_test_support\.rs"
    r"|.*_test\.rs"
    r")$"
)

# Rust 字符字面量 `'a'` / `'\n'`（避免把 `&'a str` 的生命周期当成字符串开头）
CHAR_LIT_RE = re.compile(r"'(?:\\.|[^\\'])'")


def _blank(buf, start, end):
    """把 [start, end) 区间抹成空格（保留换行，让偏移/行号不变）。"""
    for i in range(start, end):
        if buf[i] != "\n":
            buf[i] = " "


def strip_comments(text):
    """把 Rust 注释抹成空格（字符串字面量原样保留，偏移保持不变）。

    为什么要自己做而不是逐行 `startswith("//")`：SQL 都在字符串里，必须保留字符串；
    而 `/* UPDATE biz_inventory */` 这种块注释又必须抹掉，否则误报。
    """
    buf = list(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            _blank(buf, i, j)
            i = j
        elif two == "/*":
            # Rust 块注释可嵌套
            depth, j = 1, i + 2
            while j < n and depth:
                if text[j:j + 2] == "/*":
                    depth, j = depth + 1, j + 2
                elif text[j:j + 2] == "*/":
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            _blank(buf, i, j)
            i = j
        elif c == "r" and re.match(r'r#*"', text[i:i + 8] or ""):
            # 原始字符串 r"…" / r#"…"#：内容原样保留，跳到结束引号之后
            m = re.match(r'(r)(#*)(")', text[i:i + 8])
            if m is None:  # pragma: no cover - 上面 if 已保证匹配
                i += 1
                continue
            hashes = m.group(2)
            close = '"' + hashes
            j = text.find(close, i + len(m.group(0)))
            i = n if j < 0 else j + len(close)
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    # Rust 源码里字符串内的双引号写作 `\"`。把它就地替换成 `" `（长度不变，
                    # 偏移不变），否则 `UPDATE public.\"biz_inventory\"` 这种最贴近真实的
                    # 写法会因为反斜杠而漏判。
                    if j + 1 < n and text[j + 1] == '"':
                        buf[j], buf[j + 1] = " ", '"'
                    j += 2
                elif text[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            i = j
        elif c == "'":
            m = CHAR_LIT_RE.match(text, i)
            i = m.end() if m else i + 1
        else:
            i += 1
    return "".join(buf)


def _cfg_test_spans(clean):
    """返回 `#[cfg(test)]` 标记的项所占的 [(start, end)] 区间（按花括号配对）。"""
    spans = []
    for m in CFG_TEST_RE.finditer(clean):
        i, n = m.end(), len(clean)
        # 跳过属性与空白，定位项本体
        while i < n and (clean[i].isspace() or clean[i] == "]"):
            i += 1
            if clean[i - 1] == "]":
                break
        start = m.start()
        # 情形 A：`mod tests;` 这类无花括号项 —— 到分号为止
        j = i
        while j < n and clean[j] not in "{;":
            j += 1
        if j < n and clean[j] == ";":
            spans.append((start, j + 1))
            continue
        # 情形 B：带花括号的项 —— 配对大括号
        if j >= n:
            spans.append((start, n))
            continue
        depth, k = 0, j
        while k < n:
            if clean[k] == "{":
                depth += 1
            elif clean[k] == "}":
                depth -= 1
                if depth == 0:
                    k += 1
                    break
            k += 1
        spans.append((start, k))
    return spans


def is_test_file(path):
    if any(part == "tests" for part in path.parts[:-1]):
        return True
    return bool(TEST_NAME_RE.match(path.name))


def scan_source(name, text):
    """扫描一段 Rust 源码，返回 [(行号, 表名, 代码片段)]，只报**非测试**代码里的写语句。

    `name` 只用于判定是否是测试文件（测试文件整体豁免）。
    """
    if is_test_file(Path(name)):
        return []
    clean = strip_comments(text)
    spans = _cfg_test_spans(clean)
    line_starts = [0] + [i + 1 for i, ch in enumerate(clean) if ch == "\n"]

    hits = []
    for m in WRITE_RE.finditer(clean):
        pos = m.start()
        if any(a <= pos < b for a, b in spans):
            continue
        lineno = bisect.bisect_right(line_starts, pos)
        snippet = re.sub(r"\s+", " ", m.group(0)).strip()
        hits.append((lineno, m.group("table"), snippet))
    return hits


def scan_file(path):
    return scan_source(str(path), path.read_text(encoding="utf-8"))


# ---------------------------------------------------------------------------
# 自检语料：每条都必须按 expectation 红/绿。这是「故意加个直写能抓」的可重复版本，
# 也是防止以后有人把判据改回去的回归网。
# ---------------------------------------------------------------------------
SELF_TEST_CASES = [
    # (文件名, 源码, 期望是否报红, 说明)
    ("ok.rs", 'sqlx::query("UPDATE biz_inventory SET qty = 0");', True, "基线 UPDATE"),
    ("ok.rs", 'sqlx::query("insert into biz_inventory(x) values($1)");', True, "小写 INSERT"),
    ("ok.rs", 'sqlx::query("DELETE FROM biz_inventory_location WHERE id=$1");', True, "DELETE 库位账"),
    ("ok.rs", 'sqlx::query("UPDATE biz_inventory_txn SET qty=0");', True, "UPDATE 流水"),
    ("ok.rs", 'sqlx::query("UPDATE public.biz_inventory SET qty = 0");', True, "schema 限定 public."),
    ("ok.rs", 'sqlx::query("UPDATE {schema}.biz_inventory SET qty = 0");', True, "schema 占位符"),
    ('ok.rs', 'sqlx::query(r#"UPDATE "biz_inventory" SET qty = 0"#);', True, "双引号标识符"),
    ("ok.rs", 'sqlx::query("UPDATE\n        biz_inventory SET qty = 0");', True, "跨行（关键字后折行）"),
    ("ok.rs", 'sqlx::query("INSERT\n   INTO biz_inventory(qty)\n   VALUES($1)");', True, "跨行 INSERT"),
    ("ok.rs",
     'fn f(){ let _p = "UPDATE public.\\"biz_inventory\\"\\n        SET qty = 0"; }',
     True, "源码里的转义引号 \\\" + 跨行 + 限定符"),
    ("ok.rs", 'sqlx::query("TRUNCATE TABLE biz_inventory");', True, "TRUNCATE"),
    ("ok.rs", 'sqlx::query("COPY biz_inventory FROM STDIN");', True, "COPY"),
    ("ok.rs", '// 文档里提 UPDATE biz_inventory 不算\nfn f() {}', False, "行注释"),
    ("ok.rs", '/* UPDATE biz_inventory SET qty=0 */\nfn f() {}', False, "块注释"),
    ("ok.rs", 'sqlx::query("UPDATE biz_inventory_po SET x=1");', False, "非目标表 biz_inventory_po"),
    ("ok.rs", 'sqlx::query("SELECT * FROM biz_inventory");', False, "只读查询"),
    ("mod.rs", '#[cfg(test)]\nmod tests {\n    fn a(){ sqlx::query("UPDATE biz_inventory SET qty=0"); }\n}', False, "cfg(test) 模块"),
    ("mod.rs", '#[cfg(test)]\nmod tests;\n', False, "cfg(test) 外部模块"),
    ("mod.rs",
     '#[cfg(test)]\nmod tests {\n  fn a(){ sqlx::query("UPDATE biz_inventory SET qty=0"); }\n}\n'
     'fn leaked(){ sqlx::query("UPDATE biz_inventory SET qty=0"); }',
     True, "cfg(test) 模块之后的代码必须仍被抓（旧实现漏）"),
    ("stock_tests.rs", 'sqlx::query("UPDATE biz_inventory SET qty=999");', False, "测试文件按命名豁免"),
    ("latest_stock.rs", 'sqlx::query("UPDATE biz_inventory SET qty=0");', True,
     "名字含 test 子串的文件必须仍被抓（旧实现误豁免）"),
    ("attestation.rs", 'sqlx::query("UPDATE biz_inventory SET qty=0");', True,
     "attestation 含 test 子串也必须被抓"),
]


def self_test():
    bad = 0
    print("=" * 92)
    print("闸门自检：判据红/绿语料")
    print("=" * 92)
    for name, code, expect_red, label in SELF_TEST_CASES:
        hits = scan_source(name, code)
        got = bool(hits)
        ok = got == expect_red
        # 红例必须同时给出可读的 行号 + 表名：捕获组取错（例如返回 None）在这里会立刻暴露
        if got:
            well_formed = all(
                isinstance(t, str) and t.startswith("biz_inventory") and ln >= 1
                for ln, t, _ in hits
            )
            ok = ok and well_formed
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 期望{'红' if expect_red else '绿'} 实得{'红' if got else '绿'}  {label}")
    print("-" * 92)
    print(f"{len(SELF_TEST_CASES) - bad}/{len(SELF_TEST_CASES)} 通过" + ("" if not bad else f"；✗ {bad} 条不符"))
    return bad == 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--self-test", action="store_true", dest="self_test")
    args = ap.parse_args()

    if args.self_test:
        sys.exit(0 if self_test() else 1)

    violations = []
    scanned = 0
    exempt_files = []
    for path in sorted(SRC.rglob("*.rs")):
        if path == WRITER:
            continue
        if is_test_file(path):
            exempt_files.append(str(path.relative_to(ROOT)))
            continue
        scanned += 1
        for lineno, table, code in scan_file(path):
            violations.append(
                dict(file=str(path.relative_to(ROOT)), line=lineno, table=table, code=code)
            )

    writer_hits = scan_file(WRITER) if WRITER.exists() else []
    ok = not violations and WRITER.exists()

    if args.json:
        print(json.dumps(
            dict(ok=ok, scanned=scanned, exempt_files=len(exempt_files),
                 writer_statements=len(writer_hits), violations=violations),
            ensure_ascii=False, indent=2))
    else:
        print("=" * 92)
        print("库存三本账唯一写入口闸（P2-5A 收口 / P2-5B 门禁）")
        print(f"扫描 {scanned} 个非测试 .rs（豁免 {len(exempt_files)} 个测试文件）")
        print(f"唯一写入口 {WRITER.relative_to(ROOT)}：{len(writer_hits)} 条写语句")
        print("=" * 92)
        if not WRITER.exists():
            print(f"✗ 唯一写入口不存在：{WRITER}")
        if violations:
            print(f"\n✗ 有 {len(violations)} 处越界写入（必须改走 inventory_writer）：")
            for v in violations:
                print(f"  · {v['file']}:{v['line']}  [{v['table']}]  {v['code'][:110]}")
        else:
            print("\n★ 零越界：非测试代码里没有任何一处直写库存三本账。")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
