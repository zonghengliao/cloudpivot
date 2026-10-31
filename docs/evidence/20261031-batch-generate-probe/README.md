# 取证脚本 · 批量生成改判「无来源入库」（`t_1daca7f4`）

配套文档：`docs/技术方案-20261031-批量生成无来源入库.md`（§1.4 / 附录 A）。
这里的 5 个文件是**真跑过的**最小复现集，评审可以照着复跑，结论与文档里的数字逐位一致。
其中前 4 个（`probe_s1/s2/b2` + `decimal_serde`）是 v1 的**负向**取证（改前红），
`probe_design_v2.sql` 是 v2 新增的**正向**取证（按规格实现后 6 步全绿）。

## 0. 环境

- 本机 PG（与仓库 HEAD schema 一致）。本次取证的库是 `cloudpivot_test` 的**快照克隆**：

  ```bash
  su postgres -c "psql -c 'CREATE DATABASE arch_spec_probe TEMPLATE cloudpivot_test'"
  ```

  克隆库的迁移记录在 `schema_migrations`（`versioned|…|V0158…`），与仓库 `backend/migrations/business`
  最大号 `V0158` 对齐。
- 三个 SQL 全部包在 `BEGIN … ROLLBACK` 里，**不改任何数据**。
- ⚠️ 仓库在 `/root/` 下（目录 700），`su postgres` **读不到**：`psql -f` 会报
  `Permission denied`（实测）。先拷到 `/tmp` 再跑：

  ```bash
  mkdir -p /tmp/pb && cp docs/evidence/20261031-batch-generate-probe/probe_*.sql /tmp/pb/ && chmod 644 /tmp/pb/*.sql
  su postgres -c "psql -d arch_spec_probe -f /tmp/pb/probe_s2.sql"
  ```

- 本 README 里贴的输出是 **2026-10-31 从干净库重跑三个脚本得到的原样输出**（不是拼的），
  与 `docs/技术方案-20261031-批量生成无来源入库.md` §1.4 的数字逐位一致。

## 1. `probe_s2.sql` —— ADR 字面口径（S2：成本账 + 库位账落库位行）在第一次出库后自己变红

```bash
su postgres -c "psql -d arch_spec_probe -f docs/evidence/20261031-batch-generate-probe/probe_s2.sql"
```

实测输出：

```
=== A 生成后 ===                        === A 出库 1 件后 ===
A:成本账sum = 5.0000                    A:成本账sum = 9.0000    ← 5(库位行) + 4(NULL 桶重算)
A:库位账sum = 5.0000                    A:库位账sum = 5.0000
A:在库号数  = 5                         A:在库号数  = 4
```

⇒ `erp_invariants.py` B1 = |9−5|（BLOCKER 红）、C5 = |4−9|（WARN 红）。

## 2. `probe_s1.sql` —— S1（两本账落 NULL 汇总桶）两段对照

```bash
su postgres -c "psql -d arch_spec_probe -f docs/evidence/20261031-batch-generate-probe/probe_s1.sql"
```

```
=== B 出库 1 件后（现状代码：库位账不动） ===
B:成本账sum = 4.0000   B:库位账sum = 5.0000   B:在库号数 = 4      ← B1 仍红

=== C 出库 1 件 + 库位账重算后（本规格新增一步） ===
C:成本账sum = 4.0000   C:库位账sum = 4.0000   C:在库号数 = 4   C:avg_cost = 10.000000
```

⇒ 文档 §6「出库/报废侧必须补库位账重算」就是 C 段那一句 `INSERT … ON CONFLICT (material,warehouse,location_id)`。

## 3. `probe_b2.sql` —— 成本账加金额而不发凭证 ⇒ B2 差值 +50

```bash
su postgres -c "psql -d arch_spec_probe -f docs/evidence/20261031-batch-generate-probe/probe_b2.sql"
```

```
=== 写 50 元库存、不记账凭证之后 ===
B2 库存金额 = 7507.5000   （改动前 7457.5000，+50）
B2 1405余额 = 20.0000     （不变）
```

> 注：克隆库本身带着测试夹具，绝对值不可当真；**要看的只有「增量 +50 且 1405 不动」**这一条。
> prod 侧「两边平衡」的既有证据见 `docs/datafix-20261031-p11-m-w1a-004-recur.md:157-159`（435 = 435）。

## 4. `decimal_serde_probe.rs` —— `Option<Decimal>` 的 JSON 口径

```bash
# 在空的临时 cargo 工程里（依赖见 decimal_serde_probe.Cargo.toml），把一个 src/main.rs 换成该文件：
cargo run --offline -q
```

```
OK  {"unit_cost": 12.34}   -> Some(12.34)
OK  {"unit_cost": "12.34"} -> Some(12.34)
OK  {}                     -> None
OK  {"unit_cost": 0}       -> Some(0)
SER -> {"unit_cost":"12.345678"}
```

结论：`Option<Decimal>` 能同时接受 number 与字符串、缺字段给 `None`；**序列化输出是字符串**
（`rust_decimal 1.43.0`，未开 `serde-float`）⇒ 前端 `SerialBatchRes.unit_cost` 的类型必须是 `string`。

---

## 5. `probe_design_v2.sql` —— 本规格的**正向**取证（6 步全绿）

这是 v2 新增的脚本：用**与 Rust 侧逐字同形的 SQL**（`apply_cost` 的加法 upsert、`apply_location_qty` 的两个分支、
按桶重算、凭证两行分录）在真库上跑完整生命周期，每一步都断言 B1/B2/B4/C3/C4/C5 六条判据。

```bash
su postgres -c "createdb -T cloudpivot_test arch_spec_v3"
mkdir -p /tmp/archspec && chmod 755 /tmp/archspec
cp docs/evidence/20261031-batch-generate-probe/probe_design_v2.sql /tmp/archspec/
chmod 644 /tmp/archspec/probe_design_v2.sql
su postgres -c "psql -X -d arch_spec_v3 -f /tmp/archspec/probe_design_v2.sql"
```

2026-10-31 真跑输出（原样，未改一字）：

```
NOTICE:  ── 取证对象：物料 LNK-F01-72429401-M2 / 仓库 LNK-F01-72429401-WFA / 基线 B2 差值 7437.5000
NOTICE:  ✔ 步骤1 生成 5 件 @10.00：成本账 5 / 50.0000 / 10.000000，库位账 5，流水 IN 5@10=50，凭证 借1405 50 / 贷1901 50
NOTICE:  ✔ 步骤2 出库 1 件：成本账 4 / 40.0000 / 10.000000，库位账 4，凭证 借6401 10 / 贷1405 10
NOTICE:  ✔ 步骤3 二次生成 5 件 @13.00：成本账 9 / 105.0000 / 11.666667（= ROUND((40+65)/9, 6)），库位账 9
NOTICE:  ✔ 步骤4 报废 1 件：成本账 8 / 93.3333 / 11.666667，凭证 借1901 11.6667 / 贷1405 11.6667
NOTICE:  ✔ 步骤5 混桶生成 3 件 @15.00：成本账 2 个桶（NULL=8/93.3333/11.666667，库位=3/45/15.000000），库位账 8+3=11
NOTICE:  ✔ 步骤6 从库位桶出库 1 件：库位桶 2/30.0000，仓级 10，凭证 借6401 15 / 贷1405 15
NOTICE:  ════════════════════════════════════════════════════════════
NOTICE:  PROBE OK —— 6 步全部断言通过（B1/B2/B4/C3/C4/C5 每步全绿）
NOTICE:  ════════════════════════════════════════════════════════════
 汇总桶 |  qty   | amount  | avg_cost
--------+--------+---------+-----------
 t      | 8.0000 | 93.3333 | 11.666667
 f      | 2.0000 | 30.0000 | 15.000000

  b2_差值
-----------
 7437.5000
ROLLBACK
已 ROLLBACK —— 库内数据未变
```

三条要点：

1. **B2 增量恒 0**：`b2_差值` 与基线 `7437.5000` 逐位一致（5 张凭证把每次金额变动都配平了）。
   基线不为 0 是因为克隆库本身带历史差价（有库存没走凭证），生产库当前是 `32/32 / BLOCKER 0 / WARN 0`。
2. **步骤 6 是"踩坑复现"**：第一版脚本按"用序列号的 `location_id` 去 upsert 库位账"写，直接
   `B1 红：成本账 10.0000 / 库位账 13.0000` —— 因为序列号存的是 `biz_inventory_location.id`（行 id），
   而库位账的 `location_id` 列存的是**库位字典 id**。改成"以库位账自身行驱动回填"后 10 = 10 全绿。
   文档 §4.1 / §5.2 就是这条结论。
3. 脚本全程 `BEGIN … ROLLBACK`，**不改任何数据**（中途 `RAISE EXCEPTION` 时 psql 断开也会自动回滚）。

