-- ======================================================
-- V0111：变更日志新增 `operator_type`（批次 C+ · T5「AI 执行者规则细化」）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §六（AI 作为执行者：机器人操作全部记
--       changelog 可审计）、docs/云枢设计修正补丁-20261029.md 修正 1 / 修正 11、
--       卡面 t_bb7d2ef3「2.4 changelog 增加 operator_type」
-- 回滚：V0111__changelog_operator_type.down.sql
--
-- ── 为什么必须是一列而不是 join 出来 ──────────────────────────
-- `sys_user.user_type` 是**可变**的：一个账号今天 `ROBOT`，明天管理员把它改回
-- `HUMAN`（L1 的 `AI_AGENT_USER_TYPE_LOCKED` 只禁止通用接口改，AI 用户接口本身
-- 可以整体删除重建）。审计要回答的是「**当时**这条变更是不是 AI 做的」，
-- 与 `operator_name` / `operator_dept_id` 冗余落库同一理由：事后改名 / 调部门 /
-- 改类型都不该让历史记录变味。
--
-- ── 为什么默认值给 'HUMAN' 而不是 NULL ────────────────────────
-- ① `NOT NULL DEFAULT 'HUMAN'` 让**历史行**（V0111 之前写的日志）语义明确：
--    它们全部产生于「AI 用户尚未开放写操作」的阶段，只能是人类操作；
-- ② 写入侧永远显式给值（`business/changelog/model.rs::insert`），默认值只服务
--    历史行与手工补数的场景；
-- ③ NULL 会让每个查询都要写 `COALESCE(operator_type,'HUMAN')`，一次漏写就把
--    「人」的行从统计里漏掉。
--
-- ── 取值与约束 ────────────────────────────────────────────────
-- `HUMAN` / `ROBOT`，与 `sys_user.user_type` 逐字一致（V0102 的取值域）。
-- CHECK 兜底：写侧枚举是封闭的（`changelog::OperatorType`），库侧再钉一道，
-- 杜绝「手工 SQL 塞个 ai / robot 小写」这类脏值把前端标签弄坏。
--
-- ── 为什么加一条部分索引 ──────────────────────────────────────
-- 「把这个 AI 最近做过什么列出来」是 T5 之后最常见的审计问法，而 ROBOT 行在
-- 全表里占比极低（< 1%）。部分索引 `WHERE operator_type = 'ROBOT'` 的体积与
-- 维护成本近似为零，查询却能只扫这一小撮行。`HUMAN` 的检索已有
-- `idx_sys_change_log_operator (operator_id, created_at DESC)` 覆盖，不再加索引。
--
-- ── 分区 ──────────────────────────────────────────────────────
-- `sys_change_log` 是月分区表（V0084）。PG 的 `ALTER TABLE ... ADD COLUMN` 与
-- `CREATE INDEX` 在**父表**上执行会自动作用到全部现存分区与将来的分区
-- （父表建索引 → 每个分区各自建同构索引；`DROP` 同理）。因此本迁移只操作父表。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS` / `DROP CONSTRAINT IF EXISTS` + DO 块 /
-- `CREATE INDEX IF NOT EXISTS`：重复执行不报错，也不覆盖运维后来改过的注释。
--
-- ── 乱序补迁 ──────────────────────────────────────────────────
-- 本文件晚于 V0110 应用时不会出现「乱序」警告；若与并行的 V0112+ 交叉，启动日志
-- 可能打印「待应用迁移的版本低于已应用最大值（乱序补迁）」——**不阻断**，
-- 应用成功后重启即为「应用 0 个迁移」（C8 已验证过这条口径）。
-- ======================================================

-- ---------- ① 列 ----------
ALTER TABLE sys_change_log
    ADD COLUMN IF NOT EXISTS operator_type VARCHAR(16) NOT NULL DEFAULT 'HUMAN';

COMMENT ON COLUMN sys_change_log.operator_type IS
    '操作人类型快照：HUMAN=人类账号 / ROBOT=AI 用户（user_type=ROBOT）。冗余落库，'
    '避免操作人类型变更后历史记录失真（批次 C+ · T5）';

-- ---------- ② CHECK（DO 块保证幂等：PG 没有 ADD CONSTRAINT IF NOT EXISTS） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_sys_change_log_operator_type'
           AND conrelid = 'sys_change_log'::regclass
    ) THEN
        ALTER TABLE sys_change_log
            ADD CONSTRAINT ck_sys_change_log_operator_type
            CHECK (operator_type IN ('HUMAN', 'ROBOT'));
    END IF;
END $$;

-- ---------- ③ 部分索引：只看 AI 的操作 ----------
CREATE INDEX IF NOT EXISTS idx_sys_change_log_ai_operator
    ON sys_change_log (created_at DESC)
    WHERE operator_type = 'ROBOT';

-- ---------- ④ 自检（失败即启动失败，宁可起不来也不要带着半套结构跑） ----------
DO $$
DECLARE
    column_type TEXT;
    column_default TEXT;
    is_not_null BOOLEAN;
    has_check BOOLEAN;
    has_index BOOLEAN;
    robot_rows BIGINT;
BEGIN
    SELECT format_type(a.atttypid, a.atttypmod),
           pg_get_expr(d.adbin, d.adrelid),
           a.attnotnull
      INTO column_type, column_default, is_not_null
      FROM pg_attribute a
      LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
     WHERE a.attrelid = 'sys_change_log'::regclass
       AND a.attname = 'operator_type'
       AND NOT a.attisdropped;

    IF column_type IS NULL THEN
        RAISE EXCEPTION 'V0111 自检失败：sys_change_log.operator_type 不存在';
    END IF;
    IF column_type <> 'character varying(16)' THEN
        RAISE EXCEPTION 'V0111 自检失败：operator_type 类型应为 varchar(16)，实际 %', column_type;
    END IF;
    IF NOT is_not_null THEN
        RAISE EXCEPTION 'V0111 自检失败：operator_type 必须是 NOT NULL';
    END IF;
    IF column_default IS NULL OR position('HUMAN' IN column_default) = 0 THEN
        RAISE EXCEPTION 'V0111 自检失败：operator_type 的默认值应为 HUMAN，实际 %', column_default;
    END IF;

    SELECT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_sys_change_log_operator_type'
           AND conrelid = 'sys_change_log'::regclass
    ) INTO has_check;
    IF NOT has_check THEN
        RAISE EXCEPTION 'V0111 自检失败：缺少 CHECK 约束 ck_sys_change_log_operator_type';
    END IF;

    SELECT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE schemaname = current_schema()
           AND tablename = 'sys_change_log'
           AND indexname = 'idx_sys_change_log_ai_operator'
    ) INTO has_index;
    IF NOT has_index THEN
        RAISE EXCEPTION 'V0111 自检失败：缺少部分索引 idx_sys_change_log_ai_operator';
    END IF;

    -- 历史行必须全部落在 HUMAN（AI 写操作是 T5 才开放的）
    SELECT COUNT(*) INTO robot_rows FROM sys_change_log WHERE operator_type <> 'HUMAN';
    IF robot_rows > 0 THEN
        RAISE EXCEPTION 'V0111 自检失败：迁移前不应存在非 HUMAN 的日志行，实际 % 行', robot_rows;
    END IF;

    RAISE NOTICE 'V0111 自检通过：operator_type varchar(16) NOT NULL DEFAULT HUMAN + CHECK + 部分索引';
END $$;
