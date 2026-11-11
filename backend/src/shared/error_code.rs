//! 业务错误码常量。
//!
//! # 与 HTTP 状态码的分工
//!
//! 框架 `nik::codes` 定义的是 **HTTP 语义码**（`OK = 0`、`BAD_REQUEST = 400`、
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
//! `0` 表示成功（见 `nik::codes::OK`），不在本模块定义。
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
//! | `66xxx` | 批次 D（ERP 主数据接口层 W1c：`66001`–`66006`；后续 ERP 卡续用 `66007`–`66021`，其中 `66014`–`66021` 为合同域；`66022`–`66026` 采购单↔合同、`66027`–`66029` 入库↔采购、`66030` 单位止血、`66031`–`66032` 过账复核来源采购单、`66033` 审批流未配置默认拒绝、`66034`–`66035` 出库单、`66036` 信用额度、`66037` 预算超支（FIN-1）、`66038`–`66040` 固定资产期间/批次/处置（FIN-2）、`66041`–`66043` 费用报销状态/明细合计/入账期间（FIN-3）、`66044` 收款退款过账审批前置（SO-14 遗留 D10）、`66045` 销售订单↔合同客户不符（SO-27）、`66046` 资产停用/启用状态非法（FIN-ASSET-ST02）、`66054` 主数据编码禁止手工指定（CODE-08）、`66055` 成本对象关批前置闸、`66056` 批次批号重复（ARCH-9）、`66057` 领料挂的成本对象非 OPEN（ARCH-9）、`66058` 工时×费率与金额不符（ARCH-9）、`66059` 费率三级全空（ARCH-9）、`66060`–`66061` 费用台账科目/金额（ARCH-9）、`66062`–`66063` 分摊池无在产对象/基数合计 0、`66064` calc 料归集来源领料行账面非法、`66065` 池与 4101 借方不平（ARCH-9）、`66066` 分摊重算期间批次已锁定（ARCH-9）、`66067`–`66071` calc 第 6 段产量/进度非法、零成本完工、上期已红冲、计算单锁定、无成本批次（ARCH-9））、`66072`–`66074` 结算批次重复/状态闸/完工入库重复结算（ARCH-9 B3-1 · B3-4）） |
//! | `67xxx` | PDF 导出（PDF-01：`67001` 渲染服务不可用 / `67002` 超时 / `67003` 模板错误 / `67004` 参数非法 / `67005` 请求过大） |
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

/// 权限已变更，需重新登录（HTTP 401）。
///
/// 触发面：`sys_user.token_version` 与令牌 claim `ver` 不一致 —— 改密 / 改角色 /
/// 改角色权限 / 改菜单授权 / 停用 / 删除账号都会推进版本号
/// （见 `shared::session_cache` 与 `middleware::session_guard`）。
///
/// 与 [`AUTH_TOKEN_INVALID`] 的区别：令牌本身签名有效、也未过期，只是**它承载的
/// 权限快照已过期**。前端据此文案提示「权限已变更，请重新登录」并直接跳登录页。
pub const AUTH_PERMISSION_CHANGED: i32 = 20008;

/// 因长时间未操作已自动退出登录（HTTP 401）。
///
/// 触发面：会话空闲时长超过 `sys_config.auth.idle_timeout_minutes`
/// （见 `shared::idle_session` 与 `middleware::session_guard`）。
///
/// 前端据此**不再尝试刷新令牌**（刷新同样会被拒），直接清状态回登录页并提示
/// 「因长时间未操作已自动退出」，登录成功后回到原页面。
pub const AUTH_IDLE_TIMEOUT: i32 = 20009;

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

// ── T11 追加（63021）：父任务取消后的子任务守卫 ──────────────────────────

/// 父任务已取消，子任务不能继续推进（HTTP 409）。
///
/// # 为什么必须是一等业务码、而不是复用 63010（非法流转）
///
/// 63010 的语义是「这个 `from → to` 组合不存在」，提示用户「刷新看看当前状态」；
/// 本码的语义是「流转本身合法，但**父任务已经作废**」，用户要做的动作是
/// 「先恢复父任务（或另起一条线）」，两者文案与后续动作都不同。
///
/// 触发面：`PATCH /business/task/{id}/status`（子任务自己 → `CANCELLED` 除外，
/// 那是幂等收敛，见 `task::model::blocks_child_advance`）。
pub const TASK_PARENT_CANCELLED: i32 = 63021;

// ── P2-9 追加（63022–63023）：任务挂业务单据（DC-19）─────────────────────

/// 任务挂的「单据类型」不在白名单（HTTP 400）。
///
/// 白名单唯一来源：[`crate::business::task::doc_ref::DOC_TYPES`]
/// （`PURCHASE_ORDER` / `STOCK_IN` / `CONTRACT` / `PAYABLE` / `VOUCHER`）。
///
/// # 为什么不复用 63004（字段取值非法）
///
/// 「类型不认识」与「这个字段的格式不对」的下一步动作不同：前者是前端该给出
/// 可选项（下拉 / 更新版本），后者是用户填错了格式。前端文案映射按码值命中，
/// 合并会让「请选择单据类型」被写成「格式错误」。
pub const TASK_DOC_TYPE_INVALID: i32 = 63022;

/// 任务挂的单据不存在 / 已软删（HTTP 400）。
///
/// `doc_id` 是弱引用（5 张表之一，无外键），存在性只能在 handler 里查一次
/// （[`crate::business::task::doc_ref::resolve`]）。单据被软删后**不允许**再被
/// 新任务挂上去 —— 与 ERP 侧所有引用校验口径一致（`deleted_at IS NULL`）。
///
/// # 为什么是 400 而不是 404
///
/// 404 的语义是「你要的**任务**不存在」；这里是「任务参数里引用的单据无效」，
/// 动作是改请求参数后重试，不是换一个任务 id。两者混用会让前端把用户引导到
/// 「任务已被删除」的错误提示上。
pub const TASK_DOC_NOT_FOUND: i32 = 63023;

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

/// P5 卡面的命名别名：**与 [`TASK_APPROVAL_PENDING`] 同码同义**（`65030`），给**调拨单**用。
///
/// 调拨单（`biz_stock_transfer`）提交审批后挂着在跑的审批单时，人工 `PENDING → APPROVED`
/// 会被门闸拦住（`erp::transfer_model::flow_gate_blocked`），前端按 65030 统一提示
/// 「正在审批中」。与上面那条一样是别名而不是第二个定义体：65030 是架构 §10.3 码表里的
/// 既有契约，语义就是「这条单据正在审批中」，不因业务表不同而分裂。
pub const TRANSFER_FLOW_GATE_BLOCKED: i32 = TASK_APPROVAL_PENDING;

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

/// 审批人表达式不合法（HTTP 400）：**保存期结构校验** + **发布期 R16**
/// （批次 D · P1-A2，EXPRESSION-DSL 规范 §8.5 / §8.6）。
///
/// 与 [`FLOW_EXPRESSION_UNSUPPORTED`]（`501`）的分工要说清，别混：
///
/// | 码 | 何时 | 用户看到 |
/// |---|---|---|
/// | `65033`（本码） | 用户**刚配完**、正在存草稿 / 发布 | 「第 2 条规则缺少条件」这类**可立即修**的原因 |
/// | `65032` | 库里的数据比代码新（旧快照 / 被绕过改库 / 未来版本回滚） | 「数据无法识别，请联系管理员」 |
///
/// 这条码还有一个专用场景：表达式引用了保留词 `PREV_APPROVER`（能力 B，P2 才开放）。
/// D-E8 要求它在**发布期**就被拒，而不是落 `501` —— 「能存进去、跑起来才炸」正是本期要消灭的形态。
pub const FLOW_EXPRESSION_INVALID: i32 = 65033;

// ==================== 流程配置化（P-FLOW-1A，设计 §2.7 / §3.8） ====================

/// 节点配置与节点类型不匹配（HTTP 400）：如 `APPROVAL` 上配了 `config.task`、
/// `TASK_NODE` 缺 `config.task`、`config.task` 形状 / 未知键 / 交叉约束不满足（R18 的一半）。
pub const FLOW_NODE_CONFIG_MISMATCH: i32 = 65034;

/// `TASK_NODE` 的 `need_review` × `on_complete` 组合非法（HTTP 400，R18）。
///
/// 典型两条：`VERIFY + need_review=false`（声明要验收却没开验收态 = 配置静默失效）、
/// `APPROVE + need_review=true`（发起人验收一次 + 审批人再签一次 = 「点两次提交」）。
/// 1A 阶段 `on_complete=APPROVE` 本身也落这里（引擎分支在 P-FLOW-1B）。
pub const FLOW_TASK_NODE_INVALID: i32 = 65035;

/// `NOTIFY_NODE` 的模板不存在 / 占位符非法（HTTP 400，R19）。
pub const FLOW_NOTIFY_TEMPLATE_INVALID: i32 = 65036;

/// 模板变量路径不在白名单 / `task.*` 用在非 `TASK_ACCEPT` 流程（HTTP 400，R21）。
pub const FLOW_TEMPLATE_VAR_INVALID: i32 = 65037;

/// 流程任务状态与实例状态冲突（HTTP 409）：任务完成时实例已终止 / 已撤回等（P-FLOW-1B 用）。
pub const FLOW_TASK_STATE_CONFLICT: i32 = 65038;

/// 预演（preview）参数非法（HTTP 400）：变量缺失（`strict=true`）/ 渲染结果超长 / 模板语法错。
pub const FLOW_PREVIEW_INVALID: i32 = 65039;

/// 角色升级链配置非法（HTTP 422）：`escalation_role_id` 自引用 / 成环 / 指向不存在（已软删）的角色。
///
/// P3-02（ARCH-8 场景 B）：升级链是**组织配置**，配错时必须显式失败而不是静默跳过 ——
/// 静默跳过的后果是「以为有上级兜底，实际没有」，比直接报错更难排。
pub const FLOW_ESCALATION_INVALID: i32 = 65040;

/// 角色升级链超过 8 级（HTTP 422，P3-02）：正常组织不会这么深，超过即视为配置错误
/// （多半是环没被 visited 集合捕获前就已经很长，或误把整张组织表串成了链）。
pub const FLOW_ESCALATION_TOO_DEEP: i32 = 65041;

/// 节点兜底人配置无效（HTTP 422，P3-03 / ARCH-8 场景 C）。
///
/// 兜底人是「升级链走完仍无人」的最后一道人手兜底，配错时必须显式失败：
/// 用户不存在 / 已被删除 / 已停用 / 是机器人（节点没开 `allow_robot`）/
/// 兜底角色不存在 / 兜底角色没有可用成员 —— 静默当成「没配兜底」会让
/// 运维以为已经兜住了，实际单据照样发不出去。
pub const FLOW_FALLBACK_INVALID: i32 = 65042;

/// 审批委托配置非法（HTTP 422，P3-04 / ARCH-8 场景 E）。
///
/// 三种情形共用一码（都是「这条委托录错了」，前端提示与修法完全一致）：
/// `end_at <= start_at`（区间非法）/ 受托人 = 委托人（自委托）/ 与本人已存在的
/// 委托区间重叠（无论那条是否停用 —— 重新启用就会撞）。
///
/// 为什么重叠必须显式失败：同一委托人同时委托给两个人时，「谁代谁」不确定，
/// 引擎只能随机挑一档，两个人互相等对方的待办 ⇒ 单据堵到两个人都回来。
pub const FLOW_DELEGATE_INVALID: i32 = 65043;

/// 自审金额超限（HTTP 422，P3-05 / ARCH-8 §8 待确认问题 2）。
///
/// 节点 `self_approval=ALLOW` 时发起人本人可以被派到待办（受控例外），但
/// **金额超过系统参数 `erp.separation_of_duties.self_approval_max_amount`
/// 的自审上限时不许自审**：引擎把他从「能签的人」降级成 `SKIPPED` 留痕，
/// 先走 P3-02 升级链 / P3-03 兜底人找人；仍找不到人时整单按本码 **422**
/// 失败（**绝不静默自动通过** —— 那等于「大额单据没人签就生效」）。
///
/// 参数缺失 / 值为 0 = 不限额（默认口径），此时本码不会出现。
pub const FLOW_SELF_APPROVAL_AMOUNT_LIMIT: i32 = 65044;

/// 子流程定义非法 / 未发布（HTTP 422，P3-08 / ARCH-5 §4.14 R27）。
///
/// 四种情形共用一码（都是「这个 SUBPROCESS 节点指向的目标不能用」）：
/// `subprocess_def_id` 为空 / 指向的定义已软删 / 定义不是 `PUBLISHED`
/// （没有 `current_version_id`）/ `config.sub.param_mapping` 映射到了
/// 子流程表单里不存在的键。
///
/// 为什么发布期就要拦：运行期才失败的后果是**父流程停在 SUBPROCESS 节点 ACTIVE**，
/// 而节点上没有待办、没有超时、没有人能点 —— 单据永久卡死（与 `65018` 治理思路一致）。
pub const FLOW_SUBPROCESS_DEF_INVALID: i32 = 65045;

/// 子流程自递归 / 嵌套过深（HTTP 422，P3-08 / ARCH-5 §4.14 R27）。
///
/// 两种情形共用一码：① `subprocess_def_id` 指向自己（直接的 A→A，或间接的 A→B→A 环）；
/// ② 嵌套深度超过 [`crate::business::flow::subprocess::MAX_NEST_DEPTH`]（= 3 层）。
///
/// 递归子流程的后果不是「慢」而是**无限发起**：每层都是一个真实例，
/// 一条单据一次能生出无数张子单（且都能撞在同一个父节点上）。
pub const FLOW_SUBPROCESS_RECURSION: i32 = 65046;

/// 子实例发起失败（HTTP 422，P3-08 / ARCH-5 §4.14）。
///
/// 事务内建子实例失败（子流程没有可发的版本 / 子流程没有 START / 子流程派不出审批人 …）。
/// 与 [`FLOW_SUBPROCESS_DEF_INVALID`] 的分界：那个是**静态配置问题**（发布期就能查出来），
/// 本码是**运行期**才可能出现的失败（同一份配置昨天能发、今天因为角色被删发不出去）。
pub const FLOW_SUBPROCESS_START_FAILED: i32 = 65047;

/// 全员会签节点没有兜底（HTTP 422，E2E07-FIX-1 / 发布校验 R28）。
///
/// `approval_mode = ALL` 的节点推进条件是「所有候选人都签过」：候选人里只要有一个
/// **永远签不了**（离职未摘角色 / 停用 / 口令是坏号 / 长期出差），节点就永远凑不齐票 ——
/// 没有待办可点、没有报错、界面上也没有按钮，单据永久停在 `PENDING_APPROVAL`。
/// E2E-06（预算）与 E2E-07（合同）各实测一次，最终都只能改库。
///
/// 发布期拦这个组合：`ALL` + `timeout_hours` 为空 + 未配兜底人（`fallback_user_id` /
/// `fallback_role_id`）。**只有 `timeout_action` 不算**——`flow::timeout` 的扫描谓词是
/// `timeout_hours IS NOT NULL`，没有小时数就永远不会被扫到。
pub const FLOW_ALL_COUNTERSIGN_NO_GUARD: i32 = 65048;

// ==================== 66xxx：ERP 主数据 / 仓储（批次 D · W1c） ====================
//
// 码段说明：64xxx 是云枢 × Hermes，65xxx 是审批流；ERP 接口层（W1c）新开 66xxx，
// 与「校验失败一律 10001」区分开 —— 前端要按码值给不同文案
// （「编码已存在」要标红到输入框，「上级仓库不存在」要提示去别处建）。

/// 编码重复（HTTP 409）：同一唯一索引范围内的编码已存在
/// （`unit_code` / `warehouse_code` / `material_code` / `supplier_code` /
/// 仓库内唯一的 `location_code`）。软删后可复用，这里只拦「未删除行」的重复。
pub const ERP_CODE_CONFLICT: i32 = 66001;

/// 弱引用不存在（HTTP 400）：上级仓库 / 折算基准单位 / 库位所属仓库 / 计量单位 /
/// 默认仓库 / 存货科目 / 应付科目 在库里查不到（含已软删）。
///
/// 为什么不是 404：404 表达的是「请求的资源（URL 上的 id）不存在」，
/// 这里缺的是**请求体里引用的另一个资源**，请求本身是合法的。
pub const ERP_REFERENCE_MISSING: i32 = 66002;

/// 仓库还有下级 / 库位（HTTP 409）：ERP 不做级联删除
/// （删父留下孤儿物化路径，比报错难救得多）。
pub const ERP_WAREHOUSE_HAS_CHILDREN: i32 = 66003;

/// 仓库层级超限（HTTP 400）：V0137 §1.2 定死三级（总仓 / 分仓 / 库房），第 4 层拒绝。
pub const ERP_WAREHOUSE_LEVEL_LIMIT: i32 = 66004;

/// 库位只能挂叶子仓库（HTTP 400）：挂到非叶子仓库上会让「按仓库找库位」出现歧义
/// （父仓的库位到底属于父还是子）。
pub const ERP_WAREHOUSE_NOT_LEAF: i32 = 66005;

/// 查询 / 路径参数非法（HTTP 400）：最典型的是 UUID 解析失败。
///
/// 与 [`PARAM_INVALID`] 分开是为了钉住「筛选条件里的 UUID 非法**绝不静默忽略**」：
/// 忽略等于把「按仓库筛库位」变成「返回全部库位」，是静默错数据。
pub const ERP_FILTER_INVALID: i32 = 66006;

/// 物料未开启一物一码（HTTP 400）：批量生成序列号要求 `biz_material.serial_required = TRUE`。
///
/// 为什么不是 `10001`：前端要给「去物料主数据里打开这个开关」的动作指引，
/// 与「参数填错了」是两种完全不同的处理方式。
pub const ERP_SERIAL_NOT_REQUIRED: i32 = 66007;

/// 序列号状态流转非法（HTTP 400）：状态机单向不可逆
/// （只允许 `IN_STOCK→OUTBOUND` / `IN_STOCK→SCRAPPED` / `OUTBOUND→SCRAPPED`）。
///
/// 与 [`ERP_FILTER_INVALID`] 分开是为了钉住「`OUTBOUND→IN_STOCK` 不是参数错误，
/// 而是业务规则拒绝」：前端要提示「要重新入库请生成新序列号」，不是「改一下参数」。
pub const ERP_SERIAL_STATUS_INVALID: i32 = 66008;

/// 账不平（HTTP 500）：**总账 / 科目余额表的一致性闸**（P2-D1）。
///
/// 触发场景只有两种，都是「账本被写坏了」而不是「用户填错了」：
///
/// 1. `Σ本期借方 ≠ Σ本期贷方`（科目余额表）；
/// 2. 凭证详情的分录合计 ≠ 单头 `total_debit` / `total_credit`。
///
/// 为什么是 500 而不是 4xx：单张凭证有 `CHECK (total_debit = total_credit)`，
/// 正常路径不可能不平 —— 响了就是数据损坏（手工改库 / 红冲写坏分录），
/// 必须让监控抓到、让运维按消息里的「Σ借 / Σ贷 / 差额」定位，绝不能静默返回一张错的报表。
pub const ERP_LEDGER_UNBALANCED: i32 = 66009;

/// 会计期间不存在（400）：ERP-R1 资产负债表的编制前置条件。
///
/// 与余额表的「缺失则按自然月推导」刻意不同：报表是对外交付物，
/// 给一个没开账的月份出表，等于让人以为那个月真的没发生业务。消息里带开账指引。
pub const ERP_PERIOD_NOT_FOUND: i32 = 66010;

/// 报表配置错误（500）：`biz_report_item` 的公式引用了不存在的报表项、
/// 或整张报表一项都没配。
///
/// 为什么是 500 而不是 400：这是**配置**坏了，不是用户填错了 —— 让它响、
/// 让运维按消息里的 item_code 去修配置，绝不能静默出 0（"这个月没发生"和"报表配错"是两件事）。
pub const ERP_REPORT_CONFIG_INVALID: i32 = 66011;

/// 期初余额不平衡（400）：ERP-R5「期初借贷合计必须相等，不平衡不许保存、不许出表」。
///
/// 与 [`PARAM_INVALID`] 分开是为了让前端能**按码**把「差额多少、哪几行不对」渲染成
/// 明确提示（卡面 O5），而不是把整段中文扔进一个通用错误框。
pub const ERP_OPENING_UNBALANCED: i32 = 66012;

/// 违反职责分离（HTTP 409）：ERP 内控开关（`erp.separation_of_duties.*`）打开时，
/// 单据制单人不能自己审批 / 过账 / 付款。
///
/// 为什么是 409 而不是 403：403 是「你这个账号没有这个权限」，而这里账号**有**权限，
/// 只是这一次「人不对」（换个人立刻就能做）—— 前端要按「换人 / 关开关」提示，
/// 而不是「去找管理员要权限」。文案里同时给出开关键名与放行办法（O5）。
pub const ERP_SEPARATION_OF_DUTIES: i32 = 66013;

// ---------- 合同域（A1 合同表基座；ARCH-2 §3.2 的 8 个场景） ----------
//
// ⚠️ 号段说明：ARCH-2 §3.2 与 F14 写的是「66013–66020」（当时实测 66013+ 为空），
// 但 66013 已被 ERP-B2 的 [`ERP_SEPARATION_OF_DUTIES`] 占用（职责分离内控闸）。
// 按「只加常量、不改既有码」的红线，合同 8 个码**顺延到 66014–66021**，
// 语义与文档 8 条逐字对应（文档名 → 本文件常量名映射如下）。
//
// ```text
// 文档名                       本文件常量                         HTTP   场景（ARCH-2 §3.2 / §5.1）
// CONTRACT_NOT_USABLE      → ERP_CONTRACT_NOT_USABLE           409   V-22 合同不可用于建单（已删/状态不对/过期）
// CONTRACT_STATUS_ILLEGAL  → ERP_CONTRACT_STATUS_ILLEGAL       409   V-17 状态机拒绝该流转（含自环）
// CONTRACT_FIELD_LOCKED    → ERP_CONTRACT_FIELD_LOCKED         409   V-19 已签订，乙方/金额/期限不可改
// CONTRACT_NO_DUPLICATED   → ERP_CONTRACT_NO_DUPLICATED        409   V-20 合同号重复（23505 转译）
// CONTRACT_IN_USE          → ERP_CONTRACT_IN_USE               409   V-21 已被未关闭采购单引用，不可删
// CONTRACT_AMOUNT_EXCEEDED → ERP_CONTRACT_AMOUNT_EXCEEDED      409   V-23 累计下单额超出合同金额
// CONTRACT_CUSTOMER_UNSUPPORTED → ERP_CONTRACT_CUSTOMER_UNSUPPORTED 400 V-12/§3.2.3 客户主数据未上线
// CONTRACT_PARTY_INVALID   → ERP_CONTRACT_PARTY_INVALID        400   V-11/V-15 乙方不存在或已停用
// ```
//
// 实现卡（A1）正文按「中文场景」列了 8 条（合同不存在 / 状态不允许 / 金额非法 / 日期区间非法 /
// 乙方不存在 / 已被采购单引用不可删 / 合同号重复 / 类型不匹配），与文档 8 条**并非同一组**。
// 以文档为准（卡面原话「按文档的 8 个场景命名」），差异与去向：
//
//   · 「合同不存在」→ [`ERP_CONTRACT_NOT_USABLE`]（同一闸口，文案不同：不存在/已删除/状态不对）
//   · 「金额非法」「日期区间非法」「类型不匹配」→ 纯入参校验，走 [`PARAM_INVALID`]（10001）：
//     这三类在 ARCH-2 §3.1 是 `CHECK` 兜底的**最后一道网**（V-04/V-05/V-03），
//     正常路径必须被 A2 的 handler 先挡下并给 400 明确话术，不新增业务码。

/// 合同不可用于建单（HTTP 409）：ARCH-2 §5.1 的 V-22 五步判定 —— 合同已软删 /
/// 状态不是 `SIGNED`、`ACTIVE` / `order_date` 不在 `[start_date, end_date]`。
///
/// 为什么是 409 而不是 400：请求本身合法，是**合同这个业务对象当前不允许**新承诺
/// （终止/过期/未生效），前端要提示「换合同 / 先续签」而不是「改参数」。
pub const ERP_CONTRACT_NOT_USABLE: i32 = 66014;

/// 合同状态不允许该流转（HTTP 409）：ARCH-2 §4 的边表判定（含自环）。
///
/// 与框架的通用 409 分开，是为了让前端能定位到「合同状态机」并给出
/// 「允许的去向是 …」的提示（`state_machine::rejection_message` 已经这么措辞）。
pub const ERP_CONTRACT_STATUS_ILLEGAL: i32 = 66015;

/// 合同已签订，关键字段不可修改（HTTP 409）：ARCH-2 §5.3 的冻结矩阵。
///
/// `DRAFT` 之外 `party_b_* / contract_type / amount / currency_code / 三个日期` 冻结，
/// 改则拒绝；`remark / owner_id / dept_id / contract_name` 仍可改（终止原因就写 `remark`）。
pub const ERP_CONTRACT_FIELD_LOCKED: i32 = 66016;

/// 合同号重复（HTTP 409）：ARCH-2 V-20，把唯一索引 `uk_biz_contract_no` 的
/// SQLSTATE `23505` 转成业务码，**不把 SQLSTATE 透给前端**。
///
/// 注意唯一性只对 `deleted_at IS NULL` 的行生效（§2.3）：软删后可复用同号，
/// 所以本码只会在「活跃行重号」时出现。
pub const ERP_CONTRACT_NO_DUPLICATED: i32 = 66017;

/// 合同已被下游单据引用，不能删除（HTTP 409）：ARCH-2 V-21。
///
/// 判据：存在 `biz_purchase_order.contract_id = ?` 且
/// `status NOT IN ('CANCELLED','CLOSED')` 且未软删的行。文案带未关闭张数。
pub const ERP_CONTRACT_IN_USE: i32 = 66018;

/// 累计下单金额将超出合同金额（HTTP 409）：ARCH-2 V-23。
///
/// `Σ PO.total_with_tax(未取消) + 本单 > contract.amount`。
/// ⚠️ 并发坑：先查后写不是原子的，必须在**同一事务**里先
/// `SELECT id FROM biz_contract WHERE id=? FOR UPDATE`，否则两个并发请求会双双放行。
///
/// 销售侧（SO-27）复用同一个码：`Σ SO.total_with_tax(未作废) + 本单 > contract.amount`。
/// 语义、HTTP 状态、前端文案（「合同额度不足」）完全一致，
/// 前端 `error-code.ts` 一条映射同时覆盖采购与销售两处。
pub const ERP_CONTRACT_AMOUNT_EXCEEDED: i32 = 66019;

/// 客户类合同暂未开放（HTTP 400）：ARCH-2 §3.2.3 的**临时降级**。
///
/// `party_b_type = 'CUSTOMER'` 一律硬拒，不查任何表 —— 客户主数据（`biz_customer`）
/// 属批次 2。表结构（枚举 / 配对 CHECK / 索引）已按两分支建好，批次 2 只删这一行闸口。
pub const ERP_CONTRACT_CUSTOMER_UNSUPPORTED: i32 = 66020;

/// 乙方不存在或已停用（HTTP 400）：ARCH-2 V-11 / V-15。
///
/// 乙方没有外键可依赖（DEC-02），存在性靠应用层写入校验 + 每日巡检兜底；
/// 新建时额外要求供应商 `status = 1`（停用供应商不得新签），修改其它字段不重验。
pub const ERP_CONTRACT_PARTY_INVALID: i32 = 66021;

// ---------- A3 采购单 ↔ 合同勾稽（采购单侧的业务码，接在合同域 66021 之后） ----------
//
// 为什么采购单侧要有自己的码，而不是全走 `PARAM_INVALID`(10001)：
//   `purchase_type` 与 `contract_id` 的四种组合错误，前端要给出**不同的**引导
//   （去选合同 / 去掉合同 / 换合同 / 这单不能改类型），文案映射在
//   `frontend/src/api/error-code.ts`，混成一个码就只能靠匹配中文串。
//   码值只增不改（本段自检见下方 `erp_error_codes_are_stable`）。

/// 已建单不允许改 `purchase_type`（HTTP 400）：CONTRACT→SPOT 会**洗掉**合同关联，
/// 让「这张单挂过哪张合同」永久不可考。真要改必须先取消关联（本批次不提供）。
pub const ERP_PO_TYPE_LOCKED: i32 = 66022;

/// `purchase_type = CONTRACT` 却没给 `contract_id`（HTTP 400）。
///
/// DB 侧同样拦（`ck_biz_po_contract_pair`），但那是 500 形态的 `CheckViolation`；
/// handler 必须先在应用层挡下，给出「去选一张采购合同」的可执行文案。
pub const ERP_PO_CONTRACT_REQUIRED: i32 = 66023;

/// `purchase_type = SPOT` 却给了 `contract_id`（HTTP 400）。
///
/// 不静默忽略：请求自相矛盾时静默丢弃字段，会让调用方以为合同挂上了（审计灾难）。
pub const ERP_PO_CONTRACT_UNEXPECTED: i32 = 66024;

/// 客户端传的 `supplier_id` 与合同乙方不一致（HTTP 400）：设计 §6.2 的「唯一取值点」——
/// CONTRACT 单的供应商**从合同 `party_b_id` 派生**，手填值只允许与之一致（幂等），
/// 不一致必须报错，不能悄悄按合同方覆盖。
pub const ERP_PO_SUPPLIER_MISMATCH: i32 = 66025;

/// `purchase_type` 取值不在 `CONTRACT` / `SPOT` 里（HTTP 400）。
///
/// 与 `PARAM_INVALID` 分开的理由：契约类型是**业务枚举**，前端要提示「合同采购 / 零星采购」
/// 两个可选项，而不是「参数非法」这种没有出路的文案。
pub const ERP_PO_TYPE_INVALID: i32 = 66026;

// ---------- A4 入库单 ↔ 采购单勾稽（入库单侧的业务码，接在采购单侧 66026 之后） ----------
//
// 为什么入库单侧也要独立码：前端对「这单必须绑采购单」「供应商填错了」「别乱标无源」
// 三种情形的引导完全不同（去选采购单 / 清空供应商留空带出 / 取消无源标记），
// 混成 `PARAM_INVALID` 只能靠匹配中文串。码值只增不改（自检见 `erp_error_codes_are_stable`）。

/// 采购入库却没绑来源采购单，且没有显式声明「无源单据」（HTTP 400）。
///
/// AP-4 / AP-5：入库绑的是**采购单**，不是供应商；无源（期初 / 复制 / 其他）是 E4 例外，
/// 必须显式 `is_sourceless=true` 并写明原因，否则「无源」既不可枚举也进不了巡检。
pub const ERP_SI_SOURCE_REQUIRED: i32 = 66027;

/// 客户端传的 `supplier_id` 与来源采购单的供应商不一致（HTTP 400）。
///
/// 设计 §6.2 的「唯一取值点」：入库单供应商**从采购单派生**，手填值只允许与之一致（幂等），
/// 不一致必须报错，不能悄悄按采购单覆盖（那会让「带错单」无声无息）。
pub const ERP_SI_SUPPLIER_MISMATCH: i32 = 66028;

/// 无源标记与来源单自相矛盾 / 无源单没写原因（HTTP 400）。
///
/// 三种情形共用：① 有来源单却 `is_sourceless=true`；② 无源单没填 `remark` 说明来源；
/// ③ 想在有源单上改供应商 / 来源单（那一步走 409 `RESOURCE_CONFLICT`，不在这里）。
pub const ERP_SI_SOURCELESS_CONFLICT: i32 = 66029;

/// 单据明细用的单位不是**基本单位**（HTTP 400）。
///
/// ARCH-3 DC-09 / R-P0-4 的止血闸：`biz_unit` 支持配折算单位（`is_base = 0` + `rate`），
/// 但单据链路与库存三本账都只用 `qty`，不乘 `rate` —— 按箱下单会静默把 5 箱记成 5 个。
/// 本版本不支持多单位换算，单据明细的单位必须是基本单位，否则拒单并给出折算建议。
///
/// 单独给码而不是复用 [`ERP_FILTER_INVALID`]：前端要把这句话翻成「按基本单位录入」的引导
/// （并高亮物料行），与「筛选参数不合法」是两件事。
pub const ERP_UNIT_NOT_BASE: i32 = 66030;

// ---------- DC-03 收货过账复核来源采购单（过账期闸，接在 66030 之后） ----------
//
// 为什么过账期要独立码：建单期同一判据走的是 400（入参错：选了不可收的采购单），
// 过账期是 409（冲突：建单时可收、过账时被取消/关闭/收满了）。前端对两类要分别引导
// （400 → 改单换采购单；409 → 刷新看采购单现状 / 作废重开），混成一个码只能靠匹配中文。

/// 过账时来源采购单已不可收（被取消 / 关闭 / 已收满）（HTTP 409）。
///
/// ARCH-3 DC-03 / R-P0-3：建单期校验只保证「建单那一刻可收」，采购单在
/// 「建单 → 过账」之间被取消，过账仍会把三本账 + 应付 + 进项凭证写出去。
pub const ERP_SI_PO_NOT_RECEIVABLE: i32 = 66031;

/// 本次过账数量超过来源采购单的可用待收量（HTTP 409）。
///
/// 防「两张收货单双开」：两张单在建单时都按「未收额度」通过校验（那时还没有已过账收货），
/// 先后过账就会重复入库。待收口径 = 订单量 − 已过账收货量（`POSTED_RECEIVED_SQL`）。
pub const ERP_SI_OVER_RECEIVE: i32 = 66032;

// ---------- DC-01 审批流未配置改为默认拒绝（接在 66032 之后） ----------

/// 该单据类型**没有已发布的审批流程**，本次动作被拒绝（HTTP 409）。
///
/// ARCH-3 §5 DC-01 / §6 R-P0-2：改造前 6 处入口（采购提交 / 收货过账 / 收货作废 /
/// 应付付款 / 应付作废 / 凭证红冲）在「没配流程」时**静默降级为直接生效**，
/// 只留一条 `tracing::warn!` —— 内控形同虚设，且用户完全不知道这次没走审批。
///
/// 现在改为**默认拒绝**（fail-closed）：报文点名缺哪个 `flow_code`，并给出两条合法出路
/// —— ① 让管理员发布该流程；② 给该角色授予 `business:erp:skip-approval` 后显式跳过
/// （必填理由 ≥10 字，写进 `biz_status_log`）。
///
/// 为什么单独给码而不是复用 [`RESOURCE_CONFLICT`]：前端要能区分「单据状态冲突」（改单据）
/// 与「系统没配流程」（找管理员 / 显式跳过），并且要在报文里稳定拿到 `flow_code`。
pub const APPROVAL_FLOW_NOT_CONFIGURED: i32 = 66033;

/// 收款退款单**没有已通过的审批实例**，本次过账被拒绝（HTTP 409）。
///
/// SO-14 遗留卡 `t_573bfe5b` 的 D10 补丁（卡 `t_b9840c8d`）：退款是真出钱
/// （借 1122 应收 / 贷 1002 银行存款），与应付付款同级别的资金动作，所以
/// `CONFIRMED → POSTED` 之前必须有 `biz_type = 'RECEIPT_REFUND'`、`status = 'APPROVED'`
/// 的审批实例。审批**不是一个状态**（退款单仍是四值状态机），它是过账的前置条件。
///
/// 为什么单独给码而不是复用 [`RESOURCE_CONFLICT`]：前端要能按码把「去发起审批」的按钮
/// 直接渲染出来（这是可恢复的、缺一步操作），与「单据状态冲突」（要改单据）不是一类问题。
/// 报文里带退款单号与提交入口路径，照着点就能走完。
///
/// 合法出路两条：① 调 `POST /business/erp/receipt-refund/{id}/submit-approval` 发起审批
/// （审批通过后回调**自动过账**，不需要再手工点一次）；② 没配流程时由持
/// `business:erp:skip-approval` 的账号在提交时写明理由（≥10 字）显式跳过。
pub const ERP_RECEIPT_REFUND_APPROVAL_REQUIRED: i32 = 66044;

// ---------- DC-13 出库单（库存账只增不减的硬缺口，接在 66033 之后） ----------

/// 出库数量超过账面可用量，且该仓库不允许负库存（HTTP 409）。
///
/// ARCH-3 §5 DC-13 / §6 R-P0-1：出库单是本卡新增的单据类型，
/// 判据是「(物料, 仓库, 库位桶) 的账面数量 < 本次出库数量」且
/// `biz_warehouse.allow_negative = 0`。报文必须点名**物料编码/名称与可用量**
/// （包括该仓合计可用量，便于用户判断「是不是没指定库位」）。
///
/// 为什么是 409 而不是 400：入参本身合法（数量 > 0、物料存在），
/// 冲突的是「账上现在没有这么多货」这个现状 —— 建单时可能够、过账时被别人领走了。
pub const ERP_STOCK_OUT_NEGATIVE: i32 = 66034;

/// 作废出库单时账面已被别的业务取走，冲不回来（HTTP 409）。
///
/// 出库单作废要把当时扣掉的 qty / amount 原路加回。账面金额是加得回来的（加法永远成功），
/// 但**数量**若被后续业务（又一次出库 / 调拨）消耗掉，加回后仍是正的 —— 所以这条码
/// 实际拦的是「过账流水缺失 / 单据金额快照为 0」这类数据不自洽的作废：
/// 没有流水就不知道当时扣了多少，**绝不按当前均价估算**（那是污染成本的捷径）。
pub const ERP_STOCK_OUT_CANCEL_BLOCKED: i32 = 66035;

// ---------- P2-2 摆设字段处置（DC-12：信用额度开始生效） ----------

/// 采购单超供应商信用额度，且**没有已发布的 `po_approve` 流程**可以承接（HTTP 409）。
///
/// 处置策略是 B（超限不拒绝，但强制审批）。强制审批遇到「流程没配」时就只能拒绝 ——
/// 不能让 `business:erp:skip-approval` 把超限单悄悄放过去（那等于信用额度没生效）。
/// 报文必须给出两条出路：① 让管理员发布 `po_approve`；② 调高该供应商额度。
///
/// 为什么不复用 [`ERP_CONTRACT_AMOUNT_EXCEEDED`]（66019）：合同额度是「直接拒绝」，
/// 信用额度是「必须审批」——前端要能分别提示（前者改数量，后者找审批人/配流程）。
pub const ERP_CREDIT_LIMIT_EXCEEDED: i32 = 66036;

// ---------- FIN-1 预算管理（ARCH-6 第三章）----------

/// 预算校验未通过且控制策略为 **BLOCK**（HTTP 409）。
///
/// 出厂默认是 `WARN`（标记 + 强制审批），只有客户把
/// `erp.budget.control_mode` 或某条预算的 `control_mode` 显式设成 `BLOCK`
/// 才会出现这个码（ARCH-6 D3）。报文必须给出「本次金额 / 年可用 / 月可用 / 超支额」
/// 六个数，并说明两条出路：调高预算（走调整审批）或改成 WARN。
///
/// 为什么单独给码：前端要能区分「预算不足」（去找财务调额度）与
/// 「单据状态冲突」（刷新重试）—— 两者的处置动作完全不同。
pub const ERP_BUDGET_EXCEEDED: i32 = 66037;

/// 折旧的目标会计期间已结账 / 未开账，**不许补提**（HTTP 409，ARCH-6 D7 §5.5.1）。
///
/// 为什么单独给码而不是沿用 400：这不是「参数写错了」，是「这个月已经封账」——
/// 前端要能一眼提示财务「先去反结账，再重跑折旧」，而不是让用户去改日期。
/// 补提（跨多期一次提完）本版本**不支持**：它会让已上报的月度报表与总账不一致。
pub const ERP_ASSET_PERIOD_CLOSED: i32 = 66038;

/// 折旧批次的状态不允许本次动作（HTTP 409，ARCH-6 §5.5.5）。
///
/// 典型场景：对 `POSTED` 的批次再点一次「计提」（幂等返回不算错，重复**红冲**才是）、
/// 对 `CALCULATED` 的批次点红冲、对 `REVERSED` 的批次点过账。
/// 与 [`ERP_ASSET_PERIOD_CLOSED`] 分开：后者是期间的问题，这个是批次自己的状态问题。
pub const ERP_DEPRECIATION_STATE: i32 = 66039;

/// 资产已有在跑的处置审批，或资产不处于可处置状态（HTTP 409，ARCH-6 §5.6.3）。
///
/// 处置必须走审批（`asset_disposal`）；同一张卡同时只能有一个在跑的处置单 ——
/// 否则两次「同意」会生成两套处置凭证，1606 直接变成负数。
pub const ERP_ASSET_DISPOSAL_STATE: i32 = 66040;

// ---------- FIN-3 费用报销（ARCH-6 第四章，接在 66040 之后） ----------

/// 报销单的当前状态不允许本次动作（HTTP 409，ARCH-6 §4.3.3）。
///
/// 典型场景：非 `DRAFT` 想改明细 / 提交、`PAID` 后想作废（钱已付给员工，
/// 退回是 v2 的「员工退款」业务）、同一张单已有在跑的审批（`uk_biz_flow_instance_biz_active`）。
/// 与 [`ERP_EXPENSE_TOTAL_MISMATCH`] 分开：那个是**金额对不上**（数据问题），
/// 这个是**状态不允许**（时序问题），前端提示的话术完全不同。
pub const ERP_EXPENSE_STATE: i32 = 66041;

/// 明细合计与单头总金额对不上（HTTP 422 / 400，ARCH-6 本章红线 2）。
///
/// 这是 ERP 的**大忌**：单头 `total_with_tax` 与 `Σ 明细 with_tax_amount` 一旦漂移，
/// 凭证、预算占用、付款金额三处会各自按不同的数算账，事后无法判断哪个是对的。
/// 库侧三道 CHECK 保证「单头内部自洽」「明细内部自洽」，**跨表恒等只能由写入方保证**
/// —— 所以本码专门给「提交 / 改单时复算对不上」用，报文必须点名两个数。
pub const ERP_EXPENSE_TOTAL_MISMATCH: i32 = 66042;

/// 报销入账的目标会计期间已结账 / 未开账（HTTP 409，ARCH-6 §4.5.4）。
///
/// 与 [`ERP_ASSET_PERIOD_CLOSED`] 同因（期间闸），但**语义不同**：报销的期间是按
/// 「审批通过日」算的，10 月的发票 11 月批就记 11 月账；若 11 月已封账，
/// 报文要指向「让财务反结账或延后审批」，绝不自动挑下一个期间（那等于篡改会计归属）。
pub const ERP_EXPENSE_PERIOD_CLOSED: i32 = 66043;

// ---------- SO-27 销售订单 ↔ 销售合同（接在 66043 之后） ----------

/// 订单客户与合同乙方不是同一方（HTTP 400）。
///
/// 为什么单独给码而不复用 `ERP_PO_SUPPLIER_MISMATCH`（66025）：那个码的文案与
/// 前端引导都是「采购单的供应商」，销售侧要引导「改选属于该客户的合同」。
///
/// 不静默按合同乙方覆盖客户：那是替用户改单头，审计上等于伪造业务事实；
/// 也不放过（只校验额度）：把 A 客户的订单金额记到 B 客户的合同额度里，
/// 两边都会错 —— 一张合同超卖、一张永远用不完。
pub const ERP_SO_CONTRACT_CUSTOMER_MISMATCH: i32 = 66045;

// ---------- FIN-ASSET-ST02 停用 / 启用（接在 66045 之后） ----------

/// 资产的当前状态不允许本次「停用 / 启用」（HTTP 409，FIN-ASSET-ST02 D6）。
///
/// 三种场景共用本码（都属「状态不适合这个动作」，前端提示同一个「怎么办」）：
/// ① 目标态 == 当前态（重复停用 / 重复启用 —— 不是状态变更，也不会重复通知）；
/// ② 卡片不在「在役 / 闲置」（已处置 / 已作废，要先作废处置）；
/// ③ CAS 回写 0 行（锁后状态仍被改，说明有别的写入路径，必须报错不能静默）。
///
/// **不复用** [`ERP_ASSET_DISPOSAL_STATE`]（66040）：前端 `ASSET_ERROR_HINTS[66040]`
/// 固定映射成「该资产已有在跑的处置审批：请先在流程中心处理完，或作废该审批」，
/// 用在「已停用」场景会把排障直接指到错的方向。错误码不是权限码，加号不需要迁移。
pub const ERP_ASSET_STATUS_ILLEGAL: i32 = 66046;

/// 处置前置闸：**处置当月的折旧还没计提 / 没进 POSTED**（HTTP 409，FIX-3 方案 ①）。
///
/// 铁律②「当月减少当月照提」的落点：`scan_assets_for_period` 只扫
/// `status IN ('IN_USE','IDLE')`，卡片一旦 `DISPOSED` 就被整类排除 ⇒ 月中处置的资产
/// 当月一个月折旧提不到，而处置凭证已按处置时点的累计折旧落账（三处永久差一个月）。
/// 所以处置前置校验「处置当月的折旧已过账」，不满足就 409 + 本码，报文点明
/// 「请先在折旧计提里计提并过账处置当月的折旧，再提交处置」。
///
/// 为什么单独给码：前端要能把用户直接送到「折旧计提」页，而不是让人以为
/// 「资产状态不对」（66040 / 66046）或「期间已结账」（66038）—— 三者处置动作完全不同。
pub const ERP_ASSET_DEPRECIATION_PENDING: i32 = 66048;

// ---------- P2-01 询比价定标闸（接在 66046 之后） ----------

/// 定标被比价规则挡住（HTTP 409）：
/// ① 某条明细只有 1 家报价（或尚未报价），且没有给单源例外理由；
/// ② 请求里的中标组合与已录报价对不上（该供应商没给这条明细报价）；
/// ③ 询价单状态不允许定标（已定标 / 已转单）。
///
/// **不复用** `RESOURCE_CONFLICT`（通用 409）：前端要能一眼区分「数据状态冲突」
/// 与「比价规则拒绝」，并直接给出「补报价 / 填单源理由」的下一步动作。
/// 错误码不是权限码，加号不需要迁移。
pub const ERP_PRICE_INQUIRY_AWARD_GUARD: i32 = 66047;

// ---------- P2-02 采购申请（需求申请 → 审批 → 转采购订单） ----------

/// 采购申请的状态不允许本次动作（HTTP 409）：
/// ① 提交审批时不是 DRAFT（已在审批中 / 已通过 / 已转单）；
/// ② 转单时不是 APPROVED，或已经转过（`po_id` 非空）；
/// ③ 状态 CAS 回写 0 行（锁后状态仍被改，说明有别的写入路径）。
///
/// **不复用** `RESOURCE_CONFLICT`（通用 409）：前端要能一眼看出「这张申请单的
/// 状态不适合这个动作」，并直接把用户引到「看审批中心 / 看已转的 PO」。
/// 错误码不是权限码，加号不需要迁移。
pub const ERP_REQUISITION_STATUS_GUARD: i32 = 66049;

/// 合并转采购订单被合并规则挡住（HTTP 409）：
/// ① 同物料在多张申请单上的税率不一致；
/// ② 各申请单的建议供应商不一致（合并单只能是同一个供应商）；
/// ③ 一张单都没填建议供应商（必须显式指定）；
/// ④ 各申请单的收货仓库不一致（必须显式指定）。
///
/// **不复用** [`ERP_REQUISITION_STATUS_GUARD`]：单据状态是对的，是**合并口径**谈不拢，
/// 用户要做的动作是「回申请单改税率 / 补供应商 / 指定仓库」，与状态问题完全不同。
pub const ERP_REQUISITION_MERGE_MISMATCH: i32 = 66050;

/// 采购退货：可退数量不足（P2-03，`66051`）。
///
/// 语义 = 「本次退货数量 > 原入库数量 − 已退数量」（含已提交但尚未确认的占用）。
/// 与 [`ERP_SI_OVER_RECEIVE`]（超收）方向相反、单据不同，**不复用**：
/// 前端给的处置动作不同（改数量 / 换入库单），报文里要指出可退多少。
pub const ERP_PURCHASE_RETURN_OVER_RETURN: i32 = 66051;

/// 采购退货：状态闸（P2-03，`66052`）。
///
/// 只有 `DRAFT` 能提交、只有 `APPROVED` 能确认；重复提交 / 越级确认 / 已确认再确认
/// 都走这个码（报文里带当前状态与允许的动作）。
pub const ERP_PURCHASE_RETURN_STATUS_GUARD: i32 = 66052;

/// 采购退货：退货单价与来源入库单价不一致（P2-03，`66053`）。
///
/// 卡面 §④ b「退货单价 = 原入库单价（不许改）」的执行出口：前端传了 `unit_price`
/// 且与入库明细快照不相等时拒收（防价格操纵）。服务端本来就不吃这个值，
/// 这个码是给「以为能改价」的调用方一个明确的 400，而不是静默忽略。
pub const ERP_PURCHASE_RETURN_PRICE_LOCKED: i32 = 66053;

/// 主数据编码禁止手工指定（HTTP 400，CODE-08 · `66054`）。
///
/// 系统参数 `erp.master_data.allow_manual_code` 被关闭后，6 个主数据（供应商 /
/// 客户 / 物料 / 仓库 / 资产分类 / 费用类型）的建单接口收到**非空**手工编码一律
/// 拒收：关闭这个开关的企业要求编码完全由系统按号段规则生成。
///
/// 为什么不复用 [`PARAM_INVALID`]（`10001`）：前端要能区分「编码格式写错了」（让用户
/// 改编码框）与「这个环境根本不许填」（把编码框整个置灰并提示去系统参数开开关），
/// 两种情况的引导完全不同。留空仍然合法（= 自动取号），不受本码影响。
pub const ERP_MANUAL_CODE_FORBIDDEN: i32 = 66054;

/// 成本对象关批被前置校验拦下（HTTP 409，成本核算 ARCH-9 §4.1 · `66055`）。
///
/// 与 `RESOURCE_CONFLICT`（10003）分开的理由：前端要按这条码把「5 条前置」逐条
/// 列给用户（哪张领料单没归集、哪条工时还是草稿、哪个期间已锁定），而不是
/// 一句「冲突了」。报文里已经带了逐条说明，前端按 `msg` 展示即可。
///
/// 5 条前置见 `business::erp::cost_object_dto::evaluate_close`（纯函数，单测钉住）。
pub const ERP_COST_OBJECT_CLOSE_BLOCKED: i32 = 66055;

/// 批次主数据批号重复（HTTP 409，成本核算 ARCH-9 §3.1 ① · `66056`）。
///
/// 触发源是数据库唯一索引 `uk_biz_lot_material_no (material_id, lot_no)
/// WHERE deleted_at IS NULL`：同一物料下不许有两个相同批号。用户看到的必须是
/// 「物料 X 的批号『Y』已存在」，而不是 PG 的
/// `duplicate key value violates unique constraint "uk_biz_lot_material_no"`。
///
/// 与 [`ERP_CODE_CONFLICT`]（`66001`，主数据**编码**重复）分开的理由：批号是
/// 物料级业务键（同一批号可以存在于不同物料），前端要提示「换个批号 / 去批次管理
/// 里编辑那一行」，引导与「编码已存在」不同。
pub const ERP_LOT_NO_DUPLICATE: i32 = 66056;

/// 领料单挂的成本对象不是 `OPEN`（HTTP 409，成本核算 ARCH-9 §4.7.1 / B1-10 · `66057`）。
///
/// 两种触发：
/// ① 建单 / 改单时 `cost_object_id` 指向的对象已 `CLOSED` / `CANCELLED`
///    （`stockout_dto::ensure_cost_object_usable`，纯逻辑判定）；
/// ② 过账时复算借方科目前的对象已不是 `OPEN`（草稿放在那里、期间被人关批 ——
///    `cost_object_handler::close` 的前置 ④ 会拦「还有未过账领料」，这里是并发兜底）。
///
/// **不复用** [`ERP_COST_OBJECT_CLOSE_BLOCKED`]（`66055`）：那个码的语义是
/// 「关批被拦，逐条列出 5 条前置」，用户动作是「去补归集 / 去确认工时」；
/// 本码的动作是「换一个还在制的对象 / 重新开一批」，前端按码给不同引导。
pub const ERP_COST_OBJECT_NOT_OPEN: i32 = 66057;

/// 成本核算 · 工时台账「工时 × 工资率」与金额不符（`66058`，ARCH-9 §3.1 ④）。
///
/// 触发点两处，文案与码一致：
/// 1. 入口核对 —— 客户端传了 `amount` 但 ≠ `round(hours × rate, 4)`；
/// 2. 库层兜底 —— `ck_biz_cost_labor_amount` 抛 23514（并发 / 手工 SQL / 导入）。
///
/// **不复用** [`error_code`] 的 `PARAM_INVALID`（`10001`）：那个码是「参数格式不对」，
/// 用户动作是「改格式」；本码的动作是「改工时或工资率，别手填金额」，前端
/// 需要给出「差额是多少、期望值是多少」的引导（报文里已经算好了）。
pub const ERP_COST_LABOR_AMOUNT_MISMATCH: i32 = 66058;

/// 成本核算 · 费率取数三级全空（HTTP 422，ARCH-9 §4.5.4 / B2-3 · `66059`）。
///
/// 触发点：工时录入（单条 / 导入）的「工资率」留空时，服务端按
/// `COST_OBJECT`（该批专用）→ `DEPT`（车间综合）→ `COMPANY`（公司级）三级查
/// `biz_cost_rate`（只认 `status=1`、未删除、`effective_from <= 工作日期 <= effective_to`），
/// **三级都没命中**。
///
/// 为什么不复用 [`PARAM_INVALID`]（`10001`）/ [`ERP_COST_LABOR_AMOUNT_MISMATCH`]（`66058`）：
/// - `10001` 的动作是「改请求格式」，而这里的请求完全合法 —— 缺的是**配置数据**；
/// - `66058` 的动作是「改工时或费率」，这里改多少次都没用。
///
/// 用户动作是「去登记一条覆盖该日期的费率（车间级优先，公司级兜底）」，前端要能
/// 直接告诉用户「缺哪一级、缺哪一天」。**绝不**按 0 计费率（设计 D-7：编造的
/// 人工成本比算不出来更危险）。
pub const ERP_COST_RATE_MISSING: i32 = 66059;

/// 成本核算 · 手工费用台账「借方科目不合法」（HTTP 400，ARCH-9 §3.1 ⑤ / B2-4 · `66060`）。
///
/// 触发点（同一条码，报文点名是哪个字段、哪个原因）：
/// 1. `debit_account_code` 不在 `4101.*`（制造费用科目）—— 卡面验收项「科目不在 4101.* → 拒绝」；
/// 2. 科目在 `biz_account` 里不存在 / 已软删（拼错编码）；
/// 3. 科目不是末级（`is_leaf = 0`）—— 凭证闸只允许末级，非末级到结算时才炸；
/// 4. 科目已停用（`status = 0`）。
///
/// **不复用** [`PARAM_INVALID`]（`10001`）：`10001` 的动作是「改请求格式」，而这里的
/// 请求格式完全正确 —— 错的是**选的科目**。用户动作是「换一个 4101 下的启用末级科目」，
/// 前端要能一眼看出是科目选错了（而不是以为工时/金额填错了）。
///
/// 为什么一条码覆盖 4 种原因：四者的**动作完全相同**（换科目），分成 4 个码只会让前端
/// 多写 4 条提示、却给不出不同的下一步。原因写在报文里（点名编码 + 账套现状）。
pub const ERP_COST_OVERHEAD_ACCOUNT: i32 = 66060;

/// 成本核算 · 手工费用台账「金额必须大于 0 且最多 4 位小数」（HTTP 400，ARCH-9 §3.1 ⑤ / B2-4 · `66061`）。
///
/// 触发点：`amount <= 0`（含 0）、负数、超过 `NUMERIC(18,4)` 的标度 / 范围。
/// 库层 `ck_biz_cost_oh_amount CHECK (amount > 0)` 是同一口径的兜底，两边文案一致。
///
/// **不复用** [`PARAM_INVALID`]（`10001`）：卡面验收项是「金额必须 > 0」，前端要按码给
/// 「费用台账只记正数金额，负数/冲回走凭证红字」的引导，而不是笼统的「参数错误」。
pub const ERP_COST_OVERHEAD_AMOUNT: i32 = 66061;

/// 成本核算 · 分摊「池有金额但当期无在产成本对象」（HTTP 409，ARCH-9 §4.4.3 / B2-5 · `66062`）。
///
/// 设计 §4.4.3 明写：一条都不满足时**报错**，不做「挂在池里留到下期」的静默处理
/// （那正是上期漏摊、4101 余额永远清不掉的来源）。
///
/// **不复用** [`ERP_COST_ALLOC_BASIS_ZERO`]（`66063`）：两者的修法不同 ——
/// 66062 是「当期确实没生产 / 漏录领料工时」，66063 是「这个分配标准的数据源是空的
/// （例如按机器工时摊但没人填机器工时）」。前端给的下一条动作不一样。
pub const ERP_COST_ALLOC_NO_OBJECT: i32 = 66062;

/// 成本核算 · 分摊「该标准下基数合计为 0，无法按比例摊」（HTTP 409，ARCH-9 §4.4.3 / B2-5 · `66063`）。
///
/// 池 > 0 而 `basis_total = 0` 时守恒断言 `Σ alloc = pool` 不可能成立；
/// 按 0 摊会让界面显示「池 4000、摊 0」而结算时 4101 清不掉。报文点名标准与缺什么数据。
pub const ERP_COST_ALLOC_BASIS_ZERO: i32 = 66063;

/// 成本核算 · calc 料归集遇到**账面数据非法**的来源领料行（HTTP 400，ARCH-9 §4.3 / B2-6 · `66064`）。
///
/// 触发条件：已过账 + 挂了成本对象的领用单，其明细行数量 ≤ 0 / 金额 < 0 / 物料为空 /
/// 小数位超列标度。这些都说明账面上已经有异常行（正常过账路径插不进来）。
///
/// 为什么**不复用** [`PARAM_INVALID`]：用户看到的不是「你填错了参数」（他根本没填参数，
/// 是在跑计算），而是「某张单号的账面数据坏了，去修那张单」；前端要给的是单号跳转，
/// 不是表单红框。
pub const ERP_COST_COLLECT_SOURCE_INVALID: i32 = 66064;

/// 成本核算 · calc 第 4 段「费用池 vs `4101.*` 借方发生额」对不平（HTTP 409，ARCH-9 §4.11 C-COST-1 / B2-9 · `66065`）。
///
/// 触发条件：当期 `Σ biz_cost_collect(OVERHEAD, 池行)` 与当期 `4101.*` 借方发生额按
/// (车间, 科目) 分组后**任一组不等**（总额相等也不算平 —— A 车间多、B 车间少会互相抵掉）。
///
/// 报文必须列出「差额凭证号」：`biz_voucher_entry` 里那些贷/借 4101 的凭证是绕过成本模块
/// 直接记的制造费用，成本核算看不见它们，结算后 4101 清不掉（设计 §8.5 R-3）。
///
/// 为什么**不复用** [`ERP_COST_ALLOC_NO_OBJECT`]（`66062`）这类「成本侧数据缺失」的码：
/// 那是「成本模块自己的数据不全，去补录」；本码的用户动作是「把某几张凭证上的费用改走
/// 费用台账/报销，或把该笔改记别的科目」—— 处理对象是**凭证**，不是成本模块的台账。
pub const ERP_COST_POOL_LEDGER_MISMATCH: i32 = 66065;

/// 成本核算 · calc 第 5 段「该期间成本批次已提交/审批/结算，禁止重算分摊」
/// （HTTP 409，ARCH-9 §4.4.5 / B2-10 · `66066`）。
///
/// 触发条件：`biz_cost_period` 里该期间存在活跃批次，且状态 ∉
/// `DRAFT / CANCELLED / REVERSED`。分摊结果的唯一键是
/// `(period_code, rule_id, cost_object_id, account_code)`，重算 = 按期间整删重灌
/// （该表没有 `deleted_at`，设计 §3.1 ⑧）。数字已经提交给审批人看过或已经入账时重算，
/// 等于**改写历史成本**，所以必须在入口拦住。
///
/// 为什么**不复用** [`PARAM_INVALID`]（`10001`）/ [`RESOURCE_CONFLICT`]（`10009` 类）：
/// 用户动作是「先撤回批次 / 先红冲」再去重算，前端要能直接把按钮指向那张批次
/// （报文里带批次号 + 状态），不是笼统的「参数错误 / 冲突」。
pub const ERP_COST_ALLOC_PERIOD_LOCKED: i32 = 66066;

/// 成本核算 · calc 第 6 段「成本对象的本期产量 / 在制 / 进度数据不合法」
/// （HTTP 409，ARCH-9 §4.6 / B2-11 · `66067`）。
///
/// 触发条件（都在 `cost_period_item_dto::compose` 里逐条拦）：
/// 期末在制 > 0 但完工进度为 0 / 进度不在 0–100 / 累计完工小于前期已结算完工合计 /
/// 既无完工也无期末在制却挂着成本 / 投料方式不是一期唯一支持的 `ONE_TIME` /
/// 期初在制要素还原出现负数。
///
/// 为什么一条码覆盖 6 种原因：用户的**动作完全相同** —— 回成本对象页改数字后重算；
/// 具体原因写在报文里（点名对象号与数值），前端不需要 6 条不同引导。
pub const ERP_COST_OUTPUT_QTY_INVALID: i32 = 66067;

/// 成本核算 · calc 第 6 段「有完工数量但一笔材料都没归集」
/// （HTTP 409，ARCH-9 §4.10 / B2-11 · `66068`）。
///
/// 设计 §4.10 明写「零成本完工是数据错误，不是业务」：材料按开工一次投入计入成本，
/// 完工了必然投过料。期初在制里已经带了材料成本时不算（跨月完工既有在制的正常形态）。
///
/// **不复用** [`ERP_COST_OUTPUT_QTY_INVALID`]（`66067`）：那是「对象页的数字填错」，
/// 本码是「领料单没挂对象 / 没过账 / 归集漏了」—— 用户要去查的是**领料单**，不是对象页。
pub const ERP_COST_FINISHED_WITHOUT_MATERIAL: i32 = 66068;

/// 成本核算 · calc 第 6 段「上期批次已红冲，期初在制不能自动结转」
/// （HTTP 409，ARCH-9 §4.6.3 / B2-11 · `66069`）。
///
/// 设计 §4.6.3 原话：若上期批次被红冲 → 本期不得自动取数，报错「上期批次已红冲，
/// 请先重算上期」。红冲后的期末在制成本是不可信的历史，取数会把错数带进本期。
pub const ERP_COST_PRIOR_PERIOD_REVERSED: i32 = 66069;

/// 成本核算 · calc 第 6 段「该期间成本批次已提交/审批/结算，禁止重算计算单」
/// （HTTP 409，ARCH-9 §4.6 / B2-11 · `66070`）。
///
/// 计算单是结算入库金额与结转凭证的数字来源（`finished_cost` → 入库金额、
/// `unit_finished_cost` → 入库单价），重算等于改写已入账成本。
///
/// **不复用** [`ERP_COST_ALLOC_PERIOD_LOCKED`]（`66066`）：那个码的动作是「先去重算分摊」，
/// 本码是「整张计算单要重算」；报文里的修法（撤回 / 红冲）一致但提示的落点不同。
pub const ERP_COST_ITEM_PERIOD_LOCKED: i32 = 66070;

/// 成本核算 · calc 第 6 段「该期间还没有成本核算批次」
/// （HTTP 409，ARCH-9 §3.1 ⑩ / B2-11 · `66071`）。
///
/// `biz_cost_period_item.period_id` 是**外键**（V0373，`ON DELETE CASCADE`），
/// 没有批次行就无处安放计算单。修法是先建该期间的成本批次（成本核算页「选期间」）。
pub const ERP_COST_PERIOD_BATCH_MISSING: i32 = 66071;

/// 成本核算 · 同期间已有活跃批次（HTTP 409，ARCH-9 §4.1 / B3-1 · `66072`）。
///
/// `uk_biz_cost_period`（V0373）允许多条 `REVERSED` / `CANCELLED`（审计留痕不占位），
/// 但同一期间只许一条**活跃**批次。建新批次撞键时给本码：前端引导「先去红冲 / 作废
/// 那一批」，而不是把 PG 的 `duplicate key value violates unique constraint` 原样抛出。
pub const ERP_COST_PERIOD_DUPLICATE: i32 = 66072;

/// 成本核算 · 批次当前状态不允许该动作（HTTP 409，ARCH-9 §4.1 / B3-1 · `66073`）。
///
/// 只有 `DRAFT` 能改数 / 作废；`SUBMITTED` 起数据冻结（设计 §4.1「要改就撤回或作废重建」）。
/// **不复用** [`ERP_COST_ITEM_PERIOD_LOCKED`]（`66070`）：那个码锁的是**计算单重算**，
/// 本码锁的是**批次单头的写操作**（改人工来源 / 改备注 / 作废）。
pub const ERP_COST_PERIOD_STATUS_GUARD: i32 = 66073;

/// 成本核算 · 该批次的完工入库已经生成过（HTTP 409，ARCH-9 §4.8 / B3-4 · `66074`）。
///
/// 结算的完工入库是**一次性的账务动作**：同一批次重复执行会把数量 / 金额记两遍
/// （库存虚增、批次账重复）。两道闸共用本码：
///
/// * 批次级：`biz_stock_in` 里已有该 `cost_period_id` 的**非作废**单据；
/// * 计算单级：`UPDATE … WHERE status='CALC' AND stock_in_id IS NULL` 影响 0 行
///   （并发下另一个事务先结算了同一行）。
///
/// 修法两边一样：**先红冲**该批次（`SETTLED → REVERSED`，B3-5/B3-7），入库单会被置
/// `CANCELLED`、计算单回到 `CALC`，然后重新结算。`CANCELLED` 的单不算「已生成」，
/// 所以红冲之后本码不会再挡住重算。
pub const ERP_COST_FINISH_ALREADY_SETTLED: i32 = 66074;

// ==================== 67xxx：PDF 导出（PDF-01） ====================
//
// 为什么单独开 67xxx：66xxx 是 ERP 主数据的段（66001–66054 已排到 66054，前端
// `error-code.ts` 按段做文案映射）；PDF 导出不是 ERP 能力（发票 / 入库单 / 对账单
// 都会用），与主数据语义不重叠，独立成段便于后续 PDF-02..08 直接续号。

/// PDF 渲染服务不可用（HTTP 503，`67001`）。
///
/// 这是**运维信号**：本机常驻 Chromium（`chromium-pdf.service`，:9222）没起来、
/// 端口改了、或进程被 OOM 杀掉。前端文案应提示「导出服务暂不可用，请联系管理员」，
/// 而不是让用户反复重试。
pub const PDF_RENDER_UNAVAILABLE: i32 = 67001;

/// PDF 渲染超时（HTTP 504，`67002`）。
///
/// 单次渲染超过 `PDF_RENDER_TIMEOUT_MS`（默认 20s，实测 500 行仅 332ms，留了 60 倍
/// 余量）。真触发时通常是「一次导出几百张单」把并发队列打满，而不是单张太慢。
pub const PDF_RENDER_TIMEOUT: i32 = 67002;

/// PDF 模板渲染失败（HTTP 422，`67003`）。
///
/// 模板语法错、缺变量、或调用方给的上下文类型不对。与 [`INTERNAL_ERROR`] 分开的
/// 理由：这是**调用方/模板作者**要修的问题，不是系统故障。
pub const PDF_TEMPLATE_INVALID: i32 = 67003;

/// PDF 渲染参数非法（HTTP 400，`67004`）。
///
/// 纸张代码不认识、`CUSTOM` 缺宽高、缩放不在 0.1–2.0、边距吃掉整张纸。
/// CDP 对越界值**不报错**（静默出空白纸 / 1 页空 PDF），所以必须在这里挡。
pub const PDF_OPTIONS_INVALID: i32 = 67004;

/// PDF 请求体过大（HTTP 413，`67005`）。
///
/// [`crate::business::pdf::dto::MAX_RENDER_HTML_BYTES`]（4MiB）是硬上限：
/// 挡住「传一个巨型 HTML 让 chromium 吃满内存」的用法。
pub const PDF_RENDER_TOO_LARGE: i32 = 67005;

#[cfg(test)]
mod pdf_error_codes_are_stable {
    use super::*;

    /// PDF 段（67xxx）码值冻结：前端 `error-code.ts` 与运维告警按这些值映射。
    #[test]
    fn pdf_error_codes_are_stable() {
        let codes = [
            (PDF_RENDER_UNAVAILABLE, 67001),
            (PDF_RENDER_TIMEOUT, 67002),
            (PDF_TEMPLATE_INVALID, 67003),
            (PDF_OPTIONS_INVALID, 67004),
            (PDF_RENDER_TOO_LARGE, 67005),
        ];
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "67xxx 段不得有重复码值");
        assert!(
            codes.iter().all(|(code, _)| *code > 66054 && *code < 68000),
            "67xxx 段必须落在 66054 之后、68000 之前"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W1c + W2b + P2-D1 段自检：66xxx 段码齐全、不重复、不与 65xxx / 64xxx 撞车。
    ///
    /// 与 [`flow_error_codes_are_stable`] 同一口径：码值是前端文案映射的契约，
    /// 后续卡不许改值（要新语义就往下加号）。
    #[test]
    fn erp_error_codes_are_stable() {
        let codes = [
            (ERP_CODE_CONFLICT, 66001),
            (ERP_REFERENCE_MISSING, 66002),
            (ERP_WAREHOUSE_HAS_CHILDREN, 66003),
            (ERP_WAREHOUSE_LEVEL_LIMIT, 66004),
            (ERP_WAREHOUSE_NOT_LEAF, 66005),
            (ERP_FILTER_INVALID, 66006),
            (ERP_SERIAL_NOT_REQUIRED, 66007),
            (ERP_SERIAL_STATUS_INVALID, 66008),
            (ERP_LEDGER_UNBALANCED, 66009),
            (ERP_PERIOD_NOT_FOUND, 66010),
            (ERP_REPORT_CONFIG_INVALID, 66011),
            (ERP_OPENING_UNBALANCED, 66012),
            (ERP_SEPARATION_OF_DUTIES, 66013),
            // A1 合同域（ARCH-2 §3.2 的 8 个场景；66013 被 ERP-B2 占用后顺延到 66014–66021）
            (ERP_CONTRACT_NOT_USABLE, 66014),
            (ERP_CONTRACT_STATUS_ILLEGAL, 66015),
            (ERP_CONTRACT_FIELD_LOCKED, 66016),
            (ERP_CONTRACT_NO_DUPLICATED, 66017),
            (ERP_CONTRACT_IN_USE, 66018),
            (ERP_CONTRACT_AMOUNT_EXCEEDED, 66019),
            (ERP_CONTRACT_CUSTOMER_UNSUPPORTED, 66020),
            (ERP_CONTRACT_PARTY_INVALID, 66021),
            // A3 采购单 ↔ 合同勾稽（66022–66026）
            (ERP_PO_TYPE_LOCKED, 66022),
            (ERP_PO_CONTRACT_REQUIRED, 66023),
            (ERP_PO_CONTRACT_UNEXPECTED, 66024),
            (ERP_PO_SUPPLIER_MISMATCH, 66025),
            (ERP_PO_TYPE_INVALID, 66026),
            // A4 入库单 ↔ 采购单勾稽（66027–66029）
            (ERP_SI_SOURCE_REQUIRED, 66027),
            (ERP_SI_SUPPLIER_MISMATCH, 66028),
            (ERP_SI_SOURCELESS_CONFLICT, 66029),
            // 单据单位止血（ARCH-3 DC-09，66030）
            (ERP_UNIT_NOT_BASE, 66030),
            // 过账复核来源采购单（ARCH-3 DC-03，66031–66032）
            (ERP_SI_PO_NOT_RECEIVABLE, 66031),
            (ERP_SI_OVER_RECEIVE, 66032),
            // 审批流未配置改为默认拒绝（ARCH-3 DC-01，66033）
            (APPROVAL_FLOW_NOT_CONFIGURED, 66033),
            // 出库单（ARCH-3 DC-13，66034–66035）
            (ERP_STOCK_OUT_NEGATIVE, 66034),
            (ERP_STOCK_OUT_CANCEL_BLOCKED, 66035),
            (ERP_CREDIT_LIMIT_EXCEEDED, 66036),
            // FIN-1 预算（ARCH-6 第三章，66037）
            (ERP_BUDGET_EXCEEDED, 66037),
            // FIN-2 固定资产（ARCH-6 第五章，66038–66040）
            (ERP_ASSET_PERIOD_CLOSED, 66038),
            (ERP_DEPRECIATION_STATE, 66039),
            (ERP_ASSET_DISPOSAL_STATE, 66040),
            // FIN-3 费用报销（ARCH-6 第四章，66041–66043）
            (ERP_EXPENSE_STATE, 66041),
            (ERP_EXPENSE_TOTAL_MISMATCH, 66042),
            (ERP_EXPENSE_PERIOD_CLOSED, 66043),
            // SO-14 遗留 D10：收款退款过账必须有已通过的审批实例（66044）
            (ERP_RECEIPT_REFUND_APPROVAL_REQUIRED, 66044),
            // SO-27 销售订单 ↔ 销售合同（66045；66044 已被退款审批前置占用，只加不改）
            (ERP_SO_CONTRACT_CUSTOMER_MISMATCH, 66045),
            // FIN-ASSET-ST02 停用 / 启用（66046；66040 是「处置审批在跑」，语义不同不复用）
            (ERP_ASSET_STATUS_ILLEGAL, 66046),
            // FIX-3 处置前置闸（66048；处置当月折旧未过账，与 66038 期间已结账 / 66040 审批在跑分开）
            (ERP_ASSET_DEPRECIATION_PENDING, 66048),
            // P2-01 询比价定标闸（66047；单源例外 / 报价不匹配 / 状态不允许定标）
            (ERP_PRICE_INQUIRY_AWARD_GUARD, 66047),
            // P2-02 采购申请（66049–66050；状态闸与合并口径分离，前端文案不同）
            (ERP_REQUISITION_STATUS_GUARD, 66049),
            (ERP_REQUISITION_MERGE_MISMATCH, 66050),
            // P2-03 采购退货（66051–66053；防超退 / 状态闸 / 单价锁死各自独立）
            (ERP_PURCHASE_RETURN_OVER_RETURN, 66051),
            (ERP_PURCHASE_RETURN_STATUS_GUARD, 66052),
            (ERP_PURCHASE_RETURN_PRICE_LOCKED, 66053),
            // CODE-08 主数据编码开关（66054；与「编码格式不合法」10001 分开，前端引导不同）
            (ERP_MANUAL_CODE_FORBIDDEN, 66054),
            // 成本核算·关批前置闸（66055；ARCH-9 §4.1 的 5 条前置，逐条进报文）
            (ERP_COST_OBJECT_CLOSE_BLOCKED, 66055),
            // 成本核算·批号重复（66056；ARCH-9 §3.1 ① 的 uk_biz_lot_material_no）
            (ERP_LOT_NO_DUPLICATE, 66056),
            // 成本核算·领料挂的对象非 OPEN（66057；建单校验 + 过账复算兜底）
            (ERP_COST_OBJECT_NOT_OPEN, 66057),
            // 成本核算·工时「工时×费率」与金额不符（66058；入口核对 + ck_biz_cost_labor_amount）
            (ERP_COST_LABOR_AMOUNT_MISMATCH, 66058),
            // 成本核算·费率取数三级全空（66059；对象 > 车间 > 公司 都没命中，不按 0 计）
            (ERP_COST_RATE_MISSING, 66059),
            // 成本核算·分摊池无在产对象 / 基数合计为 0（66062–66063；B2-5 试算与 calc 第 5 段共用）
            (ERP_COST_ALLOC_NO_OBJECT, 66062),
            (ERP_COST_ALLOC_BASIS_ZERO, 66063),
            // 成本核算·calc 料归集遇到账面非法的来源领料行（66064；B2-6 段 1）
            (ERP_COST_COLLECT_SOURCE_INVALID, 66064),
            // 成本核算·calc 第 4 段池与 4101 借方不平（66065；B2-9 段 4）
            (ERP_COST_POOL_LEDGER_MISMATCH, 66065),
            // 成本核算·calc 第 5 段期间批次已锁定、禁止重算分摊（66066；B2-10 段 5）
            (ERP_COST_ALLOC_PERIOD_LOCKED, 66066),
            // 成本核算·calc 第 6 段产量/进度不合法、零成本完工、上期已红冲、计算单锁定、无批次（66067–66071；B2-11 段 6）
            (ERP_COST_OUTPUT_QTY_INVALID, 66067),
            (ERP_COST_FINISHED_WITHOUT_MATERIAL, 66068),
            (ERP_COST_PRIOR_PERIOD_REVERSED, 66069),
            (ERP_COST_ITEM_PERIOD_LOCKED, 66070),
            (ERP_COST_PERIOD_BATCH_MISSING, 66071),
            // 成本核算·同期间已有活跃批次 + 状态闸（66072–66073；B3-1 结算四件套骨架）
            (ERP_COST_PERIOD_DUPLICATE, 66072),
            (ERP_COST_PERIOD_STATUS_GUARD, 66073),
            // 成本核算·完工入库重复结算（66074；B3-4 —— 批次级 + 计算单级两道闸共用）
            (ERP_COST_FINISH_ALREADY_SETTLED, 66074),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "66xxx 段码值不得改动");
        }
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "66xxx 段不得有重复码值");
        assert!(
            codes.iter().all(|(code, _)| (66001..=66999).contains(code)),
            "66xxx 段必须落在 66001–66999"
        );
        println!("[码表] 66xxx 段 {} 个 ERP 码齐全且不重复 ✓", codes.len());
    }

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
        println!("[码表] 6100x 段 {} 个码齐全且不重复（61001–61012）✓", codes.len());
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
        assert_eq!(unique.len(), 24, "62xxx 段必须是 24 个码（C 段 19 + 偏好 5）");
        assert_eq!(unique.len(), codes.len(), "62xxx 段不得有重复码值");
        println!("[码表] 62xxx 段 {} 个码齐全且不重复（62001–62019）✓", codes.len());
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
            // T11 追加：父任务取消后的子任务守卫
            (TASK_PARENT_CANCELLED, 63021),
            // P2-9 追加：任务挂业务单据（类型白名单 / 单据不存在或已软删）
            (TASK_DOC_TYPE_INVALID, 63022),
            (TASK_DOC_NOT_FOUND, 63023),
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
            "[码表] 63xxx 段 {} 个码齐全且不重复（63001–63023 + 63101–63105 + 63110–63112 + 63301–63308）✓",
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
        println!("[码表] 634xx 段 {} 个码齐全且不重复（63401–63407）✓", codes.len());
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
        println!("[码表] 64xxx 段 {} 个码齐全且不重复（64001–64014）✓", codes.len());
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
            (FLOW_EXPRESSION_INVALID, 65033),
            (FLOW_NODE_CONFIG_MISMATCH, 65034),
            (FLOW_TASK_NODE_INVALID, 65035),
            (FLOW_NOTIFY_TEMPLATE_INVALID, 65036),
            (FLOW_TEMPLATE_VAR_INVALID, 65037),
            (FLOW_TASK_STATE_CONFLICT, 65038),
            (FLOW_PREVIEW_INVALID, 65039),
            (FLOW_ESCALATION_INVALID, 65040),
            (FLOW_ESCALATION_TOO_DEEP, 65041),
            (FLOW_FALLBACK_INVALID, 65042),
            (FLOW_DELEGATE_INVALID, 65043),
            (FLOW_SELF_APPROVAL_AMOUNT_LIMIT, 65044),
            (FLOW_SUBPROCESS_DEF_INVALID, 65045),
            (FLOW_SUBPROCESS_RECURSION, 65046),
            (FLOW_SUBPROCESS_START_FAILED, 65047),
            (FLOW_ALL_COUNTERSIGN_NO_GUARD, 65048),
        ];
        for (actual, expected) in codes {
            assert_eq!(actual, expected, "65xxx 段码值不得改动");
        }
        // D3a：卡面命名的 `TASK_FLOW_GATE_BLOCKED` 必须仍是 65030（别名，不新增码值）
        assert_eq!(TASK_FLOW_GATE_BLOCKED, 65030, "D3a 别名必须仍指向 65030");
        assert_eq!(TASK_FLOW_GATE_BLOCKED, TASK_APPROVAL_PENDING);
        // P5：调拨单侧的同义别名也必须指向 65030（同样不新增码值，故不进 `codes` 数组）
        assert_eq!(TRANSFER_FLOW_GATE_BLOCKED, 65030, "P5 别名必须仍指向 65030");
        assert_eq!(TRANSFER_FLOW_GATE_BLOCKED, TASK_APPROVAL_PENDING);
        let unique: std::collections::BTreeSet<i32> = codes.iter().map(|(c, _)| *c).collect();
        assert_eq!(unique.len(), codes.len(), "65xxx 段不得有重复码值");
        assert_eq!(
            unique.len(),
            43,
            "65xxx 段必须是 43 个码（架构 §10.3 的 22 个 + D2 追加的 5 个运行侧细分码 + P1-A2 的 65033 \
             + P-FLOW-1A 的 65034–65039 + P3-02 的 65040–65041 + P3-03 的 65042 + P3-04 的 65043 \
             + P3-05 的 65044 + P3-08 的 65045–65047 + E2E07-FIX-1 的 65048）"
        );
        assert!(
            codes.iter().all(|(code, _)| *code > 64014 && *code < 66000),
            "65xxx 段必须落在 64014 与 66000 之间（不侵占 AI 用户段）"
        );
        println!(
            "[码表] 65xxx 段 {} 个码齐全且不重复（65001–65005 / 65010–65028 / 65030–65048）✓",
            codes.len()
        );
    }
}
