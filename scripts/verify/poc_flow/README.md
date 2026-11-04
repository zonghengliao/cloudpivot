# POC-FLOW 验证脚本

三个画布方案 Demo（`frontend/public/demo/flow-poc/`）的自动化验证。

## 前置

本地起静态服务（`public` 作为根，这样 `/demo/flow-poc/` 路径与线上一致）：

```bash
cd frontend/public && python3 -m http.server 8099 --bind 127.0.0.1
```

`playwright-core` 用项目里 pnpm 装的那份即可；若当前 shell 解析不到，用绝对路径覆盖：

```bash
export PLAYWRIGHT_MODULE=/opt/cloudpivot/frontend/node_modules/.pnpm/playwright-core@1.64.1/node_modules/playwright-core/index.mjs
```

浏览器内核用 `~/.cache/ms-playwright` 里已下载的（chromium / webkit 都有）。

## 跑

```bash
# 冒烟：4 个页面 × 2 引擎，抓报错 / 节点数 / 边数 / 节点坐标 / 截图
node scripts/verify/poc_flow/shoot.mjs http://127.0.0.1:8099/demo/flow-poc local

# 交互：拖节点 / 滚轮缩放 / 空白平移 + 自研旧写法在 WebKit 下的塌陷复现
node scripts/verify/poc_flow/interact.mjs http://127.0.0.1:8099/demo/flow-poc
```

对线上跑就把 baseUrl 换成 `http://172.18.10.144/demo/flow-poc`。

## 判读

- 冒烟：`pos=<去重坐标数>/<节点数>` 必须是 `6/6`；`errors=0`。
  去重数变少 = Safari 那种「节点塌到同一处」的回归。
- 交互：`drag.nodesMoved` 应为 1（只动被拖的那个），`zoom.after > zoom.before`，`pan` 应接近 -90。
- legacy：`layoutRects` 与正常模式坐标一致但截图错位 —— 这正是 WebKit 的「布局对、绘制错」。
