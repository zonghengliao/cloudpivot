-- V0063 回滚：删除 `sys_role_mutex` 整表。
--
-- 数据影响：全部互斥组（含互斥理由留痕）丢失 —— 这是**合规数据**，
-- 生产回滚前必须先导出：`COPY sys_role_mutex TO ...`。
-- 回滚后 `sod::ensure_no_mutex` 的校验会查不到任何互斥对（若代码未同时回滚），
-- 即「互斥静默失效」；因此回滚顺序是「先回滚 B7 代码、再回滚本迁移」。
--
-- 索引 uk_sys_role_mutex_pair（表达式唯一）与 idx_sys_role_mutex_b 随表消失。

DROP TABLE IF EXISTS sys_role_mutex;
