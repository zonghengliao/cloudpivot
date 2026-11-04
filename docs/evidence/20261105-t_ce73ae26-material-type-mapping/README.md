# t_ce73ae26 · 预算映射 `MATERIAL_TYPE` 存不进去（修法与验收）

> 卡面：`【FIN-1-BE】预算映射 MATERIAL_TYPE 存不进去：validate_mapping 要 ref_id、resolve_for_po 只认 ref_code，假 ref_id 又撞 uk_biz_bsm_target`
> 基线：`df74a5f`（父卡 t_539dab1d 认证的 HEAD） · 交付：见本目录同一提交
> 环境：99 `/opt/cloudpivot` · 隔离库 `cloudpivot_ce73ae26`（`scripts/verify/erp_clean_db.sh` 自举，V0240/178 条迁移） · 后端 `127.0.0.1:18162` profile=test · 前端 vite dev `127.0.0.1:5183`

## 1. 结论：走方案 B（`ref_code` 就是物料类型编码），不是方案 A

卡面给了 A/B 两个修法，**A 在本仓不成立**：物料类型从来不是一张表，它是
`biz_material.material_type` 上的字符串枚举（`ck_biz_material_type`，V0187 由 4 值扩到 7 值：
`RAW / SEMI / FINISHED / PACKAGING / CONSUMABLE / OFFICE / SERVICE`），全库没有
`biz_material_type` 表、也没有 uuid 可指。给物料类型凭空造主数据表属于另一个功能（主数据化），
不在本缺陷的修复面。

所以按 B 收口：**`ref_code` 是目标编码，`ref_id` 在 `MATERIAL_TYPE` 上恒为 NULL**，
把校验、`CHECK`、唯一索引、解析四处的口径统一到这一句话上。

### 改动后各类型的 `ref_id` / `ref_code` 语义（写进代码注释与 DDL COMMENT）

| map_type | `ref_id` | `ref_code` | 唯一键（部门内） |
|---|---|---|---|
| `MATERIAL` | 必填 = `biz_material.id` | 可空（展示/对账） | `ref_id` |
| `WAREHOUSE` | 必填 = `biz_warehouse.id` | 可空（展示/对账） | `ref_id` |
| `MATERIAL_TYPE` | **必须为空**（类型没有独立 id） | **必填** = 7 值类型编码，写入时转大写 | `upper(btrim(ref_code))` |
| `DEFAULT` | 必须为空 | 必须为空 | 空串（同一部门只有一条兜底） |

## 2. 改了什么

### 2.1 迁移（新号 V0240，`business/` 与 `system/` 共用序列，动手前复核最大号是 V0239）

`backend/migrations/business/V0240__erp_budget_mapping_material_type.up.sql`（+ `.down.sql`）

- `ck_biz_bsm_ref` 从两档改三档：`DEFAULT → ref_id IS NULL`；`MATERIAL_TYPE → ref_id IS NULL 且 ref_code 非空`；
  `MATERIAL/WAREHOUSE → ref_id IS NOT NULL`。**旧约束对 MATERIAL_TYPE 强制 `ref_id` 就是「存不进去」的第一道墙**
  （`raw/old-check-probe.txt` 用真库表达式复算：旧 CHECK 对 `MATERIAL_TYPE + NULL ref_id` 判定 `f`）。
- `uk_biz_bsm_target` 目标键改成 `(map_type, COALESCE(ref_id::text, upper(btrim(ref_code)), ''), COALESCE(dept_id, 0-uuid))`。
  语义变化三条：① MATERIAL_TYPE 的唯一性落在编码上（同部门同类型只能一条）；② 顺带堵上旧索引的洞
  —— 旧键里 `ref_id` 为 NULL 时 Postgres 认为行互不相同，导致「同部门可以重复建任意多条 DEFAULT」；
  ③ 编码比较做了 `upper(btrim())`，配置侧存 `raw` 也挡得住与 `RAW` 的重复。
- 自检三道（都是「先检查、后改」，失败即整文件回滚）：新 CHECK 的既有行、新唯一键下的重复组
  （列出前 5 组、**不替用户删配置**）、改完后的索引表达式与约束存在性断言。
- down 反向恢复旧约束/旧索引，并在「表里还有 MATERIAL_TYPE 行」时 RAISE 拦一道（不静默删配置）。

> 实况：99 的 `cloudpivot_*` 与 144 的 `cloudpivot / cloudpivot_dev / cloudpivot_test` 在 V0227 之后
> **这张表一行数据都没有**（144 上甚至还没跑 V0227：`relation does not exist`），所以自检在现有环境全是 0 命中。

### 2.2 后端代码

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/budget_dto.rs` | `validate_mapping` 按类型分三档校验（MATERIAL_TYPE 只认 `ref_code`、拒绝 `ref_id`、编码必须在 7 值域内；DEFAULT 两列都不许给；MATERIAL/WAREHOUSE 必须有 `ref_id`）。新增 `normalize_ref_code` / `normalize_target`：写路径统一归一化（MATERIAL_TYPE 转大写、没有 id 的类型把 `ref_id` 丢成 NULL）。`MappingReq` 的文档注释里写清四种类型的 `ref_id` / `ref_code` 语义 |
| `backend/src/business/erp/budget_model.rs` | `map_insert` / `map_update` 走 `normalize_target`（`map_update` 新增 `map_type` 参数，只用于编码归一化）；`resolve_for_po` 的 MATERIAL_TYPE 分支改为 `trim + eq_ignore_ascii_case` 比对 `ref_code`，函数文档补了四类型命中口径表 |
| `backend/src/business/erp/budget_handler.rs` | `mapping_create` 的 23505 文案改成「map_type + 物料/仓库 id 或物料类型编码 + 部门」；`mapping_update` 也补上 23505 → 409（改部门/改编码同样会撞唯一键） |
| `backend/src/business/erp/budget_dto_tests.rs` | `mapping_validation` 扩成三档（含本卡回归断言「MATERIAL_TYPE 只给 ref_code 必须通过」「带 ref_id 必须拒」「值域外编码必须拒」）+ 新增 `mapping_target_normalization` |
| `backend/src/business/erp/budget_model_tests.rs` | 新增真库用例 `db::material_type_mapping_roundtrip`（落库 → 解析命中 → 撞唯一键 → 互不干扰 → 软删可重建）；新增 `purge_target` 精确清同目标残留（否则第二次跑必撞唯一键，见 `raw/cargo-test-budget.log` 的可重复两次） |
| `docs/architecture/ARCH-6-财务扩展设计.md` | 设计稿里的 DDL 片段与 V0240 对齐（列注释 / 三档 CHECK / 新唯一键 + 「`COALESCE` 第三参不能省」的坑） |

### 2.3 前端（同卡内撤掉拦截）

`frontend/src/views/business/erp/budget/budget-logic.ts`、`mapping.vue`

- 删除 `mappingSaveBlocker`（连同 `t-alert` 提示、保存按钮的 `disabled`、`submitForm` 的短路）。
- `checkMappingDraft` 改成三档镜像后端：MATERIAL_TYPE 必须有 `ref_code`、必须没有 `ref_id`；
  DEFAULT 两列都不许给；MATERIAL/WAREHOUSE 仍需 `ref_id` + `ref_code`。
- `toMappingReq`：MATERIAL_TYPE 的 `ref_id` 一律 `null`（切类型时残留的旧 id 不会再被发出去）。
- 文件头与 `handleMaterialTypeChange` 的注释改成「类型编码是唯一目标」的现状说明。

## 3. 验收对照（卡面 1–4）

| # | 卡面要求 | 结论 | 证据 |
|---|---|---|---|
| 1 | `POST /api/business/erp/budget/mapping` 用 `map_type=MATERIAL_TYPE` + 类型编码能 200 落库；同部门第二条返回可读 409/400（不是 23505 裸错） | **PASS** | `raw/e2e_mapping.log` 21/21（用例 3 = 200 落库；5 = 409「该目标（map_type + 物料/仓库 id 或物料类型编码 + 部门）已有映射：请直接修改那一条」；6 = 小写 `raw` 同样 409；9/10 = 400 分别点名 `ref_id` / `ref_code`）；`raw/e2e_report.json` |
| 2 | `resolve_for_po` 对「物料类型命中」有真实单测；`budget_model_tests` / `budget_guard_tests` 全绿 | **PASS** | `raw/cargo-test-budget.log`：`cargo test --lib business::erp::budget` → **40 passed / 0 failed**（真库 `ERP_DB_URL` 打开，`material_type_mapping_roundtrip` 打印「用部门 …ce73ae26 跑部门级映射」＝真跑了不是 SKIP），连跑两遍结果一致 |
| 3 | 新迁移（下一个空号）+ 自检（既有行不冲突）+ `validate_mapping` 注释写清 `ref_id`/`ref_code` 语义 | **PASS** | `V0240`（最大号 V0239 → 240）；`raw/migration-notice.txt`（`V0240 ✓` NOTICE + `schema_migrations` 行，checksum `32664aa3…`）；`raw/psql-index-and-check.txt`（真库里的索引表达式与 CHECK 定义）；语义表在 `MappingReq` 的文档注释 + DDL `COMMENT ON COLUMN/INDEX` |
| 4 | 前端撤掉 `mappingSaveBlocker` 对 MATERIAL_TYPE 的拦截 | **PASS** | 代码已删（`grep -rn mappingSaveBlocker src/` 无命中）+ `raw/pnpm-typecheck.log`（vue-tsc `--noEmit` exit 0）；真机验证见下 |

### 真机（vite dev :5183 → 后端 :18162，admin 账号）

1. `/business/erp/budget/mapping` → 新建映射 → 类型选「物料类型（MATERIAL_TYPE）」：**表单不再出现黄色拦截提示**
2. 物料类型选「原材料（RAW）」、预算科目「5602.02 管理费用-差旅费」、部门覆盖「夹具部门A」→ 保存 →
   toast「映射已创建」，列表新增一行 `物料类型 | RAW | 管理费用-差旅费 5602.02 | 夹具部门A`：
   `02-mapping-list-after-save.png`
3. 同表单再保存一次 → toast 直接给可读的 409 文案（`…已有映射：请直接修改那一条，不要重复新建（10003）`），不是 23505 裸错
4. 落库行（UI 创建后快照）`raw/psql-after-ui-create.txt`：`MATERIAL_TYPE | ref_id=NULL | ref_code=RAW | dept=…ce73ae26 | status=1`

## 4. 复现步骤（可照着重跑）

```bash
# ① 干净库（迁移跑到 240）
cd /opt/cloudpivot
ERP_DB=cloudpivot_ce73ae26 PORT=18162 RECREATE=1 bash scripts/verify/erp_clean_db.sh

# ② 真库单测（预算模块 40 条）
cd backend
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_ce73ae26' \
  cargo test --lib business::erp::budget -- --test-threads=1 --nocapture

# ③ HTTP 验收（先起隔离实例 :18162；脚本见 raw/e2e_mapping.py 旁的启动脚本说明）
ACCOUNT_ID=<5602.02 的 id> ACCOUNT_OTHER_ID=<5602.01 的 id> python3 e2e_mapping.py
```

`raw/e2e_mapping.py` 是本次验收脚本（21 条断言，含 DEFAULT 兜底唯一、PUT 撞部门、软删重建）；
它开跑前会把自己上一轮留下的可见映射软删干净，可重复执行。

## 5. 已知边界 / 留给后续卡

- 物料类型仍是**字符串枚举**，不是主数据：新增类型要同时改 `ck_biz_material_type`（迁移）、
  `dto::MATERIAL_TYPES`、前端 `MATERIAL_TYPE_OPTIONS`，`validate_mapping` 已经把它当值域校验，
  所以漏改一处会立刻 400（不会静默存进去）。要做成可维护的主数据，另开卡。
- `GET /api/business/erp/account/page` **不带 `is_leaf`/`status` 参数**时返回空 `list`
  （前端 `fetchBudgetAccountOptions` 带了参数所以 UI 正常）。这不是本卡的缺陷，已记在报告里，未改。
- `cargo clippy --all-targets` 当前在工作区是红的，原因是**别人在途的改动**
  （`tests/pflow1a.rs` / `tests/pflow1b.rs` 没跟上 `TaskCreateReq` 新增的 `doc_id/doc_type`，属 t_bb2e4702 的收尾）；
  本卡改动范围按 `cargo clippy --lib` 验证：**0 warning / exit 0**。
- 回滚：`git revert <本卡提交>`；迁移回滚跑
  `V0240__erp_budget_mapping_material_type.down.sql`（表里还有 MATERIAL_TYPE 行时会被它拦住，需先备份处理）。

## 6. 目录清单

```
README.md                          本文件
02-mapping-list-after-save.png     真机：保存成功后列表出现「物料类型 RAW」
raw/cargo-test-budget.log          预算模块真库单测 40 passed / 0 failed
raw/e2e_mapping.log                HTTP 验收 21 passed / 0 failed
raw/e2e_report.json                同上，结构化（含每条断言的响应原文）
raw/psql-after-ui-create.txt       UI 创建后的落库行（ref_id NULL / ref_code RAW）
raw/psql-index-and-check.txt       真库的 uk_biz_bsm_target 表达式与 ck_biz_bsm_ref 定义
raw/psql-table-def.txt             biz_budget_subject_map 表结构（含列 COMMENT）
raw/migration-notice.txt           V0240 NOTICE 与 schema_migrations 记录
raw/old-check-probe.txt            旧 CHECK 对「MATERIAL_TYPE + NULL ref_id」的判定（f = 旧约束必然拒绝）
raw/pnpm-typecheck.log             vue-tsc --noEmit exit 0
```
