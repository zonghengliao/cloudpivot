//! 数据权限 · 用户范围解析（[`ScopedPrincipal`]）与 Redis 缓存（架构 §2.1 / §2.8）。
//!
//! # 一次请求只解析一次
//!
//! [`ScopedPrincipal`] 的字段**全部私有**，只能由 [`resolve`] 构造。这不是风格洁癖：
//! 它是架构 §2.4「闸 1 · 编译期」的实现——[`crate::business::shared::scope::paginate`]
//! 要求传入 `&ScopedPrincipal`，于是「想查分页就必须先解析 scope」由类型系统强制，
//! 任何绕过解析的读取都编译不过。
//!
//! # 解析形态：一次往返（3–4 表 join + 递归 CTE）
//!
//! [`RESOLVE_SQL`] 是 POC-8 的形态：`sys_user` × `sys_user_role` × `sys_role`
//! 三表 join 得到「用户的全部有效角色 + 各自 `data_scope`」，`CROSS JOIN LATERAL`
//! 里跑一段与 `system/dept/model.rs::find_dept_subtree_ids` 同形态的递归 CTE 求
//! 「本部门及以下」的部门集合，`sys_role_dept` 供 `CUSTOM` 用，最后用 `UNION ALL`
//! 把「该用户最终可见的部门 / 是否可见自己 / 是否超管」压成一批 `(kind, dept_id)` 行，
//! 由 [`merge_scope_rows`] 在应用层折成**最多两段**的固定形态（架构 §2.1：
//! 不生成嵌套 OR 树，避免 SQL 形态随角色数膨胀）。
//!
//! POC-8 实测解析成本 0.54–1.18 ms / 请求，所以必须缓存（§2.8）。
//!
//! # 缓存（cache-aside，TTL 300 s）
//!
//! - 键：`perm:user_scope:{epoch}:{user_id}`，`epoch` 是 Redis 单键
//!   `perm:user_scope_epoch`（部门树变更时 `INCR`，O(1) 全量失效）；
//! - 载的是**解析后的结果**（部门集合 + 是否可见自己），不是配置——否则每次命中
//!   还要重算递归子树，等于没缓存；
//! - **缓存不可用 → 回退查库**（不 fail-closed）。数据权限是「过滤」不是「鉴权」：
//!   查库慢但结果正确，而列表因为 Redis 挂了直接 500 是不可接受的（§2.8 末行）。
//!
//! 埋点（[`cache_stats`]）给日志与 B4 的自检接口用：命中 / 未命中 / 查库 / 缓存不可用。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use nik::prelude::*;
use nik::uuid::Uuid;

use super::scope::ScopeKind;
use crate::shared::constants;

// ==================== 缓存键（架构 §2.8） ====================
//
// 两个键常量集中定义在 `shared/constants.rs`（报批 §5.3-#11：缓存键一律集中，
// 散落会导致键冲突与失效遗漏）。这里用 `pub use` 转出，保持 `business/shared`
// 内部的引用路径稳定（`guard.rs` 与单测都按 `principal::CACHE_KEY_*` 引用）。

/// 用户数据范围缓存键前缀（完整键 `perm:user_scope:{epoch}:{user_id}`）。
///
/// 中间夹 `epoch` 是刻意的：部门树一改，`epoch` 递增，**所有**旧键即刻不可达，
/// 无需 `keys()` / `delete_pattern` 逐用户清理（大基数下 `KEYS` 是阻塞风险）。
pub use crate::shared::constants::CACHE_KEY_PREFIX_USER_SCOPE;

/// 数据范围缓存的「变更版本号」键（部门树结构变更时 `INCR`）。
pub use crate::shared::constants::CACHE_KEY_USER_SCOPE_EPOCH;

/// 组装缓存键 `perm:user_scope:{epoch}:{user_id}`。
///
/// # 参数
///
/// - `epoch`：当前 epoch（[`current_epoch`] 读出，读不到按 `0` 处理）；
/// - `user_id`：用户 ID。
pub fn cache_key(epoch: u64, user_id: &Uuid) -> String {
    format!("{CACHE_KEY_PREFIX_USER_SCOPE}{epoch}:{user_id}")
}

/// 读取当前 epoch；缓存不可用或键不存在时返回 `0`。
///
/// 键不存在（首次部署 / 刚被 `DEL`）按 `0` 处理是安全的：此时所有用户都是「未命中」，
/// 只是多查一次库，不会读到别人的范围。
pub async fn current_epoch() -> u64 {
    let Some(store) = try_cache() else {
        return 0;
    };
    Cache::get::<u64>(store, CACHE_KEY_USER_SCOPE_EPOCH)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

// ==================== 埋点 ====================

/// 数据范围缓存的进程内计数（架构 §2.8 的「埋点」）。
///
/// 只做加法，无锁（`AtomicU64`）；供 `tracing` 日志与 B4 自检接口
/// （`GET /system/data-scope/preview` 的 `cache` 字段）读取，不是持久化指标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScopeCacheStats {
    /// 命中次数（第 2 次 `resolve` 同一用户）。
    pub hits: u64,
    /// 未命中次数。
    pub misses: u64,
    /// 真正查库次数（= 未命中 + 缓存不可用的次数）。
    pub db_loads: u64,
    /// 缓存不可用次数（Redis 未初始化 / 连接失败 → 回退查库）。
    pub cache_unavailable: u64,
}

static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static DB_LOADS: AtomicU64 = AtomicU64::new(0);
static CACHE_UNAVAILABLE: AtomicU64 = AtomicU64::new(0);

/// 读取当前进程的缓存计数快照。
pub fn cache_stats() -> ScopeCacheStats {
    ScopeCacheStats {
        hits: CACHE_HITS.load(Ordering::Relaxed),
        misses: CACHE_MISSES.load(Ordering::Relaxed),
        db_loads: DB_LOADS.load(Ordering::Relaxed),
        cache_unavailable: CACHE_UNAVAILABLE.load(Ordering::Relaxed),
    }
}

/// 清零缓存计数（**仅供测试**：进程内计数没有跨用例语义）。
#[doc(hidden)]
pub fn reset_cache_stats() {
    CACHE_HITS.store(0, Ordering::Relaxed);
    CACHE_MISSES.store(0, Ordering::Relaxed);
    DB_LOADS.store(0, Ordering::Relaxed);
    CACHE_UNAVAILABLE.store(0, Ordering::Relaxed);
}

// ==================== ScopedPrincipal ====================

/// 当前用户的数据范围（一次请求解析一次，之后只读）。
///
/// # 字段为什么私有
///
/// 见模块文档「闸 1」。测试需要构造任意形态时用 `for_test`（`#[cfg(test)]`）。
#[derive(Debug, Clone)]
pub struct ScopedPrincipal {
    /// 用户 ID（SELF 范围的参数，也是缓存键的一部分）。
    user_id: Uuid,
    /// 用户所属部门（`None` = 无部门，用于「无归属列的表」上的 SELF 降级）。
    own_dept_id: Option<Uuid>,
    /// 是否持有 `SUPER_ADMIN`（按库侧 `role_code` 判定，不走 claims 快照）。
    is_super: bool,
    /// 合并后的主导范围（多角色时取最宽松的那个，见 [`merge_scope_rows`]）。
    kind: ScopeKind,
    /// 可见部门集合（`Dept` 为 1 个；`DeptAndChild` 为子树；`Custom` 为角色配置集合）。
    dept_ids: Vec<Uuid>,
    /// 是否还额外可见「自己的行」（多角色并集里出现过 `SELF`）。
    self_only: bool,
}

impl ScopedPrincipal {
    /// 内部构造（唯一入口是 [`resolve`]）。
    fn new(
        user_id: Uuid,
        own_dept_id: Option<Uuid>,
        is_super: bool,
        kind: ScopeKind,
        dept_ids: Vec<Uuid>,
        self_only: bool,
    ) -> Self {
        Self {
            user_id,
            own_dept_id,
            is_super,
            kind,
            dept_ids,
            self_only,
        }
    }

    /// 用户 ID。
    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    /// 用户所属部门。
    pub fn own_dept_id(&self) -> Option<Uuid> {
        self.own_dept_id
    }

    /// 是否超级管理员（`resolve` 按库侧角色判定，`expire_time` 过期不算）。
    pub fn is_super(&self) -> bool {
        self.is_super
    }

    /// 合并后的主导范围。
    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    /// 可见部门集合（自检 / 日志展示用）。
    pub fn dept_ids(&self) -> &[Uuid] {
        &self.dept_ids
    }

    /// 是否额外可见「自己的行」。
    pub fn self_only(&self) -> bool {
        self.self_only
    }

    /// **仅供测试**：构造任意形态的 principal。
    ///
    /// 生产代码里没有公开构造器——这是「闸 1」的一部分，不要给它加 `pub`。
    #[cfg(test)]
    pub(crate) fn for_test(
        user_id: Uuid,
        own_dept_id: Option<Uuid>,
        is_super: bool,
        kind: ScopeKind,
        dept_ids: Vec<Uuid>,
        self_only: bool,
    ) -> Self {
        Self::new(user_id, own_dept_id, is_super, kind, dept_ids, self_only)
    }
}

/// 解析结果（内存形态）：部门 ID 是强类型的 [`Uuid`]。
///
/// 与 [`CachedScope`] 分开的原因是**序列化能力**：本栈的 `uuid::Uuid` 没有开 serde
/// feature（框架 `nik` 未转发，项目里所有缓存 DTO 都用 `String` 承载 ID——例如
/// `system/dept/dto.rs::DeptNode` 的 `id: String`）。因此缓存形态用 `String`，
/// 内存形态保持强类型，两者只在缓存边界转换一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedScope {
    /// 是否超管。
    pub(crate) is_super: bool,
    /// 用户所在部门。
    pub(crate) own_dept_id: Option<Uuid>,
    /// 主导范围。
    pub(crate) kind: ScopeKind,
    /// 可见部门集合。
    pub(crate) dept_ids: Vec<Uuid>,
    /// 是否额外可见自己。
    pub(crate) self_only: bool,
}

impl ResolvedScope {
    /// 转成缓存形态（写 Redis 用）。
    fn to_cached(&self) -> CachedScope {
        CachedScope {
            is_super: self.is_super,
            own_dept_id: self.own_dept_id.map(|id| id.to_string()),
            kind: self.kind.as_i16(),
            dept_ids: self.dept_ids.iter().map(|id| id.to_string()).collect(),
            self_only: self.self_only,
        }
    }

    /// 直接转成 [`ScopedPrincipal`]（查库路径用，不必经过缓存形态）。
    fn into_principal(self, user_id: Uuid) -> ScopedPrincipal {
        ScopedPrincipal::new(
            user_id,
            self.own_dept_id,
            self.is_super,
            self.kind,
            self.dept_ids,
            self.self_only,
        )
    }
}

/// 缓存里承载的「解析结果」（序列化形态，ID 用 `String`）。
///
/// `kind` 用 `i16` 存：与 `sys_role.data_scope` 对齐，便于人工核对缓存内容。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CachedScope {
    /// 是否超管。
    pub(crate) is_super: bool,
    /// 用户所在部门。
    pub(crate) own_dept_id: Option<String>,
    /// 主导范围（1..5）。
    pub(crate) kind: i16,
    /// 可见部门集合。
    pub(crate) dept_ids: Vec<String>,
    /// 是否额外可见自己。
    pub(crate) self_only: bool,
}

// ==================== 解析：一次往返 ====================

/// 解析用的 SQL（POC-8 形态 + 三处补充）。
///
/// # 每一段在干什么
///
/// - `my_roles`：用户的**有效**角色（软删角色不算、过期绑定不算、停用角色不算——
///   与权限装载口径一致，见 `system/user/model.rs::user_holds_super_admin_role`）；
/// - `me`：用户自身（`dept_id` 可能为 `NULL`，这是 S11 的输入）；
/// - `subtree`：`me.dept_id` 的子树（递归 CTE 与 `dept::model::find_dept_subtree_ids`
///   同形态：`id` + `parent_id` 向下递归、每层带 `deleted_at IS NULL`）；
/// - `UNION ALL` 的每个分支输出一类「事实」：`ME`（存在性 + 本部门）、`SUPER`、
///   `ALL`、`DEPT`、`DEPT_CHILD`、`CUSTOM`、`SELF`、`DEPT_ROLE`
///   （最后一类是**标记**：该用户有部门类角色但解析不出部门 → 降级为 `SELF`，S10/S11）。
const RESOLVE_SQL: &str = "\
WITH my_roles AS ( \
    SELECT r.id, r.role_code, r.data_scope \
    FROM sys_user u \
    INNER JOIN sys_user_role ur ON ur.user_id = u.id \
        AND (ur.expire_time IS NULL OR ur.expire_time > now()) \
    INNER JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1 \
    WHERE u.id = ? AND u.deleted_at IS NULL \
), me AS ( \
    SELECT id, dept_id FROM sys_user WHERE id = ? AND deleted_at IS NULL \
), subtree AS ( \
    SELECT c.id AS dept_id FROM me \
    INNER JOIN sys_dept d ON d.id = me.dept_id \
    CROSS JOIN LATERAL ( \
        WITH RECURSIVE dept_tree AS ( \
            SELECT id FROM sys_dept WHERE id = d.id AND deleted_at IS NULL \
            UNION ALL \
            SELECT ch.id FROM sys_dept ch \
            INNER JOIN dept_tree t ON ch.parent_id = t.id \
            WHERE ch.deleted_at IS NULL \
        ) SELECT id FROM dept_tree \
    ) c \
) \
SELECT 'ME' AS kind, dept_id FROM me \
UNION ALL SELECT 'SUPER', NULL::uuid \
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE role_code = ?) \
UNION ALL SELECT 'ALL', NULL::uuid \
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 1) \
UNION ALL SELECT 'DEPT_ROLE', NULL::uuid \
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope IN (2, 3, 5)) \
UNION ALL SELECT 'SELF', NULL::uuid \
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 4) \
UNION ALL SELECT 'DEPT', me.dept_id FROM me \
    WHERE me.dept_id IS NOT NULL \
      AND EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 2) \
UNION ALL SELECT 'DEPT_CHILD', s.dept_id FROM subtree s \
    WHERE EXISTS (SELECT 1 FROM my_roles WHERE data_scope = 3) \
UNION ALL SELECT 'CUSTOM', rd.dept_id FROM my_roles mr \
    INNER JOIN sys_role_dept rd ON rd.role_id = mr.id \
    WHERE mr.data_scope = 5";

/// 解析结果的一行（`kind` 标签 + 部门）。
///
/// 单独建这个类型是为了让**合并逻辑成为纯函数**（[`merge_scope_rows`]）：S6/S7/S8/S10/S11
/// 全部可以脱离数据库断言，不必连库。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeRowKind {
    /// 用户自身（附带 `dept_id`，可能为 `NULL`）。
    Me,
    /// 持有超级管理员角色。
    Super,
    /// 有角色的范围是「全部」。
    All,
    /// 有一条「本部门」范围（附带部门）。
    Dept,
    /// 「本部门及以下」的某个部门。
    DeptChild,
    /// 「自定义」的某个部门。
    Custom,
    /// 有一条「仅本人」范围。
    SelfOnly,
    /// 标记：存在部门类角色（`2/3/5`），但解析不出任何部门 → 降级为 SELF。
    DeptRole,
}

impl ScopeRowKind {
    /// 从 SQL 里的 `kind` 字面量解析。
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "ME" => Some(Self::Me),
            "SUPER" => Some(Self::Super),
            "ALL" => Some(Self::All),
            "DEPT" => Some(Self::Dept),
            "DEPT_CHILD" => Some(Self::DeptChild),
            "CUSTOM" => Some(Self::Custom),
            "SELF" => Some(Self::SelfOnly),
            "DEPT_ROLE" => Some(Self::DeptRole),
            _ => None,
        }
    }
}

/// 解析结果的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopeRow {
    /// 事实类型。
    pub(crate) kind: ScopeRowKind,
    /// 部门（仅 `ME` / `DEPT` / `DEPT_CHILD` / `CUSTOM` 行有值）。
    pub(crate) dept_id: Option<Uuid>,
}

/// 把解析出的多行事实**合并**成一份生效范围（纯函数，S6/S7/S8/S10/S11 的断言点）。
///
/// # 合并规则（架构 §2.1）
///
/// 1. 超管或任一角色是 `ALL` → `All`（短路，部门集合不看）；
/// 2. 否则部门集合 = `DEPT ∪ DEPT_CHILD ∪ CUSTOM` 的并集并去重；
/// 3. `SELF` 单独存在 → `SelfOnly`；与部门范围共存 → 主导范围保持部门类
///    （`DeptAndChild` 优先于 `Custom` 优先于 `Dept`），额外置 `self_only = true`；
/// 4. **安全侧降级**：部门类角色存在但部门集合为空（`CUSTOM` 没配部门 / 用户无部门）
///    → `SelfOnly`。绝不放行（S10/S11）。
///
/// # 异常情况
///
/// - 没有 `ME` 行（用户不存在 / 已软删）→ `DbError::NotFound`（调用方是已认证请求，
///   走到这里说明会话与库不一致，应当报错而不是给一个空范围）；
/// - 缓存里的 `kind` 非法（1..5 之外）→ `DbError::InvalidValue`（缓存被污染时宁可报错）。
pub(crate) fn merge_scope_rows(rows: &[ScopeRow]) -> DbResult<ResolvedScope> {
    let mut me: Option<Option<Uuid>> = None;
    let mut is_super = false;
    let mut all = false;
    let mut self_only = false;
    let mut has_dept_role = false;
    let mut saw_child = false;
    let mut saw_custom = false;
    let mut dept_ids: Vec<Uuid> = Vec::new();

    for row in rows {
        match row.kind {
            ScopeRowKind::Me => me = Some(row.dept_id),
            ScopeRowKind::Super => is_super = true,
            ScopeRowKind::All => all = true,
            ScopeRowKind::SelfOnly => self_only = true,
            ScopeRowKind::DeptRole => has_dept_role = true,
            ScopeRowKind::Dept => {
                has_dept_role = true;
                dept_ids.extend(row.dept_id);
            }
            ScopeRowKind::DeptChild => {
                has_dept_role = true;
                saw_child = true;
                dept_ids.extend(row.dept_id);
            }
            ScopeRowKind::Custom => {
                has_dept_role = true;
                saw_custom = true;
                dept_ids.extend(row.dept_id);
            }
        }
    }

    let Some(own_dept_id) = me else {
        return Err(DbError::NotFound);
    };

    // 去重（保持稳定顺序：先按出现顺序，再由 scope 层排序编码参数）
    let mut seen = BTreeSet::new();
    dept_ids.retain(|id| seen.insert(*id));

    let (kind, self_only) = if is_super || all {
        // 短路：`ALL` / 超管时部门集合没有意义，清掉以免自检接口显示「可见 3 个部门」这种误导
        dept_ids.clear();
        (ScopeKind::All, false)
    } else if !dept_ids.is_empty() {
        let primary = if saw_child {
            ScopeKind::DeptAndChild
        } else if saw_custom {
            ScopeKind::Custom
        } else {
            ScopeKind::Dept
        };
        (primary, self_only)
    } else {
        // 部门集合为空：可能是「无角色」，也可能是 2/3/5 角色但解析不出部门
        // （CUSTOM 未配部门 / 用户 dept_id 为 NULL）→ 一律降级为「仅本人」（S10/S11）。
        // 绝不放行：配错的角色 + 无部门，最宽松的合理结果只有「看到自己的行」。
        if has_dept_role {
            tracing::debug!(
                ?own_dept_id,
                "部门类数据范围未解析出任何部门（CUSTOM 未配置或用户无部门），降级为仅本人"
            );
        }
        (ScopeKind::SelfOnly, true)
    };

    Ok(ResolvedScope {
        is_super,
        own_dept_id,
        kind,
        dept_ids,
        self_only,
    })
}

/// 由缓存形态构造 [`ScopedPrincipal`]（解析 `String` → [`Uuid`]）。
///
/// # 异常情况
///
/// 缓存里的 `kind` 不在 1..5、或 ID 不是合法 UUID（缓存被人工改坏 / 版本不兼容）→
/// `DbError::InvalidValue`。这里**不**静默降级：带着坏缓存继续跑会让「谁都在看什么」
/// 变得不可解释，宁可报错让缓存自然过期。
fn principal_from_cached(user_id: Uuid, cached: CachedScope) -> DbResult<ScopedPrincipal> {
    let kind = ScopeKind::from_i16(cached.kind)
        .map_err(|error| DbError::InvalidValue(format!("数据范围下标非法：{}", error.msg)))?;
    let own_dept_id = parse_optional_id(cached.own_dept_id.as_deref(), "own_dept_id")?;
    let mut dept_ids = Vec::with_capacity(cached.dept_ids.len());
    for raw in &cached.dept_ids {
        dept_ids.push(
            Uuid::parse_str(raw)
                .map_err(|error| DbError::InvalidValue(format!("缓存里的部门 ID 非法（{raw}）：{error}")))?,
        );
    }
    Ok(ScopedPrincipal::new(
        user_id,
        own_dept_id,
        cached.is_super,
        kind,
        dept_ids,
        cached.self_only,
    ))
}

/// 解析可选的 UUID 文本。
fn parse_optional_id(raw: Option<&str>, field: &str) -> DbResult<Option<Uuid>> {
    match raw {
        None => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|error| DbError::InvalidValue(format!("缓存里的 {field} 非法（{text}）：{error}"))),
    }
}

// ==================== 解析接缝（可注入，便于单测） ====================

/// 范围数据的来源（生产实现是 PostgreSQL，测试用假实现）。
///
/// 用 `async fn in trait` 而不是 `async_trait` 宏：本 trait 只在 crate 内用泛型参数
/// 调用（不做 `dyn` 分发），不需要 `Send` 装箱。
#[allow(async_fn_in_trait)]
pub(crate) trait ScopeSource {
    /// 查库解析一个用户的范围。
    async fn load(&self, user_id: &Uuid) -> DbResult<ResolvedScope>;
}

/// 范围数据的缓存（生产实现是全局 Redis 缓存，测试用假实现）。
#[allow(async_fn_in_trait)]
pub(crate) trait ScopeCache {
    /// 缓存后端是否可用。`false` 时 [`resolve_with`] 走回退查库。
    fn available(&self) -> bool;
    /// 当前 epoch；键不存在返回 `None`。
    async fn epoch(&self) -> Option<u64>;
    /// 读缓存；未命中 / 取值失败返回 `None`。
    async fn get(&self, key: &str) -> Option<CachedScope>;
    /// 写缓存；失败只告警（缓存不是主数据源）。
    async fn store(&self, key: &str, value: &CachedScope);
}

/// 生产实现：`db()`（框架全局数据库）。
pub(crate) struct PgScopeSource;

impl ScopeSource for PgScopeSource {
    async fn load(&self, user_id: &Uuid) -> DbResult<ResolvedScope> {
        let params = vec![
            Value::from(user_id),
            Value::from(user_id),
            Value::from(constants::SUPER_ADMIN_ROLE_CODE),
        ];
        let rows = db().find_with_params(RESOLVE_SQL, params).await?;
        let mut facts = Vec::with_capacity(rows.len());
        for row in rows {
            let label: String = row.get_typed("kind")?;
            let dept_id: Option<Uuid> = row.get_typed("dept_id")?;
            match ScopeRowKind::from_label(&label) {
                Some(kind) => facts.push(ScopeRow { kind, dept_id }),
                None => tracing::warn!(label, "数据范围解析返回了未知的事实类型，已忽略"),
            }
        }
        merge_scope_rows(&facts)
    }
}

/// 生产实现：框架全局缓存（`try_cache()`）。
///
/// # 缓存不可用时
///
/// `available()` 返回 `false`（`try_cache()` 为 `None`：未初始化或初始化失败——
/// 框架在 Redis 连接失败时只 warn 不中断启动，见 `nik-web::app` 的
/// `缓存初始化失败…将不使用缓存`）。此时 [`resolve_with`] 直接回退查库。
pub(crate) struct GlobalScopeCache;

impl ScopeCache for GlobalScopeCache {
    fn available(&self) -> bool {
        try_cache().is_some()
    }

    async fn epoch(&self) -> Option<u64> {
        let store = try_cache()?;
        Cache::get::<u64>(store, CACHE_KEY_USER_SCOPE_EPOCH)
            .await
            .ok()
            .flatten()
    }

    async fn get(&self, key: &str) -> Option<CachedScope> {
        let store = try_cache()?;
        Cache::get::<CachedScope>(store, key).await.ok().flatten()
    }

    async fn store(&self, key: &str, value: &CachedScope) {
        let Some(store) = try_cache() else {
            return;
        };
        if let Err(error) = Cache::set_ex(store, key, value, constants::CACHE_PRINCIPAL_TTL_SECS).await {
            tracing::warn!(key, error = %error, "写入用户数据范围缓存失败");
        }
    }
}

/// 解析当前用户的数据范围（**带缓存**，业务代码只调它）。
///
/// # 参数
///
/// - `user_id`：已认证请求的用户 ID（来自 `AuthClaims`，不是请求体里传来的值）。
///
/// # 返回值
///
/// [`ScopedPrincipal`]。
///
/// # 异常情况
///
/// - 用户不存在 / 已软删 → `DbError::NotFound`；
/// - 查库失败 → 原样上抛（数据权限是过滤，查库失败就是真失败，不能静默放行）。
pub async fn resolve(user_id: &Uuid) -> DbResult<ScopedPrincipal> {
    resolve_with(&PgScopeSource, &GlobalScopeCache, user_id).await
}

/// handler 侧的便捷入口：从已认证的 `AuthClaims`/`TokenClaims` 解析数据范围。
///
/// # 为什么要有它
///
/// B5 要在 9 个 handler 里各解析一次范围。若每处都手写「取 sub → 判空 → 401 →
/// `resolve` → `db_error` 包装」，18 行重复代码里只要有一处写成「取不到就跳过」，
/// 就是一次静默越权（列表退回全量）。收敛成一个函数后，「解析失败 = 请求失败」只有
/// 一处实现。
///
/// # 参数
///
/// - `claims`：中间件注入的令牌声明（业务代码**不得**用请求体里的 user_id 代替它）。
///
/// # 返回值
///
/// [`ScopedPrincipal`]。
///
/// # 异常情况
///
/// - 令牌缺少 `sub` / `sub` 非 UUID → `401 + AUTH_TOKEN_INVALID`；
/// - 查库失败 → `500`（经 [`crate::utils::db_error`] 收敛，与项目既有口径一致）。
pub async fn resolve_for_claims(claims: &TokenClaims) -> std::result::Result<ScopedPrincipal, ApiError> {
    let user_id = crate::utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            crate::shared::error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少用户标识，无法解析数据范围",
        )
    })?;
    resolve(&user_id)
        .await
        .map_err(|error| crate::utils::db_error(error, "解析当前用户数据范围"))
}

/// [`resolve`] 的可注入版本（测试用：可替换数据源与缓存）。
///
/// 三件事：读 epoch → 拼键读缓存 → 未命中/不可用则查库并回填。
pub(crate) async fn resolve_with<S: ScopeSource, C: ScopeCache>(
    source: &S,
    cache: &C,
    user_id: &Uuid,
) -> DbResult<ScopedPrincipal> {
    let available = cache.available();
    if !available {
        CACHE_UNAVAILABLE.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            %user_id,
            "数据范围缓存不可用（Redis 未初始化或连接失败），回退查库"
        );
    }

    let epoch = if available { cache.epoch().await.unwrap_or(0) } else { 0 };
    let key = cache_key(epoch, user_id);

    if available {
        if let Some(cached) = cache.get(&key).await {
            CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(%user_id, key, epoch, "用户数据范围缓存命中");
            return principal_from_cached(*user_id, cached);
        }
        CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(%user_id, key, epoch, "用户数据范围缓存未命中，查库解析");
    }

    DB_LOADS.fetch_add(1, Ordering::Relaxed);
    let loaded = source.load(user_id).await?;
    if available {
        cache.store(&key, &loaded.to_cached()).await;
    }
    Ok(loaded.into_principal(*user_id))
}

// ==================== 单元测试（S6–S11 + 缓存语义） ====================

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::business::shared::scope::{ScopeTarget, filter_for};

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn row(kind: ScopeRowKind, dept: Option<Uuid>) -> ScopeRow {
        ScopeRow { kind, dept_id: dept }
    }

    fn user(n: u128) -> Uuid {
        uid(0xFE_0000 + n)
    }

    /// S6：多角色并集 `Dept ∪ Custom` → 部门集合为并集且**去重**，SQL 仍只有 1 个 `?`。
    #[test]
    fn s6_union_dept_and_custom() {
        let dept = uid(0x6101);
        let custom_a = uid(0x6102);
        let custom_b = uid(0x6103);
        let rows = vec![
            row(ScopeRowKind::Me, Some(dept)),
            row(ScopeRowKind::DeptRole, None),
            row(ScopeRowKind::Dept, Some(dept)),
            row(ScopeRowKind::Custom, Some(custom_a)),
            row(ScopeRowKind::Custom, Some(custom_b)),
            row(ScopeRowKind::Custom, Some(dept)), // 与 DEPT 重复 → 必须去重
        ];
        let cached = merge_scope_rows(&rows).expect("合并应成功");
        assert_eq!(cached.kind, ScopeKind::Custom, "有 CUSTOM 角色时主导范围=CUSTOM");
        assert!(!cached.self_only);
        assert_eq!(cached.dept_ids.len(), 3, "去重后应有 3 个部门：{cached:?}");
        for id in [dept, custom_a, custom_b] {
            assert!(cached.dept_ids.contains(&id));
        }

        let principal = cached.into_principal(user(6));
        let filter = filter_for(&principal, ScopeTarget::SYS_USER);
        assert_eq!(filter.sql().matches('?').count(), 1, "sql={}", filter.sql());
        assert_eq!(filter.params().len(), 1);
        assert!(filter.sql().contains("= ANY("));
        println!(
            "[S6] Dept ∪ Custom → dept_set={} 个（去重），片段 `{}` ✓",
            principal.dept_ids().len(),
            filter.sql()
        );
    }

    /// S7：`All ∪ 任意` → `All`（短路）。
    #[test]
    fn s7_union_all_short_circuits() {
        let rows = vec![
            row(ScopeRowKind::Me, Some(uid(0x6201))),
            row(ScopeRowKind::All, None),
            row(ScopeRowKind::Dept, Some(uid(0x6201))),
            row(ScopeRowKind::SelfOnly, None),
        ];
        let cached = merge_scope_rows(&rows).unwrap();
        assert_eq!(cached.kind, ScopeKind::All);
        assert!(cached.dept_ids.is_empty());
        assert!(!cached.self_only);
        let principal = cached.into_principal(user(7));
        let filter = filter_for(&principal, ScopeTarget::SYS_USER);
        assert!(filter.is_noop());
        assert_eq!(filter.sql(), " AND 1 = 1");
        println!("[S7] All ∪ 任意 → kind=ALL，片段 `{}` ✓", filter.sql());
    }

    /// S8（硬门槛）：`SelfOnly ∪ Dept` → `(dept = ANY(?) OR owner = ?)`，**2 个 `?`**，
    /// 参数顺序 `[dept_array, user_id]`。
    #[test]
    fn s8_union_self_and_dept() {
        let dept = uid(0x6301);
        let rows = vec![
            row(ScopeRowKind::Me, Some(dept)),
            row(ScopeRowKind::DeptRole, None),
            row(ScopeRowKind::Dept, Some(dept)),
            row(ScopeRowKind::SelfOnly, None),
        ];
        let cached = merge_scope_rows(&rows).unwrap();
        assert_eq!(cached.kind, ScopeKind::Dept);
        assert!(cached.self_only, "并集里出现 SELF → self_only=true");
        let principal = cached.into_principal(user(8));
        let filter = filter_for(&principal, ScopeTarget::SYS_USER);
        assert_eq!(filter.sql(), " AND (u.dept_id = ANY(?::text::uuid[]) OR u.id = ?)");
        assert_eq!(filter.sql().matches('?').count(), 2, "S8 要求恰好 2 个占位符");
        assert_eq!(filter.params().len(), 2);
        assert_eq!(
            filter.params()[0],
            Value::Text(format!("{{{dept}}}")),
            "第 1 个参数必须是部门数组"
        );
        assert_eq!(filter.params()[1], Value::from(&user(8)), "第 2 个参数必须是 user_id");
        println!(
            "[S8] SelfOnly ∪ Dept → `{}`，params=[数组({dept}), user_id] ✓",
            filter.sql()
        );
    }

    /// S9：超管短路——即使库里有 `data_scope=4` 的角色，`filter_for` 恒 `All`。
    #[test]
    fn s9_super_admin_short_circuits() {
        let rows = vec![
            row(ScopeRowKind::Me, Some(uid(0x6401))),
            row(ScopeRowKind::Super, None),
            row(ScopeRowKind::DeptRole, None),
            row(ScopeRowKind::SelfOnly, None),
        ];
        let cached = merge_scope_rows(&rows).unwrap();
        assert!(cached.is_super);
        assert_eq!(cached.kind, ScopeKind::All);
        let principal = cached.into_principal(user(9));
        for target in [
            ScopeTarget::SYS_USER,
            ScopeTarget::SYS_DEPT,
            ScopeTarget::SYS_IMPORT_TASK,
            ScopeTarget::OPERATION_LOGS,
            ScopeTarget::SYS_FILE,
        ] {
            let filter = filter_for(&principal, target);
            assert!(filter.is_noop(), "{}: 超管必须恒 All", target.table);
            assert_eq!(filter.sql(), " AND 1 = 1");
        }
        println!("[S9] 超管 → 5 张登记表全部 `AND 1 = 1` ✓");
    }

    /// S10：`data_scope=5` 但没配任何部门 → **降级为 SELF**（不是 allow_all）。
    #[test]
    fn s10_empty_custom_degrades_to_self() {
        let rows = vec![
            row(ScopeRowKind::Me, Some(uid(0x6501))),
            row(ScopeRowKind::DeptRole, None), // 有 CUSTOM 角色（2/3/5）
                                               // 没有任何 CUSTOM 部门行
        ];
        let cached = merge_scope_rows(&rows).unwrap();
        assert_eq!(cached.kind, ScopeKind::SelfOnly, "配错的 CUSTOM 不能变放行");
        assert!(cached.self_only);
        assert!(cached.dept_ids.is_empty());
        let principal = cached.into_principal(user(10));
        let filter = filter_for(&principal, ScopeTarget::SYS_USER);
        assert_eq!(filter.sql(), " AND u.id = ?");
        assert!(!filter.is_noop(), "降级后的片段不是「不过滤」");
        println!("[S10] CUSTOM 空集合 → SELF，片段 `{}` ✓", filter.sql());
    }

    /// S11：用户无部门（`dept_id IS NULL`）且 `data_scope=3` → 降级为 SELF。
    #[test]
    fn s11_no_dept_degrades_to_self() {
        let rows = vec![
            row(ScopeRowKind::Me, None), // 用户没有部门
            row(ScopeRowKind::DeptRole, None),
            // 子树与 DEPT 都解析不出行
        ];
        let cached = merge_scope_rows(&rows).unwrap();
        assert_eq!(cached.kind, ScopeKind::SelfOnly);
        assert!(cached.self_only);
        assert_eq!(cached.own_dept_id, None);
        let principal = cached.into_principal(user(11));
        let filter = filter_for(&principal, ScopeTarget::SYS_USER);
        assert_eq!(filter.sql(), " AND u.id = ?");
        // 也不能生成 `dept_id = NULL`（恒空）或放行
        assert!(!filter.sql().contains("dept_id"));
        println!("[S11] 无部门 + 本部门及以下 → SELF，片段 `{}` ✓", filter.sql());
    }

    /// 附加：用户不存在（没有 `ME` 行）→ `DbError::NotFound`。
    #[test]
    fn unknown_user_is_not_found() {
        let error = merge_scope_rows(&[row(ScopeRowKind::All, None)]).expect_err("必须报错");
        assert!(matches!(error, DbError::NotFound), "实际 {error:?}");
        println!("[附加] 无 ME 行 → DbError::NotFound ✓");
    }

    /// 附加：无角色（只有 ME 行）→ 安全侧默认 SELF（不是 ALL）。
    #[test]
    fn no_roles_defaults_to_self() {
        let cached = merge_scope_rows(&[row(ScopeRowKind::Me, Some(uid(0x6701)))]).unwrap();
        assert_eq!(cached.kind, ScopeKind::SelfOnly);
        assert!(cached.self_only);
        println!("[附加] 无角色 → SELF（安全侧默认）✓");
    }

    // ---------- 缓存语义（注入假实现，等价于 Redis 的行为） ----------

    /// 假数据源：计数 + 固定返回值。
    struct CountingSource {
        loads: AtomicU64,
        dept: Uuid,
    }

    impl CountingSource {
        fn new(dept: Uuid) -> Self {
            Self {
                loads: AtomicU64::new(0),
                dept,
            }
        }
        fn loads(&self) -> u64 {
            self.loads.load(Ordering::Relaxed)
        }
    }

    impl ScopeSource for CountingSource {
        async fn load(&self, _user_id: &Uuid) -> DbResult<ResolvedScope> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            Ok(ResolvedScope {
                is_super: false,
                own_dept_id: Some(self.dept),
                kind: ScopeKind::Dept,
                dept_ids: vec![self.dept],
                self_only: false,
            })
        }
    }

    /// 假缓存：行为对齐 Redis（键带 epoch、`bump` 等价于 `INCR epoch`、TTL 由键失效模拟）。
    #[derive(Default)]
    struct FakeCache {
        available: bool,
        epoch: AtomicU64,
        entries: Mutex<std::collections::HashMap<String, CachedScope>>,
        gets: AtomicU64,
        sets: AtomicU64,
    }

    impl FakeCache {
        fn new(available: bool) -> Self {
            Self {
                available,
                ..Default::default()
            }
        }
        fn bump_epoch(&self) {
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
        fn sets(&self) -> u64 {
            self.sets.load(Ordering::Relaxed)
        }
        fn gets(&self) -> u64 {
            self.gets.load(Ordering::Relaxed)
        }
    }

    impl ScopeCache for FakeCache {
        fn available(&self) -> bool {
            self.available
        }
        async fn epoch(&self) -> Option<u64> {
            Some(self.epoch.load(Ordering::Relaxed))
        }
        async fn get(&self, key: &str) -> Option<CachedScope> {
            self.gets.fetch_add(1, Ordering::Relaxed);
            self.entries.lock().unwrap().get(key).cloned()
        }
        async fn store(&self, key: &str, value: &CachedScope) {
            self.sets.fetch_add(1, Ordering::Relaxed);
            self.entries.lock().unwrap().insert(key.to_string(), value.clone());
        }
    }

    /// 埋点是**进程内全局**的，两个缓存用例必须串行（否则互相清计数会让断言抖动）。
    static STATS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 缓存验收：同一用户连续 2 次 `resolve` → 第 2 次命中；`bump_epoch` 后第 3 次未命中。
    ///
    /// 用进程内埋点（[`cache_stats`]）断言，等价于生产日志里的 `缓存命中 / 未命中`。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 单测专用：目的就是让两个缓存用例串行访问进程内埋点
    async fn cache_hit_then_epoch_bump_invalidates() {
        let _guard = STATS_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        reset_cache_stats();
        let dept = uid(0x7101);
        let source = CountingSource::new(dept);
        let cache = FakeCache::new(true);
        let user_id = user(21);

        // 第 1 次：未命中 → 查库 → 回填
        let first = resolve_with(&source, &cache, &user_id).await.unwrap();
        let stats = cache_stats();
        assert_eq!(stats.misses, 1, "第 1 次必须未命中");
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.db_loads, 1);
        assert_eq!(source.loads(), 1);
        assert_eq!(cache.sets(), 1, "未命中后必须回填缓存");

        // 第 2 次：命中，不再查库
        let second = resolve_with(&source, &cache, &user_id).await.unwrap();
        let stats = cache_stats();
        assert_eq!(stats.hits, 1, "第 2 次必须命中缓存");
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.db_loads, 1, "命中时不得再查库");
        assert_eq!(source.loads(), 1, "数据源只被查过 1 次");
        assert_eq!(first.kind(), second.kind());
        assert_eq!(first.dept_ids(), second.dept_ids());

        // 部门树变更 → epoch INCR → 第 3 次未命中（键变了）
        cache.bump_epoch();
        let third = resolve_with(&source, &cache, &user_id).await.unwrap();
        let stats = cache_stats();
        assert_eq!(stats.misses, 2, "epoch 变化后必须未命中");
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.db_loads, 2, "epoch 变化后必须重新查库");
        assert_eq!(source.loads(), 2);
        assert_eq!(third.dept_ids(), first.dept_ids());
        assert!(cache.gets() >= 3);
        println!("[缓存] 1st miss→db(1), 2nd hit(hits=1), bump_epoch → 3rd miss(db_loads=2) ✓ stats={stats:?}");
    }

    /// 缓存验收：**缓存不可用时回退查库**，结果仍然正确（不因 Redis 故障报错）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 同上：与上一个用例共用 STATS_LOCK
    async fn cache_unavailable_falls_back_to_database() {
        let _guard = STATS_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        reset_cache_stats();
        let dept = uid(0x7201);
        let source = CountingSource::new(dept);
        let cache = FakeCache::new(false); // 等价于 try_cache() == None
        let user_id = user(22);

        let principal = resolve_with(&source, &cache, &user_id).await.unwrap();
        let stats = cache_stats();
        assert_eq!(stats.cache_unavailable, 1, "必须记录一次「缓存不可用」");
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0, "不可用时不算未命中（根本没问缓存）");
        assert_eq!(stats.db_loads, 1);
        assert_eq!(source.loads(), 1);
        assert_eq!(cache.gets(), 0, "缓存不可用时不得读缓存");
        assert_eq!(cache.sets(), 0, "缓存不可用时不得写缓存");
        assert_eq!(principal.kind(), ScopeKind::Dept);
        assert_eq!(principal.dept_ids(), &[dept]);
        println!("[回退] 缓存不可用 → 查库成功，kind=DEPT dept_set=1 ✓ stats={stats:?}");
    }

    /// 附加：`bump_epoch()` 在缓存不可用时**不报错**（返回 `None`，由 TTL 兜底）。
    #[tokio::test]
    async fn bump_epoch_without_cache_is_a_noop() {
        let epoch = super::super::guard::bump_epoch().await;
        assert!(epoch.is_none(), "缓存不可用时 bump_epoch 应返回 None 而不是 panic");
        println!("[epoch] 缓存不可用时 bump_epoch() → None（告警 + TTL 兜底）✓");
    }

    /// 缓存的键形态：`perm:user_scope:{epoch}:{user_id}`。
    #[test]
    fn cache_key_shape() {
        let user_id = uid(0x7301);
        assert_eq!(cache_key(0, &user_id), format!("perm:user_scope:0:{user_id}"));
        assert_eq!(cache_key(42, &user_id), format!("perm:user_scope:42:{user_id}"));
        assert!(cache_key(7, &user_id).starts_with(CACHE_KEY_PREFIX_USER_SCOPE));
        assert_eq!(CACHE_KEY_USER_SCOPE_EPOCH, "perm:user_scope_epoch");
        println!("[键] perm:user_scope:{{epoch}}:{{uid}} ✓");
    }

    /// 附加（**真 Redis**，需要 `B2_REDIS_URL`）：键形态 / 负载序列化 / TTL / `epoch` INCR。
    ///
    /// 与生产 [`GlobalScopeCache`] 走同一批 Redis 命令（`GET` / `SETEX` / `INCR`）。
    /// 为什么不能直接驱动 `GlobalScopeCache`：`set_cache()` 的参数类型 `SharedCache`
    /// 没有被 `nik` 门面 re-export（只导出 `Cache` / `LocalCache`），本 crate 无法在
    /// 测试进程里初始化全局缓存——所以真 Redis 的**端到端**由 B3/B4 经 `App::start`
    /// 后的 HTTP 路径验证，这里验证的是命令语义与我们的键/负载/TTL 约定。
    ///
    /// 缺 `B2_REDIS_URL` 时打印 `SKIP` 并返回（CI 无 Redis 也不红）。
    #[tokio::test]
    async fn real_redis_cache_roundtrip() {
        let Ok(url) = std::env::var("B2_REDIS_URL") else {
            println!("SKIP: 未设置 B2_REDIS_URL（不连真实 Redis）");
            return;
        };
        let client = redis::Client::open(url).expect("B2_REDIS_URL 非法");
        let mut conn = client.get_connection_manager().await.expect("连接 Redis 失败");
        let prefix = format!("b2:test:{}:", std::process::id());
        let epoch_key = format!("{prefix}epoch");
        let ttl = constants::CACHE_PRINCIPAL_TTL_SECS;
        let user_id = uid(0x7401);

        let _: i64 = redis::cmd("DEL").arg(&epoch_key).query_async(&mut conn).await.unwrap();

        // ① epoch 键不存在 → 与 current_epoch() 的语义一致（按 0 处理）
        let raw: Option<String> = redis::cmd("GET").arg(&epoch_key).query_async(&mut conn).await.unwrap();
        assert_eq!(raw, None, "首次部署时 epoch 键不存在");

        // ② INCR（等价 bump_epoch）
        let epoch: u64 = redis::cmd("INCR").arg(&epoch_key).query_async(&mut conn).await.unwrap();
        assert_eq!(epoch, 1);

        // ③ 写 + 读回（等价 Cache::set_ex / Cache::get），TTL 必须是 300 s
        let key = format!("{prefix}user_scope:{epoch}:{user_id}");
        let payload = CachedScope {
            is_super: false,
            own_dept_id: Some(uid(0x7402).to_string()),
            kind: ScopeKind::DeptAndChild.as_i16(),
            dept_ids: vec![uid(0x7402).to_string(), uid(0x7403).to_string()],
            self_only: true,
        };
        let json = serde_json::to_string(&payload).expect("负载序列化失败");
        let _: String = redis::cmd("SETEX")
            .arg(&key)
            .arg(ttl)
            .arg(&json)
            .query_async(&mut conn)
            .await
            .unwrap();
        let ttl_left: i64 = redis::cmd("TTL").arg(&key).query_async(&mut conn).await.unwrap();
        assert!(
            ttl_left > 0 && ttl_left <= i64::try_from(ttl).unwrap(),
            "TTL 必须落在 (0, {ttl}]，实际 {ttl_left}"
        );
        let read: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut conn).await.unwrap();
        let decoded: CachedScope = serde_json::from_str(&read.expect("刚写入的键必须能读回")).unwrap();
        assert_eq!(decoded.kind, payload.kind);
        assert_eq!(decoded.dept_ids, payload.dept_ids);
        assert!(decoded.self_only);

        // ④ 再 INCR → 新键未命中（旧键仍在，靠 TTL 自然回收 —— epoch 方案的取舍）
        let epoch2: u64 = redis::cmd("INCR").arg(&epoch_key).query_async(&mut conn).await.unwrap();
        assert_eq!(epoch2, 2);
        let key2 = format!("{prefix}user_scope:{epoch2}:{user_id}");
        assert_ne!(key, key2, "epoch 变化必须换来新键");
        let missing: Option<String> = redis::cmd("GET").arg(&key2).query_async(&mut conn).await.unwrap();
        assert_eq!(missing, None, "epoch 递增后新键必然未命中");
        // 生产键函数与测试里手拼的形态一致（防止两处漂移）
        assert_eq!(
            cache_key(epoch, &user_id),
            format!("{CACHE_KEY_PREFIX_USER_SCOPE}{epoch}:{user_id}")
        );

        // 清理
        let _: i64 = redis::cmd("DEL")
            .arg(&[&epoch_key, &key, &key2])
            .query_async(&mut conn)
            .await
            .unwrap();
        println!("[真Redis] INCR epoch 1→2；键 `{key}` TTL={ttl_left}s；读回 serde 一致；新键未命中 ✓");
    }
}
