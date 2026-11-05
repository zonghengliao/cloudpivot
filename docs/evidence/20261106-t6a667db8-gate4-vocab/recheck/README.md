# t_6a667db8 复核（backend-developer / 本卡 assigned worker 独立复跑）

时间：2026-11-06（CST）；HEAD = `e51244b`；库 = `cloudpivot_st9` @ 127.0.0.1:5432（隔离库，**未触生产**）。

## 本卡诉求与落地

`doc_kind.rs` 的词表域落后于代码，导致 `scripts/check.sh` 闸④
（`scripts/verify/biz_type_domain.py --strict`）在**启用盘点/期初/报销凭证规则**的库恒红。
补入的 6 个值（`e51244b`）：

| 轴 | 缺的值 | 填法 |
|---|---|---|
| ③ 库存变动动作 `TxnBizType` / `TXN_BIZ_TYPE_VALUES` | `STOCK_TAKE_GAIN` / `STOCK_TAKE_LOSS` / `STOCK_TAKE_CANCEL` | 枚举项 + `ALL`（19→22）+ `as_str` + `label` + `doc_kind()`→`DocKind::StockTake`；字面量数组逐字同序 |
| ④ 流水来源单据 `TxnDocType` / `TXN_DOC_TYPE_VALUES` | `STOCK_TAKE` | 枚举项 + `ALL`（7→8）+ `as_str` + `label` + `doc_kind()`→`DocKind::StockTake` |
| 凭证规则域 `VOUCHER_RULE_BIZ_TYPE_VALUES` | `EXPENSE_REPORT` / `EXPENSE_PAYMENT` | 域 = txn 轴 ∪ voucher-source 轴（去重），29→34；报销两值只在 voucher-source 轴 |

连带同步（同一提交）：`frontend/src/api/business/erp/doc-kinds.ts` 镜像（否则
`frontend_ts_mirror_matches_rust` 红、前端下拉缺值）、`docs/architecture/snapshot-drift/10_checks.sql`
登记 `TXN_DOC.STOCK_TAKE` / `VOUCHER_SOURCE.EXPENSE_*`（否则 DI-22 报登记缺口、DI-18 静默漏报）、
`doc_kind_tests.rs` 增 2 条「Rust 词表 ↔ 巡检登记表」对账测试。

**闸④ 本身一个字未改**：`scripts/check.sh:415` 仍为 `biz_type_domain.py --strict`，
`scripts/verify/biz_type_domain.py` 最近两次提交仍是 `0eb8b81` / `b9879ec`（本卡没碰它）。
`scripts/check.sh` 工作区里那处未提交改动是**另一张卡（闸⑧ 角色码）**加的，与闸④ 无关。

## 复核结论（本目录 5 份原始输出）

| 验收项 | 结果 | 文件 |
|---|---|---|
| 闸④ `--strict` @ cloudpivot_st9 | **PASS，24 列全绿，EXIT=0** | `gate4_st9.txt` |
| `cargo test --lib business::shared::doc_kind` | **21/21 通过**（含 `domain_consts_match_enums`、`frontend_ts_mirror_matches_rust`、2 条新登记表对账） | `cargo_test_doc_kind.txt` |
| `erp_invariants.py` @ st9 | **67/68，BLOCKER 0，WARN 1**（N2：admin/st_wh 在用 HUMAN 账号无部门 —— 卡面允许，非本卡引入） | `erp_invariants_st9.txt` |
| `snapshot_audit.py` @ st9（10_checks.sql 改动无回归） | **BLOCKER 0 / WARN 0 / INFO 3**，DI-22 登记缺口 通过 | `snapshot_audit_st9.txt` |
| 负样本（克隆库插域外值） | **exit 1 且点名 `BOGUS_TXN_TYPE` / `BOGUS_DOC`** ⇒ 判据未放宽；克隆库已 drop（残留 0） | `negative_control.txt` |

st9 上被点名的值（本卡修复前判红的全部就是它们，取自 `biz_inventory_txn` / `biz_voucher_rule` 实测）：

```
biz_inventory_txn.biz_type : OPENING 1 | STOCK_TAKE_GAIN 1 | STOCK_TAKE_LOSS 5 | STOCK_TAKE_CANCEL 6
biz_inventory_txn.doc_type : OPENING 1 | STOCK_TAKE 12
biz_voucher_rule.biz_type  : DEPRECIATION 1 | EXPENSE_PAYMENT 1 | EXPENSE_REPORT 1 | FIXED_ASSET_ACQ 3 | PURCHASE_IN 1
```

## 未做（诚实边界）

- `docs/architecture/snapshot-drift/10_checks.sql` 与 `doc_kind.rs` 的「同文件二改」风险已消除：
  t_593fba6c（P1-FIX-10）在 `80e05a1` 先行落地，本卡提交在其之后，未产生冲突。
- 生产库未触碰；本轮所有跑库动作都在 `cloudpivot_st9` 与一个已删除的临时克隆库上。
