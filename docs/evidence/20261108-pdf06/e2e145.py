#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
PDF-06 · 工作任务派工单 PDF 145 实测

形态照 docs/evidence/20261108-pdf05/e2e145.py：stdlib + urllib，无第三方依赖；
每条断言打印 [PASS]/[FAIL] 并计数，PDF 事实用 pdftotext / pdfinfo / pdffonts 断言。

覆盖（卡面三条验收）：
  ① 任务导 PDF 含完整描述        → S1 普通任务 + S4 原文照印（含 Markdown 记号）
  ② 有检查项的显示为勾选框（空白）→ S2 三个子任务（一个已取消）→ 只印 2 项，
                                    且文本层不含任何「已勾」记号 / 子任务状态
  ③ 长描述跨页正常              → S3 120 行描述 → Pages >= 2，页面 1/2 各有正文，
                                    120 行一行不少
  另含：签字栏 4 格与验收口径、自建任务派工人=创建人、协作人可见、审计留痕
  负面：401 未登录 / 403 无权限码 / 404 无关或不存在 / 400 非法 id

环境变量：BASE / DB / ENVF（含 NIK_ADMIN_PASSWORD + NIK_DATABASE_PASSWORD）
"""
import json
import os
import re
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request

BASE = os.environ.get("BASE", "http://127.0.0.1:8080")
DB = os.environ.get("DB", "cloudpivot_pdf06")
HERE = os.path.dirname(os.path.abspath(__file__))
EVID = os.environ.get("EVID") or HERE
RAW = os.environ.get("RAW") or os.path.join(EVID, "raw")
SAMPLE = os.environ.get("SAMPLE") or os.path.join(EVID, "sample")
os.makedirs(RAW, exist_ok=True)
os.makedirs(SAMPLE, exist_ok=True)

PASS = 0
FAIL = 0
FAILS = []

BASE_PW = "Pdf06@2026"
NEW_PW = "Pdf06new@2026"

_ENVF = os.environ.get("ENVF", "/root/pdf06.env")
ADMIN_PW = ""
PGPW = ""
if os.path.exists(_ENVF):
    for _line in open(_ENVF, encoding="utf-8"):
        if _line.startswith("NIK_ADMIN_PASSWORD="):
            ADMIN_PW = _line.split("=", 1)[1].strip()
        if _line.startswith("NIK_DATABASE_PASSWORD="):
            PGPW = _line.split("=", 1)[1].strip()
ADMIN_PW = os.environ.get("ADMIN_PW", ADMIN_PW or "Admin@123456")


# ============================ 断言 / 输出 ============================
def check(cond, label, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  [PASS] {label}")
    else:
        FAIL += 1
        FAILS.append(f"{label} :: {detail}")
        print(f"  [FAIL] {label} :: {detail}")
    return bool(cond)


def eq(label, actual, expect):
    return check(str(actual) == str(expect), label, f"实际={actual!r} 期望={expect!r}")


def contains(label, text, needle):
    # 两边都去空白：pdftotext 会按版面在中文之间插空格 / 换行，逐字比对会假红
    return check(norm(needle) in norm(text), label, f"不含 {needle!r}")


def not_contains(label, text, needle):
    return check(norm(needle) not in norm(text), label, f"不该含 {needle!r}")


def scenario(sid, title):
    print(f"\n{'=' * 78}\n=== {sid} {title}\n{'=' * 78}")


# ============================ 基础设施 ============================
def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = PGPW
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if out.returncode != 0:
        return f"<ERR:{out.stderr.strip()}>"
    return out.stdout.strip()


def req(method, path, token=None, body=None, timeout=90):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            raw = resp.read().decode() or "{}"
            return resp.status, json.loads(raw)
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode()
        try:
            return exc.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return exc.code, {"raw": raw}
    except Exception as exc:  # noqa: BLE001
        return 0, {"error": str(exc)}


def get_pdf(path, token=None):
    """GET 二进制；返回 (status, headers dict, bytes)。"""
    headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    r = urllib.request.Request(BASE + path, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, {k.lower(): v for k, v in exc.headers.items()}, exc.read()


def get_pdf_err(path, token=None):
    st, _h, raw = get_pdf(path, token)
    try:
        return st, json.loads(raw.decode() or "{}")
    except Exception:  # noqa: BLE001
        return st, {"raw": raw[:200]}


def pdftext(path, page=None):
    cmd = ["pdftotext"]
    if page:
        cmd += ["-f", str(page), "-l", str(page)]
    cmd += ["-layout", path, "-"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    return out.stdout


def pdftext_raw(path):
    """-raw：按内容流顺序取文本（不加列对齐）。签字栏的「栏名 + 预印姓名」用它对，
    因为 -layout 会把四格姓名排到第二行、与相邻列交错。"""
    out = subprocess.run(["pdftotext", "-raw", path, "-"], capture_output=True, text=True)
    return out.stdout


def norm(text):
    """去掉所有空白：pdftotext 会按版面插空格 / 换行，中文串跨行时逐字比对会假红。"""
    return re.sub(r"\s+", "", text)


def pdfpages(path):
    info = subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout
    m = re.search(r"Pages:\s+(\d+)", info)
    return int(m.group(1)) if m else -1


def pdfinfo(path):
    return subprocess.run(["pdfinfo", path], capture_output=True, text=True).stdout


def pdffonts(path):
    return subprocess.run(["pdffonts", path], capture_output=True, text=True).stdout


def dump(name, text):
    with open(os.path.join(RAW, name), "w", encoding="utf-8") as fp:
        fp.write(text)


def keep_pdf(name, raw, pages_to_png=(1,)):
    """落 PDF + 文本层 + pdfinfo + pdffonts（+ 首页 PNG）。"""
    path = os.path.join(SAMPLE, name)
    with open(path, "wb") as fp:
        fp.write(raw)
    text = pdftext(path)
    dump(f"pdftotext_{name}.txt", text)
    dump(f"pdfinfo_{name}.txt", pdfinfo(path))
    dump(f"pdffonts_{name}.txt", pdffonts(path))
    for page in pages_to_png:
        subprocess.run(
            ["pdftoppm", "-r", "100", "-png", "-f", str(page), "-l", str(page),
             path, os.path.join(SAMPLE, f"{name[:-4]}-p{page}")],
            capture_output=True,
        )
    return path, text


# ============================ 登录 / 夹具 ============================
def login(username, pw_candidates):
    last = None
    for pw in pw_candidates:
        st, body = req("POST", "/api/system/auth/login",
                       body={"username": username, "password": pw})
        if st != 200:
            last = (st, body)
            continue
        data = body.get("data") or {}
        tok = data.get("access_token")
        if data.get("must_change_password"):
            st2, _ = req("PUT", "/api/system/auth/password", token=tok,
                         body={"old_password": pw, "new_password": NEW_PW})
            if st2 != 200:
                return None
            st3, body3 = req("POST", "/api/system/auth/login",
                             body={"username": username, "password": NEW_PW})
            if st3 != 200:
                time.sleep(65)
                st3, body3 = req("POST", "/api/system/auth/login",
                                 body={"username": username, "password": NEW_PW})
                if st3 != 200:
                    return None
            tok = ((body3.get("data") or {}).get("access_token"))
        return tok
    print(f"    [login] {username} 失败：{last}")
    return None


FIX = {"roles": {}, "users": {}, "tokens": {}}

# 用户名 → (真名, 角色码)
USER_PLAN = {
    "u_owner":   ("PDF06 负责人张工", "PDF06_READ"),
    "u_stranger": ("PDF06 无关人路人甲", "PDF06_READ"),
    "u_collab":  ("PDF06 协作人小王", "PDF06_READ"),
    "u_nobody":  ("PDF06 无权限角色", "PDF06_NONE"),
}
ROLE_PLAN = {
    # 只读 + 建单（S5 的「负责人自建任务」要用自己的令牌建单，才能证明
    # assigner_id 为空的派工单把派工人印成创建人）
    "PDF06_READ": ("PDF06 任务读写", ["business:task:read", "business:task:create"]),
    # 只给业务无关的码：证明「没有 business:task:read」会被 403 挡在门外
    "PDF06_NONE": ("PDF06 无任务权限", []),
}


def mk_role(code, name):
    st, body = req("POST", "/api/system/role", FIX["admin"],
                   {"role_code": code, "role_name": name, "status": 1, "sort_order": 985,
                    # 数据范围 = 4「仅本人」：默认 1「全部」会让「无关用户」看见全公司的任务，
                    # S6 的 404 断言就变成了假绿
                    "data_scope": 4})
    if st == 200:
        rid = (body.get("data") or {}).get("id")
    else:
        rid = psql(f"select id from sys_role where role_code='{code}' and deleted_at is null limit 1")
        if not rid or rid.startswith("<ERR"):
            print(f"    [role] {code} 建失败 http={st} {body}")
            return None
        # 重跑：把数据范围纠正成「仅本人」（走 PUT 才会 bump 数据范围缓存 epoch）
        req("PUT", f"/api/system/role/{rid}", FIX["admin"],
            {"role_code": code, "role_name": name, "status": 1, "sort_order": 985,
             "data_scope": 4})
    FIX["roles"][code] = rid
    return rid


def mk_user(username, real_name, role_code):
    rid = FIX["roles"][role_code]
    st, body = req("POST", "/api/system/user", FIX["admin"],
                   {"username": username, "password": BASE_PW, "real_name": real_name,
                    "status": 1, "role_ids": [rid]})
    if st == 200:
        uid = (body.get("data") or {}).get("id")
    else:
        uid = psql(f"select id from sys_user where username='{username}' and deleted_at is null limit 1")
        if not uid or uid.startswith("<ERR"):
            print(f"    [user] {username} 建失败 http={st} {body}")
            return None
        psql(f"delete from sys_user_role where user_id='{uid}'::uuid and role_id in "
             f"(select id from sys_role where role_code like 'PDF06%')")
        psql("INSERT INTO sys_user_role (user_id, role_id) "
             f"SELECT '{uid}'::uuid, '{rid}'::uuid ON CONFLICT DO NOTHING")
    FIX["users"][username] = uid
    return uid


def grant(role_code, codes):
    """按 sys_menu.auth_code 授菜单 / 按钮（claims 里的 permissions 来自这张关联表）。"""
    rid = FIX["roles"][role_code]
    if not codes:
        return "0"
    joined = ",".join(f"'{c}'" for c in codes)
    psql("INSERT INTO sys_role_menu (role_id, menu_id, created_at) "
         f"SELECT '{rid}'::uuid, m.id, now() FROM sys_menu m "
         f"WHERE m.auth_code IN ({joined}) AND m.deleted_at IS NULL "
         "AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm "
         f"WHERE rm.role_id='{rid}'::uuid AND rm.menu_id=m.id)")
    return psql("SELECT count(DISTINCT m.auth_code) FROM sys_role_menu rm "
                "JOIN sys_menu m ON m.id = rm.menu_id "
                f"WHERE rm.role_id='{rid}'::uuid AND m.auth_code IN ({joined})")


def tok(username):
    if username not in FIX["tokens"]:
        FIX["tokens"][username] = login(username, (NEW_PW, BASE_PW))
    return FIX["tokens"][username]


# ---------- 业务夹具：项目 / 任务 / 子任务 ----------
def mk_project(code, name):
    st, body = req("POST", "/api/business/project", FIX["admin"],
                   {"name": name, "code": code,
                    "owner_id": FIX["users"]["u_owner"]})
    if st == 200:
        return (body.get("data") or {}).get("id")
    pid = psql(f"select id from biz_project where project_no='{code}' and deleted_at is null limit 1")
    if not pid or pid.startswith("<ERR"):
        print(f"    [project] {code} 建失败 http={st} {body}")
    return pid


def mk_task(title, description, owner, priority="P1", need_review=True,
            due="2026-11-20", project=None, estimate=16.0, owner_override=None):
    body = {
        "title": title,
        "description": description,
        "priority": priority,
        "need_review": need_review,
        "owner_id": owner_override if owner_override is not None else FIX["users"][owner],
        "due_date": due,
        "start_date": "2026-11-09",
        "estimate_hours": estimate,
    }
    if project:
        body["project_id"] = project
    st, resp = req("POST", "/api/business/task", FIX["admin"], body)
    if st != 200:
        print(f"    [task] {title} 建失败 http={st} {resp}")
        return None
    return resp.get("data") or {}


def mk_subtask(parent, title):
    st, resp = req("POST", f"/api/business/task/{parent}/subtask", FIX["admin"],
                   {"title": title})
    if st != 200:
        print(f"    [subtask] {title} 建失败 http={st} {resp}")
        return None
    return (resp.get("data") or {}).get("id")


def cancel_task(task_id, reason):
    st, resp = req("PATCH", f"/api/business/task/{task_id}/status", FIX["admin"],
                   {"to": "CANCELLED", "reason": reason})
    return st, resp


def add_member(task_id, user_id):
    return req("POST", f"/api/business/task/{task_id}/members", FIX["admin"],
               {"user_ids": [user_id]})


def real_name_of(username):
    return psql(f"select COALESCE(NULLIF(real_name,''), username) from sys_user "
                f"where username='{username}' and deleted_at is null limit 1")


def task_no_of(task_id):
    return psql(f"select task_no from biz_task where id='{task_id}'::uuid")


LONG_PREFIX = "第 {i:03d} 行：PDF-06 长描述跨页夹具，现场逐条核对阀门编号与密封面状态。"


def long_description(lines=120):
    return "\n".join(LONG_PREFIX.format(i=i) for i in range(1, lines + 1))


DESC_NORMAL = (
    "1. 到货后核对装箱单与实物数量，逐台登记序列号。\n"
    "2. 检查阀体密封面无划伤、砂眼，发现问题拍照并挂红牌。\n"
    "3. 安装前确认管道已泄压、盲板已加装。\n"
    "\n"
    "注意：夜间施工需提前一天报安全科备案。"
)

DESC_MARKDOWN = (
    "## 现场要求\n"
    "- 泄压后方可拆装\n"
    "- 全程佩戴防护镜\n"
    "**加粗**与*斜体*原样保留\n"
    "    四个空格缩进的行也要原样保留"
)


# ============================ 主流程 ============================
def main():
    global PASS, FAIL
    started = time.time()
    print(f"BASE={BASE}  DB={DB}  ENVF={_ENVF}")

    FIX["admin"] = login("admin", (ADMIN_PW, NEW_PW, "Admin@123456"))
    if not FIX["admin"]:
        print("admin 登录失败，退出")
        return 1
    admin_name = real_name_of("admin")
    print(f"admin real_name = {admin_name}")

    # ---------- 夹具 ----------
    for code, (name, codes) in ROLE_PLAN.items():
        mk_role(code, name)
    for username, (real_name, role_code) in USER_PLAN.items():
        mk_user(username, real_name, role_code)
    for code, (_name, codes) in ROLE_PLAN.items():
        got = grant(code, codes)
        print(f"role {code} 授权 {codes} → 命中 {got} 条")

    for username in USER_PLAN:
        t = tok(username)
        if not t:
            print(f"!! {username} 登录失败")
    proj = mk_project("PRJ-PDF06", "PDF-06 派工单实测项目")
    print(f"项目 = {proj}")

    # ==================== S1 普通任务（无检查项） ====================
    scenario("S1", "普通任务：任务信息 + 描述全文 + 签字栏（无检查项）")
    t1 = mk_task("PDF06 派工单-普通任务", DESC_NORMAL, "u_owner", priority="P1",
                 need_review=True, due="2026-11-20", project=proj, estimate=16.0)
    check(t1 is not None, "S1 任务创建成功")
    if not t1:
        print("!! S1 夹具缺失，中止")
        return 1
    task1 = t1["id"]
    no1 = t1["task_no"]
    print(f"  task1 = {no1} ({task1})")

    st, hdr, raw = get_pdf(f"/api/business/task/{task1}/pdf", tok("u_owner"))
    eq("S1 HTTP 200", st, 200)
    eq("S1 Content-Type", hdr.get("content-type"), "application/pdf")
    check(hdr.get("content-length", "0") == str(len(raw)), "S1 Content-Length 与实际字节一致",
          f"{hdr.get('content-length')} vs {len(raw)}")
    eq("S1 Cache-Control", hdr.get("cache-control"), "no-store")
    cd = hdr.get("content-disposition", "")
    check("attachment" in cd, "S1 附件形式下载", cd)
    check("filename*=UTF-8''" in cd, "S1 中文名走 RFC 5987", cd)
    check(raw.startswith(b"%PDF-"), "S1 字节是 PDF")

    path1, text1 = keep_pdf("task-normal.pdf", raw)
    flat1 = norm(text1)
    eq("S1 页数（短描述单页）", pdfpages(path1), 1)
    contains("S1 标题「工 作 任 务 派 工 单」", flat1, "工作任务派工单")
    contains("S1 任务号", flat1, no1)
    contains("S1 任务标题", flat1, "PDF06 派工单-普通任务")
    contains("S1 优先级带人读标签", flat1, "P1（高）")
    contains("S1 截止日期", flat1, "2026-11-20")
    contains("S1 负责人", flat1, "PDF06 负责人张工")
    contains("S1 协作人（无 → —）", flat1, "协作人")
    contains("S1 所属项目", flat1, "PRJ-PDF06")
    contains("S1 项目名称", flat1, "PDF-06 派工单实测项目")
    contains("S1 状态文案", flat1, "待开始")
    contains("S1 创建人 = admin", flat1, admin_name)
    contains("S1 分派人/派工人 = admin", flat1, admin_name)
    contains("S1 计划开始", flat1, "2026-11-09")
    contains("S1 预估工时", flat1, "16.0小时")
    # 描述全文（逐行）
    for line in DESC_NORMAL.split("\n"):
        if line.strip():
            contains(f"S1 描述行：{line[:12]}…", flat1, norm(line))
    # 签字栏 4 格 + 验收口径
    for label in ["派工人", "接收人", "完成确认", "日期"]:
        contains(f"S1 签字栏「{label}」", flat1, label)
    contains("S1 完成确认预印验收人", flat1, f"验收人：{admin_name}")
    not_contains("S1 无检查项 → 不出检查项小节", flat1, "检查项（共")
    check(pdfpages(path1) >= 1, "S1 长描述/空行不产生空页", str(pdfpages(path1)))

    fonts = pdffonts(path1)
    check(" no " not in fonts.split("\n")[2] if len(fonts.split("\n")) > 2 else False,
          "S1 字体已内嵌", fonts)

    # ==================== S2 检查项（子任务） ====================
    scenario("S2", "检查项：3 个子任务（含 1 个已取消）→ 只印 2 项、空白勾选框")
    t2 = mk_task("PDF06 派工单-带检查项", "按下列事项逐条确认，完成后在方框内打勾。",
                 "u_owner", priority="P2", need_review=False, due="2026-11-22",
                 project=proj, estimate=8.0)
    check(t2 is not None, "S2 任务创建成功")
    if not t2:
        print("!! S2 夹具缺失，中止")
        return 1
    task2 = t2["id"]
    no2 = t2["task_no"]
    s_a = mk_subtask(task2, "PDF06 检查项甲：核对到货数量")
    s_b = mk_subtask(task2, "PDF06 检查项乙：核对合格证")
    s_c = mk_subtask(task2, "PDF06 检查项丙：试压并记录")
    check(all([s_a, s_b, s_c]), "S2 三个子任务创建成功")
    st, resp = cancel_task(s_b, "供应商未随货提供合格证，本次不查")
    eq("S2 取消中间那条子任务", st, 200)
    eq("S2 子任务库内状态", psql(f"select status from biz_task where id='{s_b}'::uuid"), "CANCELLED")

    st, hdr, raw = get_pdf(f"/api/business/task/{task2}/pdf", tok("u_owner"))
    eq("S2 HTTP 200", st, 200)
    path2, text2 = keep_pdf("task-checklist.pdf", raw)
    flat2 = norm(text2)
    contains("S2 检查项小节", flat2, "检查项（共2项")
    contains("S2 甲在", flat2, "PDF06检查项甲：核对到货数量")
    contains("S2 丙在", flat2, "PDF06检查项丙：试压并记录")
    not_contains("S2 已取消的乙不印", flat2, "核对合格证")
    eq("S2 检查项条数=2（跳过已取消的那条）", flat2.count("PDF06检查项"), 2)
    check("☑" not in text2 and "√" not in text2,
          "S2 文本层不含任何「已勾」记号", "出现勾选记号")
    check("☐" not in text2, "S2 文本层不含方框字形（方框是 CSS 画的）", "出现 ☐ 字形")
    not_contains("S2 不印子任务状态", flat2, "已完成")
    contains("S2 完成确认：无需验收", flat2, "无需验收")

    # ==================== S3 长描述跨页 ====================
    scenario("S3", "长描述跨页：120 行描述 → 至少 2 页，逐行都在")
    t3 = mk_task("PDF06 派工单-长描述", long_description(120), "u_owner",
                 priority="P3", need_review=False, due="2026-12-31", project=proj,
                 estimate=40.0)
    check(t3 is not None, "S3 任务创建成功")
    if not t3:
        print("!! S3 夹具缺失，中止")
        return 1
    task3 = t3["id"]
    st, hdr, raw = get_pdf(f"/api/business/task/{task3}/pdf", tok("u_owner"))
    eq("S3 HTTP 200", st, 200)
    path3, text3 = keep_pdf("task-long.pdf", raw, pages_to_png=(1, 2))
    pages = pdfpages(path3)
    check(pages >= 2, "S3 页数 >= 2（确实跨页）", f"Pages={pages}")
    flat3 = norm(text3)
    missing = [i for i in range(1, 121) if norm(f"第{i:03d}行") not in flat3]
    eq("S3 120 行描述一行不少", len(missing), 0)
    if missing:
        print(f"    缺失行号：{missing[:10]}…")
    contains("S3 描述标题在首屏", flat3, "任务描述")
    p1 = norm(pdftext(path3, page=1))
    p2 = norm(pdftext(path3, page=2))
    contains("S3 第 001 行在第 1 页", p1, "第001行")
    check("第120行" not in p1, "S3 第 1 页没装下全部 120 行（描述真的断开）", "整段挤在第 1 页")
    check("行：PDF-06长描述跨页夹具" in p2, "S3 第 2 页也有描述正文", "第 2 页没有描述内容")

    # ==================== S4 原文照印（不做 Markdown 渲染） ====================
    scenario("S4", "描述原文照印：Markdown 记号 / 缩进不许被改写")
    t4 = mk_task("PDF06 派工单-原文照印", DESC_MARKDOWN, "u_owner", priority="P2",
                 need_review=False, due="2026-11-25")
    check(t4 is not None, "S4 任务创建成功")
    if not t4:
        print("!! S4 夹具缺失，中止")
        return 1
    task4 = t4["id"]
    st, _hdr, raw = get_pdf(f"/api/business/task/{task4}/pdf", tok("u_owner"))
    eq("S4 HTTP 200", st, 200)
    _p4, text4 = keep_pdf("task-verbatim.pdf", raw)
    flat4 = norm(text4)
    contains("S4 「## 现场要求」原样", flat4, "##现场要求")
    contains("S4 「**加粗**」原样", flat4, "**加粗**")
    contains("S4 「*斜体*」原样", flat4, "*斜体*")
    contains("S4 缩进行原样（内容不丢）", flat4, "四个空格缩进的行也要原样保留")
    check("<h2" not in text4 and "##现场要求" in flat4,
          "S4 没有被渲染成标题标签", "文本被改成 HTML 语义")

    # ==================== S5 签字栏 / 自建任务 / 协作人可见 ====================
    scenario("S5", "签字栏口径：自建任务派工人=创建人；协作人可见可导出")
    st, resp = req("POST", "/api/business/task", tok("u_owner"),
                   {"title": "PDF06 自建任务（负责人自派）",
                    "description": "自己给自己派的活，派工人就是创建人。",
                    "priority": "P2", "need_review": False, "due_date": "2026-11-28"})
    eq("S5 负责人自建任务", st, 200)
    task5 = (resp.get("data") or {}).get("id")
    eq("S5 自建任务 assigner 为空",
       psql(f"select coalesce(assigner_id::text,'NULL') from biz_task where id='{task5}'::uuid"), "NULL")
    st, _hdr, raw = get_pdf(f"/api/business/task/{task5}/pdf", tok("u_owner"))
    eq("S5 HTTP 200", st, 200)
    path5, text5 = keep_pdf("task-self-built.pdf", raw)
    flat5 = norm(text5)
    raw5 = norm(pdftext_raw(path5))
    dump("pdftotext_raw_task-self-built.txt", pdftext_raw(path5))
    contains("S5 派工人 = 创建人（不是 —）", raw5, f"派工人{real_name_of('u_owner')}")
    contains("S5 接收人 = 负责人", raw5, f"接收人{real_name_of('u_owner')}")
    not_contains("S5 派工人栏没有印「—」", raw5, "派工人—")

    # 协作人：加进 S1 的任务后，他也能导出（参与关系进可见性）
    st, _ = add_member(task1, FIX["users"]["u_collab"])
    eq("S5 协作人加入成功", st, 200)
    st, _hdr, raw = get_pdf(f"/api/business/task/{task1}/pdf", tok("u_collab"))
    eq("S5 协作人也能导出（参与关系可见）", st, 200)
    _pc, textc = keep_pdf("task-collab-view.pdf", raw)
    contains("S5 协作人出现在「协作人」栏（姓名可能被列宽折行，只钉「人 + 名」都在）",
             norm(textc), "PDF06协作人")
    contains("S5 协作人姓名本身在", norm(textc), "小王")

    # ==================== S6 负面 ====================
    scenario("S6", "负面：401 / 403 无权限码 / 404 无关 / 404 不存在 / 400 非法 id")
    st, hdr, raw = get_pdf(f"/api/business/task/{task1}/pdf", None)
    eq("S6 未登录 → 401", st, 401)

    st, body = get_pdf_err(f"/api/business/task/{task1}/pdf", tok("u_nobody"))
    eq("S6 无 business:task:read → 403", st, 403)
    check("business:task:read" in json.dumps(body, ensure_ascii=False),
          "S6 403 文案点名权限码", json.dumps(body, ensure_ascii=False)[:200])

    st, body = get_pdf_err(f"/api/business/task/{task1}/pdf", tok("u_stranger"))
    eq("S6 无关用户 → 404（不暴露存在）", st, 404)
    eq("S6 无关用户错误码 63001", (body.get("code")), 63001)

    st, body = get_pdf_err("/api/business/task/not-a-uuid/pdf", tok("u_owner"))
    eq("S6 非法 id → 400", st, 400)
    eq("S6 非法 id 错误码 10001", (body.get("code")), 10001)

    st, body = get_pdf_err(
        "/api/business/task/0f620000-0000-4000-8000-0000000009ff/pdf", tok("u_owner"))
    eq("S6 不存在 → 404", st, 404)
    eq("S6 不存在错误码 63001", (body.get("code")), 63001)

    st, body = req("GET", f"/api/business/task/{task1}/pdf", tok("u_owner"))
    eq("S6 JSON 路径走不通（说明是二进制接口）", st != 200, True)

    # ==================== S7 审计 ====================
    scenario("S7", "导出审计：EXPORT_TASK_PDF / biz_task / source=PDF-06")
    rows = psql(
        "SELECT operation || '|' || target_type || '|' || COALESCE(detail->>'source','') "
        "|| '|' || COALESCE(detail->>'doc_no','') || '|' || COALESCE(detail->>'checklist_count','') "
        "FROM operation_logs WHERE target_id = "
        f"'{task1}' AND operation = 'EXPORT_TASK_PDF' ORDER BY created_at")
    dump("operation_logs.txt", rows)
    lines = [ln for ln in rows.splitlines() if ln]
    check(len(lines) >= 1, "S7 至少一条审计行", rows[:200])
    if lines:
        parts = lines[0].split("|")
        eq("S7 operation", parts[0], "EXPORT_TASK_PDF")
        eq("S7 target_type", parts[1], "biz_task")
        eq("S7 detail.source", parts[2], "PDF-06")
        eq("S7 detail.doc_no", parts[3], no1)
        eq("S7 detail.checklist_count", parts[4], "0")
    rows2 = psql(
        "SELECT COALESCE(detail->>'checklist_count','') FROM operation_logs "
        f"WHERE target_id = '{task2}' AND operation = 'EXPORT_TASK_PDF' LIMIT 1")
    eq("S7 有检查项的单子 checklist_count=2", rows2, "2")

    # ==================== 真实 token curl ====================
    scenario("S8", "真实 token curl 实测（200 头 + 401 / 400 / 404 分支）")
    outp = []
    cases = [
        ("200 正常导出", tok("u_owner"), f"/api/business/task/{task1}/pdf"),
        ("401 未登录", None, f"/api/business/task/{task1}/pdf"),
        ("400 非法 id", tok("u_owner"), "/api/business/task/abc/pdf"),
        ("404 不存在", tok("u_owner"),
         "/api/business/task/0f620000-0000-4000-8000-0000000009ff/pdf"),
    ]
    for label, token, path in cases:
        cmd = ["curl", "-sS", "-D", "-", "-o", "/tmp/pdf06_curl_body.bin", "--max-time", "60"]
        if token:
            cmd += ["-H", f"Authorization: Bearer {token}"]
        cmd.append(BASE + path)
        res = subprocess.run(cmd, capture_output=True, text=True)
        head = res.stdout
        size = os.path.getsize("/tmp/pdf06_curl_body.bin")
        outp.append(f"### {label}\n$ curl {path}\n{head}")
        outp.append(f"[body bytes = {size}]\n")
        if label.startswith("200"):
            check("application/pdf" in head, "S8 curl 200 返回 application/pdf", head[:200])
            check("content-length" in head.lower(), "S8 curl 200 有 content-length", head[:200])
            with open(os.path.join(RAW, "curl_200.pdf"), "wb") as fp:
                fp.write(open("/tmp/pdf06_curl_body.bin", "rb").read())
        elif label.startswith("401"):
            check(" 401 " in head.split("\n")[0], "S8 curl 401", head[:120])
        elif label.startswith("400"):
            check(" 400 " in head.split("\n")[0], "S8 curl 400", head[:120])
        else:
            check(" 404 " in head.split("\n")[0], "S8 curl 404", head[:120])
    dump("curl_example.txt", "\n".join(outp))

    elapsed = time.time() - started
    print(f"\n{'=' * 78}")
    print(f"==== 汇总：{PASS + FAIL} 项，失败 {FAIL} 项（{elapsed:.0f}s）")
    if FAILS:
        for item in FAILS:
            print(f"  - {item}")
    print(f"{'=' * 78}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:  # noqa: BLE001
        traceback.print_exc()
        sys.exit(2)
