# COST-B1-5 实测证据：迁移 V0375（出库加 `cost_object_id`）+ V0376（入库 `PRODUCTION` + 两列）

- 卡：`t_b2c9aa3d`（COST-B1-5【成本】迁移 V0373/V0374）
- 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.3 ①②、§1 D-11、§4.3、§4.7、§8.1 B1-5
- 日期：2026-11-10（CST）／主机 99（172.18.10.144 的姊妹工作机）

## 0. 号段偏差（必读）

卡面写「迁移 V0373/V0374」，但两个号都已被前置卡占用：

| 号 | 归属 |
|---|---|
| V0372 / V0373 | B1-3：分摊两表 / 期间两表（`a9b46ee1`） |
| V0374 | B1-4：二期三表 `biz_cost_standard/_item/variance`（`9af2409e`） |
| **V0375** | **本卡：`biz_stock_out + cost_object_id`** |
| **V0376** | **本卡：`biz_stock_in` 加 `PRODUCTION` + 两列** |

设计 §7.1 的清单（V0368~V0379）因 B1-1~B1-4 各顺延，整体已后移 3 位：B1-6（科目翻末级）→ V0377、
B1-7（菜单权限 / 号段字典 / 批号回填）→ V0378/V0379/V0381。

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0375__stock_out_cost_object.{up,down}.sql` | 出库加 `cost_object_id` + 局部索引；down 带「有单据挂着对象就拒绝回滚」护栏 |
| `backend/migrations/business/V0376__stock_in_production.{up,down}.sql` | 入库加两列 + `ck_biz_si_type` 5 值 + `ck_biz_si_production`；down 带「有 PRODUCTION 单就拒绝回滚」护栏 |
| `backend/tests/cost_b1_5_stock_out_in_cost_object_migration.rs` | 13 条静态不变量（不连库，每次 `cargo test` 都跑） |
| `docs/evidence/2026-11-10-cost-b1-5-v0375-v0376/02_up_down_drill.sh` / `.out` | 真库往返演练脚本 + 全量输出 |
| `01_replay_fresh.out` | 空库全链重放 296/296 PASS |
| `03_cargo_test_summary.out` | 全量 `cargo test` 输出 |

## 2. 卡面验收对照

| 验收项 | 怎么验的 | 实测结果 |
|---|---|---|
| 存量数据不受影响（加列给默认值/nullable） | 造 4 张入库单（PURCHASE/RETURN/OTHER/SOURCELESS）+ 4 张出库单（ISSUE/SCRAP/LOSS/OTHER），对**本卡之前就存在的列**算 md5 指纹，加列前后/回滚后/重放后四次对比 | 指纹四次全等 `in=38c68d95… / out=48967fea…`；`legacy_cost_object_filled=0`；两列均 `nullable=YES default=<无>` |
| `biz_stock_in.type` 允许 `'PRODUCTION'` | 真库活体插入 `in_type='PRODUCTION'` 单据（带 cost_object_id/cost_period_id） | 插入成功；`ck_biz_si_type :: CHECK (in_type = ANY (ARRAY['PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION']))` |
| 回滚脚本能删列且不丢其他数据 | down 前先造「挂着对象 / 有 PRODUCTION 单」的状态，验证护栏拒绝；清干净后正常 down；再 down×2 幂等；再重放 up | 护栏两次都拒绝并点名数量（`exit=3`）；正常 down 后 `new_cols_left=0 / new_idx_left=0 / ck_production_left=0`，存量 4+4 行与指纹未变 |

## 3. 真库往返演练（`02_up_down_drill.out`，16 段）

演练库：`mig_costb15_drill`（`createdb -T mig_replay_costb1_01` 的副本，含 V0370~V0374 全部成本表）

1. up 首轮 → exit=0；up ×3 幂等（V0375/V0376 各 3 轮，6 次 exit=0）
2. 结构快照：3 个新列、`ck_biz_si_type` 5 值、`ck_biz_si_production` 四要素、`ck_biz_si_supplier_scope`/`ck_biz_si_supplier`/`ck_biz_si_sourceless_pair` 原样、3 条局部索引
3. 存量指纹：加列后相等 ✅
4. ★ `PRODUCTION` 活体插入成功 ✅（卡面验收）
5. 反向反证（全部按预期被拦）：
   - PRODUCTION + 供应商 → `ck_biz_si_production`
   - PRODUCTION + 缺 `cost_period_id` → `ck_biz_si_production`
   - PRODUCTION + `is_sourceless=TRUE` → `ck_biz_si_production`
   - **PRODUCTION + 无 `source_order_id` → `ck_biz_si_sourceless_pair`（★ 见 §4 开放问题）**
6. down 护栏：有 PRODUCTION 单 / 有挂对象的出库单时，两次都拒绝执行并点名数量
7. down 正常执行 → 结构与值域回滚干净；down×2 幂等；回滚后存量指纹仍相等 ✅
8. 回滚后重放 up → 重建成功，存量指纹仍相等 ✅
9. 收尾：演练探针零残留（`residue_in=0 / residue_out=0`）

## 4. ★ 已知开放问题（设计 §3.3 与 V0199 冲突，本卡未擅改）

§3.3 的 `ck_biz_si_production` 要求 `is_sourceless = FALSE`，而 V0199 的
`ck_biz_si_sourceless_pair` 钉死 `is_sourceless = (source_order_id IS NULL)`。
两条叠起来的实际效果：**一张 PRODUCTION 单必须带 `source_order_id`（采购单）**。

但 §4.7 的结算路径根本没有采购单（结算批次在同一事务内直接生成入库单），
`02_up_down_drill.out` 第 8 段已复现：

```
insert ... in_type='PRODUCTION', cost_object_id, cost_period_id, is_sourceless=FALSE  (无 source_order_id)
→ ERROR: new row for relation "biz_stock_in" violates check constraint "ck_biz_si_sourceless_pair"
```

本卡按卡面要求「设计已定稿，不重新设计」逐字落地 §3.3，**没有**动 V0199 的约束语义；
`V0376` 的自检把这一事实打进部署日志（NOTICE，不会拦截迁移），迁移头注释也写明了。
最小修法（建议由 B3-4 结算卡或一张独立迁移来做，编号从 V0377 起算需先核对）：

```sql
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_sourceless_pair;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_sourceless_pair
    CHECK (CASE WHEN in_type = 'PRODUCTION'
                THEN is_sourceless = FALSE AND source_order_id IS NULL
                ELSE (is_sourceless = (source_order_id IS NULL)) END);
```

即「完工入库 = 没有采购单，但也不是无源单（来源是结算批次，用 `cost_period_id` 标识）」。
**不做这个放宽，B3-4 的结算第 ③a 步（INSERT biz_stock_in）一跑就报错。**

## 5. 复现命令

```bash
# 静态不变量（不连库）
cd backend && cargo test --test cost_b1_5_stock_out_in_cost_object_migration

# 空库全链重放
bash scripts/verify/mig_replay_fresh.sh costb15

# 真库往返（需要一个已跑到 V0374 的库）
bash docs/evidence/2026-11-10-cost-b1-5-v0375-v0376/02_up_down_drill.sh <db> /tmp/drill.out
```
