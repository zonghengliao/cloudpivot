-- 回滚：偏好体系两张表（先删值表：它带指向 sys_user 的外键，且语义上依赖字典表）
DROP TABLE IF EXISTS sys_user_preference;
DROP TABLE IF EXISTS sys_preference_def;
