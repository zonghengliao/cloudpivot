# E2E-05 资产域全流程演练（145 测试机 · cloudpivot_dev · 演练年度 2027-12 ~ 2028-06）

> 环境：145 `http://172.18.10.145`（nginx 80 → 后端 `127.0.0.1:8033`），库 `cloudpivot_dev`。
> 账号：`admin`（SUPER_ADMIN，制单/建卡）+ 本卡自建 `e2e05_gm`（GENERAL_MANAGER，处置审批人；
> `erp.separation_of_duties.approve=true`，制单人不能自审）。
> 代码：`c11fb863`（2 个 P2 修复）→ 145 `cargo build`（debug 1m12s）→ `systemctl restart cloudpivot-test`
> → health 200（10:22:39）。
> 结论：**3/3 页面全通**；断言 **100 + 85 + 78 = 263 条全绿**；发现并当场修 **2 个 P2**（均 1 文件），
> 另登记 P3×2 / P4×3。

## 为什么演练期间用 2027-12 ~ 2028-06

`cloudpivot_dev` 是共享演练库：2026 全年是 E2E-01/02/03 的业务年，2022 是 E2E-04 的年结年（已 12 期封账）。
2027/2028 在库里**一期都没有**（实测 `biz_accounting_period` 0 行），因此：

```bash
POST /business/erp/period/batch-open {"from":"2027-12","to":"2028-06"}   # created 7 期
```

购入日 `2027-12-01` ⇒ 起提日 `2028-01-01`（当月增加次月提），处置落在 `2028-05`，作废后 2028-06 继续提。
该期间段内**全库没有其它资产**（库内仅 p004 的 2 台已处置卡，DISPOSED 不进扫描），所以计提只命中本卡数据。

## 演练范围（3 个页面 + 联动）

| # | 页面 | 覆盖接口 |
|---|---|---|
| ① | 资产分类 | `GET /erp/asset-category/{page,list,next-code}`、`POST/PUT/DELETE /erp/asset-category[/{id}]`（含空编码自动取号 `AC-07`） |
| ② | 资产卡片 | `GET /erp/asset/page`、`GET /erp/asset/{id}`、`POST /erp/asset`（建卡即入账 + 购入凭证）、`PUT /erp/asset/{id}`、`DELETE /erp/asset/{id}`、`POST /{id}/transfer`、`POST /{id}/custodian`、`POST /{id}/status`、`POST /{id}/dispose`、`POST /{id}/dispose/cancel` |
| ③ | 折旧计提 | `POST /erp/depreciation/run`、`GET /erp/depreciation/page`、`GET /erp/depreciation/{id}`、`POST /erp/depreciation/{id}/reverse` |
| 联动 | 审批 + 通知 + 附件 | `GET /business/flow/task/todo`、`POST /business/flow/task/{id}/approve`、`POST /business/attachment/upload`、`sys_notice(FIXED_ASSET)` |

演练资产（全部走真实接口，购入日统一 2027-12-01，E 例外）：

| 卡 | 分类（月数/残值率） | 原值 | 残值 | 可折旧总额 | 月折旧 | 用途 |
|---|---|---|---|---|---|---|
| A | E2E5_EQ（60 / 5%） | 120 000 | 6 000 | 114 000 | 1 900.0000 | 停用/启用、转移、使用人、红冲、6 期折旧 |
| B | E2E5_3M（3 / 5%） | 1 000 | 50 | 950 | 316.6667 | **尾差 + 提足即停**（316.6667/316.6667/**316.6666**） |
| C | E2E5_DIS（12 / 5%） | 60 000 | 3 000 | 57 000 | 4 750.0000 | **出售处置**（售价 50 000 + 销项 6 500） |
| D | E2E5_DIS（12 / 5%） | 3 000 | 150 | 2 850 | 237.5000 | 闲置照提 + **报废处置 → 作废处置** |
| E | E2E5_3M（3 / 5%） | 500 | 25 | 475 | 158.3333 | 删卡闸门（购入日 2028-06-01，尚未起提） |

## 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| A·① | 分类分页/启用下拉/预览编码 `AC-07`；建 3 个手工分类（60/3/12 期、5% 残值）+ 1 个**空编码自动取号**（`AC-07`）；重复编码 409、残值率 1.5 → 400/10001、月数 0 → 400/10001；改分类改名+残值率 0.05→0.03；删空分类 200（软删）；**删有卡片的分类 409** | 200/400/409 | `biz_asset_category` 4 增 1 改 1 软删 | ✅ |
| A·② | 建卡 5 台（**建卡即 IN_USE + 购入凭证 POSTED**，起提日 = 购入次月 1 日）；残值/可折旧总额/月折旧三算法一致；改卡（名称/规格/地点生效、原值 999999 被忽略）；部门转移（设备部→采购部，留痕 + 部门名快照）；使用人变更（→ 超级管理员，留痕）；停用 IDLE（留痕 + `biz_status_log` + **FIXED_ASSET/IDLE 通知**），重复停用 409/66046 且不重复发通知，`/status` 传 DISPOSED → 400；启用 IN_USE（**先被附件规则拦 400，补 `INVOICE_SCAN` 后 200**，通知 +1，幂等键按事件维度可逆边照发）；删除：未计提但**已有购入凭证 → 409**（本卡修复）、已计提 → 409、已处置 → 409 | 200/400/409 | `biz_fixed_asset` 5 行、`biz_fixed_asset_change` 4 行、`biz_status_log` 4 行、`sys_notice` 2 行、（附件 1 行） | ✅（含本卡修复） |
| B·③ | 计提 2028-01（4 台 / 7 204.1667）→ 幂等二次 `EXISTS` → **红冲**（红字凭证 is_red=1、明细 REVERSED、卡片回滚 0/0 期、1602 净额 0）→ **重跑**（复用同一批次行、`REVERSED_RERUN`、新凭证号）→ 2028-02（D 闲置照提 237.50、A 已转采购部 ⇒ 借方按部门 2 行 1 900 / 5 304.1667）→ 2028-03（B 末月 **316.6666 尾差**、累计=950、净值=50）→ 2028-04（B **提足剔除**，台数 3） | 200 | 6 个批次 6 张凭证、20 行折旧明细、全库 0 张不平、缓存=明细 0 差异 | ✅ |
| C·②③ | 处置前置闸：处置当月未计提 → **409 / 66048**（FIX-3 回归，且不建实例、不写留痕）→ 计提 2028-05（C 4 750，累计 23 750）→ 提交出售处置（实例 PENDING、卡片仍 IN_USE、**账未动**）→ GM 审批通过 → 卡片 DISPOSED + 留痕挂实例 + 状态日志 + DISPOSED 通知 → **3 张处置凭证**（清理/收款/结转）→ 已处置后 改/删/转移/再处置 全部 409 | 200/409 | 3 张处置凭证全平、1606 净额 0、5301 贷 13 750 | ✅ |
| C·② | D 报废处置（借 1602 1 187.50 + 借 5711 1 812.50 / 贷 1601 3 000）→ 审批 → DISPOSED → **作废处置** → IN_USE + disposal 字段清空 + 2 张红字凭证 + 处置净额归零 + IN_USE 通知 | 200 | `biz_fixed_asset_change`: DISPOSED→IN_USE | ✅ |
| C·③ | 作废后 2028-06 继续计提（台数 2：A + 恢复的 D；C 已处置剔除；D 照提 237.50） | 200 | 全库超提 0 行、缓存=明细 0 差异 | ✅ |

## 勾稽验证（`reconcile.log` 13 组）

| 勾稽项 | 期望 | 实测 | 结论 |
|---|---|---|---|
| ① 月折旧 = round((原值−残值)/月数, 4) | 5 台全等 | 1 900 / 316.6667 / 4 750 / 237.50 / 158.3333 全部 `ok=t` | ✅ |
| ② 不能超提 | 累计 ≤ 可折旧总额 | 5 台全部 `not_over=t`（B 恰好 950=950） | ✅ |
| ③ 缓存列 = Σ明细 | 0 行差异 | 5 台逐台 `accumulated = detail` | ✅ |
| ④ 逐月序列 | 月初 0 → 月末累加；尾差在末月 | A 1 900×6；B 316.6667/316.6667/**316.6666**(is_last_month=1)；C 4 750×5；D 237.50×6 | ✅ |
| ⑤ 批次 ↔ 凭证 | 逐期金额相等 + 借贷平衡 | 6 期 `total_amount = total_debit = total_credit = detail_sum` | ✅ |
| ⑥ 折旧凭证科目 | 借 5602.01（按部门）/ 贷 1602 | 每期 1 行 1602 贷方 = 批次额；5602.01 借方按部门拆（设备部/采购部），逐期合计相等 | ✅ |
| ⑦ 处置凭证分录 | 借 1602+1606 / 贷 1601；借 1002 / 贷 1606+2221.02；结转 1606 → 5301 | 23 750 + 36 250 / 60 000；56 500 / 50 000 + 6 500；13 750 / 13 750 | ✅ |
| ⑧ 1606 清理科目净额 | 0 | 0.0000（出售与报废两条链都归零） | ✅ |
| ⑨ 处置损益 | 售价 − 净值 = 50 000 − 36 250 = 13 750 → 5301 | 5301 贷 13 750；5711 净 0（报废损失已作废冲回） | ✅ |
| ⑩ 1601 净额 ↔ 在册卡片原值 | diff 0 | 124 500.00 = 124 500.00 | ✅ |
| ⑪ 1602 净额 ↔ 在册卡片累计 | diff 0（贷方为正） | 13 775.00 = 13 775.00（A 11 400 + B 950 + D 1 425） | ✅ |
| ⑫ 全库凭证 | 0 张不平 + 每张 total = 分录合计 | 0 / 0 | ✅ |
| ⑬ 状态机留痕 | 每条边都有 `biz_status_log` + `biz_fixed_asset_change` | 14 条状态日志 / 9 条变动留痕（TRANSFER / CUSTODIAN / IDLE / ENABLE / DISPOSAL / ENABLE） | ✅ |

## 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| Q1 | **删卡不校验购入凭证**：建卡必出 `FIXED_ASSET_ACQ`（借 1601）凭证，但 `DELETE /asset/{id}` 只拦「已计提 / 已处置 / 有在跑审批」，「未计提」的卡直接软删，凭证原样留着 | **P2（已修）** | 建卡（原值 500，购入日 2028-06-01）→ `DELETE /asset/{id}` → 200；随后 `Σ1601 − Σ在册卡片原值 = 500.00` | 1601 固定资产余额永久多出这笔原值，界面上没有任何入口能冲回（卡片已软删、凭证只能靠手工红冲审批），账实不符且不可自愈 | 删除前查蓝字购入凭证，存在则 409 并指路「走处置」——已按此修（见修复记录） |
| Q2 | **使用人变更 / 建卡带使用人整页 500**：`user_name_of` 查询 `COALESCE(NULLIF(nickname,''), username)`，而 `sys_user` 实际只有 `real_name`（无 `nickname` 列） | **P2（已修）** | `POST /asset/{id}/custodian {"to_user_id":"<admin>","reason":"..."}` → **500 / code=10004「服务器内部错误」**；建卡带 `custodian_id` 同样 500 | 卡片页「使用人变更」按钮 100% 不可用（用户侧只能看到服务器内部错误） | 列名改 `real_name`——已修（见修复记录） |
| Q3 | **补提历史期间的折旧费用按「卡片当前部门」归集**，不按期间归属部门：`run_batch` 用卡片当下的 `use_dept_id` 汇总借方 | P3 | A 于 2028-02-01 从设备部转到采购部 → 事后补跑 2028-01 → 2028-01 凭证借方 1 900 记 **采购部**（应为设备部）；实测 2028-01 借方差额 1 900 与 2028-02 相同 | 跨部门转移后补提，费用按部门归集失真（`5602.01` 的 DEPT 辅助核算/部门损益报表偏）；按月准时计提时不触发 | 折旧汇总时按 `biz_fixed_asset_change`（TRANSFER，`change_date ≤ period_end`）还原该期间部门，或给卡片存「期间-部门」快照；建议 >2 文件改动，另开卡 |
| Q4 | **启用（IDLE→IN_USE）要附件，建卡（IN_USE）不要**：同一个 `DocKind::FixedAsset` 的 ACTIVATE 规则只在状态边生效 | P3（设计待确认） | 停用 A → `POST /status {"target_status":"IN_USE"}` → **400「固定资产必须上传发票或验收单(pdf/jpg/png)：至少要 1 个」**；补传 `INVOICE_SCAN` 后 200 | 业务上合理（建卡时发票可能未到），但**停用后想再启用而没有附件就是死路**，且提示只在提交时出现（前端按钮看不出前置） | 产品口径二选一：① 建卡也强制附件；② 启用降级为提示 + 允许「无附件启用（记审计）」；至少要在「启用」按钮上预检（`GET /attachment/check`） |
| Q5 | 部门转移 / 使用人变更会在 `biz_status_log` 写一条 `from=to=IN_USE` 的日志 | P4 | 转移后 `GET /asset/{id}` 时间线出现「IN_USE → IN_USE」 | 详情页状态时间线看起来像脏数据（实际是业务动作留痕） | 日志加 `action` 字段（TRANSFER/CUSTODIAN）或前端按 reason 前缀分组显示 |
| Q6 | `PUT /asset/{id}` 对 `original_value` / `category_id` **静默忽略**（DTO 未声明 + serde 默认忽略未知字段） | P4 | 改卡请求体带 `"original_value":"999999"` → 200，DB 仍 120 000 | 接口使用者/二开容易误判「金额已改」；核对靠 DB 才能发现 | DTO 加 `#[serde(deny_unknown_fields)]`，或显式返回「金额/分类不可改」的 400 |
| Q7 | 作废处置的变动留痕 `change_date` 取「今天」而非处置日/作废日 | P4 | D 报废（2028-05-25）→ 作废 → `biz_fixed_asset_change.change_date = 2026-11-09` | 按会计期间筛选资产变动会漏掉这条/错期 | 作废接口接受 `change_date`（缺省处置日），或落两列（业务日 / 记录日） |

> 附：`GENERAL_MANAGER` 角色 4 个成员时，一次处置会为**每人各建 1 条待办**（或签），审批通过后其余 3 条自动 `CANCELLED`（实测 8 行任务：2 APPROVED + 6 CANCELLED）—— 语义正确，不是缺陷，记录备查。
> 另：`gm_drill`（p004 遗留审批人）在 `sys_user_role` 里**已无任何角色**，用它审批会 403 缺 `business:flow:approve`；本卡改为自建 `e2e05_gm` 并断言其角色在位。

## 修复记录（commit `c11fb863`，1 文件）

`backend/src/business/erp/asset_handler.rs`

1. **Q1 删卡闸（新增 16 行）**：`asset_delete` 在「已计提」判据之后增加
   `voucher::find_voucher_by_source(db(), "FIXED_ASSET_ACQ", id)`，命中即 409：

   ```
   资产 FA-… 已生成购入凭证 PZ-…（借 1601 原值）：不能删。删了 1601 固定资产余额会永久
   多出这笔钱，且没有界面能冲回；建错请做处置（报废 / 盘亏）冲销
   ```

   修复前实测：`DELETE` → **200**，`Σ1601 − Σ在册卡片原值 = +500.0000`；
   修复后实测：`DELETE` → **409 / 10003**，差额 **0.0000**（`a.log` A11、`prefix-defect-evidence.txt`）。

2. **Q2 使用人姓名列**：`user_name_of` 的 SQL `COALESCE(NULLIF(nickname,''), username)`
   → `COALESCE(NULLIF(real_name,''), username)`（`sys_user` 只有 `real_name`）。
   修复前：`POST /asset/{id}/custodian` → **500 / 10004**；修复后：**200**，
   `custodian_name = 超级管理员`，留痕 `CUSTODIAN | 超级管理员`。

检查：`cargo check --lib` 通过；`rustfmt +1.94.1 --edition 2024 --check` 无 diff；
`cargo test --lib business::erp::asset` → **61 passed, 0 failed**；145 重新构建 + 重启后 health 200。

## 复跑方式

```bash
# 145 上（脚本随仓库走）
bash /opt/cloudpivot/scripts/verify/e2e05_run_all.sh
# → 重置本卡数据 → A/B/C 三段 → 勾稽 SQL，输出 PASS=263 FAIL=0
```

- `scripts/verify/e2e05_asset_common.sh` 公共库（登录 / api / 断言 / 期间 / 附件 / 审批人）
- `scripts/verify/e2e05_asset_{a,b,c}.sh` 三段演练（分类卡片 / 折旧 / 处置）
- `scripts/verify/e2e05_reconcile.sql` 13 组勾稽
- `scripts/verify/e2e05_reset.sql` 只删本卡数据（资产名 `E2E05%` / 分类码 `E2E5%` / 演练期间批次）

本目录：`a.log` / `b.log` / `c.log`（逐条断言）、`asserts.log`（263 条 PASS 汇总）、
`reconcile.log`（13 组勾稽原始输出）、`raw/`（61 个接口响应报文；登录响应含令牌，已剔除）、`prefix-defect-evidence.txt`（修复前缺陷实测）。

## 结论

**全部通畅**：资产分类 → 建卡入账 → 转移/使用人 → 停用启用 → 月度计提（含尾差、提足即停、闲置照提、红冲重跑）
→ 处置（前置闸 409/66048、当月照提、审批落账、3 张凭证、清理归零、损益 13 750）→ 作废处置回滚 → 次日继续计提，
263 条断言全绿，13 组勾稽 0 差异。FIX-3 的两个历史问题（FIXED_ASSET 接线无规则、处置当月漏提折旧）均**回归通过**。

遗留：Q3（补提按当前部门，P3）会影响跨部门费用归集，建议单独开卡；Q4 需要产品口径确认；Q5-Q7 为体验/审计类 P4。
