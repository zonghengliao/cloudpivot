-- ======================================================
-- 内置系统参数（种子迁移：指纹变化即重跑，故走 ON CONFLICT 幂等写入）
-- 登录安全项与 src/shared/constants.rs 的默认常量对应，登录流程优先读库、
-- 读不到时回退常量，因此这两行是「可调默认值」而非唯一来源。
-- ======================================================

INSERT INTO sys_config (id, config_key, config_value, config_type, description,
                        category, config_level, is_modifiable, is_encrypted, is_restart, status, sort_order)
VALUES (gen_random_uuid(), 'auth.max_login_fail_count', '5', 'NUMBER',
        '连续登录失败达到该次数即锁定账号', 'auth', 1, TRUE, FALSE, FALSE, 1, 10)
ON CONFLICT (config_key) WHERE deleted_at IS NULL
DO UPDATE SET config_value = EXCLUDED.config_value,
              config_type = EXCLUDED.config_type,
              description = EXCLUDED.description,
              category = EXCLUDED.category,
              status = EXCLUDED.status,
              sort_order = EXCLUDED.sort_order,
              updated_at = CURRENT_TIMESTAMP;

INSERT INTO sys_config (id, config_key, config_value, config_type, description,
                        category, config_level, is_modifiable, is_encrypted, is_restart, status, sort_order)
VALUES (gen_random_uuid(), 'auth.lock_minutes', '30', 'NUMBER',
        '账号锁定后自动解锁的等待时长（分钟）', 'auth', 1, TRUE, FALSE, FALSE, 1, 20)
ON CONFLICT (config_key) WHERE deleted_at IS NULL
DO UPDATE SET config_value = EXCLUDED.config_value,
              config_type = EXCLUDED.config_type,
              description = EXCLUDED.description,
              category = EXCLUDED.category,
              status = EXCLUDED.status,
              sort_order = EXCLUDED.sort_order,
              updated_at = CURRENT_TIMESTAMP;

INSERT INTO sys_config (id, config_key, config_value, config_type, description,
                        category, config_level, is_modifiable, is_encrypted, is_restart, status, sort_order)
VALUES (gen_random_uuid(), 'log.ip_location_enabled', 'true', 'BOOL',
        '是否解析登录日志的 IP 归属地（关闭后不再外呼，仅保留本地网络标识）', 'log', 1, TRUE, FALSE, FALSE, 1, 30)
ON CONFLICT (config_key) WHERE deleted_at IS NULL
DO UPDATE SET config_value = EXCLUDED.config_value,
              config_type = EXCLUDED.config_type,
              description = EXCLUDED.description,
              category = EXCLUDED.category,
              status = EXCLUDED.status,
              sort_order = EXCLUDED.sort_order,
              updated_at = CURRENT_TIMESTAMP;

-- ======================================================
-- 空闲自动退出登录（AUTO-LOGOUT，2026-11-10）
--
-- 语义（用户口径）：
--   · 1-60 分钟：空闲超过该时长即拒绝该会话的一切请求（401 / 20009）；
--   · 0        ：禁用自动退出，**只有超级管理员**能设；
--   · 上限 60  ：超管也设不了 70/120/1440。
--
-- 角色校验与区间校验在 `support::config::handler::check_idle_timeout_write`
-- 里硬拦（接口层，不看任何审批实例）—— 这个键**不接入流程引擎**，
-- 不存在「审批通过就把自动退出关掉」的路径。
--
-- 冲突策略是 DO NOTHING（本文件其它三行是 DO UPDATE）：
-- 本文件指纹变化会整份重跑，而这一行是**安全策略**，超管可能已经把它改成
-- 5 / 30 / 0；重跑种子不该把管理员的决定冲回默认值。键不存在时才插入默认 15。
-- ======================================================
INSERT INTO sys_config (id, config_key, config_value, config_type, description,
                        category, config_level, is_modifiable, is_encrypted, is_restart, status, sort_order)
VALUES (gen_random_uuid(), 'auth.idle_timeout_minutes', '15', 'NUMBER',
        '无操作自动退出登录的等待时长（分钟）：1-60；0=禁用自动退出（仅超级管理员可设）', 'auth', 1, TRUE, FALSE, FALSE, 1, 40)
ON CONFLICT (config_key) WHERE deleted_at IS NULL
DO NOTHING;
