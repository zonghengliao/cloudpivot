#!/usr/bin/env python3
"""P-FLOW-3B 补漏真机验收：三处 REST 契约缺口的**真实 HTTP**断言。

对应卡面 `t_a7fde8f7`：

| 缺口 | 断言（真 HTTP，非单测） |
|---|---|
| ① `VoucherRes` 未下发红冲审批字段 | 送审后 `GET /voucher/{id}`、`GET /voucher/page`、`POST /submit-reverse` 三处都能读到 `pending_reverse_instance_id` / `reverse_reason`，且 `status` 仍是 `POSTED` |
| ③ `payment_capability` 未拦 `PENDING_CANCEL` | 作废审批中 `GET /payable/{id}/payment-info` → `can_pay=false`、`default_amount=0`、`blocked_reason` 带「正在作废审批中（FLW-…）」；**对照**：同一张单作废前 `can_pay=true` |
| ④ `PayableRes` 未下发作废审批字段 | 送审后 `GET /payable/{id}` 与 `GET /payable/page` 都能读到 `cancel_instance_id` / `cancel_reason` |

夹具与链路复用 `pflow3b_finance_approval_e2e.py` 的同一套（真收货过账 → 自动应付、
真手工凭证），所以跑完必须 `cleanup()`（`P3B_KEEP=1` 可留场排查）。

用法：

```bash
P3B_BASE_URL=http://127.0.0.1:18097 \\
P3B_DB_NAME=cloudpivot_p3bgap P3B_DB_USER=e2erole P3B_DB_PASSWORD=e2eiso_2026 \\
P3B_ADMIN_PASSWORD='P3bgap_2026!' P3B_ADMIN_NEW_PASSWORD='P3bgap_2026!x' \\
  python3 scripts/verify/pflow3b_gap_e2e.py
```

退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足。
"""

from __future__ import annotations

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

# 复用主 e2e 的夹具/登录/清理助手（同一套真链路，不另造一份）
import pflow3b_finance_approval_e2e as p3b  # noqa: E402  (必须在 env 就绪后导入)

check = p3b.check
call = p3b.call
psql = p3b.psql

REASON_PAYABLE = "P-FLOW-3B 补漏：供应商发票开错"
REASON_VOUCHER = "P-FLOW-3B 补漏：凭证金额录错"


def one(sql: str) -> str:
    return psql(sql).strip()


def latest_payable() -> tuple[str, str]:
    raw = one(
        "SELECT id::text || '~' || payable_no FROM biz_payable "
        f"WHERE supplier_id = '{p3b.SUPPLIER_ID}'::uuid AND deleted_at IS NULL "
        "ORDER BY created_at DESC LIMIT 1"
    )
    pid, pno = raw.split("~")
    return pid, pno


def instance_no(instance_id: str) -> str:
    return one(f"SELECT instance_no FROM biz_flow_instance WHERE id = '{instance_id}'::uuid")


def payment_info(token: str, payable_id: str) -> dict:
    status, body = call("GET", f"/api/business/erp/payable/{payable_id}/payment-info", token=token)
    if status != 200:
        raise SystemExit(f"payment-info 失败：{status} {body}")
    return body["data"]["payable"]


def main() -> int:
    print(f"== P-FLOW-3B 补漏真机验收 @ {p3b.BASE}（库 {p3b.DB_NAME}）==")
    if call("GET", "/health")[0] != 200:
        print("环境不满足：/health 不是 200")
        return 2

    token = p3b.admin_token()
    p3b.setup_fixtures()
    p3b.cleanup()

    # 审批人必须真实存在：`payable_cancel` / `voucher_reverse` 的审批节点是 ROLE=FINANCE_MANAGER，
    # 角色里没人时回退超管，而发起人（admin）会被内控闸排除 → 节点无人可派 → 实例立即走完
    # （实测「审批单已结束（APPROVED），不能撤回」）。建两个审批账号让审批停在 PENDING。
    p3b.ensure_approver(token, p3b.FM_USER, "P3B 财务经理", p3b.ROLE_FM)
    p3b.ensure_approver(token, p3b.FD_USER, "P3B 财务总监", p3b.ROLE_FD)

    # ---------- 夹具：真收货过账 → 自动应付（OPEN，未付 800）----------
    p3b.post_stock_in(token)
    payable_id, payable_no = latest_payable()
    print(f"夹具应付单：{payable_no}（{payable_id}）")

    # ---------- ③ 对照：作废前 can_pay 必须是 true（证明闸是「新增」而不是恒 false）----------
    info_open = payment_info(token, payable_id)
    check(
        info_open["can_pay"] is True,
        "③ 对照：OPEN 应付单 can_pay=true（闸不是恒 false）",
        f"can_pay={info_open['can_pay']} blocked={info_open.get('blocked_reason')}",
    )
    check(
        info_open.get("blocked_reason") is None,
        "③ 对照：OPEN 应付单 blocked_reason 为 null",
        repr(info_open.get("blocked_reason")),
    )
    check(
        float(info_open["default_amount"]) == float(info_open["unpaid_amount"]) > 0,
        "③ 对照：OPEN 应付单 default_amount = 未付金额",
        f"default={info_open['default_amount']} unpaid={info_open['unpaid_amount']}",
    )

    # ---------- 提交作废审批 → PENDING_CANCEL ----------
    status, body = call(
        "POST",
        f"/api/business/erp/payable/{payable_id}/submit-cancel",
        {"reason": REASON_PAYABLE},
        token,
    )
    data = (body.get("data") or {}) if isinstance(body, dict) else {}
    check(status == 200, "提交作废审批 200", f"{status} {body}")
    check(
        data.get("outcome") == "PENDING_APPROVAL",
        "提交作废 outcome=PENDING_APPROVAL（账未动）",
        str(data.get("outcome")),
    )
    cancel_instance = one(
        f"SELECT coalesce(cancel_instance_id::text,'') FROM biz_payable WHERE id = '{payable_id}'::uuid"
    )
    cancel_no = instance_no(cancel_instance) if cancel_instance else ""
    print(f"作废审批实例：{cancel_no}（{cancel_instance}）")

    # ---------- ④ 详情 / 列表都下发 cancel_instance_id + cancel_reason ----------
    status, body = call("GET", f"/api/business/erp/payable/{payable_id}", token=token)
    detail = (body.get("data") or {}) if status == 200 else {}
    check(status == 200, "④ 应付详情 200", f"{status}")
    check(
        "cancel_instance_id" in detail,
        "④ 详情含 cancel_instance_id 键（字段必须存在，null 也算）",
        f"keys={sorted(k for k in detail if 'cancel' in k)}",
    )
    check(
        detail.get("cancel_instance_id") == cancel_instance,
        "④ 详情 cancel_instance_id == 库里的审批实例",
        f"api={detail.get('cancel_instance_id')} db={cancel_instance}",
    )
    check(
        detail.get("cancel_reason") == REASON_PAYABLE,
        "④ 详情 cancel_reason == 送审时填的原因",
        repr(detail.get("cancel_reason")),
    )
    check(detail.get("status") == "PENDING_CANCEL", "④ 详情 status=PENDING_CANCEL", str(detail.get("status")))

    status, body = call(
        "GET",
        f"/api/business/erp/payable/page?page=1&page_size=50&payable_no={payable_no}",
        token=token,
    )
    rows = ((body.get("data") or {}).get("list") or []) if status == 200 else []
    row = next((r for r in rows if r.get("id") == payable_id), None)
    check(status == 200 and row is not None, "④ 列表能查到该应付单", f"{status} rows={len(rows)}")
    if row:
        check(
            row.get("cancel_instance_id") == cancel_instance,
            "④ 列表行 cancel_instance_id 一致（重开抽屉后靠它显示进度）",
            f"api={row.get('cancel_instance_id')}",
        )
        check(
            row.get("cancel_reason") == REASON_PAYABLE,
            "④ 列表行 cancel_reason 一致",
            repr(row.get("cancel_reason")),
        )

    # ---------- ③ 作废审批中：can_pay=false + blocked_reason 带审批单号 ----------
    info_pending = payment_info(token, payable_id)
    check(
        info_pending["can_pay"] is False,
        "③ 作废审批中 can_pay=false（补漏前这里是 true）",
        f"can_pay={info_pending['can_pay']}",
    )
    blocked = info_pending.get("blocked_reason") or ""
    check("正在作废审批中" in blocked, "③ blocked_reason 点明「正在作废审批中」", blocked)
    check(
        bool(cancel_no) and cancel_no in blocked,
        "③ blocked_reason 带审批单号（用户据此去撤回）",
        f"blocked={blocked} instance_no={cancel_no}",
    )
    check(payable_no in blocked, "③ blocked_reason 带应付单号", blocked)
    check(
        float(info_pending["default_amount"]) == 0.0,
        "③ 作废审批中 default_amount=0（不再预填未付金额）",
        str(info_pending["default_amount"]),
    )
    check(
        float(info_pending["unpaid_amount"]) > 0,
        "③ 未付金额照实下发（只拦「可付」，账上还没作废）",
        str(info_pending["unpaid_amount"]),
    )

    # ---------- 撤回，恢复 OPEN（顺带证明 ④ 的字段会被清空）----------
    status, body = call(
        "POST", f"/api/business/erp/payable/{payable_id}/withdraw-cancel", {}, token
    )
    check(status == 200, "撤回作废审批 200", f"{status} {body}")
    info_back = payment_info(token, payable_id)
    check(info_back["can_pay"] is True, "撤回后 can_pay 回到 true", str(info_back["can_pay"]))
    status, body = call("GET", f"/api/business/erp/payable/{payable_id}", token=token)
    after = (body.get("data") or {}) if status == 200 else {}
    check(
        "cancel_instance_id" in after and after.get("cancel_instance_id") is None,
        "撤回后 cancel_instance_id 为 null（键仍在）",
        str(after.get("cancel_instance_id")),
    )

    # ---------- ① 凭证：送审后三处都要读到 pending 字段 ----------
    voucher_id = p3b.create_voucher(token, 800)
    status, body = call(
        "POST",
        f"/api/business/erp/voucher/{voucher_id}/submit-reverse",
        {"reason": REASON_VOUCHER},
        token,
    )
    submit = (body.get("data") or {}) if status == 200 else {}
    check(status == 200, "① 凭证送审 200", f"{status} {body}")
    check(
        submit.get("outcome") == "PENDING_APPROVAL",
        "① 送审 outcome=PENDING_APPROVAL",
        str(submit.get("outcome")),
    )
    rev_instance = one(
        "SELECT coalesce(pending_reverse_instance_id::text,'') FROM biz_voucher "
        f"WHERE id = '{voucher_id}'::uuid"
    )
    check(
        submit.get("pending_reverse_instance_id") == rev_instance and rev_instance != "",
        "① `ReverseSubmitRes`（flatten 单头）带 pending_reverse_instance_id",
        f"api={submit.get('pending_reverse_instance_id')} db={rev_instance}",
    )
    check(
        submit.get("reverse_reason") == REASON_VOUCHER,
        "① `ReverseSubmitRes` 带 reverse_reason",
        repr(submit.get("reverse_reason")),
    )

    status, body = call("GET", f"/api/business/erp/voucher/{voucher_id}", token=token)
    vdetail = (body.get("data") or {}) if status == 200 else {}
    check(status == 200, "① 凭证详情 200", f"{status}")
    check(
        "pending_reverse_instance_id" in vdetail,
        "① 详情含 pending_reverse_instance_id 键",
        f"keys={sorted(k for k in vdetail if 'reverse' in k)}",
    )
    check(
        vdetail.get("pending_reverse_instance_id") == rev_instance and rev_instance != "",
        "① 详情 pending_reverse_instance_id == 库值（前端据此打「红冲审批中」）",
        f"api={vdetail.get('pending_reverse_instance_id')} db={rev_instance}",
    )
    check(
        vdetail.get("reverse_reason") == REASON_VOUCHER,
        "① 详情 reverse_reason == 送审原因",
        repr(vdetail.get("reverse_reason")),
    )
    check(
        vdetail.get("status") == "POSTED",
        "① 送审后 status 仍是 POSTED（报表口径不变）",
        str(vdetail.get("status")),
    )

    vno = one(f"SELECT voucher_no FROM biz_voucher WHERE id = '{voucher_id}'::uuid")
    status, body = call(
        "GET",
        f"/api/business/erp/voucher/page?page=1&page_size=50&voucher_no={vno}",
        token=token,
    )
    vrows = ((body.get("data") or {}).get("list") or []) if status == 200 else []
    vrow = next((r for r in vrows if r.get("id") == voucher_id), None)
    check(status == 200 and vrow is not None, "① 凭证列表能查到该凭证", f"{status} vno={vno} rows={len(vrows)}")
    if vrow:
        check(
            "pending_reverse_instance_id" in vrow and vrow.get("pending_reverse_instance_id") == rev_instance,
            "① 列表行 pending_reverse_instance_id（前端兜底请求自动失效的判据）",
            f"api={vrow.get('pending_reverse_instance_id')}",
        )
        check(
            "reverse_reason" in vrow and vrow.get("reverse_reason") == REASON_VOUCHER,
            "① 列表行 reverse_reason",
            repr(vrow.get("reverse_reason")),
        )
        check(
            vrow.get("status") == "POSTED",
            "① 列表行 status 仍 POSTED",
            str(vrow.get("status")),
        )

    # ---------- 撤回红冲审批，恢复干净状态 ----------
    status, body = call(
        "POST", f"/api/business/erp/voucher/{voucher_id}/withdraw-reverse", {}, token
    )
    check(status == 200, "撤回红冲审批 200", f"{status} {body}")
    status, body = call("GET", f"/api/business/erp/voucher/{voucher_id}", token=token)
    vback = (body.get("data") or {}) if status == 200 else {}
    check(
        "pending_reverse_instance_id" in vback and vback.get("pending_reverse_instance_id") is None,
        "① 撤回后 pending_reverse_instance_id 为 null（键仍在）",
        str(vback.get("pending_reverse_instance_id")),
    )
    check(
        vback.get("status") == "POSTED" and bool(vback.get("reversed_by")) is False,
        "① 撤回后未生成红字（reversed_by 仍为空）",
        f"status={vback.get('status')} reversed_by={vback.get('reversed_by')}",
    )

    if os.environ.get("P3B_KEEP") != "1":
        # 手工凭证没有 source_no（清理助手按 `source_no LIKE 'P3BE2E-%'` 删不到），
        # 本脚本自己按 id + 摘要前缀收干净，避免给后续全量单测/报表口径留脏行。
        psql(
            "DELETE FROM biz_status_log WHERE entity_id IN "
            f"(SELECT id FROM biz_voucher WHERE id = '{voucher_id}'::uuid OR summary LIKE 'P3B 真机红冲%');"
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN "
            f"(SELECT id FROM biz_voucher WHERE id = '{voucher_id}'::uuid OR summary LIKE 'P3B 真机红冲%');"
            "DELETE FROM biz_voucher WHERE id = '"
            f"{voucher_id}'::uuid OR summary LIKE 'P3B 真机红冲%';"
        )
        p3b.cleanup()
        # 主数据夹具（科目 / 单位 / 供应商 / 仓库 / 物料）清理助手不管，必须自己清：
        # 留下的 `P3BE2E.1002/2202` 两个科目会让 `cash_flow::r3_real_config_covers_all_accounts`
        # （按「全部科目」断言分类配置覆盖）在全量套件里变红 —— 父卡 P-FLOW-3B 踩过同一个坑。
        psql(
            "DELETE FROM biz_opening_balance WHERE account_id IN "
            "(SELECT id FROM biz_account WHERE account_code LIKE 'P3BE2E.%');"
            "DELETE FROM biz_opening_inventory WHERE material_id IN "
            "(SELECT id FROM biz_material WHERE material_code LIKE 'P3BE2E-%');"
            "DELETE FROM biz_account WHERE account_code LIKE 'P3BE2E.%';"
            "DELETE FROM biz_material WHERE material_code LIKE 'P3BE2E-%';"
            "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'P3BE2E-%';"
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'P3BE2E-%';"
            "DELETE FROM biz_unit WHERE unit_code LIKE 'P3BE2E-%';"
        )
        residue = one(
            "SELECT (SELECT count(*) FROM biz_voucher WHERE summary LIKE 'P3B 真机红冲%') || '~' || "
            "(SELECT count(*) FROM biz_payable WHERE supplier_id = "
            f"'{p3b.SUPPLIER_ID}'::uuid) || '~' || "
            "(SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('PAYABLE_CANCEL','VOUCHER_REVERSE'))"
            " || '~' || (SELECT count(*) FROM biz_account WHERE account_code LIKE 'P3BE2E.%')"
        )
        check(residue == "0~0~0~0", "夹具清场：凭证/应付/实例/科目残留全为 0", residue)

    total = len(p3b.PASSED) + len(p3b.FAILED)
    print()
    print(f"== 汇总：PASS {len(p3b.PASSED)} / FAIL {len(p3b.FAILED)}（共 {total} 条）==")
    for item in p3b.FAILED:
        print(f"  [FAIL] {item}")
    return 0 if not p3b.FAILED else 1


if __name__ == "__main__":
    raise SystemExit(main())
