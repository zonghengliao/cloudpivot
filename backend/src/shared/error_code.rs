//! 业务错误码常量。
//!
//! # 与 HTTP 状态码的分工
//!
//! 框架 `alun::codes` 定义的是 **HTTP 语义码**（`OK = 0`、`BAD_REQUEST = 400`、
//! `UNAUTHORIZED = 401`、`FORBIDDEN = 403`、`NOT_FOUND = 404`、`CONFLICT = 409`、
//! `UNPROCESSABLE_ENTITY = 422`、`INTERNAL = 500`、`SERVICE_UNAVAILABLE = 503`），
//! 用于「一个状态码即可表达」的通用场景。
//!
//! 本模块定义的是 **业务错误码**（`1000x` 起分段），用于把「同一 HTTP 状态下
//! 的不同业务语义」区分开：统一响应体中的 `code` 字段取这里的值，HTTP 状态码
//! 仍由处理器显式指定。处理器按「HTTP 状态码 + 业务错误码 + 对外消息」三元组
//! 构造错误：
//!
//! ```ignore
//! use crate::shared::error_code;
//!
//! // 400：参数校验失败
//! return Err(ApiError::new(400, error_code::PARAM_INVALID, "用户名不能为空"));
//! // 401：用户名或密码错误
//! return Err(ApiError::new(401, error_code::AUTH_BAD_CREDENTIALS, "用户名或密码错误"));
//! // 403：无权限
//! return Err(ApiError::new(403, error_code::PERMISSION_DENIED, "权限不足"));
//! ```
//!
//! # 码段划分
//!
//! `0` 表示成功（见 `alun::codes::OK`），不在本模块定义。
//!
//! | 码段 | 含义 |
//! |------|------|
//! | `1000x` | 通用（参数校验、资源不存在、内部错误） |
//! | `2000x` | 认证（凭据错误、令牌过期、账号锁定） |
//! | `3000x` | 授权（无权限、角色 / 菜单 / 部门冲突） |
//! | `4000x` | 支撑域（字典 / 参数 / 文件 / 任务 / 日志） |
//! | `6000x` | 业务域 · 批次 A（对象存储 / 导入导出 / 消息通知） |
//! | `6100x` | 权限增强 · 批次 B（数据权限 / 列级脱敏 / 角色互斥） |
//!
//! > 错误码统一在此定义并在处理器中引用，禁止散落魔法数字。

// ==================== 1000x：通用 ====================

/// 参数校验失败（HTTP 400）。
pub const PARAM_INVALID: i32 = 10001;

/// 请求资源不存在（HTTP 404）。
pub const RESOURCE_NOT_FOUND: i32 = 10002;

/// 资源已存在或与现有数据冲突（HTTP 409）。
pub const RESOURCE_CONFLICT: i32 = 10003;

/// 服务器内部错误（HTTP 500）。
pub const INTERNAL_ERROR: i32 = 10004;

/// 请求过于频繁（HTTP 429）。
pub const TOO_MANY_REQUESTS: i32 = 10005;

/// 请求数据格式错误（HTTP 400）。
pub const PAYLOAD_INVALID: i32 = 10006;

// ==================== 2000x：认证 ====================

/// 用户名或密码错误（HTTP 401）。
pub const AUTH_BAD_CREDENTIALS: i32 = 20001;

/// 账号已被禁用（HTTP 403）。
pub const AUTH_ACCOUNT_DISABLED: i32 = 20002;

/// 账号已锁定（HTTP 403）。
pub const AUTH_ACCOUNT_LOCKED: i32 = 20003;

/// 缺少认证令牌（HTTP 401）。
pub const AUTH_TOKEN_MISSING: i32 = 20004;

/// 令牌无效或已过期（HTTP 401）。
pub const AUTH_TOKEN_INVALID: i32 = 20005;

/// 令牌已失效（登出黑名单命中，HTTP 401）。
pub const AUTH_TOKEN_REVOKED: i32 = 20006;

/// 刷新令牌无效或已过期（HTTP 401）。
pub const AUTH_REFRESH_TOKEN_INVALID: i32 = 20007;

// ==================== 3000x：授权 ====================

/// 无权限访问该资源（HTTP 403）。
pub const PERMISSION_DENIED: i32 = 30001;

/// 角色编码已存在（HTTP 409）。
pub const ROLE_CODE_EXISTS: i32 = 30002;

/// 角色不存在（HTTP 404）。
pub const ROLE_NOT_FOUND: i32 = 30003;

/// 角色被内置标识或关联数据占用，不可删除（HTTP 409）。
pub const ROLE_IN_USE: i32 = 30004;

/// 菜单 / 权限点编码冲突（HTTP 409）。
pub const MENU_CODE_CONFLICT: i32 = 30005;

/// 菜单存在子节点，不可删除（HTTP 409）。
pub const MENU_HAS_CHILDREN: i32 = 30006;

/// 部门存在子部门，不可删除（HTTP 409）。
pub const DEPT_HAS_CHILDREN: i32 = 30007;

/// 部门编码已存在（HTTP 409）。
pub const DEPT_CODE_EXISTS: i32 = 30008;

/// 用户不存在（HTTP 404）。
pub const USER_NOT_FOUND: i32 = 30009;

/// 用户名已存在（HTTP 409）。
pub const USERNAME_EXISTS: i32 = 30010;

/// 同级部门下名称已存在（HTTP 409）。
pub const DEPT_NAME_EXISTS: i32 = 30011;

/// 部门不存在（HTTP 404）。
pub const DEPT_NOT_FOUND: i32 = 30012;

/// 角色不存在或已停用（HTTP 404/409，分配角色时校验）。
pub const ROLE_INVALID: i32 = 30013;

// ==================== 4000x：支撑域 ====================

/// 字典编码已存在（HTTP 409）。
pub const DICT_CODE_EXISTS: i32 = 40001;

/// 同一字典下字典项值已存在（HTTP 409）。
pub const DICT_ITEM_VALUE_EXISTS: i32 = 40002;

/// 参数键已存在（HTTP 409）。
pub const CONFIG_KEY_EXISTS: i32 = 40003;

/// 参数不允许修改（HTTP 403）。
pub const CONFIG_NOT_MODIFIABLE: i32 = 40004;

/// 文件不存在（HTTP 404）。
pub const FILE_NOT_FOUND: i32 = 40005;

/// 文件类型不被允许（HTTP 422）。
pub const FILE_TYPE_NOT_ALLOWED: i32 = 40006;

/// 文件大小超出限制（HTTP 422）。
pub const FILE_SIZE_EXCEEDED: i32 = 40007;

/// 定时任务不存在（HTTP 404）。
pub const JOB_NOT_FOUND: i32 = 40008;

/// 定时任务处理器未注册（HTTP 422）。
pub const JOB_TARGET_UNREGISTERED: i32 = 40009;

/// cron 表达式非法（HTTP 422）。
pub const JOB_CRON_INVALID: i32 = 40010;

/// 操作日志不存在（HTTP 404）。
pub const LOG_NOT_FOUND: i32 = 40011;

/// 在线用户会话不存在或已过期（HTTP 404）。
pub const ONLINE_SESSION_NOT_FOUND: i32 = 40012;

/// 在线用户能力依赖缓存，缓存未初始化（HTTP 503）。
pub const ONLINE_CACHE_UNAVAILABLE: i32 = 40013;

/// 未配置参数加密密钥，无法按要求加密存储（HTTP 500）。
pub const CONFIG_SECRET_MISSING: i32 = 40014;

/// 参数值解密失败（密钥不匹配或密文损坏，HTTP 500）。
pub const CONFIG_DECRYPT_FAILED: i32 = 40015;

// ==================== 6000x：业务域 · 批次 A ====================
//
// 批次 A（对象存储 / 导入导出 / 消息通知）新增码段，清单见
// `docs/批次A-技术架构.md` §5.6。本批次**不改动任何既有码值**，只在本段追加。
// 当前已落地的为对象存储（A1）所需的 60001–60010；导入导出与消息通知的
// 60011–60019 由各自实施卡追加，避免提前占用未来语义。

/// 文件所在存储后端已停用 / 删除（HTTP 409）。
pub const STORAGE_BACKEND_UNAVAILABLE: i32 = 60001;

/// 目标后端 `status = 0`（HTTP 409）。
pub const STORAGE_BACKEND_DISABLED: i32 = 60002;

/// 超过策略 `max_size_mb`（HTTP 422）。
pub const STORAGE_SIZE_EXCEEDED: i32 = 60003;

/// 扩展名不在策略白名单（HTTP 400）。
pub const STORAGE_TYPE_NOT_ALLOWED: i32 = 60004;

/// MIME 与扩展名 / 白名单不符（HTTP 400）。
pub const STORAGE_MIME_NOT_ALLOWED: i32 = 60005;

/// 该分类未开启直传（HTTP 422）。
pub const STORAGE_DIRECT_UPLOAD_DISABLED: i32 = 60006;

/// 登记时对象不存在（直传未真正完成，HTTP 404）。
pub const STORAGE_OBJECT_MISSING: i32 = 60007;

/// 删除被 `sys_file` 引用的后端（HTTP 409）。
pub const STORAGE_CONFIG_IN_USE: i32 = 60008;

/// endpoint 指向私网 / 回环（SSRF 防护，HTTP 400）。
pub const STORAGE_ENDPOINT_FORBIDDEN: i32 = 60009;

/// AK/SK 解密失败或探测鉴权失败（不区分，防探测，HTTP 422）。
pub const STORAGE_CREDENTIAL_INVALID: i32 = 60010;

// ==================== 6001x：导入导出（A3） ====================

/// `biz_type` 未注册（HTTP 404）。
pub const IMPORT_DEF_NOT_FOUND: i32 = 60011;

/// 超过导入定义的单次行数上限（HTTP 422）。
pub const IMPORT_ROW_LIMIT: i32 = 60012;

/// 导入任务状态机非法跃迁（如对 `SUCCESS` 再 confirm，HTTP 409）。
pub const IMPORT_STATUS_INVALID: i32 = 60013;

/// 导入批次入库失败 / 未开部分导入但存在失败行（HTTP 422）。
pub const IMPORT_BATCH_FAILED: i32 = 60014;

/// 导出任务未就绪 / 超出同步导出上限（HTTP 409）。
pub const EXPORT_NOT_READY: i32 = 60015;

/// 导出产物已过期（HTTP 410）。
pub const EXPORT_EXPIRED: i32 = 60016;

/// 导入文件格式不合法（魔数不符 / 无法解析 / 只有表头 / 表头缺必需列，HTTP 400 或 422）。
pub const IMPORT_FILE_INVALID: i32 = 60020;

// ==================== 6002x：消息通知（A5） ====================
//
// 命名与语义对应 `docs/批次A-技术架构.md` §4（消息通知）；渲染类错误码的
// HTTP 状态统一 422（「请求语法没问题，但内容不可用」），与前端错误提示约定一致。

/// 通知不存在或已删除（HTTP 404）。
pub const NOTICE_NOT_FOUND: i32 = 60021;

/// 通知模板不存在或已停用（HTTP 404）。
pub const NOTICE_TEMPLATE_NOT_FOUND: i32 = 60022;

/// 模板变量缺失（未提供或值为 `null`，HTTP 422）。
pub const NOTICE_TEMPLATE_VAR_MISSING: i32 = 60023;

/// 模板语法错误 / 占位符未在 `variables` 中声明（HTTP 422）。
pub const NOTICE_TEMPLATE_SYNTAX: i32 = 60024;

/// 渲染后标题超 255 字符 / 正文超 64 KiB（HTTP 422）。
pub const NOTICE_TEMPLATE_TOO_LONG: i32 = 60025;

/// 发送目标非法（`target_ids` 为空或类型未知，HTTP 400）。
pub const NOTICE_TARGET_INVALID: i32 = 60026;

/// 单条通知接收人超过 5 万上限（HTTP 422）。
pub const NOTICE_RECEIVER_LIMIT: i32 = 60027;

/// 模板已被通知引用，不允许删除（HTTP 409）。
pub const NOTICE_TEMPLATE_IN_USE: i32 = 60028;

/// 模板编码重复（HTTP 409）。
pub const NOTICE_TEMPLATE_DUPLICATE: i32 = 60029;

/// 接收行不存在 / 不属于当前用户 / 已读或已删除（HTTP 404）。
pub const NOTICE_RECEIVER_NOT_FOUND: i32 = 60030;

/// 幂等键已存在但读不到对应通知（并发下的极端情况，HTTP 409）。
pub const NOTICE_DUPLICATE_KEY: i32 = 60031;

// ==================== 6003x：任务状态机与产物（A4） ====================
//
// A5 的 6002x 段（60021–60031）已占用，A4 从这里接着往后排，**不改动 A5 任何码值**。

/// 任务当前状态不允许该操作（导出任务的取消 / 重试 / 删除状态机非法跃迁，HTTP 409）。
pub const EXPORT_STATUS_INVALID: i32 = 60032;

/// 导出产物文件已丢失（记录在库但文件已被清理，与过期同语义，HTTP 410）。
pub const EXPORT_FILE_MISSING: i32 = 60033;

// ==================== 6100x：数据权限（批次 B） ====================
//
// 依据 `docs/批次B-技术架构.md` §5.3 的 6100x 段码表。本段最终 12 个码（61001–61012），
// B2（数据权限基座）用 61001/61002，B4（范围配置 API）补 61003/61004，
// B3（底座接入）补满 61005–61012；B6/B7 直接使用，**码值不得改动**
// （前端映射表已按此段规划）。统一在这里定义是为了守住项目铁律「错误码统一在此定义，
// 禁止散落魔法数字」——即使 B2 只用到两个，也不在 `business/` 里另开第二处码表。

/// 数据范围取值非法 / 缓存里的范围下标被污染（HTTP 400）。
pub const DATA_SCOPE_INVALID: i32 = 61001;

/// 新数据范围超出操作者自身的可见范围（防自我扩权，HTTP 403）。
pub const DATA_SCOPE_EXCEED_LIMIT: i32 = 61002;

/// 试图修改自己持有角色的数据范围（防自我扩权，HTTP 403；超管不受限，架构 §2.7）。
pub const DATA_SCOPE_SELF_ROLE_LOCKED: i32 = 61003;

/// `custom_dept_ids` 里有不存在 / 已软删的部门（HTTP 404）。
pub const DATA_SCOPE_DEPT_NOT_FOUND: i32 = 61004;

/// `mask_type` 非法 / `mask_param` 格式不符（如手机号参数不是 `"3,4"`，HTTP 400）。
pub const FIELD_MASK_TYPE_INVALID: i32 = 61005;

/// `table_name` / `column_name` 在 `information_schema` 里不存在（HTTP 404）。
pub const FIELD_MASK_TARGET_NOT_FOUND: i32 = 61006;

/// 自定义正则编译失败 / 捕获组越界（HTTP 422）。
pub const FIELD_MASK_REGEX_INVALID: i32 = 61007;

/// **导出 fail-closed**：敏感列没有生效规则（HTTP 422）。
pub const FIELD_MASK_RULE_MISSING: i32 = 61008;

/// 授予角色命中互斥（HTTP 409）。
pub const ROLE_MUTEX_CONFLICT: i32 = 61009;

/// 建立互斥时已有用户同时持有这两个角色（HTTP 409）。
pub const ROLE_MUTEX_HAS_HOLDERS: i32 = 61010;

/// 互斥关系已存在（含反向，HTTP 409）。
pub const ROLE_MUTEX_DUPLICATE: i32 = 61011;

/// 角色与自身互斥（HTTP 400）。
pub const ROLE_MUTEX_SELF_PAIR: i32 = 61012;

// ───────────────────────── 批次 C · 62xxx 段（13 个） ─────────────────────────
//
// 依据 `docs/批次C-技术架构.md` §5.2 码表（逐字一致，**码值不得改动**）。
// 项目纪律「错误三出口」要求业务码集中定义在这里，散落定义一律评审打回；
// 前端 `error-code.ts` 的文案映射按此段规划，改一个值就是前端文案错位。

/// 公告标题/正文格式或长度不合法（HTTP 400）。
pub const ANNOUNCEMENT_CONTENT_INVALID: i32 = 62001;

/// 公告不存在或已删除（HTTP 404）。
pub const ANNOUNCEMENT_NOT_FOUND: i32 = 62002;

/// 附件不存在或不属于该公告（HTTP 404）。
pub const ANNOUNCEMENT_ATTACHMENT_NOT_FOUND: i32 = 62003;

/// 无权访问该公告（HTTP 403）。
pub const ANNOUNCEMENT_FORBIDDEN: i32 = 62004;

/// 公告未发布或已过期（HTTP 403）。
pub const ANNOUNCEMENT_NOT_AVAILABLE: i32 = 62005;

/// 发布范围为空或超过 500 个对象（HTTP 400）。
pub const ANNOUNCEMENT_SCOPE_INVALID: i32 = 62006;

/// 置顶必须指定未来到期时间（HTTP 400）。
pub const ANNOUNCEMENT_TOP_INVALID: i32 = 62007;

/// 有效期不合法（须晚于发布时间，HTTP 400）。
pub const ANNOUNCEMENT_EXPIRE_INVALID: i32 = 62008;

/// 已发布公告不可直接编辑，请先下线（HTTP 409）。
pub const ANNOUNCEMENT_STATUS_CONFLICT: i32 = 62009;

/// 帮助文档分类层级超过 3 层（HTTP 422）。
pub const HELP_CATEGORY_DEPTH_EXCEEDED: i32 = 62010;

/// 帮助文档分类下仍有文档，不可删除（HTTP 409）。
pub const HELP_CATEGORY_NOT_EMPTY: i32 = 62011;

/// 变更日志：资源未登记白名单（HTTP 500）。
///
/// **用 500 是有意的**：它不是用户输入问题，是开发接入缺陷，必须报警而不是静默。
pub const CHANGE_LOG_RESOURCE_UNREGISTERED: i32 = 62012;

/// 变更时间不合法（未来时间或早于保留期，HTTP 400）。
pub const CHANGE_LOG_TIME_INVALID: i32 = 62013;

#[cfg(test)]
mod tests {
    use super::*;

    /// B3 段码位自检：6100x 段 12 个码齐全、码值不重复、与架构 §5.3 码表逐字一致。
    ///
    /// 这条测试的作用不是「测代码」，而是**防止后续实施步（B4/B6/B7）改码值**：
    /// 前端 `error-code.ts` 的文案映射已按此段规划，改一个值就是前端文案错位。
    #[test]
    fn batch_b_error_codes_are_stable() {
        let codes = [
            (DATA_SCOPE_INVALID, 61001),
            (DATA_SCOPE_EXCEED_LIMIT, 61002),
            (DATA_SCOPE_SELF_ROLE_LOCKED, 61003),
            (DATA_SCOPE_DEPT_NOT_FOUND, 61004),
            (FIELD_MASK_TYPE_INVALID, 61005),
            (FIELD_MASK_TARGET_NOT_FOUND, 61006),
            (FIELD_MASK_REGEX_INVALID, 61007),
            (FIELD_MASK_RULE_MISSING, 61008),
            (ROLE_MUTEX_CONFLICT, 61009),
            (ROLE_MUTEX_HAS_HOLDERS, 61010),
            (ROLE_MUTEX_DUPLICATE, 61011),
            (ROLE_MUTEX_SELF_PAIR, 61012),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "6100x 段码值不得改动");
        }
        // 与批次 A 的 6003x 段不重叠（60033 < 61001）
        assert!(codes[0].0 > 60033, "6100x 段必须排在批次 A 之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "6100x 段不得有重复码值");
        println!(
            "[码表] 6100x 段 {} 个码齐全且不重复（61001–61012）✓",
            codes.len()
        );
    }

    /// C0 段码位自检：62xxx 段 **13 个**码齐全、码值不重复、与架构 §5.2 码表逐字一致。
    ///
    /// 与上面 B3 那条测试同性质：防止后续实施步（C2/C4/C6）顺手改码值 ——
    /// 前端 `error-code.ts` 的文案映射按此段规划，改一个值就是前端文案错位。
    /// 另断言与 6100x 段不重叠，且 62012 的 HTTP 语义有意为 500（接入缺陷，必须报警）。
    #[test]
    fn batch_c_error_codes_are_stable() {
        let codes = [
            (ANNOUNCEMENT_CONTENT_INVALID, 62001),
            (ANNOUNCEMENT_NOT_FOUND, 62002),
            (ANNOUNCEMENT_ATTACHMENT_NOT_FOUND, 62003),
            (ANNOUNCEMENT_FORBIDDEN, 62004),
            (ANNOUNCEMENT_NOT_AVAILABLE, 62005),
            (ANNOUNCEMENT_SCOPE_INVALID, 62006),
            (ANNOUNCEMENT_TOP_INVALID, 62007),
            (ANNOUNCEMENT_EXPIRE_INVALID, 62008),
            (ANNOUNCEMENT_STATUS_CONFLICT, 62009),
            (HELP_CATEGORY_DEPTH_EXCEEDED, 62010),
            (HELP_CATEGORY_NOT_EMPTY, 62011),
            (CHANGE_LOG_RESOURCE_UNREGISTERED, 62012),
            (CHANGE_LOG_TIME_INVALID, 62013),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "62xxx 段码值不得改动");
        }
        // 与批次 B 的 6100x 段严格不重叠
        assert!(codes[0].0 > 61012, "62xxx 段必须排在批次 B 之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), 13, "62xxx 段必须是 13 个码");
        assert_eq!(unique.len(), codes.len(), "62xxx 段不得有重复码值");
        println!(
            "[码表] 62xxx 段 {} 个码齐全且不重复（62001–62013）✓",
            codes.len()
        );
    }
}
