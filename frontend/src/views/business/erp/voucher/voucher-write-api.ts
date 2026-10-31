/**
 * 凭证红冲 / 手工新增的 API（P7）。
 *
 * # 为什么不往 `@/api/business/erp/index.ts` 里加
 *
 * 那个文件是全 ERP 共用的契约文件（2230 行，批次 2 里多张卡同时在改它）。
 * 本卡只往 `voucher/` 页面目录加文件，避免在同一段 diff 上打架。
 *
 * 契约逐字对齐后端（`voucher_write.rs`）：
 *
 * * `POST /business/erp/voucher`              → `VoucherDetailRes`（手工凭证，直接 POSTED）
 * * `POST /business/erp/voucher/{id}/reverse` → `VoucherDetailRes`（**红字凭证**的详情，不是原凭证）
 */
import { post } from '@/api/request';
import type { VoucherDetail, VoucherRow } from '@/api/business/erp';

const BASE = '/api/business/erp';

/**
 * 凭证行 + **关联凭证号**（P7 起后端多给两列）。
 *
 * `reverse_of_no`：这张是红字凭证时，它红冲的是哪个蓝字凭证（`null` = 不是红字）。
 * `reversed_by_no`：这张被红冲过时，红冲它的红字凭证号（`null` = 没被红冲）。
 */
export interface VoucherRowEx extends VoucherRow {
  /** 被红冲的原凭证号 */
  reverse_of_no: string | null;
  /** 红冲它的红字凭证号 */
  reversed_by_no: string | null;
}

/** 凭证详情 + 关联凭证号。 */
export type VoucherDetailEx = VoucherDetail & {
  /** 被红冲的原凭证号 */
  reverse_of_no: string | null;
  /** 红冲它的红字凭证号 */
  reversed_by_no: string | null;
};

/** 手工凭证的一条分录。 */
export interface ManualVoucherEntryReq {
  /** 科目 id（`biz_account.id`） */
  account_id: string;
  /** 借方金额（缺省 0） */
  debit?: string;
  /** 贷方金额（缺省 0）—— 与 `debit` 不能同时非零 */
  credit?: string;
  /** 分录摘要（留空继承单头摘要） */
  summary?: string | null;
}

/** 手工新增凭证入参。 */
export interface CreateVoucherReq {
  /** 凭证日期 `YYYY-MM-DD`（null = 今天） */
  voucher_date?: string | null;
  /** 凭证摘要（必填） */
  summary: string;
  /** 分录（≥ 2 条，借贷必须平衡，否则后端 400） */
  entries: ManualVoucherEntryReq[];
  /** 备注 */
  remark?: string | null;
}

/**
 * 红冲一张已记账凭证。
 *
 * 后端校验：只能红冲 `POSTED` 且未红冲、非红字的凭证（否则 409）；期间已结账也 409；`reason` 必填（400）。
 * 返回的是**红字凭证**的详情（`is_red = true`、`reverse_of` 指原凭证）。
 */
export function reverseVoucher(id: string, reason: string): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher/${id}/reverse`, { reason });
}

/**
 * 手工新增凭证（分录手工录入，直接 `POSTED`）。
 *
 * 后端校验：≥ 2 条分录、每条单边、借贷平衡（否则 400 并点名差额）、期间必须 `OPEN`（否则 409）。
 */
export function createVoucher(body: CreateVoucherReq): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher`, body);
}
