# M3-20 销售预测表结构（月度 / 客户 / 物料维度 + 预测值 + 版本）

- 卡：`t_0be1f0b6`（M3-20【M3·销售】销售预测：表结构（月度/客户/物料维度））
- 执行：backend-developer，2026-11-17
- 迁移：`V0444__sales_forecast_qty_version`（只追加，上一号 V0443）

## 一、动手前的现状实测（决定这一刀补什么）

`biz_sales_forecast` 早在 **V0318**（SO-44 / ARCH-7 §15.8）就建好了，14 列，已经包含：

| 卡面要求 | V0318 是否已有 | 结论 |
|:--|:--|:--|
| 客户维度 | ✅ `customer_id UUID`（可空） | 不动 |
| 物料维度 | ✅ `material_id UUID`（可空） | 只有 `(customer_id, period_code)` 索引 → **补物料索引** |
| 月度维度 | ✅ `period_code`（`yyyy-MM` / `yyyy-QN` + `ck_biz_sf_period`） | 不动 |
| 预测值（金额） | ✅ `forecast_amount NUMERIC(18,2)` | 不动 |
| 预测值（数量） | ❌ 缺 | **补 `forecast_qty`** |
| 版本 | ⚠️ 只有 `version_no` + `uk_biz_sales_forecast_dim` | **补版本链 `revised_from_id`**，并补「当前版本」唯一口径 |

所以本卡**不重建表**，只补 4 件真正缺的结构 + 把新列接到既有 model/dto/handler。

## 二、V0444 落的四件东西（每件都有「不补会怎样」）

| # | 结构 | 不补会怎样 |
|:--:|:--|:--|
| ① | `forecast_qty NUMERIC(18,4) NOT NULL DEFAULT 0` + `ck_biz_sf_qty (>= 0)` | 物料维度预测天然按数量谈（"下月 A 料 5000 件"），没有数量列只能拿金额凑；与同模块 `biz_sales_target.target_qty` 精度/口径也对称不上 |
| ② | `revised_from_id UUID` + `ck_biz_sf_revised_from (<> id)` | `version_no` 只说明"第几版"，说明不了"从哪一版改来"；v3 是 v2 的滚动修订还是有人跳号另起，无从判断 |
| ③ | `idx_biz_sales_forecast_material (material_id, period_code DESC) WHERE deleted_at IS NULL` + `idx_biz_sales_forecast_revised_from` | 物料维度取数只能顺扫（客户维度 V0318 已有对称索引） |
| ④ | 视图 `v_biz_sales_forecast_current` | 同维度同期间有多版本（滚动预测的本意），下游直接 SUM 会把历史版本一起加（预测翻三倍还看不出来）。视图钉死口径：**存活行里 `version_no` 最大的一条** |

配套的代码改动（不加就是死列）：

- `sales_target_model.rs`：`FORECAST_COLUMNS` 补两列、`ForecastRow` 补两字段、`insert/update_forecast` 补两列、新增 `find_current_forecast()`（读④的视图，注释写明别自己写 `ORDER BY version_no DESC LIMIT 1`）。
- `sales_target_dto.rs`：`ForecastRes` / `ForecastCreateReq` / `ForecastUpdateReq` 补字段（更新入参用 `double_option`：缺失=不改、`null`=断链）。
- `sales_target_handler.rs`：`create/update_forecast` 补数量校验（复用 `check_qty`，4 位小数、非负）+ `validate_version_chain()`（自指 / 幽灵 / 跨维度 / 跳号 → 400）。
- `frontend/src/api/business/erp/sales-target.ts`：三个 TS interface 同步补字段（后端返回了就得上类型，否则前端拿不到）。

## 三、验收（全部真实执行，原始输出在本目录）

| # | 闸 / 用例 | 结果 | 证据 |
|:--:|:--|:--|:--|
| 1 | 空库按版本号重放**全部**版本化迁移（psql 逐条 `--single-transaction`） | **PASS 364/364**，表 186 张、视图 1 个（就是本卡的 ④） | `01-mig-replay-fresh-364.txt` |
| 2 | `cargo check --lib --tests` | Finished，零 error / 零 warning | `02-cargo-check-lib-tests.txt` |
| 3 | `cargo clippy --lib -- -D warnings`（touch 源文件强制重编，不是缓存命中）；另跑 `--lib --tests` 覆盖本卡新增的测试代码 | `Checking cloudpivot … Finished in 37.60s` / `--tests` 2m21s，均零 warning | `03a-cargo-clippy-lib-D-warnings.txt`、`03b-cargo-clippy-lib-tests.txt` |
| 4 | 迁移幂等 + down 数据护栏 + down/up 往返 | up 重放 0 结构变更（探针自检通过、0 残留）；有数量数据时 **down 被拒**并给出人话；清数据后 down 干净拆除（顶层结构 V0318 完好：14 列 / dim 唯一索引 / 期间 CHECK 在位）；down 重放幂等；再 up 完全复原 | `04-migration-idempotent-down-up.txt` |
| 5 | **空库首启真实迁移器**（真二进制 + 真迁移引擎） | `数据库迁移完成: 应用 376 个迁移`；`schema_migrations` 364 versioned + 12 seed、**失败 0**、最大版本 **V444**、`444:versioned:true`；`forecast_qty/revised_from_id` 在、视图在；菜单总数 540 **未涨**（本卡零菜单） | `05-empty-db-real-migrator-boot.txt` |
| 6 | 本卡真库用例（11 条，直调 handler，走和 HTTP 同一条代码路径） | **11/11 pass**，0 SKIP（`[M3-20] 建行：SFC-20261117-000020` 证明真落库） | `06-sales-forecast-db-tests.txt` |
| 7 | 销售模块宽回归（`business::erp::sales`，326 条，跑在 **V443 真实库副本**上） | **326/326 pass，0 failed** | `07-sales-module-tests.txt` |
| 8 | `pnpm typecheck` / `pnpm build` | 0 错 / `✓ built in 54.28s` | `08-frontend-typecheck.txt`、`09-frontend-build.txt` |
| 9 | **升级路径**：真实 V443 库副本（36 用户 / 190 表）用真迁移器升级 | 首次**只应用 1 条**（就是 V0444），max V443→V444、失败 0、列 14→16、视图建出、既有数据未动；同库再跑一次**应用 0 条**（幂等） | `10-upgrade-path-v443-to-v444.txt` |

### 宽回归为什么跑在「V443 库副本」而不是「空库重放库 / 模板库」

先跑了一遍空库重放库（`cp_m320_replay`）和模板库（`cloudpivot_test` 的副本），各有 11 / 8 条**与本卡无关**的红：

- 空库重放库：`quotation_state_tests` / `sales_approval_bridge::db_tests` 报「演练库里至少要有一个 sys_user」「ROLE_NO_MEMBER」——
  纯迁移库只有 bootstrap 的 1 个超管、0 条角色成员，审批类用例的夹具（要 36 个演示用户 / 角色成员）无从谈起。
- 模板库：`column "credit_policy" of relation "biz_customer" does not exist` —— 模板停在 V404，比代码落后 40 个迁移。

换到 `cloudpivot_dev` 的副本（V443 + 36 用户 + 190 表，先升级到 V444）后 **326/326 全绿**，
说明那批红是「库的夹具/版本不对」，不是本卡改坏了什么。本卡自身的 11 条用例在两个库上都绿。

顺带修了一处**被本卡改动带红的既有单测**：`sales_target_model::tests::insert_forecast_sql_binds_uuid_columns_with_cast`
钉的是 `insert_forecast_sql()` 的占位符形状（`?` 11 → 13、`::uuid` 4 → 5、`?::text::numeric` 1 → 2），
新增两列后按新形状更新，并补一条「版本链列必须带 `NULLIF(...)::uuid`」的断言。


用例清单（`06`）：

```
create_reads_back_qty_and_dimension_names   月度×客户×物料建行：金额+数量原样存取、维度名 JOIN 回来、v1/无链
qty_zero_when_only_amount_is_given          不传数量 ⇒ 库里 0（不是 NULL、不报错）
bad_qty_is_400_and_writes_nothing           负数量 / 5 位小数 → 400 且零落库
duplicate_dimension_period_version_is_409   同（客户,物料,期间,版本）第二条 → 409；换版本号可建
version_chain_links_previous_version        v2 带 revised_from_id=v1 落库；当前版本视图给 v2
version_chain_rejects_bad_links             自指/幽灵/跨维度/跳号 四种坏链 → 400，零落库
soft_deleted_latest_falls_back_to_previous  软删 v2 ⇒ 视图回落 v1（历史没被一起删）
current_view_stays_out_of_list_ordering     列表 3 条历史 vs 视图 1 条（下游 SUM 不会翻倍）
material_dimension_filter_is_exact          物料维度筛选精确；双空维度（全局）也能取当前版本
update_qty_and_break_chain                  只改数量（金额/链/版本不动）、显式 null 断链
zz_teardown_purges_fixtures                 探针/夹具 0 残留
```

## 四、留给后续卡的接口（M3-21 预测 vs 实际、M3-23 版本历史都不要另起口径）

- 当前预测口径：**只查 `v_biz_sales_forecast_current`**（或 `model::find_current_forecast`），别看表自己 `max(version_no)`。
- 版本历史：查表（`FORECAST_TABLE`），按 `version_no` 排；链的回溯走 `revised_from_id`。
- 预测数量在 `forecast_qty`（4 位小数、非负、缺省 0 = 只预测金额）。

## 五、清场

演练库全部 `dropdb`（`like 'cp_m320%'` 清零）：`cp_m320_replay`（空库重放）、`cp_m320_boot`（空库首启）、
`cp_m320_upgrade`（V443 副本升级），以及上一轮崩溃残留的 `cp_m320_forecast` / `cp_m320_mig`。
迁移实例端口 18195 / 18196 已停，进程已退出。
