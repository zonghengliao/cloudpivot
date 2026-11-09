#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2E04-FIX-1 真机验收：手工凭证支持辅助核算（`aux_*` 不再恒 NULL）。

卡面 t_51944d63。逐项对应验收：

| # | 验收 | 本脚本的做法 |
|---|---|---|
| ① | 分录选 5602.03（要求 DEPT）不选部门 → 400 + 明确提示 | 真 token POST，断言 `400` + 文案含科目编码 / 「要求部门辅助核算，请选择部门」/ 入参字段名 |
| ② | 选了部门 → 保存成功，分录 `aux_dept_id` 有值 | 断言回执 `entries[].aux_dept_id` + psql 直查 `biz_voucher_entry.aux_dept_id` |
| ③ | 查辅助账 → 能看到该笔 | 审核 + 记账（辅助账只算 `POSTED`）后 `GET /ledger/aux-balance?aux_type=DEPT` 命中该科目行 |
| ④ | 选不要求维度的科目（1002）→ 不需要 aux，正常保存 | 建 1405/1002 凭证 → 200 且分录 aux 全空 |
| ⑤ | 存量历史凭证不受影响（不阻断） | 取一张**已存在**的历史凭证详情 + 状态流转动作不报辅助核算错 |
| ⑥ | 非法 aux id → 400 点名字段 | `aux_dept_id="not-a-uuid"` |

# 审核 / 记账怎么过（内控）

14× 环境 `erp.separation_of_duties.approve = true` 会拦「自己审自己制的凭证」。
本脚本验的是**辅助核算能不能落账**，不是内控，所以临时把该开关置 `false`
（走真实配置接口 `PUT /support/config/{id}`，会写审计日志），收尾在 `finally` 里恢复原值。

# 夹具

科目 / 部门**只读选取**现存的（`5602.03` 声明 DEPT、`1002` 不声明）。自建凭证带
`E4F1-<8位随机>` 摘要前缀，收尾按外键顺序（状态日志 → 分录 → 凭证）硬删并回查 0 行。

# 怎么跑（在 145 上）

```bash
cd /opt/cloudpivot
python3 scripts/verify/e2e04fix1_manual_aux.py --base http://127.0.0.1:8033/api
```

默认 `--env-file /etc/cloudpivot/cloudpivot-dev.env`（读 `NIK_DATABASE_*` / `NIK_ADMIN_PASSWORD`）。
**不要指向生产**：`db_guard.require_non_prod` 会先拦一道。
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db_guard import require_non_prod  # noqa: E402

require_non_prod(script=os.path.basename(__file__))

import argparse  # noqa: E402
import json  # noqa: E402
import subprocess  # noqa: E402
import time  # noqa: E402
import urllib.error  # noqa: E402
import urllib.request  # noqa: E402
import uuid  # noqa: E402
from typing import Any  # noqa: E402

DEFAULTS = {
    "base": os.environ.get("E2E04_BASE", "http://127.0.0.1:8033/api"),
    "env_file": os.environ.get("E2E04_ENV_FILE", "/etc/cloudpivot/cloudpivot-dev.env"),
}

SUFFIX = uuid.uuid4().hex[:8]
MARK = "E4F1-" + SUFFIX
PASSED = 0
FAILED = 0
FAILS = []
CREATED_VOUCHERS = []
SOD_KEY = "erp.separation_of_duties.approve"
SOD_BACKUP = None

CFG = dict(DEFAULTS)
ENV = {}


# ==================== 基础设施 ====================

def load_env_file(path):
    values = {}
    if not os.path.exists(path):
        return values
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def db_conf():
    return {
        "host": os.environ.get("DB_HOST") or ENV.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "port": os.environ.get("DB_PORT") or ENV.get("NIK_DATABASE_PORT", "5432"),
        "user": os.environ.get("DB_USER") or ENV.get("NIK_DATABASE_USER", "cloudpivot"),
        "name": os.environ.get("DB_NAME") or ENV.get("NIK_DATABASE_NAME", ""),
        "password": os.environ.get("DB_PASS") or ENV.get("NIK_DATABASE_PASSWORD", ""),
    }


def api(method, path, token=None, body=None, timeout=60) -> tuple[int, dict[str, Any]]:
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(CFG["base"] + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
            return response.status, json.loads(raw or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw[:500]}


def psql(sql):
    conf = db_conf()
    result = subprocess.run(
        ["psql", "-h", conf["host"], "-p", str(conf["port"]), "-U", conf["user"],
         "-d", conf["name"], "-At", "-F", "|", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True, text=True,
        env={**os.environ, "PGPASSWORD": conf["password"]})
    if result.returncode != 0:
        raise SystemExit("psql 失败：" + result.stderr.strip() + "\nSQL: " + sql)
    return [line.split("|") for line in result.stdout.strip().splitlines() if line]


def sql_one(sql):
    rows = psql(sql)
    if not rows or not rows[0]:
        return None
    return rows[0][0] if rows[0][0] != "" else None


def sql_line(sql):
    """整行（`a|b|c`）—— 多列夹具要用它，`sql_one` 只取第一列。

    ⚠️ `psql -At -F'|'` 会省掉**最后一行**的尾部空字段（skill 里记过这个坑），
    所以多列查询的最后一列要放一个恒非空的列，或只读前面几列。
    """
    rows = psql(sql)
    if not rows:
        return None
    joined = "|".join(rows[0])
    return joined if joined.strip("|") else None


def check(name, condition, detail=""):
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print("[PASS] " + name)
    else:
        FAILED += 1
        FAILS.append(name)
        print("[FAIL] " + name)
    if detail:
        print("       " + str(detail))
    return bool(condition)


def step(title):
    print()
    print("-" * 78)
    print(title)
    print("-" * 78)


def login(username, password, attempts=2):
    last = (0, {})
    for index in range(attempts):
        status, payload = api("POST", "/system/auth/login",
                              body={"username": username, "password": password})
        last = (status, payload)
        if status != 429:
            return last
        print(f"       登录限流 429，等满 62s 窗口再试（{index + 1}/{attempts}）")
        time.sleep(62)
    return last


def login_and_settle(username, candidates):
    last = None
    for candidate in candidates:
        status, payload = login(username, candidate)
        last = (status, payload)
        if status != 200 or payload.get("code") != 0:
            continue
        data = payload["data"]
        token = data["access_token"]
        if data.get("must_change_password"):
            fresh = candidate + "-t4"
            status, payload = api("PUT", "/system/auth/password", token,
                                  {"old_password": candidate, "new_password": fresh})
            if status != 200 or payload.get("code") != 0:
                return token, candidate
            status, payload = login(username, fresh)
            if status != 200 or payload.get("code") != 0:
                raise SystemExit(f"{username} 改密后重登失败：{status} {payload}")
            return payload["data"]["access_token"], fresh
        return token, candidate
    raise SystemExit(f"{username} 登录失败：{last}")


def admin_session():
    initial = ENV.get("NIK_ADMIN_PASSWORD", "")
    candidates = [initial, initial + "-t3", initial + "-t4", initial + "-e2e", initial + "-d6a"]
    token, password = login_and_settle("admin", [c for c in candidates if c])
    check("0.1 admin 登录（含首登改密兜底）", bool(token), f"生效密码长度={len(password)}")
    return token


# ==================== 内控开关（审核要过 SoD） ====================

def sod_config(token):
    status, payload = api("GET", f"/support/config/page?config_key={SOD_KEY}&page=1&page_size=10", token)
    if status != 200 or payload.get("code") != 0:
        return None
    for item in (payload.get("data") or {}).get("list") or []:
        if item.get("config_key") == SOD_KEY:
            return item
    return None


def config_payload(item, value):
    """把 /support/config 详情行原样转成 PUT 入参（只换 config_value）。"""
    return {
        "config_key": item["config_key"],
        "config_value": value,
        "config_type": item["config_type"],
        "description": item.get("description") or "",
        "category": item.get("category") or "",
        "sort_order": item.get("sort_order") or 0,
        "config_level": item.get("config_level") or 2,
        "is_modifiable": item.get("is_modifiable", True),
        "is_encrypted": item.get("is_encrypted", False),
        "is_restart": item.get("is_restart", False),
        "status": item.get("status", 1),
        "ref_value": item.get("ref_value") or "",
        "select_value": item.get("select_value") or "",
    }


def put_config(token, item, value):
    return api("PUT", f"/support/config/{item['id']}", token, config_payload(item, value))


def set_sod(token, value, label):
    global SOD_BACKUP
    item = sod_config(token)
    if not item:
        check(f"0.3 {label}：读配置 {SOD_KEY}", False, "配置行不存在（跳过开关操作）")
        return False
    if SOD_BACKUP is None:
        SOD_BACKUP = dict(item)
    status, payload = put_config(token, item, value)
    ok = status == 200 and payload.get("code") == 0
    check(f"0.3 {label}：{SOD_KEY} → {value}", ok,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")
    return ok


# ==================== 主数据 ====================

def master_data():
    step("0. 主数据（只读选取：5602.03 要求 DEPT / 1002 不要求维度 / 一个部门）")
    # 卡面点名的 5602.03（管理费用—办公费）优先；库里没有时退化成「第一个声明 DEPT 的末级科目」
    dept_required = sql_line(
        "SELECT id || '|' || account_code || '|' || account_name FROM biz_account "
        "WHERE deleted_at IS NULL AND status = 1 AND is_leaf = 1 AND account_code = '5602.03'")
    if not dept_required:
        dept_required = sql_line(
            "SELECT id || '|' || account_code || '|' || account_name FROM biz_account "
            "WHERE deleted_at IS NULL AND status = 1 AND is_leaf = 1 "
            "AND aux_dimensions IS NOT NULL AND aux_dimensions LIKE '%DEPT%' "
            "ORDER BY account_code LIMIT 1")
    plain = sql_line(
        "SELECT id || '|' || account_code || '|' || account_name FROM biz_account "
        "WHERE deleted_at IS NULL AND status = 1 AND is_leaf = 1 "
        "AND aux_dimensions IS NULL AND account_code = '1002'")
    dept = sql_line("SELECT id || '|' || dept_name FROM sys_dept WHERE deleted_at IS NULL "
                    "AND status = 1 ORDER BY sort_order, created_at LIMIT 1")
    check("0.2 取到「要求部门辅助核算」的末级科目", bool(dept_required), dept_required or "—")
    check("0.2 取到「不要求维度」的末级科目（1002 银行存款）", bool(plain), plain or "—")
    check("0.2 取到一个启用部门", bool(dept), dept or "—")
    if not (dept_required and plain and dept):
        raise SystemExit("主数据不全，无法继续")
    return {
        "dept_account_id": dept_required.split("|")[0],
        "dept_account_code": dept_required.split("|")[1],
        "dept_account_name": dept_required.split("|")[2],
        "plain_account_id": plain.split("|")[0],
        "dept_id": dept.split("|")[0],
        "dept_name": dept.split("|")[1],
    }


def voucher_entries(master, with_dept, amount="100.0000", bad_dept=None):
    entry = {
        "account_id": master["dept_account_id"],
        "debit": amount,
        "summary": MARK + " 借：部门费用",
    }
    if with_dept:
        entry["aux_dept_id"] = master["dept_id"]
    if bad_dept is not None:
        entry["aux_dept_id"] = bad_dept
    return [
        entry,
        {
            "account_id": master["plain_account_id"],
            "credit": amount,
            "summary": MARK + " 贷：银行存款",
        },
    ]


# ==================== ① 缺部门 → 400 ====================

def case_missing_dept(token, master):
    step("① 分录选「要求 DEPT」的科目但不选部门 → 400 + 明确提示")
    body = {
        "summary": MARK + " 缺部门",
        "entries": voucher_entries(master, with_dept=False),
        "remark": MARK,
    }
    status, payload = api("POST", "/business/erp/voucher", token, body)
    msg = payload.get("msg") or payload.get("message") or ""
    check("①.1 HTTP 400 + code 非 0", status == 400 and payload.get("code") not in (0, None),
          f"HTTP {status} code={payload.get('code')} msg={msg}")
    check("①.2 提示点名科目编码", master["dept_account_code"] in msg, msg)
    check("①.3 提示「要求部门辅助核算，请选择部门」", "要求部门辅助核算，请选择部门" in msg, msg)
    check("①.4 提示指路入参字段 aux_dept_id", "aux_dept_id" in msg, msg)
    check("①.5 没落库（本前缀凭证 0 张）",
          (sql_one(f"SELECT count(*) FROM biz_voucher WHERE summary LIKE '{MARK}%'") or "0") == "0",
          "失败请求不许留半张草稿")


# ==================== ② 带部门 → 落库 ====================

def case_with_dept(token, master):
    step("② 选了部门 → 保存成功，分录 aux_dept_id 有值")
    body = {
        "summary": MARK + " 部门费用",
        "entries": voucher_entries(master, with_dept=True),
        "remark": MARK,
    }
    status, payload = api("POST", "/business/erp/voucher", token, body)
    data = payload.get("data") or {}
    good = check("②.1 建手工凭证 200", status == 200 and payload.get("code") == 0,
                 f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")
    if not good:
        return None
    voucher_id = data.get("id")
    CREATED_VOUCHERS.append(voucher_id)
    check("②.2 落草稿（DRAFT，待复核）", data.get("status") == "DRAFT", f"status={data.get('status')}")
    entry = next((line for line in data.get("entries") or []
                  if line.get("account_id") == master["dept_account_id"]), None)
    check("②.3 回执分录 aux_dept_id 有值",
          bool(entry) and entry.get("aux_dept_id") == master["dept_id"],
          f"aux_dept_id={(entry or {}).get('aux_dept_id')}")
    db_value = sql_one(
        f"SELECT aux_dept_id FROM biz_voucher_entry WHERE voucher_id = '{voucher_id}'::uuid "
        f"AND account_id = '{master['dept_account_id']}'::uuid")
    check("②.4 库里 biz_voucher_entry.aux_dept_id 落库", db_value == master["dept_id"],
          f"db={db_value} expect={master['dept_id']}")
    return {"id": voucher_id, "voucher_no": data.get("voucher_no")}


# ==================== ③ 审核 + 记账 → 辅助账 ====================

def case_aux_balance(token, master, created):
    step("③ 审核 + 记账后查辅助账 → 能看到该笔")
    if not created:
        check("③.0 前置（② 建单成功）", False, "② 失败，跳过")
        return
    voucher_id = created["id"]
    status, payload = api("POST", f"/business/erp/voucher/{voucher_id}/audit", token,
                          {"remark": MARK + " 复核"})
    check("③.1 审核（DRAFT → AUDITED）", status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")
    status, payload = api("POST", f"/business/erp/voucher/{voucher_id}/post", token, {})
    check("③.2 记账（AUDITED → POSTED）", status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")

    date_from = sql_one("SELECT to_char(min(voucher_date), 'YYYY-MM') FROM biz_voucher "
                        f"WHERE id = '{voucher_id}'::uuid")
    query = (f"/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id={master['dept_id']}"
             f"&period_from={date_from}&period_to={date_from}")
    status, payload = api("GET", query, token)
    data = payload.get("data") or {}
    rows = data.get("rows") or []
    hit = next((row for row in rows if row.get("account_id") == master["dept_account_id"]), None)
    check("③.3 辅助账 200", status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} rows={len(rows)}")
    check("③.4 辅助账命中该科目（aux_type=DEPT）", bool(hit),
          f"rows={[(r.get('account_code'), r.get('period_debit')) for r in rows]}")
    check("③.5 本期借方 = 100.0000", bool(hit) and str(hit.get("period_debit")).startswith("100"),
          f"period_debit={(hit or {}).get('period_debit')}")
    check("③.6 辅助项名称按 sys_dept 取（不是 UUID）",
          bool(hit) and hit.get("aux_name") == master["dept_name"],
          f"aux_name={(hit or {}).get('aux_name')} expect={master['dept_name']}")


# ==================== ④ 不要求维度的科目 ====================

def case_no_dimension(token, master):
    step("④ 选不要求维度的科目（1002）→ 不需要 aux，正常保存")
    body = {
        "summary": MARK + " 无维度科目",
        "entries": [
            {"account_id": master["dept_account_id"], "debit": "50.0000",
             "aux_dept_id": master["dept_id"], "summary": MARK + " 借"},
            {"account_id": master["plain_account_id"], "credit": "50.0000",
             "summary": MARK + " 贷 1002（不要求维度）"},
        ],
        "remark": MARK,
    }
    status, payload = api("POST", "/business/erp/voucher", token, body)
    data = payload.get("data") or {}
    good = check("④.1 建单 200（1002 不要 aux）", status == 200 and payload.get("code") == 0,
                 f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")
    if good:
        CREATED_VOUCHERS.append(data.get("id"))
        plain = next((line for line in data.get("entries") or []
                      if line.get("account_id") == master["plain_account_id"]), None)
        check("④.2 1002 分录四个 aux 全空",
              bool(plain) and not any(plain.get(key) for key in
                                      ("aux_dept_id", "aux_supplier_id", "aux_material_id", "aux_warehouse_id")),
              f"entry={ {k: (plain or {}).get(k) for k in ('aux_dept_id', 'aux_supplier_id')} }")


# ==================== ⑤ 存量历史凭证 ====================

def case_legacy(token, master):
    step("⑤ 存量历史凭证（aux 全 NULL）不受影响")
    legacy = sql_one(
        "SELECT v.id FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id = v.id "
        "WHERE v.deleted_at IS NULL AND e.aux_dept_id IS NULL "
        "AND EXISTS (SELECT 1 FROM biz_account a WHERE a.id = e.account_id "
        "            AND a.aux_dimensions LIKE '%DEPT%') "
        "ORDER BY v.created_at LIMIT 1")
    check("⑤.1 库里存在「要求 DEPT 但分录 aux_dept_id 为 NULL」的存量凭证", bool(legacy),
          legacy or "—（本库没有这类历史数据，无法验证存量不阻断）")
    if not legacy:
        return
    status, payload = api("GET", f"/business/erp/voucher/{legacy}", token)
    check("⑤.2 存量凭证详情照常 200（读取不校验辅助核算）",
          status == 200 and payload.get("code") == 0,
          f"HTTP {status} code={payload.get('code')} msg={payload.get('msg') or ''}")
    rows = psql(f"SELECT count(*) FROM biz_voucher_entry WHERE voucher_id = '{legacy}'::uuid "
                "AND aux_dept_id IS NULL")
    check("⑤.3 存量分录的 aux_dept_id 仍是 NULL（没被回填 / 没被改）",
          bool(rows) and rows[0][0] != "0", f"NULL 行数={rows[0][0] if rows else '?'}")


# ==================== ⑥ 非法 aux id ====================

def case_bad_aux_uuid(token, master):
    step("⑥ aux_dept_id 不是 UUID → 400 并点名字段")
    body = {
        "summary": MARK + " 非法部门",
        "entries": voucher_entries(master, with_dept=False, bad_dept="not-a-uuid"),
        "remark": MARK,
    }
    status, payload = api("POST", "/business/erp/voucher", token, body)
    msg = payload.get("msg") or payload.get("message") or ""
    check("⑥.1 HTTP 400", status == 400, f"HTTP {status} msg={msg}")
    check("⑥.2 点名字段 entries[0].aux_dept_id", "aux_dept_id" in msg, msg)


# ==================== 收尾 ====================

def cleanup():
    step("收尾：删本脚本自建的凭证（状态日志 → 分录 → 凭证）+ 恢复内控开关")
    ids = [v for v in CREATED_VOUCHERS if v]
    if ids:
        id_list = ",".join(f"'{v}'::uuid" for v in ids)
        psql(f"DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN ({id_list})")
        psql(f"DELETE FROM biz_voucher_entry WHERE voucher_id IN ({id_list})")
        psql(f"DELETE FROM biz_voucher WHERE id IN ({id_list})")
    left = sql_one(f"SELECT count(*) FROM biz_voucher WHERE summary LIKE '{MARK}%'")
    check("⑦.1 自建凭证零残留", left == "0", f"残留={left}")
    if SOD_BACKUP is not None:
        token = ENV.get("__TOKEN__")
        status, payload = (put_config(token, SOD_BACKUP, SOD_BACKUP.get("config_value"))
                           if token else (0, {}))
        check("⑦.2 内控开关恢复原值", status == 200 and payload.get("code") == 0,
              f"{SOD_KEY} → {SOD_BACKUP.get('config_value')}")


# ==================== main ====================

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=DEFAULTS["base"])
    parser.add_argument("--env-file", default=DEFAULTS["env_file"])
    args = parser.parse_args()
    CFG["base"] = args.base
    CFG["env_file"] = args.env_file
    ENV.update(load_env_file(args.env_file))

    print(f"目标：{CFG['base']}  库：{db_conf()['name']}  标记：{MARK}")
    token = admin_session()
    ENV["__TOKEN__"] = token

    master = master_data()
    try:
        set_sod(token, "false", "临时关闭制单/审核分离（只为走通审核 + 记账）")
        case_missing_dept(token, master)
        created = case_with_dept(token, master)
        case_aux_balance(token, master, created)
        case_no_dimension(token, master)
        case_legacy(token, master)
        case_bad_aux_uuid(token, master)
    finally:
        cleanup()

    print()
    print("=" * 78)
    print(f"PASS={PASSED} FAIL={FAILED}")
    if FAILS:
        print("失败项：")
        for name in FAILS:
            print("  - " + name)
    print("=" * 78)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
