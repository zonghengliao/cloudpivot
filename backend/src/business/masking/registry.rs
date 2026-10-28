//! `MaskSet`（一次请求的脱敏规则集合）+ `Maskable`（DTO 出参前脱敏）+ 规则合并口径。
//!
//! 依据 `docs/批次B-技术架构.md` §3.3（D6：DTO 出参前脱敏）、§3.4（三道闸）、
//! §3.5（谁不受脱敏）、§3.1（多规则命中时的合并方向）。
//!
//! # 为什么是「显式传参」而不是 serde 序列化器
//!
//! 架构 §3.3 的选型表已把结论钉死：serde 序列化器只能拿到字段名，拿不到「这个字段属于
//! 哪张表」，也无法在渲染阶段拿到请求上下文（`Res` 渲染时没有 handler 的栈）。于是
//! 脱敏变成**handler 里的一次显式调用**：
//!
//! ```ignore
//! let mask = masking::resolve(Some(&principal), Some(&claims)).await?;
//! for item in &mut list { item.mask(&mask); }   // Maskable::mask：纯内存
//! ```
//!
//! 好处是「可测、可见、无隐式状态」；代价是「忘了调就是明文」，靠 §3.4 的三道闸
//! （编译期全字段样例 / 测试期敏感值扫描 / 运行期导出 fail-closed）兜住。
//!
//! # 合并口径（架构 §3.1 末条 + §3.5）
//!
//! - **角色规则优先于全局规则**：同 `(table, column)` 同时命中时，角色规则更具体；
//! - **多角色按 OR 命中**：脱敏是「限制」，用户持有任一角色的规则就必须脱敏
//!   （方向与数据权限的并集相反——数据权限取并集是放行，脱敏取并集是收紧）；
//! - **同层多条**（同一列被两个角色的规则覆盖）：按 `created_at ASC, id ASC` 取第一条，
//!   保证结果与查询顺序无关（唯一索引已保证同一角色同一列只有一条，所以同层多条只可能
//!   来自不同角色）。
//!
//! # 豁免（架构 §3.6）
//!
//! `SUPER_ADMIN` 或持 `system:field-mask:plain` 权限码的人看到明文。后者**每次明文访问
//! 都落一条 `audit_logs`**（`FIELD_MASK_PLAIN_READ`）——没有这条留痕，「看明文」就是
//! 不可审计的后门。

use std::collections::BTreeMap;

use alun::prelude::*;
use alun::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::masking::masker::{self, MaskError, MaskType};
use crate::business::masking::model::{self, RawRule};
use crate::business::shared::principal::ScopedPrincipal;
use crate::shared::constants;

/// 许可「看明文」的权限码（与 `V0065__seed_batchb_permissions` 逐字一致）。
pub const PLAIN_PERMISSION: &str = "system:field-mask:plain";

/// 规则的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskSource {
    /// 全局默认规则（`role_id IS NULL`）。
    Global,
    /// 角色规则。
    Role,
}

impl MaskSource {
    /// 稳定编码（`GLOBAL` / `ROLE`）。
    pub fn code(self) -> &'static str {
        match self {
            MaskSource::Global => "GLOBAL",
            MaskSource::Role => "ROLE",
        }
    }
}

/// 一条**已合并**的脱敏规则（同 `(table, column)` 只有一条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaskRule {
    /// 规则 ID（审计追溯用）。
    pub rule_id: String,
    /// 表名。
    pub table: String,
    /// 列名。
    pub column: String,
    /// 脱敏类型（1..7）。用整数存是为了让 `MaskSet` 可缓存（`MaskType` 不实现 serde，
    /// 它属于「运行期枚举」，序列化一份进 Redis 没有意义）。
    pub mask_type: i16,
    /// 类型参数（`None` = 用该类型缺省值）。
    pub mask_param: Option<String>,
    /// 来源。
    pub source: MaskSource,
    /// 来源角色名（`Global` 时为空）。
    pub role_name: Option<String>,
}

impl MaskRule {
    /// 脱敏类型；库里出现非法值时返回 `None`（调用方按 fail-closed 处理）。
    pub fn ty(&self) -> Option<MaskType> {
        MaskType::from_i16(self.mask_type)
    }

    /// 实际生效的参数（空 → 类型缺省值）。
    pub fn effective_param(&self) -> Option<&str> {
        match self.mask_param.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => Some(text),
            _ => self.ty().and_then(|ty| ty.default_param()),
        }
    }
}

/// 豁免原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExemptReason {
    /// 未豁免（正常脱敏）。
    None,
    /// 超管整表豁免。
    SuperAdmin,
    /// 持 `system:field-mask:plain` 权限码。
    PlainPermission,
}

impl ExemptReason {
    /// 稳定编码。
    pub fn code(self) -> &'static str {
        match self {
            ExemptReason::None => "NONE",
            ExemptReason::SuperAdmin => "SUPER_ADMIN",
            ExemptReason::PlainPermission => "PLAIN_PERMISSION",
        }
    }

    /// 中文说明。
    pub fn label(self) -> &'static str {
        match self {
            ExemptReason::None => "未豁免",
            ExemptReason::SuperAdmin => "超级管理员（整表豁免）",
            ExemptReason::PlainPermission => "持有 system:field-mask:plain 权限码",
        }
    }
}

/// 一次请求的脱敏规则集合。
///
/// 两个字段都私有：集合只能由 [`resolve`] 构造，避免 `MaskSet { rules: vec![], .. }`
/// 这种「手搓一个空集合 = 关掉脱敏」的写法在业务代码里出现。
#[derive(Debug, Clone)]
pub struct MaskSet {
    rules: Vec<MaskRule>,
    exempt: ExemptReason,
}

impl MaskSet {
    /// 空集合（**未豁免**）：用于单测与「确实没有任何规则」的场景。
    pub fn empty() -> Self {
        Self {
            rules: Vec::new(),
            exempt: ExemptReason::None,
        }
    }

    /// 内部构造。
    fn new(rules: Vec<MaskRule>, exempt: ExemptReason) -> Self {
        Self { rules, exempt }
    }

    /// 是否被豁免（超管 / 明版权限）。
    pub fn is_exempt(&self) -> bool {
        self.exempt != ExemptReason::None
    }

    /// 豁免原因。
    pub fn exempt(&self) -> ExemptReason {
        self.exempt
    }

    /// 是否没有任何规则。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 规则条数。
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 全部规则（自检接口 / 日志用）。
    pub fn rules(&self) -> &[MaskRule] {
        &self.rules
    }

    /// 命中某列的规则。
    ///
    /// # 参数
    ///
    /// - `table`：表名；
    /// - `column`：列名。
    pub fn hits(&self, table: &str, column: &str) -> Option<&MaskRule> {
        if self.is_exempt() {
            return None;
        }
        self.rules.iter().find(|rule| {
            rule.table.eq_ignore_ascii_case(table) && rule.column.eq_ignore_ascii_case(column)
        })
    }

    /// 对单个值脱敏：`None` = 该列没有规则（调用方按各自的策略处理；
    /// 导出路径必须把它当错误，见 `impexp::engine::format_export_rows`）。
    ///
    /// # 参数
    ///
    /// - `table` / `column`：列坐标；
    /// - `raw`：明文值。
    ///
    /// # 异常情况
    ///
    /// 规则本身不可用（非法 `mask_type`）或参数不合法时返回 [`MaskError`]。
    /// 调用方**不得**把 `raw` 当结果（fail-closed）。
    pub fn mask_value(
        &self,
        table: &str,
        column: &str,
        raw: &str,
    ) -> std::result::Result<Option<String>, MaskError> {
        let Some(rule) = self.hits(table, column) else {
            return Ok(None);
        };
        let ty = rule.ty().ok_or_else(|| MaskError::Rule {
            reason: format!(
                "规则 {} 的 mask_type={} 非法（应 1..7），无法确定脱敏方式",
                rule.rule_id, rule.mask_type
            ),
        })?;
        masker::apply(ty, rule.effective_param(), raw).map(Some)
    }

    /// 脱敏失败时的兜底值：**全掩码**，绝不回退明文。
    ///
    /// # 参数
    ///
    /// - `raw`：原文。
    pub fn fail_closed(raw: &str) -> String {
        "*".repeat(raw.chars().count())
    }
}

/// DTO 出参前脱敏（D6 的唯一落点）。
///
/// 实现者必须是「含敏感列的响应 DTO」，并且**同文件提供全字段样例构造器**
/// （`sample_for_mask_test`，`#[cfg(test)]`）——这样给 DTO 加字段会立刻编译失败，
/// 强制作者补样例、进而被 [`tests`] 的敏感值扫描覆盖（架构 §3.4 闸 1 + 闸 2）。
pub trait Maskable {
    /// 就地脱敏。
    ///
    /// # 参数
    ///
    /// - `set`：本次请求的规则集合（豁免时 `mask` 应当是 no-op）。
    fn mask(&mut self, set: &MaskSet);

    /// 该 DTO 对应的表名（导出 / 自检 / 日志用）。
    fn mask_table() -> &'static str;
}

// ==================== 规则合并（纯函数） ====================

/// 把库里的原始规则行合并成「每列一条」。
///
/// # 参数
///
/// - `rows`：[`model::rules_for_user`] 的返回。
///
/// # 返回值
///
/// 按 `(table, column)` 升序、每列一条的规则集合。
///
/// # 合并规则
///
/// 见模块头「合并口径」。非法 `mask_type` 的行会被**丢弃并记 error 日志**：
/// 它由数据库 `CHECK` 约束保证不该存在，真出现说明有人绕过了约束（直接改库），
/// 此时宁可「没有规则」（导出会因 fail-closed 报错）也不要拿一条读不懂的规则去猜。
pub fn pick_effective(rows: Vec<RawRule>) -> Vec<MaskRule> {
    let mut sorted = rows;
    sorted.sort_by(|a, b| {
        a.table_name
            .cmp(&b.table_name)
            .then_with(|| a.column_name.cmp(&b.column_name))
            // `role_id.is_none()` false < true ⇒ 角色规则排前面（更具体者优先）
            .then_with(|| a.role_id.is_none().cmp(&b.role_id.is_none()))
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut picked: BTreeMap<(String, String), MaskRule> = BTreeMap::new();
    for row in sorted {
        if MaskType::from_i16(row.mask_type).is_none() {
            tracing::error!(
                rule_id = %row.id,
                mask_type = row.mask_type,
                "脱敏规则的 mask_type 非法（应是 1..7），已忽略该规则：请检查是否有人直接改库"
            );
            continue;
        }
        let key = (row.table_name.clone(), row.column_name.clone());
        picked.entry(key).or_insert_with(|| MaskRule {
            rule_id: row.id.to_string(),
            table: row.table_name,
            column: row.column_name,
            mask_type: row.mask_type,
            mask_param: row.mask_param,
            source: if row.role_id.is_some() {
                MaskSource::Role
            } else {
                MaskSource::Global
            },
            role_name: row.role_name,
        });
    }
    picked.into_values().collect()
}

// ==================== 解析（带缓存） ====================

/// 解析本次请求的 [`MaskSet`]。
///
/// # 参数
///
/// - `principal`：当前用户的数据范围（**只用它的 `user_id` 与 `is_super`**；数据范围
///   本身不参与脱敏判定，见架构 §3.5「范围不参与」）；
/// - `claims`：令牌声明（只用来判定 `system:field-mask:plain`）。导出 worker 没有令牌，
///   传 `None`。
///
/// # 返回值
///
/// 已合并、可直接用于 `Maskable::mask` 的集合。
///
/// # 豁免
///
/// 超管 → [`ExemptReason::SuperAdmin`]；持明版权限 → [`ExemptReason::PlainPermission`]
/// 并**落一条审计**（[`audit_plain_read`]）。
///
/// # 缓存
///
/// 键 `perm:user_mask:{user_id}`，TTL 与主体上下文一致（300 s）。规则变更时
/// [`model::invalidate_all`] 整片清理。缓存不可用 → 回退查库（与数据权限同口径：
/// 脱敏是「限制」，查库慢但结果正确，不能因为 Redis 挂了让列表 500）。
pub async fn resolve(
    principal: Option<&ScopedPrincipal>,
    claims: Option<&TokenClaims>,
) -> DbResult<MaskSet> {
    if principal.is_some_and(ScopedPrincipal::is_super) {
        return Ok(MaskSet::new(Vec::new(), ExemptReason::SuperAdmin));
    }
    if let Some(claims) = claims
        && claims.has_permission(PLAIN_PERMISSION)
    {
        let user_id = principal.map(ScopedPrincipal::user_id);
        audit_plain_read(user_id, "field-mask");
        return Ok(MaskSet::new(Vec::new(), ExemptReason::PlainPermission));
    }
    let Some(user_id) = principal.map(ScopedPrincipal::user_id) else {
        // 没有身份（导出 worker 的 create_by 为空）：只能用全局规则，宁严不松
        let rows = model::rules_for_user(&Uuid::nil()).await?;
        let global: Vec<RawRule> = rows
            .into_iter()
            .filter(|row| row.role_id.is_none())
            .collect();
        return Ok(MaskSet::new(pick_effective(global), ExemptReason::None));
    };
    let rules = load_rules(&user_id).await?;
    Ok(MaskSet::new(rules, ExemptReason::None))
}

/// 超管豁免集合（`is_exempt()` 为 true，`hits` 恒空）。
///
/// 供自检接口 / 单测构造「看明文」的集合，避免业务代码手搓 `MaskSet { rules: vec![] }`。
pub fn resolve_super() -> MaskSet {
    MaskSet::new(Vec::new(), ExemptReason::SuperAdmin)
}

/// 按用户加载规则（**不做豁免判定**，缓存优先）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
///
/// # 返回值
///
/// 该用户的规则集合（`ExemptReason::None`）。
///
/// # 与 [`resolve`] 的分工
///
/// [`resolve`] 是「当前请求」的入口（先判超管 / 明版权限）；本函数是「某个用户会看到
/// 什么」的入口（自检接口），两者共用同一份缓存键，不会出现两套结果。
pub async fn load_rules(user_id: &Uuid) -> DbResult<Vec<MaskRule>> {
    let key = format!("{}{}", constants::CACHE_KEY_PREFIX_USER_MASK, user_id);
    if let Some(store) = try_cache()
        && let Ok(Some(cached)) = Cache::get::<Vec<MaskRule>>(store, &key).await
    {
        return Ok(cached);
    }
    let rows = model::rules_for_user(user_id).await?;
    let rules = pick_effective(rows);
    if let Some(store) = try_cache()
        && let Err(error) =
            Cache::set_ex(store, &key, &rules, constants::CACHE_PRINCIPAL_TTL_SECS).await
    {
        tracing::warn!(error = %error, %user_id, "写脱敏规则缓存失败（不影响本次结果）");
    }
    Ok(rules)
}

/// 按用户加载规则集合并包成 [`MaskSet`]（自检接口用；不做豁免判定）。
///
/// # 参数
///
/// - `user_id`：用户 ID。
pub async fn load_user_set(user_id: &Uuid) -> DbResult<MaskSet> {
    Ok(MaskSet::new(load_rules(user_id).await?, ExemptReason::None))
}

/// 构造「一条规则」的集合（**仅供单测**：生产代码只能通过 [`resolve`] 得到集合）。
///
/// # 参数
///
/// - `table` / `column`：规则作用的列；
/// - `ty`：脱敏类型；
/// - `param`：类型参数。
#[doc(hidden)]
pub fn single_rule_set(table: &str, column: &str, ty: MaskType, param: Option<&str>) -> MaskSet {
    MaskSet::new(
        vec![MaskRule {
            rule_id: format!("test-{table}.{column}"),
            table: table.to_string(),
            column: column.to_string(),
            mask_type: ty.as_i16(),
            mask_param: param.map(str::to_string),
            source: MaskSource::Global,
            role_name: None,
        }],
        ExemptReason::None,
    )
}

/// 明文访问留痕（架构 §3.6 的硬要求）。
///
/// # 为什么不用 `support::log::record_audit`
///
/// 它需要 `&HeaderMap` 与 `SocketAddr` 才能落 `request_id` / 真实 IP，而脱敏解析发生在
/// 列表 handler 的中段（很多读接口没有提取这两个），强行把它们加进每个读接口的签名
/// 是本卡不该做的侵入。这里直接往 `audit_logs` 写一条**最小可审计**记录
/// （操作类型 + 目标 + 时间 + 用户），`ip` / `user_agent` 留空并如实标注
/// 「本次留痕来自脱敏解析，无请求头上下文」。
///
/// 写入在后台任务里做，**绝不影响主请求**；失败只记 error。
///
/// # 参数
///
/// - `user_id`：看明文的人；
/// - `route`：触发明文访问的路径 / 场景标识。
pub fn audit_plain_read(user_id: Option<Uuid>, route: &str) {
    let route = route.to_string();
    tracing::info!(
        audit = "FIELD_MASK_PLAIN_READ",
        user_id = ?user_id,
        route = %route,
        "持 system:field-mask:plain 的明文访问"
    );
    let Some(user_id) = user_id else {
        return;
    };
    tokio::spawn(async move {
        let detail = serde_json::json!({
            "event": "FIELD_MASK_PLAIN_READ",
            "route": route,
            "note": "由脱敏解析写入，无请求头上下文（ip / user_agent 为空）"
        });
        if let Err(error) = model::insert_plain_read_audit(&user_id, &detail.to_string()).await {
            tracing::error!(error = %error, %user_id, "写明文档访问审计失败");
        }
    });
}

// ==================== 已登记 DTO 的 Maskable 实现 ====================

/// 用户列表项：`real_name` / `email` / `mobile` 三列。
///
/// 注意 `mask_table()` 是 `sys_user`——规则按**表 + 列**匹配，字段名相同但表不同的
/// DTO 不会互相误伤（架构 §3.3 选型表最后一列的理由）。
impl Maskable for crate::system::user::dto::UserRes {
    fn mask(&mut self, set: &MaskSet) {
        if set.is_exempt() {
            return;
        }
        mask_field("sys_user", "real_name", &mut self.real_name, set);
        mask_field("sys_user", "email", &mut self.email, set);
        mask_field("sys_user", "mobile", &mut self.mobile, set);
    }

    fn mask_table() -> &'static str {
        "sys_user"
    }
}

/// 用户详情：与列表同样的三列。
impl Maskable for crate::system::user::dto::UserDetailRes {
    fn mask(&mut self, set: &MaskSet) {
        if set.is_exempt() {
            return;
        }
        mask_field("sys_user", "real_name", &mut self.real_name, set);
        mask_field("sys_user", "email", &mut self.email, set);
        mask_field("sys_user", "mobile", &mut self.mobile, set);
    }

    fn mask_table() -> &'static str {
        "sys_user"
    }
}

/// 单字段脱敏：命中规则就替换；参数坏掉 → **全掩码**（绝不留明文）+ error 日志。
fn mask_field(table: &str, column: &str, value: &mut String, set: &MaskSet) {
    match set.mask_value(table, column, value) {
        Ok(Some(masked)) => *value = masked,
        Ok(None) => {}
        Err(error) => {
            let fallback = MaskSet::fail_closed(value);
            tracing::error!(
                table,
                column,
                error = %error.message(),
                "脱敏失败 → 已按 fail-closed 全掩码（不返回明文）"
            );
            *value = fallback;
        }
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::masking::model::RawRule;

    /// 闸 1：全字段样例构造器（**struct literal，不用 `..Default::default()`**）。
    ///
    /// 任何人给 `UserRes` / `UserDetailRes` 加字段，这里立刻编译失败 → 强制作者补样例
    /// → 补完的样例会被下面 `user_dto_masking_removes_all_sensitive_shapes` 的
    /// 敏感值扫描覆盖（架构 §3.4 闸 1 + 闸 2）。
    impl crate::system::user::dto::UserRes {
        fn sample_for_mask_test() -> Self {
            Self {
                id: "0199a0b0-0000-7000-8000-000000000001".to_string(),
                username: "alice".to_string(),
                real_name: "张三丰".to_string(),
                email: "alice@mask-sample.com".to_string(),
                mobile: "13812345678".to_string(),
                dept_id: Some("0199a0b0-0000-7000-8000-000000000002".to_string()),
                dept_name: "研发部".to_string(),
                status: 1,
                is_locked: false,
                last_login_at: None,
                created_at: chrono::Utc::now(),
                roles: vec![crate::system::user::dto::RoleBriefRes {
                    role_id: "0199a0b0-0000-7000-8000-000000000003".to_string(),
                    role_code: "TEST_VIEWER".to_string(),
                    role_name: "测试只读角色".to_string(),
                }],
            }
        }
    }

    impl crate::system::user::dto::UserDetailRes {
        fn sample_for_mask_test() -> Self {
            Self {
                id: "0199a0b0-0000-7000-8000-000000000001".to_string(),
                username: "alice".to_string(),
                real_name: "张三丰".to_string(),
                email: "alice@mask-sample.com".to_string(),
                mobile: "13812345678".to_string(),
                dept_id: Some("0199a0b0-0000-7000-8000-000000000002".to_string()),
                dept_name: "研发部".to_string(),
                status: 1,
                is_locked: false,
                last_login_at: None,
                created_at: chrono::Utc::now(),
                remark: "样例备注".to_string(),
                roles: vec![crate::system::user::dto::RoleBriefRes {
                    role_id: "0199a0b0-0000-7000-8000-000000000003".to_string(),
                    role_code: "TEST_VIEWER".to_string(),
                    role_name: "测试只读角色".to_string(),
                }],
            }
        }
    }

    fn raw(
        table: &str,
        column: &str,
        mask_type: i16,
        param: Option<&str>,
        role_id: Option<Uuid>,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> RawRule {
        RawRule {
            id: Uuid::now_v7(),
            table_name: table.to_string(),
            column_name: column.to_string(),
            mask_type,
            mask_param: param.map(str::to_string),
            role_id,
            role_name: role_id.map(|_| "测试角色".to_string()),
            created_at,
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    /// 角色规则优先于全局规则（架构 §3.1 末条）。
    #[test]
    fn role_rule_wins_over_global() {
        let role = Uuid::now_v7();
        let rules = pick_effective(vec![
            raw("sys_user", "mobile", 1, None, None, now()),
            raw("sys_user", "mobile", 4, Some("2"), Some(role), now()),
        ]);
        assert_eq!(rules.len(), 1, "同一列只应保留一条");
        assert_eq!(rules[0].source, MaskSource::Role);
        assert_eq!(rules[0].mask_type, 4);
    }

    /// 同层多条（两个角色的规则命中同一列）：按 created_at 取更早的一条，结果与入参顺序无关。
    #[test]
    fn same_layer_pick_is_order_independent() {
        let early = now();
        let late = early + chrono::Duration::seconds(60);
        let role_a = Uuid::now_v7();
        let role_b = Uuid::now_v7();
        let forward = pick_effective(vec![
            raw("sys_user", "email", 3, None, Some(role_a), early),
            raw("sys_user", "email", 4, Some("1"), Some(role_b), late),
        ]);
        let backward = pick_effective(vec![
            raw("sys_user", "email", 4, Some("1"), Some(role_b), late),
            raw("sys_user", "email", 3, None, Some(role_a), early),
        ]);
        assert_eq!(forward[0].mask_type, backward[0].mask_type);
        assert_eq!(forward[0].mask_type, 3, "应取 created_at 更早的那条");
    }

    /// 非法 mask_type 被丢弃（并由导出 fail-closed 兜住）。
    #[test]
    fn invalid_mask_type_is_dropped() {
        let rules = pick_effective(vec![raw("sys_user", "mobile", 9, None, None, now())]);
        assert!(rules.is_empty());
    }

    /// 不同表的同名列互不影响（`sys_user.real_name` vs `sys_notice.sender_name`）。
    #[test]
    fn same_column_name_different_table_does_not_cross() {
        let set = MaskSet::new(
            pick_effective(vec![raw("sys_notice", "real_name", 4, None, None, now())]),
            ExemptReason::None,
        );
        assert!(set.hits("sys_user", "real_name").is_none());
        assert!(set.hits("sys_notice", "real_name").is_some());
    }

    /// 豁免集合：`hits` 恒空。
    #[test]
    fn exempt_set_never_hits() {
        let set = MaskSet::new(
            pick_effective(vec![raw("sys_user", "mobile", 1, None, None, now())]),
            ExemptReason::SuperAdmin,
        );
        assert!(set.is_exempt());
        assert!(set.hits("sys_user", "mobile").is_none());
    }

    /// 闸 2：全字段样例 → 非超管脱敏 → 序列化扫描敏感特征。
    #[test]
    fn user_dto_masking_removes_all_sensitive_shapes() {
        let set = MaskSet::new(
            pick_effective(vec![
                raw("sys_user", "mobile", 1, None, None, now()),
                raw("sys_user", "email", 3, None, None, now()),
                raw("sys_user", "real_name", 4, None, None, now()),
            ]),
            ExemptReason::None,
        );
        let mut dto = crate::system::user::dto::UserRes::sample_for_mask_test();
        let before = serde_json::to_value(&dto).expect("样例应可序列化");
        dto.mask(&set);
        let after = serde_json::to_value(&dto).expect("脱敏后应可序列化");

        assert_ne!(
            before, after,
            "mask() 必须至少改变一个字段（防止空 mask 假通过）"
        );
        let text = serde_json::to_string(&after).expect("应可序列化");
        for (pattern, label) in [
            (r"^1[3-9]\d{9}$", "手机号"),
            (r"^\d{17}[\dXx]$", "身份证"),
            // 架构 §3.4 闸 2 的邮箱判据：`[\w.\-]+@...` 的字符集**不含 `*`**，
            // 所以 `a***@mail.com` 这种合法掩码不会被误报
            (r"[\w.\-]+@[\w.\-]+\.\w+", "邮箱"),
        ] {
            let re = regex::Regex::new(pattern).expect("测试正则应合法");
            let mut stack = vec![after.clone()];
            while let Some(value) = stack.pop() {
                match value {
                    serde_json::Value::String(s) => {
                        assert!(
                            !re.is_match(&s),
                            "脱敏后仍出现{label}形态的值：{s}（完整 JSON：{text}）"
                        );
                    }
                    serde_json::Value::Array(items) => stack.extend(items),
                    serde_json::Value::Object(map) => stack.extend(map.into_values()),
                    _ => {}
                }
            }
        }
        // 具体值断言（架构 §3.2 的示例口径）
        assert_eq!(dto.mobile, "138****5678");
        assert_eq!(dto.email, "a***@mask-sample.com");
        assert_eq!(dto.real_name, "张**");
    }

    /// 超管/明版权限：DTO 一个字段都不动。
    #[test]
    fn exempt_set_leaves_dto_untouched() {
        let set = MaskSet::new(Vec::new(), ExemptReason::SuperAdmin);
        let mut dto = crate::system::user::dto::UserDetailRes::sample_for_mask_test();
        let before = serde_json::to_value(&dto).expect("应可序列化");
        dto.mask(&set);
        let after = serde_json::to_value(&dto).expect("应可序列化");
        assert_eq!(before, after);
    }

    /// 参数坏掉 → 全掩码，且不留明文（fail-closed）。
    #[test]
    fn broken_rule_falls_back_to_full_mask() {
        let mut set_rules = pick_effective(vec![raw(
            "sys_user",
            "mobile",
            1,
            Some("oops"),
            None,
            now(),
        )]);
        set_rules[0].mask_param = Some("oops".to_string());
        let set = MaskSet::new(set_rules, ExemptReason::None);
        let mut dto = crate::system::user::dto::UserRes::sample_for_mask_test();
        dto.mask(&set);
        assert_eq!(dto.mobile, "***********", "失败必须全掩码而不是明文");
    }
}
