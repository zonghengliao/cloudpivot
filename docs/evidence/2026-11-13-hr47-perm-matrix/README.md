# HR-47 权限矩阵回归（30 码 × 6 角色 + SELF 只读自己薪资）

卡：`t_3389f8aa`　批次：批 5 收口　日期：2026-11-13

## 一句话

35 个权限码（30 个按钮 + 5 个页面 `*:list`）× 6 个真实身份（5 个被授权角色 + `FINANCE_DIRECTOR`
零授权对照）在**真实例 + 真库（145 空库）+ 真 HTTP** 上逐格跑完：**556 条断言通过 / 0 失败**，
矩阵 210 格授权一致；`SELF`（员工自助）只能看自己那条工资条；越权全部 403。
过程中**挖出并修掉 3 个真缺陷**（全新库员工自助零授权、员工可列全公司薪资、路线遮蔽），
另有 2 条需要拍板的已知未修已开卡 `t_be54535c`。

## 交付物

| 文件 | 作用 |
|---|---|
| `scripts/verify/hr/hr_permission_matrix.sh` | 一条命令跑完：建 145 演练库 → 起隔离实例 → 夹具 → 三层矩阵 → 汇总 → 清库 |
| `scripts/verify/hr/hr_perm_expect.py` | **期望矩阵**解析器：从 `V0387__hr_menu_perms.up.sql` 解析 35 码 × 5 角色，不手抄 |
| `backend/src/business/hr/perm_matrix_tests.rs` | 离线静态契约（12 条）：码表 / 授权矩阵 / 路由覆盖 / 种子同源 |
| `backend/migrations/seed/S__hr_employee_grants.sql` | **修缺陷 ①**：全新库给 EMPLOYEE 补 5 行人力资源授权（幂等 + 自检） |
| `backend/src/business/hr/salary_sheet_handler.rs` | **修缺陷 ②**：全量薪资列表补第二道闸 `salary:detail` |

## 三层验证（缺一层都会漏一类越权）

| 层 | 覆盖 | 判据 | 结果 |
|---|---|---|---|
| A 授权快照 | 6 身份 × 35 码 = 210 格 | `/api/system/auth/me` 的 `permissions` 逐码等于 `V0387` 种子 | 210/210 一致 |
| B 路由闸 | 6 身份 × 31 码 = 186 次真请求 | 有码 → 非 403（且路由真实存在）；无码 → **403 且报文点名「无权限」** | 186/186 |
| C 数据范围 | SELF / DEPT_AND_CHILD / ALL | `/salary/my` 只出本人；`/salary/page` 员工 403；人事经理只见本部门子树；部门主管（ALL）见全部 | 全绿 |

`ROUTE` 表挑的一律是**不改数据**的代表路由（不存在的 id ⇒ 允许时 404、拒绝时 403；写类只打
400/404 分支），所以矩阵可以反复跑而不会把演练数据搅乱。

## 6 个角色与授权（期望值 = `V0387` 解析，不是手抄）

| 角色 | HR 码数 | 说明 |
|---|---|---|
| SUPER_ADMIN | 35 | 超管本可短路，显式授予是为了角色树上能看清范围 |
| HR_MANAGER | 35 | 人事全流程 |
| FINANCE_MANAGER | 4 | 薪资查看/过账/导出，**不含红冲**（红冲只给人事） |
| DEPT_LEADER | 4 | 员工查看 + 请假详情（审批在流程中心） |
| EMPLOYEE | 4 | 页面外壳 + `salary:my` + `leave:create`，**没有** `employee:detail` |
| FINANCE_DIRECTOR | 0 | **零授权对照列**：真实角色、真没授权 ⇒ 证明 403 不是「账号不存在」 |

## 挖出并修掉的 3 个缺陷

### ① 全新库上 EMPLOYEE 一个 HR 码都没有（已修：新增可重复种子）

空库首启日志实证：

```
NOTICE: V0387：角色 EMPLOYEE 不存在 → 跳过它的授权（自助码由 S__employee_self_service_permissions 兜）
```

而那个「兜」并不存在（那个种子只授报销域 13 行）。根因：**种子（`S__`）在版本化迁移之后执行**，
EMPLOYEE 角色由种子建 ⇒ V0387 执行时它还不存在，`IF v_role IS NULL THEN RAISE NOTICE` 分支静默跳过。
后果：新客户首装即可 `business:hr:salary:my` 永远 403，员工看不了自己工资。
修法：`S__hr_employee_grants.sql`（可重复、`NOT EXISTS` 幂等、授 5 行、自检缺一行就启动失败）。
修后矩阵里 EMPLOYEE 由 `4/0` 变 `4/4`。

### ② 员工能列出全公司薪资单（已修：全量列表补第二道闸）

实测（修前）：EMPLOYEE 调 `GET /business/hr/salary/page` → **200 + 全公司薪资单**
（应发 30000 / 代扣 3000 / 实发 27000 / 人数 3 全在）。

根因是「一个码两用」：`business:hr:salary:list` 既是**薪资管理页**菜单行的 `auth_code`
（V0387 为了让「我的薪资」tab 显形把它授给了 EMPLOYEE），又是**全量列表接口**的码。
而 `biz_salary_sheet` 也没进 `SCOPED_TABLES` ⇒ 连行级裁剪都没有。
修法：`salary_sheet_handler::ensure_full_list` 要求 `salary:detail`（员工没有）→ 403 并引导走
`/salary/my`；财务经理/人事/超管不受影响（他们都有 detail）。附 1 条单测钉死三种身份。

### ③ 矩阵脚本自己差点假绿（已修：路由存在性断言）

第一版把请求拼成 `$BASE$path`（漏了 `/api`），全部命中 router 的 404 兜底；
而「允许态」的判据是「非 403」⇒ 404 被当成通过，矩阵会**悄悄全绿**。
已改为 `/api$path`，并新增硬断言：任何一格的 404 报文若是「请求的资源不存在」即判红。

## 2 条已知未修（`[KNOWN]`，卡 `t_be54535c`，不是静默跳过）

1. **`GET /business/hr/leave/my` 对员工 403**：该路径没有自己的权限规则（设计 §5.2 #25「登录即可」），
   而模板规则 `/business/hr/leave/{id}`（要 `leave:detail`）在 scoring 里命中 ⇒ 唯一该用它的角色被挡。
   实测 HR_MANAGER / DEPT_LEADER 200、EMPLOYEE 403「需要权限: business:hr:leave:detail」。
2. **`GET /business/hr/leave/page` 对员工返回 2 行**（本人 + 同事，含假别/事由）：同「页面码复用」根因，
   数据范围没裁到本人。修法建议「缺 `leave:detail` 时强制 `mine=true`」而不是 403（前端「我的请假」tab
   走的正是 `?mine=true`）。

脚本里这两条用 `known_open` 标注：修好后自动转 `[PASS]` 并提示摘标记 —— 不存在「静默跳过」。

## 原始输出

* `01_matrix_run.out` —— 全量运行日志（556 PASS / 0 FAIL / 2 KNOWN，含 6 行 403 报文示例）
* `02_matrix_table.txt` —— 210 格矩阵表（每个角色 期望/实际 码数对拍）
* `03_static_tests.out` —— `cargo test --lib business::hr::perm_matrix_tests` → **12 passed / 0 failed**
* `04_handler_tests.out` —— `cargo test --lib business::hr::salary_sheet_handler::tests` → **10 passed / 0 failed**（含新增的
  `full_list_requires_salary_detail_not_just_list`）

> 注：离线单测用独立 `CARGO_TARGET_DIR=/root/hr47-target` 跑 —— 共享工作区的
> `/opt/cloudpivot/backend/target` 当时被邻居卡的 `cargo test` 长期持锁（09:38 起），
> 等锁会无限期阻塞。全量 `cargo test --lib`（5500+ 条）未在本卡复跑：工作区里同时挂着
> 邻居未提交的改动（`flow/config_check.rs`、`impexp/builtin/hr/employee.rs` 等），
> 整棵树的红绿与本卡无关；本卡只对本模块的两组用例负责。

## 怎么复跑

```bash
# 145（演练库只在这里建；脚本自己建库、跑完自己 DROP）
ssh root@172.18.10.145
cd /opt/cloudpivot && export HR47_PGPASSWORD=$(grep '^NIK_DATABASE_PASSWORD' /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-)
HR47_DBHOST=127.0.0.1 HR47_PORT=18157 HR47_DB=hr47_drill bash scripts/verify/hr/hr_permission_matrix.sh --fresh

# 离线那一半（不需要库/实例）
cd backend && cargo test --lib business::hr::perm_matrix_tests
```

残留：脚本结束 DROP 演练库（`pg_database` 残留计数 0）、停隔离实例、删临时 profile。
