# FIN-3 费用报销 — 验收实录（2026-11-05）

对应卡片：`t_0499dd6b`【FIN-3】费用报销实施；设计依据：`docs/architecture/ARCH-6-财务扩展设计.md` §4（D1–D11 决策在第二章）。

## 一、本目录的日志（全部是真跑产物，不是手写）

| 文件 | 内容 | 结论 |
|---|---|---|
| `dbsetup.txt` | 从模板库 `cloudpivot_fin2asset`（V0238）建 `cloudpivot_fin3`，再 up V0239–V0244 | 4 表 / 8 类型 / 2 规则 / 13 菜单行 / 7 节点 / 8 边 / 1 配置 |
| `instance_start.txt` | 隔离实例启动（`./target/debug/cloudpivot`，端口 18026） | 健康检查 200；`数据库迁移完成: 应用 6 个迁移`（走的是真 refinery 路径，不是 psql 直灌） |
| `e2e.txt` | `scripts/verify/fin3_e2e.py` 真机全链路 | **67 项全过** |
| `invariants.txt` | `scripts/verify/erp_invariants.py`（含新增 L1–L8） | **61/61 通过，BLOCKER 0 / WARN 0** |
| `migrate_drill.txt` | `scripts/verify/fin3_migrate_drill.sh`（up→down→up × 3 轮） | 42 个断言全过，graph_hash 每轮 `drifted = 0` |
| `fin3-e2e-server.txt` | 实例运行日志的**节选**（启动 + 迁移 + 全部 WARN/ERROR 行；全量日志不入库） | 无 ERROR |

## 二、复跑方式（顺序不能颠倒）

```bash
cd /opt/cloudpivot
bash scripts/verify/fin3_dbsetup.sh                 # ① 建干净库（会 DROP 同名库，库里不能有连接）
bash scripts/verify/fin3_instance.sh start          # ② 起隔离实例 :18026（应用自己的迁移）
NIK_DATABASE_NAME=cloudpivot_fin3 python3 scripts/verify/fin3_e2e.py    # ③ 全链路
NIK_DATABASE_NAME=cloudpivot_fin3 NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun python3 scripts/verify/erp_invariants.py   # ④ 不变量
bash scripts/verify/fin3_instance.sh stop
bash scripts/verify/fin3_dbsetup.sh                 # ⑤ 必须重置：V0243 的 down 有实例护栏
bash scripts/verify/fin3_migrate_drill.sh           # ⑥ up/down/up × 3
```

`V0243` 的 down 在「还有 expense 审批实例」时会 `RAISE` 拒绝回滚（实测原文：
`ERROR: V0243 回滚被拒绝：还有 4 条审批实例跑在 expense v1 上`）。这是护栏不是缺陷——
流程定义不允许在实例存活时消失。所以演练必须在干净库上跑。

## 三、单测（真库用例要靠 `ERP_DB_URL`，不设就静默 SKIP）

```bash
cd backend
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin3' \
  cargo test --lib business::erp::expense -- --test-threads=1
# → 39 passed（15 条 expense_model 真库用例 + 9 条 expense_compute 纯函数 + 7 条 expense_dto + 8 条 bridge/model）
```

## 四、这次真机验收覆盖的会计事实

1. 报销单 3 条明细（差旅 3,180 + 住宿 2,120 可抵扣 6% + 招待 1,060 不可抵扣）：
   不含税 6,060 / 税额 300 / 价税 6,360 —— 招待费税额 **0**、全额并入 `5602.04`。
2. 入账凭证：借 `5602.02` 5,000 + 借 `5602.04` 1,060 + 借 `2221.01` 300 / 贷 `2241.01` 6,360（借贷平衡）。
3. 付款（3,000 部分 + 3,360 付清）：每笔一张 `2241.01` 借 / `1002` 贷；`pay_status` UNPAID→PARTIAL→PAID。
4. 付清后作废 → **409**（`PAID` 不许作废）。
5. 另一张单审批通过后作废 → 蓝字凭证 `reversed_by` 非空 + 红字凭证（借贷互换）+ usage `RELEASED`。
6. 金额分级三档：`< 1000` 只走直属上级（无 `n_fin` 待办）；`≥ 10000` 直接进 `n_board` 会签（不经过中间档）。
