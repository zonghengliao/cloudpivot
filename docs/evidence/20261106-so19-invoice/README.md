# SO-19 · 销售发票表 + 迁移（V0295）· 证据包

卡：`t_9cfa5cda`（父卡 `t_4d37c180` SO-18 收款前端）｜执行：backend-developer｜日期：2026-11-06 CST
下游：`t_16ccd7b6`（SO-20 发票后端 API + 开票）

## 1. 交付物

| 类型 | 文件 |
|------|------|
| 迁移（up） | `backend/migrations/business/V0295__sales_invoice.up.sql`（513 行，末段 100 行自检 DO 块） |
| 迁移（down） | `backend/migrations/business/V0295__sales_invoice.down.sql` |
| 探针（外部独立校验） | `docs/evidence/20261106-so19-invoice/constraint-probes.sql`（29 条，自清场） |
| 回滚演练脚本 | `rollback-drill.sh` |
| 启动链自检脚本 | `boot-health.sh` |
| 证据 | 引擎 APPLY（`migration-apply*.txt`）/ 结构（`schema.txt`）/ 探针 ×3 轮 / 回滚往返 / boot-health |

**建了什么**

- `biz_invoice`（销售发票，**21 列**）= ARCH-7 §3.1.8 的 16 个业务字段 + 审计 5 件套
  （`created_at/updated_at/create_by/update_by/deleted_at`）
- `biz_seq(seq_type='IV')` 取号规则 → `IV-yyyyMMdd-000001`（按 CST 天滚动）
- 6 条 CHECK、7 个索引（pkey + 存活行唯一 + 5）、1 个外键（`receivable_id` → `biz_receivable`，RESTRICT）

**迁移号更正**：卡面写 `V0266__sales_invoice`，但 V0266 已被销售订单菜单卡占用（已应用迁移不可改、
指纹校验会炸）。开工实测 business/ 最大号 = **V0294**（SO-18 刚落的收款菜单页）⇒ 本卡落 **V0295**。

## 2. 验收逐条

### ① 迁移 APPLY_OK（走引擎，不是手灌 SQL）

`REPO=/opt/cloudpivot ERP_DB=cloudpivot_so19 RECREATE=1 PORT=18119 LOG=/root/so19-apply.log bash scripts/verify/erp_clean_db.sh`
→ 干净库 → 起一个实例 → 引擎把 `migrations/` 全部应用（含本次 V0295）：

```
NOTICE: V0295 自检通过：biz_invoice 21 列 / 6 约束 / 7 索引 / 外键 RESTRICT / IV 取号=IV-20261106-000001
数据库迁移完成: 应用 223 个迁移
schema_migrations(max|count) = 295|223
```

`schema_migrations` 行（`migration-apply-row.txt`）：

| kind | version | name | applied_via | success | execution_ms | checksum |
|------|---------|------|-------------|---------|--------------|----------|
| **versioned** | **295** | **sales_invoice** | **executed** | **t** | **19** | **982a773f…92ca24** |

即卡面要求：`kind=versioned`、`applied_via=executed`（引擎写入路径，无手工 SQL 记录）。
校验和与提交文件一致（`sha256sum V0295__sales_invoice.up.sql` = `982a773f1aca0aee50bc55d2a8eaec889415af92b4451c3ae4e80a24bf92ca24`），
证据对应的是最终文件版本，不是中间稿。

### ② 约束生效（29 条探针 × 3 轮全绿）

`constraint-probes.sql`（**不复用**迁移内自检逻辑，纯外部视角）：
`round-1-probes.out.txt`（引擎 applied 库 `cloudpivot_so19`）、
`round-2-probes.out.txt`（同库再跑 → 证明探针自清场）、
`rollback-drill.out.txt` [3] 段（`cloudpivot_so19_drill`，down→up 重放后走 psql 直灌路径）。

| 轮次 | 库 | 结果 |
|------|----|------|
| 1 | cloudpivot_so19（引擎 applied） | **29 / 29 PASS**（rc=0） |
| 2 | cloudpivot_so19（同库重跑） | **29 / 29 PASS**，表内残留 0 行 |
| 3 | cloudpivot_so19_drill（down→up 后） | **29 / 29 PASS** |

探针覆盖（expected → 实测）：

| 组 | 探针 | 期望 → 实测 |
|----|------|-------------|
| 结构 | P01–P08 | 21 列 / 卡面 16 业务字段 / 6 CHECK / 7 索引 / 金额三列精度 / FK RESTRICT / 只 1 个外键 — 全部一致 |
| **金额约束** `ck_biz_invoice_amount`（卡面点名） | **P09 / P10** | INSERT `1131 ≠ 1000+130` → 拒；**UPDATE** `565→600` → 拒（约束对 UPDATE 同样生效） |
| 票种与符号 `ck_biz_invoice_sign` / `_type` | P11–P14 | OUT amount=0 / OUT amount=-1000 / OUT_RED amount=+1000 / 票种 IN → 全拒 |
| 状态域 `ck_biz_invoice_status` | P15–P17 | `OPEN`（应收域）/ `CONFIRMED`（收款域）/ `CANCELLED` → 全拒（抄隔壁模块必炸） |
| 来源 / 税率 | P18–P20 | 稿面字面量 `SO` / tax_rate `-0.01` / `1.5` → 全拒 |
| 默认值 / 唯一 / 精度 | P21–P24 | 新单 `DRAFT\|OUT\|0.1300`；存活行重号拒；软删后同号可复用；`0.0002` 不截断 |
| 红字正常路径 | P25 | `OUT_RED -1000/-130 → -1130.0000`，status=POSTED 合法 |
| 外键 | P26 / P27 | 悬空 `receivable_id` → 23503；硬删被引用的应收 → **RESTRICT（23001）** |
| 取号规则 | P28 | `IV\|yyyyMMdd\|-\|1\|1` → `IV-20261106-000007` |
| 清场 | P29 | 探针行残留 0（发票 + 探针应收） |

### ③ 回滚演练（down → 断言 → up → 断言 → 二次重放）

`rollback-drill.sh` → `rollback-drill.out.txt`（从 `cloudpivot_so19` 模板克隆出一次性库
`cloudpivot_so19_drill`，不碰源库）：

| 步骤 | 实测 |
|------|------|
| [0] 演练前 | `table=biz_invoice inv_seq=1 checks=6 idx=7 receivable=biz_receivable other_seq=AR,CT,IV,PO,RC,SH,SN,SO,STOCK_OUT,STOCK_TAKE` |
| [1] 执行 down | `table=<null> inv_seq=0 checks=0 idx=0 receivable=biz_receivable other_seq=AR,CT,PO,RC,SH,SN,SO,STOCK_OUT,STOCK_TAKE`（down 自检：应收未被连带删、其它取号规则原样） |
| [2] 重放 up | `table=biz_invoice inv_seq=1 checks=6 idx=7`，自检第二次跑通过 |
| [3] 第三轮探针 | `29 / 29 PASS`，残留 0 |
| [4] up 再跑一遍（幂等） | DDL 全 `already exists, skipping`，自检通过，结构不变 |

### ④ 启动链自检（新迁移与既有 bootstrap 自检共存）

`boot-health.sh` → `boot-health.txt`（库已是最新时再起实例）：

```
数据库迁移完成: 应用 0 个迁移
SSE 鉴权自检通过 / 孤儿任务依赖自检通过：0 条 / 测试残留自检通过 / AI 用户不变式通过 / 数据权限自检通过（11 张登记表）
cloudpivot 启动完成, 耗时 585ms
GET /health round1 http=200 {"code":0,...,"status":"ok"}
GET /health round2 http=200 {"code":0,...,"status":"ok"}
端口残余监听数 = 0
```

## 3. 与 ARCH-7 稿面的差异（逐条有理由，别当漏项）

| 项 | 稿面 | 本卡 | 理由 |
|----|------|------|------|
| 金额精度 | `NUMERIC(18,2)` | `NUMERIC(18,4)` | 与 `biz_voucher_entry` / `biz_receivable.amount` / `biz_receipt.amount` 四位口径一致；少两位会在「合计 = 不含税 + 税额」恒等上静默四舍五入（V0287 / V0292 同决策） |
| `invoice_no` 宽度 | varchar(64) | varchar(64)（照稿面） | **不**跟着 `biz_receivable.invoice_no`（varchar(50)）走：那张表只是「关联发票号」拷贝列，本表才是号码 owner |
| `source_type` 宽度 | varchar(16) | varchar(24) | 与 `biz_receivable.source_type`（V0287，24）同宽，便于「发票 ← 应收」直传不截断 |
| `source_type` 值域 | `SO / SHIPMENT` | `SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL`（+NULL） | 复用既有词表：应收侧是 `SALES_OUT/SHIPMENT/MANUAL`。稿面 `SO` 归一为 `SALES_ORDER`，迁移里有**反向探针 P18**（写 `SO` 被拒）并在列 COMMENT 写明映射 |
| `status` 值域 | DRAFT/ISSUED/POSTED/VOID | 同稿面 4 值 | 稿面逐字。注意 `VOID ≠ CANCELLED`（收款用 CANCELLED），迁移用反向探针 P15–P17 钉住 |
| 红字发票金额 | §15.2.3「amount 为负，type=OUT_RED」 | **允许负数**，`ck_biz_invoice_sign` 强绑定类型与符号 | 与 `biz_voucher`（V0149：借贷互换、金额恒为正 + is_red）**口径不同** —— 发票是单据不是凭证；文件头与列 COMMENT 都写了「别互相抄」 |
| 额外 CHECK | 只提 `ck_biz_invoice_amount` | + `_sign` / `_status` / `_type` / `_tax_rate` / `_source`（共 6 条） | 恒等式只管算数；符号、状态、票种、税率、来源各自一条，报错能指认具体错因 |
| `receivable_id` 外键 | FK → biz_receivable | **ON DELETE RESTRICT**（可空） | 级联删会静默吞掉「哪张应收开了哪张票」的留痕；应收本身走软删，实务不触发。与 V0292 对 `allocation.receivable_id` 同口径 |
| `customer_id` 外键 | FK → biz_customer | **弱引用无外键** | 客户走软删（V0260），外键会堵死「客户软删但历史发票要留」；与 receivable / receipt / sales_order / shipment 同风格 |
| 期间闸（§5 约束 S4） | 已结账期间禁止新增发票 | **不建 period_code 列** | 卡面字段集与 §3.1.8 都没有该列；闸由 SO-20 用 `invoice_date` 推出的 yyyy-MM 判定（加列会让字段集与下游 DTO 漂移） |
| 汇率 5 列（§15.6） | 未来要加 | 本卡不加 | 属 §15.6 后续批次，非本期 |

## 4. 交给下游 SO-20（`t_16ccd7b6`）的硬约束

1. **取号**：`biz_seq(seq_type='IV', prefix='IV', date_pattern='yyyyMMdd', separator='-', step=1)` 已种好，
   **只在 `invoice_no` 为空时取号**（手填税控发票号优先，先取号再覆盖会白烧号、单号出现空洞）。
   六位补零由 model 的 `NO_SEQ_WIDTH` 决定（与 AR / RC 同）。
2. **金额**：三列全 NOT NULL 且**无默认值** —— INSERT 必须显式给 `amount / tax_amount / total_with_tax`，
   且 `total_with_tax = amount + tax_amount` 由 `ck_biz_invoice_amount` 兜底（UPDATE 同样生效）。
3. **红冲**：`invoice_type='OUT_RED'` 时 `amount < 0` 且 `tax_amount <= 0`；正票反之。写正数会被 23514 拒。
   红字**凭证**仍走 V0149 的「借贷互换 + is_red + 金额恒正」，不要与发票的负金额混为一谈。
4. **状态机登记**（本卡不做，SO-20 做）：`DocKind::Invoice` + `state_machine::INVOICE_EDGES`，
   边：`DRAFT→ISSUED`、`ISSUED→POSTED`、`DRAFT→VOID`、`ISSUED→VOID`（ARCH-7 §4.5）。
   ⚠ `doc_kind.rs` 单测钉住「枚举 ↔ 前端 TS ↔ 值域配置」三者一致，改枚举要同批改。
5. **凭证来源**：`doc_kind::VoucherSourceType::ALL` 是白名单枚举（被单测钉死），加 `INVOICE` 要同批改常量表。
6. **开票金额 ≤ 应收金额**：跨表条件，库层没有触发器（本仓零 TRIGGER）。handler 里
   `SELECT … FROM biz_receivable WHERE id = $1 FOR UPDATE` 锁行后判定，超限返回 409。
   若按应收分次开票，还需 `SUM(未作废发票金额) <= receivable.amount`（红字票金额为负，天然可抵扣）。
7. **回写应收**：开票后 `UPDATE biz_receivable SET invoice_no=…, invoice_date=…`；单张应收在多票场景下
   只能存最后一次（应收侧是拷贝列，权威在 `biz_invoice`）。
8. **权限码未种**：本卡只建表，`business:erp:invoice:*` 菜单/权限节点与前端页面属 SO-21。

## 5. 门禁适用性

| 门禁 | 结果 |
|------|------|
| 迁移 APPLY_OK（引擎） | 通过（295 / versioned / executed / success=t / 19ms） |
| 金额约束生效 | 通过（`ck_biz_invoice_amount` INSERT + **UPDATE** 双向被拒；29 条探针 ×3 轮 29/29） |
| 回滚演练 | down→up 往返 + 二次重放，零残留 |
| `cargo test` / clippy / fmt | **不适用**：本卡只交付 SQL 迁移（**零 Rust 改动**、零 API 改动）⇒ 无 `#[cfg(test)] mod tests` 可加。`business/erp/invoice.rs` 属 SO-20 |
| 前端 | `不适用`（零前端改动） |
| 真实 token curl | `不适用`（零 API 改动，接口在 SO-20）；引擎 APPLY + 29 条库层探针 + `/health` 200 已留证 |

## 6. 复跑命令

```bash
# ① 干净库 + 引擎全量迁移（含 V0295）
cd /opt/cloudpivot
REPO=/opt/cloudpivot ERP_DB=cloudpivot_so19 RECREATE=1 PORT=18119 \
  LOG=/root/so19-apply.log bash scripts/verify/erp_clean_db.sh

# ② 约束探针（可重复执行，自清场）
cd docs/evidence/20261106-so19-invoice
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so19 -v ON_ERROR_STOP=1 -f constraint-probes.sql

# ③ 回滚演练（克隆 → down → up → 第三轮探针 → 幂等重放）
bash rollback-drill.sh

# ④ 启动链自检（0 迁移 + /health）
bash boot-health.sh
```
