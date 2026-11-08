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

/**
 * 文件角色（与 DDL 的 `ck_biz_doc_attachment_role` 逐字一致）。
 *
 * ATT-04 起分 6 个业务角色 + 1 个兼容值：
 *
 * | 值 | 中文 | 说明 |
 * |----|------|------|
 * | `CONTRACT_SCAN` | 合同扫描件 | |
 * | `SUPPLEMENT` | 补充协议 | |
 * | `ACCEPTANCE` | 验收单 | |
 * | `INVOICE_SCAN` | 发票影像 | |
 * | `BANK_RECEIPT` | 银行回单 | |
 * | `DEFAULT` | 其他附件 | 缺省值（不传 role 的文件进这组） |
 * | `SIGN_RECEIPT` | 签收单 | **兼容值**：老数据仍在用，不出现在上传下拉里 |
 *
 * 老拼写 `INVOICE` / `OTHER` 后端会在解析时归一（`INVOICE`→`INVOICE_SCAN`、
 * `OTHER`→`DEFAULT`），前端不再使用。
 */
export type FileRole =
  | 'CONTRACT_SCAN'
  | 'SUPPLEMENT'
  | 'ACCEPTANCE'
  | 'INVOICE_SCAN'
  | 'BANK_RECEIPT'
  | 'SIGN_RECEIPT'
  | 'DEFAULT';

/** 单据类型中文名（错误提示 / 空态文案用；取自词表，不另写一份）。 */
export const DOC_TYPE_LABELS: Record<DocType, string> = {
  CONTRACT: DOC_KIND_LABELS.CONTRACT,
  PURCHASE_ORDER: DOC_KIND_LABELS.PURCHASE_ORDER,
  STOCK_IN: DOC_KIND_LABELS.STOCK_IN,
  PAYABLE: DOC_KIND_LABELS.PAYABLE,
  PAYMENT: DOC_KIND_LABELS.PAYMENT,
  FIXED_ASSET: DOC_KIND_LABELS.FIXED_ASSET,
  EXPENSE_REPORT: DOC_KIND_LABELS.EXPENSE_REPORT,
  INVOICE: DOC_KIND_LABELS.INVOICE,
  RECEIPT: DOC_KIND_LABELS.RECEIPT,
  STOCK_TAKE: DOC_KIND_LABELS.STOCK_TAKE,
  PURCHASE_RETURN: DOC_KIND_LABELS.PURCHASE_RETURN,
  SALES_RETURN: DOC_KIND_LABELS.SALES_RETURN,
};

/**
 * 文件角色下拉项（ATT-04 的 6 个分栏角色，顺序 = 分组顺序）。
 *
 * ⚠ 这份表与后端 `FileRole::GROUPS` 由 Rust 单测 `file_roles_match_frontend_vocabulary`
 * 逐条比对（code + 中文名 + 顺序）。加角色必须两边一起，否则单测直接红 ——
 * 不是「最好同步」，是**必须同步**。
 *
 * 分组标题请用接口下发的 `roles[].label`（后端唯一真相），这份表只用于「上传到哪个角色」
 * 的下拉；兼容值 `SIGN_RECEIPT` 不在下拉里（老数据仍会显示成自己的分组）。
 */
export const FILE_ROLE_OPTIONS: { value: FileRole; label: string }[] = [
  { value: 'CONTRACT_SCAN', label: '合同扫描件' },
  { value: 'SUPPLEMENT', label: '补充协议' },
  { value: 'ACCEPTANCE', label: '验收单' },
  { value: 'INVOICE_SCAN', label: '发票影像' },
  { value: 'BANK_RECEIPT', label: '银行回单' },
  { value: 'DEFAULT', label: '其他附件' },
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

/** 一个角色分组（对应后端 `DocAttachmentRoleRes`，ATT-04）。 */
export interface DocAttachmentRoleGroup {
  /** 角色字面值（`CONTRACT_SCAN` / `SUPPLEMENT` / …） */
  code: string;
  /** 中文分组标题（后端 `FileRole::label`，前端不再维护第二份） */
  label: string;
  /** 该角色下的附件条数（0 = 空分组，前端显示「暂无 XX，点击上传」） */
  count: number;
}

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
  /**
   * 分组口径（ATT-04）：固定 6 个角色（**空分组也在**）+ 数据里存在的兼容角色。
   *
   * 前端按这个数组的顺序渲染分组 —— 空分组只能从这里知道，遍历 `list` 是看不到的。
   * 后端没给（老实例）时组件回落本地 `FILE_ROLE_OPTIONS`，见 `resolveRoleCatalog`。
   */
  roles: DocAttachmentRoleGroup[];
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
 * # 契约
 *
 * - **单文件 ≤ 20 MB**（ATT-05）：后端 `dto::MAX_FILE_BYTES` 硬拦，超限 → **400** +
 *   「文件超过 20MB，请压缩或分卷上传」；前端在发请求前先用
 *   `components/attachment-upload-pure.ts` 的 `partitionFilesBySize` 拦一遍（超限的一个都不发）。
 *   两处同一个数字（`MAX_ATTACHMENT_FILE_BYTES`），漂了 cargo test 直接红。
 * - 扩展名 / MIME / 秒传仍由 storage 的 `attachment` 策略判（后端唯一判据，前端不重复实现）。
 *
 * # 为什么要 `service.post` 再手工解 `code`
 *
 * 上传是 multipart，统一响应包装要自己拆（`ApiResult.code !== 0` 时抛 `ApiError`，
 * 这样调用方的 `catch` 能拿到后端文案 —— 包括上面那句 20 MB 的提示）。
 */
export async function uploadDocAttachment(
  docType: DocType,
  docId: string,
  file: File,
  fileRole: FileRole = 'DEFAULT',
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
 * 附件必传校验的触发动作（对应后端 `attachment::rule::RequiredAt`）。
 *
 * 与 `biz_attachment_rule.required_at` 的 CHECK 逐字一致（`SUBMIT` / `CONFIRM` / `ACTIVATE`）。
 */
export type AttachmentRequiredAt = 'SUBMIT' | 'CONFIRM' | 'ACTIVATE';

/** 一条缺口（对应后端 `AttachmentCheckMissingRes`，ATT-08）。 */
export interface AttachmentCheckMissing {
  /** 规则主键（运维按它改 `biz_attachment_rule`） */
  rule_id: string;
  /** 规则指定的文件角色（`null` = 任意角色都算） */
  role: string | null;
  /** 需求的人话名（`发票影像` / `银行回单` …），红字直接用 */
  role_label: string;
  /** 要求的扩展名（`pdf/ofd/xml`）；`null` = 不限格式 */
  need: string | null;
  /** 命中白名单的附件数 */
  have: number;
  /** 至少要几个 */
  min: number;
  /** 该规则范围内的附件总数（不分格式） */
  total: number;
  /** 还差几个 */
  shortfall: number;
  /** 现有附件的扩展名（都是白名单外的） */
  actual_exts: string[];
  /** **按钮旁的一行红字**（后端拼，前端原样显示） */
  hint: string;
  /** 完整人话（与后端 400 报文逐字一致，tooltip 用） */
  message: string;
}

/** 附件必传校验结果（对应后端 `AttachmentCheckRes`，ATT-08）。 */
export interface AttachmentCheckResult {
  doc_type: string;
  doc_id: string;
  at: AttachmentRequiredAt;
  /** 动作中文（`提交` / `确认` / `生效`） */
  at_label: string;
  /** 能不能放行：`false` → 按钮 disabled + 红字 */
  can_proceed: boolean;
  missing: AttachmentCheckMissing[];
  /** 合并后的红字（多条用「；」连接） */
  hint: string | null;
  /** 合并后的完整报文（多条用换行；tooltip 用） */
  message: string | null;
}

/**
 * 查「本单据在本动作下缺什么附件」（`GET /api/business/attachment/check`，ATT-08）。
 *
 * 前端用它把按钮状态交给后端一条判据决定（见 `views/business/erp/shared/attachment-gate-pure.ts`），
 * 不再自己算扩展名 / 数量 —— 算一遍就有第二套判据，两处必然漂。
 *
 * # 异常情况（调用方必须区分）
 *
 * - 403 / 404 / 400：**单据不存在 / 无权看 / 参数非法** —— 调用方别拿它当依据，
 *   也不要禁用按钮（后端真正拦）；
 * - 500：规则表读不出来 —— 同样**不禁用按钮**（一次数据库抖动不该锁死所有人）。
 *
 * 所以本函数**不吞异常**：抛出去，让 `useAttachmentGate` 走「降级放行」分支。
 */
export function checkDocAttachments(
  docType: DocType,
  docId: string,
  at: AttachmentRequiredAt = 'SUBMIT',
): Promise<AttachmentCheckResult> {
  return get<AttachmentCheckResult>('/api/business/attachment/check', {
    doc_type: docType,
    doc_id: docId,
    at,
  });
}

/**
 * 各单据的默认上传角色（ATT-04 词表）。
 *
 * 合同 → 合同扫描件；收货 → 验收单；应付 / 付款 / 报销 → 发票影像；采购 → 其他附件。
 * （收货页原先是 `SIGN_RECEIPT`（签收单）：ATT-04 的分栏词表里没有签收单，
 * 收货验收归到「验收单」；老数据里的 `SIGN_RECEIPT` 仍会显示成独立分组。）
 */
export const DEFAULT_ROLE_BY_DOC: Record<DocType, FileRole> = {
  CONTRACT: 'CONTRACT_SCAN',
  PURCHASE_ORDER: 'DEFAULT',
  STOCK_IN: 'ACCEPTANCE',
  PAYABLE: 'INVOICE_SCAN',
  PAYMENT: 'INVOICE_SCAN',
  /** 固定资产卡片附件最常见的是合同/发票影像，但卡片页可自选，默认「其他附件」与采购单口径一致。 */
  FIXED_ASSET: 'DEFAULT',
  /** 报销单附件就是发票影像（ARCH-6 §4.7.1）。 */
  EXPENSE_REPORT: 'INVOICE_SCAN',
  /** 发票影像（ATT-06 必传规则：电子票 pdf/ofd/xml、纸质票 jpg/png/pdf）。 */
  INVOICE: 'INVOICE_SCAN',
  /** 收款单必传银行回单（ATT-06）。 */
  RECEIPT: 'BANK_RECEIPT',
  /** 盘点表 / 盈亏说明：归「其他附件」（分栏词表里没有盘点专用角色）。 */
  STOCK_TAKE: 'DEFAULT',
  /** 采购退货：退货单 / 签收凭证 → 其他附件。 */
  PURCHASE_RETURN: 'DEFAULT',
  /** 销售退货：退货单 / 物流凭证 → 其他附件。 */
  SALES_RETURN: 'DEFAULT',
};
