# LNK-10 真机取证：折旧批次（DEPRECIATION）过账 / 红冲 → 联动引擎

- 日期：2026-11-07
- 实例：`http://127.0.0.1:18061`（隔离实例，`NIK_PROFILE=test`，DB `cloudpivot_lnk09n`，252 条迁移齐全）
- 二进制：`/root/target-lnk10/debug/cloudpivot`（LNK-10 源码的隔离构建；配置 / 迁移目录用 `/opt/cloudpivot/backend`）
- 脚本：`scripts/verify/lnk10_start.sh` + `scripts/verify/lnk10_http_evidence.py`
- 输出：`docs/evidence/20261107-lnk10-depreciation/curl_evidence.txt`（脚本原样落盘）

## 链路（触发与查询全走 HTTP）

| # | 动作 | 结果 |
|---|------|------|
| ① | `GET /api/business/flow/linkage?trigger_entity=DEPRECIATION` | 配置页 `wired=true`，`wiring` 文案可读（两处真实状态推进 + 聚合口径 + 两条无源边） |
| ② | `PUT /api/business/flow/linkage`（POSTED / REVERSED 各一条 `SEND_NOTICE`，TAKEOVER→CREATOR） | 2 条规则创建成功 |
| ③ | `POST /api/business/erp/period` 开账 `2047-08` | http 200 |
| ④ | `POST /api/business/erp/asset` × 3（起提日 2047-01-01） | 3 张卡建成功 |
| ⑤ | `POST /api/business/erp/depreciation/run` | `outcome=POSTED`、批次 `asset_count=3`、金额 `7916.6667`；**通知 1 条**、正文「涉及 3 台资产」、计提凭证 1 张 |
| ⑥ | 再次 `POST depreciation/run` | `outcome=EXISTS`（幂等闸）；通知仍 1 条、凭证仍 1 张 |
| ⑦ | `POST /api/business/erp/depreciation/{id}/reverse` | 批次 `REVERSED`；REVERSED 通知 1 条 |
| ⑧ | 再次 `reverse` | http 409 / code 66039「当前是 REVERSED：只有已过账的批次能红冲」；通知仍 1 条 |

## 结论

1. **过账真的触发联动**（不是只改 `wired` 标记）：真 HTTP 一次计提 → 1 条 `LNK:DEPRECIATION:POSTED:<批次>:<凭证>` 通知。
2. **大批量聚合**：3 台资产的批次只发 **1 条**通知，台数写正文（结构上触发单据是批次，
   几百台与 1 台条数相同）。
3. **重复计提不重复触发**：`Exists` 幂等闸 + 通知幂等键（含事件凭证 id）双保险。
4. **红冲真的触发**，重复红冲被状态机挡住，通知不增。
5. **排除清单生效**：计提凭证只有 `post_multi_debit` 出的 1 张（`GENERATE_VOUCHER` 未重复落账）。

## 清场

脚本 `atexit` 清场：期间 2047-08 / 3 张卡 / 批次 + 明细 / 蓝红凭证 / 规则 / 通知 / 夹具部门。
清场后复核（同一库）：`notices=0 rules=0 batches=0 assets=0 period=0 dept=0`；
实例已停、`:18061` 已释放。
