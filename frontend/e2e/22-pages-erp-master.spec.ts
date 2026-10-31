import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/**
 * ERP 主数据 6 页（测试E · 52 页全量）：物料 / 仓库 / 库位 / 计量单位 / 供应商 / 序列号。
 *
 * 每页 6 条 `it`（① 能开 ② 内容 ③ 列表接口自洽 ④ 按钮可达 ⑤ 按钮有反应 ⑥ 空提交被拦），
 * 见 `_pagerunner.ts` 的口径说明。**库位 404 / 计量单位 404 / 会计科目 404** 是老板踩过的
 * 老问题，这三页在这里是真开、真点、真断言的。
 */
definePageChecks(
  pages(['物料管理', '仓库管理', '库位管理', '计量单位', '供应商', '序列号管理']),
);
