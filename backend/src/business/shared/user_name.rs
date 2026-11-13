//! 用户姓名**快照**（审计列专用）—— **AUD-01**。
//!
//! # 这里只做一件事
//!
//! 给定 `sys_user.id`，取一个**可以落库、永不回改**的姓名字符串。
//! 给「审计 / 历史类」列用：附件上传人、任务附件上传人（本卡），
//! 以及后续同类审计列（状态日志 `operator_name`、审批 `assignee_name` 等已有各自实现，
//! 新代码可以直接用本模块，口径统一）。
//!
//! # 为什么审计列必须存快照，而不是实时 join
//!
//! 判别标准见 `AGENTS.md` 约定 13（2026-11-08 用户反馈「到处都有只存 id 不存姓名」）：
//!
//! | 类别 | 口径 | 例子 |
//! |------|------|------|
//! | 审计 / 历史 | **存快照**（改名 / 删号后要能回溯"当时是谁"） | 附件上传人、操作日志、审批人 |
//! | 业务展示 | 实时 join 现值（换人后显示新人是对的） | 合同负责人、佣金业务员 |
//! | 主数据关联 | 实时 join 最新名（客户改名后单据显示新名） | 客户 / 物料 / 部门 / 凭证 |
//!
//! 附件上传人是第一类：用户 3 月改完名，6 月回看 1 月那份合同扫描件，
//! 界面必须显示「张三」而不是「张三丰」—— 否则审计链条上是谁传的就说不清了。
//!
//! # 三个取值口径（本模块的唯一真相）
//!
//! | 输入 | 输出 | 理由 |
//! |------|------|------|
//! | 账号在库、未删、`real_name` 非空 | `real_name` | 真实姓名优先（与全站 `COALESCE(NULLIF(real_name,''), username)` 同口径） |
//! | 账号在库、未删、`real_name` 为空 | `username` | 没填姓名的账号显示登录名，绝不显示 UUID |
//! | 账号已软删（`deleted_at` 非空） | [`DELETED_USER_LABEL`] | 与卡面「已被删 → 回填 `(已删除用户)`」逐字一致 |
//! | 账号行根本不存在 | [`DELETED_USER_LABEL`] | 与「已软删」同义：追溯不到这个人了，但**不能留空、不能留 UUID** |
//!
//! # 为什么不做批量版本（`snapshot_names_of`）
//!
//! 写入点是「一次上传一个人」，没有 N+1 的余地。批量取名的实现已经存在
//! （`erp/order_pdf_model.rs::user_names_of`，`HashMap<Uuid, String>`，PDF 导出用），
//! 再抄一份没有收益 —— 需要汇总展示时用那一个。
//!
//! **AUD-02 补充**：批量取名的唯一实现已经收口到本模块的 [`names_of`]，
//! `erp/order_pdf_model.rs::user_names_of` 等旧调用点按原样保留（口径一致，不动存量）；
//! 新代码（DTO 补 `*_name`）一律用 [`names_of`]。
//!
//! # 读路径怎么配合
//!
//! 本模块只管**写**。读的那一侧统一写成
//! `COALESCE(NULLIF(a.uploader_name, ''), NULLIF(u.real_name, ''), u.username, '')`：
//! 快照优先 → 历史行（尚未回填）回落实时 join → 兜底空串（前端显示 `-`）。
//! 这样「先上代码后跑回填」也不会在界面上闪出 UUID。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 追溯不到上传人时的占位串（已软删 / 行不存在）。
///
/// 与卡面（AUD-01 §③）「如果用户在 `sys_user` 里已被删，回填 `'(已删除用户)'`」逐字一致；
/// 一次性回填脚本 `scripts/datafix/aud01/backfill_attachment_uploader_name.sql`
/// 用的是同一个字面值（SQL 里写死，改这里要同步改脚本 —— 两侧各有一处断言）。
pub const DELETED_USER_LABEL: &str = "(已删除用户)";

/// 取用户姓名快照（**落库用**）。
///
/// # 参数
///
/// - `user_id`：上传人主键（`sys_user.id`）。
///
/// # 返回值
///
/// 可以原样写进 `uploader_name` 列的字符串。**永不返回空串**：
/// 追溯不到人时返回 [`DELETED_USER_LABEL`]，这样「空值」在库里只有一个含义 ——
/// 「这列是 V0352 之前的历史行，还没回填」。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（调用方按各模块口径 `?` 或 `utils::db_error`）。
///
/// # 为什么不过滤 `deleted_at`
///
/// 过滤器在 [`display_name`] 之后才生效：**先**把账号行读出来，**再**判断是否已删。
/// 若在 SQL 里加 `deleted_at IS NULL`，「已删」与「行不存在」两种完全不同的情况
/// 会合并成同一个空结果 —— 那样回填与写入就会各自编一个占位串（现在只有一个）。
// SCOPE-EXEMPT: 姓名快照助手 —— 按主键（或调用方已持有的 id 集合）取姓名/登录名，供「审计/历史类」
// 列落库（AGENTS.md 约定 13）。入参 id 已由调用方过完自己的数据权限，本模块不产出用户可见行集，
// 也不提供任何列表/分页入口。
pub async fn snapshot_name(user_id: Uuid) -> DbResult<String> {
    let rows = db()
        .find_with_params(
            "SELECT real_name, username, deleted_at FROM sys_user WHERE id = ?::uuid",
            vec![Value::from(&user_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        // 行不存在：账号被物理清理过，与「已软删」同义
        return Ok(DELETED_USER_LABEL.to_string());
    };
    let real_name: Option<String> = row.get_typed("real_name")?;
    let username: Option<String> = row.get_typed("username")?;
    let deleted: Option<chrono::DateTime<chrono::Utc>> = row.get_typed("deleted_at")?;
    Ok(display_name(
        real_name.as_deref(),
        username.as_deref(),
        deleted.is_some(),
    ))
}

/// 姓名口径的**纯函数**形态（不连库，单测直接打表）。
///
/// # 参数
///
/// - `real_name`：`sys_user.real_name`（可空）；
/// - `username`：`sys_user.username`（登录账号，非空列，但历史数据可能迁移成空串）；
/// - `deleted`：`sys_user.deleted_at IS NOT NULL`。
///
/// # 返回值
///
/// 见模块头「三个取值口径」表；**永不返回空串、永不返回 UUID**。
///
/// # 可见性
///
/// `pub(crate)`：`snapshot_name` 用它，`tests` 直接断言真值表（不连库）。
pub(crate) fn display_name(real_name: Option<&str>, username: Option<&str>, deleted: bool) -> String {
    if deleted {
        return DELETED_USER_LABEL.to_string();
    }
    if let Some(name) = real_name.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    if let Some(name) = username.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    // 账号在库但姓名 / 登录名都是空（历史脏数据）：仍然不能留空，
    // 否则库里「空值」的第二含义（未回填）就被污染了。
    DELETED_USER_LABEL.to_string()
}

// ==================== 实时展示口径（AUD-02）====================

/// 实时展示口径的姓名：`real_name` → `username` → [`DELETED_USER_LABEL`]。
///
/// 与 [`display_name`] 的**唯一区别**：不把「已软删」折成占位串。
///
/// 为什么：展示类字段（合同负责人 / 制单人 / 上传人…）问的是「**现在是谁**」。
/// 账号停用后姓名仍然是有意义的信息，显示「(已删除用户)」反而丢信息；
/// 审计类（写入时快照）继续走 [`display_name`] —— 那问的是「**当时是谁**」。
pub(crate) fn realtime_name(real_name: Option<&str>, username: Option<&str>) -> String {
    if let Some(name) = real_name.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    if let Some(name) = username.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    DELETED_USER_LABEL.to_string()
}

/// 批量取用户显示名（**实时**口径）：一次查询覆盖一整页 / 一整张详情。
///
/// # 参数
///
/// `ids`：任意多的人 id（**重复值自动去重**，空集合直接返回空表、不发 SQL）。
///
/// # 返回值
///
/// `Uuid → 显示名`。查不到的行**不出现在表里**（调用方按「缺失 = 人不存在」处理，
/// 别自己编占位串）。
///
/// # 为什么是批量（防 N+1）
///
/// 列表一页 20~100 行，逐行查一次就是 20~100 次往返。这里恒定 **1 条 SQL**
/// （`WHERE id = ANY(...)`），SQL 数量不随行数增长 —— AUD-02 验收 ③。
///
/// # 为什么不写 `deleted_at IS NULL`
///
/// 与 [`snapshot_name`] 同理：软删的账号仍要显示姓名（展示口径），
/// 过滤掉会让「负责人」列突然变空。真正要区分「人还在不在」的是审计口径。
///
/// # 一般不用直接调
///
/// 列表 / 详情请用 [`fill_person_names`]（它负责收集 id、查一次、回填），
/// 直接调本函数就得自己在 handler 里写三遍循环 —— 那就是 bug 的温床。
pub async fn names_of<I>(ids: I) -> DbResult<std::collections::HashMap<Uuid, String>>
where
    I: IntoIterator<Item = Uuid>,
{
    let mut unique: Vec<Uuid> = ids.into_iter().collect();
    unique.sort_unstable();
    unique.dedup();
    if unique.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let joined = unique.iter().map(Uuid::to_string).collect::<Vec<_>>().join(",");
    #[cfg(test)]
    stats::bump();
    let rows = db()
        .find_with_params(
            "SELECT id, real_name, username FROM sys_user WHERE id = ANY(?::text::uuid[])",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut out = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let real_name: Option<String> = row.get_typed("real_name")?;
        let username: Option<String> = row.get_typed("username")?;
        out.insert(id, realtime_name(real_name.as_deref(), username.as_deref()));
    }
    Ok(out)
}

/// 一行响应里「人员 id → 展示名」的映射能力（列表 / 详情补名字的统一入口）。
///
/// # 为什么用 trait 而不是每模块写一遍循环
///
/// 各模块的 `*Res` 字段名不同（`create_by` / `owner_id` / `uploaded_by`…），
/// 但「收集 id → 查一次 → 回填」这套动作**完全一样**。抽成 trait 之后
/// 每张表只需要声明「我有哪些人字段」（[`PersonNameRow::person_ids`] /
/// [`PersonNameRow::apply_names`] 各 5 行），批量语义（1 条 SQL）由
/// [`fill_person_names`] 统一保证 —— 谁也不会不小心在循环里查库。
///
/// # 实现约定
///
/// - `person_ids` **只收集、不去重**（去重由 [`names_of`] 做）；
/// - `apply_names` 查不到的人**必须保持 `None` / 空串**，绝不回填 id 文本；
/// - 实现放在 DTO 文件里（Model → Res 转换层），与 `contract_dto.rs` 同口径。
pub trait PersonNameRow {
    /// 把本行引用的所有人 id 追加进 `out`（可含重复；解析失败的 raw 值跳过）。
    fn person_ids(&self, out: &mut Vec<Uuid>);

    /// 用查回的 `id → 姓名` 表回填本行的 `*_name` 字段。
    fn apply_names(&mut self, names: &std::collections::HashMap<Uuid, String>);
}

/// 批量回填人员姓名：整页 / 整张详情**恒定 1 条 SQL**（无 N+1）。
///
/// # 参数
///
/// `rows`：任意多行；全部没有人员字段时**不发 SQL**（直接返回 `Ok(())`）。
///
/// # 返回值
///
/// 查询失败返回 [`DbError`] —— 调用方决定是「详情 500」还是「展示字段降级为空」。
/// 推荐后者（见各 handler 的 `unwrap_or_default()`）：姓名查不出来不该把业务页打死。
pub async fn fill_person_names<T: PersonNameRow>(rows: &mut [T]) -> DbResult<()> {
    let mut ids: Vec<Uuid> = Vec::new();
    for row in rows.iter() {
        row.person_ids(&mut ids);
    }
    if ids.is_empty() {
        return Ok(());
    }
    let names = names_of(ids).await?;
    for row in rows.iter_mut() {
        row.apply_names(&names);
    }
    Ok(())
}

/// 测试期统计：`names_of` 发了几条 SQL（**AUD-02 验收 ③** 的唯一证据）。
///
/// 只在 `cfg(test)` 编译，生产二进制里没有这个计数器。
///
/// # 为什么用 thread-local 而不是全局 Atomic
///
/// 同一个测试二进制里其它模块的真库用例会并行跑，也会调用 [`names_of`]；
/// 全局计数器会被它们污染，`assert_eq!(count, 1)` 就成了随机红。
/// 每个测试在自己的线程上跑（`#[tokio::test]` 默认 `current_thread`），
/// thread-local 天然按用例隔离。
#[cfg(test)]
pub(crate) mod stats {
    use std::cell::Cell;

    thread_local! {
        static NAME_QUERIES: Cell<usize> = const { Cell::new(0) };
    }

    /// 记一条 SQL。
    pub(crate) fn bump() {
        NAME_QUERIES.with(|count| count.set(count.get() + 1));
    }

    /// 归零（每条断言前调用）。
    pub(crate) fn reset() {
        NAME_QUERIES.with(|count| count.set(0));
    }

    /// 当前计数。
    pub(crate) fn count() -> usize {
        NAME_QUERIES.with(Cell::get)
    }
}

#[cfg(test)]
mod tests {
    use super::{DELETED_USER_LABEL, display_name, realtime_name};
    /// 卡面「三个取值口径」的真值表。
    #[test]
    fn display_name_truth_table() {
        // 真实姓名优先
        assert_eq!(display_name(Some("张三"), Some("zs"), false), "张三");
        // real_name 空 / 只有空白 → 回落登录名
        assert_eq!(display_name(None, Some("zs"), false), "zs");
        assert_eq!(display_name(Some("   "), Some("zs"), false), "zs");
        // 已软删：不管有没有姓名，一律占位串（改名 / 删号后要能看出"人已不在"）
        assert_eq!(display_name(Some("张三"), Some("zs"), true), DELETED_USER_LABEL);
        // 两个都空（历史脏数据）：不留空串
        assert_eq!(display_name(None, None, false), DELETED_USER_LABEL);
        assert_eq!(display_name(Some(""), Some(""), false), DELETED_USER_LABEL);
    }

    /// 展示口径（AUD-02 `realtime_name`）：与快照口径的唯一差别是软删仍取姓名。
    #[test]
    fn realtime_name_truth_table() {
        assert_eq!(realtime_name(Some("张三"), Some("zs")), "张三");
        assert_eq!(realtime_name(None, Some("zs")), "zs");
        assert_eq!(realtime_name(Some("   "), Some("zs")), "zs");
        // 账号在库但两列都空：仍然不留空串（界面显示占位，不是 UUID）
        assert_eq!(realtime_name(None, None), DELETED_USER_LABEL);
        // 与快照口径的差异点：软删不在纯函数入参里 → 展示口径天然取姓名
        assert_eq!(realtime_name(Some("张三"), None), "张三");
    }

    /// 占位串必须与回填脚本里的字面值一致（两侧各写死一处，漂了就是两套口径）。
    #[test]
    fn deleted_label_matches_backfill_script() {
        assert_eq!(DELETED_USER_LABEL, "(已删除用户)");
        let script = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scripts/datafix/aud01/backfill_attachment_uploader_name.sql"
        ));
        assert!(
            script.contains("'(已删除用户)'"),
            "回填脚本里的占位串必须与本常量逐字一致"
        );
    }
}
