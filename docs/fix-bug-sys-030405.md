# 修复记录：BUG-SYS-03 / 04 / 05（后端卡 t_0f02b4fe）

来源：`docs/test-report-system.md` 第一节风险清单（测试C，卡 t_5e9f9308）。
本文件记录**判定依据、改动内容、真实执行输出**，供评审复跑。

- 卡：`t_0f02b4fe`
- 提交：`464db33`（功能）+ `d57dca2`（上游测试文件补跑 fmt）；**未 push**
  （本地 main 与 origin/main 已分叉：origin 领先 6、本分支领先 10，交叉提交由协调环节统一处理）
- 迁移号：开工时 `migrations/business/` 最大号是 `V0159`，本卡取 `V0160`；
  并行的前端卡（t_6f023cee）后来落地为 `V0170`，两者无文件冲突、无行级交集
- 数据库：仅 `cloudpivot_test`（+ 一次性隔离库副本做迁移验收，用完即删）
- 硬红线遵循：未改 `task` / `flow` 模块（文末有遗留说明）

---

## 一、BUG-SYS-03（P2）无 Redis 时「菜单增删改 / 角色授权」写入成功却回 500

### 1.1 问题复述

`menu/handler.rs`（update / delete）与 `role/handler.rs`（update / assign_menus）在业务写入
**提交之后**调 `support::online::model::force_logout_all_except()` 做缓存侧会话吊销；无缓存
（无 Redis）时该函数 `list_all_strict()` 返回 `Err`，handler 按 fail-closed 回 500
「…已保存，但会话吊销失败，请重试」。

后果：写入已完成 + 回 500 → 客户端重试 → 菜单重复删 / 授权重复执行（不幂等）；Redis 抖动期间
管理员无法维护菜单与角色。

### 1.2 方案（已落地）

**把「旧令牌失效」下沉到数据库，缓存侧降级为卫生动作。**

1. `sys_user.token_version` 是 `middleware/session_guard.rs` 逐请求比对的字段；
   只要它推进，旧 access / refresh 令牌**在库这一侧**就必然 401 —— 与 Redis 可用性无关。
2. 因此把 token_version 的推进放进**业务写入的同一个事务**：

   | 场景 | 库侧失效范围 | 函数 |
   |------|--------------|------|
   | 菜单 update / delete | 被授权了该菜单的用户（经 `sys_role_menu` 反查） | `user::model::bump_token_version_of_menu_holders_in` |
   | 角色 update | 持有该角色的用户 | `user::model::bump_token_version_of_role_holders_in` |
   | 角色 assign_menus | 持有该角色的用户（同 `replace_role_menus` 事务） | 同上 |

   > 为什么按「持有者」而不是全站：只有被授权者的令牌快照会因这次变更而**收紧**；
   > 「放宽」方向等下次登录自然生效，没有越权风险。这样既不误伤无关用户，也避免每次
   > 菜单维护全表 UPDATE `sys_user`（热表）。
   > 菜单 delete 的 bump 必须在 `DELETE FROM sys_role_menu` **之前**执行，否则反查不到持有者。
3. 缓存侧清理改调 `support::online::model::revoke_after_permission_change(scene, except)`：
   成功 `info`、失败 `warn` + `pending_compensation=true` 留痕，**不再回 500**。
   语义 =「在线列表里会短暂残留一批实际已失效的会话（它们下一次请求就 401），
   等缓存恢复或管理员在在线用户页再踢一次即可」。
4. 内置管理员 `admin` 仍被排除在推进之外（超管走通配短路，不依赖令牌里的权限快照）。

### 1.3 安全性没有丢

- 菜单/授权的「收紧」是**同事务**落库的：要么菜单写成功且旧令牌作废，要么两者都不生效；
- 这与改密路径（`auth/handler.rs` 已有先例：bump 后把 `force_logout_user` 失败降级为
  `error!` 日志）口径一致；
- 旧令牌在 `session_guard` 里 401（`登录状态已失效，请重新登录`），refresh 路径同样比对
  `token_version`。

### 1.4 用例

- `system::menu::tests::n13_permission_change_invalidates_tokens_in_db`（新增）
  - 造「菜单 → 角色 → 用户」夹具，断言：菜单 update 后**持有者** `token_version +1`、
    无关用户与 `admin` 不变；delete 后 `+2`；两次调用都必须**成功**（无 Redis 也 200）。
- `system::role::tests::r07_assign_menus_bumps_holders_token_version`（新增）
  - 断言：`assign_menus` 成功、持有者 `+1`、无关用户与 `admin` 不变。
- `system::menu::tests::n06/n07`、`system::role::tests::r01`：原来「容忍会话吊销 500」的
  helper 已收紧为「必须成功」，否则 panic —— 这三条现在就是 BUG-SYS-03 的回归防线。

---

## 二、BUG-SYS-04（P3）接口级 403 只认令牌里的权限快照

### 2.1 缺口

`vendor/nik-web/src/middleware/mod.rs:148` 的 `has_permission` = `is_super_admin() ||
permissions.contains(perm)`，**不查库**。`is_super_admin()` 只看 claims 里有没有 `*` / `*:*:*`。
于是 **JWT 密钥泄漏 = 自签一个通配令牌就是超管**（库侧兜底只覆盖数据范围/脱敏/用户角色上限）。

### 2.2 加了一条廉价护栏

`middleware/session_guard.rs` 新增第 3 条判定 `verify_wildcard_claims()`：

- 令牌**没有**通配权限 → `NotWildcard`，不额外回库（绝大多数请求走这条，成本 0）；
- 令牌声明 `*` / `*:*:*` → 回库查该用户**启用中**的角色（`user::model::load_role_codes`），
  含 `SUPER_ADMIN` → `Backed` 放行；否则 → `RoleMismatch` → **401**
  「权限声明与账号角色不符，请重新登录」；
- 查询失败 → fail-closed（500，并且不放行）。

超管请求会多一次主键点查（`sys_user_role ⋈ sys_role`），数量极少；普通用户零成本。

### 2.3 用例

- `system::authz_tests::o15_wildcard_claims_must_be_backed_by_db_role`（新增，真库）
  - 内置管理员（库里 `SUPER_ADMIN`）+ `*:*:*` → `Backed`；
  - 普通用户（持一个普通角色）伪造 `*:*:*` → `RoleMismatch`（普通角色不能背书通配）；
  - 普通权限码令牌（`system:user:list`）→ `NotWildcard`；
  - 角色被停用后 → `RoleMismatch { role_codes: [] }`（停用角色不进授权清单）。

> 边界（诚实说明）：中间件 401 响应体需要 `tower::ServiceExt::oneshot`，本仓未声明该
> dev-dependency，所以断言的是**判定函数**；HTTP 级端到端在 e2e（Playwright）层。

---

## 三、BUG-SYS-05（P3）死权限码清理

### 3.1 扫描口径（脚本见 `scan_dead_codes.py` / `judge_codes.py` / `show_refs.py`）

1. 从库里取 `sys_menu.auth_code`（`deleted_at IS NULL`）；
2. 剥离源码（`backend/src/**/*.rs`）里的行注释与块注释；
3. 对每个码统计：① 是否有 `permission = "<码>"` 路由注解；② 是否有字符串/常量引用
   （行级 `claims.has_permission(...)` / `ensure_writable(..., PERM_*)`）。

判据：`①有` → **路由级生效**；`②有但①无` → **行级生效**（真实生效，不是死码）；
`①②都没有` → **死码**（勾给角色永不生效）。

### 3.2 逐码判定（迁移前，种子菜单）

迁移前种子菜单带码 **198 行 / 180 个去重码**，其中「死码」10 条：

| 权限码 | menu_type | 判定 | 处置 |
|---|---|---|---|
| `erp-finance` / `erp-purchase` / `erp-warehouse` / `erp-master-data` | 1（目录） | 把**模块名**当权限码，永不匹配任何注解 | `auth_code` 置空（迁移内 UPDATE） |
| `business:erp:category:list` / `:detail` / `:create` | 3（按钮） | `biz_material_category` 表未建、无 `/business/erp/category` 路由；代码里只有一个「预留清单」常量 | 菜单**下架**（迁移）+ 常量清单移除 |
| `business:erp:purchase-order:detail` | 3（按钮） | 采购订单**没有 GET 详情路由**（详情随 `page` 行返回） | 菜单**下架** + 常量清单移除 |
| `system:change-log:clean` | 3（按钮） | 无手工清理入口（清理走定时任务 `task::run_handler`），全仓 0 引用 | 菜单**下架** |
| `system:preference:view` | 2（菜单） | **不是死码**：后端无接口用它，但 `frontend/src/router/index.ts` 的 `meta.permission` 用它拦前端路由（外观设置页纯 localStorage） | **保留**（迁移注释里登记为例外） |

另有 10 条「无路由注解但有字符串引用」的码，经逐条核实在代码里**真实做了行级校验**，
属于报告的误判（报告只扫了 `#[nik::permission]` 注解），**不动**：

```
business:task:assign / :member / :dependency / :worklog / :comment:delete / :attachment:delete
business:project:update / :delete      （ensure_writable(..., PERM_UPDATE / PERM_DELETE)）
business:flow:admin                    （claims.has_permission("business:flow:admin")）
system:field-mask:plain                （脱敏豁免：masking/registry.rs 的 PLAIN_PERMISSION）
```

> 说明：报告里的 `business:erp:category:*` 等 8 条被算作「22 个死码」的一部分，与本次
> 复扫一致；但 `business:task:*`、`business:project:*` 等 10 条是**行级生效**的码。

### 3.3 落地

- 迁移 `backend/migrations/business/V0173__remove_dead_permission_codes.{up,down}.sql`
  - **号段变更（P13 合并收口）**：本条迁移原为 `V0160`，与 `origin/main` 侧的
    `V0160__erp_serial_init_accounts`（批量生成「无来源入库」，ADR 文档已定稿 V0160）撞号。
    框架 `check_identity_conflicts()` 对「同版本不同名」直接拒绝启动
    （`duplicate migration version 160`），故本条**只改文件名、内容一字未动**
    （`sha256` 仍是 `c80b4a89…`，已应用此迁移的库靠 `(kind, name)` 命中记录，不会重跑、
    也不会因指纹不符而中止）。V0171/V0172 已被在途超期、文件属主隔离占用，故顺延到 V0173。
  - up：4 条目录菜单 `auth_code = ''`；5 条按钮菜单先清 `sys_role_menu` 再物理删除
    （`NOT EXISTS(子节点)` 守卫）；末尾 DO 块自检（残留 = 0、目录置空 = 4）；
  - down：原值插回 + 目录码复原（**不重建 `sys_role_menu` 绑定**，见迁移头注释）；
  - 幂等：UPDATE 带 `auth_code <> ''`，DELETE 带子节点守卫，重复执行 0 行。
- 代码清单同步（否则「源码里的权限清单」与「真正生效的码」继续分叉）：
  - `business/erp/handler.rs::CARD_PERMISSIONS` 24 → **21**（去掉物料分类 3 码）；
  - `business/erp/purchase_dto.rs::PURCHASE_ORDER_PERMISSIONS` 5 → **4**（去掉 `:detail`）；
  - 相应单测计数断言同步（`permissions_are_complete_and_unique`：32 → 29）。

### 3.4 真实执行输出

迁移验收（隔离库副本上跑 up → down → up，脚本 `verify_v0160.sh`）：

```
== 0. 建隔离库（源库 cloudpivot_test 的副本）    副本菜单数: 238
== 1. 迁移前取证
business:erp:category:create, business:erp:category:detail, business:erp:category:list,
business:erp:purchase-order:detail, system:change-log:clean
财务管理=erp-finance, 采购管理=erp-purchase, 仓库管理=erp-warehouse, 工作任务=business:task:read, 基础数据=erp-master-data
== 2. 执行 up                                  exit=0
== 3. up 后断言
   残留死码菜单行数（应为 0）: 0
   5 条菜单物理存在数（应为 0）: 0
   绑定残留（应为 0）: 0
   目录菜单（应有 4 条 auth_code 为空）: 财务管理=[], 采购管理=[], 仓库管理=[], 基础数据=[]
== 4. 幂等：再跑一次 up                        exit=0
== 5. 执行 down（回滚）                        exit=0
   5 条菜单恢复条数（应为 5）: 5
   目录菜单带码条数（应恢复 4）: 5   ← 4 条 erp-* + 工作任务 business:task:read（真实码，未动）
== 6. down 之后再次 up（往返一致性）            exit=0
   残留死码菜单行数（应为 0）: 0
   菜单总数（up 前后应只少 5 条）: 233
```

死码扫描对照（种子菜单；`judge_codes.py --seeds-only`）：

```
迁移前: 菜单行(带码) 198 / 去重码 180 / 注解码 185   →  dead: 10
迁移后: 菜单行(带码) 221 / 去重码 196 / 注解码 185   →  dead: 1
        唯一剩下的是 system:preference:view（前端路由级权限码，见 3.2 的例外登记）
注解有、菜单无: 0 → 0
```

---

## 四、回归证据

命令（与卡面验收一致）：

```bash
cd backend && cargo test --lib --no-run
su postgres -c "SYS_DB_URL='postgres://postgres@localhost/cloudpivot_test?host=%2Fvar%2Frun%2Fpostgresql' \
  <测试二进制> 'system::' --test-threads=1"
```

结果：

```
test result: ok. 94 passed; 0 failed; 0 ignored; 0 measured; 1343 filtered out; finished in 15.65s
```

（基线 89 passed / 2 ignored；本卡 +3 条新用例（n13 / r07 / o15）= 94；
n08 / n09 的 `#[ignore]` 由并行卡撤掉，本次一并跑绿。）

lint / 格式：

```
cargo fmt -- --check        → 干净（本卡为 4 个上游未格式化文件单独做了 style 提交）
cargo clippy --all-targets -- -D warnings
  → 本卡改动的文件 0 报错；仓里只剩 **3 处既有债**，全在本卡未触碰的文件里：
    src/business/task/template.rs、src/business/task/handler.rs（`manual_range_contains`，
    `task` 模块是卡片硬红线）、src/business/impexp/definition.rs（同 lint，另一个模块）。
    这 3 处在本卡开工前就存在（`git status` 显示 3 个文件均未修改），不是本次引入。
```

---

## 五、遗留与观察（不在本卡范围）

1. **`business:task:read` 挂在 `menu_type=1` 的「工作任务」目录上**（`auth_code` 非空）。
   它是真实权限码（17 处 `#[nik::permission]` 注解），目录用它做模块级门闸，语义可用；
   与「目录一律空码」的惯例不一致，登记为观察项，未改（会动 task 模块的菜单语义）。
2. **`frontend/src/constants/permissions.ts` 的 `changelogClean`** 已无对应后端接口，
   前端随 `system:change-log:clean` 菜单一起清理（属前端卡 t_6f023cee）。
3. **`cloudpivot_test` 里有用例夹具残留**（`N01-*` / `N05-*` / `N06-*` / `N07-*` / `N11-*` / `N13-*`、
   共 30 余行），来自并发/中断的测试跑（`cleanup_table` 用 `let _ =` 吞掉了错误）。
   本卡已修掉自己 N13 的清理顺序问题（外键：先 `sys_role_menu` 再 `sys_user_role` 再实体表），
   其余残留属测试 C 的用例，未删（避免打断并行的测试跑）。
   **扫描证据因此用 `--seeds-only`（按确定性 id 号段 `^[0-9]{2}000000-` 过滤）**。
4. **`cargo fmt` 会顺手格式化上游未格式化的测试文件**（本次是
   `system/dept/tests.rs`、`business/masking/tests.rs`、`business/permission/tests.rs`），
   已单独成 `style(test): 上游测试文件补跑 fmt` 提交，不混进功能提交。
