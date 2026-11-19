#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M5-31 成本方法切换（存量期间不重算）—— 真实 HTTP 冒烟。

钉住的 9 条（每条都是真实 HTTP + 真实库，不桩）：

  ① 登录 + 首登强制改密（admin 种子口令 → 新口令）；
  ② `GET .../switch-plan` 缺省切换日 = 建议日（下月 1 日），收口种子行 + 新增一笔；
  ③ 造一个「上月已结算」期间后，预演把它列进 history 且 `recalculated=false` / `unchanged=true`；
  ④ 切换日不是期间首日 ⇒ 409 + 66113（报文带建议日）；
  ⑤ 未实现方法（品种法）不给确认 ⇒ 409 + 66101 且**一行没写**（种子行仍长期有效）；
  ⑥ 给确认 ⇒ 200；返回收口 id = 种子行、新版本从切换日生效；
  ⑦ 同一预演再执行 ⇒ 409 + 66102（同一天只能切一次）；
  ⑧ 生效方法查询：存量期间末日仍是分批法、切换日已是品种法（历史不重算）；
  ⑨ 结果表零改动：期间行的状态 / 总成本在切换前后逐字相同。

用法：
    python3 m531_http_smoke.py                # 默认 http://127.0.0.1:18076
退出码 0 = 全过。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import date

BASE = os.environ.get("M531_BASE", "http://127.0.0.1:18076")
DB = os.environ.get("M531_DB", "cloudpivot_test_m531")
HOST = os.environ.get("M531_PGHOST", "172.18.10.145")
PASS = os.environ.get("M531_PGPASS") or os.environ.get("PGPASSWORD")
if not PASS:
    sys.exit("需要 M531_PGPASS 或 PGPASSWORD 环境变量（演练库口令；仓里不落密钥）")
OLD_PW = "Admin@123456"
NEW_PW = "Admin@654321"
SEED_ID = "20000000-0000-4000-8000-00000000d001"

PASSED = 0
FAILED = 0
TOKEN = None


def say(text):
    print(text, flush=True)


def check(label, cond, detail=""):
    global PASSED, FAILED
    if cond:
        PASSED += 1
        say(f"  ✓ {label}")
    else:
        FAILED += 1
        say(f"  ✗ {label}  {detail}")


def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = PASS
    out = subprocess.run(
        ["psql", "-h", HOST, "-U", "cloudpivot", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env=env, check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败: {out.stderr.strip()}")
    return out.stdout.strip()


def call(method, path, body=None, token=None, expect=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            status, raw = resp.status, resp.read().decode()
    except urllib.error.HTTPError as err:
        status, raw = err.code, err.read().decode()
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        payload = {"raw": raw}
    if expect is not None:
        ok = status == expect[0] and (expect[1] is None or payload.get("code") == expect[1])
        check(f"{method} {path} → {status}/{payload.get('code')}", ok, str(payload)[:400])
    return status, payload


def period_bounds(code):
    year, month = int(code[:4]), int(code[5:7])
    first = date(year, month, 1)
    nxt = date(year + 1, 1, 1) if month == 12 else date(year, month + 1, 1)
    from datetime import timedelta
    return first, nxt - timedelta(days=1)


def main():
    global TOKEN
    today = date.today()
    # ① 登录 + 首登改密
    status, payload = call("POST", "/api/system/auth/login",
                           {"username": "admin", "password": OLD_PW}, token=None)
    if status != 200:
        # 上一轮已经改过密（脚本可重复跑）
        status, payload = call("POST", "/api/system/auth/login",
                               {"username": "admin", "password": NEW_PW}, expect=(200, 0))
    check("登录成功（种子口令或已改口令）", status == 200, str(payload)[:200])
    if status != 200:
        return 1
    TOKEN = payload["data"]["access_token"]
    check("access_token 是真实 JWT（长度 > 100）", len(TOKEN) > 100, f"len={len(TOKEN)}")
    if payload["data"].get("must_change_password"):
        call("PUT", "/api/system/auth/password",
             {"old_password": OLD_PW, "new_password": NEW_PW}, token=TOKEN, expect=(200, 0))
        status, payload = call("POST", "/api/system/auth/login",
                               {"username": "admin", "password": NEW_PW}, expect=(200, 0))
        TOKEN = payload["data"]["access_token"]
        check("改密后用新口令能登录", status == 200)

    # ③ 造一个「上月已结算」期间（存量期间）
    last_month_code = f"{today.year - 1}-12" if today.month == 1 else f"{today.year}-{today.month - 1:02d}"
    psql(
        "INSERT INTO biz_cost_period (id, period_code, batch_no, status, object_count, "
        "total_material, total_labor, total_overhead, total_cost, total_finished, total_wip, "
        "approval_instance_id, settled_at, finish_voucher_id, created_at, updated_at) VALUES "
        f"('53100000-0000-4000-8000-00000000b001', '{last_month_code}', 'CS-531000-0001', 'SETTLED', 1, "
        "100, 20, 30, 150, 150, 0, gen_random_uuid(), now(), gen_random_uuid(), now(), now()) "
        "ON CONFLICT (id) DO NOTHING"
    )
    snapshot = psql(
        "SELECT period_code||'/'||status||'/'||total_cost FROM biz_cost_period "
        "WHERE id = '53100000-0000-4000-8000-00000000b001'::uuid"
    )

    # ② 预演（缺省切换日 = 建议日）
    status, plan = call("GET", "/api/business/erp/cost-method/switch-plan?to_method=VARIETY",
                        token=TOKEN, expect=(200, 0))
    data = plan.get("data") or {}
    switch_on = data.get("switch_on", "")
    check("建议切换日 = 下月 1 日", data.get("suggested_switch_on") == switch_on and switch_on.endswith("-01"),
          f"switch_on={switch_on} suggested={data.get('suggested_switch_on')}")
    check("预演两笔写（收口 + 新增）",
          [op.get("action") for op in data.get("ops", [])] == ["CLOSE", "INSERT"], str(data.get("ops"))[:200])
    check("收口的是种子行", (data.get("ops") or [{}])[0].get("config_id") == SEED_ID)
    check("存量期间进 history 且不重算",
          len(data.get("history", [])) == 1
          and data["history"][0]["period_code"] == last_month_code
          and data["history"][0]["recalculated"] is False
          and data["history"][0]["unchanged"] is True,
          str(data.get("history"))[:300])
    check("history_untouched = true", data.get("history_untouched") is True)
    check("品种法一期未实现 ⇒ engine_ready=false + 66101 告警",
          data.get("engine_ready") is False
          and any("66101" in line for line in data.get("warnings", [])), str(data.get("warnings"))[:200])

    # ④ 切换日不是期间首日 ⇒ 409/66113
    mid = switch_on[:8] + "15"
    call("GET", f"/api/business/erp/cost-method/switch-plan?to_method=VARIETY&switch_on={mid}",
         token=TOKEN, expect=(409, 66113))

    # ⑤ 未实现方法不给确认 ⇒ 409/66101 且一行没写
    call("POST", "/api/business/erp/cost-method/switch",
         {"to_method": "VARIETY", "switch_on": switch_on}, token=TOKEN, expect=(409, 66101))
    seed_stop = psql(f"SELECT coalesce(effective_to::text,'') FROM biz_cost_method_config WHERE id='{SEED_ID}'::uuid")
    check("被拒后种子行仍是长期有效（一行没写）", seed_stop == "", f"effective_to={seed_stop}")

    # ⑥ 给确认 ⇒ 200
    status, result = call("POST", "/api/business/erp/cost-method/switch",
                          {"to_method": "VARIETY", "switch_on": switch_on,
                           "confirm_unimplemented": True, "remark": "M5-31 HTTP 冒烟"},
                          token=TOKEN, expect=(200, 0))
    created_id = (result.get("data") or {}).get("created_config_id")
    check("返回新版本 id", bool(created_id))
    check("返回收口动作 = 种子行", ((result.get("data") or {}).get("closed") or {}).get("config_id") == SEED_ID)
    check("结论里点名「不重算」", "不重算" in (result.get("data") or {}).get("message", ""),
          (result.get("data") or {}).get("message", ""))

    # ⑦ 同一天再切 ⇒ 409/66102
    call("POST", "/api/business/erp/cost-method/switch",
         {"to_method": "STANDARD", "switch_on": switch_on, "confirm_unimplemented": True},
         token=TOKEN, expect=(409, 66102))

    # ⑧ 生效方法：存量期间末日仍是分批法，切换日是品种法
    _, last_day = period_bounds(last_month_code)
    _, history_hit = call("GET", f"/api/business/erp/cost-method/effective?on={last_day}",
                          token=TOKEN, expect=(200, 0))
    check("存量期间末日仍按分批法", (history_hit.get("data") or {}).get("method") == "JOB",
          str(history_hit.get("data"))[:200])
    _, new_hit = call("GET", f"/api/business/erp/cost-method/effective?on={switch_on}",
                      token=TOKEN, expect=(200, 0))
    check("切换日起按品种法", (new_hit.get("data") or {}).get("method") == "VARIETY",
          str(new_hit.get("data"))[:200])

    # ⑨ 结果表零改动
    after = psql(
        "SELECT period_code||'/'||status||'/'||total_cost FROM biz_cost_period "
        "WHERE id = '53100000-0000-4000-8000-00000000b001'::uuid"
    )
    check("成本期间行零改动（不重算）", snapshot == after, f"{snapshot} vs {after}")
    seed_stop = psql(f"SELECT effective_to::text FROM biz_cost_method_config WHERE id='{SEED_ID}'::uuid")
    from datetime import date as _date, timedelta as _td
    prev_day = (_date.fromisoformat(switch_on) - _td(days=1)).isoformat()
    check("种子行停止日 = 切换日前一天", seed_stop == prev_day, f"effective_to={seed_stop} 应为 {prev_day}")

    # 收尾：还原种子行 + 删新版本 + 删期间夹具
    if created_id:
        psql(f"DELETE FROM biz_cost_method_config WHERE id = '{created_id}'::uuid")
    psql(f"UPDATE biz_cost_method_config SET effective_to = NULL, updated_at = now() WHERE id = '{SEED_ID}'::uuid")
    psql("DELETE FROM biz_cost_period WHERE id = '53100000-0000-4000-8000-00000000b001'::uuid")
    say(f"\n== 通过 {PASSED} / 失败 {FAILED} ==")
    return 0 if FAILED == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
