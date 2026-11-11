# COST-B2-15 · calc 编排接口 `POST /business/erp/cost-period/calc`（六段串联 → HTTP）

日期：2026-11-11 · 卡片：`t_45f735ef` · 基线：`049f8253`（提交前 HEAD）
对应设计：ARCH-9 §5.2 #27（`POST /business/erp/cost-period/calc`，权限 `cost-period:calc`）

## 一句话

之前六段计算全写完了但**没有 HTTP 入口**（前端「重算」按钮点下去调不到、e2e 只能绕道
example 探针）。本卡把已有的六段编排暴露成真接口：一次事务里 **定批次 → 清旧计算单 →
料 → 工 → 费 → 池对账 → 分摊 → 计算单**，出参就是批次详情（与 `GET /cost-period/{id}`
同结构，前端同一个 `CostPeriodDetail` 类型直接用）。

## 落地内容

| 层 | 文件 | 说明 |
|---|---|---|
| 编排逻辑 | `backend/src/business/erp/cost_calc_period.rs`（新） | `calc_period()` 自开事务；`run_in_tx()` 六段串联；`ensure_batch()` 定批次（无则建 `DRAFT`，非 `DRAFT` 直接 409）；`CalcRunSummary` 分段摘要；`stage_failed()` 给失败加段名（**保留**原状态码 / 业务码 / `data`） |
| HTTP 入口 | `cost_period_handler.rs` | `POST /business/erp/cost-period/calc` + `#[nik::permission(… cost-period:calc)]`；路由成对注解有单测钉住 |
| 入参 | `cost_period_dto.rs` | `CostPeriodCalcReq { period_code }`（**只有期间**：能传对象 / 金额就等于能改数） |
| 出参 | 同上 | `CostCalcRes { #[serde(flatten)] detail, calc }` —— 详情字段**平铺**在顶层，加 `calc` 增量字段 |
| 前端 API | `frontend/src/api/business/erp/cost-period.ts` + `cost-period-kinds.ts` | `calcCostPeriod()` + `CostPeriodCalcResult / CostPeriodCalcSummary`（六个段的类型） |
| 前端页面 | `views/business/erp/cost-period/index.vue` + `steps/CalcSheetPanel.vue` | 「重算」按钮真调接口（loading / 成功 toast 带金额）；「建批次」建成后**立刻算一遍**；步③ 顶部显示上次 calc 的分段摘要；三处「接口未上线」文案全部改掉 |
| e2e | `scripts/verify/cost/cost_e2e.py` + `cost_e2e.sh` | `run_calc()` 从 example 探针改成 `POST /business/erp/cost-period/calc`；新增 `enable_cost_module()`（默认 false 的开关必须先打开）与管理员初始口令注入；探针 `backend/examples/cost_calc_probe.rs` 保留供离线排查但脚本不再依赖 |
| 文档 | `docs/后端二开指南.md` §16.2、`docs/architecture/ARCH-9-…设计.md` §7.4 | 把「该接口至今未落地 / calc 用探针跑」改成「#27 已落地，前端按钮走它；探针只用于离线分段排查」 |

### 段序与幂等（本卡最容易被改坏的两件事）

```text
① 定批次   active_by_period → DRAFT 原地重算 / 无活跃批次则建 DRAFT / 其它状态 409
② 清计算单 purge_calc_rows(period_id)         ← 只删本批次的 CALC 行
③ 料 → 工 → 费 → 池对账 → 分摊 → 计算单        ← 顺序不可交换（分摊必须早于计算单）
④ 一次事务 ②③ 与建批次同生共死；任一段 409 全回滚
```

- **不做**「`DELETE … WHERE period_code = ?`」的一刀切清场（设计 §4.3 的字面写法会把同期间
  **已结算批次**的归集行一起删掉 —— 那是审计凭据）。各段自带收敛口径：料/工/费按来源行 id
  取差 + 软删过期行；分摊 `purge_period` 只删自家 `ALLOC` 行与 `biz_cost_alloc_result`；
  计算单 `purge_calc_rows` 只删本批次 `CALC` 行。
- 编排层仍**显式**再清一次计算单（卡面要点 ②），段 6 内部那次成为兜底 ⇒ 摘要里的
  `items.purged` 用编排层这次的数量（否则重算永远显示 0）。
- **红冲后重算** = 建**新** `DRAFT` 批次再算（`uk_biz_cost_period` 已排除 `REVERSED`，
  设计 §4.1 / §4.8）。不原地改 `REVERSED` 批次的账 —— 它的红字凭证与红冲原因是审计凭据。

### 失败语义（AGENTS 约定 11：不留死单）

整笔一个事务，任一段 409 就整体回滚：

- 期间本来没有批次 ⇒ **批次也不存在**（不会留一张「建了没算」的空 DRAFT 占着唯一键）
- 期间本来有 DRAFT ⇒ 旧计算单原样保留（不会「旧的删了、新的写一半」）
- 报文带段名：`…（成本计算第 4 段「费用池对账」失败；整笔已回滚，期间保持调用前的状态，修好后可直接重算）`，
  段内原有码不变（`66062`~`66069`、`66064`、`66065`），前端按码分支的逻辑不受影响
- 非 `DRAFT` ⇒ `409 + 66073`，文案点名**下一步点哪**：审批中→撤回 / 已审批→结算或驳回 /
  已结算→红冲 / 已关期→反结账

## 验收对照（卡面 8 条）

| # | 卡面验收 | 证据 |
|---|---|---|
| ① | 200 + 批次详情 + `items[]` | `02_real_db_tests.out::first_call_creates_draft_and_lands_all_six_stages`（`detail.items.len()==1`、`items[0].object_no` 有值）；`drill-*.out` 的 `CALC batch=CS-202611-0001 … items=1` |
| ② | 首次调用建 `DRAFT` + 六段落库 | 同上（`batch_created=true`、库里逐列 `22311.1104 / 4688.8896`） |
| ③ | 重复调用幂等 | `second_call_recalcs_in_place_without_doubling`（`purged=1`、行数 1、金额不变）+ drill 的「重复 calc」6 条断言 |
| ④ | 已提交批次 409 + 明确文案 | `non_draft_status_is_rejected_with_the_next_step`（4 个状态逐个）+ drill「已提交批次调 calc ⇒ 409 + 66073」「报文点名下一步「撤回」」 |
| ⑤ | `REVERSED` 后可重算 200 | `reversed_batch_can_be_recalculated` + drill「红冲后重算 200 且新建了批次（created=true）」 |
| ⑥ | 缺数据 → 可定位错误（不是 500） | `missing_progress_is_a_locatable_error_and_leaves_no_draft`（报文点名对象号 + 「第 6 段」+ **不留空批次**）；drill「calc 期间非法 ⇒ 400（不是 500）」 |
| ⑦ | 前端「计算成本」按钮调通 | `frontend/src/api/business/erp/cost-period.ts::calcCostPeriod()` + `index.vue::runCalc()`（按钮 → 真接口 → 回执直接铺到步③ 表格）；`pnpm typecheck` + `pnpm build` 通过 |
| ⑧ | e2e 改走 HTTP 后仍全过 | `drill-*.out`：**通过 96 项，失败 0 项**（9 步全链：calc → 审 → 结算 → 对平 → 红冲 → 重算 → 再结算 → 清场），calc 走的是 `POST /api/business/erp/cost-period/calc` |

## 门禁（`03_gates.out` 是原始输出）

| 项 | 结果 |
|---|---|
| `cargo fmt -- --check` | 0 文件需格式化 |
| `cargo clippy --all-targets -- -D warnings` | 0 warning / 0 error |
| `cargo test`（**含 doctest**，全仓） | **38 目标 / 5188 passed / 0 failed**，EXIT=0 |
| 真库用例（`ERP_DB_URL=…costb215_drill`） | 11 passed / 0 failed（5 纯单测 + 6 真库） |
| 前端 `pnpm typecheck` / `pnpm build` | 通过 / 通过 |
| `scripts/verify/cost_doc_check.py` | `DOC_OK 全部引用可达`（A 35/35、B 8/8、C 14/14） |
| 端到端演练 `scripts/verify/cost_e2e.sh` | 通过 96 / 失败 0，EXIT=0 |
| 残留（`04_residue.out`） | CB215 对象 / CS215 批次 / 25xx 期间·归集·计算单 / 物料仓库 / 孤儿状态日志 **全 0** |

## 已知偏差 / 顺手修的东西

1. **出参多了 `calc` 字段**（批次详情平铺 + 分段摘要）。设计 §5.2 #27 只写「出参批次详情」，
   分段摘要是我加的增量字段（前端拿它做 toast 与排查），`#[serde(flatten)]` 保证老调用方
   按 `CostPeriodDetail` 解析时字段一个不少（有单测钉住）。
2. **`REVERSED` 批次的「重算」= 新建批次**，不是原地改 `REVERSED` 批次（理由见上）。
   `CANCELLED` 同理（不占位 ⇒ 新建）。
3. **calc 不回写单头 `total_*` / `object_count`**：设计 §4.8 ⑤ 把这两组列定为**结算事务**的落库项；
   DRAFT 阶段详情走 `items_summary` 现算（B3-1 已具名偏差），列表那几列仍显示 0。
4. **顺手修了仓库一处 doctest 红**：`cost_settle_run.rs` 模块文档的代码块没标语言（裸 ```），
   `cargo test`（含 doctest）会把里面的 `BEGIN / ① …` 当 Rust 编译 → 全仓 doctest 目标红。
   加 `text` 即修好（1 字符，非本卡功能）。之前各卡的 `cargo test --all-targets` **不跑 doctest**，
   所以一直没被发现 —— 这里记一笔，免得下一个人以为是本卡引入的。
5. **e2e 脚本补了两个环境前提**（都不是本卡功能，但不补跑不起来）：
   - `NIK_ADMIN_PASSWORD`：仓库已删除管理员默认口令（`bootstrap::ensure_admin_user` 只认环境变量），
     脚本不注入就只能拿到随机口令 ⇒ `admin` 登录 401。现在脚本注入一次性口令。
   - `erp.cost.enabled`（COST-B4-4，默认 false）：关闭时 `/business/erp/cost-*` 一律 403，
     脚本开头 psql 打开 + 调 `POST /support/config/reload` 让进程内快照即时生效。

## 证据文件

```
01_calc_http_errors.out   calc 相关的 HTTP 断言（从 drill 日志摘出）
02_real_db_tests.out      真库用例 11 条（含 6 条编排验收）
03_gates.out              fmt / clippy / 全仓 test / 前端 typecheck+build / 文档可达性
04_residue.out            演练库残留自检（全 0）
drill-*.out               端到端演练全量日志（96 断言全绿那一次）
```

## 复跑

```bash
# 真库用例（任意已迁移的成本演练库）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/costb215_drill \
  cargo test --lib cost_calc_period -- --test-threads=1

# 端到端（自己建库、起隔离实例、跑完删库；--keep 可保留排查）
bash scripts/verify/cost_e2e.sh --db costb215_e2e --port 18142

# 只测这条接口（实例起来后）
curl -sS -X POST http://127.0.0.1:18142/api/business/erp/cost-period/calc \
  -H "Authorization: Bearer <token>" -H 'Content-Type: application/json' \
  -d '{"period_code":"2026-11"}'
```
