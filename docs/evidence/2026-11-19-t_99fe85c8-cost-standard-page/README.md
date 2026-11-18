# M5-09 · 标准成本：维护页 CRUD · 批量导入 — 证据包

> 卡：`t_99fe85c8`（M5 成本批第 9 张）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 演练库：`cp_m509_mig`（空库首启 → 全量迁移 **396** 条 / 最大 **V0464**）+ `cp_m509_import`（真库用例，跑完已 drop）
> 上游：M5-08（`t_bda3bb6b`，8 条版本表接口 + V0463 护栏）—— 本卡**零新表、零新码**，只在它上面补「导入」与「页面」

## 一、交付了什么（1 条菜单迁移 + 1 个 Rust 模块 + 2 条接口 + 1 个页面 + 1 个逻辑层 + 1 个单测文件）

| 层 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0464__cost_standard_menu.{up,down}.sql` | 只碰 `sys_menu` / `sys_role_menu`：页面行（sort 72、icon `chart-line`、读码复用 `cost-method:list`）+ 派生授权（持有读码的角色 + 超管）+ 6 条自检；幂等（up×2 / down×2 / up 实测） |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_import.rs` | **批量导入编排**：9 列模板 / 逐行纯校验 / 组内卡头一致性 / 引用一次批量查库 / 期间占用预检 / **单事务**逐组写入（复用 `handler::create_in_tx`）/ 模板生成 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_handler.rs` | +2 条路由：`POST /cost-standard/import`（`cost-method:create`）、`GET /cost-standard/import-template`（`cost-method:list`）；`create_in_tx` 改 `pub(crate)` 供导入复用 |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_model.rs` | +1 个取数原语 `materials_by_codes`（按编码批量取物料，`IN` 分片 500，不做 N+1） |
| 后端 | `backend/app/src/business/erp/cost/cost_standard_import_db_tests.rs` | 6 条真库用例（`ERP_DB_URL` 开关） |
| 前端 | `frontend/src/api/business/erp/cost-standard-kinds.ts` · `cost-standard.ts` | 类型层 + 10 条接口封装（含 multipart 导入、xlsx 模板下载） |
| 前端 | `frontend/src/views/business/erp/cost-standard/cost-standard-logic.ts` | **纯逻辑**（定点算数 / 表单校验 / 入参拼装 / 锁改判据 / 导入话术），被 25 条单测钉住 |
| 前端 | `frontend/src/views/business/erp/cost-standard/index.vue` | 维护页：列表（期间 / 状态 / 关键字）+ 新增/编辑抽屉（卡头 + 标准 BOM 子表 + 三要素预览）+ 生效/失效 + 删除 + **批量导入对话框**（模板下载 / 逐行错误表） |
| 前端 | `frontend/src/router/index.ts` | 注册 `business/erp/cost-standard` 路由（漏了 = 点菜单 404，`system::menu::tests::n08` 会红） |
| 前端 | `frontend/src/constants/error-messages.ts` | 补 `66101–66110` 的人话文案（**修掉一条既存红灯**：`error-messages.test.ts` 的「66xxx 常量必须都有文案」在本卡之前就红 —— M5-01~M5-08 加了码没加文案） |
| 前端 | `frontend/tests/unit/erp-cost-standard.test.ts` | 25 条纯逻辑单测 |

### 2 条新接口（权限码全部复用，零新增）

| # | 方法 | 路径（前缀 `/api`） | 码 | 说明 |
|---|---|---|---|---|
| 9 | POST | `/business/erp/cost-standard/import` | `…:cost-method:create` | xlsx 批量导入（**全有或全无**） |
| 10 | GET | `/business/erp/cost-standard/import-template` | `…:cost-method:list` | 9 列模板下载（数据 / 字段说明 / 填写须知 三个 sheet） |

## 二、口径（决定，不是待办）

1. **「9 列一张表」而不是「两张 sheet」**：一组「物料编码 + 期间」= 一版标准成本，组内一行 = 一条标准 BOM 明细。
   拆 sheet 会让两个键在两张表里各写一遍，对不上时没人知道哪张对；一张表只有一处真相。
2. **组内卡头必须逐字一致**（标准工时 / 标准工资率 / 标准制造费用 / 备注）：不一致**报错**而不是取第一行 ——
   静默取一行会让同一张卡出现两种口径，而这四列正是差异分析的分母。数值列比较前先归一（`2.50` 与 `2.5` 不算不同）。
3. **明细三列要么全空、要么全填**：留空 ≠ 0。填了耗用物料却把单价留空 ⇒ 该行报错（按 0 补会把材料成本算少且没有红灯）。
4. **导入只新建，不改不删**：唯一的外部约束是「同一物料启用版本不许重叠」（M5-08 的 66108）。想改某期标准，
   正路是维护页改未生效版 / 先「失效」旧版再导 / 收口旧版停止日 —— 报文里把三条出路原样给出。
5. **全有或全无靠「一个事务」**：写库不在导入侧另写 SQL，而是复用一个事务里的 `handler::create_in_tx`
   （锁物料 → 判重叠 → 取版本号 → 写卡头 + 明细）。任一组失败 ⇒ 整批回滚，**已写进去的前几组也撤回**。
   真库用例 `write_all_rolls_back_every_group_on_failure` 直接构造「两组必然撞重叠」把这条路径逼出来。
6. **前端金额一律「整数微元」（BigInt，6 位小数）**：`料 = Σ round(用量×单价,6)`、`工 = round(工时×费率,6)`、
   `总 = 料+工+费`，与后端 `StandardSpec::amounts` 及库层 `ck_biz_cost_std_*` 同口径、半远离零。
   单测的期望值直接取自 M5-08 真库用例（17.166666 / 77.777778 / 107.290122），前后端两套实现互相印证。
7. **已生效版本锁改**：`effective_from < 今天` 只能改停止日 / 状态 / 备注。前端 `toUpdatePayload` 在这种情况下
   **只发这三样** —— 发一份 trim 过的数字，一旦小数位与库里不同就会被「重算金额必须等于库金额」那道闸判成
   静默重算历史金额而整单拒绝。
8. **导入失败不说「失败」**：说「整批未入库（全有或全无）：N 行有 M 处错误，一条都没写入」。半批导入会让
   标准成本少一版而没人发现（ARCH-9 D-7「不猜数据」）。

## 三、真跑的验收（每项可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_migration_and_menu.out` | 空库首启 **396** 个迁移；V0464 菜单行形态（path / component / sort 72 / icon `chart-line` / title「标准成本」/ auth_code `…cost-method:list` / 父目录 `/business/erp/cost`）；授权角色 3 个（FINANCE_DIRECTOR / FINANCE_MANAGER / SUPER_ADMIN，派生 + 超管）；同父 sort 56…72 无冲突 | ✅ |
| `02_cost_standard_tests.out` | `ERP_DB_URL=…/cp_m509_mig cargo test --lib business::erp::cost::cost_standard`：**66 passed / 0 failed**（本卡新增 20 纯函数 + 6 真库；其余是 M5-08 的用例） | ✅ |
| `02b_permission_audit.out` | `system::menu::permission_audit_tests`：**8 passed**（含 N16 裸路由 / N21 静态路由不被兄弟模板遮蔽 —— 证明 `/cost-standard/import` 没被 `/{id}` 吃掉） | ✅ |
| `03_menu_invariants.out` | `scripts/verify/menu_invariants.sh cp_m509_mig`：**8 条全绿**（菜单 570 ∈ 400–580；icon 132 条全部命中 TDesign 图标库） | ✅ |
| `04_http_smoke.out` | 真实例 `127.0.0.1:18259`（库 `cp_m509_mig`）上跑 `smoke_import.py`：**57 PASS / 0 FAIL** | ✅ |
| `05_migration_idempotent.out` | V0464 `up`×2 / `down`×2 / `up` 各 0 ERROR；菜单行数 1→1 / 0→0 / 1；授权行 3 | ✅ |
| `05_cost_domain_regression.out` | `cargo test --lib business::erp::cost::`（**纯逻辑档**，不连库）：**890 passed / 0 failed** | ✅ |
| `06_fmt.out` / `07_clippy.out` | `cargo fmt --check` / `cargo clippy -p cloudpivot --lib --tests -D warnings` | rc=0 |
| `08_frontend_typecheck.out` | `pnpm typecheck`（`vue-tsc --noEmit`） | rc=0 |
| `09_frontend_unit.out` | `pnpm test:unit`：**2415 passed / 0 failed**（含本卡 25 条；本卡之前是 2414/1 FAIL —— 那条红灯由本卡补 `66101–66110` 文案修掉） | ✅ |
| `10_frontend_build.out` | `pnpm build`（typecheck + vite build） | rc=0 |

## 四、57 条 HTTP 冒烟覆盖了什么（`smoke_import.py`）

| 组 | 断言要点 |
|---|---|
| ① 模板 | 200 + xlsx PK 魔数 + 8.5KB（三个 sheet） |
| ② 导入成功 | 2 组 3 行 → `success=3 / versions_created=2`；列表与详情的料 / 工 / 总 = 17.166666 / 77.777778 / 107.290122；生效区间 = 整期；明细行号 1/2 且带耗用物料编码；**无 BOM 的组也建得出来** |
| ③ 重复导入 | `success=0`，3 行全被点名，报文给「已经有启用版本 v1」+「三条出路」，库里不多行 |
| ④ 坏行 | 物料编码不存在 ⇒ 只报第 3 行、列名「物料编码」；**好的一组也一条没入库** |
| ⑤ 卡头不一致 | 同组费率 31.111111 vs 50 ⇒ 整批拒、列名「标准工时」、报文「必须逐字一致」 |
| ⑥ 明细半填 | 填了耗用物料、单价留空 ⇒ 列名「标准单价」 |
| ⑦ 结构错 | 非 xlsx ⇒ 400 / `60020`；缺 `file` 字段 ⇒ 400 / `10001` |
| ⑧ 维护页 CRUD（页面按钮的同一批接口） | 建（料 6 / 工 30 / 总 41）→ 改（工 37.5）→ 失效（带 `warnings` + 备注留痕 `[失效 2026-11-19]`）→ 再生效 → 删 → 详情 404 |
| ⑨ 删除护栏 | 过去期间的版本删 ⇒ **409 + 66110**（报文指向「改成失效」）；将来期间的版本可删（未生效过不算历史凭据） |
| ⑩ 清场 | 按物料 id 删标准成本行 + 删夹具物料，`biz_cost_standard` 按 TAG 零残留 |

## 五、给下游卡的接缝

- **M5-10（发布 / 审核后生效）**：本卡页面已有建 / 改 / 失效 / 删除；发布审批建议加 `publish_state`，
  别复用 `status=1`（现有语义是「参与期间取数」，混用会让「审批中」在差异分析里算成生效）。
- **M5-11…M5-16（差异）**：取标准走 `GET /cost-standard/effective?material_id=&period_code=`，并检查
  `switch_in_period`（期内换版目前按期末一套口径，分段属 M5-31）。
- **M5-18（前端标准成本页）**：本卡已交付「维护页」；分析页可直接复用 `cost-standard-kinds.ts` 的类型与
  `cost-standard-logic.ts::windowText / materialLabel / amountsOf`，别另起一套金额格式化。
- **导入模板再加列**：改 `cost_standard_import::COLUMNS` + `GUIDE_LINES`，模板与校验会一起变；
  前端 `IMPORT_GUIDE` 是同一份口径的中文说明（改一处要同步另一处）。

## 六、已知边界（本卡刻意不做）

1. **不做 BOM 展开**：标准 BOM 只有一层耗用物料（V0374 的设计），多层展开属成本还原（M5-26~M5-30）。
2. **导入不做「覆盖」语义**：同物料同期已有启用版本一律拒绝（不自动失效旧版、不自动叠加版本）。
   要覆盖请人在维护页点「失效」再导 —— 自动覆盖会让历史差异失去依据。
3. **期间固定整期**：导入的生效区间 = 期初 ~ 期末（不支持「从某天起」）；跨期中途改口径请用维护页的
   「自定义日期」新增一版。
4. **页面未做浏览器 E2E**：本卡验收 = 类型检查 / 构建 / 25 条纯逻辑单测 + 后端 57 条 HTTP 冒烟；
   Playwright 用例属 M5-34（标准成本 E2E）。
5. **模板库 `cloudpivot_test` 仍停在 V0404**：迁移链验证用空库首启的 `cp_m509_mig`（已 drop）；模板库陈旧非本卡范围。
