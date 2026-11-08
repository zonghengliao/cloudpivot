# BUG-01 免审过账失败必须打回草稿（t_f427e5d6）

> 卡面：生产实测 2026-11-08，`IN-20261031-000002` 停在 `PENDING_POST` + 实例 `APPROVED`，
> 六本账一分没动，界面上没有任何接口能推进它（死单）。

## 1. 根因（修复前）

`backend/src/business/erp/stockin_handler_post.rs` 的免审分支：

```rust
tx.commit().await?;                    // ① 单据 → PENDING_POST，实例 → APPROVED
if auto_finished {
    match post_after_approval(..).await {
        Ok(()) => return Ok(()),
        Err(e) if e.code == ERP_SEPARATION_OF_DUTIES => { /* 打回 DRAFT */ }  // 只有 SoD 有兜底
        Err(error) => return Err(error),                                     // ← 其它错误留死单
    }
}
```

免审分支**不产生任何待办**，`PENDING_POST` 也没有人工过账接口 ⇒ 事务已提交后再失败，
单据就永久停在中间态。

## 2. 修复

| 文件 | 位置 | 改动 |
|---|---|---|
| `backend/src/business/erp/stockin_handler_post.rs` | `submit_post_approval` 免审分支（L244-281） | 删掉 `ERP_SEPARATION_OF_DUTIES` 特判；**任何** `Err` 都调 `restore_after_post_rejection` 打回 DRAFT；打回成功记 WARN，打回失败记 ERROR（含原始错误 + 打回错误）；原始错误照常 `return Err` 给前端 |
| `backend/src/business/erp/stockin_bridge.rs` | `settle_post` 的 `PostVerdict::Execute`（L481-518） | **同源漏洞**：审批通过回调里 `post_after_approval` 失败时原来只 WARN「等待人工处置」，单据同样停在 `PENDING_POST` 且无待办。现同样打回 DRAFT（原因带原始错误），打回失败记 ERROR |

打回原因写进 `biz_status_log.reason`（由 `state_machine::transition` 落库），格式：

```
免审过账失败：<原始错误消息>，已打回草稿
过账审批已通过，但过账失败：<原始错误消息>，已打回草稿（修正后可重新提交）
```

`restore_after_post_rejection` 本身已写 `biz_status_log`（`PENDING_POST → DRAFT` + reason），
并清空 `approval_instance_id`，因此打回后可以重新提交。

## 3. 兄弟单据审计（`grep -rn "auto_finished" backend/src`）

| # | 单据 | 定位 | 免审后动作 | 失败后单据停留 | 结论 |
|---|---|---|---|---|---|
| 1 | 收货入库（免审直过） | `stockin_handler_post.rs:244` | `post_after_approval`：六本账 + 超收/供应商/单位三道闸 | `PENDING_POST`，**无待办、无人工过账接口** | ✅ **已修** |
| 2 | 收货入库（审批通过回调） | `stockin_bridge.rs:481` | 同上 | 同上 | ✅ **已修（同源）** |
| 3 | 收款退款（receipt_refund） | `receipt_refund.rs:2074` | `on_refund_settled` → 过账（凭证） | `CONFIRMED`（状态没动过） | ⚪ 非死单：界面仍有「提交审批」按钮，且前置（`CONFIRMED` + 核销满）依然成立，可原地重试；失败原因只在日志（残余可用性缺陷，非卡死） |
| 4 | 合同（contract） | `contract_handler.rs:757` | `on_contract_settled` → `PENDING_APPROVAL → SIGNED`（唯一业务前置：三日期齐全，不齐则跳过） | `PENDING_APPROVAL` | ⚪ 非死单：合同有「审批中 → 草稿」手动撤回入口（`contract_handler.rs` ⑤ 段），补日期后可重提；失败原因只在日志 |
| 5 | 采购订单 | `purchase_handler.rs:1508` | `on_po_settled` → 纯状态回写（无账、无凭证） | `SUBMITTED` | ⚪ 失败面只剩 DB / CAS：CAS=0 意味着单据已被别的路径推走。**补偿写不可行**（失败的就是写状态本身），`SUBMITTED → DRAFT` 边存在但前端无撤回按钮 ⇒ 需人工/客服介入（残余风险：低概率 + 需人工） |
| 6 | 销售订单 | `sales_order_handler.rs:1485` | 纯状态回写 | `SUBMITTED` | ⚪ 同上 |
| 7 | 采购申请 | `purchase_requisition_handler.rs:605` | 纯状态回写 | `SUBMITTED` | ⚪ 同上 |
| 8 | 采购退货 | `purchase_return_handler.rs:803` | 纯状态回写 | `SUBMITTED` | ⚪ 同上 |
| 9 | 预算 | `budget_handler.rs:675` | 纯状态 + 同槽旧版退位（同一事务） | `SUBMITTED` | ⚪ 同上 |
| 10 | 盘点（stocktake） | `stocktake_handler.rs:529` | 提交只到 `PENDING_POST`；**审批通过不自动过账** | `PENDING_POST` | ✅ **不适用**：「过账」是独立人工动作（有按钮），不存在「没人能推进它」 |
| 11 | 出库（stockout） | `stockout_handler.rs:1164/1170` | `submit_pending` 只改状态，无免审直过 | `PENDING_POST` | ✅ **不适用**：过账走人工 `POSTED` 入口 |
| 12 | 调拨（transfer） | `transfer_handler_status.rs:change_transfer_status` | 无 `auto_finished`（人工流转 + 制单≠过账闸） | — | ✅ **不适用** |

**判据**（本卡采用的分类）：只有当「提交后动作」是**独立于状态写入的业务动作**
（记账 / 生成凭证 / 校验来源单据）时，失败才会把单据留在「状态已推进、业务没发生、
无待办」的死单态 —— 这类必须打回。若「提交后动作」本身就是写状态，又或单据在被推进到
中间态后仍有可用入口（按钮/撤回），失败不会产生死单，只需告警。

## 4. 测试

新增 3 条真库用例（`backend/src/business/erp/stockin_handler_tests_pflow3a.rs`）：

| 用例 | 场景 | 断言 |
|---|---|---|
| `bug01_auto_post_over_receive_reverts_to_draft` | 采购单 100 件双开两张入库单；A 先免审过账把采购单收满，B 再 `submit-post` | 409 + 单据 `DRAFT` + `approval_instance_id` 清空 + `biz_status_log` 有 `PENDING_POST → DRAFT` 且 reason 含「免审过账失败」+ 原始错误 + 库存只有 A 的 100 + 重提仍是可重试的普通错误且回到 `DRAFT` |
| `bug01_auto_post_supplier_mismatch_reverts_to_draft` | 绕过接口 `UPDATE biz_stock_in.supplier_id` 换供应商（金额 4 元 → 免审） | 400 `ERP_SI_SUPPLIER_MISMATCH` + 单据 `DRAFT` + 状态日志同上 + 库存一分没动 |
| `bug01_auto_post_success_still_posts` | 正常小额单（回归） | `POSTED` + 库存 2 + 采购单 → `PARTIAL_RECEIVED` |

**负向验证（证明用例真的能抓住这个 bug）**：把免审分支临时还原成修复前写法
（`Err(error) => return Err(error)`）后重跑，两条用例按时失败：

```
assertion `left == right` failed: 免审过账失败必须打回 DRAFT —— 停在 PENDING_POST 就是死单（BUG-01）
  left: "PENDING_POST"
 right: "DRAFT"
```

即生产事故里那张单的状态被精确复现。

### 全量回归

隔离库 `cp_bug01_e2e`（空库 + 迁移 + `NIK_SEED_DEMO_USERS=true` 演示组织种子）：

```
test result: FAILED. 3814 passed; 24 failed; ... finished in 1430.22s
```

24 条失败全部不在本次改动范围，逐条复跑后：

- **21 条**是「同一模块内用例并行执行」的测试隔离问题（同一条 `uk_flow_linkage_rule` / 同夹具互相踩），
  加 `--test-threads=1` 后全绿：`asset_linkage_tests`(11)、`expense_linkage_tests`(6)、
  `voucher_linkage_tests`(6)、`payable_bridge::tests`(1)、`receivable_aging`(1)
- **1 条** `report_pdf::tests::pdf07_real_three_reports_agree_on_shape`：`report_pdf.rs` 里
  `grep -c "stockin\|stock_in"` = **0**，与本改动无调用关系；断的是「本期货币资金必须有数」
  （依赖该库的期次/期初演示数据）
- **2 条** `return_pdf::db_tests::*`：断的是 `html.contains("<th>价税合计</th>")`，而模板
  `pdf/templates/return_doc.html.tera:230` 自 PDF-04（commit 2ac33d1「退货单表头 th 带列宽 class」）
  起就是 `<th class="c-with-tax">价税合计</th>` —— **断言已过期**，是 main 上的既有红，与 BUG-01 无关

本次新增的 3 条 + 全部 `stockin` 用例在隔离库上全绿（见 `runs.md`）。
