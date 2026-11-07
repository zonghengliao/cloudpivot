# LNK-11 真机取证：年结（ACCOUNTING_YEAR）close / reopen → 联动引擎

- 日期：2026-11-07
- 实例：`http://127.0.0.1:18062`（隔离实例，`NIK_PROFILE=test`，DB `cloudpivot_lnk11`，**空库首启**，
  迁移全链由实例自己跑）
- 二进制：`/root/lnk11-target/debug/cloudpivot`（LNK-11 源码的隔离构建；配置 / 迁移目录用同一棵隔离 worktree）
- 脚本：`scripts/verify/lnk11_start.sh` + `scripts/verify/lnk11_http_evidence.py`
- 输出：`docs/evidence/20261107-lnk11-year-end/curl_evidence.txt`（脚本原样落盘）

## 链路（触发与查询全走 HTTP）

| # | 动作 | 结果 |
|---|------|------|
| ① | `GET /api/business/flow/linkage?trigger_entity=ACCOUNTING_YEAR` | 配置页 `wired=true`，`wiring` 文案可读（两处真实状态推进 + 排除清单 + commit 后发通知） |
| ② | `PUT /api/business/flow/linkage`（`CLOSED` / `NOT_CLOSED` 各一条 `SEND_NOTICE`，TAKEOVER→CREATOR） | 2 条规则创建成功 |
| ③ | `POST /api/business/erp/period/batch-open`（2043-01 ~ 2044-12） | `created=24`（本年 + 下一年度各 12 期） |
| ④ | `POST /api/business/erp/voucher` × 3 + `audit` + `post` | 3 张已过账凭证（3 / 7 / 11 月，**含多张凭证的年度**） |
| ⑤ | psql 把 2043 的 12 个月置 `CLOSED`（年结前置；与 `year_end_test_support::close_all_months` 同口径） | 12 个月已结账 |
| ⑥ | `POST /api/business/erp/year-end/close`（2043） | http 200，结转凭证 `PZ-…-000006`、4 条分录、本年净利润 `4000.00`、`closed=true`、下年期初 2 行；**通知 1 条**（正文带年度 / 凭证号 / 分录数 / 净利润） |
| ⑦ | 再次 `POST year-end/close`（2043） | http 409 / code 10003「2043 年度已经结转过了…不能重复年结」；通知仍 1 条、结转凭证仍 1 张 |
| ⑧ | `POST /api/business/erp/year-end/reopen`（2043） | http 200，`reopened=true`、结转凭证整张删除、12 月解封；`NOT_CLOSED` 通知 1 条（正文带原因） |
| ⑨ | 失败场景（2045）：配 `UPDATE_RECEIVED_QTY`（订购 10 / 本次收 50 → 超收，Critical + InTx）+ `SEND_NOTICE`，`POST year-end/close` | http **500**「联动动作 UPDATE_RECEIVED_QTY…超收…已整笔回滚」；年度**无状态日志**、结转凭证 **0 张**、通知 **0 条** |
| ⑩ | 撤掉失败规则，再次 `POST year-end/close`（2045） | http 200，结转凭证 1 张、通知 **1 条**（正向对照：⑨ 的 0 不是「规则没命中」的虚绿） |

## 结论

1. **年结真的触发联动**（不是只改 `wired` 标记）：真 HTTP 一次年结 → 1 条
   `LNK:ACCOUNTING_YEAR:CLOSED:<年度>:<结转凭证>` 通知；含 3 张凭证的年度也只发 **1 条**
   （触发单据是**年度**，不是每张凭证一条）。
2. **重复年结不重复触发**：`ALREADY_CLOSED` 409 是主闸（到不了 hook），通知幂等键（含本次事件凭证 id）
   是第二道闸 —— 重跑既不翻倍通知也不翻倍凭证。
3. **反年结真的触发**：`CLOSED → NOT_CLOSED` 一条通知，正文带反年结原因与被删凭证号。
4. **卡面核心：失败回滚后通知不发**。事务内关键动作（`UPDATE_RECEIVED_QTY` 超收）失败 →
   `run_excluding` 返回 `Err` → 整笔年结回滚（结转凭证 0 张、状态日志 0 行、**通知 0 条**）；
   撤掉失败规则后同一套请求成功并发出 1 条通知，证明上面的 0 是「回滚」而不是「规则没命中」。
5. **排除清单生效**：年结只落 1 张 `YEAR_END` 结转凭证（`GENERATE_VOUCHER` 未重复落账），
   反年结把它整张删掉。

## 清场

脚本 `atexit` 清场：2043 / 2044 / 2045 / 2046 的期间 + 凭证 + 期初 + 状态日志 + 规则 + 通知 +
采购单。清场后复核（同一库）：`rules=0 notices=0 periods=0 vouchers=0 po=0`；实例已停、`:18062` 已释放。

## 复现

```bash
bash scripts/verify/lnk11_start.sh          # 起隔离实例（DB=cloudpivot_lnk11，:18062）
python3 scripts/verify/lnk11_http_evidence.py
# 证据落盘：docs/evidence/20261107-lnk11-year-end/curl_evidence.txt
```

> 注：空库首启时 `admin` 带 `must_change_password=true`（CODE-06），脚本会先走
> `PUT /api/system/auth/password` 把初始口令换成固定取证口令再继续；
> 另外 `erp.separation_of_duties.approve` 在取证库里置 `false`（单账号取证，关掉制单/审核分离），
> 该参数会被启动快照缓存，**必须在起实例前改**。
