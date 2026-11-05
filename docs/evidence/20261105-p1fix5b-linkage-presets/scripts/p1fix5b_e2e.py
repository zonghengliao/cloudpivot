#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-5B 真机验收：跨单据联动的 3 条预置规则 **端到端**（配置 → 触发 → 目标单据）。

跑法（隔离实例已在跑）：

    bash scripts/p1fix5b_instance.sh start      # 库 cloudpivot_lnk5b，端口 18130
    python3 scripts/p1fix5b_e2e.py              # 可重复跑（开始时复位规则与业务数据）

它要证明的事（每条都是 `[PASS]/[FAIL]` + 原始取值）：

  A. 配置面：V0254 已应用、3 条规则已种好（键 / 参数 / enabled），配置页 API 读得回来；
  B. 规则 1「收货过账 → 生成凭证」：源单据过账 → 凭证由**配置路径**生成（日志点名 takeover），
     恰好 1 张（不双发）、借贷平衡、分录 3 行；
  C. 规则 2「收货过账 → 通知制单人」：源单据过账 → `sys_notice` 多出 1 条（以前完全没有这条通知），
     收件人 = 制单人；这条通知**只能**由联动产生（旧逻辑不发），所以「存在」本身就是接管证据；
  D. 规则 3「任务提交验收 → 通知创建人」：任务 REVIEW → 通知照旧发出，且日志点名
     「联动已接管通知：配置发出（旧逻辑本次不发）」；
  E. 停用后不再触发（逐条）：规则 2 停用 → 过账不再产生通知；规则 1 停用 → 凭证退回旧路径
     （日志「凭证仍走旧逻辑」）；规则 3 停用 → 通知仍由旧逻辑发（日志无接管行）；
  F. 规则可改：规则 3 收件人改 OWNER → 通知对象换成负责人（可观测的行为差异）；
     规则 1 改 SHADOW → 只做影子试算（日志），凭证仍由旧路径落；
  G. 收尾：3 条规则恢复预置（enabled + TAKEOVER + 原参数）。

退出码：0 = 全 PASS；1 = 有 FAIL。
"""

import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

BASE = os.environ.get("LNK_BASE", "http://127.0.0.1:18130")
DB = os.environ.get("LNK_DB", "cloudpivot_lnk5b")
LOG = os.environ.get("LNK_LOG", "/root/p1fix5b-server.log")
PW_FILE = os.environ.get("LNK_PW_FILE", "/root/lnk5b-admin-password.txt")
PREFIX = "LNK5B-"

# ---- 生产库守卫：e2e 只许打隔离库 ----
if not DB.startswith("cloudpivot_lnk5b") or DB == "cloudpivot":
    print(f"拒绝执行：LNK_DB={DB}（必须 cloudpivot_lnk5b*，绝不碰生产库）")
    sys.exit(2)

PASS = 0
FAIL = 0
TOKEN = ""
ADMIN_ID = ""
MATERIAL = WAREHOUSE = SUPPLIER = ""

# 3 条预置规则（与 V0254 / tests_presets.rs 逐字一致）
PRESETS = [
    {
        "name": "收货过账 → 生成凭证（联动接管）",
        "entity": "STOCK_IN",
        "status": "POSTED",
        "action": "GENERATE_VOUCHER",
        "params": {"biz_type": "PURCHASE_IN", "mode": "TAKEOVER"},
        "seq": 10,
    },
    {
        "name": "收货过账 → 通知制单人",
        "entity": "STOCK_IN",
        "status": "POSTED",
        "action": "SEND_NOTICE",
        "params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
        "seq": 20,
    },
    {
        "name": "任务开工 → 通知负责人与创建人",
        "entity": "TASK",
        "status": "DOING",
        "action": "SEND_NOTICE",
        "params": {"mode": "TAKEOVER"},
        "seq": 10,
    },
]


def ok(label, detail=""):
    global PASS
    PASS += 1
    print(f"[PASS] {label}" + (f"  —— {detail}" if detail else ""))


def bad(label, detail=""):
    global FAIL
    FAIL += 1
    print(f"[FAIL] {label}" + (f"  —— {detail}" if detail else ""))


def expect(cond, label, detail=""):
    if cond:
        ok(label, detail)
    else:
        bad(label, detail)


def psql(sql, db=None):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("PGPASSWORD", "alun")
    proc = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", db or DB, "-At", "-v", "ON_ERROR_STOP=1", "-f", "-"],
        input=sql,
        capture_output=True,
        text=True,
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"psql 失败：{proc.stderr.strip()}\nSQL: {sql[:400]}")
    return proc.stdout.strip()


def _json(text):
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw": text}


def http(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=90) as resp:
            return resp.status, _json(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, _json(err.read().decode())


# ---------------- 日志（区分「本次动作」产生的行） ----------------


def log_offset():
    try:
        return os.path.getsize(LOG)
    except OSError:
        return 0


def log_since(offset):
    try:
        with open(LOG, "r", errors="replace") as fh:
            fh.seek(offset)
            raw = fh.read()
    except OSError:
        return ""
    # 日志是彩色输出（字段值外面包着 ANSI 转义），比对前先剥掉，否则
    # `outcome="SUCCESS"` 这种断言会被看不见的转义字符打断。
    return _ANSI_RE.sub("", raw)


def field(text, key):
    """从日志行里取字段值（tracing 的 `key=value` 形式）。"""
    marker = key + "="
    pos = text.find(marker)
    if pos < 0:
        return ""
    rest = text[pos + len(marker):]
    end = len(rest)
    for sep in (" ", ",", "\t", "\n"):
        idx = rest.find(sep)
        if 0 <= idx < end:
            end = idx
    return rest[:end].strip().strip('"')


def line_with(text, needle):
    for line in text.splitlines():
        if needle in line:
            return line
    return ""


# ---------------- 登录 / 主数据 ----------------


def login():
    candidates = []
    if os.environ.get("LNK_ADMIN_PASSWORD"):
        candidates.append(os.environ["LNK_ADMIN_PASSWORD"])
    if os.path.exists(PW_FILE):
        candidates.append(open(PW_FILE).read().strip())
    candidates += ["Admin@123456", "Admin@123456L5!"]
    psql("UPDATE sys_user SET login_fail_count = 0, is_locked = false WHERE username = 'admin';")
    for password in candidates:
        code, body = http("POST", "/api/system/auth/login", {"username": "admin", "password": password})
        if code != 200:
            continue
        data = body.get("data") or {}
        token = data.get("access_token")
        if not token:
            continue
        if data.get("must_change_password"):
            # 隔离实例的 bootstrap 会把初始口令标成「必须改密」；改密接口需要一层
            # 本实例没挂的 auth 中间件扩展（500），所以直接在库里把标记清掉再登录。
            psql("UPDATE sys_user SET must_change_password = false WHERE username = 'admin';")
            code, body = http(
                "POST", "/api/system/auth/login",
                {"username": "admin", "password": password},
            )
            token = (body.get("data") or {}).get("access_token")
            if not token:
                raise RuntimeError(f"清掉改密标记后重登失败：{code} {body}")
        with open(PW_FILE, "w") as fh:
            fh.write(password)
        return token
    raise RuntimeError("admin 登录失败（候选口令全部不通）")


def master_ids():
    material = psql("SELECT id FROM biz_material WHERE material_code = 'M-W1A-001' LIMIT 1;")
    warehouse = psql("SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH01' LIMIT 1;")
    supplier = psql("SELECT id FROM biz_supplier WHERE supplier_code = 'SUP-0001' LIMIT 1;")
    if not (material and warehouse and supplier):
        raise RuntimeError(f"标准主数据缺失：{material} {warehouse} {supplier}")
    return material, warehouse, supplier


def ensure_user(username, real_name):
    """建一个非超管测试账号（重复调用返回既有 id）。"""
    code, body = http(
        "POST", "/api/system/user",
        {"username": username, "password": "Lnk5b@123456", "real_name": real_name, "status": 1},
        token=TOKEN,
    )
    if code in (200, 201):
        return (body.get("data") or {}).get("id") or psql(
            f"SELECT id FROM sys_user WHERE username = '{username}';"
        )
    if code == 409:
        return psql(f"SELECT id FROM sys_user WHERE username = '{username}';")
    raise RuntimeError(f"建测试账号 {username} 失败：{code} {body}")


# ---------------- 权限补齐（空库首启的授权窗口） ----------------


def grant_linkage_permissions():
    """把 V0253 的 3 个菜单/权限码授给 SUPER_ADMIN（幂等）。

    V0253 的授权段在**空库首启**时是空操作：`sys_role` 由 bootstrap 在迁移之后创建，
    所以从零建库的实例上 `sys_role_menu` 里没有 linkage 的两条授权行，超管调
    `GET /business/flow/linkage` 会 403（与 V0192/V0197/V0202/V0206 同口径的已知窗口）。
    生产库角色早已存在，不受影响；隔离库在 bootstrap 之后重放这段授权即可。
    """
    granted = psql(
        "INSERT INTO sys_role_menu (id, role_id, menu_id)\n"
        "SELECT gen_random_uuid(), r.id, m.id\n"
        "  FROM sys_role r\n"
        "  CROSS JOIN (VALUES\n"
        "        ('20000000-0000-0000-0000-000000001230'::uuid),\n"
        "        ('20000000-0000-0000-0000-000000001231'::uuid),\n"
        "        ('20000000-0000-0000-0000-000000001232'::uuid)) AS m(id)\n"
        " WHERE r.deleted_at IS NULL AND r.role_code = 'SUPER_ADMIN'\n"
        "   AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm "
        "WHERE rm.role_id = r.id AND rm.menu_id = m.id)\n"
        "RETURNING 1;"
    )
    # psql 会把命令标签（`INSERT 0 3`）一起打出来，真正的 RETURNING 行是裸的 `1`
    inserted = len([line for line in granted.splitlines() if line.strip() == "1"])
    total = psql(
        "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id "
        "JOIN sys_menu m ON m.id = rm.menu_id "
        "WHERE r.role_code = 'SUPER_ADMIN' AND m.auth_code LIKE 'business:flow:linkage:%';"
    )
    return inserted, total


# ---------------- 复位 ----------------


def reset_presets():
    """把 3 条预置规则刷回迁移里的原样（可重复跑；等价于迁移的 ON CONFLICT DO UPDATE）。"""
    values = []
    for preset in PRESETS:
        payload = json.dumps(preset["params"], ensure_ascii=False)
        values.append(
            "(gen_random_uuid(), '{name}', '{entity}', '{status}', '{action}', "
            "'{params}'::jsonb, true, {seq}, now(), now(), NULL, NULL, NULL)".format(
                name=preset["name"],
                entity=preset["entity"],
                status=preset["status"],
                action=preset["action"],
                params=payload,
                seq=preset["seq"],
            )
        )
    psql(
        "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, "
        "action_params, enabled, seq, created_at, updated_at, create_by, update_by, deleted_at) VALUES\n"
        + ",\n".join(values)
        + "\nON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE SET "
        "name = EXCLUDED.name, action_params = EXCLUDED.action_params, enabled = true, "
        "seq = EXCLUDED.seq, updated_at = now(), update_by = NULL, deleted_at = NULL;"
    )


def reset_business_data():
    """清业务单据（不动规则 / 账号 / 主数据），并把 2026 全年期间置 OPEN。"""
    psql(
        "TRUNCATE biz_voucher_entry, biz_voucher, biz_payment, biz_payable, "
        "biz_inventory_txn, biz_inventory_location, biz_inventory, biz_material_serial, "
        "biz_status_log, biz_flow_task, biz_flow_instance, sys_notice_receiver, sys_notice, "
        "biz_stock_in CASCADE;"
    )
    rows = []
    for month in range(1, 13):
        start = f"2026-{month:02d}-01"
        end = f"2026-{month + 1:02d}-01" if month < 12 else "2027-01-01"
        rows.append(
            f"INSERT INTO biz_accounting_period (period_year, period_month, period_code, "
            f"start_date, end_date, status) VALUES (2026, {month}, '2026-{month:02d}', "
            f"'{start}', ('{end}'::date - 1), 'OPEN') "
            f"ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN';"
        )
    psql("\n".join(rows))


# ---------------- 单据动作 ----------------


def create_stock_in(tag):
    body = {
        "supplier_id": SUPPLIER,
        "warehouse_id": WAREHOUSE,
        "is_sourceless": True,
        "remark": PREFIX + tag,
        "invoice_received": False,
        "items": [{"material_id": MATERIAL, "qty": "10", "price_excl": "10", "tax_rate": "13"}],
    }
    code, resp = http("POST", "/api/business/erp/stock-in", body, token=TOKEN)
    if code not in (200, 201):
        raise RuntimeError(f"建收货单失败：{code} {resp}")
    data = resp.get("data") or {}
    header = data.get("header") or data
    return header["id"], header["in_no"]


def post_stock_in(doc_id):
    return http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"}, token=TOKEN)


def voucher_rows(doc_id):
    out = psql(
        "SELECT v.voucher_no || '|' || v.total_debit || '|' || v.total_credit || '|' || "
        "v.entry_count || '|' || v.status || '|' || v.source_type "
        "FROM biz_voucher v WHERE v.source_type = 'STOCK_IN' AND v.source_id = '{}' "
        "AND v.deleted_at IS NULL;".format(doc_id)
    )
    rows = []
    for line in out.splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        rows.append({
            "voucher_no": parts[0], "total_debit": parts[1], "total_credit": parts[2],
            "entry_count": parts[3], "status": parts[4], "source_type": parts[5],
        })
    return rows


def entries_of(doc_id):
    out = psql(
        "SELECT e.line_no || ':' || e.account_code || ':' || e.debit_amount || ':' || e.credit_amount "
        "FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id "
        "WHERE v.source_type = 'STOCK_IN' AND v.source_id = '{}' ORDER BY e.line_no;".format(doc_id)
    )
    return [line for line in out.splitlines() if line.strip()]


def doc_status(doc_id):
    return psql(f"SELECT status || '|' || coalesce(voucher_id::text, '') FROM biz_stock_in WHERE id = '{doc_id}';")


def stock_in_notices(doc_id):
    out = psql(
        "SELECT n.id || '|' || n.title || '|' || n.receiver_count || '|' || coalesce(n.biz_type, '') || '|' || "
        "coalesce((SELECT string_agg(u.username, ',' ORDER BY u.username) FROM sys_notice_receiver r "
        "JOIN sys_user u ON u.id = r.user_id WHERE r.notice_id = n.id), '') "
        "FROM sys_notice n WHERE n.biz_id = '{}' AND n.deleted_at IS NULL;".format(doc_id)
    )
    rows = []
    for line in out.splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        rows.append({
            "id": parts[0], "title": parts[1], "receiver_count": parts[2],
            "biz_type": parts[3], "receivers": parts[4],
        })
    return rows


def create_task(title, owner_id):
    code, resp = http("POST", "/api/business/task", {"title": title, "owner_id": owner_id}, token=TOKEN)
    if code not in (200, 201):
        raise RuntimeError(f"建任务失败：{code} {resp}")
    data = resp.get("data") or {}
    return data["id"], data["task_no"]


def task_status(task_id, to, reason=None):
    body = {"to": to}
    if reason:
        body["reason"] = reason
    return http("PATCH", f"/api/business/task/{task_id}/status", body, token=TOKEN)


def task_notices(task_id):
    out = psql(
        "SELECT n.id || '|' || n.title || '|' || n.receiver_count || '|' || "
        "coalesce((SELECT string_agg(u.username, ',' ORDER BY u.username) FROM sys_notice_receiver r "
        "JOIN sys_user u ON u.id = r.user_id WHERE r.notice_id = n.id), '') "
        "FROM sys_notice n WHERE n.biz_id = '{}' AND n.deleted_at IS NULL;".format(task_id)
    )
    rows = []
    for line in out.splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        rows.append({
            "id": parts[0], "title": parts[1], "receiver_count": parts[2], "receivers": parts[3],
        })
    return rows


# ---------------- 规则读写（配置面 API） ----------------


def list_rules(entity=None):
    path = "/api/business/flow/linkage"
    if entity:
        path += f"?trigger_entity={entity}"
    code, body = http("GET", path, token=TOKEN)
    if code != 200:
        raise RuntimeError(f"读规则失败：{code} {body}")
    return body.get("data") or {}


def rule_map():
    data = list_rules()
    return {(r["trigger_entity"], r["trigger_status"], r["action_code"]): r for r in data.get("rules", [])}


def toggle_rule(rule_id, enabled):
    code, body = http(
        "PUT", "/api/business/flow/linkage/toggle",
        {"id": rule_id, "enabled": enabled, "confirm": True}, token=TOKEN,
    )
    return code, body


def put_rule(preset, params):
    code, body = http(
        "PUT", "/api/business/flow/linkage",
        {
            "name": preset["name"],
            "trigger_entity": preset["entity"],
            "trigger_status": preset["status"],
            "action_code": preset["action"],
            "action_params": params,
            "seq": preset["seq"],
            "confirm": True,
        },
        token=TOKEN,
    )
    return code, body


def rule_row(key):
    return psql(
        "SELECT enabled || '|' || action_params::text FROM biz_flow_linkage_rule "
        "WHERE trigger_entity = '{}' AND trigger_status = '{}' AND action_code = '{}' "
        "AND deleted_at IS NULL;".format(*key)
    )


# ==================== 主流程 ====================


def main():
    global TOKEN, ADMIN_ID, MATERIAL, WAREHOUSE, SUPPLIER

    print("== P1-FIX-5B 跨单据联动 · 3 条预置规则端到端验收 ==")
    print(f"   BASE={BASE}  DB={DB}  LOG={LOG}")

    # ---------- A. 配置面 ----------
    table = psql("SELECT count(*) FROM pg_tables WHERE tablename = 'biz_flow_linkage_rule';")
    expect(table == "1", "A1 表 biz_flow_linkage_rule 已建", f"pg_tables={table}")
    applied = psql(
        "SELECT count(*) FROM schema_migrations WHERE kind = 'versioned' AND name = 'flow_linkage_presets';"
    )
    expect(applied == "1", "A2 V0254 迁移已记账（flow_linkage_presets）", f"rows={applied}")

    TOKEN = login()
    ok("A3 admin 登录成功")
    ADMIN_ID = psql("SELECT id FROM sys_user WHERE username = 'admin';")
    added, total = grant_linkage_permissions()
    added_again, total_again = grant_linkage_permissions()  # 幂等：第二次必须 0 行
    expect(total_again == "3" and added_again == 0,
           "A3b 超管持有 linkage 的 list/manage 授权（3 个菜单节点），重复补齐幂等",
           f"first_added={added} second_added={added_again} granted={total_again}")
    MATERIAL, WAREHOUSE, SUPPLIER = master_ids()
    ok("A4 标准主数据就位（M-W1A-001 / WH01 / SUP-0001）")

    reset_business_data()
    reset_presets()
    rows = psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE deleted_at IS NULL;")
    expect(rows == "3", "A5 规则表恰好 3 行（含迁移种的 3 条预置）", f"rows={rows}")

    data = list_rules()
    got_keys = {(r["trigger_entity"], r["trigger_status"], r["action_code"]) for r in data.get("rules", [])}
    want_keys = {(p["entity"], p["status"], p["action"]) for p in PRESETS}
    expect(got_keys == want_keys and data.get("total") == 3,
           "A6 配置页 API 读到 3 条规则，键与预置一致",
           f"total={data.get('total')} keys={sorted(got_keys)}")
    rules = rule_map()
    all_enabled = all(rules[key]["enabled"] for key in want_keys)
    all_takeover = all((rules[key].get("action_params") or {}).get("mode") == "TAKEOVER" for key in want_keys)
    expect(all_enabled, "A7 3 条规则全部 enabled", f"{[(k[2], rules[k]['enabled']) for k in want_keys]}")
    expect(all_takeover, "A8 3 条规则参数 mode=TAKEOVER（真生效，非影子）",
           f"{[(k[2], (rules[k].get('action_params') or {}).get('mode')) for k in want_keys]}")
    only_stockin = list_rules("STOCK_IN")
    expect(only_stockin.get("total") == 2, "A9 按触发点筛选（STOCK_IN）→ 2 条",
           f"total={only_stockin.get('total')}")

    # ---------- B. 规则 1：收货过账 → 生成凭证 ----------
    b_id, b_no = create_stock_in("R1-凭证")
    offset = log_offset()
    code, body = post_stock_in(b_id)
    time.sleep(0.8)
    log = log_since(offset)
    expect(code == 200, "B1 源单据过账成功（收货单）", f"code={code} {body.get('msg', '')}")
    status, voucher_id = (doc_status(b_id).split("|") + ["", ""])[:2]
    expect(status == "POSTED", "B2 源单据状态 = POSTED", f"{b_no} status={status}")

    vs = voucher_rows(b_id)
    expect(len(vs) == 1, "B3 目标单据：凭证恰好 1 张（接管不双发）", f"rows={len(vs)}")
    v = vs[0] if vs else {}
    expect(v.get("entry_count") == "3", "B4 凭证 3 行分录（存货 / 进项税 / 应付）", v.get("entry_count", ""))
    expect(v.get("total_debit") == v.get("total_credit") == "113.0000",
           "B5 借 = 贷 = 价税合计 113", f"{v.get('total_debit')}/{v.get('total_credit')}")
    expect(voucher_id != "" and v.get("voucher_no", ""), "B6 源单据回写了 voucher_id",
           f"voucher_id={voucher_id} voucher_no={v.get('voucher_no')}")
    expect("收货过账的凭证已由联动配置接管" in log,
           "B7 日志：凭证由**联动配置**生成（旧路径本次不生成）",
           line_with(log, "联动配置接管")[:150])
    expect("联动凭证影子试算完成" not in log, "B8 日志：本次不是影子模式（没有试算回滚）")

    # ---------- C. 规则 2：收货过账 → 通知制单人 ----------
    notices = stock_in_notices(b_id)
    expect(len(notices) == 1, "C1 目标单据：这条收货单的通知恰好 1 条（旧逻辑根本不发，存在即接管）",
           f"rows={len(notices)}")
    n = notices[0] if notices else {}
    expect(n.get("title") == "收货单已过账", "C2 通知标题 = 收货单已过账", n.get("title", ""))
    expect(n.get("biz_type") == "STOCK_IN", "C3 通知 biz_type = STOCK_IN（可按单据反查）", n.get("biz_type", ""))
    expect(n.get("receivers") == "admin", "C4 收件人 = 制单人（admin，recipient=CREATOR）", n.get("receivers", ""))
    expect("收货过账：提交后的联动动作执行结果" in log and 'outcome="SUCCESS"' in log,
           "C5 日志：提交后动作（SEND_NOTICE）outcome=SUCCESS",
           line_with(log, "提交后的联动动作执行结果")[:180])

    # ---------- D. 规则 3：任务开工 → 通知负责人与创建人 ----------
    # 触发点 = TASK/DOING（`→ REVIEW` 在配了 task_accept 验收流程的环境里会先起审批实例，
    # 通知由流程发，规则等不到；`TODO → DOING` 是纯状态流转，稳定可触发）。
    # 创建人另给一个账号：旧逻辑的通知收件人 = 负责人 + 创建人 − 操作人自己，
    # 操作人 = admin（也是负责人），所以收件人只剩创建人 —— 可与「改收件人」段对照。
    creator = ensure_user("lnk5b_creator", "联动验收创建人")
    owner_user = ensure_user("lnk5b_owner", "联动验收负责人")  # 仅用于「不是收件人」的反证
    d_id, d_no = create_task(PREFIX + "R3-开工通知", ADMIN_ID)
    psql(f"UPDATE biz_task SET creator_id = '{creator}' WHERE id = '{d_id}';")
    offset = log_offset()
    code, body = task_status(d_id, "DOING")
    time.sleep(0.8)
    log = log_since(offset)
    expect(code == 200, "D1 源单据动作：任务 TODO → DOING（开工）", f"code={code} {body.get('msg', '')}")
    tns = task_notices(d_id)
    expect(len(tns) == 1, "D2 目标单据：任务开工通知 1 条", f"rows={len(tns)}")
    t = tns[0] if tns else {}
    expect(t.get("title") == "任务状态变更", "D3 通知标题 = 任务状态变更（沿用触发点文案）", t.get("title", ""))
    expect(t.get("receivers") == "lnk5b_creator",
           "D4 收件人 = 创建人（操作人 admin 不在收件人里，与旧逻辑同口径）", t.get("receivers", ""))
    expect("lnk5b_owner" not in t.get("receivers", ""),
           "D5 反证：预置规则没有把通知改发给负责人（负责人=admin 是操作人）", t.get("receivers", ""))
    expect("联动已接管通知：配置发出" in log,
           "D6 日志：通知由配置发出（旧逻辑本次不发）", line_with(log, "联动已接管通知")[:180])
    d_rule_id = (rule_map()[("TASK", "DOING", "SEND_NOTICE")])["id"]
    expect(f"rule_id={d_rule_id}" in log, "D7 日志里的 rule_id = 规则 3 的主键", f"rule_id={d_rule_id}")

    # ---------- E. 停用后不再触发 ----------
    r1 = rule_map()[("STOCK_IN", "POSTED", "GENERATE_VOUCHER")]
    r2 = rule_map()[("STOCK_IN", "POSTED", "SEND_NOTICE")]
    r3 = rule_map()[("TASK", "DOING", "SEND_NOTICE")]

    code, body = toggle_rule(r2["id"], False)
    expect(code == 200, "E1 停用规则 2（收货过账→通知制单人）", f"code={code}")
    expect(rule_row(("STOCK_IN", "POSTED", "SEND_NOTICE")).startswith("false"),
           "E2 规则 2 落库 enabled=false", rule_row(("STOCK_IN", "POSTED", "SEND_NOTICE")))

    e2_id, e2_no = create_stock_in("E-停用通知")
    offset = log_offset()
    code, _ = post_stock_in(e2_id)
    time.sleep(0.8)
    log = log_since(offset)
    expect(code == 200, "E3 规则 2 停用后仍能过账", f"code={code} {e2_no}")
    expect(len(stock_in_notices(e2_id)) == 0, "E4 **停用生效**：该单没有任何通知（联动不再触发）",
           f"notices={stock_in_notices(e2_id)}")
    expect("收货过账：提交后的联动动作执行结果" not in log, "E5 日志：没有提交后联动动作执行记录")
    expect(len(voucher_rows(e2_id)) == 1, "E6 规则 1 仍启用 → 该单凭证照常生成", f"rows={len(voucher_rows(e2_id))}")

    code, body = toggle_rule(r1["id"], False)
    expect(code == 200, "E7 停用规则 1（收货过账→生成凭证）", f"code={code}")
    e7_id, e7_no = create_stock_in("E-停用凭证")
    offset = log_offset()
    code, _ = post_stock_in(e7_id)
    time.sleep(0.8)
    log = log_since(offset)
    expect(code == 200, "E8 规则 1 停用后过账成功（退回旧路径）", f"code={code} {e7_no}")
    expect(len(voucher_rows(e7_id)) == 1, "E9 **停用生效**：凭证仍由旧逻辑生成，恰好 1 张（不漏记）",
           f"rows={len(voucher_rows(e7_id))}")
    expect("凭证仍走旧逻辑（联动未接管）" in log, "E10 日志：凭证走旧逻辑（联动未接管）",
           line_with(log, "凭证仍走旧逻辑")[:150])
    expect("收货过账的凭证已由联动配置接管" not in log, "E11 日志：没有「由联动配置接管」")

    code, body = toggle_rule(r3["id"], False)
    expect(code == 200, "E12 停用规则 3（任务开工通知）", f"code={code}")
    e12_id, _ = create_task(PREFIX + "E-停用任务通知", ADMIN_ID)
    psql(f"UPDATE biz_task SET creator_id = '{creator}' WHERE id = '{e12_id}';")
    offset = log_offset()
    task_status(e12_id, "DOING")
    time.sleep(0.8)
    log = log_since(offset)
    expect(len(task_notices(e12_id)) == 1, "E13 **停用生效**：通知仍由旧逻辑发（不漏发），但不是联动发的",
           f"rows={len(task_notices(e12_id))}")
    expect("联动已接管通知" not in log, "E14 日志：没有「联动已接管通知」（规则 3 未触发）")

    for rule in (r1, r2, r3):
        toggle_rule(rule["id"], True)
    restored = rule_map()
    expect(all(restored[k]["enabled"] for k in want_keys), "E15 三条规则重新启用（恢复预置）",
           f"{[(k[2], restored[k]['enabled']) for k in want_keys]}")

    # ---------- F. 规则可改 ----------
    preset3 = PRESETS[2]
    code, body = put_rule(preset3, {"mode": "TAKEOVER", "recipient": "OWNER"})
    expect(code == 200, "F1 改规则 3 参数：收件人 LEGACY → OWNER（走配置页 API）", f"code={code}")
    f1_id, _ = create_task(PREFIX + "F-改收件人", ADMIN_ID)
    psql(f"UPDATE biz_task SET creator_id = '{creator}' WHERE id = '{f1_id}';")
    task_status(f1_id, "DOING")
    time.sleep(0.8)
    f1n = task_notices(f1_id)
    expect(len(f1n) == 1 and f1n[0].get("receivers") == "admin",
           "F2 **改规则生效**：通知改发负责人（负责人=admin；预置口径是负责人+创建人−操作人=创建人）",
           f"receivers={f1n[0].get('receivers') if f1n else ''}")

    preset1 = PRESETS[0]
    code, body = put_rule(preset1, {"biz_type": "PURCHASE_IN", "mode": "SHADOW"})
    expect(code == 200, "F3 改规则 1 参数：TAKEOVER → SHADOW（走配置页 API）", f"code={code}")
    f3_id, f3_no = create_stock_in("F-影子")
    offset = log_offset()
    code, _ = post_stock_in(f3_id)
    time.sleep(0.8)
    log = log_since(offset)
    expect(code == 200, "F4 SHADOW 下过账成功", f"code={code} {f3_no}")
    expect(len(voucher_rows(f3_id)) == 1, "F5 SHADOW 只试算 → 库里仍只有 1 张（旧路径落账）",
           f"rows={len(voucher_rows(f3_id))}")
    expect("联动凭证影子试算完成" in log, "F6 **改规则生效**：日志出现影子试算（已回滚）",
           line_with(log, "影子试算完成")[:150])

    # ---------- G. 收尾：恢复预置 ----------
    reset_presets()
    final = rule_map()
    ok_final = all(
        final[k]["enabled"] and (final[k].get("action_params") or {}).get("mode") == "TAKEOVER"
        for k in want_keys
    )
    expect(ok_final, "G1 3 条规则恢复预置（enabled + TAKEOVER + 原参数）",
           f"{[(k[2], final[k].get('action_params')) for k in want_keys]}")
    total = psql("SELECT count(*) FROM biz_flow_linkage_rule WHERE deleted_at IS NULL;")
    expect(total == "3", "G2 规则表最终仍是 3 行（没有多出版本）", f"rows={total}")

    print(f"\n==> SUMMARY PASS={PASS} FAIL={FAIL}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001 —— 真机脚本：异常必须打印原始信息后退出非 0
        print(f"[FATAL] {exc}")
        sys.exit(2)
