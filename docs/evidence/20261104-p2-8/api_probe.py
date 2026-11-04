#!/usr/bin/env python3
"""P2-8 只读接口探针（不写任何数据）。

目的：回答「0 成员」到底是**能力缺失**还是**没人用**——
      ① 登录 admin（SUPER_ADMIN）；
      ② GET /api/business/project/page       → 有几项目、每项 member_count 与工时汇总；
      ③ GET /api/business/project/{id}       → 详情是否下发 members 数组 / stats；
      ④ GET /api/business/project/{id}/members → 成员接口是否 200 且返回 []。
只做 GET，不 POST/DELETE，因此对库零副作用。

用法：python3 docs/evidence/20261104-p2-8/api_probe.py
"""
import json
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8023"
USER = "admin"
PWD = "Admin@123456"  # 仓库内置默认账号（AGENTS.md），非生产口令


def call(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode()[:300]


def payload(res):
    """兼容 {code,data,...} 与裸对象两种壳。"""
    if isinstance(res, dict) and "data" in res:
        return res["data"]
    return res


def main():
    status, res = call("POST", "/api/system/auth/login", body={"username": USER, "password": PWD})
    print(f"login: http={status} resp={str(res)[:200]}")
    data = payload(res)
    if not isinstance(data, dict):
        print("SKIP: 响应不是对象壳（登录失败）——本机 dev 库 admin 口令不是仓库默认值，"
              "按「不猜凭据」原则停在这里；0 成员问题改用 SQL 复算接口口径（见 sql_recompute.sql）")
        return
    token = data.get("access_token") or data.get("token")
    if not token:
        print("SKIP: 拿不到 token；返回体键 =", list(data)[:10])
        return
    print(f"token: len={len(token)}")

    status, res = call("GET", "/api/business/project/page?page=1&page_size=20", token)
    body = payload(res) or {}
    rows = body.get("list") or body.get("records") or []
    print(f"page: http={status} total={body.get('total')} items={len(rows)}")
    for r in rows:
        print(
            "  | {code} | {name} | status={status} | members={mc} | tasks={tt} | done={td} "
            "| rate={cr} | 工时 estimate/actual 由详情给".format(
                code=r.get("code"), name=r.get("name"), status=r.get("status"),
                mc=r.get("member_count"), tt=r.get("task_total"),
                td=r.get("task_done"), cr=r.get("completion_rate"),
            )
        )

    for r in rows[:1]:
        pid = r.get("id")
        status, res = call("GET", f"/api/business/project/{pid}", token)
        d = payload(res) or {}
        stats = d.get("stats") or {}
        print(f"detail: http={status} keys={sorted(d.keys())[:8]}")
        print(f"  members 数组长度 = {len(d.get('members') or [])}")
        print(f"  stats = {json.dumps(stats, ensure_ascii=False)}")
        status, res = call("GET", f"/api/business/project/{pid}/members", token)
        m = payload(res)
        print(f"members 接口: http={status} 返回 {len(m or [])} 条")


if __name__ == "__main__":
    main()
