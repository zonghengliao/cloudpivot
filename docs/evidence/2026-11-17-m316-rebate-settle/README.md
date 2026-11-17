# M3-16 销售返利：计算（月度 / 季度结算）· 验收证据

- 卡：`t_401d8504`（M3-16【M3·销售】销售返利：计算（月度/季度结算））
- 日期：2026-11-17
- 上一张：`t_adda469e`（M3-15 返利政策 + 规则，提交 `76aa69bd`）
- 范围：**只做「按周期算应返金额」**。生成应付 / 红字发票（M3-17）、单测扩充（M3-18）、
  前端政策与结算页（M3-19）都不在本卡。`SETTLED` 状态由 M3-17 置位，本卡只读不写。

## 交付物

| 类型 | 路径 |
|------|------|
| 迁移（结算表 + 号段 + 3 个权限码 + 6 组自检） | `backend/app/migrations/business/V0439__sales_rebate_settlement.{up,down}.sql` |
| 契约 + 纯计算（期间解析 / 基数 / 落档 / 金额 / 政策快照） | `backend/app/src/business/erp/sales/rebate_settle_dto.rs` |
| 数据层（基数聚合 CTE、读写、取号 `RB`、客户级 advisory lock） | `backend/app/src/business/erp/sales/rebate_settle_model.rs` |
| 接口层（7 条路由 + 五道业务闸） | `backend/app/src/business/erp/sales/rebate_settle_handler.rs` |
| 真库用例（12 条，含并发 / 幂等 / 引用闸） | `backend/app/src/business/erp/sales/rebate_settle_db_tests.rs` |
| 错误码 `66091` / `66092` | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` |
| 政策软删引用闸（M3-15 预留在 `soft_delete` 注释处的检查） | `backend/app/src/business/erp/sales/rebate_handler.rs` |

## 算什么（一句话）

> 某客户某个结算期间（月 / 季）的**净销售**，按该客户该粒度**覆盖整期**的返利政策落档，乘出应返金额。

### 基数口径（与销售目标 / SO-40 同一条链，不另立一套）

```text
销售额 = Σ 订单本位币含税金额（base_total_with_tax，为 0 时回落 total_with_tax）
        − Σ 退货含税金额（折本位币：r.total_with_tax × ar.base_total_amount / ar.amount）
销量   = Σ 订单明细 qty − Σ 退货明细 qty

订单白名单 = APPROVED / PARTIAL_OUT / OUTBOUND / CLOSED（复用 sales_target_dto::ORDER_STATUSES）
退货白名单 = POSTED（没冲减账的退货先减返利 = 提前认可一笔还没发生的减少）
期间归属   = 订单按 order_date、退货按 return_date，一律左闭右开 [period_from, period_to)
期间码     = MONTH ⇒ yyyy-MM（[当月1日, 次月1日)）｜ QUARTER ⇒ yyyy-QN（[季首, 次季首)）
```

### 落档与金额（落档只有一份实现：`rebate_dto::pick_tier`，M3-15 交付）

```text
落档基数 = PER_UNIT ? 净销量 : 净销售额        （负数按 0，绝不生成负返利）
命中档   = pick_tier(rules, 落档基数)          （左闭右开、末档无上限、不累进）
应返金额 = PER_UNIT ? 基数 × rate（元/件）: 基数 × rate / 100（百分比）  → 保留 2 位（分）
```

### 五道业务闸

| 闸 | 拦什么 | 返回 |
|----|--------|------|
| ① 参数形状 | 期间码不是 `yyyy-MM`/`yyyy-QN`、粒度与期间码不互配、客户 id 非 UUID | 400 + `10001` |
| ② 客户可用 | 客户不存在/已删 → 404；`FROZEN` → 409（冻结客户不再产生新返利） | 404/409 |
| ③ 政策覆盖整期 | 期间首日无启用政策、或政策没覆盖到期间末日 | 409 + **66091**（报文写清缺哪段日期） |
| ④ 期间不重复 | 同客户同粒度同期间已有未作废结算单 | 409 + **66092**（库层 `uk_biz_srs_period` 兜底） |
| ⑤ 状态可改 | 只有 `DRAFT` 能重算/作废；`SETTLED` 只能红冲/调账 | 409 |

**为什么③要求覆盖整期**：政策 11-15 生效、期间是 11 月全月时，全月套新政策 = 前 14 天不该给也算，
全月不算 = 后 16 天该给的不给，两个都是有争议的数。所以不猜：报 66091 并写明缺口，让人去补政策。

## 接口（7 条）

| 方法 | 路径 | 权限码 |
|------|------|--------|
| POST | `/business/erp/rebate-settlement/preview` | `business:erp:rebate-settlement:calc` |
| POST | `/business/erp/rebate-settlement` | 同上（计算落库，`DRAFT`） |
| POST | `/business/erp/rebate-settlement/run` | 同上（整期跑批，逐客户一个事务，幂等） |
| POST | `/business/erp/rebate-settlement/{id}/recalc` | 同上（重算，仅 `DRAFT`） |
| GET | `/business/erp/rebate-settlement/page` | `business:erp:rebate-settlement:list` |
| GET | `/business/erp/rebate-settlement/{id}` | 同上 |
| DELETE | `/business/erp/rebate-settlement/{id}` | `business:erp:rebate-settlement:delete` |

## 验收证据（全部在本目录）

| 文件 | 内容 | 结果 |
|------|------|------|
| `00-unit.out` | `cargo test --lib business::erp::sales::rebate -- --test-threads=1`（真库 = 145 的 `cp_m316_settle`） | **68 passed / 0 failed**（M3-15 34 + M3-16 34：13 纯逻辑 + 12 真库 + 9 其它纯逻辑） |
| `01-clippy.out` | `cargo clippy --lib --all-targets -- -D warnings` | rc=0 |
| `01b-fmt-check.out` | `cargo fmt --check` | rc=0 |
| `02-kernel.out` | `cargo test -p cloudpivot-kernel --lib`（66xxx 码表 stability） | **240 passed / 0 failed** |
| `03-api.out` | 空库首启隔离实例（`:18091`）真机 HTTP **29 组断言** | 全通过 |
| `04-menu-invariants.out` | `scripts/verify/menu_invariants.sh`（8 条断言，跑在 `cp_m316_e2e`） | 全绿（菜单总数 **539** / 上限 540） |
| `05-schema.out` | `\d biz_sales_rebate_settlement` + 索引 + 号段 + 菜单 3 行 + 迁移记录 | 见文件 |
| `06-perm-menu-gate.out` | `scripts/verify/perm_menu_gate.py`（死权限码闸） | **PASS** |
| `07-first-boot.out` | 空库首启迁移链（0 表 → 187 表，V0439 记录成功） | 应用 371 个迁移 |
| `08-migration-drill.out` | 重放（幂等）→ down（回滚）→ 再 up（恢复） | 三步 rc=0，菜单 539 → 536 → 539 |

### 真库用例覆盖（`rebate_settle_db_tests.rs`，12 条）

按量（净 80 × 0.5 = 40，单号 `RB-…` / DRAFT / 快照 / 客户名）、按额（1000 × 2% = 20）、
阶梯季度（150 件落第 2 档 × 2 = 300，不累进、跨年季度）、基数白名单（DRAFT/CANCELLED 订单、
PENDING_POST 退货全不计）、期间重复（409 + 66092 且库里 1 行、试算回显已有单）、
缺政策 / 半期政策（409 + 66091 且报文点名日期；试算降级成提示）、
重算（基数/金额/政策快照一起刷新；`SETTLED` 后 409）、作废（CANCELLED 行留着、同期间可重开、
`SETTLED` 不可作废）、跑批（2 客户 CREATED 合计 150.00；二次跑批全 EXISTS）、
政策引用闸（被未作废结算单引用 → 删政策 409 并提示改停用；作废后可删）、
**并发结算同客户同期间只有一个成功**（另一个稳定 409 + 66092，不是 500）、清场。

### 真机 HTTP（`03-api.out`，29 组）

未登录 401 → 登录 → 建按量政策（`BR-…`）→ 试算（2 单/100 件 − 退货 20 件 = 净 80 × 0.5 = **40.00**，
期间 `2026-10-01 ~ 2026-11-01`、末日 `10-31`，不落库）→ 落库（`RB-…` / DRAFT / 快照 1 档 / 客户名回来）→
重复结算 409 + 66092 → 详情 / 分页（关键字+粒度+期间+状态，命中 1 条）→ 非法状态筛选 400 + 10001 →
重算（单号不变、备注写入）→ 跑批 EXISTS（幂等）→ 作废（CANCELLED 且仍在列表里可审计 / 重复作废 409）→
无政策客户 409 + 66091 + 试算降级 → 月度配季度码 / 非法 UUID 400 + 10001 → 作废结算后政策可删。

## 环境

- 真库：145 的 `cp_m316_settle` = `createdb -T cloudpivot_test`（模板停在 V0404）+ 手工 `psql -f`
  应用 V0437 / V0438 / V0439（⚠ 模板库至今停在 V0404，缺 `biz_customer.credit_policy`，不能直接跑新卡真库用例）。
- 空库首启：`cp_m316_e2e`（0 表）→ 现有二进制（`:18091`，profile=test）启动 →
  371 个迁移全部应用（含 V0439）→ **187 表**；HTTP 冒烟用的订单/退货/客户是直接 SQL 插的夹具
  （HTTP 层才是本卡的验收对象）。
- 用完清理：`:18091` 实例已 kill；`cp_m316_settle` / `cp_m316_e2e` 已 dropdb。

## 留给下游（M3-17 / M3-18 / M3-19）

1. **M3-17**：结算单 `DRAFT` 是唯一可转的入口；转完把 `status` 置 `SETTLED`（本卡已按此语义
   卡死重算/作废两条路，并有真库用例钉住）。金额取 `rebate_amount`，客户/期间/政策快照都在单头上。
2. **M3-18**：本卡已有 13 条纯逻辑 + 12 条真库用例；补充方向是「多币种退货折本位币」
   （需要造应收夹具）与「跑批部分失败」。
3. **M3-19**：结算的 3 个按钮码已挂在「返利政策」页面节点（`…4380`）下，**不需要新建页面节点**；
   页面 `path/component` 收尾仍走 V0438 注释里那条 `UPDATE`。
4. **菜单总数已到 539 / 上限 540**：再加 ≥2 行菜单就破线，届时按 `menu_invariants.sh` 注释流程
   实测后上调上限并同步 `scripts/verify/README.md`。
