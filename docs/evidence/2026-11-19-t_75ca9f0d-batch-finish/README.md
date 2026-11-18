# M5-04 · 分批法：完工批次结转（完工转出）— 证据包

> 卡：`t_75ca9f0d`（M5 成本批第 4 张，接 M5-03 归集）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 验证库：`cp_m504_dev`（`createdb -T cloudpivot_test_m502`，基线 V0460；V0461 由后端实例自己跑迁移落库）；
> 迁移/回滚反证用独立库 `cp_m504_mig`（用完即 drop）

## 一、交付了什么（一张表 + 两条接口）

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0461__cost_batch_finish.{up,down}.sql` | 表 `biz_cost_batch_finish`（23 列 / 7 CHECK / 1 FK RESTRICT / 3 索引）+ 自检（形态 7 条 CHECK + 外键 + 部分唯一索引；行为 5 条）|
| 后端 | `cost_batch_finish_dto.rs` | 契约与纯逻辑：`Amounts` 值对象 / 结转判定 `evaluate_finish`（9 类拦截）/ 结转计划 `plan_transfer`（差额 + 单位成本 round 6）|
| 后端 | `cost_batch_finish_model.rs` | 取数：累计归集（全期间 FILTER 聚合）/ 已结转与历史 / 已结算计算单 / **成本对象行锁** `FOR UPDATE`（串行点）/ 写结转单 |
| 后端 | `cost_batch_finish.rs` | 编排：`preview`（只读）+ `finish`（单事务：锁行 → 重算差额 → 判定 → 写单 → 回查）|
| 后端 | `cost_batch_finish_handler.rs` | 两条路由 + 权限码字面量自检（宏参 vs 常量）|
| 后端 | `cost_batch_finish_db_tests.rs` | 12 条真库用例 |
| 后端 | `crates/cloudpivot-kernel/src/shared/error_code.rs` | 新码 `66107 ERP_COST_BATCH_FINISH_BLOCKED` + 注册表用例 + 码段文档同步 |

**接口**

```text
GET  /business/erp/cost-collect-sheet/{object_id}/finish-preview?period_code=yyyy-MM   business:erp:cost-object:list（复用）
POST /business/erp/cost-collect-sheet/{object_id}/finish                               business:erp:cost-period:calc（复用）
```

**没有**：新权限码、新菜单、前端页面（前端卡是 M5-07）、任何凭证 / 入库单（见下 ③）。

## 二、口径（决定，不是待办）

1. **本次结转 = 累计归集（全期间三要素） − 已结转（历史结转单活行之和）**，逐要素夹零。
   单位成本 = 本次合计 / 完工数量，`round(…, 6)`（与 ARCH-9 §4.6 ⑤ 同标度）。
2. **分批法整批才转**：`wip_qty > 0` 一律 409（报文写「分批法按批结转，批没完就一分不转（不做约当产量分摊）」）。
   约当产量是 calc 第 6 段（品种法 / 在制品）的算法，不在这里偷偷摊一刀。
3. **不出凭证、不建完工入库单**（边界，不是待办）：ARCH-9 **D-1** 定死「唯一出口 = 完工入库金额 + 三张结转凭证」，
   而那条出口由期间结算独占 —— 完工入库单必须挂 `cost_period_id`（`ck_biz_si_production`），结转凭证必须与入库金额同源。
   批次级结转若自己发「借 1405 / 贷 4001」：C-COST-2（`Σ 生产入库 = 1405 借方`）与 C-COST-3（`4001 余额 = Σ 在制 + 在途`）
   会**恒红**。所以本卡的结转是**成本工作底稿**，出参 `warnings[0]` 把这条边界写给用户看。
4. **可重复结转（只转差额）**：结转后才过账的领料 / 才确认的工时是真成本，必须还能转出去；
   同批第 n 次结转各留一行（`transfer_seq` 从 1 递增），差为 0 时**幂等空转**（200 + `inserted=false`，不写空单）。
5. **9 类拦截**（顺序：先这批货本身，再数据卫生）：
   作废 / 已随期间结算结转（`biz_cost_period_item.status='SETTLED'`）/ 还有在制 / 完工数 0 / 累计归集 0 /
   草稿工时 / 已过账未归集领料 / 未过账领料 / 本批活动期间已锁定；外加「结转期间批次已冻结（SUBMITTED+）」。
6. **并发**：`SELECT biz_cost_object … FOR UPDATE` 是串行点（第一次结转时结转单还不存在，锁不住行）；
   第二次进来的请求拿到锁后重算差额 = 0 ⇒ 空转。库层 `uk_biz_cost_batch_finish_seq (object_id, transfer_seq) WHERE deleted_at IS NULL` 是最后一道闸。
7. **不做的事**：不关批（关批是 `POST /cost-object/{id}/close`，有自己 5 条前置与 `force_wip` 裁量；preview 里用 warning 提示「结转后请关批」）、
   不动期间批次 / 计算单 / 归集行、不碰 settle / reverse / report / alloc 一行。
8. **权限码复用**：读 `cost-object:list`（与 M5-02/M5-03 同码）、写 `cost-period:calc`（「算成本」的写动作）。
   不作为 `cost-period:settle` 的理由见 ③：本卡不动账。新开码要种菜单，而页面属 M5-07。
9. **不可见报 404 不报 403**：403 等于承认 id 存在，把别车间批次号变成探测口。

## 三、真跑的验收（每项可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_fmt.out` | `rustfmt --edition 2024 --check` 7 个文件 | **rc=0** |
| `02_clippy.out` | `cargo clippy --lib -- -D warnings` | **0 告警**，rc=0 |
| `03_tests_and_regression.out` | ① 本卡 ② 成本域 ③ 权限审计 ④ d8c 接线 | ① **37 passed**（25 纯 + 12 真库）② **766 passed / 0 failed**（M5-03 时 729，本卡 +37）③ **8 passed** ④ **1 passed** |
| `03b_error_code_registry.out` | `cargo test -p cloudpivot-kernel shared::error_code` | **10 passed**（含 `erp_error_codes_are_stable` 的 66107 断言）|
| `04_migration_up_idempotent.out` | V0461 空库首跑 + 连跑第二/三遍 + 形态回查 | 首跑 `自检通过`；重复 **0 ERROR**；`cols=23 / checks=7 / fks=1` |
| `05_migration_down_guard.out` | down 三道：有活行 → 软删 → 幂等 → 再 up | ① **拒绝回滚（表还在）** ② 软删后放行（`<gone>`）③ 幂等 NOTICE ④ up 可重放（表回来）|
| `06_http_probe.out` | 隔离实例 `:18025` + admin 登录，九类请求 | 见下「HTTP 九连」 |
| `07_route_probe.out` | M5-02 / M5-03 / M5-04 四条 GET 同时在线 | 全 **200**（静态段 `finish-preview` 不遮蔽 `/cost-collect-sheet/{id}`，N21 实测）|
| `08_teeth_a_delta.out` | 反证 A：`plan_transfer` 改成「整额重转」（不分差额） | **红 5 条**（含真库「幂等重放仍写第二行」与「差额结转转成整额」）|
| `09_teeth_b_wip.out` | 反证 B：在制拦截分支 `if false && …` 短路 | **红 3 条**（含真库 `wip_blocks_finish_and_writes_nothing`：`preview.transferable` 变 true、`finish` 会放行写库）|
| `10_sha_before_mutation.txt` + `11_after_restore_green.out` | 变异前 sha256 与还原后重跑 | `sha256sum -c` **两文件 OK**（逐字节一致）+ **37 passed 复绿** |
| `12_check_fast.out` | `bash scripts/check.sh --fast`（L0 全部静态闸 + L1 fmt/clippy/check/typecheck）| **全部检查通过（L0 L1）**，rc=0 |

### HTTP 九连（`06_http_probe.out`，真实例 + 真登录 + 真库）

| # | 请求 | 结果 |
|---|---|---|
| ① | 无 token 试算 | **401**（未授权访问，请先登录） |
| ② | 缺 `period_code` | **400 / 10001**「period_code 必填（yyyy-MM，例如 2026-11）」 |
| ③ | `GET …/finish-preview?period_code=2026-11`（A 批整批完工） | **200**：`collected 料15000 工4000 费4000 合计23000`、`transferred 0`、`pending 合计23000`、`unit_cost 28.750000`、`finished_qty 800.0000`、`blockers []`、2 条 warnings（工作底稿边界 + 结转后请关批） |
| ④ | `POST …/finish` | **200**：`inserted=true`、`transfer 23000.0000`、`remaining 0`、`message="已结转第 1 次：料 15000.0000 / 工 4000.0000 / 费 4000.0000，合计 23000.0000，本次单位成本 28.750000"` |
| ⑤ | 再 `POST …/finish`（重放） | **200**：`inserted=false`、`transfer 0`、`message="无可结转差额……本次没有写新结转单"`；`history[0] = {seq 1, unit_cost 28.750000, 23000}` |
| ⑥ | `POST …/finish`（B 批带在制 200） | **409 / 66107**：`[WIP_NOT_ZERO] 期末在制数量是 200.0000（不为 0）：分批法按批结转，批没完就一分不转（不做约当产量分摊）⇒ …`；`data.blockers` 结构化 |
| ⑦ | 不存在 / 不可见的批次（试算 + 结转） | **404 / 10002**（两者都不区分「不存在 / 不可见」） |
| ⑧ | `period_code=2026-13` | **400 / 10001**「期间编码必须是 yyyy-MM（收到「2026-13」）」 |
| ⑨ | 结转后再试算 | **200**：`transferable=false`、`pending=0`、`transferred=23000` |

库内实况（探针结束前打印）：`object_no=CB-M504PROBE-A / transfer_seq=1 / finished_qty=800.0000 / unit_cost=28.750000 /
collected_*=15000,4000,4000,23000 / material_amount=15000 / total_amount=23000 /
remark='分批法完工结转：完工 800.0000 件，结算差额（累计归集 23000.0000 − 已结转 0.0000）'`；
清场后 `finish=0 objects=0 collects=0`。

### 真跑实例的两个前置（排障用，不是缺陷）

1. `sys_config.erp.cost.enabled` 必须为 `true`，否则所有 `/business/erp/cost*` 统一 403 + 66079。
2. `admin.must_change_password` 必须为 `false`，否则除改密 / 登出外一律 403。
3. 二进制必须在 `backend/` 目录下启动（迁移路径 `app/migrations` 是相对路径）。

## 四、12 条真库用例覆盖（`cost_batch_finish_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `preview_computes_full_transfer_without_writing` | 整批完工 ⇒ 本次 = 累计全额、单位成本 `27.888888`（22311.1104/800）、**预览不写库** |
| 2 | `finish_writes_one_row_and_replays_idempotently` | 写 1 行 + 三分量与累计快照落库 + 报文单位成本是**本次**的；重放 `inserted=false`、行数不变 |
| 3 | `second_transfer_only_moves_the_delta` | 结转后新成本 ⇒ 第 2 次只转差额（`transfer_seq=2`），累计 = 累计归集 |
| 4 | `wip_blocks_finish_and_writes_nothing` | 在制 > 0 ⇒ 409 + `WIP_NOT_ZERO` + `data.blockers`，一行不写；预览被拦但**照样给数**（15000） |
| 5 | `settled_item_blocks_finish` | 计算单已 `SETTLED` ⇒ 409 + `ALREADY_SETTLED`；`settled_in_books=true` |
| 6 | `frozen_transfer_period_blocks` | 结转期间 `DRAFT` 放行、`SUBMITTED` ⇒ 409 + `PERIOD_FROZEN` |
| 7 | `locked_period_blocks_finish` | 本批活动期间已 `SETTLED` ⇒ 409 + `PERIOD_LOCKED` |
| 8 | `draft_labor_and_uncollected_stock_out_block` | 草稿工时 + 已过账未归集领料 ⇒ 两条拦截；补齐（确认工时 + 补归集行）后放行且 **15100 全额转出** |
| 9 | `cancelled_batch_and_out_of_scope_are_rejected` | 作废批 409；别车间用户 404（试算与结转都是） |
| 10 | `zero_cost_and_zero_output_are_rejected` | 零成本完工 / 完工数 0 ⇒ 各自拦截码，一行不写 |
| 11 | `unique_index_blocks_two_rows_with_same_seq` | 绕过编排直插「同批同 seq」第二行 ⇒ 库层唯一键拦住（并报键名） |
| 12 | `zz_teardown_leaves_zero_residue` | 结转过再清场：结转单 / 归集 / 对象按 id 回查全 0（跑完实测五张表均 0） |

## 五、边界

- **新增一张表**（`biz_cost_batch_finish`）+ **一个新错误码**（`66107`，66xxx 段续号）；无新权限码、无新菜单、无前端改动
  ⇒ `menu_invariants.sh` 判据面不变（菜单总数 / 同父 sort / title 长度 / icon 均未动）。
- 未改 `biz_cost_object` / `biz_cost_collect` / `biz_cost_period*` 任何列与约束；未碰 settle / reverse / report / alloc 一行。
- 未写前端（M5-07 的活）；本卡只保证接口契约、写入口径与状态闸。
- 给 M5-05（在制批次余额）的接缝：`在制余额 = Σ biz_cost_collect(活行, 该对象) − Σ biz_cost_batch_finish(活行, 该对象)`，
  本卡 `cost_batch_finish_model::{accumulated_totals, transferred_totals}` 就是这两个原语。
- `cp_m504_dev` / `cp_m504_mig` 用完即 drop（证据里所有数字均来自真跑输出）。
