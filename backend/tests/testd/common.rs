//! 测试D 夹具：其余业务模块（公告/通知/帮助/存储/项目/日志/互斥/导入导出）。
//!
//! # 为什么有这一层
//!
//! 这些模块的单测几乎全是纯逻辑（`checker` / `masker` / `sanitize` 之类），而本卡要证的是
//! **真库上的行为**：外键、事务边界、软删、脱敏规则命中、跨模块联动。所以这里只做三件事：
//!
//! 1. **连库**：只连 `cloudpivot_test`（硬校验，指错直接 panic）；
//! 2. **造夹具**：每个用例一套自己的 UUID / 前缀（[`Ctx`]），建在一次清理之上；
//! 3. **自清理**：用例结束按前缀 + 精确 id 删净，不碰别人的数据。
//!
//! # 数据库
//!
//! | 环境变量 | 作用 |
//! |---|---|
//! | `TESTD_DB_URL` | 真库连接串（缺省 `postgres://alun:alun@127.0.0.1:5432/cloudpivot_test`） |
//!
//! 与 `tests/linkage/common.rs` 同口径：`db()` 是进程级全局池，而每个 `#[tokio::test]`
//! 都是独立 runtime，空闲连接绑在创建它的 runtime 上，故 `max_lifetime` / `idle_timeout`
//! 置零，逼池子每次借出都新建连接。
//!
//! # 并发
//!
//! 同一个测试二进制里的用例共享一个进程与全局 `db()`，因此本目标的用例**串行**执行
//! （`--test-threads=1`），夹具前缀 `TD-` 与其他卡（`LNK-` / `D07-` / `B7_`）互不干扰。

#![allow(dead_code)]

use futures_util::FutureExt;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 本目标所有夹具的统一前缀根。
pub const ROOT: &str = "TD";

/// 缺省连接串（本机开发库；可用 `TESTD_DB_URL` 覆盖）。
const DEFAULT_URL: &str = "postgres://alun:alun@127.0.0.1:5432/cloudpivot_test";

/// 连接串。
pub fn db_url() -> String {
    std::env::var("TESTD_DB_URL").unwrap_or_else(|_| DEFAULT_URL.to_string())
}

/// 初始化全局数据库（进程内只做一次）；返回 `false` 表示连不上（用例直接跳过）。
pub async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let url = db_url();
            // 红线：只碰 cloudpivot_test。指到别的库直接炸，绝不「尽力而为」。
            assert!(
                url.contains("/cloudpivot_test"),
                "TESTD_DB_URL 必须指向 cloudpivot_test 库（当前：{url}）—— 本套测试会造真实业务数据"
            );
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => {
                        println!("[testd] 已连接 cloudpivot_test");
                        true
                    }
                    Err(err) => panic!("set_db 失败：{err}"),
                },
                Err(err) => {
                    println!("SKIP: 连接 cloudpivot_test 失败：{err}");
                    false
                }
            }
        })
        .await
}

/// 用例前置：连不上库时返回 `None`（用例直接 `return`）。
pub async fn ready() -> Option<()> {
    if ensure_db().await {
        Some(())
    } else {
        None
    }
}

// ==================== 身份 ====================

/// 造一份 `AuthClaims`（HTTP 层同款入参）。
pub fn claims(user: Uuid, name: &str, perms: &[&str]) -> Extension<AuthClaims> {
    Extension(AuthClaims(token_claims(user, name, perms)))
}

/// [`claims`] 的裸值版本。
pub fn token_claims(user: Uuid, name: &str, perms: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(name.to_string()),
        roles: vec![format!("{ROOT}_FIXTURE")],
        permissions: perms.iter().map(|p| p.to_string()).collect(),
        token_type: Some(TokenType::Access),
        exp: 4_000_000_000,
        iat: 1_700_000_000,
        extra: std::collections::HashMap::new(),
    }
}

// ==================== SQL ====================

/// 执行一条语句（无参数）。
pub async fn exec(sql: &str) {
    if let Err(error) = db().execute_with_params(sql, Vec::new()).await {
        panic!("夹具 SQL 失败：{error}\nSQL: {sql}");
    }
}

/// 执行一条语句（带参数）。
pub async fn exec_with(sql: &str, params: Vec<Value>) {
    if let Err(error) = db().execute_with_params(sql, params).await {
        panic!("夹具 SQL 失败：{error}\nSQL: {sql}");
    }
}

/// 执行一条语句，返回受影响行数；失败返回错误文本（供「应当失败」的用例断言）。
pub async fn try_exec(sql: &str, params: Vec<Value>) -> std::result::Result<u64, String> {
    db().execute_with_params(sql, params)
        .await
        .map_err(|error| error.to_string())
}

/// `SELECT count(*) ...`。
pub async fn count(sql: &str, params: Vec<Value>) -> i64 {
    let rows = db()
        .query_sql(sql.to_string(), params)
        .await
        .unwrap_or_else(|error| panic!("count 查询失败：{error}\nSQL: {sql}"));
    rows.first()
        .and_then(|row| row.get_typed::<i64>("n").ok())
        .unwrap_or(0)
}

/// 取第一行第一列（文本）。
pub async fn opt_text(sql: &str, params: Vec<Value>) -> Option<String> {
    let rows = db()
        .query_sql(sql.to_string(), params)
        .await
        .unwrap_or_else(|error| panic!("文本查询失败：{error}\nSQL: {sql}"));
    rows.first()
        .and_then(|row| row.get_typed::<Option<String>>("v").ok().flatten())
}

/// 取第一行第一列（文本），查不到即 panic。
pub async fn text(sql: &str, params: Vec<Value>) -> String {
    opt_text(sql, params)
        .await
        .unwrap_or_else(|| panic!("期望有值但查不到：{sql}"))
}

/// 取第一行第一列（UUID）。
pub async fn opt_uuid(sql: &str, params: Vec<Value>) -> Option<Uuid> {
    let rows = db()
        .query_sql(sql.to_string(), params)
        .await
        .unwrap_or_else(|error| panic!("UUID 查询失败：{error}\nSQL: {sql}"));
    rows.first()
        .and_then(|row| row.get_typed::<Option<Uuid>>("v").ok().flatten())
}

// ==================== 夹具上下文 ====================

/// 一次用例的夹具上下文。
///
/// `prefix` 形如 `TD-M03-a1b2c3d4`，所有编码 / 用户名 / 标题都以它开头，因此：
/// 用例之间互不可见、失败残留能被下一次 [`Ctx::reset`] 清掉、并发跑别的卡也不受影响。
#[derive(Debug, Clone)]
pub struct Ctx {
    /// 用例短名（`I01` / `G05` …）。
    pub tag: String,
    /// 本用例专属前缀。
    pub prefix: String,
}

impl Ctx {
    /// 建上下文并做一次前置清理。
    ///
    /// 前置清理有两层，**都是按前缀精确过滤**，不依赖用例执行顺序：
    ///
    /// 1. `TD-<tag>%`：**同一条用例**上一轮崩溃/中断留下的残留（panic 时函数尾部的
    ///    `reset` 不会执行，见 [`case`]）；
    /// 2. `TD-<tag>-<suffix>%`：本次前缀（理论上刚清过，等于免费自检）。
    pub async fn new(tag: &str) -> Self {
        // 第一层：上一轮的「同名用例」残留。前缀只到 tag，仍然只有本用例会碰这些行。
        let stale = Self {
            tag: tag.to_string(),
            prefix: format!("{ROOT}-{tag}"),
        };
        let stale_left = stale.reset().await;
        if stale_left > 0 {
            println!("WARN[cleanup] 用例 {tag} 上一轮残留 {stale_left} 行，已在本轮开工前清掉");
        }

        let simple = Uuid::now_v7().simple().to_string();
        // 取**尾部** 8 位：UUIDv7 的高位是时间戳，同一批用例里前缀相同，会撞前缀。
        let suffix = simple[simple.len() - 8..].to_string();
        let ctx = Self {
            tag: tag.to_string(),
            prefix: format!("{ROOT}-{tag}-{suffix}"),
        };
        ctx.reset().await;
        ctx
    }

    /// 带名字的编码（`ctx.key("ROLE")` → `TD-I01-xxxx-ROLE`）。
    pub fn key(&self, name: &str) -> String {
        format!("{}-{name}", self.prefix)
    }

    /// 小写编码（帮助中心的 `code` / `slug` 有 `^[a-z]...` 校验）。
    pub fn lkey(&self, name: &str) -> String {
        format!("{}-{name}", self.prefix.to_lowercase())
    }

    /// 清掉本前缀的残留（按前缀精确过滤，不做全表删除）。
    ///
    /// 顺序 = 外键拓扑序（叶子 → 根），不是「想起来一张表就加一句」：
    ///
    /// ```text
    /// sys_change_log(审计) → 任务子表 → 任务 → 项目 → 公告/通知 → 导入导出
    ///   → 帮助 → 字典 → 角色关联/互斥/脱敏 → sys_file → 用户 → 角色
    /// ```
    ///
    /// 顺序错一步就留孤儿：`biz_task.project_id` 是 `NO ACTION`，先删 `biz_project` 必然
    /// 失败，于是 `biz_project.owner_id` 又卡住 `sys_user`，一行脏数据能把整轮清理拖成
    /// 「用户 58 / 角色 21 / 项目 15 全留下」。
    ///
    /// 失败不再静默：打印 `WARN[cleanup]` + 数据库原文 + SQL，最后返回本前缀的残留行数
    /// （0 = 干净）。用例结束调一次即可自证清理。
    pub async fn reset(&self) -> i64 {
        let p = format!("{}%", self.prefix);
        let lpct = format!("{}%", self.prefix.to_lowercase());
        let users = format!("(SELECT id FROM sys_user WHERE username LIKE '{p}')");
        let tasks = format!(
            "(SELECT id FROM biz_task WHERE title LIKE '{p}' OR owner_id IN {users} \
             OR creator_id IN {users} OR assigner_id IN {users})"
        );
        let projects = format!(
            "(SELECT id FROM biz_project WHERE project_no LIKE '{p}' OR project_name LIKE '{p}' \
             OR owner_id IN {users})"
        );
        let notices =
            format!("(SELECT id FROM sys_notice WHERE title LIKE '{p}' OR sender_id IN {users})");
        let anns = format!(
            "(SELECT id FROM sys_announcement WHERE title LIKE '{p}' OR publisher_id IN {users})"
        );
        let docs =
            format!("(SELECT id FROM sys_help_doc WHERE slug LIKE '{lpct}' OR slug LIKE '{p}')");
        let roles = format!("(SELECT id FROM sys_role WHERE role_code LIKE '{p}')");
        let dicts = format!("(SELECT id FROM sys_dict_type WHERE dict_code LIKE '{p}')");
        let imports = format!("(SELECT id FROM sys_import_task WHERE task_no LIKE '{p}')");
        let files = format!(
            "(SELECT id FROM sys_file WHERE file_name LIKE '{p}' OR uploaded_by IN {users})"
        );
        // 审计行要在对象还在的时候摘（对象一删就查不到 resource_id 了）
        let objs = format!(
            "(SELECT id::text FROM biz_task WHERE title LIKE '{p}' OR owner_id IN {users} OR creator_id IN {users} \
             UNION SELECT id::text FROM biz_project WHERE project_no LIKE '{p}' OR project_name LIKE '{p}' \
             UNION SELECT id::text FROM sys_announcement WHERE title LIKE '{p}' \
             UNION SELECT id::text FROM sys_notice WHERE title LIKE '{p}' \
             UNION SELECT id::text FROM sys_import_task WHERE task_no LIKE '{p}' \
             UNION SELECT id::text FROM sys_export_task WHERE task_no LIKE '{p}' OR export_name LIKE '{p}' \
             UNION SELECT id::text FROM sys_help_doc WHERE slug LIKE '{lpct}' \
             UNION SELECT id::text FROM sys_role WHERE role_code LIKE '{p}' \
             UNION SELECT id::text FROM sys_dict_type WHERE dict_code LIKE '{p}' \
             UNION SELECT id::text FROM sys_file WHERE file_name LIKE '{p}')"
        );

        let statements = [
            // ---- 审计（分区表，DELETE 直接走父表路由到月分区）----
            format!(
                "DELETE FROM sys_change_log WHERE resource_id IN {objs} OR operator_id IN {users}"
            ),
            // ---- 任务域：子表 → 任务（先断 parent_id 自引用）→ 模板 ----
            format!(
                "DELETE FROM biz_task_dependency WHERE task_id IN {tasks} OR depends_on_id IN {tasks}"
            ),
            format!("DELETE FROM biz_task_ai_token WHERE task_id IN {tasks}"),
            format!(
                "DELETE FROM biz_task_attachment WHERE task_id IN {tasks} OR file_id IN {files}"
            ),
            format!("DELETE FROM biz_task_comment WHERE task_id IN {tasks}"),
            format!("DELETE FROM biz_task_worklog WHERE task_id IN {tasks}"),
            format!("DELETE FROM biz_task_member WHERE task_id IN {tasks} OR user_id IN {users}"),
            format!(
                "DELETE FROM biz_task_template WHERE project_id IN {projects} OR owner_id IN {users} OR name LIKE '{p}'"
            ),
            format!("UPDATE biz_task SET parent_id = NULL WHERE parent_id IN {tasks}"),
            format!("DELETE FROM biz_task WHERE id IN {tasks} OR project_id IN {projects}"),
            // ---- 项目域（任务清干净之后再删项目，否则 biz_task_project_id_fkey）----
            format!(
                "DELETE FROM biz_project_member WHERE project_id IN {projects} OR user_id IN {users}"
            ),
            format!("DELETE FROM biz_project WHERE id IN {projects}"),
            // ---- 公告 / 通知（投递 → 收件 → 通知 → 公告）----
            format!(
                "DELETE FROM sys_notice_delivery WHERE notice_id IN {notices} \
                 OR receiver_id IN (SELECT id FROM sys_notice_receiver WHERE user_id IN {users})"
            ),
            format!(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN {notices} OR user_id IN {users}"
            ),
            format!("DELETE FROM sys_notice WHERE id IN {notices}"),
            format!("DELETE FROM sys_notice_template WHERE template_code LIKE '{ROOT}-%'"),
            format!("DELETE FROM sys_announcement_read WHERE announcement_id IN {anns}"),
            format!("DELETE FROM sys_announcement_scope WHERE announcement_id IN {anns}"),
            format!("DELETE FROM sys_announcement_attachment WHERE announcement_id IN {anns}"),
            format!("DELETE FROM sys_announcement WHERE id IN {anns}"),
            // ---- 导入导出 ----
            format!("DELETE FROM sys_import_task_error WHERE task_id IN {imports}"),
            format!("DELETE FROM sys_import_task WHERE id IN {imports}"),
            format!(
                "DELETE FROM sys_export_task WHERE task_no LIKE '{p}' OR export_name LIKE '{p}'"
            ),
            // ---- 帮助中心 ----
            format!("DELETE FROM sys_help_doc_version WHERE doc_id IN {docs}"),
            format!("DELETE FROM sys_help_doc WHERE id IN {docs}"),
            format!("DELETE FROM sys_help_category WHERE code LIKE '{lpct}' OR code LIKE '{p}'"),
            // ---- 字典 ----
            format!(
                "DELETE FROM sys_dict_item WHERE dict_type_id IN {dicts} OR item_label LIKE '{p}'"
            ),
            format!("DELETE FROM sys_dict_type WHERE id IN {dicts}"),
            // ---- 角色关联 / 互斥 / 脱敏 ----
            format!("DELETE FROM sys_role_menu WHERE role_id IN {roles}"),
            format!("DELETE FROM sys_role_dept WHERE role_id IN {roles}"),
            format!("DELETE FROM sys_field_mask WHERE role_id IN {roles} OR reason LIKE '{p}'"),
            format!(
                "DELETE FROM sys_role_mutex WHERE role_a_id IN {roles} OR role_b_id IN {roles} OR reason LIKE '{p}'"
            ),
            format!("DELETE FROM sys_user_role WHERE user_id IN {users} OR role_id IN {roles}"),
            format!("DELETE FROM sys_ai_agent WHERE user_id IN {users}"),
            format!("DELETE FROM sys_user_preference WHERE user_id IN {users}"),
            // ---- 审计：明文访问留痕（X05）按用户 id 摘；必须在删用户之前 ----
            format!("DELETE FROM audit_logs WHERE user_id IN {users}"),
            // ---- 文件本体（必须排在所有引用它的行之后）----
            format!("DELETE FROM sys_file WHERE id IN {files}"),
            // ---- 根：用户、角色 ----
            format!("DELETE FROM sys_user WHERE username LIKE '{p}'"),
            format!("DELETE FROM sys_role WHERE role_code LIKE '{p}'"),
        ];

        let mut failures = 0usize;
        for sql in &statements {
            if let Err(error) = db().execute_with_params(sql, Vec::new()).await {
                failures += 1;
                println!("WARN[cleanup] {error}\n  SQL: {sql}");
            }
        }
        let left = self.residue().await;
        if left > 0 {
            println!(
                "WARN[cleanup] 本前缀仍有 {left} 行残留（失败语句 {failures} 条），前缀 {}",
                self.prefix
            );
        }
        left
    }

    /// 本前缀在关键表里的残留行数（自检用；0 = 清干净）。
    ///
    /// 覆盖顺序与 [`Ctx::reset`] 的清理清单一一对应（含「关联行」：用户的角色绑定、
    /// 角色的互斥规则、通知的收件行与投递行、公告的已读回执/范围/附件）。
    /// 只查实体表会漏掉「实体删了、关联脏行还在」——那正是 BUG-D-04 的老形态。
    pub async fn residue(&self) -> i64 {
        let p = format!("{}%", self.prefix);
        let lpct = format!("{}%", self.prefix.to_lowercase());
        let users = format!("(SELECT id FROM sys_user WHERE username LIKE '{p}')");
        let roles = format!("(SELECT id FROM sys_role WHERE role_code LIKE '{p}')");
        let notices =
            format!("(SELECT id FROM sys_notice WHERE title LIKE '{p}' OR sender_id IN {users})");
        let anns = format!(
            "(SELECT id FROM sys_announcement WHERE title LIKE '{p}' OR publisher_id IN {users})"
        );
        let checks = [
            format!("SELECT count(*) AS n FROM sys_user WHERE username LIKE '{p}'"),
            format!("SELECT count(*) AS n FROM sys_role WHERE role_code LIKE '{p}'"),
            format!("SELECT count(*) AS n FROM sys_user_role WHERE user_id IN {users} OR role_id IN {roles}"),
            format!(
                "SELECT count(*) AS n FROM sys_role_mutex WHERE role_a_id IN {roles} OR role_b_id IN {roles} OR reason LIKE '{p}'"
            ),
            format!(
                "SELECT count(*) AS n FROM biz_project WHERE project_no LIKE '{p}' OR project_name LIKE '{p}'"
            ),
            format!("SELECT count(*) AS n FROM biz_task WHERE title LIKE '{p}'"),
            format!("SELECT count(*) AS n FROM sys_announcement WHERE title LIKE '{p}'"),
            format!(
                "SELECT count(*) AS n FROM sys_announcement_read WHERE announcement_id IN {anns}"
            ),
            format!(
                "SELECT count(*) AS n FROM sys_announcement_attachment WHERE announcement_id IN {anns}"
            ),
            format!("SELECT count(*) AS n FROM sys_notice WHERE title LIKE '{p}'"),
            format!("SELECT count(*) AS n FROM sys_notice_receiver WHERE notice_id IN {notices}"),
            format!("SELECT count(*) AS n FROM sys_import_task WHERE task_no LIKE '{p}'"),
            format!(
                "SELECT count(*) AS n FROM sys_export_task WHERE task_no LIKE '{p}' OR export_name LIKE '{p}'"
            ),
            format!(
                "SELECT count(*) AS n FROM sys_help_doc WHERE slug LIKE '{lpct}' OR slug LIKE '{p}'"
            ),
            format!("SELECT count(*) AS n FROM sys_dict_type WHERE dict_code LIKE '{p}'"),
            format!("SELECT count(*) AS n FROM sys_file WHERE file_name LIKE '{p}'"),
        ];
        let mut total = 0;
        for sql in &checks {
            total += count(sql, Vec::new()).await;
        }
        total
    }
}

/// 跑一条用例：**任何退出路径**（正常返回 / panic）都按前缀做一次确定性清理。
///
/// # 为什么清理不能写在用例尾部
///
/// `#[tokio::test]` 用例体 panic 时，函数末尾的 `ctx.reset().await` 根本不会执行 ——
/// 夹具就那么留在库里。2026-11-03 实测：G03 / K08 各崩 5 轮，`cloudpivot_test` 攒下
/// 20 个 `TD-` 用户 / 5 条公告 / 5 条通知 / 5 条互斥规则，把全量套件的收口用例
/// `storage::zz_no_fixture_residue_after_full_suite` 打成红 —— 这正是「守卫型用例过期」
/// 连带出的假缺陷。所以清理挂在外壳上：用例体包进 `catch_unwind`，unwind 之后先清库、
/// 再把 panic **原样抛回去**（用例仍然是红的，绝不吞失败）。
///
/// # 确定性（不靠「按名字排序最后兜底」）
///
/// - 每个用例只清**自己的前缀**（`TD-<tag>-<suffix>`），不扫全表、不看别的用例脸色；
/// - [`Ctx::new`] 开工前先扫掉**同 tag** 的历史残留（上一轮崩溃留下的），所以「崩了 →
///   库里脏了 → 下一轮开工自愈」，不需要人工介入；
/// - 正常返回时**断言本前缀残留为 0**：`residue()` 是外部事实（重新查库），不是
///   「我调过清理了」的自我报告。
///
/// 收口用例 `zz_no_fixture_residue_after_full_suite` 保留，但降级为**外部交叉验证**：
/// 它再红就意味着有夹具逃过了「每例自清 + 同 tag 自愈」两层，是真问题。
///
/// # 用法
///
/// ```ignore
/// #[tokio::test]
/// async fn g01_xxx() {
///     let Some(()) = c::ready().await else { return };
///     c::case("G01", |ctx| async move {
///         // 用例体（原来怎么写还怎么写）
///     })
///     .await;
/// }
/// ```
pub async fn case<F, Fut>(tag: &str, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let ctx = Ctx::new(tag).await;
    // AssertUnwindSafe：夹具（`Ctx`）跨 panic 边界后用不着，用例体里被改坏的状态也不会
    // 影响清理（清理只认前缀 + 全局连接池）。这是 `catch_unwind` 唯一的实际顾虑。
    let outcome = std::panic::AssertUnwindSafe(body(ctx.clone()))
        .catch_unwind()
        .await;
    let left = ctx.reset().await;
    match outcome {
        Ok(()) => assert_eq!(
            left, 0,
            "用例 {tag} 结束后本前缀仍有 {left} 行残留（{}）—— 夹具清理语句没跟上用例",
            ctx.prefix
        ),
        Err(panic) => {
            if left > 0 {
                println!(
                    "WARN[cleanup] 用例 {tag} panic 后仍残留 {left} 行（{}），已尽量清理；\
                     下一轮开工前会再扫一次同 tag 残留",
                    ctx.prefix
                );
            }
            // 真红照旧抛回去：外壳只负责清库，不负责把失败变成绿。
            std::panic::resume_unwind(panic);
        }
    }
}

/// `ROOT` 前缀的全局残留（跨用例/跨轮次自检用；0 = 整库没有本卡的夹具）。
pub async fn root_residue() -> Vec<(&'static str, i64)> {
    let p = format!("{ROOT}%");
    let lpct = format!("{}%", ROOT.to_lowercase());
    let tables: [(&'static str, String); 16] = [
        (
            "sys_user",
            format!("SELECT count(*) AS n FROM sys_user WHERE username LIKE '{p}'"),
        ),
        (
            "sys_role",
            format!("SELECT count(*) AS n FROM sys_role WHERE role_code LIKE '{p}'"),
        ),
        (
            "sys_user_role",
            format!(
                "SELECT count(*) AS n FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE '{p}')"
            ),
        ),
        (
            "biz_project",
            format!(
                "SELECT count(*) AS n FROM biz_project WHERE project_no LIKE '{p}' OR project_name LIKE '{p}'"
            ),
        ),
        (
            "biz_task",
            format!("SELECT count(*) AS n FROM biz_task WHERE title LIKE '{p}'"),
        ),
        (
            "biz_task_attachment",
            format!(
                "SELECT count(*) AS n FROM biz_task_attachment WHERE task_id IN (SELECT id FROM biz_task WHERE title LIKE '{p}')"
            ),
        ),
        (
            "sys_announcement",
            format!("SELECT count(*) AS n FROM sys_announcement WHERE title LIKE '{p}'"),
        ),
        (
            "sys_notice",
            format!("SELECT count(*) AS n FROM sys_notice WHERE title LIKE '{p}'"),
        ),
        (
            "sys_import_task",
            format!("SELECT count(*) AS n FROM sys_import_task WHERE task_no LIKE '{p}'"),
        ),
        (
            "sys_export_task",
            format!(
                "SELECT count(*) AS n FROM sys_export_task WHERE task_no LIKE '{p}' OR export_name LIKE '{p}'"
            ),
        ),
        (
            "sys_help_doc",
            format!(
                "SELECT count(*) AS n FROM sys_help_doc WHERE slug LIKE '{lpct}' OR slug LIKE '{p}'"
            ),
        ),
        (
            "sys_help_category",
            format!("SELECT count(*) AS n FROM sys_help_category WHERE code LIKE '{lpct}'"),
        ),
        (
            "sys_dict_type",
            format!("SELECT count(*) AS n FROM sys_dict_type WHERE dict_code LIKE '{p}'"),
        ),
        (
            "sys_dict_item",
            format!("SELECT count(*) AS n FROM sys_dict_item WHERE item_label LIKE '{p}'"),
        ),
        (
            "sys_role_mutex",
            format!("SELECT count(*) AS n FROM sys_role_mutex WHERE reason LIKE '{p}'"),
        ),
        (
            "sys_file",
            format!("SELECT count(*) AS n FROM sys_file WHERE file_name LIKE '{p}'"),
        ),
    ];
    let mut out = Vec::new();
    for (name, sql) in tables {
        out.push((name, count(&sql, Vec::new()).await));
    }
    out
}

// ==================== 造数 ====================

/// 建一个用户（`user_type` 默认 `HUMAN`）。返回 id。
pub async fn make_user(ctx: &Ctx, name: &str, user_type: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec_with(
        "INSERT INTO sys_user (id, username, password, real_name, status, user_type) \
         VALUES (?::uuid, ?, 'fixture-not-a-real-hash', ?, 1, ?)",
        vec![
            Value::from(&id),
            Value::Text(ctx.key(name)),
            Value::Text(format!("{} 用户", name)),
            Value::Text(user_type.to_string()),
        ],
    )
    .await;
    id
}

/// 建一个角色。返回 id。
pub async fn make_role(ctx: &Ctx, name: &str, data_scope: i16) -> Uuid {
    let id = Uuid::now_v7();
    exec_with(
        "INSERT INTO sys_role (id, role_code, role_name, data_scope, status) \
         VALUES (?::uuid, ?, ?, ?, 1)",
        vec![
            Value::from(&id),
            Value::Text(ctx.key(name)),
            Value::Text(format!("{} 角色", name)),
            Value::I16(data_scope),
        ],
    )
    .await;
    id
}

/// 把角色授予用户。
pub async fn bind_role(user: Uuid, role: Uuid) {
    exec_with(
        "INSERT INTO sys_user_role (id, user_id, role_id) VALUES (gen_random_uuid(), ?::uuid, ?::uuid)",
        vec![Value::from(&user), Value::from(&role)],
    )
    .await;
}

/// 建字典类型（导入导出的外键目标）。返回 id。
pub async fn make_dict_type(ctx: &Ctx, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec_with(
        "INSERT INTO sys_dict_type (id, dict_code, dict_name, status) VALUES (?::uuid, ?, ?, 1)",
        vec![
            Value::from(&id),
            Value::Text(ctx.key(name)),
            Value::Text(format!("{} 字典", name)),
        ],
    )
    .await;
    id
}

// ==================== 时间 ====================

/// 当前 UTC 时刻。
pub fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
