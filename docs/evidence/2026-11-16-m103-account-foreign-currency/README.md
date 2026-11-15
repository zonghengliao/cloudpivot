# M1-03 证据：科目表加「是否外币核算」标记（V0408）

- 卡：`t_52b02f6a`（M1-03【M1·财务】多币种：科目表加「是否外币核算」标记 —— DTO + handler + 页面开关）
- 上游：M1-01（`V0406` `biz_account.currency_code`）/ M1-02（`V0407` 汇率生效区间）
- 采集时间：2026-11-16（北京时间）
- 采集位置：99（开发机）→ PG `172.18.10.145:5432`（按卡面「场景 A」）

## 一句话

给 `biz_account` 加 `is_foreign_currency SMALLINT NOT NULL DEFAULT 0`（0 本位币 / 1 外币），
**只有末级科目可开启**（CHECK + 应用层 400/`66080` 双口径），配一个窄写口
`PUT /business/erp/account/{id}/foreign-currency` + 权限码 `business:erp:account:update`，
前端会计科目页加「外币核算」开关 + 同名列 + 筛选下拉。存量 83 行科目全部回填 0（恒等变换）。

## 交付清单

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0408__account_foreign_currency.up.sql` | 327 行：列 + 2 条 CHECK + 部分索引 + 权限码/菜单/授权 + 自检（6 组行为探针） |
| 迁移 | `…V0408__account_foreign_currency.down.sql` | 87 行：数据护栏（有外币科目就拒回滚）→ 拆授权/菜单 → 拆索引/约束/列 → 收尾自检 |
| 后端 | `backend/app/src/business/erp/account_admin_handler.rs` | 217 行：唯一写口 + 4 条纯逻辑单测 |
| 后端 | `backend/app/src/business/erp/dto.rs` | `AccountRes.is_foreign_currency` / `AccountFilter.is_foreign_currency` / `AccountForeignCurrencyReq` |
| 后端 | `backend/app/src/business/erp/model_master.rs` | `ACCOUNT_COLUMNS` + `AccountRow` + `account_page` 筛选 + `account_set_foreign_currency` |
| 后端 | `backend/app/src/business/erp/handler.rs` | `POST_W1C_PERMISSIONS`（新码进 `all_permissions` 汇总；单测 29→30） |
| 后端 | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | `ERP_ACCOUNT_FC_NOT_LEAF = 66080`（66xxx 段续号） |
| 前端 | `frontend/src/api/business/erp/index.ts` | `AccountRow.is_foreign_currency` / `fetchAccountPage` 入参 / `setAccountForeignCurrency` |
| 前端 | `frontend/src/views/business/erp/account/account-tree.ts` | `foreignCurrencyLabel` / `canMarkForeignCurrency` / 筛选状态与过滤 |
| 前端 | `frontend/src/views/business/erp/account/index.vue` | 开关列 + 筛选下拉 + 权限禁用 + 失败不乐观更新 |
| 前端 | `frontend/src/constants/error-messages.ts` | `66080` 人话文案（前端单测强制「后端每个 66xxx 都要有翻译」） |
| 测试 | `backend/app/tests/m1_03_account_foreign_currency_migration.rs` | 10 条迁移静态不变量（含引擎语句拆分器） |
| 测试 | `backend/app/src/business/erp/model_tests.rs` | 真库往返用例 `account_foreign_currency_flag_round_trips_and_filters` |
| 测试 | `frontend/tests/unit/erp-finance-base.test.ts` | 外币标记/筛选/禁用/文案 4 组用例 |
| 脚本 | `scripts/verify/m103_account_foreign_currency_drill.sh` | psql 真库往返 35 断言 |
| 脚本 | `scripts/verify/m103_engine_and_api.sh` | 真引擎升级 + 空库首启 + 真 HTTP 35 断言 |
| 脚本 | `scripts/verify/m103_model_tests.sh` | 真库 model 单测（带假绿护栏） |

## 实测结果（全部本机真跑，输出原样落在本目录）

| # | 内容 | 结果 | 证据 |
|---|---|---|---|
| 01 | psql 真库往返：up 首跑 / 结构对拍 / up 幂等（整表指纹不变）/ 脏数据反证（2、-1、NULL、非末级）/ 正向（末级可标、可关、已标不可改非末级）/ down 数据护栏 / down 幂等 / 重放指纹一致 | **PASS=35 FAIL=0** | `01_drill.out` |
| 02 | 真引擎升级（克隆库只应用 V0408，重启 0 个迁移）+ 空库首启（340 个迁移、V0408 自检跑过、`/health` 200）+ 真 HTTP 11 条路径 | **PASS=35 FAIL=0** | `02_engine_and_api.out` |
| 03 | 真库 `erp::model::` 全套（30 条）：本卡新增用例通过；3 条失败见 03c 反证 | 27 passed / 3 failed（**均与本卡无关**） | `03_model_tests.out`、`03b`、`03c` |
| 03b | 有 V0408 时逐条跑：`account_foreign_currency_flag_round_trips_and_filters` / `account_row_maps_direction_and_leaf` | 各 1 passed | `03b_new_cases_pass.out` |
| 03c | **同一模板库、不应用 V0408** 跑那 3 条：`list_pages_respect_filters`(0≠1)、`material_row_maps_type_and_tax`(13.0000≠0.13)、`purchase_order_row_maps_amounts_and_currency`(SUBMITTED≠DRAFT) | **同样失败** ⇒ 模板库 `cloudpivot_test` 数据漂移（前序卡的演练残留），与本卡无关 | `03c_pre_existing_failures.out` |
| 04 | 死码闸 `scripts/verify/perm_menu_gate.py`（闸门自检 19/19 + 连库比对）：`business:erp:account:update` 能授出去 | **闸：PASS**（missing_in_menu = 0） | `04_perm_menu_gate.out` |
| 05 | 前端：`typecheck` / `test:unit` 1866 pass 0 fail / `check`（palettes + 双 typecheck）/ `lint:shell`（无新增页面壳类）/ `build` | 全绿 | `05_frontend_gates.out` |
| 06 | 后端：`cargo fmt -- --check` 零差异 / `clippy -p cloudpivot --lib -D warnings` 零警告 / `account_admin_handler` 4 pass / `erp::handler` 51 pass / `permission_audit_tests` 8 pass / `m1_03_*` 10 pass / `flow_all_role_multiversion_migration` 7 pass | 全绿 | `06_backend_gates.out` |
| 07 | 演练库清场 | `cp_m103 残留库 = 0` | `07_db_cleanup.out` |

真 HTTP 实测的 11 条路径（`02_engine_and_api.out` C1–C11）：
列表现有科目全带 `is_foreign_currency`（false）→ 开启末级 200/true 且库里 =1、
`update_by` 落操作人 → 重复开启 200 且 `updated_at` 不刷新（幂等）→ 汇总科目开启
`400 + 66080` 且库里仍 0 → 不存在 `404 + 10002` → 非法 UUID `400 + 66006` →
缺字段 `422 + 10001`（不静默当 false）→ 列表 `is_foreign_currency=true` 只回被标科目 →
关闭 200/false 且库里回 0 → 未带令牌 401 → 原末级改成非末级后再开启仍 `400 + 66080`。

## 三个口径决定（写清为什么，免得下游再问）

1. **`SMALLINT 0/1` 而不是 `BOOLEAN`**：同表 `is_leaf`（V0137）/ `status` 都是
   `SMALLINT NOT NULL DEFAULT …`，同一张表的同类布尔位同型；DTO 侧照抄 `is_leaf` 的 `== 1` 转换。
2. **只有末级科目可为 1**（`ck_biz_account_fc_leaf` + 400/66080）：汇总科目自己那一行没有余额，
   标上会让 M1-05 期末调汇扫出金额恒 0 的凭证；若上下级都标就是同一笔外币余额调两次。
   同一条 CHECK 也挡住「已标外币的科目被改成非末级」（反向入口）。
3. **幂等 + 不乐观更新**：开关同值重复提交 = no-op（`IS DISTINCT FROM` 谓词，不白刷 `updated_at`）；
   前端以服务端返回值为准写回行数据，失败不动数据（避免「界面开着、库里没开」）。

## 下游约定（给 M1-04 / M1-05 / M1-07 / M1-11）

- 取「哪些科目要调汇」：`WHERE is_foreign_currency = 1`（部分索引 `idx_biz_account_foreign_currency`
  谓词同形，直接命中）；**别再判 `currency_code <> 'CNY'`** —— 那是科目币种属性，与「要不要调汇」是两件事。
- 前端 `AccountRow` 已带 `is_foreign_currency`，列表无需额外请求。
- 科目结构仍只读：本卡只开这一位，没有科目 CRUD。

## 复跑命令（99 上）

```bash
bash scripts/verify/m103_account_foreign_currency_drill.sh 172.18.10.145   # 真库往返
bash scripts/verify/m103_engine_and_api.sh 172.18.10.145                   # 真引擎 + 真 HTTP（端口 18088）
bash scripts/verify/m103_model_tests.sh 172.18.10.145                      # 真库 model 单测（SKIP / 0 命中即判红）
cd backend && cargo fmt -- --check && cargo clippy -p cloudpivot --lib -- -D warnings
cargo test -p cloudpivot --lib business::erp::account_admin_handler
cargo test -p cloudpivot --test m1_03_account_foreign_currency_migration
cd ../frontend && pnpm typecheck && pnpm test:unit && pnpm lint:shell && pnpm build
```

三个脚本都会自己建库、自己 DROP；库名含 `drill` / `e2e` / `upgrade` / `models` / `case` / `base` / `gate`
（演练库只建在 145，`cloudpivot` 用户，模板 `cloudpivot_test`）。在 145 上跑把 HOST 传 `127.0.0.1`。
