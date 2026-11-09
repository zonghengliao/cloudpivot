/**
 * ERP 单据出参三段式口径 —— `data.meta` 段（E2E01-FIX-4）。
 *
 * 后端唯一定义处：`backend/src/business/erp/doc_shape.rs::DocMeta`；
 * 规范全文：`docs/编码规范.md` §1.12「ERP 接口出参口径」。
 *
 * # 口径
 *
 * ```text
 * data.header  单据主表（单头）
 * data.items   单据明细数组（没有明细给 []，不给 null）
 * data.meta    其他附属信息（本类型；永远存在，最差是 {}）
 * ```
 *
 * `meta` 里三个固定键是本文件声明的（审批实例三元组），其余键按接口自取
 * （`merged_count` / `quote_count` / `can_to_po` …），所以带一个索引签名兜住。
 * 读自定义键请显式断言或走可选链，不要假设一定有。
 */
export interface DocMeta {
  /** 审批实例 id（单据进过审批才有） */
  instance_id?: string;
  /** 审批实例单号（`FLW-…`） */
  instance_no?: string;
  /** 审批实例状态（与后端 `biz_flow_instance.status` 逐字一致） */
  flow_status?: 'PENDING' | 'APPROVED' | 'REJECTED' | 'CANCELLED' | 'WITHDRAWN';
  /** 接口自定义的附属信息（后端平铺进来，不是嵌套在 `extra` 里） */
  [key: string]: unknown;
}
