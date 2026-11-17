# t_627d2a4b · 返利红字凭证贷记错科目（V0440 的 2241.02 被佣金占用）— 修复留痕

卡：`t_627d2a4b`（FIX【M3·销售】，由 `t_8c53043e` / M3-19F 验收时实测发现后另立）

## 一句话结论

返利负债现在记在**自己的末级科目** `2241.05 其他应付款-销售返利` 上：
全新空库首启后 `2241%` 子树是 `.01 员工垫付 / .02 佣金 / .03 社保 / .04 公积金 / .05 销售返利`，
真打一张返利红字发票，凭证贷方 = `2241.05 其他应付款-销售返利`（用 `voucher::account_required`
的**实时名**验证过，`05_db_state.txt` 第 ① 段的 `live_name` 列）。
`2241.02 其他应付款-佣金` 一个字没动（佣金结算 V0311 / `commission_handler` 仍在用它）。

## 病根（三个条件叠加，每个库都会中，含全新空库首启）

1. `V0311__commission_settle_setup`（佣金结算，SO-36）**先**种了 `2241.02 其他应付款-佣金`；
2. `V0440__sales_rebate_invoice` 第 173-180 行以为 `2241.02` 是空号，写的是
   `INSERT … WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.02')`
   —— 在 V0311 之后必然 `EXISTS` ⇒ **整条种子静默跳过**（不报错、不提示）；
3. V0440 的自检（同文件 291-300 行）只验「存在 + 末级 + 父为 2241 + 负债/贷方」，**不验名称** ⇒ 迁移照样绿。

结果 `rebate_invoice_handler::ACCOUNT_REBATE_PAYABLE = "2241.02"` 落到佣金科目上，
而 `voucher::account_required` 只按 code 取行、名称不参与校验 ⇒ 返利负债与销售佣金混在一个末级科目。

**实测复现**（`01_repro_root_cause.txt`，145 `cloudpivot_dev`，迁移已跑到 V0444）：

```
① 名称带「返利」的科目：0 行        ← V0440 想种的科目根本不存在
② 2241.02 = 其他应付款-佣金（末级）  ← V0311 先占了
③ 把 V0440 的 WHERE 原样跑一遍：would_insert = 0  ← 「静默跳过」被量化
④ 旁证：t_8c53043e 在本机 145 演练库实测的 4 张红字凭证，贷方全是
   `2241.02 其他应付款-佣金`（摘自 ../2026-11-17-m3-19f-rebate-invoice/11_db_state.txt）
```

## 口径决策（两处与卡面写法不同，都有依据）

### ① 新科目是 `2241.05`，不是卡面举例的 `2241.03`

`2241.03 其他应付款-社保(个人)` / `2241.04 其他应付款-公积金(个人)` 早在
`V0384__hr_accounts` 就种下了（144 prod 与 145 dev 实测一致，见 `00_prod_precheck.txt`）。
卡面写「如 2241.03」只是举例；硬套会撞 V0384 的既有科目，`ON CONFLICT DO NOTHING` 又静默跳过
—— 就是同一个病换个号再犯一次。取下一个空号 **2241.05**。

### ② 历史票**不追溯**（不给已有环境补调整凭证）

`V0445` 只种新科目，不自动补「借 2241.02 / 贷 2241.05」的调整凭证。三条理由：

1. **生产（144）实测没有历史错票**：`biz_sales_rebate_*` 表在 144 根本不存在（V0440 还没跑到），
   `2241.02` 上 0 条分录 —— 见 `00_prod_precheck.txt`；
2. 中招的只有 99/145 的**演练库 / 开发库**，那些凭证是夹具造的测试数据；给夹具补调整凭证等于
   往练习环境造假账，还会挡住后续 e2e 的「凭证张数」断言；
3. 调整凭证是**业务动作**（要选未结账期间、要取凭证号、要有制单人、要能被红冲），塞进 schema
   迁移既不幂等也无法审计。

真出现「生产上有真实历史错票」时的现成做法：`../2026-11-17-m3-19f-rebate-invoice/12_history_adjust.sql`
（先查数，再走手工凭证：借 `2241.02` / 贷 `2241.05`，同额、摘要写原凭证号）。
迁移文件头「口径取舍」一节与本卡汇报都写了这条取舍。

## 改动清单

| 文件 | 改动 |
|---|---|
| `backend/app/migrations/business/V0445__sales_rebate_payable_account.up.sql` | **新增**：种 `2241.05 其他应付款-销售返利`（幂等 `ON CONFLICT DO NOTHING`）；**反面教材护栏**（同码异名 / 同名异码一律 RAISE，不再静默跳过）；自检含**名称逐字**比对 + 父级/末级/级次/类别/方向/子科目/「2241.02 必须还叫佣金」；结尾把本库历史错票条数打到日志 |
| `…V0445__sales_rebate_payable_account.down.sql` | **新增**：护栏（2241.05 有凭证分录就拒绝回滚）+ 只删自己种的那行（`remark LIKE 'V0445%'` 且名称相符）+ 自检（佣金科目与 2241 末级位完好） |
| `backend/app/src/business/erp/sales/rebate_invoice_handler.rs` | `ACCOUNT_REBATE_PAYABLE` `2241.02` → `2241.05`；模块头写清「为什么又换了一次、别改回去」与「名称由两道闸兜、运行时故意不比名称」；单测 `account_codes` 加 `assert_ne!("2241.02")` 回归钉子 |
| `backend/app/src/business/erp/sales/rebate_invoice_dto.rs` | 模块文档里的科目码与来由 |
| `backend/app/src/business/erp/sales/rebate_invoice_db_tests.rs` | 断言改为 `["5001","2221.02","2241.05"]` + 新增「`2241.02` 不许出现」+ **验名称**（分录快照名 + `biz_account` 实时名 + 末级位） |
| `scripts/verify/erp_invariants.py` | 新增 **S10**（BLOCKER：2241.05 就位/名称逐字/末级/父 2241 + 2241.02 仍是佣金）与 **S11**（WARN：已开票返利凭证贷方口径，历史票提示不追溯并指向 12_history_adjust.sql） |
| `frontend/src/api/business/erp/rebate.ts`、`frontend/src/views/business/erp/rebate/invoice-logic.ts` | 契约注释里的科目码改成 2241.05（零逻辑改动） |
| `frontend/tests/unit/erp-rebate-invoice.test.ts` | 夹具里的贷方科目与一条 `assert.match(/2241.05/)` 跟着改（22/22 仍全绿） |
| `docs/evidence/2026-11-17-m3-19f-rebate-invoice/12_history_adjust.sql` | **新增**：历史错票的查数 SQL + 人工调账口径（明确「不要塞进迁移、不要裸 SQL 写凭证」） |

**零**新增权限码、**零**新增菜单行、**零**表结构改动（只加一条科目主数据），迁移号 `V0445`
（`V0440~V0444` 已被占，跨目录 `grep` 确认全仓唯一，无重号）。

## 验收（全部真实执行，时间：2026-11-17 19:1x~19:3x CST，机位 99 + 145）

| # | 项 | 结果 | 证据 |
|---|---|---|---|
| ① | **空库首启**（145 新建 `cp_627d2a4b_e2e`，起真后端让它自动跑完 377 个迁移） | `2241%` = `.01 员工垫付 / .02 佣金 / .03 社保 / .04 公积金 / **.05 销售返利（level 2, is_leaf 1, LIABILITY/CREDIT）**`；`schema_migrations` 里有 `V0445__sales_rebate_payable_account.up.sql` | `02_migration_idempotent_and_down.txt`、`05_db_state.txt` |
| ② | 迁移**幂等 + 可回滚** | `up` 重跑：`INSERT 0 0 / UPDATE 0` + 自检通过；`down`：`DELETE 1` + 2241.05 消失、佣金与 2241 完好；再 `up`：`INSERT 0 1` 回到原样 | `02_migration_idempotent_and_down.txt` |
| ③ | **不再静默跳过**（反面教材护栏实测） | 同码异名 → `ERROR: V0445 撞号…`；同名异码 → `ERROR: V0445 断链…`；恢复后重跑干净通过 | `03_guard_not_silent.txt` |
| ④ | **真打一张返利红字发票**（真后端 :18027 → 145 真库） | 试算 `voucher_lines` 贷方 = `2241.05 其他应付款-销售返利`；开票 200，`IV-20261117-000001 / PZ-20261117-000001`，结算单 `SETTLED` | `04_api_chain.txt` |
| ⑤ | 凭证**贷方科目 = 新科目（实时名，不只比 code）** | 分录：`5001 主营业务收入 884.96 借 / 2221.02 应交税费-销项税 115.04 借 / 2241.05 其他应付款-销售返利 1000.00 贷`；`snapshot_name == live_name == 其他应付款-销售返利`；2241.02 上本次凭证 0 行 | `05_db_state.txt` |
| ⑥ | `cargo test --lib business::erp::sales`（`ERP_DB_URL` 指真库，`--test-threads=1`） | **326 passed, 0 failed**（含 `rebate_invoice_db_tests` 8 条真库用例，无 SKIP） | `08_cargo_test_sales.txt` |
| ⑦ | `rustfmt --check`（本卡 3 个 .rs） | 通过 | 终端 `FMT_OK` |
| ⑧ | `scripts/verify/erp_invariants.py --group 销售` | **11/11 通过，BLOCKER 0 / WARN 0**（S10/S11 在真数据上有覆盖：`已开红字发票的返利结算单 1 张`） | `06_erp_invariants_sales.txt` |
| ⑨ | **S10/S11 反向验证**（证明闸不是永远绿） | 改坏 2241.05 名称 → S10 **BLOCKER**；把贷方行改回 2241.02（模拟历史票）→ S11 **WARN** 并列出结算单号；还原 → 11/11 全绿 | `07_negative_test.txt` |
| ⑩ | 前端（受影响的契约注释与夹具） | `npx tsx --test tests/unit/erp-rebate-invoice.test.ts` **22/22 pass**；`vue-tsc --noEmit` 见 `10_typecheck.txt` | `09_frontend_unit_rebate_invoice.txt`、`10_typecheck.txt` |
| ⑪ | 生产现状（为什么可以「不追溯」） | 144 无 `biz_sales_rebate_*` 表、`2241.02` 0 条分录 | `00_prod_precheck.txt` |

## 复跑方式

```bash
# 空库首启 + 真链路（145 建库，99 起后端）
scp 无需；在 145：su postgres -c 'createdb -O cloudpivot cp_627d2a4b_e2e'
# 99 上起后端（自动迁移到 V0445）：
cd /opt/cloudpivot/backend
set -a && . <本机 145 库口令 env> && set +a
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18027 NIK_DATABASE_HOST=172.18.10.145 \
NIK_DATABASE_NAME=cp_627d2a4b_e2e NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 32) \
NIK_ADMIN_PASSWORD=Admin@123456 ./target/debug/cloudpivot
# 首次登录必须先改密：PUT /api/system/auth/password {"old_password","new_password"}
# 夹具：docs/evidence/2026-11-17-m3-19f-rebate-invoice/20_fixture_settlements.sql + 21_fixture_extra.sql
# 真库单测：ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_627d2a4b_e2e' \
#           cargo test --lib business::erp::sales -- --test-threads=1     # 必须 --test-threads=1，否则连接被掐
# 体检：NIK_DATABASE_* 指该库 + PGPASSWORD，python3 scripts/verify/erp_invariants.py --group 销售
```

## 与卡面的差异 / 读卡人要知

- **新科目号 2241.05**（卡面举例 2241.03，已被 V0384 的社保科目占用）—— 见上「口径决策 ①」。
- 卡面「要么调整凭证、要么写明不追溯」：本卡选**写明不追溯**并留了现成调账 SQL（12_history_adjust.sql）。
- 卡面要求改的三处（常量 / dto 文档 / 单测断言）都改了；另外**多改**了三处并说明理由：
  `erp_invariants.py` 新增 S10/S11（名称一侧过去完全无人守，这才是 bug 的根）、
  db 单测加名称断言（验收要求「别只比 code」）、前端契约注释与夹具（否则仓库里留着错的科目码）。

## 清理

- 演练库 `cp_627d2a4b_e2e`（145）与后端进程 :18027 用完即删/停（见卡内汇报 `cleanup` 字段）；
- 未新增任何 99 本机依赖（未装 redis 之外的任何东西；本次未用 redis）。
