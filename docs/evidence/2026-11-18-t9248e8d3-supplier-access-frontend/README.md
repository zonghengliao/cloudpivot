# 前端 · supplier-access 菜单路由未登记（n08 红）· 证据包

- 卡：`t_9248e8d3`（前端 · M4-14 遗留）
- 上游：`t_85e005d9`（全量套件第一轮跑出来的红：`system::menu::tests::n08` 点名
  `supplier-access (/business/erp/supplier-access, component=Some("business/erp/supplier-access/index"))`）
- 日期：2026-11-18
- 结论：**路由补上了、页面也从零落地了**（M4-14 只做后端，`docs/dead-permissions-20261101.md`
  §13 明写「Vue 页面文件不在本卡」）。`n08` 从红转绿（141 条菜单路径全部解析），
  并做了**反证**（把路由 path 改歪 → 同一条用例按设计报红并点名 supplier-access）。
  前端全量单测 2337/2337、`pnpm typecheck` / `pnpm typecheck:unit` / `pnpm build` 全绿；
  真机用两套权限点过：有码 → 新建/编辑/提交/作废都在，只读 → 只剩「详情」。

## 一、改了哪些文件（零迁移 / 零后端改动 / 零新权限码）

| 文件 | 性质 | 说明 |
|------|------|------|
| `frontend/src/router/index.ts` | 改（+13 行） | 补 `business/erp/supplier-access` 路由，`meta.permission = business:erp:supplier-access:page`（页面显形码只给前端，约定 14） |
| `frontend/src/api/business/erp/supplier-access.ts` | 新增 | 6 条接口 + 全量 TS 类型，路径写全 `/api/...`（漏前缀会命中 SPA fallback 返回 HTML） |
| `frontend/src/views/business/erp/supplier-access/supplier-access-logic.ts` | 新增 | 纯逻辑（筛选拼装 / 闭集标签 / 理由字数按字符 / 按钮判据），被单测直接打 |
| `frontend/src/views/business/erp/supplier-access/index.vue` | 新增 | 列表（`ListPage`）+ 新建/编辑弹窗 + 提交弹窗 + 详情抽屉（含 `FlowProgress` 审批进度） |
| `frontend/tests/unit/erp-supplier-access.test.ts` | 新增 | 15 条纯逻辑用例 |
| `frontend/src/constants/error-messages.ts` | 改 | 补 `66099` / `66100`（M4-14 后端新增、前端码表没跟上，`error-messages` 单测本来就红） |
| `frontend/tests/unit/flow-biz-link.test.ts` | 改 | 断言字面量 27 → 28（M4-14 的 `SUPPLIER_ACCESS` 已进前端镜像，断言数字漏改） |

### 权限码对齐（按后端 `#[nik::permission]` 逐个，`V0455` 逐字）

| 码 | 管什么 | 前端落点 |
|----|--------|----------|
| `business:erp:supplier-access:page` | 侧栏显隐 + 路由 `meta.permission` | 只写在 `router/index.ts`；页面源码零引用（后端也零引用） |
| `…:list` | `GET /page` + `GET /{id}` | 页面取数码；没有它 → 顶部黄条说清缺什么，不摆点不动的按钮 |
| `…:create` | `POST /supplier-access` | 「新建申请」按钮（无码置灰 + tooltip） |
| `…:update` | `PUT /{id}` + `DELETE /{id}`（一个码管改与作废） | 行内「编辑」「作废」+ 抽屉里「编辑草稿」（无码整条不渲染） |
| `…:submit` | `POST /{id}/submit` | 行内「提交」+ 抽屉里「提交审批」 |

「三个动作」在页面上是**一张申请单的三种类型**（`ADMIT` 准入 / `BLACKLIST` 黑名单 /
`RELEASE` 解除黑名单，与迁移 `ck_biz_sa_type` 同闭集），新建弹窗用单选切换，
选黑名单时才出现「评估等级」一格（`ck_biz_sa_grade_only_blacklist`）。

### 页面遵守的三条后端口径（不自己重算）

1. **按钮判据只读后端**：`editable` / `submittable` 由 `to_row_res` 给，页面不写第二套状态机；
   越态请求由后端 409（`66100`）兜，报文原样弹给用户。
2. **「通过后」读 `target_supplier_status`**：不按 `access_type` 重推（重推迟早与后端分叉）。
3. **提交与通过都不改主数据**：页面任何按钮都不会让供应商「立刻」被拉黑 —— 生效点在审批通过。

## 二、验证（原始输出都在本目录）

| # | 命令 | 结果 | 输出 |
|---|------|------|------|
| ① | `cargo test -p cloudpivot --lib system::menu::tests::n08 -- --test-threads=1 --nocapture` | **1 passed**，`[N08] 141 条菜单路径全部能在前端路由里解析 ✓`（真连库，不是 SKIP） | `01_n08_green.out` |
| ② | 反证：把路由 `path` 改成 `…-PROBE` 再跑同一条 | **按设计报红**，报文逐字复现卡面那行 `supplier-access (/business/erp/supplier-access, component=Some("business/erp/supplier-access/index"))`；改回后 ① 复绿 | `02_n08_negative_control.out` |
| ③ | `cargo test -p cloudpivot --lib system::menu::tests -- --test-threads=1` | **14 passed / 0 failed**（n01–n14 全绿，含 n09 图标、n10 权限码承载行） | `03_menu_module_green.out` |
| ④ | `pnpm typecheck` | exit=0（vue-tsc 无输出=无错） | `04_typecheck.out` |
| ⑤ | `pnpm test:unit` | **2337 passed / 0 failed**（含本卡新增 15 条；修前是 2335/2，红的两条见下「顺手修掉的两处漂移」） | `05_unit_suite.out` |
| ⑥ | `pnpm build` | `✓ built in 53.29s`，exit=0（`dist/assets` 里含本页 chunk） | `06_build.out` |

### 跑 n08 用的库（交代清楚，别当成「随便挑了个库」）

- 用的是 **145 上 `cloudpivot_dev`**（`172.18.10.145:5432`，含 `V0455` 的 supplier-access 菜单行
  与 `V0456` 的框架协议 5 个按钮行）。`n08` 只 `SELECT sys_menu` + 读前端路由文件，是**只读**判据；
  模块里另外几条（n07 等）自带前缀夹具并自行清场，跑完复查：
  `sys_menu` 里 `name like 'n0%'` 0 行、`sys_role` 里 `%_ROLE` 0 行、近 10 分钟新建菜单 0 行。
- **踩到的坑（写下来免得下一个人再踩）**：先用一张更老的快照库 `cp_m414_gen_3653834` 跑整模块，
  `n10_annotated_permissions_have_menu_code` 红，点名
  `business:erp:framework-agreement:{list,create,update,delete,submit}` 「没有承载菜单」——
  那是**该库快照缺 `V0456`**（不是本卡引入，本卡前后端零改动）。换到含 `V0456` 的库后 14/14 全绿。
  **判据：跑菜单用例的库必须与 HEAD 迁移同代**，否则红的是库不是代码。

## 三、真机点验（两套权限，双向证明显隐）

用零依赖 Node mock 后端（`/root/.hermes/profiles/frontend-developer/cache/scratch/sa-mock/mock-server.cjs`）
起在 `127.0.0.1:8023`（= vite dev 代理默认后端地址），`pnpm dev --port 5174` 起前端，headless Chrome 驱动：

| 断言 | 全权限（page+list+create+update+submit+supplier:list） | 只读（page+list） |
|------|------|------|
| 菜单「供应商准入」可进、标题正确 | ✅ `供应商准入 - 云枢 CloudPivot` | ✅ |
| 列表出数 | ✅ 3 张，`共 3 张` / `共 3 条数据` | ✅ 3 张 |
| 「新建申请」 | ✅ 蓝色可点 | ✅ 存在但 `disabled=true`（置灰 + tooltip 说明缺哪个码） |
| 「详情」 | ✅ 3 行都有 | ✅ 3 行都有 |
| 「编辑 / 提交 / 作废」 | ✅ 草稿行可点；审批中 / 已生效行 `t-is-disabled` | ✅ DOM 里根本没有（`v-if` 整条不渲染） |
| 详情抽屉 | ✅ 标题 `SAC-20261118-000001 · 准入`、类型/状态标签、申请单号/供应商/现值/通过后/等级/申请人/时间/结论/理由/备注 | 同上 |
| 新建弹窗 | ✅ 供应商下拉含**停用**供应商（`SUP-0042 …（停用）`，解除黑名单要选的就是它）、三种类型单选 + 说明、理由实时字数 | — |
| 编辑弹窗 | ✅ 供应商与类型**锁定**（`disabled`），理由回填 `当前 18 个字（够提交）` | — |
| 提交弹窗 | ✅ 预填草稿理由 → 点「提交」→ toast `SAC-20261118-000001 已提交审批（采购经理）`，列表自动刷新 | — |

截图：`01_list_full_perm.png`（全权限，操作列 `详情 编辑 提交 作废`）、
`02_list_read_only.png`（只读，操作列只有 `详情`，新建申请置灰）。

## 四、顺手修掉的两处前端漂移（都是 M4-14 的前端侧尾巴）

`pnpm test:unit` 在修前是 **2335 passed / 2 failed**，两条都不是本卡页面引入，但都是
**M4-14 后端先落地、前端没跟上**的同一条线，属于前端职责范围，一并修掉（否则验收「全绿」不成立）：

1. `error-messages`：「`error_code.rs` 里 66xxx 每一条常量前端表里都有」红，缺
   `ERP_SUPPLIER_NOT_USABLE(66099)` / `ERP_SUPPLIER_ACCESS_STATUS_GUARD(66100)` → 按
   `error_code.rs` 的注释补「现象 + 该怎么办」（66100 首版 113 字被「≤100 字」那条断言打回，压到 62 字）。
2. `flow-biz-link`：Rust `FlowBizType::as_str` 已是 28 个字面量（含 `SUPPLIER_ACCESS`），
   前端镜像 `FLOW_BIZ_TYPES` 也已是 28 项，只有断言里的字面量停在 27 → 改成 28 并留迁移说明。

## 五、本卡**没**做的事（边界）

- 零迁移、零后端改动、零新权限码、零菜单行（菜单/权限码全部是 `V0455` 早就落好的）。
- 不做「供应商评估」页（那是 M4-16，另一张卡）；本页「评估等级」只填**快照**，不读评估模块。
- 不做「跳过审批」UI：没配置 `supplier_access` 流程时后端 409 会点名流程码，页面原样弹报文
  （`66100`/`66033` 都已翻成人话）。要跳过需 `business:erp:skip-approval` + ≥10 字理由，
  属例外通道，留给有该权限的人走审批中心的既有口径。
