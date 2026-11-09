# E2E07-FIX-4 证据：路径②实例语义 + CLOSED/COMPLETED 文档 + 归档前提示

卡：`t_e0b82129`（P3×1 + P4×3）。四条问题、三种处理方式：**① 改代码**、**②③ 改文档**、
**④ 改前端**（后端与口径按已定设计决策保持不变）。

## 验收对照

| # | 卡面验收 | 结论 | 证据 |
|---|---------|------|------|
| ① | 路径②登记签订 → 审批实例 TERMINATED、待办 CANCELLED、状态日志有记录 | **PASS** | `live-145.txt`（真机 33 条断言全绿）、`live-145-db.txt`（库内实例/待办/节点/日志）、`unit_contract_db.txt`（真库用例 `manual_sign_from_pending_terminates_running_instance`）、`teeth.txt`（摘掉修复即红） |
| ② | 文档里 `target_status` 统一为 `COMPLETED`（不再出现 `CLOSED`） | **PASS** | `docs/api-contract-quickref.md` §9 新增合同章节（❌CLOSED / ✅COMPLETED + 出处）；`docs/review/全业务演练用例清单-v1.0.md` E2E-07 五处字面量按真实枚举更正（07-003/004/006/007/016）+ 表下口径注 |
| ③ | 合同手册说明「服务商/出租方建在供应商档」 | **PASS** | 新增 `docs/manual/23-合同管理.md` §三（四类合同的乙方主档对照表 + 「系统只有客户/供应商两种主档」+ D1 出处 + 已知代价）；`docs/manual/README.md` 补编目录登记 |
| ④ | 归档按钮显示「仍有 N 张未关闭单据」提示 | **PASS** | `contract-logic.ts::openOrderWarning` + `index.vue` 确认弹窗警示条（`data-testid="contract-archive-open-orders"`）；前端单测 ⑱（`frontend.txt`）；真机 `open_order_count=1` 且归档 200（`live-145.txt` ⑤） |
| ⑤ | cargo test + 前端 build + push | **PASS** | `unit_contract_db.txt`（10 passed 真库）、`full_suite_nodb.txt`（3957 passed / 0 failed）、`frontend.txt`（typecheck rc=0 / build rc=0 / 单测 15 passed）、`live-145.txt`（PASS=33 FAIL=0） |

## ① 路径②终止审批实例（代码）

改的是 `contract_handler::contract_change_status` 的**路径②分支**（`PENDING_APPROVAL → SIGNED`，
即「合同线下已签、系统补登记」）：新增 ⑤-b，**同事务**调
`contract_bridge::terminate_for_out_of_band_sign` → `engine::terminate_instance`。

| 项 | 改之前 | 改之后 |
|---|---|---|
| 合同状态 | `SIGNED` | `SIGNED`（不变） |
| 审批实例 | 仍 `PENDING` | `CANCELLED` + `ended_at` + `result_comment` |
| 审批待办 | 仍 `PENDING`（挂在审批人处） | 全部 `CANCELLED`（留痕，不删行） |
| 实例节点 | 仍 `ACTIVE` | 收口（无 `ACTIVE/PENDING`） |
| 状态日志 | 只有「签订」 | reason 追加「审批中心外登记签订：审批实例已终止（待办一并取消）」 |
| 实例 id | 保留 | **保留**（审计，详情仍显示 FLW 单号） |
| 通知副作用 | — | 事务提交**之后** `flush_effects`（失败不回滚签订） |

**为什么状态值写 `CANCELLED` 而不是 `TERMINATED`**：`ck_biz_flow_instance_status` 只有
`PENDING / APPROVED / REJECTED / CANCELLED / WITHDRAWN`，全仓 15 个业务桥把 `CANCELLED`
读作「行政终止」（见 `engine::terminate_by_admin` 头注释；E2E01-FIX-3 的终止入口同口径）。
卡面写的 TERMINATED 是**语义**（终止），不是字面量。

**只对「在跑的」实例动手**：实例不存在（弱引用残留）→ 跳过不报错；实例已 `APPROVED` →
保留原结论（把已批的实例改写成 CANCELLED 会抹掉「谁批的」）。

## 真机证据（`live-145.txt` / `live-145-db.txt`）

145 上起隔离实例：debug 二进制 = 本卡提交，端口 **8034**，库 **`cp_fix4_e2e`（全新空库，
迁移 284/284、最大 V362）**，`/tmp/fix4_probe.env`（由 `cloudpivot-dev.env` 改写库名与端口）。
探针 `scripts/verify/e2e07_fix4_probe.sh`：真 token、真 HTTP、真库断言。

```
### 2) 路径② 登记签订：PATCH {target_status:SIGNED}
  ✅ 登记签订 HTTP/CODE = 200/c0
  ✅ 合同状态（签订后） = SIGNED
  ✅ 合同仍指向原实例（审计） = 01a1bfab-7206-76d7-93ba-4f7ebe24b7c7
  ✅ ① 审批实例状态 = CANCELLED
  ✅ ① 实例 ended_at 已写 = 1
  ✅ ① 实例 result_comment 含「审批中心外登记签订」
  ✅ ① 待办 PENDING（应为 0） = 0
  ✅ ① 待办 CANCELLED（应=原条数） = 1
  ✅ ① 实例节点未收口数（应为 0） = 0
  ✅ ① 状态日志 reason 含「审批实例已终止」
### 3) 已签再签（幂等闸）：应 409，且不重复终止
  ✅ 重复登记签订 HTTP = 409
### 4) ② 归档字面量：CLOSED 不存在（应 400 + 10001）
  ✅ CLOSED HTTP/CODE = 400/c10001
### 5) ④ 归档不硬拦（E4）
  ✅ ④ 详情 open_order_count（前端提示数据源） = 1
  ✅ ④ 归档（COMPLETED）HTTP/CODE = 200/c0
  ✅ ④ 合同状态 = COMPLETED

================ 结果：PASS=33 FAIL=0 ================
```

库内（`live-145-db.txt`）：

```
FLW-20261109-89B94BAA|CONTRACT_APPROVE|CANCELLED|t|审批中心外登记签订：审批实例已终止（待办一并取消）
待办：CANCELLED|1|all_completed=t|comment 同上
节点：APPROVED|2（历史）+ CANCELLED|1 —— 无 ACTIVE/PENDING
状态日志：PENDING_APPROVAL|SIGNED|审批中心外登记签订：审批实例已终止（待办一并取消）
归档：HT-20261109-000002|COMPLETED|open_orders=1（E4：不硬拦）
```

> 145 现状：`/etc/cloudpivot/cloudpivot-dev.env` 指向的 `cloudpivot_dev` 已不存在，主服务
> （release / 8023）未起；本卡用**全新空库 + 8034 隔离实例**取证，**没碰 144 生产**，
> 也没往别人的库写数据（探针自建的合同 / 采购单 / 审批人只落在 `cp_fix4_e2e`）。
> 采集过程中踩到两个环境坑，都写进了探针脚本的注释：全新库种子账号
> `must_change_password=true`（服务端缓存键带前缀，实测 `cloudpivot:uv:<id>`，改库后要按
> pattern 删键，否则 403「首登未改密」）；全新库只有 admin，审批节点按角色展开时
> 采购经理为空 → 提交 422 + `65018`，所以探针自建 `fix4_appr`（`PURCHASE_MANAGER`）。

## 有牙齿的证明（`teeth.txt`）

把 ⑤-b 整段临时摘掉（`let mut terminated = None;` 占位）后重跑同一条真库用例：

```
test business::erp::contract_approval_tests::manual_sign_from_pending_terminates_running_instance ... FAILED
assertion `left == right` failed
  left: Some("PENDING")
 right: Some("CANCELLED")
test result: FAILED. 0 passed; 1 failed
```

还原后 `unit_contract_db.txt`：`10 passed; 0 failed`。

## 改了哪些文件

| 文件 | 改了什么 |
|---|---|
| `backend/src/business/erp/contract_handler.rs` | ⑤-b 路径②终止实例（同事务）+ 状态日志 reason + 提交后发通知；接口文档补 5-b 步 |
| `backend/src/business/erp/contract_bridge.rs` | 新增 `terminate_for_out_of_band_sign` + `OUT_OF_BAND_SIGN_NOTE` |
| `backend/src/business/erp/contract_approval_tests.rs` | 新增 `manual_sign_from_pending_terminates_running_instance`（真库，10 条断言） |
| `frontend/src/views/business/erp/contract/contract-logic.ts` | 新增 `openOrderWarning`，`transitionConfirmText` 接未关闭单据数 |
| `frontend/src/views/business/erp/contract/index.vue` | 归档时取 `open_order_count`（列表行按需拉详情）+ 弹窗警示条 |
| `frontend/tests/unit/erp-contract.test.ts` | 新增 ⑱（0 张不提示 / 非归档不提示 / 详情字段同源） |
| `docs/api-contract-quickref.md` | 新增 §9 合同章节（COMPLETED≠CLOSED、路径②语义、D1、E4） |
| `docs/review/全业务演练用例清单-v1.0.md` | E2E-07 五处状态字面量按真实枚举更正 + 口径注 |
| `docs/manual/23-合同管理.md` · `docs/manual/README.md` | 新增合同手册（乙方主档 / 6 态 / 路径② / 归档提示） |
| `scripts/verify/e2e07_fix4_probe.sh` · `e2e07_fix4_setup_145.sh` | 真机探针 + 145 隔离实例一键起 |
