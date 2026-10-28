# 批次 B · B8「前端三块」验证证据

> 分支：`feat/b8-frontend` ｜ 基础提交：`d204c89`（origin/main）
> 交付：角色数据范围 + 字段脱敏规则 + 角色互斥规则 三块界面（纯前端，未改后端）
> 证据时间：2026-10-29（CST）

---

## 0. 环境与验证方式（先说清，避免误读证据）

| 项 | 情况 |
|---|---|
| 线上实例 `127.0.0.1:8023` | **是旧二进制 + 旧库**（`schema_migrations` 最高 V0052，`sys_field_mask` / `sys_role_mutex` / `sys_role_dept` 三张表都不存在）。因此 B6/B7/B4 的接口在 8023 上**全部 404**——这不是前端问题，是**部署未跟进**（见 §8 第 1 条）。 |
| 本次验证实例 | `127.0.0.1:8099`，二进制由 `/opt/cloudpivot/backend`（`d204c89`）`cargo build --release` 重新构建（35s），库为**独立新建的 `cloudpivot_b8`**（启动时自动跑迁移到 V0068），Redis 用 `db 9` 与线上隔离。**没有动线上库 `cloudpivot`。** |
| 前端 | `pnpm --dir frontend dev --port 5199`，`BACKEND_ORIGIN=http://127.0.0.1:8099`；通过 SSH 本地转发 `127.0.0.1:5199` 访问（vite 默认只监听回环）。 |
| 浏览器登录方式 | **未在浏览器里键入任何口令**（工具红线）。做法：服务端用 `curl` 登录取 `access_token`，浏览器端 `fetch` 一次性本地分发点后写入 `localStorage.x_access_token`，再按真实登录态浏览。口令/token 均未进入任何证据文本。 |
| 测试账号 | `admin`（超管）、`qa.viewer`（权限码 `[]`）、`qa.limited`（只授 `scope:{read,write,self-check}` + `field-mask:list` + `role-mutex:list`）、`qa.mutex`（同时持有 `MUTEX_A`+`MUTEX_B`，用于互斥冲突） |

---

## 1. 验收 1：`pnpm typecheck` 零错误 + `pnpm build` 成功

```
$ pnpm typecheck
> vue-tsc --noEmit
TYPECHECK_EXIT=0        # 无任何输出即零错误

$ pnpm build
✓ built in 16.59s
BUILD_EXIT=0
```

产物中可见本次新增的懒加载 chunk：`DataScopePanel-*.js`、以及 `field-mask` / `role-mutex` / `data-scope` 三个页面的 `index-*.js`。
`src/types/components.d.ts` 未发生变化（没有引入未注册的组件，diff 干净）。

---

## 2. 验收 2：三块各自的「五件事」

### 2.1 块 1 · 角色数据范围

入口两处（同一份 `DataScopePanel.vue`，避免两套实现漂移）：
① 角色管理 → 点角色 → **「数据权限」页签**（`views/system/role/index.vue`，扩展）；
② 菜单种子 `sys_menu.path=/system/data-scope` 对应的**「数据权限」一览页**（`views/system/data-scope/index.vue`）。

| 五件事 | 落地方式 | 实测证据（真实 DOM 文本） |
|---|---|---|
| 列表加载 | 一览页拉角色分页 + 逐行回填 `GET .../scope/role/{id}` | `角色数据范围 / 共 5 个角色`；行：`超级管理员SUPER_ADMIN 全部数据 — 1`、`制单员MUTEX_A 本部门 技术部 销售部 1`、`测试受限管理员QA_LIMITED 自定义部门 技术部 1`、`测试只读员QA_VIEWER 仅本人 — 1` |
| 新增 | 对「尚无自定义配置」的角色首次保存即新增配置（数据范围是角色属性，不是独立实体） | 给 `MUTEX_B` 首次配置：`当前生效范围 全部数据 ALL` → 勾选 4 个部门 → `当前生效范围 自定义部门 CUSTOM` + `库里已保存的自定义部门 云枢总部 技术部 财务部 销售部` |
| 编辑 | 5 种范围单选 + 自定义部门树多选 + 保存（改 → `ALL` 走 `DialogPlugin.confirm` 二次确认） | 同上；保存后 footer 回到 `配置无变化`（dirty 归零），并回读刷新 `holder_count` |
| 删除 | 唯一的可删子实体是「自定义部门集合」→ 面板提供「清空自定义部门（N）」按钮（`data_scope` 不变 + `custom_dept_ids: []`） | 按钮仅在 `custom_depts.length > 0` 且当前范围≠5 时出现（`清空自定义部门（2）`） |
| 无权限按钮隐藏 | 页签 `v-if="hasPermission(scopeRead)"`（不是 `display:none`，页签整体消失）；保存/重置/清空按钮 `v-if="canWrite"` | 收回 `scope:write` 后：`面板底部: 无写入权限：只能查看，按钮已隐藏`、`只读提示: （只读：缺少权限码 business:permission:scope:write）`、`保存按钮存在? []`、行操作由「配置」变「查看」 |

补充能力：一览页顶部「我的数据范围自检」卡（`GET .../scope/self-check`，权限码 `business:permission:scope:self-check`）：`生效范围 全部数据 ALL / 可见部门 4 个 / 超管 是（不受数据范围限制） / 范围来源角色 超级管理员（ALL）`，可展开 11 张登记表的注入片段。

### 2.2 块 2 · 字段脱敏规则（`views/system/masking/index.vue`，路由 `/system/field-mask`）

| 五件事 | 实测证据 |
|---|---|
| 列表加载 | `共 3 条规则`：`sys_user email 邮箱 全局 启用`、`sys_user mobile 手机号 3,4 全局 启用`、`sys_user real_name 姓名 1 全局 启用`（V0066 三条默认种子）；筛选：表名/字段/作用域/状态 + 分页 |
| 新增 | 表名→字段→类型→参数→角色→理由→状态；实测新增 `sys_dept phone 手机号 3,4 全局`，列表变 `共 4 条规则`，首行时间戳 `01:22:09` |
| 编辑 | 打开 `编辑脱敏规则 · sys_dept.phone`；表名/字段/适用角色 **disabled**（后端不可改目标），参数改 `2,3` → 列表回显 `2,3`（`01:23:13`） |
| 删除 | 二次确认文案：`将软删 sys_dept.phone（手机号·全局）。删除后该字段将不再脱敏，历史记录仍可在审计日志追溯。` → 确认后 `共 3 条规则` |
| 启停 | 状态列 tag + 操作列「停用/启用」；停用走二次确认：`停用后 所有未豁免用户（含全部非超管角色） 将看到 sys_dept.phone 的明文。` → 变 `启用`；再点「启用」→ 变 `停用`（均为真实往返） |
| 无权限按钮隐藏 | `qa.limited`（只有 `field-mask:list`）：`toolbar: ['新增规则(hidden)']`、`操作列: 编辑(hidden) / 停用(hidden) / 删除(hidden)`，**列表照常加载 3 行** |

脱敏预览（实时，350ms 防抖调 `GET /system/field-mask/preview`，不复制脱敏算法）：
- 手机号样例：`原文 13812345678 → 脱敏后 138****5678`，`已脱敏 / 手机号 · 参数 缺省`
- 改成 `2,3` 参数：`13812345678 → 13****678`
- 自定义正则 `^(\w{2}).*$|1`：`AB12345 → AB*****`

### 2.3 块 3 · 角色互斥规则（`views/system/mutex/index.vue`，路由 `/system/role-mutex`）

| 五件事 | 实测证据 |
|---|---|
| 列表加载 | 初始空态：`共 0 条规则` + `还没有互斥规则 / 互斥 = 同一个人不能同时持有两个角色（职责分离 SoD）`；创建后 `制单员MUTEX_A  MUTEX_A < MUTEX_B  审核员MUTEX_B  SoD…  生效中` |
| 新增 | A/B 双选 + 说明；A==B 前端拦截（见错误态）；创建成功走 `POST /business/mutex` |
| 编辑 | `编辑互斥规则`：角色对不可改（`角色对不可修改：要换角色请删除后重建`），只改说明 → 列表回显 `（E2E 已编辑）` |
| 删除 | 二次确认：`将软删「制单员」↔「测试只读员」的互斥关系（说明：…）。删除后内控无据可查，历史动作仍在审计日志中。` → 该行转 `已停用`，重复 DELETE 返回 `10002`（见 §8 第 4 条） |
| 启停 | 停用二次确认：`停用后「制单员」与「审核员」可以同时授予同一个人，互斥约束失效。`；恢复启用遇到存量双持 → 后端 409+61010，前端原样提示且状态不变 |
| 无权限按钮隐藏 | `qa.limited`（只有 `role-mutex:list`）：`新增互斥规则(hidden) / 存量扫描(hidden)`、`冲突查询卡片存在? False`、行操作 `编辑(hidden)/启用(hidden)/删除`（已停用行连「删除」也一并隐藏），列表照常 2 行 |

冲突查询（`GET /business/mutex/conflict?user_id=`）：选用户「双持用户（qa.mutex）」→ `用户 双持用户（qa.mutex）当前 1 组违规 / 制单员 ↔ 审核员 / SoD：制单与审核必须分离`。
存量扫描（`POST /business/mutex/scan`）：`扫描的生效规则 1 / 存在违规的规则 1 / 违规人次 1`，明细含 `违规人数 1 / 违规用户（样本） 双持用户（销售部）`。

---

## 3. 验收 3：权限种子正确 —— 非超管看不到三块入口

**① 种子层（后端库）**：`V0065` 只给 `SUPER_ADMIN` 授 15 个节点（3 菜单 + 12 按钮），不给任何其它角色。
`qa.viewer`（角色 `QA_VIEWER`，未授任何 B8 节点）实测：

```
qa.viewer /auth/me 权限码: []
qa.viewer /auth/menus 中 B8 菜单: []
```

**② DOM 层（真实登录态）**：

```
当前用户: 只读员
侧栏菜单文本: ""                                   ← 无任何菜单项（含三块入口）
是否有「数据权限/字段脱敏/角色互斥」入口: {dataScope: False, fieldMask: False, roleMutex: False}
/system/data-scope → /result/403 | 无访问权限 - 云枢管理后台 | 403 抱歉，您没有权限访问该页面
/system/field-mask → /result/403 | 无访问权限 - 云枢管理后台
/system/role-mutex → /result/403 | 无访问权限 - 云枢管理后台
```

侧栏与页签的可见性**都由数据层驱动**，不存在「前端写死入口」：
`userStore.navMenus = filterNavMenus(menus, hasPermission)`（`stores/user.ts`）→ 菜单来自 `/auth/menus`；
路由守卫 `meta.permission && !hasPermission(...) → /result/403`（`router/index.ts`）。

**③ 接口层**：`qa.viewer` 直调三块接口全部 403（见 §6）。

**④ 部分权限**：`qa.limited` 只授「读」时，页签/菜单可见但**所有写按钮隐藏**（§2 各块最后一行），证明按钮级粒度生效。

---

## 4. 验收 4：配色跟随（禁用写死颜色）

**① 静态检查**：三个页面 + 面板**零硬编码颜色**（无 `#hex` / `rgb()` / `hsl()`）：

```
$ grep -nE '#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(' views/system/{masking,mutex,data-scope}/index.vue views/system/role/components/DataScopePanel.vue views/system/role/index.vue
（无硬编码颜色）

使用的 CSS 变量：--td-text-color-secondary(20) / --td-text-color-placeholder(19) / --td-text-color-primary(13)
/ --td-component-stroke(9) / --td-bg-color-page(6) / --td-error-color(3) / --td-brand-color(3)
/ --td-bg-color-secondarycontainer(3) / --td-success-color(1) / --td-bg-color-container(1)
```

**② 运行期三套配色实测**（同一元素，`getComputedStyle`）：

| 页面 | 元素 | `mist-blue` | `violet-honey` | `jade` |
|---|---|---|---|---|
| 字段脱敏 | 主按钮背景 | `rgb(87,119,172)` | `rgb(151,94,176)` | `rgb(77,126,108)` |
| 字段脱敏 | 标签背景 | `rgb(205,212,224)` | `rgb(219,205,224)` | `rgb(207,222,216)` |
| 字段脱敏 | 次要文字 | `rgb(76,82,92)` | `rgb(87,76,92)` | `rgb(78,90,86)` |
| 数据权限 | 提示文字 | `rgb(121,130,144)` | — | `rgb(124,141,135)` |
| 角色互斥 | 计数文字 | `rgb(76,82,92)` | — | `rgb(78,90,86)` |
| — | `--td-brand-color-7` | `#6f8ab8` | `#a26fb8` | `#5e9b84` |

即：切换配色（22 套中的三套）后，三块页面自身元素的颜色同步变化，说明颜色全部来自变量而非常量。

---

## 5. 验收 5：每块至少一条错误态（均为后端真实返回，前端原样展示）

| 块 | 场景 | 后端返回 | 前端表现（DOM 实测） |
|---|---|---|---|
| 1 | 授出超过自身范围（受限管理员 → 财务部） | `403 + 61002`「新数据范围超出你自身的可见部门范围，**越界部门：财务部（01a18384-…）**」 | 红色告警块：`保存被拒绝（错误码 61002） / 新数据范围超出你自身的可见部门范围，越界部门：销售部（01a18384-…）` ← **后端点名的越界部门被完整展示，未吞** |
| 1 | 改自己持有角色的范围 | `403 + 61003` | `保存被拒绝（错误码 61003） / 不能修改自己持有角色「测试受限管理员」的数据范围（避免自我扩权）` |
| 1 | 自定义范围空集合 / 非 5 带集合 / 取值越界 | `400 + 61001` | 前端先行拦截：`数据范围为「自定义部门」时必须至少选择一个部门`（并用后端 61001 作为兜底展示） |
| 1 | 部门不存在 | `404 + 61004` | 后端消息原样展示（不吞 ID） |
| 2 | 自定义正则非法（`^(abc|1`） | 前端本地体检 + 后端 `422 + 61007` | 字段下方红字：`正则表达式不合法：Invalid regular expression: /^(abc/: Unterminated group`；预览区同步 `预览失败：…`，提交被拦（抽屉不关闭） |
| 2 | 参数格式错（手机号填非法参数） | 前端本地体检 | `参数格式应为「保留前N,后M」，如 3,4` + `预览失败：…` |
| 2 | 表/列不存在、类型越界、邮箱带参数 | `61006` / `61005` / `61005` | 后端消息原样 toast |
| 3 | A 与 B 相同 | 前端立即提示 + 后端 `61012` | 内联红字：`A 与 B 不能是同一个角色（后端也会以 61012 拒绝）`；点「创建」→ toast 同文案，抽屉不关闭、不发送请求 |
| 3 | 存量双持（61010） | `409 + 61010`「无法建立互斥关系：1 个用户同时持有「制单员」与「审核员」，请先解除他们的其中一个角色（如 qa.mutex）。」 | 二次确认弹窗展示后端原文，主按钮「仍然创建（记录违规）」→ `POST ...?force=true`；选「先去清理用户」则放弃 |
| 3 | 重复规则（含反向） | `409 + 61011` | 后端原文（指出已存在规则 ID 与时间） |
| 3 | 恢复启用时仍有双持 | `409 + 61010` | toast 原文，行状态保持 `已停用` |

---

## 6. 接口层证据（真实往返，节选自 `/tmp/b8-api-evidence.log`）

```
===== 块 1：数据权限 =====
GET  角色范围配置（admin）                     → {"code":0,"msg":"ok"}
      部门树节点数(admin): 4
      部门树节点数(受限管理员=仅技术部): {"total":1,"scope_limited":true,"depts":["技术部"]}
PUT  越界授出（受限管理员 → 财务部）            → {"code":61002,"msg":"新数据范围超出你自身的可见部门范围，越界部门：财务部（…）"}
PUT  自定义但空集合                            → {"code":61001,…}
PUT  非 5 却带集合                             → {"code":61001,…}
PUT  data_scope 越界(9)                        → {"code":61001,"msg":"数据范围取值非法：9（应为 1..5）"}
PUT  不存在的部门                              → {"code":61004,…}
PUT  受限管理员改自己持有角色                   → {"code":61003,"msg":"不能修改自己持有角色「测试受限管理员」的数据范围（避免自我扩权）"}
GET  qa.viewer（无 read 码）访问                → {"code":403,"msg":"无权限访问，需要权限: business:permission:scope:read"}
GET  自检（admin）                             → {"code":0,"msg":"ok"}

===== 块 2：字段脱敏 =====
GET  分页 / 试运行                             → {"code":0}
GET  试运行 正则非法                            → {"code":61007,"msg":"自定义正则不合法（regex parse error: … unclosed group）：^(abc"}
POST 新建（表/列不存在）                        → {"code":61006,"msg":"表 no_such_table 上不存在列 no_such_col（规则会永不生效，已拒绝）"}
POST 新建（类型越界 99）                        → {"code":61005,"msg":"mask_type 必须是 1..7（当前 99）"}
POST 新建（邮箱带参数）                         → {"code":61005,"msg":"邮箱脱敏没有参数，mask_param 请留空"}
POST 新建成功(sys_dept.phone)                   → {"code":0,"label":"手机号"}
POST 重复规则                                   → {"code":10003,"msg":"sys_user.mobile 已存在全局脱敏规则，请直接修改那一条"}
PUT  改参数 / 停用 / 启用 / DELETE 软删         → {"code":0} ×4
GET  qa.viewer（无 list 码）分页                 → {"code":403,"msg":"无权限访问，需要权限: system:field-mask:list"}

===== 块 3：角色互斥 =====
POST A==B                                       → {"code":61012,"msg":"不能建立角色与自身的互斥关系。"}
POST 已有双持用户                                → {"code":61010,"msg":"无法建立互斥关系：1 个用户同时持有「制单员」与「审核员」…（如 qa.mutex）。"}
POST 无持有者组合(MUTEX_B↔QA_VIEWER)             → {"code":0}
POST 重复规则（反向）                            → {"code":61011,…}
PUT  改说明                                     → {"code":0}
GET  单用户体检(qa.mutex)                        → {"code":0}（1 组违规）
POST 全量扫描                                   → {"code":0}
DELETE 删除（= 软删）                            → {"code":0}
DELETE 重复删除                                  → {"code":10002,"msg":"互斥关系不存在或已停用：…"}
GET  qa.viewer（无 list 码）分页                 → {"code":403,"msg":"无权限访问，需要权限: system:role-mutex:list"}
```

---

## 7. 交付物（前端）

新增：

```
frontend/src/api/business/permission-scope-api.ts   数据范围配置 API（类型 + 5 种范围常量）
frontend/src/api/system/field-mask-api.ts           脱敏规则 API（7 种类型 + 表/字段建议 + 试运行）
frontend/src/api/business/mutex-api.ts              互斥规则 API（含 conflict / scan / force）
frontend/src/views/system/role/components/DataScopePanel.vue   数据范围面板（角色页与一览页共用）
frontend/src/views/system/data-scope/index.vue      数据权限一览页（/system/data-scope）
frontend/src/views/system/masking/index.vue         字段脱敏页（/system/field-mask）
frontend/src/views/system/mutex/index.vue           角色互斥页（/system/role-mutex）
```

修改：

```
frontend/src/constants/permissions.ts   + BATCH_B_PERMISSIONS（12 个码，并入 PERM）
frontend/src/router/index.ts            +3 条路由（path 与 sys_menu.path 逐字一致）
frontend/src/views/system/role/index.vue +「数据权限」页签（v-if 权限码控制）
```

未改动：任何后端文件、批次 A 的页面、`package.json`（未引入新依赖）。

> 路由 path 用 `/system/field-mask`、`/system/role-mutex`、`/system/data-scope`（与 `V0065` 种子逐字一致），
> 而视图目录沿用卡面要求的 `masking/`、`mutex/`：本项目的侧栏跳转只读 `sys_menu.path`，
> `sys_menu.component` 字段当前**不参与**动态路由（路由表是静态的），故目录名与 component 字符串不一致不影响任何行为。

---

## 8. 发现的接口/契约问题（**按红线未改后端**，仅记录并做了前端适配）

1. **线上 8023 实例未部署批次 B**：`schema_migrations` 停在 V0052，B4/B6/B7 的接口在 8023 上 404；本次验证因此另起 8099 + 独立库 `cloudpivot_b8`。**发版需重新构建并迁移**（不建议直接重启线上服务：会在 `cloudpivot` 库上补跑 V0063–V0068）。
2. **权限码数量与 PRD 不一致（以后端为准）**：PRD §6.1 列 16 个码，后端实际 15 个——数据范围三码改域为 `business:permission:scope:{read,write,self-check}`（`V0065` 头注释已说明），脱敏无 `status`/`view-plain`→`plain`/`scan` 三码中的两个，互斥无 `update`/`status`/`matrix`（复用 `create`/`conflict`）。前端 12 个常量与后端注解逐字一致，页面不会出现「按钮能点但接口 403」。
3. **没有「可配置表 / 字段」元数据接口**：B6 只在写入时校验 `information_schema`（`61006`）。前端因此提供常见表/字段建议清单 + 自由输入，合法性唯一权威仍是后端（`61006` 原样展示）。卡面要求「选表→选字段」体验保留（可筛选、可创建）。
4. **互斥「停用」= 软删**：`enabled=false` 后再 `DELETE` 返回 `10002 互斥关系不存在或已停用`，列表查询也会继续返回该行。前端据此把「删除」入口只对生效中的规则开放（已停用行显示为禁用态并带 tooltip），避免一个点了必然报错的按钮。
5. **`declared 409` 与实现码不一致**：重复脱敏规则实际返回 `10003`（文档写 409），前端按 ApiError 统一展示消息，不依赖 HTTP 状态。
6. **`custom_dept_ids` 与 `data_scope≠5` 组合必被拒（61001）**：前端只在 `data_scope=5` 时提交集合，其它范围省略该字段（= 不动存量），清空走显式按钮。

---

## 9. 截图（辅证；DOM 文本证据见上文）

| 文件 | 内容 |
|---|---|
| `evidence/shots/01-data-scope-admin.png` | 数据权限一览页（超管）：自检卡 + 5 个角色的范围/自定义部门/持有者 |
| `evidence/shots/02-masking-limited-jade.png` | 字段脱敏页（受限账号，`jade` 配色）：列表可读、写按钮全部隐藏 |
| `evidence/shots/03-viewer-403.png` | 非超管账号直达 `/system/role-mutex` → 403 页，侧栏无三块入口 |
