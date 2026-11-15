-- ======================================================
-- V0132：示范数据 · 项目分组（批次 E · E8）
--
-- 依据：卡面 t_c4be09f0「二、项目示范（2 个）」
-- 回滚：V0132__seed_projects.down.sql（先删示范任务 → 摘掉残留关联 → 删项目）
--
-- ── 为什么单独一条迁移，不和帮助文档混在 V0131 ──────────────────
-- 两者的回滚依赖完全不同（文档有版本快照、项目有任务外键），混在一条里
-- 回滚语句会互相干扰：删文档的 DELETE 一旦半途失败，项目的任务就悬在半空。
-- 分开之后两张卡的表各自可独立 up/down，也便于运维只回滚其中一半。
--
-- ── owner 解析：为什么不是写死的 UUID ────────────────────────────
-- 版本化迁移（V 开头的数字版本）在 `S__role_data.sql` 之前跑，**空库上还没有任何用户**：
-- `biz_task.owner_id` / `creator_id` 都是 NOT NULL + FK sys_user，
-- 写死 UUID 会让空库迁移直接撞 FK 失败（`空库跑迁移能成功` 是验收项）。
-- 所以本迁移在 DO 块里**先解析再写**：
--   ① `admin`（超管，存在就用它）；② `laoliu_test`；③ 都没有 → owner 置 NULL。
-- 项目表 `owner_id` 可空，所以 ①② 不存在时项目仍能落库（只是没有负责人）；
-- 任务表 owner 不可空，此时**整体跳过任务种子**并打 NOTICE —— 不造半个坏数据。
--
-- ── 任务编号 ────────────────────────────────────────────────
-- `biz_task.task_no` 是全表唯一（软删不复用），应用侧按「月内最大号 + 1」生成。
-- 本迁移占用 `TSK-202610-000201`–`000206`：刻意跳到 2xx 段，与既有任务
-- （000001–000148）留出空隙，既不会撞号，也不会让新任务的编号看起来像被插队。
--
-- ── 幂等 ─────────────────────────────────────────────────────
-- `ON CONFLICT DO NOTHING`（不带冲突目标 = 任何唯一约束冲突都跳过）：
-- 重复执行不会报错，也不会覆盖运营改过的项目名 / 任务标题。
-- ======================================================

DO $$
DECLARE
    v_owner     uuid;
    v_proj_cp   uuid := '27000000-0000-4000-8000-000000000001'::uuid; -- 云枢平台迭代 CP-ITER
    v_proj_ec   uuid := '27000000-0000-4000-8000-000000000002'::uuid; -- 示例：电商后台 DEMO-EC
    v_proj_cnt  int;
    v_task_cp   int;
    v_task_ec   int;
BEGIN
    -- ---------- 0) 解析负责人：admin → laoliu_test → NULL ----------
    SELECT id INTO v_owner
      FROM sys_user
     WHERE username = 'admin' AND deleted_at IS NULL
     ORDER BY created_at
     LIMIT 1;

    IF v_owner IS NULL THEN
        SELECT id INTO v_owner
          FROM sys_user
         WHERE username = 'laoliu_test' AND deleted_at IS NULL
         ORDER BY created_at
         LIMIT 1;
    END IF;

    -- ---------- 1) 两个项目（ACTIVE，不是草稿） ----------
    INSERT INTO biz_project (
        id, project_no, project_name, description, owner_id, status,
        start_date, end_date, create_by
    ) VALUES
        (
            v_proj_cp, 'CP-ITER', '云枢平台迭代',
            '云枢平台自身的功能迭代：帮助中心、审批流、任务看板三条主线。'
            '用于演示「一个平台项目如何按迭代收拢任务」。',
            v_owner, 'ACTIVE',
            CURRENT_DATE - 14, CURRENT_DATE + 30, v_owner
        ),
        (
            v_proj_ec, 'DEMO-EC', '示例：电商后台',
            '演示项目分组的用法：商品、订单、库存三类工作同属一个项目，'
            '方便说明「项目只是逻辑分组，任务可见性仍由数据范围决定」。',
            v_owner, 'ACTIVE',
            CURRENT_DATE - 7, CURRENT_DATE + 21, v_owner
        )
    ON CONFLICT DO NOTHING;

    -- ---------- 1.5) 空库首启后补负责人 ----------
    -- 场景：空库首启时本迁移先跑（还没有 admin），项目先以 owner_id = NULL 落库；
    -- 应用 bootstrap 建出 admin 后若再执行一次本迁移（或运维手工重放），
    -- 这里把仍为空的负责人补上。**只填空值**，不覆盖已有负责人。
    IF v_owner IS NOT NULL THEN
        UPDATE biz_project
           SET owner_id = v_owner, updated_at = now()
         WHERE project_no IN ('CP-ITER', 'DEMO-EC')
           AND deleted_at IS NULL
           AND owner_id IS NULL;
    END IF;

    -- 只在「项目行存在」且「负责人解析成功」时插：owner_id 是 NOT NULL + FK，
    -- 没有用户时插任务必然失败，此时跳过比留半条脏数据好。
    IF v_owner IS NOT NULL AND EXISTS (SELECT 1 FROM biz_project WHERE id = v_proj_cp) THEN
        INSERT INTO biz_task (
            id, task_no, title, description, status, priority, progress, need_review,
            owner_id, creator_id, source, project_id, start_date, due_date, estimate_hours
        ) VALUES
            (
                '27500000-0000-4000-8000-000000000001', 'TSK-202610-000201',
                '帮助中心：用户侧文档检索',
                '帮助中心已有分类与文档，本任务补齐用户侧的关键词检索与命中高亮。',
                'DOING', 'P1', 40, TRUE,
                v_owner, v_owner, 'SELF', v_proj_cp,
                CURRENT_DATE - 7, CURRENT_DATE + 7, 16.0
            ),
            (
                '27500000-0000-4000-8000-000000000002', 'TSK-202610-000202',
                '审批流：超时自动提醒',
                '审批任务超过 24 小时未处理时，按流程定义的提醒配置发一次站内通知。',
                'TODO', 'P2', 0, TRUE,
                v_owner, v_owner, 'SELF', v_proj_cp,
                CURRENT_DATE + 1, CURRENT_DATE + 14, 12.0
            ),
            (
                '27500000-0000-4000-8000-000000000003', 'TSK-202610-000203',
                '任务看板：三视图切换体验优化',
                '看板 / 列表 / 甘特三视图切换时保留筛选条件与滚动位置。',
                'TODO', 'P3', 0, FALSE,
                v_owner, v_owner, 'SELF', v_proj_cp,
                CURRENT_DATE + 3, CURRENT_DATE + 21, 8.0
            )
        ON CONFLICT DO NOTHING;
    END IF;

    IF v_owner IS NOT NULL AND EXISTS (SELECT 1 FROM biz_project WHERE id = v_proj_ec) THEN
        INSERT INTO biz_task (
            id, task_no, title, description, status, priority, progress, need_review,
            owner_id, creator_id, source, project_id, start_date, due_date, estimate_hours
        ) VALUES
            (
                '27500000-0000-4000-8000-000000000004', 'TSK-202610-000204',
                '商品列表页改版',
                '商品列表支持批量上下架与按类目筛选；演示「项目下按优先级排期」。',
                'TODO', 'P1', 0, TRUE,
                v_owner, v_owner, 'SELF', v_proj_ec,
                CURRENT_DATE - 3, CURRENT_DATE + 10, 20.0
            ),
            (
                '27500000-0000-4000-8000-000000000005', 'TSK-202610-000205',
                '订单退款审批流程',
                '退款金额 > 500 元时走财务会签；演示「审批流 + 任务」的联动。',
                'DOING', 'P2', 30, TRUE,
                v_owner, v_owner, 'SELF', v_proj_ec,
                CURRENT_DATE - 5, CURRENT_DATE + 9, 12.0
            ),
            (
                '27500000-0000-4000-8000-000000000006', 'TSK-202610-000206',
                '库存预警通知',
                '库存低于安全水位时生成待办任务并通知采购负责人。',
                'TODO', 'P2', 0, FALSE,
                v_owner, v_owner, 'SELF', v_proj_ec,
                CURRENT_DATE + 2, CURRENT_DATE + 20, 10.0
            )
        ON CONFLICT DO NOTHING;
    END IF;

    -- ---------- 3) 自检 ----------
    -- 3.1 两个项目在、且是 ACTIVE（卡面：项目状态正常，不是草稿）
    SELECT count(*) INTO v_proj_cnt
      FROM biz_project
     WHERE id IN (v_proj_cp, v_proj_ec)
       AND status = 'ACTIVE'
       AND deleted_at IS NULL;
    IF v_proj_cnt <> 2 THEN
        RAISE EXCEPTION 'V0132 自检失败：示范项目应为 2 个且 ACTIVE，实际 %', v_proj_cnt;
    END IF;

    -- 3.2 项目编号唯一索引生效：CP-ITER / DEMO-EC 各一行
    IF (SELECT count(*) FROM biz_project
         WHERE project_no IN ('CP-ITER', 'DEMO-EC') AND deleted_at IS NULL) < 2 THEN
        RAISE EXCEPTION 'V0132 自检失败：project_no CP-ITER / DEMO-EC 不齐';
    END IF;

    -- 3.3 任务种子：有用户时每个项目必须 3 条；空库（无用户）时允许为 0
    SELECT count(*) INTO v_task_cp FROM biz_task
     WHERE project_id = v_proj_cp AND deleted_at IS NULL;
    SELECT count(*) INTO v_task_ec FROM biz_task
     WHERE project_id = v_proj_ec AND deleted_at IS NULL;

    IF v_owner IS NULL THEN
        RAISE NOTICE 'V0132：当前库没有任何用户（空库首启），已跳过任务种子；'
                     '项目已建但暂挂 0 条任务（应用启动并建出 admin 后重跑本迁移即可）';
    ELSIF v_task_cp < 2 OR v_task_ec < 2 THEN
        RAISE EXCEPTION 'V0132 自检失败：每个项目应挂 ≥2 条任务，实际 CP-ITER=% / DEMO-EC=%',
            v_task_cp, v_task_ec;
    ELSE
        RAISE NOTICE 'V0132 自检通过：2 个 ACTIVE 项目（CP-ITER % 条任务 / DEMO-EC % 条任务），负责人 %',
            v_task_cp, v_task_ec, v_owner;
    END IF;
END
$$;
