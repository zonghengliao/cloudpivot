# M3-01 销售报价单表结构（`biz_quotation` + `biz_quotation_line`）· 证据

- **卡**：`t_6a61726c`（M3-01【M3·销售】报价单：表结构 biz_quotation + biz_quotation_line）
- **迁移**：`V0432__sales_quotation`（up + down）
- **日期**：2026-11-17
- **结论**：静态 13/13、真库往返 PASS=56 FAIL=0、真引擎 PASS=16 FAIL=0；`cargo fmt --check` /
  `cargo clippy --lib -D warnings` / `cargo clippy --test -D warnings` / `scripts/check.sh --l0` 全绿。

## 交付物

| 文件 | 作用 |
|---|---|
| `backend/app/migrations/business/V0432__sales_quotation.up.sql` | 建 2 张表 + QT 取号行 + 2 个自检 DO 块 |
| `backend/app/migrations/business/V0432__sales_quotation.down.sql` | 幂等回滚 + 「还有非作废报价单就拒绝」护栏 |
| `backend/app/tests/m3_01_quotation_migration.rs` | 13 条静态不变量（文件解析 + 引擎拆分器，不连库） |
| `scripts/verify/m301_sales_quotation_drill.sh` | 真库往返 56 条断言（up×2 / 结构 / 行为 / 写入契约 / down 护栏 / 重放） |
| `scripts/verify/m301_engine_upgrade.sh` | 真引擎 16 条断言（克隆库升级 + 指纹登记 + 重启幂等） |

## 表结构

### `biz_quotation`（17 列 / 5 条 CHECK / 5 条索引）

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | UUID PK | 主键 |
| `quote_no` | VARCHAR(32) NOT NULL | QT-yyyyMMdd-000001（`biz_seq` seq_type=QT，按天滚动） |
| `customer_id` | UUID NOT NULL | 客户（**弱引用** `biz_customer.id`，无外键） |
| `quote_date` | DATE NOT NULL | 报价日期 |
| `valid_until` | DATE NOT NULL | **有效期至**（含当天） |
| `status` | VARCHAR(24) NOT NULL DEFAULT 'DRAFT' | DRAFT / SENT / ACCEPTED / REJECTED / CANCELLED |
| `total_amount` / `total_tax` / `total_with_tax` | NUMERIC(18,4) NOT NULL DEFAULT 0 | 三个合计；`ck_biz_quotation_amount` 钉死 `total_with_tax = total_amount + total_tax` |
| `converted_so_id` | UUID（可空） | 转成的销售订单（**弱引用**，M3-03 回填） |
| `converted_at` | TIMESTAMPTZ（可空） | 转单时间（与上一列成对，`ck_biz_quotation_converted`） |
| `remark` | TEXT | 备注 |
| `created_at` / `updated_at` / `create_by` / `update_by` / `deleted_at` | — | 审计四列 + 软删 |

索引：`uk_biz_quotation_no (quote_no) WHERE deleted_at IS NULL`（存活行唯一）·
`uk_biz_quotation_converted_so (converted_so_id) WHERE converted_so_id IS NOT NULL`（一单一报价）·
`idx_biz_quotation_customer / _status / _valid_until`。

### `biz_quotation_line`（12 列 / 5 条 CHECK / 2 条索引）

`id` · `quotation_id`（硬外键 **ON DELETE CASCADE**）· `line_no`（1 起，单内唯一）·
`material_id`（弱引用）· `qty` NUMERIC(18,4) > 0 · `price` NUMERIC(18,6) >= 0 ·
`amount` NUMERIC(18,4) · `tax_rate` NUMERIC(7,4) 0–100 · `tax_amount` NUMERIC(18,4) · `remark` · 审计两列。

索引：`uk_biz_quotation_line_no (quotation_id, line_no)`、`idx_biz_quotation_line_material`。

## 关键决定（都是**决定**，不是待办）

1. **有效期是时间事实，不是状态**：`valid_until >= quote_date` 在库层（`ck_biz_quotation_valid_until`），
   允许「当天有效」；「已过期」一律现算 `CURRENT_DATE > valid_until`，**不落状态列、不加定时翻转**。
   做成状态就需要定时任务 + 时钟回拨对账，且「过期后又被接受」会造出逆向迁移边。
2. **状态域 = 卡面 4 态 + `CANCELLED`**：卡面「草稿→已发→已接受/已拒绝」是 4 态，但 ARCH-7 §12.3
   「正向必配反向」要求每张单据有反向动作，否则报价单一发出去就永久停在 SENT（正是约定 11
   「单据停在中间态」那类坑）。迁移边（LEGAL_EDGES / `DocKind`）留给 M3-02 登记，本卡只锁域值。
3. **转单回链两列成对，但不与 `status` 咬合**：`ck_biz_quotation_converted` 只管「id 与时间同时有/同时无」。
   刻意**不**写「只有 ACCEPTED 才能转单」——「客户口头接受、销售先下单、事后补状态」是常态，
   写进库层会让这种路径只能在改库/改迁移之间二选一；该判断放 M3-03 的 model 层（可读可单测）。
4. **`uk_biz_quotation_converted_so` 反向唯一**：一张销售订单只能由一个报价单认领，防「这单到底是哪个
   报价换来的」多义；同时它是 M3-03 转单幂等的库层兜底（没有它，连点两次会出两张订单）。
5. **弱引用（无外键）**：`customer_id` / `material_id` / `converted_so_id`。客户与物料可软删，历史报价
   必须永远可读（AGENTS.md 约定 13）；`converted_so_id` 加外键会让 M3-03 的「建单后回填 / 作废重开」成环。
   唯一硬外键是 `明细 → 单头` 的 CASCADE（明细离开单头无意义）。
6. **行号单内唯一**（`uk_biz_quotation_line_no`）：行号是 M3-03「订单行 ↔ 报价行」的对应键。
   `biz_sales_order_item` 当年只 CHECK 了 `line_no >= 1`，那是历史选择；本表从建表起钉死。
7. **不复用 `biz_price_quote`**：它是采购询价的供应商报价（V0321，inquiry_item × supplier），方向相反，
   复用会把采购口径写脏。也不复用 `biz_price_list`（主数据当前价，历史报价会被下次改价覆盖）。
8. **不种权限码 / 不种菜单**：本卡还没有接口，码必须跟接口一起种（AGENTS.md 约定 7）；M3-02 起才会有
   `business:quotation:*` 与侧栏页面。

## 实测命令与输出

```bash
# 静态不变量（13 条，不连库）
cd backend && cargo test --test m3_01_quotation_migration        # → 02_static_tests.out

# 真库往返（56 条断言；默认 99 → 145，145 上跑加 PG_HOST=127.0.0.1）
bash scripts/verify/m301_sales_quotation_drill.sh                # → 00_drill.out

# 真引擎（16 条断言：克隆库升级 + 指纹登记 + 重启幂等）
bash scripts/verify/m301_engine_upgrade.sh                       # → 01_engine_upgrade.out

# 迁移号段全局唯一闸
python3 scripts/lint/migration_version_unique.py --root .        # → 03_version_gate.out
```

| 项 | 结果 |
|---|---|
| 静态不变量 | **13 passed / 0 failed** |
| 真库往返演练 | **PASS=56 FAIL=0**（up×2 幂等、结构、13 组行为探针、写入契约、down 护栏反证、down×2 + up×1） |
| 真引擎升级 | **PASS=16 FAIL=0**（引擎应用 28 个迁移；`V0432 自检通过` NOTICE；`schema_migrations` 登记 `432/true/app/migrations/business/V0432__sales_quotation.up.sql`；重启「应用 0 个迁移」） |
| 迁移号段 | 716 文件 / 352 对 up-down / 432 版本，最大 V0432（下一个可用 V0433） |
| `cargo fmt --check` | 干净 |
| `cargo clippy --lib -- -D warnings` | 干净 |
| `cargo clippy --test m3_01_quotation_migration -- -D warnings` | 干净 |
| `scripts/check.sh --l0` | 全部检查通过（含迁移版本号唯一 / 库存写入口 / 路径引用等闸） |

## 环境与清理

- 演练库：`cp_m301_quotation_drill_<pid>`（trap 自 drop）；真引擎库：`cp_m301_upgrade`（trap 自 drop）。
- 跑完 `pg_database` 无 `cp_m301%` 残留，`pgrep -f target/debug/cloudpivot` 为空。
- 迁移只追加、不改已应用文件：`V0432` 是本仓当前最大号（提交前复核过）。
