# Vicia reference DB comparison (full)

Facts: 1000000; repetitions: 20

| engine | role | boundary | build ms | point read ms | aggregate/scan p50 ms | p95 ms | open baseline RSS MiB | workload delta RSS MiB | peak RSS MiB | retained RSS MiB | storage MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| vicia | bi-temporal Datalog product | engineAggregate | 12779.002 | 0.241 | 1631.171 | 1698.406 | 11.789 | 380.816 | 392.605 | 78.883 | 338.34 | yes |
| grafeo | embedded graph query peer | engineAggregate | 12903.755 | 728.204 | 888.019 | 1046.272 | 666.398 | 343.469 | 1009.867 | 2 | 69.116 | yes |
| redb | B-tree KV storage floor | ownedResultScan | 2106.518 | 0.004 | 49.396 | 57.487 | 11.125 | 32.5 | 43.625 | 32.5 | 38.754 | yes |
| fjall | LSM KV storage floor | ownedResultScan | 445.823 | 0.042 | 217.714 | 228.924 | 108.375 | 13.625 | 122 | 13.625 | 50.029 | yes |
| turso | embedded SQL peer | engineAggregate | 7266.045 | 0.133 | 83.638 | 95.155 | 18.543 | 10.5 | 29.043 | 10.5 | 11.563 | yes |
| cozo | embedded Datalog peer | engineAggregate | 8247.59 | 0.185 | 392.737 | 415.739 | 14 | 4.75 | 18.75 | 4.75 | 78.195 | yes |

## Memory breakdown

| engine | baseline anonymous MiB | baseline file-backed MiB | retained anonymous delta MiB | retained file-backed delta MiB | retained [heap] delta MiB | retained DB mmap delta MiB |
| --- | --- | --- | --- | --- | --- | --- |
| vicia | 4.047 | 7.914 | 78.59 | 0.641 | 78.586 | 0 |
| grafeo | 657.152 | 9.402 | 0.023 | 2.063 | 0 | 0 |
| redb | 3.754 | 7.695 | 32.301 | 0.063 | 32.301 | 0 |
| fjall | 100.551 | 8.09 | 13.801 | 0 | 13.801 | 0 |
| turso | 5.875 | 12.828 | 7.32 | 3.195 | 5.328 | 0 |
| cozo | 4.012 | 10.164 | 2.223 | 2.875 | 1.891 | 0 |

`engineAggregate` and `ownedResultScan` are separate contracts. redb and Fjall are storage floors, not graph/query-engine peers.
Memory columns come from the fresh aggregate/scan child, so build high-water memory does not contaminate them.
Kernel buffered page cache is not attributed to a process; DB mmap reports only resident mappings owned by the process.
Every row passed the same exact count and arithmetic-checksum validation.
