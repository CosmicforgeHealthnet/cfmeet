<!-- Loopback REHEARSAL of scripts/staging against real processes (Platform Access, Messaging binary, MinIO) on a development host;
     services/messaging/tests/platform_access_e2e.rs::hosted_staging_kit_rehearsal_against_real_services. NOT hosted-staging evidence, NOT a capacity claim. -->

# Hosted staging capacity — loopback rehearsal

| Field | Value |
|---|---|
| Target | http://127.0.0.1:35047 (loopback rehearsal, not hosted staging) |
| Owner authorization | — (loopback) |
| Window | 2026-10-20T20:34:43.613Z → 2026-10-20T20:34:47.252Z |
| Runner | v22.22.0, linux 6.18.44-fc-v139, 4 CPUs |
| Parameters | members=6, messagesPerSender=8, fanoutMessages=8, fanoutIntervalMs=20, searches=6, attachments=3, attachmentBytes=65536, concurrency=16, deliveryTimeoutMs=30000, workloads=managed-send,history-read,fanout,reconnect-storm,search,attachments |
| Policy restored / devices revoked | true / 5 |
| Verdict | **MEASURED (no owner thresholds supplied)** |

## managed-send

| Measurement | Value |
|---|---|
| senders | 6 |
| attempted | 48 |
| accepted | 48 |
| statuses | `{"201":48}` |
| elapsed_s | 0.764 |
| accepted_per_s | 62.79 |
| send_latency | p50 90.21 / p95 126.73 / p99 137.3 / max 137.3 ms (n=48) |
| verification | `{"stored":48,"missing":0,"duplicated":0,"strictly_ordered":true}` |
| correct | true |

## history-read

| Measurement | Value |
|---|---|
| readers | 6 |
| pages | 6 |
| page_size | 500 |
| page_latency | p50 47.82 / p95 56.2 / p99 56.2 / max 56.2 ms (n=6) |
| correct | true |

## fanout

| Measurement | Value |
|---|---|
| receivers | 5 |
| messages | 8 |
| deliveries_expected | 40 |
| deliveries | 40 |
| connect_and_subscribe | p50 80.98 / p95 83.63 / p99 83.63 / max 83.63 ms (n=5) |
| send_to_receive_latency | p50 51.18 / p95 55.06 / p99 56.05 / max 56.05 ms (n=40) |
| verification | `{"missing":0,"duplicated":0,"out_of_order":0}` |
| correct | true |

## reconnect-storm

| Measurement | Value |
|---|---|
| clients | 5 |
| all_resubscribed_ms | 191 |
| per_client_reconnect | p50 77.95 / p95 79.69 / p99 79.69 / max 79.69 ms (n=5) |
| verification | `{"missing":0,"duplicated":0}` |
| correct | true |

## search

| Measurement | Value |
|---|---|
| queries | 6 |
| statuses | `{"200":6}` |
| latency | p50 37.99 / p95 46.12 / p99 46.12 / max 46.12 ms (n=6) |
| found | 6 |
| correct | true |

## attachments

| Measurement | Value |
|---|---|
| attachments | 3 |
| bytes_each | 65536 |
| byte_identical | 3 |
| failures | `{}` |
| intent | p50 58.45 / p95 70.68 / p99 70.68 / max 70.68 ms (n=3) |
| upload | p50 6.67 / p95 10.86 / p99 10.86 / max 10.86 ms (n=3) |
| finalize | p50 61.42 / p95 63.37 / p99 63.37 / max 63.37 ms (n=3) |
| finalize_to_ready | p50 0 / p95 0 / p99 0 / max 0 ms (n=3) |
| download | p50 2.39 / p95 5.17 / p99 5.17 / max 5.17 ms (n=3) |
| correct | true |

## Checks

| Workload | Check | Value | Rule | Outcome |
|---|---|---|---|---|
| managed-send | correctness | true |  | PASS |
| history-read | correctness | true |  | PASS |
| fanout | correctness | true |  | PASS |
| reconnect-storm | correctness | true |  | PASS |
| search | correctness | true |  | PASS |
| attachments | correctness | true |  | PASS |

These numbers describe this target and runner only; they are not a production capacity claim.
