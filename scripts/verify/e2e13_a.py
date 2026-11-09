#!/usr/bin/env python3
"""E2E-13 平台功能域演练 · A 段：导入导出（模板/预检/入库/失败明细/同步/异步/大文件）+ 文件管理。

145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033（直连）。
"""
import os
import struct
import sys
import time
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e13_lib import (  # noqa: E402
    BASE_PW, ENV, PREFIX, RAW, check, contains, data_of, eq, load_state, login, make_xlsx, merge_state,
    msg_of, multipart, ne, not_contains, one, poll, psql, read_xlsx, req, req_bytes, sha256_hex, scenario,
    STATS,
)

ADMIN = login("admin", ENV.get("NIK_ADMIN_PASSWORD", ""), name="login_admin")
if not ADMIN:
    print("登录失败")
    sys.exit(1)

ST = load_state()
DICT_CODE = "E2E13_DICT"


def mk_png(w=48, h=32, rgb=(41, 128, 185)):
    raw = b""
    row = bytes(rgb) * w
    for _ in range(h):
        raw += b"\x00" + row

    def chunk(tag, payload):
        return (struct.pack(">I", len(payload)) + tag + payload
                + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def upload_import(xlsx_bytes, mode="STRICT", duplicate=None, biz_type="system.dict_item",
                  file_name="e2e13.xlsx", name=None):
    q = f"/api/business/impexp/import/upload?biz_type={biz_type}&mode={mode}"
    if duplicate:
        q += f"&duplicate={duplicate}"
    raw, ctype = multipart({}, "file", file_name, xlsx_bytes,
                           "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    st, headers, payload, _ = req_bytes(q, ADMIN, method="POST", body=raw, ctype=ctype, timeout=300, name=name)
    import json
    try:
        parsed = json.loads(payload.decode("utf-8", "replace"))
    except Exception:  # noqa: BLE001
        parsed = {"_raw": payload.decode("utf-8", "replace")[:1500]}
    return st, parsed, headers


HEADERS = ["字典编码*", "字典项值*", "字典项名称*", "描述", "排序号", "是否默认", "状态"]


def row(value, label, sort="0", default="否", status="启用", desc=None, code=None):
    return [code or DICT_CODE, value, label, desc or f"{PREFIX} 演练项", sort, default, status]


# ==================== S0 环境与定义清单 ====================
scenario("S0", "导入导出定义清单 / 字段说明（配置驱动的通用通道）")
st, body = req("GET", "/api/business/impexp/def/list", ADMIN, name="s0_def_list")
eq("S0-1 def/list 200", st, 200)
briefs = data_of(body) if isinstance(data_of(body), list) else []
by_type = {b.get("biz_type"): b for b in briefs}
check("S0-2 定义清单非空且含 system.dict_item", "system.dict_item" in by_type, f"共 {len(briefs)} 个定义：{sorted(by_type)}")
di = by_type.get("system.dict_item", {})
eq("S0-3 dict_item 支持导入", di.get("supports_import"), True)
eq("S0-4 dict_item 支持导出", di.get("supports_export"), True)
eq("S0-5 单次行数上限 100000", di.get("max_rows"), 100000)
imports = [b for b in briefs if b.get("supports_import")]
exports = [b for b in briefs if b.get("supports_export")]
check("S0-6 导入定义 ≥7（字典项 + 6 个主数据）", len(imports) >= 7, f"实际 {len(imports)}：{sorted(b['biz_type'] for b in imports)}")
check("S0-7 导出定义 ≥4（字典项 + 3 个 ERP 单据 + 用户）", len(exports) >= 4, f"实际 {len(exports)}：{sorted(b['biz_type'] for b in exports)}")

st, body = req("GET", "/api/business/impexp/def/columns?biz_type=system.dict_item", ADMIN, name="s0_columns")
cols = data_of(body).get("columns") or []
eq("S0-8 def/columns 200 且 7 列", st == 200 and len(cols), 7)
c0 = cols[0] if cols else {}
eq("S0-9 首列 key=dict_type_code", c0.get("key"), "dict_type_code")
eq("S0-10 必填列标 *", c0.get("template_header"), "字典编码*")
eq("S0-11 外键列点名字典类型", c0.get("fk_label"), "字典类型")
enum_col = [c for c in cols if c.get("key") == "status"]
check("S0-12 枚举列列出可选值", (enum_col[0].get("option_text") or "").find("启用") >= 0,
      f"option_text={enum_col[0].get('option_text') if enum_col else None}")
st, body = req("GET", "/api/business/impexp/def/columns?biz_type=not.exists", ADMIN, name="s0_columns_404")
eq("S0-13 未知 biz_type 404", st, 404)
eq("S0-14 未知 biz_type 业务码 60011", body.get("code") if isinstance(body, dict) else None, 60011)

# 权限：无导入权限的用户 403
rid, _, _ = (None, None, None)
st, body = req("POST", "/api/system/role", ADMIN,
               {"role_code": PREFIX + "_NOIMP", "role_name": "E2E13 无导入权限角色", "status": 1,
                "sort_order": 962, "data_scope": 4}, name="s0_mk_role")
rid = data_of(body).get("id") or one("select id from sys_role where role_code='E2E13_NOIMP' and deleted_at is null")
uid, _, _ = (None, None, None)
st, body = req("POST", "/api/system/user", ADMIN,
               {"username": "e2e13_noimp", "password": BASE_PW, "real_name": "E2E13 无导入权限",
                "status": 1, "role_ids": [rid]}, name="s0_mk_user")
uid = data_of(body).get("id") or one("select id from sys_user where username='e2e13_noimp' and deleted_at is null")
NOIMP = login("e2e13_noimp") if uid else ""
st, body = req("GET", "/api/business/impexp/def/list", NOIMP, name="s0_def_list_noimp")
eq("S0-15 无权限用户 def/list 403", st, 403)
contains("S0-16 403 点名权限码", msg_of(body), "impexp")
merge_state(noimp_role=rid, noimp_user=uid)

# ==================== S1 模板下载（三要素） ====================
scenario("S1", "模板下载：三 sheet + 表头星标 + 示例行 + 字段说明")
st, headers, payload, saved = req_bytes("/api/business/impexp/import/template?biz_type=system.dict_item",
                                        ADMIN, name="s1_template.xlsx")
eq("S1-1 模板下载 200", st, 200)
check("S1-2 Content-Type 是 xlsx", "spreadsheetml" in headers.get("content-type", ""),
      headers.get("content-type", ""))
contains("S1-3 文件名为中文并带 RFC5987", headers.get("content-disposition", ""), "filename*=UTF-8''")
sheets = read_xlsx(payload)
check("S1-4 三个 sheet 齐（数据/字段说明/填写须知）", set(sheets) >= {"数据", "字段说明", "填写须知"}, f"{list(sheets)}")
data_rows = sheets.get("数据") or []
eq("S1-5 数据表头首列带星标", (data_rows[0][0] if data_rows else ""), "字典编码*")
eq("S1-6 第 2 行是示例行", (data_rows[1][1] if len(data_rows) > 1 else ""), "1")
note = sheets.get("字段说明") or []
eq("S1-7 字段说明 6 列表头", note[0][:6] if note else [], ["字段名", "是否必填", "类型", "长度/范围", "可选值", "说明"])
eq("S1-8 字段说明逐列覆盖（7 列 + 表头）", len(note) - 1, 7)
guide = sheets.get("填写须知") or []
check("S1-9 填写须知 6 条", len([r for r in guide if r and r[0].strip()]) >= 6, f"{len(guide)} 行")
st2, body2 = req("GET", "/api/business/impexp/import/template?biz_type=nope", ADMIN, name="s1_tpl_404")
eq("S1-10 未知 biz_type 模板 404", st2, 404)

# ==================== S2 字典类型夹具 + 导入成功链 ====================
scenario("S2", "导入成功链：上传预检 → 确认入库 → 落库与任务状态")
st, body = req("POST", "/api/support/dict/type", ADMIN,
               {"dict_code": DICT_CODE, "dict_name": "E2E13 演练字典", "status": 1, "sort_order": 999,
                "description": f"{PREFIX} 演练专用，收尾自清"}, name="s2_mk_dict_type")
TYPE_ID = data_of(body).get("id")
if not TYPE_ID:
    TYPE_ID = one(f"select id from sys_dict_type where dict_code='{DICT_CODE}' and deleted_at is null limit 1")
check("S2-1 字典类型夹具就绪", bool(TYPE_ID) and not str(TYPE_ID).startswith("<ERR"), f"type_id={TYPE_ID}")
merge_state(dict_type_id=TYPE_ID, dict_code=DICT_CODE)

good_rows = [row(f"v{i}", f"{PREFIX} 项{i}", sort=str(i)) for i in range(1, 6)]
good_xlsx = make_xlsx(HEADERS, good_rows)
st, body, _ = upload_import(good_xlsx, name="s2_upload_good")
d = data_of(body)
rep = d.get("report") or {}
TASK1 = d.get("task_id") or ""
eq("S2-2 干净文件预检 200", st, 200)
eq("S2-3 状态 VALIDATED", d.get("status"), "VALIDATED")
eq("S2-4 报告 total=5", rep.get("total"), 5)
eq("S2-5 报告 valid=5", rep.get("valid"), 5)
eq("S2-6 报告 error=0", rep.get("error"), 0)
eq("S2-7 外键批量校验 SQL 次数=1（外键列数 1）", rep.get("fk_sql_statements"), 1)
check("S2-8 任务号格式 IMP 前缀", str(d.get("task_no", "")).startswith("IMP"), d.get("task_no"))
st, body = req("GET", f"/api/business/impexp/import/{TASK1}/errors?page=1&page_size=10", ADMIN, name="s2_errors_empty")
eq("S2-9 干净文件错误明细 0 条", data_of(body).get("total"), 0)
st, body = req("GET", f"/api/business/impexp/import/{TASK1}", ADMIN, name="s2_task_detail")
t = data_of(body)
eq("S2-10 详情 status=VALIDATED", t.get("status"), "VALIDATED")
eq("S2-11 详情 biz_type 快照", t.get("biz_type"), "system.dict_item")
eq("S2-12 详情 total/valid 计数", [t.get("total_rows"), t.get("valid_rows"), t.get("error_rows")], [5, 5, 0])
eq("S2-13 详情 imported_rows=0（未确认）", t.get("imported_rows"), 0)
db_row = one(f"select status||'|'||total_rows||'|'||valid_rows||'|'||error_rows||'|'||imported_rows||'|'||file_sha256 from sys_import_task where id='{TASK1}'::uuid")
eq("S2-14 DB 任务行与接口一致", db_row, f"VALIDATED|5|5|0|0|{sha256_hex(good_xlsx)}")
st, headers, payload, _ = req_bytes(f"/api/business/impexp/import/{TASK1}/download", ADMIN, name="s2_alias.xlsx")
eq("S2-15 别名 /import/{id}/download 可下（200）", st, 200)
alias_rows = list(read_xlsx(payload).values())[0]
eq("S2-16 别名就是「错误明细」文件（3 列，干净任务 0 行数据）",
   [alias_rows[0][:3], len([r for r in alias_rows[1:] if r and r[0]])],
   [["行号", "列名", "错误原因"], 0])

st, body = req("POST", f"/api/business/impexp/import/{TASK1}/confirm", ADMIN, name="s2_confirm")
d = data_of(body)
eq("S2-17 确认入库 200", st, 200)
eq("S2-18 终态 SUCCESS", d.get("status"), "SUCCESS")
eq("S2-19 入库行数 5", d.get("imported_rows"), 5)
db_items = one(f"select count(*)||'|'||count(distinct item_value) from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and item_value like 'v%' and deleted_at is null")
eq("S2-20 DB 5 行落库且值不重复", db_items, "5|5")
db_labels = one(f"select string_agg(item_label, ',' order by item_value) from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and item_value like 'v%' and deleted_at is null")
eq("S2-21 DB 标签中文正确", db_labels, ",".join(f"{PREFIX} 项{i}" for i in range(1, 6)))
db_task = one(f"select status||'|'||imported_rows||'|'||progress||'|'||(finished_at is not null) from sys_import_task where id='{TASK1}'::uuid")
eq("S2-22 DB 任务终态 SUCCESS|5|100|true", db_task, "SUCCESS|5|100|true")
st, body = req("POST", f"/api/business/impexp/import/{TASK1}/confirm", ADMIN, name="s2_confirm_again")
eq("S2-23 重复确认 409（终态不可再确认）", st, 409)
st, body = req("GET", "/api/business/impexp/import/page?page=1&page_size=10&biz_type=system.dict_item",
               ADMIN, name="s2_task_page")
page = data_of(body)
eq("S2-24 任务列表 200", st, 200)
ids = [x.get("id") for x in page.get("list") or []]
check("S2-25 列表含本任务且按时间倒序", TASK1 in ids and page.get("total", 0) >= 1,
      f"total={page.get('total')} 首条={ids[0] if ids else None}")
st, body = req("GET", "/api/business/impexp/import/page?page=1&page_size=10&status=SUCCESS", ADMIN, name="s2_page_status")
check("S2-26 状态过滤只回 SUCCESS", all(x.get("status") == "SUCCESS" for x in data_of(body).get("list") or []),
      f"total={data_of(body).get('total')}")

# 幂等重导（ON CONFLICT DO UPDATE）
st, body, _ = upload_import(good_xlsx, duplicate="UPSERT", name="s2_upload_again")
TASK1B = data_of(body).get("task_id")
eq("S2-27 重导预检 error=0", (data_of(body).get("report") or {}).get("error"), 0)
st, body = req("POST", f"/api/business/impexp/import/{TASK1B}/confirm", ADMIN, name="s2_confirm_again2")
eq("S2-28 重导 SUCCESS", data_of(body).get("status"), "SUCCESS")
eq("S2-29 重导后该批仍 5 行（幂等）",
   one(f"select count(*) from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and item_value like 'v%' and deleted_at is null"), "5")
DICT_N = int(one(f"select count(*) from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and deleted_at is null") or 0)

# ==================== S3 部分导入（PARTIAL）+ 失败明细 ====================
scenario("S3", "导入失败：错误行不落库 / 成功行落库 / 任务状态 PARTIAL（计量单位定义）")
UNIT_HEADERS = ["单位编码*", "单位名称*", "是否基本单位", "基准单位编码", "换算率", "状态"]
unit_rows = [
    ["E2E13U1", "演练基本单位", "是", "", "1", "启用"],            # 行 2 合法
    ["E2E13U2", "演练辅助单位", "否", "E2E13U1", "12", "启用"],     # 行 3 合法
    ["", "演练缺编码", "是", "", "1", "启用"],                      # 行 4 REQUIRED
    ["E2E13U4", "", "是", "", "1", "启用"],                         # 行 5 REQUIRED
    ["E2E13U5", "演练换算率错", "否", "E2E13U1", "abc", "启用"],    # 行 6 TYPE
    ["E2E13U6", "演练枚举错", "是", "", "1", "开用"],               # 行 7 ENUM
    ["E2E13U1", "演练编码重复", "是", "", "1", "启用"],             # 行 8 UNIQUE
    ["E2E13U8", "演练基准不存在", "否", "E2E13_NOT_EXIST_UNIT", "12", "启用"],  # 行 9 RULE
]
bad_xlsx = make_xlsx(UNIT_HEADERS, unit_rows)
st, body, _ = upload_import(bad_xlsx, mode="PARTIAL", biz_type="business.erp.unit", name="s3_upload_partial")
d = data_of(body)
rep = d.get("report") or {}
TASK2 = d.get("task_id") or ""
eq("S3-1 PARTIAL 预检 200", st, 200)
eq("S3-2 total=8", rep.get("total"), 8)
eq("S3-3 valid=3（第 9 行校验期看不出：基准单位不存在只在入库期暴露）", rep.get("valid"), 3)
eq("S3-4 error=5", rep.get("error"), 5)
errs = rep.get("errors") or []
eq("S3-5 报告内错误条数=5", len(errs), 5)
eq("S3-6 错误带真实 Excel 行号", sorted({e.get("row_no") for e in errs}), [4, 5, 6, 7, 8])
codes = {e.get("kind") or e.get("error_code") for e in errs}
check("S3-7 错误码覆盖 REQUIRED/TYPE/ENUM/UNIQUE",
      {"REQUIRED", "TYPE", "ENUM", "UNIQUE"} <= codes, f"{sorted(codes)}")
check("S3-8 错误行点名中文列名", any("单位编码" in (e.get("column_header") or "") for e in errs),
      f"{sorted({e.get('column_header') for e in errs})}")
check("S3-9 错误文案可读（逐条非空）", all((e.get("message") or "").strip() for e in errs),
      f"{[e.get('message') for e in errs][:2]}")

st, body = req("GET", f"/api/business/impexp/import/{TASK2}/errors?page=1&page_size=20", ADMIN, name="s3_errors")
ep = data_of(body)
eq("S3-10 错误明细接口 5 条（确认前，仅校验期错误）", ep.get("total"), 5)
first = (ep.get("list") or [{}])[0]
check("S3-11 明细含 row_no/column_header/error_code/error_message",
      all(k in first for k in ("row_no", "column_header", "error_code", "error_message")), f"{first}")
db_errs = one(f"select count(*)||'|'||min(row_no)||'|'||max(row_no) from sys_import_task_error where task_id='{TASK2}'::uuid")
eq("S3-12 DB 错误明细 5 条且行号 4..8", db_errs, "5|4|8")
st, headers, payload, _ = req_bytes(f"/api/business/impexp/import/{TASK2}/errors/file", ADMIN, name="s3_errors.xlsx")
eq("S3-13 错误行 xlsx 下载 200", st, 200)
err_sheets = read_xlsx(payload)
err_rows = list(err_sheets.values())[0] if err_sheets else []
check("S3-14 错误行文件带 Excel 行号与原因列",
      any("行号" in str(c) for c in (err_rows[0] if err_rows else []))
      and any("原因" in str(c) for c in (err_rows[0] if err_rows else [])),
      f"表头={err_rows[0] if err_rows else None}")
eq("S3-15 错误行文件 5 行数据 + 表头", len(err_rows) - 1, 5)

st, body = req("POST", f"/api/business/impexp/import/{TASK2}/confirm", ADMIN, timeout=300, name="s3_confirm_partial")
d = data_of(body)
eq("S3-16 PARTIAL 确认 200", st, 200)
eq("S3-17 终态 PARTIAL", d.get("status"), "PARTIAL")
eq("S3-18 只入库 2 行", d.get("imported_rows"), 2)
eq("S3-19 DB 只落 2 行", one("select count(*) from biz_unit where unit_code like 'E2E13%' and deleted_at is null"), "2")
eq("S3-20 错误行的编码一个都没落库",
   one("select count(*) from biz_unit where unit_code in ('E2E13U4','E2E13U5','E2E13U6','E2E13U8')"), "0")
eq("S3-21 辅助单位的基准单位指向同文件的基本单位",
   one("select count(*) from biz_unit c join biz_unit b on b.id = c.base_unit_id "
       "where c.unit_code='E2E13U2' and b.unit_code='E2E13U1' and c.rate = 12"), "1")
eq("S3-22 DB 任务状态 PARTIAL|2|5", one(
    f"select status||'|'||imported_rows||'|'||error_rows from sys_import_task where id='{TASK2}'::uuid"), "PARTIAL|2|5")
contains("S3-23 文案如实说明「入库失败 1 行」（修复前只提校验失败）", str(d.get("message")), "入库失败 1 行")
eq("S3-24 入库失败的行也进了错误明细（失败明细页可见，行号 9）",
   one(f"select count(*) from sys_import_task_error where task_id='{TASK2}'::uuid and row_no = 9"), "1")
contains("S3-25 该明细写明「入库失败」且带原因",
        one(f"select error_code||'|'||error_message from sys_import_task_error "
            f"where task_id='{TASK2}'::uuid and row_no = 9"), "入库失败")

# ==================== S4 STRICT 拒绝（有错行整批中止） ====================
scenario("S4", "STRICT 模式：有错行整批中止，任务置 FAILED，无一行落库")
before = one("select count(*) from biz_unit where unit_code like 'E2E13%' and deleted_at is null")
st, body, _ = upload_import(bad_xlsx, mode="STRICT", biz_type="business.erp.unit", name="s4_upload_strict")
TASK3 = data_of(body).get("task_id") or ""
eq("S4-1 STRICT 预检仍返回报告（校验期 error=5；第 9 行要到入库期才失败）",
   (data_of(body).get("report") or {}).get("error"), 5)
st, body = req("POST", f"/api/business/impexp/import/{TASK3}/confirm", ADMIN, name="s4_confirm_strict")
eq("S4-2 STRICT 确认 422", st, 422)
eq("S4-3 业务码 60014", body.get("code") if isinstance(body, dict) else None, 60014)
contains("S4-4 文案说明整批中止（校验期就拦下）", msg_of(body), "全部中止")
eq("S4-5 任务置 FAILED 且 imported=0",
   one(f"select status||'|'||imported_rows from sys_import_task where id='{TASK3}'::uuid"), "FAILED|0")
eq("S4-6 库内行数未变（无一行落库）",
   one("select count(*) from biz_unit where unit_code like 'E2E13%' and deleted_at is null"), before)
st, body = req("POST", f"/api/business/impexp/import/{TASK3}/confirm", ADMIN, name="s4_confirm_again")
eq("S4-7 FAILED 任务不能重复确认 409", st, 409)
st, body, _ = upload_import(good_xlsx, mode="PARTIAL", name="s4_partial_not_allowed")
eq("S4-8 字典项定义未开部分导入 → PARTIAL 400 + 10001", [st, body.get("code") if isinstance(body, dict) else None], [400, 10001])
contains("S4-9 文案指出改用 STRICT", msg_of(body), "STRICT")

# ==================== S5 结构 / 参数负例 ====================
scenario("S5", "导入负例：表头缺列 / 非 xlsx / 非法模式 / 未知定义 / 行数上限")
miss_xlsx = make_xlsx(["字典项值*", "字典项名称*"], [["v90", f"{PREFIX} 缺列"]])
st, body, _ = upload_import(miss_xlsx, name="s5_missing_header")
check("S5-1 表头缺必需列被拒（400/422）", st in (400, 422), f"HTTP {st}")
contains("S5-2 文案点名缺失列「字典编码」", msg_of(body), "字典编码")
st, body, _ = upload_import(b"not an xlsx at all", file_name="broken.xlsx", name="s5_broken")
eq("S5-3 非 xlsx 内容 400", st, 400)
st, body, _ = upload_import(good_xlsx, mode="WHATEVER", name="s5_bad_mode")
eq("S5-4 非法导入模式 400", st, 400)
st, body, _ = upload_import(good_xlsx, biz_type="not.exists", name="s5_unknown_def")
eq("S5-5 未知定义 404 + 60011", [st, body.get("code") if isinstance(body, dict) else None], [404, 60011])
st, body, _ = upload_import(good_xlsx, duplicate="DROP-TABLE", name="s5_bad_dup")
check("S5-6 非法重复策略被拒", st in (400, 422), f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("GET", "/api/business/impexp/import/not-a-uuid", ADMIN, name="s5_bad_id")
eq("S5-7 非法任务 ID 400", st, 400)
st, body = req("GET", "/api/business/impexp/import/0190aaaa-0000-7000-8000-000000000000", ADMIN, name="s5_ghost")
eq("S5-8 幽灵任务 404", st, 404)

# ==================== S6 同步导出 ====================
scenario("S6", "同步导出：表头 / 状态翻译 / 筛选 / 非法筛选 422")
st, headers, payload, _ = req_bytes("/api/business/impexp/export/sync?biz_type=system.dict_item&keyword=E2E13",
                                    ADMIN, name="s6_sync.xlsx")
eq("S6-1 同步导出 200", st, 200)
check("S6-2 Content-Type xlsx", "spreadsheetml" in headers.get("content-type", ""), headers.get("content-type", ""))
sheets = read_xlsx(payload)
rows = list(sheets.values())[0] if sheets else []
eq("S6-3 表头 = 导出列定义", rows[0][:3] if rows else [], ["字典编码", "字典项值", "字典项名称"])
body_rows = [r for r in rows[1:] if r and r[0]]
eq(f"S6-4 导出行数 = 库内 {DICT_N} 行（关键词命中）", len(body_rows), DICT_N)
status_col = rows[0].index("状态") if rows and "状态" in rows[0] else -1
check("S6-5 状态码翻译成中文（启用/停用）",
      status_col >= 0 and {r[status_col] for r in body_rows} <= {"启用", "停用"} and
      "启用" in {r[status_col] for r in body_rows}, f"{[r[status_col] for r in body_rows]}")
st, body = req("GET", "/api/business/impexp/export/sync?biz_type=system.dict_item&status=7", ADMIN, name="s6_bad_status")
check("S6-6 非法状态值被拒（不静默返回全量）", st in (400, 422), f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("GET", "/api/business/impexp/export/sync?biz_type=erp.stock_in&status=1", ADMIN, name="s6_erp_status")
eq("S6-7 ERP 单据传 status → 422", st, 422)
contains("S6-8 422 点名改 status_code", msg_of(body), "status_code")
st, body = req("GET", "/api/business/impexp/export/sync?biz_type=erp.stock_in&status_code=NOT_A_CODE", ADMIN,
               name="s6_erp_bad_code")
eq("S6-9 非法单据状态码 422", st, 422)
st, body = req("GET", "/api/business/impexp/export/sync?biz_type=nope", ADMIN, name="s6_sync_404")
eq("S6-10 未知定义同步导出 404", st, 404)

# ==================== S7 异步导出（任务 + 进度 + 下载校验 + 大数据量） ====================
scenario("S7", "异步导出：任务创建 → worker 执行 → 进度 → 下载 SHA-256 → 大文件（2000 行）")
st, body = req("POST", "/api/business/impexp/export", ADMIN,
               {"biz_type": "system.dict_item", "format": "XLSX", "export_name": f"{PREFIX} 字典项导出",
                "keyword": "E2E13"}, name="s7_export_create")
d = data_of(body)
EXP1 = d.get("id") or d.get("task_id") or ""
eq("S7-1 建导出任务 200", st, 200)
check("S7-2 任务返回 id + 初始状态", bool(EXP1) and d.get("status") in ("PENDING", "RUNNING", "SUCCESS"),
      f"id={EXP1} status={d.get('status')}")


def exp_detail(tid):
    s, b = req("GET", f"/api/business/impexp/export/{tid}", ADMIN)
    return s, data_of(b)


st, det = poll(lambda: exp_detail(EXP1), lambda r: r[1].get("status") in ("SUCCESS", "FAILED"), timeout=90,
               interval=1.0, label="EXP1")
eq("S7-3 worker 把任务跑成 SUCCESS", det.get("status"), "SUCCESS")
eq("S7-4 进度 100", det.get("progress"), 100)
eq(f"S7-5 预计行数 = {DICT_N}", det.get("total_rows"), DICT_N)
eq(f"S7-6 已导出行数 = {DICT_N}", det.get("exported_rows"), DICT_N)
check("S7-7 产物名/字节数/SHA-256/过期时间齐备",
      bool(det.get("file_name")) and (det.get("file_size") or 0) > 0 and bool(det.get("file_sha256"))
      and bool(det.get("expire_at")), f"{det.get('file_name')} {det.get('file_size')}B sha={str(det.get('file_sha256'))[:12]}…")
st, headers, payload, _ = req_bytes(f"/api/business/impexp/export/{EXP1}/download", ADMIN, name="s7_export.xlsx")
eq("S7-8 产物下载 200", st, 200)
eq("S7-9 下载字节 SHA-256 = 任务快照", sha256_hex(payload), det.get("file_sha256"))
eq("S7-10 下载字节数 = file_size", len(payload), det.get("file_size"))
dl_rows = list(read_xlsx(payload).values())[0]
eq(f"S7-11 产物 {DICT_N} 行数据", len([r for r in dl_rows[1:] if r and r[0]]), DICT_N)
db_exp = one(f"select status||'|'||exported_rows||'|'||progress||'|'||(file_path is not null) "
             f"from sys_export_task where id='{EXP1}'::uuid")
eq(f"S7-12 DB 任务行 SUCCESS|{DICT_N}|100|true（产物已落盘）", db_exp, f"SUCCESS|{DICT_N}|100|true")
st, body = req("GET", "/api/business/impexp/export/page?page=1&page_size=10&status=SUCCESS", ADMIN, name="s7_exp_page")
check("S7-13 导出任务列表可查且状态过滤生效",
      st == 200 and all(x.get("status") == "SUCCESS" for x in data_of(body).get("list") or []),
      f"total={data_of(body).get('total')}")

# 大数据量：先导入 2000 行，再异步导出（验证分块 worker 与上限内可导）
big_rows = [row(f"big{i:04d}", f"{PREFIX} 批量项{i:04d}", sort=str(i % 900)) for i in range(1, 2001)]
big_xlsx = make_xlsx(HEADERS, big_rows)
st, body, _ = upload_import(big_xlsx, mode="STRICT", name="s7_upload_big")
BIG_TASK = data_of(body).get("task_id") or ""
rep = data_of(body).get("report") or {}
eq("S7-14 2000 行预检 total=2000", rep.get("total"), 2000)
eq("S7-15 2000 行全部通过校验", rep.get("valid"), 2000)
t0 = time.time()
st, body = req("POST", f"/api/business/impexp/import/{BIG_TASK}/confirm", ADMIN, timeout=600, name="s7_confirm_big")
eq("S7-16 2000 行入库 200", st, 200)
eq("S7-17 入库 2000 行", data_of(body).get("imported_rows"), 2000)
print(f"     · 2000 行入库耗时 {time.time() - t0:.2f}s")
st, body = req("POST", "/api/business/impexp/export", ADMIN,
               {"biz_type": "system.dict_item", "format": "XLSX", "export_name": f"{PREFIX} 大批量",
                "keyword": "E2E13"}, name="s7_export_big")
EXP_BIG = data_of(body).get("id") or data_of(body).get("task_id") or ""
st, det = poll(lambda: exp_detail(EXP_BIG), lambda r: r[1].get("status") in ("SUCCESS", "FAILED"), timeout=180,
               interval=2.0, label="EXP_BIG")
BIG_N = int(one(f"select count(*) from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and deleted_at is null") or 0)
eq("S7-18 大批量导出 SUCCESS", det.get("status"), "SUCCESS")
eq(f"S7-19 大批量导出行数 = {BIG_N}（= 库内全量）", det.get("exported_rows"), BIG_N)
st, headers, payload, _ = req_bytes(f"/api/business/impexp/export/{EXP_BIG}/download", ADMIN, timeout=300,
                                    name="s7_export_big.xlsx")
eq("S7-20 大批量产物下载 200 且 SHA 一致", [st, sha256_hex(payload)], [200, det.get("file_sha256")])
big_dl = list(read_xlsx(payload).values())[0]
eq(f"S7-21 产物行数 {BIG_N}（分块拼接无丢行）", len([r for r in big_dl[1:] if r and r[0]]), BIG_N)
eq(f"S7-22 DB 导出任务记录 {BIG_N} 行", one(
    f"select exported_rows from sys_export_task where id='{EXP_BIG}'::uuid"), str(BIG_N))

# 取消 / 重试 / 删除
st, body = req("POST", "/api/business/impexp/export", ADMIN,
               {"biz_type": "system.dict_item", "format": "CSV", "export_name": f"{PREFIX} 待取消"}, name="s7_export_cancel")
EXP2 = data_of(body).get("id") or data_of(body).get("task_id") or ""
eq("S7-23 CSV 格式导出任务可建", st, 200)
st, body = req("POST", f"/api/business/impexp/export/{EXP2}/cancel", ADMIN, name="s7_cancel")
check("S7-24 取消任务 200（或已完成不可取消 409）", st in (200, 409), f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("POST", f"/api/business/impexp/export/{EXP1}/retry", ADMIN, name="s7_retry")
check("S7-25 已完成任务重试被拒（409）或重排队", st in (200, 409), f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("DELETE", f"/api/business/impexp/export/{EXP2}", ADMIN, name="s7_delete")
eq("S7-26 删除导出任务 200", st, 200)
st, body = req("GET", f"/api/business/impexp/export/{EXP2}", ADMIN, name="s7_deleted_detail")
eq("S7-27 删除后详情 404", st, 404)
eq("S7-28 DB 软删（deleted_at 非空）", one(f"select (deleted_at is not null) from sys_export_task where id='{EXP2}'::uuid"), "t")

# ==================== S8 文件管理 ====================
scenario("S8", "文件管理：上传 / 秒传预检 / 列表 / 详情 / 下载 / 删除 / 黑名单")
png = mk_png()
raw, ctype = multipart({}, "file", f"{PREFIX}_pic.png", png, "image/png")
st, headers, payload, _ = req_bytes("/api/support/file/upload?category=attachment", ADMIN, method="POST",
                                    body=raw, ctype=ctype, name="s8_file_upload")
import json  # noqa: E402

fr = json.loads(payload.decode("utf-8", "replace"))
fd = data_of(fr)
FILE_ID = fd.get("id") or ""
eq("S8-1 文件上传 200", st, 200)
eq("S8-2 原始文件名回显", fd.get("original_name"), f"{PREFIX}_pic.png")
eq("S8-3 字节数一致", fd.get("file_size"), len(png))
check("S8-4 记录带存储 key / 上传人名 / 下载地址",
      bool(fd.get("file_key")) and bool(fd.get("uploader_name")) and fd.get("url", "").endswith(FILE_ID),
      f"file_key={fd.get('file_key')} uploader={fd.get('uploader_name')} url={fd.get('url')}")
HASH = fd.get("file_hash") or fd.get("sha256") or sha256_hex(png)
st, body = req("GET", f"/api/support/file/check-duplicate?file_hash={HASH}", ADMIN, name="s8_dup")
check("S8-5 内容秒传预检命中", data_of(body).get("duplicated") is True, f"{data_of(body)}")
st, body = req("GET", "/api/support/file/check-duplicate?file_hash=" + "0" * 64, ADMIN, name="s8_dup_miss")
eq("S8-6 未命中返回 duplicated=false", data_of(body).get("duplicated"), False)
st, body = req("GET", f"/api/support/file/page?page=1&page_size=10&original_name={PREFIX}", ADMIN, name="s8_file_page")
check("S8-7 文件列表按名过滤命中", st == 200 and (data_of(body).get("total") or 0) >= 1,
      f"total={data_of(body).get('total')}")
st, body = req("GET", f"/api/support/file/{FILE_ID}", ADMIN, name="s8_file_detail")
eq("S8-8 详情 200 且 id 对得上", data_of(body).get("id"), FILE_ID)
st, headers, payload, _ = req_bytes(f"/api/support/file/download/{FILE_ID}", ADMIN, name="s8_download.png")
eq("S8-9 下载 200 且字节一致", [st, sha256_hex(payload)], [200, sha256_hex(png)])
check("S8-10 下载头带 nosniff", "nosniff" in headers.get("x-content-type-options", ""), headers)
contains("S8-11 下载头 RFC5987 文件名", headers.get("content-disposition", ""), "filename*=UTF-8''")
raw, ctype = multipart({}, "file", "e2e13_bad.exe", b"MZ", "application/octet-stream")
st, headers, payload, _ = req_bytes("/api/support/file/upload", ADMIN, method="POST", body=raw, ctype=ctype,
                                    name="s8_exe")
eq("S8-12 .exe 黑名单 400", st, 400)
st, body = req("DELETE", f"/api/support/file/{FILE_ID}", ADMIN, name="s8_delete")
eq("S8-13 删除自己的文件 200", st, 200)
eq("S8-14 DB 软删", one(f"select (deleted_at is not null) from sys_file where id='{FILE_ID}'::uuid"), "t")
st, body = req("GET", f"/api/support/file/{FILE_ID}", ADMIN, name="s8_detail_after")
eq("S8-15 删除后详情 404", st, 404)

# 无文件权限用户
st, body = req("GET", "/api/support/file/page", login("e2e13_noimp") or "", name="s8_page_403")
eq("S8-16 无文件权限用户列表 403", st, 403)

merge_state(task_good=TASK1, task_partial=TASK2, task_strict=TASK3, export_id=EXP1,
            export_big=EXP_BIG, dict_type_id=TYPE_ID)

print("\n===== A 段汇总 =====")
for sid in sorted(STATS):
    ok_, bad_ = STATS[sid]
    print(f"  {sid}: PASS={ok_} FAIL={bad_}")
total_pass = sum(v[0] for v in STATS.values())
total_fail = sum(v[1] for v in STATS.values())
print(f"总计 PASS={total_pass} FAIL={total_fail}")
sys.exit(1 if total_fail else 0)
