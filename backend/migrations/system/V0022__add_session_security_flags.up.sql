-- 会话安全字段：首登强制改密标记 + 令牌版本号（幂等补丁）
--
-- 编号说明：本迁移**有意使用 `IF NOT EXISTS` 且排在 V0021 之后**。原因是同一
-- 批次里另有「令牌吊销补全（V0021）」需要给 `sys_user` 加同名列；两个迁移都能
-- 单独把库带到目标 schema，但若都用裸 `ADD COLUMN` 则后执行者必失败。这里取
-- 「先执行者建列、后执行者空转」的写法，保证任意先后顺序都能通过：
--
--   顺序 V0021 → V0022：V0021 建列，V0022 全部空转；
--   顺序 V0022 → （无 V0021）：V0022 建列。
--
-- must_change_password：为 TRUE 时认证层（middleware/session_guard.rs）只放行
--   「改密」与「登出」两个接口，其余一律 403；改密成功后置 FALSE。
--   （安全审计 CODE-06）
--
-- token_version：令牌版本号。签发时写入令牌 claim `ver`，认证层逐请求与库中值
--   比对；改密 / 重置口令 / 改角色 / 停用账号时自增，使该用户已签发的全部令牌
--   （access + refresh）立即失效。（安全审计 CODE-05 / CODE-08）

ALTER TABLE sys_user
    ADD COLUMN IF NOT EXISTS must_change_password BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS token_version INTEGER NOT NULL DEFAULT 0;

COMMENT ON COLUMN sys_user.must_change_password IS '是否必须修改密码（TRUE 时除改密/登出外全部接口 403）';
COMMENT ON COLUMN sys_user.token_version IS '令牌版本号（自增即令该用户已签发的全部令牌失效）';
