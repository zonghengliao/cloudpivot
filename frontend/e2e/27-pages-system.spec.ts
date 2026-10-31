import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** 系统管理 8 页（脱敏 / 互斥 / 变更日志走真实路由，卡面旧路径见专项 1 回归）。 */
definePageChecks(
  pages(['用户管理', '角色管理', '菜单管理', '部门管理', '数据权限', '字段脱敏', '角色互斥', '变更日志']),
);
