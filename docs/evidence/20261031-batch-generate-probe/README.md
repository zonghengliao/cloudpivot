# 取证脚本 · 批量生成改判「无来源入库」（`t_1daca7f4`）

配套文档：`docs/技术方案-20261031-批量生成无来源入库.md`（§1.4 / 附录 A）。
这里的 4 个文件是**真跑过的**最小复现集，评审可以照着复跑，结论与文档里的数字逐位一致。

## 0. 环境

- 本机 PG（与仓库 HEAD schema 一致）。本次取证的库是 `cloudpivot_test` 的**快照克隆**：

  ```bash
  su postgres -c "psql -c 'CREATE DATABASE arch_spec_probe TEMPLATE cloudpivot_test'"
  ```

  克隆库的迁移记录在 `schema_migrations`（`versioned|…|V0158…`），与仓库 `backend/migrations/business`
  最大号 `V0158` 对齐。
- 三个 SQL 全部包在 `BEGIN … ROLLBACK` 里，**不改任何数据**。

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
