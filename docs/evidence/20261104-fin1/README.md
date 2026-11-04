# 【FIN-1】预算管理实施（ARCH-6 第三章）· 验收证据

卡：`t_af3d638d` ｜ 设计依据：`docs/architecture/ARCH-6-财务扩展设计.md` 第三章（§3.1–§3.9）+ §6.1/§6.2/§6.3/§6.6/§6.7/§6.9 + §7.3
落地：本地 `main`，提交 `feat(fin): FIN-1 预算管理…`

---

## 一、设计文档对应章节（逐条落地位置）

| 设计章节 | 落地点 |
|---|---|
| §3.1.1 五个数（额度 / 占用中 / 已执行 / 可用**现算**） | `budget_guard::slot_snapshot` — 三条聚合现算，库里**没有**任何 `used_amount` 缓存列 |
| §3.1.2 两道闸（年 + 月）+ 超支额取更大缺口 | `budget_guard::BudgetVerdict::evaluate`（纯函数，6 组口径单测钉死） |
| §3.1.3 占用生命周期（OCCUPIED / CONSUMED / RELEASED）+ **释放不删行** | `budget_guard::{occupy, release, consume}`；`ck_biz_budget_usage_done` 保证 `RELEASED ⟺ released_at IS NOT NULL` |
| §3.2.1–§3.2.5 四张表 DDL | 迁移 `V0227__erp_budget_tables.up.sql`（列名 / 约束 / 索引逐字取自设计，未自创列） |
| §3.2.4 `fiscal_year = left(period_code,4)` | `ck_biz_budget_usage_year`（库级约束） |
| §3.3 状态机 7 条边 + 3 条 reason 边 | `state_machine::{BUDGET_EDGES, BUDGET_BASE, BUDGET_REASON_EDGES}` + `Entity::Budget` 六处注册（`base_edges_match_registry` 单测对齐） |
| §3.4.2/§3.4.3 编制审批 + 编制/调整双分支（`form.change_type`） | 迁移 `V0228__erp_budget_approval_flow.up.sql`（11 节点 / 13 边，50 万阈值，`FALLBACK_ADMIN`，`self_approval=ALLOW`） |
| §3.4.4 `form_data` 契约（`form.change_type` / `form.total_amount` …） | `budget_handler::budget_form_data`（字段名逐字对齐，金额是 JSON 数字） |
| §3.5.1 控制点① 报销提交 | **本卡未接**（报销模块属 FIN-3，见「差异 D1」）；守卫写成不依赖报销表的纯函数 + 入参结构体 |
| §3.5.2 控制点② 采购提交 | `purchase_handler::submit_approval` 插只读守卫 + `submit_po_tx` 同事务写 `OCCUPIED`；`form_data.over_budget` 标记 |
| §3.5.3 控制点只有 2 处 | 只有采购提交一个真实落点（另一个在 FIN-3）；收货 / 审批通过 / 付款**都没有**加校验 |
| §3.5.4 纯函数 + 6 组单测 | `budget_guard_tests.rs`（11 条：年超/月超/双超/不超/无预算/刚好用完/一分钱/已用超限/策略解析/槽构造/报文六数） |
| §3.6 并发与幂等（不锁，靠唯一键 + 快照） | `uk_biz_budget_usage_source`；`occupy` 把 23505 当幂等成功返回 `None`；`pg_advisory_xact_lock` 按设计**预留不实现** |
| §3.7 调整 = 新增版本（`version_no+1` + `supersedes_id`），旧版 `ACTIVE→SUPERSEDED` 同事务 | `budget_handler::adjust` + `budget_model::supersede_active` + `budget_bridge::settle` |
| §3.7.2 第 3 条坑：同槽只允许一条在审调整单 | `budget_model::pending_adjust_of_slot`（显式查，不依赖 `uk_biz_flow_instance_biz_active`） |
| §3.8 报表（执行率 / 可用现算 / 超支标红） | `budget_model::report` + `ReportRow::{used, available, exec_ratio}`、`GET /business/erp/budget/report` |
| §3.9 前端三页 | **本卡只交付后端 + `doc-kinds.ts` 常量**（前端页面属独立前端卡） |
| §6.2 词表 | `DocKind::Budget`（14→15）、`FlowBizType::BudgetApprove`（13→14）、`COLUMN_DOMAINS`（14→17，含 3 条预算列）、前端 `doc-kinds.ts` 同步 |
| §6.3 状态机注册 | `Entity::Budget`（10→11）+ 常量 `ENTITY_BUDGET` |
| §6.6 科目种子 | `V0226`：`2241.01` + `5602.01…09`（8 条）+ `2241/5602` 翻 `is_leaf=0` + `5601/5602/5603/5711` 标 `aux_dimensions='DEPT'` |
| §6.7 权限码 / 菜单 | `V0227`：8 个 `budget:*` 权限码 + 预算管理目录 + 3 页 + 5 个按钮码；授给 SUPER_ADMIN/ADMIN/FINANCE_MANAGER/FINANCE_DIRECTOR/DEPT_LEADER |
| §6.9 迁移纪律 | 文件内无 `BEGIN/COMMIT`；`IF NOT EXISTS` / `WHERE NOT EXISTS` / `ON CONFLICT DO NOTHING`；每条末尾 `DO $$` 自检 |

---

## 二、与设计的差异（逐条说明原因）

| # | 差异 | 原因 |
|---|---|---|
| **D1** | 控制点①（报销提交）**本卡未接线** | 设计的落点是 `expense_handler::submit`，而 `biz_expense_report` 表与报销模块属 **FIN-3**（board 上 todo）。本卡把守卫写成 `budget_guard::check_for_expense` 的等价形态（`BudgetVerdict` + `Slot` + `occupy / release / consume` 全部与单据类型无关，只差一个「报销单行→科目」的解析函数），FIN-3 接线时无需改守卫。**真机全链路用采购单跑通控制点②**（与设计 §3.5.2 落点一致）。 |
| **D2** | 迁移号段从 **V0226** 起，不是设计里的 V0220 | 设计定稿时实测最大号是 V0219；本卡开工时 `V0220`（P-FLOW 标准标志）/`V0221`（会计期间种子）已被其它卡占用、`sys/` 还有 `V0225`。号段顺延为 **V0226（科目+配置）/ V0227（四表+权限+菜单）/ V0228（流程种子）**，内容与设计 §6.9 的表逐条对应。 |
| **D3** | 附件 / 提醒两类 CHECK 放宽（设计 §6.5）**未在本卡做** | 那两个新值（`EXPENSE_REPORT` / `FIXED_ASSET`）属于 FIN-3 / FIN-2 的单据类型。现在放宽会让「库允许、词表没有」的域不对齐（`biz_type_domain.py` 会红）。留到对应卡与词表一起改。 |
| **D4** | 预算单号形如 `BDG-yyyyMMdd-000001`，设计写的是 `BDG-2026-000001` | 复用全库唯一的取号通道 `voucher::next_doc_no`（`biz_seq` 按天滚动），与 PO/PZ/AP 同款；年度滚动需要另一套取号语义，收益低于统一维护成本。 |
| **D5** | `budget_guard::check_for_expense` 方法名未落地 | 见 D1：本卡只有 `check_for_purchase` + 与单据无关的 `check_slot`。FIN-3 接线时用 `check_slot` 组装，不新增第二套口径。 |
| **D6** | 采购侧「物料类型」映射按 `biz_material.material_type` 文本编码匹配 | 设计 §3.2.5 的 `MATERIAL_TYPE` 没写死是 id 还是编码；实测 `biz_material` 同时有 `material_type` 编码列，比再引一张类型表更省事，且映射表的 `ref_code` 就是为这种场景留的。 |
| **D7** | `cargo fmt` 在整 crate 范围跑过 | 提交只 `git add` 本卡路径；**但**工作区里其它卡正在改的文件（`voucher.rs` / `serial_handler.rs` / `startup_guard.rs` / `period_*` 等）被 rustfmt 顺带重排了几行，那些文件本卡**未提交**，合并时请以各自卡为准。 |
| **D8** | 采购单 `CONSUMED` 的时点 | 设计 §3.1.3 写「收货完成或订单关闭」。本卡实现在 **订单关闭（`CLOSED`）** 转 `CONSUMED`；「收货完成」那一刻的转档要改收货过账链（属采购-收货卡），本卡不动那条链（§3.5.3 明确「收货过账不校验」，但也没要求我改它）。 |

---

## 三、验收证据（`raw/` 目录，全部实测输出）

| 文件 | 内容 | 结果 |
|---|---|---|
| `raw/01-e2e-full-chain.txt` | 真机全链路 19 步（隔离实例 `cloudpivot_fin2` :18024） | **19/19 PASS** |
| `raw/05-read-endpoints.txt` | 三个只读接口（分页 / 报表 / 映射分页） | HTTP 200 |
| `raw/02-invariants.txt` | `scripts/verify/erp_invariants.py` 全量 | **40/40 通过，BLOCKER 0** |
| `raw/03-biz-type-domain.txt` | `scripts/verify/biz_type_domain.py`（17 列值域，含 3 条预算列） | PASS |
| `raw/04-flow-graph-hash.txt` | `scripts/verify/p22b_rows_hash.py` 复算种子流程快照 | stored == computed，drifted 0 |
| `raw/06-unit-tests-budget.txt` | `cargo test --lib business::erp::budget`（含真库用例） | **30 passed** |
| `raw/07-unit-tests-shared.txt` | `cargo test --lib business::shared::`（词表 / 状态机护栏） | **108 passed** |
| `raw/08-migration-updown.txt` | V0226–V0228 **down→up 三轮**（每轮自检 NOTICE） | 三轮全过，残留表 0 |
| `raw/09-regression-purchase.txt` | `cargo test --lib business::erp::purchase`（本卡唯一侵入面） | **79 passed，0 failed** |

### 全链路 19 步摘录

```
PASS 0. budget_approve 种子流程通过引擎 validate()
PASS 1. 编制预算（DRAFT v1，12 月分解）；1b Σ=年度额度；1c 期间重复 → 400
PASS 2. 提交审批；2b 审批通过后 budget.status=ACTIVE（2 个节点）
PASS 3. 新建预算科目映射（DEFAULT → 5602.02）
PASS 4. 建采购单（含税 5 万）
PASS 4b 超预算采购单提交成功（策略 B，不是 409）
PASS 4c usage = OCCUPIED / over_budget=1 / over_amount=40000（当月额度 1 万、本次 5 万）
PASS 4d 审批单 form_data.over_budget=true
PASS 5. 取消采购单；5b usage → RELEASED（留痕 release_reason）
PASS 6. 报表可用余额回到 120000（额度未泄漏）
PASS 7. 调整（15 万）建 v2 并送审；7b v1 ACTIVE / v2 SUBMITTED(ADJUST, supersedes=v1)
PASS 7c 调整通过 → v1 SUPERSEDED / v2 ACTIVE（同槽只一个 ACTIVE）
PASS 8. control_mode=BLOCK 时提交 → 409 + 66037
PASS 8b BLOCK 阻断时不留 usage（额度没被半张单吃掉）
```

---

## 四、凭证口径（设计要求写明「预算不产生凭证」的理由）

预算审批**不落账、不生成凭证**，理由（设计 §6.4 边界 ① / 本章红线）：

1. **预算是额度控制，不是经济业务发生**：编一笔 12 万预算不代表花了 12 万，会计上没有任何科目应被借记或贷记；把它记成凭证等于凭空造费用。
2. **执行侧的凭证由既有链路产生**：采购入库走 `PURCHASE_IN_DEFAULT`（已有），报销入账/付款由 FIN-3 新增两条 rule —— 预算模块一行凭证代码都不需要。`biz_voucher_rule` / `VoucherSourceType` 本卡**没有新增任何值**。
3. **可用余额是派生量**：它是「额度 − Σ有效占用」，落成凭证反而会让「额度」与「账」两个来源打架（ARCH-6 D5）。
4. 因此本卡的单据只写 `biz_status_log`（状态机唯一入口）+ `biz_budget_usage`（占用事实），不写 `biz_voucher*`。

---

## 五、复跑命令

```bash
# 1) 全新库跑全量迁移（含 V0226–V0228）
createdb cloudpivot_fin1 && \
env NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_fin1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
    NIK_ADMIN_PASSWORD=Admin@123456 NIK_SERVER_LISTEN=127.0.0.1:18024 \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=<32+ chars> CLOUDPIVOT_STORAGE_MASTER_KEY=<32 hex> \
    ./target/debug/cloudpivot

# 2) 单测（真库用例要 ERP_DB_URL）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin2' cargo test --lib business::erp::budget

# 3) 真机全链路（脚本见卡内 comment；跑在隔离实例上，不碰生产）
python3 e2e_fin1.py http://127.0.0.1:18024/api cloudpivot_fin2

# 4) 不变量 / 词表 / 流程快照
NIK_DATABASE_NAME=cloudpivot_fin2 python3 scripts/verify/erp_invariants.py
NIK_DATABASE_NAME=cloudpivot_fin2 python3 scripts/verify/biz_type_domain.py
python3 scripts/verify/p22b_rows_hash.py cloudpivot_fin2 20000000-0000-4000-8000-0000000c0228
```
