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
//! | `63xxx` | 批次 C+（工作任务管理 T 段：`63001`–`63020`，`63101`–`63105` 为依赖 / 甘特子段） |
//! | `632xx` | 批次 C+（AI 执行者硬拒：`63201`–`63204`，T5 子段） |
//! | `64xxx` | 云枢 × Hermes（AI 用户 / profile / SOUL.md：`64001`–`64014`） |
//! | `65xxx` | 批次 D（业务审批流：定义侧 12 + 运行侧 10，`65001`–`65005` / `65010`–`65024` / `65030`–`65031`） |
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

/// 状态流转不在白名单内 / 状态冲突（HTTP 409）。
///
/// 覆盖四种语义相同的「这个流转现在不能做」：跳级（`TODO → DONE`）、终态直取
/// （`DONE → CANCELLED`）、同状态自环、`need_review=true` 时想跳过验收直接完成。
/// **不许把它们合并成 400 或静默成功**：409 才能让前端提示「刷新看看当前状态」。
pub const TASK_STATUS_TRANSITION_INVALID: i32 = 63010;

/// 当前身份无权执行该状态流转（HTTP 403）。
///
/// 典型场景：`REVIEW → DONE` 是**验收权**，负责人自己点必须被拒（哪怕他是负责人）。
pub const TASK_STATUS_FORBIDDEN: i32 = 63011;

/// 有子任务的父任务不许手改进度（HTTP 409）。
///
/// 红线：父任务进度由子任务汇总，手改必须**显式报错**（返回 409 + 提示
/// 「进度由子任务汇总」），不许悄悄改成一个下次重算就会被覆盖的值。
pub const TASK_PROGRESS_LOCKED: i32 = 63012;

/// 打回（`REVIEW → DOING`）必须填写原因，且不少于 5 个字（HTTP 400）。
pub const TASK_REJECT_REASON_REQUIRED: i32 = 63013;

// ── T3 追加（63014–63020）：分派 / 转派 / 协作人 ─────────────────────────

/// 单任务协作人超过上限（50 人）（HTTP 400）。
///
/// 上限是**防滥用**而不是业务规则：协作人越多，可见性 id 数组与通知扇出越贵
/// （决策 D7 的 2000 上限就是从这类放大效应来的）。要更多人协作就拆子任务。
pub const TASK_MEMBER_LIMIT: i32 = 63014;

/// 不许把任务的创建人 / 负责人从协作人里移除（HTTP 400）。
///
/// 他们是任务的天然可见方（可见性谓词的前两个分支），移除协作人行并不能
/// 「让他看不见」，只会让列表少一个人 —— 这类操作要么无意义要么是误操作。
pub const TASK_MEMBER_PROTECTED: i32 = 63015;

/// AI 用户（`user_type = 'ROBOT'`）被硬禁止的操作（HTTP 403）。
///
/// # ⚠️ 已由 632xx 段取代（T5）
///
/// T3 落地删任务硬拒时临时用了这个码；T5 卡面把「AI 执行者硬拒」统一到
/// **`632xx` 子段**（`63201` 禁登录 / `63202` 禁删任务 / `63203` 禁改数据权限 /
/// `63204` 禁管 AI 用户），删任务现在返回 [`AI_TASK_DELETE_FORBIDDEN`]（`63202`）。
///
/// **本常量保留、值不变**：码值一旦发出就不回收（前端可能缓存过，
/// 历史日志里也带着它），只是不再有代码返回它。稳定性自检里仍钉住 `63016`。
pub const TASK_ROBOT_FORBIDDEN: i32 = 63016;

/// 新负责人与当前负责人相同（HTTP 400）。
///
/// 静默成功会让调用方以为「转派成功了」，实际什么都没发生（也没有通知）；
/// 报错比「什么都没做却返回 200」诚实。
pub const TASK_ASSIGN_SAME_OWNER: i32 = 63017;

/// 分派时任务负责人已被他人改动（HTTP 409）。
///
/// 分派带乐观并发（`WHERE owner_id = 读到的旧值`）：两个人同时转派，
/// 后到的那次必须失败并提示刷新，否则「原负责人自动成为协作人」会记在错的人身上。
pub const TASK_ASSIGN_CONFLICT: i32 = 63018;

/// 协作人里有不存在 / 已删除 / 已停用的用户（HTTP 400）。
pub const TASK_MEMBER_INVALID: i32 = 63019;

/// 该用户不是本任务的协作人（HTTP 404）。
pub const TASK_MEMBER_NOT_FOUND: i32 = 63020;

// ── T4 追加（631xx 子段）：依赖环检测 / 前置阻塞 / 甘特时间窗 ─────────────
//
// 为什么这批码跳到 **631xx** 而不是接着 63021：卡面 t_687d35b1 明确要求
// 「成环 → 400 + **63101**」「前置未完成 → 409 + **63102**」，这两个码值是
// 前端文案映射与验收脚本的契约，不能为了「连续好看」改成 63021/63022。
// 630xx 段留给任务管理的后续小改（63021+），631xx 子段专用于依赖与甘特。

/// 依赖会成环，已拒绝（HTTP 400）。
///
/// 消息里**必须点名环上的每一个任务**（`task_no「标题」→ …`）——只回「有环」
/// 等于让用户自己去几百条依赖里找环。环路径由 `task/dep.rs::render_cycle` 渲染。
/// 依赖链超过 100 层（无法在安全成本内判定）时也用这个码：同样是「拒绝加依赖」。
pub const TASK_DEPENDENCY_CYCLE: i32 = 63101;

/// 前置任务未完成，不能进入 DOING（HTTP 409）。
///
/// 消息里**必须列出未完成的前置**（编号 + 标题 + 状态）。`CANCELLED` 的前置
/// 视为「已解除」，不在名单里（见 `task/dep.rs::is_blocking_status`）。
pub const TASK_DEPENDENCY_BLOCKED: i32 = 63102;

/// 依赖本身不合法（HTTP 400）：自依赖 / 父子任务互依赖 / 跨项目 / 类型非 FS /
/// 滞后天数越界 / 用了本版不支持的 `board_id` 维度。
pub const TASK_DEPENDENCY_INVALID: i32 = 63103;

/// 这条依赖不存在（HTTP 404）：解除依赖时命中 0 行。
///
/// **不静默成功**：静默成功会让前端以为删掉了，刷新后又出现。
pub const TASK_DEPENDENCY_NOT_FOUND: i32 = 63104;

/// 甘特时间窗非法（HTTP 400）：`end < start`，或跨度超过 180 天。
pub const TASK_GANTT_RANGE_INVALID: i32 = 63105;

// ── T7a 追加（631xx 子段续段）：甘特拖拽改期的三类依赖 / 父子冲突 ─────────
//
// 为什么接着 63106+ 而不是 63021+：这三条与 63101–63105 同属「依赖与甘特排期」
// 子段（改期本质是排期变更），且 T7a 卡面把码值写死成 63110 / 63111 / 63112，
// 前端文案映射与验收脚本按码值命中，**不许改值**。
// 三条的共同红线：消息里**必须点名冲突对象**（`task_no` + 标题 + 冲突日期），
// 只回「改期违反依赖」等于让用户自己去几百条依赖里找冲突。

/// 改期违反**前置**依赖（HTTP 400）：改期后本任务开始日早于某个未完成前置的截止日。
///
/// 文案形如：`改期违反依赖：前置 TSK-202610-000012「数据库设计」（截止 2026-10-30）尚未完成`。
/// 口径与 [`TASK_DEPENDENCY_BLOCKED`] 一致：`DONE` / `CANCELLED` 的前置已解除，不算冲突。
pub const TASK_SCHEDULE_PREDECESSOR: i32 = 63110;

/// 改期违反**后继**依赖（HTTP 400）：改期后本任务截止日晚于某个后继的计划开始日。
///
/// 文案形如：`改期违反依赖：后继 TSK-202610-000015「接口联调」计划 2026-11-01 开始`。
/// `CANCELLED` 的后继已无排期意义，不参与判定（其余状态一律参与，含 `DONE`）。
pub const TASK_SCHEDULE_SUCCESSOR: i32 = 63111;

/// 改期违反**父子区间**（HTTP 400）：子任务日期跑出父任务区间，或父任务改期把
/// 已有子任务甩在区间外（本卡默认**不级联**，所以父任务不许把子任务甩出去）。
///
/// 文案里点名父/子任务的编号 + 标题 + 区间。
pub const TASK_SCHEDULE_PARENT_RANGE: i32 = 63112;

// ── T5 追加（**632xx** 子段）：AI 执行者硬拒 ──────────────────────────────
//
// 为什么单开 632xx 而不是继续用 630xx：T5 卡面把这四件事的错误码写死成
// 63201–63204（前端文案映射与验收脚本的契约）。它们描述的不是「任务数据的哪种
// 不合法」，而是「这个操作主体（AI 用户）被永久禁止做这件事」，与任务域的
// 630xx 是两个维度；混在 630xx 里会让「任务错误」与「主体错误」在统计口径上
// 打平。631xx 已被依赖 / 甘特占用，所以顺延到 632xx。
//
// 硬拒 vs 权限码（修正 11 的边界，四个码逐条对应）：
//   · 63201 禁网页登录、63202 禁删任务、63203 禁改数据权限、63204 禁管 AI 用户
//     → **硬拒**：给什么角色都不可能放行；
//   · 分派 / 转派 / 协作人**没有**对应的 632xx 码 —— 它们只查权限码
//     （`business:task:assign` / `business:task:member`），AI 用户被授角色就能做。
//     给它加一个「AI 不能分派」的码，等于把修正 11 又焊死。

/// AI 用户（`user_type = 'ROBOT'`）禁止网页登录（HTTP 403）。
///
/// 登录接口在「查到用户之后、校验密码之前」直接拒绝，并**记 `auth_logs`**
/// （`LOCAL/LOGIN/FAIL`，`error_message = "AI 用户禁止网页登录"`）：
/// 这条日志回答的是「谁在拿 AI 账号撞登录页」，比静默 401 有用得多。
/// AI 用户的合法入口是 API Key / 任务令牌（T5b），不是网页会话。
pub const AI_LOGIN_FORBIDDEN: i32 = 63201;

/// AI 用户禁止删除任务（HTTP 403）。
///
/// 与「分派」不同，这条**硬拒**：连超管角色都救不回来。理由是一条 AI 执行者
/// 被注入后「删掉任务」是不可逆的破坏（评论 / 日志 / 通知全指向一个软删的主键），
/// 而「停止执行」有正当替代路径（转派给人类 / 改为「已取消」）。
pub const AI_TASK_DELETE_FORBIDDEN: i32 = 63202;

/// AI 用户禁止修改数据权限（HTTP 403）。
///
/// 覆盖 `business/permission/scope/*` 的**写**接口。即使把 AI 用户加进带
/// `business:permission:scope:write` 的角色也照样拒绝：数据范围是「AI 能看见
/// 哪些数据」的边界，让被约束者改约束本身就是提权。
pub const AI_DATA_SCOPE_FORBIDDEN: i32 = 63203;

/// AI 用户禁止管理 AI 用户 / 修改 profile（HTTP 403）。
///
/// 覆盖 `/system/user/ai/*` 的**写**接口（建号 / 改 SOUL.md / 删号）。
/// AI 用户若能改自己的 SOUL.md 或改别人的 profile，就等于能改自己的行为准则；
/// 这类能力只留给人类管理员。
pub const AI_AGENT_ADMIN_FORBIDDEN: i32 = 63204;

// ── T5b 追加（**633xx** 子段）：任务级令牌 / 派活 / 轮询 ──────────────────────
//
// 为什么单开 633xx：这组错误的判定主体不是「任务数据不合法」（630xx），也不是
// 「AI 这个账号被永久禁止做什么」（632xx），而是**这把短期令牌的边界**
// （它属于谁、还能不能用、能用在哪个端点）。混进 630xx 会让前端把「令牌过期」
// 与「任务状态不合法」用同一段文案兜住。
//
// 三个 403 有意分开（63301/63302/63303）：验收脚本与前端文案都要能把
// 「令牌无效」「令牌过期」「越权访问别的任务」区分开 —— 合成一个码，越权那条
// 断言就只能靠文案匹配，太脆。

/// 任务级令牌不可用（HTTP 403）：hash 无命中 / 任务已终结 / 非 AI 执行模式。
pub const TASK_AI_TOKEN_INVALID: i32 = 63301;

/// 任务级令牌已过期（HTTP 403）。
pub const TASK_AI_TOKEN_EXPIRED: i32 = 63302;

/// 令牌越权（HTTP 403）：路径任务 ≠ 令牌登记任务。
///
/// 这是「任务级令牌不能访问其它任务」这条验收项的判定码，单列出来才能在
/// 验收脚本里精确断言（而不是「反正 403 就算过」）。
pub const TASK_AI_TOKEN_SCOPE: i32 = 63303;

/// 端点不在令牌白名单内（HTTP 403）：带着任务令牌访问别的接口。
pub const TASK_AI_ENDPOINT_FORBIDDEN: i32 = 63304;

/// 派活失败（HTTP 502）：`hermes kanban create` 非 0 退出 / 超时 / 输出无法解析。
///
/// 语义是**上游（Hermes）失败**，不是云枢内部错误：任务此时仍然留在 TODO，
/// 派活人可直接重试。用 502 而不是 500 是为了让前端「重试」提示与真正的
/// 服务端 bug 区分开。
pub const TASK_DISPATCH_FAILED: i32 = 63305;

/// 任务已经派给 AI 了（HTTP 409）：`hermes_task_id` 非空且任务未终结。
///
/// 重复派活必须被拦住：新令牌会覆盖旧令牌，而旧令牌的明文已经写进了
/// 上一张 kanban body，覆盖后那一份就变成「拿了也白拿」——AI 会卡在 403 里。
/// 需要重派就先取消/收尾当前任务（`hermes_task_id` 清空），或显式指定重试。
pub const TASK_DISPATCH_CONFLICT: i32 = 63306;

/// 任务的负责人不是 AI 账号（HTTP 400）：派活前必须先把负责人改成一个 ROBOT 用户。
pub const TASK_DISPATCH_OWNER_NOT_ROBOT: i32 = 63307;

/// AI 进度回写参数不合法（HTTP 400）：`percent` 越界 / 既无 percent 也无 note。
pub const TASK_AI_PROGRESS_INVALID: i32 = 63308;

// ── 任务管理 · 项目分组（T7c）── **634xx 段** ────────────────────────────────
//
// 为什么单开 634xx 而不是接着 633xx：T5b 的 633xx 是「AI 执行者 / 任务令牌」子段，
// 语义边界清楚；项目分组是独立实体（`biz_project`），单开一段便于前端文案映射
// 与验收脚本按码值命中。634xx 与 64xxx（AI 用户）之间留空号，避免再遇到
// 「往满段里塞码会让既有自检断言失效」的问题（633xx 段的注释里记过这件事）。

/// 项目不存在 / 已软删 / **不在我的可见范围内**（HTTP 404）。
///
/// 「看不见」与「不存在」合并成同一个 404：项目只是逻辑分组，
/// 猜 id 也不该能区分「有这个东西但你没权」。
pub const PROJECT_NOT_FOUND: i32 = 63401;

/// 项目编码（`project_no`）重复（HTTP 400）。
pub const PROJECT_CODE_DUPLICATE: i32 = 63402;

/// 项目字段不合法（HTTP 400）：编码格式 / 名称超长 / 起止日期倒挂 / 负责人不存在。
pub const PROJECT_FIELD_INVALID: i32 = 63403;

/// 无权修改 / 删除该项目（HTTP 403）：既不是负责人也不是创建人，且没有对应权限码。
pub const PROJECT_FORBIDDEN: i32 = 63404;

/// 项目成员参数不合法（HTTP 400）：成员不存在 / 已停用 / 重复添加。
pub const PROJECT_MEMBER_INVALID: i32 = 63405;

/// 项目成员不存在（HTTP 404）。
pub const PROJECT_MEMBER_NOT_FOUND: i32 = 63406;

/// 无权把任务归入 / 移出项目（HTTP 403）。
///
/// 口径 = 任务负责人 ∪ 任务创建人 ∪ `business:task:update`（卡面规则 5）。
/// **不认「项目负责人」**：项目不是权限边界，负责人身份不能换来对别人任务的写权限。
pub const PROJECT_TASK_FORBIDDEN: i32 = 63407;

// ── E11 追加（**635xx** 子段）：任务模板 / 周期任务 / 燃尽图 ──────────────────
//
// 为什么单开 635xx：630xx（任务数据）/ 631xx（依赖与排期）/ 632xx（AI 主体硬拒）/
// 633xx（令牌）都不适合装「模板」「燃尽图区间」这两类错误 —— 它们的判定对象是
// **模板配置**与**只读统计的入参**，不是任务本身的状态。634xx 已被项目分组占用，
// 所以顺延到 635xx。前端 `error-code.ts` 按码值给文案，改值即文案错位。

/// 任务模板不存在或已软删（HTTP 404）。
pub const TASK_TEMPLATE_NOT_FOUND: i32 = 63501;

/// 任务模板字段非法（HTTP 400）：模板名 / 标题 / 优先级 / 工时 / 子任务。
pub const TASK_TEMPLATE_INVALID: i32 = 63502;

/// 周期任务的 cron 表达式非法 / 开启周期却没给 cron（HTTP 400）。
pub const TASK_TEMPLATE_CRON_INVALID: i32 = 63503;

/// 燃尽图日期区间非法（HTTP 400）：倒置或超过 366 天。
pub const TASK_BURNDOWN_RANGE_INVALID: i32 = 63504;

// ── 云枢 × Hermes · L1 AI 用户（AI 段）── **64xxx 段** ──────────────────────
//
// 为什么单开 64xxx：63xxx 段（任务管理）已自带「固定 9 个码」的自检断言，
// 往里塞码会让那条断言失效（等于放宽一条既有门禁）。AI 用户是独立业务域
// （profile 生命周期 + SOUL.md），单开一段最清楚。

/// AI 用户不存在或已删除（HTTP 404）。
pub const AI_AGENT_NOT_FOUND: i32 = 64001;

/// profile 名不合法（**命令注入的唯一入口**，HTTP 400）。
///
/// 触发条件：不命中 `^[a-z][a-z0-9-]{1,30}$`，或命中保留名 `default`。
/// 这条错误码出现即说明有调用方在试探，值得在日志/告警里盯。
pub const AI_AGENT_PROFILE_INVALID: i32 = 64002;

/// 用户名或 Hermes profile 名已被占用（HTTP 409）。
pub const AI_AGENT_PROFILE_EXISTS: i32 = 64003;

/// profile 目录不存在（例如磁盘上被外部删除，HTTP 404）。
pub const AI_AGENT_PROFILE_MISSING: i32 = 64004;

/// Hermes CLI 执行失败 / 超时 / CLI 不存在（HTTP 500，消息里带 stderr 前 500 字符）。
pub const AI_AGENT_HERMES_FAILED: i32 = 64005;

/// SOUL.md 内容非法（超上限，HTTP 400）。
pub const AI_AGENT_SOUL_INVALID: i32 = 64006;

/// SOUL.md 文件不存在（HTTP 404）。
pub const AI_AGENT_SOUL_MISSING: i32 = 64007;

/// SOUL.md 读写失败（IO 错误，HTTP 500）。
pub const AI_AGENT_SOUL_IO_FAILED: i32 = 64008;

/// 创建失败且回滚未完全成功（HTTP 500，消息里逐条列出残留项，需人工清理）。
pub const AI_AGENT_ROLLBACK_FAILED: i32 = 64009;

/// 职责（profile description）超长（HTTP 400）。
pub const AI_AGENT_DESCRIPTION_INVALID: i32 = 64010;

/// workspace 目录创建 / 删除失败（HTTP 500）。
pub const AI_AGENT_WORKSPACE_FAILED: i32 = 64011;

/// 模型覆盖不是 JSON 对象（HTTP 400）。
pub const AI_AGENT_MODEL_INVALID: i32 = 64012;

/// AI 用户的账号类型受保护：禁止用通用用户接口改 / 删 `user_type='ROBOT'` 的账号（HTTP 409）。
///
/// 不变式：`user_type='ROBOT'` ⇔ `sys_ai_agent` 有该 user_id 的记录。
/// 通用接口动它会打破「账号 ↔ 配置行」的一一对应（配置行还在、账号变了；
/// 或账号被删、Hermes profile 与 workspace 留在磁盘上没人清理），
/// 所以这类操作必须走 `/api/system/user/ai/*`。
pub const AI_AGENT_USER_TYPE_LOCKED: i32 = 64013;

/// AI 员工的 **profile 级凭据** 不可用（HTTP 500）：`.env` 里缺当前 provider 的 API key，
/// 或凭据文件权限不是 600。
///
/// # 为什么单列一个码（2026-10-29 端到端事故）
///
/// `hermes profile create` 建出的 profile **不继承**全局 `~/.hermes/.env`，而 Hermes worker
/// 的凭据池只读 **profile 级** `.env`。缺 key 的账号表现极具欺骗性：建号 200、派活成功、
/// 库与磁盘都「正常」，worker 一执行就
/// `AuthError: No usable credentials found for provider 'deepseek'` → 连 crash 两次 → blocked。
///
/// 用 64008（AI 配置文件 IO 失败）会把「业务上不可用」伪装成「磁盘读写故障」，
/// 运维搜码也搜不到这条真实故障形态，因此单列 64014：**能建号但不能干活**统一落到这个码上。
pub const AI_AGENT_CREDENTIAL_MISSING: i32 = 64014;

// ── 业务审批流（批次 D）── **65xxx 段** ──────────────────────────────────
//
// 依据：`docs/批次D-工作流审批-技术架构.md` §1.9（65xxx 整段空闲，实测无占用）、
//       §10.3（22 个码的码表，本段逐字落地）。
//
// 为什么单开一段：审批流是**独立业务域**（定义 / 实例 / 待办 / 抄送），
// 且架构 §10.3 的 22 个码里有一半是「运行期状态冲突」（65020–65024），
// 与 63xxx（任务）的语义边界必须清楚 —— 前端文案映射按码值命中，不许改值。
//
// 本卡（D1）只用到定义侧 12 个（65001/65005/65010–65017/65021/65022）；
// 运行侧 65002/65003/65004/65018–65020/65023/65024/65030/65031 由 D2/D3 使用，
// **一并定义**：码值一旦对外发布就不能改，先占位比事后补更安全（架构 §10.3 的码表是整体契约）。

/// 流程定义不存在 / 已软删（HTTP 404）。
pub const FLOW_NOT_FOUND: i32 = 65001;

/// 审批单不存在或无权查看（HTTP 404）。
pub const FLOW_INSTANCE_NOT_FOUND: i32 = 65002;

/// 待办不存在或已处理（HTTP 404）。
pub const FLOW_TASK_NOT_FOUND: i32 = 65003;

/// 无权执行该审批操作（HTTP 403）。
pub const FLOW_FORBIDDEN: i32 = 65004;

/// 审批流字段取值非法（HTTP 400）：流程编码格式 / 名称超长 / 状态取值 / 版本不属于该流程。
pub const FLOW_FIELD_INVALID: i32 = 65005;

/// 流程结构非法（HTTP 400）：V1 开始节点数、V2 结束节点、V4 可达性（R1/R2/R11/R12）。
pub const FLOW_GRAPH_STRUCTURE: i32 = 65010;

/// 流程存在环（HTTP 400）：文案必须给出**闭合路径**（架构 §3.7 V3）。
pub const FLOW_GRAPH_CYCLE: i32 = 65011;

/// 节点出入度非法（HTTP 400）：R4/R5/R6/R7（审批节点多出边、条件节点单出边等）。
pub const FLOW_GRAPH_DEGREE: i32 = 65012;

/// 条件节点缺少兜底分支（HTTP 400）：R13（架构 V7）。
pub const FLOW_GRAPH_DEFAULT_EDGE: i32 = 65013;

/// 汇聚方式与分支来源不匹配（HTTP 400）：R8（禁止混流汇聚，架构 V8 / D6）。
pub const FLOW_GRAPH_JOIN_MODE: i32 = 65014;

/// 节点未配置审批人 / 会签模式或阈值缺失（HTTP 400）：R9/R14（架构 V9/V10）。
pub const FLOW_NODE_ASSIGNEE_MISSING: i32 = 65015;

/// 条件表达式非法（HTTP 400）：R10（结构 / 深度 / 节点数 / 字段白名单 / 字面量类型）。
pub const FLOW_CONDITION_INVALID: i32 = 65016;

/// 流程过大（HTTP 400）：R15（节点 ≤ 200 / 边 ≤ 400，架构 V12）。
pub const FLOW_GRAPH_TOO_LARGE: i32 = 65017;

/// 节点「X」没有可用审批人（HTTP 422，运行期解析为空）。
pub const FLOW_NO_ASSIGNEE: i32 = 65018;

/// 节点「X」的审批人是发起人本人且流程不允许自审（HTTP 422）。
pub const FLOW_SELF_APPROVAL_FORBIDDEN: i32 = 65019;

/// 该待办已被处理（HTTP 409，**幂等成功语义**：前端按「已处理，刷新」展示，不弹错误）。
pub const FLOW_TASK_ALREADY_DONE: i32 = 65020;

/// 流程有进行中的审批单，不能删除（HTTP 409）。
pub const FLOW_DEF_IN_USE: i32 = 65021;

/// 该版本已发布，不能直接编辑（HTTP 409）：改流程必须「另存为新草稿」。
pub const FLOW_VERSION_NOT_EDITABLE: i32 = 65022;

/// 已有人审批，不能撤回（HTTP 409）。
pub const FLOW_INSTANCE_NOT_WITHDRAWABLE: i32 = 65023;

/// 审批单已结束（HTTP 409）。
pub const FLOW_INSTANCE_NOT_PENDING: i32 = 65024;

/// 该任务正在审批中，请在审批单里处理（HTTP 409，**任务侧使用**，架构 D19）。
pub const TASK_APPROVAL_PENDING: i32 = 65030;

/// D3a 卡面的命名别名：**与 [`TASK_APPROVAL_PENDING`] 同码同义**（`65030`）。
///
/// 卡面写「新增错误码 `TASK_FLOW_GATE_BLOCKED = 65030`」，但 65030 已由 D1 按架构 §10.3
/// 的码表**预先定义**过（那是整体契约，码值不许改）。所以这里落成**别名**而不是第二个
/// 定义体：任务侧「审批中」只有一个语义，两处各写一遍 `= 65030` 迟早被人改歪一个。
pub const TASK_FLOW_GATE_BLOCKED: i32 = TASK_APPROVAL_PENDING;

/// 超时自动通过需要流程级开关（`flow_def.allow_auto_approve`，HTTP 422，架构 D10）。
pub const FLOW_TIMEOUT_CONFIG_INVALID: i32 = 65031;

/// 退回目标非法（HTTP 400）：`return_to` 取值不认识 / 指定的 `node_key` 不在图上 /
/// 非管理员选了「退回任意节点」（架构 §4.6.1）。
pub const FLOW_RETURN_TARGET_INVALID: i32 = 65025;

/// 转办非法（HTTP 400/422）：目标用户不存在 / 已停用 / 是机器人（且节点不允许）/
/// 转给自己 / 目标已有同名待办（架构 §4.8.1）。
pub const FLOW_TRANSFER_INVALID: i32 = 65026;

/// 加签非法（HTTP 400/422）：`user_ids` 为空 / 超过 10 个 / 节点不是 `ACTIVE` /
/// 被加签人已在审批人里（架构 §4.8.2）。
pub const FLOW_ADD_SIGN_INVALID: i32 = 65027;

/// 该待办不属于当前用户（HTTP 403）：与 `65004` 的区别是它专指「行级归属」不匹配
/// （「这条待办不是你的」），前端文案不同（架构 §10.2 的中间件闸只挂粗粒度码）。
pub const FLOW_TASK_NOT_MINE: i32 = 65028;

/// 审批人来源「动态表达式」本期未实现（HTTP 501，架构 D5 / §4.3.1）。
///
/// 刻意**不**退化成「空审批人 → 自动通过」：一个配了表达式审批人的节点如果静默跳过，
/// 等于把「谁审批」变成没人审批，是坑 3/坑 5 的叠加形态。
pub const FLOW_EXPRESSION_UNSUPPORTED: i32 = 65032;

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

    /// T 段码位自检：63xxx 段 20 个码齐全、码值不重复、与 62xxx 段不重叠。
    ///
    /// 与另外两条同性质：**码值一旦发给前端就不能改**（`error-code.ts` 的文案映射
    /// 按码值命中）。新增 T 段错误时只许顺延 63014+，不许改已有值。
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
            // T2 追加：状态机 / 进度锁定 / 打回原因
            (TASK_STATUS_TRANSITION_INVALID, 63010),
            (TASK_STATUS_FORBIDDEN, 63011),
            (TASK_PROGRESS_LOCKED, 63012),
            (TASK_REJECT_REASON_REQUIRED, 63013),
            // T3 追加：分派 / 转派 / 协作人
            (TASK_MEMBER_LIMIT, 63014),
            (TASK_MEMBER_PROTECTED, 63015),
            (TASK_ROBOT_FORBIDDEN, 63016),
            (TASK_ASSIGN_SAME_OWNER, 63017),
            (TASK_ASSIGN_CONFLICT, 63018),
            (TASK_MEMBER_INVALID, 63019),
            (TASK_MEMBER_NOT_FOUND, 63020),
            // T4 追加：依赖环检测 / 前置阻塞 / 依赖不合法 / 依赖不存在 / 甘特时间窗
            (TASK_DEPENDENCY_CYCLE, 63101),
            (TASK_DEPENDENCY_BLOCKED, 63102),
            (TASK_DEPENDENCY_INVALID, 63103),
            (TASK_DEPENDENCY_NOT_FOUND, 63104),
            (TASK_GANTT_RANGE_INVALID, 63105),
            // T7a 追加：甘特拖拽改期的前置 / 后继 / 父子冲突
            (TASK_SCHEDULE_PREDECESSOR, 63110),
            (TASK_SCHEDULE_SUCCESSOR, 63111),
            (TASK_SCHEDULE_PARENT_RANGE, 63112),
            // T5b 追加：任务级令牌 / 派活 / 进度回写
            (TASK_AI_TOKEN_INVALID, 63301),
            (TASK_AI_TOKEN_EXPIRED, 63302),
            (TASK_AI_TOKEN_SCOPE, 63303),
            (TASK_AI_ENDPOINT_FORBIDDEN, 63304),
            (TASK_DISPATCH_FAILED, 63305),
            (TASK_DISPATCH_CONFLICT, 63306),
            (TASK_DISPATCH_OWNER_NOT_ROBOT, 63307),
            (TASK_AI_PROGRESS_INVALID, 63308),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "63xxx 段码值不得改动");
        }
        // 与批次 C / 偏好体系的 62xxx 段严格不重叠
        assert!(codes[0].0 > 62024, "63xxx 段必须排在 62xxx 段之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "63xxx 段不得有重复码值");
        println!(
            "[码表] 63xxx 段 {} 个码齐全且不重复（63001–63020 + 63101–63105 + 63110–63112 + 63301–63308）✓",
            codes.len()
        );
    }

    /// 项目分组段自检：634xx 段 7 个码齐全、码值不重复、与 63xxx / 64xxx 都不重叠。
    ///
    /// T7c 契约：前端文案映射（`error-code.ts`）与验收脚本按码值命中，**不许改值**。
    #[test]
    fn project_error_codes_are_stable() {
        let codes = [
            (PROJECT_NOT_FOUND, 63401),
            (PROJECT_CODE_DUPLICATE, 63402),
            (PROJECT_FIELD_INVALID, 63403),
            (PROJECT_FORBIDDEN, 63404),
            (PROJECT_MEMBER_INVALID, 63405),
            (PROJECT_MEMBER_NOT_FOUND, 63406),
            (PROJECT_TASK_FORBIDDEN, 63407),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "634xx 段码值不得改动");
        }
        assert_eq!(codes.len(), 7, "634xx 段必须是 7 个码（63401–63407）");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "634xx 段不得有重复码值");
        assert!(
            codes.iter().all(|(c, _)| *c > 63308 && *c < 64001),
            "634xx 段必须落在 63308 与 64001 之间（不侵占 AI 用户段）"
        );
        println!(
            "[码表] 634xx 段 {} 个码齐全且不重复（63401–63407）✓",
            codes.len()
        );
    }

    /// AI 执行者硬拒段自检：632xx 段 4 个码齐全、码值不重复、与 63xxx / 64xxx 都不重叠。
    ///
    /// 这四条是 T5 卡面的契约（`63201` 禁登录 / `63202` 禁删任务 / `63203` 禁改数据权限 /
    /// `63204` 禁管 AI 用户），前端文案映射与验收脚本按码值命中，**不许改值**。
    ///
    /// 同时钉住「分派没有 632xx 码」这条设计约束：修正 11 要求 AI 用户能被角色授予
    /// 分派权，一旦有人给它加一个「AI 不能分派」的硬拒码，这里就会因为码数变成 5 而红。
    #[test]
    fn ai_executor_error_codes_are_stable() {
        let codes = [
            (AI_LOGIN_FORBIDDEN, 63201),
            (AI_TASK_DELETE_FORBIDDEN, 63202),
            (AI_DATA_SCOPE_FORBIDDEN, 63203),
            (AI_AGENT_ADMIN_FORBIDDEN, 63204),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "632xx 段码值不得改动");
        }
        assert_eq!(
            codes.len(),
            4,
            "632xx 段必须是 4 个码（63201–63204）；给「分派」加硬拒码违反修正 11"
        );
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "632xx 段不得有重复码值");
        // 与任务域 63xxx 段不重叠（63xxx 最大 63105），与 AI 用户 64xxx 段不重叠
        assert!(
            codes.iter().all(|(c, _)| *c > 63105 && *c < 64001),
            "632xx 段必须落在 63105 与 64001 之间"
        );
        // 63016 保留但不再返回：删任务一律 63202（值不回收，历史日志仍可解析）
        assert_ne!(
            AI_TASK_DELETE_FORBIDDEN, TASK_ROBOT_FORBIDDEN,
            "删任务的 AI 硬拒码已迁移到 63202"
        );
        println!(
            "[码表] 632xx 段 {} 个码齐全且不重复（63201–63204；63016 保留不回收）✓",
            codes.len()
        );
    }

    /// AI 段码位自检：64xxx 段 12 个码齐全、码值不重复、与 63xxx 段不重叠。
    ///
    /// 与另外三条同性质：**码值一旦发给前端就不能改**（`frontend/src/api/error-code.ts`
    /// 的文案映射按码值命中）。新增 AI 段错误时只许顺延 64013+。
    #[test]
    fn ai_agent_error_codes_are_stable() {
        let codes = [
            (AI_AGENT_NOT_FOUND, 64001),
            (AI_AGENT_PROFILE_INVALID, 64002),
            (AI_AGENT_PROFILE_EXISTS, 64003),
            (AI_AGENT_PROFILE_MISSING, 64004),
            (AI_AGENT_HERMES_FAILED, 64005),
            (AI_AGENT_SOUL_INVALID, 64006),
            (AI_AGENT_SOUL_MISSING, 64007),
            (AI_AGENT_SOUL_IO_FAILED, 64008),
            (AI_AGENT_ROLLBACK_FAILED, 64009),
            (AI_AGENT_DESCRIPTION_INVALID, 64010),
            (AI_AGENT_WORKSPACE_FAILED, 64011),
            (AI_AGENT_MODEL_INVALID, 64012),
            (AI_AGENT_USER_TYPE_LOCKED, 64013),
            (AI_AGENT_CREDENTIAL_MISSING, 64014),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "64xxx 段码值不得改动");
        }
        // 与任务管理 63xxx 段严格不重叠
        assert!(codes[0].0 > 63009, "64xxx 段必须排在 63xxx 段之后");
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "64xxx 段不得有重复码值");
        assert_eq!(unique.len(), 14, "64xxx 段必须是 14 个码（64001–64014）");
        println!(
            "[码表] 64xxx 段 {} 个码齐全且不重复（64001–64014）✓",
            codes.len()
        );
    }

    /// 审批流段自检：65xxx 段 22 个码齐全、码值不重复、与 64xxx 段不重叠。
    ///
    /// D1 契约：架构 §10.3 的码表是**整体契约**（前端文案映射按码值命中），
    /// 定义侧先用 12 个，运行侧 10 个由 D2/D3 使用 —— 值不许改，也不许重复。
    #[test]
    fn flow_error_codes_are_stable() {
        let codes = [
            (FLOW_NOT_FOUND, 65001),
            (FLOW_INSTANCE_NOT_FOUND, 65002),
            (FLOW_TASK_NOT_FOUND, 65003),
            (FLOW_FORBIDDEN, 65004),
            (FLOW_FIELD_INVALID, 65005),
            (FLOW_GRAPH_STRUCTURE, 65010),
            (FLOW_GRAPH_CYCLE, 65011),
            (FLOW_GRAPH_DEGREE, 65012),
            (FLOW_GRAPH_DEFAULT_EDGE, 65013),
            (FLOW_GRAPH_JOIN_MODE, 65014),
            (FLOW_NODE_ASSIGNEE_MISSING, 65015),
            (FLOW_CONDITION_INVALID, 65016),
            (FLOW_GRAPH_TOO_LARGE, 65017),
            (FLOW_NO_ASSIGNEE, 65018),
            (FLOW_SELF_APPROVAL_FORBIDDEN, 65019),
            (FLOW_TASK_ALREADY_DONE, 65020),
            (FLOW_DEF_IN_USE, 65021),
            (FLOW_VERSION_NOT_EDITABLE, 65022),
            (FLOW_INSTANCE_NOT_WITHDRAWABLE, 65023),
            (FLOW_INSTANCE_NOT_PENDING, 65024),
            (TASK_APPROVAL_PENDING, 65030),
            (FLOW_TIMEOUT_CONFIG_INVALID, 65031),
            (FLOW_RETURN_TARGET_INVALID, 65025),
            (FLOW_TRANSFER_INVALID, 65026),
            (FLOW_ADD_SIGN_INVALID, 65027),
            (FLOW_TASK_NOT_MINE, 65028),
            (FLOW_EXPRESSION_UNSUPPORTED, 65032),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "65xxx 段码值不得改动");
        }
        // D3a：卡面命名的 `TASK_FLOW_GATE_BLOCKED` 必须仍是 65030（别名，不新增码值）
        assert_eq!(TASK_FLOW_GATE_BLOCKED, 65030, "D3a 别名必须仍指向 65030");
        assert_eq!(TASK_FLOW_GATE_BLOCKED, TASK_APPROVAL_PENDING);
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "65xxx 段不得有重复码值");
        assert_eq!(
            unique.len(),
            27,
            "65xxx 段必须是 27 个码（架构 §10.3 的 22 个 + D2 追加的 5 个运行侧细分码）"
        );
        assert!(
            codes.iter().all(|(code, _)| *code > 64014 && *code < 66000),
            "65xxx 段必须落在 64014 与 66000 之间（不侵占 AI 用户段）"
        );
        println!(
            "[码表] 65xxx 段 {} 个码齐全且不重复（65001–65005 / 65010–65028 / 65030–65032）✓",
            codes.len()
        );
    }
}
