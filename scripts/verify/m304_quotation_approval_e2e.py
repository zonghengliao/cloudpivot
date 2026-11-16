"""M3-04 报价单审批流接线 E2E —— 真接口断言（打本机隔离实例）。

卡：t_6bede75a（M3-04【M3·销售】报价单：审批流接线）

链路（全部走真 HTTP + 真库 + 真权限；审批人不是超管，是**真角色 + 真待办**）：

```text
夹具：3 个账号（制单员 / 销售主管 / 财务经理，密码 = admin 改后的口令）+ 客户 + 物料
   ↓
① 提交审批（空 body）→ 200：单据**仍是 DRAFT**，approval.status=PENDING，instance_no=FLW-…
   ↓
② 审批中三条闸：PATCH status=SENT 409 / PUT 409 / DELETE 409 / 重复提交 409
   ↓
③ 内控：制单员自己也在销售主管角色里，但他的待办为 0（落 SKIPPED 留痕），
   销售主管的待办恰好 1 条（同一条实例）
   ↓
④ 销售主管审批通过 → 回调**自动发出**：单据 SENT，approval.status=APPROVED
   ↓
⑤ 大额（≥ 10 万）两档：销售主管批完仍是 DRAFT（等财务），财务批完才 SENT
   ↓
⑥ 驳回：单据留在 DRAFT，可改、可重提
   ↓
⑦ 作废撤审批：DRAFT→CANCELLED 同一事务撤掉在跑实例，待办清零
   ↓
⑧ 没配流程：默认 409（66033）→ 带 skip 但理由太短 400 → 持 skip 权限 + 理由 ≥10 字 → 直接发出
   ↓
⑨ 已发出后再提交审批 → 409（只有草稿能提）
```

口径（不是 bug）：
  · **审批不是状态**：`biz_quotation.status` 五值域一个字没改，审批中单据仍是 `DRAFT`，
    「已提交待批」只能读 `approval` 段 / `approval_instance_id`；
  · **通过即发出**：审批通过回调直接走 `DRAFT → SENT`（与人工发出同一条代码路径）；
  · 驳回 / 撤回 / 终止 → 留在 `DRAFT`（价格一分没对外，可改完再提）。

用法：
    python3 scripts/verify/m304_quotation_approval_e2e.py
环境变量：M304_API_BASE（默认 http://127.0.0.1:18232/api）、
         M304_ADMIN_OLD_PASSWORD（默认 Admin@123456）、M304_ADMIN_PASSWORD（改后口令）、
         M304_USER_PASSWORD（三个夹具账号的口令，默认同 M304_ADMIN_PASSWORD）、
         以及 psql 侧：PGHOST / PGPORT / PGUSER / PGDATABASE / PGPASSWORD
"""

import datetime
import json
import os
import subprocess
import urllib.error
import urllib.request

BASE = os.environ.get("M304_API_BASE", "http://127.0.0.1:18232/api")
ADMIN_OLD = os.environ.get("M304_ADMIN_OLD_PASSWORD", "Admin@123456")
ADMIN_NEW = os.environ.get("M304_ADMIN_PASSWORD", "M304@Approve1")
USER_PASSWORD = os.environ.get("M304_USER_PASSWORD", ADMIN_NEW)

PASS = 0
FAIL = 0
TOKENS = {}


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS {name}")
    else:
        FAIL += 1
        print(f"  FAIL {name} {detail}")


def eq(name, expected, actual):
    check(name, expected == actual, f"（期望 [{expected}]，实际 [{actual}]）")


def num(name, expected, actual, places=4):
    try:
        ok = abs(float(actual) - float(expected)) < 10 ** (-places) / 2
    except (TypeError, ValueError):
        ok = False
    check(name, ok, f"（期望 {expected}，实际 [{actual}]）")


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, raw


def ok_data(method, path, body=None, token=None):
    status, resp = call(method, path, body, token)
    assert status == 200 and isinstance(resp, dict) and resp.get("code") == 0, \
        f"{method} {path} 失败：{status} {resp}"
    return resp["data"]


def err_of(method, path, body=None, token=None):
    status, resp = call(method, path, body, token)
    if not isinstance(resp, dict):
        return status, None, str(resp)
    return status, resp.get("code"), resp.get("msg") or resp.get("message") or ""


def psql(sql):
    """只读/低风险 SQL（夹具断言 + 把流程下线再上线），走 psql 客户端。"""
    env = dict(os.environ)
    cmd = ["psql", "-h", os.environ.get("PGHOST", "172.18.10.145"),
           "-p", os.environ.get("PGPORT", "5432"),
           "-U", os.environ.get("PGUSER", "cloudpivot"),
           "-d", os.environ["PGDATABASE"], "-tAc", sql]
    done = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=60)
    if done.returncode != 0:
        raise RuntimeError(f"psql 失败：{done.stderr}")
    return done.stdout.strip()


def sync_fixture_passwords():
    """三个夹具账号的口令同步成 admin 改密后的哈希（夹具账号由 shell 用 admin 的旧哈希建出来）。

    改密是走真接口做的（`PUT /system/auth/password`），所以只能在这之后用 SQL 把哈希复制过去，
    并把 `must_change_password` 清掉 —— 否则夹具账号一登就被强制改密，测的就不是审批。
    """
    psql(
        "UPDATE sys_user SET password = (SELECT password FROM sys_user WHERE username='admin'), "
        "must_change_password = false, status = 1, is_locked = false, lock_time = null, "
        "login_fail_count = 0 WHERE username IN ('m304_maker','m304_sales','m304_fin')"
    )


def login(username, password):
    status, body = call("POST", "/system/auth/login", {"username": username, "password": password})
    assert status == 200 and isinstance(body, dict) and body.get("code") == 0, \
        f"{username} 登录失败：{status} {body}"
    return body["data"]["access_token"]


def login_admin():
    status, body = call("POST", "/system/auth/login", {"username": "admin", "password": ADMIN_OLD})
    if status == 200 and isinstance(body, dict) and body.get("code") == 0:
        token = body["data"]["access_token"]
        check("空库首启：admin 首登 must_change_password=true", bool(body["data"].get("must_change_password")))
        status, changed = call("PUT", "/system/auth/password",
                               {"old_password": ADMIN_OLD, "new_password": ADMIN_NEW}, token)
        eq("首次登录改密", 0, changed.get("code") if isinstance(changed, dict) else changed)
        return login("admin", ADMIN_NEW)
    return login("admin", ADMIN_NEW)


def today():
    return datetime.date.today()


def make_quotation(token, customer_id, material_id, price, qty="1", tax_rate="13", remark="M3-04 E2E"):
    body = {
        "customer_id": customer_id,
        "valid_until": (today() + datetime.timedelta(days=30)).isoformat(),
        "remark": remark,
        "items": [{"material_id": material_id, "qty": qty, "price": price, "tax_rate": tax_rate}],
    }
    return ok_data("POST", "/business/erp/quotation", body, token)


def submit(token, quotation_id, body=None):
    return ok_data("POST", f"/business/erp/quotation/{quotation_id}/submit-approval", body, token)


def todo_of(token, quote_no):
    data = ok_data("GET", "/business/flow/task/todo?page=1&page_size=50", token=token)
    return [item for item in data["list"] if item.get("biz_no") == quote_no]


def approve(token, task_id, comment="同意"):
    return ok_data("POST", f"/business/flow/task/{task_id}/approve", {"comment": comment}, token)


def reject(token, task_id, comment="价格偏低，请复核折扣", return_to="PREV"):
    return ok_data("POST", f"/business/flow/task/{task_id}/reject",
                   {"comment": comment, "return_to": return_to}, token)


def main():
    admin = login_admin()
    sync_fixture_passwords()
    maker = login("m304_maker", USER_PASSWORD)
    sales = login("m304_sales", USER_PASSWORD)
    fin = login("m304_fin", USER_PASSWORD)
    check("三个夹具账号都登录成功（制单员 / 销售主管 / 财务经理）", bool(maker and sales and fin))

    # ------------------------------------------------------------ ⓪ 夹具
    print("\n⓪ 夹具：客户 + 物料")
    customer = ok_data("POST", "/business/erp/customer", {"name": "M304 审批演练客户", "remark": "M3-04 E2E"}, admin)
    units = ok_data("GET", "/business/erp/unit/page?page=1&page_size=1", token=admin)
    material = ok_data("POST", "/business/erp/material", {
        "material_name": "M304 审批演练物料",
        "unit_id": units["list"][0]["id"],
        "remark": "M3-04 E2E",
    }, admin)
    check("夹具客户 / 物料建出", bool(customer.get("id") and material.get("id")))

    # ------------------------------------------------------------ ① 提交审批
    print("\n① 小额报价单（价税合计 < 10 万）→ 提交审批")
    q1 = make_quotation(maker, customer["id"], material["id"], "100.00")
    quote_no = q1["quote_no"]
    num("报价单价税合计 = 113", 113.0, q1["total_with_tax"])
    eq("建单是草稿", "DRAFT", q1["status"])
    eq("建单还没审批实例", None, q1["approval_instance_id"])

    q1 = submit(maker, q1["id"])
    eq("提交审批后单据**仍是草稿**（审批不是状态）", "DRAFT", q1["status"])
    approval = q1.get("approval") or {}
    eq("审批实例状态 = PENDING", "PENDING", approval.get("status"))
    eq("审批实例状态中文名", "审批中", approval.get("status_label"))
    check("审批单号形态 FLW-…", str(approval.get("instance_no", "")).startswith("FLW-"),
          str(approval.get("instance_no")))
    check("单头 approval_instance_id 已回写", bool(q1.get("approval_instance_id")), str(q1.get("approval_instance_id")))
    check("详情 approval.pending_tasks = 1（销售主管那一票）", approval.get("pending_tasks") == 1,
          str(approval.get("pending_tasks")))
    q1_instance_id = approval.get("instance_id")

    # 列表页也能看到「有审批实例」（防 N+1 的指针列）
    page = ok_data("GET", f"/business/erp/quotation/page?quote_no={quote_no}", token=maker)
    row = next((item for item in page["list"] if item["quote_no"] == quote_no), None)
    check("列表行带 approval_instance_id（列表不必逐行回查实例）",
          bool(row and row.get("approval_instance_id")), str(row and row.get("approval_instance_id")))
    eq("列表行不返审批摘要（防 N+1，详情才有）", None, (row or {}).get("approval"))

    # ------------------------------------------------------------ ② 审批中的四条闸
    print("\n② 审批中的闸：改状态 / 改单 / 删单 / 重复提交 全部 409")
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{q1['id']}/status",
                               {"target_status": "SENT"}, maker)
    eq("审批中 PATCH status=SENT → 409", 409, status)
    check("报文点名审批单号", "已在审批中" in (msg or ""), (msg or "")[:120])

    status, code, msg = err_of("PUT", f"/business/erp/quotation/{q1['id']}", {"remark": "审批中偷偷改"}, maker)
    eq("审批中改单 → 409", 409, status)
    check("改单报文说明为什么不能改", "审批中" in (msg or ""), (msg or "")[:120])

    status, code, msg = err_of("DELETE", f"/business/erp/quotation/{q1['id']}", None, maker)
    eq("审批中删单 → 409", 409, status)

    status, code, msg = err_of("POST", f"/business/erp/quotation/{q1['id']}/submit-approval", None, maker)
    eq("重复提交审批 → 409", 409, status)

    # ------------------------------------------------------------ ③ 内控 + 待办归属
    print("\n③ 内控：制单人不可自审（制单员也在销售主管角色里，但拿不到这条待办）")
    mine = todo_of(sales, quote_no)
    eq("销售主管的待办恰好 1 条", 1, len(mine))
    maker_todo = todo_of(maker, quote_no)
    eq("制单员自己的待办为 0（被内控排除）", 0, len(maker_todo))
    skipped = psql(
        f"SELECT count(*) FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id "
        f"WHERE t.instance_id = '{q1_instance_id}' AND t.status = 'SKIPPED' AND u.username = 'm304_maker'"
    )
    eq("库里落了 1 行 SKIPPED 留痕（内控可追溯）", "1", skipped)
    skipped_reason = psql(
        f"SELECT coalesce(t.comment,'') FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id "
        f"WHERE t.instance_id = '{q1_instance_id}' AND t.status = 'SKIPPED' AND u.username = 'm304_maker' "
        f"ORDER BY t.created_at DESC LIMIT 1"
    )
    check("SKIPPED 行写明原因（制单人不可自审）", "制单人" in skipped_reason, skipped_reason[:120])

    # ------------------------------------------------------------ ④ 审批通过 → 自动发出
    print("\n④ 销售主管审批通过 → 回调自动发出")
    approve(sales, mine[0]["id"])
    q1 = ok_data("GET", f"/business/erp/quotation/{q1['id']}", token=maker)
    eq("审批通过后单据自动发出（SENT）", "SENT", q1["status"])
    eq("状态中文名 = 已发出", "已发出", q1["status_label"])
    eq("审批实例状态 = APPROVED", "APPROVED", (q1.get("approval") or {}).get("status"))
    reason = psql(
        f"SELECT coalesce(reason,'') FROM biz_status_log WHERE entity_type = 'QUOTATION' "
        f"AND entity_id = '{q1['id']}' AND from_status = 'DRAFT' AND to_status = 'SENT' "
        f"ORDER BY created_at DESC LIMIT 1"
    )
    check("状态日志 reason 写明「审批通过」（含审批单号）",
          "审批通过" in reason, reason[:160])
    check("状态日志 reason 带审批单号 FLW-", reason.startswith("审批通过（FLW-") or "FLW-" in reason, reason[:160])
    eq("发出后没有在跑待办", 0, len(todo_of(sales, quote_no)))

    # ------------------------------------------------------------ ⑤ 大额两档
    print("\n⑤ 大额（价税合计 ≥ 10 万）：销售主管 → 财务经理 两档串行")
    q2 = make_quotation(maker, customer["id"], material["id"], "100000.00")
    q2_no = q2["quote_no"]
    num("大额单价税合计 = 113000", 113000.0, q2["total_with_tax"])
    q2 = submit(maker, q2["id"])
    eq("大额单已提交审批", "PENDING", (q2.get("approval") or {}).get("status"))
    t_sales = todo_of(sales, q2_no)
    eq("第一站：销售主管有 1 条待办", 1, len(t_sales))
    eq("第二站：财务经理此时没有待办（串行）", 0, len(todo_of(fin, q2_no)))
    approve(sales, t_sales[0]["id"])
    q2 = ok_data("GET", f"/business/erp/quotation/{q2['id']}", token=maker)
    eq("销售主管批完：单据**仍未发出**（金额条件边把它送进财务复核）", "DRAFT", q2["status"])
    eq("实例仍是 PENDING", "PENDING", (q2.get("approval") or {}).get("status"))
    t_fin = todo_of(fin, q2_no)
    eq("财务经理拿到第二条待办", 1, len(t_fin))
    approve(fin, t_fin[0]["id"])
    q2 = ok_data("GET", f"/business/erp/quotation/{q2['id']}", token=maker)
    eq("财务批完：自动发出（SENT）", "SENT", q2["status"])
    eq("实例 APPROVED", "APPROVED", (q2.get("approval") or {}).get("status"))

    # ------------------------------------------------------------ ⑥ 驳回（退发起人）
    print("\n⑥ 驳回「退发起人」：单据可改（不锁死）→ 重新提交 → 通过后自动发出")
    q3 = make_quotation(maker, customer["id"], material["id"], "50.00")
    q3_no = q3["quote_no"]
    submit(maker, q3["id"])
    t3 = todo_of(sales, q3_no)
    eq("第三张单的待办在销售主管手里", 1, len(t3))
    reject(sales, t3[0]["id"], return_to="INITIATOR")
    q3 = ok_data("GET", f"/business/erp/quotation/{q3['id']}", token=maker)
    eq("退回发起人后单据**仍是草稿**（价格一分没对外）", "DRAFT", q3["status"])
    eq("实例仍 PENDING（票回到发起人手里，不是驳回终态）", "PENDING",
       (q3.get("approval") or {}).get("status"))
    eq("退回后单据**可改**（不被「审批中」锁死）", True, q3["editable"])
    edited = ok_data("PUT", f"/business/erp/quotation/{q3['id']}", {"remark": "按驳回意见改价"}, maker)
    eq("改单成功（退回发起人状态下必须能改）", "按驳回意见改价", edited["remark"])

    status, code, msg = err_of("POST", f"/business/erp/quotation/{q3['id']}/submit-approval", None, maker)
    eq("退回状态下再走「提交审批」→ 409（不是另起一轮）", 409, status)
    check("报文指向「重新提交」", "重新提交" in (msg or ""), (msg or "")[:160])

    mine_resubmit = todo_of(maker, q3_no)
    eq("发起人手里有 1 条「重新提交」待办", 1, len(mine_resubmit))
    eq("这条待办种类是 RESUBMIT", "RESUBMIT", mine_resubmit[0].get("task_kind"))
    approve(maker, mine_resubmit[0]["id"], comment="已按意见改价，重新提交")
    t3b = todo_of(sales, q3_no)
    eq("重新提交后票回到销售主管", 1, len(t3b))
    approve(sales, t3b[0]["id"])
    q3 = ok_data("GET", f"/business/erp/quotation/{q3['id']}", token=maker)
    eq("这一轮批完自动发出（SENT）", "SENT", q3["status"])
    eq("实例 APPROVED", "APPROVED", (q3.get("approval") or {}).get("status"))

    # ------------------------------------------------------------ ⑥' 终态驳回
    print("\n⑥' 终态驳回（return_to=NONE）：实例 REJECTED，单据留草稿，可改可重提")
    q6 = make_quotation(maker, customer["id"], material["id"], "60.00")
    q6_no = q6["quote_no"]
    submit(maker, q6["id"])
    t6 = todo_of(sales, q6_no)
    eq("待办在销售主管手里", 1, len(t6))
    reject(sales, t6[0]["id"], return_to="NONE", comment="报价口径整体不对，作废重报")
    q6 = ok_data("GET", f"/business/erp/quotation/{q6['id']}", token=maker)
    eq("终态驳回：单据仍是草稿", "DRAFT", q6["status"])
    eq("终态驳回：实例 REJECTED", "REJECTED", (q6.get("approval") or {}).get("status"))
    eq("终态驳回后单据可改", True, q6["editable"])
    edited6 = ok_data("PUT", f"/business/erp/quotation/{q6['id']}", {"remark": "整单重报"}, maker)
    eq("改单成功", "整单重报", edited6["remark"])
    rejected_instance = (q6.get("approval") or {}).get("instance_id")
    q6 = submit(maker, q6["id"])
    eq("可再提交一轮（新实例）", "PENDING", (q6.get("approval") or {}).get("status"))
    check("新一轮实例 id 与已驳回那条不同",
          (q6.get("approval") or {}).get("instance_id") != rejected_instance,
          f"{rejected_instance} → {(q6.get('approval') or {}).get('instance_id')}")

    # ------------------------------------------------------------ ⑦ 作废撤审批
    print("\n⑦ 作废（DRAFT→CANCELLED）同一事务撤掉在跑审批，不留孤儿待办")
    q7_no = q6_no
    status, code, msg = err_of("PATCH", f"/business/erp/quotation/{q6['id']}/status",
                               {"target_status": "CANCELLED", "reason": "客户改需求，本单先作废"}, maker)
    eq("审批中作废 → 200（作废是审批中唯一允许的状态流转）", 200, status)
    cand = ok_data("GET", f"/business/erp/quotation/{q6['id']}", token=maker)
    eq("单据已作废", "CANCELLED", cand["status"])
    eq("在跑实例被同一事务终止（CANCELLED）", "CANCELLED", (cand.get("approval") or {}).get("status"))
    eq("作废后没有孤儿待办（销售主管）", 0, len(todo_of(sales, q7_no)))

    # ------------------------------------------------------------ ⑧ 没配流程：默认拒绝 + 显式跳过
    print("\n⑧ 没配流程：默认 409（66033）→ 理由太短 400 → 持 skip 权限 + 长理由 → 直接发出")
    psql("UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'quotation_approve'")
    try:
        q4 = make_quotation(maker, customer["id"], material["id"], "10.00")
        status, code, msg = err_of("POST", f"/business/erp/quotation/{q4['id']}/submit-approval", None, maker)
        eq("没配流程 + 不请求跳过 → 409", 409, status)
        eq("错误码 = 66033（审批流未配置，fail-closed）", 66033, code)
        check("报文点名流程编码 quotation_approve", "quotation_approve" in (msg or ""), (msg or "")[:160])

        status, code, msg = err_of("POST", f"/business/erp/quotation/{q4['id']}/submit-approval",
                                   {"skip_approval": True, "skip_reason": "太短"}, maker)
        eq("跳过但理由太短 → 400", 400, status)

        q4 = submit(maker, q4["id"],
                    {"skip_approval": True, "skip_reason": "客户现场签约，来不及等审批，先报价后补手续"})
        eq("持 skip 权限 + 理由 ≥10 字 → 直接发出", "SENT", q4["status"])
        skip_reason = psql(
            f"SELECT coalesce(reason,'') FROM biz_status_log WHERE entity_type = 'QUOTATION' "
            f"AND entity_id = '{q4['id']}' AND to_status = 'SENT' ORDER BY created_at DESC LIMIT 1"
        )
        check("跳过理由写进状态日志（可追溯）", "跳过审批" in skip_reason, skip_reason[:160])
    finally:
        psql("UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'quotation_approve'")

    # 流程恢复后：配了流程时不许带 skip 绕过
    print("\n⑧' 配了流程时，带 skip_approval 一律 400（不许绕过已配置的内控）")
    q5 = make_quotation(maker, customer["id"], material["id"], "20.00")
    status, code, msg = err_of("POST", f"/business/erp/quotation/{q5['id']}/submit-approval",
                               {"skip_approval": True, "skip_reason": "我就是不想走审批，这是一段足够长的理由"}, maker)
    eq("有流程时请求跳过 → 400", 400, status)

    # ------------------------------------------------------------ ⑨ 已发出的不能再提审批
    print("\n⑨ 已发出的报价单不能再提交审批")
    status, code, msg = err_of("POST", f"/business/erp/quotation/{q1['id']}/submit-approval", None, maker)
    eq("已发出（SENT）→ 提交审批 409", 409, status)

    # ------------------------------------------------------------ ⑩ 非法 uuid / 不存在
    print("\n⑩ 入口守卫")
    status, code, msg = err_of("POST", "/business/erp/quotation/not-a-uuid/submit-approval", None, maker)
    eq("非法 uuid → 400", 400, status)
    status, code, msg = err_of("POST",
                               "/business/erp/quotation/6f0a9b1c-0000-4000-8000-000000000000/submit-approval",
                               None, maker)
    eq("不存在的报价单 → 404", 404, status)

    print(f"\n==== M3-04 真接口断言：PASS={PASS} FAIL={FAIL} ====")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
