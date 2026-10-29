"""D5 验收 1–6：新建 → 拖 4 节点 → 连线 → 改属性 → 保存 → 重开还原 → 撤销/重做 50 步 → 缩放/平移 → 自环/重复边拦截。

跑之前先把库里 d5_* 的演示流程清掉，保证每次都是干净起点。
"""

import asyncio
import json
import time

from d5_cdp import Session, login, show
from d5_step2 import (
    canvas_rect,
    connect,
    drop_from_palette,
    edge_keys,
    edge_mid,
    node_center,
    node_rects,
    palette_point,
    port_point,
)

RESULTS: list[tuple[str, bool, str]] = []


def check(label: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((label, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {label} {detail}")


async def cleanup(session: Session) -> list:
    ids = await session.js(
        """(async () => {
            const token = localStorage.getItem('x_access_token');
            const res = await (await fetch('/api/business/flow/def/page?page=1&page_size=100', {
                headers: {Authorization: `Bearer ${token}`}})).json();
            return (res.data.list || []).filter((d) => String(d.flow_code).startsWith('d5_')).map((d) => d.id);
        })()"""
    )
    deleted = []
    for def_id in ids or []:
        status = await session.js(
            f"""(async () => {{
                const res = await fetch('/api/business/flow/def/{def_id}', {{
                    method: 'DELETE', headers: {{Authorization: `Bearer ${{localStorage.getItem('x_access_token')}}`}}}});
                return res.status;
            }})()"""
        )
        deleted.append(status)
    return deleted


async def open_editor(session: Session) -> None:
    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(0.9)


async def click_text(session: Session, selector: str, text: str) -> bool:
    return bool(
        await session.js(
            f"""(() => {{
                const el = [...document.querySelectorAll({json.dumps(selector)})].find((n) => n.innerText.trim() === {json.dumps(text)});
                if (!el) return false;
                el.click();
                return true;
            }})()"""
        )
    )


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        show("登录", await login(session))

    await open_editor(session)
    removed = await cleanup(session)
    print("清理旧演示流程：", removed)
    await session.call("Page.reload")
    await asyncio.sleep(1.2)

    # ---------- ① 新建流程 ----------
    await click_text(session, "button", "新建流程")
    await asyncio.sleep(0.6)
    code = f"d5_{str(int(time.time()))[-6:]}"
    await session.js(
        f"""(() => {{
            const dialog = document.querySelector('.t-dialog');
            const inputs = [...dialog.querySelectorAll('input')];
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            const set = (el, v) => {{ el.focus(); setter.call(el, v); el.dispatchEvent(new Event('input', {{bubbles: true}})); }};
            set(inputs[0], '{code}');
            set(inputs[1], 'D5 报销审批演示');
            set(inputs[2], 'EXPENSE');
            return inputs.map((i) => i.value);
        }})()"""
    )
    await asyncio.sleep(0.2)
    await click_text(session, ".t-dialog button", "创建并进入编辑器")
    await session.wait_js("location.pathname.includes('/edit')", timeout=20)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 2", timeout=20)
    await asyncio.sleep(1.2)
    rects = await node_rects(session)
    check("① 新建流程进入编辑器（默认开始 / 结束两个节点）", len(rects) == 2, f"nodes={list(rects)}")
    laid_out = len({(r["transform"]) for r in rects.values()}) == len(rects)
    check("①+ 默认图被分层排版（不再叠成一个点）", laid_out, json.dumps({k: v["transform"] for k, v in rects.items()}, ensure_ascii=False))

    # ---------- ② 删掉默认边 ----------
    mid = await edge_mid(session, "e_start_end")
    if mid:
        await session.click_xy(mid["x"], mid["y"])
        await asyncio.sleep(0.2)
        await session.press_key("Delete", "Delete", key_code=46)
        await asyncio.sleep(0.4)
    edges = await edge_keys(session)
    check("② 点选连线 + Delete 删除默认边", len(edges) == 0, f"edges={edges}")

    # ---------- ③ 拖入两个审批节点 ----------
    canvas = await canvas_rect(session)
    a_target = {"x": canvas["left"] + canvas["w"] * 0.45, "y": canvas["top"] + canvas["h"] * 0.22}
    b_target = {"x": canvas["left"] + canvas["w"] * 0.70, "y": canvas["top"] + canvas["h"] * 0.55}
    await drop_from_palette(session, "APPROVAL", a_target)
    await asyncio.sleep(0.45)
    await drop_from_palette(session, "APPROVAL", b_target)
    await asyncio.sleep(0.5)
    rects = await node_rects(session)
    approvals = [k for k, v in rects.items() if v["type"] == "APPROVAL"]
    check("③ 左侧面板拖入 2 个审批节点", len(approvals) == 2, f"approvals={approvals}")

    # ---------- ④ 连线（开始→审批→审批→结束） ----------
    if len(approvals) == 2:
        await connect(session, "n_start", approvals[0])
        await asyncio.sleep(0.3)
        await connect(session, approvals[0], approvals[1])
        await asyncio.sleep(0.3)
        await connect(session, approvals[1], "n_end")
        await asyncio.sleep(0.4)
    edges = await edge_keys(session)
    check("④ 拖锚点连出 3 条线", len(edges) == 3, f"edges={edges}")

    # ---------- ⑤ 属性面板改名（真实键盘输入） ----------
    center = await node_center(session, approvals[0])
    await session.click_xy(center["x"], center["y"])
    await asyncio.sleep(0.4)
    box = await session.js(
        """(() => {
            const input = document.querySelector('.node-props input');
            if (!input) return null;
            input.focus();
            input.select();
            const r = input.getBoundingClientRect();
            return {x: r.left + r.width / 2, y: r.top + r.height / 2};
        })()"""
    )
    if box:
        await session.click_xy(box["x"], box["y"])
        await session.press_key("a", "KeyA", modifiers=2, key_code=65)  # Ctrl+A 全选
        await session.type_text("主管审批")
        await asyncio.sleep(0.5)
    label = await session.js(
        f"""document.querySelector('.flow-node[data-node-key="{approvals[0]}"] .flow-node__label')?.textContent"""
    )
    check("⑤ 选中节点 → 属性面板改名生效", label == "主管审批", f"label={label}")

    # ---------- ⑥ 保存 ----------
    before = await node_rects(session)
    await click_text(session, ".flow-editor__bar button", "保存")
    await asyncio.sleep(1.6)
    toast = await session.js("[...document.querySelectorAll('.t-message')].map((e) => e.innerText).join(' / ')")
    check("⑥ 保存成功（PUT /version/{vid}/graph）", "已保存" in (toast or ""), toast)

    # ---------- ⑦ 重新打开，图完整还原 ----------
    await session.call("Page.reload")
    await asyncio.sleep(1.2)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 4", timeout=20)
    await asyncio.sleep(1.2)
    after = await node_rects(session)
    edges_after = await edge_keys(session)
    same_positions = all(before[k]["transform"] == after.get(k, {}).get("transform") for k in before)
    label_after = await session.js(
        f"""document.querySelector('.flow-node[data-node-key="{approvals[0]}"] .flow-node__label')?.textContent"""
    )
    check(
        "⑦ 刷新后节点位置 / 连线 / 属性还原",
        len(after) == 4 and len(edges_after) == 3 and same_positions and label_after == "主管审批",
        json.dumps({"nodes": len(after), "edges": edges_after, "same_positions": same_positions, "label": label_after}, ensure_ascii=False),
    )

    # ---------- ⑧ 撤销 / 重做 50 步 ----------
    # 刷新后选中态已清空，先点中一个节点（键盘微调需要有选中节点）
    await session.js("document.querySelector('.flow-canvas').focus()")
    re_center = await node_center(session, approvals[0])
    await session.click_xy(re_center["x"], re_center["y"])
    await asyncio.sleep(0.4)
    start_transform = (await node_rects(session))[approvals[0]]["transform"]
    nudged = await session.js(
        """(async () => {
            const canvas = document.querySelector('.flow-canvas');
            for (let i = 0; i < 50; i += 1) {
                canvas.dispatchEvent(new KeyboardEvent('keydown', {key: 'ArrowRight', code: 'ArrowRight', bubbles: true}));
                await new Promise((r) => setTimeout(r, 8));
            }
            return true;
        })()"""
    )
    await asyncio.sleep(0.6)
    moved_transform = (await node_rects(session))[approvals[0]]["transform"]
    undo_clicks = await session.js(
        """(async () => {
            const btn = [...document.querySelectorAll('.flow-editor__bar button')].find((b) => b.innerText.trim() === '撤销');
            let clicks = 0;
            for (let i = 0; i < 50; i += 1) {
                if (btn.disabled) break;
                btn.click();
                clicks += 1;
                await new Promise((r) => setTimeout(r, 8));
            }
            return clicks;
        })()"""
    )
    await asyncio.sleep(0.6)
    restored_transform = (await node_rects(session))[approvals[0]]["transform"]
    redo_clicks = await session.js(
        """(async () => {
            const btn = [...document.querySelectorAll('.flow-editor__bar button')].find((b) => b.innerText.trim() === '重做');
            let clicks = 0;
            for (let i = 0; i < 50; i += 1) {
                if (btn.disabled) break;
                btn.click();
                clicks += 1;
                await new Promise((r) => setTimeout(r, 8));
            }
            return clicks;
        })()"""
    )
    await asyncio.sleep(0.6)
    redone_transform = (await node_rects(session))[approvals[0]]["transform"]
    check(
        "⑧ 撤销/重做 50 步（命令栈）",
        nudged and undo_clicks == 50 and redo_clicks == 50 and restored_transform == start_transform and redone_transform == moved_transform,
        json.dumps(
            {"undo": undo_clicks, "redo": redo_clicks, "start": start_transform, "moved": moved_transform,
             "restored": restored_transform, "redone": redone_transform},
            ensure_ascii=False,
        ),
    )

    # 全部重做后再撤销回去，保持与后端一致（避免后面保存把 +50px 存进去）
    await session.js(
        """(async () => {
            const btn = [...document.querySelectorAll('.flow-editor__bar button')].find((b) => b.innerText.trim() === '撤销');
            for (let i = 0; i < 50; i += 1) { if (btn.disabled) break; btn.click(); await new Promise((r) => setTimeout(r, 8)); }
            return true;
        })()"""
    )
    await asyncio.sleep(0.4)

    # ---------- ⑨ 缩放 0.3–2.0 / 平移 ----------
    center_pt = {"x": canvas["left"] + canvas["w"] / 2, "y": canvas["top"] + canvas["h"] / 2}
    for _ in range(30):
        await session.call(
            "Input.dispatchMouseEvent", type="mouseWheel", x=center_pt["x"], y=center_pt["y"], deltaX=0, deltaY=-240
        )
        await asyncio.sleep(0.03)
    await asyncio.sleep(0.3)
    zoom_max = await session.js("document.querySelector('.flow-canvas__scale')?.innerText")
    for _ in range(60):
        await session.call(
            "Input.dispatchMouseEvent", type="mouseWheel", x=center_pt["x"], y=center_pt["y"], deltaX=0, deltaY=240
        )
        await asyncio.sleep(0.03)
    await asyncio.sleep(0.3)
    zoom_min = await session.js("document.querySelector('.flow-canvas__scale')?.innerText")
    check("⑨ 滚轮缩放夹在 0.3–2.0", zoom_max == "200%" and zoom_min == "30%", f"max={zoom_max} min={zoom_min}")

    scene_before = await session.js("document.querySelector('.flow-canvas__svg > g')?.getAttribute('transform')")
    empty = {"x": canvas["left"] + 40, "y": canvas["top"] + canvas["h"] - 40}
    await session.pointer_drag(empty["x"], empty["y"], empty["x"] + 120, empty["y"] - 60, steps=8)
    await asyncio.sleep(0.3)
    scene_after = await session.js("document.querySelector('.flow-canvas__svg > g')?.getAttribute('transform')")
    check("⑨ 空白处拖拽平移画布", scene_before != scene_after, f"{scene_before} → {scene_after}")

    await session.js(
        """(() => { const b = [...document.querySelectorAll('.flow-canvas__toolbar button')].find((x) => x.innerText.includes('适配')); if (b) b.click(); return true; })()"""
    )
    await asyncio.sleep(0.4)

    # ---------- ⑩ 自环 / 重复边拦截 ----------
    edges_before = await edge_keys(session)
    out = await port_point(session, approvals[0])
    self_center = await node_center(session, approvals[0])
    await session.pointer_drag(out["x"], out["y"], self_center["x"], self_center["y"], steps=8)
    await asyncio.sleep(0.4)
    self_toast = await session.js("[...document.querySelectorAll('.t-message')].map((e) => e.innerText).join(' / ')")
    edges_after_self = await edge_keys(session)

    # 重复边：审批1 → 审批2 已经存在
    out2 = await port_point(session, approvals[0])
    target2 = await node_center(session, approvals[1])
    await session.pointer_drag(out2["x"], out2["y"], target2["x"], target2["y"], steps=8)
    await asyncio.sleep(0.4)
    dup_toast = await session.js("[...document.querySelectorAll('.t-message')].map((e) => e.innerText).join(' / ')")
    edges_after_dup = await edge_keys(session)
    check(
        "⑩ 自环 / 重复边被前端拦截",
        len(edges_after_self) == len(edges_before)
        and len(edges_after_dup) == len(edges_before)
        and "自环" in (self_toast or "")
        and "已经有连线" in (dup_toast or ""),
        json.dumps(
            {"edges": len(edges_before), "self_toast": self_toast, "dup_toast": dup_toast}, ensure_ascii=False
        ),
    )

    print("\nFLOW_CODE=", code, " APPROVALS=", approvals)
    print("\n== 汇总 ==")
    for label, ok, _ in RESULTS:
        print(("PASS " if ok else "FAIL ") + label)
    print("失败数：", sum(1 for _, ok, _ in RESULTS if not ok))


if __name__ == "__main__":
    asyncio.run(main())
