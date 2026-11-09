# ARCH9-3 成本核算升级设计 —— 证据包

配套文档：`docs/architecture/ARCH-9-成本核算升级设计.md`

本目录证明两件事：

1. 设计文档 §3 里的**每一行 DDL**（13 张新表 + 4 类改造 + 6 类种子）都能被真实 PG 吃下去，且断言「结果形态」全过；
2. 设计文档 §4.6（约当产量）、§4.4（分摊守恒）、§4.9（差异恒等式）里的**每一个数字**都是库算出来的，不是手填的。

## 文件

| 文件 | 说明 |
|---|---|
| `01_cost_ddl_check.sql` | DDL 验证脚本：整段在一个事务里跑，结尾 `ROLLBACK`（库零残留） |
| `01_cost_ddl_check.out` | 运行输出（102 行）：13 表 + 4 类改造 + 6 类种子 + 8 组断言 |
| `02_cost_calc_drill.sql` | 算例脚本：约当产量（算例 A）/ 分摊与尾差（算例 B）/ 差异恒等式（算例 C） |
| `02_cost_calc_drill.out` | 运行输出（59 行）：含实际计算结果 |

## 复跑

```bash
cd <repo>
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test -v ON_ERROR_STOP=1 \
  -f docs/evidence/2026-11-10-arch9-3-cost-ddl/02_cost_calc_drill.sql
```

> 两条都以 `ROLLBACK` 收尾，可对任何有基础 schema 的库反复跑；跑完会自检「零残留」。

## 结论（原文摘录自 .out）

```
NOTICE:  ✅ 全部断言通过：13 表 / 4 类改造 / 6 类种子 / 周期唯一键口径
ROLLBACK
NOTICE:  ✅ 零残留：ROLLBACK 干净
```

```
算例 A：equiv_material 1000 / equiv_conversion 900
        unit_material 19.000000 / unit_labor 4.444444 / unit_overhead 4.444444
        finished_cost 22311.1104 / wip_end_cost 4688.8896 / unit_finished_cost 27.888888
NOTICE:  ✅ 算例 A 通过：完工 800 件 22311.1104（单位 27.888888）/ 在制 200 件 4688.8896 / 合计 27000.0000 守恒
NOTICE:  ✅ 算例 B 通过：B1 合计 4000.00（2400/800/800）；B2 合计 1000.00（333.3333×2 + 333.3334，尾差 0.0001 单行收口）
NOTICE:  ✅ 算例 C 通过：材料 600+3840=4440（实际15000-标准10560）；人工 -180+500=320（实际4320-标准4000）
NOTICE:  ✅ 零残留：算例脚本 ROLLBACK 干净
```

## 断言清单（`01` 脚本第四部分）

| # | 断言 |
|---|---|
| ① | 13 张新表齐 |
| ② | `biz_stock_out.cost_object_id` 在；`biz_stock_in` 两个成本列在 |
| ③ | `ck_biz_si_type` 含 `PRODUCTION`（按约束定义文本判，不猜） |
| ④ | `4001/4101/2211` 已翻非末级；9 个末级科目齐且 `level=2` |
| ⑤ | 6 个页面菜单 + 20 个按钮；同父 `sort_order` 不重复（AGENTS 约定 12） |
| ⑥ | 号段 2 条；费用项目字典 7 项 |
| ⑦ | **期间唯一键口径**：两张 `REVERSED` 存在时同期间仍可建活跃批次（能重算）；同期间第二张活跃批次必被拒（防并发双单） |
| ⑧ | **归集表归属约束**：费用池行可不挂成本对象；工行不挂必被 `check_violation` 拒 |

## 跑的过程中改掉的 3 个真错误（说明「文档 DDL 必须真跑」）

1. `uk_biz_cost_period` 最初写成 `period_code` 全表唯一 → 红冲后该期间再也无法重新结算（**死单**）。改为排除 `REVERSED/CANCELLED` 的部分唯一索引，并补了正反两段断言（⑦）。
2. `biz_cost_collect.cost_object_id` 最初 `NOT NULL` → 「车间费用池」（分摊前不属于任何对象）无处安放，只能先挂对象再摊出来，池与 4101 永远对不平。改为可空 + `ck_biz_cost_collect_obj` 限定只有三类池来源可为空，并补断言（⑧）。
3. `biz_cost_collect.source_type` 值域漏了 `DEPRECIATION`/`EXPENSE`/`OVERHEAD_TICKET` → 折旧与报销的归集行根本写不进去。补齐为 7 值。

另：算例 A 的期末在制人工/费用各比手算多 0.0004（4 位舍入的必然结果）—— 这正是设计文档 §4.6.1 选择「差额倒挤进在制」的原因；若两处各算一遍，`ck_biz_cost_item_cons` 会把结算直接打回。
