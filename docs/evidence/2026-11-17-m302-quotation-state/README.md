# M3-02 销售报价单：CRUD + 状态机（草稿 → 已发 → 已接受 / 已拒绝）

卡：`t_0750ad31`｜上游 `t_6a61726c`（M3-01，V0432 建表）｜日期 2026-11-17

## 一、这张卡交付了什么

| 类别 | 落点 |
|---|---|
| 数据层 | `backend/app/src/business/erp/sales/quotation_model.rs`（行结构 / 取号 / 建单 / 改单 / 软删 / 分页 / CAS 状态落库） |
| 接口层 | `backend/app/src/business/erp/sales/quotation_handler.rs`（6 个端点 / 入参校验 / 3 个业务守卫 / 出参 DTO） |
| 状态机 | `state_machine::QUOTATION_EDGES`（6 条边）+ `QUOTATION_BASE` / `QUOTATION_REASON_EDGES` / `QUOTATION_LOCKED`，注册到 `Entity::Quotation`（注册表 29 → 30） |
| 词表 | `DocKind::Quotation`（`QUOTATION` / 「报价单」/ `biz_quotation` / 非可审批单据），`DocKind::ALL` 36 → 37 |
| 迁移 | `V0433__quotation_permissions`（5 个权限码 = 1 页面节点 + 4 按钮，挂在「销售管理」下 sort=15） |
| 前端镜像 | `doc-kinds.ts` 三张常量表 + `doc-kinds-mirror.test.ts` 计数（36 → 37） |
| 巡检登记 | `docs/architecture/snapshot-drift/10_checks.sql` 的 `STATUS_ENTITY` + `QUOTATION` 行 |
| 验收脚本 | `scripts/verify/m302_quotation_state_e2e.sh`（空库首启 + 真接口）+ `.py`（74 条断言） |

## 二、接口清单（6 个端点 / 5 个权限码）

| 方法 | 路径（前缀 `/api`） | 权限码 | 说明 |
|---|---|---|---|
| POST | `/business/erp/quotation` | `business:erp:quotation:create` | 建单（`DRAFT`，取号 `QT-yyyyMMdd-000001`） |
| PUT | `/business/erp/quotation/{id}` | `business:erp:quotation:update` | 改单（**只有 DRAFT**，明细整单替换） |
| GET | `/business/erp/quotation/{id}` | `business:erp:quotation:list` | 详情（含明细 + 客户名 + 物料名 + `expired` + `allowed_targets`） |
| GET | `/business/erp/quotation/page` | `business:erp:quotation:list` | 分页（`quote_no` 模糊 / `status` / `customer_id` / `expired` 现算） |
| PATCH | `/business/erp/quotation/{id}/status` | `business:erp:quotation:status` | 状态流转（只走 6 条合法边，非法 409） |
| DELETE | `/business/erp/quotation/{id}` | `business:erp:quotation:delete` | 软删（**只有 DRAFT / CANCELLED**） |

## 三、状态机（6 条边，与 `ck_biz_quotation_status` 5 值逐字对齐）

```text
DRAFT ──发出──▶ SENT ──客户接受──▶ ACCEPTED ──客户反悔/录错(要原因)──▶ CANCELLED
  │              │  └──客户拒绝(要原因)──▶ REJECTED（终态）
  └──草稿作废(不要原因)──▶ CANCELLED ◀──撤回(要原因)──────────────────┘
```

| 决定 | 理由 |
|---|---|
| 卡面 3 条正向边 + 3 条反向边 | ARCH-7 §12.3「正向必配反向」。只有正向边时报价单一发出去就永久停在 `SENT`，界面上没有任何可点的按钮（AGENTS.md 约定 11 同类坑） |
| **没有** `SENT → DRAFT` | 报价发出后客户手里那份是「报了多少」的唯一凭据，原地打回草稿改价 = 纸面与库里对不上；改价走**新报价单** |
| **没有** `REJECTED → *` / `CANCELLED → *` | 两个终态；要重报就开新单（`REJECTED → DRAFT` 会让「被拒过」的历史消失） |
| 需要原因的边 = `SENT→REJECTED`、`SENT→CANCELLED`、`ACCEPTED→CANCELLED` | 拒绝理由是下一版报价的唯一输入；已对外发出后作废要能回答「为什么后来不算了」。`DRAFT→CANCELLED` 不要原因（从未对外） |
| 入账态 = `SENT/ACCEPTED/REJECTED/CANCELLED`（**只有 DRAFT 可写**） | 发出即冻结单价与明细（V0432 建表口径）；`ensure_writable` 语义与销售订单同型但更严（销售订单还有审批打回草稿的回头路） |

## 四、三个业务守卫（卡面「状态流转有校验」的落点）

| 边 | 守卫 | 表现 |
|---|---|---|
| `DRAFT → SENT` | ① ≥1 行明细 ② 客户未冻结 ③ 有效期未过 | 400 / 400 / **409**「有效期已过」 |
| `SENT → ACCEPTED` | 有效期未过 | **409**（过期价不能被接受，要重报） |
| `ACCEPTED → CANCELLED` | 未转销售订单（`converted_so_id IS NULL`） | **409**（已转单作废会让订单失去来源，留给 M3-03 之后的作废流程） |

「已过期」一律**现算**（`today > valid_until`，CST），不入库、不进状态机 ——
V0432 建表注释的既定口径（做过期状态就要定时翻转 + 时钟回拨对账）。

## 五、实测结果（本目录 `raw/` 是原始输出）

| # | 命令 | 结果 |
|---|---|---|
| 01 | `bash scripts/verify/m302_quotation_state_e2e.sh` | **PASS=24 FAIL=0**；其中真接口断言 **74 条全绿**（建单 / 分页 / 改单 / 发出 / 接受 / 拒绝 / 作废 / 软删 / 过期护栏 / 400 校验），`biz_status_log` 轨迹 8 条断言，`menu_invariants.sh` 8 条全绿 |
| 02 | `ERP_DB_URL=… cargo test --lib business::erp::sales::quotation` | **21 passed / 0 failed**（16 条纯逻辑 + 5 条真库：建单 / 取号连号 / CAS 改单 / CAS 状态 / 软删 / 分页筛选） |
| 03 | `cargo test -p cloudpivot-doc --lib` / `-p cloudpivot-kernel --lib` | 19 / 240 passed —— 含「前端 TS 镜像逐字一致」「`Entity::ALL` ∈ `DocKind` 域」「`STATUS_ENTITY` 登记表覆盖全量」 |
| 04 | `cargo fmt --check`（本仓 3 个包） / `cargo clippy --lib -- -D warnings` / `scripts/check.sh --l0` / `pnpm test:unit` / `pnpm build` | 全绿（前端单测 2119 passed） |

实测数字（空库首启，`schema_migrations.success = 365` / 最大版本 `433`）：

```text
建单 4 张（主链 / 拒绝链 / 草稿作废 / 过期→改期）
biz_status_log(QUOTATION)：建单 4 · DRAFT→SENT 3 · DRAFT→CANCELLED 1
                           SENT→ACCEPTED 1 · SENT→REJECTED 1 · ACCEPTED→CANCELLED 1
非法迁移（ACCEPTED→REJECTED / REJECTED→SENT）落库 0 行
```

## 六、踩到的坑（写给下一张卡）

**`Value::Text` 不能直接配 `?::date`** —— 驱动的 `PgParam::Text` 只接受文本家族参数；
PG 看到 `?::date` 会把该参数类型定成 `date`，执行期报 `error serializing parameter N`
（对外是 500 + 10004，日志里看不出是日期问题）。正确写法：`?::text::date`
（与 `sales_order_model` 的 `NULLIF(?::text,'')::date` 同一手法）。
本卡实测踩过一次，已在 `quotation_model.rs` 的 `create_quotation` 文档里写死。

**`cargo fmt --all` 会格式化 `vendor/`**（835 处 diff，152 个文件）：本仓 `Cargo.toml` 的
`[workspace] exclude` 挡得住 `cargo clippy`，但挡不住 `fmt --all`。要用
`cargo fmt -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel`，提交前确认
`git status` 里没有 `backend/vendor`。

## 七、留给下游

- **M3-03 转订单**：`converted_so_id` / `converted_at` 两列已就位，`uk_biz_quotation_converted_so`
  是反向唯一（一张订单只能由一个报价单认领）；转单只允许 `ACCEPTED`（本卡已在
  `ACCEPTED → CANCELLED` 上挡了「已转单还作废」）。
- **M3-04 审批流**：`DocKind::Quotation::is_document() = false`；接审批时把它挪进清单，
  并加 `FlowBizType::QuotationApprove`。
- **M3-06 前端**：`sys_menu` 的页面行 `path` / `component` 留空、`is_visible = FALSE`，
  前端路由落地后一条 `UPDATE` 收尾（SQL 写在 V0433 的注释里）。
- **未做的两件事（刻意）**：`scripts/verify/README.md` 的脚本清单行没加（该文件正被其它卡
  改动，共享工作区里逐 hunk 提交风险大于收益，合并后补一行即可）；
  报价单**没有**附件 / 提醒口径（`ATTACHMENT_DOC_KINDS` 未纳入，`is_document=false`）。
