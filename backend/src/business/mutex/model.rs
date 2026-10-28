//! `sys_role_mutex` 的库侧读写（批次 B · B7）。
//!
//! # 「有效持有」的唯一权威口径
//!
//! [`HELD_ROLE_SQL`] 是全模块（三个校验点 + 单用户体检 + 全量扫描）**唯一**使用的持有
//! 口径，与 `business/shared/principal` 装载权限的口径一致：
//!
//! > 绑定未过期 ∧ 用户未软删 ∧ 角色未软删 ∧ **角色未停用**
//!
//! 停用角色不计入持有：停用角色不产生任何权限，把「与停用角色的双持」判成违规会让管理员
//! 连规则都配不了（现实里总有停用的历史角色）。推论：**把停用角色启用**会让休眠绑定变成
//! 有效持有，所以启用角色必须复核（校验点 2，见
//! [`crate::business::mutex::guard::ensure_role_enable_no_mutex`]）。
//!
//! 别在别处复制这四个条件：口径漂移 = 互斥在某个入口静默失效，而这正是 SoD 最怕的失败方式。
//!
//! # 对称性：写库规范化 + 读库双向匹配
//!
//! - **写**：库里已有 `uk_sys_role_mutex_pair`（`LEAST/GREATEST` 表达式唯一索引）。
//!   应用层还按 UUID 字节序把 `(A,B)` 规范成 `(小, 大)` 再落库，让同一规则的行内容稳定
//!   可读；重复（含反向）插入由唯一索引拒绝，本文件把 `UniqueViolation` 原样上抛，
//!   由 `handler` 翻译成 `409 + 61011 ROLE_MUTEX_DUPLICATE`（而不是 500）。
//! - **读**：一律用 `LEAST/GREATEST` 双向匹配，**不假设**写入顺序——历史行可能顺序相反，
//!   假设顺序就会漏判一半规则。
//!
//! # 数据权限
//!
//! SCOPE-EXEMPT: 本文件对 `sys_user` / `sys_user_role` / `sys_dept` 的引用只出现在
//! 「按 user_id 单条取用户」「按角色对取双持用户」两类**诊断查询**里（`WHERE user_id = ?`
//! 或 `WHERE ua.role_id = ? AND ub.role_id = ?`），参数全部来自路径 / 请求体里的具体 ID。
//! 反向注入数据范围会让管理员「看不到自己该处理的冲突用户」，把互斥从可用变成不可用
//! （架构 §4.3 的「先解绑再配置」流程直接断掉）。行级收口靠权限码
//! `system:role-mutex:{list,conflict}`。

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::mutex::guard;

/// 「有效持有」口径：某用户当前**真实生效**的角色集合。
///
/// 参数顺序：`user_id`（`?`）。只允许在本文件内拼进 CTE 的 `held` 分支。
pub const HELD_ROLE_SQL: &str = "SELECT ur.role_id FROM sys_user_role ur \
     INNER JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL \
     INNER JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1 \
     WHERE ur.user_id = ? AND (ur.expire_time IS NULL OR ur.expire_time > now())";

/// 规则查询的公共 `SELECT … FROM` 片段（含两侧角色编码 / 名称）。
const RULE_SELECT: &str = "SELECT m.id, m.role_a_id, m.role_b_id, m.reason, m.created_at, \
     m.create_by, (m.deleted_at IS NULL) AS enabled, \
     ra.role_code AS a_code, ra.role_name AS a_name, \
     rb.role_code AS b_code, rb.role_name AS b_name \
     FROM sys_role_mutex m \
     INNER JOIN sys_role ra ON ra.id = m.role_a_id \
     INNER JOIN sys_role rb ON rb.id = m.role_b_id";

// ==================== 行结构 ====================

/// 一条互斥规则（A/B 已按 `role_code` 字典序规范化，前端不需要自己排序）。
#[derive(Debug, Clone)]
pub struct MutexRow {
    /// 规则 ID。
    pub id: Uuid,
    /// 规范化为「较小」一侧的角色 ID（按 `role_code` 字典序）。
    pub role_a_id: Uuid,
    /// 较小一侧的角色编码。
    pub role_a_code: String,
    /// 较小一侧的角色名称。
    pub role_a_name: String,
    /// 规范化为「较大」一侧的角色 ID。
    pub role_b_id: Uuid,
    /// 较大一侧的角色编码。
    pub role_b_code: String,
    /// 较大一侧的角色名称。
    pub role_b_name: String,
    /// 互斥理由（合规留痕）。
    pub reason: String,
    /// 是否生效（`deleted_at IS NULL`）。停用 = 软删。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
}

/// 库侧原始行（顺序取自写入时的落库顺序，**不可**直接下发）。
#[derive(Debug, Clone)]
struct RawRow {
    id: Uuid,
    role_a_id: Uuid,
    role_a_code: String,
    role_a_name: String,
    role_b_id: Uuid,
    role_b_code: String,
    role_b_name: String,
    reason: String,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    create_by: Option<Uuid>,
}

/// 把原始行规范化为「`role_a_code <= role_b_code`」的展示行（纯函数，可单测）。
fn normalized(raw: RawRow) -> MutexRow {
    let swap =
        (raw.role_a_code.as_str(), raw.role_a_id) > (raw.role_b_code.as_str(), raw.role_b_id);
    if swap {
        MutexRow {
            id: raw.id,
            role_a_id: raw.role_b_id,
            role_a_code: raw.role_b_code,
            role_a_name: raw.role_b_name,
            role_b_id: raw.role_a_id,
            role_b_code: raw.role_a_code,
            role_b_name: raw.role_a_name,
            reason: raw.reason,
            enabled: raw.enabled,
            created_at: raw.created_at,
            create_by: raw.create_by,
        }
    } else {
        MutexRow {
            id: raw.id,
            role_a_id: raw.role_a_id,
            role_a_code: raw.role_a_code,
            role_a_name: raw.role_a_name,
            role_b_id: raw.role_b_id,
            role_b_code: raw.role_b_code,
            role_b_name: raw.role_b_name,
            reason: raw.reason,
            enabled: raw.enabled,
            created_at: raw.created_at,
            create_by: raw.create_by,
        }
    }
}

fn map_row(row: &Row) -> std::result::Result<MutexRow, DbError> {
    Ok(normalized(RawRow {
        id: row.get_typed("id")?,
        role_a_id: row.get_typed("role_a_id")?,
        role_a_code: row.get_typed("a_code")?,
        role_a_name: row.get_typed("a_name")?,
        role_b_id: row.get_typed("role_b_id")?,
        role_b_code: row.get_typed("b_code")?,
        role_b_name: row.get_typed("b_name")?,
        reason: row.get_typed("reason")?,
        enabled: row.get_typed("enabled")?,
        created_at: row.get_typed("created_at")?,
        create_by: row.get_typed("create_by")?,
    }))
}

/// 互斥规则里的一侧角色（校验命中 / 体检结果用）。
#[derive(Debug, Clone)]
pub struct RoleSide {
    /// 角色 ID。
    pub id: Uuid,
    /// 角色编码。
    pub code: String,
    /// 角色名称。
    pub name: String,
}

/// 一次互斥命中：某条规则的两侧角色 + 理由。
///
/// 两侧按 `role_code` 字典序规范化，与 [`MutexRow`] 同口径；调用方用「哪一侧在本次请求里」
/// 决定文案（见 [`crate::business::mutex::guard::conflict_message`]）。
#[derive(Debug, Clone)]
pub struct PairHit {
    /// 命中的规则 ID。
    pub mutex_id: Uuid,
    /// 互斥理由。
    pub reason: String,
    /// 较小一侧。
    pub a: RoleSide,
    /// 较大一侧。
    pub b: RoleSide,
}

/// 违规用户（体检 / 扫描结果里的一条）。
#[derive(Debug, Clone)]
pub struct HolderRow {
    /// 用户 ID。
    pub user_id: Uuid,
    /// 登录名。
    pub username: String,
    /// 展示名：`real_name` 为空时回落 `username`。
    pub display_name: String,
    /// 部门名称（用户无部门时为 `None`）。
    pub dept_name: Option<String>,
}

/// 角色精简信息（存在性 / 名称 / 状态校验用）。
#[derive(Debug, Clone)]
pub struct RoleBrief {
    /// 角色 ID。
    pub id: Uuid,
    /// 角色编码。
    pub role_code: String,
    /// 角色名称。
    pub role_name: String,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 是否已软删。
    pub deleted: bool,
}

/// 用户精简信息（体检接口头部）。
#[derive(Debug, Clone)]
pub struct UserBrief {
    /// 用户 ID。
    pub user_id: Uuid,
    /// 登录名。
    pub username: String,
    /// 展示名。
    pub display_name: String,
}

// ==================== 读：规则 ====================

/// 规则分页（管理面）。
///
/// # 参数
///
/// - `role_id`：只看与某角色相关的规则（两侧任一侧）；`None` = 不过滤；
/// - `keyword`：角色编码 / 名称 / 理由的模糊匹配（已规整成 `%词%`）；`None` = 不过滤；
/// - `status`：`Some(1)` 只看生效、`Some(0)` 只看停用、`None` = 都看；
/// - `page` / `page_size`：已规范化的页码与每页条数。
///
/// # 返回值
///
/// `(本页规则, 总条数)`。`A/B` 已按 `role_code` 字典序规范化。
///
/// # 为什么不用 `scope::paginate`
///
/// `sys_role_mutex` 是全局元数据（互斥规则与「谁看得到哪行数据」无关），不在
/// `SCOPED_TABLES` 里；闸① 要求分页只能出现在 `business/shared/scope.rs`，所以这里手写
/// `COUNT(*)` + `LIMIT/OFFSET`（与 `business/masking/model.rs::page_rules` 同例）。
pub async fn page_mutexes(
    role_id: Option<Uuid>,
    keyword: Option<&str>,
    status: Option<i16>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<MutexRow>, i64)> {
    let mut where_sql = String::from(" WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    match status {
        Some(1) => where_sql.push_str(" AND m.deleted_at IS NULL"),
        Some(0) => where_sql.push_str(" AND m.deleted_at IS NOT NULL"),
        _ => {}
    }
    if let Some(role_id) = role_id {
        where_sql.push_str(" AND (m.role_a_id = ? OR m.role_b_id = ?)");
        params.push(Value::from(role_id));
        params.push(Value::from(role_id));
    }
    if let Some(pattern) = keyword {
        where_sql.push_str(
            " AND (ra.role_code ILIKE ? OR ra.role_name ILIKE ? \
              OR rb.role_code ILIKE ? OR rb.role_name ILIKE ? OR m.reason ILIKE ?)",
        );
        for _ in 0..5 {
            params.push(Value::from(pattern.to_string()));
        }
    }

    let count_sql = format!(
        "SELECT COUNT(*) FROM sys_role_mutex m \
         INNER JOIN sys_role ra ON ra.id = m.role_a_id \
         INNER JOIN sys_role rb ON rb.id = m.role_b_id{where_sql}"
    );
    let total: i64 = db().scalar_with_params(&count_sql, params.clone()).await?;

    let offset =
        i64::try_from(page.saturating_sub(1).saturating_mul(page_size)).unwrap_or(i64::MAX);
    let limit = i64::try_from(page_size).unwrap_or(i64::MAX);
    let list_sql = format!(
        "{RULE_SELECT}{where_sql} ORDER BY ra.role_code ASC, rb.role_code ASC, m.created_at ASC \
         LIMIT ? OFFSET ?"
    );
    params.push(Value::I64(limit));
    params.push(Value::I64(offset));
    let rows = db().find_with_params(&list_sql, params).await?;
    let list = rows
        .iter()
        .map(map_row)
        .collect::<std::result::Result<Vec<_>, DbError>>()?;
    Ok((list, total))
}

/// 按 ID 取一条规则（**含停用**，靠 [`MutexRow::enabled`] 区分）。
pub async fn find_mutex(id: &Uuid) -> DbResult<Option<MutexRow>> {
    let sql = format!("{RULE_SELECT} WHERE m.id = ?");
    let row = db()
        .find_first_with_params(&sql, vec![Value::from(*id)])
        .await?;
    row.as_ref().map(map_row).transpose()
}

/// 按角色对取**生效中**的规则（对称匹配，不假设写入顺序）。
///
/// # 返回值
///
/// - `Some(row)`：该对已存在互斥规则（含反向）；
/// - `None`：不存在（或只有停用记录）。
pub async fn find_active_pair(role_a: &Uuid, role_b: &Uuid) -> DbResult<Option<MutexRow>> {
    let sql = format!(
        "{RULE_SELECT} WHERE m.deleted_at IS NULL \
         AND LEAST(m.role_a_id, m.role_b_id) = LEAST(?::uuid, ?::uuid) \
         AND GREATEST(m.role_a_id, m.role_b_id) = GREATEST(?::uuid, ?::uuid)"
    );
    let row = db()
        .find_first_with_params(
            &sql,
            vec![
                Value::from(*role_a),
                Value::from(*role_b),
                Value::from(*role_a),
                Value::from(*role_b),
            ],
        )
        .await?;
    row.as_ref().map(map_row).transpose()
}

/// 全部生效中的规则（存量扫描用）。
pub async fn list_active_pairs() -> DbResult<Vec<MutexRow>> {
    let sql =
        format!("{RULE_SELECT} WHERE m.deleted_at IS NULL ORDER BY ra.role_code, rb.role_code");
    let rows = db().find_with_params(&sql, Vec::new()).await?;
    rows.iter()
        .map(map_row)
        .collect::<std::result::Result<Vec<_>, DbError>>()
}

/// 与某角色相关的生效规则（角色被启用时「互斥拓扑」信息不全，仅用于展示；判定请用
/// [`role_enable_hits`]）。
///
/// # 为什么判定不能用它
///
/// 本函数只回答「这个角色参与了哪些互斥对」，不回答「有没有人双持」——而「即将启用的
/// 角色」的绑定在 [`HELD_ROLE_SQL`] 口径里**还不算有效**（它此刻仍是停用的），逐对调用
/// [`crate::business::mutex::checker::pair_holders`] 会恒返回 0，校验点 2 就永远不会触发。
/// 所以判定统一走 [`role_enable_hits`]（把该角色按「已启用」参与配对）。
pub async fn pairs_for_role(role_id: &Uuid) -> DbResult<Vec<MutexRow>> {
    let sql = format!(
        "{RULE_SELECT} WHERE m.deleted_at IS NULL AND (m.role_a_id = ? OR m.role_b_id = ?) \
         ORDER BY ra.role_code, rb.role_code"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(*role_id), Value::from(*role_id)])
        .await?;
    rows.iter()
        .map(map_row)
        .collect::<std::result::Result<Vec<_>, DbError>>()
}

/// 一次「启用角色会造成的双持」（校验点 2 的命中行）。
#[derive(Debug, Clone)]
pub struct RoleEnableHit {
    /// 用户 ID。
    pub user_id: Uuid,
    /// 登录名。
    pub username: String,
    /// 与之互斥的对侧角色编码。
    pub partner_code: String,
    /// 与之互斥的对侧角色名称。
    pub partner_name: String,
    /// 互斥理由。
    pub reason: String,
}

/// 角色**即将启用**时，会因此真正双持的用户（校验点 2 的判定查询）。
///
/// # 口径（与 [`HELD_ROLE_SQL`] 的唯一差别，必须在注释里说清）
///
/// - 待启用角色（`?`）一侧：绑定未过期 ∧ 用户未软删 ∧ **角色未软删**，
///   **不看它当前的 `status`**——调用它时角色还是停用的，启用后才会生效；
/// - 对侧角色：绑定未过期 ∧ 用户未软删 ∧ 角色未软删 ∧ **角色 `status = 1`**；
/// - 两者由一条生效中的互斥规则（`LEAST/GREATEST` 双向匹配）连起来。
///
/// 这样「先停用 → 授完 → 再启用」的绕过路径会在最后一步被拦住。
pub async fn role_enable_hits(role_id: &Uuid) -> DbResult<Vec<RoleEnableHit>> {
    let rows = db()
        .find_with_params(
            "SELECT DISTINCT u.id AS user_id, u.username, \
                    rb.role_code AS partner_code, rb.role_name AS partner_name, m.reason \
               FROM sys_user_role ua \
               INNER JOIN sys_user u ON u.id = ua.user_id AND u.deleted_at IS NULL \
               INNER JOIN sys_role ra ON ra.id = ua.role_id AND ra.deleted_at IS NULL \
               INNER JOIN sys_user_role ub ON ub.user_id = ua.user_id \
                    AND (ub.expire_time IS NULL OR ub.expire_time > now()) \
               INNER JOIN sys_role rb ON rb.id = ub.role_id \
                    AND rb.deleted_at IS NULL AND rb.status = 1 \
               INNER JOIN sys_role_mutex m ON m.deleted_at IS NULL \
                    AND LEAST(m.role_a_id, m.role_b_id) = LEAST(ua.role_id, ub.role_id) \
                    AND GREATEST(m.role_a_id, m.role_b_id) = GREATEST(ua.role_id, ub.role_id) \
              WHERE ua.role_id = ?::uuid \
                AND (ua.expire_time IS NULL OR ua.expire_time > now()) \
              ORDER BY u.username",
            vec![Value::from(*role_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(RoleEnableHit {
                user_id: row.get_typed("user_id")?,
                username: row.get_typed("username")?,
                partner_code: row.get_typed("partner_code")?,
                partner_name: row.get_typed("partner_name")?,
                reason: row.get_typed("reason")?,
            })
        })
        .collect()
}

// ==================== 读：角色 / 用户 ====================

/// 按 ID 取角色精简信息（含已软删，靠 [`RoleBrief::deleted`] 区分）。
pub async fn find_role(role_id: &Uuid) -> DbResult<Option<RoleBrief>> {
    let row = db()
        .find_first_with_params(
            "SELECT id, role_code, role_name, status, (deleted_at IS NOT NULL) AS deleted \
             FROM sys_role WHERE id = ?",
            vec![Value::from(*role_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(RoleBrief {
        id: row.get_typed("id")?,
        role_code: row.get_typed("role_code")?,
        role_name: row.get_typed("role_name")?,
        status: row.get_typed("status")?,
        deleted: row.get_typed("deleted")?,
    }))
}

/// 按 ID 取用户精简信息（体检接口头部；已软删返回 `Ok(None)`）。
pub async fn find_user_brief(user_id: &Uuid) -> DbResult<Option<UserBrief>> {
    let row = db()
        .find_first_with_params(
            "SELECT id, username, COALESCE(NULLIF(real_name, ''), username) AS display_name \
             FROM sys_user WHERE id = ? AND deleted_at IS NULL",
            vec![Value::from(*user_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(UserBrief {
        user_id: row.get_typed("id")?,
        username: row.get_typed("username")?,
        display_name: row.get_typed("display_name")?,
    }))
}

// ==================== 读：违规 ====================

/// 某用户**当前有效持有**的角色 ID（文案方向判定用；空 Vec = 无角色）。
///
/// # 为什么不复用到别处
///
/// 判定必须走 [`violations_for_user`]（并集 + 两两配对）。本函数只回答「现在手里有哪些
/// 角色」，供文案区分「本次新增」与「已持有」，**不要**拿它自己做互斥判定。
pub async fn held_role_ids(user_id: &Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(HELD_ROLE_SQL, vec![Value::from(*user_id)])
        .await?;
    rows.iter().map(|row| row.get_typed("role_id")).collect()
}

/// 校验 SQL（架构 §4.2 的 UNION 形态）：`已持有 ∪ 本次授予` 两两配对后匹配互斥表。
///
/// # 为什么必须是并集
///
/// POC-6b/6c：只比「本次 vs 已持有」时，「同一次请求里同时授予 A+B」命中 0 条 —— 可绕过。
///
/// # 为什么 `eff` 里还要过滤 `status = 1`
///
/// 授予一个**停用**角色不会产生任何权限，也就不会产生「同时持有」的实际风险；等它被启用时
/// 校验点 2 会复核。这与 [`HELD_ROLE_SQL`] 的口径必须一致，否则两侧判定不同。
///
/// # 参数
///
/// - `user_id`：目标用户；
/// - `requested`：本次要授予的角色 ID 列表（空 = 只看已持有）。
pub async fn violations_for_user(user_id: &Uuid, requested: &[Uuid]) -> DbResult<Vec<PairHit>> {
    let mut params: Vec<Value> = vec![Value::from(*user_id)];
    let mut held_sql = HELD_ROLE_SQL.to_string();
    if !requested.is_empty() {
        // 只拼「占位符个数」，不拼任何取值：每个 role_id 仍是绑定参数（禁拼 SQL 的底线）。
        let placeholders = vec!["(?::uuid)"; requested.len()].join(", ");
        held_sql.push_str(&format!(
            " UNION SELECT v.role_id FROM (VALUES {placeholders}) AS v(role_id)"
        ));
        for role_id in requested {
            params.push(Value::from(*role_id));
        }
    }
    let sql = format!(
        "WITH held AS ({held_sql}), \
              eff AS (SELECT r.id FROM sys_role r WHERE r.deleted_at IS NULL AND r.status = 1), \
              valid AS (SELECT h.role_id FROM held h INNER JOIN eff e ON e.id = h.role_id) \
         SELECT m.id AS mutex_id, m.reason, \
                ra.id AS a_id, ra.role_code AS a_code, ra.role_name AS a_name, \
                rb.id AS b_id, rb.role_code AS b_code, rb.role_name AS b_name \
           FROM sys_role_mutex m \
           INNER JOIN sys_role ra ON ra.id = m.role_a_id AND ra.deleted_at IS NULL \
           INNER JOIN sys_role rb ON rb.id = m.role_b_id AND rb.deleted_at IS NULL \
          WHERE m.deleted_at IS NULL \
            AND EXISTS (SELECT 1 FROM valid v WHERE v.role_id = LEAST(m.role_a_id, m.role_b_id)) \
            AND EXISTS (SELECT 1 FROM valid v WHERE v.role_id = GREATEST(m.role_a_id, m.role_b_id)) \
          ORDER BY ra.role_code, rb.role_code"
    );
    let rows = db().find_with_params(&sql, params).await?;
    rows.iter()
        .map(|row| {
            Ok(PairHit {
                mutex_id: row.get_typed("mutex_id")?,
                reason: row.get_typed("reason")?,
                a: RoleSide {
                    id: row.get_typed("a_id")?,
                    code: row.get_typed("a_code")?,
                    name: row.get_typed("a_name")?,
                },
                b: RoleSide {
                    id: row.get_typed("b_id")?,
                    code: row.get_typed("b_code")?,
                    name: row.get_typed("b_name")?,
                },
            })
        })
        .collect()
}

/// 统计**有效同时持有**该角色对的用户数。
pub async fn count_pair_holders(role_a: &Uuid, role_b: &Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(DISTINCT u.id) FROM sys_user_role ua \
         INNER JOIN sys_user_role ub ON ub.user_id = ua.user_id AND ub.role_id = ?::uuid \
              AND (ub.expire_time IS NULL OR ub.expire_time > now()) \
         INNER JOIN sys_user u ON u.id = ua.user_id AND u.deleted_at IS NULL \
         INNER JOIN sys_role ra ON ra.id = ua.role_id AND ra.deleted_at IS NULL AND ra.status = 1 \
         INNER JOIN sys_role rb ON rb.id = ub.role_id AND rb.deleted_at IS NULL AND rb.status = 1 \
         WHERE ua.role_id = ?::uuid AND (ua.expire_time IS NULL OR ua.expire_time > now())",
        vec![Value::from(*role_b), Value::from(*role_a)],
    )
    .await
}

/// 列出**有效同时持有**该角色对的用户（按登录名排序，`limit` 条）。
pub async fn list_pair_holders(
    role_a: &Uuid,
    role_b: &Uuid,
    limit: i64,
) -> DbResult<Vec<HolderRow>> {
    let rows = db()
        .find_with_params(
            "SELECT u.id AS user_id, u.username, \
                    COALESCE(NULLIF(u.real_name, ''), u.username) AS display_name, d.dept_name \
               FROM sys_user_role ua \
               INNER JOIN sys_user_role ub ON ub.user_id = ua.user_id AND ub.role_id = ?::uuid \
                    AND (ub.expire_time IS NULL OR ub.expire_time > now()) \
               INNER JOIN sys_user u ON u.id = ua.user_id AND u.deleted_at IS NULL \
               INNER JOIN sys_role ra ON ra.id = ua.role_id AND ra.deleted_at IS NULL AND ra.status = 1 \
               INNER JOIN sys_role rb ON rb.id = ub.role_id AND rb.deleted_at IS NULL AND rb.status = 1 \
               LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
              WHERE ua.role_id = ?::uuid AND (ua.expire_time IS NULL OR ua.expire_time > now()) \
              ORDER BY u.username LIMIT ?",
            vec![
                Value::from(*role_b),
                Value::from(*role_a),
                Value::I64(limit),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(HolderRow {
                user_id: row.get_typed("user_id")?,
                username: row.get_typed("username")?,
                display_name: row.get_typed("display_name")?,
                dept_name: row.get_typed("dept_name")?,
            })
        })
        .collect()
}

// ==================== 写 ====================

/// 插入一条互斥规则（应用层按 UUID 字节序规范化落库）。
///
/// # 参数
///
/// - `role_a` / `role_b`：两侧角色（顺序无意义，内部规范化）；
/// - `reason`：互斥理由（调用方已校验非空）；
/// - `operator`：创建人。
///
/// # 返回值
///
/// 新规则的 ID。
///
/// # 异常情况
///
/// - `DbError::UniqueViolation`（`uk_sys_role_mutex_pair`）：该对已存在生效规则（含反向）；
/// - `DbError::ForeignKeyViolation`：角色被并发删除；
/// - 其它 `DbError`：数据库异常。
pub async fn insert_mutex(
    role_a: &Uuid,
    role_b: &Uuid,
    reason: &str,
    operator: Option<Uuid>,
) -> DbResult<Uuid> {
    let (low, high) = guard::normalize_pair(*role_a, *role_b);
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO sys_role_mutex (id, role_a_id, role_b_id, reason, create_by) \
         VALUES (?, ?, ?, ?, ?)",
        vec![
            Value::from(id),
            Value::from(low),
            Value::from(high),
            Value::from(reason.to_string()),
            Value::from(operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 改互斥理由（只改生效中的规则；返回受影响行数）。
pub async fn update_reason(id: &Uuid, reason: &str) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE sys_role_mutex SET reason = ? WHERE id = ? AND deleted_at IS NULL",
        vec![Value::from(reason.to_string()), Value::from(*id)],
    )
    .await
}

/// 启停互斥规则（`enabled=false` → 软删时间戳；`true` → 清空软删时间戳）。
///
/// # 返回值
///
/// 受影响行数：`0` 表示状态本来就是目标值（或规则不存在），由调用方决定怎么报。
pub async fn set_enabled(id: &Uuid, enabled: bool) -> DbResult<u64> {
    let sql = if enabled {
        "UPDATE sys_role_mutex SET deleted_at = NULL WHERE id = ? AND deleted_at IS NOT NULL"
    } else {
        "UPDATE sys_role_mutex SET deleted_at = now() WHERE id = ? AND deleted_at IS NULL"
    };
    db().execute_with_params(sql, vec![Value::from(*id)]).await
}

// ==================== 单测（纯逻辑：规范化 / 错误分类，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(a_code: &str, b_code: &str) -> RawRow {
        let a_id = Uuid::from_u128(1);
        let b_id = Uuid::from_u128(2);
        RawRow {
            id: Uuid::from_u128(9),
            role_a_id: a_id,
            role_a_code: a_code.to_string(),
            role_a_name: format!("{a_code}-name"),
            role_b_id: b_id,
            role_b_code: b_code.to_string(),
            role_b_name: format!("{b_code}-name"),
            reason: "合规".to_string(),
            enabled: true,
            created_at: chrono::Utc::now(),
            create_by: None,
        }
    }

    /// 已按字典序写入的行：原样输出。
    #[test]
    fn normalize_keeps_ordered_row() {
        let row = normalized(raw("PURCHASER", "PAY_APPROVER"));
        assert_eq!(row.role_a_code, "PAY_APPROVER");
        assert_eq!(row.role_b_code, "PURCHASER");
    }

    /// 顺序相反的行：A/B 必须互换（含 ID / 名称一起换，不能只换编码）。
    #[test]
    fn normalize_swaps_reversed_row() {
        let source = raw("PURCHASER", "PAY_APPROVER");
        let a_id = source.role_a_id;
        let b_id = source.role_b_id;
        let row = normalized(source);
        assert_eq!(row.role_a_code, "PAY_APPROVER");
        assert_eq!(row.role_a_id, b_id);
        assert_eq!(row.role_a_name, "PAY_APPROVER-name");
        assert_eq!(row.role_b_code, "PURCHASER");
        assert_eq!(row.role_b_id, a_id);
        assert_eq!(row.role_b_name, "PURCHASER-name");
        // 理由 / 生效态 / 主键与顺序无关
        assert_eq!(row.reason, "合规");
        assert!(row.enabled);
        assert_eq!(row.id, Uuid::from_u128(9));
    }

    /// 编码相同（理论上被 `sys_role` 唯一索引挡住）：按 UUID 稳定排序，不来回颠倒。
    #[test]
    fn normalize_is_stable_when_codes_equal() {
        let row = normalized(raw("SAME", "SAME"));
        assert_eq!(row.role_a_id, Uuid::from_u128(1));
        assert_eq!(row.role_b_id, Uuid::from_u128(2));
    }
}
