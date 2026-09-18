# TFA candidate sourcing — 2026-09-17

This is a read-only snapshot of Meta TestInfra data for the Test Flakiness
Analyzer revival. It contains 30 distinct Buck targets grouped into ten service
families. No candidate has been run under Hermit yet.

## The current ranking signal

The signal is **PFS: Probabilistic Flakiness Score**, also described in source as
the probability of flaky state. The bulk-queryable source is
`infrastructure.dim_testinfra_tests_flakiness`; its `expectation` column is the
score on a 0–1 scale. Use the row with the greatest `ts_timestamp` for each
`test_id`. The table is refreshed hourly but is sparse: a missing row means that
no current PFS estimate is available, not a score of zero.

This is reachable from this host through the read-only Meta CLI:

```sh
meta presto.query execute \
  --namespace=infrastructure \
  --output=json \
  --reasoning='Rank tests for TFA candidate sourcing' \
  --sql="WITH pfs AS (
    SELECT test_id,
           MAX_BY(expectation, ts_timestamp) AS pfs,
           MAX_BY(brokenness_expectation, ts_timestamp) AS brokenness
    FROM dim_testinfra_tests_flakiness
    WHERE ds = '<LATEST_PFS_DS>'
    GROUP BY test_id
  )
  SELECT t.oncall, t.id, t.name, p.pfs, p.brokenness
  FROM pfs p
  JOIN dim_signalinfra_tests t
    ON t.id = p.test_id AND t.ds = '<LATEST_TESTS_DS>'
  ORDER BY p.pfs DESC
  LIMIT 300"
```

On this snapshot the latest partitions were PFS `ds=2026-09-17` and test
metadata `ds=2026-09-16`. Do not substitute either
`dim_signalinfra_tests_summary.flaky_score` or the `flakiness` field printed by
`meta testinfra.test metadata`: those are the older/simple flakiness measure.
For example, test `562950354325343` had PFS `49.44%`, while metadata reported
simple flakiness `82.30%`.

For one test, the supported metadata command returns the exact standalone and
stress commands:

```sh
meta testinfra.test metadata --id=<TEST_ID> --output=json --no-truncate
```

## Native-rate definition

`native failure rate` below is empirical, not PFS: failures divided by executed
results in up to the latest 100 final native TestInfra outcomes in the trailing
week. Included purposes are `CONTINUOUS`, `STRESS_RUN`, and
`STRESS_RUN_NEW_TEST`; PASS is `PASSED`, and failure is `FAILED`, `FATAL`, or
`TIMEOUT`. `OMITTED`, `SKIPPED`, and `INFRA_FAILURE` are excluded from both
numerator and denominator. Reproduce a row's measurement with:

```sh
meta testinfra.result history \
  --test=<TEST_ID> \
  --since=ONE_WEEK \
  --limit=100 \
  --purposes=CONTINUOUS,STRESS_RUN,STRESS_RUN_NEW_TEST \
  --final-only \
  --output=json \
  --no-truncate
```

The service key is the first path component after `fbcode//`. Candidates were
ranked by latest PFS within each service, deduplicated by Buck target, restricted
to non-disabled tests with PFS at least 5%, and then curated toward Linux-native
targets. Proxygen and Thrift deliberately use native C++ tests instead of their
higher-ranked service-harness/Java entries.

## Candidates

### cachelib

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `281475243051108` — `cachelib_cogwheel_test` (sanitizer free-list stress) | `fbcode//cachelib/facebook/cogwheel:cogwheel_cachelib_sanitizer_free_list_stress-test` | 31.04% | 35.00% (21/60) | `buck run fbcode//cachelib/facebook/cogwheel:cogwheel_cachelib_sanitizer_free_list_stress-launcher -- --build-fbpkg --run-disabled --tests cachelib_cogwheel_test` |
| `281475243043222` — `cachelib_cogwheel_test` (free-list stress) | `fbcode//cachelib/facebook/cogwheel:cogwheel_cachelib_free_list_stress-test` | 27.80% | 26.92% (7/26) | `buck run fbcode//cachelib/facebook/cogwheel:cogwheel_cachelib_free_list_stress-launcher -- --build-fbpkg --run-disabled --tests cachelib_cogwheel_test` |
| `562950242844554` — `EventTrackerTest.NvmCacheWithEventTracker` | `fbcode//cachelib/common/tests:event-tracker-test` | 22.99% | 3.45% (1/29) | `buck test @fbcode//mode/dev fbcode//cachelib/common/tests:event-tracker-test -- --exact 'fbcode//cachelib/common/tests:event-tracker-test - EventTrackerTest.NvmCacheWithEventTracker'` |

### folly

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `562950211052906` — `AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect` | `fbcode//folly/io/async/test:async_ssl_socket_test` | 36.87% | 38.54% (37/96) | `buck test @fbcode//mode/libcxx fbcode//folly/io/async/test:async_ssl_socket_test -- --exact 'fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect' --run-disabled` |
| `281475234167465` — `TimerFDTimeoutManagerTest.ReschedTest` | `fbcode//folly/io/async/test:timerfd_timeout_manager_test` | 30.54% | 30.11% (28/93) | `buck test @fbcode//mode/dev-tsan fbcode//folly/io/async/test:timerfd_timeout_manager_test -- --exact 'fbcode//folly/io/async/test:timerfd_timeout_manager_test - TimerFDTimeoutManagerTest.ReschedTest' --run-disabled` |
| `562950361061616` — `IoUringDynamicProvidedBufferRingTest.BufferCountCheck` | `fbcode//folly/io/async/test:io_uring_dynamic_provided_buffer_ring_test` | 30.29% | 22.00% (22/100) | `buck test @fbcode//mode/dev-tsan fbcode//folly/io/async/test:io_uring_dynamic_provided_buffer_ring_test -- --exact 'fbcode//folly/io/async/test:io_uring_dynamic_provided_buffer_ring_test - IoUringDynamicProvidedBufferRingTest.BufferCountCheck'` |

### logdevice

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `281475244092464` — `LogStoreHealthMonitorTest.DisabledRebuilding` | `fbcode//logdevice/admin/health_monitor:tests` | 48.63% | 67.71% (65/96) | `buck test @fbcode//mode/opt fbcode//logdevice/admin/health_monitor:tests -- --exact 'fbcode//logdevice/admin/health_monitor:tests - LogStoreHealthMonitorTest.DisabledRebuilding' --run-disabled` |
| `844425197050049` — `SslAuthenticationTest.VerifyServerAuthentication/WithoutSessionResumption` | `fbcode//logdevice/fb:tests` | 31.77% | 35.11% (33/94) | `buck test @fbcode//mode/dev fbcode//logdevice/fb:tests -- --exact 'fbcode//logdevice/fb:tests - SslAuthenticationTest/SslAuthenticationTest.VerifyServerAuthentication/WithoutSessionResumption' --run-disabled` |
| `281475271465112` — `LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries` | `fbcode//logdevice/logservice/server/log_store/test:log_store_test` | 27.71% | 9.38% (3/32) | `buck test @fbcode//mode/dev fbcode//logdevice/logservice/server/log_store/test:log_store_test -- --exact 'fbcode//logdevice/logservice/server/log_store/test:log_store_test - LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries'` |

### manifold

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `844425263427177` — `CryptoPluginIntegrationTest.EmptyPayloadAtPluginBoundary` | `fbcode//manifold/common/versioned_prod:crypto_plugin_integration_test` | 48.99% | 74.23% (72/97) | `buck test @fbcode//mode/dev fbcode//manifold/common/versioned_prod:crypto_plugin_integration_test -- --exact 'fbcode//manifold/common/versioned_prod:crypto_plugin_integration_test - CryptoPluginIntegrationTest.EmptyPayloadAtPluginBoundary'` |
| `562950216898741` — `ClientTestWithBucketTypes.TestPayloadSizeBasedTimeout/sr_unified_nosidecar` | `fbcode//manifold/client/impl/tests:manifold_client_test` | 48.31% | 46.43% (39/84) | `buck test @fbcode//mode/dev fbcode//manifold/client/impl/tests:manifold_client_test -- --exact 'fbcode//manifold/client/impl/tests:manifold_client_test - core/ClientTestWithBucketTypes.TestPayloadSizeBasedTimeout/sr_unified_nosidecar' --run-disabled` |
| `281475240048871` — `test_real_create_then_delete_dir` | `fbcode//manifold/clients/rust:manifold_client_tests` | 46.03% | 7.29% (7/96) | `buck test @fbcode//mode/dev fbcode//manifold/clients/rust:manifold_client_tests -- --exact 'fbcode//manifold/clients/rust:manifold_client_tests - test_real_create_then_delete_dir'` |

### mcrouter

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `562950220120091` — `McRefillWrapperBucketized/0.refillOnMiss` | `fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test` | 49.39% | 79.38% (77/97) | `buck test @fbcode//mode/dev-tsan fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test -- --exact 'fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test - McRefillWrapperBucketized/0.refillOnMiss'` |
| `281475243259972` — `ShadowThreadAffinityIntegrationTest.DisableJumpThreads` | `fbcode//mcrouter/routes/facebook/test/cpp_unit_tests:shadow_thread_affinity_integration_test` | 49.00% | 69.39% (68/98) | `buck test @fbcode//mode/opt-tsan fbcode//mcrouter/routes/facebook/test/cpp_unit_tests:shadow_thread_affinity_integration_test -- --exact 'fbcode//mcrouter/routes/facebook/test/cpp_unit_tests:shadow_thread_affinity_integration_test - ShadowThreadAffinityIntegrationTest.DisableJumpThreads' --run-disabled` |
| `281475243249473` — `MuxIOThreadPoolExecutorWithThreadAffinityToasterTest/0.BasicTest` | `fbcode//mcrouter/facebook/test/toaster_tests:mux_io_thread_pool_executor_toaster_test` | 48.57% | 56.82% (50/88) | `buck test @fbcode//mode/opt-tsan fbcode//mcrouter/facebook/test/toaster_tests:mux_io_thread_pool_executor_toaster_test -- --exact 'fbcode//mcrouter/facebook/test/toaster_tests:mux_io_thread_pool_executor_toaster_test - MuxIOThreadPoolExecutorWithThreadAffinityToasterTest/0.BasicTest'` |

### mysql

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `844425202171362` — `main.explain_for_connection_small_json---rocksdb_intrinsic_table` | `fbcode//mysql/server/mysql-test:mtr_parallel_main_1` | 49.42% | 82.29% (79/96) | `buck test @fbcode//mode/dbgo -c 'fbcode.arch=aarch64' fbcode//mysql/server/mysql-test:mtr_parallel_main_1 -- --exact 'fbcode//mysql/server/mysql-test:mtr_parallel_main_1 - main.explain_for_connection_small_json---rocksdb_intrinsic_table'` |
| `844425205465381` — `rocksdb_ha_consistent_snapshot_stress` | `fbcode//mysql/server/mysql-test:mtr_parallel_rocksdb_stress` | 49.39% | 86.32% (82/95) | `buck test @fbcode//mode/dbgo -c 'fbcode.arch=aarch64' fbcode//mysql/server/mysql-test:mtr_parallel_rocksdb_stress -- --exact 'fbcode//mysql/server/mysql-test:mtr_parallel_rocksdb_stress - rocksdb_stress.rocksdb_ha_consistent_snapshot_stress' --run-disabled` |
| `562950274089363` — `main.explain_for_connection_small_json---rocksdb_intrinsic_table` (no TP mutex) | `fbcode//mysql/server/mysql-test:mtr_parallel_main_no_tp_mutex_1` | 49.30% | 42.00% (21/50) | `buck test @fbcode//mode/dbgo -c 'fbcode.arch=aarch64' fbcode//mysql/server/mysql-test:mtr_parallel_main_no_tp_mutex_1 -- --exact 'fbcode//mysql/server/mysql-test:mtr_parallel_main_no_tp_mutex_1 - main.explain_for_connection_small_json---rocksdb_intrinsic_table'` |

### proxygen

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `562950215341224` — `PopulateEdgeConnectionsLoggerEvent/without_response` | `fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test` | 33.98% | 6.67% (2/30) | `buck test @fbcode//mode/dev-asan fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test -- --exact 'fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test - PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response'` |
| `281475238796418` — `TlsTerminatorTest.terminateTls/h1_innerTls_http_connect` | `fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test` | 23.44% | 23.96% (23/96) | `buck test @fbcode//mode/opt fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test -- --exact 'fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test - TlsTerminatorTest/TlsTerminatorTest.terminateTls/h1_innerTls_http_connect' --run-disabled` |
| `562950215319283` — `H2DownstreamSessionTest.ResetRateLimit/h2` | `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | 21.70% | 3.33% (1/30) | `buck test @fbcode//mode/dev-asan fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests -- --exact 'fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2'` |

### thrift

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `562950212033102` — `CoroutineTest.SumThrowsUserExPrimitive` | `fbcode//thrift/lib/cpp2/test:coroutine_test` | 23.10% | 0.00% (0/29) | `buck test @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:coroutine_test -- --exact 'fbcode//thrift/lib/cpp2/test:coroutine_test - CoroutineTest/CoroutineTest/0.SumThrowsUserExPrimitive'` |
| `281475235281183` — `CoroutineTest.SumThrows` (resource pools) | `fbcode//thrift/lib/cpp2/test:coroutine_test_resource_pools` | 20.19% | 0.00% (0/22) | `buck test @fbcode//mode/opt-tsan fbcode//thrift/lib/cpp2/test:coroutine_test_resource_pools -- --exact 'fbcode//thrift/lib/cpp2/test:coroutine_test_resource_pools - CoroutineTest/CoroutineTest/0.SumThrows'` |
| `281475235299286` — `RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest` | `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | 17.22% | 0.00% (0/29) | `buck test @fbcode//mode/dev-tsan fbcode//thrift/lib/cpp2/test:InstrumentationTest -- --exact 'fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest'` |

The zeroes here are measured, not missing: all selected Thrift C++ candidates
passed every executed final result in the one-week sample. Their nonzero PFS is
therefore likely carrying older/model evidence; they are breadth candidates,
not strong first-pilot choices.

### warm_storage

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `562950213120998` — `write_time_encoding_selection_test` | `fbcode//warm_storage/integration/ws_durability:write_time_encoding_selection_test` | 49.43% | 88.00% (88/100) | `buck test @fbcode//mode/opt fbcode//warm_storage/integration/ws_durability:write_time_encoding_selection_test` |
| `281475236296488` — `FileCacheTest.getFilesFromNVM` | `fbcode//warm_storage/file_service:file_service_tests` | 49.42% | 93.75% (90/96) | `buck test @fbcode//mode/dev fbcode//warm_storage/file_service:file_service_tests -- --exact 'fbcode//warm_storage/file_service:file_service_tests - FileCacheTest.getFilesFromNVM'` |
| `281475250915202` — `CreateReaderRetryTests.testGetBlockInfoRetryBypassesCacheOnBlockNotFound` | `fbcode//warm_storage/blockio/prod/vnext/tests/api:bc_vnext_create_reader_retry_tests` | 49.15% | 64.52% (60/93) | `buck test @fbcode//warm_storage/common/client/mode-branch-dev fbcode//warm_storage/blockio/prod/vnext/tests/api:bc_vnext_create_reader_retry_tests -- --exact 'fbcode//warm_storage/blockio/prod/vnext/tests/api:bc_vnext_create_reader_retry_tests - CreateReaderRetryTests.testGetBlockInfoRetryBypassesCacheOnBlockNotFound' --run-disabled` |

### zippydb

| Test ID and test | Target | PFS | Native failure rate | Standalone invocation |
|---|---|---:|---:|---|
| `844425193659677` — `VRTUsageTrackerTest.BumpWithIOTagTest` | `fbcode//zippydb/common/tests:vrt_usage_tracker_test` | 49.47% | 96.00% (72/75) | `buck test @fbcode//mode/opt fbcode//zippydb/common/tests:vrt_usage_tracker_test -- --exact 'fbcode//zippydb/common/tests:vrt_usage_tracker_test - VRTUsageTrackerTest.BumpWithIOTagTest' --run-disabled` |
| `562950354325343` — `DRocksDBTest.DestroySucceedsOnPopulatedBackend` | `fbcode//zippydb/drocksdb/tests:DRocksDBTest` | 49.44% | 92.78% (90/97) | `buck test @fbcode//mode/opt fbcode//zippydb/drocksdb/tests:DRocksDBTest -- --exact 'fbcode//zippydb/drocksdb/tests:DRocksDBTest - DRocksDBTest.DestroySucceedsOnPopulatedBackend'` |
| `281475240064369` — `AdminHandlerE2ETest.CrosssShardTxnReserveWithoutConditions/0` | `fbcode//zippydb/zgateway/server/tests:admin_handler_e2e_test` | 49.43% | 80.85% (38/47) | `buck test @fbcode//mode/dev fbcode//zippydb/zgateway/server/tests:admin_handler_e2e_test -- --exact 'fbcode//zippydb/zgateway/server/tests:admin_handler_e2e_test - UseCases/AdminHandlerE2ETest.CrosssShardTxnReserveWithoutConditions/0' --run-disabled` |

## Pilot recommendation and cautions

Start with the native GTest/Rust rows having both pass and fail outcomes and no
special architecture requirement. The best low-cost first probes are Cachelib
`event-tracker-test` (1/29), Proxygen `HTTPCoroSessionTests` (1/30), LogDevice
`log_store_test` (3/32), and Manifold's Rust client test (7/96). They are flaky
enough to establish the bracket without making a native pass vanishingly rare.

Do not start with MySQL: all three selected commands require AArch64 and the MTR
harness. Cachelib's Cogwheel rows also use launcher targets rather than ordinary
`buck test`. The very high native-rate Warm Storage, ZippyDB, Manifold, and
Mcrouter rows may be useful later, but first establish that they can produce a
native pass within the campaign's run budget. TSAN/ASAN candidates must retain
the exact mode in the recorded command.

This artifact establishes sourcing and candidate identity only. A standalone
command from TestInfra proves how Meta invokes the test; it does not prove that
the resulting executable and its dependencies are supported under Hermit.
