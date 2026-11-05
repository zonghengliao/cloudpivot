-- 重置演练库（scratch DB 内部对象；不 drop 库本身）
DROP SCHEMA IF EXISTS public CASCADE;
CREATE SCHEMA public;
DROP SCHEMA IF EXISTS di CASCADE;
