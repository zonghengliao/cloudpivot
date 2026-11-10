# COST-B2-3【成本】费率取数 —— 实测证据

卡：`t_bd795174`（ARCH-9-3 §8.1 的 **B2-3**，前置 B2-1 `t_2e11875e`）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§3.1 ⑥（`biz_cost_rate` 表结构）/ §4.5.4（取数优先级 `COST_OBJECT > DEPT > COMPANY`，
都取不到 ⇒ **报错，不默认 0**）/ §6.2.4（单条录入费率留空 ⇒ 后端自动取数，
出参回填「实际费率 + 来源」）/ §5.2 #11#15（工时单条录入 / 导入两条复用路径）

## 一句话结论

`biz_cost_rate` 的三级取数落地为**一个纯函数 + 一次查库**，同时接进工时台账的
单条录入与 Excel 导入；三级都没配到 ⇒ `422` + **新码 `66059`**，报文点名
「成本对象 / 车间 / 日期 / 查了哪几级 / 怎么修」，**绝不按 0 计**。

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/cost_rate_dto.rs`（**新增**） | 纯逻辑：`RateRow` / `RateRequest` / `RateSubject` / `ResolvedRate`、`resolve()`（优先级 + 版本 + 生效期 + 范围）、`missing_rate()`（`66059` 报文）、16 条单测 |
| `backend/src/business/erp/cost_rate_model.rs`（**新增**） | 读库：`rates_of_type()`（一次取回同类型全部未删除行，含停用 / 过期 —— 为了能解释「为什么没命中」）、`find_by_id()`；只有 `SELECT`（本卡不改费率数据） |
| `backend/src/business/erp/cost_rate_handler.rs`（**新增**） | 编排：`candidates()` / `resolve_labor_rate()`；**无 HTTP 路由**（设计 §5.2 没有费率接口、§5.3 没有 `cost-rate:*` 权限码） |
| `backend/src/business/erp/cost_labor_handler.rs` | `create_labor`：`rate` 留空 ⇒ 三级取数后按「显式费率」走同一套校验；出参回填 `rate_scope` / `rate_scope_label`；`ensure_cost_object_open` 改为返回对象号（报文要用） |
| `backend/src/business/erp/cost_labor_dto.rs` | `rate` 语义改为「留空 = 自动取数」；出参新增 `rate_scope` / `rate_scope_label`；新增 1 条 serde 单测（省字段 / `null` / 数字三态） |
| `backend/src/business/erp/cost_labor_import.rs` | 导入：工资率列留空不再报错，改为**引用校验后逐行取数**（`attach_rate_issues`，全批只查 1 次库）；填错数字仍报格式错、**不被取数掩盖**；模板说明 / 填写须知同步更新 |
| `backend/src/business/erp/cost_labor_db_tests.rs` | 真库用例 16 → **25** 条（新增 ⑰–㉕，`ERP_DB_URL` 开关） |
| `backend/src/business/erp/mod.rs` | 声明 3 个新模块 |
| `backend/src/shared/error_code.rs` | 新码 `66059 ERP_COST_RATE_MISSING`（`422`）+ 段文档 + 码表自检 |

**迁移：本卡无迁移** —— 表 `biz_cost_rate` 由 B1-2 的 `V0371` 建好（含
`uk_biz_cost_rate` 唯一索引与 6 条 CHECK），本卡没有 schema 变更，因此没有
`.up.sql/.down.sql`，也没有碰任何已应用的迁移。

## 取数规则（落地口径，逐条可单测）

| # | 规则 | 理由 |
|---|---|---|
| 1 | 优先级 `COST_OBJECT`(3) > `DEPT`(2) > `COMPANY`(1) | 设计 §4.5.4 |
| 2 | 同一级里**生效期更晚的版本赢** | 费率是版本化的：11-01 起 50 元覆盖 01-01 起的 40 元 |
| 3 | 只认 `status=1` + 未软删 + `effective_from <= work_date <= effective_to`（两端含） | `ck_biz_cost_rate_status`；过期 / 未来 / 停用都算「没命中」 |
| 4 | 范围必须**逐字相等**：`DEPT` 行的 `scope_id` 必须是本行车间、`COST_OBJECT` 行必须是本行对象、`COMPANY` 行必须为空 | 串到别的车间 = 成本单价错且没人看得出来 |
| 5 | 三级全空 ⇒ `422` + `66059`，报文含「怎么修」+「有几条因何被跳过」 | 设计 D-7「不猜」；0 元费率会让毛利假准 |
| 6 | `rate = 0` 是**合法配置**（DDL 只要求 `>= 0`），与「取不到」严格区分 | 不要把合法 0 当成缺失 |

## 实测输出

### ① 真库用例（`ERP_DB_URL` = 本机 `mig_replay_costb18`，**25/25**）

`01_real_db_tests.out`（`cargo test --lib cost_labor_db -- --test-threads=1`）：
旧 16 条（B2-1/B2-2）全绿 + 新增 9 条：

| # | 用例 | 断言要点 |
|---|---|---|
| ⑰ | `company_rate_fills_blank_rate` | 只有公司级 ⇒ 取到 42.5，金额 `340.0000`，`rate_scope=COMPANY`；显式填 50 不被覆盖 |
| ⑱ | `dept_rate_beats_company_rate` | 公司级 40 + 车间级 55 ⇒ 55，`rate_scope=DEPT`，来源文案以「车间（」开头 |
| ⑲ | `cost_object_rate_beats_dept_and_company` | 40 / 55 / 70 ⇒ 70，`rate_scope=COST_OBJECT` |
| ⑳ | `missing_rate_fails_loudly_and_writes_nothing` | `422` + `66059`；报文含对象号 / `2026-11-06` / `成本对象 → 车间 → 公司` / `怎么修`；`live_rows == 0` |
| ㉑ | `another_depts_rate_is_not_reused` | 别的车间的 66 元不被采用，报文「同类型费率共 1 条…未采用」 |
| ㉒ | `expired_future_and_disabled_rates_are_ignored` | 过期 / 未生效 / 停用三条都跳过（「共 3 条」） |
| ㉓ | `newest_effective_version_wins` | 01-01 的 40 与 11-01 的 50 ⇒ 取 50 |
| ㉔ | `import_fills_blank_rate_per_row` | 导入留空 ⇒ 取车间级 55（不是公司级 40），库里 `rate=55`、`amount=412.5` |
| ㉕ | `import_rejects_the_batch_when_rate_cannot_be_resolved` | 三级全空 ⇒ `success=0 failed=1`，错误列 `工资率`，零落库 |

> 这一组用例串行执行（进程内 `tokio::sync::Mutex`）并在前后清一遍
> `remark LIKE 'B2-3 真库用例%'` 的费率行：`uk_biz_cost_rate` 把公司级行的
> `scope_id` 当 `0-uuid`，两个用例并发插「公司级 + 2026-01-01」会撞唯一索引；
> 更要紧的是「缺费率」用例会被别的用例留下的公司级费率污染（恒有兜底）。

### ② 纯逻辑单测（**16/16**）

`02_unit_cost_rate.out`（`cargo test --lib cost_rate`）：三种覆盖（验收项）+ 缺费率
报错 + 生效期两端 + 停用 + 类型不串 + 版本更新 + 确定性兜底 + 脏 `scope` +
`COMPANY` 行被塞 `scope_id` + **0 元是合法值** + 机器费率同链路 + 文案工具。

另：`cost_labor_dto` 新增 `create_req_treats_absent_and_null_rate_as_auto_lookup`
（JSON 省字段 / `null` / `"55.5"` 三态），锁住「留空 ≠ 报错 ≠ 按 0」。

### ③ 工程门禁

`03_fmt_clippy.out`：`cargo fmt --check` 干净（`fmt_exit=0`）；
`cargo clippy --all-targets -- -D warnings` **0 warning / 0 error**（`EXIT=0`，
日志为本地 `03b_clippy.log`，`*.log` 不入库，可用上面命令复跑）。

`05_full_test_summary.out`：`cargo test` 全量 **33 目标 4628 passed / 0 failed**。

## 已知偏差 / 未做（都在卡内汇报过）

1. **费率维护入口缺失（跨卡，未自行加）**：设计 §5.2 的 36 个接口里没有费率的增删改查，
   §5.3 也没有 `cost-rate:*` 权限码，§6 页面清单同样没有费率页。本卡只做「取数」，
   费率数据目前由财务直接维护 `biz_cost_rate`。要界面化需单独立卡（含权限码 + 菜单 + 页面）。
   → 这也是「新装环境没有任何费率」时工时录入一定报 `66059` 的原因，属**预期**（不猜）。
2. **编辑不自动重新取数**：`PUT /cost-labor/{id}` 的 `rate` 留空 = 沿用当前快照。
   换车间时若自动重取，会**静默改写金额**（用户手工填过的费率被覆盖），
   与「不猜」冲突；要按新车间口径请显式给 `rate`。
3. **`rate_scope` 只在建单回单里有值**：`biz_cost_labor` 没有「来源」列（设计 §3.1 ④ 未定义），
   列表 / 详情无法事后还原，故给出 `null` 而不是猜。前端要显示来源（§6.2.4）用建单响应即可；
   若要求长期保留，需加列（新迁移）+ 设计确认。
4. **机器费率（`MACHINE`）只做了取数能力，未接任何调用方**：`MACHINE_HOURS` 分摊基数在
   B2-10（calc 第 5 段），届时直接调 `RateRequest::machine(...)`。
5. **无 HTTP 冒烟**：本卡没有新增接口，取数发生在 `create_labor` 内部；真库用例 25 条
   直接调用同一个 handler 入口（只跳过认证层），HTTP 层只有 `Option<Decimal>` 反序列化，
   已用 serde 单测覆盖「省字段 / null / 数字」三态。上一张卡（B2-2）的 31 项 HTTP 冒烟
   已覆盖 8 列模板与导入入口，本卡改动没有触碰路由声明。

## 复跑命令

```bash
cd backend
cargo test --lib cost_rate                       # 纯逻辑 16 条
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/mig_replay_costb18 \
  cargo test --lib cost_labor_db -- --test-threads=1   # 真库 25 条
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                       # 全量
```
