# t_6a667db8 —— doc_kind.rs 词表域补齐（盘点三态 / STOCK_TAKE / 报销两值）

日期：2026-11-06　库：`cloudpivot_st9`（隔离库，198 迁移 / V0256；**生产库未触碰**）
结论：`scripts/check.sh` 闸④ 在启用盘点/期初/报销凭证规则的库上**由红转绿**，
且「域外值」这条真信号**未被削弱**（负样本仍判红，见下方「负样本」）。

## 1. 根因（词表落后于代码，不是库里多写了一套词表）

| 列 | 缺口 | 代码侧事实 |
|---|---|---|
| `biz_inventory_txn.biz_type` | `STOCK_TAKE_GAIN / STOCK_TAKE_LOSS / STOCK_TAKE_CANCEL` 不在 `TxnBizType` 枚举与 `TXN_BIZ_TYPE_VALUES` | 只以常量 `stocktake_model::BIZ_TYPE_GAIN/LOSS/CANCEL` 存在，且**确实写入**该列（st9 实测 1 + 5 + 6 行） |
| `biz_inventory_txn.doc_type` | `STOCK_TAKE` 不在 `TXN_DOC_TYPE_VALUES`（7 值） | 盘点过账写 `doc_type='STOCK_TAKE'`（st9 实测 12 行） |
| `biz_voucher_rule.biz_type` | `EXPENSE_REPORT / EXPENSE_PAYMENT` 不在 `VOUCHER_RULE_BIZ_TYPE_VALUES`（29 值） | `expense_bridge::SOURCE_REPORT/SOURCE_PAYMENT` 会写凭证规则（st9 实测各 1 行） |

修前基线（父卡 t_086dab13 存档）：`../20261106-t086dab13-e4-biz-type/gate4_vocab_st9_preexisting.txt`（exit 1，3 列 6 个域外值）。

## 2. 改动

1. `backend/src/business/shared/doc_kind.rs`
   - `TxnBizType` 新增 3 变体（`StockTakeGain/StockTakeLoss/StockTakeCancel`）：`ALL`（19→22）/`as_str`/`label` 同步；
     `doc_kind()` 三态均 → `DocKind::StockTake`（与 `ADJUST_IN/OUT` 一样归盘点对象轴，但**不合并**取值：
     「哪张盘点单造成的」与「方向」是两件事）；并在枚举头注释写清 `ADJUST_*` 与 `STOCK_TAKE_*` 的分工。
   - `TxnDocType` 新增 `StockTake`（`STOCK_TAKE`）：`ALL`（7→8）/`as_str`/`label`/`doc_kind()` → `DocKind::StockTake`。
   - `TXN_BIZ_TYPE_VALUES` 19→22、`TXN_DOC_TYPE_VALUES` 7→8、`VOUCHER_RULE_BIZ_TYPE_VALUES` 29→34
     （txn 轴 22 ∪ 凭证来源轴 15，去重后 34），与枚举**逐字同序**。
2. `backend/src/business/shared/doc_kind_tests.rs`：新增两条登记表对账测试
   - `txn_doc_map_matches_snapshot_registry`：`TXN_DOC_TYPE_VALUES`（+ 遗留 `PURCHASE_ORDER`）↔ `10_checks.sql` 的 `TXN_DOC` 登记行（含目标表）；
   - `voucher_source_map_matches_snapshot_registry`：`VOUCHER_SOURCE_VALUES` ↔ `VOUCHER_SOURCE` 登记行。
   目的：这次漏的不是 Rust 内部不一致（`domain_consts_match_enums` 一直是绿的），而是**Rust ↔ 巡检登记表**脱节。
3. `docs/architecture/snapshot-drift/10_checks.sql`：
   - `TXN_DOC` 补 `STOCK_TAKE → biz_stock_take`（否则 DI-18 看不到盘点流水悬空、DI-22 报登记缺口）；
   - `VOUCHER_SOURCE` 补 `EXPENSE_REPORT / EXPENSE_PAYMENT → biz_expense_report`（FIN-3 报销两张凭证都挂报销单）。
4. `frontend/src/api/business/erp/doc-kinds.ts`：`TXN_BIZ_TYPES` / `TXN_BIZ_TYPE_LABELS` / `TXN_DOC_TYPES` / `TXN_DOC_TYPE_LABELS`
   同步镜像（`frontend_ts_mirror_matches_rust` / `frontend_ts_labels_match_rust` 是硬约束，前端下拉缺这三个值就与后端不一致）。
5. `scripts/verify/regression/erp_invariants_e4_regression.py`：闸① 原先把枚举数量写死 `== 19`，
   词表扩容后**这条闸自己会误判**（实测 exit 1）。改为从 `TXN_BIZ_TYPE_VALUES` 解析期望集合、断言
   「`as_str()` 集合 == 域清单集合」，并把注释里「盘点三态只在模块常量、枚举里没有」的旧事实更新。
6. `scripts/verify/erp_invariants.py`：仅更新 E4 登记表上方的取值域来源注释（19→22、三个值已进枚举）。

## 3. 验证（全部在本机隔离库执行，命令与输出见同目录 txt）

| 闸 | 命令 | 结果 |
|---|---|---|
| 闸④（本卡主目标） | `biz_type_domain.py --strict` @ `cloudpivot_st9` | **exit 0**，24 列全 PASS（`gate4_st9_after_fix.txt`） |
| 闸④ 负样本（判据未被放宽） | 克隆 st9 → `cloudpivot_g4neg_t6a667`，插一条 `biz_type='BOGUS_TXN_TYPE'` → `--strict` | **exit 1**，`✗ 域外值 'BOGUS_TXN_TYPE' × 1`（`gate4_negative_control.txt`），克隆库已 DROP |
| Rust 单测 | `cargo test --lib business::shared::doc_kind` | **21/21 通过**（含 `domain_consts_match_enums`、两条新登记表测试、前端镜像测试）（`cargo_test_doc_kind_21passed.txt`） |
| E4 回归 | `erp_invariants_e4_regression.py` @ `cloudpivot_e4reg_t086` | **23/23 全绿，exit 0**（`regression_after_scriptfix.txt`；修脚本前同一命令 exit 1 见 `regression_before_scriptfix.txt`） |
| ERP 不变量 | `erp_invariants.py` @ `cloudpivot_st9` | **67/68 通过，BLOCKER 0，WARN 1**（N2 账号无部门，卡面允许）（`erp_invariants_st9_after_fix.txt`） |
| 快照巡检 | `snapshot_audit.py --install-views` @ `cloudpivot_st9` | **BLOCKER 0 / WARN 0 / INFO 3**，DI-22 登记缺口 通过（`snapshot_audit_st9.txt`） |

## 4. 边界与遗留

- 未改闸④ 的判定口径（`--strict` 仍是严格档），未删任何库内流水；负样本证明可红。
- `docs/evidence/20261105-fin2/README.md` 里「29 值 = txn 19 ∪ 来源 13」是当时的取证快照，属历史记录，未回改。
- `STOCK_TAKE_GAIN/LOSS/CANCEL` 目前仍由 `stocktake_model.rs` 常量写入，词表已覆盖；**写入点与词表是两件事**，
  E4 登记表（`E4_BIZ_TYPE_AXIS`）与词表各自独立核对，两者都有闸。
