# t_539dab1d 证据：预算三页路由归位，n08 菜单守卫转绿

> 卡：【测试红·FIN-1-FE】预算三个页面缺前端路由（budget / budget/mapping / budget/report），菜单守卫 n08 报红
> 结论：**预算 3 条路由已在 HEAD 注册，`n08` 用例由 red 转 ok（1 passed / 0 failed）**。
> 页面本体由 `t_42d844c0` 交付（提交 `35750d5`），资产 3 条由 `t_bdea38c1` 交付（提交 `5832df8`）；本卡只做**真机复验 + 门禁认证**，未改产品代码。

## 1. 被测提交与环境

| 项 | 值 |
|----|----|
| 被测提交 | `df74a5f63117afa7bba1302ed9d4e29eda9aadb2`（HEAD，`df74a5f`；`35750d5`、`5832df8` 均为其祖先） |
| 被测树 | `/root/t_539dab1d-tree`（`git worktree add` 干净检出，`git status` 空；`/opt/cloudpivot` 工作树有他人 40 项未提交改动，**不用于门禁**） |
| 干净库 | `cloudpivot_539dab1d`，由 `scripts/verify/erp_clean_db.sh` 用被测树 release 二进制自举：`schema_migrations(max|count) = 236|177`、`public` 表 112、`menu_type=2` 菜单 69 条 |
| 后端 | `http://127.0.0.1:18127`（`profile=test`，`jwt_auth=true`，被测树 `target/release/cloudpivot`） |
| 前端 | `http://127.0.0.1:5181`（vite dev，`BACKEND_ORIGIN=http://127.0.0.1:18127`，被测树 `frontend`） |
| 账号 | `admin`（SUPER_ADMIN）；干净库首登强制改密，按提示改成 `Admin@654321` 后才进工作台 |

## 2. 门禁（全部实跑，日志在本目录）

| 检查 | 命令 | 结果 | 日志 |
|------|------|------|------|
| 类型检查 | `pnpm typecheck`（vue-tsc --noEmit） | **0 error，exit=0** | `typecheck.txt` |
| 生产构建 | `pnpm build` | **exit=0，built in 36.94s**，产出 `budget-logic-*.js` / `mapping-*.js` / `report-*.js` 等 chunk | `build.txt` |
| **n08 菜单守卫** | `SYS_DB_URL=… cargo test --release --lib system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router -- --nocapture` | **`1 passed; 0 failed`**，打印 `[N08] 90 条菜单路径全部能在前端路由里解析 ✓` | `n08-cargo-test.txt` |

> `cargo test` 用的是 `/root/9a6e3d35-target`（依赖已缓存），在被测树上 `--release --lib` 编译 4m28s 后执行；非 `--release` 会在干净树上重编全量依赖，不影响结论。

## 3. n08 的失败清单（修前 → 修后）

修前（HEAD `64b73fb`，来自 `docs/evidence/20261104-testfix-parallel/0d7c51f8/run1.log.txt`）6 条：

```
资产卡片       /business/erp/asset
资产分类       /business/erp/asset-category
预算编制       /business/erp/budget
预算科目映射   /business/erp/budget/mapping
预算执行       /business/erp/budget/report
折旧计提       /business/erp/depreciation
```

修后（本卡复算，规则与用例逐字一致：`router/index.ts` 里 `path: '…'` → `/…`，与 `sys_menu.menu_type=2` 的 path 全等比较）：`sys_menu` 69 条菜单 path，**缺失 0 条**（`n08-recalc.txt`）：

```
router registered paths: 90
sys_menu menu_type=2 rows: 69
MISSING: 0
-- budget 三条 --
/business/erp/budget: OK
/business/erp/budget/mapping: OK
/business/erp/budget/report: OK
```

## 4. 三条路由（`frontend/src/router/index.ts`，与 `sys_menu` 逐字一致）

| 菜单 path | 组件 | 权限码 | 浏览器实测 |
|-----------|------|--------|-----------|
| `/business/erp/budget` | `views/business/erp/budget/index.vue` | `business:erp:budget:list` | `01-budget-list.png` |
| `/business/erp/budget/mapping` | `views/business/erp/budget/mapping.vue` | `business:erp:budget:mapping` | `02-budget-mapping.png` |
| `/business/erp/budget/report` | `views/business/erp/budget/report.vue` | `business:erp:budget:report` | `03-budget-report.png` |

## 5. 真机取图（headless Chrome + CDP，每条路由整页加载）

每页都装同一套探针（`window.onerror` / `unhandledrejection` / `console.error` / `fetch`+`XHR` 的 ≥400 响应），整页加载后读数：

| 路由 | `document.title` | 控制台错误 | ≥400 响应 | 页面可见内容 |
|------|------------------|-----------|-----------|--------------|
| `/business/erp/budget` | 预算编制 - 云枢 CloudPivot | **0** | **0** | 工具栏（新建预算/刷新/年度/部门/状态/编制-调整/重置）+ 表头（预算单号 / 部门·年度 / 预算科目 / 年度额度 / 策略 / 版本·类型…）+ 空态「暂无数据 / 共 0 条数据」 |
| `/business/erp/budget/mapping` | 预算科目映射 - 云枢 CloudPivot | **0** | **0** | 工具栏（新建映射/刷新/类型/状态）+ 表头（类型 / 映射目标 / 预算科目 / 部门覆盖 / 优先级 / 状态 / 备注 / 操作）+ 说明「命中顺序：物料 > 仓库 / 物料类型（比优先级）> 全公司兜底」 |
| `/business/erp/budget/report` | 预算执行 - 云枢 CloudPivot | **0** | **0** | 6 张汇总卡（额度合计 / 占用中 / 已执行 / 已用合计 / 可用合计 / 全公司执行率）+ 年度·部门·关键词筛选 + 空态「2026 年还没有生效（ACTIVE）的预算」 |

截图里列表为空是**预期**：干净库没有任何预算数据（`cloudpivot_539dab1d` 只有种子菜单/角色，0 条 `biz_budget`）。带数据的完整业务链路截图（新建→提交→审批→生效→报表→映射拦截）见 `t_42d844c0` 的证据 `docs/evidence/20261105-t42d844c0-budget-pages/`（11 张）。

## 6. 复现

```bash
# 1) 干净树 + 干净库（被测提交）
git -C /opt/cloudpivot worktree add /root/t_539dab1d-tree df74a5f
cd /root/t_539dab1d-tree/backend && CARGO_TARGET_DIR=/root/t_539dab1d-target cargo build --release --bin cloudpivot
ERP_DB=cloudpivot_539dab1d RECREATE=1 REPO=/root/t_539dab1d-tree \
  BIN=/root/t_539dab1d-target/release/cloudpivot PORT=18127 \
  bash /root/t_539dab1d-tree/scripts/verify/erp_clean_db.sh

# 2) n08（预期 1 passed）
cd /root/t_539dab1d-tree/backend
SYS_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_539dab1d' \
  CARGO_TARGET_DIR=/root/9a6e3d35-target cargo test --release --lib \
  system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router -- --nocapture

# 3) 页面（干净库首登要改密）
cd /root/t_539dab1d-tree/frontend && BACKEND_ORIGIN=http://127.0.0.1:18127 pnpm dev --port 5181
```

## 7. 已知、不属本卡

- `t_42d844c0` 记录的 `MATERIAL_TYPE` 映射存不进去（`budget_dto::validate_mapping` 要 `ref_id`、`budget_model::resolve_for_po` 只比 `ref_code`）：页面已用 `mappingSaveBlocker` 显式拦住并给原因，仍需一张后端卡修 `validate_mapping`。
- 登录瞬间的 `403 /api/system/auth/preference`：既有行为（本次三条预算路由加载期间均为 0 个 ≥400）。
- `admin` 在干净库首登强制改密：本卡为登录取图把该演示库 admin 口令改为 `Admin@654321`（只动演示库数据，未改代码/种子）。
