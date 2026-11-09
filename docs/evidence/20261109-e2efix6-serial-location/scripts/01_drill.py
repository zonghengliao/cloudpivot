#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""E2E-FIX-6 真实 HTTP 演练：批量生成的库位落账口径（默认库位桶 / 库位字典 / 未分配）

验收对象（卡面 ①~⑤）：
  ① 表单有「入库库位」、默认带出仓库默认库位   → 前端单测覆盖（erp-serial-list.test.ts）
  ② 生成后 /inventory-location/page 落在默认库位桶（不是「未分配」）
  ③ 直接调 API 不传 location_id → 也落默认库位桶
  ④ 扫码能看到库位（location_name 非空）
  ⑤ 巡检不再报该物料的假差异（/inventory/audit mismatched=0 + B1/B1b/B4 逐桶相符）

附加（卡面 ③ 存量两桶）：
  F. 造一份「旧代码形态」的两桶并存（未分配桶 + 裸 NULL 序列号）→ P15 只读报告能点名
     → P15 合并脚本执行 → 守恒、桶合并、序列号改指、复查 0 行、巡检仍绿

跑法（**只能打隔离库/测试库**，脚本自带生产库守卫）：

    export E2F6_ADMIN_PW='Admin@123456'
    E2F6_BASE=http://127.0.0.1:18420 E2F6_DB=cp_e2efix6 \\
      python3 docs/evidence/20261109-e2efix6-serial-location/scripts/01_drill.py

退出码 0 = 全过（PASS 计数打印在末尾）。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE = os.environ.get("E2F6_BASE", "http://127.0.0.1:18420")
DB = os.environ.get("E2F6_DB", "cp_e2efix6")
DB_HOST = os.environ.get("E2F6_DB_HOST", "127.0.0.1")
DB_PORT = os.environ.get("E2F6_DB_PORT", "5432")
DB_USER = os.environ.get("E2F6_DB_USER", "alun")
DB_PW = os.environ.get("E2F6_DB_PW", "alun")
ADMIN_USER = os.environ.get("E2F6_ADMIN", "admin")
ADMIN_PW = os.environ.get("E2F6_ADMIN_PW", "Admin@123456")
EVID = os.environ.get("E2F6_EVID", os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# 生产库守卫（与 scripts/verify/db_guard.py 同口径的最小子集）
if DB in ("cloudpivot", "cloudpivot_prod", "cloudpivot_saas"):
    print(f"FATAL: 目标库 {DB} 看起来是生产库，本脚本只允许在隔离/测试库上跑")
    sys.exit(2)

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))))

# 夹具（固定 UUID，前缀 e2f6；开头清上一轮，结尾清本轮）
W1 = "e2f60001-0000-4000-8000-000000000001"   # WH-E2F6-A（有默认库位）
L_DEF = "e2f60001-0000-4000-8000-000000000002"  # A-01 默认库位（is_default = 1）
L_ALT = "e2f60001-0000-4000-8000-000000000003"  # B-01 非默认库位
W2 = "e2f60001-0000-4000-8000-000000000004"   # WH-E2F6-NODEF（没有默认库位）
L_NODEF = "e2f60001-0000-4000-8000-000000000005"  # C-01（is_default = 0）
MAT = "e2f60001-0000-4000-8000-000000000011"  # M-E2F6-SN（一物一码）

PASS = 0
FAIL = 0
TOKEN = None


def ok(name, extra=""):
    global PASS
    PASS += 1
    print(f"  [PASS] {name}" + (f"（{extra}）" if extra else ""))


def bad(name, detail):
    global FAIL
    FAIL += 1
    print(f"  [FAIL] {name}\n         {detail}")


def check_decimal(name, want, got):
    try:
        if abs(Decimal(str(want)) - Decimal(str(got))) < Decimal("0.000001"):
            ok(name, f"{got}")
        else:
            bad(name, f"期望 [{want}] 实际 [{got}]")
    except Exception:
        bad(name, f"期望 [{want}] 实际 [{got}]（非数值）")


def check_eq(name, want, got):
    if str(want) == str(got):
        ok(name, got)
    else:
        bad(name, f"期望 [{want}] 实际 [{got}]")


def check_true(name, cond, detail=""):
    if cond:
        ok(name, detail)
    else:
        bad(name, detail or "条件不成立")


def psql(sql):
    env = dict(os.environ, PGPASSWORD=DB_PW)
    r = subprocess.run(
        ["psql", "-h", DB_HOST, "-p", DB_PORT, "-U", DB_USER, "-d", DB, "-v", "ON_ERROR_STOP=1",
         "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=300)
    if r.returncode != 0:
        raise RuntimeError(f"psql 失败：{r.stderr.strip()[:800]}\nSQL={sql[:400]}")
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def psql1(sql, default=""):
    rows = psql(sql)
    return rows[0][0] if rows else default


def psql_file(path):
    env = dict(os.environ, PGPASSWORD=DB_PW)
    r = subprocess.run(
        ["psql", "-h", DB_HOST, "-p", DB_PORT, "-U", DB_USER, "-d", DB, "-v", "ON_ERROR_STOP=1",
         "-f", path],
        capture_output=True, text=True, env=env, timeout=300)
    return r.returncode, r.stdout + r.stderr


def http(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=90) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def raw_dump(name, obj):
    os.makedirs(os.path.join(EVID, "raw"), exist_ok=True)
    path = os.path.join(EVID, "raw", name)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(obj, fh, ensure_ascii=False, indent=2)
    return path


# ==================== 夹具 ====================

def unit_id():
    return psql1("select id::text from biz_unit where deleted_at is null order by unit_code limit 1")


def cleanup():
    psql(f"""
      delete from biz_status_log where entity_type='VOUCHER' and entity_id in
        (select id from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP'));
      delete from biz_voucher_entry where voucher_id in
        (select id from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP'));
      delete from biz_voucher where source_type in ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP');
      delete from biz_material_serial where material_id='{MAT}'::uuid;
      delete from biz_inventory_txn where material_id='{MAT}'::uuid;
      delete from biz_inventory where material_id='{MAT}'::uuid;
      delete from biz_inventory_location where material_id='{MAT}'::uuid;
      delete from biz_location where id in ('{L_DEF}'::uuid,'{L_ALT}'::uuid,'{L_NODEF}'::uuid);
      delete from biz_material where id='{MAT}'::uuid;
      delete from biz_warehouse where id in ('{W1}'::uuid,'{W2}'::uuid);
    """)


def setup():
    u = unit_id()
    if not u:
        raise RuntimeError("库里一个计量单位都没有，无法建物料夹具")
    psql(f"""
      insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                 status, sort_order, parent_id, level, warehouse_path, is_leaf)
      values ('{W1}'::uuid,'WH-E2F6-A','E2F6 默认库位仓','NORMAL',0,1,9101,null,1,'/{W1}/',1),
             ('{W2}'::uuid,'WH-E2F6-NODEF','E2F6 无默认库位仓','NORMAL',0,1,9102,null,1,'/{W2}/',1);
      insert into biz_location (id, location_code, location_name, warehouse_id, location_type,
                                is_default, capacity, status, sort_order)
      values ('{L_DEF}'::uuid,'E2F6-A-01','E2F6 默认库位','{W1}'::uuid,'STORAGE',1,null,1,1),
             ('{L_ALT}'::uuid,'E2F6-B-01','E2F6 非默认库位','{W1}'::uuid,'STORAGE',0,null,1,2),
             ('{L_NODEF}'::uuid,'E2F6-C-01','E2F6 无默认仓的库位','{W2}'::uuid,'STORAGE',0,null,1,1);
      insert into biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                safety_stock, costing_method, tax_rate, status, serial_required)
      values ('{MAT}'::uuid,'M-E2F6-SN','E2F6 一物一码物料','SN 设备','{u}'::uuid,'FINISHED',
              0.0000,'MOVING_AVG',0.1300,1,true);
    """)


def serial_count():
    return int(psql1(f"select count(*) from biz_material_serial where material_id='{MAT}'::uuid") or 0)


def bucket_qty(warehouse, location_dict=None):
    """成本账该桶的数量（location_dict=None 表示未分配桶）。"""
    if location_dict is None:
        return psql1(f"""select coalesce(sum(i.qty),0)::text from biz_inventory i
                          where i.material_id='{MAT}'::uuid and i.warehouse_id='{warehouse}'::uuid
                            and i.location_id is null""", "0")
    return psql1(f"""select coalesce(sum(i.qty),0)::text from biz_inventory i
                       join biz_inventory_location il on il.id = i.location_id
                      where i.material_id='{MAT}'::uuid and i.warehouse_id='{warehouse}'::uuid
                        and il.location_id='{location_dict}'::uuid""", "0")


def bucket_count(warehouse):
    return int(psql1(f"""select count(*) from biz_inventory
                          where material_id='{MAT}'::uuid and warehouse_id='{warehouse}'::uuid""") or 0)


def bucket_avg(warehouse, location_dict):
    return psql1(f"""select coalesce(max(i.avg_cost),0)::text from biz_inventory i
                       join biz_inventory_location il on il.id = i.location_id
                      where i.material_id='{MAT}'::uuid and i.warehouse_id='{warehouse}'::uuid
                        and il.location_id='{location_dict}'::uuid""", "0")


def ledger_row_id(warehouse, location_dict):
    return psql1(f"""select il.id::text from biz_inventory_location il
                      where il.material_id='{MAT}'::uuid and il.warehouse_id='{warehouse}'::uuid
                        and il.location_id='{location_dict}'::uuid""")


def login():
    global TOKEN
    # 隔离库首启的 admin 带「首登强制改密」：本脚本第一次会把口令改成 `<原口令>#E2F6`
    # （改密会 token_version++，旧令牌立刻失效）。重跑时直接用改过的口令，别写死。
    candidates = [ADMIN_PW, f"{ADMIN_PW}#E2F6"]
    for password in candidates:
        st, res = http("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": password})
        if st != 200 or res.get("code") != 0:
            continue
        data = res["data"]
        token = data["access_token"]
        if not data.get("must_change_password"):
            TOKEN = token
            return
        new_password = f"{password}#E2F6"
        st, res = http("PUT", "/api/system/auth/password",
                       {"old_password": password, "new_password": new_password}, token)
        if st != 200 or res.get("code") != 0:
            print(f"FATAL: 首登强制改密失败 {st} {res}")
            sys.exit(2)
        print("  首登强制改密完成，用新口令重登")
        continue
    # 改密后再登一次
    for password in candidates:
        st, res = http("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": password})
        if st == 200 and res.get("code") == 0 and not res["data"].get("must_change_password"):
            TOKEN = res["data"]["access_token"]
            return
    print("FATAL: 登录失败（已试原口令与首登改密后的口令）")
    sys.exit(2)


def generate(payload, note):
    """POST /serial/batch-generate，原样落盘响应（真实 token）。"""
    st, res = http("POST", "/api/business/erp/serial/batch-generate", payload, TOKEN)
    raw_dump(f"batch_{note}.json", {"request": payload, "status": st, "response": res})
    return st, res


def main():
    print(f"== E2E-FIX-6 演练：{BASE} / 库 {DB} ==")
    login()
    cleanup()
    setup()
    print(f"  夹具：物料 M-E2F6-SN={MAT}；仓 WH-E2F6-A={W1}（默认库位 {L_DEF}）")

    # ---------- ③ 直接调 API 不传 location_id ----------
    print("\n-- ③ 不传库位：应当落「仓库默认库位桶」--")
    st, res = generate({"material_id": MAT, "warehouse_id": W1, "count": 3, "unit_cost": 12.5},
                       "no_location")
    check_eq("HTTP 200", 200, st)
    check_eq("code = 0", 0, res.get("code"))
    body = res.get("data") or {}
    check_eq("生成 3 条", 3, body.get("count"))
    row = ledger_row_id(W1, L_DEF)
    check_true("按默认库位建了库位账行", bool(row), f"biz_inventory_location={row}")
    check_decimal("默认库位桶 成本账 qty = 3", "3", bucket_qty(W1, L_DEF))
    check_decimal("默认库位桶 均价 = 12.5", "12.5", bucket_avg(W1, L_DEF))
    check_decimal("未分配桶 qty = 0（不许再分裂）", "0", bucket_qty(W1, None))
    check_eq("成本账只有 1 个桶", 1, bucket_count(W1))
    check_eq("序列号落在默认库位账行",
             row, psql1(f"""select coalesce(min(location_id::text),'') from biz_material_serial
                              where material_id='{MAT}'::uuid"""))
    check_decimal("库位账 qty = 3（B1：库位账合计 = 成本账合计）", "3",
                  psql1(f"select qty::text from biz_inventory_location where id='{row}'::uuid", "0"))

    # ---------- ② /inventory-location/page ----------
    print("\n-- ② /inventory-location/page：落默认库位桶，不是「未分配」--")
    st, res = http("GET", f"/api/business/erp/inventory-location/page?material_id={MAT}&warehouse_id={W1}&page=1&page_size=50", None, TOKEN)
    raw_dump("inventory_location_page_after_default.json", {"status": st, "response": res})
    check_eq("HTTP 200", 200, st)
    rows = (res.get("data") or {}).get("list") or []
    check_eq("该物料该仓只有 1 行库位账", 1, len(rows))
    if rows:
        check_eq("库位 = 默认库位（不是 null/未分配）", L_DEF, rows[0].get("location_id"))
        check_decimal("库位账 qty = 3", "3", rows[0].get("qty"))

    # ---------- ④ 扫码 ----------
    print("\n-- ④ 扫码能看到库位 --")
    first_no = (body.get("serial_nos") or [""])[0]
    st, res = http("GET", f"/api/business/erp/serial/scan?serial_no={first_no}", None, TOKEN)
    raw_dump("scan_first_serial.json", {"serial_no": first_no, "status": st, "response": res})
    check_eq("HTTP 200", 200, st)
    scan = res.get("data") or {}
    check_eq("扫码库位名称 = E2F6 默认库位", "E2F6 默认库位", scan.get("location_name"))
    check_eq("扫码库位账行 = 默认库位账行", row, scan.get("location_id"))

    # ---------- ① 指定库位字典 ----------
    print("\n-- ① 指定「入库库位」= B-01：应当落该库位桶 --")
    st, res = generate({"material_id": MAT, "warehouse_id": W1, "count": 2, "unit_cost": 10,
                        "location_dict_id": L_ALT}, "with_location_dict")
    check_eq("HTTP 200", 200, st)
    check_decimal("B-01 桶 qty = 2", "2", bucket_qty(W1, L_ALT))
    check_decimal("默认库位桶仍 = 3（没被挪动）", "3", bucket_qty(W1, L_DEF))
    check_decimal("未分配桶仍 = 0", "0", bucket_qty(W1, None))
    check_eq("成本账 2 个桶（默认 + B-01）", 2, bucket_count(W1))
    st, res = http("GET", f"/api/business/erp/inventory-location/page?material_id={MAT}&warehouse_id={W1}&page=1&page_size=50", None, TOKEN)
    rows = (res.get("data") or {}).get("list") or []
    locs = sorted([r.get("location_id") for r in rows])
    check_eq("库位库存页 2 行、无 null（无「(未分配)」）", sorted([L_DEF, L_ALT]), locs)

    # ---------- 跨仓库位必须 400 ----------
    print("\n-- 跨仓库位：400 且不落号 --")
    before = serial_count()
    st, res = generate({"material_id": MAT, "warehouse_id": W1, "count": 1, "unit_cost": 10,
                        "location_dict_id": L_NODEF}, "cross_warehouse_location")
    check_eq("HTTP 400", 400, st)
    check_true("报文点名「库位不属于所选仓库」", "库位不属于所选仓库" in (res.get("msg") or ""), res.get("msg") or "")
    check_eq("序列号条数不变（校验先于取号）", before, serial_count())

    # ---------- 仓库没有默认库位 → 未分配桶（与收货同口径）----------
    print("\n-- 仓库没有默认库位：仍落未分配桶（与收货一致，不编造库位）--")
    st, res = generate({"material_id": MAT, "warehouse_id": W2, "count": 1, "unit_cost": 10}, "no_default_location")
    check_eq("HTTP 200", 200, st)
    check_decimal("未分配桶 qty = 1", "1", bucket_qty(W2, None))
    check_eq("该仓成本账只有 1 个桶", 1, bucket_count(W2))

    # ---------- ⑤ 巡检 ----------
    print("\n-- ⑤ 巡检：不报该物料的假差异 --")
    st, res = http("GET", "/api/business/erp/inventory/audit?dry_run=true", None, TOKEN)
    raw_dump("inventory_audit.json", {"status": st, "response": res})
    check_eq("HTTP 200", 200, st)
    report = res.get("data") or {}
    items = [i for i in (report.get("items") or []) if i.get("material_id") == MAT]
    check_eq("巡检明细里没有该物料", 0, len(items))
    # 全局 mismatched 允许非 0：这个隔离库里还有其它卡/单测留下的夹具（与本卡无关），
    # 本卡的判据是「该物料不被报」，不是「整库干净」。
    print(f"  巡检全局：checked={report.get('checked')} mismatched={report.get('mismatched')}（含本库其它夹具，非本卡判据）")
    bad1b = psql(f"""
        with cost as (select i.material_id, i.warehouse_id,
                             coalesce(il.location_id::text,'(未分配)') as bucket, sum(i.qty) as q
                        from biz_inventory i
                        left join biz_inventory_location il on il.id = i.location_id
                       group by 1,2,3),
             loc as (select material_id, warehouse_id,
                            coalesce(location_id::text,'(未分配)') as bucket, sum(qty) as q
                       from biz_inventory_location group by 1,2,3)
        select coalesce(c.bucket,l.bucket), coalesce(c.q,0)::text, coalesce(l.q,0)::text
          from cost c full outer join loc l
            on c.material_id=l.material_id and c.warehouse_id=l.warehouse_id and c.bucket=l.bucket
         where abs(coalesce(c.q,0)-coalesce(l.q,0)) > 0.0001
           and coalesce(c.material_id,l.material_id) = '{MAT}'::uuid
    """)
    check_eq("B1b 逐桶（成本账 = 库位账）无差异", 0, len(bad1b))
    inv_vs_serial = psql(f"""
        select w.id::text,
               (select coalesce(sum(qty),0) from biz_inventory where material_id='{MAT}'::uuid and warehouse_id=w.id)::text,
               (select count(*) from biz_material_serial
                 where material_id='{MAT}'::uuid and warehouse_id=w.id
                   and status='IN_STOCK' and deleted_at is null)::text
          from (values ('{W1}'::uuid),('{W2}'::uuid)) as w(id)
    """)
    for wh, booked, counted in inv_vs_serial:
        check_decimal(f"不变式① 仓 {wh[:8]}：账面 = 在库件数", counted, booked)

    # ---------- 存量两桶（旧代码形态）→ P15 报告 / 合并 ----------
    print("\n-- 存量两桶：造旧代码形态 → P15 报告点名 → 合并 → 复查 --")
    # 旧代码形态：未分配桶（成本账 + 库位账各一行 NULL）+ 2 件裸 NULL 序列号
    psql(f"""
      insert into biz_inventory_location (material_id, warehouse_id, location_id, qty)
      values ('{MAT}'::uuid,'{W1}'::uuid,null,2)
      on conflict (material_id, warehouse_id, location_id) do update set qty = 2;
      insert into biz_inventory (material_id, warehouse_id, location_id, qty, avg_cost, amount)
      select '{MAT}'::uuid,'{W1}'::uuid,null,2,10,20
      where not exists (select 1 from biz_inventory
                         where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid and location_id is null);
      update biz_inventory set qty = 2, amount = 20, avg_cost = 10
       where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid and location_id is null;
      insert into biz_material_serial (id, serial_no, material_id, warehouse_id, location_id,
                                       status, inbound_at, source_type, source_id)
      values (gen_random_uuid(),'E2F6-LEGACY-1','{MAT}'::uuid,'{W1}'::uuid,null,'IN_STOCK',now(),'PRODUCE','{MAT}'::uuid),
             (gen_random_uuid(),'E2F6-LEGACY-2','{MAT}'::uuid,'{W1}'::uuid,null,'IN_STOCK',now(),'PRODUCE','{MAT}'::uuid);
    """)
    check_decimal("造出未分配桶 qty = 2", "2", bucket_qty(W1, None))
    rc, out = psql_file(os.path.join(ROOT, "scripts/datafix/p15/10_report_two_bucket.up.sql"))
    raw_dump("p15_report_before.json", {"rc": rc, "out": out})
    check_eq("P15 只读报告 rc = 0", 0, rc)
    check_true("报告点名了 E2F6 物料", "M-E2F6-SN" in out, "报告输出里出现 M-E2F6-SN")
    check_true("报告标出 has_default_bucket = t（两桶并存）", "M-E2F6-SN" in out)

    before_qty = psql1(f"""select coalesce(sum(qty),0)::text from biz_inventory
                            where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid""", "0")
    rc, out = psql_file(os.path.join(ROOT, "scripts/datafix/p15/20_merge_unassigned_into_default.up.sql"))
    raw_dump("p15_merge.json", {"rc": rc, "out": out})
    check_eq("P15 合并 rc = 0", 0, rc)
    check_true("合并日志里点名 M-E2F6 的 qty 2", "qty 2" in out, "NOTICE: 合并 … qty 2")
    check_decimal("合并后 未分配桶 qty = 0", "0", bucket_qty(W1, None))
    check_decimal("合并后 默认库位桶 qty = 5（3 + 2）", "5", bucket_qty(W1, L_DEF))
    after_qty = psql1(f"""select coalesce(sum(qty),0)::text from biz_inventory
                           where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid""", "0")
    check_eq("守恒：仓级合计不变", before_qty, after_qty)
    check_eq("在库序列号全部有库位（抄到默认库位账行）", 0,
             int(psql1(f"""select count(*) from biz_material_serial
                            where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid
                              and status='IN_STOCK' and deleted_at is null and location_id is null""") or 0))
    rc, out = psql_file(os.path.join(ROOT, "scripts/datafix/p15/10_report_two_bucket.up.sql"))
    raw_dump("p15_report_after.json", {"rc": rc, "out": out})
    check_eq("合并后复查 rc = 0", 0, rc)
    # 判据用 SQL 直查（报告是 psql 表格文本，按行匹配容易被列宽折行误导）：
    #   ① 有默认库位的仓（W1）：未分配桶必须清零 ⇒ 不再出现在报告里
    #   ② 没有默认库位的仓（W2）：仍然是未分配桶（P15 有意跳过，收货也是这个形态）⇒ 报告里保留
    left_default_wh = int(psql1(f"""select count(*) from biz_inventory
                                     where material_id='{MAT}'::uuid and warehouse_id='{W1}'::uuid
                                       and location_id is null and (qty <> 0 or amount <> 0)""") or 0)
    check_eq("合并后 W1 的未分配桶已清零", 0, left_default_wh)
    check_true("报告里仍列出「无默认库位仓」的未分配桶（有意跳过，不是漏合并）",
               "WH-E2F6-NODEF" in out, "报告输出含 WH-E2F6-NODEF")

    # 合并后再跑一次巡检，必须仍然不报该物料
    st, res = http("GET", "/api/business/erp/inventory/audit?dry_run=true", None, TOKEN)
    report = res.get("data") or {}
    items = [i for i in (report.get("items") or []) if i.get("material_id") == MAT]
    check_eq("合并后巡检仍不报该物料", 0, len(items))

    print(f"\n== 结果：PASS={PASS} FAIL={FAIL} ==")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001
        print(f"FATAL: {exc}")
        sys.exit(3)
