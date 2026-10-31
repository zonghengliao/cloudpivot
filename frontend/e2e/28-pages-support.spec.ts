import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** 支撑功能 6 页：字典 / 参数配置 / 文件 / 定时任务 / 日志 / 在线用户。 */
definePageChecks(pages(['字典管理', '参数配置', '文件管理（支撑）', '定时任务', '操作日志', '在线用户']));
