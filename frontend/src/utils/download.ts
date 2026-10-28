/**
 * 二进制下载工具。
 *
 * 后端文件接口都要走鉴权头，不能直接用 `<a href>` 打开，因此统一「取 blob → 造临时链 → 触发保存」。
 */

/** 触发浏览器保存一个 blob */
export function saveBlob(blob: Blob, fileName: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = fileName;
  document.body.appendChild(link);
  link.click();
  link.remove();
  // 立即释放会让部分浏览器（Safari）拿不到内容，延后一拍更稳
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** 从 `Content-Disposition` 样式的文件名兜底：取最后一段路径作为文件名 */
export function fallbackFileName(name: string, fallback: string): string {
  const trimmed = (name || '').trim();
  if (!trimmed) {
    return fallback;
  }
  return trimmed.split(/[\\/]/).pop() || fallback;
}
