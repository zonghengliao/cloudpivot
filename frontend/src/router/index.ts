import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';
import { useUserStore } from '@/stores/user';

/** 路由元信息：`title` 供面包屑与页面标题使用，`public` 标记免登录页面 */
declare module 'vue-router' {
  interface RouteMeta {
    /** 页面标题 */
    title?: string;
    /** 是否为免登录页面 */
    public?: boolean;
    /** 访问该页面所需的权限码 */
    permission?: string;
  }
}

/** 业务路由表：权限码与后端菜单种子数据保持一致 */
const routes: RouteRecordRaw[] = [
  {
    path: '/',
    redirect: '/dashboard',
  },
  {
    path: '/login',
    name: 'Login',
    component: () => import('@/views/login/index.vue'),
    meta: { title: '登录', public: true },
  },
  {
    // 首登强制改密页：独立于工作台布局，避免强制改密时还要渲染侧边栏（菜单此时可能为空）
    path: '/change-password',
    name: 'ChangePassword',
    component: () => import('@/views/user/change-password.vue'),
    meta: { title: '修改初始密码' },
  },
  {
    path: '/',
    component: () => import('@/layouts/BasicLayout.vue'),
    children: [
      {
        path: 'dashboard',
        name: 'Dashboard',
        component: () => import('@/views/dashboard/index.vue'),
        meta: { title: '首页' },
      },
      {
        path: 'system/user',
        name: 'SystemUser',
        component: () => import('@/views/system/user/index.vue'),
        meta: { title: '用户管理', permission: 'system:user:list' },
      },
      {
        path: 'system/role',
        name: 'SystemRole',
        component: () => import('@/views/system/role/index.vue'),
        meta: { title: '角色管理', permission: 'system:role:list' },
      },
      {
        path: 'system/menu',
        name: 'SystemMenu',
        component: () => import('@/views/system/menu/index.vue'),
        meta: { title: '菜单管理', permission: 'system:menu:list' },
      },
      {
        path: 'system/dept',
        name: 'SystemDept',
        component: () => import('@/views/system/dept/index.vue'),
        meta: { title: '部门管理', permission: 'system:dept:list' },
      },
      {
        path: 'support/dict',
        name: 'SupportDict',
        component: () => import('@/views/support/dict/index.vue'),
        meta: { title: '字典管理', permission: 'support:dict:list' },
      },
      {
        path: 'support/config',
        name: 'SupportConfig',
        component: () => import('@/views/support/config/index.vue'),
        meta: { title: '参数配置', permission: 'support:config:list' },
      },
      {
        path: 'support/file',
        name: 'SupportFile',
        component: () => import('@/views/support/file/index.vue'),
        meta: { title: '文件管理', permission: 'support:file:list' },
      },
      {
        path: 'support/log',
        name: 'SupportLog',
        component: () => import('@/views/support/log/index.vue'),
        meta: { title: '操作日志', permission: 'support:log:list' },
      },
      {
        path: 'support/job',
        name: 'SupportJob',
        component: () => import('@/views/support/job/index.vue'),
        meta: { title: '定时任务', permission: 'support:job:list' },
      },
      {
        path: 'support/online',
        name: 'SupportOnline',
        component: () => import('@/views/support/online/index.vue'),
        meta: { title: '在线用户', permission: 'support:online:list' },
      },
      {
        path: 'support/invariant',
        name: 'SupportInvariant',
        component: () => import('@/views/support/invariant/index.vue'),
        meta: { title: '账务巡检', permission: 'support:invariant:list' },
      },
      {
        path: 'user/profile',
        name: 'UserProfile',
        component: () => import('@/views/user/profile.vue'),
        meta: { title: '个人中心' },
      },
      {
        // 外观设置（配色方案 × 明暗模式）：纯前端 localStorage 持久化，无后端接口。
        // 权限点 system:preference:view 已种到 sys_menu 并授予全部现有角色，
        // 便于后续按角色收回（菜单本身 is_visible=false，不进侧栏）。
        path: 'user/preference',
        name: 'UserPreference',
        component: () => import('@/views/user/preference.vue'),
        meta: { title: '外观设置', permission: 'system:preference:view' },
      },
      // ==================== 通用底座 · 批次 A（对象存储 / 导入导出 / 消息通知） ====================
      {
        path: 'business/storage/backend',
        name: 'BusinessStorageBackend',
        component: () => import('@/views/business/storage/backend/index.vue'),
        meta: { title: '存储管理', permission: 'business:storage:config:list' },
      },
      {
        path: 'business/storage/file',
        name: 'BusinessStorageFile',
        component: () => import('@/views/business/storage/file/index.vue'),
        meta: { title: '文件管理', permission: 'business:storage:list' },
      },
      {
        path: 'business/impexp/import',
        name: 'BusinessImpexpImport',
        component: () => import('@/views/business/impexp/import/index.vue'),
        meta: { title: '导入向导', permission: 'business:impexp:import:upload' },
      },
      {
        path: 'business/impexp/import-task',
        name: 'BusinessImpexpImportTask',
        component: () => import('@/views/business/impexp/import-task/index.vue'),
        meta: { title: '导入任务', permission: 'business:impexp:import:list' },
      },
      {
        path: 'business/impexp/export-task',
        name: 'BusinessImpexpExportTask',
        component: () => import('@/views/business/impexp/export-task/index.vue'),
        meta: { title: '导出任务', permission: 'business:impexp:export:list' },
      },
      {
        // 消息中心：菜单 is_visible=false，从顶栏铃铛进入（PRD §7.2 隐藏菜单口径）
        path: 'business/notice/message',
        name: 'BusinessNoticeMessage',
        component: () => import('@/views/business/notice/message/index.vue'),
        meta: { title: '消息中心', permission: 'business:notice:mine' },
      },
      {
        path: 'business/notice/template',
        name: 'BusinessNoticeTemplate',
        component: () => import('@/views/business/notice/template/index.vue'),
        meta: { title: '通知模板', permission: 'business:notice:template:list' },
      },
      {
        path: 'business/notice/send',
        name: 'BusinessNoticeSend',
        component: () => import('@/views/business/notice/send/index.vue'),
        meta: { title: '发送通知', permission: 'business:notice:send' },
      },
      {
        // ==================== 通用底座 · 批次 B（数据权限 / 字段脱敏 / 角色互斥） ====================
        // path 与 `sys_menu.path`（V0065 种子）逐字一致：侧栏链接来自后端菜单，
        // 路由不存在 = 点菜单进 404。权限码同样与后端注解 / 种子三方一致。
        path: 'system/data-scope',
        name: 'SystemDataScope',
        component: () => import('@/views/system/data-scope/index.vue'),
        meta: { title: '数据权限', permission: 'business:permission:scope:read' },
      },
      {
        path: 'system/field-mask',
        name: 'SystemFieldMask',
        component: () => import('@/views/system/masking/index.vue'),
        meta: { title: '字段脱敏', permission: 'system:field-mask:list' },
      },
      {
        path: 'system/role-mutex',
        name: 'SystemRoleMutex',
        component: () => import('@/views/system/mutex/index.vue'),
        meta: { title: '角色互斥', permission: 'system:role-mutex:list' },
      },
      // ==================== 通用底座 · 批次 C（公告 / 帮助 / 变更日志） ====================
      // path 与 `sys_menu.path` 逐字一致，否则侧栏点了进 404：
      //   公告中心 V0087 → `/announcement`；公告管理 V0085 → `/business/announcement`；
      //   帮助中心 V0088 → `/help`；变更日志 V0085 → `/system/change-log`。
      // 帮助文档管理没有独立菜单（`V0088` 只种了帮助中心一个页面），由帮助中心的
      // 「文档管理」按钮进入，用 `system:help:doc:list` 守门；变更详情是列表的子路由。
      {
        path: 'announcement',
        name: 'AnnouncementCenter',
        component: () => import('@/views/business/announcement/center/index.vue'),
        meta: { title: '公告中心', permission: 'business:announcement:read' },
      },
      {
        path: 'business/announcement',
        name: 'AnnouncementManage',
        component: () => import('@/views/business/announcement/manage/index.vue'),
        meta: { title: '公告管理', permission: 'business:announcement:list' },
      },
      {
        path: 'help',
        name: 'HelpCenter',
        component: () => import('@/views/business/help/center/index.vue'),
        meta: { title: '帮助中心', permission: 'system:help:read' },
      },
      {
        path: 'help/manage',
        name: 'HelpManage',
        component: () => import('@/views/business/help/manage/index.vue'),
        meta: { title: '帮助文档管理', permission: 'system:help:doc:list' },
      },
      {
        path: 'system/change-log',
        name: 'ChangeLogList',
        component: () => import('@/views/system/changelog/list/index.vue'),
        meta: { title: '变更日志', permission: 'system:change-log:list' },
      },
      {
        path: 'system/change-log/detail/:id',
        name: 'ChangeLogDetail',
        component: () => import('@/views/system/changelog/detail/index.vue'),
        meta: { title: '变更详情', permission: 'system:change-log:list' },
      },
      {
        // ==================== 批次 C+（工作任务管理 · T6a） ====================
        // path 与 `sys_menu.path`（V0103 种子 …0760）逐字一致：`/task`。
        // component 字段是 `business/task/index`，对应本文件的
        // `@/views/business/task/index.vue` —— 路由是**手工注册**的（没有 views 自动扫描），
        // 漏注册的表现就是点菜单进 404 / 空白页。
        // 工作任务目录化（V0130）：三个子菜单各自一条路由，共用 index.vue，
        // 通过 `meta.defaultView` 决定初始视图（列表 / 看板）。
        path: 'task/list',
        name: 'BusinessTaskList',
        component: () => import('@/views/business/task/index.vue'),
        meta: { title: '任务列表', permission: 'business:task:read', defaultView: 'list' },
      },
      {
        path: 'task/board',
        name: 'BusinessTaskBoard',
        component: () => import('@/views/business/task/index.vue'),
        meta: { title: '任务看板', permission: 'business:task:read', defaultView: 'board' },
      },
      {
        // V0134 种子菜单 …0902「任务模板」：`/task/templates`，组件 business/task/templates。
        // 权限码沿用 `business:task:read`（模板入口不是权限边界，模板 CRUD 用 create/update/delete 兜）。
        path: 'task/templates',
        name: 'TaskTemplates',
        component: () => import('@/views/business/task/templates.vue'),
        meta: { title: '任务模板', permission: 'business:task:read' },
      },
      {
        // 兼容旧链接 /task（默认列表）
        path: 'task',
        redirect: '/task/list',
      },
      {
        // 兼容通知里的旧链接 `/task/{id}`（V0130 目录化之前的格式）
        // → 跳到列表页并带上 ?id=，由 index.vue 自动打开详情抽屉
        path: 'task/:id',
        redirect: (to) => ({ path: '/task/list', query: { id: to.params.id } }),
      },
      {
        // ==================== 批次 C+（工作任务管理 · T4 甘特图） ====================
        // path 与 `sys_menu.path`（V0110 种子 …0770）逐字一致：`/task/gantt`。
        // **只读**页：拖拽改期属 T7，且改期后必须重跑依赖校验（修正 10）。
        path: 'task/gantt',
        name: 'BusinessTaskGantt',
        component: () => import('@/views/business/task/gantt.vue'),
        meta: { title: '甘特图', permission: 'business:task:read' },
      },
      {
        // ==================== 批次 C+（工作任务管理 · T7c 项目分组） ====================
        // path 与 `sys_menu.path`（V0115 种子 …0775）逐字一致：`/project`。
        // component 字段是 `business/project/index`，对应本文件的
        // `@/views/business/project/index.vue`。
        //
        // ⚠️ 项目页**不是**任务页的子路由：它是独立菜单项（`/project`），
        // 因为「项目」是任务之上的一层分组，而不是任务页的一个 tab。
        path: 'project',
        name: 'BusinessProject',
        component: () => import('@/views/business/project/index.vue'),
        meta: { title: '项目', permission: 'business:project:read' },
      },
      {
        // ==================== 批次 D（工作流审批 · D5） ====================
        // `/flow/def` 与 `sys_menu.path`（V0121 种子 …0780）逐字一致。
        // 注意 V0121 把该菜单种成 `is_visible = FALSE`（D1 当时还没有页面，避免坏链接），
        // 本卡交付页面后可以直接按 URL 访问；是否放进侧栏由 D6 用一条 UPDATE 打开，
        // 前端**不改后端迁移**（卡面「工作方式」）。
        //
        // 权限口径：定义侧两条用 `business:flow:read`（V0121）；
        // 实例侧三条用 D2 的 V0124 码（instance:create / instance:read）。
        // 注意 V0124 只种码不授权 —— 非超管角色需要在「角色管理」里勾选后才看得到这几个页面。
        path: 'flow/def',
        name: 'BusinessFlowDef',
        component: () => import('@/views/business/flow/def/index.vue'),
        meta: { title: '流程定义', permission: 'business:flow:read' },
      },
      {
        // 流程编辑器：三栏（节点面板 / SVG 画布 / 属性面板）。
        // 已发布版本进这里是**只读**的（后端 `version.editable=false` 驱动，不是前端藏按钮）。
        path: 'flow/def/:id/edit',
        name: 'BusinessFlowDefEdit',
        component: () => import('@/views/business/flow/def/edit.vue'),
        meta: { title: '流程编辑器', permission: 'business:flow:read' },
      },
      {
        // 老「流程模板库」入口（P-FLOW-5）已下线（P2-6 决策 A：合并概念，术语统一为「标准流程」）。
        // 卡片墙并入 `/flow/def` 的「标准流程」Tab，这里保留一版 redirect 让老书签 / 老菜单不 404；
        // `sys_menu` 里的节点由 P2-6d 软删（必须等本页上线后才做，否则用户直接失去入口）。
        // 下个版本连同 `sys_menu` 软删一起删掉这条 redirect。
        path: 'flow/templates',
        name: 'BusinessFlowTemplates',
        redirect: '/flow/def?view=standard',
      },
      {
        // 发起审批：只列 PUBLISHED 流程；提交走实例侧接口。
        path: 'flow/start',
        name: 'BusinessFlowStart',
        component: () => import('@/views/business/flow/start/index.vue'),
        meta: { title: '发起审批', permission: 'business:flow:instance:create' },
      },
      {
        // 审批中心：待办 / 已办 / 我发起 / 抄送我的 四 Tab。
        path: 'flow/task',
        name: 'BusinessFlowTask',
        component: () => import('@/views/business/flow/task/index.vue'),
        meta: { title: '审批中心', permission: 'business:flow:instance:read' },
      },
      {
        // 审批详情：进度条 + 时间线 + 操作区。
        path: 'flow/instance/:id',
        name: 'BusinessFlowInstance',
        component: () => import('@/views/business/flow/instance/detail.vue'),
        meta: { title: '审批详情', permission: 'business:flow:instance:read' },
      },
      {
        // ==================== ERP 主数据（W4a · 仓库 / 库位） ====================
        // `sys_menu.path`（V0141 种子 …0941）是 `/business/erp/warehouse`，component
        // `business/erp/warehouse/index`。路由是**手工注册**的（没有 views 自动扫描），
        // 漏注册 = 点菜单进 404。
        //
        // 一个菜单项 = 一个页面：左侧仓库树 + 右侧按仓库过滤的库位列表（同页，不是两个 tab），
        // 所以仓库只注册这一条；库位独立页见下面 `business/erp/location`（P4-B）。
        path: 'business/erp/warehouse',
        name: 'BusinessErpWarehouse',
        component: () => import('@/views/business/erp/warehouse/index.vue'),
        meta: { title: '仓库管理', permission: 'business:erp:warehouse:list' },
      },
      {
        // ==================== ERP 主数据（P4-B · 库位） ====================
        // 用户报的 404 就是这个：`sys_menu`（V0141 种子 …0942）里有「库位管理」
        // `/business/erp/location`，component `business/erp/location/index`，
        // 但前端一直没注册路由（**手工注册，没有 views 自动扫描**）→ 点菜单进 404。
        // 与仓库页的分工：仓库页管「单仓内」的库位增删改；本页是跨仓库总览
        //（服务端分页 + 仓库/类型/启用位筛选 + 详情抽屉），两页共用同一个库位弹窗。
        path: 'business/erp/location',
        name: 'BusinessErpLocation',
        component: () => import('@/views/business/erp/location/index.vue'),
        meta: { title: '库位管理', permission: 'business:erp:location:list' },
      },
      {
        // ==================== ERP 主数据（W4b-1 · 物料） ====================
        // `sys_menu`（V0141 种子 …0943）path = `/business/erp/material`，component
        // `business/erp/material/index`。**路由手工注册**，漏了 = 点菜单进 404。
        path: 'business/erp/material',
        name: 'BusinessErpMaterial',
        component: () => import('@/views/business/erp/material/index.vue'),
        meta: { title: '物料管理', permission: 'business:erp:material:list' },
      },
      {
        // ==================== ERP 主数据（P4-A · 计量单位 / 供应商） ====================
        // `sys_menu`（V0141 种子 …0944 / …0945）path = `/business/erp/unit` / `/business/erp/supplier`，
        // component `business/erp/unit/index` / `business/erp/supplier/index` —— 与下面的
        // 文件路径逐字一致。**路由手工注册**（没有 views 自动扫描），漏注册 = 点菜单进 404
        // （本卡存在的理由就是修这个 404）。
        path: 'business/erp/unit',
        name: 'BusinessErpUnit',
        component: () => import('@/views/business/erp/unit/index.vue'),
        meta: { title: '计量单位', permission: 'business:erp:unit:list' },
      },
      {
        path: 'business/erp/supplier',
        name: 'BusinessErpSupplier',
        component: () => import('@/views/business/erp/supplier/index.vue'),
        meta: { title: '供应商', permission: 'business:erp:supplier:list' },
      },
      {
        // ==================== ERP 序列号（W4b-2） ====================
        // `sys_menu`（V0142 种子 …0981）path = `/business/erp/serial`，component
        // `business/erp/serial/index`。**路由手工注册**，漏了 = 点菜单进 404。
        path: 'business/erp/serial',
        name: 'BusinessErpSerial',
        component: () => import('@/views/business/erp/serial/index.vue'),
        meta: { title: '序列号管理', permission: 'business:erp:serial:list' },
      },
      {
        // ==================== ERP 调拨单（W4c） ====================
        // path 与 V0140 的菜单种子同口径 `/business/erp/transfer`，component
        // `business/erp/transfer/index`。**路由手工注册**（没有 views 自动扫描），
        // 漏了 = 页面永远打不开。菜单种子由后端权限卡补（本卡只碰 frontend）。
        path: 'business/erp/transfer',
        name: 'BusinessErpTransfer',
        component: () => import('@/views/business/erp/transfer/index.vue'),
        meta: { title: '调拨管理', permission: 'business:erp:transfer:list' },
      },
      {
        // ==================== ERP 采购订单（P2-E1） ====================
        // 两条路径都指向同一个页面组件：
        // - `business/erp/purchase-order`：`sys_menu` 种子（V0141 …0953）的 path，点左侧菜单进这里；
        // - `business/erp/purchase`：卡面 §七 的验收路径 + git 目录名（`views/business/erp/purchase/`）。
        // 用 `alias` 而不是两条 route，保证 `name` 唯一、面包屑/高亮不会出现两个同名菜单。
        path: 'business/erp/purchase-order',
        alias: ['business/erp/purchase'],
        name: 'BusinessErpPurchaseOrder',
        component: () => import('@/views/business/erp/purchase/index.vue'),
        meta: { title: '采购订单', permission: 'business:erp:purchase-order:list' },
      },
      {
        // ==================== ERP 合同管理（A5） ====================
        // `sys_menu`（V0197 …1100，V0241 改名前缀）path = `/business/erp/contract`、component
        // `business/erp/contract/index`，**根级页面**（与「采购管理」「基础数据」同级，
        // 不再造「ERP」目录层 —— 用户明确要求过）。**路由手工注册**（没有 views 自动扫描），
        // 漏了 = 点菜单进 404。权限码 P2-4 已统一为 `business:erp:contract:list`
        // （与 ERP 其余 313 个码同族；旧码 `erp:contract:*` 由后端兼容别名继续认）。
        path: 'business/erp/contract',
        name: 'BusinessErpContract',
        component: () => import('@/views/business/erp/contract/index.vue'),
        meta: { title: '合同管理', permission: 'business:erp:contract:list' },
      },
      {
        // ==================== ERP 收货入库（P2-E1） ====================
        // `sys_menu` 种子（V0147 …0998）path = `/business/erp/stock-in`；
        // 卡面验收路径是 `/business/erp/stockin`（目录名），两者都通。
        path: 'business/erp/stock-in',
        alias: ['business/erp/stockin'],
        name: 'BusinessErpStockIn',
        component: () => import('@/views/business/erp/stockin/index.vue'),
        meta: { title: '收货入库', permission: 'business:erp:stock-in:list' },
      },
      {
        // ==================== ERP 出库单（DC-13，领用 / 报废 / 盘亏） ====================
        // `sys_menu` 种子（V0203 …1105）path = `/business/erp/stock-out`、component
        // `business/erp/stock-out/index`，权限码 `business:erp:stock-out:list`。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 点左侧菜单进 404 页。
        // 目录名是 `stock-out`（带连字符），与菜单 path 同口径；不另设 alias，
        // 免得出现第二条同名菜单高亮（P4-D 那个 `alias` 静默跳过导航的坑）。
        path: 'business/erp/stock-out',
        name: 'BusinessErpStockOut',
        component: () => import('@/views/business/erp/stock-out/index.vue'),
        meta: { title: '出库单', permission: 'business:erp:stock-out:list' },
      },
      {
        // ==================== ERP 盘点单（DC-14，ARCH-3 §5） ====================
        // `sys_menu` 种子（V0211 …1111）path = `/business/erp/stock-take`、component
        // `business/erp/stock-take/index`，权限码 `business:erp:stock-take:list`。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 点左侧菜单进 404 ——
        // 这正是 n08（`menu_paths_all_resolve_in_frontend_router`）抓的那类缺陷，
        // 本页就是为它补上的（卡 t_71b01775）。
        path: 'business/erp/stock-take',
        name: 'BusinessErpStockTake',
        component: () => import('@/views/business/erp/stock-take/index.vue'),
        meta: { title: '盘点单', permission: 'business:erp:stock-take:list' },
      },
      {
        // ==================== ERP 固定资产：资产卡片（FIN-2 / ARCH-6 §5.10） ====================
        // `sys_menu` 种子（V0229 …1401）path = `/business/erp/asset`、component
        // `business/erp/asset/index`，权限码 `business:erp:asset:list`。
        // 目录节点（…1400）也挂在 `/business/erp/asset` 上（menu_type=1 无组件），
        // 所以这一条既是「点目录」的落点，也是「点资产卡片」的落点 —— 只注册一条。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 点菜单 404。
        path: 'business/erp/asset',
        name: 'BusinessErpAsset',
        component: () => import('@/views/business/erp/asset/index.vue'),
        meta: { title: '资产卡片', permission: 'business:erp:asset:list' },
      },
      {
        // ==================== ERP 固定资产：资产分类（V0229 …1402） ====================
        // path `/business/erp/asset-category`、component `business/erp/asset-category/index`，
        // 权限码 `business:erp:asset-category:list`。
        path: 'business/erp/asset-category',
        name: 'BusinessErpAssetCategory',
        component: () => import('@/views/business/erp/asset-category/index.vue'),
        meta: { title: '资产分类', permission: 'business:erp:asset-category:list' },
      },
      {
        // ==================== ERP 固定资产：折旧计提（V0230 …1403） ====================
        // path `/business/erp/depreciation`、component `business/erp/depreciation/index`，
        // 权限码 `business:erp:depreciation:list`。
        path: 'business/erp/depreciation',
        name: 'BusinessErpDepreciation',
        component: () => import('@/views/business/erp/depreciation/index.vue'),
        meta: { title: '折旧计提', permission: 'business:erp:depreciation:list' },
      },
      {
        // ==================== ERP 预算（FIN-1 / ARCH-6 第三章） ====================
        // `sys_menu` 种子（V0227）三条菜单 path 与 component 逐字对应：
        //   `/business/erp/budget`          component `business/erp/budget/index`   （预算编制）
        //   `/business/erp/budget/report`   component `business/erp/budget/report`  （预算执行）
        //   `/business/erp/budget/mapping`  component `business/erp/budget/mapping` （预算科目映射）
        // 目录节点（预算管理）也挂在 `/business/erp/budget` 上（menu_type=1 无组件），
        // 所以第一条既是「点目录」的落点，也是「点预算编制」的落点 —— 只注册一条。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 点菜单进 404（卡 t_42d844c0）。
        path: 'business/erp/budget',
        name: 'BusinessErpBudget',
        component: () => import('@/views/business/erp/budget/index.vue'),
        meta: { title: '预算编制', permission: 'business:erp:budget:list' },
      },
      {
        path: 'business/erp/budget/report',
        name: 'BusinessErpBudgetReport',
        component: () => import('@/views/business/erp/budget/report.vue'),
        meta: { title: '预算执行', permission: 'business:erp:budget:report' },
      },
      {
        path: 'business/erp/budget/mapping',
        name: 'BusinessErpBudgetMapping',
        component: () => import('@/views/business/erp/budget/mapping.vue'),
        meta: { title: '预算科目映射', permission: 'business:erp:budget:mapping' },
      },
      {
        // ==================== ERP 费用报销（FIN-3 / ARCH-6 §4.10） ====================
        // `sys_menu` 种子（V0242 …1500/…1501/…1502/…1503）三条菜单 path 与 component 逐字对应：
        //   `/business/erp/expense`       component `business/erp/expense/index`       （我的报销）
        //   `/business/erp/expense/pay`   component `business/erp/expense/pay/index`   （待付款）
        //   `/business/erp/expense-type`  component `business/erp/expense-type/index`  （费用类型）
        // 目录节点（费用报销管理，…1500）也挂在 `/business/erp/expense` 上（menu_type=1 无组件），
        // 所以第一条既是「点目录」的落点，也是「点我的报销」的落点 —— 只注册一条。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 菜单可见但点进去 404（FIN-1 的坑）。
        path: 'business/erp/expense',
        name: 'BusinessErpExpense',
        component: () => import('@/views/business/erp/expense/index.vue'),
        meta: { title: '我的报销', permission: 'business:erp:expense:list' },
      },
      {
        path: 'business/erp/expense/pay',
        name: 'BusinessErpExpensePay',
        component: () => import('@/views/business/erp/expense/pay/index.vue'),
        meta: { title: '待付款', permission: 'business:erp:expense:pay' },
      },
      {
        path: 'business/erp/expense-type',
        name: 'BusinessErpExpenseType',
        component: () => import('@/views/business/erp/expense-type/index.vue'),
        meta: { title: '费用类型', permission: 'business:erp:expense-type:list' },
      },
      {
        // ==================== ERP 应付单（P2-E2） ====================
        // `sys_menu` 里**还没有**这条菜单：V0148 只建了 `biz_payable`，既没种
        // `business:erp:payable:*` 权限码也没种菜单节点（`grep -rn 'erp/payable'
        // backend/migrations/` 0 命中）。所以本页暂时只能按 URL 进（卡面验收路径就是
        // `/business/erp/payable`），侧边栏要等后端补 V0151 才出现 —— 这是在
        // P2-E2 完成说明里点明的后端缺口，前端不做假菜单。
        path: 'business/erp/payable',
        name: 'BusinessErpPayable',
        component: () => import('@/views/business/erp/payable/index.vue'),
        meta: { title: '应付单', permission: 'business:erp:payable:list' },
      },
      {
        // ==================== ERP 凭证查询（P2-E2 / 后端 P2-D1） ====================
        // `sys_menu` 种子（V0150 …1004）path = `/business/erp/voucher`，component
        // `business/erp/voucher/index`。**路由手工注册**（没有 views 自动扫描），漏了就 404。
        path: 'business/erp/voucher',
        name: 'BusinessErpVoucher',
        component: () => import('@/views/business/erp/voucher/index.vue'),
        meta: { title: '凭证查询', permission: 'business:erp:voucher:list' },
      },
      {
        // ==================== ERP 总账 / 科目余额表（P2-E2 / 后端 P2-D1） ====================
        // `sys_menu` 种子（V0150 …1007）path = `/business/erp/ledger`，component
        // `business/erp/ledger/index`。两个 Tab（流水 / 余额表）在同一个页面里。
        path: 'business/erp/ledger',
        name: 'BusinessErpLedger',
        component: () => import('@/views/business/erp/ledger/index.vue'),
        meta: { title: '总账 / 科目余额表', permission: 'business:erp:ledger:list' },
      },
      {
        // ==================== ERP 资产负债表（ERP-R1） ====================
        // `sys_menu` 种子（V0183 …1015）path = `/business/erp/report/balance-sheet`，
        // component `business/erp/report/balance-sheet/index`；导出按钮 …1016
        // 用 `business:erp:report:export`。**路由手工注册**，漏了 = 点菜单 404。
        path: 'business/erp/report/balance-sheet',
        name: 'BusinessErpBalanceSheet',
        component: () => import('@/views/business/erp/report/balance-sheet/index.vue'),
        meta: { title: '资产负债表', permission: 'business:erp:report:balance-sheet' },
      },
      {
        // ==================== ERP 利润表（ERP-R2） ====================
        // `sys_menu` 种子（V0184 …1017）path = `/business/erp/report/income-statement`，
        // component `business/erp/report/income-statement/index`；导出按钮复用
        // `business:erp:report:export`。**路由手工注册**，漏了 = 点菜单 404。
        path: 'business/erp/report/income-statement',
        name: 'BusinessErpIncomeStatement',
        component: () => import('@/views/business/erp/report/income-statement/index.vue'),
        meta: { title: '利润表', permission: 'business:erp:report:income-statement' },
      },
      {
        // ==================== ERP 现金流量表（ERP-R3，简化版） ====================
        // `sys_menu` 种子（V0185 …1018）path = `/business/erp/report/cash-flow`，
        // component `business/erp/report/cash-flow/index`；导出按钮复用
        // `business:erp:report:export`。**路由手工注册**，漏了 = 点菜单 404。
        path: 'business/erp/report/cash-flow',
        name: 'BusinessErpCashFlow',
        component: () => import('@/views/business/erp/report/cash-flow/index.vue'),
        meta: { title: '现金流量表', permission: 'business:erp:report:cash-flow' },
      },
      {
        // ==================== ERP 会计科目（P4-C，只读树形） ====================
        // `sys_menu` 种子（V0141 …0946）path = `/business/erp/account`，component
        // `business/erp/account/index`；V0154 把菜单挂到了「财务管理」目录下，path 没变。
        // **路由手工注册**（没有 views 自动扫描），漏了 = 点菜单进 404（本卡就是来补这个洞的）。
        path: 'business/erp/account',
        name: 'BusinessErpAccount',
        component: () => import('@/views/business/erp/account/index.vue'),
        meta: { title: '会计科目', permission: 'business:erp:account:list' },
      },
      {
        // ==================== ERP 会计期间（P4-C，只读） ====================
        // `sys_menu` 种子（V0141 …0947）path = `/business/erp/period`，component
        // `business/erp/period/index`；同样手工注册，漏了就 404。
        path: 'business/erp/period',
        name: 'BusinessErpPeriod',
        component: () => import('@/views/business/erp/period/index.vue'),
        meta: { title: '会计期间', permission: 'business:erp:period:list' },
      },
      {
        // ==================== ERP 期初余额（ERP-R5） ====================
        // `sys_menu`（V0189 种子 …1022）path = `/business/erp/opening`，component
        // `business/erp/opening/index`；**路由手工注册**，漏了 = 点菜单进 404。
        path: 'business/erp/opening',
        name: 'BusinessErpOpening',
        component: () => import('@/views/business/erp/opening/index.vue'),
        meta: { title: '期初余额', permission: 'business:erp:opening:read' },
      },
      {
        // ==================== ERP 年度结转（ERP-R4） ====================
        // `sys_menu`（V0190 种子 …1025）path = `/business/erp/year-end`，component
        // `business/erp/year-end/index`；**路由手工注册**，漏了 = 点菜单进 404。
        //
        // 注意：`component` 里写的是前端文件路径 `year-end`，与后端路由
        // `/business/erp/year-end/*` 是两套命名空间（前者是文件目录，后者是 API）。
        path: 'business/erp/year-end',
        name: 'BusinessErpYearEnd',
        component: () => import('@/views/business/erp/year-end/index.vue'),
        meta: { title: '年度结转', permission: 'business:erp:yearend:read' },
      },
      {
        // ==================== ERP 即时库存 / 库位库存 / 库存流水（P4-D） ====================
        // `sys_menu`（V0141 种子 …0946 / …0947 / …0948）是**三条**菜单：
        // `/business/erp/inventory`、`/business/erp/inventory-location`、`/business/erp/inventory-txn`，
        // 但前端只交**一个**页面（三个 Tab 同页），且路由是**手工注册**的 —— 三条都没注册，
        // 点哪个菜单都 404（P4-D 要修的就是这个）。
        //
        // 三条各注册一条 route（**不用 alias**）：alias 在 vue-router 4 里与本体共用同一条
        // matched record，从 `/inventory` 点菜单去 `/inventory-location` 会被判定为「同一路由」
        // 而静默跳过导航（实测：点 Tab 改 URL 无效）—— 三条独立 record 才能让侧边栏菜单、
        // 地址栏、当前 Tab 三处同步。`meta.tab` 告诉页面进来先激活哪个 Tab。
        //
        // 权限码三个 Tab 各一条（V0141 已种），各自挂在自己的 meta 上：
        // 缺哪个 Tab 的权限，就只在那个 Tab 里报错，不会整页白屏。
        path: 'business/erp/inventory',
        name: 'BusinessErpInventory',
        component: () => import('@/views/business/erp/inventory/index.vue'),
        meta: { title: '即时库存', permission: 'business:erp:inventory:list', tab: 'stock' },
      },
      {
        path: 'business/erp/inventory-location',
        name: 'BusinessErpInventoryLocation',
        component: () => import('@/views/business/erp/inventory/index.vue'),
        meta: { title: '库位库存', permission: 'business:erp:inventory-location:list', tab: 'location' },
      },
      {
        path: 'business/erp/inventory-txn',
        name: 'BusinessErpInventoryTxn',
        component: () => import('@/views/business/erp/inventory/index.vue'),
        meta: { title: '库存流水', permission: 'business:erp:inventory-txn:list', tab: 'txn' },
      },
      {
        // ==================== ERP 采购明细（卡 t_6f023cee / BUG-SYS-01） ====================
        // `sys_menu` 的 path `/business/erp/purchase-order-item`（menu_type=2、is_visible=true）
        // 一直可见，但路由与组件都缺 → 点侧栏 404（测试C 的 n08 用例钉住了这条不变量）。
        // 后端 `GET /business/erp/purchase-order-item/page` + `/{id}` 自 V0141 起就齐全。
        path: 'business/erp/purchase-order-item',
        name: 'BusinessErpPurchaseOrderItem',
        component: () => import('@/views/business/erp/purchase-order-item/index.vue'),
        meta: { title: '采购明细', permission: 'business:erp:purchase-order-item:list' },
      },
      {
        // ==================== ERP 单号序列（卡 t_6f023cee / BUG-SYS-01） ====================
        // 同上：`/business/erp/seq` 的菜单可见但没注册路由；后端只有只读的
        // `/seq/page` 与 `/seq/{id}`（没有写接口），页面因此做成只读台账。
        path: 'business/erp/seq',
        name: 'BusinessErpSeq',
        component: () => import('@/views/business/erp/seq/index.vue'),
        meta: { title: '单号序列', permission: 'business:erp:seq:list' },
      },
      {
        path: 'result/403',
        name: 'Result403',
        component: () => import('@/views/result/403/index.vue'),
        meta: { title: '无访问权限' },
      },
      {
        path: 'result/404',
        name: 'Result404',
        component: () => import('@/views/result/404/index.vue'),
        meta: { title: '页面不存在' },
      },
      {
        path: 'result/500',
        name: 'Result500',
        component: () => import('@/views/result/500/index.vue'),
        meta: { title: '服务异常' },
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'NotFound',
    component: () => import('@/views/result/404/index.vue'),
    meta: { title: '页面不存在', public: true },
  },
];

export const router = createRouter({
  history: createWebHistory(import.meta.env.VITE_APP_CONTEXT_PATH || '/'),
  routes,
});

// 全局前置守卫：未登录跳登录页；已登录访问登录页跳首页；并校验路由权限码
router.beforeEach(async (to) => {
  const userStore = useUserStore();
  const isPublic = to.meta.public === true;

  if (!userStore.token) {
    if (isPublic) {
      return true;
    }
    return { path: '/login', query: { redirect: to.fullPath } };
  }

  // 刷新页面后 Pinia 状态为空，需先补齐用户信息与菜单
  if (!userStore.loaded) {
    try {
      await userStore.loadProfile();
    } catch {
      userStore.reset();
      return { path: '/login', query: { redirect: to.fullPath } };
    }
  }

  if (to.path === '/login') {
    return { path: '/dashboard' };
  }

  // 首登强制改密：后端此时只放行 me / menus / password / logout，
  // 前端必须把用户钉在改密页，任何其它路由一律重定向（不能靠用户自觉）。
  if (userStore.mustChangePassword) {
    return to.path === '/change-password' ? true : { path: '/change-password' };
  }
  if (to.path === '/change-password') {
    // 无需改密时改密页无意义（个人中心已有改密入口），回工作台
    return { path: '/dashboard' };
  }

  const permission = to.meta.permission;
  if (permission && !userStore.hasPermission(permission)) {
    return { path: '/result/403' };
  }

  return true;
});

// 同步页面标题，便于多标签页区分
router.afterEach((to) => {
  const title = to.meta.title;
  document.title = typeof title === 'string' ? `${title} - 云枢 CloudPivot` : '云枢 CloudPivot';
});

export default router;