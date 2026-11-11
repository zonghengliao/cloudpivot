/**
 * 员工档案 API 层（ARCH-9 §5.2 #1 / #2 / #7 · HR-18）。
 *
 * 后端：`backend/src/business/hr/employee_handler.rs`（HR-12）+ `employee_dto.rs`（HR-11）。
 * 出参契约与词表在 `./employee-kinds.ts`（纯逻辑、可单测），本文件 `export *` 出来 ——
 * 页面只认 `@/api/business/hr/employee` 一个入口。
 *
 * | # | 方法 | 路径 | 权限码 | 页面用在哪 |
 * |---|---|---|---|---|
 * | 1 | GET | `/employee/page` | `business:hr:employee:list` | 列表（分页 / 筛选） |
 * | 2 | GET | `/employee/{id}` | `business:hr:employee:detail` | 详情抽屉（基本信息 / 合同 / 薪资 / 附件） |
 * | 7 | GET | `/employee/{id}/sensitive` | `business:hr:employee:sensitive` | 详情「查看明文」（**HR-13** 落地） |
 * | 16 | GET | `/employee/{id}/salary` | `business:hr:employee:detail` | **不用**：详情已带 `salary_items[]` |
 *
 * # 为什么详情页不单独调 #16（薪资标准）
 *
 * 设计 §5.2 #2 的详情出参已经含 `salary_items[]`（全量历史 + `current` 标记），
 * 再调一次 #16 只会多一次请求、多一处「两个来源不一致」的风险。#16 留给
 * 需要「只看薪资」的场景（薪资单计算页）。同理合同不调任何接口：`contracts[]` 在详情里。
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」——本仓最高频的前端踩坑。
 */

import { get, type PageData } from '@/api/request';

import type {
  EmployeeDetail,
  EmployeeListRow,
  EmployeePageQuery,
  EmployeeSensitivePlain,
} from './employee-kinds';

export * from './employee-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/hr';

/**
 * 丢掉空串 / null / undefined / 空数组（`status=[]` 表示「全部状态」）。
 *
 * 与 `erp/cost-period.ts::asParams` 同口径；数组按**逗号分隔**序列化（后端
 * `Query` 提取是 `serde_urlencoded`，重复键会被拒；本仓既有约定见
 * `announcement/handler.rs` 的 `query.status.split(',')`）。
 */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    if (Array.isArray(value)) {
      if (value.length === 0) {
        continue;
      }
      out[key] = value.join(',');
      continue;
    }
    out[key] = value;
  }
  return out;
}

/**
 * 员工列表分页（`GET /business/hr/employee/page`，权限 `business:hr:employee:list`）。
 *
 * 默认不含离职（设计 §5.3）：只有传 `include_resigned=true` 才出现 `RESIGNED`。
 * 出参是框架统一的 `{ list, total, page, page_size }`（`Res::page`）。
 */
export function fetchEmployeePage(query: EmployeePageQuery = {}): Promise<PageData<EmployeeListRow>> {
  return get<PageData<EmployeeListRow>>(`${BASE}/employee/page`, asParams(query));
}

/** 员工详情（含合同 / 薪资子表 / 敏感字段打码值），权限 `business:hr:employee:detail` */
export function fetchEmployeeDetail(id: string): Promise<EmployeeDetail> {
  return get<EmployeeDetail>(`${BASE}/employee/${id}`);
}

/**
 * 敏感字段明文（`GET /employee/{id}/sensitive?field=`，权限 `business:hr:employee:sensitive`）。
 *
 * 后端**每次调用都写 `audit_logs`**（设计 §5.2 #7 / D-14），所以页面只在用户
 * 主动点「查看明文」时才调，绝不在加载详情时预取。
 *
 * 出参形状未定稿（HR-13 未落地），由 `plainValueOf` 兜住三种候选形态。
 */
export function fetchEmployeeSensitive(
  id: string,
  field: 'id_card' | 'bank_account',
): Promise<EmployeeSensitivePlain> {
  return get<EmployeeSensitivePlain>(`${BASE}/employee/${id}/sensitive`, { field });
}
