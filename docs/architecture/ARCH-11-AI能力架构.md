# ARCH-11 AI 能力架构（AI 用户 + AI 员工）

> 状态：**设计稿，等用户确认。没有明确指令不派卡。**
> 日期：2026-11-10
> 来源：从 ARCH-10 提出 H 章 + TBD-1 合并重写
> 用户原话：「把 ARCH-10 的 H 提出来，跟 TBD-1 合并重写成一个『AI 能力架构』文档（含 AI 用户实体 + AI 员工）。AI 开发风险还是很大，先不做。没有我的明确指令不派卡。」

---

## 0. 用户已确定的决策（2026-11-10）

| # | 决策 | 用户原话 |
|---|---|---|
| **D1** | **144 不补齐 99 的 27 个 profile**。改成在云枢里建「AI 用户」，按需创建 | 「144的多agent不用补齐跟99的profile。通过在云枢里建AI用户另外根据需要建。」 |
| **D2** | **144 要跑 AI 员工** | 「144是要跑AI员工的」 |
| **D3** | **AI 开发先不做**（风险大） | 「AI开发风险还是很大，先不做。」 |
| **D4** | **没有明确指令不派卡** | 「没有我的明确指令不派卡。」 |

**D1 的深层含义**：AI 用户是**业务实体**（云枢里的一条数据），不是运维概念（Hermes profile 是运维细节）。这个区分很重要 —— 客户看到的是「财务机器人」这个用户，不是「profile cloudpivot-fin-bot」。

---

## 1. 核心概念：AI 用户 = `sys_user(user_type='ROBOT')`

### 1.1 关键发现：不需要新建 `sys_agent` 表

**实测**：现有 `sys_user` 已有 `user_type` 字段，值是 `HUMAN` / `ROBOT`（`V0102__add_sys_user_type`）。

```
→ 「AI 用户」不是新概念，是**已经建模好的**
→ 复用 sys_user 的好处：
   · 权限走 sys_user_role + sys_role_menu（现成）
   · 登录走 /api/system/auth/login（现成）
   · 审计走 operation_logs（现成，actor_id 就是 user_id）
   · 菜单可见性、数据权限全部现成
   · **不用改一行鉴权代码**
```

**所以 ARCH-10 H 章的 `sys_agent` 表设计作废**（那是冗余的）。

### 1.2 只需要补一张配置表

```sql
-- AI 用户专属配置（sys_user 存公共属性，这张存 AI 特有属性）
CREATE TABLE biz_agent_config (
    user_id         uuid PRIMARY KEY REFERENCES sys_user(id) ON DELETE CASCADE,
    agent_type      varchar(32) NOT NULL,          -- QA / FINANCE / PURCHASE / SERVICE
    hermes_profile  varchar(64) NOT NULL,          -- Hermes 的 profile 名（运维映射）
    run_mode        varchar(16) DEFAULT 'CRON',    -- CRON / EVENT / MANUAL
    cron_expr       varchar(64),                   -- 定时表达式
    event_trigger   jsonb,                         -- 事件触发条件（如 {"type":"AR_OVERDUE","days":30}）
    config          jsonb,                         -- 机器人专属配置（阈值/白名单等）
    max_actions_per_run  int DEFAULT 100,          -- 单次运行最大动作数（防失控）
    max_runtime_seconds  int DEFAULT 300,          -- 单次运行超时
    created_at      timestamptz DEFAULT now(),
    updated_at      timestamptz DEFAULT now()
);
```

**为什么单独一张表**：
```
· sys_user 是「所有用户」的公共表，不该塞 AI 专属字段
· biz_ 前缀表示业务范畴（跟 sys_ 的系统范畴分开）
· 删 AI 用户时 CASCADE 掉配置，不留孤儿
```

### 1.3 与 Hermes profile 的映射

```
一个 AI 用户 ↔ 一个 Hermes profile

创建 AI 用户时的流程：
① 云枢建 sys_user(user_type='ROBOT', username='bot_finance_01')
② 云枢建 biz_agent_config(hermes_profile='cloudpivot-fin-bot', ...)
③ 云枢调 Hermes API 建 profile（或者提示运维手工建）
④ 配 profile 的 tools 白名堂（只给 cloudpivot.* 工具，禁 terminal）
```

**D1 的落地方式**：**不预置 27 个 profile**，而是：
```
· 内置 4 个「模板」（qa/finance/purchase/service）的 profile 定义
· 客户建 AI 用户时，按模板生成 profile（10 秒）
· 客户不建 = 磁盘上 0 个 AI profile（干净）
```

---

## 2. 数据模型（4 张表）

| 表 | 作用 | 新建/复用 |
|---|---|---|
| `sys_user` | AI 用户本体（`user_type='ROBOT'`） | **复用**（已存在） |
| `biz_agent_config` | AI 用户专属配置 | 新建 |
| `sys_agent_action` | 动作白名单 + 风险分级 | 新建 |
| `sys_agent_run_log` | 运行审计 | 新建 |
| `sys_agent_confirm` | 待人工确认队列 | 新建 |

### 2.1 `sys_agent_action`（动作白名单，关键）

```sql
CREATE TABLE sys_agent_action (
    id              uuid PRIMARY KEY,
    agent_type      varchar(32) NOT NULL,          -- 按类型授权（不是按单个用户）
    action_code     varchar(64) NOT NULL,          -- FIN_CHECK_VOUCHER
    action_name     varchar(128) NOT NULL,
    http_method     varchar(8) NOT NULL,
    http_path       varchar(255) NOT NULL,         -- /api/business/erp/voucher/{id}/check
    params_schema   jsonb,                          -- 参数校验
    risk_level      varchar(8) NOT NULL,           -- LOW / MEDIUM / HIGH
    need_confirm    boolean DEFAULT false,
    enabled         boolean DEFAULT true,
    created_at      timestamptz DEFAULT now(),
    UNIQUE (agent_type, action_code)
);
```

**核心设计**：
```
· 机器人**不能直接调 API**，只能调动作库里登记过的
· 按 agent_type 授权（不是按单个用户）→ 一次配好，所有同类型机器人共享
· 每个动作有 risk_level → HIGH 的自动 need_confirm=true
```

### 2.2 `sys_agent_run_log`（审计，比人更严）

```sql
CREATE TABLE sys_agent_run_log (
    id              uuid PRIMARY KEY,
    run_id          uuid NOT NULL,                 -- 一次运行（含多个动作）
    agent_user_id   uuid REFERENCES sys_user(id),
    trigger_type    varchar(16) NOT NULL,          -- CRON / EVENT / MANUAL
    trigger_source  varchar(128),                  -- cron expr / event id / 谁点的手工
    started_at      timestamptz NOT NULL,
    ended_at        timestamptz,
    status          varchar(16),                   -- RUNNING / SUCCESS / PARTIAL / FAILED / KILLED
    actions         jsonb,                          -- [{action_code, target_id, result, reason}]
    output_summary  text,                           -- 「处理 12 张凭证，8 通过，4 转人工」
    error           text,
    trace_id        varchar(64),
    created_at      timestamptz DEFAULT now()
);
```

**关键字段**：
```
· reason：为什么这么做（引用了哪条规则/哪个数据）
· trace_id：能串起 LLM 调用 + SQL + 下游 API
· output_summary：人话总结（给客户看的）
```

### 2.3 `sys_agent_confirm`（人环确认队列）

```sql
CREATE TABLE sys_agent_confirm (
    id              uuid PRIMARY KEY,
    run_id          uuid NOT NULL,
    action_code     varchar(64) NOT NULL,
    target_type     varchar(32),                   -- 目标单据类型
    target_id       varchar(64),                   -- 目标单据 id
    payload         jsonb,                          -- 机器人想做什么（完整参数）
    reason          text,                           -- 为什么想做
    risk_level      varchar(8),
    status          varchar(16) DEFAULT 'PENDING', -- PENDING / APPROVED / REJECTED / EXPIRED
    confirmed_by    uuid REFERENCES sys_user(id),
    confirmed_at    timestamptz,
    expires_at      timestamptz DEFAULT now() + interval '24 hours',
    created_at      timestamptz DEFAULT now()
);
```

**流程**：
```
机器人想 POST 过账（HIGH）→ 不直接做 → 写一条 PENDING
→ 有权限的人看到「待确认」（首页待办 + 消息）
→ 人点批准 → 才真正执行
→ 超 24 小时未处理 → EXPIRED（不执行）
```

---

## 3. 权限模型（复用 + 一个扩展）

```
复用：sys_user_role + sys_role_menu（角色 → 菜单/权限码）
扩展：sys_agent_action 的 need_confirm 字段（不是权限，是执行方式）
```

**示例（财务 AI 用户）**：

| 权限码 | 有权限 | 执行方式 |
|---|---|---|
| `business:erp:voucher:list` | ✅ | 直接执行 |
| `business:erp:voucher:read` | ✅ | 直接执行 |
| `business:erp:voucher:create` | ✅ | 直接执行 |
| `business:erp:voucher:post` | ✅ | **写 sys_agent_confirm，等人批** |
| `business:erp:payment:create` | ✅ | **写 sys_agent_confirm，等人批** |
| `business:erp:voucher:delete` | ❌ | 无权限（角色里不给） |

**关键**：**「有权限」和「直接执行」是两件事**。
```
有权限 = 角色里有这个码
直接执行 = 动作库里 need_confirm=false

所以：机器人有权限做，但 HIGH 风险的仍然要人批。
```

---

## 4. 调度（复用 `sys_job`）

```
三种触发：
① CRON     每天 9:00 跑财务巡检        → 复用 sys_job（已有 cron 框架）
② EVENT    应收超期 30 天 → 催收       → 新事件总线（或轮询）
③ MANUAL   用户点「让机器人跑一次」      → API 触发
```

**EVENT 的实现**（一期简化）：
```
不做真事件总线，用「轮询 + 时间窗口」：
· 每 5 分钟扫一次 biz_agent_config 里 run_mode='EVENT' 的
· 检查 event_trigger 条件（如 AR_OVERDUE days=30）
· 满足 → 触发运行
· 用 sys_agent_run_log 去重（同一个目标 24h 内不重复触发）
```

---

## 5. 144 的部署形态（D1 + D2 落地）

```
┌──────────────────────────────────────────────────────┐
│  144 生产服务器                                        │
│                                                       │
│  ┌────────────────────────────────────────────────┐  │
│  │ 云枢主服务 :8023                                │  │
│  │  · 系统管理 → AI 用户（CRUD）                    │  │
│  │  · 待确认队列                                   │  │
│  │  · AI 运行日志                                  │  │
│  └────────────────────────────────────────────────┘  │
│              ↕ HTTP API                               │
│  ┌────────────────────────────────────────────────┐  │
│  │ Hermes Agent（:8100，一套）                     │  │
│  │                                                 │  │
│  │  按需创建的 profile（不是 27 个）：              │  │
│  │   · cloudpivot-qa-bot        ← 质检             │  │
│  │   · cloudpivot-fin-bot       ← 财务             │  │
│  │   · cloudpivot-pur-bot       ← 采购             │  │
│  │   · cloudpivot-svc-bot       ← 客服             │  │
│  │                                                 │  │
│  │  每个 profile：                                 │  │
│  │   · tools = [cloudpivot.*]（**无 terminal**）    │  │
│  │   · 独立的 MEMORY.md                            │  │
│  │   · 独立 cron                                   │  │
│  └────────────────────────────────────────────────┘  │
│                                                       │
│  ┌────────────────────────────────────────────────┐  │
│  │ MCP Server :8101（云枢暴露给 Hermes 的工具）     │  │
│  │  · cloudpivot.voucher.query / check / create   │  │
│  │  · cloudpivot.voucher.post（HIGH，要人确认）     │  │
│  │  · cloudpivot.invariant.run                    │  │
│  │  · ...                                         │  │
│  └────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────┘
```

**D1 的关键点**：
```
❌ 不复制 99 的 27 个 profile
✅ 只建「AI 员工需要的」profile（4 个模板 → 按需实例化）
✅ 每个 profile 的 tools 白名单里**没有 terminal**
```

---

## 6. 四个首发 AI 员工（按 ROI 排序）

| AI 用户 | 做什么 | 价值 | 风险 | 权限 |
|---|---|---|---|---|
| **质检机器人** `bot_qa` | 每天跑 152 组勾稽 + 80 项不变量，有红就报告 | 无人值守兜底 | 低（**只读**） | 只给 `*:list` + `invariant:run` |
| **财务机器人** `bot_finance` | 凭证预审（勾稽+科目异常）、自动对账、应收催收 | 会计省 2h/天 | 中（过账要人确认） | 给 `voucher:list/read/create`，`post` 要确认 |
| **采购机器人** `bot_purchase` | 安全库存预警 → 生成采购申请、询价催办 | 减少断货 | 中（下单要人确认） | 给 `purchase-request:create`，`order:create` 要确认 |
| **客服机器人** `bot_service` | 工单自动分类 + 派单 + 首次响应 | 响应从小时到分钟 | 低（外部沟通要人审） | 给 `ticket:*`，对外发送要确认 |

**推广顺序**：**质检（只读）→ 财务 → 采购 → 客服**（风险从低到高，逐步建立信任）。

---

## 7. 安全边界（六条硬约束）

```
① 动作白名单
   机器人只能调 sys_agent_action 里登记的动作
   → 不是「有这个权限就能做」，是「调这个动作才允许」

② HIGH 风险必须人确认
   risk_level='HIGH' → 自动写 sys_agent_confirm，等批
   → 过账/付款/删除/对外发送 都是 HIGH

③ 无 terminal
   Hermes profile 的 tools 白名单里**不含 terminal**
   → 防 prompt injection 后跑 rm -rf

④ 独立凭据
   AI 用户有自己的 token（不是 admin 的）
   · 有效期 24h（auto renew via agent credential）
   · 可吊销（sys_user.status=0）
   · IP 限制（只能从 127.0.0.1 调）

⑤ Kill switch
   单个：sys_user.status = 0 → 立刻停
   全局：一个开关停所有机器人（sys_config 里一个布尔）

⑥ 速率 + 超时
   max_actions_per_run（默认 100）
   max_runtime_seconds（默认 300）
   → 防循环/重试风暴
```

---

## 8. 验收（10 项）

```
【基础】
① sys_user 能建 user_type='ROBOT' 的用户
② biz_agent_config + sys_agent_action + sys_agent_run_log + sys_agent_confirm 四张表建好
③ 系统管理 → AI 用户 页面能 CRUD

【权限】
④ 财务 AI 用户只能调白名单动作（不在白名单 → 403）
⑤ HIGH 风险动作写 sys_agent_confirm（不直接执行）
⑥ 人批准后才真正执行（实测）

【运行】
⑦ 质检机器人按 cron 跑，写 run_log（含 reason + trace_id）
⑧ 人为造一个勾稽红 → 机器人发现 → 报告

【安全】
⑨ Kill switch 生效（停单个 + 停全局）
⑩ 审计能查「机器人 X 在 10:00 做了什么、为什么、谁批的」
```

---

## 9. 工作量（不派卡，仅估算）

| 卡 | 内容 | 工时 |
|---|---|---|
| AIUSER-01 | 四张表 + 迁移 + DDL | 3h |
| AIUSER-02 | AI 用户 CRUD（复用 sys_user + 扩展配置） | 3h |
| AIUSER-03 | 动作库 CRUD + 风险分级 UI | 4h |
| AIUSER-04 | MCP Server（云枢暴露 ~12 个工具） | 5h |
| AIUSER-05 | 调度（CRON 复用 sys_job + EVENT 轮询） | 4h |
| AIUSER-06 | 审计（run_log + reason + trace_id） | 3h |
| AIUSER-07 | 人环确认（confirm 队列 + 首页待办 + 批准） | 4h |
| AIUSER-08 | Kill switch（单个 + 全局） | 2h |
| AIUSER-09 | Hermes profile 模板（4 个）+ 按需实例化 | 3h |
| AIUSER-10 | 质检机器人（首发，只读，验证框架） | 4h |
| AIUSER-11 | 财务机器人（预审凭证） | 5h |

**合计约 40 小时**

---

## 10. AI 开发（G 章）：暂不做（用户 D3）

**用户原话**：「AI开发风险还是很大，先不做。」

**风险点（我的分析，供以后决策参考）**：

```
① 代码安全
   AI 改的代码进入主干 → 一个 prompt injection 就可能植入后门
   
② 数据安全
   AI 读业务数据时可能外泄（LLM 调用要出网）

③ 责任归属
   AI 改错了导致客户损失 → 谁负责？

④ 审批依赖
   需要客户有技术负责人 + 业务负责人 + 决策人三级审批
   → 中小企业根本没有这个组织

⑤ 工程复杂度
   三层隔离（sandbox/staging/prod）+ 部署审批流
   → 运维成本高
```

**用户在 2026-11-10 讨论过的两个方向**（记录备查，**不实施**）：

| 方向 | 内容 | 特点 |
|---|---|---|
| **a. 「我的应用」目录** | 用户调 AI 开发的小工具，放在云枢「我的应用」下，与官方完全隔离（前后端都不动官方） | 隔离彻底、零升级风险，但技术难度高（要插件热加载/微前端/schema 隔离） |
| **b. 沙箱 + 流程审批** | 沙箱改官方代码 → 测试环境验证 → 部署审批流 → 合并上生产 | 就是 G 章原设计，技术成熟（现有流程产品化），但改主干有冲突风险 |

**我的建议（供以后参考）**：如果将来要做，先做 a 的简化版（A2：前端页面 + 自定义只读查询），覆盖 80% 长尾需求，不动官方代码。

**状态**：⏸ **暂不做，等用户明确指令**

---

## 11. 待定事项

| # | 事项 | 状态 |
|---|---|---|
| **TBD-A** | 四个机器人先做哪几个？还是全做？ | 等用户 |
| **TBD-B** | EVENT 触发要不要做真事件总线（还是轮询够） | 建议先轮询 |
| **TBD-C** | AI 用户的 KPI 怎么统计（做了多少事、省了多少时间） | 等用户 |
| **TBD-D** | 客户不信任怎么办（先从只读开始？） | 建议从质检开始 |
| **TBD-E** | AI 员工的「工资」怎么算（token 成本谁出） | 等用户 |

---

## 12. 与 ARCH-10 的关系

```
ARCH-10 的 H 章（AI 机器人用户）：**已迁移到本文档**
ARCH-10 的 G 章（AI 快速二次开发）：**标注为「暂停，风险大，等指令」**
ARCH-10 的 TBD-1（144 多 agent 用途）：**已并入本文档 D1/D2**

ARCH-10 保留：
  A 凭证溯源 / B 一键交付 / C 智能填单 / D 大表分区 / E 错误码 / F H5 移动端
```

---

## 13. 状态

```
✅ 本文件 = 设计稿（含 AI 用户实体 + AI 员工）
⏸ 未派卡（用户 D4：没有明确指令不派卡）
⏳ 等用户确认第 11 节的 5 个 TBD + 是否开始实施
```
