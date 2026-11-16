# M1-20【M1·财务】应收账龄：报表 + 导出

- 日期：2026-11-16
- 卡号：`t_d7262e01`（M1-20）
- 结论：**已落地**。应收账龄报表（按客户汇总）有了 **xlsx 导出**：
  `GET /business/erp/receivable/aging/export`，导出的就是页面「账龄看板」那张主表
  （客户 / 单据数 / 30·60·90·180·365 七档 / 未收合计 / 最长逾期天数）+ 合计行。
- 迁移：**0 条**（账龄是现算报表，没有落库结构变更）。

## 做了什么

| # | 改动 | 文件 |
|:--:|------|------|
| 1 | 新增导出接口 `GET /business/erp/receivable/aging/export`（xlsx 字节流，权限复用 `business:erp:report:export`） | `backend/app/src/business/erp/receivable/receivable_aging.rs` |
| 2 | `export_table()`：客户汇总 + 合计行，列序 = 页面主表 = 应付账龄导出（同形）；金额走 `payable_aging::money_text`（千分位 2 位小数，与应付 / 三大报表同一份实现） | 同上 |
| 3 | `export_file_name()`：`应收账款账龄分析表_<期间>_截止<基准日>.xlsx`（与应付账龄同名规则；无期间写「无期间」） | 同上 |
| 4 | 模块头补「导出」一节：说明数就是 `build_report` 那份、明细为什么不进导出 | 同上 |
| 5 | 单测 + 真库 xlsx 回读：列序 / 补 0 / 合计行 / 空表 / 真字节读回逐格核对 / 文件名 | 同上 `#[cfg(test)] mod tests` |
| 6 | 前端 API `exportReceivableAging()` | `frontend/src/api/business/erp/index.ts` |
| 7 | 账龄看板工具栏加「导出 Excel」按钮（与查询同一组 `as_of` 参数，失败弹错误不静默） | `frontend/src/views/business/erp/receivable/index.vue` |
| 8 | 纯逻辑 `agingExportFileName()`（与后端同一套命名）+ 单测 | `receivable-logic.ts`、`tests/unit/erp-receivable.test.ts` |

## 关键决策（为什么这么切）

1. **只导客户汇总，不导单据明细**。列表接口的明细有 `DETAIL_LIMIT = 1000` 截断，
   把截断后的明细导成「完整单据清单」是坑（用户以为拿到了全部，实际少了尾巴）。
   汇总侧是全量的（SQL 聚合，不跟着截断），所以汇总可以导；要导明细得先做不分页明细接口，另开卡。
   注：本卡不做「报表口径改造」——报表本体（客户透视 / 分档 / 逾期占比）M1-19 已定稿，本卡只补导出。
2. **权限复用 `business:erp:report:export`**，不新增码。应付账龄 / 资产负债表 / 利润表 / 现金流量表
   都用这个开关，应收账龄没有理由再开一个（`V0183` 已种，`GENERAL_MANAGER` / `FINANCE_MANAGER` 已有）。
   实测证明它不是「能看报表就能导」：`SALES_SUPERVISOR` 有 `receivable:aging` 没有 `report:export` →
   报表 200、导出 **403**（见下方 ⑤b）。
3. **列序与应付账龄导出逐列同形**（客户/供应商 → 单据数 → 七档 → 合计 → 最长逾期天数）。
   财务同时导应付 / 应收两张表时不用重新学列；两边的差异只在档位数（7 vs 5，属 M1-21）。
4. **导出不二次加总**：合计行直接取出参里的 `summary` / `total_unsettled` / `doc_count`，
   客户行取 `customers[].buckets`。导出与页面同源，不会出现「页面一个数、Excel 另一个数」。
5. **缺档补 `0.00` 而不是空串**：财务报表里空格子会被读成「未知」，0 才是真实业务含义
   （这一档确实没有钱）。单测逐列钉死「每行每列都有值」。
6. **不做按钮级权限位**：与应付账龄页同口径（按钮不判权，后端 403 + 前端弹错）。
   前端单独判权会让「有码但权限未刷新」的用户看不到按钮，反而更难排查。

## 验收证据（全部真跑，非描述）

### ① 真接口 69 条断言（本机 `:8023` + **空库首启** `cp_m120_api2` + SQL 夹具 15 张应收）

`python3 api_check.py` → `00_api_check.out`（exit 0，69 PASS / 0 FAIL）。覆盖：

- 报表侧：未收合计 `10500`、`doc_count=13`（夹具口径先钉住，导出才有可比对象）；
- 导出响应：200 / `Content-Type=spreadsheetml.sheet` / `Content-Disposition` 文件名
  `应收账款账龄分析表_无期间_截止2026-11-05.xlsx` / 响应体是 zip（`PK`）不是 JSON 错误；
- **xlsx 读回逐格**（标准库 `zipfile` + `xml.etree` 直读 OOXML，不引第三方库）：
  表头 11 列逐字、行数 = 表头 + 2 客户 + 合计、每个客户的 **7 个档位金额 / 单据数 / 未收合计 /
  最长逾期天数**逐格等于报表 JSON、客户行各档之和 = 该行未收合计、
  合计行逐档 = `report.summary`、合计行最长逾期 = 各客户最大（366）；
- 期间口径 `?period=2026-10`：基准日 `2026-10-31`、11 月三张被截断（甲 5200 → 3800，乙 5300 不变，
  合计 9100），**导出合计 == 报表合计**，文件名带期间 `…_2026-10_截止2026-10-31.xlsx`；
- 出错路径：非法 `as_of=2026-13-01` → `400 + 10001`；**无 token → 401**（导出不是裸接口）；
- 权限闸（⑤b）：`m120sales`（`SALES_SUPERVISOR`）看报表 200 / 导出 **403**；
- 极端基准日 `2099-12-31`：所有金额落进「365 天以上」`10500.00`，前六档 `0.00`（分档列不错位）。

夹具：`seed.sql`（与 M1-19 同源，自带清理可重复跑）+ `perm_seed.sql`（权限对照用户，可重复跑）。
起服务：`run_backend.sh`（空库首启：日志实测「应用 349 个迁移」+ admin 种子；测试库 `cp_m120_api2`）。

### ② 单测（真库 `cp_m120_aging`，13/13 PASS）→ `03_unit_receivable_aging.out`

新增 3 条：

- `export_table_mirrors_customer_summary`（纯逻辑）：列序 11 列、列数 = 档位数 + 4、
  **每行每列都有值**（缺档 `0.00` 不是空串）、千分位 `1,234,567.89`、合计行 == `summary`、
  合计行最长逾期 = 各客户最大值；
- `export_table_handles_empty_report`（纯逻辑）：空表导出 = 表头齐全 + 一行「合计 0.00」，
  不是 500、不是空文件（月结后跑空报表是常态）；
- `db_export_xlsx_round_trip_matches_report`（真库 + 真 xlsx）：`build_report`（接口整条链）
  → `export_table` → `write_sheet` 真字节 → **calamine 读回**逐格核对表头 / 2 客户 /
  7 档金额 / 合计 / 各档之和；并断言文件名两种形态（无期间 / 只给期间）。

### ③ 门禁 → 同名 `.out`

| 门禁 | 结果 | 日志 |
|------|------|------|
| `cargo fmt --check` | exit 0 | `01_backend_fmt.out` |
| `cargo clippy --lib -- -D warnings` | exit 0（0 warning） | `02_backend_clippy.out` |
| `cargo test --lib business::erp::receivable::receivable_aging` | **13/13 PASS**（真库） | `03_unit_receivable_aging.out` |
| `cargo test --lib system::menu::permission_audit_tests` | **8/8 PASS**（N14–N21，含 N16「每条路由都有权限闸」） | `04_perm_audit.out` |
| `pnpm typecheck` | exit 0 | `05_typecheck.out` |
| `pnpm typecheck:unit` | exit 0 | `06_typecheck_unit.out` |
| `pnpm test:unit` | **1948/1948 PASS**（新增 1 条） | `07_unit_all.out` |
| `pnpm build` | exit 0 | `08_build.out` |

## 复现步骤

```bash
# 夹具库（99 连 145；145 上跑把 HOST 换 127.0.0.1）
HOST=172.18.10.145
export PGPASSWORD=<见卡面>
createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m120_aging   # 单测
createdb -h $HOST -U cloudpivot cp_m120_api2                       # 真接口（空库首启）

ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_m120_aging" \
  cargo test -p cloudpivot --lib business::erp::receivable::receivable_aging -- --test-threads=1

bash run_backend.sh &                     # 见脚本里的 NIK_* / 密钥环境变量
psql -h $HOST -U cloudpivot -d cp_m120_api2 -c \
  "UPDATE sys_user SET must_change_password = false WHERE username = 'admin';"
psql -h $HOST -U cloudpivot -d cp_m120_api2 -v ON_ERROR_STOP=1 -f seed.sql
psql -h $HOST -U cloudpivot -d cp_m120_api2 -v ON_ERROR_STOP=1 -f perm_seed.sql
sleep 65                                  # 登录侧 must_change_password 有 60s 缓存
python3 api_check.py
```

## 诚实缺口（没做，也不在本卡范围）

- **单据明细导出**（不分页）：明细接口 1000 条截断，本卡刻意不导明细，另开卡。
- **前端页面整页化**（`/business/erp/report/receivable-aging` 独立路由）：M1-23「前端：应收账龄页」；
  本卡只在现有「账龄看板」tab 加了导出按钮（否则后端有了导出而没有入口 = 半成品）。
- **应付账龄补分档**：M1-21。
- **金额单元格是文本**（带千分位）：与应付账龄 / 三大报表导出同一口径（`money_text`），
  Excel 里不能直接 `SUM`。要数值化得先统一改所有报表导出，属报表导出口径改造，不在本卡。
- **未加 Playwright 用例**：导出按钮只在 `vue-tsc` + `vite build` 层面验证过，没在浏览器里真点一遍
  （下载拦截用例属前端 e2e 体系，与应付账龄页同口径——那张页也没有）。后端 69 条真接口断言 +
  xlsx 逐格回读覆盖了「点下去会发生什么」的全部分支（200 / 400 / 401 / 403）。
