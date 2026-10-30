# 云枢 ERP 第 1 批 — 技术架构设计

> **范围**：最小主数据 + 采购 → 收货入库 → 库存 → 应付 → 凭证引擎 → 总账（垂直闭环）
> **技术栈**：Rust 1.94.1 / edition 2024 + nik 0.4.2 + PostgreSQL 16（`docker/init/Dockerfile` = `postgres:16-alpine`）+ Vue 3.5 + TDesign Vue Next 1.19
> **代码基线**：`/root/work/c144`（`backend` = crate `cailu` v0.1.0，前端 = `@cailu/web` v0.1.0）
> **文档版本**：v1.0 / 2026-10-27
> **状态**：设计定稿，待评审 → 可直接作为实施依据

---

## 0. 决策摘要（先看这一页）

全部结论都基于**对现有代码的实测**（第 1 章列出证据），不是通用 ERP 教科书。

**本设计已通过三类真实验证**（不是纸面推演，详见 §9.1）：
1. **算法验证**：用 `rust_decimal 1.43` 实跑移动加权平均、尾差倒挤、含税拆分，拿到真实数字（POC-1~5）；
2. **依赖验证**：`nik 0.4.2 + features=["lite","rust_decimal"]` 依赖解析实测通过，无版本冲突（POC-6）；
3. **DDL 真库验证**：§2 的 20 张表**原样提取**后在 PostgreSQL 18.6 执行，并用 12 组业务场景检验约束，10 项全部按设计拦住了脏数据，过程中发现并修正 1 个真实的会计科目定义缺陷（§9.1.8）。

### 0.1 十条硬结论

| # | 决策点 | 结论 | 一句话理由 |
|---|--------|------|-----------|
| D1 | 金额/数量的 Rust 类型 | `rust_decimal::Decimal`（开启 nik `rust_decimal` feature） | 会计必须精确十进制；已实测 `nik::prelude::rust_decimal` 可用，**不需要**违反"只声明 nik"铁律 |
| D2 | 金额列 DB 类型 | `NUMERIC(18,4)`（金额）/ `NUMERIC(18,6)`（单价、成本）/ `NUMERIC(20,4)`（科目余额） | PG 驱动原生二进制编解码 NUMERIC；单价必须比金额多 2 位，否则移动加权累计误差 |
| D3 | 状态列 DB 类型 | `VARCHAR(24)` + `CHECK` 约束（**不用** SMALLINT） | ERP 状态要给会计/审计直接读 SQL；偏离底座 `SMALLINT` 约定，理由见 §1.6 |
| D4 | 凭证生成时机 | **入库即生成**，贷方记"应付账款—暂估应付款" | 本批无发票模块；暂估科目为批次 2 的发票冲回留出路径，符合中国实务 |
| D5 | 科目映射策略 | 规则模板定"科目槽位" + **四级解析链**（单据行 → 主数据 → 规则匹配 → 系统参数） | 纯主数据方案无法表达"同物料不同场景不同科目"；纯规则方案新增物料必漏配 |
| D6 | 操作日志 | **双写**：`operation_logs`（中间件自动，不动）+ 新建 `biz_doc_log`（事务内） | 中间件 `describe_operation` 硬编码在 `src/middleware/`（红线禁改）；且中间件是**异步旁路**，事务回滚后仍会落"过账成功"假日志 |
| D7 | 库存并发控制 | `SELECT ... FOR UPDATE` 悲观行锁 + **按 material_id 升序加锁** + `INSERT ... ON CONFLICT DO NOTHING` 建行 | 乐观锁在此场景是重试风暴；行锁粒度 = 物料×仓库，冲突概率极低 |
| D8 | 负库存 | 默认**禁止**（仓库级开关 `allow_negative`，本批出库不实现故不触发） | 负数量无法计算移动加权均价，成本会崩 |
| D9 | 前端主题色 | **不改** `theme.less`（现有主色 `#0580a2`）；任务给的莫兰迪四色降级为**业务语义色**，局部作用域 | 全站改主色是 12 个既有页面的回归风险；莫兰迪色更适合表"借贷方向/单据状态"这类语义 |
| D10 | 单据编号 | PG 单语句原子取号 `UPDATE ... RETURNING`（不用 Redis） | 单据量级低（日均百级），事务内取号保证不重号；引入 Redis 反而新增一致性面 |

### 0.2 与任务书假设的三处偏差（必须先对齐）

| 任务书假设 | 代码实况 | 影响与处理 |
|---|---|---|
| "与现有莫兰迪配色（深海蓝 #51697C…）保持一致" | 现有 `frontend/src/style/theme.less` 主色是 **`#0580a2`（青蓝 H=193）**，整套 10 级色阶 + ECharts 图表色都锁定 H=193 | 莫兰迪四色在代码里**不存在**。按 D9 处理为语义色，不推翻现有主题 |
| 权限码"`business:xxx:yyy`"三段式 | AGENTS.md 约定三段式；但任务书要求 `business:erp:material:*` 四段 | 已核实权限校验是 `SELECT DISTINCT auth_code` 的**字符串集合匹配**（无格式校验），四段完全可用。本批统一四段，见 §4.5 |
| 迁移"从 V0030 起" | `migrations/system/` 最大 **V0022**，`business/` 最大 **V0020**，`support/` 最大 **V0017** | 无冲突。本批占 **V0030–V0039**（10 个版本），明细见 §2.10 |

### 0.3 本批交付边界（明确不做）

- ❌ 审批流（只留 `APPROVED` 状态与权限点，不做流程引擎）
- ❌ 销售 / 生产 / BOM / 调拨单（仅留接口抽象点，§6）
- ❌ 期末结账 / 报表 / 结转损益（仅做"禁止向已结账期间写凭证"，§3.3）
- ❌ 付款单、发票、核销的完整链路（本批只有应付单 + 手工核销标记）
- ❌ 出库业务（本批库存只有"入"，出库算法在 §3.2 给出设计但不在交付范围）
- ❌ 多币种汇率换算（只存 `currency_code`，汇率表批次 3）

---

## 1. 现状核查（设计的事实基础）

本章每条结论都指向真实文件，实施时若发现不符，以代码为准并回改本文档。

### 1.1 项目定位与目录

```
/root/work/c144
├── AGENTS.md              # 项目约定（必读）
├── skill/SKILL.md         # 项目级 AI 知识包
├── docs/                  # 二开文档集（后端/前端/编码规范/AI 指引）
├── backend/               # crate `cailu` v0.1.0, edition 2024, MSRV 1.94.1
│   ├── Cargo.toml
│   ├── migrations/{system,support,business,seed}/
│   └── src/{system,support,business,shared,middleware,task,utils}/
└── frontend/              # @cailu/web v0.1.0, Vue 3.5.32 + TDesign 1.19.1
    └── src/{api,views,router,stores,style,components}/
```

`business/` 现有唯一子模块 `monitor`（4 件套齐全：`mod.rs` / `handler.rs` / `dto.rs` / `model.rs`），是二开范本。

### 1.2 迁移号段实测

| 目录 | 已用最大版本 | 说明 |
|------|------------|------|
| `migrations/system/` | **V0022**（`add_session_security_flags`） | 底座表，禁改 |
| `migrations/support/` | **V0017**（`create_password_change_logs`） | 底座表，禁改 |
| `migrations/business/` | **V0020**（`add_biz_monitor_permission`） | 二开号段起点 |

→ 本批从 **V0030** 起，占 V0030–V0039（留 V0021–V0029 给 monitor 的后续演进）。

### 1.3 框架能力实测（决定设计可行性）

| 能力 | 实测结论 | 证据 |
|------|---------|------|
| 事务 | `db().transaction(\|tx\| async move { ... })`，返回 `Err` 自动回滚（RAII） | `docs/后端二开指南.md` §5；`src/system/role/model.rs:268` 实跑用例 |
| 事务内原生 SQL | `tx.find_with_params(sql, params)` / `tx.execute_with_params(...)` | `nik-db-0.4.2/src/tx.rs:165,109` |
| 原生 SQL 占位符 | 统一写 `?`，PG 驱动执行前 `rewrite_placeholders` 转 `$1,$2` | `nik-db-driver-postgres-0.4.2/src/lib.rs:33`、`connection.rs:142` |
| 动态条件 SQL | 拼**片段**（列名/占位符），值一律走 `Value` 参数 | 范本 `src/system/user/model.rs:184-228` |
| 分页响应 | `PageData { list, total, page, page_size }` | `nik-web-0.4.2/src/api.rs:75-84`；前端 `src/api/request.ts:14-23` 同构 |
| 分页规整 | `utils::normalize_page(page, page_size)`（默认 1/20，限幅） | `docs/后端二开指南.md` §6 |
| 软删除 | 字段 `deleted_at: Option<DateTime<Utc>>` 自动：查询追加 `WHERE deleted_at IS NULL`、`delete` 改 `UPDATE` | `src/shared/constants.rs` 模块文档 |
| 物理删表 | `#[model(hard_delete)]`；无 `updated_at` 用 `#[model(no_timestamps)]` | `src/support/log/model.rs` 实跑用例 |
| 错误出口 | handler `ResResult<T>` / model `DbResult<T>` / 启动任务 `nik::Result<T>`；`utils::db_error(e, "上下文")` 补中文 | `docs/后端二开指南.md` §4 |
| 权限校验 | `SELECT DISTINCT m.auth_code FROM sys_menu ...` → 字符串集合 contains，**无格式校验** | `src/system/auth/model.rs:255-267` |
| 枚举落库 | `#[derive(TypeMapper)] #[int]` → SMALLINT | `src/shared/constants.rs` `EnableStatus` |
| 日志总线 | `bus::enqueue_*` 无阻塞投递，后台 worker 批量落库；**与业务事务无关**（独立连接） | `src/support/log/bus.rs` 模块文档 |
| 前端路由 | **静态数组** + `router.beforeEach` 校验 `meta.permission`；**无动态 addRoute** | `frontend/src/router/index.ts` |

### 1.4 金额类型现状与唯一正确落地路径（关键）

**现状**：`backend/Cargo.toml` 的 nik features 为
`["db","postgres","cache","plugin","fs-local","chrono","uuid","json","ipnetwork","xss"]`
**不含 `rust_decimal`**，`Cargo.lock` 中 `rust_decimal` 出现次数 = **0**。

**结论路径**（已实测，见 §9.1 POC-6）：

1. `backend/Cargo.toml` 的 nik features 追加 `"rust_decimal"`（**不是**新增 crate 依赖）；
2. 业务代码 `use nik::prelude::rust_decimal::Decimal;`（门面重导出，不走 `extern prelude`，符合"只声明 nik"铁律）；
3. `Decimal` 已实现 `TypeMapper`：`Type::Decimal` ↔ `Value::Decimal(CompactDecimal)`；
4. PG 驱动原生支持 NUMERIC 二进制编解码（`encode_numeric` / `decode_numeric`）；
5. `serde` 序列化为**字符串**（实测 `Decimal("1234567.8901")` → `"1234567.8901"`），前端拿到 string，天然无精度损失。

**必须记住的坑**：`Value::Decimal` 只能绑到 **NUMERIC 列**。驱动在 `convert.rs:298` 明确校验 `PgParam::Numeric` 的目标列类型必须是 `Type::NUMERIC`，绑到 BIGINT / VARCHAR 会直接报错。写参数用 `TypeMapper::to_value(&d)`（`Value` 没有 `From<Decimal>` 实现）。

### 1.5 操作日志中间件的两个硬限制（D6 的依据）

`src/middleware/operation_log.rs`：

1. **描述硬编码**：`describe_operation(method, path)` 是 if-else 路径匹配，覆盖 auth/file/user/role/menu/dept/config/dict/job 等；ERP 路径未命中会 fallback 成 `"POST /api/business/erp/stock-in/xxx/post"`。加 ERP 分支 = 修改 `src/middleware/`，**触碰红线**。
2. **与业务事务无关**：日志走 `bus::enqueue_operation` 无阻塞投递，独立连接落库。**若 handler 的事务回滚后返回错误，中间件仍会写入一条操作日志**。对会计系统这是致命的：会出现"日志说已过账、账上没数据"。

→ 因此 ERP 必须有**事务内**的业务轨迹表 `biz_doc_log`（§2.9）。两层日志职责不同，不是冗余：
- `operation_logs`：安全审计（谁调了哪个接口、参数、耗时、IP），不可篡改要求高；
- `biz_doc_log`：业务轨迹（单据从草稿到过账的每一次状态迁移，含前后状态），与账务数据**同生共死**。

### 1.6 为什么业务表状态列用 VARCHAR 而不是 SMALLINT（偏离底座约定）

底座 `sys_*` 表全用 `status SMALLINT`（0/1），这是**布尔型启用状态**的最优解。但 ERP 单据状态是**多值状态机**（`DRAFT/SUBMITTED/APPROVED/PARTIAL_RECEIVED/COMPLETED/CLOSED/CANCELLED`），两者不同质：

| 方案 | 优势 | 劣势 | 结论 |
|------|------|------|------|
| `SMALLINT` + 字典 | 省 1 字节；与底座一致 | 会计/审计查库要处处 JOIN 字典；加状态要改代码映射；日志与对账 SQL 不可读 | ✗ |
| `VARCHAR(24)` + `CHECK` | 查库直接可读；加状态只需改 CHECK；与 `biz_doc_log.from_status/to_status` 天然一致 | 占 24 字节（可忽略）；需维护 CHECK 列表 | ✓ **采用** |
| PG 原生 `ENUM` 类型 | 类型安全最强 | 加值需 `ALTER TYPE`（迁移不可逆风险高）；框架 TypeMapper 不认 | ✗ |

同理：`material_type` / `costing_method` / `account_category` 等**分类字典**也用 `VARCHAR` 短码（`RAW`/`SEMI`/`FINISHED`），在 Rust 侧用 `&'static str` 常量 + `parse()` 校验（与 `src/support/log/mod.rs` 的 `AuthType::as_str()` 同款风格）。

> 保留 SMALLINT 的场合：仅"真布尔"字段（`allow_negative`、`is_leaf`、`status`），与底座语义一致。

### 1.7 前端现状

- 路由：`src/router/index.ts` 静态数组，`meta: { title, permission }`，`router.beforeEach` 比对 userStore 权限集合；
- API 层：`src/api/{system,support,business}/`，`src/api/request.ts` 统一封装（401 自动刷新）；
- 列表页范本：`src/views/system/user/index.vue`（712 行，`t-table` + `t-pagination` + 查询栏 + 抽屉表单）；
- 主题：`src/style/theme.less` 锁定品牌色阶 H=193，主色 `--td-brand-color-7: #0580a2`，亮/暗双套，**明暗切换靠选择器特异性对齐 TDesign**（改这里必须镜像 TDesign 两套选择器，否则主题失效）；
- 依赖：`axios` / `dayjs` / `echarts` / `tdesign-vue-next` / `vue` / `vue-router` / `pinia`，**无** bignumber / decimal 库（金额必须走后端字符串，见 §5.3）。

---
## 2. 数据模型（V0030 起，20 张表）

### 2.0 全局约定（先定规矩，再写 DDL）

#### 2.0.1 主键与审计列

| 约定 | 内容 |
|------|------|
| 主键 | `id UUID PRIMARY KEY DEFAULT gen_random_uuid()`；代码侧统一 `uuid::Uuid::now_v7()`（时间有序，索引友好），DEFAULT 仅兜底 |
| 创建/更新 | `created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP`、`updated_at` 同（框架 `TimestampHook` 自动维护） |
| 操作人 | `create_by UUID` / `update_by UUID`（弱引用 `sys_user.id`，与底座一致不建外键） |
| 软删 | `deleted_at TIMESTAMPTZ`（`NULL` = 未删），唯一索引一律带 `WHERE deleted_at IS NULL` |
| 物理删表 | 追加写/流水/明细表用 `#[model(hard_delete)]`，不设 `deleted_at` |

#### 2.0.2 外键策略（对齐底座 `sys_user.dept_id` 的"弱引用不建外键"约定）

| 关系类型 | 做法 | 理由 | 例子 |
|---------|------|------|------|
| 主数据引用 | **弱引用 + 索引 + 注释标注** | 主数据可能被软删/禁用，硬外键会让历史单据无法保留 | `biz_material.unit_id`、`biz_supplier.payable_account_id` |
| 单据→来源单据 | **弱引用** | 跨单据各自独立生命周期，作废/冲销不代表删行 | `biz_stock_in.source_order_id` |
| 明细→主表 | **真外键 `ON DELETE CASCADE`** | 明细无独立生命周期，主表物理删除时同步删 | `biz_purchase_order_item.order_id` |
| 分录→凭证 | **真外键 `ON DELETE CASCADE`** | 同上 | `biz_voucher_entry.voucher_id` |

#### 2.0.3 数值精度（本次设计的会计核心）

| 语义 | DB 类型 | Rust 类型 | 小数位 | 说明 |
|------|---------|-----------|--------|------|
| 数量 | `NUMERIC(18,4)` | `Decimal` | 4 | 支持 KG/米等小数计量 |
| 单价 / 成本单价 | `NUMERIC(18,6)` | `Decimal` | **6** | 移动加权均价必须比金额多 2 位，否则每次入库都被舍入，误差累计 |
| 金额 | `NUMERIC(18,4)` | `Decimal` | 4 | 单据金额、税额 |
| 科目余额 | `NUMERIC(20,4)` | `Decimal` | 4 | 累计金额可能上亿，整数位放宽到 16 位 |
| 税率 | `NUMERIC(7,4)` | `Decimal` | 4 | `0.1300` 表示 13% |
| 换算率 | `NUMERIC(18,6)` | `Decimal` | 6 | 单位换算 |

> **禁止 `FLOAT` / `DOUBLE` / `REAL`**：任何金额、数量、成本列一律 NUMERIC。§9.1 POC-3 实证了 f64 累加的精度漂移。

#### 2.0.4 索引命名

`uk_<表>_<语义>`（唯一）/ `idx_<表>_<列>`（普通），与底座 `uk_sys_dict_type_code`、`idx_sys_user_dept_id` 一致。

---

### 2.1 主数据（迁移 `V0030__create_biz_master_data`）

#### 2.1.1 `biz_unit` —— 计量单位

```sql
CREATE TABLE biz_unit (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    unit_code     VARCHAR(20)  NOT NULL,                            -- 单位编码（如 PCS / KG / BOX）
    unit_name     VARCHAR(50)  NOT NULL,                            -- 单位名称（如 个 / 千克 / 箱）
    is_base       SMALLINT     NOT NULL DEFAULT 1,                  -- 是否基础单位：1 是 / 0 否（辅助单位）
    base_unit_id  UUID,                                             -- 换算基准单位（弱引用 biz_unit.id，基础单位为空）
    rate          NUMERIC(18,6) NOT NULL DEFAULT 1,                 -- 换算率：1 本单位 = rate 个基础单位
    status        SMALLINT     NOT NULL DEFAULT 1,                  -- 状态：1 启用 / 0 停用
    sort_order    INT          NOT NULL DEFAULT 0,                  -- 排序号
    remark        TEXT,                                             -- 备注
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间
    create_by     UUID,                                             -- 创建人（弱引用 sys_user.id）
    update_by     UUID,                                             -- 更新人（弱引用 sys_user.id）
    deleted_at    TIMESTAMPTZ,                                      -- 软删时间（非空即已删除）

    CONSTRAINT ck_biz_unit_rate   CHECK (rate > 0),
    CONSTRAINT ck_biz_unit_is_base CHECK (is_base IN (0, 1)),
    -- 辅助单位必须指明基准单位；基础单位不允许带基准（防自我循环）
    CONSTRAINT ck_biz_unit_base   CHECK ((is_base = 1 AND base_unit_id IS NULL)
                                      OR (is_base = 0 AND base_unit_id IS NOT NULL)),
    CONSTRAINT ck_biz_unit_no_self CHECK (base_unit_id IS NULL OR base_unit_id <> id)
);

CREATE UNIQUE INDEX uk_biz_unit_code ON biz_unit (unit_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_unit_base_unit ON biz_unit (base_unit_id);
CREATE INDEX idx_biz_unit_status ON biz_unit (status);
```

> **换算率的语义**：`1 BOX = 12 PCS` 时，BOX 行 `is_base=0, base_unit_id=PCS.id, rate=12`。本批业务只用基础单位落库（`biz_material.unit_id` 指向基础单位），`rate` 为批次 3 的多单位单据预留。

#### 2.1.2 `biz_warehouse` —— 仓库

```sql
CREATE TABLE biz_warehouse (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    warehouse_code VARCHAR(40)  NOT NULL,                           -- 仓库编码（如 WH01）
    warehouse_name VARCHAR(120) NOT NULL,                           -- 仓库名称
    warehouse_type VARCHAR(20)  NOT NULL DEFAULT 'NORMAL',          -- 仓库类型：NORMAL 普通 / RAW 原料 / FINISHED 成品 / SCRAP 废品 / IN_TRANSIT 在途
    manager        VARCHAR(50),                                     -- 负责人姓名
    allow_negative SMALLINT     NOT NULL DEFAULT 0,                 -- 是否允许负库存：1 允许 / 0 禁止（默认禁止）
    address        VARCHAR(255),                                    -- 仓库地址
    status         SMALLINT     NOT NULL DEFAULT 1,                 -- 状态：1 启用 / 0 停用
    sort_order     INT          NOT NULL DEFAULT 0,                 -- 排序号
    remark         TEXT,                                            -- 备注
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by      UUID,                                            -- 创建人（弱引用 sys_user.id）
    update_by      UUID,                                            -- 更新人（弱引用 sys_user.id）
    deleted_at     TIMESTAMPTZ,                                     -- 软删时间

    CONSTRAINT ck_biz_warehouse_type CHECK (warehouse_type IN ('NORMAL','RAW','FINISHED','SCRAP','IN_TRANSIT')),
    CONSTRAINT ck_biz_warehouse_neg  CHECK (allow_negative IN (0, 1))
);

CREATE UNIQUE INDEX uk_biz_warehouse_code ON biz_warehouse (warehouse_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_warehouse_status ON biz_warehouse (status);
```

#### 2.1.3 `biz_account` —— 会计科目

```sql
CREATE TABLE biz_account (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    account_code      VARCHAR(30)  NOT NULL,                        -- 科目编码（如 1405 / 2202.01，按级次分段）
    account_name      VARCHAR(120) NOT NULL,                        -- 科目名称（如 库存商品 / 应付账款—暂估应付款）
    parent_id         UUID,                                         -- 父科目 ID（弱引用 biz_account.id，一级科目为空）
    level             SMALLINT     NOT NULL DEFAULT 1,              -- 科目级次（1 一级 / 2 二级 / ...）
    account_category  VARCHAR(20)  NOT NULL,                        -- 科目类别：ASSET 资产 / LIABILITY 负债 / EQUITY 权益 / COST 成本 / PROFIT_LOSS 损益
    balance_direction VARCHAR(6)   NOT NULL,                        -- 余额方向：DEBIT 借 / CREDIT 贷
    is_leaf           SMALLINT     NOT NULL DEFAULT 1,              -- 是否末级科目：1 末级（可挂分录）/ 0 非末级（仅汇总）
    aux_dimensions    VARCHAR(120),                                 -- 辅助核算维度（逗号分隔：SUPPLIER,MATERIAL,WAREHOUSE,DEPT）
    status            SMALLINT     NOT NULL DEFAULT 1,              -- 状态：1 启用 / 0 停用
    remark            TEXT,                                         -- 备注
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,

    CONSTRAINT ck_biz_account_category CHECK (account_category IN ('ASSET','LIABILITY','EQUITY','COST','PROFIT_LOSS')),
    CONSTRAINT ck_biz_account_direction CHECK (balance_direction IN ('DEBIT','CREDIT')),
    CONSTRAINT ck_biz_account_level CHECK (level >= 1),
    CONSTRAINT ck_biz_account_leaf CHECK (is_leaf IN (0, 1)),
    CONSTRAINT ck_biz_account_no_self CHECK (parent_id IS NULL OR parent_id <> id)
);

-- 科目编码全表唯一：**不带** WHERE deleted_at IS NULL（见下方说明）
CREATE UNIQUE INDEX uk_biz_account_code ON biz_account (account_code);
CREATE INDEX idx_biz_account_parent   ON biz_account (parent_id);
CREATE INDEX idx_biz_account_category ON biz_account (account_category);
CREATE INDEX idx_biz_account_leaf     ON biz_account (is_leaf, status);
```

> **⚠️ 唯一索引有意不带软删条件（全库唯一）**
> `biz_voucher_entry` 会快照 `account_code` / `account_name`。若允许软删后编码复用，同一编码在不同年份指向不同科目，历史凭证的科目语义会错乱——这在会计上是不可接受的。因此科目编码**一旦用过永久占用**；停用科目用 `status=0`，绝不软删已发生业务的科目（由应用层强制，§3.1.7）。
> 这是本设计中**唯一**偏离"唯一索引带 `WHERE deleted_at IS NULL`"的地方。

> **`is_leaf` 与 `level` 的维护规则**：新增子科目时，若父科目原本 `is_leaf=1` 且已有分录，**拒绝新增**并提示"父科目已发生业务，不可降级为非末级"（应用层校验，§3.1.7）。只允许往 `is_leaf=1` 且无分录的科目下挂子科目。

#### 2.1.4 `biz_material` —— 物料 / 商品（SKU）

```sql
CREATE TABLE biz_material (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),  -- 主键
    material_code       VARCHAR(40)   NOT NULL,                      -- 物料编码（SKU 编码，全局唯一）
    material_name       VARCHAR(120)  NOT NULL,                      -- 物料名称
    spec                VARCHAR(200),                                -- 规格型号
    unit_id             UUID          NOT NULL,                      -- 计量单位（弱引用 biz_unit.id，基础单位）
    material_type       VARCHAR(20)   NOT NULL DEFAULT 'RAW',        -- 物料类型：RAW 原料 / SEMI 半成品 / FINISHED 成品 / SERVICE 服务
    default_warehouse_id UUID,                                       -- 默认仓库（弱引用 biz_warehouse.id，采购单默认带出）
    safety_stock        NUMERIC(18,4) NOT NULL DEFAULT 0,            -- 安全库存（低于此值前端预警，本月不做自动补货）
    costing_method      VARCHAR(20)   NOT NULL DEFAULT 'MOVING_AVG', -- 计价方式：MOVING_AVG 移动加权 / FIFO 先进先出（本批只实现 MOVING_AVG）
    inventory_account_id UUID,                                       -- 存货科目（弱引用 biz_account.id，为空时走规则解析 §3.1.4）
    tax_rate            NUMERIC(7,4)  NOT NULL DEFAULT 0,            -- 默认进项税率（如 0.1300）
    status              SMALLINT      NOT NULL DEFAULT 1,            -- 状态：1 启用 / 0 停用
    remark              TEXT,                                        -- 备注
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,

    CONSTRAINT ck_biz_material_type    CHECK (material_type IN ('RAW','SEMI','FINISHED','SERVICE')),
    CONSTRAINT ck_biz_material_costing CHECK (costing_method IN ('MOVING_AVG','FIFO','STANDARD')),
    CONSTRAINT ck_biz_material_safety  CHECK (safety_stock >= 0),
    CONSTRAINT ck_biz_material_tax     CHECK (tax_rate >= 0 AND tax_rate <= 1)
);

CREATE UNIQUE INDEX uk_biz_material_code ON biz_material (material_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_material_unit   ON biz_material (unit_id);
CREATE INDEX idx_biz_material_wh     ON biz_material (default_warehouse_id);
CREATE INDEX idx_biz_material_type   ON biz_material (material_type, status);
CREATE INDEX idx_biz_material_status ON biz_material (status);
```

> **`SERVICE` 类型的处理**：服务类物料不入库存（收货时跳过库存与成本计算，只生成应付+凭证）。本批**不实现**该分支，但 `material_type` 枚举留出位置，避免批次 2 加类型时改表。

#### 2.1.5 `biz_supplier` —— 供应商

```sql
CREATE TABLE biz_supplier (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    supplier_code        VARCHAR(40)   NOT NULL,                     -- 供应商编码
    supplier_name        VARCHAR(200)  NOT NULL,                     -- 供应商全称
    short_name           VARCHAR(100),                               -- 简称
    contact_person       VARCHAR(50),                                -- 联系人
    contact_phone        VARCHAR(30),                                -- 联系电话
    email                VARCHAR(120),                               -- 邮箱
    tax_no               VARCHAR(50),                                -- 纳税人识别号
    address              VARCHAR(255),                               -- 地址
    credit_days          INT           NOT NULL DEFAULT 0,           -- 账期天数（0 = 现结）
    credit_limit         NUMERIC(18,4) NOT NULL DEFAULT 0,           -- 信用额度（0 = 不限）
    payable_account_id   UUID,                                       -- 应付科目（弱引用 biz_account.id，为空走规则解析）
    invoice_title        VARCHAR(200),                               -- 开票名称
    invoice_bank_name    VARCHAR(200),                               -- 开票开户行
    invoice_bank_account VARCHAR(64),                                -- 开票银行账号
    status               SMALLINT      NOT NULL DEFAULT 1,           -- 状态：1 启用 / 0 停用
    remark               TEXT,                                       -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_supplier_credit_days  CHECK (credit_days >= 0),
    CONSTRAINT ck_biz_supplier_credit_limit CHECK (credit_limit >= 0)
);

CREATE UNIQUE INDEX uk_biz_supplier_code ON biz_supplier (supplier_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_supplier_name   ON biz_supplier (supplier_name);
CREATE INDEX idx_biz_supplier_status ON biz_supplier (status);
```

#### 2.1.6 `biz_accounting_period` —— 会计期间

```sql
CREATE TABLE biz_accounting_period (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    period_year  SMALLINT    NOT NULL,                              -- 会计年度（如 2026）
    period_month SMALLINT    NOT NULL,                              -- 会计月份（1-12）
    period_code  VARCHAR(7)  NOT NULL,                              -- 期间编码（yyyy-MM，如 2026-10；凭证据此归期）
    start_date   DATE        NOT NULL,                              -- 期间开始日
    end_date     DATE        NOT NULL,                              -- 期间结束日
    status       VARCHAR(16) NOT NULL DEFAULT 'NOT_OPENED',         -- 期间状态：NOT_OPENED 未开启 / OPEN 已开启 / CLOSED 已结账
    closed_at    TIMESTAMPTZ,                                       -- 结账时间（批次 4 写入）
    closed_by    UUID,                                              -- 结账人（批次 4 写入）
    remark       TEXT,                                              -- 备注
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_period_month  CHECK (period_month BETWEEN 1 AND 12),
    CONSTRAINT ck_biz_period_status CHECK (status IN ('NOT_OPENED','OPEN','CLOSED')),
    CONSTRAINT ck_biz_period_range  CHECK (start_date <= end_date)
);

CREATE UNIQUE INDEX uk_biz_period_code   ON biz_accounting_period (period_code);
CREATE UNIQUE INDEX uk_biz_period_ym     ON biz_accounting_period (period_year, period_month);
CREATE INDEX idx_biz_period_status       ON biz_accounting_period (status);
```

> 本表**无 `deleted_at`**（期间不可删除，否则历史凭证失去归期依据），模型侧 `#[model(hard_delete)]`。

#### 2.1.7 `biz_seq` —— 单据编号规则

```sql
CREATE TABLE biz_seq (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    seq_type     VARCHAR(32) NOT NULL,                              -- 单据类型键：PO 采购订单 / IN 入库单 / PAY 应付单 / VCH 凭证
    seq_name     VARCHAR(50) NOT NULL,                              -- 规则名称（如 采购订单号）
    prefix       VARCHAR(12) NOT NULL,                              -- 编号前缀（如 PO）
    date_pattern VARCHAR(8)  NOT NULL DEFAULT 'yyyyMMdd',           -- 日期段格式：yyyyMMdd / yyyyMM / none
    period_key   VARCHAR(8)  NOT NULL DEFAULT '',                   -- 当前计数周期键（如 20261027，按此键滚动重置）
    current_no   BIGINT      NOT NULL DEFAULT 0,                    -- 当前流水号（已用最大值）
    step         INT         NOT NULL DEFAULT 1,                    -- 步长
    separator    VARCHAR(4)  NOT NULL DEFAULT '-',                  -- 段分隔符
    status       SMALLINT    NOT NULL DEFAULT 1,                    -- 状态：1 启用 / 0 停用
    remark       TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,

    CONSTRAINT ck_biz_seq_step CHECK (step > 0),
    CONSTRAINT ck_biz_seq_no   CHECK (current_no >= 0)
);

CREATE UNIQUE INDEX uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL;
```

**原子取号（单语句，PG 行锁保证并发安全）**：

```sql
UPDATE biz_seq
   SET current_no = CASE WHEN period_key = $2 THEN current_no + step ELSE step END,
       period_key  = $2,
       updated_at  = CURRENT_TIMESTAMP
 WHERE seq_type = $1 AND deleted_at IS NULL
RETURNING prefix, date_pattern, separator, current_no;
```

最终单号 = `prefix + separator + 日期段 + separator + lpad(current_no, 6, '0')`，例：`PO-20261027-000123`。

> **为什么不用 Redis 取号**：见 D10。同一语句既读又写、`RETURNING` 回传新值，PG 在这一行上串行化，天然无重号；而 Redis 自增需要额外保证"Redis 与 DB 双写一致"，反而多一个故障面。代价是这一行成为写热点——单据量日均百级，**不构成瓶颈**（§8.2 有量化）。

---

### 2.2 采购订单（迁移 `V0031__create_biz_purchase_order`）

#### 2.2.1 `biz_purchase_order` —— 采购订单主表

```sql
CREATE TABLE biz_purchase_order (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    order_no       VARCHAR(32)   NOT NULL,                          -- 采购订单号（biz_seq 生成，全局唯一）
    supplier_id    UUID          NOT NULL,                          -- 供应商（弱引用 biz_supplier.id）
    order_date     DATE          NOT NULL,                          -- 订单日期
    expect_date    DATE,                                            -- 期望到货日期
    warehouse_id   UUID          NOT NULL,                          -- 默认收货仓库（弱引用 biz_warehouse.id）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：DRAFT/SUBMITTED/APPROVED/PARTIAL_RECEIVED/RECEIVED/CLOSED/CANCELLED
    currency_code  CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种（ISO 4217）
    total_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 订单数量合计
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 不含税金额合计
    total_tax      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 税额合计
    total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 价税合计
    credit_days    INT           NOT NULL DEFAULT 0,                -- 账期天数快照（下单时从供应商带出，后续供应商改账期不影响在途单）
    remark         TEXT,                                            -- 备注
    submitted_at   TIMESTAMPTZ,                                     -- 提交时间
    submitted_by   UUID,                                            -- 提交人
    approved_at    TIMESTAMPTZ,                                     -- 审批通过时间（本批为直接置位，无审批流）
    approved_by    UUID,                                            -- 审批人
    closed_at      TIMESTAMPTZ,                                     -- 关闭时间
    closed_by      UUID,                                            -- 关闭人
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_po_status CHECK (status IN
        ('DRAFT','SUBMITTED','APPROVED','PARTIAL_RECEIVED','RECEIVED','CLOSED','CANCELLED')),
    CONSTRAINT ck_biz_po_qty    CHECK (total_qty >= 0),
    CONSTRAINT ck_biz_po_amount CHECK (total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    -- 价税合计恒等：合计口径必须自洽，防程序算错
    CONSTRAINT ck_biz_po_tax_sum CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_po_credit  CHECK (credit_days >= 0)
);

CREATE UNIQUE INDEX uk_biz_po_no     ON biz_purchase_order (order_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_po_supplier     ON biz_purchase_order (supplier_id, order_date DESC);
CREATE INDEX idx_biz_po_status       ON biz_purchase_order (status);
CREATE INDEX idx_biz_po_date         ON biz_purchase_order (order_date DESC);
CREATE INDEX idx_biz_po_warehouse    ON biz_purchase_order (warehouse_id);
```

> **`ck_biz_po_tax_sum` 是数据库层的会计兜底**：税额一律由 `价税合计 - 不含税金额` 倒挤（§9.1 POC-4 实证行级四舍五入会产生 ±0.01 尾差），保证恒等式成立。任何程序 bug 都会被这条约束拦住而不是写出错账。

#### 2.2.2 `biz_purchase_order_item` —— 采购订单明细

```sql
CREATE TABLE biz_purchase_order_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),     -- 主键
    order_id        UUID          NOT NULL REFERENCES biz_purchase_order(id) ON DELETE CASCADE, -- 所属订单
    line_no         INT           NOT NULL,                         -- 行号（从 1 起）
    material_id     UUID          NOT NULL,                         -- 物料（弱引用 biz_material.id）
    unit_id         UUID          NOT NULL,                         -- 单位（弱引用 biz_unit.id，下单时快照）
    qty             NUMERIC(18,4) NOT NULL,                         -- 订购数量
    price           NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 不含税单价
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,               -- 税率
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 不含税金额
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 税额
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 价税合计
    received_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 已收货数量（部分收货累计）
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_po_item_line UNIQUE (order_id, line_no),
    CONSTRAINT ck_biz_po_item_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_po_item_recv CHECK (received_qty >= 0 AND received_qty <= qty),
    CONSTRAINT ck_biz_po_item_tax  CHECK (with_tax_amount = amount + tax_amount)
);

CREATE INDEX idx_biz_po_item_order    ON biz_purchase_order_item (order_id);
CREATE INDEX idx_biz_po_item_material ON biz_purchase_order_item (material_id);
```

> 明细表**无 `deleted_at`**（`#[model(hard_delete)]`）：明细随主单编辑整体重建（先删后插，在事务内），不保留被删行的历史——历史以已过账的入库单与凭证为准，这才是审计口径。

---

### 2.3 收货入库与库存（迁移 `V0032__create_biz_stock`）

#### 2.3.1 `biz_stock_in` —— 收货入库单主表

```sql
CREATE TABLE biz_stock_in (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    in_no          VARCHAR(32)   NOT NULL,                          -- 入库单号（IN 前缀）
    in_type        VARCHAR(20)   NOT NULL DEFAULT 'PURCHASE',       -- 入库类型：PURCHASE 采购入库 / RETURN 退货入库（批次 2）/ OTHER 其他入库
    source_order_id UUID,                                           -- 来源采购订单（弱引用 biz_purchase_order.id，可空=无源入库）
    supplier_id    UUID,                                            -- 供应商（弱引用 biz_supplier.id，采购入库必填）
    warehouse_id   UUID          NOT NULL,                          -- 收货仓库（弱引用 biz_warehouse.id）
    in_date        DATE          NOT NULL,                          -- 入库日期（决定所属会计期间）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：DRAFT/POSTED/CANCELLED
    currency_code  CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种
    total_qty      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 数量合计
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 不含税金额合计（= 入库成本合计）
    total_tax      NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 税额合计
    total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 价税合计
    voucher_id     UUID,                                            -- 过账时生成的凭证（弱引用 biz_voucher.id，冲销后指向红字凭证）
    payable_id     UUID,                                            -- 过账时生成的应付单（弱引用 biz_payable.id）
    remark         TEXT,
    posted_at      TIMESTAMPTZ,                                     -- 过账时间
    posted_by      UUID,                                            -- 过账人
    cancelled_at   TIMESTAMPTZ,                                     -- 作废时间
    cancelled_by   UUID,                                            -- 作废人
    cancel_reason  VARCHAR(255),                                    -- 作废原因
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_si_status CHECK (status IN ('DRAFT','POSTED','CANCELLED')),
    CONSTRAINT ck_biz_si_type   CHECK (in_type IN ('PURCHASE','RETURN','OTHER')),
    CONSTRAINT ck_biz_si_tax    CHECK (total_with_tax = total_amount + total_tax),
    CONSTRAINT ck_biz_si_qty    CHECK (total_qty >= 0),
    -- 采购入库必须有供应商，否则应付与暂估凭证无法生成
    CONSTRAINT ck_biz_si_supplier CHECK (in_type <> 'PURCHASE' OR supplier_id IS NOT NULL)
);

CREATE UNIQUE INDEX uk_biz_si_no     ON biz_stock_in (in_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_si_source       ON biz_stock_in (source_order_id);
CREATE INDEX idx_biz_si_supplier     ON biz_stock_in (supplier_id, in_date DESC);
CREATE INDEX idx_biz_si_status       ON biz_stock_in (status);
CREATE INDEX idx_biz_si_date         ON biz_stock_in (in_date DESC);
CREATE INDEX idx_biz_si_warehouse    ON biz_stock_in (warehouse_id, in_date DESC);
CREATE INDEX idx_biz_si_voucher      ON biz_stock_in (voucher_id);
```

#### 2.3.2 `biz_stock_in_item` —— 入库明细

```sql
CREATE TABLE biz_stock_in_item (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),     -- 主键
    stock_in_id     UUID          NOT NULL REFERENCES biz_stock_in(id) ON DELETE CASCADE, -- 所属入库单
    line_no         INT           NOT NULL,                         -- 行号
    source_item_id  UUID,                                           -- 来源采购订单明细（弱引用 biz_purchase_order_item.id）
    material_id     UUID          NOT NULL,                         -- 物料（弱引用 biz_material.id）
    unit_id         UUID          NOT NULL,                         -- 单位（弱引用 biz_unit.id）
    qty             NUMERIC(18,4) NOT NULL,                         -- 入库数量
    price           NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 不含税单价（采购价）
    tax_rate        NUMERIC(7,4)  NOT NULL DEFAULT 0,               -- 税率
    amount          NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 不含税金额（= 入库成本金额）
    tax_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 税额
    with_tax_amount NUMERIC(18,4) NOT NULL DEFAULT 0,               -- 价税合计
    cost_price      NUMERIC(18,6) NOT NULL DEFAULT 0,               -- 入库后移动加权单价快照（过账时写入）
    remark          VARCHAR(255),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_si_item_line UNIQUE (stock_in_id, line_no),
    CONSTRAINT ck_biz_si_item_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_si_item_tax  CHECK (with_tax_amount = amount + tax_amount)
);

CREATE INDEX idx_biz_si_item_in       ON biz_stock_in_item (stock_in_id);
CREATE INDEX idx_biz_si_item_material ON biz_stock_in_item (material_id);
CREATE INDEX idx_biz_si_item_src      ON biz_stock_in_item (source_item_id);
```

> **`cost_price` 是快照而非现值**：入库那一刻的移动加权单价会随后续入库变化，但已入账的成本金额不可变（会计上是历史成本）。因此每行都存当时的单价快照，用于成本追溯与红冲反算。

#### 2.3.3 `biz_inventory` —— 即时库存（物料 × 仓库 唯一行）

```sql
CREATE TABLE biz_inventory (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    material_id  UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）
    warehouse_id UUID          NOT NULL,                            -- 仓库（弱引用 biz_warehouse.id）
    qty          NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 结存数量
    locked_qty   NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 锁定量（预留：批次 2 销售出库占用）
    avg_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,                  -- 移动加权平均单价
    amount       NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 结存金额（= qty × avg_cost 的账面值，以本列为准）
    last_txn_at  TIMESTAMPTZ,                                       -- 最近一次出入库时间
    created_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_inv_locked CHECK (locked_qty >= 0)
);

-- 物料×仓库 唯一（含已删行：库存行永不复用，物理删除表）
CREATE UNIQUE INDEX uk_biz_inv_mat_wh  ON biz_inventory (material_id, warehouse_id);
CREATE INDEX idx_biz_inv_warehouse     ON biz_inventory (warehouse_id);
CREATE INDEX idx_biz_inv_material      ON biz_inventory (material_id);
```

> **为什么不加 `CHECK (qty >= 0)`**：仓库级有 `allow_negative` 开关，是否允许负库存是**业务策略**而非数据结构约束。若加 DB CHECK，"允许负库存"的仓库就无法工作；且本批不实现出库，负库存只可能来自数据修复。→ 由应用层按 `allow_negative` 校验（§3.2.4），DB 只保 `locked_qty >= 0`。
>
> **表无 `deleted_at`**（`#[model(hard_delete)]`）：库存行是"物料×仓库"的永久存在，随业务清零（`qty=0, amount=0`），不作删除。

#### 2.3.4 `biz_inventory_txn` —— 库存流水（追加写，成本追溯的唯一真相）

```sql
CREATE TABLE biz_inventory_txn (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    material_id   UUID          NOT NULL,                           -- 物料（弱引用 biz_material.id）
    warehouse_id  UUID          NOT NULL,                           -- 仓库（弱引用 biz_warehouse.id）
    txn_type      VARCHAR(8)    NOT NULL,                           -- 方向：IN 入库 / OUT 出库
    biz_type      VARCHAR(24)   NOT NULL,                           -- 业务类型：PURCHASE_IN / PURCHASE_RETURN / SALE_OUT / PRODUCTION_IN / ADJUST_IN / ADJUST_OUT
    doc_type      VARCHAR(24)   NOT NULL,                           -- 来源单据类型：STOCK_IN / STOCK_OUT / ...
    doc_id        UUID          NOT NULL,                           -- 来源单据 ID
    doc_no        VARCHAR(32)   NOT NULL,                           -- 来源单号（冗余，便于直接查）
    line_no       INT           NOT NULL DEFAULT 1,                 -- 来源单据行号
    qty           NUMERIC(18,4) NOT NULL,                           -- 本次数量（正数，方向看 txn_type）
    unit_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 本次单价（入库=采购单价；出库=当时的移动均价）
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 本次金额（正数）
    before_qty    NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动前结存数量
    before_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动前结存金额
    before_cost   NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 变动前移动均价
    after_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动后结存数量
    after_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 变动后结存金额
    after_cost    NUMERIC(18,6) NOT NULL DEFAULT 0,                 -- 变动后移动均价
    period_code   VARCHAR(7)    NOT NULL,                           -- 所属会计期间（yyyy-MM，与凭证同期）
    operator_id   UUID,                                             -- 操作人
    remark        VARCHAR(255),
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 发生时间（追加写表，即操作时刻）

    CONSTRAINT ck_biz_inv_txn_type CHECK (txn_type IN ('IN','OUT')),
    CONSTRAINT ck_biz_inv_txn_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_inv_txn_amt  CHECK (amount >= 0)
);

CREATE INDEX idx_biz_inv_txn_mat  ON biz_inventory_txn (material_id, warehouse_id, created_at DESC);
CREATE INDEX idx_biz_inv_txn_doc  ON biz_inventory_txn (doc_type, doc_id);
CREATE INDEX idx_biz_inv_txn_no   ON biz_inventory_txn (doc_no);
CREATE INDEX idx_biz_inv_txn_per  ON biz_inventory_txn (period_code, material_id);
CREATE INDEX idx_biz_inv_txn_time ON biz_inventory_txn (created_at DESC);
```

> **六个前后快照字段（`before_*` / `after_*`）是成本追溯的硬要求**：只有流水金额没有前后结存，事后无法验证移动加权算得对不对，也无法在成本异常时定位到"哪一笔入库把均价带偏"。审计要的就是这条链。
>
> 追加写表：`#[model(no_timestamps, hard_delete)]`，**不提供任何修改/删除接口**，纠错靠红冲（生成反向流水）。

---
### 2.4 应付（迁移 `V0033__create_biz_payable`）

#### 2.4.1 `biz_payable` —— 应付单

```sql
CREATE TABLE biz_payable (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    payable_no    VARCHAR(32)   NOT NULL,                           -- 应付单号（PAY 前缀）
    supplier_id   UUID          NOT NULL,                           -- 供应商（弱引用 biz_supplier.id）
    source_type   VARCHAR(24)   NOT NULL DEFAULT 'STOCK_IN',        -- 来源单据类型：STOCK_IN / MANUAL
    source_id     UUID          NOT NULL,                           -- 来源单据 ID
    source_no     VARCHAR(32)   NOT NULL,                           -- 来源单号（冗余，便于直接展示）
    invoice_no    VARCHAR(50),                                      -- 发票号（本批为空，批次 2 采购发票回填）
    invoice_date  DATE,                                             -- 发票日期
    currency_code CHAR(3)       NOT NULL DEFAULT 'CNY',             -- 币种
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 应付金额（含税）
    paid_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 已付金额（批次 2 付款回写；本批为手工核销值）
    unpaid_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 未付金额（= amount - paid_amount，由应用层维护并受 CHECK 约束）
    credit_days   INT           NOT NULL DEFAULT 0,                 -- 账期天数快照
    due_date      DATE          NOT NULL,                           -- 到期日（= 入库日期 + 账期）
    period_code   VARCHAR(7)    NOT NULL,                           -- 所属会计期间
    status        VARCHAR(20)   NOT NULL DEFAULT 'OPEN',            -- 状态：OPEN 未付 / PARTIAL_PAID 部分已付 / SETTLED 已结清 / CANCELLED 已作废
    settled_at    TIMESTAMPTZ,                                      -- 结清时间
    voucher_id    UUID,                                             -- 关联凭证（弱引用 biz_voucher.id）
    remark        TEXT,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    update_by     UUID,
    deleted_at    TIMESTAMPTZ,

    CONSTRAINT ck_biz_payable_status CHECK (status IN ('OPEN','PARTIAL_PAID','SETTLED','CANCELLED')),
    CONSTRAINT ck_biz_payable_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_payable_paid   CHECK (paid_amount >= 0 AND paid_amount <= amount),
    -- 未付金额恒等：程序算错会被数据库拦住
    CONSTRAINT ck_biz_payable_unpaid CHECK (unpaid_amount = amount - paid_amount),
    CONSTRAINT ck_biz_payable_credit CHECK (credit_days >= 0)
);

CREATE UNIQUE INDEX uk_biz_payable_no ON biz_payable (payable_no) WHERE deleted_at IS NULL;
-- 幂等铁律：同一来源单据只能生成一张有效应付单（过账重试/并发不会产生重复应付）
CREATE UNIQUE INDEX uk_biz_payable_source ON biz_payable (source_type, source_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_payable_supplier ON biz_payable (supplier_id, status);
CREATE INDEX idx_biz_payable_due      ON biz_payable (due_date, status);
CREATE INDEX idx_biz_payable_period   ON biz_payable (period_code);
```

> **为什么 `unpaid_amount` 不用 PG 生成列**：`GENERATED ALWAYS AS (amount - paid_amount) STORED` 语法上更严谨，但框架 `#[derive(Model)]` 的 `insert` 会写**结构体全部字段**，写入生成列会直接报错（`cannot insert into generated column`），除非模型侧特殊处理字段排除——而宏没有 per-field 排除能力。因此改用普通列 + `CHECK` 约束达到同等正确性，代价是多一次字段赋值。

---

### 2.5 凭证与总账（迁移 `V0034__create_biz_voucher`）

#### 2.5.1 `biz_voucher` —— 凭证主表

```sql
CREATE TABLE biz_voucher (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    voucher_word        VARCHAR(8)    NOT NULL DEFAULT '记',        -- 凭证字（记 / 收 / 付 / 转）
    voucher_no          VARCHAR(32)   NOT NULL,                     -- 凭证号（期间内连续流水，如 0001；不用全局 uuid）
    period_code         VARCHAR(7)    NOT NULL,                     -- 会计期间（yyyy-MM，落库前必须校验期间状态）
    voucher_date        DATE          NOT NULL,                     -- 凭证日期（决定期间与排序）
    source_type         VARCHAR(24)   NOT NULL DEFAULT 'MANUAL',    -- 来源类型：STOCK_IN / PAYMENT / MANUAL / ...
    source_id           UUID,                                       -- 来源单据 ID
    source_no           VARCHAR(32),                                -- 来源单号
    summary             VARCHAR(255)  NOT NULL DEFAULT '',          -- 凭证摘要（整单级；分录另有明细摘要）
    total_debit         NUMERIC(20,4) NOT NULL DEFAULT 0,           -- 借方合计
    total_credit        NUMERIC(20,4) NOT NULL DEFAULT 0,           -- 贷方合计
    entry_count         INT           NOT NULL DEFAULT 0,           -- 分录行数
    status              VARCHAR(16)   NOT NULL DEFAULT 'POSTED',    -- 状态：POSTED 已过账 / REVERSED 已被冲销
    is_red              SMALLINT      NOT NULL DEFAULT 0,           -- 是否红字（冲销）凭证：1 是 / 0 否
    reversed_voucher_id UUID,                                       -- 红字凭证指向被冲销的原凭证（弱引用 biz_voucher.id）
    posted_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 过账时间
    posted_by           UUID,                                       -- 过账人
    remark              TEXT,
    created_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,

    CONSTRAINT ck_biz_voucher_status CHECK (status IN ('POSTED','REVERSED')),
    CONSTRAINT ck_biz_voucher_red    CHECK (is_red IN (0, 1)),
    -- ★ 借贷平衡：数据库层强制，任何绕过引擎的写路径都写不进不平的凭证
    CONSTRAINT ck_biz_voucher_balance CHECK (total_debit = total_credit),
    CONSTRAINT ck_biz_voucher_positive CHECK (total_debit >= 0 AND total_credit >= 0),
    CONSTRAINT ck_biz_voucher_entries  CHECK (entry_count >= 2),
    -- 红字凭证必须指向原凭证；普通凭证不允许携带该字段
    CONSTRAINT ck_biz_voucher_red_ref CHECK ((is_red = 1 AND reversed_voucher_id IS NOT NULL)
                                          OR (is_red = 0 AND reversed_voucher_id IS NULL)),
    CONSTRAINT ck_biz_voucher_self    CHECK (reversed_voucher_id IS NULL OR reversed_voucher_id <> id)
);

-- 同一期间同一凭证字下凭证号唯一
CREATE UNIQUE INDEX uk_biz_voucher_no ON biz_voucher (voucher_word, period_code, voucher_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_voucher_date    ON biz_voucher (voucher_date DESC);
CREATE INDEX idx_biz_voucher_period  ON biz_voucher (period_code, voucher_word, voucher_no);
CREATE INDEX idx_biz_voucher_source  ON biz_voucher (source_type, source_id);
CREATE INDEX idx_biz_voucher_reverse ON biz_voucher (reversed_voucher_id);
CREATE INDEX idx_biz_voucher_status  ON biz_voucher (status);
```

> **`ck_biz_voucher_balance` 是本批最重要的一条 DDL 约束**。会计第一原则"有借必有贷、借贷必相等"不该只靠程序自律——引擎算完必须校验，数据库再兜一层。这样即使批次 4 有人绕过引擎直接写表，也无法落库不平的凭证。
>
> **`entry_count >= 2`**：单分录凭证在会计上无意义（至少一借一贷）。

#### 2.5.2 `biz_voucher_entry` —— 凭证分录

```sql
CREATE TABLE biz_voucher_entry (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),    -- 主键
    voucher_id       UUID          NOT NULL REFERENCES biz_voucher(id) ON DELETE CASCADE, -- 所属凭证
    line_no          INT           NOT NULL,                        -- 分录行号（从 1 起，按录入顺序）
    summary          VARCHAR(255)  NOT NULL,                        -- 分录摘要（如「采购入库 供应商甲 物料A」）
    account_id       UUID          NOT NULL,                        -- 科目（弱引用 biz_account.id）
    account_code     VARCHAR(30)   NOT NULL,                        -- 科目编码快照（防科目改名后历史凭证语义漂移）
    account_name     VARCHAR(120)  NOT NULL,                        -- 科目名称快照
    debit_amount     NUMERIC(20,4) NOT NULL DEFAULT 0,              -- 借方金额
    credit_amount    NUMERIC(20,4) NOT NULL DEFAULT 0,              -- 贷方金额
    aux_supplier_id  UUID,                                          -- 辅助核算：供应商
    aux_material_id  UUID,                                          -- 辅助核算：物料
    aux_warehouse_id UUID,                                          -- 辅助核算：仓库
    aux_dept_id      UUID,                                          -- 辅助核算：部门（批次 3 使用）
    aux_qty          NUMERIC(18,4),                                 -- 辅助核算：数量（数量金额式科目用）
    aux_extra        JSONB,                                         -- 辅助核算扩展（批次 3 的项目/合同等自定义维度）
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_voucher_entry_line UNIQUE (voucher_id, line_no),
    CONSTRAINT ck_biz_voucher_entry_positive CHECK (debit_amount >= 0 AND credit_amount >= 0),
    -- 单边原则：一条分录只能落在借方或贷方（不允许双边都为 0 或都非 0）
    CONSTRAINT ck_biz_voucher_entry_side CHECK ((debit_amount = 0 AND credit_amount > 0)
                                             OR (debit_amount > 0 AND credit_amount = 0))
);

CREATE INDEX idx_biz_voucher_entry_voucher  ON biz_voucher_entry (voucher_id, line_no);
CREATE INDEX idx_biz_voucher_entry_account  ON biz_voucher_entry (account_id);
CREATE INDEX idx_biz_voucher_entry_supplier ON biz_voucher_entry (aux_supplier_id);
CREATE INDEX idx_biz_voucher_entry_material ON biz_voucher_entry (aux_material_id);
```

**红冲方案：用"借贷互换的反向分录"，不用负数金额**

| 方案 | 优点 | 缺点 | 结论 |
|------|------|------|------|
| A. 红字负数（借 -100 / 贷 -100） | 与原凭证一一对应，一眼看出冲的哪行；中国实务中最常见 | 借贷方出现负数，`CHECK (>=0)` 失效；试算平衡、报表取数、账龄分析都要特殊判负；导出给外部（税务/审计）易被误读 | ✗ |
| B. 借贷互换（原借 100 → 新贷 100） | 所有金额恒为正，报表口径统一；约束可保持严格；试算平衡直接累加 | 冲销凭证的行与原件不同向，"对应关系"需靠 `reversed_voucher_id` 表达 | ✓ **采用** |
| C. 物理删除原凭证 | 最"干净" | 违反会计不可篡改原则，审计不可接受 | ✗ |

方案 B 的实现：`is_red=1` + `reversed_voucher_id=原凭证.id`，分录 = 原分录的 `debit_amount` 与 `credit_amount` **互换**，金额原样。原凭证 `status` 置 `REVERSED`。
`ck_biz_voucher_entry_side` 在方案 B 下依然成立（每行仍单边、恒正），这是选择 B 的直接收益。

#### 2.5.3 `biz_account_balance` —— 科目余额表（总账查询性能表）

```sql
CREATE TABLE biz_account_balance (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),     -- 主键
    period_code     VARCHAR(7)    NOT NULL,                         -- 会计期间（yyyy-MM）
    account_id      UUID          NOT NULL,                         -- 科目（弱引用 biz_account.id）
    account_code    VARCHAR(30)   NOT NULL,                         -- 科目编码快照
    begin_balance   NUMERIC(20,4) NOT NULL DEFAULT 0,               -- 期初余额（恒非负，方向看 begin_direction）
    begin_direction VARCHAR(6)    NOT NULL DEFAULT 'DEBIT',         -- 期初余额方向：DEBIT 借 / CREDIT 贷
    period_debit    NUMERIC(20,4) NOT NULL DEFAULT 0,               -- 本期借方发生额
    period_credit   NUMERIC(20,4) NOT NULL DEFAULT 0,               -- 本期贷方发生额
    end_balance     NUMERIC(20,4) NOT NULL DEFAULT 0,               -- 期末余额（恒非负，方向看 end_direction）
    end_direction   VARCHAR(6)    NOT NULL DEFAULT 'DEBIT',         -- 期末余额方向：DEBIT 借 / CREDIT 贷
    entry_count     INT           NOT NULL DEFAULT 0,               -- 本期涉及分录行数（辅助判断科目是否动过）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_bal_begin_dir CHECK (begin_direction IN ('DEBIT','CREDIT')),
    CONSTRAINT ck_biz_bal_end_dir   CHECK (end_direction IN ('DEBIT','CREDIT')),
    CONSTRAINT ck_biz_bal_begin_pos CHECK (begin_balance >= 0),
    CONSTRAINT ck_biz_bal_end_pos   CHECK (end_balance >= 0),
    CONSTRAINT ck_biz_bal_debit_pos CHECK (period_debit >= 0),
    CONSTRAINT ck_biz_bal_credit_pos CHECK (period_credit >= 0)
);

CREATE UNIQUE INDEX uk_biz_bal_period_account ON biz_account_balance (period_code, account_id);
CREATE INDEX idx_biz_bal_account ON biz_account_balance (account_id, period_code);
CREATE INDEX idx_biz_bal_code    ON biz_account_balance (account_code);
```

**余额方向算法（全科目统一，含"资产类出现贷方余额"这种正常情况）**

设科目 `balance_direction` 为 `D`（借）或 `C`（贷），把"带符号金额"定义为：

```
signed_begin = (begin_direction == DEBIT)  ?  begin_balance : -begin_balance
signed_end   = signed_begin + period_debit - period_credit
end_balance  = |signed_end|
end_direction= (signed_end >= 0) ? DEBIT : CREDIT
```

- 对 `balance_direction=DEBIT` 的科目（资产/成本/损益支出），`signed_begin = +期初`，期末 = 期初 + 借 - 贷 → 与直觉一致；
- 对 `balance_direction=CREDIT` 的科目（负债/权益），期初以 `begin_direction=CREDIT` 存，`signed_begin = -期初`，期末 = -期初 + 借 - 贷 = -(期初 + 贷 - 借)，取绝对值即"贷方余额 = 期初 + 贷 - 借" ✓；
- 出现与科目方向相反的余额（如"累计折旧"贷方余额、"应收账款"出现贷方余额）时，公式**自动翻转方向**，无需特判——这是用"带符号中间量 + 方向列"代替"正负号余额"的核心收益。

> **不存"借方余额（可为负）"单列的理由**：单列带符号余额在报表层必须再判断方向，一旦有人漏判就会出现"负债类科目余额显示为负数"这类经典错账。方向与金额分离存储，读侧零歧义。

**余额表的更新时机与方式**（详见 §3.1.6）：
- 凭证过账时，在**同一事务**内对涉及的每个科目执行 upsert（`INSERT ... ON CONFLICT (period_code, account_id) DO UPDATE SET period_debit = ..., period_credit = ...`）；
- 本期发生额增量累加；`end_balance/end_direction` 每次重算并回写；
- 期初余额 = 上一期间的期末余额（首期手工初始化或为 0）；
- 批次 4 结账时做全量重算校验（`SUM(分录) == 余额表`），本批只做增量维护。

---

### 2.6 凭证规则（迁移 `V0035__create_biz_voucher_rule`）

#### 2.6.1 `biz_voucher_rule` —— 规则头（业务事件 → 科目模板）

```sql
CREATE TABLE biz_voucher_rule (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    rule_code           VARCHAR(50)  NOT NULL,                      -- 规则编码（如 STOCK_IN_DEFAULT）
    rule_name           VARCHAR(100) NOT NULL,                      -- 规则名称
    event_type          VARCHAR(32)  NOT NULL,                      -- 业务事件：STOCK_IN 采购入库 / PAYMENT 付款 / STOCK_OUT 出库
    match_material_type VARCHAR(20),                                -- 匹配条件：物料类型（NULL = 不限）
    match_warehouse_type VARCHAR(20),                               -- 匹配条件：仓库类型（NULL = 不限）
    priority            INT          NOT NULL DEFAULT 100,          -- 优先级（数值小者优先命中）
    is_default          SMALLINT     NOT NULL DEFAULT 0,            -- 是否默认规则（无任何条件命中时兜底）：1 是 / 0 否
    status              SMALLINT     NOT NULL DEFAULT 1,            -- 状态：1 启用 / 0 停用
    remark              TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,

    CONSTRAINT ck_biz_rule_event CHECK (event_type IN ('STOCK_IN','STOCK_OUT','PAYMENT','MANUAL')),
    CONSTRAINT ck_biz_rule_prio  CHECK (priority >= 0),
    CONSTRAINT ck_biz_rule_def   CHECK (is_default IN (0, 1))
);

CREATE UNIQUE INDEX uk_biz_rule_code ON biz_voucher_rule (rule_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_rule_event ON biz_voucher_rule (event_type, status, priority);
```

#### 2.6.2 `biz_voucher_rule_entry` —— 规则分录模板（借贷槽位）

```sql
CREATE TABLE biz_voucher_rule_entry (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),    -- 主键
    rule_id          UUID          NOT NULL REFERENCES biz_voucher_rule(id) ON DELETE CASCADE, -- 所属规则
    line_no          INT           NOT NULL,                        -- 行号（从 1 起）
    direction        VARCHAR(6)    NOT NULL,                        -- 借贷方向：DEBIT 借 / CREDIT 贷
    summary_template VARCHAR(255)  NOT NULL,                        -- 摘要模板，支持占位符 {supplier_name} {material_name} {doc_no} {qty} {date}
    account_source   VARCHAR(24)   NOT NULL,                        -- 科目来源：FIXED / MATERIAL_INVENTORY / SUPPLIER_PAYABLE / PARAM_DEFAULT
    account_id       UUID,                                          -- 固定科目（account_source=FIXED 时必填）
    amount_source    VARCHAR(24)   NOT NULL,                        -- 取数口径：见下方枚举
    group_level      VARCHAR(16)   NOT NULL DEFAULT 'DOC',          -- 分录聚合粒度：LINE 每明细行 / MATERIAL 按物料 / ACCOUNT 按科目 / DOC 整单一条
    aux_supplier     SMALLINT      NOT NULL DEFAULT 0,              -- 是否带供应商辅助核算
    aux_material     SMALLINT      NOT NULL DEFAULT 0,              -- 是否带物料辅助核算
    aux_warehouse    SMALLINT      NOT NULL DEFAULT 0,              -- 是否带仓库辅助核算
    aux_qty          SMALLINT      NOT NULL DEFAULT 0,              -- 是否带数量辅助核算
    sort_order       INT           NOT NULL DEFAULT 0,              -- 分录排序
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_rule_entry_line UNIQUE (rule_id, line_no),
    CONSTRAINT ck_biz_rule_entry_dir  CHECK (direction IN ('DEBIT','CREDIT')),
    CONSTRAINT ck_biz_rule_entry_src  CHECK (account_source IN
        ('FIXED','MATERIAL_INVENTORY','SUPPLIER_PAYABLE','PARAM_DEFAULT')),
    CONSTRAINT ck_biz_rule_entry_amt  CHECK (amount_source IN
        ('LINE_AMOUNT_EXCL','LINE_TAX','LINE_AMOUNT_INCL',
         'DOC_AMOUNT_EXCL','DOC_TAX','DOC_AMOUNT_INCL','WAREHOUSE_COST')),
    CONSTRAINT ck_biz_rule_entry_grp  CHECK (group_level IN ('LINE','MATERIAL','ACCOUNT','DOC')),
    -- FIXED 必须有科目；非 FIXED 不得固定科目（防两处配置打架）
    CONSTRAINT ck_biz_rule_entry_acc  CHECK ((account_source = 'FIXED' AND account_id IS NOT NULL)
                                          OR (account_source <> 'FIXED' AND account_id IS NULL)),
    CONSTRAINT ck_biz_rule_entry_aux  CHECK (aux_supplier IN (0,1) AND aux_material IN (0,1)
                                          AND aux_warehouse IN (0,1) AND aux_qty IN (0,1))
);

CREATE INDEX idx_biz_rule_entry_rule ON biz_voucher_rule_entry (rule_id, line_no);
```

**默认规则种子（`V0037`）——采购入库 → 凭证**

| 行 | 方向 | 科目来源 | 取数口径 | 聚合粒度 | 辅助核算 |
|----|------|---------|---------|---------|---------|
| 1 | DEBIT 借 | `MATERIAL_INVENTORY` | `LINE_AMOUNT_EXCL` | `ACCOUNT` | 物料 + 仓库 |
| 2 | DEBIT 借 | `FIXED`（应交税费—应交增值税—进项税额） | `LINE_TAX` | `ACCOUNT` | 无 |
| 3 | CREDIT 贷 | `SUPPLIER_PAYABLE` | `DOC_AMOUNT_INCL` | `DOC` | 供应商 |

→ 生成结果（示例：2 行物料，不含税 1000 + 500，税 130 + 65，价税 1695）：

```
借 1405 库存商品                1500.0000   （按存货科目聚合，两行物料同科目则合并）
借 2221.01.01 应交税费—进项税额  195.0000   （税额合计）
贷 2202.02 应付账款—暂估应付款   1695.0000   （按供应商，整单一条）
```

> **`group_level` 是规则引擎的关键抽象**：没有它，借方会按明细行拆成一堆同级分录（可读性差、余额明细膨胀），贷方会按行重复生成应付分录（同一个供应商同一张单出现多条应付），这在会计上是错的。分组粒度必须在模板里声明，不能让引擎"猜"。
>
> **`amount_source` 里 `LINE_*` 与 `DOC_*` 混用时**，引擎按 `group_level` 自动选择：`LINE`/`MATERIAL`/`ACCOUNT` 粒度用 `LINE_*` 汇总到目标粒度；`DOC` 粒度用 `DOC_*`。

---

### 2.7 单据操作轨迹（迁移 `V0036__create_biz_doc_log`）

```sql
CREATE TABLE biz_doc_log (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    doc_type      VARCHAR(24) NOT NULL,                             -- 单据类型：PURCHASE_ORDER / STOCK_IN / PAYABLE / VOUCHER
    doc_id        UUID        NOT NULL,                             -- 单据 ID
    doc_no        VARCHAR(32) NOT NULL,                             -- 单据编号（冗余，便于直接查轨迹）
    action        VARCHAR(24) NOT NULL,                             -- 动作：CREATE / UPDATE / SUBMIT / APPROVE / POST / CANCEL / REVERSE / CLOSE
    from_status   VARCHAR(24),                                      -- 变更前状态（CREATE 时为空）
    to_status     VARCHAR(24),                                      -- 变更后状态
    operator_id   UUID,                                             -- 操作人 ID（弱引用 sys_user.id）
    operator_name VARCHAR(50),                                      -- 操作人账号（冗余，便于直接展示）
    detail        JSONB,                                            -- 变更详情（金额变化、差异说明、作废原因等，非敏感）
    request_id    VARCHAR(36),                                      -- 请求追踪 ID（与 operation_logs.request_id 可关联，形成"接口日志 ↔ 业务轨迹"互查）
    ip            INET,                                             -- 操作来源 IP
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP    -- 动作发生时间（追加写，即操作时刻）
);

CREATE INDEX idx_biz_doc_log_doc  ON biz_doc_log (doc_type, doc_id, created_at);
CREATE INDEX idx_biz_doc_log_no   ON biz_doc_log (doc_no);
CREATE INDEX idx_biz_doc_log_time ON biz_doc_log (created_at DESC);
CREATE INDEX idx_biz_doc_log_op   ON biz_doc_log (operator_id, created_at DESC);
```

- 追加写表：`#[model(no_timestamps, hard_delete)]`；
- **写入必须在业务事务内**（与单据状态变更同生共死），这是它与 `operation_logs` 的本质区别（§1.5）；
- `request_id` 与中间件日志对齐：`operation_logs.request_id` 由中间件生成并通过响应头下发，ERP handler 内如需取用，从请求头/`Extension` 读取后写入。

### 2.8 操作日志结论（任务书要求明确回答）

**结论：复用 `operation_logs` + 自建 `biz_doc_log`，双写，职责分离。**

| 维度 | `operation_logs`（复用，不动中间件） | `biz_doc_log`（新建） |
|------|-----------------------------------|----------------------|
| 写入方 | `src/middleware/operation_log.rs` 自动，`bus` 异步落库 | ERP handler 显式调用，**业务事务内** |
| 粒度 | 一次接口调用（含请求/响应脱敏快照、耗时、IP、request_id） | 一次单据状态迁移（含 from/to status、动作语义） |
| 事务性 | ❌ 与业务事务无关（回滚后日志仍在） | ✅ 回滚即消失，与账务数据严格一致 |
| 操作描述 | ERP 路径未在 `describe_operation` 中登记 → fallback 为 `POST /api/...` | `POST` / `CANCEL` / `REVERSE` 等业务语义动作 |
| 查询入口 | 系统 → 操作日志（安全审计） | 单据详情页"操作轨迹"标签页（业务审计） |
| 能否改中间件补齐描述 | **不能**：`src/middleware/` 属底座，红线禁改 | — |

**为什么不能只用一个**：
- 只用 `operation_logs`：① 出现"接口日志说过账成功、账上没数据"的假日志（事务回滚后中间件照写）；② 无法按单据查轨迹（要 `LIKE` 请求路径 + JSON 解析，性能差且脆弱）；③ 描述不可读。
- 只用 `biz_doc_log`：丢失接口级安全审计能力（谁调了哪个接口、参数是什么、耗时多少、从哪个 IP），而这是等保/审计的硬要求，且底座已经提供，不该放弃。

---

### 2.9 表清单与软删除策略总表

| # | 表名 | 中文 | 迁移 | 软删 | 模型标注 | 说明 |
|---|------|------|------|------|---------|------|
| 1 | `biz_unit` | 计量单位 | V0030 | ✅ 软删 | 默认 | 主数据 |
| 2 | `biz_warehouse` | 仓库 | V0030 | ✅ 软删 | 默认 | 主数据 |
| 3 | `biz_account` | 会计科目 | V0030 | ✅ 软删（编码永久占用） | 默认 | 主数据，禁止删已用科目 |
| 4 | `biz_material` | 物料/SKU | V0030 | ✅ 软删 | 默认 | 主数据 |
| 5 | `biz_supplier` | 供应商 | V0030 | ✅ 软删 | 默认 | 主数据 |
| 6 | `biz_accounting_period` | 会计期间 | V0030 | ❌ 无 | `hard_delete` | 期间不可删 |
| 7 | `biz_seq` | 单据编号规则 | V0030 | ✅ 软删 | 默认 | 取号规则 |
| 8 | `biz_purchase_order` | 采购订单 | V0031 | ✅ 软删 | 默认 | 单据主表 |
| 9 | `biz_purchase_order_item` | 采购订单明细 | V0031 | ❌ 无 | `hard_delete` | 随主单重建 |
| 10 | `biz_stock_in` | 收货入库单 | V0032 | ✅ 软删 | 默认 | 单据主表 |
| 11 | `biz_stock_in_item` | 入库明细 | V0032 | ❌ 无 | `hard_delete` | 随主单重建 |
| 12 | `biz_inventory` | 即时库存 | V0032 | ❌ 无 | `hard_delete` | 永久行，清零不删 |
| 13 | `biz_inventory_txn` | 库存流水 | V0032 | ❌ 无 | `no_timestamps, hard_delete` | 追加写 |
| 14 | `biz_payable` | 应付单 | V0033 | ✅ 软删 | 默认 | 财务单据 |
| 15 | `biz_voucher` | 凭证主表 | V0034 | ✅ 软删 | 默认 | 财务单据（作废用 REVERSED） |
| 16 | `biz_voucher_entry` | 凭证分录 | V0034 | ❌ 无 | `hard_delete` | 随凭证 CASCADE |
| 17 | `biz_account_balance` | 科目余额 | V0034 | ❌ 无 | `hard_delete` | 按期间 upsert |
| 18 | `biz_voucher_rule` | 凭证规则 | V0035 | ✅ 软删 | 默认 | 配置数据 |
| 19 | `biz_voucher_rule_entry` | 规则分录模板 | V0035 | ❌ 无 | `hard_delete` | 随规则 CASCADE |
| 20 | `biz_doc_log` | 单据操作轨迹 | V0036 | ❌ 无 | `no_timestamps, hard_delete` | 追加写 |

> 20 张表 + 2 张种子数据迁移 = V0030–V0038 共 9 个版本（V0039 预留）。
>
> **软删判定原则**：有独立生命周期、需要"停用而非删除"的主数据 → 软删；单据明细/流水/余额/日志 → 物理删（其历史由已过账的上游单据与凭证承载）。**已过账的单据永不删除**，只用状态流转（`CANCELLED` / `REVERSED`）。

---

### 2.10 迁移文件清单

| 版本 | 文件名 | 内容 | 依赖 |
|------|--------|------|------|
| V0030 | `V0030__create_biz_master_data.up/down.sql` | 主数据 5 表 + 期间 + seq（7 表） | 无 |
| V0031 | `V0031__create_biz_purchase_order.up/down.sql` | 采购订单 2 表 | V0030 |
| V0032 | `V0032__create_biz_stock.up/down.sql` | 入库 2 表 + 库存 2 表 | V0030、V0031 |
| V0033 | `V0033__create_biz_payable.up/down.sql` | 应付 1 表 | V0032 |
| V0034 | `V0034__create_biz_voucher.up/down.sql` | 凭证 2 表 + 余额 1 表 | V0030 |
| V0035 | `V0035__create_biz_voucher_rule.up/down.sql` | 规则 2 表 | V0030、V0034 |
| V0036 | `V0036__create_biz_doc_log.up/down.sql` | 单据轨迹 1 表 | 无 |
| V0037 | `V0037__seed_biz_account_and_rule.up/down.sql` | 会计科目种子（§4.6 标准科目表）+ 默认凭证规则 + 2026 年度期间 + seq 规则 | V0030、V0035 |
| V0038 | `V0038__add_biz_erp_permissions.up/down.sql` | 菜单与按钮权限点（`module_code='business'`，幂等 `ON CONFLICT`） | V0037 |

**写法要求**（对齐 `docs/后端二开指南.md` §9）：
- 建表文件三段式：`CREATE TABLE` → `COMMENT ON TABLE/COLUMN` → 索引；
- `down.sql` 按**反序** `DROP TABLE IF EXISTS`（先删子表再删父表，防 FK 阻塞）；
- 种子幂等：`ON CONFLICT (id) DO NOTHING`（固定 UUID，参照 `V0020` 的写法）；
- 已应用迁移**指纹校验不可改**，演进只加新版本。

### 2.11 ER 关系（核心闭环）

```
biz_unit ──┐
biz_warehouse ──┬──> biz_material ──┐
biz_account ────┘                   │
                                    ▼
biz_supplier ──> biz_purchase_order ──(1:N)──> biz_purchase_order_item
                        │                              │
                        │ source_order_id               │ source_item_id
                        ▼                              ▼
                 biz_stock_in ──(1:N)──> biz_stock_in_item
                        │                      │
        ┌───────────────┼──────────────┐       │ 过账触发
        ▼               ▼              ▼       ▼
  biz_inventory   biz_payable    biz_voucher ◄── biz_voucher_entry
  biz_inventory_txn     │              │
   (流水，含6个前后快照)  │              ▼
                        └──>  biz_account_balance ──> 总账查询

  biz_voucher_rule ──(1:N)──> biz_voucher_rule_entry ──(驱动)──> biz_voucher/entry 生成
  biz_doc_log ──(按 doc_type+doc_id)── 挂任意单据
```

**一次过账写入的 7 类数据**（同一事务）：
1. `biz_inventory`（结存数量/金额/均价）
2. `biz_inventory_txn`（每行一条流水，含前后快照）
3. `biz_payable`（整单一张）
4. `biz_voucher`（整单一张）
5. `biz_voucher_entry`（按规则模板生成 N 条）
6. `biz_account_balance`（涉及科目的期间余额 upsert）
7. `biz_doc_log`（状态迁移轨迹）
+ 反写 `biz_stock_in.status/voucher_id/payable_id` 与 `biz_purchase_order_item.received_qty`

---
## 3. 核心引擎设计

### 3.0 模块结构（先说清"引擎放哪"，这是与本项目红线的交集）

项目红线是"**不设 service 层**"（`AGENTS.md` 约定 1）。但凭证引擎必然是一段跨表、跨子模块的领域逻辑，放在 handler 里会撑爆 handler、放在各 model 里会割裂。因此必须先明确它的归属：

```
backend/src/business/erp/
├── mod.rs                    # 模块文档 + 接口总表
├── shared/                   # 【model 层】跨子模块共用的领域原语（纯函数为主）
│   ├── mod.rs
│   ├── money.rs              # 金额舍入、税额倒挤、尾差分摊（纯函数 + 单测）
│   ├── seq.rs                # 单据取号（biz_seq 原子 UPDATE ... RETURNING）
│   ├── period.rs             # 期间解析/校验（日期 → period_code，期间状态检查）
│   ├── doc_status.rs         # 单据状态机定义与合法迁移表
│   └── doc_log.rs            # 业务轨迹写入（biz_doc_log）
├── unit/{mod,handler,dto,model}.rs
├── warehouse/{mod,handler,dto,model}.rs
├── account/{mod,handler,dto,model}.rs
├── material/{mod,handler,dto,model}.rs
├── supplier/{mod,handler,dto,model}.rs
├── purchase_order/{mod,handler,dto,model}.rs
├── stock_in/{mod,handler,dto,model}.rs
├── inventory/{mod,handler,dto,model}.rs
├── payable/{mod,handler,dto,model}.rs
├── voucher/
│   ├── mod.rs
│   ├── handler.rs            # 凭证查询/手工新增/冲销、总账查询
│   ├── dto.rs
│   ├── model.rs              # 凭证与分录的读写 SQL
│   └── engine.rs             # ★ 凭证生成引擎（领域逻辑）
└── ledger/{mod,handler,dto,model}.rs   # 总账/科目余额查询
```

**为什么 `engine.rs` 不是 service 层**（评审必问，先答）：

| 判据 | service 层的特征 | 本项目 engine 的实际形态 |
|------|-----------------|------------------------|
| 调用链 | handler → service → model → db（**多一层**） | handler → `voucher::engine` → `db()`（**与 handler → model → db 同构，层数不变**） |
| 职责 | 编排多模块业务、承载事务 | 只做"事件 → 分录"的纯领域转换 + 落库，不做跨模块编排；事务由**调用方 handler**（`stock_in/handler.rs`）开启 |
| 依赖方向 | service 反向依赖多个 model | engine 只依赖 `voucher` 自己的 model + `shared` 原语，**不反向依赖 stock_in/payable 的 model**（通过入参结构体解耦） |
| 项目文档依据 | — | `docs/后端二开指南.md` §2.2：「需要自写 SQL / **领域逻辑**的函数集中放这里（model 层）」 |

即：`engine.rs` 是 model 层内部的**文件级拆分**（因为凭证引擎约 500 行，塞进 `model.rs` 会不可维护），不是新增分层。**事务边界始终在 handler**——这一点与项目现有 `src/system/role/handler.rs` 调 `model::replace_role_menus` 的模式完全一致。

---

### 3.1 凭证自动生成引擎

#### 3.1.1 设计目标

| # | 目标 | 落地手段 |
|---|------|---------|
| G1 | 会计正确 | DB 层 `total_debit = total_credit` 约束 + 应用层二次校验 + 单科目末级校验 |
| G2 | 规则可配 | `biz_voucher_rule` / `_entry` 模板表，运营可改分录结构而无需发版 |
| G3 | 科目可扩 | 四级解析链（§3.1.4），主数据与规则各管一层，不互相绑架 |
| G4 | 可审计 | 每张凭证有 `source_type/source_id/source_no` 反查来源单据；每条分录存科目编码快照 |
| G5 | 幂等 | 同一来源单据只能生成一张有效凭证（引擎内先查 `(source_type, source_id)` 且 `status='POSTED'`） |
| G6 | 可冲销 | 红字凭证（借贷互换）+ 原凭证置 `REVERSED`，成对留痕 |

#### 3.1.2 规则配置模型（回答"模板怎么存"）

任务书问"配置表 `biz_voucher_rule` + `biz_voucher_rule_entry`？"——**是，但需要三个额外设计，否则引擎会写死逻辑**：

**① 事件类型（`event_type`）决定用哪套模板**

```
STOCK_IN  → 采购入库（本批实现）
STOCK_OUT → 销售出库（批次 2）
PAYMENT   → 付款（批次 2）
MANUAL    → 手工凭证（不走规则，人直接填分录）
```

**② 分录聚合粒度（`group_level`）**——这是最容易漏的一环：

| 粒度 | 含义 | 用于 |
|------|------|------|
| `LINE` | 每个明细行一条分录 | 不需要合并的场景（极少） |
| `MATERIAL` | 按物料合并 | 需要物料级辅助核算且不同行不同物料 |
| `ACCOUNT` | **按解析后的科目合并**（推荐用于借方） | 库存商品：多行物料同科目合并成一条 |
| `DOC` | 整单一条（推荐用于贷方） | 应付账款：整单一条，按供应商辅助核算 |

若不设粒度，借方会产生"同一科目 N 条分录"（余额明细膨胀、报表难读），贷方会产生"同一供应商 N 条应付"（会计上是错的）。

**③ 条件匹配与优先级**

```
命中规则 = 所有 status=1 且 event_type 匹配的规则中：
            match_material_type 为空或等于本单物料类型
        AND match_warehouse_type 为空或等于本单仓库类型
        按 priority 升序取第一条
若无命中 → 取 is_default=1 的规则
若仍无 → 报错「未配置 {event_type} 的凭证规则，拒绝过账」（绝不静默生成空凭证或跳过）
```

> **为什么"无规则即拒绝过账"而不是"跳过凭证"**：跳过会让库存动了而账没动，账实不符且无人发现。宁可过账失败、让用户去配规则。

#### 3.1.3 变量占位符体系（回答"变量占位符"）

摘要模板 `summary_template` 支持的占位符，在渲染期用**事件上下文**替换：

| 占位符 | 来源 | 示例渲染值 |
|--------|------|-----------|
| `{doc_no}` | 单据号 | `IN-20261027-000012` |
| `{doc_date}` | 业务日期 | `2026-10-27` |
| `{supplier_name}` | `biz_supplier.supplier_name` | `北京某某贸易有限公司` |
| `{supplier_code}` | 供应商编码 | `SUP001` |
| `{material_name}` | `biz_material.material_name`（仅 `LINE`/`MATERIAL` 粒度可用） | `冷轧钢板 1.5mm` |
| `{material_code}` | 物料编码 | `M-1001` |
| `{warehouse_name}` | `biz_warehouse.warehouse_name` | `原料仓` |
| `{qty}` | 该分录对应数量（`DOC` 粒度为合计） | `150.0000` |
| `{amount}` | 该分录对应金额 | `1500.00` |
| `{period}` | 会计期间 | `2026-10` |

**渲染规则（必须约束，否则摘要会不可控）**：
1. 占位符语法严格 `{name}`，仅允许上表列出的名字；遇到未知占位符 → **报错**（不是留空，防拼写错导致全表摘要变成空字符串）；
2. 渲染结果截断到 `VARCHAR(255)`，按**字符**截断（不能用字节切，避免多字节断字，与 `monitor/model.rs` 的 `truncate_chars` 同款处理）；
3. 占位符在 `DOC` 粒度下取单据级聚合值，在 `LINE` 粒度下取行级值——渲染器接收的上下文结构体需标明粒度，不能让渲染器自己猜。

> 不做"表达式/公式"（如 `{amount}*0.13`）：那会把规则表变成一门脚本语言，安全与可维护性双输。税额一律走 `amount_source=LINE_TAX` 从单据字段取——单据上已经算好了（且受 `ck_..._tax` 约束），引擎不该重算。

#### 3.1.4 科目映射策略（回答"物料→存货科目怎么配"）

**方案对比**

| 方案 | 做法 | 优势 | 劣势 | 结论 |
|------|------|------|------|------|
| A. 全部挂主数据 | `material.inventory_account_id` / `supplier.payable_account_id` 必填 | 直观、按 SKU 精确 | ① 新增物料必须先想清科目，运营负担重；② **无法表达"同物料不同业务不同科目"**（采购入库计库存商品、生产领用计生产成本、销售出库计主营业务成本）；③ 主数据是业务人员维护的，会计科目是财务维护的，权限交叉 | ✗ |
| B. 全部挂规则表 | 规则里按 `material_type` 枚举穷举 | 一处配置，集中管控 | ① 同一物料类型要拆多科目（如原料分"主要材料/辅料"）无法区分；② 每个 SKU 的特殊科目需求无法满足 | ✗ |
| C. **槽位 + 四级解析链**（推荐） | 规则模板只声明"这里要一个存货科目"，**取哪个科目按优先级解析** | 主数据可精确覆盖；规则提供兜底；新增物料不配也能跑 | 需要多一层解析逻辑与缓存 | ✓ **采用** |

**四级解析链（`account_source` 决定走哪几级）**

```
第 1 级  单据行显式指定      — 手工凭证 / 特殊业务直接传 account_id（仅 MANUAL 与预留扩展使用）
第 2 级  主数据字段          — MATERIAL_INVENTORY → biz_material.inventory_account_id
                              SUPPLIER_PAYABLE   → biz_supplier.payable_account_id
第 3 级  规则表条件匹配      — 按 material_type / warehouse_type 在 biz_voucher_rule_entry 中找固定科目
第 4 级  系统参数默认        — support/config 的 biz.erp.default_inventory_account / default_payable_account
若四级全空 → 过账失败，错误信息明确指出「物料 X 未配置存货科目，且无默认值」
```

`account_source` 各取值的解析路径：

| `account_source` | 解析路径 |
|-----------------|---------|
| `FIXED` | 直接用 `biz_voucher_rule_entry.account_id`（如"应交税费—进项税额"，全公司一个科目） |
| `MATERIAL_INVENTORY` | 物料主数据 → 物料类型规则 → 默认参数 |
| `SUPPLIER_PAYABLE` | 供应商主数据 → 默认参数（应付科目通常全公司一个，但允许大供应商单独挂账） |
| `PARAM_DEFAULT` | 直接读系统参数（无主数据依赖，用于税费等） |

**解析结果必须校验**（§3.1.7）：科目存在、`status=1`、`is_leaf=1`、类别与方向合理（借方科目的 `balance_direction` 不强制，因为备抵科目存在反向余额，但**告警**）。

**缓存策略**：科目与物料/供应商的科目字段是低频变更数据，规则命中结果按 `(event_type, material_type, warehouse_type)` 做进程内缓存（`Cache` 热查询，TTL 参照 `constants::CACHE_HOT_QUERY_TTL_SECS`），科目/规则/主数据的写操作触发失效。本批可先**不缓存**（过账是低频操作，一次过账多查 3 次表不构成瓶颈），批次 3 再按压测结果决定——避免过早引入缓存一致性问题。

#### 3.1.5 借贷平衡校验与尾差分摊（会计正确性的最后一道闸）

**校验分三层，缺一不可**：

| 层 | 位置 | 检查项 | 失败表现 |
|----|------|--------|---------|
| L1 引擎内 | `voucher/engine.rs` 生成分录后 | ① 借方合计 == 贷方合计 ② 每行单边非零 ③ 分录数 ≥ 2 ④ 至少一行借方、一行贷方 | 返回 `ApiError::bad_request("凭证借贷不平衡：借 X ≠ 贷 Y")`，过账失败 |
| L2 数据库 | `biz_voucher.ck_biz_voucher_balance` + `biz_voucher_entry.ck_biz_voucher_entry_side` | 同 L1 | 事务回滚，接口 500（说明是程序 bug，应被测试拦住） |
| L3 余额表 | 事务末尾 upsert 后 | `SUM(本期借) == SUM(本期贷)`（全科目） | 同上 |

**为什么必须做尾差分摊**（§9.1 POC-4 实证）：

实测数据：3 行含税金额 `100.00 / 200.00 / 300.01`，税率 13%
- 行级除税后再相加：`530.99`
- 按合计除税：`530.98`
- **差 -0.01**

若"借方按行算、贷方按整单算"，就必然借贷不平 1 分钱。处理规则（**唯一允许的做法**）：

```
① 以「整单口径」为准算出总额（不含税合计、税额合计、价税合计）；
② 税额恒由价税合计倒挤：tax_total = with_tax_total - amount_total（不做乘法，避免二次舍入）；
③ 行级拆分仅用于展示与辅助核算，不参与总额；
④ 若业务上必须行级分摊（如成本按行入库）：末行吸收尾差
   last.amount = amount_total - Σ(前 n-1 行 amount)
```

对应到凭证：**贷方应付按 `DOC_AMOUNT_INCL` 取整单合计**（不是行金额之和），借方按 `ACCOUNT` 粒度对 `LINE_AMOUNT_EXCL` 求和——两者都来自单据**同一份** `total_*` 字段（受 `ck_biz_po_tax_sum` / `ck_biz_si_tax` 约束），因此恒等。

> **金额舍入策略：`MidpointAwayFromZero`（四舍五入）**，与财务人员习惯一致。`rust_decimal` 提供 `RoundingStrategy::MidpointAwayFromZero`，§9.1 POC 已用该策略跑通全流程。
> **不使用银行家舍入**（`MidpointNearestEven`）：中国财务实务与凭证核对都以四舍五入为准，用银行家舍入会让"人工核对差 1 分"变成常态解释成本。

#### 3.1.6 科目余额表更新（总账查询的性能基础）

在同一事务末尾，对**本次涉及的全部科目**执行 upsert：

```sql
INSERT INTO biz_account_balance
    (id, period_code, account_id, account_code,
     begin_balance, begin_direction, period_debit, period_credit,
     end_balance, end_direction, entry_count, created_at, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
ON CONFLICT (period_code, account_id) DO UPDATE
   SET period_debit  = biz_account_balance.period_debit  + EXCLUDED.period_debit,
       period_credit = biz_account_balance.period_credit + EXCLUDED.period_credit,
       entry_count   = biz_account_balance.entry_count   + EXCLUDED.entry_count,
       end_balance   = ...,   -- 见下方重算
       end_direction = ...,
       updated_at    = NOW();
```

**实现要点**：
1. `begin_balance / begin_direction` 在**首次插入该期间科目行**时确定：查上一期间的 `end_balance/end_direction`；无上期则取 `0 / 科目.balance_direction`；
2. `end_balance / end_direction` **不靠增量算**，而是 upsert 后按 §2.5.3 的统一公式重算一遍（`signed = signed_begin + debit - credit`）——增量计算在"贷方科目出现借方发生"时容易写错方向，重算虽多一次读写但绝对正确；
3. upsert 的 `ON CONFLICT` 依赖 `uk_biz_bal_period_account`，天然处理并发（同一科目同一期间被两张凭证同时过账时，PG 会串行化这两行的更新）；
4. **本批不做**"期末结转损益 → 下期期初"（批次 4 结账功能）。

#### 3.1.7 生成时机的方案对比（任务书要求给业务差异与推荐）

| 方案 | 会计处理 | 业务差异 | 优势 | 劣势 |
|------|---------|---------|------|------|
| **A. 入库即生成**（贷方记应付账款） | 借 库存商品 / 贷 应付账款 | 货到即挂账，供应商对账以入库单为准 | 垂直闭环最短；无需发票模块；账实同步 | 若发票金额与入库暂估不同，需在批次 2 做差异调整 |
| **A'. 入库即生成（贷方记应付—暂估）** ★ | 借 库存商品 / 贷 应付账款—**暂估应付款** | 货到挂暂估；发票到时冲暂估、按实挂正式应付 | 符合中国实务"货到票未到按暂估入账"；暂估与正式应付分科目，账龄分析不失真；批次 2 冲回路径清晰 | 需多维护一个暂估科目 |
| B. 发票到达才生成 | 借 库存商品 / 贷 应付账款 | 货到不入账（存货账实不符） | 金额绝对准确 | **月末必须手工盘存调整**，否则账实不符；本批无发票模块 → 本批账永远是空的，闭环不成立 |
| C. 入库生成暂估 + 发票生成差异 | 三张凭证（暂估、冲回、正式） | 最贴近大型企业实务 | 最严谨 | 本批实现不了（无发票模块），且复杂度翻倍 |

**推荐：A'（入库即生成，贷方计入 `应付账款—暂估应付款`）**

理由：
1. 任务书要求本批闭环"收货入库 → 库存增加 → 应付账款 → 凭证 → 总账可查"。B 方案下这条链在本批**断在凭证**，等于本批没交付；
2. C 方案需要发票实体，超出本批范围；
3. A' 与 A 的实现成本几乎相同（只差科目解析指向一个不同科目），但**为批次 2 预留了标准冲回路径**：批次 2 新增"采购发票"单据后，走"红冲暂估凭证 + 生成正式凭证"（这正是 C 方案的增量实现，无需改本批的库存/应付结构）；
4. 免责说明：本批的 `biz_payable` 在语义上是**暂估应付**。字段 `invoice_no` / `invoice_date` 为空即暂估，批次 2 回填后转正式。这一点必须在 UI 上明示（应付清单标"暂估"标签），否则财务会误当正式应付去付款。

#### 3.1.8 手工调整与冲销

**能否手工调整已生成的凭证？**
- **不能直接改**。已过账凭证的 `biz_voucher` / `biz_voucher_entry` **不提供任何修改接口**（会计不可篡改原则）。
- 允许的路径只有两条：
  1. **作废整单并重做**：入库单 `POSTED → CANCELLED`（连带红冲），再新建一单重新过账。适用于刚发现错误、尚未跨期。
  2. **生成冲销凭证（红字）**：适用于已跨期或单据本身不能作废的情况（如实物已入库）。

**手工凭证（`MANUAL`）**：允许财务人员直接录入分录（不经过规则），但同样受：
- 借贷平衡校验（L1/L2）；
- 期间状态校验（只能写 `OPEN` 期间）；
- 科目必须末级且启用；
- 分录 ≥ 2 行。

**冲销实现（红字凭证）**

| 步骤 | 动作 |
|------|------|
| 1 | 校验原凭证 `status='POSTED'`（已冲销的不能再冲）、非同凭证已存在红字（`is_red=1 AND reversed_voucher_id=原id` 唯一） |
| 2 | 期间校验：冲销凭证只能开在 `OPEN` 期间（**允许跨期冲销**：原凭证可能在已结账期间，红字开在当前开放期间——这是标准做法，不允许回改已结账期间） |
| 3 | 生成新凭证：`is_red=1`、`reversed_voucher_id=原id`、`source_type/source_id` 继承原凭证 |
| 4 | 分录：原分录的 `debit_amount` 与 `credit_amount` **互换**，`account_id/code/name/aux_*` 原样复制 |
| 5 | 原凭证 `status='REVERSED'` |
| 6 | 余额表：红字凭证正常累加（借变贷），原凭证期间余额**不动**；跨期冲销时原期间余额保持历史正确，当期余额反映冲销影响 |
| 7 | 写 `biz_doc_log`（action=REVERSE，from=POSTED，to=REVERSED），detail 记录原凭证号与红字凭证号 |

**跨期冲销的会计正确性说明**：原凭证在 2026-09 已结账，红字开在 2026-10。9 月报表不变（符合已结账期间不可变原则），10 月体现冲销影响。这是审计接受的标准处理，**不允许**"反结账改 9 月"（批次 4 也不做反结账）。

---
### 3.2 库存成本核算（移动加权平均）

#### 3.2.1 算法定义（含 POC 实测数字）

**变量**：`qty`（结存数量）、`amount`（结存金额）、`avg_cost`（移动加权均价）

```
入库（IN）：
  new_qty    = qty + in_qty
  new_amount = amount + in_amount            # in_amount 来自单据行不含税金额
  new_cost   = round(new_amount / new_qty, 6)   # 6 位小数，四舍五入

出库（OUT）：
  out_amount = round(out_qty × avg_cost, 4)     # 用「当前」均价，不是重算的
  new_qty    = qty - out_qty
  new_amount = amount - out_amount
  new_cost   = avg_cost                          # 出库不改变均价

清空（出库后 new_qty == 0）：
  out_amount = amount                            # ★ 倒挤：全额出清，不用 qty × cost 重算
  new_amount = 0
  new_cost   = avg_cost                          # 保留最后一次均价作参考（见下方一致性校验口径）
```

**POC 实测（§9.1 POC-1/POC-2，代码与输出均为真实执行结果）**

| 步骤 | 数量 | 金额 | 均价（6 位） | 备注 |
|------|------|------|-------------|------|
| 期初 | 100 | 1012.34 | 10.1234 | — |
| 入库 50 @ 11.1111 | 150 | 1567.8950 | **10.452633** | `(1012.34+555.555)/150` |
| 入库 30 @ 9.8765 | 180 | 1864.1900 | **10.356611** | `(1567.895+296.295)/180` |
| 出库 33 | 147 | 1522.4218 | 10.356611 | `round(33×10.356611,4)=341.7682` |
| 校验 | — | `round(147×10.356611,4)=1522.4218` | — | ✅ **与结存金额完全相等** |
| 清空 147 | 0 | 0.0000 | 10.356611 | 倒挤 `out_amount = 1522.4218` |

> 这个结果验证了「金额 4 位 + 单价 6 位 + 出库按均价乘数量」在**不产生残差**的情况下自洽。若单价只存 4 位，上例出库成本会是 `341.7681`，结存金额与 `qty×cost` 差 0.0001，多入库几次就会累积成可见差额。

**一致性校验口径（写进单测与巡检脚本）**：

```
当 qty > 0 时：  round(qty × avg_cost, 4) == amount  必须成立
当 qty = 0 时：  amount == 0 必须成立（avg_cost 保留历史值，不参与校验）
```

巡检 SQL（运维 / 批次 4 结账前必跑）：

```sql
SELECT i.id, i.material_id, i.warehouse_id, i.qty, i.avg_cost, i.amount
  FROM biz_inventory i
 WHERE i.qty > 0
   AND round(i.qty * i.avg_cost, 4) <> i.amount
 UNION ALL
SELECT i.id, i.material_id, i.warehouse_id, i.qty, i.avg_cost, i.amount
  FROM biz_inventory i
 WHERE i.qty = 0 AND i.amount <> 0;
-- 期望返回 0 行
```

#### 3.2.2 并发安全（同一物料同一仓库并发入库）

**方案对比**

| 方案 | 机制 | 优势 | 劣势 | 结论 |
|------|------|------|------|------|
| **A. 悲观行锁** | `SELECT ... FOR UPDATE` 锁 `biz_inventory` 行 | 实现简单；无重试；正确性最强；锁粒度 = 物料×仓库，冲突概率天然低 | 同物料同仓并发时排队 | ✓ **采用** |
| B. 乐观锁 | `UPDATE ... SET version = version+1 WHERE version = $n` | 无锁等待 | 冲突后要**重算整单成本**（不能只重试 UPDATE），重试风暴下复杂度爆炸 | ✗ |
| C. 应用层 Mutex | 进程内 `tokio::sync::Mutex` | 零 DB 开销 | 多实例部署直接失效（本项目支持多实例） | ✗ |
| D. SERIALIZABLE 隔离 | 数据库级串行化 | 理论最强 | 序列化失败需应用重试，死锁概率高，吞吐骤降 | ✗ |
| E. 咨询锁 | `pg_advisory_xact_lock(hashtext(mat\|\|wh))` | 不要求行已存在 | 哈希冲突会误串行；且行仍需 UPDATE，等于多一层锁 | 备选（若将来库存行创建竞争激烈） |

**采用 A 的三段式时序（必须按序执行）**

```sql
-- ① 建行（幂等，不阻塞已存在的行；新物料首次入库必走这一步）
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount,
                           created_at, updated_at)
VALUES (?, ?, ?, 0, 0, 0, 0, NOW(), NOW())
ON CONFLICT (material_id, warehouse_id) DO NOTHING;

-- ② 锁行并取当前结存（★ 必须按 material_id 升序逐个执行，见下方死锁说明）
SELECT id, qty, avg_cost, amount
  FROM biz_inventory
 WHERE material_id = ? AND warehouse_id = ?
   FOR UPDATE;

-- ③ 在 Rust 侧算好新值后整体回写（不做 qty = qty + ? 的增量 UPDATE，
--    因为金额与均价必须基于②读到的旧值计算，增量更新无法保证三者自洽）
UPDATE biz_inventory
   SET qty = ?, amount = ?, avg_cost = ?, last_txn_at = ?, updated_at = NOW()
 WHERE id = ?;
```

**死锁防护（真实工程坑，必须写进实施要求）**

一张入库单有多行明细，若按用户录入顺序加锁：
- 单据 A 行序：物料1 → 物料2
- 单据 B 行序：物料2 → 物料1

两单并发时 A 持物料1 等物料2、B 持物料2 等物料1 → **PG 检测到死锁，杀掉一方**，用户看到莫名其妙的"死锁"报错。

**唯一正确做法**：过账前把明细行按 `material_id`（UUID 比较）**升序排序**，然后按排序后顺序逐行加锁。同一仓库内若一单内同一物料出现多行（不同行同物料），先合并为一行再处理（数量金额相加），避免对同一行加两次锁导致自锁。

> **为什么不能靠一条 `SELECT ... FOR UPDATE` 批量锁**：PostgreSQL 的 `FOR UPDATE` 加锁顺序取决于**表的扫描顺序**，`ORDER BY` 只影响返回顺序、不保证加锁顺序。因此必须"逐行执行 + 自己控制顺序"。

**锁持有时间预算**：单行「锁 → 算 → 写流水 → 写库存」约 4 条 SQL，本地 PG < 5ms；一张 20 行明细的单据全程持锁 < 100ms。验收标准：过账接口 P99 < 300ms（§6 验收标准）。

**幂等（防重复过账）—— 与库存锁同等重要**

过账事务一开始必须先锁单据行：

```sql
SELECT status FROM biz_stock_in WHERE id = ? AND deleted_at IS NULL FOR UPDATE;
-- status != 'DRAFT' → 直接返回 400「单据当前状态 POSTED，不可重复过账」
```

两个请求并发过账同一单据：第二个请求会阻塞在 `FOR UPDATE`，第一个提交后第二个读到 `POSTED` → 拒绝。**没有这一步，两次过账会把库存入两遍。**

#### 3.2.3 精度、舍入与尾差

| 项 | 规则 |
|----|------|
| 舍入策略 | `RoundingStrategy::MidpointAwayFromZero`（四舍五入），全项目统一，**禁止**在各处混用银行家舍入 |
| 数量 | `NUMERIC(18,4)` / `Decimal`，比较用 `Decimal` 相等（不是 f64 epsilon） |
| 单价 | `NUMERIC(18,6)`，**只在入库重算时产生**，出库只读 |
| 金额 | `NUMERIC(18,4)`，所有金额落库前必须 `round_dp(4)` |
| 入库金额来源 | 直接取单据行 `amount`（已由前端/后端按整单口径算好并受 CHECK 约束），**引擎不重算** |
| 出库尾差 | 清空时倒挤（`out_amount = amount`），保证 `qty=0 → amount=0` |
| 多行同物料 | 同单内同物料多行**先合并**再入库，只重算一次均价（否则多次舍入累积误差） |
| 红冲 | 反向流水的金额取原流水金额**原值**，不重算，保证冲销后结存精确回到原值（`amount - 原amount = 原amount_前`） |

> **尾差不允许"挂账"到某个科目**：不少系统会设"成本尾差调整"科目吸收尾差，本批**不采用**——移动加权本身在"数量清零倒挤"下数学上无残差（POC-2 已验证），引入调整科目只会让成本链条变脏。

#### 3.2.4 负库存

| 问题 | 结论 |
|------|------|
| 本批是否允许 | **默认禁止**（`biz_warehouse.allow_negative = 0`）；且本批**只有入库业务**，不会产生负库存 |
| 校验位置 | 应用层：出库/作废场景先校验 `allow_negative=0` 且 `qty - out_qty < 0` → 拒绝，错误信息给出可用数量 |
| 为什么不在 DB 加 `CHECK (qty >= 0)` | 会与"允许负库存的仓库"直接冲突（§2.3.3） |
| 若将来允许负库存（批次 2 销售） | 出库单价取**该物料最后一次入库单价**（`biz_inventory_txn` 最近一条 `txn_type='IN'` 的 `unit_cost`），并在流水 `remark` 标记 `负库存出库`，批次 4 报表单独披露 |
| 为什么负库存是危险的 | 负数量下 `amount/avg_cost` 的除法失去意义（`Decimal` 除负数得负均价），后续入库的移动加权会被污染 |
| 作废入库导致负库存（批次 2 场景） | 作废前必须校验"该批货尚未被出库消耗"：`SUM(之后该物料该仓出库量) > 0` → 拒绝作废，强制走红字冲销 |

---

### 3.3 期间与结账（本批最小可用集）

#### 3.3.1 期间表已定（§2.1.6），此处定义**行为边界**

| 功能 | 本批 | 批次 4 | 说明 |
|------|------|--------|------|
| 期间初始化 | ✅ 种子生成年度 12 个期间（`V0037`） | — | 也可后续做"年度初始化"接口 |
| 开启期间（`NOT_OPENED → OPEN`） | ✅ 提供接口 + 权限点 | — | 财务每月初手工开启（可支持一次开启多期） |
| 写凭证校验（必须 `OPEN`） | ✅ | — | 见 §3.3.2 |
| 结账（`OPEN → CLOSED`） | ❌ | ✅ | 本批只**尊重** `CLOSED` 状态（若批次 4 之前有人手工置 CLOSED，本批凭证也不许写） |
| 反结账 | ❌ | ❌（明确不做） | 反结账破坏审计链，用红字跨期更正代替 |
| 结转损益 / 期末调汇 | ❌ | ✅ | — |
| 余额重算校验 | ❌ | ✅ | 结账时 `SUM(分录) == 余额表` 全量核对 |

#### 3.3.2 期间判定与校验（实现口径）

```
① period_code = voucher_date 格式化 "YYYY-MM"
② 查 biz_accounting_period WHERE period_code = ?
   不存在 → 400「会计期间 {code} 未初始化，请先在『会计期间』中初始化」
③ status = 'NOT_OPENED' → 400「期间 {code} 未开启，禁止记账」
   status = 'CLOSED'     → 400「期间 {code} 已结账，禁止记账」
   status = 'OPEN'       → 通过
④ 附加校验（防脏数据）：voucher_date BETWEEN start_date AND end_date
```

**为什么用表而不是硬编码月份边界**：`start_date/end_date` 允许特例（如首期从年中启用、13 期调整期间）。硬编码 `to_char` 只能处理标准自然月，一旦业务要"2026-10 从 10/05 算起"就得改代码。表驱动多一次查询，换来零改动的灵活性。

#### 3.3.3 期间与单据日期的关系

- 入库单 `in_date` 决定凭证 `voucher_date` → 决定 `period_code`；
- 允许**补录历史**：只要目标期间 `status='OPEN'`。若已结账 → 只能开在当前期间，并需人工红字调整；
- 过账时若单据 `in_date` 属于已结账期间 → **拒绝过账**（不能事后改已结账期间的账），提示"请修改入库日期到开放期间，或联系财务处理"。

---

### 3.4 单据流转与事务边界

#### 3.4.1 状态机（`shared/doc_status.rs` 集中定义）

**采购订单 `biz_purchase_order.status`**

```
                 ┌──────────────────────────────┐
                 ▼                              │
DRAFT ──提交──> SUBMITTED ──审批──> APPROVED ──┤
  │                 │                  │        │
  │                 │                  ├──部分收货──> PARTIAL_RECEIVED ──全部收货──> RECEIVED
  │                 │                  │                        │                    │
  └────作废─────────┴──────────────────┴───作废─────────────────┘                    │
     → CANCELLED                                                                     │
                                                              手动关闭 ──> CLOSED ◄───┘
```

| 迁移 | 触发 | 校验 |
|------|------|------|
| `DRAFT → SUBMITTED` | 提交 | 明细 ≥ 1 行；每行 `qty > 0`；供应商/仓库启用 |
| `SUBMITTED → APPROVED` | 审批（本批权限点直接置位，无流程） | 具备 `business:erp:purchase-order:approve` |
| `APPROVED → PARTIAL_RECEIVED / RECEIVED` | 入库单过账时**自动**联动 | 见 §3.4.2 第 9 步 |
| `* → CLOSED` | 手工关闭（未收完也要能关） | 不在 `CANCELLED` 状态；有关闭权限 |
| `DRAFT / SUBMITTED / APPROVED / PARTIAL_RECEIVED → CANCELLED` | 作废 | **不能作废已有过账入库单的订单**（先作废入库单） |
| `RECEIVED / CLOSED / CANCELLED` | 终态 | 不可再迁移 |

**入库单 `biz_stock_in.status`**

```
DRAFT ──过账──> POSTED ──作废──> CANCELLED
  │                                 ▲
  └───────────────作废──────────────┘
```

| 迁移 | 触发 | 关键前置校验 |
|------|------|------------|
| `DRAFT → POSTED` | 过账 | 明细 ≥ 1；日期期间 `OPEN`；物料/仓库/供应商启用；源订单行 `received_qty + 本次 ≤ qty`（超收控制）；规则可解析 |
| `DRAFT → CANCELLED` | 作废草稿 | 无（直接置位） |
| `POSTED → CANCELLED` | 作废已过账 | **见 §3.4.3 作废准入门槛**（7 项，缺一不可） |
| `CANCELLED` | 终态 | 不可恢复；如需重来新建单据 |

**状态机实现方式**：`shared/doc_status.rs` 提供

```rust
/// 单据状态机的合法迁移表（模块级常量，禁止散落 if-else）。
pub(crate) fn can_transition(doc: DocKind, from: &str, to: &str) -> bool;

/// 迁移失败时构造统一的 400 错误信息（含当前状态与目标状态）。
pub(crate) fn ensure_transition(doc: DocKind, from: &str, to: &str) -> ResResult<()>;
```

所有 handler 的写操作**必须先 `ensure_transition`**，再落库；`biz_doc_log` 的 `from_status/to_status` 直接取这两个值，保证轨迹与状态机一致。

#### 3.4.2 一次"收货入库过账"的事务边界（本批最复杂的一段）

**单一事务，12 步，顺序不可调换**（顺序本身就是防并发/防脏数据的设计）：

| 步 | 动作 | SQL / 说明 | 失败后果 |
|----|------|-----------|---------|
| 0 | 事务外前置校验 | 单据存在、明细非空、`status='DRAFT'`（快速失败，不占事务） | 400 |
| 1 | `BEGIN`（`db().transaction`） | RAII，Err 自动 ROLLBACK | — |
| 2 | **锁单据行**（幂等门闩） | `SELECT status FROM biz_stock_in WHERE id=? AND deleted_at IS NULL FOR UPDATE`；`status<>'DRAFT'` → Err | 回滚，无副作用 |
| 3 | 校验期间 | `shared/period.rs::ensure_open(in_date)` | 回滚 |
| 4 | 载入明细 + 主数据 + 解析规则 | 校验物料/仓库/供应商 `status=1`；规则命中（无规则 → Err） | 回滚 |
| 5 | 超收校验 | 对每行 `source_item_id`：`received_qty + qty <= 订单行 qty` | 回滚 |
| 6 | **按 material_id 升序**逐个锁库存行 + 更新库存 + 写流水 | §3.2.2 三段式；每行写一条 `biz_inventory_txn`（含 6 个前后快照） | 回滚（库存全部还原） |
| 7 | 生成应付单 | `biz_payable` upsert；`uk_biz_payable_source` 兜底幂等；取号 `PAY` | 回滚 |
| 8 | 解析规则 → 生成凭证 + 分录 | `voucher/engine.rs::generate(...)`；L1 借贷平衡校验 | 回滚 |
| 9 | 更新科目余额 | §3.1.6 upsert；末尾 L3 全科目借贷平衡校验 | 回滚 |
| 10 | 反写单据与上游 | `biz_stock_in` 置 `POSTED` + `voucher_id` + `payable_id`；`biz_purchase_order_item.received_qty += qty`；重算订单状态（全部行收满 → `RECEIVED`，否则 `PARTIAL_RECEIVED`） | 回滚 |
| 11 | 写 `biz_doc_log` | action=POST，from=DRAFT，to=POSTED，detail 含凭证号/应付号/金额 | 回滚 |
| 12 | `COMMIT` | — | — |

**为什么第 2 步必须锁单据行**：库存行的锁只保护"同一物料同一仓"，无法阻止"同一张单据被并发过账两次"（第二次读到旧 `DRAFT` 也会继续）。单据行锁是**单据级幂等**的唯一保障。

**为什么第 11 步写日志在最后**：`biz_doc_log` 是与业务同生共死的轨迹，必须最后写——它成功即意味着前面全部成功，语义上"这条轨迹代表一次完整成功的过账"。若放最前，中途失败回滚虽也会删掉它，但顺序上"日志先于事实"不符合审计直觉。

**失败回滚策略**

| 失败类型 | 表现 | 处理 |
|---------|------|------|
| 业务校验失败（期间/超收/规则缺失） | `ApiError::bad_request`（400） | 事务回滚，单据保持 `DRAFT`，前端提示具体原因 |
| DB 约束冲突（唯一键/CHECK） | `DbError` → `utils::db_error(e, "过账入库单")` → 500 | 事务回滚。**唯一键冲突属程序 bug**（幂等已由第 2/7 步覆盖），需修代码，不做静默吞掉 |
| 死锁（`40P01`） | PG 自动杀一方 | 事务回滚，返回 500 + 日志告警。**不做自动重试**（重试会掩盖锁顺序 bug）；若上线后死锁率 > 0.1%，说明锁顺序实现有误，按 §3.2.2 复查 |
| 连接中断 | 事务未提交 | PG 自动回滚（未提交事务不会留下脏数据） |
| 日志总线（`operation_logs`）写失败 | 不影响业务（异步旁路） | 只告警。可能留下"过账失败"的接口日志，这是正确的安全审计信息 |

**事务粒度原则**：**一次业务动作 = 一个事务**。不做"分批提交"（会把一半库存写进去却无凭证），也不把取号拆到事务外（会产生跳号）。

#### 3.4.3 单据作废 / 红冲的连锁影响

**作废已过账入库单（`POSTED → CANCELLED`）的 7 项准入门槛**

| # | 检查 | 不满足时 |
|---|------|---------|
| 1 | 原凭证存在且 `status='POSTED'` | 400「已冲销/无凭证，无法作废」 |
| 2 | 原凭证所属期间 `status='OPEN'` | 400「期间已结账，请用红字凭证跨期冲销」 |
| 3 | 该批库存尚未被后续出库消耗（本批恒成立，批次 2 起生效） | 400「库存已被消耗，不可作废」 |
| 4 | 反冲后各物料该仓结存 `>= 0`（或该仓允许负库存） | 400「作废将导致物料 X 在仓库 Y 负库存（当前 Z）」 |
| 5 | 应付单 `paid_amount = 0`（未付款） | 400「应付单已付款，请先冲销付款」 |
| 6 | 供应商/物料未被停用（停用不影响作废，仅提示） | 200 + 提示 |
| 7 | 操作人具备 `business:erp:stock-in:cancel` | 403 |

**作废事务（9 步，单事务）**

| 步 | 动作 |
|----|------|
| 1 | 锁单据行，校验 `status='POSTED'` |
| 2 | 7 项准入校验 |
| 3 | 写**反向库存流水**：每行 `txn_type='OUT'`、`biz_type='ADJUST_OUT'`、`doc_type='STOCK_IN_CANCEL'`、金额与数量取原入库流水的**原值**（不倒算） |
| 4 | 库存行回滚：`qty -= 原qty`、`amount -= 原amount`；`avg_cost` 重算（`qty>0 ? round(amount/qty,6) : 保留`） |
| 5 | 生成红字凭证（§3.1.8）：借贷互换，`is_red=1`，`reversed_voucher_id=原凭证`；原凭证 `status='REVERSED'` |
| 6 | `biz_payable` 置 `CANCELLED`、`unpaid_amount=0`（保留 `amount` 原值供追溯） |
| 7 | `biz_purchase_order_item.received_qty -= 原qty`；订单状态回退（`RECEIVED` → `PARTIAL_RECEIVED` 或 `APPROVED`） |
| 8 | `biz_stock_in` 置 `CANCELLED` + `cancelled_at/cancelled_by/cancel_reason`；`voucher_id` 改写为红字凭证 ID（保留可追溯的最终凭证） |
| 9 | 写 `biz_doc_log`（action=CANCEL，from=POSTED，to=CANCELLED，detail 含红字凭证号、原因） |

**为什么库存回滚要"写反向流水"而不是"删掉原流水"**：流水是追加写的审计证据。删掉原流水等于抹掉"曾经入过货"的事实，审计无法复原。反向流水让"入库 + 冲销"两笔都在，净额为零。

**红冲 vs 作废的选择建议（写进 UI 文案）**

| 场景 | 建议动作 | 原因 |
|------|---------|------|
| 单据填错、尚未到月末、期间开放 | **作废 + 重开** | 干净，单据与账都消失（留红字痕迹） |
| 已跨月但期间未结账 | 作废（红字自动开在当期） | 同上，但注意会留下跨期红字 |
| 期间已结账 | **只能红字冲销凭证**，不动单据状态 | 已结账期间不可写 |
| 实物已入库、只是价格错 | 红字冲销 + 新建正确单据 | 保留实物入库证据 |

---
## 4. API 契约

### 4.1 通用约定

| 项 | 约定 | 依据 |
|----|------|------|
| 路径前缀 | 后端宏路径**不含** `/api`（如 `/business/erp/material/page`），前端**必须写全** `/api/business/erp/material/page` | `AGENTS.md` 约定 8 / 踩坑 1 |
| 响应包装 | `Res::ok(data)` / `Res::page(list, total, page, size)`；错误统一 `ApiError` | `nik::prelude::Res` |
| 分页字段 | `list` / `total` / `page` / `page_size` | `PageData`（§1.3） |
| 分页入参 | `page`（默认 1）、`page_size`（默认 20，限幅），统一过 `utils::normalize_page` | `docs/后端二开指南.md` §6 |
| ID | UUID 字符串；路径参数用 `utils::require_uuid(&s, "id")` 解析，失败 400 | 同上 |
| 金额/数量传输 | **字符串**（`"1500.0000"`），不是 number。`Decimal` 的 serde 实现序列化为字符串（§9.1 POC-5 实测） | 避免 JS number 精度丢失（前端无 bignumber 库） |
| 日期 | `DATE` → `"2026-10-27"`；`TIMESTAMPTZ` → RFC 3339 | — |
| 枚举/状态 | 字符串短码（`"POSTED"` / `"RAW"`），不用数字 | §1.6 |
| 明细提交 | 主表 + 明细**一次提交**（`items: [...]` 数组），后端在事务内先删后插重建明细 | 避免"主子表分别提交"产生的半成品数据 |
| 幂等 | 过账/作废/冲销类接口必须在事务内先锁单据行（§3.4.2 第 2 步） | — |

**统一错误码取值**（业务错误用 400 + 中文 message；不新增 HTTP 状态码语义）

| 场景 | 状态码 | message 示例 |
|------|-------|-------------|
| 参数/状态非法 | 400 | `单据当前状态 POSTED，不可重复过账` |
| 期间不可用 | 400 | `期间 2026-10 已结账，禁止记账` |
| 规则缺失 | 400 | `未配置 STOCK_IN 的凭证规则，拒绝过账` |
| 科目缺失 | 400 | `物料「冷轧钢板」未配置存货科目，且无默认值` |
| 无权限 | 403 | （由 `#[nik::permission]` 统一返回） |
| 对象不存在 | 404 | `入库单不存在` |
| 数据库/内部异常 | 500 | `过账入库单失败`（`utils::db_error` 补充上下文） |

### 4.2 主数据 CRUD（5 个模块，结构同构）

以**物料**为例，其余（`supplier` / `warehouse` / `account` / `unit`）路径与权限码同构替换资源名即可。

| 方法 | 后端路径（宏） | 前端路径 | 权限码 | 说明 |
|------|--------------|---------|--------|------|
| GET | `/business/erp/material/page` | `/api/business/erp/material/page` | `business:erp:material:list` | 分页查询 |
| GET | `/business/erp/material/{id}` | 同 | `business:erp:material:detail` | 详情 |
| POST | `/business/erp/material` | 同 | `business:erp:material:create` | 新增 |
| PUT | `/business/erp/material/{id}` | 同 | `business:erp:material:update` | 修改 |
| DELETE | `/business/erp/material/{id}` | 同 | `business:erp:material:delete` | 软删 |
| GET | `/business/erp/material/options` | 同 | `business:erp:material:list` | 下拉选项（`id/code/name/spec/unit_name/tax_rate`），供单据选用 |
| GET | `/business/erp/material/{id}/inventory` | 同 | `business:erp:inventory:list` | 该物料各仓结存（详情页"库存"标签） |

**物料分页查询参数**

```
page, page_size
keyword?        物料编码/名称模糊（ILIKE %kw%）
material_type?  RAW/SEMI/FINISHED/SERVICE
warehouse_id?   默认仓库
status?         0/1
```

**物料响应（`MaterialRes`）**

```json
{
  "id": "0192...", "material_code": "M-1001", "material_name": "冷轧钢板",
  "spec": "1.5mm*1250*2500", "unit_id": "...", "unit_name": "千克",
  "material_type": "RAW", "default_warehouse_id": "...", "default_warehouse_name": "原料仓",
  "safety_stock": "100.0000", "costing_method": "MOVING_AVG",
  "inventory_account_id": "...", "inventory_account_name": "1405 库存商品",
  "tax_rate": "0.1300", "status": 1, "created_at": "2026-10-27T02:11:33Z"
}
```

**关键校验（写进 handler）**

| 模块 | 校验 |
|------|------|
| 物料 | 编码唯一（软删后可复用）；`unit_id` 必须存在且启用；`default_warehouse_id` 可为空但非空时必须启用；`inventory_account_id` 必须为**末级**且类别为 `ASSET`；`tax_rate` ∈ [0,1] |
| 供应商 | 编码唯一；`payable_account_id` 必须末级且类别为 `LIABILITY` |
| 仓库 | 编码唯一；停用前校验无未完成单据 |
| 科目 | 编码全表唯一（§2.1.3）；`parent_id` 必须存在且 `is_leaf=1`（否则父科目已降级）；新增子科目后父科目 `is_leaf=0`；**父科目已有分录时拒绝挂子科目**；`balance_direction` 与 `account_category` 的一致性校验（详见 §4.7.4） |
| 单位 | 编码唯一；辅助单位必须有 `base_unit_id` 且不构成循环引用（沿 `base_unit_id` 上溯最多 3 层） |

**删除保护（重要）**

| 表 | 删除条件 |
|----|---------|
| 物料 | 存在库存（`biz_inventory.qty <> 0`）或未完成单据 → 拒绝，提示"请先清零库存/关闭单据" |
| 供应商 | 存在未结清应付（`status IN ('OPEN','PARTIAL_PAID')`）→ 拒绝 |
| 仓库 | 存在非零库存 → 拒绝 |
| 科目 | **已发生分录**（`biz_voucher_entry` 存在该 `account_id`）→ 拒绝，只能停用（`status=0`） |
| 单位 | 被物料引用 → 拒绝 |

### 4.3 采购订单

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/purchase-order/page` | `business:erp:purchase-order:list` | 分页（单号/供应商/日期区间/状态） |
| GET | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:detail` | 详情（含明细 + 操作轨迹） |
| POST | `/business/erp/purchase-order` | `business:erp:purchase-order:create` | 新建（主表+明细，状态 `DRAFT`，取号 `PO`） |
| PUT | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:update` | 修改（**仅 `DRAFT`**） |
| DELETE | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:delete` | 删除（**仅 `DRAFT`**，软删） |
| POST | `/business/erp/purchase-order/{id}/submit` | `business:erp:purchase-order:submit` | 提交（`DRAFT → SUBMITTED`） |
| POST | `/business/erp/purchase-order/{id}/approve` | `business:erp:purchase-order:approve` | 审批（`SUBMITTED → APPROVED`；本批无流程，直接置位） |
| POST | `/business/erp/purchase-order/{id}/close` | `business:erp:purchase-order:close` | 关闭（未收完也可关） |
| POST | `/business/erp/purchase-order/{id}/cancel` | `business:erp:purchase-order:cancel` | 作废 |
| GET | `/business/erp/purchase-order/{id}/items` | `business:erp:purchase-order:detail` | 明细列表（收货界面用，含 `received_qty` / `remain_qty`） |
| GET | `/business/erp/purchase-order/{id}/receivable-items` | `business:erp:purchase-order:detail` | **可收货明细**（`remain_qty > 0` 且在 `APPROVED`/`PARTIAL_RECEIVED` 状态），入库单创建页直接带入 |

**创建请求示例**

```json
{
  "supplier_id": "0192...", "order_date": "2026-10-27", "expect_date": "2026-11-05",
  "warehouse_id": "0192...", "currency_code": "CNY", "remark": "首批备料",
  "items": [
    { "material_id": "0192...", "qty": "150.0000", "price": "11.111100", "tax_rate": "0.1300", "remark": "" },
    { "material_id": "0192...", "qty": "30.0000",  "price": "9.876500",  "tax_rate": "0.1300", "remark": "" }
  ]
}
```

**金额计算规则（后端权威，前端只做即时预览）**

```
行 amount          = round(qty × price, 4)
行 tax_amount      = round(amount × tax_rate, 4)
行 with_tax_amount = amount + tax_amount          ← 恒由加法得出，不用乘除
单 total_amount    = Σ 行 amount
单 total_tax       = Σ 行 tax_amount
单 total_with_tax  = total_amount + total_tax     ← 恒等式（DB 有 CHECK 兜底）
```

> **前端预览与后端落库可能差 1 分**（浮点/舍入时机不同）：前端必须**提交后以响应为准**重绘，不能把预览值当作已保存值。这是 §5.3 统一金额工具存在的原因。

### 4.4 收货入库

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/stock-in/page` | `business:erp:stock-in:list` | 分页（单号/供应商/仓库/日期/状态/来源单号） |
| GET | `/business/erp/stock-in/{id}` | `business:erp:stock-in:detail` | 详情（含明细、凭证号、应付号、操作轨迹） |
| POST | `/business/erp/stock-in` | `business:erp:stock-in:create` | 新建（`DRAFT`，取号 `IN`；可带 `source_order_id`） |
| PUT | `/business/erp/stock-in/{id}` | `business:erp:stock-in:update` | 修改（**仅 `DRAFT`**） |
| DELETE | `/business/erp/stock-in/{id}` | `business:erp:stock-in:delete` | 删除（**仅 `DRAFT`**） |
| POST | `/business/erp/stock-in/{id}/post` | `business:erp:stock-in:post` | **过账**（核心接口，§3.4.2） |
| POST | `/business/erp/stock-in/{id}/cancel` | `business:erp:stock-in:cancel` | 作废（`DRAFT` 直接置位；`POSTED` 走 §3.4.3 全链路） |
| GET | `/business/erp/stock-in/{id}/logs` | `business:erp:stock-in:detail` | 操作轨迹（`biz_doc_log`） |
| GET | `/business/erp/stock-in/{id}/print` | `business:erp:stock-in:detail` | 打印数据（返回结构化 JSON，前端渲染，不做服务端 PDF） |

**过账响应（必须回传下游产物 ID，前端跳转用）**

```json
{
  "id": "0192...", "in_no": "IN-20261027-000012", "status": "POSTED",
  "voucher_id": "0192...", "voucher_no": "记-202610-0007",
  "payable_id": "0192...", "payable_no": "PAY-20261027-000009",
  "total_amount": "1500.0000", "total_tax": "195.0000", "total_with_tax": "1695.0000"
}
```

**过账返回 400 的典型 message**（前端需原样展示，不做二次翻译）：
- `单据当前状态 POSTED，不可重复过账`
- `期间 2026-10 已结账，禁止记账`
- `采购订单行 2 可收数量不足：剩余 10.0000，本次 20.0000`（超收控制）
- `未配置 STOCK_IN 的凭证规则，拒绝过账`
- `物料「冷轧钢板」未配置存货科目，且无默认值`
- `科目 1405 已停用，无法记账`

### 4.5 库存查询

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/inventory/page` | `business:erp:inventory:list` | 即时库存分页（物料/仓库/数量/均价/金额） |
| GET | `/business/erp/inventory/txn/page` | `business:erp:inventory:txn` | 库存流水分页（物料/仓库/日期区间/业务类型/单号） |
| GET | `/business/erp/inventory/summary` | `business:erp:inventory:list` | 汇总（按仓库或按物料聚合，页面顶部统计卡） |
| GET | `/business/erp/inventory/consistency-check` | `business:erp:inventory:list` | 一致性巡检（执行 §3.2.1 校验 SQL，返回异常行。**只读、不自动修**） |

**即时库存响应**

```json
{
  "id": "0192...", "material_id": "...", "material_code": "M-1001", "material_name": "冷轧钢板",
  "spec": "1.5mm*1250*2500", "unit_name": "千克",
  "warehouse_id": "...", "warehouse_name": "原料仓",
  "qty": "147.0000", "locked_qty": "0.0000", "avg_cost": "10.356611", "amount": "1522.4218",
  "safety_stock": "100.0000", "below_safety": false, "last_txn_at": "2026-10-27T02:20:00Z"
}
```

**流水分页响应**（每条含前后快照，允许前端"成本追溯"抽屉展示）

```json
{
  "id": "...", "txn_type": "IN", "biz_type": "PURCHASE_IN",
  "doc_type": "STOCK_IN", "doc_no": "IN-20261027-000012", "line_no": 1,
  "qty": "50.0000", "unit_cost": "11.111100", "amount": "555.5550",
  "before_qty": "100.0000", "before_amount": "1012.3400", "before_cost": "10.123400",
  "after_qty": "150.0000", "after_amount": "1567.8950", "after_cost": "10.452633",
  "period_code": "2026-10", "created_at": "2026-10-27T02:20:00Z"
}
```

### 4.6 应付

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/payable/page` | `business:erp:payable:list` | 分页（供应商/状态/到期日区间/单号） |
| GET | `/business/erp/payable/{id}` | `business:erp:payable:detail` | 详情（含来源单据、凭证） |
| POST | `/business/erp/payable/{id}/settle` | `business:erp:payable:settle` | **手工核销**（本批仅登记已付金额，付款单在批次 2） |
| GET | `/business/erp/payable/aging` | `business:erp:payable:list` | 账龄分析（按到期日分档：未到期/1-30/31-60/61-90/90+） |

**手工核销请求**

```json
{ "paid_amount": "500.0000", "remark": "10 月银行转账（批次 2 将改为按付款单核销）" }
```
- 校验：`0 < paid_amount <= unpaid_amount`；
- 效果：`paid_amount += 请求值`、`unpaid_amount = amount - paid_amount`、`status` 按 0/部分/全额切换为 `OPEN`/`PARTIAL_PAID`/`SETTLED`；
- **不生成凭证**（本批付款凭证留批次 2，避免半成品会计处理）；
- 写 `biz_doc_log`（action=SETTLE）。

### 4.7 凭证与总账

#### 4.7.1 凭证

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/voucher/page` | `business:erp:voucher:list` | 分页（期间/日期/凭证字/来源类型/状态） |
| GET | `/business/erp/voucher/{id}` | `business:erp:voucher:detail` | 详情（含分录） |
| POST | `/business/erp/voucher` | `business:erp:voucher:create` | **手工新增**（`event_type=MANUAL`，不走规则） |
| POST | `/business/erp/voucher/{id}/reverse` | `business:erp:voucher:reverse` | **冲销**（生成红字凭证） |
| GET | `/business/erp/voucher/{id}/entries` | `business:erp:voucher:detail` | 分录列表 |

**手工凭证请求**

```json
{
  "voucher_date": "2026-10-31", "voucher_word": "记", "summary": "月末计提折旧",
  "entries": [
    { "summary": "计提折旧", "account_id": "...", "debit_amount": "1200.0000", "credit_amount": "0" },
    { "summary": "计提折旧", "account_id": "...", "debit_amount": "0", "credit_amount": "1200.0000" }
  ]
}
```
- 校验：期间开放、科目末级且启用、每行单边非零、借贷合计相等、行数 ≥ 2、行金额 > 0；
- 取号 `VCH`（凭证号按 `voucher_word + period_code` 内独立流水）；
- 唯一允许"无来源单据"的凭证类型。

**冲销请求**

```json
{ "reverse_date": "2026-10-31", "reason": "供应商开票金额有误" }
```
- 校验：原凭证 `POSTED`、未被冲销、`reverse_date` 期间 `OPEN`；
- 返回新凭证 `voucher_no`（红字）与原凭证新状态。

#### 4.7.2 总账查询

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/ledger/page` | `business:erp:ledger:list` | **科目余额表**（总账）：按期间查全部科目期初/本期借/本期贷/期末 |
| GET | `/business/erp/ledger/detail` | `business:erp:ledger:list` | **明细账**：按科目 + 期间区间列分录（分页） |
| GET | `/business/erp/ledger/trial-balance` | `business:erp:ledger:list` | **试算平衡表**：全部科目借方合计 == 贷方合计校验结果 |
| GET | `/business/erp/ledger/tree` | `business:erp:ledger:list` | 科目树 + 余额（父子层级汇总，一级科目到末级） |

**科目余额表响应**

```json
{
  "period_code": "2026-10",
  "list": [
    { "account_id": "...", "account_code": "1405", "account_name": "库存商品", "level": 1,
      "begin_balance": "0.0000", "begin_direction": "DEBIT",
      "period_debit": "1500.0000", "period_credit": "341.7682",
      "end_balance": "1158.2318", "end_direction": "DEBIT" },
    { "account_id": "...", "account_code": "2202.02", "account_name": "应付账款—暂估应付款", "level": 3,
      "begin_balance": "0.0000", "begin_direction": "CREDIT",
      "period_debit": "0.0000", "period_credit": "1695.0000",
      "end_balance": "1695.0000", "end_direction": "CREDIT" }
  ],
  "total": 0
}
```

**试算平衡表响应（会计对账的核心工具）**

```json
{
  "period_code": "2026-10",
  "total_debit": "1695.0000", "total_credit": "1695.0000",
  "balanced": true, "unbalanced_accounts": []
}
```
- 实现：按 §2.5.3 的带符号口径汇总全部科目，`Σ(本期借) == Σ(本期贷)` 必须成立；
- 不成立时返回 `balanced: false` + 明细科目差项，**不做自动修复**。

#### 4.7.3 会计期间

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/period/page` | `business:erp:period:list` | 期间列表（年度筛选） |
| POST | `/business/erp/period/open` | `business:erp:period:open` | 开启期间（`NOT_OPENED → OPEN`，可批量） |
| POST | `/business/erp/period/init` | `business:erp:period:open` | 年度初始化（生成 12 个期间，幂等） |

#### 4.7.4 科目类别与余额方向的一致性规则（种子与校验共用）

| `account_category` | 允许的 `balance_direction` | 说明 |
|-------------------|--------------------------|------|
| `ASSET`（资产） | `DEBIT`（备抵科目可 `CREDIT`，如累计折旧） | 校验时仅**告警不阻断** |
| `LIABILITY`（负债） | 默认 `CREDIT`；**借方专栏允许 `DEBIT`** | 默认强制；「应交税费—应交增值税（进项税额）」这类**借方专栏**必须设 `DEBIT`，否则期末方向判定为"反向"（不影响试算平衡，但总账展示会误导）。该情形校验时**告警不阻断** |
| `EQUITY`（权益） | `CREDIT` | 强制 |
| `COST`（成本） | `DEBIT` | 强制 |
| `PROFIT_LOSS`（损益） | 收入类 `CREDIT`、费用类 `DEBIT` | 两者都允许，由财务选择 |

> 强制项在 `account/handler.rs` 校验，违反返回 400；`ASSET` 的备抵情形只在前端提示，因为"累计折旧""坏账准备"是标准业务。

>
> **设计过程中由真实数据库验证发现的问题（见 §9.1.8）**：初版种子里把"进项税额"设为 `LIABILITY + CREDIT`，在真库跑出来期末方向是 `DEBIT`，被判为"★反向"。这不是 bug，而是**科目性质定义错了**——进项税额是负债类科目下的**借方专栏**（登记可抵扣额），标准做法是 `account_category='LIABILITY'` + `balance_direction='DEBIT'`。已修正种子。由此得出的校验策略：`account_category` 与 `balance_direction` 的一致性**不能一律强校验**，只能对明显不合常理的组合告警。

### 4.8 凭证规则管理

| 方法 | 后端路径 | 权限码 | 说明 |
|------|---------|--------|------|
| GET | `/business/erp/voucher-rule/page` | `business:erp:voucher-rule:list` | 规则列表 |
| GET | `/business/erp/voucher-rule/{id}` | `business:erp:voucher-rule:detail` | 详情（含分录模板） |
| POST | `/business/erp/voucher-rule` | `business:erp:voucher-rule:create` | 新增（头+分录模板） |
| PUT | `/business/erp/voucher-rule/{id}` | `business:erp:voucher-rule:update` | 修改 |
| DELETE | `/business/erp/voucher-rule/{id}` | `business:erp:voucher-rule:delete` | 删除（**要求非 `is_default=1`**，防误删兜底） |
| POST | `/business/erp/voucher-rule/{id}/preview` | `business:erp:voucher-rule:list` | **试算预览**：给定样例单据参数，返回将生成的分录（不落库）。**这是规则上线前的必测手段** |

> `preview` 接口是本期降低"会计正确性风险"的关键工具：运营改完规则可立即看到"借什么贷什么、金额多少、是否平衡"，不必等真实单据过账才发现配错。

### 4.9 权限码清单（三方一致，缺一即 403 或菜单不显示）

**共 58 个权限点 + 1 个菜单（目录）**。`sys_menu.auth_code` = `#[nik::permission(...)]` = 前端 `meta.permission` / `v-perm` 判断，三处逐字一致。

| 模块 | 权限码 | 说明 |
|------|--------|------|
| 物料 | `business:erp:material:list` | 分页/下拉 |
| | `business:erp:material:detail` | 详情 |
| | `business:erp:material:create` | 新增 |
| | `business:erp:material:update` | 修改 |
| | `business:erp:material:delete` | 删除 |
| 供应商 | `business:erp:supplier:list` / `:detail` / `:create` / `:update` / `:delete` | 同上 5 个 |
| 仓库 | `business:erp:warehouse:list` / `:detail` / `:create` / `:update` / `:delete` | 同上 5 个 |
| 科目 | `business:erp:account:list` / `:detail` / `:create` / `:update` / `:delete` | 同上 5 个 |
| 单位 | `business:erp:unit:list` / `:detail` / `:create` / `:update` / `:delete` | 同上 5 个 |
| 采购订单 | `business:erp:purchase-order:list` | 分页/明细查询 |
| | `business:erp:purchase-order:detail` | 详情/可收明细/轨迹 |
| | `business:erp:purchase-order:create` | 新增 |
| | `business:erp:purchase-order:update` | 修改（草稿） |
| | `business:erp:purchase-order:delete` | 删除（草稿） |
| | `business:erp:purchase-order:submit` | 提交 |
| | `business:erp:purchase-order:approve` | 审批 |
| | `business:erp:purchase-order:close` | 关闭 |
| | `business:erp:purchase-order:cancel` | 作废 |
| 入库 | `business:erp:stock-in:list` | 分页 |
| | `business:erp:stock-in:detail` | 详情/轨迹/打印 |
| | `business:erp:stock-in:create` | 新增 |
| | `business:erp:stock-in:update` | 修改（草稿） |
| | `business:erp:stock-in:delete` | 删除（草稿） |
| | `business:erp:stock-in:post` | **过账**（高敏，建议只授财务/仓管主管） |
| | `business:erp:stock-in:cancel` | **作废**（高敏） |
| 库存 | `business:erp:inventory:list` | 即时库存/汇总/巡检 |
| | `business:erp:inventory:txn` | 库存流水 |
| 应付 | `business:erp:payable:list` | 分页/账龄 |
| | `business:erp:payable:detail` | 详情 |
| | `business:erp:payable:settle` | 手工核销 |
| 凭证 | `business:erp:voucher:list` | 分页 |
| | `business:erp:voucher:detail` | 详情/分录 |
| | `business:erp:voucher:create` | 手工新增 |
| | `business:erp:voucher:reverse` | **冲销**（高敏） |
| 总账 | `business:erp:ledger:list` | 科目余额/明细账/试算平衡/科目树 |
| 期间 | `business:erp:period:list` | 期间列表 |
| | `business:erp:period:open` | 开启期间/年度初始化 |
| 规则 | `business:erp:voucher-rule:list` | 列表/试算预览 |
| | `business:erp:voucher-rule:detail` | 详情 |
| | `business:erp:voucher-rule:create` | 新增 |
| | `business:erp:voucher-rule:update` | 修改 |
| | `business:erp:voucher-rule:delete` | 删除 |

**完整权限码清单（58 个，可直接作为 `V0038` 种子的输入）**

实施时按此清单逐条生成 `sys_menu` 记录，并用同一个常量数组在后端注解与前端校验间对齐——**清单就是唯一真相**，任何一处新增都必须同时改三处。

```
business:erp:material:list          business:erp:material:detail
business:erp:material:create        business:erp:material:update
business:erp:material:delete

business:erp:supplier:list          business:erp:supplier:detail
business:erp:supplier:create        business:erp:supplier:update
business:erp:supplier:delete

business:erp:warehouse:list         business:erp:warehouse:detail
business:erp:warehouse:create       business:erp:warehouse:update
business:erp:warehouse:delete

business:erp:account:list           business:erp:account:detail
business:erp:account:create         business:erp:account:update
business:erp:account:delete

business:erp:unit:list              business:erp:unit:detail
business:erp:unit:create            business:erp:unit:update
business:erp:unit:delete

business:erp:purchase-order:list    business:erp:purchase-order:detail
business:erp:purchase-order:create  business:erp:purchase-order:update
business:erp:purchase-order:delete  business:erp:purchase-order:submit
business:erp:purchase-order:approve business:erp:purchase-order:close
business:erp:purchase-order:cancel

business:erp:stock-in:list          business:erp:stock-in:detail
business:erp:stock-in:create        business:erp:stock-in:update
business:erp:stock-in:delete        business:erp:stock-in:post
business:erp:stock-in:cancel

business:erp:inventory:list         business:erp:inventory:txn

business:erp:payable:list           business:erp:payable:detail
business:erp:payable:settle

business:erp:voucher:list           business:erp:voucher:detail
business:erp:voucher:create         business:erp:voucher:reverse

business:erp:ledger:list

business:erp:period:list            business:erp:period:open

business:erp:voucher-rule:list      business:erp:voucher-rule:detail
business:erp:voucher-rule:create    business:erp:voucher-rule:update
business:erp:voucher-rule:delete
```

计数核对：主数据 5 模块 × 5 = 25；采购订单 9；入库 7；库存 2；应付 3；凭证 4；总账 1；期间 2；规则 5 → **合计 58**。

**菜单结构（`V0038` 种子，`module_code='business'`）**

```
ERP 管理（目录，menu_type=1，path=/business/erp）
├── 基础资料（目录）
│   ├── 物料档案       /business/erp/material        business:erp:material:list
│   ├── 供应商档案     /business/erp/supplier        business:erp:supplier:list
│   ├── 仓库档案       /business/erp/warehouse       business:erp:warehouse:list
│   ├── 会计科目       /business/erp/account         business:erp:account:list
│   └── 计量单位       /business/erp/unit            business:erp:unit:list
├── 采购管理（目录）
│   ├── 采购订单       /business/erp/purchase-order  business:erp:purchase-order:list
│   └── 收货入库       /business/erp/stock-in        business:erp:stock-in:list
├── 库存管理（目录）
│   ├── 即时库存       /business/erp/inventory       business:erp:inventory:list
│   └── 库存流水       /business/erp/inventory-txn   business:erp:inventory:txn
├── 应付管理（目录）
│   └── 应付清单       /business/erp/payable         business:erp:payable:list
└── 财务核算（目录）
    ├── 凭证查询       /business/erp/voucher         business:erp:voucher:list
    ├── 总账查询       /business/erp/ledger          business:erp:ledger:list
    ├── 会计期间       /business/erp/period          business:erp:period:list
    └── 凭证规则       /business/erp/voucher-rule    business:erp:voucher-rule:list
```

按钮权限点（`menu_type=3`）挂在对应页面菜单下，固定 UUID 段为 `20000000-0000-0000-0000-00000000xxxx`（延续 `V0020` 的编号风格），`ON CONFLICT (id) DO NOTHING` 幂等。

### 4.10 种子数据：标准会计科目表（`V0037`）

采用《小企业会计准则》常用科目，级次编码 `4-2-2`（点分）。**只放本批闭环必需科目 + 常用科目**（不追求全量，避免种子膨胀），财务后续可按需增补。

```sql
-- 科目种子（片段，实际按固定 UUID 全量写入）
INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
VALUES
 ('21000000-0000-0000-0000-000000000001','1001','库存现金',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000002','1002','银行存款',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000003','1122','应收账款',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000004','1123','预付账款',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000005','1403','原材料',         NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000006','1405','库存商品',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000007','1601','固定资产',       NULL,1,'ASSET','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000008','1602','累计折旧',       NULL,1,'ASSET','CREDIT',1,1),
 ('21000000-0000-0000-0000-000000000009','2202','应付账款',       NULL,1,'LIABILITY','CREDIT',0,1),
 ('21000000-0000-0000-0000-00000000000a','2202.01','应付账款—货款',      '21000000-0000-0000-0000-000000000009',2,'LIABILITY','CREDIT',1,1),
 ('21000000-0000-0000-0000-00000000000b','2202.02','应付账款—暂估应付款','21000000-0000-0000-0000-000000000009',2,'LIABILITY','CREDIT',1,1),
 ('21000000-0000-0000-0000-00000000000c','2221','应交税费',       NULL,1,'LIABILITY','CREDIT',0,1),
 ('21000000-0000-0000-0000-00000000000d','2221.01','应交税费—应交增值税','21000000-0000-0000-0000-00000000000c',2,'LIABILITY','CREDIT',0,1),
 -- 注意：进项税额是「负债类科目的借方专栏」，余额方向必须 DEBIT（详见 §4.7.4 说明）
 ('21000000-0000-0000-0000-00000000000e','2221.01.01','应交税费—应交增值税—进项税额','21000000-0000-0000-0000-00000000000d',3,'LIABILITY','DEBIT',1,1),
 ('21000000-0000-0000-0000-00000000000f','2221.01.02','应交税费—应交增值税—销项税额','21000000-0000-0000-0000-00000000000d',3,'LIABILITY','CREDIT',1,1),
 ('21000000-0000-0000-0000-000000000010','4001','实收资本',       NULL,1,'EQUITY','CREDIT',1,1),
 ('21000000-0000-0000-0000-000000000011','4104','利润分配',       NULL,1,'EQUITY','CREDIT',1,1),
 ('21000000-0000-0000-0000-000000000012','5001','生产成本',       NULL,1,'COST','DEBIT',0,1),
 ('21000000-0000-0000-0000-000000000013','5001.01','生产成本—直接材料','21000000-0000-0000-0000-000000000012',2,'COST','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000014','5101','制造费用',       NULL,1,'COST','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000015','6001','主营业务收入',   NULL,1,'PROFIT_LOSS','CREDIT',1,1),
 ('21000000-0000-0000-0000-000000000016','6401','主营业务成本',   NULL,1,'PROFIT_LOSS','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000017','6601','财务费用',       NULL,1,'PROFIT_LOSS','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000018','6602','管理费用',       NULL,1,'PROFIT_LOSS','DEBIT',1,1),
 ('21000000-0000-0000-0000-000000000019','6603','销售费用',       NULL,1,'PROFIT_LOSS','DEBIT',1,1)
ON CONFLICT (id) DO NOTHING;
```

同一迁移内还要写：
1. **默认凭证规则** `STOCK_IN_DEFAULT`（3 条分录模板，见 §2.6.2 表格）；
2. **2026 年度 12 个会计期间**（`2026-01` … `2026-12`，状态 `NOT_OPENED`）；
3. **`biz_seq` 4 条规则**：`PO`（`yyyyMMdd`）、`IN`（`yyyyMMdd`）、`PAY`（`yyyyMMdd`）、`VCH`（`yyyyMM`，凭证号按月连续）；
4. **系统参数**（写入现有 `sys_config`，走 `V0037` 的同批 SQL）：`biz.erp.default_inventory_account=1405`、`biz.erp.default_payable_account=2202.02`、`biz.erp.over_receive_tolerance=0`。

> **注意**：`biz_account` 的 `is_leaf` 必须与子科目存在性严格一致——种子里 `2202`、`2221`、`2221.01`、`5001` 显式写 `is_leaf=0`，其余一级科目 `is_leaf=1`。若写错，凭证引擎的"科目必须末级"校验会直接拒绝过账（这是有意设计的严格性）。

---
## 5. 前端架构

### 5.1 页面清单与路由（静态路由，追加到 `src/router/index.ts`）

前端路由是**静态数组 + `meta.permission`**（§1.7），无 `addRoute` 动态注册。本批沿用该机制：在 `BasicLayout` 的 `children` 里追加一个 `business/erp` 路由组。

| # | 页面 | 路由 path | 组件 | `meta.permission` | 优先级 |
|---|------|-----------|------|------------------|--------|
| 1 | 物料档案 | `business/erp/material` | `views/business/erp/material/index.vue` | `business:erp:material:list` | P0 |
| 2 | 供应商档案 | `business/erp/supplier` | `views/business/erp/supplier/index.vue` | `business:erp:supplier:list` | P0 |
| 3 | 仓库档案 | `business/erp/warehouse` | `views/business/erp/warehouse/index.vue` | `business:erp:warehouse:list` | P0 |
| 4 | 会计科目 | `business/erp/account` | `views/business/erp/account/index.vue` | `business:erp:account:list` | P0（树形表格） |
| 5 | 计量单位 | `business/erp/unit` | `views/business/erp/unit/index.vue` | `business:erp:unit:list` | P1 |
| 6 | 采购订单 | `business/erp/purchase-order` | `views/business/erp/purchase-order/index.vue` | `business:erp:purchase-order:list` | P0 |
| 7 | 采购订单编辑 | `business/erp/purchase-order/edit` | `views/business/erp/purchase-order/edit.vue` | `business:erp:purchase-order:create` | P0 |
| 8 | 收货入库 | `business/erp/stock-in` | `views/business/erp/stock-in/index.vue` | `business:erp:stock-in:list` | P0 |
| 9 | 收货入库编辑/过账 | `business/erp/stock-in/edit` | `views/business/erp/stock-in/edit.vue` | `business:erp:stock-in:create` | P0 |
| 10 | 即时库存 | `business/erp/inventory` | `views/business/erp/inventory/index.vue` | `business:erp:inventory:list` | P0 |
| 11 | 库存流水 | `business/erp/inventory-txn` | `views/business/erp/inventory-txn/index.vue` | `business:erp:inventory:txn` | P1 |
| 12 | 应付清单 | `business/erp/payable` | `views/business/erp/payable/index.vue` | `business:erp:payable:list` | P0 |
| 13 | 凭证查询 | `business/erp/voucher` | `views/business/erp/voucher/index.vue` | `business:erp:voucher:list` | P0 |
| 14 | 凭证详情 | `business/erp/voucher/detail` | `views/business/erp/voucher/detail.vue` | `business:erp:voucher:detail` | P0 |
| 15 | 总账查询 | `business/erp/ledger` | `views/business/erp/ledger/index.vue` | `business:erp:ledger:list` | P0 |
| 16 | 会计期间 | `business/erp/period` | `views/business/erp/period/index.vue` | `business:erp:period:list` | P1 |
| 17 | 凭证规则 | `business/erp/voucher-rule` | `views/business/erp/voucher-rule/index.vue` | `business:erp:voucher-rule:list` | P1 |

> 编辑页用 `?id=xxx` 区分新建/编辑（与现有 `views/system/user/index.vue` 的抽屉表单不同——单据页字段多，抽屉放不下，必须独立页）。

### 5.2 目录结构

```
frontend/src/
├── api/business/erp/
│   ├── material-api.ts        # 每个模块一个文件，与后端子模块一一对应
│   ├── supplier-api.ts
│   ├── warehouse-api.ts
│   ├── account-api.ts
│   ├── unit-api.ts
│   ├── purchase-order-api.ts
│   ├── stock-in-api.ts
│   ├── inventory-api.ts
│   ├── payable-api.ts
│   ├── voucher-api.ts
│   ├── ledger-api.ts
│   ├── period-api.ts
│   └── voucher-rule-api.ts
├── views/business/erp/
│   ├── shared/                          # ERP 内部复用（不进全局 components）
│   │   ├── DocItemsTable.vue            # ★ 明细行编辑器（受控组件）
│   │   ├── DocStatusTag.vue             # 状态标签（统一色与文案）
│   │   ├── DocLogTimeline.vue           # 操作轨迹时间线
│   │   ├── AccountPicker.vue            # 科目选择器（树形、只列末级）
│   │   ├── AmountCell.vue               # 金额单元格（右对齐 + 千分位 + 负数红）
│   │   ├── SummaryBar.vue               # 单据合计栏
│   │   └── PrintLayout.vue              # 打印布局
│   ├── composables/
│   │   ├── useDocItems.ts               # 明细行状态 + 金额计算（核心）
│   │   ├── useDocForm.ts                # 单据页通用：加载/保存/提交/过账/作废
│   │   └── useDocStatus.ts              # 状态机前端镜像（可执行动作推导）
│   ├── style/tokens.less                # ERP 语义色 token（§5.5）
│   ├── material/index.vue
│   ├── supplier/index.vue
│   ├── ...（其余页面）
│   └── voucher-rule/index.vue
└── utils/
    └── decimal.ts                        # ★ 金额工具（字符串定点运算，零依赖）
```

### 5.3 金额处理契约（前端最容易出错的地方）

**核心事实（§9.1 POC-5 实测）**：后端 `Decimal` 经 serde 序列化为**字符串**，前端拿到的 `"1522.4218"` 不是 number。所有金额/数量字段在 TS 类型里必须声明为 `string`。

```ts
/** 金额/数量一律用字符串承载（后端 Decimal 序列化结果），禁止转 number 做运算后回写 */
export interface StockInRes {
  total_amount: string;      // "1500.0000"
  total_tax: string;         // "195.0000"
  total_with_tax: string;    // "1695.0000"
}
```

**`utils/decimal.ts` 提供的能力（零第三方依赖，约 120 行）**

| 函数 | 说明 | 实现要点 |
|------|------|---------|
| `parseAmount(s: string): bigint` | 字符串 → 定点整数（放大 10^4） | 按小数点拆整数/小数部分，补零到 4 位后拼 `BigInt`，**全程不经过 number** |
| `formatAmount(s, opts)` | 千分位 + 固定小数位 | `Intl.NumberFormat('zh-CN', {minimumFractionDigits: 2})`，输入先转 bigint 再格式化 |
| `addAmounts(...s: string[])` | 定点整数相加 | bigint 相加后按 scale 还原 |
| `mulAmount(qty, price, scale)` | 数量 × 单价，指定结果小数位 | bigint 相乘后按 scale 除（舍去多余位，**四舍五入用手工 half-up**，与后端 `MidpointAwayFromZero` 一致） |
| `toAmountString(v: bigint, scale)` | 定点整数 → 字符串 | 补零、插小数点 |
| `isZero(s)` / `cmp(a,b)` | 判零/比较 | bigint 比较 |

> **为什么用 `bigint` 而不是 `decimal.js`**：前端金额只承担**预览**职责（权威值永远是后端），需求是"加减乘 + 千分位 + 四舍五入"，`bigint` 定点足够且零依赖、零包体、无版本维护成本。`decimal.js-light`（MIT，约 8KB gzip）留作批次 4 若需前端做试算平衡/账龄分析等复杂运算时再引入——见 §9.2 依赖评估。

**金额展示规则（全局统一，写进组件而非各页面各写一遍）**

| 场景 | 格式 | 示例 |
|------|------|------|
| 列表金额列 | 千分位 + 2 位小数，右对齐 | `1,522.42` |
| 单据详情/编辑合计 | 千分位 + 2 位小数，加粗 | `1,695.00` |
| 单价（含税/不含税） | 千分位 + 4 位小数 | `11.1111` |
| 成本价（均价） | 千分位 + 6 位小数 | `10.356611` |
| 数量 | 千分位 + 最多 4 位，去尾零 | `147`、`147.5`、`0.0001` |
| 负数（红字） | 括号 + 红色 | `(1,200.00)` |
| 币种 | 非 CNY 时金额前加币种码 | `USD 1,500.00` |
| 空/NULL | 显示 `—`，不显示 `0.00` | — |

### 5.4 复杂表格方案（明细行编辑 + 含税切换 + 自动计算）

**方案对比**

| 方案 | 做法 | 优势 | 劣势 | 结论 |
|------|------|------|------|------|
| A. 弹窗逐行编辑 | 表格只读，点"添加"弹窗填一行 | 校验空间足，字段多也放得下 | N 行要点 N 次弹窗，录 20 行物料极其痛苦 | ✗ |
| B. 纯行内编辑（单元格放输入框） | `t-table` + 每列 `t-input` | 录入最快，像 Excel | 校验错误没地方显示；列多时列宽挤爆；移动端不可用 | ✗（单独用） |
| C. **行内编辑 + 行尾抽屉补充** | 常用列（物料/数量/单价/税率/金额）行内编辑，次要字段（备注、来源行）点行进入抽屉 | 兼顾速度与完整性 | 需要两份字段定义（列配置 + 抽屉） | ✓ **采用** |
| D. 独立业务组件 | 抽 `DocItemsTable.vue` 受控组件 | 采购订单/入库单/手工凭证**三处复用**，逻辑只写一次 | 前期投入约 2 人日 | ✓ **采用（C+D 组合）** |

**`DocItemsTable.vue` 契约（受控组件，只负责渲染与交互，不持有业务规则）**

```ts
defineProps<{
  modelValue: DocItem[];           // 明细数组（父组件持有，单一数据源）
  columns: DocItemColumn[];        // 列配置（哪些列可编辑、宽度、类型）
  priceIncludeTax: boolean;        // 单据级：单价是否含税录入
  readonly: boolean;               // 已过账单据只读
  materialOptions?: MaterialOption[]; // 物料下拉（带默认税率/单位/存货科目）
}>();
emit('update:modelValue', items);  // 明细变更
emit('row-change', { index, field }); // 行内某字段变化（父组件做联动）
```

**自动计算的实现原则：「单一数据源 + computed，不用 watch 链」**

金额计算最怕的就是 `watch(qty) → 改 amount → watch(amount) → 改 total → watch(total) → 改 ...` 这种链式回写，最终必然出现循环触发和值抖动。正确做法：

```ts
// useDocItems.ts —— 明细的「权威字段」只有 4 个：qty / price / tax_rate / price_include_tax
// amount / tax_amount / with_tax_amount 全部是 computed 派生，不回写到 items（除提交时序列化）
const derived = computed(() => items.value.map((it, i) => {
  const qty = parseAmount(it.qty);
  const priceExcl = props.priceIncludeTax
      ? divByRate(parseAmount(it.price), it.tax_rate)  // 含税价反算不含税
      : parseAmount(it.price);
  const amount = roundScale(mulAmount(qty, priceExcl), 4);
  const tax    = roundScale(mulAmount(amount, it.tax_rate, 4), 4);
  return { amount, tax, withTax: amount + tax };
}));
```

- 表格单元格的 `amount` 列**绑定 `derived[i].amount`（只读）**，绝不绑定 `items[i].amount`；
- 合计栏绑定 `Σ derived`（**后端提交后以响应值覆盖显示**）；
- 这样任何输入变化都只有一条计算路径，不会抖动。

**含税/不含税切换（最易出精度事故的地方，定死规则）**

| 规则 | 说明 |
|------|------|
| 权威值 | **不含税单价 `price` + 税率 `tax_rate`**，这是落库值（DDL 里 `price NUMERIC(18,6)` 语义即不含税单价） |
| 开关语义 | 只改**录入框的显示口径与用户输入的解释方式**，不主动改动 `price` |
| 切到"含税"时 | 输入框显示 `price × (1+rate)`（派生值）；用户不编辑 → `price` 保持不变（**零漂移**） |
| 用户编辑含税价时 | 立即反算 `price = round(含税价 / (1+rate), 6)` 并固定，此后按不含税口径存储 |
| 反复切换 | 因为只在"用户编辑"时才反算，来回切换 10 次也不会漂移（数学上 `÷(1+r) × (1+r) ≠` 恒等，但因为我们不做无谓往返，实际不发生） |
| 提示 | 切换时右上角 Toast：`已切换为含税录入，单价将按 13% 反算为不含税价` |
| 后端校验 | 后端**不接受** `price_include_tax` 字段，只收 `price`（不含税）；防前端传错口径 |

**行级校验（在表格内就地提示，不弹全局消息）**

| 列 | 规则 | 提示位置 |
|----|------|---------|
| 物料 | 必填；同一单据内**不允许重复物料**（可合并） | 单元格红框 + tooltip |
| 数量 | > 0，最多 4 位小数 | 单元格下方 |
| 单价 | ≥ 0，最多 6 位小数 | 单元格下方 |
| 税率 | 0 ≤ rate ≤ 1 | 单元格下方 |
| 收货数量（入库单） | ≤ `remain_qty`（订单行剩余量） | 单元格下方，超量标红 |

### 5.5 配色方案（与现有主题的冲突处理）

**冲突事实**：任务书要求"与现有 TDesign + 莫兰迪配色保持一致"，并给出四色（深海蓝 `#51697C` / 霜月白 `#F2F2EE` / 雾蓝灰 `#AAB7C2` / 漂木棕 `#B79D88`）。但**代码里不存在这套配色**——`src/style/theme.less` 的品牌主色是 `#0580a2`（青蓝 H=193），10 级色阶 + ECharts 图表色全部锁定该色相，且明暗双主题靠镜像 TDesign 选择器实现（改错选择器会导致主题失效）。

**决策：不动全局主题，莫兰迪四色降级为 ERP 局部"业务语义色"**（D9）

| 理由 | 说明 |
|------|------|
| 全站改色 = 12 个既有页面 + 登录页 + 图表的回归风险 | 本次是新增 ERP，不是品牌换肤，不该把风险转嫁给既有功能 |
| 莫兰迪四色的天然用途是"语义标记"而非"交互主色" | 借贷方向、单据状态、汇总区底色——这些恰恰需要与主色区分的低饱和色 |
| 实测发现三色**不能承载白字** | 见下方对比度表：`#F2F2EE` 1.12、`#AAB7C2` 2.05、`#B79D88` 2.56，全部达不到 WCAG AA（4.5）。强推为按钮/标签底色会直接造成可读性问题 |

**WCAG 对比度实测（用 sRGB 相对亮度公式实算，非估算）**

| 颜色 | vs 白字 | vs 霜月白底 | 判定 |
|------|--------|------------|------|
| 深海蓝 `#51697C` | **5.74** ✅ AA | 5.11 ✅ | 可作文字色，也可作深底白字 |
| 霜月白 `#F2F2EE` | 1.12 ❌ | 1.00 | **仅可作底色**，不可作文字/图标 |
| 雾蓝灰 `#AAB7C2` | 2.05 ❌ | 1.82 | **仅可作边框/分割线/禁用底** |
| 漂木棕 `#B79D88` | 2.56 ❌ | 2.28 | **仅可作底色标记**（配深字 6.79 ✅） |
| 现有主色 `#0580a2` | 4.55 ✅ AA | 4.05 | 保持不变 |

**落地 token（`views/business/erp/style/tokens.less`，只在 ERP 页面根节点作用域内生效）**

```less
// ERP 业务语义色：仅用于「借贷方向 / 单据状态 / 汇总区 / 边框」，不参与交互主色
.erp-scope {
  // —— 借贷方向 ——
  --erp-debit:          #51697C;  // 借方（深海蓝，白/浅底均 >= 5.1）
  --erp-debit-bg:       #EDF1F4;  // 借方底色（同色相提亮）
  --erp-credit:         #7D5F49;  // 贷方（漂木棕压深两档，白底 5.83 / 浅底 5.19，保色相）
  --erp-credit-bg:      #F2F2EE;  // 贷方底色（霜月白）
  // —— 面/线 ——
  --erp-surface:        #F2F2EE;  // 表头底 / 汇总行底
  --erp-border:         #AAB7C2;  // 分组边框 / 分割线（仅线条，不作文字）
  // —— 文字 ——
  --erp-text-secondary: #5E6E78;  // 次要文字（白底 5.28 ✅）
  --erp-text-strong:    #51697C;  // 强调文字（单号、科目名）
}
```

- 弃用裸 `#B79D88` 作文字色：压深到 `#7D5F49` 才满足 AA（保留原色相，观感仍是同一族的暖棕）；
- `#F2F2EE` / `#AAB7C2` 严格限制为面/线用途，禁止 `.setColor(#AAB7C2)` 这类用法；
- 状态标签沿用 TDesign 的 `t-tag` 主题色（`success/warning/danger/default/primary`），**不新造状态色**。

**状态 → 标签映射（`DocStatusTag.vue` 单一实现）**

| 单据状态 | TDesign theme | 文案 |
|---------|--------------|------|
| `DRAFT` | `default` | 草稿 |
| `SUBMITTED` | `primary` | 已提交 |
| `APPROVED` | `primary` | 已审批 |
| `PARTIAL_RECEIVED` | `warning` | 部分收货 |
| `RECEIVED` | `success` | 已收货 |
| `POSTED` | `success` | 已过账 |
| `CLOSED` | `default` | 已关闭 |
| `CANCELLED` | `danger` | 已作废 |
| `REVERSED` | `danger` | 已冲销 |
| `OPEN` | `warning` | 未付 |
| `PARTIAL_PAID` | `warning` | 部分已付 |
| `SETTLED` | `success` | 已结清 |

### 5.6 单据页交互骨架（`useDocForm.ts` 统一）

所有单据页（采购订单/入库单/手工凭证）共用同一套页面结构与动作栏，避免各写一遍：

```
┌─ 页头：单据类型 + 单号（新建显示"保存后生成"） + DocStatusTag ──────────┐
│  [返回]                        [保存草稿] [提交] [过账] [作废] [打印]  │
├─ 基本信息区（t-form，2~3 列，TDesign Grid 12 列制）─────────────────────┤
│  供应商  订单日期  期望到货  仓库  币种  账期  备注                      │
├─ 明细区：DocItemsTable（行内编辑）─────────────────────────────────────┤
│  序号 物料 规格 单位 数量 单价 税率 金额 税额 价税合计 备注  [删除]     │
│  [+ 添加明细]  [从采购订单带入]                                          │
├─ 合计栏：数量合计 / 不含税 / 税额 / 价税合计（右对齐，加粗）───────────┤
└─ 底部 Tab：操作轨迹（DocLogTimeline） / 关联单据（凭证、应付）──────────┘
```

- **动作按钮的可用性由状态机推导**（`useDocStatus.ts` 内含 §3.4.1 的迁移表前端镜像），不是一堆 `v-if` 散落；
- 过账/作废/冲销三个高风险动作必须 `t-popconfirm` 二次确认，并展示影响面（"将生成凭证与应付单，库存增加 X，不可撤销"）；
- 后端返回的 400 message **原样展示**（§4.1 已约定文案），前端不做二次翻译——会计错误信息由后端统一措辞才能保证一致性。

### 5.7 权限控制

| 层 | 实现 |
|----|------|
| 路由级 | `meta.permission` → `router.beforeEach` 已有逻辑自动生效（无需改守卫） |
| 菜单级 | `sys_menu` 种子（`V0038`）+ 现有动态菜单渲染（`module_code='business'`） |
| 按钮级 | `v-perm="'business:erp:stock-in:post'"`（项目已有 `src/directives`，照现有用法） |

> 权限码必须与 §4.9 表**逐字一致**。允许前端隐藏按钮，但**后端校验不可省**——`#[nik::permission]` 是唯一有效防线（前端隐藏可被绕过）。

---
## 6. 扩展性预留（为第 2/3/4 批铺路）

### 6.1 原则：只预留"不改变现有主键/唯一键语义"的扩展点

| 扩展点类型 | 是否预留 | 理由 |
|-----------|---------|------|
| 加枚举值（`biz_type` / `event_type` / `doc_type`） | ✅ 预留 | VARCHAR 列，加值零成本，只需应用层认 |
| 加可空列（`ALTER TABLE ADD COLUMN ... NULL`） | ✅ 无需现在做 | PG 加可空列是 O(1) 元数据操作，不重写表；**现在加等于留无用字段** |
| 改唯一键（如库存加批次维度） | ❌ 不预留 | 会污染当前语义（`uk_biz_inv_mat_wh` 变 `(mat, wh, batch)` 是结构性变更），且当前无业务驱动 |
| 加辅助维度（凭证分录） | ✅ 已用 `aux_extra JSONB` 兜底 | 任意新维度可先落 JSON，稳定后再提列 |

### 6.2 销售 / 生产如何复用本批能力

**复用点 1：库存增减（`inventory/model.rs` 暴露两个事务级函数）**

```rust
/// 入库（采购入库 / 生产入库 / 调拨入 / 盘盈）
/// `tx` 由调用方 handler 开启，本函数不自行开事务 —— 保证"一次业务动作一个事务"
pub(crate) async fn apply_inbound(
    tx: &Transaction,
    req: &InboundReq,      // 单号/行(物料,仓库,数量,金额,业务类型)/期间/操作人
) -> DbResult<Vec<InventoryTxnModel>>;

/// 出库（销售出库 / 生产领料 / 调拨出 / 盘亏）
pub(crate) async fn apply_outbound(
    tx: &Transaction,
    req: &OutboundReq,
) -> DbResult<Vec<InventoryTxnModel>>;
```

- 销售出库、生产领料、生产入库、调拨，**全部走这两个函数**，成本算法（移动加权、锁顺序、清空倒挤）只实现一次；
- `biz_inventory_txn.biz_type` 已预留 `SALE_OUT` / `PRODUCTION_IN` / `PRODUCTION_OUT` / `ADJUST_IN` / `ADJUST_OUT` 枚举值（VARCHAR 无 CHECK 枚举约束，加值不需要迁移）。

**复用点 2：凭证生成（`voucher/engine.rs` 的入参是与单据类型无关的事件结构）**

```rust
/// 凭证事件：引擎只认「事件类型 + 明细行 + 合计」，不认具体单据
pub(crate) struct VoucherEvent<'a> {
    pub event_type: &'a str,           // STOCK_IN / STOCK_OUT / PAYMENT / PRODUCTION_IN ...
    pub doc_id: uuid::Uuid,
    pub doc_no: &'a str,
    pub doc_date: chrono::NaiveDate,
    pub supplier_id: Option<uuid::Uuid>,   // 采购类事件
    pub customer_id: Option<uuid::Uuid>,   // ★ 销售类事件（批次 2 使用，本批恒 None）
    pub warehouse_id: Option<uuid::Uuid>,
    pub lines: Vec<VoucherEventLine>,      // material_id / qty / amount_excl / tax / with_tax
    pub totals: VoucherEventTotals,        // amount_excl / tax / with_tax（单据级，权威）
}

pub(crate) async fn generate(
    tx: &Transaction,
    event: &VoucherEvent<'_>,
) -> DbResult<VoucherModel>;
```

- 批次 2 的销售出库单只需构造一个 `VoucherEvent { event_type: "STOCK_OUT", customer_id: Some(..), .. }` 即可复用整个引擎；
- 规则表按 `event_type` 区分模板，销售只需新增 `STOCK_OUT_DEFAULT` 规则（借 主营业务成本 / 贷 库存商品），**不改代码**。

**需要批次 2 补的一件事**：`biz_voucher_rule_entry` 目前只有 `aux_supplier`，销售需要 `aux_customer`。届时 `ALTER TABLE biz_voucher_rule_entry ADD COLUMN aux_customer_id UUID` + `biz_voucher_entry ADD COLUMN aux_customer_id UUID`——两列可空、无默认值，PG 秒级完成，不影响存量数据。

**复用点 3：单据框架（状态机 + 轨迹 + 取号 + 编号）**

`shared/{doc_status,doc_log,seq}.rs` 与 `DocItemsTable.vue` / `useDocForm.ts` 是**单据无关**的，销售订单、出库单、调拨单、生产工单直接复用。

### 6.3 多层 BOM：明确**不预留**字段（附理由）

| 方案 | 做法 | 问题 |
|------|------|------|
| A. 在 `biz_material` 上加 `bom_version` / `is_purchased` / `lead_time` 等 | "提前占位" | 典型**猜测式设计**：BOM 是独立实体（父子物料 + 用量 + 层级 + 损耗率 + 版本生效期），核心信息全在关系上，material 上的标量字段撑不出一棵多层树。批次 3 设计 BOM 时这些列大概率用不上，却要一直背着 |
| B. 什么都不加，批次 3 新建 `biz_bom` / `biz_bom_line` | 到用时再设计 | ✓ 唯一代价是批次 3 多一次迁移（成本极低） |

**结论：B 方案。** 本批**已经够用**的 BOM 前置条件是：
1. `material_type` 有 `SEMI`（半成品）/ `FINISHED`（成品），可标识"可生产物料"；
2. `biz_inventory_txn.biz_type` 预留 `PRODUCTION_IN` / `PRODUCTION_OUT`；
3. `material_type='SERVICE'` 已存在，支持"生产过程中不计库存的服务项"。

这三项不构成 BOM 表结构，但让批次 3 的 BOM **能直接挂到现有物料上**，不需要改物料表。

### 6.4 多仓库调拨：本批表结构**已足够**，不需要预留

一次调拨 = 调出仓 OUT + 调入仓 IN，成本按"调出仓当前均价"结转：

| 需求 | 现有表能否支撑 | 说明 |
|------|--------------|------|
| 两条库存流水 | ✅ | `biz_inventory_txn` 加 `biz_type='TRANSFER_OUT'/'TRANSFER_IN'`，无需迁移 |
| 调出仓成本结转 | ✅ | `apply_outbound` 按调出仓 `avg_cost` 算 `amount`；`apply_inbound` 以该 `amount` 重算调入仓均价 |
| 调拨单实体 | 需新建 `biz_transfer` / `biz_transfer_item` | 批次 3，结构与 `biz_stock_in` 同构（可直接照抄四件套） |
| 凭证 | **不生成**（或仅管理需要时生成内部结转凭证） | 调拨不产生损益、不改变总资产，会计上通常不记账（同主体内部移动）。若客户要求分仓核算，才需要"库存商品—A仓 / 库存商品—B仓"两个科目并生成凭证——届时靠规则表 `event_type='TRANSFER'` 配置即可 |
| 负库存风险 | ⚠️ 新增 | 调出量 > 调出仓库存 → 必须走 `allow_negative` 校验（§3.2.4） |

---

## 7. 实施计划（第 1 批内部 6 步）

### 7.1 分步交付（每步都可独立验收、独立上线不破坏已有功能）

| 步 | 内容 | 交付物 | 验收标准 | 人日 |
|---|------|--------|---------|------|
| **①** | 主数据 + 科目 + 期间 + 取号 | `V0030`/`V0037` 迁移；`business/erp/{unit,warehouse,account,material,supplier}/` 四件套；`shared/{money,seq,period,doc_status}.rs` + 单测 | ① 5 个 CRUD 接口全部可用，权限码三方一致；② 科目树可按父子展开、末级校验生效；③ 期间列表 + 开启接口可用；④ `cargo test` 全绿，`money.rs` 的舍入/尾差分摊单测覆盖 ≥ 8 个用例；⑤ 迁移 `up → down → up` 可重复执行 | 5 |
| **②** | 采购订单 | `V0031` 迁移；`purchase_order/` 四件套 | ① 主+明细一次提交、金额由后端算；② 状态机 4 条迁移（提交/审批/关闭/作废）全部生效且非法迁移返回 400；③ 单号 `PO-YYYYMMDD-NNNNNN` 连续不重；④ 明细金额恒等（`with_tax = amount + tax`）被 CHECK 兜住 | 4 |
| **③** | 入库 + 库存 + 成本 | `V0032` 迁移；`stock_in/`、`inventory/` 四件套 | ① 过账成功写出库存/流水/应付/凭证全部数据（此时凭证可用简化版：直接读规则种子）；② **并发测试**：同一物料同仓 20 并发过账，最终 `qty/amount/avg_cost` 与串行结果完全一致；③ 重复过账返回 400；④ 超收被拒；⑤ 一致性巡检 SQL 返回 0 行 | 7 |
| **④** | 应付 | `V0033` 迁移；`payable/` 四件套 | ① 一入库单只生成一张应付（并发下 `uk_biz_payable_source` 生效）；② 到期日 = 入库日 + 账期；③ 手工核销金额校验 + 状态流转正确；④ 账龄分档正确 | 3 |
| **⑤** | 凭证引擎 + 总账 | `V0034`/`V0035` 迁移；`voucher/{model,engine}.rs`、`ledger/` | ① 规则命中/优先级/无规则拒绝三场景覆盖；② 四级科目解析链每条路径都有单测；③ 借贷平衡 L1/L2/L3 三层校验均生效；④ 冲销生成红字凭证且原凭证 `REVERSED`；⑤ 跨期冲销正确（原期间余额不变）；⑥ 试算平衡接口 `balanced=true`；⑦ 规则 preview 接口输出与真实过账结果一致 | 9 |
| **⑥** | 前端 17 页 | `api/business/erp/*`、`views/business/erp/*`、`router` 追加、`utils/decimal.ts` | ① 全部页面可用，权限码与后端一致；② 明细行编辑金额自动算、含税切换不漂移；③ 金额展示统一（千分位/右对齐/负数红）；④ `pnpm typecheck` 零错误；⑤ 过账/作废二次确认且展示影响面 | 13 |

**合计 41 人日**

### 7.2 两种排期

| 配置 | 周期 | 关键路径 | 说明 |
|------|------|---------|------|
| **单人全栈** | ≈ 8.2 周（41 人日） | ③ → ⑤ → ⑥ | 前端与后端串行，最慢 |
| **1 后端 + 1 前端并行**（推荐） | ≈ **6 周** | 后端 ①→②→③→④→⑤（28 人日 ≈ 5.6 周）；前端在 ① 完成后即可并行做 6 个主数据页，③④⑤ 联调 | 前端依赖后端接口契约（本文档 §4 已定死），可提前用 mock 开发 |

**关键路径说明**：③（入库+库存+成本）与 ⑤（凭证引擎）是技术风险集中区，且 ⑤ 依赖 ③ 的数据流，**不能并行**。前端 ⑥ 中「采购订单」「入库」两页依赖 ②③ 的接口，其余可提前。

### 7.3 里程碑检查点（每步结束必须过）

```
每步完成必须同时满足：
□ cargo fmt && cargo clippy -- -D warnings && cargo test 全绿
□ pnpm typecheck 零错误（涉及前端时）
□ 新增权限码三方一致（sys_menu.auth_code / 注解 / 前端）
□ 新增表有 up+down 迁移且可重复执行
□ 关键纯逻辑（金额/成本/平衡/科目解析）有单测，且单测里用的是真实数值（不是"跑通即可"）
□ 迁移只在 cloudpivot_test 库验证，不碰生产库
```

---

## 8. 风险清单

### 8.1 会计正确性风险（最高优先级）

| # | 风险 | 概率 | 影响 | 缓解措施 | 兜底 |
|---|------|------|------|---------|------|
| R1 | 凭证借贷不平（尾差/取数口径不一致） | 中 | **致命**（错账） | ① 贷方取单据级 `total_with_tax`，借方取行级汇总但总额来自单据同一份 `total_*`；② 税额恒由"价税合计 - 不含税"倒挤，不做二次乘法；③ DB `ck_biz_voucher_balance` 强制；④ 引擎内 L1 校验 + 余额表 L3 校验；⑤ POC-4 已实证行级除税会产生 -0.01 尾差 | 过账失败（拒绝入账）> 错误入账 |
| R2 | 科目映射错（借贷科目配反/用了非末级/停用科目） | 中 | **致命** | ① 规则 `preview` 接口上线前试算；② 科目末级 + 启用校验（§3.1.7）；③ 科目类别与方向一致性校验（§4.7.4）；④ 默认规则种子只配 3 条分录，人工可读可审 | 停用规则 → 过账失败 |
| R3 | 余额方向算错（负债类显示为负） | 中 | 高（报表全错） | ① 用"带符号中间量 + 方向列"统一公式（§2.5.3），不写分支；② 单测覆盖"资产类出现贷方余额"（累计折旧）、"负债类出现借方余额"（预付）两种翻向场景；③ 试算平衡接口 | 批次 4 结账前全量重算 |
| R4 | 期间控制失效（往已结账期间写凭证） | 低 | **致命**（审计不可接受） | ① 所有写凭证入口统一走 `period::ensure_open`；② 校验用表驱动（`start_date/end_date/status`），不硬编码月份；③ 单测覆盖 NOT_OPENED / CLOSED / 不存在三种拒绝 | 已结账数据只可红字跨期更正 |
| R5 | 红冲后账不平 | 低 | 高 | ① 红字用"借贷互换"而非负数（§2.5.2），保持单边恒正；② 金额取原值不倒算；③ 冲销后校验余额表试算平衡 | 拒绝冲销 |

### 8.2 并发与事务风险

| # | 风险 | 概率 | 影响 | 缓解措施 | 兜底 |
|---|------|------|------|---------|------|
| R6 | 同一单据被并发过账两次 → 库存翻倍 | 中 | **致命** | ① 事务内 `SELECT ... FOR UPDATE` 锁单据行（§3.4.2 步骤 2）；② `uk_biz_payable_source` 唯一索引兜底（第二张应付插不进去） | DB 唯一键使第二次事务整体回滚 |
| R7 | 同物料同仓并发入库 → 均价算错 | 中 | 高 | ① `FOR UPDATE` 锁库存行；② 按 `material_id` 升序加锁防死锁；③ 单内同物料多行先合并 | 一致性巡检 SQL 发现后按流水重算 |
| R8 | 死锁（PG 40P01） | 中（若锁顺序写错则高） | 中（用户看到报错） | ① 锁顺序约定写进代码注释 + code review checklist；② ③ 步验收含 20 并发压测；③ **不做自动重试**（会掩盖 bug） | 死锁率 > 0.1% 即回查锁顺序 |
| R9 | 取号热点（`biz_seq` 单行锁） | 低 | 低 | 单据量日均百级，单行 UPDATE 持锁 < 1ms；不引入 Redis | 若将来量级上到千级/日，改为按日分行的 `biz_seq` + 唯一键重试 |
| R10 | 长事务持锁（一单 20 行 + 5 张表） | 中 | 中 | ① 目标 < 100ms；② 事务内禁止外部 IO（不发通知、不调第三方）；③ 过账接口 P99 < 300ms 验收 | 超过 1s 需拆分或异步化（批次 3） |

### 8.3 与 nik 框架集成风险

| # | 风险 | 概率 | 影响 | 缓解措施 |
|---|------|------|------|---------|
| R11 | `rust_decimal` feature 引入编译问题 | 低 | 中 | **已实测**（§9.1 POC-6）：`nik 0.4.2 + features=["lite","rust_decimal"]` 依赖解析成功（352 包，rust_decimal 1.43.0，无版本冲突）。实施第一步就是加 feature + `cargo check` |
| R12 | `Decimal` 参数绑错列类型 | 中 | 中（运行时报错） | PG 驱动要求 `Value::Decimal` 只能绑 `NUMERIC` 列（§1.4）；所有金额列统一定义为 NUMERIC，模型字段统一 `Decimal`，映射一一对应 |
| R13 | 想加 service 层 / 想在 model 里开事务 | 中 | 中（破坏架构一致性） | ① §3.0 已明确 engine 是 model 层；② 事务只在 handler 开；③ code review 检查"model 里是否出现 `db().transaction`"（现有代码里仅 `count_by` 类读函数在 model，事务在 handler） |
| R14 | 错误出口用错（model 返回 ApiError） | 中 | 低 | `DbResult` / `ResResult` / `nik::Result` 三出口铁律；`utils::db_error` 补中文上下文；clippy `-D warnings` 拦住 `unwrap` |
| R15 | 宏路径误加 `/api` | 中 | 中（404） | 后端宏路径一律 `/business/erp/...`；前端一律 `/api/business/erp/...`；code review 逐条核对 §4 表格 |
| R16 | 迁移号段冲突 | 低 | 低 | 本批占 V0030–V0038；动手前 `ls migrations/*/` 复核；已应用迁移禁止回改 |

### 8.4 工期与数据风险

| # | 风险 | 概率 | 影响 | 缓解 / 降级预案 |
|---|------|------|------|----------------|
| R17 | ③⑤ 两步低估（各 7/9 人日） | 高 | 高 | ① ③⑤ 各留 2 人日缓冲；② 若超期，**优先降级 UI 而非正确性**（见下方降级顺序） |
| R18 | 前端 17 页工作量溢出 | 中 | 中 | 优先交付 P0 的 11 页（物料/供应商/仓库/科目/采购单/入库/即时库存/应付/凭证/总账 6 个主数据页）；P1 的 6 页（单位/流水/期间/规则/凭证详情…）可延后到批次 2 前补齐 |
| R19 | 会计科目种子不准（用错准则） | 中 | 高 | 种子按《小企业会计准则》常用科目，**上线前必须由客户财务确认**（列入 §9.4 待确认项）；科目可增删改，配错可修 |
| R20 | 历史期初数据（库存/应付余额）需导入 | 高 | 中 | 本批**不做导入工具**（不在范围）；上线时由财务手工录入期初库存（用"其他入库"单据 + 期初应付单据），或批次 2 提供导入 |
| R21 | 前端金额精度（JS number） | 中 | 中 | `utils/decimal.ts` 用 bigint 定点，不经过 number；提交后以响应值重绘；后端不接受前端计算的金额（后端重算） |

### 8.5 降级顺序（若工期被迫压缩，按此顺序砍）

```
优先砍（不影响会计正确性）：
 1. 凭证规则管理界面（P1）→ 规则改用 SQL 种子维护
 2. 规则 preview 接口 → 用测试单据替代
 3. 库存流水成本追溯 UI（流水接口保留）
 4. 计量单位辅助单位换算（只保留基础单位）
 5. 打印布局
 6. 账龄分析 / 试算平衡接口（保留 SQL，接口延后）

绝不砍（砍了就不是会计系统）：
 ✗ 借贷平衡校验（三层）
 ✗ 期间控制
 ✗ 单据事务边界 + 行锁幂等
 ✗ 库存行锁 + 锁顺序
 ✗ 红字冲销
 ✗ 权限点后端校验
```

---

## 9. 附录

### 9.1 POC 记录（真实执行，非推演）

**环境**：Linux，Rust 工具链（与项目 MSRV 1.94.1 兼容），`rust_decimal 1.43.0`；
**脚本位置**：`~/…/cache/scratch/decimal-poc/`（临时验证，**不属于交付代码**，验收后即清理）。

#### POC-1/2/3 移动加权平均 + 尾差倒挤 + 浮点对比

```
[POC1] 期初 qty=100 amt=1012.34
[POC1] 入库后 qty=150 amt=1567.895 移动均价(6位)=10.452633
[POC1] 再入库后 qty=180 amt=1864.190 均价=10.356611
[POC1] 出库33件 成本=341.7682 结存 qty=147 amt=1522.4218
[POC1] 结存金额校验 qty*price=1522.4218          ← 完全相等，无残差
[POC2] 清空出库 qty=147 倒挤成本=1522.4218 出清后 amt=0.0000
[POC3] f64 累加 = 1864.1900000000  Decimal 累加 = 1864.190
```

**结论**：金额 4 位 + 单价 6 位 + 清空倒挤的方案数学自洽；`Decimal` 累加结果精确。

#### POC-4 含税拆分与行级尾差（证明"为什么必须整单口径"）

```
[POC4] gross=1130.00 net=1000 tax=130.00 net+tax=1130.00 balanced=true
[POC4] 行不含税合计=530.99 合计不含税=530.98 差=-0.01      ← 行级四舍五入产生 1 分尾差
```

**结论**：3 行含税金额 `100.00 / 200.00 / 300.01`、税率 13% 时，**行级除税之和与整单除税差 1 分**。因此凭证取数必须以单据级合计为准（§3.1.5），行级金额只用于展示与辅助核算。

#### POC-5 Decimal 的 serde 行为（决定前后端金额契约）

```
[POC5] Decimal serde_json = "1234567.8901"    ← 序列化为字符串（带引号）
[POC5] roundtrip == orig : true
```

**结论**：前端拿到的是 string，TypeScript 类型必须声明 `string`，且不能直接参与算术（§5.3）。

#### POC-6 依赖可行性（`nik` + `rust_decimal` feature）

```
cargo metadata 解析结果（features = ["lite","rust_decimal"]）：
nik                       -> 0.4.2
nik-db                    -> 0.4.2
nik-db-core               -> 0.4.2
nik-db-driver-postgres    -> 0.4.2
rust_decimal              -> 1.43.0
chrono                    -> 0.4.45
uuid                      -> 1.29.0
serde                     -> 1.0.229
总包数: 352
nik-db 依赖 rust_decimal: True
nik-db-core 依赖 rust_decimal: True
```

**关键代码事实（读源码确认，非推测）**：
- `nik::prelude::rust_decimal` 可用（`nik-db-core/src/lib.rs` 的 `prelude` 模块 `#[cfg(feature="rust_decimal")] pub use rust_decimal;`）→ **业务代码无需在 Cargo.toml 声明 rust_decimal 直接依赖**，符合"只声明 nik"铁律；
- `Decimal` 实现 `TypeMapper`（`type_mapper.rs` 的 `rust_decimal_impls`）：`stmt_type() = Type::Decimal`，`to_value() = Value::Decimal(CompactDecimal{mantissa, scale})`；
- PG 驱动原生支持 NUMERIC 二进制编解码（`convert.rs` 的 `encode_numeric` / `decode_numeric`，`PgParam::Numeric` / `PgValue::Numeric`）；
- **约束**：`PgParam::Numeric` 要求目标列类型为 `Type::NUMERIC`（`convert.rs:298`），绑错列型直接报错；
- `Value` **没有** `From<Decimal>` 实现 → 传参数必须用 `TypeMapper::to_value(&d)`。

#### POC-7 WCAG 对比度（决定配色）

```
#51697C vs #FFFFFF : 5.74   #51697C vs #F2F2EE : 5.11
#F2F2EE vs #FFFFFF : 1.12   #AAB7C2 vs #FFFFFF : 2.05   #B79D88 vs #FFFFFF : 2.56
#B79D88 vs #1A1A1A : 6.79   #7D5F79(压深棕) vs #FFFFFF : 5.83
#0580a2 vs #FFFFFF : 4.55
```
**结论**：原四色中三色不可承载白字，因此降级为面/线/底色语义色，贷方文字改用压深版 `#7D5F49`（§5.5）。

#### 9.1.8 DDL 与会计约束在真实 PostgreSQL 上的验证（不是纸面 SQL）

前面 9.1.1–9.1.7 验证的是**算法与依赖**。DDL 本身也做了真库验证——把本文档 §2 的 20 个 `CREATE TABLE` 块**原样提取**（脚本自动从本文档抓取，不是另写一份），在真实 PostgreSQL 上执行，再用 12 组业务场景检验约束是否真的拦得住。

**环境**：PostgreSQL **18.6**（本机实例），临时库 `erp_ddl_check`，验证后整库删除（不触碰项目库 `cloudpivot_test` / `cailu`）。

> **版本兼容性说明**：验证跑在 18.6 上，而项目用 `postgres:16-alpine`。本文档 DDL 与 SQL 只用到 PG13+ 已有的特性（`gen_random_uuid()`、部分唯一索引 `WHERE deleted_at IS NULL`、`ON CONFLICT DO UPDATE`、`CHECK` 约束、`INET`/`JSONB`、CTE 中的 `UPDATE ... RETURNING`），**PG16 全部支持**，不存在版本依赖风险。

**① 建表：20 张表全部成功，零错误**

```
$ psql -d erp_ddl_check -v ON_ERROR_STOP=1 -f ddl_check.sql   # 从文档提取，含 BEGIN/COMMIT
DDL OK

$ psql -d erp_ddl_check -tAc "select tablename from pg_tables
    where schemaname='public' order by tablename"
biz_account            biz_accounting_period   biz_doc_log             biz_inventory
biz_inventory_txn      biz_material            biz_payable             biz_purchase_order
biz_purchase_order_item biz_seq                biz_stock_in            biz_stock_in_item
biz_supplier           biz_unit                biz_voucher             biz_voucher_entry
biz_account_balance    biz_voucher_rule        biz_voucher_rule_entry  biz_warehouse
（20 行，含全部索引与 CHECK 约束）
```

**② 约束验证：10 项全部按设计生效**

| # | 验证场景 | 预期 | 真实结果（psql 输出原文） |
|---|---------|------|--------------------------|
| T1 | 插入借贷不平凭证（借 100 / 贷 90） | 拒绝 | ✅ `violates check constraint "ck_biz_voucher_balance"` |
| T2 | 插入双边分录（借 100 / 贷 50 同行） | 拒绝 | ✅ `violates check constraint "ck_biz_voucher_entry_side"` |
| T2b | 插入全零分录（借 0 / 贷 0） | 拒绝 | ✅ 通过（同一条 side 约束覆盖） |
| T3 | 入库单 `1000 + 130 ≠ 1200` | 拒绝 | ✅ `violates check constraint "ck_biz_si_tax"` |
| T4 | 同一入库单生成第二张应付 | 拒绝 | ✅ `duplicate key value violates unique constraint "uk_biz_payable_source"` |
| T5 | 连续取号 + 跨日重置 | 1,2,3 → 1 | ✅ 见下表 |
| T6 | **完整过账数据流**（入库单+2 明细+2 库存行+2 流水+凭证+3 分录+应付） | 全部写入成功 | ✅ 无错误 |
| T7 | 科目余额方向判定（资产/负债/借方专栏） | 三科目均"同向" | ✅ 见下表 |
| T8 | 试算平衡（Σ本期借 vs Σ本期贷） | 1695 = 1695 | ✅ `✅ 平衡` |
| T9 | 库存一致性巡检 | 0 行；人为破坏后 1 行 | ✅ `0` → `1` |
| T10 | 资产类出现贷方余额时方向翻转 | `500.0000 / CREDIT` | ✅ 自动翻转 |
| T11 | 期间状态 | OPEN 可记，NOT_OPENED/CLOSED 拒绝 | ✅ 见下表 |
| T12 | 软删后编码可复用（部分唯一索引） | 未删行拒绝重复，软删后允许 | ✅ `T12a PASS`；M-1002 有效行 1 / 含已删 2 |

**③ 取号（T5）真实输出——原子取号 + 跨日重置**

```
 次序 |  周期键  | 流水号
------+----------+--------
    1 | 20261027 |      1
    2 | 20261027 |      2
    3 | 20261027 |      3
    4 | 20261028 |      1     ← 跨日自动重置
```

**④ 科目余额方向（T7）真实输出——三科目全部"同向"**

```
    科目    | 期末余额  |  方向  |  本期借   |  本期贷   | 方向判定
------------+-----------+--------+-----------+-----------+----------
 1405       | 1500.0000 | DEBIT  | 1500.0000 |    0.0000 | 同向
 2202.02    | 1695.0000 | CREDIT |    0.0000 | 1695.0000 | 同向
 2221.01.01 |  195.0000 | DEBIT  |  195.0000 |    0.0000 | 同向
```

**⑤ 试算平衡（T8）真实输出**

```
 借方合计  | 贷方合计  |  结论
-----------+-----------+---------
 1695.0000 | 1695.0000 | ✅ 平衡      ← 余额表
 1795.0000 | 1795.0000 | ✅ 平衡      ← 凭证表（含 T2 用的那张测试凭证）
 1695.0000 | 1695.0000 | ✅ 平衡      ← 分录表
```

**⑥ 本次验证发现并修正的 1 个真实设计缺陷**

初版种子把 `2221.01.01 应交税费—应交增值税—进项税额` 定义为 `LIABILITY + CREDIT`。真库验证时 T7 输出：

```
 2221.01.01 |  195.0000 | DEBIT  |  195.0000 |  0.0000 | ★反向（备抵/预付情形）
```

试算平衡没问题（借贷仍相等），但总账展示会把它标成"异常方向"。**根因是科目性质定义错**：进项税额是负债类科目下的**借方专栏**（登记可抵扣进项），标准做法是 `account_category='LIABILITY'` + `balance_direction='DEBIT'`，而不是笼统按"负债→贷方"套。

已修正 §4.10 种子，并把 §4.7.4 的校验策略从"LIABILITY 强制 CREDIT"改为"默认 CREDIT、借方专栏允许 DEBIT（告警不阻断）"。

> **这就是为什么设计阶段必须把 DDL 跑一遍**：这类问题在纸面 SQL 上看不出来，只有真实数据流过余额方向公式才会暴露。若留到实施阶段才发现，种子数据已被客户财务确认、科目已被凭证引用，改起来要迁移 + 重算余额。

**验证脚本**：`ddl_check.sql`（从本文档自动提取的 20 个建表块）与 `verify_erp.sql`（12 组场景）随本文档一同交付，可随时在任意 PostgreSQL 16+ 上复现；验证完成后临时库已删除。

### 9.2 第三方库评估

| 库 | 用途 | License | 包体 | 维护活跃度 | 与现有依赖冲突 | 结论 |
|----|------|---------|------|-----------|--------------|------|
| `rust_decimal` 1.43.0 | 后端金额/成本计算 | **MIT** | crate 压缩 168KB / 解压 976KB；必需依赖仅 `arrayvec 0.7`（`serde` 由默认 features 开启） | rust-num 组织维护，1.x 稳定线，2025 仍有发布 | **无冲突**：`Cargo.lock` 现有依赖中不含 rust_decimal；经 `nik` 的 `rust_decimal` feature 间接引入，不新增顶层 crate 声明 | ✅ **采用**（经 nik feature，不直接依赖） |
| `decimal.js-light` | 前端金额运算（备选） | MIT | ≈8KB gzip | 活跃（MikeMcl） | 与现有前端依赖无冲突 | ❌ **本批不引入**：前端金额只做预览，`bigint` 定点够用且零依赖。批次 4 若需前端试算平衡/账龄运算再评估引入 |
| `bignumber.js` | 前端大数（备选） | MIT | ≈15KB gzip | 活跃 | 无冲突 | ❌ 不引入（同上的理由，且体积更大） |
| 任何 ORM / 迁移工具（sea-orm、sqlx、diesel） | — | — | — | — | **与框架铁律冲突**（只声明 nik） | ❌ 禁止 |

> **不引入的第三方能力**：PDF 生成（打印用前端 `window.print()` + 打印样式）、定时任务外的消息队列（无跨服务异步需求）、Redis 取号（D1 已否决）。ERP 本批的复杂度全部落在**领域逻辑**上，不在技术组件上——这也是能在 nik 里落地的原因。

### 9.3 决策日志（ADR 摘要）

| ID | 决策 | 备选方案 | 采纳理由 | 可逆性 |
|----|------|---------|---------|--------|
| D1 | `Decimal` + nik `rust_decimal` feature | ① `i64` 存"分" ② `f64` ③ `bigdecimal` | ① i64 存分无法表达 6 位单价（移动加权必需）；② f64 精度不可接受；③ bigdecimal 同样可用但性能与生态弱于 rust_decimal，且 nik 两 feature 都支持，选社区更活跃的 | 可逆（换类型需迁移数据，成本高） |
| D2 | 金额 NUMERIC(18,4) / 单价 NUMERIC(18,6) | 统一 NUMERIC(18,2) | 2 位小数会在移动加权中持续被舍入，误差累积可见 | 可逆（ALTER 提精度安全） |
| D3 | 状态列 VARCHAR + CHECK | SMALLINT + 字典 | 会计/审计直接读 SQL 可懂；日志表 `from_status/to_status` 天然一致 | 可逆 |
| D4 | 入库即生成凭证（贷方暂估应付） | 发票到达才生成 | 本批无发票模块；暂估科目为批次 2 冲回留路径 | 可逆（改规则即可） |
| D5 | 科目映射四级解析链 | 纯主数据 / 纯规则表 | 兼顾精确覆盖与兜底；新增物料不配也能跑 | 可逆 |
| D6 | 双层日志（operation_logs + biz_doc_log） | 只用其中一个 | 中间件是异步旁路（回滚后仍写，会留假日志）+ 描述硬编码不可改（红线） | 可逆 |
| D7 | `FOR UPDATE` 行锁 + material_id 排序 | 乐观锁 / SERIALIZABLE | 乐观锁在此需重算整单；排序加锁防死锁 | 可逆（改锁策略需改代码） |
| D8 | 负库存默认禁止 | 允许 | 负数量下均价除法失去意义 | 可逆（仓库开关） |
| D9 | 不动全局主题，莫兰迪色作语义色 | 全站改主色为莫兰迪 | 避免 12+ 既有页面回归风险；实测三色不满足 AA 白字对比 | 可逆 |
| D10 | PG 单语句原子取号 | Redis 自增 / 单独序列表 | 无重号保证简单可靠；单据量低无瓶颈 | 可逆 |
| D11 | 红冲用"借贷互换"而非负数 | 负数金额 | 保持单边恒正，报表口径统一，CHECK 约束可严格 | 可逆（但影响历史凭证口径，需谨慎） |
| D12 | BOM 不预留字段 | material 上加 bom 相关列 | 猜测式预留会留下无用字段；BOM 核心在关系表 | 可逆（批次 3 新建表） |

### 9.4 待确认问题（需老大 / 客户财务拍板，不影响开工）

| # | 问题 | 影响 | 我的建议默认值 |
|---|------|------|---------------|
| Q1 | 会计科目表用**《小企业会计准则》**还是《企业会计准则》？ | 种子科目不同（企业准则科目更多、明细更细） | 小企业准则（本批已按此种子） |
| Q2 | 暂估科目命名：`2202.02 应付账款—暂估应付款` 是否合适？ | 影响批次 2 冲回路径 | 沿用（中国实务通用叫法） |
| Q3 | 是否需要**批次号/序列号**管理？（食品、电子行业常见） | 需要则会改变 `biz_inventory` 唯一键 → **结构性变更** | 本批不做（除非明确要求） |
| Q4 | 是否已有**历史期初库存 / 应付余额**需导入？ | 影响上线方式（手工录入 vs 导入工具） | 本批手工录入，导入留批次 2 |
| Q5 | 采购审批是否确认**只留状态不做流程**？ | 若需要真实审批流，工期 +2 周 | 按任务书：只留状态 |
| Q6 | 是否需要**多币种**（进口采购）？ | 需要则要加汇率表与折算逻辑 | 本批只存 `currency_code`，全 CNY |
| Q7 | 入库单是否允许**无源入库**（不关联采购订单）？ | 影响 `ck_biz_si_supplier` 与 UI | 允许（`source_order_id` 可空，但采购入库仍要求供应商） |
| Q8 | 会计**记账本位币**与**纳税主体**是否单一套账？ | 多套账需要 `book_id` 维度 | 单套账 |

### 9.5 交付物与自检对照

**交付物**：本文档（`/root/.hermes/workspace/architect/cloudpivot-erp-batch1-architecture.md`）。
**无代码交付**：本任务仅设计；§9.1 的 POC 脚本在临时目录，属验证手段，不进入项目仓库（已在文中标注）。

**任务书要求 → 本文档覆盖位置**

| 任务书章节 | 覆盖位置 | 状态 |
|-----------|---------|------|
| 一、数据模型（完整 DDL，V0030 起） | §2.1–§2.9（20 张表，逐列含类型/默认值/可空/索引/外键/注释/软删）+ §2.10 迁移清单 | ✅ |
| `biz_material` / `biz_unit` / `biz_supplier` / `biz_warehouse` / `biz_account` | §2.1.1–§2.1.5 | ✅ |
| `biz_purchase_order(_item)` / `biz_stock_in(_item)` / `biz_inventory` / `biz_inventory_txn` | §2.2、§2.3 | ✅ |
| `biz_payable` / `biz_voucher(_entry)` / `biz_account_balance` | §2.4、§2.5 | ✅ |
| `biz_seq` + 操作日志结论 | §2.1.7、§2.8（**明确结论：双写**） | ✅ |
| 2.1 凭证引擎：规则配置模型 | §2.6（规则表）+ §3.1.2（聚合粒度/条件/优先级） | ✅ |
| 2.1 变量占位符 / 科目映射策略 | §3.1.3 / §3.1.4（四级解析链，含方案对比） | ✅ |
| 2.1 借贷平衡 / 期间控制 / 生成时机 / 手工调整与冲销 | §3.1.5 / §3.1.7 与 §3.3 / §3.1.7（4 方案对比+推荐） / §3.1.8（2 方案对比+推荐） | ✅ |
| 2.2 移动加权算法 / 并发安全 / 精度尾差 / 负库存 | §3.2.1（含 POC 实测数字）/ §3.2.2（5 方案对比）/ §3.2.3 / §3.2.4 | ✅ |
| 2.3 期间表设计 + 本批做到什么程度 | §2.1.6 + §3.3（功能边界表格） | ✅ |
| 2.4 状态机（部分收货/超收）/ 事务边界与回滚 / 作废红冲连锁 | §3.4.1 / §3.4.2（12 步 + 回滚矩阵）/ §3.4.3（7 项准入 + 9 步连锁） | ✅ |
| 三、API 契约（含权限码清单、分页字段） | §4.1–§4.9（58 个权限点 + 菜单结构） | ✅ |
| 四、前端架构（页面/复杂表格/格式化/配色） | §5.1–§5.7（含 WCAG 实测对比度） | ✅ |
| 五、扩展性（销售/生产复用、BOM、调拨） | §6（复用接口签名 + BOM 明确不预留 + 调拨无需改表） | ✅ |
| 六、实施计划（6 步） | §7（41 人日 / 两种排期 / 里程碑检查点） | ✅ |
| 七、风险清单（会计/并发/框架/工期） | §8（21 条风险 + 降级顺序） | ✅ |
| 硬约束：只做设计不写业务代码 | 全文仅 DDL / 接口契约 / 关键 SQL / 签名，无实现代码 | ✅ |
| 硬约束：符合基本会计准则 | §2.5（借贷平衡/期间/余额方向）、§3.1（引擎）、§3.3（期间） | ✅ |
| 硬约束：每个关键决策给对比 + 推荐 | D1–D12 + §3.1.4 / §3.1.7 / §3.1.8 / §3.2.2 / §5.4 / §6.3 / §6.4 | ✅ |
| 硬约束：第三方库评估 | §9.2（License/包体/维护/冲突，含实测包体 168KB） | ✅ |
| 硬约束：能在 Rust + nik 0.4.2 落地 | §1.3（框架能力实测表）、§1.4（Decimal 落地路径）、§9.1 POC-6（依赖解析实测） | ✅ |
| 附加：设计可执行性 | §9.1.8（DDL 在 PostgreSQL 18.6 实跑 + 12 组约束验证，含发现的 1 个缺陷已修正） | ✅ |
| 红线：只进 `business/` / 不动 system/support | §3.0 模块结构（全部在 `business/erp/`）、§1.5（因此不改 middleware） | ✅ |
| 红线：不设 service 层 | §3.0（engine 归属论证，四项判据） | ✅ |
| 红线：错误三出口 / 禁 unwrap 拼 SQL | §1.3、§3.4.2 回滚矩阵、§4.1 错误码 | ✅ |
| 红线：宏路径不含 `/api`，前端写全 | §4.1、§5.1 | ✅ |
| 红线：权限码三方一致 | §4.9（58 点清单） | ✅ |
| 红线：迁移 V0030 起、带单测、cargo test 全绿 | §2.10、§7.3（里程碑检查点） | ✅ |
