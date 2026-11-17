# M3-19【M3·销售】销售返利：前端政策与结算页 — 证据包

卡片：`t_be03f4e7`（后端 M3-15 政策 / M3-16 结算 / M3-17 开票，本卡只做前端两页 + 菜单收尾迁移）
日期：2026-11-17 · 机器：99（开发机，PG 连 `172.18.10.145:5432`，用户 `cloudpivot`）

## 一句话结论

返利模块的前端落地：**一个侧边栏节点「销售返利」、两个 tab**（返利政策 / 返利结算），
API 层 11 个接口、两个纯逻辑模块、22 条单测；菜单收尾用**只追加**的 `V0443` 补齐
`path / component / is_visible / title`（**零新增菜单行** —— 菜单总数实测 540 正顶
`menu_invariants` ⑤ 的上限，见下「决策」）。

## 交付物

| 文件 | 说明 |
|---|---|
| `backend/app/migrations/business/V0443__sales_rebate_menu_page.{up,down}.sql` | 菜单收尾（path `/business/erp/rebate-policy` + component `business/erp/rebate/index` + title「销售返利」）；幂等 + 可回滚 + 自检 7 个按钮仍在 |
| `frontend/src/api/business/erp/rebate.ts` | 政策 5 接口 + 结算 6 接口（字段逐字抄 `RebatePolicyRes` / `RebateSettleRes` / `RebatePreviewRes`） |
| `frontend/src/views/business/erp/rebate/policy-logic.ts` | 三种规则类型的形状校验（与后端 `checked_rules` 同序）、表单整形、入参拼装、展示文案 |
| `frontend/src/views/business/erp/rebate/settle-logic.ts` | 期间码（月度/季度互配）、试算/跑批文案、状态机、展示 |
| `frontend/src/views/business/erp/rebate/index.vue` | 两 tab 页：政策列表 + 新建/编辑弹窗（含阶梯档位编辑器）/ 结算列表 + 试算计算弹窗 + 整期跑批 + 详情抽屉 + 重算/作废 |
| `frontend/src/router/index.ts` | 路由 `business/erp/rebate-policy`（n08 与 `sys_menu.path` 整串比对） |
| `frontend/src/constants/error-messages.ts` | 补 `66089~66095` 七条人话（这 7 个码由 M3-15/16/17 后端引入，前端表此前缺失 → `error-messages.test.ts` 存量红，本卡收口） |
| `frontend/tests/unit/erp-rebate.test.ts` | 22 条纯逻辑单测（形状矩阵 / 期间码矩阵 / 入参 / 文案 / 源码即契约） |

## 决策（都是「本卡为什么这么做」的记录）

1. **一个页面节点两个 tab，而不是两个菜单行。**
   `sys_menu` 里本模块的 7 个按钮码（政策 3 + 结算 3 + 开票 1）本来就都挂在 V0438 的页面节点
   `…4380` 下；而菜单总数实测 540 正好顶 `menu_invariants` ⑤ 的 `[400,540]`，
   再插一行 `menu_type=2` 立刻 541 判红。⇒ 结算做成同页第二个 tab，权限边界不变
   （仍是 8 个独立按钮码，tab 显隐前端按 `…:list` 判、后端零引用）。
2. **title 从「返利政策」收敛为「销售返利」。** title 是侧栏显示名，一行承载两块时叫「政策」会误导；
   `name`（唯一标识 / 权限关联）不动。
3. **不做「生成红字发票」按钮。** M3-17 的接口与权限码（`…:invoice`）已就绪，但票面预览 + 凭证口径确认
   是独立交互；本卡只**只读展示** `invoice_no` / `settled_at`（已另立卡跟进，见卡内汇报）。
4. **补 7 条错误码人话。** 本页会把 `66089~66095` 原样展示给用户，而 `error-messages.test.ts`
   要求后端每条 66xxx 都有前端翻译 —— 之前是存量红，本卡收口（见 `03b_*` 证前、`03_*` 证后）。

## 验收证据（全部原始输出）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_unit_erp_rebate.txt` | `tsx --test tests/unit/erp-rebate.test.ts` | **22/22 pass** |
| `02_unit_error_messages.txt` | `tsx --test tests/unit/error-messages.test.ts` | **37/37 pass**（补 66089–66095 后） |
| `03_unit_full_suite.txt` | 全量 `pnpm test:unit` | **2216/2216 pass，0 fail** |
| `03b_unit_full_suite_before_error_code_fix.txt` | 同一套件（补错误码之前） | 2215 pass / **1 fail**（就是 66089–66095 缺失） |
| `04_typecheck.txt` | `pnpm typecheck`（vue-tsc --noEmit） | **0 错误** |
| `05_build.txt` | `pnpm build`（含 typecheck） | **✓ built in 1m 4s**（EXIT=0），dist 内有本页 chunk |
| `06_migration_idempotent_and_rollback.txt` | V0443 首跑 / 重放 / down / 再 up | 首跑 1+1+1 行；**重放 0 行**；down 还原成 V0438 终态；再 up 复原 |
| `07_menu_invariants.txt` | `menu_invariants.sh cp_be03f4e7_dev` | **8/8 全绿，菜单总数 540 未涨** |
| `08_n08_real_db.txt` | `system::menu::tests::n08`（真库） | **138 条前端路由全部可解析**，含本卡 `/business/erp/rebate-policy` |
| `09_n08_mutation_proof.txt` | 把本卡 path 改成 `…-policyMUTATION` → n08 判红并点名；还原后复绿 | 反证成立（证明 n08 真读本卡那一行） |
| `10_permission_audit_tests.txt` | `system::menu::permission_audit_tests`（真库） | **8/8 pass**（N14/N15/N16/N18/N19/N20/N21 + N17 变异自检） |
| `11_perm_menu_gate.txt` | `perm_menu_gate.py`（闸⑥ 死权限码） | **PASS** |
| `12_shell_variant_gate.txt` | `shell-variant-report.cjs --baseline`（页面零壳类声明） | **无新增**（快照 6 条 = 当前 6 条；空态改用 `<StateView>`，不再本地声明 `.panel-empty*`） |
| `19_backend_migrate_v0443.txt` | 空库首启：真实迁移器把 V0443 应用上去（`schema_migrations` 记账 version=443 + NOTICE 三行 + 菜单终态） | 通过（**不是手工 psql**） |
| `17_browser_smoke.md` + `13~16/18_*.png` | **真浏览器冒烟**（真后端 :8023 + 真库 + 真前端 :5173）：登录 → 销售返利页（一个侧栏项两 tab）→ 新建按量政策 → 切阶梯返利看档位编辑器 → 结算试算（两条路径：未覆盖整期 `66091` 原文展示 / 覆盖后 0.00 + 警告 + 政策档位）→ 计算落库 `RB-20261117-000001` → 详情抽屉（政策快照档位表）→ 整期跑批（`已有跳过 1 张`，幂等） | 全链路走通 |

后端改动只有一条 SQL 迁移（无 Rust 代码改动）：`cargo fmt --check` 0 diff、
`cargo clippy --lib -- -D warnings` EXIT=0。

## 演练库（用完即删）

| 库 | 来源 | 用途 |
|---|---|---|
| `cp_be03f4e7_dev` | `pg_dump cloudpivot_dev`（540 菜单、V0442 状态） | menu_invariants 8/8、n08 真库 + 变异反证、permission_audit_tests、perm_menu_gate |
| `cp_be03f4e7_menu` | `createdb -T cloudpivot_test_v0442a` | 首轮幂等 / down-up 反证（该克隆库自带 9 条历史孤儿 parent，与本卡无关，只在 `cp_be03f4e7_dev` 上出 8/8） |
| `cp_be03f4e7_menu2` | 同上 | `06_*` 的完整四步证据 |
| `cp_be03f4e7_e2e` | **空库** | 浏览器冒烟用：首启自动跑 432 条迁移（含 V0443），admin 口令由 `NIK_ADMIN_PASSWORD` 注入 |

三个演练库与冒烟库都已 `dropdb`（`pg_database like 'cp_be03f4e7%'` = 0）。

**环境副作用（如实记录）**：为了让后端在 99 启动，`apt-get install -y redis-server` 装了一次
（dev profile 的安全守卫要求 `[cache] type = redis`，认证吊销依赖它）。冒烟结束后我
`redis-cli shutdown nosave` 过，但 apt 装的 systemd unit 会把它拉回来（实测 pid 变了）；
`systemctl stop/disable redis-server` 属「停系统服务」被工具闸拦下，需人工批准 —— 所以
本机现在**有一个 loopback 上的 redis-server 在跑**。要彻底复原：`systemctl disable --now redis-server`。

## 复现

```bash
# ① 单测 / 类型 / 构建（前端）
cd frontend && pnpm exec tsx --test tests/unit/erp-rebate.test.ts
pnpm typecheck && pnpm build

# ② 迁移（幂等 + 回滚）
export PGPASSWORD=<pg>; HOST=172.18.10.145
createdb -h $HOST -U cloudpivot -T cloudpivot_test_v0442a cp_be03f4e7_menu2
psql -h $HOST -U cloudpivot -d cp_be03f4e7_menu2 -v ON_ERROR_STOP=1 -f backend/app/migrations/business/V0443__sales_rebate_menu_page.up.sql
psql -h $HOST -U cloudpivot -d cp_be03f4e7_menu2 -v ON_ERROR_STOP=1 -f backend/app/migrations/business/V0443__sales_rebate_menu_page.down.sql

# ③ 菜单闸 + n08（真库）
PGHOST=$HOST PGUSER=cloudpivot bash scripts/verify/menu_invariants.sh cp_be03f4e7_dev
cd backend && SYS_DB_URL="postgres://cloudpivot:<pg>@$HOST:5432/cp_be03f4e7_dev" \
  cargo test --lib n08 -- --test-threads=1 --nocapture
```

## 未做 / 交接

- **红字发票 UI**（票面试算 + 生成 + 凭证展示）：M3-17 的接口/权限码已在，本卡按「不夹带」未做，已另立卡跟进。
- 结算页数据范围（销售员只看自己的）未在 rebate 模块做 —— 后端 `rebate_settlement` 列表当前是全量口径
  （与佣金 `commission-settle:list` 同状态），要收窄需后端加 scope 裁剪。
