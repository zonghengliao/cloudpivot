# 批次 A · 导入导出接口契约（A3）

> 代码：`backend/src/business/impexp/`
> 迁移：`V0041__create_impexp_task`（`sys_import_task` / `sys_export_task`）、`V0042__create_impexp_error_detail`（`sys_import_task_error`）
> 依据：`docs/批次A-技术架构.md` §3、`docs/批次A-产品需求PRD.md` 第四章

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
| GET | `/import/{id}/errors/file` | `business:impexp:import:download` | 下载「错误行」xlsx（行号 / 列名 / 原因） |
| POST | `/import/{id}/confirm` | `business:impexp:import:confirm` | 确认入库（按批事务） |
| GET | `/export/sync?biz_type=&keyword=&status=` | `business:impexp:export:create` | 同步导出（≤ 定义 `sync_max_rows`，默认 10 000 行） |

> 异步导出 / 取消 / 重试 / 过期清理属 **A4**，本批不提供。

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
| 60015 | `EXPORT_NOT_READY` | 409 | 超出同步导出上限（异步属 A4） |
| 60016 | `EXPORT_EXPIRED` | 410 | 产物过期（A4 使用） |
| 60020 | `IMPORT_FILE_INVALID` | 400/422 | 魔数不符 / 无法解析 / 只有表头 / 表头缺必需列 |

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
