# M3-06 报价单前端页（列表 / 详情 / 转单）— 验收证据

卡：`t_3ae6db9c`（M3-06【M3·销售】报价单：前端列表/详情页，含转单按钮）
上游：M3-02（CRUD + 状态机 V0433）→ M3-03（转单 V0434）→ M3-04（审批流 V0435）→ M3-05（单测）
日期：2026-11-17 · 分支 `main`

## 一、交付了什么

| 文件 | 类型 | 说明 |
|------|------|------|
| `frontend/src/api/business/erp/quotation.ts` | 新增 | 8 个接口 + 全部入参/出参类型（逐字段照后端 DTO，不是猜的） |
| `frontend/src/views/business/erp/quotation/quotation-logic.ts` | 新增 | 纯逻辑：边表 / 原因边 / 金额 / 合计 / 校验 / 转单可点性 / 审批徽标 |
| `frontend/src/views/business/erp/quotation/index.vue` | 新增 | 列表（筛选+分页）· 详情抽屉 · 建单/改单 · 状态流转 · 送审/发出 · **转销售订单** · 软删 |
| `frontend/src/router/index.ts` | 改（+11 行） | 注册 `path: 'business/erp/quotation'`（`meta.permission = business:erp:quotation:list`） |
| `backend/app/migrations/business/V0436__quotation_page_menu.up.sql` | 新增 | 把 V0433 留空的页面节点补成真页面：`path` / `component` / `is_visible=TRUE` |
| `backend/app/migrations/business/V0436__quotation_page_menu.down.sql` | 新增 | 还原成「只有码、没有页面」，**不删**码与授权 |
| `frontend/tests/unit/quotation-page.test.ts` | 新增 | 34 条纯逻辑单测（含「源码即契约」对拍） |
| `scripts/verify/m306_quotation_frontend_drill.py` | 新增 | 49 条真后端 + 真库的接口契约烟测 |
| `frontend/src/constants/error-messages.ts` | 改（+2 行） | 补 `66088`（M3-04 新增码，前端漏登记 → 原 `error-messages.test.ts` 红） |
| `frontend/tests/unit/flow-biz-link.test.ts` | 改（+2 行） | 枚举条数断言 24 → 25（M3-04 加 `QUOTATION_APPROVE`，镜像早已同步、断言漏改） |

> 后两项是本卡落地时才暴露的上游缺口（M3-04 只改了后端 + 镜像，没补前端码表与条数断言）。
> 它们直接落在本卡页面的使用路径上（`66088` 是本页「发出」的报错码），所以一并收口。

## 二、三条口径（写错就是错账 / 必然 409）

1. **发出不是改状态**：`DRAFT → SENT` 是「把价格发给客户」的对外承诺，挂在审批闸后面。
   页面**不给** PATCH status=SENT 的按钮，只给「提交审批」（走 `submit-approval`）与
   「审批已通过后 → 发出」两条路；判据是详情返回的 `approval.status`，**不是** `status`
   （`status` 仍是草稿时也可能正在审批）。实测：直接 PATCH → `409 + 66088`（见烟测第 21–22 条）。
2. **过期是现算事实**：`expired = today > valid_until`（CST），不是状态值。列表单独打「已过期」标，
   筛选用三态（全部 / 有效期内 / 已过期 → 不传 / `false` / `true`）。
3. **转单只认 `ACCEPTED` 且只转一次**：`converted_so_id` 非空即置灰（后端幂等也只是回原单），
   `SENT` / `DRAFT` / 终态各有自己的置灰理由（tooltip 写清）。

## 三、迁移 V0436（只追加、幂等）

- 前置护栏：节点 `…4331` 必须是 `menu_type=2` + `auth_code=business:erp:quotation:list` + 父节点 `…2701`；
- 只 UPDATE 还没补过的行（`WHERE path IS DISTINCT FROM …`）→ 重复执行 `UPDATE 0`；
- 自检：三列已落 / `path` 全局唯一 / `title` ≤12 字 / 页下 **5 个按钮**（V0433 的 4 个 + V0434 的 `convert`）仍在；
- down：三列还原为 `NULL/NULL/FALSE`，**不删**任何菜单行与 `sys_role_menu`，并断言 5 个码 + 5 个按钮仍在。

> 本迁移**不写** `sys_role_menu`：把报价单授给哪些角色是管理决策（与 V0433 / V0265 同口径）。

## 四、验证（全部真跑，命令 + 结果）

### 4.1 前端

| 命令 | 结果 |
|------|------|
| `cd frontend && pnpm typecheck` | ✅ 0 error |
| `cd frontend && pnpm typecheck:unit` | ✅ 0 error |
| `cd frontend && pnpm test:unit` | ✅ **2153 / 2153** pass（含本卡新增 34 条） |
| `cd frontend && pnpm build` | ✅ 构建成功；`dist/assets/index-C3vohoR9.js` 内含 `qt-convert-dialog`（页面真的进了产物） |
| `pnpm check:palettes` | ✅ 通过（1485 条对比度断言） |
| `node scripts/shell-variant-report.cjs --baseline …` | ✅ 无新增页面壳类声明（6 条存量不变） |

### 4.2 后端（本卡只加 SQL 迁移；Rust 0 行）

| 命令 | 结果 |
|------|------|
| `cd backend && cargo fmt --check` | ✅ clean |
| `cd backend && cargo clippy --lib -- -D warnings` | ✅ 0 warning（32s） |
| 克隆模板库升级（`-T cloudpivot_test` → `cp_m306_drill_1`）+ 起服务 | ✅ `数据库迁移完成: 应用 1 个迁移`（V0436），服务 `18023` 起 |
| `psql -f V0436.up.sql` 二次执行 | ✅ `UPDATE 0` + 自检通过（幂等） |
| `psql -f V0436.down.sql` → 再 `up` | ✅ 还原 `||f` → 恢复 `/business/erp/quotation|business/erp/quotation/index|t` |
| `ERP_DB_URL=… cp_m306_drill_1 cargo test --lib system::menu::tests::n08` | ✅ ok（菜单 path ↔ 前端路由真库门禁） |
| `bash scripts/verify/menu_invariants.sh cp_m306_drill_1` | ✅ 8 条断言全绿（532 条菜单，path 唯一含报价单） |

### 4.3 接口契约烟测（真后端 + 真库）

```bash
HOST=172.18.10.145                                                  # 99 上跑，连 145 的 PG
createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m306_drill_1   # 模板库克隆
# 起后端（NIK_DATABASE_* 指向该库，listen 18023）
python3 scripts/verify/m306_quotation_frontend_drill.py
```

结果：**49 条断言 / 0 失败**（`docs/evidence/2026-11-17-m306-quotation-page/00-drill.out`），覆盖：

- 登录 → 用户菜单里有报价单页（`path=/business/erp/quotation`）；
- 分页 `PageData` 形状；建单单号 `QT-20261117-…`、明细全量回包、**金额逐位对拍**
  （`12.3456 × 3 → 37.0368` / 税 `4.8148` / 含税 `41.8516` / 单头 `267.8516`）；
- `allowed_targets = [SENT, CANCELLED]`、`editable`/`deletable`、`approval: null`；
- 直接 `PATCH status=SENT` → **409 + 66088**（前端「发出」走审批的依据）；
- 草稿作废（无原因）→ 200；终态 `allowed_targets = []`；
- 转单：`ACCEPTED` → 新建 `SO-…`、搬 2 行、`meta.line_count=2`、金额原样、备注带「由报价单」；
  再点一次 → 200 + `already_converted=true` + **同一张** `so_no`；已转单再作废 → 409；
- 软删 → 详情 404；单号筛选命中；
- 有效期三态筛选：`expired=true` 只回已过期、`false` 只回未过期、两者相加 = 全部（造了真过期单，非假绿）。

### 4.4 浏览器实测（真实页面，非截图拼图）

`vite dev(5173) + 后端(8023) + 真库`，用 admin 登录逐项点过：

| 场景 | 结果 |
|------|------|
| `/business/erp/quotation` 侧栏入口可见 → 点进去 | ✅ 列表渲染，共 5 条，列：单号+报价日 / 客户 / 有效期 / 价税合计 / 状态 / 销售订单 / 操作（截图 01） |
| 已过期单 | ✅ 有效期列带红色「已过期」标（`QT-20261117-000007`，状态仍是草稿） |
| 详情抽屉（已转单那张） | ✅ 审批「从未提交审批」、销售订单 `SO-20261117-000001 · 时间`、明细 11 列、三项合计 |
| 详情按钮态（已转单） | ✅ 编辑 / 提交审批 / 转销售订单 / 删除 **全置灰**，只有「作废已接受的报价」可点 |
| 新建报价单 | ✅ 客户下拉只列 ACTIVE；填 2 × 88.5 / 13% → 行金额 177.00、税 23.01、合计 200.01（与后端同算法）；提交后建出 `QT-20261117-000004` 并自动打开详情 |
| 提交审批 | ✅ 徽标变「审批中（还等 2 人）· FLW-…」，**状态仍是草稿**（审批不是状态）；按钮 tooltip 说明「等结果或从审批中心撤回」 |
| 行操作置灰 | ✅ 已转单行：可点仅「详情」，置灰 编辑/转销售订单/删除；草稿行：可点 详情/编辑/删除，置灰 转销售订单 |
| 转销售订单（本卡核心） | ✅ 弹窗（下单日默认今天 / 交期 / 销售员 / 备注）→ 确认 → 行内变「已转单 01a1e68d…」、列表回写、提示生成新订单（截图 03） |

截图：`01-list.png` / `02-detail-approval.png` / `03-convert-dialog.png`（同目录）。

## 五、刻意没做的（属别的卡）

- **PDF 导出**（M3-07）：页面没有导出按钮，`DocPdfButton` 未接；
- **审批动作本身**（通过 / 驳回 / 撤回）：在审批中心做，本页只显示徽标 + 给「提交审批」；
- **数据范围**：`biz_quotation` 是 ERP 单据表（V0432 无 `dept_id` / 归属人列），后端 `SCOPE-EXEMPT`，
  前端不造「按部门过滤」的假筛选项；
- **列表页的「待批」列**：列表出参不带 `approval` 段（防 N+1，后端既定口径），
  所以「待批」只在详情显示 —— 不拿 `status` 猜。

## 六、库 / 环境

- 开发机 99，PG 在 `172.18.10.145:5432`（用户 `cloudpivot`），模板库 `cloudpivot_test`；
- 演练库 `cp_m306_drill_1`（克隆 + V0436 升级，验收证据见 `00-drill.out`）：**收尾时已 `dropdb`**，
  `pg_database` 里 `cp_m306%` = 0（本卡不在测试机留库）；
- 后端实例（18023 / 8023）与 `pnpm dev`（5173）均为本次临时进程，收尾时已停（端口已释放）。
