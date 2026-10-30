# 批次 C+ · T3「分派 / 转派 / 协作人」验证证据

> 卡面：`t_30c7737d【任务管理·T3】分派/转派/协作人 + 通知分级 + AI分派按角色授予`
> 功能提交：`2ec9088`（基线修复 `b24c44d` 见 §9）　验证日期：2026-10-29
> 依据：`docs/批次C+-工作任务管理-核心设计.md` §八 / §九、
> 　　　`docs/云枢设计修正补丁-20261029.md` 修正 8（通知分级）/ 修正 11（AI 分派可授予）

## 0. 结论（一句话）

4 个接口 + 2 个权限码 + 通知 `force` 分级 + AI「默认不能、可按角色授予」全部落地：
`cargo test` **597 passed / 0 failed**、`scripts/check.sh` **全绿**、空库 `up→down→up` 通过、
真机验收 **34/34 断言通过**、生产已部署（`109 versioned` 已应用、4 个接口无令牌 401、admin 真实登录可见）。

## 1. 交付清单

| 类型 | 文件 |
|---|---|
| 迁移 | `backend/migrations/business/V0109__task_assign_permissions.{up,down}.sql` |
| 种子 | `backend/migrations/seed/S__task_permissions.sql`（追加第 3 段：只授 SUPER_ADMIN） |
| 接口 | `backend/src/business/task/handler.rs`（4 个 handler + 权限矩阵 + 通知接线） |
| 数据访问 | `backend/src/business/task/model.rs`（`assign_owner` / `insert_members` / `remove_member` / `member_ids` / `valid_users` / `user_type_of`） |
| DTO/常量 | `backend/src/business/task/dto.rs`（4 组入参回执 + `MAX_MEMBERS_PER_TASK=50` + 两个权限码常量） |
| 公共逻辑 | `backend/src/business/notice/mod.rs`（`NoticeDraft.force`）、`notice/fanout.rs`（偏好过滤层） |
| 审计 | `backend/src/business/changelog/diff.rs`（`BIZ_TASK` 白名单 + `owner_id` / `assigner_id` / `source`） |
| 错误码 | `backend/src/shared/error_code.rs`（63014–63020） |
| 模块文档 | `backend/src/business/task/mod.rs`（T3 行 + 通知分级表） |
| 基线修复 | `backend/src/business/aiagent/hermes.rs`、`aiagent/handler.rs`（见 §9） |

## 2. 接口（`/api` 前缀由 `[router].prefix` 注入）

| 方法 | 路径 | 中间件闸 | 说明 |
|---|---|---|---|
| PATCH | `/business/task/{id}/assign` | `business:task:read` | 分派 / 转派（`{owner_id, reason?}`） |
| POST | `/business/task/{id}/members` | `business:task:read` | 加协作人（`{user_ids:[...]}`，幂等、≤50） |
| DELETE | `/business/task/{id}/members/{user_id}` | `business:task:read` | 移除协作人 |
| GET | `/business/task/{id}/members` | `business:task:read` | 协作人列表（带可见性） |

### 2.1 为什么中间件闸是 `read` 而不是 `assign`

分派权的真实口径是**行级关系**：创建人 / 当前负责人 / 分派人天然可派。三类身份不是权限码能表达的
—— 把 `business:task:assign` 写进 `#[nik::permission]`，创建人（他往往没有这个码）会被中间件挡在门外。
所以：中间件只做入口闸，真矩阵在 handler 里；两个权限码是**加项**，给管理员 / 「AI 团队主管」这类
需要跨任务派活的角色用。

### 2.2 权限矩阵（`may_assign` / `may_manage_members`，单测逐格锁死）

| 身份 | 能否分派 / 管协作人 |
|---|---|
| 创建人 | ✅ |
| 当前负责人（转派给别人） | ✅ |
| 分派人 | ✅ |
| 持 `business:task:assign` / `business:task:member`（含超管 `*:*:*`） | ✅ |
| 其他人 | ❌ 403 + `63002` |

## 3. 迁移 V0109

1. 建 2 个按钮节点：`…0768 business:task:assign`、`…0769 business:task:member`（`menu_type=3`，挂在 `…0760` 工作任务下）；
2. 扩 `biz_task_comment.comment_type` 的 CHECK 值域，加入 `ASSIGN`
   —— **实测必须**：V0108 的三值 CHECK 会把转派评论拒掉（`500 + 10004 check constraint violated ck_biz_task_comment_type`，日志见 §7 备注）；
3. **授权只授 SUPER_ADMIN**（在种子迁移第 3 段执行，避开「版本化迁移先跑、角色还没建」的时序坑），
   其余角色由管理员在角色页勾选 —— 若像 read/create 那样全员授予，「无关用户分派 → 403」这条验收永远不成立。

空库 `up → down → 重复 up → 种子重跑 ×2` 实测（`/root/t3-mig.log`）：

```
===== 1) UP 后（启动自动跑）：2 个节点 + 超管授权
2                     ← 2 个节点
2                     ← SUPER_ADMIN 持有 2 个码
--- 非超管角色是否被误授
1                     ← 干净库为 0；本次是验收造出的 T3_ROBOT（§6 的 6c 显式授予，属预期）
===== 2) DOWN（回滚）
DELETE 3 / DELETE 2 / UPDATE 5 / ALTER TABLE ×2 / DO     ← 授权 3 行（超管2+T3_ROBOT1）→ 菜单 2 → ASSIGN 评论降级
--- down 后节点数 / 授权数（都应为 0）
0
0
===== 3) 再 UP（空库重建）+ 重复 UP（幂等）
INSERT 0 2 … INSERT 0 0        ← 第二次 up 命中 0 行（幂等）
2
===== 4) 种子重跑（幂等：授权回到超管）+ 再跑一次
2
===== 5) auth_code / 父节点 / 类型逐字核对
business:task:assign|3|20000000-0000-0000-0000-000000000760
business:task:member|3|20000000-0000-0000-0000-000000000760
```

库里的事实（`/root/t3_evidence.sh`）：

```
ck_biz_task_comment_type:
CHECK (((comment_type)::text = ANY ((ARRAY['PLAIN','STATUS_CHANGE','PROGRESS_AUTO','ASSIGN'])::text[])))
```

**down 的数据口径**：`comment_type='ASSIGN'` 的存量行降级为 `PLAIN`（保留评论行与正文，只丢类型标记），
与 V0108 down 的既定口径一致 —— 否则还原三值 CHECK 会因存量行失败。

## 4. 分派 / 转派（一次分派改五样东西，同一事务）

① `owner_id` + `dept_id` 快照　② `assigner_id`（操作人非创建人时=操作人）　③ `source=TRANSFERRED`
　④ **原负责人自动成为协作人**　⑤ `ASSIGN` 评论 + 变更日志（`owner_id/assigner_id/source`）

并发保护：`UPDATE … WHERE owner_id = ?（读到的旧值）` → 命中 0 行回 `409 + 63018`，避免并发转派时
「原负责人自动成协作人」记在错的人身上。

## 5. 通知分级（修正 8）

`notice::dispatch` 新增 `NoticeDraft.force`；偏好过滤**下沉到扇出 SQL**
（`fanout::respecting_preference` 的 `NOT EXISTS (… p.pref_key='notify.inbox' AND p.pref_value='false'::jsonb)`），
保持集合式扇出（一条 `INSERT ... SELECT`，5 万人不做 5 万次往返）。

| 事件 | 收件人 | `force` | 落地位置 |
|---|---|---|---|
| 被分派任务 | 新负责人 | **true** | `assign` |
| 任务被转派（你不再是负责人） | 原负责人 | **true** | `assign`（`exclude_operator=false`：原负责人常常就是操作人，这条回执必须发） |
| 被加为协作人 | 被加的人 | **true** | `add_members` |
| 协作人移除 | 被移除的人 | false | `remove_member` |
| 被打回 / 待你验收 | 负责人 / 创建人+分派人 | **true** | `notify_status_change` |
| 其它状态变更 / 子任务全部完成 | 负责人 + 创建人 | false | `notify_status_change` / `notify_ready_to_close` |

**同时删掉了 T2 的 Rust 侧双实现**（`model::inbox_enabled`）：偏好口径现在只有一处，
否则「任务通知看一份、公告/系统通知看另一份」迟早不一致。

**范围说明**：`force` 只作用于**立即发送**链路；定时通知（`scheduled_at` 未来）到点由巡逻任务扇出，
它读的是落库的 `target_type/target_ids`，没有 `force` 标记（要加得给 `sys_notice` 加列，属批次 A 接口变更，
本卡不动）—— 定时通知维持历史口径（不过滤偏好）。公告 / 人工发通知 / 工作流待办的历史口径也**不动**（都传 `force=true`）。

## 6. AI 执行者（修正 11）：默认不能，可按角色授予

- 实现 = **只查权限码**，`user_type` 不参与分派/协作人的准入判定 → 「可按角色授予」自然成立；
- 无码时 403 + `63002`，文案：「AI 用户默认不能分派任务，如需请授予角色权限（business:task:assign）」；
- **删任务仍是硬拒**（`403 + 63016`）：`is_robot(me)` 在 handler 最前面短路，连超管角色都救不回来
  （AI 被注入后「删任务」不可逆，而「停止执行」有正当替代路径：转派给人类 / 改为已取消）。

## 7. 逐条验收（真机，`/root/t3-accept.log`，34/34）

环境：`profile=test`、库 `cloudpivot_t3v`、`127.0.0.1:8026`（独立 target 目录 + 独立实例，不碰 dev 8024 / prod 8023）。
业务账号角色 `T3_STAFF`（任务 8 个节点，**不含** assign/member，`data_scope=4 仅本人`）——
这样「协作人能看任务」只能来自 `biz_task_member`，不可能是数据范围放水。

| 卡面项 | 实测 |
|---|---|
| 1 创建人分派给 B | `owner=t3b / source=TRANSFERRED / assigner=NULL`，B 收到「你被分派了任务」，changelog `ASSIGN` 且 `changed=owner_id,source`，评论 `ASSIGN` |
| 2 B 转派给 C | `owner=t3c / assigner=t3b`，B 自动进 `biz_task_member`，C 收到分派通知，B 收到「任务已被转派」 |
| 3 无关用户 D 分派 | `403 + 63002`，文案列出四条通路 |
| 4 分派给已删除用户 | `400 + 63009`「新负责人不存在或已停用」 |
| 5 分派给 ROBOT 用户 | `200`（AI 作为被分派方允许） |
| 6 ROBOT 作为操作人 | 授权前 `403 + 63002`「AI 用户默认不能分派任务…」；给 `T3_ROBOT` 角色勾上 `…0768` 后重登 → `200`，`owner` 变 C |
| 7 加 3 个协作人 | `biz_task_member` 3 条；t3m01（数据范围仅本人）`GET /{id}` → `200` |
| 8 重复加 | `added=0 / existing=3 / 库里仍 3 条` |
| 9 移除协作人 | `DELETE 200`；被移除者 `GET /{id}` → `404` |
| 10 移除创建人/负责人 | `400 + 63015` |
| 11 上限 | 一次加 51 → `400 + 63014`；正好 50 → `200`（库里 50）；第 51 人 → `400 + 63014` |
| 12 强制通知 | X 的 `notify.inbox=false`（库里 `false`）仍收到「你被分派了任务」 |
| 13 非强制通知 | 同一用户的状态变更：接收行 `before=1 after=1`（不新增），通知主体照常落库（`sys_notice` 有行、`receiver_count=0`） |
| 14 通知内容 | `你被分派了任务 \| 任务「通知分级验收 …」（TSK-202610-000006）已由 t3a 分派给你… \| /task/<uuid>` —— 标题 + 任务号 + 链接齐全 |
| 15 `cargo test` | 见 §8 |
| 16 `scripts/check.sh` | 见 §8 |
| 17 §6.2 部署验收 | 见 §10 |

附加两条：`附1` ROBOT 删任务 → `403 + 63016`；`附2` 无关用户加协作人 → `403`（与分派同一矩阵）。

库里的事实（`/root/t3_evidence.sh`）：

```
C) TSK-202610-000001 | owner=t3c | assigner=t3b | source=TRANSFERRED
   ASSIGN | 分派：负责人由 创建人甲 改为 接手乙；原因：数据层先做
   ASSIGN | 分派：负责人由 接手乙 改为 接手丙
   ASSIGN | 分派 / 转派 | changed=owner_id,source | diff={… "source":{"a":"TRANSFERRED","b":"SELF"}, "owner_id":{…}}
   ASSIGN | 分派 / 转派 | changed=assigner_id,owner_id | diff={… "assigner_id":{"a":<t3b>,"b":null} …}
   member: t3a role=COLLABORATOR      ← 创建人分派后成为协作人（仍能看）
   member: t3b role=COLLABORATOR      ← 原负责人转派后自动成为协作人
D) 通知： t3b <= 你被分派了任务 / t3a <= 任务已被转派 / t3c <= 你被分派了任务 / t3b <= 任务已被转派
E) notify.inbox=false 的 t3x： 你被分派了任务 → receiver_count=1 receivers=t3x
                              任务状态变更   → receiver_count=0 receivers=(none)   ← 偏好生效
F) 上限任务协作人数：50
```

> **备注（实现期踩到的坑，已修）**：转派写 `ASSIGN` 评论最初 500 —— V0108 的
> `ck_biz_task_comment_type` 只有三值。V0109 已扩值（见 §3）。

## 8. 单测与检查

```
cargo test            → 597 passed / 0 failed / 0 ignored（lib）+ 各集成 target 全 ok
cargo clippy --all-targets -- -D warnings → 干净（含 too_many_arguments 重构：TaskNotice 结构体）
scripts/check.sh      → 全部检查通过（fmt / clippy / check / typecheck / 批次B 闸①0 闸②0 / C7 G1–G6 / 色板一致性）
```

新增单测（正常路径 + 边界/错误分支）：

| 用例 | 覆盖 |
|---|---|
| `assign_and_member_permission_matrix` | 4 身份 × 2 码 × 2 动作 = 16 格逐格断言（含「无关用户+无码=拒绝」「AI 授码后放行」两格关键点） |
| `actor_role_prefers_creator_then_owner_then_assigner` | 身份优先级 |
| `assigner_id_follows_card_rule` | `assigner_id` 落库口径三种情形 |
| `creator_and_owner_are_protected_members` | 协作人保护 |
| `member_quota_boundary` | 50 允许 / 51 拒绝（`400 + 63014`）/ 满员重复提交不占额度 |
| `assign_owner_sql_is_guarded` | 单条 UPDATE + 乐观并发条件 + 6 个 uuid 参数 |
| `members_insert_sql_is_set_based` | 一条 `INSERT … SELECT` + `ON CONFLICT` + `RETURNING` |
| `ids_array_param_is_pg_literal` | `{uuid,uuid}` 字面量与空集合 |
| `preference_filter_only_when_not_forced` | 4 种目标 × 强制/尊重偏好两种 SQL 形态 |
| `preference_layer_keeps_params` | 过滤层不改变参数个数（防绑参错位） |
| `template_name_allows_reserved_default` | 见 §9 |

## 9. 基线修复（`b24c44d`，非本卡范围但阻塞验收红线）

`cargo test` 在 `origin/main`（`8f0c7f2`）上本来就是红的：
`fix(aiagent): clone_from 默认模板从 manong 改为 default` 把默认模板改成 `default` 后，
`create_profile` 仍用 `validate_profile_name`（**新建 profile 名**的规则）校验**模板名**，
而 `default` 在 `RESERVED_PROFILE_NAMES` 里 → 显式 `clone_from="default"` 直接 `400 + 64002`，
单测 `create_profile_failure_is_reported_with_stderr` 由绿变红（A/B 实测：`LEFT=64002 RIGHT=64005`，
已在 `origin/main` 上单独复现，与 T3 代码无关）。

修法：新增 `validate_template_name`（正则/长度照旧，注入面不放松，只不套保留名规则），
`create_profile` 与 `POST /system/user/ai` 的 `clone_from` 都改用它；新建名字仍禁止叫 `default`。
新增单测 `template_name_allows_reserved_default` 锁住两边语义。

## 10. §6.2 生产部署验收（`/root/t3_deploy.sh`）

```
新二进制指纹： sha256 54499f794e7c6bcf204c2b793d415e74595a359f92f358f3ce2a0ff4aedf64a4（32,370,424 B，release）
重启：         systemctl restart cloudpivot → active，/health profile=prod
迁移：         日志「可重复迁移内容已变化，重新执行 migration=task_permissions kind=seed」
               「数据库迁移完成: 应用 2 个迁移」
生产库：       109 versioned；business:task:assign / business:task:member 两节点在位；
               CHECK 含 ASSIGN；SUPER_ADMIN => 2
无令牌 401：   PATCH /{id}/assign、POST /{id}/members、DELETE /{id}/members/{uid}、GET /{id}/members 全部 401
               （不是 404/500 —— 路由已注册、鉴权在先）
真实登录：     admin 登录 OK；GET /api/business/task/page → code=0，返回真实任务（TSK-202610-000002 …）
```

## 11. 已知边界 / 未做（诚实清单）

1. **AI 用户「不能改数据权限」的硬拒不在本卡**：它落在 `system/role`（底座），本卡只落了「不能删任务」。
   底座改动需单独授权，建议随 T5（AI 执行者）一并做。
2. **定时通知的 `force` 不持久化**：见 §5 范围说明。
3. **评论 @ 提及**（核心设计 §十三 T3 行里的第三项）不在卡面范围，未做。
4. **AI 账号打标**在验收里用 SQL `UPDATE sys_user SET user_type='ROBOT'` 完成（正式通道要跑 hermes CLI，属 T5）；
   该验收库已删，不影响生产不变式自检。
5. `T3_ROBOT` 角色的一次 `business:task:assign` 授权是**验收造的数据**（在独立验收库里），生产库只有 `SUPER_ADMIN => 2`。

## 12. 复现

```bash
# 单测 / 检查
cd /opt/cloudpivot/backend && CARGO_TARGET_DIR=/var/tmp/t3-target cargo test
cd /opt/cloudpivot && CARGO_TARGET_DIR=/var/tmp/t3-target bash scripts/check.sh

# 迁移验证（独立库）
bash /root/t3_mig_check.sh        # 依赖 /root/t3-run 的 test 实例 + 库 cloudpivot_t3v

# 真机验收（独立实例 8026）
python3 /root/t3_reset_db.py      # 重建库 + 起实例
python3 /root/t3_accept.py        # 34 条断言
bash /root/t3_evidence.sh         # 库里的事实
```
