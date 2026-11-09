# E2E-03 库存域全流程演练（145 测试机 · cloudpivot_dev）

> 环境：145 `http://172.18.10.145`（nginx 80 → 后端 127.0.0.1:8033），库 `cloudpivot_dev`，
> 账号 `admin`（密码取 `/etc/cloudpivot/cloudpivot-dev.env` 的 `NIK_ADMIN_PASSWORD`）；
> 审批人 `e2e03_wh`（WAREHOUSE_MANAGER，本卡自建，制单≠审批）。
> 部署：`ef90f11` → 145 `cargo build`（debug）→ `systemctl restart cloudpivot-test` → health 200（09:38:11）。
> 结论：**9/9 页面通畅**；13 条勾稽全绿；发现并当场修复 **1 个 P2**（自定义序列号过审批边界丢失），
> 另有 **1 个 P1**（库存巡检多桶假差异 + 修复路径会改错账面）登记待立项。

## 演练范围（9 个页面）

| # | 页面 | 覆盖接口 |
|---|---|---|
| ① | 仓库管理 | `POST/PUT/GET /business/erp/warehouse`、`/warehouse/tree`、`/warehouse/page`、`/warehouse/next-code` + 非叶子不可挂库位负向 |
| ② | 库位管理 | `POST/PUT/DELETE /business/erp/location`、`/location/page`、`/location/list`、`/location/{id}`、默认库位唯一/切换、软删 |
| ③ | 序列号管理 | `POST /business/erp/serial/batch-generate`、`/serial/page`、`/serial/scan`、`/serial/{id}`、`PATCH /serial/{id}/status` |
| ④ | 期初库存 | `POST /business/erp/opening-balance/inventory`、`GET /business/erp/opening-balance?period=` + FIX-6 对方科目可配 |
| ⑤ | 即时库存 | `GET /business/erp/inventory/page`、`/inventory/{id}`、`/inventory/audit` |
| ⑥ | 库位库存 | `GET /business/erp/inventory-location/page`、`/{id}` |
| ⑦ | 库存流水 | `GET /business/erp/inventory-txn/page`（物料 / 方向 / 单据类型过滤）、`/{id}` |
| ⑧ | 库存调拨 | `POST /business/erp/transfer`、`PATCH /{id}/status`（PENDING→APPROVED→IN_TRANSIT→DONE）、`/transfer/page`、`/{id}`、`/{id}/serials` |
| ⑨ | 盘点单 | `POST /business/erp/stock-take`（PARTIAL/FULL）、`PUT /{id}`（实盘）、`PATCH /{id}/status`（PENDING_POST→POSTED）、`/stock-take/page`、`/{id}` |

动账链路（真 HTTP + 真落库）：仓库/库位建档 → 序列号批量生成 → 期初建账 → 无源入库 →
仓间调拨 → 抽盘盘盈 → 全盘盘亏 → 领用出库。

## 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| step0 | 部门/仓管审批人/单位/供应商/物料（M1 数量管理、M2 期初、MS 一物一码） | 200 | `sys_user` / `biz_material` 落库；对账基线（全库 11 差异行、10 逐桶差异行） | ✅ |
| step1 ① | 建总仓 `E2E03-WH-P`(L1) → 子仓 A/B(L2)；树 2 子节点；改仓库；编码预览 `WH-07` | 200 | `biz_warehouse` 3 行，`warehouse_path=/P/`、`is_leaf` 随之翻转 | ✅ |
| step1 ① | 非叶子仓库建库位 → `400 + 66005`，DB 无行 | 400 | 无落库 | ✅ |
| step1 ② | A 仓 L01A(默认)/L02A、B 仓 L01B(默认)；临时库位置默认 → 旧默认自动让位 → 删临时 → 改回 | 200 | `biz_location.is_default` 恒唯一 | ✅ |
| step2 ② | 库位分页/下拉/详情/改名+容量/新增/软删 | 200 | 3→2 行，`deleted_at` 置位 | ✅ |
| step3 ③ | 非一物一码物料批量生成 → `400+66007` | 400 | — | ✅ |
| step3 ③ | 批量生成 3 个号 @50 → 库存 +3、`SERIAL_IN` 流水、凭证 150/150 | 200 | 两本账同落「未分配桶」（ADR-20261031 口径） | ✅ |
| step3 ③ | 入库带**自定义序列号** 2 件（`E2E03SN-9001/9002`） | 200 | 单据 POSTED，**号原样落库**、带来源单、落默认库位桶（修复后） | ✅（本卡修复） |
| step3 ③ | 建单号数≠数量 400；过账补号≠数量 400 且单据留 DRAFT（随后删单，不留死单） | 400 | DRAFT → 删除 | ✅ |
| step3 ③ | 入库不带号 → 自动取号 1 件；`qty == count(IN_STOCK)` = 6 | 200 | 不变式①成立 | ✅ |
| step3 ③ | 序列号出库 `IN_STOCK→OUTBOUND`（扣库存 + `SERIAL_OUT` + 原因入流水 remark）；非法回库 `OUTBOUND→IN_STOCK` 400 | 200/400 | 状态单向不可逆 | ✅ |
| step4 ④ | 配置 `inventory_opening_equity_account` 3103→3101 → 期初 100@10 → 凭证贷 **3101** | 200 | `biz_opening_inventory` 1 行 + `location_row_id` 落默认库位桶 | ✅（FIX-6） |
| step4 ④ | 配置改回 3103 → 同期补录 M2 20@5（M1 差额 0 不动） → 凭证贷 **3103** 1100/1100 | 200 | 整期覆盖按差额；无重复行 | ✅（FIX-6 双向） |
| step4 ④ | 期初数量 0 → 400 | 400 | — | ✅ |
| step5 | 无源入库 50@12（600<1000 免审直过）；`in_type=SOURCELESS`；桶与期初同一 `location_row_id`；凭证 600/600 | 200 | 库存 150 | ✅（FIX-4） |
| step5 | 无源入库缺供应商 → `400 + 66029` | 400 | — | ✅ |
| step6 ⑧ | 造「A 仓两桶并存」（L01 150 + L02 5），B 仓建桶 10 | 200 | 触发 P0-03 场景 | ✅ |
| step6 ⑧ | 同仓调拨 400 / 数量 0 400 | 400 | — | ✅ |
| step6 ⑧ | 调拨 40 **指定 from=A/L01 桶、to=B/L01 桶** → 审批 → 在途 → DONE | 200×5 | A/L01 110、A/L02 **仍 5（未被静默改扣）**、B/L01 50；总量 165 不变；两侧流水 426.6667 守恒；**不产生凭证**（同科目仓间移位） | ✅（P0-03 回归） |
| step7 ⑨ | 抽盘盘盈：账面快照 110 → 实盘 115（diff +5） | 200 | `diff_qty` 服务端生成 = 5 | ✅ |
| step7 ⑨ | 无附件提交审批 → 400「必须上传盘点表或盈亏说明」；传 png 后通过 → 审批 → 过账 | 400/200 | POSTED，库存 115 | ✅（附件闸） |
| step7 ⑨ | 盘盈流水 `STOCK_TAKE_GAIN` 5、凭证 `1405 / 1901` 53.33 平衡 | 200 | 落同一库位桶 | ✅ |
| step7 ⑨ | 全盘（FULL）快照 5 行（M1×2 桶、M2、MS×2 桶）→ M1 实盘 112（−3）→ 审批 → 过账 | 200 | 库存 112、`STOCK_TAKE_LOSS` 3、凭证 32.00 平衡、L02 桶未动 | ✅ |
| step8 | 领用出库 20（指定 A/L01 桶）→ POSTED | 200 | L01 92、`ISSUE_OUT` 20、结转成本凭证 213.33 平衡 | ✅ |
| step8 | 超量出库 99999 → 过账被拦 `409 + 66034`，单据留 DRAFT，库存不动 | 409 | 全库负库存 0 行 | ✅ |
| step9 ⑤⑥⑦ | 即时库存 / 库位库存 / 流水（含方向、单据类型过滤）/ 序列号 / 调拨 / 盘点 / 期初 / 巡检接口 | 200 | 25 条断言全绿 | ✅ |
| step10 | 全量对账（见下） | — | 本卡物料 0 差异；全库差异行数与基线一致 | ✅ |
| step11 | 历史回归：P0-03 / FIX-4 / FIX-7 | — | 4 条断言全绿 | ✅ |

## 勾稽验证

| 勾稽项 | 期望 | 实测 | 结论 |
|---|---|---|---|
| ① 库存表 qty = 流水累计（本卡物料，按 物料+仓库） | 0 差异行 | 0 行 | ✅ |
| ①b 全库差异行数（历史遗留） | 演练前后不变 | 11 → 11 | ✅ |
| ② 成本账 = 库位账（逐桶，本卡物料） | 0 差异行 | 0 行 | ✅ |
| ②b 全库逐桶差异行数 | 不变 | 10 → 10 | ✅ |
| ③ 库存 amount = 流水入−出累计（本卡物料） | 相等 | 1938.00 = 1938.00 | ✅ |
| ④ 流水自洽 `after−before = ±qty`（全库） | 0 异常 | 0 行 | ✅ |
| ⑤ 一物一码 `Σqty == count(IN_STOCK)`（本卡） | 0 违例 | 0 行（MS@A：5 = 5） | ✅ |
| ⑥ FIX-4 期初桶真实存在于成本账 | 0 违例 | 0 行（期初桶 = 收货桶同一 `location_row_id`） | ✅ |
| ⑦ FIX-7 销售出库流水 `biz_type` | 全 `SALE_OUT` | 违例 0（库内 2 张销售出库单） | ✅ |
| ⑧ 已过账单据必须有流水（入库/出库/盘点/调拨） | 0 缺流水 | 0 / 0 / 0 / 0 | ✅ |
| ⑨ 全库凭证借贷平衡 | 0 张不平 | 0 张（本卡 8 张并入校验） | ✅ |
| ⑩ 负库存 | 0 行 | 0 行 | ✅ |
| ⑪ 调拨金额守恒 | OUT = IN | 426.6667 = 426.6667，总量 165→165 | ✅ |

**期末口径核对**：M1@A 97（L01 92 + L02 5）、M1@B 50、MS@A 5；期初 100 + 入 50 + 入 5（第二桶）
− 调出 40 + 盘盈 5 − 盘亏 3 − 出库 20 = 97 ✅。

流水全轨迹见 `C_reconcile.log` 第 ⑪ 段（16 行，含 before/after 快照与成本）。

## 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| ISSUE-01 | **库存巡检（`GET /erp/inventory/audit`）对一物一码多桶物料报假差异，且 `dry_run=false` 的修复路径会改错账面**。`inventory_audit.rs` 的 `book_qty` 用 `COALESCE(MAX(qty) FILTER (WHERE location_id IS NULL), SUM(qty))`：只要存在「未分配桶」就**只取该桶数量**、忽略库位桶。FIX-4 之后「批量生成落未分配桶 + 收货落默认库位桶」是**常态**，于是 Σqty=5 被报成 2，diff=−3；执行修复会把账面改成汇总桶的值，**抹掉库位桶的 3 件** | **P1** | ①对一物一码物料批量生成 3 件（落未分配桶）②同仓入库 3 件（落默认库位桶）③`GET /erp/inventory/audit?dry_run=true` → `book=2 actual=6 diff=-4`；本卡实测 `E2E03-MS book=2.0000 actual=5.0000 diff=-3.0000` | 巡检恒假红（本次 `mismatched=2`，其中 1 条是本卡物料、1 条是别的演练夹具 `W2C2-M-SN`）；更危险的是 `dry_run=false` 会把正确账面改错（不可逆，需人工回修） | ①`book` 改为按 `(物料, 仓库)` `SUM(qty)`（与 `docs/ERP-库存对账口径.md` 一致）；②修复侧改为**差额**语义（`qty += actual − book` 记到汇总桶，不覆盖），或「多桶并存 → 标 `unfixable` 拒绝自动修」。需同步 ADR / 口径文档，故本卡未动代码 |
| ISSUE-02 | 一物一码「批量生成」不落默认库位桶：`POST /serial/batch-generate` 不传 `location_id` 时成本账与序列号都落「未分配桶」，而收货/期初落默认库位桶（ADR-20261031 明示，非缺陷）。前端批量生成表单（`serial-logic.ts::buildBatchPayload`）**不发 `location_id`** → 同物料同仓必然两桶并存：库位库存页显示「(未分配)」、扫码看不到库位、正是 ISSUE-01 的触发条件 | P3（口径） | 序列号管理页 → 批量生成（不选库位）→ 查 `/inventory-location/page`，该物料落在「未分配」 | 与「期初/收货同桶」的 FIX-4 口径不一致；不报错，属体验与口径问题 | 前端批量生成表单增加「入库库位」并默认带出仓库默认库位；或后端在不传 `location_id` 时按收货兜底（默认库位桶） |
| ISSUE-03 | 调拨**不产生凭证**（`biz_stock_transfer` 无 `voucher_id`、`biz_voucher` 无 `STOCK_TRANSFER` 来源） | P4（口径） | 调拨 DONE 后查 `biz_voucher where source_id=<transfer>` → 0 行 | 同科目（1405）仓间移位本来无 GL 影响，属合理设计；但报表若想按仓看金额只能看流水 | 在 `docs/ERP-库存对账口径.md` 明确「调拨不出凭证」的口径，避免巡检误判 |
| ISSUE-04 | 盘点单提交审批要求附件（`ATT` 规则 `CONFIRM`），前端未在提交前预校验 | P4（体验） | 盘点单不传附件直接 `PATCH /status PENDING_POST` → `400 10001「盘点单必须上传盘点表或盈亏说明」` | 报文清楚（可行动），但用户要先点一次才知道 | 提交按钮前置校验附件数（已有 `attachment` 查询接口） |
| ISSUE-05 | 全库既有差异未清：库存 vs 流水 11 行、逐桶 10 行、一物一码违例 1 条（`W2C2-M-SN`），均非本卡产生 | P3（存量） | `scripts/verify/e2e03_reconcile.sql` ①-2 / ②-2 / ⑤ | 全量巡检会带噪；本卡已按「前后不变」+「自有物料 0 差异」双口径验收 | 由数据治理卡单独清账（本卡只保证不增加） |

## 修复记录

**FIX（本卡，commit `ef90f11`，1 个源文件）—— 一物一码收货的自定义序列号过审批边界丢失**

- 现象：入库单提交过账时带 `serials`（供应商自带码），单据过账后序列号表里却是系统新取的 `SN-yyyyMMdd-NNNNNN`，用户扫的码**静默丢弃**、无任何提示。
- 根因：`stockin_handler_post::submit_post_approval` 里 `let _ = request_serials(req, &items)?;` **只校验不保存**；真正的过账发生在审批回调
  `post_after_approval_with_reason` → `posting_inputs(id, None)`，那条路径没有 HTTP 请求体 ⇒ `provided` 恒空 ⇒ 走「自动取号」分支。
  （`stock_in_post` 流程已发布，所以**每条**收货单都走这条路径，包括 <1000 的免审直过。）
- 修法（不改表结构 —— 明细表无序列号列、V0137–V0145 是红线）：
  1. 提交时把校验过的 `line_no → serial_ids` 冻进审批快照 `biz_flow_instance.form_data.serials`；
  2. 过账回调先读快照（`approval_serials` / `serials_of_form`），取不到才回落到自动取号（老单据口径不变）。
- 验证：145 部署后重跑 step3 —— `显式序列号原样落库 E2E03SN-9001/9002 = 2`、`挂上本单来源 = 2`、`落库位非空 = 2`，
  库存 3→5、`PURCHASE_IN` 流水 2、`qty == count(IN_STOCK)`=6 全绿；`cargo check --lib` 通过、`cargo fmt --check` 通过、
  `cargo clippy --lib` 只剩 3 处**既有**告警（`invoice.rs:1967` / `receivable.rs:800` / `sales_order_model.rs:699`，与本卡无关）。

## 复现文件

| 文件 | 内容 |
|---|---|
| `scripts/verify/e2e03_inventory_common.sh` | 公共库（登录 / 401 自愈 / 接口封装 / 库存与凭证取数 / 收货过账 / 附件上传） |
| `scripts/verify/e2e03_inventory_a.sh` | ① 仓库 · ② 库位 · ③ 序列号 · ④ 期初（step0–step4） |
| `scripts/verify/e2e03_inventory_b.sh` | 入库补货 · ⑧ 调拨 · ⑨ 盘点（盘盈/盘亏）· 出库（step5–step8） |
| `scripts/verify/e2e03_inventory_c.sh` | ⑤⑥⑦ 只读巡检 + 全量对账 + 历史回归（step9–step11） |
| `scripts/verify/e2e03_reconcile.sql` | 对账 SQL（①库存=流水 ②逐桶 ③金额 ④流水自洽 ⑤序列号 ⑥FIX-4 ⑦FIX-7 ⑧单据-流水 ⑨凭证 ⑩负库存 ⑪全轨迹） |
| 本目录 `A0-A4.log` / `B5-B8.log` | 各步原始输出（含每步 HTTP + DB 断言） |
| 本目录 `C_all.log` | 只读巡检 + 对账 + 回归（46 条断言 PASS） |
| 本目录 `C_reconcile.log` | 对账 SQL 全量输出（0 差异 + 全轨迹） |
| 本目录 `98_step8_recheck.txt` | step8 两条断言期望值笔误的修正后复核 |
| 本目录 `00*_probe.sql` / `01*_probe.sql` / `02*_probe.sql` | 前置探查（角色、流程节点、期初期间、列结构、既有基线） |
| 本目录 `90_reset_serial_drill.sql` | 演练自清理（只删本卡自造的 E2E03-MS 链数据，可重跑） |
| 本目录 `95_deploy_145.sh` | 145 部署（build + restart + health 轮询） |

## 数据副作用

- 演练数据全部带 `E2E03-` / `E2-03` 标记（仓库 `E2E03-WH-P/A/B`、库位 `E2E03-L01A/L02A/L01B`、物料 `E2E03-M1/M2/MS`、供应商、部门、审批人 `e2e03_wh`、单据 remark）。
- **未** DROP / TRUNCATE，**未**删任何他人演练数据，**未**改已应用迁移。
- 复现步骤：145 上 `bash /tmp/e2e03/e2e03_inventory_a.sh all` → `e2e03_inventory_b.sh all` → `e2e03_inventory_c.sh all`
  （step3 可用 `90_reset_serial_drill.sql` 重置后重跑；step6/step7 含绝对数量断言，整链一次跑通即可）。
- `sys_config.inventory_opening_equity_account` 演练中改到 `3101` 后**已改回 `3103`**（step4 断言覆盖）。
- 取号器被推进若干号段（全局单行，无功能影响）。

## 结论

**全部通畅**（9/9 页面）；13 条勾稽全绿；1 个 P2 当场修复并部署复验；
1 个 P1（库存巡检多桶假差异 + 修复路径会改错账面）已定位到代码行并给出修法，因涉及对账口径与 ADR 变更，登记为独立卡处理。
