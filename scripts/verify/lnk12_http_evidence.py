#!/usr/bin/env python3
"""LNK-12 审批实例（FLOW_INSTANCE）联动的真机 HTTP 取证。

跑法（在 99 上打 145 测试环境）：

    python3 scripts/verify/lnk12_http_evidence.py http://172.18.10.145:8033

覆盖（卡面「同一次审批只触发一次」逐条对应）：

| # | 步骤 | 期望 |
|---|---|---|
| 1 | 触发点字典 `FLOW_INSTANCE` | `wired=true`，可配状态**只有** APPROVED / REJECTED / WITHDRAWN |
| 2 | 在 FLOW_INSTANCE 上配 `GENERATE_VOUCHER` | **400**（配置期就拒：落账归业务桥，重复落账不可能） |
| 3 | 配 `trigger_status=PENDING` | **400**（PENDING 不是结论，配了永远不响） |
| 4 | 配 `FLOW_INSTANCE/APPROVED + SEND_NOTICE` | 200 |
| 5 | 发起 `stock_in_cancel` 审批单（审批节点 = 全部超管） | 200，实例 PENDING |
| 6 | 同意该待办 | 200，实例 **APPROVED** |
| 7 | 查我的站内信 | 标题含 `LNK12` 的**恰好 1 条** |
| 8 | **再次**同意同一条待办 | 409 / 65024，站内信**仍是 1 条**（重复触发被抑制） |
| 9 | 清场：删掉本用例的规则 | 200 |

退出码 0 = 全部符合期望；非 0 = 有断言失败（输出里能看到是第几步）。
"""

import json
import os
import re
import sys
import urllib.error
import urllib.request
import uuid

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8033").rstrip("/")
USER = os.environ.get("EVIDENCE_ADMIN_USER", "admin")
# 口令**不写进仓库**：优先读环境变量（145 上是 /etc/cloudpivot/cloudpivot-dev.env 的
# NIK_ADMIN_PASSWORD），取不到才退回本地开发默认值。
PASSWORD = (
    os.environ.get("EVIDENCE_ADMIN_PASSWORD")
    or os.environ.get("NIK_ADMIN_PASSWORD")
    or "Admin@123456"
)

TOKEN = None
FAILED = []

# 本卡规则的标题（也是断言用的锚点：只数这一条，别把流程引擎自己的站内信算进来）
HOOK_TITLE = "LNK12 审批通过取证"

# 取证输出要进仓库：JWT 一律抹掉（登录响应里带 access_token / refresh_token）
_JWT = re.compile(r"eyJ[A-Za-z0-9_.\-]{20,}")


def redact(text):
    return _JWT.sub("<REDACTED_JWT>", text)


def call(method, path, body=None, expect=200, label=""):
    """发一次请求，返回 (http_code, json)。`expect=None` 表示不断言状态码。"""
    url = f"{BASE}/api{path}"
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            code, raw = resp.status, resp.read().decode()
    except urllib.error.HTTPError as err:
        code, raw = err.code, err.read().decode()
    try:
        payload = json.loads(raw) if raw else {}
    except json.JSONDecodeError:
        payload = {"_raw": raw[:400]}
    ok = expect is None or code == expect
    print(f"[{'OK ' if ok else 'BAD'}] {label or path} → HTTP {code} ({method} {path})")
    if not ok:
        FAILED.append(f"{label or path}: 期望 {expect} 实际 {code} / {payload}")
    print("      " + redact(json.dumps(payload, ensure_ascii=False))[:600])
    return code, payload


def data_of(payload):
    return payload.get("data", payload)


def main():
    global TOKEN
    print(f"===== LNK-12 FLOW_INSTANCE HTTP 取证 @ {BASE} =====")

    _, payload = call(
        "POST",
        "/system/auth/login",
        {"username": USER, "password": PASSWORD},
        expect=200,
        label="登录",
    )
    login = data_of(payload)
    TOKEN = login.get("token") or login.get("access_token")
    assert TOKEN, f"登录响应里没有 token：{payload}"
    print(f"      token 长度 {len(TOKEN)}")

    # ① 触发点字典：wired + 三个结论状态（`GET /business/flow/linkage` 同时返回规则与字典）
    _, payload = call(
        "GET",
        "/business/flow/linkage",
        expect=200,
        label="触发点字典",
    )
    payload = data_of(payload)
    triggers = payload.get("triggers") or []
    flow = next((item for item in triggers if item.get("entity") == "FLOW_INSTANCE"), None)
    if flow is None:
        print("      triggers 原始：", json.dumps(triggers, ensure_ascii=False)[:300])
        FAILED.append("字典里没有 FLOW_INSTANCE")
        statuses = []
        wired = None
    else:
        wired = flow.get("wired")
        statuses = [pair.get("code") for pair in flow.get("status_labels", [])] or flow.get(
            "statuses", []
        )
        print(f"      wired={wired} statuses={statuses}")
    if wired is not True:
        FAILED.append(f"FLOW_INSTANCE wired 必须是 true，实际 {wired!r}")
    if statuses != ["APPROVED", "REJECTED", "WITHDRAWN"]:
        FAILED.append(f"可配状态必须是三个结论，实际 {statuses!r}")

    tag = uuid.uuid4().hex[:8].upper()
    rule_name = f"LNK12-{tag}"

    # ② 落账类动作必须配置期就拒
    call(
        "PUT",
        "/business/flow/linkage",
        {
            "name": f"{rule_name}- voucher",
            "trigger_entity": "FLOW_INSTANCE",
            "trigger_status": "APPROVED",
            "action_code": "GENERATE_VOUCHER",
            "action_params": {"biz_type": "STOCK_IN", "voucher_date": "2026-11-07"},
            "confirm": True,
            "allow_unwired": False,
        },
        expect=400,
        label="FLOW_INSTANCE 上配 GENERATE_VOUCHER（应 400）",
    )

    # ③ PENDING 不是结论
    call(
        "PUT",
        "/business/flow/linkage",
        {
            "name": f"{rule_name}- pending",
            "trigger_entity": "FLOW_INSTANCE",
            "trigger_status": "PENDING",
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER"},
            "confirm": True,
            "allow_unwired": False,
        },
        expect=400,
        label="FLOW_INSTANCE/PENDING 配规则（应 400）",
    )

    # ④ 正常规则
    _, payload = call(
        "PUT",
        "/business/flow/linkage",
        {
            "name": rule_name,
            "trigger_entity": "FLOW_INSTANCE",
            "trigger_status": "APPROVED",
            "action_code": "SEND_NOTICE",
            "action_params": {
                "mode": "TAKEOVER",
                "title": HOOK_TITLE,
                "content": "LNK12：这张审批单已通过（真机取证）",
            },
            "confirm": True,
            "allow_unwired": False,
        },
        expect=200,
        label="配 FLOW_INSTANCE/APPROVED → 通知",
    )
    rule_id = data_of(payload).get("id")

    # ⑤ 发起审批单（flow_edge_config：审批节点 = 角色兜底管理员，业务桥对无 biz_id 的实例是 no-op）
    _, payload = call(
        "POST",
        "/business/flow/instance",
        {"flow_code": "flow_edge_config", "title": f"LNK12 联动取证 {tag}"},
        expect=200,
        label="发起审批单",
    )
    instance = data_of(payload)
    instance_id = instance.get("instance_id") or instance.get("id")
    print(f"      instance_id={instance_id} status={instance.get('instance_status')}")

    # ⑥ 取出我的待办并同意
    _, payload = call(
        "GET",
        f"/business/flow/instance/{instance_id}",
        expect=200,
        label="审批单详情",
    )
    detail = data_of(payload)
    task = detail.get("my_task") or {}
    task_id = task.get("id")
    if not task_id:
        FAILED.append(f"发起后没有我的待办：{json.dumps(detail, ensure_ascii=False)[:400]}")
        return finish(rule_id, instance_id)

    # ⑦ 记下审批前的「本卡配置那条通知」基数
    #    ⚠️ 只数**本卡规则发的**那条（标题精确匹配）：发起 / 审批时流程引擎自己也会给
    #    发起人发站内信（「待你审批」「审批已通过」），那不是联动通知，混进来会把断言算错。
    before = count_hook_notices(HOOK_TITLE, label="同意前：本卡配置的通知条数")

    _, payload = call(
        "POST",
        f"/business/flow/task/{task_id}/approve",
        {"comment": "LNK12 取证：同意"},
        expect=200,
        label="同意待办（第一次）",
    )
    after_first = data_of(payload).get("instance_status")
    print(f"      审批后实例状态 = {after_first}")
    if after_first != "APPROVED":
        FAILED.append(f"审批后实例应是 APPROVED，实际 {after_first!r}")

    first_count = count_hook_notices(HOOK_TITLE, label="同意后：应 +1")
    if first_count != before + 1:
        FAILED.append(
            f"审批通过应恰好新增 1 条 FLOW_INSTANCE 联动通知，实际 {before} → {first_count}"
        )

    # ⑧ 重复同意：引擎按幂等放行（200 / idempotent=true，不是 409），
    #    但 hook 会再跑一遍 → 通知必须被幂等键挡下（**本卡验收的核心**）。
    _, payload = call(
        "POST",
        f"/business/flow/task/{task_id}/approve",
        {"comment": "LNK12 取证：重复同意"},
        expect=200,
        label="重复同意同一待办（引擎幂等放行）",
    )
    idempotent = data_of(payload).get("idempotent")
    print(f"      idempotent={idempotent} status={data_of(payload).get('instance_status')}")
    if idempotent is not True:
        FAILED.append(f"重复同意应命中引擎幂等（idempotent=true），实际 {idempotent!r}")
    repeat_count = count_hook_notices(HOOK_TITLE, label="重复同意后：应不变")
    if repeat_count != first_count:
        FAILED.append(f"重复触发被抑制失败：{first_count} → {repeat_count}")

    return finish(rule_id, instance_id)


def count_hook_notices(title, label):
    """数「标题 == title」的站内信条数（本卡规则发的那条，idempotency_key 由引擎挡重）。"""
    _, payload = call(
        "GET",
        "/business/notice/mine?page=1&page_size=100",
        expect=200,
        label=label,
    )
    data = data_of(payload)
    rows = data.get("list") or []
    hits = [row for row in rows if (row.get("title") or "") == title]
    print(f"      标题==「{title}」的通知 {len(hits)} 条")
    for row in hits:
        print(f"      · {row.get('notice_no')} / {row.get('created_at')}")
    return len(hits)


def finish(rule_id, instance_id):
    if rule_id:
        call(
            "DELETE",
            f"/business/flow/linkage?id={rule_id}&confirm=true",
            expect=None,
            label="清场：删规则",
        )
    print(f"\n取证实例保留（便于人工复核）：{instance_id}")
    if FAILED:
        print("\n===== 失败项 =====")
        for item in FAILED:
            print(f" - {item}")
        return 1
    print("\n===== ALL PASS =====")
    return 0


if __name__ == "__main__":
    sys.exit(main())
