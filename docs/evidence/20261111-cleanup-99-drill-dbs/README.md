# 99 上历史演练库清单（清理前留档）

> 生成时间：2026-11-11 20:38:45
> 来源：99（Hermes Agent 主机）的 PostgreSQL
> 用途：这些是过去在 99 上跑测试留下的临时库，全部无活动连接

## 清单

```
库名|大小|字节|连接数|事务数
cloudpivot_e2e_iso|36 MB|37271231|0|82810
bizperm_drill_3|34 MB|35657407|0|17925
bizperm_drill_2|33 MB|34854591|0|17786
bizperm_drill_1|33 MB|34846399|0|17818
cp_drill_99|30 MB|31889087|0|12909
cloudpivot_e2e08fix1|29 MB|30185151|0|10152
cloudpivot_e2efix2|29 MB|29931199|0|134257
cloudpivot_p15_e2e|28 MB|29882047|0|22340
cloudpivot_e2e04fix2|28 MB|29726399|0|137232
cloudpivot_e2e04fix3|28 MB|29374143|0|120027
cloudpivot_e2e08fix2|27 MB|28055231|0|94538
cost_b42_e2e|27 MB|28038847|0|4315
costb31_drill|26 MB|27449023|0|45561
cp_bug01_e2e|26 MB|27408063|0|138731
cp_e2efix6|26 MB|27154111|0|63928
cp_aud02_e2e|26 MB|27137727|0|161449
costb26_drill_1625844|26 MB|27072191|0|30839
cloudpivot_e2e_iso2|26 MB|26826431|0|91455
cost_inspect_drill|25 MB|26613439|0|5953
cloudpivot_e2e_iso_t995|25 MB|26400447|0|33233
cloudpivot_e2e_iso_t995b|25 MB|26400447|0|32378
cloudpivot_e2e_t421|25 MB|26400447|0|32943
cloudpivot_e2e_iso_fe|25 MB|26367679|0|32327
b112e2e2|25 MB|26310335|0|3224
mig_costb15_drill|25 MB|26236607|0|2394
b112e2e|25 MB|26187455|0|2561
cp_e2efix5|25 MB|25794239|0|6833
cp_e2efix5_t|24 MB|25638591|0|14131
cloudpivot_p304_e2e|24 MB|25401023|0|15917
cloudpivot_p309_drill|24 MB|25368255|0|11882
cp_e2efix5_t3|24 MB|25310911|0|11626
cp_e2efix3|24 MB|25269951|0|5514
cp_e2efix1|24 MB|25220799|0|10073
cp_e2e05fix3_ut|24 MB|25220799|0|4399
cloudpivot_e2efix8|24 MB|25212607|0|10672
cloudpivot_e2e05fix2|24 MB|25212607|0|10763
cp_aud01_e2e|24 MB|24958655|0|7191
cp_e2efix4|24 MB|24745663|0|7515
cp_libtest_e2e04aux|24 MB|24680127|0|7969
cloudpivot_e2e07fix1|23 MB|24499903|0|5702
cloudpivot_code07e2e|23 MB|24491711|0|10383
cloudpivot_code04e2e|23 MB|24475327|0|8764
cloudpivot_p302_e2e|23 MB|24458943|0|10273
cloudpivot_p303_e2e|23 MB|24434367|0|9698
cp_libtest_e2e04aux_p0fix|23 MB|24368831|0|4370
cp_drill_t8|22 MB|23107263|0|6485
cloudpivot_so32_drill|21 MB|22279871|0|11860
cloudpivot_so22e2e|21 MB|21771967|0|13371
cloudpivot_fin3drill|20 MB|21157567|0|17071
cloudpivot_fin2drill|20 MB|20879039|0|18163
cloudpivot_so16_drill|20 MB|20854463|0|12657
cp_p21c_e2e|19 MB|20272831|0|17620
cp_p21c_e2e_base|19 MB|20190911|0|16919
cloudpivot_p25a_e2e|19 MB|19936959|0|17806
cloudpivot_p25a_e2e2|19 MB|19764927|0|17526
cloudpivot_p28e2e|18 MB|19330751|0|18548
cloudpivot_p24e2e|18 MB|19166911|0|24332
cp_dc13_e2e|18 MB|18986687|0|21068
cp_p13_e2e|18 MB|18871999|0|26581
cp_p14_e2e|18 MB|18757311|0|29753
cloudpivot_3b_e2e|18 MB|18568895|0|46609
cp_units_e2e|18 MB|18413247|0|24913
cp_units_e2e2|18 MB|18380479|0|24891
cloudpivot_a4_e2e|17 MB|18306751|0|26041
cloudpivot_p16md_e2e|17 MB|18241215|0|20774
erpb2_e2e|17 MB|18003647|0|26055
cloudpivot_d1e2e|17 MB|17815231|0|25237
cloudpivot_d4e2e|16 MB|17102527|0|28903
cloudpivot_e2e_t421b|16 MB|16553663|0|31499
```

## 处置

```
· 抽样 dump 2 个（最大的 + 最新的）到 145
· 其余全部 DROP
```

## 执行结果

```
删除成功: 69
删除失败: 0
抽样备份: 2 个 → 145:/root/db-backup/from-99-drill-dbs/
执行时间: 2026-11-11 20:38:56
```
