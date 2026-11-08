/**
 * 「带 token 取 PDF → 存盘」的统一实现（PDF-08）。
 *
 * # 为什么不用 axios / 不用 window.open
 *
 * - `window.open(url)` 走的是浏览器原生导航：**带不上 `Authorization` 头**，后端必 401；
 *   把 token 拼到 query 里则会进浏览器历史 / 代理日志 / Referer（泄漏）。
 * - 所以卡片规定用 `fetch` + `Authorization: Bearer`，拿 `blob` 再造临时链下载
 *   （`saveBlob` 统一延后 1s revoke，Safari 才拿得到内容）。
 *
 * # 失败分支
 *
 * 后端出错返回的是统一响应体 JSON（`content-type: application/json`），不是 PDF。
 * 这里先 `res.ok`，不 ok 就按文本读、尝试 JSON 解析，再交给
 * `docPdfErrorMessage(status, body)` 翻成人话（403 → 无权限，5xx → 渲染服务不可用…）。
 * 用户拿到的是「可以再点一次」的提示，页面不会白屏。
 */

import { ref, type Ref } from 'vue';
import { TOKEN_KEY } from '@/api/request';
import { saveBlob } from '@/utils/download';
import { docPdfErrorMessage, filenameFromContentDisposition } from '@/utils/doc-pdf';

/** 一次下载的结果（页面据此提示成功 / 失败，失败可直接再点） */
export interface DocPdfDownloadResult {
  /** 是否成功保存到本地 */
  ok: boolean;
  /** 给人看的提示（成功 = 文件名，失败 = 原因 + 可重试） */
  message: string;
  /** 实际使用的文件名（成功时） */
  filename?: string;
}

/** 下载器：`downloading` 直接绑按钮的 loading（防连点重复渲染） */
export interface DocPdfDownloader {
  /** 是否正在下载 */
  downloading: Ref<boolean>;
  /**
   * 取 PDF 并存盘。
   *
   * @param path         带 `/api` 前缀的完整路径
   * @param fallbackName 后端没给可信中文名时用的回退名
   */
  downloadDocPdf: (path: string, fallbackName: string) => Promise<DocPdfDownloadResult>;
}

/**
 * 创建一个 PDF 下载器（每个页面 / 按钮一份，互不影响 loading）。
 *
 * @returns `{ downloading, downloadDocPdf }`
 */
export function useDocPdfDownload(): DocPdfDownloader {
  const downloading = ref(false);

  async function downloadDocPdf(path: string, fallbackName: string): Promise<DocPdfDownloadResult> {
    if (downloading.value) {
      return { ok: false, message: '正在下载，请稍候…' };
    }
    downloading.value = true;
    try {
      const token = localStorage.getItem(TOKEN_KEY);
      const headers: Record<string, string> = {};
      if (token) {
        headers.Authorization = `Bearer ${token}`;
      }
      const response = await fetch(path, { method: 'GET', headers, credentials: 'same-origin' });
      if (!response.ok) {
        const text = await response.text().catch(() => '');
        let body: unknown = text;
        try {
          body = JSON.parse(text);
        } catch {
          // 非 JSON（nginx 403 页 / 网关 HTML）→ 保留原文，交给翻译层过滤
        }
        return { ok: false, message: docPdfErrorMessage(response.status, body) };
      }
      const blob = await response.blob();
      if (blob.size === 0) {
        return { ok: false, message: '导出的 PDF 是空文件，请重试' };
      }
      const name =
        filenameFromContentDisposition(response.headers.get('content-disposition')) ||
        fallbackName ||
        'document.pdf';
      saveBlob(blob, name);
      return { ok: true, message: `已下载 ${name}`, filename: name };
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      return { ok: false, message: `下载失败：${reason}，请重试` };
    } finally {
      downloading.value = false;
    }
  }

  return { downloading, downloadDocPdf };
}
