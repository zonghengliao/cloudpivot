# ARCH-11 AI 能力架构（AI 用户 + AI 员工）

> 状态：**设计稿，等用户确认。没有明确指令不派卡。**
> 日期：2026-11-10（v2，基于现有代码盘点后重写）
> 来源：从 ARCH-10 提出 H 章 + TBD-1 合并
> 用户原话：「把 ARCH-10 的 H 提出来，跟 TBD-1 合并重写成一个『AI 能力架构』文档（含 AI 用户实体 + AI 员工）。AI 开发风险还是很大，先不做。没有我的明确指令不派卡。」

---

## 0. 用户已确定的决策（2026-11-10）

| # | 决策 | 用户原话 |
|---|---|---|
| **D1** | **144 不补齐 99 的 27 个 profile**。改成在云枢里建「AI 用户」，按需创建 | 「144的多agent不用补齐跟99的profile。通过在云枢里建AI用户另外根据需要建。」 |
| **D2** | **144 要跑 AI 员工** | 「144是要跑AI员工的」 |
| **D3** | **AI 开发先不做**（风险大） | 「AI开发风险还是很大，先不做。」 |
| **D4** | **没有明确指令不派卡** | 「没有我的明确指令不派卡。」 |

---

## 1. 现状盘点（重要：已有 60%，不是从零）

**盘点时间**：2026-11-10 10:42，实测代码与库。

### 1.1 已经做完的（✅ 不要重做）

| 能力 | 实现位置 | 状态 |
|---|---|---|
| **`sys_ai_agent` 表** | 迁移（含 `hermes_profile` / `gateway_status` / `agent_description` / 软删） | ✅ 已建 |
| **profile 名白名单** | `hermes.rs` `PROFILE_NAME_PATTERN` + 表级 CHECK 约束 `^[a-z][a-z0-9-]{1,30}$` | ✅ 双层防护 |
| **AI 用户 CRUD（6 个 API）** | `aiagent/handler.rs` | ✅ 完整 |
| | `POST /system/user/ai` | 建（建 profile + 写 SOUL） |
| | `GET /system/user/ai/{id}` | 详情 |
| | `PUT /system/user/ai/{id}` | 更新 |
| | `GET /system/user/ai/{id}/status` | 状态（gateway 是否在线） |
| | `POST /system/user/ai/{id}/repair` | 修复凭据 |
| | `DELETE /system/user/ai/{id}` | 删（先备份 SOUL） |
| **19 条安全红线** | `aiagent/safety.rs` `SAFETY_BLOCK` | ✅ 强制注入 + 自动补回 + 审计 |
| | 版本化标记 `<!-- CLOUDPIVOT-SAFETY-BEGIN v1 -->` | |
| | 启动自检 `check_ai_agent_missing_safety_block` | |
| **Hermes 集成** | `aiagent/hermes.rs` | ✅ |
| | `create_profile` / `delete_profile` / `write_soul` | |
| | `ensure_profile_config` / `ensure_profile_env`（写 provider API key） | |
| | SOUL.md 走 **stdin**（不进命令行参数，防泄漏） | |
| **前端** | `views/system/user/components/AiUserForm.vue` | ✅ |
| | `views/system/user/components/SoulEditor.vue`（SOUL 编辑器） | |
| | `api/system/ai-user-api.ts`（5 个函数） | |

**19 条安全红线的覆盖面**（`safety.rs` 实测）：
```
一、数据安全（4 条）：禁止未授权 SQL / 禁止删库删表 / 禁止删文件 / 禁止改审计日志
二、系统安全（6 条）：禁止改系统配置 / 禁止提权 / 禁止装软件 / 禁止读凭据 / 禁止外联 / 禁止杀进程
三、权限边界（3 条）：只操作自己任务 / 禁止改自己 SOUL / 禁止批量导出
四、产出纪律（3 条）：文件名带任务号 / 不得含凭据 / 破坏性操作先确认
五、诚实纪律（3 条）：不确定就停 / 失败如实报 / 不伪造证据
```

### 1.2 还没做的（❌ 本设计要补齐的）

| 缺什么 | 影响 |
|---|---|
| **❌ MCP Server** | Hermes 没有「调云枢 API」的正式通道（现在只能靠 SOUL.md 里的文字指引） |
| **❌ 动作白名单** | 无法限制「AI 用户能调哪些接口」 |
| **❌ 运行日志** | 无法回答「AI 员工在 10:00 做了什么、为什么」 |
| **❌ 人环确认** | HIGH 风险动作没有「先提议、等人批」的机制 |
| **❌ 调度** | 没有 CRON / EVENT 触发（AI 用户建了但不会自己动） |
| **❌ 4 个 AI 员工实现** | 质检/财务/采购/客服的具体能力 |
| **❌ 菜单入口** | AI 用户藏在「用户管理」里，客户找不到 |

### 1.3 一个已修复的历史坑（记录备查）

```
t_3b74de23：建 AI 用户时必须把 provider API key 写进 profile 的 .env
           否则 worker 起不来（已修）
t_c3b4d40d：创建 AI 员工时强制注入 19 条安全红线到 SOUL.md（不可删除）
           已实现 safety.rs
```

---

## 2. 要补齐的 6 块（按依赖顺序）

### 块 1：MCP Server（把云枢能力暴露给 Hermes）

**为什么必须有**：
```
现在的状态：AI 用户建好了、profile 建好了、SOUL 写好了
但 AI 用户**没法调云枢 API**（SOUL.md 里只能写文字指引，没有工具）

→ 就像一个员工入职了、工位有了、规则讲清了，但没有系统账号
```

**设计**（`backend/src/business/aiagent/mcp.rs` 或独立 crate）：

```
云枢暴露一个 MCP Server（:8101，只监听 127.0.0.1）
Hermes 作为 MCP client 连上来

初版 12 个工具（带风险分级）：

| 工具 | 方法 | 路径 | 风险 |
|---|---|---|---|
| cloudpivot.voucher.query | GET | /api/business/erp/voucher/page | LOW |
| cloudpivot.voucher.check | POST | /api/business/erp/voucher/{id}/check | LOW |
| cloudpivot.voucher.create | POST | /api/business/erp/voucher | MEDIUM |
| cloudpivot.voucher.post | POST | /api/business/erp/voucher/{id}/post | **HIGH** |
| cloudpivot.stock.query | GET | /api/business/erp/inventory-location/page | LOW |
| cloudpivot.inventory_txn.query | GET | /api/business/erp/inventory-txn/page | LOW |
| cloudpivot.ar.list_overdue | GET | /api/business/erp/receivable/overdue | LOW |
| cloudpivot.ar.collect | POST | /api/business/erp/receivable/{id}/collect | MEDIUM |
| cloudpivot.invariant.run | POST | /api/support/invariant/run | LOW |
| cloudpivot.linkage.run | POST | /api/business/flow/linkage/check | LOW |
| cloudpivot.task.create | POST | /api/business/task | MEDIUM |
| cloudpivot.notice.send | POST | /api/business/notice/send | MEDIUM |
```

**调用链**：
```
Hermes agent → MCP protocol → 云枢 MCP Server
  ↓ 校验 agent_token（AI 用户的 token）
  ↓ 查 sys_agent_action 白名单（这个 agent_type 能不能调这个动作）
  ↓ 查 risk_level
  ↓ HIGH → 写 sys_agent_confirm（返回 NEED_CONFIRM，不执行）
  ↓ LOW/MEDIUM → 代调云枢 API
  ↓ 写 sys_agent_run_log
```

### 块 2：动作白名单（`sys_agent_action`）

```sql
CREATE TABLE sys_agent_action (
    id              uuid PRIMARY KEY,
    agent_type      varchar(32) NOT NULL,          -- QA / FINANCE / PURCHASE / SERVICE
    action_code     varchar(64) NOT NULL,          -- voucher.post
    http_method     varchar(8) NOT NULL,
    http_path       varchar(255) NOT NULL,
    params_schema   jsonb,
    risk_level      varchar(8) NOT NULL,           -- LOW / MEDIUM / HIGH
    need_confirm    boolean DEFAULT false,
    enabled         boolean DEFAULT true,
    created_at      timestamptz DEFAULT now(),
    UNIQUE (agent_type, action_code)
);
```

**按 `agent_type` 授权**（不是按单个用户）：一次配好，所有同类型 AI 员工共享。

### 块 3：运行日志（`sys_agent_run_log`）

```sql
CREATE TABLE sys_agent_run_log (
    id              uuid PRIMARY KEY,
    run_id          uuid NOT NULL,
    agent_user_id   uuid REFERENCES sys_user(id),
    trigger_type    varchar(16) NOT NULL,          -- CRON / EVENT / MANUAL
    trigger_source  varchar(128),
    started_at      timestamptz NOT NULL,
    ended_at        timestamptz,
    status          varchar(16),                   -- RUNNING / SUCCESS / PARTIAL / FAILED / KILLED
    actions         jsonb,                          -- [{action_code, target_id, result, reason}]
    output_summary  text,                           -- 人话总结
    error           text,
    trace_id        varchar(64),
    created_at      timestamptz DEFAULT now()
);
```

**关键字段**：
```
· reason      为什么这么做（引用了哪条规则/哪个数据）
· trace_id    串起 LLM 调用 + SQL + 下游 API
· output_summary  人话总结（给客户看的）
```

### 块 4：人环确认（`sys_agent_confirm`）

```sql
CREATE TABLE sys_agent_confirm (
    id              uuid PRIMARY KEY,
    run_id          uuid NOT NULL,
    action_code     varchar(64) NOT NULL,
    target_type     varchar(32),
    target_id       varchar(64),
    payload         jsonb,                          -- 机器人想做什么（完整参数）
    reason          text,
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
→ 有权限的人在「首页待办」看到
→ 点批准 → 才真正执行
→ 超 24h 未处理 → EXPIRED（不执行，记日志）
```

### 块 5：调度（复用 `sys_job`）

```
三种触发：
① CRON     复用现有 sys_job（已有 cron 框架）
② EVENT    轮询（每 5 分钟扫 event_trigger 条件，用 run_log 去重）
③ MANUAL   用户点「让机器人跑一次」
```

**EVENT 一期用轮询**（不做真事件总线）：
```
· 每 5 分钟扫 biz_agent_config 里 run_mode='EVENT' 的
· 检查 event_trigger 条件（如 AR_OVERDUE days=30）
· 满足 → 触发，同一目标 24h 内不重复（查 run_log）
```

### 块 6：四个 AI 员工（按 ROI 排序）

| AI 员工 | agent_type | 做什么 | 价值 | 权限 |
|---|---|---|---|---|
| **质检机器人** `bot_qa` | QA | 每天跑 152 组勾稽 + 80 项不变量，有红就报告 | 无人值守兜底 | 只读（`invariant.run` + `*:list`） |
| **财务机器人** `bot_finance` | FINANCE | 凭证预审、自动对账、应收催收 | 会计省 2h/天 | `voucher.create` 直接，`voucher.post` 要确认 |
| **采购机器人** `bot_purchase` | PURCHASE | 安全库存预警 → 采购申请、询价催办 | 减少断货 | `purchase-request.create` 直接，`order.create` 要确认 |
| **客服机器人** `bot_service` | SERVICE | 工单分类 + 派单 + 首次响应 | 响应加速 | `ticket.*` 直接，对外发送要确认 |

**推广顺序**：**质检（只读，最安全）→ 财务 → 采购 → 客服**

---

## 3. 数据模型（补 4 张表，不动现有的）

| 表 | 状态 |
|---|---|
| `sys_ai_agent` | **已有**（不新建） |
| `biz_agent_config` | 新建（AI 用户专属配置：agent_type / run_mode / cron / event_trigger / 限额） |
| `sys_agent_action` | 新建（动作白名单） |
| `sys_agent_run_log` | 新建（运行审计） |
| `sys_agent_confirm` | 新建（人环确认） |

**`biz_agent_config` 设计**（关联 `sys_user`）：

```sql
CREATE TABLE biz_agent_config (
    user_id         uuid PRIMARY KEY REFERENCES sys_user(id) ON DELETE CASCADE,
    agent_type      varchar(32) NOT NULL,          -- QA / FINANCE / PURCHASE / SERVICE
    run_mode        varchar(16) DEFAULT 'MANUAL',  -- CRON / EVENT / MANUAL
    cron_expr       varchar(64),
    event_trigger   jsonb,
    config          jsonb,
    max_actions_per_run  int DEFAULT 100,
    max_runtime_seconds  int DEFAULT 300,
    created_at      timestamptz DEFAULT now(),
    updated_at      timestamptz DEFAULT now()
);
```

**注意**：`sys_ai_agent` 已经存了 `hermes_profile`，这张表只存「运行配置」，不重复。

---

## 4. 权限模型（复用 + 两个扩展）

```
复用：sys_user_role + sys_role_menu（角色 → 权限码）
扩展 1：sys_agent_action 的 risk_level（决定要不要人确认）
扩展 2：need_confirm 是「执行方式」不是「权限」
```

**关键区分**：
```
有权限       = 角色里有这个权限码（能不能做）
直接执行     = 动作库里 need_confirm=false（做完等不等批）

所以：AI 用户可以有 voucher:post 权限，但动作库设 need_confirm=true
→ 结果：能发起过账，但要人批
```

---

## 5. 144 的部署形态（D1 + D2 落地）

```
┌──────────────────────────────────────────────────────┐
│  144 生产服务器                                        │
│                                                       │
│  ┌────────────────────────────────────────────────┐  │
│  │ 云枢主服务 :8023                                │  │
│  │  · 系统管理 → AI 用户（已有，补菜单入口）         │  │
│  │  · AI 运行日志（新）                             │  │
│  │  · 待确认队列（新）                              │  │
│  └────────────────────────────────────────────────┘  │
│              ↕ HTTP / MCP                             │
│  ┌────────────────────────────────────────────────┐  │
│  │ MCP Server :8101（新）                          │  │
│  │  · 12 个工具（带风险分级）                        │  │
│  │  · 校验 agent_token + 白名单 + 写 run_log        │  │
│  └────────────────────────────────────────────────┘  │
│              ↕ MCP protocol                           │
│  ┌────────────────────────────────────────────────┐  │
│  │ Hermes Agent :8100（已有，PID 在跑）             │  │
│  │  profile 按需建（不复制 99 的 27 个）：           │  │
│  │   · bot_qa      ← 质检                           │  │
│  │   · bot_finance ← 财务                           │  │
│  │   · bot_purchase← 采购                           │  │
│  │   · bot_service ← 客服                           │  │
│  │  每个 profile：                                  │  │
│  │   · tools = [mcp:cloudpivot.*]（**无 terminal**） │  │
│  │   · SOUL.md 含 19 条安全红线（已有机制）          │  │
│  └────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────┘
```

**D1 的落地**：不预置 27 个 profile，只建 AI 员工要的 4 个（按需）。

---

## 6. 安全边界（六条，其中 3 条已有）

| # | 约束 | 状态 |
|---|---|---|
| ① | **19 条安全红线**（注入 SOUL.md，自动补回） | ✅ 已有（`safety.rs`） |
| ② | **profile 名白名单**（防命令注入） | ✅ 已有（正则 + CHECK 约束） |
| ③ | **SOUL.md 走 stdin**（不进命令行） | ✅ 已有（`hermes.rs`） |
| ④ | **动作白名单**（只能调登记过的） | ❌ 待做（块 2） |
| ⑤ | **HIGH 风险人确认** | ❌ 待做（块 4） |
| ⑥ | **Kill switch + 速率/超时** | ❌ 待做（`biz_agent_config` 的限额 + `sys_user.status`） |

**追加一条（重要）**：
```
⑦ AI 用户的 Hermes profile **tools 白名单里不含 terminal**
   → 防 prompt injection 后跑 rm -rf
   → 实现：建 profile 时写 config.yaml 的 toolsets 白名单
```

---

## 7. 验收（12 项）

```
【已有功能回归】
① AI 用户 CRUD 仍正常（建/查/改/删/状态/修凭据）
② SOUL.md 的 19 条红线删了能自动补回
③ profile 名非法（含空格/大写/超长）被拒

【新增：MCP + 白名单】
④ MCP Server 起来，Hermes 能连上并列出 12 个工具
⑤ AI 用户只能调白名单里的动作（不在 → 403）
⑥ 调 HIGH 风险动作 → 不执行，写 sys_agent_confirm

【新增：人环确认】
⑦ 人在首页待办看到「待确认」→ 批准 → 才真正执行
⑧ 超 24h 未处理 → EXPIRED（不执行）

【新增：调度与运行】
⑨ 质检机器人按 cron 跑，写 run_log（含 reason + trace_id + output_summary）
⑩ 人为造一个勾稽红 → 机器人发现 → 报告
⑪ Kill switch：停单个 AI 用户（status=0）→ 立刻不跑；全局开关同理

【审计】
⑫ 能查「机器人 X 在 10:00 做了什么、为什么、谁批的」
```

---

## 8. 工作量（**不派卡**，仅估算）

| 卡 | 内容 | 工时 | 依据 |
|---|---|---|---|
| AIAGT-01 | `biz_agent_config` + `sys_agent_action` + `sys_agent_run_log` + `sys_agent_confirm` 四张表 + 迁移 | 3h | 新建 |
| AIAGT-02 | MCP Server（12 个工具 + agent_token 校验） | 6h | 新建（核心） |
| AIAGT-03 | 动作白名单 CRUD + 风险分级 UI | 4h | 新建 |
| AIAGT-04 | 调度（CRON 复用 sys_job + EVENT 轮询） | 4h | 新建 |
| AIAGT-05 | 审计（run_log 写入 + 查询页） | 3h | 新建 |
| AIAGT-06 | 人环确认（confirm 队列 + 首页待办 + 批准） | 4h | 新建 |
| AIAGT-07 | Kill switch + 速率/超时限额 | 2h | 新建 |
| AIAGT-08 | profile 模板（4 个 agent_type）+ tools 白名单（无 terminal） | 3h | 补 `hermes.rs` |
| AIAGT-09 | 质检机器人（首发，只读，验证框架） | 4h | 新建 |
| AIAGT-10 | 财务机器人（预审凭证） | 5h | 新建 |
| AIAGT-11 | AI 用户菜单入口（从「用户管理」提到一级） | 1h | 小改 |

**合计约 39 小时**

**为什么比原 H 章的 34h 多 5h**：加了 MCP Server（6h，原设计没有），减掉了已完成的 AI 用户 CRUD（原 H 章 4h）。

---

## 9. AI 开发（ARCH-10 的 G 章）：暂不做

**用户原话**：「AI开发风险还是很大，先不做。」

**风险分析（供以后决策）**：

```
① 代码安全：AI 改的代码进主干 → prompt injection 可能植入后门
② 数据安全：AI 读业务数据时可能外泄（LLM 调用要出网）
③ 责任归属：AI 改错了导致客户损失 → 谁负责？
④ 审批依赖：需要客户有技术/业务/决策三级审批（中小企业没有）
⑤ 工程复杂度：三层隔离 + 部署审批流 → 运维成本高
```

**用户在 2026-11-10 讨论过的两个方向**（记录备查，**不实施**）：

| 方向 | 内容 | 特点 |
|---|---|---|
| **a. 「我的应用」目录** | 用户调 AI 开发的小工具，放在云枢「我的应用」下，与官方**完全隔离**（前后端都不动官方） | 隔离彻底、零升级风险；技术难度高（要插件热加载/微前端/schema 隔离） |
| **b. 沙箱 + 流程审批** | 沙箱改官方代码 → 测试环境验证 → 部署审批流 → 合并上生产 | 就是 G 章原设计；技术成熟，但改主干有冲突风险 |

**我的建议（供以后参考）**：如果将来要做，先做 a 的简化版（A2：前端页面 + 自定义只读查询），覆盖 80% 长尾需求，不动官方代码。

**状态**：⏸ **暂不做，等用户明确指令**

---

## 10. 待定事项

| # | 事项 | 我的建议 | 状态 |
|---|---|---|---|
| **TBD-A** | 四个 AI 员工先做哪几个？ | 先做质检（只读，最安全，能验证框架） | 等用户 |
| **TBD-B** | EVENT 用真事件总线还是轮询 | 先轮询（5 分钟一次够用，不用引入 MQ） | 等用户 |
| **TBD-C** | AI 用户的 KPI 怎么统计 | 从 run_log 聚合（做了多少事/省了多少时间） | 等用户 |
| **TBD-D** | 客户不信任怎么办 | 从只读的质检机器人开始，逐步放开 | 等用户 |
| **TBD-E** | token 成本谁出 | 客户自己的 API key（已支持 `ensure_profile_env`） | 等用户 |
| **TBD-F** | AI 用户的菜单放哪 | 建议一级菜单「AI 员工」（现在藏在用户管理里） | 等用户 |

---

## 11. 与 ARCH-10 的关系

```
ARCH-10 的 H 章（AI 机器人用户）：**已迁移到本文档**（ARCH-10 里已加迁移标注）
ARCH-10 的 G 章（AI 快速二次开发）：**暂停**（ARCH-10 里已加暂停标注）
ARCH-10 的 TBD-1（144 多 agent 用途）：**已并入本文档 §0 D1/D2**

ARCH-10 保留：A 凭证溯源 / B 一键交付 / C 智能填单 / D 大表分区 / E 错误码 / F H5 移动端
```

---

## 12. 状态

```
✅ 本文件 = 设计稿 v2（基于现有代码盘点，明确了「已有 60% / 缺 40%」）
⏸ 未派卡（用户 D4：没有明确指令不派卡）
⏳ 等用户确认：
   · §8 的 11 张卡要不要开始
   · §10 的 6 个 TBD（特别是 TBD-A 先做哪个机器人）
```
