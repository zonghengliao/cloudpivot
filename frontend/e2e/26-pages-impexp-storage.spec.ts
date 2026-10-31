import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/** 导入导出 3 页 + 文件存储 2 页。 */
definePageChecks(pages(['导入任务', '导出任务', '导入向导', '文件管理（存储）', '存储后端']));
