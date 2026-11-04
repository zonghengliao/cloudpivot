#!/usr/bin/env python3
"""t_ce73ae26 的 HTTP 验收：MATERIAL_TYPE 映射能不能落库 / 命中 / 可读报错。

跑法（实例已在 127.0.0.1:18162，库 cloudpivot_ce73ae26）：
    python3 e2e_mapping.py

断言清单（任一失败 → 退出码 1）：
 1. 登录成功（首登强制改密则先改密再登）
 2. 初始映射列表为空
 3. POST MATERIAL_TYPE + ref_code=RAW（不给 ref_id）→ 200 落库   ← 修复前必然 400
 4. 落库行 ref_id 为 NULL、ref_code 归一化成 RAW
 5. 同部门同类型再来一条 → 409 + 可读文案（不是 23505 裸错）
 6. 同部门 'raw'（小写）→ 409（唯一性大小写不敏感）
 7. 同部门另一种类型 SEMI → 200（唯一性按编码而不是 map_type）
 8. 另一个部门同类型 RAW → 200（部门进唯一键）
 9. MATERIAL_TYPE 带 ref_id → 400 且文案点名 ref_id
10. MATERIAL_TYPE + 值域外编码 BOGUS → 400 且文案点名 ref_code
11. DEFAULT 第一条 200；同部门第二条 DEFAULT → 409（新索引把兜底也钉成唯一）
12. PUT 改科目 / 改部门 → 200，且改到「已有同目标」的部门 → 409 可读
13. 软删（DELETE）后同一目标可重建 → 200
"""

import json
import os
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18162"
DEPT_A = "00000000-0000-0000-0000-0000ce73ae26"
DEPT_B = "00000000-0000-0000-0000-0000ce73ae27"
# 5602.02 = 管理费用-差旅费（V0226 种子，末级）；5602.03 用不到，仅作改科目的对照
ACC_TRAVEL = None

results = []


def call(method, path, token=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode() or "{}"
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def check(name, ok, detail=""):
    results.append({"case": name, "ok": bool(ok), "detail": detail})
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else "  >>> " + detail))


def login():
    status, body = call("POST", "/api/system/auth/login",
                        body={"username": "admin", "password": "Admin@123456"})
    if status == 200 and body.get("code") == 0:
        data = body.get("data") or {}
        token = data.get("token") or data.get("access_token")
        if data.get("must_change_password"):
            # 首登强制改密：改密接口本身要认证；改完旧 token 失效，必须用新口令重登
            status2, body2 = call("PUT", "/api/system/auth/password", token=token,
                                  body={"old_password": "Admin@123456",
                                        "new_password": "Admin@654321"})
            if status2 != 200 or body2.get("code") != 0:
                raise SystemExit("改密失败: %s %s" % (status2, body2))
            status, body = call("POST", "/api/system/auth/login",
                                body={"username": "admin", "password": "Admin@654321"})
            data = body.get("data") or {}
            token = data.get("token") or data.get("access_token")
        return token, data
    # 可能上一轮已经改过密
    status, body = call("POST", "/api/system/auth/login",
                        body={"username": "admin", "password": "Admin@654321"})
    if status != 200 or body.get("code") != 0:
        raise SystemExit("登录失败: %s %s" % (status, body))
    data = body.get("data") or {}
    return data.get("token") or data.get("access_token"), data


def page_rows(token):
    status, body = call("GET", "/api/business/erp/budget/mapping/page?page=1&page_size=50",
                        token=token)
    assert status == 200 and body.get("code") == 0, (status, body)
    data = body.get("data") or {}
    return data.get("list") or data.get("records") or [], data


def main():
    token, me = login()
    check("1. 登录成功（拿得到 token）", bool(token), str(me)[:200])

    # 科目 id：末级科目 5602.02（差旅费）/ 5602.01（折旧费），由 psql 取好后经环境变量传入
    # （`/api/business/erp/account/page` 在本环境返回空列表，属另一张卡的排查面，这里不纠缠）
    acc = os.environ.get("ACCOUNT_ID", "")
    acc_other = os.environ.get("ACCOUNT_OTHER_ID", "")
    check("1-b. 取到末级科目 id（5602.02 / 5602.01）", bool(acc) and bool(acc_other),
          "acc=%s other=%s" % (acc, acc_other))

    # 幂等起点：把本用例上一次跑剩下的映射（本卡专用库，只有这些）软删干净
    rows, _ = page_rows(token)
    for stale in rows:
        call("DELETE", "/api/business/erp/budget/mapping/" + str(stale.get("id")), token=token)
    rows, _ = page_rows(token)
    check("2. 初始映射列表为空", len(rows) == 0, "rows=%d" % len(rows))

    # ③ 核心用例：只给 ref_code 就能落库
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_id": None, "ref_code": "RAW",
        "account_id": acc, "dept_id": DEPT_A, "priority": 5, "status": 1,
        "remark": "e2e MATERIAL_TYPE 全公司原材料",
    })
    ok = status == 200 and body.get("code") == 0
    check("3. POST MATERIAL_TYPE+ref_code=RAW（不给 ref_id）→ 200", ok, "%s %s" % (status, body))
    raw_id = (body.get("data") or {}).get("id") if ok else None

    rows, _ = page_rows(token)
    row = next((r for r in rows if r.get("id") == raw_id), None)
    check("4. 列表能查到该映射", row is not None, "rows=%d" % len(rows))
    if row:
        check("4-b. 落库 ref_id = NULL", row.get("ref_id") is None, json.dumps(row, ensure_ascii=False))
        check("4-c. 落库 ref_code = RAW（归一化大写）", row.get("ref_code") == "RAW",
              json.dumps(row, ensure_ascii=False))
        check("4-d. map_type = MATERIAL_TYPE", row.get("map_type") == "MATERIAL_TYPE", str(row.get("map_type")))

    # ⑤ 同部门同类型重复 → 可读 409
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_code": "RAW", "account_id": acc, "dept_id": DEPT_A,
    })
    msg = str(body.get("msg", ""))
    check("5. 同部门同类型第二条 → 409 可读文案",
          status == 409 and "映射" in msg and "23505" not in msg and "duplicate key" not in msg,
          "%s %s" % (status, body))

    # ⑥ 小写编码也撞同一条唯一键
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_code": "raw", "account_id": acc, "dept_id": DEPT_A,
    })
    check("6. 同部门 'raw'（小写）→ 409（唯一性大小写不敏感）", status == 409,
          "%s %s" % (status, body))

    # ⑦ 另一种物料类型可以共存
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_code": "SEMI", "account_id": acc, "dept_id": DEPT_A,
    })
    semi_id = (body.get("data") or {}).get("id")
    check("7. 同部门另一种类型 SEMI → 200（唯一性按编码）", status == 200 and body.get("code") == 0,
          "%s %s" % (status, body))

    # ⑧ 另一个部门同类型
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_code": "RAW", "account_id": acc, "dept_id": DEPT_B,
    })
    raw_b_id = (body.get("data") or {}).get("id")
    check("8. 另一个部门同类型 RAW → 200（部门进唯一键）", status == 200 and body.get("code") == 0,
          "%s %s" % (status, body))

    # ⑨ MATERIAL_TYPE 带 ref_id → 400 点名 ref_id
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_id": "00000000-0000-0000-0000-0000000000aa",
        "ref_code": "OFFICE", "account_id": acc,
    })
    check("9. MATERIAL_TYPE 带 ref_id → 400 且点名 ref_id",
          status == 400 and "ref_id" in str(body.get("msg", "")), "%s %s" % (status, body))

    # ⑩ 值域外编码 → 400 点名 ref_code
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "MATERIAL_TYPE", "ref_code": "BOGUS", "account_id": acc,
    })
    check("10. 值域外编码 BOGUS → 400 且点名 ref_code",
          status == 400 and "ref_code" in str(body.get("msg", "")), "%s %s" % (status, body))

    # ⑪ DEFAULT 唯一（新索引顺带堵住旧洞）
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "DEFAULT", "account_id": acc, "dept_id": DEPT_A,
    })
    default_id = (body.get("data") or {}).get("id")
    check("11. DEFAULT 第一条 → 200", status == 200 and body.get("code") == 0, "%s %s" % (status, body))
    status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
        "map_type": "DEFAULT", "account_id": acc, "dept_id": DEPT_A,
    })
    check("11-b. 同部门第二条 DEFAULT → 409（兜底唯一）", status == 409, "%s %s" % (status, body))

    # ⑫ PUT：改科目 / 改部门；改到已有同目标的部门必须 409 可读
    if acc_other and raw_id:
        status, body = call("PUT", "/api/business/erp/budget/mapping/" + raw_id, token=token, body={
            "map_type": "MATERIAL_TYPE", "ref_code": "RAW", "account_id": acc_other,
            "dept_id": DEPT_A, "priority": 5, "status": 1,
        })
        check("12. PUT 改预算科目 → 200", status == 200 and body.get("code") == 0,
              "%s %s" % (status, body))
        status, body = call("PUT", "/api/business/erp/budget/mapping/" + raw_id, token=token, body={
            "map_type": "MATERIAL_TYPE", "ref_code": "RAW", "account_id": acc_other,
            "dept_id": DEPT_B, "priority": 5, "status": 1,
        })
        check("12-b. PUT 改到「已有同目标」的部门 → 409 可读",
              status == 409 and "duplicate key" not in str(body.get("msg", "")),
              "%s %s" % (status, body))

    # ⑬ 软删后可重建
    if raw_id:
        status, body = call("DELETE", "/api/business/erp/budget/mapping/" + raw_id, token=token)
        check("13. DELETE（软删）→ 200", status == 200 and body.get("code") == 0,
              "%s %s" % (status, body))
        status, body = call("POST", "/api/business/erp/budget/mapping", token=token, body={
            "map_type": "MATERIAL_TYPE", "ref_code": "RAW", "account_id": acc, "dept_id": DEPT_A,
        })
        check("13-b. 软删后同一目标可重建 → 200", status == 200 and body.get("code") == 0,
              "%s %s" % (status, body))
        rebuilt_id = (body.get("data") or {}).get("id")
    else:
        rebuilt_id = None

    rows, _ = page_rows(token)
    final = sorted(
        [(r.get("map_type"), r.get("ref_code") or "-", r.get("dept_id") or "ALL") for r in rows]
    )
    # 可见行 = 重建的 RAW-A + SEMI-A + RAW-B + DEFAULT-A（被软删的那条不再出现）
    check("14. 终态可见映射 4 条（重建 RAW-A / SEMI-A / RAW-B / DEFAULT-A）", len(rows) == 4,
          json.dumps(final, ensure_ascii=False))

    with open("/root/.hermes/profiles/backend-developer/cache/scratch/t_ce73ae26/e2e_report.json", "w") as fh:
        json.dump({"results": results, "ids": {
            "raw": raw_id, "semi": semi_id, "raw_b": raw_b_id,
            "default": default_id, "rebuilt": rebuilt_id,
            "account": acc, "account_other": acc_other,
        }, "final_rows": final}, fh, ensure_ascii=False, indent=2)

    failed = [r for r in results if not r["ok"]]
    print("\n== %d passed / %d failed ==" % (len(results) - len(failed), len(failed)))
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
