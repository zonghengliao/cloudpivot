#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P2-5 迁移往返验收：V0219 up → down → up（有真实调拨日志在做证据）。

验的是「down 脚本能把值改回去、再改回来不丢东西」：

```
① 造一张真实调拨单（DRAFT）+ 一条 entity_type='STOCK_TRANSFER' 的状态日志
② 跑 V0219 down.sql  → 该日志必须变回 'TRANSFER'
③ 跑 V0219 up.sql    → 该日志必须变回 'STOCK_TRANSFER'
④ 词表巡检仍然 PASS（域里没有 TRANSFER）
```

用法：
    P25_DB_NAME=cp_p25_vocab P25_DB_USER=alun P25_DB_PASSWORD=alun \\
      python3 docs/evidence/20261104-p2-5-biz-type/migration_roundtrip.py

退出码：0 = 全绿；1 = 有断言失败；2 = 环境不满足。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P25_BASE_URL", "http://127.0.0.1:18145").rstrip("/")
DB_NAME = os.environ.get("P25_DB_NAME", "cp_p25_vocab")
DB_USER = os.environ.get("P25_DB_USER", "alun")
DB_PASS = os.environ.get("P25_DB_PASSWORD", "alun")
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

ADMIN_USER = os.environ.get("P25_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("P25_ADMIN_PASSWORD", "Admin@123456")
ADMIN_NEW_PASS = os.environ.get("P25_ADMIN_NEW_PASSWORD", "P25Admin#2026x")
MAKER_USER = "p25maker"
MAKER_PASS = "P25Maker@12345"

UNIT_ID = "b2e2e001-0000-4000-8000-0000000000a5"
WH_FROM = "b2e2e003-0000-4000-8000-0000000000a5"
WH_TO = "b2e2e005-0000-4000-8000-0000000000a5"
MATERIAL_ID = "b2e2e004-0000-4000-8000-0000000000a5"

UP = os.path.join(ROOT, "backend/migrations/business/V0219__erp_p2_5_biz_type_vocabulary.up.sql")
DOWN = os.path.join(ROOT, "backend/migrations/business/V0219__erp_p2_5_biz_type_vocabulary.down.sql")

PASSED: list[str] = []
FAILED: list[str] = []


def check(ok: bool, name: str, detail: str = "") -> bool:
    if ok:
        PASSED.append(name)
        print(f"[PASS] {name}")
    else:
        FAILED.append(name)
        print(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))
    return ok


def psql(sql: str) -> str:
    args = ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", DB_NAME,
            "-X", "-q", "-v", "ON_ERROR_STOP=1", "-tAc", sql]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB_PASS), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return out.stdout.strip()


def run_sql_file(path: str) -> None:
    args = ["psql", "-h", "127.0.0.1", "-p", "5432", "-U", DB_USER, "-d", DB_NAME,
            "-X", "-q", "-v", "ON_ERROR_STOP=1", "-f", path]
    out = subprocess.run(args, capture_output=True, text=True,
                         env=dict(os.environ, PGPASSWORD=DB_PASS), check=False)
    if out.returncode != 0:
        raise RuntimeError(f"跑 {os.path.basename(path)} 失败：{out.stderr.strip()}")
    for line in (out.stdout or "").splitlines():
        if "V0219" in line:
            print(f"  · {line.strip()}")


def call(method: str, path: str, body: dict | None = None, token: str | None = None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(user: str, password: str, new_password: str | None = None) -> str:
    status, body = call("POST", "/api/system/auth/login", {"username": user, "password": password})
    if status != 200 and new_password:
        status, body = call("POST", "/api/system/auth/login",
                            {"username": user, "password": new_password})
    if status != 200:
        raise SystemExit(f"[env] 登录 {user} 失败：{status} {body}")
    return (body.get("data") or {}).get("access_token") or ""


def main() -> int:
    maker = login(MAKER_USER, MAKER_PASS)

    # ---------- ① 造真实调拨单 + 一条调拨状态日志 ----------
    psql(f"""
        INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
        VALUES ('{WH_TO}', 'P25E2E-WH2', 'P2-5 真机仓二号', '/{WH_TO}/', 1, 1, 1)
        ON CONFLICT DO NOTHING;
    """)
    st, body = call("POST", "/api/business/erp/transfer", {
        "from_warehouse_id": WH_FROM,
        "to_warehouse_id": WH_TO,
        "reason": "P2-5 迁移往返验收",
        "items": [{"material_id": MATERIAL_ID, "qty": "2"}],
    }, maker)
    transfer = ((body.get("data") or {}).get("header") or (body.get("data") or {}))
    transfer_id = transfer.get("id")
    if st != 200 or not transfer_id:
        print(f"[env] 建调拨单失败：{st} {str(body)[:300]}")
        return 2
    print(f"[info] 调拨单 {transfer.get('transfer_no')} id={transfer_id}")

    # 真实业务里 DRAFT 不会写状态日志，这里补一条「过账/发运」形态的日志做迁移证据
    psql(f"""
        INSERT INTO biz_status_log (id, entity_type, entity_id, entity_no, from_status, to_status, reason, created_at)
        VALUES (gen_random_uuid(), 'STOCK_TRANSFER', '{transfer_id}'::uuid,
                '{transfer.get("transfer_no")}', 'DRAFT', 'IN_TRANSIT', 'P2-5 迁移往返验收', now())
    """)
    before = psql(f"SELECT string_agg(DISTINCT entity_type, ',') FROM biz_status_log "
                  f"WHERE entity_id='{transfer_id}'::uuid")
    check(before == "STOCK_TRANSFER", "① 迁移前调拨日志 = STOCK_TRANSFER", before)

    # ---------- ② down ----------
    print("[info] 执行 V0219 down.sql")
    run_sql_file(DOWN)
    after_down = psql(f"SELECT string_agg(DISTINCT entity_type, ',') FROM biz_status_log "
                      f"WHERE entity_id='{transfer_id}'::uuid")
    check(after_down == "TRANSFER", "② down 之后回到旧值 TRANSFER", after_down)

    # ---------- ③ up ----------
    print("[info] 执行 V0219 up.sql")
    run_sql_file(UP)
    after_up = psql(f"SELECT string_agg(DISTINCT entity_type, ',') FROM biz_status_log "
                    f"WHERE entity_id='{transfer_id}'::uuid")
    check(after_up == "STOCK_TRANSFER", "③ up 之后再次统一为 STOCK_TRANSFER", after_up)

    # ---------- ④ 巡检 ----------
    run = subprocess.run([sys.executable, "scripts/verify/biz_type_domain.py"],
                         capture_output=True, text=True, cwd=ROOT,
                         env=dict(os.environ, NIK_DATABASE_HOST="127.0.0.1",
                                  NIK_DATABASE_USER=DB_USER, NIK_DATABASE_PASSWORD=DB_PASS,
                                  NIK_DATABASE_NAME=DB_NAME))
    tail = "\n".join(run.stdout.strip().splitlines()[-4:])
    check(run.returncode == 0, "④ 往返之后词表巡检仍 PASS", tail)
    print(tail)

    print("\n" + "=" * 60)
    print(f"结果：{len(PASSED)} 通过 / {len(FAILED)} 失败")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
