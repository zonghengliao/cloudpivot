"""D5 验收 10–11 + 17：流程列表（筛选 / 停用）、发起页流程预览、部署指纹。

验收 12 / 13（待办四 Tab + 详情进度条）由 `d5_verify_instance.py` 用真实审批单验证
（D2 交付后已经能走完「发起 → 待办 → 同意 → 已通过」全链路）。
"""

import asyncio
import json

from d5_cdp import Session, login

RESULTS: list[tuple[str, bool, str]] = []


def check(label: str, ok: bool, detail: str = "") -> None:
    RESULTS.append((label, ok, detail))
    print(f"[{'PASS' if ok else 'FAIL'}] {label} {detail}")


async def main() -> None:
    session = Session()
    await session.connect()
    if not await session.js("!!localStorage.getItem('x_access_token')"):
        await login(session)

    # ---------- ⑩ 流程定义列表 ----------
    await session.goto("/flow/def")
    await session.wait_js("!!document.querySelector('.flow-def-page')", timeout=15)
    await asyncio.sleep(1.2)
    rows = await session.js(
        """(() => [...document.querySelectorAll('.t-table__body tr')]
            .filter((tr) => tr.querySelector('.status-badge'))
            .map((tr) => [...tr.querySelectorAll('td')].slice(0, 6).map((td) => td.innerText.trim()).join(' | ')))()"""
    )
    check("⑩-1 流程列表显示定义（编码 / 名称 / 状态 / 版本 / 图规模）", len(rows) >= 1, json.dumps(rows[:2], ensure_ascii=False))

    # 关键词筛选
    await session.js(
        """(() => {
            const input = document.querySelector('.filter-card input');
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            input.focus();
            setter.call(input, 'D5');
            input.dispatchEvent(new Event('input', {bubbles: true}));
            return true;
        })()"""
    )
    await asyncio.sleep(0.3)
    await session.js(
        """(() => { const b = [...document.querySelectorAll('.filter-card button')].find((x) => x.innerText.includes('查询')); if (b) b.click(); return !!b; })()"""
    )
    await asyncio.sleep(1.5)
    filtered = await session.js(
        """(() => [...document.querySelectorAll('.t-table__body tr')].filter((tr) => tr.querySelector('.status-badge')).length)()"""
    )
    check("⑩-2 关键词筛选生效", filtered >= 1, f"过滤后数据行={filtered}")

    # 状态筛选：已停用
    await session.js(
        """(() => { const s = document.querySelector('.filter-card .t-select input'); if (s) { s.focus(); s.click(); } return !!s; })()"""
    )
    await asyncio.sleep(0.6)
    picked = await session.js(
        """(() => {
            const opts = [...document.querySelectorAll('.t-select-option')];
            const target = opts.find((o) => o.innerText.trim() === '已停用');
            if (!target) return false;
            target.click();
            return true;
        })()"""
    )
    await asyncio.sleep(0.4)
    await session.js(
        """(() => { const b = [...document.querySelectorAll('.filter-card button')].find((x) => x.innerText.includes('查询')); if (b) b.click(); return !!b; })()"""
    )
    await asyncio.sleep(1.5)
    offline_rows = await session.js(
        """(() => [...document.querySelectorAll('.t-table__body tr')].filter((tr) => tr.querySelector('.status-badge')).map((tr) => tr.querySelector('.status-badge').innerText.trim()))()"""
    )
    check(
        "⑩-3 状态筛选（已停用）只返回已停用的行",
        picked and all(badge == "已停用" for badge in (offline_rows or [])),
        json.dumps(offline_rows, ensure_ascii=False),
    )

    # 复原筛选
    await session.js(
        """(() => { const b = [...document.querySelectorAll('.filter-card button')].find((x) => x.innerText.includes('重置')); if (b) b.click(); return !!b; })()"""
    )
    await asyncio.sleep(1.6)

    # ---------- ⑪ 发起页：流程预览 ----------
    await session.goto("/flow/start")
    await session.wait_js("!!document.querySelector('.flow-start-page')", timeout=15)
    await asyncio.sleep(2.2)
    # 手动选一个流程（库里有多张已发布流程时不会自动选中，正好验选择 + 预览联动）
    await session.js(
        """(() => { const input = document.querySelector('.start-form .t-select input'); if (input) { input.focus(); input.click(); } return !!input; })()"""
    )
    await asyncio.sleep(0.8)
    picked_flow = await session.js(
        """(() => {
            const opts = [...document.querySelectorAll('.t-select-option')];
            if (!opts.length) return false;
            opts[0].click();
            return true;
        })()"""
    )
    await asyncio.sleep(2.0)
    start_state = await session.js(
        """(() => ({
            nodes: document.querySelectorAll('.flow-canvas .flow-node').length,
            edges: document.querySelectorAll('.flow-canvas .flow-edge').length,
            steps: [...document.querySelectorAll('.preview__step')].map((e) => e.innerText.replace(/\\n/g, ' ').trim()),
            empty: !!document.querySelector('.preview__state'),
            selected: document.querySelector('.start-form .t-select input')?.value || null,
        }))()"""
    )
    check(
        "⑪ 发起页选流程 → 只读预览图 + 步骤链正确",
        picked_flow
        and start_state["nodes"] >= 2
        and start_state["edges"] >= 1
        and len(start_state["steps"]) >= 2
        and not start_state["empty"],
        json.dumps(start_state, ensure_ascii=False),
    )

    # ---------- ⑰ 部署指纹 ----------
    deploy = await session.js(
        """(async () => {
            const res = await fetch('/', {cache: 'no-store'});
            const html = await res.text();
            const match = html.match(/assets\\/index-[A-Za-z0-9_-]+\\.js/);
            let chunkOk = false;
            let chunkSize = 0;
            if (match) {
                const chunk = await fetch('/' + match[0], {cache: 'no-store'});
                chunkSize = (await chunk.text()).length;
                chunkOk = chunk.status === 200;
            }
            return {index: match ? match[0] : null, chunkOk, chunkSize};
        })()"""
    )
    check(
        "⑰ 生产 nginx 提供新构建产物（index.html + 主 chunk 可下载）",
        bool(deploy and deploy["chunkOk"]),
        json.dumps(deploy, ensure_ascii=False),
    )

    print("\n== 汇总 ==")
    for label, ok, _ in RESULTS:
        print(("PASS " if ok else "FAIL ") + label)
    print("失败数：", sum(1 for _, ok, _ in RESULTS if not ok))


asyncio.run(main())
