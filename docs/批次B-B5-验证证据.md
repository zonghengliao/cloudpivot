# 批次 B · B5 验证证据 —— 数据权限全量接入（门禁清零 19+18 处）

- 任务卡：`t_ac7f6c91`【批次B·B5】数据权限全量接入：门禁清零（19+18 处）
- 依据：`docs/批次B-技术架构.md` §2.4 四道防漏闸、§2.5 登记表清单、**附录 C 注入点接入表（逐行打勾见 §5）**
- 代码基线：`c46ce41`（B3 交付后的 main）+ 本卡提交
- 验证机：`172.18.10.144`，DB 只用 `cloudpivot_test`（profile=test，127.0.0.1:8025）

---

## 1. 门禁清零前后对比（`bash scripts/check.sh`）

### 1.1 开工前（`c46ce41` 树，同口径实测）

```
=== 闸① ===
backend/src/support/config/model.rs:447      backend/src/support/dict/model.rs:175 / :297
backend/src/support/file/model.rs:153        backend/src/support/job/model.rs:256 / :314
backend/src/support/log/model.rs:333 / :448  backend/src/system/role/model.rs:150 / :216
backend/src/business/storage/model.rs:220 / :465 / :991
backend/src/business/impexp/model.rs:455 / :549 / :913
backend/src/business/notice/model.rs:556 / :670 / :830        ← 共 19 处
=== 闸② ===
FAIL: support/dashboard/model.rs sys_user          FAIL: support/dashboard/model.rs sys_dept
FAIL: support/dashboard/model.rs operation_logs    FAIL: support/dashboard/model.rs auth_logs
FAIL: system/auth/model.rs sys_user                FAIL: system/role/model.rs sys_user
FAIL: business/notice/fanout.rs sys_user           FAIL: business/notice/fanout.rs sys_dept
FAIL: business/impexp/cleanup.rs sys_import_task   FAIL: business/impexp/cleanup.rs sys_export_task
FAIL: business/impexp/model.rs sys_import_task     FAIL: business/impexp/model.rs sys_export_task
FAIL: business/notice/cleanup.rs sys_notice        FAIL: business/notice/model.rs sys_notice
FAIL: support/file/model.rs sys_file               FAIL: business/storage/cleanup.rs sys_file
FAIL: business/storage/model.rs sys_file           FAIL: support/log/model.rs operation_logs   ← 共 18 处
```

（与任务卡记录的 19 / 18 完全一致。）

### 1.2 收尾（本卡 HEAD）

```
$ bash scripts/check.sh
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 后端：cargo check
==> 前端：pnpm typecheck
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
    白名单：business/shared/scope.rs（+ 注释行不计）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> 全部检查通过
$ echo $?
0
```

| 门禁 | 开工前 | 收尾 | 说明 |
|---|---|---|---|
| 闸① `paginate_with_params`（白名单外） | **19** | **0** | 19 处全部改走 `scope::paginate`（10 处）或 `scope::paginate_unscoped`（9 处，§2.5 判为 ❌ 的全局元数据表）+ 3 处 `paginate_unscoped`（子表/自身已收口）|
| 闸② 登记表查询无 `scope::` / `SCOPE-EXEMPT` | **18** | **0** | 8 个 file×table 组合由真实接入消掉，其余由**带理由的** `SCOPE-EXEMPT` 豁免（12 处注释，9 个文件）|
| `cargo fmt --check` | 0 | 0 | 只对改动文件跑 `rustfmt`（仓内既有格式债务不碰）|
| `cargo clippy --all-targets -D warnings` | 0 | 0 | 无新增告警 |
| `cargo test` | 305 passed | **309 passed / 0 failed** | +4 个新用例（见 §6），既有 305 个一个不少 |
| `pnpm typecheck` | 0 | 0 | 前端未改动 |

---

## 2. 闸①：19 处逐处处理（不许一刀切）

**判定口径**（架构 §2.5「哪些表需要数据权限」）：
- ✅ 登记表 → 必须真实注入；
- ❌ 全局元数据 / 全局配置 → 不注入，但**仍走唯一入口**（`paginate_unscoped` + 显式 `ScopeTarget::unscoped("表名")`），并把「不注入」写成代码里的声明，而不是漏接。

| # | 位置 | 主体表 | 处理 | 依据 |
|---|---|---|---|---|
| 1 | `system/role/model.rs::page_role_users` | `sys_user` | `scope::paginate(SYS_USER)`，`{FILTER}` 置于 `ur.role_id = ?` 之前 | 附录 C #7。「角色详情页签」是人员列表的第二个入口，不收口 = 绕过 `page_users` |
| 2 | `support/file/model.rs::page_files` | `sys_file` | `scope::paginate(SYS_FILE)`，别名 `f` | 附录 C #11 |
| 3 | `support/log/model.rs::page_operation_logs` | `operation_logs` | `scope::paginate(OPERATION_LOGS)`，别名 `l` | 附录 C #9 |
| 4 | `support/log/model.rs::page_json_logs` | `audit_logs` / `auth_logs` / `security_logs` / `password_change_logs` | `scope::paginate(表锚点.aliased("t"))`，锚点由 `JsonLogTable::scope_target()` 映射 | 附录 C #10 |
| 5 | `business/storage/model.rs::page_storage_files` | `sys_file` | `scope::paginate(SYS_FILE)` | 附录 C #11（第二个文件列表入口）|
| 6 | `business/impexp/model.rs::page_import_tasks` | `sys_import_task` | `scope::paginate(SYS_IMPORT_TASK)` | 附录 C #12。此前 `only_mine=false` 可看全站 |
| 7 | `business/impexp/model.rs::page_export_tasks` | `sys_export_task` | `scope::paginate(SYS_EXPORT_TASK)` | 附录 C #13 |
| 8 | `business/notice/model.rs::page_notices` | `sys_notice` | `scope::paginate(SYS_NOTICE)` | 附录 C #14。此前该查询**无任何范围约束** |
| 9 | `business/impexp/model.rs::page_task_errors` | `sys_import_task_error` | `paginate_unscoped` + `SCOPE-EXEMPT`：子表按 `task_id` 收口，父任务可见性由带 scope 的 `load_task` 判定 | 附录 C #21 同源（子表无归属列）|
| 10 | `business/notice/model.rs::page_my_notices` | `sys_notice_receiver` | `paginate_unscoped` + `SCOPE-EXEMPT`：已按 `user_id` 硬绑定（令牌 sub） | 附录 C #15 |
| 11 | `support/config/model.rs::page_configs` | `sys_config` | `paginate_unscoped(sys_config)` | 附录 C #18（全局配置类 ❌）|
| 12 | `support/dict/model.rs::page_dict_types` | `sys_dict_type` | `paginate_unscoped` | 附录 C #18 |
| 13 | `support/dict/model.rs::page_dict_items` | `sys_dict_item` | `paginate_unscoped` | 附录 C #18 |
| 14 | `support/job/model.rs::page_jobs` | `sys_job` | `paginate_unscoped` | 附录 C #18 |
| 15 | `support/job/model.rs::page_job_logs` | `sys_job_log` | `paginate_unscoped` | 附录 C #18（运维数据）|
| 16 | `system/role/model.rs::page_roles` | `sys_role` | `paginate_unscoped(sys_role)` | 附录 C #8（全局元数据 ❌）|
| 17 | `business/storage/model.rs::page_configs` | `sys_storage_config` | `paginate_unscoped` | 附录 C #17（全局基础设施 ❌）|
| 18 | `business/storage/model.rs::page_policies` | `sys_storage_policy` | `paginate_unscoped` | 附录 C #17 |
| 19 | `business/notice/model.rs::page_templates` | `sys_notice_template` | `paginate_unscoped` | 附录 C #16（全局资产 ❌）|

### 2.1 为什么要有 `paginate_unscoped` 而不是「让调用方自己分页」

闸① 的语义是「分页只有一种写法」。若给 ❌ 的表留一条「自己调 `paginate_with_params`」的口子，
闸① 就退化成「谁都能绕过」。因此：

```rust
// scope.rs（B5 新增）
pub const fn unscoped(table: &'static str) -> Self   // 只有它能造出 scoped=false 的锚点
pub async fn paginate_unscoped(target, base_sql, base_params, order_by, page, page_size)
```

守卫写在函数体第一行：`target.is_scoped()` 为真 → 直接 `DbError::InvalidValue`
（错误文案要求改走 `scope::paginate(…, &principal)`）。配套单测 3 个（§6），
其中 `paginate_unscoped_rejects_scoped_target` 断言「登记表传进来必报错」。

> 换言之：**要么表被登记进 `SCOPED_TABLES`（走 `paginate` + principal），要么调用方必须
> 在源码里显式写 `ScopeTarget::unscoped("表名")`** —— 没有第三种可能，「忘了接」不再存在。

---

## 3. 闸②：18 处逐处处理

### 3.1 由真实接入消掉（8 处 file×table）

`system/role/model.rs`（sys_user）、`business/impexp/model.rs`（sys_import_task / sys_export_task）、
`business/notice/model.rs`（sys_notice）、`support/file/model.rs`（sys_file）、
`business/storage/model.rs`（sys_file）、`support/log/model.rs`（operation_logs）—— 见 §2 的 #1–#8。

### 3.2 `SCOPE-EXEMPT`（10 处 file×table / 12 处注释 / 9 个文件）

| 位置 | 表 | EXEMPT 理由（代码里逐处写明，此处摘要）|
|---|---|---|
| `support/dashboard/model.rs::load_metrics` | sys_user / sys_dept / operation_logs / auth_logs | 工作台是**全局聚合计数**（标量，无行级明细），接口只对持 `support:dashboard:list` 的角色开放；注入范围会让「全公司用户总数」变成「本部门人数」，大盘语义失真（与 `business/monitor` 同口径）|
| `system/auth/model.rs::invalidate_role_holders` | sys_user（实为 `sys_user_role`） | 门禁按子串匹配把 `FROM sys_user_role` 算成 `sys_user`；用途是按角色反查需失效缓存的主体，属系统内部维护，注入范围会**漏失效**（越权）|
| `system/auth/model.rs::load_roles_uncached` | sys_user（同上） | 装载**当前登录者本人**的角色/权限快照 —— 「算权限」的查询不能再被「权限」过滤，否则自我循环 |
| `business/notice/fanout.rs::NoticeTarget::user_set_sql` | sys_user / sys_dept | 扇出**必须**能枚举收件人；注入**发送者**的范围会让「该收到的人收不到」= 业务正确性故障（静默少数据）。目标合法性由 `NoticeTarget::validate` + `business:notice:create` 控制 |
| `business/impexp/cleanup.rs::reclaim_stale` | sys_export_task | 后台定时任务（无请求者身份），按 `heartbeat_at` 全局回收僵尸任务；注入范围会让别人的僵尸任务永久 `RUNNING` |
| `business/impexp/cleanup.rs::expire_artifacts` | sys_export_task | 同上，按 `expire_at` 全局回收产物 |
| `business/impexp/cleanup.rs::purge_import_files` | sys_import_task | 同上，按保留期全局回收上传文件 |
| `business/notice/cleanup.rs::run_once` | sys_notice | 定时清理按 `expire_at` 全局硬删过期通知，与某个用户能看到哪些数据无关 |
| `business/storage/cleanup.rs::run_once` | sys_file | 孤儿文件回收（软删超保留期且无同哈希有效记录），后台任务无请求者身份；注入 `uploaded_by` 会让别人的孤儿文件永久占盘 |

### 3.3 例外说明：`sys_file` 的部门形态

Q3 决策为「**下一批**（P1）」——`sys_file` 没有 `dept_id`，部门形态需要 join `sys_user`。
本批的处理是**先把归属列收口**：

- `ScopeTarget::SYS_FILE` = `(table=sys_file, alias=f, dept_col=None, owner_col=uploaded_by)`；
- 部门类范围（DEPT / DEPT_AND_CHILD / CUSTOM）在 `filter_for` 里 **fail-closed 降级为「仅本人」**
  并置 `is_degraded()`（B2 已实现，B5 首次真正生效）；
- 因此「有 `support:file:list` 就能翻全站文件」这个口子在本批关掉，代价是部门类账号只看到自己的文件
  （比放开安全，且不是静默多数据）；join `sys_user` 取部门留给下一批。

实测见 §4 验收 ③：`b5_lead`（本部门及以下）只看到自己上传的 1 个文件，admin 看到 13 个。

---

## 4. 真机抽查（profile=test / DB `cloudpivot_test` / 127.0.0.1:8025）

实例：`/root/b5run`（独立 `CARGO_TARGET_DIR=/var/tmp/b5-target`，避免与并行卡抢二进制），
`NIK_CACHE_TYPE=redis`（否则 epoch/范围缓存都在进程内，「范围已生效」的前提不成立）。
启动自检输出：

```
INFO cloudpivot::business::shared::guard: 数据权限自检通过：11 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在
INFO cloudpivot::shared::startup_guard: 启动守卫通过：缓存后端满足吊销耐久性要求 profile=test cache_type=redis
```

夹具（`/root/b5run/fixtures.sql`，只写 `cloudpivot_test`）：3 个角色（`B5_DEPT`=2 / `B5_DEPT_CHILD`=3 /
`B5_SELF`=4）+ 3 个账号（`b5_mgr` 研发部、`b5_lead` 研发部、`b5_self` 后端组）+ 3 条公告（不同发送人）/
4 条导入任务 / 4 个文件（不同归属人）。部门树：总部 → 研发部 → 后端组；总部 → 销售部。

### 4.1 登录（token 指纹，不贴明文）

```
登录 admin    token=eyJ0eXAiOiJK…eqBXcP3U (len=461)
登录 b5_mgr   token=eyJ0eXAiOiJK…e-Qu8DrU (len=3521)
登录 b5_lead  token=eyJ0eXAiOiJK…WjaBbkeA (len=3531)
登录 b5_self  token=eyJ0eXAiOiJK…cDfbvKvU (len=3523)
```

### 4.2 验收 ①：本部门账号查 `notice/mine`（收口方式：按接收人 `user_id`）

```
① b5_mgr(本部门, data_scope=2)  GET /api/business/notice/mine    http=200 total=2 rows=2
   收到的公告号： ['B5-N2', 'B5-N3']
```

- `B5-N2` 是 `b5_lead` 发给 `b5_mgr` 的；`B5-N3` 是 admin 发给 `b5_mgr` 的；
- `B5-N1`（`b5_mgr` 自己发出、收件人是 `b5_lead`）**没有**出现在「我的通知」里 ——
  说明 `page_my_notices` 的收口维度是「接收行 `user_id`」而不是「发送人/部门」，符合附录 C #15 的
  `SCOPE-EXEMPT`（已按 user_id 硬绑定）。
- 对照：同一账号查管理视角 `GET /api/business/notice/page` → `total=1`（只有自己**发出**的 `B5-N1`），
  即 `sys_notice` 的 sender_id 收口生效（部门类范围降级为「仅本人」）。

### 4.3 验收 ②：仅本人账号查 `impexp/import/page`（`only_mine=false`）

```
② b5_self(仅本人, data_scope=4) GET /api/business/impexp/import/page?only_mine=false  http=200 total=1 rows=1
   任务号： ['B5-IMP-SELF']
   admin(全部数据) 同 URL：                                        http=200 total=8 rows=8
   任务号： ['B5-IMP-ADMIN','B5-IMP-MGR','B5-IMP-LEAD','B5-IMP-SELF', 'IMP20261028-*' ×4]
```

- `only_mine=false`（**故意关掉**旧的前端自过滤）时，`b5_self` 仍然只看到自己创建的 1 条；
- 同 URL 下 admin 看到 8 条 —— 证明过滤来自 `sys_import_task.create_by` 的 scope 片段，
  不是 handler 里的 `only_mine` 分支。

### 4.4 验收 ③：本部门及以下账号查 `storage/file/page`

```
③ b5_lead(本部门及以下, data_scope=3) GET /api/business/storage/file/page  http=200 total=1 rows=1
   文件名： ['b5_lead.txt']
   admin(全部数据)：                                                        http=200 total=13 rows=13
```

- `sys_file` 在 §Q3 被判为下一批（P1），本批按 §3.3 fail-closed 降级为「仅本人」：
  `b5_lead` 只看到自己上传的文件，不是「含下级部门」，也**不是**「全部文件」；
- 升级路径明确：下一批在 `ScopeTarget::SYS_FILE` 上补 `LEFT JOIN sys_user`/`dept_id` 即可
  （`filter_for` 的降级分支会自动退出）。

### 4.5 附加证据：有部门列的表确实按部门/子树过滤

```
b5_lead(本部门及以下) GET /api/system/user/page  http=200 total=14   ← 研发部 + 后端组 + 自身部门链
b5_mgr (本部门)       GET /api/system/user/page  http=200 total=6    ['b5_mgr','b5_lead',a5_u07,a5_u08,a5_u09,a5_u10]
```

### 4.6 附加证据：详情/写操作也收口（附录 C #4 + 单条 filter_for）

```
b5_lead GET /api/business/impexp/import/<admin 的导入任务 id>   http=404（"导入任务不存在"，不泄露存在性）
b5_mgr  GET /api/support/log/page                              http=200 total=0（部门类范围降级为 user_id=本人）
```

### 4.7 回归（非数据权限接口，行为未变）

```
GET /health            http=200 {"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"test",...}}
GET /api/system/menu/tree (admin) http=200 顶层菜单 9 个
```

---

## 5. 附录 C 逐行打勾

| # | 行 | 状态 | 落地位置 |
|---|---|---|---|
| 1 | `page_users` | ✅（B3） | `system/user/model.rs` |
| 2 | `find_user_roles_for_users` | ✅ 豁免 | 入参已受 #1 约束 |
| 3 | `user_holds_super_admin_role` | ✅ 豁免 | 判自己 |
| 4 | `batch_update_dept`（写操作） | ✅ **本卡** | `system/user/model.rs::count_visible_users` + handler 校验（403/61002）|
| 5 | `find_dept_list` | ✅（B3） | `system/dept/model.rs` |
| 6 | `find_dept_subtree_ids` | ✅ 豁免 | 被 scope 解析自身调用 |
| 7 | `page_role_users` | ✅ **本卡** | §2 #1 |
| 8 | `page_roles` | ✅ 豁免（unscoped） | §2 #16 |
| 9 | 操作日志分页 | ✅ **本卡** | §2 #3 |
| 10 | 审计/认证/安全/密码变更日志 | ✅ **本卡** | §2 #4（`password_change_logs` 一并登记进 `SCOPED_TABLES`，10→11 张）|
| 11 | 文件分页 | ✅ **本卡** | §2 #2 / #5 / §3.3 |
| 12 | 导入任务分页 | ✅ **本卡** | §2 #6 |
| 13 | 导出任务分页 | ✅ **本卡** | §2 #7 |
| 14 | `page_notices` | ✅ **本卡** | §2 #8 |
| 15 | `page_my_notices` | ✅ 豁免 | §2 #10 |
| 16 | `page_templates` | ✅ 豁免 | §2 #19 |
| 17 | 存储配置/策略分页 | ✅ 豁免 | §2 #17 / #18 |
| 18 | `support/dict|config|job` 各分页 | ✅ 豁免 | §2 #11–#15 |
| 19/20 | 同步导出 / 异步导出取数 | ⏭ B6（`masking` + `ExportDefinition.fetch`） | 本卡不改 |
| 21 | `impexp/cleanup.rs` | ✅ 豁免 | §3.2 |
| 22 | `notice/cleanup.rs` | ✅ 豁免 | §3.2 |
| 23 | `storage/cleanup.rs` | ✅ 豁免 | §3.2 |
| 24 | `dashboard/model.rs` | ✅ 豁免 | §3.2 |
| 25 | `notice/fanout.rs` | ✅ 豁免 | §3.2 |
| 26 | `find_user_display_names` | ✅ 豁免 | 入参是已授权的 user_id 列表 |
| 27 | `auth/model.rs`（角色/权限/菜单装载） | ✅ 豁免 | §3.2 |

**本卡额外补的两处单条读取**（附录 C 未单列，但属于「单条/详情用 `filter_for`」的要求）：

- `business/impexp/model.rs::find_import_task` / `find_export_task` 已改为必须传 `&ScopedPrincipal`，
  片段进 `WHERE`；`handler::load_task` / `load_export_task` 统一收口 → 越权读他人任务返回 **404**。
- 后台 worker 与集成测试无请求者身份，走显式命名的 `find_export_task_unscoped`（文档写明
  「任何面向用户的 handler 都不得调用」）。

---

## 6. 新增/修改的单测

| 用例 | 断言 |
|---|---|
| `scope::tests::registered_tables_are_all_scoped` | 登记清单里每一张都必须 `is_scoped()`（防用 `unscoped` 混进登记表）|
| `scope::tests::unscoped_target_always_allows` | `unscoped` 锚点在五种范围下都恒真、0 绑定参数；对照组 `SYS_FILE + SELF` 生成 `AND f.uploaded_by = ?` |
| `scope::tests::paginate_unscoped_rejects_scoped_target` | 登记表传给 `paginate_unscoped` → `DbError::InvalidValue`（闸① 不可绕过）|
| `scope::tests::inject_filter_keeps_token_boundaries` | `{FILTER}` 三种推送姿势都得到单空格分隔（**真机踩过的 `NULLAND` 粘连回归**）|
| `guard::tests::preflight_uses_the_registry` | 登记表 10 → 11 张（补登 `password_change_logs`）|

`cargo test`：**309 passed / 0 failed**（B3 基线 305 + 4 个新用例；既有用例一个没少，
`tests/impexp_worker.rs` 的 6 处调用改名为 `find_export_task_unscoped`，断言不变）。

### 6.1 真机验收抓到的一个真 bug（已修 + 已加回归）

第一轮真机抽查 `import/page` 与 `notice/page` 直接 500。日志里打出的 count SQL 是：

```sql
... WHERE t.deleted_at IS NULLAND 1 = 1 ...
```

根因：`sql.push_str(scope::FILTER_MARKER)` 推送的标记**前面没有空格**，而 B2 的
`inject_filter` 用 `replacen(marker, filter.sql().trim())` 替换 → 生成了 `NULLAND`。
修法不是逐个调用方补空格，而是把 `inject_filter` 改成「标记两侧各补一个空格、并裁掉原有空白」，
任何推送姿势都安全；并补了 `inject_filter_keeps_token_boundaries` 单测守住这条边界。
这条也说明：**门禁绿 ≠ 功能对**，闸①/闸② 都过了 SQL 仍可能是坏的。

---

## 7. 复现命令

```bash
# ① 门禁（自包含）
cd /opt/cloudpivot && bash scripts/check.sh

# ② 单测
cd /opt/cloudpivot/backend && cargo test

# ③ 真机抽查（只写 cloudpivot_test）
bash /root/b5run/start.sh            # 起 127.0.0.1:8025，NIK_CACHE_TYPE=redis
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_test -f /root/b5run/fixtures.sql
python3 /root/b5run/verify_b5.py     # 登录 4 个账号 + 3 条验收 + 2 条回归
```

---

## 8. 同步状态

见 `summary` 与提交信息：功能提交 + 本证据提交后，
`git rev-parse HEAD` == `origin/main` == `git --git-dir=/opt/git/cloudpivot.git rev-parse main`。
