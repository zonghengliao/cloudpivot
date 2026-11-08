#!/usr/bin/env bash
# ============================================================================
# AUD-01 · 附件上传人姓名回填 · 执行器
#
#   00 只读体检   bash apply.sh --check
#   01 回填       bash apply.sh            （幂等，可重复跑；第二遍影响行数为 0）
#   02 验收       bash apply.sh --verify   （只读；空值数必须 0）
#   03 回滚       bash apply.sh --rollback （按台账还原「还是回填值」的行）
#
# 库连接（环境变量，口令只走环境变量，别写进命令行历史）：
#   AUD01_DB   目标库名，默认 cloudpivot
#   PGHOST / PGPORT / PGUSER / PGPASSWORD
#
# 前置：迁移 V0352 必须已应用（列不存在时 --check 会明确报出来，回填脚本会直接失败）。
# ============================================================================
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

DB=${AUD01_DB:-cloudpivot}
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
PGUSER=${PGUSER:-cloudpivot}

# 写库用：静默 + 失败即停
PSQL_RW=(psql -X -q -v ON_ERROR_STOP=1 -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB")
# 只读报告用：保留表头与对齐，方便人眼 / 证据留档
PSQL_RO=(psql -X -v ON_ERROR_STOP=1 -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB")

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2
}

case "${1:-}" in
    --check)
        echo "== AUD-01 回填前体检：库 ${DB}@${PGHOST}:${PGPORT}（只读）"
        "${PSQL_RO[@]}" -f "${SCRIPT_DIR}/10_precheck.sql"
        ;;
    --verify)
        echo "== AUD-01 回填后验收：库 ${DB}@${PGHOST}:${PGPORT}（只读）"
        "${PSQL_RO[@]}" -f "${SCRIPT_DIR}/20_verify.sql"
        ;;
    --rollback)
        echo "== AUD-01 回滚：库 ${DB}@${PGHOST}:${PGPORT}"
        "${PSQL_RW[@]}" <<'SQL'
BEGIN;
DO $$
DECLARE
    n_doc  integer;
    n_task integer;
BEGIN
    UPDATE biz_doc_attachment a
       SET uploader_name = b.uploader_name_before
      FROM datafix_aud01_uploader_name_backup b
     WHERE b.tbl = 'biz_doc_attachment'
       AND a.id = b.row_id
       AND b.uploader_name_after IS NOT NULL
       AND a.uploader_name = b.uploader_name_after;
    GET DIAGNOSTICS n_doc = ROW_COUNT;

    UPDATE biz_task_attachment a
       SET uploader_name = b.uploader_name_before
      FROM datafix_aud01_uploader_name_backup b
     WHERE b.tbl = 'biz_task_attachment'
       AND a.id = b.row_id
       AND b.uploader_name_after IS NOT NULL
       AND a.uploader_name = b.uploader_name_after;
    GET DIAGNOSTICS n_task = ROW_COUNT;

    RAISE NOTICE 'AUD-01 回滚完成：biz_doc_attachment % 行，biz_task_attachment % 行', n_doc, n_task;
END $$;
COMMIT;
SQL
        ;;
    "")
        echo "== AUD-01 回填：库 ${DB}@${PGHOST}:${PGPORT}"
        "${PSQL_RW[@]}" -f "${SCRIPT_DIR}/backfill_attachment_uploader_name.sql"
        echo "== 回填完成，跑一次验收：bash $0 --verify"
        ;;
    *)
        usage
        ;;
esac
