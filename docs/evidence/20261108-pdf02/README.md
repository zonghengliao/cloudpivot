# PDF-02 证据：采购订单 / 销售订单 PDF 模板（2026-11-08）

> 卡：`t_5e6411fb`「PDF 模板-1：采购订单 + 销售订单」
> 提交：`4e5f8c4`（主体）→ `ba64545`（真库用例收口）→ `f8c3670`（明细列宽收口）
> 环境：145 隔离实例 `127.0.0.1:8077`（库 `cloudpivot_pdf02`，空库首启迁移 **277** 条），
> 二进制 md5 `20e96a2a3998d7e7001ef8aab48efc59`（**99 干净 worktree @ `f8c3670` 构建后 scp 过去**，
> 不含任何他人未提交的 WIP）；**没有动** 145 的 `cloudpivot-test.service`（8033），也没碰 144 生产。
> 渲染侧：145 本机 `chromium-pdf.service`（`:9222`，Chromium 157.0.8103.38）+ 静态 glyf Noto Sans SC（PDF-01 装好的设施）。

## 一、验收 5 项（逐条实测）

| # | 卡面验收 | 结果 | 关键实测值 | 证据 |
|---|---------|------|-----------|------|
| ① | 3 行明细采购订单 → 单页正确 | ✅ | `PO-20261108-000004`，**1 页**，61,243 B，渲染 **195ms**；单据头 6 字段 / 九列明细 / 合计四要素 / 大写 / 5 个签字位全部在 | `sample/purchase-order-3rows.pdf`、`sample/po-p1-1.png`、`raw/pdftotext_po.txt` |
| ② | 50 行明细销售订单 → 跨页表头重复 | ✅ | `SO-20261108-000003`，**2 页**，185,311 B；**2/2 页都含表头**（物料编码/单价/金额），序号 34→50 连续（不是从 1 重来），50 行一行不少 | `sample/sales-order-50rows.pdf`、`sample/so-p1-1.png`、`sample/so-p2-2.png` |
| ③ | 金额大写正确（整数/带角/带分/零/大额） | ✅ | 单测 5 个边界逐例钉死；实测 PDF 里的「人民币肆佰贰拾壹元玖角玖分（￥421.99）」/「人民币伍佰陆拾伍元整（￥565.00）」与**脚本内独立实现**（Python 版人民币大写）逐字一致 | `raw/e2e_run.txt`、`raw/02_unit_tests_pdf02.txt` |
| ④ | 无权限账号导出 → 403 | ✅ | 新建 `PDF02_NO_PERM` 角色（不授任何菜单）+ 账号 `pdf02_noperm`：两个接口都 **403 `无权限访问，需要权限: business:erp:purchase-order:list` / `business:erp:sale-order:list`**（且账号本身能登录 —— 排除「401 混成 403」） | `raw/e2e_run.txt` |
| ⑤ | PDF 用浏览器打开不乱码 | ✅ | `pdffonts`：`AAAAAA+NotoSansSC-Bold CID TrueType Identity-H emb=yes sub=yes uni=yes`；`pdftotext` 能无损抽出全部中文（无方框）；`pdfinfo`：PDF 1.4 / Tagged / 无 JS / A4 595.9×841.9pt。**真机目视仍需人工点开一次**（同 PDF-01 #13 的口径） | `raw/pdffonts_po.txt`、`raw/pdfinfo_po.txt`、两张 PNG |

E2E 共 **43 项断言，0 失败**（`raw/e2e_run.txt`：`==== 汇总：43 项，失败 0 项 ====`）。

## 二、接口与响应头（真实 token）

```
GET /api/business/erp/purchase-order/{id}/pdf   →  200 application/pdf
GET /api/business/erp/sales-order/{id}/pdf      →  200 application/pdf
```

PO 实测响应头：

```
Content-Type: application/pdf
Content-Disposition: attachment; filename="____-PO-20261108-000004.pdf";
                     filename*=UTF-8''%E9%87%87%E8%B4%AD%E8%AE%A2%E5%8D%95-PO-20261108-000004.pdf
Cache-Control: no-store
X-Content-Type-Options: nosniff
```

ASCII 回退名把中文换成 `_`（不破坏头语法），中文走 RFC 5987 的 `filename*`（浏览器优先采用）。

## 三、导出审计（operation_logs）

`GET` 不会被操作日志中间件记录（`constants::AUDITED_METHODS` 只有 POST/PUT/PATCH/DELETE），
所以导出成功后由 `order_pdf_model::write_export_log` 显式补一行（`raw/01_operation_logs.txt`）：

| operation | target_type | username | 单号 | 行数 | 价税合计 | 字节 | 文件名 |
|---|---|---|---|---|---|---|---|
| EXPORT_PURCHASE_ORDER_PDF | biz_purchase_order | admin | PO-20261108-000004 | 3 | 421.99 | 61,243 | 采购订单-PO-20261108-000004.pdf |
| EXPORT_SALES_ORDER_PDF | biz_sales_order | admin | SO-20261108-000003 | 50 | 565.00 | 185,311 | 销售订单-SO-20261108-000003.pdf |

- `request_method = 'GET'`、`status_code = 200`、`target_id` = 单据主键（可按单 join）；
- `detail`（jsonb）里还有 `doc_type` / `row_count` / `total_with_tax` / `bytes` / `duration_ms` / `source="PDF-02"`；
- **`ip` 为空**：本机 curl 直连（无 `X-Real-IP` / `X-Forwarded-For`），框架回退到 unspecified 被
  `utils::client_ip` 过滤掉 —— 中间件写的行在同一场景下也是空的，生产走 nginx 时才有值。这是已知口径，不是漏写。

## 四、门禁（本卡跑过的命令）

| 项 | 命令 | 结果 |
|---|---|---|
| 单元测试（模板 + 格式化） | `cargo test --lib business::pdf::` | **45 passed / 0 failed**（`raw/03_unit_tests_pdf.txt`） |
| 单元 + 真库测试（导出） | `ERP_DB_URL=… cargo test --lib business::erp::order_pdf -- --test-threads=1` | **17 passed / 0 failed**（10 纯逻辑 + 3 模型 + 4 真库；`raw/02_unit_tests_pdf02.txt`） |
| clippy | `cargo clippy --lib` | 本卡文件 **0 告警**（全 crate 仅 1 条既有：`sales_order_model::update_order` 参数过多；`raw/04_clippy.txt`） |
| rustfmt | `rustfmt +1.94.1 --edition 2024 --check <本卡 4 个 .rs>` | 无 diff |
| 全量 lib 套件 | `cargo test --lib` | **3612 passed / 3 failed**（`raw/06_full_lib_test.txt`）—— 3 条全是 **LNK-12 计数族既有红**：`linkage::tests_wiring` 的触发点白名单 / 真实调用点计数 / `catalog_triggers_carry_wired_flag`，差异来源是 `src/business/erp/purchase_return_handler.rs`（别的卡的采购退货触发点）没同步白名单，**与本卡文件无关**（本卡 4 个 .rs 一次 `linkage::engine` 都没调） |
| 迁移 | 空库首启 | **277** 条；二次启动 **0** 条（本卡**没有新增迁移**，只复用 V0343 起的 PDF 设施） |

## 五、产物

| 文件 | 说明 |
|------|------|
| `sample/purchase-order-3rows.pdf` | 验收①：3 行采购订单，1 页（61KB） |
| `sample/sales-order-50rows.pdf` | 验收②：50 行销售订单，2 页，跨页表头重复 + 页脚页码（185KB） |
| `sample/po-p1-1.png` / `so-p1-1.png` / `so-p2-2.png` | 100dpi 光栅图（目视取证：单据头 / 列宽 / 合计 / 签字栏 / 页码） |
| `raw/e2e_run.txt` | 43 项断言的完整输出（含 403、大写比对、页脚页码） |
| `raw/result.json` | 同一轮的结构化结果（单据 id / 单号 / 金额 / 每项断言） |
| `raw/pdftotext_po.txt` / `pdftotext_so.txt` | `pdftotext -layout` 全文（中文可抽取 = 不乱码） |
| `raw/pdffonts_po.txt` / `pdfinfo_po.txt` / `pdfinfo_so.txt` | 字体嵌入 / 文档属性 |
| `raw/01_operation_logs.txt` | 导出审计行（6 行，含 3 轮重跑） |
| `raw/05_instance_startup.txt` | 145 隔离实例启动 + 4 次渲染耗时（195 / 186 / 898 / 406 ms） |

## 六、怎么复现

```bash
# ① 145 起隔离实例（库 cloudpivot_pdf02，端口 8077；二进制来自 99 的干净 worktree）
bash /root/pdf02_setup145.sh     # 建库 + 写 env
bash /root/pdf02_run145.sh       # 起进程（cwd 必须是 /opt/cloudpivot/backend，否则读不到 config-dev.toml）
bash /root/pdf02_seed145.sql     # 主数据夹具（psql -f）
python3 /root/pdf02_e2e.py http://127.0.0.1:8077 /root/pdf02-evidence

# ② 本机跑本 card 的测试（真库用例需要 ERP_DB_URL；未设置时打印 SKIP）
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_pdf02' \
  cargo test --lib business::erp::order_pdf -- --test-threads=1
```

> ⚠️ 145 上 admin 首登强制改密（CODE-06），且 `must_change_password` 有 **60s 会话缓存** ——
> 改密后 60s 内所有业务接口都返回 403「首次登录必须先修改初始密码」。E2E 脚本为此显式等待 70s
> （`raw/e2e_run.txt` 里能看到两次等待日志），否则会把「必须改密」误判成「缺权限」。
