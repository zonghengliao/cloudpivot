#!/usr/bin/env bash
# A2 验证环境对齐（**只作用于 cloudpivot_test**）：把 A1 冒烟留下的 attachment 策略行
# 对齐到 V0048 的设计值（V0048 用 ON CONFLICT DO NOTHING，不会覆盖既有行 —— 幂等语义正确）。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -q -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_test -c "
UPDATE sys_storage_policy
   SET max_size_mb        = 100,
       allowed_extensions = 'jpg,jpeg,png,gif,bmp,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,zip,rar,7z,gz,tar,mp3,wav,mp4,mov',
       allowed_mimes      = '',
       direct_upload      = TRUE,
       presign_ttl_secs   = 900,
       updated_at         = NOW()
 WHERE category = 'attachment' AND deleted_at IS NULL;
" && echo "ALIGNED attachment policy → V0048 设计值"
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d cloudpivot_test \
  -c "select category,max_size_mb,left(allowed_extensions,40) as exts,direct_upload,presign_ttl_secs from sys_storage_policy order by sort_order;"
