import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** 公告 / 消息 / 帮助 共 7 页。公告中心与消息中心是卡片流（非表格），走 custom 口径。 */
definePageChecks(
  pages(['公告中心', '公告管理', '消息中心', '消息发送', '消息模板', '帮助中心', '帮助文档管理']),
);
