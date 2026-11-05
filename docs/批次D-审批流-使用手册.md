# 批次 D · 审批流使用手册

> 适用版本：批次 D（可编辑工作流审批 D1–D6）落地的 `main`。
> 面向读者：**管理员**（画流程、发布流程）、**发起人**（提单）、**审批人**（审批/退回）。
> 三个范例与「任务验收种子流程」都经过真机验证，见文末 §7「验证留证」。

---

## 0. 一分钟理解这套审批流

| 概念 | 库表 | 一句话 |
|------|------|--------|
| 流程定义 | `biz_flow_def` | 「这条审批是谁」：`flow_code` 是唯一编码、`biz_type` 决定哪类单据能发起、`is_default` 决定同类型默认用哪条 |
| 流程版本 | `biz_flow_def_version` | 每次发布冻结一份图（`graph_snapshot` + `graph_hash`）。**实例绑版本**，所以改流程不影响在跑的老单 |
| 节点 | `biz_flow_node` | 图上的方块：`START / APPROVAL / COUNTERSIGN / CONDITION / PARALLEL / CC / END` |
| 审批人 | `biz_flow_node_assignee` | 节点上「指定的那些人」（只允许 `USER / ROLE / DEPT_LEADER` 落行） |
| 连线 | `biz_flow_edge` | 图上的箭头；**条件写在边上**（`condition_expr`），不写在节点里 |
| 审批单 | `biz_flow_instance` | 一次发起；`PENDING / APPROVED / REJECTED / WITHDRAWN / TRANSFERRED …` |
| 待办 | `biz_flow_task` | 某个节点要某个人干的活：`APPROVAL / RESUBMIT / CC / TRANSFER …` |

三条铁律（记不住别的也要记住这三条）：

1. **只有 `PUBLISHED` 的版本能发起**。草稿（`DRAFT`）存得下但发不起来。
2. **运行时的图只读发布快照**（`graph_snapshot`），编辑器改行不影响在跑的实例。
3. **要改已发布的流程，不能改老版本**，只能「另存为新草稿 → 改 → 发布」，老版本自动 `ARCHIVED`。

---

## 1. 画一个流程：四步

```
① POST /business/flow/def                   建定义（自动带一个 DRAFT v1 版本）
② PUT  /business/flow/version/{vid}/graph   整图提交（nodes + edges 一次性覆盖）
③ POST /business/flow/version/{vid}/validate  发布前自检（可跳过，publish 内部同款校验）
④ POST /business/flow/def/{id}/publish      发布（写快照 + hash，切当前版本）
```

登录拿 token（后文所有 curl 都用它）：

```bash
TOKEN=$(curl -s -X POST http://127.0.0.1:8023/api/system/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"你的密码"}' | jq -r '.data.access_token')
BASE=http://127.0.0.1:8023/api
```

### 1.1 节点类型怎么选

| 类型 | 干什么 | 关键字段 |
|------|--------|----------|
| `START` | 起点（引擎占位，每个流程必有且只有 1 个） | — |
| `APPROVAL` | 单签：**任一**审批人通过就过 | `assignee_type` + `assignees` |
| `COUNTERSIGN` | 会签：按 `approval_mode` 决定过没过 | `approval_mode=ALL/RATIO/ANY`、`approve_ratio`（仅 RATIO）、`reject_veto`（一票否决） |
| `CONDITION` | 条件分支网关，出边 ≥ 2，**恰有一条 `is_default`** | 条件写在出边上 |
| `PARALLEL` | 并行网关，出边 ≥ 2，几条支线同时开工 | `fork_mode=AND`（默认） |
| `CC` | 抄送节点（只发通知，不阻塞流程） | `assignees` |
| `END` | 终点，**不做汇聚门控**（两个分支各自连 END 合法） | — |

### 1.2 审批人从哪来（`assignee_type`）

| 取值 | 含义 | 要落审批人行吗 |
|------|------|----------------|
| `USER` | 指定用户 | 要（`assignees:[{type:USER, ref_id:<用户id>}]`） |
| `ROLE` | 指定角色（该角色所有人都要审） | 要 |
| `DEPT_LEADER` | 部门负责人 | 要（`ref_id` 是部门 id） |
| `INITIATOR` | 发起人本人 | 不要 |
| `INITIATOR_SUPERIOR` | 发起人的上级（`superior_levels` 1–10） | 不要 |
| `TASK_OWNER` / `TASK_CREATOR` / `TASK_ASSIGNER` | 关联任务的责任人 / 创建人 / 派活人（**只有 `biz_type=TASK_ACCEPT` 支持**） | 不要 |
| `FORM_VAR` | 表单变量指定（`form_var_key`） | 不要 |
| `ADMIN` | 兜底给管理员 | 不要 |

节点上的三个兜底开关（强烈建议按需显式写）：

- `empty_assignee`：一个审批人都解析不出来时怎么办 —— `SKIP`（跳过该节点，默认）/ `ERROR`（卡住报错）/ `FALLBACK_ADMIN`（转给管理员）。
- `self_approval`：审批人 == 发起人时 —— `ALLOW`（可自审，默认）/ `SKIP`（自动跳过）/ `FORBID`（直接报错 `65019`）。
- `timeout_hours` + `timeout_action`：超时怎么处理 —— `REMIND`（默认，走催办）/ `AUTO_APPROVE` / `AUTO_REJECT` / `ESCALATE`。
  注意 `AUTO_APPROVE` 还要求流程级 `allow_auto_approve=true`，否则不生效（安全总闸）。

### 1.3 整图提交的 JSON 结构

```json
{
  "nodes": [
    {"key":"s","name":"开始","type":"START","x":0,"y":0},
    {"key":"n_lead","name":"主管审批","type":"APPROVAL","x":240,"y":0,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<用户id>"}]},
    {"key":"e","name":"结束","type":"END","x":480,"y":0}
  ],
  "edges": [
    {"key":"e1","from":"s","to":"n_lead"},
    {"key":"e2","from":"n_lead","to":"e"}
  ],
  "change_note": "初版"
}
```

- 接口收发都用 **`key`**（不是数据库 id）：`from`/`to` 填节点 key。
- `key` 正则：`^[A-Za-z][A-Za-z0-9_]{0,63}$`（字母开头）。
- `x`/`y` 只是编辑器坐标，随便填也不影响运行。
- 整图是**覆盖写**：漏传的节点/边会被删掉。

### 1.4 条件表达式（写在出边上）

```json
{"op":"cmp","field":"form.amount","cmp":"gte","value":10000}
{"op":"and","args":[{"op":"cmp","field":"form.amount","cmp":"gte","value":10000},
                    {"op":"cmp","field":"task.priority","cmp":"in","value":["P0","P1"]}]}
{"op":"not","args":[{"op":"cmp","field":"initiator.dept_id","cmp":"is_null"}]}
```

- 算子：`eq ne gt gte lt lte in nin contains is_null not_null`（`is_null/not_null` 不带 `value`）。
- 字段白名单：`form.*`（任意合法标识符）+ 固定字段 `initiator.id/name/dept_id`、`instance.biz_type/biz_no/title`、`task.status/priority/estimate_hours/title`。
  **没有函数调用、不能写 SQL** —— 这套 DSL 是强类型枚举，永久无代码执行。
- 嵌套深度 ≤ 8、条件节点数 ≤ 64、`args` ≤ 16 项。

### 1.5 发布时会被拦下的典型错误（含错误码）

| 错误码 | 含义 | 怎么改 |
|--------|------|--------|
| `65011` | 图里有环 | 删掉回指的连线 |
| `65012` | `CONDITION`/`PARALLEL` 出边少于 2 | 至少分两支，否则删掉网关 |
| `65013` | 条件节点出边不是「恰一条 default」/ default 边带了条件 | 每条条件边写条件，另加一条无条件的 `is_default:true` |
| `65014` | 汇聚 `join_mode` 不对 | 同一 `CONDITION` 汇聚用 `OR`，同一 `PARALLEL` 汇聚用 `AND`，不同网关不许混流 |
| `65015` | 审批节点没配审批人（或 `FORM_VAR` 没填变量名） | 补 `assignee_type` 与 `assignees` |
| `65016` | 条件字段不在白名单 / 算子非法 | 用 `form.*` 或固定字段 |
| `65017` | 节点 > 200 或边 > 400 | 拆流程 |
| `65022` | 想直接改已发布版本 | 用 `POST /def/{id}/draft` 另存新草稿 |

---

## 2. 三个可照抄范例

> 三个范例都在真机上跑过（建 → 存图 → 校验 → 发布 → 发起 → 审批到底），见 §7。
> 把 `<用户id>` 换成真实用户 id 即可：`GET /business/flow/def/page` 同权限下可从
> 系统管理 → 用户列表查，或用 `SELECT id FROM sys_user WHERE username='xxx'`。

### 2.1 范例一：单签（一个人审）

流程：`START → 主管审批 → END`

```bash
# ① 建定义
DEF=$(curl -s -X POST $BASE/business/flow/def -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"flow_code":"ex1_single","flow_name":"单签示例","biz_type":"DEMO"}')
DEF_ID=$(echo "$DEF" | jq -r .data.id); VID=$(echo "$DEF" | jq -r .data.version_id)

# ② 存整图
curl -s -X PUT $BASE/business/flow/version/$VID/graph -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{
  "nodes":[
    {"key":"s","name":"开始","type":"START","x":0,"y":0},
    {"key":"n_lead","name":"主管审批","type":"APPROVAL","x":240,"y":0,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<用户id>"}]},
    {"key":"e","name":"结束","type":"END","x":480,"y":0}],
  "edges":[{"key":"e1","from":"s","to":"n_lead"},{"key":"e2","from":"n_lead","to":"e"}],
  "change_note":"单签示例"}'

# ③ 发布
curl -s -X POST $BASE/business/flow/def/$DEF_ID/publish -H "Authorization: Bearer $TOKEN"
```

### 2.2 范例二：会签 + 条件（金额≥1万走财务会签 2 人，否则备案）

流程：`START → 主管审批 → 金额判断 →(≥10000) 财务会签(ALL, 2人) → END`
　　　　　　　　　　　　　　　`└─(默认) 备案登记 → END`

```bash
curl -s -X PUT $BASE/business/flow/version/$VID/graph -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{
  "nodes":[
    {"key":"s","name":"开始","type":"START","x":0,"y":0},
    {"key":"n_lead","name":"主管审批","type":"APPROVAL","x":200,"y":0,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<主管id>"}]},
    {"key":"n_if","name":"金额判断","type":"CONDITION","x":400,"y":0},
    {"key":"n_fin","name":"财务会签","type":"COUNTERSIGN","x":600,"y":-120,
     "assignee_type":"USER","approval_mode":"ALL","reject_veto":true,"self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<财务甲id>"},
                  {"type":"USER","ref_id":"<财务乙id>"}]},
    {"key":"n_file","name":"备案登记","type":"APPROVAL","x":600,"y":120,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<备案人id>"}]},
    {"key":"e","name":"结束","type":"END","x":820,"y":0}],
  "edges":[
    {"key":"e1","from":"s","to":"n_lead"},
    {"key":"e2","from":"n_lead","to":"n_if"},
    {"key":"e3","from":"n_if","to":"n_fin","priority":10,"label":"金额≥10000",
     "condition":{"op":"cmp","field":"form.amount","cmp":"gte","value":10000}},
    {"key":"e4","from":"n_if","to":"n_file","is_default":true,"label":"其它情况"},
    {"key":"e5","from":"n_fin","to":"e"},
    {"key":"e6","from":"n_file","to":"e"}],
  "change_note":"会签+条件"}'
```

要点：

- 会签 `approval_mode=ALL` ⇒ 2 个人都要点通过；`RATIO` 要配 `approve_ratio`（如 0.6）；`ANY` 是任一通过即过。
- `reject_veto=true` ⇒ 任一人驳回就整体驳回（与 `approval_mode` 正交）。
- **两个分支各自连 END 是合法的**（END 不参与汇聚门控），不必额外加汇聚节点。
- 发起时把表单变量放进 `form_data`：`{"def_id":"...","title":"...","form_data":{"amount":25000}}`。
  没传的 `form.*` 视为空 ⇒ 条件不成立 ⇒ 走 `is_default` 那条边。

### 2.3 范例三：并行（两条支线同时开工，再汇聚）

流程：`START → 并行分叉 →(技术评估 ∥ 商务评估) → 汇总审批(AND 汇聚) → END`

```bash
curl -s -X PUT $BASE/business/flow/version/$VID/graph -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{
  "nodes":[
    {"key":"s","name":"开始","type":"START","x":0,"y":0},
    {"key":"n_fork","name":"并行分叉","type":"PARALLEL","x":200,"y":0,"fork_mode":"AND"},
    {"key":"n_tech","name":"技术评估","type":"APPROVAL","x":420,"y":-120,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<技术id>"}]},
    {"key":"n_biz","name":"商务评估","type":"APPROVAL","x":420,"y":120,
     "assignee_type":"USER","self_approval":"ALLOW",
     "assignees":[{"type":"USER","ref_id":"<商务id>"}]},
    {"key":"n_join","name":"汇总审批","type":"APPROVAL","x":660,"y":0,
     "assignee_type":"USER","self_approval":"ALLOW","join_mode":"AND",
     "assignees":[{"type":"USER","ref_id":"<汇总人id>"}]},
    {"key":"e","name":"结束","type":"END","x":880,"y":0}],
  "edges":[
    {"key":"e1","from":"s","to":"n_fork"},
    {"key":"e2","from":"n_fork","to":"n_tech"},
    {"key":"e3","from":"n_fork","to":"n_biz"},
    {"key":"e4","from":"n_tech","to":"n_join"},
    {"key":"e5","from":"n_biz","to":"n_join"},
    {"key":"e6","from":"n_join","to":"e"}],
  "change_note":"并行"}'
```

要点：

- 汇聚节点必须写 `join_mode:"AND"`（同一 `PARALLEL` 的支线汇聚只允许 AND）；写 `OR` 会被 `65014` 拦（并行任务提前放行是事故）。
- 并行分叉用 `fork_mode:"AND"`（默认值，可以不写）。

---

## 3. 怎么发起

```bash
# 按流程 id 发起
curl -s -X POST $BASE/business/flow/instance -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"def_id":"<流程id>","title":"采购申请-2026-001","form_data":{"amount":25000}}'

# 按编码发起
  -d '{"flow_code":"ex2_countersign","title":"..."}'

# 按业务类型的默认流程发起（推荐：调用方不需要知道流程 id）
  -d '{"biz_type":"TASK_ACCEPT","title":"..."}'
```

返回 `data.instance_id` + `data.task_ids`（首批待办）。

- `def_id / flow_code / biz_type` **三选一**；按 `biz_type` 走的是「该类型 `is_default` 的已发布流程」。
- 同一 `biz_type + biz_no` 已有在跑的审批单会 `409 + 10003`。
- 任务验收不需要手工发起：任务 `DOING → REVIEW` 时如果存在已发布的 `TASK_ACCEPT` 流程，
  系统会**自动**发起并把 `approval_instance_id` 回写到任务上（没配流程则走原两级验收，不算错）。

---

## 4. 怎么审批 / 退回

### 4.1 看待办

```bash
GET /business/flow/task/todo?page=1&page_size=20      # 我的待办（APPROVAL / RESUBMIT / TRANSFER）
GET /business/flow/task/done?page=1&page_size=20      # 我已处理（按完成时间倒序）
GET /business/flow/task/cc?page=1&page_size=20        # 抄送我的
GET /business/flow/instance/my?page=1&page_size=20    # 我发起的
```

### 4.2 通过 / 驳回 / 退回 / 转办 / 加签 / 撤回

```bash
# 通过（意见可空）
POST /business/flow/task/{task_id}/approve   {"comment":"同意"}

# 驳回：退回给上一步（return_to 省略 = PREV）
POST /business/flow/task/{task_id}/reject    {"comment":"金额与合同不一致，请修改后再提"}

# 驳回并直接退回发起人（会生成发起人名下的「重提」待办，轮次 +1）
POST /business/flow/task/{task_id}/reject    {"comment":"...","return_to":"INITIATOR"}

# 驳回到指定节点（return_to 填节点 key），或「驳回即结束」用 "NONE"
POST /business/flow/task/{task_id}/reject    {"comment":"...","return_to":"n_lead"}

# 转办给别人
POST /business/flow/task/{task_id}/transfer  {"to_user_id":"<用户id>","reason":"我不在岗"}

# 加签（拉人一起审）
POST /business/flow/task/{task_id}/add-sign  {"user_ids":["<用户id1>","<用户id2>"]}

# 批量处理（兼容端点；内部逐条委托下面的 batch-approve，语义与单条完全一致）
POST /business/flow/task/batch  {"task_ids":["<t1>","<t2>"],"action":"approve","comment":"同意"}

# 批量处理（推荐：D7 之后的唯一实现，逐条走单条审批的全部业务回调与内控闸）
POST /business/flow/task/batch-approve  {"ids":["<t1>","<t2>"],"decision":"APPROVE","comment":"同意"}

# 发起人撤回整单（仅进行中、且尚未有人处理过时可撤；否则 65023）
POST /business/flow/instance/{instance_id}/withdraw  {"reason":"填错了"}
```

读返回值的正确姿势：

- `instance_status`：整单状态；`node_status`：当前节点状态。
- **`done` 只代表「你这条待办处理完了」，不代表节点/整单结束**。会签 2 人里 1 人通过时 `done=true`，
  但实例仍是 `PENDING`、节点还是 `ACTIVE` —— 要不要继续等，看 `node_status/instance_status`。
- `next_nodes` 对「START 节点驱动」的动作（首次发起、重提）**为空**，
  判断下游有没有激活要 `GET /business/flow/instance/{id}` 看 `nodes[].status`（附带 `approver_count/approved_count/approval_mode`）。

### 4.3 打回之后怎么重提

1. 审批人驳回并 `return_to:"INITIATOR"` ⇒ 实例仍 `PENDING`（不是 REJECTED），`round` 从 1 变 2。
2. 发起人待办里出现一条 `task_kind=RESUBMIT` 的待办（节点回到 `START`）。
3. 发起人改完单子，对这条待办点**通过**（`POST /task/{id}/approve`）＝ 重提；流程从 `START` 重新往下跑。
4. 重提后所有审批节点重新开始（上一轮的意见仍在实例轨迹里）。

---

## 5. 管理员怎么发布流程

### 5.1 首次发布（四步，见 §1）

```bash
POST /business/flow/def                       # ① 建定义 → data.id / data.version_id
PUT  /business/flow/version/{vid}/graph       # ② 整图
POST /business/flow/version/{vid}/validate    # ③ 自检（可选；publish 内部同款校验）
POST /business/flow/def/{id}/publish          # ④ 发布 → 返回 graph_hash / node_count
```

发布成功后：`def.status=PUBLISHED`、该版本 `is_current=true`、旧版本自动 `ARCHIVED`。

### 5.2 改已发布的流程（唯一正确姿势）

```bash
POST /business/flow/def/{def_id}/draft  {"source_version_id":"<可选，默认当前版本>"}
# → 得到新的 DRAFT v2（v1 一行不动），改完再 publish
```

直接 `PUT /version/{已发布vid}/graph` 会 `65022`（`FLOW_VERSION_NOT_EDITABLE`）。

### 5.3 下线 / 删除 / 查看版本

```bash
POST /business/flow/def/{id}/offline      # 下线：新单不能再发起，在跑的实例继续跑完
DELETE /business/flow/def/{id}            # 软删（有在跑实例会 65021 FLOW_DEF_IN_USE）
GET  /business/flow/def/{id}/versions     # 版本列表（含状态与发布人）
GET  /business/flow/instance/page         # 审批单列表（支持状态/类型/时间筛选）
GET  /business/flow/stats/overview        # 总览（待办量、通过率、平均时长）
GET  /business/flow/stats/efficiency      # 效率（按流程/节点平均耗时）
GET  /business/flow/stats/node-bottleneck # 卡点（哪一步最堵）
```

### 5.4 权限（给谁开哪一档）

| 权限码 | 用途 |
|--------|------|
| `business:flow:read` | 看流程定义/版本/图 |
| `business:flow:create` / `update` / `delete` | 画、改、删流程（管理员） |
| `business:flow:publish` | **发布/下线**（建议只给管理员） |
| `business:flow:instance:create` / `read` | 发起 / 看待办与统计 |
| `business:flow:instance:withdraw` | 撤回自己的单 |
| `business:flow:approve` | 通过/驳回/转办/加签/批量 |

权限节点由迁移 `V0121`（定义侧）与 `V0124`（运行侧）种子化，挂在 `S__workflow_permissions.sql` 里。
**注意**：审批动作除了权限码，还要求「这条待办确实是你的」——`65028` 表示不是你的活，`65004` 表示越权。

### 5.5 任务验收默认流程 `task_accept`（迁移 V0127 内置）

全新库开箱即有一条**已发布 + 默认**的流程：

| 项 | 值 |
|----|----|
| `flow_code` | `task_accept` |
| `biz_type` | `TASK_ACCEPT`（任务提交验收） |
| 图 | `START(s) → 创建人验收(n_creator) → END(e)`，`graph_hash=f5e20abd1098ed8b6fa84d713d5c5456db4e5743cbd4cdb7605cd13d3761c05f` |
| 审批人 | `TASK_CREATOR`（运行时按 `biz_task.creator_id` 解析，故无审批人行） |
| 节点参数 | `self_approval=ALLOW`、`empty_assignee=SKIP`、`timeout_hours=48` + `timeout_action=REMIND` + `max_remind_times=2` |

效果：任务的执行人把任务 `DOING → REVIEW` ⇒ 自动发起审批单 ⇒ 任务**创建人**收到待办 ⇒
通过后由 `FlowCallback` 把任务推到 `DONE`；驳回/退回则任务回到原状态。**零配置可用。**

想改它（比如换成「项目经理 + 创建人」两级）：`POST /business/flow/def/{task_accept的def_id}/draft`
新建 v2 → 改图 → publish。老单继续按 v1 跑。

---

## 6. 常见问题（FAQ）

**Q1 打回的单子怎么重提？**
见 §4.3：发起人待办里会出现 `RESUBMIT` 待办，对它点「通过」即重提（轮次 +1，流程从 START 重跑）。
驳回时如果用 `return_to:"NONE"`（或 `PREV` 回到上一节点），就不会有重提待办，单子按节点继续走。

**Q2 流程改了，在跑的老单怎么办？**
不受影响。实例在发起那一刻就绑定了版本（`biz_flow_instance.version_id`），运行只读该版本的发布快照。
改流程 = 发新版本，只影响**改完之后新发起**的单子。要看老单当初走的什么图：
`GET /business/flow/instance/{id}` 里带着 `version_no`，或 `GET /def/{id}/versions` 查历史版本。

**Q3 发布时报 `65015 审批人缺失`？**
节点写了 `assignee_type` 但没选人（`USER/ROLE/DEPT_LEADER` 必须有 `assignees`）；或者选了 `FORM_VAR` 却没填 `form_var_key`。

**Q4 条件分支走错了？**
先看条件边的字段是否真的在表单里：`form_data` 里没有的键视为空 ⇒ 条件不成立 ⇒ 走 `is_default` 边。
字段名必须命中白名单（`form.*` 或 §1.4 的固定字段），否则发布就报 `65016`。

**Q5 会签 2 个人，1 个通过就结束了？**
说明 `approval_mode` 不是 `ALL`。`ANY` = 任一通过即过；`RATIO` = 达到 `approve_ratio` 才过。
另外，同一个用户既在 `USER` 里又在某个 `ROLE` 里会被**去重**成 1 个人。

**Q6 我自己发的单能自己批吗？**
看节点的 `self_approval`：`ALLOW` 可以（默认），`SKIP` 自动跳过该节点，`FORBID` 报 `65019`。

**Q7 审批人离职/没配到人，单子卡住了怎么办？**
节点的 `empty_assignee`：`SKIP`（跳过）/`ERROR`（发起就报 `65018`）/`FALLBACK_ADMIN`（转管理员）。
已经在跑的单子可以由有 `business:flow:approve` 的人用**转办**（`transfer`）接力，或用**加签**（`add-sign`）拉人。

**Q8 会催办吗？**
会。节点设了 `timeout_hours` 后，后台任务每 10 分钟扫一次超时待办并发提醒，
同一条待办最多催 `max_remind_times` 次（默认 3），台账记在 `biz_flow_task.urge_count/last_urge_at`。

**Q9 错误码到哪查？**
`65001` 流程不存在、`65002` 审批单不存在、`65003` 待办不存在、`65004` 越权、`65005` 字段非法、
`65010~65017` 图结构/条件问题、`65018` 解析不到审批人、`65019` 禁止自审、`65020` 待办已处理、
`65021` 流程有在跑实例不能删、`65022` 已发布版本不可编辑、`65023` 该单不可撤回、`65024` 该单不在进行中、
`65025` 退回目标非法、`65026` 转办非法、`65027` 加签非法、`65028` 不是你的待办、`65031` 超时配置非法。

---

## 7. 验证留证（真机跑出来的，不是纸上推演）

| 验证 | 脚本 | 结果 |
|------|------|------|
| 全新空库（144 隔离实例 8032 / 库 `cloudpivot_seed2`）由迁移器建库建表 + V0127 种子，零配置发起任务验收并跑完 | `d6b_verify.py` | **24/24 PASS**：种子行 PUBLISHED/默认/v1、快照重算 sha256 == `graph_hash`、`DOING→REVIEW` 自动发起、待办落到 `TASK_CREATOR`、通过后任务 `DONE`、up.sql 重放不产生第二份 |
| 三个手册范例（单签 / 会签+条件 / 并行）建→存图→校验→发布→发起→审批到底 | `d6b_examples.py` | **30/30 PASS**：均 `passed=true` 发布成功；会签 ALL 2 人、条件命中走财务会签、并行两支同时 ACTIVE 且 AND 汇聚后才放行 |
| V0127 down/up 往返 | `d6b_downup.sh` | down 后 def/version/node/edge 全 0（零残留断言过）；up 恢复且 hash 一致；对「已有第三方 `task_accept` 流程」的库跑 down **不动别人的行** |
| 条件不成立走兜底边 | `d6b_branch.py` | 发起时不传 `form.amount` ⇒ 主管通过后 **`n_file`（备案登记）ACTIVE、财务会签未激活**，兜底分支符合预期 |
| 回归 | `cargo test --lib` | **810 passed / 0 failed** |

---

## 附录：接口清单（前缀 `/api`）

| # | 方法 | 路径 | 用途 |
|---|------|------|------|
| 1 | GET | `/business/flow/def/page` | 流程定义分页 |
| 2 | GET | `/business/flow/def/{id}` | 定义详情（含图） |
| 3 | POST | `/business/flow/def` | 建定义（带 DRAFT v1） |
| 4 | PUT | `/business/flow/def/{id}` | 改定义基本信息 |
| 5 | DELETE | `/business/flow/def/{id}` | 软删定义 |
| 6 | POST | `/business/flow/def/{id}/draft` | 另存为新草稿版本 |
| 7 | POST | `/business/flow/def/{id}/publish` | 发布 |
| 8 | POST | `/business/flow/def/{id}/offline` | 下线 |
| 9 | GET | `/business/flow/def/{id}/versions` | 版本列表 |
| 10 | GET | `/business/flow/version/{vid}` | 版本详情 |
| 11 | GET | `/business/flow/version/{vid}/graph` | 读整图 |
| 12 | PUT | `/business/flow/version/{vid}/graph` | 整图覆盖写 |
| 13 | POST | `/business/flow/version/{vid}/validate` | 发布前校验 |
| 14 | POST | `/business/flow/instance` | 发起审批单 |
| 15 | GET | `/business/flow/instance/page` | 审批单分页 |
| 16 | GET | `/business/flow/instance/{id}` | 审批单详情（节点轨迹） |
| 17 | POST | `/business/flow/instance/{id}/withdraw` | 撤回 |
| 18 | GET | `/business/flow/instance/my` | 我发起的 |
| 19 | POST | `/business/flow/task/{id}/approve` | 通过（RESUBMIT 待办即重提） |
| 20 | POST | `/business/flow/task/{id}/reject` | 驳回 / 退回 |
| 21 | POST | `/business/flow/task/{id}/transfer` | 转办 |
| 22 | POST | `/business/flow/task/{id}/add-sign` | 加签 |
| 23 | POST | `/business/flow/task/batch` | 批量处理（兼容层） |
| 23b | POST | `/business/flow/task/batch-approve` | 批量处理（推荐） |
| 24 | GET | `/business/flow/task/todo` | 我的待办 |
| 25 | GET | `/business/flow/task/done` | 我已处理 |
| 26 | GET | `/business/flow/task/cc` | 抄送我的 |
| 27 | GET | `/business/flow/stats/overview` | 统计总览 |
| 28 | GET | `/business/flow/stats/efficiency` | 效率统计 |
| 29 | GET | `/business/flow/stats/node-bottleneck` | 节点卡点 |
