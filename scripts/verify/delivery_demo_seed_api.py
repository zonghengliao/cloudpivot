#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""demo_seed.sh 的接口级验收（DELIVERY-2）：真实 HTTP 复核「灌完数据 → 登录看到什么」。

为什么还要这一层：`demo_seed.sh` 末尾的自检只保证「库里的账对得上」，
但客户看到的是**页面**。页面读的是接口 —— 接口口径（可见性过滤、分页、字段名）
一变，库里数据全对也可能一张卡都看不到。本脚本按卡片验收清单逐条打接口。

用法
    # 默认打本机 8023
    python3 scripts/verify/delivery_demo_seed_api.py
    # 打隔离实例
    DEMO_BASE_URL=http://127.0.0.1:18130 python3 scripts/verify/delivery_demo_seed_api.py
    # 连「同意 3 条待办」的写测试（会改数据，跑完请 demo_seed.sh --clean 再重灌）
    DEMO_BASE_URL=http://127.0.0.1:18130 python3 scripts/verify/delivery_demo_seed_api.py --approve

环境变量
    DEMO_BASE_URL        后端根地址（默认 http://127.0.0.1:8023，脚本自己拼 /api）
    DEMO_ADMIN_USER      管理员账号（默认 admin）
    DEMO_ADMIN_PASSWORD  管理员口令（默认 Admin@123456）
    DEMO_NEW_PASSWORD    空库首启被要求改密时改成的口令（默认 Admin@654321）

退出码：0 = 全绿；1 = 有失败项
"""
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("DEMO_BASE_URL", "http://127.0.0.1:8023").rstrip("/") + "/api"
ADMIN_USER = os.environ.get("DEMO_ADMIN_USER", "admin")
ADMIN_PASS = os.environ.get("DEMO_ADMIN_PASSWORD", "Admin@123456")
NEW_PASS = os.environ.get("DEMO_NEW_PASSWORD", "Admin@654321")
APPROVE = "--approve" in sys.argv
DEMO = "de300000-"

OK = 0
BAD = 0


def call(method, path, token=None, body=None):
    req = urllib.request.Request(BASE + path,
                                 data=json.dumps(body).encode() if body is not None else None,
                                 method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        return {"http_error": exc.code, "body": exc.read().decode()[:300]}


def get(path, token):
    return call("GET", path, token)


def payload(resp):
    return (resp or {}).get("data")


def login(password):
    d = payload(call("POST", "/system/auth/login",
                     body={"username": ADMIN_USER, "password": password})) or {}
    return d.get("access_token") or d.get("token")


def check(name, cond, detail=""):
    global OK, BAD
    if cond:
        OK += 1
        print(f"  [PASS] {name}" + (f"  {detail}" if detail else ""))
    else:
        BAD += 1
        print(f"  [FAIL] {name}  {detail}")


def main():
    token = login(ADMIN_PASS) or login(NEW_PASS)
    if not token:
        print(f"登录失败（{BASE}）：改 Demo_BASE_URL / DEMO_ADMIN_PASSWORD 再试")
        return 1
    probe = get("/business/dashboard/my-metrics", token)
    if isinstance(probe, dict) and probe.get("http_error") == 403 and "修改初始密码" in str(probe.get("body")):
        call("PUT", "/system/auth/password", token,
             {"old_password": ADMIN_PASS, "new_password": NEW_PASS})
        print(f"① 空库首启 {ADMIN_USER} 首登要求改密（预期行为）→ 已改密")
        token = login(NEW_PASS)
        if not token:
            print("改密后重新登录失败")
            return 1
    print("① 登录成功")

    # ② 首页四张指标卡
    cards = {c["key"]: c for c in (payload(get("/business/dashboard/my-metrics", token)) or [])}
    print("\n② 首页指标卡（值, 附加数）：",
          {k: (v.get("value"), v.get("extra")) for k, v in cards.items()})
    check("待办审批 = 3", (cards.get("my_todo_count") or {}).get("value") == 3)
    check("逾期 = 1", (cards.get("my_overdue") or {}).get("value") == 1)
    check("今日任务（含逾期）>= 2", ((cards.get("my_task_today") or {}).get("value") or 0) >= 2)
    check("本周已办 >= 1", ((cards.get("my_done_week") or {}).get("value") or 0) >= 1)

    # ③ 待办审批 3 条，覆盖采购/销售/报销
    todo = payload(get("/business/flow/task/todo?page=1&page_size=20", token)) or {}
    rows = todo.get("list") or []
    print("\n③ 待办审批：", [(r.get("biz_no"), r.get("biz_type"), r.get("current_node_name")) for r in rows])
    check("待办审批 3 条", todo.get("total") == 3, f"total={todo.get('total')}")
    check("覆盖采购/销售/报销三类",
          {r.get("biz_type") for r in rows} == {"PO_APPROVE", "SO_APPROVE", "EXPENSE"})

    # ④ 工作任务 5 个（1 逾期）
    my = payload(get("/business/task/my", token)) or {}
    buckets = {k: v for k, v in my.items() if isinstance(v, list)}
    check("工作任务 5 个", sum(len(v) for v in buckets.values()) == 5,
          str({k: len(v) for k, v in buckets.items()}))
    check("其中 1 个逾期（标红那一个）", len(buckets.get("overdue") or []) == 1)

    # ⑤ 公告 3 条 / 站内消息 5 条
    ann = payload(get("/business/announcement/visible/page?page=1&page_size=20", token)) or {}
    check("公告 3 条", ann.get("total") == 3, f"total={ann.get('total')}")
    notice = payload(get("/business/notice/mine?page=1&page_size=50", token)) or {}
    demo_notices = [n for n in (notice.get("list") or []) if "D9000" in str(n.get("notice_no"))]
    check("演示站内消息 5 条（系统自带通知不算）", len(demo_notices) == 5,
          f"全部 {notice.get('total')} / 演示 {len(demo_notices)}")

    # ⑥ 采购订单：状态 + 明细 + 审批记录
    po = payload(get("/business/erp/purchase-order/page?page=1&page_size=50", token)) or {}
    demo_po = [r for r in (po.get("list") or []) if str(r.get("id", "")).startswith(DEMO)]
    print("\n⑥ 采购订单：", [(r.get("order_no"), r.get("status"), r.get("total_with_tax")) for r in demo_po])
    check("采购订单 3 张（2 已完成 + 1 审批中）", len(demo_po) == 3, f"{len(demo_po)}")
    check("含审批中（SUBMITTED）与已完成（RECEIVED）",
          {"SUBMITTED", "RECEIVED"} <= {r.get("status") for r in demo_po})
    received = next((r for r in demo_po if r.get("status") == "RECEIVED"), None)
    if received:
        items = payload(get(f"/business/erp/purchase-order-item/page?order_id={received['id']}&page=1&page_size=50", token)) or {}
        check("采购订单明细可读", (items.get("total") or 0) >= 1, f"total={items.get('total')}")
        d = payload(get(f"/business/erp/purchase-order/{received['id']}", token)) or {}
        inst = d.get("approval_instance_id")
        check("已完成采购订单挂着审批实例", bool(inst), str(inst))
        inst_d = payload(get(f"/business/flow/instance/{inst}", token)) or {}
        check("审批记录可读（节点 ≥ 5 / 时间轴 ≥ 1）",
              len(inst_d.get("nodes") or []) >= 5 and len(inst_d.get("timeline") or []) >= 1,
              f"nodes={len(inst_d.get('nodes') or [])} timeline={len(inst_d.get('timeline') or [])}")

    # ⑦ 销售订单：明细 + 发货 + 应收
    so = payload(get("/business/erp/sales-order/page?page=1&page_size=50", token)) or {}
    demo_so = [r for r in (so.get("list") or []) if str(r.get("id", "")).startswith(DEMO)]
    print("\n⑦ 销售订单：", [(r.get("so_no"), r.get("status"), r.get("total_with_tax")) for r in demo_so])
    check("销售订单 2 张（已出库 + 待审批）",
          {r.get("status") for r in demo_so} == {"OUTBOUND", "SUBMITTED"})
    out = next((r for r in demo_so if r.get("status") == "OUTBOUND"), None)
    if out:
        ship = payload(get(f"/business/erp/shipment/page?so_id={out['id']}&page=1&page_size=10", token)) or {}
        ships = ship.get("list") or []
        check("该销售订单有发货单（POSTED）",
              any(s.get("status") == "POSTED" for s in ships), f"{[s.get('ship_no') for s in ships]}")
        check("发货单已关联销售出库单", any(s.get("stock_out_id") for s in ships))
    ar = payload(get("/business/erp/receivable/page?page=1&page_size=50", token)) or {}
    rc = payload(get("/business/erp/receipt/page?page=1&page_size=50", token)) or {}
    check("应收/收款都有数据", (ar.get("total") or 0) >= 1 and (rc.get("total") or 0) >= 1,
          f"AR={ar.get('total')} RC={rc.get('total')}")

    # ⑧ 凭证 10 张，能追溯来源单据
    vp = payload(get("/business/erp/voucher/page?page=1&page_size=50", token)) or {}
    demo_v = [v for v in (vp.get("list") or []) if str(v.get("id", "")).startswith(DEMO)]
    print("\n⑧ 凭证列表 total =", vp.get("total"), " 演示", len(demo_v))
    check("凭证 >= 10 张", (vp.get("total") or 0) >= 10, f"{vp.get('total')}")
    check("演示凭证 10 张", len(demo_v) == 10, f"{len(demo_v)}")
    auto = [v for v in demo_v if v.get("source_type") != "MANUAL"]
    check("自动凭证都能追溯源单号", all(v.get("source_no") for v in auto), f"{len(auto)} 张")

    # ⑨ 科目余额表有数
    period = (demo_v[0].get("period") if demo_v else "2026-11")
    tb = payload(get(f"/business/erp/ledger/trial-balance?period={period}", token)) or {}
    tb_rows = tb.get("rows") or []
    moved = [r for r in tb_rows
             if abs(float(r.get("period_debit") or 0)) + abs(float(r.get("period_credit") or 0)) > 0.001]
    print("\n⑨ 科目余额表：", period, "科目行", len(tb_rows), "本期有发生额", len(moved))
    check("科目余额表有数（本期发生额科目 ≥ 10）", len(moved) >= 10, f"{len(moved)}")

    # ⑩ 审批可真实执行（写测试）
    if APPROVE and rows:
        print("\n⑩ 逐条同意待办（写测试，会改数据）：")
        for r in rows:
            resp = call("POST", f"/business/flow/task/{r['id']}/approve", token,
                        {"comment": "演示验证：核对无误，同意"})
            ok = not (isinstance(resp, dict) and resp.get("http_error"))
            check(f"同意 {r.get('biz_no')}（{r.get('biz_type')}）", ok,
                  "" if ok else json.dumps(resp, ensure_ascii=False)[:200])
        after = payload(get("/business/flow/task/todo?page=1&page_size=20", token)) or {}
        check("同意后我的待办清空", after.get("total") == 0, f"{after.get('total')}")

    print(f"\n==== 汇总：PASS {OK} / FAIL {BAD} ====")
    return 0 if BAD == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
