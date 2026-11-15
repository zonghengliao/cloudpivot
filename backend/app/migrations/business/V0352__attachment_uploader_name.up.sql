-- ============================================================
-- V0352：附件上传人姓名**快照**列（`biz_doc_attachment` / `biz_task_attachment`）—— **AUD-01**
--
-- # 这条迁移补的是什么
--
-- 两张附件表都只存了上传人 id（`biz_doc_attachment.uploaded_by` /
-- `biz_task_attachment.create_by`），**没有姓名**。界面上因此只能显示 UUID，
-- 或者靠实时 `JOIN sys_user` 现取名字 —— 后者在「用户改名 / 销号」之后就回不到
-- 「当时是谁传的」了，而附件上传人恰恰是**审计属性**。
--
-- 判断标准（AGENTS.md 约定 13 + 本卡 §判断标准）：
--
--   审计 / 历史类 → 必须存姓名快照（改名 / 删号后要能回溯"当时是谁"）
--       状态变更日志 operator_name ✅ / 操作日志 username ✅ / 安全日志 username ✅
--       流程审批人 assignee_name ✅ / 流程发起人 initiator_name ✅
--       附件上传人 ❌ ← 本迁移补
--       任务附件上传人 ❌ ← 本迁移补
--
--   业务展示 / 主数据关联类 → 实时 join 最新名才对（合同负责人 owner、佣金业务员
--       salesperson、客户 / 物料 / 部门名……），**不在本迁移范围**（AUD-02 判断）。
--
-- # 为什么是「加一列」而不是「改查询」
--
-- 读路径本来就有 `LEFT JOIN sys_user`（ATT-01 / ATT-03 为了不显示 UUID 加的）。
-- 那一步只能给出**现值**。快照列落库之后，读路径改为
-- `COALESCE(NULLIF(a.uploader_name,''), <实时 join>)`：
--
--   · 有快照 → 用快照（改名后仍显示上传当时的名字，验收⑤）；
--   · 没快照（本迁移之前的历史行，回填还没跑）→ 回落到实时 join，界面不会出现空白；
--   · 两者都没有 → 空串（前端显示 `-`），**永不回落 UUID**。
--
-- 这样「先上代码、后跑回填」与「先回填、后上代码」两种顺序都不会出现 UUID。
--
-- # 列的性质
--
--   varchar(64)  可空
--     可空是必须的：① 历史行要等回填（回填是一次性脚本，
--     `scripts/datafix/aud01/`，**不进迁移** —— 迁移在启动链上跑，大库回填会拖慢启动，
--     而且回填依赖 sys_user 的当下现状，属于数据修复不属于结构变更）；
--     ② 极端情况下上传人 id 本身为空（系统代传 / 历史夹具）。
--     读路径对 NULL 有明确回落口径，不靠 NOT NULL 兜底。
--
--   不加外键：姓名是**快照值**，不是对 sys_user 的引用（引用已经是 uploaded_by /
--     create_by）。加了外键反而会禁止「用户被物理清理后快照还在」这件事。
--
-- # 幂等 / 回滚
--
--   `ADD COLUMN IF NOT EXISTS`；重复执行不报错、不覆盖已有值。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：`V0352__attachment_uploader_name.down.sql` —— 列可丢、数据不可恢复，
--   down 文件头写明了这一点（回滚后界面回到「实时 join 现值」口径，不会显示 UUID）。
-- ============================================================

-- ============================================================
-- §1 加列
-- ============================================================
ALTER TABLE biz_doc_attachment
    ADD COLUMN IF NOT EXISTS uploader_name varchar(64);

ALTER TABLE biz_task_attachment
    ADD COLUMN IF NOT EXISTS uploader_name varchar(64);

-- ============================================================
-- §2 列注释（语义写进库，别只写在代码注释里）
-- ============================================================
COMMENT ON COLUMN biz_doc_attachment.uploader_name IS
    '上传人姓名**快照**（AUD-01）：写入时从 sys_user 取 real_name / username 一次性落库，此后不随用户改名而变；历史行由 scripts/datafix/aud01 回填，NULL 时读路径回落实时 join';

COMMENT ON COLUMN biz_task_attachment.uploader_name IS
    '上传人姓名**快照**（AUD-01）：口径同 biz_doc_attachment.uploader_name；上传人 id 在 create_by（行级删除判据用它）';

-- ============================================================
-- §3 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    n_col   integer;
    n_type  integer;
    n_null  integer;
BEGIN
    -- ① 两列都在
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name IN ('biz_doc_attachment', 'biz_task_attachment')
       AND column_name = 'uploader_name';
    IF n_col <> 2 THEN
        RAISE EXCEPTION 'V0352 自检失败：预期 2 列 uploader_name，实际 %', n_col;
    END IF;

    -- ② 类型 = character varying(64)（长度写死，防止有人手改成 text）
    SELECT count(*) INTO n_type
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name IN ('biz_doc_attachment', 'biz_task_attachment')
       AND column_name = 'uploader_name'
       AND data_type = 'character varying'
       AND character_maximum_length = 64;
    IF n_type <> 2 THEN
        RAISE EXCEPTION 'V0352 自检失败：预期 2 列 varchar(64)，实际 %', n_type;
    END IF;

    -- ③ 必须可空（历史行要等一次性回填）
    SELECT count(*) INTO n_null
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name IN ('biz_doc_attachment', 'biz_task_attachment')
       AND column_name = 'uploader_name'
       AND is_nullable = 'YES';
    IF n_null <> 2 THEN
        RAISE EXCEPTION 'V0352 自检失败：uploader_name 必须可空（历史行待回填），实际可空列 % 个', n_null;
    END IF;

    RAISE NOTICE 'V0352 完成：biz_doc_attachment / biz_task_attachment 各 1 列 uploader_name varchar(64) 就绪';
END $$;
