# CODE-02 供应商 / 客户创建时自动取号（允许手工覆盖）— 交付证据

- 卡：`t_69d21b58`（CODE-02）
- 主提交：`246f68f`（5 文件 `+691 −51`）
- 依赖：CODE-01 的 `biz_seq` 六条主数据号段 + `erp::seq`（`555337e` / `410200c`）
- 验证环境：145（测试环境 `cloudpivot-test.service`，后端 `127.0.0.1:8033`，nginx `:80`）；本机库 `cloudpivot_code01`（`ERP_DB_URL`）
- 证据脚本：`scripts/05_145_curl.sh`、`scripts/06_db_assert_and_cleanup.sh`、`scripts/07_verify_145.sh`

## 一、卡面验收 vs 实测

| # | 卡面验收 | 实测（145，真实 token curl） | 结论 |
|---|---|---|---|
| ① | 传 `supplier_code:""` → `SUP-0001` | HTTP 200 → **`SUP-0002`** | 通过（**偏差见 §三.1**：老库 `SUP-0001` 已被 V0137 种子占用，取号器顺延） |
| ② | 再建一个 → `SUP-0002` | 传 `supplier_code:null` → **`SUP-0003`** | 通过（连续 +1） |
| ③ | 传 `supplier_code:"GYS-001"` → 保留 | HTTP 200 → **`GYS-001`** | 通过（非 `SUP-xxx` 形态也保留） |
| ④ | 传重复编码 → 400 + 明确提示 | HTTP **409** + 业务码 **66001**，`msg=供应商编码 \`SUP-0002\` 已存在（已软删的编码可以复用）` | 通过（**偏差见 §三.2**：仓内重复编码统一 409） |
| ⑤ | 客户同样四条 | `""`→`CUS-0004`；字段缺席→`CUS-0005`；`KH-001`→保留；重复→409/66001 且文案点名编码 | 通过（`CUS-0001..0003` 已被历史数据占用） |
| ⑥ | `cargo test` 全绿 | 本卡相关模块全绿；全量 `--lib` 有 208 条**与本卡无关**的历史/在途红（归因见 §五） | **部分通过，已如实归因** |

额外补的一条（卡面没写但「格式校验」要求的落点）：`supplier_code:"GYS 001"`（含空格）→ **400 + 10001 + 人话文案**「编码里不能出现 ` `：只允许字母、数字与 `-` `_` `.`」。

原始响应：`raw/01_sup_blank.json` … `raw/14_cus_dup.json`，断言汇总 `raw/00_summary.txt`；库侧断言 `raw/07_db_assert_145.txt`。

## 二、改了什么（5 文件）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/seq.rs` | 新增 `MAX_MANUAL_CODE_LEN` / `manual_code` / `validate_manual_code` / `resolve_manual_code`（纯函数，空/空白→`None`，有值→只校验长度+字符集，**不强制 `SUP-xxx` 形态**）+ `master_code_taken(ex, seq_type, code)`（建档案前的唯一性预检，表名/列名来自常量表，不拼 SQL） |
| `backend/src/business/erp/dto.rs` | `CreateSupplierReq.supplier_code`：`String` → **`Option<String>`**（`#[serde(default)]`，`null`/缺省都收）；`validate()` 改成「留空放过，填了才校验」 |
| `backend/src/business/erp/handler.rs` | 供应商建单抽成可单测的 `create_supplier(operator, req)`：手工编码预检 → `409/66001`；留空则 `seq::next_master_code_free(&tx, "SUP")`；**取号与 INSERT 同一事务**；`supplier_write_error` 把唯一索引冲突翻成带编码的 409；新增 `param_invalid` / `seq_error` 两个错误出口 |
| `backend/src/business/erp/customer_handler.rs` | `CreateCustomerReq.code`：`String` → **`Option<String>`**；`create_customer` 同样「预检 → 事务内取号 `CUS` → INSERT」；新增 `customer_write_error` / `seq_error` |
| `backend/src/business/erp/customer_model.rs` | 新增 `customer_insert_ex(ex, …)`（可在调用方事务里插），`customer_insert` 变成它的 `db()` 包装（对外签名不变） |

### 为什么取号必须进事务

`biz_seq` 取号是单语句 `UPDATE … SET current_no = current_no + step RETURNING current_no`（行锁，天然不重号）。
放进 `db().begin()` 的同一事务后：

- 插入失败（唯一索引 / CHECK / 别的原因）→ 整体回滚，**号一起退回**，不留空号；
- 两个并发请求拿到的号必然不同（行锁持有到提交）；
- 手工编码那条路径也在同一事务里插，语义一致。

### 错误口径（三个出口保持仓内惯例）

| 场景 | HTTP | 业务码 | 文案 |
|---|---|---|---|
| 手工编码格式不合法（超长/非法字符） | 400 | 10001 | `supplier_code 不合法：编码里不能出现 …` |
| 手工编码已存在（预检命中或并发撞索引） | 409 | 66001 | `供应商编码 \`XXX\` 已存在（已软删的编码可以复用）` |
| 取号规则缺失 / 连号全被占 | 500 | 10004 | `新建供应商：自动取号失败，请联系管理员检查取号规则`（真实原因只进 `internal_detail`/日志） |

## 三、与卡面的三处偏差（都以「库为准 + 仓内既有惯例」收口）

1. **首个自动号不是 `SUP-0001` / `CUS-0001`**：145 库里 `SUP-0001` 是 V0137 的种子行、`CUS-0001..0003` 是历史数据，而 `next_master_code_free` 的设计就是「撞号顺延」。
   实测首个自动号 = `SUP-0002` / `CUS-0004`；把测试行删掉后 `biz_seq.current_no` 仍停在 `SUP=3` / `CUS=5`（主数据是档案，建成功就永久占号，号只前进不回退）。
   若是**全新空库**，首个自动号就是卡面写的 `SUP-0001` / `CUS-0001`（CODE-01 在用 `cloudpivot_code01`
   空库上验证过六条号段的首次取号，见 `docs/evidence/20261108-code01-master-data-seq/`）。
2. **重复编码回 409 而不是 400**：全仓主数据编码重复一律 `409 + 66001`（`uk_biz_*_code` 部分唯一索引的翻译口径，客户/物料/仓位都是这套），本卡沿用，没有为卡面单独造一个 400。文案按要求点名具体编码。
3. **卡面的请求体 `{"name": …}` 不适用于供应商**：供应商 DTO 的字段是 `supplier_name`（列名就是 `biz_supplier.supplier_name`），卡面那个 JSON 会被 400（缺少必填 `supplier_name`）。实测脚本用的是 `{"supplier_name": …}`；客户侧确实是 `{"name": …}`。

## 四、单测（`#[cfg(test)] mod tests`，全部真库、无 ignore/skip）

| 位置 | 用例 | 结果 |
|---|---|---|
| `erp::seq` | `t02b_manual_code_resolution`（null/空串/空白→自动；`GYS-001`/`S.001_A`→保留） | pass |
| `erp::seq` | `t02c_manual_code_validation_rejects_unusable_input`（超长 41 拒、正好 40 收；空格/中文/`/`/`#`/TAB 拒） | pass |
| `erp::seq` | `t07_free_code_skips_taken_codes`（追加 `master_code_taken` 断言：已占用 true / 未占用 false） | pass |
| `erp::handler::tests` | `supplier_auto_code_generated_when_blank`（`None`/`""`/`"   "` 三种都取号且真落库、两两不同号） | pass |
| `erp::handler::tests` | `supplier_manual_code_is_preserved`（`W1C-GYS-001` 原样保留，只 trim） | pass |
| `erp::handler::tests` | `supplier_auto_code_increments_consecutively`（两次自动号 +1） | pass |
| `erp::handler::tests` | `supplier_duplicate_code_rejected_with_message`（409 + 66001 + 文案含编码） | pass |
| `erp::customer_handler::tests` | 上面四条的同构版本（含 `is_auto_code` / `seq_no` / `hard_delete` 夹具） | pass |

跑法（真库用例要 `ERP_DB_URL`，缺省会 `SKIP` 打印 —— **本卡没有靠 SKIP 蒙绿**）：

```bash
export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_code01'
cargo test --lib -j 1 -- --test-threads=1 seq::tests               # 10 passed
cargo test --lib -j 1 -- --test-threads=1 customer_handler::tests  # 11 passed
cargo test --lib -j 1 -- --test-threads=1 supplier_                # 21 passed（含既有供应商用例）
```

## 五、全量回归归因（以及为什么不是「全绿」）

`cargo test --lib -j 1`（默认并行测试线程，2026-11-08 10:32→10:52）：**3260 passed / 208 failed / 0 ignored**（`raw/02_full_regression_summary.txt`）。

- **本卡相关模块失败数 = 0**（`seq::tests` / `customer_handler::tests` / `handler::tests::supplier_*` 全部 pass）。
- 失败集中在 `transfer_handler::db_*`、`pflow3b_tests`、`stocktake_tests`、`stockin_handler::*`、`purchase_handler`、`payable_bridge`、`asset/expense/contract linkage` 等**本卡未触碰**的文件，典型报错是「调拨单状态流转: query failed」这类与取号无关的 500。
- 归因依据（三条硬证据）：
  1. `git show --stat 246f68f` 只有 5 个文件，失败模块的源码与本卡提交**逐字节相同**；
  2. `grep -rln "create_supplier\|supplier_create\|CreateSupplierReq\|create_customer\|CreateCustomerReq" src/` → 只有 `dto.rs` / `handler.rs` / `customer_handler.rs` 三处，其它模块的用例**调不到**本卡的新逻辑；
  3. `grep -rn "seq::next_master_code\|master_code_taken\|resolve_manual_code\|customer_insert_ex" src/` → 除 `seq.rs` 自身外只有这两个建单路径引用，没有别的调用点。
- 另外：跑全量的那台共享工作区同时还有别的卡的在跑改动（`git status` 里有 transfer / stockin / flow 等 20+ 个未提交文件），且跑完之后又落了 `60e0329`（P3-05）并继续在编 `V0339 sign_freeze`（当前工作区存在 25 个编译错误）。**所以「全量红」不是本卡的结论，只能说明共享工作区当时的状态**。

## 六、145 上线（测试环境，没碰 144 生产）

1. `git push origin main` → `f9e372d..246f68f`；
2. 145 上 `git pull --ff-only`（ff 到 `246f68f`）+ `cargo build -j 2`（debug，1m09s，`target/debug/cloudpivot` 时间戳 10:29）；
3. `systemctl restart cloudpivot-test` → `active`；`journalctl` 显示 **「应用 0 个迁移」**（本卡不带迁移，号段来自 CODE-01 的 V0337）；
4. `curl :8033/health = 200`、`curl :80/health = 200`；
5. 真实 token curl 13 条断言全 PASS（`raw/00_summary.txt`），库侧 7 组断言全过（`raw/07_db_assert_145.txt`），测试行已清场（`sup_left=0 / cus_left=0`）。

状态取证：`raw/08_145_state.txt`（HEAD=246f68f、服务 active、8033 监听、健康 200、二进制 10:29、迁移 0）。

## 七、留给下游的（不在本卡范围）

- **CODE-05（前端 + 预览接口）**：
  - 后端还需要「预览取号」接口（`GET /api/business/erp/{supplier,customer}/next-code`）。建议**方案 B**（只算下一个号不占号，保存时再真取；撞号由本卡的 409/66001 兜住），`seq.rs` 里可以直接复用 `master_code_exists_sql` / `manage`… 具体实现见卡面。
  - 前端 `supplier/index.vue` 的 `validateSupplierForm` 现在**强制要求** `supplier_code`（`'供应商编码必填'`），客户页同理 —— 不清掉这条规则，「不点生成、直接保存」会被前端自己拦住（后端口径已经支持留空）。`buildSupplierCreatePayload` 目前发 `supplier_code: ""`，正好会被后端当自动生成。
- **CODE-08（系统开关 `erp.master_data.allow_manual_code`）**：开关关闭时在本卡的 `create_supplier` / `create_customer` 里加一道「有值即 400」的判断即可，取号分支不用动。
- **CODE-03（物料 / 仓库等）**：`seq::next_master_code_free(&tx, "MAT" / "WH" / "AC" / "ET")` 直接可用，`master_code_exists_sql` 已内置这四张表。
