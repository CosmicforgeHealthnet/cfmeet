#!/usr/bin/env python3
"""Creates an S3 bucket (idempotent) on a test S3-compatible server (MinIO in CI).

  s3-create-bucket.py <endpoint> <bucket> <access key> <secret key> [region]

Python standard library only (AWS Signature Version 4, path-style). Test tooling: never
point it at a production account.
"""
import datetime
import hashlib
import hmac
import sys
import urllib.error
import urllib.parse
import urllib.request


def sign(key: bytes, msg: str) -> bytes:
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


def main() -> int:
    if len(sys.argv) < 5:
        print(__doc__, file=sys.stderr)
        return 2
    endpoint, bucket, access, secret = sys.argv[1:5]
    region = sys.argv[5] if len(sys.argv) > 5 else "us-east-1"
    url = urllib.parse.urlsplit(endpoint)
    host = url.netloc
    now = datetime.datetime.now(datetime.timezone.utc)
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    day = now.strftime("%Y%m%d")
    payload = hashlib.sha256(b"").hexdigest()
    path = "/" + urllib.parse.quote(bucket)
    canonical = "\n".join(
        ["PUT", path, "", f"host:{host}", f"x-amz-content-sha256:{payload}", f"x-amz-date:{amz_date}", "",
         "host;x-amz-content-sha256;x-amz-date", payload]
    )
    scope = f"{day}/{region}/s3/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    k = sign(sign(sign(sign(("AWS4" + secret).encode(), day), region), "s3"), "aws4_request")
    signature = hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest()
    req = urllib.request.Request(f"{url.scheme}://{host}{path}", method="PUT", headers={
        "x-amz-date": amz_date,
        "x-amz-content-sha256": payload,
        "Authorization": f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}",
    })
    try:
        urllib.request.urlopen(req, timeout=10)
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        if e.code == 409 and ("BucketAlreadyOwnedByYou" in body or "BucketAlreadyExists" in body):
            print(f"bucket {bucket} exists")
            return 0
        print(f"create bucket failed: {e.code} {body}", file=sys.stderr)
        return 1
    print(f"bucket {bucket} created")
    return 0


if __name__ == "__main__":
    sys.exit(main())
