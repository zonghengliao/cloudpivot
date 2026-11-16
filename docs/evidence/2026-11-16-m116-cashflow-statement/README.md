# M1-16 现金流底稿：由底稿生成现金流量表（卡 t_74e707ca）

**结论**：新增模块 `cashflow_statement.rs`（一个只读出表接口 + 一个 xlsx 导出接口）+ 迁移 `V0417`
（两个按钮权限码）。三栏报表 = **项目 / 本期金额 / 上期金额**，数据只取自底稿
`biz_cashflow_worksheet`（M1-13 建表、M1-14 录入），不读凭证、不做二次分类、不写一个字节。

- 提交：`<commit>`（origin/main）
- 上游：V0414（底稿表）/ V0415（底稿两码）/ V0416（间接法调整项两码）
- 下游：M1-17（底稿单测）/ M1-18（前端底稿页）

## 一、接口（两个，都是只读）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/api/business/erp/report/cash-flow/statement?period=2026-11` | `business:erp:cashflow-statement:list` | 三栏报表 + 勾稽 + 告警；`hide_zero=true` 可隐藏「两栏 0 且都没录」的行 |
| GET | `/api/business/erp/report/cash-flow/statement/export?period=2026-11` | `business:erp:cashflow-statement:export` | 同一份表导出 xlsx（**出表闸**：未闭合 400 + 66087，不生成文件） |

出参要点（`CashStatementRes`）：

```text
report_code = CASH_FLOW_WORKSHEET · source = WORKSHEET · unit = 元
period / period_name / period_status / period_start / period_end
previous_period（上一会计期间，上期没开账 = null）· previous_period_status
current_label = 本期金额 · previous_label = 上期金额
sections[3] = { activity, activity_name,
                inflow[] { item_code, item_name, activity, direction, sort_order,
                           current, previous, current_entered, previous_entered,
                           item_active, remark },
                inflow_total { current, previous } , outflow[] , outflow_total , net }
net_increase { current, previous }
opening / closing = { line_type, item_name, current(null=未录), previous }
check = { balanced（本期出表闸）, previous_balanced, arithmetic_ok, diff, message,
          current（复用 M1-14 的 CheckRes，含 problems[]）, previous }
line_count · hidden_zero_lines · warnings[] · source_note
```

## 二、口径与关键决定（都有理由，别随手改）

1. **「三栏」= 项目 / 本期金额 / 上期金额**，`上期` = 上一会计期间（`report::previous_period_of`），
   与资产负债表 / 利润表同一个「上期」定义 —— V0414 的 `idx_biz_cashflow_ws_item_code`
   注释就是为这一列建的。
2. **上期只认会计日历里真有期间行的期间**：没开账 ⇒ 整列 `null` + 告警点名，
   既不按 0、也不按 1 推导（与 `report::build_snapshot_of` 逐字同规则）。
3. **行集合 = 本期底稿合并结果 ∪ 上期独有行**，直接复用 M1-14 的 `compose_sheet`
   （配置项 ∪ 已录行；已录行以快照为准；配置里已删的已录行照样出）。不在这里重写一遍
   「哪一行该出现」—— 重写就会有两份真相。
4. **不设加减号推断**：`FLOW` 行金额恒 ≥ 0，方向由 `direction` 表达；本模块不做
   「流出取负再求和」（那会让流入合计出现负数、两栏互相抵消）。
5. **跨期活动 / 方向不一致要吭声**（配置被人改过方向）：上期金额按本期归属列示 +
   `warnings` 点名，不静默。
6. **两条等式都验才算闭合**：① 底稿勾稽 `期初 + 流入 − 流出 = 期末`（复用 M1-14 的
   `reconcile`，精确到 4 位小数，绝不四舍五入抹平）；② 报表自身 `净增加额 + 期初 = 期末`。
   只看 ① 的话，一张「小计加错」的表也能过闸。
7. **GET 不因未闭合而报错、导出才是闸**：页面要能打开一张还没编完的表去补；
   但 xlsx 一旦落盘就会流出系统，所以导出要求闭合（`400 + 66087`，报文点名期间与待补项）。
8. **不夹带间接法补充资料**：M1-15 只配了调整项口径（`biz_cashflow_adjust_item`），
   出间接法金额要取利润表净利润 + 科目发生额 —— 不是「由底稿出表」这件事，属于另一张卡。
   本模块 `warnings` / 文档都写明这一点，不用空壳字段占位。
9. **不复用也不覆盖 ERP-R3**（`cash_flow.rs` 按凭证分类的简化版现金流量表）：两条并行编制路径，
   本表以「编制底稿的人」为准；两边差异是 M1-24（收口 E2E）要观察的对象，不是本卡要抹平的噪声。

## 三、迁移 V0417（只追加，幂等）

- 两个 `menu_type=3` 按钮码挂「现金流量表」页面 `…001018`：`…001810`（出表查看 / sort 50）、
  `…001820`（出表导出 / sort 60），接在底稿 10·20 与调整项 30·40 之后。
- 授权 `SUPER_ADMIN` + `FINANCE_MANAGER`；**刻意不给 `FINANCE_DIRECTOR`**（它没有该页面码，
  授了就是孤儿按钮，`orphan_button_gate` 判红）。
- 幂等：`ON CONFLICT (id) DO NOTHING` + `NOT EXISTS` 去重 + 自检块；重放不覆盖管理员改过的 title。
- down：先删授权再删菜单，天然幂等；回滚语义 = 出表功能对非超管下线（不产生数据护栏，本迁移无业务数据）。

## 四、验收（全部真跑，读数在 `*.out`）

| # | 项 | 结果 | 证据 |
|---|---|---|---|
| 1 | `cargo fmt -p cloudpivot -- --check` | 0 diff | `00_fmt_check.out` |
| 2 | `cargo clippy -p cloudpivot --lib -- -D warnings` | 0 警告 | `01_clippy.out` |
| 3 | 模块单测（9 纯逻辑 + 5 真库，含 worksheet / adjust 回归） | **42/42** | `02_unit_tests.out` |
| 4 | V0417 真库往返（up ×2 幂等 / 结构 / 授权 / down / down 幂等 / 再放） | **30/30** | `03_migration_drill.out` |
| 5 | 真 HTTP + 真引擎（V0405..V0417 全应用 / 重启 0 迁移 / 三栏读数 / xlsx / 出表闸 / 66010 / 10001 / 401） | **75/75** | `04_http_e2e.out` |
| 6 | 权限 / 菜单闸 | `perm_menu_gate` PASS（missing_in_menu=0）、`menu_invariants` **8/8**（512 条）、`role_code_gate` ✓ | `05_gates.out` |
| 7 | `orphan_button_gate` | 3 条孤儿**均为存量**（notice:mine ×2、role-mutex ×1）；对照库 `cloudpivot_test` 同样 3 条，不含本卡码 | `05_gates.out` 末尾 |

导出的 xlsx 真件：`sample/statement_1913-05.xlsx`（6,874 B），表头 `项目 / 本期金额 / 上期金额`，
读数与接口一致（本期销售收款 1,200.00 / 上期 900.00；期末 1,000.00 / 500.00；末行勾稽整句「已闭合」）。

## 五、复现

```bash
# 1) 单测（99 上跑，连 145 的 PG）
HOST=172.18.10.145
PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m116_stmt_unit
for f in V0414__cashflow_worksheet.up V0415__cashflow_worksheet_perm.up \
         V0416__cashflow_adjust_item.up V0417__cashflow_statement_perm.up; do
  psql -h $HOST -U cloudpivot -d cp_m116_stmt_unit -q -1 -v ON_ERROR_STOP=1 \
       -f backend/app/migrations/business/$f.sql; done
cd backend && ERP_DB_URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@$HOST:5432/cp_m116_stmt_unit" \
  cargo test --lib business::erp::cashflow -- --test-threads=1

# 2) 迁移往返 + 真 HTTP（脚本自建自删演练库）
bash scripts/verify/m116_cashflow_statement_drill.sh
bash scripts/verify/m116_statement_http_e2e.sh          # 145 上跑加参数 127.0.0.1

# 3) 闸（需要一个跑到最新迁移的库；本卡用 cp_m116_gates = 模板 + 引擎补齐 V0405..V0417）
NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m116_gates \
NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=… python3 scripts/verify/perm_menu_gate.py
```

## 六、清场

`cp_m116_stmt_unit` / `cp_m116_stmt_drill_*` / `cp_m116_stmt_e2e` / `cp_m116_gates` 全部由脚本或本卡删除；
`cloudpivot_test` 未改（只读克隆）。收尾核对：`select datname from pg_database where datname like 'cp_m116%'` = 0 行。

## 七、踩坑记录（给后来人）

- **`cargo fmt --all` 会格式化 `backend/vendor/nik-*`**（153 个文件），把 vendored 依赖一起改脏。
  正确姿势：`cargo fmt -p cloudpivot`。本次误伤已全部 `git checkout --` 还原。
- 报表金额在 JSON 里是**字符串**（`rust_decimal` 默认 serde），jq 断言要 `tonumber`。
  文件名 / `Content-Type` 走 `xlsx_response`，中文名是 RFC5987 `filename*=UTF-8''`。
