# SO-32 证据包 —— 价格两表 + 物料标准价（迁移 V0270）

- 卡片：`t_5063e6ec`（SO-32，ARCH-7 §15.3.2 / §15.3.4）
- 结论：**迁移 V0270 APPLY_OK，唯一索引生效，回滚可逆，历史迁移零改动**
- 本卡**无 Rust / 前端改动**（只落存储层：两张表 + 一列），故无 API curl 留证；
  验收面全部落在「迁移真实执行」上。

## 1. 交付清单

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0270__sales_price.up.sql` | `biz_customer_price`(14 列) + `biz_price_tier`(16 列) + `biz_material.standard_price numeric(18,4)`，含末尾自检 DO 块（结构 + 12 组行为探针） |
| `backend/migrations/business/V0270__sales_price.down.sql` | 真逆：删两表 + 删列 + 反向自检（含数据丢失备份提示） |
| `scripts/verify/so32_price_migration.sh` | 三阶段验证脚本（空库全量重放 / 乱序补迁 / down→up→二次 up） |
| `docs/evidence/20261107-so32-price-migration/*` | 本证据包 |

## 2. 验证结果（全部真实执行）

| 证据文件 | 内容 | 结果 |
|---|---|---|
| `migration_drill.txt` | 三阶段 34 条断言 | **FAIL=0 全绿** |
| `00_schema.sql.txt` | 空库重放后 `\d` 结构 + `schema_migrations` 记录 | 结构逐列与设计一致 |
| `01_out_of_order.log.txt` | 存量库（已到 V0306）补迁 V0270 的告警原文 | `WARN nik_db::migrate: 待应用迁移的版本低于已应用最大值（乱序补迁）… migration=sales_price version=270 latest_applied=306` |
| `cargo_test_lib.txt` | `cargo test --lib` | **2888 passed / 0 failed**（无代码改动，证明既有套件不受影响） |

关键断言摘录：

```
[OK] V0270 schema_migrations 记录 = 270|versioned|executed|true      ← kind=versioned / applied_via=executed
[OK] versioned 迁移条数 = 224（= 磁盘上 V*.up.sql 文件数）
[OK] applied_via 非法值行数 = 0（只允许 executed / imported）
[OK] 同键第二条被 uk_biz_cust_price 拒绝（唯一索引生效）
[OK] 价格 = 0 被 ck_biz_cust_price_price 拒绝（价格必须 > 0）
[OK] standard_price = 0 被 CHECK 拒绝
[OK] 补迁落在已应用最大值之后（applied_seq 递增，不重排历史）= true
[OK] down 后两张表/列消失 = true、物料主数据行数 5→5 未变
[OK] 二次 up（幂等）退出码 0
```

## 3. 设计决策（8 条，均有判据）

1. **号段取 V0270（卡面逐字），不取 max+1**：开工实测最大号 V0306，`V0268–V0279` 是销售链
   整段空洞，V0270 **未被占用**。alun-db 的 `apply()` 对低版本 pending 迁移只 `WARN 乱序补迁`、
   不拒绝（与 sqlx 同行为），本迁移只新建两张配置表 + 加一列，**不引用任何 V0281+ 出现的
   销售链对象**，因此空库重放（排在 V0267 之后）与存量库补迁（排在 V0306 之后）都安全。
   两种路径本次都实跑验证（阶段 1 / 阶段 2）。
2. **金额列 `NUMERIC(18,4)`**：照 ARCH-7 §15.3.2 与 `biz_customer.credit_limit` 同口径。
   ⚠ 与**单据行** `biz_sales_order_item.price` 的 `NUMERIC(18,6)` 不同：取价时 18,4→18,6 无损，
   方向不可逆。自检块断言四列都是 (18,4)，防后人照单据口径改宽。
3. **`min_qty` / `effective_from` 一律 NOT NULL**：`uk_biz_cust_price` 的四列全在唯一键里，
   PG 唯一索引里 NULL 彼此不相等 ⇒ 任何一列可空都会让「同客户同物料同起订量同起期」插入
   第二条而不报错（价格出现两条真相）。`min_qty DEFAULT 0`、`effective_from DEFAULT CURRENT_DATE`
   只是常见录入的便利，不是可空。
4. **`price > 0`（卡面硬要求）三处都落**：`biz_customer_price.price`、`biz_price_tier.price`、
   `biz_material.standard_price`（后者可空 = 未维护；非空必须 > 0）。0 元不表示免费——
   免费品走单据 0 元行，价格表里出现 0 只会让取价链静默命中 0 元，比取不到价更危险。
5. **外键 = 硬外键 `ON DELETE RESTRICT`**（与 ADR-06「单据链零外键」不冲突：本表是主数据配置、
   单态引用，同 V0138/V0260 风格）。RESTRICT 而非 CASCADE：价格是「下一单按什么价开」的依据，
   静默被级联删会让销售按裸价开单且事后无从追溯；代价是清场脚本必须**先删价格行再删物料/客户**
   （自检块 4.10p 专门验证 RESTRICT 真的挡住，注意 `ON DELETE RESTRICT` 抛的是
   **23001 restrict_violation**，不是 23503，只接 `foreign_key_violation` 会被顶穿）。
6. **`uk_biz_price_tier` 用 `NULLS NOT DISTINCT`**：通用档位 `customer_id IS NULL` 必须也受唯一性
   约束，否则同物料同档位可录 N 条并列价，取价规则（取 min_qty 最大）遇并列就没有确定答案。
   PG15+ 特性，本机 18.6 实测；仓库先例 V0137 `uk_biz_inv_loc`。ON CONFLICT 的索引推断仍按列集合
   + 谓词匹配，后续卡 upsert 照写 `ON CONFLICT (material_id, customer_id, tier_type, min_qty, effective_from) WHERE deleted_at IS NULL`。
7. **唯一索引都带 `WHERE deleted_at IS NULL`**（软删后同键可重录，与 `uk_biz_customer_code` /
   `uk_biz_sreturn_no` 同口径）。自检块有 4.10i「软删后同键重录存活行 = 1」正面探针。
8. **不建菜单 / 权限码 / 状态机迁移边**：价格是配置不是单据，`status` 只是 ACTIVE/INACTIVE 开关，
   没有 DRAFT→…→POSTED 状态机 ⇒ 不注册 `DocKind` / `LEGAL_EDGES`。取价接口与价格 CRUD 属后续卡。

## 4. 复现命令

```bash
# 三阶段（空库全量重放 + 乱序补迁 + down/up 演习），约 2 分钟
cd /opt/cloudpivot && bash scripts/verify/so32_price_migration.sh

# 单条迁移手工回滚演习（在隔离库上）
psql -h 127.0.0.1 -U alun -d cloudpivot_so32_drill -f backend/migrations/business/V0270__sales_price.down.sql
psql -h 127.0.0.1 -U alun -d cloudpivot_so32_drill -f backend/migrations/business/V0270__sales_price.up.sql
```

隔离库（保留供复现）：`cloudpivot_so32`（空库全量重放结果）、`cloudpivot_so32_ooo`（存量库乱序补迁结果）、
`cloudpivot_so32_drill`（回滚演习结果）。实例均已停止（18232 / 18233 无监听）。

## 5. 交给后续卡（SO-33 取价 / 价格页）的坑

- 取价 SQL 的确定性口径：协议价取 `min_qty` 最大的命中行；档位价按 `min_qty <= 订购量 < max_qty`，
  `max_qty IS NULL` 表示无上限；`status='ACTIVE'` 且 `effective_from <= 当日 <= COALESCE(effective_to, 'infinity')`。
- 价格表精度 18,4，落到单据行（18,6）时直接扩位即可，**不要**反向截断。
- 写测试/清场时先删 `biz_customer_price` / `biz_price_tier` 再删物料或客户（RESTRICT 外键）。
- 本卡未写 `sys_role_menu`、未建菜单（授谁是管理决策），建成页面时按 V0304/V0306 的「权限码 + 页面」两段式补。
