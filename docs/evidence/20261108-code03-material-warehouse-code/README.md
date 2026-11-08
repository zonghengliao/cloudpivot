# CODE-03 物料 / 仓库创建时自动取号（允许手工覆盖）— 交付证据

- 卡：`t_56c6be71`（CODE-03）
- 依赖：CODE-01 的 `biz_seq` 六条主数据号段 + `erp::seq`（`555337e`/`410200c`）、CODE-02 的
  `resolve_manual_code` / `master_code_taken`（`246f68f`）
- 验证环境：**99 本机隔离实例**（`profile=test`，库 `cloudpivot_code03run`，`127.0.0.1:8043`），
  真实 token curl；**没有碰 144 生产**，也没有动 145 测试环境（别的卡正在用）
- 证据脚本：`scripts/00_start_instance.sh`（起停）/ `01_curl.sh` / `02_db_assert.sh` / `03_after_restart.sh`

## 一、卡面验收 vs 实测（真实 token curl，全新空库）

| # | 卡面验收 | 实测 | 结论 |
|---|---|---|---|
| ① | 物料编码留空 → `MAT-0001` | 字段**缺席** → `MAT-0001`；`""` → `MAT-0002`；`null` → `MAT-0003` | 通过 |
| ② | 手工指定 → 保留 | `YB-001-02` → 原样保留（**按分类自编号也能用**，不强制 `MAT-xxx`） | 通过 |
| ③ | 连续取号 | 上一条三个号严格 +1（`0001→0002→0003`） | 通过 |
| ④ | 重复编码 | HTTP **409** + 业务码 **66001**，`msg=物料编码 \`YB-001-02\` 已存在（已软删的编码可以复用）` | 通过（**偏差 §三.1**：仓内主数据编码重复统一 409 而非 400） |
| ⑤ | 仓库编码留空 → `WH-01`（**2 位**） | 字段**缺席** → `WH-01`；`""` → `WH-02`；`null` → `WH-03` | 通过（2 位由 `biz_seq.pad_width=2` 决定） |
| ⑥ | 仓库手工指定 → 保留 | `CK-001` → 原样保留 | 通过 |
| ⑦ | 仓库连续取号 | `WH-01→WH-02→WH-03` 严格 +1 | 通过 |
| ⑧ | 仓库重复编码 | 409 + 66001，文案点名 `CK-001` | 通过 |
| ⑨ | 重启不重置（主数据不计日期） | 重启后再取号 = `MAT-0004` / `WH-04` | 通过（额外补的一条，见 `03_after_restart.sh`） |
| ⑩ | 非法字符 | `material_code="BAD 001"` → 400 + 10001 + 人话文案 | 通过（额外补的一条） |

原始响应：`raw/00_login.json` … `raw/21_wh_after_restart.json`；断言汇总 `raw/00_summary.txt`、
`raw/03_after_restart.txt`；库侧断言 `raw/02_db_assert.txt`。

实测摘要（`raw/00_summary.txt` 原文节选）：

```
① 不传 material_code        → HTTP 200, material_code=MAT-0001
② 传 material_code=""      → HTTP 200, material_code=MAT-0002
③ 传 material_code=null     → HTTP 200, material_code=MAT-0003
④ 传 material_code="YB-001-02" → HTTP 200, material_code=YB-001-02
⑤ 重复 material_code        → HTTP 409, code=66001, msg=物料编码 `YB-001-02` 已存在（已软删的编码可以复用）
① 不传 warehouse_code       → HTTP 200, warehouse_code=WH-01
② 传 warehouse_code=""     → HTTP 200, warehouse_code=WH-02
③ 传 warehouse_code=null    → HTTP 200, warehouse_code=WH-03
④ 传 warehouse_code="CK-001" → HTTP 200, warehouse_code=CK-001
⑤ 重复 warehouse_code       → HTTP 409, code=66001, msg=仓库编码 `CK-001` 已存在（已软删的编码可以复用）
RESULT: ALL PASS
```

**为什么用全新空库**：卡面写的是首个自动号 = `MAT-0001` / `WH-01`。老库里早有 `MAT-xxxx` / `WH-xx`
形态的历史编码，`next_master_code_free` 的设计就是「撞号顺延」，只有空库才能逐字验证卡面示例
（CODE-02 在 145 上就因此首个号是 `SUP-0002` / `CUS-0004`）。空库里的种子数据是
`M-W1A-001..005` / `WH01`/`WH02`，**不占用** `MAT-0001` / `WH-01`，所以首次自动号正是卡面示例。

## 二、改了什么（2 个代码文件 + 证据）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/dto.rs` | `CreateMaterialReq.material_code` / `CreateWarehouseReq.warehouse_code`：`String` → **`Option<String>`**（`#[serde(default)]`，`null`/缺省都收）；两边 `validate()` 改成「留空放过，填了才校验长度 ≤ 40 + 字符集」（复用 CODE-02 的 `seq::resolve_manual_code`）；顺带改写既有的 `validate_rejects_blank_code`（物料空编码从「必须拒」变成「自动取号」） |
| `backend/src/business/erp/handler.rs` | 新建物料 / 仓库抽成可单测的业务主体 **`create_material(operator, req)`** / **`create_warehouse(operator, req)`**（axum handler 只做参数解包，单测走同一条代码路径）：手工编码预检 → `409/66001`（`material_code_conflict` / `warehouse_code_conflict`）→ 留空则 `seq::next_master_code_free(&tx, "MAT"/"WH")` → **取号与 INSERT 同一事务** → 唯一索引冲突再翻一次带编码的 409（`material_write_error` / `warehouse_write_error`） |
| `docs/evidence/20261108-code03-material-warehouse-code/**` | 本证据包（脚本 + 原始响应 + 库侧断言 + README） |

### 物料：原来没有事务，现在有了

`material_create` 之前是 `db().execute_with_params(INSERT …)` 一把梭。要「取号与落库同事务」
（插入失败必须退号、并发不许重号），这次把它包进 `db().begin()`：取号 → INSERT → `commit()`。

### 仓库：2 位号段确认

`WH` 的 `biz_seq.pad_width = 2`，`seq::format_master_code` 是 `format!("{:0width$}", no)`：

- `WH-01` … `WH-99` 补零成 2 位；
- 第 100 个起自然输出 `WH-100`（**只补零不截断**）——2 位号段被腰斩成 `WH-00` 才是重号灾难。
  CODE-01 的 `seq::tests` 里 `T2DGT` 已断言这条进位，本轮实测 `WH-01..WH-04` 形态正确。

### 错误口径（与 CODE-02 完全一致）

| 场景 | HTTP | 业务码 | 文案 |
|---|---|---|---|
| 手工编码格式不合法（超长 / 字符集外） | 400 | 10001 | `material_code 不合法：编码里不能出现 …`（仓库侧是 `warehouse_code 不合法：…`） |
| 手工编码已存在（预检命中或并发撞唯一索引） | 409 | 66001 | `物料编码 \`XXX\` 已存在（已软删的编码可以复用）` |
| 取号规则缺失 / 连号全被占 | 500 | 10004 | `新建物料：自动取号失败，请联系管理员检查取号规则` |

## 三、与卡面的两处偏差

1. **重复编码回 409 而不是 400**：全仓主数据编码重复一律 `409 + 66001`（`uk_biz_material_code` /
   `uk_biz_warehouse_code` 部分唯一索引的翻译口径），本卡沿用，没有为卡面单独造一个 400；
   文案按要求点名具体编码。
2. **卡面写的文件 `material_handler.rs` / `material_model.rs` / `warehouse_handler.rs` / `warehouse_model.rs`
   在仓里不存在**：物料与仓库的 CRUD 都在 `business/erp/handler.rs`（+ `dto.rs` / 模型在 `model_master.rs`）。
   改动落在真实文件上，没有为了对齐卡面去拆文件。

## 四、单测（`#[cfg(test)] mod tests`，真库，无 ignore/skip）

`backend/src/business/erp/handler.rs` 的 `tests` 模块新增 8 条（物料 4 + 仓库 4），全部走
`ERP_DB_URL` 真库、`--test-threads=1`：

| 用例 | 断言 |
|---|---|
| `material_auto_code_generated_when_blank` | `None` / `""` / `"   "` 三态都取号、彼此不同、**真落库** |
| `material_manual_code_is_preserved` | `"  W1C-YB-001-02  "` → `W1C-YB-001-02`（只 trim，不改形态） |
| `material_auto_code_increments_consecutively` | 第二次 = 第一次 + 1 |
| `material_duplicate_code_rejected_with_message` | 409 + 66001 + 文案点名编码 |
| `warehouse_auto_code_generated_when_blank` | 三态取号、**2 位形态**、真落库、不影响 `level`/`warehouse_path` |
| `warehouse_manual_code_is_preserved` | `"  W1C-CK-01  "` → `W1C-CK-01` |
| `warehouse_auto_code_increments_consecutively` | 第二次 = 第一次 + 1 |
| `warehouse_duplicate_code_rejected_with_message` | 409 + 66001 + 文案点名编码 |

跑法与结果：

```bash
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_code03run \
  cargo test --lib -- business::erp::handler::tests business::erp::dto::tests --test-threads=1
# → test result: ok. 57 passed; 0 failed; 0 ignored
```

全量 `--lib` 回归：见 §五（如实归因，不掩盖他卡的存量红）。

## 五、全量回归与检查链（原文见 `raw/05_checks.txt`）

| 检查 | 命令 | 结果 |
|---|---|---|
| fmt | `rustfmt --edition 2024 --check src/business/erp/{handler,dto}.rs` | clean |
| clippy | `cargo clippy --lib --tests` | 本卡 2 文件 **0 条**；其余 11 条是 asset_linkage / fx_revaluation / opening / purchase_requisition_dto / voucher / year_end_linkage_tests / sales_order_model / flow::{assignee,self_approval} 的既有 lint |
| 本卡模块 | `cargo test --lib -- business::erp::handler::tests business::erp::dto::tests --test-threads=1` | **57 passed / 0 failed** |
| 全量 `--lib` | `ERP_DB_URL=… cargo test --lib -- --test-threads=1` | **3461 passed / 20 failed / 0 ignored**（1546.72s） |

20 条红的归属（逐条列在 `raw/05_checks.txt`）：`asset_linkage_tests::lnk09_*`/`st02_*` 11 条、
`expense_linkage_tests::lnk05_*` 6 条、`flow::linkage` 3 条。归因：

1. 本卡只改 `erp/{dto.rs,handler.rs}`，而这三个失败文件里
   `grep -l 'create_material|create_warehouse'` = **0 处** —— 它们根本不走本卡改的建单路径；
2. 与 CODE-01 记录的全量基线**同族**（当时 21 条，同为 asset/expense/flow::linkage），21→20 的差异来自
   他在途改动，不是本卡引入；
3. 无 `ignore` / `skip`（`0 ignored`）：红是真实红，不用 ignore 掩盖。

## 五之二、2 位 / 4 位进位边界（实机 + 单测双证）

| 场景 | 做法 | 结果 |
|---|---|---|
| WH 2 位进位 | 把 `biz_seq.WH.current_no` 推到 99 再建仓库 | **`WH-100`**（`raw/22_wh_boundary.json`） |
| MAT 4 位正常 | `current_no=4` 再建物料 | `MAT-0010`（`raw/23_mat_boundary.json`） |
| MAT 4 位溢出 | `current_no=9999` 再建物料 | **`MAT-10000`**（`raw/24_mat_overflow.json`） |

即「只补零、不截断」：2 位号段跑到第 100 个不会回卷成 `WH-00`（那才是重号灾难）。
纯函数层面 CODE-01 的 `seq::tests::t05_two_digit_pad_and_overflow` 也断言了同一条（`T2D-01/02/100`）。

## 六、迁移

**本卡不带迁移**：号段与 `pad_width` 由 CODE-01 的 `V0337__master_data_seq` 提供。

顺带验证了迁移体系在新库上的行为：

- 全新库首启：应用 **272** 条迁移记录（`schema_migrations` 计数），`biz_seq` 六条规则就位；
- 二次启动（同库）：`schema_migrations` 计数 272 → 272、`max(applied_at)` 不变、日志里
  **0** 条 `executing statement batch`（= 一条迁移 SQL 都没重放）⇒ 幂等；
- 重启后再取号 `MAT-0004` / `WH-04`（号段不重置）。

## 七、给下游卡（CODE-06 / CODE-08）的交接

- **CODE-06（前端「生成编码」按钮）需要的预览接口 `GET /api/business/erp/material/next-code` /
  `.../warehouse/next-code` 本卡没有实现**——那两个接口写在 CODE-06 自己的任务书里，本卡刻意不做，
  避免两边各注册一次路由（重复注册会在启动时 panic）。实现建议：**只读预览、不占号**
  （`SELECT prefix, separator, pad_width, current_no + step` 或复用 `seq::master_code_exists_sql`
  顺延到首个空号），真正占号仍由保存时的事务取号负责；撞号由 409+66001 兜底。
- **CODE-08（`allow_manual_code` 开关）**：开关关掉时，只需在 `create_material` / `create_warehouse`
  的 `manual` 分支各加一道 400，取号分支不用动；读取时机放在 `req.validate()` 之后、`db().begin()`
  之前（与 CODE-02 给的建议一致）。
