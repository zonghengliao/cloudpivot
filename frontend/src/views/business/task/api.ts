/**
 * 任务页接口出口（薄壳）。
 *
 * 项目约定接口封装放 `frontend/src/api/business/<模块>-api.ts`
 * （见 `docs/前端二开指南.md` §一.1），这里只做再导出：
 * 页面同目录引用即可，也让本目录「列表 + 详情 + 表单 + 接口」自解释。
 *
 * 真正的实现与字段注释在 `@/api/business/task-api`。
 */
export * from '@/api/business/task-api';
