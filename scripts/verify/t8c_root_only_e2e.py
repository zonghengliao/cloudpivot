#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""T8-C 接口层验收（真 HTTP，非 mock）：`root_only` 的 list / total 口径 + 向后兼容。

用法（先起一个隔离实例指向**有数据的副本库**，不要打生产）：

```bash
T8C_BASE=http://127.0.0.1:18085 T8C_USER=laoliu T8C_PASS=*** \\
  python3 scripts/verify/t8c_root_only_e2e.py
```

断言口径（16 条）：默认 43 条 → `root_only=true` 28 条且全是顶层、`total` 与 `list` 同口径、
父任务的 child_count 之和 = 15（子任务一条不少，只是靠懒加载取）、分页无空白尾页、
keyword / status 都在顶层集合里筛、`root_only` 非法值 400（不静默当 false）。
"""
import json
import os
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("T8C_BASE", "http://127.0.0.1:18085")
USER = os.environ.get("T8C_USER", "laoliu")
PASS = os.environ.get("T8C_PASS", "")

ok = 0
bad = 0


def check(label, cond, detail=""):
    global ok, bad
    if cond:
        ok += 1
        print(f"[PASS] {label}" + (f" :: {detail}" if detail else ""))
    else:
        bad += 1
        print(f"[FAIL] {label}" + (f" :: {detail}" if detail else ""))


def http(method, path, body=None, token=None):
    url = BASE + path
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except Exception:
            return err.code, {"raw": raw}


if not PASS:
    raise SystemExit("必须通过 T8C_PASS 传入口令（不要写死在任何文件里）")

# ---------- 登录 ----------
status, res = http("POST", "/api/system/auth/login", {"username": USER, "password": PASS})
check("登录", status == 200 and res.get("code") == 0, f"HTTP {status} code={res.get('code')} msg={res.get('msg')}")
token = (res.get("data") or {}).get("access_token")
check("拿到 access_token", bool(token), f"len={len(token or '')}")
if not token:
    raise SystemExit(1)

PAGE = "/api/business/task/page"


def page(**params):
    q = {"page": 1, "page_size": 50}
    q.update({k: v for k, v in params.items() if v is not None})
    status, res = http("GET", f"{PAGE}?{urllib.parse.urlencode(q)}", token=token)
    assert status == 200 and res.get("code") == 0, (status, res)
    return res["data"]


# ---------- ① 默认（不传）仍然是全部 43 条 ----------
all_default = page()
check(
    "不传 root_only → total 仍是全部任务（向后兼容）",
    all_default["total"] == 43,
    f"total={all_default['total']} page_size=50 list={len(all_default['list'])}",
)
children_in_default = [row for row in all_default["list"] if row.get("parent_id")]
check(
    "不传 root_only → 列表里仍有子任务（parent_id 非空）",
    len(children_in_default) == 15,
    f"子任务 {len(children_in_default)} 条 / 15",
)

# ---------- ② root_only=false 与不传一致 ----------
explicit_false = page(root_only="false")
check(
    "root_only=false → 与不传完全一致（total 43）",
    explicit_false["total"] == all_default["total"] == 43,
    f"total={explicit_false['total']}",
)

# ---------- ③ root_only=true → 只有顶层，total 同口径 ----------
roots = page(root_only="true")
check(
    "root_only=true → total 变成顶层数 28",
    roots["total"] == 28,
    f"total={roots['total']}（43 - 15 子）",
)
check(
    "root_only=true → 列表里没有任何子任务",
    all(not row.get("parent_id") for row in roots["list"]),
    f"list={len(roots['list'])} 条，parent_id 全空",
)
check(
    "root_only=true → total 与 list 同口径（page_size 足够时不出现空白页）",
    roots["total"] == len(roots["list"]),
    f"total={roots['total']} list={len(roots['list'])}",
)
parents_with_children = [row for row in roots["list"] if row["child_count"] > 0]
check(
    "顶层里 5 个父任务的 child_count > 0（前端据此画可点的箭头）",
    len(parents_with_children) == 5,
    "child_count: " + str(sorted(row["child_count"] for row in parents_with_children)),
)
check(
    "父任务的 child_count 之和 = 15（子任务一条不少，只是靠懒加载取）",
    sum(row["child_count"] for row in parents_with_children) == 15,
    f"sum={sum(row['child_count'] for row in parents_with_children)}",
)

# ---------- ④ 分页：page_size=10 时第 1 页就是 10 条顶层、共 3 页 ----------
p1 = page(root_only="true", page_size=10)
check(
    "root_only + page_size=10 → 第 1 页 10 条顶层，total 28",
    len(p1["list"]) == 10 and p1["total"] == 28,
    f"list={len(p1['list'])} total={p1['total']}",
)
p3 = page(root_only="true", page_size=10, page=3)
check(
    "root_only + 第 3 页仍有数据（8 条），没有空白尾页",
    len(p3["list"]) == 8,
    f"page=3 list={len(p3['list'])}",
)

# ---------- ⑤ 组合筛选仍在顶层口径内 ----------
kw = roots["list"][0]["title"][:4]
kroot = page(root_only="true", keyword=kw)
check(
    f"root_only + keyword('{kw}') → 结果全部是顶层",
    all(not row.get("parent_id") for row in kroot["list"]) and kroot["total"] >= 1,
    f"total={kroot['total']}",
)
todo_roots = page(root_only="true", status="TODO")
todo_all = page(status="TODO")
check(
    "root_only + status=TODO 的 total ≤ 不带的 total（在顶层里筛）",
    0 <= todo_roots["total"] <= todo_all["total"],
    f"root_only={todo_roots['total']} / 全部={todo_all['total']}",
)

# ---------- ⑥ 其它视图走的接口不受影响（同一接口不带参数就好） ----------
todo = page(status="TODO")["total"]
doing = page(status="DOING")["total"]
check(
    "看板/甘特/燃尽图不传 root_only 时行为不变（按状态取数仍有数据）",
    todo + doing >= 1,
    f"TODO={todo} DOING={doing}",
)

# ---------- ⑦ 非法值不静默 ----------
status, res = http("GET", f"{PAGE}?root_only=notabool", token=token)
check(
    "root_only 传非法值 → 4xx（不静默当 false）",
    status in (400, 422),
    f"HTTP {status} code={res.get('code')} msg={str(res.get('msg'))[:80]}",
)

print(f"\n=== 汇总：{ok} PASS / {bad} FAIL ===")
raise SystemExit(1 if bad else 0)
