# ARCH-2 · 第 2 章：`biz_contract` 合同表数据模型与校验规则

> **本章定位**：ARCH-2《合同管理 + 单据勾稽关系重构》的**第二章独立稿**，只负责「合同表本身长什么样、每条规则由谁执行、失败时说什么话、被改动时谁受伤」。
> 下游章节依赖本章：§3（采购/入库勾稽重构）、§4（快照与真相巡检）、§5（通用规则映射）都以本章的列名 / 枚举值 / 错误码为唯一口径。
> **本章不含业务代码**（无 Rust、无 Vue），DDL 是设计产物；迁移文件由实现卡落盘。
>
> 口径基线：任务书 `t_014f0f18` + 父卡 `t_e3cc469f`（三条核心原则 / Q1–Q5）。
> 全部事实来自实测：`docs/evidence/2026-11-02-arch2-contract/model-poc-output.txt`（psql 真实输出，236 行，exit=0）、`model-poc.sql`（可复跑脚本）。

---

## 0. 实测基线（先证后设计）

在写任何 DDL 之前，先把「现状到底是什么」钉死。以下每条都有命令与输出，**不接受印象**。

| # | 事实 | 证据 |
|---|------|------|
| F1 | `biz_contract` / `biz_customer` **完全不存在**；全库 **137 个非模板库**逐一 grep 均无 | `select count(*) from information_schema.tables where table_name in ('biz_contract','biz_snapshot_drift')` → 0（逐库扫描） |
| F2 | 主干业务表存在且结构可用：`biz_supplier`(22 列) / `biz_purchase_order`(27 列) / `biz_stock_in`(28 列) / `biz_payable`(27 列) | P0 段输出 + `information_schema.columns` 计数（取未改动的 `cloudpivot_3b_sql`，四表 `\d` 原文见执行终端记录） |
| F3 | 仓库迁移目录上限 **V0192**，`V0193` 无迁移文件占用（`docs/evidence/*/V0193__erp_contract.up.sql` 是草稿，不是迁移） | `ls backend/migrations/*/ \| grep -oE 'V[0-9]{4}' \| sort -u \| tail -1` → `V0192`；`grep -c V0193` → 0 |
| F4 | 全库 **0 个触发器**（零触发器是本仓基线，`updated_at` 由模型层写） | `information_schema.triggers` → 0 |
| F5 | 全库 82 个外键，但**单据链零外键**：`biz_purchase_order` / `biz_stock_in` / `biz_payable` 各 0 个 | `pg_constraint where contype='f'` 分组统计 → 三表均无行 |
| F6 | 主数据侧确有外键先例：`biz_stock_transfer` 的 3 个仓库列 → `biz_warehouse.id` | pg_constraint 明细 |
| F7 | 金额精度全仓统一 `NUMERIC(18,4)`（PO/IN/PAYABLE 的 `total_amount`、`amount`） | `information_schema.columns` |
| F8 | 币种全仓统一 `CHAR(3)`，默认 `'CNY'` | 同上 |
| F9 | 唯一约束的软删写法已有先例：`uk_biz_po_no ON biz_purchase_order(order_no) WHERE deleted_at IS NULL` | `pg_indexes.indexdef` |
| F10 | 取号器 `biz_seq` 支持 `seq_type/prefix/date_pattern/period_key/current_no/step`，唯一键 `uk_biz_seq_type ... WHERE deleted_at IS NULL` | `\d biz_seq` + V0137 |
| F11 | 现成状态机入口 `business/shared/state_machine.rs`：7 个实体、边表 + reason 边表 + 注册表核对（`transition` 会核对调用方传的 edges 与注册表**完全一致**，不一致给 500） | 源文件 §"边表的唯一真相" |
| F12 | 状态变更日志表 `biz_status_log` 已存在（V0159），字段 `entity_type/entity_id/entity_no/from_status/to_status/operator_id/reason/biz_ref_*`，只增不改 | `\d biz_status_log` |
| F13 | 现成「引用存在性」校验的写法：handler 解析 uuid → `purchase_model::supplier_ref(id)` 查 `biz_supplier where id=? and deleted_at is null` → 缺失时 `reference_missing("supplier_id 供应商不存在或已删除")` | `purchase_handler.rs:246-252`、`purchase_model.rs:747-756` |
| F14 | 业务错误码 ERP 段 `660xx` 已用到 **66012**，`66013+` 空 | `error_code.rs` grep |

**F5 + F6 一起读，才有结论**：本仓不是「一律不建外键」，而是「**单据之间不建外键、主数据可以建**」。合同乙方是**多态引用**（SUPPLIER→供应商表 / CUSTOMER→客户表），PostgreSQL 没有多态外键，且客户表本期不存在 ⇒ 单列 FK 在物理上无法表达这个约束。这不是风格选择，是能力边界。详见 DEC-02。

---

## 1. 设计决策（有争议的点，逐条给结论 + 理由 + 后果）

> 格式统一：**背景 / 选项 / 决定 / 后果**。凡与既有草稿（`docs/evidence/2026-11-02-arch2-contract/V0193__erp_contract.up.sql`）口径不同的，都单独标出，供汇总卡统一（附录 A 有对照表）。

### DEC-01 甲方列名用 `party_a_id`，不用 `party_a_dept_id`

- **背景**：本库没有「公司/法人」表，只有 `sys_dept`。草稿用了 `party_a_dept_id`。
- **选项**：A) `party_a_dept_id`（语义最直白，但与乙方 `party_b_id` 不对称）；B) `party_a_id`（与乙方对称，任务书原文用这个名）。
- **决定**：**`party_a_id`**。语义固定为：`NULL` = 本组织本体（不挂任何部门）；非 `NULL` = `sys_dept.id`。同时加 `party_a_name` 快照。
- **后果**：任务书字段清单可直接对齐；代价是列名不自我解释，靠 `COMMENT ON COLUMN` 补足（已写进 DDL）。若其他章节已用 `party_a_dept_id`，以本章为准改名 —— 合同表只有一张，口径只能有一个。

### DEC-02 乙方**不建外键**，用「应用层写入校验 + 每日巡检」双轨

- **背景**：F5/F6 说明本仓对单据链不建外键；`party_b_id` 是多态引用。
- **选项**：
  - A) 建两个可空列 `supplier_id` / `customer_id` 各带 FK —— 能建 FK，但乙方变成「二选一的两列」，所有查询、索引、快照都要写两遍，且与任务书要求的 `party_b_id + party_b_type` 形状冲突。
  - B) 单列 `party_b_id` + 无 FK（弱引用），存在性由应用层与巡检保证。
  - C) 单列 + 触发器做跨表存在性校验。
- **决定**：**B**。理由三条：① 多态引用 FK 在 PG 无法表达（A 是把多态摊平成两列，代价大于收益）；② 客户表本期不存在，FK 根本无处指；③ F5 已确立「单据链弱引用」基线，合同属于业务单据层，与 PO/IN/PAYABLE 同层，口径应一致。
- **后果**：数据库层**允许**写入孤儿 `party_b_id`（POC P5 已实证：`deadbeef-...` 能写进去）。因此 V-11（写入校验）与 V-22（孤儿巡检）是**必需项而非增强项**，两张卡都要做；**不允许**因为「已建表」就认为约束已到位。
- **C 方案不采用**：见 §3.4。

### DEC-03 金额精度取 `NUMERIC(18,4)`，与全仓对齐（否决草稿的 20,4）

- **背景**：F7 全仓金额是 18,4；草稿写 20,4。
- **决定**：**18,4**。上限 99,999,999,999,999.9999（约 10^14），远超任何单份合同。
- **后果**：合同额与 PO `total_with_tax`、应付 `amount` 精度一致，**累计下单额与合同额可以直接比较，不需要任何 scale 转换**；若取 20,4，则「合同 20,4 vs 单据 18,4」在超限时会出现「合同写得下、单据写不下」的金额，属于人为引入的跨表精度不对称。

### DEC-04 `amount > 0` 是硬约束；「0 元 / 不限额框架合同」本批次**不支持**

- **背景**：任务书要求 `amount > 0`；草稿写 `amount >= 0` 并用「0 = 不限额」表达框架合同。
- **决定**：**`CHECK (amount > 0)`**。框架/不限额合同不在本批次建模。
- **理由**：`amount = 0` 会让「已下单额 / 合同额」这一核心可用性校验（V-21）除零，必须在代码里到处写 `if amount > 0` 分支 —— 一个「合法数据」把下游全部拖进特判，是典型的模型缺陷前置。正确做法是新增维度（`limit_type IN ('FIXED','UNLIMITED')`），而不是让 0 兼职「无穷大」。
- **后果**：本期只能建定额合同。批次 3 若需框架合同，加 `limit_type` 列 + 调整 V-21，**不改既有数据**（现有合同全是 FIXED）。已列入 §7 不做清单。
- **POC 证据**：`V-04a PASS amount=0 被拦 [23514] ck_biz_contract_amount`。

### DEC-05 状态机比任务书多一条边：`SIGNED → TERMINATED`

- **背景**：任务书给的链是 `DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED`（4 条边）。
- **选项**：A) 严格 4 条边 —— 已签但从未生效的合同要终止，必须**先点 ACTIVE 再终止**；B) 补 `SIGNED→TERMINATED`。
- **决定**：**B**（5 条边）。
- **理由**：A 会强迫用户把「从未生效」的合同先写成「已生效」，`biz_status_log` 里留下一条从未真实发生的 `SIGNED→ACTIVE`。审计要的是「当时是什么」，不是「为了走流程补出来的什么」。这是**用状态机形状伪造审计事实**，不可接受。
- **后果**：本章的边表是 5 条（§4.1），禁止边 15 条（§4.2）逐条列出，实现卡照抄成单测。汇总卡需同步：其他章节若写 4 条边，以本章为准。

### DEC-06 合同号：DB 只校验字符集，格式由取号器保证

- **背景**：`contract_no` 若用 `CHECK (~ '^HT-[0-9]{8}-[0-9]{6}$')` 能钉死格式，但二开客户常有「沿用旧合同号」的诉求（如 `OLD-2019-001`），一旦需要就只能改 CHECK（迁移成本 > 收益）。
- **决定**：DB 层 `CHECK (contract_no ~ '^[A-Za-z0-9][A-Za-z0-9_-]{5,31}$')`（禁空白/中文/符号，长度 6–32）；格式 `HT-yyyyMMdd-000001` 由 `biz_seq(seq_type='CT')` 生成器保证。
- **后果**：取号器写错了 DB 拦不住 → 由单测（取号器单测 + 唯一索引兜底）覆盖；换来的好处是存量/外部合同号可原样录入。

### DEC-07 合同在 `SIGNED` 之后**冻结**乙方与金额，不提供"合同变更单"

- **背景**：合同变更（尤其换乙方）是真实业务。草稿只讨论了「变更后下游快照会漂移、靠巡检发现」。
- **选项**：A) 允许随时改，靠巡检兜底；B) 变更走「变更单」（新表 + 审批 + 版本链）；C) 状态锁：`DRAFT` 全可改，`SIGNED` 起冻结关键字段，要改就「终止原合同 + 新签合同」。
- **决定**：**C**。冻结字段矩阵见 §5.3。
- **理由**：A 把数据正确性押在「巡检能发现」上，而巡检是 T+1 的，发现时凭证已经记账；B 是一张新表 + 一套审批 + 一条版本链，本批次（3 小时设计窗 + 后续实现卡）吃不下，且与现行"合同只是参照物、下游单据才是事实"的定位不符；C 用**已经存在**的终止/新建能力表达变更，零新表、零新流程，且「终止一张 + 新签一张」在审计上比「原地改乙方」清楚得多。
- **后果**：
  - 换乙方只可能发生在 `DRAFT`，此时通常还没有下游单据 ⇒ **正常路径下不存在漂移**；
  - 残留漂移只可能来自：历史数据修复、直改库、供应商主档改名。§6 按来源分别给判定与巡检；
  - 变更单需求 → §7 不做清单，作为批次 3 候选。

### DEC-08 CUSTOMER 分支本期「结构留位、写入降级」

- **背景**：客户表属批次 2；任务书要求明确写出降级策略与替换点。
- **决定**：表结构、枚举、配对 CHECK、索引**全部按两分支建好**；应用层在**唯一一个校验函数**里拒绝 `party_b_type='CUSTOMER'` 的写入（400 / 66019），前端不展示需要客户的合同类型组合。
- **理由**：如果本期把 `party_b_type` 只留 `SUPPLIER`，批次 2 要改 CHECK 约束（`ALTER TABLE ... DROP/ADD CONSTRAINT` 在生产上是锁表动作）+ 改枚举 + 改前端联动，一处漏改就是脏数据。留位成本 = 0，替换成本 = 删一个 `if`。
- **后果 / 替换点**（批次 2 客户表落地后，四处，精确到函数）见 §3.2.3。

### DEC-09 合同的 `dept_id` 承担数据权限，`owner_id` 承担"跟单人"

- **背景**：仓内数据权限按部门（`shared/scope.rs`），`biz_supplier` 没有 dept 维度。
- **决定**：`dept_id` 可空（NULL = 全公司可见，由权限层规则决定），用于列表数据权限过滤；`owner_id` 是负责人，用于「我负责的合同」筛选与提醒收件人。两者**都不建外键**（与仓内 `create_by` 等弱引用一致）。
- **后果**：`dept_id` 不建索引（合同量级 10^4，全表扫可接受），标注为可选优化；若后续按部门过滤成为热点，再加 `(dept_id, status) WHERE deleted_at IS NULL`。

### DEC-10 状态推进不走「改 `status` 列」的普通 update

- **背景**：F11 的 `state_machine::transition` 是全仓唯一入口，且会核对边表注册；F12 的 `biz_status_log` 与业务变更同事务。
- **决定**：合同状态变更**必须**经 `state_machine::transition(Entity::Contract, ...)`，模型层 `update` 不提供 `status` 字段写入路径。合同 CRUD 的 `PUT` 只改基础信息，状态走独立 `POST /contract/{id}/status`。
- **后果**：需要在 `Business/shared/state_machine.rs` 同步改**七处**（`Entity::Contract` 变体、`ALL`、`as_str`、`label`、`edges()`、`reason_edges()`、`locked_statuses()`），漏一处会在运行期拿到 500 而不是静默放行（这是该文件的设计意图，不是缺陷）。清单见附录 B。

### 与既有草稿的 4 处口径差异（汇总卡必须收敛）

| # | 项 | 既有草稿 `V0193__erp_contract.up.sql` | 本章 | 理由 |
|---|---|---|---|---|
| 1 | 甲方列 | `party_a_dept_id` | `party_a_id` | DEC-01 |
| 2 | 金额精度 | `NUMERIC(20,4)` | `NUMERIC(18,4)` | DEC-03 |
| 3 | 金额下限 | `amount >= 0`（0=不限额） | `amount > 0` | DEC-04 |
| 4 | 单个迁移文件范围 | 合同 + PO 三列 + 漂移表 + 两个视图，一锅端在 V0193 | 拆 V0193(合同)/V0194(PO 列)/V0195(漂移表)；视图归 §4 巡检章 | 一次迁移一个关注点，出问题可单独回滚；V0193 只含本章 DDL |

---

## 2. 数据模型

### 2.1 字段字典（23 列）

| # | 列 | 类型 | 空 | 默认 | 语义与取值 | 备注 |
|---|---|---|---|:--:|---|---|
| 1 | `id` | `UUID` | NOT NULL | `gen_random_uuid()` | 主键 | 与全仓一致 |
| 2 | `contract_no` | `VARCHAR(32)` | NOT NULL | — | 合同号，`HT-yyyyMMdd-000001` | 软删后可复用（DEC-06、§2.3） |
| 3 | `contract_name` | `VARCHAR(200)` | NOT NULL | — | 合同名称 | 与 `supplier_name(200)` 同宽 |
| 4 | `contract_type` | `VARCHAR(16)` | NOT NULL | — | `PURCHASE` 采购 / `SALES` 销售 / `SERVICE` 服务 / `LEASE` 租赁 | 决定乙方类型（V-03） |
| 5 | `party_a_id` | `UUID` | NULL | — | 甲方：`NULL`=本组织本体；非空=`sys_dept.id` | DEC-01；弱引用 |
| 6 | `party_a_name` | `VARCHAR(200)` | NULL | — | 甲方名称**快照** | 与 `party_a_id` 同生同灭（V-10） |
| 7 | `party_b_type` | `VARCHAR(16)` | NOT NULL | — | `SUPPLIER` / `CUSTOMER` | 决定 `party_b_id` 指向哪张表 |
| 8 | `party_b_id` | `UUID` | NOT NULL | — | 乙方 id | **唯一真相**；弱引用（DEC-02） |
| 9 | `party_b_name` | `VARCHAR(200)` | NOT NULL | — | 乙方名称**快照**（签约时） | 主档改名不回溯（§6.2） |
| 10 | `amount` | `NUMERIC(18,4)` | NOT NULL | — | 合同金额（不含税口径，与 PO `total_amount` 同口径） | `> 0`（DEC-04） |
| 11 | `currency_code` | `CHAR(3)` | NOT NULL | `'CNY'` | ISO 4217 | 与 PO/IN/PAYABLE 同型 |
| 12 | `sign_date` | `DATE` | NULL | — | 签订日期 | `status>=SIGNED` 必填（V-08） |
| 13 | `start_date` | `DATE` | NULL | — | 生效日 | `status>=SIGNED` 必填；`<= end_date` |
| 14 | `end_date` | `DATE` | NULL | — | 到期日 | 下游建单必须落在 `[start,end]`（V-21） |
| 15 | `status` | `VARCHAR(16)` | NOT NULL | `'DRAFT'` | `DRAFT/SIGNED/ACTIVE/COMPLETED/TERMINATED` | 只能经状态机改（DEC-10） |
| 16 | `owner_id` | `UUID` | NULL | — | 负责人 `sys_user.id` | 弱引用 |
| 17 | `dept_id` | `UUID` | NULL | — | 归属部门 `sys_dept.id` | 数据权限过滤（DEC-09） |
| 18 | `remark` | `TEXT` | NULL | — | 备注；**终止原因也写这里**（V-19） | 不为一次终止新增列 |
| 19 | `created_at` | `TIMESTAMPTZ` | NOT NULL | `CURRENT_TIMESTAMP` | 创建时间 | 与全仓同 |
| 20 | `updated_at` | `TIMESTAMPTZ` | NOT NULL | `CURRENT_TIMESTAMP` | 更新时间 | **模型层写**，无触发器（F4） |
| 21 | `create_by` | `UUID` | NULL | — | 创建人 | 弱引用 |
| 22 | `update_by` | `UUID` | NULL | — | 最后修改人 | 弱引用 |
| 23 | `deleted_at` | `TIMESTAMPTZ` | NULL | — | 软删除时间 | 唯一索引的部分谓词来源 |

**为什么没有这些列**（防止实现卡「顺手加」）：

| 候选列 | 不加的理由 |
|---|---|
| `parent_contract_id` / `version` | 合同变更走「终止 + 新签」（DEC-07），版本链是批次 3 的事 |
| `approval_instance_id` | 合同审批流明确不在本卡范围（§7）；加列不难，但会诱导实现卡顺手接审批，与批次规划冲突 |
| `file_ids` / 附件 | §7 不做 |
| `used_amount`（已下单额缓存） | 违反「单一数据源」：累计值必须从 PO 现算（V-21），缓存列一定会漂移 |
| `supplier_id`（冗余乙方） | 与 `party_b_id` 同义重复，正是本次重构要消灭的东西 |
| `party_b_code`（编码快照） | 编码可改、名称才是人读的；快照名称已足够 |

### 2.2 完整 DDL（PostgreSQL，可原样执行）

> 与 `model-poc.sql` 的 `#region DDL` 段落**逐字一致**（POC 已实跑，exit=0）。

```sql
CREATE TABLE biz_contract (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_no          VARCHAR(32)   NOT NULL,
    contract_name        VARCHAR(200)  NOT NULL,
    contract_type        VARCHAR(16)   NOT NULL,
    party_a_id           UUID,
    party_a_name         VARCHAR(200),
    party_b_type         VARCHAR(16)   NOT NULL,
    party_b_id           UUID          NOT NULL,
    party_b_name         VARCHAR(200)  NOT NULL,
    amount               NUMERIC(18,4) NOT NULL,
    currency_code        CHAR(3)       NOT NULL DEFAULT 'CNY',
    sign_date            DATE,
    start_date           DATE,
    end_date             DATE,
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    owner_id             UUID,
    dept_id              UUID,
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_contract_type
        CHECK (contract_type IN ('PURCHASE','SALES','SERVICE','LEASE')),
    CONSTRAINT ck_biz_contract_party_b_type
        CHECK (party_b_type IN ('SUPPLIER','CUSTOMER')),
    CONSTRAINT ck_biz_contract_type_party_pair
        CHECK (CASE contract_type
                 WHEN 'PURCHASE' THEN party_b_type = 'SUPPLIER'
                 WHEN 'SALES'    THEN party_b_type = 'CUSTOMER'
                 ELSE party_b_type IN ('SUPPLIER','CUSTOMER')
               END),
    CONSTRAINT ck_biz_contract_amount
        CHECK (amount > 0),
    CONSTRAINT ck_biz_contract_date_order
        CHECK (start_date IS NULL OR end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_contract_signed_dates
        CHECK (CASE WHEN status IN ('SIGNED','ACTIVE','COMPLETED')
                    THEN sign_date IS NOT NULL AND start_date IS NOT NULL AND end_date IS NOT NULL
                    ELSE TRUE END),
    CONSTRAINT ck_biz_contract_status
        CHECK (status IN ('DRAFT','SIGNED','ACTIVE','COMPLETED','TERMINATED')),
    CONSTRAINT ck_biz_contract_currency
        CHECK (currency_code ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_contract_no_charset
        CHECK (contract_no ~ '^[A-Za-z0-9][A-Za-z0-9_-]{5,31}$')
);

COMMENT ON TABLE  biz_contract IS '合同（采购/销售/服务/租赁）：乙方是供应商/客户的唯一真相；下游单据只引用合同，不各自维护乙方';
COMMENT ON COLUMN biz_contract.contract_no       IS '合同号：biz_seq(seq_type=CT) 取号，格式 HT-yyyyMMdd-000001；DB 只校验字符集与长度';
COMMENT ON COLUMN biz_contract.contract_name     IS '合同名称';
COMMENT ON COLUMN biz_contract.contract_type     IS 'PURCHASE 采购 / SALES 销售 / SERVICE 服务 / LEASE 租赁';
COMMENT ON COLUMN biz_contract.party_a_id        IS '甲方：NULL=本组织本体；非空=sys_dept.id（弱引用，本库无“公司”表）';
COMMENT ON COLUMN biz_contract.party_a_name      IS '甲方名称【快照】：签约时名称，随合同冻结';
COMMENT ON COLUMN biz_contract.party_b_type      IS '乙方类型：SUPPLIER→biz_supplier / CUSTOMER→biz_customer（客户表属批次2）';
COMMENT ON COLUMN biz_contract.party_b_id        IS '乙方 id：弱引用（非外键）。存在性由应用层写入校验 + 每日巡检兜底';
COMMENT ON COLUMN biz_contract.party_b_name      IS '乙方名称【快照】：签约时名称；主档改名不回溯历史合同（审计要“当时是谁”）';
COMMENT ON COLUMN biz_contract.amount            IS '合同金额（不含税口径与采购单 total_amount 一致），精度 NUMERIC(18,4)；必须 > 0';
COMMENT ON COLUMN biz_contract.currency_code     IS '币种 ISO 4217 三位大写';
COMMENT ON COLUMN biz_contract.sign_date         IS '签订日期；status>=SIGNED 时必填';
COMMENT ON COLUMN biz_contract.start_date        IS '生效日；status>=SIGNED 时必填，且 <= end_date';
COMMENT ON COLUMN biz_contract.end_date          IS '到期日；下游建单必须落在 [start_date, end_date] 内';
COMMENT ON COLUMN biz_contract.status            IS '状态机：DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED，另有 SIGNED→TERMINATED；边表在应用层';
COMMENT ON COLUMN biz_contract.owner_id          IS '负责人（弱引用 sys_user.id）';
COMMENT ON COLUMN biz_contract.dept_id           IS '归属部门（弱引用 sys_dept.id），数据权限过滤用';
COMMENT ON COLUMN biz_contract.updated_at        IS '最后更新时间：由模型层写入（本库零触发器，不靠 DB 自动维护）';
COMMENT ON COLUMN biz_contract.deleted_at        IS '软删除时间；合同号唯一约束只对未删除行生效（软删后可复用号码，审计仍可查旧行）';

CREATE UNIQUE INDEX uk_biz_contract_no
    ON biz_contract (contract_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_party_b
    ON biz_contract (party_b_id, sign_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_status
    ON biz_contract (status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_end
    ON biz_contract (end_date) WHERE deleted_at IS NULL;

INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '合同号：HT-yyyyMMdd-000001，按天滚动，与 PO/IN/TR/SN 同款取号器'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL);
```

**结构自检结果（POC P1/P6 实测）**：23 列 / 9 个 CHECK / 5 个索引（含主键） / 1 条 `CT` 取号规则 / `biz_contract` 作为外键目标 0 个。

### 2.3 软删除下的唯一约束与索引

**唯一约束的正确写法**（GENERATED 草稿里的 `unique(contract_no) where deleted_at is null` 是语法错误，PG 不支持在建表语句里给 UNIQUE 带谓词）：

```sql
-- ✅ 部分唯一索引：只用活跃行建唯一性，软删行不进索引
CREATE UNIQUE INDEX uk_biz_contract_no ON biz_contract (contract_no) WHERE deleted_at IS NULL;

-- ❌ 下面三种都写过或见过，全部不合格：
--   UNIQUE (contract_no) WHERE deleted_at IS NULL   -- 建表语法错误，PG 直接报错
--   UNIQUE (contract_no, deleted_at)                -- 同一号能因 deleted_at 不同重复插入多次，软删后还能再删一次
--   UNIQUE (contract_no)                            -- 软删后号码永久不可复用，与「审计留旧行」冲突
```

**语义（POC P3 实证）**：

| 场景 | 结果 |
|---|---|
| 活跃行重号 | `23505` / `uk_biz_contract_no`（PASS） |
| 软删后同号重建 | 允许；表里出现两条同号行，`deleted_at` 一有一无 |
| 审计 | 旧行仍在，`select * where contract_no='HT-...' order by created_at` 能看到「作废 → 重签」全过程 |

> 为什么软删后**允许**复用号码：合同号是业务对外编号，作废的号若永久占用，取号器要回退或跳号，反而更难审计。允许复用 + 保留旧行，是「唯一性只对现状生效，历史全部保留」的标准做法。

**索引清单与理由**：

| 索引 | 定义 | 服务的查询 | 为什么这样建 |
|---|---|---|---|
| `uk_biz_contract_no` | UNIQUE `(contract_no) WHERE deleted_at IS NULL` | 按号精确查、取号器查重 | 见上 |
| `idx_biz_contract_party_b` | `(party_b_id, sign_date DESC) WHERE deleted_at IS NULL` | 「某供应商/客户的全部合同」、合同详情页关联、§6 巡检 | 前导列 `party_b_id` 满足必备要求；第二列让列表天然按签约倒序出，省一次排序 |
| `idx_biz_contract_status` | `(status) WHERE deleted_at IS NULL` | 按状态筛选、`status IN ('SIGNED','ACTIVE')` 可用性判定 | 必备项；合同基数小（10^4），单列足够，不做 `(status, xxx)` 复合以免写入放大 |
| `idx_biz_contract_end` | `(end_date) WHERE deleted_at IS NULL` | 到期提醒（`end_date between now() and now()+30d`）、逾期扫描 | 必备项；不用 `WHERE status='ACTIVE'` 收窄，因为 `SIGNED` 也参与到期提醒，谓词太窄会让查询走不上索引 |

- 三条必备索引都带 `WHERE deleted_at IS NULL`：软删行永不参与业务查询，索引更小更热；且 `uuid = 常量` 是严格算子，PG 能证明「列=值 ⇒ 列 IS NOT NULL」，部分索引对等值/反连接同样有效（V0186 已有同款论证与本仓实测）。
- **未建**：`dept_id` 索引（DEC-09，量级不需要）、`owner_id` 索引（同上）、`(contract_type, status)` 复合索引（列表页筛选组合多，先不猜，等真实慢查询）。

**为什么不建外键（附反证）**：POC 直接往 `biz_contract` 写了一个指向不存在供应商的 `party_b_id`（`deadbeef-...`），**写入成功**；随后孤儿巡检 SQL 精准捞出这一行。这就是 DEC-02 的代价与配套：**DB 不拦，应用层与巡检必须拦**。

### 2.4 合同号取号

- 规则：`biz_seq.seq_type='CT'`、`prefix='HT'`、`date_pattern='yyyyMMdd'`、`separator='-'` ⇒ `HT-20261102-000001`，按天滚动归零。
- 为什么复用 `biz_seq` 而不是自增/序列：全仓 5 种单据（PO/IN/TR/SN/PAY）都用它，取号是「单语句 `UPDATE ... RETURNING` 原子推进」，行锁天然防重号（V0137 表注释原文）。新增 CT 只是插一行规则，零新机制。
- 种子用 `INSERT ... SELECT ... WHERE NOT EXISTS` 保证幂等（POC 复跑不会报唯一冲突）。

---

## 3. 校验规则清单（每条注明执行层 + 失败语义）

分层原则（先说清楚，避免实现卡乱放）：

```
DB CHECK      → 单行内、与外部无关的事实（枚举、区间、精度、必填联动）
应用层        → 需要读别的表 / 需要区分操作人 / 需要给业务话术的判断
触发器        → 本仓基线是 0 个，只有「DB 层不拦就会造成不可逆损伤」才考虑（本章结论：不采用）
定时巡检      → T+1 可发现、不需要阻断写入的一致性（孤儿引用、跨表漂移）
```

### 3.1 DB 级（`CHECK` / 唯一索引；失败 → SQLSTATE `23514` / `23505`，HTTP 500 由框架兜底包装）

> 定位：这是**最后一道网**，不是给用户看的错误提示。正常路径应先被应用层挡下并给出 400/409 业务码；DB 报错说明应用层漏了校验，属于缺陷。

| 编号 | 规则 | 约束名 | 失败 SQLSTATE | POC 证据 |
|---|---|---|---|---|
| V-01 | `contract_type ∈ (PURCHASE,SALES,SERVICE,LEASE)` | `ck_biz_contract_type` | 23514 | PASS |
| V-02 | `party_b_type ∈ (SUPPLIER,CUSTOMER)` | `ck_biz_contract_party_b_type` | 23514 | PASS |
| V-03 | 类型与乙方类型配对：`PURCHASE→SUPPLIER`、`SALES→CUSTOMER`、`SERVICE/LEASE→任一` | `ck_biz_contract_type_party_pair` | 23514 | PASS（两个方向都测） |
| V-04 | `amount > 0`（0 与负数都拒） | `ck_biz_contract_amount` | 23514 | PASS（两条） |
| V-05 | `start_date <= end_date`（任一为空不判） | `ck_biz_contract_date_order` | 23514 | PASS |
| V-06 | `status ∈ 5 值` | `ck_biz_contract_status` | 23514 | PASS |
| V-07 | `currency_code ~ '^[A-Z]{3}$'`（小写一律拒） | `ck_biz_contract_currency` | 23514 | PASS |
| V-08 | `status ∈ (SIGNED,ACTIVE,COMPLETED)` ⇒ `sign_date/start_date/end_date` 全部非空 | `ck_biz_contract_signed_dates` | 23514 | PASS |
| V-09 | `contract_no` 字符集 `[A-Za-z0-9_-]`、首字符非符号、长度 6–32 | `ck_biz_contract_no_charset` | 23514 | PASS |
| V-10 | 活跃行 `contract_no` 唯一 | `uk_biz_contract_no` | 23505 | PASS |

**V-08 是本章新增的「状态与事实不许互相矛盾」约束**：`status='SIGNED'` 却没有任何日期，意味着「已签但不知道何时签、何时生效」——这种行会让下游的期限校验（V-21）直接失效（`order_date between null and null` 恒为 null ⇒ 校验被静默跳过）。用 `CASE ... ELSE TRUE` 写法，`DRAFT`/`TERMINATED` 不受影响（终止可能发生在没有到期日的情况下）。

### 3.2 应用层（handler + model；给出 HTTP + 业务码 + 中文话术）

业务错误码新增段：`66013–66020`（F14 实测 `66013+` 空）。定义建议（写进 `shared/error_code.rs`）：

```text
66013 CONTRACT_NOT_USABLE          409 合同不可用于建单（状态/期限/已删除）
66014 CONTRACT_STATUS_ILLEGAL      409 合同状态不允许该流转
66015 CONTRACT_FIELD_LOCKED        409 合同已签订，该字段不可修改
66016 CONTRACT_NO_DUPLICATED       409 合同号已存在
66017 CONTRACT_IN_USE              409 合同已被下游单据引用，不能删除
66018 CONTRACT_AMOUNT_EXCEEDED     409 累计下单金额将超出合同金额
66019 CONTRACT_CUSTOMER_UNSUPPORTED 400 客户类合同暂未开放（客户主数据未上线）
66020 CONTRACT_PARTY_INVALID       400 乙方不存在或已停用
```

| 编号 | 规则 | 执行位置（函数级） | 失败语义 |
|---|---|---|---|
| V-11 | 乙方存在性：`party_b_type='SUPPLIER'` ⇒ `party_b_id` 必须命中 `biz_supplier(id, deleted_at IS NULL)`；**新建时**额外要求 `status = 1`（停用供应商不得新签） | `contract_handler::ensure_party_b` → `contract_model::party_b_ref(type,id)`（照抄 F13 的 `supplier_ref` 写法） | 400 / `66020` / 「乙方供应商不存在或已停用」。停用只在**新建**校验；修改其它字段不重验（否则供应商一停用，连备注都改不了） |
| V-12 | CUSTOMER 降级：`party_b_type='CUSTOMER'` ⇒ 直接拒绝，不查任何表 | 同上函数第一行 | 400 / `66019` / 「客户类合同暂未开放（客户主数据未上线）」 |
| V-13 | 甲方存在性：`party_a_id` 非空 ⇒ `sys_dept.id` 存在且 `deleted_at IS NULL`；同时把 `sys_dept.dept_name` 写入 `party_a_name` | `ensure_party_a` | 400 / `PARAM_INVALID(10001)` / 「甲方部门不存在」 |
| V-14 | `party_a_id` 与 `party_a_name` 同生同灭（有 id 必有 name，反之亦然） | `ensure_party_a` | 400 / `10001` / 「甲方信息不完整」（DB 未建配对 CHECK：允许「只有 id、无名称」的存量/导入行；写入路径必须成对，靠本规则） |
| V-15 | 乙方名称快照：创建时 `party_b_name` **由服务端从主档读取**覆盖前端传值（不接受前端自填） | `ensure_party_b` 返回 `(id, name)` → 直接落库 | 400 / `66020`（读不到即不存在） |
| V-16 | `owner_id` / `dept_id` 非空时必须存在（`sys_user` / `sys_dept`） | `ensure_owner_dept` | 400 / `10001` / 「负责人或部门不存在」 |
| V-17 | 状态流转合法性（边表见 §4）；自环一律 409 | `state_machine::transition(Entity::Contract, from, to, reason)` | 409 / `66014` / 「合同当前状态 X 不允许变更为 Y」 |
| V-18 | 终止必须给原因：`to='TERMINATED'` ⇒ `reason` 非空且 ≤ 200 字，写入 `biz_status_log.reason` 与 `remark` | `contract_handler::change_status` | 400 / `10001` / 「请填写终止原因」 |
| V-19 | 已签字段冻结：`status <> 'DRAFT'` 时 `party_b_type/party_b_id/party_b_name/contract_type/amount/currency_code/sign_date/start_date/end_date` 任一发生变化 ⇒ 拒绝（矩阵见 §5.3） | `contract_handler::update` 先读旧行比对 | 409 / `66015` / 「合同已签订，乙方/金额/期限不可修改；如需变更请终止后重新签订」 |
| V-20 | 合同号冲突：捕获 `23505` 转业务码，不把 SQLSTATE 透给前端 | `contract_model` 写库处 | 409 / `66016` / 「合同号 HT-xxx 已存在」 |
| V-21 | 删除守卫：存在未关闭下游单据（`biz_purchase_order.contract_id = ? AND status NOT IN ('CANCELLED','CLOSED') AND deleted_at IS NULL`）时禁止软删 | `contract_handler::delete` | 409 / `66017` / 「该合同下仍有 N 张未关闭采购订单，请先处理」 |
| V-22 | 采购单引用合同的可用性（**由 PO 侧执行**，合同侧只提供判定函数） | `purchase_handler` 建单/换合同处，规则见 §5.1 | 409 / `66013` / 「合同已终止/未生效/已过期/已删除，不能下单」 |
| V-23 | 累计下单额不得超合同额：`Σ PO.total_with_tax(未取消) + 本单 ≤ amount`；**同一事务内**先 `SELECT id FROM biz_contract WHERE id=? FOR UPDATE` 再求和 | `purchase_handler` + `purchase_model::sum_open_amount` | 409 / `66018` / 「累计下单 12,500.00 将超过合同金额 10,000.00」 |

**V-23 的并发坑（必须写进实现卡）**：先查后写不是原子的，两个并发请求各自看到「还剩 1 万」会双双放行。合同行 `FOR UPDATE` 是最小代价的串行化（合同是天然聚合根，冲突面小），**不要**用「事后巡检」代替 —— 超签一旦生成 PO 并入库，回滚成本远高于一次行锁。

#### 3.2.3 CUSTOMER 分支：临时策略与批次 2 替换点（任务书点名要）

**临时策略（本期 P0）**

1. **写入硬拒**：`party_b_type='CUSTOMER'` ⇒ 400 / `66019`。**不做**「先存 id 后补主档」——那会制造一批永远无法校验的孤儿数据，且等客户表上线时无人知道该把哪条 id 认领成客户。
2. **前端降级**：合同类型下拉只开放 `PURCHASE`，以及「乙方=供应商」的 `SERVICE` / `LEASE`；`SALES` 与「乙方=客户」的 `SERVICE/LEASE` 置灰并给 tooltip「客户主数据上线后开放」。**不做**假客户选项。
3. **结构留位**：`party_b_type` 枚举、`ck_biz_contract_type_party_pair` 配对 CHECK、`idx_biz_contract_party_b` 索引全部按两分支建好（DEC-08）。
4. **不发种子数据**：不造任何示例客户/客户合同（违反「不编造审计事实」）。

**批次 2 替换点（客户表落地后，改 4 处，表结构 0 改动）**

| # | 位置 | 改动 | 风险 |
|---|---|---|---|
| R1 | `contract_handler::ensure_party_b` | 删掉 `if type == 'CUSTOMER' → 66019` 这一行 | 无（唯一闸口） |
| R2 | `contract_model::party_b_ref` | 增加 `CUSTOMER` 分支：查 `biz_customer(id, deleted_at IS NULL, status = 1)` 返回 `customer_name` | 需保证客户表有 `status`/`deleted_at`；若命名不同（如 `customer_status`），在本函数内做映射，**不外溢** |
| R3 | 巡检 SQL（§6.3 的 Q1） | `UNION ALL` 一条 CUSTOMER 分支 | 无 |
| R4 | 前端合同表单 | 放开 `SALES`、放开「乙方=客户」联动，选择器切 `api/business/erp/customer` | 需批次 2 的客户列表接口先就绪 |

> 替换点的价值：**本期一块砖都没铺死**。R1 是删一行，R2 是加一个分支，R3 是加一个 `UNION`，R4 是放开前端。没有任何 `ALTER TABLE`、没有数据修复、没有历史脏数据要认领。

### 3.3 巡检（T+1，非阻断；落地归 t_c7610b31，本章给检出 SQL 与判定口径）

| 编号 | 巡检项 | 判定 | 承载表/卡 |
|---|---|---|---|
| Q1 | 孤儿乙方：`SUPPLIER` 分支 `party_b_id` 在 `biz_supplier` 不存在/已软删 | `DEFECT / ERROR`（写入校验漏了或主档被硬删） | `biz_snapshot_drift`，t_c7610b31 |
| Q2 | 合同「已生效却已过期」：`status='ACTIVE' AND end_date < current_date` | `INFO / WARN`（不是数据错误，是待办提醒源） | 提醒任务（不改状态，判定见 §5.1） |
| Q3 | 超签：`Σ PO(未取消) > amount` | `DEFECT / ERROR` | t_c7610b31 |
| Q4 | 下游快照漂移：`PO.supplier_id ≠ contract.party_b_id` | `SNAPSHOT`（合法历史）或 `DEFECT`（拷贝断裂），判定见 §6.2 | t_c7610b31 + t_52f3cb32 |

Q1 的检出 SQL（POC 实测：精准捞出 1 行人为孤儿，0 误报）：

```sql
SELECT c.id, c.contract_no, c.party_b_id
  FROM biz_contract c
 WHERE c.deleted_at IS NULL
   AND c.party_b_type = 'SUPPLIER'
   AND NOT EXISTS (SELECT 1 FROM biz_supplier s
                    WHERE s.id = c.party_b_id AND s.deleted_at IS NULL);
```

Q3 的检出 SQL：

```sql
SELECT c.id, c.contract_no, c.amount, coalesce(sum(o.total_with_tax), 0) AS ordered
  FROM biz_contract c
  LEFT JOIN biz_purchase_order o
         ON o.contract_id = c.id AND o.deleted_at IS NULL AND o.status <> 'CANCELLED'
 WHERE c.deleted_at IS NULL AND c.status IN ('SIGNED','ACTIVE','COMPLETED')
 GROUP BY c.id, c.contract_no, c.amount
HAVING coalesce(sum(o.total_with_tax), 0) > c.amount;
```

### 3.4 触发器方案评估（结论：不采用）

任务书要求把「触发器」作为可选落地层评估，这里给方案与否决理由，**不留给实现卡自由发挥**。

候选方案（可行，但本章否决）：

```sql
-- 【不采用】乙方存在性触发器（示意，勿落库）
CREATE OR REPLACE FUNCTION trg_biz_contract_party_b_check() RETURNS trigger AS $$
BEGIN
    IF NEW.party_b_type = 'SUPPLIER' THEN
        IF NOT EXISTS (SELECT 1 FROM biz_supplier
                        WHERE id = NEW.party_b_id AND deleted_at IS NULL) THEN
            RAISE EXCEPTION '乙方供应商不存在: %', NEW.party_b_id;
        END IF;
    ELSIF NEW.party_b_type = 'CUSTOMER' THEN
        RAISE EXCEPTION '客户主数据未上线';   -- 批次2 换成 biz_customer 查询
    END IF;
    RETURN NEW;
END $$ LANGUAGE plpgsql;

CREATE TRIGGER trg_biz_contract_party_b BEFORE INSERT OR UPDATE OF party_b_id, party_b_type
    ON biz_contract FOR EACH ROW EXECUTE FUNCTION trg_biz_contract_party_b_check();
```

否决理由（按权重排序）：

1. **它救不了「已存在的数据」**：触发器只在写入时生效；本项目的数据漂移来源是「主档删了/直改库/存量导入」，这些路径不经过触发器。真正兜底的是巡检（§3.3 Q1）。
2. **与本仓基线冲突**：F4 实测全库 0 触发器。一旦合同表开了先例，PO/IN/PAYABLE/VOUCHER 每张表都会被要求「同样的保护」，最后变成触发器森林，迁移/数据修复/批量导入全部踩坑（`ALTER TABLE ... DISABLE TRIGGER` 会出现在每一份运维脚本里）。
3. **错误语义不可控**：触发器只能抛 `P0001`，HTTP 层拿不到业务码，最终表现成 500 —— 而同一规则在应用层是 400 + 中文话术。同一个错误两套话术，前端没法处理。
4. **性能与批量**：2 万行导入 × 2 次主档 lookup，且无法用 `JOIN` 优化。本仓真有 2 万行级的单据导入场景（P11 批量生成）。
5. **客户分支无处指**：本期 `biz_customer` 不存在，触发器里只能硬编码「拒绝」，与 DEC-08 的应用层闸口重复。

**唯一会重新考虑的场景**（留给未来，写清楚触发条件）：出现「绕过应用的写库通道」（如第三方 ETL 直接写 `biz_contract`），且该通道不可改造。届时采用的是**只读校验函数 + 巡检调用**，仍然不是触发器。

---

## 4. 状态机

### 4.1 允许的边（5 条）

| # | from | to | 触发动作 | 是否必须给 reason | 允许的前置条件 |
|---|---|---|---|---|:--:|
| E1 | `DRAFT` | `SIGNED` | 登记签订（填 `sign_date/start_date/end_date`） | 否 | 三个日期必填（V-08 同步生效）；乙方存在（V-11） |
| E2 | `SIGNED` | `ACTIVE` | 生效（到达 `start_date` 或手工确认） | 否 | — |
| E3 | `SIGNED` | `TERMINATED` | 已签未生效即终止 | **是** | DEC-05 |
| E4 | `ACTIVE` | `COMPLETED` | 履约完成 | 否 | 无未关闭下游单据（建议；不硬拦，见 §5.2 注） |
| E5 | `ACTIVE` | `TERMINATED` | 中途终止 | **是** | 无未关闭下游单据（同上） |

Rust 侧落地形状（**设计规格，不是代码交付**）：在 `business/shared/state_machine.rs` 新增

```text
ENTITY_CONTRACT:  &str = "CONTRACT"
CONTRACT_EDGES: [(&str,&str); 5] = [
    ("DRAFT","SIGNED"), ("SIGNED","ACTIVE"), ("SIGNED","TERMINATED"),
    ("ACTIVE","COMPLETED"), ("ACTIVE","TERMINATED"),
];
CONTRACT_REASON_EDGES: [(&str,&str); 2] = [("SIGNED","TERMINATED"), ("ACTIVE","TERMINATED")];
CONTRACT_LOCKED: [&str; 4] = ["SIGNED","ACTIVE","COMPLETED","TERMINATED"];   // 只读标记用，见下
Entity::Contract（含变体 / ALL / as_str / label / edges() / reason_edges() / locked_statuses() 七处同步，见附录 B）
```

**`CONTRACT_LOCKED` 的用法要收窄**（不要照抄别的实体的 `ensure_writable` 一刀切）：其它实体的 locked 是「入账态，整单只读」，合同则是「已签后只冻结关键字段，备注/负责人/部门仍可改」（§5.3 矩阵）。因此 `locked_statuses()` 只用于**前端置灰与 `is_locked` 判断**；写库校验必须走 §5.3 的字段白名单，**handler 不得直接调 `ensure_writable`** —— 否则连「终止原因写 remark」都会被自己挡住。

### 4.2 禁止的边（15 条，逐条列出，实现卡直接照抄成单测）

| from | to | 禁止理由 |
|---|---|---|
| `DRAFT` | `ACTIVE` | 跳签。没有签订日却已生效，`SIGNED` 存在的意义被抹掉 |
| `DRAFT` | `COMPLETED` | 跳两级。且「从未签过却已完成」无业务含义 |
| `DRAFT` | `TERMINATED` | 草稿作废应走**软删除**（DEC-07 配套）：草稿从未对外生效，「终止」是对已签合同的动作。两者混用会让「终止率」统计失真 |
| `SIGNED` | `DRAFT` | 回退。签订事实不可撤销，只能终止 |
| `SIGNED` | `COMPLETED` | 跳级。必须先 `ACTIVE`，否则「已完成」缺乏生效区间 |
| `ACTIVE` | `DRAFT` | 回退 |
| `ACTIVE` | `SIGNED` | 回退 |
| `COMPLETED` | `DRAFT` / `SIGNED` / `ACTIVE` / `TERMINATED` | 终态。完成后发现违约，走新签合同，不改历史 |
| `TERMINATED` | `DRAFT` / `SIGNED` / `ACTIVE` / `COMPLETED` | 终态。终止不可撤销；恢复 = 新签 |

**自环（`X → X`，5 条）**：一律 409 `66014`。F11 的 `transition` 已经拒绝自环并只写「真实变化」的日志；合同侧调用方**不得**自己写 `if from != to`（那会绕过注册表核对）。

### 4.3 为什么 DB 拦不住状态机（反证，写进文档防止有人「加个 CHECK 就以为搞定了」）

POC P4 实测：

```text
INSERT 0 1        -- DRAFT 合同
UPDATE 1          -- 直接 update status='COMPLETED'（被 4.2 明令禁止的边）
P4 结论：DRAFT→COMPLETED 直改库成功（DB 无 CHECK 可表达边）⇒ 边表必须由应用层 state_machine 统一入口把关
```

原因：`CHECK` 只能看到**单行当前值**，看不到「上一状态」。要表达「从什么变成什么」必须有历史，而本仓的写库路径是普通 `UPDATE`（不是 CDC，也没有触发器）。因此：

- **边表只存在于应用层**（`state_machine` 注册表 + `LEGAL_EDGES` 常量）；
- **审计靠 `biz_status_log`**：`transition` 同事务写日志，`from_status` 由调用方传入并与 DB 现值比对；
- **防绕过**：代码评审规则 —— 合同模块内**禁止**出现 `UPDATE biz_contract SET status`（唯一允许处是 `state_machine::transition` 的内部实现）。建议加一条 grep 级 CI 检查（`rg "SET status" business/erp/contract_*` 应为 0 命中）。

---

## 5. 状态机与业务可用性

### 5.1 采购单只能引用 `status IN ('SIGNED','ACTIVE')` 的合同（V-22 细则）

**判定顺序**（每步一个明确错误话术，避免"合同不可用"这种笼统提示）：

| 步 | 检查 | 失败话术（66013 细分 message） |
|---|---|---|
| 1 | `contract.deleted_at IS NULL` | 「合同已删除，不能下单」 |
| 2 | `status IN ('SIGNED','ACTIVE')` | 「合同状态为 草稿/已完成/已终止，不能下单」 |
| 3 | `:order_date BETWEEN start_date AND end_date` | 「下单日期 2026-11-02 不在合同有效期 2026-01-01 ~ 2026-12-31 内」 |
| 4 | `party_b_type='SUPPLIER' AND party_b_id = :supplier_id` | 「合同乙方与所选供应商不一致」（防止"拿 A 合同下 B 供应商的单"） |
| 5 | `Σ PO(未取消) + 本单 ≤ amount`，且同一事务 `FOR UPDATE` 锁合同行（V-23） | 「累计下单将超出合同金额（剩余 3,200.00）」 |

等价 SQL（应用层一条查询即可拿全，省三次往返）：

```sql
SELECT id, contract_no, amount, party_b_id, start_date, end_date, status
  FROM biz_contract
 WHERE id = :contract_id
   AND deleted_at IS NULL
   AND status IN ('SIGNED','ACTIVE');
-- 命中 0 行 ⇒ 66013；命中后用返回的 start/end/party_b 做 3、4 步判定
```

**只在建单与换合同时校验**（这是个重要决定）：

- **建单**（`purchase_type='CONTRACT'` 必须带 `contract_id`）→ 全量校验 1–5；
- **换合同**（`PUT` 改 `contract_id`）→ 全量校验 1–5；
- **提交 / 审批 / 收货** → **不再复检合同状态**。

理由：合同在单据生命周期中途变 `COMPLETED`（履约完成）或 `TERMINATED` 是常态。若在提交/审批处复检，会出现「PO 建好三天后合同到期，PO 再也提交不了」——把一个**已成立的业务承诺**卡死。合同状态影响的是「能不能新承诺」，不是「能不能继续履行已承诺的」。这条必须在实现卡里写明，否则会被"顺手加一道校验"破坏。

### 5.2 合同 `TERMINATED` 之后：已有下游单据怎么处理

| 对象 | 处理 | 理由 |
|---|---|---|
| 已存在的采购单（任何状态） | **不改**（`status`/金额/供应商一律不动） | 合同终止不改变已发生的收货与付款事实。改 PO 状态 = 回滚历史 = 编造 |
| 已生成入库单 / 应付单 / 凭证 | **不改、不冲红** | 同上。若业务上真要退货/退款，走既有的退货/付款冲销链，不由合同终止触发 |
| 新的采购单（`purchase_type='CONTRACT'`） | **阻断**（V-22 步 2） | 「仅阻断新建」——任务书原话 |
| 已有 `DRAFT` 状态的采购单 | **不强制作废**，列表打标签「合同已终止」 | 强制作废会删掉别人的在建工作；是否继续由业务决定，系统只提示 |
| 累计金额校验（V-23） | 终止后新建已阻断，该校验自然失效 | — |
| 合同自身 | `status='TERMINATED'` + `reason` + `biz_status_log` 一行 | 审计可查「谁、何时、为什么终止」 |
| 统计口径 | 「终止合同的在途订单」= `PO.status NOT IN ('CANCELLED','CLOSED')`，作为**经营看板指标**，不当作数据错误 | 这是需要人跟进的事，不是缺陷 |

**`COMPLETED` 同理**：不再接受新单，已有单据照旧。区别只是 `COMPLETED` 是"履约完毕"，`TERMINATED` 是"提前结束"，对下游的影响**完全一致**（都不回溯、都阻断新建）。这种一致性要写进实现卡，避免两套分支。

### 5.3 已签合同的字段冻结矩阵（V-19 的判定表）

| 字段 | `DRAFT` | `SIGNED` | `ACTIVE` | `COMPLETED` | `TERMINATED` |
|---|:--:|:--:|:--:|:--:|:--:|
| `contract_no` | 改 | 冻结 | 冻结 | 冻结 | 冻结 |
| `contract_name` | 改 | 改 | 改 | 改 | 改（名称笔误可修） |
| `contract_type` | 改 | **冻结** | 冻结 | 冻结 | 冻结 |
| `party_a_id` / `party_a_name` | 改 | 改（快照随读） | 改 | 冻结 | 冻结 |
| `party_b_type` / `party_b_id` / `party_b_name` | 改 | **冻结** | **冻结** | **冻结** | **冻结** |
| `amount` / `currency_code` | 改 | **冻结** | **冻结** | **冻结** | **冻结** |
| `sign_date` / `start_date` / `end_date` | 改 | 冻结日 | 冻结日 | 冻结 | 冻结 |
| `owner_id` / `dept_id` | 改 | 改 | 改 | 改 | 改 |
| `remark` | 改 | 改 | 改 | 改 | 改（终止原因在此） |
| `status` | 仅经状态机 | 仅经状态机 | 仅经状态机 | 终态 | 终态 |

**「冻结日」的含义**：签订后日期不再改，因为下游已经按这个区间建了单（V-22 步 3）。改日期 = 让已存在的 PO 一夜之间"落在合同期外"。

**这张表就是实现卡的全部校验逻辑**：一次读旧行 → 按 `status` 取允许列集合 → 逐列比对 → 首个越界列直接 `66015`（话术里点名列名）。

---

## 6. 合同变更的影响面分析（尤其 `party_b_id`）

### 6.1 变更矩阵：改什么、谁受伤

| 变更 | 本期是否允许 | 下游谁存了相关事实 | 受伤形式 |
|---|---|---|---|
| `party_b_id`（换乙方） | 仅 `DRAFT`（DEC-07） | `biz_purchase_order.supplier_id`（快照）+ `supplier_name_snapshot`；`biz_stock_in.supplier_id`（拷贝自 PO）；`biz_payable.supplier_id`（拷贝自入库）；凭证 aux 分录的供应商辅助核算 | 「供应商」在各单据上指向不同主体：合同说 B，历史单据说 A |
| `party_b_name`（主档改名，合同不动） | 合同**永不跟随**（快照） | 列表若 JOIN 主档当前名，会显示新名 | 「合同上写老名字，供应商档案是新名字」 |
| `amount`（改额度） | 仅 `DRAFT` | 无存储；V-23 现算 | 已下单额 > 新额度 ⇒ 超签（Q3 检出） |
| `end_date`（缩期） | 仅 `DRAFT` | 无存储；V-22 步 3 现算 | 已有 PO 的 `order_date` 落在新区间外 |
| `status`（终止） | 允许（经状态机） | 无存储；下游不改 | 见 §5.2（合法历史，不算漂移） |
| 供应商主档被软删 | 允许（主档功能） | 合同 `party_b_id` 变成孤儿 | Q1 检出（`DEFECT`） |

### 6.2 「偏移」分两类，判定口径不同（这是本节的核心）

```
SNAPSHOT（合法业务变更）：快照记录的是"当时是谁"，真相变了快照不变是设计意图 → 只展示、可 ACCEPTED，不修
DEFECT（数据缺陷）    ：拷贝应随上游、却与上游不一致 → 必须 REPAIRED
```

判定的关键是**比较对象必须是"直接上游"，不是"最终真相"**：

| 下游字段 | 应等于 | 与「合同乙方」比 | 判定 |
|---|---|---|---|
| `biz_purchase_order.supplier_id` | 建单时刻的 `contract.party_b_id` | 现在可能不等 | **SNAPSHOT**（合法历史） |
| `biz_purchase_order.supplier_name_snapshot` | 建单时刻的乙方名 | 可能不等 | **SNAPSHOT** |
| `biz_stock_in.supplier_id` | **其采购单**的 `supplier_id` | 不直接比合同 | 与其 PO 不等 = **DEFECT** |
| `biz_payable.supplier_id` | **其入库单**的 `supplier_id` | 不直接比合同 | 与其 IN 不等 = **DEFECT** |
| 凭证 aux 分录供应商 | 其应付单/业务单据 | 同上逐环比 | **DEFECT** |

**为什么不能"全部和合同比"**：P5 实测给出了反例 —— 换乙方后，2,000 张入库单与 2,000 张应付单**与合同全部不一致，但它们与各自上游完全一致**。若拿合同当真相比，这 4,000 行会被误判成缺陷，运维被淹；正确口径下缺陷数 = **0**。

**POC 实测数据（20,000 张采购单 / 2,000 入库 / 2,000 应付）**

| 指标 | 换乙方前 | 换乙方后 | 含义 |
|---|---|---|---|
| `PO.supplier_id` vs 合同 `party_b_id` 偏移 | 0 | **20,000** | 全部是 SNAPSHOT，一行都不是缺陷 |
| `IN.supplier_id` vs 其 PO 的 `supplier_id` 偏移 | 0 | **0** | 拷贝链内部自洽 |
| `PAYABLE.supplier_id` vs 其 IN 的 `supplier_id` 偏移 | 0 | **0** | 同上 |
| 人为改错 1 张入库单的供应商 | — | **1** | 缺陷检出精确到 1 行，无误报 |
| 巡检 SQL 耗时（20,000 行） | — | **5.6 ms**（Hash Join，shared hit=543） | 不需要建新索引也够快 |

> 结论：**20,000 行的"看起来偏移"，真缺陷是 0**。这直接决定巡检表的设计 —— 不能按"行粒度 + 与合同比"落表，否则一次合同换乙方就往 `biz_snapshot_drift` 里灌 2 万行，运维直接放弃。建议按「真相对象 + 字段 + 受影响计数」粒度落一条（`affected_count=20000`），这也正是章节 4 建议的形状。

### 6.3 巡检 SQL（交付给 t_c7610b31，可直接跑）

```sql
-- Q4-a 采购单快照 vs 合同真相（SNAPSHOT：合法历史，只统计不修）
SELECT c.id AS entity_id, c.contract_no AS entity_no, 'party_b_id' AS field_name,
       count(*) AS affected_count, 'SNAPSHOT' AS drift_kind, 'INFO' AS severity,
       'biz_contract.party_b_id' AS truth_source
  FROM biz_purchase_order o
  JOIN biz_contract c ON c.id = o.contract_id
 WHERE o.deleted_at IS NULL AND c.deleted_at IS NULL
   AND o.supplier_id IS DISTINCT FROM c.party_b_id
 GROUP BY c.id, c.contract_no;

-- Q4-b 入库单 vs 其采购单（DEFECT：拷贝断裂，必须修）
SELECT s.id AS entity_id, s.in_no AS entity_no, 'supplier_id' AS field_name,
       'DEFECT' AS drift_kind, 'ERROR' AS severity, 'biz_purchase_order.supplier_id' AS truth_source
  FROM biz_stock_in s
  JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL AND o.deleted_at IS NULL
   AND s.supplier_id IS DISTINCT FROM o.supplier_id;

-- Q4-c 应付单 vs 其入库单（DEFECT）
SELECT p.id AS entity_id, p.payable_no AS entity_no, 'supplier_id' AS field_name,
       'DEFECT' AS drift_kind, 'ERROR' AS severity, 'biz_stock_in.supplier_id' AS truth_source
  FROM biz_payable p
  JOIN biz_stock_in s ON s.id = p.source_id AND p.source_type = 'STOCK_IN'
 WHERE p.deleted_at IS NULL AND s.deleted_at IS NULL
   AND p.supplier_id IS DISTINCT FROM s.supplier_id;
```

> 注意 Q4-b/Q4-c 的 `JOIN` 条件必须锚定**直接上游**（`s.source_order_id` / `p.source_id`），不能 JOIN 到合同 —— 这是 6.2 结论的 SQL 化。

### 6.4 责任划分（任务书要求「由哪张卡负责巡检」）

| 工作 | 负责卡 | 交付物 |
|---|---|---|
| 合同表 DDL、校验规则、状态机边表、本影响面分析 | **本卡 t_014f0f18** | 本章 |
| `biz_purchase_order` 加 `contract_id` / `purchase_type` / `contract_no_snapshot` / `supplier_name_snapshot`；建单与换合同时的 V-22/V-23 校验；`FOR UPDATE` 锁 | **t_52f3cb32**（重构采购订单与入库单的勾稽关系） | 迁移 V0194 + handler 改造 |
| `biz_snapshot_drift` 表结构、巡检任务编排、告警与 ACCEPTED/REPAIRED 流程 | **t_c7610b31**（快照与真相一致性巡检机制及告警方案） | 迁移 V0195 + 定时任务 |
| 通用规则与各业务链映射（把本章的「快照 vs 拷贝」扩到销售/生产/委外/财务/调拨/退货） | t_156f8f8a（产品经理） | 映射表章节 |
| 汇总与一致性校核（口径冲突收敛） | t_7320b4dd | 设计说明书 |

**本章对 t_c7610b31 的硬要求**：Q4-a 必须落成「聚合 + `affected_count`」而不是行粒度；`drift_kind` 必须区分 `SNAPSHOT`/`DEFECT`；同一未解决漂移用部分唯一索引做幂等（`ON CONFLICT`）。否则本节的 2 万行结论会被推翻。

---

## 7. 本卡不做（明确边界）

| 不做 | 归属 | 说明 |
|---|---|---|
| **合同附件**（扫描件、PDF 上传、归档） | 不做，无排期 | 需 `sys_file` owner 隔离 + 业务对象绑定，独立卡 |
| **合同审批流**（签订/终止走流程审批） | 不做 | 需接入 `biz_flow_instance`；本卡不加 `approval_instance_id` 列（见 §2.1 说明），避免诱导实现卡顺手接 |
| **客户表 `biz_customer` 及客户主数据** | 批次 2 | 本期 CUSTOMER 分支降级（DEC-08 / §3.2.3） |
| 合同变更单 / 版本链 / 补充协议 | 不做，批次 3 候选 | DEC-07 用「终止 + 新签」表达 |
| 框架合同 / 不限额合同 | 不做，批次 3 候选 | DEC-04；需 `limit_type` 维度而非放宽 `amount > 0` |
| 合同台账报表（执行率、逾期率看板） | 不做 | 数据可由本章字段 + PO 现算，报表归报表卡 |
| 销售合同/出库/应收链的落地 | 批次 2 | 本章只保证 `SALES` 枚举与配对 CHECK 结构留位 |
| 具体代码实现（Rust handler/model、Vue 页面、迁移文件） | 实现卡（t_52f3cb32 等） | 本章只有 DDL 与规则 |

---

## 8. 验收自检（可复跑）

**复跑命令**（探针库可随时重建，不碰任何业务库）：

```bash
# 1) 用与主干同构的库做探针（本机 cloudpivot_3b_sql 有 93 张表、无 biz_contract）
pg_dump -h 127.0.0.1 -U alun -d cloudpivot_3b_sql --schema-only --no-owner --no-privileges -f /tmp/arch2_schema.sql
# 2) 重建空库（alun 无 CREATEDB 权限，实测须由 postgres 建库）
su postgres -c "dropdb --if-exists arch2_contract_poc && createdb -O alun arch2_contract_poc"
psql -q -h 127.0.0.1 -U alun -d arch2_contract_poc -f /tmp/arch2_schema.sql
# 3) 跑 POC（ON_ERROR_STOP：任何一条不符合预期即中断）
psql -v ON_ERROR_STOP=1 -h 127.0.0.1 -U alun -d arch2_contract_poc \
     -f docs/evidence/2026-11-02-arch2-contract/model-poc.sql
# 期望：exit=0；11 条 V-xx PASS + 1 条 P3-1 PASS；po_drift_after=20000；in_vs_po_defect=0；in_vs_po_defect_after_break=1；cols=23 / checks=9
```

**验收对照表**

| 验收标准（任务书） | 证据 | 状态 |
|---|---|---|
| DDL 可直接执行 | `model-poc.sql` 在 93 表真库上 `exit=0`；P1 段结构输出 23 列 / 9 CHECK / 5 索引 / CT 取号 | ✅ |
| 字段覆盖全部要求 | §2.1 字段字典逐条对照（含契约金额 `18,4` 与币种 `CHAR(3)`） | ✅ |
| 软删唯一约束写法 | §2.3（部分唯一索引 + 三种错误写法对照 + P3 实测三场景） | ✅ |
| 必备索引 party_b_id / status / end_date | §2.3 索引表（三条齐备，另注明为何用部分索引） | ✅ |
| 每条校验注明执行层与失败语义 | §3.1（DB，SQLSTATE）/ §3.2（应用层，HTTP+66013-66020+话术）/ §3.3（巡检，kind+severity）/ §3.4（触发器评估） | ✅ |
| SUPPLIER 命中 `biz_supplier` | V-11（应用层，照抄 F13 现成写法）+ Q1（巡检 SQL 实测精确检出 1 行） | ✅ |
| CUSTOMER 降级 + 批次 2 替换点 | §3.2.3（临时策略 4 条 + 替换点 R1–R4，表结构零改动） | ✅ |
| `amount > 0` / `end_date >= start_date` | V-04（0 与负数两条均 PASS）/ V-05（PASS） | ✅ |
| 状态机允许边与禁止边 | §4.1（5 条）/ §4.2（15 条 + 5 自环），另附 DB 拦不住的反证 P4 | ✅ |
| 采购单只引用 SIGNED/ACTIVE | §5.1（5 步判定 + 等价 SQL + 只在建单/换合同时校验的决定） | ✅ |
| TERMINATED 后下游处理 | §5.2（8 行矩阵：不回滚、不冲红、不强制作废、只阻断新建） | ✅ |
| 变更影响面 + 巡检责任卡 | §6.1–§6.4（含 20,000 行实测：偏移 20,000、缺陷 0、人为断裂检出 1） | ✅ |
| 明确列出不做的事 | §7（8 项，含附件/审批流/客户表） | ✅ |
| 输出为设计文档独立章节 | 本文件 | ✅ |

**已知未覆盖 / 留给下游卡的**

1. `biz_snapshot_drift` 的最终表结构不在本章（t_c7610b31），本章只约定字段契约与判定口径。
2. 合同列表/详情接口的响应体不在本章（属实现卡），但 §5.1 的 5 步判定与 §5.3 的冻结矩阵是接口必须表达的业务规则。
3. `dept_id` 数据权限的具体过滤表达式依赖 `shared/scope.rs` 现状，本章只声明列语义。

---

## 附录 A：与既有草稿 `V0193__erp_contract.up.sql` 的对照

该草稿（`docs/evidence/2026-11-02-arch2-contract/`，08:24 生成）覆盖范围比本章大（含 PO 列、漂移表、两个追溯视图），本章**只收敛合同表口径**，差异 4 处见 §1 末表。除此之外它有几处事实值得继承：

| 草稿里的做法 | 本章处理 |
|---|---|
| `unique(contract_no) where deleted_at is null` | ❌ **语法错误**，改为 `CREATE UNIQUE INDEX ... WHERE deleted_at IS NULL`（§2.3） |
| 自检 `DO $$ ... RAISE EXCEPTION` 断言列数/CHECK 数/索引数 | ✅ 继承（POC P1/P6 采用，断言改为 23 列 / 9 CHECK / 5 索引） |
| 「存量不回填、不编造」 | ✅ 继承（本章 §7 + 迁移口径：无存量数据，因为表是新建的） |
| `biz_snapshot_drift` 的 SNAPSHOT/DEFECT 二分 | ✅ 继承并**实证**（§6.2：20,000 偏移 vs 0 缺陷） |
| 「一次合同换乙方影响 20,001 张采购单」 | ⚠️ 本章实测为 **20,000**（自建夹具规模，非生产数据）；口径一致，具体数字以本章 POC 为准，禁止在汇报中混用 |

## 附录 B：实现卡落地清单（照做即可，无需再决策）

| # | 事项 | 位置 | 备注 |
|---|---|---|---|
| 1 | 迁移 `V0193__erp_contract.up.sql` | `backend/migrations/business/` | 只放 §2.2 的 DDL（1 张表 + 4 个索引 + 19 条 COMMENT〔1 表 + 18 列〕+ CT 取号 1 行）；开工前复核 V0193 仍空（F3） |
| 2 | 迁移 down 脚本 | 同上 | `DELETE FROM biz_seq WHERE seq_type='CT';` + `DROP TABLE biz_contract;`（顺序不敏感，写清是为了 down 可重复执行） |
| 3 | `Entity::Contract` 七处注册 | `business/shared/state_machine.rs` | 变体 / `ALL` / `as_str` / `label` / `edges()` / `reason_edges()` / `locked_statuses()`；漏一处运行期 500（F11） |
| 4 | 错误码 `66013–66020` | `shared/error_code.rs` | 已确认 66013+ 空（F14）；只加常量，不改既有码 |
| 5 | `contract_model::party_b_ref` | 新建 `business/erp/contract_model.rs` | 照抄 `purchase_model::supplier_ref`（F13）：`SELECT supplier_name ... WHERE id=? AND deleted_at IS NULL` |
| 6 | 巡检 SQL 三条 + 孤儿 SQL | 交 t_c7610b31 | §3.3 Q1、§6.3 Q4-a/b/c，**原样使用**（POC 已验证） |
| 7 | 权限码 | `erp:contract:{list,create,update,delete,status}` | 与 `sys_menu.auth_code` 一一对应；SUPER_ADMIN 短路 |
| 8 | 前端路由与菜单 | `views/business/erp/contract/` | 本期只开放 PURCHASE（DEC-08 / §3.2.3） |

---

**本章结论一句话**：合同表落地成本很低（23 列、9 个 CHECK、4 个索引），**真正的成本在三处不在表里** —— ① 状态机七处注册与边表单测；② 乙方存在性没有外键可依赖，必须应用层 + 巡检双轨；③ 下游快照的「合法偏移」与「缺陷」必须分开判定，否则 2 万行噪声会淹没 1 行真缺陷。
