# t_45bda7ad · E2E-08 夹具清理漏删 `biz_inventory_location`（每跑一次残留 1 行 qty=3 孤儿行）

> 目标库：145 `cloudpivot_dev`（**144 生产全程未碰**）
> 根因文件：`scripts/verify/e2e08_master_common.sh` 的 `cleanup_own()`
> 存量清账：`scripts/datafix/p17/`
> 验收脚本：`scripts/verify/e2e08_{reset,cleanup}.sh`（新增 `assert_clean()` 收尾门禁）

## 0. 一句话结论

11-09 12:00–12:07 那 5 行 `qty=3 / location_id IS NULL` 的孤儿库位账，来源是 **E2E-08 主数据域演练**
（`e2e08_master_b.sh` ⑥ 用 `POST /business/erp/serial/batch-generate` 生成 3 个序列号 → 落一行库位账），
收尾的 `cleanup_own()` 删了「序列号 / 流水 / 成本账 / 物料 / 仓库」**却没删库位账**，
而且也没删 batch-generate 写出的 `source_type='SERIAL_BATCH'` 凭证与其状态日志。

已修：清场顺序补齐为「状态日志 → 凭证分录 → 凭证 → 序列号 → 流水 → **成本账 → 库位账** → 物料/仓库」，
并加 `assert_clean()` 收尾门禁（本卡四张账全 0 + 全库无孤儿库位账）。
修复后在 145 上连跑 3 次 E2E-08(b)：**每轮 257 条自检断言 0 FAIL，库位账总数 22 → 22 不变，孤儿行 0**。

## 1. 定位过程（每一步都是实测，不是推测）

| 步骤 | 做法 | 结果 |
|---|---|---|
| ① 只看残留行本身 | 5 行 id 是 uuid v7、`location_id IS NULL`、qty=3 | 说明是**真实 HTTP 流程**建的，不是确定性夹具 id |
| ② 用其它表反查 | 这 5 行的 `created_at`（12:00:17 / 12:02:32 / 12:05:20 / 12:05:29 / 12:07:08）在 `biz_voucher` 里各命中一张 `source_type='SERIAL_BATCH'` 的凭证 `PZ-20261109-00093{4..8}`，`source_no=SN-20261109-00213{5,8}…` | 业务流是 **batch-generate（无来源入库）**，每张 30.00 元、分录 1405/1901 各一行 |
| ③ 找出调用方 | 全仓 `batch-generate` 调用点里，只有 `w2c1_invariant.sh` / `w2c2_audit.sh` / `t421_batch_generate_e2e.py` / `e2e08_master_b.sh` 用 count=3；前两者跑**隔离库**（每次 dropdb 重建），t421 用 5 件且清场含库位账 | 只剩 `e2e08_master_b.sh` ⑥ |
| ④ 现场指纹 | 145 的 `/tmp/e2e08/` 目录创建于 **12:02**、`asserts.log`/`raw/` 最后写入 **12:07** | 与 5 次运行的时刻完全吻合 |
| ⑤ 代码对账 | `git show 42550f10:scripts/verify/e2e08_master_common.sh` 里 `biz_inventory_location` 出现 **0 次**，`cleanup_own()` 结尾是「删 `biz_inventory` → 删 `biz_material` → 删 `biz_warehouse`」 | 根因坐实：**删主数据不删库位账**（且 `biz_inventory` 先删、库位账根本不删） |

## 2. RED 复现（旧代码，可重复）

用 `42550f10` 版的 `e2e08_master_common.sh` 跑一次 `e2e08_master_b.sh` + 旧 `cleanup_own`：

```
① 跑前：                 location_total=22 orphan_loc=0 serial_batch_vouchers=1
② 旧版 b 脚本跑完：       生成 3 个序列号（HTTP 200）
③ 旧版 cleanup_own 跑完： 已清理本卡前缀（E2E08）的残留演练行
④ RED：                  location_total=23 orphan_loc=1 serial_batch_vouchers=2
   01a1bf34-a6d1-75a8-aca0-b721aaf98850 | material 01a1bf34-a645-… | warehouse 01a1bf34-a5dc-… | 3.0000 | 15:27:41
⑤ 新版 assert_clean：     ❌ 全库 孤儿库位账（material 不存在）= 1（期望 0）
                         ❌ 全库 孤儿库位账（warehouse 不存在）= 1（期望 0）   → exit 1
```

⇒ ①「每次运行漏一行」复现成功；② 新断言能把这个状态判红（不是摆设）。
原始输出：`raw-145/red_proof.txt`。

## 3. 修复（只动夹具清理，不动业务写入口）

`scripts/verify/e2e08_master_common.sh::cleanup_own()`

1. 清场前先记下「本次要删的 物料 / 仓库 / 批次 id」（`/tmp/e2e08/last_cleanup_ids.sh`）——
   删完就查不到，收尾断言要用它逐表复核。
2. 清场顺序（硬外键决定）：
   ```
   状态日志(biz_status_log, VOUCHER) → 凭证分录 → 凭证(SERIAL_BATCH)
   → 价格挂靠 → 序列号(先解引用 location_id) → 流水
   → 成本账 biz_inventory → 库位账 biz_inventory_location → 物料 → 仓库 → 其余主数据
   ```
   - 库位账按 `material_id IN (本卡物料) OR warehouse_id IN (本卡仓库)` 删，**仓库维度顺带收敛历史漏删行**；
   - `biz_inventory.location_id → biz_inventory_location.id` 是硬外键，成本账必须先删（反过来 23503）；
   - 批次凭证用「本卡物料流水里的 `doc_id`（批次 id）」定位 ⇒ 夹具身份是从物料推出来的，不会误删别人的真凭证。
3. 新增 `assert_clean()`（`e2e08_reset.sh` / `e2e08_cleanup.sh` 都调用，有 FAIL 就 exit 1）：
   - 本卡 (物料,仓库) 四张账 = 0（流水 / 成本账 / 库位账 / 序列号）
   - 本卡批次凭证、凭证分录、VOUCHER 状态日志 = 0
   - **全库形态判据**：`biz_inventory_location` 反查不到物料 / 反查不到仓库的行必须为 0
     （与 `scripts/datafix/p16/10_report.up.sql` ⑤、`erp_check` 口径一致，不写死物料编码）
   - 「本次运行前后库位账总数不变」：`e2e08_reset.sh` 记干净基线，`assert_clean` 比对；
     dev 库是共享的，差额只作提示（并发写库会造假差额），硬门禁是上面的孤儿行判据。

`scripts/verify/e2e08_reset.sh`：清场后写 `/tmp/e2e08/location_baseline.txt` 并跑 `assert_clean`。
`scripts/verify/e2e08_cleanup.sh`：`cleanup_own` 后跑 `assert_clean`，FAIL 即非 0 退出。

## 4. 验收：145 上连跑 3 次（真实 HTTP，非 mock）

> 代码状态：145 工作树 `git log -1` = `8e16457b`（含本卡提交 `72444dc0`），
> 清场脚本与提交逐字节一致（`git hash-object scripts/verify/e2e08_master_common.sh` = `b0bd230e…`）。

命令：`bash scripts/verify/e2e08_reset.sh` → 三轮 `e2e08_master_b.sh` + `e2e08_cleanup.sh`（原始输出 `raw-145/after_3runs.txt`）

| 轮次 | b 脚本断言 | 清场前 | 清场自检 | 清场后 |
|---|---|---|---|---|
| 1 | 71 PASS / 0 FAIL | location 23 / orphan 0 / 批次凭证 2 | 93 PASS / **0 FAIL** | **location 22 / orphan 0 / 批次凭证 1** |
| 2 | 71 PASS / 0 FAIL | location 23 / orphan 0 / 批次凭证 2 | 175 PASS / **0 FAIL**（累计） | **location 22 / orphan 0 / 批次凭证 1** |
| 3 | 71 PASS / 0 FAIL | location 23 / orphan 0 / 批次凭证 2 | 257 PASS / **0 FAIL**（累计） | **location 22 / orphan 0 / 批次凭证 1** |

- 每轮清场后 `location_total` 都回到 **22**（= reset 记下的干净基线），`serial_batch_vouchers` 回到 **1**
  （只剩那条**有流水**的存活批次 `PZ-20261109-000501`，脚本按「有流水就不动」保留）。
- 自检逐项：流水/成本账/库位账/序列号/批次凭证/分录/状态日志 = 0；全库孤儿库位账/成本账/流水 = 0；
  「本次运行前后库位账总数不变（22 行）」。

对账基线（`raw-145/after_e2e03_reconcile.txt`，`scripts/verify/e2e03_reconcile.sql`）：

```
①-1 全账 1403 = 1403  diff 0    ①-2 全库差异行 0
②-2 全库逐桶差异行 0             ④ 流水自洽 0    ⑤ 一物一码 0    ⑧ 已过账单据缺流水 0
```

## 5. 存量清账（`scripts/datafix/p17/`）

根因修好只保证「以后不再长」，库里那批存量（含本次 RED 复现造出的 1 行）另用 p17 清：

```bash
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --check    # 只读盘点
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --backup   # 整表 dump + 逐行 jsonb
P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --apply    # 清理（G1/G2/G3 门禁）+ 验收
```

判据（形态判据，不写死编码/凭证号）：

- ① `biz_inventory_location`：`material_id` 或 `warehouse_id` 在 `biz_material` / `biz_warehouse` 里不存在；
- ② `biz_voucher`：`source_type='SERIAL_BATCH'` 且它的批次流水（`txn.doc_id = voucher.source_id`）已不存在
  ⇒ 连带 `biz_voucher_entry`、`biz_status_log(VOUCHER)`。

门禁：G1 不得有红冲/反冲关系；G2 不得被任何业务表 `*_voucher_id` 引用（动态扫 `information_schema`）；
G3 分录科目只能是 1405/1901、库位账不得被成本账/序列号/调拨明细引用。任一命中整单回滚。
每一行被删的原始数据写进 `datafix_p17_doomed`（jsonb），可回滚复查。

实测（`raw-145/p17_after.txt`）：

| 对象 | 删除行数 | 说明 |
|---|---|---|
| `biz_inventory_location` | 2 | 1 行 12:00–12:07 的历史孤儿 + 1 行本次 RED 复现 |
| `biz_voucher` | 7 | 6 张历史无主批次凭证（`PZ-…-00093{4..8}` + 1 张 RED 复现）——原来这个泄漏每跑一次 +30 元 1405 借方 |
| `biz_voucher_entry` | 14 | 每张 2 行（1405/1901） |
| `biz_status_log` | 7 | `entity_type='VOUCHER'` 的状态日志（无外键，必须先删） |

- 1405 余额（POSTED / 未软删 / 非红冲）：**530628 → 530448**，差额 −180 = 6 张 × 30.00，与删除明细逐分对得上。
- 幂等：`--apply` 复跑所有删除 **0 行**，验收仍全绿。
- 保留：`PZ-20261109-000501`（`SN-…-002131`，150.00）**批次流水还在**，不是孤儿，脚本不动。

## 6. 影响与遗留

- `scripts/verify/e2e03_reconcile.sql` ② 逐桶对账从此可稳定维持 0；本次清账后 3 轮跑完仍是 0。
- **遗留（不在本卡范围）**：全库 `biz_status_log` 里 `entity_type='VOUCHER'` 指向已删凭证的行仍有 **123 条**，
  是别的卡的历史夹具；p17 只处理本次两类形态，不整库扫（避免与并行卡互删）。
- `erp_invariants.py` 的 B2（库存金额 = 1405 余额）仍红：本次只清了 180 元无主凭证，
  1405 余额 530448 vs 库存成本账 28811 的差额属 E2E-FIX-7 已记录的遗留口径，不在本卡处理。
- 144 生产未碰；未重启 8033 服务（只改脚本与数据，未换二进制）。

## 7. 复跑

```bash
# 145 上，仓库根目录
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
bash scripts/verify/e2e08_reset.sh                 # 清场 + 记基线 + 自检
bash scripts/verify/e2e08_master_b.sh               # 真实 HTTP 演练（批量生成 3 件）
bash scripts/verify/e2e08_cleanup.sh                # 清场 + 收尾门禁（FAIL 即非 0 退出）
psql -d "$NIK_DATABASE_NAME" -f scripts/verify/e2e03_reconcile.sql | grep -A3 '②-2'
```
