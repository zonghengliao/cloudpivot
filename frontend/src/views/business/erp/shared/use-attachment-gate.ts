/**
 * 附件必传闸门的组合式函数（**ATT-08**）。
 *
 * # 用法（单据详情抽屉）
 *
 * ```ts
 * const { gate: attachGate, refresh: refreshAttachGate } = useAttachmentGate({
 *   docType: ref<DocType | null>('INVOICE'),
 *   docId: computed(() => (detail.value?.status === 'DRAFT' ? detail.value.id : null)),
 *   at: 'SUBMIT',
 * });
 * ```
 *
 * ```vue
 * <t-button :disabled="!attachGate.canProceed" ...>开票</t-button>
 * <AttachmentGateHint :hint="attachGate.hint" />
 * <DocAttachmentPanel ... @changed="refreshAttachGate()" />
 * ```
 *
 * # 三个必须由本函数负责的细节
 *
 * 1. **自动重查**：`docType` / `docId` 一变就查（打开抽屉时单据从 `null` 变成 id）；
 *    面板上传 / 删除附件后调 `refresh()`（面板 `changed` 事件）。卡面验收②
 *    「上传正确格式附件 → 按钮自动变亮」就是这条链路。
 * 2. **乱序响应丢弃**：连续上传两个文件会打出两次请求，先发的可能后回 ——
 *    用自增序号只认最后一次，否则按钮状态会退回到「还有缺口」。
 * 3. **失败降级**：见 `attachment-gate-pure.ts` 文件头 —— 查询失败**不禁用**按钮。
 *
 * # 为什么不在这里做 v-if 级别的错误提示
 *
 * 闸门只是「提前告知」。403（无权看这张单）/ 404（单据被删）这类错误在详情页
 * 自己的加载流程里已经会显示，闸门再弹一次就是重复噪音。所以失败原因只放进
 * `gate.degradedReason` 供排查（devtools 里能看到），界面保持安静。
 */

import { ref, watch, type Ref } from 'vue';

import {
  checkDocAttachments,
  type AttachmentRequiredAt,
  type DocType,
} from '@/api/business/erp/attachment';

import { degradedGate, gateFromCheck, pendingGate, type AttachmentGate } from './attachment-gate-pure';

/** `useAttachmentGate` 的入参。 */
export interface AttachmentGateOptions {
  /** 单据类型；`null` = 还没有单据（面板 / 按钮都不该出现） */
  docType: Ref<DocType | null | undefined>;
  /** 单据主键；`null` = 还没有单据（新建未保存） */
  docId: Ref<string | null | undefined>;
  /** 触发动作，缺省 `SUBMIT`（发票开票 / 报销提交 / 退货提交 / 收款确认都是它） */
  at?: AttachmentRequiredAt;
  /** 查询失败的上报钩子（默认 `console.warn`，测试可注入） */
  onError?: (error: unknown) => void;
}

/** `useAttachmentGate` 的返回值。 */
export interface AttachmentGateHandle {
  /** 闸门状态（页面直接渲染：`canProceed` / `hint` / `tooltip`） */
  gate: Ref<AttachmentGate>;
  /** 是否正在查（用于按钮 loading；不参与禁用判定） */
  checking: Ref<boolean>;
  /** 手动重查（面板 `@changed` 调它） */
  refresh: () => Promise<void>;
}

/**
 * 建一个附件必传闸门。
 *
 * @param options 单据类型 / 主键（都是 ref）与动作
 * @returns 闸门状态 + 手动重查
 */
export function useAttachmentGate(options: AttachmentGateOptions): AttachmentGateHandle {
  const at: AttachmentRequiredAt = options.at ?? 'SUBMIT';
  const gate = ref<AttachmentGate>(pendingGate());
  const checking = ref(false);
  /** 请求序号：只认最后一次（乱序响应丢弃，见文件头第 2 条） */
  let seq = 0;

  async function refresh(): Promise<void> {
    const docType = options.docType.value;
    const docId = options.docId.value;
    if (!docType || !docId) {
      // 还没有单据：回到初始态（不查、不禁用、不画红字）
      seq += 1;
      checking.value = false;
      gate.value = pendingGate();
      return;
    }
    const mine = ++seq;
    checking.value = true;
    try {
      const result = await checkDocAttachments(docType, docId, at);
      if (mine !== seq) return;
      gate.value = gateFromCheck(result);
    } catch (error) {
      if (mine !== seq) return;
      // 降级：查不到不禁用（后端仍会拦），原因只留给排查
      gate.value = degradedGate(error instanceof Error ? error.message : String(error));
      if (options.onError) {
        options.onError(error);
      } else {
        console.warn('[attachment-gate] 必传校验查询失败，已降级为“不禁用按钮”', error);
      }
    } finally {
      if (mine === seq) checking.value = false;
    }
  }

  watch([options.docType, options.docId], () => {
    void refresh();
  }, { immediate: true });

  return { gate, checking, refresh };
}
