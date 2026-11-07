# FIX-1 新环境开箱即用 · 145 实测证据（2026-11-07）

卡：`t_2756e93c`（FIX-1 422 引导指派审批人 + 首次启动向导 + 演示账号开关）
机器：`172.18.10.145`（**只碰测试机，从未连 144 生产**）
二进制：`/opt/cloudpivot/backend/target/debug/cloudpivot-fix1`（commit `c030759` 之后又合入
`ApiError::appended` 的修复，最终 commit 见卡面完成记录）

## 0. 环境隔离（为什么这么搭）

145 上正在服役的是 `cloudpivot-test`（`127.0.0.1:8033` / 库 `cloudpivot_dev`）。
本次验收用的 5 个**全新库**与 4 个隔离进程（8034–8038），各自独立 env 文件
（`/root/fix1/env*`，Redis 分库 `/3`–`/7`），验完即 `pkill`，全程未动 8033 与
`cloudpivot_dev`（收尾复核：`health_8033=200` / `systemctl is-active cloudpivot-test = active`）。

| 库 | 端口 | `NIK_SEED_DEMO_USERS` | 用途 |
|----|------|------------------------|------|
| `cloudpivot_fix1` | 8034 | **true** | 验收②：启动即 9 个演示账号 → 销售链直接跑通；重启验幂等 |
| `cloudpivot_fix5` | 8038 | false | 验收①④：只有 admin → 422 带 data → 指派 → 重提 → 审批通过；普通用户 403 |
| `cloudpivot_fix3` | 8036 | false → true | 验收③：库已有历史 `demo_lead` 时不重复建 |

## ① 全新库只有 admin：422 带结构化 data → 指派 → 重提成功 → 审批走通

```
===== 2. GET /system/setup/check =====
http=200 all_ready=false demo_users_enabled=false
missing=SALES_SUPERVISOR,PURCHASE_MANAGER,FINANCE_MANAGER,DEPT_LEADER,WAREHOUSE_MANAGER,GENERAL_MANAGER

===== 3. 建销售订单并提交（应 422 / 65018，且带结构化 data）=====
create SO http=200 so=SO-20261107-000001 status=DRAFT
submit http=422 code=65018
msg=节点「销售主管审批」的审批人全部是制单人 / 发起人本人，内控（职责分离）不允许他们审批自己的单据，
    本节点没有一个可用审批人：超级管理员（内控：单据制单人不可审批自己制单的单据（职责分离），已跳过并留痕）。
    请先把该节点的审批岗位（角色成员 / 指定审批人）指派给非制单人、非发起人；或改由他人提交本单。
----- 结构化 data -----
{"assignee_type":"ROLE","candidate_count":1,
 "excluded":["超级管理员：内控：单据制单人不可审批自己制单的单据（职责分离），已跳过并留痕"],
 "fix_hint":"角色「销售主管」解析出的审批人全部是制单人 / 发起人本人：请再指派一名**其他**成员，或改由他人提交本单",
 "flow_instance_id":"01a1b63a-e6d1-7753-a2e7-43c5b74a967f","member_count":0,
 "node_id":"01a1b63a-e6d7-72b7-ac79-6c586f274a50","node_key":"n_sales","node_name":"销售主管审批",
 "reason":"SELF_ONLY","reason_text":"该节点解析到的审批人都是制单人 / 发起人本人（内控不允许自审）",
 "role_code":"SALES_SUPERVISOR","role_id":"20000000-0000-4000-8000-0000000f0007","role_name":"销售主管",
 "usable_member_count":0}
断言：data 四要素齐全 ✔
reason=SELF_ONLY role=SALES_SUPERVISOR member_count=0

===== 4. 建一名非制单人 + /system/setup/assign 指派 SALES_SUPERVISOR =====
create user http=200 {"code":0,"msg":"ok"}
assign http=200 {"code":0,"code_":"SALES_SUPERVISOR","added":["01a1b63a-…"],"skipped":[],"member_count":1}

===== 4b. 重复指派（幂等）=====
assign(2nd) http=200 {"added":[],"skipped":["01a1b63a-…"],"member_count":1}

===== 5. 重新提交审批 ====="
submit http=200 code=0 status=SUBMITTED instance=01a1b63a-e849-76e0-be45-5b694428c7e4

===== 6. 被指派的人登录 → 待办 → 审批 =====
todo http=200 total=1 assignee=FIX1 销售主管
approve http=200 {"code":0,"msg":"ok"}
SO status=APPROVED

===== 7. 普通用户（无 system:setup:manage）=====
assign as ordinary user http=403 code=403 msg=无权限访问，需要权限: system:setup:manage
check as ordinary user http=403 code=403
```

DB 断言（`cloudpivot_fix5`）：

```
 so_no              | status   | total_amount
 SO-20261107-000001 | APPROVED |    1000.0000

 node_key | status   | assignee_name
 n_sales  | APPROVED | FIX1 销售主管

 flow_code  | status
 so_approve | APPROVED

 role_code        | members
 SALES_SUPERVISOR |       1
（其余 5 个关键角色仍为 0 —— 向导会继续提示）
```

> **踩到并修掉的一个真 bug**：第一版只给 `assignee::resolve` / `engine` 挂了载荷，
> 实机 422 却是 `"data": null` —— 因为业务侧 13 处「在流程拒绝报文后补一句怎么解决」
> 都是 `ApiError::new(rejected.status, rejected.code, format!("{}{hint}", rejected.msg))`
> **重建**错误，把 `data` 丢了。修法：framework 加 `ApiError::appended(hint)`
> （保留 code/status/internal_detail/data），13 处全部改成 `rejected.appended(hint)`。

## ② 全新库 + `NIK_SEED_DEMO_USERS=true`：启动即有演示账号，链路直接跑通

```
===== 0. 启动日志 =====
演示账号种子完成（幂等：已有账号不重复建） created=9 existed=0 bindings_added=9 dept_created=true
created_users=["demo_sales","demo_sales_mgr","demo_purchase","demo_purchase_mgr",
               "demo_finance","demo_finance_mgr","demo_wh","demo_lead","demo_gm"]

===== 2. setup/check：6 个关键角色全就绪 =====
http=200 all_ready=true demo_users_enabled=true
SALES_SUPERVISOR 1/1  PURCHASE_MANAGER 2/2  FINANCE_MANAGER 2/2
DEPT_LEADER 1/1      WAREHOUSE_MANAGER 1/1 GENERAL_MANAGER 1/1

===== 5. demo_sales 建单 → 提交（不再 65018）=====
create SO http=200 so=SO-20261107-000001 status=DRAFT
submit http=200 code=0 status=SUBMITTED instance=01a1b63b-49cf-…
（对照组：①里同样的操作在无演示账号的库上是 422/65018）

===== 6. demo_sales_mgr 待办 → 审批 =====
todo http=200 total=1 assignee=演示·销售主管
approve http=200 code=0
SO status=APPROVED
```

DB（`cloudpivot_fix1`）：10 个账号（admin + 9 演示）、6 个关键角色成员 1/2/2/1/1/1、
`biz_flow_instance so_approve = APPROVED`、`biz_flow_task n_sales = APPROVED(演示·销售主管)`。

## ③ 已有 `demo_lead` 的环境（模拟 145）再开种子：不重复建、不覆盖

```
===== 2. 手工建「历史遗留」的 demo_lead（口令 Legacy@2026 / 真名 老演练主管，无角色）=====
create user http=200 {"username":"demo_lead","real_name":"老演练主管",...}

===== 4. 打开 NIK_SEED_DEMO_USERS 重启 =====
演示账号种子完成（幂等：已有账号不重复建） created=8 existed=1 bindings_added=9 dept_created=true
created_users=["demo_sales","demo_sales_mgr","demo_purchase","demo_purchase_mgr",
               "demo_finance","demo_finance_mgr","demo_wh","demo_gm"]
existed_users=["demo_lead"]

===== 5. 老账号没被覆盖 =====
login demo_lead with ORIGINAL password -> token_len=3088（>0 = 口令没被覆盖）
me: {"username":"demo_lead","real_name":"老演练主管","must_change_password":false}
roles now: ["20000000-0000-4000-8000-0000000f0001"]   ← 只补了缺的 DEPT_LEADER 绑定

===== 6. 用户总数 =====
{"total":10,"users":["demo_gm","demo_wh","demo_finance_mgr","demo_finance","demo_purchase_mgr",
                     "demo_purchase","demo_sales_mgr","demo_sales","demo_lead","admin"]}
```

同一实例**再重启一次**（演示账号已全存在）：`created=0 existed=9 bindings_added=0 dept_created=false`。

## ④ 普通用户不能自助指派

见 ① 第 7 步：`POST /system/setup/assign` → **403**，「无权限访问，需要权限: system:setup:manage」；
`GET /system/setup/check` → **403**。
前端对应的表现：`65018` 拦截器发现当前用户没有 `system:setup:manage` ⇒ 不弹可操作框，
只提示「请联系管理员为角色「销售主管」指派审批人（该节点解析到的审批人都是制单人…）」。

## ⑤ 迁移幂等（V0322）

`V0322__setup_wizard_permissions` 用固定 uuid + `ON CONFLICT (id) DO UPDATE`，
并在末尾 `DO $$` 自检两个权限码是否齐全。三个全新库各自跑过一次全量迁移 + 重跑
（重启实例时 migration runner 会再校验一次指纹），无一次报错、`sys_menu` 两行不增不减：

```
SELECT auth_code FROM sys_menu WHERE auth_code LIKE 'system:setup:%'
system:setup:read
system:setup:manage
```

## ⑥ 未做到的一项（如实说明）

`pnpm typecheck && pnpm build` 在本工作区**无法整体绿**：同一工作区里另一张卡
（`V0321 price_inquiry`）的**未提交代码**有 3 个 TS 错误，与本次改动无关：

```
src/api/business/erp/price-inquiry.ts(363,80): error TS2345 ...
src/views/business/erp/price-inquiry/index.vue(159,23): error TS2551 ...
src/views/business/erp/price-inquiry/index.vue(721,3): error TS6133 ...
```

本次改动自身的验证：
- `vue-tsc --noEmit` 的报错清单里**没有本卡任何文件**（`utils/flow-assignee-guard.ts`、
  `components/FlowAssigneeDialog.vue`、`components/SetupWizardDialog.vue`、
  `api/system/setup-api.ts`、`api/request.ts`、`layouts/BasicLayout.vue`）；
- `vite build`（跳过 typecheck 的那一半）**成功**（`EXIT=0`），说明新组件能正常打包；
- `tsx --test tests/unit/flow-assignee-guard.test.ts` → **8/8 pass**。
