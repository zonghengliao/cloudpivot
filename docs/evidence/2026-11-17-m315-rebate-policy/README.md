# M3-15 销售返利：政策表 + 规则（按量 / 按额 / 阶梯）· 验收证据

- 卡：`t_adda469e`（M3-15【M3·销售】销售返利：政策表 + 规则（按量/按额/阶梯））
- 日期：2026-11-17
- 范围：**只做政策与规则**。返利计算（M3-16）、生成应付/红字发票（M3-17）、单测扩充（M3-18）、
  前端政策与结算页（M3-19）都不在本卡。

## 交付物

| 类型 | 路径 |
|------|------|
| 迁移（表 + 号段 + 4 个权限码 + 自检块） | `backend/app/migrations/business/V0438__sales_rebate_policy.{up,down}.sql` |
| 契约 / 校验（三种规则类型的唯一实现 + 落档纯函数） | `backend/app/src/business/erp/sales/rebate_dto.rs` |
| 数据层（政策 + 规则读写、取号、重叠检查、客户级 advisory lock） | `backend/app/src/business/erp/sales/rebate_model.rs` |
| 接口层（5 条路由 + 三条业务闸） | `backend/app/src/business/erp/sales/rebate_handler.rs` |
| 真库用例（12 条，含并发） | `backend/app/src/business/erp/sales/rebate_db_tests.rs` |
| 错误码 `66089` / `66090` | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` |

## 三种规则类型（V0438 建表注释与 `rebate_dto::checked_rules` 是同一份口径）

| rule_type | 中文 | rate_unit | 规则行数 | tier_* 口径 |
|---|---|---|---|---|
| `BY_QTY` | 按量 | `PER_UNIT`（元/件，可省略默认） | 恰好 1 | `tier_from=0` 且 `tier_to` 空（平铺） |
| `BY_AMOUNT` | 按额 | `PERCENT`（销售额 %，可省略默认） | 恰好 1 | 同上 |
| `TIERED` | 阶梯 | **必填**，`PER_UNIT` 或 `PERCENT` | 2 ~ 20 | 从 0 起、首尾相接、末档 `tier_to` 空 |

- 落档：`rebate_dto::pick_tier`（左闭右开 `[tier_from, tier_to)`，末档无上限，不累进）——M3-16 唯一实现。
- 计价单位放**政策级**：放规则级会允许「第 1 档元/件、第 2 档百分比」这种无法相加的阶梯，库层
  `ck_biz_srbp_unit_pair` 直接挡。
- 阶梯依据不单列一列：由 `rate_unit` 决定（`PER_UNIT` ⇒ 按销量分档，`PERCENT` ⇒ 按销售额分档）。

## 验收证据（全部在本目录）

| 文件 | 内容 | 结果 |
|------|------|------|
| `00-unit.out` | `cargo test --lib business::erp::sales::rebate -- --test-threads=1`（真库 = 145 的 `cp_m315_clone`） | **34 passed / 0 failed**（22 条纯逻辑 + 12 条真库） |
| `01-clippy.out` / `01b-fmt-check.out` | `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` | rc=0 / rc=0 |
| `03-api.out` | 空库首启隔离实例（`:18089`）+ 真机 HTTP **17 组断言** | 全通过 |
| `04-menu-invariants.out` | `scripts/verify/menu_invariants.sh`（8 条断言，跑在 `cp_m315_e2e`） | 全绿（菜单总数 536 / 上限 540） |
| `05-schema.out` | `\d biz_sales_rebate_policy` / `\d biz_sales_rebate_rule` | 两张表 + 约束 + 索引 |

### 真库用例覆盖（`rebate_db_tests.rs`）

按量建单（取号 / 客户名 JOIN / 详情=列表）、按额、阶梯乱序重排 + 从库读回落档、重叠闸（相交 409+66090 /
错开可建 / 季度并存 / 停用不参与）、改政策整单替换（3 档 → 2 档不残留）、改自己不算重叠、改后重叠 409 且原值不变、
冻结客户 409 / 不存在 404、规则形状错 400+66089 且 0 落库、软删后列表/详情 0 条而规则行保留、
M3-16 取政策口径（按日/粒度/停用）、**并发建重叠政策只有一个成功**（advisory lock）、清场。

### 真机 HTTP（`03-api.out`，17 组）

登录 → ① 按量建单（`BR-20261117-000004`）→ ② 重叠 409+66090（文案点名撞了哪份政策）→ ③ 季度政策并存 →
④ 阶梯乱序入参 → `seq=[1,2,3]`、档位 `0/100/500`、末档开口 → ⑤ 断档 400+66089 → ⑥ 阶梯单档 400+66089 →
⑦ 百分比 150% 400+66089 → ⑧ 冻结客户 409 → ⑨ 分页带规则（`total=3`，每行规则数 `[3,1,1]`）→
⑩ 中文关键字 + 某日生效 → ⑪ 详情 → ⑫ 改政策 3 档→2 档 → ⑬ 改后重叠 409+66090 → ⑭ 被拒后原值不变 →
⑮ 软删 → 详情 404 → ⑯ 非法 UUID / 非法日期 400+10001 → ⑰ 未登录 401。

## 环境

- 真库：145 的 `cp_m315_clone`（`pg_dump cloudpivot_dev | psql` 克隆，含 V0437；再手工应用 V0438）。
  ⚠ `cloudpivot_test` 模板库仍停在 V0404，缺 `biz_customer.credit_policy`（V0437），**不能直接用于本卡的真库用例**。
- 空库首启：`cp_m315_e2e`（0 表）→ 现有二进制启动 → 迁移全部应用（含 V0438，`schema_migrations.version=438` 记录成功）
  → 186 表。启动期日志见卡内汇报；`NIK_ADMIN_PASSWORD` + 改 `must_change_password=false` 后等 ~60s 缓存才可登录。
- 两套临时库与 `:18089` 实例用完均已清理（见卡内 `cleanup` 字段）。
