# E2E-09 系统管理域演练：用户 / 角色 / 部门 / 菜单 / 权限 / 数据权限 / 脱敏 / 互斥

> 卡面：`t_3fa6665a`｜日期：2026-11-09｜执行机：145 测试机（`cloudpivot_dev`，真后端 `127.0.0.1:8033`）
> 演练时代码：`808b6910`（含本卡修复）｜断言：**323 PASS / 0 FAIL**
> 证据：`raw/asserts.log`（323 行逐条断言）、`raw/run_all.log`（四段完整输出）、`raw/*.json`（关键响应原文）

## 一、演练范围（8 个页面 + 2 项关键点 + 2 项历史回归）

| # | 页面 | 覆盖点 |
|---|------|--------|
| ① | 用户管理 | 建用户（带部门 + 角色）→ 改角色（全量替换/回退）→ 停用/启用 → 管理员重置密码（首登强制改密）→ 批量调岗 → 用户名重复 409 |
| ② | 角色管理 | 建角色（data_scope / 上级角色）→ 菜单授权（分配/回读/清空）→ 角色用户列表 → 编码重复 409 → 上级角色自引用/成环 400（30013） |
| ③ | 菜单管理 | 目录/页面/按钮三层树 → 排序 → 可见性 → 按钮软删 → title/sort 不变量（本卡节点） |
| ④ | 部门管理 | 部门树（父子 + dept_path）→ 负责人（leader_name 实时取名）→ 编码/同名 409 → 迁移删除（用户先迁、部门后删，同事务）→ 有子部门 409（30007） |
| ⑤ | 数据权限 | 范围规则（1/2/4/5）**真实数据过滤**（本部门/仅本人/自定义/全部）→ 非法范围 61001 / 部门不存在 61004 → 防自我扩权 61003 / 越界 61002 → 部门树按范围裁剪 → 自检注入片段 |
| ⑥ | 字段脱敏 | 角色规则 CRUD → 重复 409 / 目标列不存在 61006 / 类型非法 61005 → 试运行（手机号/身份证/邮箱/自定义参数）→ 生效清单（未豁免）→ **真实接口出参打码**（列表+详情同口径）→ 停用回落全局规则 → 恢复 |
| ⑦ | 角色互斥 | 建规则（规范化回显）→ 自反 400（61012）→ 重复对 409 → 冲突授予 409（61009）→ 存量双持拦建/拦启用 409（61010）→ 停用后放行 → 体检 / 全量扫描 |
| ⑧ | 变更日志 | 列表（resource 过滤 + 操作人）→ 对象时间线 → 统计 → 表清单 → 详情 → 导出 xlsx（L1+L3 脱敏头）→ **L1 写库即脱敏**（diff 无明文） |
| 关键点 | 权限生效 | 未分配 → 403（报文点名缺失权限码）；分配后重登 → 200；收回 → 旧令牌 401/20008（无残留越权） |
| 关键点 | 审批降级（ARCH-8） | 自审 SKIP → 升级上级角色（留痕两列）→ 二级上溯 → 到顶仍无人 422/65018 → 兜底人接管 → 兜底配错 422/65042 → 成环 422/65040 → FORBID 回归 65019 |
| 历史 | MENU-FIX-1（V0335） | 全库 menu_type 1/2 的 title >12 字 = **0**；同父 sort 重复 = **0**；`menu_invariants.sh` **8 条全绿**（434 条菜单，含 icon 合法性） |
| 历史 | V0320 单号序列归位 | `/business/erp/seq` parent = `/business/erp/master-data`、sort=30；迁移 320/335 均已登记 |

## 二、结果总览（节选；完整 323 条见 `raw/asserts.log`）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 建部门 | `POST /system/dept` ×4（D1/D1A/D2/D3） | 200 | `sys_dept` 4 行，D1A.parent=D1，dept_path 含父 id | ✅ |
| 部门重复 | 同编码 / 同父同名 | 409 / 409 | 未落库 | ✅ 30008 / 30011 |
| 建角色 | `POST /system/role`（VIEWER scope=2，BOSS scope=1） | 200 | data_scope 与请求一致 | ✅ |
| 上级角色 | `PUT /system/role/{id}` escalation=BOSS | 200 | `sys_role.escalation_role_id`=BOSS | ✅ |
| 自引用/成环 | escalation=自己 / 互指 | 400 / 400 | 未落库 | ✅ 30013（报文含「不能是本角色自己」/「环」） |
| 建用户 | `POST /system/user`（dept=D1A, role=VIEWER） | 200 | `sys_user.dept_id`=D1A；`sys_user_role` 1 行 | ✅ |
| 停用 | `PATCH /system/user/{id}/status` 0 → 登录 | 200 → 403 | status=0 | ✅ 20002 ACCOUNT_DISABLED |
| 改密 | `PATCH /system/user/{id}/password` → 新密码登录 | 200 → 200 | `must_change_password=true` | ✅ 首登强制改密生效 |
| 调岗 | `PUT /system/user/batch-dept` | 200 | dept_id 变更 | ✅ |
| 菜单 | 目录/页面/按钮 + 可见性 + 删除 | 200 | 三层树、is_visible=false、按钮软删 | ✅ |
| **权限闸** | 未授权访问 `/system/user/page` | **403** | — | ✅ 报文点名 `system:user:list` |
| **权限闸** | 授权 → 重登 → 访问 | 200 | `sys_role_menu` 1 行 | ✅ |
| **权限闸** | 收回 → 旧令牌访问 | **401/20008** | token_version 推进 | ✅ 无残留越权 |
| 数据权限 | VIEWER 2/4/5/1 四档切换 | 200 | data_scope 2/4/5/1 | ✅ 任务列表可见集随档位变化（见勾稽 ④） |
| 数据权限 | 操作员改自己角色 / 越界 | 403 / 403 | 未改 | ✅ 61003 / 61002 |
| 脱敏 | 角色规则 2,2 + 试运行 | 200 | `sys_field_mask` 1 行 status=1 | ✅ 出参 `13****01`，admin 明文 |
| 互斥 | 建规则 → 双授 | 200 → 409 | 无半截写入（1 行） | ✅ 61009 |
| 互斥 | 存量双持 → 建规则/恢复启用 | 409 / 409 | 未落库 | ✅ 61010（点名持有者） |
| 变更日志 | 列表/时间线/统计/详情/导出 | 200 | `sys_change_log` CREATE+UPDATE | ✅ 导出头 `x-mask-applied: true` / `L1+L3` |
| 降级链 | 自审 SKIP → 上级角色审批 | 200 → 200 | 2 条待办 escalated_from_role=MAKER；实例 APPROVED | ✅ |
| 降级链 | 二级上溯（0→0→命中） | 200 | reason=NO_MEMBER、by=ESCALATED | ✅ |
| 降级链 | 到顶仍无人 | 422 | 0 实例残留 | ✅ 65018（报文含「升级链已走完」+ 链路诊断 1 级） |
| 降级链 | 兜底人 | 200 | by=FALLBACK，审批人=boss1 → APPROVED | ✅ |
| 降级链 | 兜底配错 | 422 | 0 实例残留 | ✅ 65042（报文点名 bogus uuid） |
| 降级链 | 升级链成环 | 422 | — | ✅ 65040 |
| 降级链 | FORBID 回归 | 422 | 0 实例残留 | ✅ 65019 |

## 三、勾稽验证（9 组，全部 0 差异）

| # | 勾稽项 | 结果 |
|---|--------|------|
| ① | 部门树：`sys_dept.parent_id` / `dept_path` 与树接口嵌套一致 | 子部门 parent=D1、path 含父 id；树里 D1.children=[D1A] |
| ② | 角色-菜单：`sys_role_menu` 行数 ↔ 授权接口回读 ↔ 实际访问结果 | VIEWER 4 个权限码；回读 4 行；授予→200 / 收回→403 |
| ③ | 用户-角色：`sys_user_role` 行数 ↔ 用户接口 roles 回显 | auditor 1 行 / VIEWER；临时授 2 角色时 DB=2、回显 2 |
| ④ | 任务归属：`biz_task.dept_id` = 负责人所在部门 | 3 条任务 0 处不一致；数据权限按部门过滤的可见集精确匹配（2 档：T1+T3；4 档：T1；5 档：T1+T2；1 档：全 3 条） |
| ⑤ | 数据权限：`sys_role.data_scope` ↔ 过滤结果 ↔ 自定义部门存量 | 回滚为 2 后可见集恢复；`sys_role_dept` 保留 1 行（三态语义：切回 2 不清空，便于切回 5） |
| ⑥ | 脱敏：库内明文 ↔ 出参打码 | `sys_user.mobile`=13800009001（明文），taskview 出参 `13****01`，admin 出参明文；规则停用即回落全局 `138****9001` |
| ⑦ | 互斥：规则对规范化 ↔ 双持用户数 | `role_a_id < role_b_id`=1；A/B 双持用户 0 人 |
| ⑧ | 变更日志：`sys_change_log` ↔ 实际操作 | VIEWER 有 CREATE + UPDATE，且 UPDATE 的 `changed_fields` 含 `escalation_role_id`（本卡修复项）；L1 检查：diff 中手机号明文 0 行、打码值存在 |
| ⑨ | 降级链留痕：`biz_flow_task.escalated_from_role/escalate_reason/assigned_by_rule` ↔ 场景 | SKIP→MAKER/SKIP_SELF_APPROVAL ×2；上溯→DEAD/NO_MEMBER/ESCALATED；兜底→FALLBACK ×1；最终 **0 条 PENDING 待办残留** |

## 四、问题清单（P0–P4）

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|------|------|----------|------|------|
| 1 | **角色「上级角色」变更不入变更日志**（`escalation_role_id` 不在 `SYS_ROLE.fields` 白名单 → diff 恒空 → `RecordOutcome::Skipped`） | **P3（已修）** | 建角色 A/B → `PUT /system/role/A` 设 `escalation_role_id=B`（200）→ 查 `sys_change_log`（无记录）/ 变更日志时间线只有 CREATE | 审批降级链是内控配置（谁能接管审批），改了查不到，出事只能翻库比对 | 白名单补列 + 回归单测 → **本次已修**（`808b6910`） |
| 2 | **数据权限配置变更零留痕**：`PUT /business/permission/scope/role/{id}` 既不写 `sys_change_log` 也不写 `audit_logs` | P3（待拍板） | 把某角色 `data_scope` 从 1 改成 5 再改回，查两个审计表均无该操作 | 提权面（数据可见范围）变更无痕，与 `role.data_scope` 在 `/system/role` 侧有日志形成两套口径 | 在 `scope_handler` 补 `changelog::record`（data_scope 已在白名单）+ 自定义部门集合登记为 `sys_role_dept` 资源；涉 handler/model/diff 三处，按「≤2 文件」规则入清单 |
| 3 | **流程定义保存/发布不校验兜底人存在性**：`fallback_user_id` 填不存在的 uuid 也能 200 保存 + 200 发布，直到**发起实例**才 422/65042 | P3（待拍板） | `PUT /business/flow/version/{vid}/graph`（fallback_user_id=00000000-…-beef）→ 200；`publish` → 200；`POST /business/flow/instance` → 422/65042 | 配置错误延后暴露：业务员提交单据时才被拦，且报文让人以为是系统问题 | 保存图时校验 fallback_user_id/role_id 存在（400 + 字段级报文）；同一份 `parse_fallback_ids` 语义上移 |
| 4 | 权限闸 403 响应体是 `{"code":403}`，与 `error_code::PERMISSION_DENIED=30001` 不一致 | P4 | 用无权限账号访问任意受保护接口 | 前端若按 `code==30001` 判定「无权限」会漏判（现在只能靠 HTTP 状态码） | 二选一：中间件改下 30001，或文档明确「权限闸以 HTTP 403 为准」 |
| 5 | 身份证脱敏星号固定 4 个：18 位身份证输出 `1101****1234`（框架 `Mask::id_card` 实现），与文档示例 `1101**********1234` 不一致 | P4 | `GET /system/field-mask/preview?mask_type=2&value=110101199001011234` | 不泄露中间位数，但长度信息丢失，管理员会以为规则配错 | 统一为「星号数 = 长度 - 前后保留位数」，或改文档示例 |
| 6 | 授权变更后旧令牌在「**新增**权限方向」返回 403「无权限」而非 401/20008「请重新登录」（权限闸先于会话版本校验执行；收回方向正常 401/20008） | P4 | 给角色新增权限 → 用旧令牌访问新权限接口 → 403 | 文案误导（用户以为没授权成功），实际需重登 | 权限闸命中且 `token_version` 已变时优先返回 20008 |
| 7 | 脱敏 `preview` 不校验值形态：手机号类型喂 18 位身份证也会「脱敏」成功（输出 `110****1234`） | P4 | `preview?mask_type=1&value=110101199001011234` | 试运行结果有误导性（配置是错的也看着像对的） | 加轻量形态校验（仅提示，不阻断） |

> 无 P0 / P1 / P2。本卡未发现「事务提交后失败无补偿」「单据卡中间态」类问题。

## 五、修复记录

| commit | 文件 | 内容 |
|--------|------|------|
| `808b6910` | `backend/src/business/changelog/diff.rs` | `SYS_ROLE.fields` 补 `escalation_role_id`（+9 行注释说明为什么必须在白名单）+ 新单测 `role_escalation_change_is_recorded`（断言 diff 非空、changed_fields 恰为该列、after 值正确） |

验证：`cargo check --bin cloudpivot` RC=0；`cargo test --lib business::changelog` **52 passed / 0 failed**；145 重建 + 重启后 health=200，重跑演练时该角色的变更日志出现 `UPDATE|{escalation_role_id}`（勾稽 ⑧）。

## 六、演练脚本（可重跑）

```
scripts/verify/e2e09_common.sh        公共库（登录 / api / 断言 / 状态）
scripts/verify/e2e09_sys_a.sh         ① ② ③ ④ + 权限闸（110 断言）
scripts/verify/e2e09_sys_b.sh         ⑤ ⑥ ⑦ ⑧（125 断言）
scripts/verify/e2e09_escalation.sh    ARCH-8 降级链（65 断言）
scripts/verify/e2e09_db_evidence.sh   DB 证据 + 9 组勾稽 + 菜单不变量 + 历史回归（23 断言）
scripts/verify/e2e09_cleanup.sh       只删 E2E09 前缀（按外键顺序，不 DROP/TRUNCATE）
scripts/verify/e2e09_run_all.sh       一键：清场 → A → B → C → DB 证据
scripts/verify/e2e09_deploy_145.sh    145 ff-merge + 重建 + 重启 + health
```

在 145 上执行：`bash scripts/verify/e2e09_run_all.sh`

## 七、边界与残留

- **证据边界**：本轮是 API + DB 层演练，未做浏览器截图（145 的 nginx 静态目录为旧构建，前端页面无法代表当前代码）；页面字段契约由响应 JSON 原文（`raw/*.json`）与断言钉住。
- **数据边界**：只造 `e2e09*` / `E2E09*` 前缀数据；未 DROP / TRUNCATE、未改已应用迁移、未动既有脱敏规则与角色。
- **残留**：演练结束后执行 `e2e09_cleanup.sh`，本卡业务行全部归零（见 `raw/cleanup_after.log`）；仅 `sys_change_log` / `audit_logs` 保留审计痕迹（审计表按设计不可删）。
- **临时改库并复原**：为验证「运行期兜底」（65040 成环），演练中直接把 `sys_role.escalation_role_id` 改成环，验证后已复原为 NULL（断言「链条已复原」通过）。

## 八、结论

**全部通畅**：8 个页面 + 权限生效 + 数据权限 + 脱敏 + 互斥 + 变更日志 + ARCH-8 四条降级路径（升级 / 兜底 / 65042 / 65018）+ 两条历史回归，共 **323 条断言全绿、9 组勾稽 0 差异、菜单不变量 8 条全绿**。当场修复 1 个 P3（审批降级链配置变更丢日志），另登记 P3×2 + P4×4 待拍板。
