# COST-B2-14 · 成本对象「本期产量与在制」录入（output 接口 + 前端小面板）证据

卡：`t_61d337b4`（ARCH-9 §4.5.1 数据源矩阵「产量 / 进度」行 · §8.3 R-4 已知风险 · 拆卡清单 §8.1 B2-14）
上游：B2-11（calc 第 6 段）落地时具名报告的缺口 —— 三个数**没有任何录入路径**，库里恒为 0，
calc 第 6 段逻辑齐全但真实客户一跑就撞 `66067`。

## 1. 交付内容

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/cost_object_dto.rs` | `CostObjectOutputReq` / `OutputSpec` / `spec_from_output`（值域 + 在制>0 必须给进度）/ `output_warnings`（超产、全 0 有成本）/ `CostObjectOutputRes`；详情追加 `output_filled_by` / `output_filled_at` |
| `backend/src/business/erp/cost_object_handler.rs` | `set_output`（404/409/400/409+66070 四道闸 → 同事务写三列 + 变更日志 → 读库回单）、`detail_object`（HTTP 与真库用例共用入口）、`PUT /business/erp/cost-object/{id}/output` 路由（权限复用 `cost-object:update`） |
| `backend/src/business/erp/cost_object_model.rs` | `settled_periods`（SETTLED/CLOSED 且有计算单 ⇒ 只读）、`apply_output`（CAS `WHERE status='OPEN'`）、`latest_output_fill`（R-4 的填写人 + 时间） |
| `backend/src/business/changelog/diff.rs` | 登记 `biz_cost_object` 白名单三列（`completed_qty` / `wip_qty` / `completion_pct`）+ 单测钉住 |
| `backend/src/business/changelog/model.rs` | `latest_for_object`（按对象取最近一条审计的操作人 + 时间，走 `idx_sys_change_log_object`） |
| `backend/src/business/erp/cost_object_db_tests.rs` | 追加 6 条真库用例（⑨–⑭），原 8 条（含 B1-8 的 5 个接口用例）保持绿 |
| `frontend/src/api/business/erp/cost-object-kinds.ts` | `CostObjectOutputPayload` / `CostObjectOutputResult` / `COST_OBJECT_PERIOD_LOCKED_CODE`；详情加两个 `output_filled_*` |
| `frontend/src/api/business/erp/cost-object.ts` | `saveCostObjectOutput`（`PUT /api/business/erp/cost-object/{id}/output`，**带 `/api` 前缀**） |
| `frontend/src/views/business/erp/cost-object/cost-object-logic.ts` | `outputFormFromDetail` / `validateOutputForm`（与后端同口径）/ `toOutputPayload` / `outputErrorText`（66070 补「去哪修」） |
| `frontend/src/views/business/erp/cost-object/index.vue` | 详情抽屉新增「本期产量与在制」小面板（三个数 + 保存 + 上次填写人/时间 + 提示） |
| `frontend/tests/unit/erp-cost-object.test.ts` | 追加 5 条纯逻辑单测（值域 / 在制>0 必须给进度 / 全量提交 / 错误文案 / 回填去尾零） |

**无迁移**：三列由 V0368/V0370 建好（`completed_qty NUMERIC(18,4)` / `wip_qty NUMERIC(18,4)` /
`completion_pct NUMERIC(5,2)`），一字未改（`git status backend/migrations` = 0 行）⇒ 卡面
「迁移幂等 + `.down.sql`」N/A。审计**不加三列快照**，写 `sys_change_log`（§4.6.3 末段一期口径）。

## 2. 验收（卡面逐条）

| 卡面要求 | 怎么证明 |
|---|---|
| `PUT /cost-object/{id}/output`（三字段 + 权限码） | 路由 `cost_object_output_api`（宏路径无 `/api`，前端 api 层写全 `/api`）；权限复用 `business:erp:cost-object:update`（单测 `output_route_reuses_update_permission` 钉住「没新开码」） |
| 对象存在且未软删 / 状态 `OPEN` / 期间已结算 409 / 在制>0 必须进度 / 超产只提示 | 真库用例 ⑩（值域 400）、⑫（已结算 409+66070）、⑬（关批后 409）、⑪（超产 warnings）；纯逻辑 6 条 |
| 审计留「填写人 + 时间」 | 真库用例 ⑨：`sys_change_log` 一条 UPDATE 带前后值、`operator_name` = 车间主任张三、`created_at` 有值；详情 `output_filled_by/at` 回显 |
| 前端小面板（三个数 + 保存） | `index.vue`「本期产量与在制」小节；`pnpm typecheck` / `pnpm build` / `pnpm test:unit` 全绿 |
| 回归：B1-8 的 5 个接口用例保持绿 | `cargo test --lib cost_object_db` 14/14 通过（原 8 条 + 新 6 条） |

## 3. 实测输出

见同目录 `01_unit_tests.out` / `02_real_db_tests.out` / `03_gates.out` / `04_residue.out`。

## 4. 判断与偏差（**要评审认一下**）

1. **权限码复用 `cost-object:update`**（卡面允许二选一）。新开 `cost-object:output` 要连带种
   `sys_permission` / `sys_menu`（= 迁移），本卡是「无迁移」卡；语义上「能改这张表的人」确实是
   同一批人。真要拆，等菜单权限重排时一起做（`all_permissions()` 仍是 5 个，单测钉住）。
2. **期间已结算的 409 复用 `66070`**（calc 第 6 段的「期间锁定」）。理由：用户动作完全相同
   （先红冲 / 撤回该期间批次再改数字），报文点名期间；`66072`/`66073` 已被 B3-1 占用，
   本卡不新增码值（也避免动 `error_code.rs` 这份多人共用的号段表）。
3. **「在制 > 0 但进度 0」在录入侧就 400**，不等 calc 才 409：判据与 `66067` 同一条
   （`wip_qty > 0 ⇒ completion_pct > 0`），报文直接点名「跑计算时会被 66067 拦下」。
4. **值域在录入侧先拦（含小数位）**：`NUMERIC(18,4)` / `NUMERIC(5,2)` 超位数时 PG 会**静默四舍五入**，
   用户以为存进去了；这里 400 点名「最多 4 / 2 位小数」。
5. **三个数没变 ⇒ 不写库、不写审计**（`changed=false`）：写 `updated_at` 会让「最后修改时间」骗人，
   写空 diff 会让审计表变操作流水（`changelog` 自己的口径也是 `Skipped`）。
6. **`completed_qty` 语义是累计**（卡面「别改」）：面板文案与接口文档都写「累计完工」，
   没引入「本期增量」的第二种读法。
7. **额外加了一条「三个数全 0 但有归集成本」的提示**（不拦）：这正是本卡要治的症状
   （客户不填 ⇒ 一跑就 66067），在录入这一步就把后果说清楚。
8. **`latest_for_object` 放在 changelog 模块**：门禁 C7 G1 规定 `sys_change_log` 只能由
   `business/changelog/` 访问，成本对象 model 只调它、不碰表名。
