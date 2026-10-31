/**
 * 全量页面目录（测试E / t_4c9d4e71）。
 *
 * 这份表是「52 页逐个点名」的唯一来源：卡面点名的 52 项 + 生产菜单里另外两条
 * 未在卡面出现但确实在侧栏里的页面（`/business/erp/purchase-order-item`、`/business/erp/seq`），
 * 共 56 项（登录页单独在 20 号 spec 里跑，不参与已登录页面套件）。
 *
 * 路径取自**真实路由**（`src/router/index.ts`），不是卡面草稿：
 * 卡面写 `/system/masking` / `/system/mutex` / `/system/changelog` / `/announcement/center` /
 * `/business/help/center`，真实路由分别是 `/system/field-mask` / `/system/role-mutex` /
 * `/system/change-log` / `/announcement` / `/help`（菜单种子同口径）。卡面路径能否打开
 * 由 `30-menu-alignment` 里的「卡面旧路径回归」一段单独断言。
 */

export type PageKind =
  /** 表格型列表页：有分页接口 + 表格 */
  | 'list'
  /** 非表格页（卡片 / 表单 / 看板 / 自定义布局）：只断言有内容、接口 200 */
  | 'custom'
  /** 错误页 / 结果页 */
  | 'result';

export interface PageSpec {
  /** 页面中文名（报告里的主键） */
  key: string;
  /** 真实前端路由 */
  path: string;
  kind: PageKind;
  /** 列表接口 URL 片段（`kind=list` 时必填；缺省由 path + '/page' 推出来） */
  listApi?: string;
  /** 卡面（工单）里写的路径，与真实路由不一致时填，用于回归「旧路径是否还能开」 */
  legacyPath?: string;
  /** 该页没有「新建/新增」入口（只读页），⑥ 项按「只读页无写入口」断言 */
  readOnly?: boolean;
  /**
   * ⑥ 的入口形态：
   * - `dialog`（默认）：点「新建 / 新增」弹弹窗 / 抽屉，然后在弹窗里空提交；
   * - `page`：页面本身就是表单（如消息发送），提交按钮在页面上；
   * - `navigate`：入口是跳转（如导入任务「发起导入」→ /business/impexp/import）。
   */
  formMode?: 'dialog' | 'page' | 'navigate';
  /**
   * 已知产品缺陷（**不是**测试口径问题）：命中时该页对应的 it 用 `test.fail()` 标注，
   * 套件仍然整体绿，但会以「expected to fail」列在报告里；修好后它会变红提醒摘标注。
   */
  knownBug?: { checks?: Array<'①' | '②' | '③' | '④' | '⑤' | '⑥'>; reason: string };
  /** 特殊说明（写进报告） */
  note?: string;
}

/** 表格型列表页的列表接口默认规则：`<path>/page` */
function defaultListApi(path: string): string {
  return `${path}/page`;
}

const raw: PageSpec[] = [
  // ===== 登录与首页（3）=====
  { key: '工作台', path: '/dashboard', kind: 'custom', listApi: '/api/support/dashboard/overview', note: '首页没有列表接口，断言概览接口 200 + 快捷入口数量' },
  { key: '403 无权限页', path: '/result/403', kind: 'result' },
  { key: '404 页面不存在', path: '/result/404', kind: 'result' },
  { key: '500 服务异常页', path: '/result/500', kind: 'result' },

  // ===== 工作任务（1 页 4 视图 + 任务列表路由）=====
  { key: '任务列表', path: '/task/list', kind: 'list', listApi: '/api/business/task/page' },
  { key: '任务看板', path: '/task/board', kind: 'custom', listApi: '/api/business/task/page', note: '看板是五列卡片，不是表格' },
  { key: '甘特图', path: '/task/gantt', kind: 'custom', listApi: '/api/business/task/gantt' },
  { key: '任务模板', path: '/task/templates', kind: 'list', listApi: '/api/business/task/templates' },

  // ===== 项目（1）=====
  { key: '项目', path: '/project', kind: 'list', listApi: '/api/business/project/page', legacyPath: '/business/project' },

  // ===== 审批流（3）=====
  { key: '流程定义', path: '/flow/def', kind: 'list', listApi: '/api/business/flow/def/page' },
  { key: '发起审批', path: '/flow/start', kind: 'custom', listApi: '/api/business/flow/def/page', note: '发起页是选流程卡片 + 表单' },
  { key: '审批中心（待办）', path: '/flow/task', kind: 'custom', listApi: '/api/business/flow/task/todo', note: '待办是卡片列表' },

  // ===== 公告与消息（5）=====
  // 公告中心 / 消息中心是**卡片流 + 详情抽屉**（实测 .t-table = 0），不是表格页；
  // 判它们「内容是表格」会假红，所以按 custom 口径断言「主内容有实质内容 + 接口 200」。
  { key: '公告中心', path: '/announcement', kind: 'custom', listApi: '/api/business/announcement/visible', legacyPath: '/announcement/center', note: '卡片流 + 详情抽屉，无表格' },
  { key: '公告管理', path: '/business/announcement', kind: 'list', listApi: '/api/business/announcement/page', legacyPath: '/announcement/manage' },
  { key: '消息中心', path: '/business/notice/message', kind: 'custom', listApi: '/api/business/notice/mine', note: '卡片流 + 详情面板，无表格' },
  { key: '消息发送', path: '/business/notice/send', kind: 'custom', listApi: '/api/business/notice/template/page', formMode: 'page', note: '发送页本身就是表单，提交按钮在页面上' },
  { key: '消息模板', path: '/business/notice/template', kind: 'list', listApi: '/api/business/notice/template/page', knownBug: { checks: ['⑥'], reason: '新建模板弹窗空提交直接打 POST /api/business/notice/template（前端缺必填校验，靠后端 422 兜底）' } },

  // ===== ERP（15 + 2 条菜单里多出来的）=====
  { key: '物料管理', path: '/business/erp/material', kind: 'list', listApi: '/api/business/erp/material/page' },
  { key: '仓库管理', path: '/business/erp/warehouse', kind: 'list', listApi: '/api/business/erp/warehouse/tree', note: '仓库是「树 + 库位列表」页：数据源是 /warehouse/tree（数组），不是 /warehouse/page' },
  { key: '库位管理', path: '/business/erp/location', kind: 'list', listApi: '/api/business/erp/location/page' },
  { key: '计量单位', path: '/business/erp/unit', kind: 'list', listApi: '/api/business/erp/unit/page' },
  { key: '供应商', path: '/business/erp/supplier', kind: 'list', listApi: '/api/business/erp/supplier/page' },
  { key: '序列号管理', path: '/business/erp/serial', kind: 'list', listApi: '/api/business/erp/serial/page' },
  { key: '即时库存', path: '/business/erp/inventory', kind: 'list', listApi: '/api/business/erp/inventory/page' },
  { key: '库位库存', path: '/business/erp/inventory-location', kind: 'list', listApi: '/api/business/erp/inventory-location/page' },
  { key: '库存流水', path: '/business/erp/inventory-txn', kind: 'list', listApi: '/api/business/erp/inventory-txn/page' },
  { key: '库存调拨', path: '/business/erp/transfer', kind: 'list', listApi: '/api/business/erp/transfer/page', knownBug: { checks: ['①', '③'], reason: 'GET /api/business/erp/transfer/page 在 HEAD 代码上稳定 500，页面显示「调拨单列表加载失败：服务器内部错误」（P10/P11 正在改 transfer，需他们确认）' } },
  { key: '采购订单', path: '/business/erp/purchase-order', kind: 'list', listApi: '/api/business/erp/purchase-order/page', legacyPath: '/business/erp/purchase' },
  { key: '收货入库', path: '/business/erp/stock-in', kind: 'list', listApi: '/api/business/erp/stock-in/page', legacyPath: '/business/erp/stockin' },
  { key: '应付单', path: '/business/erp/payable', kind: 'list', listApi: '/api/business/erp/payable/page' },
  { key: '记账凭证', path: '/business/erp/voucher', kind: 'list', listApi: '/api/business/erp/voucher/page', knownBug: { checks: ['⑥'], reason: '新增凭证弹窗空提交后未观察到任何必填校验文案（按钮「保存并记账」），需要人工复核是否漏了必填校验' } },
  { key: '总账', path: '/business/erp/ledger', kind: 'list', listApi: '/api/business/erp/account/page', note: '总账是「先选科目再查流水」，进页面只会拉科目下拉（/account/page）；列接口在选科目后才发' },
  { key: '会计科目', path: '/business/erp/account', kind: 'list', listApi: '/api/business/erp/account/page' },
  { key: '会计期间', path: '/business/erp/period', kind: 'list', listApi: '/api/business/erp/period/page' },
  { key: '采购明细（菜单有、卡面无）', path: '/business/erp/purchase-order-item', kind: 'list', listApi: '/api/business/erp/purchase-order-item/page', note: '侧栏菜单可见，但 router 里没有这条路由 → 预期 404，由专项 1 判红' },
  { key: '单号序列（菜单有、卡面无）', path: '/business/erp/seq', kind: 'list', listApi: '/api/business/erp/seq/page', note: '侧栏菜单可见，但 router 里没有这条路由 → 预期 404，由专项 1 判红' },

  // ===== 导入导出（3）=====
  { key: '导入任务', path: '/business/impexp/import-task', kind: 'list', listApi: '/api/business/impexp/import/page', formMode: 'navigate', note: '「发起导入」是跳转到 /business/impexp/import 向导页，不是弹窗' },
  { key: '导出任务', path: '/business/impexp/export-task', kind: 'list', listApi: '/api/business/impexp/export/page' },
  { key: '导入向导', path: '/business/impexp/import', kind: 'custom', listApi: '/api/business/impexp/def/list', note: '三步向导（选文件 → 校验 → 入库），没有表格' },

  // ===== 文件存储（2）=====
  { key: '文件管理（存储）', path: '/business/storage/file', kind: 'list', listApi: '/api/business/storage/file/page' },
  { key: '存储后端', path: '/business/storage/backend', kind: 'list', listApi: '/api/business/storage/config/page' },

  // ===== 帮助（2）=====
  { key: '帮助中心', path: '/help', kind: 'custom', listApi: '/api/business/help/category/tree', legacyPath: '/business/help/center' },
  { key: '帮助文档管理', path: '/help/manage', kind: 'list', listApi: '/api/business/help/doc/page', legacyPath: '/business/help/manage' },

  // ===== 系统管理（8）=====
  { key: '用户管理', path: '/system/user', kind: 'list', listApi: '/api/system/user/page' },
  { key: '角色管理', path: '/system/role', kind: 'list', listApi: '/api/system/role/page' },
  { key: '菜单管理', path: '/system/menu', kind: 'list', listApi: '/api/system/menu/tree' },
  { key: '部门管理', path: '/system/dept', kind: 'list', listApi: '/api/system/dept/tree' },
  { key: '数据权限', path: '/system/data-scope', kind: 'list', listApi: '/api/system/role/page', note: '数据范围是角色的属性：左「我的范围自检」+ 右角色列表（/system/role/page），不是独立实体分页' },
  { key: '字段脱敏', path: '/system/field-mask', kind: 'list', listApi: '/api/system/field-mask/page', legacyPath: '/system/masking' },
  { key: '角色互斥', path: '/system/role-mutex', kind: 'list', listApi: '/api/business/mutex/page', legacyPath: '/system/mutex' },
  { key: '变更日志', path: '/system/change-log', kind: 'list', listApi: '/api/business/changelog/page', legacyPath: '/system/changelog' },

  // ===== 支撑功能（6）=====
  { key: '字典管理', path: '/support/dict', kind: 'custom', listApi: '/api/support/dict/type/page', note: '左树右详情（字典类型列表 + 字典项表格），列表接口是 /type/page' },
  { key: '参数配置', path: '/support/config', kind: 'custom', listApi: '/api/support/config/page', note: '按分类分组的卡片流（每项一行「保存/编辑」），不是分页表格' },
  { key: '文件管理（支撑）', path: '/support/file', kind: 'list', listApi: '/api/support/file/page' },
  { key: '定时任务', path: '/support/job', kind: 'list', listApi: '/api/support/job/page' },
  { key: '操作日志', path: '/support/log', kind: 'list', listApi: '/api/support/log/page' },
  { key: '在线用户', path: '/support/online', kind: 'list', listApi: '/api/support/online/page', readOnly: true },
];

export const PAGES: PageSpec[] = raw.map((p) => ({ ...p, listApi: p.listApi ?? defaultListApi(p.path) }));

/** 按 key 取一页（spec 里写错 key 会立刻抛，不许静默丢掉） */
export function page(key: string): PageSpec {
  const found = PAGES.find((p) => p.key === key);
  if (!found) {
    throw new Error(`页面目录里没有「${key}」，可用 key：${PAGES.map((p) => p.key).join(' / ')}`);
  }
  return found;
}

export function pages(keys: string[]): PageSpec[] {
  return keys.map(page);
}
