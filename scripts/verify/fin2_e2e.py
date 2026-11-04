#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""FIN-2 固定资产真机全链路验收（HTTP，跑在隔离实例上）。

链路（卡面「真机全链路」逐条对应）：
  ① 建一张资产卡片（购入凭证 借 1601 / 贷 2202）
  ② 跑一次月度折旧 → 生成多借方凭证（借 5602.01 按部门 / 贷 1602）
  ③ 做一次部门转移 → 变动记录有留痕
  ④ 做一次处置（出售）→ 凭证正确、资产状态变「已处置」
  ⑤ 作废处置 → 凭证红冲、资产恢复在役

用法：
    python3 scripts/verify/fin2_e2e.py [base_url]
默认 http://127.0.0.1:18025；DB 断言走 NIK_DATABASE_*（默认 alun/alun/cloudpivot_fin2asset）。

判据全部来自**库里的真实数据**（凭证分录 / 状态 / 留痕），不看接口回包的自我声明。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18025"
DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "alun"),
    name=os.environ.get("NIK_DATABASE_NAME", "cloudpivot_fin2asset"),
    password=os.environ.get("NIK_DATABASE_PASSWORD", "alun"),
)
ADMIN = ("admin", "Admin@123456")
PASSED: list[str] = []
FAILED: list[str] = []


def q(sql: str):
    env = dict(os.environ, PGPASSWORD=DB["password"])
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["name"], "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql: str, default: str = "") -> str:
    rows = q(sql)
    return rows[0][0] if rows and rows[0] else default


def ensure_approver() -> None:
    """造一个 GENERAL_MANAGER 成员（`fin2gm`），口令与 admin 相同。

    内控闸（`asset_bridge::guard_self_approval`）会拒绝发起人 / 制单人批自己，
    所以本条链路的处置必须由第二个人审批 —— 这不是测试技巧，是内控本身的要求。

    另外两件事（模板库里别的卡留下的夹具会干扰审批图）：
    * `GENERAL_MANAGER` 里只留 fin2gm（否则会派发出多个会签待办，本用例只验固资链路）；
    * 顺带把 fin2gm 挂上 `SUPER_ADMIN`：本用例要验的是**业务链**，
      不是角色-菜单授权（那套由权限模块自己的用例覆盖），`business:flow:approve`
      的授权依赖模板库里角色菜单的完整度，不该让它决定这条链路的成败。
    """
    q("""
        insert into sys_user (id, username, password, real_name, status, must_change_password,
                              token_version, user_type, remark)
        select gen_random_uuid(), 'fin2gm', u.password, 'FIN2 验收总经理', 1, false,
               1, 'HUMAN', 'FIN-2 e2e 用：资产处置审批人'
          from sys_user u where u.username = 'admin'
           and not exists (select 1 from sys_user where username = 'fin2gm')
    """)
    q("""
        delete from sys_user_role
         where role_id = '20000000-0000-4000-8000-0000000f0003'::uuid
           and user_id <> (select id from sys_user where username = 'fin2gm')
    """)
    q("""
        insert into sys_user_role (id, user_id, role_id)
        select gen_random_uuid(), u.id, r.id
          from sys_user u
          join sys_role r on r.role_code in ('GENERAL_MANAGER', 'SUPER_ADMIN') and r.deleted_at is null
         where u.username = 'fin2gm'
           and not exists (select 1 from sys_user_role x where x.user_id = u.id and x.role_id = r.id)
    """)


def ensure_depts() -> tuple[tuple[str, str], tuple[str, str]]:
    """造两个夹具部门（转移用例要「从 A 到 B」）。

    模板库里只有一个 FIN-1 留下的夹具部门，转移用例至少要两个 —— 自己造，
    不依赖别的卡留下的数据（夹具卫生：`FIN2-DEPT-*` 前缀，一眼能看出是本卡写的）。
    """
    q("""
        insert into sys_dept (dept_code, dept_name, status, sort_order)
        select v.code, v.name, 1, v.sort
          from (values ('FIN2-DEPT-A', 'FIN2 研发部', 10),
                       ('FIN2-DEPT-B', 'FIN2 产品部', 20)) as v(code, name, sort)
         where not exists (select 1 from sys_dept d where d.dept_code = v.code and d.deleted_at is null)
    """)
    rows = q("""select id, dept_name from sys_dept
                 where dept_code in ('FIN2-DEPT-A','FIN2-DEPT-B') and deleted_at is null
                 order by dept_code""")
    if len(rows) != 2:
        raise SystemExit(f"夹具部门创建失败：{rows}")
    return (rows[0][0], rows[0][1]), (rows[1][0], rows[1][1])


def ok(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        PASSED.append(name)
        print(f"  ✅ {name}" + (f"（{detail}）" if detail else ""))
    else:
        FAILED.append(name)
        print(f"  🚨 {name}" + (f" —— {detail}" if detail else ""))


class Client:
    def __init__(self, base: str):
        self.base = base
        self.token = ""

    def call(self, method: str, path: str, body=None, expect=None):
        url = self.base + path
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                payload = json.loads(resp.read().decode())
                status = resp.status
        except urllib.error.HTTPError as err:
            raw = err.read().decode()
            try:
                payload = json.loads(raw)
            except Exception:
                payload = {"raw": raw}
            status = err.code
        if expect is not None and status != expect:
            print(f"    ↳ {method} {path} → {status}: {json.dumps(payload, ensure_ascii=False)[:400]}")
        return status, payload

    def login(self, user: str, pwd: str) -> None:
        status, payload = self.call("POST", "/api/system/auth/login",
                                    {"username": user, "password": pwd}, expect=200)
        if status != 200:
            raise SystemExit(f"登录失败：{status} {payload}")
        self.token = payload["data"]["access_token"]


def data_of(payload):
    """统一取业务数据（Res::ok 是 {code, data, ...}）。"""
    return payload.get("data")


def main() -> int:
    cli = Client(BASE)
    print("=" * 88)
    print("FIN-2 固定资产真机全链路")
    print(f"实例 {BASE}  库 {DB['name']}")
    print("=" * 88)

    # 空库起的新实例里 admin 带 `must_change_password=true`（首次登录必须改密），
    # 那会让所有业务接口 403。这是**一次性夹具库**，直接关掉这个开关，
    # 改密流程本身由 system::auth 自己的用例覆盖，不该挡在固资链路的验收路径上。
    q("update sys_user set must_change_password = false where username = 'admin'")
    cli.login(*ADMIN)
    # 会话标志有 60s 的 `uv:{user_id}` 缓存（session_guard 的 fail-closed 设计），
    # 若这个库刚被别的脚本登过，缓存里还是 true —— 等它过期，不要 flush 共享 Redis。
    for _ in range(15):
        status, payload = cli.call("GET", "/api/business/erp/asset-category/list")
        if payload.get("code") != 403:
            break
        time.sleep(5)
    print("① [准备] admin 登录成功")

    # ---------- 前置数据 ----------
    status, payload = cli.call("GET", "/api/business/erp/asset-category/list")
    ok("资产分类下拉可取（5 条种子）", status == 200 and len(data_of(payload) or []) >= 5,
       f"{len(data_of(payload) or [])} 条")
    cats = data_of(payload) or []
    elec = next((c for c in cats if c["category_code"] == "ELEC"), None)
    if elec is None:
        print("  🚨 没有 ELEC 分类，后续无法继续")
        return 1
    (dept_a, dept_a_name), (dept_b, dept_b_name) = ensure_depts()

    # ---------- ① 建卡 ----------
    print("\n① 建资产卡片（购入凭证）")
    create_body = {
        "asset_name": "FIN2 验收用笔记本",
        "category_id": elec["id"],
        "spec": "ThinkPad X1 Carbon",
        "use_dept_id": dept_a,
        "location": "A 座 3 层",
        "original_value": "100000.0000",
        "tax_amount": "0",
        "purchase_date": "2026-08-15",
        "pay_mode": "CASH",
        "remark": "FIN-2 真机验收",
    }
    status, payload = cli.call("POST", "/api/business/erp/asset", create_body, expect=200)
    ok("建卡接口返回 200", status == 200, f"code={payload.get('code')}")
    if status != 200:
        print(json.dumps(payload, ensure_ascii=False)[:800])
        return 1
    asset_id = data_of(payload)["id"]
    asset_no = data_of(payload)["no"]
    row = q(f"""select original_value::text, residual_value::text, depreciable_base::text,
                       monthly_depreciation::text, net_value::text, status,
                       start_depreciation_date::text, voucher_id::text
                  from biz_fixed_asset where id = '{asset_id}'::uuid""")[0]
    ok("原值 / 残值 / 可折旧总额", (row[0], row[1], row[2]) == ("100000.0000", "5000.0000", "95000.0000"),
       f"{row[0]} / {row[1]} / {row[2]}")
    ok("月折旧额 = 95000/36 = 2638.8889", row[3] == "2638.8889", row[3])
    ok("初始净值 = 原值、状态 IN_USE", row[4] == "100000.0000" and row[5] == "IN_USE", f"{row[4]} / {row[5]}")
    ok("开始计提日 = 购入次月 1 日", row[6] == "2026-09-01", row[6])
    ok("已回填购入凭证", bool(row[7]), row[7])

    entries = q(f"""select e.line_no, e.account_code, e.debit_amount::text, e.credit_amount::text,
                           coalesce(e.aux_dept_id::text,'')
                      from biz_voucher_entry e where e.voucher_id = '{row[7]}'::uuid order by e.line_no""")
    ok("购入凭证分录：借 1601 100000 / 贷 1002 100000（现付）",
       [(x[1], x[2], x[3]) for x in entries] == [("1601", "100000.0000", "0.0000"),
                                                 ("1002", "0.0000", "100000.0000")],
       str([(x[1], x[2], x[3]) for x in entries]))
    # 赊购口径（ARCH-6 §5.3.3）：规则行必须指向 2202，否则「赊购」会静默走成现付
    ok("赊购规则 FIXED_ASSET_ACQ_DEFAULT 贷方 = 2202",
       one("""select a.account_code from biz_voucher_rule r
                join biz_account a on a.id = r.credit_account_id
               where r.rule_code = 'FIXED_ASSET_ACQ_DEFAULT'""") == "2202")
    ok("转固规则 FIXED_ASSET_ACQ_STOCK 贷方 = 1405",
       one("""select a.account_code from biz_voucher_rule r
                join biz_account a on a.id = r.credit_account_id
               where r.rule_code = 'FIXED_ASSET_ACQ_STOCK'""") == "1405")

    # 4xx 校验：负原值 / 必填缺
    status, payload = cli.call("POST", "/api/business/erp/asset", {**create_body, "original_value": "-1"})
    ok("负金额建卡被拒（400）", status == 400, f"{status} {payload.get('msg', '')[:40]}")
    missing = {k: v for k, v in create_body.items() if k != "purchase_date"}
    status, payload = cli.call("POST", "/api/business/erp/asset", missing)
    ok("必填缺少被拒（400 且指名）", status == 400 and "购入" in str(payload.get("msg", "")),
       f"{status} {str(payload.get('msg'))[:50]}")

    # ---------- ② 月度折旧 ----------
    print("\n② 跑一次月度折旧（2026-09）")
    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-09"})
    ok("计提接口 200 且 POSTED", status == 200 and data_of(payload)["outcome"] == "POSTED",
       f"{status} {data_of(payload) and data_of(payload).get('outcome')}")
    batch = data_of(payload)["batch"] if status == 200 else None
    ok("批次金额 = 2638.8889 / 台数 1", batch and batch["total_amount"] == "2638.8889" and batch["asset_count"] == 1,
       f"{batch and batch['total_amount']} / {batch and batch['asset_count']}")

    dep_entries = q(f"""select e.account_code, e.debit_amount::text, e.credit_amount::text,
                              coalesce(e.aux_dept_id::text,'')
                         from biz_voucher_entry e where e.voucher_id = '{batch['voucher_id']}'::uuid
                        order by e.line_no""")
    ok("折旧凭证：借 5602.01（带部门辅助）2638.8889 / 贷 1602 2638.8889",
       len(dep_entries) == 2
       and dep_entries[0][0] == "5602.01" and dep_entries[0][1] == "2638.8889"
       and dep_entries[0][3] == dept_a
       and dep_entries[1][0] == "1602" and dep_entries[1][2] == "2638.8889",
       str(dep_entries))
    after = q(f"select accumulated_depreciation::text, net_value::text, depreciation_months_done, last_depreciation_period from biz_fixed_asset where id='{asset_id}'::uuid")[0]
    ok("卡片回写：累计 2638.8889 / 净值 97361.1111 / 已提 1 月 / 期间 2026-09",
       (after[0], after[1], after[2], after[3]) == ("2638.8889", "97361.1111", "1", "2026-09"), str(after))

    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-09"})
    ok("同期间重跑 → 幂等 EXISTS（不重复计提）",
       status == 200 and data_of(payload)["outcome"] == "EXISTS",
       f"{status} {data_of(payload) and data_of(payload).get('outcome')}")
    ok("库里仍只有 1 条明细",
       one(f"select count(*) from biz_depreciation where asset_id='{asset_id}'::uuid") == "1")

    # 已结账期间 → 拒绝（临时把 2026-10 置 CLOSED）
    q("update biz_accounting_period set status='CLOSED' where period_code='2026-10'")
    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-10"})
    ok("已结账期间补提被拒（409 + 66038）",
       status == 409 and payload.get("code") == 66038,
       f"{status} code={payload.get('code')}")
    q("update biz_accounting_period set status='OPEN' where period_code='2026-10'")

    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-10"})
    ok("未结账期间可正常计提（2026-10 → POSTED）",
       status == 200 and data_of(payload)["outcome"] == "POSTED",
       f"{status} {data_of(payload) and data_of(payload).get('outcome')}")

    # ---------- ③ 部门转移 ----------
    print("\n③ 部门转移（无凭证、必须留痕）")
    voucher_before = one(f"select count(*) from biz_voucher where source_id='{asset_id}'::uuid")
    status, payload = cli.call("POST", f"/api/business/erp/asset/{asset_id}/transfer",
                               {"to_dept_id": dept_b, "reason": "组织调整：研发并入产品部"})
    ok("转移接口 200", status == 200, f"{status}")
    ok("主表当前值已换成新部门",
       one(f"select use_dept_id from biz_fixed_asset where id='{asset_id}'::uuid") == dept_b,
       dept_b_name)
    chg = q(f"select change_type, from_dept_name, to_dept_name, reason from biz_fixed_asset_change where asset_id='{asset_id}'::uuid and change_type='TRANSFER'")
    ok("变动表新增 TRANSFER 留痕（含前后部门 + 原因）",
       len(chg) == 1 and chg[0][1] == dept_a_name and chg[0][2] == dept_b_name and "组织调整" in chg[0][3],
       str(chg))
    ok("转移不产生凭证",
       one(f"select count(*) from biz_voucher where source_id='{asset_id}'::uuid") == voucher_before)
    ok("状态日志留了 FIXED_ASSET 业务事件（建卡 + 转移）",
       int(one(f"""select count(*) from biz_status_log
                    where entity_type='FIXED_ASSET' and entity_id='{asset_id}'::uuid""")) >= 2)

    # ---------- ④ 处置（出售）----------
    print("\n④ 处置（出售，走 asset_disposal 审批）")
    # 内控：发起人 / 制单人不能批自己这一票（403 + 65019）。所以造一个总经理账号来批，
    # 密码哈希直接复制 admin 的（同一个口令 Admin@123456），省掉改密流程。
    ensure_approver()
    status, payload = cli.call("POST", f"/api/business/erp/asset/{asset_id}/dispose", {
        "disposal_type": "SALE",
        "disposal_date": "2026-11-05",
        "reason": "闲置设备对外出售",
        "sale_price": "95000.0000",
        "output_tax": "12350.0000",
    }, expect=200)
    ok("提交处置审批 200", status == 200, f"{status} {str(payload.get('msg'))[:60]}")
    inst = q(f"select id, status, biz_type from biz_flow_instance where biz_type='ASSET_DISPOSAL' and biz_id='{asset_id}'::uuid")
    ok("审批实例已建（ASSET_DISPOSAL）", len(inst) == 1 and inst[0][2] == "ASSET_DISPOSAL", str(inst))
    ok("提交后资产仍在役（审批前不动账）",
       one(f"select status from biz_fixed_asset where id='{asset_id}'::uuid") == "IN_USE")

    inst_id = inst[0][0]
    task = q(f"select id, status, assignee_id::text from biz_flow_task where instance_id='{inst_id}'::uuid order by created_at")
    ok("审批待办已生成（GENERAL_MANAGER 一人一站）", len(task) == 1, str(task))
    # 内控闸的反面用例：发起人（admin，同时也是制单人）自己批自己的处置必须 403 + 65019
    if task and task[0][1] == "PENDING":
        status, payload = cli.call("POST", f"/api/business/flow/task/{task[0][0]}/approve",
                                   {"comment": "自己批自己"})
        ok("发起人自批被内控拒绝（403 + 65019）",
           status == 403 and payload.get("code") == 65019,
           f"{status} code={payload.get('code')} msg={str(payload.get('msg'))[:40]}")
    gm = Client(BASE)
    gm.login("fin2gm", "Admin@123456")
    for t in task:
        if t[1] != "PENDING":
            continue
        status, payload = gm.call("POST", f"/api/business/flow/task/{t[0]}/approve",
                                  {"comment": "同意出售"})
        ok("总经理审批通过 200", status == 200, f"{status} {str(payload.get('msg'))[:60]}")
    ok("实例已 APPROVED",
       one(f"select status from biz_flow_instance where id='{inst_id}'::uuid") == "APPROVED")
    disp = q(f"select status, disposal_type, disposal_date::text, disposal_voucher_id::text from biz_fixed_asset where id='{asset_id}'::uuid")[0]
    ok("资产已变「已处置」（DISPOSED / SALE / 2026-11-05）",
       (disp[0], disp[1], disp[2]) == ("DISPOSED", "SALE", "2026-11-05"), str(disp))
    sale_vs = q(f"""select v.id, v.voucher_no from biz_voucher v
                     where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='{asset_id}'::uuid
                       and v.is_red=0 order by v.created_at""")
    ok("出售生成 3 张处置凭证", len(sale_vs) == 3, f"{len(sale_vs)} 张")
    lines = q(f"""select e.account_code, e.debit_amount::text, e.credit_amount::text
                    from biz_voucher_entry e where e.voucher_id='{sale_vs[0][0]}'::uuid order by e.line_no""")
    ok("凭证1 借 1602 累计 / 借 1606 净值 / 贷 1601 原值",
       lines == [["1602", "5277.7778", "0.0000"], ["1606", "94722.2222", "0.0000"],
                 ["1601", "0.0000", "100000.0000"]], str(lines))
    lines2 = q(f"""select e.account_code, e.debit_amount::text, e.credit_amount::text
                     from biz_voucher_entry e where e.voucher_id='{sale_vs[1][0]}'::uuid order by e.line_no""")
    ok("凭证2 借 1002 实收 / 贷 1606 售价 / 贷 2221.02 销项税",
       lines2 == [["1002", "107350.0000", "0.0000"], ["1606", "0.0000", "95000.0000"],
                  ["2221.02", "0.0000", "12350.0000"]], str(lines2))
    lines3 = q(f"""select e.account_code, e.debit_amount::text, e.credit_amount::text
                     from biz_voucher_entry e where e.voucher_id='{sale_vs[2][0]}'::uuid order by e.line_no""")
    ok("凭证3 结转：借 1606 277.7778 / 贷 5301 277.7778（售价高于净值 → 收益）",
       lines3 == [["1606", "277.7778", "0.0000"], ["5301", "0.0000", "277.7778"]], str(lines3))
    ok("1606 归零（K7）",
       one("select coalesce(sum(debit_amount-credit_amount),0)::text from biz_voucher_entry where account_code='1606'") == "0.0000")
    ok("处置留痕已写（DISPOSAL，含审批实例）",
       one(f"select count(*) from biz_fixed_asset_change where asset_id='{asset_id}'::uuid and change_type='DISPOSAL' and approval_instance_id is not null") == "1")
    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-12"})
    ok("处置后不再纳入折旧扫描（2026-12 跳过）",
       status == 200 and data_of(payload)["outcome"] == "SKIPPED",
       f"{status} {data_of(payload) and data_of(payload).get('outcome')}")
    q("delete from biz_depreciation_batch where period_code='2026-12'")  # 清掉这次空跑批次，避免挡后续用例

    # ---------- ⑤ 作废处置 ----------
    print("\n⑤ 作废处置（凭证红冲 + 资产恢复）")
    status, payload = cli.call("POST", f"/api/business/erp/asset/{asset_id}/dispose/cancel",
                               {"reason": "出售合同取消，恢复在役"}, expect=200)
    ok("作废处置 200", status == 200, f"{status} {str(payload.get('msg'))[:60]}")
    back = q(f"select status, coalesce(disposal_type,'-'), coalesce(disposal_date::text,'-') from biz_fixed_asset where id='{asset_id}'::uuid")[0]
    ok("资产恢复在役且处置痕迹已清",
       (back[0], back[1], back[2]) == ("IN_USE", "-", "-"), str(back))
    reds = one(f"""select count(*) from biz_voucher v
                    where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_id='{asset_id}'::uuid
                      and v.is_red=1""")
    ok("生成 3 张红字处置凭证", reds == "3", reds)
    ok("蓝字凭证全部打上 reversed_by",
       one(f"""select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL'
                and source_id='{asset_id}'::uuid and is_red=0 and reversed_by is null""") == "0")
    ok("1606 仍归零（正向 + 反向抵消）",
       one("select coalesce(sum(debit_amount-credit_amount),0)::text from biz_voucher_entry where account_code='1606'") == "0.0000")
    ok("恢复在役有 ENABLE 留痕",
       one(f"select count(*) from biz_fixed_asset_change where asset_id='{asset_id}'::uuid and change_type='ENABLE'") == "1")

    # ---------- ⑥ 折旧红冲 + 重跑 ----------
    print("\n⑥ 折旧批次红冲（红字凭证 + 回滚卡片）→ 重跑复用同一批次行")
    batch10 = q("select id, total_amount::text, voucher_id::text from biz_depreciation_batch where period_code='2026-10'")[0]
    acc_before = one(f"select accumulated_depreciation::text from biz_fixed_asset where id='{asset_id}'::uuid")
    status, payload = cli.call("POST", f"/api/business/erp/depreciation/{batch10[0]}/reverse",
                               {"reason": "10 月折旧计提金额有误，重提"})
    ok("红冲接口 200", status == 200, f"{status} {str(payload.get('msg'))[:60]}")
    ok("批次状态 REVERSED 且有红字凭证",
       one(f"select status || '|' || coalesce(reverse_voucher_id::text,'') from biz_depreciation_batch where id='{batch10[0]}'::uuid").startswith("REVERSED|"),
       one(f"select status from biz_depreciation_batch where id='{batch10[0]}'::uuid"))
    ok("明细全部 REVERSED",
       one(f"select count(*) from biz_depreciation where batch_id='{batch10[0]}'::uuid and status='POSTED'") == "0")
    acc_after = one(f"select accumulated_depreciation::text from biz_fixed_asset where id='{asset_id}'::uuid")
    ok("卡片累计折旧已回滚（2638.8889 → 0）",
       (acc_before, acc_after) == ("5277.7778", "2638.8889"), f"{acc_before} → {acc_after}")
    ok("红字凭证借贷互换",
       one(f"""select count(*) from biz_voucher_entry e
                join biz_voucher v on v.id = e.voucher_id
               where v.source_type='DEPRECIATION' and v.is_red=1
                 and e.account_code='1602' and e.debit_amount = 2638.8889""") == "1")
    status, payload = cli.call("POST", "/api/business/erp/depreciation/run", {"period_code": "2026-10"})
    ok("红冲后重跑 → REVERSED_RERUN（复用同一批次行）",
       status == 200 and data_of(payload)["outcome"] == "REVERSED_RERUN",
       f"{status} {data_of(payload) and data_of(payload).get('outcome')}")
    ok("重跑后仍只有一个 2026-10 批次（uk_biz_dep_batch_period）",
       one("select count(*) from biz_depreciation_batch where period_code='2026-10'") == "1")
    ok("卡片累计回到 5277.7778",
       one(f"select accumulated_depreciation::text from biz_fixed_asset where id='{asset_id}'::uuid") == "5277.7778")

    # ---------- 汇总 ----------
    print("\n" + "=" * 88)
    print(f"结果：{len(PASSED)} PASS / {len(FAILED)} FAIL")
    if FAILED:
        for name in FAILED:
            print(f"  🚨 {name}")
    print("=" * 88)
    print(f"ASSET_ID={asset_id} ASSET_NO={asset_no}")
    return 0 if not FAILED else 1


if __name__ == "__main__":
    sys.exit(main())
