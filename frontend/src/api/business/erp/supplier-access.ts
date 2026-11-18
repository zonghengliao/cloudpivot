/**
 * 供应商准入 / 黑名单 API 层（后端 `backend/app/src/business/erp/purchase/supplier_access*`）。
 *
 * # 接口与权限码（M4-14 · `V0455` 的 4 条按钮行 …4485–…4488）
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET    | `/page`      | `business:erp:supplier-access:list`（列表 + 详情共用一个码） |
 * | GET    | `/{id}`      | 同上 |
 * | POST   | ``           | `business:erp:supplier-access:create` |
 * | PUT    | `/{id}`      | `business:erp:supplier-access:update`（改草稿） |
 * | DELETE | `/{id}`      | 同上（删草稿，**一个码管改与删** —— 同一批人的同一类动作） |
 * | POST   | `/{id}/submit` | `business:erp:supplier-access:submit` |
 *
 * 页面显形码 `business:erp:supplier-access:page` **只在路由 `meta.permission`**，
 * 后端零引用（AGENTS 约定 14），本文件也不拿它判任何东西。
 *
 * # 三条渲染口径（都来自后端 DTO 注释，别在页面里重新猜）
 *
 * 1. **提交与通过都不改主数据**：状态 `APPROVED` 才落 `biz_supplier.status`
 *    （拉黑 → 停用 0，准入 / 解除 → 启用 1）。行里的 `target_supplier_status`
 *    就是「通过后会变成什么」，前端不自己按类型推。
 * 2. **`editable` / `submittable` 由后端给**（草稿可改可删；草稿 + 理由 ≥5 字才可提交），
 *    页面不写第二套状态机 —— 越权 / 越态请求后端 409，报文原样弹给用户。
 * 3. **等级快照只对黑名单有意义**（`ck_biz_sa_grade_only_blacklist`）：准入 / 解除单
 *    传 `grade_snapshot` 会被 400，所以表单只在 `BLACKLIST` 时展示那一格。
 *
 * 前缀 `/api` 必须写全（后端 `#[nik::get]` 注解不含 `/api`，漏了会命中 SPA fallback 返回 HTML）。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（与 `framework-agreement.ts` / `index.ts` 同一常量口径） */
const BASE = '/api/business/erp/supplier-access';

/** 申请类型闭集（与 `supplier_access_model::ACCESS_TYPES` / `ck_biz_sa_type` 逐字一致） */
export type AccessType = 'ADMIT' | 'BLACKLIST' | 'RELEASE';

/** 状态闭集（与 `ACCESS_STATUSES` / `ck_biz_sa_status` 逐字一致：**只有三个**，没有 REJECTED） */
export type AccessStatus = 'DRAFT' | 'SUBMITTED' | 'APPROVED';

/** 评估等级快照闭集（只对黑名单有意义） */
export type AccessGrade = 'A' | 'B' | 'C' | 'D';

/**
 * 申请单一行（= 后端 `AccessRowRes`，列表与详情**同一个结构**）。
 *
 * 列表行与详情行同构：后端两个接口都返回它，页面不为详情另建窄类型（会漏字段、渲染时再猜一次）。
 */
export interface AccessRow {
  /** 主键 */
  id: string;
  /** 单号 `SAC-yyyyMMdd-NNNNNN` */
  access_no: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商编码**快照**（主档改码后仍能对出当时是谁） */
  supplier_code: string;
  /** 供应商名**快照**（审计 / 历史类，约定 13） */
  supplier_name: string;
  /** 申请类型 */
  access_type: AccessType;
  /** 类型中文（准入 / 黑名单 / 解除黑名单） */
  access_type_label: string;
  /** 状态 */
  status: AccessStatus;
  /** 状态中文（拟稿 / 审批中 / 已生效） */
  status_label: string;
  /** 申请理由（提交时必填 ≥5 字；草稿可空） */
  reason: string | null;
  /** 评估等级快照（**只有黑名单单**有） */
  grade_snapshot: AccessGrade | null;
  /** 申请人 id */
  apply_by: string | null;
  /** 申请人姓名快照 */
  apply_by_name: string | null;
  /** 审批实例 id（提交时写入；草稿为 null） */
  approval_instance_id: string | null;
  /** 结论人 id */
  decided_by: string | null;
  /** 结论人姓名快照 */
  decided_by_name: string | null;
  /** 结论时间（RFC3339；未结论为 null） */
  decided_at: string | null;
  /** 备注（驳回意见 / 补充说明） */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 供应商**现值**状态（1 启用 / 0 停用） */
  supplier_status: number;
  /** 供应商现值状态中文（启用 / 停用） */
  supplier_status_label: string;
  /** 通过后会把供应商置成什么状态（1 启用 / 0 停用）—— 「生效后」预览用 */
  target_supplier_status: number;
  /** 草稿可改可删（**按钮显隐读它**，不自己写状态机；真正的闸在服务端） */
  editable: boolean;
  /** 可提交（草稿 + 理由够字） */
  submittable: boolean;
}

/**
 * 分页入参（后端 `AccessPageQuery`）。
 *
 * ⚠ 用 `type` 而不是 `interface`：`request()` 的 `params` 是 `Record<string, unknown>`，
 * 类型别名才有隐式索引签名（interface 赋不过去，会逼着页面到处写 as）。
 */
export type AccessQuery = {
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（后端 clamp 到 1–200） */
  page_size?: number;
  /** 关键字：单号 / 供应商编码 / 供应商名 / 理由（后端上限 64 字符） */
  keyword?: string;
  /** 状态（闭集外的值后端 400，不静默忽略） */
  status?: AccessStatus | '';
  /** 类型（同上） */
  access_type?: AccessType | '';
  /** 供应商 id（uuid 字符串；非法值后端 400） */
  supplier_id?: string;
};

/** 建单入参（后端 `CreateAccessReq`：供应商 + 类型必填，理由草稿可空） */
export type CreateAccessPayload = {
  /** 供应商 id（必填 uuid） */
  supplier_id: string;
  /** 类型（ADMIT / BLACKLIST / RELEASE） */
  access_type: AccessType;
  /** 申请理由（草稿可空；提交时 ≥5 字） */
  reason?: string | null;
  /** 评估等级快照（只有黑名单可填 A/B/C/D） */
  grade_snapshot?: AccessGrade | null;
  /** 备注 */
  remark?: string | null;
};

/**
 * 改草稿入参（后端 `UpdateAccessReq`：**类型与供应商不可改**）。
 *
 * 后端是「给了就覆盖」语义：`null` / 缺省 = 保持原值，空串 = 清空（`clean_reason` 会把
 * 空串归一成 `None`）。
 */
export type UpdateAccessPayload = {
  /** 申请理由 */
  reason?: string | null;
  /** 评估等级快照（准入 / 解除单传了会 400） */
  grade_snapshot?: AccessGrade | null;
  /** 备注 */
  remark?: string | null;
};

/**
 * 提交审批入参（后端 `SubmitAccessReq`）。
 *
 * * `reason` 给了就**覆盖**草稿里的理由；两者都空 → 400（提交必须有 ≥5 字理由）。
 * * `skip_approval` / `skip_reason` 是 `SkipApprovalReq`（`#[serde(flatten)]`）：
 *   **只对「没配流程」生效**，且必须持 `business:erp:skip-approval` + 理由 ≥10 字；
 *   流程已发布时请求跳过 → 400（不许绕过已配置的内控）。
 */
export type SubmitAccessPayload = {
  /** 申请理由（覆盖草稿） */
  reason?: string;
  /** 显式跳过审批（未配置流程时；需 skip-approval 权限） */
  skip_approval?: boolean;
  /** 跳过审批的理由（≥10 字） */
  skip_reason?: string;
};

/** 分页（`GET /supplier-access/page`，权限码 `…:list`） */
export function fetchAccessPage(query: AccessQuery): Promise<PageData<AccessRow>> {
  return get<PageData<AccessRow>>(`${BASE}/page`, query);
}

/** 详情（`GET /supplier-access/{id}`，权限码 `…:list`；不存在 / 已软删 → 404） */
export function fetchAccessDetail(id: string): Promise<AccessRow> {
  return get<AccessRow>(`${BASE}/${id}`);
}

/** 新建申请单（`POST /supplier-access`，权限码 `…:create`；单号 `SAC-yyyyMMdd-NNNNNN`） */
export function createAccess(payload: CreateAccessPayload): Promise<AccessRow> {
  return post<AccessRow>(BASE, payload);
}

/** 改草稿（`PUT /supplier-access/{id}`，权限码 `…:update`；只有 DRAFT 能改） */
export function updateAccess(id: string, payload: UpdateAccessPayload): Promise<AccessRow> {
  return put<AccessRow>(`${BASE}/${id}`, payload);
}

/** 软删草稿（`DELETE /supplier-access/{id}`，权限码 `…:update`；只有 DRAFT 能删） */
export function deleteAccess(id: string): Promise<void> {
  return del<void>(`${BASE}/${id}`);
}

/**
 * 提交审批（`POST /supplier-access/{id}/submit`，权限码 `…:submit`）。
 *
 * 走 `supplier_access` 流程（采购经理 → 黑名单再加财务经理复核）；没有已发布流程时
 * 后端默认 409 并点名 `supplier_access`（只有持 `business:erp:skip-approval` + 理由 ≥10 字才放行）。
 */
export function submitAccess(id: string, body: SubmitAccessPayload = {}): Promise<AccessRow> {
  return post<AccessRow>(`${BASE}/${id}/submit`, body);
}
