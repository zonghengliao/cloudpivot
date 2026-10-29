#!/usr/bin/env python3
"""L1 修复 E2E —— 阶段 B3：三层回传取证（复用已跑完的任务）"""
import json
import sys

sys.path.insert(0, "/tmp/l1fix")
from e2e_a import call, login  # noqa: E402

state = json.load(open("/tmp/l1fix/state.json"))
token = login()
task_id = state["task_id"]

st, r = call("GET", f"/api/business/task/{task_id}", token=token)
d = r.get("data") or {}
print(f"[B3-1] GET /api/business/task/{task_id} -> HTTP {st} code={r.get('code')}")
for k in ("task_no", "title", "status", "progress", "exec_mode", "hermes_task_id", "hermes_status",
          "hermes_board", "done_at", "owner_name"):
    print(f"    {k} = {d.get(k)!r}")
print(f"    ai_summary = {(d.get('ai_summary') or '')[:200]!r}")
res = d.get("ai_result")
res_text = json.dumps(res, ensure_ascii=False) if not isinstance(res, str) else res
print(f"    ai_result 类型 = {type(res).__name__} 长度 = {len(res_text or '')}")
print(f"    ai_result 前 400 字 = {(res_text or '')[:400]!r}")

st, r = call("GET", f"/api/business/task/{task_id}/comments", token=token)
payload = r.get("data") or {}
comments = payload.get("list") or payload.get("records") or payload
print(f"\n[B3-2] GET .../comments -> HTTP {st} code={r.get('code')} 条数={len(comments)}")
for c in comments:
    ct = c.get("comment_type") or c.get("type")
    print(f"    - type={ct!r} author={(c.get('author_name') or c.get('create_by_name'))!r} "
          f"len={len(c.get('content') or '')}")
    if ct == "AI_REPORT":
        print(f"      AI_REPORT 前 200 字 = {(c.get('content') or '')[:200]!r}")

st, r = call("GET", f"/api/business/task/{task_id}/attachments", token=token)
payload = r.get("data") or {}
atts = payload.get("list") or payload.get("records") or payload
print(f"\n[B3-3] GET .../attachments -> HTTP {st} code={r.get('code')} 个数={len(atts)}")
for a in atts:
    print(f"    - {json.dumps(a, ensure_ascii=False)[:300]}")

ok_report = any((c.get("comment_type") or c.get("type")) == "AI_REPORT" for c in comments)
ok_result = bool(res_text and res_text.strip() not in ("", "null", "{}"))
ok_att = any((a.get("source") or "").upper() == "AI" for a in atts)
ok_status = d.get("status") in ("REVIEW", "DONE")
print("\n=== 验收结论 ===")
print(f"  [{'PASS' if ok_status else 'FAIL'}] 任务跑到 {d.get('status')}（不是 blocked/pending）")
print(f"  [{'PASS' if ok_report else 'FAIL'}] ① AI_REPORT 评论")
print(f"  [{'PASS' if ok_result else 'FAIL'}] ② ai_result 非空")
print(f"  [{'PASS' if ok_att else 'FAIL'}] ③ 附件 source=AI")
