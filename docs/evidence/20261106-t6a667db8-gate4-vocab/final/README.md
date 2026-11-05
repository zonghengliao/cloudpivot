# t_6a667db8 终验（backend-developer，本卡 assigned worker 自跑）

时间：2026-11-06 01:48–01:52 CST　HEAD = `c097b98`（本卡实现提交 `e51244b` 之后，兄弟卡又落了 3 个提交）
库：`cloudpivot_st9` @ 127.0.0.1:5432（隔离库，**生产库未触碰**）

本目录是「实现已提交后、在最新 HEAD 上重新跑一遍四条验收」的原始输出，用来排除
「实现提交之后被别的卡带坏」和「上一轮 worker 的自我声明」两种风险。

## 结论

| 验收项（卡面） | 命令 | 结果 | 文件 |
|---|---|---|---|
| 闸④ `--strict` @ cloudpivot_st9 退出码 0 | `python3 scripts/verify/biz_type_domain.py --strict` | **PASS，24 列全 PASS，EXIT=0**；原先判红的 3 列现在都是 PASS：`biz_inventory_txn.biz_type`(4 种) / `biz_voucher_rule.biz_type`(5 种) / `biz_inventory_txn.doc_type`(2 种) | `gate4_st9_strict.txt` |
| 判据未放宽（负样本必红） | 克隆 st9 → 改两条既有流水为 `BOGUS_TXN_TYPE` / `BOGUS_DOC` → `--strict` | **EXIT=1**，列名+域外值+行数都点名；克隆库已 DROP（残留计数 0） | `negative_control.txt` |
| `cargo test --lib business::shared::doc_kind` 全绿 | 同左 | **21 passed / 0 failed**，含 `domain_consts_match_enums`、`txn_doc_map_matches_snapshot_registry`、`voucher_source_map_matches_snapshot_registry`、`frontend_ts_mirror_matches_rust` | `cargo_test_doc_kind.txt` |
| `erp_invariants.py` @ cloudpivot_st9 仍 BLOCKER 0 | `python3 scripts/verify/erp_invariants.py` | **67/68 通过，BLOCKER 0，WARN 1**（N2：`admin`/`st_wh` 在用 HUMAN 账号无部门 —— 卡面允许、非本卡引入），EXIT=0 | `erp_invariants_st9.txt` |

## 词表现状（`doc_kind.rs`，实现提交 e51244b）

| 词表 | 数量 | 本卡补入 |
|---|---|---|
| `TxnBizType` / `TXN_BIZ_TYPE_VALUES` | 19 → **22** | `STOCK_TAKE_GAIN` / `STOCK_TAKE_LOSS` / `STOCK_TAKE_CANCEL`（`doc_kind()` 三态 → `DocKind::StockTake`） |
| `TxnDocType` / `TXN_DOC_TYPE_VALUES` | 7 → **8** | `STOCK_TAKE`（`doc_kind()` → `DocKind::StockTake`，归「④ 流水来源单据」轴） |
| `VOUCHER_RULE_BIZ_TYPE_VALUES` | 29 → **34** | `EXPENSE_REPORT` / `EXPENSE_PAYMENT`（只走 voucher-source 轴，按设计不进 txn 轴） |

枚举 ↔ 字面量数组逐字同序由 `domain_consts_match_enums` 守着，本轮 21/21 已含它。
连带同步（同一提交）：`frontend/src/api/business/erp/doc-kinds.ts` 镜像、`docs/architecture/snapshot-drift/10_checks.sql`
的 `TXN_DOC.STOCK_TAKE` / `VOUCHER_SOURCE.EXPENSE_*` 登记行、2 条「Rust 词表 ↔ 巡检登记表」对账测试。

## 边界

- **闸④ 口径一个字未改**：`scripts/check.sh` 里仍是 `biz_type_domain.py --strict`（block），`scripts/verify/biz_type_domain.py`
  本卡零改动；不许 off/warn 换绿灯的要求已满足（负样本可红即证）。
- 未删任何库内流水；st9 的盘点/期初流水原样保留（这正是闸④ 要覆盖的真实数据）。
- 本轮只对 `backend/src/business/shared/doc_kind{,_tests}.rs` 做了 **rustfmt 收口**（纯格式，无逻辑改动，
  提交见「fmt 收口」那条），因为 `e51244b` 落库时这两处长行未按 rustfmt 折叠，`cargo fmt --check` 会红。
- 同目录 `recheck/` 是历史轮次（HEAD=e51244b 那轮）的复核输出；本目录 `final/` 为准。
  `final/gate4_st9_head1bb2b11.txt` 是上一轮 worker 在 HEAD=`1bb2b11` 上的同一条命令输出（与 `gate4_st9_strict.txt`
  在 HEAD=`c097b98` 上互相印证），`final/list_domains.txt` 是 `--list-domains` 导出的 24 列值域清单（词表最终态）。
