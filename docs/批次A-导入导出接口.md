# 批次 A · 导入导出接口契约（A3 + A4 合并后的最终口径）

> 代码：`backend/src/business/impexp/`
> 迁移：`V0041__create_impexp_task`（`sys_import_task` / `sys_export_task`）、`V0042__create_impexp_error_detail`（`sys_import_task_error`）、`V0052__seed_impexp_export_retry`（补 `export:retry` 权限点）
> 依据：`docs/批次A-技术架构.md` §3、`docs/批次A-产品需求PRD.md` 第四章
> 本文件在「批次 A 遗留修复」中已按实现校正（下载路径别名、权限码、取消 / 重试 / 异步导出），差异见文末「§7 变更记录」。

前缀：`/api/business/impexp`（`[router].prefix` 注入，宏内写 `/business/impexp/...`）。
所有接口要求 `Authorization: Bearer <token>`，权限码见每行；错误统一返回 `{code, msg}`，`code` 为 `shared/error_code.rs` 的业务码。

## 1. 接口清单

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| GET | `/def/list` | `business:impexp:def:list` | 已注册的导入 / 导出定义（前端下拉） |
| GET | `/import/template?biz_type=` | `business:impexp:import:template` | 下载导入模板（xlsx，三要素） |
| POST | `/import/upload?biz_type=&mode=` | `business:impexp:import:upload` | 上传并**同步预检**（multipart 字段名 `file`），返回任务与报告 |
| GET | `/import/page` | `business:impexp:import:list` | 导入任务分页（默认只看自己创建的任务） |
| GET | `/import/{id}` | `business:impexp:import:list` | 任务详情（含前 100 条错误摘要） |
| GET | `/import/{id}/errors` | `business:impexp:import:list` | 错误明细分页（行号 / 列名 / 原因） |
| GET | `/import/{id}/errors/file` | `business:impexp:import:download` | 下载「错误行」xlsx（行号 / 列名 / 原因；**推荐路径**） |
| GET | `/import/{id}/download` | `business:impexp:import:download` | 同上（**文档别名**，与上一条同一实现、同一权限码，见 §2.5） |
| POST | `/import/{id}/confirm` | `business:impexp:import:confirm` | 确认入库（按批事务） |
| POST | `/import/{id}/cancel` | `business:impexp:import:cancel` | 取消（`PENDING`/`PARSING`/`VALIDATED` 直接取消；`IMPORTING` 打标记，批次边界中止） |
| POST | `/import/{id}/retry` | `business:impexp:import:retry` | 失败重试（**新建任务**，原任务保留做审计） |
| GET | `/export/sync?biz_type=&keyword=&status=` | `business:impexp:export:create` | 同步导出（≤ 定义 `sync_max_rows`，默认 10 000 行） |
| POST | `/export` | `business:impexp:export:create` | 创建**异步导出**任务（`{biz_type, keyword?, status?, format?, export_name?}`） |
| GET | `/export/page` | `business:impexp:export:list` | 导出任务分页 |
| GET | `/export/{id}` | `business:impexp:export:list` | 导出任务详情（进度 / 产物） |
| GET | `/export/{id}/download` | `business:impexp:export:download` | 下载产物（未完成 409 / 过期 410） |
| POST | `/export/{id}/cancel` | `business:impexp:export:cancel` | 取消（`PENDING` 直接取消；`RUNNING` 打标记，分块边界中止） |
| POST | `/export/{id}/retry` | `business:impexp:export:retry` | 重试（仅 `FAILED` / `CANCELED` / 已过期；复用同一任务行） |
| DELETE | `/export/{id}` | `business:impexp:export:delete` | 删除任务与产物（`RUNNING` 拒绝，先取消） |

> 任务书里出现过的「泛型」写法 `POST /business/impexp/{task_id}/cancel` **不提供**：导入与导出是两张表、两套状态机，
> 泛型路径还得再传类型判别参数，比 `/import/{id}/cancel` / `/export/{id}/cancel` 更容易用错；架构文档 §3.8、
> 后端注解、前端 `api/business/impexp/index.ts` 三处用的都是带类型前缀的写法，保持一致。

## 2. 关键约定

### 2.1 两步式：预检 → 确认（绝不「上传即入库」）

```
upload ──▶ sys_import_task(PENDING) ──▶ 解析 + 逐行校验 ──▶ VALIDATED
                                                          │
                                             confirm ─────┘──▶ IMPORTING ──▶ SUCCESS / PARTIAL / FAILED
```

- `upload` 只写任务行与错误明细，**不碰业务表**；
- `confirm` **重新读盘、重新校验**（文件可能被替换、字典 / 外键可能变化），并核对 `file_sha256`。

### 2.2 事务边界（架构 §3.4）

| 模式 | 语义 |
|------|------|
| `STRICT`（默认） | 任一行校验失败 → 整批不入库（`422 / 60014`）；入库阶段某批失败 → 终止后续批次，`message` 写明「第 n 批（行 x–y）失败，前 m 行已入库」 |
| `PARTIAL` | 跳过失败批次继续，最终 `PARTIAL`（需定义实现 `allow_partial()`，本批内置定义**未开启**） |

批大小由定义 `batch_size()` 决定（内置 `system.dict_item` 为 2 000 行/事务）。

### 2.3 行号

全部错误的行号是 **Excel 里看到的真实行号**（第 1 行是表头，从 1 开始），与 PRD §4.2.2 一致。
第 2 行若与模板示例值完全一致会被自动识别并跳过（`skipped_example` 计数）。

### 2.4 外键批量校验

外键列取值先去重聚合，再**每列一条 SQL**：

```sql
SELECT DISTINCT <col>::text FROM <table>
WHERE <col>::text IN (SELECT jsonb_array_elements_text(?::jsonb)) [AND deleted_at IS NULL]
```

10 万行 × 1 个外键列 = **1 条 SQL**（不是 10 万条）。执行条数写入预检报告的 `fk_sql_statements`，
并以 `tracing::info!` 同时落到日志，可与 PG 日志交叉验证。

### 2.5 下载路径（两个都可用，同一实现）

历史原因：架构文档 §3.8 与本文档早期把导入下载写成 `/import/{id}/download`，A3 实现时落成了语义更精确的
`/import/{id}/errors/file`，A7 前端两种写法都出现过 → 出现「按文档路径 404」。

最终口径：**两个都提供**（同一 handler、同一权限码 `business:impexp:import:download`）：

| 路径 | 定位 |
|------|------|
| `GET /import/{id}/errors/file` | 推荐路径（语义精确：下的是错误行明细） |
| `GET /import/{id}/download` | 文档别名（对外承诺的路径，已按文档接入的调用方无需改代码） |

导出的下载只有 `GET /export/{id}/download`（文档与实现一直一致）。

### 2.6 取消 / 重试 / 异步导出（A4 落地，遗留修复⑤实测确认）

| 动作 | 接口 | 合法前置状态 | 非法状态返回 |
|------|------|--------------|--------------|
| 取消导入 | `POST /import/{id}/cancel` | `PENDING`/`PARSING`/`VALIDATED`（直接 CANCELED）、`IMPORTING`（打标记） | `409 + 60013` |
| 重试导入 | `POST /import/{id}/retry` | `FAILED` / `CANCELED` / `PARTIAL` → **新建任务** | `409 + 60013` |
| 建异步导出 | `POST /export` | — | 定义未注册 `404 + 60011` |
| 取消导出 | `POST /export/{id}/cancel` | `PENDING`（直接 CANCELED）、`RUNNING`（打标记） | `409 + 60032` |
| 重试导出 | `POST /export/{id}/retry` | `FAILED` / `CANCELED` / 已过期（复用同一任务行） | `409 + 60032` |

> 遗留修复⑤ 的真问题不在接口，而在**权限种子**：`business:impexp:export:retry` 是 A4 才加的注解权限码，
> `V0047` 只种了 13 个 impexp 权限点、不含它 → 非超管角色点「重试导出」必然 403。
> `V0052` 已补种（按钮级菜单点 + 授 SUPER_ADMIN + 授其余所有角色），前端 `PERM.impexpExportRetry` 与导出任务页按钮同步补齐。

## 3. 上传 + 预检响应

```json
{
  "code": 0,
  "msg": "ok",
  "data": {
    "task_id": "01a18266-e1ec-77b7-8537-9d3249158a1e",
    "task_no": "IMP20261028-8A1E",
    "status": "VALIDATED",
    "report": {
      "total": 5000,
      "valid": 4995,
      "error": 5,
      "skipped_example": 0,
      "skipped_blank": 0,
      "warnings": [{"code": "E-IMP-04", "message": "文件中有 2 个不属于本模板的列（备注1、备注2），这些列将被忽略"}],
      "errors": [
        {"row_no": 12, "column_key": "item_label", "column_header": "字典项名称",
         "raw_value": "", "kind": "REQUIRED", "message": "必填项为空"},
        {"row_no": 43, "column_key": "dict_type_code", "column_header": "字典编码",
         "raw_value": "user_status/…", "kind": "UNIQUE", "message": "字典编码已存在（与第 42 行重复）"}
      ],
      "truncated": false,
      "fk_sql_statements": 1,
      "elapsed_ms": 195
    }
  }
}
```

`errors` 最多 2 000 条（`truncated=true` 表示已截断），全量明细落 `sys_import_task_error`，可 `/errors` 分页或 `/errors/file` 下载。

## 4. 错误码（`shared/error_code.rs` 6000x 段）

| 码 | 常量 | HTTP | 场景 |
|----|------|------|------|
| 60011 | `IMPORT_DEF_NOT_FOUND` | 404 | `biz_type` 未注册 |
| 60012 | `IMPORT_ROW_LIMIT` | 422 | 超过定义 `max_rows` |
| 60013 | `IMPORT_STATUS_INVALID` | 409 | 状态机非法跃迁（如对 `SUCCESS` 再 confirm） |
| 60014 | `IMPORT_BATCH_FAILED` | 422 | 未开部分导入但存在失败行 / 某批入库失败 |
| 60015 | `EXPORT_NOT_READY` | 409 | 任务未完 / 失败时请求下载，或超出同步导出上限 |
| 60016 | `EXPORT_EXPIRED` | 410 | 产物过期 |
| 60020 | `IMPORT_FILE_INVALID` | 400/422 | 魔数不符 / 无法解析 / 只有表头 / 表头缺必需列 / 没有数据行 |
| 60032 | `EXPORT_STATUS_INVALID` | 409 | 导出任务状态机非法跃迁（取消 / 重试 / 删除） |
| 60033 | `EXPORT_FILE_MISSING` | 410 | 产物文件已被清理（记录在库但文件没了） |

行级错误码（`sys_import_task_error.error_code`）：
`REQUIRED` / `TYPE` / `LENGTH` / `ENUM` / `RANGE` / `DATE` / `DICT` / `UNIQUE` / `FK_NOT_FOUND` / `RULE` / `ROW_LIMIT` / `STRUCTURE`。

## 5. 接入一个新业务定义

```rust
// 1. 定义列（表头、必填、类型、唯一、外键、示例、说明）
static COLUMNS: &[ImportColumn] = &[ /* ... */ ];

// 2. 实现 trait（只能写只读底座表 + 自己的业务表）
pub struct MaterialImport;

#[async_trait]
impl ImportDefinition for MaterialImport {
    fn biz_type(&self) -> &'static str { "erp.material" }
    fn biz_name(&self) -> &'static str { "物料" }
    fn columns(&self) -> &'static [ImportColumn] { COLUMNS }
    async fn persist(&self, tx: &Transaction, rows: &[RowValues], ctx: &ImportCtx)
        -> Result<usize, DbError> { /* INSERT ... ON CONFLICT ... */ Ok(rows.len()) }
}

// 3. 在 business/impexp/builtin/mod.rs::register 里挂一行
```

导出侧实现 `ExportDefinition`（`count` + keyset `fetch`）即可复用同步导出。

## 6. 内置定义：`system.dict_item`（字典项）

| 列 | 必填 | 类型 | 约束 |
|----|------|------|------|
| 字典编码 | 是 | 文本 ≤50 | 外键 → `sys_dict_type.dict_code`；参与组合唯一 |
| 字典项值 | 是 | 文本 ≤50 | 参与组合唯一 |
| 字典项名称 | 是 | 文本 ≤100 | |
| 描述 | 否 | 文本 ≤500 | |
| 排序号 | 否 | 整数 0–9999 | 空按 0 |
| 是否默认 | 否 | 布尔 | 空按「否」 |
| 状态 | 否 | 枚举 启用/停用 | 落库 `'1'/'0'`，空按启用 |

入库：`INSERT ... ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO UPDATE`（幂等重导）。

## 7. 变更记录（批次 A 遗留修复）

1. **下载路径**：补 `GET /import/{id}/download` 别名路由（以文档为准补齐），本文档同步标注「推荐路径 / 别名」，
   并给出「两个都保留」的理由（见 §2.5）。
2. **权限码统一**：架构文档 §3.8 原写 `business:impexp:import:download-error`，而实现、种子（`V0047`）、
   前端常量一直是 `business:impexp:import:download` → **以实现为准改文档**。理由：改码要同时动注解 / 种子 /
   前端三处，且会让已按 `import:download` 授权的角色失效；文档笔误的修正成本最低、风险为零。
3. **补 `business:impexp:export:retry` 权限种子**（`V0052`）与前端常量 / 按钮（见 §2.6）。
4. **补齐取消 / 重试 / 异步导出**接口与合法状态（原文只有一句「属 A4，本批不提供」）。
5. 错误码表补 `60032` / `60033`，并修正 60015 / 60016 / 60020 的场景描述。
