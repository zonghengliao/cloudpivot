# M5-01 · 成本方法配置（品种法 / 分批法 / 标准成本）— 证据包

> 卡：`t_43ccfe64`（M5-01，M5 成本批第 1 张）· 日期：2026-11-18 · 主机：99（开发机，PG 在 145）
> 演练库：`cp_m501_dev`（`createdb` 空库 → 后端实例跑全量迁移；跑完已 drop）

## 一、交付了什么（一张表 + 五个接口 + 一页 + 一处接线）

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0459__cost_method_config.{up,down}.sql` | 表 `biz_cost_method_config`（13 列 / 5 CHECK / 4 索引）+ 默认口径种子行（`GLOBAL`/`JOB`，1970-01-01 起长期有效）+ 末尾自检 |
| 迁移 | `backend/app/migrations/business/V0460__cost_method_menu_and_perm.{up,down}.sql` | 菜单 4 行（1 页面 + 3 按钮，挂「成本核算」目录 sort 56）+ 4 个权限码 + 角色授权 |
| 后端 | `backend/app/src/business/erp/cost/cost_method_dto.rs` | 值域 / 判定（对象级 > 全公司默认 > 代码兜底 JOB）/ 区间重叠 / 5 个业务码，纯函数 |
| 后端 | `backend/app/src/business/erp/cost/cost_method_model.rs` | 取数（全量一次 + 分页 + 本期拦截清单）/ 写入 / 软删 |
| 后端 | `backend/app/src/business/erp/cost/cost_method_handler.rs` | 5 条路由 + 三条纪律（重叠 / 已生效锁定 / 已生效禁删）+ 计算入口方法闸 |
| 后端 | `cost_calc_period.rs`（+6 行） | 开算前调 `ensure_methods_runnable_in_tx`：本期有未实现方法 ⇒ `409 + 66101`，库里零改动 |
| 后端 | `crates/cloudpivot-kernel/src/shared/error_code.rs` | 新码 `66101`–`66105`（66xxx 段接在 66100 后）+ 码表稳定性用例 |
| 前端 | `frontend/src/api/business/erp/cost-method{-kinds,}.ts` | 词表 + 5 个接口 |
| 前端 | `frontend/src/views/business/erp/cost-method/{index.vue,cost-method-logic.ts}` | 列表 + 配置表单（已生效版本口径字段置灰）+ 生效方法查询探针 |
| 前端 | `frontend/src/router/index.ts` | 路由 `business/erp/cost-method`（菜单 path 必须在路由表里，否则 n08 红） |
| 测试 | `cost_method_db_tests.rs`（11 条真库用例）+ 后端纯函数用例 21 条 + `frontend/tests/unit/erp-cost-method.test.ts`（14 条） | 见下表 |

## 二、口径（决定，不是待办）

1. **核算方法 ≠ 存货计价法**。`biz_material.costing_method` 按 ARCH-9 D-2 已收敛为只有 `MOVING_AVG`，一个字节没碰；本卡另开 `biz_cost_method_config`，值域 `JOB`（分批法）/ `VARIETY`（品种法）/ `STANDARD`（标准成本）。
2. **判定顺序**：成本对象级 > 全公司默认 > 代码兜底 `JOB`；同级取生效日最晚的一条；停用行不参与。
3. **一期只实现分批法**（ARCH-9 D-3）。三种方法都可配置（二期 / 三期排期数据，也是 M5-31 切换策略的输入），但**计算入口会拦**：本期（与期间相交）出现 `method <> 'JOB'` 的启用配置 ⇒ `409 + 66101`，报文点名对象号与三条出路（推到下期 / 停用 / 改回 JOB）。**不静默按分批法算完**——那样报表是错的且看不出来。
4. **已生效过（`effective_from < 今天`）的版本只允许改停止日 / 状态 / 备注**（`409 + 66105`），**也不许删**（`409 + 66104`）。切换方法的正确姿势：旧版停止日收到切换日前一天 → 新增切换日生效的版本（用例 9 就是这个流程的说明书）。
5. **同范围生效期不许重叠**（`409 + 66102`；库层另有 `(scope, effective_from)` 唯一索引兜底并发）。
6. 数据权限 **SCOPE-EXEMPT**（全公司财务口径配置，不是部门单据），分页走唯一入口 `scope::paginate_unscoped`。

## 三、真跑的验收（每项都可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_migrate_and_schema.out` | 空库首启：`schema_migrations` 392 条 / 最大版本 460；V0459/V0460 `success=t`；表 13 列；种子行；菜单 4 行（页面挂「成本核算」sort 56）；授权 FINANCE_MANAGER×4 / FINANCE_DIRECTOR×1 / SUPER_ADMIN×4 | ✅ |
| `02_cost_method_tests.out` | `cargo test --lib cost_method`：**37 passed / 0 failed**（dto 纯函数 17 + handler 4 + model 5 + 真库用例 11） | ✅ |
| `03_cost_domain_regression.out` | `cargo test --lib business::erp::cost::`：**669 passed / 0 failed**（含 calc 全链路六段用例——证明加在 `run_in_tx` 的方法闸没破坏既有行为） | ✅ |
| `04_permission_audit.out` | `system::menu::permission_audit_tests`（N14 死码 / N16 裸路由 / N20 非超管授权 / N21 模板遮蔽）：**8 passed** | ✅ |
| `05_menu_invariants.out` | `scripts/verify/menu_invariants.sh cp_m501_dev`：**8 条全绿**（菜单总数 568 ∈ 400–580；同父 sort 唯一；title ≤12；icon 真实存在） | ✅ |
| `06_fmt.out` / `07_clippy.out` | `cargo fmt --check` / `cargo clippy --lib -D warnings` | rc=0 |
| `08..10` 前端 | `pnpm typecheck` / `typecheck:unit` / `tsx --test erp-cost-method`（14 passed） | rc=0 |
| `11_shell_variant.out` | 页面零壳类声明（把 `panel-head__tip` 换成本页私有 `method-page__tip`） | ✅ 无新增 |
| `12_up_idempotent_down_guard.out` | up 连跑两遍 0 ERROR；`V0460.down` 删菜单可重放；`V0459.down` 有数据时**拒绝回滚**（报文点名几条）；清数据后放行；再 up 恢复正常 | ✅ 有牙 |

另跑过：`bash scripts/check.sh --fast`（L0+L1 全绿，唯一告警是本页壳类声明，已在提交前改掉）。

## 四、11 条真库用例覆盖（`cost_method_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `seed_row_is_the_phase_one_default` | 种子行（GLOBAL/JOB）存在，生效查询落到它 |
| 2 | `create_list_and_effective_for_object` | 对象级配置 CRUD 出参（带对象号）+ 生效查询按日期切换 + `method_implemented=false` + 结论含 `66101` |
| 3 | `overlap_rejected_and_adjacent_window_accepted` | 重叠 `409/66102`；相邻窗口（旧版止于前一日）放行 |
| 4 | `same_effective_day_is_blocked_by_the_unique_index` | 绕过 handler 直插 → 库层唯一键冲突（并发兜底真的在） |
| 5 | `effective_window_is_inclusive_on_both_ends` | 生效期两端含，区间外落回默认 |
| 6 | `scope_validation_rejects_bad_targets` | 全局带对象 / 对象不存在 / 对象已作废 ⇒ `400/66103` |
| 7 | `delete_rules_protect_effective_history` | 已生效不许删 `409/66104`；未来版本可删且删后查不到 |
| 8 | `update_rules_protect_effective_history` | 已生效改方法 / 改生效日 `409/66105`；改停止日 / 备注 / 停用放行 |
| 9 | `switch_method_needs_closing_the_previous_version` | 完整切换流程（拦 → 收停止日 → 放行 → 按日期切 → 还原） |
| 10 | `calc_guard_blocks_unimplemented_method` | `ensure_methods_runnable_in_tx` 与 `calc_period` 双双 `66101`；停用后放行 |
| 11 | `page_filters_and_keyword` | 范围 / 方法 / 状态 / 关键字筛选真的生效 |

## 五、给下游卡的接缝（M5-02 / M5-08 / M5-31 直接调用）

```rust
// 判定某对象某天用什么方法（纯函数，进程内零查库）
let rows = cost_method_model::configs().await?;                    // 一次取全表
let views: Vec<MethodConfigView> = rows.iter().map(CostMethodRow::to_view).collect();
let hit = cost_method_dto::resolve(&views, Some(cost_object_id), period_end);
hit.method / hit.source / hit.config
```

- 归集 / 结转层（M5-02 起）：按 `hit.method` 分支；非 `JOB` 时应复用 `dto::unimplemented_error` 或直接由计算入口闸拦。
- 标准成本（M5-08 起）：`method = STANDARD` 的配置行即「这批对象按标准卡算差异」的依据。
- 切换（M5-31）：`configs` 的时间线就是切换史（`effective_from`/`effective_to`），不需要另建表。

## 六、已知边界（本卡刻意不做）

1. **品种法 / 标准成本的计算实现**属 M5-08…M5-35（二期 / 三期），本卡只做「选方法 + 拒绝在未实现时静默计算」。
2. **按产品 / 按车间**的方法粒度没做：ARCH-9 §7 尚未定这类层级，无写入方的层级就是死结构；真要做时追加迁移放行新的 `scope_type` 值即可。
3. **切换时的存量期间重算策略**（M5-31）：本卡只提供「版本时间线 + 已生效锁定」两块地基，不实现「切了以后历史期间要不要重算」。
4. 前端未做浏览器 E2E（本卡无独立前端卡，页面随本卡落地）：`pnpm build` + typecheck + 14 条纯逻辑单测已覆盖；需要浏览器档的话建议并入 M5-07（批次成本页）一起跑。
