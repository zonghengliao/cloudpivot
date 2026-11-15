//! 成本费率取数（成本核算 · ARCH-9 §4.5.4 / §3.1 ⑥ / §8.1 B2-3）。
//!
//! 设计依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
//!
//! | 章节 | 本模块用到的地方 |
//! |---|---|
//! | §3.1 ⑥ | `biz_cost_rate` 表结构（`rate_type` / `scope` / `scope_id` / 生效期 / `status`） |
//! | §4.5.4 | 取数优先级：`COST_OBJECT`（该批专用）> `DEPT`（车间综合）> `COMPANY`（公司级）；**都取不到 → 报错，不默认 0** |
//! | §6.2.4 | 单条录入费率留空 ⇒ 后端自动取数，出参回填「实际取到的费率 + 来源」 |
//!
//! # 三条口径（本模块的全部规则）
//!
//! 1. **只认活的、覆盖该日期的行**：`status = 1`（`ck_biz_cost_rate_status`）+
//!    未软删（model 侧 `deleted_at IS NULL`）+ `effective_from <= work_date` +
//!    `effective_to IS NULL OR effective_to >= work_date`。
//! 2. **三级优先级写死**：`COST_OBJECT`(3) > `DEPT`(2) > `COMPANY`(1)。同一级里
//!    **生效期更晚的版本赢**（费率是版本化的：`2026-01-01` 的 40 元/小时与
//!    `2026-11-01` 的 50 元/小时并存时，11 月的工时用 50）。
//! 3. **命中范围必须逐字相等**：`DEPT` 行的 `scope_id` 必须是**本行的车间**，
//!    `COST_OBJECT` 行必须是**本行的成本对象**，`COMPANY` 行的 `scope_id` 必须为空
//!    （`ck_biz_cost_rate_scope_id` 已保证，纯函数侧再核一遍防「弱引用指错」）。
//!
//! # 为什么缺费率是硬错误（`422` + `66059`）
//!
//! 「取不到就按 0 计」会让这一批的人工成本凭空少一块，而且**成本计算单照样算得出来**
//! —— 算错的成本比算不出来的成本危险得多（设计 D-7「不猜」/ §4.5.2 硬规则 1）。
//! 报文里点名「成本对象号 + 车间 + 工作日期 + 已查过哪几级 + 怎么修」，用户能自己解决。
//!
//! # 为什么这里没有 HTTP 入口
//!
//! 设计 §5.2 的 36 个接口里**没有费率的增删改查**（费率属基础配置，由财务在
//! `biz_cost_rate` 维护；§5.3 也没有 `cost-rate:*` 权限码）。本模块只提供
//! 「取数」这一件事，被工时台账（单条 #11 / 导入 #15）复用：
//!
//! ```text
//! cost_labor_handler::create_labor ─┐
//! cost_labor_import::import_labors ─┴→ cost_rate_handler::resolve_labor_rate
//!                                        → cost_rate_model::rates_of_type（一次查库）
//!                                        → cost_rate_dto::resolve（纯函数，可单测）
//! ```
//!
//! 费率维护入口缺失这件事已在 COST-B2-3 卡内报告（不属本卡范围，不自行加页面）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

// ==================== 值域（与 V0371 的 CHECK 逐字一致） ====================

/// 费率类型：工资率（元/小时）。
pub const RATE_TYPE_LABOR: &str = "LABOR";
/// 费率类型：机器费率（元/小时，`MACHINE_HOURS` 分摊基数用）。
pub const RATE_TYPE_MACHINE: &str = "MACHINE";
/// 费率类型值域（`ck_biz_cost_rate_type`）。
pub const RATE_TYPES: [&str; 2] = [RATE_TYPE_LABOR, RATE_TYPE_MACHINE];

/// 作用范围：全公司（`scope_id` 必须为空）。
pub const SCOPE_COMPANY: &str = "COMPANY";
/// 作用范围：车间（`scope_id` = `sys_dept.id`）。
pub const SCOPE_DEPT: &str = "DEPT";
/// 作用范围：成本对象（`scope_id` = `biz_cost_object.id`）。
pub const SCOPE_COST_OBJECT: &str = "COST_OBJECT";
/// 作用范围值域（`ck_biz_cost_rate_scope`）。
pub const SCOPES: [&str; 3] = [SCOPE_COMPANY, SCOPE_DEPT, SCOPE_COST_OBJECT];

/// 启用状态（`ck_biz_cost_rate_status`：`1` 启用 / `0` 停用）。
pub const STATUS_ACTIVE: i16 = 1;

/// 取数失败的业务错误码（`66059`，见 `shared::error_code`）。
pub const RATE_MISSING_CODE: i32 = error_code::ERP_COST_RATE_MISSING;

// ==================== 数据结构 ====================

/// `biz_cost_rate` 一行（**只用于取数**，不做增删改）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateRow {
    /// 主键（回填给出参能追到是哪一条费率）。
    pub id: Uuid,
    /// 费率类型 `LABOR` / `MACHINE`。
    pub rate_type: String,
    /// 作用范围 `COMPANY` / `DEPT` / `COST_OBJECT`。
    pub scope: String,
    /// 范围 id（`COMPANY` 为空；`DEPT` = 部门 id；`COST_OBJECT` = 成本对象 id）。
    pub scope_id: Option<Uuid>,
    /// 费率（元/小时，`NUMERIC(18,6)`）。
    pub rate: Decimal,
    /// 生效开始日（含）。
    pub effective_from: NaiveDate,
    /// 生效结束日（含，空 = 长期有效）。
    pub effective_to: Option<NaiveDate>,
    /// `1` 启用 / `0` 停用。
    pub status: i16,
}

impl RateRow {
    /// 是否启用（停用的行永不参与取数）。
    pub fn is_active(&self) -> bool {
        self.status == STATUS_ACTIVE
    }

    /// 生效期是否覆盖 `date`（两端都含）。
    pub fn covers(&self, date: NaiveDate) -> bool {
        self.effective_from <= date && self.effective_to.is_none_or(|to| to >= date)
    }

    /// 优先级档位（数字越大越优先）；未知 scope ⇒ `None`（脏数据，不参与取数）。
    pub fn priority(&self) -> Option<u8> {
        match self.scope.as_str() {
            SCOPE_COST_OBJECT => Some(3),
            SCOPE_DEPT => Some(2),
            SCOPE_COMPANY => Some(1),
            _ => None,
        }
    }

    /// 这一行的「范围」是否正好是本次取数指向的主体。
    ///
    /// 车间行的 `scope_id` 指到别的车间 ⇒ **不命中**（这是最容易写错、也最贵的错：
    /// 把 A 车间的费率算到 B 车间的头上，成本单价直接错，还没人看得出来）。
    pub fn scope_matches(&self, request: &RateRequest) -> bool {
        match self.scope.as_str() {
            SCOPE_COST_OBJECT => self.scope_id == Some(request.cost_object_id),
            SCOPE_DEPT => self.scope_id == Some(request.dept_id),
            SCOPE_COMPANY => self.scope_id.is_none(),
            _ => false,
        }
    }

    /// 这一行是否参与本次取数（类型 + 启用 + 范围；生效期由 [`RateRow::covers`] 另判）。
    pub fn applies_to(&self, request: &RateRequest) -> bool {
        self.rate_type == request.rate_type && self.is_active() && self.scope_matches(request)
    }
}

/// 取数请求（「给谁、哪一天、要哪种费率」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateRequest {
    /// 费率类型（`LABOR` / `MACHINE`）。
    pub rate_type: String,
    /// 成本对象（对象级费率匹配用）。
    pub cost_object_id: Uuid,
    /// 车间（部门级费率匹配用）。
    pub dept_id: Uuid,
    /// 工作日期（决定用哪个版本）。
    pub work_date: NaiveDate,
}

impl RateRequest {
    /// 工资率取数（工时台账单条 / 导入都用它）。
    pub fn labor(cost_object_id: Uuid, dept_id: Uuid, work_date: NaiveDate) -> Self {
        Self {
            rate_type: RATE_TYPE_LABOR.to_string(),
            cost_object_id,
            dept_id,
            work_date,
        }
    }

    /// 机器费率取数（`MACHINE_HOURS` 分摊基数用；B2-10 接入）。
    pub fn machine(cost_object_id: Uuid, dept_id: Uuid, work_date: NaiveDate) -> Self {
        Self {
            rate_type: RATE_TYPE_MACHINE.to_string(),
            cost_object_id,
            dept_id,
            work_date,
        }
    }
}

/// 报错 / 出参用的人话主体（对象号 + 车间名；两处都有，别让用户去猜 UUID）。
#[derive(Debug, Clone, Copy)]
pub struct RateSubject<'a> {
    /// 成本对象号（`CB-yyyyMMdd-NNNNNN`）。
    pub cost_object_no: &'a str,
    /// 车间（工作中心）名。
    pub dept_name: &'a str,
}

/// 取数结果（**唯一的成功出口**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRate {
    /// 命中的那一行（可追溯）。
    pub rate_id: Uuid,
    /// 费率（元/小时）。
    pub rate: Decimal,
    /// 命中的范围 `COST_OBJECT` / `DEPT` / `COMPANY`（前端要显示「来源」）。
    pub scope: String,
}

impl ResolvedRate {
    /// 范围的中文名（前端字段右侧显示用）。
    pub fn scope_label(&self) -> &'static str {
        scope_label(&self.scope)
    }

    /// 带主体的来源说明，如 `车间（机加工车间）` / `公司（全公司）`。
    pub fn scope_detail(&self, subject: &RateSubject<'_>) -> String {
        match self.scope.as_str() {
            SCOPE_COST_OBJECT => format!("成本对象（{}）", subject.cost_object_no),
            SCOPE_DEPT => format!("车间（{}）", subject.dept_name),
            _ => "公司（全公司）".to_string(),
        }
    }
}

// ==================== 展示小件 ====================

/// 范围的中文名。
pub fn scope_label(scope: &str) -> &'static str {
    match scope {
        SCOPE_COST_OBJECT => "成本对象",
        SCOPE_DEPT => "车间",
        SCOPE_COMPANY => "公司",
        _ => "未知范围",
    }
}

/// 费率类型的中文名（报文里的「工资率」/「机器费率」）。
pub fn rate_type_label(rate_type: &str) -> &'static str {
    match rate_type {
        RATE_TYPE_LABOR => "工资率",
        RATE_TYPE_MACHINE => "机器费率",
        _ => "费率",
    }
}

/// 三级查找的中文顺序说明（报错 / 提示共用一句话，避免两处口径不一致）。
pub fn lookup_chain() -> &'static str {
    "成本对象 → 车间 → 公司"
}

// ==================== 取数（纯函数，全部逻辑都在这里） ====================

/// 从候选行里按「对象 > 车间 > 公司 + 同级取最新生效版本」挑一条。
///
/// # 参数
///
/// - `rows`：`biz_cost_rate` 里**同类型、未软删**的行（停用 / 过期 / 别的车间都允许
///   混在里面：本函数负责筛，这样才能在报错里说清「有几条因为什么没被采用」）。
/// - `request`：给谁、哪一天、要哪种费率。
/// - `subject`：报错文案里的成本对象号 / 车间名。
///
/// # 返回值
///
/// 命中 ⇒ [`ResolvedRate`]（含费率与来源范围）。
///
/// # 异常情况
///
/// 三级全空 ⇒ `422` + [`RATE_MISSING_CODE`]（`66059`），报文点名对象、车间、
/// 日期、已查级别、以及**怎么修**；**绝不返回 0**。
pub fn resolve(
    rows: &[RateRow],
    request: &RateRequest,
    subject: &RateSubject<'_>,
) -> std::result::Result<ResolvedRate, ApiError> {
    let mut best: Option<&RateRow> = None;
    for row in rows {
        if !row.applies_to(request) || !row.covers(request.work_date) {
            continue;
        }
        if is_better(row, best) {
            best = Some(row);
        }
    }
    match best {
        Some(row) => Ok(ResolvedRate {
            rate_id: row.id,
            rate: row.rate,
            scope: row.scope.clone(),
        }),
        None => Err(missing_rate(rows, request, subject)),
    }
}

/// `candidate` 是否应当取代 `current`（无 `current` 时恒真）。
///
/// 比较顺序：优先级档位 → 生效开始日更晚（版本更新）→ id 更大（唯一索引已挡住
/// 同 `(type, scope, scope_id, effective_from)` 的组合，这一级纯属兜底的确定性）。
fn is_better(candidate: &RateRow, current: Option<&RateRow>) -> bool {
    let Some(current) = current else {
        return true;
    };
    let (new_rank, old_rank) = (candidate.priority().unwrap_or(0), current.priority().unwrap_or(0));
    new_rank > old_rank
        || (new_rank == old_rank
            && (candidate.effective_from > current.effective_from
                || (candidate.effective_from == current.effective_from && candidate.id > current.id)))
}

/// 三级全空时的 `ApiError`（`422` + `66059`，报文带「怎么修」）。
fn missing_rate(rows: &[RateRow], request: &RateRequest, subject: &RateSubject<'_>) -> ApiError {
    let label = rate_type_label(&request.rate_type);
    let date = request.work_date;
    ApiError::new(
        422,
        RATE_MISSING_CODE,
        format!(
            "{label}取数失败：成本对象 {object}（车间 {dept}）在 {date} 没有可用的{label}。\n\
             已按「{chain}」三级查 biz_cost_rate（只认启用中、未删除、生效期覆盖 {date} 的记录），三级均未命中{note}。\n\
             怎么修：在费率配置里登记一条覆盖 {date} 的{label}（元/小时）——\
             优先登记该车间的费率（含社保 / 津贴的综合费率），没有车间差异就登记公司级兜底；\
             已有记录请检查生效期是否覆盖 {date}、状态是否被停用。\n\
             系统不会按 0 计{label}：编造出来的成本比算不出来更危险（设计 D-7 / §4.5.4）",
            object = subject.cost_object_no,
            dept = subject.dept_name,
            chain = lookup_chain(),
            note = skipped_note(rows, request),
        ),
    )
}

/// 「为什么没命中」的补充说明（有几条同类型记录因何被跳过）。
///
/// 没有可解释的行时返回空串（避免报文里出现「另有 0 条…」这种噪音）。
fn skipped_note(rows: &[RateRow], request: &RateRequest) -> String {
    let same_type = rows.iter().filter(|row| row.rate_type == request.rate_type).count();
    if same_type == 0 {
        return String::new();
    }
    format!(
        "（同类型费率共 {same_type} 条，均因为「已停用 / 生效期不覆盖 {date} / 属于其他成本对象或车间」之一而未采用）",
        date = request.work_date
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("测试日期")
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额")
    }

    fn subject() -> RateSubject<'static> {
        RateSubject {
            cost_object_no: "CB-20261101-000001",
            dept_name: "机加工车间",
        }
    }

    /// 造一行费率（`scope_id` 按范围自动给：`COMPANY` 恒空）。
    fn rate(id: u128, scope: &str, scope_id: Option<Uuid>, value: &str, from: &str, to: Option<&str>) -> RateRow {
        RateRow {
            id: Uuid::from_u128(id),
            rate_type: RATE_TYPE_LABOR.to_string(),
            scope: scope.to_string(),
            scope_id: if scope == SCOPE_COMPANY { None } else { scope_id },
            rate: dec(value),
            effective_from: date(from),
            effective_to: to.map(date),
            status: STATUS_ACTIVE,
        }
    }

    /// 固定主体：对象 / 车间各一个 id，另一个车间 / 另一个对象用来验「不串味」。
    struct Ids {
        object: Uuid,
        dept: Uuid,
        other_dept: Uuid,
        other_object: Uuid,
    }

    fn ids() -> Ids {
        Ids {
            object: Uuid::from_u128(0xA1),
            dept: Uuid::from_u128(0xB1),
            other_dept: Uuid::from_u128(0xB2),
            other_object: Uuid::from_u128(0xA2),
        }
    }

    fn request(ids: &Ids) -> RateRequest {
        RateRequest::labor(ids.object, ids.dept, date("2026-11-06"))
    }

    // ---------- 验收项：三种覆盖 ----------

    /// **验收项 ①**：只有公司级费率 ⇒ 用公司级。
    #[test]
    fn company_only_falls_back_to_company() {
        let ids = ids();
        let rows = vec![rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None)];
        let resolved = resolve(&rows, &request(&ids), &subject()).expect("公司级兜底");
        assert_eq!(resolved.rate, dec("40"));
        assert_eq!(resolved.scope, SCOPE_COMPANY);
        assert_eq!(resolved.rate_id, Uuid::from_u128(1));
        assert_eq!(resolved.scope_label(), "公司");
        assert_eq!(resolved.scope_detail(&subject()), "公司（全公司）");
    }

    /// **验收项 ②**：有车间级 ⇒ 车间级压过公司级（即使公司级生效更晚）。
    #[test]
    fn dept_beats_company_regardless_of_effective_date() {
        let ids = ids();
        let rows = vec![
            rate(1, SCOPE_COMPANY, None, "40", "2026-11-01", None),
            rate(2, SCOPE_DEPT, Some(ids.dept), "55", "2026-01-01", None),
        ];
        let resolved = resolve(&rows, &request(&ids), &subject()).expect("车间级");
        assert_eq!(resolved.rate, dec("55"), "车间级优先，与生效期新旧无关");
        assert_eq!(resolved.scope, SCOPE_DEPT);
        assert_eq!(resolved.scope_detail(&subject()), "车间（机加工车间）");
    }

    /// **验收项 ③**：有对象级 ⇒ 对象级压过车间级与公司级。
    #[test]
    fn cost_object_beats_dept_and_company() {
        let ids = ids();
        let rows = vec![
            rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None),
            rate(2, SCOPE_DEPT, Some(ids.dept), "55", "2026-01-01", None),
            rate(3, SCOPE_COST_OBJECT, Some(ids.object), "70", "2026-01-01", None),
        ];
        let resolved = resolve(&rows, &request(&ids), &subject()).expect("对象级");
        assert_eq!(resolved.rate, dec("70"));
        assert_eq!(resolved.scope, SCOPE_COST_OBJECT);
        assert_eq!(resolved.scope_detail(&subject()), "成本对象（CB-20261101-000001）");
    }

    /// **验收项 ④**：三级全空 ⇒ 明确报错（`422` + `66059`），**绝不返回 0**。
    #[test]
    fn missing_rate_is_a_loud_error_never_zero() {
        let ids = ids();
        let error = resolve(&[], &request(&ids), &subject()).expect_err("三级全空必须报错");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, 66059);
        assert_eq!(error.code, RATE_MISSING_CODE);
        // 报文要点名「谁、哪天、查了哪几级、怎么修」
        assert!(error.msg.contains("CB-20261101-000001"), "{}", error.msg);
        assert!(error.msg.contains("机加工车间"), "{}", error.msg);
        assert!(error.msg.contains("2026-11-06"), "{}", error.msg);
        assert!(error.msg.contains("成本对象 → 车间 → 公司"), "{}", error.msg);
        assert!(error.msg.contains("工资率"), "{}", error.msg);
        assert!(error.msg.contains("怎么修"), "{}", error.msg);
        assert!(error.msg.contains("不会按 0"), "{}", error.msg);
        assert!(!error.msg.contains("另有"), "没有同类型行时不许出现噪音：{}", error.msg);
    }

    /// 别的车间 / 别的对象的费率**不许串味**（串了就是成本单价算错还没人发现）。
    #[test]
    fn other_dept_and_other_object_rates_do_not_apply() {
        let ids = ids();
        let rows = vec![
            rate(1, SCOPE_DEPT, Some(ids.other_dept), "55", "2026-01-01", None),
            rate(2, SCOPE_COST_OBJECT, Some(ids.other_object), "70", "2026-01-01", None),
        ];
        let error = resolve(&rows, &request(&ids), &subject()).expect_err("都不是本车间 / 本对象");
        assert_eq!(error.code, 66059);
        assert!(
            error.msg.contains("同类型费率共 2 条"),
            "要说明有几条被跳过：{}",
            error.msg
        );
    }

    // ---------- 生效期 / 状态 ----------

    /// 生效期必须**两端都覆盖**工作日期：未来的、已过期的都不算。
    #[test]
    fn effective_window_must_cover_the_work_date() {
        let ids = ids();
        // 还没生效
        let future = vec![rate(1, SCOPE_COMPANY, None, "40", "2026-12-01", None)];
        assert_eq!(resolve(&future, &request(&ids), &subject()).unwrap_err().code, 66059);
        // 已过期
        let expired = vec![rate(2, SCOPE_COMPANY, None, "40", "2026-01-01", Some("2026-10-31"))];
        assert_eq!(resolve(&expired, &request(&ids), &subject()).unwrap_err().code, 66059);
        // 恰好覆盖（effective_to == work_date 当天仍有效）
        let boundary = vec![rate(3, SCOPE_COMPANY, None, "40", "2026-11-06", Some("2026-11-06"))];
        assert_eq!(
            resolve(&boundary, &request(&ids), &subject()).expect("当天有效").rate,
            dec("40")
        );
        // 恰好覆盖（effective_from == work_date 当天开始生效）
        let boundary = vec![rate(4, SCOPE_COMPANY, None, "41", "2026-11-06", None)];
        assert_eq!(
            resolve(&boundary, &request(&ids), &subject()).expect("当天生效").rate,
            dec("41")
        );
    }

    /// 停用的行永不参与取数（`status = 0`）。
    #[test]
    fn disabled_rate_is_ignored() {
        let ids = ids();
        let mut row = rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None);
        row.status = 0;
        let error = resolve(&[row], &request(&ids), &subject()).expect_err("停用不参与");
        assert_eq!(error.code, 66059);
    }

    /// 类型不串：机器费率不会被当成工资率。
    #[test]
    fn rate_type_must_match() {
        let ids = ids();
        let mut row = rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None);
        row.rate_type = RATE_TYPE_MACHINE.to_string();
        assert_eq!(resolve(&[row], &request(&ids), &subject()).unwrap_err().code, 66059);
    }

    // ---------- 版本化 ----------

    /// 同一级里**生效更晚的版本赢**（费率涨了就用新费率）。
    #[test]
    fn newer_version_wins_within_the_same_scope() {
        let ids = ids();
        let rows = vec![
            rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None),
            rate(2, SCOPE_COMPANY, None, "50", "2026-11-01", None),
            rate(3, SCOPE_COMPANY, None, "45", "2026-06-01", Some("2026-10-31")),
        ];
        let resolved = resolve(&rows, &request(&ids), &subject()).expect("最新版本");
        assert_eq!(resolved.rate, dec("50"), "11 月的工时用 11-01 起生效的 50");
        assert_eq!(resolved.rate_id, Uuid::from_u128(2));
    }

    /// 同一级、同一生效期（唯一索引挡住的极端情形）：按 id 兜底，结果**稳定**。
    #[test]
    fn tie_break_is_deterministic() {
        let ids = ids();
        let rows = vec![
            rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None),
            rate(2, SCOPE_COMPANY, None, "41", "2026-01-01", None),
        ];
        let first = resolve(&rows, &request(&ids), &subject()).expect("命中");
        let second = resolve(&rows, &request(&ids), &subject()).expect("命中");
        assert_eq!(first, second, "同样输入必须同样输出");
        assert_eq!(first.rate_id, Uuid::from_u128(2));
    }

    /// 脏 scope（值域外的）不参与取数，也不 panic。
    #[test]
    fn unknown_scope_is_skipped() {
        let ids = ids();
        let mut row = rate(1, SCOPE_DEPT, Some(ids.dept), "55", "2026-01-01", None);
        row.scope = "WORKSHOP".to_string();
        assert_eq!(resolve(&[row], &request(&ids), &subject()).unwrap_err().code, 66059);
    }

    /// `COMPANY` 行被塞了 `scope_id`（弱引用写错）：不当作公司级命中。
    #[test]
    fn company_row_with_scope_id_is_not_a_match() {
        let ids = ids();
        let mut row = rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None);
        row.scope_id = Some(ids.dept);
        assert_eq!(resolve(&[row], &request(&ids), &subject()).unwrap_err().code, 66059);
    }

    /// 0 元费率是**合法**配置（DDL 只要求 `rate >= 0`），与「取不到」是两件事。
    #[test]
    fn zero_rate_is_a_legit_value_not_a_miss() {
        let ids = ids();
        let rows = vec![rate(1, SCOPE_COMPANY, None, "0", "2026-01-01", None)];
        let resolved = resolve(&rows, &request(&ids), &subject()).expect("0 是合法值");
        assert_eq!(resolved.rate, Decimal::ZERO);
    }

    /// 机器费率取数走同一条链路（B2-10 会用）。
    #[test]
    fn machine_request_uses_machine_rates() {
        let ids = ids();
        let mut machine = rate(2, SCOPE_DEPT, Some(ids.dept), "18.5", "2026-01-01", None);
        machine.rate_type = RATE_TYPE_MACHINE.to_string();
        let rows = vec![rate(1, SCOPE_COMPANY, None, "40", "2026-01-01", None), machine];
        let resolved = resolve(
            &rows,
            &RateRequest::machine(ids.object, ids.dept, date("2026-11-06")),
            &subject(),
        )
        .expect("机器费率");
        assert_eq!(resolved.rate, dec("18.5"));
        assert_eq!(resolved.scope, SCOPE_DEPT);
    }

    /// 工具函数口径（前端显示 / 报文共用）。
    #[test]
    fn labels_are_stable() {
        assert_eq!(rate_type_label(RATE_TYPE_LABOR), "工资率");
        assert_eq!(rate_type_label(RATE_TYPE_MACHINE), "机器费率");
        assert_eq!(scope_label(SCOPE_COMPANY), "公司");
        assert_eq!(scope_label(SCOPE_DEPT), "车间");
        assert_eq!(scope_label(SCOPE_COST_OBJECT), "成本对象");
        assert_eq!(lookup_chain(), "成本对象 → 车间 → 公司");
        assert_eq!(RATE_TYPES.len(), 2);
        assert_eq!(SCOPES.len(), 3);
    }
}
