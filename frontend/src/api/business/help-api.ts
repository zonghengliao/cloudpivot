/**
 * 帮助文档接口（批次 C · C4）。
 *
 * 路径与后端 `backend/src/business/help/handler.rs` 逐条对应。
 *
 * # 四条口径
 *
 * 1. **权限码是 `system:help:*`**（不是 PRD 里写的 `business:help:*`）：后端注解与
 *    `V0088` 种子都用 `system:help:*`，前端一律以后端为准（偏差记入 C9 summary）。
 * 2. **正文渲染只在后端做**：`content_html` 由 `Recipe::Help` 生成（heading 带
 *    `h-{level}-{index}` 锚点，TOC 跳转依赖它），前端**不得**自己拼 HTML；
 *    `toc` 也不落库，直接取详情的 `toc` 字段。
 * 3. **检索是 `ILIKE` + 转义**，`q` 传原文即可（后端负责转义 `%` `_`）。
 * 4. **编辑已发布文档会自动退回 `DRAFT`**（后端 PUT 行为），页面需提示用户重新发布。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/**
 * 把强类型查询对象交给 axios 的 `params`。
 *
 * 两点处理（都踩过）：
 * 1. `Record<string, unknown>` 需要索引签名，接口类型没有，所以做一次断言；
 * 2. **丢掉空串 / null / undefined**：后端把 `?status=` 当成「status 等于空串」的过滤条件
 *    （实测 `help/doc/page?status=` 返回 0 条），页面里 `query.status = ''` 表示「不过滤」，
 *    必须不发这个参数。
 */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 分类树节点（对应后端 `HelpCategoryNodeRes`） */
export interface HelpCategoryNode {
  /** 分类 id */
  id: string;
  /** 父分类 id（根为 null） */
  parent_id: string | null;
  /** 名称 */
  name: string;
  /** 编码（唯一） */
  code: string;
  /** 图标名 */
  icon: string;
  /** 描述 */
  description: string;
  /** 排序号 */
  sort_order: number;
  /** 状态：1 启用 / 0 停用 */
  status: number;
  /** 已发布文档数 */
  doc_count: number;
  /** 子分类 */
  children: HelpCategoryNode[];
}

/** 分类树查询 */
export interface CategoryTreeQuery {
  /** 是否包含空分类 */
  include_empty?: boolean;
  /** 是否包含停用分类 */
  include_disabled?: boolean;
}

/** 分类保存入参（对应后端 `CategorySaveReq`） */
export interface CategorySaveReq {
  /** 父分类（空=根，深度上限 3 层由后端递归 CTE 保证） */
  parent_id?: string | null;
  /** 名称 */
  name: string;
  /** 编码 */
  code: string;
  /** 图标 */
  icon?: string;
  /** 描述 */
  description?: string;
  /** 排序号 */
  sort_order?: number;
  /** 状态 */
  status?: number;
}

/** 文档分页查询（对应后端 `DocPageQuery`） */
export interface DocPageQuery {
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 分类 id */
  category_id?: string;
  /** 是否含子分类文档 */
  include_children?: boolean;
  /** 标题 / 摘要 / 正文关键字 */
  keyword?: string;
  /** 状态过滤（`DRAFT` / `PUBLISHED` / `OFFLINE`） */
  status?: string;
  /** 是否包含未发布（管理侧才可传 true） */
  include_unpublished?: boolean;
}

/** 文档卡片（对应后端 `HelpDocCardRes`） */
export interface HelpDocCard {
  /** 文档 id */
  id: string;
  /** 标题 */
  title: string;
  /** 唯一 slug */
  slug: string;
  /** 摘要 */
  summary: string;
  /** 分类 id */
  category_id: string;
  /** 分类名 */
  category_name: string;
  /** 关键字（逗号分隔） */
  keywords: string;
  /** 版本号 */
  version: number;
  /** 状态 */
  status: string;
  /** 排序号 */
  sort_order: number;
  /** 阅读量 */
  view_count: number;
  /** 发布时间 */
  published_at: string | null;
  /** 更新时间 */
  updated_at: string;
}

/** TOC 节点（对应后端 `HelpTocRes`） */
export interface HelpTocNode {
  /** 标题级别 1–4 */
  level: number;
  /** 标题文本 */
  text: string;
  /** 锚点 id（`h-{level}-{index}`） */
  id: string;
  /** 子节点 */
  children: HelpTocNode[];
}

/** 文档详情（对应后端 `HelpDocDetailRes`） */
export interface HelpDocDetail {
  /** 文档 id */
  id: string;
  /** 标题 */
  title: string;
  /** slug */
  slug: string;
  /** 摘要 */
  summary: string;
  /** 分类 id */
  category_id: string;
  /** 分类名 */
  category_name: string;
  /** 关键字 */
  keywords: string;
  /** Markdown 原文 */
  content_md: string;
  /** 后端渲染并净化的 HTML */
  content_html: string;
  /** 目录树（后端生成，不落库） */
  toc: HelpTocNode[];
  /** 版本号 */
  version: number;
  /** 状态 */
  status: string;
  /** 阅读量 */
  view_count: number;
  /** 发布时间 */
  published_at: string | null;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 文档保存入参（对应后端 `DocSaveReq`） */
export interface DocSaveReq {
  /** 分类 id（必填） */
  category_id: string;
  /** 标题 */
  title: string;
  /** slug（唯一，留空由后端按标题生成） */
  slug: string;
  /** 摘要 */
  summary?: string;
  /** Markdown 正文 */
  content_md: string;
  /** 关键字 */
  keywords?: string;
  /** 排序号 */
  sort_order?: number;
}

/** 变更说明入参（对应后端 `ChangeNoteReq`） */
export interface ChangeNoteReq {
  /** 变更说明（发布 / 回滚时写进版本快照） */
  change_note?: string;
}

/** 版本快照（对应后端 `HelpVersionRes`） */
export interface HelpVersion {
  /** 版本号 */
  version: number;
  /** 该版本标题 */
  title: string;
  /** 变更说明 */
  change_note: string;
  /** 创建时间 */
  created_at: string;
  /** 创建人 */
  create_by: string;
  /** 创建人姓名（AUD-02 实时取值；空串 = 账号已不存在，显示 `—`） */
  create_by_name?: string;
  /** 正文字节数 */
  content_bytes: number;
}

/** 检索查询（对应后端 `HelpSearchQuery`） */
export interface HelpSearchQuery {
  /** 关键字 */
  q?: string;
  /** 页码 */
  page?: number;
  /** 每页条数 */
  page_size?: number;
  /** 状态过滤 */
  status?: string;
  /** 分类 id */
  category_id?: string;
}

/** 检索命中（对应后端 `HelpSearchHitRes`） */
export interface HelpSearchHit {
  /** 文档 id */
  id: string;
  /** 标题 */
  title: string;
  /** slug */
  slug: string;
  /** 摘要 */
  summary: string;
  /** 分类名 */
  category_name: string;
  /** 命中片段（含高亮标记） */
  snippet: string;
  /** 命中词 */
  matched_terms: string[];
  /** 状态 */
  status: string;
  /** 发布时间 */
  published_at: string | null;
}

/** 阅读量上报结果（对应后端 `HelpReadRes`） */
export interface HelpReadResult {
  /** 当前阅读量 */
  view_count: number;
  /** 本次是否真的 +1（去重窗口内为 false） */
  counted: boolean;
}

// ===== 分类树 =====

/** 分类树（用户侧 / 管理侧共用；`system:help:read` 即可读） */
export function getCategoryTree(query: CategoryTreeQuery = {}): Promise<HelpCategoryNode[]> {
  return get<HelpCategoryNode[]>('/api/business/help/category/tree', asParams(query));
}

/** 平铺分类列表（含停用；`system:help:category:list`） */
export function listCategories(query: CategoryTreeQuery = {}): Promise<HelpCategoryNode[]> {
  return get<HelpCategoryNode[]>('/api/business/help/category/list', asParams(query));
}

/** 新建分类（`system:help:category:create`） */
export function createCategory(data: CategorySaveReq): Promise<HelpCategoryNode> {
  return post<HelpCategoryNode>('/api/business/help/category', data);
}

/** 编辑 / 移动分类（`system:help:category:update`） */
export function updateCategory(id: string, data: CategorySaveReq): Promise<HelpCategoryNode> {
  return put<HelpCategoryNode>(`/api/business/help/category/${id}`, data);
}

/** 删除分类（`system:help:category:delete`，分类下有文档会被拒） */
export function deleteCategory(id: string): Promise<null> {
  return del<null>(`/api/business/help/category/${id}`);
}

// ===== 文档 =====

/** 文档分页（用户侧只出 `PUBLISHED`；管理侧 `include_unpublished=true`） */
export function pageDocs(query: DocPageQuery = {}): Promise<PageData<HelpDocCard>> {
  return get<PageData<HelpDocCard>>('/api/business/help/doc/page', asParams(query));
}

/** 文档详情（含 `content_html` 与 `toc`） */
export function getDoc(id: string): Promise<HelpDocDetail> {
  return get<HelpDocDetail>(`/api/business/help/doc/${id}`);
}

/** 新建文档（草稿，`system:help:doc:create`） */
export function createDoc(data: DocSaveReq): Promise<HelpDocDetail> {
  return post<HelpDocDetail>('/api/business/help/doc', data);
}

/** 编辑文档（`system:help:doc:update`；已发布文档会被退回草稿） */
export function updateDoc(id: string, data: DocSaveReq): Promise<HelpDocDetail> {
  return put<HelpDocDetail>(`/api/business/help/doc/${id}`, data);
}

/** 删除文档（软删，`system:help:doc:delete`） */
export function deleteDoc(id: string): Promise<null> {
  return del<null>(`/api/business/help/doc/${id}`);
}

/** 发布文档（版本号 +1 并写快照，`system:help:doc:publish`） */
export function publishDoc(id: string, data: ChangeNoteReq = {}): Promise<HelpDocDetail> {
  return post<HelpDocDetail>(`/api/business/help/doc/${id}/publish`, data);
}

/** 版本列表（新 → 旧，`system:help:doc:list`） */
export function listDocVersions(id: string): Promise<HelpVersion[]> {
  return get<HelpVersion[]>(`/api/business/help/doc/${id}/versions`);
}

/** 回滚到指定版本（生成新版本，历史不删，`system:help:doc:publish`） */
export function rollbackDoc(id: string, version: number, data: ChangeNoteReq = {}): Promise<HelpDocDetail> {
  return post<HelpDocDetail>(`/api/business/help/doc/${id}/versions/${version}/rollback`, data);
}

/** 全文检索（`ILIKE` + 转义，管理侧可传 `include_unpublished` 对应的 `status`） */
export function searchDocs(query: HelpSearchQuery): Promise<PageData<HelpSearchHit>> {
  return get<PageData<HelpSearchHit>>('/api/business/help/search', asParams(query));
}

/** 上报阅读量（去重窗口内不重复计数） */
export function markDocRead(id: string): Promise<HelpReadResult> {
  return put<HelpReadResult>(`/api/business/help/doc/${id}/read`);
}

/** 帮助文档接口集合 */
export const helpApi = {
  getCategoryTree,
  listCategories,
  createCategory,
  updateCategory,
  deleteCategory,
  pageDocs,
  getDoc,
  createDoc,
  updateDoc,
  deleteDoc,
  publishDoc,
  listDocVersions,
  rollbackDoc,
  searchDocs,
  markDocRead,
};
