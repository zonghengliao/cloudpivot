/**
 * 业务单据附件 API（ARCH-3 DC-17 / P1-4）。
 *
 * # 契约来源
 *
 * - 后端 handler：`backend/src/business/attachment/handler.rs`
 * - 出参结构：`backend/src/business/attachment/dto.rs`（`DocAttachmentItemRes` / `DocAttachmentListRes` …）
 * - 表与权限码：`backend/migrations/business/V0212__erp_doc_attachment.up.sql`
 *
 * # 路径必须写全 `/api/...`
 *
 * 后端路由宏里的路径**不含** `/api`（由 `[router].prefix` 注入）。前端漏前缀会命中 SPA
 * fallback 返回 `index.html`，报「响应体格式不符合统一响应规范」——本仓库最高频的坑。
 *
 * # 权限码
 *
 * | 操作 | 权限码 |
 * |------|--------|
 * | 上传 | `business:attachment:upload` |
 * | 列表 / 下载 | `business:attachment:list` |
 * | 删**别人**的附件 | `business:attachment:delete`（上传人可删自己的，后端行级判据） |
 */

import service, { del, get, put, ApiError, type ApiResult } from '@/api/request';
import {
  DOC_KIND_LABELS,
  type AttachmentDocType,
} from '@/api/business/erp/doc-kinds';

/**
 * 单据类型（附件可挂的 7 类）。
 *
 * P2-5 词表收敛：取值来自 `doc-kinds.ts` 的 `ATTACHMENT_DOC_TYPES`（唯一真相），
 * 与后端 `doc_kind::ATTACHMENT_DOC_KINDS` + DDL 的 `ck_biz_doc_attachment_type` 逐字一致。
 * 后端单测会比对两边的取值与顺序，漂了直接红。
 *
 * 注意：词表每加一类，本文件两张 `Record<DocType, …>` 穷举映射表
 * （`DOC_TYPE_LABELS`、`DEFAULT_ROLE_BY_DOC`）必须同步补，否则 `vue-tsc` 报 TS2741。
 */
export type DocType = AttachmentDocType;

/** 文件角色（与 DDL 的 `ck_biz_doc_attachment_role` 逐字一致）。 */
export type FileRole = 'CONTRACT_SCAN' | 'SIGN_RECEIPT' | 'INVOICE' | 'OTHER';

/** 单据类型中文名（错误提示 / 空态文案用；取自词表，不另写一份）。 */
export const DOC_TYPE_LABELS: Record<DocType, string> = {
  CONTRACT: DOC_KIND_LABELS.CONTRACT,
  PURCHASE_ORDER: DOC_KIND_LABELS.PURCHASE_ORDER,
  STOCK_IN: DOC_KIND_LABELS.STOCK_IN,
  PAYABLE: DOC_KIND_LABELS.PAYABLE,
  PAYMENT: DOC_KIND_LABELS.PAYMENT,
  FIXED_ASSET: DOC_KIND_LABELS.FIXED_ASSET,
  EXPENSE_REPORT: DOC_KIND_LABELS.EXPENSE_REPORT,
};

/**
 * 文件角色下拉项。
 *
 * 顺序按「最常见」排：合同扫描件 → 签收单 → 发票影像 → 其他。
 * 合同页也会用它（合同页默认选 `CONTRACT_SCAN`，其余页面默认 `OTHER`）。
 */
export const FILE_ROLE_OPTIONS: { value: FileRole; label: string }[] = [
  { value: 'CONTRACT_SCAN', label: '合同扫描件' },
  { value: 'SIGN_RECEIPT', label: '签收单' },
  { value: 'INVOICE', label: '发票影像' },
  { value: 'OTHER', label: '其他' },
];

/** 一条附件（对应后端 `DocAttachmentItemRes`）。 */
export interface DocAttachmentItem {
  /** 绑定行主键（删除 / 下载用） */
  id: string;
  doc_type: string;
  doc_id: string;
  /** `sys_file.id` */
  file_id: string;
  file_role: string;
  /** 角色中文名（后端下发，前端不再维护第二份词表） */
  file_role_label: string;
  remark: string | null;
  file_name: string;
  file_size: number;
  mime_type: string | null;
  /** 手动排序序号（ATT-03：手动模式下按它升序；后端 `sort_order ASC`） */
  sort_order: number;
  uploaded_by: string | null;
  uploader_name: string;
  created_at: string;
  /** 当前用户能否删除（上传人本人 ∪ `business:attachment:delete`） */
  can_delete: boolean;
}

/** 列表排序口径（ATT-03，对应后端 `attachment::model::ListOrder`） */
export type DocAttachmentSortMode = 'time' | 'manual';

/** 列表（对应后端 `DocAttachmentListRes`）。 */
export interface DocAttachmentList {
  doc_type: string;
  doc_id: string;
  list: DocAttachmentItem[];
  total: number;
  /** 单张单据的附件上限（后端 `MAX_ATTACHMENTS_PER_DOC` = 50） */
  max_count: number;
  /** 本次响应用的排序口径（后端回显；`PUT /sort` 固定回 `manual`） */
  sort: DocAttachmentSortMode;
}

/** 上传结果（对应后端 `DocAttachmentUploadRes`）。 */
export interface DocAttachmentUploadResult {
  id: string;
  file_id: string;
  file_name: string;
  file_size: number;
  file_role: string;
  doc_type: string;
  doc_id: string;
  /** 是否命中内容秒传（同一份字节此前上传过 → 复用 `sys_file` 记录） */
  duplicated: boolean;
}

/** 删除结果（对应后端 `DocAttachmentDeleteRes`）。 */
export interface DocAttachmentDeleteResult {
  id: string;
  file_id: string;
  deleted: boolean;
  /** `sys_file` 是否同时被软删（还有别的单据在用这份文件时为 false） */
  file_deleted: boolean;
}

/**
 * 按单据查附件（`GET /api/business/attachment/list`）。
 *
 * @param sort 排序口径（ATT-03）：`time`（缺省）= 上传时间倒序；`manual` = 手动序号升序。
 *   后端对未知值返回 400（不静默回落），所以这里只传两个合法值。
 */
export function listDocAttachments(
  docType: DocType,
  docId: string,
  sort: DocAttachmentSortMode = 'time',
): Promise<DocAttachmentList> {
  return get<DocAttachmentList>('/api/business/attachment/list', {
    doc_type: docType,
    doc_id: docId,
    sort,
  });
}

/**
 * 手动排序（`PUT /api/business/attachment/sort`，ATT-03）。
 *
 * `ids` 必须是**该单据附件的完整顺序**（第 1 个 → `sort_order = 0`）：
 * 后端会把它与库里「该单据当前未删附件」逐一对上，多一个 / 少一个都 400。
 *
 * 返回值是**重排后的列表**（`sort = manual`），调用方直接替换即可，不必再补一次 GET。
 */
export function sortDocAttachments(
  docType: DocType,
  docId: string,
  ids: string[],
): Promise<DocAttachmentList> {
  return put<DocAttachmentList>('/api/business/attachment/sort', {
    doc_type: docType,
    doc_id: docId,
    ids,
  });
}

/**
 * 上传并绑定附件（`POST /api/business/attachment/upload`，multipart，字段名 `file`）。
 *
 * 走 A 批次的存储策略（分类 `attachment`：100 MB、办公文档 / 图片 / 压缩包白名单）：
 * 扩展名 / MIME / 大小由后端策略判，前端不做二次校验（两处规则必然漂移）。
 */
export async function uploadDocAttachment(
  docType: DocType,
  docId: string,
  file: File,
  fileRole: FileRole = 'OTHER',
  remark?: string,
): Promise<DocAttachmentUploadResult> {
  const form = new FormData();
  form.append('doc_type', docType);
  form.append('doc_id', docId);
  form.append('file_role', fileRole);
  if (remark && remark.trim()) {
    form.append('remark', remark.trim());
  }
  form.append('file', file);
  const response = await service.post<ApiResult<DocAttachmentUploadResult>>(
    '/api/business/attachment/upload',
    form,
  );
  const body = response.data;
  if (!body || typeof body.code !== 'number' || body.code !== 0) {
    throw new ApiError(typeof body?.code === 'number' ? body.code : -1, body?.msg || '上传失败');
  }
  return body.data;
}

/** 软删附件（`DELETE /api/business/attachment/{id}`）。 */
export function deleteDocAttachment(id: string): Promise<DocAttachmentDeleteResult> {
  return del<DocAttachmentDeleteResult>(`/api/business/attachment/${id}`);
}

/**
 * 下载附件（`GET /api/business/attachment/download/{id}`）。
 *
 * **必须走 axios 取 Blob**（不能 `<a href>`）：接口要 Bearer 鉴权，裸链接触发浏览器
 * 导航、不带 `Authorization` → 401。后端在出流前已校验附件归属与权限码，
 * 越权拿 id 只会得到 403 / 404。
 */
export async function downloadDocAttachment(id: string): Promise<Blob> {
  const response = await service.get<Blob>(`/api/business/attachment/download/${id}`, {
    responseType: 'blob',
  });
  return response.data;
}

/**
 * 触发浏览器保存（下载的第二步）。
 *
 * # 参数
 *
 * - `blob`：接口返回的字节；
 * - `fileName`：保存名（后端 `Content-Disposition` 里的原始名）。
 */
export function saveBlob(blob: Blob, fileName: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = fileName || 'attachment';
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

/**
 * 合同页的默认角色（合同最常见的附件就是扫描件）。
 *
 * 其余单据页默认 `OTHER`：收货单要签收单、付款单要发票影像，各页在调用处显式传。
 */
export const DEFAULT_ROLE_BY_DOC: Record<DocType, FileRole> = {
  CONTRACT: 'CONTRACT_SCAN',
  PURCHASE_ORDER: 'OTHER',
  STOCK_IN: 'SIGN_RECEIPT',
  PAYABLE: 'INVOICE',
  PAYMENT: 'INVOICE',
  /** 固定资产卡片附件最常见的是合同/发票影像，但卡片页可自选，默认「其它」与采购单口径一致。 */
  FIXED_ASSET: 'OTHER',
  /** 报销单附件就是发票影像（ARCH-6 §4.7.1）。 */
  EXPENSE_REPORT: 'INVOICE',
};
