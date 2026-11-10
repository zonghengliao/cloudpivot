# COST-B2-5 分摊规则后端 —— 证据与交付说明

- 卡：`t_1be97cdf`（COST-B2-5【成本】分摊规则后端）
- 批次：B2 归集（ARCH-9 §8.1 B2-5）
- 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ⑦⑧ / §4.4 / §5.2 #20–#24 / §5.3 / §6.2.3
- 迁移：**本卡不新增迁移**（表/索引/CHECK 在 V0372，5 个权限码 + 页面菜单在 V0378）
- 代码：`backend/src/business/erp/cost_alloc_rule_{dto,model,handler}.rs` + `cost_alloc_rule_db_tests.rs`

---

## 1. 交付了什么

| # | 方法 | 路径 | 权限码 | 行为 |
|---|---|---|---|---|
| 20 | GET | `/business/erp/cost-alloc-rule` | `business:erp:cost-alloc-rule:list` | 分页；筛选 期间 / 车间 / 科目 / 分配标准 / 状态 / 关键字；`period_code` 表示「只看生效期覆盖该期间的版本」 |
| 21 | POST | `/business/erp/cost-alloc-rule` | `...:create` | 建一条规则版本（编码唯一、科目必须 4101.* 且存在+末级+启用、同 (车间,科目,生效日) 唯一） |
| 22 | PUT | `/business/erp/cost-alloc-rule/{id}` | `...:update` | 改规则；**已参与过分摊的版本只允许改 名称/备注/状态**，改口径 409 |
| 23 | DELETE | `/business/erp/cost-alloc-rule/{id}` | `...:delete` | 软删；已有分摊结果则 409（不许让历史结果的 rule_id 指向删掉的规则） |
| 24 | POST | `/business/erp/cost-alloc-rule/preview` | `...:preview` | **试算**：给期间（+ 可选 `rule_ids`）→ 逐规则逐对象的 `basis_value/basis_total/pool_amount/alloc_amount/rounding_adj`，**只算不写** |

卡面三条验收的落点：

| 验收项 | 落点 | 证据 |
|---|---|---|
| 规则按生效期选最新版本 | `dto::pick_latest_version` + `model::active_rules` | 单测 5 条（`latest_version_wins_within_the_period` 等）；真库用例 ①；HTTP 冒烟第 6/7/9 步 |
| preview：给期间 + 池金额 → 各对象分摊额（不落库） | `handler::preview_allocations` | 真库用例 ②③；HTTP 冒烟第 6 步（`biz_cost_alloc_result` 行数保持 0） |
| 单测：四种分配标准各一例 | `dto::basis_value` 四分支 | 单测 4 条 + 真库用例 ②（四种标准同车间同期，逐个核基数与分摊额） |

---

## 2. 实测输出

### 2.1 纯逻辑单测（`01_unit_tests.out`）

```
cargo test --lib cost_alloc_rule_dto      → 30 passed / 0 failed
cargo test --lib cost_alloc_rule_model    → 13 passed / 0 failed
cargo test --lib cost_alloc_rule_handler  →  8 passed / 0 failed
```

覆盖：四种分配标准各一例、最大余数法（§4.4.4 的算例 B1 整除 / B2 除不尽）、97 个对象的守恒、
期间边界（含闰年 2 月 / 跨年 12 月）、版本选择的五种边界（最新版赢 / 收口版失效 / 停用跳过 /
未生效跳过 / 同一天并列按 rule_code 确定）、规格组装与写库错误翻译、权限码与路由形状。

### 2.2 真库用例（`02_real_db_tests.out`）

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_b24 \
  cargo test --lib cost_alloc_rule_db -- --test-threads=1
→ 7 passed / 0 failed（11.7s，跑完零残留：rule/object/account/period 均 0 行）
```

| 用例 | 断言要点 |
|---|---|
| `rule_crud_and_effective_version_selection` | 建两条版本 → 11 月试算取 11-01 版本、10 月取 1 月版本；停用新版后回落；同 (车间,科目,生效日) → 409；编码重复 → 409；改名 / 双删（第二次 404） |
| `preview_uses_all_four_bases` | 四标准同车间同期：LABOR_HOURS 60/300/100、MACHINE_HOURS 6 基数、OUTPUT_QTY 600/200/200、MATERIAL_COST 166.6667/333.3333/500；四组合计 = 池 4000；**没投入的对象不参与分摊** |
| `preview_rounds_remainder_onto_one_row` | 池 1000、基数 1:1:1 → `333.3334/333.3333/333.3333`，尾差 0.0001 只在唯一一行，Σ = 1000 |
| `preview_refuses_pool_without_object_or_basis` | 池 4000 无在产对象 → 409 + `66062`；池 800 按机器工时摊但机器工时全 0 → 409 + `66063` |
| `referenced_rule_cannot_be_edited_or_deleted` | 有 `biz_cost_alloc_result` 引用：改 basis → 409、删除 → 409（点名期间）；改名/停用放行；删结果后可删 |
| `unassigned_pool_is_reported_not_swallowed` | 来源单据已删的池行 777 → 出现在 `unallocated` 诊断（科目未知 + 原因），不静默丢弃 |

### 2.3 HTTP 冒烟（真实例 + admin 真登录，`03_http_smoke.out`）

隔离库 `costb25_http` / `:18028`（`00_up.sh up|down`），5 个接口全通 + 3 类反证：

```
GET  列表（空）                      → code=0 total=0 page_size=20
POST 建旧版本（2026-01-01 工时）      → code=0 basis=LABOR_HOURS 科目 4101.02/制造费用-水电费
POST 建新版本（2026-11-01 机器工时）  → code=0 priority=20
GET  列表（period+dept+account）     → total=2（两条版本都在）
POST /preview 2026-11                → 选中 WL-…-V2（2026-11-01 版本）✓
POST /preview 2026-10                → 选中 WL-…-V1（1 月版本）✓ 历史不被改口径
PUT  改名+停用                        → code=0 status=0/停用
POST /preview 2026-11（停用后）       → 回落 V1 ✓
DELETE ×2                            → HTTP 200/200；列表 total=0
POST 科目 5602.01                    → HTTP 400 + 66060（文案点名「只摊 4101.*」）
POST preview period_code=2026-13      → HTTP 400 + 10001
GET/POST 无 token                     → HTTP 401
GET 未知路径                          → HTTP 404
```

### 2.4 门禁（`04_gates.out` + `99_full_test.raw`）

```
cargo fmt -- --check                        → clean
cargo clippy --all-targets -- -D warnings   → 0 warning / 0 error
cargo test（全仓）                          → 33 个目标全 ok，4722 passed / 0 failed
```

---

## 3. 已知偏差（逐条具名）

1. **池金额的科目是「反查」出来的，不是 `biz_cost_collect.account_code`。**
   设计 §4.4.3 ① 的池公式写 `… AND dept_id = dept AND account_code = account`，
   但 §3.1 ③ 的 `biz_cost_collect` DDL（V0371 实际落地）**没有 `account_code` 列**。
   本卡按来源单据反查：

   | `source_type` | 反查表 | 科目列 |
   |---|---|---|
   | `OVERHEAD_TICKET` | `biz_cost_overhead` | `debit_account_code` |
   | `DEPRECIATION` | `biz_depreciation` | `expense_account_code` |
   | `EXPENSE` | `biz_expense_item` | `account_code` |

   三路之外没有池行（`ck_biz_cost_collect_obj` 只允许这三类不挂对象）。反查不到的行
   （来源单据被删 / 池行没车间）**不静默丢弃**，由 `model::unassigned_pool` 统计并作为
   `unallocated` 诊断返回。

   > **给后续卡的影响（只报不改）**：B2-8（费归集）写池行时、B2-9（池与 4101 对账）
   > 对账时**必须用同一套反查口径**，否则试算/分摊数与对账数对不上。`model::pool_rows`
   > 已经写成可复用函数，B2-9 直接调它即可，不必再写一份。

2. **「按期间取生效版本」的口径本卡落成「区间有交集 + 取生效最晚」。**
   期间是月、生效期是日：`effective_from = 2026-11-15` 的规则对 `2026-11` 生效（本月改口径本月生效）、
   对 `2026-10` 不生效。同一天两条并列时取 `rule_code` 字典序最小（避免结果随行序漂移）。

3. **「参与分摊的对象」判据比设计文字宽一处。**
   设计写「当期有料或工的**归集行**」；本卡额外把「当期 `CONFIRMED` 的工时台账行」也算投入。
   理由：试算的用途是 **calc 之前**先看摊多少，而 `biz_cost_collect` 的工行要等 calc 第 2 段才写，
   只认归集行会让试算在 calc 之前永远报「无在产对象」——那正好是这个接口最该有用的时刻。

4. **已参与过分摊的规则版本不许改口径 / 删除（409）。**
   设计只说「改规则不溯及历史」；法规化落地：`biz_cost_alloc_result.rule_id` 已有引用时，
   PUT 只放行 `rule_name/remark/status`，DELETE 直接拒绝并告诉用户「新建一条生效日为下期的规则」。
   删除前查引用与 B2-4「删除前查归集」同一范式（AGENTS 约定 11）。

5. **同 (车间, 科目, 生效日) 唯一是入口校验，不是库约束。** 设计没给它唯一索引（V0372 已应用不可改），
   本卡在 `create_rule` / `update_rule` 里 409 拦（`model::version_conflict`）；若将来要库层兜底需新迁移。

6. **新增两个错误码**：`66062 ERP_COST_ALLOC_NO_OBJECT`（池有金额但无在产对象）、
   `66063 ERP_COST_ALLOC_BASIS_ZERO`（该标准基数合计 0）。两者都是 409、修法不同所以不合并。
   科目类错误复用 `66060`（动作都是「换科目」，与 B2-4 一致）。

7. **数值出参固定 4 位小数**（`dto::money4`）。`Decimal::to_string()` 会把 `2400.0000` 打成 `2400`，
   前端「合计 = 池」字面比较与表格对齐都会抖；`NUMERIC(18,4)` 的标度就是 4。

8. **无新增迁移**：表 / 索引 / 5 条 CHECK 在 V0372，5 个权限码与页面菜单在 V0378 ⇒
   卡面「迁移幂等 + 配套 `.down.sql`」对本卡 N/A，未碰任何已应用迁移。

9. **数据权限**：`biz_cost_alloc_rule` 不在设计 D-18 的登记表里，显式 `unscoped`
   （分页仍走唯一入口 `scope::paginate_unscoped`），注释里写清了「要按车间裁剪需三处一起改」。

---

## 4. 复跑方法

```bash
# 纯逻辑
cd backend && cargo test --lib cost_alloc_rule

# 真库（本机 alun/alun，库 mig_replay_b24 已全链迁移）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/mig_replay_b24' \
  cargo test --lib cost_alloc_rule_db -- --test-threads=1

# HTTP 冒烟（隔离库 + 隔离端口，跑完自动 dropdb）
cd ../docs/evidence/2026-11-11-cost-b2-5-alloc-rule
bash 00_up.sh up && BASE=http://127.0.0.1:18028 bash 03_http_smoke.sh && bash 00_up.sh down
```

文件清单：

| 文件 | 内容 |
|---|---|
| `00_up.sh` | 隔离实例起停（库 `costb25_http` / 端口 `:18028`） |
| `01_unit_tests.out` | 51 条纯逻辑单测 |
| `02_real_db_tests.out` | 7 条真库用例 |
| `03_http_smoke.sh` / `.out` | HTTP 冒烟脚本与输出 |
| `04_gates.out` | fmt / clippy / 目标测试 |
| `99_full_test.raw` | 全仓 `cargo test` 原始输出（33 目标 4722 passed） |
