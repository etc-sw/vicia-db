# Vicia reference DB comparison (full)

Facts: 1000000; repetitions: 20

## Engine aggregate

| engine | role | build ms | point p50 ms | point p95 ms | point max ms | workload p50 ms | workload p95 ms | open baseline RSS MiB | workload delta RSS MiB | peak RSS MiB | retained RSS MiB | storage MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| vicia | bi-temporal Datalog product | 9024.76 | 0.011 | 0.019 | 0.019 | 320.751 | 341.448 | 11.922 | 1.375 | 13.297 | 1.375 | 338.34 | yes |
| grafeo | embedded graph query peer | 12671.491 | 748.354 | 789.255 | 863.349 | 773.544 | 810.695 | 665.703 | 343.32 | 1009.023 | 2.125 | 69.116 | yes |
| turso | embedded SQL peer | 9886.347 | 0.006 | 0.008 | 0.013 | 114.213 | 148.463 | 18.297 | 10.125 | 28.422 | 10.125 | 11.563 | yes |
| cozo | embedded Datalog peer | 9733.337 | 0.086 | 0.13 | 0.145 | 504.927 | 699.449 | 13.875 | 4.75 | 18.625 | 4.75 | 78.195 | yes |

## Owned result scan storage floors

| engine | role | build ms | point p50 ms | point p95 ms | point max ms | workload p50 ms | workload p95 ms | open baseline RSS MiB | workload delta RSS MiB | peak RSS MiB | retained RSS MiB | storage MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| redb | B-tree KV storage floor | 1857.442 | 0 | 0 | 0.001 | 29.243 | 37.326 | 11.125 | 32.375 | 43.5 | 32.375 | 38.754 | yes |
| fjall | LSM KV storage floor | 409.399 | 0 | 0.001 | 0.001 | 229.743 | 307.726 | 108.5 | 13.75 | 122.25 | 13.75 | 50.029 | yes |

## Engine aggregate memory breakdown

| engine | baseline anonymous MiB | baseline file-backed MiB | retained anonymous delta MiB | retained file-backed delta MiB | retained [heap] delta MiB | retained DB mmap delta MiB |
| --- | --- | --- | --- | --- | --- | --- |
| vicia | 4.055 | 8.082 | 1.07 | 0.387 | 1.07 | 0 |
| grafeo | 656.582 | 9.25 | 0.023 | 2.188 | 0 | 0 |
| turso | 5.875 | 12.559 | 7.281 | 3.055 | 5.328 | 0 |
| cozo | 4.02 | 10.031 | 2.223 | 2.875 | 1.891 | 0 |

## Owned result scan memory breakdown

| engine | baseline anonymous MiB | baseline file-backed MiB | retained anonymous delta MiB | retained file-backed delta MiB | retained [heap] delta MiB | retained DB mmap delta MiB |
| --- | --- | --- | --- | --- | --- | --- |
| redb | 3.762 | 7.559 | 32.301 | 0.125 | 32.301 | 0 |
| fjall | 100.551 | 8.176 | 13.801 | 0 | 13.801 | 0 |

`engineAggregate` and `ownedResultScan` are separate contracts. redb and Fjall are storage floors, not graph/query-engine peers.
Memory columns come from the fresh aggregate/scan child, so build high-water memory does not contaminate them.
Kernel buffered page cache is not attributed to a process; DB mmap reports only resident mappings owned by the process.
Every row passed the same exact count and arithmetic-checksum validation.
