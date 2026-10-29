#!/usr/bin/env python3
"""L1 修复 E2E —— 阶段 B2：派活给新 AI 用户，等它真跑完（done → REVIEW），验三层回传"""
import json
import subprocess
import sys
import time

sys.path.insert(0, "/tmp/l1fix")
from e2e_a import call, login  # noqa: E402

PROFILE = "fix-check-ai"


def kanban_show(card):
    out = subprocess.run(
        ["env", "HERMES_HOME=/root/.hermes", "/root/.hermes/hermes-agent/hermes",
         "kanban", "--board", "dev", "show", card, "--json"],
        capture_output=True, text=True, timeout=120,
    )
    try:
        return json.loads(out.stdout)
    except Exception:
        return {"_raw_stdout": out.stdout[:800], "_stderr": out.stderr[:800]}


def main():
    token = login()
    state = json.load(open("/tmp/l1fix/state.json"))
    uid, token = state["user_id"], token

    print(f"[B2-1] POST /api/business/task 建任务，负责人 = AI 账号 {uid}")
    st, r = call(
        "POST",
        "/api/business/task",
        {
            "title": "[L1修复验收] 输出一段 markdown 交付说明",
            "description": (
                "请在当前工作区写一个 markdown 文件 l1-fix-report.md，内容包含：\n"
                "1. 一句话说明你已能正常认证（这是 profile 级 .env 凭据生效的证据）；\n"
                "2. 当前时间戳；\n"
                "3. 一段 20 字以上的说明文字。\n"
                "写完后把文件作为附件上传到本任务，并在任务评论里写 AI 报告。"
            ),
            "priority": "P2",
            "owner_id": uid,
            "need_review": True,
        },
        token=token,
    )
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    if st != 200 or r.get("code") != 0:
        print(json.dumps(r, ensure_ascii=False)[:800])
        sys.exit(2)
    task = r["data"]
    task_id = task["id"]
    print(f"  task_id={task_id} status={task.get('status')} owner={task.get('owner_name')}")
    state["task_id"] = task_id
    json.dump(state, open("/tmp/l1fix/state.json", "w"))

    print(f"\n[B2-2] POST /api/business/task/{task_id}/dispatch-ai")
    st, r = call("POST", f"/api/business/task/{task_id}/dispatch-ai", {}, token=token)
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    if st != 200 or r.get("code") != 0:
        print(json.dumps(r, ensure_ascii=False)[:900])
        sys.exit(3)
    disp = r["data"]
    card = disp.get("hermes_task_id")
    print(f"  hermes_task_id={card} board={disp.get('hermes_board')} status={disp.get('status')}")
    state["card"] = card
    json.dump(state, open("/tmp/l1fix/state.json", "w"))

    print("\n[B2-3] 轮询执行结果（每 10s 一次，最多 8 分钟）")
    final = None
    for i in range(48):
        time.sleep(10)
        st, r = call("GET", f"/api/business/task/{task_id}", token=token)
        d = r.get("data") or {}
        k = kanban_show(card) if card else {}
        kstatus = k.get("status") or k.get("column") or "?"
        print(f"  t+{(i + 1) * 10:3d}s  task.status={d.get('status')!s:10s} "
              f"exec_mode={d.get('exec_mode')!s:6s} hermes_status={d.get('hermes_status')!s:10s} "
              f"kanban={kstatus!s:10s} ai_result_len={len(d.get('ai_result') or '')}")
        if d.get("status") in ("REVIEW", "DONE"):
            final = d
            break
        if d.get("status") in ("CANCELLED",):
            final = d
            break
        if isinstance(kstatus, str) and kstatus.lower() in ("blocked", "failed", "crashed"):
            print("  !! Hermes 侧未成功执行，dump kanban show：")
            print(json.dumps(k, ensure_ascii=False)[:2500])
            break

    if final is None:
        print("  [FAIL] 8 分钟内未跑到 REVIEW/DONE")
        print(json.dumps(kanban_show(card), ensure_ascii=False)[:3000])
        sys.exit(4)

    print(f"\n[B2-4] 任务终态：status={final.get('status')} progress={final.get('progress')} "
          f"done_at={final.get('done_at')}")
    print(f"  ai_summary={ (final.get('ai_summary') or '')[:200]!r}")
    print(f"  ai_result 长度={len(final.get('ai_result') or '')}")
    print(f"  hermes_status={final.get('hermes_status')} hermes_task_id={final.get('hermes_task_id')}")

    print("\n[B2-5] 三层回传")
    st, r = call("GET", f"/api/business/task/{task_id}/comments", token=token)
    comments = (r.get("data") or {}).get("list") or (r.get("data") or {}).get("records") or []
    print(f"  评论接口 -> HTTP {st} code={r.get('code')} 共 {len(comments)} 条")
    for c in comments:
        print(f"    - type={c.get('comment_type')} author={c.get('author_name')} "
              f"len={len(c.get('content') or '')} 前 60 字={(c.get('content') or '')[:60]!r}")
    st, r = call("GET", f"/api/business/task/{task_id}/attachments", token=token)
    atts = (r.get("data") or {}).get("list") or (r.get("data") or {}).get("records") or []
    print(f"  附件接口 -> HTTP {st} code={r.get('code')} 共 {len(atts)} 个")
    for a in atts:
        print(f"    - name={a.get('file_name')} source={a.get('source')} "
              f"ai_profile={a.get('ai_profile')} size={a.get('file_size')}")

    ok_report = any(c.get("comment_type") == "AI_REPORT" for c in comments)
    ok_result = bool((final.get("ai_result") or "").strip())
    ok_att = any((a.get("source") or "").upper() == "AI" for a in atts)
    print(f"\n  [{'PASS' if ok_report else 'FAIL'}] ① AI_REPORT 评论")
    print(f"  [{'PASS' if ok_result else 'FAIL'}] ② ai_result 非空")
    print(f"  [{'PASS' if ok_att else 'FAIL'}] ③ 附件 source=AI")
    json.dump(final, open("/tmp/l1fix/final_task.json", "w"), ensure_ascii=False, default=str)
    if not (ok_report and ok_result):
        sys.exit(5)


if __name__ == "__main__":
    main()
