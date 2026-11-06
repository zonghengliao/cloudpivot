# SO-20 销售发票后端 API + 开票（证据）

日期：2026-11-06　｜　卡：`t_16ccd7b6`　｜　前置：SO-19（`V0295__sales_invoice.up.sql`，提交 `91d206b`）

## 1. 交付内容（文件 / 函数）

| 文件 | 性质 | 说明 |
|---|---|---|
| `backend/src/business/erp/invoice.rs` | **新增 2210 行** | 发票 4 接口 + 开票/作废编排 + 14 条单测 |
| `backend/src/business/erp/mod.rs` | 改 | `pub mod invoice;` + 模块表行 |
| `backend/src/business/shared/doc_kind.rs` | 改 | `DocKind::Invoice`（ALL 22→23 / as_str / label / table / is_document / route）+ `VoucherSourceType::Invoice`（ALL 16→17）+ `DOC_KIND_VALUES` / `VOUCHER_SOURCE_VALUES` |
| `backend/src/business/shared/state_machine.rs` | 改 | `ENTITY_INVOICE` + `INVOICE_EDGES`(4) / `INVOICE_BASE` / `INVOICE_REASON_EDGES`(2) / `INVOICE_LOCKED`(3) + `Entity::Invoice` 七个注册点（ALL 18→19 + 单测断言） |
| `backend/src/business/shared/doc_kind_tests.rs` | 改 | `voucher_source_target_table` 补 `INVOICE → biz_invoice` |
| `docs/architecture/snapshot-drift/10_checks.sql` | 改 | 巡检登记表补 `VOUCHER_SOURCE.INVOICE` + `STATUS_ENTITY.INVOICE` |
| `frontend/src/api/business/erp/doc-kinds.ts` | 改 | 词表镜像：`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES` / `VOUCHER_SOURCE_TYPES` / `VOUCHER_SOURCE_TYPE_LABELS` 各补 `INVOICE` |
| `backend/migrations/business/V0296__sales_invoice_permissions.{up,down}.sql` | 新增 | 4 个权限码（list/detail/create/status）：1 页面节点 + 3 按钮，path/component 留 NULL、`is_visible=FALSE`（前端页 SO-21 落地后 UPDATE） |

接口（ARCH-7 §5.6 的 4 个，权限码与 `V0296` 逐字一致）：

| 方法 | 路径 | 权限码 | 关键行为 |
|---|---|---|---|
| POST | `/api/business/erp/invoice` | `business:erp:invoice:create` | 从应收开票（金额/客户/来源自动带出、锁应收判额度）或手工开票；落 `DRAFT`；`invoice_no` 为空才取 `IV-yyyyMMdd-000001` |
| GET | `/api/business/erp/invoice/{id}` | `business:erp:invoice:detail` | 客户名 / 应收单号 / 应收金额 / `voucher_no` / 剩余可开票额度 |
| GET | `/api/business/erp/invoice/page` | `business:erp:invoice:list` | 分页（status / invoice_type / customer / receivable / 单号+客户名模糊 / 日期区间，全部校验不静默忽略） |
| PATCH | `/api/business/erp/invoice/{id}/status` | `business:erp:invoice:status` | `ISSUED`（收入凭证 + 回写应收）/ `POSTED`（归档）/ `VOID`（红冲 + 撤回应收发票号，必须给原因） |

编排入口（`invoice.rs`）：
`create` → `resolve_amounts` / `validate_sign` / `validate_headroom`（锁应收 `FOR UPDATE` + `invoiced_total`）→ `voucher::next_doc_no`；
`change_status` → `state_machine::transition` → `post_invoice_voucher`（借 1122 1130 / 贷 5001 1000 / 贷 2221.02 130）→ `writeback_receivable`；
`VOID` → `reverse_invoice_voucher`（`voucher::reverse_posted`，红字凭证 `PZR-…`）→ `clear_receivable_writeback`。

## 2. 设计口径（与稿面的差异，逐条给理由）

1. **`ISSUED` 就生成收入凭证**（不是 `POSTED`）。ARCH-7 §4.5 只给 `DRAFT/ISSUED → VOID` 两条作废边；若凭证到 `POSTED` 才生成，`VOID` 永远没有「可红冲的蓝字凭证」，卡面「作废时生成红字凭证」无从落地。⇒ `ISSUED` = 开票（凭证 + 回写应收），`POSTED` = 归档终态（不产生第二张凭证）。
2. **收入科目取 `5001` 不取稿面的 `6001`**：本仓 `biz_account` 的损益类是 `5xxx`（`V0183` 种的是 `5001 主营业务收入`），`6001` 在科目表里根本不存在，用它会 `account_required` 直接报「科目不存在」。销项税用叶子科目 `2221.02`（`2221` 是汇总科目，记账会被 `post_voucher` 的非末级闸拒）。
3. **`VOID` 从 `DRAFT` 来时不红冲**（账上没凭证，无从冲）；从 `ISSUED` 来必红冲，并把 `biz_receivable.invoice_no/invoice_date` 按号撤回（只清「确实指回本发票」的行）。
4. **`POSTED` 不可作废**（稿面 §4.5 无此边，`409 + 10003`，文案给出去向「无（终态）」）。
5. **额度口径 = 价税合计**：`SUM(total_with_tax) WHERE receivable_id=? AND status<>'VOID'`（作废票不占额度），建单与 `ISSUED` 各判一次（同一把应收行锁串行化）。
6. **手工建红字票不开放**（`400`，文案指路退货流程 `ARCH-7 §15.2.3` 与作废接口）；表层仍允许 `OUT_RED` + 负金额（`ck_biz_invoice_sign`）。

## 3. 门禁实测

### 3.1 编译 / 单测

```
cargo check --lib                  → Finished，零警告
cargo test --lib business::erp::invoice::tests        → 14 passed; 0 failed
cargo test --lib business::shared::doc_kind           → 21 passed; 0 failed
cargo test --lib business::shared::state_machine      → 39 passed; 0 failed
```

单测覆盖：状态域 / 票种域 / 四边注册 + BASE/REASON/LOCKED 一致 / DocKind+VoucherSource 注册 / 税额与合计互算（含除不尽尾差、0 税率）/ 金额任意组合（6 分支 + 不自洽 + 空）/ 税率边界 / 合计上限 / 正红票符号 / 额度（刚好开满、分次、超额 1 分、用完再开、库层漂移）/ 发票号形状 / 来源归一 / 期间与日期形状 / 原因长度 / 科目常量。

### 3.2 迁移（隔离库 `cloudpivot_so20`）

```
versioned | 295 | sales_invoice             | executed | success=true | 23ms | 982a773f1aca0aee（与 SO-19 一致）
versioned | 296 | sales_invoice_permissions | executed | success=true | 1ms  | 0ae75e721bfe9e33
数据库迁移完成: 应用 224 个迁移 → 启动完成 2210ms
GET /health ×2 → http=200 code=0
```

（`boot-and-migration.txt`）

### 3.3 curl 实测（真实 token / 真实库 / 真凭证，两轮同库连跑）

```
round-1-curl.out.txt → PASS=70 FAIL=0
round-2-curl.out.txt → PASS=70 FAIL=0
```

覆盖：从应收开票带出金额/客户/来源、`IV-` 取号形状、分页与过滤器、超额开票两档被拒（已开满再开 1 元 / 226.01 > 226）、分次开票恰好开满、金额三列不自洽、负金额、手工建红字票、客户不存在、详情、`ISSUED` 生成凭证（分录逐行打印：1 借1122 1130 / 2 贷5001 1000 / 3 贷2221.02 130，借贷各 1130）、回写应收 `invoice_no/invoice_date`、作废生成红字凭证（`PZR-20261106-000001`、原凭证 `reversed_by` 置位、分录方向相反）、撤回应收发票号、作废后额度恢复 1130、`VOID` 终态拒绝再开票、`ISSUED→POSTED` 归档不产生第二张凭证、`POSTED→VOID` 无此边 409、手工开票（手填税控号 `25337000000099887766` 原样保留、税 260/合计 2260）、手填号重复 409。

库层汇总（第二轮）：`biz_invoice` DRAFT 2 / POSTED 1 / VOID 1；`biz_voucher` 蓝字 2 + 红字 1；`biz_status_log(INVOICE)` 8 条；借贷不平凭证 0 张。

### 3.4 前端

```
pnpm typecheck → 通过（vue-tsc --noEmit 无输出）
pnpm build     → 见 build.txt
```

## 4. 复跑方式

```bash
su - postgres -c "dropdb --if-exists cloudpivot_so20; createdb -O alun cloudpivot_so20"
bash docs/evidence/20261106-so20-invoice/start-instance.sh          # 起实例 :18123
bash docs/evidence/20261106-so20-invoice/curl-4-endpoints.sh        # 两轮各 70 断言
```

## 5. 已知边界（交给后续卡）

- 红字**发票单据**（`OUT_RED` 行）本卡不生成 —— 作废走「红字凭证」；单据级红字发票属退货流程（ARCH-7 §15.2.3），待退货卡落地时复用同一 `ck_biz_invoice_sign` 口径。
- 发票无改单接口（`DRAFT` 也只能作废重开）：改金额会让 `ISSUED` 的凭证与单据金额漂移，审计不可接受。
- `post_voucher` 的期间闸用「开票日期所在期间」；作废红字凭证用**作废当天**的期间（跨期作废在旧期已结账时仍允许，这是红冲的正确口径）。
