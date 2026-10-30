#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""云枢（CloudPivot）验证脚本公共模块。

提供：登录 / HTTP 请求 / 断言 / DB 查询 / 清理登记 / **字段契约构造器**。

# 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `BASE_URL` | `http://127.0.0.1:8023` | 被测实例地址 |
| `TEST_USER` | `laoliu_test` | 登录用户名（需超管或全权限） |
| `TEST_PASS` | `Test@12345` | 登录密码 |
| `DB_ENV_FILE` | `/etc/cloudpivot/cloudpivot.env` | 从服务 env 文件读 `ALUN_DATABASE_*` |
| `DB_HOST` / `DB_PORT` / `DB_NAME` / `DB_USER` / `DB_PASS` | 取 env 文件 | 直连库校验用，逐项覆盖 |
| `DB_ENABLED` | `1` | 置 `0` 跳过所有 DB 断言（无 psql 时用） |

# 字段契约（踩过的坑，全部固化在这里，用例只准用构造函数）

1. **流程图**：`nodes[].key`（不是 `node_key`）、`edges[].key` **必填**、
   边用 `from` / `to`（不是 `source` / `target`）、节点类型字段是 `type`。
2. **审批人**：`assignees: [{type, ref_id}]`（`type` 不是 `kind`，`ref_id` 不是 `id`）。
3. **公告**：正文是 `content_md`（不是 `content`）。
4. **通知**：正文是 `content`（和公告**相反**，别记混）。
5. **帮助分类**：必填 `code`；**帮助文档**：必填 `slug` + `content_md`。
6. **变更日志**：库表 `sys_change_log` 的列是 `resource_id`（不是 `target_id`）；
   但 `GET /business/changelog/timeline` 的 **query 参数**仍叫 `target_id`——
   一个在库、一个在 API，两处名字不同，别互相套。
7. **文件删除**：`DELETE /api/business/storage/{file_id}`（不是 `/storage/file/{id}`）。
8. **通知已读**：`PUT /api/business/notice/read-all`，**body 必须是 None**；
   单条已读是 `PUT /api/business/notice/read/{receiver_id}`（收件人 id，不是通知 id）。
9. **导出**：`POST /business/impexp/export` 用 `biz_type`（不是 `code` / `def_code`）。
10. **列级脱敏**：`column_name`（不是 `field_name`）、`mask_type` 是 **int 1..7**
    （不是 `"EMAIL"` 字符串）、`reason` **必填**、角色是单个 `role_id`（不是 `role_ids`）。
11. **角色互斥**：`role_a_id` / `role_b_id` / `reason`（`reason` 必填）。
"""

import json
import os
import subprocess
import time
import urllib.error
import urllib.request
import uuid

# ---------------------------------------------------------------- 环境

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:8023").rstrip("/")
TEST_USER = os.environ.get("TEST_USER", "laoliu_test")
TEST_PASS = os.environ.get("TEST_PASS", "Test@12345")

DB_ENV_FILE = os.environ.get("DB_ENV_FILE", "/etc/cloudpivot/cloudpivot.env")
DB_ENABLED = os.environ.get("DB_ENABLED", "1") not in ("0", "false", "no")


def _read_env_file(path):
    """读 systemd EnvironmentFile 风格的键值（容忍引号、注释、空行）。"""
    out = {}
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                key, _, val = line.partition("=")
                out[key.strip()] = val.strip().strip('"').strip("'")
    except OSError:
        pass
    return out


_SVC_ENV = _read_env_file(DB_ENV_FILE)

DB_HOST = os.environ.get("DB_HOST") or _SVC_ENV.get("ALUN_DATABASE_HOST", "127.0.0.1")
DB_PORT = os.environ.get("DB_PORT") or _SVC_ENV.get("ALUN_DATABASE_PORT", "5432")
DB_NAME = os.environ.get("DB_NAME") or _SVC_ENV.get("ALUN_DATABASE_NAME", "cloudpivot")
DB_USER = os.environ.get("DB_USER") or _SVC_ENV.get("ALUN_DATABASE_USER", "cloudpivot")
DB_PASS = os.environ.get("DB_PASS") or _SVC_ENV.get("ALUN_DATABASE_PASSWORD", "")

STAMP = time.strftime("%m%d%H%M%S")

# ---------------------------------------------------------------- 字段契约构造器


def assignee(ref_id, kind="USER", sort_order=None):
    """审批人引用：`{type, ref_id}`（`type` 不是 `kind`）。"""
    item = {"type": kind, "ref_id": ref_id}
    if sort_order is not None:
        item["sort_order"] = sort_order
    return item


def node(key, node_type, name=None, x=100, y=200, **extra):
    """流程节点：key/name/type + 可选项。"""
    item = {"key": key, "name": name or key, "type": node_type, "x": x, "y": y}
    item.update(extra)
    return item


def edge(key, src, dst, **extra):
    """连线：`key` 必填，端点用 `from` / `to`。"""
    item = {"key": key, "from": src, "to": dst}
    item.update(extra)
    return item


def graph(nodes, edges):
    """整图：`{nodes, edges}`。edges 缺 key 直接报错，别让服务端兜。"""
    for item in edges:
        if not item.get("key"):
            raise ValueError("edges[].key 必填：%r" % (item,))
        if "from" not in item or "to" not in item:
            raise ValueError("edges 必须用 from/to：%r" % (item,))
    for item in nodes:
        if "node_key" in item:
            raise ValueError("nodes 用 key，不是 node_key：%r" % (item,))
    return {"nodes": nodes, "edges": edges}


def linear_graph(mid_type="APPROVAL", mid_key="n1", mid_name="主管审批", assignees=None,
                 **mid_extra):
    """START → 审批节点 → END 三段式（最常用）。"""
    mid = node(mid_key, mid_type, mid_name, x=300, **mid_extra)
    if assignees is not None:
        mid["assignee_type"] = "USER"
        mid["assignees"] = assignees
    return graph(
        [node("s", "START", "开始", x=100), mid, node("e", "END", "结束", x=500)],
        [edge("e1", "s", mid_key), edge("e2", mid_key, "e")],
    )


# ---------------------------------------------------------------- 主控对象


class Ctx:
    """一次验证运行的上下文：计数器、清理登记、连接信息。"""

    def __init__(self, base_url=None, user=None, password=None, label=None):
        self.base_url = (base_url or BASE_URL).rstrip("/")
        self.user = user or TEST_USER
        self.password = password or TEST_PASS
        self.label = label or "verify"
        self.token = None
        self.me = None
        self.passed = 0
        self.failed = 0
        self.skipped = 0
        self.fails = []
        self._cleanups = []

    # ---------- 断言 ----------

    def chk(self, name, cond, detail=""):
        """断言一条。detail 只在失败时打印，最多 110 字。"""
        if cond:
            self.passed += 1
            print("  \u2705 " + name)
        else:
            self.failed += 1
            self.fails.append("%s | %s" % (name, str(detail)[:110]))
            print("  \u274c %s  %s" % (name, str(detail)[:110]))
        return bool(cond)

    def ok(self, name, resp, expect_code=0):
        """断言响应 `code == expect_code`。"""
        code = resp.get("code") if isinstance(resp, dict) else None
        return self.chk(
            name,
            code == expect_code,
            "code=%s msg=%s" % (code, str((resp or {}).get("msg"))[:80] if isinstance(resp, dict) else resp),
        )

    def skip(self, reason):
        self.skipped += 1
        print("  \u23f8 跳过：" + reason)

    # ---------- HTTP ----------

    def req(self, method, path, body=None, raw=False, ctype=None, token=None,
            timeout=30):
        """发请求。返回 (status, payload)；`raw=True` 返回文本。"""
        data = None
        if body is not None:
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
        req = urllib.request.Request(self.base_url + path, data=data, method=method)
        req.add_header("Content-Type", ctype or "application/json")
        auth = token if token is not None else self.token
        if auth:
            req.add_header("Authorization", "Bearer " + auth)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                text = resp.read().decode(errors="replace")
                return resp.status, (text if raw else _json(text))
        except urllib.error.HTTPError as exc:
            text = exc.read().decode(errors="replace")
            return exc.code, (text if raw else _json(text))
        except Exception as exc:  # noqa: BLE001 — 网络异常不该炸掉整轮
            return 0, {"code": -1, "msg": "请求异常: %s" % exc}

    def get(self, path, **kw):
        return self.req("GET", path, **kw)

    def post(self, path, body=None, **kw):
        return self.req("POST", path, body if body is not None else {}, **kw)

    def put(self, path, body=None, **kw):
        return self.req("PUT", path, body, **kw)

    def patch(self, path, body=None, **kw):
        return self.req("PATCH", path, body, **kw)

    def delete(self, path, **kw):
        return self.req("DELETE", path, **kw)

    def data(self, resp):
        """取 data 字段（dict 或 list），没有就返回 {}。"""
        if not isinstance(resp, dict):
            return {}
        val = resp.get("data")
        return val if val is not None else {}

    def page(self, path):
        """分页接口：返回 (ok, list, total)。顺带断言分页字段完整。"""
        st, resp = self.get(path)
        data = self.data(resp)
        ok = st == 200 and isinstance(resp, dict) and resp.get("code") == 0
        if not ok:
            return False, [], 0
        has_shape = all(k in data for k in ("list", "total", "page", "page_size"))
        return has_shape, data.get("list") or [], data.get("total") or 0

    # ---------- DB ----------

    def q(self, sql):
        """执行 SQL 返回首行首列（`-tAc`）。DB 不可用时返回空串。"""
        if not DB_ENABLED:
            return ""
        env = dict(os.environ)
        env["PGPASSWORD"] = DB_PASS
        try:
            proc = subprocess.run(
                ["psql", "-h", DB_HOST, "-p", str(DB_PORT), "-U", DB_USER,
                 "-d", DB_NAME, "-tAc", sql],
                capture_output=True, text=True, env=env, timeout=30,
            )
        except (OSError, subprocess.SubprocessError):
            return ""
        return (proc.stdout or "").strip()

    def q_rows(self, sql):
        """执行 SQL 返回行列表（每行按 `|` 切开）。"""
        raw = self.q("select string_agg(r, E'\\n') from (%s) t(r)" % sql)
        return [line.split("|") for line in raw.splitlines() if line]

    def db_ok(self):
        """DB 是否可用。"""
        return self.q("select 1") == "1"

    # ---------- 清理 ----------

    def on_cleanup(self, desc, fn):
        """登记一个清理动作（逆序执行，单条失败不影响其余）。"""
        self._cleanups.append((desc, fn))
        return fn

    def cleanup(self):
        """跑全部清理动作，返回 (执行数, 失败描述列表)。"""
        if not self._cleanups:
            print("\n【清理】无登记项")
            return 0, []
        print("\n【清理】")
        errs = []
        done = 0
        for desc, fn in reversed(self._cleanups):
            try:
                res = fn()
                done += 1
                # 幂等清理：404（本来就没了）和 code=0 都算正常，只有 5xx / 真错误才告警
                bad = False
                if isinstance(res, tuple) and res:
                    status = res[0]
                    payload = res[1] if len(res) > 1 else None
                    code = payload.get("code") if isinstance(payload, dict) else None
                    if isinstance(status, int) and status >= 500:
                        bad = True
                    elif code not in (0, None) and status not in (404, 400):
                        bad = True
                print("  %s %s" % ("\u26a0" if bad else "\u2713", desc))
            except Exception as exc:  # noqa: BLE001
                errs.append("%s: %s" % (desc, exc))
                print("  \u26a0 %s 失败：%s" % (desc, exc))
        self._cleanups.clear()
        print("  共 %d 项" % done)
        return done, errs

    # ---------- 登录 ----------

    def login(self):
        st, resp = self.req("POST", "/api/system/auth/login",
                            {"username": self.user, "password": self.password})
        token = (self.data(resp) or {}).get("access_token") if isinstance(self.data(resp), dict) else None
        if not token:
            self.chk("登录 %s" % self.user, False, "http=%s resp=%s" % (st, str(resp)[:120]))
            return False
        self.token = token
        self.me = self.q("select id::text from sys_user where username='%s'" % _sql_escape(self.user))
        return True


def _json(text):
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        return {"raw": text[:300]}


def _sql_escape(value):
    return str(value).replace("'", "''")


def new_id():
    return uuid.uuid4().hex


# ---------------------------------------------------------------- 业务小工具（踩坑固化）


def create_flow_def(ctx, flow_code, flow_name="验证流程", biz_type="OTHER", **extra):
    """新建流程定义，返回 (did, vid)。失败时 (None, None)。"""
    body = {"flow_code": flow_code, "flow_name": flow_name, "biz_type": biz_type}
    body.update(extra)
    st, resp = ctx.post("/api/business/flow/def", body)  # noqa: F841
    data = ctx.data(resp)
    did = data.get("id") if isinstance(data, dict) else None
    vid = data.get("version_id") if isinstance(data, dict) else None
    if did and not vid:
        vid = draft_version(ctx, did)
    return did, vid


def draft_version(ctx, did):
    """取 DRAFT 版本 id（草稿态下 def 详情的 current_version_id 为 null）。"""
    st, resp = ctx.get("/api/business/flow/def/%s/versions" % did)  # noqa: F841
    items = (ctx.data(resp) or {}).get("list") or [] if isinstance(ctx.data(resp), dict) else []
    for ver in items:
        if ver.get("version_status") == "DRAFT":
            return ver.get("id")
    return items[0].get("id") if items else None


def upload_file(ctx, content=b"cloudpivot verify", filename="verify.txt",
                category="task", path="/api/business/storage/upload"):
    """multipart 上传，返回 (status, resp, file_id)。"""
    boundary = "----verify" + uuid.uuid4().hex
    body = b""
    body += ("--" + boundary + "\r\n").encode()
    body += ('Content-Disposition: form-data; name="category"\r\n\r\n%s\r\n' % category).encode()
    body += ("--" + boundary + "\r\n").encode()
    body += ('Content-Disposition: form-data; name="file"; filename="%s"\r\n' % filename).encode()
    body += b"Content-Type: text/plain\r\n\r\n" + content + b"\r\n"
    body += ("--" + boundary + "--\r\n").encode()
    st, resp = ctx.post(path, body, ctype="multipart/form-data; boundary=" + boundary)
    data = ctx.data(resp)
    fid = data.get("id") or data.get("file_id") if isinstance(data, dict) else None
    return st, resp, fid


def delete_file(ctx, fid):
    """删文件：`DELETE /api/business/storage/{file_id}`。"""
    return ctx.delete("/api/business/storage/%s" % fid)


def notice_read_all(ctx):
    """全部已读：`PUT /api/business/notice/read-all`，**body 必须 None**。"""
    return ctx.put("/api/business/notice/read-all", None)


def changelog_count(ctx, resource, resource_id):
    """变更日志条数：库列是 `resource_id`（不是 target_id）。"""
    rows = ctx.q(
        "select count(*) from sys_change_log where resource='%s' and resource_id='%s'"
        % (_sql_escape(resource), _sql_escape(resource_id))
    )
    return int(rows) if rows.isdigit() else 0


def ensure_roles(ctx, count=2, prefix="VRF"):
    """保证有 count 个非超管角色可改；不够就用 API 建临时角色。返回 role_id 列表。"""
    sql = ("select id||'|'||coalesce(role_code,'') from sys_role where deleted_at is null "
           "and role_code not like 'SUPER%' order by role_code limit " + str(int(count)))
    rows = ctx.q_rows(sql)
    roles = [(r[0], r[1]) for r in rows if len(r) >= 2]
    created = []
    idx = 0
    while len(roles) < count:
        idx += 1
        code = "%s_%s_%d" % (prefix, STAMP, idx)
        st, resp = ctx.post("/api/system/role", {
            "role_code": code, "role_name": "验证临时角色 %d" % idx,
            "description": "verify 临时角色，跑完即删", "status": 1, "data_scope": 1,
        })
        rid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
        if not rid:
            break
        created.append(rid)
        roles.append((rid, code))

        def _del(role_id=rid):
            return ctx.delete("/api/system/role/%s" % role_id)

        ctx.on_cleanup("删临时角色 %s" % code, _del)
    return [rid for rid, _ in roles], created


def sweep(ctx, since_ts, verbose=True):
    """跑完扫尾：清掉本次运行窗口内产生的「活垃圾」。

    覆盖三类 API 删不干净的残留（都是真实踩到的）：
    1. **任务/流程联动留下的在跑审批单** —— 任务进 REVIEW 会拉起默认流程 task_accept，
       任务软删后审批单还挂在 PENDING（流程定义也就 409 删不掉）；
    2. **孤儿 AI 任务令牌** —— 派活建的 `biz_task_ai_token` 在任务删除后不会跟着删；
    3. **待办类通知** —— 审批/分派产生的 TODO 通知。

    只动「本次运行开始之后创建」的行，不碰别人的数据；DB 不可用时直接跳过。
    """
    if not ctx.db_ok():
        if verbose:
            print("\n【扫尾】DB 不可用，跳过")
        return 0
    touched = 0
    sqls = [
        ("撤销遗留流程实例",
         "update biz_flow_instance set status='CANCELLED', ended_at=coalesce(ended_at, now()) "
         "where ended_at is null and created_at >= '%s'::timestamptz" % since_ts),
        ("取消遗留待办",
         "update biz_flow_task set status='CANCELLED' "
         "where status='PENDING' and instance_id in (select id from biz_flow_instance "
         "where ended_at is not null and status='CANCELLED' and created_at >= '%s'::timestamptz)" % since_ts),
        ("清孤儿 AI 任务令牌",
         "delete from biz_task_ai_token t where not exists "
         "(select 1 from biz_task x where x.id=t.task_id and x.deleted_at is null)"),
        ("软删运行期 TODO 通知",
         "update sys_notice set deleted_at=now() "
         "where deleted_at is null and notice_type='TODO' and created_at >= '%s'::timestamptz" % since_ts),
    ]
    if verbose:
        print("\n【扫尾】清理本次运行窗口内的残留")
    for desc, sql in sqls:
        out = ctx.q(sql + " returning 1")
        n = len([line for line in out.splitlines() if line.strip()]) if out else 0
        touched += n
        if verbose:
            print("  ✓ %s：%d 行" % (desc, n))
    return touched


def report(ctx, title=None):
    """打印汇总并返回进程退出码（0 全通过 / 1 有 FAIL）。"""
    width = 66
    print("\n" + "=" * width)
    if title:
        print(title)
    print("PASS=%d  FAIL=%d  SKIP=%d" % (ctx.passed, ctx.failed, ctx.skipped))
    if ctx.fails:
        print("失败项：")
        for item in ctx.fails:
            print("  - " + item)
    print("=" * width)
    return 0 if ctx.failed == 0 else 1
