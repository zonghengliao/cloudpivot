-- ============================================================================
-- V0369：性能基线（N-8）—— 两张**列表页排序索引**
--
-- 卡：t_dd421965（NEAR-FIX-7【N-8】性能基线：k6 脚本 + 10 万行数据集验证）
-- 依据：docs/运维规程-性能基线.md §4「索引缺口」/ §5「慢查询与处置」
--
-- # 为什么需要（10 万行实测，145 的 cloudpivot_perf）
--
--   两条列表的 ORDER BY 在 10 万行时**没有任何索引能提供顺序**，优化器只能
--   「全表扫 + top-N 排序」：
--
--     GET /business/task/page       ORDER BY t.due_date ASC NULLS LAST,
--                                            t.created_at DESC, t.id DESC
--       → Seq Scan biz_task 100 006 行 + 两个 LEFT JOIN + Sort ：**64.1 ms**
--       → 加本索引后：Index Scan，取 20 行即停           ：**0.11 ms**（第 1 页）
--       → 第 1000 页（OFFSET 19980）仍从 30.6 ms 降到 7.5 ms
--
--     GET /business/erp/voucher/page ORDER BY voucher_date DESC,
--                                             voucher_word DESC, voucher_no DESC
--       → 全表 100 211 行 + Sort（idx_biz_voucher_date 只锚 voucher_date，
--         并列 voucher_date 仍要排序）                     ：**45.6 ms**
--       → 加本索引后第 1000 页                            ：**5.3 ms**
--
--   端到端（HTTP，真实令牌）：任务列表 85.3 → 39.2 ms，凭证列表第 1000 页
--   78.8 → 22.1 ms。余下那 30 余 ms 是 count(*)（见基线文档 §5.2，属已知项）。
--
-- # 设计取舍
--
--   ① **列序与 SQL 的 ORDER BY 逐字一致**（含 `NULLS LAST`）：只要差一个
--      NULLS 方向，索引就给不出顺序，优化器会退回 Sort —— 这类索引写错不会报错，
--      只会静默失效。`business/task/task_model_query.rs` 与
--      `business/erp/voucher_query.rs` 里的 ORDER BY 常量是**唯一真源**，
--      改那边必须成对改这里。
--   ② **partial（WHERE deleted_at IS NULL）**：两条列表都恒带
--      `deleted_at IS NULL`，软删行永远不展示。排除后索引体积 ≈ 活行数，
--      10 万行实测 task 4.9 MB / voucher 4.0 MB。
--      代价：查询里必须**逐字**出现该谓词才会被选中（现有 SQL 满足）。
--   ③ **不做覆盖索引（INCLUDE）**：任务列表要 26 列、凭证列表要 20 列，全带上
--      索引会膨胀 5~8 倍；回表 20 次（LIMIT 20）本来就是常数级开销。
--
-- # 回滚
--
--   V0369__perf_list_order_indexes.down.sql
--   （删索引不影响任何数据；删后列表退化为全表扫 + 排序，见上面的数字）
-- ============================================================================

-- 任务列表：ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC
CREATE INDEX IF NOT EXISTS idx_biz_task_list_order
    ON biz_task (due_date ASC NULLS LAST, created_at DESC, id DESC)
 WHERE deleted_at IS NULL;

-- 凭证列表：ORDER BY voucher_date DESC, voucher_word DESC, voucher_no DESC
CREATE INDEX IF NOT EXISTS idx_biz_voucher_list_order
    ON biz_voucher (voucher_date DESC, voucher_word DESC, voucher_no DESC)
 WHERE deleted_at IS NULL;

COMMENT ON INDEX idx_biz_task_list_order IS
    'N-8 列表基数索引：给 GET /business/task/page 的 ORDER BY 供序（partial: deleted_at IS NULL）';
COMMENT ON INDEX idx_biz_voucher_list_order IS
    'N-8 列表基数索引：给 GET /business/erp/voucher/page 的 ORDER BY 供序（partial: deleted_at IS NULL）';
