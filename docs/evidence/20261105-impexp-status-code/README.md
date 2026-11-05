# 导出状态过滤缺陷（t_93cba3f4）：红绿证据

日期：2026-11-05 ｜ 提交：`dce703b`（github/main 与 origin/main 同一 sha）
环境：本机 99，PG 127.0.0.1:5432，隔离库 `cp_t93cba3f4_1`
（从 `cloudpivot_regress_20261105` 用 `pg_dump --schema=public` 逻辑复制，V0241/173 条迁移，
0 业务写入，只有本卡用例的夹具进出并已清理）。

## 1. 修复前的形态（本卡修的缺陷）

`ExportFilter.status: Option<i16>`（1 启用 / 0 停用）→ `erp_doc::push_filters` 只有
keyword 与 ids 两个分支，ERP 单据的状态是 `varchar` 文本码 ⇒ 传 `status` 既不筛也不报错。
**红证**：把新加的 status 分支临时注释掉，跑真库用例：

```
thread '...::voucher_status_code_filters_real_rows' panicked at src/business/impexp/builtin/erp_doc.rs:1024:9:
assertion `left == right` failed: status_code=POSTED 只能命中已记账那张（本卡修的就是这里放过全量）
  left: 2
 right: 1
test result: FAILED. 0 passed; 1 failed; ... finished in 0.27s
```

（夹具 2 张凭证：`POSTED` 与 `CANCELLED`；不传状态本来就该是 2 行 —— 说明用例能抓到「筛了等于没筛」。）

## 2. 修复后（绿证）

```
$ ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_t93cba3f4_1' \
  cargo test --lib business::impexp -- --test-threads=1 --nocapture
test ...::builtin::erp_doc::tests::voucher_status_code_filters_real_rows ... ok
test result: ok. 147 passed; 0 failed; 0 ignored; 0 measured; 2484 filtered out; finished in 8.42s

$ cargo test --lib business::masking
test business::masking::user_export::tests::status_filter_contract_is_unchanged_for_user_export ... ok
test result: ok. 40 passed; 0 failed; ...

$ cargo clippy --lib --tests -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 14s      # 零警告

$ cargo test --lib          # 全量（真库用例无 ERP_DB_URL 时 SKIP）
test result: ok. 2631 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 3. 无库时必须看得见 SKIP（不许假绿）

```
$ cargo test --lib business::impexp::builtin::erp_doc -- --test-threads=1 --nocapture
test ...::stock_in_export_fetches_real_rows ... SKIP: 未设置 ERP_DB_URL（不连真实数据库）
test ...::voucher_status_code_filters_real_rows ... ok
```

`ensure_db()` 是进程级 `OnceCell`，SKIP 只在同进程首个真库用例打印一次（既有行为）；
本轮输出的第一行就是它，不会被"绿"盖掉。

## 4. 真库断言的具体数字

| 用例 | 输入 | 期望 | 实测 |
|------|------|------|------|
| 不传状态（回归） | `keyword=D7EXPSTATUS` | 2 行 | 2 |
| `status_code=posted`（小写） | 同上 + 码 | 1 行，且命中的是 `POSTED` 那张 id | 1 / id 对上 |
| `status_code=CANCELLED` | 同上 + 码 | 1 行，命中的是另一张 | 1 / id 对上 |
| `status_code=OPEN`（凭证非法码） | 同上 + 码 | 422 + 消息含 `POSTED` / `AUDITED` | 通过 |
| ERP 单据传 `status=1` | — | 422 + 提示改用 `status_code` 并列出合法码 | 通过 |
| 夹具清理 | — | 0 行残留 | `biz_voucher like 'D7EXPSTATUS%'` = 0、`biz_stock_in like 'IN-D7EXP%'` = 0 |

## 5. 复现命令

```bash
# 1) 造隔离库（勿在生产跑；本机 alun 无 createdb 权限，用 postgres 建库）
su - postgres -c "psql -q -c \"create database cp_t93cba3f4_1 owner alun\""
PGPASSWORD=alun pg_dump -h 127.0.0.1 -U alun --no-owner --no-privileges --schema=public \
  cloudpivot_regress_20261105 > /root/cp_t93_dump.sql
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cp_t93cba3f4_1 -f /root/cp_t93_dump.sql
#    注意：dump 带 CREATE SCHEMA public，必报一次 already exists，**不要加 ON_ERROR_STOP**

# 2) 跑用例
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_t93cba3f4_1' \
  cargo test --lib business::impexp -- --test-threads=1 --nocapture
```

## 6. 未做 / 已知未清

- 前端随卡：另开 `t_f83ae18f`（`BatchExportPayload.status_code` 与三张单据页把列表状态码透传）。
- `scripts/lint/scope_gate.py` 当前在 HEAD 上**已红**（`expense_model_tests.rs` /
  `stockout_fifo_tests.rs` / `task_handler_*_tests.rs` / `system/auth/cache_tests.rs`
  未登记 `SKIP_MANIFEST`），与本卡无关（这些文件均为他人已提交内容），未在本卡内改动。
- 本次提交的 author 显示为 `frontend-developer`：本工作区 `.git/config` 的 `user.name`
  被其他 worker 改过，近期所有提交（含上游卡）都是该 author；未改写历史（force-push 在无人值守模式被禁）。
