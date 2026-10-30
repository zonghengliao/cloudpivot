# 批次 D · D6a 端到端联调验证证据

- 卡面：`t_92b276ff`【D6a】端到端联调脚本 + 全流程验证（≤30 分钟）
- 交付物：`scripts/verify/flow_e2e.py`（新增 1 个脚本，**迁移 0 / 接口 0**）
- 结论：**PASS 64 / FAIL 0**，连续跑两次（`EXIT=0`），收尾 17 张表逐个回查 `count=0`

## 1. 环境与可复现命令

| 项 | 值 |
|---|---|
| 执行机 | 172.18.10.144（`/opt/cloudpivot`） |
| 被验服务 | 隔离实例 `127.0.0.1:8030`，profile=test，库 `cloudpivot_e2e`（**空库首启自动跑迁移**） |
| 二进制 | 复用 HEAD `fcd8816` 构建产物（本卡只加脚本，无 Rust 改动） |
| Redis | `redis://127.0.0.1:6379/8` |
| 启动脚本 | `/root/d6a_run.sh`（拷 config、软链 migrations、清端口后起进程） |

```bash
# 1) 空库 + 起实例
su postgres -c "psql -c 'CREATE DATABASE cloudpivot_e2e OWNER cloudpivot'"
bash /root/d6a_run.sh            # 输出 started: {"status":"ok",...}

# 2) 一键跑全流程
FLOW_E2E_BASE=http://127.0.0.1:8030/api \
FLOW_E2E_DB=cloudpivot_e2e \
FLOW_E2E_ENV_FILE=/root/d6a-run/d6a.env \
python3 scripts/verify/flow_e2e.py     # 退出码 0 = 全 PASS
```

脚本参数与默认值（都能用 `--base/--db/--env-file/--pg-host/--pg-port/--pg-user` 覆盖）：
默认指向 `127.0.0.1:8029` + `cloudpivot_d2v` + `/etc/cloudpivot/cloudpivot-test.env`，
环境文件只取两行：`ALUN_DATABASE_PASSWORD`（psql）、`ALUN_ADMIN_PASSWORD`（登录 admin）。

## 2. 卡面五步逐项对照（真实日志摘录）

### ① 建流程 → 3 节点 → 发布

```
[PASS] 2.1 新建流程定义（biz_type=D6A_E2E）
[PASS] 2.2 新建流程返回 DRAFT v1 + 版本 id
       def_id=01a18a35-… version_id=01a18a35-…
[PASS] 2.3 图落地：4 个节点（START/主管审批/财务会签/END）+ 3 条边、无保存期告警
       node_count=4 edge_count=3 warnings=[]
[PASS] 2.4 主流程 · 发布返回 64 位 graph_hash
       version_no=1 hash=608434f3a666b843…
[PASS] 2.6 定义状态 PUBLISHED + 当前版本已挂
```

> 卡面「3 节点」= 主干三个业务节点（**主管审批 / 财务会签 / 结束**）；START 是引擎占位节点、
> 无审批人、发起即 APPROVED。所以落地为 4 行节点 + 3 条边（`node_count=4 edge_count=3`）。
> 想严格 3 行就只能 START→会签→END，那样「主管通过」这一步无处落，故按业务语义实现。

### ② 发起 → 主管通过 → 财务会签 2/2 → 完成

```
[PASS] 3.2 发起后停在主管审批（实例 PENDING / START 自动过 / n_sup ACTIVE 应到 1 人）
       status=PENDING auto_passed=0 节点={'n_sup': ('ACTIVE', 1), 's': ('APPROVED', 0)}
[PASS] 3.3 主管待办可查（assignee_source=USER）  node=n_sup source=USER
[PASS] 3.4 主管通过（待办动作：节点 APPROVED / 实例仍 PENDING）
[PASS] 3.4b 下游财务会签激活：ACTIVE / 应到 2 人 / 已审 0 人 / ALL 模式
       n_fin=ACTIVE 0/2 mode=ALL
[PASS] 3.5 两位会签人各有一条待办（会签不是单点）
[PASS] 3.6 会签 1/2 未达阈值：节点仍 ACTIVE、实例仍 PENDING
       1/2 node=ACTIVE inst=PENDING
[PASS] 3.7 会签 2/2 达成：节点 APPROVED + 实例 APPROVED
       2/2 node=APPROVED inst=APPROVED
[PASS] 3.9 详情：4 个节点全 APPROVED、无自动通过、时间线留痕 ≥3 条
       节点={'n_sup': 'APPROVED', 's': 'APPROVED', 'n_fin': 'APPROVED', 'e': 'APPROVED'}
       auto_passed=0 timeline=3
```

**这一步是本卡最关键的语义断言**：`approval_mode=ALL` 下 1/2 不能过节点（3.6），
2/2 才落 APPROVED（3.7）——不是「两个人各点一次就过」。

### ③ 再造一单 → 驳回 → 退回发起人 → 重提 → 通过

```
[PASS] 4.2 主管驳回并退回发起人（return_to=INITIATOR）
[PASS] 4.3 驳回后实例未终止（PENDING，退回 = 同实例 round+1）
       inst=PENDING node=REJECTED next=['s']
[PASS] 4.4 发起人收到 RESUBMIT 待办（round 2）  node=s round=2
[PASS] 4.6 退回后时间线保留第 1 轮、并新增第 2 轮节点
       节点(轮次/状态)=[('n_sup', 1, 'REJECTED'), ('s', 1, 'APPROVED'), ('s', 2, 'ACTIVE')]
[PASS] 4.7 重提后回到主管审批（实例 PENDING + n_sup 第 2 轮 ACTIVE）
[PASS] 4.8 第 2 轮会签待办重新生成（旧轮次不复活）  fin1=2 fin2=2
[PASS] 4.9 重提链路走完：实例 APPROVED             2/2
[PASS] 4.11 详情：第 2 轮全通过、第 1 轮驳回留痕仍在
       inst=APPROVED 通过轮次=[1, 2] 驳回留痕=True
```

> 退回**不是**新开一单：同实例 `round+1`，第 1 轮节点行与驳回留痕全部保留；
> 「重提」的载体是发起人名下一条 `task_kind=RESUBMIT` 的待办（4.4），
> 点它才驱动引擎回到 `n_sup`。

### ④ 任务验收：REVIEW → 自动发起 → 通过 → DONE

```
[PASS] 5.1 建 TASK_ACCEPT 流程（默认流程）  → 5.2 发布 hash=3c8287a995f3a182…
[PASS] 5.3 建任务（need_review=true）  status=TODO
[PASS] 5.5 任务 DOING → REVIEW（触发自动发起审批）
[PASS] 5.7 REVIEW 时自动发起审批单并回写 approval_instance_id
       task_status=REVIEW approval_instance_id=01a18a35-…
[PASS] 5.9 审批单挂在任务上（biz_type=TASK_ACCEPT / biz_id=任务 id / PENDING）
[PASS] 5.11 审批通过 → 实例 APPROVED
[PASS] 5.13 流程回调把任务推到 DONE（REVIEW → DONE 不是人工点的）
       task_status=DONE
```

> 这一段验的是 D3b 的接线：`DOING→REVIEW` 时若配了 `TASK_ACCEPT` 默认发布流程，
> 任务提交验收**同事务**发起审批单并回写 `approval_instance_id`（5.7/5.9）；
> 审批通过后由 `FlowCallback` 身份把任务推到 `DONE`（5.13），不是人点出来的。

### ⑤ PASS/FAIL 汇总 + 清理

```
汇总：PASS 64 / FAIL 0
[PASS] 6.1 零残留：17 张表逐个回查 count=0
       回查明细：biz_flow_instance=0, biz_flow_instance_node=0, biz_flow_task=0, biz_flow_cc=0,
       sys_notice（审批通知）=0, sys_notice_receiver（送达）=0, biz_flow_node=0, biz_flow_edge=0,
       biz_flow_node_assignee=0, biz_flow_def=0, biz_flow_def_version=0, biz_task=0,
       biz_task_comment=0, sys_user（夹具账号）=0, sys_change_log（变更日志）=0,
       auth_logs（夹具登录留痕）=0, security_logs（夹具登录留痕）=0
```

连跑两次（`final_run1.log` / `final_run2.log` / `final_run4.log`）都是 `PASS 64 / FAIL 0`、`EXIT=0`，
第二次跑同上（脚本自清理后重跑结果一致 → 幂等可重跑）。

## 3. 断言分布（64 条）

| 段 | 条数 | 覆盖 |
|---|---|---|
| 0 前置 | 3 | `/health` 200、匿名访问流程定义 401、admin 登录（含首登改密） |
| 1 夹具 | 5 | SUPER_ADMIN 角色、3 个夹具账号创建、可登录 |
| ① 定义侧 | 6 | 建流程/存图/发布/64 位 hash/定义 PUBLISHED |
| ② 全通过 | 9 | START 自动过、主管待办、会签激活 0/2、待办双份、**1/2 不过**、**2/2 通过**、详情一致性 |
| ③ 驳回重提 | 11 | 驳回不终止、RESUBMIT 待办、轮次留痕、回到主管、第 2 轮会签、最终 APPROVED |
| ④ 任务验收 | 13 | TASK_ACCEPT 流程、TODO→DOING→REVIEW、自动发起、biz_id 挂靠、通过→DONE 回调 |
| ⑤ 清理 | 1（含 17 表回查） | 逐表 `count=0` |

每条断言都同时校验「HTTP 200 + code=0」**和**业务语义（实例状态 / 节点状态 / 票数 / 轮次 / 留痕），
只看 HTTP 码不算过。

## 4. 零残留的实现方式（不是「跳过清理」）

按依赖顺序硬删：`sys_notice_receiver/delivery → sys_notice → biz_flow_cc → biz_flow_task →
biz_flow_instance_node → biz_flow_instance → biz_flow_node_assignee → biz_flow_edge →
biz_flow_node → biz_task_comment → biz_task_member → biz_task → biz_flow_def_version →
biz_flow_def → sys_user_role → sys_user → auth_logs/security_logs(夹具) → sys_change_log`，
然后逐表回查。

全库回查（`cloudpivot_e2e`，跑完 4 轮之后）：

| 指标 | 值 |
|---|---|
| `biz_flow_*` 九表合计 | 0 |
| `biz_task` | 0 |
| `d6a_*` 夹具账号 | 0 |
| `sys_notice` | 0 |
| `sys_change_log` | 0 |

仅剩 `auth_logs=14 / security_logs=6`，全部是 **admin 账号自身**的登录审计行
（`select username,count(*) group by 1` 实测 `admin|14 / S:admin|6`）。这些是安全审计留痕、
不属于本卡造的测试数据，脚本按设计不清 admin 的审计行。

## 5. 踩到的坑（已写进脚本注释，后续卡直接用）

1. **`ActionRes.done` 是「这条待办处理完了」，不是「节点/实例结束」**。会签 1/2 时 `done=true`
   但实例仍 PENDING —— 断言写成 `done is False` 会假失败，要看 `node_status`/`instance_status`。
2. **`ActionRes.next_nodes` 对「START 节点驱动」不填**（发起、重提都是空数组），
   但 `approve` 会返回被点节点的 `node_status`。要看下游是否激活，去 `GET /instance/{id}`
   的 `nodes[]`（含 `approver_count/approved_count/approval_mode`）。
3. **`PUT .../graph` 的回执是 `node_count/edge_count/warnings`**，不是整图 `nodes[]`，
   断言别去找 `nodes`。
4. **登录限流**：`login:rate:{用户名}:{IP}`，60 秒窗口 / 上限 10 次（`shared/constants.rs`）。
   踩到 429 时**不能连打**（每打一次都加计数，会把窗口一直顶住），必须等满窗口；
   本实例的计数器在进程内缓存里，`redis-cli` 清不到，必要时重启实例。
5. 脚本会改 admin 密码（首登 `must_change_password`），所以候选密码列表按
   `初始 / 初始-d6a` 顺序试；同一空库连跑会自动落到已改过的那个。

## 6. 边界与未覆盖

- 本卡只验「能跑通 + 会清理」，**不覆盖**并发抢单、超时催办（D4c 独立卡）、抄送送达强度、
  条件/并行/加签/转办分支（各自有卡）。
- 脚本对库是**硬删**权限（psql 直连），只能指向测试库；docstring 顶部已红字标注，
  绝不要指向生产 `cloudpivot`。
