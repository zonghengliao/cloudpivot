-- 偏好字典种子：8 项预置偏好（配色 / 明暗 / 侧边栏 / 表格密度 / 分页 / 语言 / 站内信 / 邮件）
--
-- 幂等：ON CONFLICT (pref_key) DO UPDATE —— 重放（up→down→up）时按本文件为准，
-- 既保证空库首跑可用，也保证修默认值/校验规则时迁移可重放而不报重复键。
--
-- ⚠️ ui.palette 的 oneof 列表必须与前端 `frontend/src/style/palettes.ts` 的 22 套色板 id
--    逐字一致：多一个/少一个都会造成「界面能选、后端拒绝保存」或「库里存着前端不认识的值」。
--    色板增删时**必须**同步改这里（`scripts/check.sh` 有一条一致性门禁盯着这件事）。
INSERT INTO sys_preference_def
    (pref_key, pref_name, value_type, default_value, scope, validator, sort_order, status, remark)
VALUES
    ('ui.palette', '配色方案', 'string', '"mist-blue"'::jsonb, 'ui',
     'oneof:mist-blue,deep-sea,teal-sunset,rose-gold,violet-honey,jade-crimson,glaze-teal,inkberry,clear-sky,peach-sky,cream-celadon,celadon-mist,spring-sugar,coral-bay,peacock,amber,jade,rouge,grape,moon-white,clear-mist,deep-blue-orange',
     10, 1, '22 套色板，id 取自 style/palettes.ts；默认「雾蓝流金」'),
    ('ui.theme_mode', '明暗模式', 'string', '"light"'::jsonb, 'ui',
     'oneof:light,dark', 20, 1, '浅色 / 深色'),
    ('ui.side_compact', '侧边栏折叠', 'boolean', 'false'::jsonb, 'ui',
     NULL, 30, 1, '管理布局左侧菜单是否折叠成图标条'),
    ('ui.table_density', '表格密度', 'string', '"default"'::jsonb, 'table',
     'oneof:default,compact', 40, 1, '列表页行高密度（本卡只做读存，业务表格消费为后续增量）'),
    ('ui.page_size', '默认分页条数', 'number', '10'::jsonb, 'table',
     'range:5-200', 50, 1, '列表页默认每页条数，范围 5–200'),
    ('ui.language', '界面语言', 'string', '"zh-CN"'::jsonb, 'ui',
     'oneof:zh-CN,en-US', 60, 1, '界面语言（本卡只做读存，i18n 切换为后续增量）'),
    ('notify.inbox', '站内信开关', 'boolean', 'true'::jsonb, 'notify',
     NULL, 70, 1, '是否接收站内消息通知'),
    ('notify.email', '邮件通知开关', 'boolean', 'false'::jsonb, 'notify',
     NULL, 80, 1, '是否接收邮件通知')
ON CONFLICT (pref_key) DO UPDATE
   SET pref_name     = EXCLUDED.pref_name,
       value_type    = EXCLUDED.value_type,
       default_value = EXCLUDED.default_value,
       scope         = EXCLUDED.scope,
       validator     = EXCLUDED.validator,
       sort_order    = EXCLUDED.sort_order,
       status        = EXCLUDED.status,
       remark        = EXCLUDED.remark,
       updated_at    = now();
