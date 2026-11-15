-- V0049 回滚：索引随表删除会一并消失，这里显式 DROP 以支持「回滚索引但留表」的场景。
DROP INDEX IF EXISTS idx_sys_notice_delivery_retry;
DROP INDEX IF EXISTS uk_sys_notice_delivery;

DROP INDEX IF EXISTS idx_sys_notice_receiver_list;
DROP INDEX IF EXISTS idx_sys_notice_receiver_unread;
DROP INDEX IF EXISTS uk_sys_notice_receiver;

DROP INDEX IF EXISTS idx_sys_notice_biz;
DROP INDEX IF EXISTS idx_sys_notice_created;
DROP INDEX IF EXISTS uk_sys_notice_idem;
DROP INDEX IF EXISTS uk_sys_notice_no;
