//! 列级脱敏的请求 / 响应 DTO（批次 B · B6）。
//!
//! # 约定
//!
//! - ID 一律下发 `String`（`uuid::Uuid` 未开 serde feature，与 `business/` 其余模块一致）；
//! - 枚举值同时下发「落库值」与「稳定编码 + 中文标签」，前端不必内置一份码表；
//! - 列表项带 `scope`（`GLOBAL` / `ROLE`），让管理员一眼看出这条规则是给所有人还是给某个角色。

use serde::{Deserialize, Serialize};

use alun::prelude::*;

use crate::business::masking::masker::MaskType;
use crate::business::masking::model::FieldMaskRow;

/// 规则分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FieldMaskPageQuery {
    /// 页码，从 1 开始。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 表名精确匹配（如 `sys_user`）。
    pub table_name: Option<String>,
    /// 列名精确匹配。
    pub column_name: Option<String>,
    /// 作用域过滤：`global` 只看全局规则；`role` 只看角色规则；留空都看。
    pub scope: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: Option<i16>,
}

/// 一条脱敏规则（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct FieldMaskRes {
    /// 规则 ID。
    pub id: String,
    /// 表名。
    pub table_name: String,
    /// 列名。
    pub column_name: String,
    /// 脱敏类型落库值（1..7）。
    pub mask_type: i16,
    /// 脱敏类型稳定编码（`mobile` / `id_card` / …）。
    pub mask_type_code: String,
    /// 脱敏类型中文标签。
    pub mask_type_label: String,
    /// 实际生效的参数（库里为空时回填该类型的缺省参数，让前端看到「真正在用的值」）。
    pub mask_param: String,
    /// 适用角色 ID；`None` = 全局规则。
    pub role_id: Option<String>,
    /// 适用角色名称（随响应下发，避免前端二次查询）。
    pub role_name: Option<String>,
    /// 作用域：`GLOBAL` / `ROLE`。
    pub scope: String,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 脱敏理由（合规留痕）。
    pub reason: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl FieldMaskRes {
    /// 由库侧行投影成出参。
    ///
    /// # 参数
    ///
    /// - `row`：`sys_field_mask` 的一行（含 join 出来的角色名）。
    pub fn from_row(row: &FieldMaskRow) -> Self {
        let ty = MaskType::from_i16(row.mask_type);
        Self {
            id: row.id.to_string(),
            table_name: row.table_name.clone(),
            column_name: row.column_name.clone(),
            mask_type: row.mask_type,
            mask_type_code: ty.map(|t| t.code().to_string()).unwrap_or_default(),
            mask_type_label: ty.map(|t| t.label().to_string()).unwrap_or_default(),
            mask_param: effective_param(ty, row.mask_param.as_deref()),
            role_id: row.role_id.map(|id| id.to_string()),
            role_name: row.role_name.clone(),
            scope: if row.role_id.is_some() {
                "ROLE"
            } else {
                "GLOBAL"
            }
            .to_string(),
            status: row.status,
            reason: row.reason.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 实际生效的参数：库里为空 → 该类型的缺省参数；无参数类型 → 空串。
fn effective_param(ty: Option<MaskType>, stored: Option<&str>) -> String {
    match stored.map(str::trim).filter(|item| !item.is_empty()) {
        Some(text) => text.to_string(),
        None => ty
            .and_then(|t| t.default_param())
            .unwrap_or_default()
            .to_string(),
    }
}

/// 新建脱敏规则请求。
#[derive(Debug, Clone, Deserialize)]
pub struct FieldMaskCreateReq {
    /// 表名（小写下划线，必须真实存在于 `information_schema`）。
    pub table_name: String,
    /// 列名（同上）。
    pub column_name: String,
    /// 脱敏类型（1..7）。
    pub mask_type: i16,
    /// 类型参数（见 `MaskType::param_hint`）。
    pub mask_param: Option<String>,
    /// 适用角色；`None` = 全局规则。
    pub role_id: Option<String>,
    /// 状态，缺省 1（启用）。
    pub status: Option<i16>,
    /// 脱敏理由（必填，合规留痕）。
    pub reason: String,
}

/// 修改脱敏规则请求（表名 / 列名 / 角色不可改——要换目标请删了重建，
/// 否则「同一条规则悄悄换了列」会让审计追溯失效）。
#[derive(Debug, Clone, Deserialize)]
pub struct FieldMaskUpdateReq {
    /// 脱敏类型（1..7）。
    pub mask_type: i16,
    /// 类型参数。
    pub mask_param: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: Option<i16>,
    /// 脱敏理由。
    pub reason: String,
}

/// 试运行查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct MaskPreviewQuery {
    /// 脱敏类型（1..7）。
    pub mask_type: i16,
    /// 类型参数。
    pub mask_param: Option<String>,
    /// 待试运行的原文。
    pub value: Option<String>,
}

/// 试运行结果。
///
/// `changed = false` 有两种情况：值本来就无需脱敏（空 / 过短 / 不匹配），
/// 或掩码后与原文相同。前端据此提示「配置生效但该值不含敏感信息」。
#[derive(Debug, Clone, Serialize)]
pub struct MaskPreviewRes {
    /// 脱敏类型落库值。
    pub mask_type: i16,
    /// 脱敏类型稳定编码。
    pub mask_type_code: String,
    /// 脱敏类型中文标签。
    pub mask_type_label: String,
    /// 实际生效的参数。
    pub mask_param: String,
    /// 原文。
    pub input: String,
    /// 脱敏后。
    pub output: String,
    /// 是否发生了变化。
    pub changed: bool,
}

/// 「某用户实际生效的规则」查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EffectiveMaskQuery {
    /// 目标用户 ID（缺省 = 当前登录用户）。
    pub user_id: Option<String>,
}

/// 一条生效规则（带来源）。
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveRuleRes {
    /// 规则 ID。
    pub rule_id: String,
    /// 表名。
    pub table_name: String,
    /// 列名。
    pub column_name: String,
    /// 脱敏类型落库值。
    pub mask_type: i16,
    /// 脱敏类型中文标签。
    pub mask_type_label: String,
    /// 实际生效的参数。
    pub mask_param: String,
    /// 来源：`GLOBAL` / `ROLE`。
    pub source: String,
    /// 来源角色名（`GLOBAL` 时为空）。
    pub source_name: Option<String>,
    /// 用默认样例试运行的结果（让运维当场看出「这条规则到底会把值变成什么」）。
    pub sample: String,
}

/// 某用户实际生效的规则清单。
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveMaskRes {
    /// 目标用户 ID。
    pub user_id: String,
    /// 目标用户名。
    pub username: String,
    /// 是否超管（超管整表豁免）。
    pub is_super_admin: bool,
    /// 是否被豁免（超管 / 持 `system:field-mask:plain`）。
    pub exempt: bool,
    /// 豁免原因（`NONE` / `SUPER_ADMIN` / `PLAIN_PERMISSION`）。
    pub exempt_reason: String,
    /// 生效规则条数。
    pub total: usize,
    /// 生效规则。
    pub rules: Vec<EffectiveRuleRes>,
}

/// 规则的默认样例值（`effective` 接口的 `sample` 字段用它试运行）。
///
/// # 参数
///
/// - `ty`：脱敏类型。
///
/// # 返回值
///
/// 该类型最典型的输入形态；试运行失败时（例如正则缺参数）返回空串，
/// 不让自检接口因为一条坏配置整体 500。
pub fn sample_value(ty: MaskType) -> &'static str {
    match ty {
        MaskType::Mobile => "13812345678",
        MaskType::IdCard => "110101199001011234",
        MaskType::Email => "alice@mail.com",
        MaskType::Name => "张三丰",
        MaskType::BankCard => "6222021234567890",
        MaskType::Amount => "48620.00",
        MaskType::Regex => "AB12345",
    }
}
