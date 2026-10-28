# A2「存储接口与兼容」验证证据（t_75330dcf）

- 交付提交：`882147e feat(storage): A2 存储接口 —— S3 真读写 + 流式下载 + 预签名直传 + 旧接口零回归`
- 仓库：`172.18.10.144:/opt/cloudpivot`（origin = `/opt/git/cloudpivot.git`，`git rev-parse HEAD == origin/main`）
- 提交前核对：`git rev-list --count HEAD..origin/main` = **0**（已包含 A3 的 66eefb1，线性提交，无冲突）
- 验证环境：profile=test、库 `cloudpivot_test`、127.0.0.1:8025（dev 8024 / prod 8023 未触碰）
- S3 服务：MinIO `RELEASE.2025-09-07T16-13-09Z` 跑在 `172.18.10.99:9000`，bucket `cloudpivot-test`
- 脚本归档：`backend/tests/sql/a2/`（README 内含运行步骤）

> **容器 vs 二进制的偏差（如实说明）**：卡里写的是 `docker run minio/minio`，但 144 与本机
> 都**没有 docker**（`docker: command not found`），`dl.min.io` 对该版本已返回 410。改为跑
> **官方 MinIO 二进制**（GitHub release 资产，linux-arm64，sha256 `5c83cd2c…`），S3 协议、
> 签名校验、桶语义与容器镜像完全一致；AK/SK 仍走数据库加密那一套链路。

## ① 旧接口零回归 —— `smoke_legacy.sh`

```
== 旧接口回归（profile=test / cloudpivot_test） ==
  PASS  POST /support/file/upload                  200
        file_id=01a18256-3b7a-77c2-b7bf-b0aa5a7e88df url=/api/support/file/download/01a18256-3b7a-77c2-b7bf-b0aa5a7e88df
  PASS    upload 响应字段齐全                true
  PASS    upload url 指向旧下载接口         ok
  PASS    upload file_size                         33
  PASS  GET /support/file/check-duplicate 命中   true
  PASS    duplicated.file.id 相同                01a18256-3b7a-77c2-b7bf-b0aa5a7e88df
  PASS  GET /support/file/check-duplicate 未命中 false
  PASS  GET /support/file/check-duplicate 空参 400 400
  PASS  GET /support/file/page 状态              0
  PASS    page 字段齐全                        true
  PASS    本记录出现在第一页              true
  PASS  GET /support/file/{id}                     0
  PASS    detail.id 一致                         01a18256-3b7a-77c2-b7bf-b0aa5a7e88df
  PASS  GET /support/file/download/{id} sha256     0a6d9c35dca341c3300b23587740d04851d8a1bdeef19d16aa48f57ffd60ee39
  PASS  GET /support/file/download/{id} 大小     33
  PASS  POST /support/file/upload/batch 数量     2
  PASS    batch 顺序 = 提交顺序              a.txt,b.txt
  PASS  POST /support/file/upload .exe 被拒      400
  PASS  DELETE /support/file/{id}                  200
  PASS    删除后 GET /support/file/{id} 404     404
  PASS    删除后下载 404                      404
== 旧接口回归结果：PASS=21 FAIL=0 ==
```

**加强证据**：把默认后端切成 MinIO（`config/{id}/default`）后**再跑一遍**，仍 `PASS=21 FAIL=0`
—— 证明 `/support/file/*` 完全不依赖新的存储路由（`git diff` 里 `support/` 零改动）：

```
set default minio-test -> 200
  local-default	default=false	health=HEALTHY
  minio-test	default=true	health=HEALTHY
=== 旧接口在默认后端=S3 时的回归 ===
== 旧接口回归结果：PASS=21 FAIL=0 ==
set default local-default -> 200
```

## ② 新接口闭环 —— `smoke_new.sh`

```
== 新接口闭环 /api/business/storage/* ==
  PASS  POST /business/storage/upload                  200
        file_id=01a18256-3d2a-7128-8160-9d0560dd61ea
  PASS    响应含 storage_code                       local-default
  PASS    响应含 upload_mode=SERVER                 SERVER
  PASS    响应含新下载 url                       /api/business/storage/download/01a18256-3d2a-7128-8160-9d0560dd61ea
  PASS    file_size 一致                             37
  PASS  GET /business/storage/file/page                0
  PASS    列表命中本次文件                     true
  PASS    按 content hash 过滤命中                true
  PASS  GET /business/storage/download/{id} sha256     626c6c8e1f72610298e82d3bd781907ff3e18e5a4df72c6113f8ad4a76885d4b
  PASS    响应含 Content-Disposition attachment     1
  PASS    响应含 X-Content-Type-Options nosniff     1
  PASS    响应 Content-Length                        1
  PASS  POST /business/storage/presign 本地后端 422 422
  PASS    错误码 = 60006(直传未开)              60006
  PASS  POST /business/storage/complete 对象不存在 404 404
  PASS    错误码 = 60007(对象缺失)              60007
  PASS  DELETE /business/storage/{id}                  200
  PASS    软删后下载 404                          404
  PASS    重复删除幂等 200                       200
  PASS    记录仍在库（deleted_at 非空）       1
  PASS  POST /business/storage/{id}/rebind             200
  PASS    新记录 ID 不同                          yes
  PASS    新记录 file_key 相同（不复制对象） 2026/10/28/01a18256-3d29-7134-a176-587cdbd1a06d.txt
  PASS    rebind 后下载 sha256 一致               626c6c8e1f72610298e82d3bd781907ff3e18e5a4df72c6113f8ad4a76885d4b
  PASS  GET /business/storage/download/not-a-uuid 400  400
  PASS  POST /business/storage/upload 超 avatar 上限 422 422
  PASS  POST /business/storage/upload 扩展名不在策略 400 400
== 新接口闭环结果：PASS=27 FAIL=0 ==
```

## ③ 本地流式下载（1 GiB，RSS 采样）—— `rss_download.sh`

```
生成 1 GiB 测试文件 ...
== 1 GB 本地流式下载 RSS 采样（pid=380052） ==
下载字节数     : 1073741824 (期望 1073741824)
下载耗时       : 13.01s
RSS 基线       : 85904 KB
RSS 峰值       : 85904 KB
RSS 增量       : 0 MB（阈值 < 200 MB）
采样点数       : 69
前 5 个采样    : 85904 85904 85904 85904 85904
后 5 个采样    : 85792 85784 85664 85664 85664
RESULT: PASS（流式，内存不随文件增长）
```

采样方式：下载期间每 0.2 s 读一次 `/proc/<pid>/status` 的 `VmRSS`。峰值与基线相同
（增量 0 MB），说明「整个对象不进内存」；若走 `std::fs::read` 全量读，1 GiB 会让 RSS 涨约 1 GB。

## ④ S3 真读写（MinIO）—— `minio_e2e.sh` + `tests/s3_minio.rs`

```
== S3（MinIO http://172.18.10.99:9000 / bucket=cloudpivot-test）全链路 ==
  backend config_code=minio-a2-194823
  PASS  元数据端点 endpoint 被 SSRF 防护拒绝   400
  PASS    错误码 = 60009(ENDPOINT_FORBIDDEN)          60009
  PASS  POST /business/storage/config 建 S3 后端      200
  PASS    AK 只回掩码（不含明文）              1
  PASS    响应体不含明文 SecretKey                0
  PASS  POST /business/storage/config/{id}/test healthy  true
        latency_ms=1 message=连接成功
  PASS  POST /business/storage/config/{id}/default       200
  PASS  POST /business/storage/presign 返回 storage_code minio-a2-194823
  PASS    未命中秒传                                false
        upload_url=http://172.18.10.99:9000/cloudpivot-test/a2/2026/10/28/01a18257-05c1-76da-b4a9-0...（已签名，351 字节）
  PASS  PUT 预签名 URL 直传 MinIO                   200
  PASS  POST /business/storage/complete 登记           200
  PASS    upload_mode=DIRECT                             DIRECT
  PASS    storage_code=minio-a2-194823                   minio-a2-194823
  PASS  GET download S3 后端 302                       1
        Location=http://172.18.10.99:9000/cloudpivot-test/a2/2026/10/28/01a18257-05c1-7...
  PASS    302 后内容 sha256 一致                    423a12841713fa7711dc362380311aafb696872100bf92a0a306d86d2086fea2
  PASS  POST /business/storage/upload 12 MB 到 S3（分片） 200
  PASS    big storage_code=minio-a2-194823               minio-a2-194823
  PASS    12 MB 下载 sha256 一致（分片合并正确） 3132d0819152fde3ffba1d512656ce89132b4c7711fe23ba85eaea64da20538d
  PASS  DELETE /business/storage/{id}（S3 记录）     200
  PASS    软删后下载 404                            404
== S3 全链路结果：PASS=20 FAIL=0 ==
```

对象级集成测试（`cargo test --test s3_minio`，证明 **S3 DELETE 真的删掉对象**、分片合并正确）：

```
running 2 tests
test s3_minio_multipart_upload_matches ... OK s3_minio_multipart_upload_matches size=12582912 sha256=b6967a4c54cdab8a16907be0774af71e5db8198045f91933ebed106ddba22dfb
test s3_minio_real_roundtrip ... OK s3_minio_real_roundtrip key=it/roundtrip-1793188138753343621-1.txt sha256=45661e97fc58ed070dbec45dc99ca48b0b745eefaafd20d5c23af96c29fcc143
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.91s
```

`roundtrip` 用例断言链：PUT → GET（内容相等）→ HEAD(stat: size/ETag) → exists=true →
流式 GET `Range: bytes=10-` 内容相等 → **DELETE 后 exists=false、HEAD=404** → 重复 DELETE 幂等。

## ⑤ 检查链（提交后在工作树 & 干净 worktree 各跑一次）

```
clippy --all-targets -- -D warnings  →  clippy_rc=0
cargo test（干净 worktree @ 882147e） →  test result: ok. 167 passed; 0 failed / integration 2 passed
```

`rustfmt --edition 2024 --check` 对本次 6 个 Rust 文件全部 FMT_CLEAN（只格式化本次改动文件，
避免 skill §6.1-2 的「fmt 重排无关文件」噪声）。

## ⑥ 两点如实说明

1. **`attachment` 策略行**：`cloudpivot_test` 里该行是 A1 冒烟时创建的（80 MB / 不含 txt），
   V0048 用 `ON CONFLICT DO NOTHING` **正确地没有覆盖**它（这是幂等语义，不是 bug）。
   验证前用 `align_test_policy.sh` 把它对齐到 V0048 设计值（100 MB / 含 txt / TTL 900）。
2. **软删的 config_code 不可复用**：`config_create` 的唯一性检查（A1 的 `count_config_code`）
   会算上已软删行，所以删掉 `minio-test` 后再建同名后端会 409。A2 未改这条语义（属 A1 行为），
   验证脚本因此用带时间戳的 `minio-a2-HHMMSS` 编码。**建议后续卡**：唯一性检查应只算
   `deleted_at IS NULL`，或明确产品语义「编码禁止复用」。
