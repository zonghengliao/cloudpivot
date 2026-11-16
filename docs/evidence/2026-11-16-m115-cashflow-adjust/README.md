# M1-15 现金流底稿：间接法调整项配置（V0416）

> 卡：`t_ce938a2a` ｜ 迁移：`backend/app/migrations/business/V0416__cashflow_adjust_item.{up,down}.sql`
> 代码：`backend/app/src/business/erp/cashflow_adjust.rs`（新增，1731 行）
> 静态不变量：`backend/app/tests/m1_15_cashflow_adjust_migration.rs`（11 条）
> 脚本：`scripts/verify/m115_cashflow_adjust_item_drill.sh`、`m115_engine_upgrade.sh`、`m115_adjust_http_e2e.sh`
> 上游：M1-13（`V0414` 底稿表）/ M1-14（`V0415` 底稿接口码）；下游：M1-16（由底稿出表）/ M1-17（勾稽单测）

## 0. 一句话结论

间接法（净利润 → 经营活动现金流量）的**调整项配置**落地：一张纯配置表（9 列 / 6 条 CHECK /
零金额列）、12 行标准模板、4 个取数口径、5 个接口、2 个权限码。**不算金额、不出表**（M1-16/17 的活）。

## 1. 做了什么

| 交付 | 内容 |
|---|---|
| 表 `biz_cashflow_adjust_item` | 一行 = 一个调整项：编码 / 名称 / 取数来源 / 取数口径 / 科目模式 / 顺序 / 启停 / 备注。**无金额列**（配置表只存规则） |
| 12 行标准模板 | 企业会计准则《现金流量表补充资料》间接法部分；4 个口径各被用到；2 行 `MANUAL`（递延所得税 / 其他） |
| 4 个取数口径 | `CREDIT_NET` 贷方净额 / `DEBIT_NET` 借方净额 / `BALANCE_INCREASE` 期末−期初 / `BALANCE_DECREASE` 期初−期末 |
| 5 个接口 | `GET …/adjust-item/list`、`GET …/adjust-item/catalog`、`POST …/adjust-item`、`PUT …/adjust-item/{id}`、`PUT …/adjust-item/{id}/toggle` |
| 2 个权限码 | `business:erp:cashflow-adjust:list`（…001790）/ `:manage`（…001800），挂既有「现金流量表」页面，授 SUPER_ADMIN + FINANCE_MANAGER |

## 2. 关键设计决定（都不是随手写的）

1. **不设「加 / 减」符号列**。准则的间接法表每行都印「加：」，方向由**数值正负**表达
   （「减少（增加以『−』号填列）」）。符号列会让方向出现两个真相：符号说加、口径算出负数。
   所以方向**只**由口径决定 —— 存货增加时 `BALANCE_DECREASE` 自动算出负数，处置有收益时
   `1606` 借方净额自动为负，报表出数代码不需要按行特判。
2. **`MANUAL` 行必须把口径 / 模式留空**（服务端 400 + 库层 `ck_biz_cashflow_adjust_shape` 双闸）。
   留着不参与计算的模式串就是「看起来配了、其实不用」的摆设（`docs/ERP-摆设字段口径.md`）。
3. **起点「净利润」与小计不落配置**：前者取利润表（ERP-R2），后者是求和。手填净利润会造出
   与利润表对不上的报表。两者作为 `anchors` 返回给前端渲染报表形状。
4. **编码不可变、没有 DELETE**：编码是落数身份；删一行 = 报表少一行且没人会发现 → 只 `toggle`。
5. **重放不覆盖管理员改动**：种子 `ON CONFLICT (item_code) DO NOTHING`（实测：改名 + 调顺序后重放，值保住了）。
6. **两个静默失败在读接口点名**：① 模式命中 0 个科目（本卡模板里 `ADJ_ASSET_IMPAIRMENT` 就是，
   科目表还没建 1231/1471）→ 行级 `warnings`；② 同一科目被两行命中 / 完全重复的口径 → 顶层
   `warnings`（完全重复在写入口直接 409）。
7. **不给 FINANCE_DIRECTOR 授码**：它没有现金流量表页面码，授了就是孤儿按钮（`orphan_button_gate` 判红）。

## 3. 验收（全部真实输出，见同目录 `.out`）

| 证据 | 结果 |
|---|---|
| `00_fmt_check.out` | `cargo fmt --check` 0 diff |
| `01_clippy.out` | `cargo clippy -p cloudpivot --lib -- -D warnings` 0 警告 |
| `02_unit_tests.out` | 模块单测 **16/16**（纯逻辑 11 + 真库 5，库 `cp_m115_unit`）+ 迁移静态不变量 **11/11** |
| `03_migration_drill.out` | 真库往返 **45/45**：up×2 / 结构 / 模板 / 权限 / 幂等不覆盖 / 5 条约束反证 / down / down 重放 / 护栏反证 |
| `04_engine_upgrade.out` | 真引擎 **23/23**：克隆库升级（应用 12 个迁移）→ `V0416 自检通过` NOTICE → `schema_migrations 416/true` → 重启 0 迁移 → down 护栏 |
| `05_http_e2e.out` | 真 HTTP **74/74**：401 / list（12 行 + 命中自检）/ catalog / 新增 / 重复码 409 / 同口径 409 / MANUAL 带口径 400 / 改码 400 / 启停 / 404 / DB 对账 |
| `06_gates.out` | `perm_menu_gate` PASS（missing_in_menu=0）；`menu_invariants` 8/8（菜单总数 510）；`role_code_gate` ✓ 无新增未知角色码；`orphan_button_gate` 3 条**均为存量**（对照库 `cloudpivot_test` 同样 3 条，不含本卡码） |

## 4. 库与环境

- 演练/引擎/HTTP/闸门库全部自建自删：`cp_m115_adjust_drill_*`、`cp_m115_upgrade`、`cp_m115_adjust_e2e`、`cp_m115_gates`、`cp_m115_unit`。
- 收尾核对：`select datname from pg_database where datname like 'cp_m115%'` = 0 行；`cloudpivot_test`（模板库）未被修改。
- 引擎/HTTP 实测用真二进制 `backend/target/debug/cloudpivot`（11:09 重建，含本模块路由）。

## 5. 给 M1-16 / M1-17 的契约

```text
GET  /api/business/erp/report/cash-flow/adjust-item/list
  → items[]: { item_code, item_name, source_type, calc_basis, account_pattern,
               sort_order, is_active, matched_account_count, warnings }
    · 只取 is_active = true 的行（停用行仅维护面可见）
    · 金额 = 按 calc_basis 从 account_pattern 命中的科目算：
        CREDIT_NET        贷方发生额 − 借方发生额
        DEBIT_NET         借方发生额 − 贷方发生额
        BALANCE_INCREASE  期末余额 − 期初余额
        BALANCE_DECREASE  期初余额 − 期末余额
      MANUAL 行的金额由出表入口传入（本卡不提供填数接口）
  → anchors[0] = 净利润（取利润表）、anchors[1] = 经营活动现金流量净额（= 净利润 + Σ调整项）
```

- **出表前先看 `warnings`**：命中 0 个科目的行会恒为 0（如 1231/1471 未建账），不能当「本期没发生」。
- 顺序取 `sort_order`（同值按 `item_code`），与模板一致。

## 6. 已知边界（刻意不做，属别的卡）

- 不计算任何金额、不写底稿、不生成凭证、不动 `biz_cashflow_item` / `biz_cashflow_worksheet` / `biz_report_item`。
- 没有前端口径页面（M1-18 是直接法底稿页；间接法页面未在卡清单中，需要时另开卡）。
- 没有「把调整项金额落到某期间」的表 —— 若 M1-16 需要冻结历史调整项金额，需新建期间表（本卡不夹带）。
