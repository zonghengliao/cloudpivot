#!/usr/bin/env python3
"""P1-FIX-1 真机验收：收货单作废（stock_in_cancel）走通。

场景（全部走生产的 HTTP 同一路径）：
  A. 非超管制单人发起作废 → PENDING_CANCEL（六本账一分没动）
  B. 仓管主管（WAREHOUSE_MANAGER，非超管）复核通过 → CANCELLED（六本账回滚 + 红字凭证）
  C. 反例①自审：复核人自己发起作废 → 422 + 65019（内控不破），报文点名 WAREHOUSE_MANAGER
  D. 反例②岗位空缺：角色 0 人时发起 → 422 + 65018，报文给配置指引（不兜底超管）
最后打印六本账 / 凭证 / 任务行快照供留档。

用法：python3 p1fix1_cancel_e2e.py <base_url> <db_name> [round]
"""
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8023"
DB = sys.argv[2] if len(sys.argv) > 2 else "cloudpivot_p1fix1_a"
ROUND = sys.argv[3] if len(sys.argv) > 3 else "1"
ADMIN_PW = sys.argv[4] if len(sys.argv) > 4 else "Admin@123456"
ROLE_WH = "20000000-0000-4000-8000-0000000f0004"
U_MAKER = "c1f1f100-0000-4000-8000-000000000001"
U_REVIEWER = "c1f1f100-0000-4000-8000-000000000002"

PASS, FAIL = [], []


def psql(sql: str):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{out.stderr.strip()}\nSQL: {sql}")
    return [line.split("|") for line in out.stdout.strip().splitlines() if line]


def http(method: str, path: str, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def ok(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}{('   ' + str(detail)) if detail else ''}")


def login(username, password=ADMIN_PW):
    st, res = http("POST", "/api/system/auth/login", {"username": username, "password": password})
    assert st == 200 and res.get("code") == 0, f"{username} 登录失败：{st} {res}"
    return res["data"]["access_token"]


def setup_fixtures():
    """制单人（非超管、无复核岗位）+ 复核人（WAREHOUSE_MANAGER，非超管）。"""
    # 制单人角色：只给收货单相关的权限码（照 p9_cancel_e2e 的分工）
    psql(
        "insert into sys_role (id, role_code, role_name, description, status, sort_order) "
        "select 'c1f1f100-0000-4000-8000-0000000000a1'::uuid, 'P1FIX1_MAKER', 'P1FIX1 制单人', "
        "       'P1-FIX-1 夹具：只有收货单权限', 1, 998 "
        " where not exists (select 1 from sys_role where role_code = 'P1FIX1_MAKER')"
    )
    psql(
        "insert into sys_role_menu (role_id, menu_id) "
        "select 'c1f1f100-0000-4000-8000-0000000000a1'::uuid, m.id from sys_menu m "
        " where m.deleted_at is null and m.auth_code like 'business:erp:stock-in%' "
        "   and not exists (select 1 from sys_role_menu x "
        "                    where x.role_id = 'c1f1f100-0000-4000-8000-0000000000a1'::uuid and x.menu_id = m.id)"
    )
    # 两个账号：口令哈希抄 admin（同一口令 Admin@123456），status=1
    psql(
        "insert into sys_user (id, username, password, real_name, status, must_change_password, token_version) "
        "select v.id, v.uname, u.password, v.rname, 1, false, 1 "
        "  from sys_user u, (values "
        f"   ('{U_MAKER}'::uuid, 'p1fix1_maker', 'P1FIX1 制单人'), "
        f"   ('{U_REVIEWER}'::uuid, 'p1fix1_reviewer', 'P1FIX1 复核人')"
        "  ) as v(id, uname, rname) "
        " where u.username = 'admin' "
        "   and not exists (select 1 from sys_user s where lower(s.username) = lower(v.uname))"
    )
    psql(
        "insert into sys_user_role (user_id, role_id) "
        "select 'c1f1f100-0000-4000-8000-000000000001'::uuid, 'c1f1f100-0000-4000-8000-0000000000a1'::uuid "
        " where not exists (select 1 from sys_user_role x "
        "                    where x.user_id = 'c1f1f100-0000-4000-8000-000000000001'::uuid "
        "                      and x.role_id = 'c1f1f100-0000-4000-8000-0000000000a1'::uuid)"
    )
    psql(
        "insert into sys_user_role (user_id, role_id) "
        f"select '{U_REVIEWER}'::uuid, '{ROLE_WH}'::uuid "
        " where not exists (select 1 from sys_user_role x "
        f"                    where x.user_id = '{U_REVIEWER}'::uuid and x.role_id = '{ROLE_WH}'::uuid)"
    )
    # 复核人也要能开收货单页面（反例①要由他来点「作废」，权限层先拦就没意义了）。
    # 真实业务里仓管主管本来就有收货单权限，这里把制单人角色也给他 —— 不影响候选来源
    # （候选只来自 n_review 绑定的 WAREHOUSE_MANAGER 角色）。
    psql(
        "insert into sys_user_role (user_id, role_id) "
        "select 'c1f1f100-0000-4000-8000-000000000002'::uuid, 'c1f1f100-0000-4000-8000-0000000000a1'::uuid "
        " where not exists (select 1 from sys_user_role x "
        "                    where x.user_id = 'c1f1f100-0000-4000-8000-000000000002'::uuid "
        "                      and x.role_id = 'c1f1f100-0000-4000-8000-0000000000a1'::uuid)"
    )


def inv_pair(material_id):
    """(库存数量, 库存金额) —— 用 Decimal 比较（PostgreSQL 的 numeric 文本是 0.0000 这种）。"""
    from decimal import Decimal

    qty, amount = inventory_of(material_id)
    return Decimal(qty), Decimal(amount)


def inventory_of(material_id):
    rows = psql(
        "select coalesce(sum(qty), 0), coalesce(sum(amount), 0) from biz_inventory "
        f" where material_id = '{material_id}'::uuid"
    )
    return rows[0][0], rows[0][1]


def ledger_snapshot(tag, material_id):
    from decimal import Decimal

    qty, amount = inventory_of(material_id)
    vouchers = psql("select count(*), coalesce(sum(case when is_red = 1 then 1 else 0 end), 0) from biz_voucher")[0]
    txns = psql("select count(*) from biz_inventory_txn")[0][0]
    print(f"  · {tag}: 库存 qty={qty} amount={amount} ｜ 凭证 {vouchers[0]}（红字 {vouchers[1]}）｜ 库存流水 {txns}")
    return Decimal(qty), Decimal(amount), vouchers[0], vouchers[1]


def main():
    print(f"=== P1-FIX-1 真机作废链路（round {ROUND}）base={BASE} db={DB} ===")
    setup_fixtures()
    mat, wh, sup = (
        psql("select id from biz_material where deleted_at is null order by created_at limit 1")[0][0],
        psql("select id from biz_warehouse where deleted_at is null order by created_at limit 1")[0][0],
        psql("select id from biz_supplier where deleted_at is null order by created_at limit 1")[0][0],
    )
    token_maker = login("p1fix1_maker")
    token_reviewer = login("p1fix1_reviewer")
    token_admin = login("admin")
    print(f"  · 三个账号登录成功（制单人/复核人/管理员）；物料={mat[:8]}… 仓库={wh[:8]}…")

    print("\n## A. 非超管制单人发起作废（POSTED → PENDING_CANCEL）")
    before = ledger_snapshot("作废前", mat)
    st, res = http("POST", "/api/business/erp/stock-in", {
        "supplier_id": sup, "warehouse_id": wh, "is_sourceless": True,
        "remark": f"P1-FIX-1 真机 round{ROUND}",
        "items": [{"material_id": mat, "qty": "4", "price_excl": "10", "tax_rate": "13"}],
    }, token_maker)
    ok("制单人建单", st == 200 and res.get("code") == 0, f"{st} {res.get('msg')}")
    doc_id = res["data"]["header"]["id"]
    in_no = res["data"]["header"]["in_no"]
    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"}, token_maker)
    ok("制单人过账", st == 200 and res.get("code") == 0, f"{st} {res.get('msg')} {in_no}")
    posted = ledger_snapshot("过账后", mat)
    from decimal import Decimal

    ok("过账使库存增加 4", posted[0] - before[0] == Decimal("4"), f"{before[0]} → {posted[0]}")

    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                   {"status": "CANCELLED", "reason": "P1-FIX-1：供应商召回，整批退回"}, token_maker)
    ok("非超管制单人发起作废 → 200（修复前这里是 422 自审禁止）", st == 200 and res.get("code") == 0,
       f"{st} {res.get('msg')}")
    status = psql(f"select status from biz_stock_in where id = '{doc_id}'::uuid")[0][0]
    ok("单据进入「作废审批中」", status == "PENDING_CANCEL", status)
    after_submit = ledger_snapshot("发起作废后（账不许动）", mat)
    ok("发起作废不动账", after_submit[:4] == posted[:4])
    rows = psql(
        "select t.status, u.username from biz_flow_task t join sys_user u on u.id = t.assignee_id "
        f" where t.instance_id = (select approval_instance_id from biz_stock_in where id = '{doc_id}'::uuid)"
    )
    ok("待办恰好 1 条且落在仓管主管（复核人）头上",
       len(rows) == 1 and rows[0][0] == "PENDING" and rows[0][1] == "p1fix1_reviewer", rows)

    print("\n## B. 仓管主管复核通过 → CANCELLED（六本账回滚）")
    instance_id = psql(
        "select approval_instance_id from biz_stock_in "
        f" where id = '{doc_id}'::uuid"
    )[0][0]
    task_id = psql(
        "select t.id from biz_flow_task t "
        f" where t.instance_id = '{instance_id}'::uuid and t.status = 'PENDING'"
    )[0][0]
    st, res = http("POST", f"/api/business/flow/task/{task_id}/approve",
                   {"comment": "复核通过（P1-FIX-1 真机）"}, token_reviewer)
    ok("复核人审批通过", st == 200 and res.get("code") == 0, f"{st} {res.get('msg')}")
    status = psql(f"select status from biz_stock_in where id = '{doc_id}'::uuid")[0][0]
    ok("单据落 CANCELLED", status == "CANCELLED", status)
    cancelled = ledger_snapshot("作废后（回滚 + 红冲）", mat)
    ok("库存回到作废前", cancelled[:2] == before[:2], f"{before[:2]} vs {cancelled[:2]}")
    ok("凭证红冲出红字", int(cancelled[3]) > int(after_submit[3]), f"红字 {after_submit[3]} → {cancelled[3]}")
    inst = psql(f"select status from biz_flow_instance where id = '{instance_id}'::uuid")
    ok("审批单落 APPROVED", bool(inst) and inst[0][0] == "APPROVED", inst)

    print("\n## C. 反例①：复核人自己发起作废 → 422 + 65019（自审仍被拒）")
    st, res = http("POST", "/api/business/erp/stock-in", {
        "supplier_id": sup, "warehouse_id": wh, "is_sourceless": True,
        "remark": f"P1-FIX-1 自审反例 round{ROUND}",
        "items": [{"material_id": mat, "qty": "1", "price_excl": "10", "tax_rate": "13"}],
    }, token_maker)
    doc2 = res["data"]["header"]["id"]
    http("PATCH", f"/api/business/erp/stock-in/{doc2}/status", {"status": "POSTED"}, token_maker)
    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc2}/status",
                   {"status": "CANCELLED", "reason": "自己作废自己批"}, token_reviewer)
    ok("复核人 = 发起人 → 422 + 65019", st == 422 and res.get("code") == 65019, f"{st} code={res.get('code')}")
    msg = str(res.get("msg", ""))
    ok("报文点名复核岗位与配置路径", "WAREHOUSE_MANAGER" in msg and "仓管主管" in msg, msg[:120])
    st2 = psql(f"select status from biz_stock_in where id = '{doc2}'::uuid")[0][0]
    ok("被拒后单据仍是 POSTED（整笔回滚）", st2 == "POSTED", st2)

    print("\n## D. 反例②：复核岗位 0 人 → 422 + 65018，报文给配置指引（不兜底超管）")
    psql(f"delete from sys_user_role where role_id = '{ROLE_WH}'::uuid and user_id = '{U_REVIEWER}'::uuid")
    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc2}/status",
                   {"status": "CANCELLED", "reason": "岗位空缺试试"}, token_maker)
    ok("角色 0 人 → 422 + 65018", st == 422 and res.get("code") == 65018, f"{st} code={res.get('code')}")
    msg = str(res.get("msg", ""))
    ok("报文说清「没有候选人 + 去哪儿配」", "审批人" in msg and ("角色" in msg or "成员" in msg), msg[:160])
    ok("没有偷偷兜底给超管（没有派给 admin）",
       psql("select count(*) from biz_flow_task t join sys_user u on u.id = t.assignee_id "
            " where u.username = 'admin' and t.created_at > now() - interval '2 minutes'")[0][0] == "0")
    # 复原岗位成员（后续轮次要用）
    psql(f"insert into sys_user_role (user_id, role_id) values ('{U_REVIEWER}'::uuid, '{ROLE_WH}'::uuid) "
         " on conflict do nothing")
    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc2}/status",
                   {"status": "CANCELLED", "reason": "岗位补人后重新发起"}, token_maker)
    ok("把复核人补回岗位后同一张单再发起 → 200", st == 200 and res.get("code") == 0, f"{st} {res.get('msg')}")
    st, res = http("POST", "/api/business/flow/task/"
                   + psql("select t.id from biz_flow_task t where t.instance_id = "
                          f"(select approval_instance_id from biz_stock_in where id = '{doc2}'::uuid) "
                          "and t.status='PENDING'")[0][0]
                   + "/approve", {"comment": "复核通过"}, token_reviewer)
    ok("补人后复核通过 → CANCELLED",
       st == 200 and psql(f"select status from biz_stock_in where id = '{doc2}'::uuid")[0][0] == "CANCELLED",
       f"{st} {res.get('msg')}")

    print("\n## E. 快照/行 一致性（由 `flow::maintain` 与 p16 单测覆盖）")
    print("  · 行 ↔ graph_hash 重算 = 见 cargo test p16_graph_snapshot_selfcheck_is_clean（maintain::run_once 实跑）")
    print("  · 快照 ↔ graph_hash 自洽 = 见 p16_seed_flow_is_self_consistent")

    print(f"\n=== 汇总：PASS={len(PASS)} FAIL={len(FAIL)} ===")
    for name in FAIL:
        print(f"  [FAIL] {name}")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
