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
//! 5. 装载运行时参数快照（见 [`load_runtime_config`]），供订阅了参数的插件直接取用；
//! 6. 补齐当年 12 个会计期间行（见 [`ensure_accounting_periods`]）—— 年度来自时钟，
//!    写不进迁移，缺了会让凭证落到「不存在的期间」上（巡检 F2）。
//!
//! 内置超级管理员角色、菜单与按钮权限点等**无需运行期计算的种子数据**已全部下沉到
//! `migrations/seed/S__*.sql` 种子迁移；后续新增功能只需追加迁移，无需改动本文件。
//!
//! 基础设施的初始化由框架在 `App::start()` 内完成，不要在此重复做。

use nik::prelude::*;

use crate::system::role::model::SysRoleModel;
use crate::system::user::model::{SysUserModel, SysUserRoleModel};
use cloudpivot_kernel::shared::constants;

/// P2-33（P2-B 解环）：把 ERP 的 16 个单据查询器注册进 `flow` 的进程级注册表。
///
/// # 为什么要在这里、而且要「失败即停」
///
/// 注册表是「启动期写、请求期读」的两阶段结构（见 `flow::doc_reader` 的模块注释）。
/// 注册不全的后果是**静默的**：漏掉的那条审批线，内控闸会退化成放行
/// （`guard_self_approval_of_code` 对未注册的业务类型返回 `Ok(())`），
/// 终态回调也没人接 —— 单据会停在「审批已结束、单据没动」的中间态，
/// 而没有任何待办能推进它（AGENTS.md 约定 11 的事故形态）。
///
/// 所以：
/// - **prod 拒启动**（`exit(1)`，与 [`preflight_permission_guard`] / [`ai_agent_invariant_guard`]
///   同一先例）；
/// - 其它 profile **`panic`**：这是纯编程错误（重复注册 = 两个实现抢一个 `biz_type`），
///   不依赖任何数据，开发机上必须立刻炸出来而不是「日志里有条 error」。
///
/// # 幂等性
///
/// 本函数**只允许调一次**（`bootstrap` 只跑一次）。重复调用会因注册表已有这些键而返回
/// `Err` 并点名 —— 这正是想要的行为：二次注册说明有人把注册挪到了请求路径上。
/// 注册「审批实例落终态」的订阅者（P2-34 · P2-B 解环）。
///
/// 目前唯一订阅者是 `erp::linkage::flow_instance_trigger`（`FLOW_INSTANCE` 联动触发点：
/// 结论落库后发通知 / 建任务）。它原来住在 `flow::instance_linkage` 里、被 `flow::handler`
/// 与 `flow::timeout` 直接调用 —— 那条 `flow → erp` 的边就是这样来的。
///
/// # 失败口径（与 [`register_doc_readers`] 同款）
///
/// 注册失败只可能是「重名注册 = 编程错误」：prod `exit(1)`、其它 profile `panic`。
/// 漏注册的后果是**静默**的 —— 审批结论落库了，联动规则永远不触发（通知不发、任务不建），
/// 没有任何错误日志会指向这里。
fn register_flow_hooks() {
    match crate::business::erp::linkage::flow_instance_trigger::register_into_flow_hooks() {
        Ok(count) => tracing::info!(
            count,
            "审批终态订阅者注册完成（P2-34）：FLOW_INSTANCE 联动改由 flow::instance_hooks 扇出"
        ),
        Err(error) => {
            let message = format!("审批终态订阅者注册失败（订阅表不完整）：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            panic!("{message}");
        }
    }
}

fn register_doc_readers() {
    match crate::business::erp::doc_reader_impl::register_all() {
        Ok(count) => tracing::info!(
            count,
            "ERP 单据查询器注册完成（P2-33）：flow 侧的终态扇出 / 内控闸 / 制单人查询改走注册表"
        ),
        Err(error) => {
            let message = format!("ERP 单据查询器注册失败（注册表不完整）：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            panic!("{message}");
        }
    }
}

/// P2-35（P2-B 解环）：把 HR 的「员工只读查询器」装进 `hr::employee_reader` 的注册表。
///
/// # 为什么在启动期同步做、失败了为什么不能继续
///
/// 消费方（`erp/cost/cost_labor_*`，P2-36 切换）读员工基础信息**只认这个注册表**。
/// 漏安装的后果是**静默的**：`employees_by_emp_nos` 对未安装状态返回 `Ok(vec![])`
/// （见 `employee_reader` 模块注释「没装 reader 时是什么语义」），于是工时导入会把
/// **每一行**都报成「员工工号不存在」—— 界面上是数据问题，根因是启动链少了一步。
///
/// 所以口径与 [`register_doc_readers`] / [`register_flow_hooks`] 一致：
/// prod `exit(1)`、其它 profile `panic`（重复安装是纯编程错误，不依赖任何数据）。
fn register_hr_employee_reader() {
    match crate::business::hr::employee_reader_impl::install() {
        Ok(()) => tracing::info!("HR 员工只读查询器安装完成（P2-35）：跨域读 biz_employee 改走 employee_reader 注册表"),
        Err(error) => {
            let message = format!("HR 员工只读查询器安装失败（注册表不完整）：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            panic!("{message}");
        }
    }
}

/// P2-37 / P2-38（P2-B 解环）：把接线域的实现装进 `task::approval_port` 与 `flow::task_port`。
///
/// # 为什么在启动期同步做、失败了为什么不能继续
///
/// 两条边各有一张注册表，**全部只认注册表**（P2-37 之前 `task` 直调 `flow::task_bridge`，
/// P2-38 之前 `flow` 直调 `task::model`）。漏安装的后果是**静默的**：
///
/// - `task` 侧：见 `task::approval_port` 的降级表 —— 提交验收不再自动发起审批、
///   流程任务完成后不再推进流程节点、取消任务不再撤在跑待办，而任务侧状态**照样落库**；
/// - `flow` 侧：`TASK_NODE` 造不出任务（`flow::task_port` 未装时返回 `Failed(500)`，
///   引擎整笔回滚），审批结论也不再推进任务。
///
/// 这类「半接线」正是 AGENTS 约定 11 说的死单温床，所以口径与 [`register_doc_readers`] /
/// [`register_hr_employee_reader`] 一致：prod `exit(1)`、其它 profile `panic`
/// （重复安装是纯编程错误，不依赖任何数据）。
fn register_task_approval_port() {
    match crate::business::taskflow::install_ports() {
        Ok(()) => tracing::info!(
            approval_port = crate::business::task::approval_port::port_name().unwrap_or(""),
            flow_task_port = crate::business::flow::task_port::port_name().unwrap_or(""),
            "任务 ⇄ 审批流 接缝安装完成（P2-37/P2-38）：两边都只认注册表，接线实现在 taskflow"
        ),
        Err(error) => {
            let message = format!("任务 ⇄ 审批流 接缝安装失败（注册表不完整）：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            panic!("{message}");
        }
    }
}

/// P2-B 解环（卡 `t_3442b4fb`）：把凭证域的「记账能力」装进 `shared::voucher_port` 的注册表。
///
/// # 为什么在启动期同步做、失败了为什么不能继续
///
/// HR 的工资过账 / 红冲**只认这条接缝**（P2-B 之前是直调 `erp::voucher` /
/// `voucher_multi`）。漏安装的后果是**刺眼的 500**（不是静默错账）：
/// 接缝的写方法在未安装时返回 `InvalidValue("凭证写入接缝未安装…")`，
/// 用户看到的是「过账失败」而不是「过账成功但账上没凭证」。
///
/// 但「用户点过账报 500」同样是事故，所以口径与 [`register_doc_readers`] /
/// [`register_hr_employee_reader`] / [`register_task_approval_port`] 一致：
/// prod `exit(1)`、其它 profile `panic`（重复安装是纯编程错误，不依赖任何数据）。
fn register_voucher_port() {
    match crate::business::erp::voucher::voucher_port_impl::install() {
        Ok(()) => tracing::info!(
            port = cloudpivot_kernel::business::shared::voucher_port::port_name().unwrap_or(""),
            "凭证写入接缝安装完成（P2-B 解环）：hr 的工资过账 / 红冲改走 shared::voucher_port"
        ),
        Err(error) => {
            let message = format!("凭证写入接缝安装失败（注册表不完整）：{error}");
            tracing::error!("{message}");
            if is_prod_profile() {
                eprintln!("FATAL: {message}");
                std::process::exit(1);
            }
            panic!("{message}");
        }
    }
}

/// 内核基座的两条读取接缝（P2-59 · C1）：系统参数读取 / 会话安全字段读取。
///
/// # 为什么要装
///
/// 基座（`cloudpivot-kernel`）里的空闲超时、成本核算开关、内控开关、会话字段缓存都要
/// 读「属于平台层」的数据（`sys_config` / `sys_user`），抽包后基座不能反向依赖
/// `support` / `system`，所以实现留在域里、启动期装进注册表。
///
/// # 装不上的语义
///
/// - 配置读取：未装 = 读参数一律 `None`（各调用点都有自己的兜底），所以这里**必须**
///   装成功，装不上按「启动链缺陷」处理（prod `exit(1)`、其余 profile `panic`）；
/// - 会话字段读取：未装 = 鉴权路径**大声失败**（`DbError::InvalidValue`），
///   同样必须启动期装好。
fn register_kernel_ports() {
    use std::sync::Arc;

    match cloudpivot_kernel::shared::config_port::install(Arc::new(crate::support::config::model::SysConfigReader)) {
        Ok(()) => tracing::info!(
            port = cloudpivot_kernel::shared::config_port::port_name().unwrap_or(""),
            "配置读取接缝安装完成（P2-59）：空闲超时 / 成本开关 / 内控开关走 config_port"
        ),
        Err(error) => fatal_port_install("配置读取接缝", &error),
    }

    match cloudpivot_kernel::shared::session_flags_port::install(Arc::new(
        crate::system::user::model::SessionFlagsReader,
    )) {
        Ok(()) => tracing::info!(
            port = cloudpivot_kernel::shared::session_flags_port::port_name().unwrap_or(""),
            "会话字段读取接缝安装完成（P2-59）：shared::session_cache 未命中时回到 system::user"
        ),
        Err(error) => fatal_port_install("会话字段读取接缝", &error),
    }
}

/// 接缝安装失败的处理（与 [`register_voucher_port`] 同口径）：prod `exit(1)`，其余 `panic`。
fn fatal_port_install(what: &str, error: &str) {
    let message = format!("{what}安装失败（注册表不完整）：{error}");
    tracing::error!("{message}");
    if is_prod_profile() {
        eprintln!("FATAL: {message}");
        std::process::exit(1);
    }
    panic!("{message}");
}

/// 测试用接线（`#[cfg(test)]`）：lib 单测没有启动链，跨域接缝的实现只能在用例里显式装。
///
/// # 为什么这个入口在 `bootstrap` 而不是在 HR 的测试文件里
///
/// `bootstrap` 是**同时看得到两侧**的地方（消费者 `hr` / 实现者 `erp`），而它本身不是业务域
/// —— 用例 `use crate::bootstrap::…` 不会产生任何业务域之间的引用。
/// 反过来在 `hr/salary_bridge_db_tests.rs` 里写 `use crate::business::erp::…` 就会留下一条
/// `hr → erp` 引用，正是本卡要摘的东西（`module_cycle_report` 连测试文件一起扫）。
///
/// 幂等：多次调用只装一次（`OnceLock`）。
#[cfg(test)]
pub(crate) fn install_voucher_port_for_tests() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let port = cloudpivot_kernel::business::shared::voucher_port::is_installed();
        match crate::business::erp::voucher::voucher_port_impl::install() {
            Ok(()) => {}
            // 已被别的用例装过：正常（注册表是进程级）
            Err(_) if port => {}
            Err(error) => panic!("测试期安装凭证写入接缝失败：{error}"),
        }
    });
}

/// 测试用接线（`#[cfg(test)]`）：内核基座的两条**读取接缝**（P2-59 · C1）在 lib 单测里
/// 没有启动链可依 —— [`register_kernel_ports`] 只在 `bootstrap()`（`App::start()` 的钩子）
/// 里跑，而 `cargo test --lib` 从不跑启动链。不装就跑出两类假红（与业务代码无关）：
///
/// - `session_flags_port` 未装 ⇒ `shared::session_cache` 未命中的回源**大声失败**
///   （`DbError::InvalidValue("…接缝未安装实现…")`）⇒ `system::session_tests` 的
///   s02 / s06 / s07 三条红；
/// - `config_port` 未装 ⇒ 内控开关 / 成本开关 / 空闲超时一律回退代码默认值，
///   而 `stockin_handler_tests_pflow3a::bug02` **靠运行期改开关**来证明内控闸真的接上了，
///   读成 `None`（= 关）时闸永远不拦 ⇒ 该用例红（「内控开关打开时制单人不能自己过账」）。
///
/// # 为什么两条一起装（而不是「只给需要的那条用例装」）
///
/// 注册表是**进程级**的（`OnceLock` + `RwLock`，无卸载口）：用例里 `install` 一次，
/// 同进程后续所有用例都看得见。只给一条用例装就等于「同一个进程里，排在前面的用例读代码
/// 默认值、排在后面的用例读库值」—— 那是**顺序相关的抖动**，比确定性安装难查得多。
/// 因此统一在这里装，并用「装与不装读到同一值」把影响面收干：模板库（
/// `scripts/verify/make_full_suite_db.sh` 建的 `cloudpivot_test_full`，178 表 / 336 迁移）
/// 的种子值与代码默认**逐键相等**，实测取值：
///
/// | 键（读者） | 种子（迁移 / 实测值） | 未装时的回退 |
/// |---|---|---|
/// | `auth.idle_timeout_minutes`（`idle_session`） | `15`（`S__config_data.sql`，实测 `15`） | `DEFAULT_IDLE_TIMEOUT_MINUTES = 15` |
/// | `erp.cost.enabled`（`business::shared::cost_switch`） | `false`（实测 `false`） | `false`（读不到即关） |
/// | `erp.separation_of_duties.enabled` / `.post` | `false`（V0196，实测 `false`） | `Duty::{Approve,Pay,…}::default_enabled()` 对应 `false` |
/// | `erp.separation_of_duties.approve` / `.pay` | `true`（V0196，实测 `true`） | `true` |
/// | `erp.separation_of_duties.{approve_execute,stocktake,purchase_accept}` | `false`（V0340，实测 `false`） | `false` |
/// | `erp.separation_of_duties.self_approval_max_amount` | `0`（V0338，实测 `0`） | `SELF_APPROVAL_MAX_AMOUNT_DEFAULT = 0.0` |
///
/// 于是**唯一**的行为变化就是本卡要的那一处：`bug02` 运行期把 `.post` 改成 `true`
/// 之后内控闸真的拦得住（这正是它要证明的事），其余用例读到的东西一字不差。
///
/// # 它为什么在 `bootstrap`
///
/// 与 [`install_voucher_port_for_tests`] 同理：`bootstrap` 同时看得到两侧
/// （消费者 `system` / `erp`、实现者 `system::user::model` / `support::config::model`），
/// 而它本身不是业务域 —— 用例 `use crate::bootstrap::…` 不会新增任何域间引用
/// （`module_cycle_report` 连测试文件一起扫）。
///
/// 幂等：`std::sync::Once` 只执行一次；已被别处装过时跳过（注册表进程级，重复安装会报错）。
/// 生产构建里符号不存在（`#[cfg(test)]`）。
#[cfg(test)]
pub(crate) fn install_kernel_ports_for_tests() {
    use std::sync::{Arc, Once};

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if !cloudpivot_kernel::shared::session_flags_port::is_installed() {
            cloudpivot_kernel::shared::session_flags_port::install(Arc::new(
                crate::system::user::model::SessionFlagsReader,
            ))
            .unwrap_or_else(|error| panic!("测试期安装会话字段读取接缝失败：{error}"));
        }
        if !cloudpivot_kernel::shared::config_port::is_installed() {
            cloudpivot_kernel::shared::config_port::install(Arc::new(crate::support::config::model::SysConfigReader))
                .unwrap_or_else(|error| panic!("测试期安装配置读取接缝失败：{error}"));
        }
    });
}

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
    // ⚠ 例外（t_92234f6b）：`seed_super_admin` 与 `seed_demo_users` 之间**有依赖**，
    //    必须串成一步按序执行 —— 演示账号种子的「部门补漏」要把内置超管 `admin` 一起补
    //    （N2 把超管也算「在用 HUMAN」，漏了他照样 WARN）。两者并发时实测 admin 往往
    //    后于演示账号落地（`join!` 不保证顺序），补漏那一句会 UPDATE 0 行。
    //
    // 第 5 步 `preflight_permission` 是批次 B 的「闸 3」（架构 §2.4）：查
    // `information_schema` 确认登记表的部门列 / 归属列真实存在，并确认
    // `SUPER_ADMIN` 角色存在。它是**唯一**在「资源就绪、插件未启动」时执行的入口，
    // 因此把「部署即失败」的校验放在这里（与 `seed_super_admin` 的 prod `exit(1)` 先例一致）。
    // 单据查询器注册（P2-33 · P2-B 解环）：`flow` 侧只认注册表，实现在 ERP 业务域里。
    // 必须在 `tokio::join!` 之前、在这里同步做完：任何一次审批请求都可能读注册表
    // （内控闸 / 终态扇出），带着半个注册表跑起来 = 随机几条审批线没有内控闸。
    register_doc_readers();
    // 审批终态订阅者（P2-34）：`flow` 在结论落库后扇出，实现在 ERP 业务域里注册进来。
    // 同样必须在 `tokio::join!` 之前同步做完 —— 漏注册 = 联动规则永远不触发且没有报错。
    register_flow_hooks();
    // 员工只读查询器（P2-35 · P2-B 解环）：跨域读 `biz_employee` 只认这张注册表，
    // 实现在 HR 业务域里。同样同步做完：漏装 = 消费方把「查不到人」当成数据问题。
    register_hr_employee_reader();
    // 任务审批接缝（P2-37 · P2-B 解环）：`task` 对审批的四条硬连接只认这张注册表，
    // 实现在审批域里。漏装 = 提交验收不发起审批 / 流程节点不推进，且任务侧不报错。
    register_task_approval_port();
    // 凭证写入接缝（P2-B 解环 · t_3442b4fb）：hr 的工资过账 / 红冲只认 shared::voucher_port，
    // 实现在凭证域里。漏装 = 过账直接 500（大声失败），所以启动期就装、装不上就退出。
    register_voucher_port();
    // 内核基座读取接缝（P2-59 · C1）：配置读取 / 会话字段读取。必须早于任何一次
    // 参数读取（空闲超时 / 成本开关 / 内控开关）与会话字段回源。
    register_kernel_ports();

    tokio::join!(
        async {
            seed_super_admin().await;
            seed_demo_users().await;
        },
        load_scheduled_jobs(),
        crate::support::dict::model::warm_dict_cache(),
        load_runtime_config(),
        preflight_permission_guard(),
        ai_agent_invariant_guard(),
        orphan_dependency_guard(),
        test_residue_guard(),
        ensure_accounting_periods(),
        freeze_published_sign_modes(),
        sse_query_token_guard(),
    );
}

/// P3-06：存量 `APPROVAL` 节点的签名模式冻结（**已发布 / 归档版本**这一半）。
///
/// # 为什么在启动期而不是纯 SQL 迁移
///
/// 草稿版本的行由 `V0339` 的 SQL 冻结（草稿没人读）。已发布版本不行：
/// 实例真正读的是 `biz_flow_def_version.graph_snapshot`，而 `graph_hash` 是
/// 「从行重建的 canonical JSON」的 sha256 —— 只改行会让快照与 hash 双双对不上，
/// `flow::maintain` 的快照自检（每天 03:30）会把这些版本逐条报成「行被直接修改」。
/// 详见 [`crate::business::flow::sign_freeze`] 的模块注释。
///
/// # 幂等与失败处理
///
/// 只处理 `approval_mode IS NULL` 的行，跑完就再也找不到目标（每次启动一条索引扫描）。
/// 失败**不阻断启动**（钩子没有错误通道）：记 error，下次启动重试。
async fn freeze_published_sign_modes() {
    if try_db().is_none() {
        tracing::error!("数据库未初始化，跳过 P3-06 签名模式冻结");
        return;
    }
    match crate::business::flow::sign_freeze::freeze_published_versions().await {
        Ok(report) if report.is_empty() => {
            tracing::debug!("P3-06 签名模式冻结：无需处理（已发布版本里没有空模式的审批节点）")
        }
        Ok(report) => tracing::info!(
            versions = report.versions,
            nodes = report.nodes,
            all = report.all,
            any = report.any,
            "P3-06 签名模式冻结完成（已发布版本：快照 + hash + 行 + 实例节点 同改）"
        ),
        Err(error) => {
            tracing::error!(error = %error, "P3-06 签名模式冻结失败（不阻断启动，下次启动重试）")
        }
    }
}

/// 演示账号种子（FIX-1 · 新环境开箱即用）。
///
/// # 触发条件
///
/// **只有**环境变量 `NIK_SEED_DEMO_USERS=true|1|yes|on` 才建账号；
/// 缺省 / `false` / 写错一律不建（生产默认安全）。
///
/// # 为什么放在启动链里（而不是只在接口里）
///
/// 「新环境开箱即用」的定义是**启动完就能跑链路**，不是「先调一个接口」：
/// 开发 / 测试机一条 `NIK_SEED_DEMO_USERS=true cargo run` 就该拿到演示账号
/// 与 6 个关键角色的成员，六条链立刻能点。接口版（`POST /system/setup/demo-users`）
/// 是给「忘了设环境变量」的运维补救，两者走**同一个**模型函数，口径不会分叉。
///
/// # 幂等
///
/// 已存在的账号整条跳过（见 [`crate::system::setup::model::seed_demo_users`]），
/// 只对 `dept_id IS NULL` 的老账号补写部门（t_92234f6b）。
/// 145 测试环境早就手工建过 `demo_lead`，本步只把它记进「跳过」列表，
/// 不重复建、不改它的口令与已有部门。
///
/// # 失败处理
///
/// 种子失败**不阻断启动**（钩子没有错误通道）：只记 error 日志，服务照常起来
/// —— 起不来的服务连「去向导页手工配」的机会都没有。
async fn seed_demo_users() {
    use crate::system::setup::model as setup;

    if !setup::demo_users_enabled() {
        tracing::info!(env = setup::ENV_SEED_DEMO_USERS, "演示账号种子未开启（生产默认），跳过");
        return;
    }
    if try_db().is_none() {
        tracing::error!("数据库未初始化，跳过演示账号种子");
        return;
    }

    match setup::seed_demo_users(&setup::demo_password()).await {
        Ok(outcome) => tracing::info!(
            created = outcome.created.len(),
            existed = outcome.existed.len(),
            bindings_added = outcome.bindings_added,
            dept_created = outcome.dept_created,
            dept_backfilled = outcome.dept_backfilled,
            created_users = ?outcome.created,
            existed_users = ?outcome.existed,
            "演示账号种子完成（幂等：已有账号不重复建）"
        ),
        Err(error) => tracing::error!(error = %error, "演示账号种子失败（不阻断启动）"),
    }
}

/// SSE 实时推送的鉴权自检（P1-FIX-6）。
///
/// # 为什么需要
///
/// 浏览器 `EventSource` 不能设置 `Authorization` 头，SSE 订阅只能退化成
/// `?token=<access token>`；而认证层**只**对白名单里的端点采信查询令牌。白名单一旦漏了
/// 实时推送端点，症状是「每个页面登录后都在控制台刷一条
/// `401 GET /api/business/notice/stream`」，实时推送静默失效（前端只剩 30 秒轮询兜底）。
/// 这个症状此前只能靠人肉逐页看控制台发现，因此把判定搬到首启日志里。
///
/// 判定用的 [`resolve_query_token_paths`] 与框架装配认证层时调用的是**同一个函数**，
/// 所以「启动日志说了什么」与「运行期实际采信哪些路径」不会分叉
/// （环境变量 `NIK_AUTH_QUERY_TOKEN_PATHS` 优先、配置项 `[middleware.auth].query_token_paths` 兜底）。
///
/// # 处置
///
/// 只告警、不拦启动：漏配只影响实时通道（站内信本身仍可用），不该让它成为部署阻塞项。
async fn sse_query_token_guard() {
    // 站内信 SSE 端点在宏里的注册路径（不含 `[router] prefix`）。
    const SSE_ENDPOINT: &str = "/business/notice/stream";

    let prefix = cfg().router.prefix.clone();
    let expected = join_path_prefix(&prefix, SSE_ENDPOINT);
    let whitelist = resolve_query_token_paths(&prefix, &cfg().middleware.auth.query_token_paths);
    if whitelist
        .iter()
        .any(|allowed| allowed.trim_end_matches('/') == expected)
    {
        tracing::info!(
            endpoint = %expected,
            whitelist = ?whitelist,
            "SSE 鉴权自检通过：EventSource 可用 ?token= 订阅实时通知"
        );
    } else {
        tracing::warn!(
            endpoint = %expected,
            whitelist = ?whitelist,
            env_var = ENV_QUERY_TOKEN_PATHS,
            "SSE 鉴权自检未通过：白名单未覆盖实时推送端点 → 浏览器 EventSource 将 401、实时推送静默失效。\
             修法：配置 [middleware.auth] query_token_paths = [\"/business/notice/stream\"]，\
             或注入 NIK_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream"
        );
    }
}

/// 启动保底：当年 12 个会计期间行必须都在（卡 P0-FIX-5）。
///
/// # 为什么放在启动钩子而不是只写给迁移
///
/// 迁移只跑一次，而「当年」每年都在变：`V0221` 把**迁移当年**的 12 期建齐之后，
/// 到了下一年 1 月 1 日库里又没有新一年的期间行 —— `voucher::period_of` 对缺失期间
/// 是 fail-open 的，凭证照样入库、期间却指空，巡检 F2（凭证期间必有期间记录）
/// 立刻转 WARN，首笔业务就报警。这正是本文件开头那句「**需要运行期计算**的种子
/// 数据放这里」的场景：年度来自时钟，没法写死进 SQL。
///
/// 口径与 `V0221` 完全一致（12 期、`status = OPEN`、只补齐缺失行不覆盖已有行），
/// 实现只有一份：[`crate::business::erp::period::ensure_year_periods`]。
///
/// # 异常情况
///
/// 查库失败（连不上 / 权限不足）只记 `warn` 并继续启动：期间行缺失不会让服务起不来，
/// 但不该由它单点拖垮启动（与同文件其它 guard 的处置一致）。
async fn ensure_accounting_periods() {
    match crate::business::erp::period::ensure_current_year_periods().await {
        Ok(0) => tracing::info!("会计期间保底：当年 12 期已齐，无需补建"),
        Ok(created) => tracing::info!(created, "会计期间保底：已补齐当年缺失的会计期间（status = OPEN）"),
        Err(error) => tracing::warn!("会计期间保底无法完成（继续启动）：{error}"),
    }
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
    match cloudpivot_kernel::shared::startup_guard::count_test_residue().await {
        Ok(residue) => match cloudpivot_kernel::shared::startup_guard::test_residue_warning(residue) {
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
    match cloudpivot_kernel::shared::startup_guard::count_orphan_task_dependencies().await {
        Ok(count) => match cloudpivot_kernel::shared::startup_guard::orphan_dependency_warning(count) {
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
    // E15 安全红线巡检（P2-59 · C1）：随 `aiagent` 域走，启动链在这里调一次。
    // 放在不变式判定之前：不变式坏了也要把「哪些 profile 没有红线」一起点出来
    // （与改造前 `check_ai_agent_invariant` 内部的调用顺序一致）。
    crate::business::aiagent::safety::warn_missing_safety_block().await;
    match cloudpivot_kernel::shared::startup_guard::check_ai_agent_invariant().await {
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
///   [`cloudpivot_kernel::business::shared::guard`] 单测不依赖真库）。
///
/// # 异常情况
///
/// 查库失败（连不上 / 权限不足）与「自检不健康」同样处理：prod 拒启动，其余告警。
async fn preflight_permission_guard() {
    let report = match cloudpivot_kernel::business::shared::guard::preflight_permission().await {
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
    tracing::warn!("数据权限自检不通过（非 prod，继续启动）：{}", report.summary());
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

    // 超管落位到根部门（E2E-14 评审 N2）：账号是上面这一步刚建出来的，迁移执行时
    // 库里还没有这一行，所以只能在这里补 —— 详见
    // [`crate::system::dept::model::land_users_into_dept_by_code`]。
    // 只认 `dept_code = HQ`（没有这个部门的环境一行不动），只补 `dept_id IS NULL`。
    match crate::system::dept::model::land_users_into_dept_by_code(
        &[constants::ADMIN_USERNAME],
        constants::ROOT_DEPT_CODE,
    )
    .await
    {
        Ok(landed) if landed.is_empty() => {
            tracing::debug!("超管部门落位：无需处理（已有部门，或本库没有 HQ 根部门）")
        }
        Ok(landed) => tracing::info!(
            users = ?landed,
            dept = constants::ROOT_DEPT_CODE,
            "超管部门落位：admin 原先没有部门，已归入根部门"
        ),
        Err(e) => tracing::error!(error = %e, "超管部门落位失败（不影响启动，下次启动重试）"),
    }

    // 内置超级管理员角色由种子迁移创建，此处只查询其主键用于绑定。
    let role = match SysRoleModel::find_first_by(db, "role_code", (constants::SUPER_ADMIN_ROLE_CODE,)).await {
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
    if let Some(user) = SysUserModel::find_first_by(db, "username", (constants::ADMIN_USERNAME,)).await? {
        tracing::info!(username = constants::ADMIN_USERNAME, "管理员账号已存在，跳过创建");
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
    let password =
        tokio::task::spawn_blocking(move || Crypto::hash_password(&password_plain).map_err(|e| e.to_string()))
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

    tracing::info!(username = constants::ADMIN_USERNAME, "已写入管理员账号种子数据");
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
    let existing = SysUserRoleModel::count_by(db, "user_id = ? AND role_id = ?", (user_id, role_id)).await?;
    if existing > 0 {
        tracing::info!(username = constants::ADMIN_USERNAME, "管理员角色绑定已存在，跳过创建");
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
        assert_eq!(password.chars().count(), constants::GENERATED_ADMIN_PASSWORD_LEN);
        assert!(password.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_passwords_are_not_repeated() {
        let a = generate_password(24);
        let b = generate_password(24);
        assert_ne!(a, b, "两次生成的口令不应相同");
    }
}
