//! 应用启动引导。
//!
//! [`bootstrap`] 由 `App::on_startup()` 调用，执行时机固定为
//! 「全局资源（配置 / 数据库 / 缓存 / 上传下载目录）已初始化完成、插件尚未启动」，
//! 因此 `cfg()` / `db()` / `try_cache()` 等全局访问器在此均可安全使用。
//!
//! 本模块承载**业务侧**的初始化：
//!
//! 1. 输出启动摘要；
//! 2. 写入幂等种子数据：管理员账号 `admin` → 超级管理员角色绑定（见 [`seed_super_admin`]）；
//! 3. 装载定时任务（系统内置任务 + `sys_job` 中启用的任务，见 [`load_scheduled_jobs`]）；
//! 4. 预热内置字典缓存（见 [`warm_dict_cache`]）；
//! 5. 装载运行时参数快照（见 [`load_runtime_config`]），供订阅了参数的插件直接取用。
//!
//! 内置超级管理员角色、菜单与按钮权限点等**无需运行期计算的种子数据**已全部下沉到
//! `migrations/seed/S__*.sql` 种子迁移；后续新增功能只需追加迁移，无需改动本文件。
//!
//! 基础设施的初始化由框架在 `App::start()` 内完成，不要在此重复做。

use nik::prelude::*;

use crate::shared::constants;
use crate::system::role::model::SysRoleModel;
use crate::system::user::model::{SysUserModel, SysUserRoleModel};

/// 启动钩子：完成业务侧初始化并输出启动摘要。
///
/// # 异常情况
///
/// 钩子签名没有错误通道（`FnOnce() -> Future<Output = ()>`），因此这里不做可能失败的
/// 操作；需要失败即中止启动的校验必须放在 `App::start()` 之前的代码路径中。
/// 种子数据写入失败只记录 `tracing::error!` 并继续，不阻断服务启动。
pub async fn bootstrap() {
    let app_name = cfg().app_name.clone();
    let profile = cfg().profile.clone();

    tracing::info!(
        app = %app_name,
        %profile,
        db_driver = %cfg().database.r#type,
        "启动引导完成"
    );

    // 五步互不依赖，并发执行：原先串行会把各步等待累加进启动耗时；
    // 每步自身的失败处理（只记日志、不阻断启动）保持不变，
    // 仅启动日志的先后顺序不再固定。
    //
    // 第 5 步 `preflight_permission` 是批次 B 的「闸 3」（架构 §2.4）：查
    // `information_schema` 确认登记表的部门列 / 归属列真实存在，并确认
    // `SUPER_ADMIN` 角色存在。它是**唯一**在「资源就绪、插件未启动」时执行的入口，
    // 因此把「部署即失败」的校验放在这里（与 `seed_super_admin` 的 prod `exit(1)` 先例一致）。
    tokio::join!(
        seed_super_admin(),
        load_scheduled_jobs(),
        crate::support::dict::model::warm_dict_cache(),
        load_runtime_config(),
        preflight_permission_guard(),
        ai_agent_invariant_guard(),
        orphan_dependency_guard(),
        test_residue_guard(),
    );
}

/// 启动自检：库里有没有 **e2e 测试残留**（`t_61dca492`）。
///
/// # 为什么只 WARN、不拒启动
///
/// 残留（`biz_type` = `E2E_*` / `D2_SMOKE` 的流程定义 / 实例）不影响任何业务读写，
/// 它污染的是「词表巡检的返回码」和「管理员在审批中心看到的东西」。
/// 为它拒绝上线，代价远大于收益（而且清残留本身有专门的 datafix 脚本 + 回滚）。
/// 与 [`ai_agent_invariant_guard`] 刻意不同：那一项错的是权限与审计口径，
/// 带病上线比不启动更危险。
///
/// 详情同时从 `GET /business/erp/approval-flow/health` 的 `test_residue` 字段可取
/// （给运维一个「不用翻日志」的入口）。
///
/// # 异常情况
///
/// 查库失败（连不上 / 权限不足）只记 `warn` 并继续启动。
async fn test_residue_guard() {
    match crate::shared::startup_guard::count_test_residue().await {
        Ok(residue) => match crate::shared::startup_guard::test_residue_warning(residue) {
            Some(message) => tracing::warn!("{message}"),
            None => tracing::info!("测试残留自检通过：biz_flow_def / biz_flow_instance 均 0 行 E2E_*, D2_SMOKE"),
        },
        Err(error) => tracing::warn!("测试残留自检无法完成（继续启动）：{error}"),
    }
}

/// 启动自检：孤儿任务依赖（E7 兜底告警）。
///
/// # 为什么只 WARN、任何 profile 都不拒启动
///
/// 孤儿依赖（后继或前置已软删的 `biz_task_dependency` 行）不影响任何读路径 ——
/// 列表 / 详情 / 甘特全部带 `JOIN biz_task … AND deleted_at IS NULL`，看不见它。
/// 它的危害是「脏数据永久累积」，所以**点名告警**给运维，但不值得为它拒绝上线
/// （与 [`ai_agent_invariant_guard`] 的 prod `exit(1)` 刻意不同：那一项错的是权限与
/// 审计口径，带病上线比不启动更危险）。
///
/// # 异常情况
///
/// 查库失败（连不上 / 权限不足）只记 `warn` 并继续启动：这一项不该成为启动的单点故障。
async fn orphan_dependency_guard() {
    match crate::shared::startup_guard::count_orphan_task_dependencies().await {
        Ok(count) => match crate::shared::startup_guard::orphan_dependency_warning(count) {
            Some(message) => tracing::warn!("{message}"),
            None => tracing::info!("孤儿任务依赖自检通过：0 条"),
        },
        Err(error) => tracing::warn!("孤儿任务依赖自检无法完成（继续启动）：{error}"),
    }
}

/// AI 用户不变式自检（L1 · laoliu 2026-10-29 追加硬要求）。
///
/// 不变式：`sys_user.user_type = 'ROBOT'` ⇔ `sys_ai_agent` 有该 `user_id` 的记录。
///
/// # 为什么要查
///
/// 破坏形态都是**静默的**：
/// - 手工删掉一条 `sys_ai_agent` → 那个机器人的 Hermes profile 再也没人能编辑 / 清理；
/// - 把账号 `user_type` 改成 `HUMAN` 而没清配置行 → 审计与数据权限把它当普通账号。
///
/// 两种都不会让任何接口报错，只会在「该有的机器人不见了 / 该清理的磁盘没清理」时
/// 才被发现。所以启动即自检。
///
/// # 处置
///
/// - 恒打 `ERROR` 日志（带 id 清单，最多各 50 条）；
/// - **prod 拒启动**（`exit(1)`，与 `seed_super_admin` / 数据权限自检同一先例）：
///   带病上线比不启动更危险；
/// - 其余 profile 只告警，便于开发/测试环境先修数据。
async fn ai_agent_invariant_guard() {
    match crate::shared::startup_guard::check_ai_agent_invariant().await {
        Ok(()) => {}
        Err(error) => {
            let message = format!("AI 用户不变式自检不通过：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            tracing::warn!("AI 用户不变式自检不通过（非 prod，继续启动，请尽快修复数据）");
        }
    }
}

/// 启动自检：数据权限登记表 / 超管角色（批次 B 的「闸 3」，架构 §2.4、§5.3-#9）。
///
/// # 为什么必须在启动期查
///
/// 表被改名 / 删列后，注入点会在**运行期**抛 `column does not exist`——那时是 500
/// 与用户报障。启动期查 = 部署即失败。超管角色同理：`data_scope` 的短路逻辑依赖
/// `role_code = 'SUPER_ADMIN'`，角色缺失等于超管被自己锁在数据权限外面。
///
/// # 失败处理
///
/// - **prod**：`eprintln!` + `exit(1)`（宁可不启动也不带病上线，与
///   [`ensure_admin_user`] 缺初始口令的处理同口径）；
/// - 其它 profile：只记 `error` 日志并继续（开发 / 测试环境需要能带着不完整的
///   登记表启动来调试；且 `cargo test` 的
///   [`crate::business::shared::guard`] 单测不依赖真库）。
///
/// # 异常情况
///
/// 查库失败（连不上 / 权限不足）与「自检不健康」同样处理：prod 拒启动，其余告警。
async fn preflight_permission_guard() {
    let report = match crate::business::shared::guard::preflight_permission().await {
        Ok(report) => report,
        Err(e) => {
            let message = format!("数据权限自检无法完成（查库失败）：{e}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            return;
        }
    };
    if report.is_ok() {
        return;
    }
    // `preflight_permission` 内部已按 error 级打过摘要，这里只决定要不要中止启动
    if is_prod_profile() {
        let message = format!("数据权限自检不通过，拒绝启动：{}", report.summary());
        tracing::error!("{message}");
        eprintln!("FATAL: {message}");
        std::process::exit(1);
    }
    tracing::warn!(
        "数据权限自检不通过（非 prod，继续启动）：{}",
        report.summary()
    );
}

/// 装载运行时参数快照（`sys_config` → 进程内 `RwLock<HashMap>`）。
///
/// 必须在插件启动**之前**执行：插件（如 [`crate::support::log::LogPlugin`]）通过
/// [`subscribe_runtime`] 订阅参数，注册时会立刻以当前快照回调一次；快照未装载则
/// 回调被跳过，只能等运行期首次写操作或 TTL 过期回源才补上，导致启动瞬间读到的是
/// 回退默认值而非库中配置。
///
/// # 异常情况
///
/// 装载失败只记录错误日志并继续（启动钩子没有错误通道）：读路径会回源数据库，
/// 快照缺失只损失性能，不影响正确性。
async fn load_runtime_config() {
    match crate::support::config::model::reload_runtime_snapshot().await {
        Ok(count) => tracing::info!(params = count, "运行时参数快照已装载"),
        Err(e) => tracing::error!(error = %e, "运行时参数快照装载失败，读路径将回源数据库"),
    }
    // 启动跨实例同步：其它实例改参数后本实例在轮询间隔内自动跟随，无需重启或手动
    // 调用重载接口（缓存未启用时内部自行跳过）
    crate::support::config::model::spawn_config_version_watcher();
}

/// 装载定时任务：注册系统内置任务，并从 `sys_job` 读取启用任务一并注册。
///
/// 调用 [`crate::task::scheduler`] 的 `reload()`：内部先停止调度器，再按
/// 「系统内置任务 + 库中启用任务」重建注册表并重新启动调度。
///
/// # 异常情况
///
/// 装载失败只记录错误日志并继续（启动钩子没有错误通道）：此时调度器仍会保留
/// 已经注册成功的任务，不影响服务对外提供接口。
async fn load_scheduled_jobs() {
    if let Err(e) = crate::task::scheduler().reload().await {
        tracing::error!("定时任务装载失败: {e:?}");
    }
}

/// 写入内置种子数据：管理员账号 `admin`，以及它与超级管理员角色的绑定关系。
///
/// # 与迁移的分工
///
/// 超级管理员**角色**由种子迁移 `migrations/seed/S__role_data.sql` 创建，此处不再
/// 写角色，只在角色就绪后查询其主键，完成「账号 → 角色」绑定。角色缺失属异常
/// （迁移未执行），此时只记错误日志并跳过绑定。
///
/// # 幂等性
///
/// 账号按 `username = admin` 判定、绑定按 `(user_id, role_id)` 计数判定，存在即
/// 跳过，因此重启不会产生重复数据。
///
/// # 初始口令
///
/// 管理员密码**只**取自环境变量 [`constants::ADMIN_PASSWORD_ENV`]：
///
/// - 生产环境（`profile = prod`）缺失时直接 `FATAL` + `exit(1)`，绝不回退任何
///   内置缺省口令（仓库里已不存在该常量，安全审计 CODE-06）；
/// - 开发 / 测试环境缺失时生成一次性随机口令，**仅在启动日志中出现一次**，
///   并把账号标记为 `must_change_password = true`（除改密 / 登出外全部接口 403）。
///
/// 账号已存在时不会触碰其口令，因此运行中的实例不会因为「运维没配 env」而被锁死。
///
/// # 异常情况
///
/// 任一步骤失败只记录错误日志并继续（启动钩子无错误通道）：账号写入失败、角色查询
/// 失败或角色缺失时均跳过绑定；绑定失败仅记录错误。数据库未初始化时直接返回。
async fn seed_super_admin() {
    let db = match try_db() {
        Some(db) => db,
        None => {
            tracing::error!("数据库未初始化，跳过种子数据写入");
            return;
        }
    };

    let user = match ensure_admin_user(db).await {
        Ok(user) => user,
        Err(e) => {
            tracing::error!(error = %e, "写入管理员账号失败，跳过角色绑定");
            return;
        }
    };

    // 内置超级管理员角色由种子迁移创建，此处只查询其主键用于绑定。
    let role = match SysRoleModel::find_first_by(
        db,
        "role_code",
        (constants::SUPER_ADMIN_ROLE_CODE,),
    )
    .await
    {
        Ok(Some(role)) => role,
        Ok(None) => {
            tracing::error!(
                role_code = constants::SUPER_ADMIN_ROLE_CODE,
                "内置超级管理员角色缺失（应由 migrations/seed/S__role_data.sql 创建），跳过管理员角色绑定"
            );
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "查询超级管理员角色失败，跳过角色绑定");
            return;
        }
    };

    if let Err(e) = ensure_admin_role_binding(db, &user.id, &role.id).await {
        tracing::error!(error = %e, "写入管理员角色绑定失败");
    }
}

/// 确保内置管理员账号存在，返回该账号（已存在则直接返回库中记录）。
///
/// # 参数
///
/// - `db`：数据库执行器。
///
/// # 返回值
///
/// 库中 `username = admin` 的用户模型；不存在时以初始口令创建后返回。
///
/// # 异常情况
///
/// 查询失败、密码哈希失败或插入失败时返回 [`DbError`]。
async fn ensure_admin_user(db: &Db) -> std::result::Result<SysUserModel, DbError> {
    if let Some(user) =
        SysUserModel::find_first_by(db, "username", (constants::ADMIN_USERNAME,)).await?
    {
        tracing::info!(
            username = constants::ADMIN_USERNAME,
            "管理员账号已存在，跳过创建"
        );
        return Ok(user);
    }

    // 初始口令：**只**认环境变量。缺失时不再回退任何内置缺省口令（仓库里已删除
    // 该常量）：生产环境直接拒绝启动，开发 / 测试环境生成一次性随机口令并强制改密。
    let password_plain = match std::env::var(constants::ADMIN_PASSWORD_ENV) {
        Ok(custom) if !custom.trim().is_empty() => custom,
        _ => {
            if is_prod_profile() {
                let message = format!(
                    "生产环境未注入管理员初始口令（{}），拒绝启动：仓库内不保留任何缺省口令",
                    constants::ADMIN_PASSWORD_ENV
                );
                tracing::error!("{message}");
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            let generated = generate_password(constants::GENERATED_ADMIN_PASSWORD_LEN);
            // 只在这一行打印一次：管理员据此首次登录，随后被 must_change_password 强制改密
            tracing::warn!(
                env = constants::ADMIN_PASSWORD_ENV,
                username = constants::ADMIN_USERNAME,
                password = %generated,
                "未注入管理员初始口令，已生成一次性随机口令（仅本行打印一次），首次登录必须修改"
            );
            generated
        }
    };

    // 口令哈希：Argon2 为 CPU 密集慢哈希，放入阻塞线程池执行，
    // 避免占据异步执行器线程。
    let password = tokio::task::spawn_blocking(move || {
        Crypto::hash_password(&password_plain).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| DbError::InvalidValue(format!("管理员初始密码哈希任务失败: {e}")))?
    .map_err(|e| DbError::InvalidValue(format!("管理员初始密码哈希失败: {e}")))?;

    let now = chrono::Utc::now();
    let mut user = SysUserModel {
        id: uuid::Uuid::now_v7(),
        username: constants::ADMIN_USERNAME.to_string(),
        password,
        real_name: Some(constants::SUPER_ADMIN_ROLE_NAME.to_string()),
        email: None,
        mobile: None,
        dept_id: None,
        status: constants::EnableStatus::enabled,
        login_fail_count: 0,
        is_locked: false,
        lock_time: None,
        last_login_at: None,
        remark: None,
        // 种子账号一律要求首登改密：初始口令可能来自 env 注入或本进程随机生成，
        // 无论哪种都不应长期有效（安全审计 CODE-06）
        must_change_password: true,
        token_version: 0,
        created_at: now,
        updated_at: now,
        create_by: None,
        update_by: None,
        deleted_at: None,
    };
    user.insert(db).await?;

    tracing::info!(
        username = constants::ADMIN_USERNAME,
        "已写入管理员账号种子数据"
    );
    Ok(user)
}

/// 确保管理员账号与超级管理员角色的绑定存在。
///
/// # 参数
///
/// - `db`：数据库执行器。
/// - `user_id`：管理员账号主键。
/// - `role_id`：超级管理员角色主键。
///
/// # 返回值
///
/// 绑定已存在或写入成功时返回 `Ok(())`。
///
/// # 异常情况
///
/// 计数查询或插入失败时返回 [`DbError`]。
async fn ensure_admin_role_binding(
    db: &Db,
    user_id: &uuid::Uuid,
    role_id: &uuid::Uuid,
) -> std::result::Result<(), DbError> {
    let existing =
        SysUserRoleModel::count_by(db, "user_id = ? AND role_id = ?", (user_id, role_id)).await?;
    if existing > 0 {
        tracing::info!(
            username = constants::ADMIN_USERNAME,
            "管理员角色绑定已存在，跳过创建"
        );
        return Ok(());
    }

    // 关联表关闭了时间戳 Hook，created_at 需显式赋值。
    let mut binding = SysUserRoleModel {
        id: uuid::Uuid::now_v7(),
        user_id: *user_id,
        role_id: *role_id,
        expire_time: None,
        created_at: chrono::Utc::now(),
    };
    binding.insert(db).await?;

    tracing::info!(
        username = constants::ADMIN_USERNAME,
        role_code = constants::SUPER_ADMIN_ROLE_CODE,
        "已写入管理员角色绑定种子数据"
    );
    Ok(())
}

// ==================== 公共辅助 ====================

/// 当前 profile 是否为生产环境。
///
/// # 返回值
///
/// 配置中的 `profile` 等于 `prod` 时返回 `true`。
fn is_prod_profile() -> bool {
    cfg().profile.eq_ignore_ascii_case("prod")
}

/// 生成一次性随机口令（十六进制字符）。
///
/// # 参数
///
/// - `len`：口令字符数。
///
/// # 返回值
///
/// 长度为 `len` 的随机字符串。
///
/// # 随机源
///
/// 取自 `uuid::Uuid::new_v4()`（底层为操作系统 CSPRNG），与项目其它随机标识同源，
/// 不额外引入 `rand` 依赖。十六进制字符集无取模偏差，24 位即 96 bit 熵；
/// 配合 Argon2id 存储，离线爆破成本可忽略。
fn generate_password(len: usize) -> String {
    let mut raw = String::with_capacity(len + 32);
    while raw.len() < len {
        raw.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    raw.truncate(len);
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_password_has_requested_length_and_charset() {
        let password = generate_password(constants::GENERATED_ADMIN_PASSWORD_LEN);
        assert_eq!(
            password.chars().count(),
            constants::GENERATED_ADMIN_PASSWORD_LEN
        );
        assert!(password.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_passwords_are_not_repeated() {
        let a = generate_password(24);
        let b = generate_password(24);
        assert_ne!(a, b, "两次生成的口令不应相同");
    }
}
