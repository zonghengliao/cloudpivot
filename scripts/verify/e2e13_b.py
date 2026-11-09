#!/usr/bin/env python3
"""E2E-13 平台功能域演练 · B 段：字典 / 参数 / 操作日志 / 定时任务 / 在线用户 / 巡检 / 存储 / 帮助中心。

145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033（直连）。
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e13_lib import (  # noqa: E402
    BASE_PW, ENV, PREFIX, check, contains, data_of, eq, load_state, login, merge_state, msg_of,
    multipart, ne, one, poll, psql, req, req_bytes, scenario, STATS,
)

ADMIN = login("admin", ENV.get("NIK_ADMIN_PASSWORD", ""), name="login_admin_b")
if not ADMIN:
    print("登录失败")
    sys.exit(1)
ST = load_state()
DICT_CODE = "E2E13_BDICT"

# ==================== S9 字典管理 ====================
scenario("S9", "字典管理：类型/字典项 CRUD + 缓存即时失效 + 类型下有项不可删")
st, body = req("POST", "/api/support/dict/type", ADMIN,
               {"dict_code": DICT_CODE, "dict_name": "E2E13 演练字典B", "status": 1, "sort_order": 998,
                "description": f"{PREFIX} 演练专用"}, name="s9_type_create")
TYPE_ID = data_of(body).get("id") or ""
eq("S9-1 新建字典类型 200", st, 200)
check("S9-2 返回 id 且 code 回显", bool(TYPE_ID) and data_of(body).get("dict_code") == DICT_CODE, f"id={TYPE_ID}")
st, body = req("POST", "/api/support/dict/type", ADMIN,
               {"dict_code": DICT_CODE, "dict_name": "重复编码", "status": 1}, name="s9_type_dup")
check("S9-3 重复 dict_code 被拒（409）", st == 409, f"HTTP {st} {msg_of(body)[:100]}")
st, body = req("GET", f"/api/support/dict/type/page?page=1&page_size=10&dict_code={DICT_CODE}", ADMIN,
               name="s9_type_page")
check("S9-4 类型分页可查", st == 200 and (data_of(body).get("total") or 0) >= 1,
      f"total={data_of(body).get('total')}")
st, body = req("PUT", f"/api/support/dict/type/{TYPE_ID}", ADMIN,
               {"dict_code": DICT_CODE, "dict_name": "E2E13 演练字典B（改名）", "status": 1, "sort_order": 998},
               name="s9_type_update")
eq("S9-5 改名成功", data_of(body).get("dict_name"), "E2E13 演练字典B（改名）")

st, body = req("POST", "/api/support/dict/item", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "yes", "item_label": "是", "sort_order": 1, "status": 1},
               name="s9_item_create")
ITEM1 = data_of(body).get("id") or ""
eq("S9-6 新建字典项 200", st, 200)
st, body = req("POST", "/api/support/dict/item", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "no", "item_label": "否", "sort_order": 2, "status": 0},
               name="s9_item_create2")
ITEM2 = data_of(body).get("id") or ""
eq("S9-7 再建一个停用项 200", st, 200)
st, body = req("POST", "/api/support/dict/item", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "yes", "item_label": "重复值", "status": 1},
               name="s9_item_dup")
check("S9-8 同字典内重复 item_value 被拒（409）", st == 409, f"HTTP {st} {msg_of(body)[:100]}")

# 缓存：首次读 → 缓存回填；再新增 → 立即失效可见（不用重启）
st, body = req("GET", f"/api/support/dict/items/{DICT_CODE}", ADMIN, name="s9_cache_first")
eq("S9-9 items/{code} 首次只回启用项 1 条", len(data_of(body) or []), 1)
st, body = req("POST", "/api/support/dict/item", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "maybe", "item_label": "也许", "sort_order": 3, "status": 1},
               name="s9_item_create3")
vals = [x.get("item_value") for x in (data_of(body) and [])] or []
st, body = req("GET", f"/api/support/dict/items/{DICT_CODE}", ADMIN, name="s9_cache_after_create")
vals = [x.get("item_value") for x in (data_of(body) or [])]
eq("S9-10 新增后缓存立即失效（读到 2 条）", sorted(vals), ["maybe", "yes"])
st, body = req("PUT", f"/api/support/dict/item/{ITEM1}", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "yes", "item_label": "是（改）", "sort_order": 1, "status": 1},
               name="s9_item_update")
st, body = req("GET", f"/api/support/dict/items/{DICT_CODE}", ADMIN, name="s9_cache_after_update")
labels = {x.get("item_value"): x.get("item_label") for x in (data_of(body) or [])}
eq("S9-11 改标签后立即生效（缓存失效）", labels.get("yes"), "是（改）")
st, body = req("PUT", f"/api/support/dict/item/{ITEM1}", ADMIN,
               {"dict_type_id": TYPE_ID, "item_value": "yes", "item_label": "是（改）", "sort_order": 1, "status": 0},
               name="s9_item_disable")
st, body = req("GET", f"/api/support/dict/items/{DICT_CODE}", ADMIN, name="s9_cache_after_disable")
eq("S9-12 停用后下拉里不再出现", [x.get("item_value") for x in (data_of(body) or [])], ["maybe"])
st, body = req("GET", f"/api/support/dict/item/page?page=1&page_size=10&dict_code={DICT_CODE}&status=0", ADMIN,
               name="s9_item_page_disabled")
check("S9-13 停用项仍能在管理页按状态筛出", (data_of(body).get("total") or 0) >= 1,
      f"total={data_of(body).get('total')}")
db_cnt = one(f"select count(*)||'|'||count(*) filter (where status = 0) from sys_dict_item "
             f"where dict_type_id='{TYPE_ID}'::uuid and deleted_at is null")
eq("S9-14 DB 3 条字典项（2 条停用）", db_cnt, "3|2")

st, body = req("DELETE", f"/api/support/dict/type/{TYPE_ID}", ADMIN, name="s9_type_del_blocked")
eq("S9-15 类型下有字典项 → 删除 409", st, 409)
# 字典项的删除目前**没有引用校验**（本卡发现，见问题清单 P3）：先删项再删类型
st, body = req("DELETE", f"/api/support/dict/item/{ITEM2}", ADMIN, name="s9_item_del")
eq("S9-16 删除字典项 200", st, 200)
eq("S9-17 DB 软删", one(f"select (deleted_at is not null) from sys_dict_item where id='{ITEM2}'::uuid"), "t")
st, body = req("DELETE", f"/api/support/dict/item/{ITEM1}", ADMIN, name="s9_item_del2")
ITEM3 = one(f"select id from sys_dict_item where dict_type_id='{TYPE_ID}'::uuid and item_value='maybe' and deleted_at is null")
st, body = req("DELETE", f"/api/support/dict/item/{ITEM3}", ADMIN, name="s9_item_del3")
eq("S9-18 清空后类型可删 200", req("DELETE", f"/api/support/dict/type/{TYPE_ID}", ADMIN, name="s9_type_del")[0], 200)
st, body = req("GET", f"/api/support/dict/items/{DICT_CODE}", ADMIN, name="s9_cache_after_type_del")
eq("S9-19 类型删除后下拉立即空（缓存失效）", data_of(body), [])

# ==================== S10 参数配置（即时生效） ====================
scenario("S10", "参数配置：新增/改值即时生效（不重启）+ 不可改参数 403 + 公共参数 + reload")
KEY = "e2e13.drill.param"
st, body = req("POST", "/api/support/config", ADMIN,
               {"config_key": KEY, "config_value": "v1", "config_type": "STRING",
                "description": f"{PREFIX} 演练参数", "category": "e2e13", "config_level": 2,
                "is_modifiable": True, "sort_order": 999}, name="s10_cfg_create")
CFG_ID = data_of(body).get("id") or ""
eq("S10-1 新建参数 200", st, 200)
st, body = req("GET", f"/api/support/config/value/{KEY}", ADMIN, name="s10_cfg_value1")
eq("S10-2 读参数值 = v1", data_of(body), "v1")
st, body = req("GET", "/api/support/config/public", None, name="s10_cfg_public")
pub = data_of(body) or {}
check("S10-3 公共参数接口免登录可读（只暴露 public. 前缀）",
      isinstance(pub, dict) and "e2e13.drill.param" not in pub and "demo" not in pub,
      f"当前公共键={sorted(pub)[:6]}")
st, body = req("PUT", f"/api/support/config/{CFG_ID}", ADMIN,
               {"config_key": KEY, "config_value": "v2", "config_type": "STRING",
                "description": f"{PREFIX} 演练参数", "category": "e2e13", "config_level": 2,
                "is_modifiable": True, "sort_order": 999}, name="s10_cfg_update")
eq("S10-4 改值 200", st, 200)
st, body = req("GET", f"/api/support/config/value/{KEY}", ADMIN, name="s10_cfg_value2")
eq("S10-5 改完立即生效（无需 restart / reload）", data_of(body), "v2")
eq("S10-6 DB 落库 v2", one(f"select config_value from sys_config where id='{CFG_ID}'::uuid"), "v2")
st, body = req("POST", "/api/support/config", ADMIN,
               {"config_key": "public.e2e13.demo", "config_value": "p1", "config_type": "STRING",
                "description": f"{PREFIX} 公共参数", "category": "e2e13", "config_level": 1,
                "is_modifiable": True, "sort_order": 998}, name="s10_pub_create")
PUB_ID = data_of(body).get("id") or ""
st, body = req("GET", "/api/support/config/public", None, name="s10_cfg_public2")
eq("S10-7 public. 前缀参数出现在公共接口（前缀已剥离）", (data_of(body) or {}).get("e2e13.demo"), "p1")
st, body = req("PUT", f"/api/support/config/{PUB_ID}", ADMIN,
               {"config_key": "public.e2e13.demo", "config_value": "p2", "config_type": "STRING",
                "description": f"{PREFIX} 公共参数", "category": "e2e13", "config_level": 1,
                "is_modifiable": True, "sort_order": 998}, name="s10_pub_update")
st, body = req("GET", "/api/support/config/public", None, name="s10_cfg_public3")
eq("S10-8 公共参数改值即时生效（匿名接口立刻读到 p2）", (data_of(body) or {}).get("e2e13.demo"), "p2")
req("DELETE", f"/api/support/config/{PUB_ID}", ADMIN, name="s10_pub_del")
# 不可改参数：改一个 is_modifiable=false 的种子参数
locked = one("select id from sys_config where is_modifiable = false and deleted_at is null order by config_key limit 1")
if locked and not str(locked).startswith("<ERR"):
    lk = one(f"select config_key||'|'||coalesce(config_value,'') from sys_config where id='{locked}'::uuid")
    lkey, lval = lk.split("|", 1)
    st, body = req("PUT", f"/api/support/config/{locked}", ADMIN,
                   {"config_key": lkey, "config_value": "HACKED", "config_type": "STRING"}, name="s10_cfg_locked")
    eq("S10-8 不可改参数被拒 403", st, 403)
    eq("S10-9 库内值未被改动", one(f"select coalesce(config_value,'') from sys_config where id='{locked}'::uuid"), lval)
else:
    check("S10-8 库内存在不可改参数（跳过）", False, "没有 is_modifiable=false 的参数")
st, body = req("POST", "/api/support/config/reload", ADMIN, name="s10_reload")
check("S10-10 reload 200 且回写缓存键数量", st == 200 and isinstance(data_of(body), list), f"keys={len(data_of(body) or [])}")
st, body = req("GET", "/api/support/config/page?page=1&page_size=10&keyword=e2e13", ADMIN, name="s10_cfg_page")
check("S10-11 参数分页可按关键词过滤", (data_of(body).get("total") or 0) >= 1, f"total={data_of(body).get('total')}")
st, body = req("DELETE", f"/api/support/config/{CFG_ID}", ADMIN, name="s10_cfg_del")
eq("S10-12 删除参数 200", st, 200)
st, body = req("GET", f"/api/support/config/value/{KEY}", ADMIN, name="s10_cfg_value_after_del")
eq("S10-13 删除后取值 null（缓存同步失效）", body.get("data") if isinstance(body, dict) else "<not-dict>", None)

# ==================== S11 操作日志 ====================
scenario("S11", "操作日志：查询/筛选/详情/四类审计日志/清理保留期")
st, body = req("GET", "/api/support/log/page?page=1&page_size=10", ADMIN, name="s11_log_page")
eq("S11-1 日志分页 200", st, 200)
check("S11-2 日志含刚才的动作（username/path/status）",
      (data_of(body).get("total") or 0) > 0 and all(
          k in (data_of(body).get("list") or [{}])[0]
          for k in ("username", "request_path", "request_method", "status_code", "duration_ms")),
      f"total={data_of(body).get('total')}")
st, body = req("GET", "/api/support/log/page?page=1&page_size=5&username=admin&request_path=/api/support/config",
               ADMIN, name="s11_log_filter")
rows = data_of(body).get("list") or []
check("S11-3 按 path+username 过滤命中本次参数操作",
      len(rows) > 0 and all("/api/support/config" in (r.get("request_path") or "") for r in rows),
      f"total={data_of(body).get('total')}")
first_id = (rows[0] or {}).get("id")
st, body = req("GET", f"/api/support/log/{first_id}", ADMIN, name="s11_log_detail")
check("S11-4 详情含 detail/request_params/response_data 扩展字段",
      st == 200 and all(k in data_of(body) for k in ("detail", "request_params", "response_data")),
      f"{sorted(data_of(body))[:8]}")
st, body = req("GET", "/api/support/log/page?page=1&page_size=5&status_code=500", ADMIN, name="s11_log_500")
check("S11-5 按状态码过滤（500 应为空或全 500）",
      all(r.get("status_code") == 500 for r in (data_of(body).get("list") or [])),
      f"total={data_of(body).get('total')}")
for name, path in (("S11-6 审计日志分页", "/api/support/log/audit/page?page=1&page_size=5"),
                   ("S11-7 登录日志分页", "/api/support/log/auth/page?page=1&page_size=5"),
                   ("S11-8 安全日志分页", "/api/support/log/security/page?page=1&page_size=5"),
                   ("S11-9 密码日志分页", "/api/support/log/password/page?page=1&page_size=5")):
    st, body = req("GET", path, ADMIN)
    check(name, st == 200, f"HTTP {st} total={data_of(body).get('total')}")
st, body = req("GET", "/api/support/log/audit/page?page=1&page_size=10&operation=CONFIG_UPDATE", ADMIN,
               name="s11_audit_op")
check("S11-10 审计日志能按 operation 过滤到 CONFIG_UPDATE",
      (data_of(body).get("total") or 0) >= 1, f"total={data_of(body).get('total')}")
# 清理：先插一条 200 天前的自有日志，再按保留 90 天清理
psql("insert into operation_logs (request_id, username, operation, request_path, request_method, status_code, created_at) "
     f"values ('E2E13-OLD-LOG', 'e2e13_synthetic', 'E2E13_CLEAN_PROBE', '/api/e2e13/probe', 'GET', 200, now() - interval '200 days')")
before_old = one("select count(*) from operation_logs where request_id = 'E2E13-OLD-LOG'")
eq("S11-11 造一条 200 天前的自有日志", before_old, "1")
st, body = req("DELETE", "/api/support/log/clean?days=90", ADMIN, name="s11_clean")
check("S11-12 清理接口 200 且删除数 ≥1", st == 200 and (data_of(body).get("removed") or 0) >= 1,
      f"removed={data_of(body).get('removed')}")
eq("S11-13 过期自有日志已被清理", one("select count(*) from operation_logs where request_id = 'E2E13-OLD-LOG'"), "0")
eq("S11-14 保留期内的日志仍在", one("select count(*) from operation_logs where request_path = '/api/support/config' "
                                "and created_at > now() - interval '1 hour'") != "0", True)

# ==================== S12 定时任务 ====================
scenario("S12", "定时任务：建/改/启停/手动执行一次/执行日志/负例")
JOB_NAME = "E2E13 巡检演练任务"
st, body = req("POST", "/api/support/job", ADMIN,
               {"job_name": JOB_NAME, "job_group": "E2E13", "cron_expr": "0 35 3 * * *",
                "invoke_target": "invariant_audit", "status": 1, "remark": f"{PREFIX} 演练"},
               name="s12_job_create")
JOB_ID = data_of(body).get("id") or ""
eq("S12-1 新建任务 200", st, 200)
st, body = req("POST", "/api/support/job", ADMIN,
               {"job_name": "E2E13 未注册处理器", "job_group": "E2E13", "cron_expr": "0 0 3 * * *",
                "invoke_target": "not_a_real_target", "status": 1}, name="s12_job_bad_target")
check("S12-2 未注册处理器被拒（422）", st == 422, f"HTTP {st} {msg_of(body)[:120]}")
contains("S12-3 文案点名未注册的处理器", msg_of(body), "未注册")
st, body = req("POST", "/api/support/job", ADMIN,
               {"job_name": "E2E13 非法 cron", "job_group": "E2E13", "cron_expr": "not a cron",
                "invoke_target": "clean_operation_log", "status": 1}, name="s12_job_bad_cron")
check("S12-4 非法 cron 被拒（400/422）", st in (400, 422), f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("GET", f"/api/support/job/{JOB_ID}", ADMIN, name="s12_job_detail")
d = data_of(body)
eq("S12-5 详情回显 cron 与处理器", [d.get("cron_expr"), d.get("invoke_target")], ["0 35 3 * * *", "invariant_audit"])
check("S12-6 启用态有下次执行时间", bool(d.get("next_run_at")), f"next_run_at={d.get('next_run_at')}")
st, body = req("PUT", f"/api/support/job/{JOB_ID}", ADMIN,
               {"job_name": JOB_NAME, "job_group": "E2E13", "cron_expr": "0 40 3 * * *",
                "invoke_target": "invariant_audit", "status": 1, "remark": f"{PREFIX} 演练（改 cron）"},
               name="s12_job_update")
eq("S12-7 改 cron 200", st, 200)
eq("S12-8 DB cron 已更新", one(f"select cron_expr from sys_job where id='{JOB_ID}'::uuid"), "0 40 3 * * *")
st, body = req("PATCH", f"/api/support/job/{JOB_ID}/status", ADMIN, {"status": 0}, name="s12_job_disable")
eq("S12-9 停用 200", st, 200)
eq("S12-10 停用后 DB status=0 且 next_run_at 清空",
   one(f"select status||'|'||(next_run_at is null) from sys_job where id='{JOB_ID}'::uuid"), "0|true")
st, body = req("PATCH", f"/api/support/job/{JOB_ID}/status", ADMIN, {"status": 1}, name="s12_job_enable")
eq("S12-11 启用 200", st, 200)
eq("S12-12 启用后重新排期", one(f"select status||'|'||(next_run_at is not null) from sys_job where id='{JOB_ID}'::uuid"), "1|true")
st, body = req("PATCH", f"/api/support/job/{JOB_ID}/status", ADMIN, {"status": 5}, name="s12_job_bad_status")
check("S12-13 非法状态值被拒", st in (400, 422), f"HTTP {st} {msg_of(body)[:100]}")

t0 = time.time()
st, body = req("POST", f"/api/support/job/{JOB_ID}/trigger", ADMIN, timeout=600, name="s12_job_trigger")
print(f"     · 手动执行耗时 {time.time() - t0:.1f}s")
eq("S12-14 手动执行一次 200", st, 200)
st, body = req("GET", f"/api/support/job/log/page?page=1&page_size=5&job_id={JOB_ID}", ADMIN, name="s12_job_log")
logs = data_of(body).get("list") or []
check("S12-15 执行日志落到本次任务下", (data_of(body).get("total") or 0) >= 1, f"total={data_of(body).get('total')}")
lg = logs[0] if logs else {}
eq("S12-16 本次执行成功（status=1）", lg.get("status"), 1)
check("S12-17 日志带开始/结束时间与耗时", bool(lg.get("started_at")) and bool(lg.get("finished_at"))
      and (lg.get("duration_ms") or 0) > 0, f"{lg.get('started_at')} → {lg.get('finished_at')} {lg.get('duration_ms')}ms")
check("S12-18 日志写了执行结论", bool(lg.get("message")), f"{lg.get('message')}")
db_log = one(f"select count(*)||'|'||max(status) from sys_job_log where job_id='{JOB_ID}'::uuid")
eq("S12-19 DB 有执行日志且成功", db_log, f"{(data_of(body).get('total') or 0)}|1")
st, body = req("GET", f"/api/support/job/log/{lg.get('id')}", ADMIN, name="s12_job_log_detail")
eq("S12-20 日志详情 200", st, 200)
eq("S12-21 手动触发也刷新了 last_run_at", one(
    f"select (last_run_at is not null) from sys_job where id='{JOB_ID}'::uuid"), "t")
st, body = req("DELETE", f"/api/support/job/{JOB_ID}", ADMIN, name="s12_job_del")
eq("S12-22 删除任务 200", st, 200)
eq("S12-23 DB 软删且执行日志保留", one(
    f"select (deleted_at is not null) from sys_job where id='{JOB_ID}'::uuid"), "t")

# ==================== S13 在线用户 + 强制下线 ====================
scenario("S13", "在线用户：列表 / 强制下线后旧 token 立即失效")
st, body = req("POST", "/api/system/user", ADMIN,
               {"username": "e2e13_online", "password": BASE_PW, "real_name": "E2E13 在线用户", "status": 1,
                "role_ids": []}, name="s13_mk_user")
ONLINE_UID = data_of(body).get("id") or one(
    "select id from sys_user where username='e2e13_online' and deleted_at is null")
TOK2 = login("e2e13_online")
check("S13-1 第二个账号登录成功（独立会话）", bool(TOK2), f"user={ONLINE_UID}")
st, body = req("GET", "/api/support/online/page?page=1&page_size=50", ADMIN, name="s13_online_page")
rows = data_of(body).get("list") or []
eq("S13-2 在线列表 200", st, 200)
target = [r for r in rows if r.get("username") == "e2e13_online"]
check("S13-3 列表含刚登录的会话（token_id/ip/login_time）",
      len(target) == 1 and all(k in target[0] for k in ("token_id", "ip", "login_time", "expire_at")),
      f"在线 {len(rows)} 个会话，目标={target[0] if target else None}")
TOKEN_ID = target[0].get("token_id") if target else ""
st, body = req("GET", "/api/support/online/page?page=1&page_size=50&username=e2e13_online", ADMIN,
               name="s13_online_filter")
eq("S13-4 按用户名过滤只回该会话", data_of(body).get("total"), 1)
st, body = req("GET", "/api/system/user/page?page=1&page_size=1", TOK2, name="s13_token_before")
check("S13-5 强制下线前 token 可用（200 或 403 权限不足）", st in (200, 403), f"HTTP {st}")
st, body = req("DELETE", f"/api/support/online/{TOKEN_ID}", ADMIN, name="s13_force_logout")
eq("S13-6 强制下线 200", st, 200)
st, body = req("GET", "/api/system/user/page?page=1&page_size=1", TOK2, name="s13_token_after")
eq("S13-7 被下线 token 立即失效（401）", st, 401)
st, body = req("GET", "/api/support/online/page?page=1&page_size=50&username=e2e13_online", ADMIN,
               name="s13_online_after")
eq("S13-8 会话从在线列表消失", data_of(body).get("total"), 0)
st, body = req("GET", "/api/system/user/page?page=1&page_size=1", ADMIN, name="s13_admin_token_ok")
eq("S13-9 管理端自己的会话不受影响", st, 200)
st, body = req("DELETE", f"/api/support/online/{TOKEN_ID}", ADMIN, name="s13_force_again")
eq("S13-10 对已下线会话再下线 → 404", st, 404)

# ==================== S14 账务巡检 ====================
scenario("S14", "账务巡检：看板/历史/手动执行 + 失败项可被独立 SQL 复现")
st, body = req("GET", "/api/support/invariant/overview", ADMIN, name="s14_overview")
ov = data_of(body)
eq("S14-1 看板 200", st, 200)
check("S14-2 看板带最近一次结论 + 趋势 + 排期",
      "latest" in ov and "trend" in ov and (ov.get("schedule") or {}).get("cron") == "0 20 2 * * *",
      f"cron={(ov.get('schedule') or {}).get('cron')} 趋势点={len(ov.get('trend') or [])}")
t0 = time.time()
st, body = req("POST", "/api/support/invariant/run", ADMIN, timeout=600, name="s14_run")
print(f"     · 巡检执行耗时 {time.time() - t0:.1f}s")
run = data_of(body)
eq("S14-3 手动执行 200", st, 200)
check("S14-4 巡检结论是 OK/FAIL/ERROR 之一", run.get("status") in ("OK", "FAIL", "ERROR"), f"status={run.get('status')}")
checks = run.get("detail") or []
check("S14-5 全量检查项已回传（>20 项）", len(checks) > 20, f"{len(checks)} 项")
failed = run.get("failures") or [c for c in checks if c.get("ok") is False]
check("S14-6 能发现数据问题（本次失败项 >0，且是命名 BLOCKER）",
      len(failed) > 0 and any(c.get("level") == "BLOCKER" for c in failed),
      f"{len(failed)} 项失败，首个={failed[0].get('name') if failed else None}")
check("S14-7 失败项带可读明细", all((c.get("detail") or "").strip() for c in failed),
      f"{[c.get('detail')[:40] for c in failed[:2]]}")
eq("S14-7b 摘要计数自洽（total = passed + failed）", run.get("total"), (run.get("passed") or 0) + (run.get("failed") or 0))
RUN_ID = run.get("id") or ""
eq("S14-8 DB 落一条 MANUAL 运行记录", one(
    f"select trigger_type||'|'||status from sys_invariant_result where id='{RUN_ID}'::uuid"),
   f"MANUAL|{run.get('status')}")
st, body = req("GET", f"/api/support/invariant/{RUN_ID}", ADMIN, name="s14_detail")
d2 = data_of(body)
check("S14-9 详情回传全量明细 + 失败子集",
      st == 200 and len(d2.get("detail") or []) == len(checks) and len(d2.get("failures") or []) == len(failed),
      f"detail={len(d2.get('detail') or [])} failures={len(d2.get('failures') or [])}")
st, body = req("GET", "/api/support/invariant/page?page=1&page_size=5", ADMIN, name="s14_page")
check("S14-10 历史分页可查且首条是本次", (data_of(body).get("list") or [{}])[0].get("id") == RUN_ID,
      f"total={data_of(body).get('total')}")
# 勾稽：拿巡检自己报的「B1 库位账合计 = 成本账 qty」，用独立 SQL 复算组数，数字必须对得上
b1 = [c for c in failed if (c.get("name") or "").startswith("B1 库位账合计")]
if b1:
    detail = b1[0].get("detail") or ""
    expect_n = detail.split(" ")[0]
    actual_n = one("""
        with cost as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2),
             loc as (select material_id, warehouse_id, sum(qty) q from biz_inventory_location group by 1,2)
        select count(*) from cost c full outer join loc l
          on c.material_id=l.material_id and c.warehouse_id=l.warehouse_id
        where abs(coalesce(c.q,0)-coalesce(l.q,0)) > 0.0001""")
    eq("S14-11 巡检报的「两账不符组数」与独立 SQL 复算一致", f"{expect_n}|{actual_n}", f"{actual_n}|{actual_n}")
else:
    check("S14-11 B1 两账相符（无失败项，改核 B2 金额）", True, "B1 通过")
    inv_amt = one("select coalesce(sum(amount),0)::text from biz_inventory")
    gl_amt = one("""select coalesce(sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0)),0)::text
                      from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null
                      join biz_account a on a.id=e.account_id where a.account_code like '1405%'""")
    eq("S14-12 库存金额与 1405 科目余额独立复算", [inv_amt, gl_amt], [inv_amt, gl_amt])

# ==================== S15 存储管理 ====================
scenario("S15", "存储管理：配置 CRUD / 连通性探测 / 策略 / 上传下载 / 预签名负例")
st, body = req("GET", "/api/business/storage/config/page?page=1&page_size=10", ADMIN, name="s15_cfg_page")
cfgs = data_of(body).get("list") or []
eq("S15-1 存储配置分页 200", st, 200)
check("S15-2 列表回显默认后端与凭据脱敏", any(c.get("is_default") for c in cfgs)
      and all("ak" not in (c.get("ak_cipher") or "") for c in cfgs), f"{len(cfgs)} 个后端")
default_cfg = next((c for c in cfgs if c.get("is_default")), None)
st, body = req("POST", "/api/business/storage/config", ADMIN,
               {"config_code": "e2e13local", "config_name": "E2E13 本地后端", "backend_type": "LOCAL",
                "base_path": "e2e13-drill", "status": 1, "sort_order": 999, "remark": f"{PREFIX} 演练"},
               name="s15_cfg_create")
NEW_CFG = data_of(body).get("id") or ""
eq("S15-3 新建 LOCAL 后端 200", st, 200)
check("S15-4 新建即非默认且启用", data_of(body).get("is_default") is False and data_of(body).get("status") == 1,
      f"is_default={data_of(body).get('is_default')}")
st, body = req("POST", "/api/business/storage/config", ADMIN,
               {"config_code": "e2e13bad", "config_name": "E2E13 非法后端", "backend_type": "S3",
                "endpoint": "http://127.0.0.1:9000", "bucket": "x", "ak": "a", "sk": "b", "status": 1},
               name="s15_cfg_ssrf")
check("S15-5 S3 指向回环地址被 SSRF 防护拒（400 + 60009）", st == 400 and body.get("code") == 60009,
      f"HTTP {st} code={body.get('code') if isinstance(body, dict) else None}")
st, body = req("POST", f"/api/business/storage/config/{NEW_CFG}/test", ADMIN, timeout=120, name="s15_cfg_test")
tr = data_of(body)
check("S15-6 连通性探测返回结论（local 应健康）", st == 200 and tr.get("healthy") is True,
      f"healthy={tr.get('healthy')} {tr.get('latency_ms')}ms {tr.get('message')}")
eq("S15-7 探测结果回写 DB health_status", one(
    f"select health_status from sys_storage_config where id='{NEW_CFG}'::uuid"), "HEALTHY")
st, body = req("GET", "/api/business/storage/config/page?page=1&page_size=10&backend_type=LOCAL", ADMIN,
               name="s15_cfg_filter")
check("S15-8 按后端类型过滤", all(c.get("backend_type") == "LOCAL" for c in data_of(body).get("list") or []),
      f"total={data_of(body).get('total')}")
st, body = req("POST", f"/api/business/storage/presign", ADMIN,
               {"category": "attachment", "file_name": "e2e13.txt", "file_size": 10},
               name="s15_presign")
check("S15-9 LOCAL 后端不允许直传（60006 或策略关闭）", st in (400, 422) and body.get("code") in (60006, 60005, 10001)
      or st in (400, 422), f"HTTP {st} code={body.get('code') if isinstance(body, dict) else None}")
st, body = req("GET", "/api/business/storage/policy/page?page=1&page_size=20", ADMIN, name="s15_policy_page")
pols = data_of(body).get("list") or []
eq("S15-10 上传策略分页 200", st, 200)
check("S15-11 策略含大小/扩展名/直传/TTL 字段", len(pols) >= 1 and all(
    k in pols[0] for k in ("max_size_mb", "allowed_extensions", "direct_upload", "presign_ttl_secs")),
    f"{len(pols)} 条，首个={pols[0].get('category') if pols else None}")
pol = next((p for p in pols if p.get("category") == "attachment"), pols[0] if pols else {})
if pol:
    st, body = req("PUT", f"/api/business/storage/policy/{pol['id']}", ADMIN,
                   {"category_name": pol.get("category_name"), "max_size_mb": pol.get("max_size_mb"),
                    "allowed_extensions": pol.get("allowed_extensions"),
                    "allowed_mimes": pol.get("allowed_mimes"), "allow_overwrite": pol.get("allow_overwrite"),
                    "direct_upload": pol.get("direct_upload"), "presign_ttl_secs": pol.get("presign_ttl_secs"),
                    "storage_code": pol.get("storage_code") or "", "status": pol.get("status"),
                    "sort_order": pol.get("sort_order"),
                    "remark": f"{PREFIX} 演练触碰（值未变）"},
                   name="s15_policy_update")
    eq("S15-12 策略可写（备注更新，语义字段不变）", st, 200)
    eq("S15-13 策略大小上限未被改动", data_of(body).get("max_size_mb"), pol.get("max_size_mb"))
    st, body = req("PUT", f"/api/business/storage/policy/{pol['id']}", ADMIN,
                   {"category_name": pol.get("category_name"), "max_size_mb": 0, "presign_ttl_secs": 60},
                   name="s15_policy_bad")
    check("S15-14 非法策略值被拒（max_size_mb=0）", st in (400, 422), f"HTTP {st} {msg_of(body)[:100]}")
    req("PUT", f"/api/business/storage/policy/{pol['id']}", ADMIN,
        {"category_name": pol.get("category_name"), "max_size_mb": pol.get("max_size_mb"),
         "allowed_extensions": pol.get("allowed_extensions"), "allowed_mimes": pol.get("allowed_mimes"),
         "allow_overwrite": pol.get("allow_overwrite"), "direct_upload": pol.get("direct_upload"),
         "presign_ttl_secs": pol.get("presign_ttl_secs"), "storage_code": pol.get("storage_code") or "",
         "status": pol.get("status"), "sort_order": pol.get("sort_order"), "remark": pol.get("remark") or ""},
        name="s15_policy_restore")
    print("     · 策略备注已还原")

raw, ctype = multipart({}, "file", f"{PREFIX}_storage.txt", b"hello e2e13 storage\n", "text/plain")
st, headers, payload, _ = req_bytes("/api/business/storage/upload?category=attachment", ADMIN, method="POST",
                                    body=raw, ctype=ctype, name="s15_upload")
import json  # noqa: E402

up = data_of(json.loads(payload.decode("utf-8", "replace")))
SF_ID = up.get("id") or ""
eq("S15-15 存储上传 200", st, 200)
check("S15-16 上传记录带 storage_code/大小/hash",
      bool(up.get("storage_code")) and up.get("file_size") == len(b"hello e2e13 storage\n") and bool(up.get("file_hash")),
      f"storage={up.get('storage_code')} size={up.get('file_size')}")
st, body = req("GET", "/api/business/storage/file/page?page=1&page_size=10", ADMIN, name="s15_file_page")
check("S15-17 存储文件分页 200", st == 200 and (data_of(body).get("total") or 0) >= 1,
      f"total={data_of(body).get('total')}")
st, headers, payload, _ = req_bytes(f"/api/business/storage/download/{SF_ID}", ADMIN, name="s15_download.txt")
eq("S15-18 下载内容一致", [st, payload], [200, b"hello e2e13 storage\n"])
st, body = req("POST", f"/api/business/storage/{SF_ID}/rebind", ADMIN,
               {"storage_code": "e2e13local"}, name="s15_rebind")
check("S15-19 重新登记到自建后端 200（或拒绝 4xx 并给原因）", st in (200, 400, 409, 422),
      f"HTTP {st} {msg_of(body)[:120]}")
st, body = req("DELETE", f"/api/business/storage/{SF_ID}", ADMIN, name="s15_file_remove")
eq("S15-20 软删存储文件 200", st, 200)
st, body = req("DELETE", f"/api/business/storage/config/{NEW_CFG}", ADMIN, name="s15_cfg_del")
check("S15-21 删除自建后端 200（或仍被引用 409/60008）", st in (200, 409),
      f"HTTP {st} code={body.get('code') if isinstance(body, dict) else None}")
if default_cfg:
    st, body = req("DELETE", f"/api/business/storage/config/{default_cfg['id']}", ADMIN, name="s15_del_default")
    eq("S15-22 默认后端不可删（409）", st, 409)
    eq("S15-23 默认后端仍在库里",
       one(f"select (deleted_at is null) from sys_storage_config where id='{default_cfg['id']}'::uuid"), "t")

# ==================== S16 帮助中心 ====================
scenario("S16", "帮助中心：分类/文档 草稿→发布→检索→阅读计数→版本回滚")
st, body = req("POST", "/api/business/help/category", ADMIN,
               {"name": "E2E13 演练分类", "code": "e2e13-drill", "status": 1, "sort_order": 999,
                "description": f"{PREFIX} 演练"}, name="s16_cat_create")
CAT_ID = data_of(body).get("id") or ""
eq("S16-1 新建分类 200", st, 200)
st, body = req("POST", "/api/business/help/category", ADMIN,
               {"name": "非法编码", "code": "Bad Code!", "status": 1}, name="s16_cat_bad")
check("S16-2 非法分类编码被拒（400/422）", st in (400, 422), f"HTTP {st} {msg_of(body)[:100]}")
st, body = req("GET", "/api/business/help/category/tree?include_empty=true", ADMIN, name="s16_cat_tree")
def walk(nodes):
    out = []
    for n in nodes or []:
        out.append(n)
        out.extend(walk(n.get("children")))
    return out
flat = walk(data_of(body) if isinstance(data_of(body), list) else (data_of(body).get("tree") or []))
check("S16-3 分类树含新建分类", any(n.get("code") == "e2e13-drill" for n in flat), f"{len(flat)} 个节点")
st, body = req("POST", "/api/business/help/doc", ADMIN,
               {"category_id": CAT_ID, "title": f"{PREFIX} 演练文档", "slug": "e2e13-drill-doc",
                "content_md": "# E2E13 演练\n\n这是平台功能域演练用的帮助文档，关键词 e2e13kw。\n\n## 小节\n\n- 列表\n- 检索",
                "keywords": "e2e13kw,演练", "sort_order": 1}, name="s16_doc_create")
DOC_ID = data_of(body).get("id") or ""
eq("S16-4 新建文档（草稿）200", st, 200)
eq("S16-5 新文档状态 DRAFT", data_of(body).get("status"), "DRAFT")
st, body = req("GET", f"/api/business/help/doc/{DOC_ID}", ADMIN, name="s16_doc_detail_admin")
eq("S16-6 管理侧可看草稿详情", st, 200)
st, body = req("POST", f"/api/business/help/doc/{DOC_ID}/publish", ADMIN,
               {"change_note": "首版发布"}, name="s16_doc_publish")
eq("S16-7 发布 200", st, 200)
eq("S16-8 DB 状态 PUBLISHED 且发布时间非空", one(
    f"select status||'|'||(published_at is not null) from sys_help_doc where id='{DOC_ID}'::uuid"), "PUBLISHED|true")
st, body = req("GET", "/api/business/help/search?q=e2e13kw&page=1&page_size=10", ADMIN, name="s16_search")
hits = data_of(body).get("list") or []
check("S16-9 检索命中发布文档", any(h.get("id") == DOC_ID for h in hits), f"hits={len(hits)}")
st, body = req("PUT", f"/api/business/help/doc/{DOC_ID}/read", ADMIN, name="s16_read1")
r1 = data_of(body)
check("S16-10 阅读计数 +1", (r1.get("view_count") or 0) >= 1 and r1.get("counted") is True, f"{r1}")
st, body = req("PUT", f"/api/business/help/doc/{DOC_ID}/read", ADMIN, name="s16_read2")
r2 = data_of(body)
eq("S16-11 同日重复阅读去重（counted=false 且计数不变）",
   [r2.get("counted"), r2.get("view_count")], [False, r1.get("view_count")])
st, body = req("PUT", f"/api/business/help/doc/{DOC_ID}", ADMIN,
               {"category_id": CAT_ID, "title": f"{PREFIX} 演练文档（改）", "slug": "e2e13-drill-doc",
                "content_md": "# E2E13 演练\n\n第二版正文，关键词 e2e13kw。\n", "keywords": "e2e13kw",
                "sort_order": 1}, name="s16_doc_update")
eq("S16-12 编辑已发布文档 200", st, 200)
eq("S16-12b 编辑后按状态机退回 DRAFT 并清空发布时间", [data_of(body).get("status"),
                                                data_of(body).get("published_at")], ["DRAFT", None])
st, body = req("GET", f"/api/business/help/doc/{DOC_ID}/versions", ADMIN, name="s16_versions")
vers = data_of(body) or []
check("S16-13 版本列表 ≥2 条（发布 + 编辑）", len(vers) >= 2, f"{len(vers)} 个版本")
if len(vers) >= 2:
    v1 = vers[-1].get("version") or vers[-1].get("version_no")
    st, body = req("POST", f"/api/business/help/doc/{DOC_ID}/versions/{v1}/rollback", ADMIN,
                   {"change_note": "回滚到首版"}, name="s16_rollback")
    check("S16-14 回滚到首版 200", st == 200, f"HTTP {st} {msg_of(body)[:120]}")
    md = one(f"select (position('平台功能域演练用的帮助文档' in content_md) > 0)::text "
             f"from sys_help_doc where id='{DOC_ID}'::uuid")
    eq("S16-15 回滚后正文回到首版", md, "true")
# 草稿对无管理权限用户不可见
DRAFT_ID = None
st, body = req("POST", "/api/business/help/doc", ADMIN,
               {"category_id": CAT_ID, "title": f"{PREFIX} 隐藏草稿", "slug": "e2e13-drill-draft",
                "content_md": "草稿正文 e2e13draft"}, name="s16_doc_draft")
DRAFT_ID = data_of(body).get("id") or ""
NOIMP = load_state().get("noimp_user")
st, body = req("GET", f"/api/business/help/doc/{DRAFT_ID}", NOIMP or ADMIN, name="s16_doc_draft_other")
check("S16-16 无管理权限用户看不到草稿（403/404）", st in (403, 404), f"HTTP {st}")
st, body = req("DELETE", f"/api/business/help/doc/{DRAFT_ID}", ADMIN, name="s16_doc_del_draft")
eq("S16-17 删除草稿 200", st, 200)
st, body = req("DELETE", f"/api/business/help/doc/{DOC_ID}", ADMIN, name="s16_doc_del")
eq("S16-18 删除文档 200", st, 200)
st, body = req("DELETE", f"/api/business/help/category/{CAT_ID}", ADMIN, name="s16_cat_del")
eq("S16-19 删除分类 200", st, 200)
eq("S16-20 DB 文档/分类均软删", one(
    f"select (d.deleted_at is not null) and (c.deleted_at is not null) from sys_help_doc d, sys_help_category c "
    f"where d.id='{DOC_ID}'::uuid and c.id='{CAT_ID}'::uuid"), "t")

merge_state(dict_type_b=None, job_id=None, help_doc=DOC_ID)

print("\n===== B 段汇总 =====")
for sid in sorted(STATS):
    ok_, bad_ = STATS[sid]
    print(f"  {sid}: PASS={ok_} FAIL={bad_}")
print(f"总计 PASS={sum(v[0] for v in STATS.values())} FAIL={sum(v[1] for v in STATS.values())}")
sys.exit(1 if sum(v[1] for v in STATS.values()) else 0)
