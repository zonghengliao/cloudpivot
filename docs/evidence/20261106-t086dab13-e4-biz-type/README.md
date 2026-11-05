# t_086dab13 · erp_invariants E4 白名单缺 OPENING / STOCK_TAKE_GAIN / STOCK_TAKE_CANCEL → 期初/盘盈库必假红

**结论：已修。** E4 的「其他合法入库」从手写 4 值白名单改为**登记表驱动**（22 个代码值 + 1 个遗留值），
新增 E4b（未登记 biz_type 判红）/ E4c（遗留值 WARN）两道配套闸，并加了可复跑的回归
（正样本 + 2 个负样本 + 遗留值样本，21 断言）。产品代码零改动。

| 项 | 值 |
|---|---|
| 被修脚本 | `scripts/verify/erp_invariants.py`（E4 判据 + 新增 E4b/E4c） |
| 新增回归 | `scripts/verify/regression/erp_invariants_e4_regression.py` |
| 复现库（原样保留） | `cloudpivot_st9`（127.0.0.1:5432，隔离库；**生产 144 未碰**） |
| 回归库 | `cloudpivot_e4reg_t086`（由 `cloudpivot_st9` TEMPLATE 克隆的隔离库） |
| 提交 | 见本卡 handoff（脚本 + 本证据目录） |

---

## 一、根因

E4（改前 L580-607）把「会产库存 IN 的来源」写成了**手写白名单**：

```sql
other_moves as (… from biz_inventory_txn
                where biz_type in ('TRANSFER_IN','TRANSFER_OUT','SERIAL_IN','SERIAL_OUT'))
```

而 `biz_inventory_txn.biz_type` 的实际取值域有 22 个（`doc_kind.rs::TxnBizType::ALL` 19 个
+ 盘点三态 3 个只在 `stocktake_model.rs` 里）。凡是**不经过 `biz_stock_in` 单据轴**的入库，
白名单里没写就一条都扣不掉，于是 `库存 > 有效入库` 恒成立：

| biz_type | 方向 | 谁写 | 改前是否计入 |
|---|---|---|---|
| `OPENING`（库存期初） | IN | `opening.rs:1471/1520` | ❌ 漏 |
| `STOCK_TAKE_GAIN`（盘盈） | IN | `stocktake_model.rs:951` | ❌ 漏 |
| `STOCK_TAKE_CANCEL`（盘亏作废回补） | IN（方向与正向相反） | `stocktake_model.rs:1105/1175` | ❌ 漏 |
| `STOCK_TAKE_LOSS`（盘亏） | OUT | `stocktake_model.rs:1042` | ❌ 漏（漏 OUT 会让 E4 偏松） |

实测（隔离库 `cloudpivot_st9`，三本账自洽 100 = 100 = 100、1405 余额 1000 = 库存金额 1000）：

```
改前：🚨 [状态] E4 … 1 组库存大于有效入库量（有作废单没退库存）
        ↳ 定位：ST9-M-001 | P1-FIX-9 验收总仓 | 0 | 0 | 100.0000 | 100.0000
      结果：3/4 通过   🚨 BLOCKER 1
```
（`st9_before_fix_group_state.txt`；改前脚本取自 `git show HEAD:scripts/verify/erp_invariants.py`）

危害不止假红：`scripts/check.sh` 闸③ 直接吃这个退出码，一旦生产启用期初/盘盈就阻断所有提交；
而且**真正「作废没退库存」的 P0 会被这条恒红淹没**。

---

## 二、改法（判据强度只增不减）

1. **登记表替代白名单** —— `erp_invariants.py` 顶部新增 `E4_BIZ_TYPE_AXIS`，把
   `biz_inventory_txn.biz_type` 逐条登记，三种归属：
   - `DOC`（`PURCHASE_IN` / `PURCHASE_IN_CANCEL`）：由 `biz_stock_in` 单据轴解释
     （过账写 `PURCHASE_IN` @ `stockin_write.rs:551`，作废写 `PURCHASE_IN_CANCEL` @ `stockin_cancel.rs:270`），
     单据 status 一翻，`posted` 与流水同步涨落 ⇒ **不得再计入 other_moves**（否则重复扣减，反而放松判据）；
   - `NET`（其余 20 个）：按 `txn_type` 正负号计入 `other_moves`；
   - `LEGACY`（`PURCHASE` + `doc_type='PURCHASE_ORDER'`）：现行代码不产、老库里确实有
     （`cloudpivot_p1v` / `cloudpivot_nightreg` 各 1 条 ×5），按 NET 计入以免老库假红，但 E4c 亮 WARN。
2. **E4b（BLOCKER）**：库里出现登记表外的 biz_type（`null` 也算）→ 直接红，并打印
   「值 / doc_type / 方向 / 条数 / 净额 + 登记到哪个文件」。这样新 biz_type 不可能被静默漏算。
3. **E4c（WARN）**：遗留值仍在用 → WARN 提示确认来源/清理，不阻断。
4. **静态漂移闸**（回归脚本闸①）：解析 `doc_kind.rs::TxnBizType::as_str()` 与 9 个动账写入模块的
   `BIZ_TYPE_*` 常量字面量，要求全部 ⊆ 登记表。代码新增 biz_type 而登记表没跟上 → 回归红。
   （反向验证：临时摘掉 `STOCK_TAKE_GAIN` 一行 → `gate_static_fail_when_unregistered.txt`，退出码 1）

E4 的 SQL 判据本体（`库存 > posted + other + 0.0001`、BLOCKER 级别）**一字未放宽**。

---

## 三、验收对照

| 验收项 | 结果 | 证据 |
|---|---|---|
| □ E4 在「期初 + 盘盈 + 盘亏作废」库上绿 | ✅ `cloudpivot_st9` E4/E4b/E4c 全绿；回归库夹具有确定的 155 = 50 + 100 + 5 − 8 + 8 | `st9_after_fix_group_state.txt`、`regression_run1.txt`[闸②] |
| □ 负样本（人为删一条回补流水）E4 必红 | ✅ 删掉 `STOCK_TAKE_CANCEL` 那 1 条 → E4 红、退出码 1、定位到夹具物料 `E4REG-M1 … 155.0000 \| 8.0000`（多出恰好 8） | `regression_run1.txt`[闸③] |
| □ 全量 `erp_invariants.py` 在该库 BLOCKER 0 | ✅ `cloudpivot_st9`：**67/68 通过，BLOCKER 0，WARN 1**（N2 账号无部门，允许） | `st9_after_fix_full.txt`、`st9_after_fix_full_json.txt` |
| □ git commit（脚本 + 证据） | ✅ 见本卡 handoff 提交号 | — |

附加（超出验收，用于证明「不只是把 E4 改绿」）：

- **E4b 负样本**：插一条 `biz_type='E4REG_UNKNOWN_TYPE'` → E4b 红并点出类型名（`regression_run1.txt`[闸④]）；
- **遗留值样本**：插 `PURCHASE/PURCHASE_ORDER` → E4 不错杀、E4b 不误判「未知」、E4c 出 WARN（[闸⑤]）；
- **拒绝生产**：`NIK_DATABASE_NAME=cloudpivot` 时脚本退出码 2、不动任何数据（`prod_guard.txt`）；
- **跨库 sanity**（7 个库）：E4 判定只在「有 legacy 行」的 p1v/nightreg 上变化——
  多出组数 8 → 7（legacy 那条被正确解释），其余原样（`cross_db_sanity.txt`）；
- 回归可重复：连跑两轮均 21/21（`regression_run1.txt` / `regression_run2.txt`），夹具自清理、无残留。

---

## 四、相邻问题（**不在本卡范围**，已另开卡给 `backend-developer`）

`scripts/check.sh` **闸④**（`biz_type_domain.py --strict`）在 `cloudpivot_st9` 上**本来就红**，
与本卡改动无关（`doc_kind.rs` 未改）：

```
[FAIL] ③ 库存变动动作  biz_inventory_txn.biz_type   ✗ 'STOCK_TAKE_CANCEL' ×6  ✗ 'STOCK_TAKE_LOSS' ×5
[FAIL] ③ 库存变动动作  biz_voucher_rule.biz_type    ✗ 'EXPENSE_PAYMENT' ×1    ✗ 'EXPENSE_REPORT' ×1
[FAIL] ④ 流水来源单据  biz_inventory_txn.doc_type   ✗ 'STOCK_TAKE' ×12
```

根因同源：`doc_kind.rs` 的 `TXN_BIZ_TYPE_VALUES` / `TXN_DOC_TYPE_VALUES` / `VOUCHER_RULE_BIZ_TYPE_VALUES`
没跟上盘点三态（`STOCK_TAKE_GAIN/LOSS/CANCEL` + `doc_type='STOCK_TAKE'`）与报销（`EXPENSE_REPORT/PAYMENT`）。
证据：`gate4_vocab_st9_preexisting.txt`。**这条同样会在生产启用盘点后阻断闸④**，属产品代码改动，
本卡按「不改业务代码」约束未动，已开衍生卡。

---

## 五、复跑

```bash
# ① 静态漂移闸（不连库，可进 pre-commit）
python3 scripts/verify/regression/erp_invariants_e4_regression.py --static-only

# ② 全套回归（需隔离库；重造克隆库：）
#    su postgres -s /bin/sh -c 'createdb -O alun -T cloudpivot_st9 cloudpivot_e4reg_t086'
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=<口令> \
NIK_DATABASE_NAME=cloudpivot_e4reg_t086 \
python3 scripts/verify/regression/erp_invariants_e4_regression.py

# ③ 复现库全量审计（本次验收口径）
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=<口令> \
NIK_DATABASE_NAME=cloudpivot_st9 python3 scripts/verify/erp_invariants.py
```

> 回归脚本会**改写**库存轴数据（只碰 `E4REG-` 前缀的夹具行，跑前跑后都清理），
> 因此内置保护：库名不在 `cloudpivot_*` 白名单、或含 `prod`、或是 `cloudpivot`/`postgres` → 直接退出码 2。

---

## 六、复核（父卡 t_066e9f2d 完成后重新调度，2026-11-06 01:2x）

本卡首次执行时被后来新增的父卡 `t_066e9f2d`（P1-FIX-11）挡住，`complete` 未成；父卡完成后重新调度，
在**主仓已前进到 `a6e15be`** 的状态下把两条验收口径原样复跑了一遍（脚本 diff 仍为 `fccded1`，未被他人改动：

```
$ git log --oneline -1 -- scripts/verify/erp_invariants.py scripts/verify/regression/erp_invariants_e4_regression.py
fccded1 fix(verify): erp_invariants E4 白名单改登记表驱动（补 OPENING/盘点三态）+ E4b/E4c + 回归（t_086dab13）
$ git status --short -- scripts/verify/erp_invariants.py    # 无输出 = 工作树与提交一致
```

| 复核项 | 命令 | 结果 | 存档 |
|---|---|---|---|
| ① 全量审计（复现库） | `NIK_DATABASE_NAME=cloudpivot_st9 python3 scripts/verify/erp_invariants.py` | **67/68 通过 🚨 BLOCKER 0 ⚠️ WARN 1（N2 账号无部门）**，退出码 0 | `recheck_st9_full.txt` |
| ② 回归第 1 轮 | `NIK_DATABASE_NAME=cloudpivot_e4reg_t086 python3 scripts/verify/regression/erp_invariants_e4_regression.py` | **21/21 断言通过**，退出码 0 | `recheck_regression_run1.txt` |
| ③ 回归第 2 轮（同库连跑，验夹具自清理） | 同上 | **21/21 断言通过**，退出码 0 | `recheck_regression_run2.txt` |

E4 / E4b / E4c 在复现库上的逐条输出（`recheck_st9_full.txt` L51-53）：

```
✅ [状态] E4  作废单的库存已回滚（库存 ≤ 有效入库＋其它合法动账净额＋0.0001）
✅ [状态] E4b 库存流水 biz_type 已全部登记进 E4 判据
✅ [状态] E4c 无遗留 biz_type（现行代码已不产，历史行建议确认/清理）
```

> 注：卡面写的复现库端口 `99` 是误记，实际实例在 `127.0.0.1:5432`（`cloudpivot_st9` / `cloudpivot_e4reg_t086` 均在此）。
