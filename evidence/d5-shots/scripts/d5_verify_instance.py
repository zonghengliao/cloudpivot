"""D5 验收 12–13（D2 已交付后补验）：真实实例走一遍 —— 发起 → 待办 → 详情进度条 → 同意 → 已通过。

前置：库里要有 PUBLISHED 的 d5_ 流程，且其审批节点可解析出审批人。
脚本会把该节点的「审批人来源」改成「发起人本人」+「自审策略 = 允许自审」，
这样单子会停在第 2 步等我审 —— 正好用来验「3 节点流程走到第 2 步」。
"""

import asyncio
import json

from d5_cdp import Session, login, show
from d5_step2 import node_center, node_rects

RESULTS: list[tuple[str, bool, str]] = []


def check(label: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((label, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {label} {detail}")


async def pick_select(session: Session, label_text: str, option_text: str) -> bool:
    """在属性面板里点开「label_text」对应的下拉，选中 option_text。"""
    opened = await session.js(
        f"""(() => {{
            const items = [...document.querySelectorAll('.node-props .t-form__item')];
            const item = items.find((it) => it.innerText.trim().startsWith({json.dumps(label_text)}));
            if (!item) return false;
            const input = item.querySelector('.t-select input, input');
            if (!input) return false;
            input.focus();
            input.click();
            return true;
        }})()"""
    )
    if not opened:
        return False
    await asyncio.sleep(0.6)
    picked = await session.js(
        f"""(() => {{
            const opts = [...document.querySelectorAll('.t-select__dropdown .t-select-option, .t-select-option')];
            const target = opts.find((o) => o.innerText.trim() === {json.dumps(option_text)});
            if (!target) return false;
            target.click();
            return true;
        }})()"""
    )
    await asyncio.sleep(0.5)
    return bool(picked)


async def click_bar(session: Session, text: str) -> bool:
    return bool(
        await session.js(
            f"""(() => {{
                const btn = [...document.querySelectorAll('.flow-editor__bar button')].find((b) => b.innerText.trim() === {json.dumps(text)});
                if (!btn || btn.disabled) return false;
                btn.click();
                return true;
            }})()"""
        )
    )


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        await login(session)

    # ---------- 0. 找一个 PUBLISHED 的 d5 流程，进它的编辑器 ----------
    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(1.0)
    rows = await session.js(
        """(() => [...document.querySelectorAll('.t-table__body tr')].map((tr) => tr.innerText.replace(/\\s+/g, ' ')))()"""
    )
    show("流程列表", rows)
    clicked = await session.js(
        """(() => {
            const row = [...document.querySelectorAll('.t-table__body tr')].find((tr) => tr.innerText.includes('D5'));
            const link = row ? [...row.querySelectorAll('.t-link')].find((a) => a.innerText.trim() === '编辑') : null;
            if (link) link.click();
            return !!link;
        })()"""
    )
    await session.wait_js("location.pathname.includes('/edit')", timeout=20)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 2", timeout=20)
    await asyncio.sleep(1.4)
    print("进编辑器:", clicked, await session.js("location.pathname"))

    # 只读（当前版本已发布）→ 另存草稿
    ports = await session.js("document.querySelectorAll('.flow-node__port').length")
    if ports == 0:
        await session.js(
            """(() => { const b = [...document.querySelectorAll('button')].find((x) => x.innerText.includes('另存为新草稿')); if (b) b.click(); return !!b; })()"""
        )
        await session.wait_js("document.querySelectorAll('.flow-node__port').length > 0", timeout=20)
        await asyncio.sleep(1.2)

    # ---------- 1. 把审批节点的审批人改成「发起人本人」+ 允许自审 ----------
    rects = await node_rects(session)
    approvals = [k for k, v in rects.items() if v["type"] == "APPROVAL"]
    if not approvals:
        print("没有审批节点，先拖一个")
        import sys

        sys.exit(1)
    center = await node_center(session, approvals[0])
    await session.click_xy(center["x"], center["y"])
    await asyncio.sleep(0.5)
    ok_source = await pick_select(session, "审批人来源", "发起人本人")
    ok_self = await pick_select(session, "自审策略", "允许自审")
    check("⑬-0 属性面板改审批人来源（发起人本人）+ 自审策略（允许）", ok_source and ok_self, f"source={ok_source} self={ok_self}")
    show(
        "面板当前值",
        await session.js(
            """(() => [...document.querySelectorAll('.node-props .t-form__item')].map((it) => it.innerText.replace(/\\n/g, ' ').slice(0, 60)))()"""
        ),
    )

    await click_bar(session, "保存")
    await asyncio.sleep(1.6)
    await click_bar(session, "校验")
    await asyncio.sleep(2.2)
    validate_toast = await session.js("[...document.querySelectorAll('.t-message')].map((e) => e.innerText).join(' / ')")
    await click_bar(session, "发布")
    await session.wait_js("document.body.innerText.includes('已发布')", timeout=20)
    await asyncio.sleep(1.6)
    print("保存/校验/发布:", validate_toast)

    # ---------- 2. 发起审批 ----------
    await session.goto("/flow/start")
    await session.wait_js("!!document.querySelector('.flow-start-page')", timeout=15)
    await asyncio.sleep(2.2)
    title = "D5 真机：报销 6800 元（第 2 步待审）"
    await session.js(
        f"""(() => {{
            const input = document.querySelector('.start-form input[placeholder*="一句话"]');
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            input.focus();
            setter.call(input, {json.dumps(title)});
            input.dispatchEvent(new Event('input', {{bubbles: true}}));
            const area = document.querySelector('.start-form textarea');
            if (area) {{
                const setter2 = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
                area.focus();
                setter2.call(area, '部门团建 + 客户餐叙，发票 12 张，请审。');
                area.dispatchEvent(new Event('input', {{bubbles: true}}));
            }}
            return true;
        }})()"""
    )
    await asyncio.sleep(0.4)
    await session.js(
        """(() => { const b = [...document.querySelectorAll('.start-form__actions button')].find((x) => x.innerText.includes('提交审批')); if (b) b.click(); return !!b; })()"""
    )
    await session.wait_js("location.pathname.includes('/flow/instance/')", timeout=20)
    await asyncio.sleep(2.0)
    detail_url = await session.js("location.pathname")
    check("⑫-1 发起审批成功并跳详情", "/flow/instance/" in detail_url, detail_url)

    # ---------- 3. 详情页：进度条位置 + 操作区 ----------
    detail_state = await session.js(
        """(() => ({
            title: document.querySelector('.head__title')?.innerText || null,
            no: document.querySelector('.head__no')?.innerText || null,
            badges: [...document.querySelectorAll('.status-badge')].map((e) => e.innerText.trim()),
            steps: [...document.querySelectorAll('.flow-progress__step')].map((e) => e.innerText.replace(/\\n/g, ' ').trim()),
            activeIndex: [...document.querySelectorAll('.flow-progress__step')].findIndex((e) => e.classList.contains('is-active')),
            doneCount: document.querySelectorAll('.flow-progress__step.is-done').length,
            myActions: document.querySelectorAll('.action-card').length,
            timeline: [...document.querySelectorAll('.timeline__item')].map((e) => e.innerText.replace(/\\n/g, ' ').slice(0, 60)),
            nodeItems: [...document.querySelectorAll('.node-item')].map((e) => e.innerText.replace(/\\n/g, ' ').slice(0, 50)),
        }))()"""
    )
    show("详情页状态", detail_state)
    check(
        "⑬ 详情页头部 + 进度条位置正确（3 节点，第 2 步审批中）",
        detail_state["title"] == title
        and len(detail_state["steps"]) == 3
        and detail_state["activeIndex"] == 1
        and detail_state["doneCount"] == 1,
        json.dumps({"steps": detail_state["steps"], "active": detail_state["activeIndex"], "done": detail_state["doneCount"]}, ensure_ascii=False),
    )
    check("⑬-2 我是当前审批人 → 操作区可见", detail_state["myActions"] == 1, f"actions={detail_state['myActions']}")

    # ---------- 4. 待办列表出现这张单子 ----------
    await session.goto("/flow/task")
    await session.wait_js("!!document.querySelector('.flow-task-page')", timeout=15)
    await asyncio.sleep(2.0)
    todo_state = await session.js(
        """(() => ({
            tabs: [...document.querySelectorAll('.t-tabs__nav-item')].map((e) => e.innerText.trim()),
            cards: [...document.querySelectorAll('.task-card')].map((e) => e.innerText.replace(/\\n/g, ' ').slice(0, 120)),
            buttons: [...document.querySelectorAll('.task-card__actions button')].map((e) => e.innerText.trim()),
        }))()"""
    )
    show("待办 Tab", todo_state)
    check(
        "⑫-2 待办列表出现真实待办卡片（含同意/驳回/转办/加签）",
        len(todo_state["cards"]) >= 1 and "同意" in todo_state["buttons"],
        json.dumps(todo_state["cards"][:2], ensure_ascii=False),
    )

    # ---------- 5. 同意 ----------
    await session.js(
        """(() => { const b = [...document.querySelectorAll('.task-card__actions button')].find((x) => x.innerText.trim() === '同意'); if (b) b.click(); return !!b; })()"""
    )
    await asyncio.sleep(2.5)
    after_approve = await session.js(
        """(() => ({
            cards: document.querySelectorAll('.task-card').length,
            empty: document.querySelector('.task-empty')?.innerText.replace(/\\n/g, ' ') || null,
            toasts: [...document.querySelectorAll('.t-message')].map((e) => e.innerText),
        }))()"""
    )
    check("⑫-3 同意后待办消失", after_approve["cards"] == 0, json.dumps(after_approve, ensure_ascii=False))

    # ---------- 6. 回详情：已通过 ----------
    await session.goto(detail_url)
    await session.wait_js("!!document.querySelector('.flow-detail-page')", timeout=15)
    await asyncio.sleep(2.0)
    final_state = await session.js(
        """(() => ({
            badges: [...document.querySelectorAll('.status-badge')].map((e) => e.innerText.trim()),
            steps: [...document.querySelectorAll('.flow-progress__step')].map((e) => e.className.replace('flow-progress__step', '').trim()),
            actions: document.querySelectorAll('.action-card').length,
            timeline: [...document.querySelectorAll('.timeline__item')].map((e) => e.innerText.replace(/\\n/g, ' ').slice(0, 70)),
        }))()"""
    )
    show("审批完成后的详情", final_state)
    check(
        "⑭ 审完后：实例徽标=已通过、进度条全绿、操作区消失、时间线有记录",
        "已通过" in " ".join(final_state["badges"])
        and final_state["steps"].count("is-done") >= 3
        and final_state["actions"] == 0
        and len(final_state["timeline"]) >= 1,
        json.dumps(final_state, ensure_ascii=False),
    )

    # ---------- 7. 我发起的 / 已办 ----------
    await session.goto("/flow/task")
    await session.wait_js("!!document.querySelector('.flow-task-page')", timeout=15)
    await asyncio.sleep(1.8)
    for label in ("我发起的", "我已审批", "抄送我的"):
        await session.js(
            f"""(() => {{ const t = [...document.querySelectorAll('.t-tabs__nav-item')].find((e) => e.innerText.trim() === {json.dumps(label)}); if (t) t.click(); return !!t; }})()"""
        )
        await asyncio.sleep(1.6)
        state = await session.js(
            """(() => ({cards: document.querySelectorAll('.task-card').length, empty: document.querySelector('.task-empty')?.innerText.replace(/\\n/g, ' ') || null}))()"""
        )
        print(f"Tab「{label}」:", json.dumps(state, ensure_ascii=False))

    print("\n== 汇总 ==")
    for label, ok, _ in RESULTS:
        print(("PASS " if ok else "FAIL ") + label)
    print("失败数：", sum(1 for _, ok, _ in RESULTS if not ok))


asyncio.run(main())
