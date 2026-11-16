# M1-13 · 现金流底稿表 `biz_cashflow_worksheet`（V0414）证据

- **卡**：`t_31c91cd5`（M1-13【M1·财务】现金流底稿：表结构 biz_cashflow_worksheet）
- **拆卡依据**：`docs/planning/20261113-新卡清单.md` §4 M1 财务 M1-13 行（底稿行 = 项目 × 期间 × 金额）
- **日期**：2026-11-16
- **迁移**：`backend/app/migrations/business/V0414__cashflow_worksheet.{up,down}.sql`（版本号 V0414，上一卡头是 V0413，下一卡从 V0415 续）

## 一、本卡交付了什么

**只建一张表**：现金流量表**编制底稿**的行表。不种数据（底稿逐期间编制，种子行没有期间可挂）、不种权限码（还没有接口）、不动任何存量表。

| # | 交付物 | 路径 |
|:--:|---|---|
| ① | 迁移 up（建表 + 7 CHECK + 4 索引 + 9 条注释 + 2 个自检块） | `backend/app/migrations/business/V0414__cashflow_worksheet.up.sql` |
| ② | 迁移 down（幂等删表 + 「有底稿就拒绝回滚」护栏） | `backend/app/migrations/business/V0414__cashflow_worksheet.down.sql` |
| ③ | 静态不变量门禁（10 条，纯文件解析 + 框架拆分器，零库零环境变量） | `backend/app/tests/m1_13_cashflow_worksheet_migration.rs` |
| ④ | 单卡真库往返演练（30 断言，脚本自建/自删演练库） | `scripts/verify/m113_cashflow_worksheet_drill.sh` |
| ⑤ | 真引擎实测（克隆库升级 + 指纹登记 + 重启幂等，14 断言） | `scripts/verify/m113_engine_upgrade.sh` |
| ⑥ | 架构扫描器归属登记（新表不再落到 `_unknown`） | `scripts/verify/module_graph.py`（`("biz_cashflow_worksheet", "erp", "report")`） |

## 二、表结构（`\d` 实测，见 `04_ddl_shape.txt`）

```
 id          uuid                     not null  gen_random_uuid()
 period_code varchar(7)               not null            -- 'YYYY-MM'，弱引用 biz_accounting_period.period_code
 line_type   varchar(8)               not null  'FLOW'    -- FLOW / OPENING / CLOSING
 item_code   varchar(32)              null                -- FLOW 行必填，弱引用 biz_cashflow_item.item_code
 item_name   varchar(64)              not null            -- 项目名**快照**
 activity    varchar(12)              null                -- OPERATING/INVESTING/FINANCING 快照（FLOW 必填）
 direction   varchar(8)               null                -- INFLOW/OUTFLOW 快照（FLOW 必填）
 amount      numeric(18,4)            not null  0         -- FLOW=本期发生额(≥0)/余额行=余额(可负)
 sort_order  integer                  not null  0
 remark      varchar(255)             null
 create_by   uuid / created_at / update_by / updated_at
```

**7 条 CHECK**：`line_type` 三选一 · 期间正则 `^[0-9]{4}-(0[1-9]|1[0-2])$` · `activity` 三选一 · `direction` 二选一 · **明细行金额 ≥ 0** · 项目名非空白 · **两类行字段形状互斥**（FLOW 三要素齐全 ⇄ 余额行三要素全空）。

**4 条索引**：`uk_biz_cashflow_ws_item`（部分唯一 `period_code,item_code WHERE item_code IS NOT NULL`）、`uk_biz_cashflow_ws_balance`（部分唯一 `period_code,line_type WHERE line_type<>'FLOW'`）、`idx_biz_cashflow_ws_period`、`idx_biz_cashflow_ws_item_code`。

## 三、为什么这么设计（关键决定，都写进了迁移头注释）

1. **为什么另建表、不复用 `biz_cashflow_item`**：配置表回答「这笔钱归哪一行」（规则），底稿回答「这一行这期多少钱」（金额）。混在一张表里，配置行会被期间数乘开，「停用一行配置」与「这期这行为 0」变成同一件事。
2. **`item_name / activity / direction` 三个快照列**：配置改名/停用后历史底稿与报表仍按当时名称呈现（AGENTS.md 约定 13「审计/历史类存快照」）；M1-16 出表也不必 join 配置，少一次快照漂移。
3. **期初/期末余额也放进同一张表**：M1-14 的勾稽式「期初 + 流入 − 流出 = 期末」= 一次单表聚合，不需要另一张「底稿头」去做跨表一致性（也就没有两处真相）。
4. **金额 `NUMERIC(18,4)`**：底稿金额来自凭证分录（`biz_voucher_entry` / `total_*` 是 `NUMERIC(20,4)`）。写窄到 2 位会在拆分时造出「底稿合计 ≠ 凭证合计」的尾差，**不报错**只差几分钱。与全仓 43 张 `amount` 列同口径。
5. **明细行金额非负、方向只由 `direction` 表达**：负数流入会让「流入合计」出现负值、与流出栏互相抵消。余额行例外（银行透支/受限资金必须录得进）。
6. **两条唯一性用部分唯一索引**：余额行 `item_code` 为 NULL，PG 允许多个 NULL —— 普通唯一约束挡不住「一个期间两个期末余额」。
7. **不加币种列**：现金流量表只按本位币编制（外币已在记账时折算）。加币种列会引出「原币还是本位币」的二义；M1-09 的 `currency` 参数只管资产负债表/利润表。
8. **不做软删**：底稿行是可重算的工作底稿、不是单据；墓碑行会造出「这行已删、报表还算着」的双真相。要追改动走操作日志。
9. **不加外键**：全仓 120 条外键里指向 `biz_accounting_period` 的是 **0 条**（期间是弱引用，反结账/重开期间不该被底稿卡住）；`biz_cashflow_item` 是配置表，停用/换版删行是正常运维 —— 快照三列保证底稿脱离配置仍可读，写入时的「项目存在且启用」由 M1-14 写入口校验。

## 四、给 M1-14（录入 + 勾稽校验·直接法）的写入口契约

```sql
-- 明细行 upsert（部分唯一索引做冲突键）
INSERT INTO biz_cashflow_worksheet
       (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order, create_by)
VALUES ('2026-11','FLOW','OPER_SALES_RECEIPTS','销售商品、提供劳务收到的现金','OPERATING','INFLOW',1200.0,101,$1)
ON CONFLICT (period_code, item_code) WHERE item_code IS NOT NULL
DO UPDATE SET amount = EXCLUDED.amount, item_name = EXCLUDED.item_name,
              activity = EXCLUDED.activity, direction = EXCLUDED.direction,
              sort_order = EXCLUDED.sort_order, update_by = $1, updated_at = now();

-- 期初 / 期末余额 upsert
INSERT INTO biz_cashflow_worksheet (period_code, line_type, item_name, amount, sort_order, create_by)
VALUES ('2026-11','OPENING','期初现金及现金等价物余额',5000.0,1,$1)
ON CONFLICT (period_code, line_type) WHERE line_type <> 'FLOW'
DO UPDATE SET amount = EXCLUDED.amount, update_by = $1, updated_at = now();

-- 勾稽：期初 + 流入 − 流出 = 期末（实测闭合 0.0000，见 01_drill.out §5）
SELECT max(CASE WHEN line_type='OPENING' THEN amount END)                              AS opening,
       sum(CASE WHEN line_type='FLOW' AND direction='INFLOW'  THEN amount ELSE 0 END)  AS inflow,
       sum(CASE WHEN line_type='FLOW' AND direction='OUTFLOW' THEN amount ELSE 0 END)  AS outflow,
       max(CASE WHEN line_type='CLOSING' THEN amount END)                              AS closing
  FROM biz_cashflow_worksheet WHERE period_code = $1;
```

两条 upsert 的 `ON CONFLICT ... WHERE` 语法**已在 145 真库上实测通过**（部分唯一索引当冲突键），M1-14 照抄即可。

## 五、验证（真实输出，全部可复跑）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 静态不变量（10 条） | `cargo test -p cloudpivot --test m1_13_cashflow_worksheet_migration` | **10 passed / 0 failed**（`03_static_tests.out`） |
| 单卡真库往返（30 断言） | `bash scripts/verify/m113_cashflow_worksheet_drill.sh` | **PASS=30 FAIL=0**（`01_drill.out`）：up×2 / down×2 / 重放 / 7 组约束反证 / upsert / 勾稽闭合 / 护栏反证 |
| 真引擎升级（14 断言） | `bash scripts/verify/m113_engine_upgrade.sh` | **PASS=14 FAIL=0**（`02_engine_upgrade.out`）：引擎应用 10 个迁移含 V0414、`V0414 自检通过` NOTICE 出现、`schema_migrations = 414/true/源文件`、重启「应用 0 个迁移」 |
| 迁移版本号唯一 | `python3 scripts/lint/migration_version_unique.py` | ✓ 680 文件 / 414 版本，最大 V0414，下一可用 V0415 |
| 迁移授权段角色码 | `python3 scripts/verify/role_code_gate.py` | ✓ 无新增未知角色码（本卡未种权限码） |
| 格式 / 静态检查 | `cargo fmt --check` / `cargo clippy --lib -- -D warnings` / `cargo clippy --test m1_13_… -- -D warnings` | 全绿（0 warning） |

### 门禁反证（证明它会红，不是摆设）

| 注入的变异 | 结果 |
|---|---|
| `amount NUMERIC(18,4)` → `NUMERIC(18,2)` | `v0414_columns_have_designed_shape` **FAILED**（"amount 必须 NUMERIC(18,4)…"） |
| 去掉 `uk_biz_cashflow_ws_balance` 的 `WHERE line_type <> 'FLOW'` | `v0414_unique_indexes_are_partial` **FAILED**（"缺「每期间一份期初/期末」的部分唯一索引…"） |

两处变异均已还原，还原后 10/10 全绿。

## 六、文件清单

```
backend/app/migrations/business/V0414__cashflow_worksheet.up.sql     新增
backend/app/migrations/business/V0414__cashflow_worksheet.down.sql   新增
backend/app/tests/m1_13_cashflow_worksheet_migration.rs              新增
scripts/verify/m113_cashflow_worksheet_drill.sh                      新增
scripts/verify/m113_engine_upgrade.sh                                新增
scripts/verify/module_graph.py                                       改 1 行（新表归属 erp/report）
docs/evidence/2026-11-16-m113-cashflow-worksheet/                    本目录
```

**不做的事**（留给下游卡，避免夹带）：无 Rust 业务代码、无接口、无权限码、无菜单行、无前端页面、无演示数据。
