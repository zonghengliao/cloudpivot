#!/usr/bin/env bash
# T9-A 验收：任务状态多选筛选（`statuses=TODO,DOING`）
#
# 用法：
#   bash scripts/verify/t9a_statuses.sh
# 可选覆盖（评审在任意 clone 上复跑）：
#   REPO=/root/t9a-build          # 仓根（要含 backend/ 与 scripts/）
#   T9A_DB=cloudpivot_t9a_iso     # 本脚本自建/自用的隔离库（**不碰 prod / dev / test**）
#   T9A_PORT=18085               # 隔离实例端口
#   T9A_BIN=<binary>             # 默认 $REPO/backend/target/debug/cloudpivot
#   T9A_ENV=/etc/cloudpivot/cloudpivot-dev.env
#
# 做了什么：
#   ① 建隔离库（owner=cloudpivot）+ 起隔离实例（自动跑迁移/种子）
#   ② 造 10 条夹具任务（3 TODO / 3 DOING 含 1 个子任务 / 2 DONE / 1 REVIEW / 1 CANCELLED）
#   ③ 真实 HTTP 断言 8 项：statuses 多选、单值等价、statuses 优先、空串不筛、
#      非法值 400、与 root_only 组合、不传不筛
#   ④ 可选：把库打开 log_statement=all，从 PG 日志证明真的走 `t.status IN ($1,$2)`
#   ⑤ 收尾：删干净夹具（按 task_no 前缀）+ 停实例
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
T9A_DB=${T9A_DB:-cloudpivot_t9a_iso}
T9A_PORT=${T9A_PORT:-18085}
T9A_ENV=${T9A_ENV:-/etc/cloudpivot/cloudpivot-dev.env}
if [ -z "${T9A_BIN:-}" ]; then
  if [ -x "$REPO/backend/target/debug/cloudpivot" ]; then
    T9A_BIN="$REPO/backend/target/debug/cloudpivot"
  elif [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "$CARGO_TARGET_DIR/debug/cloudpivot" ]; then
    T9A_BIN="$CARGO_TARGET_DIR/debug/cloudpivot"
  else
    T9A_BIN="$REPO/backend/target/debug/cloudpivot"
  fi
fi

exec python3 - "$REPO" "$T9A_DB" "$T9A_PORT" "$T9A_ENV" "$T9A_BIN" <<'PY'
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPO, DB, PORT, ENV_FILE, BIN = sys.argv[1:6]
PORT = int(PORT)
BASE = "http://127.0.0.1:%d" % PORT
FIXTURE_PREFIX = "T9A-E2E"
FAILS = []
CHECKS = []


def say(msg):
    print(msg, flush=True)


def check(name, ok, detail=""):
    CHECKS.append((name, ok, detail))
    print(("[PASS] " if ok else "[FAIL] ") + name + ("  " + detail if detail else ""), flush=True)
    if not ok:
        FAILS.append(name)


def read_env(path):
    env = {}
    if not os.path.exists(path):
        return env
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, _, value = line.partition("=")
            env[key.strip()] = value.strip()
    return env


def psql(sql, db=None, user="cloudpivot", password=None, tuples_only=True):
    env = dict(os.environ)
    env["PGPASSWORD"] = password or DB_PASSWORD
    cmd = ["psql", "-h", "127.0.0.1", "-U", user, "-d", db or DB, "-At", "-c", sql]
    out = subprocess.run(cmd, capture_output=True, text=True, env=env)
    if out.returncode != 0:
        return "<ERR: %s>" % out.stderr.strip().splitlines()[-1:] if out.stderr.strip() else "<ERR>"
    return out.stdout.strip() if tuples_only else out.stdout


def super_psql(sql):
    out = subprocess.run(["su", "-", "postgres", "-c", "psql -At -c %s" % shell_quote(sql)],
                         capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else "<ERR: %s>" % out.stderr.strip()


def shell_quote(text):
    return "'" + text.replace("'", "'\"'\"'") + "'"


def http(method, path, body=None, token=None, timeout=20):
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except Exception:
            return err.code, {"raw": raw}


env = read_env(ENV_FILE)
db_password = env.get("NIK_DATABASE_PASSWORD", "")
DB_PASSWORD = db_password
admin_password = env.get("NIK_ADMIN_PASSWORD", "")
if not db_password or not admin_password:
    say("FATAL: %s 缺少 NIK_DATABASE_PASSWORD / NIK_ADMIN_PASSWORD" % ENV_FILE)
    sys.exit(2)

say("========== 0. 隔离库 ==========")
exists = super_psql("select 1 from pg_database where datname='%s'" % DB)
if exists.strip() != "1":
    created = subprocess.run(["su", "-", "postgres", "-c", "createdb -O cloudpivot %s" % DB],
                             capture_output=True, text=True)
    say("createdb %s -> rc=%d %s" % (DB, created.returncode, created.stderr.strip()))
else:
    say("库 %s 已存在" % DB)
say("owner = %s" % psql("select pg_get_userbyid(datdba) from pg_database where datname=current_database()"))

# 语句级 SQL 证据（步骤 5 用）：只对本卡隔离库开，收尾 RESET
alter = subprocess.run(
    ["su", "-", "postgres", "-c", "psql -At -c %s" % shell_quote(
        "ALTER DATABASE %s SET log_statement='all'" % DB)],
    capture_output=True, text=True)
say("ALTER DATABASE log_statement=all -> rc=%d %s" % (alter.returncode, alter.stderr.strip()[:120]))

say("")
say("========== 1. 起隔离实例（本脚本独占端口 %d，每次先踢掉旧进程）==========" % PORT)
log_path = "/tmp/t9a_e2e_instance_%d_%d.log" % (PORT, int(time.time()))


def pids_on_port(port):
    out = subprocess.run(["ss", "-ltnp"], capture_output=True, text=True).stdout
    pids = set()
    for line in out.splitlines():
        if (":%d " % port) in line:
            for hit in re.findall(r"pid=(\d+)", line):
                pids.add(int(hit))
    return pids


def stop_instance():
    for pid in pids_on_port(PORT):
        try:
            os.kill(pid, 15)
            say("已停旧实例 pid=%d" % pid)
        except ProcessLookupError:
            pass
    deadline = time.time() + 20
    while time.time() < deadline and pids_on_port(PORT):
        time.sleep(0.5)


stop_instance()
inst_env = dict(os.environ)
inst_env.update(env)
inst_env.update({
    "NIK_PROFILE": env.get("NIK_PROFILE", "dev"),
    "NIK_DATABASE_NAME": DB,
    "NIK_SERVER_LISTEN": "127.0.0.1:%d" % PORT,
    "NIK_REDIS_URL": "redis://127.0.0.1:6379/9",
    "RUST_LOG": "info",
})
log_fh = open(log_path, "w")
# start_new_session=True 在子进程里直接 setsid()，不再套一层 `setsid` 命令
# （套了的话 poll() 看到的是包装进程，会误判「实例已退出」）
instance = subprocess.Popen([BIN], cwd=os.path.join(REPO, "backend"),
                            stdout=log_fh, stderr=subprocess.STDOUT,
                            env=inst_env, start_new_session=True)
deadline = time.time() + 180
healthy = False
while time.time() < deadline:
    try:
        code, _ = http("GET", "/health", timeout=3)
        if code == 200:
            healthy = True
            break
    except Exception:
        pass
    if instance.poll() is not None:
        say("实例已退出（rc=%s），日志见下：" % instance.returncode)
        say(open(log_path, encoding="utf-8", errors="replace").read()[-3000:])
        sys.exit(2)
    time.sleep(2)
say("实例日志：%s（healthy=%s, pid=%s, bin=%s）" % (log_path, healthy, instance.pid, BIN))
if not healthy:
    say(open(log_path, encoding="utf-8", errors="replace").read()[-3000:])
    sys.exit(2)
for line in open(log_path, encoding="utf-8", errors="replace"):
    if "数据库迁移完成" in line:
        say(line.rstrip())

say("")
say("========== 2. 登录 admin ==========")
# 首登强制改密会把口令换掉：先试 env 里的，再试本脚本上一轮改过的新口令
token = None
for password in [admin_password, "T9aE2e#2026"]:
    code, payload = http("POST", "/api/system/auth/login",
                         {"username": "admin", "password": password})
    if code == 200 and payload.get("data", {}).get("access_token"):
        token = payload["data"]["access_token"]
        if payload["data"].get("must_change_password"):
            say("admin 首登需要改密")
            new_password = "T9aE2e#2026"
            code2, _ = http("PUT", "/api/system/auth/password",
                            {"old_password": password, "new_password": new_password}, token=token)
            say("改密 -> HTTP %d" % code2)
            token = None
            continue
        break
    say("登录失败（口令候选尝试，HTTP %d）：%s" % (code, json.dumps(payload, ensure_ascii=False)[:160]))
if not token:
    say("FATAL: 拿不到令牌，无法继续")
    sys.exit(2)
code, me = http("GET", "/api/system/auth/me", token=token)
say("GET /api/system/auth/me -> HTTP %d, username=%s" % (code, me.get("data", {}).get("username")))

say("")
say("========== 3. 夹具（10 条任务，前缀 %s）==========" % FIXTURE_PREFIX)
admin_id = psql("select id from sys_user where username='admin' and deleted_at is null")
if admin_id.startswith("<ERR"):
    say("FATAL: 取 admin id 失败 %s" % admin_id)
    sys.exit(2)
say("admin_id = %s" % admin_id)


def cleanup_fixtures():
    psql("UPDATE biz_task SET parent_id = NULL WHERE task_no LIKE '%s%%'" % FIXTURE_PREFIX)
    psql("DELETE FROM biz_task WHERE task_no LIKE '%s%%'" % FIXTURE_PREFIX)


fixtures = [
    ("T9A-E2E-T1", "TODO", None),
    ("T9A-E2E-T2", "TODO", None),
    ("T9A-E2E-T3", "TODO", None),
    ("T9A-E2E-D1", "DOING", None),
    ("T9A-E2E-D2", "DOING", None),
    ("T9A-E2E-C1", "DOING", "T9A-E2E-T1"),
    ("T9A-E2E-N1", "DONE", None),
    ("T9A-E2E-N2", "DONE", None),
    ("T9A-E2E-R1", "REVIEW", None),
    ("T9A-E2E-X1", "CANCELLED", None),
]
cleanup_fixtures()
for task_no, status, parent_no in fixtures:
    parent = "null" if parent_no is None else "(select id from biz_task where task_no='%s')" % parent_no
    sql = (
        "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, status, "
        "priority, progress, need_review, created_at, updated_at) VALUES "
        "(gen_random_uuid(), '%s', '%s %s 夹具', '%s'::uuid, '%s'::uuid, %s, '%s', 'P2', 0, false, now(), now())"
        % (task_no, FIXTURE_PREFIX, task_no, admin_id, admin_id, parent, status)
    )
    out = psql(sql)
    if out.startswith("<ERR"):
        say("夹具写入失败 %s：%s" % (task_no, out))
say(psql(
    "select 'status_dist: ' || string_agg(status || '=' || n, ', ' order by status) "
    "from (select status, count(*) n from biz_task where task_no like 'T9A-E2E%' group by status) s"))
say("夹具总数 = %s" % psql("select count(*) from biz_task where task_no like 'T9A-E2E%'"))

say("")
say("========== 4. HTTP 断言 ==========")
KW = "keyword=" + FIXTURE_PREFIX


def page(query):
    code, payload = http("GET", "/api/business/task/page?page=1&page_size=50&" + query, token=token)
    data = payload.get("data") or {}
    return code, payload, data


def ids(data):
    return sorted(row["task_no"] if "task_no" in row else row.get("title", "")
                  for row in data.get("list", []))


def run_check(name, query, expect_total, expect_statuses=None, expect_http=200, expect_code=None):
    code, payload, data = page(query)
    detail = "HTTP %d total=%s" % (code, data.get("total"))
    ok = code == expect_http
    if expect_http == 200:
        ok = ok and data.get("total") == expect_total
        if expect_statuses is not None:
            got = {row["status"] for row in data.get("list", [])}
            ok = ok and got.issubset(set(expect_statuses))
            detail += " statuses=%s" % sorted(got)
    else:
        ok = ok and payload.get("code") == expect_code
        detail += " code=%s msg=%s" % (payload.get("code"), (payload.get("msg") or "")[:60])
    check(name, ok, detail)
    return data


# ① statuses 多选：3 TODO + 3 DOING（含 1 个 DOING 子任务）= 6
d_multi = run_check("① statuses=TODO,DOING → 6 条且只含这两种状态",
                    "%s&statuses=TODO,DOING" % KW, 6, ["TODO", "DOING"])
# ② 单值旧参数：3 TODO
d_single = run_check("② status=TODO（旧参数）→ 3 条", "%s&status=TODO" % KW, 3, ["TODO"])
# ③ statuses 单值 == 旧 status（同一行集合）
d_one = run_check("③ statuses=DONE 单值 → 2 条", "%s&statuses=DONE" % KW, 2, ["DONE"])
d_legacy = page("%s&status=DONE" % KW)[2]
check("③' statuses=DONE 与 status=DONE 行集合一致",
      sorted(r["id"] for r in d_one.get("list", [])) == sorted(r["id"] for r in d_legacy.get("list", [])),
      "statuses=%d 条 / status=%d 条" % (d_one.get("total", -1), d_legacy.get("total", -1)))
# ④ statuses 与 status 同时传 → statuses 优先（返回 3 条 TODO，而不是 2 条 DONE）
d_prio = page("%s&statuses=TODO&status=DONE" % KW)[2]
check("④ statuses=TODO&status=DONE → statuses 优先（3 条 TODO，不是 0 条）",
      d_prio.get("total") == 3 and {r["status"] for r in d_prio.get("list", [])} == {"TODO"},
      "HTTP 200 total=%s statuses=%s" % (d_prio.get("total"), sorted({r["status"] for r in d_prio.get("list", [])})))
# ⑤ 空字符串 = 不筛（10 条）
run_check("⑤ statuses=（空串）→ 等价不筛（10 条）", "%s&statuses=" % KW, 10)
# ⑥ 不传 = 不筛（10 条）
run_check("⑥ 不传 statuses/status → 10 条", KW, 10)
# ⑦ 非法值 → 400 + 10001（PARAM_INVALID）
run_check("⑦ statuses=TODO,FOO → 400 + 10001", "%s&statuses=TODO,FOO" % KW,
          0, None, expect_http=400, expect_code=10001)
# ⑧ 与 root_only 组合：5 条顶层（DOING 子任务被排除）
d_roots = run_check("⑧ statuses=TODO,DOING&root_only=true → 5 条顶层",
                    "%s&statuses=TODO,DOING&root_only=true" % KW, 5, ["TODO", "DOING"])
check("⑧' root_only 结果里没有子任务",
      all(row.get("parent_id") in (None, "") for row in d_roots.get("list", [])),
      "parent_id 全为空？")

say("")
say("========== 5. SQL 证据（PG 日志里是否真的走 IN (...)）==========")
# log_statement=all 已在步骤 0 打开，且实例是本次新起的（连接在读设置之后建立）
found = ""
for path in sorted(__import__("glob").glob("/var/log/postgresql/postgresql-*-main.log")):
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh.readlines()[-8000:]:
            if "status IN ($" in line:
                found = line.strip()
say("PG 日志：%s" % found[:400] if found else "PG 日志里没抓到 IN 子句")
check("⑨ PG 日志出现 `t.status IN ($n, $m)`（多选真的拼成 IN 子句）", bool(found),
      (found[:260] if found else "日志里没抓到"))

say("")
say("========== 6. 收尾 ==========")
cleanup_fixtures()
say("夹具残留 = %s（应为 0）" % psql("select count(*) from biz_task where task_no like 'T9A-E2E%'"))
subprocess.run(["su", "-", "postgres", "-c", "psql -At -c %s" % shell_quote(
    "ALTER DATABASE %s RESET log_statement" % DB)], capture_output=True, text=True)
say("已 RESET log_statement")
stop_instance()
say("已停隔离实例（端口 %d）" % PORT)

say("")
say("========== 汇总 ==========")
for name, ok, detail in CHECKS:
    say(("[PASS] " if ok else "[FAIL] ") + name)
total = len(CHECKS)
say("共 %d 项，通过 %d 项，失败 %d 项" % (total, total - len(FAILS), len(FAILS)))
sys.exit(1 if FAILS else 0)
PY
