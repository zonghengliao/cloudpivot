# M3-17 证据：销售返利 —— 生成红字发票（OUT_RED）

卡 `t_bdcd2326` · 2026-11-17 · 分支 `main` · 迁移 `V0440`

## 卡面要求 → 本卡交付

| 卡面 | 落点 |
|:--|:--|
| 「生成应付或红字发票，**二选一**」 | 选**红字发票**：`biz_invoice` 的 `OUT_RED` 一支（理由见 `rebate_invoice_dto` 模块文档：返利是销售折让；`biz_payable` 是供应商台账，客户不能填 `supplier_id`） |
| 结算单 → 票 | `POST /business/erp/rebate-settlement/{id}/invoice`（权限码 `business:erp:rebate-settlement:invoice`） |
| 先看票面 | `POST /business/erp/rebate-settlement/{id}/invoice-preview`（不落库：三列拆分 + 凭证分录 + `can_generate` + `blockers`） |
| 状态流转 | `DRAFT ──开红字发票──▶ SETTLED`（M3-16 的 `update_calc` / `mark_cancelled` 只认 `DRAFT`，时序闸真库已钉） |

## 金额口径（含税反算，分位）

```
total_with_tax = −rebate_amount                          （负）
amount         = −round(rebate_amount / (1 + rate/100), 2)（负）
tax_amount     = total_with_tax − amount                 （≤ 0）
```

1000 元应返 @13% → `−884.96 / −115.04 / −1000.00`；`net.abs() + tax.abs() == total.abs()` 恒等
（`00-unit.out` 里 10 金额 × 9 税率 = 90 组表驱动断言）。

## 凭证（同一事务，`voucher::post_voucher`，`source_type='INVOICE'`）

```
借 5001    主营业务收入      884.96
借 2221.02 应交税费-销项税     115.04
贷 2241.02 其他应付款-销售返利 1000.00      ← V0440 种的末级科目（2241 在 V0226 已是非末级）
```

## 三道闸（都能被真库用例单独复现）

| 闸 | 返回 | 用例 |
|:--|:--|:--|
| 只有 `DRAFT` 能开票 | 409 + **66093** | `second_issue_is_66093` / `cancelled_settlement_is_66093` |
| 应返金额必须 > 0 | 409 + **66094** | `zero_rebate_is_66094` |
| 开票日期间未结账 | 409 + **66095** | `closed_period_is_66095_then_open_month_works` |

三道闸都是**写之前**拦，事务整体回滚 ⇒ 结算单一律留在 `DRAFT`（值变了可重来，不需要改库）。

## 证据文件（全部为命令原始输出）

| 文件 | 内容 |
|:--|:--|
| `00-unit.out` | `cargo test --lib business::erp::sales::rebate` → **91 passed / 0 failed**（含本卡 23 条：14 纯逻辑 + 9 真库） |
| `01-clippy.out` | `cargo clippy --all-targets -- -D warnings` → rc=0 |
| `01b-fmt-check.out` | `cargo fmt --check` → rc=0 |
| `02-kernel.out` | `cargo test -p cloudpivot-kernel --lib` → **240 passed**（66xxx 段含 66093/66094/66095） |
| `03-api.out` | 真机 HTTP 验收（隔离实例 :18097）：登录 / 未登录 401 / 预览 200 / 开票 200 / 重复开票 409+66093 / 列表回显 `invoice_no` → 全过 |
| `04-menu-invariants.out` | `menu_invariants.sh cp_m317_invoice` → 8 条全绿，**菜单 540 / 上限 540** |
| `05-schema.out` | 结算单 40 列 + `ck_biz_invoice_source` 含 `SALES_REBATE` + 科目 2241.02 + 按钮行 sort 70 |
| `06-perm-menu-gate.out` | `perm_menu_gate.py` → `missing_in_menu = 0`，PASS（436 个码） |
| `07-first-boot.out` | **空库首启**：187 表 / 372 个迁移 / V0440 `success=true` |
| `08-migration-drill.out` | V0440 重放（幂等）→ down（自检回落 35 列）→ 再 up，三步 rc=0 |
| `09-check-fast.out` | `bash scripts/check.sh --fast`（L0 + L1） |

## 需要下游知道的

- **菜单总数已达上限 540**（本次 539 → 540）。下一张销售域卡若再加菜单行，必须按
  `scripts/verify/menu_invariants.sh` 注释里的流程实测后上调 `MENU_TOTAL_MAX` 并同步
  `scripts/verify/README.md`（这两个文件当前还有其他 worker 的在途改动，本卡未动）。
- `SETTLED` 的中文标签由「已转应付」改为 **「已开红字发票」**（M3-16 的
  `statuses_match_migration` 断言已同步）；`SETTLED` 的语义就此定为「票已开、收入与销项税已冲」。
- `ck_biz_srs_settled_link`（SETTLED ⇒ 必须有 `invoice_id`/`invoice_no`/`settled_at`）会让
  「直接改库把结算单置 SETTLED」的旧写法撞 23514 —— M3-16 的两条真库用例已按新口径补齐三列。
- 红冲留待后续卡：票被作废（`biz_invoice.status='VOID'`）**不会**把结算单改回 `DRAFT`，
  这是决定（红冲/调账是另一条业务路径），不是缺口。
