/**
 * 仓库 / 库位页用到的权限码。
 *
 * # 为什么不放 `src/constants/permissions.ts`
 *
 * 那个文件是全站共用的单点，ERP 这一批（仓库 / 库位 / 物料 / 单位 / 供应商）**四五张卡会同时往里加**，
 * 谁后提交谁撞冲突。本页只用到这 6 个码，就近放在自己的目录里，改起来不碰别人的行。
 *
 * 码值与 `backend/migrations/business/V0141__erp_permissions.up.sql` 的 `auth_code` 逐字一致
 * （后端 `#[nik::permission(permission = "...")]` 同源）。
 */
export const WAREHOUSE_PERM = {
  /** 列表 / 树（页面入口） */
  warehouseList: 'business:erp:warehouse:list',
  /** 新建仓库 */
  warehouseCreate: 'business:erp:warehouse:create',
  /** 编辑仓库 */
  warehouseUpdate: 'business:erp:warehouse:update',
  /** 库位列表（本页右侧列表） */
  locationList: 'business:erp:location:list',
  /** 新建库位 */
  locationCreate: 'business:erp:location:create',
  /** 编辑库位 */
  locationUpdate: 'business:erp:location:update',
  /** 删除库位 */
  locationDelete: 'business:erp:location:delete',
} as const;
