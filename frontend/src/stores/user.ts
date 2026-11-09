import { computed, ref } from 'vue';
import { defineStore } from 'pinia';
import {
  fetchMe,
  fetchMenus,
  login as loginApi,
  logout as logoutApi,
  type CurrentUserRes,
  type LoginParams,
  type MenuNode,
} from '@/api/system/auth-api';
import { clearTokens, REFRESH_TOKEN_KEY, saveTokens, TOKEN_KEY } from '@/api/request';
import { syncPreferenceFromServer } from '@/composables/usePalette';
import { clearLoginUnreadShown } from '@/utils/login-unread';

/** 超级管理员通配权限码 */
const SUPER_PERMISSION = '*:*:*';

/**
 * 超级管理员角色编码（与后端 `constants::SUPER_ADMIN_ROLE_CODE` 逐字一致）。
 *
 * 判定口径照后端 `system::auth::model::is_super_admin`：角色码列表里含它即为超管，
 * 后端据此短路全部权限校验并下发 `*:*:*`。前端不能自创第二套判定，
 * 否则「首页显示系统信息但接口 403」这类错位会从这个缺口长出来。
 */
const SUPER_ADMIN_ROLE_CODE = 'SUPER_ADMIN';

/** 参与导航渲染的菜单类型：目录与菜单（按钮类型仅作为权限点） */
const NAV_MENU_TYPES: number[] = [1, 2];

/** 递归过滤菜单树：剔除隐藏节点、按钮节点以及当前用户无权限的节点 */
function filterNavMenus(nodes: MenuNode[], hasPermission: (code: string) => boolean): MenuNode[] {
  const result: MenuNode[] = [];
  for (const node of nodes) {
    if (!NAV_MENU_TYPES.includes(node.menu_type) || !node.is_visible) {
      continue;
    }
    if (!hasPermission(node.auth_code)) {
      continue;
    }
    const children = filterNavMenus(node.children ?? [], hasPermission);
    // 目录下若无可见子菜单则整体隐藏，避免出现空目录
    if (node.menu_type === 1 && children.length === 0) {
      continue;
    }
    result.push({ ...node, children });
  }
  return result;
}

/** 用户会话状态：令牌、当前用户、权限与导航菜单 */
export const useUserStore = defineStore('user', () => {
  /** 访问令牌 */
  const token = ref(localStorage.getItem(TOKEN_KEY) ?? '');
  /** 刷新令牌 */
  const refreshToken = ref(localStorage.getItem(REFRESH_TOKEN_KEY) ?? '');
  /** 当前登录用户信息 */
  const userInfo = ref<CurrentUserRes | null>(null);
  /** 后端下发的菜单树 */
  const menus = ref<MenuNode[]>([]);
  /** 是否已完成用户信息加载（路由守卫据此决定是否拉取） */
  const loaded = ref(false);
  /** 是否必须先修改初始密码（后端 must_change_password，登录与刷新都以此为准） */
  const mustChangePassword = ref(false);

  /** 权限码列表 */
  const permissions = computed<string[]>(() => userInfo.value?.permissions ?? []);
  /** 角色编码列表 */
  const roles = computed<string[]>(() => userInfo.value?.roles ?? []);
  /**
   * 是否超级管理员（首页「系统信息」Tab 与服务器状态入口的开关）。
   *
   * 只认后端下发的角色码，与 `system::auth::model::is_super_admin` 同口径；
   * 不用 `hasPermission('*:*:*')`——那是权限层的推论，角色层才是语义源头。
   */
  const isSuperAdmin = computed<boolean>(() => roles.value.includes(SUPER_ADMIN_ROLE_CODE));
  /** 顶栏展示名称，优先真实姓名 */
  const displayName = computed<string>(() => userInfo.value?.real_name || userInfo.value?.username || '');
  /** 侧边栏菜单树（已按可见性与权限过滤） */
  const navMenus = computed<MenuNode[]>(() => filterNavMenus(menus.value, hasPermission));

  /**
   * 判断是否拥有某个权限码。
   *
   * 未传 code 视为无需校验；持有 `*:*:*` 的超级管理员恒为 true。
   */
  function hasPermission(code?: string | null): boolean {
    if (!code) {
      return true;
    }
    const list = permissions.value;
    return list.includes(SUPER_PERMISSION) || list.includes(code);
  }

  /** 登录并加载用户信息与菜单 */
  async function login(params: LoginParams): Promise<void> {
    const res = await loginApi(params);
    token.value = res.access_token;
    refreshToken.value = res.refresh_token;
    // 令牌必须同步落盘：后续请求的 Authorization 头由 axios 拦截器从 localStorage 注入
    saveTokens(res.access_token, res.refresh_token);
    // 首登强制改密：后端此时只放行 me / menus / password / logout
    mustChangePassword.value = res.must_change_password === true;
    await loadProfile();
  }

  /** 拉取当前用户信息与菜单树，供路由守卫与布局使用 */
  async function loadProfile(): Promise<void> {
    const [me, menuTree] = await Promise.all([fetchMe(), fetchMenus()]);
    userInfo.value = me;
    // 刷新页面后 Pinia 状态为空，强制改密状态以服务端为准（不能靠前端记忆）
    mustChangePassword.value = me.must_change_password === true;
    menus.value = menuTree;
    loaded.value = true;
    // 登录后拉取后端偏好并校正本地（加载优先级：后端 > localStorage > 默认）。
    // 失败只 warn：偏好同步绝不能阻塞登录，也不能让页面打不开（验收第 4 条 fail-safe）。
    try {
      await syncPreferenceFromServer();
    } catch (error) {
      console.warn('[user] 偏好同步失败（继续使用本地偏好）：', error);
    }
  }

  /** 清空本地会话状态 */
  function reset(): void {
    clearTokens();
    token.value = '';
    refreshToken.value = '';
    userInfo.value = null;
    menus.value = [];
    loaded.value = false;
    mustChangePassword.value = false;
  }

  /** 登出：无论后端是否成功，本地会话必须清理 */
  async function logout(): Promise<void> {
    try {
      // 必须带上 refresh_token：后端只有拿到它才能把刷新令牌一并拉黑
      await logoutApi(refreshToken.value || undefined);
    } catch {
      // 后端登出失败（如令牌已过期）不影响前端清理，忽略即可
    }
    reset();
    // 清「登录未读提示已弹过」标记：下次登录要重新提示（不清的话同一标签页里换账号登录就再也不弹）
    clearLoginUnreadShown();
  }

  return {
    token,
    refreshToken,
    userInfo,
    menus,
    loaded,
    mustChangePassword,
    permissions,
    roles,
    isSuperAdmin,
    displayName,
    navMenus,
    hasPermission,
    login,
    loadProfile,
    logout,
    reset,
  };
});