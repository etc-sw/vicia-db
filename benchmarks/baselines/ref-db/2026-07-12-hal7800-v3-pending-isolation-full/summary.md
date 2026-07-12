# Vicia reference DB comparison (full)

Facts: 1000000; repetitions: 20

| engine | role | boundary | build ms | point read ms | aggregate/scan p50 ms | p95 ms | open baseline RSS MiB | workload delta RSS MiB | peak RSS MiB | retained RSS MiB | storage MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| vicia | bi-temporal Datalog product | engineAggregate | 12940.413 | 0.239 | 330.322 | 357.688 | 12.164 | 1.375 | 13.539 | 1.375 | 338.34 | yes |
| grafeo | embedded graph query peer | engineAggregate | 11467.39 | 824.828 | 922.943 | 990.494 | 666.273 | 343.449 | 1009.723 | 2.25 | 69.116 | yes |
| redb | B-tree KV storage floor | ownedResultScan | 1929.233 | 0.005 | 47.493 | 53.97 | 11.125 | 32.625 | 43.75 | 32.625 | 38.754 | yes |
| fjall | LSM KV storage floor | ownedResultScan | 424.868 | 0.008 | 217.323 | 227.756 | 108.25 | 13.75 | 122 | 13.75 | 50.029 | yes |
| turso | embedded SQL peer | engineAggregate | 7218.795 | 0.171 | 84.362 | 93.836 | 18.543 | 10 | 28.543 | 10 | 11.563 | yes |
| cozo | embedded Datalog peer | engineAggregate | 7642.777 | 0.19 | 382.586 | 409.054 | 13.875 | 4.875 | 18.75 | 4.875 | 78.195 | yes |

## Memory breakdown

| engine | baseline anonymous MiB | baseline file-backed MiB | retained anonymous delta MiB | retained file-backed delta MiB | retained [heap] delta MiB | retained DB mmap delta MiB |
| --- | --- | --- | --- | --- | --- | --- |
| vicia | 4.047 | 8.32 | 1.074 | 0.313 | 1.07 | 0 |
| grafeo | 657.152 | 9.387 | 0.023 | 2.25 | 0 | 0 |
| redb | 3.754 | 7.699 | 32.301 | 0.25 | 32.301 | 0 |
| fjall | 100.551 | 8.16 | 13.801 | 0 | 13.801 | 0 |
| turso | 5.871 | 12.785 | 7.281 | 2.926 | 5.328 | 0 |
| cozo | 4.016 | 10.082 | 2.223 | 2.813 | 1.891 | 0 |

`engineAggregate` and `ownedResultScan` are separate contracts. redb and Fjall are storage floors, not graph/query-engine peers.
Memory columns come from the fresh aggregate/scan child, so build high-water memory does not contaminate them.
Kernel buffered page cache is not attributed to a process; DB mmap reports only resident mappings owned by the process.
Every row passed the same exact count and arithmetic-checksum validation.
