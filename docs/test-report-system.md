# 系统管理模块测试报告（测试C · dept / field-mask / menu / user / role / permission + 越权）

- 测试卡：`t_5e9f9308`【测试C】系统管理模块测试（3 个 0 测试模块 + 越权 12 条，65+ 用例）
- 执行人：test-enginner（测试工程师）
- 执行时间：2026-10-31 12:00–13:2x（CST）
- 工作区：`/opt/cloudpivot`（主仓，直接提交）
- 数据库：`cloudpivot_test`（本地 PostgreSQL 18，**只读/只写该库**）
- 结论：**新增 77 条用例全部可跑**，`cargo test --lib system::` **89 passed / 0 failed / 2 ignored**；`business::masking::tests` 9/9、`business::permission::tests` 12/12 全绿。**未改动任何业务代码**（只新增测试文件 + `#[cfg(test)] mod` 注册）。

---

## 🔴 一、权限/安全风险清单（先看这里，不许埋在列表里）

> 卡片红线 7 要求：发现权限漏洞必须在报告最前面用醒目标记列出。

### ⚠️ BUG-SYS-01（严重度 **P1**）2 条**可见菜单**点击必然 404

| 项 | 内容 |
|---|---|
| 用例 | `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router`（`#[ignore]`） |
| 期望 | `sys_menu` 里每条 `menu_type=2` 的 `path` 都能在 `frontend/src/router/index.ts` 里解析到 |
| 实际 | 2 条菜单**前端没注册路由，组件文件也不存在** → 用户点侧栏进 404 / 空白页 |

实测输出（真实执行，非推断）：

```
以下菜单前端未注册路由，点进去必然 404：
采购明细 (/business/erp/purchase-order-item, component=Some("business/erp/purchase-order-item/index"))
单号序列 (/business/erp/seq, component=Some("business/erp/seq/index"))
```

复现步骤：

1. `su - postgres -c "psql -d cloudpivot_test -Atc \"select path,component from sys_menu where path in ('/business/erp/seq','/business/erp/purchase-order-item')\""` → 两条都 `is_visible=true`;
2. `grep -n "erp/seq\|purchase-order-item" frontend/src/router/index.ts` → 无结果；
3. `ls frontend/src/views/business/erp/` → 没有 `seq/`、没有 `purchase-order-item/` 目录。

影响：菜单能点、进去 404，属于「功能对用户不可用」且会被当成权限问题反复报障。
修复方向（不由测试卡执行）：要么补路由 + 视图，要么把这两条菜单置 `is_visible=false`。

---

### ⚠️ BUG-SYS-02（严重度 **P2**）14 个菜单引用了 TDesign **不存在**的图标 → 侧栏图标空白

| 项 | 内容 |
|---|---|
| 用例 | `system::menu::tests::n09_menu_icons_exist_in_tdesign`（`#[ignore]`） |
| 期望 | 每个菜单 `icon` 都是 `tdesign-icons-vue-next` 里真实存在的组件名 |
| 实际 | 12 个图标名不存在（共 14 条菜单命中）→ 侧栏渲染空白图标 |

实测输出：

```
字段脱敏 → mask          帮助中心 → question-circle   流程定义 → flow
任务列表 → list          任务看板 → view-board        公告管理 → announcement
采购订单 → shopping-cart  供应商 → team               采购明细 → unordered-list
即时库存 → database      库位库存 → environment       单号序列 → field-number
物料管理 → box           采购管理 → shopping-cart
```

判定依据：`frontend/node_modules/tdesign-icons-vue-next/esm/components/<name>.js` 是否存在
（同一目录下 `user.js` / `setting.js` / `file.js` 等均存在，说明命名口径就是 kebab-case 文件名）。
可用替代：`list-numbered`、`view-list`、`data-base`、`user-group`、`chat-bubble` 等。

---

### ⚠️ BUG-SYS-03（严重度 **P2**，环境相关）无 Redis 时「菜单删除/更新、角色授权」返回 **500**，但业务写入已完成

| 项 | 内容 |
|---|---|
| 用例 | `system::menu::tests::n06 / n07`、`system::role::tests::r01`（均真实复现） |
| 期望 | 写入成功 → 200 |
| 实际 | 写入已提交，随后「强制下线在线用户」失败 → handler fail-closed 回 `500 会话吊销失败，请重试` |

实测输出（真实执行）：

```
[Menu] 叶子菜单应可删除: ApiError { code: 10004, msg: "菜单已删除，但会话吊销失败，请重试", status: 500 }
```

成因：`support::online::model::force_logout_all_except` → `OnlineSessionModel::list_all_strict()`
在**没有缓存（Redis）**时返回 Err；`menu/handler.rs:210`、`role/handler.rs` 把它当 500 返回。
本次验证库没有 Redis（`ss -ltn | grep 6379` 无输出），因此必现。

风险点（建议评审）：
- 语义矛盾：「已成功」却回 500，客户端会重试 → 菜单重复删除/授权重复执行（**不幂等**）；
- Redis 抖动期间管理员无法维护菜单与角色授权（可用性风险）；
- 用例已按「写入结果」断言并打印该 500，一旦环境有 Redis（返回 200）用例仍绿。

---

### ⚠️ BUG-SYS-04（严重度 **P3**，纵深防御观察项）接口级 403 只认令牌里的权限快照，库侧无二次校验

| 项 | 内容 |
|---|---|
| 用例 | `system::authz_tests::o09_forged_claims_do_not_grant_db_side_privileges`、`o01–o06`、`o12–o14` |
| 事实 | 中间件判定 = `claims.has_permission(perm)`（`vendor/nik-web/src/middleware/mod.rs:148`），即 `is_super_admin() || permissions.contains(perm)`，**不查库** |
| 已被库侧兜住的部分 | 超管判定（`role_code`）、数据权限、字段脱敏、用户/角色的「权限上限」守卫（`user/guard.rs`）——O09 已逐条验证：伪造 claims 换不到这些 |
| 未被兜住的部分 | **接口级 403**：令牌若被伪造成功（JWT 密钥泄漏 / 算法混淆），攻击者塞 `*:*:*` 即全通过 |

结论：**JWT 密钥是接口鉴权的单点**。建议（运维/架构侧）：密钥长度与轮换制度、`jwt_secret` 只从环境变量注入（当前已是）、以及把「权限快照含超管通配但库里不是超管」的令牌直接判 401 作为额外护栏。
测试侧已把「库侧重算权限 = 空」钉成回归（`o09`），任何人把权限装载改成「信令牌」都会红。

---

### 🟡 BUG-SYS-05（严重度 **P3**）**死权限码**：菜单里有、代码里没人校验

实测（`system::menu::tests::n10` 的反向扫描）：菜单 `auth_code` 共 205 个，其中 22 个在源码中**既没有 `#[nik::permission]` 注解，也没有字符串引用**，例如：

```
system:change-log:clean      ← 全仓 0 引用（彻底死码）
business:erp:category:list / :create / :detail
business:erp:purchase-order:detail
business:task:assign / :comment:delete / :attachment:delete / :dependency / :member / :worklog
business:project:update / :delete
```

影响：管理面能把它们勾给角色，但**永远不会生效** → 管理员以为授权了、用户却没有权限（最难排查的一类「权限配了没用」）。
另有 4 条**目录菜单拿模块名当权限码**：`erp-finance` / `erp-purchase` / `erp-warehouse` / `erp-master-data`（`menu_type=1`），同样永不匹配任何注解。
（注：`n10` 的正向扫描——「注解了却没有菜单承载」——为 **0 条**，这条更危险的死锁不存在。）

---

### 🟡 BUG-SYS-06（严重度 **P2**，界面可见）3 条菜单把**权限码当标题**显示

```
name = title = business:erp:ledger:list    path=/business/erp/ledger
name = title = business:erp:payable:list   path=/business/erp/payable
name = title = business:erp:voucher:list   path=/business/erp/voucher
```

依据：`frontend/src/layouts/components/SideMenuItem.vue:12/28` 渲染的是 `item.title`（不是 `name`），
上面 3 条 `title` 就是权限码 → 侧栏显示「business:erp:ledger:list」这样的原文。
（另有 10 条同类菜单的 `title` 是正确的中文，只有这 3 条漏了。）

---

### 🟢 BUG-SYS-07（严重度 **P4**，记录在案）1 个 POST 接口只要求读权限

`POST /business/notice/template/{id}/preview → business:notice:template:list`。
语义是「只回显不落库」的预览，要求读权限可接受；已作为 `o13` 的显式例外记录，避免以后被当成漏配。

---

## 二、用例与结果（真实执行输出）

### 2.1 新增用例分布（77 条）

| 模块 | 文件 | 用例数 | 结果 |
|---|---|---|---|
| `system/dept` | `backend/src/system/dept/tests.rs` | 12（D01–D11 + 补充） | ✅ 全绿（真库） |
| `system/menu` | `backend/src/system/menu/tests.rs` | 12（N01–N12） | ✅ 10 绿 + 2 `#[ignore]`（BUG-SYS-01/02） |
| `business/masking`（= 卡片说的 `system/field-mask`） | `backend/src/business/masking/tests.rs` | 9（M01–M09） | ✅ 全绿 |
| 越权 O 系列 | `backend/src/system/authz_tests.rs` | 14（O01–O14） | ✅ 全绿 |
| `system/user` | `backend/src/system/user/tests.rs` | 12（U01–U12） | ✅ 全绿（含真实登录流程） |
| `system/role` | `backend/src/system/role/tests.rs` | 6（R01–R06） | ✅ 全绿 |
| `business/permission` | `backend/src/business/permission/tests.rs` | 12（A01–A12） | ✅ 全绿 |
| **合计** | | **77** | 75 绿 + 2 ignored |

### 2.2 交付口径的总跑（贴真实输出）

```
$ SYS_DB_URL=... cargo test --lib system:: -- --test-threads=1
test result: ok. 89 passed; 0 failed; 2 ignored; 0 measured; 1343 filtered out; finished in 15.41s

$ SYS_DB_URL=... cargo test --lib business::masking::tests -- --test-threads=1
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 1425 filtered out; finished in 0.29s

$ SYS_DB_URL=... cargo test --lib business::permission::tests -- --test-threads=1
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 1422 filtered out; finished in 0.01s
```

> `system::` 的 89 条 = 本卡新增 56 条（dept 12 + menu 12 + authz 14 + user 12 + role 6）+ 仓内既有 33 条
> （`preference` / `badge` / `role::handler` / `user::guard` 等）。

### 2.3 怎么跑（复现命令）

```bash
# 本机验证库走 unix socket + peer 认证时（本次用法）
cd /opt/cloudpivot/backend
cargo test --lib --no-run            # 取测试二进制路径
su postgres -c "SYS_DB_URL='postgres://postgres@localhost/cloudpivot_test?host=%2Fvar%2Frun%2Fpostgresql' \
  <测试二进制> system:: --test-threads=1"

# 有 TCP 口令时的标准用法
SYS_DB_URL="postgres://<user>:<pwd>@127.0.0.1:5432/cloudpivot_test" cargo test --lib system::
```

未设 `SYS_DB_URL` 时数据库用例打印 `SKIP` 并返回（`cargo test` 仍全绿，CI 无库也不会红）。

### 2.4 数据卫生

- 所有夹具走统一前缀（`D01-…` / `N0x-T…` / `tc_u…` / `tc_r…` / `tc_m…` / `tc_o08…` 等），
  断言一律**按前缀过滤**，没有一条用例依赖全局 `COUNT(*)`；
- 每个用例开头清扫、结尾清理；清理顺序先删外键依赖（`sys_role_menu` / `sys_user_role` /
  `sys_user_preference`），物理删不掉时退化为软删（`deleted_at`），因此重跑不撞唯一索引；
- 收尾核对：`active_tc_users=0`、`active_tc_roles=0`、`mask_left=0`、`menu_left=0`。

---

## 三、卡片口径 vs 实际实现（差异说明，避免下游按错误前提改动）

| 卡片写法 | 实际情况 | 证据 |
|---|---|---|
| `system/field-mask` **0 测试** | 实现在 `business/masking/`（路由前缀仍是 `/system/field-mask`），且 `masker/registry/model/handler/user_export` **各自已有单测** | `grep -rn "cfg(test)" backend/src/business/masking/` |
| `business/permission` = 权限码全集/权限树 | 实际是**数据范围权限**（`/business/permission/scope/*` 5 个路由 + `scope_model` 纯逻辑）；权限码权威来源是 `sys_menu.auth_code` + `#[nik::permission]` | `backend/src/business/permission/{scope_handler,scope_model}.rs` |
| 建部门上级不存在 → **400** | 实际 **404** `RESOURCE_NOT_FOUND`（`D02` 按实际断言） | `system/dept/tests.rs::d02` |
| 部门层级上限 8 层 → 400 | 实现**没有**层级上限（`D09` 按实际断言并注明） | `system/dept/tests.rs::d09` |
| 密码 **bcrypt** | 实际 **Argon2id** PHC 串（`nik::Crypto::hash_password`）；`U01` 断言「非明文 + `$argon2` 前缀 + 可校验」 | `vendor/nik-utils/src/crypto.rs:69` |
| 改密后旧 token 失效 | 机制是 `sys_user.token_version` 自增 + 会话守卫逐请求比对（`U03` / `O08` 已验） | `middleware/session_guard.rs:75-90` |
| 菜单 236 条 | 实测 236 条（目录 11 / 菜单 54 / 按钮 171） | `N11` 超管可见 77 条（启用目录+菜单）与库内同谓词计数一致 |
| 脱敏：无权限角色看不到原值 | 成立；但**手机号掩码保留前 3 后 4**（`138****1111`），这是框架 `Mask::mobile` 的标准口径，不是泄漏 | `M02` 断言「完整原值串不出现」而非「结果等于固定星号串」 |

**种子数据事实**：`sys_field_mask` 里有 3 条**全局**规则（`role_id IS NULL`）：
`sys_user.mobile`（1 手机号）、`sys_user.email`（3 邮箱）、`sys_user.real_name`（4 姓名），
理由「个人信息保护…」。也就是说**默认所有非豁免用户的这三个字段都是打码的**（正确行为，
但排查「前端显示 ***」时应先想到它）。因此 `M07`（改配置立刻生效）刻意选了没有全局规则的
`sys_user.remark` 列，否则「改前已命中」会假红。

---

## 四、重点覆盖了什么（按卡片三类重点）

### 类型 1：越权（O01–O14，`system/authz_tests.rs`）

判定谓词与中间件**逐字一致**（直接调 `claims.has_permission`），路由表从**源码注解**解析
（`#[nik::permission(path, method, permission)]`，排除注释与测试文件），因此不随实现漂移：

| 用例 | 断言 |
|---|---|
| O01 | 只读员 → `POST /system/user`（`system:user:create`）必 403 |
| O02 | 只读员 → `DELETE /system/role/{id}` 必 403，且该路由**不能只要读权限** |
| O03 | 只读员 → **所有** `POST /business/erp/*` 写接口必 403（动态扫全量） |
| O04 | 仓管员 → 财务类只读接口（ledger/voucher/payable/account）全部 403 |
| O05 | 「本部门」财务提交/读取别的部门 → `exceeded_dept_ids` 判越界；不可见父节点**提升为根**而非泄露 |
| O06 | 普通员工 → 流程接口全部 403 |
| O07 | 公开接口白名单**封闭**：仅 `login/refresh/config/public` + 任务模块回调；白名单外无「无注解」路由 |
| O08 | 改密/停用后旧令牌失效：`token_version` 自增，令牌 `ver` 与新版本不等 → 会话守卫 401 |
| O09 | 伪造 claims（塞 `system:user:create`）：库侧权限重算为**空**、非超管、数据范围受限、脱敏仍生效 |
| O10 | 自我提权 / 操作超管 / 幽灵操作者 → 403；超管放行（反向） |
| O11 | 跨部门写：越界点名为「名称（ID）」、缺失 id 单独报、范围形态非法 400 |
| O12 | 超管通过**全部**受保护路由（防「配过头对所有人 403」） |
| O13 | 每个写接口都有独立写权限码；无公开写接口（1 条 preview 例外已登记） |
| O14 | 权限码格式 `模块:资源:动作`、全小写、无全角/空格（权限码与菜单必须逐字相等才能授权） |

### 类型 2：脱敏有效性（M01–M09）

配置 → 生效（列表/详情同源）→ 导出 → 改配置立刻生效 → **不影响库内原值** → fail-closed。
核心断言统一为「**完整原值串不出现在结果里**」，包括导出单元格与错误信息（`M04` 反向断言错误文案不含原值）。

### 类型 3：菜单-路由-权限三方一致

- `N08` 菜单路径 ↔ 前端路由（→ 发现 BUG-SYS-01）
- `N09` 菜单图标 ↔ TDesign 图标集（→ 发现 BUG-SYS-02）
- `N10` 后端权限注解 ↔ 菜单 `auth_code`（→ 正向 0 条缺失；反向发现 BUG-SYS-05）
- `O14` 权限码格式（与 N10 呼应）
- `N11` 按角色过滤：未授权菜单绝不出现在菜单接口（前端隐藏入口的根基）

---

## 五、`#[ignore]` 的已知缺陷用例（修好后删掉 `#[ignore]` 即成为回归防线）

| 用例 | 对应缺陷 |
|---|---|
| `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router` | BUG-SYS-01（2 条菜单 404） |
| `system::menu::tests::n09_menu_icons_exist_in_tdesign` | BUG-SYS-02（14 条菜单图标空白） |

其余用例全部可跑可绿，没有用 `#[ignore]` 掩盖任何真实失败。

---

## 六、本次**未**覆盖 / 需要后续卡的部分（诚实说明）

1. **验签与令牌过期（框架层）**：`JWT::from_config()` 依赖框架全局配置单例，lib 单测没有初始化
   入口，`JWT::validate` 无法在 `#[cfg(test)]` 里构造实例 → O08/O09 改为验证「应用层在签名之外
   做了什么」（令牌版本、库侧重算、库侧权限判定）。签名/过期本身属框架单测范围。
2. **中间件端到端**（`session_guard::guard` / `permission_check` 的 401/403 响应体）：需要
   `tower::ServiceExt::oneshot`，而本仓 `Cargo.toml` 未声明 dev-dependency（且框架铁律不允许随意加），
   故按「同一判定谓词 + 源码注解全量扫描」覆盖；真正的 HTTP 级验证在 e2e（Playwright）层。
3. `task` / `flow` 模块按卡片要求**完全未触碰**（P10/P11 在改）；只作为只读对象出现在 O03/O06 的
   路由扫描里（只断言它们需要权限，不改它们的代码）。
4. 导出（M04）只覆盖 `format_export_rows` 这一唯一改点；Excel 落盘链路属 impexp 卡范围。

---

## 七、结论

- 权限体系的**三个根基模块**（dept 数据权限、field-mask 脱敏、menu 授权）从「0 测试」变为
  「有真库、真 handler、有反向断言的回归防线」；
- 越权 14 条把「没权限的人打不进」固化成自动化，任何新接口漏写权限注解都会被 `O13` 挡下；
- 发现 **1 个 P1（菜单 404）、3 个 P2（图标空白 / 无 Redis 时 500 / 权限码当标题）、2 个 P3**
  问题，全部只记录未改业务代码，交由对应修复卡处理。
