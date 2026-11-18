# M4-18 · 采购订单挂合同额度守卫（金额不超合同 · 超限拒绝）· 证据

卡：`t_b90d4761`（M4-18【M4·采购】订单挂合同校验（金额不超合同）—— 超限拒绝）
上游：`t_566fc2dc`（M4-17 `contract_id` 字段）收尾时点名「M4-18 也已实现，先核验再动手」。
日期：2026-11-18 · 机器：99 开发机 → 145 PG（`172.18.10.145:5432`）

## 一句话结论

**守卫本体在 HEAD 上已经完整、语义正确，本卡没有改业务代码；本卡交付的是把它钉死的 8 条真库用例 + 一条可复跑的演练（23 条断言全绿）+ 变异反证（每条用例都能红）。**

为什么不重写一遍守卫：同一道校验写两遍就是两处真相；而「超限拒绝」的正确性只由**判据细节**
决定（`>` 还是 `>=`、累不累加、排不排自己、作废/软删算不算、有没有锁），这些恰恰是没人测过的部分
（改动前只有 2 条额度用例，且用的是 10 万和 1 元两张极端合同 —— 累计路径、改单路径、
并发路径一条都没跑过）。

## 守卫在哪（HEAD 现状，逐条核过）

| 环节 | 落点 | 口径 |
|:--|:--|:--|
| 守卫本体 | `purchase_handler.rs` `guard_contract_amount` | 事务内 `contract_lock_full`（`FOR UPDATE`）锁合同行 → 锁内二次校状态 → `committed_amount` 现算 → `after > 合同额` 报 **409 + 66019** |
| 建单入口 | `create_purchase_order`（`guard_contract_amount(&tx, contract, totals.with_tax, None)`） | 事务内、`purchase_model::create` 之前 |
| 改单入口 | `update_purchase_order`（`…, Some(id)`） | 排除本单，否则自己算两遍会误拒 |
| 现算函数 | `purchase_model::committed_amount` | `WHERE contract_id = ?::uuid AND status <> 'CANCELLED' AND deleted_at IS NULL`，`SUM(total_with_tax)`，**不做缓存列**（V-21/R1：缓存必漂移） |
| 唯一写入口 | `purchase_model::create` / `update` | 各**只有 1 个调用方**（两个 handler），连「采购申请 → 订单」（`purchase_requisition_handler.rs:872`）也是走 `create_purchase_order` ⇒ 没有旁路能绕开守卫 |
| 合同状态闸 | `contract_model::USABLE_FOR_ORDER`（SIGNED/ACTIVE） | 锁内再校一次，防止读到写之间合同被终止 |

判据基数是**含税额**（`totals.with_tax`）对合同 `amount`，与合同详情页 `committed_amount`
（`contract_dto.rs:842`）同一口径 —— 界面上的「履约进度」和下单时的拦截不会打架。

## 本卡新增的 8 条真库用例（`purchase_handler::tests::m418_*`）

夹具四张合同（每张一个金额，这样每类断言才有唯一解）：`30` / `22.60` / `22.59` / `1000`，
而 `contract_req` 那一单含税正好 **22.60**（10 × 2.00 + 13%）。

| # | 用例 | 钉住的口径 | 被放宽后谁会红 |
|:--:|:--|:--|:--|
| ① | `m418_cumulative_over_limit_is_rejected` | 30 元合同：第一张 22.60 放行，第二张累计 45.20 → 409 + 66019，**整单回滚** | 守卫只看本单（不累加） |
| ② | `m418_exact_limit_is_allowed_and_one_cent_over_is_rejected` | 22.60 == 22.60 放行（`>` 不是 `>=`）；22.60 > 22.59 拒绝 | 判据改成 `>=` |
| ③ | `m418_update_over_limit_is_rejected_and_rolls_back` | 改单 22.60 → 45.20 拒，且明细/金额原地不动 | 改单漏走守卫 |
| ④ | `m418_update_excludes_self_so_a_full_contract_stays_editable` | 用满的合同改备注必须放行（自己不算两遍） | 漏 `exclude = Some(id)` |
| ⑤ | `m418_cancelled_order_frees_quota` | 作废后额度释放（含「未作废时必拒」的对照组） | 漏 `status <> 'CANCELLED'` |
| ⑥ | `m418_soft_deleted_order_frees_quota` | 软删后额度释放 | 漏 `deleted_at IS NULL` |
| ⑦ | `m418_other_contract_orders_do_not_consume_this_contract` | 跨合同不串额 | 漏 `contract_id = ?` |
| ⑧ | `m418_concurrent_creates_only_one_wins` | 两张 22.60 **并发**只准一张过，另一张 409（`multi_thread` + `tokio::join!`） | 去掉 `FOR UPDATE` |

并发那条是这道守卫唯一不能靠单线程推理保证的地方：两个事务各自「读到还剩额度」再各自写入，
加起来就超了（ARCH-2 V-23）。行锁 + READ COMMITTED 下「拿到锁之后才取语句快照」是它成立的
全部依据 —— 所以 ⑧ 必须真并发跑，不能写成串行两连击。

## 变异反证（证明这 8 条不是摆设）

| 变异（只改 1 处） | 期望 | 实测 |
|:--|:--|:--|
| M1 `committed_amount` 去掉 `AND id <> ?::uuid` | 只有 ④ 红 | ✅ 7 绿 1 红（④），报文：`合同 P2A3-HT-FULL 额度不足：合同金额 22.6000，已下单 22.6000…本单 22.60，下单后累计 45.2000` |
| M2 去掉 `status <> 'CANCELLED'` | 只有 ⑤ 红 | ✅ 7 绿 1 红（⑤），报文：`合同 P2A3-HT-CAP30 额度不足：合同金额 30.0000，已下单 22.6000…` |
| M3 `contract_lock_full` 去掉 `FOR UPDATE` | ⑧ 红 | ✅ 连跑 2 轮全红（脚本）/ 手工跑 3 轮全红（见 `02_mutation_detail.txt`），**不是偶发** |

三次变异都按 `cp` 备份还原，还原后 `sha256sum` 与原文件逐字节一致
（`purchase_model.rs` = `c8451acb…`，`contract_model.rs` = `6598646e…`），`git status` 干净。

## 门禁与回归

| 项 | 结果 |
|:--|:--|
| `cargo fmt --check` | rc=0（见 `03_gates.txt`） |
| `cargo clippy --lib -- -D warnings` | rc=0（见 `03_gates.txt`） |
| M4-18 八条 | `test result: ok. 8 passed; 0 failed; 0 ignored` |
| 采购单 handler 整套件 | `test result: ok. 73 passed; 0 failed; 0 ignored`（新夹具没踩坏兄弟用例） |
| 夹具自清 | 跑完夹具供应商采购单回到基线 0 张、四张额度合同残留 0 行 |
| 演练总账 | **PASS=23 FAIL=0**（`01_drill_full.txt`） |

## 复跑

```bash
# 99 上（默认连 145 的 PG）
bash scripts/verify/m418_po_contract_amount_guard_drill.sh
# 145 上（本机 PG）
PG_HOST=127.0.0.1 bash scripts/verify/m418_po_contract_amount_guard_drill.sh
# 加跑变异反证（约 +15 分钟：三处各重编译一次）
MUTATE=1 bash scripts/verify/m418_po_contract_amount_guard_drill.sh
```

脚本自己克隆 `cloudpivot_test` → 回放 `V0405…HEAD`（模板停在 V0404）→ 跑用例 → 清自己的夹具 →
DROP 演练库（`KEEP_DB=1` 可留库排查）。库名必须含 `drill/e2e/test`，否则 rc=2 直接拒。

## 两条留给下一张卡的事实（别当 bug 去「修」）

1. **没有、也不会有数据库层的额度约束**：判据是「同一合同下多行合计 ≤ 合同额」，跨行聚合写不进
   `CHECK`，`FOR UPDATE` 行锁 + 事务内现算是唯一可行的强制点。谁要「在库里加个约束」，答案是不行。
2. **模板库 `cloudpivot_test` 自带一行历史残留**：`biz_flow_instance` 里
   `FLW-20261110-DB2F055B`（`PO_APPROVE` / `PENDING`，供应商 `b1b00005-…`，2026-11-10 入库）。
   采购模块的收尾用例 `zz_teardown_purges_po_approval_residue` 有一条**全库**断言
   「不剩 PENDING 采购审批实例」，会被这行顶红 —— 它与本卡改动无关（本卡的 8 条用例一条审批实例都不建），
   采购模块按约定也不许清别人夹具的行（`purge_fixture_docs` 只动自己供应商的）。演练脚本因此在
   **跑用例之前**把「此刻已存在」的这类行清掉并逐行打印，之后的残留才算本次跑出来的。
