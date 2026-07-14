# Vicia reference DB comparison (full, v5)

Facts: 1000000; trials: 5; samples/trial: 20
Stability gate (trial median MAD <= 5%): pass

## Lifecycle and physical storage

| engine | build p50 ms | open p50 ms | first read p50 ms | storage MiB |
| --- | --- | --- | --- | --- |
| vicia | 5924.611434 | 0.696288 | 0.083986 | 262.609375 |
| grafeo | 10349.302192 | 2377.054442 | 531.177549 | 69.116327 |
| sqlite | 2988.483638 | 0.24853 | 0.040592 | 11.480469 |
| turso | 5889.372352 | 0.797434 | 0.201225 | 11.563423 |
| cozo | 6507.499935 | 0.557003 | 1.279859 | 78.195313 |
| redb | 1658.968082 | 0.509528 | 0.017223 | 38.753906 |
| fjall | 527.102922 | 984.362517 | 0.008321 | 50.028607 |

## Warm point workloads

| engine | hot p50/p95 ms | distributed p50/p95 ms | miss p50/p95 ms | trial MAD max % |
| --- | --- | --- | --- | --- |
| vicia | 0.004195/0.004586 | 0.009137/0.00985 | 0.006772/0.007423 | 1.591776 |
| grafeo | 531.312662/594.626376 | 523.821601/591.407483 | 0.320553/0.35135 | 2.527172 |
| sqlite | 0.00942/0.010382 | 0.009605/0.010486 | 0.009404/0.010293 | 2.114649 |
| turso | 0.006731/0.007382 | 0.007426/0.008641 | 0.006466/0.006956 | 3.157091 |
| cozo | 0.066259/0.074389 | 0.069899/0.079155 | 0.063698/0.070763 | 2.399758 |
| redb | 0.000331/0.000364 | 0.000405/0.00046 | 0.000326/0.000371 | 1.122726 |
| fjall | 0.000264/0.00028 | 0.000518/0.000548 | 0.000211/0.000226 | 1.502492 |

## Engine aggregate

| engine | role | workload p50 ms | p95 ms | max ms | trial MAD % | baseline RSS MiB | delta RSS MiB | retained RSS MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| vicia | bi-temporal Datalog product | 171.455389 | 177.240236 | 182.979217 | 0.486157 | 12.097656 | 1.625 | 1.625 | yes |
| grafeo | embedded graph query peer | 618.555322 | 654.114642 | 682.739678 | 2.283487 | 666.53125 | 343.46875 | 2.375 | yes |
| sqlite | embedded SQL reference | 29.529511 | 32.768821 | 34.598536 | 3.912792 | 11.875 | 2.25 | 2.25 | yes |
| turso | embedded SQL peer | 70.77016 | 77.212949 | 83.791477 | 3.617729 | 18.675781 | 10.125 | 10.125 | yes |
| cozo | embedded Datalog peer | 316.729832 | 405.28922 | 413.576777 | 2.693689 | 13.875 | 4.875 | 4.875 | yes |

## Owned result scan storage floors

| engine | role | workload p50 ms | p95 ms | max ms | trial MAD % | baseline RSS MiB | delta RSS MiB | retained RSS MiB | correct |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| redb | B-tree KV storage floor | 26.746946 | 29.194229 | 37.13598 | 1.529122 | 11.375 | 32.375 | 32.375 | yes |
| fjall | LSM KV storage floor | 185.382756 | 190.674086 | 193.040235 | 1.606472 | 110.625 | 13.625 | 13.625 | yes |

`engineAggregate` and `ownedResultScan` remain separate contracts.
Every trial builds and closes its database, then measures reopen and queries in a fresh process.
Point workloads use adaptive operation batches; first-read latency is reported separately from warmed latency.
Storage bytes are physical adapter output, not equivalent semantic capacity; Vicia retains native bi-temporal ledger identity.
Kernel page cache is neither dropped nor attributed to a process.
Every sample passed point or exact count/checksum validation.
