//! 员工档案导入（`hr.employee`）。
//!
//! 设计定稿：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #11（员工导入）/ §6.2.1。
//! 框架是 `business::impexp`（模板生成 / 结构校验 / 逐行校验 / 错误行 xlsx 全部由它给），
//! 本文件只写**员工档案特有的三件事**：列契约、跨行/跨库的业务规则、入库 SQL。
//!
//! # 列契约（模板列 = 卡面 A..AA 27 列 + 末尾追加的「工号」列）
//!
//! | # | 列键 | 表头 | 必填 | 类型 | 落库列 |
//! |:--:|---|---|:--:|---|---|
//! | 1 | `name` | 姓名 | ✅ | 文本 ≤50 | `name` |
//! | 2 | `gender` | 性别 | ✅ | 枚举 男/女 | `gender`（1/2） |
//! | 3 | `id_card_type` | 证件类型 | ✅ | 枚举 | `id_card_type` |
//! | 4 | `id_card` | 身份证号 | — | 文本 ≤18 | `id_card` |
//! | 5 | `mobile` | 手机号 | — | 文本 ≤20 | `mobile` |
//! | 6 | `email` | 邮箱 | — | 文本 ≤100 | `email` |
//! | 7 | `dept_name` | 部门名称 | ✅ | 文本 ≤100 | 查 `sys_dept` → `dept_id` |
//! | 8 | `position_code` | 岗位编码 | — | 文本 ≤32 | `position_code` |
//! | 9 | `job_level` | 职级 | — | 文本 ≤20 | `job_level` |
//! | 10 | `employment_type` | 用工类型 | ✅ | 枚举 | `employment_type` |
//! | 11 | `hire_date` | 入职日期 | ✅ | 日期 | `hire_date` |
//! | 12 | `probation_end_date` | 试用期结束 | — | 日期 | `probation_end_date` |
//! | 13 | `birth_date` | 生日 | — | 日期 | `birth_date` |
//! | 14 | `education` | 学历 | — | 枚举 | `education` |
//! | 15 | `school` | 学校 | — | 文本 ≤100 | `school` |
//! | 16 | `major` | 专业 | — | 文本 ≤100 | `major` |
//! | 17 | `graduate_date` | 毕业日期 | — | 日期 | `graduate_date` |
//! | 18 | `native_place` | 籍贯 | — | 文本 ≤100 | `native_place` |
//! | 19 | `address` | 地址 | — | 文本 ≤200 | `address` |
//! | 20 | `emergency_contact` | 紧急联系人 | — | 文本 ≤50 | `emergency_contact` |
//! | 21 | `emergency_phone` | 紧急联系电话 | — | 文本 ≤20 | `emergency_phone` |
//! | 22 | `emergency_relation` | 与本人关系 | — | 文本 ≤20 | `emergency_relation` |
//! | 23 | `bank_name` | 开户行 | — | 文本 ≤100 | `bank_name` |
//! | 24 | `bank_branch` | 开户支行 | — | 文本 ≤200 | `bank_branch` |
//! | 25 | `bank_account` | 银行账号 | — | 文本 ≤40 | `bank_account` |
//! | 26 | `bank_account_name` | 账户名 | — | 文本 ≤50 | `bank_account_name` |
//! | 27 | `remark` | 备注 | — | 文本 ≤500 | `remark` |
//! | 28 | `emp_no` | 工号 | — | 文本 ≤32 | `emp_no`（留空 ⇒ 自动取号） |
//!
//! ## 为什么第 28 列（工号）在末尾，而不是插在姓名前面
//!
//! 卡面把 A..AA 27 列逐字定死（模板列顺序一经发布不可改），所以前 27 列**一列不动**。
//! 但「工号留空 ⇒ 自动取号」（卡面关键规则）要求模板里真的有一个**可以填**的工号列 ——
//! 否则「若填」永远不成立，而且企业已有工号体系（`employee_no.rs` 明确支持 `E2003` 这类
//! 非 `EMP-` 形态的既有工号）在批量建档时会被强制换成新号，历史单据就对不上了。
//! 卡面自己的注意事项允许「追加新列到末尾」，故工号落在第 28 列（AB）。
//!
//! # 服务端算的列（模板里不出现）
//!
//! | 列 | 取值 | 为什么不让用户填 |
//! |---|---|---|
//! | `id` / `org_id` | `gen_random_uuid()` / [`SENTINEL_ORG_ID`] | 主键与组织维度不是业务数据 |
//! | `dept_id` | 按 `dept_name` 查 `sys_dept`（**填名字，不填 UUID**） | 让用户填 UUID 等于让人手工拼主键 |
//! | `status` | 用工类型推导（试用 / 实习 ⇒ `PROBATION`，其余 ⇒ `ACTIVE`） | 导入只建**在职**档案；离职走离职单（状态机有边表），让导入能写 `RESIGNED` 等于绕过离职流程 |
//! | `leave_date` / `leave_type` | 一律 NULL | 同上（`ck_biz_employee_leave` 双向约束） |
//! | `user_id` | 不写 | 绑定登录账号是独立动作（`uk_biz_employee_user`），导入不猜 |
//!
//! # 四类校验，各在哪一层
//!
//! | 类 | 例子 | 谁做 | 用户看到 |
//! |---|---|---|---|
//! | 结构 / 单元格 | 缺列、性别写了「男男」、日期写 `2026-13-01` | 框架（[`ColumnKind::parse`]） | 上传即报，带 Excel 行号 |
//! | 单行业务规则 | 入职日期在未来、生日不满 16 岁、身份证校验位错 | [`EmployeeImport::validate_row`]（**纯计算，不碰库**） | 同上 |
//! | 跨行 / 跨库规则 | 部门名不存在 / 重名、文件内手机号重复、工号已被占用 | [`EmployeeImport::validate_batch`]（整批一条 SQL + 一次部门字典装载） | 上传即报，带行号 + 原因 |
//! | 幂等 | 身份证 / 手机号与**库里已有**的员工相同 | [`EmployeeImport::persist`] | **不报错**，跳过并在结果里报「已跳过 N 行（重复）」 |
//!
//! 前三类在**上传预检**阶段就出结果（用户先看到「2 行可导入 / 1 行错误」，改完再传），
//! 第四类是幂等语义（同一份花名册导第二遍不产生第二个人），因此**不算错误**。
//!
//! # 部门按「名称」匹配（用户不会填 UUID）
//!
//! `dept_name` 支持两种写法：
//!
//! - **全名**：`生产部` —— 全库唯一时直接命中；
//! - **路径**：`总部/生产部/一车间`（分隔符 `/`、`>`、`／` 都认）—— 同名部门挂在
//!   不同父部门下时用它区分。
//!
//! 命中 0 个 → 「部门不存在」；命中多个且没写路径 → 报错并**列出候选路径**（不猜）；
//! 命中的部门 `status = 0`（停用）→ 拒绝（停用部门不能再进人）。
//!
//! # 幂等与重复策略
//!
//! `uk_biz_employee_idcard (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL`
//! 是库层唯一键：
//!
//! - `duplicate_strategy = SKIP`（页面默认）→ `ON CONFLICT … DO NOTHING`，重复行**跳过**；
//! - `duplicate_strategy = UPSERT` → `ON CONFLICT … DO UPDATE`，只覆盖**人事可变列**
//!   （姓名 / 部门 / 岗位 / 联系方式 / 学历 / 银行 / 紧急联系人……），
//!   且 `WHERE biz_employee.status <> 'RESIGNED'` —— **已离职档案不许被一次导入「复活」**；
//!   工号 / 状态 / 离职日期 / `user_id` 永不被覆盖。
//!
//! 手机号没有库层唯一键（一家人可能共用一个号，强行加唯一键会误伤），所以「手机号重复」
//! 在 [`EmployeeImport::persist`] 里用一条 SQL 预筛，命中即跳过（与身份证同口径）。
//!
//! # 为什么 `batch_size == max_rows`（一份文件一个事务）
//!
//! 与 `hr.attendance` 同一取舍：`STRICT` 的语义是「任一行错就整体不入库」，而框架按批开
//! 事务。若一批只装 200 行，第 2 批失败时第 1 批已提交，界面会显示「前 200 行已入库」——
//! 花名册出现「一半人有档案、一半没有」，而工资试算按在职人数算，缺人会静默少发。
//! 一份文件落在一个事务里，`STRICT` 才真的是全有或全无（1000 行以内，单事务毫无压力）。

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{Datelike, Local, NaiveDate};
use nik::prelude::*;
use uuid::Uuid;

use crate::business::hr::employee_no;
use crate::business::hr::model::SENTINEL_ORG_ID;
use crate::business::impexp::definition::{
    ColumnKind, DuplicateStrategy, EnumItem, ErrorKind, FieldIssue, ImportColumn, ImportCtx, ImportDefinition,
    RowIssue, RowValues,
};

/// 员工档案导入的业务编码（`sys_import_task.biz_type`）。
pub const EMPLOYEE_BIZ_TYPE: &str = "hr.employee";

/// 单次导入行数上限（卡面：1000 行以内，超过提示分批）。
pub const MAX_ROWS: usize = 1_000;

/// 分批大小 = 行数上限 ⇒ 一份文件一个事务（理由见模块文档末尾）。
pub const BATCH_SIZE: usize = MAX_ROWS;

// ==================== 列键 ====================

const KEY_NAME: &str = "name";
const KEY_GENDER: &str = "gender";
const KEY_ID_CARD_TYPE: &str = "id_card_type";
const KEY_ID_CARD: &str = "id_card";
const KEY_MOBILE: &str = "mobile";
const KEY_EMAIL: &str = "email";
const KEY_DEPT_NAME: &str = "dept_name";
const KEY_POSITION_CODE: &str = "position_code";
const KEY_JOB_LEVEL: &str = "job_level";
const KEY_EMPLOYMENT_TYPE: &str = "employment_type";
const KEY_HIRE_DATE: &str = "hire_date";
const KEY_PROBATION_END: &str = "probation_end_date";
const KEY_BIRTH_DATE: &str = "birth_date";
const KEY_EDUCATION: &str = "education";
const KEY_SCHOOL: &str = "school";
const KEY_MAJOR: &str = "major";
const KEY_GRADUATE_DATE: &str = "graduate_date";
const KEY_NATIVE_PLACE: &str = "native_place";
const KEY_ADDRESS: &str = "address";
const KEY_EMERGENCY_CONTACT: &str = "emergency_contact";
const KEY_EMERGENCY_PHONE: &str = "emergency_phone";
const KEY_EMERGENCY_RELATION: &str = "emergency_relation";
const KEY_BANK_NAME: &str = "bank_name";
const KEY_BANK_BRANCH: &str = "bank_branch";
const KEY_BANK_ACCOUNT: &str = "bank_account";
const KEY_BANK_ACCOUNT_NAME: &str = "bank_account_name";
const KEY_REMARK: &str = "remark";
const KEY_EMP_NO: &str = "emp_no";

// ==================== 枚举（中文值 ↔ 落库码，与 V0383 的 CHECK 逐字一致） ====================

/// 性别（`ck_biz_employee_gender`：0 未知 / 1 男 / 2 女）。
///
/// 「未知」不进模板：卡面要求性别必填，能填「未知」等于允许不填。
static GENDER_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "男", code: "1"
    },
    EnumItem {
        label: "女", code: "2"
    },
];

/// 证件类型（`ck_biz_employee_idtype` 的 4 个码）。
///
/// 卡面写「身份证（默认）/ 护照 / 其他」；库里的「其他」细分成了 `HK_MO_TW` 与
/// `FOREIGN` 两码（港澳台通行证 / 外国护照），模板按库的 4 码给，避免用户填「其他」
/// 后系统猜不到该落哪个码。
static ID_CARD_TYPE_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "身份证",
        code: "ID_CARD",
    },
    EnumItem {
        label: "护照",
        code: "PASSPORT",
    },
    EnumItem {
        label: "港澳台通行证",
        code: "HK_MO_TW",
    },
    EnumItem {
        label: "外国护照",
        code: "FOREIGN",
    },
];

/// 用工类型（`ck_biz_employee_emptype` 的 6 个码）。
///
/// 卡面写 4 种（正式 / 试用 / 实习 / 劳务）；库里另有 `PART_TIME`（兼职）与
/// `DISPATCH`（劳务派遣）。模板给全 6 种：少给两种等于「这类员工永远进不了系统」，
/// 而它们都是合法的库值。
static EMPLOYMENT_TYPE_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "正式",
        code: "FULL_TIME",
    },
    EnumItem {
        label: "试用",
        code: "PROBATION",
    },
    EnumItem {
        label: "实习",
        code: "INTERN",
    },
    EnumItem {
        label: "劳务",
        code: "OUTSOURCE",
    },
    EnumItem {
        label: "兼职",
        code: "PART_TIME",
    },
    EnumItem {
        label: "派遣",
        code: "DISPATCH",
    },
];

/// 学历（`sys_dict_item` 的 `hr_education` 7 项，V0386 种子逐字）。
///
/// 用枚举而不是字典列：卡面要求「学历」在模板里是**下拉**，而字典列的取值是运行时数据，
/// 生成模板时拿不到（框架 `template::data_validations` 对字典列不加验证）。
/// 代价是字典新增学历时要同步这里 —— 已在 `dict_hr_education_matches_v0386` 用例里
/// 把 7 项钉住，改字典不改这里会被用例拦下（见 `salary_item_seed_tests` 的同款做法）。
static EDUCATION_ITEMS: &[EnumItem] = &[
    EnumItem {
        label: "小学",
        code: "PRIMARY",
    },
    EnumItem {
        label: "初中",
        code: "JUNIOR",
    },
    EnumItem {
        label: "高中",
        code: "SENIOR",
    },
    EnumItem {
        label: "大专",
        code: "COLLEGE",
    },
    EnumItem {
        label: "本科",
        code: "BACHELOR",
    },
    EnumItem {
        label: "硕士",
        code: "MASTER",
    },
    EnumItem {
        label: "博士",
        code: "DOCTOR",
    },
];

// ==================== 列定义（顺序即模板列顺序） ====================

static COLUMNS: &[ImportColumn] = &[
    ImportColumn {
        key: KEY_NAME,
        header: "姓名",
        required: true,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "张三",
        width: Some(12),
        remark: "必填，≤50 字；与身份证一起决定是不是同一个人",
    },
    ImportColumn {
        key: KEY_GENDER,
        header: "性别",
        required: true,
        kind: ColumnKind::Enum(GENDER_ITEMS),
        unique_key: false,
        fk: None,
        example: "男",
        width: Some(8),
        remark: "必填，只能选「男」或「女」（下拉）",
    },
    ImportColumn {
        key: KEY_ID_CARD_TYPE,
        header: "证件类型",
        required: true,
        kind: ColumnKind::Enum(ID_CARD_TYPE_ITEMS),
        unique_key: false,
        fk: None,
        example: "身份证",
        width: Some(12),
        remark: "必填，下拉四选一；没有证件的员工请先在档案里手工建档",
    },
    ImportColumn {
        key: KEY_ID_CARD,
        header: "身份证号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(18) },
        unique_key: true,
        fk: None,
        example: "440203199005201235",
        width: Some(20),
        remark: "可空；18 位（末位可以是 X）或 15 位老证。填了就按它判重（库内唯一），校验位错的会被拦下",
    },
    ImportColumn {
        key: KEY_MOBILE,
        header: "手机号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: None,
        example: "13800138000",
        width: Some(14),
        remark: "可空；数字（可带 +86 / 空格 / 连字符）。与库里已有员工相同时该行按「重复」跳过",
    },
    ImportColumn {
        key: KEY_EMAIL,
        header: "邮箱",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "zhangsan@example.com",
        width: Some(24),
        remark: "可空；必须含 @ 与域名点号",
    },
    ImportColumn {
        key: KEY_DEPT_NAME,
        header: "部门名称",
        required: true,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "生产部",
        width: Some(16),
        remark: "必填，**填部门名称不是编号**；同名部门挂在不同的父部门下时写路径，如「总部/生产部/一车间」",
    },
    ImportColumn {
        key: KEY_POSITION_CODE,
        header: "岗位编码",
        required: false,
        kind: ColumnKind::Text { max_len: Some(32) },
        unique_key: false,
        fk: None,
        example: "WORKER",
        width: Some(14),
        remark: "可空；字典 hr_position 的码（如 WORKER / ENGINEER / MANAGER），也接受自定义岗位编码",
    },
    ImportColumn {
        key: KEY_JOB_LEVEL,
        header: "职级",
        required: false,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: None,
        example: "P5",
        width: Some(10),
        remark: "可空；字典 hr_job_level 的取值（P1 / P5 / P8 / M1 / M4）",
    },
    ImportColumn {
        key: KEY_EMPLOYMENT_TYPE,
        header: "用工类型",
        required: true,
        kind: ColumnKind::Enum(EMPLOYMENT_TYPE_ITEMS),
        unique_key: false,
        fk: None,
        example: "正式",
        width: Some(12),
        remark: "必填，下拉；试用 / 实习 ⇒ 档案状态「试用期」，其余 ⇒「在职」",
    },
    ImportColumn {
        key: KEY_HIRE_DATE,
        header: "入职日期",
        required: true,
        kind: ColumnKind::Date,
        unique_key: false,
        fk: None,
        example: "2026-01-15",
        width: Some(14),
        remark: "必填，YYYY-MM-DD（也认 2026/1/15 与 Excel 日期格式）；不能晚于今天",
    },
    ImportColumn {
        key: KEY_PROBATION_END,
        header: "试用期结束",
        required: false,
        kind: ColumnKind::Date,
        unique_key: false,
        fk: None,
        example: "2026-04-15",
        width: Some(14),
        remark: "可空，YYYY-MM-DD；不能早于入职日期（转正提醒依据）",
    },
    ImportColumn {
        key: KEY_BIRTH_DATE,
        header: "生日",
        required: false,
        kind: ColumnKind::Date,
        unique_key: false,
        fk: None,
        example: "1990-05-20",
        width: Some(14),
        remark: "可空，YYYY-MM-DD；必须比入职日期早 16 年以上",
    },
    ImportColumn {
        key: KEY_EDUCATION,
        header: "学历",
        required: false,
        kind: ColumnKind::Enum(EDUCATION_ITEMS),
        unique_key: false,
        fk: None,
        example: "本科",
        width: Some(10),
        remark: "可空，下拉七选一（小学 / 初中 / 高中 / 大专 / 本科 / 硕士 / 博士）",
    },
    ImportColumn {
        key: KEY_SCHOOL,
        header: "学校",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "某某大学",
        width: Some(20),
        remark: "可空，≤100 字",
    },
    ImportColumn {
        key: KEY_MAJOR,
        header: "专业",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "机械工程",
        width: Some(18),
        remark: "可空，≤100 字",
    },
    ImportColumn {
        key: KEY_GRADUATE_DATE,
        header: "毕业日期",
        required: false,
        kind: ColumnKind::Date,
        unique_key: false,
        fk: None,
        example: "2012-06-30",
        width: Some(14),
        remark: "可空，YYYY-MM-DD；不能晚于今天",
    },
    ImportColumn {
        key: KEY_NATIVE_PLACE,
        header: "籍贯",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "广东韶关",
        width: Some(16),
        remark: "可空，≤100 字",
    },
    ImportColumn {
        key: KEY_ADDRESS,
        header: "地址",
        required: false,
        kind: ColumnKind::Text { max_len: Some(200) },
        unique_key: false,
        fk: None,
        example: "广东省韶关市武江区某某路 1 号",
        width: Some(30),
        remark: "可空，≤200 字（现住址）",
    },
    ImportColumn {
        key: KEY_EMERGENCY_CONTACT,
        header: "紧急联系人",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "李四",
        width: Some(14),
        remark: "可空，≤50 字",
    },
    ImportColumn {
        key: KEY_EMERGENCY_PHONE,
        header: "紧急联系电话",
        required: false,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: None,
        example: "13900139000",
        width: Some(16),
        remark: "可空；数字（可带 +86 / 空格 / 连字符）",
    },
    ImportColumn {
        key: KEY_EMERGENCY_RELATION,
        header: "与本人关系",
        required: false,
        kind: ColumnKind::Text { max_len: Some(20) },
        unique_key: false,
        fk: None,
        example: "配偶",
        width: Some(14),
        remark: "可空，≤20 字（配偶 / 父 / 母 / 子 / 女 …）",
    },
    ImportColumn {
        key: KEY_BANK_NAME,
        header: "开户行",
        required: false,
        kind: ColumnKind::Text { max_len: Some(100) },
        unique_key: false,
        fk: None,
        example: "中国工商银行",
        width: Some(20),
        remark: "可空，≤100 字（工资卡开户行）",
    },
    ImportColumn {
        key: KEY_BANK_BRANCH,
        header: "开户支行",
        required: false,
        kind: ColumnKind::Text { max_len: Some(200) },
        unique_key: false,
        fk: None,
        example: "韶关分行武江支行",
        width: Some(24),
        remark: "可空，≤200 字",
    },
    ImportColumn {
        key: KEY_BANK_ACCOUNT,
        header: "银行账号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(40) },
        unique_key: false,
        fk: None,
        example: "6222021234567890123",
        width: Some(24),
        remark: "可空；8–40 位数字（空格 / 连字符会被忽略）。出参按脱敏规则打码",
    },
    ImportColumn {
        key: KEY_BANK_ACCOUNT_NAME,
        header: "账户名",
        required: false,
        kind: ColumnKind::Text { max_len: Some(50) },
        unique_key: false,
        fk: None,
        example: "张三",
        width: Some(14),
        remark: "可空，≤50 字；一般与姓名一致",
    },
    ImportColumn {
        key: KEY_REMARK,
        header: "备注",
        required: false,
        kind: ColumnKind::Text { max_len: Some(500) },
        unique_key: false,
        fk: None,
        example: "内部调岗入职",
        width: Some(24),
        remark: "可空，≤500 字",
    },
    // 第 28 列：卡面「工号留空 ⇒ 自动取号」要求模板里真有一个可填的工号列（见模块文档）。
    ImportColumn {
        key: KEY_EMP_NO,
        header: "工号",
        required: false,
        kind: ColumnKind::Text { max_len: Some(32) },
        unique_key: false,
        fk: None,
        example: "EMP-0001",
        width: Some(14),
        remark: "可空；**留空自动取号**（EMP-0001 递增）。企业已有工号体系请照原样填（如 E2003），不能与已有档案重复",
    },
];

// ==================== 小工具 ====================

/// 文本列：空串 → `None`。
fn text_of(row: &RowValues, key: &str) -> Option<String> {
    let value = row.get(key).trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 按 key 取列定义（给规则错误挂列名）。
fn column_of(key: &str) -> Option<&'static ImportColumn> {
    COLUMNS.iter().find(|column| column.key == key)
}

/// 构造一条带列名与原始值的业务规则错误。
fn column_issue(row: &RowValues, key: &str, kind: ErrorKind, message: impl Into<String>) -> Option<RowIssue> {
    let column = column_of(key)?;
    Some(RowIssue::column(
        row.row_no,
        column,
        row.get(column.key),
        FieldIssue {
            kind,
            message: message.into(),
        },
    ))
}

/// 单元格里的日期（已由 `ColumnKind::Date` 规范化成 `YYYY-MM-DD`）；空 / 非法 → `None`。
fn date_of(row: &RowValues, key: &str) -> Option<NaiveDate> {
    let text = row.get(key).trim();
    if text.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// 业务日（`Asia/Shanghai`，即 UTC+8）。
///
/// 不用 `Local::now()` 的机器时区：容器里常年是 UTC，「今天」会差 8 小时 —— 早上 7 点
/// 导入一份今天入职的花名册，UTC 判日期还停在前一天，会被「入职日期不能晚于今天」
/// 误挡。与 `employee_model` 里合同到期天数的口径一致（按 `+08:00` 业务日）。
fn today_cst() -> NaiveDate {
    let now = Local::now();
    (now + chrono::Duration::hours(8)).date_naive()
}

/// 是否早于「`hire` 往前 `years` 年」的日期边界（用于「入职时须满 N 岁」）。
fn is_older_than(date: NaiveDate, reference: NaiveDate, years: i32) -> bool {
    let Some(boundary) = reference.with_year(reference.year() - years) else {
        return false;
    };
    date <= boundary
}

/// 手机 / 电话类字段的规范化：去掉空格、连字符与 `+86` 前缀。
fn normalize_phone(raw: &str) -> String {
    let cleaned = raw
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-' && *ch != '(' && *ch != ')')
        .collect::<String>();
    cleaned.strip_prefix("+86").unwrap_or(&cleaned).to_string()
}

/// 电话号码是否合法（7–15 位数字；拦「写成了邮箱 / 汉字」这类明显错误）。
///
/// 不强制 11 位手机号：固话、分机、境外号码都可能是真实值，卡面也没要求按运营商号段校验。
fn phone_valid(raw: &str) -> bool {
    let digits = normalize_phone(raw);
    (7..=15).contains(&digits.chars().count()) && digits.chars().all(|ch| ch.is_ascii_digit())
}

/// 银行账号是否合法（去空格 / 连字符后 8–40 位数字）。
fn bank_account_valid(raw: &str) -> bool {
    let digits = normalize_phone(raw);
    (8..=40).contains(&digits.chars().count()) && digits.chars().all(|ch| ch.is_ascii_digit())
}

/// 邮箱是否合法（含 `@`，且域名部分带点）。
fn email_valid(raw: &str) -> bool {
    let text = raw.trim();
    match text.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
        }
        None => false,
    }
}

/// 身份证号是否合法（18 位含 ISO 7064 MOD 11-2 校验位，或 15 位老证全数字）。
///
/// 15 位老证是纯数字（无校验位），18 位末位可以是 `X`。
pub fn id_card_valid(raw: &str) -> bool {
    let text = raw.trim();
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() == 15 {
        return chars.iter().all(char::is_ascii_digit);
    }
    if chars.len() != 18 {
        return false;
    }
    let (body, check) = chars.split_at(17);
    if !body.iter().all(char::is_ascii_digit) {
        return false;
    }
    let expected = id_card_check_char(body);
    match expected {
        Some(expected) => check[0].to_ascii_uppercase() == expected,
        None => false,
    }
}

/// 按前 17 位算 ISO 7064 MOD 11-2 校验位。
fn id_card_check_char(body: &[char]) -> Option<char> {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const TABLE: [char; 11] = ['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let mut sum = 0u32;
    for (index, ch) in body.iter().enumerate() {
        let digit = ch.to_digit(10)?;
        sum += digit * WEIGHTS[index];
    }
    TABLE.get((sum % 11) as usize).copied()
}

/// 用工类型 → 建档时的档案状态。
///
/// 试用 / 实习 ⇒ `PROBATION`（试用期），其余 ⇒ `ACTIVE`（在职）。
/// 导入**只建在职档案**：`SUSPENDED` / `RESIGNED` 必须走状态机 / 离职单（有边表与审批），
/// 让导入能写这两个状态等于绕开流程。
fn status_of(employment_type: &str) -> &'static str {
    match employment_type {
        "PROBATION" | "INTERN" => "PROBATION",
        _ => "ACTIVE",
    }
}

/// 读本任务生效的重复数据策略（`options.duplicate_strategy`，默认 `UPSERT`）。
async fn duplicate_strategy(ctx: &ImportCtx) -> DbResult<DuplicateStrategy> {
    let raw = crate::business::impexp::model::import_task_options(&ctx.task_id).await?;
    let parsed = raw
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| {
            value
                .get("duplicate_strategy")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        })
        .and_then(|text| DuplicateStrategy::parse(&text));
    Ok(parsed.unwrap_or_default())
}

// ==================== 部门字典（名称 / 路径 → dept_id） ====================

/// 部门树的一次性快照（装载全部未删除部门，`sys_dept` 是小表；**不许逐行查库**）。
#[derive(Default)]
struct DeptIndex {
    /// `id` → (名称, 父 id, 是否启用)。
    nodes: HashMap<Uuid, (String, Option<Uuid>, bool)>,
    /// 规范化路径 → id（同名部门靠它区分）。
    by_path: HashMap<String, Uuid>,
    /// 名称 → `(id, 完整路径)`（同名时给候选）。
    by_name: HashMap<String, Vec<(Uuid, String)>>,
}

/// 部门解析失败的原因（`Display` 文案直接进错误明细）。
enum DeptError {
    /// 库里没有这个部门。
    NotFound(String),
    /// 同名部门挂在多个父部门下，必须写路径。
    Ambiguous(String, Vec<String>),
    /// 命中的部门已停用。
    Disabled(String, String),
}

impl std::fmt::Display for DeptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeptError::NotFound(name) => write!(
                formatter,
                "部门「{name}」在组织架构里不存在（请填部门名称；同名部门多时写路径，如「总部/生产部」）"
            ),
            DeptError::Ambiguous(name, candidates) => write!(
                formatter,
                "部门「{name}」对应 {} 个部门，请改填路径：{}",
                candidates.len(),
                candidates.join("、")
            ),
            DeptError::Disabled(_name, path) => write!(
                formatter,
                "部门「{path}」已停用，不能再给新员工建档（请先在组织架构里启用，或换一个部门）"
            ),
        }
    }
}

/// 部门装载 SQL：一次取全部未删除部门（`sys_dept` 没有 `org_id`，是系统级组织架构）。
// SCOPE-EXEMPT: 员工导入的部门索引 —— 一次性取全部未删除部门，把模板里的「部门名称 / 部门路径」
// 解析成 dept_id（导入是管理员批量作业，入口权限 business:hr:employee:import）。部门是系统级组织架构
// （sys_dept 无 org_id、也不是按部门可见的业务数据），按数据范围裁剪会让导入看不出「别的部门的部门名
// 为什么解析失败」。本索引只用于**校验与解析**，不产出任何列表行集。
const DEPT_INDEX_SQL: &str = "SELECT id, dept_name, parent_id, status FROM sys_dept WHERE deleted_at IS NULL";

impl DeptIndex {
    /// 装载部门快照。
    ///
    /// # 参数
    ///
    /// - `ex`：SQL 执行器（导入时传事务，保证与入库同一快照）。
    ///
    /// # 异常情况
    ///
    /// 数据库异常原样返回（宁可整批失败，也不猜一个 dept_id 写进去）。
    async fn load(ex: &impl SqlExecutor) -> DbResult<Self> {
        let rows = ex.query_sql(DEPT_INDEX_SQL.to_string(), vec![]).await?;
        let mut index = DeptIndex::default();
        for row in &rows {
            let id: Uuid = row.get_typed("id")?;
            let name: String = row.get_typed("dept_name")?;
            let parent_id: Option<Uuid> = row.get_typed("parent_id")?;
            let status: i16 = row.get_typed("status")?;
            index.nodes.insert(id, (name, parent_id, status == 1));
        }
        // 第二遍：算完整路径（父链最长 = 树深；节点数有界，不会爆）
        let ids = index.nodes.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let path = index.path_of(id);
            let name = index.nodes.get(&id).map(|node| node.0.clone()).unwrap_or_default();
            index.by_path.insert(normalize_dept_path(&path), id);
            index.by_name.entry(name).or_default().push((id, path));
        }
        Ok(index)
    }

    /// 从根到该部门的名称路径（`总部/生产部/一车间`）。
    fn path_of(&self, id: Uuid) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut current = Some(id);
        let mut guard = 0usize;
        while let Some(node_id) = current {
            guard += 1;
            if guard > 64 {
                // 数据成环（正常不可能）：直接截断，避免死循环
                break;
            }
            let Some((name, parent, _)) = self.nodes.get(&node_id) else {
                break;
            };
            parts.push(name.clone());
            current = *parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// 按名称或路径解析部门 id。
    ///
    /// # 参数
    ///
    /// - `raw`：文件里的原文（空 → 由必填校验先拦）。
    fn resolve(&self, raw: &str) -> std::result::Result<Uuid, DeptError> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(DeptError::NotFound(String::new()));
        }
        let normalized = normalize_dept_path(text);
        if normalized.contains('/')
            && let Some(id) = self.by_path.get(&normalized)
        {
            return self.check_enabled(*id, text);
        }
        match self.by_name.get(text) {
            Some(hits) if hits.len() == 1 => self.check_enabled(hits[0].0, text),
            Some(hits) if hits.len() > 1 => {
                let candidates = hits.iter().map(|(_, path)| path.clone()).collect::<Vec<_>>();
                // 只在唯一命中时才用路径兜底：这里再按路径精确试一次
                if let Some(id) = self.by_path.get(&normalized) {
                    return self.check_enabled(*id, text);
                }
                Err(DeptError::Ambiguous(text.to_string(), candidates))
            }
            _ => Err(DeptError::NotFound(text.to_string())),
        }
    }

    /// 命中的部门必须是启用状态。
    fn check_enabled(&self, id: Uuid, raw: &str) -> std::result::Result<Uuid, DeptError> {
        match self.nodes.get(&id) {
            Some((_, _, true)) => Ok(id),
            Some((_, _, false)) => Err(DeptError::Disabled(raw.to_string(), self.path_of(id))),
            None => Err(DeptError::NotFound(raw.to_string())),
        }
    }
}

/// 部门路径规范化：全角斜杠 / `>` / `»` 一律变 `/`，每段 trim，去掉空段。
fn normalize_dept_path(raw: &str) -> String {
    let unified = raw.replace(['／', '\\'], "/").replace(['＞', '»'], ">");
    let text = unified.replace('>', "/");
    text.split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

// ==================== 库内冲突预筛 ====================

/// 一行的库内冲突标记。
#[derive(Debug, Clone, Copy, Default)]
struct Conflicts {
    /// 身份证号与库里已有员工相同。
    id_card_taken: bool,
    /// 手机号与库里已有员工相同。
    mobile_taken: bool,
}

/// 预筛 SQL：本批每行的身份证 / 手机号是否已在库里（一条往返，`LATERAL` 各走一次索引）。
const CONFLICT_SQL: &str = "\
WITH v AS ( \
    SELECT * FROM jsonb_to_recordset(?::jsonb) AS x(row_no int, id_card text, mobile text) \
) \
SELECT v.row_no::text AS row_no, \
       (COALESCE(v.id_card, '') <> '' AND EXISTS ( \
           SELECT 1 FROM biz_employee e \
            WHERE e.org_id = ?::uuid AND e.deleted_at IS NULL AND e.id_card = v.id_card)) AS id_card_taken, \
       (COALESCE(v.mobile, '') <> '' AND EXISTS ( \
           SELECT 1 FROM biz_employee e \
            WHERE e.org_id = ?::uuid AND e.deleted_at IS NULL AND e.mobile = v.mobile)) AS mobile_taken \
  FROM v ORDER BY v.row_no";

/// 冲突预筛（身份证 / 手机号 → 库里是否已有）。
async fn conflicts_of(ex: &impl SqlExecutor, rows: &[RowValues]) -> DbResult<HashMap<u32, Conflicts>> {
    let mut map = HashMap::new();
    if rows.is_empty() {
        return Ok(map);
    }
    let payload = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "row_no": row.row_no,
                "id_card": text_of(row, KEY_ID_CARD),
                "mobile": text_of(row, KEY_MOBILE),
            })
        })
        .collect::<Vec<_>>();
    let payload =
        serde_json::to_string(&payload).map_err(|error| DbError::InvalidValue(format!("员工行序列化失败：{error}")))?;
    let found = ex
        .query_sql(
            CONFLICT_SQL.to_string(),
            vec![
                Value::Json(payload),
                Value::from(&SENTINEL_ORG_ID),
                Value::from(&SENTINEL_ORG_ID),
            ],
        )
        .await?;
    for row in &found {
        let row_no: String = row.get_typed("row_no")?;
        let row_no = row_no.parse::<u32>().unwrap_or_default();
        map.insert(
            row_no,
            Conflicts {
                id_card_taken: row.get_typed("id_card_taken")?,
                mobile_taken: row.get_typed("mobile_taken")?,
            },
        );
    }
    Ok(map)
}

/// 工号占用预筛：本批里填了工号、且（库里已有 / 文件内重复）的行。
///
/// 返回 `(row_no, 原因)`；`existing_id_card` 用于 UPSERT 语义下区分「同一个人」。
const EMP_NO_CONFLICT_SQL: &str = "\
WITH v AS ( \
    SELECT * FROM jsonb_to_recordset(?::jsonb) AS x(row_no int, emp_no text, id_card text) \
), \
dup AS ( \
    SELECT emp_no FROM v WHERE COALESCE(emp_no, '') <> '' GROUP BY emp_no HAVING count(*) > 1 \
) \
SELECT v.row_no::text AS row_no, v.emp_no::text AS emp_no, \
       (e.id IS NOT NULL) AS taken, COALESCE(e.id_card, '') AS existing_id_card, \
       (d.emp_no IS NOT NULL) AS duplicated \
  FROM v \
  LEFT JOIN biz_employee e ON COALESCE(v.emp_no, '') <> '' AND e.org_id = ?::uuid \
       AND e.deleted_at IS NULL AND e.emp_no = v.emp_no \
  LEFT JOIN dup d ON d.emp_no = v.emp_no \
 WHERE e.id IS NOT NULL OR d.emp_no IS NOT NULL \
 ORDER BY v.row_no";

// ==================== 入库 SQL ====================

/// 入库 SQL 的固定前缀（另一半是 [`conflict_clause`] 拼出来的冲突处理子句）。
///
/// 六个占位符的顺序：org / 操作人 ×2 / jsonb 载荷。
const INSERT_SQL_HEAD: &str = "\
INSERT INTO biz_employee ( \
    id, org_id, emp_no, name, gender, birth_date, id_card, id_card_type, mobile, email, \
    dept_id, position_code, job_level, employment_type, hire_date, probation_end_date, status, \
    education, school, major, graduate_date, native_place, address, \
    emergency_contact, emergency_phone, emergency_relation, \
    bank_name, bank_branch, bank_account, bank_account_name, remark, \
    created_at, updated_at, create_by, update_by) \
SELECT gen_random_uuid(), ?::uuid, v.emp_no, v.name, v.gender::smallint, \
       NULLIF(v.birth_date, '')::date, NULLIF(v.id_card, ''), v.id_card_type, \
       NULLIF(v.mobile, ''), NULLIF(v.email, ''), v.dept_id::uuid, \
       NULLIF(v.position_code, ''), NULLIF(v.job_level, ''), v.employment_type, \
       NULLIF(v.hire_date, '')::date, NULLIF(v.probation_end_date, '')::date, v.status, \
       NULLIF(v.education, ''), NULLIF(v.school, ''), NULLIF(v.major, ''), \
       NULLIF(v.graduate_date, '')::date, NULLIF(v.native_place, ''), NULLIF(v.address, ''), \
       NULLIF(v.emergency_contact, ''), NULLIF(v.emergency_phone, ''), NULLIF(v.emergency_relation, ''), \
       NULLIF(v.bank_name, ''), NULLIF(v.bank_branch, ''), NULLIF(v.bank_account, ''), \
       NULLIF(v.bank_account_name, ''), NULLIF(v.remark, ''), \
       CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
       NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid \
  FROM jsonb_to_recordset(?::jsonb) AS v( \
         row_no int, emp_no text, name text, gender text, birth_date text, id_card text, \
         id_card_type text, mobile text, email text, dept_id text, position_code text, \
         job_level text, employment_type text, hire_date text, probation_end_date text, \
         status text, education text, school text, major text, graduate_date text, \
         native_place text, address text, emergency_contact text, emergency_phone text, \
         emergency_relation text, bank_name text, bank_branch text, bank_account text, \
         bank_account_name text, remark text)";

/// UPSERT 时更新的列（**人事可变列**；工号 / 状态 / 离职日期 / `user_id` 永不覆盖）。
///
/// 为什么不覆盖 `emp_no`：工号是主数据码，工资单与考勤的历史快照都引用它，
/// 一次导入把它改掉，历史单据就对不上人了。
/// 为什么不覆盖 `status` / `leave_date`：已在库的档案可能刚办完离职，
/// 被一份旧花名册「改回在职」是最典型的静默数据事故（`DO UPDATE` 的 `WHERE` 再兜一层）。
const UPSERT_SET: &str = "name = EXCLUDED.name, \
     gender = EXCLUDED.gender, \
     birth_date = EXCLUDED.birth_date, \
     id_card_type = EXCLUDED.id_card_type, \
     mobile = EXCLUDED.mobile, \
     email = EXCLUDED.email, \
     dept_id = EXCLUDED.dept_id, \
     position_code = EXCLUDED.position_code, \
     job_level = EXCLUDED.job_level, \
     employment_type = EXCLUDED.employment_type, \
     hire_date = EXCLUDED.hire_date, \
     probation_end_date = EXCLUDED.probation_end_date, \
     education = EXCLUDED.education, \
     school = EXCLUDED.school, \
     major = EXCLUDED.major, \
     graduate_date = EXCLUDED.graduate_date, \
     native_place = EXCLUDED.native_place, \
     address = EXCLUDED.address, \
     emergency_contact = EXCLUDED.emergency_contact, \
     emergency_phone = EXCLUDED.emergency_phone, \
     emergency_relation = EXCLUDED.emergency_relation, \
     bank_name = EXCLUDED.bank_name, \
     bank_branch = EXCLUDED.bank_branch, \
     bank_account = EXCLUDED.bank_account, \
     bank_account_name = EXCLUDED.bank_account_name, \
     remark = EXCLUDED.remark, \
     updated_at = CURRENT_TIMESTAMP, \
     update_by = EXCLUDED.update_by";

/// 冲突处理子句（`SKIP` 跳过 / `UPSERT` 更新，两者都以 `(org_id, id_card)` 部分唯一索引为键）。
fn conflict_clause(strategy: DuplicateStrategy) -> String {
    match strategy {
        DuplicateStrategy::Skip => {
            " ON CONFLICT (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL DO NOTHING".to_string()
        }
        DuplicateStrategy::Upsert => format!(
            " ON CONFLICT (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL \
             DO UPDATE SET {UPSERT_SET} WHERE biz_employee.status <> 'RESIGNED'"
        ),
    }
}

/// 落库对账 SQL：本批每一行都必须能在库里找到（按工号或身份证），否则就是被静默丢了。
const VERIFY_SQL: &str = "\
WITH v AS ( \
    SELECT * FROM jsonb_to_recordset(?::jsonb) AS x(row_no int, emp_no text, id_card text) \
) \
SELECT v.row_no::text AS row_no \
  FROM v \
 WHERE NOT EXISTS ( \
         SELECT 1 FROM biz_employee e \
          WHERE e.org_id = ?::uuid AND e.deleted_at IS NULL \
            AND (e.emp_no = v.emp_no \
                 OR (COALESCE(v.id_card, '') <> '' AND e.id_card = v.id_card))) \
 ORDER BY v.row_no LIMIT 20";

// ==================== 定义本体 ====================

/// 员工档案导入定义（`hr.employee`）。
pub struct EmployeeImport;

impl EmployeeImport {
    /// 把行拼成 `jsonb_to_recordset` 的载荷（`dept_id` / `emp_no` 已解析好）。
    ///
    /// # 参数
    ///
    /// - `rows`：要入库的行（已剔除跳过行）；
    /// - `dept_ids`：`row_no` → 部门 id；
    /// - `emp_nos`：`row_no` → 工号。
    fn payload(
        rows: &[&RowValues],
        dept_ids: &HashMap<u32, Uuid>,
        emp_nos: &HashMap<u32, String>,
    ) -> std::result::Result<String, DbError> {
        let items = rows
            .iter()
            .map(|row| {
                let dept_id = dept_ids.get(&row.row_no).map(Uuid::to_string).unwrap_or_default();
                let emp_no = emp_nos.get(&row.row_no).cloned().unwrap_or_default();
                let employment_type = row.get(KEY_EMPLOYMENT_TYPE).trim().to_string();
                serde_json::json!({
                    "row_no": row.row_no,
                    "emp_no": emp_no,
                    "name": row.get(KEY_NAME).trim(),
                    "gender": row.get(KEY_GENDER).trim(),
                    "birth_date": text_of(row, KEY_BIRTH_DATE),
                    "id_card": text_of(row, KEY_ID_CARD),
                    "id_card_type": row.get(KEY_ID_CARD_TYPE).trim(),
                    "mobile": text_of(row, KEY_MOBILE),
                    "email": text_of(row, KEY_EMAIL),
                    "dept_id": dept_id,
                    "position_code": text_of(row, KEY_POSITION_CODE),
                    "job_level": text_of(row, KEY_JOB_LEVEL),
                    "employment_type": employment_type,
                    "hire_date": row.get(KEY_HIRE_DATE).trim(),
                    "probation_end_date": text_of(row, KEY_PROBATION_END),
                    "status": status_of(row.get(KEY_EMPLOYMENT_TYPE).trim()),
                    "education": text_of(row, KEY_EDUCATION),
                    "school": text_of(row, KEY_SCHOOL),
                    "major": text_of(row, KEY_MAJOR),
                    "graduate_date": text_of(row, KEY_GRADUATE_DATE),
                    "native_place": text_of(row, KEY_NATIVE_PLACE),
                    "address": text_of(row, KEY_ADDRESS),
                    "emergency_contact": text_of(row, KEY_EMERGENCY_CONTACT),
                    "emergency_phone": text_of(row, KEY_EMERGENCY_PHONE),
                    "emergency_relation": text_of(row, KEY_EMERGENCY_RELATION),
                    "bank_name": text_of(row, KEY_BANK_NAME),
                    "bank_branch": text_of(row, KEY_BANK_BRANCH),
                    "bank_account": text_of(row, KEY_BANK_ACCOUNT),
                    "bank_account_name": text_of(row, KEY_BANK_ACCOUNT_NAME),
                    "remark": text_of(row, KEY_REMARK),
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_string(&items).map_err(|error| DbError::InvalidValue(format!("员工行序列化失败：{error}")))
    }

    /// 落库对账：本批每行都必须在库里能找到。
    ///
    /// # 异常情况
    ///
    /// 有行找不到 → [`DbError::InvalidValue`]（报文带 Excel 行号，整批回滚）。
    async fn verify_persisted(tx: &Transaction, rows: &[&RowValues], emp_nos: &HashMap<u32, String>) -> DbResult<()> {
        let items = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "row_no": row.row_no,
                    "emp_no": emp_nos.get(&row.row_no).cloned().unwrap_or_default(),
                    "id_card": text_of(row, KEY_ID_CARD),
                })
            })
            .collect::<Vec<_>>();
        let payload = serde_json::to_string(&items)
            .map_err(|error| DbError::InvalidValue(format!("员工行序列化失败：{error}")))?;
        let missing = tx
            .find_with_params(VERIFY_SQL, vec![Value::Json(payload), Value::from(&SENTINEL_ORG_ID)])
            .await?;
        if missing.is_empty() {
            return Ok(());
        }
        let lines = missing
            .iter()
            .map(|row| row.get_typed::<String>("row_no").unwrap_or_default())
            .collect::<Vec<_>>();
        Err(DbError::InvalidValue(format!(
            "员工导入落库对账不平：Excel 第 {} 行写完在库里找不到（部门 / 工号在预检之后被并发改动？），\
             已整批回滚，请刷新后重新导入",
            lines.join("、")
        )))
    }

    /// 操作人文本（空串 → SQL 里 `NULLIF(?::text,'')::uuid` → NULL）。
    fn operator_text(ctx: &ImportCtx) -> String {
        ctx.operator.map(|id| id.to_string()).unwrap_or_default()
    }
}

#[async_trait]
impl ImportDefinition for EmployeeImport {
    fn biz_type(&self) -> &'static str {
        EMPLOYEE_BIZ_TYPE
    }

    fn biz_name(&self) -> &'static str {
        "员工档案"
    }

    fn max_rows(&self) -> usize {
        MAX_ROWS
    }

    fn batch_size(&self) -> usize {
        BATCH_SIZE
    }

    /// 允许部分导入：花名册里几行错是常态（部门写错一个、身份证抄错一位），
    /// 卡面要求「导入结果：成功 N 行 / 失败 M 行」，因此 `PARTIAL` 必须可用。
    fn allow_partial(&self) -> bool {
        true
    }

    fn columns(&self) -> &'static [ImportColumn] {
        COLUMNS
    }

    /// 单行业务规则（纯计算，不碰库）。
    ///
    /// # 参数
    ///
    /// - `row`：已过框架逐格校验（必填 / 类型 / 长度 / 枚举）的行；
    /// - `_ctx`：导入上下文（本定义不需要）。
    ///
    /// # 异常情况
    ///
    /// 返回非空 `Vec<RowIssue>` 表示该行不可入库（每条都带 Excel 行号与列名）。
    async fn validate_row(&self, row: &RowValues, _ctx: &ImportCtx) -> std::result::Result<(), Vec<RowIssue>> {
        let mut issues = Vec::new();
        let today = today_cst();
        let hire = date_of(row, KEY_HIRE_DATE);

        // ① 入职日期不能在未来
        if let Some(hire_date) = hire
            && hire_date > today
            && let Some(issue) = column_issue(
                row,
                KEY_HIRE_DATE,
                ErrorKind::Rule,
                format!("入职日期「{hire_date}」在未来（今天 {today}）：未来入职请到时再建档"),
            )
        {
            issues.push(issue);
        }

        // ② 生日必须比入职日期早满 16 年（劳动法下限；拦「抄错年份」）
        if let (Some(hire_date), Some(birth)) = (hire, date_of(row, KEY_BIRTH_DATE))
            && !is_older_than(birth, hire_date, 16)
            && let Some(issue) = column_issue(
                row,
                KEY_BIRTH_DATE,
                ErrorKind::Rule,
                format!("生日「{birth}」到入职日「{hire_date}」不满 16 岁，请核对年份"),
            )
        {
            issues.push(issue);
        }

        // ③ 毕业日期不能在未来
        if let Some(graduate) = date_of(row, KEY_GRADUATE_DATE)
            && graduate > today
            && let Some(issue) = column_issue(
                row,
                KEY_GRADUATE_DATE,
                ErrorKind::Rule,
                format!("毕业日期「{graduate}」在未来（今天 {today}）"),
            )
        {
            issues.push(issue);
        }

        // ④ 试用期结束不能早于入职（与库层 ck_biz_employee_probation 同口径，
        //    但要在导入期就报出来，别让用户先看到库约束名）
        if let (Some(hire_date), Some(probation)) = (hire, date_of(row, KEY_PROBATION_END))
            && probation < hire_date
            && let Some(issue) = column_issue(
                row,
                KEY_PROBATION_END,
                ErrorKind::Rule,
                format!("试用期结束「{probation}」早于入职日期「{hire_date}」"),
            )
        {
            issues.push(issue);
        }

        // ⑤ 身份证：填了就要合法（格式 + 校验位），身份证类型还要求 18/15 位居民证
        let id_card = row.get(KEY_ID_CARD).trim();
        if !id_card.is_empty()
            && !id_card_valid(id_card)
            && let Some(issue) = column_issue(
                row,
                KEY_ID_CARD,
                ErrorKind::Rule,
                format!("身份证号「{id_card}」不合法（18 位含校验位，或 15 位老证）"),
            )
        {
            issues.push(issue);
        }

        // ⑥ 手机 / 紧急联系电话：数字（可带 +86 / 空格 / 连字符）
        for key in [KEY_MOBILE, KEY_EMERGENCY_PHONE] {
            let raw = row.get(key).trim();
            if !raw.is_empty()
                && !phone_valid(raw)
                && let Some(issue) = column_issue(
                    row,
                    key,
                    ErrorKind::Rule,
                    format!("电话号码「{raw}」不合法（7–15 位数字，可带 +86 / 空格 / 连字符）"),
                )
            {
                issues.push(issue);
            }
        }

        // ⑦ 邮箱
        let email = row.get(KEY_EMAIL).trim();
        if !email.is_empty()
            && !email_valid(email)
            && let Some(issue) = column_issue(
                row,
                KEY_EMAIL,
                ErrorKind::Rule,
                format!("邮箱「{email}」格式不正确（应形如 name@example.com）"),
            )
        {
            issues.push(issue);
        }

        // ⑧ 银行账号
        let account = row.get(KEY_BANK_ACCOUNT).trim();
        if !account.is_empty()
            && !bank_account_valid(account)
            && let Some(issue) = column_issue(
                row,
                KEY_BANK_ACCOUNT,
                ErrorKind::Rule,
                format!("银行账号「{account}」不合法（8–40 位数字，可带空格 / 连字符）"),
            )
        {
            issues.push(issue);
        }

        if issues.is_empty() { Ok(()) } else { Err(issues) }
    }

    /// 声明有批次级校验（框架据此保留整批行给 [`Self::validate_batch`]）。
    fn validates_batch(&self) -> bool {
        true
    }

    /// 批次级校验：**部门名 → dept_id**、**工号占用**（都要整批 + 库内数据才有答案）。
    ///
    /// 手机号 / 身份证与库里已有的重复**不在这里报错**：那是幂等语义（同一份花名册
    /// 导第二遍不该红一片），由 [`Self::persist`] 跳过并在结果里报「已跳过 N 行（重复）」。
    ///
    /// # 参数
    ///
    /// - `rows`：整批已过逐行校验的行；
    /// - `ctx`：导入上下文（UPSERT 语义下要区分「同一个人的工号」）。
    ///
    /// # 异常情况
    ///
    /// 数据库异常 → [`DbError`]（框架转 500，不算用户错）。
    async fn validate_batch(&self, rows: &[RowValues], ctx: &ImportCtx) -> DbResult<Vec<RowIssue>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut issues = Vec::new();

        // ① 部门名称 / 路径 → dept_id
        let index = DeptIndex::load(db()).await?;
        for row in rows {
            if let Err(error) = index.resolve(row.get(KEY_DEPT_NAME))
                && let Some(issue) = column_issue(row, KEY_DEPT_NAME, ErrorKind::Rule, error.to_string())
            {
                issues.push(issue);
            }
        }

        // ② 工号占用（填了才有）：库里已有 / 文件内重复
        let filled = rows
            .iter()
            .filter(|row| !row.get(KEY_EMP_NO).trim().is_empty())
            .map(|row| {
                serde_json::json!({
                    "row_no": row.row_no,
                    "emp_no": row.get(KEY_EMP_NO).trim(),
                    "id_card": text_of(row, KEY_ID_CARD),
                })
            })
            .collect::<Vec<_>>();
        if !filled.is_empty() {
            let payload = serde_json::to_string(&filled)
                .map_err(|error| DbError::InvalidValue(format!("员工行序列化失败：{error}")))?;
            let found = db()
                .query_sql(
                    EMP_NO_CONFLICT_SQL.to_string(),
                    vec![Value::Json(payload), Value::from(&SENTINEL_ORG_ID)],
                )
                .await?;
            let upsert = duplicate_strategy(ctx).await? == DuplicateStrategy::Upsert;
            let by_row = rows.iter().map(|row| (row.row_no, row)).collect::<HashMap<_, _>>();
            for conflict in &found {
                let row_no = conflict
                    .get_typed::<String>("row_no")?
                    .parse::<u32>()
                    .unwrap_or_default();
                let emp_no: String = conflict.get_typed("emp_no")?;
                let duplicated: bool = conflict.get_typed("duplicated")?;
                let existing_id_card: String = conflict.get_typed("existing_id_card")?;
                let Some(row) = by_row.get(&row_no) else {
                    continue;
                };
                if duplicated {
                    if let Some(issue) = column_issue(
                        row,
                        KEY_EMP_NO,
                        ErrorKind::Unique,
                        format!("工号「{emp_no}」在本文件里出现了多次（每人一个唯一工号）"),
                    ) {
                        issues.push(issue);
                    }
                    continue;
                }
                // UPSERT + 同一身份证 ⇒ 是同一个人，允许按身份证更新（不报错）
                let same_person =
                    upsert && !existing_id_card.is_empty() && existing_id_card == row.get(KEY_ID_CARD).trim();
                if same_person {
                    continue;
                }
                if let Some(issue) = column_issue(
                    row,
                    KEY_EMP_NO,
                    ErrorKind::Unique,
                    format!(
                        "工号「{emp_no}」已被别的员工占用（身份证对不上），请留空由系统取号，\
                         或填这个人自己的工号"
                    ),
                ) {
                    issues.push(issue);
                }
            }
        }

        Ok(issues)
    }

    /// 整批入库（框架已在事务内）。
    ///
    /// # 参数
    ///
    /// - `tx`：当前事务；
    /// - `rows`：已过预检的行；
    /// - `ctx`：导入上下文（操作人与重复数据策略）。
    ///
    /// # 返回值
    ///
    /// 实际写入（含 UPSERT 更新）的行数。`SKIP` 策略下重复行不算写入，返回值小于
    /// `rows.len()` —— 前端用「预检通过数 − 本返回值」显示「已跳过 N 行（重复）」。
    ///
    /// # 异常情况
    ///
    /// - 部门在预检之后被删 / 停用 → [`DbError::InvalidValue`]（带行号，整批回滚）；
    /// - 落库对账不平（有行被静默丢弃）→ [`DbError::InvalidValue`]（整批回滚）。
    async fn persist(&self, tx: &Transaction, rows: &[RowValues], ctx: &ImportCtx) -> DbResult<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let strategy = duplicate_strategy(ctx).await?;

        // ① 部门解析（同一事务快照内复核，防「预检之后部门被删」把行静默 JOIN 掉）
        let index = DeptIndex::load(tx).await?;
        let mut dept_ids: HashMap<u32, Uuid> = HashMap::new();
        let mut missing: Vec<String> = Vec::new();
        for row in rows {
            match index.resolve(row.get(KEY_DEPT_NAME)) {
                Ok(id) => {
                    dept_ids.insert(row.row_no, id);
                }
                Err(error) => missing.push(format!("Excel 第 {} 行：{error}", row.row_no)),
            }
        }
        if !missing.is_empty() {
            return Err(DbError::InvalidValue(format!(
                "员工导入被拦下，整批未入库（{} 行部门对不上）：{}",
                missing.len(),
                missing.join("；")
            )));
        }

        // ② 重复行（身份证 / 手机号与库里已有相同）：SKIP 策略下跳过，UPSERT 策略下按身份证更新
        let conflicts = conflicts_of(tx, rows).await?;
        let mut kept: Vec<&RowValues> = Vec::new();
        for row in rows {
            let flags = conflicts.get(&row.row_no).copied().unwrap_or_default();
            if strategy == DuplicateStrategy::Skip && (flags.id_card_taken || flags.mobile_taken) {
                continue;
            }
            kept.push(row);
        }
        if kept.is_empty() {
            return Ok(0);
        }

        // ③ 工号：填了原样保留，留空取号（与插入同事务 ⇒ 失败回滚，号不被白吃）
        let mut emp_nos: HashMap<u32, String> = HashMap::new();
        for row in &kept {
            let filled = row.get(KEY_EMP_NO).trim().to_string();
            if !filled.is_empty() {
                emp_nos.insert(row.row_no, filled);
                continue;
            }
            let generated = employee_no::generate_emp_no(tx).await.map_err(|error| {
                DbError::InvalidValue(format!(
                    "自动取号失败（Excel 第 {} 行留空工号）：{}，本次导入整批回滚",
                    row.row_no, error.msg
                ))
            })?;
            emp_nos.insert(row.row_no, generated);
        }

        // ④ 入库
        let payload = Self::payload(&kept, &dept_ids, &emp_nos)?;
        let sql = format!("{INSERT_SQL_HEAD}{}", conflict_clause(strategy));
        let operator = Self::operator_text(ctx);
        let affected = tx
            .execute_with_params(
                &sql,
                vec![
                    Value::from(&SENTINEL_ORG_ID),
                    Value::from(operator.clone()),
                    Value::from(operator),
                    Value::Json(payload),
                ],
            )
            .await?;
        let affected = affected as usize;
        // 少写不是错误（SKIP 跳过重复 / UPSERT 跳过已离职），多写才是 bug（同一条 SQL 不会多写）。
        if affected > kept.len() {
            return Err(DbError::InvalidValue(format!(
                "员工导入异常：本批 {} 行却写了 {} 行，已整批回滚",
                kept.len(),
                affected
            )));
        }
        if affected == 0 && strategy == DuplicateStrategy::Skip {
            tracing::debug!(
                task = %ctx.task_id,
                rows = kept.len(),
                "员工导入：本批全部命中库内重复，按 SKIP 跳过"
            );
        }

        // ⑤ 落库对账：被 JOIN / 唯一键静默丢行是不能接受的（花名册少人 = 工资少发）
        Self::verify_persisted(tx, &kept, &emp_nos).await?;
        Ok(affected)
    }
}

// ==================== 单测（纯计算，不需要库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::impexp::definition::ImportMode;

    fn row_of(row_no: u32, pairs: &[(&'static str, &str)]) -> RowValues {
        let mut values = RowValues {
            row_no,
            ..Default::default()
        };
        for (key, value) in pairs {
            values.fields.insert(key, value.to_string());
        }
        values
    }

    /// 一行合法数据（日期按「今天」推导，避免随日期腐化）。
    fn good_row(row_no: u32) -> RowValues {
        let today = today_cst();
        let hire = (today - chrono::Duration::days(30)).format("%Y-%m-%d").to_string();
        let birth = (today - chrono::Duration::days(365 * 30))
            .format("%Y-%m-%d")
            .to_string();
        let probation = (today - chrono::Duration::days(-60)).format("%Y-%m-%d").to_string();
        let graduate = (today - chrono::Duration::days(365 * 8)).format("%Y-%m-%d").to_string();
        row_of(
            row_no,
            &[
                (KEY_NAME, "张三"),
                (KEY_GENDER, "1"),
                (KEY_ID_CARD_TYPE, "ID_CARD"),
                (KEY_ID_CARD, "440203199005201235"),
                (KEY_MOBILE, "13800138000"),
                (KEY_EMAIL, "zhangsan@example.com"),
                (KEY_DEPT_NAME, "生产部"),
                (KEY_POSITION_CODE, "WORKER"),
                (KEY_JOB_LEVEL, "P5"),
                (KEY_EMPLOYMENT_TYPE, "FULL_TIME"),
                (KEY_HIRE_DATE, hire.as_str()),
                (KEY_PROBATION_END, probation.as_str()),
                (KEY_BIRTH_DATE, birth.as_str()),
                (KEY_EDUCATION, "BACHELOR"),
                (KEY_SCHOOL, "某某大学"),
                (KEY_MAJOR, "机械工程"),
                (KEY_GRADUATE_DATE, graduate.as_str()),
                (KEY_NATIVE_PLACE, "广东韶关"),
                (KEY_ADDRESS, "广东省韶关市武江区某某路 1 号"),
                (KEY_EMERGENCY_CONTACT, "李四"),
                (KEY_EMERGENCY_PHONE, "13900139000"),
                (KEY_EMERGENCY_RELATION, "配偶"),
                (KEY_BANK_NAME, "中国工商银行"),
                (KEY_BANK_BRANCH, "韶关分行武江支行"),
                (KEY_BANK_ACCOUNT, "6222021234567890123"),
                (KEY_BANK_ACCOUNT_NAME, "张三"),
                (KEY_REMARK, "内部调岗入职"),
            ],
        )
    }

    fn ctx() -> ImportCtx {
        ImportCtx {
            task_id: Uuid::nil(),
            operator: None,
            mode: ImportMode::Partial,
        }
    }

    #[test]
    fn column_contract_matches_card_table() {
        let keys = COLUMNS.iter().map(|column| column.key).collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "name",
                "gender",
                "id_card_type",
                "id_card",
                "mobile",
                "email",
                "dept_name",
                "position_code",
                "job_level",
                "employment_type",
                "hire_date",
                "probation_end_date",
                "birth_date",
                "education",
                "school",
                "major",
                "graduate_date",
                "native_place",
                "address",
                "emergency_contact",
                "emergency_phone",
                "emergency_relation",
                "bank_name",
                "bank_branch",
                "bank_account",
                "bank_account_name",
                "remark",
                "emp_no"
            ],
            "前 27 列 = 卡面 A..AA 逐字；第 28 列「工号」是按卡面「追加列到末尾」的规则补的"
        );
        // 必填：姓名 / 性别 / 证件类型 / 部门 / 用工类型 / 入职日期
        let required = COLUMNS
            .iter()
            .filter(|column| column.required)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(
            required,
            vec![
                "name",
                "gender",
                "id_card_type",
                "dept_name",
                "employment_type",
                "hire_date"
            ]
        );
        // 文件内唯一只有身份证（工号由 validate_batch 自己判，手机号不设唯一）
        let unique = COLUMNS
            .iter()
            .filter(|column| column.unique_key)
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(unique, vec!["id_card"]);
        // 表头互不相同（Excel 表头重复会被框架判结构错误）
        let mut headers = COLUMNS
            .iter()
            .map(|column| column.template_header())
            .collect::<Vec<_>>();
        let before = headers.len();
        headers.sort_unstable();
        headers.dedup();
        assert_eq!(headers.len(), before, "模板表头必须互不相同");
        // 枚举列必须都有下拉（卡面要求性别 / 证件类型 / 用工类型 / 学历四列下拉）
        let enum_columns = COLUMNS
            .iter()
            .filter(|column| matches!(column.kind, ColumnKind::Enum(_)))
            .map(|column| column.key)
            .collect::<Vec<_>>();
        assert_eq!(
            enum_columns,
            vec!["gender", "id_card_type", "employment_type", "education"]
        );
    }

    #[test]
    fn enum_codes_are_legal_db_values() {
        // 与 V0383 的 CHECK 逐字对齐：英文码写错会在入库时撞库约束，而那时用户
        // 早就把模板填完了 —— 必须在列定义这一层钉住
        let gender = GENDER_ITEMS.iter().map(|item| item.code).collect::<Vec<_>>();
        assert_eq!(gender, vec!["1", "2"]);
        let id_types = ID_CARD_TYPE_ITEMS.iter().map(|item| item.code).collect::<Vec<_>>();
        assert_eq!(id_types, vec!["ID_CARD", "PASSPORT", "HK_MO_TW", "FOREIGN"]);
        let employments = EMPLOYMENT_TYPE_ITEMS.iter().map(|item| item.code).collect::<Vec<_>>();
        assert_eq!(
            employments,
            vec!["FULL_TIME", "PROBATION", "INTERN", "OUTSOURCE", "PART_TIME", "DISPATCH"]
        );
        let educations = EDUCATION_ITEMS.iter().map(|item| item.code).collect::<Vec<_>>();
        assert_eq!(
            educations,
            vec!["PRIMARY", "JUNIOR", "SENIOR", "COLLEGE", "BACHELOR", "MASTER", "DOCTOR"]
        );
        // 中文标签互不相同（下拉里出现两个一样的选项等于没得选）
        for items in [GENDER_ITEMS, ID_CARD_TYPE_ITEMS, EMPLOYMENT_TYPE_ITEMS, EDUCATION_ITEMS] {
            let mut labels = items.iter().map(|item| item.label).collect::<Vec<_>>();
            let before = labels.len();
            labels.sort_unstable();
            labels.dedup();
            assert_eq!(labels.len(), before);
        }
    }

    #[test]
    fn status_derivation_only_creates_on_duty_records() {
        assert_eq!(status_of("PROBATION"), "PROBATION");
        assert_eq!(status_of("INTERN"), "PROBATION");
        assert_eq!(status_of("FULL_TIME"), "ACTIVE");
        assert_eq!(status_of("OUTSOURCE"), "ACTIVE");
        assert_eq!(status_of("PART_TIME"), "ACTIVE");
        assert_eq!(status_of("DISPATCH"), "ACTIVE");
    }

    #[test]
    fn id_card_checksum_matches_known_samples() {
        // 真实可用的样例号（校验位按 ISO 7064 MOD 11-2 算得）
        for ok in ["11010519491231002X", "440524188001010014", "440203199005201235"] {
            assert!(id_card_valid(ok), "{ok} 应合法");
        }
        for bad in [
            "",
            "4402031990052012",    // 16 位
            "440203199005201233",  // 校验位错
            "44020319900520123X",  // 校验位错
            "44020319900520123A",  // 非数字
            "44020319900520123",   // 17 位
            "123456789012345678",  // 校验位错
            "4402031990052012321", // 19 位
        ] {
            assert!(!id_card_valid(bad), "{bad} 应非法");
        }
        // 15 位老证（纯数字，无校验位）
        assert!(id_card_valid("440203900520123"));
        assert!(!id_card_valid("44020390052012A"));
    }

    #[test]
    fn phone_and_bank_account_normalization() {
        assert!(phone_valid("13800138000"));
        assert!(phone_valid("+86 138-0013-8000"));
        assert!(phone_valid("0751-8888888"));
        assert!(!phone_valid("138001")); // 6 位太短
        assert!(!phone_valid("1380013800012345")); // 16 位太长
        assert!(!phone_valid("张三的电话"));
        assert!(bank_account_valid("6222 0212 3456 7890 123"));
        assert!(bank_account_valid("6222021234567890"));
        assert!(!bank_account_valid("6222021")); // 太短
        assert!(!bank_account_valid("62220212ABC"));
        assert_eq!(normalize_phone("+86 138-0013-8000"), "13800138000");
    }

    #[test]
    fn email_validation() {
        assert!(email_valid("zhangsan@example.com"));
        assert!(email_valid("a.b+c@sub.example.cn"));
        assert!(!email_valid("zhangsan.example.com"));
        assert!(!email_valid("zhangsan@example"));
        assert!(!email_valid("@example.com"));
        assert!(!email_valid("zhangsan@.com"));
    }

    #[test]
    fn dept_path_normalization() {
        assert_eq!(normalize_dept_path("生产部"), "生产部");
        assert_eq!(normalize_dept_path(" 总部 / 生产部 "), "总部/生产部");
        assert_eq!(normalize_dept_path("总部>生产部>一车间"), "总部/生产部/一车间");
        assert_eq!(normalize_dept_path("总部＞生产部"), "总部/生产部");
        assert_eq!(normalize_dept_path("总部／生产部"), "总部/生产部");
        assert_eq!(normalize_dept_path("/总部//生产部/"), "总部/生产部");
    }

    #[tokio::test]
    async fn validate_row_accepts_a_complete_row() {
        EmployeeImport
            .validate_row(&good_row(3), &ctx())
            .await
            .expect("完整行应通过");
    }

    #[tokio::test]
    async fn validate_row_rejects_future_hire_date() {
        let mut row = good_row(9);
        let future = (today_cst() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        row.fields.insert(KEY_HIRE_DATE, future.clone());
        let issues = EmployeeImport
            .validate_row(&row, &ctx())
            .await
            .expect_err("未来入职应报错");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].row_no, 9);
        assert_eq!(issues[0].column_key, Some(KEY_HIRE_DATE));
        assert!(issues[0].message.contains(&future), "{}", issues[0].message);
    }

    #[tokio::test]
    async fn validate_row_rejects_under_sixteen_at_hire() {
        let mut row = good_row(4);
        let hire = (today_cst() - chrono::Duration::days(365))
            .format("%Y-%m-%d")
            .to_string();
        // 出生日期比入职只早 10 年
        let birth = (today_cst() - chrono::Duration::days(365 * 11))
            .format("%Y-%m-%d")
            .to_string();
        row.fields.insert(KEY_HIRE_DATE, hire);
        row.fields.insert(KEY_BIRTH_DATE, birth);
        let issues = EmployeeImport
            .validate_row(&row, &ctx())
            .await
            .expect_err("不满 16 岁应报错");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].column_key, Some(KEY_BIRTH_DATE));
        assert!(issues[0].message.contains("16 岁"), "{}", issues[0].message);
    }

    #[tokio::test]
    async fn validate_row_rejects_probation_before_hire() {
        let mut row = good_row(5);
        let hire = (today_cst() - chrono::Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let probation = (today_cst() - chrono::Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();
        row.fields.insert(KEY_HIRE_DATE, hire);
        row.fields.insert(KEY_PROBATION_END, probation);
        let issues = EmployeeImport
            .validate_row(&row, &ctx())
            .await
            .expect_err("试用期早于入职应报错");
        assert_eq!(issues[0].column_key, Some(KEY_PROBATION_END));
    }

    #[tokio::test]
    async fn validate_row_reports_every_bad_cell_in_one_pass() {
        let mut row = good_row(7);
        row.fields.insert(KEY_ID_CARD, "440203199005201233".into());
        row.fields.insert(KEY_MOBILE, "abc".into());
        row.fields.insert(KEY_EMAIL, "not-an-email".into());
        row.fields.insert(KEY_BANK_ACCOUNT, "12".into());
        let issues = EmployeeImport
            .validate_row(&row, &ctx())
            .await
            .expect_err("四处都错应一次报四条");
        let keys = issues.iter().filter_map(|issue| issue.column_key).collect::<Vec<_>>();
        assert_eq!(keys, vec![KEY_ID_CARD, KEY_MOBILE, KEY_EMAIL, KEY_BANK_ACCOUNT]);
        assert_eq!(issues[0].row_no, 7);
    }

    #[test]
    fn empty_optional_cells_are_not_errors() {
        // 只填必填项的最小合法行：可选列全空不该报任何错
        let row = row_of(
            3,
            &[
                (KEY_NAME, "李四"),
                (KEY_GENDER, "2"),
                (KEY_ID_CARD_TYPE, "ID_CARD"),
                (KEY_EMPLOYMENT_TYPE, "INTERN"),
                (KEY_DEPT_NAME, "生产部"),
                (KEY_HIRE_DATE, "2026-01-15"),
            ],
        );
        let handle = tokio::runtime::Runtime::new().expect("runtime");
        handle.block_on(async {
            EmployeeImport.validate_row(&row, &ctx()).await.expect("最小行应通过");
        });
    }

    #[test]
    fn payload_carries_resolved_ids_and_nulls_blanks() {
        let row = good_row(7);
        let mut minimal = row_of(
            8,
            &[
                (KEY_NAME, "李四"),
                (KEY_GENDER, "2"),
                (KEY_ID_CARD_TYPE, "PASSPORT"),
                (KEY_EMPLOYMENT_TYPE, "INTERN"),
                (KEY_DEPT_NAME, "生产部"),
                (KEY_HIRE_DATE, "2026-01-15"),
            ],
        );
        minimal.fields.remove(KEY_MOBILE);
        let dept = Uuid::now_v7();
        let mut dept_ids = HashMap::new();
        dept_ids.insert(7u32, dept);
        dept_ids.insert(8u32, dept);
        let mut emp_nos = HashMap::new();
        emp_nos.insert(7u32, "EMP-0007".to_string());
        emp_nos.insert(8u32, "EMP-0008".to_string());
        let payload = EmployeeImport::payload(&[&row, &minimal], &dept_ids, &emp_nos).expect("序列化应成功");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("应是合法 JSON");
        let items = parsed.as_array().expect("应是数组");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["row_no"], 7);
        assert_eq!(items[0]["dept_id"], dept.to_string());
        assert_eq!(items[0]["emp_no"], "EMP-0007");
        assert_eq!(items[0]["status"], "ACTIVE");
        assert_eq!(items[1]["status"], "PROBATION");
        // 未填的可选列 → null（落库走 NULLIF，不是空串）
        assert!(items[1]["mobile"].is_null());
        assert!(items[1]["birth_date"].is_null());
        assert!(items[1]["bank_account"].is_null());
        assert!(items[1]["remark"].is_null());
    }

    #[test]
    fn insert_sql_placeholders_and_guards() {
        // 占位符个数与 persist 的绑定顺序必须一致（多一个少一个都是运行期报错，
        // 而这条 SQL 在单测里跑不到 —— 要先有库）。
        // 本定义没有 JOIN 守卫（部门 id 在 Rust 侧解析好），所以只有 1 个 org 占位符：
        // org ×1 + 操作人 ×2 + jsonb 载荷 ×1 = 4。这条断言在开发期抓到过一次
        // 「绑了 6 个参数、SQL 只有 4 个 ?」的错位。
        assert_eq!(INSERT_SQL_HEAD.matches('?').count(), 4, "org ×1 + 操作人 ×2 + jsonb ×1");
        // 冲突目标必须是 (org_id, id_card) 的部分唯一索引（与 V0383 逐字一致）
        let skip = conflict_clause(DuplicateStrategy::Skip);
        assert!(skip.contains("ON CONFLICT (org_id, id_card) WHERE id_card IS NOT NULL AND deleted_at IS NULL"));
        assert!(skip.contains("DO NOTHING"));
        let upsert = conflict_clause(DuplicateStrategy::Upsert);
        assert!(upsert.contains("DO UPDATE SET"));
        assert!(
            upsert.contains("WHERE biz_employee.status <> 'RESIGNED'"),
            "已离职档案不许被导入复活"
        );
        // UPSERT 不许改写工号 / 状态 / 离职日期 / 绑定账号
        let assigned = UPSERT_SET
            .split(',')
            .map(|fragment| fragment.trim().split('=').next().unwrap_or("").trim())
            .collect::<Vec<_>>();
        for guarded in [
            "id",
            "emp_no",
            "status",
            "leave_date",
            "leave_type",
            "user_id",
            "org_id",
        ] {
            assert!(
                !assigned.contains(&guarded),
                "upsert 不许改写 {guarded}，实际更新列：{assigned:?}"
            );
        }
        // 服务端算的列必须显式写死 / 显式置空，不能从载荷里取
        assert!(INSERT_SQL_HEAD.contains("CURRENT_TIMESTAMP"));
        assert!(INSERT_SQL_HEAD.contains("gen_random_uuid()"));
        // 预筛 SQL 的两个判据（身份证 / 手机号）都在
        for fragment in ["id_card", "mobile", "deleted_at IS NULL", "org_id = ?::uuid"] {
            assert!(CONFLICT_SQL.contains(fragment), "预筛必须覆盖：{fragment}");
        }
        assert_eq!(CONFLICT_SQL.matches('?').count(), 3);
        // 工号预筛要能区分「文件内重复」与「库里已占用」
        assert!(EMP_NO_CONFLICT_SQL.contains("HAVING count(*) > 1"));
        assert!(EMP_NO_CONFLICT_SQL.contains("existing_id_card"));
        // 对账 SQL 按「工号或身份证」找回每一行
        assert!(VERIFY_SQL.contains("e.emp_no = v.emp_no"));
        assert!(VERIFY_SQL.contains("e.id_card = v.id_card"));
    }

    #[test]
    fn definition_metadata_is_stable() {
        let definition = EmployeeImport;
        assert_eq!(definition.biz_type(), "hr.employee");
        assert_eq!(definition.biz_name(), "员工档案");
        assert_eq!(definition.template_version(), 1);
        assert_eq!(definition.max_rows(), MAX_ROWS);
        assert_eq!(MAX_ROWS, 1_000, "卡面：1000 行以内，超过提示分批");
        // 一份文件一个事务（STRICT 才是真「全有或全无」）
        assert_eq!(definition.batch_size(), MAX_ROWS);
        assert!(definition.allow_partial(), "花名册几行错是常态，必须允许部分导入");
        assert!(definition.validates_batch(), "本定义有批次级校验（部门 / 工号）");
        assert_eq!(definition.columns().len(), 28);
    }

    #[test]
    fn definition_is_registered_in_global_registry() {
        // 注册才是「接进框架」的判据：漏挂 builtin::register 的话，模板下载 /
        // 上传接口按 biz_type 查不到定义 → 60011
        let registry = crate::business::impexp::registry::global_registry();
        let definition = registry
            .import_definition(EMPLOYEE_BIZ_TYPE)
            .expect("hr.employee 应已注册为导入定义");
        assert_eq!(definition.biz_type(), EMPLOYEE_BIZ_TYPE);
        assert_eq!(definition.columns().len(), COLUMNS.len());
        let brief = registry
            .briefs()
            .into_iter()
            .find(|brief| brief.biz_type == EMPLOYEE_BIZ_TYPE)
            .expect("应在定义摘要列表里（前端下拉用它渲染）");
        assert_eq!(brief.biz_name, "员工档案");
        assert!(brief.supports_import);
        assert_eq!(brief.template_version, 1);
        assert_eq!(brief.max_rows, MAX_ROWS);
    }

    #[test]
    fn template_can_be_built() {
        // 模板是「定义 → xlsx」的纯函数，不需要库
        let bytes = crate::business::impexp::template::build_template(&EmployeeImport).expect("模板应生成成功");
        assert!(bytes.len() > 4_000, "xlsx 至少几 KB，实际 {} 字节", bytes.len());
        let name = crate::business::impexp::template::template_file_name(&EmployeeImport);
        assert_eq!(name, "员工档案导入模板_v1.xlsx");
    }
}
