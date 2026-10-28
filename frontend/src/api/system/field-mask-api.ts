import { del, get, post, put, type PageData } from '@/api/request';

/**
 * 字段脱敏（批次 B · B6）管理接口。
 *
 * # 契约来源
 *
 * 后端 `backend/src/business/masking/{dto,handler}.rs`（接口挂在 `/system/field-mask`）。
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET    | `/api/system/field-mask/page` | `system:field-mask:list` |
 * | POST   | `/api/system/field-mask` | `system:field-mask:create` |
 * | PUT    | `/api/system/field-mask/{id}` | `system:field-mask:update` |
 * | DELETE | `/api/system/field-mask/{id}` | `system:field-mask:delete` |
 * | GET    | `/api/system/field-mask/preview` | `system:field-mask:list` |
 * | GET    | `/api/system/field-mask/effective` | `system:field-mask:list` |
 *
 * # 三条不能违反的口径
 *
 * 1. **编辑不许改目标**：`PUT` 只接受 `mask_type` / `mask_param` / `status` / `reason`；
 *    要换表 / 换列 / 换角色必须删了重建（否则「同一条规则悄悄换了列」让审计失效）。
 * 2. **没有独立的启停接口**：启停走 `PUT`（带 `status`），因此启停按钮用 `update` 权限码。
 * 3. **试运行以服务端为准**：`preview` 由后端 `masker::apply` 计算，前端不复制一份
 *    脱敏算法（复制必然漂移，且正则分支无法与 Rust 的 `regex` 完全等价）。
 */

/** 一条脱敏规则 */
export interface FieldMaskRes {
  /** 规则 ID */
  id: string;
  /** 表名 */
  table_name: string;
  /** 列名 */
  column_name: string;
  /** 脱敏类型落库值（1..7） */
  mask_type: number;
  /** 脱敏类型稳定编码 */
  mask_type_code: string;
  /** 脱敏类型中文标签 */
  mask_type_label: string;
  /** 实际生效参数（库里为空时后端回填缺省值） */
  mask_param: string;
  /** 适用角色 ID；null = 全局规则 */
  role_id: string | null;
  /** 适用角色名称 */
  role_name: string | null;
  /** 作用域：GLOBAL / ROLE */
  scope: string;
  /** 状态：1 启用 / 0 停用 */
  status: number;
  /** 脱敏理由（合规留痕） */
  reason: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 规则分页查询参数 */
export type FieldMaskPageQuery = {
  /** 页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 表名精确匹配 */
  table_name?: string;
  /** 列名精确匹配 */
  column_name?: string;
  /** 作用域：global / role */
  scope?: string;
  /** 状态：1 启用 / 0 停用 */
  status?: number;
};

/** 新建规则参数 */
export type FieldMaskCreateParams = {
  /** 表名 */
  table_name: string;
  /** 列名 */
  column_name: string;
  /** 脱敏类型（1..7） */
  mask_type: number;
  /** 类型参数（正则类型必填） */
  mask_param?: string;
  /** 适用角色；缺省 = 全局 */
  role_id?: string;
  /** 状态，缺省 1 */
  status?: number;
  /** 脱敏理由（必填） */
  reason: string;
};

/** 修改规则参数（目标不可改） */
export type FieldMaskUpdateParams = {
  /** 脱敏类型（1..7） */
  mask_type: number;
  /** 类型参数 */
  mask_param?: string;
  /** 状态 */
  status?: number;
  /** 脱敏理由 */
  reason: string;
};

/** 试运行参数 */
export type MaskPreviewParams = {
  /** 脱敏类型（1..7） */
  mask_type: number;
  /** 类型参数 */
  mask_param?: string;
  /** 待试运行的原文 */
  value?: string;
};

/** 试运行结果 */
export interface MaskPreviewRes {
  /** 类型落库值 */
  mask_type: number;
  /** 类型编码 */
  mask_type_code: string;
  /** 类型中文标签 */
  mask_type_label: string;
  /** 实际生效参数 */
  mask_param: string;
  /** 原文 */
  input: string;
  /** 脱敏后 */
  output: string;
  /** 是否发生变化（false 可能是值本身无需脱敏） */
  changed: boolean;
}

/** 一条生效规则（带来源） */
export interface EffectiveRuleRes {
  /** 规则 ID */
  rule_id: string;
  /** 表名 */
  table_name: string;
  /** 列名 */
  column_name: string;
  /** 类型落库值 */
  mask_type: number;
  /** 类型中文标签 */
  mask_type_label: string;
  /** 生效参数 */
  mask_param: string;
  /** 来源：GLOBAL / ROLE */
  source: string;
  /** 来源角色名 */
  source_name: string | null;
  /** 默认样例的试运行结果 */
  sample: string;
}

/** 某用户实际生效的规则清单 */
export interface EffectiveMaskRes {
  /** 目标用户 ID */
  user_id: string;
  /** 目标用户名 */
  username: string;
  /** 是否超管 */
  is_super_admin: boolean;
  /** 是否被豁免 */
  exempt: boolean;
  /** 豁免原因编码 */
  exempt_reason: string;
  /** 生效规则条数 */
  total: number;
  /** 生效规则 */
  rules: EffectiveRuleRes[];
}

/**
 * 7 种脱敏类型（值 / 编码 / 文案 / 参数提示 / 缺省参数 / 样例值）。
 *
 * 与后端 `MaskType` 逐字一致；参数提示来自 `MaskType::param_hint`，样例值来自
 * `dto::sample_value`——前端只用它们做输入引导，**最终结果一律以 `preview` 接口为准**。
 */
export const MASK_TYPE_OPTIONS: ReadonlyArray<{
  /** 落库值 */
  value: number;
  /** 稳定编码 */
  code: string;
  /** 中文标签 */
  label: string;
  /** 参数提示 */
  paramHint: string;
  /** 缺省参数（正则类型无缺省，必须显式填写） */
  defaultParam: string;
  /** 样例值（试运行输入框的默认内容） */
  sample: string;
}> = [
  { value: 1, code: 'mobile', label: '手机号', paramHint: '保留前N,后M（如 3,4）', defaultParam: '3,4', sample: '13812345678' },
  { value: 2, code: 'id_card', label: '身份证', paramHint: '保留前N,后M（如 4,4）', defaultParam: '4,4', sample: '110101199001011234' },
  { value: 3, code: 'email', label: '邮箱', paramHint: '无参数（留空）', defaultParam: '', sample: 'alice@mail.com' },
  { value: 4, code: 'name', label: '姓名', paramHint: '保留前N（如 1）', defaultParam: '1', sample: '张三丰' },
  { value: 5, code: 'bank_card', label: '银行卡', paramHint: '保留前N,后M（如 4,4）', defaultParam: '4,4', sample: '6222021234567890' },
  { value: 6, code: 'amount', label: '金额', paramHint: '区间宽度（如 1000）', defaultParam: '1000', sample: '48620.00' },
  { value: 7, code: 'regex', label: '自定义正则', paramHint: '正则与保留组（如 ^(\\w{2}).*$|1）', defaultParam: '', sample: 'AB12345' },
];

/**
 * 表 → 可选字段的建议清单（**仅作输入引导**）。
 *
 * 后端没有「列出可配置表 / 字段」的元数据接口（B6 只在写入时查 `information_schema`
 * 校验，不存在即 `404 + 61006`），因此前端给一份常见表 + 字段的建议项，并允许自由输入；
 * 合法性的唯一权威是后端。清单里的字段名已与本机 `information_schema` 核对。
 */
export const FIELD_SUGGESTIONS: Readonly<Record<string, string[]>> = {
  sys_user: ['mobile', 'email', 'real_name', 'username', 'remark'],
  sys_dept: ['phone', 'email', 'dept_name', 'dept_code'],
  sys_role: ['role_name', 'description', 'remark'],
  sys_file: ['original_name', 'file_name', 'storage_path'],
  sys_notice: ['title', 'content', 'sender_name', 'notice_no'],
  sys_notice_template: ['template_name', 'title_template', 'content_template'],
  sys_config: ['config_key', 'config_value', 'description'],
  sys_storage_config: ['config_name', 'endpoint', 'bucket', 'base_path'],
};

/** 表名建议项（与上面的字段清单同一来源） */
export const TABLE_SUGGESTIONS: string[] = Object.keys(FIELD_SUGGESTIONS);

/** 分页查询脱敏规则 */
export function pageFieldMasks(query: FieldMaskPageQuery): Promise<PageData<FieldMaskRes>> {
  return get<PageData<FieldMaskRes>>('/api/system/field-mask/page', query);
}

/** 新建脱敏规则 */
export function createFieldMask(data: FieldMaskCreateParams): Promise<FieldMaskRes> {
  return post<FieldMaskRes>('/api/system/field-mask', data);
}

/** 修改脱敏规则（表 / 列 / 角色不可改） */
export function updateFieldMask(id: string, data: FieldMaskUpdateParams): Promise<FieldMaskRes> {
  return put<FieldMaskRes>(`/api/system/field-mask/${id}`, data);
}

/** 删除脱敏规则（软删） */
export function deleteFieldMask(id: string): Promise<unknown> {
  return del<unknown>(`/api/system/field-mask/${id}`);
}

/** 试运行某一类型 + 参数对给定值的效果 */
export function previewMask(params: MaskPreviewParams): Promise<MaskPreviewRes> {
  return get<MaskPreviewRes>('/api/system/field-mask/preview', {
    mask_type: params.mask_type,
    mask_param: params.mask_param,
    value: params.value,
  });
}

/** 查询某用户实际生效的规则（缺省 = 自己） */
export function getEffectiveMasks(userId?: string): Promise<EffectiveMaskRes> {
  return get<EffectiveMaskRes>('/api/system/field-mask/effective', userId ? { user_id: userId } : {});
}

/** 字段脱敏模块接口集合 */
export const fieldMaskApi = {
  pageFieldMasks,
  createFieldMask,
  updateFieldMask,
  deleteFieldMask,
  previewMask,
  getEffectiveMasks,
};
