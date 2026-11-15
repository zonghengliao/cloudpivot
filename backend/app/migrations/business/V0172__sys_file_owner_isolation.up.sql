-- V0172：`sys_file` 增加「共享标记」—— 文件中心下载补属主隔离（BUG-D-08）
--
-- 依据：docs/test-report-modules.md §0「安全类发现」BUG-D-08（P2，越权）：
--   下载读路径 `business/storage/model.rs::find_active_file(id)` 只有 id 入参，
--   结构上不可能按用户收口，凡持 `business:storage:download` 的账号能下载任何未删
--   文件（含别人上传的私有文件）。
--
-- 收口口径（卡面「共享标记可用 sys_file.category / 新列」）：
--   读路径判据 = `uploaded_by = 当前用户` **或** `is_shared = TRUE`。
--   取「新列」而不是复用 `category`：分类是上传策略的路由键（决定大小 / 扩展名白名单），
--   把「谁可见」塞进分类会让「换分类」变成一次隐式的权限变更，且无法表达
--   「同一个 document 分类里，有的文件共享、有的私有」。
--
-- 本迁移只做三件事：
--   1. 加列 `is_shared BOOLEAN NOT NULL DEFAULT FALSE`（默认私有，fail-closed）；
--   2. 回填**组织级公共资产**分类：`avatar`（头像，列表里要给别人看）、
--      `template`（导入模板，人人可用）。其余分类（attachment / document /
--      announcement / task / task_ai / import）一律保持私有：它们的跨用户访问
--      由各自的业务入口授权（公告附件走公告可见性、任务附件走任务可见性）。
--   3. 自检：列存在、非空、默认值 FALSE、回填行数为「库里 avatar/template 行数」。
--
-- 幂等：`ADD COLUMN IF NOT EXISTS` + 回填只动 `is_shared = FALSE` 的行，可重复执行。
-- 回滚：down 只删列（共享标记本身不是业务数据，退回旧行为 = 全局共享读路径）。
--
-- 注：手工重放本迁移时**必须用应用账号**（表 owner），否则之后应用账号会 permission denied。

ALTER TABLE sys_file
    ADD COLUMN IF NOT EXISTS is_shared BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN sys_file.is_shared IS
    '是否共享给全体（文件中心下载的属主隔离判据：uploaded_by = 当前用户 OR is_shared）';

-- 组织级公共资产回填（只回填仍为私有且未删的行；幂等）
UPDATE sys_file
SET is_shared = TRUE
WHERE deleted_at IS NULL
  AND is_shared = FALSE
  AND category IN ('avatar', 'template');

DO $$
DECLARE
    v_not_null BOOLEAN;
    v_default  TEXT;
    v_type     TEXT;
    v_expect   BIGINT;
    v_actual   BIGINT;
BEGIN
    SELECT c.is_nullable = 'NO', pg_get_expr(d.adbin, d.adrelid), format_type(a.atttypid, a.atttypmod)
      INTO v_not_null, v_default, v_type
      FROM information_schema.columns c
      JOIN pg_attribute a
        ON a.attrelid = 'public.sys_file'::regclass
       AND a.attname = c.column_name
      LEFT JOIN pg_attrdef d
        ON d.adrelid = a.attrelid
       AND d.adnum = a.attnum
     WHERE c.table_name = 'sys_file'
       AND c.column_name = 'is_shared';

    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0172 自检失败：sys_file.is_shared 列不存在';
    END IF;
    IF v_type <> 'boolean' THEN
        RAISE EXCEPTION 'V0172 自检失败：is_shared 类型应为 boolean，实际 %', v_type;
    END IF;
    IF NOT v_not_null THEN
        RAISE EXCEPTION 'V0172 自检失败：is_shared 必须 NOT NULL（默认私有，fail-closed）';
    END IF;
    IF v_default IS NULL OR v_default NOT LIKE '%false%' THEN
        RAISE EXCEPTION 'V0172 自检失败：is_shared 默认值应为 FALSE，实际 %', coalesce(v_default, '(无)');
    END IF;

    -- 回填结果核对：共享行 = 库里 avatar/template 的存活行
    SELECT count(*) INTO v_expect
      FROM sys_file
     WHERE deleted_at IS NULL AND category IN ('avatar', 'template');
    SELECT count(*) INTO v_actual
      FROM sys_file
     WHERE deleted_at IS NULL AND is_shared = TRUE
       AND category IN ('avatar', 'template');
    IF v_actual <> v_expect THEN
        RAISE EXCEPTION 'V0172 自检失败：avatar/template 应共享 % 行，实际 % 行', v_expect, v_actual;
    END IF;

    -- 非共享分类不得被顺手打开
    SELECT count(*) INTO v_actual
      FROM sys_file
     WHERE deleted_at IS NULL
       AND is_shared = TRUE
       AND category NOT IN ('avatar', 'template');
    IF v_actual <> 0 THEN
        RAISE EXCEPTION 'V0172 自检失败：非共享分类出现 % 行 is_shared = TRUE', v_actual;
    END IF;

    RAISE NOTICE 'V0172 完成：sys_file.is_shared 就绪，avatar/template 已共享 % 行', v_expect;
END $$;
