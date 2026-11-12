-- ======================================================
-- 偏好字典 ui.palette 的值域白名单：接入 gallery 77 套配色方案
--
-- 背景（T-ac0466d8）：前端 `style/palettes.ts` 现在有 99 套方案
-- （云枢精选 22 + gallery 77）。后端 `sys_preference_def.validator` 里
-- `oneof:` 白名单仍只有 22 个 id，用户选 gallery 方案会被 400 + 62022 拒掉：
-- **本地立即生效、但刷新/换设备后被后端旧值覆盖**，表现就是「选了不保存」。
--
-- 为什么还要 ALTER 列宽：V0091 建表时 `validator varchar(300)`，
-- 22 个 id 的 oneof 已经是 250 字符，99 个 id 约 1500 字符必然溢出。
-- 这一列是「校验规则文本」，加宽向后兼容（读取方只当字符串解析）。
--
-- 事实来源：`frontend/src/style/palettes.ts` 的 id 列表（生成器
-- scripts/verify/gen_palette_schemes.py 产出 gallery 段）。
-- 新增方案时这里必须同步追加，否则该方案无法持久化。
--
-- 顺序不参与校验（oneof 只判集合），此处按 palettes.ts 的数组顺序书写。
-- ======================================================

ALTER TABLE sys_preference_def
    ALTER COLUMN validator TYPE varchar(2000);

UPDATE sys_preference_def
SET validator = 'oneof:mist-blue,deep-sea,teal-sunset,rose-gold,violet-honey,jade-crimson,glaze-teal,inkberry,clear-sky,peach-sky,cream-celadon,celadon-mist,spring-sugar,coral-bay,peacock,amber,jade,rouge,grape,moon-white,clear-mist,deep-blue-orange,nat-canglan-naiyou,nat-yunhui-tuo,nat-shenhai-wulan,guci-jiutao,guci-hulu,guci-zhuangxin,guci-zhuangxin-2,guci-wanhua,guci-huaniao,guci-meiping,guci-fushou,nat-dizhonghai,guci-qingluan,guci-qianli,guci-yuanshan,nat-danxia,pms-sports-outdoor,pms-popular,pms-sports-outdoor-2,pms-sports-outdoor-3,pms-sports-outdoor-4,pms-shuangzi,pms-chengbao,pms-xinchao,pms-sports-outdoor-5,nat-img9151,nat-img9152,nat-img9153,nat-img9154,nat-img9155,nat-img9156,nat-img9157,nat-img9158,nat-img9159,guci-youlibai,guci-ciyun,guci-haishui,guci-yingyue,nat-wulan-qing,nat-img9165,nat-img9166,nat-img9167,nat-img9168,nat-img9169,nat-img9170,pms-sports-outdoor-6,pms-sports-outdoor-7,pms-shanye,pms-sports-outdoor-8,pms-migua,pms-sports-outdoor-9,pms-shenshuilan,pms-yueliang,pms-miaomiao,pms-kaoguo,pms-xingrenlao,pms-heimei,pms-sports-outdoor-10,pms-hejin,pms-shanchuang,pms-sports-outdoor-11,pms-dama,pms-shanxi,nat-persimmon,nat-crested-ibis,nat-bee-eater,nat-osmanthus,nat-muhly-grass,nat-huxiu,nat-oak,nat-maple,nat-chanterelle,guci-fenqiang,guci-chishui,guci-xuanzhi,guci-yuedong,guci-gongming',
    updated_at = CURRENT_TIMESTAMP
WHERE pref_key = 'ui.palette';

-- 兜底自检：白名单里数不到 99 个 id 就是迁移写歪了（值域会静默少方案）。
-- 分母引用列值（`n`）而不是常量：PostgreSQL 会在**规划期**折叠常量表达式，
-- 写成 `CASE WHEN n = 99 THEN 1 ELSE 1 / 0 END` 即使 n=99 也会直接 division_by_zero（实测踩过）。
-- 另外不要写 DO 块：迁移执行器按语句切分，`$$` 包裹的 PL/pgSQL 块会让整批回滚。
SELECT n / CASE WHEN n = 99 THEN 1 ELSE 0 END AS palette_whitelist_ok
  FROM (
        SELECT cardinality(string_to_array(regexp_replace(validator, '^oneof:', ''), ',')) AS n
          FROM sys_preference_def
         WHERE pref_key = 'ui.palette'
       ) t;
