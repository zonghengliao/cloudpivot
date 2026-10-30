/**
 * 菜单徽标接口（批次 E · E16）。
 *
 * 路径与后端 `backend/src/system/badge/handler.rs` 的
 * `#[nik::get("/system/badge/counts")]` 逐字对应 —— 注解里**不带** `/api`，
 * 前端必须写全（漏了会命中 SPA fallback 返回 HTML，报「响应体格式不符合」）。
 *
 * # 四个桶
 *
 * | 字段 | 含义 | 出现在哪个菜单 |
 * |------|------|----------------|
 * | `announcement_unread` | 公告未读 | 公告中心 |
 * | `notice_unread` | 通知未读 | 消息中心 |
 * | `flow_todo` | 待我审批 | 审批中心 |
 * | `task_todo` | 待我处理（7 天内） | 任务列表 / 看板 / 甘特图（**共用同一个数**） |
 *
 * 值为 0 是正常返回（不是字段缺失）；>99 由 `t-badge` 的 `max-count` 渲染成 `99+`。
 */
import { get } from '@/api/request';

/** 四类徽标计数（与后端 `BadgeCounts` 逐字同名） */
export interface BadgeCounts {
  /** 公告未读（可见 ∧ 未过期 − 已读） */
  announcement_unread: number;
  /** 通知未读（`read_flag = FALSE`） */
  notice_unread: number;
  /** 待我审批（`assignee_id = me AND status = 'PENDING'`） */
  flow_todo: number;
  /** 待我处理（`owner_id = me AND status = 'TODO'` 且 7 天内创建） */
  task_todo: number;
}

/** 全 0 基线：接口失败 / 尚未拉到数据时用它，页面永远拿到四个数字（不判 undefined） */
export const EMPTY_BADGE_COUNTS: BadgeCounts = {
  announcement_unread: 0,
  notice_unread: 0,
  flow_todo: 0,
  task_todo: 0,
};

/** 拉取当前登录用户的四类徽标计数 */
export function getBadgeCounts(): Promise<BadgeCounts> {
  return get<BadgeCounts>('/api/system/badge/counts');
}
