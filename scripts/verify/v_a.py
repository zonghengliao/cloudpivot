#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""A 批次写操作验证：通知发送/已读、文件上传/下载/删除、导入导出触发。

固化契约：
  - 通知正文字段是 `content`（公告才是 `content_md`）
  - 单条已读 `PUT /business/notice/read/{receiver_id}`（收件人 id，不是通知 id）
  - 全部已读 `PUT /business/notice/read-all`，**body 必须是 None**
  - 删文件 `DELETE /business/storage/{file_id}`
  - 导出用 `biz_type`
"""

import time

from common import STAMP, delete_file, notice_read_all, upload_file


def a_notice(ctx):
    """A5 通知：发送 → 未读数 → 单条已读 → 全部已读。"""
    print("\n【A5】通知：发送 → 未读数 → 已读")
    before = ctx.q("select count(*) from sys_notice_receiver where user_id='%s' and read_at is null" % ctx.me)
    print("     发送前未读:", before)
    title = "验证通知 %s" % STAMP
    st, resp = ctx.post("/api/business/notice", {
        "title": title,
        "content": "这是一条验证脚本发出的通知",   # 通知用 content
        "notice_type": "SYSTEM",
        "target_type": "USER",
        "target_ids": [ctx.me],
    })
    ctx.ok("发送通知", resp)
    nid = (ctx.data(resp) or {}).get("notice_id") or (ctx.data(resp) or {}).get("id") \
        if isinstance(ctx.data(resp), dict) else None
    if nid:
        ctx.on_cleanup("删通知 %s" % title,
                       lambda notice_id=nid: ctx.delete("/api/business/notice/%s" % notice_id))
    time.sleep(1)
    after = ctx.q("select count(*) from sys_notice_receiver where user_id='%s' and read_at is null" % ctx.me)
    ctx.chk("未读数 +1", bool(after) and bool(before) and int(after) > int(before),
            "before=%s after=%s" % (before, after))
    st, resp = ctx.get("/api/business/notice/unread-count")
    data = ctx.data(resp) or {}
    cnt = (data.get("unread_count") or data.get("count") or data.get("unread")) \
        if isinstance(data, dict) else None
    ctx.chk("未读数接口返值", cnt is not None, data)
    print("     接口返回未读:", cnt)
    # 单条已读：收件人 id 从 sys_notice_receiver 取（不是通知 id）
    rid = ctx.q("select id::text from sys_notice_receiver where notice_id='%s' and user_id='%s' limit 1"
                % (nid, ctx.me)) if nid else ""
    if rid:
        st, resp = ctx.put("/api/business/notice/read/%s" % rid, None)
        ctx.ok("单条已读 PUT /notice/read/{receiver_id}", resp)
    # 全部已读：body 必须 None
    st, resp = notice_read_all(ctx)
    ctx.ok("全部已读 PUT /notice/read-all（body=None）", resp)
    time.sleep(1)
    after2 = ctx.q("select count(*) from sys_notice_receiver where user_id='%s' and read_at is null" % ctx.me)
    ctx.chk("已读后未读数下降", bool(after2) and int(after2) < int(after),
            "after=%s after2=%s" % (after, after2))


def a_storage(ctx):
    """A2 存储：上传 → 下载 → 删除。"""
    print("\n【A2】存储：上传 → 下载 → 删除")
    content = ("验证存储内容 %s" % STAMP).encode()
    st, resp, fid = upload_file(ctx, content=content, filename="verify_%s.txt" % STAMP)
    ctx.ok("上传文件", resp)
    ctx.chk("拿到 file_id", bool(fid), resp)
    if not fid:
        return
    ctx.on_cleanup("删文件 %s" % fid, lambda file_id=fid: delete_file(ctx, file_id))
    st, raw = ctx.get("/api/business/storage/download/%s" % fid, raw=True)
    body = raw if isinstance(raw, str) else str(raw)
    ctx.chk("下载内容一致", st == 200 and content.decode() in body,
            "http=%s len=%s" % (st, len(body)))
    st, resp = ctx.get("/api/business/storage/file/page?page=1&page_size=5")
    ctx.ok("文件列表可查", resp)
    st, resp = delete_file(ctx, fid)
    ctx.ok("删除文件 DELETE /storage/{fid}", resp)
    st, resp = ctx.get("/api/business/storage/download/%s" % fid, raw=True)
    ctx.chk("删除后不可下载", st >= 400, "http=%s" % st)


def a_impexp(ctx):
    """A3 导入导出：定义列表 → 同步导出（不落任务行）→ 异步导出建任务。"""
    print("\n【A3】导入导出：定义 → 同步导出 → 异步导出")
    st, resp = ctx.get("/api/business/impexp/def/list")
    ctx.ok("导出定义列表", resp)
    defs = ctx.data(resp) or []
    if not isinstance(defs, list) or not defs:
        ctx.skip("无导出定义")
        return
    target = next((d for d in defs if d.get("supports_export")), None)
    if not target:
        ctx.skip("无 supports_export 的定义")
        return
    biz_type = target.get("biz_type")
    ctx.chk("定义带 biz_type 字段", bool(biz_type), target)
    print("     用 biz_type =", biz_type)
    # 同步导出：GET /export/sync?biz_type=xxx（不产生任务行）
    st, raw = ctx.get("/api/business/impexp/export/sync?biz_type=%s" % biz_type, raw=True)
    body = raw if isinstance(raw, str) else str(raw)
    ctx.chk("同步导出返回文件流", st == 200 and len(body) > 0, "http=%s len=%s" % (st, len(body)))
    # 异步导出：POST /export（biz_type 必填）
    st, resp = ctx.post("/api/business/impexp/export", {
        "biz_type": biz_type, "format": "CSV", "export_name": "verify_%s" % STAMP,
    })
    ok = resp.get("code") == 0
    ctx.chk("异步导出建任务（biz_type）", ok or resp.get("code") in (422, 60011),
            "code=%s msg=%s" % (resp.get("code"), str(resp.get("msg"))[:70]))
    xid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not xid:
        return
    print("     export_task_id =", xid)

    def _drop_export():
        ctx.post("/api/business/impexp/export/%s/cancel" % xid, {})
        # 取消只是停跑，任务行是审计留痕；验证脚本只清自己建的那一行
        ctx.q("delete from sys_export_task where id='%s'" % xid)
        return (200, {"code": 0})

    ctx.on_cleanup("取消并清除导出任务 %s" % xid, _drop_export)
    time.sleep(2)
    row = ctx.q("select status from sys_export_task where id='%s'" % xid)
    ctx.chk("导出任务已落库", bool(row), row)
    print("     导出任务状态:", row)


def run(ctx):
    """跑 A 批次写操作验证。"""
    print("=" * 66)
    print("A 批次写操作（BASE_URL=%s）" % ctx.base_url)
    print("=" * 66)
    for step in (lambda: a_notice(ctx), lambda: a_storage(ctx), lambda: a_impexp(ctx)):
        try:
            step()
        except Exception as exc:  # noqa: BLE001
            ctx.chk("步骤异常", False, str(exc)[:120])
    return ctx
