#!/usr/bin/env python3
"""HOME-5 验收：用**真实接口**发起 12 张审批单（真实流程引擎建实例 + 派待办）。

不直接插库造审批实例：`POST /api/business/flow/instance` 会按已发布流程建实例 / 节点 / 待办，
派单规则与线上完全一致（本库里 FINANCE_MANAGER 等角色 0 人 → FALLBACK_ADMIN 兜底给 admin）。
只有「到达时间」在实例建好之后用 SQL 回填（模拟历史待办，验证相对时间），这一步是测试夹具，不是伪造接口返回。
"""
import json
import subprocess
import urllib.request
import uuid

BASE = "http://127.0.0.1:18097"
TOKEN = open("/var/tmp/home5/api/clerk.token").read().strip()

CASES = [
    # (biz_type, 单据类型前缀, 单号, 金额, 到达时间回填)
    # 只用 PO / 合同 / 报销：这三类**没有**「孤儿实例回收」后台任务，不会被对账任务自动终止。
    ("PO_APPROVE", "采购订单", "PO-20261109-000003", 12800, "30 minutes"),
    ("EXPENSE", "费用报销", "EXP-20261109-0001", 3200, "45 minutes"),
    ("PO_APPROVE", "采购订单", "PO-20261109-000004", 8600, "2 hours"),
    ("EXPENSE", "费用报销", "EXP-20261109-0002", 1280, "3 hours"),
    ("CONTRACT_APPROVE", "合同", "CT-20261109-000001", 150000, "5 hours"),
    ("PO_APPROVE", "采购订单", "PO-20261108-000002", 96000, "6 hours"),
    ("EXPENSE", "费用报销", "EXP-20261107-0003", 2600, "1 day"),
    ("CONTRACT_APPROVE", "合同", "CT-20261108-000002", 88000, "2 days"),
    ("PO_APPROVE", "采购订单", "PO-20261107-000001", 450, "3 days"),
    ("EXPENSE", "费用报销", "EXP-20261106-0004", 780, "4 days"),
    ("PO_APPROVE", "采购订单", "PO-20261105-000001", 4200, "8 days"),
    ("CONTRACT_APPROVE", "合同", "CT-20261101-000003", 12000, "9 days"),
]


def call(path, payload):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "Authorization": "Bearer " + TOKEN},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as err:
        return {"http": err.code, "body": err.read().decode()}


created = []
for biz_type, label, no, amount, age in CASES:
    form = {"order_no": no, "doc_no": no, "amount": amount, "title": f"{label} {no}"}
    payload = {
        "biz_type": biz_type,
        "biz_id": str(uuid.uuid4()),
        "biz_no": no,
        "title": f"{label} {no}",
        "form_data": form,
    }
    res = call("/api/business/flow/instance", payload)
    instance_id = ((res.get("data") or {}).get("instance_id")) if res.get("code") == 0 else None
    print(biz_type, no, "->", res.get("code"), res.get("msg"), instance_id or res)
    if instance_id:
        created.append({"biz_type": biz_type, "no": no, "instance_id": instance_id, "age": age})

with open("/var/tmp/home5/api/instances.json", "w") as fh:
    json.dump(created, fh, ensure_ascii=False, indent=2)
print("created:", len(created))
