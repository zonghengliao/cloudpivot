# M5-21 在制品：完工率录入（按工序）· `t_46c1745d` 交付证据

一句话：**把「期末考试」从对象级一个手填框挪到工序级** —— 一个成本的完工率由「下料 100% /
焊接 50% / 装配 0%」这样的逐工序进度按权重加权汇总出来，保存时**同一个事务**回写
`biz_cost_object.completion_pct`；M5-20 的约当产量表与期间计算读到的仍是那一列，**零改动**。

| 项 | 值 |
|---|---|
| 新增表 | `biz_cost_object_operation`（V0470，12 列 / 5 条 CHECK / 1 外键 / 2 索引） |
| 新增模块 | `cost_wip_pct_dto.rs`（契约 + 纯逻辑）、`cost_wip_pct_model.rs`（取数 / 写）、`cost_wip_pct_handler.rs`（路由 + 编排）、`cost_wip_pct_db_tests.rs`（真库用例） |
| 新增路由 | `GET /business/erp/cost-object/{id}/operations`（`cost-object:list`）、<br>`PUT /business/erp/cost-object/{id}/operations`（`cost-object:update`） |
| 权限码 | **零新增**（复用成本对象两个既有码，菜单零改动） |
| 错误码 | **零新增**（复用 `PARAM_INVALID` / `RESOURCE_CONFLICT` / `66070` 已结算锁） |
| 改动文件 | `cost/mod.rs`（4 行模块声明）、`changelog/diff.rs`（登记 `biz_cost_object_operation` + 1 条单测）、`scripts/lint/scope_gate.py`（登记本卡测试面 1 行） |
| 前端 | **N/A**（页面属 M5-25「前端：在制品页」；本卡只出接口） |

## 1. 口径（为什么这么算）

```
综合完工率 = Σ(工序权重 × 工序完工率) / Σ工序权重        ← 加权平均法，收口 2 位（四舍五入）
加工约当   = 本期完工 + 期末在制 × 综合完工率              ← M5-20 消费，本次不改
```

* **权重**缺省 1（= 算术平均）；车间填「标准工时 / 定额工时」时，干活多的工序说话更重。
  例：`下料 100%×1 / 焊接 50%×2 / 装配 0%×1` ⇒ `(100+100+0)/4 = 50%`。
* **权重 > 0 是硬约束**（库 CHECK + DTO 400）：`Σ权重 = 0` 时公式是 `0/0`，没有定义。
* **对象级 = 派生物**：保存工序在**同一个事务**里回写 `biz_cost_object.completion_pct`；
  提交后才做的只有只读回查（AGENTS 约定 11 无「提交后失败」形态）。
* **不带期间**（决定，不是疏漏）：对象级完工率本身是「当前快照」（V0370 列注释），
  工序表若自带 `period_code` 就会与对象抬头变成两本账。按期间留档的完工率在**计算单**
  （`biz_cost_period_item.completion_pct`）里，那是结算那一刻的法定快照。
* **整表覆盖**：payload 有几行就是几行 —— 同 seq 更新、新 seq 插入、**库里多的 seq 软删**。
  没有逐行 DELETE 接口：那会让「删一道工序」和「漏传一行」在服务端无法区分。
* **空 payload = 清空工序**，对象级**保持**最后的值（回到手填模式，不归零 —— 否则在制 > 0
  的批会被 `66067` 直接拦在计算门口）。
* **状态闸**：非 `OPEN`（已关批 / 已作废）⇒ 409；该批参与的成本批次已结算 ⇒ 409 + `66070`
  （与 `PUT …/output` 同码同口径）。两件事都**不是权限问题**，所以不用 403。
* **录入侧提前拦 66067**：`期末在制 > 0` 且加权汇总 `= 0` ⇒ 400，报文点名「跑计算会撞 66067」。
* **审计**：每行增 / 改 / 删各一条 `sys_change_log`（白名单 5 列），对象级完工率回写单独一条
  （`resource = biz_cost_object`，`action_label = 录入工序完工率`）。幂等重放**不写空审计**。

## 2. 逐工序 / 对象级不一致怎么发现（不制造第二本账）

`PUT /cost-object/{id}/output` 仍可改对象级那一列（本卡不动那条路径），但工序页 `GET` 会直接
标出来：`rollup.matched = false` + `diff`（对象级 − 汇总）+ 一条提醒；`warnings` 里
写明「有人直接改过对象级，保存本表会把对象级回写成汇总值」。保存一次即一致（真库用例 v8 钉住）。

## 3. 真跑输出（`raw/` 全部真执行）

| 文件 | 命令 / 内容 | 结果 |
|---|---|---|
| `raw/01-unit-tests.txt` | `cargo test --lib business::erp::cost::cost_wip_pct_dto` | **30 passed**（加权汇总 / 四舍五入 / 值域 6 类 / 状态闸 / 提醒 5 类 / 权限码对拍） |
| `raw/01b-model-tests.txt` | `cargo test --lib …cost_wip_pct_model` | **5 passed**（表名 / 锚点 / SQL 别名与排序 / 十进制解析） |
| `raw/02-db-tests.txt` | 145 `cp_m521_mig`（克隆 `cp_m511_test` + 手工回放 V0470）`cargo test --lib …cost_wip_pct_db_tests` | **9 passed**（见下） |
| `raw/03-migration-replay.txt` | V0470 up ×2（幂等）/ 有数据 down（**拒绝**）/ 清数据 down（成功）/ 再 up | 4 个 rc 全部符合预期，自检 5 条行为断言全过 |
| `raw/04-clippy.txt` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | **rc=0**（零 warning，含测试面） |
| `raw/05-fmt.txt` | `cargo fmt -p cloudpivot -p cloudpivot-kernel --check` | **rc=0** |
| `raw/06-cost-domain.txt` | `cargo test --lib business::erp::cost::` | **1209 passed / 0 failed**（本卡 +35 纯单测、+9 真库用例，零回归） |
| `raw/07-changelog.txt` | `cargo test --lib business::changelog::` | **54 passed**（新增表登记 + 白名单断言） |
| `raw/08-permission-audit.txt` | `cargo test --lib system::menu::permission_audit_tests` | **8 passed**（零新增码，审计无变化） |
| `raw/09-lint-gates.txt` | `migration_version_unique` / `changelog_in_tx` / `sys_change_log_gate` / `scope_gate` | 前三个 **PASS**；`scope_gate` 仍有 **13 条 HEAD 既有欠账**（见 §5），本卡文件已登记 |

### 9 条真库用例（`raw/02-db-tests.txt`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `v1_weighted_rollup_writes_back_to_object` | 1×100 + 2×50 + 1×0 = 200 / 4 = **50%** 回写对象级；逐行占比 50% / 贡献 25；审计 3+1 行 |
| 2 | `v2_replay_is_idempotent` | 同一 payload 再保存：`changed=false`、0 写入、审计不增、`updated_at` 不变 |
| 3 | `v3_missing_seq_is_soft_deleted_and_pct_recomputed` | 少一个 seq ⇒ 软删 1 行、对象级重算 **66.67**、活行 2 / 总行 3 |
| 4 | `v4_closed_and_settled_objects_are_rejected` | 已关批 409（点名「已关批」）/ 已结算期间 409 + **66070**；两种拒绝都**零落库** |
| 5 | `v5_other_workshop_cannot_see_or_write` | 别车间 GET / PUT 都 **404**（不是 403），零落库；本车间对照组可读可写 |
| 6 | `v6_wip_without_progress_and_bad_rows_are_rejected` | 在制 40 + 汇总 0 ⇒ 400 且点名 **66067**；重复 seq / 3 位小数 ⇒ 400；对象级未被改 |
| 7 | `v7_clearing_operations_keeps_object_pct` | 清空 ⇒ 3 行全软删、`rollup=null`、对象级**保持 50**（不归零） |
| 8 | `v8_out_of_band_object_pct_shows_drift` | 绕过工序把对象级改成 80 ⇒ `matched=false`、`diff=30.0000`、提醒「不一致」；再保存回写 50 |
| 9 | `zz_teardown_leaves_zero_residue` | 清场后本卡造的行（对象 / 工序 / 计算单）全 0，工序审计无孤儿行 |

## 4. 迁移（V0470）

* **号段**：`migration_version_unique` 报「最大 V0470 / 下一个可用 V0471」，跨目录全局唯一。
* **幂等**：`CREATE TABLE / INDEX IF NOT EXISTS`、`COMMENT ON` 可重放、自检探针断言后即删。
* **自检 5 条行为断言**（写进迁移，失败即整笔回滚）：完工率 101 拒 / 权重 0 拒 / 同批同 seq
  第二行撞唯一键 / 软删行不占键 / 空白工序名拒。
* **down 护栏**：有活行拒绝回滚（工序行是对象级完工率的唯一依据），清数据后可重放。

## 5. 已知欠账 / 边界（报备，不是本卡 bug）

1. **`scope_gate` 在 HEAD 上已是红的**：13 条测试面文件未登记 `SKIP_MANIFEST`
   （`cost_batch_collect_db_tests` / `cost_batch_finish_db_tests` / `cost_batch_method_tests` /
   `cost_batch_wip_db_tests` / `cost_collect_sheet_db_tests` / `cost_standard_compare_db_tests` /
   `cost_standard_db_tests` / `cost_standard_import_db_tests` / `cost_standard_publish_db_tests` /
   `cost_standard_variance_tests` / `cost_variance_voucher_db_tests` / `cost_wip_equiv_db_tests` /
   `stockin_handler_tests_zz_shared_roles`，逐条 `git cat-file -e HEAD:…` 确认**都在 HEAD 上**）。
   本卡只登记自己的 `cost_wip_pct_db_tests.rs`（1 行），不动别卡的文件 —— 那是别人的卡面。
2. **对象级手改不拦**（见 §2）：`PUT …/output` 仍可写那一列；不一致由本模块 `GET` 标出。
   要不要在对象级入口硬拦，属 M5-25 页面卡 / 成本域权限重排时一起定。
3. **无 `period_code`**：本表是「当前工序进度」快照（与对象级同生共死）；按期间的历史完工率
   在计算单里。跨期想留多版本，需要新的「完工率版本表」，不在本卡范围。
4. **无前端**：本卡按卡面只做接口；页面（含工序网格录入）属 M5-25。前端未改 ⇒ `pnpm` 门禁不适用。

## 6. 复现命令

```bash
# 纯单测（不需要数据库）
cd backend && cargo test --lib business::erp::cost::cost_wip_pct

# 真库用例（99 上连 145；演练库克隆 + 回放本卡迁移）
export PGPASSWORD=***; H=172.18.10.145
createdb -h $H -U cloudpivot -T cp_m511_test cp_m521_mig
psql -h $H -U cloudpivot -d cp_m521_mig -v ON_ERROR_STOP=1 \
  -f app/migrations/business/V0470__cost_wip_operation_pct.up.sql
ERP_DB_URL="postgres://cloudpivot:***@$H:5432/cp_m521_mig" \
  cargo test --lib business::erp::cost::cost_wip_pct_db_tests -- --test-threads=1
dropdb -h $H -U cloudpivot cp_m521_mig
```
