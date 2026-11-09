#!/usr/bin/env python3
"""E2E-14 系统评审：1 万行导入/导出性能取证（145 / cloudpivot_dev）。

只造 E2E14BIG 前缀自有数据（字典类型 + 1 万条字典项 + 导入任务 + 1 个上传文件），
跑完由 `e2e14_review_perf_cleanup.sh` 按前缀清理；不 DROP / TRUNCATE、不动他人数据。

用法（145 上）：
    cd /opt/cloudpivot/scripts/verify && python3 e2e14_review_perf.py
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e13_lib import (ENV, data_of, login, make_xlsx, multipart, req,  # noqa: E402
                       req_bytes)

CODE = "E2E14BIG"
HEADERS = ["字典编码*", "字典项值*", "字典项名称*", "描述", "排序号", "是否默认", "状态"]
N = int(os.environ.get("N", "10000"))


def timed(label, fn):
    t0 = time.perf_counter()
    out = fn()
    dt = (time.perf_counter() - t0) * 1000
    print(f"  {label:<30}{dt:>9.0f} ms")
    return out, dt


def main():
    print(f"== E2E-14 大数据取证：{N} 行字典项 ==")
    admin = login("admin", ENV.get("NIK_ADMIN_PASSWORD", ""), name="login_admin")
    if not admin:
        print("登录失败")
        return 2

    st, body = req("POST", "/api/support/dict/type", admin,
                   {"dict_code": CODE, "dict_name": "E2E14 大数据演练字典", "status": 1,
                    "sort_order": 998, "description": "E2E14 性能取证，用后即清"})
    type_id = data_of(body).get("id")
    if not type_id:
        # 409（复跑）时按 dict_code 精确回查
        st2, body2 = req("GET", f"/api/support/dict/type/page?page=1&page_size=50&keyword={CODE}", admin)
        for row in data_of(body2).get("list") or []:
            if row.get("dict_code") == CODE:
                type_id = row.get("id")
                break
    print(f"  dict_type: HTTP {st} id={type_id}")

    # 排序号列有 0–9999 的 RANGE 约束（实测第 10001 行 sort=10000 被判 RANGE），故取模
    rows = [[CODE, f"v{i}", f"{CODE} 项{i}", "E2E14 性能取证", str(i % 9999), "否", "启用"]
            for i in range(1, N + 1)]
    xlsx, _ = timed("生成 xlsx", lambda: make_xlsx(HEADERS, rows))
    print(f"  {'xlsx 大小':<30}{len(xlsx)/1024/1024:>9.2f} MB")

    raw, ctype = multipart({}, "file", "e2e14_big.xlsx", xlsx,
                           "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    q = "/api/business/impexp/import/upload?biz_type=system.dict_item&mode=STRICT"
    (res, dt_pre) = timed("上传+预检（解析 1 万行）",
                          lambda: req_bytes(q, admin, method="POST", body=raw, ctype=ctype, timeout=600))
    st, headers, payload_bytes, _ = res
    import json
    payload = json.loads(payload_bytes)
    d = data_of(payload)
    rep = d.get("report") or {}
    task_id = d.get("task_id") or ""
    print(f"  HTTP {st} status={d.get('status')} total={rep.get('total')} valid={rep.get('valid')} error={rep.get('error')}")
    if not task_id:
        print("  预检失败，终止：", json.dumps(payload, ensure_ascii=False)[:400])
        return 3

    (st2, body2), dt_conf = timed("确认入库（1 万行事务）",
                                  lambda: req("POST", f"/api/business/impexp/import/{task_id}/confirm", admin, {}))
    d2 = data_of(body2)
    print(f"  HTTP {st2} status={d2.get('status')} imported_rows={d2.get('imported_rows')}")

    (res3, dt_exp) = timed("同步导出 1 万行 xlsx",
                           lambda: req_bytes(f"/api/business/impexp/export/sync?biz_type=system.dict_item&keyword={CODE}",
                                             admin, timeout=600, name="export_big.xlsx"))
    st3, hdr3, bytes3, _ = res3
    print(f"  HTTP {st3} content-type={hdr3.get('content-type')} bytes={len(bytes3)/1024/1024:.2f} MB")
    if bytes3:
        from e2e13_lib import read_xlsx
        sheets = read_xlsx(bytes3)
        data_rows = list(sheets.values())[0] if sheets else []
        print(f"  导出 xlsx 行数（含表头）= {len(data_rows)}")

    for label, path in [
        ("字典项第1页(50)", f"/api/support/dict/item/page?page=1&page_size=50&dict_type_id={type_id}"),
        ("字典项第100页(50)", f"/api/support/dict/item/page?page=100&page_size=50&dict_type_id={type_id}"),
        ("字典项第200页(50)", f"/api/support/dict/item/page?page=200&page_size=50&dict_type_id={type_id}"),
    ]:
        (st4, body4), _ = timed(label, lambda p=path: req("GET", p, admin))
        dd = data_of(body4)
        print(f"    HTTP {st4} total={dd.get('total')} rows={len(dd.get('list') or [])}")

    # 全量同步导出（应命中 1 万行上限 → 409 + 60015）
    (res5, _) = timed("全量同步导出（应超限）",
                      lambda: req_bytes("/api/business/impexp/export/sync?biz_type=system.dict_item", admin, timeout=600))
    st5, hdr5, body5, _ = res5
    print(f"    HTTP {st5} ctype={hdr5.get('content-type')} body={str(body5[:160], 'utf-8', 'replace')}")

    print(f"\nSUMMARY precheck={dt_pre:.0f}ms confirm={dt_conf:.0f}ms export_sync={dt_exp:.0f}ms rows={N}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
