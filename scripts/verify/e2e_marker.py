#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
E2E 夹具标记（marker）—— **唯一判据源**
==========================================================================
本文件回答一个问题：**「这一行是不是 e2e 造的测试数据？」**

为什么需要「唯一判据源」
------------------------
2026-10-30/31 生产库（144 / `cloudpivot`）被前端 e2e 套件写进了成批残留：

  · 9 个活跃任务 `[E2E]详情-e2emvvvdf01*`（另有 346 个已软删的同类）
  · 105 条通知（「待你审批：[E2E]审批单-…」）+ 1 条已发布公告（`E2E公告测试`）
  · 221 条任务评论、43 个上传文件、2 个导出任务、1 个帮助分类
  · 6 个仍在启用的测试账号（`laoliu_test` / `qa.limited` / `qa.viewer` / `qa.mutex` /
    `cl_43870` / `verify-ai`）

成因不是某条用例写错，而是**「e2e 数据」这件事从来没有一个可机读的标记**：
标题里有没有 `[E2E]` 全靠每个 spec 手写，`support.ts::createTask` 不做任何约束；
于是一旦 `E2E_BASE_URL` 指到生产（`target-guard.mjs` 出事前的默认值就是生产），
造出来的行与真实业务行长**完全一样**，事后只能靠人眼和前缀猜测。

所以本模块把「标记」定成代码：

  ① 每个可被 e2e 写入的表，都用**同一个前缀/谓词**标记（见 [`TABLE_SPECS`]）；
  ② 前端侧同一个前缀由 `frontend/e2e/e2e-marker.ts` 生成，并**强制**套在
     `createTask()` 上（漏了也跑不掉，见该文件头注释）；
  ③ 残留检测只有这一份 SQL（[`RESIDUE_SQL`]），三处复用：
     · `erp_invariants.py` 的 O1 不变量（生产库出现标记数据 → WARN）
     · `scripts/datafix/e2e_residue3/30_verify.sql`（清理后必须为 0，
       它调用本文件的 `--assert-clean` 做交叉断言 —— 谁把谓词擅自放宽，这里立刻红）
     · `frontend/e2e/target-guard.mjs` 的 prod 后置清单（放行打生产后强制复查）

判据为什么要「按表分写」而不是一个 `%E2E%` 大扫
------------------------------------------------
一个全局 `LIKE '%E2E%'` 会误伤真实数据，实测反例（2026-11-05 生产库）：
`biz_task_comment` 里有 4 条 `laoliu` 的真实开发汇报、2 条 `verify-ai` 的真实阻塞通知
正文里都提到了 "e2e"；它们挂在 `T8-A`/`P4-A` 这种**真实任务**上。
所以任务评论的判据是「挂在标记任务下」，不是「正文含 E2E」。

用法
----
    set -a; . /etc/cloudpivot/cloudpivot.env; set +a

    python3 scripts/verify/e2e_marker.py               # 人看（有残留 → 退出码 1）
    python3 scripts/verify/e2e_marker.py --json        # 机器看
    python3 scripts/verify/e2e_marker.py --assert-clean  # 静默模式：干净才 0，否则打印清单退出 1
    python3 scripts/verify/e2e_marker.py --self-test   # 判据自测（不连库，钉住误伤反例）
    NIK_DATABASE_NAME=cloudpivot_drill python3 ...      # 换库

退出码：0 = 干净；1 = 有残留；2 = 连库失败（**不要**当成干净）；3 = 参数错。
本脚本**只读**（不装 db_guard：它是审计，必须能在生产上跑 —— 与
`e2e_fixture_residue.py` 同口径）。
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)

# ──────────────────────────────────────────────────────────────────────────
# 标记本身
# ──────────────────────────────────────────────────────────────────────────
TASK_TITLE_PREFIX = "[E2E]"
# 历史遗留的错拼变体：09-t8c 的骨架在早期版本里写成了 `[E9E2E]`（9 与 E 混键），
# 生产库里还有 49 行这种标题。新代码一律只用 TASK_TITLE_PREFIX，
# 但判据必须认它 —— 否则「按标记清理」会把这一批漏掉（假绿）。
LEGACY_TITLE_PREFIXES = ("[E9E2E]",)
WAREHOUSE_CODE_PREFIX = "E2E-"
FILE_NAME_PREFIXES = ("e2e", "e9-")

# 已知测试账号：username → 来历（判据用，**不是**删除名单）。
# 判据 = 这些名字仍是「在用」状态（deleted_at is null and status = 1）。
# 加一行之前先回答：这个号是不是只用来验收？是不是已经有实名账号能干同样的事？
TEST_ACCOUNTS = {
    "laoliu_test": "e2e 套件默认登录号（frontend/e2e/support.ts 的 ACCOUNT 默认值）",
    "qa.viewer": "QA 只读验收号（P0-FIX-2：is_locked=true，角色 QA_VIEWER）",
    "qa.limited": "QA 受限管理员验收号（角色 QA_LIMITED）",
    "qa.mutex": "QA 双角色互斥验收号（角色 MUTEX_A + MUTEX_B）",
    "cl_43870": "操作日志模块验收号（同批 cl2_43911 已软删）",
    "verify-ai": "AI 派活验证机器人（ROBOT，2026-10-29 一次性验收）",
}

_TITLE_LIKE = " or ".join(
    "title like '" + p.replace("'", "''") + "%'"
    for p in (TASK_TITLE_PREFIX, *LEGACY_TITLE_PREFIXES)
)

# ──────────────────────────────────────────────────────────────────────────
# 逐表判据
# ──────────────────────────────────────────────────────────────────────────
# 每个 dict：
#   table  表名
#   label  用来给人看的列（`id` / 名称 / 标题）
#   where  「算残留」的谓词（**只写取值**，`deleted_at is null` 由外层统一加）
#   why    为什么这条谓词不会误伤真实数据
#   cascade 可选：该表是「挂在别的标记行下」才算出问题（值为父表名）
TABLE_SPECS: list[dict] = [
    {
        "table": "biz_task",
        "label": "title",
        "where": f"({_TITLE_LIKE})",
        "why": "任务标题前缀是 e2e 唯一的造数口径（support.ts + 各 spec 手写），"
               "真实任务不会以它开头；含历史错拼变体 [E9E2E]",
    },
    {
        "table": "biz_task_comment",
        "label": "content",
        "where": "task_id in (select id from biz_task where "
                 + _TITLE_LIKE.replace("title like", "title like") + ")",
        "why": "判据是**父任务带标记**，不是正文含 E2E：生产库里有 4 条 laoliu 的"
               "真实开发汇报、2 条 verify-ai 的真实阻塞通知正文提到 e2e，"
               "它们挂在 T8-A/P4-A 这类真实任务上，正文匹配会误删",
        "cascade": "biz_task",
    },
    {
        "table": "biz_flow_instance",
        "label": "title",
        "where": "(title ilike '%E2E%')",
        "why": "流程实例标题来自创建它的任务/单据标题（`TSK-… [E2E]探针-…`），"
               "真实业务流程不会在标题里写 E2E",
    },
    {
        "table": "sys_notice",
        "label": "title",
        "where": "(title ilike '%E2E%')",
        "why": "通知标题是模板直接拼 biz 标题（`待你审批：[E2E]审批单-…`）；"
               "只看标题不看正文，避免把「正文里提到 e2e」的真实通知算进来",
    },
    {
        "table": "sys_announcement",
        "label": "title",
        "why": "公告标题由人工填写，`E2E公告测试 085522` 这种只在验收里出现",
        "where": "(title ilike '%E2E%')",
    },
    {
        "table": "sys_file",
        "label": "original_name",
        "where": "(" + " or ".join(f"original_name ilike '{p}%'" for p in FILE_NAME_PREFIXES) + ")",
        "why": "e2e 上传件一律叫 `e2e*.txt` / `e9-*.txt`（全局生命周期用例）；"
               "真实上传件是业务单据名",
    },
    {
        "table": "sys_export_task",
        "label": "export_name",
        "where": "(export_name ilike 'e2e%' or file_name ilike 'e2e%')",
        "why": "导入导出用例的导出名固定写 `e2e_export`",
    },
    {
        "table": "sys_help_category",
        "label": "code",
        "where": "(code ilike 'e2e%' or name ilike '%E2E%')",
        "why": "帮助中心分类是人工维护的，`e2e_cat_<stamp>` 只由验收脚本建",
    },
    {
        "table": "biz_warehouse",
        "label": "warehouse_code",
        "where": f"(warehouse_code like '{WAREHOUSE_CODE_PREFIX}%' or warehouse_name ilike '%E2E%')",
        "why": "仓/库区编码由 10-erp-warehouse 用例按 `E2E-W4A-<stamp>` 生成",
    },
]

# 单条 UNION ALL：一次 round-trip 拿到 (表, 活跃残留数, 样例)
RESIDUE_SQL = " union all ".join(
    f"select '{s['table']}' as tbl, "
    f"count(*) filter (where deleted_at is null) as alive, "
    f"coalesce(string_agg(nullif({s['label']}::text, ''), ' | ' order by {s['label']}) "
    f"         filter (where deleted_at is null), '') as sample "
    f"from {s['table']} where {s['where']}"
    for s in TABLE_SPECS
)

# 测试账号判据（同样是「唯一判据源」，datafix 的 30_verify 会交叉断言）
# 一行一个账号，**不做聚合**：聚合式 `string_agg` 在「零命中」时会返回 NULL，
# 各种 split 会把它变成一个幽灵账号（2026-11-05 演练实测：清干净了却报「仍有 1 个账号」）。
TEST_ACCOUNT_LIST_SQL = ",".join("'" + u.replace("'", "''") + "'" for u in TEST_ACCOUNTS)
TEST_ACCOUNTS_SQL = f"""
select username
  from sys_user
 where deleted_at is null and status = 1
   and username in ({TEST_ACCOUNT_LIST_SQL})
 order by username
"""


def psql_rows(sql: str) -> list[list[str]]:
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    cmd = ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
           "-d", DB["name"], "-tAF", "\x1f", "-v", "ON_ERROR_STOP=1", "-c", sql]
    run = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=60)
    if run.returncode != 0:
        raise RuntimeError(run.stderr.strip()[:400] or "psql 执行失败")
    return [line.split("\x1f") for line in run.stdout.strip().split("\n") if line]


def residue() -> list[dict]:
    """[{table, alive, sample, why}]，只包含 alive > 0 的表。"""
    rows = psql_rows(RESIDUE_SQL)
    out = []
    by_table = {s["table"]: s for s in TABLE_SPECS}
    for r in rows:
        tbl, alive = r[0], int(r[1] or 0)
        if alive:
            out.append(dict(table=tbl, alive=alive,
                            sample=(r[2] or "").split(" | ")[:6],
                            why=by_table[tbl]["why"]))
    out.sort(key=lambda x: -x["alive"])
    return out


def enabled_test_accounts() -> list[str]:
    return [r[0] for r in psql_rows(TEST_ACCOUNTS_SQL) if r and r[0]]


def total_alive() -> int:
    return sum(h["alive"] for h in residue())


# ──────────────────────────────────────────────────────────────────────────
# 判据自测（不连库）：钉住「误伤反例」与「必须命中反例」，改谓词时先跑它
# ──────────────────────────────────────────────────────────────────────────
MUST_MATCH = [
    ("biz_task", "[E2E]详情-e2emvvvdf01"),
    ("biz_task", "[E9E2E]详情重构-e2emvufu76b"),
    ("biz_warehouse", "E2E-W4A-MVUUPN1M-Q"),
    ("sys_file", "e2e-e2emvugheec.txt"),
    ("sys_export_task", "e2e_export"),
    ("sys_notice", "待你审批：[E2E]审批单-e2emvumjhst"),
]
MUST_NOT_MATCH = [
    ("biz_task_comment", "8 层深度已完成并验证。commit 0f4cad3 … e2e 用例"),
    ("sys_notice", "AI 需要人工协助"),
    ("biz_task", "T8-A 任务父子深度 2 → 8 层"),
]


def _match(table: str, text: str) -> bool:
    """把 MUST_MATCH / MUST_NOT_MATCH 的样例按谓词口径判一遍（纯字符串，不连库）。"""
    spec = next(s for s in TABLE_SPECS if s["table"] == table)
    label = spec["label"]
    if label == "title":
        return text.startswith(TASK_TITLE_PREFIX) or any(
            text.startswith(p) for p in LEGACY_TITLE_PREFIXES) or "E2E" in text.upper()
    if label == "original_name":
        return any(text.lower().startswith(p) for p in FILE_NAME_PREFIXES)
    if label == "export_name":
        return text.lower().startswith("e2e")
    if label == "warehouse_code":
        return text.startswith(WAREHOUSE_CODE_PREFIX) or "E2E" in text.upper()
    if table == "biz_task_comment":
        # 评论的判据是父任务，样例这里只看「正文含 e2e 不算命中」这一条
        return text.startswith(TASK_TITLE_PREFIX)
    return "E2E" in text.upper()


def self_test() -> int:
    checks = 0
    failed = 0
    for table, text in MUST_MATCH:
        ok = _match(table, text)
        checks += 1
        failed += 0 if ok else 1
        print(f"[{'ok  ' if ok else 'FAIL'}] 应命中   {table:<18} {text[:52]}")
    for table, text in MUST_NOT_MATCH:
        ok = not _match(table, text)
        checks += 1
        failed += 0 if ok else 1
        print(f"[{'ok  ' if ok else 'FAIL'}] 不应命中 {table:<18} {text[:52]}")

    # 标记常量与前端 TS 侧必须一致（前端文件不存在时跳过：本脚本可单独拷走使用）
    ts = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "..", "..", "frontend", "e2e", "e2e-marker.ts")
    try:
        with open(ts, encoding="utf-8") as fh:
            body = fh.read()
        m = re.search(r"E2E_MARK\s*=\s*'([^']+)'", body)
        ok = bool(m) and m.group(1) == TASK_TITLE_PREFIX
        checks += 1
        failed += 0 if ok else 1
        print(f"[{'ok  ' if ok else 'FAIL'}] 前端 TS 标记一致  E2E_MARK = {m.group(1) if m else '<未找到>'}"
              f"（Python 侧 {TASK_TITLE_PREFIX}）")
    except OSError:
        print("[skip] 前端 e2e-marker.ts 不在本目录树，跳过跨语言一致性检查")

    # ---- 护栏是**接线**出来的，不是写在文档里的：两个强制点必须还在代码里 ----
    # 2026-11-05 实测教训：`e2e-marker.ts` 单独存在时一行数据都不会被标记，
    # 只有 support.ts 真的调 markTitle、target-guard 真的跑 --assert-clean 才叫「守卫」。
    for path, needle, what in (
        (os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                      "frontend", "e2e", "support.ts"),
         "markTitle(", "support.ts::createTask/createInstance 过 markTitle()"),
        (os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..",
                      "frontend", "e2e", "target-guard.mjs"),
         "e2e_marker.py --assert-clean", "target-guard.mjs 的 prod 后置清单含 --assert-clean"),
    ):
        try:
            with open(path, encoding="utf-8") as fh:
                wired = needle in fh.read()
        except OSError:
            print(f"[skip] {os.path.basename(path)} 不在本目录树，跳过接线检查")
            continue
        checks += 1
        failed += 0 if wired else 1
        print(f"[{'ok  ' if wired else 'FAIL'}] 接线在          {what}")

    print(f"---- self-test：{checks - failed}/{checks} 项通过 ----")
    return 0 if failed == 0 else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="e2e 夹具标记残留检查（只读）")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--assert-clean", action="store_true",
                    help="静默：干净 → 0；有残留 → 打印清单并退出 1")
    ap.add_argument("--self-test", action="store_true", help="判据自测（不连库）")
    ap.add_argument("--list-specs", action="store_true", help="打印逐表判据（Markdown 表格）")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    if args.list_specs:
        print("| 表 | 标记列 | 判据（alive） | 为什么不会误伤 |")
        print("|---|---|---|---|")
        for s in TABLE_SPECS:
            print(f"| `{s['table']}` | `{s['label']}` | `{s['where']}` | {s['why']} |")
        return 0

    try:
        hits = residue()
        accounts = enabled_test_accounts()
    except (RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f"✗ 连库/查询失败（{DB['name']}@{DB['host']}:{DB['port']}）：{exc}", file=sys.stderr)
        print("  提示：先 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a`", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps(dict(db=DB["name"], ok=not hits and not accounts,
                              residue=hits, enabled_test_accounts=accounts),
                         ensure_ascii=False, indent=2))
        return 1 if (hits or accounts) else 0

    if args.assert_clean:
        if not hits and not accounts:
            return 0
        for h in hits:
            print(f"❌ {h['table']}: {h['alive']} 行（样例：{' | '.join(h['sample'])}）")
        if accounts:
            print(f"❌ 仍启用的测试账号：{', '.join(accounts)}")
        return 1

    print("=" * 86)
    print("e2e 夹具标记残留检查")
    print(f"库: {DB['name']}@{DB['host']}:{DB['port']}    标记: {TASK_TITLE_PREFIX}…")
    print("=" * 86)
    if not hits and not accounts:
        print("  ✅ 干净：没有任何「活跃的 e2e 标记行」，也没有仍在启用的测试账号")
        return 0
    for h in hits:
        print(f"  ❌ {h['table']:<20} {h['alive']:>4} 行   样例：{' | '.join(h['sample'])[:90]}")
        print(f"       判据：{h['why'][:110]}")
    if accounts:
        print(f"  ❌ 仍启用的测试账号（{len(accounts)}）：{', '.join(accounts)}")
        for name in accounts:
            print(f"       {name:<14} {TEST_ACCOUNTS.get(name, '（未登记来历）')[:80]}")
    print("-" * 86)
    print(f"合计：{sum(h['alive'] for h in hits)} 行标记数据 + {len(accounts)} 个在用测试账号")
    print("处置：bash scripts/datafix/e2e_residue3/apply.sh（先 00_backup.sh）")
    return 1


if __name__ == "__main__":
    sys.exit(main())
