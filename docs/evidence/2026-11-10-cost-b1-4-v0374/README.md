# COST-B1-4 实测证据 —— 迁移 V0374：二期三表（标准成本 + 差异）

- 卡：`t_b8b96fac`（卡面标题写「迁移 V0372」，实际落 **V0374**，理由见下）
- 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md` §3.2 ⑪⑫⑬、§4.9 差异分析、
  §8.1 拆卡清单 B1-4「二期三表（先建表，代码后写）」
- 范围：**只建表，不写业务代码**（差异计算属二期 S-* 卡）

## 号段偏差（已在卡内点名）

卡面写 V0372，但 V0372（分摊两表）/ V0373（期间两表）已被前置卡 B1-3 占用
（commit `a9b46ee1`）。nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号会让
服务启动直接失败（AGENTS 约定 6）⇒ 本卡落 **V0374**，设计 §7.1 的清单整体后移 2 位：

| 卡 | 设计 §7.1 | 实际号段 |
|---|---|---|
| B1-1 | V0368 | V0370 |
| B1-2 | V0369 | V0371 |
| B1-3 | V0370/V0371 | V0372/V0373 |
| **B1-4** | V0372 | **V0374**（本卡） |
| B1-5 | V0373/V0374 | V0375/V0376 |

## 建了什么

| 表 | 内容 | 索引 | CHECK | 外键 |
|---|---|---|---|---|
| `biz_cost_standard` | 标准成本卡（卡头）：单位标准料/工/费，按 `effective_from` 版本化 | `uk_biz_cost_std (material_id, effective_from) WHERE deleted_at IS NULL` | 5 | 0（物料/人员全弱引用，§3.4） |
| `biz_cost_standard_item` | 卡明细（标准 BOM）：标准用量/标准价 → 材料量差依据 | `uk_biz_cost_std_item (standard_id, line_no)` | 2 | 1：`→ standard_id ON DELETE CASCADE` |
| `biz_cost_variance` | 差异明细（六类） | `idx_biz_cost_var_period (period_code, variance_type)`、`idx_biz_cost_var_obj (cost_object_id, period_code)`（均非唯一） | 5 | 2：`→ cost_object_id` / `→ standard_id` 均 RESTRICT |

合计：3 表 / 7 索引（4 业务 + 3 PK）/ 12 CHECK / 3 外键。

## 库层防线（本卡的核心价值）

差异分析最容易「静默算错」的四处，全部落成 CHECK，自检里逐条反证：

1. `ck_biz_cost_std_sum`：标准总成本 = 料 + 工 + 费（**差异报表的分母**）
2. `ck_biz_cost_std_labor`：标准人工成本 = `round(标准工时 × 标准工资率, 6)`
3. `ck_biz_cost_std_item_cal`：明细金额 = `round(标准用量 × 标准价, 6)`
4. `ck_biz_cost_var_sign`：`direction` 与 `amount` 符号锁死（不利 ≥ 0 / 有利 ≤ 0，§4.9.4）

以及 `ck_biz_cost_std_money`（无负成本）、`ck_biz_cost_std_dates`（`effective_to >= effective_from`）、
`ck_biz_cost_std_ver`、`ck_biz_cost_std_item_num`（`std_qty > 0`）、
`ck_biz_cost_var_type`（六类）、`ck_biz_cost_var_elem`（三要素）、`ck_biz_cost_var_period`（yyyy-MM）。

> **差异不入账**（设计 D-15）：本卡三表只服务差异报表，1404 一分不动，不参与存货计价。
> 将来要入账需另立 ADR + `biz_cost_variance_alloc`，与本表结构不冲突。

## 实测输出（真实执行，可复跑）

### 1. 空库全链重放 —— `01_replay_fresh.out`

```
bash scripts/verify/mig_replay_fresh.sh costb14
== 结果：PASS —— 294/294 条版本化迁移在空库上按版本号升序全部通过
```

（B1-3 时是 293/293，本卡 +1 = V0374）

### 2. 真库往返演练 —— `02_up_down_drill.out`（脚本 `02_up_down_drill.sh`，在 `mig_replay_costb1_01` 上跑）

```
1. 首轮 up + 第二、三轮 up                    → 3 次 exit=0（幂等）
3. tables=3 / idx=7 / checks=12 / fks=3       → 与设计逐项一致
   probe_leftover=0                            → 重复执行零残留
4. uk_biz_cost_std :: CREATE UNIQUE INDEX ... (material_id, effective_from) WHERE deleted_at IS NULL
6. standard_item -> standard confdeltype=c ；variance -> cost_object/standard confdeltype=r
7. 标准卡（单位口径 13.20/5.00/0=18.20）插入成功
   → 期望失败：同物料同生效日第二张活卡   → duplicate key value violates "uk_biz_cost_std"
   → 期望失败：总计 ≠ 料+工+费           → violates check constraint "ck_biz_cost_std_sum"
   → 期望失败：人工 ≠ 工时×工资率         → violates "ck_biz_cost_std_labor"
   → 期望失败：明细金额 ≠ 用量×单价       → violates "ck_biz_cost_std_item_cal"
   → 期望失败：标准用量=0                 → violates "ck_biz_cost_std_item_num"
8. 四条差异（材料价差+600 / 量差+3840 / 人工率差−180 / 效差+500）插入成功
   材料恒等式 PASS（Σ=4440 = 15000−10560）  ← §4.9.2 算例 C
   人工恒等式 PASS（Σ=320  = 4320−4000）
   → 期望失败：UNFAVORABLE 但 amount<0 → violates "ck_biz_cost_var_sign"
   → 期望失败：MAT_MIX               → violates "ck_biz_cost_var_type"
   → 期望失败：2026-13               → violates "ck_biz_cost_var_period"
9. → 期望失败：有差异时删标准卡   → RESTRICT（variance_standard_id_fkey）
   → 期望失败：有差异时删成本对象 → RESTRICT（variance_cost_object_id_fkey）
10. items_after_cascade=0（删卡头 ⇒ 明细 CASCADE 消失）
11. leftover=0
12/13. down 后 standard/item/variance=NULL、cost_object 仍在、idx_left=0
14. down 再跑一次 → 幂等（只打 NOTICE）
15. 回滚后重新 up → replay tables=3
16. 下游护栏：外部表 FK 到 biz_cost_standard 时
    → "V0374 down 拒绝执行：下游表仍在引用二期三表（zz_downstream）…"
17. final tables=3（三表在位，供二期 S-* 卡使用）
```

### 3. 静态不变量单测 —— `03_cargo_test_summary.out`

```
cargo test --test cost_b1_4_standard_and_variance_ddl_migration  → 12 passed / 0 failed
cargo test（全仓）                                              → 28 目标全 ok，4356 passed / 0 failed
cargo fmt --check                                               → 干净
```

## 迁移自检（写进 SQL，部署即验）

`V0374__cost_standard_tables.up.sql` 三个 `DO $$` 块：

- **前置护栏**：`biz_cost_object` 不存在 → 直说「先执行 V0370」
- **形态断言**：三表在、`uk_biz_cost_std` 谓词（列组合 + 软删）、`uk_biz_cost_std_item` 列组合、
  12 条 CHECK 计数、3 条外键删除动作（`c`/`r` 逐条）、11 列精度、2 条报表索引
- **行为断言 B1~B11**：唯一键正/反、软删后重建、四条自洽 CHECK 反证、明细金额/用量反证、
  CASCADE、符号约定四连反证、★§4.9.2 两因素恒等式（算例 C 真数字）、RESTRICT 双反证、
  探针零残留（明细/卡头/差异/成本对象四表逐表计数）

## 已知偏差 / 设计留白（未擅改设计）

1. **号段 V0372 → V0374**（前置卡占用，见上）。
2. **`biz_cost_variance` 没有唯一键 / 无 `deleted_at` / 无 `status`**：设计 §3.2 未给，
   差异是「按期间整删重灌」的计算产物（与 §4.4.5 分摊结果同口径），幂等由写入方（S-9）保证。
   本卡**未擅自加**唯一键；单测里反向锁死「不许加 `uk_biz_cost_var*`」。
3. **`variance_type` 与 `element` 没有交叉约束**（设计未给）：例如 `MAT_PRICE` 配
   `element='OVERHEAD'` 库层拦不住，只能靠写入方/接口校验。已记录，供二期 S-9 或三期
   追加 CHECK（属向后兼容的新迁移）。
4. **`ck_biz_cost_std_labor` 要求 `std_labor_cost = round(std_labor_hours × std_labor_rate, 6)`**：
   演练时先误用「累计人工成本 4000」写卡头被库层拦下 —— 这是**正确**行为（卡头是单位口径：
   0.10h × 50.00 = 5.00/件，× 800 件 = 4000）。该反例已写进演练脚本注释。
5. **未跑 clippy --all-targets**：本卡只新增测试文件与 SQL，无生产代码改动；`cargo test` 全绿、`cargo fmt` 干净。

## 复跑方式

```bash
# ① 空库全链重放（约 1 分钟）
bash scripts/verify/mig_replay_fresh.sh costb14

# ② 真库往返演练（需要一个已跑过 V0370 的库，例如 mig_replay_costb1_01）
bash docs/evidence/2026-11-10-cost-b1-4-v0374/02_up_down_drill.sh mig_replay_costb1_01 /tmp/drill.out

# ③ 静态不变量
cd backend && cargo test --test cost_b1_4_standard_and_variance_ddl_migration
```
