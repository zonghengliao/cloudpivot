# E2E08-FIX-3 证据（2026-11-09）

卡：`t_f0e8a6aa` — 物料 DTO 补 `unit_name` / `warehouse_name`（AGENTS.md 约定 13）+ 序列号批量生成 `unit_cost` 文档缺口。

代码提交：`e73839dd`（`origin/main`，`git ls-remote` 复核）。

## 一、145 真机真 token 实测（隔离实例 :8038，库 `cloudpivot_dev`，只发 GET + 一次失败的 POST）

脚本：本目录 `run_145.sh`（= 提交时的 `145_fix3_e2e.sh`）。原始输出 `result.txt`，响应原文
`detail_wh.json` / `detail_nowh.json` / `page_1.json` / `page_20.json` / `batch_no_cost.json`。

> 用 `cloudpivot_dev` 而不是新建库的原因：仓库里 V0363（他人卡片的 monitor 菜单迁移）在**空库首启**时
> 自检失败回滚（`数据库迁移失败: query failed`），本卡不碰它；`cloudpivot_dev` 已迁移到位且带真实主数据。
> 实例是另起的 8038，跑完即杀；8033 / 8034 全程未动（`SERVER_ERRORS=0`）。

| 验收点 | 结果 | 证据 |
|---|---|---|
| ① `GET /material/{id}` 返回 `unit_name` + `warehouse_name` | **PASS** | `unit_name="套"`、`warehouse_name="总仓（种子）"`（与库内 `DEMO-M001` 口径逐字一致） |
| ② 没设默认仓库时 `warehouse_name` 键在、值为 `null` | **PASS** | `D7-M1`：`unit_name="个"`、`warehouse_name=null`、`default_warehouse_id=null` |
| ③ `GET /material/page` 每行都带两个名字 | **PASS** | page_size=1（1 行）与 page_size=20（20 行）逐行断言 |
| ③′ **SQL 条数不随行数增长** | **PASS** | PG `log_statement=all`（按本实例连接 PID 过滤）：`page_size=1 → 4 条`，`page_size=20 → 4 条`；详情 3 条 |
| ④ 批量生成不带 `unit_cost` → 400 + 10001（文档 §10 写的就是这条） | **PASS** | `HTTP=400`，`msg="unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）"` |

断言汇总：`PASS=17 FAIL=0`（`OVERALL=PASS`），`NO_N1_OVERALL=PASS`。

### 无 N+1 的判据（不是「看起来没有」）

```
page_size=1  行数=1   SQL条数=4
page_size=20 行数=20  SQL条数=4     ← 20 倍行数、同样 4 条
```

4 条 = 鉴权 1 + `COUNT(*)` 1 + 数据页 1 + 事务/附加 1；单位名与仓库名都在**同一条数据页 SQL** 里
`LEFT JOIN` 出来（`u.id = m.unit_id` / `w.id = m.default_warehouse_id`，两侧都是主键），
所以一页 N 行不会有 N 次回查。代码层还有两条纯逻辑用例钉住这一点：
`material_select_named_is_single_roundtrip`（1 个 SELECT、2 个 LEFT JOIN、ORDER BY 带别名）。

## 二、代码层证据（无库 / 真库单测）

| 用例 | 位置 | 钉住什么 |
|---|---|---|
| `material_res_carries_joined_names` | `backend/src/business/erp/dto.rs` | `MaterialRes` 名字透传；JSON 两个键**始终存在**（`null` 也占位），前端才能区分「没设仓库」与「老后端没这字段」 |
| `material_columns_named_matches_plain` | `backend/src/business/erp/model_tests.rs` | `MATERIAL_COLUMNS_NAMED` 与 `MATERIAL_COLUMNS` 逐字段同序（只差 `m.` 前缀），漏改一份就红 |
| `material_select_named_is_single_roundtrip` | 同上 | 取名是**一条** SQL，两个 LEFT JOIN 都走主键，排序带别名（join 后 `status` / `deleted_at` 会 ambiguous） |
| `material_named_pages_and_detail_carry_names` | 同上（真库） | 一页 3 行只有一条数据页查询；`W1B-M-001` 有仓库名、`W1B-M-002` 无仓库 → `None`；DTO 出口逐条带上 |
| `列表文案：单位译名、分类中文、状态` | `frontend/tests/unit/erp-material-list.test.ts` | `materialUnitLabel`：优先后端 `unit_name`，`null`/空白/缺席时退回本地单位字典 |

## 三、文档与前端 hint
- `docs/api-contract-quickref.md` §10：`POST /serial/batch-generate` 的 `unit_cost` **必填**
  （四种 400 报文逐字、`count` 1..=1000、`location_id`/`location_dict_id` 二选一、
  `source_type` 白名单、出参字符串 Decimal 口径）。
- `frontend/src/views/business/erp/serial/index.vue`：批量生成表单「成本单价」下方 hint 改为
  「批量生成按「期初 / 其他入库」记账（没有采购单可挂），**必须录不含税成本单价**…」，
  `data-testid="batch-unit-cost-hint"`（E2E 可断言）。

## 四、测试与构建

| 项 | 结果 | 证据文件 |
|---|---|---|
| `cargo test --lib`（不设 `ERP_DB_URL`，真库用例 SKIP） | **3990 passed / 0 failed** | `full_lib_no_db.txt` |
| 真库定向 `business::erp::model::tests` | **29 passed / 0 failed**（含本次 3 条） | `db_suites.txt` |
| 真库定向 `business::erp::dto::tests` | **19 passed / 0 failed**（含 `material_res_carries_joined_names`） | 同上 |
| 真库定向 `business::erp::handler::tests` | **51 passed / 0 failed** | 同上 |
| `cargo check --all-targets`（隔离 worktree，HEAD f8d775e6 + 本卡改动） | 通过（无 error / warning） | `cargo_check.txt` |
| 前端 `vue-tsc --noEmit` | 通过（无输出，RC=0） | —— |
| 前端 `vite build`（`--outDir dist-e2e08fix3`，避免覆盖别人在用的 `dist/`） | 通过 | 见下 |
| 前端单测 `tests/unit/erp-material-list.test.ts` | **11 passed / 0 failed** | —— |

> 真库用**克隆库** `cp_fix3full2`（`TEMPLATE cp_fix4_ut`，已迁移到 362）：
> 本机从空库首启会撞上仓库里 V0363（他人卡片的 monitor 菜单迁移）的自检失败回滚，
> 与本卡无关；克隆库不跑迁移校验，只给 DB 用例用。跑完删库即可（本卡未删，留给复核）。

## 五、已知取舍

- 单位 / 仓库被**软删**后名称照给（不按 `deleted_at` 过滤被关联表）：物料指向的单位即使停用/软删，
  界面也该显示「箱」而不是空白；这与列表页原来的字典兜底口径一致（字典也含停用项）。
- 前端保留本地 `/erp/unit/page` 字典映射作为**兜底**（老实例 / 缓存中的旧响应），
  但列表渲染已不再依赖它先加载完。
