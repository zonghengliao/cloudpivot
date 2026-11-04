#!/usr/bin/env python3
"""ERP-B2 真机验收（审批侧）：`erp.separation_of_duties.approve` 开关**真的**管住了审批。

卡面 §三 的 ④「采购单审批同样拦截」/ ⑤「付款同样拦截」在单测里是纯逻辑；
这个脚本在真机上钉住「开关 → 待办生成 → 403」这条**接线**，防止「开关读了但读反了」。

# 观察点（现象级，不看代码）

种子流程 `po_approve` 的审批人按角色展开、没人时回退「全部超管」，制单人必然在名单里。
于是同一个动作在两个开关状态下结果完全不同：

```
approve = true（默认）  制单人 → 待办落 SKIPPED（留痕「内控…职责分离」），批不动
approve = false         制单人 → 待办落 PENDING，能自己批（小团队自审）
```

用法：
```bash
B2_BASE_URL=http://127.0.0.1:18097 B2_DB_NAME=erpb2_e2e python3 scripts/verify/b2_approve_switch_e2e.py
```
"""

from __future__ import annotations


# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import b2_separation_e2e as base  # noqa: E402  复用登录 / psql / call / 夹具

PASSED: list[str] = []
FAILED: list[str] = []
APPROVE_KEY = base.APPROVE_KEY


def check(ok: bool, name: str, detail: str = "") -> bool:
    if ok:
        PASSED.append(name)
        print(f"[PASS] {name}")
    else:
        FAILED.append(name)
        print(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))
    return ok


def create_po(token: str, remark: str) -> str:
    status, body = base.call("POST", "/api/business/erp/purchase-order", {
        "supplier_id": base.SUPPLIER_ID,
        "warehouse_id": base.WAREHOUSE_ID,
        "order_date": "2026-11-02",
        "remark": remark,
        "items": [{"material_id": base.MATERIAL_ID, "qty": 2, "price_excl": 10, "tax_rate": 13}],
    }, token)
    if status != 200:
        raise SystemExit(f"建采购单失败：{status} {body}")
    return (body.get("data") or {})["order"]["id"]


def submit_po(po_id: str, token: str) -> tuple[int, dict]:
    return base.call("POST", f"/api/business/erp/purchase-order/{po_id}/submit-approval", {}, token)


def maker_task(instance_id: str, maker_id: str) -> tuple[str, str]:
    raw = base.scalar(
        "SELECT status || '|' || coalesce(comment, '') FROM biz_flow_task "
        f"WHERE instance_id = '{instance_id}'::uuid AND assignee_id = '{maker_id}'::uuid "
        "ORDER BY created_at DESC LIMIT 1"
    )
    parts = (raw.split("|", 1) + ["", ""])[:2]
    return parts[0], parts[1]


def instance_of(po_id: str) -> str:
    return base.scalar(
        f"SELECT coalesce(approval_instance_id::text, '') FROM biz_purchase_order WHERE id = '{po_id}'::uuid"
    )


def cleanup_po() -> None:
    for sql in (
        f"DELETE FROM biz_flow_task WHERE instance_id IN (SELECT approval_instance_id FROM biz_purchase_order WHERE supplier_id = '{base.SUPPLIER_ID}'::uuid AND approval_instance_id IS NOT NULL)",
        f"DELETE FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{base.SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{base.SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_purchase_order_item WHERE order_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{base.SUPPLIER_ID}'::uuid)",
        f"DELETE FROM biz_purchase_order WHERE supplier_id = '{base.SUPPLIER_ID}'::uuid",
    ):
        try:
            base.psql(sql)
        except RuntimeError as error:  # noqa: PERF203
            print(f"[warn] 清理失败：{error}")


def main() -> int:
    print(f"== ERP-B2 审批侧真机验收 @ {base.BASE} / {base.DB_NAME}")
    base.cleanup_all()
    base.setup_fixtures()
    admin_token = base.login(base.ADMIN_USER, base.ADMIN_PASS, allow_change=True) or base.login(
        base.ADMIN_USER, base.ADMIN_NEW_PASS)
    if not admin_token:
        print("admin 登录失败", file=sys.stderr)
        return 2
    base.ADMIN_TOKEN = admin_token
    maker_id = base.ensure_maker()
    maker_token = base.login(base.MAKER_USER, base.MAKER_PASS)
    if not maker_token:
        raise SystemExit("制单人账号登录失败")

    # ---------- ① 默认开：制单人的待办落 SKIPPED（内控排除），批不动 ----------
    base.set_switch(APPROVE_KEY, "true")
    po1 = create_po(maker_token, "ERP-B2 审批默认开")
    status, body = submit_po(po1, maker_token)
    check(status == 200, "① 制单人提交采购单审批 200", f"{status} {body}")
    inst1 = instance_of(po1)
    check(inst1 != "", "① 审批实例已创建", inst1)
    task_status, comment = maker_task(inst1, maker_id)
    check(task_status == "SKIPPED", "① 默认开：制单人待办落 SKIPPED（内控排除）", task_status or "无待办")
    check("内控" in comment and "职责分离" in comment, "① SKIPPED 行留痕写明原因", comment)
    pending = base.scalar(
        f"SELECT count(*) FROM biz_flow_task WHERE instance_id = '{inst1}'::uuid "
        f"AND assignee_id = '{maker_id}'::uuid AND status = 'PENDING'"
    )
    check(pending == "0", "① 制单人列表里没有可批的待办（不是「看得见点不动」）", pending)

    # ---------- ② 关掉开关：制单人待办落 PENDING，能自己批 ----------
    st, resp = base.set_switch(APPROVE_KEY, "false")
    check(st == 200, "② 关掉「制单与审批分离」", f"{st} {resp}")
    po2 = create_po(maker_token, "ERP-B2 审批关掉后自审")
    status, body = submit_po(po2, maker_token)
    check(status == 200, "② 制单人再次提交审批 200", f"{status} {body}")
    inst2 = instance_of(po2)
    task_status, _ = maker_task(inst2, maker_id)
    check(task_status == "PENDING", "② 关掉后：制单人待办落 PENDING（小团队可自审）", task_status or "无待办")
    task_id = base.scalar(
        f"SELECT id::text FROM biz_flow_task WHERE instance_id = '{inst2}'::uuid "
        f"AND assignee_id = '{maker_id}'::uuid AND status = 'PENDING' LIMIT 1"
    )
    if task_id:
        status, body = base.call("POST", f"/api/business/flow/task/{task_id}/approve",
                                 {"comment": "小团队自审（开关已关）"}, maker_token)
        check(status == 200, "② 制单人能批自己那一票（200，不再是 403）", f"{status} {body}")
        after_status, _ = maker_task(inst2, maker_id)
        check(after_status == "APPROVED", "② 制单人那一票真的落地了（APPROVED）", after_status)
        # 采购流程可能不止一个审批节点：这里只断言「制单人这一票批得动」，
        # 不断言整单 APPROVED（那取决于种子里配了几个节点，与本开关无关）
        inst_status = base.scalar(
            f"SELECT status FROM biz_flow_instance WHERE id = '{inst2}'::uuid")
        check(inst_status in ("PENDING", "APPROVED"), "② 审批单仍在正常流转", inst_status)

    # ---------- ③ 打开开关再提交：又回到「批不动」 ----------
    base.set_switch(APPROVE_KEY, "true")
    po3 = create_po(maker_token, "ERP-B2 再打开")
    submit_po(po3, maker_token)
    inst3 = instance_of(po3)
    task_status, _ = maker_task(inst3, maker_id)
    check(task_status == "SKIPPED", "③ 重新打开开关后制单人又被排除（改完立即生效）", task_status or "无待办")

    base.set_switch(APPROVE_KEY, "true")
    cleanup_po()
    if not base.KEEP:
        base.cleanup_all()
    print(f"\n== 结果 PASS={len(PASSED)} FAIL={len(FAILED)}")
    for item in FAILED:
        print("  - " + item)
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
