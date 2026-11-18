//! 供应商准入 / 黑名单数据层（M4-14）。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0455__supplier_access.up.sql`。
//! 审批回调与内控在 [`crate::business::erp::purchase::supplier_access_flow`]，
//! 接口在 [`crate::business::erp::purchase::supplier_access_handler`]。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_supplier_access` | 一家供应商同时最多一张未结单 | 单号存活唯一（`SAC-yyyyMMdd-000001`）；`uk_biz_sa_supplier_open` 保证「同供应商只有一张 DRAFT/SUBMITTED」；提交后理由 ≥ 5 字；审批中/已通过必须有实例；`APPROVED` 与结论人/时间同生共死；等级快照只对黑名单有意义 |
//! | `biz_supplier.status` | 1 启用 / 0 停用 | **唯一强制点**：采购订单建单时 `status <> 1` → 409 + 66099 |
//!
//! # 三种申请单（起点判据 = 本文件的 [`check_start`]，单测逐条钉住）
//!
//! ```text
//! ADMIT     准入  ：未在黑名单中，且从未通过过准入（首次进合格名录）→ 通过后 status = 1
//! BLACKLIST 拉黑  ：未在黑名单中                                      → 通过后 status = 0
//! RELEASE   解除  ：正在黑名单中                                      → 通过后 status = 1
//! ```
//!
//! # 「是否在黑名单」为什么现算，不读回写列
//!
//! 判据是**单据链的真相**：取该供应商最近一张 `APPROVED` 的准入/黑名单/解除单，
//! `access_type` 是什么就是什么状态（`BLACKLIST` = 在名单里，其余 = 不在）。
//! 加一列 `is_blacklisted` 回写就多了一份要人工对齐的账 —— 单被撤回/红冲/补录历史时
//! 没有任何入口会把它改回来（与 V0449 执行率 / M4-09 视图同一条理由）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_supplier_access 与供应商档案 / 采购计划 / 框架协议同口径 —— 供应商准入是
//!         公司级采购文件（采购要看全公司、财务要复核），本表也没有 dept 归属列，
//!         所以不登记 shared::scope::SCOPED_TABLES，分页走 paginate_unscoped。
//! ```
//!
//! # 命名
//!
//! 表 `biz_supplier_access`、约束/索引前缀 `sa`、单号前缀 `SAC`（`biz_seq.seq_type='SAC'`）。
//! 动手前 grep 过：`sa_` 前缀、`SAC` 取号键、`biz_supplier_access` 表名全库无占用。

use nik::prelude::chrono::{DateTime, Datelike, Duration, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::business::shared::scope;

// ==================== 表名 / 取号 / 枚举值 ====================

/// 供应商准入 / 黑名单申请单表。
pub const ACCESS_TABLE: &str = "biz_supplier_access";
/// 取号键（`biz_seq.seq_type`）。
pub const SEQ_TYPE: &str = "SAC";
/// 单号前缀。
pub const NO_PREFIX: &str = "SAC";
/// 流水号宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 类型：准入（首次进入合格名录）。
pub const TYPE_ADMIT: &str = "ADMIT";
/// 类型：黑名单。
pub const TYPE_BLACKLIST: &str = "BLACKLIST";
/// 类型：解除黑名单。
pub const TYPE_RELEASE: &str = "RELEASE";
/// 类型值域（与库层 `ck_biz_sa_type` 同一份）。
pub const ACCESS_TYPES: &[&str] = &[TYPE_ADMIT, TYPE_BLACKLIST, TYPE_RELEASE];

/// 状态：拟稿（可改可删）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：审批中（必有实例）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：已生效（结论已落地到 `biz_supplier.status`）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态值域（与库层 `ck_biz_sa_status` 同一份）。
pub const ACCESS_STATUSES: &[&str] = &[STATUS_DRAFT, STATUS_SUBMITTED, STATUS_APPROVED];

/// `biz_supplier.status`：启用（可下单）。
pub const SUPPLIER_ENABLED: i16 = 1;
/// `biz_supplier.status`：停用（含被拉黑；不可下单）。
pub const SUPPLIER_DISABLED: i16 = 0;

/// 提交时申请理由的最小字数（与库层 `ck_biz_sa_reason_on_submit` 同一个 5）。
pub const MIN_REASON_CHARS: usize = 5;

/// 申请单分页锚点（SCOPE-EXEMPT：公司级采购文件，无 dept 列）。
pub const ACCESS_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(ACCESS_TABLE);

/// 类型中文名。
pub fn type_label(access_type: &str) -> &'static str {
    match access_type {
        TYPE_ADMIT => "准入",
        TYPE_BLACKLIST => "黑名单",
        TYPE_RELEASE => "解除黑名单",
        _ => "未知类型",
    }
}

/// 状态中文名。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "拟稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已生效",
        _ => "未知状态",
    }
}

// ==================== 行结构 ====================

/// 申请单行（含供应商**现值**状态，只用于展示与起点判断，不参与写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`SAC-yyyyMMdd-000001`）。
    pub access_no: String,
    /// 供应商 id。
    pub supplier_id: Uuid,
    /// 供应商编码快照。
    pub supplier_code: String,
    /// 供应商名快照。
    pub supplier_name: String,
    /// 类型（ADMIT / BLACKLIST / RELEASE）。
    pub access_type: String,
    /// 状态（DRAFT / SUBMITTED / APPROVED）。
    pub status: String,
    /// 申请理由。
    pub reason: Option<String>,
    /// 评估等级快照（仅黑名单）。
    pub grade_snapshot: Option<String>,
    /// 申请人。
    pub apply_by: Option<Uuid>,
    /// 申请人姓名快照。
    pub apply_by_name: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<Uuid>,
    /// 结论人。
    pub decided_by: Option<Uuid>,
    /// 结论人姓名快照。
    pub decided_by_name: Option<String>,
    /// 结论时间。
    pub decided_at: Option<DateTime<Utc>>,
    /// 备注（驳回意见等）。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 供应商**现值**状态（1 启用 / 0 停用；主档已删时按停用展示）。
    pub supplier_status: i16,
}

/// 供应商当前状态快照（起点判据的输入，纯数据 —— 便于脱库单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupplierState {
    /// 供应商是否存在（未删）。
    pub exists: bool,
    /// 当前 `biz_supplier.status`。
    pub status: i16,
    /// 是否在有效黑名单中（最近一张已通过的黑名单/解除单说的算）。
    pub blacklisted: bool,
    /// 是否曾经通过过准入（首次准入判据）。
    pub ever_admitted: bool,
}

impl SupplierState {
    /// 供应商不存在时的形态（任何申请都不许发起）。
    pub const fn missing() -> Self {
        Self {
            exists: false,
            status: SUPPLIER_DISABLED,
            blacklisted: false,
            ever_admitted: false,
        }
    }

    /// 是否可下单（采购订单建单的统一判据）。
    pub const fn usable(&self) -> bool {
        self.exists && self.status == SUPPLIER_ENABLED
    }

    /// 当前状态的中文描述（报文与日志共用）。
    pub const fn label(&self) -> &'static str {
        if !self.exists {
            "不存在或已删除"
        } else if self.blacklisted {
            "黑名单"
        } else if self.status == SUPPLIER_ENABLED {
            "启用"
        } else {
            "停用"
        }
    }
}

// ==================== 政策：起点判据 / 落地值（纯函数） ====================

/// 校验「这类申请单能不能对该供应商发起」。`Err` = 中文拒绝理由（handler 直接透传）。
///
/// 三条判据的出处见模块注释的表格；规矩是**一种状态只有一个合法动作**：
/// 已拉黑的不能再拉黑（要解除），没拉黑的不能解除，已准入过的不需要再准入。
pub fn check_start(access_type: &str, state: &SupplierState) -> std::result::Result<(), String> {
    if !ACCESS_TYPES.contains(&access_type) {
        return Err(format!(
            "未知的申请类型 {access_type}（只能是 ADMIT / BLACKLIST / RELEASE）"
        ));
    }
    if !state.exists {
        return Err("供应商不存在或已删除，不能发起准入 / 黑名单申请".to_string());
    }
    match access_type {
        TYPE_ADMIT => {
            if state.blacklisted {
                return Err(format!(
                    "该供应商当前在{}中，不能发「准入」申请：请先发「解除黑名单」申请，\
                     解除通过后再走准入",
                    type_label(TYPE_BLACKLIST)
                ));
            }
            if state.ever_admitted {
                return Err(format!(
                    "该供应商已通过过{}（当前{}），不需要重复准入；\
                     要恢复供货请用「解除黑名单」或在供应商档案里启用",
                    type_label(TYPE_ADMIT),
                    state.label()
                ));
            }
            Ok(())
        }
        TYPE_BLACKLIST => {
            if state.blacklisted {
                return Err(format!(
                    "该供应商已在{}中，不能重复拉黑；要恢复请发「解除黑名单」申请",
                    type_label(TYPE_BLACKLIST)
                ));
            }
            Ok(())
        }
        TYPE_RELEASE => {
            if !state.blacklisted {
                return Err(format!(
                    "该供应商当前不在{}中（当前{}），不需要解除",
                    type_label(TYPE_BLACKLIST),
                    state.label()
                ));
            }
            Ok(())
        }
        _ => Err(format!("未知的申请类型 {access_type}")),
    }
}

/// 通过后落 `biz_supplier.status` 的值：准入 / 解除 → 启用；拉黑 → 停用。
pub fn target_supplier_status(access_type: &str) -> i16 {
    match access_type {
        TYPE_BLACKLIST => SUPPLIER_DISABLED,
        _ => SUPPLIER_ENABLED,
    }
}

/// 申请理由清洗：`trim` + 连续空白折叠；空 → `None`。
pub fn clean_reason(raw: Option<&str>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(cleaned)
}

/// 校验提交时的理由（≥ [`MIN_REASON_CHARS`] 字）。`Err` = 中文报文。
pub fn check_reason(reason: Option<&str>) -> std::result::Result<String, String> {
    match clean_reason(reason) {
        Some(text) if text.chars().count() >= MIN_REASON_CHARS => Ok(text),
        Some(text) => Err(format!(
            "申请理由至少 {MIN_REASON_CHARS} 个字（当前 {} 个字）：拉黑 / 准入一家供应商的理由要能让审计看懂",
            text.chars().count()
        )),
        None => Err(format!(
            "申请理由必填（至少 {MIN_REASON_CHARS} 个字）：拉黑 / 准入一家供应商的理由要能让审计看懂"
        )),
    }
}

/// 校验等级快照（只有黑名单允许带；值域 A/B/C/D）。
pub fn check_grade(access_type: &str, grade: Option<&str>) -> std::result::Result<Option<String>, String> {
    let Some(raw) = grade.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let upper = raw.to_uppercase();
    if !["A", "B", "C", "D"].contains(&upper.as_str()) {
        return Err(format!("评估等级快照只能是 A / B / C / D，收到 {raw}"));
    }
    if access_type != TYPE_BLACKLIST {
        return Err("评估等级快照只对「黑名单」申请有意义（准入 / 解除单请留空）：\
             库层 ck_biz_sa_grade_only_blacklist 同样会拒"
            .to_string());
    }
    Ok(Some(upper))
}

// ==================== 取号 ====================

/// 北京时间的天键（`yyyyMMdd`）—— 取号按 CST 滚动，与其它 ERP 单号同口径。
fn period_key_of(now: DateTime<Utc>) -> String {
    let cst = now + Duration::hours(8);
    format!("{:04}{:02}{:02}", cst.year(), cst.month(), cst.day())
}

/// 拼单号：`SAC-yyyyMMdd-000001`。
fn format_access_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0width$}", width = NO_SEQ_WIDTH)
}

/// `biz_seq` 缺行时补一行（迁移已种；这里是「克隆库 / 手工删过」的兜底）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SAC', '供应商准入/黑名单申请单号', 'SAC', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '供应商准入/黑名单申请单号：SAC-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SAC' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|error| match error {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='SAC')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'SAC' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个申请单号（周期键按北京时间）。返回形如 `SAC-20261118-000001`。
pub async fn next_access_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_access_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SAC') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 读入口 ====================

/// 单头 select（带供应商现值状态；主档已删按停用展示）。
fn access_select_sql() -> String {
    format!(
        "SELECT a.id, a.access_no, a.supplier_id, a.supplier_code, a.supplier_name, \
                a.access_type, a.status, a.reason, a.grade_snapshot, \
                a.apply_by, a.apply_by_name, a.approval_instance_id, \
                a.decided_by, a.decided_by_name, a.decided_at, a.remark, \
                a.created_at, a.updated_at, a.create_by, \
                COALESCE(s.status, {SUPPLIER_DISABLED}) AS supplier_status \
           FROM {ACCESS_TABLE} a \
           LEFT JOIN biz_supplier s ON s.id = a.supplier_id AND s.deleted_at IS NULL \
          WHERE a.deleted_at IS NULL"
    )
}

fn read_access_row(row: &Row) -> DbResult<AccessRow> {
    Ok(AccessRow {
        id: row.get_typed("id")?,
        access_no: row.get_typed("access_no")?,
        supplier_id: row.get_typed("supplier_id")?,
        supplier_code: row.get_typed("supplier_code")?,
        supplier_name: row.get_typed("supplier_name")?,
        access_type: row.get_typed("access_type")?,
        status: row.get_typed("status")?,
        reason: row.get_typed("reason")?,
        grade_snapshot: row.get_typed("grade_snapshot")?,
        apply_by: row.get_typed("apply_by")?,
        apply_by_name: row.get_typed("apply_by_name")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        decided_by: row.get_typed("decided_by")?,
        decided_by_name: row.get_typed("decided_by_name")?,
        decided_at: row.get_typed("decided_at")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        updated_at: row.get_typed("updated_at")?,
        create_by: row.get_typed("create_by")?,
        supplier_status: row.get_typed("supplier_status")?,
    })
}

/// 按主键取一张申请单（未删）。
pub async fn find_access(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<AccessRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行（单条入口的可见性由权限码控制，不做数据范围裁剪）。
    let rows = ex
        .query_sql(
            format!("{} AND a.id = ?::uuid", access_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(read_access_row(row)?)),
    }
}

/// 该供应商当前**未结**（草稿 / 审批中）的申请单（`uk_biz_sa_supplier_open` 保证至多 1 张）。
pub async fn find_open_for_supplier(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Option<AccessRow>> {
    // SCOPE-EXEMPT: 按供应商取 1 行未结单。
    let rows = ex
        .query_sql(
            format!(
                "{} AND a.supplier_id = ?::uuid AND a.status IN ('DRAFT', 'SUBMITTED')",
                access_select_sql()
            ),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(read_access_row(row)?)),
    }
}

/// 供应商当前状态（起点判据的唯一取数点）。
///
/// 「是否在黑名单」= 最近一张**已通过**的申请单类型是不是 `BLACKLIST`；
/// 「是否曾通过准入」= 存在任意一张 `APPROVED` 的 `ADMIT` 单。
pub async fn supplier_state(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<SupplierState> {
    // SCOPE-EXEMPT: 供应商主数据按主键取 1 行。
    let rows = ex
        .query_sql(
            "SELECT s.id, s.status, \
                    (SELECT a.access_type FROM biz_supplier_access a \
                      WHERE a.supplier_id = s.id AND a.status = 'APPROVED' AND a.deleted_at IS NULL \
                      ORDER BY a.decided_at DESC NULLS LAST, a.created_at DESC LIMIT 1) AS last_type, \
                    EXISTS (SELECT 1 FROM biz_supplier_access a \
                             WHERE a.supplier_id = s.id AND a.access_type = 'ADMIT' \
                               AND a.status = 'APPROVED' AND a.deleted_at IS NULL) AS ever_admitted \
               FROM biz_supplier s \
              WHERE s.id = ?::uuid AND s.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(SupplierState::missing());
    };
    let last_type: Option<String> = row.get_typed("last_type")?;
    Ok(SupplierState {
        exists: true,
        status: row.get_typed("status")?,
        blacklisted: last_type.as_deref() == Some(TYPE_BLACKLIST),
        ever_admitted: row.get_typed("ever_admitted")?,
    })
}

/// 供应商编码 + 名称快照（建单时冻结）。`None` = 不存在 / 已删。
pub async fn supplier_snapshot(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Option<(String, String)>> {
    // SCOPE-EXEMPT: 供应商主数据按主键取 1 行。
    let rows = ex
        .query_sql(
            "SELECT supplier_code, supplier_name FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("supplier_code")?, row.get_typed("supplier_name")?))),
    }
}

/// 用户姓名（申请人 / 结论人快照用；查不到 → `None`，不阻断业务）。
pub async fn user_name(ex: &impl SqlExecutor, user_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: sys_user 按主键取 1 行。
    let rows = ex
        .query_sql(
            "SELECT COALESCE(NULLIF(btrim(real_name), ''), username) AS display_name \
               FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&user_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("display_name").ok().flatten()),
    }
}

/// 申请单分页（关键字：单号 / 供应商编码 / 供应商名 / 理由；可筛类型 / 状态 / 供应商）。
pub async fn page_access(
    filter: &crate::business::erp::purchase::supplier_access_dto::AccessFilter,
) -> DbResult<(Vec<AccessRow>, u64)> {
    let mut sql = access_select_sql();
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND a.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(access_type) = filter.access_type.as_deref() {
        sql.push_str(" AND a.access_type = ?");
        params.push(Value::Text(access_type.to_string()));
    }
    if let Some(supplier_id) = filter.supplier_id {
        sql.push_str(" AND a.supplier_id = ?::uuid");
        params.push(Value::from(&supplier_id));
    }
    if let Some(pattern) = cloudpivot_kernel::utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (a.access_no LIKE ? OR a.supplier_code LIKE ? OR a.supplier_name LIKE ? \
                OR COALESCE(a.reason, '') LIKE ?)",
        );
        for _ in 0..4 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        ACCESS_TARGET,
        &sql,
        params,
        Some("ORDER BY a.created_at DESC, a.access_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page.list.iter().map(read_access_row).collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

// ==================== 写入口（状态机 CAS） ====================

/// 建单需要的字段（`reason` 在草稿允许为空，提交时才校验）。
#[derive(Debug, Clone)]
pub struct NewAccess {
    /// 供应商 id。
    pub supplier_id: Uuid,
    /// 类型。
    pub access_type: String,
    /// 申请理由。
    pub reason: Option<String>,
    /// 评估等级快照（仅黑名单）。
    pub grade_snapshot: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 申请人。
    pub operator_id: Option<Uuid>,
}

/// 插一张草稿单，返回新单 id。
pub async fn insert_draft(ex: &impl SqlExecutor, input: &NewAccess, now: DateTime<Utc>) -> DbResult<Uuid> {
    let Some((supplier_code, supplier_name)) = supplier_snapshot(ex, input.supplier_id).await? else {
        return Err(DbError::InvalidValue("供应商不存在或已删除，不能建申请单".to_string()));
    };
    let access_no = next_access_no(ex, now).await?;
    let apply_by_name = match input.operator_id {
        Some(id) => user_name(ex, id).await?,
        None => None,
    };
    let id = Uuid::now_v7();
    ex.exec_sql(
        format!(
            "INSERT INTO {ACCESS_TABLE} \
                 (id, access_no, supplier_id, supplier_code, supplier_name, access_type, status, \
                  reason, grade_snapshot, apply_by, apply_by_name, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?::uuid, ?, ?, ?, 'DRAFT', ?, ?, ?, ?, ?, ?::uuid, ?::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(access_no),
            Value::from(&input.supplier_id),
            Value::Text(supplier_code),
            Value::Text(supplier_name),
            Value::Text(input.access_type.clone()),
            optional_str(input.reason.as_deref()),
            optional_str(input.grade_snapshot.as_deref()),
            optional_uuid(input.operator_id),
            optional_str(apply_by_name.as_deref()),
            optional_str(input.remark.as_deref()),
            optional_uuid(input.operator_id),
            optional_uuid(input.operator_id),
        ],
    )
    .await?;
    Ok(id)
}

/// 改草稿（只有 `DRAFT` 能改；类型与供应商不可改 —— 改类型等于换一件事）。
///
/// 返回受影响行数：0 = 状态已不是草稿（调用方按 409 处理）。
pub async fn update_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    reason: Option<&str>,
    grade_snapshot: Option<&str>,
    remark: Option<&str>,
    operator_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} \
                SET reason = ?, grade_snapshot = ?, remark = ?, update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            optional_str(reason),
            optional_str(grade_snapshot),
            optional_str(remark),
            optional_uuid(operator_id),
            Value::from(&id),
        ],
    )
    .await
}

/// 删草稿（软删）。返回受影响行数：0 = 不是草稿（调用方按 409 处理）。
pub async fn delete_draft(ex: &impl SqlExecutor, id: Uuid, operator_id: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} SET deleted_at = now(), update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![optional_uuid(operator_id), Value::from(&id)],
    )
    .await
}

/// 提交：`DRAFT → SUBMITTED` 并写实例 id（CAS，0 行 = 并发改动）。
pub async fn mark_submitted(
    ex: &impl SqlExecutor,
    id: Uuid,
    instance_id: Uuid,
    reason: &str,
    operator_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} \
                SET status = 'SUBMITTED', approval_instance_id = ?::uuid, reason = ?, \
                    update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&instance_id),
            Value::Text(reason.to_string()),
            optional_uuid(operator_id),
            Value::from(&id),
        ],
    )
    .await
}

/// 审批通过：`SUBMITTED → APPROVED` + 结论人/时间（CAS，0 行 = 已不是审批中）。
pub async fn mark_approved(ex: &impl SqlExecutor, id: Uuid, decided_by: Option<Uuid>) -> DbResult<u64> {
    let decided_by_name = match decided_by {
        Some(user_id) => user_name(ex, user_id).await?,
        None => None,
    };
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} \
                SET status = 'APPROVED', decided_by = ?::uuid, decided_by_name = ?, \
                    decided_at = now(), update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'SUBMITTED' AND deleted_at IS NULL"
        ),
        vec![
            optional_uuid(decided_by),
            optional_str(decided_by_name.as_deref()),
            optional_uuid(decided_by),
            Value::from(&id),
        ],
    )
    .await
}

/// 驳回 / 撤回 / 终止：`SUBMITTED → DRAFT`（把意见写进备注）。
pub async fn mark_back_to_draft(
    ex: &impl SqlExecutor,
    id: Uuid,
    remark: &str,
    operator_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} \
                SET status = 'DRAFT', remark = ?, approval_instance_id = NULL, \
                    update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'SUBMITTED' AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(remark.to_string()),
            optional_uuid(operator_id),
            Value::from(&id),
        ],
    )
    .await
}

/// 没配流程时的「显式跳过审批」：`DRAFT → APPROVED` + 结论人/时间 + 理由进备注（CAS）。
///
/// 与 [`mark_approved`] 的区别只在**起点状态**（跳过审批没有 `SUBMITTED` 这一步），
/// 落地列完全一致 —— 两条路径共用同一份「结论列同生共死」约束（`ck_biz_sa_decided`）。
pub async fn mark_direct_approved(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator_id: Option<Uuid>,
    remark: Option<&str>,
) -> DbResult<u64> {
    let decided_by_name = match operator_id {
        Some(user_id) => user_name(ex, user_id).await?,
        None => None,
    };
    ex.exec_sql(
        format!(
            "UPDATE {ACCESS_TABLE} \
                SET status = 'APPROVED', decided_by = ?::uuid, decided_by_name = ?, \
                    decided_at = now(), remark = COALESCE(?, remark), \
                    update_by = ?::uuid, updated_at = now() \
              WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
        ),
        vec![
            optional_uuid(operator_id),
            optional_str(decided_by_name.as_deref()),
            optional_str(remark),
            optional_uuid(operator_id),
            Value::from(&id),
        ],
    )
    .await
}

/// 落供应商状态（**唯一**改 `biz_supplier.status` 的入口；由审批回调在事务内调用）。
pub async fn set_supplier_status(
    ex: &impl SqlExecutor,
    supplier_id: Uuid,
    status: i16,
    operator_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_supplier SET status = ?, update_by = ?::uuid, updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::I16(status),
            optional_uuid(operator_id),
            Value::from(&supplier_id),
        ],
    )
    .await
}

// ==================== 小工具 ====================

fn optional_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(uuid) => Value::from(&uuid),
        None => Value::Null,
    }
}

fn optional_str(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: i16, blacklisted: bool, ever_admitted: bool) -> SupplierState {
        SupplierState {
            exists: true,
            status,
            blacklisted,
            ever_admitted,
        }
    }

    /// ① 值域常量与库层 CHECK 逐字一致（错一个字 = 写库直接 23514）。
    #[test]
    fn domain_constants_match_ddl() {
        assert_eq!(ACCESS_TYPES, &["ADMIT", "BLACKLIST", "RELEASE"]);
        assert_eq!(ACCESS_STATUSES, &["DRAFT", "SUBMITTED", "APPROVED"]);
        assert_eq!(MIN_REASON_CHARS, 5, "与 ck_biz_sa_reason_on_submit 的 5 同源");
        assert_eq!(SUPPLIER_ENABLED, 1);
        assert_eq!(SUPPLIER_DISABLED, 0);
        assert_eq!(SEQ_TYPE, "SAC");
        assert_eq!(NO_PREFIX, "SAC");
    }

    /// ② 单号形状：`SAC-yyyyMMdd-000001`（19 字符，日期段与流水段都是数字）。
    #[test]
    fn access_no_shape_is_stable() {
        let no = format_access_no("20261118", 1);
        assert_eq!(no, "SAC-20261118-000001");
        assert_eq!(no.len(), 19);
        assert!(no[4..12].chars().all(|c| c.is_ascii_digit()));
        assert!(no[13..].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(format_access_no("20261118", 123456), "SAC-20261118-123456");
    }

    /// ③ 起点判据：一种状态只有一个合法动作。
    #[test]
    fn check_start_is_single_valued() {
        // 启用中（已准入）：只能拉黑
        let enabled = state(SUPPLIER_ENABLED, false, true);
        assert!(check_start(TYPE_BLACKLIST, &enabled).is_ok());
        assert!(check_start(TYPE_ADMIT, &enabled).is_err(), "已准入过就不能再准入");
        assert!(check_start(TYPE_RELEASE, &enabled).is_err(), "没拉黑不能解除");

        // 黑名单中：只能解除
        let blacklisted = state(SUPPLIER_DISABLED, true, true);
        assert!(check_start(TYPE_RELEASE, &blacklisted).is_ok());
        assert!(check_start(TYPE_BLACKLIST, &blacklisted).is_err(), "不能重复拉黑");
        assert!(
            check_start(TYPE_ADMIT, &blacklisted).is_err(),
            "黑名单中不能直接准入（必须先解除）"
        );

        // 启用但从未准入（主数据直接建档）：只能准入
        let fresh = state(SUPPLIER_ENABLED, false, false);
        assert!(check_start(TYPE_ADMIT, &fresh).is_ok());
        assert!(check_start(TYPE_RELEASE, &fresh).is_err());
        assert!(check_start(TYPE_BLACKLIST, &fresh).is_ok(), "新供应商也能直接拉黑");

        // 停用但从未准入：准入与拉黑都可以（准入 = 把它启用）
        let disabled = state(SUPPLIER_DISABLED, false, false);
        assert!(check_start(TYPE_ADMIT, &disabled).is_ok());
        assert!(check_start(TYPE_BLACKLIST, &disabled).is_ok());

        // 供应商不存在：三种都不许
        let missing = SupplierState::missing();
        for access_type in ACCESS_TYPES {
            assert!(
                check_start(access_type, &missing).is_err(),
                "{access_type} 不该放过不存在的供应商"
            );
        }
        // 未知类型
        assert!(check_start("DELETE", &enabled).is_err());
    }

    /// ④ 落地值：拉黑 → 停用；准入 / 解除 → 启用。
    #[test]
    fn target_status_per_type() {
        assert_eq!(target_supplier_status(TYPE_ADMIT), SUPPLIER_ENABLED);
        assert_eq!(target_supplier_status(TYPE_RELEASE), SUPPLIER_ENABLED);
        assert_eq!(target_supplier_status(TYPE_BLACKLIST), SUPPLIER_DISABLED);
    }

    /// ⑤ 理由：提交校验 ≥ 5 字，空白折叠，空 → None。
    #[test]
    fn reason_rules() {
        assert_eq!(clean_reason(Some("  质量   问题  ")).as_deref(), Some("质量 问题"));
        assert_eq!(clean_reason(Some("   ")), None);
        assert_eq!(clean_reason(None), None);
        assert!(check_reason(Some("来料不合格")).is_ok());
        assert!(check_reason(Some("不合格")).is_err(), "3 个字不够");
        assert!(check_reason(Some("     ")).is_err());
        assert!(check_reason(None).is_err());
        // 中文按字符数算，不按字节数：4 个字不够（12 字节也不行），5 个字够
        assert!(
            check_reason(Some("交叉污染")).is_err(),
            "4 个中文字符 = 4 字，不够 5 字"
        );
        assert!(check_reason(Some("交叉污染风险")).is_ok());
    }

    /// ⑥ 等级快照：只有黑名单能带；值域 A/B/C/D。
    #[test]
    fn grade_rules() {
        assert_eq!(check_grade(TYPE_BLACKLIST, Some(" c ")).unwrap().as_deref(), Some("C"));
        assert_eq!(check_grade(TYPE_BLACKLIST, None).unwrap(), None);
        assert!(check_grade(TYPE_BLACKLIST, Some("E")).is_err());
        assert!(
            check_grade(TYPE_ADMIT, Some("D")).is_err(),
            "准入单带等级会被库层 ck_biz_sa_grade_only_blacklist 拒，接口侧先拦住"
        );
        assert!(check_grade(TYPE_RELEASE, Some("A")).is_err());
    }

    /// ⑦ 可下单判据：只有「存在 + 启用」才算可用。
    #[test]
    fn usable_requires_enabled_and_exists() {
        assert!(state(SUPPLIER_ENABLED, false, true).usable());
        assert!(!state(SUPPLIER_DISABLED, false, true).usable());
        assert!(!SupplierState::missing().usable());
        assert_eq!(SupplierState::missing().label(), "不存在或已删除");
    }
}
