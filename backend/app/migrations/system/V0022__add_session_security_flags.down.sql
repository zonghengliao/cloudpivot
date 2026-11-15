-- 回滚 V0022：删除会话安全字段
--
-- 注意：若同批次的 V0021（令牌吊销补全）也已执行，本回滚会一并删掉它依赖的列；
-- 生产回滚前请先确认下游代码已不再读取 token_version / must_change_password。
ALTER TABLE sys_user
    DROP COLUMN IF EXISTS must_change_password,
    DROP COLUMN IF EXISTS token_version;
