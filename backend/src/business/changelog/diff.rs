//! diff 引擎（批次 C · C5）：白名单登记 + 只记变化字段 + `{b,a}` 成对 + 大字段摘要化。
//!
//! # 四条铁律（架构 §4.4.2，逐条都有对应单测）
//!
//! | 铁律 | 违反后果 | 本文件的强制手段 |
//! |------|----------|------------------|
//! | **只记变化字段** | diff 塞满未变字段，行宽爆炸（POC-5：6 字段 355 B vs 全字段 ~2 KB） | [`build`] 先做 `b == a` 过滤 |
//! | **`b` / `a` 必须成对** | 只有 `a` 没 `b` 会被渲染成「从空改成 X」，对 UPDATE 是错的 | [`FieldPair`] 只有同时含 `b` 与 `a` 的构造方式；新增/删除用显式 `null` 侧 |
//! | **同一字段只有一个键** | 出现 `mobile` 与 `Mobile` 两行 | 字段名一律取**列名原样**（小写下划线），不做驼峰转换 |
//! | **未登记的表拒绝记录** | `password` 这类列悄悄进 diff | [`spec`] 返回 `None` → 调用方报 62012（fail-closed） |
//!
//! # 白名单为什么是「每资源显式登记」而不是全字段快照
//!
//! 全字段快照的噪声（`updated_at` 每次都变）与体积（~2 KB/行 × 36 个月）都不划算，
//! 且「哪些字段值得审计」本身是业务语义，只能由人声明。登记表就是这份声明，
//! 一表一条 [`ResourceSpec`]。**未登记 = 拒绝记录**，而不是「退化为全字段」——
//! 与批次 B D8 的 fail-closed 一致：宁可让开发在接入时补白名单，也不让脏列混进审计。
//!
//! # 大字段（富文本 / bytea）怎么处理
//!
//! 结构化摘要 `{len, sha256, preview(≤200 字), truncated: true}`，**全文不入库**：
//! 8k 字不可压缩富文本的前后双快照落盘 51 KB/行（POC-5 实测），日志是热表，扛不住。
//! 触发条件：列被登记为 `big`（text / bytea / jsonb，类型信息只有登记表知道），
//! 或单个值的序列化长度 > [`BIG_FIELD_BYTES`]（2048）。
//!
//! # 敏感列的优先级高于大字段
//!
//! 一个既「大」又「敏感」的列（如带 token 的长文本）**不进 `big`**，而是进 `fields`
//! 并整体 `{"redacted": true}`：摘要里的 `len` 与 `sha256` 对低熵秘密（口令、验证码）
//! 是可用信息（长度可枚举、摘要可暴力反查）。宁可少记，不可泄露。

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value as Json;

use crate::business::changelog::dto::Action;
use crate::business::changelog::mask::{self, Decision, Policy};

/// 大字段阈值（值序列化后超过它就摘要化）。
///
/// 2048 ≈ 中文 700 字，覆盖 95% 的「短字段」。运维可调的口径在
/// `sys_config: changelog.big_field_bytes`（C6 接入），本卡用常量默认值。
pub const BIG_FIELD_BYTES: usize = 2048;

/// 摘要里保留的预览字符数（按**字符**不是字节，中文 200 字 = 600 字节）。
pub const PREVIEW_CHARS: usize = 200;

/// diff JSON 的 schema 版本（`diff.v`）。加字段靠它分支。
pub const DIFF_SCHEMA_VERSION: u8 = 1;

/// 一种资源的登记条目：表名 + 模块归属 + 审计白名单 + 大字段列。
#[derive(Debug, Clone, Copy)]
pub struct ResourceSpec {
    /// 表名（同时是 `sys_change_log.resource` 的取值，须匹配 `^[a-z][a-z0-9_]{1,63}$`）。
    pub table: &'static str,
    /// 模块归属（`system` / `business` / `support`），由登记表决定，调用方无权覆盖。
    pub module: &'static str,
    /// 审计白名单：只有这些列会进 `diff.fields` / `diff.big`。
    pub fields: &'static [&'static str],
    /// 大字段列（`text` / `bytea` / `jsonb`）：无论多短都摘要化，避免把全文写进热表。
    pub big: &'static [&'static str],
}

/// `sys_user`（系统用户）。
///
/// 白名单挑了「改了会被人追问」的 9 列；`login_fail_count` / `last_login_at` / `lock_time`
/// 是运行时状态，进 diff 只会把审计表变成访问日志。
/// `password` **故意包含**：改密必须留痕，值由 L1 脱敏成 `{"redacted": true}`。
pub const SYS_USER: ResourceSpec = ResourceSpec {
    table: "sys_user",
    module: "system",
    fields: &[
        "username",
        "real_name",
        "email",
        "mobile",
        "dept_id",
        "status",
        "is_locked",
        "remark",
        "password",
    ],
    big: &["remark"],
};

/// `sys_role`（角色）。
pub const SYS_ROLE: ResourceSpec = ResourceSpec {
    table: "sys_role",
    module: "system",
    fields: &[
        "role_code",
        "role_name",
        "description",
        "status",
        // C6 追加：数据范围是提权面（B3），改它必须留字段级凭证
        "data_scope",
        "sort_order",
        "remark",
    ],
    big: &["remark"],
};

/// `sys_announcement`（公告主体，批次 C · C6 接入）。
///
/// 白名单挑了「改了会被追问」的字段：`content_md` 是 Markdown 唯一真源（进 `big` 摘要化），
/// `content_html` / `receiver_count` / `read_count` 是派生与统计列，进 diff 只会制造噪声
/// （扇出与已读每次都变，等于把审计表当访问日志）。`ann_no` 包含在内：编号被改是事故。
pub const SYS_ANNOUNCEMENT: ResourceSpec = ResourceSpec {
    table: "sys_announcement",
    module: "business",
    fields: &[
        "ann_no",
        "title",
        "summary",
        "content_md",
        "level",
        "scope_type",
        "is_top",
        "top_expire_at",
        "status",
        "expire_at",
        "remark",
    ],
    big: &["content_md"],
};

/// `sys_help_doc`（帮助文档，批次 C · C6 接入）。
///
/// `content_md` 进 `big`（正文可达 128 KiB）；`view_count` 是运行时计数、`content_html`
/// 是渲染快照，都不进白名单。`version` 保留：版本号跳变本身是审计线索。
pub const SYS_HELP_DOC: ResourceSpec = ResourceSpec {
    table: "sys_help_doc",
    module: "business",
    fields: &[
        "category_id",
        "title",
        "slug",
        "summary",
        "content_md",
        "keywords",
        "status",
        "sort_order",
        "version",
    ],
    big: &["content_md"],
};

/// `biz_task`（工作任务，批次 C+ · T2 接入）。
///
/// 白名单口径：T2 的状态机 5 列 + **T3 的转派 3 列**。
///
/// | 列 | 谁写 | 为什么必须留痕 |
/// |----|------|----------------|
/// | `status` / `progress` / `done_at` / `reject_reason` / `cancel_reason` | T2 状态机 | 「谁把任务推到了哪一步」 |
/// | `owner_id` | T3 分派 / 转派 | **「原来派给谁」是转派审计的核心**（责任链） |
/// | `assigner_id` | T3 分派 / 转派 | 谁派的活（与「谁干的」分开） |
/// | `source` | T3 转派 | `SELF → ASSIGNED → TRANSFERRED` 的来源演变 |
///
/// 刻意**不含**：
/// - `updated_at` / `update_by`：每次都会变的运行时列，进白名单等于把审计表变成访问日志
///   （与 `SYS_USER` 排除 `last_login_at` 同一个理由）；
/// - `description`：TEXT 大字段，状态流转 / 转派时根本不会变，进白名单只会白占 `big` 摘要；
/// - `title` / `priority`：T3 的编辑接口（`PUT /{id}`）属于「普通编辑」，等 T6 前端
///   详情页一起定稿（避免本卡把白名单放开成「什么都能记」）。
///
/// 登记之后 `biz_task` 的**任何** `record()` 调用都按这份白名单裁剪 ——
/// 想把字段加进审计，改这里（这是刻意的摩擦）。
pub const BIZ_TASK: ResourceSpec = ResourceSpec {
    table: "biz_task",
    module: "business",
    fields: &[
        "status",
        "progress",
        "done_at",
        "reject_reason",
        "cancel_reason",
        "owner_id",
        "assigner_id",
        "source",
    ],
    big: &[],
};

/// 全部登记项。
///
/// **新增资源只能加在这里**（并且必须在同一提交里给 `record()` 的调用点补白名单字段），
/// 否则 `record()` 会以 62012 拒绝——这正是 fail-closed 的目的。
///
/// C6 已追加 `sys_announcement` / `sys_help_doc`（C2/C4 的 DDL 已落地，列名逐字取
/// V0080 / V0082）。**每加一张表，必须在同一提交里给它补上 `record()` 的调用点**，
/// 否则表登记了却永远没有日志 —— `table-registry` 接口会如实显示这份承诺。
pub const REGISTRY: &[&ResourceSpec] = &[
    &SYS_USER,
    &SYS_ROLE,
    &SYS_ANNOUNCEMENT,
    &SYS_HELP_DOC,
    &BIZ_TASK,
];

/// 按表名取登记项；`None` = 未登记（调用方必须拒绝记录）。
///
/// # 参数
///
/// - `table`：表名（小写下划线，区分大小写——库表名就是小写）。
pub fn spec(table: &str) -> Option<&'static ResourceSpec> {
    REGISTRY.iter().find(|entry| entry.table == table).copied()
}

/// 已登记的表名（自检 / 门禁 / 单测用）。
pub fn registered_tables() -> Vec<&'static str> {
    REGISTRY.iter().map(|entry| entry.table).collect()
}

/// 一个字段的前后值（`{b, a}` 成对）。
///
/// `masked` 字段只在为 `true` 时序列化：绝大多数字段没打码，少写 12 个字节 × 数百万行。
#[derive(Debug, Clone, Serialize)]
pub struct FieldPair {
    /// 变更前（新增时为 `null`）。
    pub b: Json,
    /// 变更后（删除时为 `null`）。
    pub a: Json,
    /// 该字段的值是否经过脱敏（L1）。
    #[serde(skip_serializing_if = "is_false")]
    pub masked: bool,
}

/// 大字段的结构化摘要（**不含全文**）。
#[derive(Debug, Clone, Serialize)]
pub struct BigSummary {
    /// 原值的字符数（字符串按字符算，其它按序列化字节数算）。
    pub len: usize,
    /// 原文的 SHA-256（小写 hex）；`redacted` 时为空串。
    pub sha256: String,
    /// 前 [`PREVIEW_CHARS`] 个字符（已脱敏的值则来自脱敏后的文本）。
    pub preview: String,
    /// 是否为「敏感列被整值抹掉」的占位摘要。
    #[serde(skip_serializing_if = "is_false")]
    pub redacted: bool,
}

/// 一个大字段的前后摘要。
#[derive(Debug, Clone, Serialize)]
pub struct BigPair {
    /// 变更前摘要。
    pub b: BigSummary,
    /// 变更后摘要。
    pub a: BigSummary,
    /// 恒为 `true`：进入 `big` 即意味着「原文未被完整记录」。
    pub truncated: bool,
}

/// diff 的 `meta` 段：让一行日志自己说清「这是哪张表的哪条数据、什么动作」。
#[derive(Debug, Clone, Serialize)]
pub struct DiffMeta {
    /// 表名。
    pub table: String,
    /// 业务主键。
    pub pk: String,
    /// 动作（`CREATE` / `UPDATE` / ...）。
    pub action: &'static str,
}

/// 完整的 diff 文档（写进 `sys_change_log.diff` 的 JSONB）。
#[derive(Debug, Clone, Serialize)]
pub struct DiffDoc {
    /// schema 版本。
    pub v: u8,
    /// 记录模式，v1 恒为 `WHITELIST`。
    pub mode: &'static str,
    /// 字段级变化（列名 → 前后值）。
    pub fields: BTreeMap<String, FieldPair>,
    /// 大字段摘要（列名 → 前后摘要）；为空时不序列化（省体积，读侧按缺省 `{}` 处理）。
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub big: BTreeMap<String, BigPair>,
    /// 定位信息。
    pub meta: DiffMeta,
}

impl DiffDoc {
    /// 是否「没有任何变化」（所有白名单字段前后相等）。
    ///
    /// 为 `true` 时 [`crate::business::changelog::record`] 返回
    /// [`crate::business::changelog::dto::RecordOutcome::Skipped`]，**不写空日志**。
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.big.is_empty()
    }

    /// 变化字段名（`fields` ∪ `big`，升序）——直接作为 `changed_fields` 列。
    pub fn changed_fields(&self) -> Vec<String> {
        let mut names: Vec<String> = self.fields.keys().chain(self.big.keys()).cloned().collect();
        names.sort();
        names.dedup();
        names
    }

    /// 序列化成写库的 JSON 文本。
    ///
    /// # 异常情况
    ///
    /// 序列化失败（理论上不可能：全是 `Serialize` 的简单类型）返回 [`serde_json::Error`]。
    pub fn to_json_string(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// 构造 diff 文档（**纯函数**：不查库、不看操作人、无 IO）。
///
/// # 参数
///
/// - `spec`：登记条目（决定白名单与大字段列）；
/// - `policy`：写入侧脱敏策略（`sys_field_mask` + 内置兜底）；
/// - `action`：动作（决定缺失侧的语义：`CREATE` 的 `b` 全为 `null`）；
/// - `pk`：业务主键（写进 `meta.pk`）；
/// - `before` / `after`：两份 JSON（`None` 表示该侧不存在）。
///
/// # 返回值
///
/// 只含变化字段的 [`DiffDoc`]；无变化时 `is_empty()` 为 `true`。
///
/// # 字段缺失的读法
///
/// 对象里**没有这个键**与「键存在但值为 null」都按 `null` 处理：登记表现在是静态列表，
/// 拿不到列类型，区分二者只会制造两种「空」而收益为零。
pub fn build(
    spec: &ResourceSpec,
    policy: &Policy,
    action: Action,
    pk: &str,
    before: Option<&Json>,
    after: Option<&Json>,
) -> DiffDoc {
    let mut fields = BTreeMap::new();
    let mut big = BTreeMap::new();

    for column in spec.fields {
        let column = *column;
        let b = before
            .and_then(|doc| doc.get(column))
            .cloned()
            .unwrap_or(Json::Null);
        let a = after
            .and_then(|doc| doc.get(column))
            .cloned()
            .unwrap_or(Json::Null);
        if b == a {
            continue; // 铁律 1：值相等不记
        }
        let decisions = policy.decisions(column);
        let is_big = spec.big.contains(&column)
            || payload_len(&b) > BIG_FIELD_BYTES
            || payload_len(&a) > BIG_FIELD_BYTES;
        // 敏感优先于大字段：敏感列不进 big（摘要里的 len / sha256 对低熵秘密是可用信息）
        let redacted = decisions.iter().any(|d| matches!(d, Decision::Redact));

        if is_big && !redacted {
            big.insert(
                column.to_string(),
                BigPair {
                    b: summarize(&b, &decisions),
                    a: summarize(&a, &decisions),
                    truncated: true,
                },
            );
        } else {
            let (b_masked, b_flag) = mask::apply(&decisions, &b);
            let (a_masked, a_flag) = mask::apply(&decisions, &a);
            fields.insert(
                column.to_string(),
                FieldPair {
                    b: b_masked,
                    a: a_masked,
                    masked: b_flag || a_flag,
                },
            );
        }
    }

    DiffDoc {
        v: DIFF_SCHEMA_VERSION,
        mode: "WHITELIST",
        fields,
        big,
        meta: DiffMeta {
            table: spec.table.to_string(),
            pk: pk.to_string(),
            action: action.code(),
        },
    }
}

/// 值的「长度」：字符串按字符数，其它按序列化字节数。
fn payload_len(value: &Json) -> usize {
    match value {
        Json::Null => 0,
        Json::String(text) => text.chars().count(),
        other => other.to_string().len(),
    }
}

/// 把一个值压缩成 [`BigSummary`]。
fn summarize(value: &Json, decisions: &[Decision]) -> BigSummary {
    if value.is_null() {
        return BigSummary {
            len: 0,
            sha256: String::new(),
            preview: String::new(),
            redacted: false,
        };
    }
    let text = match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    };
    let (masked, _) = mask::apply(decisions, value);
    let shown = match &masked {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    };
    BigSummary {
        len: text.chars().count(),
        // 摘要算的是**原文**：它就是「内容是否真的变了」的判据；脱敏只影响展示用的 preview
        sha256: mask::sha256_hex(&text),
        preview: shown.chars().take(PREVIEW_CHARS).collect(),
        redacted: false,
    }
}

/// `serde` 的 `skip_serializing_if` 谓词。
fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 白名单字段前后相等 → 不进 diff（铁律 1）。
    #[test]
    fn diff_skips_unchanged_fields() {
        let policy = Policy::builtin_only("sys_user");
        let before = json!({"username": "alice", "real_name": "张三", "status": 1});
        let after = json!({"username": "alice", "real_name": "张三", "status": 0});
        let doc = build(
            &SYS_USER,
            &policy,
            Action::Update,
            "u-1",
            Some(&before),
            Some(&after),
        );
        assert_eq!(doc.changed_fields(), vec!["status".to_string()]);
        assert!(!doc.fields.contains_key("username"));
        assert!(!doc.fields.contains_key("real_name"));
        assert_eq!(doc.fields["status"].b, json!(1));
        assert_eq!(doc.fields["status"].a, json!(0));
    }

    /// 新增：`b` 侧全为 null；删除：`a` 侧全为 null（铁律 2）。
    #[test]
    fn diff_marks_absent_side_as_null() {
        let policy = Policy::builtin_only("sys_user");
        let after = json!({"username": "alice", "status": 1});
        let created = build(
            &SYS_USER,
            &policy,
            Action::Create,
            "u-1",
            None,
            Some(&after),
        );
        assert_eq!(created.fields["username"].b, Json::Null);
        assert_eq!(created.fields["username"].a, json!("alice"));
        assert!(!created.fields["username"].masked);

        let deleted = build(
            &SYS_USER,
            &policy,
            Action::Delete,
            "u-1",
            Some(&after),
            None,
        );
        assert_eq!(deleted.fields["username"].a, Json::Null);
        assert_eq!(deleted.fields["username"].b, json!("alice"));
    }

    /// 全字段相等 → 空文档（`record` 据此跳过写库）。
    #[test]
    fn empty_when_nothing_changed() {
        let policy = Policy::builtin_only("sys_user");
        let same = json!({"username": "alice", "status": 1});
        let doc = build(
            &SYS_USER,
            &policy,
            Action::Update,
            "u-1",
            Some(&same),
            Some(&same),
        );
        assert!(doc.is_empty());
        assert!(doc.changed_fields().is_empty());
        assert_eq!(
            doc.to_json_string().unwrap(),
            r#"{"v":1,"mode":"WHITELIST","fields":{},"meta":{"table":"sys_user","pk":"u-1","action":"UPDATE"}}"#
        );
    }

    /// 阈值边界：2048 字符不摘要、2049 字符摘要；登记为大字段的列无论多短都摘要。
    #[test]
    fn big_threshold_boundary() {
        let policy = Policy::builtin_only("sys_role"); // 无敏感内置列，避免脱敏干扰
        let at = "甲".repeat(BIG_FIELD_BYTES);
        let at2 = "乙".repeat(BIG_FIELD_BYTES);
        let doc = build(
            &SYS_ROLE,
            &policy,
            Action::Update,
            "r-1",
            Some(&json!({"description": at})),
            Some(&json!({"description": at2})),
        );
        assert!(
            doc.fields.contains_key("description"),
            "恰好 2048 字符仍走 fields"
        );
        assert!(doc.big.is_empty());

        let over = "甲".repeat(BIG_FIELD_BYTES + 1);
        let over2 = "乙".repeat(BIG_FIELD_BYTES + 1);
        let doc = build(
            &SYS_ROLE,
            &policy,
            Action::Update,
            "r-1",
            Some(&json!({"description": over})),
            Some(&json!({"description": over2})),
        );
        assert!(doc.big.contains_key("description"), "超过阈值必须摘要化");
        assert_eq!(doc.big["description"].a.len, BIG_FIELD_BYTES + 1);
        assert!(doc.big["description"].truncated);

        // 登记为大字段的列：短值也摘要（remark 在 sys_role.big 里）
        let doc = build(
            &SYS_ROLE,
            &policy,
            Action::Update,
            "r-1",
            Some(&json!({"remark": "旧"})),
            Some(&json!({"remark": "新"})),
        );
        assert!(doc.big.contains_key("remark"));
        assert_eq!(doc.big["remark"].a.preview, "新");
    }

    /// 未登记的表 → `spec()` 返回 None（调用方据此报 62012）。
    #[test]
    fn unregistered_table_has_no_spec() {
        assert!(spec("sys_not_registered").is_none());
        assert!(spec("").is_none());
        assert!(spec("SYS_USER").is_none(), "表名区分大小写");
        assert_eq!(spec("sys_user").map(|s| s.module), Some("system"));
        let tables = registered_tables();
        assert!(tables.contains(&"sys_user"));
        assert!(tables.contains(&"sys_role"));
    }

    /// 登记表的字段名必须是小写下划线（否则 GIN `changed_fields` 反查会漏）。
    #[test]
    fn registered_fields_are_lowercase_columns() {
        for entry in REGISTRY {
            assert!(
                entry
                    .table
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "表名 {} 不是小写下划线",
                entry.table
            );
            for column in entry.fields {
                assert!(
                    column
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{} 的字段 {column} 不是小写下划线",
                    entry.table
                );
            }
            for column in entry.big {
                assert!(
                    entry.fields.contains(column),
                    "{} 的大字段 {column} 必须在白名单里",
                    entry.table
                );
            }
        }
    }

    /// 敏感 + 大字段 → 进 `fields` 并整值抹掉，**不**进 `big`（不给 len / sha256）。
    #[test]
    fn sensitive_big_field_goes_redacted_not_big() {
        let policy = Policy::builtin_only("sys_user");
        let long = format!("token-{}", "x".repeat(BIG_FIELD_BYTES + 10));
        // password 是内置 Redact 且长度超阈值
        let doc = build(
            &SYS_USER,
            &policy,
            Action::Update,
            "u-1",
            Some(&json!({"password": "old-hash"})),
            Some(&json!({"password": long})),
        );
        assert!(doc.big.is_empty(), "敏感列不得进 big（摘要含 len/sha）");
        assert!(doc.fields.contains_key("password"));
        assert_eq!(doc.fields["password"].a, json!({"redacted": true}));
        assert!(doc.fields["password"].masked);
        let text = doc.to_json_string().unwrap();
        assert!(!text.contains("old-hash"));
        assert!(!text.contains("xxxxxxxxxx"));
    }

    /// 大字段摘要里不含全文：只有前 200 字符的预览。
    #[test]
    fn big_preview_is_truncated_to_200_chars() {
        let policy = Policy::builtin_only("sys_role");
        let full = "甲".repeat(3000);
        let doc = build(
            &SYS_ROLE,
            &policy,
            Action::Update,
            "r-1",
            Some(&json!({"remark": "旧"})),
            Some(&json!({"remark": full})),
        );
        let summary = &doc.big["remark"].a;
        assert_eq!(summary.len, 3000);
        assert_eq!(summary.preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(summary.sha256.len(), 64);
        let text = doc.to_json_string().unwrap();
        assert!(!text.contains(&full), "全文不得入库");
        assert!(
            !text.contains(&"甲".repeat(PREVIEW_CHARS + 1)),
            "预览必须截断"
        );
    }
}
