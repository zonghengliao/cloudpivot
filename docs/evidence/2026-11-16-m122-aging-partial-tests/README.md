# M1-22 账龄单测（跨期、部分收款）— 证据

> 卡：`t_6a347bcb`（M1-22【M1·财务】账龄：单测（跨期、部分收款））
> 日期：2026-11-16 · 分支 `main` · 库 `cp_m122_unit`（在 145 上 `-T cloudpivot_test` 克隆，跑完已删）

## 1. 交付物

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/erp/receivable/aging_partial_tests.rs` | **新增**（8 条用例：3 条纯逻辑 + 5 条真库） |
| `backend/app/src/business/erp/receivable/mod.rs` | 注册 `#[cfg(test)] mod aging_partial_tests;` |
| `backend/app/src/business/erp/receivable/receivable_aging.rs` | `build_report` 由 `async fn` → `pub(crate) async fn`（**只改可见性**，零行为改动）+ 说明为什么测试要走整条链 |
| `backend/app/src/test_seed.rs` | `ensure_customer` 的 `status` 由 `1` → `'ACTIVE'`（**修夹具 helper 的既有 bug**，见下） |

迁移 0 条；前端 0 改动；接口契约 0 改动。

### 顺手修掉的一个既有 bug：`test_seed::ensure_customer` 写不进任何库

`biz_customer.status` 是 **VARCHAR + `CHECK (status IN ('ACTIVE','FROZEN'))`**（V0260），而
`test_seed::ensure_customer` 写的是 `status = 1`（smallint 的写法）→ 每次插入都撞 `ck_biz_customer_status`（23514）。
它在 M1-22 之前**没有任何调用方**，所以一直没暴露；本卡的账龄夹具是第一个用户，踩到才发现。
`test_seed_gate.sh` ③ 又禁止测试代码手写 `INSERT INTO biz_customer`（预算 1 处，已被别人占满），
所以正确做法就是把这个 helper 修对（其它 `ensure_*` 的目标表 status 都是 `smallint`，只有客户是文本域，
已核对 `information_schema`：`biz_account/location/material/supplier/unit/warehouse` = smallint，`biz_customer` = varchar）。


## 2. 八条用例与它钉住的东西

| # | 用例 | 钉住什么 |
|:--|:--|:--|
| ① | `partial_receipt_keeps_balance_in_its_own_bucket_and_sorting` | 余额**整笔**落该单自身档位（1000/已收 300 ⇒ 31-60 档只有 700，不是 1000）；客户按**余额**排序（乙 800 在甲 700 之前，而不是按应收金额甲在前） |
| ② | `received_part_never_lands_in_a_bucket` | 已收的 300 **不进任何档位**（错法会摊进「未到期」，汇总多出 300 或 1000）；各档之和 = 未收余额 |
| ③ | `cross_period_base_date_moves_bucket_but_not_balance` | 同一张单、同一余额 700，基准日 09-05 / 09-25 / 11-15 ⇒ 未到期 / 1-30 / 61-90，**金额不动**，且每次只允许一个非零档 |
| ④ | `cross_period_receipt_does_not_rewrite_prior_balance_hint_is_explicit` | 跨期收款的语义是**当前余额快照**（不改写历史），出参 `hints` 必须原文写明「基准日之后的收款不会回滚」+「单据按所属会计期间截断」+ 状态白名单 + 分档刻度 |
| ⑤ | `db_partial_receipts_allocate_balance_per_document` | 真写路径（`receipt::create` → `receipt::allocate` → `build_report`）：一笔收款核两张（300 + 200，剩 200 未核销）+ 一笔核第三张（500）；逐单 `未收 = 应收 − 已收`、逐单档位；汇总 7 档 `600/300/800/700/0/0/0`，合计 2400 = 应收 3400 − 已收 1000；逾期占 75.00%；**核销额 300/200/500 不许出现在档位里** |
| ⑤b | `db_reallocating_same_receipt_moves_balance_by_delta_only` | 同一张收款改核销金额（300 → 500）= **UPSERT 改金额**：已收按**差额** +200 累加（不是 +500），明细仍 1 行，超出收款金额被拒且余额不动，账龄未收 500 |
| ⑥ | `db_cross_period_receipt_does_not_restate_prior_aging` | 跨期：基准日 2026-09-30（早于 10 月收款）时，10/11 月单据被期间截断、余额仍是「已核销之后」的 700/800（不回滚）；基准日推到 10-31 三张进表、档位随基准日推进（66 天 → 61 天 → …）；同一张单余额在三个基准日恒为 700 |
| ⑦ | `db_two_partial_receipts_settle_document_and_it_leaves_aging` | 两次部分核销把单收清：`OPEN → PARTIAL → SETTLED`，收清的单**整行退出**账龄（它的已收 600 也不进 `total_received`）；收清后再核 1 元 → 拒（409 文案点名），且不留痕（收款仍 0 核销） |

## 3. 三次代码反证（证明这些用例真的会红，不是「怎么写都绿」）

| 反证 | 改法（都已还原） | 实测结果 |
|:--|:--|:--|
| ① 用应收金额而不是余额 | `base_cte` 里 `r.unsettled_amount` → `r.amount AS unsettled_amount` | 2 条红：`AR-AG22-0001 未收金额（余额分摊的分子） left: 1000.0000 right: 700`；`700 + 800（10 月收的钱已经减掉） left: 2000.0000 right: 1500` |
| ② 去掉期间截断 | `AND r.period_code <= ?` → `AND r.period_code <= '9999-12' AND ?::text IS NOT NULL` | 1 条红：`10 / 11 月的两张被期间截断 left: 4 right: 2` |
| ③ 回写用「本次金额」而不是差额 | `receipt.rs`: `received_amount + delta` → `received_amount + item.amount` | 1 条红：`已收按差额累加（300 + 200 = 500）`，实测 `(800.0000, 200.0000, "PARTIAL")` |

## 4. 门禁（本机 99，连 145 的 PG）

```text
cargo fmt --check                                  → exit 0
cargo clippy -p cloudpivot --lib --tests -- -D warnings → exit 0（0 warning）
cargo test business::erp::receivable::aging_partial_tests       → 8 passed / 0 failed
cargo test business::erp::receivable::receivable_aging          → 13 passed / 0 failed（既有用例未回归）
cargo test business::erp::payable::payable_aging                → 10 passed / 0 failed
cargo test system::menu::permission_audit_tests                 → 8 passed / 0 failed
python3 scripts/test_domain.py check                            → ✓ 5868 条 / 19 域，无遗漏无重叠无空域
bash scripts/verify/test_seed_gate.sh                           → ✓（① 转发 41 / ② 导出 23 / ③ 主数据 INSERT 109=预算 109）
bash scripts/check.sh --fast（= pre-push 的 L0+L1 闸）           → EXIT 0「全部检查通过（L0 L1）」
```

真库基线（`--test-threads=1`，1 条纯逻辑 + 7 条真库同进程）：

```text
running 8 tests ... test result: ok. 8 passed; 0 failed  (0.36s)
```

## 5. 已知基线红（**不是本卡引入**，未改动这些模块）

`python3 scripts/test_domain.py run --package erp-sales --jobs 1` ⇒ `322 passed / 10 failed`，全部集中在
`receivable::receivable_bridge`（8 条）与 `receipt_refund`（2 条），报错是 `过账应成功：10004 服务器内部错误`
/ `生成退款凭证: query failed: : db error`（过账取科目的 SQL 失败）。

反证「与本卡无关」：在**未加载本卡测试模块**的前提下，从两个模板各克隆一份干净库单独跑这两个模块 ——

```text
cloudpivot_test      -T 克隆 cp_m122_bridge2  → receivable_bridge: 5 passed / 8 failed
cloudpivot_test_full -T 克隆 cp_m122_bridge   → receivable_bridge: 5 passed / 8 failed（同上 8 条）
cloudpivot_test      -T 克隆 cp_m122_bridge2  → receipt_refund:   22 passed / 2 failed（同上 2 条）
```

即：这 10 条在 HEAD 的模板库上本来就红（缺演示组织种子 / 过账科目，属 P2-60 §6 的口径问题），
本卡只是**多跑了 8 条绿的**（域清单 324 → 332 条，`test_domain.py check` 可复核）。

## 6. 清理

```text
dropdb cp_m122_unit / cp_m122_bridge / cp_m122_bridge2 / cloudpivot_test_p261_erp_sales  → 全部完成
psql "select datname … like 'cp_m122%' or like '%p261%'" → 0 行
本机未起后端实例（本卡只跑单测，不需要 :8023）
```
