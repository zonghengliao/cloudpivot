#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P9 端到端验收：建单 → 过账 → 作废 → 三账回基线（真实 HTTP + 真库）

跑在 144 上，实例指向 cloudpivot_p9e（prod 修复后的副本）。
四个快照必须满足：④ 作废后 == ① 基线（逐字段相等）。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("P9_BASE", "http://127.0.0.1:18091")
DB = os.environ.get("P9_DB", "cloudpivot_p9e")
USER = os.environ.get("P9_DB_USER", "cloudpivot")
PW = os.environ["NIK_DATABASE_PASSWORD"]
ADMIN_PW = os.environ.get("P9_ADMIN_PW") or os.environ["NIK_ADMIN_PASSWORD"]


def psql(sql):
    env = dict(os.environ, PGPASSWORD=PW)
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", USER, "-d", DB, "-tA", "-F", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def http(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def snapshot(tag):
    """三账 + 凭证 + 流水（按本卡测试物料/仓库过滤，避免被别的数据干扰）"""
    row = psql(
        "select "
        "coalesce((select sum(qty)::text from biz_inventory where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(amount)::text from biz_inventory where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(qty)::text from biz_inventory_location where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select count(*)::text from biz_inventory_txn where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(case when txn_type='IN' then qty else -qty end)::text "
        "          from biz_inventory_txn where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null),'0'),"
        "coalesce((select sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0))::text "
        "          from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null "
        "          join biz_account a on a.id=e.account_id where a.account_code like '1405%'),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null and is_red=1),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null and source_no = {doc}),'0')"
        .format(m="'" + MAT + "'::uuid", w="'" + WH + "'::uuid", doc="'" + DOC_NO[0] + "'")
    )[0]
    names = ["成本账数量", "成本账金额", "库位账数量", "流水条数", "流水净额",
             "凭证总数", "1405净额", "红字凭证数", "本单凭证数"]
    snap = dict(zip(names, row))
    print(f"\n【{tag}】")
    for k, v in snap.items():
        print(f"    {k:>10} = {v}")
    return snap


def ensure_second_approver():
    """作废复核 `stock_in_cancel / n_review` 是 `self_approval=FORBID`（内控：作废必须他人复核）。

    脚本以前用 admin 一人提交 + 作废，P0/P17 之后会 422 + 65019 直接拒单 —— 不是回归，
    是产品把内控收紧了。这里备一个**非发起人**的超管复核账号（口令哈希抄 admin）。
    """
    psql(
        "insert into sys_user (id, username, password, real_name, status, must_change_password, token_version) "
        "select 'a2a9cccc-0000-4000-8000-0000000000cc'::uuid, 'p9_cancel_approver', u.password, "
        "       'P9 作废复核人', 1, false, 1 from sys_user u where u.username = 'admin' "
        "  and not exists (select 1 from sys_user where username = 'p9_cancel_approver')"
    )
    psql(
        "insert into sys_user_role (user_id, role_id) "
        "select 'a2a9cccc-0000-4000-8000-0000000000cc'::uuid, r.id from sys_role r "
        " where r.role_code = 'SUPER_ADMIN' and r.deleted_at is null "
        "   and not exists (select 1 from sys_user_role x "
        "                    where x.user_id = 'a2a9cccc-0000-4000-8000-0000000000cc'::uuid and x.role_id = r.id)"
    )


def ensure_maker():
    """建一个**非超管**的制单人（`p9_maker`）并只给它收货单 7 个权限。

    为什么必须换人：作废复核节点 `n_review` 是 `assignee_type=ADMIN` + `self_approval=FORBID`，
    引擎的判据是「候选里出现发起人本人 → 直接 fail-closed」。admin（超管）既是发起人又是候选，
    作废就会 422 + 65019。真实业务里收货单是仓管员制单，超管只做复核 —— 脚本照这个分工。
    """
    psql(
        "insert into sys_role (id, role_code, role_name, description, status, sort_order) "
        "select 'a2a9dddd-0000-4000-8000-0000000000dd'::uuid, 'P9_MAKER', 'P9 制单人', "
        "       'NIGHT-REGRESSION 夹具：只有收货单权限，用于作废链路', 1, 999 "
        " where not exists (select 1 from sys_role where role_code = 'P9_MAKER')"
    )
    psql(
        "insert into sys_role_menu (role_id, menu_id, created_at) "
        "select 'a2a9dddd-0000-4000-8000-0000000000dd'::uuid, m.id, now() from sys_menu m "
        " where (m.auth_code like 'business:erp:stock-in%' or m.auth_code like 'business:erp:supplier:list' "
        "        or m.auth_code like 'business:erp:material:list') "
        "   and not exists (select 1 from sys_role_menu x "
        "                    where x.role_id = 'a2a9dddd-0000-4000-8000-0000000000dd'::uuid and x.menu_id = m.id)"
    )
    psql(
        "insert into sys_user (id, username, password, real_name, status, must_change_password, token_version) "
        "select 'a2a9cccc-0000-4000-8000-0000000000cd'::uuid, 'p9_maker', u.password, "
        "       'P9 制单人', 1, false, 1 from sys_user u where u.username = 'admin' "
        "  and not exists (select 1 from sys_user where username = 'p9_maker')"
    )
    psql(
        "insert into sys_user_role (user_id, role_id) "
        "select 'a2a9cccc-0000-4000-8000-0000000000cd'::uuid, 'a2a9dddd-0000-4000-8000-0000000000dd'::uuid "
        " where not exists (select 1 from sys_user_role x "
        "                    where x.user_id = 'a2a9cccc-0000-4000-8000-0000000000cd'::uuid "
        "                      and x.role_id = 'a2a9dddd-0000-4000-8000-0000000000dd'::uuid)"
    )


_TOKENS = {}


def token_for(username):
    """夹具账号的口令哈希都抄 admin，所以同一个口令登录得上；按需登录取 token 并缓存。"""
    if username not in _TOKENS:
        st, res = http("POST", "/api/system/auth/login", {"username": username, "password": ADMIN_PW})
        if st != 200 or res.get("code") != 0:
            raise RuntimeError(f"{username} 登录失败：{st} {res}")
        _TOKENS[username] = res["data"]["access_token"]
    return _TOKENS[username]


def approve_pending(instance_id, token2=None, username=None, rounds=8):
    """把该实例上**所有**待办批完（会签 ALL 时多个超管各一条），每步用待办归属人的 token。

    `n_review` 是 ADMIN 节点、`approval_mode` 为空 ⇒ 引擎按会签 ALL 展开全部超管，
    admin 与 p9_cancel_approver 各有一条待办；拿别人的任务 id 去 approve 会 403/65028。
    """
    for _ in range(rounds):
        rows = psql(
            "select t.id, u.username from biz_flow_task t join sys_user u on u.id = t.assignee_id "
            f" where t.instance_id = '{instance_id}'::uuid and t.status = 'PENDING' "
            " order by t.created_at"
        )
        if not rows:
            st_inst = psql(f"select status from biz_flow_instance where id = '{instance_id}'::uuid")
            return bool(st_inst) and st_inst[0][0] in ("APPROVED", "REJECTED", "CANCELLED")
        pending = [(tid, uname) for tid, uname in rows]
        for task_id, uname in pending:
            tok = token_for(uname)
            st, res = http("POST", f"/api/business/flow/task/{task_id}/approve",
                           {"comment": "同意作废（NIGHT-REGRESSION 真机）"}, tok)
            if st != 200 or res.get("code") != 0:
                print(f"  [FAIL] {uname} 审批失败：{st} {res}")
                return False
    return False


def main():
    global MAT, WH, DOC_NO
    MAT = psql("select id from biz_material where material_code='M-W1A-001' and deleted_at is null")[0][0]
    WH = psql("select id from biz_warehouse where warehouse_code='WH01'")[0][0]
    SUP = psql("select id from biz_supplier where deleted_at is null order by created_at limit 1")[0][0]
    DOC_NO = ["(尚未建单)"]

    st, res = http("POST", "/api/system/auth/login", {"username": "admin", "password": ADMIN_PW})
    if st != 200 or res.get("code") != 0:
        print(f"登录失败：{st} {res}")
        return 1
    token_admin = res["data"]["access_token"]
    print(f"登录成功（must_change_password={res['data'].get('must_change_password')}）")

    # 制单人必须是**非超管**（作废复核 n_review 是 ADMIN + self_approval=FORBID，
    # 超管自建自撤会 fail-closed 422/65019）—— 照真实分工：仓管员制单、超管复核。
    ensure_maker()
    st, res = http("POST", "/api/system/auth/login", {"username": "p9_maker", "password": ADMIN_PW})
    if st != 200 or res.get("code") != 0:
        print(f"制单人登录失败：{st} {res}")
        return 1
    token = res["data"]["access_token"]
    print("制单人 p9_maker 登录成功（非超管，只有收货单权限）")

    base = snapshot("① 基线（建单前）")

    st, res = http("POST", "/api/business/erp/stock-in", {
        "supplier_id": SUP, "warehouse_id": WH, "remark": "P9 e2e：作废回滚验证",
        # 无源入库：本卡验的是「作废回滚」，不是采购单来源（P0 起采购入库必须绑定 source_order_id，
        # 无来源需显式声明 is_sourceless=true —— 产品给的合法口子，NIGHT-REGRESSION 补上）
        "is_sourceless": True,
        "items": [{"material_id": MAT, "qty": "9", "price_excl": "7", "tax_rate": "13"}],
    }, token)
    if st != 200 or res.get("code") != 0:
        print(f"建单失败：{st} {res}")
        return 1
    doc_id = res["data"]["header"]["id"]
    DOC_NO[0] = res["data"]["header"]["in_no"]
    print(f"\n建单成功：{DOC_NO[0]}（id={doc_id}，9 件 @7 不含税）")
    after_create = snapshot("② 建单后（应 == 基线：草稿不碰库存）")

    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"}, token)
    if st != 200 or res.get("code") != 0:
        print(f"过账失败：{st} {res}")
        return 1
    after_post = snapshot("③ 过账后（成本账 +9 件 / +63 元，库位账同步 +9，凭证 +1）")

    ensure_second_approver()
    st, res = http("POST", "/api/system/auth/login", {"username": "p9_cancel_approver", "password": ADMIN_PW})
    if st != 200 or res.get("code") != 0:
        print(f"复核人登录失败：{st} {res}")
        return 1
    token2 = res["data"]["access_token"]

    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                   {"status": "CANCELLED", "reason": "P9 e2e：作废必须退库存"}, token)
    if st != 200 or res.get("code") != 0:
        print(f"作废失败：{st} {res}")
        return 1
    inst = psql(f"select coalesce(approval_instance_id::text,'') from biz_stock_in where id = '{doc_id}'::uuid")
    inst_id = inst[0][0] if inst else ""
    if inst_id and (res.get("data") or {}).get("status") != "CANCELLED":
        print(f"  作废走审批（实例 {inst_id}），由 p9_cancel_approver 复核…")
        if not approve_pending(inst_id, token2):
            print("  [FAIL] 作废复核未走完")
            return 1
        status = psql(f"select status from biz_stock_in where id = '{doc_id}'::uuid")[0][0]
        print(f"  复核完成，单据状态 = {status}")
        if status != "CANCELLED":
            print(f"  [FAIL] 复核通过后单据仍不是 CANCELLED（{status}）")
            return 1
    after_cancel = snapshot("④ 作废后（必须 == ① 基线）")

    print("\n===== 断言 =====")
    ok = True
    # ② 建单是草稿：三账 + 凭证一个都不许动
    for k in base:
        if k == "本单凭证数":
            continue
        if base[k] != after_create[k]:
            print(f"  [FAIL] ② 建单后 {k} 变了：基线 {base[k]} → {after_create[k]}")
            ok = False
    print("  [PASS] ② 建单后 三账/凭证全部未动（草稿不碰账）" if ok else "")

    checks = [
        ("③ 过账后 成本账数量 = 基线 + 9",
         f"{float(base['成本账数量']) + 9:.4f}", f"{float(after_post['成本账数量']):.4f}"),
        ("③ 过账后 成本账金额 = 基线 + 63",
         f"{float(base['成本账金额']) + 63:.4f}", f"{float(after_post['成本账金额']):.4f}"),
        ("③ 过账后 库位账数量 = 基线 + 9（P6 两账同步）",
         f"{float(base['库位账数量']) + 9:.4f}", f"{float(after_post['库位账数量']):.4f}"),
        ("③ 过账后 1405 净额 = 基线 + 63",
         f"{float(base['1405净额']) + 63:.4f}", f"{float(after_post['1405净额']):.4f}"),
        ("③ 过账后 流水条数 = 基线 + 1", str(int(base["流水条数"]) + 1), after_post["流水条数"]),
        # ④ 作废：库存三账回基线；凭证侧留痕（原凭证 + 红字各一张）
        ("④ 作废后 成本账数量 == 基线", base["成本账数量"], after_cancel["成本账数量"]),
        ("④ 作废后 成本账金额 == 基线", base["成本账金额"], after_cancel["成本账金额"]),
        ("④ 作废后 库位账数量 == 基线", base["库位账数量"], after_cancel["库位账数量"]),
        ("④ 作废后 流水净额 == 基线", base["流水净额"], after_cancel["流水净额"]),
        ("④ 作废后 1405 净额 == 基线（红冲后净额归零）",
         f"{float(base['1405净额']):.4f}", f"{float(after_cancel['1405净额']):.4f}"),
        ("④ 作废后 流水条数 = 基线 + 2（IN + 冲销 OUT）",
         str(int(base["流水条数"]) + 2), after_cancel["流水条数"]),
        ("④ 作废后 凭证总数 = 基线 + 2（蓝字 + 红字）",
         str(int(base["凭证总数"]) + 2), after_cancel["凭证总数"]),
        ("④ 作废后 红字凭证数 = 基线 + 1",
         str(int(base["红字凭证数"]) + 1), after_cancel["红字凭证数"]),
        ("④ 本单凭证数 = 2（一蓝一红）", "2", after_cancel["本单凭证数"]),
    ]
    for name, want, got in checks:
        if str(want) == str(got):
            print(f"  [PASS] {name}")
        else:
            print(f"  [FAIL] {name}\n         期望 {want}\n         实际 {got}")
            ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
