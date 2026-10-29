"""设计修正 A · 修正 4「任务附件 storage 策略」验收脚本（真迁移 + 真机 HTTP，可重复执行）。

用法（在 144 上执行；实例监听 127.0.0.1:8026、库 cloudpivot_fixa_v107）：

    python3 scripts/verify/fixa_storage_policy.py migrate   # V0107 空库 up/down/up + 自检负例
    python3 scripts/verify/fixa_storage_policy.py upload    # 5 条策略实测（.sh/.md/51MB/49MB/.exe）
    python3 scripts/verify/fixa_storage_policy.py all

设计说明
--------
* **迁移验证走 psql 直接跑 up/down 文件**，不走 `Migrator::rollback`：种子迁移
  （`S__*.sql`）是 rerunnable 且没有 down 脚本，rollback 会被它卡住（见 skill cailu-dev §9.14-1）。
  「空库」由 `run_instance.sh` 起的隔离实例首次启动时把整条迁移链跑完来提供。
* **上传走真实 multipart HTTP**，不是单测：策略是「数据 + 代码」两层（V0107 白名单 +
  `FORBIDDEN_EXECUTABLE_EXTENSIONS` 黑名单），只有真跑一遍才能证明两层都在生效。
* 每条断言打印「期望 vs 实际」，输出即为 `docs/批次设计修正A-验证证据.md` 的原始证据。
"""

import json
import os
import subprocess as sp
import sys

import requests

BASE = "http://127.0.0.1:8026/api"
DB = "cloudpivot_fixa_v107"
ENVF = "/etc/cloudpivot/cloudpivot-test.env"
MIG_DIR = "/var/tmp/designA/backend/migrations/business"
UP = MIG_DIR + "/V0107__storage_task_policies.up.sql"
DOWN = MIG_DIR + "/V0107__storage_task_policies.down.sql"

PASS = 0
FAIL = 0
INITIAL_ADMIN_PASSWORD = None
ADMIN_PASSWORD = None


# ==================== 基础设施 ====================


def env(key):
    for line in open(ENVF, encoding="utf-8"):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("缺少环境变量 " + key)


def _init_passwords():
    global INITIAL_ADMIN_PASSWORD, ADMIN_PASSWORD
    if INITIAL_ADMIN_PASSWORD is None:
        INITIAL_ADMIN_PASSWORD = env("ALUN_ADMIN_PASSWORD")
        ADMIN_PASSWORD = INITIAL_ADMIN_PASSWORD + "-fixa"


def psql(sql):
    """执行 SQL，返回二维数组（-At -F '|'）。allow_fail=True 时用 run_sql。"""
    result = sp.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")},
        check=True,
    )
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def psql_file(path):
    """跑一个 .sql 文件；返回 (returncode, stdout, stderr)。失败**不抛异常**（负例要用）。"""
    result = sp.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-v", "ON_ERROR_STOP=1", "-f", path],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": env("ALUN_DATABASE_PASSWORD")},
    )
    return result.returncode, result.stdout, result.stderr


def check(label, ok, expected, actual):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS  {label}\n        期望={expected}\n        实际={actual}")
    else:
        FAIL += 1
        print(f"  FAIL  {label}\n        期望={expected}\n        实际={actual}")


def section(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


def call(method, path, token=None, body=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    response = requests.request(method, BASE + path, headers=headers, json=body, timeout=30)
    try:
        return response.status_code, response.json()
    except json.JSONDecodeError:
        return response.status_code, {"raw": response.text[:200]}


def login(username, password):
    status, body = call("POST", "/system/auth/login", body={"username": username, "password": password})
    if status == 200 and body.get("code") == 0:
        return body["data"]["access_token"]
    raise SystemExit(f"登录失败 {username}: {status} {body}")


def admin_token():
    """取 admin token；空库首登必须改密（session_guard 拦所有其它接口），这里自动完成。"""
    _init_passwords()
    status, body = call("POST", "/system/auth/login",
                        body={"username": "admin", "password": INITIAL_ADMIN_PASSWORD})
    if status == 200 and body.get("code") == 0:
        token = body["data"]["access_token"]
        status, changed = call("PUT", "/system/auth/password", token,
                               {"old_password": INITIAL_ADMIN_PASSWORD, "new_password": ADMIN_PASSWORD})
        print(f"[admin] 首登改密 → HTTP {status} code={changed.get('code')} msg={changed.get('msg')}")
    else:
        print("[admin] 初始口令已失效（此前已改密过），直接用改密后的口令登录")
    return login("admin", ADMIN_PASSWORD)


def policy_rows():
    return psql(
        "SELECT category, max_size_mb, allowed_extensions, direct_upload, sort_order "
        "FROM sys_storage_policy WHERE deleted_at IS NULL ORDER BY sort_order"
    )


# ==================== 1. 迁移：空库 up / down / up ====================


def verify_migrate():
    section("1. V0107 迁移：空库（整链已跑到 V0107）up / down / up")

    rows = policy_rows()
    cats = [r[0] for r in rows]
    print("  sys_storage_policy 当前行：")
    for r in rows:
        print(f"    {r[0]:<14} {r[1]:>4} MB  direct_upload={r[3]:<5} sort={r[4]}  ext={r[2]}")
    check("策略总条数 = 8（原 6 + 本卡 2）", len(rows) == 8, 8, len(rows))
    check("含 task 与 task_ai", "task" in cats and "task_ai" in cats, True, (cats.count("task"), cats.count("task_ai")))

    task = next((r for r in rows if r[0] == "task"), None)
    task_ai = next((r for r in rows if r[0] == "task_ai"), None)
    check("task.max_size_mb = 50", task is not None and task[1] == "50", "50", task and task[1])
    check("task_ai.max_size_mb = 100", task_ai is not None and task_ai[1] == "100", "100", task_ai and task_ai[1])
    check(
        "task 允许扩展名与设计逐字一致",
        task is not None
        and task[2]
        == "jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar",
        "jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar",
        task and task[2],
    )
    check(
        "task_ai 允许扩展名与设计逐字一致",
        task_ai is not None and task_ai[2] == "jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log",
        "jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log",
        task_ai and task_ai[2],
    )
    check("task 开启浏览器直传", task is not None and task[3] == "t", "t", task and task[3])
    check("task_ai 关闭浏览器直传（AI 产出必过服务端）", task_ai is not None and task_ai[3] == "f", "f", task_ai and task_ai[3])

    # ---- down ----
    rc, out, err = psql_file(DOWN)
    after_down = policy_rows()
    check("down 执行成功", rc == 0, 0, rc if rc == 0 else err.strip()[:200])
    check("down 后策略 8 → 6 条", len(after_down) == 6, 6, len(after_down))
    check(
        "down 后 task / task_ai 均不存在",
        all(r[0] not in ("task", "task_ai") for r in after_down),
        "无 task/task_ai",
        [r[0] for r in after_down if r[0] in ("task", "task_ai")] or "无",
    )

    # ---- up（跑两次，验 ON CONFLICT 幂等）----
    rc1, out1, err1 = psql_file(UP)
    check("down 后重新 up 成功", rc1 == 0, 0, rc1 if rc1 == 0 else err1.strip()[:200])
    once = policy_rows()
    check("up 后策略 6 → 8 条", len(once) == 8, 8, len(once))
    rc2, out2, err2 = psql_file(UP)
    twice = policy_rows()
    check("重复 up 不报错（ON CONFLICT 幂等）", rc2 == 0, 0, rc2 if rc2 == 0 else err2.strip()[:200])
    check("重复 up 不产生重复行（仍 8 条）", len(twice) == 8, 8, len(twice))
    notice = [
        line
        for line in (out1 + out2 + err1 + err2).splitlines()
        if "V0107 自检通过" in line
    ]
    check("迁移自检块打出通过 NOTICE", bool(notice), "含『V0107 自检通过』", notice[:1] or "无")

    # ---- 负例：把 sh 塞进 task_ai 白名单 → 重放 up 必须被自检拦住 ----
    psql(
        "UPDATE sys_storage_policy SET allowed_extensions = allowed_extensions || ',sh' "
        "WHERE category = 'task_ai' AND deleted_at IS NULL"
    )
    dirty = next(r for r in policy_rows() if r[0] == "task_ai")[2]
    rc3, out3, err3 = psql_file(UP)
    check(
        "白名单被污染后重放 up → 迁移自检报错（不静默通过）",
        rc3 != 0 and "硬禁止" in err3,
        "非 0 退出 + 错误含『硬禁止』",
        f"rc={rc3} err={err3.strip().splitlines()[-1] if err3.strip() else '无'}",
    )
    print(f"        被污染的白名单：{dirty}")
    # 恢复
    psql(
        "UPDATE sys_storage_policy SET allowed_extensions = "
        "'jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log' "
        "WHERE category = 'task_ai' AND deleted_at IS NULL"
    )
    restored = next(r for r in policy_rows() if r[0] == "task_ai")[2]
    check(
        "恢复后白名单回到设计值（8 条策略不变）",
        restored == "jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log" and len(policy_rows()) == 8,
        "设计值 + 8 条",
        f"{restored} / {len(policy_rows())} 条",
    )


# ==================== 2. 上传实测 ====================


def upload(token, category, filename, content, mime="application/octet-stream"):
    response = requests.post(
        BASE + "/business/storage/upload",
        params={"category": category},
        headers={"Authorization": "Bearer " + token},
        files={"file": (filename, content, mime)},
        timeout=180,
    )
    try:
        return response.status_code, response.json()
    except json.JSONDecodeError:
        return response.status_code, {"raw": response.text[:200]}


def verify_upload():
    section("2. 任务附件上传实测（真机 HTTP multipart）")
    token = admin_token()

    # 2.1 .sh 到 task_ai → 必须拒绝（400 / 60004）
    status, body = upload(token, "task_ai", "deploy.sh", b"#!/bin/sh\necho pwn\n", "text/x-shellscript")
    check(
        ".sh 上传 task_ai → 拒绝 400 / 60004",
        status == 400 and body.get("code") == 60004,
        "400 / code=60004",
        f"{status} / code={body.get('code')} msg={body.get('msg')}",
    )

    # 2.2 .py / .js 到 task_ai → 同样拒绝（AI 产出追加的两个禁止项）
    for name in ("train.py", "run.js"):
        status, body = upload(token, "task_ai", name, b"print(1)\n", "text/plain")
        check(
            f"{name} 上传 task_ai → 拒绝 400 / 60004",
            status == 400 and body.get("code") == 60004,
            "400 / code=60004",
            f"{status} / code={body.get('code')}",
        )

    # 2.3 .md 到 task_ai → 通过
    md = "# AI 产出\n\n这一步的结论：**策略生效**。\n".encode()
    status, body = upload(token, "task_ai", "ai-report.md", md, "text/markdown")
    file_id = (body.get("data") or {}).get("file_id") or (body.get("data") or {}).get("id")
    check(
        ".md 上传 task_ai → 通过 200 / code=0 且有 file_id",
        status == 200 and body.get("code") == 0 and bool(file_id),
        "200 / code=0 / file_id 非空",
        f"{status} / code={body.get('code')} file_id={file_id}",
    )
    if file_id:
        row = psql(f"SELECT category, file_size, original_name FROM sys_file WHERE id = '{file_id}'")
        check(
            "sys_file 落库分类 = task_ai",
            row and row[0][0] == "task_ai",
            "task_ai",
            row[0] if row else "无记录",
        )
        # 签名/下载链路：本地后端走代理下载（302 只对 S3 预签名成立）
        got = requests.get(
            BASE + f"/business/storage/download/{file_id}",
            headers={"Authorization": "Bearer " + token},
            timeout=30,
        )
        check(
            "task_ai 文件可下载且内容一致",
            got.status_code == 200 and got.content == md,
            f"200 + {len(md)} 字节相同内容",
            f"{got.status_code} + {len(got.content)} 字节",
        )
        print(f"        file_id={file_id}  download_url={BASE}/business/storage/download/{file_id}")

    # 2.4 .exe 到 task → 拒绝（人类上传的硬禁止）
    status, body = upload(token, "task", "tool.exe", b"MZ\x90\x00", "application/octet-stream")
    check(
        ".exe 上传 task → 拒绝 400 / 60004",
        status == 400 and body.get("code") == 60004,
        "400 / code=60004",
        f"{status} / code={body.get('code')} msg={body.get('msg')}",
    )

    # 2.5 51 MB 到 task → 拒绝（422 / 60003）
    big = b"0" * (51 * 1024 * 1024)
    status, body = upload(token, "task", "big.zip", big, "application/zip")
    check(
        "51 MB 上传 task → 拒绝 422 / 60003",
        status == 422 and body.get("code") == 60003,
        "422 / code=60003",
        f"{status} / code={body.get('code')} msg={body.get('msg')}",
    )

    # 2.6 49 MB 到 task → 通过
    mid = b"1" * (49 * 1024 * 1024)
    status, body = upload(token, "task", "mid.zip", mid, "application/zip")
    mid_id = (body.get("data") or {}).get("file_id") or (body.get("data") or {}).get("id")
    check(
        "49 MB 上传 task → 通过 200 / code=0",
        status == 200 and body.get("code") == 0 and bool(mid_id),
        "200 / code=0 / file_id 非空",
        f"{status} / code={body.get('code')} file_id={mid_id}",
    )
    if mid_id:
        row = psql(f"SELECT category, file_size FROM sys_file WHERE id = '{mid_id}'")
        check(
            "49 MB 文件落库 category=task 且 size 一致",
            row and row[0][0] == "task" and row[0][1] == str(len(mid)),
            f"task / {len(mid)}",
            row[0] if row else "无记录",
        )

    section("2.7 策略表最终状态（应为 8 条）")
    rows = policy_rows()
    print(f"  {'category':<14} {'max_mb':>6}  direct  sort  allowed_extensions")
    for r in rows:
        print(f"  {r[0]:<14} {r[1]:>6}  {r[3]:<6}  {r[4]:>4}  {r[2]}")
    check("策略总数 = 8", len(rows) == 8, 8, len(rows))


# ==================== 入口 ====================


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "all"
    if mode in ("migrate", "all"):
        verify_migrate()
    if mode in ("upload", "all"):
        verify_upload()
    section(f"汇总：PASS={PASS}  FAIL={FAIL}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
