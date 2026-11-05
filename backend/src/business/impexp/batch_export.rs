//! 批量导出（D7）：按勾选的主键建一条**异步**导出任务（复用 impexp 的 worker / 产物落盘 / 下载）。
//!
//! # 为什么要异步
//!
//! 勾 5 张单子时同步也能出文件，但同一套按钮在「选中全部筛选结果」时可能是几万行：
//! 同步导出会把请求线程按在那儿几十秒（前端早就超时了）。所以批量导出**只建任务**，
//! 立即返回 `task_no`，前端提示「已提交导出任务，请到导入导出页下载」——
//! 与「导入导出」页的手动异步导出是**同一条链路**（同一个 worker，同一张 `sys_export_task`）。
//!
//! # 过滤条件的两种形态
//!
//! ```text
//! { "ids": ["..."] }                        → 只导勾中的这些行
//! { "ids": [], "keyword": "IN-2026", ... }  → 「选中全部筛选结果」：按筛选条件导
//! ```
//!
//! `ids` 与筛选条件是**并集之外**的关系：给了 `ids` 就只认 `ids`（筛选条件照旧会进快照，
//! 但不会再放宽范围）。行数在**建任务时**就按同一口径数一遍，0 行直接 422，
//! 不会出现「任务建成功了、跑完是个空文件」。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 本模块只写 sys_export_task（任务表，无 dept_id / 归属人列；
//! create_by 是发起人快照，不参与可见性过滤）。行级可见性由各业务导出定义自己负责
//! （`count_scoped` / `fetch_scoped`），任务本身的读回仍走 impexp 的现有入口。
//! ```

use crate::utils;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::impexp::definition::{self, ExportFilter};
use crate::business::impexp::model::{self, NewExportTask};
use crate::business::impexp::registry::global_registry;
use crate::business::impexp::worker;
use crate::shared::error_code;

/// 批量导出统一出 `XLSX`（勾选场景行数少，XLSX 对业务用户更友好）。
pub const FORMAT: &str = "XLSX";

/// 建任务成功后返回给前端的摘要（不再回读任务行：插入成功即存在）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchExportTask {
    /// 任务主键（前端可拿去跳详情）。
    pub id: String,
    /// 任务号（人看的那一个）。
    pub task_no: String,
    /// 业务编码。
    pub biz_type: String,
    /// 业务名称。
    pub biz_name: String,
    /// 导出文件名（不含扩展名）。
    pub export_name: String,
    /// 预计行数（建任务时按同一口径数出来的）。
    pub total: i64,
}

/// 按主键 / 筛选条件建一条异步导出任务。
///
/// # 参数
///
/// - `biz_type`：导出定义编码（必须已在注册表里）；
/// - `ids`：勾中的主键（空 = 按 `filter` 导「当前筛选结果」）；
/// - `filter`：筛选快照（`keyword` / `status` / `status_code`，会被原样写进任务，可追溯）；
/// - `operator`：发起人（落 `create_by`，导入导出页「只看自己的」靠它）；
/// - `export_name`：自定义文件名（空 = 「业务名_时间戳」）。
///
/// # 异常情况
///
/// - `404 + 60011`：`biz_type` 未注册；
/// - `422 + 10001`：状态过滤与定义不匹配（见 [`definition::check_export_filter`]）；
/// - `422`：命中 0 行（勾中的行都被删了 / 筛选条件太窄）；
/// - `500`：写任务行失败。
pub async fn create_by_ids(
    biz_type: &str,
    ids: &[Uuid],
    mut filter: ExportFilter,
    operator: Option<Uuid>,
    export_name: Option<&str>,
) -> std::result::Result<BatchExportTask, ApiError> {
    let biz_type = biz_type.trim();
    let definition = global_registry()
        .export_definition(biz_type)
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::IMPORT_DEF_NOT_FOUND,
                format!("导入导出功能「{biz_type}」未注册或已停用，请联系管理员"),
            )
        })?;
    filter.ids = ids.to_vec();
    // 卡 t_93cba3f4：批量导出同样过状态过滤契约（非法 `status_code` / 给 ERP 单据传
    // `status` → 422），别让「勾选后导出」成为绕过校验的后门。
    definition::check_export_filter(definition.as_ref(), &mut filter)?;

    // 建任务时就按**同一口径**数一遍：0 行直接 422，别让用户等到任务跑完才发现是空文件。
    let total = definition
        .count(&filter)
        .await
        .map_err(|error| utils::db_error(error, "统计批量导出行数"))?;
    if total <= 0 {
        return Err(ApiError::new(
            422,
            error_code::PARAM_INVALID,
            if ids.is_empty() {
                "当前筛选条件下没有可导出的数据"
            } else {
                "勾中的数据都已不存在（可能刚被删除），请刷新列表后重试"
            },
        ));
    }

    let id = Uuid::now_v7();
    let task_no = model::build_export_task_no(&id);
    let export_name = utils::trimmed(export_name)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}_{}",
                definition.biz_name(),
                chrono::Local::now().format("%Y%m%d_%H%M%S")
            )
        });
    // 快照：ids + 筛选条件。worker 用 `filter_from_params` 还原成 `ExportFilter`（含 ids）。
    let query_params = serde_json::json!({
        "keyword": filter.keyword,
        "status": filter.status,
        "status_code": filter.status_code,
        "ids": filter.ids.iter().map(Uuid::to_string).collect::<Vec<_>>(),
    })
    .to_string();
    let columns_snapshot = serde_json::to_string(
        &definition
            .columns()
            .iter()
            .map(|column| {
                serde_json::json!({
                    "key": column.key,
                    "header": column.header,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    model::insert_export_task(&NewExportTask {
        id,
        task_no: &task_no,
        biz_type: definition.biz_type(),
        biz_name: definition.biz_name(),
        export_name: &export_name,
        format: FORMAT,
        query_params: Some(&query_params),
        columns_snapshot: Some(&columns_snapshot),
        max_attempts: worker::DEFAULT_MAX_ATTEMPTS,
        operator,
    })
    .await
    .map_err(|error| utils::db_error(error, "创建批量导出任务"))?;

    tracing::info!(
        task_id = %id,
        task_no = %task_no,
        biz_type = definition.biz_type(),
        total = total,
        selected = ids.len(),
        "已创建批量导出任务"
    );
    Ok(BatchExportTask {
        id: id.to_string(),
        task_no,
        biz_type: definition.biz_type().to_string(),
        biz_name: definition.biz_name().to_string(),
        export_name,
        total,
    })
}
