#!/usr/bin/env python3
"""E2E-11 第 2 段：项目 + 公告 + 消息（SSE）+ 任务附件
   S7 项目（建项目→成员→关联任务→完成率→解绑删除）
   S8 公告（草稿→发布→可见范围→已读→附件→置顶→下线）
   S9 消息（站内信→未读/已读→模板→SSE 实时推送→频道隔离）
   S10 任务附件（上传→uploader_name 快照→下载→软删）

在 145 上执行：python3 /tmp/e2e11/e2e11_b.py
"""
import sys
import threading
import time
import urllib.request

sys.path.insert(0, "/tmp/e2e11")
from e2e11_lib import *  # noqa: F403,E402

# 1x1 PNG（公告/任务附件策略只放行 jpg,png,pdf,docx,xlsx,zip；txt 会被 60004 拒）
import base64  # noqa: E402

PNG_1PX = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)


def s7_project(state):
    scenario("S7", "项目：建项目 → 成员（MEMBER/VIEWER）→ 关联任务 → 完成率 → 解绑")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    w2 = state["users"]["e2e11_w2"]
    root = state["status_tasks"]["leaf"]  # 已 DONE 的叶子任务（完成率分母=1、分子=1）

    st, body = req("POST", "/api/business/project", admin,
                   {"name": "E2E11 演练项目", "code": "E2E11P", "description": "演练项目",
                    "owner_id": w1, "start_date": "2026-11-01", "due_date": "2026-12-31"},
                   name="s7_create")
    eq("建项目 → 200", st, 200)
    pid = data_of(body).get("id")
    eq("项目编码落库大写", psql(f"select project_no from biz_project where id='{pid}'::uuid"), "E2E11P")
    eq("项目负责人 = 甲", psql(f"select owner_id from biz_project where id='{pid}'::uuid"), w1)
    st, body = req("POST", "/api/business/project", admin,
                   {"name": "E2E11 重复编码", "code": "E2E11P"}, name="s7_dup")
    check(st in (400, 409), "重复编码被拒", f"http={st} msg={msg_of(body)[:60]}")

    st, body = req("POST", f"/api/business/project/{pid}/members", admin, {"user_id": w1, "role": "MEMBER"},
                   name="s7_member1")
    eq("加成员甲（MEMBER）→ 200", st, 200)
    eq("成员行落库 role=MEMBER", psql(
        f"select role from biz_project_member where project_id='{pid}'::uuid and user_id='{w1}'::uuid"), "MEMBER")
    st, body = req("POST", f"/api/business/project/{pid}/members", admin, {"user_id": w2, "role": "VIEWER"},
                   name="s7_member2")
    eq("加成员乙（VIEWER）→ 200", st, 200)
    eq("重复加成员幂等 changed=false", data_of(req("POST", f"/api/business/project/{pid}/members", admin,
                                                 {"user_id": w2, "role": "VIEWER"},
                                                 name="s7_member_again")[1]).get("changed"), False)
    st, body = req("POST", f"/api/business/project/{pid}/members", admin, {"user_id": w2, "role": "OWNER"},
                   name="s7_member_bad")
    eq("非法成员角色 OWNER → 400", st, 400)
    st, body = req("GET", f"/api/business/project/{pid}/members", admin, name="s7_members")
    eq("成员列表 → 200", st, 200)
    mem = data_of(body) if isinstance(data_of(body), list) else (data_of(body).get("list") or [])
    eq("成员数 = 2", len(mem), 2)
    names = {m.get("username"): m.get("role") for m in mem}
    eq("成员角色刻画（甲 MEMBER / 乙 VIEWER）", f"{names.get('e2e11_w1')}/{names.get('e2e11_w2')}", "MEMBER/VIEWER")
    check(all(m.get("real_name") for m in mem), "成员实时取名不全为空", f"names={[m.get('real_name') for m in mem]}")

    st, body = req("PUT", f"/api/business/task/{root}/project", admin, {"project_id": pid}, name="s7_bind")
    eq("任务归入项目 → 200", st, 200)
    eq("任务 project_id 落库", psql(f"select project_id from biz_task where id='{root}'::uuid"), pid)
    st, body = req("GET", f"/api/business/project/{pid}/tasks?page=1&page_size=20", admin, name="s7_ptasks")
    eq("项目任务列表 → 200", st, 200)
    ids = [x.get("id") for x in data_of(body).get("list") or []]
    check(root in ids, "项目任务列表含该任务", f"total={data_of(body).get('total')}")
    st, body = req("GET", f"/api/business/task/page?page=1&page_size=20&project_id={pid}", admin,
                   name="s7_taskfilter")
    eq("任务分页按 project_id 过滤 → 200", st, 200)
    check(root in [x.get("id") for x in data_of(body).get("list") or []], "过滤命中该任务")

    st, body = req("GET", f"/api/business/project/{pid}", admin, name="s7_detail")
    eq("项目详情 → 200", st, 200)
    d = data_of(body)
    eq("详情 task_total = 1", d.get("task_total"), 1)
    eq("详情 task_done = 1（绑定任务已 DONE）", d.get("task_done"), 1)
    eq("完成率 = 100.0", d.get("completion_rate"), 100.0)
    eq("详情 member_count = 2", d.get("member_count"), 2)
    eq("详情 owner_name 实时取名", d.get("owner_name"), "E2E11 执行人甲")

    st, body = req("GET", "/api/business/project/page?page=1&page_size=20&keyword=E2E11", admin, name="s7_page")
    eq("项目分页 → 200", st, 200)
    check(pid in [x.get("id") for x in data_of(body).get("list") or []], "项目分页命中")

    # 成员角色语义：VIEWER 与 MEMBER 目前都不改变可写性（写权限只看项目负责人/创建人/管理员）
    tw2 = state["tokens"]["e2e11_w2"]
    st, body = req("POST", f"/api/business/project/{pid}/members", tw2, {"user_id": w2, "role": "MEMBER"},
                   name="s7_member_by_viewer")
    check(st in (403, 404), "非负责人/创建人维护成员被拒", f"http={st} code={code_of(body)}")

    st, body = req("DELETE", f"/api/business/project/{pid}/members/{w2}", admin, name="s7_del_member")
    eq("移除成员 → 200", st, 200)
    eq("成员行已删", psql(
        f"select count(*) from biz_project_member where project_id='{pid}'::uuid and user_id='{w2}'::uuid"), "0")

    state["project"] = {"id": pid}
    save_state(state)


def s8_announcement(state):
    scenario("S8", "公告：草稿 → 发布（扇出通知）→ 可见范围 → 已读 → 附件 → 置顶 → 下线")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    w2 = state["users"]["e2e11_w2"]
    ro = state["users"]["e2e11_ro"]
    tw1 = state["tokens"]["e2e11_w1"]
    tw2 = state["tokens"]["e2e11_w2"]

    # 附件先上传（走存储服务，拿 file_id）；公告类策略只允许 jpg/png/pdf/docx/xlsx/zip
    payload, ctype = multipart(None, "file", "e2e11-ann.png", PNG_1PX, "image/png")
    st, body = req("POST", "/api/business/storage/upload?category=announcement", admin,
                   raw_body=payload, ctype=ctype, name="s8_upload")
    eq("上传公告附件 → 200", st, 200)
    file_id = data_of(body).get("id")
    check(bool(file_id), "附件 file_id 就绪", f"id={file_id} size={data_of(body).get('file_size')}")

    st, body = req("POST", "/api/business/announcement", admin,
                   {"title": "E2E11 全员公告", "content_md": "## E2E11 演练公告\n\n正文内容。",
                    "level": "IMPORTANT", "scope_type": "ALL", "is_top": False,
                    "attachments": [{"file_id": file_id, "sort_order": 0}]}, name="s8_create")
    eq("建公告（草稿）→ 200", st, 200)
    ann = data_of(body).get("id")
    eq("初始状态 DRAFT", data_of(body).get("status"), "DRAFT")
    check(str(data_of(body).get("ann_no") or "").startswith("AN"), "公告编号 AN*（AN+日期+6 位）",
          f"ann_no={data_of(body).get('ann_no')}")
    eq("附件绑定落库 1 条", psql(
        f"select count(*) from sys_announcement_attachment where announcement_id='{ann}'::uuid"), "1")

    # 未发布前用户看不见
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=20&keyword=E2E11", tw1,
                   name="s8_visible_draft")
    eq("草稿态对用户不可见（0 条）", data_of(body).get("total"), 0)

    st, body = req("POST", f"/api/business/announcement/{ann}/publish", admin, None, name="s8_publish")
    eq("发布 → 200", st, 200)
    eq("发布后状态 PUBLISHED", data_of(body).get("status"), "PUBLISHED")
    eq("DB 状态 PUBLISHED", psql(f"select status from sys_announcement where id='{ann}'::uuid"), "PUBLISHED")
    eq("publish_at 已写", psql(
        f"select case when publish_at is null then 'null' else 'set' end from sys_announcement where id='{ann}'::uuid"), "set")

    # 扇出通知（公告发布 → notice::dispatch，幂等）
    fan = psql("select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id "
               "where r.user_id='%s'::uuid and n.title like '%%E2E11 全员公告%%'" % w1)
    check(int(fan or 0) >= 1, "发布扇出站内信（收件人甲）", f"条数={fan}")

    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=20&keyword=E2E11", tw1,
                   name="s8_visible")
    eq("发布后用户可见 → 200", st, 200)
    items = data_of(body).get("list") or []
    hit = [x for x in items if x.get("id") == ann]
    check(bool(hit), "可见列表含该公告", f"total={data_of(body).get('total')}")
    if hit:
        eq("可见卡片 read=false", hit[0].get("read"), False)
        eq("可见卡片 attachment_count = 1", hit[0].get("attachment_count"), 1)
        eq("可见卡片 publisher_name 快照", hit[0].get("publisher_name"), "超级管理员")

    st, body = req("GET", "/api/business/announcement/unread-count", tw1, name="s8_unread")
    eq("未读数 → 200", st, 200)
    unread_before = data_of(body).get("announcement_unread")
    check(int(unread_before or 0) >= 1, "未读数 ≥1", f"未读={unread_before}")

    st, body = req("POST", f"/api/business/announcement/{ann}/read", tw1, None, name="s8_read")
    eq("标记已读 → 200", st, 200)
    eq("首次已读 first_read=true", data_of(body).get("first_read"), True)
    eq("已读回执落库", psql(
        f"select count(*) from sys_announcement_read where announcement_id='{ann}'::uuid and user_id='{w1}'::uuid"), "1")
    st, body = req("POST", f"/api/business/announcement/{ann}/read", tw1, None, name="s8_read_again")
    eq("重复已读幂等 first_read=false", data_of(body).get("first_read"), False)
    st, body = req("GET", "/api/business/announcement/unread-count", tw1, name="s8_unread_after")
    eq("已读后未读数减少 1", int(unread_before) - int(data_of(body).get("announcement_unread") or 0), 1)
    st, body = req("PUT", "/api/business/announcement/read-all", tw1, None, name="s8_read_all")
    eq("一键全部已读 → 200", st, 200)
    st, body = req("GET", "/api/business/announcement/unread-count", tw1, name="s8_unread_zero")
    eq("全部已读后未读 = 0", data_of(body).get("announcement_unread"), 0)

    st, body = req("GET", f"/api/business/announcement/{ann}/read-stats", admin, name="s8_readstats")
    eq("阅读统计 → 200", st, 200)
    eq("已读人数 = 1", data_of(body).get("read_count"), 1)

    # 附件下载（路径里的 {file_id} 语义是「附件绑定行 id」，不是 sys_file.id）
    st, body = req("GET", f"/api/business/announcement/{ann}/manage", admin, name="s8_manage_for_att")
    atts = data_of(body).get("attachments") or []
    att_id = atts[0].get("id") if atts else None
    eq("管理详情下发附件绑定行 id", bool(att_id), True)
    st, body = req("GET", f"/api/business/announcement/{ann}/attachments/{att_id}/download", tw1,
                   name="s8_download")
    eq("公告附件下载 → 200", st, 200)
    st, body = req("GET", f"/api/business/announcement/{ann}/attachment/{att_id}", tw1,
                   name="s8_download_alias")
    eq("附件下载旧路径别名 → 200", st, 200)

    # 范围：USER 只给乙 / ROLE 只给执行人角色
    st, body = req("POST", "/api/business/announcement", admin,
                   {"title": "E2E11 定向公告（仅乙）", "content_md": "定向内容", "scope_type": "USER",
                    "scope_ids": [w2]}, name="s8_create_user_scope")
    eq("建 USER 范围公告 → 200", st, 200)
    ann_u = data_of(body).get("id")
    req("POST", f"/api/business/announcement/{ann_u}/publish", admin, None, name="s8_publish_user_scope")
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 定向公告", tw1,
                   name="s8_visible_user_scope_w1")
    eq("甲看不到只发给乙的公告", data_of(body).get("total"), 0)
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 定向公告", tw2,
                   name="s8_visible_user_scope_w2")
    eq("乙看得到发给自己的公告", data_of(body).get("total"), 1)

    role_id = state["roles"]["E2E11_WORKER"]
    st, body = req("POST", "/api/business/announcement", admin,
                   {"title": "E2E11 角色公告（执行人角色）", "content_md": "角色范围内容",
                    "scope_type": "ROLE", "scope_ids": [role_id]}, name="s8_create_role_scope")
    eq("建 ROLE 范围公告 → 200", st, 200)
    ann_r = data_of(body).get("id")
    req("POST", f"/api/business/announcement/{ann_r}/publish", admin, None, name="s8_publish_role_scope")
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 角色公告", tw2,
                   name="s8_visible_role_w2")
    eq("同角色用户（乙）看得到 ROLE 公告", data_of(body).get("total"), 1)
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 角色公告", admin,
                   name="s8_visible_role_admin")
    eq("超管不在 ROLE 受众里 → 公告中心看不到（受众口径，管理端可看）", data_of(body).get("total"), 0)
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 全员公告", admin,
                   name="s8_visible_all_admin")
    eq("ALL 范围公告超管可见", data_of(body).get("total"), 1)

    # 范围为空 → 400
    st, body = req("POST", "/api/business/announcement", admin,
                   {"title": "E2E11 空范围公告", "content_md": "x", "scope_type": "ROLE", "scope_ids": []},
                   name="s8_empty_scope")
    eq("ROLE 范围但 scope_ids 空 → 400", st, 400)
    eq("错误码 62006", code_of(body), 62006)

    # 置顶
    st, body = req("PUT", f"/api/business/announcement/{ann}/top", admin,
                   {"is_top": True, "top_expire_at": "2026-12-31T00:00:00Z"}, name="s8_top")
    eq("置顶（带到期时间）→ 200", st, 200)
    eq("is_top 落库", psql(f"select is_top from sys_announcement where id='{ann}'::uuid"), "t")
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11", tw1,
                   name="s8_visible_top")
    first = (data_of(body).get("list") or [{}])[0]
    eq("置顶公告排在可见列表首位", first.get("id"), ann)

    # 管理端列表 / 详情
    st, body = req("GET", "/api/business/announcement/page?page=1&page_size=20&keyword=E2E11", admin,
                   name="s8_manage_page")
    eq("管理端分页 → 200", st, 200)
    check(int(data_of(body).get("total") or 0) >= 3, "管理端看到本卡 3 条公告", f"total={data_of(body).get('total')}")
    st, body = req("GET", f"/api/business/announcement/{ann}/manage", admin, name="s8_manage_detail")
    eq("管理端详情 → 200", st, 200)
    eq("详情带 Markdown 原文", "E2E11 演练公告" in str(data_of(body).get("content_md") or ""), True)

    # 下线
    st, body = req("POST", f"/api/business/announcement/{ann}/offline", admin, {"reason": ""}, name="s8_offline_no_reason")
    check(st in (400, 422), "下线原因空 → 4xx", f"http={st}")
    st, body = req("POST", f"/api/business/announcement/{ann}/offline", admin,
                   {"reason": "E2E11 演练下线"}, name="s8_offline")
    eq("下线 → 200", st, 200)
    eq("状态 OFFLINE", psql(f"select status from sys_announcement where id='{ann}'::uuid"), "OFFLINE")
    st, body = req("GET", "/api/business/announcement/visible?page=1&page_size=50&keyword=E2E11 全员公告", tw1,
                   name="s8_visible_after_offline")
    eq("下线后用户不可见", data_of(body).get("total"), 0)

    state["announcement"] = {"all": ann, "user": ann_u, "role": ann_r, "file_id": file_id}
    save_state(state)


# ---------- SSE ----------
SSE_BOX = {}


def sse_open(name, token, timeout=25):
    """后台线程订阅 SSE，把收到的 data 行塞进 SSE_BOX[name]。"""
    box = SSE_BOX.setdefault(name, {"events": [], "ready": threading.Event(), "error": None})

    def run():
        try:
            request = urllib.request.Request(BASE + "/api/business/notice/stream", method="GET")
            request.add_header("Authorization", "Bearer " + token)
            request.add_header("Accept", "text/event-stream")
            with urllib.request.urlopen(request, timeout=timeout) as resp:
                box["status"] = resp.status
                box["ctype"] = resp.headers.get("Content-Type", "")
                box["ready"].set()
                deadline = time.time() + timeout
                while time.time() < deadline:
                    line = resp.readline()
                    if not line:
                        break
                    text = line.decode("utf-8", "replace").strip()
                    if text.startswith("data:"):
                        box["events"].append(text[5:].strip())
        except Exception as exc:  # noqa: BLE001
            box["error"] = str(exc)
            box["ready"].set()

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    box["thread"] = thread
    return box


def s9_notice(state):
    scenario("S9", "消息：站内信 → 未读/已读 → 模板 → SSE 实时推送 → 频道隔离")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    w2 = state["users"]["e2e11_w2"]
    tw1 = state["tokens"]["e2e11_w1"]
    tw2 = state["tokens"]["e2e11_w2"]

    # ① SSE 先连上（甲、乙各一条）
    box1 = sse_open("w1", tw1)
    box2 = sse_open("w2", tw2)
    box1["ready"].wait(5)
    box2["ready"].wait(5)
    eq("SSE 订阅甲 → HTTP 200", box1.get("status") or (200 if not box1.get("error") else 0), 200)
    contains("SSE Content-Type = text/event-stream", box1.get("ctype", ""), "text/event-stream")
    st, body = req("GET", "/api/business/notice/stream/stats", admin, name="s9_stream_stats")
    eq("频道统计 → 200", st, 200)
    stats = data_of(body)
    check(int(stats.get("total_users") or 0) >= 2, "频道表 ≥2 个在线用户", f"total_users={stats.get('total_users')}")
    check(int(stats.get("total_receivers") or 0) >= 2, "订阅端 ≥2", f"total_receivers={stats.get('total_receivers')}")

    # ② 发站内信（USER 定向甲）
    st, body = req("POST", "/api/business/notice", admin,
                   {"notice_type": "TODO", "level": "WARN", "title": "E2E11 消息直投",
                    "content": "E2E11 SSE 演练正文", "target_type": "USER", "target_ids": [w1],
                    "link_url": "/business/task"}, name="s9_send")
    eq("发送站内信 → 200", st, 200)
    sent = data_of(body)
    check(int(sent.get("receiver_count") or 0) == 1, "接收人数 = 1", f"receiver_count={sent.get('receiver_count')}")
    notice_id = sent.get("notice_id")
    eq("接收行落库", psql(
        f"select count(*) from sys_notice_receiver where notice_id='{notice_id}'::uuid and user_id='{w1}'::uuid"), "1")

    # ③ SSE 实时推送
    for _ in range(60):
        if box1["events"]:
            break
        time.sleep(0.25)
    check(bool(box1["events"]), "SSE 实时收到推送（甲）", f"事件数={len(box1['events'])}")
    if box1["events"]:
        import json as _json
        event = _json.loads(box1["events"][0])
        eq("SSE 事件标题", event.get("title"), "E2E11 消息直投")
        eq("SSE 事件带 receiver_id（前端据此已读）", bool(event.get("receiver_id")), True)
        eq("SSE 事件 notice_id 一致", event.get("notice_id"), notice_id)
    check(not box2["events"], "频道隔离：未投递给乙", f"乙事件数={len(box2['events'])}")

    # ④ 我的通知 / 未读 / 已读 / 删除
    st, body = req("GET", "/api/business/notice/mine?page=1&page_size=50", tw1, name="s9_mine")
    eq("我的通知 → 200", st, 200)
    mine = data_of(body).get("list") or []
    hit = [x for x in mine if x.get("notice_id") == notice_id]
    check(bool(hit), "我的通知含该条", f"total={data_of(body).get('total')}")
    receiver_id = hit[0].get("receiver_id") if hit else None
    st, body = req("GET", "/api/business/notice/unread-count", tw1, name="s9_unread")
    eq("未读数 → 200", st, 200)
    unread_before = int(data_of(body).get("unread_count") or 0)
    check(unread_before >= 1, "未读数 ≥1", f"未读={unread_before}")
    st, body = req("PUT", f"/api/business/notice/read/{receiver_id}", tw1, None, name="s9_read")
    eq("单条已读 → 200", st, 200)
    eq("已读标记落库", psql(
        f"select case when read_at is null then 'unread' else 'read' end from sys_notice_receiver "
        f"where id='{receiver_id}'::uuid"), "read")
    st, body = req("GET", "/api/business/notice/unread-count", tw1, name="s9_unread_after")
    eq("已读后未读数 -1", unread_before - int(data_of(body).get("unread_count") or 0), 1)

    st, body = req("PUT", "/api/business/notice/read-all", tw1, None, name="s9_read_all")
    eq("全部已读 → 200", st, 200)
    st, body = req("GET", "/api/business/notice/unread-count", tw1, name="s9_unread_zero")
    eq("全部已读后未读 = 0", data_of(body).get("unread_count"), 0)

    # ⑤ 模板：建 → 试渲染 → 按模板发送 → 被引用时删除 409
    st, body = req("POST", "/api/business/notice/template", admin,
                   {"template_code": "E2E11_TPL", "template_name": "E2E11 演练模板",
                    "notice_type": "SYSTEM", "default_level": "INFO",
                    "title_template": "【E2E11】{{action}}通知",
                    "content_template": "任务 {{task}} 已 {{action}}",
                    "variables": [{"name": "action", "label": "动作", "required": True, "sample": "完成"},
                                  {"name": "task", "label": "任务", "required": True, "sample": "E2E11 巡检"}]},
                   name="s9_tpl_create")
    eq("建通知模板 → 200", st, 200)
    tpl_id = data_of(body).get("id")
    st, body = req("POST", f"/api/business/notice/template/{tpl_id}/preview", admin,
                   {"variables": {"action": "完成", "task": "E2E11 巡检"}}, name="s9_tpl_preview")
    eq("模板试渲染 → 200", st, 200)
    contains("渲染标题含变量值", str(data_of(body).get("title") or ""), "E2E11")
    st, body = req("POST", f"/api/business/notice/template/{tpl_id}/preview", admin,
                   {"variables": {"action": "完成"}}, name="s9_tpl_preview_missing")
    eq("试渲染缺变量 → 200（用模板 sample 兜底）", st, 200)
    contains("缺失变量用 sample 填充", str(data_of(body).get("content") or ""), "E2E11 巡检")
    st, body = req("POST", "/api/business/notice", admin,
                   {"template_code": "E2E11_TPL", "template_vars": {"action": "完成"},
                    "target_type": "USER", "target_ids": [w2]}, name="s9_send_missing_var")
    check(st in (400, 422), "按模板发送缺必填变量 → 4xx", f"http={st} code={code_of(body)}")

    st, body = req("POST", "/api/business/notice", admin,
                   {"template_code": "E2E11_TPL", "template_vars": {"action": "完成", "task": "E2E11 巡检"},
                    "target_type": "USER", "target_ids": [w2]}, name="s9_send_by_tpl")
    eq("按模板发送 → 200", st, 200)
    tpl_notice = data_of(body).get("notice_id")
    eq("模板渲染标题落库", psql(f"select title from sys_notice where id='{tpl_notice}'::uuid"), "【E2E11】完成通知")
    st, body = req("DELETE", f"/api/business/notice/template/{tpl_id}", admin, name="s9_tpl_del_ref")
    eq("被通知引用的模板删除 → 409", st, 409)

    # ⑥ 管理端分页 / 详情 / 删除
    st, body = req("GET", "/api/business/notice/page?page=1&page_size=20&keyword=E2E11", admin, name="s9_page")
    eq("通知管理分页 → 200", st, 200)
    check(int(data_of(body).get("total") or 0) >= 2, "管理端看到本卡通知", f"total={data_of(body).get('total')}")
    st, body = req("GET", f"/api/business/notice/{tpl_notice}", admin, name="s9_detail")
    eq("通知详情 → 200", st, 200)
    detail = data_of(body)
    eq("详情带通知主体", bool(detail.get("notice")), True)
    eq("详情带已读/未读统计", detail.get("read_count"), 0)
    st, body = req("DELETE", f"/api/business/notice/{tpl_notice}", admin, name="s9_del_notice")
    eq("删除通知（软删）→ 200", st, 200)
    st, body = req("GET", "/api/business/notice/mine?page=1&page_size=50", tw2, name="s9_mine_after_del")
    eq("接收行同步不可见", [x.get("notice_id") for x in data_of(body).get("list") or []].count(tpl_notice), 0)

    box1["thread"].join(1)
    box2["thread"].join(1)
    state["notice"] = {"notice_id": notice_id, "receiver_id": receiver_id, "tpl_notice": tpl_notice}
    save_state(state)


def s10_attachment(state):
    scenario("S10", "任务附件（AUD-01 回归）：上传 → uploader_name 快照 → 改名后仍是旧名 → 下载 → 软删")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    tw1 = state["tokens"]["e2e11_w1"]
    task = state["chain"][1]

    st, body = req("POST", "/api/business/task", admin, {"title": "E2E11 附件任务", "owner_id": w1},
                   name="s10_mk")
    att_task = data_of(body).get("id")

    payload, ctype = multipart(None, "file", "e2e11-task.png", PNG_1PX + b"E2E11TASK", "image/png")
    st, body = req("POST", f"/api/business/task/{att_task}/attachment", tw1, raw_body=payload, ctype=ctype,
                   name="s10_upload")
    eq("上传任务附件 → 200", st, 200)
    att = data_of(body)
    check(bool(att.get("attachment_id")), "附件绑定 id 就绪", f"keys={list(att.keys())}")
    eq("文件名回显", att.get("file_name"), "e2e11-task.png")
    eq("uploader_name 落库快照 = 执行人甲", psql(
        f"select uploader_name from biz_task_attachment where task_id='{att_task}'::uuid and deleted_at is null"),
       "E2E11 执行人甲")

    st, body = req("GET", f"/api/business/task/{att_task}/attachments", tw1, name="s10_list")
    eq("附件列表 → 200", st, 200)
    rows = data_of(body).get("list") or []
    eq("列表 1 条", len(rows), 1)
    eq("列表带 uploader_name", rows[0].get("uploader_name"), "E2E11 执行人甲")
    att_id = rows[0].get("id")

    # AUD-01 核心：改名后回看，仍是上传当时的人名
    psql(f"update sys_user set real_name='E2E11 执行人甲（改名后）' where id='{w1}'::uuid")
    st, body = req("GET", f"/api/business/task/{att_task}/attachments", tw1, name="s10_list_after_rename")
    eq("改名后 uploader_name 仍是旧名（快照不回改）", (data_of(body).get("list") or [{}])[0].get("uploader_name"),
       "E2E11 执行人甲")
    st, body = req("GET", f"/api/business/task/{att_task}", tw1, name="s10_detail_after_rename")
    eq("任务负责人姓名走实时 join（显示新名）", data_of(body).get("owner_name"), "E2E11 执行人甲（改名后）")
    psql(f"update sys_user set real_name='E2E11 执行人甲' where id='{w1}'::uuid")

    st, body = req("GET", f"/api/business/task/{att_task}/attachments/{att_id}/download", tw1, name="s10_download")
    eq("附件下载 → 200", st, 200)
    st, body = req("GET", f"/api/business/task/{att_task}/attachments/{att_id}/download", admin, name="s10_download_admin")
    eq("管理员也能下载 → 200", st, 200)

    st, body = req("DELETE", f"/api/business/task/{att_task}/attachments/{att_id}", admin, name="s10_del")
    eq("软删附件 → 200", st, 200)
    st, body = req("GET", f"/api/business/task/{att_task}/attachments", tw1, name="s10_list_after_del")
    eq("软删后列表为空", len(data_of(body).get("list") or []), 0)
    eq("附件行软删留痕", psql(
        f"select case when deleted_at is null then 'live' else 'deleted' end from biz_task_attachment where id='{att_id}'::uuid"),
       "deleted")

    state["attachment"] = {"task": att_task, "att_id": att_id}
    save_state(state)


def main():
    state = load_state()
    if not state.get("admin_token"):
        raise SystemExit("缺少 a 段状态（state.json），请先跑 e2e11_a.py")
    s7_project(state)
    s8_announcement(state)
    s9_notice(state)
    s10_attachment(state)
    print("\n================ B 段汇总 ================")
    total_ok = sum(v[0] for v in STATS.values())
    total_bad = sum(v[1] for v in STATS.values())
    for sid in sorted(STATS):
        ok, bad = STATS[sid]
        print(f"  {sid}: PASS={ok} FAIL={bad}")
    print(f"  合计 PASS={total_ok} FAIL={total_bad}")
    save_state(state)
    return 0 if total_bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
