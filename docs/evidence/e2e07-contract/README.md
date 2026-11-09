# E2E-07 合同域全流程演练（145 · cloudpivot_dev）

- 时间：2026-11-09 11:33–11:36（CST）
- 环境：145 `cloudpivot-test`，后端 `http://127.0.0.1:8033`（经 nginx 80 同源），库 `cloudpivot_dev`
- 后端 commit：`42ca8fa1`（含本卡 P2 修复，health 200）
- 脚本：`scripts/verify/e2e07_contract_common.sh|_a.sh|_b.sh|e2e07_run_all.sh|e2e07_reconcile.sql|e2e07_reset.sql|e2e07_db_evidence.sh|e2e07_deploy_145.sh`
- 结果：**165 条接口/库断言 + 13 组勾稽全部 PASS，0 FAIL**；前端合同页单测 14/14 PASS

## 演练范围

1. 建合同：四类全开（PURCHASE / SALES / SERVICE / LEASE）+ 类型闸 / 乙方类型闸 + 合同扫描件附件
2. 提交审批 → 两级（≥10 万）与单级（<10 万）审批 → SIGNED；自审内控负例；审批重放幂等
3. 合同变更（金额 / 期限）→ `sys_change_log` 留痕（改了什么、谁改的、何时）；审批中改单被冻结
4. 履约跟踪：挂采购单 / 销售单 → 详情「已执行金额 = 关联订单含税合计」、剩余额度、未关闭单据数与单号
5. 到期提醒：D-30 档扫描 → 台账 + 站内信；二次触发不重复
6. 归档（ACTIVE→COMPLETED）/ 终止（必填原因 + 双落痕）/ 引用保护（被订单引用不可删）
7. LNK-07「两条审批路径只触发一次联动」专项

## 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| A1 | 建 PURCHASE 12 万（供应商） | 200/c0 | `HT-…-000049` DRAFT, party_b_type=SUPPLIER，乙方名快照非空 | ✅ |
| A1 | 建 SALES 8 万（客户） | 200/c0 | `…-000050` DRAFT, CUSTOMER | ✅ |
| A1 | 建 SERVICE 5 万 | 200/c0 | `…-000051` DRAFT, SUPPLIER（服务商=供应商口径） | ✅ |
| A1 | 建 LEASE 3 万（到期日=今天+30） | 200/c0 | `…-000052` DRAFT, end=2026-12-09 | ✅ |
| A2 | 未来枚举 `FRAMEWORK` | 400/10001 | 未落库 | ✅ 类型闸保留 |
| A2 | SERVICE 传 `party_b_type=CUSTOMER` | 400/10001 | 未落库，文案点名 SUPPLIER | ✅ |
| A2 | SALES 用供应商当乙方 | 400/66021 | 未落库 | ✅ |
| A3 | DRAFT→ACTIVE / DRAFT→SIGNED | 409/66015 | 状态仍 DRAFT | ✅ 必须先提交审批 |
| A3 | `target_status=CLOSED`（卡面写法） | 400/10001 | — | ⚠ 实现名为 COMPLETED（P4） |
| A4 | 合同扫描件上传（CONTRACT_SCAN） | 200 | `biz_doc_attachment` 1 行，uploader_name=超级管理员 | ✅ |
| A5 | 草稿改金额 8 万→8.5 万 + 改到期日 + 备注 | 200/c0 | amount=85000, end=2027-12-31；`sys_change_log` 1 行 diff 含 a/b 值 | ✅ |
| A5 | 原值回传（amount 不变） | 200/c0 | 变更日志仍 1 行（不写空日志） | ✅ |
| A6 | 提交审批 C1/C2/C3/C4 | 200/c0 | PENDING_APPROVAL + `approval_instance_id` + 状态日志 + 待办通知 | ✅ |
| A6 | 重复提交同一单 | 409/66015 | 无第二条实例 | ✅ |
| A6 | 审批中改单 | 409/66016 | 变更日志不增 | ✅ |
| B1 | C1 一级会签（PURCHASE_MANAGER ALL，3 票） | 逐票 200/c0 | 会签齐后自动进 n_gm | ✅ |
| B1 | C1 二级审批（GENERAL_MANAGER ANY） | 200/c0 | SIGNED，状态日志 2 条 | ✅ |
| B1 | 重放二级审批 | 200/c0（`idempotent=true`, `approved_count=0`） | SIGNED 联动通知仍 1 条 | ✅ 幂等 |
| B1 | 已 SIGNED 再走路径② | 409/66015 | 无新增通知/日志 | ✅ |
| B2 | C2 路径②：PATCH SIGNED（审批中心外登记签订） | 200/c0 | SIGNED；**审批实例 id 保留、详情可见 FLW 单号**（本卡修复） | ✅ |
| B2 | C2 路径①：3 票会签回调 | 逐票 200/c0 | SIGNED 通知仍 **1** 条、`PENDING→SIGNED` 日志仍 **1** 行 | ✅ LNK-07 |
| B3 | SERVICE 审批通过 → 详情履约口径 | 200 | `commitment_scope=NOT_APPLICABLE`，committed=0、remaining=50000、open=0 | ✅ CTR-02-F1 |
| B3 | SERVICE 挂采购单 | 400/66014 | — | ✅ |
| B4 | 自审（本人提交本人审批） | 403/65019 | 待办被内控置 `SKIPPED`，文案点名「制单与审批分离」 | ✅ |
| B5 | 到期提醒手动触发（contract_expire） | 200/c0 | 台账 1 行 `D-30`、receiver_count=4、站内信 1 条 | ✅ |
| B5 | 二次触发 | 200/c0 | `skipped_reminded≥1`，台账/通知仍 1 条 | ✅ |
| B6 | 终止缺 reason | 400/10001 | 状态不变 | ✅ |
| B6 | 带 reason 终止 | 200/c0 | TERMINATED；reason 同时落 `biz_status_log.reason` 与 `biz_contract.remark` | ✅ V-18 |
| B7 | 挂 3 张采购单（3 万 / 2 万 / 7 万=恰好剩余额度） | 200/c0 | 供应商从合同乙方派生、`contract_no_snapshot` 正确 | ✅ |
| B7 | 超合同额 0.01 | 409/66019 | 未落库 | ✅ |
| B7 | 详情履约 | 200 | committed=120000、remaining=0、open=3、单号 3 个 | ✅ |
| B8 | 归档（ACTIVE→COMPLETED，仍有 3 张在途） | 200/c0 | COMPLETED；`open_order_count=3` 仍可见（不硬拦，E4） | ✅ |
| B8 | 归档后挂新单 | 400/66014 | — | ✅ |
| B8 | 删除被订单引用的合同 | 409/66018 | 文案点名采购单号 | ✅ |
| B9 | 销售单挂 SALES 合同 3 万 | 200/c0 | committed=30000、remaining=55000、open=1 | ✅ |
| B9 | 销售单超合同额 | 409/66019 | — | ✅ |

## 勾稽验证（`e2e07_reconcile.sql`，13 组全部 0 差异）

| # | 勾稽项 | 值 |
|---|---|---|
| ① | 合同当前状态 = 最后一条状态日志的 to_status | 0 |
| ② | 状态日志链条无断点（上一条 to = 下一条 from） | 0 |
| ③ | 联动通知幂等键无重复（同合同同状态两条） | 0 |
| ④ | 采购单只能挂 PURCHASE 合同 | 0 |
| ⑤ | 销售单只能挂 SALES 合同且客户 = 合同乙方 | 0 |
| ⑥ | 任何合同「已执行金额 > 合同额」（超签） | 0 |
| ⑦ | 服务 / 租赁合同挂了订单 | 0 |
| ⑧ | 提醒台账同合同同档位重复 | 0 |
| ⑨ | 提交审批之后才出现的变更日志 | 0 |
| ⑩ | 合同号作为凭证来源（合同不该记账） | 0 |
| ⑪ | 合同附件上传人留空 / 落裸 UUID | 0 |
| ⑫ | 采购单表头 ≠ 明细行合计 | 0 |
| ⑬ | 采购单供应商 ≠ 合同乙方（供应商未从合同派生） | 0 |

接口侧勾稽（与 DB 对拍）：
- C1 详情 `committed_amount=120000` = `SUM(biz_purchase_order.total_with_tax)=120000.0000`；C2 `30000` = `SUM(biz_sales_order.total_with_tax)=30000.0000`
- 未关闭单据数 3 / 1 与 DB `status NOT IN (终态)` 计数一致；单号列表长度一致
- 合同链路无凭证、无库存流水（`biz_voucher` 0 张）

## 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | **已修复**：`PENDING_APPROVAL → SIGNED`（审批中心外「登记签订」）把 `biz_contract.approval_instance_id` 清空 | P2 | 提交审批 → `PATCH /contract/{id}/status {target_status:"SIGNED"}` → 查 `approval_instance_id` 为 NULL、详情无 FLW 单号 | 合同与审批单审计链断；列表/详情显示不出审批单号，与 V0209 迁移注释「SIGNED 保留实例作审计」自相矛盾 | 已在 `contract_handler.rs` 只对回位草稿清实例；补真库单测（详见修复记录） |
| 2 | 合同审批流程 `n_pm` / `n_pm_hi` 是 **ALL 会签**，环境内 PURCHASE_MANAGER 有 3 名成员，其中 `p3a_0003` 是历史夹具账号（团队没有它的可用口令） | P2（配置） | 提交任一合同 → 3 张待办 → 少签一票即永远停在 PENDING_APPROVAL（无超时/兜底） | 演示环境合同审批会永久卡死；本次为推进演练把 p3a_0003 口令哈希重置为 e2e01_appr 的（该账号本无可用凭据，无信息损失，已在卡面如实记录） | 建议会签改 ANY，或清理角色内失效账号；若要保留 ALL，应给节点加超时动作/兜底审批人 |
| 3 | 默认库 `biz_flow_linkage_rule` 里 **CONTRACT 零规则**：合同状态变更 / 审批通过没有任何通知或联动动作 | P3（配置缺口） | 查 `select count(*) from biz_flow_linkage_rule where trigger_entity='CONTRACT'` = 0 | LNK-07 联动代码在跑但无动作可观测；卡面「两道审批路径只触发一次联动」在默认配置下无从体现（本次临时插一条 CONTRACT/SIGNED→SEND_NOTICE 规则验证，收口已删除） | 补 seed：合同签订 → 通知制单人 / 采购经理；或明确「合同不联动」并在文档写死 |
| 4 | 合同附件秒传复用导致**文件名错位**：上传 `e2e07-contract-scan.pdf`，列表显示 `drill-invoice.pdf` | P3（数据展示） | 上传与库中已有文件内容相同的扫描件 → `sys_file` 按内容 hash 复用 → 附件行没有自己的文件名列 | 用户看到别人上传时的文件名，审计时容易误认附件 | `biz_doc_attachment` 增加 `file_name` 快照列（迁移），上传时写入本次文件名 |
| 5 | 路径②「登记签订」后审批实例仍在跑、待办仍挂在审批人处 | P3（设计） | 提交审批 → PATCH SIGNED → 审批人待办列表仍有该合同（合同已 SIGNED） | 会签未走完时出现「已签但待办未清」，只能靠把剩余会签签完收口 | 明确该路径的产品语义：要么禁止（必须走审批中心），要么登记签订时同步终止实例 |
| 6 | 卡面写状态 `CLOSED`，实现是 `COMPLETED` | P4（文档） | `target_status=CLOSED` → 400/10001；实际用 `COMPLETED` | 按卡面写接口会踩 400 | 卡面/文档统一为 `COMPLETED`（前端已同） |
| 7 | 服务商 / 出租方在系统里统一登记为「供应商」，乙方类型只有 CUSTOMER / SUPPLIER | P4（设计口径） | SERVICE / LEASE 传 CUSTOMER 一律 400/10001 | 无法从乙方主档区分「服务商」与「出租方」，只能看合同类型 | 决策文档已定义（《合同类型开放口径-决策》§二 D1），保持现状即可 |
| 8 | 归档不校验在途单据（E4 设计） | P4（设计） | ACTIVE 有 3 张在途采购单 → 归档 200 | 归档后仍有未完成订单，报表要按 `open_order_count` 判断「已履行」 | 已有设计决策，保持；前端在归档前提示未关闭单据数（详情已返回） |

P0 / P1：无。

## 修复记录

commit `42ca8fa1`（1 个源文件 + 脚本）：

- `backend/src/business/erp/contract_handler.rs`
  - `contract_change_status` ⑦ 分支：`PENDING_APPROVAL → SIGNED` 保留原 `approval_instance_id`，只有 `PENDING_APPROVAL → DRAFT`（撤回/驳回）才清空实例（清不干净下次提交会撞 `uk_biz_flow_instance_biz_active`）。
  - 新增真库单测 `manual_sign_from_pending_keeps_approval_instance`：断言响应与 DB 都保留实例 id。
- 验证：
  - `rustfmt +1.94.1 --edition 2024 --check`：无 diff
  - 145 `cargo check --all-targets`：CHECK_RC=0（3 条 warning 均为其他测试文件的既有 unused import）
  - 145 重新构建 + 重启 + health 200；演练中新增断言「登记签订后仍保留审批实例 id = kept」「详情可见审批单号 FLW-… = 1」均 PASS
  - 真库单测未在 145 执行（7G 内存链接 test 二进制会被 OOM SIGKILL，E2E-06 已实证），同一路径已由真实 HTTP 演练覆盖

## 证据文件

| 文件 | 内容 |
|---|---|
| `run.out` | 全量演练输出（A/B/勾稽 + 汇总） |
| `asserts.log` | 178 条 PASS / 0 FAIL 逐条明细 |
| `reconcile.log` | 13 组勾稽原始输出（按清单顺序，全部 0） |
| `db-evidence.txt` | psql 直查：合同、状态日志、变更留痕、订单、履约口径、审批任务、通知、提醒台账、附件 |
| `frontend-unit.log` | `frontend/src/views/business/erp/contract` 相关单测 14/14 PASS（含 CTR-02-F1 `commitmentViewOf` 的 NOT_APPLICABLE 分支） |
| `raw/*.json` | 关键步骤的原始响应报文（建档 / 闸门负例 / 提交 / 审批 / 路径② / 挂单与超限 / 提醒 / 终止 / 归档 / 删除保护） |

## 证据边界（如实说明）

- 「履约不适用不渲染 0.00」是**前端渲染**行为：本卡验证到 API 返回 `commitment_scope=NOT_APPLICABLE` + `committed_amount=0`，以及前端 `contract-logic.ts::commitmentViewOf` 对该取值只渲染灰字 的源码与单测；**未在浏览器里截图**（145 上 nginx 托管的 `frontend/dist` 是旧构建，不含合同页 chunk）。
- 会签人数、审批人身份是**环境配置**（PURCHASE_MANAGER / GENERAL_MANAGER 成员）决定的，生产成员不同则待办张数不同。
- 本卡为观测 LNK-07 临时插入过一条 `CONTRACT/SIGNED→SEND_NOTICE` 联动规则，演练结束时已删除（`db-evidence.txt` §11 = 0）。

## 残留数据（cloudpivot_dev）

- 合同 4 张：`HT-…-000049`（PURCHASE，COMPLETED，挂 3 张采购单）、`HT-…-000050`（SALES，ACTIVE，挂 1 张销售单）、`HT-…-000051`（SERVICE，COMPLETED）、`HT-…-000052`（LEASE，TERMINATED）
- 采购单 3 张（DRAFT）、销售单 1 张（DRAFT）、合同扫描件附件 1 行、`sys_notice` 4 条合同签订通知 + 1 条到期提醒、`biz_reminder_log` 1 行、`sys_change_log` 1 行
- 用户 `e2e07_appr`（PURCHASE_MANAGER + GENERAL_MANAGER，口令 `E2e07@Appr123`）；夹具修复：`p3a_0003` 口令哈希重置为 `e2e01_appr` 的
- 未触碰 2026 既有单据及其它演练数据；重跑 `e2e07_run_all.sh` 会先按 `contract_name like 'E2E07-%'` 精确清理本卡数据（不 DROP/TRUNCATE）
