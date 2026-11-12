# ARCH-9 人力资源模块设计（员工档案 / 考勤 / 薪资）

> 卡片：`t_c8e263a6`（ARCH9-2【L-3】）｜上游：评审报告 §5.1 / §6.2「缺 2 个整块能力域：生产制造 与 人力资源」
> 代码基线：`origin/main` = `0628e916`（2026-11-10 实测 `git log --oneline -1`）
> 影响迁移号段：**V0378 起**（现状 business 最大 `V0367__flow_all_role_approval_to_any_multiversion`；**V0368~V0377 已被 ARCH-9-1 预留**，见 `docs/architecture/ARCH-9-多组织与合并报表设计.md` §7.1）
> 本文只做设计：**不含实现代码**（除 DDL 与接口签名示例），不改现有代码，不建迁移文件。
> DDL 已在 PG 18.6 真库跑通（整段事务 + `ROLLBACK`，零残留），证据见 `docs/evidence/2026-11-10-arch9-2-hr-ddl/`。

---

## 0. 结论前置（16 条定稿决策）

| 编号 | 决策点 | **结论** | 硬依据（可复核） |
|:--:|---|---|---|
| **D-1** | 员工是不是账号 | **不是。分离，关联列放 `biz_employee.user_id`（唯一、可空），`sys_user` 一列都不加** | `sys_user` 只有账号字段（`system/V0001:2-24`：username/password/real_name/email/mobile/dept_id/status…，无任何人事字段）；`sys_user.user_type` 已能表达「非人账号」（`ROBOT`，`system/V0102`）。放 employee 侧的理由见 §4.1.1 |
| **D-2** | 一人几个账号 | **一人一档一账号**（`uk_biz_employee_user` 唯一索引兜死）；无账号员工 `user_id IS NULL`；一个账号不能同时挂两份档案 | 车间工人没账号、外包/机器人有账号非员工是**两个方向的例外**，用「可空 + 唯一」一次表达；多对多会让数据权限「仅本人」二义（`shared/scope.rs` `filter_for` 只按单列比较，见 D-12） |
| **D-3** | 部门归属谁是权威 | **`biz_employee.dept_id` 是人事权威**；`sys_user.dept_id` 保持现状不动（继续喂数据权限与审批人解析）；**调岗动作同时写两处**（同一事务、一个函数） | `sys_dept` 树 + `dept_path`（`system/V0006:7`）被 `scope.rs`（`SYS_USER.dept_col='dept_id'`）与 `flow/assignee.rs` 复用；改动它们的影响面远大于多写一列 |
| **D-4** | 岗位/职级建模 | **一期用字典**（`hr_position` / `hr_job_level` / `hr_education`），不建岗位主数据表 | 岗位主数据要带职级映射、编制、序列，一期客户面窄；字典已是现成能力（`sys_dict_type/item`），二期升级不破坏既有数据（只多一张 `biz_position` 表与一次回填） |
| **D-5** | 合同 | **独立表 `biz_employee_contract`**（一份一行、续签链式），员工表不存合同字段 | 合同有独立生命周期（签/续/解除）与到期提醒；塞进员工表只能留最新一份，"到期前 30 天提醒"与"合同台账"都做不出来 |
| **D-6** | 考勤做多深 | **一期只做「月度考勤汇总 + 请假单审批 + Excel 导入 + 锁定」**；**不做**实时打卡、排班、调休冲抵、加班申请单、考勤机直连 | 打卡需要硬件对接（SDK/中间库/网络专线），与本系统"纯后台"部署模型冲突；而薪资只吃"月度汇总数字"，见 §1.2 |
| **D-7** | 考勤粒度 | **一人一月一行汇总**（`biz_attendance_record`），不做日明细表 | 日明细 100 人 × 30 天 = 3000 行/月且薪资不用它；月度汇总 100 行/月；日明细若将来要，从考勤机导 Excel 保留即可，不占本库 |
| **D-8** | 薪资公式 | **配置化：薪资项表 + 受限表达式 DSL**（手写解析器，不引第三方表达式库） | 「不硬编码流程」原则（`AGENTS.md` 约定 2/4）；依赖铁律只允许 `nik`（`AGENTS.md` 约定 3）；现成的 `report::eval_formula` 只支持 `+`/`-`（`erp/report.rs:496`），复用不了 |
| **D-9** | 社保公积金 / 个税 | **一期：应发算自动、社保公积金与个税手工录**（4 个金额列），单位部分也手工录；**二期**再上城市规则与累计预扣法（§4.5 已给完整公式与二期表 DDL） | 各地社保基数/比例/封顶线每月变，规则库是独立产品级工作量；累计预扣法必须建年度累计台账（`biz_iit_ledger`，二期），一期硬算会在 7 月跳档时算错 |
| **D-10** | 薪资单粒度 | **一公司一期间一张**（`uk_biz_salary_sheet_period (org_id, period_code)`）；明细两层：`detail`（一人一行汇总）+ `detail_item`（一人一行 × 每薪资项，公式解释层） | 财务问的是"这个月工资发了吗 / 发了多少 / 凭证号"——答案在单头；员工问的是"为什么扣这么多"——答案在 item 层。两层都不可省，但汇总恒等式必须在 `detail` 上，才能在库层加 `CHECK` |
| **D-11** | 工资凭证出几张 | **两张：计提（借费用/贷 2211.01~03）+ 发放（借 2211.01 / 贷 1002 + 代扣负债）**；金额为 0 的薪资项**不落分录行**；同一费用科目+部门按**净额**汇总 | `biz_voucher_entry` 有 `ck_biz_voucher_entry_side`（每行必须单边且 `> 0`，`V0149:100-110`），零金额行写不进；计提在月末（权责发生制）、发放在实际发薪日（可能跨月），合一张会让费用与现金挤进同一期间 |
| **D-12** | 数据权限 | 员工/薪资明细**登记进 `SCOPED_TABLES`**（`dept_col=dept_id`,`owner_col=user_id`）；**薪资单头不登记**（单头是 company 级合计，SELF 无意义），只靠权限码 | `scope::paginate` 是唯一分页入口、且 `filter_for` 的 SELF 形态只做 `owner_col = ?`（`shared/scope.rs` filter_for ③）；薪资明细因此必须冗余 `user_id`（由 `employee.user_id` 派生，无账号员工为 NULL） |
| **D-13** | 离职怎么做 | **走审批单 `biz_employee_resign`**；通过后：`status=RESIGNED` + `leave_date` + **账号停用（`status=0`）不删除** + 所有历史单据按 `employee_id` 留存并带姓名/工号快照 | 账号删掉 → 审批轨迹、操作日志、历史单据的"当时是谁"全断（`AGENTS.md` 约定 13 审计类必须存快照）；`biz_status_log` 的 `operator_name` 是快照列（`V0159`） |
| **D-14** | 敏感字段（身份证/银行卡） | **存库明文 + 出参脱敏**（`sys_field_mask` 全局 2 条）+ **明文需 `business:hr:employee:sensitive` 码**；不做列级加密 | `sys_field_mask` 已支持表+列+角色三级、7 种脱敏类型（`V0062`），是现成机制；加密会让「导入查重（按身份证）」「按卡号匹配发放」全部退化 |
| **D-15** | 编号 | 工号 `EMP-0001`（主数据码，永久不重置）；单据号 `LV-yyyyMMdd-000001` / `ATT-yyyyMM-000001` / `SAL-yyyyMM-000001`（`biz_seq`） | 工号是人事档案码，不能按日/按月重置（`erp/seq.rs` 模块文档：主数据码 `date_pattern='none'`、`period_key='GLOBAL'`） |
| **D-16** | 入职/转正要不要审批 | **不要**（HR 直接建档/改状态 + 写状态日志）；**离职、调薪、请假、工资发放要审批** | 入职是数据录入不是资金/资产动作；离职与调薪直接改钱与账号权限，必须过复核（与 `approval_gate` fail-closed 口径一致） |

**明确不做（一期划出边界，写进产品口径）**：

1. **不做实时打卡 / 排班 / 考勤机直连**（D-6）；不做调休自动冲抵（调休只在请假类型里存在，冲抵人工判断）。
2. **不做加班申请单**：加班小时数在考勤汇总里录（Excel 导入），薪资按公式算钱。
3. **不做社保公积金与个税自动计算**（D-9）；不做个税专项附加扣除登记（二期，与 `biz_iit_ledger` 同批）。
4. **不做计件工资**：挂在 ARCH-4（MES 工作量结算）上，一期薪资项只留 `source='MES'` 的钩子（§4.7）。
5. **不做多币种薪资**：工资一律本位币（与 ARCH-9-1 的"合并要求本位币一致"同口径）。
6. **不做招聘 / 培训 / 绩效 / 组织编制**（HR 六个模块里的另外四个）。
7. **不做员工自助工作流**（转正申请、补卡申请）：员工侧只做"我的档案 / 我的薪资 / 我的请假"。

---

## 1. 业务背景与目标

### 1.1 解决什么问题（客户提的 / 竞品都有的 / 不做会丢单）

| 问题 | 现状证据 | 后果 |
|---|---|---|
| 没有人事档案，员工信息散落在账号表 | `sys_user` 只有 `username/real_name/dept_id/mobile/email/status`（`system/V0001:2-24`）；全仓 `grep -ril "biz_employee\|biz_salary\|biz_attendance" backend/migrations/` = **0 个文件** | 入职日期、身份证、学历、合同、紧急联系人、工资卡都没有地方放；客户第一轮演示就会问"你们人事模块在哪" |
| 有员工没账号、有账号不是员工 | `sys_user` 无 `employee_id`，`user_type` 只有 `HUMAN`/`ROBOT`（`system/V0102`） | 车间工人（无账号）无法被"员工"口径统计；外包/系统账号被误算进人头与工资表 |
| 工资靠 Excel，与总账两条线 | `biz_voucher` 的 `source_type` 枚举里没有工资来源（`shared/doc_kind.rs` `VoucherSourceType`：`STOCK_IN`…`Commission` 共 18 值，无 `Payroll`）；`biz_account` 2211 应付职工薪酬**至今没有一张凭证落过账**（`SELECT count(*) FROM biz_voucher_entry WHERE account_code LIKE '2211%'` 实测 **0**） | 每月工资靠会计手工做凭证；"应付职工薪酬"科目长期挂零，资产负债表这一行永远是假的 |
| 竞品都有 | 用友 U8/畅捷通 T+、金蝶 KIS 云均含"人事 + 考勤 + 工资"三件套，且工资凭证自动生成 | 中端客户选型清单里，"能不能发工资/出工资表"是硬指标；只做账不做工资 = 只能卖给已有人事系统的客户 |
| 不做会丢单 | 评审报告 §5.1：「缺 2 个整块能力域：生产制造 与 人力资源」 | 制造业客户（本仓已有生产/委外/库存能力）最需要"人、机、料"三条线，缺人这条线就是缺半条腿 |

### 1.2 不解决什么问题（明确边界）

见 §0 末尾「明确不做」7 条。补充三条技术边界：

1. **不做"人事全模块"**：本设计 = 档案 + 考勤 + 薪资。招聘/培训/绩效/编制不在内。
2. **不做考勤硬件**：不实现打卡、不做设备对接协议、不做指纹/人脸库。考勤的唯一入口是「Excel 导入 + 手工修正 + 请假单审批」。
3. **不做个税申报与社保代缴**：不生成税务申报文件（金税盘/社保网厅接口），只出「工资表 + 代扣金额 + 凭证」。

### 1.3 成功的判据（上线后怎么算成功，逐条可测）

| # | 判据 | 测法 |
|:--:|---|---|
| S1 | 员工与账号的两个方向例外都被表达 | 建一个 `user_id IS NULL` 的员工（车间工人）→ 出现在员工列表、不出现在"可登录用户"统计；建一个 `user_type='ROBOT'` 的账号（无 employee）→ 不出现在员工列表、不进工资单 |
| S2 | 一个月工资 30 分钟出完，且与自己算的对得上 | 100 人客户：导入考勤 → 点"试算" → 逐人核对 `gross/net` 与手工 Excel 一致（误差 0，到分）；差异人可单独改单项金额后重算，改完仍有 `detail_item` 解释 |
| S3 | 过账后总账与工资表对得上 | 薪资单过账后：`biz_voucher` 有两张凭证（`source_type='PAYROLL_ACCRUAL'/'PAYROLL_PAYMENT'`）；`SUM(2211.*贷方) - SUM(2211.*借方)` = 已过账未发放工资；资产负负债表 `PAYROLL_PAYABLE`（映射 2211，`V0183:216`）这一行 = 工资表应发合计 |
| S4 | 员工只能看到自己的工资 | 用普通员工账号 `GET /business/hr/salary/my` → 只有自己一行；`GET /business/hr/salary/detail?page=1&page_size=50` → 403（无 `salary:detail` 码）；同一接口给 HR 账号 → 全量 |
| S5 | 离职后历史可回溯 | 员工离职并通过审批 → 员工列表默认不显示；按"含离职"筛选可见，`leave_date/leave_type` 齐全；他 3 个月前的工资单仍能打开，姓名/工号正常显示（快照）；账号 `status=0` 但登录日志、审批轨迹仍在 |
| S6 | 公式改一次，全公司生效 | 把 `OT` 项的加班倍率从 1.5 改成 2.0（只改一行 `formula`）→ 下次试算金额变化；已过账的旧薪资单金额**不变**（`expr_snapshot` 是快照） |

---

## 2. 与现状的关系

### 2.1 会被影响的现有表 / 模块（逐条实测）

| 现有对象 | 位置（实测） | 影响方式 |
|---|---|---|
| `sys_user` | `system/V0001:2-24` | **不改表**（D-1）。只在新接口里按 `biz_employee.user_id` 反查，账号管理页多一列"关联员工"（JOIN） |
| `sys_dept` | `system/V0006`，`dept_path` 子树查询 | **不改**。员工归属引用它；调岗时同时写 `sys_user.dept_id`（D-3） |
| `sys_role` / `sys_role_menu` | `system/V0002`、`V0005` | **加 1 个角色** `HR_MANAGER` + 授权（§3.2 ⑤） |
| `sys_menu` | `system/V0004`，顶级 19 个（实测 `SELECT … WHERE parent_id IS NULL`） | **加 1 目录 + 5 页面 + 30 按钮**（36 行），顶级 `sort_order=77`（实测 77 空闲） |
| `sys_dict_type` / `sys_dict_item` | `system` 字典 | **加 4 类 + 26 项**（岗位/职级/学历/请假类型） |
| `sys_config` | `support/V0009`（`config_level`：1 系统级 / 2 业务级） | **加 3 条** `hr.*` 开关（一期口径写进配置，二期改配置即切） |
| `sys_field_mask` | `V0062`（`mask_type` 1..7，`role_id IS NULL` = 全局） | **加 2 条**：`biz_employee.id_card`（type 2）、`biz_employee.bank_account`（type 5） |
| `biz_account` | `V0137:100-131`、`V0183`（56 个科目）、`V0226`（`2241.01`、`5602.01~09`）、`V0311`（`2241.02`） | **加 9 个末级科目 + 3 个父科目翻非末级**（§3.2 ①）。`2211` 实测 `is_leaf=1`（唯一没有下级的负债科目），加子科目后必须翻 `is_leaf=0` |
| `biz_voucher_rule` | `V0149:117-140` | **加 2 条**规则（`PAYROLL_ACCRUAL_DEFAULT` / `PAYROLL_PAYMENT_DEFAULT`），只做口径登记与配置页可见；真实分录由 `voucher_multi::post_explicit_lines` 逐项展开（§4.6） |
| `biz_seq` | `V0137:285-305`（`date_pattern` 支持 `yyyyMMdd/yyyyMM/None`） | **加 4 条号段**（`EMP/LV/ATT/SAL`） |
| `biz_flow_def` / `_version` / `_graph` | `V0120:47`（定义侧）、`V0122`（运行侧 4 表）、`V0243`（`expense` 流程种子模板） | **加 4 个流程**（请假/工资发放/调薪/离职） |
| `biz_voucher` / `biz_voucher_entry` | `V0149:41-115` | **不改表**。工资凭证走既有 `post_voucher`（`erp/voucher.rs:1963`）+ `post_explicit_lines`（`erp/voucher_multi.rs:239`），只新增 `source_type` 枚举值 |
| `biz_status_log` | `V0159`（`entity_type` **无 CHECK 白名单**，`V0219:44` 明确写过） | **不改表**。新增 5 个 `entity_type` 取值（由 `DocKind` 域自动覆盖） |
| `biz_doc_attachment` | `V0212`、`V0347:79`、`V0350`（`doc_type` CHECK 现为 **16 值**） | **放宽 CHECK 到 20 值**（+ `EMPLOYEE`/`EMPLOYEE_CONTRACT`/`LEAVE_REQUEST`/`SALARY_SHEET`）+ `biz_attachment_rule.doc_type` 同步 |
| `biz_attachment_rule` | `V0347:71-115`（必传附件规则） | **加 2 条**：劳动合同必传扫描件（`EMPLOYEE_CONTRACT`+`SUBMIT`）、工资表可选（不校验） |
| 附件 / 流程 / 任务 / impexp 组件 | `business/attachment`、`business/flow`、`business/task`、`business/impexp` | **全部复用，不改**。新增 impexp 导入定义 1 个（考勤）、导出走各模块自己的 export |
| `biz_task_worklog` | `V0100:208-224`（`task_id/user_id/work_date/hours`） | **不改**。它是"任务工时"（`user_id` 口径），与考勤工时是两回事（§4.7.3） |

### 2.2 新增 / 改造矩阵

| 类型 | 对象 | 数量 |
|---|---|---|
| **新增表** | `biz_employee`、`biz_employee_contract`、`biz_employee_salary`、`biz_attendance_period`、`biz_attendance_record`、`biz_leave_request`、`biz_salary_item`、`biz_salary_sheet`、`biz_salary_detail`、`biz_salary_detail_item` | **10 张** |
| **改造表（结构）** | 无（**零张现有表加列/删列**） | **0** |
| **改造（数据/种子）** | `biz_account`(9+3)、`biz_seq`(4)、`sys_role`(1)、`sys_menu`(36)、`sys_dict_type`(4)、`sys_dict_item`(26)、`sys_config`(3)、`sys_field_mask`(2)、`biz_voucher_rule`(2)、`biz_flow_def`+版本+图(4 套) | 10 类 |
| **改造（约束）** | `biz_doc_attachment.ck_biz_doc_attachment_type`、`biz_attachment_rule.ck_biz_attachment_rule_doc_type`（16 → 20 值） | 2 条 CHECK |
| **代码改造（枚举）** | `shared/doc_kind.rs`（+5 `DocKind`、+3 `FlowBizType`、+2 `VoucherSourceType`）、`shared/state_machine.rs`（+5 `Entity` + 边表）、前端 `doc-kinds.ts` / `attachment.ts` | 4 个文件 |

### 2.3 会不会破坏现有数据（必须：不破坏）

**结论：不破坏。逐条论证：**

1. **零张现有表加列** —— 本设计对存量表只做 `INSERT` 种子与 2 条 CHECK **放宽**（16 → 20 值），不删不改任何列、不加 `NOT NULL`、不改唯一键。ARCH-9-1 才动 42 张表；本卡刻意把"碰存量表"压到 0，就是为了让 HR 可以先于/独立于多组织上线。
2. **科目：`2211` 由末级翻非末级** —— 这是唯一有"语义变化"的一步。风险与实测：
   - 存量分录落在 `2211` 上的行数：`cloudpivot_test` 实测 **0 行**（`SELECT count(*) FROM biz_voucher_entry WHERE account_code LIKE '2211%'`）。
   - `voucher::post_voucher` 有"记账科目必须末级"的闸（`voucher_multi::ensure_leaf`，`erp/voucher_multi.rs:305`；`voucher::non_leaf_codes`）。翻成非末级后，**新的**分录必须落 `2211.01~03`——这正是本设计要的；**历史**分录（如果有）只是不再允许新增同类，不影响读取。
   - 报表不受影响：`report::match_account` 按「编码本身**或**其下级」匹配（`erp/report.rs:485-487`，`account_code == token || starts_with(token.".")`），所以资产负债表项 `PAYROLL_PAYABLE → 2211`（`V0183:216`）在加子科目后**自动汇总子科目**，报表 SQL 一行不改。
3. **`biz_seq` 加 4 条号段** —— 纯新增，唯一索引 `uk_biz_seq_type (seq_type)` 不冲突（`EMP/LV/ATT/SAL` 实测未被占用）。ARCH-9-1 之后该唯一键变成 `(org_id, seq_type)`，本设计的种子脚本已写成"有 `org_id` 列就带、没有就不带"的兼容形态（证据脚本 §② 的 `DO` 块动态拼接，实跑验证过两条分支）。
4. **`sys_menu` 加 36 行** —— 顶级 `sort_order=77` 实测空闲（现有 19 个顶级的 sort：5/10/18/20/55/60/60/61/62/62/65/66/69/70/70/72/75/76/95）；同父 `sort_order` 唯一、`title` ≤ 12 字，两条都写进证据断言（AGENTS.md 约定 12）。
5. **附件 CHECK 放宽** —— 只放宽不放宽（`IN` 集合加 4 值），存量 16 类附件全部仍合法；`V0350` 已有同样的放宽先例（12 → 16）。

### 2.4 与 ARCH-1~9-1 已有设计的关系

| 设计 | 关系 | 说明 |
|---|---|---|
| **ARCH-9-1 多组织/多公司** | **前置依赖（硬）** | 本设计 10 张表全部带 `org_id UUID NOT NULL`（与 ARCH-9-1 §3.2 对 42 张表的改造同口径：**只加列、不建物理外键到 `biz_org`**，避免 10 张表同时锁父表）。ARCH-9-1 §7.1 预留 V0368~V0377，本卡用 V0378 起。若 HR 先上线（不推荐），只需把 `org_id` 换成常量列，其余 DDL 不变 |
| **ARCH-6 财务扩展（FIN-1~3）** | **复用** | 凭证生成复用 `voucher_multi::post_explicit_lines`（`erp/voucher_multi.rs:239`，折旧/处置/报销都用它）；科目补种口径照 `V0226`（`2241.01` 员工垫付）与 `V0311`（`2241.02` 佣金）；状态机写法照 `biz_expense_report` 的 6 态。**冲突点**：报销单的贷方是 `2241.01 其他应付款-员工垫付`，工资代扣不能用它（那是"公司欠员工"），必须新开 `2241.03/04`（"公司代扣、欠社保中心/公积金中心"） |
| **ARCH-4 通用 MES（工作量结算）** | **有冲突，本卡拍板** | ARCH-4 的 `biz_work_settlement` 自带凭证：`借 4001 生产成本-直接人工 / 贷 2211 应付职工薪酬`（`docs/architecture/ARCH-4-通用MES与委外架构设计.md:1938`），列 `voucher_id` 在结算单上（同文件 1890）。**若 HR 同期上线，同一笔计件工资会被计提两次**（MES 一次、薪资单一次）。本卡结论：**工资凭证的唯一出口是薪资单**；MES 结算单只产出"计件工资数据"，由薪资单的 `source='MES'` 薪资项吸收（§4.7.2）。两模块都还在设计阶段，现在改设计零成本 |
| **ARCH-5 工作流引擎** | **复用** | 4 个审批流走 `biz_flow_def/version/graph` + `flow::engine`；审批人解析用既有 8+2 种来源（`flow/assignee.rs`）；不硬编码节点 |
| **ARCH-7 销售 / ARCH-8 审批职责分离** | **复用** | 工资发放审批必须过 `flow::self_approval`（自己不能批自己的工资单，与 `expense_bridge::guard_self_approval` 同口径，`erp/expense_bridge.rs` 模块文档入口 ②） |
| **ARCH-3 勾稽审计** | **补充** | 新增 1 个巡检脚本 `scripts/verify/hr_invariants.sql`（10 条断言，§4.9），并纳入 ARCH-3 的勾稽总表 |
| **ARCH-2 合同表** | **不冲突** | `biz_contract` 是"与外部单位的合同"（采购/销售/服务/租赁）；`biz_employee_contract` 是"劳动合同"，两者主体、科目、流程都不同，**不合并** |
| **ARCH-10 产品演进路线** | **本卡即其 §0.3 的「ARCH9-2 人力资源」** | `docs/architecture/ARCH-10-产品演进路线-v1.md:144` 已列出该待补项 |
---

## 3. 数据模型

### 3.0 关系总览

```text
sys_dept ──dept_id──┐                    ┌── org_id (ARCH-9-1：多公司硬隔离，本卡只加列不建 FK)
                    │                    │
                    ├──> biz_employee ───┴── user_id ──(可空，唯一)──> sys_user.id
                    │        │  │  │
                    │        │  │  └──> biz_employee_contract   （劳动合同，1:N，链式续签）
                    │        │  └─────> biz_employee_salary     （薪资标准，1:N，按生效日期版本化）
                    │        │
                    │        ├──> biz_attendance_record ──period_id──> biz_attendance_period（月度批次 1:N）
                    │        ├──> biz_leave_request                  （请假单，走审批流）
                    │        └──> biz_salary_detail ──sheet_id──> biz_salary_sheet（一公司一期一张）
                    │                    └──> biz_salary_detail_item ──item_id──> biz_salary_item（薪资项 + 公式）
                    │
                    └──(调岗时同事务双写)──> sys_user.dept_id

biz_salary_sheet ──accrual_voucher_id / payment_voucher_id──> biz_voucher（两张，source_type=PAYROLL_*）
```

### 3.1 新增表（10 张，完整 DDL）

> 以下 DDL 与 `docs/evidence/2026-11-10-arch9-2-hr-ddl/01_new_tables_check.sql` **逐字一致**（脚本在该 DDL 后追加 45 条断言，见 §7.5）。
> 命名与既有表一致：`id UUID PK DEFAULT gen_random_uuid()`、审计五列（`created_at/updated_at/create_by/update_by/deleted_at`）、软删一律 `deleted_at IS NULL` 进唯一索引。

#### ① `biz_employee`（员工主档，人事维度）

```sql
CREATE TABLE IF NOT EXISTS biz_employee (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,                    -- 所属公司（ARCH-9-1 硬隔离维度）
    emp_no                VARCHAR(32)  NOT NULL,            -- 工号（EMP-0001，公司内唯一，软删可复用）
    name                  VARCHAR(50)  NOT NULL,            -- 姓名（主数据：改名后所有单据显示新名，见 AGENTS.md 约定 13）
    gender                SMALLINT     NOT NULL DEFAULT 0,  -- 0 未知 / 1 男 / 2 女
    birth_date            DATE,
    id_card               VARCHAR(18),                      -- 身份证号（出参按 sys_field_mask 脱敏）
    id_card_type          VARCHAR(16)  NOT NULL DEFAULT 'ID_CARD',
    mobile                VARCHAR(20),
    email                 VARCHAR(100),
    dept_id               UUID         NOT NULL,            -- 人事归属部门（sys_dept.id，弱引用不建 FK）
    position_code         VARCHAR(32),                      -- 岗位（字典 hr_position）
    job_level             VARCHAR(20),                      -- 职级（字典 hr_job_level）
    employment_type       VARCHAR(16)  NOT NULL DEFAULT 'FULL_TIME',
    hire_date             DATE         NOT NULL,            -- 入职日期
    probation_end_date    DATE,                             -- 试用期截止（转正提醒用）
    leave_date            DATE,                             -- 离职日期（status=RESIGNED 时必填，双向 CHECK）
    leave_type            VARCHAR(16),
    status                VARCHAR(16)  NOT NULL DEFAULT 'PROBATION',
    education             VARCHAR(16),                      -- 学历（字典 hr_education）
    school                VARCHAR(100),
    major                 VARCHAR(100),
    graduate_date         DATE,
    native_place          VARCHAR(100),
    address               VARCHAR(200),
    emergency_contact     VARCHAR(50),                      -- 紧急联系人
    emergency_phone       VARCHAR(20),
    emergency_relation    VARCHAR(20),
    bank_name             VARCHAR(100),                     -- 工资卡开户行
    bank_branch           VARCHAR(200),
    bank_account          VARCHAR(40),                      -- 工资卡号（出参脱敏）
    bank_account_name     VARCHAR(50),
    user_id               UUID,                             -- ★ 关联登录账号（可空、唯一；见 §4.1.1）
    remark                TEXT,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_employee_gender    CHECK (gender IN (0, 1, 2)),
    CONSTRAINT ck_biz_employee_idtype    CHECK (id_card_type IN ('ID_CARD', 'PASSPORT', 'HK_MO_TW', 'FOREIGN')),
    CONSTRAINT ck_biz_employee_emptype   CHECK (employment_type IN ('FULL_TIME','PROBATION','INTERN','OUTSOURCE','PART_TIME','DISPATCH')),
    CONSTRAINT ck_biz_employee_status    CHECK (status IN ('PROBATION','ACTIVE','SUSPENDED','RESIGNED')),
    CONSTRAINT ck_biz_employee_leavetype CHECK (leave_type IS NULL OR leave_type IN ('RESIGN','DISMISS','RETIRE','CONTRACT_END','DEATH')),
    -- 离职语义双向：有 leave_date ⇔ 状态是 RESIGNED（防「标了离职但没日期」与「有离职日但还在职」两种脏数据）
    CONSTRAINT ck_biz_employee_leave     CHECK ((status = 'RESIGNED') = (leave_date IS NOT NULL)),
    CONSTRAINT ck_biz_employee_leavetype2 CHECK (status <> 'RESIGNED' OR leave_type IS NOT NULL),
    CONSTRAINT ck_biz_employee_dates     CHECK (leave_date IS NULL OR leave_date >= hire_date),
    CONSTRAINT ck_biz_employee_probation CHECK (probation_end_date IS NULL OR probation_end_date >= hire_date),
    CONSTRAINT ck_biz_employee_idcard_len CHECK (id_card IS NULL OR length(id_card) BETWEEN 6 AND 18)
);

COMMENT ON TABLE  biz_employee IS '员工主档（人事维度）：与登录账号 sys_user 是 1:0..1 —— 车间工人无账号，外包/机器人有账号非员工';
COMMENT ON COLUMN biz_employee.user_id  IS '关联登录账号 sys_user.id（可空、唯一）。放员工侧而非 sys_user 侧的理由见 ARCH-9-2 §4.1.1';
COMMENT ON COLUMN biz_employee.id_card  IS '身份证号。存明文（导入查重/社保申报需要），出参按 sys_field_mask(mask_type=2) 脱敏，明文需 business:hr:employee:sensitive';

CREATE UNIQUE INDEX uk_biz_employee_no     ON biz_employee (org_id, emp_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_employee_user   ON biz_employee (user_id) WHERE user_id IS NOT NULL AND deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_employee_idcard ON biz_employee (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_biz_employee_dept   ON biz_employee (org_id, dept_id, status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_employee_status ON biz_employee (org_id, status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_employee_hire   ON biz_employee (org_id, hire_date DESC) WHERE deleted_at IS NULL;
```

#### ② `biz_employee_contract`（劳动合同台账）

```sql
CREATE TABLE IF NOT EXISTS biz_employee_contract (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID NOT NULL,
    employee_id       UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    contract_no       VARCHAR(32) NOT NULL,                 -- 合同编号（公司内唯一，人工填，不取号：合同号必须与纸质一致）
    contract_type     VARCHAR(16) NOT NULL DEFAULT 'FIXED_TERM',
    sign_date         DATE        NOT NULL,                 -- 签订日
    start_date        DATE        NOT NULL,
    end_date          DATE,                                 -- 无固定期限为 NULL
    probation_months  SMALLINT    NOT NULL DEFAULT 0,
    status            VARCHAR(16) NOT NULL DEFAULT 'ACTIVE',
    terminated_date   DATE,
    terminate_reason  VARCHAR(255),
    renewed_from_id   UUID REFERENCES biz_employee_contract(id) ON DELETE SET NULL,  -- 续签链
    remark            TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_emp_contract_type   CHECK (contract_type IN ('FIXED_TERM','UNFIXED_TERM','INTERN','LABOR_SERVICE','DISPATCH')),
    CONSTRAINT ck_biz_emp_contract_status CHECK (status IN ('ACTIVE','EXPIRED','TERMINATED','RENEWED')),
    CONSTRAINT ck_biz_emp_contract_range  CHECK (end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_emp_contract_sign   CHECK (sign_date <= start_date),
    CONSTRAINT ck_biz_emp_contract_prob   CHECK (probation_months BETWEEN 0 AND 6),   -- 劳动合同法第 19 条上限
    CONSTRAINT ck_biz_emp_contract_term   CHECK ((status = 'TERMINATED') = (terminated_date IS NOT NULL)),
    CONSTRAINT ck_biz_emp_contract_unfixed CHECK (contract_type <> 'UNFIXED_TERM' OR end_date IS NULL)
);

CREATE UNIQUE INDEX uk_biz_emp_contract_no ON biz_employee_contract (org_id, contract_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_contract_emp ON biz_employee_contract (employee_id, start_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_contract_due ON biz_employee_contract (org_id, end_date) WHERE status = 'ACTIVE' AND deleted_at IS NULL;
```

> **为什么不把合同塞进 `biz_employee`**（D-5）：一份劳动合同到期 → 续签 → 再续签，是 3 行历史；单表只能留最后一行，"合同到期前 30 天提醒"要扫全表且看不到历史。附件（扫描件）挂在 `biz_doc_attachment(doc_type='EMPLOYEE_CONTRACT', doc_id=合同 id)` 上，靠 `biz_attachment_rule` 做**提交前必传**校验（§3.2 ⑦）。

#### ③ `biz_employee_salary`（员工薪资标准，版本化）

```sql
CREATE TABLE IF NOT EXISTS biz_employee_salary (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL,
    employee_id     UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    item_code       VARCHAR(32)   NOT NULL,                 -- 薪资项编码（弱引用 biz_salary_item.item_code，不建 FK）
    amount          NUMERIC(18,4) NOT NULL,                 -- 标准金额（月标准，正数）
    effective_from  DATE          NOT NULL,                 -- 生效起（含）
    effective_to    DATE,                                   -- 生效止（含），NULL = 至今
    remark          VARCHAR(200),
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_biz_emp_salary_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_emp_salary_range  CHECK (effective_to IS NULL OR effective_to >= effective_from)
);

CREATE UNIQUE INDEX uk_biz_emp_salary ON biz_employee_salary (employee_id, item_code, effective_from) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_emp_salary_emp ON biz_employee_salary (employee_id, effective_from DESC) WHERE deleted_at IS NULL;
```

> **为什么版本化**（照 ARCH-4 D3 的第一性要求：调价不许改历史工资）：试算时按 `effective_from <= period_end AND (effective_to IS NULL OR effective_to >= period_start)` 取当期标准；调薪 = 给旧行写 `effective_to = 新生效日 - 1 天` + 插一行新 `effective_from`。**不允许两条区间重叠**（应用层校验 + §4.9 巡检断言 A3），否则同一期间取到两个标准，谁后被 `ORDER BY` 选中就成了随机数。

#### ④ `biz_attendance_period`（月度考勤批次）

```sql
CREATE TABLE IF NOT EXISTS biz_attendance_period (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id            UUID NOT NULL,
    period_code       VARCHAR(7)   NOT NULL,                -- 考勤月 yyyy-MM（与 biz_accounting_period.period_code 同口径）
    period_start      DATE         NOT NULL,                -- 起（含）：支持"26 日~次月 25 日"的计薪周期
    period_end        DATE         NOT NULL,
    status            VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    employee_count    INT          NOT NULL DEFAULT 0,      -- 汇总快照（导入/修改后重算）
    should_days_total NUMERIC(12,2) NOT NULL DEFAULT 0,
    actual_days_total NUMERIC(12,2) NOT NULL DEFAULT 0,
    ot_hours_total    NUMERIC(12,2) NOT NULL DEFAULT 0,
    import_task_id    UUID,                                 -- 最近一次导入的 sys_import_task.id
    submitted_at      TIMESTAMPTZ,
    submitted_by      UUID,
    locked_at         TIMESTAMPTZ,
    locked_by         UUID,
    remark            TEXT,
    created_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    -- 月度正则必须写 (0[1-9]|1[0-2])：宽正则 ^[0-9]{4}-[0-9]{2}$ 会放过 2026-13（ARCH-9-1 踩过同一个坑）
    CONSTRAINT ck_biz_att_period_code   CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_att_period_range  CHECK (period_end >= period_start),
    CONSTRAINT ck_biz_att_period_status CHECK (status IN ('DRAFT','SUBMITTED','LOCKED')),
    CONSTRAINT ck_biz_att_period_counts CHECK (employee_count >= 0 AND should_days_total >= 0
                                           AND actual_days_total >= 0 AND ot_hours_total >= 0)
);

CREATE UNIQUE INDEX uk_biz_att_period ON biz_attendance_period (org_id, period_code) WHERE deleted_at IS NULL;
```

#### ⑤ `biz_attendance_record`（考勤记录，一人一月一行）

```sql
CREATE TABLE IF NOT EXISTS biz_attendance_record (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    period_id           UUID NOT NULL REFERENCES biz_attendance_period(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32)  NOT NULL,              -- 工号快照（考勤口径的历史留痕）
    employee_name       VARCHAR(50)  NOT NULL,              -- 姓名快照
    dept_id             UUID,                               -- 归属部门（凭证 aux_dept_id 用）
    should_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 应出勤天数（口径由客户定：自然月/21.75/排班）
    actual_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 实际出勤
    leave_paid_days     NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 带薪假（年假/婚丧/产假）
    leave_unpaid_days   NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 无薪假（事假）→ 薪资扣款依据
    sick_days           NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 病假（按比例发，比例在薪资项公式里配）
    absent_days         NUMERIC(6,2) NOT NULL DEFAULT 0,    -- 旷工
    ot_hours_workday    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 平时加班小时
    ot_hours_weekend    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 休息日加班小时
    ot_hours_holiday    NUMERIC(8,2) NOT NULL DEFAULT 0,    -- 法定节假日加班小时
    late_count          INT          NOT NULL DEFAULT 0,
    early_leave_count   INT          NOT NULL DEFAULT 0,
    source              VARCHAR(16)  NOT NULL DEFAULT 'IMPORT',
    remark              TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by           UUID,
    update_by           UUID,
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT ck_biz_att_rec_nonneg CHECK (should_days >= 0 AND actual_days >= 0
        AND leave_paid_days >= 0 AND leave_unpaid_days >= 0 AND sick_days >= 0 AND absent_days >= 0
        AND ot_hours_workday >= 0 AND ot_hours_weekend >= 0 AND ot_hours_holiday >= 0
        AND late_count >= 0 AND early_leave_count >= 0),
    CONSTRAINT ck_biz_att_rec_source CHECK (source IN ('IMPORT','MANUAL','LEAVE_SYNC'))
);

CREATE UNIQUE INDEX uk_biz_att_rec ON biz_attendance_record (period_id, employee_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_att_rec_emp ON biz_attendance_record (employee_id, period_id);
```

> **为什么没有「应出勤 = 实出勤 + 各类假 + 旷工」的守恒 CHECK**：应出勤口径客户之间不同（自然月 30/31、固定 21.75、排班制），硬 CHECK 会把合法数据挡在门外（例：固定 21.75 口径下实际出勤 22 天）。改为**导入校验器**出「行级警告」+ 巡检断言 W1（§4.9），既有约束又不误伤。

#### ⑥ `biz_leave_request`（请假单）

```sql
CREATE TABLE IF NOT EXISTS biz_leave_request (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    leave_no              VARCHAR(32) NOT NULL,             -- LV-yyyyMMdd-000001
    employee_id           UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no           VARCHAR(32) NOT NULL,
    employee_name         VARCHAR(50) NOT NULL,
    dept_id               UUID,
    leave_type            VARCHAR(16) NOT NULL,             -- 与字典 hr_leave_type 一一对应
    start_time            TIMESTAMPTZ NOT NULL,             -- 精确到时（半天假用 12:00 切）
    end_time              TIMESTAMPTZ NOT NULL,
    duration_days         NUMERIC(6,2) NOT NULL,            -- 请假天数（0.5 天粒度）
    paid                  SMALLINT    NOT NULL DEFAULT 1,   -- 1 带薪 / 0 无薪（薪资扣款只看这一列）
    status                VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    reason                VARCHAR(500),
    approval_instance_id  UUID,
    submitted_at          TIMESTAMPTZ,
    submitted_by          UUID,
    cancelled_at          TIMESTAMPTZ,
    cancelled_by          UUID,
    cancel_reason         VARCHAR(255),
    remark                TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_leave_type   CHECK (leave_type IN ('ANNUAL','SICK','PERSONAL','MARRIAGE','MATERNITY','PATERNITY','BEREAVEMENT','COMPENSATORY','OTHER')),
    CONSTRAINT ck_biz_leave_status CHECK (status IN ('DRAFT','SUBMITTED','APPROVED','CANCELLED')),
    CONSTRAINT ck_biz_leave_range  CHECK (end_time > start_time),
    CONSTRAINT ck_biz_leave_days   CHECK (duration_days > 0),
    CONSTRAINT ck_biz_leave_paid   CHECK (paid IN (0, 1)),
    CONSTRAINT ck_biz_leave_cancel CHECK (status <> 'CANCELLED' OR cancel_reason IS NOT NULL)
);

CREATE UNIQUE INDEX uk_biz_leave_no ON biz_leave_request (org_id, leave_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_leave_emp  ON biz_leave_request (employee_id, start_time DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_leave_stat ON biz_leave_request (org_id, status) WHERE deleted_at IS NULL;
```

#### ⑦ `biz_salary_item`（薪资项定义 + 公式）

```sql
CREATE TABLE IF NOT EXISTS biz_salary_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    item_code             VARCHAR(32) NOT NULL,             -- BASE / POST / PERF / OT / ATT_DEDUCT / SI_PERSONAL / IIT / GROSS / NET …
    item_name             VARCHAR(50) NOT NULL,
    item_type             VARCHAR(16) NOT NULL,             -- EARNING/PRE_DEDUCT/WITHHOLD/EMPLOYER/SUMMARY（语义见 §4.4.1）
    calc_type             VARCHAR(16) NOT NULL,             -- FIXED 取标准 / FORMULA 公式 / INPUT 手工录
    source                VARCHAR(24) NOT NULL DEFAULT 'INPUT',
    formula               VARCHAR(500),                     -- calc_type=FORMULA 时必填（DSL 见 §4.4.2）
    sort_order            INT         NOT NULL DEFAULT 10,  -- 计算顺序（拓扑外的稳定排序，公司内唯一）
    debit_account_code    VARCHAR(30),                      -- 借方科目（EARNING/PRE_DEDUCT/EMPLOYER 必填）
    credit_account_code   VARCHAR(30),                      -- 贷方科目（EARNING/PRE_DEDUCT/EMPLOYER/WITHHOLD 必填）
    is_taxable            SMALLINT    NOT NULL DEFAULT 1,   -- 是否计入个税应税收入（二期自动个税用）
    is_active             SMALLINT    NOT NULL DEFAULT 1,
    is_system             SMALLINT    NOT NULL DEFAULT 0,   -- 系统项（GROSS/NET）不许删
    remark                VARCHAR(500),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_sal_item_type    CHECK (item_type IN ('EARNING','PRE_DEDUCT','WITHHOLD','EMPLOYER','SUMMARY')),
    CONSTRAINT ck_biz_sal_item_calc    CHECK (calc_type IN ('FIXED','FORMULA','INPUT')),
    CONSTRAINT ck_biz_sal_item_source  CHECK (source IN ('EMPLOYEE_STANDARD','ATTENDANCE','INPUT','SYSTEM','MES')),
    CONSTRAINT ck_biz_sal_item_formula CHECK (calc_type <> 'FORMULA' OR formula IS NOT NULL),
    CONSTRAINT ck_biz_sal_item_negflag CHECK (is_taxable IN (0,1) AND is_active IN (0,1) AND is_system IN (0,1)),
    -- 科目要求按类型分开：SUMMARY 不要科目（对账时由具体项汇总），WITHHOLD 只需贷方
    CONSTRAINT ck_biz_sal_item_account CHECK (
        (item_type IN ('EARNING','PRE_DEDUCT','EMPLOYER') AND debit_account_code IS NOT NULL AND credit_account_code IS NOT NULL)
        OR (item_type = 'WITHHOLD' AND credit_account_code IS NOT NULL)
        OR (item_type = 'SUMMARY' AND formula IS NOT NULL)
    )
);

CREATE UNIQUE INDEX uk_biz_sal_item_code ON biz_salary_item (org_id, item_code) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_sal_item_sort ON biz_salary_item (org_id, sort_order) WHERE deleted_at IS NULL;
```

#### ⑧ `biz_salary_sheet`（薪资单，一头）

```sql
CREATE TABLE IF NOT EXISTS biz_salary_sheet (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id                UUID NOT NULL,
    sheet_no              VARCHAR(32) NOT NULL,             -- SAL-yyyyMM-000001
    period_code           VARCHAR(7)  NOT NULL,             -- 薪资期间 yyyy-MM
    period_start          DATE        NOT NULL,
    period_end            DATE        NOT NULL,
    status                VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    attendance_period_id  UUID REFERENCES biz_attendance_period(id) ON DELETE RESTRICT,  -- 数据来源批次
    employee_count        INT           NOT NULL DEFAULT 0,
    earning_total         NUMERIC(20,4) NOT NULL DEFAULT 0, -- Σ EARNING（含正向与负数补发）
    pre_deduct_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- Σ PRE_DEDUCT（考勤扣款）
    gross_total           NUMERIC(20,4) NOT NULL DEFAULT 0, -- 应发 = earning - pre_deduct
    withhold_total        NUMERIC(20,4) NOT NULL DEFAULT 0, -- 代扣合计 = 社保+公积金个人+个税+其他
    net_total             NUMERIC(20,4) NOT NULL DEFAULT 0, -- 实发 = 应发 - 代扣
    si_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- 单位社保（进费用，不进应发）
    hf_company_total      NUMERIC(20,4) NOT NULL DEFAULT 0, -- 单位公积金
    accrual_voucher_id    UUID,                             -- 计提凭证
    accrual_voucher_no    VARCHAR(32),
    payment_voucher_id    UUID,                             -- 发放凭证
    payment_voucher_no    VARCHAR(32),
    reversal_voucher_id   UUID,                             -- 红冲凭证（POSTED → REVERSED 时写）
    approval_instance_id  UUID,
    calculated_at         TIMESTAMPTZ, calculated_by  UUID,
    submitted_at          TIMESTAMPTZ, submitted_by   UUID,
    posted_at             TIMESTAMPTZ, posted_by      UUID,
    reversed_at           TIMESTAMPTZ, reversed_by    UUID,
    reverse_reason        VARCHAR(255),
    remark                TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_sal_sheet_code   CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_sal_sheet_range  CHECK (period_end >= period_start),
    CONSTRAINT ck_biz_sal_sheet_status CHECK (status IN ('DRAFT','CALCULATED','SUBMITTED','APPROVED','POSTED','REVERSED')),
    -- 三个恒等式写进库：任何绕过引擎的写路径都写不进"算错"的薪资单
    CONSTRAINT ck_biz_sal_sheet_idem   CHECK (gross_total = net_total + withhold_total),
    CONSTRAINT ck_biz_sal_sheet_gross  CHECK (gross_total = earning_total - pre_deduct_total),
    CONSTRAINT ck_biz_sal_sheet_nonneg CHECK (earning_total >= 0 AND pre_deduct_total >= 0
                                            AND gross_total >= 0 AND withhold_total >= 0 AND net_total >= 0
                                            AND si_company_total >= 0 AND hf_company_total >= 0)
);

CREATE UNIQUE INDEX uk_biz_sal_sheet_no     ON biz_salary_sheet (org_id, sheet_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_sal_sheet_period ON biz_salary_sheet (org_id, period_code) WHERE deleted_at IS NULL;
```

#### ⑨ `biz_salary_detail`（薪资明细，一人一行汇总）

```sql
CREATE TABLE IF NOT EXISTS biz_salary_detail (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id              UUID NOT NULL,
    sheet_id            UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,
    employee_id         UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    employee_no         VARCHAR(32) NOT NULL,               -- 快照：工号（审计口径）
    employee_name       VARCHAR(50) NOT NULL,               -- 快照：姓名
    dept_id             UUID,                               -- 快照：部门（凭证 aux_dept_id / 部门汇总）
    dept_name           VARCHAR(100),                       -- 快照：部门名（部门改名后工资表不漂移）
    user_id             UUID,                               -- ★ 数据权限 SELF 锚点（由 employee.user_id 派生，可空）
    position_code       VARCHAR(32),                        -- 快照：岗位
    job_level           VARCHAR(20),                        -- 快照：职级
    should_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,   -- 考勤快照
    actual_days         NUMERIC(6,2)  NOT NULL DEFAULT 0,
    leave_unpaid_days   NUMERIC(6,2)  NOT NULL DEFAULT 0,
    ot_hours            NUMERIC(8,2)  NOT NULL DEFAULT 0,
    earning_amount      NUMERIC(18,4) NOT NULL DEFAULT 0,   -- Σ EARNING
    pre_deduct_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,   -- Σ PRE_DEDUCT
    gross_amount        NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 应发
    social_personal     NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 社保个人
    fund_personal       NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 公积金个人
    iit_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 个税
    other_withhold      NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 其他代扣
    withhold_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 代扣合计
    net_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 实发
    si_company          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 单位社保（费用侧）
    hf_company          NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 单位公积金（费用侧）
    remark              VARCHAR(500),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_sal_det_gross   CHECK (gross_amount = earning_amount - pre_deduct_amount),
    CONSTRAINT ck_biz_sal_det_withold CHECK (withhold_amount = social_personal + fund_personal + iit_amount + other_withhold),
    CONSTRAINT ck_biz_sal_det_net     CHECK (gross_amount = net_amount + withhold_amount),
    CONSTRAINT ck_biz_sal_det_nonneg  CHECK (earning_amount >= 0 AND pre_deduct_amount >= 0 AND gross_amount >= 0
                                           AND social_personal >= 0 AND fund_personal >= 0 AND iit_amount >= 0
                                           AND other_withhold >= 0 AND withhold_amount >= 0 AND net_amount >= 0
                                           AND si_company >= 0 AND hf_company >= 0)
);

CREATE UNIQUE INDEX uk_biz_sal_det ON biz_salary_detail (sheet_id, employee_id);
CREATE INDEX idx_biz_sal_det_emp  ON biz_salary_detail (employee_id, sheet_id);
CREATE INDEX idx_biz_sal_det_user ON biz_salary_detail (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX idx_biz_sal_det_dept ON biz_salary_detail (org_id, dept_id);
```

> **明细行没有 `deleted_at`**：明细随单头级联（`ON DELETE CASCADE`），单头删了明细就该没了；要留痕靠 `detail_item.expr_snapshot` + `biz_status_log`。

#### ⑩ `biz_salary_detail_item`（薪资明细项目，公式解释层）

```sql
CREATE TABLE IF NOT EXISTS biz_salary_detail_item (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id         UUID NOT NULL,
    sheet_id       UUID NOT NULL REFERENCES biz_salary_sheet(id) ON DELETE CASCADE,   -- 冗余：按单查项免 join
    detail_id      UUID NOT NULL REFERENCES biz_salary_detail(id) ON DELETE CASCADE,
    item_id        UUID REFERENCES biz_salary_item(id) ON DELETE SET NULL,            -- 薪资项被删后项快照仍在
    item_code      VARCHAR(32) NOT NULL,
    item_name      VARCHAR(50) NOT NULL,
    item_type      VARCHAR(16) NOT NULL,
    calc_type      VARCHAR(16) NOT NULL,
    amount         NUMERIC(18,4) NOT NULL DEFAULT 0,        -- 允许负数（补发/冲回，见 §4.5.3）
    expr_snapshot  VARCHAR(500),                            -- 计算时公式原文（快照：改公式不改历史）
    calc_note      VARCHAR(500),                            -- 代入值说明，如 "8000/21.75*2 = 735.63"
    line_no        INT NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_sal_ditem_type CHECK (item_type IN ('EARNING','PRE_DEDUCT','WITHHOLD','EMPLOYER','SUMMARY')),
    CONSTRAINT ck_biz_sal_ditem_calc CHECK (calc_type IN ('FIXED','FORMULA','INPUT')),
    CONSTRAINT uk_biz_sal_ditem UNIQUE (detail_id, item_code)
);

CREATE INDEX idx_biz_sal_ditem_sheet ON biz_salary_detail_item (sheet_id, item_code);
CREATE INDEX idx_biz_sal_ditem_item  ON biz_salary_detail_item (item_id);
```

### 3.2 改造表 / 种子（完整 SQL，幂等）

#### ① `biz_account`：9 个末级科目 + 3 个父科目翻非末级

```sql
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT v.code, v.name,
       (SELECT id FROM biz_account WHERE account_code = split_part(v.code, '.', 1) AND deleted_at IS NULL),
       2, v.category, v.direction, 1, 1, v.aux, 'ARCH-9-2 工资相关科目'
  FROM (VALUES
        ('2211.01', '应付职工薪酬-工资',        'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.02', '应付职工薪酬-社保(单位)',  'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.03', '应付职工薪酬-公积金(单位)','LIABILITY',   'CREDIT', 'DEPT'),
        ('2221.03', '应交税费-应交个人所得税',  'LIABILITY',   'CREDIT', NULL),
        ('2241.03', '其他应付款-社保(个人)',    'LIABILITY',   'CREDIT', NULL),
        ('2241.04', '其他应付款-公积金(个人)',  'LIABILITY',   'CREDIT', NULL),
        ('5602.10', '管理费用-工资',            'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.11', '管理费用-社保(单位)',      'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.12', '管理费用-公积金(单位)',    'PROFIT_LOSS', 'DEBIT',  'DEPT')
       ) AS v(code, name, category, direction, aux)
 WHERE NOT EXISTS (SELECT 1 FROM biz_account b WHERE b.account_code = v.code)
   AND EXISTS (SELECT 1 FROM biz_account p WHERE p.account_code = split_part(v.code, '.', 1) AND p.deleted_at IS NULL);

-- 父科目翻非末级（幂等：只翻「确实有未删子科目」且当前还是末级的父科目）
UPDATE biz_account p SET is_leaf = 0, updated_at = now()
 WHERE p.deleted_at IS NULL AND p.is_leaf = 1
   AND p.account_code IN ('2211','2221','2241','5602')
   AND EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL);
```

**为什么是这 9 个**（每条都对照现有科目集）：

| 科目 | 用途 | 为什么不能用现有的 |
|---|---|---|
| `2211.01/02/03` | 计提贷方：工资 / 单位社保 / 单位公积金 | `2211` 实测 `is_leaf=1`（无子科目），而 `post_voucher` 要求记账科目末级（`voucher_multi::ensure_leaf`）。三项必须分开：负债要能回答"欠员工多少工资"vs"欠社保中心多少" |
| `2221.03` | 代扣个税 | 只有 `2221.01 进项税`、`2221.02 销项税`（`V0149:149`、`V0183`），个税是第三种税 |
| `2241.03/04` | 代扣社保个人 / 公积金个人 | `2241.01` 已被报销占用（"其他应付款-员工垫付"，`V0226:36`），`2241.02` 被佣金占用（`V0311:36`）。代扣项是"公司代收代缴"，与"欠员工"是两种负债 |
| `5602.10/11/12` | 计提借方：工资 / 社保 / 公积金 | 现有 `5602.01~09`（`V0226:45-55`）没有"工资""社保"。不新建 66xx：本仓科目集是小企业会计制度核心科目，利润表项 `MANAGE_EXPENSE` 映射 `5602`，新增 66xx 会让管理费用在报表里凭空消失（`V0311` 头注释踩过同一个坑） |

#### ② `biz_seq`：4 条号段

```sql
-- EMP 工号：永久不重置（主数据码）；LV/ATT/SAL 是单据号，按日/按月滚动
INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status)
SELECT v.t, v.n, v.p, v.d, CASE WHEN v.d = 'none' THEN 'GLOBAL' ELSE '' END, 0, 1, '-', 1
  FROM (VALUES ('EMP','员工工号','EMP','none'),
               ('LV','请假单号','LV','yyyyMMdd'),
               ('ATT','考勤批次号','ATT','yyyyMM'),
               ('SAL','薪资单号','SAL','yyyyMM')) AS v(t,n,p,d)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq s WHERE s.seq_type = v.t AND s.deleted_at IS NULL);
-- ★ ARCH-9-1 之后 biz_seq 有 org_id 列（uk 变 (org_id, seq_type)）：上面这条 INSERT 要加 org_id 列与
--   默认公司常量；本设计的证据脚本用 DO 块动态判断列是否存在（两条分支都实跑过，见 §7.5）
```

#### ③ 字典（4 类 26 项）

`hr_position`（车间工人/文员/工程师/销售/经理）、`hr_job_level`（P1/P5/P8/M1/M4）、`hr_education`（小学…博士 7 项）、`hr_leave_type`（年假/病假/事假/婚假/产假/陪产假/丧假/调休/其他 9 项，与 `biz_leave_request.leave_type` 的 CHECK 一一对应）。完整 `INSERT` 见证据脚本 §③。

#### ④ 菜单 / 权限（1 目录 + 5 页面 + 30 按钮 = 36 行）

| 菜单 | 类型 | `path` | `auth_code` | `title`（≤12 字） |
|---|---|:--:|---|---|
| 人力资源 | 目录 | `/business/hr` | — | 人力资源 |
| 员工档案 | 页面 | `/business/hr/employee` | `business:hr:employee:list` | 员工档案 |
| 考勤管理 | 页面 | `/business/hr/attendance` | `business:hr:attendance:list` | 考勤管理 |
| 请假单 | 页面 | `/business/hr/leave` | `business:hr:leave:list` | 请假单 |
| 薪资管理 | 页面 | `/business/hr/salary` | `business:hr:salary:list` | 薪资管理 |
| 薪资项 | 页面 | `/business/hr/salary-item` | `business:hr:salary-item:list` | 薪资项 |

- 目录挂**顶级**（`parent_id IS NULL`），`sort_order = 77`（实测 77 空闲）、`icon='user-business'`、`module_code='business'`。
- 30 个按钮 `menu_type=3`、`is_visible=FALSE`，父节点是 5 个页面之一，`sort_order` 10 递增（**同父唯一**，AGENTS.md 约定 12）。
- 完整 36 行 `INSERT`（含固定 UUID 段）**不要照抄证据脚本的 `…-001600~001653`**：该段
  `…001600` 已是 `business:erp:report:payable-aging`（应付账龄分析）。落地实现（HR-04 / V0387）改用
  空闲段 **`20000000-0000-4000-8000-00000000e0xx`**（e000 目录 / e001-e005 页面 / e011-e019 员工 /
  e021-e025 考勤 / e031-e036 请假 / e041-e047 薪资 / e051-e053 薪资项），见
  `backend/migrations/business/V0387__hr_menu_perms.up.sql`。

#### ⑤ `sys_role`：HR_MANAGER

```sql
-- ★ 落地版（V0387）与下面的设计稿有两处差异，已实测：
--   ① id 不能用 …f0008 —— 那是 SALES_CLERK（销售员），会撞 sys_role 主键；落地用 …f0016。
--   ② sort_order 不能用 80 —— 也是 SALES_CLERK 的排序位；落地用 25（财务角色 20/21 之后、
--      销售员 80 之前）。
INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope, sort_order)
SELECT '20000000-0000-4000-8000-0000000f0016'::uuid, 'HR_MANAGER', '人事经理',
       'ARCH-9-2：员工档案 / 考勤 / 薪资维护；无财务过账权（过账另有 salary:post 码）', 1, 3, 25
 WHERE NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL);
```

`data_scope=3`（本部门及以下）：人事经理默认能看自己部门子树，跨部门看全公司要超管或改范围。授权（`sys_role_menu`）：HR_MANAGER 拿全部 36 个码；`FINANCE_MANAGER` 拿薪资查看/过账/导出 4 个；`DEPT_LEADER` 拿员工查看 + 请假审批；全员默认拿 `business:hr:salary:my` + `business:hr:leave:create`。

#### ⑥ `sys_field_mask`：2 条全局脱敏

> **落地记录（HR-05 / t_b85f402e，`V0388__hr_mask_config`）**：下面这段 SQL 的 `gen_random_uuid()`
> 在实施时改成 **固定 UUID**（`…b8800000-0000-4000-8000-000000000001 / …0002`）——
> 随机主键没有幂等性（重放插第二条同 `(表,列,NULL)`，撞 `uk_sys_field_mask_rule` 的 `NULLS NOT DISTINCT`），
> 且 `down` 无处下手（无法按 `(表,列)` 通配删，那会连坐客户加的角色级规则）。
> 另：`110101 + '6,4'` 的实际输出是 `110101****1234`（中间星号**固定 4 个**，`nik-utils::Mask::id_card` 口径），
> 不是「长度 − 前后保留」。

```sql
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT gen_random_uuid(), 'biz_employee', 'id_card', 2, '6,4', NULL, 1, '个人信息保护：身份证号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (SELECT 1 FROM sys_field_mask WHERE table_name='biz_employee' AND column_name='id_card' AND role_id IS NULL AND deleted_at IS NULL);
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, status, reason)
SELECT gen_random_uuid(), 'biz_employee', 'bank_account', 5, '4,4', NULL, 1, '个人信息保护：工资卡号默认脱敏（明文需 business:hr:employee:sensitive）'
 WHERE NOT EXISTS (SELECT 1 FROM sys_field_mask WHERE table_name='biz_employee' AND column_name='bank_account' AND role_id IS NULL AND deleted_at IS NULL);
```

#### ⑦ 附件类型放宽（17 → 21 值）+ 必传规则

> **落地记录（HR-05 / t_b85f402e，`V0389__hr_attachment_doc_types`）—— 本节有 3 处必须按实施口径读**
>
> 1. **不是 16 → 20，是 17 → 21**：V0350 放宽到 16 后，V0368 又加了 `RECEIPT_REFUND`（现网 17 值）。
>    照抄下面这段 20 值列表会**把 `RECEIPT_REFUND` 从 CHECK 里摘掉** —— 存量退款凭据附件在
>    `ADD CONSTRAINT` 校验阶段就失败，整笔迁移回滚。落地版列表 = 现网 17 值 ∪ 新 4 值 = **21 值**。
> 2. **`SALARY_SHEET` 那条规则的 `file_role` 改成 `NULL`（任意角色都算）**，不是下面写的 `'OTHER'`：
>    `'OTHER'` 是上传接口的**入参老别名**，落库时被归一成 `'DEFAULT'`（`attachment::model::ROLE_OTHER`），
>    而规则侧按**字面值**比较（`rule.rs::file_role_filter`）—— 写 `'OTHER'` 永远匹配不到任何附件。
> 3. **`SALARY_SHEET` 那条 `enabled = FALSE`**：本节标题与 §2.1 都写「建议传，**不硬拦**」，
>    而规则表只有 `enabled` 布尔、没有「建议」档。置 `false` 才是准确编码（规则在附件规则页可见，
>    运维要变硬闸时一键打开）；置 `true` 会让任何走 `ensure_attachments` 的工资表动作被**永久硬拦**。
>    `EMPLOYEE_CONTRACT` 那条是 D-5 的**必传**，`enabled = TRUE`。
>
> 另：`biz_attachment_rule` 在老库上可能不存在（没跑过 V0347），放宽与种子整段必须放在
> `DO` 块里用 `to_regclass` 前置判断 —— 写成 `INSERT … WHERE to_regclass(...) IS NOT NULL AND NOT EXISTS (…)`
> 不管用（PG 会先规划整条语句，`NOT EXISTS` 里的表不存在直接报错）。

```sql
-- biz_doc_attachment.ck_biz_doc_attachment_type：16 → 20（+ EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET）
ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT',
                        'FIXED_ASSET','EXPENSE_REPORT','INVOICE','RECEIPT','STOCK_TAKE','PURCHASE_RETURN',
                        'SALES_RETURN','STOCK_TRANSFER','SUPPLIER','CUSTOMER','OPENING_BALANCE',
                        'EMPLOYEE','EMPLOYEE_CONTRACT','LEAVE_REQUEST','SALARY_SHEET'));
-- biz_attachment_rule.ck_biz_attachment_rule_doc_type 同步放宽（该表老库可能不存在 → DO 块 + 存在性守卫）

INSERT INTO biz_attachment_rule (doc_type, file_role, required_at, min_count, max_count, remark)
VALUES
    ('EMPLOYEE_CONTRACT', 'CONTRACT_SCAN', 'SUBMIT', 1, 20, '劳动合同签订/续签必须上传扫描件（ARCH-9-2 D-5）'),
    ('SALARY_SHEET',      'OTHER',         'ACTIVATE', 1, 20, '工资表过账前建议上传签字扫描件（建议传，不硬拦）');
```

#### ⑧ 凭证规则（2 条，口径登记）

```sql
INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, is_active, remark)
SELECT v.rule, v.biz, d.id, c.id, NULL, 1, v.remark
  FROM (VALUES
        ('PAYROLL_ACCRUAL_DEFAULT','PAYROLL_ACCRUAL','5602.10','2211.01',
         'ARCH-9-2 工资计提：借 管理费用-工资 / 贷 应付职工薪酬-工资（多借方按部门展开）'),
        ('PAYROLL_PAYMENT_DEFAULT','PAYROLL_PAYMENT','2211.01','1002',
         'ARCH-9-2 工资发放：借 应付职工薪酬-工资 / 贷 银行存款（+代扣项贷方）')
       ) AS v(rule, biz, debit, credit, remark)
  LEFT JOIN biz_account d ON d.account_code = v.debit  AND d.deleted_at IS NULL
  LEFT JOIN biz_account c ON c.account_code = v.credit AND c.deleted_at IS NULL
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r WHERE r.rule_code = v.rule AND r.deleted_at IS NULL);
```

> **这 2 条规则不驱动分录**：`voucher::create_by_rule` 是单借单贷模型（`erp/voucher.rs` 模块文档），工资凭证是"按薪资项展开的多行"。规则在此只做**口径可见性**（配置页能查到"工资凭证走哪些科目"）与巡检对账锚点；真实分录由 `post_explicit_lines` 生成（§4.6）。

#### ⑨ 审批流程（4 套：定义 + 版本 + 图 + hash）

| `flow_code` | `biz_type` | 节点 | 依据 |
|---|---|---|---|
| `hr_leave` | `HR_LEAVE` | 直属上级 →（≥3 天）人事经理 → END | 长假要人事复核（年假余额、产假证明） |
| `hr_salary` | `HR_SALARY` | 人事经理 → 财务经理（会签）→（应发 ≥ 50 万）总经理 → END | 工资是真出钱；`≥50 万` 与预算审批同档 |
| `hr_salary_adjust` | `HR_SALARY_ADJUST` | 直属上级 → 人事经理 → 财务经理 → END | 调薪直接改后续每月的钱，三签 |
| `hr_employee_resign` | `HR_EMPLOYEE_RESIGN` | 直属上级 → 人事经理 →（关键岗位）总经理 → END | 离职牵动账号权限与交接 |

四条都 `is_system = TRUE`（预置流程客户不能删；`V0233` 就是为漏写 `is_system` 打的补丁，P2-6b 的 H6 不变量会判红），图快照按 `flow::graph::snapshot()` 的确定性序列化算 sha256，写入迁移头注释。**具体图与 hash 由实现卡（HR-26/HR-38）生成**，本卡只定拓扑与审批人来源。

> **落地记录（HR-26 / t_10899fe4，`V0393__hr_leave_flow`）—— 三条必须按实施口径读**
>
> 1. **号段**：`hr_leave_flow` 实落 **V0393**（设计号 V0384 → 曾映射 V0390；V0390/0391/0392 已被
>    成本菜单分组 / 成本流程 / HR-23 考勤锁定守卫实占）。详见 §7.1「第三次号段修正」。
> 2. **实际图形是 5 节点 / 5 边**（不是线性 3 节点）：`s → n_sup(直属上级) → c_days(天数判定)
>    → {e_short→e | e_long→n_hr(人事经理)→e}`，`node_count=5`、`edge_count=5`，
>    `graph_hash = 4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97`。
>
>    | 节点 | 类型 | 审批人来源 | 或签 | 兜底 | 超时 |
>    |---|---|---|---|---|---|
>    | `s` / `e` | START / END | — | — | — | — |
>    | `c_days` | CONDITION | — | — | 兜底边 `e_short`（<3 天 → 结束） | — |
>    | `n_sup` | APPROVAL | `INITIATOR_SUPERIOR`（levels=1，随发起人变化，**不落枚举行**） | ANY | `FALLBACK_ADMIN` | 24h REMIND |
>    | `n_hr` | APPROVAL | `ROLE` = `HR_MANAGER`（`…f0016`） | ANY | `FALLBACK_ADMIN` | 48h REMIND |
>
> 3. ★ **条件字段必须用数值镜像 `form.duration_days_num`**：`leave_handler::leave_form_data` 里
>    `duration_days` 是 **JSON 字符串**（展示口径），而条件求值器 `graph::compare` 对 `gte` 走
>    `Value::as_f64()` —— 字符串左值 ⇒ `TypeMismatch` ⇒ 引擎按「条件不成立」**静默走兜底边**
>    （只有 WARN），长假会**绕过人事复核且不报错**。故 form_data 同时冻结 `duration_days_num`
>    （JSON number，与字符串同源同值），种子条件边只认它；两侧各有一条测试钉死这个契约。

> **落地记录（HR-38 / t_37006311，`V0394__hr_salary_flow` + `V0395__hr_adjust_resign_flow`）
> —— 五条必须按实施口径读**
>
> 1. **号段**：`hr_salary_flow` 实落 **V0394**、`hr_adjust_resign_flow` 实落 **V0395**
>    （设计号 V0385/V0386 → 曾映射 V0391/V0392；V0391 被成本流程、V0392 被 HR-23 考勤
>    锁定守卫、V0393 被 HR-26 请假流程实占）。见 §7.1「第三次号段修正」。
> 2. **薪资单流程实际是 6 节点 / 6 边**（设计表里的线性 4 节点不足以表达「应发 ≥ 50 万」
>    这一步分级）：`s → n_hr(人事经理) → n_fin(财务会签) → c_amt(应发判定)
>    → {e_big→n_gm(总经理)→e | e_small(兜底)→e}`，`graph_hash = 0a296af0…d5d0`。
>
>    | 节点 | 类型 | 审批人来源 | 签名模式 | 兜底 | 超时 |
>    |---|---|---|---|---|---|
>    | `s` / `e` | START / END | — | — | — | — |
>    | `n_hr` | APPROVAL | `ROLE` = `HR_MANAGER`（`…f0016`） | ANY | `FALLBACK_ADMIN` | 48h REMIND |
>    | `n_fin` | **COUNTERSIGN** | `ROLE` = `FINANCE_MANAGER`（`…f0005`）+ `FINANCE_DIRECTOR`（`…f0006`） | **ALL** | `FALLBACK_ADMIN` | 48h REMIND |
>    | `c_amt` | CONDITION | — | — | 兜底边 `e_small`（< 50 万 → 结束） | — |
>    | `n_gm` | APPROVAL | `ROLE` = `GENERAL_MANAGER`（`…f0003`） | ANY | `FALLBACK_ADMIN` | 72h REMIND |
>
> 3. **「财务经理（会签）」落地成 `COUNTERSIGN + ALL` + 两个岗位角色**（财务经理 + 财务总监，
>    同 V0182「采购经理+总经理会签」/ V0191「财务经理+财务总监会签」口径）。**不**把单角色写成
>    `ALL`：那会退化成「该角色所有成员都要点一遍」，一人停用未摘角色即永久卡单（V0359/V0367 的 P0）。
>    ALL 节点同时配 `FALLBACK_ADMIN` + 超时小时（R28 与 `flow_countersign_invariants.sh` 断言①）。
> 4. ★ **条件字段的类型契约（两条，各一套流程）**：
>    · 薪资单：`form.gross_total_num` 必须是 **JSON number**（`gte 500000`）；冻结成字符串
>      ⇒ 求值器 `TypeMismatch` ⇒ 静默走兜底 ⇒ **大额工资绕过总经理审批**。
>    · 离职：`form.is_key_position` 必须是 **JSON boolean**（`eq true`）；冻结成字符串 `"true"`
>      ⇒ `json_eq` 不相等 ⇒ 静默走兜底 ⇒ **关键岗位离职没有总经理签字**。
>    两个契约由 `tests/hr_38_salary_adjust_resign_flow_seed.rs` 在引擎求值器上**正反两向**钉死
>    （数值/布尔命中、字符串不命中）。持有方：HR-39/HR-42 的 `/salary/{id}/submit`、
>    离职提交审批 handler 必须按此冻结 `form_data`。
> 5. **调薪 / 离职没有独立单表**（`FlowBizType::doc_kind()` 已定稿：两者的审批实例 `biz_id`
>    都是 `biz_employee.id`）：调薪 = `s → n_sup → n_hr → n_fin → e`（5/4，三签，
>    `graph_hash = a413971a…9171`）；离职 = `s → n_sup → n_hr → c_key(关键岗位判定)
>    → {e_key→n_gm→e | e_plain(兜底)→e}`（6/6，`graph_hash = ec42328b…8496`）。
>    `n_sup` 是 `INITIATOR_SUPERIOR`，**不落枚举行**（V0355 的教训）。

#### ⑩ `sys_config`：3 个开关

> **落地记录（HR-05 / t_b85f402e，`V0388__hr_mask_config`）**：下面这段用
> `VALUES (gen_random_uuid(), …) … ON CONFLICT (id) DO NOTHING` —— **那个 `ON CONFLICT (id)` 永远不会命中**
> （每次 `gen_random_uuid()` 都是新主键），重放会插 3 条重复键。落地版改成
> **固定 UUID + `WHERE NOT EXISTS (config_key, deleted_at IS NULL)`**（配置键唯一是
> `uk_sys_config_key` 的**未软删部分唯一索引**），重放插 0 行、且不覆盖运维改过的 `config_value`。

```sql
INSERT INTO sys_config (id, config_key, config_value, config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status)
VALUES
 (gen_random_uuid(),'hr.salary.iit_mode','MANUAL','STRING','个税算法：MANUAL 手工录入（一期）/ AUTO 累计预扣法（二期）','hr',10,1,TRUE,FALSE,FALSE,1),
 (gen_random_uuid(),'hr.salary.si_mode','MANUAL','STRING','社保公积金算法：MANUAL 手工录入（一期）/ AUTO 按城市规则（二期）','hr',20,1,TRUE,FALSE,FALSE,1),
 (gen_random_uuid(),'hr.attendance.require_leave_match','TRUE','BOOLEAN','导入考勤时是否校验「无薪假 ≤ 已批准请假天数」','hr',30,1,TRUE,FALSE,FALSE,1)
ON CONFLICT (id) DO NOTHING;
```

（`sys_config.config_level` 是 `SMALLINT`，1 系统级 / 2 业务级，`support/V0009:32`；`config_type` 用大写下划线枚举 `STRING/BOOLEAN/NUMBER`。证据脚本第一版写成了 `'SYSTEM'`/`'string'`，真库直接报 `invalid input syntax for type smallint` —— 已按实测改正。）

### 3.3 二期表 DDL（先写下来，本期**不建**）

```sql
-- 个税累计预扣台账（D-9 二期）。一期不建：手工录个税时没有累计概念
CREATE TABLE biz_iit_ledger (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id             UUID NOT NULL,
    employee_id        UUID NOT NULL REFERENCES biz_employee(id) ON DELETE RESTRICT,
    tax_year           SMALLINT    NOT NULL,               -- 纳税年度
    month_no           SMALLINT    NOT NULL CHECK (month_no BETWEEN 1 AND 12),
    cum_income         NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计收入（应税项合计）
    cum_basic_deduct   NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计减除费用（5000 × 月数）
    cum_special_deduct NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计专项扣除（社保+公积金个人）
    cum_extra_deduct   NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计专项附加扣除（子女教育/房贷/赡养…）
    cum_other_deduct   NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计其他扣除（企业年金/商险）
    cum_taxable        NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计应纳税所得额（不足 0 记 0）
    cum_tax_due        NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 累计应预扣税额
    cum_tax_paid       NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 截至上月累计已预扣
    period_tax         NUMERIC(18,4) NOT NULL DEFAULT 0,   -- 本期应预扣 = 应预扣 - 已预扣
    sheet_id           UUID REFERENCES biz_salary_sheet(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_biz_iit_ledger UNIQUE (org_id, employee_id, tax_year, month_no)
);
```

### 3.4 数据量预估（每客户每年）

| 表 | 100 人客户 | 1000 人客户 | 说明 |
|---|---:|---:|---|
| `biz_employee` | ~110 行（含离职） | ~1100 行 | 每年新增 ≈ 人员流动率 × 人数 |
| `biz_employee_contract` | ~130 行 | ~1300 行 | 一份合同 1~3 年，年增量 ≈ 人数 / 2 |
| `biz_employee_salary` | ~1.4 千行（首年） | ~1.4 万行（首年） | 每人 3~6 个固定项建首份标准（100 人 ≈ 400 行）；每次调薪只给"被调项"插新行（不整组复制），约 600 行/年 |
| `biz_attendance_period` | 12 行 | 12 行 | 一月一批 |
| `biz_attendance_record` | 1200 行 | 1.2 万行 | 一人一月一行 |
| `biz_leave_request` | ~300 行 | ~3000 行 | 人均 3 次/年 |
| `biz_salary_item` | ~15 行 | ~15 行 | 配置表 |
| `biz_salary_sheet` | 12 行 | 12 行 | 一公司一月一张 |
| `biz_salary_detail` | 1200 行 | 1.2 万行 | 一人一月一行 |
| `biz_salary_detail_item` | **~1.8 万行** | **~18 万行** | 一人一月 × 15 项；这是全模块最大的表，也是"公式可解释"的代价 |
| **合计** | **≈2.2 万行/年** | **≈22 万行/年** | 5 年后 1000 人客户 ≈110 万行，单表 PG 无压力（有 `(sheet_id)` / `(detail_id)` 索引，查询永远带 sheet 或 detail 前缀） |
---

## 4. 业务规则与流程

### 4.1 员工与账号的关系（本设计的核心问题）

#### 4.1.1 结论：分离，关联列放员工侧

**四种组合全部合法**，用一张表 + 一个可空唯一列表达：

| 场景 | `biz_employee` | `sys_user` | 例子 |
|---|---|---|---|
| 有员工有账号 | 有行，`user_id` 非空 | 有行，`user_type='HUMAN'` | 办公室职员、车间主任 |
| **有员工没账号** | 有行，`user_id = NULL` | 无行 | 车间工人、保洁、临时工（要算工资、要记考勤，但不登录系统） |
| **有账号没员工** | 无行 | 有行，`user_type` 任意 | 外包运维账号、`ROBOT` 机器人（`system/V0102`）、甲方演示账号 |
| 有账号有员工但不同公司 | 有行（A 公司），`user_id` 非空 | 有行（A 公司） | 多公司下一个账号只挂一家公司的档案（D-2） |

**为什么关联列放 `biz_employee.user_id` 而不是卡面建议的 `sys_user.employee_id`**（这是本卡唯一一处**有意偏离卡面**，理由如下，请评审确认）：

1. **数据权限只能按列比较**。`scope::filter_for` 的 SELF 形态是 `AND {alias}.{owner_col} = ?`（`shared/scope.rs` filter_for ③，参数是当前登录人 `user_id`）。「员工只能看自己的档案/薪资」要么在员工侧有 `user_id` 列，要么每次查 `sys_user` 做子查询——而 `RowFilter` 的结构只支持「列 = 值」(不给调用方拼 SQL 的机会，这是它的安全边界)。放员工侧，SELF 范围零改动即生效。
2. **"员工有没有账号"是员工的属性，不是账号的属性**。`employee.user_id IS NULL` 直接读作"这人没账号"；反过来 `sys_user.employee_id IS NULL` 读作"这账号没员工"，语义都成立，但**列表页的主查询是员工列表**（按部门、在职状态分页），列在主表上免一次 LEFT JOIN。
3. **`sys_user` 是登录路径的热点表**。ARCH-9-1 已经要给它加 `org_id`（§3.2 D 组）；本卡再往同一张表加列，两张卡的迁移会互锁（都改 `sys_user`，谁先谁后都要 rebase）。放员工侧 = **本卡对 `sys_user` 零改动**，两卡可并行。
4. **离职返聘**只改员工表一行（`user_id` 置空 + 新档），不动账号行；账号行有 `uk_sys_user_username`（`system/V0001`）与改密缓存（`must_change_password` 60s 缓存），改它面更大。

**代价（诚实说明）**：账号列表页要显示"关联员工"得 `LEFT JOIN biz_employee e ON e.user_id = u.id`；`SCOPED_TABLES` 里的 `SYS_USER`（`owner_col='id'`）不受影响。运维要按"账号 → 员工"反查时多一次 join，可接受。

#### 4.1.2 一人多号 / 一号多人：明确**不支持**

- **一人多号**（同一人两年内换两次账号）：不支持。做法是"旧账号置 `status=0`、`employee.user_id` 改指新账号"——历史单据按 `employee_id` 归档，不受影响；`biz_status_log.operator_name` 是当时快照（`V0159`），"当时是谁点的"仍可查。
- **一号多人**（共用账号）：**禁止**。这不是技术限制而是内控口径：共用账号 = 审批轨迹失去意义（谁批的都是同一个人）。若客户现有习惯如此，一律要求拆账号，README 写明。
- **一个人在两家公司**：两行 `biz_employee`（工号各一套），各挂各的账号（D-2）。工资按公司各发一份，凭证按 `org_id` 隔离（ARCH-9-1）。

#### 4.1.3 离职流程（含账号与历史单据处置）

```text
① HR 建离职单 biz_employee_resign（离职类型 / 最后工作日 / 交接人 / 原因）
② 提交审批（flow: hr_employee_resign：直属上级 → 人事经理 →（关键岗位）总经理）
③ 审批通过（同一事务）：
     a. employee.status = 'RESIGNED'、leave_date、leave_type     ← state_machine::transition 写 biz_status_log
     b. 账号处置：sys_user.status = 0（停用）+ is_locked 不动    ← 不删、不改 username
     c. 该员工所有 SUBMITTED 状态的请假单 → CANCELLED（reason='离职终止'）
     d. 员工未发的薪资单不受影响（下期照常算，按实际工作天数人工调整）
④ 归档：员工列表默认不显示 RESIGNED；带「含离职」筛选可见
```

**账号为什么不删**：删了之后 `operation_logs` / `auth_logs` / `biz_status_log` / `biz_flow_instance_node` 里的 `user_id` 会变成一处无处可查的 UUID，而这几张表是审计的第一查项（`V0159` 的头注释就是为这个写的）。停用 + 保留是唯一能同时满足"不能再登录"与"历史可回溯"的做法。

**历史单据怎么留**：所有业务表存 `employee_id`（稳定主键）+ 姓名/工号**快照**（审计类）。员工软删（`deleted_at`）也不物理删——`biz_salary_detail.employee_id` 是 `ON DELETE RESTRICT`，物理删会被数据库直接拒绝（证据脚本断言 ⑰ 已验）。主数据改名后列表显示新名（join），工资单显示快照名（当时是谁）。

### 4.2 状态机

#### 4.2.1 员工（`biz_employee.status`）

```text
PROBATION ──转正──> ACTIVE ──停薪留职──> SUSPENDED ──复职──> ACTIVE
    │                  │                     │
    └──────离职────────┴─────────────────────┴──> RESIGNED（终态）
```

| 边 | 守卫 | 谁触发 |
|---|---|---|
| `PROBATION → ACTIVE` | 必须有生效中的劳动合同 | 转正按钮（`employee:update`） |
| `ACTIVE → SUSPENDED` | 原因 ≥ 5 字（进 `biz_status_log.reason`） | 停薪留职（`employee:update`） |
| `SUSPENDED → ACTIVE` | — | 复职 |
| `* → RESIGNED` | **必须走离职单审批通过**（唯一合法入口），且 `leave_date` 必填 | `hr_employee_resign` 审批回调 |

登记进 `state_machine`：`Entity::Employee` + `EMPLOYEE_EDGES`（6 条）+ `EMPLOYEE_REASON_EDGES`（3 条：转正/停薪/离职）+ `EMPLOYEE_LOCKED = ["RESIGNED"]`。**所有状态写入只走 `state_machine::transition`**（它自己写 `biz_status_log`），handler 里不许出现 `match (from, to)`。

#### 4.2.2 请假单（`biz_leave_request.status`）

```text
DRAFT ──提交──> SUBMITTED ──审批通过──> APPROVED ──销假/作废(给原因)──> CANCELLED
  │                 │
  │                 └──驳回/撤回──> DRAFT
  └──删除（软删）──> (deleted_at)
```

- **Approved 之后改数据 = 禁止**：`APPROVED`/`CANCELLED` 是锁定态（`LEAVE_LOCKED`），要改只能作废重开。
- 请假类型是否带薪由 `paid` 决定（`paid=0` 的假才进薪资扣款公式的 `{ATT_LEAVE_UNPAID_DAYS}` 校验基线）。

#### 4.2.3 考勤期间（`biz_attendance_period.status`）

```text
DRAFT ──提交──> SUBMITTED ──锁定──> LOCKED ──解锁(给原因, 需 attendance:lock)──> DRAFT
```

- `LOCKED` 之后**记录不许增删改**（handler 闸 + 巡检 A8）；薪资单引用锁定的考勤期间才允许过账（避免"工资按 A 版算、考勤改成 B 版"）。
- 锁定不校验审批（HR 自己锁），但要 `attendance:lock` 权限码 + 写 `biz_status_log`。

#### 4.2.4 薪资单（`biz_salary_sheet.status`）

```text
DRAFT ──试算──> CALCULATED ──提交──> SUBMITTED ──审批通过──> APPROVED ──过账──> POSTED ──红冲──> REVERSED
  │                │                     │
  └──作废──────────┴─────────────────────┴──> CANCELLED（仅 DRAFT/CALCULATED/SUBMITTED 可作废）
```

- **过账才出凭证**，`APPROVED → POSTED` 在同一事务里做三件事（§4.6）：生成两张凭证 + 回写 `accrual/payment_voucher_id` + 状态流转。失败一律整笔回滚（不存在"审批过了没入账"的中间态——这是 AGENTS.md 约定 11 的红线）。
- `REVERSED` 是终态，**不回到 CALCULATED**：红冲后再改数会留下"红字凭证 + 新数"两套口径，账上说不清。要重发就**新开一张**（同期间撞唯一键 → 必须先把旧单 `deleted_at` 或改用"补发单"，见 §4.8.2）。
- 状态机：`Entity::SalarySheet` + `SALARY_SHEET_EDGES`（7 条）+ `SALARY_SHEET_LOCKED = ["SUBMITTED","APPROVED","POSTED","REVERSED"]`。

### 4.3 审批怎么接（复用流程引擎，不硬编码）

| 单据 | `biz_type` | 提交时做什么 | 审批通过后做什么（bridge 回调） |
|---|---|---|---|
| 请假单 | `HR_LEAVE` | `DRAFT→SUBMITTED` + 发起实例（写 `approval_instance_id`） | `SUBMITTED→APPROVED`；写 `biz_status_log`；不改考勤（考勤由导入/人工体现，daemon 只做校验） |
| 薪资单 | `HR_SALARY` | `CALCULATED→SUBMITTED` + 发起实例 | `SUBMITTED→APPROVED`（**不出凭证**） |
| 调薪单 | `HR_SALARY_ADJUST` | `DRAFT→SUBMITTED` | 通过后写 `biz_employee_salary` 新版本行（旧行 `effective_to` 收口），`SUBMITTED→APPROVED` |
| 离职单 | `HR_EMPLOYEE_RESIGN` | `DRAFT→SUBMITTED` | 见 §4.1.3 ③ 四件事（同事务） |

**四条硬口径**（与 `erp/expense_bridge.rs` 的四个入口同款，`erp/expense_bridge.rs` 模块文档）：

1. 回调在**审批事务提交之后**执行 → 业务侧失败**不许**回滚审批单，只记 `error` 日志 + 让单据停在 `SUBMITTED`（可由 HR 手工重试"过账"按钮）。
2. 自审批：`flow::self_approval` + 每个 bridge 的 `guard_self_approval`。工资单的发起人（HR）不能是财务经理那一签的同一人；**员工不能批自己的请假单**（用 `INITIATOR_SUPERIOR` 取直属上级，解析不到走 `FALLBACK_ADMIN`，不用 `SKIP`——`SKIP` 会把"没人可派"变成自动通过，`V0243` 头注释已把这个教训写死）。
3. 未配置已发布流程 → **fail-closed**：`approval_gate::authorize_missing_flow`，409 + `APPROVAL_FLOW_NOT_CONFIGURED`（点名 `flow_code`），要跳过必须有 `business:erp:skip-approval` + ≥10 字理由。**不做"没流程就直接生效"**。
4. 单据作废时必须终止在跑的实例（`cancel_approval_of`），否则孤儿实例会顶住 `uk_biz_flow_instance_biz_active`（`erp/expense_handler::cancel` 的既有做法）。

### 4.4 薪资计算公式的配置方案（本设计的第二个核心）

#### 4.4.1 五类薪资项（`item_type`）—— 决定"进不进应发、要不要科目"

| 类型 | 语义 | 进应发？ | 进实发？ | 科目 | 一期例 |
|---|---|:--:|:--:|---|---|
| `EARNING` | 收入项 | **+** | 间接 | 借 费用 / 贷 2211.01 | 基本工资、津贴、绩效、加班费、全勤奖 |
| `PRE_DEDUCT` | 应发前扣减（考勤扣款） | **−** | 间接 | 借 费用（负向，净额法）/ 贷 2211.01 | 事假扣款、迟到扣款 |
| `WITHHOLD` | 代扣项（形成负债） | — | **−** | 贷 代扣负债科目 | 社保个人、公积金个人、个税、其他代扣 |
| `EMPLOYER` | 单位承担（进费用不进应发） | — | — | 借 费用 / 贷 2211.02/03 | 单位社保、单位公积金 |
| `SUMMARY` | 汇总项（只展示与校验） | — | — | 无 | 应发合计、实发合计 |

恒等式（**库层 `CHECK` 兜死**，见 §3.1 ⑧⑨）：

```text
应发 gross   = Σ EARNING − Σ PRE_DEDUCT
代扣 withhold = 社保个人 + 公积金个人 + 个税 + 其他代扣
实发 net     = 应发 − 代扣
计提总额     = 应发 + 单位社保 + 单位公积金
```

> **为什么不把"考勤扣款"做成 `WITHHOLD`**：它是"应发算少一点"，不是"发的时候扣走"。做成 `WITHHOLD` 会造出"贷方挂着社保中心/谁？"这种无主体的负债，而且计提凭证的贷方虚增（先全额计提再冲回），月末 2211 余额对不上。

#### 4.4.2 公式 DSL（受限表达式，手写解析器）

**语法（白名单，EBNF）**：

```ebnf
expr     := or_expr
or_expr  := and_expr ( '||' and_expr )*
and_expr := cmp_expr ( '&&' cmp_expr )*
cmp_expr := add_expr [ ( '==' | '!=' | '>=' | '<=' | '>' | '<' ) add_expr ]
add_expr := mul_expr ( ( '+' | '-' ) mul_expr )*
mul_expr := unary   ( ( '*' | '/' ) unary   )*
unary    := '-' unary | primary
primary  := NUMBER | VAR | FUNC '(' args ')' | '(' expr ')'
VAR      := '{' [A-Z][A-Z0-9_]* '}'
FUNC     := ROUND(x[,n]) | MIN(a,b) | MAX(a,b) | ABS(x) | CEIL(x) | FLOOR(x) | IF(cond,a,b)
NUMBER   := [0-9]+ ('.' [0-9]+)?
```

**求值约定（每条都有理由）**：

| 约定 | 理由 |
|---|---|
| 全程 `Decimal`，中间结果保留 10 位小数，**最终 `ROUND_HALF_UP` 到 2 位** | 用 `f64` 会出现 `0.1+0.2=0.30000000000000004`，工资条上少一分钱客户会打电话来；仓内已统一 `rust_decimal`（`nik::prelude::rust_decimal`） |
| **除零 → 报错**（不是 0、不是 NULL） | 静默 0 会让"应出勤 0 天却算出扣款 0"这种错误一路走到凭证；与 `report::eval_formula` 的取向一致（`erp/report.rs:494-496`：引用不存在的项必须报错而不是算 0） |
| **未定义变量 → 报错**，并回「变量名 + 可用变量清单」 | 同上；且客户改公式最常犯的错就是写错编码 |
| **环依赖 → 报错**（拓扑排序时检测） | `A={B}+1` 且 `B={A}+1` 会死循环；报错时点名环上的项 |
| `IF` 的两分支**都不求值**（惰性） | 否则 `IF({ATT_SHOULD_DAYS} == 0, 0, {BASE}/{ATT_SHOULD_DAYS})` 在应出勤 0 天时必然除零 |
| 公式长度 ≤ 500（列宽）、禁用 `;` `--` `/*` 与 SQL 关键字 | 与 `flow::linkage::spec` 的 `ck_flow_linkage_no_sql_key` 同一口径（配置里不许出现 SQL 片段，`V0254` 头注释） |
| **前端不做求值**，只调 `POST /salary/calc/preview` 拿后端结果 | 两套实现 = 两套精度/两套 `ROUND` 口径；前端只做语法高亮与变量补全 |

**为什么不用现成的 `report::eval_formula`**：它只支持 `+`/`-` 与空格分隔的项编码（`erp/report.rs:496-520`），连括号都没有，工资的 `ROUND(.../21.75*2, 2)` 写不出来。**为什么不引第三方表达式库**：`AGENTS.md` 约定 3 只允许 `nik` 一个框架 crate（+`nik-utils`），而且通用 eval 库（`evalexpr`/`rhai`）带函数注册/字符串/循环，安全面比"20 行 tokenizer + 100 行递归下降"大得多。

#### 4.4.3 一期种子：15 个计算项 + 2 个汇总项

| # | `item_code` | 名称 | 类型 | 算法 | 公式 / 取数 | 借 | 贷 |
|:--:|---|---|---|---|---|---|---|
| 10 | `BASE` | 基本工资 | EARNING | FIXED | 取员工薪资标准 | 5602.10 | 2211.01 |
| 20 | `POST` | 岗位津贴 | EARNING | FIXED | 取员工薪资标准 | 5602.10 | 2211.01 |
| 30 | `MEAL` | 餐补 | EARNING | FIXED | 取员工薪资标准 | 5602.10 | 2211.01 |
| 40 | `PERF` | 绩效奖金 | EARNING | INPUT | 手工/导入 | 5602.10 | 2211.01 |
| 50 | `OT_WD` | 平时加班费 | EARNING | FORMULA | `ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)` | 5602.10 | 2211.01 |
| 60 | `OT_WE` | 休息日加班费 | EARNING | FORMULA | `ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)` | 5602.10 | 2211.01 |
| 70 | `OT_HO` | 节假日加班费 | EARNING | FORMULA | `ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)` | 5602.10 | 2211.01 |
| 80 | `ATT_BONUS` | 全勤奖 | EARNING | FORMULA | `IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)` | 5602.10 | 2211.01 |
| 90 | `ATT_DEDUCT` | 考勤扣款 | PRE_DEDUCT | FORMULA | `ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)` | 5602.10 | 2211.01 |
| 100 | `SI_PERSONAL` | 社保(个人) | WITHHOLD | INPUT | 手工（二期自动） | — | 2241.03 |
| 110 | `HF_PERSONAL` | 公积金(个人) | WITHHOLD | INPUT | 手工（二期自动） | — | 2241.04 |
| 120 | `IIT` | 个人所得税 | WITHHOLD | INPUT | 手工（二期累计预扣法） | — | 2221.03 |
| 130 | `OTHER_WITHHOLD` | 其他代扣 | WITHHOLD | INPUT | 手工（借支扣回/罚款） | — | 1221 |
| 140 | `SI_COMPANY` | 单位社保 | EMPLOYER | INPUT | 手工（二期自动） | 5602.11 | 2211.02 |
| 150 | `HF_COMPANY` | 单位公积金 | EMPLOYER | INPUT | 手工（二期自动） | 5602.12 | 2211.03 |
| 160 | `GROSS` | 应发合计 | SUMMARY | FORMULA | `{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}` | — | — |
| 170 | `NET` | 实发合计 | SUMMARY | FORMULA | `{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}` | — | — |

`21.75` 是法定月计薪天数（《关于职工全年月平均工作时间和工资折算问题的通知》劳社部发〔2008〕3 号），写在公式里而不是代码里 —— 客户要改成 21.75/22/当月实际天数，改一行公式即可。

> **`SUMMARY` 项（`GROSS`/`NET`）不落 `biz_salary_detail_item`**：它们的值已经写在 `biz_salary_detail` 的 `gross_amount` / `net_amount` 列上（库层 `CHECK` 保证恒等），再存一遍就是同一事实存两处。`detail_item` 只存 **15 个计算项**（8 收入 + 1 应发前扣减 + 4 代扣 + 2 单位承担），行数 = 人数 × 15 × 月数。

#### 4.4.4 试算演算（真实数字，已在 PG 上算过，见 §7.5 证据）

**输入**：张三，`BASE=8000`、`POST=500`、`MEAL=300`；考勤 `should_days=22`、`actual_days=20`、`leave_unpaid_days=2`、`OT_WD=10` 小时；`PERF=1200`；社保个人 840、公积金个人 960、个税 0、其他 0；单位社保 1680、单位公积金 960。

| 项 | 计算 | 金额 |
|---|---|---:|
| 日薪 | `8000 / 21.75` | 367.82 |
| 时薪 | `367.82 / 8` | 45.9770 |
| `OT_WD` | `ROUND(45.9770 × 10 × 1.5, 2)` | **689.66** |
| `ATT_BONUS` | `IF(旷工==0 && 无薪假==0, 200, 0)` → 有 2 天事假 | 0 |
| `Σ EARNING` | `8000+500+300+1200+689.66` | **10689.66** |
| `ATT_DEDUCT` | `ROUND(367.82 × 2, 2)` | **735.63** |
| 应发 `GROSS` | `10689.66 − 735.63` | **9954.03** |
| 代扣合计 | `840 + 960 + 0 + 0` | **1800.00** |
| 实发 `NET` | `9954.03 − 1800.00` | **8154.03** |
| 计提总额 | `9954.03 + 1680 + 960` | **12594.03** |

> 注意 `ROUND(367.82 × 2, 2) = 735.64`，而 `ROUND(8000/21.75×2, 2) = 735.63` —— **中间结果不许提前四舍五入**（DSL 用 10 位中间精度），否则与客户手工 Excel 差 1 分钱。这一条要写进单测（HR-32）。

### 4.5 社保 / 公积金 / 个税：一期手工，二期自动（D-9）

#### 4.5.1 一期做法

- 社保/公积金/个税都是 `WITHHOLD`（个人）与 `EMPLOYER`（单位）类型的 `INPUT` 项，**金额由 HR 从社保网厅/税务系统导出后导入或手填**。
- 三个 `sys_config` 开关（`hr.salary.iit_mode=MANUAL` 等）作为"二期切自动"的闸：切 `AUTO` 前必须有对应规则数据，否则试算直接报错（不许静默按手工口径算）。

#### 4.5.2 二期：个税累计预扣法（给出完整公式，实现卡照抄）

```text
累计应纳税所得额 = 累计收入 − 累计减除费用(5000 × 在本单位任职月数)
                  − 累计专项扣除(社保个人 + 公积金个人)
                  − 累计专项附加扣除(子女教育/继续教育/住房贷款利息/住房租金/赡养老人/婴幼儿照护)
                  − 累计其他扣除(企业年金/商业健康险/税延养老)
（若 < 0 记 0）

本期应预扣预缴税额 = ROUND(累计应纳税所得额 × 预扣率 − 速算扣除数 − 累计已预扣预缴税额, 2)
```

| 累计应纳税所得额区间 | 预扣率 | 速算扣除数 |
|---|---:|---:|
| ≤ 36,000 | 3% | 0 |
| 36,000 ~ 144,000 | 10% | 2,520 |
| 144,000 ~ 300,000 | 20% | 16,920 |
| 300,000 ~ 420,000 | 25% | 31,920 |
| 420,000 ~ 660,000 | 30% | 52,920 |
| 660,000 ~ 960,000 | 35% | 85,920 |
| > 960,000 | 45% | 181,920 |

**7 月跳档实例**（月薪 10,000、无专项附加扣除）：1~7 月累计应纳税所得额 = 35,000 → 3% → 累计应预扣 1,050；8 月累计 40,000 → 进 10% 档 → 累计应预扣 `40,000×10% − 2,520 = 1,480` → **8 月单月扣 430**（而 1~7 月每月才 150）。这就是为什么一期绝对不能"按月 × 3%"硬算 —— 台账（`biz_iit_ledger`）是二期的前置。

### 4.6 工资凭证的勾稽设计（借贷科目）

#### 4.6.1 两张凭证（D-11）

**① 计提凭证**（`source_type='PAYROLL_ACCRUAL'`，期间 = 薪资期间）

```text
借 5602.10 管理费用-工资           应发 9,954.03   （aux_dept_id = 部门）
借 5602.11 管理费用-社保(单位)      1,680.00      （aux_dept_id = 部门）
借 5602.12 管理费用-公积金(单位)      960.00
    贷 2211.01 应付职工薪酬-工资          9,954.03
    贷 2211.02 应付职工薪酬-社保(单位)    1,680.00
    贷 2211.03 应付职工薪酬-公积金(单位)    960.00
借贷合计 12,594.03 = 12,594.03 ✔
```

**② 发放凭证**（`source_type='PAYROLL_PAYMENT'`，期间 = **实际过账日**所在期间）

```text
借 2211.01 应付职工薪酬-工资        9,954.03
    贷 1002 银行存款                      8,154.03
    贷 2241.03 其他应付款-社保(个人)        840.00
    贷 2241.04 其他应付款-公积金(个人)      960.00
    （贷 2221.03 应交个人所得税 —— 本期 0，**不落行**）
借贷合计 9,954.03 = 9,954.03 ✔
```

#### 4.6.2 三条硬规则

1. **净额法汇总借方**：计提凭证的借方按 `(费用科目, dept_id)` 汇总 `Σ EARNING − Σ PRE_DEDUCT`。理由：`biz_voucher_entry` 的单边 CHECK 要求每行金额 `> 0`（`V0149:100-110`），不能写负数行；而"考勤扣款"已经在 `PRE_DEDUCT` 里是正数，直接与收入相减得净额。若某部门净额为 0 → 不落行；若净额为负 → **拒绝过账**（400，提示"部门 X 考勤扣款超过收入"）。
2. **零金额项不落行**：`amount = 0` 的薪资项在两张凭证里都不产生分录（否则撞单边 CHECK）。但 `detail_item` 里**保留 0 值行**（"全勤奖 0"是有信息量的）。
3. **部门辅助核算**：借方与贷方都带 `aux_dept_id`（`5602.1x`、`2211.0x` 的 `aux_dimensions='DEPT'`，见 §3.2 ①），所以既能出"全公司工资"，也能出"研发部工资"。

#### 4.6.3 与财务报表的勾稽

| 报表 | 取数 | 变化 |
|---|---|---|
| 资产负债表 `PAYROLL_PAYABLE` | 映射 `2211`（`V0183:216`），`match_account` 按前缀汇总（`erp/report.rs:485-487`） | **报表 SQL 不改**；上线后这一行从常年 0 变成真实余额 |
| 利润表 `MANAGE_EXPENSE` | 映射 `5602`（`V0183`） | 工资/社保/公积金自动进管理费用；生产人员工资若要进成本，把 `BASE` 的借方科目改成 `4001/4101`（配置即可，不改代码） |
| 现金流量表 | `支付给职工以及为职工支付的现金`（`V0185` 已含 `2211`，行 98） | 发放凭证贷 `1002` → 自动进"支付给职工"类目 |
| 试算平衡 | `SUM(debit) = SUM(credit)` | 两张凭证都平衡（`ck_biz_voucher_balance`）；红冲走借贷互换（`V0149` 头注释） |

#### 4.6.4 过账事务（不许留中间态）

```text
BEGIN
  SELECT ... FROM biz_salary_sheet WHERE id=? FOR UPDATE          -- 行锁
  校验：status=APPROVED；考勤期间 LOCKED；明细恒等式复算通过
  ① post_explicit_lines(计提凭证)  → accrual_voucher_id
  ② post_explicit_lines(发放凭证)  → payment_voucher_id
  ③ UPDATE biz_salary_sheet SET status='POSTED', 两个 voucher_id, posted_at/by
  ④ state_machine::transition(APPROVED → POSTED)（写 biz_status_log）
COMMIT
```

任一步失败 → 整笔回滚，单据仍是 `APPROVED`，界面上"过账"按钮还能再点（**这就是 AGENTS.md 约定 11 要求的状态**：不用工程师改库就能恢复）。

##### 落地记录（HR-39，2026-11-12）

代码：`backend/src/business/hr/salary_bridge.rs`（过账 + 审批回调桥）、
`salary_sheet_handler.rs` 的 #42/#43 两条路由、`salary_sheet_model.rs` 的 4 个状态回写 CAS。

| 设计条目 | 落地方式 | 偏差 / 说明 |
|---|---|---|
| §4.6.2 ① 净额法汇总借方 | `plan_accrual`：按 `(费用科目, dept_id)` 组内 `Σ EARNING − Σ PRE_DEDUCT`；净额 0 → 不落行；净额 < 0 → 409 点名「科目 + 部门 + 净额」 | 无偏差 |
| §4.6.2 ② 零金额项不落行 | `plan_accrual` / `plan_payment` 入口 `amount.is_zero()` 直接跳过；组内净额 0 也丢 | 无偏差（`detail_item` 里仍保留 0 值行） |
| §4.6.2 ③ 部门辅助核算 | 借方与贷方都对声明了 `DEPT` 的科目带 `aux_dept_id`；科目是否带 `DEPT` 由 `biz_account.aux_dimensions` **现查**（`dept_aware_accounts`），不在代码里写死科目号 | `2241.0x` / `2221.03` / `1221` / `1002` 没有 `DEPT` 维度 → **不带**部门（给它们带部门会让按部门汇总的辅助账多一层永远对不上的切分） |
| §4.6.1 ② 贷 1002 银行存款 | 倒挤：`借方合计 − 代扣合计`；倒挤为负 → 409；为 0 → 不落银行行 | 科目来源 = 规则 `PAYROLL_PAYMENT_DEFAULT` 的贷方，规则缺失/失效回落按编码 `1002` 取末级（客户把银行裂成 `1002.01/02` 时规则仍指父科目，直接 409 会把整张工资单卡死） |
| §4.6.4 过账事务 | 单一事务：锁单 → 状态闸 → 考勤期间 `LOCKED` → 明细恒等式复算（巡检 A6 在线版）→ 两张 `post_explicit_lines` → 状态机 `APPROVED→POSTED` → CAS 回写 → commit | 无偏差；`commit` 之后只读回执，无「提交后失败」分支 |
| §4.3 审批回调 | `on_salary_settled`：`APPROVED` → `SUBMITTED→APPROVED`（**不出凭证**）；`REJECTED/WITHDRAWN/CANCELLED/TERMINATED` → `SUBMITTED→CALCULATED`；`PENDING` 一律不动 | 复查 `approval_instance_id` 防陈旧回调；重复回调幂等跳过 |
| §4.3 第 2 条 自审批 | `guard_self_approval`：`initiator_id == operator` → 403 + 65019，挂在 `flow::handler::approve_task` 的守卫链上 | 薪资单没有「员工本人」语义（批的是全公司汇总），所以只判发起人 |
| 提交时冻条件边字段 | `submit_sheet` 把 `form["gross_total_num"]` 冻成 **JSON number**（同值另存字符串 `gross_total` 供展示） | HR-38 在迁移侧钉死了 `condition_expr`，本卡在提交侧钉死左值类型；两侧单测各守一头 |

**已知不做**（属别的卡）：红冲（HR-40，两张红字凭证 + `reversal_voucher_id`）、工资表导出（HR-42）、
`/salary/my`（HR-41）、巡检脚本 `hr_invariants.sql`（HR-45）。

### 4.7 与现有模块的关系

#### 4.7.1 任务工时（`biz_task_worklog`）—— 不混用

`biz_task_worklog`（`V0100:208-224`）是"某个任务上花了多少小时"（`task_id + user_id + work_date + hours`），用途是**项目进度与工单成本**；考勤是"这个人这个月上了多少天班"，用途是**发工资**。两者口径不同（任务工时可能一天 12 小时、也可能一天 0 小时，仍算出勤 1 天），**不做同步、不做汇总**。要出"人均工时/项目人工成本"，用 `biz_task_worklog` join `biz_employee`（按 `user_id`），不改表。

#### 4.7.2 ARCH-4 MES 计件工资 —— 单一凭证出口（本卡拍板）

- MES 的 `biz_work_settlement_line.net_amount` 是"计件应发"，它已经是**金额**而不是工时。
- 本设计在 `biz_salary_item.source='MES'` 预留一个薪资项（如 `PIECE`，`calc_type='INPUT'`，由 MES 结算汇总值填入 `detail_item`）。这样：
  - 工资表只有一张，凭证只有一套（计提/发放各一张）；
  - MES 结算单**不再自己出凭证**（要改 ARCH-4 §4.3.7 的 `voucher_id` 段，两模块都在设计期，改期零成本）；
  - 若客户不买 MES，`PIECE` 项停用即可，薪资照常跑。
- **反向约束**：本卡不实现 MES；若 MES 先上线而 HR 未上线，MES 仍可自行出凭证，两者不会同时存在（上线顺序在 §7.4 里写死）。

#### 4.7.3 数据权限（D-12）

| 表 | 是否进 `SCOPED_TABLES` | 锚点 | 说明 |
|---|:--:|---|---|
| `biz_employee` | **是** | `dept_col='dept_id'`, `owner_col='user_id'` | 「仅本人」= 看自己的档案；`user_id IS NULL` 的员工对 SELF 范围不可见（他没有"本人"视角），部门/全部范围正常可见 |
| `biz_salary_detail` | **是** | `dept_col='dept_id'`, `owner_col='user_id'` | 员工只看自己那一行；`user_id` 是试算时从 `employee.user_id` 派生的**冗余列**（不是 join，因为 `filter_for` 只做列比较） |
| `biz_salary_sheet` | **否**（`unscoped`） | — | 单头是全公司合计，SELF 语义不成立；可见性由 `business:hr:salary:list` 权限码控制 |
| `biz_attendance_record` | **是** | `dept_col='dept_id'`, `owner_col=NULL` | 员工能否看自己考勤？一期由 `attendance:record` 码控制（不给普通员工），SELF 视角走 `/attendance/my` 接口按 `employee.user_id` 过滤 |
| `biz_leave_request` | **是** | `dept_col='dept_id'`, `owner_col=NULL` | 同上；员工自助走 `/leave/my` |
| `biz_salary_item` / `biz_employee_salary` | 否（配置表） | — | 薪资项是配置；`biz_employee_salary` 是配置数据，按 `employee:detail` + `sensitive` 控制 |

**新增 3 个 `ScopeTarget` 常量**：`BIZ_EMPLOYEE`、`BIZ_SALARY_DETAIL`、`BIZ_ATTENDANCE_RECORD`（`shared/scope.rs` 的 `SCOPED_TABLES` 是开放清单，加常量 + 过单测 `scoped_tables_are_sane` + 启动自检 `preflight_permission` 校验列存在）。

### 4.8 特殊场景

#### 4.8.1 跨期（计提在 11 月、发放在 12 月）

- 计提凭证期间 = 薪资单 `period_code` 所在会计期间；发放凭证期间 = **过账日**所在会计期间（两者都必须是 `OPEN`，否则 `post_voucher` 的期间闸会拒绝）。
- 11 月末 2211 有贷方余额 = 已计提未发放工资，这是**正确**的资产负债表状态，不是 bug。
- 若 12 月期间已关账 → 发放过账报 409 + 点名期间，HR 找财务开期间（不给"强制记账"后门）。

#### 4.8.2 红冲 / 重发

- `POSTED → REVERSED`：生成**两张红字凭证**（借贷互换、金额恒正、`is_red=1`、`reverse_of` 指向原凭证、走 `PZR` 号段），薪资单状态 `REVERSED` + `reversal_voucher_id` 只记第一张（第二张通过 `reverse_of` 反查）。
- 红冲后同期间**不能再开新单**（`uk_biz_salary_sheet_period`）；重发要么把红冲单软删再开新单（审计上"删除"有痕：`deleted_at` + 状态日志），要么用**补差单**（`period_code` 用当月，`remark` 写"补发 X 月差额"）—— 一期选后者，前者留作运维手段。

##### 落地记录（HR-40，2026-11-13）

代码：`salary_bridge.rs`（`reverse_salary` / `reverse_in_tx` / `lock_original` / `reverse_one`）、
`salary_sheet_handler.rs` 的 #44 路由、`salary_sheet_model.rs` 的 `mark_reversed`、
`salary_sheet_dto.rs` 的 `SalaryReverseReq` + `parse_reverse_reason`、迁移 `V0397`。

| 设计条目 | 落地方式 | 偏差 / 说明 |
|---|---|---|
| 两张红字凭证 | 逐张调 `erp/voucher.rs::reverse_posted`（借贷互换 + 金额恒正 + `is_red=1` + `reverse_of` + 独立号段 `PZR-` + 原凭证回写 `reversed_by` + 原期间已结账则拒绝）；本卡只负责「串两张 + 状态机 + 回写单据」 | 无偏差（不手写分录：红冲算法只有一份） |
| `reversal_voucher_id` 只记第一张 | CAS 回写记**计提凭证的红字**；发放凭证的红字靠 `biz_voucher.reverse_of = payment_voucher_id` 反查 | 无偏差 |
| 原因必填 | 两道：DTO `parse_reverse_reason`（trim 后 ≥5 字、≤255 字 = `reverse_reason` 列宽）→ 状态机 `POSTED → REVERSED` 是必填原因边（第二道守内部调用）；原因同时落 `reverse_reason` + `biz_status_log.reason` + 两张红字凭证的 `remark` | 无偏差 |
| 状态机 | `SALARY_SHEET_EDGES` 的 `POSTED → REVERSED`（`REVERSED` 仍属 `SALARY_SHEET_LOCKED` 冻结态） | 无偏差；单据 `REVERSED` 是终态，不回 `CALCULATED` |
| **红冲后可重算** | 迁移 `V0397` 把 `uk_biz_sal_sheet_period` 谓词扩成 `deleted_at IS NULL AND status <> 'REVERSED'`，并同步改 `salary_compute::existing_sheet_no` 的前置检查 | ⚠️ **具名偏差**：本节原文「红冲后同期间不能再开新单」被本卡放松为「**红冲单不占期间位**」。理由：`REVERSED` 是终态 + 接口清单无 DELETE + 唯一键挡路 ⇒ 客户红冲完就再也发不出当月工资，只能找研发改库（撞 `AGENTS.md` 约定 11 的判据）。放松后「补差单」与「软删重开」两条老路仍可用；同款先例见 `V0373` 的 `uk_biz_cost_period`（早就排除 `REVERSED`/`CANCELLED`）。回滚有护栏：`V0397.down.sql` 遇到同期间多张未删除单会 `RAISE EXCEPTION` 而不是硬建索引 |
| 事务与补偿 | 单一事务：锁单 → 状态闸（只 `POSTED`）→ 两张凭证必须在（A8 前置）→ `FOR UPDATE` 锁两张原凭证并核对 `source_id/source_type` → 两张红字 → 状态机 → CAS 回写 | `COMMIT` 之后只读回执、不写库不发通知 ⇒ 无「提交后失败」分支；任一步失败整笔回滚，**两张红字一起消失**、单据仍 `POSTED`，红冲按钮可再点 |

#### 4.8.3 并发

| 竞争点 | 措施 |
|---|---|
| 两人同时试算同一张单 | `SELECT … FOR UPDATE` 薪资单 → 后到的等锁，重算覆盖前一次结果（试算幂等：先删旧明细再写） |
| 两人同时建同期间薪资单 | `uk_biz_salary_sheet_period` 唯一索引兜底 → 后者 409 + 友好文案（"2026-11 的薪资单已存在（SAL-202611-000001）"） |
| 取号并发 | `biz_seq` 单语句 `UPDATE … RETURNING`（行锁，`erp/seq.rs` 模块文档「并发安全」） |
| 试算与考勤导入并发 | 试算读考勤期间状态；若 `DRAFT` 允许试算但**过账要求 `LOCKED`**（避免按未定稿数据发钱） |

#### 4.8.4 期中入职 / 离职 / 调薪

- **一期整月计入**：入职当月按整月发（差额由 HR 在 `detail` 上手工改 `BASE` 金额，说明写 `remark`）；离职当月同理。二期待确认（Q5）是否做按天折算。
- **调薪当月**：按 `effective_from` 落在期间内的新标准取数（`effective_from <= period_end` 且 `effective_to IS NULL OR >= period_start`）；同期间跨调薪日的一期不拆分（Q5）。
- **明细手工覆盖**（HR-37）：允许改任意 `EARNING/PRE_DEDUCT/WITHHOLD/EMPLOYER` 项金额，**改完必须重算汇总 + 重写恒等式**，并写 `biz_status_log`（`action='AMOUNT_OVERRIDE'` + reason）。这是"公式算不准时的人工出口"，没有它客户第一个月就会卡死。

#### 4.8.5 负数与补发

- `biz_salary_detail_item.amount` 允许负数（补发/冲回），但 `biz_salary_detail` 的 10 个金额列全是 `>= 0` 的 CHECK → 负数只能让某个 `EARNING` 变小，不能让应发变负（可能吗？本月无收入只有扣款 → 应发 0，扣款留到下月，一期人工处理，见 Q5）。
- 一张薪资单里"补发上月"的推荐做法：加一个 `EARNING` 项 `BACK_PAY`（`calc_type='INPUT'`，手工填），`expr_snapshot` 记来源说明。**不新开期间**。

### 4.9 巡检断言（`scripts/verify/hr_invariants.sql`，10 条）

| # | 断言 | 级别 |
|:--:|---|---|
| A1 | `biz_employee.user_id` 非空的行，必须指向未软删的 `sys_user`，且 `user_type='HUMAN'` | BLOCKER |
| A2 | 同一 `user_id` 挂两份未删员工档 = 0 行 | BLOCKER |
| A3 | 同一 `employee_id + item_code` 的薪资标准区间**不重叠**（`daterange` 相交检测） | BLOCKER |
| A4 | 状态 `PROBATION/ACTIVE` 的员工，必须有 `status='ACTIVE'` 且覆盖今天的劳动合同 | WARN |
| A5 | 合同 `end_date` 早于员工 `leave_date` 的不一致行 = 0（合同不该在离职后到期） | WARN |
| A6 | 薪资单三方对账：`sheet.gross_total = Σ detail.gross_amount` 且 `sheet.net_total = Σ detail.net_amount` | BLOCKER |
| A7 | `detail` 与 `detail_item` 对账：`Σ item(EARNING) − Σ item(PRE_DEDUCT) = detail.gross_amount`；`Σ item(WITHHOLD) = detail.withhold_amount` | BLOCKER |
| A8 | `status in ('POSTED','REVERSED')` 的薪资单必须有 `accrual_voucher_id` 与 `payment_voucher_id`，且两张凭证存在、`total_debit = total_credit`、`source_type` 正确 | BLOCKER |
| A9 | 已过账薪资单的凭证 `period` 必须存在且 `status <> 'CLOSED'`（防事后解锁期间改数） | BLOCKER |
| A10 | 状态 `SUBMITTED` 的请假单 `approval_instance_id` 非空且实例未终止；状态 `APPROVED` 的必须有已通过的节点 | BLOCKER |

（另有 2 条 WARN 观察项：账号部门与员工部门漂移、考勤天数守恒异常，只报不拦，见 §4.9 脚本头注释。）

> **落地记录（HR-45，t_914199ef）**：10 条断言 = `scripts/verify/hr_invariants.sql`
> （只读，可反复跑：生产 / 演练库 / CI），入口 `scripts/verify/hr_invariants.sh`，
> 造错自检 `scripts/verify/hr_invariants_selftest.sh`（全绿基线 R0 + 逐条注入 I1~I10 + fail-closed F1），
> 全绿基线夹具 `scripts/verify/hr/hr_invariants_fixture.sql`。
> 静态种子完整性由迁移 `V0398__hr_selfcheck` 单独兜底（一次性、缺一即 `RAISE EXCEPTION`）。
>
> **两处具名偏差**（照设计字面写会造成**永久误报**，判据都是「正常经营动作不该报红」）：
>
> 1. **A9** 原文「已过账薪资单的凭证 `period` 必须存在且 `status <> 'CLOSED'`」——月结是常规动作，
>    11 月账结掉后那张 11 月工资凭证就永远是 CLOSED 期间里的凭证，巡检从此恒红、没人再看。
>    设计意图（防结账后改数）改为两条等价强化：①凭证期间必须在 `biz_accounting_period` 有行
>    （否则凭证挂在黑期间，报表/账龄查不到）；②**已结账期间里不许留 `SUBMITTED`/`APPROVED` 的薪资单**
>    —— 这些单永远过不了账（过账期间闸会拒 CLOSED），是必须人工处理的死单。
> 2. **A10** 原文「`APPROVED` 的请假单必须有已通过的节点」——请假提交时若流程**没有审批节点**
>    （免审配置 / 全被跳过），引擎在同一事务里直接把实例与单据一起推到 `APPROVED`，
>    此时可能一个 `APPROVED` 节点行都没有（`leave_handler` 的 `auto_finished` 分支）。
>    改为断言**实例终态 = `APPROVED`**，既容得下免审直过，也拦得住「单据批了实例还在跑」。

---

## 5. 接口设计

### 5.1 通用口径（照 `AGENTS.md` 约定 13 与出参约定）

- 路径**不含 `/api`**（由 `[router] prefix` 注入）；前端 **必须写全 `/api/...`**。
- 出参统一 `header / items / meta`（框架 `Res::page`）；`*_id` 必带 `*_name`（批量 join，不许 N+1）。
- 金额一律 `numeric` → 字符串（`Decimal` 序列化为 `"9954.03"`，避免 JS 浮点）；日期 `YYYY-MM-DD`；时间 `ISO8601 +08:00`。
- 错误出口只有三个名字：handler `ResResult<T>`、model `DbResult<T>`、启动 `nik::Result<T>`；`DbError/io::Error` 直接 `?`。

### 5.2 API 清单（42 条）

#### 员工档案（12 条）

| # | 方法 | 路径 | 入参关键字段 | 出参关键字段 | 权限码 |
|:--:|---|---|---|---|---|
| 1 | GET | `/business/hr/employee/page` | `keyword`(工号/姓名/手机)、`dept_id`、`status[]`、`employment_type`、`include_resigned`、`page/page_size` | `id, emp_no, name, gender, mobile, dept_id, dept_name, position_code, position_name, job_level, employment_type, hire_date, status, status_label, user_id, username, contract_end_date` | `business:hr:employee:list` |
| 2 | GET | `/business/hr/employee/{id}` | — | 全字段 + `dept_name, position_name, job_level_name, education_name` + `sensitive_masked`（`id_card_masked/bank_account_masked`）+ `contracts[]` + `salary_items[]` | `business:hr:employee:detail` |
| 3 | POST | `/business/hr/employee` | 全字段（`emp_no` 留空自动取号） | `id, emp_no` | `business:hr:employee:create` |
| 4 | PUT | `/business/hr/employee/{id}` | 同上（不含 `emp_no`） | `id` | `business:hr:employee:update` |
| 5 | DELETE | `/business/hr/employee/{id}` | — | — | `business:hr:employee:delete` |
| 6 | POST | `/business/hr/employee/{id}/status` | `to_status`、`reason`、`effective_date` | `status` | `business:hr:employee:update` |
| 7 | GET | `/business/hr/employee/{id}/sensitive` | `field`（`id_card`/`bank_account`） | 明文 + 本次访问写 `audit_logs` | `business:hr:employee:sensitive` |
| 8 | POST | `/business/hr/employee/{id}/bind-user` | `user_id` 或 `null` | `user_id, username` | `business:hr:employee:update` |
| 9 | GET | `/business/hr/employee/next-no` | — | `next_no`（`EMP-0002`，不耗号） | `business:hr:employee:create` |
| 10 | POST | `/business/hr/employee/seed-from-users` | `dept_id`、`hire_date`、`dry_run` | `created, skipped, preview[]` | `business:hr:employee:create` |
| 11 | POST | `/business/hr/employee/import` | 走 `impexp`（`biz_type='hr.employee'`） | `task_id` | `business:hr:employee:import` |
| 12 | GET | `/business/hr/employee/export` | 同筛选条件 | xlsx 流 | `business:hr:employee:export` |

**劳动合同**（3 条，挂在员工下）：

| # | 方法 | 路径 | 权限码 |
|:--:|---|---|---|
| 13 | POST | `/business/hr/employee/{id}/contract` | `business:hr:contract:manage` |
| 14 | PUT | `/business/hr/contract/{contract_id}` | `business:hr:contract:manage` |
| 15 | POST | `/business/hr/contract/{contract_id}/renew` | `business:hr:contract:manage`（自动把旧合同 `status='RENEWED'` + `renewed_from_id` 链） |

#### 员工薪资标准（2 条）

| # | 方法 | 路径 | 说明 | 权限码 |
|:--:|---|---|---|---|
| 16 | GET | `/business/hr/employee/{id}/salary` | 当前生效标准 + 历史（按 `effective_from DESC`） | `business:hr:employee:detail` + `business:hr:salary:detail` |
| 17 | POST | `/business/hr/employee/{id}/salary` | 调薪：`items[{item_code, amount}]`、`effective_from`；后端把旧行 `effective_to` 收口 | `business:hr:salary:calc` |

#### 考勤（8 条）

| # | 方法 | 路径 | 说明 | 权限码 |
|:--:|---|---|---|---|
| 18 | GET | `/business/hr/attendance/period/page` | 月度批次列表 | `business:hr:attendance:list` |
| 19 | POST | `/business/hr/attendance/period` | 建期间（`period_code`、`period_start/end`） | `business:hr:attendance:period` |
| 20 | POST | `/business/hr/attendance/period/{id}/status` | 提交 / 锁定 / 解锁（`to_status` + `reason`） | `business:hr:attendance:lock` |
| 21 | GET | `/business/hr/attendance/record/page` | `period_id` + `dept_id` + `keyword` | `business:hr:attendance:record` |
| 22 | PUT | `/business/hr/attendance/record/{id}` | 手工修正（各天数/加班小时） | `business:hr:attendance:record` |
| 23 | POST | `/business/hr/attendance/import` | impexp（`biz_type='hr.attendance'`，带 `period_code`） | `business:hr:attendance:import` |
| 24 | GET | `/business/hr/attendance/export` | xlsx | `business:hr:attendance:export` |
| 25 | GET | `/business/hr/attendance/my` | 本人考勤（按 `employee.user_id`） | 登录即可 |

#### 请假（6 条）

| # | 方法 | 路径 | 说明 | 权限码 |
|:--:|---|---|---|---|
| 26 | GET | `/business/hr/leave/page` | 列表（按 dept 数据权限） | `business:hr:leave:list` |
| 27 | GET | `/business/hr/leave/{id}` | 详情 + 审批轨迹（`flow::instance_linkage`） | `business:hr:leave:detail` |
| 28 | POST | `/business/hr/leave` | 建单（`employee_id`、类型、起止、`duration_days`、`paid`、事由） | `business:hr:leave:create` |
| 29 | PUT | `/business/hr/leave/{id}` | 改草稿 | `business:hr:leave:update` |
| 30 | POST | `/business/hr/leave/{id}/submit` | 提交审批（发起 `hr_leave` 流程） | `business:hr:leave:submit` |
| 31 | POST | `/business/hr/leave/{id}/cancel` | 作废（`reason` ≥ 5 字；APPROVED 也可作废=销假） | `business:hr:leave:cancel` |

#### 薪资项（4 条）

| # | 方法 | 路径 | 说明 | 权限码 |
|:--:|---|---|---|---|
| 32 | GET | `/business/hr/salary-item/page` | 配置列表（含 `formula`） | `business:hr:salary-item:list` |
| 33 | POST | `/business/hr/salary-item` | 新建（校验公式可解析 + 变量可达 + 无环） | `business:hr:salary-item:create` |
| 34 | PUT | `/business/hr/salary-item/{id}` | 改（改公式后**旧薪资单不受影响**，靠 `expr_snapshot`） | `business:hr:salary-item:update` |
| 35 | POST | `/business/hr/salary-item/validate` | 公式语法校验 + 依赖图返回（配置页"检查"按钮） | `business:hr:salary-item:update` |

#### 薪资单（11 条）

| # | 方法 | 路径 | 说明 | 权限码 |
|:--:|---|---|---|---|
| 36 | GET | `/business/hr/salary/page` | 列表（期间、状态、金额合计） | `business:hr:salary:list` |
| 37 | GET | `/business/hr/salary/{id}` | 详情：单头 + 部门汇总 + 明细分页 | `business:hr:salary:detail` |
| 38 | GET | `/business/hr/salary/{id}/detail/page` | 明细分页（含 `detail_item` 展开开关） | `business:hr:salary:detail` |
| 39 | POST | `/business/hr/salary/calc` | 试算（`period_code` + `attendance_period_id` + `dry_run`）→ 返回逐人逐项结果，不落库 | `business:hr:salary:calc` |
| 40 | POST | `/business/hr/salary` | 建单并落库（同上的 `dry_run=false`） | `business:hr:salary:calc` |
| 41 | PUT | `/business/hr/salary/{id}/detail/{detail_id}` | 明细覆盖（改项金额 + `reason`） | `business:hr:salary:calc` |
| 42 | POST | `/business/hr/salary/{id}/submit` | 提交审批 `hr_salary` | `business:hr:salary:submit` |
| 43 | POST | `/business/hr/salary/{id}/post` | 过账（生成两张凭证） | `business:hr:salary:post` |
| 44 | POST | `/business/hr/salary/{id}/reverse` | 红冲（`reason` 必填） | `business:hr:salary:reverse` |
| 45 | GET | `/business/hr/salary/{id}/export` | 工资表 xlsx（按部门分组，含项列） | `business:hr:salary:export` |
| 46 | GET | `/business/hr/salary/my` | 我的薪资（列表 + 明细项），按 `employee.user_id` 过滤 | `business:hr:salary:my` |

> 合计 **46 条**（上表编号 1~46，其中 4 处是 `/my` 自助接口）。**权限码共 30 个**，与 §3.2 ④ 的 30 个按钮一一对应。

### 5.3 分页 / 筛选 / 导出

- 所有列表走 `scope::paginate`（唯一分页入口，`shared/scope.rs:613`）；`salary_sheet` / `salary_item` 走 `paginate_unscoped` + 权限码。
- 员工列表默认筛选 `status IN ('PROBATION','ACTIVE','SUSPENDED')`；`include_resigned=true` 才含离职。
- 导出统一 `xlsx`（`impexp` / `Cargo feature xlsx`），导出任务落 `sys_export_task`（大数据量异步 + 下载中心）。
- 工资表导出列 = 员工列 + 全部启用薪资项（`item_code` 为列，`item_name` 为表头）+ 5 个汇总列；与 `detail_item` 一一对应，方便 HR 与 Excel 对拍（S2 判据的测法）。

### 5.4 关键出参示例（员工详情，含 `*_id/*_name` 成对）

```json
{
  "header": {"code": 0, "message": "ok"},
  "data": {
    "id": "…", "emp_no": "EMP-0007", "name": "张三",
    "dept_id": "…", "dept_name": "生产部",
    "position_code": "WORKER", "position_name": "车间工人",
    "job_level": "P3", "job_level_name": "P3",
    "employment_type": "FULL_TIME", "employment_type_label": "正式",
    "status": "ACTIVE", "status_label": "在职",
    "hire_date": "2024-03-01", "probation_end_date": "2024-06-01",
    "leave_date": null, "leave_type": null, "leave_type_label": null,
    "education": "SENIOR", "education_name": "高中",
    "user_id": null, "username": null,
    "id_card_masked": "110101********0011",
    "bank_account_masked": "6222********1234",
    "contract": {"id": "…", "contract_no": "HT-2024-007", "contract_type": "FIXED_TERM",
                 "start_date": "2024-03-01", "end_date": "2027-02-28", "status": "ACTIVE",
                 "status_label": "生效中", "expire_in_days": 476}
  }
}
```

> `username`/`dept_name`/`position_name` 全部由**批量 join** 取出（列表页一次 join，不做 N+1）；`*_masked` 由 `sys_field_mask` 规则生成（全局规则 → 所有角色都遮），明文走 `/employee/{id}/sensitive`（要 `sensitive` 码，且写审计日志）。
---

## 6. 前端设计

### 6.1 页面清单（菜单挂哪、路由 path）

| 菜单（`title` ≤12 字） | 路由 `path` | 组件 | 说明 |
|---|---|---|---|
| 人力资源（目录） | `/business/hr` | `Layout` | 挂**顶级**，`sort_order=77`（实测空闲），`icon='user-business'` |
| 员工档案 | `/business/hr/employee` | `business/hr/employee/index.vue` | 列表 + 抽屉详情 + 新建/编辑抽屉 |
| 考勤管理 | `/business/hr/attendance` | `business/hr/attendance/index.vue` | 两个 tab：**考勤期间**（批次列表 + 锁定）/ **考勤台账**（一人一行 + 导入 + 导出） |
| 请假单 | `/business/hr/leave` | `business/hr/leave/index.vue` | 列表 + 「我的请假」tab |
| 薪资管理 | `/business/hr/salary` | `business/hr/salary/index.vue` | 列表 + 详情页 |
| 薪资项 | `/business/hr/salary-item` | `business/hr/salary-item/index.vue` | 配置列表 + 公式编辑抽屉 |

**薪资单详情**是独立路由页 `/business/hr/salary/:id`（不进菜单，`is_visible=FALSE` 的页面级路由由 router 直接注册），因为它是典型的"单头 + 明细 + 审批轨迹 + 凭证 + 操作栏"四段式，塞抽屉里放不下。

**员工自助**不新增菜单（保持 5 页面，与 §3.2 ④ 种子一致），以**页面内 tab** 提供：

| 自助视图 | 触发方式 | 数据来源 |
|---|---|---|
| 我的档案 | 员工档案页，无 `business:hr:employee:list` 码时自动只显示"我的档案"tab | `GET /employee/{my_employee_id}`（后端按 `employee.user_id = 我` 解析） |
| 我的考勤 | 考勤页 tab | `GET /attendance/my` |
| 我的请假 | 请假页 tab | `GET /leave/page?mine=true` |
| 我的薪资 | 薪资页 tab（或顶栏头像菜单） | `GET /salary/my` |

理由：一期不为自助单独做 4 个页面（菜单项会从 19 涨到 26，侧边栏更挤）；tab 形态对"一个月看一次工资"的场景完全够用；二期待确认（Q6）是否给员工独立门户。

### 6.2 关键交互

#### 6.2.1 员工档案列表

- 列：工号 / 姓名 / 部门 / 岗位 / 状态（`DictTag` 着色：试用蓝、在职绿、停薪灰、离职深灰）/ 入职日期 / 合同到期（`expire_in_days ≤ 30` 时橙字 + tooltip"还有 N 天到期"）/ 手机 / 操作。
- 筛选：关键字（工号/姓名/手机）、部门树（`DeptTreeSelect`）、状态多选、用工形式、**「含离职」开关**（默认关）。
- 行操作：详情 / 编辑 / 调岗 / 调薪 / 离职（后三者按状态与权限码禁用）。
- **绑定账号**：编辑抽屉里的"关联账号"用远程搜索下拉（`sys_user` 中 `user_id` 未被占用的启用账号 + `user_type='HUMAN'`）；已绑定显示 `username` + "解绑"按钮（解绑 = `user_id=NULL`，二次确认："解绑后该账号将无法查看本人的档案与薪资"）。

#### 6.2.2 员工详情抽屉/页

三段式：
1. **基本信息**（含 `*_masked` 字段 + 「查看明文」按钮，点了才调 `/sensitive` 并弹提示"本次查看已记入审计日志"）；
2. **合同台账**（时间线：签订 → 续签 → 解除；每条右侧"扫描件"缩略图，走 `DocAttachmentPanel`）；
3. **薪资标准**（当前生效项 + 生效日期 + 调薪历史表格，金额右对齐）。

按钮态：`RESIGNED` 状态 → 编辑/调岗/调薪/离职全部禁用，只留"查看"与"附件"；`PROBATION` → 显"转正"主按钮。

#### 6.2.3 考勤台账

- 顶部：期间选择（下拉，只列 `DRAFT`/`SUBMITTED`）+ 期间状态标签 + 「导入」「导出」「锁定」按钮。
- 表格列：工号/姓名/部门/应出勤/实际/带薪假/无薪假/病假/旷工/加班(平时/休息日/节假日)/迟到/操作。
- **单元格直接编辑**（`t-input-number`，失焦保存，`PATCH` 单行）——HR 最常做的是改一两个数，抽屉编辑太重。
- 导入：`ImportEntryButton` → 选择 `hr.attendance` 模板 → 上传 → 轮询 `sys_import_task` 状态 → 错误行表格（行号 + 列 + 原因）。
- 锁定前弹确认："锁定后考勤不许修改；薪资单按锁定版本计算"。

#### 6.2.4 薪资项配置（公式编辑）

- 列表：顺序 / 编码 / 名称 / 类型（彩色标签）/ 算法 / 公式（等宽字体截断）/ 科目 / 启用。
- 编辑抽屉：类型选择后**动态显示必填项**（`EARNING` → 借+贷科目必填；`WITHHOLD` → 只贷方；`SUMMARY` → 只公式，科目隐藏）；科目用 `t-select` 远程搜（只列 `is_leaf=1` 的科目，避免选了非末级过账被拒）。
- **公式编辑器**：`t-textarea` + 右侧"变量"面板（可点插入 `{BASE}` / `{ATT_OT_WD}`），下方「检查」按钮调 `/salary-item/validate`，返回：✅ 语法通过 + 依赖顺序（`BASE → OT_WD → GROSS`）或 ❌ 具体错误（"未定义变量 `{BASEE}`，可用：BASE/POST/…"）。**前端不做求值**（§4.4.2）。
- 删除保护：`is_system=1` 的项（`GROSS`/`NET`）不可删、不可停用；被员工薪资标准引用的项删除 → 409 + 列出引用人数。

#### 6.2.5 薪资单详情页

四段式（与 `expense` 详情页同构，照抄布局）：

```text
┌ 单头卡：期间 2026-11 / 状态标签 / 人数 42 / 应发 418,320.55 / 实发 342,110.20 ┐
│ 操作栏 DocActionBar：试算 · 提交 · 过账 · 红冲 · 导出 · 附件               │
├ 汇总卡：按部门（人数 / 应发 / 单位社保 / 单位公积金 / 实发）                │
├ 明细表：一人一行，可展开 → 17 个薪资项（金额 + 算法 + 说明 "8000/21.75*2"）  │
│        改单项金额 → 弹窗填原因 → 保存后单头合计自动刷新                     │
├ 审批轨迹 FlowProgress（复用 views/business/erp/shared/approval-progress.ts）│
└ 凭证卡：计提 PZ-20261130-000012（点击跳凭证详情）/ 发放 PZ-20261205-000003  ┘
```

**按钮态**（`DocActionBar` 的 `actions` 由状态算，不手写 if）：

| 状态 | 可用动作 |
|---|---|
| `DRAFT` | 试算、删除、作废 |
| `CALCULATED` | 试算（重算）、提交、作废 |
| `SUBMITTED` | 撤回（审批页），只读 |
| `APPROVED` | **过账**、作废 |
| `POSTED` | 红冲、导出、打印 |
| `REVERSED` | 只读 + 导出 |

#### 6.2.6 错误文案（前端直出，不暴露错误码）

| 后端场景 | 用户看到 |
|---|---|
| `uk_biz_salary_sheet_period` 冲突 | 「2026-11 的薪资单已存在（SAL-202611-000001），请直接打开它」 |
| 过账时期未开 | 「2026-12 会计期间未开启，请联系财务开期间后再发放」 |
| 考勤未锁定 | 「考勤期间 2026-11 尚未锁定，锁定后才能过账（防止工资按未定稿数据计算）」 |
| 公式解析失败 | 「薪资项「加班费」公式有误：未定义变量 {ATT_OT_WD2}」 |
| 部门净额为负 | 「生产部考勤扣款（1,200.00）超过该部门工资（800.00），请先在考勤台账核对」 |
| 无敏感字段权限 | 「查看完整身份证/银行卡号需要「人事敏感信息」权限，请联系管理员」 |

### 6.3 复用哪些现有组件

| 组件（实测路径） | 用在哪 |
|---|---|
| `views/business/erp/shared/DocActionBar.vue` | 薪资单/请假单操作栏（按钮态由状态算） |
| `views/business/erp/shared/DocAttachmentPanel.vue` | 员工（合同扫描件/学历证明）、请假单（病假证明）、薪资单（签字工资表） |
| `views/business/erp/shared/attachment-gate-pure.ts` + `use-attachment-gate.ts` | 提交前"必传附件"闸（劳动合同扫描件，`biz_attachment_rule` 驱动） |
| `views/business/erp/shared/approval-progress.ts` + `FlowAssigneeDialog.vue` | 请假单审批轨迹、指定审批人 |
| `views/business/erp/shared/CancelImpactDialog.vue` | 薪资单作废/红冲的"影响预览"（会红冲哪两张凭证、哪些期间受影响） |
| `views/business/erp/shared/money.ts` | 金额格式化（千分位 + 2 位小数） |
| `components/DeptTreeSelect.vue` | 员工部门筛选、调岗选部门 |
| `components/DictSelect.vue` / `DictTag.vue` | 岗位/职级/学历/请假类型（字典驱动，不硬编码） |
| `components/ImportEntryButton.vue` | 考勤导入、员工导入 |
| `components/ListMemoryBar.vue` | 列表筛选记忆（员工列表 42 人以上就要记筛选） |
| `components/AttachmentPreview.vue` | 合同扫描件预览 |
| `api/business/erp/doc-kinds.ts`（镜像）+ `attachment.ts` | 新增 4 个 `doc_type` 的标签与必传规则映射 |

---

## 7. 迁移与上线

### 7.1 迁移文件清单（V0378 ~ V0388，11 个文件 / 22 个 up+down）

> **号段修正（2026-11-11 实施时定稿，与代码一致）**
>
> 下表的「设计号」`V0378~V0388` **已不可用**：成本核算（ARCH-9 成本）先落地并已提交，实占了
> `V0378__cost_menu_and_perm` / `V0379__cost_seq_and_dict` / `V0380__cost_lot_backfill` /
> `V0381__cost_flow_def` / `V0382__stock_in_production_sourceless`。本仓迁移加载器对版本号做
> **跨目录全局唯一**校验（重号 ⇒ 服务启动失败，见 `AGENTS.md` 约定 6）。
>
> 故 **HR 全系列整体后移 5 号**：`V0378→V0383` … `V0388→V0393`。下表「实际号」列为落地号，
> HR-01 ~ HR-05 实施时以该列为准（HR-01 已在 `V0383__hr_tables.up.sql` 头注释里写明映射）。
>
> **第二次号段修正（HR-03 实施时，t_7799d567）**：上述映射里的 `V0385` 又被成本核算的灰度开关
> 实占（`V0385__cost_module_switch`，COST-B4-4 / t_e757c1e7，已落入工作区）。HR-03 是第一个
> 撞上的卡（它要写 `V0385__hr_seq_dict`），故 **HR-03 及其后的全部号段再后移 1 号**：
> `hr_seq_dict` V0385→**V0386**，`hr_menu_perms` →**V0387**，`hr_mask_config` →**V0388**，
> `hr_attachment_doc_types` →**V0389**，批 3 三套流程 →**V0390/0391/0392**，
> 批 5 →**V0393/0394**。下表已按此更新；后续卡（HR-04/05/26/35/38/45）以本表**实际号**列为准。
> 规律：**先落地者定号，后落地者避让**（本仓迁移版本号跨目录唯一，重号 = 启动即失败）。
>
> **第三次号段修正（HR-26 实施时，t_10899fe4）**：批 3 的三套流程号段 `V0390/0391/0392` 也已被
> 成本核算与 HR-23 实占 —— `V0390__cost_menu_group`（成本菜单分组）、`V0391__flow_cost_settle_is_system`、
> `V0392__attendance_lock_guard`（HR-23 考勤锁定守卫）。HR-26 要写 `hr_leave_flow`（原映射 V0390），
> 故 **HR-26 取 V0393**，其后各卡顺延：`hr_salary_flow`→**V0394**、`hr_adjust_resign_flow`→**V0395**、
> `hr_salary_items_seed`→**V0396**、`hr_selfcheck`→**V0397**（下表已更新）。
> 这仍是**建议号**：每张卡落地前必须 `ls backend/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`
> 再核对，撞号就继续后移（先落地者定号）。
>
> **第四次号段修正（HR-45 实施时，t_914199ef）**：第三次修正给本卡留的 `V0397` 已被 **HR-40 红冲**
> 实占（`V0397__hr_salary_period_after_reverse`，红冲单不占期间位），故收口自检取 **V0398**
> （`V0398__hr_selfcheck`）。至此人力资源 16 条迁移号为 V0383~V0398 连续无空洞。

| 设计号 | 实际号 | 文件名 | 内容 | 幂等策略 |
|---|---|---|---|---|
| ~~V0378~~ | **V0383** | `hr_tables` | 10 张新表 + 索引 + 注释（§3.1） | `CREATE TABLE/INDEX IF NOT EXISTS` + 末尾 `DO` 自检（10 表 / 12 唯一键带软删 / 49 CHECK / 12 FK / 12 条行为断言 + 探针清理） |
| ~~V0379~~ | **V0384** | `hr_accounts` | 9 个末级科目 + 父科目翻非末级 + 2 条 `biz_voucher_rule` | `WHERE NOT EXISTS (account_code)`；翻末级用「确实有子科目才翻」的条件 UPDATE |
| ~~V0380~~ | **V0386** | `hr_seq_dict` | 4 条号段 + 4 类字典 + 26 项字典值 | `WHERE NOT EXISTS` / `NOT EXISTS (dict_type_id, item_value)`；✅ 已落地（HR-03） |
| ~~V0381~~ | **V0387** | `hr_menu_perms` | 36 行菜单 + `HR_MANAGER` 角色 + `sys_role_menu` 授权 | `ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` |
| ~~V0382~~ | **V0388** | `hr_mask_config` | 2 条 `sys_field_mask` + 3 条 `sys_config` | `NOT EXISTS`（表+列+role）/ `NOT EXISTS`（config_key，未软删）+ 固定 UUID。✅ 已落地（HR-05） |
| ~~V0383~~ | **V0389** | `hr_attachment_doc_types` | `ck_biz_doc_attachment_type` 17→21、`ck_biz_attachment_rule_doc_type` 同步 + 2 条规则 | `DO` 块先读 `pg_get_constraintdef` 判断是否已含新值，已含则跳过（照 `V0350` 的做法）；规则表不存在则整段跳过。✅ 已落地（HR-05） |
| ~~V0384~~ | **V0393** | `hr_leave_flow` | `hr_leave` 流程：定义 + 版本 + 图快照（含 sha256）。✅ 已落地（HR-26）：直属上级 →（≥3 天，条件边 `form.duration_days_num gte 3`）人事经理 → END，5 节点 / 5 边，hash `4fc6722a…` | 4 层 `WHERE NOT EXISTS`（同名流程/同 biz_type 默认流程/角色存在/版本重复）+ 版本号 `MAX+1` |
| ~~V0385~~ | **V0394** | `hr_salary_flow` | `hr_salary` 流程（同上）。✅ 已落地（HR-38）：人事经理 → 财务会签（COUNTERSIGN + 两个角色）→（应发 ≥ 50 万）总经理，6 节点 / 6 边，hash `0a296af0…d5d0` | 同上 |
| ~~V0386~~ | **V0395** | `hr_adjust_resign_flow` | `hr_salary_adjust` + `hr_employee_resign` 两套。✅ 已落地（HR-38）：调薪 5 节点 / 4 边（`a413971a…9171`）、离职 6 节点 / 6 边（关键岗位分叉，`ec42328b…8496`） | 同上 |
| ~~V0387~~ | **V0396** | `hr_salary_items_seed` | 17 个薪资项（§4.4.3 表逐行） | `WHERE NOT EXISTS (org_id, item_code)`；`sort_order` 冲突检查 |
| ~~V0388~~ | **V0398** | `hr_selfcheck` | **收口自检**（✅ 已落地 HR-45）：10 表 / 9 科目 + 4 父级翻非末级 + 2 凭证规则 / 4 号段 + 4 字典 26 项 / 36 菜单 + `HR_MANAGER` 授权 / 2 脱敏 + 3 配置 / 4 流程 / 17 薪资项 全部在位，缺一 `RAISE EXCEPTION` 并一次性列出全部缺项 | 纯 `DO` 块，无副作用（可重复应用）；`.down.sql` 是显式空回滚（本迁移无可反向状态） |

**为什么单独一个收口自检迁移**：本仓的迁移是"文件内不写 BEGIN/COMMIT、由迁移器开事务"，一旦某个种子在某类库（老库/空库/克隆库）上静默没种进去，前面的迁移已经提交、指纹已记，事后只能手写补丁。收口自检把"种齐"变成一个**原子承诺**：任何一条不齐，整批回滚（照 `V0378`/`V0226` 末尾的 `DO $$ … RAISE EXCEPTION` 既有做法）。

### 7.2 存量数据处理（有没有 backfill、要不要 downtime）

| 项 | 处理 | 是否需要停机 |
|---|---|---|
| 10 张新表 | 纯新增，无回填 | 否 |
| `sys_user` 存量账号 ↔ 员工档 | **不自动回填**（人事主档要有工号/入职日期/身份证，机器猜不出来）。上线后由 HR 在员工档案页点「从账号生成员工档」→ `POST /employee/seed-from-users`（先 `dry_run=true` 出预览，确认后落库，`status='ACTIVE'`、`hire_date` 取用户选的日期、工号自动取号） | 否 |
| 9 个科目 + 父科目翻末级 | 纯新增 + 条件 UPDATE；`cloudpivot_test` 实测落在 `2211*` 的分录 = **0 行**，翻末级无历史数据风险 | 否（DDL 毫秒级） |
| 号段 / 字典 / 菜单 / 角色 / 脱敏 / 配置 | 纯新增 | 否 |
| 附件 CHECK 放宽 | 只放宽集合，存量 16 类全部仍合法 | 否 |
| 4 套审批流程 | 纯新增（未发布 → `DRAFT` 或直接 `PUBLISHED`，见 §7.4 批次 3） | 否 |

**唯一需要"重启"的动作**：后端进程重启一次（加载新增的 `DocKind`/`Entity` 枚举、路由、`SCOPED_TABLES`）。这不是停机——本仓部署模型是 `systemctl restart cloudpivot`，前端静态文件双 rename（见 §7.3）。**结论：零停机**（重启窗口 < 10 秒，与既有部署方式一致）。

### 7.3 回滚方案

| 层 | 回滚动作 | 注意 |
|---|---|---|
| 迁移 | 每个 `V03xx__*.up.sql` 配 `.down.sql`，逐条反向：删种子 → 恢复 CHECK 到 16 值 → 删表 | **有业务数据就拒绝回滚**：`biz_salary_sheet` 存在 `POSTED`/`REVERSED` 单时 `RAISE EXCEPTION`（照 `V0347.down.sql` 的既有做法）；`biz_employee` 有未删行时也拒绝 |
| 前端 | `dist` 双 rename 回上一版（`dist-prev-*`） | 与既有上线口径一致（`git` 不管静态文件） |
| 科目 | `down` 删 9 个新科目；**父科目 `is_leaf` 不回翻**（有历史分录风险） | 回滚后 `2211` 仍非末级 —— 这是安全的（非末级科目只是不能再直接记账，读取/报表不受影响） |
| 角色/菜单 | 删 `sys_role_menu` → 删菜单（按固定 UUID 列表）→ 删角色 | 先删授权再删菜单，否则外键报错 |
| 流程 | 删图 → 版本 → 定义（逆序，且先检查无审批实例引用） | 有在跑实例则拒绝 |

### 7.4 分几批上线（能否灰度）

| 批 | 内容 | 能否灰度 | 判据 |
|---|---|---|---|
| **批 1 底座** | V0378~V0383（表/科目/号段/字典/菜单/脱敏/附件放宽） | ✅ 直接上（用户看不到任何新菜单入口，除超管） | 只超管可见 → 内部验收 |
| **批 2 员工档案** | 员工列表/详情/合同/薪资标准 + V0384 前的流程无关项 | ✅ 灰度给 1 个客户/1 个部门 | S1 判据 |
| **批 3 审批流** | V0384~V0386（4 套流程发布） | ✅ 按客户开关（未发布 = 提交时报 409 走 skip-approval 口径） | 与 `approval_gate` fail-closed 一致 |
| **批 4 考勤 + 请假** | 考勤期间/台账/导入 + 请假单 | ✅ 灰度（考勤可先只做导入不动薪资） | 导入行数与源 Excel 对平 |
| **批 5 薪资** | V0387 薪资项 + 薪资单/过账/红冲/导出 | ⚠️ 只灰 1 家客户的 1 个月（并行跑：系统算 + 手工 Excel 算，逐人比差） | S2/S3 判据全过才放量 |

**上线顺序硬约束**：`批 1 → 批 2 → 批 3 → 批 4 → 批 5`（薪资依赖考勤锁定、凭证依赖科目、审批依赖流程定义）。MES（ARCH-4）若同期开发，**MES 的凭证出口必须在 HR 薪资上线前改为"不出凭证"**（§4.7.2），否则双计提。

### 7.5 DDL 已在真库跑通（不是纸面 SQL）

**复现命令**（两条，都在 `cloudpivot_test` 上整段事务 + `ROLLBACK`，零残留）：

```bash
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-2-hr-ddl/01_new_tables_check.sql    # → 45 条断言
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql  # → 24 条断言
```

**实跑结果**（`.out` 已入库）：

```text
01_new_tables.out:  NOTICE:  ARCH-9-2 新表断言通过 45 条          RC=0
02_alter_tables.out: NOTICE:  ARCH-9-2 改造表/种子断言通过 24 条  RC=0
                     NOTICE:  本库无 biz_doc_attachment：放宽段按幂等守卫跳过（生产库会执行）
```

**残留复核**（跑完后自查，全 0）：

```text
SELECT count(*) FROM information_schema.schemata WHERE schema_name='arch92_hr';           -- 0
SELECT count(*) FROM biz_account WHERE account_code IN ('2211.01','5602.10','2221.03');   -- 0
SELECT count(*) FROM sys_menu    WHERE path LIKE '/business/hr%';                          -- 0
SELECT count(*) FROM biz_seq     WHERE seq_type IN ('EMP','LV','ATT','SAL');               -- 0
```

**跑真库改掉了设计里的 3 个真错误**（这就是为什么设计稿的 DDL 必须真跑）：

1. **漏了 3 个科目**：第一版只种了 `2211.*/2221.03/2241.03/2241.04`，忘了 `5602.10~12 管理费用-工资/社保/公积金` —— 结果 `PAYROLL_ACCRUAL_DEFAULT` 规则的借方科目解析为空，断言直接红（"2 条工资凭证规则未全部解析到科目（实际 1）"）。
2. **父科目翻末级只写了 `2211`**：`cloudpivot_test` 里 `2241`、`5602` 也是末级（该库没跑过 V0226），挂子科目后父仍是末级 → 自相矛盾。改成"任何有未删子科目且当前是末级的父科目都翻"的条件 UPDATE。
3. **`sys_config` 两列类型写错**：`config_level` 是 `SMALLINT`（1 系统级 / 2 业务级），不是字符串 `'SYSTEM'`；`config_type` 用 `STRING/BOOLEAN/NUMBER` 大写枚举。第一版真库直接 `invalid input syntax for type smallint`。

**未覆盖清单（诚实说明）**：`cloudpivot_test` 缺 `biz_doc_attachment` 与 `biz_attachment_rule`，所以 V0383 的 CHECK 放宽与必传规则**只验到了"守卫分支"**（打印 NOTICE 跳过），真实放宽要在生产库/全量库上验；4 套流程的**图与 sha256** 未在本次校验（由实现卡 HR-26/HR-38 生成后按 `V0243` 的做法写进迁移头注释并用 `flow::graph::snapshot()` 复算）。

---

## 8. 工作量估算与风险

### 8.1 实现卡清单（50 张，每张 ≤1.5 h，标注依赖）

#### 批 1 底座（8 张）

| 卡 | 内容 | 依赖 | 估算 |
|:--:|---|---|---:|
| HR-01 | 迁移 **V0383**（设计号 V0378）：10 张表 + 索引 + `DO` 自检（DDL 照 §3.1 抄） | — | 1.5 h |
| HR-02 | 迁移 **V0384**（V0379）：9 科目 + 父科目翻末级 + 2 条 voucher_rule | HR-01 | 1.0 h |
| HR-03 | 迁移 **V0386**（设计号 V0380 / 曾映射 V0385）：4 号段 + 4 字典 + 26 项 | HR-01 | 1.0 h |
| HR-04 | 迁移 **V0387**（V0381）：36 菜单 + HR_MANAGER + 授权 | HR-01 | 1.5 h |
| HR-05 | 迁移 **V0388 + V0389**（V0382 + V0383）：脱敏/配置/附件 CHECK 放宽 | HR-01 | 1.0 h |
| HR-06 | `doc_kind.rs`：+5 `DocKind` +3 `FlowBizType` +2 `VoucherSourceType` + 交叉映射 + `COLUMN_DOMAINS` | — | 1.5 h |
| HR-07 | 前端镜像 `doc-kinds.ts` / `attachment.ts`，跑 `doc_kind_tests` 全绿 | HR-06 | 1.0 h |
| HR-08 | `state_machine.rs`：+5 `Entity` + 边表/BASE/REASON/LOCKED + 单测 | HR-06 | 1.5 h |

#### 批 2 员工档案（10 张）

| 卡 | 内容 | 依赖 | 估算 |
|:--:|---|---|---:|
| HR-09 | `business/hr/` 模块骨架（`mod.rs`/路由注册/空 handler）+ `cargo check` 绿 | HR-06 | 1.0 h |
| HR-10 | `employee_model.rs`：CRUD + 唯一键冲突翻译（工号/身份证/账号三处） | HR-09 | 1.5 h |
| HR-11 | `employee_dto.rs`：`*_id/*_name` 成对 + `*_masked` 出参 | HR-10 | 1.0 h |
| HR-12 | `employee_handler.rs` 6 接口 + `SCOPED_TABLES` 加 `BIZ_EMPLOYEE` + 单测 | HR-10 | 1.5 h |
| HR-13 | 敏感字段明文接口 + `audit_logs` 留痕 + `sys_field_mask` 接线验证 | HR-12 | 1.0 h |
| HR-14 | 工号取号（复用 `seq::next_master_code_free`，`EMP`） | HR-10 | 0.5 h |
| HR-15 | 合同台账 model/dto/handler（3 接口 + 续签链） | HR-10 | 1.5 h |
| HR-16 | 员工薪资标准 CRUD + 生效区间不重叠校验 | HR-15 | 1.0 h |
| HR-17 | `seed-from-users`（dry-run + 批量建 + 回写 user_id） | HR-10 | 1.0 h |
| HR-18 | 员工列表页 + 详情抽屉（含合同时间线/薪资标准/附件面板） | HR-11 | 1.5 h |

#### 批 3 考勤 + 请假（12 张）

| 卡 | 内容 | 依赖 | 估算 |
|:--:|---|---|---:|
| HR-19 | `attendance_period` model/handler（建/改/状态/汇总重算） | HR-09 | 1.5 h |
| HR-20 | `attendance_record` model + 台账分页 + 单行修正 | HR-19 | 1.5 h |
| HR-21 | impexp 定义 `hr.attendance`（`builtin/hr.rs` + `register`）+ 模板 | HR-20 | 1.5 h |
| HR-22 | 导入后处理：汇总重算 + 「无薪假 ≤ 已批请假天数」校验 + 错误行报告 | HR-21 | 1.0 h |
| HR-23 | 考勤锁定/解锁 + 状态机接线 + 锁定后写保护 | HR-19 | 1.0 h |
| HR-24 | `leave_request` model/dto | HR-09 | 1.0 h |
| HR-25 | leave handler：CRUD + 提交/撤回/作废（6 接口） | HR-24 | 1.5 h |
| HR-26 | 迁移 V0393（设计 V0384 / 曾映射 V0390）：`hr_leave` 流程（定义+版本+图+hash+自检）。✅ 已落地 | HR-25 | 1.5 h |
| HR-27 | `leave_bridge`：审批回调 → 状态 + 实例终止 + 自审批守卫 | HR-26 | 1.0 h |
| HR-28 | 请假列表页 + 表单（含「我的请假」tab） | HR-25 | 1.5 h |
| HR-29 | 考勤页（期间 + 台账 + 导入入口 + 单元格编辑） | HR-20,HR-21 | 1.5 h |
| HR-30 | 考勤导出 xlsx（含锁定状态列） | HR-20 | 1.0 h |

#### 批 4 薪资（16 张）

| 卡 | 内容 | 依赖 | 估算 |
|:--:|---|---|---:|
| HR-31 | `salary_item` model/dto/handler 4 接口 | HR-09 | 1.5 h |
| HR-32 | **公式解析器** `hr/salary_formula.rs`（tokenizer + 递归下降 + Decimal 求值 + 惰性 IF）+ 20 条单测 | HR-31 | 1.5 h+ |
| HR-33 | 变量解析（项引用 + `ATT_*` 白名单）+ 依赖拓扑 + 环检测 + 未定义变量报错 | HR-32 | 1.5 h |
| HR-34 | `salary_compute.rs`：试算（读标准+考勤+人工项）→ 逐人逐项 → 恒等式校验 → 落库事务 | HR-33,HR-20 | 1.5 h+ |
| HR-35 | 迁移 V0396（设计 V0387 / 曾映射 V0393）：17 个薪资项种子（含公式与科目） | HR-31 | 1.0 h |
| HR-36 | `salary_sheet` model/handler：建单/试算/明细分页/单头汇总 | HR-34 | 1.5 h |
| HR-37 | 明细覆盖（改单项 → 重算汇总 → 写状态日志）+ 越权校验 | HR-36 | 1.0 h |
| HR-38 | 迁移 V0394（设计 V0385 / 曾映射 V0391）：`hr_salary` 流程 + 迁移 V0395（设计 V0386 / 曾映射 V0392）：调薪/离职流程。✅ 已落地：6/6 + 5/4 + 6/6 三套 PUBLISHED、节点绑角色（会签=财务经理+财务总监）、145 演练库 69 条断言 + 5 条真库用例全绿 | HR-36 | 1.5 h |
| HR-39 | `salary_bridge`：过账（两张凭证，`post_explicit_lines`）+ 审批回调 + 净额法汇总。✅ 已落地：净额法汇总 15 条单测（逐行对设计 §4.6.1 两张表）+ 7 条真库用例（借贷平衡 / 净额法 / 倒挤 / 状态闸 / 考勤闸 / 回调三路径）全绿；#42 提交审批（冻 `gross_total_num` 为 JSON number）与 #43 过账两条路由同卡落地 | HR-38,HR-02 | 1.5 h+ |
| HR-40 | 红冲（两张红字凭证 + 状态机 + 原因必填）。✅ 已落地：两张红字（`PZR-` 号段、借贷互换、`reverse_of` + `reversed_by` 双向指针）+ `POSTED → REVERSED` 状态机 + 原因两道闸（DTO + 必填原因边）+ 迁移 V0397「红冲单不占期间位 ⇒ 红冲后可重算」；145 演练库 4 条真库用例（两张红字 / 状态闸 / 原因必填 / 红冲后重算闭环）全绿 | HR-39 | 1.0 h |
| HR-41 | `/salary/my` + `/attendance/my` + `/leave/my` 自助三接口 | HR-36 | 1.0 h |
| HR-42 | 工资表导出 xlsx（项为列 + 部门分组） | HR-36 | 1.5 h |
| HR-43 | 薪资单详情页（四段式 + 明细展开 + 凭证卡） | HR-36 | 1.5 h |
| HR-44 | 薪资项配置页（公式编辑器 + 变量面板 + 检查按钮） | HR-31 | 1.5 h |
| HR-45 | 迁移 **V0398**（设计 V0388 / 曾映射 V0394→V0397）：收口自检 + `scripts/verify/hr_invariants.sql`（10 断言）。✅ 已落地：V0398 纯 `DO` 块断言 8 组种子（10 表 / 9 科目+4 父级+2 规则 / 4 号段+4 字典 26 项 / 36 菜单+`HR_MANAGER` / 2 脱敏+3 配置 / 4 流程 / 17 薪资项），空库重放 319 条迁移全过；巡检脚本给 A1~A10（8 BLOCKER + A4/A5 WARN）+ W1/W2，配 `hr_invariants_selftest.sh` 造错自检（R0 零误报 + I1~I10 十条逐条报红 + F1 fail-closed）；两处具名偏差（A9 结账期间、A10 免审直过）写在脚本头注释 | HR-39 | 1.5 h |
| HR-46 | E2E 脚本：建员工→导入考勤→请假→试算→审批→过账→红冲→对账（10 步）。✅ 已落地：`scripts/verify/hr/hr_salary_e2e.sh`（+ `hr_e2e_xlsx.py`，纯标准库造真 xlsx）一条命令完成「145 建演练库 → 回放 300+ 迁移 → 起隔离实例 → 10 步真 HTTP → 自己 DROP」；**98 项断言全绿**（工号取号 / 期间汇总重算 / 公式项 `OT_WD=1034.48` / 试算零落库 / 审批通过不出凭证 / 两张蓝字 + 两张红字借贷互换 / 二次红冲 409 / **4 张凭证逐科目净额 0** / `hr_invariants.sh` 12 行 blocker_fail=0）；失败打印请求 + 回执 + 日志尾 + 夹具 id（改坏期望值实测 3 条报红 + `rc=1`）；配套静态契约测试 8 条（步骤数 / 每步断言 / 演练库护栏 / 只删自己的库 / 失败定位 / 零第三方依赖 / 设计硬口径 / 金额自洽） | HR-45 | 1.5 h+ |

#### 批 5 收口（4 张）

| 卡 | 内容 | 依赖 | 估算 |
|:--:|---|---|---|
| HR-47 | 权限矩阵回归（30 码 × 6 角色，含 SELF 只读自己薪资） | 全部 | 1.5 h |
| HR-48 | 全量回归（既有 verify 脚本 + 新增 HR 脚本，`scripts/check.sh`） | 全部 | 1.5 h |
| HR-49 | 上线演练：空库首启（迁移全跑）+ 克隆库升级（含 2211 翻末级） | 全部 | 1.5 h |
| HR-50 | 文档回写（`docs/README.md` 索引、`AGENTS.md` 菜单/权限口径、二开指南补 HR 模板） | 全部 | 1.0 h |

**合计**：50 张卡 × ≈1.2 h ≈ **60 人时 ≈ 7.5 人日**；加回归/演练/文档（批 5 的 4 张已计入）+ 联调返工 ≈ **11 人日**（1 人 ≈ 2.5 周；2 人并行 ≈ 6 工作日）。

### 8.2 与卡面「每张 ≤1.5 小时」的关系（诚实说明）

标 `+` 的 4 张（HR-32 公式解析器、HR-34 计算引擎、HR-39 过账、HR-46 E2E）**实际会溢出到 3~4 小时**：

- HR-32：递归下降 + 20 条单测（含精度/除零/惰性 IF/未定义变量/环检测），1.5 h 只够写完 parser，单测要另算。
- HR-34：读标准 + 读考勤 + 依赖排序 + 恒等式 + 事务落库 + 明细项解释文本，是"一个人一天的活"。
- HR-39：净额法汇总 + 零金额行过滤 + 部门辅助核算 + 期间闸 + 审批回调，任何一处错都会出"不平的凭证"。
- HR-46：10 步 E2E 要造 42 人规模数据，跑一遍 3 分钟起。

建议这 4 张**单独拆成 2 张**（写实现 / 写测试与断言），总量不变，卡面粒度守规矩。

### 8.3 已知风险（哪些地方可能推翻设计）

| # | 风险 | 影响 | 缓解 |
|:--:|---|---|---|
| **R1** | `sys_field_mask` 是否对 `biz_employee` 自动生效**未实测**（脱敏模块按 `table_name+column_name` 注入，需确认它对"非 SCOPED_TABLES 的新表"也走同一条路径） | 身份证/银行卡可能明文返回 | HR-13 卡的第一步就是**实测**：造一条员工 → 调列表接口 → 看 `id_card` 是否已遮；若未生效，退化为「DTO 层手工调 `masking` 函数」 |
| **R2** | 公式 DSL 的精度口径与客户 Excel 不一致（`ROUND(367.82*2)` vs `ROUND(8000/21.75*2)`） | 差 1 分钱，HR 不信系统 | 中间精度 10 位写进单测；提供"按人导出计算过程"（`calc_note`）让 HR 自己核 |
| **R3** | 账号 ↔ 员工双写漂移（`employee.user_id` 与绑定的账号被停用/删除） | 自助接口查不到人 | 巡检 A1/A2（BLOCKER）；账号停用不解除绑定（离职后才解绑） |
| **R4** | 部门双写漂移（`employee.dept_id` vs `sys_user.dept_id`） | 数据权限范围与人事归属不一致 | 调岗 API 强制同事务双写；巡检 WARN 列表（借调合法，不能 BLOCKER） |
| **R5** | 期中入离职/调薪按整月算（不做按天折算） | 首月工资偏差 | 明细覆盖接口兜底（HR-37）+ Q5 待确认 |
| **R6** | 与 ARCH-4 MES 的凭证归属冲突（双计提） | 账上工资翻倍 | 本卡 §4.7.2 已拍板"薪资单唯一出口"；MES 卡需同步改设计（上线顺序硬约束） |
| **R7** | ARCH-9-1（V0368~V0377）未落地就上 HR | `org_id` 列无来源 | 本卡 §7.5 证据脚本已做"有 org_id 列/无 org_id 列"两分支验证；实际发布顺序：ARCH-9-1 → HR |
| **R8** | 一公司一期间一张薪资单，补发/二次发放无处落 | 客户月中发两次 | 用"补差单"（期间用当月 + `remark`）或红冲重开；二期评估"补发单"独立单据 |
| **R9** | 无账号员工（`user_id IS NULL`）在 SELF 范围不可见 | 车间工人自助查不到 | 已知边界：他们本来就不登录；HR 在正式列表（部门范围）能看到 |
| **R10** | 4 个 `doc_type` 新值要同步 **4 处**（`ck_biz_doc_attachment_type`、`ck_biz_attachment_rule_doc_type`、`attachment::model::DocKind`、前端 `attachment.ts`），漏一处即静默失败 | 附件上传报 500 或标签空白 | HR-05/HR-07 两卡分别负责库侧与前端侧；`rg 'EXPENSE_REPORT' frontend/src` 的同款清单写进卡里逐项勾 |

### 8.4 待确认（必须产品/老板拍板，已给确认方式）

| # | 待确认 | 默认取向（不确认就按这个做） | 确认方式 |
|:--:|---|---|---|
| **Q1** | 一期薪资项是否要含「计件工资/提成」（依赖 ARCH-4 MES 与销售佣金） | **不含**，只留 `source='MES'` 的钩子与 `PIECE` 项占位（默认停用） | 问老大：首批客户是否有计件场景；有则把 MES 排到 HR 之前 |
| **Q2** | 社保公积金是否一期就要按城市规则自动算 | **手工录**（D-9） | 问财务口径：客户是否接受"每月从社保网厅导出再导入" |
| **Q3** | 身份证/银行卡是否需要**加密存储**（等保/个保法要求） | **明文 + 脱敏**（D-14） | 问合规：若客户要通过等保三级，需改列加密 + 密文检索方案（工作量 +3 人日） |
| **Q4** | 工资允许跨月发放吗（8 月工资 9 月 5 日发） | **允许**：计提按薪资期间、发放按过账日期间 | 问财务：若要求"计提与发放必须同期间"，把发放凭证期间锁到薪资期间（改 1 行） |
| **Q5** | 期中入职/离职/调薪是否按天折算 | **整月 + 人工覆盖** | 问 HR：客户是否有"按 21.75 折算"的硬要求；有则加 `prorate` 卡（+1.5 人日） |
| **Q6** | 员工是否需要独立自助门户（而不是页面内 tab） | **tab**（§6.1） | 问产品：若客户规模 > 500 人，独立门户更合理（+3 人日） |

---

## 附：本文证据索引（全部可复核）

| 结论 | 证据位置 | 复核方式 |
|---|---|---|
| 10 张表 DDL 可执行、45 条语义断言通过 | `docs/evidence/2026-11-10-arch9-2-hr-ddl/01_new_tables_check.sql` + `.out` | `psql -f 01_…sql`（RC=0，`NOTICE: 通过 45 条`） |
| 科目/号段/字典/菜单/角色/脱敏/配置/规则/流程 种子可执行、24 条断言通过 | 同目录 `02_alter_tables_check.sql` + `.out` | `psql -f 02_…sql`（RC=0，`NOTICE: 通过 24 条`） |
| `sys_user` 无任何人事实段 | `backend/migrations/system/V0001__create_sys_user.up.sql:2-24` | `read_file` |
| `sys_user.user_type` 只有 HUMAN/ROBOT | `backend/migrations/system/V0102__add_sys_user_type.up.sql` | 同上 |
| 数据权限 SELF 只按列比较 | `backend/src/business/shared/scope.rs`（`filter_for` ③ 与 `ScopeTarget` 常量表） | `search_files "filter_for"` |
| `SCOPED_TABLES` 是开放清单（12 项） | `backend/src/business/shared/scope.rs`（`SCOPED_TABLES` 常量） | 同上 |
| `DocKind` 29 值 / 7 根轴 / 前端镜像单测 | `backend/src/business/shared/doc_kind.rs:59-183,1309-1339,1525+`；`frontend/src/api/business/erp/doc-kinds.ts`；`doc_kind_tests.rs`（`frontend_ts_mirror_matches_rust`） | `cargo test doc_kind` |
| 状态机唯一入口 + 边表写法 | `backend/src/business/shared/state_machine.rs:1552+`（`Entity`）、`1720+`（EDGES） | `read_file` |
| 凭证写入口（唯一）与分录单边/非负约束 | `backend/src/business/erp/voucher.rs:1920-1963`（`PostVoucher`/`post_voucher`）；`V0149:100-110`（`ck_biz_voucher_entry_side`） | `read_file` / `psql \d biz_voucher_entry` |
| 多借方凭证入口（工资凭证复用） | `backend/src/business/erp/voucher_multi.rs:239`（`post_explicit_lines`）、`:305`（`ensure_leaf`） | `read_file` |
| `2211` 当前是末级、且无历史分录 | `psql -d cloudpivot_test -c "SELECT account_code,is_leaf FROM biz_account WHERE account_code LIKE '2211%'"`（1 行，`is_leaf=1`）；`SELECT count(*) FROM biz_voucher_entry WHERE account_code LIKE '2211%'`（0） | 两条 SQL |
| 报表按编码前缀汇总（加子科目免改报表） | `backend/src/business/erp/report.rs:473-488`（`match_account`）；`V0183:216`（`PAYROLL_PAYABLE → 2211`） | `read_file` |
| `2241.01/02` 已被报销/佣金占用 | `V0226:36`、`V0311:36` | `grep 2241.` |
| 附件 `doc_type` 现为 16 值 + 必传规则表 | `V0347:79`、`V0350`（12→16 放宽先例） | `read_file` |
| `biz_status_log.entity_type` 无 CHECK 白名单 | `V0159`、`V0219:44` | `read_file` |
| 号段四入口与主数据码语义 | `backend/src/business/erp/seq.rs`（模块文档「与单据号的区别」） | `read_file` |
| 流程种子写法（含 `is_system=TRUE` 的 H6 教训） | `V0243`（`expense` 流程） | `read_file` |
| 审批 fail-closed 口径 | `backend/src/business/erp/approval_gate.rs`（模块文档） | `read_file` |
| 附件/流程/操作栏组件路径 | `frontend/src/views/business/erp/shared/{DocActionBar,DocAttachmentPanel,CancelImpactDialog}.vue`、`approval-progress.ts`、`use-attachment-gate.ts` | `ls` |
| impexp 定义注册方式 | `backend/src/business/impexp/builtin/mod.rs`（`register`） | `read_file` |
| 顶级菜单 19 个、`sort_order=77` 空闲、`title ≤12` 断言 | `psql` 实测（19 行 sort：…75/76/95）；证据脚本 §④ 断言 | 两条 SQL + 断言 |
| MES 结算单自带凭证（冲突来源） | `docs/architecture/ARCH-4-通用MES与委外架构设计.md:1890,1938` | `grep` |
| 迁移号段：business 最大 V0367、ARCH-9-1 预留 V0368~V0377 | `ls backend/migrations/business | sort | tail -1`；`ARCH-9-多组织与合并报表设计.md` §7.1 | `ls` / `grep` |
| 演算数字（689.66 / 735.63 / 9954.03 / 8154.03 / 12594.03） | `psql` 单查询实算（见 §7.5 同批命令） | 任选一条 `SELECT 8000/21.75/8*10*1.5` |
