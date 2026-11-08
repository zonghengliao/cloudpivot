# AUD-01 · 附件上传人姓名快照（`uploader_name`）—— 交付证据

> 卡面：`AUD-01 附件上传人姓名快照：加 uploader_name + setter + 存量 backfill`
> 日期：2026-11-09 · 执行：backend-developer

## 一句话结论

两张附件表都加上了**姓名快照**列并在**写入时**落库（不是实时 join）；存量行已在演练库与生产库
（144 `cloudpivot`）回填完毕，空值数 = 0；改名后回看旧附件仍显示**上传当时的名字**（HTTP 真机验证）。

## 验收对照

| # | 验收项 | 结果 | 证据 |
|---|--------|------|------|
| ① | 迁移后两表都有 `uploader_name` 列 | ✅ `varchar(64)`，可空 | §1（迁移自检）/ §4（生产库核对） |
| ② | 上传一个新附件 → `uploader_name` 有值（非 NULL、非 UUID） | ✅ 落库 `超级管理员（改名后）` | §5（HTTP 上传后 API + DB 核对） |
| ③ | 存量 backfill 后空值数 = 0 | ✅ 演练库 5+5 行、生产库 68 行全部回填 | §2 / §3 |
| ④ | 附件列表 API 返回 `uploader_name` | ✅ 每行带该字段，值为姓名 | §5（列表 API 响应） |
| ⑤ | 用户改名后旧附件显示**改名前的名字** | ✅ 快照优先于实时 join | §4（SQL 对照）/ §5（API 对照） |
| ⑥ | commit + 汇报 | ✅ | 见提交记录 |

## 改动清单

| 文件 | 作用 |
|------|------|
| `backend/migrations/business/V0352__attachment_uploader_name.{up,down}.sql` | 加列 + 列注释 + 自检（幂等） |
| `backend/src/business/shared/user_name.rs` | **姓名的唯一真相**：`snapshot_name()`（写）+ `display_name()`（纯函数真值表） |
| `backend/src/business/attachment/model.rs` | `insert_with_uploader()`（生产入口，带姓名）、`insert()` 收为 `#[cfg(test)]` 夹具专用；读列改「快照优先」三段回落 |
| `backend/src/business/attachment/handler.rs` | 上传时 `snapshot_name(claims.user_id)` 落库 |
| `backend/src/business/task/task_model_ai.rs` | `bind_ai_attachment` / `bind_human_attachment` 增 `uploader_name` 参数；**复活分支同时更新 `create_by` 与姓名** |
| `backend/src/business/task/task_handler_attachment.rs`、`hermes_sync.rs` | 两条上传路径（人 / AI）都取一次快照 |
| `backend/src/business/task/collab.rs` | 任务附件读列改「快照优先」三段回落 |
| `backend/src/business/attachment/tests.rs` | +1 真库用例：快照 / 改名 / 历史行回落 / 单条读 |
| `backend/src/business/task/task_attachment_snapshot_tests.rs` | +1 真库用例：任务附件写入 / 改名 / 历史行 / **复活** |
| `scripts/datafix/aud01/` | 一次性回填：`README.md` / `10_precheck.sql` / `backfill_attachment_uploader_name.sql` / `20_verify.sql` / `apply.sh`（`--check` / `--rollback`） |
| `scripts/verify/aud01_uploader_name_snapshot.py` | HTTP 层真机证据（②④⑤） |

### 读路径的三段回落（两处读列常量逐字一致）

```sql
COALESCE(NULLIF(a.uploader_name, ''), NULLIF(u.real_name, ''), u.username, '')
--       ① 写入时的快照              ② 历史行回落实时 join      ③ 兜底空串（前端显示 -）
```

「先上代码、后跑回填」与「先回填、后上代码」两种顺序都不会在界面上闪出 UUID。

## §1 迁移（应用启动链自动执行）

隔离实例首次启动（空库，`cp_aud01_e2e`）→ `数据库迁移完成: 应用 285 个迁移`，
其中 V0352 打出：

```
NOTICE:  V0352 完成：biz_doc_attachment / biz_task_attachment 各 1 列 uploader_name varchar(64) 就绪
```

重复应用（幂等）只打 `already exists, skipping`，自检仍通过。

## §2 回填演练（`cloudpivot_aud01b`，5+5 条历史行覆盖四类情形）

回填前体检（`apply.sh --check`）：

```
 tbl                 | total_rows | null_rows | filled_rows
 biz_doc_attachment  |          5 |         5 |           0
 biz_task_attachment |          5 |         5 |           0
--- 空值分布
 biz_doc_attachment  | no_uploader_id 1 | live_user 2 | soft_deleted_user 2 | user_row_missing 0
 biz_task_attachment | no_uploader_id 1 | live_user 2 | soft_deleted_user 2 | user_row_missing 0
```

第一遍回填：

```
NOTICE:  AUD-01 / biz_doc_attachment：实名 2 行，已删 2 行，账号不存在 0 行，无上传人 1 行
NOTICE:  AUD-01 / biz_task_attachment：实名 2 行，已删 2 行，账号不存在 0 行，无上传人 1 行
NOTICE:  AUD-01 回填完成：两张表 uploader_name 空值数均为 0
 distribution: (已删除用户) 2 / (未记录上传人) 1 / 张三 1 / aud01_noname 1
```

第二遍（幂等）：`实名 0 行，已删 0 行，账号不存在 0 行，无上传人 0 行`。

回填后验收（`apply.sh --verify`）：

```
② null_rows: doc 0 / task 0      empty_string_rows: 0 / 0
③ UUID 形态脏值: 0 / 0
④ wrong_deleted_label: 0 / 0     wrong_no_uploader_label: 0 / 0
```

改名对照（把 `张三` 改成 `张三丰`，行不动）：

```
 tbl  | snapshot_value | live_value
 doc  | 张三           | 张三丰
 task | 张三           | 张三丰
--- 列表读 SQL 的实际取值（快照优先）
 doc  | 张三
 task | 张三
```

## §3 生产库（144 `cloudpivot`）回填

迁移脚本与回填脚本经 `su - postgres -c psql -f` 执行（幂等）：

```
NOTICE:  V0352 完成：两张表各 1 列 uploader_name varchar(64) 就绪
NOTICE:  AUD-01 / biz_doc_attachment：实名 0 行，已删 0 行，账号不存在 0 行，无上传人 0 行
NOTICE:  AUD-01 / biz_task_attachment：实名 67 行，已删 1 行，账号不存在 0 行，无上传人 0 行
NOTICE:  AUD-01 回填完成：两张表 uploader_name 空值数均为 0
分布：老六测试号 40 / 验证AI 26 / 超级管理员 1 / (已删除用户) 1
台账：biz_task_attachment 68 行（before/after 两列都在，可精确回滚）
```

生产复核（`20_verify.sql`）：

```
② biz_doc_attachment 0 行（该表本来为空）/ biz_task_attachment 68 行，null_rows = 0，empty_string_rows = 0
③ UUID 形态脏值 0 / 0      ④ wrong_* 标记 0 / 0
⑤ 抽样：01a19022… = 老六测试号、…（均为姓名）
```

> 说明：生产**只**做了「加列 + 回填」这两步（扩列不改行为，旧二进制忽略新列）；
> 新代码随下一次发布生效，届时读路径自动切到「快照优先」。

## §4 自动化测试

```bash
# 纯函数（口径真值表 + 回填脚本占位串一致）
cargo test --lib business::shared::user_name        → 2 passed / 0 failed

# 单据附件（含新增真库用例 db_uploader_name_is_snapshot_and_survives_rename）
ERP_DB_URL=…/cloudpivot_aud01b cargo test --lib business::attachment:: -- --test-threads=1
                                                    → 86 passed / 0 failed

# 任务模块（含新增真库用例 db_task_attachment_uploader_name_is_snapshot）
ERP_DB_URL=…/cloudpivot_aud01b TASK_DB_URL=同 cargo test --lib business::task:: -- --test-threads=1
                                                    → 240 passed / 0 failed
```

### 全量 `cargo test --lib`（跑到 1792 / 3830 条时终止）

在共享工作区（同一目录里还有其它 worker **未提交**的 `erp/*` 改动）与复用库
`cloudpivot_aud01b` 上跑过一遍：前 1792 条里 **3 条 ERP PDF 真库用例 FAILED** ——
`business::erp::report_pdf::tests::pdf07_real_three_reports_agree_on_shape`、
`business::erp::return_pdf::db_tests::{purchase,sales}_return_context_has_source_numbers_and_reason`。

判定**与本卡无关**：① 这三个用例都在 `erp::report_pdf` / `erp::return_pdf`，本卡 diff 只碰
`business/{attachment,task,shared}`；② 它们断言「借贷必须平 / 上下文数据齐全」，对库的
数据新鲜度极敏感（仓库为此专门有 `scripts/verify/make_full_suite_db.sh` 新建库的流程），
而本库此前已跑过 300+ 条会写夹具的用例；③ 工作区当时带有他人未提交的 `opening.rs` /
`asset_*` 等 ERP 改动。

**本卡涉及的两个模块是真跑绿的**（§4：attachment 86 / task 240）。要一次全绿，需在
`make_full_suite_db.sh` 新建的库上跑完整套件。

## §5 HTTP 真机证据
隔离实例（`cp_aud01_e2e`，285 迁移全过）+ `p14_attachment_e2e.py`（38/38 通过）造夹具，
再跑 `scripts/verify/aud01_uploader_name_snapshot.py`：

```
· 登录成功
== ④ 列表 API 每行带 uploader_name（存量行）==
[PASS] 响应字段里有 uploader_name
[PASS] uploader_name 有值：'超级管理员'
== ⑤ 上传人改名 → 旧附件仍显示改名前的名字 ==
[PASS] 改名后旧附件仍显示 '超级管理员' ——快照生效
[PASS] sys_user 里的现值确实已改成新名字（对照）
== ② 改名后新上传一个附件 → 快照取的是新名字 ==
[PASS] 上传成功（HTTP 200）
[PASS] 新附件显示上传当时的新名字 '超级管理员（改名后）'
[PASS] uploader_name 不是 UUID（长度 10）
全部通过
```

DB 侧核对（上传由 API 真实完成，`uploader_name` 直接落库）：

```
 doc_type       | file_role    | uploader_name | uploaded_by
 CONTRACT       | CONTRACT_SCAN| 超级管理员     | 01a1bc76-…
 PURCHASE_ORDER | SIGN_RECEIPT | 超级管理员     | 01a1bc76-…
 STOCK_IN       | SIGN_RECEIPT | 超级管理员     | 01a1bc76-…
 PAYMENT        | INVOICE_SCAN | 超级管理员     | 01a1bc76-…
```

## 上线注意（下一次发布要做的事）

1. V0352 已在 144 手工应用，发布时启动链会再跑一遍（`IF NOT EXISTS`，幂等），并补记
   `schema_migrations`。
2. 回填已跑完（68 行）；**不需要**再跑一次。若未来从更老的备份恢复，按
   `scripts/datafix/aud01/README.md` 重跑（幂等）。
3. 前端**无需改动**：`DocAttachmentPanel.vue` / 任务详情页早已渲染 `uploader_name`（ATT-01 / ATT-03 接入）。

## 未纳入本卡（按卡面留给 AUD-02）

`biz_contract.owner_id` / `biz_commission_settlement.salesperson_id` 属「当前负责人 / 业务员」，
语义上是**实时展示**（换人后显示新人是正确的），本卡不动；留 AUD-02 判断。
