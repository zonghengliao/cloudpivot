# E2E05-FIX-3 证据（状态日志动作码 / 只读字段 / 作废处置留痕日期）

卡：`t_e99f0719` · 提交：`a1565ee8`（代码）· 环境：145 测试机 · 库 `cloudpivot_dev` · 隔离实例 `127.0.0.1:8038`

## 1. 三条问题与修法

| # | 现场（E2E-05 演练） | 修法 | 关键文件 |
|---|---|---|---|
| ① | 部门转移 / 使用人变更不改状态，`biz_status_log` 写一条 `from = to = IN_USE`，时间线读起来像脏数据 | **方案 A（数据层分清）**：`biz_status_log` 加 `action varchar(32)`（迁移 `V0365`），转移写 `TRANSFER`、使用人变更写 `CUSTODIAN`，真正的状态流转仍为 `NULL`；`trail_of_no` 一并读出新列 | `migrations/business/V0365__biz_status_log_action.{up,down}.sql`、`shared/state_machine.rs`、`erp/asset_handler.rs` |
| ② | `PUT /asset/{id}` 带 `original_value` / `category_id` 静默 200（DTO 没声明 ⇒ serde 丢字段） | **方案 B（精准报错）**：DTO 显式接收这两个字段，与现值不同 → 400 点名「原值 / 资产分类不可修改」并给出当前值与该走的路径；回传现值（整表单回传）照常 200。**不用** `deny_unknown_fields`（会误伤其它扩展字段） | `erp/asset_dto.rs::reject_immutable_update`、`erp/asset_handler.rs` |
| ③ | 作废处置的变动留痕恒取「今天」，处置日期 2028-05-25 的卡作废后落在今天，按会计期间筛本期变动两头都查不到 | 作废处置写 `ENABLE` 行时 `change_date` **缺省 = 原处置日**；接口新增可选 `change_date`（显式指定优先）；异常数据（已处置却没有处置日）才退回今天并 `tracing::warn!` | `erp/asset_dto.rs::cancel_disposal_change_date`、`erp/asset_handler.rs::asset_dispose_cancel` |

前端配套（①的可视层）：`asset-logic.ts::changeSummary` 改为只列**真正变了**的字段
（`from == to` 的部门 / 使用人 / 状态不再拼接），所以时间线显示「部门转移 · 研发部 → 财务部」
而不是「在役 → 在役；张三 → 张三」。单测：`frontend/tests/unit/fin2-asset.test.ts`。

## 2. 验收结果（145 真 token curl，`145_run.out` / `145_asserts.log`）

`PASS=61 FAIL=0`（脚本 `scripts/verify/e2e05fix3_p4x3_e2e.sh`，`RUN_TAG=evid1`）。要点：

- **①**：转移后卡片状态仍 `IN_USE`；`biz_status_log` 里 `IN_USE→IN_USE` 那两行 `action` 分别是
  `TRANSFER` / `CUSTODIAN`（建卡那条是状态流转，`action = NULL`）；`GET /asset/{id}` 的
  `changes[0]` 是 `change_type=TRANSFER`、`change_label=部门转移`、`from_status=to_status=IN_USE`、
  `from_dept_name=演练设备部 → to_dept_name=演练设备二部`。
- **②**：`PUT` 带 `original_value=999999` → **400**「原值不可修改（当前 12000.0000，请求 999999）…」，
  库里仍是 `12000.0000`，且没有多写状态日志（拒绝发生在 UPDATE 之前）；带别的 `category_id` → 400
  「资产分类不可修改」；整表单回传现值 + 改名 → 200 且改名生效。
- **③**：`disposal_date=2030-01-15` 报废 → 审批通过 → 作废处置（不传 `change_date`）→
  `biz_fixed_asset_change(ENABLE).change_date = 2030-01-15`（**不是今天 2026-11-09**）；
  第二次作废显式传 `2030-02-03` → 落 `2030-02-03`（显式优先）。

库侧复核见 `db_check.sql` / `db_check.out`（状态日志逐行、变动时间线逐行、卡片原值、ENABLE 行日期）。

## 3. 测试

```bash
# 本机（99）真库：新建 scratch 库 fix03_test 并从 cloudpivot_copy_fix4 复制，
# 手工 apply V0361 / V0362 / V0365（前两条是复制库落后于仓库的迁移）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/fix03_test' cargo test --lib business::erp::asset_linkage_tests -- --test-threads=1
#   → 15 passed（含 fix03_transfer_and_custodian_log_action_codes /
#      fix03_update_rejects_readonly_fields / fix03_cancel_disposal_keeps_disposal_date）
cargo test --lib business::erp::asset_dto            # 15 passed（3 条新纯逻辑用例）
ERP_DB_URL=… cargo test --lib business::shared::state_machine   # 44 passed
ERP_DB_URL=… cargo test --lib business::erp::asset              # 71 passed
cargo check --lib / --tests                          # 0 error 0 warning
cd frontend && pnpm typecheck && pnpm build          # 均 exit 0
npx tsx --test tests/unit/fin2-asset.test.ts         # 29 passed
```

迁移幂等：`V0365` 的 `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` 连跑两次均通过
（第二次只有 `already exists, skipping`），且迁移末尾 DO 块自检列类型。

## 4. 落地决策与理由

- **① 选方案 A**（卡面建议）：数据层分清后，任何消费者（前端时间线、对账脚本、审计导出）都不用猜
  `reason` 前缀；词表与 `biz_fixed_asset_change.change_type` 同字面（`TRANSFER` / `CUSTODIAN`），两边可对拍。
  存量 `from = to` 行**不回填**：历史动作码无从还原，猜错比留空更糟（前端已按「from == to 就不渲染状态对」兜底）。
- **② 选方案 B**：`deny_unknown_fields` 会把未来新增字段、前端冗余字段一起拒掉，误伤面太大；
  精准拒「这两个字段变了」即可。判据用 `round_dp(4)`（库里 `numeric(18,4)`，`120000` 与 `120000.0000` 同值）。
- **③ 缺省回原处置日而非「今天」**：变动行的业务日是「这次处置被撤销」这件事发生的账务日；
  取今天会让它落在与处置无关的期间。显式参数留给「确实要记今天」的调用方。
  两列（业务日 / 记录日）改动大，本次不做，`created_at` 已经承担「记录日」。
- **迁移号段**：并发在途卡占用了 `V0363`（服务器状态菜单），`0364` 留作缓冲，本卡取 `V0365`；
  迁移版本号跨目录全局唯一（重复会启动失败）。

## 5. 145 上的残留（可安全保留）

- `cloudpivot_dev` 已由实例启动时自动应用 `V0365`（预期行为）；夹具按
  `asset_name LIKE 'E2E05FIX3-%'` / 部门 `E2E5F3-D2` 命名，可随时按名字清。
- 隔离实例 `127.0.0.1:8038` 验收后已 kill（复核无监听）。

> **操作事故（本卡，已恢复）**：收尾 kill 8038 时用了 `pkill -f "target/debug/cloudpivot"`，
> 该模式把 145 上另外两个实例（8033 `cloudpivot-test`、8034 dev 实例）一起杀了约 9 分钟。
> 恢复：`systemctl start cloudpivot-test` → 8033 `health=200`；8034 按同一份
> `/etc/cloudpivot/cloudpivot-dev.env` + `NIK_SERVER_LISTEN=127.0.0.1:8034` 重启 → `health=200`
> （pid 1431354）。教训：**杀实例必须按端口 / pid 定位**，绝不用宽匹配的 `pkill -f`。


## 6. 复跑方式

```bash
# 145
cd /opt/cp-e2e05fix2 && git fetch origin main && git checkout a1565ee8
cd backend && cargo build --bin cloudpivot          # 增量约 2 分钟
bash /tmp/fix03_start.sh &                          # 见本目录 README 附带的启动方式：8038 + cloudpivot_dev
# 99（脚本与公共库在仓库里）
scp scripts/verify/e2e05fix3_p4x3_e2e.sh root@145:/tmp/
WORK=/tmp/fix03_145 BASE=http://127.0.0.1:8038/api bash /tmp/e2e05fix3_p4x3_e2e.sh
```
