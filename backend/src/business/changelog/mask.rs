//! 写入侧脱敏 L1（批次 C · C5）：`sys_field_mask` + 12 个内置兜底字段名，**无条件**执行。
//!
//! # D13：库里永不存明文，与「谁改的」无关
//!
//! 一个常见的错误设计是「超管有明文权限，所以超管操作时日志也存明文」。这会直接制造后门：
//! 超管把某人的手机号从 `13800001111` 改成 `13800002222` → 日志里存明文 →
//! **任何能看日志的人（哪怕只有 `system:change-log:list` 的普通审计员）都能拿到明文**。
//!
//! 因此本模块**没有任何「操作人」参数**——不是「忘了传」，而是签名层面就拒绝：
//! [`Policy::decisions`] 只吃列名。想给超管开例外，就必须改这个文件的签名，
//! 而那一定会在 code review 里被看见。
//!
//! # 判定顺序（架构 §4.7.2 L1）
//!
//! 1. `sys_field_mask` 里登记的 `(table, column)` 规则（**任意角色**，多角色规则取「最严」）；
//! 2. 内置兜底：12 个字段名，逐字对齐 `middleware/operation_log.rs` 的 `SENSITIVE_FIELDS`
//!    （`password / new_password / old_password / confirm_password / mobile / phone /
//!    id_card / idcard / token / access_token / refresh_token / secret`）；
//! 3. 都没命中 → 原样记录。
//!
//! # fail-closed：脱敏失败绝不回退明文
//!
//! 规则解析不到（`mask_type` 被直接改库改坏）或参数非法时，值被替换成
//! `{"redacted": true}` 并记 `tracing::error!`——**业务继续**（日志是旁路产物，
//! 不该因为脱敏规则没配好就让「改个用户资料」失败），但值一定被抹掉。
//! 这与批次 B 导出路径的「422 拒绝导出」是**有意的差异**：那边产物本身就是数据，
//! 这边产物是日志。
//!
//! # 为什么不用 `masking::registry::MaskSet`
//!
//! `MaskSet` 的口径是「某个用户能看到什么」，带豁免与角色合并语义；L1 要的是
//! 「这个列是不是敏感、怎么打码」，与用户无关。复用 `MaskSet` 反而要假装一个用户，
//! 一旦谁把 `is_exempt()` 传成 true 就会静默存明文。所以这里只复用**纯函数**
//! [`crate::business::masking::masker::apply`]（七种类型的算法唯一实现）。

use alun::prelude::*;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::business::masking::masker::{self, MaskType};

/// 内置兜底字段名（12 个，与 `middleware/operation_log.rs` 的 `SENSITIVE_FIELDS` 逐字一致）。
///
/// 顺序也一致——改这里的唯一正当理由是那边的列表变了，而那时两边必须一起改
/// （`tests::builtin_list_matches_operation_log` 会盯着字段名集合）。
pub const BUILTIN_SENSITIVE: &[&str] = &[
    "password",
    "new_password",
    "old_password",
    "confirm_password",
    "mobile",
    "phone",
    "id_card",
    "idcard",
    "token",
    "access_token",
    "refresh_token",
    "secret",
];

/// 内置兜底字段的脱敏方式。
///
/// 我们只在内置清单里分两类：**联系方式类按掩码**（保留首尾，让人能核对「改的是不是同一个号」），
/// **凭据类整体打成 `{"redacted": true}`**（连长度都不给——`token` 的长度也是信息）。
fn builtin_decision(column: &str) -> Option<Decision> {
    match column {
        "mobile" | "phone" => Some(Decision::Rule {
            ty: MaskType::Mobile,
            param: None,
        }),
        "id_card" | "idcard" => Some(Decision::Rule {
            ty: MaskType::IdCard,
            param: None,
        }),
        "password" | "new_password" | "old_password" | "confirm_password" | "token"
        | "access_token" | "refresh_token" | "secret" => Some(Decision::Redact),
        _ => None,
    }
}

/// 一条列规则的处置方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 整值抹掉，只留 `{"redacted": true}`（凭据类）。
    Redact,
    /// 按给定类型 + 参数打码。
    Rule {
        /// 七种脱敏类型之一。
        ty: MaskType,
        /// 类型参数（`"3,4"` / `"1"` / `"pattern|group"`），`None` 用默认值。
        param: Option<String>,
    },
}

/// `sys_field_mask` 里的一行（只取 L1 需要的三列）。
#[derive(Debug, Clone)]
pub struct ColumnRule {
    /// 列名（小写下划线）。
    pub column: String,
    /// 脱敏类型（库里的 `mask_type`，`1..=7`）。
    pub mask_type: i16,
    /// 类型参数。
    pub param: Option<String>,
}

/// 某张表在**写入侧**生效的脱敏策略。
///
/// 与 `masking::registry::MaskSet` 的区别见模块头最后一段：本结构没有「用户」，因此**没有豁免路径**。
#[derive(Debug, Clone, Default)]
pub struct Policy {
    table: String,
    rules: Vec<ColumnRule>,
}

impl Policy {
    /// 只用内置兜底（不查库）。单测与「无库环境」用。
    ///
    /// # 参数
    ///
    /// - `table`：表名（只用于日志与 `meta`，不参与判定顺序 2）。
    pub fn builtin_only(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            rules: Vec::new(),
        }
    }

    /// 用给定的库内规则构造（`sys_field_mask` 的查询结果）。
    ///
    /// # 参数
    ///
    /// - `table`：表名；
    /// - `rules`：该表所有启用规则的 `(column, mask_type, param)`。
    pub fn new(table: impl Into<String>, rules: Vec<ColumnRule>) -> Self {
        Self {
            table: table.into(),
            rules,
        }
    }

    /// 从 `sys_field_mask` 载入某张表的启用规则（**用调用方给的 executor**）。
    ///
    /// # 为什么也吃 `&impl SqlExecutor`
    ///
    /// 这条 SELECT 必须与业务写、与日志 INSERT 处在**同一个事务快照**里：规则如果在事务外
    /// 读，就会出现「读到别人刚改的规则、却写在回滚了的事务里」这种不可解释的组合。
    /// 也正因为它是同事务读，本模块**绝不**调用 `db()`（门禁 G2）。
    ///
    /// # 参数
    ///
    /// - `ex`：业务事务的执行器；
    /// - `table`：表名。
    ///
    /// # 异常情况
    ///
    /// 数据库异常返回 [`DbError`]。**注意**：调用方（[`crate::business::changelog::record`]）
    /// 会把该错误向上抛，从而让整个业务事务回滚——这是同事务设计的必然结果
    /// （T-CL-12：日志写失败必须连业务一起回滚，而不是留一条「业务成功但无凭证」的记录）。
    pub async fn load(ex: &impl SqlExecutor, table: &str) -> DbResult<Self> {
        // 不按角色过滤：写入侧脱敏不看操作人（D13），多角色规则在 apply 阶段取最严。
        let rows = ex
            .query_sql(
                "SELECT column_name, mask_type, mask_param FROM sys_field_mask \
                 WHERE table_name = ?::text AND status = 1 AND deleted_at IS NULL \
                 ORDER BY column_name, mask_type, id"
                    .to_string(),
                vec![Value::from(table)],
            )
            .await?;
        let mut rules = Vec::with_capacity(rows.len());
        for row in &rows {
            rules.push(ColumnRule {
                column: row.get_typed::<String>("column_name")?,
                mask_type: row.get_typed::<i16>("mask_type")?,
                param: row.get_typed::<Option<String>>("mask_param")?,
            });
        }
        Ok(Self {
            table: table.to_string(),
            rules,
        })
    }

    /// 表名（日志与 `meta` 用）。
    pub fn table(&self) -> &str {
        &self.table
    }

    /// 该列在写入侧的全部处置方式（库内规则优先于内置兜底，可能多条）。
    ///
    /// 多条只可能来自「同一列按不同角色配了不同规则」；[`apply`] 会挑最严的一条，
    /// 因为**脱敏是限制，取并集才安全**（与批次 B §3.1 的合并口径一致）。
    ///
    /// # 参数
    ///
    /// - `column`：列名。
    pub fn decisions(&self, column: &str) -> Vec<Decision> {
        let from_db: Vec<Decision> = self
            .rules
            .iter()
            .filter(|rule| rule.column.eq_ignore_ascii_case(column))
            .map(|rule| match MaskType::from_i16(rule.mask_type) {
                Some(ty) => Decision::Rule {
                    ty,
                    param: rule.param.clone(),
                },
                // 库里 mask_type 越界（只可能被直接改库绕过 CHECK）→ 按「整值抹掉」处理，
                // 并留 error 日志：宁可少记信息，也不许把明文写进审计表。
                None => {
                    tracing::error!(
                        table = %self.table,
                        column = %column,
                        mask_type = rule.mask_type as i64,
                        "sys_field_mask.mask_type 非法，按 redacted 处理"
                    );
                    Decision::Redact
                }
            })
            .collect();
        if !from_db.is_empty() {
            return from_db;
        }
        builtin_decision(column).into_iter().collect()
    }

    /// 该列是否敏感（任一判定命中）。
    ///
    /// # 参数
    ///
    /// - `column`：列名。
    pub fn is_sensitive(&self, column: &str) -> bool {
        !self.decisions(column).is_empty()
    }
}

/// 对单个 JSON 值应用脱敏。
///
/// # 参数
///
/// - `decisions`：[`Policy::decisions`] 的结果（空 = 不敏感，原样返回）；
/// - `raw`：原始值。
///
/// # 返回值
///
/// `(脱敏后的值, 是否发生过脱敏)`。第二个值进 diff 的 `masked: true`。
///
/// # 规则
///
/// - 有多条候选时取**最严**（打码后可见字符最少的那条；并列取先出现的，顺序由 SQL 的
///   `ORDER BY` 固定，因此结果可复现）；
/// - `null` 原样返回且不算脱敏（`null` 不是信息）；
/// - 对象 / 数组带敏感列名 → 整体 `{"redacted": true}`（不递归，递归容易被构造的嵌套结构绕过）；
/// - 脱敏函数报错 → `{"redacted": true}` + `tracing::error!`（fail-closed，绝不回退明文）。
pub fn apply(decisions: &[Decision], raw: &Json) -> (Json, bool) {
    if decisions.is_empty() || raw.is_null() {
        return (raw.clone(), false);
    }
    if decisions.iter().any(|d| matches!(d, Decision::Redact)) {
        return (json!({ "redacted": true }), true);
    }
    // 只有标量能「打码」；对象 / 数组一律整值抹掉（它们无法被 masker 处理，硬转会漏明文）。
    let raw_text = match raw {
        Json::String(text) => text.clone(),
        Json::Number(number) => number.to_string(),
        Json::Bool(value) => value.to_string(),
        _ => return (json!({ "redacted": true }), true),
    };

    let mut best: Option<String> = None;
    for decision in decisions {
        let Decision::Rule { ty, param } = decision else {
            continue;
        };
        match masker::apply(*ty, param.as_deref(), &raw_text) {
            Ok(masked) => {
                let better = match &best {
                    Some(current) => visible_chars(&masked) < visible_chars(current),
                    None => true,
                };
                if better {
                    best = Some(masked);
                }
            }
            Err(error) => {
                // fail-closed：规则坏了就不记这个值（业务继续，值一定被抹掉）
                tracing::error!(
                    error = ?error,
                    "变更日志脱敏失败（规则参数非法），该字段按 redacted 记录"
                );
                return (json!({ "redacted": true }), true);
            }
        }
    }
    match best {
        Some(masked) => (Json::String(masked), true),
        // 理论上不可达（decisions 非空且全是 Rule），保守起见按抹掉处理
        None => (json!({ "redacted": true }), true),
    }
}

/// 可见字符数（非 `*` 的字符），用于在候选规则间挑「最严」。
fn visible_chars(text: &str) -> usize {
    text.chars().filter(|c| *c != '*').count()
}

/// 字符串的 SHA-256（小写 hex）。
///
/// 用 `sha256` 而不只是长度：改一个字时长度可能不变，审计员无法分辨；
/// 摘要能分辨，还能顺手识别「同一内容重复修改」（`b.sha256 == a.sha256`）。
///
/// # 参数
///
/// - `text`：原文。
pub fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置清单必须与 `operation_log.rs` 的 12 个字段名逐字一致（顺序无关，集合相等）。
    #[test]
    fn builtin_list_matches_operation_log() {
        let mut ours: Vec<&str> = BUILTIN_SENSITIVE.to_vec();
        ours.sort_unstable();
        let mut theirs = vec![
            "password",
            "new_password",
            "old_password",
            "confirm_password",
            "mobile",
            "phone",
            "id_card",
            "idcard",
            "token",
            "access_token",
            "refresh_token",
            "secret",
        ];
        theirs.sort_unstable();
        assert_eq!(ours, theirs);
        assert_eq!(ours.len(), 12);
        // 每个内置字段都必须有处置方式（不能「列了名却没规则」）
        for name in BUILTIN_SENSITIVE {
            let policy = Policy::builtin_only("sys_user");
            assert!(
                policy.is_sensitive(name),
                "内置敏感字段 {name} 没有对应脱敏方式"
            );
        }
    }

    /// 手机号走掩码、口令类走整值抹掉。
    #[test]
    fn builtin_decisions_by_kind() {
        let policy = Policy::builtin_only("sys_user");
        assert_eq!(
            policy.decisions("mobile"),
            vec![Decision::Rule {
                ty: MaskType::Mobile,
                param: None
            }]
        );
        assert_eq!(policy.decisions("password"), vec![Decision::Redact]);

        let (value, masked) = apply(&policy.decisions("mobile"), &json!("13812345678"));
        assert_eq!(value, json!("138****5678"));
        assert!(masked);

        let (value, masked) = apply(&policy.decisions("password"), &json!("$2b$12$abcdef"));
        assert_eq!(value, json!({"redacted": true}));
        assert!(masked);
        assert!(!value.to_string().contains("abcdef"));
    }

    /// 非敏感列原样返回、不算脱敏；null 也不动。
    #[test]
    fn plain_and_null_untouched() {
        let policy = Policy::builtin_only("sys_user");
        let (value, masked) = apply(&policy.decisions("real_name"), &json!("张三"));
        assert_eq!(value, json!("张三"));
        assert!(!masked);
        let (value, masked) = apply(&policy.decisions("mobile"), &Json::Null);
        assert_eq!(value, Json::Null);
        assert!(!masked);
    }

    /// 同一列多条规则（不同角色配了不同参数）取最严：保留位数越少越严。
    #[test]
    fn strictest_rule_wins() {
        let policy = Policy::new(
            "sys_user",
            vec![
                ColumnRule {
                    column: "mobile".to_string(),
                    mask_type: 1,
                    param: Some("3,4".to_string()),
                },
                ColumnRule {
                    column: "mobile".to_string(),
                    mask_type: 1,
                    param: Some("0,0".to_string()),
                },
            ],
        );
        let (value, masked) = apply(&policy.decisions("mobile"), &json!("13812345678"));
        assert!(masked);
        assert_eq!(value, json!("****"), "应取保留 0 前 0 后的最严规则");
    }

    /// 库内规则优先于内置兜底：`sys_field_mask` 把 email 登记成邮箱脱敏后，
    /// 内置清单里没有 email 也照样打码。
    #[test]
    fn db_rule_beats_builtin() {
        let policy = Policy::new(
            "sys_user",
            vec![ColumnRule {
                column: "email".to_string(),
                mask_type: 3,
                param: None,
            }],
        );
        let (value, masked) = apply(&policy.decisions("email"), &json!("alice@mail.com"));
        assert!(masked);
        assert_eq!(value, json!("a***@mail.com"));
    }

    /// 规则参数非法 → fail-closed（redacted），绝不回退明文。
    #[test]
    fn broken_rule_never_leaks_plaintext() {
        let policy = Policy::new(
            "sys_user",
            vec![ColumnRule {
                column: "mobile".to_string(),
                mask_type: 1,
                param: Some("不是数字".to_string()),
            }],
        );
        let (value, masked) = apply(&policy.decisions("mobile"), &json!("13812345678"));
        assert!(masked);
        assert_eq!(value, json!({"redacted": true}));
        assert!(!value.to_string().contains("13812345678"));
    }

    /// 对象 / 数组带敏感列名 → 整值抹掉（不递归、不给明文留缝）。
    #[test]
    fn non_scalar_sensitive_value_redacted() {
        let policy = Policy::builtin_only("sys_user");
        let raw = json!({"nested": "13812345678"});
        let (value, masked) = apply(&policy.decisions("mobile"), &raw);
        assert!(masked);
        assert_eq!(value, json!({"redacted": true}));
        assert!(!value.to_string().contains("13812345678"));
    }

    /// sha256 稳定、64 位小写 hex，且改一个字就变。
    #[test]
    fn sha256_is_stable_and_sensitive() {
        assert_eq!(sha256_hex("abc").len(), 64);
        assert_eq!(sha256_hex("abc"), sha256_hex("abc"));
        assert_ne!(sha256_hex("abc"), sha256_hex("abd"));
        assert!(
            sha256_hex("abc")
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
