"""D5 前端真机验证的 CDP 驱动（复用 headless-chrome-ui-verification 技能的打法）。

用法：在终端里 `uv run --with websockets python3 d5_verify.py <步骤名>`。
凭证只从 144 的 /etc/cloudpivot/cloudpivot.env 读进内存，任何日志都不打印它。
"""

import json
import subprocess
import sys
import time
import urllib.request

import asyncio
import websockets

APP = "http://172.18.10.144"
HOST = "root@172.18.10.144"


# ---------------------------------------------------------------- 基础连接

def devtools_port() -> int:
    """从进程表里找 browser_exec 起的 headless Chrome 的调试端口。"""
    for _ in range(40):
        ps = subprocess.run(["ps", "-eo", "args"], capture_output=True, text=True).stdout
        for line in ps.splitlines():
            if "remote-debugging-port=0" in line and "--user-data-dir=" in line and "--type=" not in line:
                for token in line.split():
                    if token.startswith("--user-data-dir="):
                        try:
                            with open(f"{token.split('=', 1)[1]}/DevToolsActivePort") as fh:
                                return int(fh.readline().strip())
                        except FileNotFoundError:
                            pass
        time.sleep(0.5)
    raise SystemExit("找不到 headless chrome")


def page_target(port: int) -> str:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list") as response:
        targets = json.load(response)
    pages = [item for item in targets if item.get("type") == "page"]
    for item in pages:
        if "172.18.10.144" in item.get("url", ""):
            return item["webSocketDebuggerUrl"]
    if pages:
        return pages[0]["webSocketDebuggerUrl"]
    raise SystemExit("没有可用的页面 target")


class Session:
    def __init__(self) -> None:
        self.messages: list[dict] = []
        self.pending: dict[int, dict] = {}
        self.next_id = 1
        self.ws = None

    async def connect(self) -> None:
        port = devtools_port()
        self.ws = await websockets.connect(page_target(port), max_size=64 * 1024 * 1024)
        asyncio.create_task(self._pump())
        await self.call("Page.enable")
        await self.call("Runtime.enable")
        # 隐藏标签页会让 Vue Transition / 路由失效（技能 §3）
        await self.call("Page.bringToFront")
        try:
            await self.call("Page.setWebLifecycleState", state="active")
        except Exception:
            pass

    async def _pump(self) -> None:
        async for raw in self.ws:
            data = json.loads(raw)
            if "id" in data:
                self.pending[data["id"]] = data
            else:
                self.messages.append(data)

    async def call(self, method: str, **params):
        call_id = self.next_id
        self.next_id += 1
        await self.ws.send(json.dumps({"id": call_id, "method": method, "params": params}))
        for _ in range(2000):
            if call_id in self.pending:
                data = self.pending.pop(call_id)
                if "error" in data:
                    raise RuntimeError(f"{method} 失败：{data['error']}")
                return data.get("result", {})
            await asyncio.sleep(0.01)
        raise TimeoutError(method)

    async def js(self, expression: str):
        result = await self.call(
            "Runtime.evaluate",
            expression=expression,
            returnByValue=True,
            awaitPromise=True,
            userGesture=True,
        )
        if result.get("exceptionDetails"):
            raise RuntimeError(f"JS 异常：{result['exceptionDetails'].get('text')} :: {result['exceptionDetails'].get('exception', {}).get('description', '')}")
        return result.get("result", {}).get("value")

    async def wait_js(self, expression: str, timeout: float = 12.0, interval: float = 0.15):
        deadline = time.time() + timeout
        while time.time() < deadline:
            value = await self.js(expression)
            if value:
                return value
            await asyncio.sleep(interval)
        return None

    async def goto(self, path: str) -> None:
        url = path if path.startswith("http") else f"{APP}{path}"
        await self.call("Page.navigate", url=url)
        await asyncio.sleep(0.6)
        await self.wait_js("document.readyState === 'complete'")

    # -------------------------------------------------- 输入与鼠标

    async def click_xy(self, x: float, y: float) -> None:
        for kind, extra in (("mouseMoved", {}), ("mousePressed", {"button": "left", "buttons": 1, "clickCount": 1}),
                            ("mouseReleased", {"button": "left", "buttons": 0, "clickCount": 1})):
            await self.call("Input.dispatchMouseEvent", type=kind, x=x, y=y, **extra)
            await asyncio.sleep(0.02)

    async def click_selector(self, selector: str) -> bool:
        box = await self.js(
            f"""(() => {{
                const el = document.querySelector({json.dumps(selector)});
                if (!el) return null;
                el.scrollIntoView({{block: 'center'}});
                const r = el.getBoundingClientRect();
                return {{x: r.left + r.width / 2, y: r.top + r.height / 2, w: r.width, h: r.height}};
            }})()"""
        )
        if not box:
            return False
        await self.click_xy(box["x"], box["y"])
        return True

    async def pointer_drag(self, x0: float, y0: float, x1: float, y1: float, steps: int = 12) -> None:
        """pointer 事件拖拽（画布用 pointerdown/move/up，不是 HTML5 DnD）。"""
        await self.call("Input.dispatchMouseEvent", type="mouseMoved", x=x0, y=y0)
        await self.call(
            "Input.dispatchMouseEvent", type="mousePressed", x=x0, y=y0, button="left", buttons=1, clickCount=1
        )
        await asyncio.sleep(0.05)
        for step in range(1, steps + 1):
            x = x0 + (x1 - x0) * step / steps
            y = y0 + (y1 - y0) * step / steps
            await self.call(
                "Input.dispatchMouseEvent", type="mouseMoved", x=x, y=y, button="left", buttons=1
            )
            await asyncio.sleep(0.02)
        await asyncio.sleep(0.1)
        await self.call(
            "Input.dispatchMouseEvent", type="mouseReleased", x=x1, y=y1, button="left", buttons=0, clickCount=1
        )
        await asyncio.sleep(0.15)

    async def type_text(self, text: str) -> None:
        await self.call("Input.insertText", text=text)

    async def press_key(self, key: str, code: str = "", modifiers: int = 0, key_code: int = 0) -> None:
        await self.call(
            "Input.dispatchKeyEvent",
            type="keyDown",
            key=key,
            code=code or key,
            windowsVirtualKeyCode=key_code,
            nativeVirtualKeyCode=key_code,
            modifiers=modifiers,
        )
        await self.call(
            "Input.dispatchKeyEvent",
            type="keyUp",
            key=key,
            code=code or key,
            windowsVirtualKeyCode=key_code,
            nativeVirtualKeyCode=key_code,
            modifiers=modifiers,
        )
        await asyncio.sleep(0.12)


# ---------------------------------------------------------------- 登录

def admin_password() -> str:
    """从 144 的 env 文件读管理员密码（只在内存里用，绝不打印）。"""
    output = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", HOST, "grep -m1 '^ALUN_ADMIN_PASSWORD=' /etc/cloudpivot/cloudpivot.env"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    return output.split("=", 1)[1]


async def login(session: Session) -> dict:
    password = admin_password()
    await session.js("localStorage.clear()")
    await session.goto("/login")
    await session.wait_js("!!document.querySelector('input')")
    # 账号
    await session.js(
        """(() => {
            const el = document.querySelectorAll('input')[0];
            el.focus();
            const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
            setter.call(el, 'admin');
            el.dispatchEvent(new Event('input', {bubbles: true}));
        })()"""
    )
    await session.js("document.querySelectorAll('input')[1].focus()")
    await session.type_text(password)
    await session.js(
        """(() => {
            const btns = [...document.querySelectorAll('button')];
            const target = btns.find((b) => b.type === 'submit') || btns[btns.length - 1];
            target.click();
        })()"""
    )
    ok = await session.wait_js(
        "!!localStorage.getItem('x_access_token') && !location.pathname.endsWith('/login')", timeout=15
    )
    return {"logged_in": bool(ok), "url": await session.js("location.href")}


# ---------------------------------------------------------------- 断言小工具

async def snapshot(session: Session) -> dict:
    return await session.js(
        """(() => ({
            url: location.href,
            title: document.title,
            bodyText: document.body.innerText.slice(0, 4000),
            nodeCount: document.querySelectorAll('.flow-node').length,
            edgeCount: document.querySelectorAll('.flow-edge').length,
        }))()"""
    )


def show(label: str, payload) -> None:
    print(f"--- {label} ---")
    print(json.dumps(payload, ensure_ascii=False)[:4000])
