# P15 · 「未分配桶」并进「默认库位桶」（E2E-FIX-6 ③ 存量口径）

> 一次性/可重放的数据修复脚本，不是业务代码。代码侧的根因修复见
> `serial_model::ensure_batch_location_row` 与 `docs/adr-20261031-erp-serial-batch-generate-semantics.md` §7。

## 要修的是什么

E2E-FIX-6 之前，`POST /business/erp/serial/batch-generate` **不接受也不透传库位**：

```text
批量生成 → 成本账 biz_inventory.location_id = NULL、库位账 location_id = NULL、序列号 location_id = NULL
收货/期初 → 落「默认库位桶」biz_location.is_default = 1
⇒ 同物料同仓两个桶并存：
   · 库位库存页多一行「(未分配)」
   · 扫码只看到仓库、看不到库位
   · 巡检逐桶对账（erp_invariants B1b）对不上
```

代码已修（新数据不再分裂）。存量数据两种处理：

| 情况 | 处理 |
|---|---|
| 测试/演练数据 | **不用管**（生产已清空；隔离库跑完 E2E 直接删库） |
| 真实业务数据 | 用本目录的脚本把「未分配桶」并进「默认库位桶」 |

## 三个脚本

| 文件 | 作用 |
|---|---|
| `10_report_two_bucket.up.sql` | **只读**报告：列出有未分配桶的 (物料, 仓库)，并标出该仓是否有默认库位、是否已存在默认库位桶、未分配桶里有多少在库序列号 |
| `20_merge_unassigned_into_default.up.sql` | 合并：默认库位桶 += 未分配桶（成本账 qty/amount 相加、avg_cost 重算；库位账 qty 相加），在库序列号改指默认库位账行，未分配桶清零（**保留行**） |
| `20_merge_unassigned_into_default.down.sql` | **不可自动回滚**（合并后无法区分「本来就在默认桶」与「刚从未分配桶并过来」），故意 RAISE EXCEPTION；回退请用 `--backup` 的备份 |
| `apply.sh` | `--check` / `--backup` / `--apply` 三个动作的入口 |

## 怎么跑

```bash
# 连接默认 127.0.0.1:5432、alun/alun、库由 P15_DB（或 NIK_DATABASE_NAME）指定
cd /opt/cloudpivot
P15_DB=<目标库> bash scripts/datafix/p15/apply.sh --check    # ① 只读：先看范围
P15_DB=<目标库> bash scripts/datafix/p15/apply.sh --backup   # ② 备份 biz_inventory / biz_inventory_location / biz_material_serial
P15_DB=<目标库> bash scripts/datafix/p15/apply.sh --apply    # ③ 合并（事务内；守恒校验失败自动回滚）
```

`--apply` 会先跑一次报告、再合并、再跑一次报告，输出里 `NOTICE: 合并 物料… 仓库… → 库位账行…：qty … / amount …` 就是逐条动作。

## 安全阀（都写在 SQL 里）

- **只动有默认库位的仓**：没有默认库位的仓，收货也落未分配桶，这是正常形态 —— 跳过并 `RAISE NOTICE` 说明。
- **只动未分配桶**：已分配库位桶的 qty / avg_cost 一个字节不动。
- **守恒**：合并前后按 (物料, 仓库) 比对 `SUM(qty)` / `SUM(amount)`，不一致 `RAISE EXCEPTION` 整单回滚。
- **幂等**：未分配桶清零后不再命中（`qty <> 0 OR amount <> 0`），重跑不重复加。
- **不删行**：未分配桶行程清零而非删除（行 id 仍被历史流水/审计引用）。
- **序列号同步**：只改 `status='IN_STOCK'` 的未分配序列号（在库件数是账实依据）；历史 OUTBOUND/SCRAPPED 行保持原样。

## 实跑证据

`docs/evidence/20261109-e2efix6-serial-location/` 的 `01_drill.py` 里有这一段：
造旧代码形态（未分配桶 qty=2 + 2 件裸 NULL 序列号）→ 报告点名 → 合并 → 未分配桶清零、
默认桶 3+2=5、仓级合计守恒 7=7、在库序列号 0 件无库位、复查报告不含该仓、巡检仍不报该物料。
