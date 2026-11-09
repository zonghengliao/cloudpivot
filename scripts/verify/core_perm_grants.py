#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""core_perm_grants.py —— 单据核心权限码「零授予」常驻闸

## 为什么要有这道闸（不是猜，是有现场）

2026-11-09 E2E-02 销售域演练发现（卡 `t_c7038fa7` / E2E-FIX-3）：

    business:erp:sales-return:{create,list,status} 在 sys_role_menu 里**一行授予都没有**

V0304 只把这三个码「种」进 `sys_menu`（保证码授得出去，`perm_menu_gate.py` 管的那一档），
并明确写了「本迁移不写 sys_role_menu：授给哪些角色是管理决策」。于是形成一类**静默死锁**：

    菜单承载有 ✔  权限码能勾 ✔  但没有任何角色被授 ✘
    ⇒ 除超管（`*:*:*` 短路）外**所有人都 403**
    ⇒ 演练里 `e2e02_appr` 挂着 销售主管/财务经理/总经理/仓管/财务总监 五个角色，
       PATCH 退货单状态照样 403
    ⇒ 「审批」只能超管自己批 → **职责分离形同虚设（内控红线）**

`perm_menu_gate.py` 挡不住这类：它比的是「注解码 − 菜单 auth_code」，两边都有这个码，
missing_in_menu = 0，闸是绿的。反向的 `unused_in_code` 也不判红。
所以必须再加一道**从菜单到授权**的闸。

## 判据

    ERP 核心码 = sys_menu 里活着的 auth_code，形如 business:erp:<资源>:<动作>，
                 动作 ∈ {create, list, status}
    对每个核心码：非超管角色中**至少一个**真持有（sys_role_menu 里有行）

判红（exit 1）：
    · 任一 ERP 核心码「零授予」（除去 EXEMPT 里带理由的例外）

只报告不判红（INFO）：
    · `business:erp:*` 之外同动作零授予的码（平台治理面：system:* / business:flow:* /
      business:storage:* / business:impexp:* …）。这些可能是**故意**只给超管的配置面，
      判红会逼人往白名单里塞东西；但必须打印出来，免得「静默」本身成为下一个死锁的藏身处。

## 为什么排除 SUPER_ADMIN

超管持 `*:*:*` 通配，`sys_role_menu` 里授不授它都短路放行 —— 一行超管授权等于没授。
`t_c7038fa7` 卡的验收就是「除超管外没人能用」，所以判据里**必须**排除超管，
否则现场那 25 个零授予码会被 1 行超管授权全部掩盖成绿。

## 白名单（**必须带理由**，拒绝裸白名单）

    EXEMPT = {"business:erp:xxx:create": "<为什么这个码暂时可以零授予>"}

理由 < 8 字 / 写成 set / 空理由 → exit 1 并点名（与 perm_menu_gate 同口径）。
当前为空：V0356 之后 ERP 核心码零授予 = 0。

## 用法

    python3 scripts/verify/core_perm_grants.py                  # 连库比对（NIK_DATABASE_*）
    python3 scripts/verify/core_perm_grants.py --json           # 给 CI 解析
    python3 scripts/verify/core_perm_grants.py --self-test      # 闸门自检（红/绿语料，不连库）
    python3 scripts/verify/core_perm_grants.py --menu-grants-file X  # 离线比对快照（无库环境）

快照文件格式（一行一码，`|` 分隔；`--dump` 可直接生成同格式）：

    business:erp:sales-return:list|SALES_CLERK,SALES_SUPERVISOR

连接口径（与 `perm_menu_gate.py` / `erp_invariants.py` 一致）：

    NIK_DATABASE_HOST / NIK_DATABASE_PORT / NIK_DATABASE_USER /
    NIK_DATABASE_NAME / NIK_DATABASE_PASSWORD

退出码：0 = ERP 核心码全部有非超管授权；1 = 存在零授予 / 白名单不合法 / 自检不通过；
        2 = 环境错误（连不上库、核心码基数异常 —— 不算业务失败，run_all.sh 按「无 DB 环境」跳过）。
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

# ==================== 白名单（码 → 理由，**理由必填**） ====================
EXEMPT: dict[str, str] = {}

# 扫描基数下限：低于它说明「连错库 / 迁移没跑 / SQL 被改坏」，**不是**「项目里恰好没有核心码」。
# 基线（2026-11-09 全新库 cp_e2efix1/cp_e2efix3，356 条迁移）：business:erp:*:{create,list,status} = 98 个。
MIN_ERP_CORE = 60

# 理由最短长度：`"todo"` / `"x"` 这种等于没写
MIN_REASON_LEN = 8

ERP_CORE_SQL = r"""
WITH codes AS (
    SELECT DISTINCT m.auth_code
      FROM sys_menu m
     WHERE m.deleted_at IS NULL
       AND coalesce(m.auth_code, '') ~ '^business:erp:[a-z0-9_-]+:(create|list|status)$'
)
SELECT c.auth_code,
       coalesce((
         SELECT string_agg(DISTINCT r.role_code, ',' ORDER BY r.role_code)
           FROM sys_menu m
           JOIN sys_role_menu rm ON rm.menu_id = m.id
           JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
          WHERE m.auth_code = c.auth_code
            AND m.deleted_at IS NULL
            AND r.role_code <> 'SUPER_ADMIN'
       ), '') AS non_super_roles
  FROM codes c
 ORDER BY c.auth_code
"""

# 报告档：ERP 之外、同动作、零授予的码（不判红，只打印）
OTHER_CORE_SQL = r"""
WITH codes AS (
    SELECT DISTINCT m.auth_code
      FROM sys_menu m
     WHERE m.deleted_at IS NULL
       AND coalesce(m.auth_code, '') ~ ':(create|list|status)$'
       AND coalesce(m.auth_code, '') !~ '^business:erp:[a-z0-9_-]+:(create|list|status)$'
)
SELECT c.auth_code
  FROM codes c
 WHERE NOT EXISTS (
         SELECT 1
           FROM sys_menu m
           JOIN sys_role_menu rm ON rm.menu_id = m.id
           JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
          WHERE m.auth_code = c.auth_code
            AND m.deleted_at IS NULL
            AND r.role_code <> 'SUPER_ADMIN')
 ORDER BY c.auth_code
"""


def _psql(sql: str) -> list[list[str]]:
    host = os.environ.get("NIK_DATABASE_HOST", "127.0.0.1")
    port = os.environ.get("NIK_DATABASE_PORT", "5432")
    user = os.environ.get("NIK_DATABASE_USER", "cloudpivot")
    name = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    try:
        r = subprocess.run(
            ["psql", "-h", host, "-p", str(port), "-U", user, "-d", name,
             "-At", "-F", "|", "-P", "pager=off", "-v", "ON_ERROR_STOP=1", "-c", sql],
            capture_output=True, text=True, env=env, timeout=60,
        )
    except FileNotFoundError as e:  # pragma: no cover - 环境缺 psql
        raise RuntimeError(f"没装 psql：{e}") from e
    except subprocess.TimeoutExpired as e:  # pragma: no cover
        raise RuntimeError("psql 60s 超时（库连不上？）") from e
    if r.returncode != 0:
        raise RuntimeError(f"psql 执行失败（rc={r.returncode}）：{(r.stderr or '').strip()[:400]}")
    out = []
    for line in (r.stdout or "").splitlines():
        if line.strip():
            out.append(line.split("|"))
    return out


def _parse_rows(rows: list[list[str]]) -> dict[str, list[str]]:
    """`[[code, 'A,B']]` → `{code: ['A','B']}`（空串 = 零授予）。"""
    out: dict[str, list[str]] = {}
    for row in rows:
        code = row[0].strip()
        roles = [x.strip() for x in (row[1] if len(row) > 1 else "").split(",") if x.strip()]
        out[code] = roles
    return out


def validate_exempt(exempt: dict[str, str]) -> list[str]:
    """白名单必须是 dict[code, 理由]，理由够长；否则等于把闸关掉。"""
    errs: list[str] = []
    if not isinstance(exempt, dict):
        return ["EXEMPT 必须是 dict（码 → 理由），写成 set 等于裸白名单"]
    for code, reason in exempt.items():
        if not isinstance(code, str) or not code.startswith("business:erp:"):
            errs.append(f"白名单键必须是 business:erp:* 权限码：{code!r}")
            continue
        if not isinstance(reason, str) or len(reason.strip()) < MIN_REASON_LEN:
            errs.append(f"白名单理由太短（<{MIN_REASON_LEN} 字），等于没写：{code}")
    return errs


def evaluate(grants: dict[str, list[str]], exempt: dict[str, str] | None = None) -> dict:
    """纯函数：给定 {code: [role,...]} 判零授予。

    `grants` 的 key 就是「ERP 核心码」全集（SQL 已经过滤过正则），roles 里已排除超管。
    """
    exempt = exempt or {}
    missing = sorted(c for c, roles in grants.items() if not roles and c not in exempt)
    exempted = sorted(c for c, roles in grants.items() if not roles and c in exempt)
    granted = sorted(c for c, roles in grants.items() if roles)
    return {
        "code_count": len(grants),
        "granted_count": len(granted),
        "missing": missing,
        "exempted": exempted,
    }


def self_test() -> bool:
    """闸门自检：红/绿语料各跑一遍，证明「真能发现缺失授权」。"""
    ok = True

    def check(name: str, cond: bool, detail: str = "") -> None:
        nonlocal ok
        if cond:
            print(f"  ✅ {name}")
        else:
            ok = False
            print(f"  ❌ {name} {detail}")

    print("== 自检 1：零授予的 ERP 核心码必须被抓到 ==")
    r = evaluate({
        "business:erp:sales-return:list": [],
        "business:erp:sales-return:create": ["SALES_CLERK"],
        "business:erp:sales-return:status": ["SALES_SUPERVISOR"],
    })
    check("报出零授予的 list 码", r["missing"] == ["business:erp:sales-return:list"], str(r))

    print("== 自检 2：只有超管的授权等于没授（SQL 已排除超管，这里模拟空授权） ==")
    r = evaluate({"business:erp:receipt-refund:create": []})
    check("超管短路不算授予", r["missing"] == ["business:erp:receipt-refund:create"], str(r))

    print("== 自检 3：全部有非超管授权 → 绿 ==")
    r = evaluate({
        "business:erp:unit:list": ["PURCHASE_MANAGER"],
        "business:erp:unit:create": ["PURCHASE_MANAGER"],
    })
    check("无不变量违例", r["missing"] == [] and r["granted_count"] == 2, str(r))

    print("== 自检 4：白名单必须带理由（裸白名单 = 关闸） ==")
    check("空理由被抓", validate_exempt({"business:erp:x:create": ""}) != [])
    check("短理由被抓", validate_exempt({"business:erp:x:create": "todo"}) != [])
    check("写成 set 被抓", validate_exempt({"business:erp:x:create"}) != [])  # type: ignore[arg-type]
    check("带理由放行", validate_exempt({"business:erp:x:create": "2026-12 上线后删除"}) == [])
    r = evaluate({"business:erp:x:create": []}, {"business:erp:x:create": "2026-12 上线后删除"})
    check("白名单内的码不判红但会列进 exempted", r["missing"] == [] and r["exempted"] == ["business:erp:x:create"], str(r))

    print("== 自检 5：正则口径（SQL 侧）与判据一致 ==")
    import re
    pat = re.compile(r'^business:erp:[a-z0-9_-]+:(create|list|status)$')
    check("sales-return:create 命中", bool(pat.match("business:erp:sales-return:create")))
    check("receipt-refund:allocate 不命中（非核心动作）", not pat.match("business:erp:receipt-refund:allocate"))
    check("system:change-log:list 不命中（非 ERP）", not pat.match("system:change-log:list"))
    check("business:erp:unit:detailed 不命中（前缀伪装）",
          not pat.match("business:erp:unit:detailed"))

    print(("自检结果：" + ("PASS" if ok else "FAIL")))
    return ok


def main() -> int:
    ap = argparse.ArgumentParser(description="单据核心权限码零授予闸（ERP 域）")
    ap.add_argument("--json", action="store_true", help="JSON 输出（CI 解析）")
    ap.add_argument("--self-test", action="store_true", help="闸门自检，不连库")
    ap.add_argument("--menu-grants-file", default=None, help="离线快照（每行 auth_code|ROLE1,ROLE2）")
    ap.add_argument("--dump", action="store_true", help="把当前 ERP 核心码与授予打成快照格式后退出")
    args = ap.parse_args()

    exempt_errs = validate_exempt(EXEMPT)
    if exempt_errs:
        print("白名单不合法（裸白名单 = 把闸关掉）：", file=sys.stderr)
        for e in exempt_errs:
            print(f"  · {e}", file=sys.stderr)
        return 1

    if args.self_test:
        return 0 if self_test() else 1

    # ---- 取数：库 or 离线快照 ----
    if args.menu_grants_file:
        path = Path(args.menu_grants_file)
        if not path.is_file():
            print(f"快照文件不存在：{path}", file=sys.stderr)
            return 2
        grants = _parse_rows([
            line.split("|", 1) for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.startswith("#")
        ])
        source = f"快照 {path}"
        others: list[str] = []
    else:
        try:
            grants = _parse_rows(_psql(ERP_CORE_SQL))
            others = [row[0] for row in _psql(OTHER_CORE_SQL)]
        except RuntimeError as e:
            print(f"环境错误：{e}", file=sys.stderr)
            print("  （无库环境用 --menu-grants-file <快照>；CI 里由 run_all.sh 按「无 DB 环境」跳过）",
                  file=sys.stderr)
            return 2
        db = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
        source = f"库 {db}"

    if args.dump:
        for code in sorted(grants):
            print(f"{code}|{','.join(grants[code])}")
        return 0

    # ---- 基数防假绿：核心码太少 = 连错库 / 迁移没跑 / SQL 坏了 ----
    if len(grants) < MIN_ERP_CORE:
        print(f"环境错误：ERP 核心码只有 {len(grants)} 个（下限 {MIN_ERP_CORE}）—— "
              f"连错库或迁移没跑？口径见脚本头注释", file=sys.stderr)
        return 2

    result = evaluate(grants, EXEMPT)
    result["source"] = source
    result["other_zero_grant"] = others

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(f"单据核心权限码零授予闸  （{source}）")
        print(f"  ERP 核心码 {result['code_count']} 个，有非超管授权 {result['granted_count']} 个")
        if result["exempted"]:
            print(f"  白名单豁免 {len(result['exempted'])} 个：")
            for code in result["exempted"]:
                print(f"    · {code} —— {EXEMPT[code]}")
        if result["missing"]:
            print(f"  ❌ 零授予（除超管外无人可用）{len(result['missing'])} 个：")
            for code in result["missing"]:
                print(f"    · {code}")
            print("  修法：发迁移给合适的业务角色补授权（单据建单与审批要分属不同角色 —— 职责分离），"
                  "不要往白名单里塞。")
        else:
            print("  ✅ 所有 ERP 核心码都至少授予了 1 个非超管角色")
        if others:
            print(f"  ℹ 非 ERP 域零授予 {len(others)} 个（不判红：平台治理/配置面可能故意只给超管）：")
            for code in others[:20]:
                print(f"    · {code}")
            if len(others) > 20:
                print(f"    … 另有 {len(others) - 20} 个")

    return 1 if result["missing"] else 0


if __name__ == "__main__":
    sys.exit(main())
