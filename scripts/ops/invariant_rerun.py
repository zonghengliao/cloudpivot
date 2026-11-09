#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""账务巡检「发布/迁移后重跑」——NEAR-FIX-4 修法①

为什么需要这个脚本
==================
巡检结论是**快照**：`sys_invariant_result` 里最后一条只描述它跑的那一刻。内置 cron
（`0 20 2 * * *`，实测按 UTC 解释 = 每天 10:20 CST）一天才落一条。于是「修账的迁移」与
「告警读的结论」之间没有任何先后关系判断：

    11-09 10:20  SCHEDULED 巡检落 FAIL（H6/H5）
    11-09 22:23  V0366 把该缺陷修好并应用   ← 账已经是对的
    11-10 03:1x  告警仍喊「账已错」          ← 它读的是 17 小时前那条结论

本脚本就是补那条缺失的动作：**迁移/发布落地后立刻重跑一次巡检并落库**，让结论追上事实。
配套的另一半在 `scripts/ops/alert_check.sh`：即使没人重跑，判定侧也会认「结论早于最后一次
迁移」，降级成「结论过期、待重跑」而不是断言「账已错」。

两条执行路径（默认脚本 + 落库；`--http` 才走接口）
==================================================
1. 默认：直接跑闸③ 同一个脚本 `scripts/verify/erp_invariants.py --json`，按
   `runner::parse_report` 的口径从 `results` 重算计数，再落一行 `trigger_type='MANUAL'`。
   **口径不放宽**：解析规则、计数规则、ERROR 行语义与 Rust runner 逐条对齐（见下）。
2. `--http`（显式开启）：`POST /api/support/invariant/run`（需**运维账号**会话，
   `CLOUDPIVOT_OPS_USER/PASSWORD`，见 docs/运维规程-凭据与自查账号.md）→ 真
   `run_and_store(Manual)`，与看板「立即执行」同一条路。**默认不开**：登录要真口令，
   试错一次就给该账号记一次登录失败（5 次锁 30 分钟，实测 2026-11-10 03:33~03:35
   把 admin 锁掉）—— 例行部署不该拿生产账号试口令。

与 Rust runner 的对齐点（改任何一边都要同步另一边的自检）
==========================================================
- `parse_report`：取 stdout 里**最后一个** `{\n  "total"` 起点当 JSON 段（脚本先打中文报告）；
  找不到 / JSON 非法 / `results` 为空 ⇒ ERROR，**不静默假绿**。
- 计数：`total=len(results)`、`passed=ok 的条数`、`failed=total-passed`、
  `blockers=!ok 且 level==BLOCKER`、`warns=!ok 且 level==WARN`、`status=OK iff failed==0`。
- 落库：`trigger_type='MANUAL'`（表上 CHECK 只认 SCHEDULED/MANUAL）、`run_at` 取**开跑时刻**、
  `detail`=全量 results、`failures`=失败项子集、脚本没跑成则落 `status='ERROR'`+`error` 原因。

用法
====
    python3 scripts/ops/invariant_rerun.py                 # 结论比最后一次迁移旧才重跑（幂等）
    python3 scripts/ops/invariant_rerun.py --force         # 无条件重跑
    python3 scripts/ops/invariant_rerun.py --dry-run       # 只体检并打印计划，不跑不写
    python3 scripts/ops/invariant_rerun.py --http          # 先试 HTTP（需运维账号口令正确，见下）
    python3 scripts/ops/invariant_rerun.py --self-test     # 自检（解析/计数/单位归一/新鲜度）

默认走**脚本 + 落库**那条路，不碰登录。`--http` 才去试 `POST /api/support/invariant/run`：
登录失败会给该账号记一次失败（`sys_config` 的 `max_login_fail_count` 次即锁，
实测 2026-11-10 03:33~03:35 三次探测把 admin 锁了 30 分钟）——所以只有口令确认可用时才该开它。
口令漂移的处置见 docs/运维规程-凭据与自查账号.md，别在这里反复试。

部署脚本里由 `scripts/deploy/deploy.sh` 第 [7/9] 步调用（非致命：失败只 WARN，不回滚发版）。

退出码：0 = 已落一条不早于最后一次迁移的结论（或幂等跳过）
        3 = 巡检脚本没跑成（已落 ERROR 行：这条事实必须被看见）
        4 = 落库成功但结论仍早于最后一次迁移（异常：期间又落了迁移）
        5 = 落不了库（无 psql / 无口令 / SQL 失败 / 两条路都不可用）
"""

import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

# ── 与 rus runner 对齐的常量 ────────────────────────────────────────────────
JSON_NEEDLE = '{\n  "total"'          # runner.rs::parse_report 的 NEEDLE，逐字对齐
SCRIPT_RELATIVE = "scripts/verify/erp_invariants.py"
SCRIPT_ENV = "CLOUDPIVOT_INVARIANT_SCRIPT"
PYTHON_ENV = "CLOUDPIVOT_PYTHON"
SCRIPT_TIMEOUT_SECS = 300             # runner.rs::SCRIPT_TIMEOUT_SECS
DEFAULT_ENV_FILE = "/etc/cloudpivot/cloudpivot.env"
DB_ENV_KEYS = (
    "NIK_DATABASE_HOST",
    "NIK_DATABASE_PORT",
    "NIK_DATABASE_NAME",
    "NIK_DATABASE_USER",
    "NIK_DATABASE_PASSWORD",
    "NIK_DATABASE_SSL_MODE",
)

# ── schema_migrations.applied_at 的单位归一 ────────────────────────────────
# 契约是 epoch **秒**（vendor/nik-db/src/migrate/history.rs 写的就是秒），但历史上手工 psql
# 补记账的行写成了**毫秒**（144 实测 4 行，to_timestamp 出来是 58826 年）。混单位直接比大小
# 会让「最后一次迁移」永远晚于结论（毫秒 ≫ 秒）⇒ 所有结论都被判过期、判定被永久降级。
# 归一规则：> 1e11 视为毫秒（1e11 秒 = 5138 年、1e11 毫秒 = 1973 年，两种单位不重叠）。
MIG_EPOCH_MS_FLOOR = 100_000_000_000
MIG_EPOCH_SQL = (
    f"CASE WHEN applied_at > {MIG_EPOCH_MS_FLOOR} THEN applied_at / 1000 ELSE applied_at END"
)


# ══════════════════════════ 纯逻辑（自检覆盖） ══════════════════════════
def parse_report(stdout: str) -> dict:
    """从脚本 stdout 里解析末尾 JSON 段，并按 runner 口径重算计数。

    与 `backend/src/support/invariant/runner.rs::parse_report` 一一对应：
    取最后一个 JSON 段起点 → 解析 → `results` 必须是数组且非空 → 计数只信 `results`。

    返回 dict(total, passed, failed, blockers, warns, results, failures)。
    """
    start = stdout.rfind(JSON_NEEDLE)
    if start < 0:
        raise ValueError("巡检脚本输出里找不到 JSON 结果段（未加 --json 或脚本中途崩溃）")
    try:
        value = json.loads(stdout[start:].strip())
    except json.JSONDecodeError as exc:
        raise ValueError(f"解析巡检 JSON 失败：{exc}") from exc
    results = value.get("results")
    if not isinstance(results, list):
        raise ValueError("巡检 JSON 缺少 results 数组")
    if not results:
        raise ValueError("巡检结果的 results 为空：脚本一条检查都没跑（疑似连不上库或参数错误）")
    passed = sum(1 for item in results if item.get("ok") is True)
    total = len(results)
    failed = total - passed
    blockers = sum(
        1 for item in results
        if item.get("ok") is not True and str(item.get("level", "")).upper() == "BLOCKER"
    )
    warns = sum(
        1 for item in results
        if item.get("ok") is not True and str(item.get("level", "")).upper() == "WARN"
    )
    failures = [item for item in results if item.get("ok") is not True]
    return dict(
        total=total, passed=passed, failed=failed, blockers=blockers,
        warns=warns, results=results, failures=failures,
    )


def normalize_migration_epoch(value: int) -> int:
    """把 schema_migrations.applied_at 归一到 epoch 秒（与 MIG_EPOCH_SQL 同一规则）。"""
    return value // 1000 if value > MIG_EPOCH_MS_FLOOR else value


def staleness(run_epoch: int, mig_epoch: int) -> int:
    """返回「最后一次迁移比结论晚多少秒」，<=0 表示结论不旧。"""
    return max(mig_epoch - run_epoch, 0)


# ══════════════════════════ 环境 / psql ══════════════════════════
def load_env_file(path: str) -> dict:
    """只挑 NIK_DATABASE_* 这几行（不 source 整个文件），与 alert_check.sh 同口径。"""
    loaded = {}
    try:
        with open(path, "r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                key, _, value = line.partition("=")
                key = key.strip()
                if key in DB_ENV_KEYS or key in (
                    "NIK_ADMIN_USERNAME", "NIK_ADMIN_PASSWORD",
                    # 自查/验收登录口径（卡 t_d4e92b99）：运维账号，优先于 NIK_ADMIN_*
                    "CLOUDPIVOT_OPS_USER", "CLOUDPIVOT_OPS_PASSWORD",
                    "NIK_OPS_USER", "NIK_OPS_PASSWORD",
                ):
                    loaded[key] = value.strip().strip('"').strip("'")
    except OSError:
        return {}
    return loaded


def db_config(args) -> dict:
    """库连接参数：进程环境 → env 文件 → 缺省（与 erp_invariants.py 相同的默认值）。"""
    conf = {"host": "127.0.0.1", "port": "5432", "user": "cloudpivot",
            "db": "cloudpivot", "password": ""}
    candidates = [dict(os.environ)]
    if args.env_file:
        candidates.append(load_env_file(args.env_file))
    for source in candidates:
        conf["host"] = source.get("NIK_DATABASE_HOST", conf["host"])
        conf["port"] = source.get("NIK_DATABASE_PORT", conf["port"])
        conf["user"] = source.get("NIK_DATABASE_USER", conf["user"])
        conf["db"] = source.get("NIK_DATABASE_NAME", conf["db"])
        conf["password"] = source.get("NIK_DATABASE_PASSWORD", conf["password"])
    return conf


def psql(conf: dict, sql: str) -> tuple[bool, str]:
    """跑一条 SQL（psql -X -q -A -t）。返回 (ok, stdout 或 stderr 摘要)。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = conf["password"]
    cmd = [
        "psql", "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1",
        "-h", conf["host"], "-p", str(conf["port"]), "-U", conf["user"], "-d", conf["db"],
        "-c", sql,
    ]
    try:
        proc = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=120)
    except FileNotFoundError:
        return False, "本机没有 psql（无法读/写 sys_invariant_result）"
    except subprocess.TimeoutExpired:
        return False, "psql 超时（120s）"
    if proc.returncode != 0:
        return False, (proc.stderr or proc.stdout).strip()[:400]
    return True, proc.stdout.strip()


def dollar_quote(payload: str) -> str:
    """把字符串包成 dollar-quoted 字面量（JSON 里的单引号不用再转义）。"""
    tag = "$nv$"
    index = 0
    while tag in payload:
        index += 1
        tag = f"$nv{index}$"
    return f"{tag}{payload}{tag}"


# ══════════════════════════ 现状 / 落库 ══════════════════════════
LATEST_SQL = (
    "SELECT EXTRACT(EPOCH FROM r.run_at)::bigint || '|' || r.status || '|' || r.trigger_type"
    " || '|' || r.id"
    f" || '|' || GREATEST(COALESCE((SELECT max({MIG_EPOCH_SQL}) FROM schema_migrations"
    " WHERE success), 0) - EXTRACT(EPOCH FROM r.run_at)::bigint, 0)"
    " || '|' || COALESCE((SELECT max(" + MIG_EPOCH_SQL + ") FROM schema_migrations"
    " WHERE success), 0)"
    " FROM sys_invariant_result r ORDER BY r.run_at DESC LIMIT 1"
)


def latest_state(conf: dict) -> dict:
    """读「最后一条结论 + 最后一次迁移」：返回 dict(run_epoch, status, trigger, id, newer, mig_epoch)。"""
    ok, out = psql(conf, LATEST_SQL)
    if not ok:
        return {"error": out}
    if not out:
        return {"empty": True, "mig_epoch": _mig_epoch(conf)}
    parts = out.split("|")
    if len(parts) != 6:
        return {"error": f"最后一条结论格式意外：{out[:200]}"}
    run_epoch, status, trigger, row_id, newer, mig_epoch = parts
    return {
        "run_epoch": int(run_epoch), "status": status, "trigger": trigger,
        "id": row_id, "newer": int(newer), "mig_epoch": int(mig_epoch),
    }


def _mig_epoch(conf: dict) -> int:
    ok, out = psql(conf, f"SELECT COALESCE(max({MIG_EPOCH_SQL}), 0) FROM schema_migrations WHERE success")
    return int(out) if ok and out.isdigit() else 0


def insert_row(conf: dict, run_epoch: int, trigger: str, status: str, counts: dict,
               duration_ms: int, script_path: str, error: str | None) -> tuple[bool, str]:
    """落一行结果，返回 (ok, `id|run_epoch` 或错误原因)。字段与 runner::run_and_store 对齐。"""
    detail = dollar_quote(json.dumps(counts["results"], ensure_ascii=False)) if counts else "NULL"
    failures = dollar_quote(json.dumps(counts["failures"], ensure_ascii=False)) if counts else "NULL"
    error_sql = dollar_quote(error) if error else "NULL"
    sql = (
        "INSERT INTO sys_invariant_result"
        " (id, run_at, trigger_type, status, total, passed, failed, blockers, warns,"
        "  duration_ms, database_name, script_path, detail, failures, error, created_at)"
        " VALUES (gen_random_uuid(), to_timestamp(" + str(run_epoch) + f"), '{trigger}', '{status}',"
        f" {counts['total'] if counts else 0}, {counts['passed'] if counts else 0},"
        f" {counts['failed'] if counts else 0}, {counts['blockers'] if counts else 0},"
        f" {counts['warns'] if counts else 0}, {duration_ms},"
        f" {dollar_quote(conf['db'])}, {dollar_quote(script_path)}, {detail}, {failures},"
        f" {error_sql}, CURRENT_TIMESTAMP)"
        " RETURNING id || '|' || EXTRACT(EPOCH FROM run_at)::bigint"
    )
    return psql(conf, sql)


# ══════════════════════════ 路径 ①：HTTP（真 runner） ══════════════════════════
def http_run_invariant(base_url: str, conf: dict) -> tuple[bool, dict | str]:
    """走 `POST /api/support/invariant/run`（**运维账号**会话）。失败返回原因，不抛异常。

    登录口径（卡 t_d4e92b99）：`CLOUDPIVOT_OPS_USER` / `CLOUDPIVOT_OPS_PASSWORD` 优先；
    回退 `NIK_OPS_*`；最后才回退 `NIK_ADMIN_*`（种子语义，仅 dev/test 库成立）。
    登录**只试一次** —— 401 不重试：`sys_user.login_fail_count` 攒到 5 就把账号锁 30 分钟
    （`backend/src/shared/constants.rs`），例行部署不该拿生产账号试口令。
    """
    env_file = load_env_file(os.environ.get("NIK_ALERT_ENV_FILE", DEFAULT_ENV_FILE))

    def pick(*keys: str) -> str:
        for key in keys:
            value = os.environ.get(key) or env_file.get(key)
            if value:
                return value
        return ""

    username = pick("CLOUDPIVOT_OPS_USER", "NIK_OPS_USER", "NIK_ADMIN_USERNAME") or "admin"
    password = pick("CLOUDPIVOT_OPS_PASSWORD", "NIK_OPS_PASSWORD", "NIK_ADMIN_PASSWORD")
    if not password:
        return False, (
            "没有可用的登录凭据：env 文件需配 CLOUDPIVOT_OPS_USER/CLOUDPIVOT_OPS_PASSWORD"
            "（见 docs/运维规程-凭据与自查账号.md）；不会去猜 admin 口令"
        )
    try:
        token = _login(base_url, username, password)
    except _HttpFail as exc:
        return False, f"{username} 登录失败（不重试，避免锁号）：{exc}"
    try:
        payload = _post_json(f"{base_url}/api/support/invariant/run", {}, token)
    except _HttpFail as exc:
        return False, f"POST /api/support/invariant/run 失败：{exc}"
    data = payload.get("data") or {}
    if not data.get("id"):
        return False, f"接口返回体里没有 data.id：{json.dumps(payload, ensure_ascii=False)[:200]}"
    return True, data


class _HttpFail(Exception):
    """HTTP 路径的失败（登录 401 / 接口 5xx / 连不上）。"""


def _post_json(url: str, body: dict, token: str | None) -> dict:
    data = json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method="POST")
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(request, timeout=310) as response:
            return json.loads(response.read().decode() or "{}")
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:200]
        raise _HttpFail(f"HTTP {exc.code} {detail}") from exc
    except (urllib.error.URLError, OSError, json.JSONDecodeError) as exc:
        raise _HttpFail(str(exc)) from exc


def _login(base_url: str, username: str, password: str) -> str:
    payload = _post_json(
        f"{base_url}/api/system/auth/login",
        {"username": username, "password": password}, None,
    )
    token = ((payload.get("data") or {}).get("access_token") or "")
    if not token:
        raise _HttpFail("登录返回里没有 access_token（口令不对？）")
    return token


# ══════════════════════════ 路径 ②：脚本 + 落库 ══════════════════════════
def run_script(conf: dict, script: Path) -> tuple[int, str, int]:
    """跑巡检脚本，返回 (耗时毫秒, stdout+stderr, 退出码)。"""
    python = (os.environ.get(PYTHON_ENV) or "python3").strip() or "python3"
    env = dict(os.environ)
    env["NIK_DATABASE_HOST"] = conf["host"]
    env["NIK_DATABASE_PORT"] = str(conf["port"])
    env["NIK_DATABASE_NAME"] = conf["db"]
    env["NIK_DATABASE_USER"] = conf["user"]
    env["NIK_DATABASE_PASSWORD"] = conf["password"]
    started = time.monotonic()
    try:
        proc = subprocess.run(
            [python, str(script), "--json"],
            cwd=str(script.parent.parent),          # runner 也是以 scripts/ 为 cwd
            env=env, capture_output=True, text=True, timeout=SCRIPT_TIMEOUT_SECS,
        )
        return int((time.monotonic() - started) * 1000), proc.stdout + proc.stderr, proc.returncode
    except subprocess.TimeoutExpired:
        return SCRIPT_TIMEOUT_SECS * 1000, "", -1
    except OSError as exc:
        return 0, f"无法启动巡检脚本（{python} {script}）：{exc}", -2


def resolve_script(args) -> Path | None:
    """脚本路径：--script → CLOUDPIVOT_INVARIANT_SCRIPT → <repo>/scripts/verify/erp_invariants.py。"""
    candidates = []
    if args.script:
        candidates.append(Path(args.script))
    if os.environ.get(SCRIPT_ENV):
        candidates.append(Path(os.environ[SCRIPT_ENV].strip()))
    candidates.append(Path(args.repo) / SCRIPT_RELATIVE)
    for path in candidates:
        if path.is_file():
            return path.resolve()
    return None


# ══════════════════════════ 自检 ══════════════════════════
def self_test(args) -> int:
    """自检：解析 / 计数 / 单位归一 / 新鲜度（不依赖库；库可用时额外验归一 SQL）。"""
    total = 0
    failed = 0

    def expect(desc: str, want, got):
        nonlocal total, failed
        total += 1
        if want == got:
            print(f"  [PASS] {desc}")
        else:
            print(f"  [FAIL] {desc}（期望 {want!r}，实得 {got!r}）")
            failed += 1

    print("== 解析（与 runner::parse_report 同口径）==")
    real_stdout = (
        "====\n云枢 ERP 不变量审计套件\n====\n  ✅ [会计] A1 借贷平衡\n"
        "\n====\n结果：1/2 通过   🚨 BLOCKER 1   ⚠️ WARN 0\n====\n\n"
        '{\n  "total": 2,\n  "passed": 1,\n  "blockers": 1,\n  "warns": 0,\n  "results": [\n'
        '    {"name": "A1 借贷平衡", "group": "会计", "level": "BLOCKER", "ok": true, "detail": ""},\n'
        '    {"name": "B2 总账明细", "group": "账账", "level": "WARN", "ok": false, "detail": "差 12.50"}\n'
        "  ]\n}\n"
    )
    report = parse_report(real_stdout)
    expect("末尾 JSON 段被正确解析（只吃 results）", (2, 1, 1, 0, 1),
           (report["total"], report["passed"], report["failed"], report["blockers"], report["warns"]))
    expect("failures 只含 ok=false 的项", 1, len(report["failures"]))
    for desc, payload in (
        ("没有 JSON 段 → 报错", "boom: connection refused"),
        ("results 为空 → 报错", '{\n  "total": 0,\n  "results": []\n}\n'),
        ("results 不是数组 → 报错", '{\n  "total": 1,\n  "results": {}\n}\n'),
    ):
        try:
            parse_report(payload)
            expect(desc, "ValueError", "未报错")
        except ValueError:
            expect(desc, "ValueError", "ValueError")

    print("== 迁移时间单位归一（秒 / 毫秒混用）==")
    expect("秒值不变", 1794244598, normalize_migration_epoch(1794244598))
    expect("毫秒值归一成秒（144 的 4 行手工记账）", 1794232856, normalize_migration_epoch(1794232856000))
    expect("边界：1e11 之下按秒", 100000000000, normalize_migration_epoch(100000000000))
    expect("1e11 之上按毫秒", 100000000, normalize_migration_epoch(100000000001))

    print("== 新鲜度 ==")
    expect("迁移比结论晚 → 正数", 600, staleness(1000, 1600))
    expect("迁移不比结论晚 → 0", 0, staleness(1600, 1000))
    expect("无迁移记录（0）→ 0", 0, staleness(1600, 0))

    conf = db_config(args)
    ok, out = psql(conf, f"SELECT max({MIG_EPOCH_SQL}) FROM "
                         "(VALUES (1794244598::bigint), (1794232856000::bigint)) v(applied_at)")
    if ok:
        expect("SQL 归一（库内实测：毫秒行不该压过秒行）", "1794244598", out)
    else:
        print(f"  [SKIP] SQL 归一实测（库不可用：{out[:120]}）")

    print(f"\n自检{'通过' if failed == 0 else '失败'}：{total - failed}/{total}")
    return 0 if failed == 0 else 3


# ══════════════════════════ 主流程 ══════════════════════════
def main() -> int:
    parser = argparse.ArgumentParser(description="账务巡检「发布/迁移后重跑」（NEAR-FIX-4）")
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parents[2]),
                        help="仓库根（默认由本脚本位置推断）")
    parser.add_argument("--env-file", default=os.environ.get("NIK_ALERT_ENV_FILE", DEFAULT_ENV_FILE),
                        help=f"库口令 env 文件（默认 {DEFAULT_ENV_FILE}）")
    parser.add_argument("--base-url", default=os.environ.get("NIK_ALERT_BASE_URL", "http://127.0.0.1:8023"),
                        help="服务地址（HTTP 路径用）")
    parser.add_argument("--script", default=None, help="巡检脚本路径（默认 <repo>/scripts/verify/erp_invariants.py）")
    parser.add_argument("--force", action="store_true", help="无条件重跑（默认：结论不旧就跳过）")
    parser.add_argument("--http", action="store_true",
                        help="先试 HTTP（POST /api/support/invariant/run，需运维账号口令可用：CLOUDPIVOT_OPS_*）。"
                             "默认不试：登录失败会给该账号记一次失败，5 次即锁 30 分钟")
    parser.add_argument("--db-only", action="store_true", help="（已是默认，保留兼容）不试 HTTP")
    parser.add_argument("--http-only", action="store_true", help="只走 HTTP（隐含 --http），失败即退出（不落库兜底）")
    parser.add_argument("--dry-run", action="store_true", help="只体检并打印计划，不跑不写")
    parser.add_argument("--self-test", action="store_true", help="自检（不需要服务/库）")
    args = parser.parse_args()

    if args.self_test:
        return self_test(args)

    conf = db_config(args)
    state = latest_state(conf)
    if "error" in state:
        print(f"⚠ 读不到 sys_invariant_result：{state['error']}（该库大概没应用 V0235 巡检迁移）")
    if args.dry_run:
        if state.get("empty"):
            print(f"dry-run：库 {conf['db']} 里还没有任何巡检结论 → 将重跑并落一行 MANUAL")
        elif "error" not in state:
            verdict = "结论早于最后一次迁移，需要重跑" if state["newer"] > 0 else "结论不比迁移旧"
            print(f"dry-run：最后结论 {state['status']}@{state['run_epoch']}"
                  f"（{state['trigger']}），最后一次迁移 {state['mig_epoch']}，晚 {state['newer']}s → {verdict}")
        print("dry-run：未跑脚本、未落库")
        return 0

    # 幂等：结论不旧就不必再跑（部署脚本每次发布都会调用本脚本）
    if not args.force and "error" not in state and not state.get("empty"):
        if state["newer"] <= 0:
            print(f"已是最新：最后结论 {state['status']}@{state['run_epoch']} 不早于最后一次迁移"
                  f"（{state['mig_epoch']}）→ 无需重跑")
            return 0

    method = ""
    conclusion = None
    reason = ""

    # 路径 ① HTTP（真 runner）：**默认不走**。144 的 admin 口令漂移（t_c9c2cd5f），
    # 每试一次就给 admin 记一次登录失败，5 次锁 30 分钟 —— 例行部署不该拿生产账号试口令。
    if args.http or args.http_only:
        ok, payload = http_run_invariant(args.base_url, conf)
        if ok and isinstance(payload, dict):
            method = "http"
            conclusion = payload
            print(f"HTTP 路径成功：run_and_store(Manual) id={payload.get('id')} status={payload.get('status')}")
        else:
            reason = str(payload)
            print(f"HTTP 路径不可用（{reason}）→ 退回「脚本 + 落库」")
            if args.http_only:
                return 5

    # 路径 ② 脚本 + 落库
    if not method:
        script = resolve_script(args)
        if script is None:
            print(f"✗ 找不到巡检脚本（找过 --script / {SCRIPT_ENV} / {args.repo}/{SCRIPT_RELATIVE}）")
            return 5
        started_epoch = int(time.time())
        duration_ms, stdout, rc = run_script(conf, script)
        counts = None
        error = None
        try:
            if rc < 0:
                raise ValueError(f"脚本没能跑起来（rc={rc}）")
            counts = parse_report(stdout)
            status = "OK" if counts["failed"] == 0 else "FAIL"
        except ValueError as exc:
            status = "ERROR"
            error = str(exc)
            print(f"✗ 巡检脚本没跑成：{error}")
        ok, out = insert_row(
            conf, started_epoch, "MANUAL", status,
            counts or dict(total=0, passed=0, failed=0, blockers=0, warns=0, results=[], failures=[]),
            duration_ms, str(script), error,
        )
        if not ok:
            print(f"✗ 落库失败：{out}")
            return 5
        method = "script"
        print(f"脚本路径成功：{out} duration_ms={duration_ms} status={status}"
              + (f"（{counts['passed']}/{counts['total']} 通过，BLOCKER {counts['blockers']}）" if counts else ""))

    # 收口核对：新结论必须不早于最后一次迁移（否则「重跑」没解决问题）
    after = latest_state(conf)
    if "error" in after:
        print(f"✗ 落库后读不回来：{after['error']}")
        return 5
    print(f"最新一行：id={after['id']} status={after['status']} trigger={after['trigger']}"
          f" run_at={after['run_epoch']} 最后一次迁移={after['mig_epoch']} 迁移晚 {after['newer']}s")
    print(f"INVARIANT_RERUN method={method} id={after['id']} status={after['status']}"
          f" run_at={after['run_epoch']} mig_epoch={after['mig_epoch']} newer={after['newer']}")
    if after["newer"] > 0:
        print("✗ 结论仍早于最后一次迁移（期间又落了迁移？）—— 告警侧会继续报「结论过期待重跑」")
        return 4
    if after["status"] == "ERROR":
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
