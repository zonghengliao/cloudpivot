# COST-B1-3 实测证据：V0372 分摊两表 + V0373 期间两表

卡：`t_7efea068`（COST-B1-3【成本】迁移 V0370/0371）
批次：B1 地基（ARCH-9-3 §8.1 的 B1-3）
日期：2026-11-10
设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ⑦⑧⑨⑩ / §4.1 状态机 / §4.4.5 / §4.6.3 / §3.4

## 落地了什么

| 迁移 | 表 | 索引 | CHECK | 外键 |
|---|---|---|---|---|
| `V0372__cost_alloc_tables` | `biz_cost_alloc_rule`（⑦ 分摊规则）、`biz_cost_alloc_result`（⑧ 分摊结果） | 4（2 唯一 + 2 查询）+ 2 PK | 5 + 3 = 8 | 2 条，均 `ON DELETE RESTRICT` |
| `V0373__cost_period_tables` | `biz_cost_period`（⑨ 期间结算单）、`biz_cost_period_item`（⑩ 成本计算单） | 5（3 唯一 + 2 查询）+ 2 PK | 9 + 7 = 16 | 2 条：`item→period` **CASCADE**、`item→cost_object` **RESTRICT** |

DDL 与设计 §3.1 逐字一致（只补 `COMMENT ON`，不改任何列 / 约束 / 索引形状）。

## 证据文件

| 文件 | 内容 | 结论 |
|---|---|---|
| `01_replay_fresh.out` | `bash scripts/verify/mig_replay_fresh.sh costb13` 空库全链重放 | **PASS 293/293**（B1-2 时 291，+2 = V0372/V0373） |
| `02_up_down_drill.sh` / `.out` | 真库往返演练（在 `mig_replay_costb1_01` 上跑，该库已有 V0370/V0371） | 见下「实测输出」 |
| `../../backend/tests/cost_b1_3_alloc_and_period_ddl_migration.rs` | 12 条静态不变量（`cargo test`） | 12 passed / 0 failed |
| `03_cargo_test_summary.out` | `cargo test` 全仓逐目标结果 + 汇总 | 27 targets ok / 4344 passed / 0 failed |

## ★ 卡面验收：唯一键必须排除 REVERSED + 实测「一条 REVERSED 一条 SETTLED 不冲突」

唯一键形状（`02_up_down_drill.out` 第 4 步从 `pg_indexes` 直接读出来的真库定义）：

```sql
CREATE UNIQUE INDEX uk_biz_cost_period ON public.biz_cost_period USING btree (org_id, period_code)
 WHERE ((deleted_at IS NULL) AND ((status)::text <> ALL (
        (ARRAY['REVERSED'::character varying, 'CANCELLED'::character varying])::text[])))
```

实测（同一期间的探针行，`02_up_down_drill.out` 第 5 步）：

```
① REVERSED 行（2026-01）插入成功
② 同期间 SETTLED 行插入成功 ⇒ 不冲突（验收通过）
period_2026_01_rows=2（期望 2：一条 REVERSED + 一条 SETTLED）
→ 期望失败：同期间第三条**活跃**批次
ERROR:  duplicate key value violates unique constraint "uk_biz_cost_period"
DETAIL:  Key (org_id, period_code)=(00000000-0000-0000-0000-000000000000, 2026-01) already exists.
③ 同期间第二条 REVERSED 插入成功（红冲留痕可多条）
period_2026_01_rows_now=3（期望 3 = 2 REVERSED + 1 SETTLED）
```

即：**红冲行不占位（可重建同期间批次），但同期间的第二张活跃批次仍被拦**——这正是设计附录 A「真库跑通过程中改掉的 3 个真错误」第 1 条要守的语义（否则红冲后该期间永远无法重新结算，只能改库）。

同样的两组断言也写进了迁移自检（V0373 行为自检 B1~B4），所以**每次部署都会再验一遍**，不靠人工记得跑。

## 实测输出（`02_up_down_drill.out` 摘要）

| 步骤 | 结果 |
|---|---|
| 1~2 up（V0372→V0373，各跑 3 轮） | 6 次全部 `exit=0`，重复执行无变化 |
| 3 幂等证据 | `tables=4` / `idx=13`（9 业务索引 + 4 PK）/ `checks=24`（5+3+9+7）/ `fks=4` / `probe_leftover=0` |
| 4 唯一索引谓词快照 | 见上（5 条唯一索引全部打印） |
| 5 核心验收活体 | 见上 |
| 5 附带反证 | `SETTLED` 不带 `settled_at`/完工凭证 → `ck_biz_cost_period_settle` 拒绝（假结算死单防线） |
| 6 外键删除动作 | `alloc_result→alloc_rule r`、`alloc_result→cost_object r`、`item→cost_object r`、`item→period c` |
| 7 精度快照 | 27 个 numeric 列全部按设计：金额/数量 `18,4`、单价费率 `18,6`、完工进度 `5,2` |
| 8 业务语义 | 分摊结果同键重复 → 唯一键拒；`basis_total=0` 却摊 100 元 → `ck_biz_cost_alloc_base` 拒；计算单不守恒（150+40≠200）→ `ck_biz_cost_item_cons` 拒；删批次 → 计算单级联消失（`items_after_cascade=0`）；有分摊结果时删成本对象 → `RESTRICT` 拒 |
| 10~12 down×2 | 两次都 `exit=0`，第二次只打 `does not exist, skipping`；四表 + 索引 `idx_left=0` |
| 13 回滚后重放 | 四表重建成功（`replay tables=4`） |
| 14 下游护栏反证 | 外部表 FK 到 `biz_cost_period` 时 down 拒绝执行并点名：`ERROR: V0373 down 拒绝执行：下游表仍在引用期间两表（zz_downstream）…` |

`cargo test`（全仓）：**27 个测试目标全 ok，4344 passed / 0 failed，0 编译错误**
（其中本卡新增目标 `cost_b1_3_alloc_and_period_ddl_migration` = 12 passed；汇总见 `03_cargo_test_summary.out`）。
`cargo fmt -- --check` 干净。

## 怎么重跑

```bash
# 1) 空库全链重放（会自建自删 mig_replay_costb13）
bash scripts/verify/mig_replay_fresh.sh costb13

# 2) 真库往返演练（库需已有 V0370/V0371；本机演练库 mig_replay_costb1_01 可直接用）
bash docs/evidence/2026-11-10-cost-b1-3-v0372-v0373/02_up_down_drill.sh \
  mig_replay_costb1_01 /tmp/b13_drill.out

# 3) 静态不变量
cd backend && cargo test --test cost_b1_3_alloc_and_period_ddl_migration
```

## 已知偏差（都在卡内汇报里具名）

1. **号段 V0370/0371 → V0372/V0373**：卡面与设计 §8.1 写 B1-3 = V0370/V0371，但
   V0370（B1-1 批次 + 成本对象）、V0371（B1-2 归集四表）已被前置卡占用；nik 迁移加载器对
   版本号做跨目录全局唯一校验，重号会让服务启动失败（AGENTS 约定 6）。故 B1-3 顺延两位。
   ⇒ **设计 §7.1 的整张清单（V0368~V0379）实际整体后移 2 位**：B1-4 二期三表应是 **V0374**、
   B1-5 出库/入库加列应是 **V0375/V0376**……后续卡开工前请按实际 `ls backend/migrations/*/`
   重新核对号段（已在本卡汇报里提请总管调整后续卡面号段）。
2. **表数**：卡面「迁移 V0370/0371：分摊两表 + 期间两表」= 4 张表；实建 4 张
   （`biz_cost_alloc_rule` / `biz_cost_alloc_result` / `biz_cost_period` / `biz_cost_period_item`）。
3. **`uk_biz_cost_alloc_result` 不带软删条件**：设计 §3.1 ⑧ 如此，且该表**没有 `deleted_at` 列**
   （分摊结果是计算产物，重算＝按期间整删重灌，§4.4.5）。卡面「唯一键必须排除 REVERSED」落在
   `biz_cost_period` 上（那是唯一有 status 的表），已在迁移头与单测里写明理由。
4. **设计内部一处小冲突（未改动设计）**：D-16 说「现在就把 `biz_type` 值域留给三期
   `STEP_TRANSFER`」，而 §3.1 ⑦ 的 `ck_biz_cost_rule_biz CHECK (biz_type IN ('OVERHEAD'))`
   只允许一期值。本卡按 §3 DDL 逐字落地（设计 DDL 优先）；三期上线时用一条新迁移
   `DROP CONSTRAINT` + 重建值域即可（改造面 1 行，不影响存量数据）。

## 关联的下一张卡

同批次的期间两表建好后，`B3-1`（结算四件套骨架）与 `B2-5`（分摊规则四件套）可以开工；
`B1-4`（二期三表）紧接本卡，注意号段已是 V0374。
