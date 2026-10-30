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
        // 所以只注册这一条；库位菜单（…0942 `/business/erp/location`）留给 W4 的库位独立卡。
        path: 'business/erp/warehouse',
        name: 'BusinessErpWarehouse',
        component: () => import('@/views/business/erp/warehouse/index.vue'),
        meta: { title: '仓库管理', permission: 'business:erp:warehouse:list' },
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