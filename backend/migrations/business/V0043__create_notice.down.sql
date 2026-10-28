-- V0043 回滚：按外键依赖倒序删除（delivery → receiver → notice）。
DROP TABLE IF EXISTS sys_notice_delivery;
DROP TABLE IF EXISTS sys_notice_receiver;
DROP TABLE IF EXISTS sys_notice;
