# M5-25 在制品前端页（录入 + 余额）— 证据

- 卡号：`t_8d1440e7`（M5-25【M5·成本】在制品：前端页）
- 日期：2026-11-19
- 依赖父卡：M5-21（按工序完工率，`GET/PUT /cost-object/{id}/operations`）、M5-23（期末在制余额，`GET /cost-wip-balance`）
- 验证库：`cp_m525_drill`（145，**空库首启**跑完迁移到 HEAD，含 V0471）→ 验完 DROP

## 1. 交付物

| 类别 | 文件 | 说明 |
|---|---|---|
| 前端页面 | `frontend/src/views/business/erp/cost-wip/index.vue`（43.5 KB） | 两个页签：完工率录入（列表 + 抽屉）/ 在制余额（合计卡 + 表 + 展开行） |
| 前端逻辑 | `frontend/src/views/business/erp/cost-wip/cost-wip-logic.ts`（33.1 KB） | 纯函数：查询构造 / 词表判色 / 空的语义 / 编辑器校验与整表 payload，零业务算术 |
| API 层 | `frontend/src/api/business/erp/cost-wip.ts`（110 行）+ `cost-wip-kinds.ts`（515 行，类型与词表） | `GET/PUT …/operations`、`GET …/cost-wip-balance`，路径写全 `/api` |
| 路由 | `frontend/src/router/index.ts`（+14 行一处） | `/business/erp/cost-wip`，`meta.permission = business:erp:cost-object:list` |
| 菜单迁移 | `backend/app/migrations/business/V0471__cost_wip_menu.up.sql` / `.down.sql` | 只碰 `sys_menu` / `sys_role_menu`：1 条页面行（sort 76）+ 派生角色授权 + 祖先闭包，**零新码 / 零新表 / 零新路由** |
| 单测 | `frontend/tests/unit/erp-cost-wip.test.ts`（667 行，42 条） | `pnpm test:unit` 直接可跑（node:test + tsx） |

**没有**新增：后端 `.rs` 文件（0 行改动）、权限码、后端路由、错误码、表/列（V0471 只写 `sys_menu` / `sys_role_menu`）。

## 2. 页面口径（为什么长这样）

1. **前端零公式**：综合完工率、期初还原、要素拆分、余额倒挤、结转核对全在后端；页面只展示。
   表格改动时**不预览新汇总**，只提示「下面是上次保存的值，点保存后由后端重算」——
   预览会造出「预览 50.00 / 保存后 50.01」这种没人解释得清的差。
2. **合计吃后端 `totals`**：服务端分页下拿当前页加出来的「期末余额合计」翻页就变，而财务会拿它跟总账对。
3. **三种 `null` 不画 0**（会被读成「没有余额」）：
   `rollup=null` → 「还没有工序」+ 说明对象级沿用哪个值；`wip_total` 等为 `null`（`carry.state=blocked`）→ `—` + 展开行列 `issues`；`elements[]` 三项加不齐 → `—` + 「红冲重算」。
4. **PUT 是整表覆盖**：表格里看得见的每一行都提交（把「暂时没填」当成不提交 = 那行工序被删）。
5. **期间必填**：为空时**不发请求**（`buildBalanceQuery` 返回 `null`），后端一律 400，前端不替用户猜月份。
6. **权限零新增**：读 `business:erp:cost-object:list`、写 `business:erp:cost-object:update`
   （与后端 `cost_wip_pct_dto::PERM_READ/PERM_WRITE` 逐字一致；ERP 全域尚未做 PERM-01 解耦，新开 `cost-wip:*` 会变成没有菜单行承载的死码）。

## 3. 验证（全部为本次实跑输出）

### 3.1 前端门禁

```
pnpm typecheck        → rc=0（vue-tsc --noEmit，0 error）
pnpm typecheck:unit   → rc=0（tsconfig.unit.json 覆盖 tests/unit）
pnpm build            → rc=0，产出 dist/assets/index-*.js（34.6 KB，含「工序完工率」文案 = 页面真的进包）
npx tsx --test tests/unit/erp-cost-wip.test.ts → 42 pass / 0 fail
```

### 3.2 后端门禁（本卡无 `.rs` 改动）

```
cargo fmt --check                                  → rc=0
cargo test --lib system::menu::permission_audit_tests → 8 passed / 0 failed
```

### 3.3 迁移：空库首启 + 重放幂等（145，`cp_m525_drill`）

```
createdb cp_m525_drill（空库）→ 启动实例（127.0.0.1:18311）
schema_migrations：400 行；V0471 = versioned/cost_wip_menu/version 471/success=t/applied_seq 388
菜单行：在制品 | menu_type=2 | /business/erp/cost-wip | sort 76 | icon chart-stacked
        component business/erp/cost-wip/index | auth_code business:erp:cost-object:list
        父目录 /business/erp/cost（成本核算）
重放 V0471.up.sql（psql -f，同一库）→ rc=0，菜单行仍 1 条、授权无重复（ON CONFLICT DO NOTHING）
```

> 超管授权的时点：版本化迁移先于种子 `S__role_data.sql`，空库首启时 `SUPER_ADMIN` 角色还不存在，
> §2.2 的显式授予落 0 行（超管靠 `*:*:*` 通配，功能不受影响）——与 HR / M5-19（V0466）同一现象。
> 重放一次该迁移即补上（本次重放后授到 3 个角色）。

`bash scripts/verify/menu_invariants.sh cp_m525_drill` → **8 条断言全绿**（菜单总数 572 / path 唯一 / 同父 sort 唯一 / title ≤12 / icon 合法 …）。

### 3.4 接口冒烟（真库真接口，`smoke_wip_page.py`）

```
WIP_BASE=http://127.0.0.1:18311 NIK_DATABASE_NAME=cp_m525_drill python3 smoke_wip_page.py
===== 44 PASS / 0 FAIL =====
```

覆盖：两条路由挂上（200 而非 404/SPA HTML）、录入出参逐字段、三工序 100×1+50×2+0×1 ⇒ 综合 50%
且对象级回写 50、逐行占比 25/50/25 与贡献 25/25/0、整表覆盖（删一道+改一道 ⇒ 计数 + 综合 80%）、
400 五连（重复 seq / 缺完工率 / 101 / 权重 0 / 超 200 行）、409 已关批、余额出参（totals 24 键 /
carry 八态 / state_options 10 条）、余额 400 三连、清场零残留。

### 3.5 页面功能（真浏览器，vite dev → 代理到 18311）

| 截图 | 内容 |
|---|---|
| `01_operation_tab.png` | 完工率录入页签：成本对象 3 批 + 「录入完工率」入口 |
| `02_editor_drawer.png` | 抽屉：抬头（对象级完工率 50%）+ 3 道工序行（权重 1/2/1、占比 25/50/25、贡献 25/25/0）+ 公式与口径 |
| `03_balance_tab.png` | 在制余额页签：整表合计 8 卡（行数 / 期初 / 投入 / 转出 / 期末 / 要素 / 未入账 / 需处理）+ 表 + 「算不出」行画 `—` |

**写入链路实测**（不是只渲染）：抽屉里「添加工序」→ 填 2 行（下料 100%、焊接 50%，权重各 1）→ 保存：

```
页面提示：已保存：新增 2 行 · 更新 0 行 · 删除 0 行
          综合完工率 75%（Σ权重 2 · Σ(权重×完工率) 150）· 对象级 与汇总一致
DB 核对：biz_cost_object.CB-M525DEMO-B.completion_pct = 75.00
        biz_cost_object_operation 2 行（seq 1 100%、seq 2 50%，deleted_at IS NULL）
```

余额页签同一时刻的读数为：期初 0.00 / 本期投入 3,200.00 / 完工转出 2,755.56 / 期末余额 444.44，
未入账 2 行（试算有账面没有）、算不出 2 行（完工率 0 且本期无产量 → 金额列 `—`，不是 0）。

## 4. 未做的事（明确不在本卡范围）

- 没做「我的在制」自助视图：ARCH-9 §6.1 未列，无码无接口。
- 没做导出（xlsx）：本卡范围只有「录入 + 余额」，导出另卡。
- 没做 PERM-01 页面码 / 接口码解耦：ERP 全域未解耦，只改本卡会造出半套。
- 演示夹具 `seed_demo.py` 留在本目录（`--clean` 可清场）；验证库 `cp_m525_drill` 验完已 DROP。
