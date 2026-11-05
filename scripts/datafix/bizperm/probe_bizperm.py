#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""BIZPERM 验收探针（P0-FIX-2B-C 跟进）—— 逐账号、真身份、贴返回码 + 可见行数。

为什么要接口级验：403 只认**令牌里的权限码快照**（`nik-web::has_permission`），
菜单树由 `load_user_menus_uncached` 按角色授权拼。库里 `sys_role_menu` 补了码，
不等于「这个账号真的调得通」——必须用他本人的身份发请求。

身份从哪来：生产口令不可猜（仓库规则），用同目录 `mint_token.py` 按库内真值自签
（`ver` 逐请求与库比对；声明了 `*:*:*` 会被库侧复核 ⇒ 伪造不出超管）。

用法：
    set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    python3 probe_bizperm.py --mode laoliu --base http://127.0.0.1:8023
    python3 probe_bizperm.py --all

退出码：0 = 全部断言通过；1 = 有 FAIL。
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mint_token  # noqa: E402

TIMEOUT = 30

# 施工后必须 200 的业务列表接口（缺一个码就是 403）
BIZ_200 = [
    ("business:task",           "/api/business/task/page?page=1&page_size=1"),
    ("business:project",        "/api/business/project/page?page=1&page_size=1"),
    ("erp:purchase-order",      "/api/business/erp/purchase-order/page?page=1&page_size=1"),
    ("erp:purchase-order-item", "/api/business/erp/purchase-order-item/page?page=1&page_size=1"),
    ("erp:supplier",            "/api/business/erp/supplier/page?page=1&page_size=1"),
    ("erp:contract",            "/api/business/erp/contract/page?page=1&page_size=1"),
    ("erp:material",            "/api/business/erp/material/page?page=1&page_size=1"),
    ("erp:voucher",             "/api/business/erp/voucher/page?page=1&page_size=1"),
    ("erp:account",             "/api/business/erp/account/page?page=1&page_size=1"),
    ("erp:period",              "/api/business/erp/period/page?page=1&page_size=1"),
    # 总账必须带科目（account_id/account_code 二选一），否则 400；1001=库存现金
    ("erp:ledger",              "/api/business/erp/ledger/page?account_code=1001&page=1&page_size=1"),
    ("erp:inventory",           "/api/business/erp/inventory/page?page=1&page_size=1"),
    ("erp:stock-in",            "/api/business/erp/stock-in/page?page=1&page_size=1"),
]

# 平台治理：业务角色必须 403（与 P0-FIX-2B 的收敛目标一致）
SYS_403 = [
    ("system:user",  "/api/system/user/page?page=1&page_size=1"),
    ("system:role",  "/api/system/role/page?page=1&page_size=1"),
    ("system:menu",  "/api/system/menu/tree"),
    ("support:log",  "/api/support/log/page?page=1&page_size=1"),
]

# 每个模式的期望（页面前缀 + 数据范围）
MODES = {
    "admin": dict(username="admin", roles_min=1, must_have_role=["SUPER_ADMIN"], must_not_role=[],
                  sys_expect=200, must_pages=["/task/list", "/project", "/business/erp/voucher"],
                  must_not_pages=[], expect_total={"business:task": 71, "business:project": 3}),
    "laoliu": dict(roles_min=5, must_have_role=["DEPT_LEADER", "FINANCE_MANAGER", "GENERAL_MANAGER",
                                               "PURCHASE_MANAGER", "WAREHOUSE_MANAGER"],
                   must_not_role=["SUPER_ADMIN"], sys_expect=403,
                   must_pages=["/task/list", "/task/board", "/project", "/business/erp/purchase",
                               "/business/erp/contract", "/business/erp/supplier", "/business/erp/material",
                               "/business/erp/voucher", "/business/erp/ledger", "/business/erp/account",
                               "/business/erp/period",
                               "/business/erp/report/balance-sheet", "/business/erp/stock-in"],
                   must_not_pages=[],
                   expect_total={"business:task": 71, "business:project": 3}),
    "henry": dict(username="henry.liao", roles_min=4, must_have_role=["DEPT_LEADER", "GENERAL_MANAGER",
                                              "PURCHASE_MANAGER", "WAREHOUSE_MANAGER"],
                  must_not_role=["SUPER_ADMIN"], sys_expect=403,
                  must_pages=["/task/list", "/project", "/business/erp/purchase", "/business/erp/contract",
                              "/business/erp/supplier"],
                  # GENERAL_MANAGER 是**全域只读**：能看到凭证页（voucher:list），但没有 voucher:post/audit
                  must_not_pages=[],
                  biz_403=[],
                  expect_total={"business:task": 71, "business:project": 3}),
    "laoliu_test": dict(roles_min=3, must_have_role=["DEPT_LEADER", "PURCHASE_MANAGER", "WAREHOUSE_MANAGER"],
                        must_not_role=["SUPER_ADMIN", "GENERAL_MANAGER", "FINANCE_MANAGER"], sys_expect=403,
                        must_pages=["/task/list", "/project", "/business/erp/purchase"],
                        must_not_pages=["/business/erp/voucher"],
                        # 无财务角色 ⇒ 财务四页必须仍 403（授权面按职能归位，不是「一刀切全开」）
                        biz_403=["erp:voucher", "erp:account", "erp:period", "erp:ledger"],
                        # 3 档（本部门及以下）+ 生产活跃 biz_task.dept_id 全 NULL ⇒ 0 行（预期，不是 bug）
                        expect_total={"business:task": 0, "business:project": 0}),
}


def call(base, path, token):
    req = urllib.request.Request(base + path, headers={"Authorization": "Bearer " + token})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        return 0, "EXC " + str(e)


def total_of(payload):
    """从 Res::page 的响应里取 total（宽容解析）。"""
    try:
        obj = json.loads(payload)
    except Exception:  # noqa: BLE001
        return None
    for node in (obj, obj.get("data") if isinstance(obj, dict) else None):
        if isinstance(node, dict):
            for k in ("total", "total_count"):
                if isinstance(node.get(k), int):
                    return node[k]
    return None


def walk_pages(nodes, out):
    for n in nodes or []:
        if n.get("menu_type") == 2 or n.get("menuType") == 2:
            out.append(n.get("path") or "")
        walk_pages(n.get("children") or n.get("child") or [], out)


def run_mode(mode, base):
    cfg = MODES[mode]
    import subprocess
    token = subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                         "mint_token.py"), "--username", cfg.get("username", mode)],
                           capture_output=True, text=True)
    if token.returncode != 0:
        print("  [FAIL] 自签令牌失败：%s" % token.stderr.strip()[:200])
        return {"mode": mode, "fail": 1, "checks": []}
    token = token.stdout.strip()

    checks = []

    def chk(name, ok, actual, expect):
        checks.append({"check": name, "expect": expect, "actual": actual, "ok": bool(ok)})
        print("  [%s] %-28s 期望=%-10s 实际=%s" % ("PASS" if ok else "FAIL", name, expect, actual))

    print("--- %s ---" % mode)
    st, body = call(base, "/api/system/auth/me", token)
    chk("auth/me", st == 200, str(st), "200")
    me = {}
    try:
        me = json.loads(body).get("data") or {}
    except Exception:  # noqa: BLE001
        pass
    roles = me.get("roles") or []
    role_codes = [r.get("role_code") if isinstance(r, dict) else r for r in roles]
    perms = me.get("permissions") or me.get("perms") or []
    chk("auth/me 角色数", len(role_codes) >= cfg["roles_min"], len(role_codes), ">= %d" % cfg["roles_min"])
    for r in cfg["must_have_role"]:
        chk("必含角色 %s" % r, r in role_codes, "含" if r in role_codes else "不含", "含")
    for r in cfg["must_not_role"]:
        chk("必不含角色 %s" % r, r not in role_codes, "含" if r in role_codes else "不含", "不含")
    chk("权限码条数", len(perms) > 0, len(perms), ">0")

    st, body = call(base, "/api/system/auth/menus", token)
    chk("auth/menus", st == 200, str(st), "200")
    pages = []
    try:
        tree = json.loads(body).get("data")
        walk_pages(tree if isinstance(tree, list) else (tree or {}).get("list", []), pages)
    except Exception:  # noqa: BLE001
        pass
    chk("可见业务页数", len(pages) >= 12, len(pages), ">= 12")
    for p in cfg["must_pages"]:
        chk("菜单含 %s" % p, p in pages, "含" if p in pages else "缺", "含")
    for p in cfg["must_not_pages"]:
        chk("菜单不含 %s" % p, p not in pages, "含" if p in pages else "缺", "缺")

    totals = {}
    expect_403 = set(cfg.get("biz_403", []))
    for label, path in BIZ_200:
        st, body = call(base, path, token)
        t = total_of(body)
        totals[label] = t
        if label in expect_403:
            chk("%s（无该职能，应 403）" % label, st == 403, str(st), "403")
            continue
        exp = cfg.get("expect_total", {}).get(label)
        if exp is not None:
            chk("%s 行数" % label, t == exp, t, exp)
        else:
            chk("%s 可访问" % label, st == 200, str(st), "200")

    for label, path in SYS_403:
        st, _ = call(base, path, token)
        chk("%s（应 403）" % label, st == cfg["sys_expect"], str(st), str(cfg["sys_expect"]))

    fail = sum(1 for c in checks if not c["ok"])
    print("  fail=%d / %d" % (fail, len(checks)))
    return {"mode": mode, "roles": role_codes, "perm_count": len(perms),
            "business_pages": len(pages), "page_list": pages, "totals": totals,
            "fail": fail, "checks": checks}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=list(MODES), help="单账号模式")
    ap.add_argument("--all", action="store_true", help="四个账号全跑")
    ap.add_argument("--base", default=os.environ.get("BIZPERM_BASE", "http://127.0.0.1:8023"))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    modes = list(MODES) if args.all else ([args.mode] if args.mode else list(MODES))
    out, fail = [], 0
    for m in modes:
        r = run_mode(m, base)
        fail += r["fail"]
        out.append(r)
    if args.json:
        print("\nJSON " + json.dumps({"base": base, "results": out, "fail": fail}, ensure_ascii=False))
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
