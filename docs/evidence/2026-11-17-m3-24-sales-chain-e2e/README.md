# M3-24 销售缺口 E2E：报价单 → 销售订单 → 出库（证据）

- 卡：`t_367777a2`（M3-24【M3·销售】销售缺口 E2E：报价单 → 订单 → 出库）
- 日期：2026-11-17
- 结论：**全链一次跑通，28/28 断言项 PASS，0 失败；有牙演练（改坏期望）确认会红且演练库仍被 DROP。**

## 1. 这一卡补的是什么缺口

前面几张卡各自把自己那一段钉死了，但**没有一条用例把它们串起来**：

| 上游卡 | 有没有真跑这条链的一段 | 断在哪 |
|:--|:--|:--|
| M3-05/M3-08（报价单状态机 / 转单） | 有 | 停在「新建 `DRAFT` 销售订单」 |
| SO-06（`sales_approval_bridge`） | 有 | 订单是 **SQL 直插的 `DRAFT` 夹具**，不是报价单转出来的 |
| M2-02/M2-03（发货过账 / 签收确认） | 有 | 订单是 **SQL 直插的 `APPROVED` 夹具**，`so_id` 从没经过审批 |

于是「**一张报价单转出来的订单，到底能不能走完审批、发货、出库、挂应收**」这件事，
在库里从来没有被验证过。本卡用**一张单**走完，上游夹具全部走真实入口。

## 2. 链路与断言（9 步，84 条 assert）

```text
① 建报价单 DRAFT（2 行 / 13% 税）                      金额恒等：2000 + 260 = 2260
② 提交审批 quotation_approve → 逐节点真派真批 → APPROVED  → 自动发出（DRAFT→SENT）
   反证：SENT 直接转单 409（只有 ACCEPTED 能转），且零订单落库
③ 客户接受 SENT → ACCEPTED
④ 转销售订单（带行 / 回链 converted_so_id / 备注带 QT 单号 / 轨迹 CONVERT）
   反证：DRAFT 订单开发货单 409（审批是发货前置）
⑤ 订单提交审批 so_approve → 逐节点真派真批 → APPROVED
⑥ 建发货单（items 留空 = 从订单带出未发完的行）→ DRAFT，2 行 / 合计 15 件
⑦ 提交 PENDING_POST → 过账 POSTED：出库单 PENDING_POST + 在途 10/5 + 订单 OUTBOUND + delivered_qty 回写
   此刻断言「库存一分未扣、无凭证、无应收」（M2-02：货在途，收入成本不早确认）
⑧ 签收确认（confirm_receipt）= 出库过账：
   成本账/库位账 100→90 / 95、流水 SALE_OUT 10/5、在途归零 RECEIVED、
   成本结转凭证 1 张（借 6401 150 / 贷 1405 150，4 条分录）、应收 1 张（2260 / OPEN / 来源=出库单号）
⑨ 收口：重复转单幂等回原单（already_converted=true，不建第二张）、
   OUTBOUND 订单不能再发货（409）、无 PENDING 审批实例、状态轨迹条数、夹具清场
```

**审批不是 SQL 夹具**：`quotation_approve` / `so_approve` 都真发起实例、真按角色展开待办、
真逐条 `engine::approve` + `doc_reader::dispatch_settled`（与 HTTP `POST /flow/task/{id}/approve`
的执行链逐行同款）。审批人不由用例指定，逐个取库给的 `assignee_id`。

## 3. 交付物

| 文件 | 作用 |
|:--|:--|
| `backend/app/src/business/erp/sales/sales_chain_e2e_tests.rs`（新，1099 行） | 全链用例（1 条 9 步 84 断言）+ 收尾零残留用例（1 条） |
| `backend/app/src/business/erp/sales/mod.rs` | 挂 `mod sales_chain_e2e_tests;`（`#[cfg(test)]`） |
| `scripts/verify/m324_sales_chain_e2e_drill.sh`（新） | 克隆演练库 → 补 V0405…V0445 → 前置断言 → 跑本卡 + 销售域 + 发货域 → SKIP=0 → 夹具零残留 → DROP |
| `docs/evidence/2026-11-17-m3-24-sales-chain-e2e/*` | 本目录：主跑 / 步骤轨迹 / 库侧快照 / 有牙 / 门禁 |

**零迁移、零权限码、零菜单、零前端改动**（纯测试卡）。

## 4. 真跑输出

### 4.1 演练脚本（`10_drill_full.out`）

```text
== 汇总：PASS=28 FAIL=0 ==
ALL PASS
```

拆开看：

| 项 | 结果 |
|:--|:--|
| 模板克隆 + 补 41 条业务迁移（V0405…V0445） | PASS |
| 前置：`biz_quotation` / `biz_quotation_line` / `biz_inventory_in_transit` 表 | PASS |
| 前置：`quotation_approve`（V0435）/ `so_approve`（V0281）已发布 + `SALES_SUPERVISOR` 角色 | PASS |
| 前置：销售链四张单据没有被人为关掉的状态机边 | PASS |
| 本卡 `sales_chain_e2e_tests` | **2 条全绿，SKIP=0** |
| 销售域整组 `business::erp::sales` | **359 条全绿，SKIP=0** |
| 发货域整组 `business::erp::shipment` | **49 条全绿，SKIP=0** |
| 夹具清场（客户/账号/报价/订单/发货/出库/审批实例/凭证/应收/三本账/在途）12 项 | 全 0 |

> 本机在 99（开发机），库在 145（`172.18.10.145:5432`），符合卡面「场景 A」。

### 4.2 步骤轨迹（`11_e2e_nocapture.out`）

```text
[M3-24] ERP 审批回调实现已注册 18 个
[M3-24] 步 1/9 建报价单（2 行 / 13% 税）
[M3-24] 步 2/9 报价单提交审批（quotation_approve）
[M3-24]   审批待办：节点 n_sales / 受派人 01a1b5c9-…
…（三次待办 = 该角色在演练库里有三个成员，全部按库给的受派人点）
[M3-24] 步 9/9 签收确认（出库过账：三本账 + 成本结转凭证 + 挂应收）
[M3-24] 成本结转凭证分录：1405:0.0000/100.0000,1405:0.0000/50.0000,6401:100.0000/0.0000,6401:50.0000/0.0000
[M3-24] 全链通过：QT-20261117-000004 → SO-20261117-000003 → SH-20261117-000003 → OUT-20261117-000003 → 应收 2260.00 / 成本凭证 150.00
test result: ok. 1 passed; 0 failed
```

### 4.3 库侧快照（`13_chain_db_snapshot.txt`，`12_chain_snapshot.sh` 可复现）

| 环节 | 事实 |
|:--|:--|
| 单据链 | `QT-20261117-000004` ACCEPTED → `SO-20261117-000003` OUTBOUND（2260）→ `SH-20261117-000003` POSTED（15 件）→ `OUT-20261117-000003` POSTED（SALES/OTHER，成本 150） |
| 订单明细 | 两行 `delivered_qty` = 10 / 5（数量、单价、税率、金额整列搬自报价单） |
| 三本账 | 物料甲 90/90、物料乙 95/95（成本账 = 库位账），在途 0；流水 `OUT / SALE_OUT` `before 100 → after 90` |
| 应收 | `AR-20261117-000003` SALES_OUT / `OUT-20261117-000003` / 2260 / 未收 2260 / **OPEN** / 期间 2026-11 |
| 凭证 | `PZ-20261117-000003` POSTED，借=贷=150，4 条分录（6401 借 100/50，1405 贷 100/50） |
| 审批实例 | `FLW-…4EAE819D` QUOTATION_APPROVE APPROVED、`FLW-…6C70D3D1` SO_APPROVE APPROVED，无 PENDING |
| 状态轨迹 | 14 行，完整回答「谁在哪一步把单推到哪」；报价单发出那条 reason = `审批通过（FLW-…）自动发出`，转单那条 action = `CONVERT` |
| 全链自洽 | 8 格全 0（无 PENDING 实例 / 应收金额 / 凭证借贷 / 在途 / 出库来源 / 订单终态 / 报价终态 / 孤儿日志） |

## 5. 有牙演练（`20_teeth_break_expectation.out`）

把「应收 = 2260」故意改成 2261 再跑同一条命令：

```text
FAIL 本卡（全链 1 条 + 收尾 1 条）：… 有 1 条失败
assertion `left == right` failed: 应收金额 = 含税 2260，实际 AR-…/2260.0000/OPEN/OUT-…
  left: "2260.0000"
 right: "2261.0000"
== 汇总：PASS=26 FAIL=2 ==
有失败项，见上面的 FAIL 行
（DRILL_EXIT=1）
```

同时验证「红也要收尸」：跑完查 `pg_database` 里 `cp_m324_sales_chain_e2e_%` 的条数 = **0**（trap 照常 DROP）。
改回 2260 后重跑 → `PASS=28 FAIL=0 / ALL PASS`（`10_drill_full.out` 即改回后的那次）。

## 6. 门禁

| 检查 | 命令 | 结果 |
|:--|:--|:--|
| 格式 | `cargo fmt -p cloudpivot -- --check` | 通过（无输出） |
| Lint（库） | `cargo clippy -p cloudpivot --lib -- -D warnings` | `Finished dev profile`，0 warning |
| Lint（含测试目标） | `cargo clippy -p cloudpivot --lib --tests` | 与 `sales_chain_e2e_tests` 相关的告警 0 条 |
| 前端 | — | **本卡零前端改动**，按卡面第 2 条不适用（未跑 `pnpm typecheck/build`） |
| 数据库 | — | **本卡零迁移**（`git status` 无 `migrations/` 变更） |

## 7. 三个真跑才发现的坑（已写进模块文档，避免下一个人再踩）

1. **`cargo test --lib` 里进程级审批回调注册表是空的**。生产由 `bootstrap` 调
   `erp::doc_reader_impl::register_all()`，测试二进制不起 bootstrap ⇒ `dispatch_settled`
   一个实现都找不到，表现成「审批 APPROVED 但报价单仍是 DRAFT」——看着像业务 bug。
   用例里显式注册并断言注册表长度 ≥ 18。
2. **发货过账不扣库存**（M2-02 既定口径）：⑦ 之后库存必须**不变**、凭证/应收 0 张，
   ⑧ 签收确认才动账。在 ⑦ 之后断言「库存已减」是最容易踩的假红。
3. **`biz_flow_state_edge` 只存「被人为改过的边」**，没有行 = 用代码里的默认边表（全允许）。
   前置断言写成「这条边得有一行」会把默认态误判成环境缺件；正确判据是「没有被 `enabled=false` 的边」。

另记：演练库是 `cloudpivot_test` 的克隆，自带演示数据（历史订单 / 其他演练的应收 / 9 个未结审批实例 /
6 条销售链孤儿状态日志）。所以库侧取证脚本必须**按 `M324-CUST` 夹具收窄**，
孤儿日志那条只能「打印清理条数」而不是「断言 0」——否则会被别人的数据染红。
