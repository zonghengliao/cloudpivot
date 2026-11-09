# NEAR-FIX-6 / N-7 证据：数据权限变更留痕 + 流程保存校验兜底人

- 卡：`t_8b0ea4b1`（【NEAR-FIX-6】N-7 数据权限变更留痕 + 流程保存校验兜底人）
- 评审扣分点：5.4「数据权限配置变更零留痕」、5.2「流程保存不校验兜底人存在性（错误延后到发起时才 422）」
- 主机：145 = 172.18.10.145（`/opt/cloudpivot`，库 `cloudpivot_dev`，验收走隔离实例 `:18033`）
- 采集时间：2026-11-10 04:2x ~ 04:4x CST
- 提交：`fa031793`（功能）→ `a112efb9`（验收脚本补 3b 段）；145 HEAD 与 origin/main 均为 `a112efb9`

## ① 数据权限配置变更零留痕

### 病

`PUT /business/permission/scope/role/{id}`（B4 的专用接口）把 `sys_role.data_scope` +
`sys_role_dept` 一起改掉，**不写任何审计**。`sys_role` 的富文本白名单里虽然有 `data_scope`，
但那条通路只有「系统 → 角色 → 编辑」的 `PUT /system/role/{id}` 走，权限设置页走的这条完全绕开它。
于是「谁在什么时候把「本部门及以下」改成「全部数据」」既不在变更日志里，也不在审计日志里。

### 修法（三处，缺一不可）

| # | 落点 | 文件 |
|---|------|------|
| 1 | 写库与变更日志**同事务**：`save_role_scope` 拆成只吃执行器的 `save_role_scope_in`，handler 自己开事务，先写范围+集合，再 `changelog::record_in`（写不进 → 整笔回滚，不留「改了权限但没凭证」） | `backend/src/business/permission/{scope_model,scope_handler}.rs` |
| 2 | diff 白名单补 `custom_dept_ids`：它是 `sys_role_dept` 的**物化集合**（排序后的 id 数组）。不登记的话，「只增减自定义部门、不动 data_scope」会被 diff 引擎判成「无变化」→ `Skipped` → 整条日志丢弃 | `backend/src/business/changelog/diff.rs` |
| 3 | `audit_logs` 另落一条事件级记录（`DATA_SCOPE_UPDATE`，`before_data` / `after_data` 两份快照） | `backend/src/business/permission/scope_handler.rs` |

### 实测（145 真实令牌）

```
PUT /api/business/permission/scope/role/<role>  {"data_scope":1}          → 200
  sys_change_log  changed_fields={data_scope}
                  diff={"fields":{"data_scope":{"a":1,"b":3}}}            ← b=3 本部门及以下 → a=1 全部
  audit_logs      DATA_SCOPE_UPDATE | {"data_scope":3,"custom_dept_ids":[]} | {"data_scope":1,"custom_dept_ids":[]}

PUT .../scope/role/<role>  {"data_scope":5,"custom_dept_ids":["<dept>"]}  → 200
  sys_change_log  changed_fields={custom_dept_ids,data_scope}
                  diff={"fields":{"data_scope":{"a":5,"b":1},
                                 "custom_dept_ids":{"a":["01a1b5f3-…"],"b":[]}}}
```

证据文件：`145_run1/01_change_log.txt`、`145_run1/01b_change_log_dept_set.txt`。

## ② 流程保存不校验兜底人存在性

### 病

节点上配了 `fallback_user_id` / `fallback_role_id`（P3-03 的第 ⑦ 步兜底），保存时**一个字都不查**。
配了已删 / 停用的人，界面「保存成功」，直到有人发起、走到该节点、角色又没人 —— 才在
`assignee::apply_fallback` 拿到 `422 + 65042`。那张单没有任何待办能推进它。
同类「错得晚」还有：兜底角色 0 成员、角色升级链（`sys_role.escalation_role_id`）指向已删角色、
普通节点指定的用户/角色/部门已被删。

### 修法

新增 `backend/src/business/flow/config_check.rs`（R29–R32，与 R1–R28 的纯函数校验并列）：

| 规则 | 判据 | 错误码 | 阶段 |
|------|------|--------|------|
| R29 | 兜底审批人（用户）存在、未停用、机器人需节点开 `allow_robot` | 65042 | 保存 + 发布 |
| R30 | 兜底角色存在、有**在职**成员（未过期、未软删、status=1，与运行期同口径） | 65042 | 保存 + 发布 |
| R31 | 角色升级链逐级存在（未软删）、无自引用、无环、≤ 8 级 | 65040 / 65041 | 保存 + 发布 |
| R32 | 节点可枚举审批人（USER / ROLE / DEPT_LEADER）指向的对象存在 | 65015 | 仅发布/只校验 |

接入点：

- `PUT /business/flow/version/{vid}/graph`（保存）→ 跑 R29–R31，非空即 `400`（**普通指定审批人只进 `warnings`**：
  编辑器是边改边存的，允许先落半成品）；
- `POST /business/flow/def/{id}/publish`（发布）与 `POST /business/flow/version/{vid}/validate`（只校验）
  → 跑 R29–R32 的「全流程预演」，两边同一份判据，编辑器点「校验」看到的就是发布会拦的全部问题。

查库分两半：`load()` 把 DB 读成 `Lookup`（用户/角色/部门各一条批量 SQL；升级链按层批量查，
最多 9 条，与节点数无关），`issues()` 是纯函数 —— 所以「哪种状态报哪条规则、文案点名谁」
能在无库环境逐条断言（11 条纯单测），真库只验「读库 → 快照」这一段（1 条，`ERP_DB_URL` 开关）。

### 实测（145 真实令牌，四条都是 HTTP 400）

| 场景 | HTTP | code | 文案（点名的部分） | 文件 |
|------|------|------|--------------------|------|
| 兜底人 = 不存在的用户 | 400 | 65042 | 节点 n_appr（财务复核）的兜底审批人 3ca457c7… 不存在或已删除 | `145_run1/02_ghost_fallback.txt` |
| 兜底人 = 停用用户 | 400 | 65042 | 节点 n_appr（财务复核）的兜底审批人「N7 停用用户」已停用 | `145_run1/03_stopped_fallback.txt` |
| 升级链指向已删角色 | 400 | 65040 | 角色「N7 孤儿链角色…」的升级上级 … 不存在或已删除 …… 节点 n_appr | `145_run1/04_escalation_deleted.txt` |
| 合法配置（兜底人=在职用户） | 200 | — | 保存 200（warnings=0）→ 发布 200 | `145_run1/05_legal_save.txt`、`06_legal_publish.txt` |

完整运行日志：`00_acceptance_145.log`（`RESULT: PASS`，夹具带 `n7acc_` 前缀并在末尾全部清理）。

## 验收对照

| 卡面验收 | 结果 | 证据 |
|---|---|---|
| ① 改数据权限 → operation_log 有记录（含 before/after） | ✅（见「偏差 1」：before/after 落在 `sys_change_log` + `audit_logs`） | `145_run1/01_change_log.txt`、`01b_change_log_dept_set.txt` |
| ② 配不存在的用户当兜底 → 400 + 点名节点 | ✅ 400 / 65042 / `n_appr（财务复核）` | `145_run1/02_ghost_fallback.txt` |
| ③ 配停用用户 → 400 | ✅ 400 / 65042（文案含「已停用」+ 用户名） | `145_run1/03_stopped_fallback.txt` |
| ④ 升级链指向已删角色 → 400 | ✅ 400 / 65040 | `145_run1/04_escalation_deleted.txt` |
| ⑤ 合法配置 → 正常保存 | ✅ 保存 200 + 发布 200 | `145_run1/05_legal_save.txt`、`06_legal_publish.txt` |
| ⑥ cargo test + 前端 build + push | ✅ 见下 | `01a`–`04` |

```
01a_unit_config_check.log   config_check 纯逻辑 11 passed / 0 failed
01b_unit_permission.log     permission 15 passed / 0 failed（含 N-7 ① 三条守护）
01c_db_test_config_check.log 真库 1 passed（ERP_DB_URL → 本机空库 cp_n7_check 自动迁移后跑）
02_lib_suite_4063passed.log  cargo test --lib 全量 4063 passed / 0 failed
03_clippy.log                cargo clippy --all-targets -- -D warnings 干净
04_frontend_build.log        pnpm build（含 vue-tsc typecheck）✓ built in 41.88s
```

## 偏差与说明（逐条诚实留档）

1. **「operation_log 有记录」的落点**：`operation_logs` 表只有 `detail` / `request_params` / `response_data`，
   **没有 before_data / after_data 列**；有前后快照列的是 `audit_logs`（V0014）与 `sys_change_log`（V0084）。
   因此 before/after 落在 `sys_change_log`（字段级 `{b,a}`，与业务同事务、写不进就整笔回滚）+
   `audit_logs`（事件级 `DATA_SCOPE_UPDATE` 两份快照），`operation_logs` 侧仍然是中间件写的请求流水。
   两处都实测到了（见 ① 的证据），没有退化成「只有请求路径」。
2. **`custom_dept_ids` 不是物理列**：它是 `sys_role_dept` 的物化集合（排序后的字符串数组），
   由 `scope_handler::scope_snapshot` 生成。加进 `diff::SYS_ROLE.fields` 后，
   `/business/changelog/table-registry` 会把它列在 `sys_role` 名下 —— 这是有意的（白名单即声明），
   注释里写清了它的来源。
3. **保存期只阻断「兜底 / 升级链」**：普通节点的指定审批人在保存期只进 `warnings`（400 只在发布/只校验时给）。
   理由：编辑器是边改边存的，刚拖进来的节点常常还没选人；若保存期也拦，等于没法存草稿。
4. **发布口径变严是一等公民的取舍**：R32 让「指定审批人已被删 / 已停用」的存量流程**无法重新发布**，
   直到把节点上的人换掉。这是设计意图（发布即对所有新单生效），但对存量数据是行为变化，
   已在卡内如实汇报，供后续判断是否需要「发布豁免名单」。
5. **145 没有重启服务**：本会话的命令审批策略禁止 `systemctl restart`（headless 单查询模式无人在场批准）。
   验收改用「同一二进制 + 同一 145 库 + 换端口 18033」的隔离实例（详见 `05_env_145.txt`），
   `cloudpivot-test` 服务全程 `active` 且未被打断；验完已 kill，18033 不再监听。
6. **真库单测跑在本机（99）的空库 `cp_n7_check`**：145 只有 7G 内存，test 二进制链接有 OOM 风险，
   且单测会往库里插夹具 —— 不在 145 dev 库跑。145 侧只做 HTTP 实测（夹具自建自清）。
7. **前端零改动**：本轮只动后端 + 验收脚本；`pnpm typecheck` / `pnpm build` 仍按卡面要求跑过（41.88s）。
