# M4-01 采购计划表结构（年度 / 月度，物料 × 仓库）· 证据包

- 卡：`t_c5ecacf7`（M4-01【M4·采购】采购计划：表结构（年度/月度，物料 × 仓库））
- 分支/提交：`main`（提交号见卡内汇报）
- 本卡范围：**只交付表结构**。CRUD / 审批（M4-02）、计划→申请（M4-03）、执行率（M4-04）、
  单测（M4-05）、前端（M4-06）都不在本卡。

## 1. 交付物

| 文件 | 说明 |
|:--|:--|
| `backend/app/migrations/business/V0446__purchase_plan.up.sql` | 两张表 + 取号行 + 8 索引 + 末尾 DO 自检（406 行） |
| `backend/app/migrations/business/V0446__purchase_plan.down.sql` | 回滚：**有数据就拒绝**，再删表与取号行 |
| `backend/app/src/business/erp/purchase/purchase_plan_model.rs` | 表名/列/枚举常量 + 行结构 + 取号 `next_plan_no` + 两个读入口 + 合计重算 |
| `backend/app/src/business/erp/purchase/purchase_plan_db_tests.rs` | 真库用例 11 条（约束行为，逐条对应卡面口径） |
| `backend/app/src/business/erp/purchase/mod.rs` | 注册上面两个模块（+10 行） |
| `scripts/verify/m401_purchase_plan_schema_drill.sh` | 演练脚本：克隆库 → 推到 V0445 → 上 V0446 两遍 → 结构/约束/范围/回滚/用例/门禁/残留 |

## 2. 表设计（决定与理由）

`biz_purchase_plan`（单头，22 列）· `biz_purchase_plan_item`（明细，13 列）

| 卡面原话 | 落点 | 理由 |
|:--|:--|:--|
| 年度 / 月度 | `plan_type`(ANNUAL/MONTHLY) + `plan_year` + `plan_month`，`ck_biz_pp_period` 绑死三者 | 不造 `period_code` 冗余列：两套期间格式拼一张表迟早出现「类型说 ANNUAL、期间写 2026-03」 |
| 物料 × 仓库 | 明细唯一索引 `uk_biz_pp_item_cell (plan_id, material_id, warehouse_id)` | 卡面粒度语；同计划同物料换仓 = 两行（允许），同物料同仓两行 = 计划量被灌两遍（拒） |
| 计划量 / 已执行量 | 明细 `planned_qty`(>0) / `executed_qty`(≥0)，单头 `total_*` 三个派生合计 | 执行量**刻意不拦超计划**：超计划执行现实里有，拦在库层等于逼用户改计划才能下单，露头应在 M4-04 报表（执行率 >100%） |
| 期间唯一 | `uk_biz_purchase_plan_period (plan_type, plan_year, COALESCE(plan_month,0), COALESCE(dept_id,零 UUID))` WHERE 未软删 | 同期间两张计划会把执行率各算一遍（与 V0318 销售目标同构的坑）；软删后可重建 |
| 金额（卡面未写） | 明细 `planned_price` / `planned_amount`，单头 `total_planned_amount` | M4-03「计划 → 采购申请」要用预估单价预填 `est_price`，没这列转换只能填 0，预算与比价全失真 |
| 状态 | 只有 `DRAFT / SUBMITTED / APPROVED`；`SUBMITTED` 必有审批实例 | **不造 CLOSED/CANCELLED**：卡面没有关闭动作，造了就是没有入口的死状态（与 V0331 同一口径）。真要归档时**追加**迁移扩 CHECK |
| 年度→月度分解 | `parent_plan_id` 自引用（`ck_biz_pp_parent` 只拦自指） | 「父必须是年度、年月落在父年内」是跨行语义，CHECK 做不到 —— 明确写进注释，由 M4-02 的 handler 判，别以为库层兜住了 |

**数据权限**：与采购申请单同口径 **SCOPE-EXEMPT**（计划是公司级滚动文件，`dept_id` 是「编制部门」快照，
不是数据范围列），不登记 `SCOPED_TABLES` —— 可见性交给 M4-02 的权限码。

## 3. 一个真实修掉的坑（3VL）

第一版 `ck_biz_pp_period` 写成
`(ANNUAL AND month IS NULL) OR (MONTHLY AND month BETWEEN 1 AND 12)`。
`MONTHLY` + `month = NULL` 时第二支求值成 NULL，`FALSE OR NULL = NULL`，而 **CHECK 只拒 FALSE**
⇒ 「月度计划没有月份」能插进去。真库用例 `annual_plan_has_no_month_monthly_plan_needs_one`
第一次跑就红了，改成 `MONTHLY AND month IS NOT NULL AND month BETWEEN 1 AND 12`。
演练脚本第 3 节把这条单独钉死（`月度计划没有月份被拒（3VL 陷阱已修…）`）。

## 4. 验收（原始输出在编号文件里）

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `01_drill_full.out` | 完整演练（含空库全链重放） | **PASS=47 FAIL=0，rc=0** |
| `02_teeth_skip_guard.out` | 有牙自检：`M401_BAD_URL=1`（连接串故意写错） | **FAIL=2，rc=1**（SKIP>0 + 缺正向标记双判据生效） |
| `03_schema_dump.txt` | 两张表的 `\d+`、索引、约束定义（实库导出） | 158 行 |
| `04_scope_no_routes_menus_frontend.txt` | 范围守卫：路由 0 / 权限码 0 / 菜单语句 0 / 前端 0 | 全部 0 |
| `05_gates.out` | `cargo fmt --check` / `clippy --lib` / `clippy --lib --tests` | 全绿，0 告警 |

演练脚本覆盖的 47 条断言摘要：

1. 模板可克隆 → 41 条 V0405…V0445 迁移推到 V0445（**上 V0446 前计划表确实不存在**）
2. V0446 连跑两遍无 ERROR（幂等）；上迁移前后菜单总数不变（**本卡一条菜单都没加**）
3. 结构：2 表 / 22+13 列 / 外键 CASCADE / 8 索引 / 两个部分唯一索引 / 取号行 PPL 1 行
4. 约束行为（12 条）：年度带月、月度无月、月度 13 月、CLOSED 死状态、SUBMITTED 无实例、
   坏单号、同计划同物料同仓重复、计划量 0、负执行量 —— **全部被库层拒**；
   正样本（月度 + 同物料换仓两行）能插、执行量 150 > 计划 100 能写
5. 空库全链重放 `mig_replay_fresh.sh`：**366/366 条迁移按版本号升序全过**（V0446 不依赖更晚的迁移、没撞号）
6. 回滚三段：有数据 → 拒；删计划（明细随 CASCADE 归零）→ down 成功；再 up → 复原
7. Rust 用例 15 条（4 纯逻辑 + 11 真库），SKIP=0，正向标记 `[M4-01]` 在位
8. 门禁：`permission_audit_tests` **8/8**、`menu_invariants.sh` **8 条全绿**、菜单总数 540（未顶破）
9. 夹具残留：`M401-` 计划与明细 **0 条**

复跑：

```bash
bash scripts/verify/m401_purchase_plan_schema_drill.sh                  # 99 → 145（默认）
PG_HOST=127.0.0.1 bash scripts/verify/m401_purchase_plan_schema_drill.sh  # 在 145 上跑
SKIP_REPLAY=1 bash scripts/verify/m401_purchase_plan_schema_drill.sh    # 跳过 2 分钟的重放
M401_BAD_URL=1 bash scripts/verify/m401_purchase_plan_schema_drill.sh   # 期望报红（有牙自检）
```

## 5. 给 M4-02（CRUD + 审批）的交接

1. **model 已就位**：`purchase_plan_model::{PLAN_TABLE, ITEM_TABLE, SEQ_TYPE, PLAN_TYPE_*, STATUS_*,
   PLAN_TYPES, STATUSES, PlanRow, PlanItemRow, next_plan_no, find_plan, find_plan_by_period,
   items_of_plan, recalc_totals}` —— 建单不用再猜列名。建单/改明细/执行量回写后**必须**调
   `recalc_totals`（单头三个 `total_*` 是派生列，客户端不许传）。
2. **取号**：`biz_seq(seq_type='PPL')` 已由迁移种下，`next_plan_no(now)` 按 CST 日滚动。
3. **判重**：新建前用 `find_plan_by_period` 探「类型 + 年度 + 月 + 部门」，比撞唯一索引报 500 友好。
4. **库层已知不兜的两件事**（要在 handler 里判）：① 月度的 `parent_plan_id` 必须指向年度计划且
   年月落在父年内；② 执行量回写不许把 `executed_qty` 写成负数（库层只兜 ≥ 0，其它不变式归业务）。
5. **菜单顶格**：`menu_invariants` 的菜单总数区间是 400–540，本卡实测 **540/540 正好顶格**。
   M4-02/M4-06 落「采购计划」页面 + 按钮前，必须先由治理侧把上限口径 / 存量菜单收敛定下来，
   否则加第一条菜单门禁即红（这不是本卡能单方面改的）。
6. **权限码按约定 14 拆**：页面显形码 `business:erp:purchase-plan:page` 只给前端，
   全量列表码 `…:list` 挂按钮行，自助视图走 `auth_only` 接口。
