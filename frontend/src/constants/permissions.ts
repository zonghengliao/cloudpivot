/**
 * 批次 A（对象存储 / 导入导出 / 消息通知）+ 批次 B（数据权限 / 字段脱敏 / 角色互斥）
 * + 批次 C（公告 / 帮助 / 变更日志）+ 批次 C+（工作任务）权限码字典。
 *
 * # 单一来源
 *
 * 每个常量都必须与以下三处**逐字一致**，任何一处改动都要三处同步：
 * 1. 后端 `#[alun::permission(permission = "...")]` 注解；
 * 2. 菜单种子 `sys_menu.auth_code`（`V0046` / `V0047`）；
 * 3. 本文件。
 *
 * 页面里**禁止**再写权限码字面量，统一 `v-privilege="[PERM.xxx]"`，改码时只改一处。
 *
 * # 已知偏差（记入 A7 summary，不在前端单方面抹平）
 *
 * - 架构文档 §3.8 写 `business:impexp:import:download-error`，A3 后端实际注解是
 *   `business:impexp:import:download`，本文件以后端为准；
 * - PRD 第七章菜单树给的是一套更细的码（如 `business:storage:backend:list`），
 *   与已验收的后端注解不一致，验收以架构文档 §5.1 + 后端注解为准。
 */

/** 对象存储权限码（11 个） */
export const STORAGE_PERMISSIONS = {
  /** 后端配置查看 */
  storageConfigList: 'business:storage:config:list',
  /** 新增后端 */
  storageConfigCreate: 'business:storage:config:create',
  /** 修改 / 设默认 / 启停 */
  storageConfigUpdate: 'business:storage:config:update',
  /** 删除后端 */
  storageConfigDelete: 'business:storage:config:delete',
  /** 连通性探测 */
  storageConfigTest: 'business:storage:config:test',
  /** 策略查看 */
  storagePolicyList: 'business:storage:policy:list',
  /** 策略修改 */
  storagePolicyUpdate: 'business:storage:policy:update',
  /** 上传（含直传初始化 / 完成） */
  storageUpload: 'business:storage:upload',
  /** 文件分页 */
  storageList: 'business:storage:list',
  /** 下载 / 取限时链接 */
  storageDownload: 'business:storage:download',
  /** 软删文件 */
  storageDelete: 'business:storage:delete',
} as const;

/** 导入导出权限码（14 个） */
export const IMPEXP_PERMISSIONS = {
  /** 可用导入 / 导出定义 */
  impexpDefList: 'business:impexp:def:list',
  /** 下载导入模板 */
  impexpImportTemplate: 'business:impexp:import:template',
  /** 上传并建导入任务 */
  impexpImportUpload: 'business:impexp:import:upload',
  /** 任务列表 / 详情 / 错误明细 */
  impexpImportList: 'business:impexp:import:list',
  /** 确认入库 */
  impexpImportConfirm: 'business:impexp:import:confirm',
  /** 取消导入 */
  impexpImportCancel: 'business:impexp:import:cancel',
  /** 失败重试 */
  impexpImportRetry: 'business:impexp:import:retry',
  /** 下载「错误行」文件（后端注解口径） */
  impexpImportDownload: 'business:impexp:import:download',
  /** 建导出任务 / 同步导出 */
  impexpExportCreate: 'business:impexp:export:create',
  /** 导出任务列表 / 详情 */
  impexpExportList: 'business:impexp:export:list',
  /** 下载导出结果 */
  impexpExportDownload: 'business:impexp:export:download',
  /** 取消导出 */
  impexpExportCancel: 'business:impexp:export:cancel',
  /** 重试导出（仅失败 / 已取消 / 已过期；V0052 补种） */
  impexpExportRetry: 'business:impexp:export:retry',
  /** 删除导出任务 */
  impexpExportDelete: 'business:impexp:export:delete',
} as const;

/** 消息通知权限码（9 个） */
export const NOTICE_PERMISSIONS = {
  /** 发送通知 */
  noticeSend: 'business:notice:send',
  /** 通知主体列表 / 详情 */
  noticeList: 'business:notice:list',
  /** 删除通知 */
  noticeDelete: 'business:notice:delete',
  /** 我的通知 / 未读数 / 已读 / SSE（授予所有角色） */
  noticeMine: 'business:notice:mine',
  /** 模板列表 / 预览 */
  noticeTemplateList: 'business:notice:template:list',
  /** 新建模板 */
  noticeTemplateCreate: 'business:notice:template:create',
  /** 修改模板 */
  noticeTemplateUpdate: 'business:notice:template:update',
  /** 删除模板 */
  noticeTemplateDelete: 'business:notice:template:delete',
  /** 通道配置查看 */
  noticeChannelConfig: 'business:notice:channel:config',
} as const;

/**
 * 批次 B（数据权限 / 字段脱敏 / 角色互斥）权限码字典 —— 15 个节点（3 菜单 + 12 按钮）。
 *
 * # 单一来源
 *
 * 与批次 A 同样的纪律：每个常量必须与以下三处**逐字一致**，改一处要三处同步：
 * 1. 后端 `#[alun::permission(permission = "...")]` 注解；
 * 2. 菜单种子 `sys_menu.auth_code`（`V0065` + `V0068`）；
 * 3. 本文件。
 *
 * # 与 PRD / 架构的已知偏差（以后端已验收的注解为准，前端不单方面抹平）
 *
 * - PRD §6.1 写 `system:data-scope:{view,update,check}`、`system:role-mutex:matrix`、
 *   `system:field-mask:{status,view-plain,scan}`，共 16 个码；
 * - 后端实际落库 15 个：数据范围三码改域为 `business:permission:scope:{read,write,self-check}`
 *   （`V0065` 头注释记了原因：配置接口落在 `business/permission/`），脱敏三码合并为
 *   `system:field-mask:{list,create,update,delete,plain}`（启停走 update，无独立 status 码），
 *   互斥四码为 `system:role-mutex:{list,create,delete,conflict}`（编辑复用 create，
 *   扫描复用 conflict，无 matrix / status 码）。
 * - 偏差已记入 B8 summary；页面上出现的就是后端能放行的码，**不存在「按钮能点但接口 403」**。
 */
export const BATCH_B_PERMISSIONS = {
  /** 查看角色数据范围配置 / 自定义部门树（同时是「数据权限」菜单的入口码） */
  scopeRead: 'business:permission:scope:read',
  /** 保存角色数据范围（含自定义部门集合） */
  scopeWrite: 'business:permission:scope:write',
  /** 数据权限自检 / 未接入表清单 */
  scopeSelfCheck: 'business:permission:scope:self-check',
  /** 字段脱敏规则列表 / 试运行 / 生效清单（同时是菜单入口码） */
  fieldMaskList: 'system:field-mask:list',
  /** 新建脱敏规则 */
  fieldMaskCreate: 'system:field-mask:create',
  /** 修改脱敏规则 / 启停（后端无独立 status 接口） */
  fieldMaskUpdate: 'system:field-mask:update',
  /** 删除脱敏规则（软删） */
  fieldMaskDelete: 'system:field-mask:delete',
  /** 查看未脱敏明文（豁免码，判定位，无独立路由） */
  fieldMaskPlain: 'system:field-mask:plain',
  /** 互斥规则列表（同时是菜单入口码） */
  mutexList: 'system:role-mutex:list',
  /** 新建 / 修改互斥规则（后端同一码） */
  mutexCreate: 'system:role-mutex:create',
  /** 删除互斥规则（软删 = 停用） */
  mutexDelete: 'system:role-mutex:delete',
  /** 冲突预检：单用户体检 + 存量全量扫描 */
  mutexConflict: 'system:role-mutex:conflict',
} as const;

/**
 * 批次 C（公告管理 / 帮助文档 / 变更日志）权限码字典 —— 20 个节点。
 *
 * # 与 PRD §6.1 的偏差（**以后端已验收的注解与迁移种子为准**，不在前端单方面抹平）
 *
 * - PRD 写 `business:help:*`（7 码）、`business:changelog:*`（4 码）、公告 10 码；
 * - 后端实际落库 20 个：
 *   · 公告 7 个 `business:announcement:{read,list,create,update,delete,publish,offline}`
 *     —— 读码是 `read`（不是 `view`，`V0087` 补种并**授予所有未软删角色**），
 *     下线单独成码 `offline`（PRD 叫 `revoke`），**没有** `publish-all` / `top` /
 *     `read-stat`：全员发布未拆码，置顶与阅读明细分别复用 `update` / `list`；
 *   · 帮助 10 个 `system:help:{read,doc:list,doc:create,doc:update,doc:delete,doc:publish,
 *     category:list,category:create,category:update,category:delete}`（`V0088`），
 *     PRD 的 `business:help:view` 对应 `system:help:read`，分类四码是 `category:*`；
 *   · 变更日志 3 个 `system:change-log:{list,export,clean}`（`V0085`），
 *     PRD 的 `detail` 码不存在（详情复用 `list`），另有 `clean`（清分区）本批不出按钮。
 *
 * 结论：页面里出现的就是后端能放行的码，**不存在「按钮能点但接口 403」**。
 */

/** 公告权限码（7 个，`business:announcement:*`） */
export const ANNOUNCEMENT_PERMISSIONS = {
  /** 阅读公告（用户端；所有未软删角色默认授予） */
  announcementRead: 'business:announcement:read',
  /** 公告管理列表 / 详情 / 阅读统计 / 预览（管理端入口码） */
  announcementList: 'business:announcement:list',
  /** 新建草稿 */
  announcementCreate: 'business:announcement:create',
  /** 编辑 / 置顶 / 取消置顶 */
  announcementUpdate: 'business:announcement:update',
  /** 删除（软删） */
  announcementDelete: 'business:announcement:delete',
  /** 发布（含扇出 / 再次发布） */
  announcementPublish: 'business:announcement:publish',
  /** 下线 */
  announcementOffline: 'business:announcement:offline',
} as const;

/** 帮助文档权限码（10 个，`system:help:*`） */
export const HELP_PERMISSIONS = {
  /** 阅读帮助文档（分类树 / 列表 / 详情 / 检索；所有未软删角色默认授予） */
  helpRead: 'system:help:read',
  /** 文档管理列表 / 编辑回显 / 版本列表（管理端入口码） */
  helpDocList: 'system:help:doc:list',
  /** 新建文档（草稿） */
  helpDocCreate: 'system:help:doc:create',
  /** 编辑文档（敏感：官方口径变更单独授权） */
  helpDocUpdate: 'system:help:doc:update',
  /** 删除文档（软删） */
  helpDocDelete: 'system:help:doc:delete',
  /** 发布 / 回滚文档 */
  helpDocPublish: 'system:help:doc:publish',
  /** 分类管理列表（含停用） */
  helpCategoryList: 'system:help:category:list',
  /** 新建分类 */
  helpCategoryCreate: 'system:help:category:create',
  /** 编辑 / 移动分类 */
  helpCategoryUpdate: 'system:help:category:update',
  /** 删除分类 */
  helpCategoryDelete: 'system:help:category:delete',
} as const;

/** 变更日志权限码（3 个，`system:change-log:*`） */
export const CHANGELOG_PERMISSIONS = {
  /** 全局检索 / 详情 / 对象历史 / 分区清单（入口码） */
  changelogList: 'system:change-log:list',
  /** 导出检索结果（导出的是脱敏数据，每次导出写审计） */
  changelogExport: 'system:change-log:export',
  /** 手工清理过期分区（本批不出按钮，仅登记码值） */
  changelogClean: 'system:change-log:clean',
} as const;

/**
 * 工作任务权限码（4 个，`business:task:*`）。
 *
 * 与后台 `V0103__seed_task_permissions.up.sql` 的 4 个 `auth_code` 逐字一致
 * （第 5 个节点 …0760 是页面本身，auth_code 复用 `read`）；后端注解在
 * `backend/src/business/task/handler.rs`。三处任一处改名都要同步。
 */
export const TASK_PERMISSIONS = {
  /** 查看任务（菜单入口 / 分页 / 详情 / 我的任务） */
  taskRead: 'business:task:read',
  /** 新建任务 */
  taskCreate: 'business:task:create',
  /** 编辑任务（后端只认负责人 / 创建人） */
  taskUpdate: 'business:task:update',
  /** 删除任务（软删：负责人 / 创建人 / 管理员） */
  taskDelete: 'business:task:delete',
} as const;

/** 全部权限码合集（批次 A + B + C + C+；`PERM.xxx` 是页面里唯一的取码方式） */
export const PERM = {
  ...STORAGE_PERMISSIONS,
  ...IMPEXP_PERMISSIONS,
  ...NOTICE_PERMISSIONS,
  ...BATCH_B_PERMISSIONS,
  ...ANNOUNCEMENT_PERMISSIONS,
  ...HELP_PERMISSIONS,
  ...CHANGELOG_PERMISSIONS,
  ...TASK_PERMISSIONS,
} as const;

/** 批次 A 权限码总数自检值（34 —— V0052 补种 `business:impexp:export:retry`） */
export const BATCH_A_PERMISSION_COUNT = 34;

/** 批次 C 权限码总数自检值（20 —— 公告 7 + 帮助 10 + 变更日志 3） */
export const BATCH_C_PERMISSION_COUNT = 20;
