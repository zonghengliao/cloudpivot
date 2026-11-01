# ERP-R1 资产负债表 · 验收留痕

| 文件 | 内容 |
|------|------|
| `00-证据汇总.txt` | 提交号 / 远端一致 / `cargo test --lib` 结果 / 端到端 PASS 数 / 生产真机探针 / 不变量套件 |
| `10-端到端验收.log.txt` | `bash scripts/verify/erpr1_balance_sheet.sh` 全量输出（70 PASS / 0 FAIL，含"删一条分录制造不平"与恢复） |
| `20-cargo-test-摘要.txt` | 全新库 `cloudpivot_r1v2` 上 `cargo test --lib -- --test-threads=1` 的尾部（1685 passed / 0 failed） |
| `30-页面-平衡态.png` | 资产负债表页面·平衡态（绿框）：`✅ 平衡：资产 435.0000 = 负债和所有者权益 435.0000` |
| `31-页面-不平态.png` | 同一页面·删掉一条贷方分录后（红框）：`⚠️ 不平 … 差 226.00 元`，并点名凭证 `PZ-20261031-000001` |

> 截图的**权威判据是 DOM**（截图只是给人看的）：平衡态 `#bs-check` 类名 `bs-check--ok`、
> 背景 `rgb(227,249,233)`；不平态类名 `bs-check--bad`、背景 `rgb(255,240,237)`。
> 页面验证用的是「本项目 vite dev + 生产数据副本 `cloudpivot_r2`」（与 144 的 demo 链路同构：vite → 后端 → PG）。

## 复跑方式

```bash
# 端到端（自建隔离库 cloudpivot_erpr1v，不碰生产）
bash scripts/verify/erpr1_balance_sheet.sh

# 单测（需先有一个跑到最新迁移的空库）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/<db> \
NIK_PROFILE=test NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  cargo test --lib business::erp::report -- --test-threads=1

# 不变量
NIK_DATABASE_NAME=<db> python3 scripts/verify/erp_invariants.py
```
