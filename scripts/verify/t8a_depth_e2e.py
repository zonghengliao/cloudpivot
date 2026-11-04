#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""T8-A 真机验收脚本：任务父子深度 2 → 8（HTTP 层 + 库内递归 CTE 复核）。

# 用法

```bash
# 1) 准备一个隔离库（owner 必须给应用账号，否则启动只打一行「迁移失败: query failed」）
su - postgres -c 'createdb -O cloudpivot cloudpivot_t8a'

# 2) 用该库起一次隔离实例（迁移会自动跑完；口令注入 NIK_ADMIN_PASSWORD，否则会随机生成）
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
export NIK_PROFILE=test \
       NIK_DATABASE_NAME=cloudpivot_t8a \
       NIK_SERVER_LISTEN=127.0.0.1:18089 \
       NIK_REDIS_URL=redis://127.0.0.1:6379/9 \
       NIK_ADMIN_PASSWORD='T8aAdmin@2026'
cd backend && setsid nohup ./target/debug/cloudpivot > /tmp/t8a_server.log 2>&1 < /dev/null &

# 3) 跑验收
export T8A_BASE_URL=http://127.0.0.1:18089 T8A_DB=cloudpivot_t8a
export T8A_PG_HOST=127.0.0.1 T8A_PG_PORT=5432 T8A_PG_USER=cloudpivot T8A_PG_PASSWORD=<口令>
python3 scripts/verify/t8a_depth_e2e.py
```

# 覆盖（15 项断言）

- 建第 1 层（`POST /business/task`）→ 200；
- 逐层建第 2..8 层（`POST /business/task/{id}/subtask`）→ 全部 200（第 3 层是本卡核心，以前被 63005 拒）；
- 第 9 层 → **HTTP 400 + 63005**，文案含「最多 8 层」「第 8 层」；
- 自指（`PUT parent_id = 自己`）→ 400 + 63005；
- 成环（把顶层挂到自己的孙子下面）→ 400 + 63005；
- 库内递归 CTE 复核：最深一级祖先链长 = 8、顶层链长 = 1；
- 夹具清理（标题前缀 `T8A-E2E`）结束后残留 0 行。

幂等：每次开头按标题前缀先清一遍，可反复跑；只删自己造的行。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("T8A_BASE_URL", "http://127.0.0.1:18089")
DB = os.environ.get("T8A_DB", "cloudpivot_t8a")
PREFIX = "T8A-E2E"
PW_FILE = os.environ.get("T8A_ADMIN_PW_FILE", "/tmp/t8a_admin_pw")
NEW_ADMIN_PW = os.environ.get("T8A_ADMIN_NEW_PW", "T8aAdmin@2026")

results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print("[%s] %s%s" % ("PASS" if ok else "FAIL", name, ("  << " + detail) if detail else ""))


def api(path):
    return BASE + "/api" + path


def http(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(api(path), data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        raw = err.read().decode("utf-8")
        try:
            return err.code, json.loads(raw)
        except ValueError:
            return err.code, {"raw": raw}


def pg_env(name, fallback_name, default=""):
    for key in (name, fallback_name):
        value = os.environ.get(key)
        if value:
            return value
    return default


def psql(sql):
    password = pg_env("T8A_PG_PASSWORD", "NIK_DATABASE_PASSWORD") or pg_env(
        "T8A_PG_PASSWORD", "ALUN_DATABASE_PASSWORD"
    )
    env = dict(os.environ)
    env["PGPASSWORD"] = password
    cmd = [
        "psql",
        "-h", pg_env("T8A_PG_HOST", "NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", pg_env("T8A_PG_PORT", "NIK_DATABASE_PORT", "5432"),
        "-U", pg_env("T8A_PG_USER", "NIK_DATABASE_USER", "cloudpivot"),
        "-d", DB,
        "-Atc", sql,
    ]
    out = subprocess.run(cmd, capture_output=True, text=True, env=env)
    if out.returncode != 0:
        raise RuntimeError("psql 失败: " + out.stderr.strip())
    return out.stdout.strip()


def login():
    """登录；首登强制改密则改密后重登（新口令缓存进文件，重跑复用）。"""
    candidates = []
    if os.path.exists(PW_FILE):
        cached = open(PW_FILE).read().strip()
        if cached:
            candidates.append(cached)
    for key in ("T8A_ADMIN_PASSWORD", "NIK_ADMIN_PASSWORD", "ALUN_ADMIN_PASSWORD"):
        value = os.environ.get(key)
        if value:
            candidates.append(value)
    if not candidates:
        raise RuntimeError("没有可用口令：请设置 T8A_ADMIN_PASSWORD（或写 %s）" % PW_FILE)
    for password in candidates:
        status, payload = http("POST", "/system/auth/login", {"username": "admin", "password": password})
        if status != 200 or payload.get("code") != 0:
            print("   登录尝试返回 HTTP %s / code %s" % (status, payload.get("code")))
            continue
        data = payload.get("data") or {}
        token = data.get("access_token") or data.get("token")
        if not data.get("must_change_password"):
            open(PW_FILE, "w").write(password)
            return token
        status, payload = http(
            "PUT", "/system/auth/password",
            {"old_password": password, "new_password": NEW_ADMIN_PW}, token,
        )
        if status != 200:
            raise RuntimeError("首登改密失败 HTTP %s %s" % (status, payload))
        open(PW_FILE, "w").write(NEW_ADMIN_PW)
        status, payload = http("POST", "/system/auth/login", {"username": "admin", "password": NEW_ADMIN_PW})
        if status != 200 or payload.get("code") != 0:
            raise RuntimeError("新口令重登失败 HTTP %s %s" % (status, payload))
        return payload["data"].get("access_token") or payload["data"].get("token")
    raise RuntimeError("登录失败：所有口令都不通")


def cleanup():
    """只删自己造的行（标题前缀 T8A-E2E），先断父指针再删（自引用 FK 不是 deferrable）。"""
    psql("UPDATE biz_task SET parent_id = NULL WHERE title LIKE '%s%%' AND parent_id IS NOT NULL" % PREFIX)
    psql("DELETE FROM biz_task WHERE title LIKE '%s%%'" % PREFIX)


def chain_len(task_id):
    return psql(
        "WITH RECURSIVE up(id, parent_id, d) AS ("
        "  SELECT id, parent_id, 1 FROM biz_task WHERE id = '%s'::uuid"
        "  UNION ALL"
        "  SELECT p.id, p.parent_id, up.d + 1 FROM biz_task p JOIN up ON p.id = up.parent_id"
        ") SELECT max(d) FROM up;" % task_id
    )


def main():
    print("=== T8-A 父子深度 8 层验收 @ %s（库 %s）===" % (BASE, DB))
    cleanup()
    token = login()
    print("   登录成功")

    status, payload = http("POST", "/business/task", {"title": PREFIX + "-L1"}, token)
    ok = status == 200 and payload.get("code") == 0
    check("① 建第 1 层（parent_id = null）→ 200", ok, "HTTP %s %s" % (status, payload.get("msg")))
    if not ok:
        raise SystemExit("第 1 层都建不出来，后面的不用测了")
    chain = [payload["data"]["id"]]
    print("   L1 id=%s task_no=%s" % (chain[0], payload["data"]["task_no"]))

    for level in range(2, 9):
        status, payload = http(
            "POST", "/business/task/%s/subtask" % chain[-1],
            {"title": "%s-L%d" % (PREFIX, level)}, token,
        )
        ok = status == 200 and payload.get("code") == 0
        detail = "HTTP %s code=%s msg=%s" % (status, payload.get("code"), payload.get("msg"))
        check("② 建第 %d 层 → 200" % level, ok, detail if not ok else "")
        if not ok:
            raise SystemExit("第 %d 层被拒，后面不用测了" % level)
        chain.append(payload["data"]["id"])
        print("   L%d id=%s task_no=%s" % (level, chain[-1], payload["data"]["task_no"]))

    status, payload = http(
        "POST", "/business/task/%s/subtask" % chain[-1], {"title": PREFIX + "-L9"}, token
    )
    message = str(payload.get("msg") or payload.get("error") or payload)
    check("③ 建第 9 层 → HTTP 400 + 63005",
          status == 400 and payload.get("code") == 63005,
          "HTTP %s code=%s msg=%s" % (status, payload.get("code"), message))
    check("③' 文案含「最多 8 层」「第 8 层」",
          "最多 8 层" in message and "第 8 层" in message, message)

    status, payload = http("PUT", "/business/task/%s" % chain[0], {"parent_id": chain[0]}, token)
    check("④ 自指（parent_id = 自己）→ 400 + 63005",
          status == 400 and payload.get("code") == 63005,
          "HTTP %s code=%s msg=%s" % (status, payload.get("code"), payload.get("msg")))

    status, payload = http("PUT", "/business/task/%s" % chain[0], {"parent_id": chain[2]}, token)
    check("⑤ 成环（L1 变成 L3 的子任务）→ 400 + 63005",
          status == 400 and payload.get("code") == 63005,
          "HTTP %s code=%s msg=%s" % (status, payload.get("code"), payload.get("msg")))

    depth = chain_len(chain[-1])
    check("⑥ 库里最深一级的祖先链长 = 8", depth == "8", "psql max(d)=%s" % depth)
    depth1 = chain_len(chain[0])
    check("⑦ 顶层任务的链长 = 1", depth1 == "1", "psql max(d)=%s" % depth1)

    cleanup()
    left = psql("SELECT count(*) FROM biz_task WHERE title LIKE '%s%%'" % PREFIX)
    check("⑧ 夹具清理干净（残留 0）", left == "0", "残留 %s 行" % left)

    failed = [item for item in results if not item[1]]
    print("\n=== 汇总：%d 项通过 / %d 项失败 ===" % (len(results) - len(failed), len(failed)))
    if failed:
        for name, _, detail in failed:
            print("  FAIL: %s  %s" % (name, detail))
        sys.exit(1)


if __name__ == "__main__":
    main()
