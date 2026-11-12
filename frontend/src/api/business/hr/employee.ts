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
 * | 3 | POST | `/employee` | `business:hr:employee:create` | 新建表单（工号留空 ⇒ 后端取号） |
 * | 4 | PUT | `/employee/{id}` | `business:hr:employee:update` | 编辑表单（预填详情） |
 * | 5 | DELETE | `/employee/{id}` | `business:hr:employee:delete` | 详情抽屉「删除」（软删 + 输姓名确认） |
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

import { del, get, post, put, type PageData } from '@/api/request';

import type {
  EmployeeCreatedRes,
  EmployeeCreateReq,
  EmployeeDetail,
  EmployeeIdRes,
  EmployeeListRow,
  EmployeePageQuery,
  EmployeeSensitivePlain,
  EmployeeUpdateReq,
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

/**
 * 新建员工（`POST /business/hr/employee`，权限 `business:hr:employee:create`）。
 *
 * # 工号不用前端拼
 *
 * `emp_no` 留空 ⇒ 后端在**同一个事务**里取号（`EMP-0001` 形态，HR-14）：取号失败
 * 或插入失败一起回滚，号不被白吃。所以「留空自动取号」是本接口的正式用法，
 * 前端只把回执里的 `emp_no` 回显给用户。
 *
 * # 新档一律 PROBATION
 *
 * 后端新档入口状态是「试用期」（设计 §4.2.1），不是前端传的 —— 别在请求体里塞 status。
 */
export function createEmployee(req: EmployeeCreateReq): Promise<EmployeeCreatedRes> {
  return post<EmployeeCreatedRes>(`${BASE}/employee`, req);
}

/**
 * 修改员工（`PUT /business/hr/employee/{id}`，权限 `business:hr:employee:update`）。
 *
 * 整表单提交，但**不传的字段保持原值**（后端 `merged_write` 回填）、传空串才是清空 ——
 * 请求体的取舍由 `employee-form.ts::buildUpdatePayload` 决定（身份证 / 工资卡号拿不到
 * 明文，只在该用户真的重填时才提交）。已离职档案后端 409（只读）。
 */
export function updateEmployee(id: string, req: EmployeeUpdateReq): Promise<EmployeeIdRes> {
  return put<EmployeeIdRes>(`${BASE}/employee/${id}`, req);
}

/**
 * 删除员工（`DELETE /business/hr/employee/{id}`，权限 `business:hr:employee:delete`）。
 *
 * 后端是**软删**（`deleted_at` 落时间戳，三个唯一键随之释放）：档案从列表消失，
 * 但薪资单 / 考勤台账里的历史引用还在。界面上必须二次确认（输入姓名），
 * 因为这是「看得到的删除、查得到的残留」。
 */
export function deleteEmployee(id: string): Promise<EmployeeIdRes> {
  return del<EmployeeIdRes>(`${BASE}/employee/${id}`);
}

