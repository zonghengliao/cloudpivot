# ERP-R5 期初余额录入 —— 交付证据

> 卡：`t_2c5eb708`（ERP-R5，P1）｜提交：`ef7ae72`｜迁移：`V0189`｜生产：172.18.10.144
> 走查时间：2026-11-01 22:19–22:45（CST）

## 一、一句话结论

客户从旧系统迁过来第一件事——**录期初**——现在能做了：财务按科目录（方式 A）、仓管按物料+仓库录库存期初（方式 B 第一项），
两者在**同一事务**里拼成**一张** `voucher_type='OPENING'` 的期初凭证；不平衡不许保存（400 + 差额 + 点名科目）、
重复录入是**覆盖同一张凭证**（库层面部分唯一索引兜死）、结账后整页只读（409）、
报表侧把期初算作「期初」而不是「本期发生额」，于是**资产负债表「期末 = 期初 + 本期」恒成立、利润表本期/本年累计看不到期初**。

## 二、卡面硬红线逐条对照

| # | 硬红线 | 落地 | 证据 |
|---|--------|------|------|
| 1 | 必须 commit + push | `ef7ae72` → origin/main（`git ls-remote` = ef7ae72…） | 本文档 →§七 |
| 2 | 期初只能有一张凭证 | **库层面**：`CREATE UNIQUE INDEX uk_biz_voucher_opening_period ON biz_voucher (period) WHERE voucher_type='OPENING' AND deleted_at IS NULL`；应用层重复录入走「UPDATE 单头 + 整组替换分录」，凭证号不变 | 单测 `r5_03`、真机 `⑤`（凭证数=1、凭证号 PZ-…000013 不变） |
| 3 | 期初凭证不计入本期发生额 | `report::load_accounts` 把 `voucher_type='OPENING'` 从「本期」子查询里剔除、改记到「期初」子查询；利润表本期/本年累计直接复用该函数 | 单测 `r5_07`（REVENUE current=0、ytd=0、net_cum=-1000）、真机 `⑩` |
| 4 | 期初不平衡不许保存 | `ensure_balanced`：400 + `66012` + 差额 + 两侧科目清单；报表侧再兜一道（不平衡直接拒绝出表） | 单测 `r5_02` / `r5_12`、真机 `③` |
| 5 | 库存期初同步三本账 | `biz_inventory`（成本账）+ `biz_inventory_location`（库位账，NULL 桶）+ `biz_inventory_txn`（流水，`biz_type='OPENING'`），同一事务、复用 `stockin_model::apply_cost / apply_location_qty` | 单测 `r5_08` / `r5_09`、真机 `⑦a/⑦b/⑦c` |
| 6 | 完事跑全量测试 + 不变量 | 后端 `cargo test --lib` **1786 passed / 0 failed**；前端 381 单测 + typecheck 绿；ERP 不变量 **32/32 · BLOCKER 0 · WARN 0**（隔离库与生产各一次） | §四 |

## 三、设计决策（与卡面的差异都写在这里）

### 3.1 期初为什么落成「凭证」而不是只落一张表（卡面推荐 B，本卡按推荐）

报表（资产负债表 / 利润表 / 试算平衡 / 总账）全部按 `biz_voucher_entry` 求数。期初若只进
`biz_opening_balance`，上面四处都要各加一条 union 分支——四份实现、四种漏改的可能。
落成凭证后取数逻辑**一行不改**就包含期初；代价是「期初会被当成本期凭证统计」，所以只做了
**两条明确的排除**（`report::load_accounts` 的 `o` / `p` 两个子查询），一处收口。

### 3.2 与卡面不一致的四处

| 卡面 | 实际 | 理由 |
|------|------|------|
| §二 库存期初「复用 biz_inventory 加 `opening_flag`」 | 另建 `biz_opening_inventory`（期初录入台账） | `opening_flag` 只有布尔位，回答不了「谁录的、多少数量、什么单价」；**覆盖式更新要算差额，必须知道旧值** |
| §二 表带 `create_by/update_by`（无 `deleted_at`） | 同卡面，不做软删 | 期初是按期间**整体覆盖**的，删掉一个科目 = 重存时它不在 entries 里 |
| 卡面未提「期初凭证独占」的库约束 | 加部分唯一索引 `uk_biz_voucher_opening_period` | 硬红线 2 只写在应用层，任何一条新写路径都能绕过去；`ck_biz_voucher_balance` 也是这个手法 |
| §五「已有期初时显示『修改』而不是『新增』」 | 按钮固定为「保存期初」（一个按钮两种语义） | 卡片同一句话里的「覆盖」语义下，**没有『新增』这个动作**：重复保存就是覆盖。多一个按钮只会让人以为能开出第二张 |

### 3.3 库存期初的两条硬规则（卡面没写，但不加就出事）

1. **一物一码物料拒绝按数量录期初**：`serial_required=1` 的物料 `qty` 是「在库序列号件数」的派生值
   （巡检 C5），按数量录了立刻 WARN 且账实不符。返回 400 并点名「等序列号期初导入卡」。
2. **库存期初与手工 1405 / 3103 期初互斥**：库存期初会自动产出 `借 1405 / 贷 3103`，
   财务若同时在方式 A 里手录这两个科目，1405 会被计两次 → 巡检 B2（库存金额 = 总账 1405 余额）直接红。
   保存时拦住并点名「要么删掉这一行，要么先把库存期初清零」。

### 3.4 覆盖式更新为什么按「差额」调而不是「删了重建」

库存期初已经把数量加进了三本账。删了重建会把**期初之后**发生的收货/出库一起抹掉。
所以 `Δqty / Δamount` 算法：`Δ>0` 写 `IN` 流水，`Δ<0` 写 `OUT` 流水（`ck_biz_inv_txn_qty` 要求 `qty > 0`），
`Δ=0` 不写流水。清空期初 = 全部按差额退回 + 一条 `OUT` 流水 + 凭证撤掉。

## 四、测试与验收（全部真跑，贴原始结论）

### 4.1 后端单测（15 条，隔库 `cloudpivot_r5`，V0137–V0189）

```
cargo test --lib business::erp::opening -- --test-threads=1
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 1771 filtered out; finished in 9.95s
```

| 卡面条目 | 用例 |
|----------|------|
| ① 借贷平衡 → 保存成功 | `r5_01_balanced_save_succeeds` |
| ② 借贷不平 → 400 + 差额 | `r5_02_unbalanced_rejected_with_difference`（码 66012 + 差额 10000 + 点名 1002/3001） |
| ③ 重复录入 → 覆盖 | `r5_03_repeat_save_overwrites_single_voucher`（凭证数 1、号不变、分录 2 条） |
| ④ 结账后 → 409 | `r5_04_closed_period_rejected`（科目期初与库存期初各一次） |
| ⑤ OPENING 类型凭证 | `r5_05_opening_voucher_type_and_date`（type/日期=期间首日/摘要/来源号） |
| ⑥ 该凭证借贷平衡 | `r5_06_opening_voucher_is_balanced`（单头 + 分录 + 分录合计=单头） |
| ⑦ 不计入本期发生额 | `r5_07_opening_excluded_from_income_statement`（REVENUE current=0 / ytd=0 / net_cum=-1000） |
| ⑧ 100 件 × 10 元 | `r5_08_inventory_opening_writes_cost_book` |
| ⑨ 库位账 + 流水 | `r5_09_inventory_opening_writes_location_and_txn` |
| ⑩ 累加到 1405 借方 | `r5_10_inventory_amount_lands_on_1405_debit`（含 3103 贷方、4 条分录、合并 51000） |
| ⑪ 期末 = 期初 + 本期 | `r5_11_balance_sheet_end_equals_opening_plus_period`（net_before 50000 / net_period 2500 / 货币资金 52500 + 勾稽平） |
| ⑫ 不平衡不许出表 | `r5_12_unbalanced_opening_blocks_report`（`build_snapshot` 报错 + 接口翻成 400+66012） |
| 补充 | `r5_13` 库存期初覆盖差额（含清空）／`r5_14` 一物一码拒绝／`r5_15` 手工 1405 互斥 |

### 4.2 全量回归（关键：报表口径被本卡改过）

```
cargo test --lib -- --test-threads=1      # ERP_DB_URL=cloudpivot_r5
test result: ok. 1786 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 464.12s
```

> 1771 条既有用例 + 本卡 15 条，**0 失败**。报表侧的改动没有打红任何既有用例
> （`report` / `income_statement` / `cash_flow` 的既有用例全部绿）。

### 4.3 前端

```
pnpm typecheck      → 绿（vue-tsc --noEmit 无输出）
pnpm test:unit      → # tests 381  # pass 381  # fail 0     （既有 370 + 本卡 11）
```

### 4.4 真机 API 验收（生产 144，26/26）

```
ERP_E2E_BASE=http://127.0.0.1:8023 python3 scripts/verify/r5_opening_e2e.py
汇总：26/26 通过
```

要点摘录（原始输出）：

```
[PASS] ③ 借贷不平 → 400 + 66012 + 差额 10000 + 点名科目 — ...借方：1002 银行存款 50000 / 贷方：3001 实收资本 40000
[PASS] ④ 借贷平衡 → 200 + state=ENTERED + 生成期初凭证 — voucher=PZ-20261101-000014
[PASS] ④c 录期初后资产负债表 — 货币资金 0 → 50000.0000；资产合计 835.0000 → 50835.0000
[PASS] ⑤ 重复录入 = 覆盖 — no=PZ-20261101-000014（首次 PZ-20261101-000014）凭证数=1 科目行=2
[PASS] ⑥ 库存期初 100×10 — inv=1000.0000 合计=51000.0000/51000.0000
[PASS] ⑦a biz_inventory qty=100 amount=1000 avg_cost=10
[PASS] ⑦c biz_inventory_txn — ['IN|OPENING|OPENING-2027-01|100.0000']
[PASS] ⑧ 期初凭证 借1405=1000 / 贷3103=1000 / 4 条分录
[PASS] ⑨ 资产负债表（增量口径）— 货币资金 0→50000、存货 835→1835、资产合计 835→51835
[PASS] ⑩ 利润表营业收入 本期=0 本年累计=0
[PASS] ⑪b GET 变只读 — locked=True state=LOCKED
[PASS] ⑪c/⑪d 结账后录入 → 409（科目期初与库存期初各一次）
[PASS] ⑫ 账账相符：库存金额 = 总账 1405 余额 — 1835.0000 / 1835.0000
[PASS] ⑬ 收尾基线核对 — before=['2026-10:OPEN','2026-11:OPEN'] after=['2026-10:OPEN','2026-11:OPEN']
```

> **⑨ 为什么用「增量」而不是绝对值**：生产库里本来就有 835 元的库存（3 行 `biz_inventory`），
> 「存货 = 1000」这种绝对值断言在非空库上必假红（第一版就这么红了一次）。
> 增量口径 `存货 835 → 1835（+1000）` 才是这条断言真正想证明的东西。

### 4.5 页面验收（Playwright，19/19）

```
cd frontend && ERP_PAGE_ADMIN_PASSWORD=… node ../scripts/verify/r5_opening_page_check.mjs
汇总：19/19 通过
```

| 断言 | 结果 |
|------|------|
| 状态标签「期初已录」 | ✅ |
| 借方合计 / 贷方合计 50,000.00 | ✅ |
| 平衡状态「✅ 平衡」 | ✅ |
| 期初凭证号（`PZ-20261101-000002 · 2027-02-01 · 4 条分录 · 借 51,000.00`） | ✅ |
| 合并口径含库存期初（借 51,000.00 / 贷 51,000.00） | ✅ |
| 平衡时「保存期初」可点 | ✅ |
| 改成不平 → 提示「❌ 不平衡，差额 -10,000.00」+ 保存按钮**禁用** | ✅ |
| 库存 Tab：1 行、合计 1,000.00、保存可点 | ✅ |
| 库存期初提示点名 1405 / 3103 | ✅ |
| 结账后：状态「已锁定」、顶部「2027-02 已结账，期初不可改」 | ✅ |
| 结账后：保存按钮置灰、「添加科目」不渲染、金额输入框 disabled | ✅ |
| 无 console / page error（仅 `/notice/stream` 的 401，框架既有行为） | ✅ |

截图：`10-期初余额-可编辑.png`、`11-期初余额-结账后只读.png`（本目录）。

### 4.6 ERP 不变量

```
# 隔离库 cloudpivot_r5e（干净库）
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0

# 生产 144（部署后 / 真机 e2e 收尾后各一次）
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

## 五、生产部署（144）

| 项 | 值 |
|----|-----|
| 提交 | `ef7ae72` |
| 迁移 | `V0189__erp_opening_balance`（`数据库迁移完成: 应用 1 个迁移`，`max(version)=189`） |
| 启动公告 | `NOTICE: V0189 自检通过：ck_biz_voucher_type 拦住了白名单外的类型` / `NOTICE: V0189：期初三码已种，授权新增 2 行` / `NOTICE: V0189 自检通过：期初两表 + 期初凭证唯一索引 + 三个权限节点` |
| 二进制 | `355e2c21d7447ad072ae049ed7f97d5b`，`PROC_EQ_DISK=OK`（`/proc/<MainPID>/exe` == 磁盘） |
| health | 200 |
| 前端 dist | 入口 chunk 已换新；10 个新 `data-testid` 逐个 grep 命中（`btn-opening-refresh` / `opening-period-select` / `opening-state-tag` / `opening-tabs` / `btn-opening-save` / `btn-opening-save-inventory` / `opening-balance-state` / `opening-voucher-info` / `opening-readonly-alert` / `btn-opening-add-row`） |
| 库侧核对 | `sys_menu` 3 个期初节点（…1022/1023/1024）／`ck_biz_voucher_type` 含 `OPENING`／`uk_biz_voucher_opening_period` 已建／期初两表 0 行（尚未录） |
| 数据变更 | 无（真机 e2e 造的 2027-01 期间与期初已全部清理，期间表回到 `2026-10:OPEN,2026-11:OPEN`） |

## 六、权限与菜单

| 节点 id | auth_code | 类型 | 授权角色 |
|---------|-----------|------|----------|
| …1022 | `business:erp:opening:read` | 菜单 `/business/erp/opening` | SUPER_ADMIN / ADMIN / FINANCE / ACCOUNTANT |
| …1023 | `business:erp:opening:write` | 按钮「录入科目期初」 | + WAREHOUSE（只给 read） |
| …1024 | `business:erp:opening:inventory` | 按钮「录入库存期初」 | SUPER_ADMIN / ADMIN / FINANCE / WAREHOUSE |

三个码分开的理由与 ERP-P1 的期间三码一致：财务录科目、仓管录库存是两类人；
「能看期初」更不能等于「能改期初」。**没权限时按钮不渲染（不是置灰）**。

## 七、接口清单

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| GET | `/api/business/erp/opening-balance?period=2026-11` | `opening:read` | 合并两种口径 + 状态 + 期初凭证 + 提示 |
| POST | `/api/business/erp/opening-balance` | `opening:write` | 保存科目期初（整体覆盖，同事务出凭证） |
| POST | `/api/business/erp/opening-balance/inventory` | `opening:inventory` | 保存库存期初（整体覆盖，同事务写三本账 + 凭证） |

错误口径：

| HTTP | code | 场景 |
|------|------|------|
| 400 | `10001` | 期间格式 / 科目不存在、非末级 / 借贷同填 / 物料仓库不存在 / 一物一码 / 与 1405 冲突 |
| 400 | `66010` | 期间未开账 |
| 400 | **`66012`** | **期初不平衡**（消息带差额与两侧科目） |
| 409 | `10003` | 期间已结账（期初只读） |
| 403 | — | 缺权限码 |

## 八、已知限制 / 后续建议

1. **一物一码物料的库存期初**按数量录不了（qty 由序列号件数派生）。需要「序列号期初导入」卡
   （生成 `IN_STOCK` 序列号 + 走 `serial_model` 的取号原语），本卡刻意不做，避免制造 C5 漂移。
2. **方式 B 的其余项**（应收 / 应付 / 固定资产期初）本卡不做（卡面「后续卡」）：接口形态可以照本卡
   （`POST /opening-balance/xxx` + 自动生成对方科目 + 并入同一张 OPENING 凭证）。
3. **结账不做期末结转**（损益 → 本年利润），期初的对方科目暂时固定为 `3103 本年利润`
   （卡面 §一 方式 B 的既定口径）。若客户要求「存货期初对方科目 = 实收资本 / 以前年度损益调整」，
   建议做成 `biz_voucher_rule` 的一条映射（`biz_type='OPENING_INVENTORY'`）而不是写死常量。
4. **期初凭证的作废**：V0189 down 遇到已有 OPENING 凭证会中止（安全阀）。若业务上要撤掉期初，
   正确姿势是「把期初清空（POST 空数组）」——期初凭证会被整体撤掉；若要保留凭证做红冲，
   需要单独开卡（红冲 OPENING 凭证目前未开放）。
5. 隔离库 `cloudpivot_r5`（单测）与 `cloudpivot_r5e`（实例/页面验收）保留在 99 上，供复跑；
   实例进程已按 pid 停掉。
