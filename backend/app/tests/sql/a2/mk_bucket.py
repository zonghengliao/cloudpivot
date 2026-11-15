#!/usr/bin/env python3
"""在 MinIO 上建桶（SigV4 签名的 PUT /<bucket>，只用于 A2 验证环境准备）。

MinIO 的 `MINIO_DEFAULT_BUCKETS` 在这版二进制里没生效，而 `mc` 客户端下载被墙，
所以用标准 SigV4 直接发一个「建桶」请求：`PUT /<bucket>` + `x-amz-content-sha256`。

用法：python3 mk_bucket.py <endpoint_host:port> <bucket> <ak> <sk> [region]
"""
import hashlib
import hmac
import http.client
import sys
from datetime import datetime, timezone

host, bucket, ak, sk = sys.argv[1:5]
region = sys.argv[5] if len(sys.argv) > 5 else "us-east-1"

now = datetime.now(timezone.utc)
amz_date = now.strftime("%Y%m%dT%H%M%SZ")
date_stamp = now.strftime("%Y%m%d")
payload_hash = hashlib.sha256(b"").hexdigest()

canonical_uri = f"/{bucket}"
canonical_request = (
    f"PUT\n{canonical_uri}\n\n"
    f"host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n\n"
    f"host;x-amz-content-sha256;x-amz-date\n{payload_hash}"
)
scope = f"{date_stamp}/{region}/s3/aws4_request"
string_to_sign = (
    f"AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n"
    f"{hashlib.sha256(canonical_request.encode()).hexdigest()}"
)


def sign(key: bytes, msg: str) -> bytes:
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


k_date = sign(("AWS4" + sk).encode(), date_stamp)
k_region = sign(k_date, region)
k_service = sign(k_region, "s3")
k_signing = sign(k_service, "aws4_request")
signature = hmac.new(k_signing, string_to_sign.encode(), hashlib.sha256).hexdigest()

authorization = (
    f"AWS4-HMAC-SHA256 Credential={ak}/{scope}, "
    f"SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}"
)

connection = http.client.HTTPConnection(host, timeout=10)
connection.request(
    "PUT",
    canonical_uri,
    body=b"",
    headers={
        "Host": host,
        "x-amz-content-sha256": payload_hash,
        "x-amz-date": amz_date,
        "Authorization": authorization,
        "Content-Length": "0",
    },
)
response = connection.getresponse()
body = response.read()
print(f"PUT http://{host}{canonical_uri} -> {response.status} {response.reason}")
if body:
    print(body.decode("utf-8", "replace")[:400])
connection.close()
sys.exit(0 if response.status in (200, 409) else 1)
