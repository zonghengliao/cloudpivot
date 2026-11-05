#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""perm_menu_gate.py —— 死权限码常驻闸：**后端在用的权限码必须都能被授权出去**

## 为什么要有这道闸（不是猜，是有现场）

角色授权走的是「菜单树勾选」：`sys_role_menu` 只能挂到 `sys_menu` 的行上，而路由闸
`#[nik::permission(permission = "业务:资源:动作")]` 比对的是令牌里装载的 `auth_code`。
于是有这么一类**静默死锁**：

    后端 handler 写了 `permission = "business:erp:voucher:audit"`
    但 `sys_menu` 里没有 `auth_code = 'business:erp:voucher:audit'` 的行
    ⇒ 该码授不出去（管理面根本没有这一项可勾）
    ⇒ 除超管（`*:*:*` 通配）外**任何人都拿不到**
    ⇒ 财务经理有 `voucher:list` 却永远审不了 / 过不了凭证

2026-11-05（卡 `t_34cee9ea`）施工时人工全库比对抓到的就是这三条：

    business:erp:voucher:audit      voucher_write.rs:1589
    business:erp:voucher:un-audit   voucher_write.rs:1614
    business:erp:voucher:post       voucher_write.rs:1638

当时已补齐（迁移 `V0250__erp_voucher_three_state` 种了 3 条 `menu_type=3` 行；
数据修复 `scripts/datafix/bizperm/20_grant.up.sql` 对生产补同一批、带
`IF NOT EXISTS` 幂等兜底）。**本脚本是「防再长」的那道闸** —— 人工比对只做一次，
不做成常驻检查，下一个 `permission = "..."` 还会再长出来。

## 判据（两档，只有一档判红）

    missing_in_menu = 注解码 − 菜单 auth_code     ← **必须为 0**，否则 exit 1
    unused_in_code  = 菜单 auth_code − 注解码     ← 只报告，**不判红**

第二档为什么不判红：菜单码合法地可以「代码里没人以注解问」——
① 行级校验（handler 里 `claims.has_permission("…")` / `ensure_writable(…, PERM_*)`），
② 纯前端权限点（`frontend/src/router` 的 `meta.permission`，后端无接口），
③ 父子菜单/目录里挂的按钮码由父级接口一并管辖。
判红只会逼人往白名单里塞东西，反而不如如实报告。

## 口径（与 `system::menu::tests::n10` / `permission_audit_tests.rs` 对齐）

1. 范围 `backend/src/**/*.rs`；**排除测试文件**（按文件名：`tests.rs` / `tests_support.rs` /
   `test_*.rs` / `*_tests.rs` / `tests/` 目录）；
2. **剥注释**（`//` `///` `//!` `//*` 全部抹成空格，字符串字面量保留）——
   只在注释/文档里举例的码不算「在用」（`x:y:z` 就是这种夹具）；
3. 文件内 `#[cfg(test)]` 标记的项整段跳过（按花括号配对）：
   在 `backend/src` 下写用例夹具不可避免会写权限字符串，把它算成「在用」会让闸
   对测试夹具报警（FIX-06 的 n10 就栽过：被 `system:ok:list` 咬过）；
4. 取码只看**注解行**：`permission = "<码>"`（多行属性形态）与单行的
   `#[nik::permission(permission = "<码>")]` 两种写法都认；
5. 含 `*` 的通配码与占位符 `...` 跳过（超管通配不需要菜单承载）；
6. 菜单侧：`select distinct auth_code from sys_menu where deleted_at is null`
   （软删的菜单行不参与 —— 它授不出去）。

## 白名单（**必须带理由**，拒绝裸白名单）

两条路，都要求「码 + 理由」成对出现：

* 源码里：在 `permission = "..."` 上方（同一属性块内，最多 8 行）写
  `// PERM-EXEMPT: <为什么这个码暂时不需要菜单承载>`；
* 脚本里：`EXEMPT = {"<码>": "<理由>"}`。

裸登记（空理由 / 理由只有几个字符 / 写成 set 而不是 dict）→ 直接 exit 1 并点名，
因为「无理由白名单 = 把闸关掉」。

## 用法

    python3 scripts/verify/perm_menu_gate.py                    # 连库比对（NIK_DATABASE_*）
    python3 scripts/verify/perm_menu_gate.py --json              # 给 CI 解析
    python3 scripts/verify/perm_menu_gate.py --self-test         # 闸门自检（红/绿语料，不连库）
    python3 scripts/verify/perm_menu_gate.py --menu-codes-file X # 离线比对生产快照（无库环境）
    python3 scripts/verify/perm_menu_gate.py --repo /other/tree  # 扫另一棵树（验收时比对生产码）

连接口径（与 `erp_invariants.py` / `biz_type_domain.py` 一致）：
    NIK_DATABASE_HOST / NIK_DATABASE_PORT / NIK_DATABASE_USER /
    NIK_DATABASE_NAME / NIK_DATABASE_PASSWORD

退出码：0 = missing_in_menu 为空；1 = 有不死权限码 / 白名单裸登记 / 自检不通过；
        2 = 环境错误（连不上库、菜单码基数异常 —— 不算业务失败，`check.sh` 按「无 DB 环境」跳过）。

## 基线（2026-11-05 17:14 实测，写死在注释里供对照，**不作为判据**）

生产库 `cloudpivot@172.18.10.144`：`sys_menu` 活跃 **328 行 / 285 个去重 auth_code**；
主分支代码 `backend/src` 注解码 **273 个**（486 处，95 个码被多处复用）：

    missing_in_menu = 0     ← 本次补齐的 3 条凭证码已在 `sys_menu` 里（V0250 + datafix）
    unused_in_code  = 12    ← 逐条见下（不判红）

    business:attachment:delete         行级（权限常量在 attachment/dto.rs:31，handler 内判断）
    business:erp:skip-approval         行级（erp/approval_gate.rs:76，免审批开关）
    business:flow:admin                行级（flow/handler.rs:1594，流程定义管理面）
    business:project:delete            行级（project/handler.rs:57）
    business:project:update            行级（project/handler.rs:55）
    business:task:attachment:delete    行级（task/dto.rs:1407）
    business:task:comment:delete       行级（task/dto.rs:1404）
    business:task:dependency           行级（task/dto.rs:1093）
    business:task:member               行级（task/dto.rs:107）
    business:task:worklog              行级（task/dto.rs:1410）
    system:field-mask:plain            行级（masking/registry.rs:47，明文档位开关）
    system:preference:view             源码零引用 → 纯前端权限点（FIX-06 C 类，台账见
                                       docs/dead-permissions-20261101.md）

> 卡面写的是 `unused_in_code = 13`，实测 **12**：卡的 13 来自更早的库快照
> （菜单 282 / 代码 274，其中 1 条是单测夹具 `x:y:z`）。当前口径见上，逐条已核。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

def _default_root() -> Path:
    """仓根：脚本在 `scripts/verify/` 下时是 `parents[2]`；脚本被拷到 /tmp 跑时退回 CWD。

    验收要在生产机上用 `--repo` 指另一棵树，所以 ROOT 不能硬绑脚本位置
    （本卡实测：拷到 /tmp 后 `Path(__file__).parents[2]` 直接 IndexError）。
    """
    here = Path(__file__).resolve()
    # 拷到 /tmp 时 `parents[2]` 会 IndexError（本卡实测）
    cand = here.parents[2] if len(here.parents) > 2 else here.parent
    if (cand / "backend" / "src").is_dir():
        return cand
    cwd = Path.cwd()
    if (cwd / "backend" / "src").is_dir():
        return cwd
    return cand


ROOT = _default_root()

# ==================== 白名单（码 → 理由，**理由必填**） ====================
#
# 当前为空：`missing_in_menu = 0`，没有任何码需要豁免。
# 往里加条目前先问一句：这个码到底该不该有菜单承载？
#   · 该管没管 → 补菜单（迁移或 datafix），不是加白名单；
#   · 确实不该有（例如只有内部脚本用、或临时下线） → 写清楚理由 + 谁在什么条件下删。
EXEMPT: dict[str, str] = {}

# 扫描基数下限：低于它说明「扫描逻辑失效」（正则被改坏 / 目录换了 / 剥注释把代码吃掉），
# **不是**「项目里恰好没有权限码」—— 基线是 273 个。见 main() 里的防假绿断言。
MIN_CODE_COUNT = 50
# 菜单码基数下限：低于它说明连错库 / 迁移没跑（生产基线 285 个）
MIN_MENU_COUNT = 100

# 理由最短长度：`"todo"` / `"x"` 这种等于没写
MIN_REASON_LEN = 8

# ==================== 源码口径 ====================

# 注解行：`permission = "<码>"`（多行属性写法）
PERM_LINE_RE = re.compile(r'^[ \t]*permission[ \t]*=[ \t]*"([^"]*)"', re.MULTILINE)
# 单行属性写法：`#[nik::permission(permission = "<码>")]`
PERM_INLINE_RE = re.compile(r'#\s*\[\s*nik::permission\s*\(\s*permission\s*=\s*"([^"]*)"')
# 源码白名单注释
PERM_EXEMPT_RE = re.compile(r"PERM-EXEMPT\s*[:：]\s*(.*)$")
# `#[cfg(test)]` / `#[cfg(all(test, …))]`
CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\(\s*(?:all\(\s*)?test\b")
# Rust 字符字面量 `'a'` / `'\n'`（避免把 `&'a str` 的生命周期当成字符串开头）
CHAR_LIT_RE = re.compile(r"'(?:\\.|[^\\'])'")

# 明确的测试文件命名（整串匹配文件名）——不用「名字里有 test」这种子串判据：
# `latest_stock.rs` / `attestation.rs` 这种会被误豁免。
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

# 免检的码：超管通配与文档占位符
def _is_skippable_code(code: str) -> bool:
    return "*" in code or code.strip() in ("", "...")


def _blank(buf: list[str], start: int, end: int) -> None:
    """把 [start, end) 抹成空格（保留换行，让偏移 / 行号不变）。"""
    for i in range(start, end):
        if buf[i] != "\n":
            buf[i] = " "


def strip_comments(text: str) -> str:
    """把 Rust 注释抹成空格（字符串字面量原样保留，偏移保持不变）。

    为什么不逐行 `startswith("//")`：权限码就在字符串字面量里，必须保留；
    而 `// #[nik::permission(permission = "x:y:z")]` 这类**文档里的举例**必须抹掉，
    否则 `x:y:z` 会被当成真注解（FIX-06 的 n10 曾经被这种夹具咬过）。
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
            close = '"' + m.group(2)
            j = text.find(close, i + len(m.group(0)))
            i = n if j < 0 else j + len(close)
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
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


def cfg_test_spans(clean: str) -> list[tuple[int, int]]:
    """返回 `#[cfg(test)]` 标记的项所占的 [(start, end)] 区间（按花括号配对）。

    只跳「测试项本身」，不再「从标记到文件末尾全豁免」（否则往测试模块后面追加
    一个真注解就能悄悄蒙过闸）。
    """
    spans = []
    for m in CFG_TEST_RE.finditer(clean):
        i, n = m.end(), len(clean)
        while i < n and (clean[i].isspace() or clean[i] == "]"):
            i += 1
            if clean[i - 1] == "]":
                break
        start = m.start()
        j = i
        while j < n and clean[j] not in "{;":
            j += 1
        if j < n and clean[j] == ";":       # `#[cfg(test)] mod tests;` 无花括号
            spans.append((start, j + 1))
            continue
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


def is_test_file(path: Path) -> bool:
    if any(part == "tests" for part in path.parts[:-1]):
        return True
    return bool(TEST_NAME_RE.match(path.name))


def _line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def scan_source(name: str, text: str) -> dict:
    """扫一段 Rust 源码，返回 {code: [位置…]} 与统计。

    `name` 只用于测试文件判定（测试文件整体跳过）。
    返回的 `exempt_comments` 是「PERM-EXEMPT 写在哪个码上方」→ 理由，供白名单校验。
    """
    out = {"codes": {}, "skipped_cfg_test": 0, "exempt_comments": {}, "bad_exempt": []}
    if is_test_file(Path(name)):
        out["is_test_file"] = True
        return out
    raw = text
    clean = strip_comments(text)
    spans = cfg_test_spans(clean)
    raw_lines = raw.splitlines()

    def in_cfg_test(pos: int) -> bool:
        return any(s <= pos < e for s, e in spans)

    hits: list[tuple[int, str]] = []      # (pos, code)
    for m in PERM_LINE_RE.finditer(clean):
        hits.append((m.start(1), m.group(1)))
    for m in PERM_INLINE_RE.finditer(clean):
        hits.append((m.start(1), m.group(1)))

    seen_pos = set()
    for pos, code in hits:
        if pos in seen_pos:
            continue
        seen_pos.add(pos)
        if _is_skippable_code(code):
            continue
        if in_cfg_test(pos):
            out["skipped_cfg_test"] += 1
            continue
        code = code.strip()
        line = _line_of(clean, pos)
        out["codes"].setdefault(code, []).append(f"{name}:{line}")

        # 同一属性块内（最多往上 8 行）找 `// PERM-EXEMPT: 理由`
        lo = max(0, line - 1 - 8)
        reason = None
        for ln in range(lo, min(line, len(raw_lines))):
            for mm in PERM_EXEMPT_RE.finditer(raw_lines[ln]):
                reason = mm.group(1).strip()
        if reason is not None:
            if len(reason) >= MIN_REASON_LEN:
                out["exempt_comments"][code] = reason
            else:
                out["bad_exempt"].append((code, f"{name}:{line}", reason))
    return out


def scan_tree(repo_root: Path) -> dict:
    """扫整棵 `backend/src`（`repo_root` = 仓根，用于算相对路径）。"""
    src_root = repo_root / "backend" / "src"
    codes: dict[str, list[str]] = {}
    exempt_comments: dict[str, str] = {}
    bad_exempt: list[tuple[str, str, str]] = []
    files = 0
    test_files = 0
    skipped_cfg_test = 0
    for path in sorted(src_root.rglob("*.rs")):
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        try:
            rel = path.relative_to(repo_root).as_posix()
        except ValueError:      # --repo 与脚本不在同一棵树时（验收比对生产快照）
            rel = path.as_posix()
        res = scan_source(rel, text)
        if res.get("is_test_file"):
            test_files += 1
            continue
        files += 1
        skipped_cfg_test += res["skipped_cfg_test"]
        bad_exempt.extend(res["bad_exempt"])
        for code, locs in res["codes"].items():
            codes.setdefault(code, []).extend(locs)
        exempt_comments.update(res["exempt_comments"])
    return {
        "codes": codes,
        "exempt_comments": exempt_comments,
        "bad_exempt": bad_exempt,
        "files": files,
        "test_files": test_files,
        "skipped_cfg_test": skipped_cfg_test,
    }


# ==================== 菜单侧 ====================

def menu_codes_from_db() -> tuple[list[str], str]:
    """从库取 `sys_menu` 活跃权限码（只读 SELECT）。返回 (codes, 数据源说明)。"""
    host = os.environ.get("NIK_DATABASE_HOST", "127.0.0.1")
    port = os.environ.get("NIK_DATABASE_PORT", "5432")
    user = os.environ.get("NIK_DATABASE_USER", "cloudpivot")
    name = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    sql = (
        "select distinct auth_code from sys_menu "
        "where deleted_at is null and auth_code is not null and auth_code <> '' "
        "order by 1"
    )
    try:
        r = subprocess.run(
            ["psql", "-h", host, "-p", str(port), "-U", user, "-d", name, "-Atc", sql],
            capture_output=True, text=True, env=env, timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        raise RuntimeError(f"psql 无法执行：{e}") from e
    if r.returncode != 0:
        raise RuntimeError((r.stderr or r.stdout).strip()[:400])
    codes = [ln.strip() for ln in r.stdout.splitlines() if ln.strip()]
    return codes, f"db={name}@{host}:{port} user={user}"


def menu_codes_from_file(path: Path) -> tuple[list[str], str]:
    codes = []
    for ln in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        s = ln.strip()
        if not s or s.startswith("#"):
            continue
        codes.append(s)
    return codes, f"file={path}"


# ==================== 判定 ====================

def validate_whitelist(exempt: dict[str, str], bad_exempt: list[tuple[str, str, str]]) -> list[str]:
    """白名单校验：拒绝裸登记（空理由 / 太短 / 类型不是 str）。返回错误清单。"""
    errs = []
    if not isinstance(exempt, dict):
        return [f"EXEMPT 必须是 {{码: 理由}} 字典，当前是 {type(exempt).__name__}（裸白名单被拒绝）"]
    for code, reason in exempt.items():
        if not isinstance(reason, str) or not reason.strip():
            errs.append(f"EXEMPT 里 `{code}` 没有理由（裸白名单）")
        elif len(reason.strip()) < MIN_REASON_LEN:
            errs.append(f"EXEMPT 里 `{code}` 的理由太短（<{MIN_REASON_LEN} 字符）：{reason!r}")
    for code, where, reason in bad_exempt:
        errs.append(f"{where} 的 `// PERM-EXEMPT:` 理由为空或太短（{reason!r}）→ 码 {code} 仍判红")
    return errs


def evaluate(code_locs: dict[str, list[str]], menu: set[str],
             exempt: dict[str, str], exempt_comments: dict[str, str],
             row_level_refs: dict[str, list[str]]) -> dict:
    """纯判定（不碰 IO）：给出 missing / unused / 豁免明细。"""
    code_set = set(code_locs)
    effective_exempt = dict(exempt)
    effective_exempt.update(exempt_comments)     # 源码白名单并入

    missing_raw = sorted(code_set - menu)
    missing = [c for c in missing_raw if c not in effective_exempt]
    exempted = [c for c in missing_raw if c in effective_exempt]
    stale_exempt = sorted(c for c in effective_exempt if c not in code_set)
    unused = sorted(menu - code_set)
    return {
        "missing_in_menu": [{"code": c, "locations": code_locs.get(c, [])} for c in missing],
        "exempted": [{"code": c, "reason": effective_exempt[c]} for c in exempted],
        "stale_exempt": stale_exempt,
        "unused_in_code": [
            {
                "code": c,
                "kind": "row_level" if row_level_refs.get(c) else "frontend_only",
                "refs": row_level_refs.get(c, [])[:3],
            }
            for c in unused
        ],
        "ok": not missing,
    }


def collect_row_level_refs(repo_root: Path, candidates: list[str]) -> dict[str, list[str]]:
    """对 `unused_in_code` 的码，看看源码里是不是有**非注解**的字符串引用。

    有 = 行级校验（`claims.has_permission("…")` / `ensure_writable(…, "…")` / 常量声明），
    无 = 纯前端权限点。只为把「菜单有、代码无」这一档说清楚，不参与判红。
    """
    out: dict[str, list[str]] = {}
    if not candidates:
        return out
    src_root = repo_root / "backend" / "src"
    for path in sorted(src_root.rglob("*.rs")):
        if is_test_file(path):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        clean = strip_comments(text)
        try:
            rel = path.relative_to(repo_root).as_posix()
        except ValueError:
            rel = path.as_posix()
        for code in candidates:
            if f'"{code}"' in clean:
                line = _line_of(clean, clean.index(f'"{code}"'))
                out.setdefault(code, []).append(f"{rel}:{line}")
    return out


# ==================== 自检（闸门有牙） ====================

SELF_TEST_CASES = [
    # (文件名, 源码, 期望被扫到的码集合)
    ("h.rs",
     '#[nik::permission(\n    permission = "a:b:c",\n)]\nasync fn x() {}\n',
     {"a:b:c"}, "多行属性注解"),
    ("h.rs", '#[nik::permission(permission = "a:b:d")]\nasync fn x() {}\n',
     {"a:b:d"}, "单行属性注解"),
    ("h.rs", '// #[nik::permission(permission = "x:y:z")]\nasync fn x() {}\n',
     set(), "行注释里的举例不算在用"),
    ("h.rs", '/// 文档举例如 `permission = "x:y:z"`\nasync fn x() {}\n',
     set(), "文档注释里的举例不算在用"),
    ("h.rs", '/*\n#[nik::permission(\n    permission = "blk:c:d",\n)]\n*/\nfn x() {}\n',
     set(), "块注释里的注解不算在用（且行号不能乱）"),
    ("tests.rs", 'permission = "t:u:v"\n', set(), "测试文件整体排除"),
    ("h.rs", 'const S: &str = "// 不是注释";\n', set(), "字符串里的 // 不能吃掉后面的代码"),
    ("h.rs",
     '#[cfg(test)]\nmod tests {\n    fn f() {\n        #[nik::permission(\n            permission = "t:u:w",\n        )]\n        fn g() {}\n    }\n}\n',
     set(), "cfg(test) 项内的夹具码不算在用"),
    ("h.rs",
     '#[cfg(test)]\nmod tests { fn f() {} }\n#[nik::permission(\n    permission = "real:c:d",\n)]\nfn real() {}\n',
     {"real:c:d"}, "cfg(test) 之后的真注解仍必须被扫到"),
]

VERDICT_CASES = [
    # (code_locs, menu, exempt, exempt_comments, 期望 ok, 期望 missing, 期望 unused 数)
    ({"a:b:c": ["h.rs:1"]}, {"a:b:c", "x:y:z"}, {}, {}, True, [], 1, "码全在菜单里 → 绿；多出的菜单码只报告"),
    ({"a:b:c": ["h.rs:1"], "dead:c:d": ["h.rs:9"]}, {"a:b:c"}, {}, {}, False, ["dead:c:d"], 0,
     "代码在用、菜单没有 → 红并点名"),
    ({"a:b:c": ["h.rs:1"], "dead:c:d": ["h.rs:9"]}, {"a:b:c", "dead:c:d"}, {}, {}, True, [], 0,
     "补上菜单后 → 绿"),
    ({"a:b:c": ["h.rs:1"], "tmp:c:d": ["h.rs:9"]}, {"a:b:c"}, {"tmp:c:d": "临时下线，等 X 卡补菜单后删除"},
     {}, True, [], 0, "带理由的脚本白名单 → 放行且进豁免明细"),
    ({"a:b:c": ["h.rs:1"], "tmp:c:d": ["h.rs:9"]}, {"a:b:c"}, {}, {"tmp:c:d": "等 X 卡补菜单"},
     True, [], 0, "带理由的源码 PERM-EXEMPT → 放行"),
    ({"a:b:c": ["h.rs:1"]}, {"a:b:c"}, {"gone:c:d": "这条码早就不存在了"}, {}, True, [], 0,
     "过期豁免不判红（但会进 stale 列表）"),
]


def self_test() -> bool:
    bad = 0
    print("=" * 92)
    print("闸门自检：判据红/绿语料（不连库）")
    print("=" * 92)
    for name, code, expect, label in SELF_TEST_CASES:
        res = scan_source(name, code)
        got = set(res["codes"])
        ok = got == expect
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 取码 {sorted(got)}  期望 {sorted(expect)}  {label}")

    for code_locs, menu, exempt, exempt_comments, expect_ok, expect_missing, expect_unused, label in VERDICT_CASES:
        v = evaluate(code_locs, set(menu), exempt, exempt_comments, {})
        got_missing = [m["code"] for m in v["missing_in_menu"]]
        ok = (v["ok"] == expect_ok and got_missing == expect_missing
              and len(v["unused_in_code"]) == expect_unused)
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 判定 ok={v['ok']} missing={got_missing} "
              f"unused={len(v['unused_in_code'])}  {label}")

    # 裸白名单必须被拒
    for bad_whitelist, label in [
        ({"a:b:c": ""}, "空理由"),
        ({"a:b:c": "todo"}, "理由太短"),
        ({"a:b:c": None}, "理由不是字符串"),
    ]:
        errs = validate_whitelist(bad_whitelist, [])
        ok = bool(errs)
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 裸白名单被拒（{label}）：{errs[0] if errs else '没拒绝！'}")
    errs = validate_whitelist({"a:b:c": "临时下线，等 t_xxx 补迁移后删除"}, [])
    ok = not errs
    bad += 0 if ok else 1
    print(f"{'✔' if ok else '✘'} 带理由的白名单被接受：{errs or 'OK'}")

    total = len(SELF_TEST_CASES) + len(VERDICT_CASES) + 4
    print("-" * 92)
    print(f"{total - bad}/{total} 通过" + ("" if not bad else f"；✗ {bad} 条不符"))
    return bad == 0


# ==================== main ====================

def main() -> int:
    ap = argparse.ArgumentParser(description="死权限码常驻闸（只读）")
    ap.add_argument("--repo", default=str(ROOT), help="仓库根（默认本脚本所在仓）")
    ap.add_argument("--json", action="store_true", help="JSON 输出（给 CI / 监控）")
    ap.add_argument("--self-test", action="store_true", dest="self_test",
                    help="闸门自检：红/绿语料，不连库")
    ap.add_argument("--menu-codes-file", default=None,
                    help="离线模式：从快照文件读菜单权限码（一码一行，# 开头为注释）")
    args = ap.parse_args()

    if args.self_test:
        return 0 if self_test() else 1

    repo = Path(args.repo).resolve()
    src_root = repo / "backend" / "src"
    if not src_root.is_dir():
        print(f"✗ 找不到源码目录：{src_root}", file=sys.stderr)
        return 2

    scan = scan_tree(repo)

    # 防假绿①：扫描基数。扫出 0 个码时 missing_in_menu 必然「为空」——
    # 那是扫描器坏了，不是权限码没问题（2026-11-05 本脚本首次跑就栽过：
    # 正则漏了 re.MULTILINE → 0 个码 → 闸「PASS」，纯假绿）。
    if len(scan["codes"]) < MIN_CODE_COUNT:
        print(f"✗ 扫描基数异常：只扫到 {len(scan['codes'])} 个注解码（下限 {MIN_CODE_COUNT}）",
              file=sys.stderr)
        print(f"  扫了 {scan['files']} 个非测试 .rs（跳过 cfg(test) 夹具码 "
              f"{scan['skipped_cfg_test']} 处）→ 扫描逻辑失效，闸判红而不是判绿", file=sys.stderr)
        print("  排查：注解形态是否变了（本脚本认 `permission = \"…\"` 与单行 "
              "`#[nik::permission(permission = \"…\")]`）", file=sys.stderr)
        return 1

    # 白名单校验先做：裸白名单 = 把闸关掉，必须立刻失败
    wl_errs = validate_whitelist(EXEMPT, scan["bad_exempt"])

    try:
        if args.menu_codes_file:
            menu, source = menu_codes_from_file(Path(args.menu_codes_file))
        else:
            menu, source = menu_codes_from_db()
    except Exception as e:  # noqa: BLE001
        print(f"✗ 拿不到菜单权限码：{e}", file=sys.stderr)
        print("  （无库环境可加 --menu-codes-file <快照文件>；CI 里由 check.sh 按「无 DB 环境」跳过）",
              file=sys.stderr)
        return 2

    if len(menu) < MIN_MENU_COUNT:
        print(f"✗ 菜单权限码基数异常（{len(menu)} < {MIN_MENU_COUNT}）：连错库 / 迁移没跑 / 表名不对",
              file=sys.stderr)
        print(f"  数据源：{source}", file=sys.stderr)
        return 2

    candidates = sorted(set(menu) - set(scan["codes"]))
    row_refs = collect_row_level_refs(repo, candidates)
    v = evaluate(scan["codes"], set(menu), EXEMPT, scan["exempt_comments"], row_refs)

    if args.json:
        print(json.dumps(
            dict(
                ok=v["ok"] and not wl_errs,
                source=source,
                code_count=len(scan["codes"]),
                code_occurrences=sum(len(x) for x in scan["codes"].values()),
                menu_count=len(menu),
                missing_in_menu=v["missing_in_menu"],
                unused_in_code=v["unused_in_code"],
                exempted=v["exempted"],
                stale_exempt=v["stale_exempt"],
                whitelist_errors=wl_errs,
                scanned_files=scan["files"],
                skipped_test_files=scan["test_files"],
                skipped_cfg_test_items=scan["skipped_cfg_test"],
            ),
            ensure_ascii=False, indent=2,
        ))
        return 0 if (v["ok"] and not wl_errs) else 1

    print("=" * 92)
    print("死权限码常驻闸 · perm_menu_gate.py（handler 注解码 − sys_menu.auth_code）")
    print("=" * 92)
    print(f"代码侧：{scan['files']} 个非测试 .rs（跳过测试文件 {scan['test_files']} 个、"
          f"cfg(test) 项内夹具码 {scan['skipped_cfg_test']} 处）→ 注解码 {len(scan['codes'])} 个"
          f"（{sum(len(x) for x in scan['codes'].values())} 处）")
    print(f"菜单侧：{len(menu)} 个活跃 auth_code（{source}）")
    print("-" * 92)

    if v["missing_in_menu"]:
        print(f"✗ missing_in_menu = {len(v['missing_in_menu'])}  ← 必须为 0！"
              f"这些码授不出去，除超管外任何人都会 403：")
        for item in v["missing_in_menu"]:
            locs = ", ".join(item["locations"])
            print(f"  · {item['code']}")
            print(f"      用在这：{locs}")
        print("  修法：补一条 sys_menu 行（迁移优先；生产补数走 scripts/datafix/…，"
              "固定 uuid + IF NOT EXISTS 幂等）；确实不该有菜单的写 `// PERM-EXEMPT: 理由`。")
    else:
        print("✓ missing_in_menu = 0   （代码在用的码全部有菜单承载，可被授予）")

    if v["exempted"]:
        print(f"  白名单放行 {len(v['exempted'])} 条：")
        for item in v["exempted"]:
            print(f"  · {item['code']}  ← {item['reason']}")
    if v["stale_exempt"]:
        print(f"  ⚠ 过期白名单 {len(v['stale_exempt'])} 条（码已不在源码里，建议删条目）："
              f"{', '.join(v['stale_exempt'])}")

    print(f"ℹ unused_in_code  = {len(v['unused_in_code'])}  （只报告，不判红：行级校验 / 纯前端权限点都合法）")
    for item in v["unused_in_code"]:
        if item["kind"] == "row_level":
            loc = item["refs"][0] if item["refs"] else "?"
            print(f"  · {item['code']:<34} 行级/常量引用  {loc}")
        else:
            print(f"  · {item['code']:<34} 源码零引用（纯前端权限点）")

    if wl_errs:
        print("-" * 92)
        print(f"✗ 白名单裸登记 {len(wl_errs)} 条（无理由的豁免等于关闸）：")
        for e in wl_errs:
            print(f"  · {e}")

    print("=" * 92)
    if v["ok"] and not wl_errs:
        print("==> 闸：PASS")
        return 0
    print("==> 闸：FAIL")
    return 1


if __name__ == "__main__":
    sys.exit(main())
