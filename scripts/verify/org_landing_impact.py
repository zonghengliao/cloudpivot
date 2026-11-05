#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""组织落位「影响面零变化」核算（【P0-FIX-2B-A】t_db82c38e）。

落位只补 `sys_user.dept_id` 一个字段，**不许改变任何人今天能看到什么**。
本脚本就是这句话的可执行断言：对每个在用账号算出

    (kind, dept_ids, self_only) + 5 张受控表的可见行数

落位前后各拍一次，两次必须逐账号完全一致；不一致就退出 1（说明落位改到了不该改的东西）。

## 口径来源（不重新发明）

- `backend/src/business/shared/principal.rs::RESOLVE_SQL`（原样搬过来，`?` 换成字面量）；
- `principal.rs::merge_scope_rows`（多角色并集 / 超管与 ALL 短路清空 dept_ids /
  部门类解析不出部门 → 降级 SelfOnly，S10/S11）；
- `shared/scope.rs::filter_for`（All→恒真；部门集合→`dept_id = ANY`；self_only→`OR owner = me`；
  无部门列无归属列→拒绝全部）。

5 张受控表（`shared/scope.rs` + 各业务模块的本地锚点）：

| 表 | 别名 | 部门列 | 归属列 | 额外并集（本脚本不计入，见下） |
|---|---|---|---|---|
| `sys_user` | u | dept_id | id | — |
| `biz_task` | t | dept_id | owner_id | 参与关系 id 数组 |
| `biz_project` | p | dept_id | owner_id | 负责人/创建人/成员/我有任务 |
| `biz_budget` | b | dept_id | — | — |
| `biz_expense_report` | e | dept_id | — | — |

> 任务/项目的「参与关系并集」是按 **user_id** 表达的（我负责/我创建/我参与），
> 与 dept_id 无关，落位不可能改变它 —— 所以这里只算 scope 片段本身。
> 断言的是「scope 片段 + 结果行数不变」，比「算出来一个总数」更严格：同一片段必然同一行数。

## 用法

    # 落位前
    python3 scripts/verify/org_landing_impact.py --out /tmp/impact-before.json

    # 落位后
    python3 scripts/verify/org_landing_impact.py --out /tmp/impact-after.json

    # 对照（不一致 → 退出 1）
    python3 scripts/verify/org_landing_impact.py --before /tmp/impact-before.json \
                                                 --after  /tmp/impact-after.json

    # 一步到位：落位前后各拍一次并对照（apply 由调用方在两次之间执行）
    python3 scripts/verify/org_landing_impact.py --apply-wrapper 'bash scripts/datafix/org_landing/apply.sh'

环境变量（默认取 NIK_DATABASE_*，与本仓其它巡检脚本同口径）：
    NIK_DATABASE_HOST/PORT/USER/PASSWORD/NAME，或 IMPACT_DB_* 覆盖。
"""

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime
from typing import Any

DB = dict(
    host=os.environ.get("IMPACT_DB_HOST", os.environ.get("NIK_DATABASE_HOST", "127.0.0.1")),
    port=os.environ.get("IMPACT_DB_PORT", os.environ.get("NIK_DATABASE_PORT", "5432")),
    user=os.environ.get("IMPACT_DB_USER", os.environ.get("NIK_DATABASE_USER", "cloudpivot")),
    name=os.environ.get("IMPACT_DB_NAME", os.environ.get("NIK_DATABASE_NAME", "cloudpivot")),
    password=os.environ.get("IMPACT_DB_PASSWORD", os.environ.get("NIK_DATABASE_PASSWORD", "")),
)

SUPER_ROLE = "SUPER_ADMIN"

# 与 scope.rs::filter_for 的 5 张受控表一一对应（alias 用代码里的别名，便于对照）
TABLES = [
    # key,                    表名,                  别名, 部门列,      归属列
    ("sys_user",            "sys_user",            "u", "dept_id", "id"),
    ("biz_task",            "biz_task",            "t", "dept_id", "owner_id"),
    ("biz_project",         "biz_project",         "p", "dept_id", "owner_id"),
    ("biz_budget",          "biz_budget",          "b", "dept_id", None),
    ("biz_expense_report",  "biz_expense_report",  "e", "dept_id", None),
]

KIND_LABEL = {1: "All", 2: "Dept", 3: "DeptAndChild", 4: "SelfOnly", 5: "Custom"}


# --------------------------------------------------------------------------
# 取数
# --------------------------------------------------------------------------
def psql(sql, tuples=True):
    env = dict(os.environ)
    env["PGPASSWORD"] = DB["password"]
    args = ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
            "-d", DB["name"], "-X", "-q", "-v", "ON_ERROR_STOP=1"]
    if tuples:
        args += ["-tAF", "\x1f"]
    args += ["-c", sql]
    out = subprocess.run(args, capture_output=True, text=True, env=env, timeout=120)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()[:400]}\nSQL: {sql[:400]}")
    rows = []
    if tuples:
        for line in out.stdout.split("\n"):
            if line.strip() == "":
                continue
            rows.append(line.split("\x1f"))
    return rows


# --------------------------------------------------------------------------
# principal.rs::RESOLVE_SQL（原样搬运；`?` 依次是 user_id / user_id / 超级管理员 role_code）
# --------------------------------------------------------------------------
def resolve_sql(uid, super_role):
    return f"""
WITH my_roles AS (
    SELECT r.id, r.role_code, r.data_scope
    FROM sys_user u
    INNER JOIN sys_user_role ur ON ur.user_id = u.id
        AND (ur.expire_time IS NULL OR ur.expire_time > now())
    INNER JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1
    WHERE u.id = '{uid}' AND u.deleted_at IS NULL
), me AS (
    SELECT id, dept_id FROM sys_user WHERE id = '{uid}' AND deleted_at IS NULL
), subtree AS (
    SELECT c.id AS dept_id FROM me
    INNER JOIN sys_dept d ON d.id = me.dept_id
    CROSS JOIN LATERAL (
        WITH RECURSIVE dept_tree AS (
            SELECT id FROM sys_dept WHERE id = d.id AND deleted_at IS NULL
            UNION ALL
            SELECT ch.id FROM sys_dept ch
            INNER JOIN dept_tree t ON ch.parent_id = t.id
            WHERE ch.deleted_at IS NULL
        ) SELECT id FROM dept_tree
    ) c
)
SELECT 'ME' AS kind, dept_id FROM me
UNION ALL SELECT 'SUPER', NULL::uuid
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE role_code = '{super_role}')
UNION ALL SELECT 'ALL', NULL::uuid
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 1)
UNION ALL SELECT 'DEPT_ROLE', NULL::uuid
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope IN (2, 3, 5))
UNION ALL SELECT 'SELF', NULL::uuid
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 4)
UNION ALL SELECT 'DEPT', me.dept_id FROM me
    WHERE me.dept_id IS NOT NULL
      AND EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 2)
UNION ALL SELECT 'DEPT_CHILD', s.dept_id FROM subtree s
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 3)
UNION ALL SELECT 'CUSTOM', rd.dept_id FROM my_roles mr
    INNER JOIN sys_role_dept rd ON rd.role_id = mr.id
    WHERE mr.data_scope = 5
"""


def merge_scope_rows(rows):
    """principal.rs::merge_scope_rows 的 Python 镜像（规则一一对应，别在这里加新规则）。"""
    me = None
    is_super = all_ = self_only = has_dept_role = saw_child = saw_custom = False
    dept_ids = []
    for kind, dept in rows:
        if kind == "ME":
            me = dept if dept else None
        elif kind == "SUPER":
            is_super = True
        elif kind == "ALL":
            all_ = True
        elif kind == "SELF":
            self_only = True
        elif kind == "DEPT_ROLE":
            has_dept_role = True
        elif kind == "DEPT":
            has_dept_role = True
            if dept:
                dept_ids.append(dept)
        elif kind == "DEPT_CHILD":
            has_dept_role = True
            saw_child = True
            if dept:
                dept_ids.append(dept)
        elif kind == "CUSTOM":
            has_dept_role = True
            saw_custom = True
            if dept:
                dept_ids.append(dept)
    # 去重保持出现顺序
    seen, uniq = set(), []
    for d in dept_ids:
        if d not in seen:
            seen.add(d)
            uniq.append(d)
    dept_ids = uniq

    if is_super or all_:
        dept_ids = []
        kind, self_only = 1, False
    elif dept_ids:
        kind = 3 if saw_child else (5 if saw_custom else 2)
        self_only = self_only
    else:
        kind, self_only = 4, True
    return dict(is_super=is_super, own_dept_id=me, kind=kind,
                dept_ids=dept_ids, self_only=self_only, has_dept_role=has_dept_role)


def scope_predicate(p, tbl, alias, dept_col, owner_col, uid):
    """scope.rs::filter_for 的等价谓词（返回 SQL 片段；不含前导 AND）。"""
    if p["is_super"] or p["kind"] == 1:
        return "1 = 1"
    dept_ids = p["dept_ids"]
    if dept_ids:
        arr = "ARRAY['" + "','".join(dept_ids) + "']::uuid[]"
        dept_pred = f"{alias}.{dept_col} = ANY({arr})"
        if p["self_only"] and owner_col:
            return f"({dept_pred} OR {alias}.{owner_col} = '{uid}')"
        return dept_pred
    if owner_col:
        return f"{alias}.{owner_col} = '{uid}'"
    if p["own_dept_id"]:
        return f"{alias}.{dept_col} = '{p['own_dept_id']}'"
    return "1 = 0"


# --------------------------------------------------------------------------
# 快照
# --------------------------------------------------------------------------
def table_exists(name):
    rows = psql("select 1 from information_schema.tables "
                f"where table_schema='public' and table_name='{name}'")
    return bool(rows)


def snapshot():
    users = psql("""
        select u.id, u.username, u.user_type, u.status::text, u.is_locked::text,
               coalesce(u.dept_id::text, '')
          from sys_user u
         where u.deleted_at is null
         order by u.username""")
    result = {"generated_at": datetime.now().astimezone().isoformat(timespec="seconds"),
              "db": DB["name"], "host": DB["host"], "users": []}

    present = {key for key, tbl, *_ in TABLES if table_exists(tbl)}
    result["tables_present"] = sorted(present)

    for row in users:
        uid, username, user_type, status, locked, dept_id = (row + [""] * 6)[:6]
        rows = psql(resolve_sql(uid, SUPER_ROLE))
        p: Any = merge_scope_rows([(r[0], (r[1] if len(r) > 1 else "")) for r in rows])
        p["own_dept_id"] = dept_id or p["own_dept_id"]
        visible = {}
        for key, tbl, alias, dept_col, owner_col in TABLES:
            if key not in present:
                visible[key] = None
                continue
            pred = scope_predicate(p, tbl, alias, dept_col, owner_col, uid)
            n = psql(f"select count(*) from {tbl} {alias} "
                     f"where {alias}.deleted_at is null and ({pred})")[0][0]
            visible[key] = int(n)
        result["users"].append(dict(
            username=username, user_type=user_type, status=status, is_locked=locked,
            own_dept_id=dept_id or None,
            kind=KIND_LABEL.get(p["kind"], str(p["kind"])),
            dept_ids=sorted(p["dept_ids"]),
            self_only=p["self_only"],
            visible=visible,
        ))
    return result


def print_table(data):
    print(f"\n库 {data['db']}@{data['host']}  快照 {data['generated_at']}")
    head = f"{'账号':<14}{'类型':<7}{'kind':<14}{'self':<6}{'dept_ids':<12}"
    head += "".join(f"{k.replace('biz_', ''):<20}" for k, *_ in TABLES)
    print(head)
    print("-" * len(head))
    for u in data["users"]:
        line = (f"{u['username']:<14}{u['user_type']:<7}{u['kind']:<14}"
                f"{str(u['self_only']):<6}{len(u['dept_ids']):<12}")
        for k, *_ in TABLES:
            v = u["visible"].get(k)
            line += f"{('n/a' if v is None else str(v)):<20}"
        print(line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", help="把快照写到这个 JSON 文件")
    ap.add_argument("--before", help="落位前的快照 JSON")
    ap.add_argument("--after", help="落位后的快照 JSON")
    ap.add_argument("--apply-wrapper", help="一条命令：跑它之前拍一次、之后拍一次并对照（危险操作交给调用方）")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    if args.apply_wrapper:
        import tempfile
        tmp = tempfile.mkdtemp(prefix="orgland-impact-")
        before_p = os.path.join(tmp, "before.json")
        after_p = os.path.join(tmp, "after.json")
        snap = snapshot()
        json.dump(snap, open(before_p, "w"), ensure_ascii=False, indent=2)
        print(f"== 落位前快照：{before_p}")
        print_table(snap)
        rc = subprocess.run(args.apply_wrapper, shell=True).returncode
        if rc != 0:
            print(f"✗ 落位命令失败（rc={rc}），不拍 after、不做对照", file=sys.stderr)
            return rc
        snap = snapshot()
        json.dump(snap, open(after_p, "w"), ensure_ascii=False, indent=2)
        print(f"\n== 落位后快照：{after_p}")
        print_table(snap)
        return compare(before_p, after_p)

    if args.before and args.after:
        return compare(args.before, args.after)

    snap = snapshot()
    if args.out:
        with open(args.out, "w") as fh:
            json.dump(snap, fh, ensure_ascii=False, indent=2)
        if not args.quiet:
            print(f"== 快照已写入 {args.out}")
    print_table(snap)
    return 0


def compare(before_path, after_path):
    before = json.load(open(before_path))
    after = json.load(open(after_path))
    b = {u["username"]: u for u in before["users"]}
    a = {u["username"]: u for u in after["users"]}
    fails = []
    if set(b) != set(a):
        fails.append(f"账号集合变了：仅在 before {sorted(set(b) - set(a))}，仅在 after {sorted(set(a) - set(b))}")
    print("\n== 逐账号对照（落位前 → 落位后）==")
    head = f"{'账号':<14}{'kind':<26}{'dept_ids':<14}{'self':<16}"
    head += "".join(f"{k.replace('biz_', ''):<22}" for k, *_ in TABLES)
    print(head)
    print("-" * len(head))
    for name in sorted(set(b) & set(a)):
        x, y = b[name], a[name]
        kind_txt = f"{x['kind']} → {y['kind']}" if x["kind"] != y["kind"] else x["kind"]
        if x["kind"] != y["kind"]:
            fails.append(f"{name}: kind {x['kind']} → {y['kind']}")
        if x["dept_ids"] != y["dept_ids"]:
            fails.append(f"{name}: dept_ids {x['dept_ids']} → {y['dept_ids']}")
        if x["self_only"] != y["self_only"]:
            fails.append(f"{name}: self_only {x['self_only']} → {y['self_only']}")
        dept_txt = f"{len(x['dept_ids'])}→{len(y['dept_ids'])}" if x["dept_ids"] != y["dept_ids"] else str(len(x["dept_ids"]))
        self_txt = f"{x['self_only']}→{y['self_only']}" if x["self_only"] != y["self_only"] else str(x["self_only"])
        line = f"{name:<14}{kind_txt:<26}{dept_txt:<14}{self_txt:<16}"
        for k, *_ in TABLES:
            vx, vy = x["visible"].get(k), y["visible"].get(k)
            if vx != vy:
                fails.append(f"{name}: {k} 可见行数 {vx} → {vy}")
                line += f"{str(vx)}→{str(vy):<18}"
            else:
                line += f"{('n/a' if vx is None else str(vx)):<22}"
        print(line)

    if fails:
        print("\n🚨 影响面发生了变化（落位改到了不该改的东西，停手）：")
        for f in fails:
            print(f"  · {f}")
        return 1
    print(f"\n✅ 影响面零变化：{len(set(b) & set(a))} 个在用账号的 "
          f"(kind, dept_ids, self_only) 与 5 张受控表可见行数，落位前后完全一致。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
