# PDF 导出基础设施（PDF-01）

> 单据 PDF 的底座：常驻 Chromium（CDP `:9222`）+ HTML 模板 + Rust 渲染引擎。
> 本文是**装机器 / 排故障 / 加新模板**的唯一入口；14 项技术验证的实测数据与原始产物见
> `docs/evidence/20261108-pdf-infra/`。

## 1. 链路一句话

```
业务模块（发票 / 入库单 / 对账单）
  → template::render_statement(ctx)            # tera 渲染 HTML（编译期内嵌）
  → engine::render_pdf(html, &opts)            # 信号量（默认 3 并发）
      → 连接池取一条 CDP WebSocket
      → Page.setContent → 等 document.fonts 就绪 → Page.printToPDF → Vec<u8>
  → handler::pdf_response(bytes, filename)     # application/pdf + Content-Disposition
```

渲染核心是**外部常驻的 headless Chromium**（systemd 托管），后端进程内没有任何
PDF/CSS 引擎。理由：只有真浏览器才能兑现 CSS 分页规则（`thead` 每页重复、
`break-inside: avoid`、`tabular-nums`），而这些正是单据排版的全部需求。

## 2. 装机器（幂等脚本，测试机 145 / 生产机 144 同一套）

```bash
sudo bash scripts/pdf/install-pdf-infra.sh          # 安装 + 自检，退出码 0 才算成功
sudo bash scripts/pdf/install-pdf-infra.sh --check  # 只自检，不改系统（巡检/上线前用）
```

脚本做四件事，每一步都有 PASS/FAIL 输出：

| 步骤 | 内容 | 为什么必须 |
|------|------|-----------|
| ① ② | `fonts-noto-cjk` + `fonts-noto-cjk-extra` + `fontconfig` | 没有中文字体时 Chromium **不报错**，直接画方框 |
| ③ ④ | 从可变字体实例化 **静态 glyf** `Noto Sans SC` Regular/Bold 到 `/usr/share/fonts/truetype/noto-sc/` | 见 §7「字体与体积」，差 24 倍 |
| ⑤ ⑥ | `snap install chromium`（Ubuntu 24.04+ 的 deb 已是 snap 过渡包） | 容器里 snap 常卡 apparmor，脚本会 `apparmor_parser -r` 修 |
| ⑦ ⑧ | `chromium-pdf.service`（`:9222`，`RuntimeMaxSec=86400`） | 常驻进程；每天 03:00 自动重启防内存增长 |

**装完必须重启服务**才能让新字体生效（Chromium 进程会缓存 fontconfig 字体列表，
脚本第 ⑦ 步已 `systemctl restart`）。

## 3. 后端环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `PDF_CDP_URL` | `http://127.0.0.1:9222` | CDP 地址；也可写 `ws://…` 直连 |
| `PDF_MAX_CONCURRENCY` | `3` | 并发上限，硬裁剪到 1–8（越界告警） |
| `PDF_RENDER_TIMEOUT_MS` | `20000` | 单次渲染总超时（含建连接） |

不走 `config.toml`：`nik` 的 `AppConfig` 是框架强类型结构，二开代码无法加自定义段
（改 vendor 就是侵入底座）；`business/storage` 的 AES 主密钥同样走环境变量，保持一致。

## 4. 接口

三个接口都挂权限码 `business:pdf:render:export`，而迁移 `V0343` **只把这个码授予
`SUPER_ADMIN`**。`/render` 接受任意 HTML，等价于「让服务器渲染任意页面」，不能给普通角色。

> 空库首启时 `V0343` 里那条 `sys_role_menu` 插入是 **0 行**（`SUPER_ADMIN` 由 seed 在全部
> `V*` 之后才种，`V0020` 的监控权限点同理）。这不影响鉴权：超管走 `role_code` 短路。
> 已有数据的库升级时会正常插入 1 行，角色授权页可见该权限点。

### 4.1 `GET /api/business/pdf/health`

自检（运维巡检用）：返回 Chromium 版本、协议版本、并发上限、当前在渲数、累计成功/失败数、
最近一次耗时与体积。CDP 连不上时返回 **503 + `67001`**，消息里带 CDP 地址。

```bash
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8023/api/business/pdf/health
```

### 4.2 `POST /api/business/pdf/sample`

生成内置「对账单」样本（**不读数据库**，纯渲染）。验收、压测、字体排查都用它。

```json
{"rows": 500, "page": "A4", "landscape": false, "doc_title": "对 账 单"}
```

全部字段可选；`rows` 裁剪到 1–5000。响应是 `application/pdf`（二进制，**不是**统一响应体）。

### 4.3 `POST /api/business/pdf/render`

任意 HTML → PDF（排障 / 联调口）。

```json
{"html": "<!DOCTYPE html>…", "page": "A4", "margin_mm": 10, "scale": 1.0, "filename": "单据.pdf"}
```

`html` 上限 4MiB（超限 413 / `67005`），空串 400（CDP 对空文档会静默出一页空白）。

### 4.4 错误码（`67xxx` 段，前端 `error-code.ts` 按此映射）

| 码 | HTTP | 含义 | 排障动作 |
|----|------|------|---------|
| 67001 | 503 | 渲染服务不可用 | `systemctl status chromium-pdf` / `curl 127.0.0.1:9222/json/version` |
| 67002 | 504 | 渲染超时 | 看是否并发打满；单张不该超（500 行实测 332ms） |
| 67003 | 422 | 模板渲染失败 | 模板语法 / 缺变量，属调用方要修 |
| 67004 | 400 | 参数非法 | 纸张代码 / 尺寸 / 边距 / 缩放越界 |
| 67005 | 413 | 请求体过大 | HTML 超 4MiB |

前端取 PDF 必须 `responseType: 'blob'`，失败分支按 `content-type` 判断是 JSON 错误体。

## 5. 14 项技术验证（实测结论，2026-11-08 · 145 测试机）

| # | 项 | 结果 | 实测证据 |
|---|----|------|---------|
| 1 | `chromium --version` | ✅ | `Chromium 157.0.8103.38 snap`（`/snap/bin/chromium`） |
| 2 | 中文字体 + `fc-match` | ✅ | `fc-list :lang=zh` 80 条；`fc-match "Noto Sans CJK SC"` → `NotoSansCJK-Regular.ttc`；静态字体 `fc-match "Noto Sans SC"` → `NotoSansSC-Regular.ttf` |
| 3 | `--dump-dom about:blank` | ✅ | 输出 `<html><head></head><body></body></html>`，rc=0 |
| 4 | `curl :9222/json/version` | ✅ | `Chrome/157.0.8103.38`，`Protocol-Version 1.3` |
| 5 | 中文不是方框 | ✅ | 正文 PDF 字体对象 `NotoSansSC-Regular/Bold  CID TrueType  emb=yes`；`pdftotext` 抽出完整中文；渲染 PNG 目视无方框 |
| 6 | 500 行跨页 + 表头重复 | ✅ | 14 页，**14/14 页**都含表头（`thead{display:table-header-group}`） |
| 7 | 合计行 / 签字栏不断裂 | ✅ | 合计行与三栏签字（制单/审核/客户确认）**同在第 14 页**；`break-inside: avoid` |
| 8 | 金额右对齐 + 等宽数字 | ✅ | bbox 取证：金额列右边界恒定（481.17 / 562.66），`61.70` 类金额字宽恒为 23.57pt |
| 9 | 单页 < 500ms | ✅ | 20 行热态 **154–170ms**（5 次），首请求含建连 267ms |
| 10 | 500 行 < 2s | ✅ | **405–435ms**（5 次）；样本上限 5000 行（132 页）3.85s |
| 11 | 并发 3 内存稳定 | ✅ | 3 并发各 0.499/0.508/0.527s（wall 0.535s）；chromium PSS 空载 129MiB → 峰值 **528MiB** → 3 轮后回落 **130MiB**，无累积 |
| 12 | < 200KB/页 | ✅ | 500 行 14 页 1,265,627B = **88.3KB/页**；20 行单页 98,057B |
| 13 | Win / Mac / 手机可打开 | ⚠️ 间接验证 | 本机无真机。已证：PDF 1.4、`Tagged=yes`、无加密 / 无 JS / 0 嵌入附件、字体全嵌入、`qpdf --check` 无语法/流错误、Ghostscript（独立解析链）复渲 exit=0、Poppler 光栅化成功、A4 595.92×841.92pt。**真机仍需老六在 Windows/Mac/手机各点开一次** |
| 14 | 中文标点 / 全角 / 大写金额 | ✅ | 末页实测 `人民币壹拾柒万柒仟捌佰壹拾叁元柒角伍分（￥177,813.75）`、`ＡＢＣ１２３ 全角空格（含税）《对账单》——顿号、书名号、破折号、省略号……`（大写由 `amount_upper` 计算，13 个边界用例单测覆盖） |

> 逐项原始输出、样例 PDF 与光栅图见 **`docs/evidence/20261108-pdf-infra/`**
> （`README.md` 是 14 项对照表，`raw/01`–`raw/12` 是命令原始输出）。

> ⚠️ 第 13 项必须诚实标注：容器环境没有 Windows/Mac/移动端设备，只能做到「PDF 结构 +
> 字体嵌入 + 第二解析器复渲」三重间接证明。它对 Type3 方框、缺字、字体未嵌入这类
> 「换机器就打不开」的故障是有效拦截；但请运维在真机上各点开一次样例 PDF。

## 6. 性能与内存（145：4 核 / 7.4GB / arm64）

| 场景 | 耗时 | PDF 体积 | 内存（PSS） |
|------|------|----------|-------------|
| 空载（服务已起、无请求） | — | — | 129 MiB |
| 20 行 / 1 页 | 154–170 ms | 98 KB | — |
| 500 行 / 14 页 | 405–435 ms | 1.21 MB（88 KB/页） | — |
| 3 并发 × 500 行 | 各 0.50–0.53 s（wall 0.535 s） | — | 峰值 528 MiB |
| 连跑 3 轮后 | — | — | 130 MiB（回落到空载水平，无泄漏） |
| 5000 行 / 132 页（样本上限） | 3.85 s | 12.5 MB（95 KB/页） | — |

> 数据来源：`docs/evidence/20261108-pdf-infra/raw/07_timing.txt`、`raw/08_memory.txt`
> （145 机器 4 核 / 7.4GB，实例 :8077，chromium 严格 3 并发）。

`MemoryMax=2G` 写在 unit 里但**对 snap 无效**（chromium 跑在 snap 自己的 scope，
不在 service cgroup 内），所以真实的保护是：并发信号量（3）+ 每日 `RuntimeMaxSec` 重启。
监控口径用 `ps`/`smaps_rollup` 的 PSS 而不是 `systemctl show MemoryCurrent`。

## 7. 字体与体积（最重要的一条坑）

| 字体来源 | 轮廓/形态 | Chromium 嵌入方式 | 20 行对账单体积 |
|---------|----------|------------------|----------------|
| `fonts-noto-cjk`（NotoSansCJK-*.ttc） | CFF + 字体集合 | **每个字形一个 Type3 字体对象** | 311 KB |
| 可变字体 VF（NotoSansSC[wght].ttf） | glyf + 可变 | Type3（Skia 不支持 VF 子集） | 38 KB（小页）/ 311KB 级 |
| **静态 glyf TTF（本仓用法）** | glyf + 单实例 | **CID TrueType 子集** | **13–99 KB** |

结论：模板里 `font-family` **必须首选 `"Noto Sans SC"`**（脚本 ③ 装的就是它），
`"Noto Sans CJK SC"` 只能作为兜底。判断线上是否踩坑：

```bash
# 出现大量 Type 3 就是踩坑了（正常应该只有两三行 CID TrueType）
pdffonts 某份导出.pdf
```

## 8. 加新单据模板（PDF-02 起）

1. 新建 `backend/src/business/pdf/templates/<单据>.html.tera`，复制 `statement.html.tera` 的
   `<style>` 段（`@page` / `thead` / `break-inside` / `.num` 四条约束**不要改**），只改 body 结构；
2. 在 `template.rs` 里加 `include_str!` 常量 + 一个 `XxxContext` + `render_xxx()`；
   金额一律用 `format_money(fen)` / `amount_upper(fen)`，**不要用 f64**；
3. 调用方：查库 → 组上下文 → `render_xxx` → `engine::render_pdf(&html, &opts)` → `handler::pdf_response`；
4. 单测：模板能编译、关键 CSS 规则字符串存在、用户数据被转义（参考 `template.rs` 的
   `item_content_is_escaped`）。

## 9. 排障速查

| 现象 | 原因 | 处理 |
|------|------|------|
| 503 `67001` | chromium 没起 / 端口改了 | `systemctl restart chromium-pdf`；`journalctl -u chromium-pdf -n 50` |
| PDF 中文全是方框 | 没装中文字体，或装完没重启 chromium | 跑 `--check`；`systemctl restart chromium-pdf` |
| PDF 单页 300KB+ | 落到了 `.ttc`（Type3） | 确认 `fc-match "Noto Sans SC"` 命中 NotoSansSC；模板 font-family 首选它 |
| 表头只在第一页 | 模板里用 `<div>` 模拟表头，或 `thead` 被 `display:block` 破坏 | 用真 `<table><thead>`，CSS 写 `display: table-header-group` |
| 合计/签字栏被劈开 | 缺 `break-inside: avoid` | 整块加 `.no-break { break-inside: avoid }` |
| 金额小数点不对齐 | 只右对齐没开等宽数字 | `.num { font-variant-numeric: tabular-nums; text-align: right }` |
| 首屏偶发空白页 | 用了 `prefer_css_page_size` 且模板 `@page` 与参数冲突 | 后端参数优先（默认 `prefer_css_page_size=false`） |
