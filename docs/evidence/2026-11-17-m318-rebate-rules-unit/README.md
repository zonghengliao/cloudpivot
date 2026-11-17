# M3-18 销售返利：单测（三种规则 + 边界）

**卡**：`t_e97935e5` · **日期**：2026-11-17 · **结论**：新增 13 条纯逻辑单测（`rebate_rule_matrix_tests.rs`，
约 1000 行），**零业务代码改动**；返利相关 104 条用例（13 新 + 91 存量）在**空库首启（373 迁移 / 187 表）**上全绿。

## 本卡只做单测

没有改 `rebate_dto` / `rebate_settle_dto` / `rebate_invoice_*` 的任何一行实现，也没有新迁移。
新增文件一个、`mod.rs` 加 3 行 `#[cfg(test)] mod` 声明。

`rebate_rule_matrix_tests.rs` 的 13 条用例（段号即文件里的分节）：

| # | 用例 | 钉住什么 |
|:--:|:--|:--|
| 段1 | `shape_matrix_three_rules` | **18 行**形状矩阵：按量/按额「恰好 1 档 + 平铺」、阶梯「2~20 档 / 从 0 起 / 首尾相接 / 末档开口 / 下限唯一」，全部 400 + 66089 并点名原因；合法输出恒 `seq=1..N` 升序、末档开口 |
| 段2 | `numeric_column_limits_are_exact` | 库列精度两侧：档位 `NUMERIC(18,4)` 的 `99999999999999.9999` 收 / `10^14` 拒、4 位小数收 / 5 位拒；比率 `NUMERIC(18,6)` 的 6 位收 / 7 位拒；百分比 100 收 / 101 拒；元/件 500 合法；负数全拒 |
| 段2 | `percent_limit_checks_integer_part_only` | **现状留痕**：百分比上限只比整数部分 ⇒ `100.999999` 会通过（详见「真发现 ②」） |
| 段3 | `pick_tier_edges_are_half_open` | 每个 `tier_from`/`tier_to` 的 **-0.0001 / 恰好 / +0.0001** 三侧共 11 例：边界值归下一档（左闭右开）、负数 `None`、末档无上限、空表 `None` 不 panic |
| 段3 | `pick_tier_uses_the_base_matching_the_unit` | 同一份档位：`PER_UNIT` 看销量落 1 档、`PERCENT` 看销售额落 2 档 |
| 段4 | `calc_matrix_over_boundary_bases` | **14 例**计价矩阵：三种规则 × 基数 `0 / 边界 / 边界±ε / 100% / 纯退货`，落档 + `pick_value` + 金额 + `money2` 幂等四件套 |
| 段5 | `rounding_strategy_is_bankers_half_even` | 半分策略**逐格钉死**：`0.005→0.00 / 0.015→0.02 / 0.025→0.02 / 0.035→0.04`（详见「真发现 ①」） |
| 段6 | `empty_negative_and_zero_net_bases` | 空期间 1 条提示 / 纯退货 1 条（文案不同）/ 净额恰好 0 零提示 / 三种规则负基数一律按 0 落首档 |
| 段7 | `wire_path_from_rule_req_to_amount` | 走 handler 同一条路：`RebateRuleReq::to_input` → `checked_rules` → `calc_rebate`；覆盖「省略 tier_from 默认 0 / 省略 rate 400 点名第 2 档 / 备注 trim / 乱序入参重排 1..3」 |
| 段8 | `type_unit_pairing_matrix` | 3 类型 × 2 单位 = 6 组合，**4 能算 + 2 拒**（`BY_QTY+PERCENT`、`BY_AMOUNT+PER_UNIT` 不让量纲不同的数相乘）；默认单位按量→元/件、按额→百分比、阶梯必须显式 |
| 段9 | `invariants_hold_across_base_grid` | 3 规则 × 12 个基数网格：`pick_value = max(净额,0)`、应返恒 ≥ 0、精度 ≤ 2 位、`money2` 幂等、费率不递减时基数涨则金额不跌 |
| 段10 | `settlement_period_boundaries` | 期间左闭右开 7 例：30 天月、跨年月、**闰年 2028-02（末日 02-29）**、平年 02-28、Q1 末日 03-31、跨年 Q4、含闰日的 Q1；月度配季度码 / 季度配月度码 400 |
| 段11 | `max_tier_ladder_works_end_to_end` | 20 档（上限）不只是存得下：首档 0 / 中间档 10 / 末档 190 与超上限全部落对 |

## 首次运行的 4 条红 = 两个真发现（已按现状钉住，未改实现）

**① 金额半分用「银行家舍入」（四舍六入五取偶），不是「五就进位」。**
`rust_decimal 1.43` 的 `round_dp` 默认是 `MidpointNearestEven`，`money2` 直接用它，
所以结算金额 / 红字发票金额 / 凭证行整条链都是这个口径：`0.005 → 0.00`（半分钱被舍掉）、
`0.025 → 0.02`（而不是财务手工表的 `0.03`）。实测表见段 5。
是否改成 `MidpointAwayFromZero` 是**会计口径决定**（要同时看结算金额、`split_invoice` 的分位恒等、
凭证借贷平衡三处），本卡只做单测 → 用真表钉死现状，谁改口径这张表立刻红。
**建议**：若财务要求「四舍五入」，另开一张卡改 `money2` 的舍入策略并回归这三处，本卡不要夹带。

**② 百分比上限只比整数部分**（`value.trunc().mantissa()`），所以 `100.999999` 能通过、`101` 被拒。
`checked_unit`/`checked_rules` 与 V0438 的 CHECK 都是这个判据。同上：留断言 = 留发现点。

**另外两条红是测试自己写错了**（不是产品缺陷），一并记录免得下次再踩：
- `round_dp(2)` 的结果**不保证 `scale()==2`**（`0.00` 的 scale 是 0、`0.5` 是 1）——「2 位小数」是
  「精度 ≤ 2 位」，断言要用 `scale() <= 2` + `money2` 幂等，别用 `scale() == 2`。
- 按量/按额是**平铺**规则（`tier_from` 恒 0、`tier_to` 恒空），想测 `tier_from`/`tier_to` 的数值上限必须
  用两档阶梯，否则先撞「平铺」那条 400，测不到上限。

## 复现

```bash
# 纯逻辑（无需数据库，秒级）
cd backend && cargo test --lib business::erp::sales::rebate_rule_matrix_tests -- --test-threads=1

# 全量返利回归（含 34 条真库用例）：空库首启一个库再指过去
PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot cp_m318_rules
NIK_PROFILE=dev NIK_SERVER_LISTEN=127.0.0.1:18099 NIK_DATABASE_HOST=172.18.10.145 \
NIK_DATABASE_NAME=cp_m318_rules NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=*** \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=*** \
  ./target/debug/cloudpivot        # 起一次让迁移+bootstrap 跑完（本次 373 迁移 / 187 表），curl /health 通后 kill
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m318_rules" \
  cargo test --lib business::erp::sales::rebate -- --test-threads=1
PGPASSWORD=*** dropdb -h 172.18.10.145 -U cloudpivot cp_m318_rules
```

> 模板库 `cloudpivot_test` 停在 V0404（缺 `biz_voucher.currency_code` 等列），克隆后真库用例会假红；
> 按上面**空库首启**走一遍才可靠（M3-17 已记过同一个坑）。

## 证据文件

| 文件 | 内容 |
|:--|:--|
| `00-unit-matrix.out` | 新增 13 条矩阵用例（`--nocapture`，含每条 `[M3-18]` 结论文案） |
| `01-fmt-check.out` | `cargo fmt --check` 无差异 |
| `02-clippy.out` | `cargo clippy --all-targets -- -D warnings` → `CLIPPY_EXIT=0` |
| `03-db-regression.out` | 返利全量回归（`ERP_DB_URL` 指向真库）：**104 passed / 0 failed**，其中 34 条真库用例全部实际执行（无 SKIP） |
| `04-schema.out` | 空库首启环境：373 迁移 / 187 表 / PG 18.6 / 返利三表在位 |
