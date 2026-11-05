#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""orphan_button_gate.py —— 孤儿按钮常驻闸：**角色拿到的按钮码，必须至少有一个「看得见的父页」**

## 为什么要有这道闸（不是猜，是有现场）

角色授权走「菜单树勾选」：`sys_role_menu` 挂的是 `sys_menu` 的行，按钮（`menu_type=3`）
挂在页面（`menu_type=2`）下。于是有一类结构性缺陷：

    角色持有按钮码 X，X 的按钮节点挂在页面 P 下，而 P 的码没授给该角色
    ⇒ 在权限树里，这个按钮挂在一个该角色永远看不见的页面下（「孤儿按钮」）

2026-11-06 生产 `cloudpivot@172.18.10.144` 实测 8 对：`FLOW_CONFIG_APPROVER` / `FINANCE_DIRECTOR`
持有 `business:flow:approve` / `instance:create` / `instance:read` / `instance:withdraw`
4 个实例侧按钮，但父页 `/flow/def`（`business:flow:read`）没授。根因是 V0124 写这 5 个按钮时
「发起审批 / 审批中心」两个页面还不存在（V0128 才种），按钮只能先挂在流程定义页下，
之后再没人归位。修法见 `V0259__flow_instance_button_parentage`（父页归位，零授权变化）。

**本脚本是「防再长」的那道闸**：人工核对只做一次，不做成常驻检查，下一个按钮还会挂错父页。

## 判据（两档，只有一档判红）

对每个 (角色, 码)，把它被授予的**全部按钮节点**逐个向上走祖先链，
看有没有「带码祖先」没授给该角色：

    orphan_pairs     = all_blocked 的 (角色,码)                  ← **必须为 0**，否则 exit 1
        该码的每个按钮节点都挂在角色看不见的页面下 = 真孤儿：按钮不可达。
    redundant_grants = 部分节点 blocked、但同一码另有可达节点    ← 只报告，不判红
        同一 auth_code 可能挂在多个页面上（本仓有 26 例页面与其按钮共码）。V0258 按码补授权时
        会把「该码的全部节点」一起写进去 ⇒ 某角色在一页上可达、在另一页上不可达。
        功能上没有损失（码已经拿到了），只是那个节点在树上不可达，属于冗余授权。
        判红只会逼人拆授权去凑绿，反而破坏权限树勾选的一致性。

档二是从实测里长出来的：全量装配库上 `WAREHOUSE_MANAGER × business:erp:doc:cancel-{cascade,impact}`
就是这种形态（同一码挂在 /purchase、/stock-in、/payable 三个页面，该角色只有前两页的码）。

## 另外两处口径（都有实测依据）

- **`SUPER_ADMIN` 不参与判红**：超管在 `system/auth/model.rs::is_super_admin` 按
  `role_code = SUPER_ADMIN` 短路，令牌里是 `*:*:*` 通配 ⇒ 它的 `sys_role_menu` 行对功能零影响
  （V0258 的矩阵也明令不含超管）。装配库里 `S__voucher_three_state_permissions` 会把 3 个
  凭证按钮授给超管而不授 `voucher:list` 页面码 —— 那不是缺陷，只报告。
- **软删角色不参与判红**：软删角色登录不了，不影响在岗用户；仍存在的孤儿作为
  `soft_deleted_role_orphans` 只报告。

## 防假绿（本仓 perm_menu_gate 栽过的两个坑，这里都有对应防线）

1. **扫描基数断言**：活跃 `sys_menu` < 100 行、或「带码父页的按钮节点」< 50 个 ⇒ exit 2
   （连错库 / 迁移没跑完 / 表名不对），绝不因为「查出 0 行」就判绿；
2. **判据自检**（`--self-test`，不连库）：SQL 文本必须仍包含四段关键谓词
   （`menu_type = 3` / `deleted_at IS NULL` / `coalesce(a.auth_code` / `NOT EXISTS`），
   且分档与 verdict 映射用红/绿语料验一遍 —— 判据被谁改坏了，自检先红。

## 用法

    python3 scripts/verify/orphan_button_gate.py                       # 连库（NIK_DATABASE_*）
    python3 scripts/verify/orphan_button_gate.py --json               # 给 CI 解析
    python3 scripts/verify/orphan_button_gate.py --self-test          # 判据自检，不连库
    python3 scripts/verify/orphan_button_gate.py --dsn postgres://…   # 任意库
    python3 scripts/verify/orphan_button_gate.py --remote root@172.18.10.144 --db cloudpivot
                                                                      # 生产只读核对（psql 走 peer）

连接口径（与 `perm_menu_gate.py` / `erp_invariants.py` 一致）：
    NIK_DATABASE_HOST / NIK_DATABASE_PORT / NIK_DATABASE_USER /
    NIK_DATABASE_NAME / NIK_DATABASE_PASSWORD

退出码：0 = 孤儿 0 对；1 = 有孤儿（点名到 (角色,码,不可达父页)）；
        2 = 环境错误（连不上库 / 基数异常 —— 不算业务失败，`check.sh` 按「无 DB 环境」跳过）。

## 基线（2026-11-06 实测，写死供对照，**不作为判据**）

    生产 cloudpivot@144（V0259 部署前）：活跃 sys_menu 335 行 / 带码父页的按钮 242 个 /
        按钮授权 619 行 → orphan_pairs = 8（FLOW_CONFIG_APPROVER、FINANCE_DIRECTOR ×
        business:flow:approve / instance:create / instance:read / instance:withdraw）
    全量装配库（190 条版本化迁移 + V0259 + 10 个种子 + bootstrap）：orphan_pairs = 0，
        redundant_grants = 2（WAREHOUSE_MANAGER × doc:cancel-{cascade,impact}，见上）
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

SUPER_ADMIN_ROLE_CODE = "SUPER_ADMIN"

# ==================== 判据 SQL ====================

# (角色, 码) 粒度：把该角色被授的每个按钮节点向上走祖先链，判「该节点是否可达」
#   blocked = 至少一个「带码祖先」没授给该角色
QUERY_PAIRS = """
WITH RECURSIVE btn(role_id, btn_id, code, anc_id) AS (
    SELECT rm.role_id, m.id, m.auth_code, m.parent_id
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
      JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 3 AND m.deleted_at IS NULL
    UNION
    SELECT b.role_id, b.btn_id, b.code, p.parent_id
      FROM btn b
      JOIN sys_menu p ON p.id = b.anc_id AND p.deleted_at IS NULL
),
mark AS (
    SELECT b.role_id, b.btn_id, b.code,
           bool_or(coalesce(a.auth_code, '') <> '' AND NOT EXISTS (
               SELECT 1
                 FROM sys_role_menu rm2
                 JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
                WHERE rm2.role_id = b.role_id AND m2.auth_code = a.auth_code)) AS blocked
      FROM btn b
      JOIN sys_menu a ON a.id = b.anc_id
     GROUP BY b.role_id, b.btn_id, b.code
),
pair AS (
    SELECT k.role_id, k.code,
           bool_and(k.blocked) AS all_blocked,
           count(*) AS button_nodes,
           count(*) FILTER (WHERE NOT k.blocked) AS reachable_nodes,
           string_agg(DISTINCT coalesce(pm.path, '-') || '('
                      || coalesce(nullif(pm.auth_code, ''), '无码') || ')',
                      ', ' ORDER BY coalesce(pm.path, '-') || '('
                      || coalesce(nullif(pm.auth_code, ''), '无码') || ')')
             FILTER (WHERE k.blocked) AS blocked_parents
      FROM mark k
      JOIN sys_menu mm ON mm.id = k.btn_id
      LEFT JOIN sys_menu pm ON pm.id = mm.parent_id AND pm.deleted_at IS NULL
     GROUP BY k.role_id, k.code
)
SELECT r.role_code, p.code, p.all_blocked, p.button_nodes, p.reachable_nodes,
       coalesce(p.blocked_parents, '')
  FROM pair p
  JOIN sys_role r ON r.id = p.role_id
 WHERE p.all_blocked OR p.reachable_nodes < p.button_nodes
 ORDER BY 1, 2
"""

# 软删角色上的孤儿角色数：只报告（登录不了，不影响在岗用户）
QUERY_SOFTDEL = """
WITH RECURSIVE btn(role_id, btn_id, anc_id) AS (
    SELECT rm.role_id, m.id, m.parent_id
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NOT NULL
      JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 3 AND m.deleted_at IS NULL
    UNION
    SELECT b.role_id, b.btn_id, p.parent_id
      FROM btn b
      JOIN sys_menu p ON p.id = b.anc_id AND p.deleted_at IS NULL
),
mark AS (
    SELECT b.role_id, b.btn_id,
           bool_or(coalesce(a.auth_code, '') <> '' AND NOT EXISTS (
               SELECT 1
                 FROM sys_role_menu rm2
                 JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
                WHERE rm2.role_id = b.role_id AND m2.auth_code = a.auth_code)) AS blocked
      FROM btn b
      JOIN sys_menu a ON a.id = b.anc_id
     GROUP BY b.role_id, b.btn_id
)
SELECT count(*) FROM (
    SELECT role_id FROM mark GROUP BY role_id HAVING bool_and(blocked)) z
"""

# 基数（防假绿）：活跃菜单行 / 带码父页的按钮节点数 / 按钮授权行数
QUERY_CARDINALITY = """
SELECT (SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL),
       (SELECT count(*)
          FROM sys_menu m
          JOIN sys_menu p ON p.id = m.parent_id AND p.deleted_at IS NULL
         WHERE m.deleted_at IS NULL AND m.menu_type = 3 AND coalesce(p.auth_code, '') <> ''),
       (SELECT count(*)
          FROM sys_role_menu rm
          JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 3 AND m.deleted_at IS NULL)
"""

MIN_MENU_ROWS = 100                  # 活跃菜单行下限（连错库 / 迁移没跑完）
MIN_BUTTONS_WITH_CODE_PARENT = 50    # 带码父页的按钮节点下限（判据扫描面异常即环境错误）

# 判据自检：SQL 里必须仍然存在的四段谓词（谁把它们删了 = 闸被关掉）
REQUIRED_PREDICATES = [
    ("menu_type = 3", "只看按钮节点（页面节点 menu_type=2 不参与父码判定）"),
    ("deleted_at IS NULL", "软删的菜单/角色不参与判定"),
    ("coalesce(a.auth_code", "只对「带码祖先」追责，无码目录/页面不参与判定"),
    ("NOT EXISTS", "必须真的反查「祖先页码是否授给该角色」"),
]


# ==================== 连接 ====================

def run_sql(sql: str, dsn: str | None, remote: str | None, db: str | None) -> str:
    if remote:
        cmd = ["ssh", "-o", "BatchMode=yes", remote,
               f"su - postgres -c 'psql -d {db} -At -F \"|\" -f -'"]
        r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    elif dsn:
        r = subprocess.run(["psql", dsn, "-At", "-F", "|", "-f", "-"],
                           input=sql, capture_output=True, text=True)
    else:
        host = os.environ.get("NIK_DATABASE_HOST", "127.0.0.1")
        port = os.environ.get("NIK_DATABASE_PORT", "5432")
        user = os.environ.get("NIK_DATABASE_USER", "cloudpivot")
        name = os.environ.get("NIK_DATABASE_NAME", "cloudpivot")
        env = dict(os.environ)
        env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", env.get("PGPASSWORD", ""))
        r = subprocess.run(
            ["psql", "-h", host, "-p", str(port), "-U", user, "-d", name, "-At", "-F", "|", "-f", "-"],
            input=sql, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        raise RuntimeError((r.stderr or "").strip()[:500] or f"psql 退出码 {r.returncode}")
    return r.stdout.strip()


def fetch(dsn, remote, db):
    """读三份数据：判定行、软删角色孤儿数、基数三元组。"""
    rows = []
    for line in run_sql(QUERY_PAIRS, dsn, remote, db).splitlines():
        if not line.strip():
            continue
        role_code, code, all_blocked, nodes, reachable, parents = (line.split("|") + [""] * 6)[:6]
        rows.append({"role_code": role_code, "code": code,
                     "all_blocked": all_blocked == "t",
                     "button_nodes": int(nodes or 0), "reachable_nodes": int(reachable or 0),
                     "blocked_parents": parents})
    softdel_raw = run_sql(QUERY_SOFTDEL, dsn, remote, db).splitlines()
    softdel = int(softdel_raw[0] or 0) if softdel_raw else 0
    card = run_sql(QUERY_CARDINALITY, dsn, remote, db).splitlines()[0].split("|")
    menu_rows, buttons, button_grants = (int(x or 0) for x in card[:3])
    return split_rows(rows), softdel, menu_rows, buttons, button_grants


def split_rows(rows):
    """三档：orphans（判红）/ redundant（只报告）/ super_admin（只报告）。"""
    orphans, redundant, super_admin = [], [], []
    for r in rows:
        if not r.get("all_blocked"):
            redundant.append(r)
        elif r.get("role_code") == SUPER_ADMIN_ROLE_CODE:
            super_admin.append(r)
        else:
            orphans.append(r)
    return {"orphans": orphans, "redundant": redundant, "super_admin": super_admin}


# ==================== 判定（纯函数，自检也用它） ====================

def verdict(buckets, menu_rows, buttons):
    """返回 (退出码, 原因)。0=绿 1=红 2=环境错误。"""
    if menu_rows < MIN_MENU_ROWS:
        return 2, f"活跃 sys_menu 只有 {menu_rows} 行（下限 {MIN_MENU_ROWS}）→ 连错库或迁移没跑完"
    if buttons < MIN_BUTTONS_WITH_CODE_PARENT:
        return 2, (f"带码父页的按钮节点只有 {buttons} 个（下限 {MIN_BUTTONS_WITH_CODE_PARENT}）"
                   f"→ 判据扫描面异常，按环境错误处理，不判绿")
    if buckets["orphans"]:
        return 1, f"孤儿 (角色,码) {len(buckets['orphans'])} 对"
    return 0, "孤儿 (角色,码) = 0"


# ==================== 自检（不连库） ====================

SELF_TEST_CASES = [
    # (buckets, menu_rows, buttons, 期望退出码, 说明)
    ({"orphans": [], "redundant": [], "super_admin": []}, 335, 242, 0, "无孤儿 → 绿"),
    ({"orphans": [{"role_code": "R", "code": "a:b:c"}], "redundant": [], "super_admin": []},
     335, 242, 1, "有 1 对孤儿 → 红"),
    ({"orphans": [], "redundant": [{"role_code": "R", "code": "a:b:c"}], "super_admin": []},
     335, 242, 0, "只有冗余节点授权（同码另有可达节点）→ 绿"),
    ({"orphans": [], "redundant": [], "super_admin": [{"role_code": "SUPER_ADMIN", "code": "a:b:c"}]},
     335, 242, 0, "超管的行对功能零影响（*:*:* 短路）→ 绿"),
    ({"orphans": [], "redundant": [], "super_admin": []}, 3, 242, 2, "活跃菜单 3 行（连错库）→ 环境错误"),
    ({"orphans": [], "redundant": [], "super_admin": []}, 335, 0, 2, "扫描面 0 个按钮 → 环境错误，不判绿"),
]

SPLIT_CASES = [
    # (输入行, 期望 orphans / redundant / super_admin 数)
    ([{"role_code": "A", "code": "x", "all_blocked": True},
      {"role_code": "A", "code": "y", "all_blocked": False},
      {"role_code": "SUPER_ADMIN", "code": "z", "all_blocked": True}], 1, 1, 1),
    ([], 0, 0, 0),
]


def self_test() -> bool:
    bad = 0
    print("=" * 92)
    print("判据自检：SQL 谓词 + 分档 + verdict 红/绿语料（不连库）")
    print("=" * 92)
    sql_all = QUERY_PAIRS + QUERY_SOFTDEL
    for needle, why in REQUIRED_PREDICATES:
        ok = needle in sql_all
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} SQL 谓词保留：`{needle}`  —— {why}")
    for rows, e_o, e_r, e_s in SPLIT_CASES:
        got = split_rows(rows)
        ok = (len(got["orphans"]) == e_o and len(got["redundant"]) == e_r
              and len(got["super_admin"]) == e_s)
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} 分档 orphans={len(got['orphans'])} redundant={len(got['redundant'])} "
              f"super={len(got['super_admin'])}（期望 {e_o}/{e_r}/{e_s}）")
    for buckets, menu_rows, buttons, expect, label in SELF_TEST_CASES:
        rc, why = verdict(buckets, menu_rows, buttons)
        ok = rc == expect
        bad += 0 if ok else 1
        print(f"{'✔' if ok else '✘'} verdict → {rc}（期望 {expect}）：{label}  [{why}]")
    total = len(REQUIRED_PREDICATES) + len(SPLIT_CASES) + len(SELF_TEST_CASES)
    print("-" * 92)
    print(f"{total - bad}/{total} 通过" + ("" if not bad else f"；✗ {bad} 条不符"))
    return bad == 0


# ==================== main ====================

def main() -> int:
    ap = argparse.ArgumentParser(description="孤儿按钮常驻闸（只读）")
    ap.add_argument("--dsn", default=None, help="任意库的 psql DSN（如 postgres://user:pass@host/db）")
    ap.add_argument("--remote", default=None, help="远端主机（如 root@172.18.10.144，走 su postgres peer 只读）")
    ap.add_argument("--db", default=None, help="--remote 时的库名（如 cloudpivot）")
    ap.add_argument("--json", action="store_true", help="JSON 输出（给 CI / 监控）")
    ap.add_argument("--self-test", action="store_true", dest="self_test", help="判据自检，不连库")
    args = ap.parse_args()

    if args.self_test:
        return 0 if self_test() else 1

    try:
        buckets, softdel, menu_rows, buttons, button_grants = fetch(args.dsn, args.remote, args.db)
    except Exception as e:  # noqa: BLE001
        print(f"✗ 拿不到库数据：{e}", file=sys.stderr)
        print("  （无库环境时 check.sh 按「无 DB 环境」跳过；本机要跑先 . /etc/cloudpivot/cloudpivot.env）",
              file=sys.stderr)
        return 2

    rc, why = verdict(buckets, menu_rows, buttons)

    if args.json:
        print(json.dumps(dict(ok=rc == 0, exit=rc, reason=why,
                              orphan_pairs=buckets["orphans"],
                              orphan_pair_count=len(buckets["orphans"]),
                              redundant_node_grants=buckets["redundant"],
                              super_admin_blocked=buckets["super_admin"],
                              soft_deleted_role_orphans=softdel,
                              menu_rows=menu_rows, buttons_with_code_parent=buttons,
                              button_grants=button_grants),
                         ensure_ascii=False, indent=2))
        return rc

    print("=" * 92)
    print("孤儿按钮常驻闸 · orphan_button_gate.py（角色拿到的按钮码 ⇒ 至少有一个可达父页）")
    print("=" * 92)
    print(f"扫描面：活跃 sys_menu {menu_rows} 行 / 带码父页的按钮节点 {buttons} 个 / 按钮授权 {button_grants} 行")
    print("-" * 92)

    if rc == 1:
        print(f"✗ 孤儿 (角色,码) = {len(buckets['orphans'])}  ← 必须为 0！这些按钮挂在该角色看不见的页面下：")
        for p in buckets["orphans"]:
            print(f"  · {p['role_code']:<22} {p['code']:<34} 不可达父页 {p['blocked_parents']}"
                  f"（该角色未持有父页码）")
        print("  修法：① 该角色确实该看该页面 ⇒ 补授父页码（新迁移，按码判重 + down）；")
        print("        ② 不该看 ⇒ 把按钮挂到别的父页（改 sys_menu.parent_id，零授权变化，如 V0259）。")
    elif rc == 0:
        print("✓ 孤儿 (角色,码) = 0   （每个按钮码都至少有一个角色看得见的父页）")
    else:
        print(f"⚠ {why}")
        print("  → 按「环境错误」退出（2），check.sh 不判红但会打印原因")

    for p in buckets["redundant"]:
        print(f"ℹ 冗余节点授权（同码另有可达节点，不判红）：{p['role_code']} {p['code']} @ {p['blocked_parents']}"
              f"（{p['reachable_nodes']}/{p['button_nodes']} 个节点可达）")
    for p in buckets["super_admin"]:
        print(f"ℹ 超管行（*:*:* 短路，对功能零影响，不判红）：{p['role_code']} {p['code']} @ {p['blocked_parents']}")
    if softdel:
        print(f"ℹ 软删角色上还有 {softdel} 个角色级孤儿（只报告：软删角色登录不了）")
    return rc


if __name__ == "__main__":
    sys.exit(main())
