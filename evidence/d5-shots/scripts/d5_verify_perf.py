"""验收 4：拖拽不掉帧 —— 用 PerformanceObserver('longtask') 抓拖拽期间的长任务。"""

import asyncio
import json

from d5_cdp import Session, login
from d5_step2 import node_center, node_rects


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        await login(session)
    if "/edit" not in await session.js("location.pathname"):
        await session.goto("/flow/def")
        await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
        await asyncio.sleep(1.0)
        await session.js(
            """(() => {
                const row = [...document.querySelectorAll('.t-table__body tr')].find((tr) => tr.innerText.includes('D5'));
                const link = row ? [...row.querySelectorAll('.t-link')].find((a) => a.innerText.trim() === '编辑') : null;
                if (link) link.click();
                return !!link;
            })()"""
        )
        await session.wait_js("location.pathname.includes('/edit')", timeout=20)
        await session.wait_js("document.querySelectorAll('.flow-node').length >= 2", timeout=20)
        await asyncio.sleep(1.5)

    # 若当前版本已发布（只读），先另存草稿再测拖拽
    ports = await session.js("document.querySelectorAll('.flow-node__port').length")
    if ports == 0:
        await session.js(
            """(() => {
                const btn = [...document.querySelectorAll('button')].find((b) => b.innerText.includes('另存为新草稿'));
                if (btn) btn.click();
                return !!btn;
            })()"""
        )
        await session.wait_js("document.querySelectorAll('.flow-node__port').length > 0", timeout=20)
        await asyncio.sleep(1.0)

    await session.js(
        """(() => {
            window.__longtasks = [];
            try {
                window.__po = new PerformanceObserver((list) => {
                    for (const entry of list.getEntries()) window.__longtasks.push({dur: Math.round(entry.duration), name: entry.name});
                });
                window.__po.observe({type: 'longtask', buffered: false});
            } catch (e) { window.__longtaskErr = String(e); }
            return true;
        })()"""
    )

    rects = await node_rects(session)
    key = next(iter(rects))
    center = await node_center(session, key)
    start = {"x": center["x"], "y": center["y"]}

    # 一次持续拖拽：60 步位移，模拟真实手速（约 1.5s）
    await session.call("Input.dispatchMouseEvent", type="mouseMoved", x=start["x"], y=start["y"])
    await session.call(
        "Input.dispatchMouseEvent", type="mousePressed", x=start["x"], y=start["y"], button="left", buttons=1, clickCount=1
    )
    frames = 60
    for step in range(1, frames + 1):
        x = start["x"] + step * 3
        y = start["y"] + step * 2
        await session.call("Input.dispatchMouseEvent", type="mouseMoved", x=x, y=y, button="left", buttons=1)
    await session.call(
        "Input.dispatchMouseEvent", type="mouseReleased", x=start["x"] + frames * 3, y=start["y"] + frames * 2,
        button="left", buttons=0, clickCount=1,
    )
    await asyncio.sleep(0.8)

    result = await session.js(
        """(() => ({
            longtasks: window.__longtasks || [],
            err: window.__longtaskErr || null,
            frames: (() => { try { return window.__po ? 'observed' : 'no-po'; } catch (e) { return 'err'; } })(),
            moved: document.querySelector('.flow-node')?.getAttribute('transform') || null,
        }))()"""
    )
    print(json.dumps(result, ensure_ascii=False))
    long_tasks = [t for t in result["longtasks"] if t["dur"] > 50]
    print("结论：长任务(>50ms) 条数 =", len(long_tasks), " 最长 =", max([t["dur"] for t in result["longtasks"]], default=0), "ms")

    # 再测 rAF 帧率（同一段拖拽内的帧间隔）
    fps = await session.js(
        """(async () => {
            const stamps = [];
            return await new Promise((resolve) => {
                let last = performance.now();
                const tick = () => {
                    const now = performance.now();
                    stamps.push(now - last);
                    last = now;
                    if (stamps.length < 60) requestAnimationFrame(tick);
                    else resolve({avg: +(stamps.slice(5).reduce((a, b) => a + b, 0) / (stamps.length - 5)).toFixed(2), max: +Math.max(...stamps).toFixed(2)});
                };
                requestAnimationFrame(tick);
            });
        })()"""
    )
    print("空转 rAF 帧间隔:", fps)


asyncio.run(main())
