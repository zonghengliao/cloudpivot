-- 回滚：只删本迁移种下的 8 个键，不动别的偏好定义（将来别的迁移加的行不受影响）
DELETE FROM sys_preference_def
 WHERE pref_key IN (
    'ui.palette',
    'ui.theme_mode',
    'ui.side_compact',
    'ui.table_density',
    'ui.page_size',
    'ui.language',
    'notify.inbox',
    'notify.email'
 );
