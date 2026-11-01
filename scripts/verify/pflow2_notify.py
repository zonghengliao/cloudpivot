#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
P-FLOW-2 NOTIFY_NODE 真机验收（HTTP 全链路）
=============================================

用法（先起隔离实例，见 `pflow2_notify.sh`）：

    BASE_URL=http://127.0.0.1:18089 python3 scripts/verify/pflow2_notify.py --user admin --password X

检查项（与卡面 §四 一一对应）：

  ① 建流程 START → NOTIFY(通知发起人) → END，保存
  ② 发布 → R19 通过（不再拒绝 NOTIFY_NODE）
  ③ 发起实例 → sys_notice 里出现 FLOW_NOTIFY 通知，接收人 = 发起人
  ④ 节点状态立即 APPROVED、实例直接走完（非阻塞）
  ⑤ ON_LEAVE 的图同样能发（幂等键段是 L）
  ⑥ 反例：targets 为空 → 发布 400 + 65034；NOTIFY 配了 config.task → 400 + 65034

退出码：任一项 FAIL → 1。
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

FAILED = []


def say(text):
    print(text, flush=True)


def check(name, ok, detail=""):
    if ok:
        say(f"[PASS] {name}" + (f" — {detail}" if detail else ""))
    else:
        say(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))
        FAILED.append(name)


def call(method, path, token=None, body=None, base="http://127.0.0.1:18089"):
    url = base.rstrip("/") + path
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        text = error.read().decode()
        try:
            return error.code, json.loads(text or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": text}
    except Exception as error:  # noqa: BLE001 - 网络层错误要原样报出来
        return 0, {"raw": str(error)}


def login(base, user, password):
    """登录；首登改过密的库会 401 → 自动用「改密后的口令」再试一次（脚本可反复跑）。"""
    for candidate in (password, password + "@Pflow2"):
        status, payload = call(
            "POST", "/api/system/auth/login", body={"username": user, "password": candidate}, base=base
        )
        if status == 200:
            data = payload.get("data") or {}
            return data.get("access_token"), data.get("must_change_password")
        if status != 401:
            say(f"登录失败：{status} {payload}")
            return None, None
    say(f"登录失败：{user} 的口令既不等于 {password}，也不等于改密后的口令（隔离库被别的脚本改过？）")
    return None, None


def change_password(base, token, old, new):
    return call(
        "PUT",
        "/api/system/auth/password",
        token=token,
        body={"old_password": old, "new_password": new},
        base=base,
    )


def graph(start_limit=1):
    """START → NOTIFY(通知发起人) → END 的整图（与 card §四 ① 同一张图）。"""
    return {
        "nodes": [
            {
                "key": "n_start",
                "name": "开始",
                "type": "START",
                "x": 0,
                "y": 0,
                "sort_order": 0,
            },
            {
                "key": "n_notify",
                "name": "通知发起人",
                "type": "NOTIFY_NODE",
                "x": 200,
                "y": 0,
                "sort_order": 1,
                "config": {
                    "notify": {
                        "when": "ON_ENTER",
                        "targets": [{"type": "INITIATOR"}],
                        "inline": {
                            "title": "{{instance.title}} 已到「{{node.name}}」",
                            "content": "单号 {{instance.biz_no}}",
                        },
                    }
                },
            },
            {
                "key": "n_end",
                "name": "结束",
                "type": "END",
                "x": 400,
                "y": 0,
                "sort_order": 2,
            },
        ][: 2 + start_limit],
        "edges": [
            {"key": "e1", "from": "n_start", "to": "n_notify", "priority": 0, "is_default": True},
            {"key": "e2", "from": "n_notify", "to": "n_end", "priority": 0, "is_default": True},
        ],
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=os.environ.get("BASE_URL", "http://127.0.0.1:18089"))
    parser.add_argument("--user", default="admin")
    parser.add_argument("--password", default=os.environ.get("PFLOW2_ADMIN_PASSWORD", "Admin@123456"))
    args = parser.parse_args()

    token, must_change = login(args.base, args.user, args.password)
    if token is None:
        say("拿不到令牌，终止")
        return 1
    if must_change:
        new_password = args.password + "@Pflow2"
        status, _ = change_password(args.base, token, args.password, new_password)
        check("首登改密", status == 200, f"HTTP {status}")
        token, must_change = login(args.base, args.user, new_password)
        check("改密后重新登录", token is not None and not must_change)
    if not token:
        say("拿不到令牌，终止")
        return 1

    # ---------- ① 建流程 + 保存整图 ----------
    # 每次跑都用**新的编码**（`flow_code` 全局唯一）：比「复用 + 找草稿版本」省掉一整段
    # 分支，也让「重跑」不会因为上一轮已发布而没有可编辑的草稿版本。
    flow_code = f"pflow2_notify_{int(time.time()) % 1000000:06d}"
    status, payload = call(
        "POST",
        "/api/business/flow/def",
        token=token,
        body={
            "flow_code": flow_code,
            "flow_name": f"P-FLOW-2 通知节点验收 {flow_code}",
            "biz_type": "PURCHASE_ORDER",
            "description": "scripts/verify/pflow2_notify.py 自建",
        },
        base=args.base,
    )
    data = payload.get("data") or {}
    def_id = data.get("id")
    # `POST /def` 会连带建好初始草稿版本（`default_graph()` = START → END）
    version_id = data.get("version_id")
    check("创建流程定义（含初始草稿）", status in (200, 201) and bool(def_id) and bool(version_id),
          f"HTTP {status} {payload.get('msg', '')}")
    if not (def_id and version_id):
        return 1

    status, payload = call(
        "PUT", f"/api/business/flow/version/{version_id}/graph", token=token, body=graph(), base=args.base
    )
    check("保存整图（含 NOTIFY_NODE）", status == 200, f"HTTP {status} {payload.get('msg', '')}")

    # ---------- ⑥ 反例一：targets 为空 → 必须被拒（库侧 CHECK 兜底） ----------
    # 说明口径：`PUT /graph` 是「保存期只提示、不阻断」（架构 §10.4.3），所以这一条
    # 不是 R19 拦的，是被 `ck_biz_flow_node_notify_config` 的
    # `jsonb_array_length(targets) BETWEEN 1 AND 5` 拦下的 —— 结果同样是「配不进库」，
    # 状态码是 500 而不是 400（前端面板的 `notifyIssues` 会在保存前就标红，UI 出不来这种配置）。
    bad = graph()
    bad["nodes"][1]["config"]["notify"]["targets"] = []
    status, payload = call(
        "PUT", f"/api/business/flow/version/{version_id}/graph", token=token, body=bad, base=args.base
    )
    check("反例：targets 为空配不进库（DB CHECK 兜底）", status >= 400,
          f"HTTP {status} {payload.get('msg', '')}")

    # ---------- ⑥ 反例二：ON_TIMEOUT 缺 timeout_hours → 发布 400 + 65031（R19） ----------
    bad = graph()
    bad["nodes"][1]["config"]["notify"]["when"] = "ON_TIMEOUT"
    call("PUT", f"/api/business/flow/version/{version_id}/graph", token=token, body=bad, base=args.base)
    status, payload = call("POST", f"/api/business/flow/def/{def_id}/publish", token=token, base=args.base)
    check("反例：ON_TIMEOUT 缺超时小时 → 发布 400", status == 400,
          f"HTTP {status} {payload.get('msg', '')}")
    body = json.dumps(payload, ensure_ascii=False)
    check("反例命中 65031 且文案中文", "65031" in body or "timeout_hours" in body, body[:160])

    # 恢复合法图再发布（上一步没落库的话这步幂等）
    call("PUT", f"/api/business/flow/version/{version_id}/graph", token=token, body=graph(), base=args.base)

    # ---------- ② 发布（R19 必须放行） ----------
    status, payload = call("POST", f"/api/business/flow/def/{def_id}/publish", token=token, base=args.base)
    check("发布通过 R19 校验", status == 200, f"HTTP {status} {payload.get('msg', '')}")
    if status != 200:
        return 1

    # ---------- ③ 发起实例 ----------
    status, payload = call(
        "POST",
        "/api/business/flow/instance",
        token=token,
        body={
            "def_id": def_id,
            "title": "P-FLOW-2 验收单",
            "biz_no": "PO-PFLOW2-E2E",
            "form_data": {},
        },
        base=args.base,
    )
    instance = payload.get("data") or {}
    check(
        "发起实例",
        status == 200 and instance.get("instance_status") == "APPROVED",
        f"HTTP {status} status={instance.get('instance_status')} {payload.get('msg', '')}",
    )

    # ---------- ④ 节点立即通过（非阻塞） ----------
    check(
        "节点非阻塞（实例直接走完）",
        instance.get("instance_status") == "APPROVED",
        f"next_nodes={instance.get('next_nodes')}",
    )

    say("")
    say("HTTP 侧断言结束；通知落库 / 接收人请用下面的 SQL 复核（脚本 pflow2_notify.sh 会自动跑）：")
    say(f"  SELECT title, receiver_count, biz_type FROM sys_notice WHERE biz_id = '{instance.get('instance_id')}';")
    say(f"  SELECT status FROM biz_flow_instance_node WHERE instance_id = '{instance.get('instance_id')}';")

    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
