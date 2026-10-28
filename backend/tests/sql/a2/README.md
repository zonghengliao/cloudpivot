# A2 验证脚本（对象存储接口与兼容）

> 全部脚本**真跑过**，对应卡 `t_75330dcf`（批次 A · A2）的四条硬指标。
> 运行环境：`172.18.10.144`，profile=test、库 `cloudpivot_test`、端口 8025；
> S3 侧是 `172.18.10.99:9000` 上的 MinIO（bucket `cloudpivot-test`）。

## 脚本清单

| 脚本 | 对应验收 | 说明 |
|------|---------|------|
| `smoke_legacy.sh` | ① 旧接口零回归 | `/support/file/*` 七个接口逐个打：upload / upload_batch / check_duplicate / page / detail / download / delete，断言状态码、响应字段、内容 sha256、.exe 拒绝、删后 404 |
| `smoke_new.sh` | ② 新接口闭环 | `/business/storage/*`：上传 → 列表（含 hash 过滤）→ 下载 sha256 → 软删 → 下载 404 → rebind 复活；另含 presign 本地后端 422、complete 对象缺失 404、超限 422、扩展名 400 |
| `rss_download.sh` | ③ 本地流式下载 | 造 1 GiB 文件 + 直接插一条历史语义（`storage_code IS NULL`）记录，下载期间每 0.2 s 采样 `/proc/<pid>/status` 的 VmRSS，打印基线/峰值/增量 |
| `minio_e2e.sh` | ④ S3 真读写 | 元数据端点被 SSRF 拒绝 → 建 S3 后端 → 测试连接 → 设默认 → 预签名 PUT 直传 → complete 登记 → 302 下载 sha256 → 12 MB 服务端上传（分片）→ 下载 sha256 → 软删 → 404 |
| `mk_bucket.py` | ④ 环境准备 | 用 SigV4 直接 `PUT /<bucket>` 建桶（本环境无 `mc`，`MINIO_DEFAULT_BUCKETS` 未生效） |
| `align_test_policy.sh` | 环境准备 | 把 A1 冒烟留下的 `attachment` 策略行对齐到 V0048 设计值（V0048 用 `ON CONFLICT DO NOTHING`，不覆盖既有行） |
| `flip_default.sh` | ① 加强验证 | 切换默认后端后重跑旧接口回归（证明旧接口完全不依赖存储路由） |

## 用法

```bash
# 0) 起测试后端（关键：必须带 SSRF 例外白名单，否则私网 endpoint 会被拒）
cd /root/a2-test-run && ( set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a; \
  export CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST=172.18.10.99; \
  setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/cloudpivot-test-a2.log 2>&1 & )
curl -s http://127.0.0.1:8025/health

# 1..4) 四条验收
bash backend/tests/sql/a2/smoke_legacy.sh
bash backend/tests/sql/a2/smoke_new.sh
bash backend/tests/sql/a2/rss_download.sh
bash backend/tests/sql/a2/minio_e2e.sh <minio_ak> <minio_sk>

# S3 后端级集成测试（证明 DELETE 真的删掉了对象、分片合并正确）
CLOUDPIVOT_MINIO_ENDPOINT=http://172.18.10.99:9000 \
CLOUDPIVOT_MINIO_BUCKET=cloudpivot-test \
CLOUDPIVOT_MINIO_AK=... CLOUDPIVOT_MINIO_SK=... \
cargo test --test s3_minio -- --nocapture --test-threads=1
```

## 注意

1. 测试后端的工作目录必须是**含 `config/` 与 `migrations/`** 的目录（框架按 CWD 找），
   否则启动时 `数据库未初始化` 直接 panic；本环境用 `/root/a2-test-run`（`migrations` 是软链）。
2. 端口 8023 是生产、8024 是 dev，**不要** pkill 全局 `cloudpivot`，只 kill
   `^/opt/cloudpivot/backend/target/debug/cloudpivot`。
3. 脚本里 `BASE=http://127.0.0.1:8025/api`：后端 `[router].prefix="/api"`，漏了会命中 SPA fallback。
