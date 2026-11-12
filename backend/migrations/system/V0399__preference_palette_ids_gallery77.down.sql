-- 回滚：ui.palette 值域白名单恢复为底座自带的 22 套
--
-- 列宽不回滚（varchar(2000) → varchar(300) 会失败：此时 22 个 id 的规则文本
-- 仍在 300 字符以内，但回滚收窄列宽没有任何收益，反而可能在别的偏好键上炸）。
--
-- 注意：回滚后用户已选的 gallery 方案（如 guci-jiutao）留在 sys_user_preference 里，
-- 但再次写入会被 62022 拒掉；读取时前端 `isPaletteId()` 也会回退默认方案，
-- 所以界面不会崩，只是这几套方案不可再选。
UPDATE sys_preference_def
SET validator = 'oneof:mist-blue,deep-sea,teal-sunset,rose-gold,violet-honey,jade-crimson,glaze-teal,inkberry,clear-sky,peach-sky,cream-celadon,celadon-mist,spring-sugar,coral-bay,peacock,amber,jade,rouge,grape,moon-white,clear-mist,deep-blue-orange',
    updated_at = CURRENT_TIMESTAMP
WHERE pref_key = 'ui.palette';
