# E2E07-FIX-3 证据：附件秒传复用导致文件名错位（P3，审计隐患）

卡：`t_b30fb5be`。原缺陷：`sys_file` 按内容 hash **秒传复用**（同字节共用一条记录），
`biz_doc_attachment` 只存 `file_id`，列表一律读 `sys_file.original_name` ——
于是「B 上传的扫描件」显示「A 第一次上传时的发票名」。审计会据此认错材料。

修法：给**绑定行**加文件名快照（迁移 `V0362` `biz_doc_attachment.file_name`），
读路径 `COALESCE(NULLIF(a.file_name,''), f.original_name, '')`：快照优先、历史行回落。

## 验收对照

| # | 卡面验收 | 证据 |
|---|---------|------|
| ① | 迁移后 `biz_doc_attachment` 有 `file_name` 列 | `local_fresh_db.txt`（全新库 284/284 迁移，含 V0362 自检）；`migration_roundtrip.txt`（列 varchar(255)、DO 自检 NOTICE） |
| ② | 存量数据回填（空值为 0） | `local_fresh_db.txt` 的 V0362 自检；`migration_roundtrip.txt`：3 条无名字的存量行回填后空值 0、`original_name` 为空的行回落对象键 basename |
| ③ | 秒传场景两个不同名的同内容文件各显其名 | `unit_attachment_db.txt` 的 `db_file_name_snapshot_survives_seconds_upload`（同一 `file_id`、两个 `file_name`）；`live-145.txt`（真机） |
| ④ | 列表 / 详情 API 返回 `file_name` | 同上 + `live-145.txt`（`GET /business/attachment/list`） |
| ⑤ | 前端显示正确 | 前端零改动：`DocAttachmentPanel.vue` 全部读 `item.file_name`（列表 / 下载保存名 / 删除确认文案 / 分组标题），后端把该字段改对即正确；`pnpm typecheck && pnpm build` → `frontend.txt`（两条 rc=0） |
| ⑥ | cargo test + 前端 build + push | `unit_attachment_db.txt`（95 passed，含真库）、`full_suite_nodb.txt`（3932 passed / 0 failed）、`migration_roundtrip.txt`（18 PASS / 0 FAIL）、`frontend.txt`（typecheck_rc=0 / build_rc=0） |

## 145 真机证据（`live-145.txt` / `live-145-db.txt`）

145 上起隔离实例（debug 二进制 `029167e8`，端口 8033，库 `cp_fix3_probe` = **全新空库**，
迁移链 284/284 从零跑通、版本 V362、V0362 在册），真 token 真 HTTP：

```
================ PASS=28 FAIL=0 ================
```

关键几行：

```
✅ 上传 A 的显示名 = e2e07-contract-scan-1109170748.pdf
✅ 上传 B 的显示名（旧实现会显示 A 的名字） = drill-invoice-1109170748.pdf
✅ 秒传命中（duplicated） = true
✅ 两条绑定共用同一个 sys_file（file_id 相同） = 01a1bf90-516e-…
✅ 合同 B 列表 file_name（原 bug：会是 A 的名字） = drill-invoice-1109170748.pdf
✅ 置空后列表回落到 sys_file.original_name（= A 的名字，正是原 bug 的表现）
✅ 超长文件名 HTTP/CODE = 400/c10001
```

库内（`live-145-db.txt`，两轮探针各一对）：同一个 `file_id`、两个不同的 `file_name`。

> 说明：145 上 `/etc/cloudpivot/cloudpivot-dev.env` 指向的 `cloudpivot_dev` 库已不存在
> （现在只有 `cloudpivot` / `cloudpivot_e2e_iso` / `cloudpivot_fix10_demo` / `cp_e2efix3`），
> 所以探针改用 `cp_fix3_probe` 空库 + `/tmp/fix3_probe.env`（主库 `cloudpivot` 停在 V0353、
> `biz_doc_attachment` 0 行，没有可回填的存量）。主服务（release，8023）未重启 —— 本卡只要求
> 真机实测留证，正式切版走 `scripts/deploy/deploy.sh`。副本库那条路试过并被一个与该库自身
> 数据有关的老问题挡住（V0359 撞 `uk_biz_flow_def_version`，主库数据里已有 v2 流程定义），
> 与本卡无关，已换空库路线。

## 有牙齿的证明（`teeth.txt`）

把 `COLUMNS` 读路径换回旧口径（只读 `sys_file.original_name`）后重跑：

```
test business::attachment::tests::db_file_name_snapshot_survives_seconds_upload ... FAILED
test business::attachment::tests::file_name_snapshot_is_read_before_sys_file_name ... FAILED
```

失败信息正是 bug 本身：「合同这侧显示 A 传的名字（绝不能是 sys_file 的名字）」。
还原后 95 条全绿 —— 说明这两条守得住（其它真库用例在没有秒传的夹具上仍然会绿，
只有这两条会红，所以必须有它们）。

## 文件

- `backend/migrations/business/V0362__doc_attachment_file_name.{up,down}.sql`
- `backend/src/business/attachment/model.rs`（`COLUMNS` / `display_name` / `insert_with_uploader`）
- `backend/src/business/attachment/dto.rs`（`normalize_file_name` / `FILE_NAME_MAX_CHARS`）
- `backend/src/business/attachment/handler.rs`（上传写快照、响应与留痕同源）
- `backend/src/business/attachment/tests.rs`（3 条纯逻辑 + 1 条真库 + 2 条漂移守卫）
- `scripts/verify/e2e07_fix3_attachment_name_probe.sh`（145 真机探针）
