#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""hr_perm_expect.py —— 从迁移文件解析「人力资源权限矩阵」的**期望值**

## 为什么要有它（而不是在脚本里手抄一张表）

矩阵的唯一真源是 `V0387__hr_menu_perms.up.sql`：35 个权限码（5 个页面 list 码 +
30 个按钮码）与 5 个角色的授权数组都在里面。脚本里再手抄一份，「矩阵回归」就变成
「两份手抄互相比」，抄错的那份永远比不过、或者两份一起抄错永远绿。

所以：
  · **期望** = 本脚本解析迁移文件（种子怎么种，就该是什么）
  · **实际** = 真实例 `/api/system/auth/me` 的 `permissions` + 各路由的 200/403
  · Rust 侧 `backend/src/business/hr/perm_matrix_tests.rs` 用 `include_str!` 独立解析
    同一份 SQL 并对拍设计定稿的矩阵 —— 两个解析器互相牵制，谁偏了都红。

## 输出（--json）

    {
      "source": "<迁移文件路径>",
      "codes": [{"id": "...", "code": "business:hr:employee:list", "menu_type": 2, "name": "员工档案"}, ...],
      "buttons": ["..."],            # menu_type=3 的 30 个码（设计 §3.2 ④）
      "pages":   ["..."],            # menu_type=2 的 5 个 list 码
      "roles":   {"SUPER_ADMIN": [...], "HR_MANAGER": [...], ...}
    }

## 用法

    python3 scripts/verify/hr/hr_perm_expect.py                       # 人读表格
    python3 scripts/verify/hr/hr_perm_expect.py --json                # 给脚本消费
    python3 scripts/verify/hr/hr_perm_expect.py --migration <path>     # 换文件（回归演练）
"""

import argparse
import json
import pathlib
import re
import sys

REPO = pathlib.Path(__file__).resolve().parents[3]
DEFAULT_MIGRATION = REPO / "backend/migrations/business/V0387__hr_menu_perms.up.sql"

# 迁移里的角色数组变量 → 角色编码（数组名见 V0387 §4）
ARRAY_TO_ROLE = {
    "all_hr": ["SUPER_ADMIN", "HR_MANAGER"],
    "fin_hr": ["FINANCE_MANAGER"],
    "lead_hr": ["DEPT_LEADER"],
    "emp_hr": ["EMPLOYEE"],
}

# 第 6 个角色：种子存在、但 V0387 一行都没授给它 —— 矩阵里的「零授权对照列」。
# 6 角色 = 5 个被授权的 + 1 个真实存在但零授权的（证明「没码就一律 403」不是
# 因为账号不存在，而是因为角色确实没这个码）。
ZERO_ROLE = "FINANCE_DIRECTOR"

MENU_ROW = re.compile(
    r"\(\s*'(20000000-0000-4000-8000-00000000e0[0-9a-f]{2})'::uuid\s*,\s*'([^']*)'\s*,\s*(1|2|3)\s*,\s*'([^']*)'",
    re.S,
)
ROLE_ARRAY = re.compile(r"(\w+_hr)\s+UUID\[\]\s*:=\s*ARRAY\[(.*?)\]::uuid\[\]", re.S)
UUID = re.compile(r"'(20000000-0000-4000-8000-00000000e0[0-9a-f]{2})'")


def parse(migration: pathlib.Path) -> dict:
    sql = migration.read_text(encoding="utf-8")

    # ---------- ① 36 行菜单：id → (码, 类型, 名字) ----------
    menus = {}
    for uid, name, mtype, code in MENU_ROW.findall(sql):
        # 同 id 只在 §1/§2 的 INSERT 里出现一次；§0/§4/§5 里的裸 uuid 不带这些列，
        # 不会被本正则命中（它要求紧跟 name/type/auth_code）。
        if uid in menus:
            raise SystemExit(f"FATAL: 菜单 id {uid} 在迁移里出现两次（解析不了唯一矩阵）")
        menus[uid] = {"id": uid, "name": name, "menu_type": int(mtype), "code": code}

    if len(menus) != 36:
        raise SystemExit(
            f"FATAL: 只解析到 {len(menus)} 行菜单（应 36 = 1 目录 + 5 页面 + 30 按钮）；"
            "迁移文件被改动过？解析规则需同步"
        )

    pages = [m for m in menus.values() if m["menu_type"] == 2]
    buttons = [m for m in menus.values() if m["menu_type"] == 3]
    if len(pages) != 5 or len(buttons) != 30:
        raise SystemExit(f"FATAL: 页面 {len(pages)} 行 / 按钮 {len(buttons)} 行（应 5 / 30）")
    for m in pages + buttons:
        if not m["code"].startswith("business:hr:"):
            raise SystemExit(f"FATAL: 菜单 {m['id']} 的 auth_code「{m['code']}」不在 business:hr 域")

    # ---------- ② 角色授权数组 ----------
    roles = {}
    seen = {}
    for array_name, body in ROLE_ARRAY.findall(sql):
        if array_name not in ARRAY_TO_ROLE:
            continue
        ids = UUID.findall(body)
        codes = []
        for uid in ids:
            menu = menus.get(uid)
            if menu is None:
                raise SystemExit(f"FATAL: {array_name} 里的 {uid} 不是本迁移种的菜单行")
            if menu["menu_type"] == 1:
                continue  # 目录没有权限码，不算进码矩阵
            codes.append(menu["code"])
        for role in ARRAY_TO_ROLE[array_name]:
            if role in seen:
                raise SystemExit(f"FATAL: 角色 {role} 被两个数组授权（{seen[role]} / {array_name}）")
            seen[role] = array_name
            roles[role] = codes

    missing = [r for names in ARRAY_TO_ROLE.values() for r in names if r not in roles]
    if missing:
        raise SystemExit(f"FATAL: 迁移里没解析到这些角色的授权数组：{missing}")
    roles[ZERO_ROLE] = []

    order = {m["code"]: i for i, m in enumerate(pages + buttons)}
    for role in roles:
        roles[role] = sorted(set(roles[role]), key=lambda c: order[c])

    return {
        "source": str(migration),
        "codes": [m["code"] for m in pages + buttons],
        "pages": [m["code"] for m in pages],
        "buttons": [m["code"] for m in buttons],
        "menus": pages + buttons,
        "roles": roles,
    }


def render(expect: dict) -> str:
    roles = list(expect["roles"].keys())
    lines = []
    lines.append(f"期望矩阵来源：{expect['source']}")
    lines.append(f"权限码：{len(expect['codes'])}（页面 list {len(expect['pages'])} + 按钮 {len(expect['buttons'])}）")
    lines.append("")
    head = f"{'权限码':44s}" + "".join(f"{r:18s}" for r in roles)
    lines.append(head)
    lines.append("-" * len(head))
    width = 18
    for code in expect["codes"]:
        row = f"{code:44s}"
        for role in roles:
            row += ("G" if code in expect["roles"][role] else ".").ljust(width)
        lines.append(row)
    lines.append("-" * len(head))
    row = f"{'合计（码数）':44s}"
    for role in roles:
        row += str(len(expect["roles"][role])).ljust(width)
    lines.append(row)
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="解析 V0387 人力资源权限矩阵期望值")
    ap.add_argument("--migration", default=str(DEFAULT_MIGRATION))
    ap.add_argument("--json", action="store_true", help="输出 JSON（脚本消费）")
    args = ap.parse_args(argv)

    path = pathlib.Path(args.migration)
    if not path.is_file():
        print(f"FATAL: 迁移文件不存在：{path}", file=sys.stderr)
        return 2
    expect = parse(path)
    if args.json:
        print(json.dumps(expect, ensure_ascii=False, indent=1))
    else:
        print(render(expect))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
