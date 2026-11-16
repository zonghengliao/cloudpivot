# M2-10 库龄：呆滞料标记（超 N 天无出库）—— 交付证据

卡：`t_bb0fe3c6`（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））
上游：`t_397dc77c`（M2-08 库龄计算，V0424 的库函数 `biz_inventory_aging_layers`）+
`t_4e2cb6f6`（M2-09 分段报表，V0425）
位置：`backend/app/src/business/erp/inventory_aging/slow_moving/`（子模块四件套）+ 迁移 `V0426`

## 1. 做了什么（一句话 + 边界）

按 **物料 × 仓库** 标记「库龄超过 N 天仍有剩余」的入库批次（呆滞料），给出呆滞数量 /
参考金额 / 占结存比例，接口 `GET /business/erp/inventory-aging/slow-moving`，
权限码 `business:erp:inventory-aging:slow-moving`（V0426），**阈值 N 可配置**。

* **没有第二份 FIFO**：呆滞只是 `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`
  （V0424 · M2-08）外面套一层「按 物料 × 仓库 条件聚合」—— 与上游 handoff 里写的下游契约一致。
* **「超 N 天无出库」按批次口径**：某入库层**还有剩余**且 `库龄 > N` 即为呆滞；
  不按「这一格最后一次出库距今多久」判定（那会把新到的货一起算老，或让一笔小额出库
  把 200 天前的老货洗白）。理由写在 `slow_moving/dto.rs` 模块头注释与响应 `hints` 里。
* **阈值三级**：请求参数 `days` > 系统参数 `erp.inventory.slow-moving-days`（V0426 种 90）>
  常量 90；来源在 `threshold_source` 回显（`QUERY`/`CONFIG`/`DEFAULT`），配置值坏了回缺省
  **并进 `hints` 点名**（不静默）。
* **合计只有一条路径**：SQL 条件聚合 → 行 = 该格「全部层 / 呆滞层」两组数 → `summary` = Σ格；
  行上限 2000 **在汇总与行数统计之后**才截（`rows_truncated` + `hints` 明写）。
* **本卡不夹带**：库龄单测扩充（M2-11）、前端页（M2-12）、导出、呆滞处置动作都没做。

## 2. 接口契约

`GET /business/erp/inventory-aging/slow-moving?as_of=&material_id=&warehouse_id=&days=`
（权限码 `business:erp:inventory-aging:slow-moving`；`SUPER_ADMIN` 通配短路）

| 层 | 字段 | 说明 |
|:--|:--|:--|
| 表头 | `report_code` / `report_name` / `as_of` / `as_of_source` | `INVENTORY_SLOW_MOVING` / `呆滞料清单`；`as_of_source = QUERY`（传了）/ `TODAY`（留空取 CST 今天） |
| 阈值 | `threshold_days` / `threshold_source` / `threshold_config_key` | 生效阈值、来源（`QUERY`/`CONFIG`/`DEFAULT`）、参数键（前端「去改配置」的锚点） |
| 汇总 | `summary.scanned_row_count` / `slow_row_count` / `layer_count` / `slow_layer_count` / `qty_on_hand` / `qty_slow` / `amount_on_hand` / `amount_slow` / `slow_ratio` | **截断前的全量**；`scanned_row_count` 是「呆滞面」的分母（全库扫过多少格） |
| 统计 | `row_count` / `row_limit` / `rows_truncated` | `row_count` = 呆滞格总数（截断前） |
| 行 | `rows[]` | `material_*` / `warehouse_*` / `unit_name` / `layer_count` / `qty_on_hand` / `amount_on_hand` / `slow_layer_count` / `qty_slow` / `amount_slow` / `slow_ratio` / `oldest_inbound_date` / `max_age_days` |
| 提示 | `hints[]` | 判定口径 / 阈值来源 / 参考金额 / 单位混合 / 口径互证 / 空结果 / 截断，纯文本 |

* 只返回 `qty_slow > 0` 的格（这就是「标记」的结果集）；不呆滞的格不丢，进了 `summary`。
* 行序：**呆滞金额降序** → 呆滞数量降序 → 最老日期升序 → 物料 id → 仓库 id（全序，刷新不换行）。
* 错误口径与 ERP 其余查询接口一致：非法 UUID / 日期 / 阈值（非整数、`<1`、`>3650`）→
  **400 + 66006**，文案点名字段（**不静默夹取**）。

## 3. 证据清单

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `01_cargo_test.out` | `ERP_DB_URL=…cargo test --lib business::erp::inventory_aging::`（真库 `cp_m210_slow_test`） | **86 passed / 0 failed，0 SKIP**（M2-08 的 36 + M2-09 的 26 + 本卡 24（16 纯逻辑 + 8 真库）） |
| `02_migration_drill.out` | `bash scripts/verify/m210_inventory_slow_moving_drill.sh cp_m210_slow_test` | **PASS=25 FAIL=0 / ALL PASS** |
| `03_api_check.out` | `python3 docs/evidence/…/m210_api_check.py`（隔离实例 `127.0.0.1:18087`，profile=test） | **PASS=80 FAIL=0 / ALL PASS** |
| `04_api_response_sample.json` | 一次真实响应（物料乙 × 仓乙，N=90） | 原样落盘 |
| `05_gates.out` | `cargo fmt -- --check` / `cargo fmt -p cloudpivot -- --check` / clippy（`--lib` 与 `--lib --all-targets`，均 `-D warnings`） | fmt rc=0；clippy **0 warning** |
| `06_scope_perm_menu.out` | `menu_invariants` / `perm_menu_gate`（含**反证**）/ `scope_gate` / `cargo test system::menu` | 8 条全绿（菜单 519）；`missing_in_menu=0`；反证只缺本卡 1 个码；scope 0 处；menu 22 passed |
| `m210_api_check.py` | 上面 03 的断言脚本（可重跑） | 80 条逐条 PASS/FAIL |

本卡**零前端改动**（前端页是 M2-12），所以没跑 `pnpm typecheck/build`。

## 4. 真接口证据摘要（03）

* 200 + JSON + `code=0`；无令牌 → **401**；持明细码但**没有**呆滞码的令牌 → **403**（权限闸真的挂着）
* 表头：`INVENTORY_SLOW_MOVING` / `呆滞料清单` / `as_of=2026-11-16` / `QUERY` /
  `threshold_days=90`、`threshold_source=CONFIG`（V0426 种子 90）、参数键 `erp.inventory.slow-moving-days`
* 物料乙 × 仓乙（70@07-01 / 30@09-01 / 20@10-20，无出库）：结存 **120**、呆滞 **70**、
  呆滞层数 **1** / 总层数 **3**、呆滞金额 **140**（70×2）、占比 **0.5833**、最大库龄 **138**、
  最老 `2026-07-01`，物料/仓库编码名称与单位全部补齐
* 物料甲 × 仓甲（100@06-01 被吃满 / 200@09-20 ×20 / 50@11-05，出库 120@11-10）：
  N=90 **不进行**（最老剩货 57 天）；N=50 → 呆滞 **180** / 结存 230 / 占比 **0.7826**（来源 `QUERY`）
* 边界（物料丙 × 仓甲：10@2026-08-18 = 90 天整 / 20@2026-08-17 = 91 天）：
  N=90 → 呆滞 **20**（90 天整不算）；N=89 → **30**；N=91 → 该格 **0 呆滞行但仍被扫过**（scanned=1）
* 汇总口径（单物料）：scanned=1 / slow=1 / 层 3 / 呆滞层 1 / Σ结存 120 / Σ呆滞 70 / Σ金额 140 / 占比 0.5833
* 不变量（全量）：逐行 `0 < 呆滞 ≤ 结存`、`呆滞层数 ≤ 层数`、`0 < 占比 ≤ 1`；
  `Σ行呆滞 = summary.qty_slow`；`slow_row_count == row_count == len(rows)`；`scanned ≥ len(rows)`
* **口径互证**：同一格 N=90 的 `qty_slow`（70）== 库龄报表 `OVER_90` 档数量（70）
* 非法参数 5 组（`days=abc` / `days=0` / `days=99999` / `as_of=2026-13-01` / `material_id=not-a-uuid`）
  → 400 + 66006 且文案点名字段
* `as_of` 留空 → `as_of_source=TODAY`、`as_of=2026-11-16`（CST 今天）
* 响应无 `page`/`page_size`；`row_limit=2000`、`rows_truncated=false`

## 5. 迁移 V0426（只追加、幂等、不碰业务数据）

`V0426__inventory_slow_moving.up.sql` / `.down.sql`：

* ① 前置护栏：底座三张表在、V0424 的库龄函数必须在（呆滞没有第二个数据源）、
  「即时库存」页唯一、码未被占用；
* ② 阈值参数 `sys_config.erp.inventory.slow-moving-days = '90'`（`NUMBER`，`1..3650`；
  已存在则不覆盖 —— 重放不吃掉运维改过的值）；
* ③ 权限点 `business:erp:inventory-aging:slow-moving`（`menu_type=3`，挂「即时库存」页，
  `sort_order=60`，`title=呆滞料标记`，`is_visible=false`，`path=NULL`）；
* ④ 授权：跟**持库龄报表码**的角色走 + 超管（数据驱动，重放不增长）；
* ⑤ 自检 8 条 + **行为探针**：按呆滞真实形状（库函数 → `age_days` → `is_slow` → 条件聚合）
  用**参数里的阈值**在库层执行一次（`EXECUTE format(... %s ...)`），空库 0 行也算过；
* down：只删本迁移的授权行 / 菜单行 / 参数行（按 `description` 前缀认领），
  **保住 V0424 的函数与 V0425 的报表菜单**（演练 ③ 专门验这条），并在 down 里再自检一次。

演练 25 条断言覆盖：前置归零 → up → up（幂等）→ down → down（幂等）→ up（恢复）、
菜单字段 / 父子 / 同父 sort、授权闭包缺口 0、阈值参数可用、业务数据行数不变、
菜单总数 = 基线 + 1、聚合探针可执行。

## 6. 决策记录（为什么这么做）

| 决策 | 理由 |
|:--|:--|
| 呆滞挂 `inventory_aging/slow_moving/` 子模块（四件套），不塞进 M2-08/M2-09 的文件 | 三个视图、三个权限码、三个问题（明细 / 分布 / 处置清单）；但算法仍只有库函数一份 |
| 新开 `:slow-moving` 码，不复用 `:list` / `:report` | 能看分布 ≠ 该看处置清单（催销 / 减值）；单开才能在授权树上单独开关 |
| 判定按**批次库龄**而不是「整格最后出库日」 | 后者会把新到的货一起判老、也会让一笔小额出库把老货洗白（详见 `dto.rs` 头注释） |
| 边界用 `库龄 > N`（严格大于） | 与库龄报表「90 天以上 = 91 起」同一把尺 ⇒ **N=90 时与报表 OVER_90 档逐格相等**，两张表可对拍 |
| 阈值放 `sys_config`，不新开写接口 | 一个整数不值得一套 CRUD；参数页已有审计 + 快照刷新（与 `erp.fx.gain_loss_account` 同一取舍），出参给 `threshold_config_key` 当跳转锚点 |
| 阈值参数**只出现一次**（`flagged` CTE 的 `is_slow` 布尔列） | 逐列各写一遍 `FILTER (WHERE age_days > ?)` 会让阈值出现 4 次，漏改一处 = 「层数是呆滞的、金额不是」且无人报警 |
| 先汇总再截断 | 先截再汇总 = 「扫过 2000 格、呆滞 2000 格」的假清单 |
| 行序按**呆滞金额**降序 | 报表回答「压了多久」，呆滞料回答「压了多少钱」——处置动作先看钱多的 |
| 不呆滞的格不返回但进 `summary` | 「标记」的结果集只该有呆滞格；但「8 格呆滞」必须有分母（8/10 还是 8/8000） |
| 拆三个 CTE（`layers` → `aged` → `flagged`） | `age_days` 要穿过布尔判断与最终 `MAX`，且阈值只能比一次 |

## 7. 已知口径（明写，不假装）

1. **没有层表**：呆滞与库龄层一样是每次查询推导（V0424 的窗口函数）；千万级流水需要预聚合。
2. **业务日期兜底档**：`MATERIAL_SERIAL` / `SERIAL_BATCH` 无业务日期列，回落 `created_at`(CST)。
3. **没有入库流水的库存不计入呆滞**（直接写 `biz_inventory` 的行看不见）。
4. **金额是参考金额**：`Σ(呆滞层剩余 × 层入库单价)`，与移动加权账面金额本来就不相等。
5. **不提供「最近一次出库日期」列**：`biz_inventory_txn` 的**业务日期**推导逻辑住在 V0424 的
   库函数里（多表 CASE），在报告侧再写一遍就是第二份口径；真要看「最后出库日」应另开一张卡
   （在库层给一个 `biz_inventory_txn_dated` 视图/函数）。
6. **跨物料 `qty_slow` 单位混合**（件 / 公斤 / 米），只作总量级参考；跨物料可比的是 `slow_ratio`。
7. **真接口验收跑在 `cloudpivot_test` 克隆 + 夹具上**（`cp_m210_api_test`，loader 把迁移补到 V0426），
   不是 144 生产数据。

## 8. 复现步骤

```bash
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
# ① 迁移演练库 + 真库用例库（99 上跑，PG 在 145）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m210_slow_test
psql -h 172.18.10.145 -U cloudpivot -d cp_m210_slow_test -v ON_ERROR_STOP=1 \
  -f backend/app/migrations/business/V0424__inventory_aging_calc.up.sql
psql -h 172.18.10.145 -U cloudpivot -d cp_m210_slow_test -v ON_ERROR_STOP=1 \
  -f backend/app/migrations/business/V0425__inventory_aging_report.up.sql
psql -h 172.18.10.145 -U cloudpivot -d cp_m210_slow_test -v ON_ERROR_STOP=1 \
  -f backend/app/migrations/business/V0426__inventory_slow_moving.up.sql

# ② 迁移幂等 / 回滚（25 条断言）
bash scripts/verify/m210_inventory_slow_moving_drill.sh cp_m210_slow_test

# ③ 真库用例（86 条：M2-08 的 36 + M2-09 的 26 + 本卡 24）
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m210_slow_test" \
  cargo test --lib business::erp::inventory_aging -- --test-threads=1

# ④ 真接口：另起隔离实例（loader 自动补迁移到 V0426）→ 灌夹具 → 自签 token → 跑断言
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m210_api_test
bash /root/.hermes/profiles/backend-developer/cache/scratch/m210inst/run.sh   # 127.0.0.1:18087
psql -h 172.18.10.145 -U cloudpivot -d cp_m210_api_test -v ON_ERROR_STOP=1 \
  -f scripts/verify/m210_inventory_slow_moving_fixture.sql
python3 docs/evidence/2026-11-16-m210-slow-moving/m210_api_check.py

# ⑤ 清场
dropdb -h 172.18.10.145 -U cloudpivot cp_m210_api_test
dropdb -h 172.18.10.145 -U cloudpivot cp_m210_slow_test
```
