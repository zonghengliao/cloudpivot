/**
 * 单据 PDF 下载的纯逻辑（PDF-08）。
 *
 * 为什么单独一层：12 个单据页面都要「带 token 取 blob → 取后端返回的中文文件名 →
 * 出错按状态码给人话」。这些判断写 12 遍必然漂移，收在这里可被 `pnpm test:unit` 直接钉住。
 *
 * # 关键口径
 *
 * - **文件名只认 `filename*=`（RFC 5987）**：后端 `pdf::handler::content_disposition`
 *   恒发 `attachment; filename="<ASCII 回退名>"; filename*=UTF-8''<百分号编码>`，而 ASCII
 *   回退名会把每个中文字替换成 `_`（`采购订单-PO-1.pdf` → `____-PO-1.pdf`）。所以
 *   `filename=` 只在**不含 `_`** 时才可信，否则一律用调用方给的回退名 —— 宁可名字朴素，
 *   也不能让用户存成 `____-PO-1.pdf`。
 * - **错误体是 JSON**（`ApiError::into_response` 的统一响应体），但 403/401 可能被网关
 *   先拦（HTML）。解析失败时按状态码兜底，不把 `<!DOCTYPE html>` 甩给用户。
 */

/** 后端 `Res<T>` 的形状（只取错误分支需要的字段） */
export interface DocPdfErrorBody {
  /** 业务码（0 = 成功） */
  code?: number;
  /** 人话消息 */
  msg?: string;
}

/**
 * 从 `Content-Disposition` 里取下载名。
 *
 * @param header 响应头原值（可能为 `null`）
 * @returns 可用的文件名；取不到可信名字时返回 `null`（调用方用自己的回退名）
 */
export function filenameFromContentDisposition(header: string | null | undefined): string | null {
  if (!header) {
    return null;
  }
  // ① 优先 RFC 5987：filename*=UTF-8''%E9%87%87%E8%B4%AD...
  const star = /filename\*\s*=\s*UTF-8''([^;]+)/i.exec(header);
  if (star?.[1]) {
    const raw = star[1].trim().replace(/^"|"$/g, '');
    try {
      const decoded = decodeURIComponent(raw).trim();
      if (decoded) {
        return sanitizeFileName(decoded);
      }
    } catch {
      // 百分号编码坏掉（不该发生）→ 落回下一步
    }
  }
  // ② 朴素 filename="..."：仅当没被 ASCII 回退「打码」时可信
  const plain = /filename\s*=\s*"([^"]*)"/i.exec(header) ?? /filename\s*=\s*([^;]+)/i.exec(header);
  if (plain?.[1]) {
    const value = plain[1].trim().replace(/^"|"$/g, '');
    // 后端把每个非 ASCII 字符换成 `_`；出现 `_` 说明原名很可能是中文，别用它
    if (value && !value.includes('_')) {
      return sanitizeFileName(value);
    }
  }
  return null;
}

/**
 * 收敛文件名：去掉路径分隔符 / 控制字符（防 `../` 与换行注入），保留中文。
 *
 * @param name 原始文件名
 * @returns 可安全赋给 `<a download>` 的名字
 */
export function sanitizeFileName(name: string): string {
  const cleaned = name
    .replace(/[\u0000-\u001f\u007f]/g, '')
    .replace(/[\\/]/g, '-')
    .trim();
  return cleaned || 'document.pdf';
}

/**
 * 把 HTTP 失败翻译成给人看的一句话（含「可重试」的提示）。
 *
 * @param status HTTP 状态码
 * @param body  已解析的 JSON 错误体（解析失败时传原文或 `null`）
 * @returns 提示文案
 */
export function docPdfErrorMessage(status: number, body: unknown): string {
  const message = pickMessage(body);
  if (status === 401) {
    return '登录已过期，请重新登录后再导出';
  }
  if (status === 403) {
    return message ? `没有导出该单据 PDF 的权限：${message}` : '没有导出该单据 PDF 的权限（请联系管理员分配）';
  }
  if (status === 404) {
    return message || '单据不存在或已被删除';
  }
  if (status === 409) {
    return message || '当前单据状态不允许导出';
  }
  if (status >= 500) {
    return message ? `PDF 渲染服务暂时不可用：${message}` : 'PDF 渲染服务暂时不可用，请稍后重试';
  }
  return message || `导出失败（HTTP ${status}）`;
}

/** 从错误体里取人话消息（兼容 `msg` / `message` / 纯文本，且过滤掉 HTML） */
function pickMessage(body: unknown): string | null {
  if (!body) {
    return null;
  }
  if (typeof body === 'string') {
    const text = body.trim();
    if (!text || text.startsWith('<')) {
      return null;
    }
    // 后端偶尔返回 `{"msg":...}` 字符串，再试一次 JSON
    if (text.startsWith('{')) {
      try {
        return pickMessage(JSON.parse(text));
      } catch {
        return text.slice(0, 200);
      }
    }
    return text.slice(0, 200);
  }
  if (typeof body === 'object') {
    const record = body as Record<string, unknown>;
    const candidate = record.msg ?? record.message ?? record.error;
    if (typeof candidate === 'string' && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}
