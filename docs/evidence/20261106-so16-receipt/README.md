# SO-16 · 收款单表 + 迁移（V0292）· 证据包

卡：`t_c8b17248`（父卡 `t_3145cf38` SO-15 应收台账前端）｜执行：backend-developer｜日期：2026-11-06 CST
下游：`t_d071ab1c`（SO-17 收款后端 API + 核销）

## 1. 交付物

| 类型 | 文件 |
|------|------|
| 迁移（up） | `backend/migrations/business/V0292__sales_receipt.up.sql` |
| 迁移（down） | `backend/migrations/business/V0292__sales_receipt.down.sql` |
| 探针（外部独立校验） | `docs/evidence/20261106-so16-receipt/constraint-probes.sql` |
| 回滚演练脚本 | `docs/evidence/20261106-so16-receipt/rollback-drill.sh` |
| 证据 | 本目录（引擎落库 / 结构 / 31 条探针 ×3 轮 / down→up 往返 / 启动自检） |

**建了什么**

- `biz_receipt`（收款单头，**16 列**）= ARCH-7 §3.1.7 的 11 个业务字段 + 审计 5 件套
  （`created_at/updated_at/create_by/update_by/deleted_at`）
- `biz_receipt_allocation`（核销明细，**7 列**）= 稿面 5 字段 + `created_at/create_by`
- `biz_seq(seq_type='RC')` 取号规则 → `RC-yyyyMMdd-000001`（按 CST 天滚动）

**迁移号更正**：卡面写 `V0265__sales_receipt`，但 V0265 已被 SO-04 占用（已应用迁移不可改、
指纹校验会炸）。开工实测 business/ 最大号 = V0291（V0290 属并行卡在途）⇒ 本卡落 **V0292**。

## 2. 验收逐条

### ① 迁移 APPLY_OK（走引擎，不是手灌 SQL）

`REPO=/opt/cloudpivot ERP_DB=cloudpivot_so16b RECREATE=1 PORT=18117 bash scripts/verify/erp_clean_db.sh`
→ `migration-apply.txt`：干净库 → 一个实例启动 → 引擎把 `migrations/` 全部应用（含本次 V0292）：

```
schema_migrations(max|count) = 292|220
NOTICE: V0292 自检通过：biz_receipt 16 列 / biz_receipt_allocation 7 列 / 5 约束 / 9 索引 /
        外键(CASCADE+RESTRICT) / RC 取号=RC-20261106-000001
数据库迁移完成: 应用 220 个迁移
```

`schema_migrations` 行（`migration-apply-row.txt`）：

| kind | version | name | applied_via | success | execution_ms | checksum16 |
|------|---------|------|-------------|---------|--------------|------------|
| **versioned** | **292** | **sales_receipt** | **executed** | **t** | **17** | **5d7acb8c5802aa9c** |

即卡面要求：`kind=versioned`、`applied_via` 只用 `executed`（引擎写入路径，无手工 SQL 记录）。

结构实测（`schema.txt`，`\d` 原文）：

| 表 | 列 | CHECK | 索引 | 外键删除动作 |
|----|----|-------|------|--------------|
| `biz_receipt` | 16 | 4（status / amount / method / cancelled） | 6（pkey + uk_no + customer/status/date/voucher） | — |
| `biz_receipt_allocation` | 7 | 1（`amount > 0`） | 3（pkey + uk_pair + receivable） | `receipt_id` **CASCADE** / `receivable_id` **RESTRICT** |

### ② 约束生效（31 条探针 × 3 轮全绿）

`constraint-probes.sql`（**不复用**迁移内自检逻辑，纯外部视角）：
`round-1-probes.out.txt`（引擎 applied 库 `cloudpivot_so16b`）、
`round-2-probes.out.txt`（同库再跑一遍 → 证明探针自清场）、
`round-3-probes.out.txt`（`cloudpivot_so16_drill`，down→up 重放后走 psql 直灌路径）。

| 轮次 | 库 | 结果 |
|------|----|------|
| 1 | cloudpivot_so16b（引擎 applied） | **31 / 31 PASS**（rc=0） |
| 2 | cloudpivot_so16b（同库重跑） | **31 / 31 PASS**，表内残留 0 行 |
| 3 | cloudpivot_so16_drill（down→up 后） | **31 / 31 PASS** |

探针覆盖（expected → 实测）：

| 组 | 探针 | 期望 → 实测 |
|----|------|-------------|
| 结构 | P01–P07 | 16 列 / 7 列 / 5 CHECK / 9 索引 / 三列 `NUMERIC(18,4)` / FK 动作 `c`+`r` — 全部一致 |
| 金额三连 `ck_biz_receipt_amount` | P08–P12 | amount=0、amount=-5、allocated 100.0001>100、allocated=-0.0001、amount 省略(NULL) → 全部被拒 |
| UPDATE 超核销 | P21 | `allocated_amount=1000.0001 > amount 1000` → 拒；改 600 → 通过 |
| 状态域 `ck_biz_receipt_status` | P13/P14 | `OPEN`（应收域值）/ `PARTIAL_PAID`（应付域值）→ 均拒（抄隔壁模块必炸） |
| 方式域 `ck_biz_receipt_method` | P15 | `ALIPAY` → 拒；`NULL` → 合法（P20） |
| 作废口径 `ck_biz_receipt_cancelled` | P16 | `CANCELLED` 且 allocated=60 → 拒 |
| 默认值 / 唯一 / 精度 | P17–P20 | 新单 `DRAFT|0.0000|1000.0000`；存活行重号拒；软删后同号可复用；`0.0002` 不截断 |
| 核销明细 `ck_biz_rcpt_alloc_amount` | P22/P23 | amount=0 / -1 → 拒；300 → 通过（P24） |
| 重复核销 `uk_biz_rcpt_alloc_pair` | P25 | 同 `(receipt, receivable)` 第二行 → 拒 |
| 外键 | P26/P27 | 核销到不存在的 receivable / receipt → 均拒 |
| 删除动作 | P28/P29 | 硬删被核销应收 → 拒（**RESTRICT / SQLSTATE 23001**）；删收款头 → 明细级联清零（CASCADE） |
| 取号规则 | P30 | `RC|yyyyMMdd|-|1|1` → `RC-20261106-000001` |

### ③ 回滚演练（down → 断言 → up → 断言）

`rollback-drill.sh` → `rollback-drill.out.txt`（从 `cloudpivot_so16b` 模板克隆出一次性库
`cloudpivot_so16_drill`，不碰源库）：

| 步骤 | 实测 |
|------|------|
| [0] 演练前 | `table=biz_receipt alloc=biz_receipt_allocation rc_seq=1 checks=5 idx=9` |
| [1] 执行 down | `table=<null> alloc=<null> rc_seq=0 receivable=biz_receivable other_seq=AR,CT,PO,SH,SN,SO,STOCK_OUT,STOCK_TAKE`（down 自检断言：receivable 未被连带删、其它取号规则原样） |
| [2] 重放 up | `table=biz_receipt alloc=biz_receipt_allocation rc_seq=1 checks=5 idx=9`，自检第二次跑通过 |
| [3] 第三轮探针 | `31 PASS`，表内残留 0 行 |

另：对**已存在**表的重复执行（幂等）也已实测 —— 索引 `already exists, skipping`、自检通过、无报错。

### ④ 启动链自检（新迁移与既有自检共存）

`boot-health.txt`：库已是最新时再起实例 ——

```
数据库迁移完成: 应用 0 个迁移
cloudpivot 启动完成, 耗时 578ms
GET /health → http=200 {"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"test",...}}
```

`boot-run.log` 另记全套引导自检：SSE 鉴权自检通过 / 数据权限自检通过（11 张登记表）/
测试残留 0 行 / 孤儿任务依赖 0 条 / AI 用户不变式通过 / 三项启动守卫通过。

## 3. 与 ARCH-7 稿面的差异（逐条有理由，别当漏项）

| 项 | 稿面 | 本卡 | 理由 |
|----|------|------|------|
| 金额精度 | `NUMERIC(18,2)` | `NUMERIC(18,4)` | 与 `biz_voucher_entry` / `biz_receivable.amount` / `biz_payment.amount` 四位口径一致；少两位会静默四舍五入（V0287 对 receivable 同决策） |
| 核销明细列 | 5 字段 | +`created_at`/`create_by` | 核销是动钱动作，审计要能回答「谁在何时把哪笔钱抵到哪笔应收」。随收款级联删除，故不加 `updated_at`/`deleted_at` |
| `receivable_id` 外键 | 未写 | **ON DELETE RESTRICT**（不是 CASCADE） | 卡面写「外键 ON DELETE CASCADE」，但级联删会**静默吞掉**核销明细而收款头 `allocated_amount` 仍是原值 ⇒ 恒等与审计同时崩。应收本身走软删（V0287），实务不触发 RESTRICT；真要物理删一笔被核销过的应收，库层让他先解核销 |
| `receipt_id` 外键 | CASCADE | **CASCADE**（照稿面/卡面） | 收款头没了明细就是孤儿行 |
| `customer_id` 外键 | FK → biz_customer | **弱引用无外键** | 客户走软删（V0260），外键会堵死「客户软删但历史收款单要留」；与 `biz_receivable` / `biz_sales_order` / `biz_shipment` 同风格 |
| 唯一约束 | 只提 `receipt_no` 唯一 | 另加 `(receipt_id, receivable_id)` 唯一 | 防「同一收款对同一应收插两行」让「核销总额 ≤ 收款金额」的校验形同虚设 |
| 额外 CHECK | 无 | `ck_biz_receipt_method` / `ck_biz_receipt_cancelled` | 方式白名单 + 「作废的收款不可能核销过钱」（与 V0290 `ck_biz_ar_cancelled` 同思路） |

## 4. 交给下游 SO-17（`t_d071ab1c`）的硬约束

1. **核销写法**：`INSERT … ON CONFLICT (receipt_id, receivable_id) DO UPDATE SET amount = …`
   —— `uk_biz_rcpt_alloc_pair` 让同一对只存一行；反核销 = `DELETE` 该行 + 回滚应收。
2. **核销事务必须同时改三处**：`biz_receipt.allocated_amount`（≤ amount，库层 CHECK 兜底）、
   `biz_receivable.received_amount` + `unsettled_amount`（**恒等式 CHECK**：`amount = received + unsettled`）、
   应收 `status` 走 `OPEN → PARTIAL → SETTLED`（**不是** `PARTIAL_PAID`，`receivable::STATUSES` 是唯一真相）。
3. **状态机登记**（本卡不做，SO-17 做）：`DocKind::Receipt` + `state_machine::RECEIPT_EDGES`。
   ARCH-7 §4.4 的边：`DRAFT→CONFIRMED`、`CONFIRMED→POSTED`、`DRAFT→CANCELLED`；
   **已过账收款如何冲销（红字凭证）§4.4 图里没画**，属 SO-17 决策（别默认把 `POSTED→CANCELLED` 加进去就完事）。
   注意 `doc_kind.rs` 的单测钉住「枚举 ↔ 前端 TS ↔ 值域配置」三者一致，改枚举要同步。
4. **凭证来源**：`biz_voucher.source_type` 库层无 CHECK 白名单，但
   `doc_kind::VoucherSourceType::ALL` 有 15 项且被单测钉死 —— 加 `RECEIPT` 要同批改枚举 + 常量表。
5. **权限码未种**：本卡只建表，`business:erp:receipt:*` 的菜单/权限节点与前端菜单由 UI 卡负责。
6. **单号取号**：`biz_seq(seq_type='RC', prefix='RC', date_pattern='yyyyMMdd', separator='-', step=1)`
   已种好，`RC-20261106-000001` 六位补零由 model 层 `NO_SEQ_WIDTH` 决定（与 receivable 同）。

## 5. 门禁适用性

| 门禁 | 结果 |
|------|------|
| 迁移 APPLY_OK（引擎） | 通过（292 / versioned / executed / success=t） |
| 约束生效 | 31 条探针 × 3 轮 31/31；含「应收/应付域值必须被拒」的反向探针 |
| 回滚演练 | down→up 往返 + 幂等重放，零残留 |
| `cargo test` / clippy / fmt | **不适用**：本卡只交付 SQL 迁移（零 Rust 改动、零 API 改动）。
`business/` 下 `receipt.rs` 属 SO-17；且开工时工作区有并行卡（`t_b33adc45` 出库作废红冲）的
在途 Rust 改动（`receivable*.rs` / `doc_kind.rs` / `state_machine.rs`），本卡**未碰未提交**，
故不在此卡跑 `cargo test`（结果会被他的 WIP 污染，且会抢 target 锁） |
| 前端 | `不适用`（本卡零前端改动） |
| 真实 token curl | `不适用`（本卡零 API 改动，接口在 SO-17）；引擎/psql 实测已留证 |

## 6. 复跑命令

```bash
# ① 干净库 + 引擎全量迁移（含 V0292）
cd /opt/cloudpivot
REPO=/opt/cloudpivot ERP_DB=cloudpivot_so16b RECREATE=1 PORT=18117 \
  LOG=/root/so16b.log bash scripts/verify/erp_clean_db.sh

# ② 约束探针（可重复执行，自清场）
cd docs/evidence/20261106-so16-receipt
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so16b -v ON_ERROR_STOP=1 -f constraint-probes.sql

# ③ 回滚演练（克隆 → down → up → 第三轮探针）
bash rollback-drill.sh
```
