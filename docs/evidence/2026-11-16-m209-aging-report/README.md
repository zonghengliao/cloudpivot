# M2-09 库龄分析：报表（0-30/31-60/61-90/90+）—— 交付证据

卡：`t_4e2cb6f6`（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））
上游：`t_397dc77c`（M2-08 库龄计算，V0424 的库函数 `biz_inventory_aging_layers`）
位置：`backend/app/src/business/erp/inventory_aging/report/`（子模块四件套）+ 迁移 `V0425`

## 1. 做了什么（一句话 + 边界）

按 **物料 × 仓库** 汇总 0-30 / 31-60 / 61-90 / 90 天以上 四档的库龄结构与参考金额，
接口 `GET /business/erp/inventory-aging/report`，权限码 `business:erp:inventory-aging:report`（V0425）。

* **没有第二份 FIFO**：报表只是 `biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)`
  （V0424 · M2-08）外面套一层 `GROUP BY` —— 上游 handoff 里写死的下游契约。
* **档位表只有一张**：`report::dto::LAYER_BUCKETS`（4 档），`CASE` 由
  `payable_aging::bucket_case_sql_in` 生成、Rust 侧分档用 `bucket_of_in`，改阈值只改这张表。
  （不复用应付 / 应收的 `AGING_BUCKETS`：那张表语义是「到期后拖了多久」7 档，套过来会把
  「未到期 / 365 天以上」挂到库龄报表上。）
* **合计只有一条加法路径**：分档在 SQL、行合计 = Σ本行各档、全表合计 = Σ各行；
  行上限 2000 **在合计之后**才截（`rows_truncated` + `hints` 明写，绝不静默少行）。
* **本卡不夹带**：呆滞料（M2-10）、单测扩充（M2-11）、前端页（M2-12）、导出 xlsx 都没做。

## 2. 接口契约

`GET /business/erp/inventory-aging/report?as_of=&material_id=&warehouse_id=`
（权限码 `business:erp:inventory-aging:report`；`SUPER_ADMIN` 通配短路）

| 层 | 字段 | 说明 |
|:--|:--|:--|
| 表头 | `report_code` / `report_name` / `as_of` / `as_of_source` | `as_of_source = QUERY`（传了）/ `TODAY`（留空取 CST 今天） |
| 表头 | `buckets[]` | `key` / `label` / `min_days` / `max_days`（前端按它出列，不写死 4 列） |
| 合计 | `summary[]` | 四档 `layer_count` / `qty` / `amount`（全量，不受行截断影响） |
| 合计 | `qty_total` / `amount_total` / `layer_count` | = Σ`summary` |
| 统计 | `row_count` / `material_count` / `warehouse_count` / `row_limit` / `rows_truncated` | `row_count` 是截断前总数 |
| 行 | `rows[]` | `material_*` / `warehouse_*` / `unit_name` / `layer_count` / `qty_total` / `amount_total` / `buckets[]` / `oldest_inbound_date` / `max_age_days` / `avg_age_days` |
| 提示 | `hints[]` | 口径 / 单位混合 / 空数据 / 截断，纯文本（不含 markdown 装饰） |

错误口径与 ERP 其余查询接口一致：非法 UUID / 日期 → **400 + 66006**，文案点名字段。

## 3. 证据清单

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `01_cargo_test.out` | `ERP_DB_URL=…cargo test --lib business::erp::inventory_aging::`（真库 cp_m209_report_test） | **62 passed / 0 failed，0 SKIP**（M2-08 原有 36 + 本卡 26，其中真库 10） |
| `02_migration_drill.out` | `bash scripts/verify/m209_inventory_aging_report_drill.sh cp_m209_mig_drill`（cloudpivot_test 新克隆） | **PASS=20 FAIL=0 / ALL PASS** |
| `03_api_check.out` | `python3 docs/evidence/…/m209_api_check.py`（隔离实例 `127.0.0.1:18086`，profile=test） | **PASS=54 FAIL=0 / ALL PASS** |
| `04_api_response_sample.json` | 一次真实响应（物料乙 × 仓乙，四档各一层） | 原样落盘 |
| `05_gates.out` | fmt / clippy（`--lib` 与 `--lib --all-targets`，均 `-D warnings`） | 本包 fmt rc=0；clippy **0 warning** |
| `06_scope_perm_menu.out` | `scope_gate` / `perm_menu_gate` / `menu_invariants` | 闸② 0 处；`missing_in_menu=0`；8 条全绿（菜单总数 518） |
| `m209_api_check.py` | 上面 03 的断言脚本（可重跑） | 54 条逐条 PASS/FAIL |

另：`cargo test --lib system::menu::` → **22 passed**（含 `permission_audit_tests` n14–n21、
n10「注解码必须有菜单承载」——本卡新码就在这条的判定面里）。

## 4. 真接口证据摘要（03）

- 200 + JSON（不是 SPA fallback 的 HTML）+ `code=0`；无 token → **401**（权限闸真的挂着）
- 表头四档：`D0_30/0-30 天`、`D31_60/31-60 天`、`D61_90/61-90 天`、`OVER_90/90 天以上`
- 夹具「物料甲 × 仓甲」（100@06-01 被吃满 / 200@09-20 ×20 / 50@11-05，出库 120@11-10）：
  数量 **230**、层数 **2**、最老 **2026-09-20**、最大库龄 **57**、加权平均 **47.00**；
  四档 **50 / 180 / 0 / 0**，参考金额 **500 / 1800 / 0 / 0**（行合计 2300）
- 夹具「物料乙 × 仓乙」（70@07-01 / 30@09-01 / 20@10-20，无出库）：数量 **120**、3 层、
  四档 **20 / 0 / 30 / 70**、加权平均 **104.00**、金额 **240**
- 基准日快照：甲 `as_of=2026-10-01` → **300**（11-05 的层与 11-10 的出库都不在），
  90 天以上 **100**、0-30 **200**
- 空结果：甲 `as_of=2025-01-01` → 0 行 / 0 合计 / 有提示，**不报错**
- `as_of` 留空 → `as_of_source=TODAY`、`as_of=2026-11-16`（CST 今天）
- 合计恒等式：全量 / 甲 / 乙 三组响应都满足 `Σ档位 == Σ行 == qty_total`
- 非法参数 2 组 → 400 + 66006 且文案点名字段；响应里无 `page/page_size`，`row_limit=2000`、
  `rows_truncated=false`

## 5. 迁移 V0425（只追加、幂等、不碰业务数据）

`V0425__inventory_aging_report.up.sql` / `.down.sql`：

* ① 前置护栏：V0424 的库函数必须在（报表没有第二个数据源）、「即时库存」页唯一、码未被占用；
* ② 权限点 `business:erp:inventory-aging:report`（`menu_type=3`，挂「即时库存」页，`sort_order=50`，
  `title=库龄分段报表`，`is_visible=false`）；
* ③ 授权：跟**持库龄明细码**（`…:inventory-aging:list`）的角色走 + 超管（数据驱动，重放不增长）；
* ④ 自检 7 条 + **行为探针**：按报表真实形状（WITH 层 → 算 `age_days` → `GROUP BY 物料/仓库/档`）
  在库层执行一次，空库 0 行也算过（证明列名 / 日期算术 / CASE 分档都跑得通）；
* down：只删本迁移的授权行与菜单行 —— **保留 V0424 的函数**（报表回滚 ≠ 计算回滚，演练 ③ 专门验这条）。

演练 20 条断言覆盖：up→up（幂等）→down→down（幂等）→up（恢复）、菜单字段 / 父子 / 同父 sort、
授权闭包缺口 0、业务数据行数不变、菜单总数 = 基线 + 1、聚合探针可执行。

## 6. 决策记录（为什么这么做）

| 决策 | 理由 |
|:--|:--|
| 报表挂在 `inventory_aging/report/` 子模块（四件套），不塞进 M2-08 的四个文件 | 明细与报表是两张表、两个权限码、两种访问频度（分页 vs 汇总）；混在一起两套契约会互相污染。算法仍只有一份（库函数） |
| 新开 `:report` 码，不复用 `:list` | 「能看明细」≠「能看全公司按物料的库龄金额分布」；与应收 `receivable:aging`、应付 `report:payable-aging` 单开报表码同一取向 |
| 库龄档位表单开 `LAYER_BUCKETS`（4 档），算法函数复用 payable 那份 | 档位语义不同（库龄 vs 逾期），但「一张表 + CASE 生成 + bucket_of_in」这套机制不该再写第二遍 |
| `age_days` 在 SQL 里算成列（`with_age` CTE），`CASE` 里零占位符 | `CASE` 要出现在 `SELECT` 与 `GROUP BY` 两处；带参数会让占位符个数翻倍、参数顺序难核对 |
| 合计在截断**之前**算，`summary` 永远全量 | 先截再合计 = 给用户一张「合计也少一截」的假报表 |
| 行序 = 最老货在前 → 数量降序 → 物料 id → 仓库 id | 库龄分析先看压得久的；全序保证翻页 / 刷新不会换行 |
| `avg_age_days`（数量加权）与 `max_age_days` 都给 | `max` 是极端值（90 天前入 1 件就把整行拉成 90 天），加权平均才回答「平均压了多久」 |
| 只统计 `qty_remaining > 0` 的层 | 吃满的层是历史：数量上等价（剩 0），但会把「层数」和「最老日期」带偏 |
| 一次聚合查询不开 `REPEATABLE READ` 事务 | 单条 SQL 本身就是一个快照（应收 / 应付账龄要开是因为它们分多条 SQL 取明细与汇总） |

## 7. 已知口径（明写，不假装）

1. **没有层表**：库龄层每次查询推导（V0424 的窗口函数）；千万级流水需要预聚合 / 物化视图，
   当前 181 表模板库上毫秒级。
2. **业务日期兜底档**：`MATERIAL_SERIAL` / `SERIAL_BATCH` 无业务日期列，回落 `created_at`(CST)。
3. **没有入库流水的库存不计入库龄**（直接写 `biz_inventory` 的行看不见）；合规路径的库存期初会写 `OPENING` 流水。
4. **金额是参考金额**：`Σ(层剩余 × 层入库单价)`，与移动加权账面成本（`biz_inventory.avg_cost`）本来就不相等 —— 库龄是数量分析。
5. **跨物料 `qty_total` 单位混合**（件 / 公斤 / 米），只作总量级参考（`hints` 里明写）。
6. **真接口验收跑在 `cloudpivot_test` 克隆 + 夹具上**，不是 144 生产数据。
7. `cargo fmt --all -- --check` 在 main 上本来就红（**vendored `nik-*` crates 未按 rustfmt 格式化**，与本卡无关）：
   本卡口径用 `cargo fmt -p cloudpivot -- --check`（rc=0）。`cargo fmt --all` 会顺手重排 152 个 vendor 文件 ——
   已回退，不入提交。

## 8. 复现步骤

```bash
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
# ① 克隆演练库（99 上跑，PG 在 145）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m209_mig_drill
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m209_report_test

# ② 迁移幂等 / 回滚（20 条断言）
bash scripts/verify/m209_inventory_aging_report_drill.sh cp_m209_mig_drill

# ③ 真库用例（62 条：M2-08 的 36 + 本卡 26）
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m209_report_test" \
  cargo test --lib business::erp::inventory_aging:: -- --test-threads=1

# ④ 真接口：先起隔离实例（迁移由 loader 补齐）→ 灌夹具 → 自签超管 token → 跑断言
psql -h 172.18.10.145 -U cloudpivot -d cp_m209_report_test -v ON_ERROR_STOP=1 \
  -f scripts/verify/m209_inventory_aging_report_fixture.sql
python3 docs/evidence/2026-11-16-m209-aging-report/m209_api_check.py

# ⑤ 清场
dropdb -h 172.18.10.145 -U cloudpivot cp_m209_report_test
dropdb -h 172.18.10.145 -U cloudpivot cp_m209_mig_drill
```
