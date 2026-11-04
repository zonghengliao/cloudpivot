#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
云枢 · 快照一致性巡检（B1）—— **只报不修**
==========================================

把 `docs/architecture/快照一致性巡检与告警设计.md` §2.4 的巡检项落成可反复执行的只读巡检。
一条巡检 = 设计文档里的一个只读视图 `di.c_diNN`（定义见
`docs/architecture/snapshot-drift/10_checks.sql`，本脚本**不复制、不改写**那些 SQL，
运行时按路径读取原文件），输出统一 11 列后由本脚本渲染成人看/机看两种格式。

用法
----
    python3 scripts/verify/snapshot_audit.py                 # 全量跑，人看格式
    python3 scripts/verify/snapshot_audit.py --json           # 纯 JSON 到 stdout（人看格式转 stderr）
    python3 scripts/verify/snapshot_audit.py --group 资金链    # 只跑一组（也可写 DI-06 只跑一条）
    python3 scripts/verify/snapshot_audit.py --dry-run        # 不连库：只列出将执行的巡检与 SQL
    python3 scripts/verify/snapshot_audit.py --install-views  # 先幂等安装 di 兼容层视图，再巡检

    # 定时档（cron 包装脚本 scripts/cron/snapshot_audit.sh 用的就是这条）
    python3 scripts/verify/snapshot_audit.py --json --out /var/log/cloudpivot/snapshot-audit.latest.json \
            --log-file /var/log/cloudpivot/snapshot-audit.log

连接口径（全部可用环境变量覆盖，与 `erp_invariants.py` 同源）
------------------------------------------------------------
| 变量 | 默认值 | 说明 |
|---|---|---|
| `NIK_DATABASE_HOST` | `127.0.0.1` | 库地址 |
| `NIK_DATABASE_PORT` | `5432` | 端口 |
| `NIK_DATABASE_USER` | `cloudpivot` | 账号 |
| `NIK_DATABASE_NAME` | `cloudpivot` | 库名 |
| `NIK_DATABASE_PASSWORD` | 空 | 口令（走 `PGPASSWORD`，不落盘） |
| `SNAPSHOT_AUDIT_SQL_DIR` | `<repo>/docs/architecture/snapshot-drift` | 巡检 SQL 目录（找不到就报错，不会静默用内置副本） |
| `SNAPSHOT_AUDIT_LOG` | `/var/log/cloudpivot/snapshot-audit.log` | 日志文件（--log-file 优先） |
| `SNAPSHOT_AUDIT_NOTIFY_CMD` | 空 | 有 BLOCKER 时执行的通知钩子（--notify-cmd 优先） |

分级与退出码
------------
* 视图 `severity` 三值直接映射到本脚本的三级：`ERROR → BLOCKER` / `WARN → WARN` / `INFO → INFO`。
  设计文档 §2.3 的口径：BLOCKER = 钱已动过 / 追溯链已断 / 主体被无审批改过 / 护栏丢失。
* `0` 无 BLOCKER；`1` 有 BLOCKER；`2` 环境或用法错误（连不上库、`di` 视图缺失、`--group` 不存在）；
  `3` 上一轮巡检还在跑（flock 未拿到锁）。

**只读纪律（三条，缺一不可）**
1. 每条巡检都在 `BEGIN READ ONLY` 事务 + `statement_timeout` 里跑：任何写操作都会被 PG 直接拒绝。
2. 本脚本全文没有一行 DML —— 只 `SELECT`。唯一会执行的 DDL 是 `--install-views`
   （`CREATE OR REPLACE VIEW`，只落在 `di` 这个纯巡检 schema，幂等、回滚 `DROP SCHEMA di CASCADE`）。
3. **发现漂移绝不回写业务数据**（设计文档 AP-8）：脚本只产出日志、JSON 结果与退出码，
   修复必须是人工动作 + 审计（设计文档 §6）。

⚠️ 0 行 ≠ 无缺陷：合同 schema（`biz_contract` + 采购单 `contract_id/purchase_type`）没就位时，
依赖它的巡检记 `SKIPPED` 而不是 `通过`（设计文档 §3.5）。
"""
import argparse
import fcntl
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone, timedelta

# ══════════════════════════════════════════════════════════════════
# 0. 常量与连接口径
# ══════════════════════════════════════════════════════════════════

TZ = timezone(timedelta(hours=8))  # 北京时间：日志与告警正文都用它，不用宿主本地时区
HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(HERE))
SQL_DIR = os.environ.get("SNAPSHOT_AUDIT_SQL_DIR",
                         os.path.join(REPO_ROOT, "docs", "architecture", "snapshot-drift"))
INSTALL_FILES = ("00_install_compat.sql", "10_checks.sql")
DEFAULT_LOG = os.environ.get("SNAPSHOT_AUDIT_LOG", "/var/log/cloudpivot/snapshot-audit.log")
LOCK_FILE = os.environ.get("SNAPSHOT_AUDIT_LOCK", "/var/lock/cloudpivot-snapshot-audit.lock")
STATEMENT_TIMEOUT = os.environ.get("SNAPSHOT_AUDIT_TIMEOUT", "30s")
DETAIL_ROWS = 5          # 卡面要求：每条给出前 5 条明细
SEP = "\x1f"             # psql 字段分隔符（不可见，避免与业务文本里的逗号/竖线打架）

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)

# severity → 本脚本的三级（唯一映射处，别在别处再写一遍）
LEVEL_OF = {"ERROR": "BLOCKER", "WARN": "WARN", "INFO": "INFO"}
LEVEL_ORDER = {"BLOCKER": 0, "WARN": 1, "INFO": 2}
MARK = {"BLOCKER": "✗", "WARN": "⚠", "INFO": "ℹ"}

# ══════════════════════════════════════════════════════════════════
# 1. 巡检项清单（编号 / 名称 / 组 / 最高可能级别 / 依赖）
#    SQL 一律指到设计文档的视图，不在本文件复制一份（口径只有一处真相）
# ══════════════════════════════════════════════════════════════════

CHECKS = [
    dict(id="DI-01", group="合同链", view="di.c_di01", level="BLOCKER", contract=True,
         name="采购单供应商快照 ≠ 合同乙方（三态）",
         meaning="无变更台账 ⇒ 疑似篡改(P0)；有台账且建单晚于变更 ⇒ 快照未跟随(P1)；建单更早 ⇒ 合法历史快照(INFO)"),
    dict(id="DI-02", group="合同链", view="di.c_di02", level="BLOCKER", contract=True,
         name="采购单引用的合同已软删",
         meaning="已审批/已过账 ⇒ 付了钱却没有合同依据(P0)；草稿态 ⇒ P1"),
    dict(id="DI-03", group="合同链", view="di.c_di03", level="BLOCKER", contract=True,
         name="采购单 contract_id 悬空",
         meaning="指向的行根本不存在：追溯链到此断掉，一律 P0"),
    dict(id="DI-04", group="合同链", view="di.c_di04", level="WARN", contract=True,
         name="CONTRACT 采购单缺 contract_id",
         meaning="合同采购没有合同 = 乙方没有真相来源；约束 ck_biz_po_contract_pair 被绕过时现形"),
    dict(id="DI-05", group="合同链", view="di.c_di05", level="WARN", contract=True,
         name="SPOT 采购单缺 supplier_id",
         meaning="零星采购没有供应商 = 应付/凭证无法生成；现状被 NOT NULL 拦住，恒 0 行"),
    dict(id="DI-13", group="合同链", view="di.c_di13", level="WARN", contract=True,
         name="同一合同下快照互斥（多个不同供应商）",
         meaning="一份合同的乙方只有一个真相，同代内互斥不可能由「合同换乙方」解释"),
    dict(id="DI-15", group="合同链", view="di.c_di15", level="BLOCKER", contract=True,
         name="合同已终止/已完成但仍有在途采购单",
         meaning="单据的乙方真相来自一份已不允许履约的合同（I-P5）；在途态 ⇒ P0"),

    dict(id="DI-06", group="实物链", view="di.c_di06", level="BLOCKER", contract=False,
         name="入库单供应商 ≠ 来源采购单供应商",
         meaning="入库单这层是冗余拷贝（可无歧义重算），任何偏移都是缺陷；已过账/已生成应付 ⇒ P0"),
    dict(id="DI-07", group="实物链", view="di.c_di07", level="BLOCKER", contract=False,
         name="入库单的来源采购单已软删",
         meaning="货收在没有采购依据的单子上；已过账 ⇒ P0"),
    dict(id="DI-08", group="实物链", view="di.c_di08", level="BLOCKER", contract=False,
         name="入库单来源采购单悬空",
         meaning="source_order_id 指向不存在的行：追溯链断裂(P0)"),
    dict(id="DI-09", group="实物链", view="di.c_di09", level="WARN", contract=False,
         name="采购入库（PURCHASE）缺 supplier_id",
         meaning="应付与凭证无法生成；约束 ck_biz_si_supplier 缺失时兜底"),
    dict(id="DI-16", group="实物链", view="di.c_di16", level="BLOCKER", contract=False,
         name="已过账收货单的来源采购单已不可收",
         meaning="库存/应付/进项凭证已生成，但采购单不在可收状态（含软删/悬空）⇒ P0（DC-03 存量）"),

    dict(id="DI-10", group="资金链", view="di.c_di10", level="BLOCKER", contract=False,
         name="应付单供应商 ≠ 来源入库单供应商",
         meaning="钱付给了和实物入库不同的人（涉资典型形态），一律 P0"),
    dict(id="DI-11", group="资金链", view="di.c_di11", level="BLOCKER", contract=False,
         name="应付来源入库单悬空/已软删",
         meaning="应付追不回入库单 = 无法对账、无法三单匹配(P0)"),

    dict(id="DI-12", group="主档", view="di.c_di12", level="BLOCKER", contract=False,
         name="采购单供应商主档悬空/已软删",
         meaning="悬空 ⇒ 追溯链断掉(P0)；软删 ⇒ 「当时是谁」只能靠快照名兜底(P1)"),
    dict(id="DI-23", group="主档", view="di.c_di23", level="WARN", contract=False,
         name="供应商 ID→名字映射唯一性（同名/同简称/简称撞他人全名/晚写旧名/快照名≠现名）",
         meaning="关联只认 id，名字只用于显示：两个活跃供应商同名或同简称(P1)会让下拉框与按名字汇总"
                 "把两家并成一家，简称撞别人全名(P1)会让人分不清全名/简称；SPOT 采购单建单晚于主档"
                 "改名却写旧名(P1)=写路径漏取现值（可重算的偏移）。快照名≠现名(P2，合法历史)与"
                 "存量快照 NULL(P2，V0198 未回填、显示回落现值)只留痕不告警——快照漂移只报不修"),

    dict(id="DI-14", group="护栏", view="di.c_di14", level="BLOCKER", contract=False,
         name="结构护栏自检（约束/部分唯一索引缺失）",
         meaning="CHECK 约束或去重索引被 drop = 缺陷先落库再被发现、且修复后再发生写不进来（漏报）"),

    # ── 多态引用（ARCH-3 §5 DC-08 / 卡 P2-6）────────────────────────────────
    # 「类型列 + id 列」的引用无法用外键表达，单据软删后引用方仍挂着，没人发现。
    # 分级口径：账务证据链断 = BLOCKER；审批/追溯体验坏 = WARN；只增不改的历史 = INFO。
    dict(id="DI-17", group="多态引用", view="di.c_di17", level="BLOCKER", contract=False,
         name="凭证来源悬空（source_type 指出的单据已软删/不存在，或类型有而 source_id 空）",
         meaning="凭证是审计证据链的终点：悬空 ⇒ 按来源筛凭证出现点不进去的幽灵行(P0)；缺 id ⇒ 只剩 source_no 可人工追(P1)"),
    dict(id="DI-18", group="多态引用", view="di.c_di18", level="BLOCKER", contract=False,
         name="库存流水单据悬空（doc_id 指向的单据已软删/不存在）",
         meaning="流水是「账动了多少」的唯一证据，必须能回跳到造成它的单据；无源 ⇒ 账实不符时说不出谁动的(P0)。"
                 "doc_type 本来没有实体表（ADJUST/OPENING）却填了 doc_id ⇒ 手工修数痕迹，INFO"),
    dict(id="DI-19", group="多态引用", view="di.c_di19", level="WARN", contract=False,
         name="流程实例业务单据悬空/缺 biz_id",
         meaning="审批挂在不存在的单据上：账不受影响，但审批中心点进去 404、TASK_OWNER 等变量解析不到(P1)"),
    dict(id="DI-20", group="多态引用", view="di.c_di20", level="INFO", contract=False,
         name="状态日志实体悬空（entity_type 指向的实体已软删/不存在）",
         meaning="日志是只增不改的历史台账：实体被删是正常生命周期，日志必须保留「谁在什么时候改了什么」⇒ 只记录不告警"),
    dict(id="DI-21", group="多态引用", view="di.c_di21", level="WARN", contract=False,
         name="序列号来源未记录（source_type 有值、source_id 恒空；且来源类型无目标表）",
         meaning="序列号的影响在「一物一码」的件数（C5 已守），来源单据缺失只影响「这件货哪张单进来的」追责 ⇒ INFO；"
                 "无表映射却填了 id ⇒ WARN（映射未定义，需人工确认指向）"),
    dict(id="DI-22", group="多态引用", view="di.c_di22", level="INFO", contract=False,
         name="多态引用登记缺口（数据里出现 di.v_poly_ref_map 未登记的类型）",
         meaning="本组巡检的可信度护栏：未登记的类型 ⇒ 它的悬空不会被 DI-17…DI-21 查到（静默漏报），"
                 "典型命中是客户端自建流程 biz_type / E2E 夹具（按设计不参与自检）"),
]

GROUPS = []
for _c in CHECKS:
    if _c["group"] not in GROUPS:
        GROUPS.append(_c["group"])

# ══════════════════════════════════════════════════════════════════
# 2. psql 执行层（唯一出口；只读事务在这里强制）
# ══════════════════════════════════════════════════════════════════


def _psql_base():
    return ["psql", "-X", "-q", "-h", DB["host"], "-p", str(DB["port"]),
            "-U", DB["user"], "-d", DB["name"], "-v", "ON_ERROR_STOP=1",
            "--set", "client_encoding=UTF8"]


def _env(readonly=True):
    env = dict(os.environ)
    # 口令只走环境变量（不落盘、不进 argv）。空值表示「不设」，交给 ~/.pgpass / peer 兜底
    pwd = os.environ.get("NIK_DATABASE_PASSWORD", "")
    if pwd:
        env["PGPASSWORD"] = pwd
    else:
        env.pop("PGPASSWORD", None)
    # 双保险：即使有人把 BEGIN READ ONLY 删了，会话级默认也只读。
    # 只有 --install-views 的 DDL 会话把默认关掉（并显式说明，见 run_sql_file）。
    env["PGOPTIONS"] = ("-c default_transaction_read_only=on" if readonly
                        else "-c default_transaction_read_only=off")
    return env


def run_sql(sql, readonly=True, timeout=180):
    """执行一段 SQL，返回行列表（每行是字段列表）。readonly=True 时整体包在只读事务里。"""
    text = sql
    if readonly:
        text = ("BEGIN READ ONLY;\n"
                f"SET LOCAL statement_timeout = '{STATEMENT_TIMEOUT}';\n"
                f"SET LOCAL idle_in_transaction_session_timeout = '60s';\n"
                f"{sql}\n"
                "ROLLBACK;")
    proc = subprocess.run(_psql_base() + ["-tA", "-F", SEP, "-c", text],
                          capture_output=True, text=True, env=_env(readonly), timeout=timeout)
    if proc.returncode != 0:
        raise RuntimeError((proc.stderr or proc.stdout).strip()[:600])
    rows = []
    for line in proc.stdout.splitlines():
        if line:
            rows.append(line.split(SEP))
    return rows


def run_sql_file(path, timeout=180):
    """执行一个 .sql 文件（仅 --install-views 用；这些文件只建视图，无 DML）。

    这是本脚本唯一会做 DDL 的入口：`CREATE SCHEMA di` + `CREATE OR REPLACE VIEW di.*`
    （幂等，回滚 `DROP SCHEMA di CASCADE`）。业务表一行不碰。
    """
    proc = subprocess.run(_psql_base() + ["-f", path],
                          capture_output=True, text=True, env=_env(readonly=False), timeout=timeout)
    if proc.returncode != 0:
        raise RuntimeError((proc.stderr or proc.stdout).strip()[:600])
    return proc.stdout


# ══════════════════════════════════════════════════════════════════
# 3. 能力探测
# ══════════════════════════════════════════════════════════════════


def probe_capability():
    """读 di.di_capability（设计文档 §2.0 的能力探测视图）。视图本身不在 = 兼容层没装。"""
    rows = run_sql(
        "SELECT has_contract_table, has_po_contract_id, has_po_purchase_type, "
        "       has_party_change_ledger, contract_checks_ready, "
        "       to_regclass('public.biz_snapshot_drift') IS NOT NULL "
        "  FROM di.di_capability;")
    hc, hci, hpt, hled, ready, has_drift = (rows[0] + [""] * 6)[:6]
    return dict(
        has_contract_table=hc == "t",
        has_po_contract_id=hci == "t",
        has_po_purchase_type=hpt == "t",
        has_party_change_ledger=hled == "t",
        contract_checks_ready=ready == "t",
        has_drift_table=has_drift == "t",
    )


def views_installed():
    rows = run_sql("SELECT count(*) FROM pg_views "
                   "WHERE schemaname = 'di' AND viewname ~ '^c_di[0-9]+$';")
    return int(rows[0][0]) if rows and rows[0] and rows[0][0].isdigit() else 0


# ══════════════════════════════════════════════════════════════════
# 4. 单条巡检
# ══════════════════════════════════════════════════════════════════

CHECK_SQL = """
SELECT count(*) OVER () AS hit,
       issue_code, entity_type, entity_id::text, entity_no, field_name,
       snapshot_value, truth_value, truth_source, drift_kind, severity,
       affected_count
  FROM (
        SELECT *
          FROM {view}
         ORDER BY CASE severity WHEN 'ERROR' THEN 0 WHEN 'WARN' THEN 1 ELSE 2 END,
                  entity_no, entity_id
       ) v
 LIMIT {limit};
"""


def run_check(check) -> dict:
    """跑一条巡检。返回结果 dict（不抛异常，错误记在 status/note 里，避免一条炸全盘）。"""
    sql = CHECK_SQL.format(view=check["view"], limit=DETAIL_ROWS)
    out = dict(id=check["id"], name=check["name"], group=check["group"],
               level=check["level"], status="CHECKED", hit=0, rows=[], by_severity={},
               note="")
    try:
        rows = run_sql(sql)
    except Exception as exc:  # noqa: BLE001 —— 巡检项之间必须互相隔离
        out["status"] = "ERROR"
        out["note"] = f"执行失败：{exc}"
        return out

    if not rows:
        out["hit"] = 0
        return out

    out["hit"] = int(rows[0][0])
    detail = []
    for r in rows:
        _, issue_code, etype, eid, eno, field, snap, truth, src, kind, sev, affected = (r + [""] * 12)[:12]
        detail.append(dict(
            issue_code=issue_code, entity_type=etype, entity_id=eid, entity_no=eno,
            field_name=field, snapshot_value=snap, truth_value=truth,
            truth_source=src, drift_kind=kind, severity=sev,
            affected_count=int(affected) if str(affected).isdigit() else None,
        ))
    out["rows"] = detail

    # 命中数必须按**全量**统计，不能只数展示出来的前 5 条（否则汇总会少报）
    try:
        agg = run_sql(f"SELECT severity, count(*) FROM {check['view']} GROUP BY severity;")
        out["by_severity"] = {s: int(n) for s, n in agg if s}
    except Exception as exc:  # noqa: BLE001
        out["status"] = "ERROR"
        out["note"] = f"级别统计失败：{exc}"
        return out

    # 级别 = 本轮观测到的最高级别；无命中时保留该巡检的最高可能级别（只用于展示）
    levels = [LEVEL_ORDER[LEVEL_OF[s]] for s in out["by_severity"] if s in LEVEL_OF]
    if levels:
        worst = min(levels)
        out["level"] = [k for k, v in LEVEL_ORDER.items() if v == worst][0]
    return out


# ══════════════════════════════════════════════════════════════════
# 5. 渲染
# ══════════════════════════════════════════════════════════════════


def fmt_ts(ts):
    return ts.strftime("%Y-%m-%d %H:%M:%S%z")


def render_human(results, cap, started, exit_code, note_stream=sys.stdout):
    w = note_stream.write
    w("=" * 84 + "\n")
    w("云枢 · 快照一致性巡检（B1，只报不修）\n")
    w(f"库: {DB['name']}@{DB['host']}:{DB['port']}   生成: {fmt_ts(started)}\n")
    if cap:
        w("能力: 合同表=%s 采购单合同列=%s 变更台账=%s 结果表 biz_snapshot_drift=%s\n" % (
            "就位" if cap["has_contract_table"] else "缺失",
            "就位" if cap["contract_checks_ready"] else "缺失",
            "就位" if cap["has_party_change_ledger"] else "无（按「无台账 ⇒ P0」保守处理）",
            "就位" if cap["has_drift_table"] else "缺失（本脚本只报不落表，见卡 B2）"))
    w("=" * 84 + "\n")

    for g in GROUPS:
        items = [r for r in results if r["group"] == g]
        if not items:
            continue
        w(f"\n── {g}（{len(items)} 项）" + "─" * max(0, 70 - len(g) * 2) + "\n")
        for r in items:
            if r["status"] == "SKIPPED":
                w(f"  ⊘ {r['id']}  {r['name']}  —— 跳过（{r['note']}）\n")
                continue
            if r["status"] == "ERROR":
                w(f"  ✗ {r['id']}  {r['name']}  —— 执行失败：{r['note']}\n")
                continue
            if r["hit"] == 0:
                w(f"  ✓ {r['id']}  {r['name']}  通过\n")
                continue
            w(f"  {MARK.get(r['level'], '·')} {r['id']}  {r['name']}  {r['hit']} 条 [{r['level']}]\n")
            for row in r["rows"]:
                affected = row.get("affected_count") or 0
                extra = f" 影响{affected}张单" if affected > 1 else ""
                w(f"       ▸ {row['entity_type']} {row['entity_id']} ({row['entity_no']}) "
                  f"{row['field_name']}: 快照={row['snapshot_value']} ≠ 真相={row['truth_value']}"
                  f" [{row['drift_kind']}/{row['severity']}]{extra}\n")
                w(f"         真相来源: {row['truth_source']}\n")
            if r["hit"] > len(r["rows"]):
                w(f"       … 另有 {r['hit'] - len(r['rows'])} 条未展开（详见 --json 或库内视图）\n")

    w("\n" + "=" * 84 + "\n")
    checked = [r for r in results if r["status"] == "CHECKED"]
    skipped = [r for r in results if r["status"] == "SKIPPED"]
    errored = [r for r in results if r["status"] == "ERROR"]
    passed = [r for r in checked if r["hit"] == 0]
    by_level = {k: 0 for k in LEVEL_ORDER}
    for r in checked:
        for sev, n in (r.get("by_severity") or {}).items():
            lv = LEVEL_OF.get(sev)
            if lv:
                by_level[lv] += n
    w(f"汇总: BLOCKER {by_level['BLOCKER']} / WARN {by_level['WARN']} / INFO {by_level['INFO']}"
      f"   巡检项 {len(results)}（通过 {len(passed)}、有命中 {len(checked) - len(passed)}、"
      f"跳过 {len(skipped)}、失败 {len(errored)}）\n")
    if skipped:
        w("⚠ 跳过项不是「通过」：0 行 ≠ 无缺陷（设计文档 §3.5）。要覆盖它们需要合同 schema 就位。\n")
    w(f"退出码: {exit_code}\n")
    w("=" * 84 + "\n")


def build_json(results, cap, started, exit_code, dry_run=False) -> dict:
    by_level = {k: 0 for k in LEVEL_ORDER}
    for r in results:
        for sev, n in (r.get("by_severity") or {}).items():
            lv = LEVEL_OF.get(sev)
            if lv:
                by_level[lv] += n
    checked = [r for r in results if r["status"] == "CHECKED"]
    return dict(
        generated_at=started.isoformat(timespec="seconds"),
        dry_run=dry_run,
        database=dict(host=DB["host"], port=DB["port"], name=DB["name"], user=DB["user"]),
        capability=cap,
        summary=dict(
            blocker=by_level["BLOCKER"], warn=by_level["WARN"], info=by_level["INFO"],
            checks_total=len(results),
            checks_checked=len(checked),
            checks_skipped=len([r for r in results if r["status"] == "SKIPPED"]),
            checks_failed=len([r for r in results if r["status"] == "ERROR"]),
            checks_passed=len([r for r in checked if r["hit"] == 0]),
            exit_code=exit_code,
        ),
        checks=results,
    )


# ══════════════════════════════════════════════════════════════════
# 6. 只读护栏 + 告警落盘
# ══════════════════════════════════════════════════════════════════


def acquire_lock():
    """同一时刻只允许一轮巡检（cron 与手工触发撞车时挡掉后者）。

    返回文件句柄（拿到锁）/"unlocked"（锁文件不可写，只告警不拦巡检）/None（别人在跑）。
    """
    try:
        fh = open(LOCK_FILE, "w")
    except OSError as exc:
        sys.stderr.write(f"⚠ 锁文件不可写（{LOCK_FILE}: {exc}），本轮不加互斥继续跑\n")
        return "unlocked"
    try:
        fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        fh.close()
        return None
    return fh


def append_log(path, text):
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(text)
        return None
    except OSError as exc:
        return f"{path}: {exc}"


def write_json_file(path, payload):
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(payload, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        os.replace(tmp, path)
        return None
    except OSError as exc:
        return f"{path}: {exc}"


def log_text(payload, results):
    s = payload["summary"]
    db = payload["database"]
    head = (f"[{payload['generated_at']}] db={db['name']}@{db['host']} "
            f"BLOCKER={s['blocker']} WARN={s['warn']} INFO={s['info']} "
            f"checks={s['checks_total']}(passed={s['checks_passed']} "
            f"skipped={s['checks_skipped']} failed={s['checks_failed']}) "
            f"exit={s['exit_code']}")
    lines = [head]
    for r in results:
        if r["status"] == "SKIPPED":
            lines.append(f"  SKIPPED {r['id']} {r['name']} — {r['note']}")
        elif r["status"] == "ERROR":
            lines.append(f"  FAILED  {r['id']} {r['name']} — {r['note']}")
        elif r["hit"]:
            ids = ", ".join(f"{x['entity_no']}({x['entity_id'][:8]})" for x in r["rows"])
            lines.append(f"  HIT {r['level']:7s} {r['id']} {r['name']} — {r['hit']} 条: {ids}")
    if not any(r["hit"] or r["status"] != "CHECKED" for r in results):
        lines.append("  ALL PASS（本轮无 BLOCKER/WARN/INFO 命中）")
    return "\n".join(lines) + "\n"


def notify(cmd, payload, json_path, blockers):
    """有 BLOCKER 时执行通知钩子。

    ⚠️ 设计文档 §5.1 要求走 `notice::dispatch`（站内信 + SSE 扇出）。它是 **in-process
    的 Rust 函数**（要连接池、模板渲染、收件人扇出），没有 CLI 入口；从 cron 只能经
    `POST /api/business/notice/send` 打后端实例（需要登录 token 与实例在线）。
    因此本脚本把「怎么通知」做成一个外部钩子：不配 `--notify-cmd` 就只写日志 + JSON
    结果文件（卡面允许的退路），由 B2 卡接线；配了就把 JSON 路径和摘要通过环境变量传进去，
    钩子怎么实现（curl 打接口 / 转其它工单系统）与巡检本身解耦。
    """
    if not cmd:
        return "（未配置 --notify-cmd：只写日志 + JSON 结果文件，通知接线留给 B2 卡）"
    env = dict(os.environ)
    env.update(
        SNAPSHOT_AUDIT_JSON=json_path or "",
        SNAPSHOT_AUDIT_BLOCKERS=str(blockers),
        SNAPSHOT_AUDIT_SUMMARY=(f"快照一致性巡检检出 {blockers} 条 BLOCKER"
                                f"（库 {payload['database']['name']}，{payload['generated_at']}）"),
    )
    try:
        proc = subprocess.run(cmd, shell=True, capture_output=True, text=True,
                              env=env, timeout=60)
        if proc.returncode == 0:
            return f"通知钩子已执行：{cmd}"
        return f"通知钩子返回 {proc.returncode}（不改变巡检结果）：{(proc.stderr or '').strip()[:200]}"
    except Exception as exc:  # noqa: BLE001 —— 通知失败绝不影响巡检结论
        return f"通知钩子异常（不改变巡检结果）：{exc}"


# ══════════════════════════════════════════════════════════════════
# 7. main
# ══════════════════════════════════════════════════════════════════


def select_checks(group):
    if not group:
        return list(CHECKS)
    g = group.strip()
    if g in GROUPS:
        return [c for c in CHECKS if c["group"] == g]
    hit = [c for c in CHECKS if c["id"].lower() == g.lower()]
    if hit:
        return hit
    hit = [c for c in CHECKS if g in c["name"] or g in c["id"]]
    if hit:
        return hit
    sys.stderr.write(f"--group {group!r} 不认识。可用组：{' / '.join(GROUPS)}；"
                     f"也可直接给编号（DI-06）或名称片段。\n")
    sys.exit(2)


def dry_run(selected, as_json):
    plan = [dict(id=c["id"], group=c["group"], name=c["name"], level=c["level"],
                 view=c["view"], requires_contract=c["contract"], meaning=c["meaning"],
                 sql=CHECK_SQL.format(view=c["view"], limit=DETAIL_ROWS).strip())
            for c in selected]
    missing = [f for f in INSTALL_FILES
               if not os.path.isfile(os.path.join(SQL_DIR, f))]
    if as_json:
        print(json.dumps(dict(dry_run=True, sql_dir=SQL_DIR, sql_files_missing=missing,
                              database=dict(host=DB["host"], port=DB["port"],
                                            name=DB["name"], user=DB["user"]),
                              checks=plan), ensure_ascii=False, indent=2))
    else:
        print("=" * 84)
        print("云枢 · 快照一致性巡检（--dry-run：只列计划，不连库、不产出日志/JSON 文件）")
        print(f"库(计划): {DB['name']}@{DB['host']}:{DB['port']}   巡检 SQL 目录: {SQL_DIR}")
        print(f"SQL 文件: {', '.join(INSTALL_FILES)} {'（缺：' + ', '.join(missing) + '）' if missing else '（齐备）'}")
        print("=" * 84)
        for g in GROUPS:
            items = [c for c in plan if c["group"] == g]
            if not items:
                continue
            print(f"\n── {g}（{len(items)} 项）")
            for c in items:
                dep = "依赖合同 schema" if c["requires_contract"] else "无依赖"
                print(f"  {c['id']}  {c['name']}  [最高 {c['level']}]  {c['view']}（{dep}）")
                print(f"        口径: {c['meaning']}")
        print(f"\n合计 {len(plan)} 项；实跑请去掉 --dry-run（只读事务，不改任何业务数据）。")
    return 0


def main():
    ap = argparse.ArgumentParser(
        description="云枢快照一致性巡检（只报不修；有 BLOCKER 退出码 1）")
    ap.add_argument("--json", action="store_true", help="stdout 输出纯 JSON（人看格式转 stderr）")
    ap.add_argument("--group", default=None, help=f"只跑某一组：{' / '.join(GROUPS)}（也可给 DI-06）")
    ap.add_argument("--dry-run", action="store_true", help="不连库：只列出将执行的巡检与 SQL")
    ap.add_argument("--install-views", action="store_true",
                    help="先幂等安装 di 巡检视图（读 docs/architecture/snapshot-drift/*.sql）")
    ap.add_argument("--out", default=None, help="JSON 结果文件路径（cron 用）")
    ap.add_argument("--log-file", default=None, help=f"追加日志路径（默认 {DEFAULT_LOG}）")
    ap.add_argument("--notify-cmd", default=os.environ.get("SNAPSHOT_AUDIT_NOTIFY_CMD"),
                    help="有 BLOCKER 时执行的通知钩子（shell 命令）")
    args = ap.parse_args()

    selected = select_checks(args.group)
    started = datetime.now(TZ)

    if args.dry_run:
        return dry_run(selected, args.json)

    lock = acquire_lock()
    if lock is None:
        sys.stderr.write(f"另一轮巡检正在跑（{LOCK_FILE} 被占用），本轮跳过。\n")
        return 3
    # 人看格式在 --json 时走 stderr，保证 stdout 可被 jq/程序直接吃
    human = sys.stderr if args.json else sys.stdout

    # ① 兼容层：di 视图不存在就必须显式安装，绝不静默跳过整组
    try:
        installed = views_installed()
        if installed == 0 and not args.install_views:
            sys.stderr.write(
                "di 巡检视图不存在（di.c_diNN）。\n"
                f"请先执行：python3 {os.path.relpath(__file__, REPO_ROOT)} --install-views\n"
                f"（安装脚本：{SQL_DIR}/00_install_compat.sql + 10_checks.sql；只建视图，回滚 DROP SCHEMA di CASCADE）\n")
            return 2
        if args.install_views:
            before = installed
            for name in INSTALL_FILES:
                path = os.path.join(SQL_DIR, name)
                if not os.path.isfile(path):
                    sys.stderr.write(f"缺少巡检 SQL 文件：{path}\n")
                    return 2
            for name in INSTALL_FILES:
                run_sql_file(os.path.join(SQL_DIR, name))
            sys.stderr.write(f"已幂等安装 di 巡检视图（{before} → {views_installed()} 个 c_di* 视图）\n")
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write(f"连接数据库 / 安装视图失败：{exc}\n")
        return 2

    # ② 能力探测 → 决定哪些项必须记 SKIPPED
    try:
        cap = probe_capability()
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write(f"读取 di.di_capability 失败：{exc}\n")
        return 2

    # ③ 逐条跑
    results = []
    for check in selected:
        if check["contract"] and not cap["contract_checks_ready"]:
            results.append(dict(id=check["id"], name=check["name"], group=check["group"],
                                level=check["level"], status="SKIPPED", hit=0, rows=[],
                                by_severity={},
                                note="合同 schema 未就位（contract_checks_ready=false）：0 行 ≠ 无缺陷"))
            continue
        results.append(run_check(check))

    # ④ 汇总
    s_json = build_json(results, cap, started, 0)
    blockers = s_json["summary"]["blocker"]
    failed = s_json["summary"]["checks_failed"]
    exit_code = 1 if (blockers or failed) else 0
    payload = build_json(results, cap, started, exit_code)

    # ⑤ 落盘：日志永远写；JSON 结果文件按 --out 写（B2 的告警接线读它）
    log_path = args.log_file or DEFAULT_LOG
    warn = append_log(log_path, log_text(payload, results))
    if warn:
        sys.stderr.write(f"⚠ 日志写入失败（不改变巡检结论）：{warn}\n")
    if args.out:
        warn = write_json_file(args.out, payload)
        if warn:
            sys.stderr.write(f"⚠ JSON 结果写入失败（不改变巡检结论）：{warn}\n")

    # ⑥ 渲染
    render_human(results, cap, started, exit_code, note_stream=human)
    if args.json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))

    # ⑦ 告警：只有 BLOCKER（或巡检项本身执行失败）才打扰人；只有 WARN 一律只写日志
    if blockers or failed:
        msg = notify(args.notify_cmd, payload, args.out, blockers)
        sys.stderr.write(f"告警：{msg}\n")
        append_log(log_path, f"  ALERT blockers={blockers} checks_failed={failed} → {msg}\n")

    if lock != "unlocked":
        lock.close()
    return exit_code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
