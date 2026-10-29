"""D5 验收 7–9：校验（无 END 高亮）→ 修复后校验通过 → 发布 → 编辑器只读 → 另存草稿恢复可编辑。"""

import asyncio
import json
import time

from d5_cdp import Session, login, show
from d5_step2 import edge_keys, node_center, node_rects

RESULTS: list[tuple[str, bool, str]] = []


def check(label: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((label, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {label} {detail}")


async def open_last_editor(session: Session) -> str:
    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(0.9)
    href = await session.js(
        """(() => {
            const a = [...document.querySelectorAll('.t-table__body .t-link')].find((x) => x.innerText.includes('D5'));
            return a ? a.getAttribute('href') || null : null;
        })()"""
    )
    clicked = await session.js(
        """(() => {
            const a = [...document.querySelectorAll('.t-table__body .t-link')].find((x) => x.innerText.includes('D5'));
            if (!a) return false;
            a.click();
            return true;
        })()"""
    )
    await session.wait_js("location.pathname.includes('/edit')", timeout=15)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 3", timeout=15)
    await asyncio.sleep(1.2)
    return href or ("" if clicked else "未找到流程")


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


async def setup_fresh_graph(session: Session) -> str:
    """新造一张干净的图：新建流程 → 删默认边 → 拖一个审批节点 → 开始→审批→结束。"""
    from d5_step2 import canvas_rect, connect, drop_from_palette, edge_keys, edge_mid, node_rects

    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(0.9)

    # 清掉历史 d5_* 演示数据，保证每次起点一致
    ids = await session.js(
        """(async () => {
            const token = localStorage.getItem('x_access_token');
            const res = await (await fetch('/api/business/flow/def/page?page=1&page_size=100', {
                headers: {Authorization: `Bearer ${token}`}})).json();
            return (res.data.list || []).filter((d) => String(d.flow_code).startsWith('d5_')).map((d) => d.id);
        })()"""
    )
    for def_id in ids or []:
        await session.js(
            f"""(async () => {{
                await fetch('/api/business/flow/def/{def_id}', {{method: 'DELETE',
                    headers: {{Authorization: `Bearer ${{localStorage.getItem('x_access_token')}}`}}}});
                return true;
            }})()"""
        )
    await session.call("Page.reload")
    await asyncio.sleep(1.2)

    await session.js(
        """(() => {
            const btn = [...document.querySelectorAll('button')].find((b) => b.innerText.includes('新建流程'));
            if (btn) btn.click();
            return !!btn;
        })()"""
    )
    await asyncio.sleep(0.6)
    code = f"d5_{str(int(time.time()))[-6:]}"
    await session.js(
        f"""(() => {{
            const dialog = document.querySelector('.t-dialog');
            const inputs = [...dialog.querySelectorAll('input')];
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            const set = (el, v) => {{ el.focus(); setter.call(el, v); el.dispatchEvent(new Event('input', {{bubbles: true}})); }};
            set(inputs[0], '{code}');
            set(inputs[1], 'D5 校验发布演示');
            set(inputs[2], 'EXPENSE');
            return true;
        }})()"""
    )
    await asyncio.sleep(0.2)
    await session.js(
        """(() => {
            const btn = [...document.querySelectorAll('.t-dialog button')].find((b) => b.innerText.includes('创建并进入编辑器'));
            if (btn) btn.click();
            return true;
        })()"""
    )
    await session.wait_js("location.pathname.includes('/edit')", timeout=20)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 2", timeout=20)
    await asyncio.sleep(1.2)

    # 删默认边
    mid = await edge_mid(session, "e_start_end")
    if mid:
        await session.click_xy(mid["x"], mid["y"])
        await asyncio.sleep(0.2)
        await session.press_key("Delete", "Delete", key_code=46)
        await asyncio.sleep(0.4)

    # 拖一个审批节点并连成 开始 → 审批 → 结束
    canvas = await canvas_rect(session)
    await drop_from_palette(session, "APPROVAL", {"x": canvas["left"] + canvas["w"] * 0.5, "y": canvas["top"] + canvas["h"] * 0.3})
    await asyncio.sleep(0.5)
    approvals = [k for k, v in (await node_rects(session)).items() if v["type"] == "APPROVAL"]
    await connect(session, "n_start", approvals[0])
    await asyncio.sleep(0.3)
    await connect(session, approvals[0], "n_end")
    await asyncio.sleep(0.4)
    await click_bar(session, "保存")
    await asyncio.sleep(1.6)
    print("建图完成：", await edge_keys(session), "code=", code)
    return code


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        await login(session)

    await setup_fresh_graph(session)
    show("打开编辑器", await session.js("({url: location.href, nodes: [...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)})"))

    # ---------- ⑦ 造一个「无 END」的图，校验要高亮 ----------
    # 节点可能互相重叠：最多尝试 3 次「点 END → Delete」，直到 n_end 真的没了。
    deleted_rounds = []
    for _ in range(3):
        nodes_now = await session.js("[...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)")
        if "n_end" not in nodes_now:
            break
        end_center = await node_center(session, "n_end")
        if not end_center:
            break
        await session.click_xy(end_center["x"], end_center["y"])
        await asyncio.sleep(0.3)
        await session.press_key("Delete", "Delete", key_code=46)
        await asyncio.sleep(0.6)
        deleted_rounds.append(
            await session.js("[...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)")
        )
    nodes_after_delete = await session.js("[...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)")
    check(
        "⑦-1 删除结束节点（造无 END 的图）",
        "n_end" not in nodes_after_delete,
        f"每轮剩余={deleted_rounds}",
    )

    await click_bar(session, "校验")
    await asyncio.sleep(2.5)
    issue_state = await session.js(
        """(() => ({
            board: !!document.querySelector('.issue-board'),
            count: document.querySelectorAll('.issue-board__item').length,
            highlighted: document.querySelectorAll('.flow-node.has-issue').length,
            texts: [...document.querySelectorAll('.issue-board__item')].map((e) => e.innerText.replace(/\\n/g, ' ')).slice(0, 6),
            toasts: [...document.querySelectorAll('.t-message')].map((e) => e.innerText),
        }))()"""
    )
    check(
        "⑦-2 校验后画布高亮问题节点 + 列表给出原因",
        issue_state["board"] and issue_state["count"] > 0 and issue_state["highlighted"] > 0,
        json.dumps(issue_state, ensure_ascii=False),
    )

    # 点一条问题 → 定位（选「带节点」的那条：R2 这类「缺结束节点」没有具体节点可定位）
    clicked_issue = await session.js(
        """(() => {
            const items = [...document.querySelectorAll('.issue-board__item')];
            const target = items.find((item) => item.querySelector('.issue-board__locate'));
            if (!target) return { clicked: false, rule: null, text: items[0] ? items[0].innerText : null };
            const rule = target.querySelector('.t-tag')?.innerText || null;
            target.click();
            return {clicked: true, rule};
        })()"""
    )
    await asyncio.sleep(0.6)
    locate = await session.js(
        """(() => ({
            selected: document.querySelectorAll('.flow-node.is-selected').length,
            first: document.querySelector('.flow-node.is-selected')?.dataset.nodeKey || null,
        }))()"""
    )
    check(
        "⑦-3 点问题条目可定位到节点",
        bool(clicked_issue and clicked_issue.get("clicked")) and locate["selected"] == 1,
        json.dumps({"clicked": clicked_issue, "locate": locate}, ensure_ascii=False),
    )

    # ---------- ⑧ 修好（撤销恢复 END + 重新连线）→ 校验通过 ----------
    await session.js("document.querySelector('.flow-canvas').focus()")
    await click_bar(session, "撤销")
    await asyncio.sleep(0.6)
    restored = await session.js("[...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)")
    check("⑧-1 撤销恢复结束节点", "n_end" in restored, f"nodes={restored}")

    # END 恢复后需要重新连 approval2 → END（删节点时边一起删掉了）
    from d5_step2 import connect

    approvals = [k for k, v in (await node_rects(session)).items() if v["type"] == "APPROVAL"]
    edges_now = await edge_keys(session)
    if len(edges_now) < 3 and approvals:
        await connect(session, approvals[-1], "n_end")
        await asyncio.sleep(0.5)
    edges_fixed = await edge_keys(session)
    print("修复后 edges:", edges_fixed)

    await click_bar(session, "校验")
    await asyncio.sleep(2.5)
    passed = await session.js(
        """(() => ({
            board: !!document.querySelector('.issue-board'),
            toasts: [...document.querySelectorAll('.t-message')].map((e) => e.innerText),
            highlighted: document.querySelectorAll('.flow-node.has-issue').length,
        }))()"""
    )
    check(
        "⑧-2 修复后校验通过（画布无高亮、提示通过）",
        not passed["board"] and passed["highlighted"] == 0 and any("校验通过" in t for t in passed["toasts"]),
        json.dumps(passed, ensure_ascii=False),
    )

    # ---------- ⑨ 发布 → PUBLISHED → 编辑器只读 ----------
    await click_bar(session, "发布")
    await session.wait_js("document.body.innerText.includes('已发布')", timeout=20)
    await asyncio.sleep(1.8)
    readonly_state = await session.js(
        """(() => ({
            banner: document.body.innerText.includes('如需修改请另存草稿'),
            ports: document.querySelectorAll('.flow-node__port').length,
            saveDisabled: (() => {
                const b = [...document.querySelectorAll('.flow-editor__bar button')].find((x) => x.innerText.trim() === '保存');
                return b ? b.disabled : null;
            })(),
            statusTag: [...document.querySelectorAll('.status-badge')].map((e) => e.innerText.trim()).join(','),
            toasts: [...document.querySelectorAll('.t-message')].map((e) => e.innerText),
        }))()"""
    )
    check(
        "⑨-1 发布成功、流程变 PUBLISHED、编辑器只读（无锚点 / 保存禁用 / 横幅提示）",
        readonly_state["banner"] and readonly_state["ports"] == 0 and readonly_state["saveDisabled"] is True
        and "已发布" in (readonly_state["statusTag"] or ""),
        json.dumps(readonly_state, ensure_ascii=False),
    )

    # 只读态下尝试拖节点应该无效
    node_key = (await session.js("[...document.querySelectorAll('.flow-node')].map((e) => e.dataset.nodeKey)"))[0]
    before = await session.js(
        f"""document.querySelector('.flow-node[data-node-key="{node_key}"]')?.getAttribute('transform')"""
    )
    center = await node_center(session, node_key)
    await session.pointer_drag(center["x"], center["y"], center["x"] + 80, center["y"] + 40, steps=6)
    await asyncio.sleep(0.4)
    after = await session.js(
        f"""document.querySelector('.flow-node[data-node-key="{node_key}"]')?.getAttribute('transform')"""
    )
    check("⑨-2 只读态拖不动节点", before == after, f"{before} → {after}")

    # ---------- 另存为新草稿 → 又可编辑 ----------
    clicked = await session.js(
        """(() => {
            const btn = [...document.querySelectorAll('button')].find((b) => b.innerText.includes('另存为新草稿'));
            if (!btn) return false;
            btn.click();
            return true;
        })()"""
    )
    await session.wait_js("document.querySelectorAll('.flow-node__port').length > 0", timeout=20)
    await asyncio.sleep(1.0)
    draft_state = await session.js(
        """(() => ({
            ports: document.querySelectorAll('.flow-node__port').length,
            saveDisabled: (() => {
                const b = [...document.querySelectorAll('.flow-editor__bar button')].find((x) => x.innerText.trim() === '保存');
                return b ? b.disabled : null;
            })(),
            versionOptions: [...document.querySelectorAll('.flow-editor__version-select .t-select__wrap, .flow-editor__version-select')].length,
            toasts: [...document.querySelectorAll('.t-message')].map((e) => e.innerText),
            banner: document.body.innerText.includes('如需修改请另存草稿'),
        }))()"""
    )
    check(
        "⑨-3 另存为新草稿后编辑器恢复可编辑",
        clicked and draft_state["ports"] > 0 and draft_state["saveDisabled"] is False and not draft_state["banner"],
        json.dumps(draft_state, ensure_ascii=False),
    )

    print("\n== 汇总 ==")
    for label, ok, _ in RESULTS:
        print(("PASS " if ok else "FAIL ") + label)
    print("失败数：", sum(1 for _, ok, _ in RESULTS if not ok))


asyncio.run(main())
