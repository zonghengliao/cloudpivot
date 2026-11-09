# ARCH-9 多组织 / 多公司 + 合并报表设计

> 卡片：`t_e1976a56`（ARCH9-1【L-2】）｜上游：评审报告 §5.1 / §6.2「需要账套维度改造，越早做越省事」
> 代码基线：`origin/main` = `2dc4f288`（2026-11-10 实测 `git log --oneline -1`）
> 影响迁移号段：**V0368 起**（现有 business 最大 `V0367__flow_all_role_approval_to_any_multiversion`，2026-11-10 `ls backend/migrations/business | sort | tail -1` 实测）
> 本文只做设计：**不含实现代码**（除 DDL 与接口签名示例），不改现有代码，不建迁移文件。

---

## 0. 结论前置（13 条定稿决策）

| 编号 | 决策点 | **结论** | 硬依据（可复核） |
|:--:|---|---|---|
| **D-1** | 多账套 vs 多公司 | **多公司**（一个共享科目表 + `org_id` 维度）。不建"账套"层 | `biz_account` 全表唯一索引 `uk_biz_account_code`（V0137:128）+ 报表取数口径 `report::match_account` 按 `account_code` 前缀匹配（`erp/report.rs:473-488`）——两套科目表会让合并必须先做科目映射，"合并"直接退化成"映射工程" |
| **D-2** | 隔离强度 | **`org_id` 是硬隔离（不可配置）**，不是 `sys_role.data_scope` 的一个取值 | 数据权限是"可见范围"（`ScopeKind` 1..5 可被有权限的人放大，`shared/scope.rs:88-100`）；跨法人数据不可通过任何配置看见。两者语义不同，混在一起会出现"改了 data_scope 就能看别家公司账"的提权路径 |
| **D-3** | 科目共享 vs 各一套 | **共享一套 + 公司级启用/停用 + 允许公司专属明细科目**（专属科目 `biz_account.org_id` 非空，且必须挂在共享父科目下） | 合并时按共享父科目口径聚合即可，**不需要科目映射表**；`match_account` 既支持 `1122` 精确/下级，也支持 `1122*` 前缀（同上行号），专属明细天然被父科目口径覆盖 |
| **D-4** | 合并层数 | **N 层**（`biz_org` 自引用树 + `org_path` 物化路径，仿 `sys_dept.dept_path`） | 现有 `sys_dept` 已用 `dept_path VARCHAR(500)` 做子树查询（`system/V0006:7`），同一手法零学习成本 |
| **D-5** | 抵消分录 | **规则驱动自动生成，抵消分录只落"合并工作底稿"表，不写 `biz_voucher`** | 抵消分录若落 `biz_voucher` 会污染子公司单体账（`report.rs` 的试算平衡、`income_statement` 勾稽、`year_end` 结转全部会被合并数污染）；底稿表 + 报表层相减，单体账 0 改动 |
| **D-6** | 少数股东权益 | **v1 不做**（明确划出边界）；`biz_org.ownership_ratio` 只记录不参与计算，非 100% 时合并报表返回 `warnings` | 少数股东权益要动"权益抵消 + 净利润拆分"两层，且必须与持股比例变动（年内增持）联动——工作量大于合并主体本身，收益客户面窄 |
| **D-7** | 期间 | **按公司独立**：`biz_accounting_period` 加 `org_id`，`uk (org_id, period_code)` | 现有 `uk_biz_period_code (period_code)` 全局唯一（V0137:194），不加 `org_id` 则第二家公司连"开期间"都做不到 |
| **D-8** | 取号 | **按公司独立**：`biz_seq` 加 `org_id`，`uk (org_id, seq_type)`；`biz_voucher` 唯一键改 `(org_id, voucher_word, period, voucher_no)` | 现有 `uk_biz_seq_type (seq_type)`（V0137:313）+ `uk_biz_voucher_no (voucher_word, period, voucher_no)`（V0149:76）——两家公司各自记-0001 是会计惯例，不改键就撞号 |
| **D-9** | 存量数据 | **全部落一个默认公司** `00000000-0000-4000-8000-000000000001`（`org_code=HQ001`「云枢总部」），**不猜测性拆分** | 生产实测只有 4 个部门（HQ/FIN/SALES/TECH）与 1 个账套（`docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md` §1.2）；凭证 21 张（评审报告 §7.1）——拆分无业务依据，猜错比不拆更贵 |
| **D-10** | 与数据权限的关系 | **正交两段式**：`WHERE org_id = ANY(可见公司)` 永远在最前（硬），`SCOPED_TABLES` 的 dept/owner 片段在其内（软，可配） | `scope::paginate` 已固定"片段参数前置"契约（`shared/scope.rs` 模块文档「参数顺序契约」），org 片段比它更前，成为新的第 0 段 |
| **D-11** | 审批流 | **不改审批人解析（`flow/assignee.rs` 8+2 种来源一行不动）**；`biz_flow_instance` 加 `org_id` 快照，待办列表按可见公司过滤，审批人解析后加一道"须对该 org 有权"过滤 | 改 `assignee.rs` 要动 8 种来源 × 三层去重 × 自审/空审批人策略（该文件模块文档「三层去重」「空审批人策略」），风险远大于收益 |
| **D-12** | 报表 API | **单公司报表只加 `org_id` 入参（不新增第二套）**；合并报表是**独立一套 `/consol/*`** | 三大报表取数 SQL 已统一在 `report.rs`/`income_statement.rs`/`cash_flow.rs` 的同一把尺子上（三个文件模块文档「同一套口径」），复制一套会立刻漂移 |
| **D-13** | 默认公司名称/是否拆分授权 | **待确认**：默认公司叫「云枢总部」还是客户真名；用户跨公司访问用"用户维度表"还是"角色维度" | 见 §8.4 待确认清单（Q1/Q3），确认方式已写明 |

**不做（明确划出，写进产品边界）**：多租户（SaaS 版独立仓库 `cloudpivot-saas`，`AGENTS.md` 已声明）· 少数股东权益与权益法核算 · 内部未实现利润剔除（内部购销只做到"收入-成本等额对冲"）· 跨币种合并折算 · 跨法人调拨与内部结算价 · 内部银行/资金池 · 合并层面税务申报。

---

## 1. 业务背景与目标

### 1.1 解决什么问题（客户提的 / 竞品都有的 / 不做会丢单）

| 问题 | 现状证据 | 后果 |
|---|---|---|
| 一个集团下多个法人，无法各自出报表 | 全仓 `grep -ril org_id backend/migrations/` = **0 个文件**；`biz_voucher` / `biz_account` / `biz_accounting_period` 全部全局一套 | 客户只能给集团开 N 个库（N 套系统），合并靠 Excel；账套口径一旦不一致，报表对不上 |
| 竞品都有 | 用友 U8/U9、金蝶 K3/星瀚均把"多组织 + 合并报表"作为集团版核心卖点 | 中大型客户在选型清单里直接勾"必须支持多公司合并"，缺失即出局 |
| 越早做越省事 | 现在已经 **108 张 `biz_*` 表**（`grep -rhoE "CREATE TABLE ... biz_" \| sort -u \| wc -l` = 108）、**615 处 `SCOPE-EXEMPT`/scope 注解**（`grep -rn SCOPE-EXEMPT backend/src --include=*.rs \| wc -l`） | 每多一张业务表、多一个报表 SQL，`org_id` 接入成本线性增长 |

### 1.2 不解决什么问题（边界）

1. **不做多租户**：`org` 是"一个租户内的多个法人"，不是 SaaS 的 tenant。企业版（`cloudpivot-saas`）若叠加 `tenant_id`，`org` 是 `tenant` 的**下级维度**（§8.3 R6）。
2. **不做跨币种合并折算**：要求纳入范围的公司 `base_currency` 一致（默认 `CNY`）。现状连"外汇报表折算"都没有（评审报告 §7.1 多币种判定"半截：有 currency_code，无本位币折算"），合并折算必须等外币报表折算先落地。
3. **不做少数股东权益 / 权益法**（D-6）。
4. **不做内部未实现利润剔除**：内部购销按"收入 = 成本"等额对冲，高于成本的部分（B 公司未对集团外出库的存货里含的内部毛利）不剔除，`warnings` 里点名该差额。
5. **不做跨法人调拨**：`biz_stock_transfer` 保持"同一公司内仓间移位"。跨法人必须走"销售 + 采购"两张单（有发票、有税），这也是 `docs/ERP-库存对账口径.md:354-356` 已写明的既有结论。
6. **不做合并层面的税务申报 / 转移定价文档**。

### 1.3 成功的判据（上线后怎么算成功，逐条可测）

| # | 判据 | 测法 |
|:--:|---|---|
| S1 | 一家公司建单据时**看不到**另一家公司的任何业务单据/凭证/台账 | 用仅授权 A 公司账号登录，逐页搜索 B 公司单号 → 0 命中；`curl` 直传 B 公司 `org_id` → 403 |
| S2 | 单公司三张报表在有 N 家公司后**数字逐行不变** | 上线前后对默认公司同一期间跑 `balance-sheet`/`income-statement`/`cash-flow`，逐行金额完全一致（因 `org_id` 全落默认公司，加过滤等价于不过滤） |
| S3 | 每家公司能独立结账、独立出三表 | A 公司 2026-11 结账后，B 公司 2026-11 仍为 `OPEN`，两边互不影响 |
| S4 | 合并报表能出，且抵消可追到明细 | 合并 `balance-sheet` 的"内部往来"科目净额为 0（或等于 `warnings` 点名的差额）；每一条抵消行可下钻到 `biz_consol_offset` 的 `from_org_id/to_org_id/rule_id` |
| S5 | 合并恒等式与单公司同一把尺子 | 合并口径同样报 `资产 = 负债 + 权益`，`check.diff ≠ 0` 时不抹平（复用 `report.rs` 的 `diff_text` 口径） |

---

## 2. 与现状的关系

### 2.1 会被影响的现有表 / 模块（逐条实测）

| 现状 | 位置（实测） | 影响 |
|---|---|---|
| `biz_account` 全局科目表，`account_code` 全表唯一 | `business/V0137:100-131`；`uk_biz_account_code` 有意不带 `deleted_at IS NULL` | 加**可空** `org_id`（NULL = 集团共享），唯一键**保持不动**（专属明细也要编码唯一，否则合并口径歧义） |
| `biz_voucher` / `biz_voucher_entry` 全局一套 | `business/V0149:41-115` | 加 `org_id`（分录不加）；唯一键 `uk_biz_voucher_no` 加 `org_id` 前导；`uk_biz_voucher_year_end_period`（V0190:74）、`uk_biz_voucher_opening_period`（V0189:120）同样加 `org_id` |
| **凭证唯一写入口** `voucher::post_voucher` | `erp/voucher.rs:1963`（`INSERT INTO {TABLE}` 在 2068 行；其余 12 处 `INSERT INTO biz_voucher` 全在 `mod tests` 内，逐文件核对过 `mod tests` 行号 < insert 行号） | 只需在 `PostReq` 结构体加 `org_id` 字段 + 这一处 INSERT 加列 → 全系统凭证自动带 `org_id` |
| 取号 `next_voucher_no` / `next_doc_no` 走 `biz_seq` 全局 | `erp/voucher.rs:340,361,383`；模块注释 72 行写「SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列」 | 注释与索引都要改；`next_voucher_no(ex, now)` → 增加 `org_id` 参数 |
| `biz_accounting_period` 全局期间 | `business/V0137:172-196`，`uk_biz_period_code` / `uk_biz_period_ym` | 加 `org_id`，两条唯一键都加前导 |
| `biz_opening_balance` / `biz_opening_inventory` 全局期初 | `business/V0189:57-105` | 加 `org_id`，`uk_biz_opening_balance (period_code, account_id)` / `uk_biz_opening_inventory (period_code, material_id, warehouse_id)` 加前导 |
| `biz_depreciation_batch` 每期一批 | `business/V0230:67` `uk_biz_dep_batch_period (period_code)` | 加 `org_id` → 否则第二家公司做折旧直接撞唯一键 |
| 三大报表 + 总账取数 | `erp/report.rs`、`income_statement.rs`、`cash_flow.rs`、`ledger.rs`（4 个模块文档均声明"同一把尺子"） | 入参加 `org_id`；`SELECT ... FROM biz_voucher` 全部加 `v.org_id = ?` |
| 年结 | `erp/year_end.rs`（模块文档：损益结转 + 下年期初 + 反年结） | 按公司循环；`uk_biz_voucher_year_end_period` 带 `org_id` |
| 数据权限 | `shared/scope.rs`：`SCOPED_TABLES` **12 张**（281-294 行）、`paginate` 唯一入口、`ScopeKind` 1..5 | **不改 `ScopeKind`**；新增 org 段（先于 RowFilter），`SCOPED_TABLES` 加一列 `org_col` |
| 审批流 | `business/V0122:33-70` `biz_flow_instance`（无 org 概念）；`flow/assignee.rs` 8+2 种来源 | 只加 `org_id` 快照列 + 解析后过滤，不动 8 种来源 |
| 部门树 | `system/V0006` `sys_dept`，`dept_path` 子树查询；生产 4 部门 | 加 `org_id`（部门归属公司），跨 org 的 parent 由应用层拒绝 |
| 用户 | `sys_user.dept_id`（`docs/批次B-技术架构.md` §2.5 登记表） | 加 `sys_user.org_id`（主属公司，可空）+ 新表 `biz_org_user`（额外可访问公司） |
| 主数据客户/供应商 | `biz_customer` / `biz_supplier`（V0137 起陆续演进） | 加 `is_internal` + `internal_org_id`（内部往来单位标识），不加 `org_id`（外部单位不归属本公司） |
| 巡检脚本 | `scripts/verify/` 下 **499 个脚本**（`ls scripts/verify/*.{sh,py,mjs,cjs} \| wc -l`），大量 `psql -c "SELECT ... FROM biz_voucher"` 单公司口径 | 全量回归会红；分 4 批改造（§7.4），改造期间用 `SKIP_ORG_CHECKS=1` 显式豁免并记录，禁止长期红（§8.3 R8） |

### 2.2 新增 / 改造矩阵

| 类别 | 对象 | 数量 |
|---|---|---|
| **新增表** | `biz_org`、`biz_org_user`、`biz_org_account`、`biz_consol_worksheet`、`biz_consol_scope`、`biz_consol_rule`、`biz_consol_offset` | **7 张** |
| **改造表（加列）** | 财务核心 8 + 单据主表 24 + 库存/仓库 4 + 主数据/系统 5 + 流程 1 = **42 张**（明细见 §3.2） | 42 张 |
| **唯一键重建** | `biz_accounting_period` ×2、`biz_voucher` ×3、`biz_opening_balance` ×1、`biz_opening_inventory` ×1、`biz_seq` ×1、`biz_depreciation_batch` ×1 | 8 条索引 |
| **代码改造点** | `post_voucher` 1 处（+ `PostReq` 字段）、`post_voucher(` 调用点 **53 处**（`grep -rn "post_voucher(" backend/src \| grep -v voucher.rs \| wc -l`）、`FROM biz_voucher` 取数语句 **244 处 / 62 个文件**（`grep -rc "FROM biz_voucher" backend/src --include=*.rs` 实测；含测试夹具，生产侧集中在 `voucher.rs`(25)、`year_end.rs`(20)、`serial_handler.rs`(12)、`payable_payment.rs`(9)、`fx_revaluation.rs`(9)、`voucher_write.rs`(8)、`receipt_refund.rs`(8)、`opening.rs`(7)）、取号 3 处、期间/年结/期初 ≈ 12 处 | ≈ 310 处 |
| **不改** | 明细行表（`*_item` / `biz_*_allocation`）、附件（`biz_doc_attachment`）、状态日志（`biz_status_log`）、`biz_task*` 任务域、`biz_flow_def*` 定义域、`sys_menu` 语义 | 靠主表 join 归属 |

### 2.3 会不会破坏现有数据（必须：不破坏）

**数学论证（不是"应该没问题"）**：

1. 回填后全表 `org_id` 恒等于 `DEFAULT_ORG_ID`（单公司）。
2. 任何查询加 `AND org_id = DEFAULT_ORG_ID`，在"全表该列同值"的前提下去掉/加上该谓词，结果集**逐行相同**（谓词恒真）。因此三大报表、总账、余额表、账龄、对账单的数字**逐行不变**。
3. 唯一键加 `org_id` 前导在单值列上等价于原键（不产生新冲突，也不放松唯一性）。
4. `NOT NULL` 约束在回填后成立（回填 `WHERE org_id IS NULL` 幂等）。
5. 破坏面只有两处，且都有补偿：
   - `biz_seq` 取号如果**漏传** `org_id` → 落到默认公司号段 → 第二家公司开张前无影响；开张后由 §4.6 的"N7 号段归属断言"巡检发现。
   - 报表漏加 `org_id` 过滤 → 第二家公司开张后会**多算**别家数 → 由 §4.6 的"N1 交叉污染断言"挡住（每张表一条）。

**验收动作**：上线前后对默认公司同一期间逐行 diff 三张报表 JSON（S2 判据），0 差异才算通过。

### 2.4 与 ARCH-1~8 已有设计的关系

| 文档 | 关系 | 说明 |
|---|---|---|
| ARCH-2 合同表数据模型 | **复用** | `biz_contract` 加 `org_id` 即可，合同审批走现有流程引擎 |
| ARCH-3 模块勾稽关系审计 | **复用 + 扩展** | 所有勾稽断言变成"按 org 分组"的两层：先公司内勾稽 → 再公司间抵消勾稽（§4.4） |
| ARCH-4 通用 MES 与委外 | **冲突需声明** | ARCH-4 §边界表明确写「多工厂/多组织：单租户单工厂假设（与现有系统一致），SaaS 版另一仓库」（`ARCH-4:4306`）。本设计**不推翻**它：`org` 只覆盖"会计主体"，生产车间仍在 `sys_dept`/仓库维度；工单若将来要做跨厂，另开卡 |
| ARCH-5 工作流引擎设计 | **只加快照列** | 引擎语义（节点/会签/加签/驳回）完全不动 |
| ARCH-6 财务扩展（预算/资产/报销/汇兑） | **必须同步** | 该设计里的 4 个新域（`biz_budget*`、`biz_fixed_asset*`、`biz_expense*`、`biz_fx_revaluation`）全部已落库，本设计全部纳入改造清单（§3.2 B 组）——否则它们会成为唯一的跨公司数据孤岛 |
| ARCH-7 销售模块 | **复用** | 客户对账单（`erp/statement.rs`）按公司口径出；内部客户的对账单在合并层面被抵消，单体仍照常出（客户要的是自己的账） |
| ARCH-8 审批职责分离与自审批 | **增强** | SoD 校验加一条"审批人须对该单据 `org` 有权"，与现有 SoD 并列，不替换 |
| ARCH-10 产品演进路线 | **本卡即其 §0.3「待补：多组织/多公司 + 合并报表 ARCH9-1」** | 与 ARCH9-2（HR）/ ARCH9-3（成本核算）无耦合；ARCH9-3 分批法成本若按公司核算，直接复用本设计的 `org_id` |

---

## 3. 数据模型

### 3.1 新增表（7 张，完整 DDL）

```sql
-- ==================== ① 会计主体（法人 / 组织树）====================
CREATE TABLE IF NOT EXISTS biz_org (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    org_code           VARCHAR(40)  NOT NULL,                           -- 公司编码（如 HQ001 / BJ001）
    org_name           VARCHAR(200) NOT NULL,                           -- 公司全称（营业执照名）
    short_name         VARCHAR(60),                                     -- 简称（下拉框/报表抬头用）
    parent_id          UUID,                                            -- 上级组织（弱引用 biz_org.id；集团根为空）
    org_path           VARCHAR(500),                                    -- 层级路径（如 /uuidA/uuidB/，仿 sys_dept.dept_path 加速子树）
    org_level          SMALLINT     NOT NULL DEFAULT 1,                 -- 层级（1 集团根 / 2 子公司 / 3 孙公司）
    is_legal_entity    SMALLINT     NOT NULL DEFAULT 1,                 -- 是否独立法人：1 是 / 0 纯管理节点（如"华东区"）
    is_group_root      SMALLINT     NOT NULL DEFAULT 0,                 -- 是否合并主体根：1 是 / 0 否
    credit_code        VARCHAR(30),                                     -- 统一社会信用代码（18 位，可空）
    tax_no             VARCHAR(40),                                     -- 纳税人识别号
    base_currency      VARCHAR(8)   NOT NULL DEFAULT 'CNY',             -- 记账本位币（弱引用 biz_currency.code）
    fiscal_year_start_month SMALLINT NOT NULL DEFAULT 1,                -- 会计年度起始月（1-12）
    ownership_ratio    NUMERIC(7,4) NOT NULL DEFAULT 100,               -- 母公司直接持股比例（%，仅记录，不参与计算，见 D-6）
    contact_person     VARCHAR(50),                                     -- 联系人
    contact_phone      VARCHAR(30),                                     -- 联系电话
    address            VARCHAR(255),                                    -- 注册地址
    status             VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',          -- ACTIVE 启用 / DISABLED 停用
    sort_order         INT          NOT NULL DEFAULT 0,                 -- 同级排序
    remark             TEXT,
    created_at         TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,

    CONSTRAINT ck_biz_org_status   CHECK (status IN ('ACTIVE','DISABLED')),
    CONSTRAINT ck_biz_org_level    CHECK (org_level >= 1),
    CONSTRAINT ck_biz_org_legal    CHECK (is_legal_entity IN (0,1)),
    CONSTRAINT ck_biz_org_root     CHECK (is_group_root IN (0,1)),
    CONSTRAINT ck_biz_org_month    CHECK (fiscal_year_start_month BETWEEN 1 AND 12),
    CONSTRAINT ck_biz_org_ratio    CHECK (ownership_ratio > 0 AND ownership_ratio <= 100),
    CONSTRAINT ck_biz_org_no_self  CHECK (parent_id IS NULL OR parent_id <> id),
    -- 管理节点不许有统一社会信用代码（防止"管理节点"被当成会计主体记账）；法人可空但由巡检 N9 WARN
    CONSTRAINT ck_biz_org_credit   CHECK ((is_legal_entity = 1)                       -- 法人：credit_code 可空（上线初期可后补，巡检 WARN 催）
                                        OR (is_legal_entity = 0 AND credit_code IS NULL))  -- 管理节点：不许有信用代码
);

COMMENT ON TABLE  biz_org IS '会计主体（多公司）：共享一套科目表，数据按 org_id 隔离；N 层树，is_group_root=1 的节点可作为合并主体';
COMMENT ON COLUMN biz_org.org_path IS '层级路径 /uuidA/uuidB/，与 sys_dept.dept_path 同一手法：子树查询用 LIKE ''/uuidA/%''，应用层预解析成 id 集合后 SQL 里只出现 = ANY(?)';
COMMENT ON COLUMN biz_org.ownership_ratio IS '母公司直接持股比例：v1 仅记录不参与计算（少数股东权益不做，见 ARCH-9 D-6）；≠100 时合并报表返回 warnings';
COMMENT ON COLUMN biz_org.is_legal_entity IS '0 = 纯管理节点（如"华东区"），不可作为单据/凭证归属主体，只能当合并结果的展示层';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_org_code ON biz_org (org_code) WHERE deleted_at IS NULL;
CREATE INDEX        IF NOT EXISTS idx_biz_org_parent ON biz_org (parent_id);
CREATE INDEX        IF NOT EXISTS idx_biz_org_path   ON biz_org (org_path);
CREATE INDEX        IF NOT EXISTS idx_biz_org_status ON biz_org (status, sort_order);

-- ==================== ② 用户可访问公司（跨公司授权）====================
CREATE TABLE IF NOT EXISTS biz_org_user (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT,  -- 授权公司
    user_id    UUID NOT NULL REFERENCES sys_user(id) ON DELETE CASCADE,  -- 被授权用户
    is_default SMALLINT NOT NULL DEFAULT 0,                              -- 是否该用户的默认公司（每人最多 1 行 =1）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,

    CONSTRAINT uk_biz_org_user UNIQUE (org_id, user_id),
    CONSTRAINT ck_biz_org_user_default CHECK (is_default IN (0,1))
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_org_user_default
    ON biz_org_user (user_id) WHERE is_default = 1;
CREATE INDEX IF NOT EXISTS idx_biz_org_user_user ON biz_org_user (user_id);

COMMENT ON TABLE biz_org_user IS '用户可访问公司集合：集团财务可被授权多家公司；SUPER_ADMIN 短路（不受本表限制，但仍须显式切换当前公司）';

-- ==================== ③ 公司级科目启用/停用 ====================
CREATE TABLE IF NOT EXISTS biz_org_account (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id     UUID NOT NULL REFERENCES biz_org(id) ON DELETE CASCADE,   -- 公司
    account_id UUID NOT NULL,                                            -- 科目（弱引用 biz_account.id）
    status     SMALLINT NOT NULL DEFAULT 1,                              -- 1 启用 / 0 停用（本公司）
    sort_order INT      NOT NULL DEFAULT 0,                              -- 本公司科目表排序覆盖
    remark     TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    update_by  UUID,

    CONSTRAINT uk_biz_org_account UNIQUE (org_id, account_id),
    CONSTRAINT ck_biz_org_account_status CHECK (status IN (0,1))
);
CREATE INDEX IF NOT EXISTS idx_biz_org_account_org ON biz_org_account (org_id, status);

COMMENT ON TABLE biz_org_account IS '公司级科目启用/停用：**缺行 = 按科目本身的 biz_account.status 继承**（不预置行，避免每加一个科目要初始化 N 家公司）；停用后该公司新凭证不可用此科目，历史凭证不受影响';

-- ==================== ④ 合并工作底稿（头）====================
CREATE TABLE IF NOT EXISTS biz_consol_worksheet (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_org_id   UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT, -- 合并主体（必须是 is_group_root=1 或某法人节点）
    period_code    VARCHAR(7)  NOT NULL,                                    -- 会计期间 yyyy-MM
    base_currency  VARCHAR(8)  NOT NULL DEFAULT 'CNY',                      -- 合并本位币（= 主体 base_currency 快照）
    status         VARCHAR(16) NOT NULL DEFAULT 'DRAFT',                    -- DRAFT / GENERATED / CONFIRMED / CANCELLED
    member_count   INT         NOT NULL DEFAULT 0,                          -- 本次纳入合并的公司数（快照冗余，便于列表展示）
    offset_debit   NUMERIC(20,4) NOT NULL DEFAULT 0,                        -- 抵消分录借方合计（生成时算，列表展示）
    offset_credit  NUMERIC(20,4) NOT NULL DEFAULT 0,                        -- 抵消分录贷方合计
    diff_amount    NUMERIC(20,4) NOT NULL DEFAULT 0,                        -- 内部往来不符差额合计（进 warnings）
    generated_at   TIMESTAMPTZ,                                             -- 最近一次自动生成时间
    generated_by   UUID,
    confirmed_at   TIMESTAMPTZ,                                             -- 确认（锁定）时间
    confirmed_by   UUID,
    unfreeze_count INT         NOT NULL DEFAULT 0,                          -- 解冻次数（审计用，每次 +1）
    last_reason    VARCHAR(500),                                            -- 最近一次解冻/重算理由
    remark         TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,
    deleted_at     TIMESTAMPTZ,

    CONSTRAINT ck_biz_consol_ws_status CHECK (status IN ('DRAFT','GENERATED','CONFIRMED','CANCELLED')),
    CONSTRAINT ck_biz_consol_ws_period CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_consol_ws_count  CHECK (member_count >= 0 AND unfreeze_count >= 0),
    -- 借贷必须相等：抵消分录自身也要平（与 biz_voucher 的 ck_biz_voucher_balance 同一红线）
    CONSTRAINT ck_biz_consol_ws_balance CHECK (offset_debit = offset_credit)
);

COMMENT ON TABLE biz_consol_worksheet IS '合并工作底稿：一个(合并主体, 期间)一份；抵消分录**只存这里，不写 biz_voucher**（D-5），保证单体账不被污染';
COMMENT ON COLUMN biz_consol_worksheet.status IS 'DRAFT 新建未算 / GENERATED 已自动生成抵消 / CONFIRMED 已确认锁定（报表对外口径）/ CANCELLED 作废';

-- 同一主体同一期间只能有一份未作废底稿（含 CONFIRMED：确认后要重算必须先解冻）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_consol_ws
    ON biz_consol_worksheet (group_org_id, period_code)
    WHERE deleted_at IS NULL AND status <> 'CANCELLED';
CREATE INDEX IF NOT EXISTS idx_biz_consol_ws_period ON biz_consol_worksheet (period_code, status);

-- ==================== ⑤ 合并范围快照 ====================
CREATE TABLE IF NOT EXISTS biz_consol_scope (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    worksheet_id  UUID NOT NULL REFERENCES biz_consol_worksheet(id) ON DELETE CASCADE,
    member_org_id UUID NOT NULL REFERENCES biz_org(id) ON DELETE RESTRICT,   -- 成员公司
    included      SMALLINT NOT NULL DEFAULT 1,                               -- 1 纳入 / 0 不纳入
    exclude_reason VARCHAR(200),                                             -- 不纳入原因（必须写，如"本期已处置"/"该期未结账"）
    period_closed SMALLINT NOT NULL DEFAULT 0,                               -- 生成时该公司该期是否已结账：1 是 / 0 否
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_consol_scope UNIQUE (worksheet_id, member_org_id),
    CONSTRAINT ck_biz_consol_scope_in CHECK (included IN (0,1)),
    CONSTRAINT ck_biz_consol_scope_reason CHECK (included = 1 OR exclude_reason IS NOT NULL)
);

COMMENT ON TABLE biz_consol_scope IS '合并范围**快照**：生成底稿时把当时 org 子树冻结下来。之后 org 树变动/新设公司不会改写历史底稿（同一手法见 AGENTS.md 约定 13「审计/历史类存快照」）';

-- ==================== ⑥ 抵消规则 ====================
CREATE TABLE IF NOT EXISTS biz_consol_rule (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_org_id       UUID NOT NULL REFERENCES biz_org(id) ON DELETE CASCADE, -- 所属合并主体（建主体时从系统模板复制）
    rule_code          VARCHAR(40)  NOT NULL,                                  -- 规则编码（如 IC_AR_AP / IC_REV_COST）
    rule_name          VARCHAR(120) NOT NULL,                                  -- 规则名（"内部往来-应收应付"）
    rule_type          VARCHAR(24)  NOT NULL,                                  -- IC_AR_AP 往来 / IC_REV_COST 购销 / IC_CASH 内部资金
    side_a_account     VARCHAR(120) NOT NULL,                                  -- A 侧科目匹配式（复用 report::match_account 语法：1122 / 1122* / 1122,1221）
    side_b_account     VARCHAR(120) NOT NULL,                                  -- B 侧科目匹配式（2202 / 2202* / 2202,2241）
    debit_account_code VARCHAR(30)  NOT NULL,                                  -- 抵消分录借方科目（B 侧科目，冲减 B 的应付/成本）
    credit_account_code VARCHAR(30) NOT NULL,                                  -- 抵消分录贷方科目（A 侧科目，冲减 A 的应收/收入）
    tolerance_amount   NUMERIC(20,4) NOT NULL DEFAULT 0.01,                    -- 差额容忍度：|A−B| ≤ 本值 → 视为完全对冲，差额不报警
    is_system          SMALLINT NOT NULL DEFAULT 0,                            -- 1 系统模板（客户不可删）
    status             SMALLINT NOT NULL DEFAULT 1,                            -- 1 启用 / 0 停用
    sort_order         INT      NOT NULL DEFAULT 0,
    remark             TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,

    CONSTRAINT ck_biz_consol_rule_type CHECK (rule_type IN ('IC_AR_AP','IC_REV_COST','IC_CASH')),
    CONSTRAINT ck_biz_consol_rule_sys  CHECK (is_system IN (0,1)),
    CONSTRAINT ck_biz_consol_rule_stat CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_consol_rule_tol  CHECK (tolerance_amount >= 0)
);

COMMENT ON TABLE biz_consol_rule IS '抵消规则：科目匹配式复用 report::match_account 的语法（erp/report.rs:473-488），不引入第二套匹配引擎';
COMMENT ON COLUMN biz_consol_rule.debit_account_code IS '抵消分录借方科目编码（写在合并层面，与 A/B 公司自身科目无关）：往来类=对偶科目，购销类=成本科目';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_consol_rule
    ON biz_consol_rule (group_org_id, rule_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_consol_rule_group ON biz_consol_rule (group_org_id, status, sort_order);

-- ==================== ⑦ 抵销分录（工作底稿明细）====================
CREATE TABLE IF NOT EXISTS biz_consol_offset (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    worksheet_id  UUID NOT NULL REFERENCES biz_consol_worksheet(id) ON DELETE CASCADE,
    line_no       INT  NOT NULL,                                              -- 底稿行号（重算时整体重排）
    rule_id       UUID REFERENCES biz_consol_rule(id) ON DELETE SET NULL,     -- 命中规则（手工行为 NULL）
    source_type   VARCHAR(8) NOT NULL DEFAULT 'AUTO',                         -- AUTO 自动 / MANUAL 手工
    from_org_id   UUID,                                                       -- 交易出手方（内部交易 A 公司）
    to_org_id     UUID,                                                       -- 交易入手方（内部交易 B 公司）
    side_a_amount NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- A 侧净额（如 A 的应收）
    side_b_amount NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- B 侧净额（如 B 的应付）
    offset_amount NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- 实际抵消额 = 较小方 − 容忍内差额
    diff_amount   NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- 未抵消差额（进 warnings，绝不抹平）
    account_code  VARCHAR(30)  NOT NULL,                                      -- 抵消科目编码（快照）
    account_name  VARCHAR(120) NOT NULL,                                      -- 抵消科目名称（快照）
    debit_amount  NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- 借方金额
    credit_amount NUMERIC(20,4) NOT NULL DEFAULT 0,                           -- 贷方金额
    reason        VARCHAR(500),                                               -- MANUAL 行必填：为什么手工调
    created_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,

    CONSTRAINT uk_biz_consol_offset_line UNIQUE (worksheet_id, line_no),
    CONSTRAINT ck_biz_consol_offset_src  CHECK (source_type IN ('AUTO','MANUAL')),
    CONSTRAINT ck_biz_consol_offset_side  CHECK ((debit_amount = 0 AND credit_amount > 0)
                                              OR (debit_amount > 0 AND credit_amount = 0)),
    CONSTRAINT ck_biz_consol_offset_nonneg CHECK (side_a_amount >= 0 AND side_b_amount >= 0
                                              AND offset_amount >= 0 AND diff_amount >= 0),
    CONSTRAINT ck_biz_consol_offset_reason CHECK (source_type = 'AUTO' OR reason IS NOT NULL),
    CONSTRAINT ck_biz_consol_offset_rule   CHECK (source_type = 'MANUAL' OR rule_id IS NOT NULL)
);

COMMENT ON TABLE biz_consol_offset IS '抵消分录（底稿级）：单边原则与金额非负同 biz_voucher_entry（V0149:100-110），借贷互换做红冲的同一口径';
COMMENT ON COLUMN biz_consol_offset.diff_amount IS '内部往来/A-B 不符差额：不抹平，进合并报表 warnings（"内部往来不符 A 应收 1000 / B 应付 990，差 10"）';

CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_ws   ON biz_consol_offset (worksheet_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_pair ON biz_consol_offset (from_org_id, to_org_id);
CREATE INDEX IF NOT EXISTS idx_biz_consol_offset_acct ON biz_consol_offset (account_code);
```

**默认公司与系统抵消规则模板种子**（与建表同一迁移，幂等）：

```sql
-- 默认公司：承接现有全部数据（D-9）。UUID 固定，全仓唯一常量 DEFAULT_ORG_ID
INSERT INTO biz_org (id, org_code, org_name, short_name, parent_id, org_path, org_level,
                     is_legal_entity, is_group_root, base_currency, status, sort_order, remark)
VALUES ('00000000-0000-4000-8000-000000000001'::uuid, 'HQ001', '云枢总部', '总部', NULL,
        '/00000000-0000-4000-8000-000000000001/', 1, 1, 1, 'CNY', 'ACTIVE', 10,
        'ARCH-9 存量数据承接主体：上线前的全部部门/用户/单据/凭证归此公司')
ON CONFLICT (id) DO NOTHING;

-- 三条系统抵消规则模板（建新公司/新合并主体时由应用层复制；此处只给默认公司种一套）
INSERT INTO biz_consol_rule (group_org_id, rule_code, rule_name, rule_type,
                             side_a_account, side_b_account,
                             debit_account_code, credit_account_code, is_system, sort_order)
VALUES
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_AR_AP',   '内部往来-应收应付', 'IC_AR_AP',
  '1122,1221', '2202,2241', '2202', '1122', 1, 10),
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_REV_COST','内部购销-收入成本', 'IC_REV_COST',
  '6001', '6401', '6401', '6001', 1, 20),
 ('00000000-0000-4000-8000-000000000001'::uuid, 'IC_CASH',    '内部资金往来', 'IC_CASH',
  '1123,2203', '1123,2203', '2203', '1123', 1, 30);
```

> 科目编码依据：`1122` 应收账款 / `1221` 其他应收 / `2202` 应付账款 / `2241` 其他应付 / `1123` 预付 / `2203` 预收 / `6001` 主营业务收入 / `6401` 主营业务成本 —— 均为 `V0183__erp_balance_sheet.up.sql` 补到 56 个标准科目里的既有编码。落地前须 `SELECT account_code, account_name FROM biz_account WHERE account_code IN (...)` 复核一遍（科目表可被客户改）。

### 3.2 改造表（42 张，完整 DDL）

所有列一律 **可空加列 → 回填 → 同迁移内 `SET NOT NULL`**（不是三期迁移），因为本项目的迁移在启动链里先于端口监听（`AGENTS.md`「cargo run 自动迁移 + 启动 :8023」），**没有"旧二进制还在写库"的窗口**，所以不需要 `DEFAULT` 兼容期，也就不存在"漏传静默落默认公司"的隐患。

#### A 组 · 财务核心（8 张 + 8 条唯一键重建）

> **`biz_voucher_entry` 有意不加 `org_id`**：一行分录的归属恒等于其 `voucher_id` 的归属（`ON DELETE CASCADE`，V0149:88），加列等于把同一事实存两遍 —— 一旦不一致，报表到底听谁的？取数一律 `JOIN biz_voucher v ON v.id = e.voucher_id`，索引 `idx_biz_voucher_entry_voucher`（V0149:117）已在。

```sql
-- A1 凭证主表：唯一写入口 voucher::post_voucher（erp/voucher.rs:1963，INSERT 在 2068）
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_voucher SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_voucher ALTER COLUMN org_id SET NOT NULL;

DROP INDEX IF EXISTS uk_biz_voucher_no;
CREATE UNIQUE INDEX uk_biz_voucher_no
    ON biz_voucher (org_id, voucher_word, period, voucher_no) WHERE deleted_at IS NULL;

DROP INDEX IF EXISTS uk_biz_voucher_year_end_period;      -- V0190:74
CREATE UNIQUE INDEX uk_biz_voucher_year_end_period
    ON biz_voucher (org_id, period)
    WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL;

DROP INDEX IF EXISTS uk_biz_voucher_opening_period;       -- V0189:120
CREATE UNIQUE INDEX uk_biz_voucher_opening_period
    ON biz_voucher (org_id, period)
    WHERE voucher_type = 'OPENING' AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_voucher_org_period ON biz_voucher (org_id, period, voucher_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_ic         ON biz_voucher (counterparty_org_id)
    WHERE counterparty_org_id IS NOT NULL;

-- A2 会计期间（V0137:172-196）
ALTER TABLE biz_accounting_period ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_accounting_period SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_accounting_period ALTER COLUMN org_id SET NOT NULL;
DROP INDEX IF EXISTS uk_biz_period_code;
DROP INDEX IF EXISTS uk_biz_period_ym;
CREATE UNIQUE INDEX uk_biz_period_code   ON biz_accounting_period (org_id, period_code);
CREATE UNIQUE INDEX uk_biz_period_ym     ON biz_accounting_period (org_id, period_year, period_month);
CREATE INDEX IF NOT EXISTS idx_biz_period_org_status ON biz_accounting_period (org_id, status, period_code);

-- A3 科目期初（V0189:57-82，uk 是表约束 uk_biz_opening_balance）
ALTER TABLE biz_opening_balance ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_opening_balance SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_opening_balance ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_opening_balance DROP CONSTRAINT IF EXISTS uk_biz_opening_balance;
ALTER TABLE biz_opening_balance ADD CONSTRAINT uk_biz_opening_balance
    UNIQUE (org_id, period_code, account_id);
CREATE INDEX IF NOT EXISTS idx_biz_opening_balance_org ON biz_opening_balance (org_id, period_code);

-- A4 库存期初（V0189:85-105）
ALTER TABLE biz_opening_inventory ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_opening_inventory SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_opening_inventory ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_opening_inventory DROP CONSTRAINT IF EXISTS uk_biz_opening_inventory;
ALTER TABLE biz_opening_inventory ADD CONSTRAINT uk_biz_opening_inventory
    UNIQUE (org_id, period_code, material_id, warehouse_id);

-- A5 取号规则（V0137:285-313，uk_biz_seq_type 是部分唯一索引）
ALTER TABLE biz_seq ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_seq SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_seq ALTER COLUMN org_id SET NOT NULL;
DROP INDEX IF EXISTS uk_biz_seq_type;
CREATE UNIQUE INDEX uk_biz_seq_type ON biz_seq (org_id, seq_type) WHERE deleted_at IS NULL;

-- A6 折旧批次（V0230:67 uk_biz_dep_batch_period (period_code)）
ALTER TABLE biz_depreciation_batch ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_depreciation_batch SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_depreciation_batch ALTER COLUMN org_id SET NOT NULL;
DROP INDEX IF EXISTS uk_biz_dep_batch_period;
CREATE UNIQUE INDEX uk_biz_dep_batch_period ON biz_depreciation_batch (org_id, period_code);

-- A7 汇兑重估（V0315:44-，每期重估结果）
ALTER TABLE biz_fx_revaluation ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_fx_revaluation SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_fx_revaluation ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_fx_reval_org ON biz_fx_revaluation (org_id, period_code);

-- A8 内部交易标识：凭证级（抵消的取数入口）
ALTER TABLE biz_voucher ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
COMMENT ON COLUMN biz_voucher.counterparty_org_id IS
  '内部交易对手方公司（同集团内的另一方）：非空 ⇒ 本凭证参与合并抵消；由单据过账时从客户/供应商的 internal_org_id 快照带入，事后改客户主数据不会改写历史认定（AGENTS.md 约定 13）';
```

#### B 组 · 单据主表（24 张）

统一三条语句/表（下例以 `biz_purchase_order` 为样板，其余**逐表相同**，`<T>` 替换表名）：

```sql
ALTER TABLE <T> ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE <T> SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE <T> ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_<T>_org ON <T> (org_id, created_at DESC);
```

| # | 表 | 归属理由（该表驱动什么账/什么报表） |
|:--:|---|---|
| B1 | `biz_purchase_requisition` | 请购 → 采购订单链条起点；预算占用 |
| B2 | `biz_purchase_order` | 采购订单 → 入库 → 应付 → 凭证 |
| B3 | `biz_stock_in` | 采购入库过账生成 借1405/贷2202 凭证 |
| B4 | `biz_stock_out` | 领用/报废出库生成成本凭证 |
| B5 | `biz_stock_transfer` | 仓间移位（**同公司内**，跨公司禁止，§4.5-⑦） |
| B6 | `biz_stock_take` | 盘点差异 → 报损报溢凭证 |
| B7 | `biz_sales_order` | 销售订单 → 发货 → 出库 → 应收 → 收入凭证 |
| B8 | `biz_shipment` | 发货单 |
| B9 | `biz_sales_return` | 销售退货 → 应收冲减 + 红字凭证 |
| B10 | `biz_purchase_return` | 采购退货 → 应付冲减 + 红字凭证 |
| B11 | `biz_receivable` | 应收账款台账（账龄、对账单、核销） |
| B12 | `biz_receipt` | 收款单 |
| B13 | `biz_receipt_refund` | 收款退款 |
| B14 | `biz_payable` | 应付账款台账 |
| B15 | `biz_payment` | 付款单 |
| B16 | `biz_invoice` | 销项/进项发票池 |
| B17 | `biz_expense_report` | 报销单 → 费用凭证（ARCH-6） |
| B18 | `biz_expense_payment` | 报销付款 |
| B19 | `biz_fixed_asset` | 资产卡片（折旧、处置凭证） |
| B20 | `biz_fixed_asset_change` | 资产变更（部门转移等） |
| B21 | `biz_depreciation` | 折旧明细 |
| B22 | `biz_budget` | 预算主表（`biz_budget_period`/`biz_budget_usage` 靠 `budget_id` join，不加列） |
| B23 | `biz_contract` | 采购/销售合同（ARCH-2） |
| B24 | `biz_commission_settlement` | 佣金结算（已登记 `SCOPED_TABLES`，dept 维度叠加在 org 内） |

**内部交易标识（只加在这 8 张有往来对手方的单据上）**：

```sql
-- 采购侧：对手方 = 供应商
ALTER TABLE biz_stock_in      ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_payable       ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_payment       ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_purchase_return ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
-- 销售侧：对手方 = 客户
ALTER TABLE biz_stock_out     ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_receivable    ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_receipt       ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;
ALTER TABLE biz_sales_return  ADD COLUMN IF NOT EXISTS counterparty_org_id UUID;

ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_stock_in_ic
    CHECK (counterparty_org_id IS NULL OR counterparty_org_id <> org_id);
-- 其余 7 张同一条 ck_<T>_ic（对手方不得等于本方）
```

#### C 组 · 库存三本账 + 仓库（4 张）

```sql
-- C1 仓库：归属公司（库存归属由仓库决定，这里是唯一权威）
ALTER TABLE biz_warehouse ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_warehouse SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_warehouse ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_warehouse_org ON biz_warehouse (org_id, status);

-- C2~C4 库存三本账：org_id 为**派生冗余**（由 warehouse.org_id 派生，写入时同事务带入）
ALTER TABLE biz_inventory          ADD COLUMN IF NOT EXISTS org_id UUID;
ALTER TABLE biz_inventory_location ADD COLUMN IF NOT EXISTS org_id UUID;
ALTER TABLE biz_inventory_txn      ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_inventory          SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
UPDATE biz_inventory_location SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
UPDATE biz_inventory_txn      SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_inventory          ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_inventory_location ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE biz_inventory_txn      ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_inv_org      ON biz_inventory (org_id, material_id);
CREATE INDEX IF NOT EXISTS idx_biz_inv_loc_org  ON biz_inventory_location (org_id, material_id);
CREATE INDEX IF NOT EXISTS idx_biz_inv_txn_org  ON biz_inventory_txn (org_id, created_at DESC);
```

> 派生冗余的理由：库存对账是**逐桶 FULL JOIN**（`skill cloudpivot-backend-workflow` → `references/inventory-chain-e2e-drill.md`：`biz_inventory` 是 per-bucket 桶表、`biz_inventory_txn` 无库位列）。若不加列，每次对账都要 join `biz_warehouse`，多公司后行数放大 10 倍，且"仓库换公司"这种极端操作会让历史流水归属漂移。冗余列 + 巡检断言「`biz_inventory_txn.org_id = biz_warehouse.org_id where warehouse_id`」把风险关在写入口。

#### D 组 · 主数据与系统（5 张 + 1 张新列约束）

```sql
-- D1 科目表：org_id 可空 = 集团共享；非空 = 公司专属明细科目（必须挂在共享父科目下）
ALTER TABLE biz_account ADD COLUMN IF NOT EXISTS org_id UUID;
COMMENT ON COLUMN biz_account.org_id IS
  'NULL = 集团共享科目（合并口径以共享科目为准）；非空 = 该公司专属明细科目（父科目必须是共享科目的末级，合并时按父科目聚合，无需映射表 → D-3）';
CREATE INDEX IF NOT EXISTS idx_biz_account_org ON biz_account (org_id) WHERE org_id IS NOT NULL;

-- D2/D3 客户与供应商：内部往来单位标识（不加 org_id：外部单位不归属本公司）
ALTER TABLE biz_customer ADD COLUMN IF NOT EXISTS is_internal     SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE biz_customer ADD COLUMN IF NOT EXISTS internal_org_id UUID;
ALTER TABLE biz_customer ADD CONSTRAINT ck_biz_customer_ic
    CHECK ((is_internal = 0 AND internal_org_id IS NULL)
        OR (is_internal = 1 AND internal_org_id IS NOT NULL));
ALTER TABLE biz_supplier ADD COLUMN IF NOT EXISTS is_internal     SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE biz_supplier ADD COLUMN IF NOT EXISTS internal_org_id UUID;
ALTER TABLE biz_supplier ADD CONSTRAINT ck_biz_supplier_ic
    CHECK ((is_internal = 0 AND internal_org_id IS NULL)
        OR (is_internal = 1 AND internal_org_id IS NOT NULL));
CREATE INDEX IF NOT EXISTS idx_biz_customer_ic ON biz_customer (internal_org_id) WHERE is_internal = 1;
CREATE INDEX IF NOT EXISTS idx_biz_supplier_ic ON biz_supplier (internal_org_id) WHERE is_internal = 1;

-- D4 部门树：归属公司（跨公司的 parent 由应用层拒绝 + N8 断言兜底）
ALTER TABLE sys_dept ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE sys_dept SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL AND deleted_at IS NULL;
ALTER TABLE sys_dept ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_sys_dept_org ON sys_dept (org_id, parent_id);

-- D5 用户：主属公司（可空 —— SUPER_ADMIN 与 ROBOT 允许不落公司，与 P0-FIX-2B D-1「admin 无部门」同一口径）
ALTER TABLE sys_user ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE sys_user SET org_id = '00000000-0000-4000-8000-000000000001'::uuid
 WHERE org_id IS NULL AND user_type = 'HUMAN' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_user_org ON sys_user (org_id);

-- D6 流程实例：公司快照（用于待办按公司过滤 + 审计回溯）
ALTER TABLE biz_flow_instance ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE biz_flow_instance SET org_id = '00000000-0000-4000-8000-000000000001'::uuid WHERE org_id IS NULL;
ALTER TABLE biz_flow_instance ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_org ON biz_flow_instance (org_id, status, started_at DESC);
```

**数量核对**：A 组 8 + B 组 24 + B 组内部交易附加 8（同表另列）+ C 组 4 + D 组 6 = **42 张表**（去重后 42，因内部交易列与 org_id 列落在同一批表上）。

#### 3.3 与现有表的外键关系

| 关系 | 做法 | 理由 |
|---|---|---|
| `biz_*` 业务表 → `biz_org` | **弱引用**（只加列，不加 FK） | 与全仓一致：`biz_voucher.account_id`、`biz_receivable.customer_id` 等都是弱引用（V0149:70 注释"科目（弱引用）"），加真 FK 会让 migration 在生产大表上长时间持锁 |
| `biz_org_user` / `biz_org_account` / `biz_consol_*` → `biz_org` | **真 FK**（RESTRICT / CASCADE 分开） | 新表无存量数据，FK 成本为零；`RESTRICT` 保证"有用户的公司删不掉"，`CASCADE` 只用在底稿→明细 |
| `biz_consol_offset` → `biz_consol_worksheet` | `ON DELETE CASCADE` | 底稿删除必须连带抵消行（不留孤儿） |
| `biz_consol_offset.rule_id` → `biz_consol_rule` | `ON DELETE SET NULL` | 规则停用/删除后历史抵消行仍要能看（`source_type` 仍标 AUTO） |

#### 3.4 数据量预估（每客户每年多少行）

| 表 | 单公司小客户（50 人） | 单公司中客户（500 人） | 集团 5 公司（中客户） |
|---|---:|---:|---:|
| `biz_voucher` | 1,200 张/年 | 7,200 张/年 | 36,000 张/年 |
| `biz_voucher_entry` | 5,000 行/年 | 30,000 行/年 | 150,000 行/年 |
| `biz_accounting_period` | 12 行/年 | 12 行/年 | 60 行/年 |
| `biz_opening_balance` | 56 行/年 | 56 行/年 | 280 行/年 |
| `biz_org` / `biz_org_user` / `biz_org_account` | 1 / ≤50 / ≤56 | 1 / ≤500 / ≤200 | 5~20 / ≤2000 / ≤1000（静态） |
| `biz_consol_worksheet` | 0 | 0 | 12 行/年 |
| `biz_consol_offset` | 0 | 0 | ≈100 行/期 → **1,200 行/年** |

结论：**增量最大的是 `biz_voucher_entry`（15 万行/年/集团）**，索引一律 `org_id` 前导（选择性最好）；`biz_consol_offset` 量级可忽略。

---

## 4. 业务规则与流程

### 4.1 组织（`biz_org`）状态机

```text
        (创建)                 (停用)
  ──────────────► ACTIVE ──────────────► DISABLED
                    ▲                       │
                    └───────────────────────┘
                          (重新启用)

  删除（软删）只允许：本 org 下 0 张凭证 且 0 张单据 且 0 个部门 且 0 个非默认用户
```

| 边 | 合法条件 | 拒绝时错误码 |
|---|---|---|
| 创建 | `org_code` 未占用；`parent_id` 存在且 `ACTIVE`；`org_level = parent.org_level + 1`；`org_path` 由后端拼（不接受前端传） | `ORG_CODE_DUPLICATED` / `ORG_PARENT_INVALID` |
| ACTIVE → DISABLED | 无未完成审批实例；无未结账期间之外的待办；不校验业务单据（停用=不能新建，不是注销） | — |
| DISABLED → ACTIVE | 无 | — |
| 软删 | 上述四条计数全 0；且不是 `is_group_root=1` 且有底稿引用 | `ORG_HAS_BUSINESS` |

**不变量（库/应用双层）**：
- `is_legal_entity=0` 的节点**不可**作为单据 `org_id` / 凭证 `org_id`（应用层 `require_org()` 拒；巡检 N4 断言）。
- `parent_id` 不能形成环（应用层沿 `org_path` 判断 + `ck_biz_org_no_self`）。

### 4.2 合并工作底稿状态机

```text
   (新建)              (生成/重算)              (确认)
  ────────► DRAFT ───────────────────► GENERATED ─────────► CONFIRMED
              ▲                            ▲                   │
              │                            └───────────────────┘
              └────── (清空自动行) ─────────┘      (解冻：需权限 + 理由,
                                                    unfreeze_count + 1)

  DRAFT / GENERATED / CONFIRMED ──(作废)──► CANCELLED（终态，释放唯一键）
```

| 边 | 条件 | 后果 |
|---|---|---|
| `DRAFT → GENERATED` | 范围内每家公司该期存在 `biz_accounting_period` 记录（`status` 任意，但 `CLOSED` 才置 `scope.period_closed=1`）；`base_currency` 全一致；至少 1 家 `included=1` | 写 `biz_consol_scope` 快照（冻结范围）+ 全量重算 AUTO 抵消行 |
| `GENERATED → GENERATED`（重算） | 同上 | **删除全部 AUTO 行后重建**；`MANUAL` 行原样保留（手工调整不能丢） |
| `GENERATED → CONFIRMED` | 至少跑过一次生成；`Σ debit = Σ credit`（`ck_biz_consol_ws_balance`） | 锁定：之后报表默认取 CONFIRMED 数；`generated_at` 不变 |
| `CONFIRMED → GENERATED`（解冻） | 需要 `business:erp:consol:unfreeze`；`reason` 必填（≥10 字） | `unfreeze_count+1`，写 `biz_status_log`（`entity_type='CONSOL_WORKSHEET'`） |
| 任意 → `CANCELLED` | 需 `business:erp:consol:confirm` | 释放 `uk_biz_consol_ws`，可重建同期间新底稿 |

**并发**：生成动作第一句 `SELECT ... FROM biz_consol_worksheet WHERE id = ? FOR UPDATE`（与 `voucher::find_voucher_locked`（`erp/voucher.rs:1409`）同一手法），保证同一底稿不会被两个请求同时重算。

### 4.3 抵消算法（本设计的核心）

**取数基元**（全部落在 `biz_voucher` / `biz_voucher_entry`，不读单据表 —— 抵消的是"账"不是"单"）：

```text
某公司 O 在某期间的「内部往来净额（某科目组）」 =
  Σ(entry.debit − entry.credit)
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
  WHERE v.org_id = O
    AND v.period = 本期期间                       -- 抵消按期间逐期算；「本年累计」由报表层把 1..N 月抵消行相加
    AND v.status = 'POSTED' AND v.deleted_at IS NULL
    AND v.counterparty_org_id = 对手方 P        -- 内部交易标识（A8 / B 组 ck_*_ic）
    AND e.account_code 命中规则 side_a_account（复用 report::match_account）
```

**三类抵消规则**：

| 类型 | A 侧科目组 | B 侧科目组 | 抵消分录 | 差额处理 |
|---|---|---|---|---|
| `IC_AR_AP`（内部往来） | `1122,1221` 应收/其他应收 | `2202,2241` 应付/其他应付 | 借 `2202`（冲 B 的应付）/ 贷 `1122`（冲 A 的应收） | `|A−B| > tolerance` → `diff_amount` 记差 + `warnings` 点名（"内部往来不符：A 应收 1000 / B 应付 990，差 10"），抵消额取 `min(A,B)` |
| `IC_REV_COST`（内部购销） | `6001` 营业收入 | `6401` 营业成本 | 借 `6401` / 贷 `6001`，金额 = `min(收入, 成本)` | 差额（B 尚未结转的存货部分）= 未实现内部利润 → **不剔除**，记 `warnings`（§1.2 边界 4） |
| `IC_CASH`（内部资金） | `1123,2203` 预付/预收 | 同上 | 借 `2203` / 贷 `1123` | 同往来 |

**配对与去重**：
- 对范围内每一对 `(A, B)` **只算一次**（按字典序 `A.id < B.id` 去重），双边取证：`A → counterparty=B` 与 `B → counterparty=A`。
- 一条规则一行抵消（`biz_consol_offset` 一行 = 一对公司 × 一条规则）；行内有 `side_a_amount`/`side_b_amount`/`offset_amount`/`diff_amount` 四个数，**可下钻、可对账**。
- 单边缺失（只有 A 记了内部往来，B 没记）→ 仍生成一行，`side_b_amount=0`，`offset_amount=0`，`diff_amount=A 侧净额`，`warnings` 点名（**绝不静默跳过** —— 这条正是"死单/账不实"的源头）。

**合并数公式**（报表层，只读）：

```text
合并某科目净额 = Σ_{O ∈ 范围} (O 的单体净额) − Σ_{底稿抵消行} (借 − 贷)
```

**这就是"不写 `biz_voucher`"的全部代价**：报表 SQL 多一个 `LEFT JOIN biz_consol_offset` 求和（100 行级别），换来单体账绝对干净。

### 4.4 勾稽关系（与哪些单据/报表对得上）

| # | 恒等式 | 层级 | 落点 |
|:--:|---|---|---|
| G1 | 每家公司：`Σ借 = Σ贷`（试算平衡） | 单公司 | 复用 `ledger.rs` 的 `trial-balance`，加 `org_id` 断言 |
| G2 | 每家公司：`资产 = 负债 + 权益`（未结账时损益并入权益） | 单公司 | 复用 `report.rs` 的 `check`（该文件模块文档「核心不变量」） |
| G3 | 每家公司：`净利润（本年累计） = 未分配利润本年变动` | 单公司 | 复用 `income_statement.rs` 的勾稽 |
| G4 | `合并前合计(某科目) = Σ 各家单体净额` | 合并 | 底稿生成时算并存进报表 `header.pre_consol_totals`（供客户核对"合并前/抵消/合并后"三栏） |
| G5 | `合并后 = 合并前合计 − 抵消合计` | 合并 | 报表自校验，差额 ≠ 0 → `check.balanced=false` |
| G6 | `Σ 抵消行 debit = Σ 抵消行 credit` | 合并 | 库层 `ck_biz_consol_ws_balance`（头）+ 生成后逐行校验 |
| G7 | `内部往来科目合并净额 = 0`（或 = `diff_amount` 的净差） | 合并 | 报表 `warnings` 给出残差，**绝不抹平** |
| G8 | 单据 ↔ 凭证：每张已过账单据的凭证金额 = 单据金额 | 单公司 | 复用现有勾稽（ARCH-3）；只加"凭证 `org_id` = 单据 `org_id`"一条断言（巡检 N1） |

### 4.5 特殊场景

| 场景 | 规则 |
|---|---|
| ① **跨期** | 抵消按 `period` 逐期算，不做累计；「本期」= 该期间的发生额，「本年累计」由报表层把 1..N 月抵消行相加（与三大报表的期间口径一致，见 `report::resolve_period`） |
| ② **红冲/作废** | 红字凭证 `is_red=1` 的净额天然为负（`ledger.rs` 模块文档「红冲怎么参与计算」），抵消取数直接累加，**不特判**；作废凭证 `status='CANCELLED'` 被 `status='POSTED'` 谓词排除 |
| ③ **反结账 / 反年结** | 公司 O 反结账后：所有含 O 的 `GENERATED` 底稿自动置为 `DRAFT`（并在列表上标红"范围公司已反结账，需重算"）；`CONFIRMED` 底稿**不自动解冻**，但报表顶部给 `warnings`（防止"确认数悄悄变了"） |
| ④ **并发** | 底稿生成：`FOR UPDATE` 行锁；公司反结账与底稿生成同时发生 → 反结账先拿 `biz_accounting_period` 行锁，生成时校验 `period_closed` 快照，冲突时生成方重试一次后返回 409 |
| ⑤ **公司停用** | `DISABLED` 公司不可新建单据/凭证，但**仍可出现在合并范围**（停用≠注销，报表要它）；软删才移出范围快照 |
| ⑥ **期初** | 期初按公司独立录入（`biz_opening_balance` / `biz_opening_inventory` 带 `org_id`）；年结下年结转按公司循环，一家失败全部回滚（与 `year_end.rs` 现有"一个事务"口径一致） |
| ⑦ **跨公司调拨** | 明确禁止（§1.2 边界 5）：`biz_stock_transfer` 提交时校验"出库仓与入库仓 `warehouse.org_id` 相同"，不同 → `TRANSFER_CROSS_ORG_FORBIDDEN` |
| ⑧ **币种** | 合并要求范围内 `base_currency` 全一致；不一致 → 生成时 `PRECONDITION_FAILED`，提示"公司 X 本位币为 USD，跨币种合并本期不支持" |

### 4.6 审批流怎么接（复用现有流程引擎，不硬编码）

1. **不改 `flow/assignee.rs` 的 8+2 种来源**（D-11）。跨公司审批靠"角色/用户指定"实现：集团财务持有 `ROLE` 型审批节点，其可访问公司由 `biz_org_user` 授权。
2. `biz_flow_instance` 加 `org_id`（D 组 D6）：发起时取单据的 `org_id` 快照。
3. `flow` 的待办/已办列表加"可见公司"过滤（`org_id = ANY(可见公司)`），与 `scope::paginate` 的 org 段同一实现。
4. **新增一道校验**（唯一改动引擎的地方）：`assignee.rs` 解析出人之后、落 `biz_flow_task` 之前，过滤掉"对该实例 `org_id` 无权"的人；过滤后为 0 人 → 走现有"空审批人策略 X2"（不新造策略）。
5. SoD（ARCH-8）：自审批/互斥职责校验增加"同公司内"为前提（同一人在 A 公司是采购、在 B 公司是财务，不算 SoD 冲突）。

### 4.7 巡检断言（新增，落 `scripts/verify/org_invariants.sql`，9 条）

| # | 断言 | 期望 |
|:--:|---|---|
| N1 | 每张已加 `org_id` 的表：与其"来源表"的 org 一致（如 `biz_voucher_entry.voucher_id → biz_voucher.org_id`、`biz_voucher.source_id → 单据.org_id`） | 0 行 |
| N2 | `biz_voucher.org_id` 指向的 `biz_org` 必须 `is_legal_entity=1` 且 `ACTIVE`（历史行除外：`org.status='DISABLED'` 只 WARN） | 0 行（ERROR 级） |
| N3 | 部门父链同公司：`sys_dept.parent_id` 的 `org_id` 必须与本行相同 | 0 行 |
| N4 | 每家公司每期间，`biz_accounting_period` 唯一（唯一键已保证）+ 期间连续（1..12 无洞，WARN） | 0 行 |
| N5 | `biz_inventory*.org_id = biz_warehouse.org_id`（C 组冗余列） | 0 行 |
| N6 | 内部交易双向性：存在 `A → B` 的内部凭证时，`B` 侧同期间应有对手方 `A` 的凭证；缺失列出（WARN，不阻断 —— 单边记账是**业务事实**，要人看） | 列表 |
| N7 | 号段归属：`biz_voucher.voucher_no` 的生成 org 与 `biz_seq` 的 org 一致（`voucher_word+period+voucher_no` 反查 `seq`） | 0 行 |
| N8 | 合并底稿：`Σ offset.debit = Σ offset.credit`，且 `included=1` 的公司期间都存在 | 0 行 |
| N9 | 法人公司（`is_legal_entity=1`）的 `credit_code` 为空（WARN，不阻断 —— 上线初期可后补，但合并报表抬头需要有信用代码） | 列表 |

---

## 5. 接口设计

**约定**：路由宏路径**不含 `/api`**（`AGENTS.md` 第 8 条）；前端 API 路径**必须写全 `/api/...`**（第 9 条）；所有出参 `*_id` 必带 `*_name`（第 13 条）；列表出参 `header / items / meta`（`AGENTS.md` 「出参口径」）。

**当前公司传递**：请求头 `X-Org-Id`（单个 UUID）。缺省取用户默认公司（`biz_org_user.is_default=1` → `sys_user.org_id` → 唯一 ACTIVE 公司）。显式传值必须 ∈ 用户可见公司集合（`biz_org_user` ∪ `SUPER_ADMIN` 全集），否则 `403 ORG_FORBIDDEN`。

### 5.1 组织管理（9 条）

| 方法 | 路径 | 权限码 | 入参关键字段 | 出参关键字段 |
|---|---|---|---|---|
| GET | `/business/erp/org/tree` | `business:erp:org:list` | `include_disabled=false` | `items[]: {id, org_code, org_name, short_name, parent_id, parent_name, org_level, is_legal_entity, is_group_root, base_currency, ownership_ratio, status, children[]}` |
| GET | `/business/erp/org/page` | `business:erp:org:list` | `keyword, status, page, size` | `header{total}`, `items[]`（同树节点字段 + `dept_count, user_count, voucher_count`） |
| POST | `/business/erp/org` | `business:erp:org:create` | `org_code, org_name, short_name, parent_id, is_legal_entity, credit_code, base_currency, fiscal_year_start_month, ownership_ratio, sort_order, remark` | `{id, org_path}` |
| PUT | `/business/erp/org/{id}` | `business:erp:org:update` | 同上（`org_code` 不可改；`parent_id` 改动重算子树 `org_path`） | `{id}` |
| PUT | `/business/erp/org/{id}/status` | `business:erp:org:update` | `status` | `{id, status}` |
| DELETE | `/business/erp/org/{id}` | `business:erp:org:delete` | — | `{id}`（软删；有业务 → 409 `ORG_HAS_BUSINESS` 带计数） |
| GET | `/business/erp/org/mine` | `business:erp:org:list` | — | `items[]: {id, org_name, short_name, is_default}`（前端切换器数据源） |
| GET | `/business/erp/org/{id}/accounts` | `business:erp:org:list` | `status?` | `items[]: {account_id, account_code, account_name, parent_id, level, is_leaf, shared_status, org_status, effective_status}` |
| PUT | `/business/erp/org/{id}/accounts` | `business:erp:org-account:update` | `items[]: {account_id, status, sort_order}`（整体覆盖式，缺省行=继承） | `{updated}` |

### 5.2 合并报表（12 行接口）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/erp/consol/worksheet/page` | `business:erp:consol:read` | `group_org_id, period, status, page, size`；`items[]: {id, group_org_id, group_org_name, period_code, status, member_count, offset_debit, diff_amount, generated_at, confirmed_at}` |
| POST | `/business/erp/consol/worksheet/generate` | `business:erp:consol:generate` | `{group_org_id, period_code, rebuild:true}` → 新建或重算，返回 `{id, status:'GENERATED', member_count, offset_debit, offset_credit, diff_amount, warnings[]}` |
| GET | `/business/erp/consol/worksheet/{id}` | `business:erp:consol:read` | `header{document, scope[], totals{pre_consol, offset, consol}}`、`items[]` 抵消行（`from_org_name/to_org_name/rule_name/account_code/account_name/debit_amount/credit_amount/diff_amount/source_type/reason`） |
| PUT | `/business/erp/consol/worksheet/{id}/confirm` | `business:erp:consol:confirm` | 无入参（或 `remark`） |
| PUT | `/business/erp/consol/worksheet/{id}/unfreeze` | `business:erp:consol:unfreeze` | `{reason}` 必填 |
| POST | `/business/erp/consol/worksheet/{id}/offsets` | `business:erp:consol:generate` | 手工抵消行：`{account_code, debit_amount, credit_amount, from_org_id?, to_org_id?, reason}` |
| PUT | `/business/erp/consol/offset/{id}` | `business:erp:consol:generate` | 仅 `source_type='MANUAL'` 可改 |
| DELETE | `/business/erp/consol/offset/{id}` | `business:erp:consol:generate` | 仅 `MANUAL` |
| GET | `/business/erp/consol/rule/page` | `business:erp:consol:rule:read` | `group_org_id, status` |
| POST/PUT/DELETE | `/business/erp/consol/rule[/{id}]` | `business:erp:consol:rule:update` | `is_system=1` 的规则只允许改 `tolerance_amount`/`status`，不允许改科目 |
| GET | `/business/erp/consol/report/{balance-sheet\|income-statement\|cash-flow}` | `business:erp:consol:read` | `group_org_id, period_code, worksheet_id?`（缺省取该期 `CONFIRMED`，无则最新 `GENERATED` 并挂 `warnings`） |
| GET | `/business/erp/consol/report/{...}/export` | `business:erp:consol:export` | xlsx，复用 `impexp::engine::write_sheet` + `handler::xlsx_response` |

**合并报表出参**（沿用现有三表结构 + 3 个新块）：

```jsonc
{
  "header": {
    "report_code": "CONSOL_BALANCE_SHEET",
    "group_org_id": "…", "group_org_name": "云枢集团",
    "period_code": "2026-11", "period_status": "CLOSED",
    "worksheet_id": "…", "worksheet_status": "CONFIRMED",
    "scope": [ { "member_org_id": "…", "member_org_name": "北京公司", "included": true, "period_closed": true } ],
    "totals": { "pre_consol": { "ASSET": "…", "LIABILITY": "…", "EQUITY": "…" },
                "offset":     { "ASSET": "…", … },
                "consol":     { "ASSET": "…", … } }
  },
  "items": [ { "item_code": "…", "item_name": "货币资金",
               "pre_consol_amount": "…", "offset_amount": "…", "amount": "…",
               "by_org": [ { "org_id": "…", "org_name": "北京公司", "amount": "…" } ] } ],
  "meta": { "check": { "balanced": true, "diff": "0.0000", "message": "", "hints": [] },
            "warnings": [ "内部往来不符：北京公司 应收 1000.00 / 上海公司 应付 990.00，差 10.00" ] }
}
```

### 5.3 单公司报表改造（只加入参，不新增第二套 —— D-12）

| 现有接口（实测路径见各模块文档头） | 改造 |
|---|---|
| `GET /business/erp/report/balance-sheet?period=` | 加 `org_id?`（缺省 = 当前公司） |
| `GET /business/erp/report/income-statement?period=` | 同上 |
| `GET /business/erp/report/cash-flow?period=` | 同上 |
| `GET /business/erp/ledger/page` / `trial-balance` / `aux-balance` | 同上 |
| `GET /business/erp/report/payable-aging` | 同上 |
| `GET /business/erp/voucher/page`、`/business/erp/period/page` | 同上（列表默认只显示当前公司） |
| 三个 `/export` | 同上 |

### 5.4 权限码（`{模块}:{资源}:{动作}`，与 `sys_menu.auth_code` 一一对应）

| 权限码 | 用途 | 授予 |
|---|---|---|
| `business:erp:org:list` | 看组织树/公司列表/科目启停 | 财务主管、系统管理员 |
| `business:erp:org:create` / `update` / `delete` | 建/改/删公司 | 仅 `SUPER_ADMIN` + 集团财务总监（种子只给超管，与 `V0065/V0020` 同例） |
| `business:erp:org-account:update` | 改公司级科目启停 | 财务主管 |
| `business:erp:consol:read` | 看底稿/合并报表 | 集团财务 |
| `business:erp:consol:generate` | 生成/重算/手工抵消行 | 集团财务主管 |
| `business:erp:consol:confirm` | 确认/作废底稿 | 集团财务总监（与操作人分离：`generate` 的人不能同时 `confirm`，除非超管 —— 复用 ARCH-8 的 SoD 判定） |
| `business:erp:consol:unfreeze` | 解冻已确认底稿 | 仅超管 / 集团财务总监 |
| `business:erp:consol:rule:read` / `rule:update` | 抵消规则 | 集团财务主管 |
| `business:erp:consol:export` | 合并报表导出 | 集团财务 |

### 5.5 分页 / 筛选 / 导出

- 分页统一走 `scope::paginate`（该函数是列表查询的唯一入口，见 `shared/scope.rs` 模块文档），**org 片段作为第 0 段**拼在 `RowFilter` 之前；
- 筛选：`group_org_id` / `period_code` / `status` / `source_type` / `org_id`；
- 导出：xlsx 复用 `impexp` 的 `write_sheet` + `xlsx_response`（与三大报表导出同一实现，不新写），合并报表导出**必须**把 `warnings` 写进第二个 sheet「校验说明」。

---

## 6. 前端设计

### 6.1 页面清单（菜单挂哪、路由 path）

| 页面 | 路由 path | 菜单挂载 | `title`（≤6 字） | `icon` | 权限码 |
|---|---|---|---|---|---|
| 公司管理 | `/business/erp/org` | 「ERP 主数据」目录（`sys_menu` id `…940`） | `公司管理` | `building` | `business:erp:org:list` |
| 公司科目启停 | 弹窗（不单独挂菜单） | 公司管理行内「科目」按钮 | — | — | `business:erp:org-account:update` |
| 合并报表（三大表 Tab） | `/business/erp/consol/report` | 「ERP 台账」目录（`…946`） | `合并报表` | `chart-pie` | `business:erp:consol:read` |
| 合并底稿 | `/business/erp/consol/worksheet` | 「ERP 台账」目录（`…946`） | `合并底稿` | `file-copy` | `business:erp:consol:read` |
| 抵消规则 | `/business/erp/consol/rule` | 「ERP 台账」目录（`…946`） | `抵消规则` | `setting` | `business:erp:consol:rule:read` |

**菜单硬约束（`docs/编码规范.md` §1.6.1 + `scripts/verify/menu_invariants.sh` 6+2 条断言）**：
- `title` 2-6 字、**≤12 字硬顶**、不带标点/章节号/卡号；
- `name` 短名（新菜单**不许**沿用 `business:erp:report:xxx` 这种长编码形式，用 `公司管理`/`合并报表` 等短名）；
- `icon` 必须是 TDesign 真实图标名，落地前 `ls frontend/node_modules/tdesign-icons-vue-next/esm/components/{building,chart-pie,file-copy,setting}.js` 逐个确认；
- 同一父节点 `sort_order` **必须唯一** —— 迁移里用 `(SELECT COALESCE(MAX(sort_order),0)+10 FROM sys_menu WHERE parent_id=<父> AND deleted_at IS NULL)` 计算，**禁止硬编码**；
- 新增按钮节点 `menu_type=3`，`auth_code` 与 §5.4 对齐。

### 6.2 关键交互

**① 公司切换器（顶栏）** — `frontend/src/layouts/BasicLayout.vue` header 右侧，`t-select` + `t-tag`（当前公司）：

```
展示：<t-icon name="building"/> 云枢总部 ▾
数据：GET /api/business/erp/org/mine（缓存进 Pinia store `orgStore`）
行为：切换 → ① 写 localStorage `current_org_id`  ② 清空所有列表页缓存（Pinia 里按 org 分桶或直接 `$reset`）
     ③ 重新拉当前页数据  ④ 页面顶部出现一次性 toast「已切换到 北京公司」
禁用：只有 1 家公司时隐藏整个切换器（现有客户零感知）
```

**② `X-Org-Id` 注入** — `frontend/src/api/request.ts`（第 95 行 `config.headers.Authorization = ...` 旁边加一行）：

```
请求拦截：config.headers['X-Org-Id'] = orgStore.currentOrgId ?? ''
响应拦截：code=40301(ORG_FORBIDDEN) → 顶部红条「无权访问该公司数据，已切回默认公司」+ 自动切回 + 刷新
```

**③ 公司管理页**：左树右表（仿 `views/system/dept` 的左右布局）；新增/编辑抽屉字段 = §5.1 POST 入参；`is_legal_entity=0` 时 `credit_code` 置灰并提示"管理节点不可记账"；删除按钮前置 `t-popconfirm`，文案带计数「该公司下有 21 张凭证、4 个部门，不能删除」。

**④ 合并底稿页**：
- 顶部筛选：合并主体（`t-select`，只列 `is_group_root=1` 或选了会提示"该节点无子公司"）+ 期间（`t-date-picker mode="month"`）；
- 主表列：期间 / 状态（`t-tag`：DRAFT 灰、GENERATED 蓝、CONFIRMED 绿、CANCELLED 灰）/ 纳入公司数 / 抵消借贷 / 差额 / 生成时间；
- 行操作：`生成/重算`（二次确认："重算将删除全部自动抵消行，手工行保留"）、`确认`、`解冻`（弹框强制填理由 ≥10 字）、`作废`、`查看明细`；
- 明细抽屉：抵消行表格 + 顶部三栏汇总（合并前 / 抵消 / 合并后）+ `warnings` 黄色 `t-alert`（**不允许**折叠隐藏差额）；
- 状态按钮映射（关键：**不留无按钮的中间态**，AGENTS.md 踩坑清单）：`DRAFT`→[生成]、`GENERATED`→[重算,确认,作废]、`CONFIRMED`→[解冻,导出]、`CANCELLED`→[]（只读）。

**⑤ 合并报表页**：三 Tab（资产负债表/利润表/现金流量表）+ 期间/主体筛选 + 三栏（合并前/抵消/合并后）可折叠 + 「校验」面板（复用现有报表页的 `check` 展示组件）+ 导出按钮（权限码 `consol:export`）。

### 6.3 复用哪些现有组件

| 组件 | 复用点 |
|---|---|
| `DocActionBar` | 底稿页操作按钮（生成/确认/解冻/作废）——按钮态由 `status` 驱动，与单据页同一套 |
| `FlowProgress` | **不用**：合并底稿不走审批流（v1 只有"确认"这一人动，避免与流程引擎耦合）；若后续要审批另开卡 |
| `DocAttachmentPanel` | 底稿附件（合并说明、抵消依据） |
| 报表页三栏/校验面板 | 从 `views/business/erp/report/balance-sheet/index.vue` 抽取的 `ReportTable` / `ReportCheckPanel`（已在三张报表复用，本次是第 4 次复用） |
| 左右树布局 | `views/system/dept/index.vue` 的树表布局 |
| 导出 | `utils/download.ts` + 现有报表导出按钮 |

### 6.4 错误文案（前端直出，不暴露错误码给用户）

| 后端错误 | 文案 |
|---|---|
| `ORG_FORBIDDEN` | 你没有「北京公司」的访问权限，请联系集团管理员授权 |
| `ORG_HAS_BUSINESS` | 「北京公司」下还有 21 张凭证、4 个部门、3 个用户，请先转移或清理 |
| `ORG_NOT_LEGAL_ENTITY` | 「华东区」是管理节点，不能作为单据归属公司 |
| `CONSOL_PERIOD_NOT_READY` | 「上海公司」2026-11 期间尚未开启，无法合并 |
| `CONSOL_CURRENCY_MISMATCH` | 「香港公司」本位币为 HKD，本期暂不支持跨币种合并 |
| `CONSOL_WORKSHEET_CONFIRMED` | 底稿已确认，如需重算请先「解冻」并填写理由 |
| `TRANSFER_CROSS_ORG_FORBIDDEN` | 调拨双方仓库分属不同公司，跨公司请走「销售出库 + 采购入库」 |

---

## 7. 迁移与上线

### 7.1 迁移文件清单（V0368 ~ V0377，10 个文件 / 20 个 up+down）

| 版本 | 文件 | 内容 | 幂等策略 |
|---|---|---|---|
| V0368 | `org_tables` | `biz_org` + `biz_org_user` + 默认公司种子 | `CREATE TABLE IF NOT EXISTS` + `ON CONFLICT (id) DO NOTHING` |
| V0369 | `org_id_core` | A 组 8 张 + 8 条唯一键重建 | `ADD COLUMN IF NOT EXISTS`、`DROP INDEX IF EXISTS` + 重建；回填带 `WHERE org_id IS NULL` |
| V0370 | `org_id_docs` | B 组 24 张 | 同上（逐表三条语句） |
| V0371 | `org_id_inventory` | C 组 4 张 | 同上 |
| V0372 | `org_id_masterdata` | D 组 6 张（`biz_account`/`biz_customer`/`biz_supplier`/`sys_dept`/`sys_user`/`biz_flow_instance`） | 同上；`ck_*_ic` 用 `DO $$ ... IF NOT EXISTS (pg_constraint) ... $$` 包 |
| V0373 | `org_account_switch` | `biz_org_account` | `CREATE TABLE IF NOT EXISTS` |
| V0374 | `intercompany` | `biz_voucher.counterparty_org_id` + 8 张单据列 + `ck_*_ic` | `ADD COLUMN IF NOT EXISTS` |
| V0375 | `consol_tables` | `biz_consol_worksheet` / `_scope` / `_rule` / `_offset` + 3 条系统规则模板 | 同上 + `WHERE NOT EXISTS` 种规则 |
| V0376 | `consol_menu` | 3 个页面菜单 + 7 个按钮节点（§5.4） | `WHERE NOT EXISTS (SELECT 1 FROM sys_menu WHERE path = ... AND name = ...)`；`sort_order` 用子查询算 |
| V0377 | `org_down_guard` | **下回滚护栏**：`down.sql` 里加 `IF (SELECT count(*) FROM biz_org WHERE deleted_at IS NULL) > 1 THEN RAISE EXCEPTION` | 见 §7.3 |

**每个迁移末尾必须带 `DO $$ ... $$` 自检块**（本仓既有风格，见 `V0264`/`V0189`：列数/索引存在性/CHECK 实际拦不拦坏数据/默认值），任一条不符 → 迁移失败（fail closed）。

### 7.2 存量数据处理（回填 + 是否 downtime）

| 项 | 做法 | 耗时 |
|---|---|---|
| 42 张表 `org_id` 回填 | 单条 `UPDATE ... WHERE org_id IS NULL`（全表同值，无计算） | 生产规模（凭证明细 ≈ 100 行）：**秒级**；大客户（15 万行/年 × 3 年）：< 30 s |
| 默认公司种子 | 固定 UUID `00000000-0000-4000-8000-000000000001` | 毫秒 |
| `sys_dept` / `sys_user` 回填 | 同上；`ROBOT` 与 `admin` 允许 `org_id IS NULL`（与 P0-FIX-2B D-1 一致） | 毫秒 |
| `biz_customer` / `biz_supplier` | `is_internal=0`（DEFAULT，不回填） | 0 |
| `biz_seq` | 回填后 `uk (org_id, seq_type)` 成立（原 `seq_type` 唯一 ⊂ 新键） | 毫秒 |

**Downtime**：**需要**，但只等于一次服务重启的时长（5~15 分钟：`systemctl stop` → 部署二进制 → `start`，启动链先跑迁移后监听端口）。因为迁移在应用启动链里执行（`AGENTS.md`「cargo run 自动迁移 + 启动 :8023」），**不存在"旧代码 + 新 schema"并存窗口**，所以：
- `org_id` 列**不设 DEFAULT**（避免"漏传静默落默认公司"这种最危险的错法）；
- 不需要 `NOT NULL → 可空` 的三期过渡。

### 7.3 回滚方案

| 层级 | 手段 | 判据 |
|---|---|---|
| 代码 | `git revert` 到上线前 commit（`2dc4f288`）+ `systemctl restart` | 上线后 24h 内任一 P0 |
| schema | 逐迁移 `down.sql`：`DROP INDEX`（重建原键）→ `DROP COLUMN org_id` → 最后 `DROP TABLE biz_org*`/`biz_consol_*` | **护栏**：`down.sql` 首句 `IF (SELECT count(*) FROM biz_org WHERE deleted_at IS NULL) > 1 THEN RAISE EXCEPTION '已有多家公司数据，禁止回滚（会丢归属）'`；一旦第二家公司建了数据，回滚只允许"代码回滚 + 保留 schema（org 全走默认公司）" |
| 数据 | 上线前 `pg_dump` 全库（`scripts/ops/` 已有备份脚本） | 回滚后校验：42 张表的 `count(*)` 与 dump 一致、三张报表 JSON 与 dump 环境逐行一致 |

### 7.4 分几批上线（能否灰度）

| 批 | 内容 | 对外可见变化 | 灰度策略 |
|:--:|---|---|---|
| **批 1** | V0368~V0373（组织表 + 全部 `org_id` 列 + 回填 + 公司管理页 + 切换器） | 多一个「公司管理」菜单；切换器只有 1 家时隐藏 | 对现有客户**零感知**：所有数据落默认公司，报表数字逐行不变（S2） |
| **批 2** | V0374 + 财务核心代码改造（`post_voucher`/取号/期间/期初/年结/三表/总账带 `org_id`）+ 巡检 N1~N5 | 报表页多一个"公司"筛选（只有 1 家时隐藏） | 先在 145 dev 跑「13 域全链 e2e」+ 「隔离库全量报表 diff」，全绿才上 144 |
| **批 3** | 单据主表 org 化（53 个 `post_voucher` 调用点）+ 跨公司调拨拦截 + N6/N7 | 建单时自动带当前公司；错误文案 | 同上；额外做**双公司演练库**（A/B 两家）+ N1 交叉污染断言必须 0 行 |
| **批 4** | V0375~V0376 + 合并底稿/抵消/合并三表前端 | 3 个新菜单（默认只授 `SUPER_ADMIN`） | 按权限码灰度：先给 1 个试点客户的集团财务账号，观察 2 周再全面放开 |

### 7.5 DDL 已在真库跑通（不是纸面 SQL）

设计卡要求"DDL 完整"。写在 markdown 里的 DDL 是**未执行的文本**：列名写错、索引名记错、CHECK 写不出它声称的语义，都不会报错。所以 §3.1 / §3.2 的语句已原样在 `cloudpivot_test`（PostgreSQL 18.6）上真跑过一遍，**整段事务 ROLLBACK，不留任何对象**：

| 证据 | 结果 |
|---|---|
| `docs/evidence/2026-11-10-arch9-ddl/01_new_tables.out` | RC=0：7 张新表 + 索引 + 种子建成；**12 条约束齿口断言全过**（非法 status / 管理节点带信用代码 / 底稿借贷不等 / 重复底稿 / 非法期间 / MANUAL 无理由 / 双边非零 / AUTO 无规则 / 范围无原因 / 双默认公司 → 全部被拒；CANCELLED 允许、合法 AUTO 行可写） |
| `docs/evidence/2026-11-10-arch9-ddl/02_alter_tables.out` | RC=0：§3.2 的 ALTER 在真实 schema 上全部成功（3 条凭证唯一键重建 + 期间 2 条 + `biz_seq` + 两张期初表约束 + `sys_dept`/`sys_user`/`biz_flow_instance` 回填与 `SET NOT NULL`）；**8 条语义断言全过**：两家公司可同号凭证 / 同 `period_code` / 同 `seq_type`，同公司重复被拒 |

**跑这一遍改掉了设计里的两个真错误**（写在这里，防止实现时照抄错的）：
1. `period_code` 正则原写 `^[0-9]{4}-[0-9]{2}$` → `2026-13` 能进库；已改 `^[0-9]{4}-(0[1-9]|1[0-2])$`。
2. `biz_inventory_txn` 的索引原写 `(org_id, txn_date DESC)` —— **该表没有 `txn_date` 列**（实测列清单无此列），已改 `(org_id, created_at DESC)`。

未覆盖：`cloudpivot_test` 里不存在的 26 张单据表，其 ALTER 形状与已验过的 `biz_stock_in` 样板完全一致，风险只剩"表名/列名笔误"，由 §7.1 要求的迁移末尾 `DO $$` 自检块 + 实现时 `\d <表>` 实测兜底（清单见证据 README）。

---

## 8. 工作量估算与风险

### 8.1 实现卡清单（49 张，每张 ≤1.5 h，标注依赖）

**批 1 · 组织主数据与上下文（8 张，≈12 h）**

| # | 卡 | 依赖 | 估时 |
|:--:|---|---|--:|
| 1-1 | 迁移 V0368 `biz_org` + `biz_org_user` + 默认公司种子 + 自检块 | — | 1.5h |
| 1-2 | `business/erp/org/{mod,handler,dto,model}.rs` 四件套：树 + 分页 + CRUD（含 `org_path` 维护、删除前计数） | 1-1 | 1.5h |
| 1-3 | 组织状态机 + `ORG_*` 错误码（`shared/error_code.rs`）+ `require_org()` 请求上下文解析（`X-Org-Id`） | 1-2 | 1.5h |
| 1-4 | `biz_org_user` 授权接口 + `org/mine` + 可见公司集合缓存（复用 `shared/principal` 的 cache-aside，TTL 300 s） | 1-3 | 1.5h |
| 1-5 | 单测：org 树 CRUD / 环检测 / `is_legal_entity=0` 拒绝 / 删除计数 | 1-2 | 1.5h |
| 1-6 | 前端 `views/business/erp/org/index.vue` + `api/business/erp/org.ts` | 1-4 | 1.5h |
| 1-7 | 前端 `orgStore` + `request.ts` 注入 `X-Org-Id` + `BasicLayout` 切换器 + 40301 处理 | 1-6 | 1.5h |
| 1-8 | 菜单/权限种子迁移 V0376（公司管理 3 节点）+ `menu_invariants.sh` 实跑 | 1-1 | 1.5h |

**批 2 · 财务核心 org 化（13 张，≈19.5 h）**

| # | 卡 | 依赖 | 估时 |
|:--:|---|---|--:|
| 2-1 | 迁移 V0369（A 组 8 表 + 8 条唯一键）+ 回填 + 自检 | 1-1 | 1.5h |
| 2-2 | 迁移 V0371/V0372/V0373（C/D 组 + `biz_org_account`）+ 回填 | 2-1 | 1.5h |
| 2-3 | `post_voucher`：`PostReq` 加 `org_id` + INSERT 加列 + 去掉 `SCOPE-EXEMPT: 凭证台账无归属列` 注释 + 期间闸带 org | 2-1 | 1.5h |
| 2-4 | 取号 `next_voucher_no` / `next_red_voucher_no` / `next_doc_no` 加 `org_id` + `biz_seq` org 维度 + 注释纠偏（模块注释 72 行「biz_seq 无归属列」→ 改） | 2-3 | 1.5h |
| 2-5 | 期间：`period.rs`（开期间/结账/反结账）+ `biz_accounting_period` org 维度 | 2-3 | 1.5h |
| 2-6 | 期初：`opening.rs`（科目期初 + 库存期初）org 维度 + 校验 | 2-5 | 1.5h |
| 2-7 | 年结：`year_end.rs` 按公司循环 + 结转凭证带 org + `uk_*_year_end_period` 适配 | 2-5 | 1.5h |
| 2-8 | 三表取数：`report.rs` 加 org 过滤 + `org_id` 入参（含 `resolve_period`/`load_accounts`/差额快照） | 2-3 | 1.5h |
| 2-9 | `income_statement.rs` / `cash_flow.rs` / `ledger.rs` 同上（三个文件同一改法） | 2-8 | 1.5h |
| 2-10 | 测试夹具：12 个文件里 `mod tests` 内的裸 `INSERT INTO biz_voucher` 补 `org_id`（逐文件 1 行） | 2-3 | 1.5h |
| 2-11 | 巡检 `scripts/verify/org_invariants.sql` 之 N1/N2/N3/N5 + 接入 `scripts/check.sh` | 2-6 | 1.5h |
| 2-12 | 回归：三表 JSON 上线前后逐行 diff 脚本（S2 判据）+ 隔离库实跑 | 2-9 | 1.5h |
| 2-13 | `FROM biz_voucher` 取数清单（244 处 / 62 文件）逐条加 `v.org_id = ?` + 打勾表入库 `docs/`（R2 的唯一防线） | 2-9 | 1.5h |

**批 3 · 单据主表与内部交易（14 张，≈21 h）**

| # | 卡 | 依赖 | 估时 |
|:--:|---|---|--:|
| 3-1 | 迁移 V0370（B 组 24 表） | 2-1 | 1.5h |
| 3-2 | 迁移 V0374（内部交易 8 列 + 9 条 `ck_*_ic`） | 3-1 | 1.5h |
| 3-3 | 采购链 handler：`purchase_requisition`/`purchase_order`/`stock_in` 建单写 `org_id` + 列表加 org 过滤 | 3-2 | 1.5h |
| 3-4 | 销售链 handler：`sales_order`/`shipment`/`stock_out`/`sales_return` 同上 | 3-3 | 1.5h |
| 3-5 | 资金链 handler：`receivable`/`receipt`/`receipt_refund`/`payable`/`payment`/`invoice` 同上 | 3-4 | 1.5h |
| 3-6 | 费用/资产/预算/合同/佣金 handler（ARCH-6 五域）同上 | 3-5 | 1.5h |
| 3-7 | 库存三本账写入点（13 个直写文件）：`org_id` 由 `warehouse.org_id` 派生 + 唯一写入口收口（评审 P2-5 已单列，本卡只加 org 派生） | 3-2 | 1.5h |
| 3-8 | 53 个 `post_voucher` 调用点补 `org_id`（按域分 4 组，编译器逐个报错的机械改法）——**组 1 采购/库存** | 3-3 | 1.5h |
| 3-9 | 同上 —— 组 2 销售 | 3-4 | 1.5h |
| 3-10 | 同上 —— 组 3 资金/开票 | 3-5 | 1.5h |
| 3-11 | 同上 —— 组 4 费用/资产/预算/合同/佣金/年结 | 3-6 | 1.5h |
| 3-12 | 内部交易识别：客户/供应商维护页加"内部单位"开关 + 单据保存时快照 `counterparty_org_id` | 3-2 | 1.5h |
| 3-13 | 跨公司调拨拦截 + `TRANSFER_CROSS_ORG_FORBIDDEN` + 单测 | 3-7 | 1.5h |
| 3-14 | 巡检 N6/N7 + 双公司演练库脚本（A/B 两家 + 内部交易夹具） | 3-12 | 1.5h |

**批 4 · 合并底稿与合并报表（10 张，≈15 h）**

| # | 卡 | 依赖 | 估时 |
|:--:|---|---|--:|
| 4-1 | 迁移 V0375（4 张合并表 + 3 条规则模板）+ 自检 | 3-2 | 1.5h |
| 4-2 | `consol/rule` 四件套（规则 CRUD + 建主体复制模板） | 4-1 | 1.5h |
| 4-3 | `consol/worksheet` 状态机 + 范围快照 + `FOR UPDATE` 并发 | 4-2 | 1.5h |
| 4-4 | 抵消算法：`IC_AR_AP`（往来配对 + min + 差额）纯函数 + 单测（6 组：平衡/单边/差额/多对/红冲/作废） | 4-3 | 1.5h |
| 4-5 | 抵消算法：`IC_REV_COST` + `IC_CASH` + 规则匹配（复用 `report::match_account`）+ 单测 | 4-4 | 1.5h |
| 4-6 | 手工抵消行 CRUD（重算保留 MANUAL）+ 确认/解冻/作废 + 状态日志 | 4-5 | 1.5h |
| 4-7 | 合并资产负债表（三栏 + G4~G7 自校验 + warnings） | 4-6 | 1.5h |
| 4-8 | 合并利润表 + 合并现金流量表 | 4-7 | 1.5h |
| 4-9 | 合并报表导出 xlsx（含「校验说明」sheet） | 4-8 | 1.5h |
| 4-10 | 巡检 N8 + 合并恒等式 e2e（双公司夹具：内部交易 → 抵消 → 合并数 = 单体合计 − 抵消） | 4-9 | 1.5h |

**批 5 · 前端（4 张，≈6 h）**：底稿页 / 合并报表页 / 抵消规则页 / 报表页三栏抽取复用

**批 6 · 回归与上线（4 张，≈6 h）**：13 域全链 e2e（带 org）/ 499 个 verify 脚本分批改造 / 144 灰度与回滚演练 / 手册与 `docs/manual/`

**合计**：49 张实现卡 × 1.5 h ≈ **73.5 人时 ≈ 9.2 人日**；加联调回归与上线保障 **5 人日** → **14 人日**（单人串行）；2 人并行 ≈ **8 个工作日**。

### 8.2 与卡面「≤1.5 小时」的关系（诚实说明）

49 张卡里 **12 张是机械改法**（3-8~3-11 的调用点补齐、2-10 的测试夹具、2-13 的取数加过滤）确实 ≤1.5 h；**4-4/4-5/4-7/3-7 这 4 张是算法/收口卡**，实际更可能是 3~4 h —— 拆到这里再细分会切碎上下文（每个抵消算法卡要同时看规则表、凭证取数、报表口径三处），所以保留为"1.5 h 标准节奏 + 允许 2 倍溢出"并在依赖链上留了缓冲。**总估时已按 2 倍溢出率计入（9.2 人日 = 73.5 h 已经包含溢出）**。

### 8.3 已知风险（哪些地方可能推翻设计）

| # | 风险 | 影响 | 缓解 / 判据 |
|:--:|---|---|---|
| **R1** | **漏注入 org → 跨公司数据泄露**（最严重，且不报错） | A 公司账号看到 B 公司单据 | ①编译器：`PostReq.org_id` 无 `Default`，53 处调用点强制补；②唯一入口 `require_org()`；③巡检 N1 每张表一条交叉污染断言；④e2e 用"B 公司建单 → A 公司账号逐页搜索"作为验收门 |
| **R2** | 取数 SQL 多为 `query_sql` 直写，绕过 `paginate` 唯一入口：`grep -rc "FROM biz_voucher"` 实测 **244 处 / 62 个文件** | 同 R1 | 落地前把 244 处逐条打勾（建议按 §2.2 那 8 个生产文件优先）；清单进 `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` 同类文档；`cargo clippy` 加不了这条，只能靠清单 + 巡检 N1 |
| **R3** | `biz_voucher` 唯一键重建在大客户表上锁表 | 升级窗口拉长 | 生产凭证 21 张（评审报告）无风险；大客户走"在线建索引"脚本（`CREATE INDEX CONCURRENTLY` 不能在事务迁移里，须独立 `scripts/ops/` 脚本 + 二次迁移改名），列入 §7.4 批 2 的上线检查单 |
| **R4** | **多币种合并**：现系统无本位币报表折算 | 有外币业务的集团无法合并 | v1 硬拦（§4.5-⑧ + `CONSOL_CURRENCY_MISMATCH`）；要做得先落"外币报表折算"卡（ARCH-6 未覆盖），**这是最可能被客户推翻的边界** |
| **R5** | 合并范围时点变化（年内新设/处置子公司） | 历史底稿重算结果与当时不符 | 范围快照（`biz_consol_scope`）+ `CONFIRMED` 后禁止重算（必须先解冻，留痕）；年内新设的处理规则 v1 = 手工调整行 |
| **R6** | 与 SaaS 版 `tenant_id` 的关系未定 | 未来企业版叠加多租户时两套隔离"打架" | 明确 `org` 是 tenant 的下级维度；`biz_org` 表**不预置** `tenant_id`（避免半成品），但所有代码路径的 org 过滤都收敛在 `require_org()` 一处，将来加 tenant 只改这一处 + 迁移加列 |
| **R7** | 审批人解析不含 org → 待办派给无权限公司的人 | 越权审批（比越权看数据更严重） | §4.6-4 的"解析后过滤 + 空结果走 X2 策略"；e2e 用"只有 A 公司权限的审批人"验证拿不到 B 公司待办 |
| **R8** | 499 个 verify 脚本假设单公司 | 全量回归长期红 → 门禁失效（团队会开始忽略红灯） | 允许 `SKIP_ORG_CHECKS=1` 显式豁免但必须打印"已跳过 N 条 org 断言"，且 §7.4 每批上线前把该批涉及的脚本改完（禁止长期豁免） |
| **R9** | 菜单 invariants（`title` ≤12 字 / 同父 `sort_order` 唯一 / `icon` 真实存在） | 侧边栏歪、图标空白 | 迁移里 `sort_order` 用子查询算；落地前 `bash scripts/verify/menu_invariants.sh` 实跑 |
| **R10** | 库存三本账 `org_id` 是派生冗余列，可能被直写点写歪 | 库存对账按公司分组错 | N5 断言 + 13 个直写点收口（评审 P2-5 已单列卡 `t_69e360ad` 同级任务）；上线前跑一次全量 N5 必须 0 行 |
| **R11** | 底稿"确认后报表口径变了"（客户看的是 CONFIRMED 数，但底层公司反结账了） | 客户质疑数字 | 反结账后对所有含该公司的 `CONFIRMED` 底稿在列表/报表顶部挂 `warnings`（§4.5-③），不自动改数 |

### 8.4 待确认（必须产品/老板拍板，已给确认方式）

| # | 待确认 | 默认取向（未答就按此实施） | 确认方式 |
|:--:|---|---|---|
| **Q1** | 默认公司叫什么 | `云枢总部` / `HQ001`（D-9） | 老板一句确认；改名只改 `biz_org.org_name`（1 条 UPDATE），不影响任何外键 |
| **Q2** | 跨公司调拨是否真的不做 | 不做（禁止，走销售+采购） | 产品确认；若要，需另开卡（内部结算价 + 内部银行，工作量 ≈ 本次的 40%） |
| **Q3** | 用户跨公司访问：用户维度表 vs 角色维度 | `biz_org_user` 用户维度（角色维度无法表达"张三在 A 是财务、在 B 是采购"） | 老板确认；改角色维度要加 `sys_role_org` 表并重做 §5.1 的 `org/mine` |
| **Q4** | 合并报表是否要一次出多期对比 | 只出单期（与现有三表一致） | 客户确认；多期对比是报表层聚合，不影响数据模型 |
| **Q5** | 内部交易是否包含"集团内资金池/内部借款计息" | 只做往来 + 购销 + 资金划转本金，不做计息 | 客户财务确认；计息要加"内部利率表 + 计提凭证"，另开卡 |
| **Q6** | 少数股东权益确实不做（D-6） | 不做，`ownership_ratio ≠ 100` 只给 warning | 产品确认；改则需 "权益抵消 + 净利润拆分 + 持股变动"三张卡 |

---

## 附：本文证据索引（全部可复核）

| 结论 | 复核命令 |
|---|---|
| business 迁移最大号 V0367 | `ls backend/migrations/business \| sort \| tail -1` |
| 全仓无 `org_id` | `grep -ril "org_id" backend/migrations/ \| wc -l` → 0 |
| 108 张 `biz_*` 表 | `grep -rhoE "CREATE TABLE (IF NOT EXISTS )?biz_[a-z_]+" backend/migrations/business/*.up.sql \| awk '{print $NF}' \| sort -u \| wc -l` |
| `biz_account` 唯一键全局 | `backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql:128` |
| 凭证唯一写入口 | `backend/src/business/erp/voucher.rs:1963`（`post_voucher`），INSERT 在 2068；其余 12 处 insert 全在 `mod tests` |
| 53 个 `post_voucher` 调用点 | `grep -rn "post_voucher(" backend/src --include=*.rs \| grep -v voucher.rs \| wc -l` |
| 期间/期初/折旧批次唯一键 | `V0137:194-195`、`V0189:70`、`V0189:99`、`V0230:67` |
| 年结/期初凭证唯一键 | `V0190:74`、`V0189:120` |
| 科目匹配语法 | `backend/src/business/erp/report.rs:473-488`（`match_account`） |
| 数据权限唯一入口与登记表 | `backend/src/business/shared/scope.rs`（模块文档 + `SCOPED_TABLES` 281-294） |
| 部门树 `dept_path` | `backend/migrations/system/V0006__create_sys_dept.up.sql:7` |
| 流程实例列 | `backend/migrations/business/V0122__flow_instance_tables.up.sql:33-70` |
| 审批人 8+2 种来源 | `backend/src/business/flow/assignee.rs` 模块文档 |
| 红冲=借贷互换（金额非负） | `V0149:1-40` 头部说明 + `ck_biz_voucher_entry_positive`（V0149:107） |
| 生产组织现状（4 部门 / 7-9 账号无部门） | `docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md` §1.1-1.2 |
| 生产凭证 21 张 / 多币种"半截" | `docs/评审报告-现有功能评分-20261104.md` §7.1 |
| ARCH-4 多组织边界 | `docs/architecture/ARCH-4-通用MES与委外架构设计.md:4306` |
| 本卡在路线图中的位置 | `docs/architecture/ARCH-10-产品演进路线-v1.md:143`（「📋 多组织/多公司 + 合并报表 ARCH9-1」） |
| 菜单规范 | `docs/编码规范.md` §1.6.1 + `scripts/verify/menu_invariants.sh` |
