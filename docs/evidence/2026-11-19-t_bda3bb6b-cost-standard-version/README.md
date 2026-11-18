# M5-08 · 标准成本：版本表（物料 × 期间）· 含生效/失效 — 证据包

> 卡：`t_bda3bb6b`（M5 成本批第 8 张）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 演练库：`cp_m508_e2e_full`（`scripts/verify/make_full_suite_db.sh` 空库首启 → 全量迁移 395 条 /
> 最大版本 **463**）+ `cp_m508_probe*`（迁移幂等与反证专用，跑完已 drop）
> 本卡**不动账、不出凭证**（ARCH-9 D-15：标准成本只做差异分析，不改存货计价）

## 一、交付了什么（1 条迁移 + 3 个 Rust 模块 + 8 条接口 + 13 条真库用例）

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0463__cost_standard_version_guard.{up,down}.sql` | 4 件护栏：启用版本区间排除约束（EXCLUDE）+ **收窄版**同日唯一索引 + 版本号唯一 + 物料/区间定位索引；含 8 条行为探针与 3 条反证护栏 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_dto.rs` | 契约层：期间解析、三要素算数（与库层 4 条 CHECK 同口径）、区间覆盖/相交/重叠判定、已生效锁改判据、3 个业务码、入参/出参 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_model.rs` | 取数层：卡头 + 物料实时 join + 明细行数、标准 BOM、按物料/日期/期间取版本、同期换版探测、`pg_advisory_xact_lock` 串行化、卡头/明细写入原语 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_handler.rs` | 接口层：8 条路由 + 三条纪律（重叠 / 已生效锁改 / 已生效禁删）+ 生效失效闭环 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_db_tests.rs` | 13 条真库用例（`ERP_DB_URL` 开关，不设即 SKIP） |
| 后端 | `backend/app/src/business/erp/cost/mod.rs` | 注册 4 个模块（3 pub + 1 cfg(test)） |
| 后端 | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | 新码 `66108`（重叠）/ `66109`（改已生效口径）/ `66110`（删已生效）+ 码表稳定性用例 |

### 8 条接口（4 个**复用**权限码，零新增码）

| # | 方法 | 路径（前缀 `/api`） | 码 | 说明 |
|---|---|---|---|---|
| 1 | GET | `/business/erp/cost-standard` | `…:cost-method:list` | 版本列表（物料 / 状态 / 期间 / 日期 / 关键字） |
| 2 | GET | `/business/erp/cost-standard/effective` | 同上 | **某物料某期间生效的是哪一版**（+ 明细 + 换版提示） |
| 3 | GET | `/business/erp/cost-standard/period-sheet` | 同上 | 期间 × 物料 矩阵（版本表主视图） |
| 4 | GET | `/business/erp/cost-standard/{id}` | 同上 | 版本详情（卡头 + 标准 BOM） |
| 5 | POST | `/business/erp/cost-standard` | `…:cost-method:create` | 新建一版（含标准 BOM，一个事务） |
| 6 | PUT | `/business/erp/cost-standard/{id}` | `…:cost-method:update` | 改一版（未生效可全改；已生效只许改停止日/状态/备注） |
| 7 | POST | `/business/erp/cost-standard/{id}/status` | 同上 | **生效 / 失效**（失效不占窗口） |
| 8 | DELETE | `/business/erp/cost-standard/{id}` | `…:cost-method:delete` | 软删（已生效过 409，改失效） |

## 二、口径（决定，不是待办）

1. **不新建表**。标准成本的两张表 V0374（`biz_cost_standard` 卡头 / `biz_cost_standard_item` 标准 BOM）
   早就建好且**零业务代码**；`biz_cost_variance.standard_id` 还 `ON DELETE RESTRICT` 指向它们。
   本卡是「给既有表补代码 + 补护栏」，再画一张「版本表」会让差异卡（M5-11…M5-16）面对两张表。
2. **「物料 × 期间」是取数视图，不是新列**。版本的时间维度由 `effective_from` / `effective_to`
   表达；`period_code → [期初, 期末]` 在 DTO 里解析，一版可以连月有效（不必每月复制一张卡）。
   期间**末**生效的那一版就是差异分析的口径（与结账口径一致）；期间中途换版由
   `switch_in_period` + `other_versions` 如实报出，不静默取其一。
3. **三要素算数（与库层 CHECK 逐条同口径）**：
   `料 = Σ round(标准用量 × 标准价, 6)`、`工 = round(标准工时(先舍 4 位) × 标准工资率, 6)`、
   `总 = 料 + 工 + 费`。先舍工时到 4 位再乘，是因为 `ck_biz_cost_std_labor` 拿**存下来的**工时算；
   超 4 位小数直接 400（不静默丢精度）。
4. **权限码复用 `cost-method` 四个，不新开**：标准成本卡与「按什么方法核算」是同一类
   **全公司财务口径配置**，维护角色一致（FINANCE_MANAGER 改 / FINANCE_DIRECTOR 只看）。
   新开码在 M5-18（标准成本页）落地前没有菜单行承载 ⇒ 就是「有码无菜单」死码（n14 判红）；
   先插菜单又会指向不存在的组件（n08 判红）。M5-07 在同一岔路口同样选了复用。
5. **已生效版本（`effective_from < 今天`）只许改停止日 / 状态 / 备注**，且要求「按明细重算的金额 ==
   库里的金额」——否则一次「只改备注」的保存会把历史标准悄悄重算成另一个数（库层没有
   「料 = Σ明细」的 CHECK）。要换口径：新增一版 + 旧版收口停止日。
6. **`uk_biz_cost_std` 换成带 `status = 1` 谓词的 `uk_biz_cost_std_active_start`**（本卡踩到的真问题）：
   V0374 的旧索引把**失效**行一起锁，于是「标准过时 → 失效 v1 → 同一天按新口径重建 v2」这条
   唯一正路直接撞唯一键（实测 409），用户只能改库或把生效日挪到月中（期间对齐就破了）。
   它想防的「同一天两个启用版本」已由区间排除约束覆盖，收窄后既不丢防线、又放开了失效→重建。
   btree_gist 缺失的降级环境里，这条部分唯一索引就是「同一天不许两个启用版本」的唯一库层防线。
7. **并发**：写路径在事务内 `pg_advisory_xact_lock(物料)` 串行化 → 重查版本 → 判重叠；
   库层 `ex_biz_cost_std_active_window`（EXCLUDE，仅锁启用未删行）兜「绕过 handler 直插」。
8. **失效留痕**：给了 `reason` 才追加到备注（`[失效 yyyy-MM-dd] 原因`），不给原因不动业务备注；
   追加后仍守 200 字上限（超了 400，不静默截断）。
9. 数据权限 **SCOPE-EXEMPT**（全公司口径配置，不是部门单据），分页走 `scope::paginate_unscoped`。
10. 本卡**不出凭证、不动库存、不动账**：提交后只剩一次只读回查 ⇒ 没有「提交后失败留死单」的面
    （AGENTS 约定 11）。

## 三、真跑的验收（每项可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_migration_and_schema.out` | 空库首启全量迁移 395 条 / 最大版本 **463**；V0374+V0463 `success=t`；`ex_biz_cost_std_active_window` 定义带 `daterange &&` 且谓词 `deleted_at IS NULL AND status = 1`；4 条 CHECK + 5 个索引（含收窄后的同日唯一） | ✅ |
| `02_cost_standard_tests.out` | `cargo test --lib business::erp::cost::cost_standard`（真库 `cp_m508_e2e_full`）：**40 passed / 0 failed**（27 纯函数 + 13 真库用例） | ✅ |
| `03_permission_audit.out` | `system::menu::permission_audit_tests`：**8 passed**（N14 死码 / N16 裸路由 / N20 非超管授权 / N21 模板遮蔽） | ✅ |
| `04_menu_invariants.out` | `scripts/verify/menu_invariants.sh cp_m508_e2e_full`：**8 条全绿**（菜单 569 ∈ 400–580；本卡未种菜单） | ✅ |
| `05_http_smoke.out` | 真实例 `127.0.0.1:18244` 上跑 `smoke_cost_standard.py`：**52 PASS / 0 FAIL**（建版算数 / 三种筛选 / 详情 / 期间取数含 404+400 / 矩阵 / 重叠 409+66108 / 相邻放行 / 未生效改版 / 已生效锁改 409+66109 / 删 409+66110 / 失效→同日重建→再启用 409 / 删未生效 / 清场零残留） | ✅ |
| `06_migration_idempotent.out` | `V0463.up` 连跑两遍 0 ERROR / 探针零残留；`down` 连跑两遍幂等（并把 V0374 的 `uk_biz_cost_std` 建回来）；再 `up` 复原 | ✅ |
| `06b_migration_teeth.out` | 4 条反证：① 库里已有区间重叠 ⇒ up 拒绝并给定位查询 ② 升级后直插同日第二条启用 ⇒ 唯一键拒绝 ③ 同日一条**失效** ⇒ 放行（本卡放开的能力）④ 同日「启用+失效」数据存在时 down 拒绝恢复旧索引（不放静默丢行） | ✅ 有牙 |
| `07_fmt.out` / `08_clippy.out` | `cargo fmt --check` / `cargo clippy --lib -D warnings`（含 `--lib --tests`） | rc=0 |
| `09_cost_domain_regression.out` | `cargo test --lib business::erp::cost::`（**纯逻辑档**，不连库）：**864 passed / 0 failed** | ✅ |

## 四、13 条真库用例（`cost_standard_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `create_version_computes_amounts_and_items` | 建版三要素算数（17.166666 / 77.777778 / 107.290122）、版本号从 1 起、明细带物料码、物料实时 join |
| 2 | `overlap_is_rejected_while_adjacent_periods_pass` | 重叠 409/66108 且报文给三条出路；端点相接放行且版本号递增；清场零残留 |
| 3 | `effective_resolves_by_date_and_period` | 按日两端含；期间取期末；区间外 404（报文给建卡引导）；期间与日期矛盾 400；缺取数条件 400 |
| 4 | `effective_reports_switch_inside_the_period` | 期间内换版 ⇒ 取期末版 + `switch_in_period` + `other_versions` |
| 5 | `period_sheet_lists_materials_with_switch_flag` | 矩阵一行一物料、换版标记、关键字收窄、期间必填 400 |
| 6 | `effective_version_cannot_rewrite_cost_basis` | 已生效改费率/生效日 409/66109；改停止日/备注放行且金额一分未动 |
| 7 | `future_version_can_rewrite_items` | 未生效版本换标准 BOM（整表替换）金额重算；`Some([])` 清空 |
| 8 | `delete_rules_protect_effective_history` | 已生效禁删 409/66110；未生效可删且删后 404 |
| 9 | `deactivate_frees_the_window_for_a_new_version` | **「含生效/失效」闭环**：失效给警告 + 备注留痕 → **同日同窗口重建新版放行**（V0463 收窄索引的能力）→ 再启用旧版 409/66108 → 重复失效幂等 → 失效新版后旧版可恢复 → 缺 status 400 |
| 10 | `db_exclusion_guard_blocks_overlapping_active_rows` | 绕过 handler 直插重叠启用行 ⇒ 库层区间约束拒绝；同窗口 status=0 可并存 |
| 11 | `db_version_no_is_unique_per_material` | 绕过 handler 直插同版本号 ⇒ `uk_biz_cost_std_ver` 拒绝 |
| 12 | `page_filters_status_period_and_keyword` | 状态 / 期间 / 关键字筛选真生效；乱码关键字 0 条 |
| 13 | `material_validation_rejects_missing_and_service` | 物料不存在 / 服务类 / 停用 / 明细物料不存在 ⇒ 400 且点名（第几行） |

## 五、给下游卡的接缝

- **M5-09 / M5-18（维护页 / 标准成本页）**：8 条接口已是全量消费者；列表用 `cost-standard`，
  矩阵用 `period-sheet`（页面第一屏就是「物料 × 期间」），单版编辑用 `PUT` + `POST …/status`。
  **数值字段按字符串传**（`rust_decimal` 默认不开 `serde-float`，JSON 浮点会被拒）。
- **M5-10（发布 / 审核后生效）**：本卡 `status` 只有 1/0（生效/失效）；发布审批要加的是
  「草稿 → 审批 → 生效」的状态机与 `flow` 绑定。建议在卡头加 `publish_state`，**不要**在
  `status` 上叠语义（`status=1` 已表示「参与期间取数」，两者混用会让「审批中」的状态
  在差异分析里算成生效）。
- **M5-11~M5-16（差异）**：取标准一律走 `GET /cost-standard/effective?material_id=&period_code=`
  或 `biz_cost_standard` 直查（启用 + 覆盖期末），并检查 `switch_in_period`：期间内换过版时
  「按期末一套口径算整月」是**当前**口径，分段口径属 M5-31 的切换策略。
- **M5-31（方法切换）**：标准成本卡的生效区间时间线就是「哪天起按哪版标准算」的切换史，
  不需要另建表（与 M5-01 的 `biz_cost_method_config` 时间线同构）。

## 六、已知边界（本卡刻意不做）

1. **标准 BOM 不做 BOM 展开**：`biz_cost_standard_item` 只有一层耗用物料（V0374 的设计），
   多层 BOM 展开属成本还原（M5-26~M5-30）。
2. **状态变更没有独立历史表**：只按「给了原因才追加备注」留痕；要审计级历史请等审批流卡
   （M5-10）引入 `flow_instance`，那时备注留痕可以退役。
3. **前端页面不在本卡**（M5-09 / M5-18）：本卡只交付后端与迁移；没有种菜单，所以
  `menu_invariants` / `permission_audit` 的菜单面不变。
4. **期间分段取数不做**：一期内换版时按期末口径取整期（`switch_in_period` 只提示），
   分段口径属 M5-31。
