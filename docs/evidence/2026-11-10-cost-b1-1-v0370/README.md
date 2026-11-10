# COST-B1-1（V0370：`biz_lot` + `biz_cost_object`）实测证据

卡：`t_f2b6d80a`（COST-B1-1【成本】迁移）
设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.1 ① ②
迁移文件：`backend/migrations/business/V0370__cost_lot_and_cost_object.{up,down}.sql`

> **号段偏差**：设计稿 §8.1 写 B1-1=V0368，但 `V0368` / `V0369` 已被
> `receipt_refund_page_and_price_page` / `perf_list_order_indexes` 占用，
> 而 nik 迁移加载器要求版本号**跨目录全局唯一**
> （`backend/vendor/nik-db/src/migrate/loader.rs`，文件头第 18 行与 `load_from_sources` 注释），
> 重号会让服务启动直接报错。故本卡顺延到 **V0370**，后续成本卡从 V0371 起排。

## 01_replay_fresh.out —— 空库全链重放

```
bash scripts/verify/mig_replay_fresh.sh costb1_02
→ PASS —— 290/290 条版本化迁移在空库上按版本号升序全部通过
```

作用：证明 V0370 **没有乱序依赖**（自检不引用更晚迁移才建的表 / 菜单码），
全新安装与灾难重建这条路能跑通。

## 02_up_down_drill.out —— 真库往返演练（同一库 `mig_replay_costb1_01`）

重跑：`bash 02_up_down_drill.sh mig_replay_costb1_01 /tmp/drill.out`
（库由 `KEEP_DB=1 bash scripts/verify/mig_replay_fresh.sh costb1_01` 造出）

| 步骤 | 断言 | 实测 |
|---|---|---|
| up 连跑 3 次 | 幂等、结果不变 | 9 条 `already exists, skipping` + 末行 `V0370 完成…探针已清理`，exit=0 |
| 探针残留 | 重复执行零垃圾 | `biz_lot_rows=0` / `biz_cost_object_rows=0` |
| 索引/约束数量 | 与设计一致 | `idx_biz_lot=4`（PK+3）、`idx_biz_cost_object=5`（PK+4）、`checks=11` |
| 唯一键谓词 | 含软删条件 | `CREATE UNIQUE INDEX uk_biz_lot_material_no … WHERE (deleted_at IS NULL)`；`uk_biz_cost_object_no … WHERE (deleted_at IS NULL)` |
| 软删后重建 | 允许 | 同 `(material_id, lot_no)` 软删后再插活行成功 |
| 活行重号 | 必须被拦 | `ERROR: duplicate key value violates unique constraint "uk_biz_lot_material_no"` |
| 关批无时间戳 | 必须被拦 | `ERROR: … violates check constraint "ck_biz_cost_object_closed"` |
| down | 回滚零残留 | `biz_lot=NULL biz_cost_object=NULL`、`idx_left=0` |
| down 再跑 | 幂等 | `table … does not exist, skipping` + `V0370 down 完成`，exit=0 |
| 回滚后重新 up | 可重放 | `V0370 完成…`，两表重建 |
| 下游护栏反证 | 有下游表时拒绝 | 造出 `biz_cost_collect` 后 down 被拒：`V0370 down 拒绝执行：下游成本表仍在（biz_cost_collect），请先回滚建立它们的迁移（V0371 及之后）` |
