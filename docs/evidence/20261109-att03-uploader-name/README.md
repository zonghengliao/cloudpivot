# ATT-03 取证：文件列表 / 详情补 `uploader_name`

卡：`t_761295b2`（父卡 ATT-01 `t_68ac5070`）。症状：附件预览右侧「文件信息」的上传人显示
UUID（如 `075c3b0e-…`），而单据附件面板同一套预览组件显示真名 —— 两处不一致。

## 1. 改了什么

| 文件 | 改动 |
|------|------|
| `backend/src/support/file/model.rs` | 新增 `file_select()` 唯一列清单（`SELECT f.*, COALESCE(NULLIF(u.real_name,''), u.username, '') AS uploader_name FROM sys_file f LEFT JOIN sys_user u ON u.id = f.uploaded_by`）、`FileRow`（模型 + 显示名）、`build_page_query()`（纯函数，便于单测）、`find_file_with_uploader()`（详情）、`uploader_display_name()`（上传 / 秒传 / 秒传预检）；`page_files` 改为返回 `Page<FileRow>`；6 条 SQL 片段单测 |
| `backend/src/support/file/dto.rs` | `FileRes` 增加 `uploader_name: String`；`From<&FileRow>` / `From<(&SysFileModel, String)>` 两个构造入口 |
| `backend/src/support/file/handler.rs` | `detail` 走 `find_file_with_uploader`；上传 / 秒传 / 秒传预检用 `uploader_display_name` 补齐（秒传按**记录里的** `uploaded_by` 取名，不拿当前操作者顶替） |
| `frontend/src/api/support/file-api.ts` | `FileRes.uploader_name?: string` |
| `frontend/src/views/support/file/uploader-label.ts` | 新增纯函数 `uploaderLabel()`（有名字→名字；空串→`-`；字段缺失→回退 ID） |
| `frontend/src/views/support/file/index.vue` | 「上传人」列改 `colKey: 'uploader_name'` + cell 模板（带 `data-testid="file-uploader-name"`）；预览 `items.uploader` 走 `uploaderLabel` |
| `frontend/tests/unit/file-uploader-label.test.ts` | 5 条单测（三条分支 + 视图接线 + API 契约） |
| `frontend/e2e/39-file-uploader-name.spec.ts` | 3 条真实浏览器 E2E（列表 / 预览 / 脏数据） |

关键口径（写进代码注释）：

1. **LEFT JOIN，不是 INNER**：`uploaded_by` 指向不存在用户时，历史行**仍要出现**在列表里；
2. **不过滤 `u.deleted_at`**：用户被软删后，历史文件仍显示「当时是谁上传」；
3. `COALESCE` 兜**空串**（不是 NULL）：接口不因脏数据 500，前端显示 `-`；
4. JOIN 在 `{FILTER}` **之前**，`u.id = f.uploaded_by` 一对一 → 计数与排序口径不变（单测 + 实测双证）。

## 2. 怎么验的（全部原始输出在本目录）

### 2.1 后端单测

```bash
cd backend && cargo test -j 1 --lib support::file:: -- --test-threads=1
# test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 3817 filtered out
```

6 条断言：LEFT JOIN 形态、`real_name → username → ''` 兜底链、join 版与单用户版表达式逐字一致、
`{FILTER}` 恰一次且在业务参数之前、无过滤时空参数、详情复用同一列清单。

### 2.2 前端

```bash
pnpm exec tsx --test tests/unit/file-uploader-label.test.ts   # 5 passed
pnpm typecheck                                                # EXIT=0
pnpm build                                                    # EXIT=0
```

### 2.3 145 真实 token curl（隔离实例 18229 + `cloudpivot_dev` 拷贝库）

```bash
bash /root/att03/10_prepare_145.sh    # pull + cargo build + 拷贝库 cloudpivot_att03 + 起 18229
bash /root/att03/20_curl.sh           # 22 条断言
# === 汇总：22 passed, 0 failed ===
```

覆盖：上传响应 / 列表 / 详情的 `uploader_name` = `超级管理员`；列表 total == 库内未删行数（14）；
排序仍 `created_at DESC`；`real_name` 清空 → 回落 `admin`；`uploaded_by` 指向幽灵 UUID → 200 + 空串 +
行仍在；`uploaded_by = NULL` → 200 + 空串；复原后回到真名；无 token → 401。原始输出：

```
$ bash /root/att03/20_curl.sh
=== ATT-03 curl 2026-11-09 01:58:37 db=cloudpivot_att03 base=http://127.0.0.1:18229 ===
admin id=01a17c3b-4f78-77c3-bbc5-1c1c2a412195 期望显示名=[超级管理员]
[PASS] 上传接口 code=0
[PASS] ① 上传响应 uploader_name = admin 显示名
[PASS] 上传响应 uploaded_by 仍是用户 ID（字段保留）
[PASS] 列表接口 code=0
[PASS] 列表按文件名过滤命中 1 行
[PASS] ① 列表行 uploader_name = admin 显示名
[PASS] 列表 uploader_name 与 UUID 不同（不再把 ID 当名字）
[PASS] 详情 HTTP 200
[PASS] ② 详情 uploader_name = admin 显示名
[PASS] join 未放大计数：API total == 库内未删行数（14）
[PASS] 排序仍是 created_at DESC（join 未打乱）
[PASS] real_name 为空时回落 username（[admin]）
[PASS] real_name 复原后 uploader_name 回到真名
[PASS] ③ 指向不存在用户的 uploaded_by：详情仍是 200（不报错）
[PASS] ③ 指向不存在用户时 uploader_name 为空串（前端显示 '-'）
[PASS] ③ uploaded_by 原样返回（脏数据不丢信息）
[PASS] ③ 脏数据行在列表里仍然出现（LEFT JOIN 没把行过滤掉）
[PASS] ③ 列表里 uploader_name 同样为空串
[PASS] uploaded_by=NULL：详情 200
[PASS] uploaded_by=NULL：uploader_name 空串
[PASS] 恢复后 uploader_name 回到 admin 显示名
[PASS] 无 token 访问列表 → 401
=== 汇总：22 passed, 0 failed ===
```

### 2.4 真实浏览器 E2E（`39-file-uploader-name.spec.ts`，vite :5189 反代 145:18229）

```bash
BACKEND_ORIGIN=http://127.0.0.1:18229 ./node_modules/.bin/vite --port 5189 --strictPort
E2E_USERNAME=admin E2E_PASSWORD=*** E2E_RUN=att03ui1794160600 \
E2E_DIRTY_FILE_ID=01a1bc4d-e249-7157-ba4e-e03dae385bc7 \
  pnpm exec playwright test e2e/39-file-uploader-name.spec.ts
# 3 passed (4.5s)
```

原始输出：

```
$ pnpm exec playwright test e2e/39-file-uploader-name.spec.ts
[e2e-target] OK 隔离实例：http://127.0.0.1:5189（回环 + 隔离端口 5189）

Running 3 tests using 1 worker

  ✓  1 [chromium] › e2e/39-file-uploader-name.spec.ts:133:1 › 列表：上传人列显示显示名（不是 UUID） (1.2s)
  ✓  2 [chromium] › e2e/39-file-uploader-name.spec.ts:139:1 › 预览：右侧文件信息的上传人同口径 (1.3s)
  ✓  3 [chromium] › e2e/39-file-uploader-name.spec.ts:157:1 › 脏数据：uploaded_by 指向不存在用户时上传人显示 "-" (1.2s)

  3 passed (4.5s)
```

截图（列表 / 预览 / 脏数据）见任务交付 artifacts。
脏数据夹具由 `30_dirty_fixture.sh` 现造：真上传一张 PNG → `UPDATE sys_file SET uploaded_by='<幽灵 UUID>'`
（没有任何接口能造这种数据，只能在拷贝库里改）。

## 3. 已知与边界（如实记录）

- **`cargo clippy --lib -- -D warnings` 与 `cargo fmt --check` 当前基线是红的**，红点全在别人的文件里：
  `business/erp/invoice.rs:1967`（`explicit lifetimes`）、`receivable.rs:800`（doc list indentation）、
  `sales_order_model.rs:699`（8/7 args）、`business/erp/report.rs`（`cargo fmt` 换行）——
  这四个文件 `git status --porcelain` 为空（不是本卡改动），属并行卡（PDF-07 / INV-01 / SO-40 …）遗留。
  本卡改动的 5 个文件无任何 clippy / fmt 告警（`rustfmt +1.94.1 --edition 2024 --check` 三项全过）。
- **拷贝库要 `-T schema_migrations_att05_bak`**：`cloudpivot_dev` 里有一张 postgres 属主的备份表，
  应用账号连 `LOCK` 都没有，`pg_dump` 会整库失败（第一版就是这么把「拷贝库」跑成空库的，已在脚本里
  `set -o pipefail` + 失败即 exit 修掉）。
- **隔离实例与拷贝库留在 145**：`/root/att03/`（`instance.pid` / `server.log` / 脚本），库名
  `cloudpivot_att03`。重跑 `bash /root/att03/10_prepare_145.sh && bash /root/att03/20_curl.sh` 可复现。
- 未部署、未碰 144 生产；8033（cloudpivot-test 服役实例）全程只有只读的 `pg_dump` 访问。
