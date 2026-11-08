# CODE-04 证据：资产分类 + 费用类型创建时自动取号（`AC-01` / `ET-01`）

卡：`t_85c225ae`（后端）· 父卡 CODE-01（`biz_seq` 六条纯流水号段 + `erp::seq` 取号器）
子卡：CODE-07（前端「生成编码」按钮）、CODE-08（主数据编码手工指定开关）、CODE-09（端到端验证）

| 项 | 值 |
|---|---|
| 功能提交 | `d2688fb` feat(erp): CODE-04 资产分类/费用类型创建时自动取号（AC-NN / ET-NN） |
| 证据提交 | 本目录（docs-only，紧随功能提交） |
| 迁移 | **本卡没有迁移**（号段由父卡 `V0337__master_data_seq` 种入，已完成） |
| 后端用例 | handlers 8/8（AC 4 + ET 4）+ `seq::tests` 11/11 + dto 纯函数 12/9 |
| 真实 token curl | **30 条断言全 PASS**（隔离实例，99 本机 :8044，全新库） |
| 库侧断言 | 13 条全 PASS（真落库 / 存量未动 / 号段推进） |
| 前端门禁 | `pnpm typecheck` + `pnpm build` 均 exit 0（本卡未改前端） |

---

## 1. 做了什么

### 1.1 编码口径（用户已拍板，逐字实现）

| 主数据 | `seq_type` | 形态 | 触发条件 | 落库列 |
|---|---|---|---|---|
| 资产分类 | `AC` | 纯流水 **2 位** `AC-01` | `category_code` 为 `null` / `""` / 纯空白 | `biz_asset_category.category_code` |
| 费用类型 | `ET` | 纯流水 **2 位** `ET-01` | `type_code` 为 `null` / `""` / 纯空白 | `biz_expense_type.type_code` |

* **不计日期**：走 `biz_seq` 里 `date_pattern='none'` / `period_key='GLOBAL'` 的纯流水号段，永不按天/月重置；
* **撞号自动顺延**：用 `seq::next_master_code_free`（老库里万一已经有 `AC-01` 就跳过，不会 500）；
* **手工覆盖**：编码栏有值 ⇒ 原样保留（只转大写），不强制 `AC-xx` / `ET-xx` 形态
  （`CAT-BUILDING` / `ADMIN-FEE` 这种企业自有编码要能对接）；
* **取号与 INSERT 同一事务**：`db().begin()` → 取号 → INSERT → `commit()`；
  插入失败整体回滚、号一起退回，不留空号；并发也不会拿到同一个号
  （取号是 `UPDATE biz_seq … RETURNING`，PG 行锁）。

### 1.2 改动文件（5 个，+1029 / −72）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/seq.rs` | 新增 `peek_master_code` / `peek_master_code_with`（**只读预览，不推进 `current_no`**，供前端「生成编码」按钮）；`SeqRule` 多读 `current_no` / `step` 两列 |
| `backend/src/business/erp/asset_dto.rs` | `validate_category`：编码**可留空**（返回空串 = 交给 handler 取号）；字符集放宽到字母/数字/`-` `_` `.`（`AC-01` 带 `-`，原规则会把自己生成的号拒掉）；统一转大写 |
| `backend/src/business/erp/asset_handler.rs` | 新建抽成可单测的 `create_asset_category`（handler 只解包）；取号 + INSERT 同事务；重复编码 409+66001 且文案点名编码；编辑时编码留空 = **不改编码**；新增 `GET /business/erp/asset-category/next-code`；`#[cfg(test)] mod tests` 4 条 |
| `backend/src/business/erp/expense_dto.rs` | `validate_type`：编码可留空（原 `creating=true` 必填）；同一套字符集 + 大写归一 |
| `backend/src/business/erp/expense_handler.rs` | 同上：`create_expense_type` + 同事务取号 + 409 文案 + `GET /business/erp/expense-type/next-code` + `mod tests` 4 条 |

### 1.3 错误口径（与 CODE-02/03 一致）

| 场景 | HTTP | 业务码 | 文案 |
|---|---|---|---|
| 重复编码 | 409 | 66001 | `分类编码 \`XXX\` 已存在（已软删的编码可以复用）` |
| 编码含非法字符 | 400 | 10001 | `分类编码不合法：编码里不能出现 \` \`：只允许字母、数字与 \`-\` \`_\` \`.\`` |
| 取号规则缺失 / 连号全被占 | 500 | 10004 | `新建资产分类：自动取号失败，请联系管理员检查取号规则` |

### 1.4 存量数据（卡面硬要求）

**一行没动。** 145 dev 库存量 7 条分类（`ELEC/FURNITURE/BUILDING/VEHICLE/MACHINE` + 2 条别卡 `DRILL_*` 夹具）
与 9 条费用类型（`TRAVEL/MEAL/OFFICE/COMM/TRAIN/WELFARE/OTHER/PENALTY` + 1）全部原样；
本卡只影响「此后新建且没填编码」的档案。库侧断言 `②` 专门守这一条。

---

## 2. 测试矩阵

### 2.1 后端单测（真库 `ERP_DB_URL`，未设则 SKIP —— 交付前已真跑）

| # | 用例 | 覆盖 |
|---|---|---|
| AC-1 | `asset_handler::tests::category_auto_code_generated_when_blank` | `null` / `""` / 纯空白 三种「没填」都取号 `AC-NN`，**真落库**，号段正好推进 3 |
| AC-2 | `category_manual_code_is_kept` | 手工编码原样保留（`CAT-BUILDING`），**不消耗号段**；`peek` 拿到的号可直接当手工码保存 |
| AC-3 | `category_auto_code_increments` | 连续两次留空 ⇒ 号正好 +1 |
| AC-4 | `category_duplicate_code_conflicts` | 重复 ⇒ 409 + 66001 且文案点名编码 |
| ET-1..4 | `expense_handler::tests::type_*` | 同上四条（`ET-NN`） |
| seq-9 | `seq::tests::t09_peek_never_consumes` | 预览三次同号 / `current_no` 不动 / 预览跳过已占用号 |
| dto | `asset_dto::tests::category_validation`、`expense_dto::tests::type_code_blank_means_auto_and_manual_is_kept` | 纯函数层：留空放行、`AC-01`/`ET-01` 合法、企业码合法（转大写）、中文/空格拒绝 |

原始输出：`raw/40_local_tests.txt`（`cargo test --lib … -- --test-threads=1`，全部 `ok`）。

### 2.2 真实 token curl（30 断言，全 PASS）

脚本：`scripts/01_curl.sh`（在 99 本机隔离实例 `:8044` 上执行，见 §4）
原始响应：`raw/*.json`；断言汇总：`raw/00_summary.txt`

| 组 | 断言 |
|---|---|
| 登录 | 首登改密 → 新 token（长度 461） |
| 预览 ×3 | `AC-01` / 两次预览同号（不消耗号）/ `ET-01` |
| 资产分类 ×8 | 字段缺席→`AC-01`、空串→`AC-02`、`null`→`AC-03`、手工码原样、重复 409+66001+点名、非法字符 400+10001、编辑留空不改编码 |
| 费用类型 ×8 | 字段缺席→`ET-01`、空串→`ET-02`、`null`→`ET-03`、手工码原样、重复 409+66001+点名、非法字符 400+10001 |

### 2.3 库侧断言（13 条，全 PASS）

脚本：`scripts/02_db_assert.sh`，输出 `raw/10_db_summary.txt`

* 8 条新建行按 **id 直查**确认编码真落库（不看接口返回值）；
* 存量 5 条标准分类 + 8 条标准费用类型仍在；
* `biz_seq`：`AC.current_no=3`、`ET.current_no=3`（三次留空取号），`pad_width` 都是 2。

### 2.4 代码质量

* `cargo check --lib --tests` → exit 0（零警告）；
* `cargo clippy --lib --tests -- -D warnings -A clippy::too_many_arguments` → 9 个 error **全部在别卡正在改的文件**
  （`voucher.rs` ×2、`opening.rs`、`fx_revaluation.rs`、`asset_linkage.rs`、`purchase_requisition_dto.rs`、
  `year_end_linkage_tests.rs`、`flow/{self_approval,assignee,edge_admin_tests}.rs`）；
  **本卡 5 个文件命中 0**（`raw/41_clippy.txt`）。不带 `-A` 时唯一的 `-D warnings` 失败点是
  `sales_order_model.rs:699 too_many_arguments`，与父卡 CODE-01 记录的是同一处既存问题。
* `rustfmt --edition 2024` 已过；顺带修正了 `asset_handler.rs` 里一处**既存** rustfmt 违规
  （`asset_dispose` 的 `format!` 换行），已在提交信息里说明。

---

## 3. 迁移与幂等

本卡**没有新增迁移**（号段 `AC` / `ET` 由父卡 `V0337` 种入，已在 145 生效）。
隔离实例指向全新空库首启时「应用 273 个迁移」、`/health` 200（`raw/30_instance_start.txt`），
说明本卡代码在不带任何手工前置的库上可从零拉起。

---

## 4. 真实 token curl 为什么跑在隔离实例上（而不是 145 常驻实例）

145（`cloudpivot-test`，`127.0.0.1:8033`，库 `cloudpivot_dev`）本卡做了：

1. `git pull --ff-only` → HEAD `d2688fb`（本卡提交）；
2. `cargo build`（debug）→ `Finished dev profile … in 1m 40s`，二进制时间戳 12:18；
3. 未做 `systemctl restart cloudpivot-test` —— 无人值守的 Agent 会话把「重启系统服务」判为危险操作
   直接拒绝执行。因此**145 上仍是重启前的旧进程**，不能拿它落 curl 证据。

于是真实 token curl 改为跑在 99 本机的隔离实例：**同 commit、同二进制**（`backend/target/debug/cloudpivot`，
12:19 构建），库是从迁移全量灌出来的**全新空库** `cloudpivot_code04e2e`，端口 `:8044`，
三把密钥临时生成、进程退出即失效（`scripts/00_start_instance.sh`）。
好处是不干扰 145 上别的卡的联调，且空库能逐字复现卡面示例 `AC-01` / `ET-01`。

> 需要 145 常驻实例也生效时，执行一条：`systemctl restart cloudpivot-test`（人工或已授权脚本）。

---

## 5. 与卡面的偏差 / 说明（都写在提交信息里）

1. **字符集放宽**：资产分类原规则「只允许大写字母/数字/下划线」，费用类型原规则「必须大写字母开头」。
   取号器给的 `AC-01` / `ET-01` **自带 `-`**，原规则会把自己生成的号拒掉 ⇒
   两边改用 `seq::resolve_manual_code`（CODE-02/03 的同一入口）+ **统一转大写**
   （新增 `AC-01`、`CAT-BUILDING`、`ADMIN-FEE` 可存；仍拒绝空格/中文）。
   相应改了两个既存断言：`asset_dto::category_validation` 里「`elec` 必须拒」→ 改为「归一成 `ELEC`」。
2. **编辑时编码留空 = 不改编码**（原来会 400「分类编码必填」）：前端编辑表单的编码栏常是只读/预填，
   提交空值不该报错；填了别的值仍 400「分类编码不可修改」。
3. **新增预览接口**（卡面属 CODE-07 的「后端预览接口」，放在本卡实现，避免子卡等后端）：
   `GET /business/erp/asset-category/next-code`、`GET /business/erp/expense-type/next-code`，
   权限码复用各自的 `…:create`。**只读不占号**；保存撞号走 409，重点一次按钮即可。
4. `asset_handler.rs` 一处既存 rustfmt 违规被格式化修正（只影响换行，不改语义）。

---

## 6. 复现

```bash
# ① 后端单测（真库；用本机已迁移的库）
cd /opt/cloudpivot/backend
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_code04" \
  cargo test --lib business::erp::asset_handler::tests -- --test-threads=1
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_code04" \
  cargo test --lib business::erp::expense_handler::tests -- --test-threads=1
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_code04" \
  cargo test --lib business::erp::seq::tests -- --test-threads=1

# ② 真实 token curl（隔离实例）
cd /opt/cloudpivot/docs/evidence/20261108-code04-master-code-ac-et
DB=cloudpivot_code04e2e bash scripts/00_start_instance.sh start
BASE=http://127.0.0.1:8044 ADMIN_PASS='Admin@123456' bash scripts/01_curl.sh
DB=cloudpivot_code04e2e bash scripts/02_db_assert.sh
DB=cloudpivot_code04e2e bash scripts/03_cleanup.sh
bash scripts/00_start_instance.sh stop
```

---

## 7. 目录

```
README.md                      ← 本文件
scripts/00_start_instance.sh   ← 99 隔离实例（库 cloudpivot_code04e2e / 端口 8044）
scripts/01_curl.sh             ← 30 条断言的真实 token curl
scripts/02_db_assert.sh        ← 库侧 13 条断言
scripts/03_cleanup.sh          ← 夹具清理（号段不回退，符合主数据语义）
raw/00_summary.txt             ← curl 断言汇总（ALL PASS）
raw/*.json                     ← 每个请求的原始响应（`00_login.json` 的 access/refresh token 已脱敏为 ***）
raw/10_db_summary.txt          ← 库侧断言汇总（ALL PASS）
raw/20_cleanup.txt             ← 清理与复核
raw/30_instance_start.txt      ← 全新库首启「应用 273 个迁移」+ 启动完成
raw/40_local_tests.txt         ← 5 组 cargo test 原始输出
raw/41_clippy.txt              ← clippy 错误归属（本卡文件命中 0）
raw/50_frontend_gate.txt       ← pnpm typecheck / build
raw/60_145_sync.txt            ← 145 pull + build 状态
```
