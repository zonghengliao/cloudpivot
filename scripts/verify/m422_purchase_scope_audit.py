#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M4-22 采购缺口收口 —— 接口面 / 权限码 / 页面码 三向对拍审计（离线：读源码 + 读库导出）。

为什么要有它
------------
M4 的 22 张卡各自都验过自己那一段，但**没有人**验过「拼起来之后接口面与权限码表还对不对得上」：
  · 有路由没挂权限声明 → 登录即可访问，等于漏权；
  · 源码用了某个码、`sys_menu` 里没种 → 除超管外全 403；
  · 按钮码种了、没有任何接口承载 → 摆设码（约定 8 / 死权限）；
  · 权限块写了、路由宏漏了/写错 → 声明了但接口根本没注册；
  · 页面显形码（`…:page`）被后端**代码**引用 → 违约 AGENTS 约定 14
    （文档注释里写一句「本页用 xx:page、后端零引用」不算引用）。

输入
----
1. `--src`：后端源码根（默认 `backend/app/src`）；
2. `--menu-tsv`：从库里导出的菜单行，4 列 TSV —— `auth_code \t menu_type \t path \t title`，空值写 `-`；
3. `--scope-regex`：范围内接口路径的正则（可给多次，默认本卡四主题 + 合同执行 + 采购单承载面）。

判定
----
逐条打印 PASS / FAIL；任一 FAIL 退出码 1。纯只读：不改库、不改源码。
"""

from __future__ import annotations

import argparse
import os
import re
import sys

PERM_RE = re.compile(
    r"#\[nik::permission\(\s*"
    r'path\s*=\s*"(?P<path>[^"]+)"\s*,\s*'
    r'method\s*=\s*"(?P<method>[A-Z]+)"\s*,\s*'
    r'permission\s*=\s*"(?P<code>[^"]*)"\s*'
    r"\)\]"
)
ROUTE_RE = re.compile(r'#\[nik::(get|post|put|delete)\("(?P<path>[^"]+)"\)\]')

# 四主题 + 合同执行 + 采购单承载面（合同号 / 框架协议要在采购单身上生效）
DEFAULT_SCOPE = [
    r"^/business/erp/purchase-plan(?=/|$)",
    r"^/business/erp/framework-agreement(?=/|$)",
    r"^/business/erp/supplier-evaluation(?=/|$)",
    r"^/business/erp/supplier-access(?=/|$)",
    r"^/business/erp/contract/(execution/page|\{id\}/execution)$",
    r"^/business/erp/purchase-order(?=/|$)",
]

# 每个前缀期望的路由条数（本卡实测基线：9 / 8 / 5 / 6 / 2 / 11）
EXPECT_ROUTES = [
    (r"^/business/erp/purchase-plan(?=/|$)", 9, "主题① 采购计划"),
    (r"^/business/erp/framework-agreement(?=/|$)", 8, "主题② 框架协议"),
    (r"^/business/erp/supplier-evaluation(?=/|$)", 5, "主题③ 供应商评估"),
    (r"^/business/erp/supplier-access(?=/|$)", 6, "主题③ 供应商准入"),
    (r"^/business/erp/contract/(execution/page|\{id\}/execution)$", 2, "主题④ 合同执行量"),
    # 既有域（非本批新增）：9 条 CRUD/查询 + 1 条 `PATCH .../status`；`purchase-order-item/*` 不算
    (r"^/business/erp/purchase-order(?=/|$)", 10, "主题④ 采购单承载面（既有域）"),
]

# 每个主题期望的菜单行（页面行 + 按钮行），合计 24
EXPECT_MENU = [
    ("business:erp:purchase-plan:", 8, "主题① 采购计划（1 页 + 7 按钮）"),
    ("business:erp:framework-agreement:", 7, "主题② 框架协议（1 页 + 6 按钮）"),
    ("business:erp:supplier-evaluation:", 3, "主题③ 供应商评估（1 页 + 2 按钮）"),
    ("business:erp:supplier-access:", 5, "主题③ 供应商准入（1 页 + 4 按钮）"),
    ("business:erp:contract:execution", 1, "主题④ 合同执行量（1 按钮）"),
]

# 审计门禁自身持有的页面码清单（`permission_audit_tests` 的 PAGE_ONLY_CODES），
# 它是**判据出处**，不算「后端引用页面码」。
AUDIT_GATE_FILES = {"system/menu/permission_audit_tests.rs"}


def strip_comments(text: str) -> str:
    """去掉行注释与块注释（够用即可：本仓库没有含 `//` 的字符串字面量影响判定）。"""
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r"//[^\n]*", "", text)


class Result:
    def __init__(self) -> None:
        self.passed = 0
        self.failed = 0

    def ok(self, label: str) -> None:
        self.passed += 1
        print(f"  PASS {label}")

    def bad(self, label: str) -> None:
        self.failed += 1
        print(f"  FAIL {label}")


def scan_routes(src_root: str, scope: list[str]) -> list[dict]:
    patterns = [re.compile(p) for p in scope]
    rows: list[dict] = []
    for base, _dirs, files in os.walk(src_root):
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            path = os.path.join(base, name)
            text = open(path, encoding="utf-8", errors="replace").read()
            if "nik::permission" not in text:
                continue
            for m in PERM_RE.finditer(text):
                rpath = m.group("path")
                if not any(p.search(rpath) for p in patterns):
                    continue
                tail = text[m.end() : m.end() + 400]
                route = ROUTE_RE.search(tail)
                rows.append(
                    {
                        "file": os.path.relpath(path, src_root),
                        "path": rpath,
                        "method": m.group("method"),
                        "code": m.group("code"),
                        "macro": f'{route.group(1).upper()} {route.group("path")}' if route else "",
                    }
                )
    return rows


def load_menu(tsv_path: str) -> list[dict]:
    rows: list[dict] = []
    with open(tsv_path, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            parts = line.rstrip("\n").split("\t")
            while len(parts) < 4:
                parts.append("-")
            code, mtype, path, title = parts[:4]
            rows.append(
                {
                    "code": "" if code == "-" else code,
                    "type": mtype.strip(),
                    "path": "" if path == "-" else path,
                    "title": "" if title == "-" else title,
                }
            )
    return rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default="backend/app/src")
    ap.add_argument("--menu-tsv", required=True)
    ap.add_argument("--scope-regex", action="append", default=None)
    args = ap.parse_args()

    scope = args.scope_regex or DEFAULT_SCOPE
    res = Result()
    rows = scan_routes(args.src, scope)
    menu = load_menu(args.menu_tsv)

    # ---------- 1) 每个主题的路由条数（少一条 = 某个入口丢了） ----------
    for regex, want, label in EXPECT_ROUTES:
        pat = re.compile(regex)
        got = [r for r in rows if pat.search(r["path"])]
        if len(got) == want:
            res.ok(f"{label} 路由 {want} 条")
        else:
            res.bad(f"{label} 路由条数（期望 {want}，实际 {len(got)}）：" + ", ".join(f"{r['method']} {r['path']}" for r in got))

    # ---------- 2) 一条路由都不许裸奔（没有权限码 = 登录即可） ----------
    no_code = [r for r in rows if not r["code"].strip()]
    if no_code:
        res.bad(f"{len(no_code)} 条路由没挂权限码：" + ", ".join(f"{r['method']} {r['path']}" for r in no_code[:6]))
    else:
        res.ok(f"{len(rows)} 条范围内路由全部挂了 `#[nik::permission]` 权限码")

    # ---------- 3) 权限声明的 path 与路由宏必须一致（否则「声明了但没注册」） ----------
    drift = [r for r in rows if r["macro"] and not r["macro"].endswith(r["path"])]
    if drift:
        res.bad("权限声明 path 与路由宏不一致：" + ", ".join(f"{r['file']} {r['path']}" for r in drift[:6]))
    else:
        res.ok("权限声明 path 与路由宏 path 逐条一致")

    # ---------- 4) 每个主题的菜单行（页面行 + 按钮行）条数 ----------
    for prefix, want, label in EXPECT_MENU:
        got = [m for m in menu if m["code"].startswith(prefix)]
        if len(got) == want:
            res.ok(f"{label} 菜单 {want} 行")
        else:
            res.bad(f"{label} 菜单行数（期望 {want}，实际 {len(got)}）")

    # ---------- 5) 源码用到的码，`sys_menu` 里必须都有 ----------
    menu_codes = {m["code"] for m in menu if m["code"]}
    used = sorted({r["code"] for r in rows if r["code"]})
    missing = [c for c in used if c not in menu_codes]
    if missing:
        res.bad("源码用了但 sys_menu 没种的码：" + ", ".join(missing))
    else:
        res.ok(f"源码用到的 {len(used)} 个权限码在 `sys_menu` 全部有承载")

    # ---------- 6) 按钮码不许摆设（每个码至少一条路由用） ----------
    buttons = [m for m in menu if m["type"] == "3" and m["code"]]
    orphans = [m["code"] for m in buttons if m["code"] not in used]
    if orphans:
        res.bad("按钮码没有任何接口承载（摆设码）：" + ", ".join(orphans))
    else:
        res.ok(f"{len(buttons)} 个按钮码全部有接口承载（无摆设码）")

    # ---------- 7) 页面显形码：后端**代码**零引用（约定 14） ----------
    #
    # 判据只认「真接线」：`permission = "<页面码>"`。文档注释里写「本页用 xx:page、
    # 后端零引用」是说明文字；`permission_audit_tests` 与各模块自带的
    # `!PERMISSIONS.contains("…:page")` 是**反向守卫**（正是证明零引用的断言），
    # 一律不算违约 —— 否则这条断言会红在自己身上。
    pages = [m for m in menu if m["type"] == "2" and m["code"].endswith(":page")]
    leaked: list[str] = []
    for base, _dirs, files in os.walk(args.src):
        for name in files:
            if not name.endswith(".rs"):
                continue
            rel = os.path.relpath(os.path.join(base, name), args.src)
            if rel in AUDIT_GATE_FILES:
                continue
            body = strip_comments(open(os.path.join(base, name), encoding="utf-8", errors="replace").read())
            for p in pages:
                if re.search(r'permission\s*=\s*"' + re.escape(p["code"]) + r'"', body):
                    leaked.append(f"{p['code']} @ {rel}")
    if leaked:
        res.bad("页面码被后端代码引用（违约约定 14）：" + ", ".join(leaked[:6]))
    else:
        res.ok(f"{len(pages)} 个页面显形码在后端代码零引用（只给前端判显隐）")

    # ---------- 8) 菜单 title 长度（局部复核；全局门禁另跑 menu_invariants.sh） ----------
    # 只查本批新增的四主题（采购单是既有域，它的长 title 不归本卡修，另行登记）
    themed_prefixes = tuple(p for p, _n, _l in EXPECT_MENU) + ("business:erp:contract:execution",)
    long_titles = [m for m in menu if m["code"].startswith(themed_prefixes) and len(m["title"]) > 12]
    if long_titles:
        res.bad("菜单 title 超 12 字：" + ", ".join(f"{m['code']}「{m['title']}」" for m in long_titles[:5]))
    else:
        res.ok("四主题菜单 title 全部 ≤ 12 字")

    print(f"\n== 审计汇总：PASS={res.passed} FAIL={res.failed}（路由 {len(rows)} 条 / 菜单行 {len(menu)} 行）==")
    return 0 if res.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
