-- ======================================================
-- V0176：令牌吊销落库兜底 —— `sys_token_blacklist`
--
-- 依据：docs/adr-20261101-redis-degradation-mode.md（【决策】Redis 不可用时后台
--       应用什么形态）。结论是「选 3：黑名单落库兜底」，本迁移是它的落地点。
--
-- 回滚：V0176__create_sys_token_blacklist.down.sql（DROP TABLE，零残留）
--
-- ── 为什么这张表存在（一句话）──────────────────────────────
-- 单条令牌的吊销事实（`token:blacklist:{jti}`）此前**只**住在 Redis 里：
--   · Redis 不可读 → 认证层 fail-closed，全站除登录外 401（可用性事故）；
--   · Redis 内存写满（noeviction）→ 写黑名单失败，框架 `JWT::blacklist` 用
--     `let _ = ...` 吞掉错误（vendor/nik-web/src/jwt.rs:219），登出照回 200，
--     而令牌实际未失效（安全静默失效）。
-- 本表让「哪条令牌被吊销」成为**库侧事实**，缓存退化为读加速器。
--
-- ── 为什么不给 user_id 加外键 ─────────────────────────────
-- 吊销记录的寿命必须**长于**账号本身：账号被物理删除时若级联删掉吊销行，一个
-- 仍在有效期内的 access 令牌就会在缓存故障窗口里「复活」。账号删除本身已由
-- `sys_user` 的行缺失 + `session_guard` 拦住（逐请求点查），不依赖本表。
--
-- ── 为什么不给 reason 加 CHECK ───────────────────────────
-- `reason` 是排障用的诊断字段。给它加 CHECK 等于把「一个枚举值没同步」升级成
-- 「登出失败 500」——FIX-03 的教训正是「安全动作不许因旁支失败而把主流程写成
-- 半成品」。取值由 Rust 侧枚举收敛（`BlacklistReason::as_str`），库侧只约束
-- 长度。`token_kind` 同理不加 CHECK。
--
-- ── 编号 ────────────────────────────────────────────────
-- 全仓共用一个号段（`migrations/{system,business,support}`），V0175 是当前最大值
-- （`business/V0175__erp_payable_payment_approval`）。本迁移取 V0176。
-- ======================================================

CREATE TABLE IF NOT EXISTS sys_token_blacklist (
    -- 令牌唯一标识（JWT claim `jti`），同时是吊销查询的唯一入口
    jti        varchar(64)  PRIMARY KEY,
    -- 令牌主体（JWT claim `sub`）；仅用于排障与「按用户查吊销」，**不加外键**（见文件头）
    user_id    uuid,
    -- 令牌类型：access / refresh（写作 varchar 而非 ENUM，取值由 Rust 侧枚举收敛）
    token_kind varchar(8)   NOT NULL DEFAULT 'access',
    -- 吊销原因（诊断用）：logout / force_logout / rotate / reuse_detected ...
    reason     varchar(32)  NOT NULL DEFAULT 'unspecified',
    -- 令牌的绝对到期时刻（= JWT `exp`）。本表行**只需活到这一刻**，
    -- 过期行由 `token_blacklist_cleanup` 定时清理（见 task/mod.rs）。
    expire_at  timestamptz  NOT NULL,
    -- 写入时刻（审计 / 排查「吊销是否落在故障窗口内」）
    created_at timestamptz  NOT NULL DEFAULT now()
);

-- 清理任务的唯一索引：`DELETE ... WHERE expire_at < now() - interval '1 hour'`
CREATE INDEX IF NOT EXISTS idx_sys_token_blacklist_expire
    ON sys_token_blacklist (expire_at);

COMMENT ON TABLE  sys_token_blacklist IS '令牌吊销事实（库侧权威）：单条 jti 的即时失效；缓存不可用时由 auth_guard 回查本表';
COMMENT ON COLUMN sys_token_blacklist.jti IS 'JWT claim jti（= 缓存键 token:blacklist:{jti} 的后缀）';
COMMENT ON COLUMN sys_token_blacklist.user_id IS 'JWT claim sub；仅排障用，故意不设外键（吊销记录须长于账号寿命）';
COMMENT ON COLUMN sys_token_blacklist.token_kind IS 'access / refresh（取值由 Rust 侧 BlacklistKind 收敛）';
COMMENT ON COLUMN sys_token_blacklist.reason IS '吊销原因：logout / force_logout / rotate / reuse_detected；故意不加 CHECK（诊断字段不得成为可用性风险）';
COMMENT ON COLUMN sys_token_blacklist.expire_at IS '令牌绝对到期时刻（JWT exp）；过期行可清理，活过它没有任何读路径会用到';
COMMENT ON COLUMN sys_token_blacklist.created_at IS '写入时刻；用于核对「吊销是否恰好落在缓存故障窗口」';
