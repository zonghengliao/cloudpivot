# HR-19 考勤期间（`biz_attendance_period`）落地证据

卡面：`attendance_period` model/handler（建 / 改 / 状态 / 汇总重算），依赖 HR-09。
设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.1 ④（DDL）、§4.2.3（状态机）、§5.2 #18~#20。

## 交付了什么

| 层 | 文件 | 内容 |
|---|---|---|
| 契约 | `backend/src/business/hr/attendance_period_dto.rs` | 入参 / 回执 / 纯校验（期间码 `yyyy-MM` 窄口径、区间跨度 ≤31 天、状态域与解锁原因）+ 列表筛选 + 7 条单测 |
| 数据 | `backend/src/business/hr/attendance_period_model.rs` | 唯一 SQL 出口：建 / 改 / 状态 CAS / 软删 / 分页 / **汇总全量重算** + 撞码与 CHECK 翻译 + 7 条单测 |
| 接口 | `backend/src/business/hr/attendance_period_handler.rs` | 6 条路由（列表 / 建 / 改 / 状态 / 重算 / 删）+ 每条写操作一个事务且先 `FOR UPDATE` 锁期间 + 2 条单测 |
| 真库 | `backend/src/business/hr/attendance_period_db_tests.rs` | 6 条真库用例（CRUD / 撞码 / 状态机 / 锁定守卫 / 重算幂等 / 删除守卫） |
| 冒烟 | `scripts/verify/hr/attendance_period_http_smoke.sh` | 48 条 HTTP 断言（真实例 + 真库） |
| 接线 | `hr/mod.rs`、`hr/handler.rs`（ROUTE_CONTRACT 46→49）、`shared/error_code.rs`（68009/68010）、`scripts/lint/scope_gate.py`（SKIP_MANIFEST 登记本卡测试文件） | — |

## 接口清单

| # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
|:--:|---|---|---|---|
| 18 | GET | `/business/hr/attendance/period/page` | `business:hr:attendance:list` | 列表（`keyword` / `status` 多选 / `year` / 分页） |
| 19 | POST | `/business/hr/attendance/period` | `business:hr:attendance:period` | 建期间（`period_code` + `period_start/end` + `remark`） |
| 20 | POST | `/business/hr/attendance/period/{id}/status` | `business:hr:attendance:lock` | 提交 / 锁定 / 解锁（`to_status` + `reason`） |
| ★ | PUT | `/business/hr/attendance/period/{id}` | `business:hr:attendance:period` | 改起止 / 备注（部分更新） |
| ★ | DELETE | `/business/hr/attendance/period/{id}` | `business:hr:attendance:period` | 软删（仅草稿 + 期间下无记录） |
| ★ | POST | `/business/hr/attendance/period/{id}/recalc` | `business:hr:attendance:period` | 汇总重算（幂等，回执带 `before/after/changed`） |

★ = 设计 §5.2 未列、按卡面「期间 CRUD + 汇总重算」补的三条（偏差与理由见 handler 模块文档）。
三条**不新增权限码**（复用 `V0387` 已种下的 `attendance:period` / `attendance:list` / `attendance:lock`），
`contract_permissions_are_seeded_by_v0387` 仍是对菜单种子逐码对拍。

## 验收实测

| 验收项 | 证据 | 结果 |
|---|---|---|
| 期间 CRUD | `01_db_tests.out` ①⑥ + `02_http_smoke.out`（建→列表→改→删→重建） | 6/6 真库用例通过；HTTP 48/48 断言通过 |
| 状态机正确 | `01_db_tests.out` ③ + `02_http_smoke.out` 状态段 | `DRAFT→SUBMITTED→LOCKED→DRAFT` 三边全通；非法边（`DRAFT→LOCKED`、`LOCKED→SUBMITTED`）409；解锁缺原因 400；每次成功流转都写 `biz_status_log`（建单 + 3 次 = 4 行） |
| 汇总重算幂等 | `01_db_tests.out` ⑤ | 首次 `changed=true`（0 → 44.00/42.50/7.00）；第二次 `changed=false` 且四列逐列相等；记录改动后重算 `changed=true` 并跟上（43.00） |
| 锁定后写保护 | `01_db_tests.out` ④ + `02_http_smoke.out` | 改 / 删 / 重算一律 `409 + 68010`，报文点名「先解锁」；库层 `RECALC_SQL` 也带 `status <> 'LOCKED'` |
| 撞码报文 | `01_db_tests.out` ② + 冒烟 | `409 + 68009`，报文点名期间码 + 「一个月只能有一批」+ 下一步 |
| 门禁 | `03_gates.out` | `fmt --check` / `clippy -D warnings` / `cargo test --lib`（5032 passed 0 failed）/ 前端 `typecheck` + `build` 全部 EXIT=0 |
| 残留 | `04_residue.out` | `hr19%` 库 0 个；18095 无监听；工作区只含本卡文件 |

## 关键实现取舍（都在代码注释里写明白了）

1. **汇总重算是全量覆盖，不是增量累加**：`UPDATE … FROM (SELECT count/sum … FROM biz_attendance_record WHERE period_id=$1 AND deleted_at IS NULL)`。
   全量覆盖天生幂等；增量累加会「重算两次翻倍」，且导入重试后永远收敛不回真值（只能改库修 = AGENTS.md 约定 11 的红线）。
2. **锁定连重算一起拦**：锁定期间是薪资试算的基线（设计 §4.2.3），记录冻结 ⇒ 快照也必须冻；守卫同时写在 handler 与 SQL 里。
3. **解密不清空 `locked_at/locked_by`**：四列是「最近一次动作」的审计信息，流转轨迹在 `biz_status_log`；出参另给 `locked: bool` 让前端别自己判状态。
4. **删是软删，且只允许草稿 + 无记录**：`biz_attendance_record.period_id` 是 `ON DELETE CASCADE`（硬删会连带抹记录），
   `biz_salary_detail.attendance_period_id` 还是 `RESTRICT`；软删保留数据，同时因唯一索引带 `WHERE deleted_at IS NULL` 立刻释放期间码，可重建同月。
5. **状态机不自己写 `match (from, to)`**：三条边唯一真相是 `state_machine::ATTENDANCE_PERIOD_EDGES`（HR-08 注册），handler 只锁行 → 判期间态 → `transition` → CAS 回写。

## 复现命令

```bash
# ① 真库用例（需一个已迁移的空库）
HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr19_card \
  cargo test --lib business::hr::attendance_period_db -- --test-threads=1 --nocapture

# ② HTTP 冒烟（需实例已起；脚本内置首次登录改密）
BASE=http://127.0.0.1:18095 ADMIN_PASSWORD='Admin@123456' \
  bash scripts/verify/hr/attendance_period_http_smoke.sh
```

## 遗留 / 发现（未在卡内自行处理）

- `state_machine::ATTENDANCE_PERIOD_EDGES` **没有 `SUBMITTED → DRAFT`**（HR-08 按设计图落的），所以「提交后发现建错月份」
  只能 锁定 → 解锁（带原因）回草稿，或删掉重来（草稿才可删）。若产品希望「提交后能直接撤回」，要改 HR-08 的边表 + 设计 §4.2.3，属另一张卡。
- 记录层「锁定后不许增删改」属 HR-23（把期间的写保护接到 `biz_attendance_record`），本卡只封期间头。
- `scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 仍有 40+ 条存量漏登记（HR-17 已上报），本卡只补自己那一条；
  闸② 因此在本次提交前后都是红的（与改动无关），建议单开收口卡。
