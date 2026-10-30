#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""A/B/C/D 全模块只读冒烟（35 项）。

只发 GET，不写库、不需要清理 —— 用来快速判断「服务活着、接口在线、字段没变形」。
"""

from common import changelog_count


def _get_ok(ctx, label, path):
    st, resp = ctx.get(path)
    code = resp.get("code") if isinstance(resp, dict) else None
    ctx.chk(label, st == 200 and code == 0, "http=%s code=%s msg=%s" % (st, code, str(resp.get("msg"))[:40] if isinstance(resp, dict) else ""))
    return resp


def _shape(ctx, label, resp, keys):
    data = resp.get("data") if isinstance(resp, dict) else None
    if isinstance(data, dict):
        missing = [k for k in keys if k not in data]
        ctx.chk(label + " 分页字段完整", not missing, "缺: %s" % missing)
    elif isinstance(data, list):
        ctx.chk(label + " 返回数组", True)
    else:
        ctx.chk(label + " 有数据", data is not None, repr(data)[:40])


def batch_a(ctx):
    print("\n[A] 存储 / 导入导出 / 通知")
    resp = _get_ok(ctx, "A2 存储配置分页", "/api/business/storage/config/page?page=1&page_size=5")
    _shape(ctx, "存储配置", resp, ("list", "total", "page", "page_size"))
    _get_ok(ctx, "A3 导入导出定义列表", "/api/business/impexp/def/list")
    resp = _get_ok(ctx, "A3 导入任务分页", "/api/business/impexp/import/page?page=1&page_size=5")
    _shape(ctx, "导入任务", resp, ("list", "total", "page", "page_size"))
    _get_ok(ctx, "A5 我的通知", "/api/business/notice/mine?page=1&page_size=5")
    resp = _get_ok(ctx, "A5 通知分页", "/api/business/notice/page?page=1&page_size=5")
    _shape(ctx, "通知", resp, ("list", "total", "page", "page_size"))
    st, resp = ctx.get("/api/business/notice/unread-count")
    ctx.chk("A5 未读数接口", st == 200 and resp.get("code") == 0,
            "http=%s code=%s" % (st, resp.get("code")))
    _get_ok(ctx, "A2 存储策略分页", "/api/business/storage/policy/page?page=1&page_size=5")
    _get_ok(ctx, "A3 导出任务分页", "/api/business/impexp/export/page?page=1&page_size=5")


def batch_b(ctx):
    print("\n[B] 数据权限 / 脱敏 / 互斥")
    resp = _get_ok(ctx, "B2 未覆盖表自检", "/api/business/permission/scope/uncovered-tables")
    data = resp.get("data")
    if isinstance(data, dict):
        uncovered = data.get("uncovered") or data.get("tables") or []
        print("     未覆盖表数:", len(uncovered) if isinstance(uncovered, list) else uncovered)
    _get_ok(ctx, "B2 权限自检", "/api/business/permission/scope/self-check")
    _get_ok(ctx, "B2 部门树", "/api/business/permission/scope/dept-tree")
    # 角色 id 优先从库取：多实例共用同一个 Redis 时，/role/list 可能命中别的实例的缓存
    db_rid = ctx.q("select id::text from sys_role where deleted_at is null order by role_code limit 1")
    st, resp = ctx.get("/api/system/role/list")
    api_rid = None
    if st == 200 and resp.get("code") == 0:
        roles = resp.get("data") or []
        if isinstance(roles, dict):
            roles = roles.get("list") or []
        api_rid = roles[0].get("id") if roles else None
        ctx.chk("B2 角色列表可读", bool(roles), len(roles or []))
    if not db_rid:
        ctx.skip("库里无角色可测 scope")
        return
    _get_ok(ctx, "B2 角色数据范围", "/api/business/permission/scope/role/%s" % db_rid)
    if api_rid and api_rid != db_rid:
        print("     \u26a0 /role/list 返回的 id 与库不一致（多实例共用 Redis 缓存）：%s" % str(api_rid)[:8])
    st, resp = ctx.get("/api/system/field-mask/page?page=1&page_size=5")
    ctx.chk("B6 脱敏规则列表", st == 200 and resp.get("code") == 0, "http=%s" % st)
    st, resp = ctx.get("/api/business/mutex/page?page=1&page_size=5")
    ctx.chk("B7 角色互斥分页", st == 200 and resp.get("code") == 0, "http=%s code=%s" % (st, resp.get("code")))
    st, resp = ctx.get("/api/business/mutex/conflict")
    ctx.chk("B7 互斥冲突检查（缺参 400 属正常）", st in (200, 400), "http=%s" % st)


def batch_c(ctx):
    print("\n[C] 公告 / 帮助 / 变更日志")
    resp = _get_ok(ctx, "C2 可见公告分页", "/api/business/announcement/visible/page?page=1&page_size=5")
    _shape(ctx, "可见公告", resp, ("list", "total", "page", "page_size"))
    _get_ok(ctx, "C2 公告未读数", "/api/business/announcement/unread-count")
    _get_ok(ctx, "C4 帮助分类树", "/api/business/help/category/tree")
    st, resp = ctx.get("/api/business/help/doc/page?page=1&page_size=5")
    ctx.chk("C4 帮助文档分页", st == 200 and resp.get("code") == 0, "http=%s code=%s" % (st, resp.get("code")))
    resp = _get_ok(ctx, "C6 变更日志分页", "/api/business/changelog/page?page=1&page_size=5")
    _shape(ctx, "变更日志", resp, ("list", "total", "page", "page_size"))
    # timeline 的 query 参数叫 target_id（库列叫 resource_id，别混）
    _get_ok(ctx, "C6 变更日志时间线",
            "/api/business/changelog/timeline?table=sys_user&target_id=00000000-0000-0000-0000-000000000001&limit=5")
    _get_ok(ctx, "C6 日志表注册表", "/api/business/changelog/table-registry")


def batch_d(ctx):
    print("\n[D] 审批流")
    resp = _get_ok(ctx, "D1 流程定义分页", "/api/business/flow/def/page?page=1&page_size=20")
    items = (resp.get("data") or {}).get("list") or [] if isinstance(resp.get("data"), dict) else []
    codes = [x.get("flow_code") for x in items]
    ctx.chk("D1 有种子流程 task_accept", "task_accept" in codes, codes[:5])
    resp = _get_ok(ctx, "D2 我的待办", "/api/business/flow/task/todo?page=1&page_size=5")
    _shape(ctx, "待办", resp, ("list", "total", "page", "page_size"))
    _get_ok(ctx, "D2 我的已办", "/api/business/flow/task/done?page=1&page_size=5")
    _get_ok(ctx, "D2 抄送我的", "/api/business/flow/task/cc?page=1&page_size=5")
    _get_ok(ctx, "D2 我发起的", "/api/business/flow/instance/my?page=1&page_size=5")
    _get_ok(ctx, "D2 实例分页", "/api/business/flow/instance/page?page=1&page_size=5")
    _get_ok(ctx, "D4b 统计总览", "/api/business/flow/stats/overview")
    _get_ok(ctx, "D4b 流程效率", "/api/business/flow/stats/efficiency")
    _get_ok(ctx, "D4b 卡点分析", "/api/business/flow/stats/node-bottleneck")


BATCHES = {"a": batch_a, "b": batch_b, "c": batch_c, "d": batch_d}


def run(ctx, batches=None):
    """跑只读冒烟；batches 为空 = 全跑。"""
    print("=" * 66)
    print("只读冒烟（BASE_URL=%s  user=%s）" % (ctx.base_url, ctx.user))
    print("=" * 66)
    for key in (batches or list(BATCHES)):
        fn = BATCHES.get(key)
        if not fn:
            print("  \u26a0 未知批次：%s" % key)
            continue
        try:
            fn(ctx)
        except Exception as exc:  # noqa: BLE001
            ctx.chk("批次 %s 执行异常" % key, False, str(exc)[:120])
    return ctx
