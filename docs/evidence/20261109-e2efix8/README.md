# E2E-FIX-8【P4】杂项 5 条 —— 真机证据（t_e220697e）

> 仓库：`/opt/cloudpivot`（99 = 工作仓）· 真机：`172.18.10.145`（`cloudpivot-test.service` · `:8033` · 库 `cloudpivot_dev`）
> 后端 HEAD = **69fbf1e1**（`git ls-remote origin refs/heads/main` 与本地一致）· 迁移 **V0361** 已应用
> 采集时间：2026-11-09 16:05（CST）· 原始输出见 `raw-145/`、`raw-99/`

---

## 0. 一句话结论

| # | 问题 | 结论 | 证据 |
|---|---|---|---|
| ① | 无源入库同时挂应付 | **口径 B：视同采购，挂应付**（不改代码）。依据：「无源」= 无采购单，不是无供应商；期初走 `opening`、销售退货走 `sales_return_posting` | 本文件 §1 · docs §5.1 |
| ② | 退款单没有 `approval_instance_id` | **加列（V0361）+ 列表 LEFT JOIN**：一条 SQL 带出 `approval_status` / `approval_instance_no` / `approval_status_label` | §2 · `raw-145/refund_page.json` |
| ③ | 汇率接口参数名不一致 | **参数对齐 `from_currency` / `to_currency`**（`currency_code` 保留为废弃别名）+ **未知参数 400/10001** | §3 · `raw-145/quote_*.json` |
| ④ | 调拨不出凭证 | **合理设计**，写进 `docs/ERP-库存对账口径.md` §7，附 3 条可抄审计 SQL | §4 |
| ⑤ | 盘点单提交前未预检附件 | 详情按钮闸门已有（ATT-09）；本卡补**弹窗「提交审批」前重查 + 禁用 + 红字** | §5 · `raw-145/att_*.json` |

---

## 1. ① 无源入库挂应付：定口径 B（不改代码，写文档 + 注释指针）

**结论**：`is_sourceless=true` 的入库单过账后**照旧**生成 `借 存货/进项税、贷 2202 应付` 与一张应付单。

三条依据：

1. **「无源」= 没有采购单，不是没有供应商**：`ck_biz_si_supplier_scope`（V0330）要求
   `PURCHASE` / `SOURCELESS` 必须有 `supplier_id`；DTO 层对无源单还强制 `remark`
   （AP-4 `66027` / AP-5 `66029`）。所以「欠款给谁」有答案：单头供应商，凭证贷方辅助核算也挂它。
2. **期初不走这张表**：库存期初是 `POST /opening-balance/inventory`（`biz_opening_inventory`，
   凭证 `借 1405 / 贷 3103`，`opening.rs`），不生成应付。所以「期初类无源入库不挂应付」的前提不成立。
3. **不挂应付会把缺陷转成人工**：只借 1405 不贷记负债 → 试算平衡缺口 + 应付账龄/现金流各漏一笔真实负债。

**真机反证（145 `cloudpivot_dev`）**：

```text
in_no                  in_type     is_sourceless  supplier                payable_no            amount   status  voucher_no
IN-20261109-000256     SOURCELESS  t              E2E-03 演练供应商       AP-20261109-000209     60.0000  OPEN    PZ-20261109-000508
  凭证分录：PZ-20261109-000508  line1 1405 借 60.0000 / line2 2202 贷 60.0000
IN-20261109-000255     SOURCELESS  t              E2E-03 演练供应商       AP-20261109-000208    120.0000  OPEN    PZ-20261109-000507
```

对照（同一库）销售退货回库走**另一条事务**、**不挂应付**：

```text
IN-20261109-000243  RETURN  POSTED  凭证 PZ-20261109-000485：1405 借 8.0000 / 6401 贷 8.0000；无应付行
```

**落地位置**：`docs/ERP-库存对账口径.md` §5.1（含选路表）、`stockin_handler_post.rs` ⑦ 注释指针。
**没有改代码** —— 因为要在 145 上改行为就得重跑全部财务口径与巡检，而现口径本身自洽。

---

## 2. ② 退款单 `approval_instance_id`（V0361）

**迁移** `V0361__receipt_refund_approval_instance`：
`ADD COLUMN IF NOT EXISTS approval_instance_id UUID` + 按「同一 (biz_type, biz_id) 最近一条实例」
回填 + 部分索引 + 自检块（列存在 / 不得指向非 `RECEIPT_REFUND` 实例 / 孤儿实例只 NOTICE）。

**写入**：`receipt_refund::start_approval` 在**同一事务**里 `UPDATE biz_receipt_refund SET approval_instance_id`
与实例同生共死，不会出现「实例建了、指针没落」。

**读**：列表 `LIST_FROM` 加 `LEFT JOIN biz_flow_instance fi ON fi.id = p.approval_instance_id AND fi.deleted_at IS NULL`，
一次查询带出三列；详情单头用 `latest_approval_of` 的同源摘要填充（零额外查询）。

**真机（145）**：

```json
GET /api/business/erp/receipt-refund/page?page=1&page_size=10 → HTTP 200
{"refund_no":"RR-20261109-000005","status":"POSTED",
 "approval_instance_id":"01a1bddb-c206-7268-b6e0-984ba9d1215d",
 "approval_instance_no":"FLW-20261109-90CF88CA",
 "approval_status":"APPROVED","approval_status_label":"已通过"}
DB 对照：RR-20261109-000005|t（approval_instance_id 非空）；RR-20261109-000004|t
库存回填：biz_receipt_refund 2 行 / approval_instance_id 非空 2 行
```

**真库单测（99 新建库 `cloudpivot_e2efix8`，全量迁移到 V0361）**：
`refund_approval_chain_posts_on_approve_and_stays_confirmed_on_reject` 现在额外断言
「提交后单头 `approval_status = PENDING` / 标签=审批中 / 列表 `approval_status = PENDING` 且带出单号」，
22/22 通过（`raw-99/db_tests_receipt_refund.txt`，耗时 6.6s = 真跑非 SKIP）。

**注**：仓库里没有退款单的前端列表页（`frontend/src` 无 `receipt-refund` 调用），
所以「列表页直接显示」= 列表接口已经给出字段；前端接入时不需要二次查询。

---

## 3. ③ 汇率预览：参数对齐 + 未知参数 400

**改动**：`QuoteQuery` 从 serde 结构体改为**显式解析**（`parse_quote_query(&HashMap)`）：

| 参数 | 语义 |
|---|---|
| `from_currency` | 要折的币种（缺省 = 本位币） |
| `to_currency` | 折成的币种；**只能是本位币**（交叉汇率不做，明确 400，不硬算未核过的倒数） |
| `date` | 取数日期 |
| `currency_code` | **废弃别名**（= `from_currency`；同时给两个且不一致 → 400） |
| 其它任何参数 | **400 `PARAM_INVALID`(10001)**，报文点名未知参数 + 列出白名单 |

响应新增 `from_currency` / `to_currency`，旧字段 `currency_code` / `base_currency` 保留（同值，向后兼容）。
前端 `quoteRate()` 改传 `from_currency`。

**真机（145 · 真实 token）**：

```
GET ?from_currency=USD                → 200 {"from_currency":"USD","to_currency":"CNY","exchange_rate":"7.10000000","source":"SPOT"}
GET ?from_currnecy=USD                → 400 {"code":10001,"msg":"未知查询参数 `from_currnecy`：汇率预览只接受 from_currency / to_currency / date（`currency_code` 是已废弃别名，等价于 from_currency）"}
GET ?currency_code=USD                → 200 {"from_currency":"USD",...,"source":"SPOT"}   ← 老脚本不炸
GET ?from_currency=USD&to_currency=HKD→ 400 {"code":10001,"msg":"to_currency 只支持账套本位币 `CNY`（当前 `HKD`）..."}
GET (无参数)                          → 200 {"from_currency":"CNY","exchange_rate":"1","source":"BASE"}
```

对照卡面现场：改之前 `?from_currency=USD` 被静默忽略 → `{currency_code:"CNY", exchange_rate:"1"}`（误判「汇率没维护」）。
现在**要么正确取到 7.1（SPOT），要么明确 400**。

---

## 4. ④ 调拨不出凭证（合理设计，写进口径文档）

`docs/ERP-库存对账口径.md` 新增 **§7**：结论、三条理由、三本账/流水对照表、3 条可抄审计 SQL、唯一例外（跨法人/内部加价调拨）。

结构证据：

```text
biz_voucher.source_type='STOCK_TRANSFER' 张数 = 0
biz_stock_transfer 是否有 voucher_id 列      = 0（没有）
调拨流水（doc_type='STOCK_TRANSFER'）不配对行数 = 0（6 行流水 = 3 张 DONE 单 × OUT/IN）
transfer_settle.rs / transfer_handler.rs 里 "voucher" 出现次数 = 0 / 0
VoucherSourceType::ALL 共 19 个来源，无 StockTransfer
```

---

## 5. ⑤ 盘点单提交前预检附件

**现场**：ATT-09（`436a55b9`，2026-11-08 22:35）已经给盘点详情页装了闸门
（`actionGateBlocked` + `AttachmentGateHint`）；E2E-03（11-09 09:42）测的是**接口直调**，
所以卡面写「前端未预校验」—— 当时 UI 侧其实已有闸门。

**本卡补的缺口**（弹窗路径原来没有二次校验）：

1. 打开「提交审批」弹窗时 `refreshAttachGate()` 重查一次；
2. `confirmTransition()` **提交前** `await refreshAttachGate()`，`resolved && !canProceed` → 不提交；
3. 弹窗里新增红字块 `stocktake-confirm-gate-hint`（文案取后端 `hint` 原文）+ 确认按钮 `:disabled`。

**真机（145 · 自建自删草稿 `ST-20261109-000027`）**：

```
建草稿（全盘，仓库 01a1bdc4-…）→ HTTP 200
附件数 = 0
GET /api/business/attachment/check?doc_type=STOCK_TAKE&doc_id=…&at=CONFIRM
  → 200 {"can_proceed":false,"at_label":"确认","hint":"缺少盘点表或盈亏说明（需 pdf/jpg/png 格式）"}
PATCH /status=PENDING_POST（不带附件）→ 400 {"code":10001,"msg":"盘点单必须上传盘点表或盈亏说明（pdf/jpg/png）：至少要 1 个，当前 0 个…"}
DELETE 草稿 → 200（不留夹具）
```

前端构建证据：145 `frontend/dist` 重建成功（`dist/assets/index-CfkxJ48A.js` 命中
`stocktake-confirm-gate-hint`，`dist/index.html` 时间 2026-11-09 16:04），99 侧 `pnpm typecheck` + `pnpm build` 均通过。

> **没做**浏览器点击走查：本环境禁止用自动化工具输入口令（登录表单），
> 而按钮禁用/红字的判据就是 `can_proceed=false` + `hint` 两项，两者已在真机拿到；
> 构建产物也确认包含新 testid。若需截图级证据，可由持可用会话的操作员补一次。

---

## 6. 检查链结果

| 项 | 结果 | 原始输出 |
|---|---|---|
| `cargo check --lib` | 通过（45.8s） | — |
| `cargo test --lib -j 1`（99，全量） | **3926 passed / 0 failed** | `raw-99/lib_tests_all.txt` |
| `ERP_DB_URL=…cloudpivot_e2efix8 cargo test --lib business::erp::receipt_refund -- --test-threads=1` | **22 passed**（6.6s，真跑） | `raw-99/db_tests_receipt_refund.txt` |
| 新库迁移链（空库 → V0361） | 359/360/361 全 `executed` | `raw-99/fresh_db_migrations.txt` |
| `cargo fmt -p cloudpivot -- --check` | 本卡 4 个 .rs 无 diff；**基线另有 2 处**（`sales_return_posting.rs:128`、`serial_handler.rs:198`，非本卡文件） | — |
| `cargo clippy --lib -- -D warnings` | 本卡文件无告警；**基线 3 处既有 error**（`invoice.rs:1912`、`receivable.rs:782`、`sales_order_model.rs:661`，`git status` 证明这三文件未被本卡修改） | — |
| `pnpm typecheck` / `pnpm build`（99） | 通过 | — |
| 145 部署 | HEAD `69fbf1e1`；`cargo build` 2m08s；`systemctl restart cloudpivot-test`；health 200；二进制命中新文案「未知查询参数」=1 | §7 |
| 145 验收脚本 | `scripts/verify/e2efix8_accept.sh` → **PASS=17 FAIL=0** | `raw-145/accept.log` |

145 迁移状态（部署脚本输出原文）：

```text
360|linkage_rules_coverage
361|receipt_refund_approval_instance
approval_instance_id|uuid
2|2            ← biz_receipt_refund：approval_instance_id 非空 2 行 / 共 2 行
```

---

## 7. 复跑方式

```bash
# 145（真机验收，17 项断言；自建自删一张 DRAFT 盘点单）
scp scripts/verify/e2efix8_accept.sh root@172.18.10.145:/tmp/ && ssh root@172.18.10.145 'bash /tmp/e2efix8_accept.sh'

# 99（真库单测；库 cloudpivot_e2efix8 = 空库 + 全量迁移）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_e2efix8' \
  cargo test --lib business::erp::receipt_refund -- --test-threads=1
```

---

## 8. 遗留 / 不在本卡范围

- ① 若产品以后确实要「无源不挂应付」，需要同时定贷方科目（3103 / 以前年度损益调整）、
  改 B2/B3 巡检口径、处理存量应付 —— 不是关一个分支（已写进 docs §5.1 警示块）。
- `other` 类型入库：收货 API 已不再落 `in_type=OTHER`（`in_type_of` 只出 PURCHASE/SOURCELESS），
  历史行若走 ⑦ 会被 `payable::create_for_stock_in` 的「必须有供应商」拦下，不会静默生成无供应商应付。
- 145 dev 库里 E2E-03 演练供应商的 9 张 SOURCELESS 单与对应应付仍在（别的卡的演练数据），本卡未动。
