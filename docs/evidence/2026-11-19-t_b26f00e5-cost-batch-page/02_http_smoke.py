#!/usr/bin/env python3
"""M5-07 批次成本页 · 真跑 HTTP 冒烟（8 条接口 / 前端消费的字段逐条验）。

不依赖任何第三方库（urllib），跑在一台真起着的实例上：
  - 登录（admin / Admin@123456）
  - 建一个成本对象（M-W1A-005 / WH01 / DEPT_DEMO），拿到 object_id
  - 归集表 / 明细 / 三路来源 / 结转试算 / 在制余额 / 台账：逐条断言**前端要用的字段真的在**
  - 归集（真写：本期没来源 → 0 行写入）、结转（被在制数量拦下 → 409 + 逐条原因）
  - 404 口径：随机 uuid 的 6 条接口都必须是 404（不是 500 / 403）
退出码：0 = 全绿；1 = 有断言失败。
"""
import json
import sys
import urllib.error
import urllib.request
import uuid

BASE = "http://127.0.0.1:18243"
PERIOD = "2026-11"
DEPT_DEMO = "01a1f0a4-1ea3-779f-aa75-2698715d197c"
PRODUCT = "d1370003-0000-4000-8000-000000000005"  # M-W1A-005 FINISHED
WAREHOUSE = "d1370001-0000-4000-8000-000000000001"  # WH01

TOKEN = ""
FAILS = []


def call(method, path, body=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOKEN:
        req.add_header("Authorization", "Bearer " + TOKEN)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw[:400]}


def check(label, cond, detail=""):
    ok = "PASS" if cond else "FAIL"
    print(f"  [{ok}] {label}" + (f" — {detail}" if detail and not cond else ""))
    if not cond:
        FAILS.append(label)


def keys_present(payload, keys, label):
    missing = [k for k in keys if k not in payload]
    check(label, not missing, f"缺字段 {missing}")


def section(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


# ---------- ① 登录 ----------
section("① 登录 admin")
status, res = call("POST", "/api/system/auth/login", {"username": "admin", "password": "Admin@123456"})
check("登录 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} res={res}")
TOKEN = ((res.get("data") or {}).get("access_token")) or ""
check("拿到 access_token", bool(TOKEN), "data.access_token 为空")
if not TOKEN:
    print("\n登录失败，后续无法继续")
    sys.exit(1)

# ---------- ①b 打开成本核算开关 ----------
# `erp.cost.enabled` 默认 false：关着时 `/business/erp/cost-*` 一律 403 + 66079
# （`business::shared::cost_switch`）。演练要跑真接口，先让实例重载参数快照
# （库里的值由调用方 psql 改成 true，切换不重启）。
section("①b POST /api/support/config/reload（打开 erp.cost.enabled）")
status, res = call("POST", "/api/support/config/reload")
check("重载参数 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")

# ---------- ② 建成本对象 ----------
section("② 建成本对象（批次）")
status, res = call(
    "POST",
    "/api/business/erp/cost-object",
    {
        "product_id": PRODUCT,
        "target_warehouse_id": WAREHOUSE,
        "plan_qty": 100,
        "workshop_dept_id": DEPT_DEMO,
        "start_date": "2026-11-02",
        "due_date": "2026-11-30",
        "source_doc_no": "SO-2026-018",
        "lot_no": "LOT-M507",
    },
)
check("建批 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
obj = res.get("data") or {}
object_id = obj.get("id") or obj.get("object_id") or ""
check("拿到 object id", bool(object_id), f"data keys={list(obj)[:12]}")
print(f"  批次号 = {obj.get('object_no')}  id = {object_id}")
if not object_id:
    sys.exit(1)

# ---------- ③ 归集表（M5-02） ----------
section("③ GET /cost-collect-sheet（归集表）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet?period_code={PERIOD}")
check("归集表 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
data = res.get("data") or {}
keys_present(data, ["period_code", "totals", "list", "total", "page", "page_size"], "出参含分页 + 整表合计")
# ⚠️ 真跑抓到的形状：`SheetTotalsRes` 对 amounts 用了 `#[serde(flatten)]` ⇒ **扁的**，
# 不是 `{amounts:{mat,...}}`。前端 TS 已按扁形状写（cost-batch-kinds::SheetTotals）。
keys_present((data.get("totals") or {}), ["mat", "labor", "oh", "total", "line_count", "batch_count"], "totals 是扁平四金额 + 两计数（页脚合计）")
rows = data.get("list") or []
check("列表里有刚建的批次", any(r.get("object_id") == object_id for r in rows), f"total={data.get('total')}")
if rows:
    keys_present(
        rows[0],
        [
            "object_id", "object_no", "object_type", "source_doc_no", "lot_no", "product_id",
            "product_code", "product_name", "workshop_dept_id", "workshop_dept_name",
            "plan_qty", "completed_qty", "wip_qty", "completion_pct", "status",
            "start_date", "due_date", "mat", "labor", "oh", "total", "line_count", "last_occur_date",
        ],
        "SheetRow 字段（页面 16 列全在这；金额同样是扁的）",
    )

# ---------- ④ 明细（M5-02 #2） ----------
section("④ GET /cost-collect-sheet/{id}（归集明细）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet/{object_id}?period_code={PERIOD}")
check("明细 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
detail = res.get("data") or {}
keys_present(detail, ["period_code", "batch", "totals", "lines"], "明细出参结构")
keys_present(detail.get("totals") or {}, ["mat", "labor", "oh", "total"], "明细 totals 是 SheetAmounts（**嵌套**，与列表的扁平不同）")
keys_present(detail.get("batch") or {}, ["object_id", "object_no", "product_name", "status", "wip_qty", "completion_pct"], "batch 抬头字段")
print(f"  本期归集行数 = {len(detail.get('lines') or [])}（新建批应为 0）")

# ---------- ⑤ 三路来源（M5-03） ----------
section("⑤ GET /cost-collect-sheet/{id}/sources（三路来源）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet/{object_id}/sources?period_code={PERIOD}")
check("来源 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
src = res.get("data") or {}
keys_present(
    src,
    ["period_code", "batch", "period_batch", "collectable", "block_reason", "block_fix",
     "recalc_required", "material", "labor", "overhead", "totals", "after_totals"],
    "BatchSources 顶层字段",
)
for leg in ("material", "labor", "overhead"):
    keys_present(
        src.get(leg) or {},
        ["element", "element_label", "rows", "rows_total", "amount_total", "collected_rows",
         "collected_amount", "pending_rows", "pending_amount", "purged_rows", "purged_amount",
         "uncollected_rows", "uncollected_amount", "after_amount"],
        f"{leg} 要素块字段（页面卡片摘要用）",
    )
check("新批可归集（collectable=true）", src.get("collectable") is True, f"collectable={src.get('collectable')} reason={src.get('block_reason')}")
keys_present(src.get("totals") or {}, ["mat", "labor", "oh", "total"], "来源 totals 嵌套四字段")
keys_present(src.get("after_totals") or {}, ["mat", "labor", "oh", "total"], "来源 after_totals 嵌套四字段")

# ---------- ⑥ 归集执行（M5-03 写） ----------
section("⑥ POST /cost-collect-sheet/{id}/collect（归集本期）")
status, res = call("POST", f"/api/business/erp/cost-collect-sheet/{object_id}/collect", {"period_code": PERIOD})
check("归集 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
col = res.get("data") or {}
keys_present(col, ["period_code", "batch", "period_batch", "recalc_required", "material", "labor", "overhead", "totals", "message"], "BatchCollect 顶层字段")
for leg in ("material", "labor", "overhead"):
    keys_present(
        col.get(leg) or {},
        ["element", "element_label", "scanned", "inserted", "skipped_existing", "purged",
         "inserted_amount", "purged_amount", "collected_rows", "collected_amount",
         "uncollected_rows", "uncollected_amount"],
        f"{leg} 归集结果字段（页面回执用）",
    )
print(f"  回执 = {col.get('message')}")

# ---------- ⑦ 结转试算（M5-04） ----------
section("⑦ GET /cost-collect-sheet/{id}/finish-preview（结转试算）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet/{object_id}/finish-preview?period_code={PERIOD}")
check("试算 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
prev = res.get("data") or {}
keys_present(
    prev,
    ["batch", "period_code", "period_batch", "collected", "transferred", "pending",
     "finished_qty", "unit_cost", "cumulative_unit_cost", "transferable", "transfer_no",
     "settled_in_books", "history", "blockers", "warnings"],
    "BatchFinishPreview 顶层字段",
)
for leg in ("collected", "transferred", "pending"):
    keys_present(prev.get(leg) or {}, ["material", "labor", "overhead", "total"], f"{leg} 金额四字段")
blockers = prev.get("blockers") or []
check("新批（在制 100）被闸门拦下", len(blockers) > 0, f"blockers={blockers}")
if blockers:
    keys_present(blockers[0], ["code", "message", "fix"], "FinishBlocker 三字段（页面整段展示）")
    print(f"  第一条闸门 = [{blockers[0].get('code')}] {blockers[0].get('message')}")

# ---------- ⑧ 结转执行（被拦 → 409） ----------
section("⑧ POST /cost-collect-sheet/{id}/finish（应被 409 拦下）")
status, res = call("POST", f"/api/business/erp/cost-collect-sheet/{object_id}/finish", {"period_code": PERIOD, "remark": "M5-07 冒烟"})
check("结转被拦 409", status == 409, f"status={status}")
check("报文带业务码与「怎么修」", bool(res.get("msg")) and res.get("code", 0) != 0, f"code={res.get('code')} msg={str(res.get('msg'))[:120]}")
print(f"  409 code={res.get('code')}")

# ---------- ⑨ 在制余额（M5-05） ----------
section("⑨ GET /cost-collect-sheet/wip-balance（在制余额）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet/wip-balance?period_code={PERIOD}")
check("余额表 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
wip = res.get("data") or {}
keys_present(wip, ["period_code", "totals", "list", "total", "page", "page_size"], "WipBalanceRes 结构")
keys_present(wip.get("totals") or {}, ["collected", "transferred", "balance", "period_in", "period_out", "batch_count", "wip_batch_count", "negative_batch_count"], "WipTotals 字段")
wrows = wip.get("list") or []
check("余额表里有刚建的批次", any(r.get("object_id") == object_id for r in wrows), f"total={wip.get('total')}")
if wrows:
    keys_present(
        wrows[0],
        ["object_id", "object_no", "collected", "transferred", "balance", "period_in", "period_out",
         "collect_lines", "transfer_times", "first_period", "last_period", "has_balance", "warnings"],
        "WipRow 字段（页面 16 列全在这）",
    )

# ---------- ⑩ 在制台账（M5-05） ----------
section("⑩ GET /cost-collect-sheet/{id}/wip-ledger（在制台账）")
status, res = call("GET", f"/api/business/erp/cost-collect-sheet/{object_id}/wip-ledger?period_code={PERIOD}")
check("台账 200 且 code=0", status == 200 and res.get("code") == 0, f"status={status} msg={res.get('msg')}")
led = res.get("data") or {}
keys_present(
    led,
    ["period_code", "batch", "opening", "summary", "periods", "truncated", "after_period", "settled", "warnings", "notes"],
    "WipLedgerRes 顶层字段",
)
keys_present(led.get("summary") or {}, ["collected", "transferred", "balance", "collect_lines", "transfer_times"], "WipSummary 字段")
periods = led.get("periods") or []
check("含截止期间那一行", any(p.get("period_code") == PERIOD for p in periods), f"periods={[p.get('period_code') for p in periods]}")
if periods:
    keys_present(periods[-1], ["period_code", "opening", "inflow", "outflow", "closing", "collect_lines", "transfer_times"], "LedgerPeriod 字段（页面台账表）")

# ---------- ⑪ 404 口径（看不见 = 404，不是 403/500） ----------
section("⑪ 随机 uuid：6 条带 id 的接口都应 404")
ghost = str(uuid.uuid4())
for method, path, body in (
    ("GET", f"/api/business/erp/cost-collect-sheet/{ghost}?period_code={PERIOD}", None),
    ("GET", f"/api/business/erp/cost-collect-sheet/{ghost}/sources?period_code={PERIOD}", None),
    ("POST", f"/api/business/erp/cost-collect-sheet/{ghost}/collect", {"period_code": PERIOD}),
    ("GET", f"/api/business/erp/cost-collect-sheet/{ghost}/finish-preview?period_code={PERIOD}", None),
    ("POST", f"/api/business/erp/cost-collect-sheet/{ghost}/finish", {"period_code": PERIOD}),
    ("GET", f"/api/business/erp/cost-collect-sheet/{ghost}/wip-ledger?period_code={PERIOD}", None),
):
    status, res = call(method, path, body)
    check(f"{method} {path.split('?')[0]} → 404", status == 404, f"status={status} msg={str(res.get('msg'))[:80]}")

# ---------- ⑫ 期间必填（前端兜底的存在理由） ----------
section("⑫ 不带 period_code 的列表 → 400（前端的期间兜底就是为这个）")
status, res = call("GET", "/api/business/erp/cost-collect-sheet")
check("400 + 不可用期间", status == 400, f"status={status} msg={str(res.get('msg'))[:100]}")

print("\n" + "=" * 78)
if FAILS:
    print(f"结果：{len(FAILS)} 项失败")
    for item in FAILS:
        print("  ✗ " + item)
    sys.exit(1)
print("结果：全绿 ✅（8 条接口 + 404 口径 + 期间必填，全部与前端契约一致）")
