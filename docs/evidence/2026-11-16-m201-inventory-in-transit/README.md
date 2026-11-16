# M2-01 · 在途库存表结构 `biz_inventory_in_transit`（V0420）验收证据

- 卡：`t_7ba5b0e2`（M2-01【M2·仓库】在途库存：表结构 biz_inventory_in_transit）
- 拆卡依据：`docs/planning/20261113-新卡清单.md` §5 M2 仓库 的 M2-01 行
- 迁移：`backend/app/migrations/business/V0420__inventory_in_transit.{up,down}.sql`
- 下游：M2-02（发货过账 → 在途 +）、M2-03（签收 → 在途转已出库）、
  M2-04（查询）、M2-05（对账：在途 + 已出 = 发货总量）按本表契约写 SQL

## 一句话

新建**一张**表（不夹带、不建接口、不种权限码）：`biz_inventory_in_transit`，
一行 = 物料 × 发货仓 × 发货单，在途量是 PG 生成列 `shipped_qty - received_qty`，
「在途 + 已签收 = 发货量」由库保证恒等。

## 表结构（15 列）

| 列 | 类型 | 说明 |
|:--|:--|:--|
| `id` | UUID PK | 主键 |
| `shipment_id` | UUID NOT NULL | 发货单（**唯一硬外键** → `biz_shipment(id)` `ON DELETE CASCADE`） |
| `material_id` | UUID NOT NULL | 物料（弱引用 `biz_material.id`，无外键） |
| `warehouse_id` | UUID NOT NULL | 发货仓库（弱引用 `biz_warehouse.id`，无外键） |
| `ship_date` | DATE NOT NULL | 发货日期快照（在途账龄口径） |
| `shipped_qty` | NUMERIC(18,4) NOT NULL | 发货数量（M2-02 过账写入，之后只读） |
| `received_qty` | NUMERIC(18,4) NOT NULL DEFAULT 0 | 累计签收（M2-03 累加） |
| `qty_in_transit` | NUMERIC(18,4) **GENERATED ALWAYS AS (shipped_qty - received_qty) STORED** | 当前在途量 |
| `status` | VARCHAR(24) NOT NULL DEFAULT `IN_TRANSIT` | IN_TRANSIT / RECEIVED / CANCELLED |
| `received_at` | TIMESTAMPTZ | 全部签收时间（RECEIVED 必填） |
| `remark` | VARCHAR(255) | 备注（作废原因等） |
| `created_at` / `updated_at` / `create_by` / `update_by` | 审计四列 | — |

约束 4 条 CHECK（状态域 / 数量 / 状态×数量一致性 / 签收时间）+ 1 条外键；
索引 4 条：`uk_biz_inv_transit_key`（物料+发货仓+发货单，M2-02 的 upsert 键）、
`idx_biz_inv_transit_shipment`、`idx_biz_inv_transit_open`（部分索引：只索引在途行）、
`idx_biz_inv_transit_age`（status + ship_date）。

## 关键设计决定（都在迁移头注释里写清了理由）

1. **不塞进 `biz_inventory` / `biz_inventory_txn`**：在途不是仓里的量（卡面 M2-02「库存不扣」），
   塞进结存账会虚增可用库存；流水没有「这一批还在路上、签收了多少」的状态。
2. **在途量是生成列**：`shipped_qty - received_qty` 由 PG 算，任何写路径都写不出不符的行；
   先例 = V0211 的 `biz_stock_take_item.diff_qty`。
3. **状态与数量在库层咬合**：`IN_TRANSIT` 必须还有余量、`RECEIVED` 必须余量为 0；
   ⇒ M2-03 必须在**同一条 UPDATE** 里同时推进 `received_qty` 与 `status`（分两句会被拦）。
4. **不留 `deleted_at`**：在途行没有「删一行」的业务动作（作废是状态），
   发货单物理删时 CASCADE；墓碑会造出「没删但也不算在途」的第二种隐藏状态。
5. **不加库位维度**：卡面口径是「物料 + 仓库 + 发货单」。
6. **对账口径**：所有在途取数与对账一律带 `status <> 'CANCELLED'`（写在列注释上）。

## 实测证据

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `00_drill.out` | `scripts/verify/m201_inventory_in_transit_drill.sh` 真库往返（up×2 / down×2 / 重放 / 护栏反证 / 生成列 / 唯一键 / 写入契约 / 对账恒等 / 级联） | **PASS=44 FAIL=0**，EXIT=0 |
| `01_engine_upgrade.out` | `scripts/verify/m201_engine_upgrade.sh` **真引擎**（克隆 `cloudpivot_test` → 二进制真启动 → 应用 V0405..V0420 共 16 条 → 指纹登记 → 重启 0 条重放） | **PASS=15 FAIL=0**，EXIT=0 |
| `02_static_tests.out` | `cargo test --test m2_01_inventory_in_transit_migration` 静态不变量（12 条，含引擎拆分器校验） | **12 passed; 0 failed** |

其它门禁：

- `cd backend && cargo fmt` → exit 0；`cargo clippy --lib -- -D warnings` → exit 0（0 warning）
- `cargo clippy --test m2_01_inventory_in_transit_migration -- -D warnings` → exit 0
- `bash scripts/check.sh --l0`（SKIP_METRICS_CHECK=1）→ 「全部检查通过（L0）」，含
  迁移文件名/版本号全局唯一闸、库存三本账唯一写入口闸（新表**不在**三本账口径内，
  故不强制走 `inventory_writer.rs`，闸门正则带 `(?!\w)` 收尾不会误判）
- 前端零改动（本卡只建表，无接口/无页面）⇒ 未跑 pnpm
- 演练库全部自建自删：`select count(*) from pg_database where datname like 'cp_m201%'` = 0

## 数据库环境

- 跑在 99（开发机），PG = `172.18.10.145:5432`，用户 `cloudpivot`
- 模板库 `cloudpivot_test`（181 表，升级前 V0404 / 326 条 versioned 迁移）
- 演练库：`cp_m201_transit_drill_<pid>`（脚本自删）、`cp_m201_upgrade`（脚本自删）
