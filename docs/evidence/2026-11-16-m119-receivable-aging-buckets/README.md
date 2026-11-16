# M1-19【M1·财务】应收账龄：计算（30/60/90/180/365 段）

- 日期：2026-11-16
- 卡号：`t_daca34c0`（M1-19）
- 结论：**已落地**。应收账龄从 5 档（… / 61-90 / 90 天以上）改为 **30/60/90/180/365 六段 + 未到期（7 档）**，
  段边界按「基准日 − 到期日」（逾期天数）判定；机制与应付账龄同源、刻度应收更细；**应付报表契约零变化**。
- 迁移：**0 条**（账龄是现算报表，没有落库结构变更）。

## 做了什么

| # | 改动 | 文件 |
|:--:|------|------|
| 1 | 档位机制参数化：`bucket_of_in(buckets, days)` / `bucket_case_sql_in(buckets, expr)` / `bucket_defs` / `empty_bucket_cells`；`bucket_of` / `bucket_case_sql` 保留为「按应付 5 档表走」的薄包装 | `backend/app/src/business/erp/payable/payable_aging.rs` |
| 2 | 新增档位表 `AGING_BUCKETS`（7 档：未到期 / 1-30 / 31-60 / 61-90 / 91-180 / 181-365 / 365 天以上），前四档与应付 `BUCKETS` 逐字相同 | 同上 |
| 3 | 应收账龄整条链改吃 `AGING_BUCKETS`：SQL `CASE`、明细档位现算、客户透视、汇总行、出参表头、口径提示 | `backend/app/src/business/erp/receivable/receivable_aging.rs` |
| 4 | 单测：边界逐天比对（两表共有的 −90..=90 完全一致）+ 180/365 两刀 + 真库 7 档金额 + 端到端出参 | 两处 `#[cfg(test)] mod tests` |
| 5 | 前端配色补齐 `D91_180 / D181_365 / OVER_365`（表头本来就是按响应 `buckets[]` 渲染，不用改列） | `frontend/src/views/business/erp/receivable/receivable-logic.ts`、`index.vue`、`api/business/erp/index.ts` |
| 6 | 前端单测：七档配色断言 | `frontend/tests/unit/erp-receivable.test.ts` |

## 关键决策（为什么这么切）

1. **刻度**：M1-19 卡面写「30/60/90/180/365 段」，落地就是 6 个逾期段 + 未到期 = 7 档。
   逾期 0 天（到期当天）留在「未到期」，与应付同口径（`payable_aging` 既有约定：到期当天不算逾期）。
2. **为什么不做成改共享 `BUCKETS`**：`payable_aging::BUCKETS` 是应付报表的**列布局来源**，
   直接扩成 7 档会改掉应付的接口契约（`OVER_90` 消失、列数变化），而「应付账龄补分档」是另一张卡（M1-21）。
   所以：机制参数化（唯一实现）+ 两张档位表（应付 5 档 / 应收 7 档），
   并用单测钉住「应收前四档与应付逐字相同」—— 不是各抄一份阈值。
3. **为什么阈值不落库**：账龄是现算报表（`as_of` / `period` 决定基准日），没有「档位配置」表；
   本次不引入新表，避免为一张卡开配置面（真要做配置化是产品决策，另开卡）。
4. **明细与汇总同源**：SQL 只回 `due_date`，明细档位由 `bucket_of_in` 现算；汇总用 SQL `CASE` 聚合。
   两者都由同一张档位表生成，单测用「逐档金额相等」交叉验证（`⑦` 与 `⑩`）。

## 验收证据（全部真跑，非描述）

### ① 真接口 37 条断言（本机 8023 + 空库首启 `cp_m119_api2` + SQL 夹具 15 张应收）

`python3 api_check.py` → `00_api_check.out`。覆盖：

- 表头 7 档的 **key / 中文名 / 上下界**（`(91,180) (181,365) (366,None)`）逐项对；
- 逐档金额 `1200 / 500 / 900 / 1300 / 2100 / 2900 / 1600`（合计 10500，逾期 9300，占比 88.57）；
- `doc_count=13`：`SETTLED` / `WRITTEN_OFF` 两张**不进表**（口径不是「全表都算」）；
- 客户透视：乙 5300 在甲 5200 前（按未收倒序），每行 7 格、各档之和 = 该客户未收；
- 明细按到期日升序、首行 = 366 天 `OVER_365`；
- **明细逐档合计 == 汇总**（SQL 聚合与 Rust 现算同口径的硬证明）；
- `?period=2026-10`：基准日取期间末日 10-31、期间晚于基准月的 3 张被截断（10 条）、表头仍 7 档（空档补 0）；
- 非法 `as_of=2026-13-01` → `400 + 10001`；
- **应付账龄复查仍是 5 档 + `OVER_90`**（本卡没动应付契约）。

夹具：`seed.sql`（自带清理，可重复跑；`biz_receivable` 15 行，到期日 = 2026-11-05 − 逾期天数）。

### ② 库层 / 纯逻辑单测（真库 `cp_m119_aging`）

- `business::erp::receivable::receivable_aging`：**10/10 PASS**（`03_unit_receivable_aging.out`）
  - ① 两表共有区间 −90..=90 逐天同档 + 180/365 两刀边界
  - ② SQL `CASE` 由档位表生成（含 `D91_180` / `D181_365`，末档 `OVER_365`）
  - ④⑤ 透视 / 汇总按 7 档补 0
  - ⑦ 真库：各档金额 1200/500/900/1300/2100/2900/1600，且 **SQL 聚合 == Rust 现算**
  - ⑧ 真库：客户倒序、逾期占比 88.57、明细首行 366 天
  - ⑨ 真库：`period` 截断（10 条 / 13 条）
  - ⑩ 真库：走 `build_report` 的**出参**闭环（表头 7 档 + 逐档金额 + 口径提示 + 客户行列数）
- `business::erp::payable::payable_aging`：**9/9 PASS**（`04_unit_payable_aging.out`，含新增
  `receivable_buckets_extend_payable_prefix`：前四档与 `BUCKETS` 逐字相同 + 应收 7 档边界）

### ③ 门禁

| 门禁 | 结果 | 日志 |
|------|------|------|
| `cargo fmt --check` | exit 0 | `01_backend_fmt.out` |
| `cargo clippy --lib -- -D warnings` | exit 0（0 warning） | `02_backend_clippy.out` |
| `pnpm typecheck` | exit 0 | `05_typecheck.out` |
| `pnpm typecheck:unit` | exit 0 | `06_typecheck_unit.out` |
| `pnpm test:unit` | **1947/1947 PASS** | `07_unit_all.out` |
| `pnpm build` | exit 0 | `08_build.out` |

## 复现步骤

```bash
# 夹具库（99 连 145；145 上跑把 HOST 换 127.0.0.1）
HOST=172.18.10.145
PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m119_aging   # 单测
PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 createdb -h $HOST -U cloudpivot cp_m119_api2                        # 真接口（空库首启）

ERP_DB_URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@$HOST:5432/cp_m119_aging" \
  cargo test --lib business::erp::receivable::receivable_aging -- --test-threads=1

# 真接口：空库首启（迁移自动跑 + 种 admin），把 admin 的 must_change_password 关掉后再灌夹具
cd backend && NIK_PROFILE=test NIK_DATABASE_HOST=$HOST NIK_DATABASE_NAME=cp_m119_api2 \
  NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=… NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 16) \
  ./target/debug/cloudpivot &
psql -h $HOST -U cloudpivot -d cp_m119_api2 -c "UPDATE sys_user SET must_change_password = false WHERE username = 'admin';"
psql -h $HOST -U cloudpivot -d cp_m119_api2 -v ON_ERROR_STOP=1 -f docs/evidence/2026-11-16-m119-receivable-aging-buckets/seed.sql
sleep 65   # 登录侧的 must_change_password 有 60s 缓存
python3 docs/evidence/2026-11-16-m119-receivable-aging-buckets/api_check.py
```

## 诚实缺口（没做，也不在本卡范围）

- **报表导出 xlsx**：M1-20（「应收账龄：报表 + 导出」）的活，本卡只做计算与出参。
- **应付账龄补分档**：M1-21；本卡刻意不动 `BUCKETS`，应付接口出参仍是 5 档。
- **账龄桶作为列表筛选条件**（PRD S4-02 的「账龄桶」筛选项）：属应收列表页改造，本卡未碰。
- **档位配置化**（比例/段位可配）：产品决策项，当前是编译期常量表。
- 未加 Playwright 用例：前端只改了配色映射，逻辑单测已覆盖；表格列本身按 `buckets[]` 动态渲染。
