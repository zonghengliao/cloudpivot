//! 用户偏好体系：字典读取、用户值读写、以及**唯一的**校验入口。
//!
//! # 为什么校验规则写在这里而不是 handler
//!
//! 「未知键拒绝 / 类型校验 / 值域校验」是**领域规则**，不是编排细节：GET 合并默认值、
//! PUT 落库、将来批量导入偏好都走同一套规则。规则只有一份，才不会出现「PUT 校验了、
//! 管理端导入没校验」这种静默不一致。
//!
//! # 三个纯函数（不碰库，单测覆盖）
//!
//! | 函数 | 职责 |
//! |------|------|
//! | [`validate_patch`] | 逐 key 按字典校验（未知键 / 类型 / oneof / range） |
//! | [`assemble`] | 把「字典默认值 + 用户已存值」合并成 GET / PUT 的响应体 |
//! | [`Validator::parse`] / [`Validator::check`] | `oneof:` / `range:` 规则的解析与判定 |
//!
//! # 数据权限
//!
//! SCOPE-EXEMPT: 偏好是「用户对自己的设置」，行级隔离就是 `user_id = 当前登录用户`
//! （由 `AuthClaims.sub` 派生，**永远**来自令牌，不来自请求体）。它没有部门归属列、
//! 也不该被数据范围裁剪 —— 给偏好注入 `scope::` 会让「自定义范围的用户读不到自己的配色」。
//!
//! # 为什么不做字典缓存
//!
//! 字典表只有个位数行、查询走主键顺序扫描，单次 < 1 ms；而缓存会引入「改了字典要等
//! 过期」的运维复杂度。等偏好项真的上百条再谈缓存（届时加 `Cache` + 显式失效即可）。

use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use crate::shared::error_code;
use crate::system::preference::dto::PreferenceRes;

/// 字典读取 SQL：只取启用项，按 `sort_order` 升序（= 前端设置页展示顺序）。
const DEF_SELECT: &str = "SELECT pref_key, pref_name, value_type, default_value::text AS default_value, \
     scope, validator, sort_order \
     FROM sys_preference_def WHERE status = 1 ORDER BY sort_order ASC, pref_key ASC";

/// 用户值读取 SQL：`jsonb` 一律 `::text` 取出、应用层反序列化
/// （与 `business/impexp/model.rs` 同一口径，不依赖驱动的 JSONB 绑定细节）。
const VALUE_SELECT: &str = "SELECT pref_key, pref_value::text AS pref_value \
     FROM sys_user_preference WHERE user_id = ?::uuid";

/// 幂等 upsert：同键再写只覆盖值 + 更新时间，不产生第二行。
const VALUE_UPSERT: &str = "INSERT INTO sys_user_preference (user_id, pref_key, pref_value, updated_at) \
     VALUES (?::uuid, ?, ?::jsonb, now()) \
     ON CONFLICT (user_id, pref_key) \
     DO UPDATE SET pref_value = EXCLUDED.pref_value, updated_at = now()";

// ==================== 字典定义 ====================

/// 一条偏好字典定义（`sys_preference_def` 的启用行）。
#[derive(Debug, Clone)]
pub struct PreferenceDef {
    /// 偏好键（`ui.palette` 这种形态）。
    pub pref_key: String,
    /// 中文名（错误文案与前端展示都用它）。
    pub pref_name: String,
    /// 值类型：`string` / `number` / `boolean` / `json`（库里有 CHECK 兜底）。
    pub value_type: String,
    /// 默认值的 JSON **文本**（`jsonb::text`），由 [`PreferenceDef::default_value`] 解析。
    ///
    /// 存文本而不是 `JsonValue`：解析失败要能报「字典配错了」（500 + 62024），
    /// 而不是在 `load_defs` 里退化成一个 `DbError` 把业务语义丢掉。
    pub default_value_text: String,
    /// 分组：`ui` / `table` / `notify`。
    pub scope: String,
    /// 值域校验规则原文（`oneof:a,b,c` / `range:5-200`）。
    pub validator: Option<String>,
    /// 展示顺序。
    pub sort_order: i32,
}

impl PreferenceDef {
    /// 解析默认值 JSON 文本。
    ///
    /// # 异常情况
    ///
    /// 文本不是合法 JSON（字典被手工改坏）→ `500 + 62024`，消息里带键名，便于运维定位。
    pub fn default_value(&self) -> std::result::Result<JsonValue, ApiError> {
        serde_json::from_str(&self.default_value_text).map_err(|e| {
            tracing::error!(
                pref_key = %self.pref_key,
                raw = %self.default_value_text,
                error = %e,
                "偏好字典的 default_value 不是合法 JSON"
            );
            ApiError::new(
                500,
                error_code::PREFERENCE_DEF_INVALID,
                format!("偏好项「{}」的默认值配置损坏，请联系管理员", self.pref_name),
            )
        })
    }
}

/// 值类型（`value_type` 列的强类型映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// 字符串。
    String,
    /// 数字。
    Number,
    /// 布尔。
    Boolean,
    /// 任意 JSON（对象 / 数组 / 字符串 / 数字 / 布尔）。
    Json,
}

impl ValueType {
    /// 解析 `value_type` 列。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// 中文名（错误文案用）。
    pub fn label(self) -> &'static str {
        match self {
            Self::String => "字符串",
            Self::Number => "数字",
            Self::Boolean => "布尔",
            Self::Json => "JSON",
        }
    }

    /// 该 JSON 值是否满足本类型。
    ///
    /// `null` 一律不接受：`sys_user_preference.pref_value` 是 `NOT NULL`，
    /// 且「把偏好设成 null」在语义上等于「没设置」，应该走删除而不是写入。
    pub fn is_instance(self, value: &JsonValue) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Json => !value.is_null(),
        }
    }
}

/// 值域校验规则（`validator` 列的强类型映射）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validator {
    /// 枚举：值必须是给定字符串之一。
    OneOf(Vec<String>),
    /// 闭区间：值必须是整数且 `min <= v <= max`。
    Range(i64, i64),
}

impl Validator {
    /// 解析 `validator` 列。
    ///
    /// # 返回值
    ///
    /// - `Ok(None)`：空规则（`NULL` / 空串），只按类型校验；
    /// - `Ok(Some(rule))`：解析成功；
    /// - `Err(500 + 62024)`：规则写坏（字典配置缺陷，必须报警而不是静默放行）。
    pub fn parse(raw: &str) -> std::result::Result<Option<Self>, ApiError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let broken = || {
            ApiError::new(
                500,
                error_code::PREFERENCE_DEF_INVALID,
                "偏好项的校验规则配置损坏，请联系管理员",
            )
        };
        if let Some(list) = raw.strip_prefix("oneof:") {
            let items: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
            if items.is_empty() {
                tracing::error!(rule = raw, "偏好校验规则 oneof 取值为空");
                return Err(broken());
            }
            return Ok(Some(Self::OneOf(items)));
        }
        if let Some(bounds) = raw.strip_prefix("range:") {
            let mut parts = bounds.splitn(2, '-');
            let min = parts.next().and_then(|v| v.trim().parse::<i64>().ok());
            let max = parts.next().and_then(|v| v.trim().parse::<i64>().ok());
            match (min, max) {
                (Some(min), Some(max)) if min <= max => return Ok(Some(Self::Range(min, max))),
                _ => {
                    tracing::error!(rule = raw, "偏好校验规则 range 无法解析");
                    return Err(broken());
                }
            }
        }
        tracing::error!(rule = raw, "未知的偏好校验规则前缀");
        Err(broken())
    }

    /// 校验单个值。
    ///
    /// # 异常情况
    ///
    /// 不满足规则 → `400 + 62022`（消息里写清规则与当前值）。
    pub fn check(
        &self,
        def: &PreferenceDef,
        value: &JsonValue,
    ) -> std::result::Result<(), ApiError> {
        match self {
            Self::OneOf(allowed) => {
                let hit = value
                    .as_str()
                    .map(|text| allowed.iter().any(|item| item == text))
                    .unwrap_or(false);
                if hit {
                    return Ok(());
                }
                Err(ApiError::new(
                    400,
                    error_code::PREFERENCE_VALUE_INVALID,
                    format!(
                        "偏好项「{}」的取值只能是 {}（当前：{}）",
                        def.pref_name,
                        allowed.join(" / "),
                        value
                    ),
                ))
            }
            Self::Range(min, max) => {
                // 区间校验只接受整数：`ui.page_size = 10.5` 这类值在库里是合法的
                // number，但对「每页条数」没有意义，必须在入口挡掉。
                match value.as_i64() {
                    Some(number) if number >= *min && number <= *max => Ok(()),
                    _ => Err(ApiError::new(
                        400,
                        error_code::PREFERENCE_VALUE_INVALID,
                        format!(
                            "偏好项「{}」必须是 {min}–{max} 之间的整数（当前：{}）",
                            def.pref_name, value
                        ),
                    )),
                }
            }
        }
    }
}

// ==================== 读 ====================

/// 读取全部启用中的偏好字典（按 `sort_order` 升序）。
///
/// # 参数
///
/// - `ex`：数据库执行器（连接或事务）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn load_defs(ex: &impl SqlExecutor) -> DbResult<Vec<PreferenceDef>> {
    let rows = ex.query_sql(DEF_SELECT.to_string(), Vec::new()).await?;
    let mut defs = Vec::with_capacity(rows.len());
    for row in &rows {
        defs.push(PreferenceDef {
            pref_key: row.get_typed("pref_key")?,
            pref_name: row.get_typed("pref_name")?,
            value_type: row.get_typed("value_type")?,
            default_value_text: row.get_typed("default_value")?,
            scope: row.get_typed("scope")?,
            validator: row.get_typed("validator")?,
            sort_order: row.get_typed("sort_order")?,
        });
    }
    Ok(defs)
}

/// 读取某用户**已显式设置**的偏好值（不含默认值）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `user_id`：用户 ID（只能来自 `AuthClaims`）。
pub async fn load_values(
    ex: &impl SqlExecutor,
    user_id: &Uuid,
) -> DbResult<BTreeMap<String, JsonValue>> {
    let rows = ex
        .query_sql(VALUE_SELECT.to_string(), vec![Value::from(*user_id)])
        .await?;
    let mut values = BTreeMap::new();
    for row in &rows {
        let key: String = row.get_typed("pref_key")?;
        let text: String = row.get_typed("pref_value")?;
        // 库里是 jsonb，取出必定是合法 JSON；真解析失败说明行被外部改坏，
        // 直接跳过这一项（用字典默认值兜底），不让一行脏数据打挂整个设置页。
        match serde_json::from_str::<JsonValue>(&text) {
            Ok(value) => {
                values.insert(key, value);
            }
            Err(e) => {
                tracing::warn!(
                    user_id = %user_id,
                    pref_key = %key,
                    raw = %text,
                    error = %e,
                    "用户偏好行不是合法 JSON，已忽略并按默认值返回"
                );
            }
        }
    }
    Ok(values)
}

// ==================== 写 ====================

/// 批量 upsert（同一事务内逐键写；幂等）。
///
/// # 参数
///
/// - `ex`：执行器（**应为事务**，由 handler 用 `db().transaction(..)` 打开）；
/// - `user_id`：用户 ID（来自 `AuthClaims`）；
/// - `patch`：已通过 [`validate_patch`] 的键值对。
///
/// # 返回值
///
/// 受影响行数之和（`INSERT` 与 `UPDATE` 都算 1）。
///
/// # 异常情况
///
/// - `DbError::ForeignKeyViolation`：用户被并发删除（外键 `fk_sys_user_preference_user`）；
/// - 其它 `DbError`：数据库异常。
pub async fn upsert_values(
    ex: &impl SqlExecutor,
    user_id: &Uuid,
    patch: &[(String, JsonValue)],
) -> DbResult<u64> {
    let mut affected = 0u64;
    for (key, value) in patch {
        affected += ex
            .exec_sql(
                VALUE_UPSERT.to_string(),
                vec![
                    Value::from(*user_id),
                    Value::from(key.clone()),
                    Value::Json(value.to_string()),
                ],
            )
            .await?;
    }
    Ok(affected)
}

// ==================== 纯逻辑：校验 / 合并 ====================

/// 逐 key 校验补丁（未知键 / 类型 / 值域）。
///
/// # 参数
///
/// - `defs`：字典启用项；
/// - `patch`：待写入的键值对。
///
/// # 异常情况
///
/// - `400 + 62020`：键不在字典里（或已被停用）；
/// - `400 + 62021`：类型不符；
/// - `400 + 62022`：值域不符（oneof / range）；
/// - `500 + 62024`：字典自身的 value_type / validator 配坏。
pub fn validate_patch(
    defs: &[PreferenceDef],
    patch: &[(String, JsonValue)],
) -> std::result::Result<(), ApiError> {
    let index: BTreeMap<&str, &PreferenceDef> = defs
        .iter()
        .map(|def| (def.pref_key.as_str(), def))
        .collect();
    for (key, value) in patch {
        let def = index.get(key.as_str()).ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PREFERENCE_KEY_UNKNOWN,
                format!("未知的偏好项：{key}"),
            )
        })?;
        let value_type = ValueType::parse(&def.value_type).ok_or_else(|| {
            tracing::error!(pref_key = %def.pref_key, value_type = %def.value_type,
                "偏好字典的 value_type 非法");
            ApiError::new(
                500,
                error_code::PREFERENCE_DEF_INVALID,
                format!("偏好项「{}」的类型配置损坏，请联系管理员", def.pref_name),
            )
        })?;
        if !value_type.is_instance(value) {
            return Err(ApiError::new(
                400,
                error_code::PREFERENCE_VALUE_TYPE_INVALID,
                format!(
                    "偏好项「{}」要求{}类型的值（当前：{}）",
                    def.pref_name,
                    value_type.label(),
                    value
                ),
            ));
        }
        if let Some(raw) = def.validator.as_deref()
            && let Some(rule) = Validator::parse(raw)?
        {
            rule.check(def, value)?;
        }
    }
    Ok(())
}

/// 合并「字典默认值 + 用户已存值」→ 响应体。
///
/// # 语义
///
/// - `items`：**全量**（字典里每个启用键都出现），未设置过的键取字典默认值；
/// - `palette` / `theme_mode` / `side_compact`：兼容旧前端的扁平字段，
///   取值恒等于 `items` 里对应的键（不另开一份真相）；
/// - 用户表里存在、但字典已删除/停用的键**不下发**（防止脏数据泄漏到界面）。
///
/// # 异常情况
///
/// 字典默认值不是合法 JSON → `500 + 62024`。
pub fn assemble(
    defs: &[PreferenceDef],
    stored: &BTreeMap<String, JsonValue>,
) -> std::result::Result<PreferenceRes, ApiError> {
    let mut items: BTreeMap<String, JsonValue> = BTreeMap::new();
    for def in defs {
        let value = match stored.get(&def.pref_key) {
            Some(value) => value.clone(),
            None => def.default_value()?,
        };
        items.insert(def.pref_key.clone(), value);
    }

    // 兼容字段：字典里没有这三个键时（例如被人误删种子）用写死的兜底值，
    // 保证前端「至少拿到一个合法值」，不至于把界面渲染成空白。
    let palette = items
        .get("ui.palette")
        .and_then(JsonValue::as_str)
        .unwrap_or(FALLBACK_PALETTE)
        .to_string();
    let theme_mode = items
        .get("ui.theme_mode")
        .and_then(JsonValue::as_str)
        .unwrap_or(FALLBACK_THEME_MODE)
        .to_string();
    let side_compact = items
        .get("ui.side_compact")
        .and_then(JsonValue::as_bool)
        .unwrap_or(FALLBACK_SIDE_COMPACT);

    Ok(PreferenceRes {
        palette,
        theme_mode,
        side_compact,
        items,
    })
}

/// 兜底配色（字典缺 `ui.palette` 时使用；与 `style/palettes.ts` 的默认值一致）。
pub const FALLBACK_PALETTE: &str = "mist-blue";
/// 兜底明暗模式。
pub const FALLBACK_THEME_MODE: &str = "light";
/// 兜底侧边栏折叠状态。
pub const FALLBACK_SIDE_COMPACT: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条字典项（默认值给 JSON 文本，与库里取出的形态一致）。
    fn def(
        key: &str,
        name: &str,
        value_type: &str,
        default_text: &str,
        validator: Option<&str>,
    ) -> PreferenceDef {
        PreferenceDef {
            pref_key: key.to_string(),
            pref_name: name.to_string(),
            value_type: value_type.to_string(),
            default_value_text: default_text.to_string(),
            scope: "ui".to_string(),
            validator: validator.map(str::to_string),
            sort_order: 0,
        }
    }

    /// 8 项预置字典（与 V0091 逐条对应；单测不连库，用同一份元数据驱动）。
    fn seed_defs() -> Vec<PreferenceDef> {
        vec![
            def(
                "ui.palette",
                "配色方案",
                "string",
                "\"mist-blue\"",
                Some("oneof:mist-blue,deep-sea,jade"),
            ),
            def(
                "ui.theme_mode",
                "明暗模式",
                "string",
                "\"light\"",
                Some("oneof:light,dark"),
            ),
            def("ui.side_compact", "侧边栏折叠", "boolean", "false", None),
            def(
                "ui.table_density",
                "表格密度",
                "string",
                "\"default\"",
                Some("oneof:default,compact"),
            ),
            def(
                "ui.page_size",
                "默认分页条数",
                "number",
                "10",
                Some("range:5-200"),
            ),
            def(
                "ui.language",
                "界面语言",
                "string",
                "\"zh-CN\"",
                Some("oneof:zh-CN,en-US"),
            ),
            def("notify.inbox", "站内信开关", "boolean", "true", None),
            def("notify.email", "邮件通知开关", "boolean", "false", None),
        ]
    }

    fn patch(pairs: &[(&str, JsonValue)]) -> Vec<(String, JsonValue)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn code_of(err: &ApiError) -> i32 {
        err.code
    }

    #[test]
    fn valid_patch_passes() {
        let defs = seed_defs();
        let items = patch(&[
            ("ui.palette", serde_json::json!("deep-sea")),
            ("ui.theme_mode", serde_json::json!("dark")),
            ("ui.side_compact", serde_json::json!(true)),
            ("ui.table_density", serde_json::json!("compact")),
            ("ui.page_size", serde_json::json!(50)),
            ("ui.language", serde_json::json!("en-US")),
            ("notify.inbox", serde_json::json!(false)),
            ("notify.email", serde_json::json!(true)),
        ]);
        assert!(validate_patch(&defs, &items).is_ok());
    }

    /// 字典里没有的键 → 62020（防止前端乱塞脏数据）。
    #[test]
    fn unknown_key_is_rejected() {
        let defs = seed_defs();
        let err = validate_patch(&defs, &patch(&[("evil.key", serde_json::json!(1))]))
            .expect_err("字典外的键必须被拒绝");
        assert_eq!(code_of(&err), error_code::PREFERENCE_KEY_UNKNOWN);
        assert_eq!(code_of(&err), 62020);
    }

    /// 类型不符（boolean 字段塞字符串）→ 62021。
    #[test]
    fn type_mismatch_is_rejected() {
        let defs = seed_defs();
        let err = validate_patch(
            &defs,
            &patch(&[("ui.side_compact", serde_json::json!("yes"))]),
        )
        .expect_err("字符串不能写进布尔偏好");
        assert_eq!(code_of(&err), error_code::PREFERENCE_VALUE_TYPE_INVALID);
        assert_eq!(code_of(&err), 62021);
    }

    /// oneof 之外的值 → 62022。
    #[test]
    fn oneof_violation_is_rejected() {
        let defs = seed_defs();
        let err = validate_patch(
            &defs,
            &patch(&[("ui.palette", serde_json::json!("not-a-palette"))]),
        )
        .expect_err("不存在的配色必须被拒绝");
        assert_eq!(code_of(&err), error_code::PREFERENCE_VALUE_INVALID);
        assert_eq!(code_of(&err), 62022);
    }

    /// 超 range → 62022；边界值与小数也要覆盖。
    #[test]
    fn range_violation_is_rejected() {
        let defs = seed_defs();
        for bad in [
            serde_json::json!(999),
            serde_json::json!(4),
            serde_json::json!(10.5),
        ] {
            let err = validate_patch(&defs, &patch(&[("ui.page_size", bad.clone())]))
                .expect_err("超范围 / 小数的每页条数必须被拒绝");
            assert_eq!(
                code_of(&err),
                error_code::PREFERENCE_VALUE_INVALID,
                "值={bad}"
            );
        }
        // 边界值必须通过（range 是闭区间）
        for ok in [serde_json::json!(5), serde_json::json!(200)] {
            assert!(
                validate_patch(&defs, &patch(&[("ui.page_size", ok.clone())])).is_ok(),
                "边界值 {ok} 应通过"
            );
        }
    }

    /// null 不能写进任何偏好（语义上等于「没设置」）。
    #[test]
    fn null_is_rejected_for_every_type() {
        let defs = seed_defs();
        for key in ["ui.palette", "ui.page_size", "ui.side_compact"] {
            let err = validate_patch(&defs, &patch(&[(key, JsonValue::Null)]))
                .expect_err("null 不应写入偏好");
            assert_eq!(
                code_of(&err),
                error_code::PREFERENCE_VALUE_TYPE_INVALID,
                "键={key}"
            );
        }
    }

    /// GET 合并默认值：用户只设了 1 个键，返回全量 8 个键，其余取默认值。
    #[test]
    fn assemble_merges_defaults() {
        let defs = seed_defs();
        let mut stored = BTreeMap::new();
        stored.insert("ui.palette".to_string(), serde_json::json!("jade"));

        let res = assemble(&defs, &stored).expect("合并应当成功");
        assert_eq!(res.items.len(), 8, "items 必须是全量");
        assert_eq!(res.items["ui.palette"], serde_json::json!("jade"));
        assert_eq!(res.items["ui.theme_mode"], serde_json::json!("light"));
        assert_eq!(res.items["ui.side_compact"], serde_json::json!(false));
        assert_eq!(res.items["ui.page_size"], serde_json::json!(10));
        assert_eq!(res.items["notify.inbox"], serde_json::json!(true));
        // 兼容字段必须等于 items 里的同名字段
        assert_eq!(res.palette, "jade");
        assert_eq!(res.theme_mode, "light");
        assert!(!res.side_compact);
    }

    /// 用户表里有、但字典已停用的键不下发（防脏数据泄漏）。
    #[test]
    fn stored_key_outside_dictionary_is_not_returned() {
        let defs = seed_defs();
        let mut stored = BTreeMap::new();
        stored.insert("legacy.key".to_string(), serde_json::json!("x"));

        let res = assemble(&defs, &stored).expect("合并应当成功");
        assert!(!res.items.contains_key("legacy.key"));
    }

    /// 字典配坏（默认值不是 JSON / validator 写错）→ 500 + 62024，不能静默放行。
    #[test]
    fn broken_dictionary_is_reported() {
        let broken_default = vec![def("ui.palette", "配色方案", "string", "{oops", None)];
        let err = assemble(&broken_default, &BTreeMap::new()).expect_err("默认值损坏必须报警");
        assert_eq!(code_of(&err), error_code::PREFERENCE_DEF_INVALID);
        assert_eq!(code_of(&err), 62024);

        let broken_type = vec![def("ui.palette", "配色方案", "colour", "\"x\"", None)];
        let err = validate_patch(
            &broken_type,
            &patch(&[("ui.palette", serde_json::json!("x"))]),
        )
        .expect_err("value_type 非法必须报警");
        assert_eq!(code_of(&err), error_code::PREFERENCE_DEF_INVALID);

        let broken_validator = vec![def(
            "ui.page_size",
            "默认分页条数",
            "number",
            "10",
            Some("range:5"),
        )];
        let err = validate_patch(
            &broken_validator,
            &patch(&[("ui.page_size", serde_json::json!(10))]),
        )
        .expect_err("validator 非法必须报警");
        assert_eq!(code_of(&err), error_code::PREFERENCE_DEF_INVALID);
    }

    /// validator 解析单测（含空规则）。
    #[test]
    fn validator_parsing() {
        assert_eq!(Validator::parse("  ").expect("空规则合法"), None);
        assert_eq!(
            Validator::parse("oneof:a,b").expect("合法"),
            Some(Validator::OneOf(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            Validator::parse("range:5-200").expect("合法"),
            Some(Validator::Range(5, 200))
        );
        assert!(Validator::parse("oneof:").is_err());
        assert!(Validator::parse("regex:^a").is_err());
        assert!(Validator::parse("range:200-5").is_err());
    }
}
