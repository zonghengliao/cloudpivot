# PDF-01 技术验证证据（2026-11-08 · 145 测试机）

> 卡：`t_7997b30d`「PDF 基础设施：装 chromium + 中文字体 + CDP 服务 + 技术验证（14 项）」
> 提交：`9b00450`（主体）→ `89c59e6`（rustfmt + 字段名）→ `d0e8976`（负数金额）→ `af6a103`（大写金额附小写）
> 环境：172.18.10.145 / Ubuntu 26.04 / aarch64 / 4 核 / 7.4GB / Chromium 157.0.8103.38 (snap)
> 后端实例：隔离跑在 `127.0.0.1:8077`（库 `cloudpivot_pdf01`，全新空库自动迁移 276 条），
> **没有动** 145 上共享的 `cloudpivot-test.service`，也更没有碰 144 生产。

## 一、14 项逐条实测

| # | 验证项 | 结果 | 证据文件 | 关键实测值 |
|---|--------|------|---------|-----------|
| 1 | `chromium --version` | ✅ | `raw/01_chromium.txt` | `Chromium 157.0.8103.38 snap`；`chromium-pdf.service` active，`Restart=always`，`RuntimeMaxUSec=1d`（23h 后自动重启） |
| 2 | 中文字体 + `fc-match` | ✅ | `raw/02_fonts.txt` | `fc-list :lang=zh` = **85** 条；`fc-match "Noto Sans CJK SC"` → `NotoSansCJK-Regular.ttc`；`fc-match "Noto Sans SC"` → `NotoSansSC-Regular.ttf`（`fontformat: TrueType`） |
| 3 | `--headless --dump-dom` | ✅ | `raw/03_dumpdom.txt` | 输出 `<html><head></head><body></body></html>`，exit=0 |
| 4 | `curl :9222/json/version` | ✅ | `raw/04_cdp_version.json` | `Browser: Chrome/157.0.8103.38`，`Protocol-Version: 1.3`，ws 地址可用 |
| 5 | 中文不是方框 | ✅ | `raw/09_pdf_quality.txt`、`sample/page1-01.png` | PDF 内嵌字体只有 2 个对象：`NotoSansSC-Regular` / `NotoSansSC-Bold`，`type=CID TrueType`、`emb=yes sub=yes uni=yes`；`pdftotext` 抽出完整中文；PNG 目视无方框 |
| 6 | 500 行跨页 + 表头重复 | ✅ | `raw/09_pdf_quality.txt` | 14 页；**14/14 页**都含表头（`金额（元）` + `序号`）；每页行数 34/38/38/…/10 |
| 7 | 合计行 + 签字栏不跨页断裂 | ✅ | `raw/09_pdf_quality.txt`、`sample/pagelast-14.png` | 第 14 页同时含「行数合计 / 数量合计 / 金额合计 / 金额大写」与三栏签字（制单 / 审核 / 客户确认） |
| 8 | 金额右对齐 + 等宽数字 | ✅ | `raw/10_alignment.txt` | 68 个金额 token 的右边界只有 2 个取值（`481.17` 单价列、`562.66` 金额列）；5 字符金额字宽唯一值 `23.57pt`（tabular-nums） |
| 9 | 单页 < 500ms | ✅ | `raw/07_timing.txt`、`raw/05_http_e2e.txt` | 热态 20 行 **0.154–0.170s**（5 次），首请求（含建连）0.267s |
| 10 | 500 行 < 2s | ✅ | `raw/07_timing.txt` | 5 次 **0.405–0.435s**；另测样本上限 5000 行（132 页）3.85s |
| 11 | 并发 3 内存稳定不泄漏 | ✅ | `raw/08_memory.txt`、`raw/07_timing.txt` | 3 个并发各 0.499/0.508/0.527s，wall **0.535s**；chromium PSS：空载 129MiB → 峰值 **528MiB** → 3 轮后回落 **130MiB**（无累积） |
| 12 | PDF < 200KB/页 | ✅ | `raw/09_pdf_quality.txt` | 1,265,627 B / 14 页 = **88.3 KB/页**（20 行单页 98KB） |
| 13 | Win / Mac / 手机可打开 | ⚠️ **间接验证** | `raw/12_compat.txt` | 本机无真机。已证：PDF 1.4、`Tagged=yes`、`Encrypted=no`、`JavaScript=no`、`0 embedded files`、字体全嵌入、A4 595.92×841.92pt、`qpdf --check` 无语法/流错误、Ghostscript（独立解析链）复渲 exit=0、Poppler 光栅化成功。**真机仍需人工各点开一次** |
| 14 | 中文标点 / 全角 / 大写金额 | ✅ | `raw/11_text_extract.txt`、`sample/pagelast-14.png` | 末页实测：`金额大写 人民币壹拾柒万柒仟捌佰壹拾叁元柒角伍分（￥177,813.75）`、`ＡＢＣ１２３ 全角空格（含税）《对账单》——顿号、书名号、破折号、省略号……` |

**结论：13 项通过，1 项（#13 跨端打开）只能做到间接验证 —— 需要在 Windows（Edge/Adobe）、
Mac（预览）、手机上各点开 `sample/statement-500rows.pdf` 确认一次，其余技术项全部实测达标，
不需要降级方案（typst / 并发 1 + 缓存）。**

## 二、HTTP 端到端（真实 token）

`raw/05_http_e2e.txt`：`POST /api/system/auth/login` 拿到 461 字符 access_token 后

| 请求 | 结果 |
|------|------|
| `GET /api/business/pdf/health` | 200，0.012s，返回 `browser=Chrome/157.0.8103.38`、`protocol_version=1.3`、`max_concurrency=3`、`timeout_ms=20000` |
| `POST /api/business/pdf/sample {"rows":20}` | 200，0.267s，98,057 B，`content-type: application/pdf` |
| `POST /api/business/pdf/sample {"rows":500}` | 200，0.414s，1,265,627 B |
| 无 token 访问 | 401 `{"code":401,"msg":"未授权访问，请先登录"}` |

响应头（`raw/05_http_e2e.txt` 末尾）：`content-type: application/pdf`、
`content-disposition: attachment; filename="___-500_.pdf"; filename*=UTF-8''%E5%AF%B9%E8%B4%A6%E5%8D%95-500%E8%A1%8C.pdf`
（中文名走 RFC 5987）、`cache-control: no-store`、`x-content-type-options: nosniff`。

## 三、负向用例（`raw/06_negative.txt`）

| 入参 | HTTP | 业务码 | 消息 |
|------|------|--------|------|
| `page=B5` | 400 | 67004 | 纸张代码不支持：B5（可选 A3 / A4 / A5 / LETTER / CUSTOM） |
| `page=CUSTOM` 缺宽高 | 400 | 67004 | page=CUSTOM 时必须同时给出 width_mm 与 height_mm |
| `render` 空 html | 400 | 67004 | html 不能为空 |
| 无 token | 401 | — | 未授权访问，请先登录 |

## 四、产物

| 文件 | 说明 |
|------|------|
| `sample/statement-500rows.pdf` | **验收样例**：500 行对账单，14 页，含中文 / 跨页表头 / 合计行 / 签字栏 / 全角字符 / 大写金额（1.21MB） |
| `sample/statement-20rows.pdf` | 单页样例（98KB） |
| `sample/page1-01.png` / `page2-02.png` / `pagelast-14.png` | 第 1 / 2 / 末页 110dpi 光栅图（目视取证：无方框、表头重复、签字栏完整） |
| `raw/bbox-page1.xml` | 第 1 页 `pdftotext -bbox-layout` 原始坐标（对齐取证的底稿） |

> 5000 行样本（132 页 / 12.5MB）没有入库，体积太大；需要时
> `POST /api/business/pdf/sample {"rows":5000}` 一条命令可复现（3.85s）。

## 五、怎么复现

```bash
# ① 装机器（幂等；--check 只体检）
sudo bash scripts/pdf/install-pdf-infra.sh

# ② 起后端（任何实例都行，只要 PDF_CDP_URL 指向本机 9222）
export PDF_CDP_URL=http://127.0.0.1:9222 PDF_MAX_CONCURRENCY=3 PDF_RENDER_TIMEOUT_MS=20000

# ③ 取 token 后打三个接口
TOKEN=$(curl -s -X POST http://127.0.0.1:8023/api/system/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"<口令>"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["access_token"])')
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8023/api/business/pdf/health
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"rows":500}' http://127.0.0.1:8023/api/business/pdf/sample -o /tmp/statement500.pdf

# ④ 体检导出的 PDF（出现 Type 3 就是字体回退，体积会涨 20 倍以上）
pdffonts /tmp/statement500.pdf
pdfinfo /tmp/statement500.pdf | grep -E 'Pages|Page size|PDF version'
```

## 六、门禁（最终 commit `2dc0912` 上重跑）

| 检查 | 命令 | 结果 |
|------|------|------|
| 模块单测 | `cargo test --lib business::pdf` | **35 passed / 0 failed / 0 ignored** |
| clippy | `cargo clippy --lib` | `business/pdf` **0 告警**（全 lib 剩 6 条都是别的模块的既有告警：`unused import`） |
| 全量 lib 单测 | `cargo test --lib` | **3571 passed / 4 failed**；4 条全是**既有红**：`business::flow::linkage::{admin,tests_wiring}` ×3（LNK-12 触发点计数未跟上）+ `shared::startup_guard::tests::merge_semantics_or_base_and_profile`。这三个文件最后的改动者是 LNK-12（`6eb7699` / `e83e1f8`）与 `47043b6`，与本卡无关 |
| 迁移 | 空库首启 / 二次启动 | **276 条** / **0 条**（幂等）；`business:pdf:render:export` 权限点行存在（`menu_type=3`，空库首启 `sys_role_menu` 0 行，见 V0343 注释） |
| 安装脚本 | `bash scripts/pdf/install-pdf-infra.sh --check` | **8 项全 PASS** |

## 七、过程中发现并修掉的问题（都写进了代码注释）

1. **字体是体积的生死线**：`fonts-noto-cjk` 的 `NotoSansCJK-*.ttc`（CFF + 字体集合）被 Chromium
   的 Skia 退化成「每个字形一个 Type3 字体对象」，20 行对账单单页 **311KB**；换成从可变字体实例化
   出来的**静态 glyf `Noto Sans SC`** 后同一页 **13–98KB**（24 倍）。安装脚本因此多了一步
   「下载 VF → fonttools 实例化 → 装字体 → 重启 chromium」。
2. **snap 在容器里起不来**：报 `missing profile snap-update-ns.chromium`，
   脚本里 `systemctl start snapd.apparmor` + `apparmor_parser -r` 修好。
3. **字体缓存**：装完字体必须重启 chromium 进程，否则 Chromium 一直用旧的 fontconfig 快照
   （脚本第 ⑦ 步已包含 restart）。
4. **snap 的 chromium 不在 service cgroup 里**：`MemoryMax=2G` 对它**无效**，
   `systemctl show MemoryCurrent` 恒为 20KB。真实保护是并发信号量（3）+ 每日重启；
   监控要看 PSS（`raw/08_memory.txt` 就是这样取的）。
5. **`GetVersionReturns` 没有 `browser` 字段**（是 `product`），编译期踩到，见 `89c59e6`。
6. **`format_money` 负数**：`div_euclid/rem_euclid` 会把 `-12345` 分写成 `-124.55`（红字单据差
   1.1 元），单测抓到，见 `d0e8976`。
7. **既有红（与 PDF-01 无关，但挡住了 `cargo test`）**：`tests/pflow1a.rs` / `pflow1b.rs` 的
   `GraphNodeReq` 夹具漏了 V0341 新增的 `subprocess_def_id`，导致两个集成测试**编译不过**；
   已补 `None`（`0e3ef90` / `5162b75`，纯夹具、不改断言）。
