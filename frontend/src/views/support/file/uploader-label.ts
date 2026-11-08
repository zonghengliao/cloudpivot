/** 只关心这两个字段的行（列表行 / 测试桩都能传） */
export type UploaderFields = {
  /** 上传人显示名（后端 `uploader_name`；老后端没有这个字段） */
  uploader_name?: string;
  /** 上传人用户 ID */
  uploaded_by?: string;
};

/**
 * 上传人显示名（ATT-03）。
 *
 * 后端 `FileRes.uploader_name` 是 `COALESCE(NULLIF(u.real_name,''), u.username, '')`，
 * **恒为非 NULL 字符串**（空串 = 上传人已被删 / `uploaded_by` 是历史脏数据）。
 * 所以判定必须按「字段在不在」而不是「值真不真」：
 *
 * - 有名字 → 显示名字；
 * - 空串 → 上传人已不存在，显示 `-`，**绝不**把 UUID 甩给用户；
 * - 字段缺失（老后端 / 老缓存）→ 才回退 `uploaded_by`，再空显示 `-`。
 *
 * 抽成纯函数是为了单测能直接钉住这三条分支（列表「上传人」列与预览面板共用）。
 */
export function uploaderLabel(row: UploaderFields): string {
  if (row.uploader_name !== undefined) {
    return row.uploader_name.trim() || '-';
  }
  return row.uploaded_by || '-';
}
