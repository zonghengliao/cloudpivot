# 关于 `03_cargo_test_summary.out` 里那一条 doctest 失败（非本卡）

`cargo test` 全量输出的结尾：

```
test result: ok. ... (28 个目标全部 ok，0 failed)
   Doc-tests cloudpivot
error[E0405]: cannot find trait `Deserializer` in this scope
   --> src/business/erp/cost_object_dto.rs:593:8
```

**这条与本卡（COST-B1-5）无关**，判据：

1. 报错文件 `backend/src/business/erp/cost_object_dto.rs` **不在 git 索引里**
   （`git ls-files --error-unmatch` → `did not match any file(s) known to git`），
   是并发卡（成本对象模块）正在写的未提交文件；本卡提交里不含它。
2. 28 个测试目标全部 `test result: ok`，其中本卡新增目标
   `cost_b1_5_stock_out_in_cost_object_migration` 13 passed / 0 failed。
3. 该文件在本卡 `cargo test` 进行中**被别的 worker 持续改动**
   （lib 目标先编译通过、doctest 阶段又编到新版本），属共享工作区的时间竞态。
4. 本卡只新增 4 个 `.sql` + 1 个测试文件 + docs，**没有改任何 `src/`**（`git show --stat` 可验）。

复跑本卡的确定性证据：

```bash
cd backend && cargo test --test cost_b1_5_stock_out_in_cost_object_migration   # 13 passed
```
