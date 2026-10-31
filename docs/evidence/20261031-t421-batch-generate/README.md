# t_421d8aa1 · batch-generate 改判「无来源入库」—— 交付与实测证据

- 卡：`t_421d8aa1`（【后端】batch-generate 改判「无来源入库」：必录 unit_cost + 成本账/库位账两本账 + 流水写实成本）
- 依据：`docs/adr-20261031-erp-serial-batch-generate-semantics.md`（总管拍板 + §6 R1/R2）
  + `docs/技术方案-20261031-批量生成无来源入库.md`（规格 v3，架构师 `t_1daca7f4`）
- 日期：2026-10-31
- 运行环境：`ubuntu-svr`（172.18.10.99，4 核 / 15G，PG 18.6 本机）

本文件里的每一段输出都是**本机真跑**的原文（命令见每节标题下的 `$` 行）。
可复跑的脚本都进了仓库：`scripts/verify/t421_batch_generate_e2e.py`、`scripts/deploy/v0160-restart.sh`。

---

## 1. 改了什么（与规格 §3.9 的逐函数清单一致）

| # | 文件 | 改动 |
|---|---|---|
| 1 | `backend/src/business/erp/serial_dto.rs` | `BatchGenerateReq` 加 `unit_cost: Option<OptionalDecimal>`（`untagged`，number / string 都收）+ `parse_unit_cost()`（必填 / >0 / <1e11，报文逐字照规格 §2.3）；`SerialBatchRes` 加 `unit_cost / amount / inventory` |
| 2 | `backend/src/business/erp/serial_model.rs` | `batch_generate()` 改「取号 → 落号（断言行数=N）→ `apply_cost` → `apply_location_qty` → 凭证 → 一条流水」；`post_serial_txn` 改 `INSERT … VALUES` 并写实单价/金额；`recompute_quantity` 改**按桶**版（成本账 + 库位账一起对齐）；`transition_status_with` 的出库/报废走 `post_outbound_movement`（快照 → 按桶重算 → 流水 → 结转凭证）；新增 `read_warehouse_snapshot` / `lock_warehouse_snapshot` / `warehouse_snapshot` |
| 3 | `backend/src/business/erp/voucher.rs` | 新增 `SerialMovement` + `create_for_serial_movement()`（**唯一写入口**：生成 借1405/贷1901、出库 借6401/贷1405、报废 借1901/贷1405，金额取调用方传入的账上真实差额；差额 ≤0 跳过 + WARN） |
| 4 | `backend/src/business/erp/serial_handler.rs` | `generate_serials` 传 `unit_cost`、响应体回显单价/金额/快照；单测（T1–T20 除 T19）+ 3 处既有用例适配 |
| 5 | `backend/migrations/business/V0160__erp_serial_init_accounts.{up,down}.sql` | 种 `1901 待处理财产损溢` / `6401 主营业务成本`（0 DDL），带自检；down 有分录引用就 no-op |
| 6 | `scripts/verify/t421_batch_generate_e2e.py`（新） | 端到端验收脚本（83 条断言，见 §4） |
| 7 | `scripts/deploy/v0160-restart.sh`（新） | 生产发布脚本（体检 → 换二进制 → 重启 → 双验 → 失败自动回滚）；本卡**不执行** |

**「只允许一处加数量」的落点**：生成侧只调 `stockin_model::apply_cost`（成本账）与
`stockin_model::apply_location_qty`（库位账），**不再调** `recompute_quantity`；
出库/报废侧只调 `recompute_quantity`（按桶绝对值重算）。两条路径没有一次加法 + 一次重算的叠加。

---

## 2. 单测：`cargo test --lib`（真库，隔离库 `cloudpivot_t421`）

库的准备（每次都是新建后由实例自动迁移到 V0160）：

```text
$ su - postgres -c "createdb -O e2erole cloudpivot_t421"
$ NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_t421 … ./target/debug/cloudpivot
  INFO nik_web::app: 数据库迁移完成: 应用 115 个迁移
max(version)=160
科目 1901/6401: 1901,6401
```

跑用例（提交前的最终树 = `origin/main` `66e5ac8` + 本卡改动）：

```text
$ ERP_DB_URL='postgres://e2erole:…@127.0.0.1:5432/cloudpivot_t421' \
  cargo test -j 4 -- --test-threads=1
test result: ok. 1373 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 302.09s   # --lib
test result: ok. 1 passed;  0 failed; …                                                              # ×13 集成测试二进制
test result: ok. 11 passed; 0 failed; …
test result: ok. 8 passed;  0 failed; …
test result: ok. 5 passed;  0 failed; …
test result: ok. 7 passed;  0 failed; …
test result: ok. 1 passed; 0 failed; 8 ignored; …                                                     # Doc-tests
EXIT=0
```

> `cargo test`（**全部 target**，含 13 个 `tests/*.rs` 集成二进制 + doctests）全绿：
> lib 1373 passed / 0 failed，其余 target 逐个 `ok`，末行 `EXIT=0`。
> 同一套用例在**已经跑过一遍的库**上再跑一遍仍然全绿 ⇒ 夹具/断言都不依赖「干净库」
> （流水条数这类断言一律用增量，不用绝对值）。

新增用例覆盖（规格 §9.2 的 T1–T20，`T19` 见 §6 偏差说明）：

| 用例 | 覆盖 |
|---|---|
| `t1_missing_unit_cost_rejects_with_zero_writes` | 缺 `unit_cost` ⇒ 400/10001 + 逐字报文 + **六表 + 号段零变化** |
| `t3_invalid_unit_cost_values_reject` | `0` / `-1.5` / `"abc"` / `1e11` 四种非法值 400 + 逐字报文 + 零落库 |
| `t4_to_t7_and_t14_single_generate_writes_两本账_and_voucher` | 5 件 @10：成本账 5/50/10、库位账 5、流水 IN+SERIAL_IN+SERIAL_BATCH 写实、凭证 借1405/贷1901、响应体回显、B2 增量 0 |
| `t8_second_generate_uses_moving_weighted_average` | 规格 §8.3 步骤 1–3：5@10 → 出 1 → 5@13 ⇒ 9/105/11.666667 |
| `t9_outbound_converts_cost_and_voucher` | 出库 1 件 ⇒ 4/40/10、流水 OUT 实值、凭证 借6401/贷1405、B2 增量 0 |
| `t10_scrap_converts_cost_by_difference` | 规格 §8.3 步骤 4：报废 1 件 ⇒ 8/93.3333，凭证 11.6667（**差额**口径） |
| `t11_t12_t20_mixed_buckets_keep_b1_and_one_txn_per_movement` | 混桶（NULL 桶 + 库位桶）：两桶各自正确、界①/B1 恒等、每次变动恰一条流水、桶出空归零 |
| `t15_missing_account_rolls_back_whole_batch` | 停用 `1901` ⇒ 500 且**零落库**（号/两本账/流水/凭证全回滚） |
| `t16_legacy_zero_cost_outbound_skips_voucher` | 改判前的零成本存量出库：成功、流水金额 0、**不发凭证**（§8.2） |
| `t17_t18_existing_validations_do_not_regress` | 非一物一码 400/66007、缺仓库 400/66005 不回归 |

---

## 3. `cargo clippy`：不新增告警

```text
$ cargo clippy --all-targets -j 4 -- -D warnings
error: manual `!RangeInclusive::contains` implementation
   --> src/business/impexp/definition.rs:493:31
error: manual `!RangeInclusive::contains` implementation
    --> src/business/task/handler.rs:2987:38
error: manual `!RangeInclusive::contains` implementation
   --> src/business/task/template.rs:398:35
error: could not compile `cloudpivot` (lib) due to 3 previous errors
```

这 3 条**都是既存债**，不是我改出来的（三行原文在 `HEAD` 里就有，`git diff` 也没碰这两个模块）：

```text
$ git show HEAD:backend/src/business/impexp/definition.rs | sed -n '493p'
    if !serial.is_finite() || serial < 1.0 || serial > 2_958_465.0 {
$ git show HEAD:backend/src/business/task/handler.rs | sed -n '2987p'
            if !hours.is_finite() || hours < 0.0 || hours > 9_999.9 {
$ git show HEAD:backend/src/business/task/template.rs | sed -n '398p'
        && (!hours.is_finite() || hours < 0.0 || hours > 9999.0)
$ git diff --name-only
backend/src/business/erp/serial_dto.rs
backend/src/business/erp/serial_handler.rs
backend/src/business/erp/serial_model.rs
backend/src/business/erp/voucher.rs
```

把这三条既存 lint 放行后，**我的改动零告警**：

```text
$ cargo clippy --all-targets -j 4 -- -D warnings -A clippy::manual_range_contains
    Checking cloudpivot v0.1.0 (/root/p10-cp-131816/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.93s
```

（本轮我自己被 clippy 抓出一条 `useless use of format!`（`serial_model.rs:790`，无占位符却用了 `format!`），
已改成静态 SQL + `.to_string()`。）

`cargo fmt -- --check` 干净（`cargo fmt` 已跑，输出为空，exit 0）。

---

## 4. 隔离实例端到端实测（`scripts/verify/e2e_isolated_instance.sh`）

> 下面两组都是**最终树**（`origin/main 66e5ac8` + 本卡改动，含上游刚合入的调拨补库位账）
> 上用 `target/debug/cloudpivot` 复跑的结果。原始日志：
> `/tmp/t421_e2e_final_prodcopy.log`（82 条断言）与 `/tmp/t421_e2e_final_clean.log`（83 条）。

### 4.1 从 prod 复制副本库（端口 18090 / 5190）

```text
$ E2E_ISO_DB=cloudpivot_e2e_t421 E2E_ISO_BACKEND_PORT=18090 E2E_ISO_FRONTEND_PORT=5190 \
  E2E_ISO_FRESH=1 E2E_ISO_SRC=ssh://root@172.18.10.144 \
  E2E_ISO_BACKEND_BIN=…/target/debug/cloudpivot bash scripts/verify/e2e_isolated_instance.sh up
✅ 端口判据：后端 18090 / 前端 5190 都在隔离段且空闲
… 从 root@172.18.10.144 的 prod 库 pg_dump（只读）→ 本地 cloudpivot_e2e_t421
✅ 副本库就绪：sys_user=25 行（源库一份拷贝，之后的造数只落这里）
✅ 后端就绪：http://127.0.0.1:18090（profile=test，库 cloudpivot_e2e_t421）
✅ 前端就绪：http://127.0.0.1:5190（/api 反代 → 127.0.0.1:18090，探测码 401）
```

> 18089/5189 当时被并行的前端卡实例占着，所以本卡用同一隔离段的 18090/5190
> （脚本的端口判据与 `frontend/e2e/target-guard.mjs` 都只认 18000–18999 / 5100–5999）。
> **prod 全程只读**：只做了 `pg_dump`，没有任何写操作。

后端启动日志（V0160 由框架自动应用，prod 副本从 159 → 160）：

```text
INFO nik_web::app: 数据库迁移完成: 应用 1 个迁移
max(version)=160
```

**生成前**的巡检基线（prod 副本自带 2 条历史 `STOCK_TRANSFER` 流水，见 §6 偏差 3）：

```text
$ NIK_DATABASE_NAME=cloudpivot_e2e_t421 python3 scripts/verify/erp_invariants.py
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

### 4.2 端到端跑一次（带单价生成 → 二次生成 → 出库 → 报废 → 巡检）

```text
$ T421_BASE=http://127.0.0.1:18090 T421_DB=cloudpivot_e2e_t421 T421_DB_USER=e2erole \
  T421_DB_PW=… T421_ADMIN_PW=… python3 scripts/verify/t421_batch_generate_e2e.py
```

关键原文（完整 82 条断言见 `/tmp/t421_e2e_run2.log`，脚本在仓库里可复跑）：

```text
==> 1. 缺 unit_cost ⇒ 400 + 10001 + 逐字报文，且零落库
  [PASS] HTTP 400（400）
  [PASS] 业务码 10001（10001）
  [PASS] 报文逐字（规格 §2.3）（unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税））
  [PASS] 零落库：号段 未变（15）        [PASS] 零落库：序列号 未变（21）
  [PASS] 零落库：成本账 未变（2）       [PASS] 零落库：库位账 未变（2）
  [PASS] 零落库：流水 未变（11）        [PASS] 零落库：凭证 未变（9）
  [PASS] 零落库：分录 未变（25）        [PASS] 号段未推进（validate 在 begin 之前）（15）

==> 2. 非法单价：0 / "abc" / 1e11 都 400 + 逐字报文
  [PASS] 报文逐字（unit_cost=0）（unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收）
  [PASS] 报文逐字（unit_cost=abc）（unit_cost 不是合法数字：「abc」）
  [PASS] 报文逐字（unit_cost=100000000000）（unit_cost 超出可记账范围（须 < 100000000000）：100000000000）

==> 3. 生成 5 件 @12.34（＝无来源入库：两本账 + 凭证 + 写实流水）
  [PASS] 响应 unit_cost（6 位字符串）（12.34）      [PASS] 响应 amount = 5 × 12.34（61.7000）
  [PASS] 响应 inventory.qty（5.0000）  [PASS] 响应 inventory.amount（61.7000）  [PASS] 响应 inventory.avg_cost（12.340000）
【② 生成 5 件 @12.34 后】成本账qty=5.0000  成本账amount=61.7000  avg_cost=12.340000  库位账合计=5.0000  流水条数=1  在库号=5  序列号凭证=1
  [PASS] 流水 txn_type（IN）   [PASS] 流水 biz_type（SERIAL_IN）   [PASS] 流水 doc_type（SERIAL_BATCH）
  [PASS] 流水 unit_cost 写实 12.340000（原为 0）   [PASS] 流水 amount 写实 61.7000（原为 0）
  [PASS] 流水 remark 带单价与金额（批量生成 5 件（SN-20261031-000011 起）｜单价 12.340000｜金额 61.7000）
  [PASS] 凭证①科目 1405 库存商品 借方（('1405', '61.7000', '0.0000')）
  [PASS] 凭证②科目 1901 待处理财产损溢 贷方（('1901', '0.0000', '61.7000')）

==> 4. 二次生成 4 件 @15.00 ⇒ 移动加权 avg = round((61.7+60)/9, 6) = 13.522222
【③ 二次生成 4 件 @15 后】成本账qty=9.0000  成本账amount=121.7000  avg_cost=13.522222  库位账合计=9.0000  流水条数=2  在库号=9  序列号凭证=2
  [PASS] avg_cost = ROUND(121.7/9, 6) = 13.522222（13.522222）
  [PASS] 流水条数 = 2（每次生成一条，不随桶数翻倍）（2）

==> 5. 出库 1 件 ⇒ 成本结转（借 6401 / 贷 1405，金额取账上真实差额）
【④ 出库 1 件后】成本账qty=8.0000  成本账amount=108.1778  avg_cost=13.522222  库位账合计=8.0000  流水条数=3  在库号=8  序列号凭证=3
  [PASS] 出库流水 txn_type（OUT）   [PASS] 出库流水 biz_type（SERIAL_OUT）
  [PASS] 出库流水 amount = 账上差额 13.5222（13.5222）
  [PASS] 出库凭证①科目 6401 主营业务成本 借方（('6401', '13.5222')）
  [PASS] 出库凭证②科目 1405 贷方（('1405', '13.5222')）

==> 6. 报废 1 件 ⇒ 报废损失（借 1901 / 贷 1405）
【⑤ 报废 1 件后】成本账qty=7.0000  成本账amount=94.6556  avg_cost=13.522222  库位账合计=7.0000  流水条数=4  在库号=7  序列号凭证=4
  [PASS] 报废凭证①科目 1901 借方（('1901', '13.5222')）
  [PASS] 报废凭证②科目 1405 贷方（('1405', '13.5222')）

==> 7. 不变式① / B1 全程复核（本夹具物料）
  [PASS] 不变式①：在库号 == 成本账 qty（7.0000）
  [PASS] B1：库位账合计 == 成本账 qty（7.0000）

==> 8. 本库全量巡检：erp_invariants.py
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
  [PASS] 巡检汇总 32/32    [PASS] BLOCKER 0    [PASS] WARN 0    [PASS] 巡检退出码 0
  [NOTE] 副本库含 2 条历史 STOCK_TRANSFER 流水（prod 自带，R3 已单卡收口）；本库的 B1 仍绿由上面 32/32 证明

==> 汇总：PASS=82 FAIL=0
```

### 4.3 空库隔离实例（`STOCK_TRANSFER = 0` 那条判据）

卡面要求「副本库 `doc_type='STOCK_TRANSFER'` 行数必须为 0」，而 prod 副本自带 2 条历史调拨流水
（R3，已由 `t_270b472e` 收口）。所以再起一个**空库隔离实例**（`E2E_ISO_SRC_MODE=none`，端口 18092/5192，
库 `cloudpivot_e2e_t421b`，迁移自动跑），同一套脚本 + `T421_EXPECT_NO_TRANSFER=1` 硬判：

```text
$ NIK_ADMIN_PASSWORD='…' E2E_ISO_DB=cloudpivot_e2e_t421b E2E_ISO_BACKEND_PORT=18092 \
  E2E_ISO_FRONTEND_PORT=5192 E2E_ISO_SRC_MODE=none … bash scripts/verify/e2e_isolated_instance.sh up
✅ 后端就绪：http://127.0.0.1:18092（profile=test，库 cloudpivot_e2e_t421b）

$ T421_BASE=http://127.0.0.1:18092 T421_DB=cloudpivot_e2e_t421b … T421_EXPECT_NO_TRANSFER=1 \
  python3 scripts/verify/t421_batch_generate_e2e.py
  首登强制改密 → 口令已更新（R2 用新口令重登）
【② 生成 5 件 @12.34 后】成本账qty=5.0000  成本账amount=61.7000  avg_cost=12.340000  库位账合计=5.0000  流水条数=1  在库号=5  序列号凭证=1
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
  [PASS] 副本库 STOCK_TRANSFER 流水行数 = 0（否则 B1 的绿不可信，规格 §11 R3）（0）
==> 汇总：PASS=83 FAIL=0
```

---

## 5. 与规格的偏差（3 处，全部有理由）

1. **`recompute_quantity` 的「记账断链」判据**：规格 §5.2 ③ 写的是「重算后任何
   `qty > 0 且 avg_cost = 0` 的行 ⇒ 报错」。照字面实现会把**改判前的历史零成本存量**
   （§8.2 明确「不补账、必须能正常出库」，单测 T16）一起判死。落地改成
   「**有在库件数但成本账没有对应桶行** ⇒ 报错回滚」，语义是同一件事（不许凭空造 `avg_cost=0` 的行），
   但不误伤存量。
2. **`recompute_quantity` 的实现形态**：规格 §5.2 给的是 `INSERT … ON CONFLICT` upsert；
   实跑发现它漏「桶里最后一件也走了」的情形（该桶不再出现在 `GROUP BY` 结果里 ⇒ 成本账保住旧 qty
   而库位账已是 0 ⇒ B1 红）。改成「以两本账**自身行**驱动对齐（含显式归零）」+ 一条孤儿桶检查，
   语义等价且覆盖两种边界（T11 的「桶出空后归零」用例就是钉这个）。
   两种形态的 SQL 都在真库跑过，最终形态见 `serial_model.rs::recompute_quantity` 的文档注释。
3. **`T19`（落号条数不符注入）没有单测**：要造「插入 ≠ N」必须让 `biz_material_serial` 的
   INSERT 静默少插（唯一键冲突时 `INSERT … SELECT` 会整条失败而不是少插，除非改 SQL）。
   断言本身在代码里（`inserted != count` ⇒ `InvalidValue` 整单回滚），e2e 的「400 零落库」与
   T15 的「凭证写失败整单回滚」证明回滚路径有效，但没有直接注入点。

---

## 6. 不在本卡范围 / 待总管决定

- **生产部署**：本卡只写脚本 `scripts/deploy/v0160-restart.sh`，**没有在 prod 上执行**
  （prod 迁移仍停在 159、`biz_account` 仍只有 5 个科目 —— 见 §4.1 的只读体检）。需要总管定发布窗口，
  且**前端必须同一次发布**（表单必填 `unit_cost`，老前端提交会 400）。
- **调拨结算不写库位账 ⇒ B1 必红**（R3）：已由 `t_270b472e` 收口，不在本卡。
- **存量不补账**：改判前生成的零成本号出库/报废时差额为 0 ⇒ 只写流水不发凭证（WARN 点名），
  要彻底消掉这类数据走报废/盘点单，不跑数据修复脚本。
