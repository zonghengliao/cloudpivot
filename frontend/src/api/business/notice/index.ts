/**
 * 消息通知接口（批次 A · A5/A6）。
 *
 * 路径与后端 `backend/src/business/notice/handler.rs` 的 `#[nik::get/post/put/delete]` 逐条对应。
 *
 * # 取值口径
 *
 * 库内与响应里 `notice_type` / `level` 均为**大写字面量**（`SYSTEM` / `TODO` / `WARN` …），
 * 入参 `NoticeSendReq.notice_type` 与 `level` 大小写不敏感（后端 `parse` 会先 `to_ascii_uppercase`）。
 *
 * # SSE
 *
 * 实时推送是普通 `EventSource` 天然拿不到 `Authorization` 头的接口，因此：
 * 1. 优先用 `token` 查询参数订阅（A6 handler 支持 `?token=`，与 `EventSource` 兼容）；
 * 2. 订阅失败（网络 / 403）时降级为**轮询未读数**，保证角标不会长期停在旧值。
 */
import { del, get, post, put, TOKEN_KEY, type PageData } from '@/api/request';

/** 通知类型 */
export type NoticeType = 'SYSTEM' | 'TODO' | 'ALERT' | 'WORKFLOW' | 'CIRCULAR';

/** 通知级别 */
export type NoticeLevel = 'INFO' | 'SUCCESS' | 'WARN' | 'ERROR';

/** 正文格式 */
export type ContentFormat = 'TEXT' | 'HTML' | 'MARKDOWN';

/** 发送目标类型：`USER` 指定用户 / `ROLE` 角色 / `DEPT` 部门含下级 / `ALL` 全员 */
export type NoticeTargetType = 'USER' | 'ROLE' | 'DEPT' | 'ALL';

/** 模板变量声明（对应后端 `TemplateVarDecl`） */
export interface TemplateVarDecl {
  /** 变量名（不含花括号） */
  name: string;
  /** 中文说明 */
  label?: string | null;
  /** 是否必填 */
  required: boolean;
  /** 示例值（预览用） */
  sample?: string | null;
}

/** 发送通知参数（对应后端 `NoticeSendReq`） */
export interface NoticeSendReq {
  /** 类型 */
  notice_type?: string;
  /** 级别 */
  level?: string;
  /** 标题（与模板二选一） */
  title?: string;
  /** 正文 */
  content?: string;
  /** 正文格式 */
  content_format?: string;
  /** 点击跳转地址 */
  link_url?: string;
  /** 业务类型 */
  biz_type?: string;
  /** 业务 id */
  biz_id?: string;
  /** 模板编码（用模板渲染时必填） */
  template_code?: string;
  /** 模板变量 */
  template_vars?: Record<string, string>;
  /** 目标类型 */
  target_type: string;
  /** 目标 id 列表（`USER` 用户 / `ROLE` 角色 / `DEPT` 部门；`ALL` 留空） */
  target_ids?: string[];
  /** 通道掩码（站内信恒为开启） */
  channel_mask?: number;
  /** 幂等键 */
  idempotency_key?: string;
  /** 过期时间 */
  expire_at?: string;
}

/** 通知主体（对应后端 `NoticeRes`） */
export interface NoticeRes {
  /** 通知 id */
  id: string;
  /** 通知编号 */
  notice_no: string;
  /** 类型 */
  notice_type: string;
  /** 级别 */
  level: string;
  /** 标题 */
  title: string;
  /** 正文 */
  content: string;
  /** 正文格式 */
  content_format: string;
  /** 跳转地址 */
  link_url: string;
  /** 业务类型 */
  biz_type: string;
  /** 业务 id */
  biz_id: string;
  /** 模板编码 */
  template_code: string;
  /** 发送人 id */
  sender_id: string;
  /** 发送人名称 */
  sender_name: string;
  /** 目标类型 */
  target_type: string;
  /** 目标 id 列表 */
  target_ids: string[];
  /** 通道掩码 */
  channel_mask: number;
  /** 已启用通道名称 */
  channels: string[];
  /** 接收人数 */
  receiver_count: number;
  /** 已读人数 */
  read_count: number;
  /** 已读率（0~1） */
  read_rate: number;
  /** 状态 */
  status: number;
  /** 过期时间 */
  expire_at?: string | null;
  /** 创建时间 */
  created_at: string;
}

/** 送达统计（对应后端 `DeliverySummaryRes`） */
export interface DeliverySummaryRes {
  /** 通道名称 */
  channel: string;
  /** 通道状态 */
  status: string;
  /** 条数 */
  count: number;
}

/** 通知详情（对应后端 `NoticeDetailRes`） */
export interface NoticeDetailRes {
  /** 主体 */
  notice: NoticeRes;
  /** 已读数 */
  read_count: number;
  /** 未读数 */
  unread_count: number;
  /** 已删除数 */
  deleted_count: number;
  /** 各通道送达统计 */
  deliveries: DeliverySummaryRes[];
}

/** 我的通知条目（对应后端 `MyNoticeRes`） */
export interface MyNoticeRes {
  /** 接收行 id（已读 / 删除都按它操作） */
  receiver_id: string;
  /** 通知 id */
  notice_id: string;
  /** 通知编号 */
  notice_no: string;
  /** 类型 */
  notice_type: string;
  /** 级别 */
  level: string;
  /** 标题 */
  title: string;
  /** 正文 */
  content: string;
  /** 正文格式 */
  content_format: string;
  /** 跳转地址 */
  link_url: string;
  /** 是否已读 */
  read_flag: boolean;
  /** 已读时间 */
  read_at?: string | null;
  /** 接收时间 */
  created_at: string;
}

/** 未读数响应（对应后端 `UnreadCountRes`） */
export interface UnreadCountRes {
  /** 未读条数 */
  unread_count: number;
}

/** 通知模板（对应后端 `TemplateRes`） */
export interface NoticeTemplateRes {
  /** 模板 id */
  id: string;
  /** 模板编码 */
  template_code: string;
  /** 模板名称 */
  template_name: string;
  /** 类型 */
  notice_type: string;
  /** 默认级别 */
  default_level: string;
  /** 标题模板（支持 `{{var}}`） */
  title_template: string;
  /** 正文模板 */
  content_template: string;
  /** 正文格式 */
  content_format: string;
  /** 跳转地址模板 */
  link_template: string;
  /** 通道（逗号分隔） */
  channels: string;
  /** 通道掩码 */
  channel_mask: number;
  /** 变量声明 */
  variables: TemplateVarDecl[];
  /** 状态 */
  status: number;
  /** 备注 */
  remark: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 模板保存参数（对应后端 `TemplateSaveReq`） */
export interface NoticeTemplateSaveReq {
  /** 模板编码（新增后不可改） */
  template_code: string;
  /** 模板名称 */
  template_name: string;
  /** 类型 */
  notice_type?: string;
  /** 默认级别 */
  default_level?: string;
  /** 标题模板 */
  title_template: string;
  /** 正文模板 */
  content_template: string;
  /** 正文格式 */
  content_format?: string;
  /** 跳转地址模板 */
  link_template?: string;
  /** 通道 */
  channels?: string;
  /** 变量声明 */
  variables?: TemplateVarDecl[];
  /** 状态 */
  status?: number;
  /** 备注 */
  remark?: string;
}

/** 试渲染结果（对应后端 `TemplatePreviewRes`） */
export interface TemplatePreviewRes {
  /** 渲染后的标题 */
  title: string;
  /** 渲染后的正文 */
  content: string;
}

/** 通道配置（对应后端 `ChannelConfigRes`） */
export interface ChannelConfigRes {
  /** 站内信 */
  inbox: boolean;
  /** 邮件 */
  email: boolean;
  /** Webhook */
  webhook: boolean;
  /** 短信 */
  sms: boolean;
  /** Webhook 地址（掩码） */
  webhook_masked: string;
}

/** 通知主体分页查询参数 */
export type NoticePageQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 标题模糊 */
  keyword?: string;
  /** 类型 */
  notice_type?: string;
  /** 级别 */
  level?: string;
  /** 业务类型 */
  biz_type?: string;
};

/** 我的通知分页查询参数 */
export type MyNoticeQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** `true` 已读 / `false` 未读 / 不传全部 */
  read_flag?: boolean;
};

/** 模板分页查询参数 */
export type TemplatePageQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 名称 / 编码模糊 */
  keyword?: string;
  /** 类型 */
  notice_type?: string;
  /** 状态 */
  status?: number;
};

/** 发送通知（管理端） */
export function sendNotice(data: NoticeSendReq): Promise<NoticeRes> {
  return post<NoticeRes>('/api/business/notice', data);
}

/** 通知主体分页（管理视角，含送达 / 已读统计） */
export function pageNotices(query: NoticePageQuery): Promise<PageData<NoticeRes>> {
  return get<PageData<NoticeRes>>('/api/business/notice/page', query);
}

/** 通知详情（含送达统计） */
export function getNotice(id: string): Promise<NoticeDetailRes> {
  return get<NoticeDetailRes>(`/api/business/notice/${id}`);
}

/** 删除通知（级联接收行） */
export function deleteNotice(id: string): Promise<null> {
  return del<null>(`/api/business/notice/${id}`);
}

/** 我的通知分页 */
export function pageMyNotices(query: MyNoticeQuery): Promise<PageData<MyNoticeRes>> {
  return get<PageData<MyNoticeRes>>('/api/business/notice/mine', query);
}

/** 未读数（顶栏角标与消息中心共用） */
export function fetchUnreadCount(): Promise<UnreadCountRes> {
  return get<UnreadCountRes>('/api/business/notice/unread-count');
}

/** 单条已读 */
export function markNoticeRead(receiverId: string): Promise<null> {
  return put<null>(`/api/business/notice/read/${receiverId}`);
}

/** 全部已读 */
export function markAllNoticeRead(): Promise<null> {
  return put<null>('/api/business/notice/read-all');
}

/** 删除我的这条 */
export function deleteMyNotice(receiverId: string): Promise<null> {
  return del<null>(`/api/business/notice/mine/${receiverId}`);
}

/** 模板分页 */
export function pageNoticeTemplates(query: TemplatePageQuery): Promise<PageData<NoticeTemplateRes>> {
  return get<PageData<NoticeTemplateRes>>('/api/business/notice/template/page', query);
}

/** 新建模板 */
export function createNoticeTemplate(data: NoticeTemplateSaveReq): Promise<NoticeTemplateRes> {
  return post<NoticeTemplateRes>('/api/business/notice/template', data);
}

/** 修改模板 */
export function updateNoticeTemplate(id: string, data: NoticeTemplateSaveReq): Promise<NoticeTemplateRes> {
  return put<NoticeTemplateRes>(`/api/business/notice/template/${id}`, data);
}

/** 删除模板（被引用时 409 + E-NTC-09） */
export function deleteNoticeTemplate(id: string): Promise<null> {
  return del<null>(`/api/business/notice/template/${id}`);
}

/** 试渲染（前端「预览」按钮与保存前校验共用） */
export function previewNoticeTemplate(id: string, vars: Record<string, string>): Promise<TemplatePreviewRes> {
  return post<TemplatePreviewRes>(`/api/business/notice/template/${id}/preview`, { vars });
}

/** 通道配置读取 */
export function fetchChannelConfig(): Promise<ChannelConfigRes> {
  return get<ChannelConfigRes>('/api/business/notice/channel/config');
}

/**
 * 构造 SSE 订阅地址。
 *
 * `EventSource` 不能自定义请求头，因此令牌走查询参数；调用方需在后端侧确认
 * `notice/stream` 接受 `?token=`（需后端 NIK_AUTH_QUERY_TOKEN_PATHS 白名单放行），
 * 否则降级轮询。
 *
 * 2026-10-30 修：原路径写成 `/notice/sse`，后端实际注册的是 `/notice/stream`，
 * 导致 SSE 连接一直 401、实时推送完全没工作过。
 */
export function buildNoticeSseUrl(): string {
  const base = import.meta.env.VITE_APP_BASE_API || '';
  const token = localStorage.getItem(TOKEN_KEY) || '';
  const query = token ? `?token=${encodeURIComponent(token)}` : '';
  return `${base}/api/business/notice/stream${query}`;
}

/**
 * 构造 SSE 订阅地址（**不带令牌**）。
 *
 * 给 `fetch` + `ReadableStream` 那条读者用：令牌放在 `Authorization` 头里，
 * 不依赖 `NIK_AUTH_QUERY_TOKEN_PATHS` 白名单（该环境变量默认空 ⇒ `?token=` 版本实测 401）。
 */
export function buildNoticeStreamUrl(): string {
  const base = import.meta.env.VITE_APP_BASE_API || '';
  return `${base}/api/business/notice/stream`;
}

/** 消息通知模块接口集合 */
export const noticeApi = {
  sendNotice,
  pageNotices,
  getNotice,
  deleteNotice,
  pageMyNotices,
  fetchUnreadCount,
  markNoticeRead,
  markAllNoticeRead,
  deleteMyNotice,
  pageNoticeTemplates,
  createNoticeTemplate,
  updateNoticeTemplate,
  deleteNoticeTemplate,
  previewNoticeTemplate,
  fetchChannelConfig,
  buildNoticeSseUrl,
  buildNoticeStreamUrl,
};
