<!--
  DocPdfButton.vue —— 单据「下载 PDF」按钮（PDF-08 统一接入）

  # 为什么是一个组件而不是每页复制一段

  12 个入口（采购 / 销售 / 合同 / 入库 / 出库 / 调拨 / 盘点 / 采购退货 / 销售退货 /
  审批实例 / 任务 / 三张报表）都要同一套行为：带 token 取 blob、用后端给的中文名存盘、
  下载中 loading、失败提示可重试、无权限（403）说清是缺权限。复制 12 遍必然有一两处
  漏掉 loading 或忘了 `revokeObjectURL`。

  # 用法

  ```vue
  <DocPdfButton
    :path="`/api/business/erp/purchase-order/${detail.id}/pdf`"
    :filename="`采购订单-${detail.order_no}.pdf`"
    testid="purchase-detail-pdf"
  />
  ```

  `path` 必须是**带 `/api` 前缀**的完整路径（漏前缀会命中 SPA fallback 拿回 HTML，
  解析成 JSON 后提示「导出失败」）。
-->
<template>
  <t-button
    :size="size"
    variant="outline"
    :loading="downloading"
    :disabled="disabled"
    :data-testid="testid"
    @click="onClick"
  >
    <template #icon><t-icon name="download" /></template>
    下载 PDF
  </t-button>
</template>

<script setup lang="ts">
import { MessagePlugin } from 'tdesign-vue-next';
import { useDocPdfDownload } from '@/composables/useDocPdfDownload';

const props = withDefaults(
  defineProps<{
    /** 带 `/api` 前缀的 PDF 接口路径 */
    path: string;
    /** 后端没给可信中文名时的回退名 */
    filename?: string;
    /** 自动化测试的 testid */
    testid?: string;
    /** 额外禁用（一般不用：无权限靠 403 提示，而不是把按钮藏起来） */
    disabled?: boolean;
    /** 按钮尺寸（报表页工具条用 medium，单据详情用 small） */
    size?: 'small' | 'medium' | 'large';
  }>(),
  { filename: 'document.pdf', testid: 'btn-doc-pdf', disabled: false, size: 'small' },
);

const { downloading, downloadDocPdf } = useDocPdfDownload();

async function onClick(): Promise<void> {
  const result = await downloadDocPdf(props.path, props.filename);
  if (result.ok) {
    MessagePlugin.success(result.message);
  } else {
    // 明确说「可重试」：失败分支按钮自动恢复可点，用户再点一次即可
    MessagePlugin.error(`${result.message}（可重试）`);
  }
}
</script>
