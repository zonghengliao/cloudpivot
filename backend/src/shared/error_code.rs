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
//! | `62xxx` | 批次 C（公告 / 帮助 / 变更日志）+ 批次 D（偏好体系 KV） |
//! | `63xxx` | 批次 C+（工作任务管理 T 段：`63001`–`63009`） |
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

// ── 帮助文档（C4）── 架构 §5.2 的 62010/62011 只覆盖「分类」两条规则，
// 文档 CRUD / 版本 / slug 的失败语义需要独立码位，顺延 62014–62019。
// 号段说明：62001–62009 属公告（C2/C3），62012/62013 属变更日志（C5），互不重叠。

/// 帮助文档分类不存在 / 已软删（HTTP 404）。
pub const HELP_CATEGORY_NOT_FOUND: i32 = 62014;

/// 帮助文档不存在 / 已软删 / 对当前用户不可见（HTTP 404）。
///
/// 未发布的文档对普通用户**也返回这个码**（而不是 403）：403 会泄露"这份文档存在"。
pub const HELP_DOC_NOT_FOUND: i32 = 62015;

/// 帮助文档 slug 已被占用（HTTP 409）。
pub const HELP_DOC_SLUG_CONFLICT: i32 = 62016;

/// 帮助文档当前状态不允许该操作（HTTP 409）。
pub const HELP_DOC_STATUS_CONFLICT: i32 = 62017;

/// 帮助文档标题 / 正文 / slug 格式或长度不合法（HTTP 400）。
pub const HELP_DOC_CONTENT_INVALID: i32 = 62018;

/// 帮助文档指定的历史版本不存在（HTTP 404）。
pub const HELP_DOC_VERSION_NOT_FOUND: i32 = 62019;

// ── 批次 D · 偏好体系（平台通用 KV + 字典）── 62xxx 段顺延 62020–62024。
// 与 C 段（公告/帮助/变更日志）同段但互不重叠：62001–62019 属批次 C，62020 起属偏好。

/// 偏好键不在字典里（或已被停用）（HTTP 400）。
///
/// 「字典里没有的键一律拒绝」是偏好体系的第一道防线：前端乱塞的脏数据不会进库。
pub const PREFERENCE_KEY_UNKNOWN: i32 = 62020;

/// 偏好值类型与字典的 `value_type` 不符（HTTP 400）。
pub const PREFERENCE_VALUE_TYPE_INVALID: i32 = 62021;

/// 偏好值不满足字典的 `validator`（oneof / range）（HTTP 400）。
pub const PREFERENCE_VALUE_INVALID: i32 = 62022;

/// 更新偏好的请求体里一个偏好项都没有（HTTP 400）。
///
/// 不当作「无操作成功」：空请求通常是前端 bug，静默成功会把设置丢掉。
pub const PREFERENCE_PAYLOAD_EMPTY: i32 = 62023;

/// 偏好字典自身配置损坏（`value_type` / `validator` / `default_value` 非法）（HTTP 500）。
///
/// **用 500 是有意的**：这不是用户输入问题，是字典数据被改坏，必须报警而不是静默放行。
pub const PREFERENCE_DEF_INVALID: i32 = 62024;

// ── 批次 C+ · 工作任务管理（T 段）── **63xxx 段**（62025 起让给下一张 C 卡，避免与 62xxx 抢段）。
//
// 为什么单开 63xxx 而不是继续顺延 62025+：
//   ① 62xxx 段的既有单测硬断言「24 个码、62001–62024」，那是**冻结**的契约（前端
//      `error-code.ts` 的文案映射按段规划）；往里塞会逼着改一条「不得改动」的测试；
//   ② 任务管理的错误语义与公告/帮助/偏好没有重叠，独立成段更好维护。

/// 任务不存在 / 已软删 / 不在我的可见范围内（HTTP 404）。
///
/// **有意用 404 而不是 403**：403 会告诉调用者「这个 ID 存在」，等于把任务 ID
/// 变成可枚举的资源清单。看不见就是看不见。
pub const TASK_NOT_FOUND: i32 = 63001;

/// 没有操作该任务的权限（HTTP 403）。
///
/// 编辑只认「负责人 / 创建人」；删除额外认「超管」；协作人**看得见但改不了**。
pub const TASK_FORBIDDEN: i32 = 63002;

/// 任务标题为空或超长（HTTP 400）。
pub const TASK_TITLE_INVALID: i32 = 63003;

/// 任务字段取值非法（优先级 / 状态 / 工时 / 日期范围）（HTTP 400）。
pub const TASK_FIELD_INVALID: i32 = 63004;

/// 父任务非法（不存在 / 已删除 / 超过 2 层 / 成环 / 指向自己）（HTTP 400）。
pub const TASK_PARENT_INVALID: i32 = 63005;

/// 任务还有未完成的子任务，不允许删除（HTTP 409）。
pub const TASK_HAS_OPEN_CHILDREN: i32 = 63006;

/// 任务编号生成失败（月内序号取不到 / 咨询锁失败）（HTTP 500）。
pub const TASK_NO_GENERATE_FAILED: i32 = 63007;

/// 数据权限片段形态与本模块的可见性拼法不符（HTTP 500）。
///
/// 这条**不是**给用户看的错误，是给运维的信号：`shared/scope.rs` 生成的片段形态
/// 变了（比如去掉了前导 `AND`），任务可见性的 OR 拼法必须同步复查 ——
/// 否则 `(1 = 1 OR …)` 会让全表可见（静默越权）。
pub const TASK_SCOPE_UNEXPECTED: i32 = 63008;

/// 任务负责人不存在或已停用（HTTP 400）。
pub const TASK_OWNER_INVALID: i32 = 63009;

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

    /// C0 段码位自检：62xxx 段 **24 个**码齐全（C 段 19 + 批次 D 偏好 5）、码值不重复、与架构 §5.2 码表逐字一致
    /// （C4 在 62013 之后顺延了帮助文档的 62014–62019）。
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
            // C4：帮助文档 6 个码（62014–62019）
            (HELP_CATEGORY_NOT_FOUND, 62014),
            (HELP_DOC_NOT_FOUND, 62015),
            (HELP_DOC_SLUG_CONFLICT, 62016),
            (HELP_DOC_STATUS_CONFLICT, 62017),
            (HELP_DOC_CONTENT_INVALID, 62018),
            (HELP_DOC_VERSION_NOT_FOUND, 62019),
            // 批次 D：偏好体系 5 个码（62020–62024）
            (PREFERENCE_KEY_UNKNOWN, 62020),
            (PREFERENCE_VALUE_TYPE_INVALID, 62021),
            (PREFERENCE_VALUE_INVALID, 62022),
            (PREFERENCE_PAYLOAD_EMPTY, 62023),
            (PREFERENCE_DEF_INVALID, 62024),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "62xxx 段码值不得改动");
        }
        // 与批次 B 的 6100x 段严格不重叠
        assert!(codes[0].0 > 61012, "62xxx 段必须排在批次 B 之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            unique.len(),
            24,
            "62xxx 段必须是 24 个码（C 段 19 + 偏好 5）"
        );
        assert_eq!(unique.len(), codes.len(), "62xxx 段不得有重复码值");
        println!(
            "[码表] 62xxx 段 {} 个码齐全且不重复（62001–62019）✓",
            codes.len()
        );
    }

    /// T 段码位自检：63xxx 段 9 个码齐全、码值不重复、与 62xxx 段不重叠。
    ///
    /// 与另外两条同性质：**码值一旦发给前端就不能改**（`error-code.ts` 的文案映射
    /// 按码值命中）。新增 T 段错误时只许顺延 63010+，不许改已有值。
    #[test]
    fn task_error_codes_are_stable() {
        let codes = [
            (TASK_NOT_FOUND, 63001),
            (TASK_FORBIDDEN, 63002),
            (TASK_TITLE_INVALID, 63003),
            (TASK_FIELD_INVALID, 63004),
            (TASK_PARENT_INVALID, 63005),
            (TASK_HAS_OPEN_CHILDREN, 63006),
            (TASK_NO_GENERATE_FAILED, 63007),
            (TASK_SCOPE_UNEXPECTED, 63008),
            (TASK_OWNER_INVALID, 63009),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "63xxx 段码值不得改动");
        }
        // 与批次 C / 偏好体系的 62xxx 段严格不重叠
        assert!(codes[0].0 > 62024, "63xxx 段必须排在 62xxx 段之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "63xxx 段不得有重复码值");
        println!(
            "[码表] 63xxx 段 {} 个码齐全且不重复（63001–63009）✓",
            codes.len()
        );
    }
}
