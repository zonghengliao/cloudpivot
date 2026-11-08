# CODE-01 主数据编码地基：`biz_seq` 六条序列 + `erp::seq` 通用取号器

- 卡号：`t_6e689ef9`（CODE-01）
- 提交：`555337e`（代码 4 文件 +741 行）· 本条提交 = 证据
- 跑在：145 测试环境（`cloudpivot-test.service`，`NIK_SERVER_LISTEN=127.0.0.1:8033`）已上线验证

## 1. 交付物

| 路径 | 内容 |
|---|---|
| `backend/migrations/business/V0337__master_data_seq.up.sql` | `biz_seq` 加 `pad_width` + 种入六条主数据序列（幂等 + 末尾自检） |
| `backend/migrations/business/V0337__master_data_seq.down.sql` | 回滚：删六条规则 + 撤 `pad_width` |
| `backend/src/business/erp/seq.rs` | 取号器：`format_master_code` / `next_master_code(_global)` / `next_master_code_free(_global)` / `next_master_code_free_with` + 8 条用例 |
| `backend/src/business/erp/mod.rs` | `pub mod seq;`（一行声明） |

## 2. 与卡面的三处偏差（**先核了真实表结构，照卡面抄会直接失败**）

| 卡面写法 | 真实情况 | 本卡落地 |
|---|---|---|
| `date_pattern = NULL` | `V0137` 建表：`date_pattern VARCHAR(8) NOT NULL DEFAULT 'yyyyMMdd'` + `CHECK (date_pattern IN ('yyyyMMdd','yyyyMM','none'))` ⇒ 写 NULL 触发 23502/23514 | `date_pattern = 'none'`（库里本来就用它表示「不带日期段」），验收口径改为 `= 'none'` |
| `ON CONFLICT (seq_type) DO NOTHING` | 唯一约束是**部分索引** `uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL` ⇒ 只写列名报 42P10 | `ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING`（谓词逐字对应） |
| 位数「加 `step` 或 `pad_width`」 | `step` 是**步长**（一次跳几个号），借它承载补零位数会把两个语义糅在一起 | 新增 `pad_width SMALLINT NOT NULL DEFAULT 4`（`CHECK 1..12`），4 位 = SUP/CUS/MAT，2 位 = WH/AC/ET |

## 3. 取号器 API（给 CODE-02 / CODE-03 用的口径）

```rust
use crate::business::erp::seq;

// ① 推荐：自动跳过已被占用的号（老数据/企业自建编码都能共存）
let code = seq::next_master_code_free(&tx, "SUP").await?;   // 在调用方事务里取号，回滚即退号
let code = seq::next_master_code_free_global("SUP").await?;  // 全局 db()（自动提交）

// ② 纯取号（不查重）：调用方自己保证编码没被占用
let code = seq::next_master_code(&tx, "WH").await?;          // WH-01

// ③ 非标准表/特殊软删口径：自己给占位检查 SQL（常量，一个 ? 占位）
let code = seq::next_master_code_free_with(&tx, "SUP", "SELECT 1 FROM biz_supplier WHERE supplier_code = ? AND deleted_at IS NULL", 20).await?;

// ④ 纯函数（预览/测试）
assert_eq!(seq::format_master_code("SUP", "-", 4, 1), "SUP-0001");
assert_eq!(seq::format_master_code("WH",  "-", 2, 1), "WH-01");
assert_eq!(seq::format_master_code("WH",  "-", 2, 100), "WH-100"); // 超位数不截断
```

- **并发安全**：单语句 `UPDATE biz_seq SET current_no = current_no + step … RETURNING current_no`，PG 行锁 ⇒ 并发不重号（用例 t06 用 10 个并发事务证明）。
- **位数**：来自 `biz_seq.pad_width`（`biz_seq` 那一行可改，改完立即生效；`format_master_code` 会对非法值收敛到 1..12）。
- **错误口径**：缺规则/停用 → `DbError::InvalidValue`，消息里带 `seq_type` 与「主数据号段由 V0337 种入」；非主数据号段调 `_free` → 消息里列出支持范围 `SUP/CUS/MAT/WH/AC/ET`。
- **手工覆盖**：「编码栏有值就保留用户输入，只有空/纯空白才取号」是调用方的判断（`next_master_code_free` 不碰已有编码）。
- **交互方式 B（点「生成编码」预览）**：预览就是调一次取号函数——号会被消耗（预览后不保存 = 跳号），这与「单据号预览」同款；不点按钮则在保存时取号。跳号不影响唯一性。

## 4. 交接：145 上六类编码「已占用」现状（决定第一次自动取号给几号）

实测（`raw/06_145_token_curl.txt` 的 ③ 段）：

| 主数据 | 145 已有编码 | 第一次自动取号会给 |
|---|---|---|
| 供应商 | `SUP-0001`（V0137 种子「示例供应商」） | **`SUP-0002`** |
| 客户 | `CUS-0001` / `CUS-0002` / `CUS-0003` | **`CUS-0004`** |
| 物料 | 无 `MAT-*`（种子是 `M-W1A-001…`） | `MAT-0001` |
| 仓库 | 无 `WH-NN` 形态（种子 `WH01`/`WH02`、演练 `WH-W3B-1`、`WH-DVA` 等） | `WH-01` |
| 资产分类 | 无 `AC-*`（种子是 `ELEC`/`FURNITURE`…） | `AC-01` |
| 费用类型 | 无 `ET-*`（种子是 `TRAVEL`/`MEAL`…） | `ET-01` |

> CODE-02 卡面写「传空 → 返回 SUP-0001」在**已种入 V0137 的库上拿不到** —— `SUP-0001` 已被示例供应商占用，`next_master_code_free` 会顺延到 `SUP-0002`。这是卡面 ③「取号撞了就重试下一个号」的**真实用例**，不是缺陷。

## 5. 验收证据（全部原始输出）

| 文件 | 证明什么 |
|---|---|
| `raw/01_migration_syntax_and_idempotency.txt` | 只有 `biz_seq` 的临时库上：up → 6 行；**第二遍 up = 0 行、无报错**；down → 只剩单据号段、`pad_width` 列消失；再 up 可重放 |
| `raw/02_unit_tests.txt` | `cargo test --lib seq::tests` → **8 passed / 0 failed**（真库 `ERP_DB_URL`） |
| `raw/03_145_deploy.log` | 145：`git pull` → `555337ef` → `cargo build` → 二进制含新代码 → 重启；启动日志「`V0337 自检通过：6 条主数据取号规则就位`」+「`数据库迁移完成: 应用 1 个迁移`」 |
| `raw/04_145_biz_seq.txt` | 145 实测：`schema_migrations` max=337、`master_data_seq / version=337`；六条规则 `date_pattern=none / period_key=GLOBAL / pad_width 4,4,4,2,2,2`；`/health` 200 |
| `raw/05_145_idempotent_restart.txt` | 145 再重启一次：`应用 0 个迁移`、六类行数仍 6、`version=337` 记录仍 1 条（**幂等**） |
| `raw/06_145_token_curl.txt` | 真实 token：`POST /api/system/auth/login` 200（access_token 487 字符）→ `GET /api/business/erp/supplier/page` 200（真实数据）→ `GET /api/system/auth/me` 200（`SUPER_ADMIN` / `*:*:*`）；另含「六类编码已占用现状」与「进程 exe md5 == 磁盘二进制 md5」 |
| `raw/07_clippy_baseline.txt` | `cargo clippy --lib --tests -- -D warnings`：HEAD 上唯一的 error 在 `sales_order_model.rs:699`（非本卡文件）；放行 5 条既有 lint 后 **exit=0，日志里 `seq.rs` 命中 0 次** |
| `raw/08_full_lib_tests.txt` | 全量 lib 回归：**3402 passed / 21 failed**，21 条全在联动测试，归因见下 |

## 6. 全量回归的 21 条失败：归因（不是本卡引入）

```
test result: FAILED. 3402 passed; 21 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1571.07s
```

- 17 条 `asset_linkage_tests::*` / `expense_linkage_tests::*`：panic 原文
  「库里没有部门（先跑基础种子迁移）」/「库里必须有一名带部门的用户」——
  本机隔离库是**只跑迁移**的空库：`sys_dept = 0`（demo 部门/带部门用户由演示种子脚本灌，不在迁移里）。
- 1 条 `stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean`：流程图快照自检（既有流程种子漂移）。
- 3 条 `flow::linkage::*`：联动白名单断言（当前真实调用点 16 个文件 vs 期望 15 —— `purchase_return_handler.rs` 这个调用点未进白名单，属 V0332 采购退货卡的既有漂移）。

反证：这 5 个失败用例所在文件里 `biz_seq|pad_width|erp::seq` 命中 **0 次**；本卡只加了一列 + 六行种子 + 一个新模块，不触碰 `sys_dept` / `sys_user` / 流程种子 / 联动白名单。

## 7. 本机复现（跑测试的最小步骤）

```bash
# 脚本都在本目录 scripts/（原样存档；里面 /root/code01-src2 是本卡的隔离 clone 路径，复现时改成自己的）
bash scripts/00_local_db.sh        # 建 cloudpivot_code01（owner=alun）
bash scripts/01_local_instance.sh  # profile=test + 18337 端口，等 /health 200

# 2) 只跑本卡用例
cd <repo>/backend
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_code01' \
  cargo test --lib seq::tests -- --test-threads=1
```

其余脚本：`02_migration_syntax.sh`（up/up/down/up 四段幂等回放）、`03_145_deploy.sh`（145 发布）、
`04_145_probe.sh`（145 六条规则实测）、`05_145_idempotent_restart.sh`（145 重启幂等）、
`06_145_token_curl.sh`（真实 token + 业务接口 + 编码占用盘点）。

> 例会用**专用测试号段**（`T4CON` / `T2DGT` / `T7SKP` / `TCCON` / `TOFF`），首尾按 `seq_type` 精确删除，
> 不消耗也不污染真实号段（PO/SUP/WH…），可反复跑。
