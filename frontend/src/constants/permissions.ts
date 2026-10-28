/**
 * 批次 A（对象存储 / 导入导出 / 消息通知）权限码字典。
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

/** 全部批次 A 权限码合集（供迁移脚本与自检脚本比对） */
export const PERM = {
  ...STORAGE_PERMISSIONS,
  ...IMPEXP_PERMISSIONS,
  ...NOTICE_PERMISSIONS,
} as const;

/** 批次 A 权限码总数自检值（34 —— V0052 补种 `business:impexp:export:retry`） */
export const BATCH_A_PERMISSION_COUNT = 34;
