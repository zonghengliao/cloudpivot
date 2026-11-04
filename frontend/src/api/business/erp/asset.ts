/**
 * 固定资产 API 层（后端 `backend/src/business/erp/asset_{dto,handler,model,compute}.rs`）。
 *
 * # 为什么单独成文件
 *
 * 与 `stock-take.ts` 同款理由：`erp/index.ts` 已经是几千行的仓储域总契约，而固定资产是
 * 独立后端模块（自己的 handler / DTO / 迁移 `V0229__erp_asset_tables`）。按模块边界单独成文件，
 * 页面只 `import '@/api/business/erp/asset'`，不会和正在改 `index.ts` 的人互相踩。
 *
 * 词表（状态 / 变动 / 处置 / 购入口径 …）在 `./asset-kinds`，那里零 import，
 * 纯逻辑层可以放心引用而不会把 axios 带进 node 单测。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 资产编号 | `asset_no`（`FA-yyyyMMdd-000001`） | `asset_dto.rs::AssetRes::asset_no` |
 * | 状态 | `status` + `status_label`（在役 / 闲置 / 已处置 / 已作废） | 后端给中文，前端不维护第二份译名 |
 * | 月折旧额 | `monthly_depreciation` | 建卡时由服务端按分类参数算好，**前端只做预览** |
 * | 可折旧总额 | `depreciable_base` = 原值 − 预计净残值 | DDL `ck_biz_fa_base` 钉死 |
 * | 净值 | `net_value` = 原值 − 累计折旧 | DDL `ck_biz_fa_net` 钉死 |
 * | 剩余可提 | 详情顶层 `months_left` / `amount_left` | `AssetDetailRes` 直接下发，前端不自己算 |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **金额一律字符串**（numeric 四位小数，如 `"2638.8889"`）。类型上就声明成 `string`，
 *    不要写成 `number` —— 后端序列化 `Decimal` 就是字符串，`Number()` 之后做等值判断
 *    会在 `0.1 + 0.2` 这类地方翻车（页面上用 `asset-logic.ts` 的四位定点工具比较）；
 * 3. **详情是包了一层的**：`AssetDetailRes { asset, changes, depreciations, next_statuses,
 *    pending_disposal, months_left, amount_left }`，卡片字段在 `detail.asset` 里
 *    （与盘点单的扁平 `#[serde(flatten)]` **不同**，不要照抄别的页面）。
 *
 * # 校验口径（镜像后端的 400 文案，页面上先拦一道）
 *
 * - 建卡：名称 / 分类 / 使用部门 / 原值 > 0 / 购入日期必填；可抵扣进项税 < 原值；
 *   `FROM_STOCK` 不得带进项税；`MANUAL` 不能带来源单；
 * - 改卡：只能改名称 / 规格 / 地点 / 备注（金额与折旧口径不许改，K2 不变量）；
 * - 转移 / 换使用人 / 处置 / 作废处置：原因必填（留痕要能回答「为什么变」）；
 * - 处置：`SALE` 必须有售价，`SCRAP` / `LOSS` 的售价与销项税必须为 0。
 */

import { del, get, post, put, type PageData } from '@/api/request';
import type { DepDetail } from '@/api/business/erp/depreciation';
import type {
  AssetSourceType,
  AssetStatus,
  BuyMode,
  ChangeType,
  DisposalType,
  PayMode,
} from './asset-kinds';

// 词表从「零依赖」的 asset-kinds 转出：页面继续从本文件取，逻辑层直接取 kinds。
export {
  ASSET_SOURCE_LABELS,
  ASSET_SOURCE_TYPES,
  ASSET_STATUSES,
  ASSET_STATUS_LABELS,
  BUY_MODES,
  CHANGE_TYPES,
  CHANGE_TYPE_LABELS,
  DEPRECIATION_METHODS,
  DEPRECIATION_METHOD_LABELS,
  DISPOSAL_TYPES,
  DISPOSAL_TYPE_LABELS,
  PAY_MODES,
  PAY_MODE_LABELS,
} from './asset-kinds';

export type {
  AssetSourceType,
  AssetStatus,
  BuyMode,
  ChangeType,
  DepreciationMethod,
  DisposalType,
  PayMode,
} from './asset-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
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

// ==================== 资产分类 ====================

/** 资产分类（逐字抄 `CategoryRes`）。 */
export interface AssetCategory {
  /** 主键 */
  id: string;
  /** 分类编码（大写字母 / 数字 / 下划线，如 `ELEC`） */
  category_code: string;
  /** 分类名称（如「电子设备」） */
  category_name: string;
  /** 折旧方法（本期恒为 `STRAIGHT_LINE`） */
  depreciation_method: string;
  /** 使用月数（1–600） */
  useful_life_months: number;
  /** 净残值率（0 ≤ r < 1，字符串四位小数如 `"0.0500"`） */
  salvage_rate: string;
  /** 原值科目 id（默认 1601） */
  asset_account_id: string;
  /** 累计折旧科目 id（默认 1602） */
  accum_account_id: string;
  /** 折旧费用科目 id（默认 5602.01） */
  expense_account_id: string;
  /** 状态：1 启用 / 0 停用 */
  status: number;
  /** 排序 */
  sort_order: number;
  /** 备注 */
  remark: string | null;
}

/** 分类分页查询。 */
export interface AssetCategoryQuery {
  /** 页码（从 1 开始） */
  page?: number;
  /** 每页条数（后端上限 100） */
  page_size?: number;
  /** 编码 / 名称模糊 */
  keyword?: string;
  /** 状态：1 启用 / 0 停用 */
  status?: number | '';
}

/** 新建 / 修改分类的入参（后端 DTO 全是 `Option`，缺什么由后端指名回 400）。 */
export interface AssetCategoryPayload {
  /** 编码 */
  category_code?: string;
  /** 名称 */
  category_name?: string;
  /** 使用月数 */
  useful_life_months?: number;
  /** 净残值率（字符串或数字都行，后端收 `Decimal`） */
  salvage_rate?: string | number;
  /** 原值科目 */
  asset_account_id?: string;
  /** 累计折旧科目 */
  accum_account_id?: string;
  /** 折旧费用科目 */
  expense_account_id?: string;
  /** 状态 */
  status?: number;
  /** 排序 */
  sort_order?: number;
  /** 备注 */
  remark?: string;
}

/** 启用分类（建卡下拉；`GET /asset-category/list` 不分页、只回启用的） */
export function fetchAssetCategoryList(): Promise<AssetCategory[]> {
  return get<AssetCategory[]>(`${BASE}/asset-category/list`);
}

/** 分类分页（`GET /asset-category/page`） */
export function fetchAssetCategoryPage(query: AssetCategoryQuery): Promise<PageData<AssetCategory>> {
  return get<PageData<AssetCategory>>(`${BASE}/asset-category/page`, asParams(query));
}

/** 新建分类（`POST /asset-category`，回整条记录） */
export function createAssetCategory(payload: AssetCategoryPayload): Promise<AssetCategory> {
  return post<AssetCategory>(`${BASE}/asset-category`, payload);
}

/** 修改分类（`PUT /asset-category/{id}`，回整条记录） */
export function updateAssetCategory(
  id: string,
  payload: AssetCategoryPayload,
): Promise<AssetCategory> {
  return put<AssetCategory>(`${BASE}/asset-category/${id}`, payload);
}

/** 删除分类（软删；分类下有卡片时后端 409） */
export function deleteAssetCategory(id: string): Promise<unknown> {
  return del<unknown>(`${BASE}/asset-category/${id}`);
}

// ==================== 资产卡片 ====================

/** 资产卡片（逐字抄 `AssetRes`；金额全是字符串）。 */
export interface AssetRow {
  /** 主键 */
  id: string;
  /** 资产编号 `FA-yyyyMMdd-000001` */
  asset_no: string;
  /** 资产名称 */
  asset_name: string;
  /** 分类 id */
  category_id: string;
  /** 分类编码（快照） */
  category_code: string;
  /** 规格型号 */
  spec: string | null;
  /** 使用部门 id */
  use_dept_id: string;
  /** 使用部门名称（快照） */
  use_dept_name: string;
  /** 使用人 id */
  custodian_id: string | null;
  /** 使用人姓名 */
  custodian_name: string | null;
  /** 存放地点 */
  location: string | null;
  /** 原值 */
  original_value: string;
  /** 可抵扣进项税 */
  tax_amount: string;
  /** 预计净残值 */
  residual_value: string;
  /** 可折旧总额 */
  depreciable_base: string;
  /** 月折旧额（服务端按分类参数算好的缓存列） */
  monthly_depreciation: string;
  /** 开始计提日（购入次月 1 日） */
  start_depreciation_date: string;
  /** 累计折旧 */
  accumulated_depreciation: string;
  /** 净值 */
  net_value: string;
  /** 已提月数 */
  depreciation_months_done: number;
  /** 使用月数 */
  useful_life_months: number;
  /** 最后计提期间 `yyyy-MM`（从未提过 = `null`） */
  last_depreciation_period: string | null;
  /** 购入日期 */
  purchase_date: string;
  /** 来源类型 */
  source_type: AssetSourceType | string;
  /** 来源单号（快照） */
  source_no: string | null;
  /** 购入口径 */
  buy_mode: BuyMode | string | null;
  /** 供应商名称（快照） */
  supplier_name: string | null;
  /** 购入凭证 id */
  voucher_id: string | null;
  /** 状态 */
  status: AssetStatus | string;
  /** 状态中文（后端给） */
  status_label: string;
  /** 处置类型 */
  disposal_type: DisposalType | string | null;
  /** 处置日期 */
  disposal_date: string | null;
  /** 处置凭证 id */
  disposal_voucher_id: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间（ISO8601） */
  created_at: string;
}

/** 变动记录（逐字抄 `ChangeRes`）。 */
export interface AssetChange {
  /** 主键 */
  id: string;
  /** 变动类型 */
  change_type: ChangeType | string;
  /** 变动类型中文（后端给） */
  change_label: string;
  /** 变动日期 `YYYY-MM-DD` */
  change_date: string;
  /** 原部门名 */
  from_dept_name: string | null;
  /** 新部门名 */
  to_dept_name: string | null;
  /** 原使用人 */
  from_user_name: string | null;
  /** 新使用人 */
  to_user_name: string | null;
  /** 原状态 */
  from_status: string | null;
  /** 新状态 */
  to_status: string | null;
  /** 原因（必填，内控留痕） */
  reason: string;
  /** 关联凭证 */
  voucher_id: string | null;
  /** 审批实例（处置） */
  approval_instance_id: string | null;
  /** 操作人 */
  operator_name: string | null;
  /** 备注 */
  remark: string | null;
  /** 时间 */
  created_at: string;
}

/**
 * 卡片详情（逐字抄 `AssetDetailRes`）。
 *
 * ⚠️ 卡片字段在 `asset` 里包了一层（不是扁平展开）。
 */
export interface AssetDetail {
  /** 卡片 */
  asset: AssetRow;
  /** 变动时间线（倒序） */
  changes: AssetChange[];
  /** 折旧明细（倒序；与折旧批次明细同构） */
  depreciations: DepDetail[];
  /** 可去的下一个状态（前端不写第二套状态机） */
  next_statuses: string[];
  /** 在跑的处置审批实例（`null` = 没有） */
  pending_disposal: string | null;
  /** 剩余可提月数（处置决策要看「还能提多少」） */
  months_left: number;
  /** 剩余可提金额 */
  amount_left: string;
}

/** 建卡 / 变更接口回的主键与单号（`IdRes`）。 */
export interface AssetIdRes {
  /** 主键 */
  id: string;
  /** 资产编号 */
  no: string;
  /** 附带说明（凭证号之类） */
  note?: string;
}

/** 卡片分页查询（`AssetFilter`）。 */
export interface AssetQuery {
  /** 页码 */
  page?: number;
  /** 每页条数（后端上限 100） */
  page_size?: number;
  /** 编号 / 名称 / 规格模糊 */
  keyword?: string;
  /** 分类 */
  category_id?: string;
  /** 使用部门 */
  use_dept_id?: string;
  /** 状态 */
  status?: AssetStatus | '';
  /** 来源类型 */
  source_type?: AssetSourceType | '';
}

/** 建卡入参（逐字抄 `CreateAssetReq`；金额一律传字符串）。 */
export interface AssetCreatePayload {
  /** 名称 */
  asset_name?: string;
  /** 分类 */
  category_id?: string;
  /** 规格型号 */
  spec?: string;
  /** 使用部门 */
  use_dept_id?: string;
  /** 使用人 */
  custodian_id?: string;
  /** 存放地点 */
  location?: string;
  /** 原值 */
  original_value?: string;
  /** 可抵扣进项税 */
  tax_amount?: string;
  /** 购入 / 入账日期 */
  purchase_date?: string;
  /** 来源类型 */
  source_type?: AssetSourceType;
  /** 来源单 id */
  source_id?: string;
  /** 来源单号 */
  source_no?: string;
  /** 购入口径 */
  buy_mode?: BuyMode;
  /** 付款方式 */
  pay_mode?: PayMode;
  /** 供应商 */
  supplier_id?: string;
  /** 备注 */
  remark?: string;
}

/** 改卡入参（只允许改这四个字段，后端 `UpdateAssetReq` 也就这四个）。 */
export interface AssetUpdatePayload {
  /** 名称 */
  asset_name?: string;
  /** 规格型号 */
  spec?: string;
  /** 存放地点 */
  location?: string;
  /** 备注 */
  remark?: string;
}

/** 部门转移入参（`TransferReq`）。 */
export interface AssetTransferPayload {
  /** 目标部门 */
  to_dept_id?: string;
  /** 变动日期（默认今天） */
  change_date?: string;
  /** 原因（必填） */
  reason?: string;
}

/** 换使用人入参（`CustodianReq`）。 */
export interface AssetCustodianPayload {
  /** 新使用人 */
  to_user_id?: string;
  /** 变动日期（默认今天） */
  change_date?: string;
  /** 原因（必填） */
  reason?: string;
}

/** 提交处置入参（`DisposeReq`）。 */
export interface AssetDisposePayload {
  /** 处置类型 */
  disposal_type?: DisposalType;
  /** 处置日期（默认今天） */
  disposal_date?: string;
  /** 原因 / 出售说明（必填） */
  reason?: string;
  /** 出售价（不含税；仅 SALE 可为非零） */
  sale_price?: string;
  /** 销项税（仅 SALE；不填 = 0） */
  output_tax?: string;
}

/** 卡片分页（`GET /asset/page`） */
export function fetchAssetPage(query: AssetQuery): Promise<PageData<AssetRow>> {
  return get<PageData<AssetRow>>(`${BASE}/asset/page`, asParams(query));
}

/** 卡片详情（`GET /asset/{id}`，含变动时间线 + 折旧明细 + 在跑审批） */
export function fetchAssetDetail(id: string): Promise<AssetDetail> {
  return get<AssetDetail>(`${BASE}/asset/${id}`);
}

/** 建卡（`POST /asset`，**同事务生成购入凭证**；回 `{id,no,note?}`） */
export function createAsset(payload: AssetCreatePayload): Promise<AssetIdRes> {
  return post<AssetIdRes>(`${BASE}/asset`, payload);
}

/** 改卡（`PUT /asset/{id}`，只改名称 / 规格 / 地点 / 备注；回详情） */
export function updateAsset(id: string, payload: AssetUpdatePayload): Promise<AssetDetail> {
  return put<AssetDetail>(`${BASE}/asset/${id}`, payload);
}

/** 删卡（`DELETE /asset/{id}`，软删） */
export function deleteAsset(id: string): Promise<unknown> {
  return del<unknown>(`${BASE}/asset/${id}`);
}

/** 部门转移（`POST /asset/{id}/transfer`，不产生凭证，只留痕） */
export function transferAsset(id: string, payload: AssetTransferPayload): Promise<AssetDetail> {
  return post<AssetDetail>(`${BASE}/asset/${id}/transfer`, payload);
}

/** 换使用人（`POST /asset/{id}/custodian`，不产生凭证，只留痕） */
export function custodianAsset(id: string, payload: AssetCustodianPayload): Promise<AssetDetail> {
  return post<AssetDetail>(`${BASE}/asset/${id}/custodian`, payload);
}

/** 提交处置（`POST /asset/{id}/dispose`，走 `asset_disposal` 审批流） */
export function disposeAsset(id: string, payload: AssetDisposePayload): Promise<AssetDetail> {
  return post<AssetDetail>(`${BASE}/asset/${id}/dispose`, payload);
}

/** 作废处置（`POST /asset/{id}/dispose/cancel`，红字回滚后资产回在役） */
export function cancelAssetDispose(id: string, reason: string): Promise<AssetDetail> {
  return post<AssetDetail>(`${BASE}/asset/${id}/dispose/cancel`, { reason });
}
