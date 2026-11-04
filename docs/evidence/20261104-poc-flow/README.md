# 证据 · POC-FLOW 三方案画布对比（2026-11-04）

线上地址（真机可访问，nginx → `/opt/cloudpivot/frontend/dist`）：

- 入口：<http://172.18.10.144/demo/flow-poc/>
- ① 自研：<http://172.18.10.144/demo/flow-poc/demo-native.html>
- ② Vue Flow：<http://172.18.10.144/demo/flow-poc/demo-vue-flow.html>
- ③ LogicFlow：<http://172.18.10.144/demo/flow-poc/demo-logicflow.html>

## 一、可达性（实测 HTTP 200，prod 172.18.10.144）

```
/demo/flow-poc/                   200  text/html          （目录 → index.html）
/demo/flow-poc/index.html         200  text/html
/demo/flow-poc/demo-native.html   200  text/html
/demo/flow-poc/demo-vue-flow.html 200  text/html
/demo/flow-poc/demo-logicflow.html 200 text/html
/demo/flow-poc/shared-data.js     200  application/javascript
/demo/flow-poc/shared-style.css   200  text/css
/demo/flow-poc/vendor/*.js        200  application/javascript
```

## 二、双引擎冒烟（Chromium 141 + WebKit 2370，视口 1280×760）

`shots/report.json`

| 引擎 | 页面 | 节点数 | 边数 | 节点位置去重 | 控制台报错 |
|---|---|---|---|---|---|
| Chromium | demo-native | 6 | 6 | 6/6（无塌陷） | 0 |
| Chromium | demo-vue-flow | 6 | 6 | 6/6 | 0 |
| Chromium | demo-logicflow | 6（SVG） | 6 | 6/6 | 0 |
| WebKit | demo-native | 6 | 6 | 6/6 | 0 |
| WebKit | demo-vue-flow | 6 | 6 | 6/6 | 0 |
| WebKit | demo-logicflow | 6（SVG） | 6 | 6/6 | 0 |

「节点位置去重 6/6」= 六张卡各有各的屏幕坐标，即 Safari 那种「全塌到左上角」没有发生。

## 三、交互（`shots/interact.json`，两个引擎都跑）

| 方案 | 拖节点 | 滚轮缩放 | 空白平移 | 报错 |
|---|---|---|---|---|
| 自研 | 1 个节点位移 108px | 1.00 → 1.82 | -90px | 0 |
| Vue Flow | 1 个节点位移 97px | 1.00 → 1.74 | -90px | 0 |
| LogicFlow | 1 个节点位移 97px | 1.00 → 1.82 | -90px | 0 |

（Vue Flow / LogicFlow 有拖拽阈值或吸附，位移略小于鼠标位移。）

## 四、视觉一致度

`compare-triptych.png`（三行堆叠）、`compare-side-by-side.png`（三列并排）、`compare-node-zoom.png`（单卡片 3× 放大）

像素差（只比画布主体区域，Chromium 与 WebKit 各一组）：

| 对比 | 平均差 | 差异>40 像素占比 |
|---|---|---|
| 自研 vs Vue Flow | 0.58 / 255 | 0.28% |
| 自研 vs LogicFlow | 2.15 / 255 | 1.77% |

## 五、Safari / WebKit：旧写法塌陷复现

`compare-safari-legacy.png`、`shots/webkit-native-legacy.png`、`shots/chromium-native-legacy.png`

- 自研 demo 右上角开关切到「旧写法 `<g transform>`」后：
  - **Chromium**：卡片与连线仍然对齐（这个坑只在 WebKit）；
  - **WebKit**：卡片整体错位（被画在未变换的坐标），连线留在原地 ——
    但 `getBoundingClientRect()` 仍然全部正确（`interact.json` → `legacy.layoutRects` 的 6 个坐标与正常模式完全一致）。
- 现写法（平移缩放用 `<svg>` 自己的 CSS transform）在两个引擎都正常。

> 说明：WebKit 是 Playwright 1.64.1 的 `webkit-2370`（Safari 内核），不是真机 Safari；
> 真机 Safari 上建议再人工确认一眼（尤其说明卡的 `backdrop-filter`）。

## 六、包体积（`curl` 下载 + `wc -c`，gzip = `gzip -9`）

| 方案 | 文件 | 字节 | gzip |
|---|---|---|---|
| ① | demo-native.html | 12,774 | 4.7 KiB |
| ② | vendor/vue-flow.bundle.js | 290,655 | 99.8 KiB |
| ② | vendor/vue-flow.css | 8,039 | 1.6 KiB |
| ③ | vendor/logicflow.bundle.js | 393,597 | 98.4 KiB |
| ③ | vendor/logicflow.css | 2,833 | 0.9 KiB |

合计：Vue Flow **291.7 KiB**（gzip 101.4 KiB）／LogicFlow **387.1 KiB**（gzip 99.3 KiB）／自研 **12.1 KiB**（gzip 4.5 KiB，零依赖）。

## 七、复现方式

```bash
# 本地起静态服务（public 根）
cd frontend/public && python3 -m http.server 8099
# 打开 http://127.0.0.1:8099/demo/flow-poc/

# 抓图/冒烟/交互（脚本在任务工作目录，用项目里的 playwright-core）
node shoot.mjs <baseUrl> <tag>
node interact.mjs <baseUrl>
```

线上更新（POC 是纯静态，不需要跑 pnpm build）：

```bash
cd frontend/public && tar czf /tmp/flow-poc.tgz demo/flow-poc
scp /tmp/flow-poc.tgz root@172.18.10.144:/tmp/
ssh root@172.18.10.144 'cd /opt/cloudpivot/frontend/public && tar xzf /tmp/flow-poc.tgz \
  && cd /opt/cloudpivot/frontend/dist && tar xzf /tmp/flow-poc.tgz'
```

> 目录同时放在 `frontend/public/demo/flow-poc/`（下次 `vite build` 会带进 dist）和线上 `dist/demo/flow-poc/`（当前立即可访问）。
