#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-6「模板库概念」现状探针（只读，不改任何数据）。

跑法：
    python3 scripts/verify/p2_6_template_probe.py

输出一份人类可读报告 + 每条断言的 PASS/FAIL。用来给决策文档
`docs/architecture/P2-6-模板实体-决策.md` 提供**可复现的证据**，
而不是拿印象说话。

探的是**真实在跑的那个实例**（默认 http://127.0.0.1:8023）和它连的那个库
（从 /proc/<pid>/environ 读 NIK_DATABASE_*，找不到再用 psql 默认 admin 库）。

**P2-6b 之后的契约**（口径已收口，别再用旧断言读它）：
- `GET /business/flow/def/templates` 默认 = **标准流程列表**（`is_system = TRUE`），
  `?scope=all`（legacy `?include_mine=1`）才带客户副本；
- 所以 H4/H5 恒为 0 是对的；证明「过滤真的生效」要看 **H12**
  （接口 total == 库里 `is_system=TRUE` 条数）与「库里确实存在非系统行」时的 H5=0。
"""

import json
import os
import subprocess
import sys
import urllib.request

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:8023")
USER = os.environ.get("TEST_USER", "admin")
PASS = os.environ.get("TEST_PASS", "Admin@123456")

RESULTS = []


def rec(tag, expect, actual, desc):
    ok = True if expect is None else (expect == actual)
    RESULTS.append(ok)
    print(f"{tag:<6} {'OK' if ok else 'FAIL':<8} expect={expect!s:<10} actual={actual!s:<12} {desc}")
    return ok


def http(method, path, token=None, body=None):
    url = BASE_URL + path
    data = json.dumps(body, ensure_ascii=False).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read().decode())


def find_backend_pid():
    """从监听 8023 的进程里找后端 pid（工作目录在 backend/ 的那一个）。"""
    out = subprocess.run(
        ["ss", "-lntp"], capture_output=True, text=True
    ).stdout
    for line in out.splitlines():
        if ":8023" not in line or "pid=" not in line:
            continue
        pid = line.split("pid=")[1].split(",")[0]
        try:
            env = open(f"/proc/{pid}/environ", "rb").read().decode(errors="replace").split("\0")
        except OSError:
            continue
        kv = dict(e.split("=", 1) for e in env if "=" in e)
        if kv.get("NIK_DATABASE_NAME"):
            return pid, kv
    return None, {}


def psql(db, sql, password):
    env = dict(os.environ, PGPASSWORD=password)
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", db, "-tAc", sql],
        capture_output=True, text=True, env=env, timeout=30,
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip())
    return out.stdout.strip()


def main():
    pid, kv = find_backend_pid()
    db = kv.get("NIK_DATABASE_NAME", "cloudpivot_manual")
    pwd = kv.get("NIK_DATABASE_PASSWORD", "")
    print(f"== P2-6 探针 @ {db} （后端 pid={pid} {BASE_URL}）==")

    # ---------------- HTTP 侧：用户真实看到的「模板库」 ----------------
    login = http("POST", "/api/system/auth/login", body={"username": USER, "password": PASS})
    token = (login.get("data") or {}).get("access_token") or ""
    if not token:
        print("登录失败，无法继续 HTTP 断言：", login)
        return 2

    # P2-6b 之后：默认口径（不传参数）**只回标准流程**（`is_system = TRUE`），
    # 所以 H4/H5 恒为 0 才是对的；「全量」要显式带 `?scope=all`（legacy `?include_mine=1` 同义）。
    tpl = http("GET", "/api/business/flow/def/templates", token)
    tpl_data = tpl.get("data") or {}
    groups = tpl_data.get("groups") or []
    items = [it for g in groups for it in g.get("items") or []]
    total = tpl_data.get("total")
    rec("H1", None, total, "标准流程列表「共 N 个」= templates API total")
    rec("H2", None, len(items), "接口实际返回的卡片数")
    sys_items = [it for it in items if it.get("is_system")]
    non_sys = [it for it in items if not it.get("is_system")]
    rec("H3", None, len(sys_items), "其中 is_system=TRUE（挂「标准」标签）")
    rec("H4", 0, len(non_sys), "其中 is_system=FALSE（默认口径下必须 0）")
    rec("H5", 0, len(non_sys), "默认口径里混进了非标准流程的数量（期望 0 = 过滤生效；注意库里若本来没有非系统行，这条没有证明力，须造副本或看 H12）")
    rec("H6", None, len(groups), "按 category 的分组数")

    # `?scope=all`：标准流程 + 客户副本（P2-6b 新增口径）
    allp = http("GET", "/api/business/flow/def/templates?scope=all", token)
    all_data = allp.get("data") or {}
    all_items = [it for g in all_data.get("groups") or [] for it in g.get("items") or []]
    all_total = all_data.get("total")
    rec("H10", None, all_total, "?scope=all 的 total（标准流程 + 客户副本）")
    rec("H11", None, len(all_items), "?scope=all 实际返回条数")
    rec("H13", 1, int((all_total or 0) >= (total or 0)), "scope=all 必须 ≥ 默认口径（1=是）")

    # 流程定义页（同一张表的另一个入口）
    dp = http("GET", "/api/business/flow/def/page?page=1&page_size=100", token)
    dp_data = dp.get("data") or {}
    # 后端 `DefPage` 的字段名是 `list`（不是 `items` / `records`）—— 早期探针按 `items` 取，
    # 拿到 0 行得出「两个页面不是同一批行」的假结论。
    dp_items = dp_data.get("list") or []
    dp_total = dp_data.get("total")
    rec("H7", None, len(dp_items), "流程定义页列表条数（同一张 biz_flow_def，含客户副本）")
    rec("H7t", None, dp_total, "流程定义页 total 字段")
    # P2-6b 前这里断言「两个页面条数相等」；收口后标准流程视图是流程定义页的**子集视图**，
    # 相等不再是契约 —— 改为「templates 条目 ⊂ 流程定义页条目」，且默认口径恰好是全量里的标准流程。
    dp_ids = {it.get("id") for it in dp_items}
    rec(
        "H8",
        1,
        int(all(it.get("id") in dp_ids for it in items)),
        "标准流程列表的每一条都能在流程定义页找到（1=是 → 两个入口仍是同一张表，不是另一张表）",
    )
    rec("H9a", 1, int(total == len(sys_items)), "默认口径 total = is_system=TRUE 条目数（1=是）")

    # 卡片级明细：让证据可逐条核对（谁在「模板库」里、带不带「标准」标签）
    for it in items:
        rec(
            "H0",
            None,
            f"{it.get('flow_code'):<18} is_system={str(it.get('is_system')):<5} "
            f"status={it.get('status'):<10} cat={it.get('category')}",
            "    卡片明细",
        )

    # 名称自称「标准」但 is_system=FALSE 的行（错标）
    mis = [it for it in items if not it.get("is_system") and "标准" in (it.get("flow_name") or "")]
    rec("H9", None, len(mis), "名称自称「标准」但 is_system=FALSE 的行数（口径不一致）")
    for it in mis:
        rec("H9x", None, it.get("flow_code"), f"    错标明细：{it.get('flow_name')}")

    # ---------------- DB 侧：结构事实 ----------------
    rec("D1", 1, int(psql(db, "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_flow_template','sys_flow_template','biz_flow_tpl')", pwd) == "0"),
        "有没有独立模板表（biz_flow_template / sys_flow_template / biz_flow_tpl 任一存在即 0）")
    d2_all = psql(db, "select count(*) from biz_flow_def where deleted_at is null", pwd)
    d3_sys = psql(db, "select count(*) from biz_flow_def where deleted_at is null and is_system", pwd)
    rec("D2", None, d2_all, "biz_flow_def 未软删行数")
    rec("D3", None, d3_sys, "其中 is_system=TRUE")
    # P2-6b 硬断言：接口口径与库内事实逐字对齐（N = 标准流程数这一条就靠它钉住）
    rec("H12", int(d3_sys), total, "默认口径 total 必须 = 库里 is_system=TRUE 条数（N = 标准流程数）")
    rec("H14", int(d2_all), all_total, "scope=all 的 total 必须 = 库里未软删定义数（标准流程 + 客户副本）")
    rec("D4", None, psql(db, "select count(*) from biz_flow_def_version", pwd), "流程版本行数")
    rec("D5", None, psql(db, "select count(*) from biz_flow_instance where deleted_at is null", pwd), "审批实例数（def 被复用发起）")
    rec("D6", None, psql(db, "select count(*) from sys_menu where path in ('/flow/def','/flow/templates') and deleted_at is null", pwd),
        "sys_menu 里指向流程的两个页面节点数（2 = 两个入口一张表）")
    rec("D7", None, psql(db, "select count(*) from sys_menu where path='/flow/templates' and deleted_at is null and parent_id is null", pwd),
        "其中 /flow/templates 是顶层节点（无父级）")
    # is_system 初始值 / 是否可空
    rec("D8", None, psql(db, """select column_default||' / nullable='||is_nullable from information_schema.columns
                                 where table_name='biz_flow_def' and column_name='is_system'""", pwd),
        "biz_flow_def.is_system 默认值 / 可空性")
    # 有没有「模板专属」列（版本、骨架标记等）
    rec("D9", None, psql(db, """select coalesce(string_agg(column_name, ',' order by ordinal_position),'(none)')
                                 from information_schema.columns where table_name='biz_flow_def'
                                   and column_name ~ 'template|tpl|skeleton'""", pwd),
        "biz_flow_def 里带 template/tpl/skeleton 字样的列（为空 = 模板只是 flag）")

    # DB 侧复核「名称自称标准 / is_system=FALSE」的错标（前端标签只是 flag 的影子，这里查源头）
    rec("D10", None, psql(db, """select coalesce(string_agg(flow_code, ' | ' order by flow_code),'(none)')
                                  from biz_flow_def
                                 where deleted_at is null and not is_system and flow_name like '%标准%'""", pwd),
        "库里「名称含标准但 is_system=FALSE」的行（应为 0）")

    # 对照物：任务模块的「任务模板」是**真表**（biz_task_template）。
    # 这条是决策文档里「同一个产品里『模板』有两种含义」的硬证据。
    rec("D11", 1, int(psql(db, """select count(*) from information_schema.tables
                                   where table_schema='public' and table_name='biz_task_template'""", pwd) == "1"),
        "存在 biz_task_template（任务模板 = 独立实体，与流程侧形成反差）")

    # 迁移号段：决策文档里给实施卡提的迁移编号必须 > 现状最大值
    # （`schema_migrations.version` 存的是纯数字串 '219'，不是 'V0219'）
    rec("D12", None, psql(db, """select max(cast(version as int)) from schema_migrations
                                  where kind='versioned' and success""", pwd),
        "库里已应用的最大迁移号（新增迁移必须更大）")

    fails = RESULTS.count(False)
    print(f"-- 断言 {len(RESULTS)} 条，FAIL={fails}")
    print(f"-- 数据库：{db}")
    print("-- 结论：FAIL=0 表示现状与决策文档描述一致；>0 先看是哪条变了")
    return 0 if fails == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
