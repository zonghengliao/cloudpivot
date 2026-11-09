# 证据：E4b 登记缺口 `SALES_RETURN_IN`（真 BLOCKER）+ 孤儿应收口径补 `source_type`

卡片：`t_9c938b6b`（REVIEW-FIX-3 跟进 / 上一张 `t_3ec444ff` 的 I-5 清库发现）
结论：**是代码枚举漏（不是写入点用错值）**，已补进词表 + E4 登记表，按 **DOC 轴**登记；
回归闸从「手写模块清单 + 只比 domain↔registry」改成「写入点 ⊆ 词表 ⊆ 登记表」三层。
`cloudpivot_dev` 一行数据没动，E4b 由 🚨 转 ✅（72/80 → 73/80）。

---

## 1. 三个问题的判定与依据

### ① 是代码枚举漏，还是写入点不该用这个值？→ **枚举漏，写入点是对的**

- 写入点：`backend/src/business/erp/sales_return_posting.rs:629` 用
  `BIZ_TYPE_SALES_RETURN_IN`（`"SALES_RETURN_IN"`）写 `biz_inventory_txn`。
- 为什么不能改成 `PURCHASE_IN`：退货入库单（`biz_stock_in.in_type='RETURN'`、
  `source_type='SALES_RETURN'`）没有供应商 / 应付 / 进项税，记成采购入库会让
  「库存流水 → 采购入库」的追溯给出假答案（该文件头注释已写明）。
- 为什么不能改成 `PURCHASE_RETURN`：那是**采购**退货出库（OUT 方向，`doc_type='STOCK_OUT'`）。
- 所以正确做法是把它补进唯一取值域：`doc_kind.rs::TxnBizType::SalesReturnIn`
  （`as_str` = `SALES_RETURN_IN`、`label` = 销售退货入库、`doc_kind()` = `DocKind::StockIn`）
  + `TXN_BIZ_TYPE_VALUES` 22 → **23** + 前端镜像 `doc-kinds.ts::TXN_BIZ_TYPES`
  （Rust 侧 `frontend_ts_mirror_matches_rust` / `frontend_ts_labels_match_rust` 逐字盯着）。
- 没动 `sales_return_posting.rs`（另一张卡正在改它的格式，避免撞车；它写的字面量本来就正确，
  现在被闸①以「写入点 ⊆ 词表」直接盯着，漂不了）。

### ② 按 `NET` 轴还是 `DOC` 轴？→ **DOC**（有反证，见下）

事实（`01_probe.out`，145 `cloudpivot_dev`）：

```
SALES_RETURN_IN | STOCK_IN | IN | doc_id=01a1bddc-… | IN-20261109-000243 | qty 2
  ↳ 对应 biz_stock_in：status=POSTED / in_type=RETURN / source_type=SALES_RETURN / 未软删 / 明细 qty 2
```

E4 的 `posted` CTE 是 `biz_stock_in where deleted_at is null and status='POSTED'`，
**不过滤 `in_type`** ⇒ 这 2 件已经被「单据轴」计入。若登记成 `NET`，`other_moves` 会**再加 2**，
判据静默放宽 2 件。

反证（`07_axis_net_vs_doc.out`：库存 4 件、唯一来源是该流水、其入库单被撤成 `DRAFT`）：

| 登记口径 | E4 判定 | 含义 |
|---|---|---|
| `"SALES_RETURN_IN": "DOC"`（本卡） | 🚨 红，多出 4.0000 | 4 件库存没人解释 —— 判据抓得住 |
| `"SALES_RETURN_IN": "NET"`（备选） | ✅ 绿 | NET 把这 4 件当「其它合法动账」，真问题**静默通过** |

同一结论在 145 上复跑一致（`14_axis_145.out`）。

### ③ 静态回归为什么没抓到 + 现在怎么守

旧闸（`regression/erp_invariants_e4_regression.py` 闸①）两处盲：

1. 写入点模块是**手写清单** `E4_TXN_WRITER_MODULES`（9 个文件），
   `sales_return_posting.rs` 不在里面 ⇒ 它的常量从来没被扫到；
2. 只比「枚举 + 清单里的常量」↔ 登记表，**没有「写入点 ⊆ 词表」这一环**。
   写入点用了词表里根本没用的值时，枚举侧与登记表侧**同时缺同一个值**，集合相等发现不了。

现在（`05_gate_before_after.out`，用 `git show HEAD` 取出修前三份真相文件对照）：

```
[旧闸①] 未登记的 biz_type = []   → ★ 判绿（漏了真缺口）
[新闸①] 探测到写入点模块 10 个（含 sales_return_posting.rs）
[新闸①] L1 写入点 ⊆ 词表   未通过 = ['SALES_RETURN_IN']
[新闸①] L2 写入点 ⊆ 登记表 未通过 = ['SALES_RETURN_IN']
[新闸①] L3 词表 ⊆ 登记表   未通过 = []    ← 正是旧闸的盲区
```

新闸结构：

- **写入点内容探测**（不维护清单）：文件里出现 `inventory_writer::insert_txn` / `TxnRecord {`
  即算写入点；另设两条锚点（`erp/inventory_writer.rs` + `erp/stockin_write.rs`）——
  探测规则一旦失效（比如函数改名），锚点消失直接红，不许按「扫不到 = 没问题」放行。
- **三层逐层断言**：写入点字面量 ⊆ `TXN_BIZ_TYPE_VALUES` ⊆ `E4_BIZ_TYPE_AXIS`；
  外加「登记表词表外只允许 LEGACY」与「前端 `doc-kinds.ts` 镜像逐字同序」。
- **内置两条负样本自检**（每次跑都跑，不需要数据库）：
  注入一个写了 `E4REG_GATE_FAKE` 的假写入点模块 ⇒ 必须红并点名；
  摘掉登记表的 `OPENING` 一行 ⇒ 必须红并点名。

---

## 2. 实测（145 / 本机隔离库）

| 动作 | 结果 | 证据 |
|---|---|---|
| 修前 `cloudpivot_dev` 巡检 | 72/80，BLOCKER 5（含 E4b） | `02_before_dev_invariants.out` |
| **修后 `cloudpivot_dev` 巡检** | **73/80，BLOCKER 4**；E4b ✅、E4c ✅、S6 ✅ | `10_dev_invariants_after.out` |
| 干净基线 `cloudpivot_dev_clean` 验收闸 | **80/80 全绿，RC=0**（未被本卡影响） | `11_acceptance_gate_clean.out` |
| E4 回归（145，dev_clean 的拷贝库） | **34/34 断言全绿**，RC=0 | `12_e4_regression_145.out` |
| E4 回归（本机隔离库） | 34/34 全绿 | `06_e4_regression.out` |
| 静态闸 | 14/14 全绿 | 同上文件开头 |
| `biz_type_domain.py`（dev） | ③ 库存变动动作 **PASS**（修前域外值 `SALES_RETURN_IN`） | `13_dev_biz_type_domain.out` |
| 轴口径反证 | DOC 红 / NET 绿 | `07_axis_net_vs_doc.out`、`14_axis_145.out` |
| 修前/修后闸对照 | 旧闸绿、新闸点名 | `05_gate_before_after.out` |
| 编译/类型 | `cargo check --lib` 通过；`doc_kind` 21 条单测全绿（含前端镜像 2 条）；`pnpm typecheck` RC=0 | `16_cargo_check_lib.out`、`15_frontend_typecheck.out` |

**`cloudpivot_dev` 仍剩 4 条 BLOCKER，与本卡无关（历史夹具）**：

- `B2 库存金额 = 1405 余额` 差 -501625：库存模块 28811 vs 总账 530436（总账侧是整库累计，含历史夹具凭证）
- `B5 出库单已过账 → 必有成本结转凭证`：1 张历史出库单
- `S1` / `S5`：`SO14T-*` / `RC-20261107-*` 两张历史夹具单

判据：这 4 条在修前就在（`02_before_dev_invariants.out` 的 5 条 BLOCKER 里除去 E4b），
且 `cloudpivot_dev_clean` 同样代码跑到 **80/80** ⇒ 代码没错，是演练库历史残留。
按 `docs/运维规程-演练库与验收基线.md`：验收只看 `cloudpivot_dev_clean`。

---

## 3. 问题 2（口径）：孤儿应收必须带 `source_type='SALES_OUT'`

`03_orphan_ar_scope.out`（145 `cloudpivot_dev`，含软删行以复现修前现场）：

| 口径 | 行数 |
|---|---|
| ① 卡面 SQL（只有 `source_id IS NOT NULL AND NOT EXISTS stock_out`） | **21** |
| ② 只加 `source_type='SALES_OUT'` | **5** |
| ③ 巡检口径（+ 应收未软删 + 出库单未软删） | 0（5 张已在 I-5 软删） |
| ④ 巡检旧口径（出库单不看 `deleted_at`） | 0 |

①的 21 行按来源拆开：`MANUAL` × 16（`source_no='MANUAL'`，`source_id` 是挂账占位 UUID）+
`SALES_OUT` × 5（`OUT-20261109-000007..11`，出库单物理不存在 —— 真孤儿，已在 I-5 软删）。
落地：

- 巡检 `erp_invariants.py` 的 `S6b` 补 `o.deleted_at IS NULL`（与 `acceptance_gate.sh` 第 ② 条同口径），
  并在代码里写明「口径不许省 `source_type='SALES_OUT'`」及 21 vs 5 的原因；
- `docs/运维规程-演练库与验收基线.md` 新增 **§8 附录：两条口径的标准 SQL（别手写）**
  （孤儿应收 + 库存 vs 流水），把踩过的坑写成可直接抄的 SQL。

---

## 4. 改动文件

| 文件 | 改了什么 |
|---|---|
| `backend/src/business/shared/doc_kind.rs` | 新增 `TxnBizType::SalesReturnIn` + `ALL`/`as_str`/`label`/`doc_kind()`；`TXN_BIZ_TYPE_VALUES` 22→23；注明 `SALES_RETURN_IN` 不进 `VOUCHER_RULE_BIZ_TYPE_VALUES` 的理由 |
| `frontend/src/api/business/erp/doc-kinds.ts` | `TXN_BIZ_TYPES` + `TXN_BIZ_TYPE_LABELS` 同步（Rust 镜像单测盯着） |
| `scripts/verify/erp_invariants.py` | `E4_BIZ_TYPE_AXIS` 增 `"SALES_RETURN_IN": "DOC"`（含轴判定依据）；E4 头部计数 22→23；S6b 补 `deleted_at` + 孤儿应收口径注释 |
| `scripts/verify/regression/erp_invariants_e4_regression.py` | 闸①重做（内容探测 + 三层断言 + 前端镜像 + 2 条负样本自检）；新增**闸⑥**（SALES_RETURN_IN 的 DOC 轴判据牙齿） |
| `docs/运维规程-演练库与验收基线.md` | §8 附录：孤儿应收 / 库存 vs 流水的标准 SQL 与实测数字 |
| `scripts/verify/biz_type_domain.py` | **未改**（它从 `doc_kind.rs` 解析域，词表补上后自动跟着绿） |

## 5. 复跑

```bash
# ① 静态闸（不需要数据库，可进 pre-commit）
python3 scripts/verify/regression/erp_invariants_e4_regression.py --static-only

# ② E4 全量回归（只许打隔离库；脚本自己拒生产库名）
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cloudpivot_e4reg_t9c \
  python3 scripts/verify/regression/erp_invariants_e4_regression.py

# ③ 145 演练库巡检（E4b 应绿；余红是历史夹具）
ssh root@172.18.10.145 'cd /opt/cloudpivot && . /etc/cloudpivot/cloudpivot-dev.env \
  && NIK_DATABASE_NAME=cloudpivot_dev python3 scripts/verify/erp_invariants.py'

# ④ 干净基线（必须全绿）
ssh root@172.18.10.145 'cd /opt/cloudpivot && bash scripts/ops/acceptance_gate.sh'

# ⑤ 本卡两张反证脚本
python3 docs/evidence/20261109-e4b-registry-sales-return/05_gate_before_after.py
NIK_DATABASE_NAME=cloudpivot_e4reg_t9c python3 \
  docs/evidence/20261109-e4b-registry-sales-return/07_axis_net_vs_doc.py
```

隔离库怎么造（145 或本机均可，1 分钟）：

```bash
# 从干净库拷贝（schema 已迁移到最新 + 只有种子数据）
su postgres -s /bin/sh -c "createdb -T cloudpivot_dev_clean -O cloudpivot cloudpivot_e4reg_t9c"
```

## 6. 顺带发现（不在本卡范围，留给后续卡）

`python3 scripts/verify/biz_type_domain.py` 在 `cloudpivot_dev` 上仍有 3 列域外值
（`13_dev_biz_type_domain.out`），与本卡无关但值得单独立卡：

- ② 审批动作：`biz_flow_def.biz_type` / `biz_flow_instance.biz_type` 里的
  `PR_APPROVE` / `SO_APPROVE` / `PRT_APPROVE` 不在 `FlowBizType` 词表里；
- ⑤ 凭证来源：`biz_voucher.source_type = 'FX_REVALUATION'` 不在 `VoucherSourceType` 词表里。

这两处与本次 `SALES_RETURN_IN` 是**同一类**缺口（写入点 → 词表 → 登记表 三层链上缺一环），
本次的三层闸只覆盖库存动账轴（`biz_inventory_txn.biz_type`），没覆盖 flow / voucher-source 轴。
