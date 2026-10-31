# 死权限码逐个定性（BUG-SYS-05 / FIX-06，2026-11-01）

> **来源**：`docs/test-report-system.md` 第一节风险清单 BUG-SYS-05（P3）：`sys_menu.auth_code`
> 共 205 个，其中 22 个在源码里既没有 `#[nik::permission]` 注解、也没有字符串引用 → 配了也没用。
> **本卡结论**：**当前死码 0 个**（唯一例外是 §3 登记在册的 C 类 `system:preference:view`）。
> 22 条复扫得 **20 条**（口径见 §4），其中 **9 条系统已下线（B 类，已在 V0173 落地）**、
> **10 条是报告误判（行级校验真实生效，不是死码）**、**1 条是纯前端权限点（C 类，保留 + TODO）**。
> **未发现「接口存在但完全无权限校验」的安全漏洞**（逐条核查见 §1）。

扫描口径（三处一致）：`backend/src/**/*.rs`，**剥离注释**（`//` `///` `//!` `/* */`）、
**保留字符串字面量**、**排除测试文件**。`①注解` = 中间件闸；`②行级` = handler 内
`claims.has_permission(...)`；`①②都没有` = 死码。

---

## 0. 一句话交代：为什么没有安全漏洞，也没有新迁移

- **没有 A 类（该管没管）**：372 条路由里 356 条带 `#[nik::permission]`，剩下 16 条逐条核查过，
  每一条都有替代把关人（匿名 / 自助 / 双身份），见 §1。**0 条裸奔接口**。
- **没有可删的 B 类残留**：5 条废弃菜单 + 4 条「模块名当权限码」的目录菜单，已在
  `V0173__remove_dead_permission_codes`（commit `cd8b054`）里删掉/置空。本次复扫确认库里
  已经查不到（§5 A4），**再写一条删菜单的迁移只会是空操作**。
- **本次真正的交付**是「不留死角的防线」：报告做的是**一次性人工扫描**，扫完没有留下任何
  自动化守卫，所以这类缺陷可以再长出来。本次补了 5 条常驻不变量用例（§6）+ 验收脚本。

---

## 1. 【安全漏洞专项】有接口但完全无权限校验 —— 0 条

> 红线②要求：发现即单列在文档最前面。**本次核查结论是 0 条**，下面给出覆盖面与逐条把关人，
> 便于复核（而不是「没查所以没有」）。

**覆盖面**：扫描 `backend/src/**/*.rs` 的全部 372 条路由声明（`#[nik::{get,post,put,delete,patch}]`），
其中 356 条带 `#[nik::permission(permission = "...")]`，**16 条无注解** —— 逐条如下：

| # | 方法 路径 | 谁在把权限闸 | 证据 |
|---|---|---|---|
| 1 | POST `/system/auth/login` | 匿名接口，凭账号口令自证 | `system/auth/handler.rs` login |
| 2 | POST `/system/auth/refresh` | 匿名接口，凭 refresh 自证（轮换后旧 refresh 立刻失效） | `system/auth/handler.rs` refresh |
| 3 | POST `/system/auth/logout` | 自助：只吊销令牌里的 `jti`（本人） | `system/auth/handler.rs` logout |
| 4 | GET `/system/auth/me` | 自助：本人画像 + 权限快照 | handler 只用 `claims.sub` |
| 5 | GET `/system/auth/menus` | 自助：本人可见菜单（本身就是授权结果） | 按 `claims` 过滤，无跨人入口 |
| 6 | PUT `/system/auth/profile` | 自助：只写 `claims.sub` 指向的本人资料 | 无 `{user_id}` 入参 |
| 7 | PUT `/system/auth/password` | 自助：只改本人口令（旧口令在 handler 内校验） | 无 `{user_id}` 入参 |
| 8 | GET `/system/auth/logs` | 自助：本人登录日志（`claims.username` + 数据范围双过滤） | `auth/handler.rs::login_logs` |
| 9 | GET `/system/auth/preference` | 自助：本人偏好（**DTO 无 `user_id` 字段**，SQL 恒带 `WHERE user_id = ?`） | `system/preference/handler.rs` 头注释三层越权防线 |
| 10 | PUT `/system/auth/preference` | 同上（upsert 本人） | 同上 |
| 11 | GET `/system/badge/counts` | 自助：本人待办计数 | `badge/handler.rs::counts` |
| 12 | GET `/support/config/public` | 匿名接口，只出 `public.*` 参数（登录页标题 / Logo） | `support/config/handler.rs::public` |
| 13 | GET `/business/task/{id}` | **双身份**：`#[nik::no_auth]` + handler 内 `ensure_task_read_permission`（`business:task:read`） | `task/handler.rs:2129` |
| 14 | POST `/business/task/{id}/comment` | 双身份：AI 任务令牌；人类侧走 `resolve_reader → principal()`（读权限 + 可见性） | `task/handler.rs:2065/2129` |
| 15 | POST `/business/task/{id}/attachment` | 双身份：同上 | 同上 |
| 16 | PATCH `/business/task/{id}/ai-progress` | **只接受任务令牌**：人类会话直接 `403 + 63304`，无跨任务入口 | `task/handler.rs::ai_progress` |

这 16 条现在被 `NO_PERMISSION_ROUTES` 白名单钉住（用例 N16）：**新增裸接口不登记就红**，
白名单过期（路径改了 / 补了注解）也红。

另：`#[nik::no_auth]` 白名单共 7 条（login / refresh / config-public / task 详情 / 评论 /
附件 / ai-progress），与上表 1、2、12、13–16 对应，无额外豁免。

> **诚实说明**：`GET /business/task/{id}` 这类 `no_auth` 端点是最容易被写成越权的地方（中间件
> 整段跳过权限层）。本次逐字读了 `TaskReader::principal()`：人类分支第一句就是
> `ensure_task_read_permission(claims)?`，AI 分支以任务负责人身份解析且令牌边界锁死单任务 ——
> 成立。若日后有人在这条路径上再加分支，N16 不会自动发现（它只看「有没有注解/白名单」），
> 所以 §7 把「no_auth 端点改动必须同时补行级判定」写成约定。

---

## 2. 现状复扫（V0173 之后，当前真实清单）

```
隔离库（本次全新迁移，零夹具）：cloudpivot_fix06_verify
迁移数量：122 个
菜单行（带码）：221 条 / 去重码：196 个
  ├─ 185 个有 #[nik::permission] 注解（中间件闸）
  ├─  10 个只有行级校验（handler 内 has_permission / ensure_writable，真实生效）
  └─   1 个后端无校验点（纯前端路由权限点，C 类，登记在册）
死码：0 个                                    ← 卡面验收 ①
路由：372 条 = 356 条注解闸 + 16 条白名单例外（§1）
注解有、菜单无：0 条                          ← n10 正向不变量，未退化
```

与报告的差异：报告实测「205 个 auth_code / 22 个死码」是 **V0173 之前**的库。本次把
`V0173.down.sql` 回放到隔离库复现了那个状态（恰好 **205 个去重码**，与报告一致），再扫一遍得
**20 条**非注解码（§4 解释这 2 条差异）。

---

## 3. 逐条定性（20 条，口径：注解缺失 ≠ 死码）

### 3.1 B 类 · 废弃功能 → 从菜单删（9 条，**均已在 V0173 落地**）

| # | 权限码 | 对应功能 | 定性 | 处置 | 状态 |
|---|---|---|---|---|---|
| 1 | `system:change-log:clean` | 手工清理过期分区（清理已改定时任务 `task::run_handler`，无手工入口） | B 废弃 | V0173 软删前物理删除 + 清 `sys_role_menu` | ✅ 已落地 |
| 2 | `business:erp:category:list` | 物料分类列表（`biz_material_category` 表未建、无 `/business/erp/category` 路由） | B 废弃 | 同上 | ✅ 已落地 |
| 3 | `business:erp:category:detail` | 物料分类详情（同上） | B 废弃 | 同上 | ✅ 已落地 |
| 4 | `business:erp:category:create` | 新建物料分类（同上） | B 废弃 | 同上 | ✅ 已落地 |
| 5 | `business:erp:purchase-order:detail` | 采购订单详情（**没有 GET 详情路由**，详情随 `page` 行返回） | B 废弃 | 同上 | ✅ 已落地 |
| 6 | `erp-purchase` | 采购管理**目录**把模块名当权限码（`menu_type=1`） | B 废弃 | V0173 `auth_code = ''` | ✅ 已落地 |
| 7 | `erp-warehouse` | 仓库管理目录（同上） | B 废弃 | 同上 | ✅ 已落地 |
| 8 | `erp-finance` | 财务管理目录（同上） | B 废弃 | 同上 | ✅ 已落地 |
| 9 | `erp-master-data` | 基础数据目录（同上） | B 废弃 | 同上 | ✅ 已落地 |

> 为什么没有写 `V0174__remove_dead_permission_menus`：上面 9 条已经在 V0173 处理完，隔离库上
> 复查均为 0 行（§5 A4）。**空迁移不写**（写了就等于在迁移表里留一条什么也不做的记录）。

### 3.2 报告误判 · 行级校验真实生效，**不是死码**（10 条，不动）

报告把它们算进「22 个死码」，是因为只扫了 `#[nik::permission]` 注解，**没覆盖 handler 内的
行级判定**。逐条核到具体判定语句：

| # | 权限码 | 对应功能 | 定性 | 处置 | 证据（判定语句） |
|---|---|---|---|---|---|
| 10 | `business:flow:admin` | 退回「任意节点 / 终止」（管理动作） | 行级生效 | 不动 | `flow/handler.rs:1401`、`:1579` `claims.has_permission("business:flow:admin")` |
| 11 | `business:project:update` | 编辑项目 / 维护成员（管理员侧） | 行级生效 | 不动 | `project/handler.rs:312` `ensure_writable(..., PERM_UPDATE, ...)` |
| 12 | `business:project:delete` | 删除项目（管理员侧） | 行级生效 | 不动 | `project/handler.rs:414` `ensure_writable(..., PERM_DELETE, ...)` |
| 13 | `business:task:assign` | 分派 / 转派任务 | 行级生效 | 不动 | `task/handler.rs:2680`、`:3710` `may_assign(role, claims.has_permission(dto::PERM_ASSIGN))` |
| 14 | `business:task:member` | 管理任务协作人 | 行级生效 | 不动 | `task/handler.rs:3924`、`:4052` `has_permission(dto::PERM_MEMBER)` |
| 15 | `business:task:dependency` | 管理任务依赖 | 行级生效 | 不动 | `task/handler.rs:1713`、`:1878` `has_permission(dto::PERM_DEPENDENCY)` |
| 16 | `business:task:worklog` | 登记 / 删除工时（非本人） | 行级生效 | 不动 | `task/collab.rs:1076`、`:1227`、`:1271` `has_permission(PERM_WORKLOG)` |
| 17 | `business:task:comment:delete` | 删他人评论 | 行级生效 | 不动 | `task/collab.rs:764`、`:832` `has_permission(PERM_COMMENT_DELETE)` |
| 18 | `business:task:attachment:delete` | 删他人附件 | 行级生效 | 不动 | `task/collab.rs:881`、`:941` `has_permission(PERM_ATTACHMENT_DELETE)` |
| 19 | `system:field-mask:plain` | 字段脱敏豁免（看明文，每次落审计） | 行级生效 | 不动 | `masking/registry.rs:344` `claims.has_permission(PLAIN_PERMISSION)` |

### 3.3 C 类 · 预留 / 纯前端 → 保留 + TODO（1 条）

| # | 权限码 | 对应功能 | 定性 | 处置 | 状态 |
|---|---|---|---|---|---|
| 20 | `system:preference:view` | 外观设置（纯 localStorage，**后端无接口**） | C 预留（前端路由权限点） | 保留；登记到用例 `ROUTE_LEVEL_ONLY` 并写 TODO | ✅ 已在册 |

`frontend/src/router/index.ts:118` 用 `meta.permission = 'system:preference:view'` 拦前端路由，
所以它对「界面可见性」是真生效的；对后端数据无意义（该页没有后端接口）。
**TODO**：日后若给外观设置补后端接口，必须给接口加 `#[nik::permission(permission = "system:preference:view")]`
并删除 `ROUTE_LEVEL_ONLY` 里的这条例外（用例 N18 会红，逼你清理台账）。

---

## 4. 与报告的 2 条差异（22 vs 20）

- 报告给的是**举例清单**（13 条列举 + 4 条目录），没有完整 22 条清单，无法逐条对账；
- 本卡用可复现口径重算：把 `V0173.down.sql` 回放到隔离库，得到**恰好 205 个去重码**
  （与报告的「205 个」一致，说明复现的正是报告当时的库状态），再跑同口径扫描得 **20 条**；
- 因此差异 2 条只能是「报告当时的扫描脚本口径更宽」（例如把某些常量清单也算作注解缺失），
  不是「本卡漏扫」。**结论按 20 条走**，且 20 条已逐条定性（§3），无一悬空。

---

## 5. 真实执行输出（5 条命令）

### ① 反向扫描：死码 0 个（字面量口径，脚本 A3）

```bash
$ bash scripts/verify/fix06_dead_permissions.sh
=== A3 反向扫描（菜单 auth_code → 后端源码是否有人问） ===
菜单权限码：196 个去重码 / 221 条菜单行
[PASS] 权限码基数正常（196 ≥ 150，扫描不是空转）
[PASS] 死码 0 个（例外仅 system:preference:view，已在台账登记）
```

### ② 权限闸实证：无权限角色 403 / 管理员 200（脚本 A5）

```bash
=== A5 权限闸实证：同一接口，管理员 200 / 无权限用户 403 ===
[PASS] 管理员 GET /system/menu/tree（system:menu:list）（200）
[PASS] 无令牌 GET /system/menu/tree（401）
[PASS] 无权限用户登录成功
[PASS] 无权限用户 GET /system/menu/tree → 403（注解闸生效）（403）
[PASS] 403 文案点明缺哪个权限码
[PASS] 无权限用户 GET /system/user/page → 403（403）
[PASS] 对照：自助接口 /system/auth/me → 200（不需要权限码）（200）
```

### ③ 下架的菜单：库里查不到、侧栏不出现（脚本 A4 + A7）

```bash
[PASS] 权限码形态合规（≥2 个冒号，无「模块名当权限码」）（0）
[PASS] 已下架的死码 business:erp:category:list 不在菜单里（0）
[PASS] 已下架的死码 business:erp:category:detail 不在菜单里（0）
[PASS] 已下架的死码 business:erp:category:create 不在菜单里（0）
[PASS] 已下架的死码 business:erp:purchase-order:detail 不在菜单里（0）
[PASS] 已下架的死码 system:change-log:clean 不在菜单里（0）
[PASS] sys_role_menu 无孤儿绑定（指向已删菜单）（0）
...
[PASS] 侧栏里没有已下架权限码 system:change-log:clean
[PASS] 无权限用户侧栏为空（菜单按授权过滤）
```

### ④ 不变量套件：n10 正向 + N14–N18 反向全绿（脚本 A8）

```bash
=== A8 单测：n10（正向）+ N14–N18（反向） ===
[PASS] n10 + N14–N18 全绿（6 条）
```

单独跑（带 `--nocapture` 看每条的量化结论）：

```bash
$ SYS_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_fix06" \
    cargo test --lib system::menu::permission_audit_tests -- --nocapture --test-threads=1
running 5 tests
[N14] 196 个菜单权限码全部有源码落地 ✓（185 条走 #[nik::permission] 注解闸，10 条走行级校验）
[N15] 221 条菜单行权限码形态合规 ✓
[N16] 372 条路由：356 条有权限注解 + 16 条白名单例外 ✓
[N17] 扫描器自检通过（死码 / 裸路由都能扫出来）✓
[N18] 例外台账 1 条、白名单 16 条，全部有理由且仍然成立 ✓
test result: ok. 5 passed; 0 failed
```

### ⑤ ERP 不变量 32/32 不退（卡面验收 ④）

```bash
$ NIK_DATABASE_NAME=cloudpivot_fix06 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=*** \
    python3 scripts/verify/erp_invariants.py
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

---

## 6. 防线（本次新增，防复发）

`backend/src/system/menu/permission_audit_tests.rs`（`#[cfg(test)]`，5 条用例）：

| 用例 | 断言 | 为什么值钱 |
|---|---|---|
| **N14** | 每个 `sys_menu.auth_code` 在**剥注释后的后端源码**里必须有落地 | 报告只做了「注解 → 菜单」，反向没人守；这是死码的唯一自动化防线 |
| **N15** | 权限码必须是 `{模块}:{资源}:{动作}`（≥2 冒号） | 让 `erp-finance` 这类「模块名当权限码」在 CI 就红 |
| **N16** | 每条路由要么有注解、要么在 `NO_PERMISSION_ROUTES` 白名单且写明把关人 | 直接盯「裸奔接口」；白名单过期也红 |
| **N17** | 扫描器自检：人造死码 / 裸路由必须被扫出来 | 防止扫描逻辑退化成永远绿（测试的测试） |
| **N18** | 例外台账「活着」：登记的码仍未被引用、仍在菜单、理由非空 | 例外不会烂在代码里 |

**N14 真能红**（反证，已在隔离库上执行）：插入一条假菜单码后

```bash
$ psql ... -c "insert into sys_menu (...,auth_code) values (...,'fix06:planted:dead')"
$ cargo test --lib system::menu::permission_audit_tests::n14
thread '...n14_menu_auth_codes_have_source_action_point' panicked at ...:
以下菜单权限码在后端源码里 0 落地（勾给角色永不生效，BUG-SYS-05）：
fix06:planted:dead
test result: FAILED. 0 passed; 1 failed        ← 删掉假数据后恢复 5/5 ok
```

---

## 7. 遗留与约定

1. `system:preference:view`（C 类）保留，TODO 已在用例 `ROUTE_LEVEL_ONLY` 里写明触发条件。
2. **约定：动 `#[nik::no_auth]` 白名单端点时必须同时补 handler 内的行级判定**（N16 只能保证
   「登记过」，保不了「handler 里真的判了」——任务模块的做法见 `TaskReader::principal()`）。
3. **约定：新增权限码三件套** = ① `sys_menu` 种子行 ② 接口 `#[nik::permission]` 注解（或行级判定）
   ③ 角色授权。三件缺一，N14/N16/n10 任一必红。
4. 本次未改任何运行时业务代码（只加测试 + 脚本 + 文档），故不需要数据修复或回滚方案。
