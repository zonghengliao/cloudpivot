"""D5 步骤 2：编辑器画布交互 —— 拖入节点 / 连线 / 改属性 / 保存 / 重新打开还原。"""

import asyncio
import json

from d5_cdp import Session, login, show


async def node_rects(session: Session) -> dict:
    return await session.js(
        """(() => {
            const out = {};
            for (const el of document.querySelectorAll('.flow-node')) {
                const r = el.getBoundingClientRect();
                out[el.dataset.nodeKey] = {
                    type: el.dataset.nodeType,
                    x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height),
                    transform: el.getAttribute('transform'),
                };
            }
            return out;
        })()"""
    )


async def edge_keys(session: Session) -> list:
    return await session.js("[...document.querySelectorAll('.flow-edge')].map((el) => el.dataset.edgeKey)")


async def port_point(session: Session, key: str) -> dict | None:
    return await session.js(
        f"""(() => {{
            const node = document.querySelector('.flow-node[data-node-key="{key}"]');
            if (!node) return null;
            const port = node.querySelector('.flow-node__port--out');
            const r = port.getBoundingClientRect();
            return {{x: r.left + r.width / 2, y: r.top + r.height / 2}};
        }})()"""
    )


async def node_center(session: Session, key: str) -> dict | None:
    return await session.js(
        f"""(() => {{
            const node = document.querySelector('.flow-node[data-node-key="{key}"]');
            if (!node) return null;
            const r = node.getBoundingClientRect();
            return {{x: r.left + r.width / 2, y: r.top + r.height / 2}};
        }})()"""
    )


async def palette_point(session: Session, node_type: str) -> dict | None:
    return await session.js(
        f"""(() => {{
            const el = document.querySelector('.palette-item[data-node-type="{node_type}"]');
            if (!el) return null;
            const r = el.getBoundingClientRect();
            return {{x: r.left + r.width / 2, y: r.top + r.height / 2}};
        }})()"""
    )


async def canvas_rect(session: Session) -> dict:
    return await session.js(
        """(() => {
            const el = document.querySelector('.flow-editor__canvas');
            const r = el.getBoundingClientRect();
            return {left: r.left, top: r.top, w: r.width, h: r.height};
        })()"""
    )


async def edge_mid(session: Session, key: str) -> dict | None:
    return await session.js(
        f"""(() => {{
            const path = document.querySelector('.flow-edge[data-edge-key="{key}"] .flow-edge__hit');
            if (!path) return null;
            const point = path.getPointAtLength(path.getTotalLength() / 2);
            const m = path.getScreenCTM();
            const p = new DOMPoint(point.x, point.y).matrixTransform(m);
            return {{x: p.x, y: p.y}};
        }})()"""
    )


async def drop_from_palette(session: Session, node_type: str, target: dict) -> bool:
    start = await palette_point(session, node_type)
    if not start:
        return False
    await session.pointer_drag(start["x"], start["y"], target["x"], target["y"], steps=10)
    return True


async def connect(session: Session, from_key: str, to_key: str) -> bool:
    out = await port_point(session, from_key)
    target = await node_center(session, to_key)
    if not out or not target:
        return False
    await session.pointer_drag(out["x"], out["y"], target["x"], target["y"], steps=10)
    return True


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        show("登录", await login(session))

    # 打开刚建的流程（URL 从列表页取第一条「编辑」链接）
    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(0.8)
    await session.js(
        """(() => {
            const link = [...document.querySelectorAll('.t-table__body a, .t-table__body .t-link')].find((a) => a.innerText.includes('D5'));
            if (link) link.click();
            return !!link;
        })()"""
    )
    await session.wait_js("location.pathname.endsWith('/edit')", timeout=15)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 2", timeout=15)
    await asyncio.sleep(1.0)

    show("初始图", {"nodes": await node_rects(session), "edges": await edge_keys(session)})
    canvas = await canvas_rect(session)
    print("canvas", canvas)

    # ① 删掉新建时自带的那条 START→END（保持后续是一条干净的链）
    mid = await edge_mid(session, "e_start_end")
    if mid:
        await session.click_xy(mid["x"], mid["y"])
        await session.press_key("Delete", "Delete", key_code=46)
        await asyncio.sleep(0.4)
    print("删默认边后 edges:", await edge_keys(session))

    # ② 从左侧面板拖两个审批节点进画布
    a_target = {"x": canvas["left"] + canvas["w"] * 0.42, "y": canvas["top"] + canvas["h"] * 0.30}
    b_target = {"x": canvas["left"] + canvas["w"] * 0.62, "y": canvas["top"] + canvas["h"] * 0.62}
    await drop_from_palette(session, "APPROVAL", a_target)
    await asyncio.sleep(0.4)
    await drop_from_palette(session, "APPROVAL", b_target)
    await asyncio.sleep(0.5)
    rects = await node_rects(session)
    show("拖入两个审批节点后", rects)

    keys = list(rects.keys())
    approvals = [k for k in keys if rects[k]["type"] == "APPROVAL"]
    print("APPROVALS:", approvals)

    # ③ 连线：START → A → B → END
    if len(approvals) >= 2:
        await connect(session, "n_start", approvals[0])
        await asyncio.sleep(0.35)
        await connect(session, approvals[0], approvals[1])
        await asyncio.sleep(0.35)
        await connect(session, approvals[1], "n_end")
        await asyncio.sleep(0.4)
    print("连线后 edges:", await edge_keys(session))

    # ④ 选中第一个审批节点，改名称（属性面板 → patch-node 命令）
    center = await node_center(session, approvals[0])
    if center:
        await session.click_xy(center["x"], center["y"])
        await asyncio.sleep(0.4)
    props = await session.js(
        """(() => {
            const panel = document.querySelector('.node-props');
            if (!panel) return null;
            const input = panel.querySelector('input');
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            input.focus();
            setter.call(input, '主管审批');
            input.dispatchEvent(new Event('input', {bubbles: true}));
            return {title: panel.querySelector('.node-props__title')?.innerText, name: input.value};
        })()"""
    )
    show("属性面板改名", props)
    await asyncio.sleep(0.4)

    # ⑤ 保存
    before = await node_rects(session)
    await session.js(
        """(() => {
            const btn = [...document.querySelectorAll('.flow-editor__bar button')].find((b) => b.innerText.trim() === '保存');
            if (!btn) return false;
            btn.click();
            return true;
        })()"""
    )
    saved = await session.wait_js(
        "(() => { const t = document.body.innerText; return t.includes('已保存') || !t.includes('未保存'); })()", timeout=15
    )
    await asyncio.sleep(0.8)
    show("保存回执", {"dirtyTagGone": bool(saved), "toast": await session.js("[...document.querySelectorAll('.t-message')].map((e)=>e.innerText)")})

    await session.press_key("Escape", "Escape", key_code=27)

    # ⑥ 刷新页面，验证图完整还原
    await session.call("Page.reload")
    await asyncio.sleep(1.0)
    await session.wait_js("document.querySelectorAll('.flow-node').length >= 4", timeout=20)
    await asyncio.sleep(1.2)
    after = await node_rects(session)
    edges_after = await edge_keys(session)
    show("刷新后", {"nodes": after, "edges": edges_after})
    print(
        "RESTORE_CHECK=",
        json.dumps(
            {
                "node_count_same": len(before) == len(after),
                "positions_same": all(
                    before[k]["transform"] == after.get(k, {}).get("transform") for k in before
                ),
                "edges": edges_after,
            },
            ensure_ascii=False,
        ),
    )
    # 名称是否持久化
    name = await session.js(
        f"""(() => {{
            const node = document.querySelector('.flow-node[data-node-key="{approvals[0]}"] .flow-node__label');
            return node ? node.textContent : null;
        }})()"""
    )
    print("NODE_LABEL_AFTER_RELOAD=", name)


if __name__ == "__main__":
    asyncio.run(main())
