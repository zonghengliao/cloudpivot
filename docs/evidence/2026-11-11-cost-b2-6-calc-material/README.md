# COST-B2-6 calc 第 1 段：料归集 —— 证据与交付说明

- 卡：`t_a9416552`（COST-B2-6【成本】calc 第 1 段）
- 批次：B2 归集（ARCH-9 §8.1 B2-6）
- 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.3（归集时点与来源）、§3.1 ③（`biz_cost_collect` DDL）、
  §5.2 #27（`POST /cost-period/calc` 是唯一入口）
- 迁移：**本卡不新增迁移**（`biz_cost_collect` 在 V0371、`biz_stock_out.cost_object_id` 在 V0375，均已应用且未改一字）
- 代码：
  - `backend/src/business/erp/cost_collect_dto.rs`（纯逻辑：幂等键 / 行校验 / 增量计划 / 段落出参）
  - `backend/src/business/erp/cost_collect_model.rs`（`biz_cost_collect` 数据层：候选扫描 / 已归集行 / 幂等写入 / 软删）
  - `backend/src/business/erp/cost_calc_material.rs`（段 1 编排：一次事务内 扫描 → 清过期 → 写新 → 回查）
  - `backend/src/business/erp/cost_calc_material_db_tests.rs`（6 条真库用例，`ERP_DB_URL` 开关）
  - `backend/src/business/erp/mod.rs`（模块登记）、`backend/src/shared/error_code.rs`（新码 66064）

---

## 1. 交付了什么

### 1.1 为什么没有 HTTP 接口

设计 §5.2 的接口清单里只有 **#27 `POST /business/erp/cost-period/calc`** 一个计算入口。calc 是六段流水线
（料 → 工 → 费 → 对账 → 分摊 → 约当/计算单），本卡只落地**第 1 段**；单独暴露一个「只归集料」的接口会让期间
停在「料有了、工费没有」的半算状态，比不给接口更危险。所以本卡的交付面是**内部 API**（同 B2-3 费率取数的处理）：

```rust
// 段 1 入口（自己开事务、自己提交）
pub async fn collect_material(period_code: &str, operator: Option<Uuid>)
    -> Result<MaterialCollectOutcome, ApiError>;

// 给 calc 编排方复用的「事务内版本」（不提交，由调用方 commit/rollback）
pub async fn collect_material_in_tx(ex: &impl SqlExecutor, period_code: &str,
    start: NaiveDate, end: NaiveDate, operator: Option<Uuid>)
    -> Result<MaterialCollectOutcome, ApiError>;
```

### 1.2 取数口径（设计 §4.3 料行，逐条落进 SQL）

```sql
FROM biz_stock_out_item i
JOIN biz_stock_out h ON h.id = i.stock_out_id
LEFT JOIN biz_cost_object o ON o.id = h.cost_object_id
LEFT JOIN sys_dept d ON d.id = o.workshop_dept_id
WHERE h.deleted_at IS NULL
  AND h.status   = 'POSTED'          -- 未过账没有账上金额（草稿 amount 恒 0）
  AND h.out_type = 'ISSUE'           -- 报废/盘亏恒进 1901，不是料成本
  AND h.cost_object_id IS NOT NULL   -- 不挂对象 = 车间公用消耗，不进料成本
  AND h.out_date BETWEEN 期间首日 AND 期间末日
```

| 归集列 | 取值 | 依据 |
|---|---|---|
| `cost_object_id` | `biz_stock_out.cost_object_id` | §4.3 |
| `element` / `source_type` | `MATERIAL` / `STOCK_OUT`（代码常量，不接受外部传值） | §3.1 ③ CHECK 值域 |
| `source_id` | `biz_stock_out_item.id`（**幂等键第二段**） | §4.3 幂等键 |
| `line_no` | 恒 `1`（`source_id` 已唯一定位到行） | §4.3 幂等键 `(STOCK_OUT, item_id, 1)` |
| `source_no` | `biz_stock_out.out_no` 快照 | §3.1 ③ |
| `occur_date` | `biz_stock_out.out_date` | §4.3 口径说明 ② |
| `material_id` / `qty` | 明细列 | `ck_biz_cost_collect_mat` |
| `unit_price` | `biz_stock_out_item.unit_cost`（移动加权单价快照，`NUMERIC(18,6)`） | §3.1 ③ |
| `amount` | **`biz_stock_out_item.amount`（过账写入的账上真实扣减，不重算）** | §4.3 口径说明 ① |
| `dept_id` / `dept_name` | 成本对象的 `workshop_dept_id`；名取 `sys_dept` 实时名，落空回落对象上的车间名快照 | AGENTS 约定 13 |

**金额绝不按 `qty × 单价` 重算**：一张领料单可能跨多仓 / 多库位 / 多批号，`item.amount` 是逐桶扣减后的真实差额，
与 `qty × 单价` 在 4 位舍入下必然有分毫差（与 `voucher::create_for_stock_out` 同一条红线）。真库用例 ① 特意造了
`qty=3 / unit_cost=10 / amount=30.0001` 的行：归集到 `30.0001`，并断言 `amount != qty × 单价`。

### 1.3 幂等的两种语义（都实现）

| 语义 | 落点 | 说明 |
|---|---|---|
| **不重复写** | `uk_biz_cost_collect_src (source_type, source_id, line_no) WHERE deleted_at IS NULL` + `ON CONFLICT … DO NOTHING`，并先用纯函数 `dto::plan` 算好「该写几行」 | 第二次跑 `inserted=0 / skipped_existing=scanned`，行 id、金额、`updated_at` 一律不动 |
| **结果收敛** | 已有行若「来源已作废」或「对象 / 金额与来源不一致」⇒ 进 `stale`，**先软删再重写** | 少了这一半，作废后的旧归集行会永远留在期间里（C-COST-5 天天红），且改挂对象后金额停在旧对象上 |

> ⚠️ **顺序坑（本卡实测踩到）**：`uk_biz_cost_collect_src` 是 `WHERE deleted_at IS NULL` 的**部分**唯一索引。
> 同一来源行「改挂对象 / 金额被修正」时旧行还活着就占着键 —— 先 INSERT 会被 `ON CONFLICT DO NOTHING` 静默吞掉，
> 随后再删旧行 ⇒ 这一键上一行不剩。所以段内固定 **先清过期行、再写新行**（`cost_calc_material.rs` ④⑤）。

### 1.4 段落出参（前端「未归集清单」与排错都靠它）

```rust
MaterialCollectOutcome {
    period_code, scanned, inserted, skipped_existing, purged, duplicated,
    inserted_amount, purged_amount, zero_amount,
    collected_rows, collected_amount,      // 写后回查的真实值（C-COST-5 用这个数）
    purged_detail: Vec<PurgedRow>,         // 最多 50 条，超出只计数
}
```

### 1.5 错误码

新增 `66064 ERP_COST_COLLECT_SOURCE_INVALID`（400）：已过账 + 挂对象的领用单，其明细行**账面非法**
（数量 ≤ 0 / 金额 < 0 / 物料为空 / 小数位超列标度）。报文点名单号 + 原因，**整笔回滚，一行都不写**。
不复用 `10001`：用户没填参数，他需要的是「去修那张单」，前端给的是单号跳转而不是表单红框。

---

## 2. 实测输出

### 2.1 纯逻辑单测 —— `01_unit_tests.out`

```
cargo test --lib cost_collect      → 19 passed / 0 failed
cargo test --lib cost_calc_material →  8 passed / 0 failed（其中 6 条真库用例无 ERP_DB_URL 时 SKIP）
```

覆盖：幂等键稳定性 / 首次全写 / 重复跑全跳过 / 增量只补新行 / 来源消失→stale / **对象或金额变了→清旧写新** /
候选重复键收敛 / 零金额行归集并计数 / 6 类非法行校验 / 小数位 `normalize` 判定 / stale 排序确定性 /
常量与 DDL 值域一致 / 扫描 SQL 三条硬条件 / 金额取 `item.amount` 且不含 `qty *` / `ON CONFLICT` 与部分索引谓词逐字一致。

### 2.2 真库用例 —— `02_real_db_tests.out`

```
库：costb26_drill_1625844（空库首启自动迁移，167 张表；biz_cost_collect 21 列、biz_stock_out.cost_object_id 就位）
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/costb26_drill_1625844 \
  cargo test --lib cost_calc_material_db -- --test-threads=1
→ 6 passed / 0 failed（6.25s）
→ 跑完零残留：collect=0 / stock_out=0 / item=0 / object=0 / material=0 / warehouse=0
```

| # | 用例 | 断言要点 |
|---|---|---|
| 1 | `collects_only_posted_issue_with_cost_object` | 同一张已过账领用单两条明细 ⇒ 两行归集（一单多行各自独立幂等）；**六类不该扫的行一行都没写**：不挂对象 / `DRAFT` / `PENDING_POST` / `POSTED+SCRAP`（直插脏数据）/ 上期日期 / 单头已软删；逐列核对 `amount=30.0001`（≠ 3×10）、`unit_price=10.000000`、`occur_date`、`source_no`、`line_no=1`、`cost_object_id`、`dept_id`、`dept_name` |
| 2 | `rerun_does_not_duplicate_and_leaves_rows_untouched` | 第二次 `inserted=0 / skipped_existing=2 / purged=0`，行数与金额不翻倍，**行 id 与 `updated_at` 逐个不变** |
| 3 | `purges_rows_whose_source_no_longer_qualifies` | 领料单作废后重跑：`purged=1`、当期总额 50→20、旧行进软删（`deleted_at` 留痕、表里仍可见）、第三次跑收敛为无操作 |
| 4 | `relinked_cost_object_moves_the_collected_amount` | 改挂到另一个成本对象：旧行清掉、新行落到新对象，总额不变（证明幂等键是**来源明细 id**，不是「对象 + 金额」） |
| 5 | `zero_amount_lines_are_collected_and_counted` | 零成本存量领料照样归集（`amount=0`），`zero_amount=1` 单独计数（「成本为什么是 0」有答案） |
| 6 | `period_without_posted_issue_is_a_clean_noop` | 空期间一行不写不报错；非法期间在开事务之前 400 |

夹具纪律：每个用例独占一个**期间槽**（进程内递增槽位 → 年 2000+slot%80 / 月 1+(slot/80)%12），
跨次运行的残留按前缀（`B26OUT-` / `CB26-`）先清；清场按 id 删，FK 顺序 归集 → 明细 → 单头 → 对象 → 物料/仓库。

### 2.3 门禁 —— `04_gates.out`

```
cargo fmt -- --check                 → FMT_EXIT=0
cargo clippy --all-targets -D warnings → 0 warning / 0 error
cargo test（全仓）                    → 33 目标 4749 passed / 0 failed（TEST_EXIT=0）
```

> 首次全仓跑的唯一失败是 `testd::impexp::i14_tampered_import_file_rejected`：`uk_sys_import_task_no` 撞
> `IMP20261110-402C`（`cloudpivot_test` 库里 2026-11-10 22:35 留下的残留，任务号只有 4 位十六进制随机）。
> 与本卡无关（未碰 `impexp` 任何文件），单独重跑同一用例即通过 —— 详见 `03_pre_existing_flake.out`。

---

## 3. 卡面三条验收的落点

| 验收项 | 落点 | 证据 |
|---|---|---|
| 只扫已过账（POSTED）且挂了成本对象的领料 | `cost_collect_model::MATERIAL_SOURCE_SQL` 的三条硬条件 + 单测 `source_sql_keeps_the_three_hard_filters` | 真库用例 ①（六类不该扫的行全不写） |
| 重复跑不重复写（幂等键：来源单据 id） | `uk_biz_cost_collect_src` + `ON CONFLICT DO NOTHING` + `dto::plan` | 真库用例 ②（行 id / 金额 / `updated_at` 三重不变） |
| 金额取领料单实际出库成本 | `i.amount AS amount`（不重算），单测禁 `qty *` | 真库用例 ①（`30.0001` vs `3×10`）+ ⑤ |

---

## 4. 已知偏差 / 本卡决策

1. **无迁移**：表与列在 V0371 / V0375 已应用，本卡一字未改已应用迁移 ⇒ 卡面「迁移幂等 + `.down.sql`」对本卡 **N/A**。
2. **无前端**：本卡无 HTTP 入口（见 §1.1）⇒ `pnpm typecheck && build` 对本卡 N/A。
3. **幂等从「只插不删」扩到「清旧写新」**：设计 §4.3 只写「重算 = 清当期 + 重灌（只允许 DRAFT）」。本卡的两条实测
   （作废后重跑、改挂对象后重跑）说明「增量重跑」也必须收敛，否则期间里会留脏归集行；实现上用软删（留痕、且释放部分唯一键），
   并把 DRAFT 闸留在 calc 编排层（本段不判断期间状态 —— 它不掌握 `biz_cost_period` 的语义）。
4. **料行也写 `dept_id` / `dept_name`**：设计 §3.1 ③ 的列注释说 `dept_id` 是「费行归属车间」，但料行记上车间对
   车间成本报表有用，且不影响任何既有口径（池查询 `element='OVERHEAD'`、参与对象判据都不看它）。
5. **期间区间优先取 `biz_accounting_period`**（复用 `ledger::period_range`）：月结把期间拉长/缩短时也跟得上；
   期间行不存在才按自然月推导（设计 §4.3 口径说明 ② 只说「按 out_date 所属期间」）。
6. **新增错误码 `66064`**，已在 `error_code.rs` 的 66xxx 码值稳定性数组登记。
7. **`66064` 没有真库用例**：正常过账路径插不进「数量 ≤ 0 / 金额 < 0」的明细（`ck_biz_so_item_qty` / `ck_biz_so_item_cost`
   拦着），这些异常只能人工改库造出来。这一层是**防御性守卫**（历史脏数据 / 未来旁路写入），由纯函数单测
   `validation_rejects_broken_source_rows` 逐条钉住，真库侧不造脏数据来测。
8. **数据权限 unscoped**：设计 D-18 未登记 `biz_cost_collect`，本卡也没有列表接口（列表属「未归集清单」B2-12）。

## 5. 后续卡对齐（只报不改）

- **B2-7（工归集）/ B2-8（费归集）**：`cost_collect_model` 的读写函数都带 `element='MATERIAL' AND source_type='STOCK_OUT'`
  过滤（有意为之：各段互不踩）。请**加同族函数**（`insert_labor_line` / `collect_rows_of_period_by(...)`）而不是改本卡这两个，
  并且同样遵守「先清过期、再写新行」的顺序（部分唯一索引同理）。
- **calc 编排卡**（#27 `POST /cost-period/calc`）：重算可以在 DRAFT 下先 `DELETE … WHERE period_code = ?` 再逐段跑；
  本段对「已清库」和「不清库」两种形态都收敛（不清库也只会补写，不会翻倍）。
- **C-COST-5 巡检**：`Σ biz_cost_collect(MATERIAL)` 与 `Σ biz_stock_out_item(cost_object_id 非空, POSTED, 当期).amount`
  两侧口径一致（本卡的 `collected_amount` 就是前者），可以直接对。

## 6. 复跑命令

```bash
# 纯逻辑
cd backend && cargo test --lib cost_collect && cargo test --lib cost_calc_material

# 真库（仓库里没有现成的演练库时：新建空库 + 起一次实例自动迁移即可）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/costb26_drill_1625844 \
  cargo test --lib cost_calc_material_db -- --test-threads=1

# 门禁
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

## 7. 泄漏 / 环境

- 本机演练库 `costb26_drill_1625844` **保留**（跑完零残留，供后续卡复跑）；临时实例 :18029 已停。
- `cloudpivot_test` 里 `IMP20261110-402C` 那行残留**不是本卡产生的**（2026-11-10 22:35，早于本卡），未清理（不属本卡范围）。
