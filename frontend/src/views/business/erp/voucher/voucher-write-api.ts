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
 * * `POST /business/erp/voucher`              → `VoucherDetailRes`（手工凭证，落 **DRAFT**，见下方 `createVoucher`）
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
   * `voucher_query.rs::VoucherRes` 的**列表 / 详情 / 红冲回执三处都下发**
   * （P-FLOW-3B 补漏），不审批时是 `null` —— **字段恒在**，所以这里声明成必填的
   * `string | null`：谁把它改回可选，`vue-tsc` 会立刻报「模板可能读到 undefined」。
   */
  pending_reverse_instance_id: string | null;
  /** 红冲原因（提审时填的那段文字；通过后成为红字凭证的分录摘要）。同样是恒在的 `string | null`。 */
  reverse_reason: string | null;
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
  /**
   * 辅助核算：部门 id（`sys_dept.id`）。
   *
   * 科目 `aux_dimensions` 含 `DEPT` 时**后端必填**（E2E04-FIX-1）：5601 / 5602.x / 5603 /
   * 5711 / 2241.01 这些费用类科目要按部门核算，不填这条分录的 `aux_dept_id` 会是 NULL，
   * 部门辅助账（`GET /ledger/aux-balance?aux_type=DEPT`）就查不到它。不要求该维度的科目传了也不报错。
   */
  aux_dept_id?: string | null;
  /** 辅助核算：供应商 id（`biz_supplier.id`）；科目维度含 `SUPPLIER` 时必填 */
  aux_supplier_id?: string | null;
  /** 辅助核算：物料 id（`biz_material.id`）；科目维度含 `MATERIAL` 时必填 */
  aux_material_id?: string | null;
  /** 辅助核算：仓库 id（`biz_warehouse.id`）；科目维度含 `WAREHOUSE` 时必填 */
  aux_warehouse_id?: string | null;
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
 * 手工新增凭证（分录手工录入）。
 *
 * **状态流：落库即 `DRAFT`（草稿），不是 `POSTED`** —— P0-FIX-4A 的双人复核链要求
 * 制单人一个人推不进总账，建完单还要走两步：
 *
 * 1. `POST /business/erp/voucher/{id}/audit` → `AUDITED`（制单人不能审自己的凭证）；
 * 2. `POST /business/erp/voucher/{id}/post`  → `POSTED`（这时才进总账 / 报表）。
 *
 * 只建单、不审核记账 ⇒ 凭证停在草稿，余额与报表都不会变（别照老注释以为建完就入账）。
 *
 * 后端校验：≥ 2 条分录、每条单边、借贷平衡（否则 400 `10001` 并点名差额）；
 * 分录科目必须存在且启用（否则 400 `66002`），声明了辅助核算的科目必须补齐维度；
 * 期间必须 `OPEN` —— 已结账 / 未开启 / 不存在都是 **409 `10003`**（先反结账或先开账）。
 */
export function createVoucher(body: CreateVoucherReq): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher`, body);
}
