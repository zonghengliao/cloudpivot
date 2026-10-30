#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""C 批次写操作验证：公告全生命周期 / 帮助文档（分类+文档+版本）/ 变更日志。

固化契约：
  - 公告正文是 `content_md`（不是 `content`）
  - 帮助分类必填 `code`；帮助文档必填 `slug` + `content_md`
  - 变更日志库表 `sys_change_log` 的列是 `resource_id`（不是 `target_id`）
"""

import time

from common import STAMP, changelog_count


def c_announcement(ctx):
    """C2 公告：草稿 → 发布 → 可见（XSS 净化）→ 已读 → 下线。"""
    print("\n【C2】公告：草稿 → 发布 → 可见 → 已读 → 下线")
    title = "验证公告 %s" % STAMP
    st, resp = ctx.post("/api/business/announcement", {
        "title": title,
        "content_md": "# 验证公告\n\n这是 **Markdown** 正文，含 `<script>alert(1)</script>` 用于 XSS 验证。",
        "scope_type": "ALL",
        "is_top": False,
    })
    ctx.ok("建公告（content_md 草稿）", resp)
    aid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    ctx.chk("拿到 announcement_id", bool(aid), resp)
    if not aid:
        return
    ctx.on_cleanup("删公告 %s" % title,
                   lambda ann_id=aid: ctx.delete("/api/business/announcement/%s" % ann_id))
    status = ctx.q("select status from sys_announcement where id='%s'" % aid)
    ctx.chk("初始状态 DRAFT", status == "DRAFT", status)
    st, resp = ctx.post("/api/business/announcement/%s/publish" % aid, {})
    ctx.ok("发布公告", resp)
    time.sleep(1)
    ctx.chk("库状态 → PUBLISHED",
            ctx.q("select status from sys_announcement where id='%s'" % aid) == "PUBLISHED",
            ctx.q("select status from sys_announcement where id='%s'" % aid))
    st, resp = ctx.get("/api/business/announcement/visible/page?page=1&page_size=20")
    items = (ctx.data(resp) or {}).get("list") if isinstance(ctx.data(resp), dict) else []
    hit = next((x for x in (items or []) if x.get("id") == aid), None)
    ctx.chk("用户侧可见", hit is not None, "total=%s" % (ctx.data(resp) or {}).get("total") if isinstance(ctx.data(resp), dict) else "")
    # 正文只在详情接口下发（列表是摘要视图）；详情给的是**净化后的 content_html**，
    # 不是原始 content_md —— 所以 XSS 断言应该打在 content_html 上
    st, detail = ctx.get("/api/business/announcement/%s" % aid)
    ctx.ok("详情接口可读", detail)
    ddata = ctx.data(detail) if isinstance(ctx.data(detail), dict) else {}
    content = str(ddata.get("content_html") or "")
    ctx.chk("详情返回净化后的 content_html", bool(content), list(ddata.keys())[:8])
    ctx.chk("XSS 净化（无 <script>）", "<script" not in content.lower(), content[:100])
    st, resp = ctx.post("/api/business/announcement/%s/read" % aid, None)
    ctx.ok("标记已读", resp)
    st, resp = ctx.post("/api/business/announcement/%s/offline" % aid, {"reason": "verify 脚本验证下线"})
    ctx.ok("下线（reason 必填）", resp)
    time.sleep(1)
    st, resp = ctx.get("/api/business/announcement/visible/page?page=1&page_size=20")
    items2 = (ctx.data(resp) or {}).get("list") if isinstance(ctx.data(resp), dict) else []
    ctx.chk("下线后不可见", not any(x.get("id") == aid for x in (items2 or [])), "还在可见列表")
    ctx.chk("库状态 → OFFLINE",
            ctx.q("select status from sys_announcement where id='%s'" % aid) == "OFFLINE",
            ctx.q("select status from sys_announcement where id='%s'" % aid))


def c_help(ctx):
    """C4 帮助：建分类（code）→ 建文档（slug+content_md）→ 改两次 → 版本回滚。"""
    print("\n【C4】帮助文档：建分类 → 建文档 → 版本 → 回滚")
    cat_name = "验证分类 %s" % STAMP
    st, resp = ctx.post("/api/business/help/category", {
        "name": cat_name,
        "code": "vrf_cat_%s" % STAMP,        # 必填 code
        "parent_id": None,
        "sort_order": 99,
    })
    ctx.ok("建帮助分类（code 必填）", resp)
    cid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not cid:
        return
    ctx.on_cleanup("删帮助分类 %s" % cat_name,
                   lambda cat_id=cid: ctx.delete("/api/business/help/category/%s" % cat_id))
    title = "验证帮助文档 %s" % STAMP
    st, resp = ctx.post("/api/business/help/doc", {
        "category_id": cid,
        "title": title,
        "slug": "verify-doc-%s" % STAMP,     # 必填 slug
        "content_md": "# 标题\n\n正文 v0",     # 必填 content_md
    })
    ctx.ok("建帮助文档（slug + content_md）", resp)
    did = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not did:
        return
    ctx.on_cleanup("删帮助文档 %s" % title,
                   lambda doc_id=did: ctx.delete("/api/business/help/doc/%s" % doc_id))
    # PUT /help/doc/{id} 是**全量更新**（DocSaveReq 四个字段都必填），只传 content_md 会被拒
    slug = "verify-doc-%s" % STAMP
    for ver in (1, 2):
        st, resp = ctx.put("/api/business/help/doc/%s" % did, {
            "category_id": cid,
            "title": title,
            "slug": slug,
            "content_md": "# 标题\n\n正文 v%d" % ver,
        })
        ctx.ok("改文档产生 v%d" % ver, resp)
        time.sleep(0.4)
    nver = ctx.q("select count(*) from sys_help_doc_version where doc_id='%s'" % did)
    ctx.chk("产生多个版本", bool(nver) and int(nver) >= 2, nver)
    print("     版本数:", nver)
    st, resp = ctx.post("/api/business/help/doc/%s/versions/1/rollback" % did, {})
    ctx.ok("版本回滚到 v1", resp)


def c_changelog(ctx):
    """C6 变更日志：改用户 → 日志有记录（resource_id）。"""
    print("\n【C6】变更日志：改用户 → 日志有记录")
    uid = ctx.q("select id::text from sys_user where deleted_at is null and username<>'%s' limit 1" % ctx.user)
    if not uid:
        ctx.skip("无第二个用户可改")
        return
    old_name = ctx.q("select real_name from sys_user where id='%s'" % uid)
    before = changelog_count(ctx, "sys_user", uid)
    print("     改前日志数:", before, " 目标用户:", uid[:8])
    st, resp = ctx.put("/api/system/user/%s" % uid, {"real_name": "验证改名 %s" % STAMP})
    ctx.ok("改用户", resp)
    if resp.get("code") == 0 and old_name:
        ctx.on_cleanup("还原用户 real_name",
                       lambda user_id=uid, name=old_name: ctx.put("/api/system/user/%s" % user_id, {"real_name": name}))
    time.sleep(1)
    after = changelog_count(ctx, "sys_user", uid)
    ctx.chk("日志记录 +1（resource_id 列）", after > before, "before=%s after=%s" % (before, after))
    st, resp = ctx.get("/api/business/changelog/timeline?table=sys_user&target_id=%s&limit=5" % uid)
    ctx.ok("timeline 接口（参数叫 target_id）", resp)
    data = ctx.data(resp) or {}
    ctx.chk("timeline 有记录", bool(data.get("items") or data.get("total")),
            str(data)[:90])


def run(ctx):
    """跑 C 批次写操作验证。"""
    print("=" * 66)
    print("C 批次写操作（BASE_URL=%s）" % ctx.base_url)
    print("=" * 66)
    for step in (lambda: c_announcement(ctx), lambda: c_help(ctx), lambda: c_changelog(ctx)):
        try:
            step()
        except Exception as exc:  # noqa: BLE001
            ctx.chk("步骤异常", False, str(exc)[:120])
    return ctx
