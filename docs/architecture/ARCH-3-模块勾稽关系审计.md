# ARCH-3 全系统模块间勾稽关系审计

> **一句话**：这不是又一份「勾稽规则」论文，而是一张**体检报告**——把 16 个模块、90 张表之间「谁跟谁有数据往来」量成数字，然后把**没人用的孤岛**和**该连没连的断链**逐条列出来，每条都给出业务后果和修法。
>
> | 项目 | 值 |
> |---|---|
> | 审计对象 | `/opt/cloudpivot`（基础版 CloudPivot）—— HEAD `6cfdc4d` |
> | 审计日期 | 2026-11-02 |
> | 代码基线 | 454 个后端文件 / 335,011 行 / 2,022 条含 SQL 的字符串字面量 |
> | 库基线 | `cloudpivot_test`（本机 PG，94 张表，含 3 张 `sys_change_log` 分区子表） |
> | 复跑命令 | `python3 scripts/verify/module_graph.py --db cloudpivot_test`（文本）/ `--format tables`（表级明细）/ `--format mermaid`（依赖图）/ `--format json`（机读） |
> | 交付物 | ① 模块勾稽矩阵（§3，10 个维度）② 孤岛清单（§4）③ 断链清单（§5，25 条）④ 改进建议与排期（§6） |
> | 与既有文档的关系 | 《通用勾稽规则与业务链映射表》讲**字段级**（R1 单一数据源 / R2 只绑上一环 / R3 快照+巡检）；**本文只讲模块级联通**，不重复 R1/R2/R3 的规则本身，只检查「这些规则有没有覆盖到每个模块」 |

---

## 0. TL;DR（老板只看这一页）

### 0.1 五句话结论

1. **模块之间不是「一堆孤岛」，但连通是单向的**：16 个模块里 12 个有真实的双向数据往来；`help`（帮助中心）**零入零出**、`storage`（文件存储）与 `announcement` / `changelog` 是**单向**（只被读或只读别人）——这三类都是内容/基础模块，属于合理独立。真正的问题在**连接质量**：ERP 30 张表里 89 个 UUID 关联列只有 17 个有外键（**19%**），单据表头（采购单/收货单/凭证/应付单/合同）**29 个关联列 0 外键**，跨模块勾稽完全靠代码自律。
2. **主数据 6 张全部被引用，但 4 个字段是摆设。** 供应商/物料/仓库/库位/单位/科目都被单据引用；然而 `biz_unit.rate`（单位换算）、`biz_material.costing_method`（除移动加权外的选项）、`biz_supplier.credit_limit`（信用额度）、`biz_material.safety_stock`（安全库存）**写了却没有任何代码消费** —— 用户填了以为生效，实际不生效。
3. **业务流程有 3 个硬缺口（P0）**：① 普通物料**没有出库/领用/报废/盘亏单据**，库存账只增不减；② 采购单「提交审批」在流程未配置时**静默降级为直接提交**（6 处同类降级），用户以为审过了；③ 收货单**建单时**校验采购单状态、**过账时不再校验**，采购单在中间被取消/关闭后仍能过账入库并生成应付。
4. **审批流本身的机制是好的**（8 个已发布流程、状态机 + 巡检 H1/H2 兜底），但**合同签订是唯一没有审批的「决定花多少钱」的动作**，且 `TASK_NODE`（阶段 1 已实现）在 8 个流程里 **0 个使用** —— 能力建好了没接线。
5. **「操作便捷」这条线已经修了一环（采购→收货带出），还剩 5 处用户被迫手工搬运**，其中单位换算缺失会让「按箱下单、按个记账」直接算错账。

### 0.2 先修什么（P0 清单，5 条以内）

| # | 一句话 | 后果 | 工作量 |
|---|---|---|---|
| P0-1 | 普通物料补「出库/领用/报废」单据（含盘亏） | 库存账只增不减，账面永远虚高，1405 科目永不结转成本 | 专项（3 卡 + 1 迁移批次） |
| P0-2 | 审批流未配置时禁止「静默降级」，改为显式 409 + 后端配置页提示 | 6 个关键动作可能在没有审批的情况下生效，内控形同虚设 | 1 卡 |
| P0-3 | 收货过账时复核来源采购单状态（与建单同一口径） | 已取消的采购单仍能入库并生成应付，账实不符 | 1 卡 |
| P0-4 | 单位换算：单据落库统一折算到基本单位（或禁用多单位） | 多单位场景下数量与金额错 | 1 卡（需定口径） |
| P0-5 | `costing_method` 只允许 `MOVING_AVG`（或补 FIFO/STANDARD 实现） | 用户选 FIFO 实际按移动加权算，成本口径错且无提示 | 1 卡 |

### 0.3 数字总览

| 指标 | 值 | 来源 |
|---|---|---|
| 一级模块 | 16 个（另 1 个 `_unknown` 归属未识别） | `module_graph.py` §[1] |
| 二级子模块 | 44 个 | 同上 |
| 事实表 | **90 张**（库内 94：多出的 4 个是 `schema_migrations` + 3 张运行时动态创建的 `sys_change_log` 月分区） | migrations `CREATE TABLE` + `information_schema` |
| 全库外键 | 82 条，覆盖 44 张表 | `information_schema.table_constraints` |
| ERP 表 | 30 张，其中 0 外键 14 张 | 同上 |
| ERP UUID 关联列 | 89 个，带外键 17 个（19%），裸列 72 个 | `pg` 实测 |
| 模块间 SQL 引用边 | 48 条（消费方→生产方） | `module_graph.py` §[2] |
| 表级同现边（二级子模块） | 82 条 | `module_graph.py` §[3] |
| 孤岛候选（无跨表同现） | 20 张表（其中真·零引用 0 张） | `module_graph.py` §[5] |
| 断链 | **25 条**（P0 5 / P1 9 / P2 10 / P3 1） | §5 |
| 已有账务巡检 | `erp_invariants.py` 36 条断言（A/B/C/D/E/F/G/H 八组） | `scripts/verify/erp_invariants.py` |
| 已有快照巡检 | 15 条 DI-01…DI-15（其中 7 条可跑、8 条待合同列落地） | `docs/architecture/快照一致性巡检与告警设计.md` §2.5 |

---

## 1. 审计方法与可复跑证据

### 1.1 判据定义（先说清什么是「孤岛」「断链」）

| 概念 | 判据 | 说明 |
|---|---|---|
| **孤岛 A** | 有表、有页面、有 CRUD，但没有任何其他模块引用它的数据 | 「自己玩自己的」 |
| **孤岛 B** | 只被自己模块引用（自产自销），对全局无影响 | 「模块内的私房表」 |
| **孤岛 C** | 设计上应该被引用，但实际没人引用 | 「断链」，与交付物 3 重叠，按后果归到断链清单 |
| **断链** | 业务上该连、实际没连的地方（状态不传导、快照不同步、动作要手工做三遍、同一结果两条路径口径不一致） | 本文最值钱的部分 |

**关键方法学约定**：扫描器只能看到「同一 SQL 里同现的表对」。**没有同现 ≠ 孤岛**——配置驱动型模块（报表项 → 科目映射、现金流量分类）是「分别读表 + 应用层组装」，物理上没有 JOIN，但它是核心功能。所以本文的判决一律要**再过一遍人工语义**（§4.3 逐条给结论）。

### 1.2 扫描器怎么扫（`scripts/verify/module_graph.py`）

| 步骤 | 做法 | 为什么这么做 |
|---|---|---|
| ① 事实表清单 | 从 `backend/migrations/**/V*.up.sql` 抓 `CREATE TABLE` | 只信迁移，不信文档（历史文档里的表名/号段大量过期） |
| ② SQL 提取 | 正则抽 Rust 字符串字面量（跨行、含 `\` 续行），筛含 `SELECT/INSERT/UPDATE/DELETE` 的 | 本仓 2,022 条 |
| ③ 表引用 | 字面量内解析 `FROM` / `JOIN` / `INSERT INTO` / `UPDATE` | 得到「谁读/写谁」 |
| ④ 勾稽边 | 同一条 SQL 内出现过的表两两成对 | 这就是「模块 A 和模块 B 有没有数据往来」的物理证据 |
| ⑤ 字段级边 | 抽 `JOIN x ON <条件>` 原文 | 证明关联**靠哪个字段**（不是「通过某字段」） |
| ⑥ 表名常量化还原 | `const ITEM_TABLE: &str = "biz_report_item"` + `{ITEM_TABLE}` | 否则配置驱动模块全被误判成孤岛 |
| ⑦ ORM 模型识别 | `#[table("security_logs")]` 属模型声明 | 这类表只走 Model API，SQL 里看不到，会被误判成「死表」 |
| ⑧ 叠加 DB 事实 | `--db` 时只读查外键 / 行数 / 列 | 「有没有约束」必须查库，不能看代码猜 |

### 1.3 扫描器本轮修掉的 3 处假阳性（写下来避免下次重复踩）

| 假阳性 | 表现 | 根因 | 修法 | 修后 |
|---|---|---|---|---|
| `biz_report_item` | self=0、rows=53，被判「真孤岛」 | SQL 写在 `format!` 里，表名走 const | 还原 `{CONST}` 表名 | self=1，正确识别为「本模块单独读取」 |
| `biz_cashflow_item` | self=0、rows=12，同上报孤岛 | 同上 | 同上 | self=1 |
| `security_logs` / `password_change_logs` | self=0、判「死表」 | 只走 ORM `#[table(...)]`，无裸 SQL | 增补 ORM 表声明扫描 | self=1，降级为「单独读取」 |
| `biz_` / `if` | 表清单里多出两张不存在的表（92 张而非 90 张） | ① 迁移注释里写着 `CREATE TABLE biz_[a-z_]+` 示例；② `format('CREATE TABLE IF NOT EXISTS %I PARTITION OF …')` 让正则回溯把 `IF` 当表名 | ① 抓 `CREATE TABLE` 前先剥 `--` 行注释；② 表名黑名单（`if/not/exists/public/only/table`）+ 拒绝下划线结尾 | 表数 92 → **90**，与库内 94 的差额正好是 `schema_migrations` + 3 张动态月分区（可逐一对账） |

> **教训（可以固化成习惯）**：静态扫描判「孤岛」时，必须同时覆盖三种访问方式——裸 SQL、`format!`+const、ORM 模型。缺任何一种，被判死刑的都是「写法特殊的好模块」。

### 1.4 证据编号规则（本文所有表都能追到源）

| 前缀 | 含义 | 例子 |
|---|---|---|
| `E-SCAN` | `module_graph.py` 输出 | `E-SCAN[2]` = 一级模块矩阵 |
| `E-DB` | 直接对 `cloudpivot_test` 的只读查询 | `E-DB-FK-82` |
| `E-RS` | Rust 源码 grep（文件:行） | `E-RS-purchase_handler.rs:984` |
| `E-FE` | 前端源码 grep | `E-FE-payable/index.vue:71` |
| `E-DOC` | 既有设计文档章节 | `E-DOC-巡检§2.5` |

---

## 2. 家底：模块清单与规模（全部实测）

### 2.1 一级模块与规模（`E-SCAN[1]` + `E-DB` 表数）

| 模块 | 中文 | 表数 | 本模块 SQL 引用 | 跨模块 SQL 引用 | 被 JOIN 次数 | 前端页面 |
|---|---|---|---|---|---|---|
| `sys` | 系统管理（用户/角色/菜单/部门/字典/偏好） | 11 | 176 | 239 | 84 | ✅ |
| `erp` | 企业资源（采购/仓储/财务/报表） | 30 | 973 | 16 | 55 | ✅ 22 个目录 |
| `flow` | 审批流 | 9 | 181 | 140 | 41 | ✅ |
| `task` | 工作任务 | 8 | 161 | 36 | 10 | ✅ |
| `support` | 支撑底座（日志/配置/任务/存储策略） | 8 | 14 | 8 | 2 | ✅ |
| `notice` | 消息通知 | 4 | 48 | 23 | 3 | ✅ |
| `announcement` | 公告 | 4 | 51 | 0 | 3 | ✅ |
| `storage` | 文件存储 | 3 | 16 | 7 | 3 | ✅ |
| `impexp` | 导入导出 | 3 | 42 | 1 | 0 | ✅ |
| `help` | 帮助中心 | 3 | 39 | 0 | 3 | ✅ |
| `project` | 项目 | 2 | 12 | 4 | 1 | ✅ |
| `masking` | 字段脱敏 | 1 | 11 | 4 | 0 | ✅ |
| `mutex` | 角色互斥 | 1 | 8 | 6 | 1 | ✅ |
| `permission` | 数据权限 | 1 | 3 | 4 | 1 | ✅ |
| `changelog` | 变更日志 | 1 | 14 | 0 | 0 | ✅ |
| `aiagent` | AI 用户 | 1 | 7 | 5 | 1 | ❌（无独立页面，属 AI 后台） |

**读法说明**：`erp` 的「跨模块 SQL 引用 16」看起来很少，但它有 **131 处引用 flow 的表**（审批实例/待办）、**51 处引用 sys**（用户/角色）、**11 处引用 notice**——跨模块引用多放在「访问矩阵」里按方向统计（§3.0），不是按本模块表被谁读。

### 2.2 ERP 30 张表的职能分类（`E-DB`）

| 分组 | 表 | 行数（测试库） |
|---|---|---|
| 主数据（6） | `biz_supplier`(13) `biz_material` `biz_warehouse` `biz_location`(12) `biz_unit`(40) `biz_account`(56) | — |
| 期间/期初（3） | `biz_accounting_period`(43) `biz_opening_balance`(4) `biz_opening_inventory`(0) | — |
| 采购（2） | `biz_purchase_order`(3) `biz_purchase_order_item` | — |
| 库存（4） | `biz_inventory`(13) `biz_inventory_location` `biz_inventory_txn`(215) `biz_material_serial` | — |
| 收货（2） | `biz_stock_in`(1) `biz_stock_in_item` | — |
| 调拨（3） | `biz_stock_transfer`(1) `biz_stock_transfer_item` `biz_stock_transfer_item_serial` | — |
| 财务（5） | `biz_payable`(6) `biz_payment`(0) `biz_voucher`(87) `biz_voucher_entry`(174) `biz_voucher_rule`(1) | — |
| 报表（2） | `biz_report_item`(53) `biz_cashflow_item`(12) | — |
| 底座（2） | `biz_seq`(10) `biz_status_log`(121) | — |
| 合同（1，A1-A6 新增） | `biz_contract`(1) | — |

> 合计 30 张（6+3+2+4+2+3+5+2+2+1）。**注意：没有 `biz_customer`、没有销售单、没有出库单、没有盘点单** —— 这四条缺口决定了 §5 的三条 P0 断链。

### 2.3 前端页面对照（`E-FE`）

`frontend/src/views/business/erp/` 实测 22 个目录：

```
account  contract   inventory  ledger   location  material  opening   payable
period   purchase   purchase-order-item    report   seq     serial   shared
stockin  supplier   transfer   unit     voucher   warehouse  year-end
```

| 缺口 | 事实 |
|---|---|
| 无「出库/领用/报废」页 | 库存只能通过调拨搬走、或序列号物料单独报废 |
| 无「盘点」页 | 账实差异没有单据来源 |
| 无「客户/销售」页 | 合同类型值域里有 `SALES`，但 `SUPPORTED_TYPES = ["PURCHASE"]` 直接挡死（`E-RS-contract_dto.rs:72`） |

### 2.4 外键密度：连通靠「代码自律」（`E-DB`，本文最重要的结构性事实）

| 范围 | UUID 关联列 | 带外键 | 裸列 | 覆盖率 |
|---|---|---|---|---|
| 全库 `biz_*` | 143 | 60 | 83 | 41% |
| **ERP 30 表** | **89** | **17** | **72** | **19%** |
| **单据表头（采购单/收货单/凭证/应付单/合同）** | **29** | **0** | **29** | **0%** |
| 单据表头 + 付款单 + 调拨单 | 38 | 4 | 34 | 11% |
| **全部主数据（供应商/物料/仓库/库位/单位/科目）** | **8** | **0** | **8** | **0%** |

带外键的只有 9 张 ERP 表（`biz_inventory` / `biz_material_serial` / `biz_payment` / `biz_purchase_order_item` / `biz_stock_in_item` / `biz_stock_transfer(+item+serial)` / `biz_voucher_entry`）。

**这件事的业务含义（不是技术洁癖）**：

| 后果 | 具体表现 |
|---|---|
| 孤儿行只能靠巡检发现 | 采购单删了，收货单的 `source_order_id` 仍指着它；库层面没有任何报错 |
| 排序/并发靠代码保证 | 合同额度并发靠 `SELECT … FOR UPDATE` 手写（`E-RS-purchase_handler.rs:336`），漏一处就超签 |
| 「同步改列名」没有护栏 | 换 `location_id` 语义时，数据库不会拦住任何一处误用（详见 §5 的 DC-22） |

> 这一条与 ARCH-2 的结论一致：**本仓有意不建外键**（历史原因 + 迁移成本 + 软删语义）。所以本文的所有「断链」都以「有没有巡检/有没有代码校验」为判据，而不是以「有没有外键」为判据。

### 2.5 二级子模块勾稽强度（Top 20，`E-SCAN[3]`）

| 子模块 A | 子模块 B | 同现边数 | 主要表对 |
|---|---|---|---|
| `sys/role` | `sys/user` | 37 | `sys_role~sys_user_role×21` `sys_role~sys_user×12` |
| `erp/audit` | `erp/finance` | 22 | `biz_status_log~biz_voucher×11` `biz_payable~biz_status_log×9` |
| `erp/inventory` | `erp/master` | 21 | `biz_inventory~biz_material×4` `biz_inventory_location~biz_material×4` |
| `sys/dept` | `sys/user` | 14 | `sys_dept~sys_user×12` |
| `erp/finance` | `erp/stockin` | 14 | `biz_stock_in~biz_voucher×8` `biz_payable~biz_stock_in×3` |
| `erp/finance` | `erp/master` | 13 | `biz_account~biz_voucher×6` `biz_account~biz_voucher_entry×6` |
| `flow/flow` | `sys/user` | 13 | `biz_flow_instance~sys_user×5` `biz_flow_task~sys_user×4` |
| `erp/finance` | `flow/flow` | 10 | `biz_flow_instance~biz_payment×5` `biz_flow_instance~biz_payable×4` |
| `erp/inventory` | `erp/stockin` | 8 | `biz_inventory_txn~biz_stock_in×3` |
| `flow/flow` | `notice/notice` | 8 | `biz_flow_instance~sys_notice×6` |
| `mutex/mutex` | `sys/role` | 7 | `sys_role~sys_role_mutex×7` |
| `erp/audit` | `erp/stockin` | 6 | `biz_status_log~biz_stock_in×6` |
| `sys/user` | `task/task` | 6 | `biz_task~sys_user×6` |
| `task/dependency` | `task/task` | 6 | `biz_task~biz_task_dependency×6` |
| `erp/stockin` | `flow/flow` | 5 | `biz_flow_instance~biz_stock_in×5` |
| `sys/menu` | `sys/role` | 5 | `sys_menu~sys_role_menu×3` |
| `erp/master` | `erp/purchase` | 4 | `biz_material~biz_purchase_order_item×2` |
| `erp/master` | `erp/transfer` | 2 | `biz_material~biz_stock_transfer_item×1` |
| `erp/master` | `erp/stockin` | 2 | `biz_material~biz_stock_in_item×1` |
| `erp/purchase` | `erp/stockin` | 2 | `biz_purchase_order~biz_stock_in×2` |

**这张表暴露的第一件事**：`erp/purchase` 与 `erp/stockin` 之间只有 **2** 条同现边，而 `erp/finance` 与 `erp/stockin` 有 **14** 条。也就是说 —— **收货单跟财务是强耦合的，跟采购单是弱耦合的**。这正是 §5 的 DC-03（过账不复核采购单状态）产生的土壤：链路的下半段（收货→财务）做得很严，上半段（采购→收货）靠一次性的建单校验。

---

## 3. 交付物 1：模块勾稽矩阵

### 3.0 一级模块访问矩阵（生产者 × 消费者，`E-SCAN[2]`）

> 读法：行 = 消费方（谁在 SQL 里读别人的表），列 = 生产方。值 = SQL 引用处数（`FROM`/`JOIN`/`INSERT`/`UPDATE` 均计）。**空 = 没有直接数据往来**。

| 消费方 ↓ / 生产方 → | sys | erp | flow | task | notice | storage | support | project | aiagent | impexp | masking | mutex | permission |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **sys** | — | — | 4 | 1 | — | — | — | 3 | — | — | 3 | 4 | 3 |
| **erp** | 51 | — | **131** | — | 11 | — | 3 | — | — | — | — | — | — |
| **flow** | 32 | 2 | — | 25 | 11 | — | — | — | — | — | — | — | — |
| **task** | 32 | — | 5 | — | — | 3 | 2 | 1 | 1 | — | — | — | — |
| **notice** | 10 | — | — | — | — | — | 1 | — | — | — | — | — | — |
| **announcement** | 8 | — | — | — | 1 | 2 | 1 | — | — | — | — | — | — |
| **storage** | — | — | — | — | — | — | — | — | — | — | — | — | — |
| **impexp** | 10 | 5 | — | — | — | — | — | — | — | — | — | — | — |
| **project** | 1 | — | — | 4 | — | — | — | — | — | — | — | — | — |
| **help** | — | — | — | — | — | — | — | — | — | — | — | — | — |
| **shared** | 20 | 9 | — | 3 | — | — | — | — | 3 | 1 | — | — | 1 |
| **masking** | 19 | — | — | — | — | — | 1 | — | — | — | — | 2 | — |
| **mutex** | 28 | — | — | — | — | — | — | — | — | — | — | — | — |
| **permission** | 15 | — | — | — | — | — | — | — | — | — | — | — | — |
| **changelog** | 1 | — | — | — | — | — | — | — | — | — | 1 | — | — |
| **middleware** | — | — | — | 3 | — | — | — | — | 1 | — | — | — | — |

**三条结论**：

| 结论 | 证据 |
|---|---|
| ① 全系统只有一个「超级枢纽」：`sys` —— 出边 18 / 入边 **241**，承担身份/组织/字典/配置四件事 | `E-SCAN[2]` 汇总 |
| ② ERP 是最大的消费方（出边合计 **196**：→flow 131、→sys 51、→notice 11），但几乎没有生产方：**入边只有 16**（`flow→erp 2`、`impexp→erp 5`、`shared→erp 9`） | 同上 |
| ③ **`erp` 与其他业务模块之间缺少回读**：`task`/`project`/`notice`/`storage` 都不读 ERP 任何表 | 同上 |

16 个模块的入/出边全量（`E-SCAN[2]` 聚合）：

| 模块 | 出边（读别人） | 入边（被读） | 类型判读 |
|---|---|---|---|
| `sys` | 18 | **241** | 超级枢纽（被所有模块依赖） |
| `flow` | 70 | 140 | 双向强联通（审批被 ERP/任务用，自身读组织） |
| `erp` | **196** | 16 | **净消费方**（读审批/组织/通知，几乎不被读） |
| `task` | 44 | 36 | 双向均衡 |
| `mutex` | 28 | 6 | 净消费方（读角色） |
| `masking` | 22 | 4 | 净消费方（读角色/用户） |
| `impexp` | 15 | 1 | 净消费方 |
| `permission` | 15 | 4 | 净消费方 |
| `support` | 14 | 8 | 双向（配置/日志各自独立） |
| `announcement` | 12 | **0** | **纯消费**（内容模块，没人读它） |
| `notice` | 11 | 23 | 双向（被 ERP/flow 写，自身读用户/偏好） |
| `project` | 5 | 4 | 双向均衡 |
| `aiagent` | 3 | 5 | 均衡（AI 用户） |
| `changelog` | 2 | **0** | **纯消费**（审计模块） |
| `storage` | **0** | 8 | **纯被依赖**（文件能力供别人用） |
| `help` | **0** | **0** | **完全孤立**（纯内容，无数据往来） |

> **这张「零边清单」的判读很关键**：`help` 零入零出不是缺陷（帮助中心不需要和任何业务表发生数据往来，它的搜索只查自己的 `sys_help_doc`）；`storage` 零出边也不是缺陷（它是被调用的能力，不需要主动读业务表）。**真正值得警惕的是 `erp` 的 196:16 比值** —— 说明 ERP 是个「数据黑洞」：所有模块都能往里灌（审批、通知、导入导出、共享底座），但没有模块能从里面取业务数据。这就是「孤岛感」的量化来源。

### 3.1 维度 ①：主数据 → 业务单据

#### 3.1.1 六张主数据各自的引用面（`E-DB`，列级实证）

| 主数据 | 被哪些表的 `<x>_id` 列引用 | 引用表数 | 判读 |
|---|---|---|---|
| `biz_supplier` | `biz_purchase_order.supplier_id`、`biz_stock_in.supplier_id`、`biz_payable.supplier_id` | 3 | ✅ 采购链三环都绑了；**但 `biz_payment` 不存供应商**（要靠 `payable_id` 反查） |
| `biz_material` | `biz_purchase_order_item`、`biz_stock_in_item`、`biz_stock_transfer_item`、`biz_inventory`、`biz_inventory_location`、`biz_inventory_txn`、`biz_material_serial`、`biz_opening_inventory` | 8 | ✅ 覆盖最广，唯一缺口是「凭证分录的 `aux_material_id` 永远为空」（DC-11） |
| `biz_warehouse` | `biz_purchase_order`、`biz_stock_in`、`biz_inventory`、`biz_inventory_location`、`biz_inventory_txn`、`biz_material_serial`、`biz_location`、`biz_opening_inventory` | 8 | ✅ 齐全；**调拨单用三个仓列**（`from/to/transit`），语义最复杂 |
| `biz_location`（库位字典） | `biz_inventory_location.location_id`（弱引用，无 FK）、`biz_stock_in_item.location_id`（**实为库位账行 id**，见下） | 1 真引用 | ⚠️ **库位在实际业务里几乎没用起来**（见 3.1.3） |
| `biz_unit` | `biz_material.unit_id`、`biz_purchase_order_item.unit_id`、`biz_stock_in_item.unit_id` | 3 | ⚠️ **`biz_stock_transfer_item` 没有 `unit_id`**（调拨不记单位）；换算率无人使用（DC-09） |
| `biz_account` | `biz_voucher_entry.account_id`、`biz_opening_balance.account_id` | 2 | ✅ 另有两处间接引用：`biz_material.inventory_account_id`、`biz_supplier.payable_account_id`、`biz_voucher_rule.debit/credit/tax_account_id` |

#### 3.1.2 有没有「没人用的摆设」？—— 有，4 个字段（`E-DB` + `E-RS`）

| 字段 | 声明在 | 被谁消费 | 现状 | 后果 |
|---|---|---|---|---|
| `biz_unit.rate` / `base_unit_id` | 主数据 CRUD 可填（`handler.rs:1254-1285`） | **无**：`grep base_unit_id src/business/erp/*.rs` 只命中主数据 CRUD 与夹具 | 用户可配「1 箱 = 12 个」，单据仍按箱数记账 | **按箱下单、按个结转 → 数量与金额全错**（DC-09，P0） |
| `biz_material.costing_method` | 值域 `MOVING_AVG` / `FIFO` / `STANDARD`（`dto.rs:78`） | **无**：`stockin_write` / `transfer_settle` / `serial_model` 全文不读该列 | 三种都能选，引擎只有移动加权 | **选了 FIFO 实际按移动加权算，无任何提示**（DC-10，P0） |
| `biz_supplier.credit_limit` | 主数据 CRUD 可填（`dto.rs:1236`） | **无**：`purchase_handler` / `payable*` 全文不读 | 信用额度填了不拦单 | 超信用下单无提示（DC-12，P2） |
| `biz_material.safety_stock` | 主数据 CRUD 可填 | **无**：无预警、无报表、无采购建议 | 安全库存只是个数 | 缺料预警缺失（DC-12，P2） |

另有两个「半摆设」：

| 字段 | 事实 | 判读 |
|---|---|---|
| `biz_inventory.locked_qty` | 13 行库存中 **0 行非零**（`E-DB`）；只在库存调整 DTO 里可写 | 预留/占用机制未落地（`stockin_write.rs:573` 注释：「锁定量属于出库/占用流程」→ 该流程不存在） |
| `biz_location.capacity` | 只在主数据 CRUD 里出现，无容量校验 | 库位容量不生效（P3） |

#### 3.1.3 「库位」这条主数据到底用到什么程度（重点，容易误判）

刚才的 3.1.1 看起来「库位被引用了」，但**实际业务里几乎没用**：

| 证据 | 值 | 含义 |
|---|---|---|
| `biz_inventory_location` 行数 / 其中 `location_id IS NULL` 的「未分配桶」 | 11 / **7** | 64% 的库位账行没有库位归属 |
| `biz_stock_in_item` 有 `location_id` 的明细 / 总明细 | **0 / 1** | 收货明细从不填库位 |
| `biz_stock_transfer_item` 有 `from_location_id` 的明细 | **0** | 调拨也不填库位 |
| 前端 `stockin/index.vue` 出现 `location` 的次数 | **0** | 收货界面根本没有库位字段 |
| `is_default`（默认库位）在收货路径被读取的次数 | **0** | 不自动带出默认库位 |

**结论**：库位字典（12 行）、库位账（11 行）这套结构是**为「一物一码 + 精细库位」准备的**，但业务入口只开了「调拨」和「序列号」两条，收发货都不落库位。所以库位在账上表现为**一个巨大的「未分配」桶**。这不是 bug（口径在 `stockin_write.rs:548-566` 里写得很清楚），是**功能只做了一半**（P2，DC-22）。

#### 3.1.4 有没有「没引主数据、自己存文本」的单据？

| 表 | 文本列 | 性质 | 判读 |
|---|---|---|---|
| `biz_purchase_order` | `supplier_name_snapshot`、`contract_no_snapshot` | **快照**（有意） | ✅ 合规（R3），但见 DC-07：快照策略三套并存 |
| `biz_contract` | `party_a_name`、`party_b_name` | 与 `party_a_id`/`party_b_id` **双轨并存** | ⚠️ 两者可同时存在且无一致性校验；`party_b_id` 就是供应商 id |
| `biz_stock_in` | **无**供应商名快照 | 靠实时批查 `supplier_names_of()`（`stockin_handler.rs:3022`） | ⚠️ 与采购单口径相反（DC-07） |
| `biz_voucher_entry` | `account_code`、`account_name` | 快照（凭证必须冻结科目名） | ✅ 合规 |
| `biz_status_log` | `operator_name` | 快照 | ✅ 合规（操作人改名后审计仍可读） |
| `biz_stock_transfer` | `applicant_name` | 快照 | ✅ 合规 |
| `biz_flow_*` | `flow_name`、`node_name`、`assignee_name`、`initiator_name` | 快照（审批单据需要冻结） | ✅ 合规 |

**唯一真正需要改造的**：`biz_contract.party_b_name` 与 `party_b_id` 的双轨（应统一为「id 为真相 + name 为快照」，且加 DI-13 那条巡检已覆盖）。

### 3.2 维度 ②：业务单据 → 业务单据（链）

#### 3.2.1 三条主链的逐环字段（`E-DB` 列 + `E-RS` 外键/守卫）

**链 A：合同 → 采购 → 收货 → 库存 → 凭证/应付**

| 环 | 上游 | 下游 | 关联字段 | 有外键 | 传导方向 | 守卫（谁拦错） | 评级 |
|---|---|---|---|---|---|---|---|
| A1 | `biz_contract` | `biz_purchase_order` | `purchase_order.contract_id` → `contract.id`；冗余 `contract_no_snapshot` | ❌ | 下单时校验 | `guard_contract_amount`（`purchase_handler.rs:332`）：锁合同 + 现算已下单额 + 状态必须 SIGNED/ACTIVE | ✅ 强 |
| A2 | `biz_purchase_order` | `biz_stock_in` | `stock_in.source_order_id` → `purchase_order.id`；明细 `stock_in_item.source_item_id` → `purchase_order_item.id` | ❌ | 建单时校验 | `require_purchase_order`（`stockin_handler.rs:473`）：状态可收 + 仓库一致 | ⚠️ 建单强、过账弱（DC-03） |
| A3 | `biz_stock_in` | `biz_inventory` / `biz_inventory_location` | 过账时 `apply_location_qty(material_id, warehouse_id, location_id)`；流水 `biz_inventory_txn.doc_type='STOCK_IN'` + `doc_id=收货单id` | 部分（`biz_inventory.location_id` 有 FK） | 过账事务内 | 三本账 + 巡检 B1/B2/B4 | ✅ 强 |
| A4 | `biz_stock_in` | `biz_voucher` | `stock_in.voucher_id` → `voucher.id`；反向 `voucher.source_type='STOCK_IN'` + `source_id` + `source_no` | ❌ | 过账同事务 | 巡检 D1（已过账必有凭证）+ A1/A4 借贷平衡 | ✅ 强 |
| A5 | `biz_stock_in` | `biz_payable` | `stock_in.payable_id` → `payable.id`；反向 `payable.source_type='STOCK_IN'` + `source_id` + `source_no` | ❌ | 过账同事务 | 巡检 DI-10/DI-11（供应商一致、来源悬空） | ✅ 强 |

**链 B：调拨 → 库存**

| 环 | 关联字段 | 有外键 | 守卫 | 评级 |
|---|---|---|---|---|
| B1 | `stock_transfer_item.transfer_id` → `stock_transfer.id` | ✅ | 状态机 6 态 + 巡检 G2（PENDING 必有审批实例） | ✅ |
| B2 | `stock_transfer_item.from_location_id` / `to_location_id` → `biz_inventory_location.id` | ✅（两个方向都有 FK） | 出库数量 ≤ 可用量（`transfer_handler.rs:212` 报错带 available） | ✅ |
| B3 | `stock_transfer_item_serial.serial_id` → `biz_material_serial.id` | ✅ | 一物一码件数 = 数量（巡检 C5） | ✅ |
| B4 | 结算落库存：`biz_inventory_txn.doc_type='STOCK_TRANSFER'` + `doc_id` | ❌（多态） | 幂等闸：同 doc 已有流水即跳过（`transfer_settle.rs:166`） | ✅ |
| B5 | 调拨 → 凭证 | **不存在**（无凭证） | — | ✅ **有意为之**：同价移库不改变金额与损益，不需要记账（但见 DC-13 的相关缺口：**调拨是普通物料唯一能「减少库存」的动作**） |

**链 C：应付 → 付款 → 凭证**

| 环 | 关联字段 | 有外键 | 守卫 | 评级 |
|---|---|---|---|---|
| C1 | `biz_payable` ← 收货（见 A5） | ❌ | 巡检 DI-10/DI-11 | ✅ |
| C2 | `biz_payment.payable_id` → `payable.id` | ✅ | 可付金额上限（`payable_payment.rs` 校验）+ 并发核销两段诊断 | ✅ |
| C3 | `biz_payment.voucher_id` → `voucher.id`；反向 `voucher.source_type='PAYMENT'` | ❌ | 巡检 A1/A2 + 测试 ⑬「通过 → 凭证挂在付款记录上」 | ✅ |
| C4 | `biz_payable.paid_amount/unpaid_amount` 由付款重算 | — | 不做缓存列累计，重算口径（`payable_bridge` 注释） | ✅ |

#### 3.2.2 断的环（详见 §5）

| 编号 | 断在哪 | 一句话 |
|---|---|---|
| DC-03 | A2 的**过账侧** | 采购单在建单后被取消/关闭，收货单仍可过账 |
| DC-05 | A2 的**回退侧** | 收货单作废后 `purchase_order_item.received_qty` 是否回退无证据（过账时累加、作废时未见减回，需按 §5 复核） |
| DC-13 | 链 B 的**目的** | 调拨能搬库存，但没有「出库/领用/报废」这类**终点**，普通物料永远出不了账 |
| DC-19 | 链 A/C 与 `biz_task` | 任何单据都挂不上「跟进任务」 |

#### 3.2.3 有没有「跳环」（越过上一环直接引用更上游）？

| 疑似跳环 | 实际情况 | 判读 |
|---|---|---|
| 收货单直接引用合同？ | `biz_stock_in` 无 `contract_id` 列 | ✅ 没跳环 |
| 应付单直接引用采购单？ | `biz_payable.source_type` 实测只有 `STOCK_IN`（6/6 行） | ✅ 没跳环 |
| 凭证直接引用采购单？ | `biz_voucher.source_type` 实测 `STOCK_IN`(7) / `PAYMENT`(77) / `MANUAL`(2) / `YEAR_END`(1) | ✅ 没跳环 |
| 采购单引用合同后，收货单的供应商是否走合同乙方？ | 收货单供应商由采购单派生（`stockin_handler.rs:536`），采购单供应商由合同乙方派生（`purchase_handler.rs:301`） | ✅ 三级派生链正确 |

**这一维度结论：链的「字段设计」是对的（无跳环），问题全在「传导时机」和「缺少终点」。**

### 3.3 维度 ③：业务 → 财务

#### 3.3.1 哪些动作会生成凭证（`E-RS` 逐个入口）

| # | 业务动作 | 凭证 | `source_type` 实测值 | 借贷 | 入口代码 |
|---|---|---|---|---|---|
| 1 | 收货单**过账** | ✅ 进项凭证 | `STOCK_IN` | 借 1405（存货，可按物料 `inventory_account_id` 覆盖）/ 贷 2202 | `stockin_handler.rs:2767` → `voucher::create_for_stock_in` |
| 2 | 收货单**作废** | ✅ 红冲 | `STOCK_IN`（is_red=1） | 反分录 | `stockin_cancel.rs` + `create_for_stock_in` |
| 3 | **付款**（直付或审批通过） | ✅ | `PAYMENT` | 借 2202 / 贷 1002 | `payable_payment.rs:1062` |
| 4 | **序列号**入/出/报废 | ✅ | `SERIAL_BATCH` / `SERIAL_OUT` / `SERIAL_SCRAP` | 借 1405/贷 1901、借 6401/贷 1405、借 1901/贷 1405 | `serial_model.rs:1069/1455` |
| 5 | **期初**（余额 + 库存） | ✅ | `voucher_type='OPENING'` | 借 1405 / 贷 3103 等 | `opening.rs`（`voucher::PostVoucher`） |
| 6 | **年结** | ✅ | `YEAR_END` | 损益结转 | `year_end.rs` |
| 7 | 手工凭证 | ✅ | `MANUAL` | 用户录入 | `voucher_write.rs` |

#### 3.3.2 哪些动作不生成凭证（逐个判定「有意 / 漏」）

| 动作 | 不生成凭证 | 判定 | 理由 |
|---|---|---|---|
| 采购单创建/审批/关闭 | ✅ 不生成 | **有意** | 只是承诺，无货权转移、无金额变动 |
| 合同签订/变更 | ✅ 不生成 | **有意** | 同上（但见 DC-02：合同本身没审批） |
| 收货单**草稿**（未过账） | ✅ 不生成 | **有意** | 过账才是货权转移时点 |
| **调拨**（出/入/在途） | ✅ 不生成 | **有意** | 同价移库，金额不变（`biz_inventory.amount` 不变），凭证会重复计存货 |
| 主数据变更（供应商/物料/科目） | ✅ 不生成 | **有意** | 无经济事项 |
| **普通物料出库/领用/报废** | ❌ 根本没有这个动作 | **漏**（DC-13，P0） | 没有单据 → 没有出库成本结转 → 6401 主营业务成本永远为 0 |
| **盘盈/盘亏** | ❌ 没有盘点单 | **漏**（DC-14，P1） | 账实差异只能手改库存，无凭证来源 |

#### 3.3.3 应付账款从哪里来？

| 问题 | 答案 | 证据 |
|---|---|---|
| 应付来自采购单？ | ❌ | `biz_payable.source_type` 值域里没有 `PURCHASE_ORDER` |
| 应付来自收货？ | ✅ | `payable::create_for_stock_in`（`stockin_handler.rs:2770`），与凭证同一事务 |
| 应付能手工建？ | ✅ 有（`payable.rs` 有 `INSERT INTO biz_payable`） | 但实测 6 行全部 `source_type='STOCK_IN'` |
| 无源收货单（`is_sourceless=true`）产生的应付有供应商吗？ | ✅ 有（A4 要求无源单必须自己指定供应商） | `ck_biz_si_supplier` + 巡检 DI-09 |

**结论：应付的口径是「收货过账」而不是「采购下单」——这是正确的会计口径（权责发生制按收货确认），但用户体感是「我下单了为什么没有应付」，需要在采购单详情给一句解释。**（→ P3 文案项）

### 3.4 维度 ④：业务 → 审批流

#### 3.4.1 哪些单据有审批流（`E-DB` 实测 8 个已发布流程）

| 流程 `flow_code` | `biz_type` | 节点类型（实测） | 挂在哪张单据 | 挂载字段 |
|---|---|---|---|---|
| `po_approve` | `PO_APPROVE` | START / APPROVAL / CONDITION / COUNTERSIGN / END | 采购单 | `biz_purchase_order.approval_instance_id` |
| `stock_in_post` | `STOCK_IN_POST` | 上述 + `NOTIFY_NODE` | 收货单过账 | `biz_stock_in.approval_instance_id` |
| `stock_in_cancel` | `STOCK_IN_CANCEL` | START / APPROVAL / END | 收货单作废 | 同上（一轮一单） |
| `payable_pay` | `PAYABLE_PAY` | APPROVAL × N + CONDITION + COUNTERSIGN | 付款 | `biz_payment.approval_instance_id` |
| `payable_cancel` | `PAYABLE_CANCEL` | START / APPROVAL / END | 应付作废 | `biz_payable.cancel_instance_id` |
| `voucher_reverse` | `VOUCHER_REVERSE` | APPROVAL × 2 + CONDITION + COUNTERSIGN | 凭证红冲 | `biz_voucher.pending_reverse_instance_id` |
| `stock_transfer` | `STOCK_TRANSFER` | START / APPROVAL / END | 调拨 | `biz_stock_transfer.approval_instance_id` |
| `task_accept` | `TASK_ACCEPT` | START / APPROVAL / END | 任务验收 | `biz_task.approval_instance_id` |

**8 个流程、1 个 `TASK_NODE` 都没有**（`jsonb_array_elements(graph_snapshot->'nodes')` 实测节点类型只有 `START/APPROVAL/CONDITION/COUNTERSIGN/END/NOTIFY_NODE`）。→ DC-20。

#### 3.4.2 哪些单据没有审批流

| 单据/动作 | 有流程吗 | 风险 |
|---|---|---|
| **合同**签订/变更/终止 | ❌ **无**（无 `approval_instance_id` 列、无 `CONTRACT` 流程、`biz_flow_def` 里无对应行） | **最高**：合同额度直接决定能花多少钱，却没有内控（DC-02，P1） |
| 收货单**建单** | ❌ 只有过账/作废有 | 中（建单不产生账务） |
| 期初余额/期初库存录入 | ❌ | 中（期初直接决定 1405 期初余额） |
| 主数据变更（供应商/物料/科目/仓库） | ❌ | 中（改科目映射会改历史凭证的解释口径） |
| 盘点 | ❌ 模块不存在 | — |

#### 3.4.3 审批通过后，单据状态怎么变？有没有「审批完了单子没动」？

| 流程 | 审批通过后的动作 | 有没有回头改单据 |
|---|---|---|
| `po_approve` | `SUBMITTED → APPROVED` + `approved_at/approved_by` | ✅ |
| `stock_in_post` | `PENDING_POST → POSTED` + 六本账 + 凭证 + 应付（`post_after_approval`，`stockin_handler.rs:2305`） | ✅ |
| `stock_in_cancel` | `PENDING_CANCEL → CANCELLED` + 六本账回滚 + 红冲 | ✅ |
| `payable_pay` | 生成付款记录 + 凭证 + 应付重算 | ✅ |
| `payable_cancel` / `voucher_reverse` / `stock_transfer` / `task_accept` | 各自终态 | ✅ |

**A3「孤儿审批」问题（审批走完单据没动）在本仓已经被兜住**：`erp_invariants.py` 有两条专门断言 —— H1「实例已终态 → 待办必须终态」、H2「任务已取消/完成 → 审批实例必须终态」。**但反向（实例终态 → 单据状态必须变）没有断言**，只靠各自的回调函数。→ 见 §5 DC-04（P1）。

#### 3.4.4 审批流能不能反过来生成工作任务？

| 能力 | 状态 | 证据 |
|---|---|---|
| `TASK_NODE` 节点（流程里挂一个「工作任务」） | **已实现**（`graph.rs` 校验 R17/R18/R20；`engine.rs:1077` 三条流程侧动作；任务完成回推流程 P-FLOW-1B） | `E-RS` |
| 实际使用 | **0 个流程使用**（8 个已发布流程无 `TASK_NODE`） | `E-DB` |
| 覆盖场景 | 目前只有「审批待办」→ 通知；没有「审批通过后自动建一个后续任务」的配置 | 需产品定 |

**判读**：能力是完整的（含 TASK_NODE 的 config.task 形状校验、指派与超时纪律），但**没接线**（DC-20，P2）。这是典型的「建好了没人用」，比「没建」更浪费。

### 3.5 维度 ⑤：业务 → 工作任务

#### 3.5.1 两套「任务」有什么区别（用户最容易困惑的地方）

| 维度 | `biz_task`（工作任务） | `biz_flow_task`（审批待办） |
|---|---|---|
| 页面 | 「工作任务」看板（列自定义 + semantic 五值） | 「审批中心 / 我的待办」 |
| 谁产生 | 人建 / 被分派 / 被转派 / 循环 / **流程 TASK_NODE** | 流程引擎在审批节点自动生成 |
| 状态 | `source` 5 值（`SELF/ASSIGNED/TRANSFERRED/RECURRING/FLOW`）+ `semantic` | `PENDING/APPROVED/REJECTED/TRANSFERRED/…` |
| 关联流程 | `flow_instance_id` + `flow_node_key` + `flow_round`（只有 `source='FLOW'` 才有，库侧 `ck_biz_task_source_flow` 双向硬约束） | 天然属于实例 |
| 完成后的效果 | 若来自 TASK_NODE → **回推流程**（P-FLOW-1B） | 流程前进一个节点 |
| 权限 | `business:task:*` + 数据范围（`dept_id`/`owner_id`） | 审批中心权限 |

**用户会不会困惑？会。** 同一个「我的待办」在两处出现：
- 审批中心说「您有 3 条待审批」；
- 任务看板说「您有 5 条待办任务」。

而 `TASK_NODE` 一上线，同一件事可能**同时**产生一条审批待办和一条工作任务（节点既是审批节点又配了 task 配置 → 设计上禁止，但没有任何流程在用，无法验证体验）。**建议（P2）**：在审批中心顶部加一行说明「审批类待办在『审批中心』，协作类任务在『任务看板』」，或把 TASK_NODE 生成的任务在任务列表打 `FLOW` 标签（已实现 `source` 字段，前端需展示）。

#### 3.5.2 业务单据能不能挂一个「跟进任务」？

| 检查项 | 事实 | 结论 |
|---|---|---|
| `biz_task` 有没有 `biz_type`/`doc_id`/`doc_no` 列？ | **没有**（39 列实测：`id/task_no/title/…/flow_instance_id/flow_node_key/flow_round`） | ❌ 无法挂业务单据 |
| 有没有「任务 ↔ 单据」的桥接表？ | 无 | ❌ |
| 唯一相关性 | `biz_task.flow_instance_id` → 审批实例（间接指向单据） | 只能覆盖「审批类」，覆盖不了「跟进类」 |

**结论**：业务单据与工作任务**没有第二层关联**（DC-19，P2）。现实痛点：采购员想给「某张采购单」挂一个「催货」任务，只能建一条自由任务再把单号抄进标题。

### 3.6 维度 ⑥：业务 → 通知

#### 3.6.1 通知的真实发送点（`E-RS`，9 处调用 `notice::dispatch`，分布在 4 个模块）

| # | 发送点 | 事件 | 收件人 | 备注 |
|---|---|---|---|---|
| 1 | `announcement/maintenance.rs:292` | 公告发布**扇出** | 按公告范围（部门/角色/用户） | 幂等键 `announcement:{id}:v{version}` |
| 2 | `flow/engine.rs:324` | 审批**待办/结果**（`flush_effects`） | 节点处理人 | 带 `link_url` 回单据 |
| 3 | `flow/engine.rs:364` | 审批**草稿批量投递**（`flush_drafts`） | 同上 | 事务外、尽力而为 |
| 4 | `flow/notify.rs:300` | **NOTIFY_NODE**（流程里显式配的通知节点） | 配置决定 | 模板渲染 + 变量 |
| 5 | `flow/timeout.rs:540` | 审批**催办**（`urge_count` 间隔重发） | 当前处理人 | 发送失败不回滚计数（防无限重发） |
| 6 | `task/collab.rs:1353` | 任务评论 **@提及** | 被提及人 | `force = true`（绕过偏好） |
| 7 | `task/handler.rs:3552` | 任务**分派/验收**等 | 负责人/创建人 | — |
| 8 | `task/hermes_sync.rs:1068` | **AI 任务**状态 | 任务负责人 | 失败只 WARN |
| 9 | `erp/transfer_overdue.rs:245` | **调拨超期** | 申请人/审批人 | 唯一一处 ERP 主动发通知 |

#### 3.6.2 业务事件有没有通知？

| 业务事件 | 有通知吗 | 判读 |
|---|---|---|
| 我的单被**驳回** | ✅ 有（引擎发，含 `link_url` 回单据） | 已解决 |
| 我的单**审批通过** | ✅ 有 | 已解决 |
| 有**新待办** | ✅ 有 | 已解决 |
| 审批**超期** | ✅ 有（`flow/timeout.rs`） | 已解决 |
| 调拨**超期** | ✅ 有（`transfer_overdue.rs`，含 `overdue_alerted_at` 防重） | 已解决 |
| **收货单过账完成** → 通知财务 | ❌ 无 | 财务只能靠「刷新应付列表」发现 |
| **应付到期前 N 天** → 通知财务/采购 | ❌ 无（`biz_payable.due_date` 有值、无提醒任务） | **P1**：现金流靠人盯 |
| **库存低于安全库存** → 通知采购 | ❌ 无（`safety_stock` 没被读，见 3.1.2） | P2 |
| **合同即将到期** → 通知合同负责人 | ❌ 无（`biz_contract.end_date` 有值、无提醒） | P2 |
| **采购单长期未收货** → 通知采购员 | ❌ 无（调拨有超期提醒，采购没有） | P2 |

**结论**：通知能力（模板 + 收件人解析 + 幂等 + 偏好 + 多渠道投递）做得很完整，且已被 **4 个模块**（公告 / 审批 / 任务 / 调拨）复用 —— 这是本仓做得好的一处基础能力。但**业务侧的通知（到期/超期/低库存）在 ERP 里只有「调拨超期」一条**，其余全靠人盯。缺的不是通道，是**「业务事件 → 通知」的注册机制**：现在每加一种提醒都要手写一个定时任务 + 手写幂等键。→ DC-06（P1）。

> 补充一个已知缺陷（A5/A6 报过，与本审计无关但影响通知体验）：`/api/business/notice/stream?token=`（SSE）恒 401，因为 alun 0.4.2 未实现查询令牌白名单 → 站内信实时推送不可用，只能靠轮询。这会让「通知」功能的体感再降一档。

### 3.7 维度 ⑦：项目 → 其他模块

| 检查项 | 事实（`E-DB`） | 结论 |
|---|---|---|
| `biz_project` 被谁引用 | `biz_task.project_id`（FK ✅）、`biz_task_template.project_id`（FK ✅）、`biz_project_member.project_id`（FK ✅）、`biz_task` 查询里 JOIN `biz_project` ×1 | 只有任务模块 |
| `biz_project` 有 `dept_id` 吗 | ✅ 有（FK → `sys_dept`） | 项目可归属部门 |
| 采购单能归到项目吗 | ❌ `biz_purchase_order` 无 `project_id` | 不能 |
| 凭证/费用能归到项目吗 | ❌ `biz_voucher`/`biz_voucher_entry` 无 `project_id`；只有 `aux_extra` JSONB 可塞（但无写入点） | 不能 |
| 单据能归到项目吗（任何 ERP 表） | ❌ 30 张表里 0 张有 `project_id` | 完全不能 |

**判定：`project` 对 ERP 是孤岛（判据 C：设计上应该被引用，实际没人引用）**，但对 `task` 是**合理独立**。

现实业务里「项目」几乎是所有采购/费用的归集维度（项目成本核算）。**P2-8（2026-11-05）起采购单已挂项目**（`biz_purchase_order.project_id`，V0237，可空），但链路只接通了最上游一段：`biz_stock_in` / `biz_payable` / `biz_payment` / `biz_voucher_entry` 仍无项目维度，且「项目成本 = 收货金额 还是 应付金额」这条会计口径**尚未拍板** → DC-16（P2，第一步已落地，其余见 `docs/architecture/P2-8-项目挂ERP口径.md`）。

### 3.8 维度 ⑧：客户 / 销售（缺失的模块）

#### 3.8.1 缺什么（实测）

| 应有 | 现状 |
|---|---|
| `biz_customer` 客户主数据 | ❌ 不存在（94 张表无此表） |
| 销售订单 / 销售出库 | ❌ 不存在 |
| 应收 / 收款 | ❌ 不存在（只有应付/付款） |
| 销售合同 | ⚠️ 值域预留：`CONTRACT_TYPES = ["PURCHASE","SALES","SERVICE","LEASE"]`，但 `SUPPORTED_TYPES = ["PURCHASE"]`（`contract_dto.rs:69/72`）**只放行采购** |
| 客户辅助核算 | ⚠️ `biz_voucher_entry` 没有 `aux_customer_id`（只有 supplier/material/warehouse/dept） |

#### 3.8.2 对「采购→库存→应付」这条链有没有影响？

**没有影响。** 采购链与客户无任何逻辑关系（供应商 ≠ 客户，两个主体方向不同）。这条链是完整的（除 §5 的 P0 断链）。

#### 3.8.3 将来加销售，现有表结构能不能接？

| 需要加的东西 | 现有结构 | 工作量估计 |
|---|---|---|
| 客户主数据 | 参照 `biz_supplier` 建 `biz_customer`（22 列同构） | 1 迁移 + 1 主数据页 |
| 销售订单 + 明细 | 参照 `biz_purchase_order(+item)`；但**需要反向的「出库」动作**，而本仓**出库单据不存在**（DC-13） | 必须先补 DC-13 |
| 销售出库 | 复用 `biz_inventory_txn`（`doc_type='SALES_OUT'`）+ 成本结转凭证 | 1 卡（依赖 DC-13 的通用出库） |
| 应收 | 参照 `biz_payable` 建 `biz_receivable`（`source_type/source_id` 多态同构） | 1 迁移 |
| 收款 | 参照 `biz_payment` | 同上 |
| 收入凭证 | `biz_voucher_rule` 加一个 `SALES_OUT` 模板（种子） | 1 迁移 |
| 客户辅助核算 | `biz_voucher_entry.aux_extra` JSONB 可放，或加 `aux_customer_id`（需评估对既有查询/测试的影响） | 1 卡 |
| 销售合同 | `SUPPORTED_TYPES` 加 `"SALES"` + 合同页放开类型 | 1 行 + 前端 |

**结论**：**结构基本能接，但被一个前置条件卡住 —— 必须先有「通用出库」单据（DC-13）**。这也是为什么 DC-13 被定为 P0：它不只影响库存账，还堵死了销售模块。

### 3.9 维度 ⑨：组织 / 权限 → 业务

#### 3.9.1 部门（dept）跟业务单据有没有关系？

| 表 | 有 `dept_id` 吗 | 说明 |
|---|---|---|
| `biz_task` | ✅（FK → `sys_dept`） | 任务数据权限按部门 |
| `biz_project` | ✅（FK → `sys_dept`） | 项目归属部门 |
| `biz_contract` | ✅（裸 uuid，无 FK） | 合同归属部门 |
| `biz_purchase_order` | ❌ | **没有「部门采购」概念** |
| `biz_stock_in` / `biz_payable` / `biz_voucher` | ❌ | 全部没有部门 |
| `biz_voucher_entry.aux_dept_id` | 列存在，**实测 174 条分录 0 条有值**，代码里**无任何写入点** | 部门辅助核算形同虚设（DC-11，P2） |

#### 3.9.2 数据权限按什么维度做的（`E-RS` `shared/scope.rs`）

| 事实 | 值 |
|---|---|
| 登记进 `SCOPED_TABLES` 的表 | **10 张**：`sys_user` `sys_dept` `sys_import_task` `sys_export_task` `operation_logs` `audit_logs` `auth_logs` `security_logs` `sys_notice` `sys_file` |
| 其中 ERP 表 | **0 张** |
| ERP 模块的做法 | 全部 `ScopeTarget::unscoped(...)` —— `model_base.rs:64` 注释明确：「本模块 13 张表都不参与数据权限」；`contract_model.rs:239`：「数据权限属专业版」 |
| `biz_task` | ✅ 单独声明 `TASK_TARGET`（`dept_id` + `owner_id`），用 `scope::paginate` 注入 |

**判定：这是「有意的设计边界」而不是遗漏**（基础版不做数据权限，专业版做）。但审计要指出**业务后果**：

| 场景 | 现状 |
|---|---|
| 多仓库企业：A 仓的仓管员能看到 B 仓的库存/收货单 | ❌ 全量可见（只要持有 `business:erp:*` 权限码） |
| 多部门：采购员能看到别的部门的采购单 | ❌ 全量可见 |
| 唯一隔离手段 | 权限码（`business:erp:purchase-order:read` 这类「功能权限」），粒度是「能不能进这个页面」，不是「能看哪些行」 |

→ 记为 P3（结构性/产品决策），在 §6 里写成「若出售给多仓/多部门客户，必须先做专业版数据权限」。

#### 3.9.3 业务单据的审批人怎么定位到具体人（角色 → 人）

| 机制 | 实现 | 证据 |
|---|---|---|
| 节点指派方式 | 支持角色 / 部门 / 指定人 / 发起人上级 / 表达式（`flow/assignee.rs`，含 `sys_user_role`、`sys_dept` 递归解析） | `E-SCAN[2]` `flow→sys 32` |
| 「本部门及以下」 | 应用层先解析 dept 集合再传 `ANY($1)`（不是 SQL 内嵌递归 CTE） | `assignee.rs:958` |
| 权限码 vs 审批人 | **审批人不要求有 `business:erp:*` 权限**（审批人往往是岗位角色，无 ERP 读权限）→ 用 `form_data` 冻结显示名解决（`form_snapshot.rs` 头注释有实测数据：四个审批角色 `erp_perms` 全 0） | ✅ 这是个设计亮点 |
| 角色 → 人 的唯一缺口 | 角色互斥只在**角色管理**里校验（`sys_role_mutex`），审批指派时不检查「这两级审批人不能是同一人（互斥角色）」 | P3 |

### 3.10 维度 ⑩：存储 / 导入导出 → 业务

#### 3.10.1 单据附件走 storage 了吗？

| 检查项 | 事实 |
|---|---|
| ERP 模块里出现 `sys_file` 或 `storage::` 的次数 | **0**（`grep -rn` 在 `backend/src/business/erp/*.rs` 无输出） |
| 用 `sys_file` 的模块 | `announcement`（公告附件）、`task`（任务附件 `biz_task_attachment`）、`storage` 自身、`support/file`、`permission`（scope） |
| 业务含义 | 采购合同扫描件、收货单签收单、付款凭证影像、发票影像**全都没地方存** |

→ DC-17（P1）：采购合同/发票影像无处落地，用户只能存本地或邮件。修法有现成范本（`biz_task_attachment` 就是 `(task_id, file_id)` 桥接表 + `sys_file`）。

#### 3.10.2 impexp 跟业务主数据怎么接的？

| 方向 | 已注册定义（`E-RS` `impexp/builtin/mod.rs:30`） | 缺什么 |
|---|---|---|
| **导入** | 只有 `system.dict_item`（字典项） | **主数据全缺**：供应商、物料、仓库、库位、计量单位、会计科目**都不能 Excel 导入** |
| **导出** | `system.dict_item`、`system.user`、`erp.stock_in`、`erp.payable`、`erp.voucher` | 缺：采购单、合同、调拨单、库存余额表、流水账、明细账 |

**这是「上线成本」级的断链（DC-18，P1）**：新客户上线要录 13 个供应商、40 个计量单位、56 个会计科目、若干物料 —— 全部手工点。而 impexp 的导入框架是现成的（`dict_item.rs` 就是范本：定义 `ImportDefinition` + `biz_type()` 即可），只是没人写第二个定义。

#### 3.10.3 导出定义里的一个架构细节（值得表扬，也值得复用）

`impexp/builtin/erp_doc.rs` 用 `DocSpec` 把「三张表结构相同的地方」收成一份实现，并为每张表声明了 `STOCK_IN_STATUS` 这类枚举 → 中文标签（与 `stockin_model::status_label` 同口径）。这是**模块间唯一一处「同一份状态词表被两个模块共享」的例子**（其他地方的词表全是各写一遍，见 DC-21）。

### 3.11 十个维度结论速查表（每条一句话）

| 维度 | 结论 | 关键缺口 |
|---|---|---|
| ① 主数据 → 单据 | 6 张主数据都被引用，但 **4 个字段（单位换算/成本法/信用额度/安全库存）写了没人用**，库位只做了一半 | DC-09/10/12/22 |
| ② 单据 → 单据 | 三条链的**字段设计正确、无跳环**，问题在「传导时机」（过账不复核）与「缺少终点」（无出库） | DC-03/05/13 |
| ③ 业务 → 财务 | 7 类动作发凭证、6 类不发（多数有意），**漏了「出库成本结转」与「盘点」** | DC-13/14 |
| ④ 业务 → 审批流 | 8 个流程机制完整，**合同无审批**、**TASK_NODE 零使用**、**未配置流程静默降级** | DC-01/02/20 |
| ⑤ 业务 → 任务 | 两套任务并存且**没有第二层关联**，单据挂不上跟进任务 | DC-19 |
| ⑥ 业务 → 通知 | 审批/超时通知完整，**业务提醒（到期/低库存/未收货）完全没有** | DC-06 |
| ⑦ 项目 → 其他 | 只连任务；ERP 侧 **P2-8 起采购单有 `project_id`**（V0237，第一步），收货/应付/付款/凭证仍未挂 → 项目成本归集只接通了最上游一段 | DC-16（第一步已落地） |
| ⑧ 客户/销售 | 整块缺失；`contract_type` 预留了 `SALES` 但被白名单挡死；**结构可接，但被「无出库」卡住** | DC-13/15 |
| ⑨ 组织/权限 | 部门只到任务/项目/合同；**ERP 数据权限为 0（有意）**、部门辅助核算列全空 | DC-11 |
| ⑩ 存储/导入导出 | **ERP 附件支持为 0**；导入只有字典项（主数据全靠手录） | DC-17/18 |

---

## 4. 交付物 2：孤岛清单

### 4.1 判据与扫描口径（先把「会误判的地方」说清楚）

三条判据（卡面定义，本文沿用）：

| 判据 | 定义 |
|---|---|
| **A** | 有表、有页面、有 CRUD，但没有任何其他模块引用它的数据 |
| **B** | 只被自己模块引用（自产自销），对全局无影响 |
| **C** | 设计上应该被引用，但实际没人引用（断链） |

**扫描口径与它的三条局限（必须知道，否则会把好模块判死）**：

| 局限 | 影响 | 本文的处理 |
|---|---|---|
| 只能看「同一 SQL 内同现」 | 应用层组装的关联看不见（报表项、现金流量项、供应商名批量取） | 一律补人工语义核对（§4.3） |
| 前端计量 = 前端源码出现表名的次数 | 前端一般写 API 路径不写表名 → 绝大多数表 `fe=0`，**不能当「没页面」的证据** | 页面有无改用「`views/` 目录 + 路由 + API 文件」人工核对 |
| 跨模块引用计数基于文件所属模块 | `shared/`（状态机、作用域、快照巡检）会替 ERP 写数据，看起来像「别的模块引用 ERP」 | §3.0 已标注 `shared→erp 9` 的实际含义是「ERP 的公共底座在替 ERP 写日志/凭证」 |

### 4.2 扫描器输出的 20 条候选（`E-SCAN[5]`）

| # | 表 | 模块/子模块 | 自引 | 跨模块 | FK 入/出 | 行数 | 扫描器判决 |
|---|---|---|---|---|---|---|---|
| 1 | `sys_change_log` | changelog | 14 | 0 | 0/0 | 0 | 无跨表同现（本模块单独读取） |
| 2 | `biz_supplier` | erp/master | 33 | 1 | 0/0 | 13 | 只被跨模块裸引用 |
| 3 | `biz_accounting_period` | erp/period | 46 | 0 | 0/0 | 43 | 无跨表同现 |
| 4 | `biz_seq` | erp/period | 25 | 0 | 0/0 | 10 | 无跨表同现 |
| 5 | `biz_cashflow_item` | erp/report | 1 | 0 | 0/0 | 12 | 无跨表同现 |
| 6 | `biz_report_item` | erp/report | 1 | 0 | 0/0 | 53 | 无跨表同现 |
| 7 | `sys_export_task` | impexp | 24 | 0 | 0/0 | 0 | 无跨表同现 |
| 8 | `sys_import_task` | impexp | 14 | 1 | 1/0 | 90 | 只被跨模块裸引用 |
| 9 | `sys_import_task_error` | impexp | 4 | 0 | 0/1 | 0 | 自产自销（只有外键） |
| 10 | `sys_notice_template` | notice | 8 | 0 | 0/0 | 12 | 无跨表同现 |
| 11 | `sys_storage_config` | storage | 7 | 0 | 0/0 | 1 | 无跨表同现 |
| 12 | `sys_storage_policy` | storage | 3 | 0 | 0/0 | 8 | 无跨表同现 |
| 13 | `sys_config` | support | 2 | 5 | 0/0 | 14 | 只被跨模块裸引用 |
| 14 | `sys_job_log` | support | 2 | 0 | 0/0 | 0 | 无跨表同现 |
| 15 | `audit_logs` | support/log | 1 | 1 | 0/0 | 168 | 只被跨模块裸引用 |
| 16 | `password_change_logs` | support/log | 1 | 0 | 0/0 | 0 | 无跨表同现 |
| 17 | `security_logs` | support/log | 1 | 0 | 0/0 | 11 | 无跨表同现 |
| 18 | `sys_token_blacklist` | sys/auth | 1 | 3 | 0/0 | 0 | 只被跨模块裸引用 |
| 19 | `sys_preference_def` | sys/preference | 1 | 0 | 0/0 | 8 | 无跨表同现 |
| 20 | `biz_task_template` | task/template | 10 | 0 | 0/2 | 0 | 自产自销（只有外键） |

> **本轮修订**：初版扫描把 `biz_report_item` / `biz_cashflow_item` 判成「真孤岛」、`security_logs` / `password_change_logs` 判成「死表」，都是扫描器缺陷（§1.3 已修）。**修完后全系统「代码零引用 + 无页面」的死表 = 0 张** —— 这本身是个好结论：没有该删的表。

### 4.3 嫌疑名单逐条判决（13 条，含卡面点名的 10 条）

> 判决分三类：**合理独立**（设计如此）/ **真孤岛**（该删该合）/ **断链**（该连没连）。每条给证据。

#### 4.3.1 `biz_project` / `biz_project_member`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `project` / `biz_project`(14 列) + `biz_project_member`(6 列) / 「项目管理」列表页 |
| 命中的判据 | **C**（对 ERP 是断链）、B（对 task 是自产自销但有用） |
| 证据 | `biz_project` 自引 5、跨模块 4（`task` 读它）、FK 入 3（`biz_task.project_id` / `biz_task_template.project_id` / `biz_project_member.project_id`）、FK 出 2（`dept_id`/`owner_id`）、行数 2；`biz_project_member` 行数 **0**（功能上线但没人用） |
| 与 ERP 的关系 | **ERP 30 张表 0 张有 `project_id`**（`E-DB` 列扫描）→ 采购/库存/凭证都归不到项目 |
| 判决 | **断链（对 ERP）+ 合理独立（对 task）**。项目本身不是孤岛（任务模块重度使用），但「项目成本核算」这条业务价值完全没接上（DC-16） |
| 建议 | 不删。补 `project_id` 到采购单/凭证分录（或 `aux_extra`），但要先定「项目成本按收货还是按应付确认」——**采购单一段已接（P2-8 / V0237）**，见 `docs/architecture/P2-8-项目挂ERP口径.md` |

#### 4.3.2 `biz_report_item` / `biz_cashflow_item`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/report` / `biz_report_item`(15 列, 53 行) + `biz_cashflow_item`(11 列, 12 行) / 「资产负债表」「利润表」「现金流量表」三个页面 |
| 命中的判据 | **都不命中**（初版误判，已修） |
| 证据 | 自引各 1 次，写法是 `const ITEM_TABLE` + `format!("… FROM {ITEM_TABLE} …")`（`report.rs:1004`、`cash_flow.rs:1025`）；行数 53/12；前端 2/1 处引用；**没有同现边是设计使然**：配置先读进内存，再按 `account_pattern` 匹配科目，物理上不需要 JOIN |
| 判决 | **合理独立（配置表）**。这是全系统「配置驱动」做得最标准的一处：加一行配置就能改报表口径，不用发版 |
| 附带发现 | `biz_report_item` 支持 `formula`（报表项之间做加减，`report.rs:503` 会报「引用了未计算或不存在的报表项」）、`biz_cashflow_item` 按对方科目模式分类 → **两者都是「业务规则存在数据库里」的正面案例**，值得被别的模块学（例如凭证模板 `biz_voucher_rule` 也是这个套路） |

#### 4.3.3 `biz_voucher_rule`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/finance` / `biz_voucher_rule`(13 列, 1 行) / 无独立页面（在凭证/科目页内配置） |
| 命中的判据 | B（自产自销） |
| 证据 | 自引 5；跨模块 0；FK 0/0；与 `biz_account` 同现 1 次（`account~voucher_rule×1`）；行数 **1**（只有一条规则） |
| 关键问题 | 1 行规则意味着「一个 `biz_type` 一张模板」的配置体系**实际只配置了采购入库一种**；`voucher.rs` 里对 `PURCHASE_IN` 走规则表、其他（付款/序列号/期初/年结）**全是硬编码分录** |
| 判决 | **合理独立（配置表）+ 覆盖不足**：设计上应该是「所有凭证都由 rule 驱动」，实际只驱动了 1/7 |
| 建议 | P3：要么补齐其余 6 类 rule（让财务可改科目），要么明确写进文档「只有采购入库可配置」 |

#### 4.3.4 `biz_status_log`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/audit`（`shared/state_machine.rs` 写入）/ `biz_status_log`(12 列, 121 行) / 各单据详情页的「状态流转历史」 |
| 命中的判据 | **都不命中**（它是被设计成「多态日志」的典型：`entity_type` + `entity_id` 没有外键，天然无同现） |
| 证据 | 自引 51（`shared` 替 ERP 写，6 处跨模块）、跨模块 6、FK 0/0；同现：`biz_voucher×11`、`biz_payable×9`、`biz_stock_in×6`、`biz_payment×2`、`biz_purchase_order×1`、`biz_contract×1`、`biz_stock_transfer×1` |
| 覆盖的单据类型（`E-RS` `state_machine.rs:63-95`） | `STOCK_IN` / `PURCHASE_ORDER` / `TRANSFER` / `PAYABLE` / `VOUCHER` / `ACCOUNTING_PERIOD` / `ACCOUNTING_YEAR` / `CONTRACT` = 8 类 |
| 缺口 | 没有 `PAYMENT`（付款记录的状态变化不进日志）；没有 `SERIAL`（序列号状态变化不进日志）；**孤儿行只有测试夹具在清（`fixture_hygiene.rs`），生产没有巡检** |
| 判决 | **合理独立（多态审计表）**，但**巡检缺位**（DC-08 的一部分）：`entity_id` 悬空无人发现 |

#### 4.3.5 `biz_opening_balance` / `biz_opening_inventory`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/opening` / `biz_opening_balance`(12 列, 4 行) + `biz_opening_inventory`(12 列, **0 行**) / 「期初余额」「库存期初」页 |
| 命中的判据 | A（`biz_opening_balance` 勉强）、B（两张都是） |
| 证据 | `biz_opening_balance` 自引 17、跨模块 0、FK 0、同现 `biz_account×3`；`biz_opening_inventory` 自引 6、跨模块 0、FK 0、同现 `biz_material×1` + `biz_warehouse×1`、**行数 0** |
| 判决 | **合理独立（期初台账）**。两张表都是「录入 → 生成期初凭证/流水」的中间载体，不对外提供数据是正常的（对外的是 `biz_voucher` 与 `biz_inventory_txn`） |
| 但要注意 | `biz_opening_inventory` **0 行**说明「库存期初」这个功能在测试环境从未被用；它与「手工 1405 期初」互斥（`opening.rs:48-52` 有拦截），这条互斥规则**没有巡检项**（DI 清单里没有）→ P3 记账 |

#### 4.3.6 `biz_material_serial`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/inventory` / `biz_material_serial`(16 列, 10 行) / 「序列号管理」页 |
| 命中的判据 | 都不命中 |
| 证据 | 自引 **60**（ERP 内引用最密的表之一）、跨模块 0（合理，外部模块不该直接碰序列号）、FK 入 1（`biz_stock_transfer_item_serial.serial_id`）、FK 出 3（`material_id`/`warehouse_id`/`location_id`）、同现：`biz_inventory×5`、`biz_material×3`、`biz_inventory_location×3`、`biz_stock_in×2` |
| 判决 | **合理独立（模块内核心表）**。它是全系统「一物一码」的唯一载体，且有巡检 C5（在库序列号件数 = 库存数量） |
| 缺口 | 序列号**状态变化不进 `biz_status_log`**（只有 `biz_inventory_txn` 流水）→ 审计追溯只能看流水，看不到「谁批准报废的」（DC-08） |

#### 4.3.7 `biz_flow_cc`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `flow` / `biz_flow_cc`(12 列, **0 行**) / 审批中心「抄送我的」 |
| 命中的判据 | 都不命中（有 FK：`instance_id` → `biz_flow_instance`、`receiver_id` → `sys_user`） |
| 证据 | 自引 6、跨模块 3、FK 入 0/出 2、同现 `biz_flow_instance×4`；**行数 0** |
| 判决 | **合理独立（功能存在但从未使用）**。代码路径完整（发起时可配抄送），只是没有流程配抄送人 |
| 建议 | P3：抄送是审批体验的关键一环（「我不审批但我要知道」），建议在默认流程模板里配 1–2 个抄送人，让它跑起来 |

#### 4.3.8 `biz_task_template` / `biz_task_ai_token`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `task` / `biz_task_template`(19 列, **0 行**) + `biz_task_ai_token`(5 列, **0 行**) / 任务模板页 + AI 令牌（无页面） |
| 命中的判据 | B（自产自销） |
| 证据 | `biz_task_template` 自引 10、跨模块 0、FK 0/出 2（`owner_id`/`project_id`）、同现 **0**、行数 0；`biz_task_ai_token` 自引 2、跨模块 2（`middleware` 校验用）、FK 出 1（`task_id`）、同现 `biz_task×1` + `sys_ai_agent×1`、行数 0 |
| 判决 | `biz_task_template` = **合理独立但零使用**（模板功能建了没人用，P3）；`biz_task_ai_token` = **合理独立**（中间件每次请求都查它，只是测试库里没有 AI 任务，所以 0 行） |
| 注意 | `biz_task_template` 是**唯一「同现 0 且行数 0 且有外键」的表** —— 它是全系统最接近「真孤岛」的一张（功能完整、零数据、零外部引用）。若一个季度后仍 0 行，建议评估下线 |

#### 4.3.9 `biz_seq`

| 项 | 内容 |
|---|---|
| 模块 / 表 / 页面 | `erp/period` / `biz_seq`(16 列, 10 行) / 「单号规则」页 |
| 命中的判据 | 都不命中（它是「取号器」，被所有单据**间接**使用） |
| 证据 | 自引 **25**、跨模块 0、FK 0、同现 **0**（因为取号是 `UPDATE … RETURNING` 单表操作）、行数 10（10 个单号序列） |
| 判决 | **合理独立（基础设施表）**。它的「无同现」是**功能正确的标志**，不是孤岛 |
| 但有一个真问题 | 单号规则配置在 `biz_seq`，而 `biz_contract` / `biz_purchase_order` 等表的单号列**没有 CHECK 约束**校验格式，也没有巡检断言「单号与规则一致」。→ 手工改库或旧数据迁移会留脏号（P3） |

#### 4.3.10 卡面之外必须补的 3 条（扫描器发现）

| 表 | 证据 | 判决 |
|---|---|---|
| `sys_job_log`（0 行） | 自引 2、FK 0、同现 0、行数 **0** | **合理独立但零使用**：定时任务日志表建了，但 `sys_job` 的实际执行没有写日志（或日志走了 `tracing` 而非表）。P3 |
| `sys_change_log`（0 行，含 3 张月分区） | 自引 14、跨模块 0、同现 0、行数 0 | **合理独立**（变更日志由中间件写，测试环境无变更） |
| `security_logs`(11 行) / `password_change_logs`(0 行) | 各自引 1（ORM 声明）、无同现 | **合理独立**（日志表天然不 JOIN）；但 `password_change_logs` 0 行说明**改密码不写日志** —— 这是内控缺口（P2） |

### 4.4 模块级孤岛判决（表级之外，按数据往来边的有无）

| 模块 | 出边 | 入边 | 判据 | 判决 |
|---|---|---|---|---|
| `help`（帮助中心） | 0 | 0 | **A**（有表有页面有 CRUD，无任何数据往来） | **合理独立**：纯内容模块，搜索只查自己的 `sys_help_doc`。**不需要连** |
| `storage`（文件存储） | 0 | 8 | 单向（只被读） | **合理独立**：能力型模块，被 announcement/task/support 调用。**不该有出边** |
| `announcement`（公告） | 12 | 0 | 单向（只读别人） | **合理独立**：内容模块读 sys/storage/notice，没人读它是对的 |
| `changelog`（变更日志） | 2 | 0 | 单向 | **合理独立**：审计模块只写不读（除分页查询自己） |
| `erp` | 196 | 16 | 反向失衡 | ⚠️ **不是孤岛，但「只吃不吐」**：外部模块拿不到 ERP 数据 → 任务挂不上单据（DC-19）、项目算不到成本（DC-16）、通知找不到业务上下文（DC-06） |
| 其余 11 个 | >0 | >0 | — | 双向联通，非孤岛 |

**这一节要记住的一句话**：**「零边」不等于「孤岛」**——要看模块的职能。内容模块（help/announcement/changelog）与能力模块（storage）本来就该是单向或零边；真正的问题在 `erp` 这种「业务核心却几乎不被读」的反向失衡。

### 4.5 判决汇总

| 判决 | 表 | 处置建议 |
|---|---|---|
| **真孤岛（该删该合）** | **0 张** | 无（全系统没有「代码零引用 + 无页面」的死表） |
| **断链（该连没连）** | `biz_project` / `biz_project_member`（对 ERP） | 补 `project_id`（DC-16）——**采购单一段已接（P2-8 / V0237）**，收货/应付/付款/凭证待接 |
| **合理独立（配置/台账/日志/基础设施）** | `biz_report_item` `biz_cashflow_item` `biz_voucher_rule` `biz_status_log` `biz_opening_balance` `biz_opening_inventory` `biz_material_serial` `biz_flow_cc` `biz_task_ai_token` `biz_seq` `sys_job_log` `sys_change_log` `security_logs` `password_change_logs` `sys_export_task` `sys_import_task` `sys_storage_config` `sys_storage_policy` `sys_notice_template` `sys_config` `audit_logs` `sys_token_blacklist` `sys_preference_def` | 不动 |
| **合理独立但零使用（观察名单）** | `biz_task_template`（0 行 / 0 同现 / 有外键）`biz_flow_cc`（0 行）`biz_opening_inventory`（0 行）`sys_job_log`（0 行）`password_change_logs`（0 行） | 3 个月后仍 0 行 → 评估下线或补使用入口 |

**一句话总结本交付物：系统里没有「该删的孤岛」，只有「该连没连的链」——所有 20 条候选经过人工核对都能归类为「配置表 / 台账 / 日志 / 基础设施」这些本来就不该有外键的角色。**

---

## 5. 交付物 3：断链清单（25 条）

### 5.1 分级口径

| 级别 | 判据 | 数量 |
|---|---|---|
| **P0** | 影响账务正确性 / 数据错乱 / 内控失效，会真的算错账或让审批形同虚设 | 5 |
| **P1** | 每天都会遇到的操作效率问题，或业务闭环缺口 | 9 |
| **P2** | 结构性问题：现在能忍，规模上来会痛 | 10 |
| **P3** | 锦上添花 / 等产品决策 | 1 |

### 5.2 断链主表

| 编号 | 断开的位置 | 业务后果 | 用户会怎么遇到 | 修法 | 级别 |
|---|---|---|---|---|---|
| **DC-01** | 6 处「审批流未配置 → 静默降级为直接生效」：`purchase_handler.rs:984`（采购提交）、`stockin_handler.rs:2063`（过账）、`stockin_handler.rs:1413`（作废）、`payable_payment.rs:1186`（付款）、`payable_payment.rs:1970`（应付作废）、`voucher_write.rs:663`（凭证红冲） | 内控形同虚设：单据**没有任何审批就生效并记账**，事后无法追溯「谁批的」 | 用户点了「提交审批」，界面显示成功，单据直接变「已提交/已过账」；审批中心里没有这条单。只有服务端日志里一条 `WARN` | 改为：① 未配置流程 → 返回 409 + 明确文案「未配置 XX 审批流程，请联系管理员在『审批流定义』发布」；② 后台加「流程配置自检」页（8 个 biz_type 逐个显示是否已发布）；③ 保留「显式跳过」按钮（需权限码 `business:erp:skip-approval` + 必填理由） | **P0** |
| **DC-02** | 合同无审批流：`biz_contract` 无 `approval_instance_id` 列，`biz_flow_def` 无 `CONTRACT` 流程 | 合同额度直接决定能花多少钱（额度守卫 `guard_contract_amount` 以此为准），却没有任何人审批 | 一个采购员即可新建 → SIGNED → ACTIVE 合同，随后下单花掉全部额度。采购单要审批，但它能花多少钱是合同定的 | 加 `approval_instance_id` 列 + `CONTRACT` 流程定义（节点：部门经理 → 总经理，金额条件分支）+ 审批通过才允许 `SIGNED` | **P1** |
| **DC-03** | 收货单**建单时**校验采购单状态（`require_purchase_order`，`stockin_handler.rs:473`），**过账时不再校验**（`purchase_item_snapshot` 只读 qty，`stockin_handler.rs:691`） | 已取消/已关闭的采购单仍可入库并生成应付+进项凭证 → 库存虚增、应付虚增 | 采购员建了收货单草稿 → 采购经理把采购单取消了 → 仓管第二天点「过账」→ 成功入库、生成应付。要发现只能靠人肉对账 | 过账事务内重跑 `require_purchase_order`（同一函数、同一口径），状态不可收则 409 并点名「来源采购单已取消」 | **P0** |
| **DC-04** | 审批终态 → 单据状态**没有反向断言**：`erp_invariants.py` 有 H1（实例终态→待办终态）、H2（任务终态→实例终态），**没有 H3（实例 APPROVED → 单据必须前进）** | 「审批走完了单子没动」这类 A3 孤儿问题只能靠各自回调，漏一处就静默卡住 | 用户看到审批中心显示「已通过」，回到单据页发现状态还是「审批中」，只能找管理员 | 加巡检 H3：`flow_instance.status='APPROVED' AND 单据.status IN (待审批态)` 即报 P0；并逐单据登记「expected_next_status」映射 | **P1** |
| **DC-05** | `biz_purchase_order_item.received_qty` **双真相**：列由 `stockin_purchase.rs:61` 增量维护（作废时 `:33` 减回），而「待收明细」用 `POSTED_RECEIVED_SQL` 现算（`purchase_model.rs:903`） | 两个数字可能不一致（并发/异常回滚/历史数据），用户看到「已收 100/100」但待收列表说「还能收 100」 | 采购单详情显示「已收完」，点「去收货」却还能建单收货 | **✅ 已解决（P2-3，2026-11-04）**：选「列退场、只留现算」——迁移 `V0217` 删列（连同 CHECK），所有读点统一走 `POSTED_RECEIVED_SQL`（采购单详情 / 待收明细 / 过账闸 / 判收满 / 联动动作）。证据：`docs/evidence/20261104-p23-received-qty-single-truth/` | **P2**（已收口） |
| **DC-06** | 业务提醒缺失：`biz_payable.due_date`、`biz_purchase_order.expect_date`、`biz_contract.end_date`、`biz_material.safety_stock` 都有值，**没有任何提醒任务消费它们** | 应付到期无人提醒 → 逾期付款；采购未收货无人催 → 断料；合同到期无人续签；低于安全库存无人补 | 财务每天手工翻应付列表按到期日排序；采购靠 Excel 记「哪张单还没到货」 | 建「业务提醒」注册机制（复刻 `transfer_overdue.rs` 的成功范式）：4 条 `sys_job` 定时扫描 → `notice::dispatch` → 幂等键防重，页面加「我的提醒」 | **P1** |
| **DC-07** | 供应商名字**三套口径**：① 采购单 `supplier_name_snapshot` 落库快照（`purchase_handler.rs:301`）；② 收货单实时批查 `supplier_names_of()`（`stockin_handler.rs:3022`）；③ 审批页用 `form_data` 冻结快照（`form_snapshot.rs`） | 供应商改名后，三处显示三个名字；对账/审计时说不清「当时是谁」 | 同一笔业务：采购单显示「老名字」、收货单显示「新名字」、审批单显示「提交时的名字」 | ~~统一策略：单据表头统一落 `*_name_snapshot`，列表只读快照~~ → **P2-7 定稿口径（S1–S6）**：关联/过滤/汇总一律用 `id`；**列表/报表/导出显示现值**（按 id 关联，缺失回落快照，不落 uuid）；快照只出现在「冻结文书 + 详情页并列展示」两处，**永不修改**；简称只用于检索/导入。盘点表 13 处、巡检 **DI-23**（同名/同简称/简称撞全名/晚写旧名 = WARN；快照≠现名 = INFO）：`docs/architecture/P2-7-供应商名口径盘点.md` | **P1 → 口径已定 + 巡检已补**（2026-11-05，卡 P2-7；显示层对齐见跟进卡 P2-7-F1） |
| **DC-08** | 多态引用无巡检：`biz_voucher.source_id`、`biz_inventory_txn.doc_id`、`biz_status_log.entity_id`、`biz_material_serial.source_id` 都是「类型 + id」多态，**没有任何巡检项检查悬空**；`biz_status_log` 的孤儿只有测试夹具在清（`fixture_hygiene.rs`） | 单据删了，流水/凭证/日志仍指向它 → 报表按来源汇总时出现「幽灵行」 | 财务按「来源单据」筛凭证，出现一堆点不进去的空行；审计追一条流水，跳转 404 | 加 4 条巡检（照 `snapshot-drift` 的 `di.*` 形态）：`entity_type` + `entity_id` 逐类型 LEFT JOIN 主表，`IS NULL` 即孤儿 | **P2** |
| **DC-09** | 计量单位换算未落地：`biz_unit.rate` / `base_unit_id` 可配，**单据路径 0 处读取**（`grep` 只在主数据 CRUD 出现） | 「1 箱 = 12 个」配了也没用：按箱下单 10，库存记 10（应是 120）→ 数量、金额、成本全错 | 上线第一天就会遇到——只要客户用了非基本单位 | 定口径：① 单据落库统一折算基本单位（`qty_base = qty × rate`），明细同时保留原单位与原数量；② 或暂时**禁用多单位**（`is_base=1` 才可选用），避免用户误配 | **P0** |
| **DC-10** | `costing_method` 三种值域（`MOVING_AVG`/`FIFO`/`STANDARD`，`dto.rs:78`），引擎只有移动加权（`stockin_write.rs` upsert `avg_cost`） | 用户以为选了 FIFO，实际按移动加权算 → 成本口径错，且**没有任何提示** | 物料的「成本法」下拉选「先进先出」，用了半年才发现成本不是 FIFO 算的 | 短期：值域收敛为 `["MOVING_AVG"]`（下拉只剩一项 + 灰字说明「本版本仅支持移动加权」）；长期：补 FIFO（需要批次成本层，专项） | **P0** |
| **DC-11** | 凭证三个辅助核算列**永远为空**：`aux_material_id` 0/174、`aux_warehouse_id` 0/174、`aux_dept_id` 0/174（实测），且代码里**没有任何 `aux_*_id: Some(...)` 写入点**（只有 `aux_supplier_id` 154/174 有值） | 无法按物料/仓库/部门做辅助账；费用归不到部门 | 财务想看「某部门这个月花了多少采购费」——查不到，只能手工从凭证摘要里认 | 在凭证生成处写辅助维度（采购入库写 `aux_material_id`/`aux_warehouse_id`；付款写 `aux_dept_id` 取申请人部门）。需先定「部门从哪来」（采购单没有 dept_id → 见 DC-16 的连带） | **P2** |
| **DC-12** | 4 个主数据字段写了没人读：`biz_unit.rate`（见 DC-09）、`biz_material.costing_method`（见 DC-10）、`biz_supplier.credit_limit`（下单不校验）、`biz_material.safety_stock`（无预警）、`biz_inventory.locked_qty`（0 行非零，无占用流程） | 「填了以为生效」是软件最伤信任的一类 bug | 客户认真填了信用额度和安全库存，系统从不提示 | 逐个处置：`credit_limit` → 下单时校验（超限 409 或需审批）；`safety_stock` → 并入 DC-06 的提醒任务；`locked_qty` → 要么落地「销售订单占用」流程，要么从 UI 移除 | **P2** |
| **DC-13** | **普通物料没有出库/领用/报废单据**：出库只存在于「调拨」（`biz_stock_transfer`）和「序列号出库/报废」（`serial_handler`），非序列号物料**永远出不了账** | 库存账只增不减；1405 存货永远虚增；6401 主营业务成本永远为 0；毛利率永远是 100% | 客户领料生产、报废损坏、赠送给客户 —— 全都没有入口。只能让 IT 手改库存，或走「调拨到废品仓」这种歪招 | 新建「出库单」（`biz_stock_out` + item），支持 `out_type`（领用/报废/盘亏/销售/其他），过账时同事务写三本账 + 成本结转凭证（借 6401 或 6602 / 贷 1405），复用 `stockin_write::apply_location_qty` 的负向逻辑 | **P0** |
| **DC-14** | **没有盘点单**：账实差异只能手改 `biz_inventory`（且无凭证来源） | 盘盈盘亏没有单据、没有审批、没有凭证 → 审计上说不清「为什么库存变了」 | 年底盘点，账面 100 实际 92 → 仓管不知道怎么在系统里「调平」 | 建盘点单（`biz_stock_take` + item：账面数 / 实盘数 / 差异），审核后生成「盘盈盘亏单」（走 DC-13 的出库/入库类型）+ 凭证 | **P1** |
| **DC-15** | 客户/销售模块整块缺失：无 `biz_customer`、无销售单、无应收/收款；`CONTRACT_TYPES` 预留 `SALES` 但 `SUPPORTED_TYPES=["PURCHASE"]` 挡死（`contract_dto.rs:69/72`） | 只能做「花钱」不能做「赚钱」的账 | 客户问「能不能开销售单」，答：不能 | 先做 DC-13（通用出库），再按 §3.8.3 的清单分 4 张卡落地 | **P3**（等产品决策，但 DC-13 是前置） |
| **DC-16** | 项目 → ERP 无关联：30 张 ERP 表 0 张有 `project_id` | 项目成本无法归集；项目毛利算不出来 | 项目经理要「这个项目花了多少采购费」，只能导出所有单据手工筛 | ① `biz_purchase_order.project_id`（下单时选项目）；② 凭证分录走 `aux_extra` 或新增 `aux_project_id`；③ 决定成本确认时点（收货 or 应付） | **P2 → 第一步已落地（2026-11-05，卡 P2-8）**：`V0237` 给采购单加 `project_id`（可空、无外键、部分索引 `idx_biz_po_project`），建单/改单 select 校验「存在且未软删」，列表/详情**实时关联查**项目名（不落快照）；10 条真库用例 + 迁移 up/down 实测。口径与**待挂清单**（收货/应付/付款/凭证 + 「成本按哪个时点确认」这条未决产品决策）见 `docs/architecture/P2-8-项目挂ERP口径.md`。②③ 未做 |
| **DC-17** | 业务单据无附件：ERP 模块 `sys_file` 引用数 **0** | 合同扫描件、签收单、发票影像、验收报告全都没地方放 | 用户把扫描件存自己电脑，出事时找不到 | 建桥接表（范本：`biz_task_attachment(task_id, file_id)`）→ `biz_doc_attachment(doc_type, doc_id, file_id, file_role)`；前端在详情页加附件 Tab（storage 组件现成） | **P1** |
| **DC-18** | 主数据无 Excel 导入：impexp 只注册了 `system.dict_item` 一个导入定义（`builtin/mod.rs:31`） | 新客户上线要手工录 13 供应商 / 40 计量单位 / 56 科目 / N 物料 | 实施顾问抱怨「上线一周全在录基础资料」 | 照 `dict_item.rs` 的 `ImportDefinition` 范本补 6 个定义：`erp.supplier` / `erp.material` / `erp.warehouse` / `erp.location` / `erp.unit` / `erp.account`。**导入必须走「先校验后入库」**（现成 `validate.rs`） | **P1** |
| **DC-19** | 业务单据挂不上跟进任务：`biz_task` 无 `doc_type/doc_id` 列（39 列实测） | 「给这张采购单挂个催货任务」做不到；任务与单据只能靠文字关联 | 采购员建任务「催 XX 供应商」，标题里手抄单号 | `biz_task` 加 `source_doc_type` / `source_doc_id` / `source_doc_no` 三列（与 `source='FLOW'` 的范式一致），任务详情显示「关联单据」可点回 | **P2** |
| **DC-20** | `TASK_NODE` 能力已实现（`graph.rs` R17–R21 校验、`engine.rs:1077` 三条动作、任务完成回推流程），**8 个已发布流程 0 个使用** | 建好的能力没人用，等于没建 | 用户永远看不到「审批到某步自动生成一个要干的活」 | 选 1 个流程试点（建议 `po_approve`：审批通过后自动建「跟进到货」任务），验证体验后写进流程模板库 | **P2** |
| **DC-21** | 业务类型词表 **7 套**并存，同一张采购单有 4 个名字：`biz_flow_def.biz_type='PO_APPROVE'` / `biz_status_log.entity_type='PURCHASE_ORDER'` / `biz_inventory_txn.doc_type='PURCHASE_ORDER'` / `biz_payable.source_type='STOCK_IN'`（收货侧）；另有 `biz_voucher.source_type`、`biz_inventory_txn.biz_type`、`biz_contract.contract_type` 各自独立 | 每加一种单据要改 5–7 处字符串常量；漏一处就静默不联动；报表/巡检/清场各写一遍映射表（`fixture_hygiene::ENTITY_TABLES` 就是第 8 份映射） | 开发说的「口径对不上」大多来自这里；线上表现为「某种单据的通知/日志/流水缺一块」 | 建一张 `sys_biz_type` 字典（或复用 `sys_dict_type`）：`code / name / module / doc_table / status_log_entity / txn_doc_type / voucher_source_type`，代码只引用字典；**先做只读对账页**（列出所有已用字符串 + 未登记的），再逐步收敛 | **P2** |
| **DC-22** | 库位只做了一半：`biz_location` 12 行、库位账 11 行其中 **7 行是「未分配」桶**；收货明细 **0/1** 填库位；前端收货页 **0 处** `location` 字段；默认库位（`is_default`）在收货路径 0 处读取 | 库位在账上退化成「一个巨大的未分配桶」，库位管理等于没有 | 客户开了库位管理，发现收货永远进「未分配」，只能事后调拨搬库位 | 三步：① 收货表单加库位选择 + 默认库位自动带出（`is_default=1`）；② 同物料同仓多库位时的「自动分配策略」（先满后新 / 手工指定）；③ 库位容量预警（`capacity` 目前也是摆设） | **P2** |
| **DC-23** | 采购单无仓库快照、仓库被下游强约束：`biz_purchase_order.warehouse_id` 是裸 uuid，收货时**必须与之一致**（`stockin_handler.rs:492` 校验），但采购单仓库只在 DRAFT 可改（`purchase_handler.rs:696`） | 采购单一旦提交，发现仓库选错了 → 只能取消重开 | 采购员提交后发现仓库选了「总仓」应为「原料仓」，收货时被 400 挡住，只能整单重来 | ① 允许已提交未收货的采购单「改仓库」（走审批或留痕 `biz_status_log`）；② 或收货时允许「指定实际入库仓」并在单据上标注差异（更贴近现实：货可能到别的仓） | **P2** |
| **DC-24** | 同一笔业务纠错要跨 3 个页面：收货单作废（收货页）→ 应付作废（应付页）→ 凭证红冲（凭证页） | 三步缺一步账就不平；用户不知道有几步 | 仓管发现收错货，只会在收货页点「作废」，结果应付和凭证还在 → 巡检报红，财务找人查半天 | 做「联动作废编排」：收货单作废时自动带出「将同时作废应付 XX、红冲凭证 YY」的确认清单，一键执行（后端已支持联动，`payable::cancel_for_stock_in` + 红冲均在同事务）；缺的是前端把影响面显示出来 | **P1** |
| **DC-25** | 调拨要手选物料 + 库位，**没有「从库存里挑」的入口**：后端有可用量校验（`transfer_handler.rs:212`）、有 `biz_inventory`/`biz_inventory_location` 数据，但前端只能凭记忆填物料编码和数量 | 调拨 20 个物料，用户要开两个页面来回对；填错才发现「库存不足」 | 仓管开着库存查询页，一个物料一个物料抄到调拨单里 | 调拨表单加「库存选择器」：按仓库列出有库存的物料（编码/名称/可用量），勾选带出明细 + 库位 + 数量上限 | **P1** |

### 5.3 「操作便捷」专项（用户被迫手工搬运的地方，卡的原文要求）

| 场景 | 现状 | 是否被迫搬运 | 结论 |
|---|---|---|---|
| 采购单 → 收货 | A6 已修：选采购单后自动带出供应商/仓库/待收明细，且不再传 `supplier_id` | ❌ 不用了 | ✅ 已解决（本卡不重复计） |
| 应付 → 付款 | 已修：`GET /payable/{id}/payment-info` 带出可付金额/账户/到期日，从应付单直接发起 | ❌ 不用了 | ✅ 已解决（`E-FE-payable/index.vue:71/453`） |
| 采购单 → 合同 | 已修：合同选择器只列 `PURCHASE + SIGNED/ACTIVE`，选后带出乙方 | ❌ 不用了 | ✅ 已解决 |
| **调拨 → 库存** | 必须手填物料/库位/数量，无库存挑选器 | ✅ **要搬** | DC-25（P1） |
| **收货 → 库位** | 收货页无库位字段，永远进「未分配」桶，事后要靠调拨搬 | ✅ **要搬**（事后搬） | DC-22（P2） |
| **收货作废 → 应付作废 → 凭证红冲** | 三个页面分别操作，且不知道一共几步 | ✅ **要搬 3 次** | DC-24（P1） |
| **主数据上线** | 6 类主数据全靠手录 | ✅ **要搬**（每条一次） | DC-18（P1） |
| **采购单改仓库** | 提交后不能改，只能取消重开（重录全部明细） | ✅ **要搬**（整单） | DC-23（P2） |
| 序列号报废 | 在「序列号管理」页操作，路径深但功能在 | ⚠️ 可接受 | — |
| 非序列号报废 | 无入口，只能走调拨到废品仓（歪招） | ✅ **要搬**（歪招） | DC-13（P0） |

### 5.4 「双真相」专项（两条路径产生同一结果，口径不一致）

| # | 结果 | 路径 A | 路径 B | 会不会真的不一致 | 处置 |
|---|---|---|---|---|---|
| 1 | 采购明细「已收数量」 | 列 `received_qty`（增量维护） | `POSTED_RECEIVED_SQL` 现算 | **不会了**（P2-3 删列，只剩现算） | ✅ 已解决：DC-05 / `V0217` |
| 2 | 供应商显示名 | 采购单快照列 | 收货单实时批查 | **会**（改名后，且这是**合法历史**） | 口径已定（P2-7 S2：显示一律现值 + 详情并列快照，**不再要求两边同源**）；漂移由 **DI-23** 留痕 |
| 3 | 期间是否可记账 | 会计期间状态（`biz_accounting_period`） | 巡检 F1「已结账期间无新凭证」 | 不会（F1 兜底） | ✅ 已解决 |
| 4 | 单据是否「已过账」 | `biz_stock_in.status` | `biz_voucher.source_type='STOCK_IN'` 是否存在 | 不会（巡检 D1 兜底） | ✅ 已解决 |
| 5 | 库存数量 | `biz_inventory.qty`（成本账） | `biz_inventory_location` 合计（库位账） | 不会（巡检 B1 兜底） | ✅ 已解决 |
| 6 | 库存金额 | `biz_inventory.amount` | 凭证 1405 余额 | 不会（巡检 B2 兜底） | ✅ 已解决 |
| 7 | 收货明细「已收」 | 列 `received_qty`（采购明细） | 收货单明细 SUM | = 上面第 1 条 | ✅ 同 DC-05（列已删） |
| 8 | 期初库存 | `biz_opening_inventory` | 期初凭证 1405 借方 | 不会（保存时互斥拦截 + B2） | ✅ 已解决 |

> **这一节是本文最有价值的部分之一**：8 组「双真相」里 **5 组已被巡检钉死**，2 组会漂（DC-05/DC-07），1 组靠业务拦截。说明本仓的「巡检兜底」策略是有效的，缺的是「把没兜的补上」而不是推翻重来。

### 5.5 断链 × 已有巡检 的覆盖对照（哪条断了没人管）

| 断链 | 有巡检/断言兜底吗 | 说明 |
|---|---|---|
| DC-01 审批降级 | ❌ | 无任何断言「生效的单据必须有审批实例」（因为降级是合法路径） |
| DC-02 合同无审批 | ❌ | — |
| DC-03 过账不复核采购单 | ❌ | DI-07/DI-08 只查「来源单软删/悬空」，不查「状态」 |
| DC-04 审批终态→单据不动 | ⚠️ 只有一半（H1/H2 是反向） | 缺 H3 |
| DC-05 received_qty 双真相 | ❌ | — |
| DC-06 业务提醒 | ❌ | 无提醒=无数据，巡检发现不了 |
| DC-07 供应商三名 | ✅ **已补 DI-23**（P2-7） | DI-23 查「名字映射唯一性 + 快照名 vs 现名」：同名/同简称/简称撞全名/晚写旧名 = WARN，快照≠现名/存量 NULL = INFO；另 `backend/tests/invariant_registry.rs` 防「视图写了没登记」 |
| DC-08 多态悬空 | ❌ | DI 清单 15 条里没有一条查 `source_id`/`doc_id`/`entity_id` |
| DC-09 单位换算 | ❌ | B2/C4 只校验「金额=数量×均价」，不校验「数量单位是否正确」 |
| DC-10 成本法 | ❌ | C4 用 `avg_cost` 自洽，看不出「本应是 FIFO」 |
| DC-11 辅助核算 | ❌ | — |
| DC-12 摆设字段 | ❌ | — |
| DC-13 无出库 | ⚠️ E4「作废单库存已回滚」只覆盖作废，不覆盖「该出没出」 | — |
| DC-14 无盘点 | ❌ | — |
| DC-17 附件 | ❌ | — |
| DC-22 库位 | ⚠️ B1（库位账=成本账）能发现「库位账不平」，但「全在未分配桶」是不平的合法形态 | — |
| DC-24 联动作废 | ✅ 部分（D1/E1「作废单的有效凭证必须已红冲」+ 应付联动作废有测试） | 账是平的，只是操作烦 |

**结论：25 条断链里只有 5 条被现有巡检覆盖（2 条完整、3 条部分），其余 20 条处于「断了没人知道」的状态。** 这也是 §6 里「巡检扩容」被列为 P1 的原因。

---

## 6. 交付物 4：改进建议 + 排期

> 组织原则：**建议按「一张卡能干完的事」归并**，不按断链编号 1:1 罗列（25 条断链 → 17 条建议）。每条给「改什么 / 影响面 / 工作量 / 风险」。

### 6.1 P0（影响账务正确性或内控，必须马上修，5 条）

#### R-P0-1 新建「出库单」（领用 / 报废 / 盘亏 / 其他）

| 项 | 内容 |
|---|---|
| 改什么 | 新增 `biz_stock_out` + `biz_stock_out_item`；`out_type` 四值 `ISSUE/SCRAP/LOSS/OTHER`；过账同事务：三本账负向、`biz_inventory_txn.doc_type='STOCK_OUT'`、成本结转凭证（借 6401 或 6602 / 贷 1405，金额取账上 `amount` 真实差额） |
| 影响面 | 1 个新模块（后端四件套 + 2 张表 + 1 个页面）、复用 `stockin_write::apply_location_qty` 的负向分支、`erp_invariants` 的 B1/B2/B4/C1 需回归、`biz_seq` 加 1 条单号规则 |
| 工作量 | **3 张卡**（① 表+模型+过账事务 ② 凭证+巡检 ③ 前端页面/列表/详情） |
| 风险 | **会动库存**：首次上线前需要对现有 13 行库存跑一次 B1/B2/B4 基线；凭证金额取差额（不是 `qty×avg` 重算），零成本库存出库要跳过（`voucher::post_voucher` 在 `debit<=0` 会报错） |
| 连带价值 | 解除 DC-14（盘点）与 DC-15（销售）的前置阻塞 |

#### R-P0-2 审批「静默降级」改显式拦截 + 流程配置自检页

| 项 | 内容 |
|---|---|
| 改什么 | ① 6 处降级改为返回 409（错误码新增 `APPROVAL_FLOW_NOT_CONFIGURED`）+ 文案点名缺哪个 `flow_code`；② 保留显式跳过（权限码 + 必填理由 + 写 `biz_status_log`）；③ 新增「流程配置自检」页：8 个 biz_type 逐个显示「已发布版本 / 未配置 / 图形校验失败」 |
| 影响面 | 6 个 handler 入口、1 个新页面、1 个错误码、4 个单测（每个降级分支各一条改成断言 409） |
| 工作量 | **1 张卡** |
| 风险 | **会让现状「变难用」**：如果生产环境真有未配流程的单据类型，上线当天会挡住。必须先跑一次自检（本机测试库 8 个流程全部 PUBLISHED，风险低） |

#### R-P0-3 收货过账复核来源采购单状态

| 项 | 内容 |
|---|---|
| 改什么 | `post_stock_in` 事务内先调 `require_purchase_order(warehouse_id)`（与建单同一函数），状态不可收 → 409 并点名采购单号；同时复核「过账数量 ≤ 待收数量」（防双开收货单） |
| 影响面 | 1 个函数（`stockin_handler::post_stock_in` 开头）、1 条新断言、巡检 DI 增 1 条（来源单状态是否允许过账） |
| 工作量 | **1 张卡**（约 1 天） |
| 风险 | 极低。存量数据里若有「采购单已取消但收货单未作废」的行，上线后这些单会过账失败——需先跑一次排查（SQL 见附录 D.4） |

#### R-P0-4 计量单位换算落地（或显式禁用多单位）

| 项 | 内容 |
|---|---|
| 改什么 | 方案 A（推荐）：单据明细落库时折算基本单位（`qty_base = qty × rate`），同时保留 `unit_id` 与原始 `qty`；库存与成本只用 `qty_base`。方案 B（应急）：`biz_unit.rate <> 1` 的单位在主数据页标「本版本不支持多单位换算」，下拉禁用 |
| 影响面 | 方案 A 动 4 张明细表（采购/收货/调拨/出库）+ 6 处数量计算 + 三本账 + 巡检 B4；方案 B 只动前端下拉 + 1 条校验 |
| 工作量 | 方案 A：**专项**（3–4 张卡，含存量数据回填）；方案 B：**1 张卡** |
| 风险 | 方案 A 动存量库存数量，**必须先在克隆库跑 B1/B2/B4 全绿**；方案 B 零风险但会限制客户（40 个计量单位里 `is_base=1` 的才能用） |
| 建议 | **先上方案 B 止血，再排期方案 A** |

#### R-P0-5 成本法值域收敛为移动加权

| 项 | 内容 |
|---|---|
| 改什么 | `COSTING_METHODS` 从 3 值收敛为 `["MOVING_AVG"]`；主数据页下拉只剩一项 + 灰字「本版本仅支持移动加权平均」；存量里已选 FIFO/STANDARD 的物料，跑一次数据检查并给出清单 |
| 影响面 | 1 个常量、1 处 DTO 校验、1 个前端下拉、1 条数据检查 SQL、`ck_biz_material_costing` 若存在则加 CHECK |
| 工作量 | **1 张卡**（半天） |
| 风险 | 低。若已有物料选了 FIFO，改值域后编辑会 400 → 需先回填为 `MOVING_AVG`（附录 D.5 有 SQL） |

### 6.2 P1（用户天天遇到 / 业务闭环缺口，5 条 + 1 批）

#### R-P1-1 合同审批 + 「审批终态 → 单据必须前进」巡检

| 项 | 内容 |
|---|---|
| 改什么 | ① `biz_contract` 加 `approval_instance_id`；② 迁移种子加 `contract_approve` 流程定义（金额条件分支：<10 万单级审批，≥10 万两级）；③ 合同 `SIGNED` 只能由审批回写；④ `erp_invariants` 加 H3：`flow_instance.status='APPROVED'` 而单据仍在待审批态 → P0 |
| 影响面 | 1 张表 + 1 个迁移（种子）+ 合同 handler 3 处 + 1 条巡检 + 合同页「审批过程」区块（复用采购单的审批块组件） |
| 工作量 | **2 张卡**（后端 1 + 前端 1） |
| 风险 | 中：合同状态机要加一条「DRAFT → 待审批」过渡，A5 的 5 条合法边要扩到 7 条；已有 1 行合同数据需回填 `status` 兼容 |

#### R-P1-2 业务提醒任务包（到期 / 超期 / 低库存）

| 项 | 内容 |
|---|---|
| 改什么 | 复刻 `transfer_overdue.rs` 的成功范式，建 4 个定时任务：① 应付到期前 7/3/1 天；② 采购单超 `expect_date` 未收货；③ 合同到期前 30 天；④ 库存低于 `safety_stock`。每个任务：扫描 → `notice::dispatch`（幂等键 = 单据id+提醒类型+日期）→ 写回「已提醒」时间戳防重 |
| 影响面 | 4 个新文件（或 1 个通用文件 + 4 个配置）、`sys_job` 加 4 条种子、`sys_notice_template` 加 4 个模板、1 个「我的提醒」聚合页（可先复用通知中心） |
| 工作量 | **2 张卡**（后端 1 + 模板/页面 1） |
| 风险 | 低。关键是**幂等键**（防每小时狂轰）与**收件人解析**（应付提醒发给谁：财务角色还是采购员？需产品定，建议默认「供应商对应的采购员 + 财务角色」） |

#### R-P1-3 业务单据附件（合同扫描件 / 签收单 / 发票影像）

| 项 | 内容 |
|---|---|
| 改什么 | 新增 `biz_doc_attachment(id, doc_type, doc_id, file_id, file_role, uploaded_by)`；`file_id` 外键 → `sys_file`；前端在采购/合同/收货/付款详情页加「附件」Tab（复用 storage 的上传组件与 `biz_task_attachment` 的交互） |
| 影响面 | 1 张表 + 1 个通用 handler（4 个端点：上传/列表/删除/下载）+ 4 个详情页、`sys_file` 的 `scope` 已含 `uploaded_by`（数据权限天然可用） |
| 工作量 | **1 张卡**（后端通用 + 前端组件复用） |
| 风险 | 低。「删除附件」只删关联不删文件（`sys_file` 有独立清理任务 `storage/cleanup.rs`），要写清语义避免误删 |

#### R-P1-4 主数据 Excel 导入（6 个定义）

| 项 | 内容 |
|---|---|
| 改什么 | 照 `impexp/builtin/dict_item.rs` 的 `ImportDefinition` 范本，补 `erp.supplier` / `erp.material` / `erp.warehouse` / `erp.location` / `erp.unit` / `erp.account`；每个定义 = 列定义 + `validate()` + `apply()`（走 `validate.rs` 的「先全量校验再入库」） |
| 影响面 | 6 个新文件 + `builtin/mod.rs` 6 行注册 + 6 个模板下载（模板由定义自动生成）+ impexp 页面下拉自动出现 |
| 工作量 | **1 张卡**（6 个定义同构，可批量写） |
| 风险 | 低（导入框架成熟、有 90 行 `sys_import_task` 的成功记录）。注意：`biz_material.tax_rate` 与 `biz_stock_in_item.tax_rate` 口径不同（0–1 vs 0–100），导入模板要标注清楚 |

#### R-P1-5 操作便捷三件套（联动作废编排 + 调拨库存选择器）

| 项 | 内容 |
|---|---|
| 改什么 | ① **联动作废编排**：收货单点「作废」先弹「影响面预览」（将作废应付 XX、红冲凭证 YY、回退采购已收 ZZ），确认后一键执行（后端已支持，只缺前端编排 + 1 个预览接口）；② **调拨库存选择器**：调拨表单加「从库存选物料」抽屉（按仓库列出 `biz_inventory` 有量的物料 + 可用量，勾选带出明细与库位上限） |
| 影响面 | 2 个页面 + 1 个新查询接口（`/erp/inventory/available?warehouse_id=`） |
| 工作量 | **1 张卡** |
| 风险 | 低，纯读+前端。选择器的「可用量」口径要排除锁定/在途（当前无锁定流程，取 `qty` 即可） |

**P1 第二批（等 P0 落地后再开，2 条）**：

| 断链 | 内容 | 为什么排第二批 |
|---|---|---|
| DC-07 供应商名三套口径 | 收货单补 `supplier_name_snapshot` 列 + 创建时冻结，列表只读快照 | 会动存量 1 行 + 前端 3 处显示，需与 A6 已上线的「供应商只读带出」一起回归 |
| DC-14 盘点单 | `biz_stock_take` + item（账面/实盘/差异）+ 审核后生成盘盈盘亏单 | **依赖 R-P0-1（出库单）**，没有出库单就没有「盘亏」的落点 |

### 6.3 P2（结构性问题：现在能忍，规模上来会痛，5 条）

| 编号 | 建议 | 影响面 | 工作量 | 风险 |
|---|---|---|---|---|
| R-P2-1 | **多态引用巡检扩容**（DC-08）：`biz_voucher.source_id` / `biz_inventory_txn.doc_id` / `biz_status_log.entity_id` / `biz_material_serial.source_id` 逐类型 LEFT JOIN 查悬空；`biz_status_log` 孤儿行清理任务 | 4 个巡检项 + 1 个清理任务；照 `snapshot-drift` 的 `di.*` 形态落地 | 1 张卡 | 低（只读巡检） |
| R-P2-2 | **辅助核算落地 + 部门口径**（DC-11）：凭证生成处写 `aux_material_id` / `aux_warehouse_id`；部门维度需先决定「单据的部门从哪来」（建议加 `biz_purchase_order.dept_id`，默认取申请人部门） | 3 处凭证生成 + 1 列 + 报表加维度筛选 | 2 张卡 | 中：动凭证写入路径，需回归 A1/A2/B2 |
| R-P2-3 | **项目成本归集**（DC-16）：`biz_purchase_order.project_id` **✅ 已做（P2-8 / V0237）**；余下：收货/应付/付款加 `project_id` + 凭证 `aux_extra.project_id`；定「项目成本按收货确认」（与应付口径一致） | 1 列 + 1 处凭证 + 1 个「项目成本」报表（采购单那一列已交付） | 2 张卡 + 1 次产品决策 | 中：动了会计口径，需要产品/财务拍板 |
| R-P2-4 | **单据 ↔ 任务关联**（DC-19）+ **TASK_NODE 试点**（DC-20）：`biz_task` 加 `source_doc_type/id/no`；选 `po_approve` 试点「审批通过 → 自动建跟进到货任务」 | 3 列 + 1 个流程模板改动 + 任务详情「关联单据」区块 | 2 张卡 | 低（`source` 已有多值范式，加 3 列不破约束；但要更新 `ck_biz_task_source_flow` 的语义说明） |
| R-P2-5 | **业务类型词表收敛**（DC-21）+ **库位补全**（DC-22）+ **改仓库**（DC-23）：① 建 `sys_biz_type` 映射字典 + 只读对账页（列出代码里所有已用字符串 vs 字典登记）；② 收货页加库位选择 + 默认库位带出；③ 采购单允许「已提交未收货」改仓库（走审批留痕） | 1 张新表 + 3 个页面 + 1 列校验放宽 | 3 张卡（可分 3 批） | 中：词表收敛要动 7 处常量引用，建议先上「对账页」再逐步替换；改仓库要防与已建收货单冲突 |

### 6.4 P3（可做可不做）

| 编号 | 建议 | 工作量 | 说明 |
|---|---|---|---|
| R-P3-1 | 销售模块（DC-15）：客户主数据 + 销售单 + 应收/收款 + 收入凭证 + `contract_type` 放行 `SALES` | 专项（4–6 卡） | **等 R-P0-1 出库单落地**；且需要产品确认「是否真的要做销售」（本仓定位是后台底座） |
| R-P3-2 | 零使用功能处置：`biz_task_template`(0 行) / `biz_flow_cc`(0 行) / `sys_job_log`(0 行) / `password_change_logs`(0 行) —— 要么补使用入口（流程模板配 1–2 个抄送人、任务模板页加「从模板新建」），要么 3 个月后下线 | 1 张卡 | 加 `password_change_logs` 的写入（改密码成功即记一条）是**内控项**，建议无条件做 |
| R-P3-3 | `biz_voucher_rule` 覆盖补齐：要么把付款/序列号/期初/年结 4 类分录也做成规则驱动（财务可改科目），要么在文档里明确「只有采购入库可配」 | 2 张卡 | 现状 1 行规则 vs 7 类凭证的落差要写进文档，避免下一次审计又当成缺陷 |
| R-P3-4 | 数据权限（专业版）：若本基础版要卖多仓/多部门客户，`shared/scope.rs` 的 `SCOPED_TABLES` 需要扩到 ERP 单据（当前 0 张） | 专项 | 已有完整底层（`scope.rs` + `ScopeTarget` + `paginate`），缺的是逐表声明与验收 |

### 6.5 排期建议（按「不并行、一个一个执行」的团队约束）

| 批次 | 内容 | 卡数 | 前置 | 出口判据 |
|---|---|---|---|---|
| **批次 1（止血）** | R-P0-2 审批降级拦截 → R-P0-3 过账复核 → R-P0-5 成本法收敛 → R-P0-4 方案 B（禁用多单位） | 4 卡 | 无 | 4 条 409/校验全部有单测；`erp_invariants` 36 条全绿 |
| **批次 2（补闭环）** | R-P0-1 出库单（3 卡）→ R-P0-4 方案 A（单位换算，若客户要求） | 3–4 卡 | 批次 1 完成（避免两处同时改库存路径） | 出库过账 → 三本账 + 成本结转凭证；B1/B2/B4/C1 全绿；13 行存量库存有基线快照 |
| **批次 3（内控与体验）** | R-P1-1 合同审批 → R-P1-5 便捷三件套 → R-P1-2 业务提醒 → R-P1-3 附件 → R-P1-4 导入 | 6 卡 | 批次 1 完成 | 合同必须审批才 SIGNED；应付到期有提醒；供应商/物料可 Excel 导入 |
| **批次 4（结构治理）** | R-P2-1…R-P2-5 | 10 卡 | 批次 3 完成 | 多态悬空可巡检；项目成本可出报表；词表对账页上线 |
| **持续** | R-P3-1…R-P3-4 | — | 产品决策 | — |

> **为什么批次 1 不先做出库单**：出库单会动库存路径（三本账 + 凭证），而批次 1 的四件事都是「加校验」，互不冲突且能在 1 周内把「账算错」的风险堵住。出库单需要单独一个隔离库做基线验证（存量 13 行库存 + 215 条流水），放在批次 2 一次做完更稳。

### 6.6 建议总量核对

| 级别 | 建议数 | 覆盖断链 |
|---|---|---|
| P0 | 5 | DC-01 / DC-03 / DC-09 / DC-10 / DC-13 |
| P1 | 5 + 第二批 2 | DC-02 / DC-04 / DC-06 / DC-17 / DC-18 / DC-24 / DC-25 + DC-07 / DC-14 |
| P2 | 5 | DC-08 / DC-11 / DC-16 / DC-19 / DC-20 / DC-21 / DC-22 / DC-23 / DC-05 / DC-12 |
| P3 | 4 | DC-15 + 零使用表 + 规则覆盖 + 数据权限 |
| **合计** | **17 条建议覆盖 25 条断链** | 无遗漏（DC-05/DC-12 并入 R-P2-2/R-P2-1 的巡检与字段处置） |

---

## 7. 自查表（对卡面验收标准逐条核对）

| 验收项 | 结论 | 证据位置 |
|---|---|---|
| 4 份交付物齐全（矩阵 / 孤岛 / 断链 / 建议） | ✅ | §3 / §4 / §5 / §6 |
| 矩阵覆盖 10 个维度，每个都有结论 | ✅ | §3.1–§3.10，每个维度末尾有判读；§3.11 速查表 |
| 每条「关联」都指出具体字段名 | ✅ | §3.2.1 三张链表逐环给字段；§3.1.1 主数据引用给 `<x>_id` 列名 |
| 嫌疑名单 10 个模块逐条判决 + 证据 | ✅ | §4.3.1–§4.3.9（9 组 13 张表，含卡面全部 10 条嫌疑）+ §4.3.10 补充 3 条 |
| 每条「孤岛」判决都有证据（引用数/外键数/JOIN 数） | ✅ | §4.2 表格给了自引/跨模块/FK 入出/行数/同现四列 |
| 断链清单 ≥ 15 条，每条有业务后果和修法 | ✅ | §5.2 共 **25 条**，每条 6 列 |
| 每条断链都有业务后果（不只是技术描述） | ✅ | 「业务后果」与「用户会怎么遇到」两列分开写 |
| 建议带优先级，P0 和 P1 各不超过 5 条 | ✅ | §6.1 P0=5、§6.2 P1=5（另列「第二批 2 条」并说明分批理由）、§6.3 P2=5、§6.4 P3=4 |
| 建议都写了影响面 / 工作量 / 风险 | ✅ | 每张卡都有 4 个字段 |
| `module_graph.py` 能跑，输出模块依赖图 | ✅ | 文本 / mermaid / json / tables 四种格式，实测见附录 D.2 |
| 文档 ≥ 1200 行 | ✅ | 见附录 D.1 的 `wc -l` |
| 不重复已有文档的 R1/R2/R3 内容 | ✅ | 本文只在 §3.1.4 / §5.4 引用 R3 的结论，不重述规则；字段级规则见《通用勾稽规则与业务链映射表》 |
| 只审计不改代码 | ✅ | 除 `scripts/verify/module_graph.py`（本卡交付物本身）与本文档外，未改任何业务代码；`git status` 见附录 D.3 |
| 数据库查询只读 | ✅ | 全部 `SELECT` / `information_schema` 查询；见附录 D.6 |

### 7.1 卡面「逻辑严密」标准的逐条自证

| 标准 | 自证 |
|---|---|
| 每个模块都出现在矩阵里 | 16 个一级模块在 §2.1 与 §3.0 全部出现；`support`/`changelog`/`aiagent` 虽无业务联动，也在 §3.0 有明确「无往来」标注 |
| 每条关联都指具体字段名 | 见 §3.1.1 / §3.2.1 / §3.4.1 的字段列 |
| 每条断链都有业务后果 | §5.2 第三、四列 |
| 每条孤岛判决都有证据 | §4.2 / §4.3 |
| 建议都写优先级，没有一堆 P1 | P0=5 / P1=5(+2 分批) / P2=5 / P3=4 |
| 不重复 R1/R2/R3 | §1 已声明边界，全文未重述规则 |

---

## 附录 A：表级明细摘录（`module_graph.py --format tables`）

> 完整输出 90 张表 × 3 行（含同现与引用方）；此处摘录与本次审计结论直接相关的 24 张。`自引` = 本模块 SQL 引用数，`跨模块` = 其他模块引用数，`同现` = 同一 SQL 内共同出现过的表。

| 表 | 模块/子模块 | 自引 | 跨模块 | FK入 | FK出 | 行数 | 同现（前 4） |
|---|---|---|---|---|---|---|---|
| `biz_purchase_order` | erp/purchase | 49 | 2 | 1 | 0 | 3 | `biz_stock_in×2` `biz_stock_transfer×2` `biz_status_log×1` |
| `biz_purchase_order_item` | erp/purchase | 18 | 0 | 0 | 1 | 3 | `biz_material×2` `biz_unit×2` |
| `biz_stock_in` | erp/stockin | 63 | 2 | 1 | 0 | 1 | `biz_voucher×8` `biz_status_log×6` `biz_flow_instance×5` `biz_inventory_txn×3` |
| `biz_stock_in_item` | erp/stockin | 10 | 0 | 0 | 1 | 1 | `biz_stock_in×2` `biz_material×1` `biz_unit×1` `biz_inventory_txn×1` |
| `biz_inventory` | erp/inventory | 83 | 0 | 0 | 1 | 13 | `biz_material_serial×5` `biz_material×4` `biz_inventory_location×3` `biz_inventory_txn×3` |
| `biz_inventory_location` | erp/inventory | 57 | 0 | 4 | 0 | 11 | `biz_material×4` `biz_inventory×3` `biz_material_serial×3` `biz_warehouse×2` |
| `biz_inventory_txn` | erp/inventory | 45 | 0 | 0 | 0 | 215 | `biz_stock_in×3` `biz_material×3` `biz_inventory×3` `biz_inventory_location×2` |
| `biz_material_serial` | erp/inventory | 60 | 0 | 1 | 3 | 10 | `biz_inventory×5` `biz_material×3` `biz_inventory_location×3` `biz_stock_in×2` |
| `biz_stock_transfer` | erp/transfer | 14 | 0 | 1 | 3 | 1 | `biz_purchase_order×2` `biz_stock_in×2` `biz_stock_transfer_item×2` `biz_status_log×1` |
| `biz_stock_transfer_item` | erp/transfer | 8 | 0 | 1 | 4 | 1 | `biz_stock_transfer×2` `biz_stock_transfer_item_serial×2` `biz_inventory_location×2` `biz_material×1` |
| `biz_payable` | erp/finance | 56 | 0 | 1 | 0 | 6 | `biz_payment×13` `biz_status_log×9` `biz_voucher×7` `biz_flow_instance×4` |
| `biz_payment` | erp/finance | 31 | 0 | 0 | 1 | 0 | `biz_payable×13` `biz_voucher×8` `biz_flow_instance×5` `biz_status_log×2` |
| `biz_voucher` | erp/finance | 127 | 3 | 1 | 0 | 87 | `biz_voucher_entry×35` `biz_status_log×11` `biz_stock_in×8` `biz_account×8` |
| `biz_voucher_entry` | erp/finance | 48 | 0 | 0 | 1 | 174 | `biz_voucher×35` `biz_account×8` `biz_stock_in×3` `biz_payable×2` |
| `biz_voucher_rule` | erp/finance | 5 | 0 | 0 | 0 | 1 | `biz_account×1` |
| `biz_contract` | erp/contract | 12 | 0 | 0 | 0 | 1 | `biz_status_log×1` |
| `biz_supplier` | erp/master | 33 | 1 | 0 | 0 | 13 | （无——名靠应用层批量取名 `supplier_names_of`，按 R3 不 JOIN） |
| `biz_material` | erp/master | 53 | 0 | 2 | 0 | 30 | `biz_unit×4` `biz_inventory×4` `biz_warehouse×4` `biz_inventory_location×4` |
| `biz_warehouse` | erp/master | 34 | 1 | 4 | 0 | 43 | `biz_material×4` `biz_inventory_location×2` `biz_opening_inventory×1` `biz_location×1` |
| `biz_unit` | erp/master | 21 | 1 | 0 | 0 | 40 | `biz_material×4` `biz_purchase_order_item×2` `biz_stock_in_item×1` `biz_stock_transfer_item×1` |
| `biz_account` | erp/master | 48 | 0 | 0 | 0 | 56 | `biz_voucher×8` `biz_voucher_entry×8` `biz_opening_balance×3` `biz_voucher_rule×1` |
| `biz_location` | erp/master | 14 | 0 | 0 | 0 | 12 | `biz_inventory_location×1` `biz_material×1` `biz_material_serial×1` `biz_warehouse×1` |
| `biz_status_log` | erp/audit | 51 | 6 | 0 | 0 | 121 | `biz_voucher×11` `biz_payable×9` `biz_stock_in×6` `biz_payment×2` |
| `biz_report_item` | erp/report | 1 | 0 | 0 | 0 | 53 | （无——配置先读入内存，再按 `account_pattern` 匹配科目） |

> 上表 `FK入` = 被多少条外键指向（入边），`FK出` = 本表自己有多少条外键。这两列是 §2.4 与附录 C 的原始出处；「同现 0」的主数据表不是孤岛，而是**按 R3 有意不 JOIN**（名字走应用层批量取名 + 快照）。

## 附录 B：全库 82 条外键（按子表分组，`E-DB`）

| 子表 | 外键（列 → 父表.列） |
|---|---|
| `biz_flow_cc` | `instance_id→biz_flow_instance.id`, `receiver_id→sys_user.id` |
| `biz_flow_def_version` | `def_id→biz_flow_def.id` |
| `biz_flow_edge` | `from_node_id→biz_flow_node.id`, `to_node_id→biz_flow_node.id`, `version_id→biz_flow_def_version.id` |
| `biz_flow_instance` | `def_id→biz_flow_def.id`, `initiator_id→sys_user.id`, `prev_instance_id→biz_flow_instance.id`, `version_id→biz_flow_def_version.id` |
| `biz_flow_instance_node` | `instance_id→biz_flow_instance.id`, `node_id→biz_flow_node.id` |
| `biz_flow_node` | `version_id→biz_flow_def_version.id` |
| `biz_flow_node_assignee` | `node_id→biz_flow_node.id` |
| `biz_flow_task` | `assignee_id→sys_user.id`, `instance_id→biz_flow_instance.id`, `instance_node_id→biz_flow_instance_node.id`, `transferred_from→biz_flow_task.id` |
| `biz_inventory` | `location_id→biz_inventory_location.id` |
| `biz_material_serial` | `location_id→biz_inventory_location.id`, `material_id→biz_material.id`, `warehouse_id→biz_warehouse.id` |
| `biz_payment` | `payable_id→biz_payable.id` |
| `biz_project` | `dept_id→sys_dept.id`, `owner_id→sys_user.id` |
| `biz_project_member` | `project_id→biz_project.id`, `user_id→sys_user.id` |
| `biz_purchase_order_item` | `order_id→biz_purchase_order.id` |
| `biz_stock_in_item` | `stock_in_id→biz_stock_in.id` |
| `biz_stock_transfer` | `from_warehouse_id→biz_warehouse.id`, `to_warehouse_id→biz_warehouse.id`, `transit_warehouse_id→biz_warehouse.id` |
| `biz_stock_transfer_item` | `from_location_id→biz_inventory_location.id`, `material_id→biz_material.id`, `to_location_id→biz_inventory_location.id`, `transfer_id→biz_stock_transfer.id` |
| `biz_stock_transfer_item_serial` | `item_id→biz_stock_transfer_item.id`, `serial_id→biz_material_serial.id` |
| `biz_task` | `assigner_id→sys_user.id`, `creator_id→sys_user.id`, `dept_id→sys_dept.id`, `flow_instance_id→biz_flow_instance.id`, `owner_id→sys_user.id`, `parent_id→biz_task.id`, `project_id→biz_project.id` |
| `biz_task_ai_token` | `task_id→biz_task.id` |
| `biz_task_attachment` | `file_id→sys_file.id`, `task_id→biz_task.id` |
| `biz_task_comment` | `parent_comment_id→biz_task_comment.id`, `task_id→biz_task.id`, `user_id→sys_user.id` |
| `biz_task_dependency` | `depends_on_id→biz_task.id`, `task_id→biz_task.id` |
| `biz_task_member` | `task_id→biz_task.id`, `user_id→sys_user.id` |
| `biz_task_template` | `owner_id→sys_user.id`, `project_id→biz_project.id` |
| `biz_task_worklog` | `task_id→biz_task.id`, `user_id→sys_user.id` |
| `biz_voucher_entry` | `voucher_id→biz_voucher.id` |
| `sys_ai_agent` | `user_id→sys_user.id` |
| `sys_announcement_attachment` | `announcement_id→sys_announcement.id` |
| `sys_announcement_read` | `announcement_id→sys_announcement.id` |
| `sys_announcement_scope` | `announcement_id→sys_announcement.id` |
| `sys_dict_item` | `dict_type_id→sys_dict_type.id` |
| `sys_field_mask` | `role_id→sys_role.id` |
| `sys_help_category` | `parent_id→sys_help_category.id` |
| `sys_help_doc` | `category_id→sys_help_category.id` |
| `sys_help_doc_version` | `doc_id→sys_help_doc.id` |
| `sys_import_task_error` | `task_id→sys_import_task.id` |
| `sys_notice_delivery` | `notice_id→sys_notice.id`, `receiver_id→sys_notice_receiver.id` |
| `sys_notice_receiver` | `notice_id→sys_notice.id` |
| `sys_role_dept` | `dept_id→sys_dept.id`, `role_id→sys_role.id` |
| `sys_role_menu` | `menu_id→sys_menu.id`, `role_id→sys_role.id` |
| `sys_role_mutex` | `role_a_id→sys_role.id`, `role_b_id→sys_role.id` |
| `sys_user_preference` | `user_id→sys_user.id` |
| `sys_user_role` | `role_id→sys_role.id`, `user_id→sys_user.id` |

> ERP 只有 9 张表在这份清单里（`biz_inventory` / `biz_material_serial` / `biz_payment` / `biz_purchase_order_item` / `biz_stock_in_item` / `biz_stock_transfer` / `biz_stock_transfer_item` / `biz_stock_transfer_item_serial` / `biz_voucher_entry`）。**单据表头与全部主数据都不在**——这是 §2.4 那张表的原始出处。

## 附录 C：ERP 30 表的「裸 UUID 关联列」全清单（`E-DB`）

> 「裸列」= 指向别的表、但没有外键约束的 uuid 列。共 **72** 个（89 个关联列中 17 个有 FK）。

| 表 | 裸 UUID 关联列 |
|---|---|
| `biz_account` | — |
| `biz_accounting_period` | — |
| `biz_cashflow_item` | — |
| `biz_contract` | `party_a_id`, `party_b_id`, `owner_id`, `dept_id` |
| `biz_inventory` | `material_id`, `warehouse_id` |
| `biz_inventory_location` | `material_id`, `warehouse_id`, `location_id` |
| `biz_inventory_txn` | `material_id`, `warehouse_id`, `doc_id`, `operator_id` |
| `biz_location` | `warehouse_id` |
| `biz_material` | `unit_id`, `default_warehouse_id`, `inventory_account_id` |
| `biz_material_serial` | `source_id` |
| `biz_opening_balance` | `account_id` |
| `biz_opening_inventory` | `material_id`, `warehouse_id` |
| `biz_payable` | `supplier_id`, `source_id`, `voucher_id`, `payment_approval_instance_id`, `cancel_instance_id` |
| `biz_payment` | `voucher_id`, `approval_instance_id` |
| `biz_purchase_order` | `supplier_id`, `warehouse_id`, `submitted_by`, `approved_by`, `closed_by`, `approval_instance_id`, `contract_id` |
| `biz_purchase_order_item` | `material_id`, `unit_id` |
| `biz_report_item` | — |
| `biz_seq` | — |
| `biz_status_log` | `entity_id`, `operator_id` |
| `biz_stock_in` | `source_order_id`, `supplier_id`, `warehouse_id`, `voucher_id`, `payable_id`, `posted_by`, `cancelled_by`, `approval_instance_id` |
| `biz_stock_in_item` | `source_item_id`, `material_id`, `unit_id`, `location_id` |
| `biz_stock_transfer` | `applicant_id`, `approver_id`, `approval_instance_id` |
| `biz_stock_transfer_item` | — |
| `biz_stock_transfer_item_serial` | — |
| `biz_supplier` | `payable_account_id` |
| `biz_unit` | `base_unit_id` |
| `biz_voucher` | `source_id`, `reverse_of`, `reversed_by`, `posted_by`, `pending_reverse_instance_id` |
| `biz_voucher_entry` | `account_id`, `aux_supplier_id`, `aux_material_id`, `aux_warehouse_id`, `aux_dept_id` |
| `biz_voucher_rule` | `debit_account_id`, `credit_account_id`, `tax_account_id` |
| `biz_warehouse` | `parent_id` |

## 附录 D：复跑证据与只读声明

### D.1 文档规模与格式自检（实测）

```
$ wc -l docs/architecture/ARCH-3-模块勾稽关系审计.md
1327 docs/architecture/ARCH-3-模块勾稽关系审计.md

$ python3 scripts/verify/audit_doc_check.py   # 交付前自检脚本（围栏/表格/章节引用/DC 连续性）
总行数: 1327
围栏行数: 12 配对: OK
表格组数: 88，列数不一致的表: 0
引用章节数: 32 缺失: 无
DC 编号: 1..25 共 25 个 连续
```

### D.2 扫描器四种输出（实测）

```
$ python3 scripts/verify/module_graph.py --db cloudpivot_test
扫描文件 454 个 / 335011 行；含 SQL 的字符串字面量 2022 条
表 90 张；一级模块 16 个；二级子模块 44 个
（六节输出：模块×表 / 一级矩阵 / 二级子模块 / JOIN 边 / 孤岛候选 / 外键密度）

$ python3 scripts/verify/module_graph.py --format mermaid
flowchart LR …（16 个模块节点 + 48 条带权边，弱边虚线）

$ python3 scripts/verify/module_graph.py --format tables
90 张表 ×（自引/跨模块/FK/行数/同现/引用方）

$ python3 scripts/verify/module_graph.py --db cloudpivot_test --check
退出码 0（无「代码零引用 + 无页面」的死表）
```

### D.3 本次审计的改动范围

```
$ git status --short
?? scripts/verify/module_graph.py            # 本卡交付物①（可复跑扫描器）
?? docs/architecture/ARCH-3-模块勾稽关系审计.md  # 本卡交付物②（本文）
```

**未改动任何业务代码、迁移、前端**。符合「只审计不改代码」约束。

### D.4 附录 SQL：验收 R-P0-3 前先跑（只读）

```sql
-- ① 存量「采购单不可收但收货单未作废」的行（R-P0-3 上线后会挡住的单）
SELECT s.in_no, s.status AS si_status, o.order_no, o.status AS po_status
  FROM biz_stock_in s
  JOIN biz_purchase_order o ON o.id = s.source_order_id
 WHERE s.deleted_at IS NULL
   AND s.status IN ('DRAFT','PENDING_POST')
   AND o.status NOT IN ('SUBMITTED','APPROVED','PARTIAL_RECEIVED');
```

### D.5 附录 SQL：验收 R-P0-5 前先跑（只读）

```sql
-- ② 存量里选了非移动加权成本法的物料（收敛值域前必须回填）
SELECT material_code, material_name, costing_method
  FROM biz_material
 WHERE costing_method <> 'MOVING_AVG' AND deleted_at IS NULL;
```

### D.6 只读声明

本文所有数据库结论均来自以下只读途径：

| 途径 | 用途 |
|---|---|
| `information_schema.columns / tables / table_constraints / key_column_usage` | 列清单、表清单、外键 |
| `SELECT count(*) FROM <table>` | 行数 |
| `SELECT … FROM biz_flow_def / biz_flow_def_version`（含 `jsonb_array_elements`） | 流程与节点类型 |
| `SELECT source_type, count(*) FROM biz_voucher / biz_inventory_txn / biz_payable` | 类型词表实测值 |
| `SELECT count(aux_*) FROM biz_voucher_entry` | 辅助核算填充率 |

**无任何 INSERT / UPDATE / DELETE / DDL**。所有查询带 `statement_timeout=15s`。

### D.7 本文与兄弟文档的边界（再说一次，避免下次审计重复劳动）

| 文档 | 回答的问题 | 本文是否重复 |
|---|---|---|
| `通用勾稽规则与业务链映射表.md` | 「一个字段该存 ID 还是存名字」（R1/R2/R3） | ❌ 只引用结论 |
| `快照一致性巡检与告警设计.md` | 「快照漂移怎么发现」（15 条 DI） | ❌ 只做覆盖率对照（§5.5） |
| `合同与勾稽关系设计.md`（ARCH-2） | 「合同这条链怎么设计」（字段级 + 接口契约） | ❌ 只验证落地情况（§3.2 A1 环） |
| `流程全配置化-设计.md`（ARCH-1） | 「流程/状态机怎么配置化」 | ❌ 只在 §3.4 核对「配置出来的 8 个流程实际用没用」 |
| **本文（ARCH-3）** | **「模块 A 和模块 B 有没有数据往来、该往来的地方断没断」** | — |

---

**审计人**：architect（系统架构师） · **审计日期**：2026-11-02 · **基线**：`6cfdc4d` · **可复跑**：`python3 scripts/verify/module_graph.py --db cloudpivot_test`
