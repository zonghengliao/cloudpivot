#!/usr/bin/env python3
"""apply_api.py —— 走**接口路**建「最小权限自查账号」svc_selfcheck + 只读角色 OPS_SELFCHECK。

卡：t_882d48c8（替掉借用的真人账号 henry.liao）。与同目录 apply.sh（DBA 直写库）的区别：

* 本脚本**只调后端接口**（`POST /api/system/role`、`PUT /api/system/role/{id}/menus`、
  `POST /api/system/user`、`PUT /api/system/user/{id}/roles`），让后端的权限上限 guard
  （`guard::ensure_role_grant_allowed` / `ensure_menus_grantable` / `ensure_no_mutex`）
  自己校验，脚本不碰 `sys_*` 表、不自己算 argon2id；
* 需要一个 **SUPER_ADMIN 会话**：`NIK_ADMIN_PASSWORD` 从 `/etc/cloudpivot/cloudpivot.env`
  读（也可用 `--admin-password-file` 指另一个 600 文件）。**只登录一次**，失败即停
  （错 5 次锁 30 分钟、60 秒内 11 次 429）。

为什么账号必须是 `user_type='HUMAN'`：`backend/src/system/auth/handler.rs` 的
`ai_actor::login_forbidden` 禁止 ROBOT 走网页登录，ROBOT 还与 `sys_ai_agent` 有不变式。
接口建号不传 user_type → 落库走 `sys_user.user_type` 的默认值 `'HUMAN'`（V0102），
所以本脚本建完会**回读**确认它确实是 HUMAN。

幂等：角色/用户已存在时不重复建；已存在则把「菜单授权集」和「角色绑定集」重新对齐到
期望值（不是追加），跑第二遍的输出应与第一遍完全一致。

口令：`secrets.token_urlsafe(24)`（32 字符）。只写 `/root/svc_selfcheck.cred`（600）与
env 的 `CLOUDPIVOT_OPS_PASSWORD`；**不进日志、不进 argv、不打印**。

用法::

    python3 apply_api.py --dry-run                 # 只体检 + 打印计划，不写
    python3 apply_api.py --apply                   # 落库 + 写 env（幂等，可复跑）
    python3 apply_api.py --apply --no-env          # 只落库（隔离演练）
    python3 apply_api.py --apply --base-url http://127.0.0.1:8023

退出码：0 成功；1 失败；2 用法/环境问题；3 断言失败（有写动作没通过回读校验）。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any

ENV_FILE_DEFAULT = "/etc/cloudpivot/cloudpivot.env"
CRED_FILE_DEFAULT = "/root/svc_selfcheck.cred"

OPS_USER = "svc_selfcheck"
ROLE_CODE = "OPS_SELFCHECK"
ROLE_NAME = "运维自查（只读）"
ROLE_DESC = "自动化自查账号专用：只授列表/页面级查看权限，无任何 create/update/delete/export/approve"
ROLE_REMARK = "卡 t_882d48c8 / svc_selfcheck 专用，勿扩权"
ROLE_SORT = 90
REAL_NAME = "自动化自查账号（勿删/勿改密）"
USER_REMARK = (
    "卡 t_882d48c8：最小权限只读自查账号；口令见 /etc/cloudpivot/cloudpivot.env 的 "
    "CLOUDPIVOT_OPS_PASSWORD"
)

# 授权节点白名单 = 5 个只读页面 + 5 个分组目录（目录 auth_code 为空，本身不授权任何接口）。
# 命中数必须恰好 1，多一行就停手（拒绝猜授权目标）。
TARGET_PATHS = [
    ("/task", "business:task:read"),
    ("/task/list", "business:task:read"),
    ("/business/erp/sales-mgmt", ""),
    ("/business/erp/sales-order", "business:erp:sale-order:list"),
    ("/business/erp/finance-mgmt", ""),
    ("/business/erp/voucher", "business:erp:voucher:list"),
    ("/business/erp/master-data", ""),
    ("/business/erp/seq", "business:erp:seq:list"),
    ("/business/erp/warehouse-mgmt", ""),
    ("/business/erp/inventory-location", "business:erp:inventory-location:list"),
]

# 只读码闸门：白名单只认「读」结尾（与 apply.sh / verify_svc_selfcheck.sh 同一口径）
READONLY_SUFFIX = re.compile(r":(list|view|read|page|detail)$")

# 自查必需的只读码（缺任何一条，三个列表接口就会 403）
REQUIRED_CODES = [
    "business:task:read",
    "business:erp:sale-order:list",
    "business:erp:voucher:list",
]


def say(msg: str = "") -> None:
    print(msg, flush=True)


def ok(msg: str) -> None:
    say(f"  ✅ {msg}")


def bad(msg: str) -> None:
    say(f"  ✗ {msg}")


def warn(msg: str) -> None:
    say(f"  ⚠ {msg}")


# ---------------------------------------------------------------- env 文件


def env_get(env_file: str, key: str) -> str:
    """从 env 文件里取一个键（不做 shell 展开，只去引号与首尾空白）。"""
    try:
        with open(env_file, "r", encoding="utf-8") as fh:
            for line in fh:
                stripped = line.strip()
                if stripped.startswith("#") or "=" not in stripped:
                    continue
                k, v = stripped.split("=", 1)
                if k.strip() == key:
                    return v.strip().strip('"').strip("'")
    except OSError:
        return ""
    return ""


def env_write(env_file: str, pairs: dict[str, str]) -> str:
    """原子改写 env 里的若干键：先备份（600）、再原子替换、回读校验。返回备份路径。"""
    backup = f"{env_file}.bak-{time.strftime('%Y%m%d-%H%M%S')}"
    shutil.copy2(env_file, backup)
    os.chmod(backup, 0o600)

    with open(env_file, "r", encoding="utf-8") as fh:
        lines = fh.readlines()

    seen: set[str] = set()
    out: list[str] = []
    for line in lines:
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and "=" in stripped:
            key = stripped.split("=", 1)[0].strip()
            if key in pairs:
                out.append(f"{key}={pairs[key]}\n")
                seen.add(key)
                continue
        out.append(line)
    for key, value in pairs.items():
        if key not in seen:
            out.append(f"{key}={value}\n")

    tmp = env_file + ".tmp-svcc"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.writelines(out)
    os.chmod(tmp, 0o600)
    os.replace(tmp, env_file)
    os.chmod(env_file, 0o600)

    for key, value in pairs.items():
        if env_get(env_file, key) != value:
            raise RuntimeError(f"env 回读不一致：{key}")
    return backup


# ---------------------------------------------------------------- HTTP


class ApiError(RuntimeError):
    def __init__(self, method: str, path: str, status: int, body: object):
        super().__init__(f"{method} {path} -> http {status}: {str(body)[:300]}")
        self.status = status


def call(base: str, method: str, path: str, token: str | None = None,
         body: dict | None = None, timeout: int = 20) -> Any:
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode(errors="replace")
            payload = json.loads(raw or "{}")
            env = payload if isinstance(payload, dict) else {}
            if env.get("code") not in (0, None):
                raise ApiError(method, path, resp.status, payload)
            return env.get("data")
    except urllib.error.HTTPError as exc:
        raw = exc.read().decode(errors="replace")
        try:
            payload: object = json.loads(raw or "{}")
        except json.JSONDecodeError:
            payload = raw[:400]
        raise ApiError(method, path, exc.code, payload) from None


# ---------------------------------------------------------------- 数据


@dataclass
class Plan:
    admin_id: str = ""
    dept_id: str = ""
    menu_ids: list[str] = field(default_factory=list)
    role_id: str = ""
    role_created: bool = False
    user_id: str = ""
    user_created: bool = False


def flatten(tree: list, parent: str | None = None) -> list[tuple[str, str, str, str]]:
    rows: list[tuple[str, str, str, str]] = []
    for node in tree or []:
        path = node.get("path") or ""
        code = (node.get("authCode") or node.get("auth_code") or "").strip()
        rows.append((path, node.get("id") or "", code, node.get("title") or ""))
        rows.extend(flatten(node.get("children") or [], path))
    return rows


def resolve_menus(base: str, token: str) -> tuple[list[str], dict[str, str]]:
    tree = call(base, "GET", "/api/system/menu/tree", token=token) or []
    rows = flatten(tree)
    by_path: dict[str, list[tuple[str, str, str, str]]] = {}
    for row in rows:
        by_path.setdefault(row[0], []).append(row)

    ids: list[str] = []
    codes: dict[str, str] = {}
    valid = True
    for path, expected in TARGET_PATHS:
        hits = by_path.get(path, [])
        if len(hits) != 1:
            bad(f"菜单 path={path} 命中 {len(hits)} 行（要求恰好 1 行），拒绝猜授权目标")
            valid = False
            continue
        menu_id, code, title = hits[0][1], hits[0][2], hits[0][3]
        if code and not READONLY_SUFFIX.search(code):
            bad(f"菜单 path={path} 的权限码 {code} 不是只读码（白名单只认 :list/:view/:read/:page/:detail）")
            valid = False
            continue
        if code and expected and code != expected:
            warn(f"菜单 path={path} 权限码 = {code}（记录值 {expected}）—— 以库内为准，仍属只读")
        ids.append(menu_id)
        codes[path] = code
        say(f"     {path:<42} id={menu_id} code={code or '(目录)'} title={title}")
    if not valid:
        raise RuntimeError("菜单解析未通过（见上），未做任何写操作")
    return ids, codes


def find_role(base: str, token: str) -> dict | None:
    roles = call(base, "GET", "/api/system/role/list", token=token) or []
    for role in roles:
        if role.get("role_code") == ROLE_CODE:
            return role
    return None


def find_user(base: str, token: str, username: str) -> dict | None:
    page = call(base, "GET", f"/api/system/user/page?page=1&page_size=20&username={username}", token=token) or {}
    for item in page.get("list") or []:
        if item.get("username") == username:
            return item
    return None


def load_or_create_cred(cred_file: str, rotate: bool) -> tuple[str, bool]:
    """返回 (口令, 是否新生成)。已有凭据文件就沿用（改密有专门流程，见 README §7）。"""
    if os.path.exists(cred_file) and not rotate:
        perm = oct(os.stat(cred_file).st_mode)[-3:]
        if perm != "600":
            raise RuntimeError(f"凭据文件权限是 {perm}（要求 600）")
        password = ""
        for line in open(cred_file, "r", encoding="utf-8"):
            if line.startswith("SVCC_PASSWORD="):
                password = line.split("=", 1)[1].strip().strip('"').strip("'")
        if not password:
            raise RuntimeError(f"{cred_file} 里没有 SVCC_PASSWORD")
        if len(password) < 24:
            raise RuntimeError(f"凭据文件里的口令只有 {len(password)} 位（要求 ≥24）")
        ok(f"沿用已有凭据文件（口令长度 {len(password)}）")
        return password, False
    password = secrets.token_urlsafe(24)
    if len(password) < 24:
        raise RuntimeError("生成的口令长度不足 24")
    tmp = cred_file + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write("# svc_selfcheck 凭据（600，勿提交、勿打印）。改密流程见 scripts/ops/svc_selfcheck/README.md\n")
        fh.write(f"SVCC_USER={OPS_USER}\n")
        fh.write(f"SVCC_PASSWORD={password}\n")
    os.chmod(tmp, 0o600)
    os.replace(tmp, cred_file)
    ok(f"生成新口令（长度 {len(password)}，token_urlsafe(24)）→ {cred_file}（600）")
    return password, True


# ---------------------------------------------------------------- 主流程


def main() -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:8023")
    parser.add_argument("--env-file", default=ENV_FILE_DEFAULT)
    parser.add_argument("--cred-file", default=CRED_FILE_DEFAULT)
    parser.add_argument("--admin-password-file", default="",
                        help="从该文件第一行读 admin 口令（缺省从 --env-file 的 NIK_ADMIN_PASSWORD）")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true", help="只体检 + 打印计划，不写")
    mode.add_argument("--apply", action="store_true", help="落库 + 写 env")
    parser.add_argument("--no-env", action="store_true", help="只落库，不改 env")
    parser.add_argument("--rotate", action="store_true",
                        help="重新生成口令（需配合 --apply；接口路改密会把 must_change_password 置 true，"
                             "故本脚本拒绝，见 README §7）")
    parser.add_argument("--allow-admin-session", action="store_true",
                        help="显式确认「已获人工授权使用 SUPER_ADMIN 会话」—— --apply 必填（或 SVCC_AUTHORIZED=1）。"
                             "本脚本是唯一需要 admin 会话的建号脚本，防止被当成日常工具误跑")
    args = parser.parse_args()

    # 授权闸门：只有明确确认过才允许真正的写操作（建号要 SUPER_ADMIN 会话，属一次性动作）
    if args.apply and not (args.allow_admin_session or os.environ.get("SVCC_AUTHORIZED") == "1"):
        bad("缺少授权标记：--apply 需要 --allow-admin-session（或 SVCC_AUTHORIZED=1）")
        say("     本脚本要用 admin 的 SUPER_ADMIN 会话建号，属一次性动作；"
            "未获人工书面授权时请勿在生产执行")
        return 2

    base = args.base_url.rstrip("/")
    say("════ svc_selfcheck 接口路落地 ════")
    say(f"  base={base}  env={args.env_file}  cred={args.cred_file}")
    say(f"  模式={'dry-run（不写）' if args.dry_run else 'apply（写库' + ('，不写 env' if args.no_env else ' + 写 env') + '）'}")

    if args.rotate:
        bad("接口路不支持改密：管理员 PATCH /api/system/user/{id}/password 会把 must_change_password 置 true，"
            "而本卡要求 false")
        say("     改密请用 DBA 路：bash scripts/ops/svc_selfcheck/apply.sh --rotate（同事务写库 + 写 env）")
        return 2

    # ① admin 口令
    if args.admin_password_file:
        with open(args.admin_password_file, "r", encoding="utf-8") as fh:
            admin_pw = fh.readline().strip()
        source = args.admin_password_file
    else:
        admin_pw = env_get(args.env_file, "NIK_ADMIN_PASSWORD")
        source = f"{args.env_file}:NIK_ADMIN_PASSWORD"
    if not admin_pw:
        bad(f"拿不到 admin 口令（{source}）")
        return 2

    # ② 登录（单次，不重试）
    say(f"== 1. admin 登录（口令来源 {source}，长度 {len(admin_pw)}，只试一次）")
    try:
        data = call(base, "POST", "/api/system/auth/login",
                    body={"username": "admin", "password": admin_pw}) or {}
    except ApiError as exc:
        bad(f"登录失败 http={exc.status}：{exc}")
        say("     401 = 口令不对（别重试，5 次锁 30 分钟）；429 = 限流等 60 秒；403 = 账号被锁/停用")
        return 1
    token = data.get("access_token") or ""
    if not token:
        bad("登录 200 但没有 access_token")
        return 1
    ok(f"登录 200（token {len(token)} 字符）")

    plan = Plan()

    # ③ admin 自己（拿 id + 部门：svc_selfcheck 与 admin 同部门，不硬编码 uuid）
    say("== 2. 解析 admin 的 dept_id（svc_selfcheck 与它同部门）")
    admin_row = find_user(base, token, "admin")
    if not admin_row:
        bad("接口里查不到 admin 账号，停手")
        return 1
    plan.admin_id = admin_row.get("id") or ""
    plan.dept_id = admin_row.get("dept_id") or ""
    if not plan.dept_id:
        bad("admin 没有 dept_id：先按 docs/待决策.md 的口径落位，本脚本不猜部门")
        return 1
    ok(f"admin id={plan.admin_id} dept_id={plan.dept_id}")

    # ④ 菜单节点
    say("== 3. 解析授权节点（10 个：5 个只读页 + 5 个分组目录）")
    try:
        plan.menu_ids, codes = resolve_menus(base, token)
    except (RuntimeError, ApiError) as exc:
        bad(str(exc))
        return 3
    missing = [code for code in REQUIRED_CODES if code not in codes.values()]
    if missing:
        bad(f"必需的只读码缺 {len(missing)} 条：{', '.join(missing)}")
        return 3
    ok(f"授权节点 {len(plan.menu_ids)} 个，必需只读码齐全（{', '.join(REQUIRED_CODES)}）")

    # ⑤ 角色现状
    say("== 4. 角色 OPS_SELFCHECK")
    role = find_role(base, token)
    if role:
        plan.role_id = role.get("id") or ""
        ok(f"已存在 id={plan.role_id} data_scope={role.get('data_scope')} status={role.get('status')}")
        if role.get("data_scope") != 1 or role.get("status") != 1:
            warn("现有角色的 data_scope/status 不是 1/1（只读现状不自动改，改请用 PUT /api/system/role/{id}）")
    else:
        say("     不存在 → 将 POST /api/system/role")

    # ⑥ 用户现状
    say("== 5. 账号 svc_selfcheck")
    user = find_user(base, token, OPS_USER)
    if user:
        plan.user_id = user.get("id") or ""
        ok(f"已存在 id={plan.user_id} status={user.get('status')} locked={user.get('is_locked')} "
           f"dept={user.get('dept_id')}")
    else:
        say("     不存在 → 将 POST /api/system/user（role_ids 带 OPS_SELFCHECK）")

    if args.dry_run:
        say("== dry-run：不做任何写操作。将要做的写动作：")
        if not role:
            say("     POST /api/system/role                （建 OPS_SELFCHECK，data_scope=1）")
        say(f"     PUT  /api/system/role/{{id}}/menus      （授权 {len(plan.menu_ids)} 个只读节点）")
        if not user:
            say("     POST /api/system/user                （建 svc_selfcheck，HUMAN，must_change_password=false）")
        else:
            say("     PUT  /api/system/user/{id}/roles     （把角色对齐到 OPS_SELFCHECK）")
        if not args.no_env:
            say(f"     写 {args.env_file} 的 CLOUDPIVOT_OPS_USER={OPS_USER} / CLOUDPIVOT_OPS_PASSWORD")
        return 0

    # ⑦ 口令（先准备，避免落库成功却没有可用口令）
    say("== 6. 口令")
    try:
        password, _ = load_or_create_cred(args.cred_file, rotate=False)
    except (RuntimeError, OSError) as exc:
        bad(str(exc))
        return 2

    # ⑧ 建角色
    if not role:
        try:
            created = call(base, "POST", "/api/system/role", token=token, body={
                "role_code": ROLE_CODE,
                "role_name": ROLE_NAME,
                "description": ROLE_DESC,
                "status": 1,
                "data_scope": 1,
                "sort_order": ROLE_SORT,
                "remark": ROLE_REMARK,
            }) or {}
        except ApiError as exc:
            bad(f"建角色失败：{exc}")
            return 1
        plan.role_id = created.get("id") or ""
        plan.role_created = True
        ok(f"建角色成功 id={plan.role_id}（后端 guard 已放行：SUPER_ADMIN 会话）")
    if not plan.role_id:
        bad("拿不到角色 id，停手")
        return 1

    # ⑨ 授权（整批对齐，不是追加）
    say("== 7. 授权节点（PUT /menus 整批对齐）")
    try:
        call(base, "PUT", f"/api/system/role/{plan.role_id}/menus", token=token,
             body={"menu_ids": plan.menu_ids})
        saved = call(base, "GET", f"/api/system/role/{plan.role_id}/menus", token=token) or []
    except ApiError as exc:
        bad(f"授权失败：{exc}")
        return 1
    saved_set = {str(x) for x in saved}
    if saved_set != set(plan.menu_ids):
        bad(f"回读授权集与期望不一致：期望 {len(plan.menu_ids)} 个，实到 {len(saved_set)} 个")
        return 3
    ok(f"授权 {len(saved_set)} 个节点，回读一致")

    # ⑩ 建/对齐账号
    if not user:
        say("== 8. 建账号（HUMAN / status=1 / must_change_password=false / 与 admin 同部门）")
        try:
            created = call(base, "POST", "/api/system/user", token=token, body={
                "username": OPS_USER,
                "password": password,
                "real_name": REAL_NAME,
                "dept_id": plan.dept_id,
                "status": 1,
                "remark": USER_REMARK,
                "role_ids": [plan.role_id],
            }) or {}
        except ApiError as exc:
            bad(f"建账号失败：{exc}")
            return 1
        plan.user_id = created.get("id") or ""
        plan.user_created = True
        ok(f"建账号成功 id={plan.user_id}")
    else:
        say("== 8. 账号已存在：把角色对齐到 OPS_SELFCHECK（不多不少）")
    if not plan.user_id:
        bad("拿不到账号 id，停手")
        return 1

    try:
        call(base, "PUT", f"/api/system/user/{plan.user_id}/roles", token=token,
             body={"role_ids": [plan.role_id]})
        bound = call(base, "GET", f"/api/system/user/{plan.user_id}/roles", token=token) or []
    except ApiError as exc:
        bad(f"绑定角色失败：{exc}")
        return 1
    if {str(x) for x in bound} != {plan.role_id}:
        bad(f"回读角色绑定不一致：{bound}（要求恰好 [{plan.role_id}]）")
        return 3
    ok("角色绑定回读一致（恰好 1 个 = OPS_SELFCHECK）")

    # ⑪ 回读账号属性（user_type 必须 HUMAN —— ROBOT 不能网页登录）
    detail = call(base, "GET", f"/api/system/user/{plan.user_id}", token=token) or {}
    user_type = detail.get("user_type") or ""
    # 注意：UserDetailRes 不下发 must_change_password（只有登录响应里有），
    # 故这里只回读接口能给的字段；must_change_password=false 由登录响应 + 库侧验收兜底。
    flags = {
        "user_type": user_type,
        "status": detail.get("status"),
        "is_locked": detail.get("is_locked"),
        "dept_id": detail.get("dept_id"),
    }
    say(f"     回读：{json.dumps(flags, ensure_ascii=False)}")
    if user_type != "HUMAN":
        bad(f"user_type={user_type!r}（要求 HUMAN；ROBOT 禁止网页登录）")
        return 3
    if flags["status"] != 1 or flags["is_locked"] not in (False, None):
        bad("账号状态不对（要求 status=1 / is_locked=false）")
        return 3
    if flags["dept_id"] != plan.dept_id:
        warn(f"dept_id={flags['dept_id']} 与 admin({plan.dept_id}) 不一致")

    # ⑫ 写 env
    say("== 9. env")
    if args.no_env:
        warn("--no-env：跳过（隔离演练）")
    else:
        try:
            backup = env_write(args.env_file, {
                "CLOUDPIVOT_OPS_USER": OPS_USER,
                "CLOUDPIVOT_OPS_PASSWORD": password,
            })
        except (RuntimeError, OSError) as exc:
            bad(f"写 env 失败：{exc}")
            return 1
        ok(f"env 已更新（备份 {backup}，600，回读一致；口令长度 {len(password)}）")

    say("════ 完成 ════")
    say("  下一步验收：")
    say(f"    bash /root/svcchk-run/verify_svc_selfcheck.sh --base-url {base}")
    say("    bash /root/prod_selfcheck.sh")
    say("  回滚：")
    say("    账号/角色：POST /api/system/user/{id} DELETE + /api/system/role/{id} DELETE（或 DBA 路 apply.sh --down）")
    say(f"    env：还原 {args.env_file}.bak-*")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
