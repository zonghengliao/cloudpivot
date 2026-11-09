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
| ⑤ | 前端显示正确 | 前端零改动：`DocAttachmentPanel.vue` 全部读 `item.file_name`（列表 / 下载保存名 / 删除确认文案 / 分组标题），后端把该字段改对即正确；`pnpm typecheck && pnpm build` 见提交说明 |
| ⑥ | cargo test + 前端 build + push | `unit_attachment_db.txt`（95 passed，含真库）、`full_suite_nodb.txt`（3932 passed / 0 failed）、`migration_roundtrip.txt`（18 PASS / 0 FAIL） |

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
