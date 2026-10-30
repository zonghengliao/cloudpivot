#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""B 批次写操作验证：数据权限改 scope / 列级脱敏规则 / 角色互斥。

固化契约：
  - 脱敏规则：`column_name`（不是 field_name）、`mask_type` 是 **int 1..7**
    （不是 `"EMAIL"` 字符串）、`reason` **必填**、角色是单个 `role_id`
  - 角色互斥：`role_a_id` / `role_b_id` / `reason`（reason 必填）
  - 角色不足时由 `ensure_roles()` 现建临时角色，跑完自动删
"""

import json

from common import STAMP, ensure_roles

# mask_type 落库值（masker.rs 的 MaskType 顺序：1..7）
MASK_EMAIL = 3
MASK_CODES = {1: "mobile", 2: "id_card", 3: "email", 4: "name", 5: "bank_card",
              6: "amount", 7: "regex"}


def b_scope(ctx, roles):
    """B2 数据权限：读 scope → 改 → 库值校验 → 改回 → 非法值被拒。"""
    print("\n【B2】数据权限：改角色 scope → 校验库值")
    rid = roles[0][0]
    before = ctx.q("select data_scope from sys_role where id='%s'" % rid)
    print("     角色 %s 改前 data_scope=%s" % (roles[0][1], before))
    st, resp = ctx.get("/api/business/permission/scope/role/%s" % rid)
    ctx.ok("读角色 scope", resp)
    print("     接口返回:", json.dumps(ctx.data(resp), ensure_ascii=False)[:140])
    new_scope = 3 if str(before) != "3" else 2
    st, resp = ctx.put("/api/business/permission/scope/role/%s" % rid,
                       {"data_scope": new_scope, "dept_ids": []})
    ctx.ok("改角色 scope → %d" % new_scope, resp)
    after = ctx.q("select data_scope from sys_role where id='%s'" % rid)
    ctx.chk("库值已变", str(after) == str(new_scope), "after=%s expect=%s" % (after, new_scope))
    st, resp = ctx.put("/api/business/permission/scope/role/%s" % rid,
                       {"data_scope": int(before) if before else 1, "dept_ids": []})
    ctx.ok("改回原值", resp)
    back = ctx.q("select data_scope from sys_role where id='%s'" % rid)
    ctx.chk("库值已还原", str(back) == str(before), "back=%s before=%s" % (back, before))
    st, resp = ctx.put("/api/business/permission/scope/role/%s" % rid,
                       {"data_scope": 99, "dept_ids": []})
    ctx.chk("非法 scope=99 被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))


def b_mask(ctx, roles=None):
    """B6 列级脱敏：建规则（column_name + int mask_type + reason）→ 校验 → 删。"""
    print("\n【B6】列级脱敏：建规则 → 校验 → 删")
    st, resp = ctx.get("/api/system/field-mask/page?page=1&page_size=10")
    ctx.ok("脱敏规则分页", resp)
    before_n = (ctx.data(resp) or {}).get("total") if isinstance(ctx.data(resp), dict) else 0
    print("     现有规则数:", before_n)
    # 迁移里已种了 sys_user 的 email/mobile/real_name 三条 GLOBAL 规则（唯一约束含 role_id，
    # NULLS NOT DISTINCT）；所以优先挂到角色作用域，避开全局重复，同时顺带验证 role_id 字段。
    role_id = roles[0][0] if roles else None
    scope_sql = "role_id is null" if role_id is None else "role_id='%s'" % role_id
    candidates = [("sys_user", "email", MASK_EMAIL), ("sys_user", "username", 4),
                  ("sys_user", "real_name", 4), ("sys_user", "mobile", 1)]
    picked = None
    for table, column, mask_type in candidates:
        exists = ctx.q(
            "select count(*) from sys_field_mask where deleted_at is null and table_name='%s' "
            "and column_name='%s' and %s" % (table, column, scope_sql)
        )
        if exists == "0":
            picked = (table, column, mask_type)
            break
    if not picked:
        ctx.skip("候选列在该作用域都已有规则")
        return
    table, column, mask_type = picked
    print("     目标列: %s.%s mask_type=%d 作用域=%s"
          % (table, column, mask_type, "GLOBAL" if role_id is None else "ROLE"))
    body = {
        "table_name": table,
        "column_name": column,           # 不是 field_name
        "mask_type": mask_type,          # int，不是 "EMAIL"
        "reason": "verify 脚本：列级脱敏验证",   # 必填
    }
    if role_id:
        body["role_id"] = role_id
    st, resp = ctx.post("/api/system/field-mask", body)
    ctx.ok("建脱敏规则（column_name/mask_type=int/reason）", resp)
    data = ctx.data(resp) or {}
    mid = data.get("id") if isinstance(data, dict) else None
    if mid:
        ctx.on_cleanup("删脱敏规则 %s" % mid,
                       lambda mask_id=mid: ctx.delete("/api/system/field-mask/%s" % mask_id))
    ctx.chk("响应回 mask_type_code", (data.get("mask_type_code") if isinstance(data, dict) else "") == MASK_CODES.get(mask_type),
            data if not isinstance(data, dict) else data.get("mask_type_code"))
    ctx.chk("响应带 reason", bool((data.get("reason") if isinstance(data, dict) else "")), data if not isinstance(data, dict) else data.get("reason"))
    after_n = ctx.q("select count(*) from sys_field_mask where deleted_at is null")
    ctx.chk("规则数 +1", bool(after_n) and int(after_n) > int(before_n or 0),
            "before=%s after=%s" % (before_n, after_n))
    if mid:
        st, resp = ctx.get("/api/system/field-mask/page?table_name=%s&column_name=%s" % (table, column))
        items = (ctx.data(resp) or {}).get("list") if isinstance(ctx.data(resp), dict) else []
        ctx.chk("按表+列可查到该规则", any(x.get("id") == mid for x in (items or [])), len(items or []))
        st, resp = ctx.delete("/api/system/field-mask/%s" % mid)
        ctx.ok("删脱敏规则", resp)
        row = ctx.q("select count(*) from sys_field_mask where id='%s' and deleted_at is null" % mid)
        ctx.chk("删除后不可见", row == "0", row)


def b_mutex(ctx, roles):
    """B7 角色互斥：建规则（reason 必填）→ 冲突检测 → 反向重复被拒 → 删。"""
    print("\n【B7】角色互斥：建规则 → 冲突检测 → 删")
    st, resp = ctx.get("/api/business/mutex/page?page=1&page_size=10")
    ctx.ok("互斥规则分页", resp)
    before = (ctx.data(resp) or {}).get("total") if isinstance(ctx.data(resp), dict) else 0
    print("     现有互斥规则数:", before)
    if len(roles) < 2:
        ctx.skip("角色不足 2 个，互斥无法验证")
        return
    r1, r2 = roles[0][0], roles[1][0]
    # 建两个临时角色（避免现成角色对已有用户同时持有 -> 61010）
    import time as _t
    _s = _t.strftime("%H%M%S")
    tmp_roles = []
    for _suffix in ("a", "b"):
        st, rr = ctx.post("/api/system/role", {
            "role_code": "vrf_mx_%s_%s" % (_suffix, _s),
            "role_name": "互斥临时角色" + _suffix,
            "status": 1, "sort_order": 999,
        })
        _d = ctx.data(rr)
        _rid = _d.get("id") if isinstance(_d, dict) else None
        if _rid:
            tmp_roles.append(_rid)
            ctx.on_cleanup("删临时角色 " + _suffix,
                           lambda x=_rid: ctx.delete("/api/system/role/" + x))
    if len(tmp_roles) < 2:
        ctx.chk("建临时角色（互斥测试用）", False, "只建到 %d 个" % len(tmp_roles))
        return
    r1, r2 = tmp_roles[0], tmp_roles[1]
    st, resp = ctx.post("/api/business/mutex", {
        "role_a_id": r1, "role_b_id": r2, "reason": "verify 脚本：互斥验证",
    })
    ctx.ok("建互斥规则（reason 必填）", resp)
    mid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not mid:
        return
    ctx.on_cleanup("删互斥规则 %s" % mid,
                   lambda mutex_id=mid: ctx.delete("/api/business/mutex/%s" % mutex_id))
    after = ctx.q("select count(*) from sys_role_mutex where id='%s' and deleted_at is null" % mid)
    ctx.chk("规则已落库（按 id 校验）", after == "1", "row=%s" % after)
    # 冲突体检接口的参数是 user_id（不是 role_ids）
    st, resp = ctx.get("/api/business/mutex/conflict?user_id=%s" % ctx.me)
    ctx.ok("冲突体检返回（user_id 参数）", resp)
    data = ctx.data(resp)
    ctx.chk("体检结果为结构体/数组", isinstance(data, (dict, list)), str(data)[:90])
    print("     体检结果:", json.dumps(data, ensure_ascii=False)[:110])
    st, resp = ctx.get("/api/business/mutex/page?page=1&page_size=20")
    items = (ctx.data(resp) or {}).get("list") if isinstance(ctx.data(resp), dict) else []
    ctx.chk("新建规则出现在分页里", any(x.get("id") == mid for x in (items or [])), len(items or []))
    st, resp = ctx.post("/api/business/mutex", {
        "role_a_id": r2, "role_b_id": r1, "reason": "反向重复验证",
    })
    ctx.chk("反向重复被拒（对称唯一）", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    st, resp = ctx.delete("/api/business/mutex/%s" % mid)
    ctx.ok("删互斥规则", resp)
    live = ctx.q("select count(*) from sys_role_mutex where id='%s' and deleted_at is null" % mid)
    ctx.chk("删除后不可见", live == "0", live)


def run(ctx):
    """跑 B 批次写操作验证。"""
    print("=" * 66)
    print("B 批次写操作（BASE_URL=%s）" % ctx.base_url)
    print("=" * 66)
    role_ids, _created = ensure_roles(ctx, count=2, prefix="VRFB")
    roles = [(rid, ctx.q("select coalesce(role_code,'') from sys_role where id='%s'" % rid))
             for rid in role_ids]
    if not roles:
        ctx.chk("准备角色夹具", False, "连临时角色都建不出来")
        return ctx
    for step in (lambda: b_scope(ctx, roles), lambda: b_mask(ctx, roles), lambda: b_mutex(ctx, roles)):
        try:
            step()
        except Exception as exc:  # noqa: BLE001
            ctx.chk("步骤异常", False, str(exc)[:120])
    return ctx
