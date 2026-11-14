//! 员工基础信息的**只读查询接缝**（P2-35 · P2-B 解环）。
//!
//! # 这个文件解决什么
//!
//! `biz_employee` 是 HR 的表（`V0383` 建），但**别的域也在读它**：`erp/cost/cost_labor_*`
//! 直接对这张表写 SQL（`employee_table_present` 探表、`employee_ids_by_no` 按工号批量解析，
//! 2026-11-15 实测 `business/erp/` 生产面共 **19 行**提到 `biz_employee`）。
//! 「谁的表」与「谁在读写」分离成两个事实，代价是：
//!
//! - 表改名 / 加列 / 换软删口径（`deleted_at` → 别的列）时，改一处不够，要全仓 grep；
//! - 消费方的单元测试**必须连真库**才能验证「工号解析」，因为 SQL 长在它自己身上；
//! - 多组织（ARCH-9-1）落地时，组织维度要加到**每一处**直连上，漏一处就是越权读。
//!
//! 本模块把「读员工基础信息」收敛成一个 trait + 一个注册表，**实现在 HR 侧**
//! （[`super::employee_reader_impl`]），消费方（P2-36 的 `erp`）只认这个接缝：
//!
//! | 谁 | 依赖方向 | 说明 |
//! |---|---|---|
//! | `hr`（本模块） | 只依赖框架 + 本域 | 本文件**不出现任何业务域路径**（`seam_source_is_free_of_business_paths` 钉住，含注释） |
//! | `erp` 等消费方 | → `hr::employee_reader` | 调本模块的 `employees_by_ids` / `employees_by_emp_nos`，不碰 `biz_employee` |
//! | 实现方（这里只有 HR 自己） | 启动期 [`install`] 注入 | 表属主是唯一有能力实现它的人 |
//!
//! # 为什么是 trait + 注册表，而不是把 `employee_model` 的函数改成 `pub`
//!
//! 三条都是可验证的，不是洁癖：
//!
//! 1. **口径不外泄**：`employee_model` 里有软删口径（`deleted_at IS NULL`）、
//!    组织哨兵（`hr::model::SENTINEL_ORG_ID`）、列名口径（工号列叫 `emp_no` 不是
//!    `employee_no` —— 这个坑 ERP 侧真的踩过，`cost_labor_model` 的注释里有事故记录）。
//!    直调 model 等于把这三件事变成跨域契约，改一次要协调两个域。
//! 2. **消费方能脱库测试**：P2-36 之后 `erp` 侧的导入校验用例可以注入一个假实现，
//!    不必为了「工号能不能解析」建库。直调 model 就做不到（model 的 SQL 是真库才有意义）。
//! 3. **「谁能读员工」在调用点显式可见**：读到 `employee_reader::employees_by_ids(...)`
//!    就知道这是跨域读，评审时能一眼看出新增的跨域耦合；`employee_model::list(...)`
//!    混在 hr 自己的调用里，看不出来。
//!
//! # 为什么注册表只有一个实现（不像 `flow::doc_reader` 那样按 `biz_type` 分组）
//!
//! 员工主档只有一张表、一个属主。分组键存在的理由是「一条审批一个实现」，
//! 这里没有这个维度：两个实现注册进来一定是**重复实现**，必须是错误（见 [`EmployeeRegistry::install`]）。
//!
//! # 没装 reader 时是什么语义（三条，全部是**数据事实**不是错误）
//!
//! | 入口 | 未安装 | 理由 |
//! |---|---|---|
//! | [`employees_by_ids`] / [`employees_by_emp_nos`] | `Ok(vec![])` | 与「查不到人」同形：调用方按「工号不存在」处理，界面能继续操作 |
//! | [`employee_by_id`] | `Ok(None)` | 同上 |
//! | [`is_available`] | `Ok(false)` | 这是唯一能区分「没有这个能力」与「没有这个人」的入口 |
//!
//! 为什么不返回 500：`hr` 与 `erp` 在同一个进程、同一份 `bootstrap` 里注册，
//! 未安装只可能是**启动链漏了一步**（编程错误，`bootstrap` 里 prod `exit(1)` 兜住），
//! 而不是请求能遇到的业务状态。让它在请求期变成 500，等于把一个启动期错误
//! 摊到每一个用户请求上，还查不出根因。
//!
//! # 三条纪律（与 `flow::doc_reader` 同款）
//!
//! 1. **不许跨 `await` 持锁**：先 `.read()` 把 `Arc` 克隆出来、`drop` 掉 guard 再 `await`；
//!    跨 `await` 持锁会把全进程的员工查询串行化；
//! 2. **读锁 poisoning 直接放行**（`into_inner`）：注册期 panic 不该让之后每次读取都 500；
//! 3. **只有一个写入口** [`install`]，调用方固定是 `bootstrap::register_hr_employee_reader`。

use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 员工基础信息快照（跨域读员工时能看到的**全部**字段）。
///
/// # 为什么是这五个字段（每个都能指到调用点 / 表列，不凭想象加）
///
/// | 字段 | 来源列（`V0383`） | 调用点 |
/// |---|---|---|
/// | [`id`](Self::id) | `id` | 工时台账 `employee_id` 就是它；导入校验把工号解析成它 |
/// | [`emp_no`](Self::emp_no) | `emp_no` | 导入回执 / 报错文案点名的「哪个工号没解析到」 |
/// | [`name`](Self::name) | `name` | 展示（**主数据 ⇒ join 实时取名**，AGENTS 约定 13） |
/// | [`dept_id`](Self::dept_id) | `dept_id` | 数据范围裁剪（员工主档按人事归属部门裁剪，ARCH-9 D-12） |
/// | [`status`](Self::status) | `status` | 只算在职 / 在岗时用：`PROBATION` / `ACTIVE` / `SUSPENDED` / `RESIGNED` |
///
/// **敏感字段一律不在快照里**：`id_card` / `bank_account` 要明文得走
/// `GET /business/hr/employee/{id}/sensitive`（`business:hr:employee:sensitive` + 写审计）。
/// 跨域读接口不提供敏感面，少一个泄漏渠道。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmployeeBrief {
    /// 员工主键（`biz_employee.id`）。
    pub id: Uuid,
    /// 工号（`biz_employee.emp_no`，公司内唯一，软删可复用）。
    pub emp_no: String,
    /// 姓名（**实时值** —— 改名后所有单据都应显示新名）。
    pub name: String,
    /// 人事归属部门（`sys_dept.id`，弱引用不建 FK）。
    pub dept_id: Uuid,
    /// 档案状态**落库原值**：`PROBATION` / `ACTIVE` / `SUSPENDED` / `RESIGNED`。
    ///
    /// 给原值而不是归一后的布尔：调用方要跟自己的状态机逐字比对（例如「离职不参与当月计提」），
    /// 归一会在实现方加一个状态时静默改变语义。
    pub status: String,
}

impl EmployeeBrief {
    /// 是否在岗（`PROBATION` 或 `ACTIVE`）。
    ///
    /// 只有这两个状态算「在岗」：`SUSPENDED`（停薪留职）不发薪也不计工时，
    /// `RESIGNED` 更不用提。判据放接缝里，省得每个消费方各写一遍 `status == ...` 然后漏一个。
    pub fn is_on_duty(&self) -> bool {
        matches!(self.status.as_str(), "PROBATION" | "ACTIVE")
    }
}

/// 员工基础信息的只读查询接口（**唯一实现者是 HR 自己**，见模块注释）。
///
/// # 实现方约定
///
/// - 实现放在**表的属主域**里（现在是 `hr/employee_reader_impl.rs`）；
/// - 只读：本 trait **没有**任何写方法。写员工走 HR 的 `employee_model`，
///   跨域写员工（改部门 / 改状态）必须是接口 + 权限码，不能藏在查询接缝里；
/// - 启动期 [`install`] 注入，不要在请求里注册 —— 注册表没有运行期改写路径。
///
/// # 为什么 `employees_by_ids` / `employees_by_emp_nos` 不给默认实现
///
/// 这两个方法各自有真实调用点（P2-36 的工时导入批量解析 + 后续按 id 取姓名），
/// 给个默认 `Ok(vec![])` 等于「漏实现 ⇒ 静默返回查无此人」：导入会把每一行都报成
/// 「工号不存在」，而根因是漏了一个方法。**required ⇒ 漏了编译不过**，这是编译期
/// 唯一能发现漏项的工具（同 `flow::doc_reader` 对 `on_settled` 的取舍）。
#[async_trait]
pub trait EmployeeReader: Send + Sync {
    /// 按 id 批量取员工基础信息（**本 trait 的核心方法**）。
    ///
    /// # 参数口径
    ///
    /// - `ids`：员工主键集合。**允许重复、允许空**（空 ⇒ 不查库，直接 `Ok(vec![])`）；
    /// - 软删（`deleted_at` 非空）的行**不返回** —— 与「查不到」同形，软删就是不存在的语义。
    ///
    /// # 返回值
    ///
    /// - `Ok(vec!)`：查到了若干条；返回顺序必须**确定**（实现方给全序，调用方要求可复现）；
    /// - `Ok(vec![])`：一个都没查到，是**数据事实**；
    /// - `Err(e)`：数据库故障，调用方按 500 处理。
    async fn employees_by_ids(&self, ids: &[Uuid]) -> DbResult<Vec<EmployeeBrief>>;

    /// 按工号批量取（导入把 `emp_no` 解析成 `employee_id` 用）。
    ///
    /// # 参数口径
    ///
    /// - `emp_nos`：工号文本集合，实现方负责 `trim` + 去重 + 丢弃空串；
    /// - 同一工号在**多组织**下可能各有一行（多组织未落地前实际只有一行）：
    ///   实现方必须给出**确定性**的取法（HR 侧现行口径：`DISTINCT ON (emp_no)` + 最小 `id`），
    ///   不能「返回顺序随机 ⇒ 每次导入解析到不同的人」。
    async fn employees_by_emp_nos(&self, emp_nos: &[String]) -> DbResult<Vec<EmployeeBrief>>;

    /// 单条取（默认走批量，实现方通常不需要覆盖）。
    async fn employee_by_id(&self, id: Uuid) -> DbResult<Option<EmployeeBrief>> {
        Ok(self.employees_by_ids(&[id]).await?.into_iter().next())
    }

    /// 员工主档当前是否可读（替换消费方原先的 `to_regclass('public.biz_employee')` 探表）。
    ///
    /// 默认 `Ok(true)`：装上 reader 就说明表的属主在位、迁移已随模块上线；
    /// 表真的缺了（部署事故）由 HR 自己的实现覆盖成真实探针（见 `employee_reader_impl`）。
    /// 默认值存在的理由只有一条：**假实现（单测 / 消费方脱库测试）不必为它写一行**。
    async fn is_available(&self) -> DbResult<bool> {
        Ok(true)
    }
}

/// 员工只读查询器的注册表（**实例**版，可单测；进程级单例见 [`registry`]）。
#[derive(Default)]
pub struct EmployeeRegistry {
    /// 唯一实现者（表的属主）。`None` = 启动链还没装（或单测里被清空）。
    reader: Option<Arc<dyn EmployeeReader>>,
}

impl EmployeeRegistry {
    /// 空注册表（未安装状态）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 安装实现。
    ///
    /// # 异常情况
    ///
    /// 已经装了还再装一次返回 `Err`（**不静默覆盖**）：员工主档只有一个属主，
    /// 两个实现抢一个接缝，谁赢取决于调用顺序 —— 这种不确定性必须在启动期暴露出来
    /// （`bootstrap` 里 prod `exit(1)`、其它 profile `panic`）。
    pub fn install(&mut self, reader: Arc<dyn EmployeeReader>) -> std::result::Result<(), String> {
        if self.reader.is_some() {
            return Err("员工只读查询器重复安装：biz_employee 只有一个属主（hr）".to_string());
        }
        self.reader = Some(reader);
        Ok(())
    }

    /// 取实现（未安装 → `None`）。
    pub fn reader(&self) -> Option<Arc<dyn EmployeeReader>> {
        self.reader.clone()
    }

    /// 是否已安装。
    pub fn is_installed(&self) -> bool {
        self.reader.is_some()
    }
}

/// 进程级注册表。
///
/// # 为什么是 `RwLock` 而不是 `OnceLock<Arc<dyn …>>`
///
/// 与 `flow::doc_reader` 同一个理由：装载动作必须由 `bootstrap` 驱动，
/// 而 `OnceLock::get_or_init` 的闭包里只能调本模块能看到的东西 —— 那正好变成
/// 「接缝 → 实现」的反向依赖，把要解的边又装回去。所以是「空表先就位、
/// 启动期由属主写满、之后只读」，用 `RwLock` 表达这个两阶段。
static REGISTRY: OnceLock<RwLock<EmployeeRegistry>> = OnceLock::new();

/// 取进程级注册表（惰性初始化成空表）。
fn registry() -> &'static RwLock<EmployeeRegistry> {
    REGISTRY.get_or_init(|| RwLock::new(EmployeeRegistry::new()))
}

/// 读锁（poisoning 时取回内部值，见模块注释纪律 2）。
fn read_registry() -> std::sync::RwLockReadGuard<'static, EmployeeRegistry> {
    registry().read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 写锁（同上）。
fn write_registry() -> std::sync::RwLockWriteGuard<'static, EmployeeRegistry> {
    registry().write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 启动期安装实现（`bootstrap` 调用；重复安装返回 `Err`）。
pub fn install(reader: Arc<dyn EmployeeReader>) -> std::result::Result<(), String> {
    write_registry().install(reader)
}

/// 是否已安装 reader（启动期自检 / 单测断言用）。
pub fn is_installed() -> bool {
    read_registry().is_installed()
}

/// 取已安装的实现（未安装 → `None`）。
pub fn reader() -> Option<Arc<dyn EmployeeReader>> {
    read_registry().reader()
}

/// 按 id 批量取员工基础信息（消费方入口）。
///
/// 未安装 reader → `Ok(vec![])`（语义见模块注释「没装 reader 时是什么语义」）。
pub async fn employees_by_ids(ids: &[Uuid]) -> DbResult<Vec<EmployeeBrief>> {
    // 锁在 await 之前放掉：先克隆 Arc。
    let reader = reader();
    match reader {
        None => Ok(Vec::new()),
        Some(reader) => reader.employees_by_ids(ids).await,
    }
}

/// 按工号批量取员工基础信息（消费方入口，未安装 → `Ok(vec![])`）。
pub async fn employees_by_emp_nos(emp_nos: &[String]) -> DbResult<Vec<EmployeeBrief>> {
    let reader = reader();
    match reader {
        None => Ok(Vec::new()),
        Some(reader) => reader.employees_by_emp_nos(emp_nos).await,
    }
}

/// 按 id 取单个员工（未安装 → `Ok(None)`）。
pub async fn employee_by_id(id: Uuid) -> DbResult<Option<EmployeeBrief>> {
    let reader = reader();
    match reader {
        None => Ok(None),
        Some(reader) => reader.employee_by_id(id).await,
    }
}

/// 员工主档是否可读（未安装 → `Ok(false)`）。
pub async fn is_available() -> DbResult<bool> {
    let reader = reader();
    match reader {
        None => Ok(false),
        Some(reader) => reader.is_available().await,
    }
}

/// 清空进程级注册表（**仅供单测**，生产代码不许调用）。
#[cfg(test)]
pub fn reset_for_tests() {
    let mut registry = write_registry();
    *registry = EmployeeRegistry::new();
}

/// 进程级注册表的单测串行锁（**仅供单测**）。
///
/// 注册表是进程级单例：所有「reset / install + 断言」的用例（含真库用例）
/// 必须拿**同一把**锁，否则断言会随执行顺序漂移。
#[cfg(test)]
pub static GLOBAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
