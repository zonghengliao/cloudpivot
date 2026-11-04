/**
 * 凭证「审核 / 反审核 / 记账」三个写接口（P0-FIX-4B，配合后端 P0-FIX-4A 三态）。
 *
 * # 为什么不写进 `@/api/business/erp/index.ts`
 *
 * 那个文件是全 ERP 共用的契约文件（3000+ 行，多张卡同时在改）。本卡只在
 * `voucher/` 页面目录加文件，避免在同一段 diff 上打架 —— 与
 * `voucher-write-api.ts`（P7 / P-FLOW-3B）同一个理由、同一个写法。
 *
 * # 契约（前端先落，后端 4A 按此实现）
 *
 * ```text
 * POST /api/business/erp/voucher/{id}/audit       body {remark?}   DRAFT   → AUDITED
 * POST /api/business/erp/voucher/{id}/un-audit    body {reason}    AUDITED → DRAFT   （reason 必填）
 * POST /api/business/erp/voucher/{id}/post        body {}          AUDITED → POSTED
 * ```
 *
 * 三条都回 `VoucherDetailRes`（凭证详情，含全部分录），前端直接拿它刷新抽屉，
 * 不再多发一次 `GET /voucher/{id}`。
 *
 * 权限码（与后端 `#[alun::permission]` 一一对应，V0xxx 迁移授权）：
 * `business:erp:voucher:audit` / `business:erp:voucher:un-audit` / `business:erp:voucher:post`。
 *
 * 后端负责的闸（前端**只做提前置灰，不重复校验**）：
 * - 非法流转 → `409`（例如 POSTED 反审核、DRAFT 记账）；
 * - 制单人自审 → `403`（内控：制单 ≠ 复核）；
 * - 反审核没原因 → `400`；
 * - 期间已结账 → `409`。
 *
 * ⚠️ 后端 4A（DRAFT/AUDITED 两态 + 这三个接口）**尚未合并进 main** 时，
 * 列表里不会出现 DRAFT/AUDITED 的凭证 → 三个按钮一个都不渲染（这是正确的降级：
 * 后端没有的能力不摆假入口）。契约若在 4A 落地时改了路径，只改本文件三行 `post(...)`。
 */
import { post } from '@/api/request';
import type { VoucherDetailEx } from './voucher-write-api';

const BASE = '/api/business/erp';

/**
 * 审核（`DRAFT → AUDITED`，PRD E4-09）。
 *
 * 与「红冲审批」不是一件事：这是凭证自身的**双人复核**闸，不走流程引擎，
 * 所以不给审批实例 id、也不产生待办。
 *
 * @param remark 审核意见（可空；后端会写进凭证状态日志）
 */
export function auditVoucher(id: string, remark?: string | null): Promise<VoucherDetailEx> {
  const body = remark && remark.trim() ? { remark: remark.trim() } : {};
  return post<VoucherDetailEx>(`${BASE}/voucher/${id}/audit`, body);
}

/**
 * 反审核（`AUDITED → DRAFT`，PRD E4-09）：**原因必填**。
 *
 * 原因会写进凭证状态日志（审计要看「为什么把已审核打回草稿」），
 * 后端对空原因回 `400`，前端在弹窗里先拦一道（见 `voucher-audit-logic.ts::unAuditReasonError`）。
 */
export function unAuditVoucher(id: string, reason: string): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher/${id}/un-audit`, { reason: reason.trim() });
}

/**
 * 记账（`AUDITED → POSTED`，PRD E4-10）：记账后凭证进总账 / 三大报表，**不可再反审核**。
 *
 * 更正只能红冲（新开红字凭证），所以前端在点之前有一次「不可撤销」的二次确认。
 */
export function postVoucher(id: string): Promise<VoucherDetailEx> {
  return post<VoucherDetailEx>(`${BASE}/voucher/${id}/post`, {});
}
