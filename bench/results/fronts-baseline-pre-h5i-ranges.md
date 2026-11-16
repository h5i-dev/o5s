# §6.5 front batch, baseline pre-h5i: per-metric run-to-run range

Source rows: `bench/results/fronts-baseline-pre-h5i.jsonl` (2026-11-16, net @ c8f9942 + bench first_push_s). Range = (max−min)/median.

| front | metric | corpus | system | n | median | min | max | range | load max |
|---|---|---|---|---|---|---|---|---|---|
| git | clone_v0_s | jq | git-daemon | 3 | 0.578 s | 0.573 s | 0.579 s | 0.9% | 4.70 |
| git | clone_v0_s | jq | pico2peta | 3 | 0.536 s | 0.533 s | 0.566 s | 6.2% | 4.13 |
| git | clone_v0_s | redis | git-daemon | 3 | 10.760 s | 10.656 s | 11.031 s | 3.5% | 2.24 |
| git | clone_v0_s | redis | pico2peta | 3 | 7.974 s | 7.944 s | 7.987 s | 0.5% | 2.62 |
| git | clone_v0_s | ripgrep | git-daemon | 3 | 0.484 s | 0.483 s | 0.489 s | 1.3% | 5.96 |
| git | clone_v0_s | ripgrep | pico2peta | 3 | 0.460 s | 0.458 s | 0.462 s | 0.8% | 5.36 |
| git | clone_v2_s | jq | git-daemon | 3 | 0.579 s | 0.571 s | 0.590 s | 3.4% | 4.85 |
| git | clone_v2_s | jq | pico2peta | 3 | 0.583 s | 0.581 s | 0.825 s | 41.9% | 4.40 |
| git | clone_v2_s | redis | git-daemon | 3 | 10.751 s | 10.725 s | 10.977 s | 2.3% | 2.64 |
| git | clone_v2_s | redis | pico2peta | 3 | 7.933 s | 7.907 s | 22.234 s | 180.6% | 3.90 |
| git | clone_v2_s | ripgrep | git-daemon | 3 | 0.578 s | 0.574 s | 0.594 s | 3.5% | 6.22 |
| git | clone_v2_s | ripgrep | pico2peta | 3 | 0.461 s | 0.458 s | 0.623 s | 35.7% | 5.56 |
| git | clone_wire_bytes | jq | git-daemon | 1 | 8.36 MB | 8.36 MB | 8.36 MB | 0.0% | 4.40 |
| git | clone_wire_bytes | jq | pico2peta | 1 | 9.59 MB | 9.59 MB | 9.59 MB | 0.0% | 3.96 |
| git | clone_wire_bytes | redis | git-daemon | 1 | 226.93 MB | 226.93 MB | 226.93 MB | 0.0% | 1.77 |
| git | clone_wire_bytes | redis | pico2peta | 1 | 131.55 MB | 131.55 MB | 131.55 MB | 0.0% | 1.95 |
| git | clone_wire_bytes | ripgrep | git-daemon | 1 | 6.07 MB | 6.07 MB | 6.07 MB | 0.0% | 5.56 |
| git | clone_wire_bytes | ripgrep | pico2peta | 1 | 6.83 MB | 6.83 MB | 6.83 MB | 0.0% | 5.01 |
| git | fetch10_s | jq | git-daemon | 3 | 0.148 s | 0.139 s | 0.157 s | 12.0% | 4.70 |
| git | fetch10_s | jq | pico2peta | 3 | 0.110 s | 0.105 s | 0.113 s | 7.0% | 3.96 |
| git | fetch10_s | redis | git-daemon | 3 | 0.219 s | 0.215 s | 0.227 s | 5.5% | 1.72 |
| git | fetch10_s | redis | pico2peta | 3 | 0.199 s | 0.198 s | 0.205 s | 3.5% | 1.86 |
| git | fetch10_s | ripgrep | git-daemon | 3 | 0.180 s | 0.179 s | 0.187 s | 4.6% | 5.56 |
| git | fetch10_s | ripgrep | pico2peta | 3 | 0.140 s | 0.123 s | 0.140 s | 12.7% | 5.01 |
| git | first_push_s | jq | git-daemon | 3 | 0.547 s | 0.545 s | 0.553 s | 1.4% | 4.85 |
| git | first_push_s | jq | pico2peta | 3 | 0.723 s | 0.720 s | 0.732 s | 1.7% | 3.96 |
| git | first_push_s | redis | git-daemon | 3 | 10.813 s | 10.607 s | 11.021 s | 3.8% | 3.05 |
| git | first_push_s | redis | pico2peta | 3 | 13.177 s | 13.119 s | 13.606 s | 3.7% | 4.35 |
| git | first_push_s | ripgrep | git-daemon | 3 | 0.536 s | 0.534 s | 0.540 s | 1.1% | 6.22 |
| git | first_push_s | ripgrep | pico2peta | 3 | 0.739 s | 0.733 s | 0.760 s | 3.5% | 5.01 |
| git | push_s | jq | git-daemon | 3 | 0.547 s | 0.545 s | 0.553 s | 1.4% | 4.85 |
| git | push_s | jq | pico2peta | 3 | 0.476 s | 0.432 s | 0.734 s | 63.6% | 4.40 |
| git | push_s | redis | git-daemon | 3 | 10.813 s | 10.607 s | 11.021 s | 3.8% | 3.05 |
| git | push_s | redis | pico2peta | 3 | 9.040 s | 6.996 s | 13.956 s | 77.0% | 3.12 |
| git | push_s | ripgrep | git-daemon | 3 | 0.536 s | 0.534 s | 0.540 s | 1.1% | 6.22 |
| git | push_s | ripgrep | pico2peta | 3 | 0.563 s | 0.540 s | 0.752 s | 37.8% | 5.56 |
| hf | download_s |  | pico2peta-lfs | 3 | 0.689 s | 0.646 s | 0.690 s | 6.4% | 3.73 |
| hf | download_s |  | pico2peta-xet | 3 | 0.457 s | 0.441 s | 0.481 s | 8.8% | 3.97 |
| hf | download_s |  | v1-reference | 3 | 0.294 s | 0.292 s | 0.294 s | 0.8% | 2.63 |
| hf | family_added_bytes |  | pico2peta-xet | 1 | 212.51 MB | 212.51 MB | 212.51 MB | 0.0% | 3.73 |
| hf | family_raw_bytes |  | source | 1 | 538.12 MB | 538.12 MB | 538.12 MB | 0.0% | 3.50 |
| hf | family_stored_bytes |  | pico2peta-xet | 1 | 424.80 MB | 424.80 MB | 424.80 MB | 0.0% | 3.63 |
| hf | family_stored_bytes |  | xet-dedup | 1 | 465.94 MB | 465.94 MB | 465.94 MB | 0.0% | 3.50 |
| hf | upload_s |  | pico2peta-lfs | 3 | 0.533 s | 0.527 s | 1.093 s | 106.3% | 3.73 |
| hf | upload_s |  | pico2peta-xet | 3 | 1.967 s | 1.931 s | 2.314 s | 19.5% | 2.90 |
| hf | upload_s |  | v1-reference | 3 | 0.257 s | 0.255 s | 0.434 s | 70.0% | 2.63 |
| maven | cold_p50_s |  | pico2peta | 3 | 2.553 s | 2.517 s | 2.666 s | 5.8% | 3.53 |
| maven | cold_p50_s |  | reposilite | 3 | 2.582 s | 2.564 s | 2.655 s | 3.6% | 3.22 |
| maven | cold_total_s |  | pico2peta | 3 | 129.527 s | 127.090 s | 133.590 s | 5.0% | 3.53 |
| maven | cold_total_s |  | reposilite | 3 | 129.905 s | 129.121 s | 133.926 s | 3.7% | 3.22 |
| maven | hot_p50_s |  | pico2peta | 3 | 2.458 s | 2.451 s | 2.462 s | 0.4% | 3.31 |
| maven | hot_p50_s |  | reposilite | 3 | 2.423 s | 2.408 s | 2.454 s | 1.9% | 4.09 |
| maven | hot_total_s |  | pico2peta | 3 | 123.554 s | 123.415 s | 123.635 s | 0.2% | 3.31 |
| maven | hot_total_s |  | reposilite | 3 | 122.788 s | 120.855 s | 125.842 s | 4.1% | 4.09 |
| maven | stored_bytes |  | pico2peta | 3 | 45.59 MB | 45.59 MB | 45.59 MB | 0.0% | 3.31 |
| maven | stored_bytes |  | reposilite | 3 | 48.87 MB | 48.87 MB | 48.87 MB | 0.0% | 4.09 |
| npm | cold_p50_s |  | pico2peta | 3 | 0.599 s | 0.591 s | 0.599 s | 1.3% | 1.18 |
| npm | cold_p50_s |  | verdaccio | 3 | 0.572 s | 0.568 s | 0.575 s | 1.2% | 1.17 |
| npm | cold_total_s |  | pico2peta | 3 | 30.181 s | 30.039 s | 30.346 s | 1.0% | 1.18 |
| npm | cold_total_s |  | verdaccio | 3 | 29.882 s | 29.823 s | 30.140 s | 1.1% | 1.17 |
| npm | hot_p50_s |  | pico2peta | 3 | 0.569 s | 0.566 s | 0.570 s | 0.7% | 1.24 |
| npm | hot_p50_s |  | verdaccio | 3 | 0.534 s | 0.534 s | 0.536 s | 0.4% | 1.18 |
| npm | hot_total_s |  | pico2peta | 3 | 28.460 s | 28.181 s | 28.549 s | 1.3% | 1.24 |
| npm | hot_total_s |  | verdaccio | 3 | 27.623 s | 27.560 s | 27.690 s | 0.5% | 1.18 |
| npm | stored_bytes |  | pico2peta | 3 | 1.66 MB | 1.66 MB | 1.66 MB | 0.0% | 1.24 |
| npm | stored_bytes |  | verdaccio | 3 | 14.71 MB | 14.71 MB | 14.71 MB | 0.0% | 1.18 |
| oci | image_set_bytes |  | source | 1 | 135.68 MB | 135.68 MB | 135.68 MB | 0.0% | 3.50 |
| oci | pull_s |  | distribution | 3 | 0.189 s | 0.178 s | 0.207 s | 15.0% | 3.50 |
| oci | pull_s |  | pico2peta | 3 | 0.377 s | 0.368 s | 0.378 s | 2.7% | 3.38 |
| oci | push_s |  | distribution | 3 | 0.174 s | 0.169 s | 0.177 s | 4.3% | 3.50 |
| oci | push_s |  | pico2peta | 3 | 0.107 s | 0.105 s | 0.224 s | 110.9% | 3.38 |
| oci | stored_bytes |  | distribution | 1 | 76.04 MB | 76.04 MB | 76.04 MB | 0.0% | 3.50 |
| oci | stored_bytes |  | pico2peta | 1 | 76.24 MB | 76.24 MB | 76.24 MB | 0.0% | 3.38 |
| pypi | cold_p50_s |  | devpi | 3 | 0.465 s | 0.448 s | 0.488 s | 8.5% | 2.92 |
| pypi | cold_p50_s |  | pico2peta | 3 | 0.472 s | 0.468 s | 0.499 s | 6.5% | 1.97 |
| pypi | cold_total_s |  | devpi | 3 | 26.517 s | 25.858 s | 27.959 s | 7.9% | 2.92 |
| pypi | cold_total_s |  | pico2peta | 3 | 23.629 s | 23.499 s | 24.910 s | 6.0% | 1.97 |
| pypi | hot_p50_s |  | devpi | 3 | 0.338 s | 0.338 s | 0.338 s | 0.1% | 2.37 |
| pypi | hot_p50_s |  | pico2peta | 3 | 0.403 s | 0.400 s | 0.403 s | 0.7% | 1.68 |
| pypi | hot_total_s |  | devpi | 3 | 17.189 s | 17.130 s | 17.239 s | 0.6% | 2.37 |
| pypi | hot_total_s |  | pico2peta | 3 | 20.236 s | 20.141 s | 20.332 s | 0.9% | 1.68 |
| pypi | stored_bytes |  | devpi | 3 | 14.51 MB | 14.51 MB | 14.58 MB | 0.5% | 2.37 |
| pypi | stored_bytes |  | pico2peta | 3 | 6.79 MB | 6.79 MB | 6.79 MB | 0.0% | 1.68 |
