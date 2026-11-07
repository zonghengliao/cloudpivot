# t_c3c2793a：erp_invariants K5 排除 DISPOSED 卡片（假红收口）+ K5b 逐卡对账

> 归属：test-enginner｜来源卡：REVERIFY **t_2f92ee87** 新发现 ②
> 环境：145（`172.18.10.145`）/ PG 18 `127.0.0.1:5432`；库 `cloudpivot_rev_demo`（1 张 `DISPOSED` + 1 张 `IN_USE`）
> 提交：**974a883**（`fix(verify): erp_invariants K5 排除已处置卡片（+ K5b 逐卡对账）(t_c3c2793a)`）
> 改动：只碰 `scripts/verify/erp_invariants.py` + 新增 `scripts/verify/references/erp-k5-disposed.md`，
> 后端业务代码 0 改动、迁移 0 改动。

## 1. 结论

| 项 | 改前（c8f82c0） | 改后（974a883） |
|---|---|---|
| `cloudpivot_rev_demo` 退出码 | **1**（挡住 `scripts/check.sh` 闸③） | **0** |
| 汇总 | `75/78 通过  🚨 BLOCKER 1  ⚠️ WARN 2` | `77/79 通过  🚨 BLOCKER 0  ⚠️ WARN 2` |
| K5 | `🚨 [固资] K5 1602 贷方余额 = Σ卡片累计折旧 — 1602 = 0.0000 / 卡片 = 3166.6667` | `✅ [固资] K5 1602 贷方余额 = Σ在役卡片累计折旧`（`1602 = 0.0000`） |
| K5b（新增） | — | `✅ K5b 已处置卡的累计折旧 = 其处置凭证借 1602 的金额（逐卡）` |

两条 WARN（`B3 应付未付` / `N2 无部门`）与本卡无关：是**用例数据/演示种子**造成，已在
`docs/evidence/20261107-p0-reverify/README.md` §5 归因（分别属手工建卡无应付单、t_92234f6b 种子）。

## 2. 根因（复核，与卡面一致）

* 处置审批通过 → `asset_bridge::post_disposal_vouchers` 生成 1~3 张
  `source_type='FIXED_ASSET_DISPOSAL'`、`source_id = 资产 id` 的凭证；**凭证 1 固定**
  「借 **1602 = 卡片累计折旧** + 借 1606/1901 净值 / 贷 1601 原值」⇒ 处置凭证已把 1602 借平。
* 卡片只置 `DISPOSED`，`accumulated_depreciation` **保留历史值**（K2 要求它 = Σ明细 POSTED，
  清零会把 K2 弄红）。
* 原 K5 卡片侧 `where deleted_at is null`（未排除 DISPOSED），与科目侧口径不一致 ⇒
  任何一次处置后必报 BLOCKER。

实测凭证（`cloudpivot_rev_demo`，逐笔）：

| 凭证 | 科目 | 借 | 贷 | 摘要 |
|---|---|---|---|---|
| PZ…000008 | 1601 | 120000 | 0 | FA-…0001 固定资产购入 |
| PZ…000009 | 1602 | 0 | 3166.6667 | 2026-11 计提折旧汇总 |
| PZ…000010 | 1601 / 1602 / 1606 | 0 / **3166.6667** / 116833.3333 | 120000 / 0 / 0 | 出售处置（1/3） |
| PZ…000011 | 1606 | 0 | 1000 | 出售处置（2/3） |
| PZ…000012 | 1606 | 0 | 115833.3333 | 出售处置（3/3） |
| PZ…000019 | 1601 | 120000 | 0 | FA-…0002 固定资产购入 |

1602 借 3166.6667 / 贷 3166.6667 ⇒ 余额 0；在役卡（FA-…0002）累计折旧 0 ⇒ **K5 应当绿**。

## 3. 改法（两处）

1. **K5 卡片侧**：`where deleted_at is null and status <> 'DISPOSED'`（与 K6 同口径）；
   检查名改为「K5 1602 贷方余额 = **Σ在役卡片**累计折旧」（名字不改回来，避免下一个人照着旧名再写一遍）；
   覆盖行打印「在役 N 张 Σ / 已排除已处置 M 张 Σ」。
2. **新增 K5b（BLOCKER，逐卡对账）**：每张 `DISPOSED` 卡的 `accumulated_depreciation`
   必须 = 它**未红冲的蓝字**处置凭证对 1602 的借方合计
   （`source_type='FIXED_ASSET_DISPOSAL'` + `source_id=asset.id`，只认 `is_red = 0 and reversed_by is null`）。
   —— 排除 DISPOSED 后只剩总量口径，多张卡「一多一少」会互相抵消；K5b 把口径收紧回来。

四条互相印证与完整判定依据：**`scripts/verify/references/erp-k5-disposed.md`**。

## 4. 回归矩阵（全部实测，145）

| # | 库 / 构造 | 期望 | 实测 | 证据 |
|---|---|---|---|---|
| ① | `cloudpivot_rev_demo`（1 张 DISPOSED） | BLOCKER 0 | **77/79，BLOCKER 0，exit 0** | `raw/after_cloudpivot_rev_demo.txt`（改前：`raw/before_cloudpivot_rev_demo.txt`） |
| ② | 副本库 + **人为制造「1602 与卡片不符」**：`PZ…000009` 的 1602 贷方 +1000、`5602.01` 借方 +1000、凭证头同步（凭证仍平） | K5 必须报红 | **76/79，BLOCKER 1**：`K5 — 1602 余额 = 1000.0000 / 在役卡片 Σ = 0.0000`；K5b 仍绿（隔离到位），exit 1 | `raw/N2_tamper1602_cp_k5t1.txt` |
| ③ | 副本库原样（对照组，证明 ② 的差异只来自注入） | BLOCKER 0 | **77/79，BLOCKER 0** | `raw/N1_copy_asis_cp_k5t1.txt` |
| ④ | 副本库删掉 3 张处置凭证（= 处置没记账、卡片仍 DISPOSED） | K5 + K5b 必须报红（证明排除 DISPOSED 不放水） | **74/79，BLOCKER 3**：K5（1602 = 3166.6667 / 在役 0）、K5b（`FA-…0001 卡片 3166.6667 / 处置凭证借 1602 0`）、K6（1601 = 240000 / 在役 120000） | `raw/N3_clean_no_disposal_voucher_cp_k5t3.txt` |
| ⑤ | 无资产的库 `cloudpivot_rev_admin`（全新库只有 admin） | 不误伤 | **78/79，BLOCKER 0**（WARN 1 = N2 无部门，种子问题），exit 0 | `raw/C1_rev_admin_cloudpivot_rev_admin.txt` |
| ⑥ | 6 个含 DISPOSED 卡的历史库（fix4legacy / fix6_suite / fix7_idem / fix8_fresh / fix9 / rev_…0348） | 全绿（无新的假红） | 逐个 **11/11，BLOCKER 0，exit 0** | `raw/C2_*.txt` |
| ⑦ | 边界：**作废处置**（红冲 3 张 + 卡片回 IN_USE） | 不假红 | **11/11，BLOCKER 0**（1602 = 3166.6667 = 在役卡合计） | `raw/E1_cancelled_cp_k5t4.txt` |
| ⑧ | 边界：作废后**重新处置**（6 张蓝字里 3 张已 `reversed_by`） | 不假红、K5b 不双计 | **11/11，BLOCKER 0**（K5b 只算未红冲的 3 张） | `raw/E2_redisposed_cp_k5t4.txt` |

> ⑥ 的 6 个库每张 2 张 DISPOSED 卡、`Σ累计折旧 = 10450.0000`，K5/K5b 均绿 —— 说明「排除 DISPOSED」
> 不是只在 rev_demo 这一份数据上凑出来的。

副本库统一用 `pg_dump cloudpivot_rev_demo | psql -d <copy>` 造，**跑完全部 drop**（已核 `pg_database` 0 残留）。

## 5. 复跑（命令都在 `scripts/` 子目录）

```bash
# 改前/改后对比（c8f82c0 版 vs 974a883）
scp scripts/k5_run_145.sh root@172.18.10.145:/root/ && ssh root@172.18.10.145 'bash /root/k5_run_145.sh cloudpivot_rev_demo'

# ① ② ③ N1/N2/N3 + 多库矩阵
scp scripts/k5_neg_145.sh root@172.18.10.145:/root/k5_neg.sh && ssh root@172.18.10.145 'bash /root/k5_neg.sh'

# ④ 干净副本的「处置没记账」
scp scripts/k5_n3_clean.sh root@172.18.10.145:/root/k5_n3.sh && ssh root@172.18.10.145 'bash /root/k5_n3.sh'

# ⑦ ⑧ 作废 / 重新处置
scp scripts/k5_edge_145.sh scripts/k5_cancel.sql scripts/k5_redispose.sql root@172.18.10.145:/root/ \
  && ssh root@172.18.10.145 'bash /root/k5_edge_145.sh'
```

单库直接跑：

```bash
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
NIK_DATABASE_NAME=cloudpivot_rev_demo python3 scripts/verify/erp_invariants.py            # 全量
NIK_DATABASE_NAME=cloudpivot_rev_demo python3 scripts/verify/erp_invariants.py --group 固资
```

## 6. 副作用（诚实记录）

* **只改 1 个脚本 + 新增 1 份 references**，后端/前端/迁移 0 改动。
* 145 worktree 跑到 `974a883` 且 `git status` 干净；8051 / 8033 `health=200` 未重启。
* 造了 4 个临时副本库（`cp_k5t1` / `cp_k5t3` / `cp_k5t4`，另一个在首轮脚本里同名复用），**全部已 drop**；
  源库 `cloudpivot_rev_demo` **只读**（只 `select`，未写）。
* 144 未触碰；`cloudpivot_dev` 未写。

## 7. 遗留 / 不在本卡范围

* `B3 应付未付`、`N2 无部门` 两条 WARN 仍是「用例数据 + 演示种子」，分别归
  t_92234f6b（种子部门/`expense:submit`）与手工建卡口径，**不是脚本假红**。
* `K5` 检查名从「Σ卡片累计折旧」改为「Σ在役卡片累计折旧」：`docs/evidence/2026*` 里的历史快照
  仍写旧名（点时间快照，不回改）；新写的文档请用新名。
