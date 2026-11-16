| front | metric | corpus | pico2peta | specialist | ratio | tier | load (max of both) |
|---|---|---|---|---|---|---|---|
| git | clone_v0_s | jq | 0.536 s (pico2peta, n=3) | 0.578 s (git-daemon) | 0.93x | STRONG PASS | 4.70 |
| git | clone_v0_s | redis | 7.974 s (pico2peta, n=3) | 10.760 s (git-daemon) | 0.74x | STRONG PASS | 2.62 |
| git | clone_v0_s | ripgrep | 0.460 s (pico2peta, n=3) | 0.484 s (git-daemon) | 0.95x | STRONG PASS | 5.96 |
| git | clone_v2_s | jq | 0.583 s (pico2peta, n=3) | 0.579 s (git-daemon) | 1.01x | PASS | 4.85 |
| git | clone_v2_s | redis | 7.933 s (pico2peta, n=3) | 10.751 s (git-daemon) | 0.74x | STRONG PASS | 3.90 |
| git | clone_v2_s | ripgrep | 0.461 s (pico2peta, n=3) | 0.578 s (git-daemon) | 0.80x | STRONG PASS | 6.22 |
| git | clone_wire_bytes | jq | 9.6 MB (pico2peta, n=1) | 8.4 MB (git-daemon) | 1.15x | PASS | 4.40 |
| git | clone_wire_bytes | redis | 131.5 MB (pico2peta, n=1) | 226.9 MB (git-daemon) | 0.58x | STRONG PASS | 1.95 |
| git | clone_wire_bytes | ripgrep | 6.8 MB (pico2peta, n=1) | 6.1 MB (git-daemon) | 1.12x | PASS | 5.56 |
| git | fetch10_s | jq | 0.110 s (pico2peta, n=3) | 0.148 s (git-daemon) | 0.74x | STRONG PASS | 4.70 |
| git | fetch10_s | redis | 0.199 s (pico2peta, n=3) | 0.219 s (git-daemon) | 0.91x | STRONG PASS | 1.86 |
| git | fetch10_s | ripgrep | 0.140 s (pico2peta, n=3) | 0.180 s (git-daemon) | 0.78x | STRONG PASS | 5.56 |
| git | push_s | jq | 0.476 s (pico2peta, n=3) | 0.547 s (git-daemon) | 0.87x | STRONG PASS | 4.85 |
| git | push_s | redis | 9.040 s (pico2peta, n=3) | 10.813 s (git-daemon) | 0.84x | STRONG PASS | 3.12 |
| git | push_s | ripgrep | 0.563 s (pico2peta, n=3) | 0.536 s (git-daemon) | 1.05x | PASS | 6.22 |
| oci | pull_s |  | 0.377 s (pico2peta, n=3) | 0.189 s (distribution) | 1.99x | FAIL | 3.50 |
| oci | push_s |  | 0.107 s (pico2peta, n=3) | 0.174 s (distribution) | 0.62x | STRONG PASS | 3.50 |
| oci | stored_bytes |  | 76.2 MB (pico2peta, n=1) | 76.0 MB (distribution) | 1.00x | PASS | 3.50 |
| hf | download_s |  | 0.689 s (pico2peta-lfs, n=3) | 0.294 s (v1-reference) | 2.35x | FAIL | 3.73 |
| hf | download_s |  | 0.457 s (pico2peta-xet, n=3) | 0.294 s (v1-reference) | 1.56x | FAIL | 3.97 |
| hf | family_stored_bytes |  | 424.8 MB (pico2peta-xet, n=1) | 465.9 MB (xet-dedup) | 0.91x | STRONG PASS | 3.63 |
| hf | upload_s |  | 0.533 s (pico2peta-lfs, n=3) | 0.257 s (v1-reference) | 2.08x | FAIL | 3.73 |
| hf | upload_s |  | 1.967 s (pico2peta-xet, n=3) | 0.257 s (v1-reference) | 7.66x | FAIL | 2.90 |
| pypi | cold_p50_s |  | 0.472 s (pico2peta, n=3) | 0.465 s (devpi) | 1.02x | PASS | 2.92 |
| pypi | hot_p50_s |  | 0.403 s (pico2peta, n=3) | 0.338 s (devpi) | 1.19x | PASS | 2.37 |
| pypi | stored_bytes |  | 6.8 MB (pico2peta, n=3) | 14.5 MB (devpi) | 0.47x | STRONG PASS | 2.37 |
| npm | cold_p50_s |  | 0.599 s (pico2peta, n=3) | 0.572 s (verdaccio) | 1.05x | PASS | 1.18 |
| npm | hot_p50_s |  | 0.569 s (pico2peta, n=3) | 0.534 s (verdaccio) | 1.06x | PASS | 1.24 |
| npm | stored_bytes |  | 1.7 MB (pico2peta, n=3) | 14.7 MB (verdaccio) | 0.11x | STRONG PASS | 1.24 |
| maven | cold_p50_s |  | 2.553 s (pico2peta, n=3) | 2.582 s (reposilite) | 0.99x | STRONG PASS | 3.53 |
| maven | hot_p50_s |  | 2.458 s (pico2peta, n=3) | 2.423 s (reposilite) | 1.01x | PASS | 4.09 |
| maven | stored_bytes |  | 45.6 MB (pico2peta, n=3) | 48.9 MB (reposilite) | 0.93x | STRONG PASS | 4.09 |

| front | functional checks | verdict (worst tier) |
|---|---|---|
| git | 90/90 passed | PASS |
| oci | 24/24 passed | FAIL |
| hf | 6/6 passed | FAIL |
| pypi | 6/6 passed | PASS |
| npm | 6/6 passed | PASS |
| maven | 3/3 passed | PASS |

(untiered) git first_push_s jq: 0.723 s vs 0.547 s (git-daemon) = 1.32x

(untiered) git first_push_s redis: 13.177 s vs 10.813 s (git-daemon) = 1.22x

(untiered) git first_push_s ripgrep: 0.739 s vs 0.536 s (git-daemon) = 1.38x

Other measurements (medians, not tiered):

- git first_push_s jq git-daemon: 0.547 s
- git first_push_s jq pico2peta: 0.723 s
- git first_push_s redis git-daemon: 10.813 s
- git first_push_s redis pico2peta: 13.177 s
- git first_push_s ripgrep git-daemon: 0.536 s
- git first_push_s ripgrep pico2peta: 0.739 s
- hf family_added_bytes  pico2peta-xet: 212.5 MB
- hf family_raw_bytes  source: 538.1 MB
- maven cold_total_s  pico2peta: 129.527 s
- maven cold_total_s  reposilite: 129.905 s
- maven hot_total_s  pico2peta: 123.554 s
- maven hot_total_s  reposilite: 122.788 s
- npm cold_total_s  pico2peta: 30.181 s
- npm cold_total_s  verdaccio: 29.882 s
- npm hot_total_s  pico2peta: 28.460 s
- npm hot_total_s  verdaccio: 27.623 s
- oci image_set_bytes  source: 135.7 MB
- pypi cold_total_s  devpi: 26.517 s
- pypi cold_total_s  pico2peta: 23.629 s
- pypi hot_total_s  devpi: 17.189 s
- pypi hot_total_s  pico2peta: 20.236 s
