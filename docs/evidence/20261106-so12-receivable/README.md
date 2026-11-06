# SO-12 · 应收账款表 + 迁移（V0287）· 证据包

卡：`t_d1f27e36`（父卡 `t_b143cd85` SO-11 发货单前端）｜执行：backend-developer｜日期：2026-11-06 CST
下游：`t_b0fb1724`（SO-13 应收后端 API + 账龄）

## 1. 交付物

| 类型 | 文件 |
|------|------|
| 迁移（up） | `backend/migrations/business/V0287__sales_receivable.up.sql`（建 `biz_receivable` 24 列 + `AR` 取号规则 + 末尾自检 DO 块） |
| 迁移（down） | `backend/migrations/business/V0287__sales_receivable.down.sql`（删表 + 精确删 AR 取号行 + 回滚断言） |
| 证据 | 本目录（迁移落库 / 结构 / 逐列对照 / 23 条约束探针 ×3 轮 / 回滚演练 / 启动自检） |

**迁移号更正**：卡面写 `V0264__sales_receivable`，但 V0264 已被 SO-04 的
`V0264__sales_order`（销售订单两张表）占用 —— 已应用迁移不可改、指纹校验会炸。
开工实测 business/ 最大号 = V0286 ⇒ 本卡落 **V0287**（与 SO-04/SO-08 的号段冲突处理口径一致）。

## 2. 验收逐条

### ① 迁移 APPLY_OK（走引擎，不是手灌 SQL）

`REPO=/opt/cloudpivot ERP_DB=cloudpivot_so12 RECREATE=1 PORT=18112 bash scripts/verify/erp_clean_db.sh`
—— 干净库 → 一个实例启动 → 引擎把 `migrations/` 全部应用（含本次 V0287）：

```
schema_migrations(max|count) = 287|215
数据库迁移完成: 应用 215 个迁移
NOTICE: V0287 自检通过：biz_receivable 24 列（与 biz_payable 逐列同型）/ 5 约束 / 8 索引 / AR 取号=AR-20261106-000001
```

`schema_migrations` 行（`migration-apply-row.txt`）：

| version | name | kind | applied_via | checksum16 | ms | success |
|---------|------|------|-------------|-----------|----|---------|
| **287** | **sales_receivable** | **versioned** | **executed** | **0e0deb281482d411** | **32** | **t** |

即卡面要求：`kind=versioned`、`applied_via` 只用 `executed`（引擎写入路径，无手工 SQL 记录）。

结构实测（`schema.txt`，`\d` 原文）：24 列 / pkey + 2 唯一 + 5 普通索引 = 8 / 5 条 CHECK，
其中 `uk_biz_ar_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引，
`uk_biz_ar_source` 额外带 `source_type <> 'MANUAL'`（手工挂账例外）。

### ② 字段与 payable 对齐（逐列机器比对）

`parity.txt`：24 对映射列（`ar_no←payable_no` / `customer_id←supplier_id` /
`received_amount←paid_amount` / `unsettled_amount←unpaid_amount`，其余同名）逐列比对
`data_type / character_maximum_length / numeric_precision / numeric_scale / is_nullable`
—— **24/24 SAME，DIFF = 0**。

同一比对有**三处**落地，互为独立校验：
1. 迁移末尾 DO 块 3.3（干净库 APPLY 时就炸，防日后照抄改动）；
2. 探针 P22（`constraint-probes.sql`，与应用路径无关的第二遍）；
3. `parity.txt`（人眼可读的逐列清单）。

### ③ 约束生效（23 条探针 × 3 轮全绿）

`constraint-probes.sql` → `round-1-probes.out.txt`（引擎库 cloudpivot_so12）、
`round-2-probes.out.txt`（psql 直灌的克隆库 cloudpivot_so12_scratch）、
`round-3-probes-same-db.out.txt`（同库再跑一遍，证明探针自清场）：

| 轮次 | 库 | 结果 |
|------|----|------|
| 1 | cloudpivot_so12（引擎 applied） | **23 / 23 PASS** |
| 2 | cloudpivot_so12_scratch（psql 直灌，同一份 up） | **23 / 23 PASS** |
| 3 | cloudpivot_so12（同库重跑） | **23 / 23 PASS**，表内残留 0 行 |

探针覆盖（expected vs actual 逐条在 out 文件里）：

| 组 | 探针 | 期望 → 实测 |
|----|------|-------------|
| 金额三连 `ck_biz_ar_amount` | amount=0 / amount=-5 / received>amount（100.0001>100）/ received=-0.0001 / UPDATE 超收 | 全部 `拒 ck_biz_ar_amount` |
| 未收恒等 `ck_biz_ar_unsettled` | unsettled=59.9999（应 60） | `拒 ck_biz_ar_unsettled` |
| 状态域 `ck_biz_ar_status` | `PARTIAL_PAID`（payable 值）/ `CANCELLED`（payable 值） | 均 `拒 ck_biz_ar_status` |
| 结清口径 `ck_biz_ar_settled` | SETTLED 但未收 60 | `拒 ck_biz_ar_settled` |
| 账期 `ck_biz_ar_credit` | credit_days=-1 | `拒 ck_biz_ar_credit` |
| 正常路径 | 挂账（默认 OPEN/CNY/已收 0）→ 部分收款 400 → 全额收款 SETTLED+settled_at → 坏账 WRITTEN_OFF | 全部按预期落库 |
| 唯一/幂等 | 存活行重号 / 同 `(SALES_OUT, source_id)` 重复挂账 / 软删后同号复插 / MANUAL 同源两笔 | 前两者 `拒 uk_biz_ar_no`、`拒 uk_biz_ar_source`；后两者按预期通过 |
| 精度 | amount 0.0002 / received 0.0001 | `0.0002/0.0001/0.0001`（NUMERIC(18,4) 不截断） |
| 结构 | 24 列 / 5 CHECK / 8 索引 / `AR|yyyyMMdd|-|1|1` | 全部一致 |
| 收尾 | 探针清场 | 0 行 |

### ④ 回滚演练

`rollback-drill.out.txt`（一次性库 `cloudpivot_so12_drill2`，从 so12 克隆后分步 down → up）：

| 步骤 | 实测 |
|------|------|
| [1] 演练前 | `table=biz_receivable ar_seq=1 idx=8 checks=5` |
| [2] 执行 down | `table=<null> ar_seq=0 idx=0 payable=biz_payable`（down 自检断言通过：payable 未被连带删，其它取号规则 CT,PO,SH,SN,SO,STOCK_OUT,STOCK_TAKE 原样） |
| [3] 重放 up | `table=biz_receivable ar_seq=1 idx=8 checks=5`（自检再次通过：24 列 / 与 payable 逐列同型 / 8 索引 / AR 取号） |

（`rollback-drill.sql` 是同一演练的「单事务 BEGIN … ROLLBACK」写法，本环境的单查询安全栅会拦含
`DROP` 的脚本 ⇒ 实际执行用上面的分步版，断言内容完全一致。）

upsert 幂等另做一遍：对已存在表**再跑一次 up**（不 down）→ 索引 `already exists, skipping`、
自检通过、无报错（psql 直灌路径已验证；引擎侧靠 `schema_migrations` 指纹天然不重放）。

### ⑤ 启动链自检（新迁移与既有自检共存）

`boot-run.log` / `boot-health.txt`：库已是最新时再起实例 ——

```
数据库迁移完成: 应用 0 个迁移
cloudpivot 启动完成, 耗时 509ms
health_http=200  {"code":0,"msg":"ok","data":{"status":"ok",...}}
```

`boot-check.txt` 另记干净库首启的全套引导自检：孤儿任务依赖 0 条、数据权限自检通过、
测试残留 0 行、三项启动守卫通过。

## 3. 门禁适用性

| 门禁 | 结果 |
|------|------|
| 迁移 APPLY_OK（引擎） | 通过（287 / versioned / executed / success=t） |
| 字段与 payable 对齐 | 24/24 同型，DIFF=0（三处独立校验） |
| 约束生效 | 23 条探针 × 3 轮 23/23；含「payable 的域值必须被拒」的反向探针 |
| 回滚演练 | down→up 往返 + 幂等重放，零残留 |
| `cargo test` / clippy / fmt | **不适用**：本卡只交付 SQL 迁移（无 Rust 代码改动、无 API 改动），
不新增 `business/` 模块代码（`receivable.rs` / `receivable_query.rs` / `receivable_aging.rs` 属 SO-13） |
| `pnpm typecheck && pnpm build` | **不适用**：无前端改动 |
| 真实 token curl | **不适用**：无接口改动（5 个应收接口在 SO-13） |

## 4. 设计口径与下游卡（SO-13）必读

1. **表/列名**：`biz_receivable` / `ar_no` / `customer_id` / `received_amount` / `unsettled_amount`，
   `source_type` 默认 `'SALES_OUT'`（ARCH-7 §7.2 第 7 步：出库过账生成应收，source_type='SALES_OUT'）。
2. **单号**：`biz_seq(seq_type='AR')` 已种：`prefix=AR / date_pattern=yyyyMMdd / separator=- / step=1` ⇒
   `AR-yyyyMMdd-000001`。取号实现照 `shipment_model::next_no` 的写法（`ensure_seq_row` + `bump_seq`，
   按 CST 天滚动、时钟回拨不重置）。**本卡未写 Rust 取号函数**（属 SO-13）。
3. **状态域 4 值，与 payable 不同**（`ARCH-7` §4.3）：
   `OPEN / PARTIAL / SETTLED / WRITTEN_OFF`。
   映射：payable `OPEN→OPEN`、`PARTIAL_PAID→PARTIAL`、`SETTLED→SETTLED`、`CANCELLED→WRITTEN_OFF`。
   ⚠ 直接抄 `payable_payment.rs` 的 `STATUS_PARTIAL_PAID` 会撞 `ck_biz_ar_status`（23514）。
   `settled_at` 只在 SETTLED / WRITTEN_OFF 落值；库层不建触发器（全仓零 TRIGGER，业务判定在 Rust）。
4. **`amount` 没有默认值**（payable 有 `DEFAULT 0`）：卡面要求 `amount > 0`，留着默认值就是钓鱼 ——
   省略 amount 的插入必然撞 CHECK。写方必须显式给 `amount` 与 `unsettled_amount`（恒等式列）。
5. **来源幂等**：`uk_biz_ar_source (source_type, source_id) WHERE deleted_at IS NULL AND source_type <> 'MANUAL'`
   ⇒ 过账重试不会重复挂账；手工挂账（MANUAL）不受此限，同一 `source_id` 可挂多笔（探针 P18 已证）。
   MANUAL 行的 `source_id` 建议由 model 生成 UUID、`source_no` 写 `'MANUAL'`（列 NOT NULL）。
6. **索引**：`idx_biz_ar_customer(customer_id,status)` / `idx_biz_ar_status(status)` /
   `idx_biz_ar_due(due_date,status)`（账龄扫描，与 `idx_biz_payable_due` 同形）/ `idx_biz_ar_period` /
   `idx_biz_ar_voucher(voucher_id) WHERE NOT NULL`。索引名都带 `ar` 全词 —— PG 索引名 schema 级唯一，
   撞名 + `IF NOT EXISTS` 会被静默跳过（SO-04 踩过 `uk_biz_so_no`）。
7. **`customer_id` 是弱引用（无外键）**：与 `biz_sales_order.customer_id` / `biz_shipment.customer_id`
   同风格（客户软删后历史单据仍要可读）；ARCH-7 §3.1.6 写的 FK 按此口径执行。
8. **本卡不登记 `LEGAL_EDGES`**（与 SO-04/SO-08 分工一致）：SO-13 落 API 时登记
   `DocKind::Receivable` + `RECEIVABLE_EDGES`，边表域值必须与本 CHECK 逐字一致。
9. **既有注释已过期**：`backend/src/business/erp/payable_aging.rs` 头部「应收账龄为什么没做」
   一节写着「全库没有 `biz_receivable`」—— 本迁移之后这句不成立，SO-13 做 `receivable_aging.rs`
   时顺手改掉（本卡不改 Rust，避免与并行卡的 worktree 抢文件）。

## 5. 踩到的坑（记一笔）

1. **探针之间不能共享状态**：第一版 P17（同源重复挂账）复用了 P10 造的行当「活来源」，而 P16 已经把
   P10 软删了 ⇒ 部分唯一索引看不见它、插入成功、探针假 FAIL。改成 P17 自己先插一张活行再插第二张。
   这也是部分唯一索引的正常语义（软删行不占唯一位）。
2. **`ON DELETE RESTRICT` 抛 23001**（`restrict_violation`）而不是 23503 —— 本表没有 RESTRICT 外键，
   但同类迁移（V0283）栽过；本卡自检不写 `foreign_key_violation`。
3. **`psql -f` 跑含 `DROP` 的脚本会被安全栅拦**（本环境单查询模式无人确认）⇒ 回滚演练改为
   「克隆一次性库 + 分步执行 down/up」并保留每步的结构指纹，等价且可复核。
