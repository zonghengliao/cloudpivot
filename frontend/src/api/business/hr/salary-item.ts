/**
 * 薪资项配置页 API 层（ARCH-9 §5.2 #32~#35 · §6.2.4 · HR-44 前端）。
 *
 * 后端：`salary_item_handler.rs`（HR-31）。出参契约、词表、公式编辑器纯逻辑在
 * `./salary-item-kinds.ts`（纯逻辑、可单测），本文件 `export *` 出来 ——
 * 页面只认 `@/api/business/hr/salary-item` 一个入口。
 *
 * | # | 方法 | 路径（后端注解不含 `/api`） | 权限码 | 页面用在哪 |
 * |:--:|---|---|---|---|
 * | 32 | GET  | `/business/hr/salary-item/page` | `salary-item:list` | 配置列表（含 `formula` / `depends_on`） |
 * | 33 | POST | `/business/hr/salary-item` | `salary-item:create` | 新建 |
 * | 34 | PUT  | `/business/hr/salary-item/{id}` | `salary-item:update` | 编辑（编码不可改） |
 * | 35 | POST | `/business/hr/salary-item/validate` | `salary-item:update` | 「检查」按钮 |
 *
 * # 路径必须写全 `/api`
 *
 * 后端注解不含前缀（`[router].prefix` 注入）；漏了会命中 SPA fallback 返回 HTML，
 * 报「响应体格式不符合」—— 本仓最高频的前端踩坑（`AGENTS.md` 约定 9）。
 *
 * # 检查按钮为什么不用 `catch` 判成功
 *
 * `POST /salary-item/validate` 在「公式有错」时**仍然回 200**（`ok=false`）：
 * 「公式有错」是查询结论，不是请求错误。所以页面必须看 `res.ok` / `res.errors`，
 * 只有网络错误 / 没传 formula / id 不存在才走 `catch`（后端 #35 头注释）。
 *
 * # 没有 DELETE
 *
 * 设计 §5.2 的 46 条接口里没有「删除薪资项」，后端也没有 DELETE 路由：
 * 配置表的生命周期是**停用**（`is_active=false`），不是删行 —— 历史薪资单的
 * `detail_item.item_id` 指向它（`ON DELETE SET NULL`），删了行等于把历史工资条上的项名抹掉。
 * 所以本文件不提供 `deleteSalaryItem`，页面只给「启用 / 停用」开关。
 */

import { get, post, put, type PageData } from '@/api/request';
import { fetchAccountPage } from '@/api/business/erp';

import type {
  SalaryItemRow,
  SalaryItemValidateRes,
  SalaryItemWritePayload,
} from './salary-item-kinds';

export * from './salary-item-kinds';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/hr';

/**
 * 配置列表（`GET /salary-item/page`，权限 `salary-item:list`）。
 *
 * 查询串由 `buildPageQuery` 生成（空筛选不发空参数）。回执含 `formula` 本体与
 * `depends_on`（后端抽好的依赖编码），列表页不用自己解析公式。
 */
export function fetchSalaryItemPage(
  query: Record<string, unknown> = {},
): Promise<PageData<SalaryItemRow>> {
  return get<PageData<SalaryItemRow>>(`${BASE}/salary-item/page`, query);
}

/**
 * 新建薪资项（`POST /salary-item`，权限 `salary-item:create`）。
 *
 * 后端在一个事务里做「公式依赖检查 → 编码 / 顺序占用预检 → 插入」，任何一环不过回 400/409，
 * 报文是中文人话（可直接弹）。
 */
export function createSalaryItem(payload: SalaryItemWritePayload): Promise<SalaryItemRow> {
  return post<SalaryItemRow>(`${BASE}/salary-item`, payload);
}

/**
 * 修改薪资项（`PUT /salary-item/{id}`，权限 `salary-item:update`）。
 *
 * **编码改不了**（后端守卫：公式按 `{编码}` 引用、薪资标准按 `item_code` 弱引用），
 * 表单里编码框是 disabled 的；`is_taxable` / `is_active` 不传则保持原值。
 */
export function updateSalaryItem(id: string, payload: SalaryItemWritePayload): Promise<SalaryItemRow> {
  return put<SalaryItemRow>(`${BASE}/salary-item/${id}`, payload);
}

/**
 * 公式检查（`POST /salary-item/validate`，权限 `salary-item:update`）。
 *
 * `id` 优先于 `item_code`（改已有项时传 id：后端会把库里那一项的旧公式替换成新公式再判环）。
 * 新建时传 `item_code`（还没 id）。**两个都不传也合法**：只检查语法与「引用到哪些变量」。
 *
 * 回执 `ok=false` 也是 200，详见文件头。
 */
export function validateSalaryItemFormula(payload: {
  formula: string;
  item_code?: string;
  id?: string;
}): Promise<SalaryItemValidateRes> {
  return post<SalaryItemValidateRes>(`${BASE}/salary-item/validate`, payload);
}

// ==================== 科目下拉（末级科目） ====================

/** 科目候选项（薪资项存的是**科目码**，不是 id —— 与 `biz_salary_item.*_account_code` 一致） */
export interface SalaryAccountOption {
  /** 科目码（`5602.10`）—— 提交进 `debit_account_code` / `credit_account_code` 的就是它 */
  code: string;
  /** 科目名（`管理费用-工资`） */
  name: string;
}

/**
 * 拉可用科目：**只要末级 + 启用**（设计 §6.2.4：避免选了非末级过账被拒）。
 *
 * 与 `budget.ts::fetchBudgetAccountOptions` 同口径（`is_leaf=true` + `status=1`），但
 * 这里返回的是 `account_code`：薪资项表存科目码（`VARCHAR(30)`），没有科目 id 外键，
 * 过账时按码去 `biz_account` 找（找不到 / 非末级在过账卡 HR-39 报错）。
 *
 * 失败**不抛**：科目下拉拿不到不该挡住整个抽屉（用户可能只是想改公式），
 * 退回可手输的 `t-select`（`creatable` 关、但输入框能打字 → 页面用 `filterable` + 空 options
 * 时空态提示）。所以这里 `catch` 回空数组，由页面显示空态。
 */
export async function fetchSalaryAccountOptions(limit = 500): Promise<SalaryAccountOption[]> {
  try {
    const data = await fetchAccountPage({ is_leaf: true, status: 1, page: 1, page_size: limit });
    return (data.list ?? []).map((row) => ({ code: row.account_code, name: row.account_name }));
  } catch {
    return [];
  }
}
