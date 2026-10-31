# P11-followup · prod 不变量 B1/C3 复红（M-W1A-004 夹具第二次复发）处置留痕

- 日期：2026-10-31（12:14 建卡 → 12:2x 收口）
- 卡片：`t_13ed5d2c`（P11 验收时发现，与 P11 代码无关）
- 涉及库：prod `cloudpivot` @ 172.18.10.144:5432（**未做任何「删别人数据」的清理**，见 §3）
- 相关前序：`docs/datafix-20261031-p10.md`（P10 第一次清 M-W1A-004，10:54）

---

## 0. 结论（先看这三条）

1. **11:34:11 的复发源头是「有人直连 prod 点了批量生成」**，不是仓库里的隔离实例脚本：
   prod 日志显示 `POST /api/business/erp/serial/batch-generate` 由客户端 **172.18.10.7** 在
   11:34:11 打成 200，前后 10 秒内同一 IP 还在翻「物料/序列号/仓库/调拨」页并建了一张调拨单、
   批了一张审批单 —— 是一次**跑在 8023 上的 UI/E2E 走查**（详见 §2）。
2. **这行数据在 12:12:58 已经被清掉**（另一次会话按 P10 流程做的：`00_backup.sh` 12:12:57 →
   `apply.sh` 12:12:58）。本卡核对属实，并补齐了它缺的部分：**本卡自己的备份**、**幂等复跑证据**、
   **32/32 复验**、**留痕文档**（原来 `docs/` 下没有任何 12:12 那次清理的记录）。
3. 本卡真正的交付是**判据升级**：P10 的 `10_` 只认物料编码 `M-W1A-004`，换一个物料就看不见。
   新增 `20_residue_report` / `21_residue_clean` 两支**按形态特征**判定（不绑物料编码）+ 一个只读检出脚本
   + 一个注入式演练脚本。演练在 prod 副本库上把这件事完整重放了一遍：**31 条断言全绿**
   （注入 → 30/32 红 → 检出 → 清理 → 32/32 → 第二遍 0 行 → down 精确还原）。

---

## 1. 事实（原始输出，全部贴自真实命令）

### 1.1 12:12 那次清理留下的证据

`/root/datafix-backup-p10-20261031-121257/MANIFEST.txt`：

```
P10 datafix backup
time      : 2026-10-31T12:12:57+08:00
database  : cloudpivot on 127.0.0.1:5432 (user cloudpivot)
created by: scripts/datafix/p10/00_backup.sh
biz_inventory = 3
biz_inventory_location = 2
biz_inventory_txn = 10
biz_material_serial = 21
```

`biz_inventory.sql`（**修复前**那一行，就是卡面点名的形状）：

```
01a19005-a3ab-7464-934a-616bf2caeadf  d1370003-…-000000000004  d1370001-…-000000000001  \N
10.0000  0.0000  0.000000  0.0000   2026-10-31 11:34:11.113291+08   (created/updated 同值)
```

`m_w1a_004_rows.sql`（同一次备份里该物料的流水，说明它是「批量生成」造的）：

```
… | IN | SERIAL_IN | SERIAL_BATCH | doc_no=SN-20261031-000001 | line 1 | qty 10.0000
  | unit_cost 0 | amount 0 | before 0/0/0 | after 10.0000/0/0 | period 2026-10
  | remark=批量生成 10 件（SN-20261031-000001 起） | 2026-10-31 11:34:11.115021+08
```

同备份里 10 条号 `SN-20261031-000001..000010` 均为 `IN_STOCK`、`source_type=PURCHASE`、
**`source_id` 为空**、`deleted_at` 为空 —— 即「修复前」状态。

### 1.2 12:15 复测（本卡开工时的现状）

```
== DB: cloudpivot ==（2026-10-31 12:15:41 CST）
--- 2. biz_inventory 行（该物料）        （空）
--- 3. biz_inventory_location 行（该物料）（空） loc_qty_sum=0
--- 4. 序列号 total=21 alive=0 alive_in_stock=0
   SN-20261031-000001..000010 | IN_STOCK | source_type=PURCHASE | source_id=NULL
                               | created=2026-10-31 11:34:11 | deleted=2026-10-31 12:12:58
--- 5. 流水（该物料）                     （空）
--- 6. stock_in_item=0 purchase_order_item=0
--- 7. transfer_link_rows=5
--- 8. C3：qty>0 且 avg_cost=0            （空）
--- 9. B1：成本账 qty ≠ 库位账合计          （空）
```

⇒ 12:12:58 起该物料已无成本账/库位账/流水，仅剩软删序列号与调拨单链接行（P10 口径：审计痕迹保留）。

### 1.3 复发源头（prod journal，`journalctl -u cloudpivot`）

```
Oct 31 11:34:11 … "method":"POST","path":"/api/business/erp/serial/batch-generate","status":200,
                "duration":"6ms","ip":"Some(\"172.18.10.7\")"
```

同一 IP 在 11:34:00–11:34:20 的请求序列（节选，按时间）：

```
11:34:07 GET  /api/system/badge/counts                     200
11:34:11 POST /api/business/erp/serial/batch-generate      200   ← 就是它
11:34:11 GET  /api/business/erp/serial/page                200
11:34:20 GET  /api/business/erp/warehouse/page             200
11:34:20 GET  /api/business/erp/material/page              200
11:34:20 GET  /api/business/erp/transfer/page              200
```

`172.18.10.7` 自 11:00 起共 237 个请求，形态是**浏览器/Playwright**（`/notice/stream` SSE、
`/auth/preference`、`/auth/refresh`、`/system/auth/login`），且在同一分钟内还打了
`POST /api/business/erp/transfer`、`…/status`、`/api/business/flow/task/{id}/approve`。
⇒ 是**一条跑在 prod（8023）上的 UI 业务闭环走查**，不是 `scripts/verify/` 里的隔离实例
（那些脚本自建 `cloudpivot_*` 库 + 1808x/181xx 端口，`w2b_serial_api.sh` 结尾还断言「prod 8023 仍 200」）。

### 1.4 修后不变量（本卡复验，`scripts/verify/erp_invariants.py`，P11 版 32 条）

```
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
INV_EXIT=0
```

> 注意口径：**共享工作区 `/opt/cloudpivot` 当时停在 `b0bfb8b`（P11 之前）**，那里的脚本还是 29 条
> （P10 版），所以有人在那里跑会看到 `29/29`。32 条（含 H1/H2/H3）来自 bare main `3d05430`，
> 本卡全部验收都在 `/root/t13-src`（bare main 的干净 clone）里跑。

---

## 2. 归因

| 假设 | 判定 | 依据 |
|---|---|---|
| P11 代码引入 | ❌ | P11（`c52dfc3`）只动 `biz_flow_*` / 任务状态机 / 前端，**一行库存 SQL 都没有**（`git show --stat`）；且 11:34 早于 P11 的 11:56 开跑、12:08 动 prod |
| P10 没清干净 | ❌ | P10 交付时该物料库存 0 行 / 在库号 0 条（`docs/datafix-20261031-p10.md` §5 快照）；11:34 是**之后**新造的一批（`SN-20261031-*` 号段） |
| e2e 脚本在隔离实例上跑 | ❌ | 命中请求来自 `172.18.10.7` 打 prod 8023，且同一 IP 是浏览器形态；仓库脚本都自建库、自起 1808x 端口 |
| **有客户端直连 prod 做了 UI 走查** | ✅ | §1.3 的 journal 证据链（登录 → 翻 ERP 页 → 批量生成 → 建调拨 → 批审批）|
| 判据太窄（只认一个物料编码） | ✅ | `10_` 把 `material_code='M-W1A-004'` 写死；下次 e2e 换物料，C3/B1 照样会红但没人会想起来跑它 |

**产品侧的真问题（本次只记录、不在本卡改契约）**：
`POST /business/erp/serial/batch-generate` 的语义是「取号」，但它同时把号落成 `IN_STOCK`
并按「物料×仓库」重算库存数量 —— 而这批货**没有任何成本来源**，于是天然产生
`qty>0 / amount=0 / avg_cost=0` 的库存行：
- B1（库位账合计 = 成本账 qty）会红，因为它不写库位账；
- C3（有库存则成本价非零）一定红，因为成本就是 0。

也就是说：**合法用户在生产上点一次「批量生成」，就会把 ERP 不变量打红一次。**
改法有两条（要么要求录入成本，要么生成的号不直接进「在库」），都会动 W2b 的接口契约与它的
e2e 断言，属于要拍板的变更 —— 见 §6 遗留 ②。

---

## 3. 处置（本次做了什么，没做什么）

### 3.1 没有做的事

- **没有删别人卡的数据**：11:34 那批已经在 12:12:58 被清掉，本卡不再动它；P10 的 `10_` 复跑是
  `序列号软删 0 条 / 流水 0 条 / 库位账 0 行 / 成本账 0 行`（幂等，见 §3.2）。
- **没有动 `biz_task` / 审批实例 / 调拨单**：`TK-*` 单据与 `biz_stock_transfer_item_serial` 链接行保留。
- **没有回拨 `biz_seq` 的 SN 号段**（P10 口径：只前进不回退）。

### 3.2 做了的事（含原始输出）

```bash
# ① 本卡自己的备份（12:12 那份是别人的，不能当本卡的凭据）
bash scripts/datafix/p10/00_backup.sh p11f-prod
→ /root/datafix-backup-p10-p11f-prod（7 张表 + full.dump + MANIFEST）

# ② 改前现状
bash scripts/datafix/p10/apply.sh --check
→ 库存金额 435.0000 / 库存数量 207.0000 / 库位账 207.0000 / 1405 435.0000
  M-W1A-004 库存行数 0 / 在库号数 0

# ③ 应用（10_ 幂等 + 20_ 只读报告 + 21_ 清理；21 默认不跑，这里显式开）
P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh
→ NOTICE: P10: M-W1A-004 夹具残留已清理 —— 序列号软删 0 条 / 流水 0 条 / 库位账 0 行 / 成本账 0 行
→ NOTICE: P10: 保留的夹具单据：调拨明细链接行 5 条（serial_id 现在指向软删号，FK 与审计都在）
→ NOTICE: P10-20: 全库无「批量生成零成本夹具」残留（库存合计未被这类夹具污染）
→ NOTICE: P10-21: 没有需要清理的夹具（跳过 0 个物料），本脚本未改任何数据

# ④ 第二遍（幂等 —— 卡面要求的「第二遍 0 行」）
→ 同 ③ 的两行 NOTICE，三支脚本合计改动 0 行；快照批次表无新增

# ⑤ 不变量 + 检出脚本
python3 scripts/verify/erp_invariants.py     → 32/32, BLOCKER 0, WARN 0, exit 0
python3 scripts/verify/e2e_fixture_residue.py → ✅ 无残留, exit 0
```

---

## 4. 本卡新增的资产（判据升级）

| 文件 | 作用 |
|---|---|
| `scripts/datafix/p10/20_residue_report.up.sql` | **全库扫描**（只读）：把「批量生成零成本夹具」的形态特征写成四道判据，结果写 `datafix_residue_report` 并 RAISE NOTICE（每次运行重写） |
| `scripts/datafix/p10/21_residue_clean.up.sql` | 按报告清单清理；删除前**再复核一遍判据**；整行快照进 `datafix_residue_batch` |
| `scripts/datafix/p10/21_residue_clean.down.sql` | 按最近一批未还原快照**精确写回**（id / 时间戳用快照原值），幂等 |
| `scripts/datafix/p10/apply.sh` | `APPLY_ORDER` 从「只有 10_」改成 `10_ + 20_`；`P10_GENERIC_APPLY=1` 追加 `21_`（并参与回滚）；`--check` 也显示通用报告 |
| `scripts/verify/e2e_fixture_residue.py` | 只读检出（退出码 1 = 有残留），给出「备份 → 看 → 清」三条命令；适合 e2e 跑完的后置检查 / 日巡检 |
| `scripts/verify/p11f_fixture_residue_drill.sh` | 注入式演练：在 **prod 副本库** 上重放 11:34，31 条断言覆盖「红 → 检出 → 清 → 32/32 → 幂等 → 回滚」 |

### 4.1 判据（四道同时成立才动，任何一道不成立就跳过）

1. 该物料至少有 1 条 `biz_inventory_txn` 满足 `biz_type='SERIAL_IN' AND doc_type='SERIAL_BATCH'`
   —— 只有 `serial_model::batch_generate` 写这个组合（`serial_model.rs:802`）；
2. 没有业务来源：`biz_stock_in_item` / `biz_purchase_order_item` 里都没有它；
3. 该物料**存在**序列号，且**全部**是 `source_type='PURCHASE' AND source_id IS NULL`
   （收货过账走 `stockin_model::insert_serials`，必定回填 `source_id`）；
4. 库存**零价值**：该物料所有 `biz_inventory` 行的 `amount` 与 `avg_cost` 都是 0。

反例（**故意不命中**，也是为什么这条分支不会毁账）：

| 场景 | 被哪条挡住 |
|---|---|
| 正常采购收货的物料 | ②（有 `stock_in_item`） |
| 批量生成后又做过收货/调拨、建立了成本 | ④（`amount/avg_cost ≠ 0`） |
| 人工补过号、混有收货号 | ③（不是全部 `source_id IS NULL`） |
| 已经清过一次的物料 | ①（`SERIAL_BATCH` 流水已被删）⇒ 第二遍 0 行 |

### 4.2 演练证据（`scripts/verify/p11f_fixture_residue_drill.sh`，prod 副本库 `cloudpivot_p11f`）

演练特意**不用** `M-W1A-004`，而是新建物料 `M-P11F-GEN` 注入同款形态 —— 直接证明判据不绑物料编码。

```
== 2) 注入 11:34 的同款夹具（批量生成 3 件：无单据、零价值、无库位账）… PASS×4
== 3) 不变量应当转红
      🚨 [账账] B1 库位账合计 = 成本账 qty
      ⚠️ [账实] C3 有库存则成本价非零
    结果：30/32 通过   🚨 BLOCKER 1   ⚠️ WARN 1          ← 与 P11 验收时的 30/32 完全一致
== 4) 检出脚本 exit=1 且点名 M-P11F-GEN … PASS
      ❌ 命中 M-P11F-GEN：库存 1 行 / 数量 3.0000；流水 1 条；存活序列号 3（全部 3 条）
== 5) 通用清理
      NOTICE: P10-20: 命中夹具残留 M-P11F-GEN —— 库存 1 行/数量 3.0000/金额 0.0000；流水 1 条；存活序列号 3 / 全部 3 条
      NOTICE: P10-21: 已清理 M-P11F-GEN —— 序列号软删 3 / 流水 1 / 库位账 0 / 成本账 1
      库存数量合计 210.0000 → 207.0000；库位账 207.0000（两账重新相等）
== 6) 清理后不变量 32/32 通过 BLOCKER 0 WARN 0 … PASS×4
== 7) 幂等：第二遍「全库无残留」「没有需要清理的夹具（0 行）」，快照批次仍 1 批 … PASS×3
== 8) 回滚：down 写回 成本账 1 / 流水 1 / 序列号 3（deleted_at 复原）→ 不变量重新转红（= 修复前）… PASS×6
------------------------------------------------------------------------------
汇总：PASS=31  FAIL=0
★ 演练通过：注入 → 红 → 检出 → 清 → 绿(32/32) → 幂等 → 回滚
```

> 演练期间 prod 全程只读（只 `pg_dump` 出副本），副本库演练完即 `dropdb`。

---

## 5. 复跑方法（给下一个人）

```bash
cd /root/t13-src          # 或任意 bare main 的干净 clone（共享工作区 /opt/cloudpivot 可能落后）
set -a; . /etc/cloudpivot/cloudpivot.env; set +a

python3 scripts/verify/e2e_fixture_residue.py        # ① 先看有没有残留（只读，exit 1 = 有）
bash    scripts/datafix/p10/apply.sh --check         # ② 三账快照
bash    scripts/datafix/p10/00_backup.sh p11f-prod   # ③ 备份（apply 会校验 MANIFEST 的库名）
P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh # ④ 清（快照进 datafix_residue_batch）
python3 scripts/verify/erp_invariants.py             # ⑤ 必须 32/32
# 回滚：P10_GENERIC_APPLY=1 bash scripts/datafix/p10/apply.sh --rollback

# 想先在副本库上验证这套流程（推荐）：
bash scripts/verify/p11f_fixture_residue_drill.sh
```

---

## 6. 遗留（不藏）

1. **源头还在**：`172.18.10.7` 上的 UI/E2E 走查是直接打 prod 8023 的（§1.3）。只要它还这么跑，
   每次都会往生产里写夹具单/序列号/调拨单。已另开卡给测试侧（`t_…`）要求「E2E 不得直连 prod，
   或跑完必须调用 `e2e_fixture_residue.py` + 清理」。本卡的 20/21/检出脚本只能**事后 1 分钟修回来**，
   不能阻止别人往生产写数据 —— 这一条必须测试侧改口径才算闭环。
2. **`batch-generate` 的语义缺陷**（§2 末）：取号即「在库」+ 零成本 ⇒ 合法用户点一次也会打红 C3。
   三条候选：① 请求体加 `unit_cost`（但批量生成不是入库，凭空给成本 = 造账）；
   ② 生成的号落成「未入库」状态、不写成本账（要改 `recompute_quantity` 的天平，W2b e2e 依赖现行为）；
   ③ 至少让前端「批量生成」只出现在隔离环境。需要产品/架构拍板，不在本卡擅自改契约。
3. **`datafix_residue_report` / `datafix_residue_batch` 两张表留在 prod**（只读报告 + 清理快照）。
   它们是数据修复的凭据，不参与业务；如果运维要求「生产不许多余表」，清空后 `DROP` 也可以，
   但那样就丢了回滚依据。
4. **共享工作区 `/opt/cloudpivot` 落后 bare main**（`b0bfb8b` vs `3d05430`，P11 的迁移/脚本不在里面）。
   本卡全程用 `/root/t13-src`，没去动别人未提交的 2 个未跟踪脚本；共享工作区对齐留给运维/下一次发布。
