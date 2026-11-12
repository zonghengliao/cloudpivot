# HR-38 · 迁移 V0394 + V0395（薪资单 / 调薪 / 离职 三套审批流种子）

卡：t_37006311（HR-38【HR】迁移 V0385，设计号 → 实落 **V0394 + V0395**）
设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑨ / §7.1 / §8.1

## 交付物

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0394__hr_salary_flow.up.sql` | `hr_salary` 流程：定义 + v1 版本 + 图快照（6 节点 / 6 边）+ 4 层守卫 + `DO` 自检 |
| `backend/migrations/business/V0394__hr_salary_flow.down.sql` | 回滚：3 道安全阀（HR_SALARY 实例 / 已过账薪资单 / 额外版本）+ 自底向上删 |
| `backend/migrations/business/V0395__hr_adjust_resign_flow.up.sql` | `hr_salary_adjust`（5/4）+ `hr_employee_resign`（6/6）两套，各自守卫与自检 |
| `backend/migrations/business/V0395__hr_adjust_resign_flow.down.sql` | 回滚：安全阀 + 两套各自自底向上删 |
| `backend/tests/hr_38_salary_adjust_resign_flow_seed.rs` | 16 条静态用例（文件解析，不连库，`cargo test` 必跑） |
| `backend/tests/hr_38_salary_adjust_resign_flow_seed_db.rs` | 5 条真库用例（`ERP_DB_URL` 开关） |

## 三套流程（实测形态）

| flow_code | biz_type | 拓扑 | 节点/边 | graph_hash |
|---|---|---|---|---|
| `hr_salary` | `HR_SALARY` | 人事经理 → 财务会签（COUNTERSIGN + 财务经理/财务总监，ALL）→（应发 ≥ 50 万）总经理 → END | 6/6 | `0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0` |
| `hr_salary_adjust` | `HR_SALARY_ADJUST` | 直属上级 → 人事经理 → 财务经理 → END | 5/4 | `a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171` |
| `hr_employee_resign` | `HR_EMPLOYEE_RESIGN` | 直属上级 → 人事经理 →（关键岗位）总经理 → END | 6/6 | `ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496` |

三条都 `PUBLISHED` + `is_default` + `is_system` + `allow_auto_approve = FALSE`；
审批节点一律显式 `ANY`（除了会签那一个 `ALL`，绑两个岗位角色）；`INITIATOR_SUPERIOR` 不落枚举行。

## 复现命令

```bash
# ① 真库演练（建在 145，跑完自动 DROP）：69 条断言
. /root/.hermes/profiles/backend-developer/cache/scratch/hr38_db.env   # NIK_DATABASE_*
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
bash docs/evidence/2026-11-12-hr38-salary-flows-v0394-v0395/01_drill.sh

# ② 真库用例（建 hr38_drill → 回放 317 条迁移 → cargo test → DROP）
bash docs/evidence/2026-11-12-hr38-salary-flows-v0394-v0395/02_db_tests.sh

# ③ 静态用例（不连库）
cd backend && cargo test --test hr_38_salary_adjust_resign_flow_seed -- --test-threads=1
```

## 实测结论（本目录 `.out` 为原始输出）

- `01_drill.out`：**69 条断言 0 失败**，覆盖 up → 幂等 → hash 库内重算 → down → 幂等 → 重放，
  以及 4 层守卫的反证（同名 flow_code / 同 biz_type 默认流 / 角色缺失 / 版本递增）
  与 down 安全阀；最后 `hr38_flow_drill` 已 DROP（`pg_database` 复查 0）。
- `02_db_tests.out`：5 条真库用例 **5 passed / 0 failed**（`ERP_DB_URL=…/hr38_drill`），
  含 `load_graph` → `validate()` 0 问题、`graph_hash()` 三方对账、条件边在引擎求值器上
  按 49.9999 万 / 50 万 / 80 万与「关键岗位 true/false」正确分流；库已 DROP。
- `03_static_tests.out`：16 条静态用例 **16 passed / 0 failed**。

## 契约（交给后续卡）

1. 薪资单提交审批（HR-39/HR-42 `/salary/{id}/submit`）必须把 `form["gross_total_num"]`
   冻结成 **JSON number**（不是字符串）—— 否则条件求值 `TypeMismatch`，大额工资静默绕过总经理。
2. 离职提交审批必须把 `form["is_key_position"]` 冻结成 **JSON boolean** —— 字符串 `"true"`
   与布尔 `true` 不相等，会静默走兜底边（关键岗位离职没有总经理签字）。
3. 调薪 / 离职**没有独立单表**，审批实例 `biz_id` = `biz_employee.id`（`FlowBizType::doc_kind()` 已定稿）。
