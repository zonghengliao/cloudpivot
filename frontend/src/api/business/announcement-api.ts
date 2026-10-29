/**
 * 公告接口（批次 C · C2 用户侧 / C3 管理侧）。
 *
 * 路径与后端 `backend/src/business/announcement/handler.rs` 的 `#[alun::get/post/put/delete]`
 * 逐条对应（注解里不带 `/api` 前缀，前端补齐）。
 *
 * # 三条取值口径（照后端 DTO，别自己发明）
 *
 * 1. **枚举一律大写字面量**：`level` ∈ `NORMAL|IMPORTANT|URGENT`，
 *    `scope_type` ∈ `ALL|ROLE|DEPT|USER`，`status` ∈ `DRAFT|PUBLISHED|OFFLINE|EXPIRED`；
 *    后端会 `to_ascii_uppercase` 后再比对，前端统一传大写。
 * 2. **时间一律 RFC3339 字符串**（`Option<DateTime<Utc>>` 序列化结果），可空时间由页面渲染 `-`。
 * 3. **附件不给存储细节**：`download_url` 是后端代理下载入口，必须带鉴权头取 blob，
 *    不能直接 `<a href>`（浏览器不会带 `Authorization`）。
 */
import service, { del, get, post, put, type PageData } from '@/api/request';

/**
 * 把强类型查询对象交给 axios 的 `params`。
 *
 * 两点处理（都踩过）：
 * 1. `Record<string, unknown>` 需要索引签名，接口类型没有，所以做一次断言；
 * 2. **丢掉空串 / null / undefined**：后端把 `?status=` 当成「status 等于空串」的过滤条件
 *    （实测 `help/doc/page?status=` 返回 0 条），页面里 `query.status = ''` 表示「不过滤」，
 *    必须不发这个参数。
 */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 公告级别 */
export type AnnouncementLevel = 'NORMAL' | 'IMPORTANT' | 'URGENT';

/** 发布范围类型 */
export type AnnouncementScopeType = 'ALL' | 'ROLE' | 'DEPT' | 'USER';

/** 公告状态 */
export type AnnouncementStatus = 'DRAFT' | 'PUBLISHED' | 'OFFLINE' | 'EXPIRED';

/** 用户侧列表查询（对应后端 `VisibleQuery`） */
export interface VisibleQuery {
  /** 页码，从 1 开始 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 标题 / 摘要模糊搜索 */
  keyword?: string;
  /** 只看未读 */
  only_unread?: boolean;
}

/** 用户侧列表卡片（对应后端 `AnnouncementCardRes`） */
export interface AnnouncementCard {
  /** 公告 id */
  id: string;
  /** 公告编号 */
  ann_no: string;
  /** 标题 */
  title: string;
  /** 摘要（纯文本） */
  summary: string;
  /** 级别 */
  level: string;
  /** 是否置顶 */
  is_top: boolean;
  /** 发布时间 */
  publish_at: string | null;
  /** 我是否已读 */
  read: boolean;
  /** 附件数量 */
  attachment_count: number;
  /** 发布人 */
  publisher_name: string;
}

/** 附件（对应后端 `AttachmentRes`） */
export interface AnnouncementAttachment {
  /** 附件绑定行 id */
  id: string;
  /** 存储文件 id */
  file_id: string;
  /** 展示文件名 */
  file_name: string;
  /** 字节数 */
  file_size: number;
  /** MIME */
  mime_type: string;
  /** 排序号 */
  sort_order: number;
  /** 代理下载地址（含 `/api` 前缀，需带鉴权头取 blob） */
  download_url: string;
}

/** 用户侧详情（对应后端 `AnnouncementDetailRes`） */
export interface AnnouncementDetail {
  /** 公告 id */
  id: string;
  /** 公告编号 */
  ann_no: string;
  /** 标题 */
  title: string;
  /** 摘要 */
  summary: string;
  /** 后端渲染并净化过的 HTML（前端仍要再过一遍 DOMPurify 同款白名单） */
  content_html: string;
  /** 级别 */
  level: string;
  /** 发布范围类型 */
  scope_type: string;
  /** 状态 */
  status: string;
  /** 是否置顶 */
  is_top: boolean;
  /** 发布时间 */
  publish_at: string | null;
  /** 有效期 */
  expire_at: string | null;
  /** 发布人 */
  publisher_name: string;
  /** 我是否已读 */
  read: boolean;
  /** 我的阅读时间 */
  read_at: string | null;
  /** 附件列表 */
  attachments: AnnouncementAttachment[];
}

/** 未读数（对应后端 `UnreadCountRes`） */
export interface AnnouncementUnreadCount {
  /** 未读公告数 */
  announcement_unread: number;
}

/** 标已读结果（对应后端 `MarkReadRes`） */
export interface AnnouncementMarkRead {
  /** 本次实际标记的行数 */
  read_count: number;
  /** 是否首次已读 */
  first_read: boolean;
}

/** 全部已读结果（对应后端 `ReadAllRes`） */
export interface AnnouncementReadAll {
  /** 影响行数 */
  affected: number;
}

/** 管理侧列表查询（对应后端 `ManagePageQuery`） */
export interface ManagePageQuery {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 标题 / 摘要 / 编号模糊搜索 */
  keyword?: string;
  /** 状态过滤 */
  status?: string;
  /** 范围过滤 */
  scope_type?: string;
  /** 级别过滤 */
  level?: string;
  /** 发布起始（`YYYY-MM-DD` 或 RFC3339） */
  publish_from?: string;
  /** 发布截止 */
  publish_to?: string;
}

/** 管理侧列表行（对应后端 `ManageCardRes`） */
export interface ManageCard {
  /** 公告 id */
  id: string;
  /** 公告编号 */
  ann_no: string;
  /** 标题 */
  title: string;
  /** 摘要 */
  summary: string;
  /** 级别 */
  level: string;
  /** 范围类型 */
  scope_type: string;
  /** 状态 */
  status: string;
  /** 是否置顶 */
  is_top: boolean;
  /** 置顶到期 */
  top_expire_at: string | null;
  /** 发布时间 */
  publish_at: string | null;
  /** 有效期 */
  expire_at: string | null;
  /** 发布人 */
  publisher_name: string;
  /** 受众数 */
  receiver_count: number;
  /** 已读数 */
  read_count: number;
  /** 已读率（0–1） */
  read_rate: number;
  /** 附件数 */
  attachment_count: number;
  /** 版本号（每次发布 / 编辑 +1） */
  version: number;
  /** 关联站内信 id */
  notice_id: string | null;
  /** 更新时间 */
  updated_at: string | null;
}

/** 附件绑定入参（对应后端 `AnnouncementAttachmentReq`） */
export interface AnnouncementAttachmentRef {
  /** 存储文件 id */
  file_id: string;
  /** 排序号 */
  sort_order?: number;
}

/** 新增 / 编辑入参（对应后端 `AnnouncementSaveReq`） */
export interface AnnouncementSaveReq {
  /** 标题（必填） */
  title: string;
  /** 摘要（空则由后端从正文截取） */
  summary?: string;
  /** Markdown 正文（必填） */
  content_md: string;
  /** 级别 */
  level?: string;
  /** 范围类型 */
  scope_type?: string;
  /** 范围明细（范围非 ALL 时必填） */
  scope_ids?: string[];
  /** 是否置顶 */
  is_top?: boolean;
  /** 置顶到期（三态：不传=不改，null=清空，字符串=设置） */
  top_expire_at?: string | null;
  /** 有效期（三态） */
  expire_at?: string | null;
  /** 备注（三态） */
  remark?: string | null;
  /** 附件（整体替换） */
  attachments?: AnnouncementAttachmentRef[];
}

/** 保存结果（对应后端 `AnnouncementSaveRes`） */
export interface AnnouncementSaveResult {
  /** 公告 id */
  id: string;
  /** 公告编号 */
  ann_no: string;
  /** 状态 */
  status: string;
  /** 版本号 */
  version: number;
}

/** 管理侧详情（对应后端 `AnnouncementManageDetailRes`） */
export interface AnnouncementManageDetail {
  /** 公告 id */
  id: string;
  /** 公告编号 */
  ann_no: string;
  /** 标题 */
  title: string;
  /** 摘要 */
  summary: string;
  /** Markdown 原文 */
  content_md: string;
  /** 渲染后 HTML（已净化） */
  content_html: string;
  /** 级别 */
  level: string;
  /** 范围类型 */
  scope_type: string;
  /** 范围明细 id */
  scope_ids: string[];
  /** 状态 */
  status: string;
  /** 是否置顶 */
  is_top: boolean;
  /** 置顶到期 */
  top_expire_at: string | null;
  /** 发布时间 */
  publish_at: string | null;
  /** 有效期 */
  expire_at: string | null;
  /** 发布人 */
  publisher_name: string;
  /** 关联站内信 */
  notice_id: string | null;
  /** 受众数 */
  receiver_count: number;
  /** 已读数 */
  read_count: number;
  /** 已读率 */
  read_rate: number;
  /** 版本号 */
  version: number;
  /** 备注 */
  remark: string;
  /** 附件 */
  attachments: AnnouncementAttachment[];
  /** 创建时间 */
  created_at: string | null;
  /** 更新时间 */
  updated_at: string | null;
}

/** 预览入参（对应后端 `AnnouncementPreviewReq`） */
export interface AnnouncementPreviewReq {
  /** Markdown 正文 */
  content_md: string;
}

/** 预览结果（对应后端 `AnnouncementPreviewRes`） */
export interface AnnouncementPreviewResult {
  /** 渲染后 HTML（已净化） */
  content_html: string;
  /** 后端解析出的摘要 */
  summary: string;
  /** 渲染告警（如含外链图片） */
  warnings: string[];
}

/** 发布结果（对应后端 `AnnouncementPublishRes`） */
export interface AnnouncementPublishResult {
  /** 公告 id */
  id: string;
  /** 状态 */
  status: string;
  /** 版本号 */
  version: number;
  /** 关联站内信 id */
  notice_id: string | null;
  /** 关联站内信编号 */
  notice_no: string | null;
  /** 受众数 */
  receiver_count: number;
  /** 扇出状态（`DONE` / `PENDING` / `DISABLED` …） */
  fanout: string;
  /** 非致命告警 */
  warning: string | null;
}

/** 下线结果（对应后端 `AnnouncementOfflineRes`） */
export interface AnnouncementOfflineResult {
  /** 公告 id */
  id: string;
  /** 状态 */
  status: string;
  /** 版本号 */
  version: number;
}

/** 置顶结果（对应后端 `AnnouncementTopRes`） */
export interface AnnouncementTopResult {
  /** 公告 id */
  id: string;
  /** 是否置顶 */
  is_top: boolean;
  /** 置顶到期 */
  top_expire_at: string | null;
}

/** 阅读明细查询（对应后端 `ReadersQuery`） */
export interface ReadersQuery {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** `READ` / `UNREAD` / 空=全部 */
  state?: string;
  /** 用户名 / 姓名 / 部门模糊 */
  keyword?: string;
}

/** 阅读明细行（对应后端 `ReaderRes`） */
export interface ReaderRow {
  /** 用户 id */
  user_id: string;
  /** 用户名 */
  username: string;
  /** 姓名 */
  real_name: string;
  /** 部门名 */
  dept_name: string;
  /** 是否已读 */
  read: boolean;
  /** 阅读时间 */
  read_at: string | null;
}

/** 阅读明细响应（对应后端 `ReadersRes`） */
export interface ReadersResult {
  /** 公告 id */
  announcement_id: string;
  /** 受众数 */
  receiver_count: number;
  /** 已读数 */
  read_count: number;
  /** 已读率 */
  read_rate: number;
  /** 总行数 */
  total: number;
  /** 当前页 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 明细 */
  list: ReaderRow[];
}

// ===== 用户侧（`business:announcement:read`，全员默认授予） =====

/** 我能看的公告（分页） */
export function pageVisibleAnnouncements(query: VisibleQuery = {}): Promise<PageData<AnnouncementCard>> {
  return get<PageData<AnnouncementCard>>('/api/business/announcement/visible', asParams(query));
}

/** 公告详情（未发布 / 超出范围时后端返回 403 + 62005） */
export function getAnnouncementDetail(id: string): Promise<AnnouncementDetail> {
  return get<AnnouncementDetail>(`/api/business/announcement/${id}`);
}

/** 标记已读（幂等：重复调用 `read_count` 为 0、`first_read` 为 false） */
export function markAnnouncementRead(id: string): Promise<AnnouncementMarkRead> {
  return post<AnnouncementMarkRead>(`/api/business/announcement/${id}/read`);
}

/** 未读公告数 */
export function fetchAnnouncementUnreadCount(): Promise<AnnouncementUnreadCount> {
  return get<AnnouncementUnreadCount>('/api/business/announcement/unread-count');
}

/** 全部标记已读 */
export function markAllAnnouncementsRead(): Promise<AnnouncementReadAll> {
  return put<AnnouncementReadAll>('/api/business/announcement/read-all');
}

/**
 * 下载公告附件（附件代理接口要鉴权头，必须取 blob 再触发保存）。
 *
 * # 参数
 * - `downloadUrl`：后端 `AttachmentRes.download_url`（已含 `/api` 前缀）。
 */
export async function downloadAnnouncementAttachment(downloadUrl: string): Promise<Blob> {
  const response = await service.get<Blob>(downloadUrl, { responseType: 'blob' });
  return response.data;
}

// ===== 管理侧（`business:announcement:list` 起步） =====

/** 管理列表分页 */
export function pageManageAnnouncements(query: ManagePageQuery = {}): Promise<PageData<ManageCard>> {
  return get<PageData<ManageCard>>('/api/business/announcement/page', asParams(query));
}

/** 管理详情（含 Markdown 原文与范围明细，用于编辑回显） */
export function getManageAnnouncement(id: string): Promise<AnnouncementManageDetail> {
  return get<AnnouncementManageDetail>(`/api/business/announcement/${id}/manage`);
}

/** 新建公告（保存草稿） */
export function createAnnouncement(data: AnnouncementSaveReq): Promise<AnnouncementSaveResult> {
  return post<AnnouncementSaveResult>('/api/business/announcement', data);
}

/** 编辑公告（已发布公告编辑后仍需再发布才生效） */
export function updateAnnouncement(id: string, data: AnnouncementSaveReq): Promise<AnnouncementSaveResult> {
  return put<AnnouncementSaveResult>(`/api/business/announcement/${id}`, data);
}

/** 删除公告（仅草稿 / 已下线 / 已过期可删，软删） */
export function deleteAnnouncement(id: string): Promise<null> {
  return del<null>(`/api/business/announcement/${id}`);
}

/** 发布（扇出站内信 + 重算 HTML 快照） */
export function publishAnnouncement(id: string): Promise<AnnouncementPublishResult> {
  return post<AnnouncementPublishResult>(`/api/business/announcement/${id}/publish`);
}

/** 下线（原因必填，进变更日志 action_label） */
export function offlineAnnouncement(id: string, reason: string): Promise<AnnouncementOfflineResult> {
  return post<AnnouncementOfflineResult>(`/api/business/announcement/${id}/offline`, { reason });
}

/** 置顶 / 取消置顶（置顶到期时间可选） */
export function setAnnouncementTop(
  id: string,
  isTop: boolean,
  topExpireAt?: string | null,
): Promise<AnnouncementTopResult> {
  return put<AnnouncementTopResult>(`/api/business/announcement/${id}/top`, {
    is_top: isTop,
    top_expire_at: topExpireAt ?? null,
  });
}

/** 渲染预览（后端权威渲染器，前端不自造 Markdown 子集） */
export function previewAnnouncement(contentMd: string): Promise<AnnouncementPreviewResult> {
  return post<AnnouncementPreviewResult>('/api/business/announcement/preview', { content_md: contentMd });
}

/** 追加一个附件（保留已有附件，不整体替换） */
export function bindAnnouncementAttachment(
  id: string,
  ref: AnnouncementAttachmentRef,
): Promise<AnnouncementAttachment> {
  return post<AnnouncementAttachment>(`/api/business/announcement/${id}/attachment`, ref);
}

/** 阅读明细（受众 + 已读状态） */
export function pageAnnouncementReaders(id: string, query: ReadersQuery = {}): Promise<ReadersResult> {
  return get<ReadersResult>(`/api/business/announcement/${id}/readers`, asParams(query));
}

/** 公告管理接口集合（按域内聚，便于 `import { announcementApi }`） */
export const announcementApi = {
  pageVisibleAnnouncements,
  getAnnouncementDetail,
  markAnnouncementRead,
  fetchAnnouncementUnreadCount,
  markAllAnnouncementsRead,
  downloadAnnouncementAttachment,
  pageManageAnnouncements,
  getManageAnnouncement,
  createAnnouncement,
  updateAnnouncement,
  deleteAnnouncement,
  publishAnnouncement,
  offlineAnnouncement,
  setAnnouncementTop,
  previewAnnouncement,
  bindAnnouncementAttachment,
  pageAnnouncementReaders,
};
