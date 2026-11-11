# HR-11 证据：`employee_dto.rs`（`*_id/*_name` 成对 + `*_masked` 出参）

- 卡：`t_ce56282f`（ARCH-9-2 §8.1 批 2 / HR-11，依赖 HR-10）
- 时间：2026-11-12 03:0x CST
- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #1/#2、§5.4、§3.1 ①、§3.2 ⑥/③、§6.2 + `AGENTS.md` 约定 13

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/hr/employee_dto.rs`（新，1694 行含文档与单测） | 列表行 / 详情 / 合同 / 薪资标准 / `SensitiveMasked` 五组出参 + 6 张「码值 → 中文」表 + 打码纯函数 + 24 条单测 |
| `backend/src/business/hr/employee_dto_db_tests.rs`（新） | 3 条真库/形状用例：V0388 规则 = DTO 兜底常量、真库规则 → DTO 打码值、空规则集不退化 |
| `backend/src/business/hr/mod.rs`（+6） | 登记 `pub mod employee_dto` + `#[cfg(test)] mod employee_dto_db_tests` |

## 三条验收怎么验的

### ① 所有关联字段成对输出

成对规则写死为**同生同灭**（`code = None ⇒ name = None`），并由 5 条用例钉住：

| 对 | 来源 | 缺名时的口径 |
|---|---|---|
| `dept_id` / `dept_name` | `employee_model` 的 join | 部门已删 → `（部门已删除）`（不给空串） |
| `position_code` / `position_name` | 字典 `hr_position` | 回退码值 `WORKER` |
| `job_level` / `job_level_name` | 字典 `hr_job_level` | 回退码值 |
| `user_id` / `username` | join `sys_user` | 账号已删 → `（账号已删除）` |
| `education` / `education_name` | 字典 `hr_education` | 回退码值 |
| `renewed_from_id` / `renewed_from_no` | 合同自连接 | 原合同已删 → `（原合同已删除）` |
| `item_code` / `item_name` | join `biz_salary_item` | 回退码值 |

用例：`list_pairs_are_complete` / `list_pairs_are_null_together` / `detail_pairs_are_complete` /
`deleted_relations_fall_back_to_placeholder_not_empty` / `contract_pairs_and_expire_days`。

**不下发三个东西**（故意的，写在模块头表格里）：`org_id`（隔离维度、无组织主数据表可 join）、
`create_by` / `update_by` 裸 UUID（只给 `create_by_name` / `update_by_name`）、
`id_card` / `bank_account` 明文。

### ② 敏感字段返回 masked 版本

- `id_card` / `bank_account` **不进 DTO**，只出 `sensitive_masked.{id_card_masked, bank_account_masked}`；
- 打码口径 = `MaskSet` 命中就用库里的规则，没命中（含 `SUPER_ADMIN` 豁免、含规则被删）就走
  `V0388` 的两条全局规则参数（`2/6,4`、`5/4,4`）—— **超管在普通 DTO 上也看不到明文**，
  明文唯一出口是 `/employee/{id}/sensitive`（HR-13）；
- 打码失败（参数被改坏）→ `fail_closed` 全星号 + `tracing::error`，绝不回退原文；
- 真库实测（`hr0389_t1`）：

```
[HR-11] sys_field_mask(biz_employee) = [("bank_account", 5, Some("4,4")), ("id_card", 2, Some("6,4"))]
[HR-11] 生效的 biz_employee 规则 = ["bank_account(5,Some(\"4,4\"))", "id_card(2,Some(\"6,4\"))"]
[HR-11] id_card_masked = 110101****1234 / bank_account_masked = 6222 **** 7890
```

用例：`masked_values_follow_v0388_rules` / `empty_rule_set_still_masks` /
`exempt_set_still_masks_employee_sensitive_fields` / `absent_sensitive_values_are_null_not_masked` /
`broken_mask_param_fails_closed_to_full_stars` / `detail_never_carries_plaintext_sensitive_columns`
+ 真库 `seeded_rules_match_dto_defaults` / `detail_masks_with_real_db_rules`。

`mobile` 走**有规则才遮**（`V0388` 没给 `biz_employee.mobile` 种规则 ⇒ 设计 §5.2 #1 的列表列保持明文；
客户加规则后自动改遮），用例 `mobile_is_masked_only_when_a_rule_exists`。

### ③ 单测验证 DTO 形状

两条 census 用例把 JSON 字段集合**逐个列出**并排序比对（`list_json_shape_is_pinned` 21 个键、
`detail_json_shape_is_pinned` 49 个键）—— 多一个少一个都红，防止前端静默 `undefined`。
另有：金额是字符串 `"8000.0000"`、日期 `YYYY-MM-DD`、时间戳 `+08:00`（`2026-11-12T10:36:00+08:00`）、
子表空时是 `[]`（不是缺字段）、空值是 `null`（不是 `""`）。
真实出参见 `03_detail_json_sample.json`（`dto_json_samples_are_printable` 打印）。

## 门禁（`07_gates.out`）

| 项 | 结果 |
|---|---|
| `rustfmt --check`（3 个文件） | clean |
| `cargo clippy --all-targets -- -D warnings` | EXIT=0 |
| `cargo test --lib`（带真库） | **4875 passed / 0 failed**（HR-10 基线 4848 + 本卡 27） |
| `pnpm typecheck` | EXIT=0 |
| `pnpm build` | EXIT=0 |

## 环境与残留

- 真库用例取 `HR_DB_URL`、回退 `ERP_DB_URL`（与 HR-10 同口径）；本卡用 `hr0389_t1`（只读，不造夹具）。
- 未新建任何迁移（`biz_employee` / `biz_employee_contract` / `biz_employee_salary` 已在 `V0383` 建好、
  两条脱敏规则已在 `V0388` 种好，均已应用 ⇒ 不可改）；未触碰任何已应用迁移。
- 未起后端实例、未建演练库（本卡纯代码 + 只读真库查询），无残留。

## 给下游卡的两条交接

1. **HR-12（handler）**：字典名 / 用户名要**批量 join 后**填进 `EmployeeNames`（`.with_position/.with_job_level/.with_education/.with_operators`），
   合同的 `renewed_from_no` 用 SQL 自连接取 —— 不填就会回退成码值（不会空白，但界面上是 `SENIOR` 这种码）。
2. **HR-13（明文接口）**：`MaskSet` 的 `is_exempt()` 在本 DTO 上**故意不生效**，勿把 `sensitive_masked` 改成「超管给明文」；
   明文请走 `/employee/{id}/sensitive` + `audit_logs`。
