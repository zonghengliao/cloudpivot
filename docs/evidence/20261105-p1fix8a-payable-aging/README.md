# P1-FIX-8A 应付账款账龄分析（PRD E4-03 P0）— 交付证据

> 卡面问题：`grep -rn 账龄 backend/src frontend/src` **0 命中** → PRD E4-03 的 P0 需求完全没做。
> 本卡从零建这张表：后端算法 + 接口 + 前端报表页 + 单测 + 真机截图 + 迁移（菜单/权限）。

## 1. 做了什么

### 1.1 后端：`backend/src/business/erp/payable_aging.rs`（新，约 1210 行含单测）

| 组成 | 说明 |
|------|------|
| `BUCKETS` | 账龄档位**唯一真相**：`NOT_DUE`(≤0) / `D0_30`(1–30) / `D31_60`(31–60) / `D61_90`(61–90) / `OVER_90`(≥91) |
| `bucket_of(days)` | Rust 侧分档（明细行用）；纯函数，边界单测逐个钉 |
| `bucket_case_sql(expr)` | SQL 侧 `CASE` **由同一张 `BUCKETS` 生成**（按供应商汇总用）→ 加档位只改一处，两侧不会漂移 |
| `overdue_days(as_of, due)` | 逾期天数 = 基准日 − 到期日（0 = 到期当天算未到期，负数 = 未到期） |
| `resolve_as_of()` | 基准日解析：`as_of` 优先 → `period` 末日 → 今天(CST)；非法 400+10001 |
| `base_cte()` | 取数口径只写一份：`deleted_at IS NULL` + `status IN (OPEN,PARTIAL_PAID,PENDING_CANCEL)` + `unpaid_amount > 0` + `period_code <= 基准期间` |
| `load_cells / load_totals_and_details` | 三条查询：供应商×档位聚合、总量、明细（`DETAIL_LIMIT=1000`，汇总不受截断影响） |
| `pivot_suppliers / summarise` | 透视与汇总（纯逻辑，可单测）；缺档补 0、按未付倒序、最长逾期取 max |
| `assemble()` | 出参组装：档位定义（前端据此渲染表头）、汇总、逾期占比、供应商行、明细、口径 hints |
| `payable_aging_handler` | `GET /business/erp/report/payable-aging`，权限 `business:erp:report:payable-aging` |
| `payable_aging_export_handler` | `GET .../payable-aging/export`（xlsx，复用 `impexp::engine::write_sheet`），权限复用 `business:erp:report:export` |
| 事务 | 整张表在一个 `REPEATABLE READ` 快照里算完（与资产负债表同口径） |

**为什么直接算不落快照表**：`due_date / unpaid_amount / status` 都是 `biz_payable` 既有列（V0148），
加一张「账龄快照表」等于要求所有人记住「跑报表前先刷新快照」，多一个必忘的动作。

**为什么明细在 Rust 侧再分一次档**：SQL 只回 `due_date`，明细档位由 `bucket_of` 现算，
于是「明细档位」与「供应商汇总档位」必然同源；e2e 里再用「汇总 == 明细按档求和」互证两条实现。

### 1.2 前端

| 文件 | 内容 |
|------|------|
| `frontend/src/api/business/erp/index.ts` | `AgingBucketDef/AgingBucketAmount/AgingSupplier/AgingDetail/PayableAging` 类型 + `fetchPayableAging` + `exportPayableAging` |
| `frontend/src/views/business/erp/report/payable-aging/index.vue`（新） | 期间下拉 + 基准日（可手填/留空=期间末日）+ KPI（未付/逾期/占比/供应商·单据）+ 供应商汇总表（按后端 `buckets` 渲染表头）+ 单据明细 + 口径提示；金额千分位用十进制字符串四舍五入（不用 `Number()`，避免浮点丢分） |
| `frontend/src/router/index.ts` | 手工注册路由（漏了 = 点菜单 404，本仓已有先例） |
| `frontend/src/views/.../payable-aging/index.vue` 的基准日控件 | 留空 = 该期间末日（后端回 `as_of_source=PERIOD`）；手填 = `QUERY`；页头照实写来源，避免「前端自己算末日再传上去，来源永远显示手填」 |

### 1.3 迁移：`backend/migrations/business/V0255__erp_payable_aging.{up,down}.sql`

- 菜单 id `…1600`：`business:erp:report:payable-aging`，path `/business/erp/report/payable-aging`，
  component `business/erp/report/payable-aging/index`，父目录 `…0e02`「财务管理」，sort 47。
- 授权 8 个角色码（SUPER_ADMIN / ADMIN / TEST_UADMIN / FINANCE / FINANCE_MANAGER / FINANCE_DIRECTOR / ACCOUNTANT / GENERAL_MANAGER），
  空库先行（迁移早于 bootstrap）时只 NOTICE 不报错；重放补齐（实测第 2 次补了 bootstrap 新建的 SUPER_ADMIN，第 3 次 0 行）。
- **不新增导出按钮菜单行**：直接复用 `business:erp:report:export`（同一 auth_code 两行会让「按码取菜单」重复）。

## 2. 验收对照

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| 账龄计算正确（单测验证分档边界） | ✅ 8 条单测，边界 −120/−1/**0**/**1**/**30**/**31**/**60**/**61**/**90**/**91**/3650 逐格断 | `unit_tests.txt` |
| 接口返回结构清晰 | ✅ 档位定义下发（前端不写死）＋汇总/供应商/明细/口径 hints；金额为 numeric 字符串 | `e2e_round1.txt`（69 条断言） |
| 前端报表页展示 | ✅ 真机 DOM 77 条断言（表头/合计逐格/供应商排序/明细逐行档位/改基准日重算） | `page_check_round1.txt` + 两张截图 |
| 真机截图 | ✅ `01-payable-aging-2026-11-05.png`、`02-payable-aging-2026-11-06.png`（1560×1100 全页） | 本目录 |
| git commit | ✅ 见卡内汇报 | — |
| 连跑两轮 | ✅ e2e 两轮逐字一致；页面 DOM 两轮逐字一致 | `e2e_round1/2.txt`、`page_check_round1/2.txt` |

**额外做到的**：导出 xlsx 真落内容（`export_xlsx_dump.txt`：表头 9 列 + 3 家供应商 + 合计行，金额正确）；
基准日 +1 天整表重算（未到期 17,000→12,000、1-30 天 8,000、31-60 天 14,000，30 天那张跨档迁到 31-60）。

## 3. 实测数字（隔离库 `cloudpivot_aging8`，基准日 2026-11-05）

| 档位 | 金额 | 由哪些夹具构成 |
|---|---|---|
| 未到期 | 17,000.00 | 未到期 12,000 + **到期当天(0 天)** 5,000 |
| 1-30 天 | 11,000.00 | 逾期 **1 天** 3,000 + 逾期 **30 天** 8,000 |
| 31-60 天 | 17,000.00 | 逾期 **31 天** 6,000（部分已付 9000−3000）+ 逾期 **60 天** 11,000 |
| 61-90 天 | 37,000.00 | 逾期 **61 天** 15,000 + 逾期 **90 天** 22,000 |
| 90 天以上 | 30,000.00 | 逾期 **91 天** 30,000（部分已付 40000−10000） |
| 合计 | 112,000.00 | 逾期合计 95,000.00 · 逾期占比 84.82% · 3 家供应商 / 9 张单据 |

排除样本（各一条，实测都没进表）：`SETTLED`（未付 0）、`CANCELLED`、`period_code=2026-12`（基准日之后的期间）。

## 4. 复现步骤

```bash
# 1. 空库 + 起隔离实例（端口 18144，二进制 /root/p1fix5c-target/debug/cloudpivot）
bash scripts/verify/fix8a_aging_db.sh recreate
bash scripts/verify/fix8a_aging_instance.sh start
bash scripts/verify/fix8a_aging_db.sh seed            # 9 张边界夹具 + 3 张排除样本

# 2. 接口层 69 条断言（连跑两轮）
BASE_URL=http://127.0.0.1:18144 python3 scripts/verify/fix8a_aging_e2e.py

# 3. 前端 dev（代理指向隔离实例）+ 真机 DOM 77 条断言 + 截图
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18144 npx vite --host 127.0.0.1 --port 15144 --strictPort
cd frontend && FIX8A_WEB=http://127.0.0.1:15144 FIX8A_API=http://127.0.0.1:18144 \
  node ../scripts/verify/fix8a_aging_page_check.mjs
```

脚本原件在本目录 `scripts/`（与 `scripts/verify/fix8a_aging_*` 同一份）。

## 5. 已知缺口 / 如实说明

1. **应收账龄没做**：全库没有应收台账 —— `biz_receivable` 不存在（`backend/migrations/**/CREATE TABLE` 全量清单已核），
   也没有销售订单 / 销售出库（`biz_stock_out` 只有成本账，无客户、无应收金额、无账期）。1122「应收账款」只是会计科目，
   没有单据级明细可做账龄。按卡面「没有就先只做应付」执行；等销售/应收模块落地再挂同一套 `BUCKETS`。
2. **金额是「当前未付余额」快照**，不做历史余额重算：基准日之后发生的付款不会回滚成历史值。
   页面口径提示里写明了（`assemble()` 的第二条 hint）。要做真历史得引入付款流水的时间旅行查账，属另一张卡。
3. **隔离库是空库 + 边界夹具**，不是生产快照：原因是生产快照副本里的 admin 口令未知、隔离实例登不进去；
   而生产现有 9 张应付单**全部未到期**（`due_date` 2026-11-30 ~ 12-03），对分档验证没有信息量。
4. **生产 144 未部署**（本卡只到 99 隔离实例）。上线时 `deploy.sh` 会带上 V0255，记得清菜单缓存
   （`redis-cli -n 0 DEL cloudpivot:menu:tree:all` + 删 `cloudpivot:perm:user_menus:*`），否则侧边栏看不到新菜单。
5. `erp_invariants` 没跑：本库的应付单是**没有凭证**的纯夹具（只为验分档），会让「应付必有凭证」这类不变量假红，
   跑出来的红与本卡改动无关（夹具口径问题，不是代码问题）。

## 6. 文件清单

**新增**
- `backend/src/business/erp/payable_aging.rs`
- `backend/migrations/business/V0255__erp_payable_aging.up.sql` / `.down.sql`
- `frontend/src/views/business/erp/report/payable-aging/index.vue`
- `scripts/verify/fix8a_aging_db.sh` / `fix8a_aging_instance.sh` / `fix8a_aging_e2e.py` / `fix8a_aging_page_check.mjs` / `fix8a_export_dump.py`
- 本目录（README + 2 张截图 + 全部日志 + 导出件）

**修改**
- `backend/src/business/erp/mod.rs`（注册模块 + 模块表补一行）
- `frontend/src/api/business/erp/index.ts`（+ 类型与两个 API 函数）
- `frontend/src/router/index.ts`（+ 一条路由）
