# M4-10 框架协议：单测（含超限反例）—— 原始证据

- 卡：`t_4da4c498`（M4-10【M4·采购】框架协议：单测 · 含超限反例）
- 上游：M4-07 表结构（`6019e4e5`）→ M4-08 订单引用（`56138ebf`）→ M4-09 执行量统计（`a5aeb295`）
- 本卡范围：**只做单测**。零前端、零迁移、零权限码、零路由（协议 CRUD 计划里没有那张卡，不夹带）。

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `backend/app/src/business/erp/purchase/framework_agreement_over_limit_tests.rs` | **新，778 行 / 12 条真库用例**：超限反例 + 边界 + 判据来源 |
| `backend/app/src/business/erp/purchase/mod.rs` | +4 行注册（`#[cfg(test)] mod framework_agreement_over_limit_tests;`） |
| `scripts/verify/m410_framework_agreement_unit_drill.sh` | 新，218 行：演练（假绿对照 → 真库 → 变异反证 → 门禁），30 条断言 |
| `scripts/verify/m410_mutations.py` | 新，121 行：4 个变异（文本替换 + 备份 + 恢复，锚点必须唯一） |
| 本目录 | 原始输出：不连库 / 真库 / 4 个变异 / 4 次恢复 / fmt / clippy |

## 2. 为什么只写这 12 条（不重复劳动）

M4-07/08/09 三卡已各带真库用例（12 + 14 + 12 = 38 条）与纯逻辑用例（model 8 + ref 5 + execution_dto 9
+ execution_model 3 = 25 条）。本卡**不抄第二遍**，只补它们没覆盖的**拒绝分支**，也就是卡面点名的
「超限反例」，外加超限判定里最容易写错的三件事：绕限、判据来源、边界。

| # | 用例 | 钉住的口径 |
|:--:|---|---|
| 1 | `split_lines_on_same_material_cannot_bypass_cap` | 同一物料拆两行各 60（单行都不超 100）合计 120 → **409/66098**，报文点名「本单该物料合计」；改成 60+40=100 放行 |
| 2 | `per_material_cap_rejects_fractional_overrun` | 已满 100 再下 **0.0001** 也拒（`>` 不是 `>=`），报文「剩余 0」 |
| 3 | `unlimited_material_is_not_capped` | `max_qty IS NULL` = 不限：123456 照下，回写如实（不限量 ≠ 不统计） |
| 4 | `stale_booked_column_does_not_open_the_gate` | 回写列被清零（漏刷）仍按**现算 100** 拦住，报文「已用 100」 |
| 5 | `inflated_booked_column_does_not_false_reject` | 回写列虚高 999999 不误拒，下单后重算**自愈**回 100（不是 999999+40） |
| 6 | `soft_deleted_agreement_is_rejected` | 协议软删 / 协议不存在 → **400/66096**「不存在或已删除」 |
| 7 | `currency_mismatch_is_rejected` | 协议价 USD ↔ 本单 CNY → **400/66096**，报文点出币种 |
| 8 | `tax_inclusive_total_amount_cap_counts_with_tax` | 含税协议的总额上限按**价税合计**比：10×10（不含税 100 / 价税合计 113）超 100 → 409；8 件 90.4 放行（用不含税比就会放 100，等于白送 13%） |
| 9 | `update_does_not_double_count_own_usage` | 改单排除本单旧明细（100 原样改回不许算成 200 误拒）；改到 120 照拒且额度保持 100 |
| 10 | `head_total_qty_cap_allows_exact_boundary` | 单头总量 30+20=50 恰等于上限放行，+1 拒绝 |
| 11 | `head_total_amount_cap_allows_exact_boundary` | 单头总额 10×10=100 恰等于上限放行，再下 20 拒绝 |
| 12 | `zz_teardown_purges_fixtures` | 清场：`M410-` 协议 / 采购单 0 残留 |

夹具：协议两表**仍无 CRUD 接口**，夹具走裸 SQL（号段 `FWA-19990101-9410xx`，严格 6 位流水）；
采购单走真实入口 `purchase_handler::create_purchase_order` / `update_purchase_order`；
主数据一律 `test_seed::ensure_*`（物料用 `ensure_material_base_unit`，否则被 `unit_guard` 66030 拦）。

## 3. 验收结果（本目录原始输出）

```
不连库：passed=75 failed=0  本卡标记=0   （假绿对照：passed 数与连库完全一样）
真  库：passed=75 failed=0  SKIP=0  本卡标记=12
整组 75 = 真库 50（12 表结构 + 14 引用 + 12 执行量 + 12 本卡）+ 纯逻辑 25
断言：PASS=30 FAIL=0 → RESULT: PASS
```

- `01_module_without_db.out`：不设 `ERP_DB_URL` 时整组仍报 `75 passed` —— **passed 数不能用来判真假绿**，
  真库用例只是 `ensure_db()` 提前 return；判据是「本卡正向标记 12 条」与 `SKIP=0`。
- `02_module_with_db.out`：`ERP_DB_URL` 指向从 `cloudpivot_dev` 克隆的 `cp_m410_ut_drill`，`SKIP=0`。
- `04_mutation_*.out` / `05_restored_*.out`：变异反证（见下）。
- `06_fmt.out`（空）/ `07_clippy_lib.out` / `08_clippy_all_targets.out`：门禁全绿、0 warning。

## 4. 变异反证（证明用例有牙）

| 变异 | 改错什么 | 期望转红的用例 | 实测 |
|---|---|---|---|
| A | 本单用量按**逐行**而不是按协议行聚合 | `split_lines_on_same_material_cannot_bypass_cap` | rc=101，该例 FAILED；恢复后 12/12 绿 |
| B | 明细上限 + 单头总量上限 `>` → `>=`（恰等于也拒） | `head_total_qty_cap_allows_exact_boundary` 等 6 条 | rc=101，6 条 FAILED；恢复后 12/12 绿 |
| C | 金额上限**恒按不含税**比（含税协议白送一个税率） | `tax_inclusive_total_amount_cap_counts_with_tax` | rc=101，该例 FAILED；恢复后 12/12 绿 |
| D | 单头总额上限 `>` → `>=` | `head_total_amount_cap_allows_exact_boundary` | rc=101，该例 FAILED；恢复后 12/12 绿 |

收尾断言：`git diff --quiet backend/app/src/business/erp/purchase/framework_agreement_ref.rs`
（生产代码与 HEAD 逐字一致），实测 PASS —— 变异没有留在工作区。

## 5. 本次踩到的坑（已回写 skill `references/framework-agreement-cards.md` §12）

1. **模板库 `cloudpivot_test` 落后到 `V0357`**（`schema_migrations` 里 `max(version::int)`），没有
   `biz_framework_agreement` → 克隆它跑真库用例第一条就 `42P01`。`createdb -T cloudpivot_dev` 也不行
   （145 上 dev 库有 10 个常连，报 `source database is being accessed by other users`）。
   可行做法：`pg_dump --no-owner cloudpivot_dev | psql 新库`（54 MB，几十秒）。
   演练脚本开头先断言源库**协议两表 + `biz_purchase_order.framework_agreement_id` + 两个执行量视图**在位，
   缺了就 `exit 2`（环境问题），不让它退化成「全部 SKIP 但 passed 照报」。
2. **`--nocapture` 下 `... ok` 消失**：用例里的 `println!` 标记会和 test 行挤在一行
   （`test path ... [M4-10] … ✓`，下一行才是 `ok`）→ 「逐条通过」不能 grep ` ok$`，要数
   `^test <模块>::` 行数 + 单独数 `FAILED` 行。
3. **变异窗口越短越好**：`/opt/cloudpivot` 是多 worker 共享工作区，变异期间文件是错的；
   脚本用「先备份 → 替换 → 跑 → restore → `git diff --quiet` 断言」，一轮约 2 分钟。
4. `ck_biz_po_item_tax` 要求三列自洽（`with_tax_amount = amount + tax_amount`）—— 本卡沿用真实入口，
   没有再手写采购明细探针（M4-09 的坑）。

## 6. 复跑方法

```bash
# 99 开发机（连 145 的 PG；脚本自己建/删 cp_m410_ut_drill）
bash scripts/verify/m410_framework_agreement_unit_drill.sh        # 约 13 分钟（含 4 个变异重编译）
DBHOST=127.0.0.1 bash scripts/verify/m410_framework_agreement_unit_drill.sh   # 145 本机

# 只跑本卡用例（自备库）
cd backend && ERP_DB_URL="postgres://…/库" \
  cargo test --lib business::erp::purchase::framework_agreement_over_limit -- --test-threads=1 --nocapture
```

## 7. 本卡没做什么（避免被当漏项）

- **零前端**：框架协议页面属 M4-11（`t_da2f0a9b`，本卡的子卡，完成后自动放行）。
- **零迁移 / 零权限码 / 零路由 / 零菜单**：单测卡不改运行期行为。
- **不写协议 CRUD 的单测**：CRUD 接口后端尚不存在（计划里没有那张卡），没有可测的入口；
  等它落地时按本卡 + M4-05 的模板补状态守卫矩阵即可。
