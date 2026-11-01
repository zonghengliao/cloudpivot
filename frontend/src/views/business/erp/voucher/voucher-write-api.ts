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
 * 凭证行 + **关联凭证号**（P7 起后端多给两列）+ **红冲审批中标记**（P-FLOW-3B）。
 *
 * `reverse_of_no`：这张是红字凭证时，它红冲的是哪个蓝字凭证（`null` = 不是红字）。
 * `reversed_by_no`：这张被红冲过时，红冲它的红字凭证号（`null` = 没被红冲）。
 * `pending_reverse_instance_id` / `reverse_reason`：**红冲审批中**（`POSTED` 不变，账没动）。
 */
export interface VoucherFlowState {
  /**
   * 红冲审批实例 id（有值 = 正在审批红冲）。
   *
   * ⚠️ 后端契约缺口：`voucher_query.rs::VoucherRes`（列表 + 详情）**目前还没下发**这个字段
   * （`VoucherRow` 与 `biz_voucher` 列都有了，只是 DTO 没带出来）。所以声明成可选：
   * 后端补上之前它是 `undefined`（不是 `null`），前端据此切到
   * `pageFlowInstances` 的兜底路径（见 `voucher/index.vue::loadPendingReverseFallback`）。
   * 补上之后这一层自动失效（字段一出现就以它为准）。
   */
  pending_reverse_instance_id?: string | null;
  /** 红冲原因（提交审批时填的；通过后成为红字凭证的分录摘要）。同上：DTO 暂未下发。 */
  reverse_reason?: string | null;
}

/** 凭证行 + 关联凭证号 + 红冲审批中标记。 */
export interface VoucherRowEx extends VoucherRow, VoucherFlowState {
  /** 被红冲的原凭证号 */
  reverse_of_no: string | null;
  /** 红冲它的红字凭证号 */
  reversed_by_no: string | null;
}

/** 凭证详情 + 关联凭证号 + 红冲审批中标记。 */
export type VoucherDetailEx = VoucherDetail &
  VoucherFlowState & {
    /** 被红冲的原凭证号 */
    reverse_of_no: string | null;
    /** 红冲它的红字凭证号 */
    reversed_by_no: string | null;
  };

/**
 * 提交红冲的出参（`voucher_write.rs::ReverseSubmitRes`）。
 *
 * `#[serde(flatten)] detail` → 凭证字段平铺在顶层。
 *
 * - `outcome = 'PENDING_APPROVAL'`：已送审（凭证**仍是 `POSTED`**，只挂了 pending，账没动）；
 * - `outcome = 'REVERSED'`：未配流程 → 直接红冲（detail 是**红字凭证**）。
 */
export interface ReverseSubmitResult extends VoucherDetailEx {
  /** `PENDING_APPROVAL` / `REVERSED` */
  outcome: string;
  /** 后端给的一句话（前端按 `outcome` 出文案，未知取值时兜底用它） */
  message: string;
  /** 审批实例 id（未配流程 / 直接红冲时为 null） */
  approval_instance_id: string | null;
}

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
 * 提交红冲（**P-FLOW-3B 起走审批**，唯一入口）。
 *
 * # 为什么打老 URL `/reverse`，而不是 `/submit-reverse`
 *
 * 两个 URL 在 Rust 侧是同一个函数（`voucher_write::submit_reverse`），出参同为
 * `ReverseSubmitRes`；老 URL 绕不过审批。权限码差别是关键：
 * `/submit-reverse` 要 `business:erp:voucher:submit-reverse`，V0191 只授给 `SUPER_ADMIN`；
 * 老 URL 的 `business:erp:voucher:reverse` 才是 `ADMIN / FINANCE / ACCOUNTANT` 都有的（V0158）。
 * 沿用老 URL 才不会让财务角色点「红冲」直接 403（权限矩阵对齐后换 URL 只是一行）。
 *
 * 后端校验：只能红冲 `POSTED` 且未红冲、非红字、**不在红冲审批中**的凭证（否则 409）；
 * 期间已结账 409；`reason` 必填（400）。
 */
export function submitReverseVoucher(id: string, reason: string): Promise<ReverseSubmitResult> {
  return post<ReverseSubmitResult>(`${BASE}/voucher/${id}/reverse`, { reason });
}

/**
 * 撤回在跑的红冲审批（发起人自己撤 → 只清 `pending_reverse_instance_id`，凭证一直 `POSTED`）。
 *
 * 后端：`POST /business/erp/voucher/{id}/withdraw-reverse` → `VoucherDetailRes`（无 `outcome`）。
 * 不在红冲审批中 → 409；没权限（`business:erp:voucher:withdraw-reverse`）→ 403。
 */
export function withdrawReverseVoucher(id: string, reason?: string | null): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher/${id}/withdraw-reverse`, reason ? { reason } : {});
}

/**
 * 手工新增凭证（分录手工录入，直接 `POSTED`）。
 *
 * 后端校验：≥ 2 条分录、每条单边、借贷平衡（否则 400 并点名差额）、期间必须 `OPEN`（否则 409）。
 */
export function createVoucher(body: CreateVoucherReq): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher`, body);
}
