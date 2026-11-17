| front | metric | corpus | o5s | specialist | ratio | tier | load (max of both) |
|---|---|---|---|---|---|---|---|
| git | clone_v0_s | jq | 0.519 s (o5s, n=3) | 0.579 s (git-daemon) | 0.90x | STRONG PASS | 1.06 |
| git | clone_v0_s | redis | 7.951 s (o5s, n=3) | 10.855 s (git-daemon) | 0.73x | STRONG PASS | 2.14 |
| git | clone_v0_s | ripgrep | 0.443 s (o5s, n=3) | 0.487 s (git-daemon) | 0.91x | STRONG PASS | 1.00 |
| git | clone_v2_s | jq | 0.564 s (o5s, n=3) | 0.580 s (git-daemon) | 0.97x | STRONG PASS | 1.07 |
| git | clone_v2_s | redis | 7.937 s (o5s, n=3) | 10.742 s (git-daemon) | 0.74x | STRONG PASS | 3.30 |
| git | clone_v2_s | ripgrep | 0.488 s (o5s, n=3) | 0.585 s (git-daemon) | 0.83x | STRONG PASS | 1.00 |
| git | clone_wire_bytes | jq | 9.2 MB (o5s, n=1) | 8.4 MB (git-daemon) | 1.10x | PASS | 1.06 |
| git | clone_wire_bytes | redis | 131.5 MB (o5s, n=1) | 226.9 MB (git-daemon) | 0.58x | STRONG PASS | 1.83 |
| git | clone_wire_bytes | ripgrep | 6.4 MB (o5s, n=1) | 6.1 MB (git-daemon) | 1.06x | PASS | 1.08 |
| git | fetch10_s | jq | 0.115 s (o5s, n=3) | 0.154 s (git-daemon) | 0.75x | STRONG PASS | 1.06 |
| git | fetch10_s | redis | 0.153 s (o5s, n=3) | 0.216 s (git-daemon) | 0.71x | STRONG PASS | 1.73 |
| git | fetch10_s | ripgrep | 0.151 s (o5s, n=3) | 0.182 s (git-daemon) | 0.83x | STRONG PASS | 1.08 |
| git | push_s | jq | 0.480 s (o5s, n=3) | 0.547 s (git-daemon) | 0.88x | STRONG PASS | 1.07 |
| git | push_s | redis | 8.629 s (o5s, n=3) | 10.731 s (git-daemon) | 0.80x | STRONG PASS | 2.67 |
| git | push_s | ripgrep | 0.554 s (o5s, n=3) | 0.542 s (git-daemon) | 1.02x | PASS | 1.00 |
| oci | pull_s |  | 0.328 s (o5s, n=3) | 0.189 s (distribution) | 1.74x | FAIL | 3.22 |
| oci | push_s |  | 0.106 s (o5s, n=3) | 0.175 s (distribution) | 0.60x | STRONG PASS | 3.22 |
| oci | stored_bytes |  | 76.2 MB (o5s, n=1) | 76.0 MB (distribution) | 1.00x | PASS | 3.22 |
| hf | download_server_cpu_s |  | 0.850 s (o5s-lfs, n=3) | 0.840 s (v1-reference) | 1.01x | PASS | 3.77 |
| hf | download_server_cpu_s |  | 0.830 s (o5s-xet, n=3) | 0.840 s (v1-reference) | 0.99x | STRONG PASS | 3.89 |
| hf | family_stored_bytes |  | 424.8 MB (o5s-xet, n=1) | 465.9 MB (xet-dedup) | 0.91x | STRONG PASS | 3.50 |
| hf | upload_server_s |  | 0.331 s (o5s-lfs, n=3) | 0.246 s (v1-reference) | 1.35x | FAIL | 3.77 |
| hf | upload_server_s |  | 0.091 s (o5s-xet, n=3) | 0.246 s (v1-reference) | 0.37x | STRONG PASS | 3.89 |
| pypi | cold_p50_s |  | 0.419 s (o5s, n=3) | 0.379 s (devpi) | 1.10x | PASS | 2.51 |
| pypi | hot_p50_s |  | 0.400 s (o5s, n=3) | 0.340 s (devpi) | 1.18x | PASS | 2.18 |
| pypi | stored_bytes |  | 6.8 MB (o5s, n=3) | 14.5 MB (devpi) | 0.47x | STRONG PASS | 2.18 |
| npm | cold_p50_s |  | 0.599 s (o5s, n=3) | 0.568 s (verdaccio) | 1.06x | PASS | 1.51 |
| npm | hot_p50_s |  | 0.570 s (o5s, n=3) | 0.538 s (verdaccio) | 1.06x | PASS | 1.43 |
| npm | stored_bytes |  | 1.7 MB (o5s, n=3) | 14.7 MB (verdaccio) | 0.11x | STRONG PASS | 1.43 |
| maven | cold_p50_s |  | 2.517 s (o5s, n=3) | 2.549 s (reposilite) | 0.99x | STRONG PASS | 3.19 |
| maven | hot_p50_s |  | 2.464 s (o5s, n=3) | 2.386 s (reposilite) | 1.03x | PASS | 3.53 |
| maven | stored_bytes |  | 45.6 MB (o5s, n=3) | 48.9 MB (reposilite) | 0.93x | STRONG PASS | 3.53 |

| front | functional checks | verdict (worst tier) |
|---|---|---|
| git | 90/90 passed | PASS |
| oci | 24/24 passed | FAIL |
| hf | 6/6 passed | FAIL |
| pypi | 6/6 passed | PASS |
| npm | 6/6 passed | PASS |
| maven | 3/3 passed | PASS |

(untiered) git first_push_s jq: 0.730 s vs 0.547 s (git-daemon) = 1.33x

(untiered) git first_push_s redis: 12.027 s vs 10.731 s (git-daemon) = 1.12x

(untiered) git first_push_s ripgrep: 0.742 s vs 0.542 s (git-daemon) = 1.37x

(untiered) hf upload_s : 0.529 s vs 0.255 s (v1-reference) = 2.07x

(untiered) hf upload_s : 1.340 s vs 0.255 s (v1-reference) = 5.25x

(untiered) hf download_s : 0.695 s vs 0.293 s (v1-reference) = 2.37x

(untiered) hf download_s : 0.434 s vs 0.293 s (v1-reference) = 1.48x

(untiered) hf download_server_s : 0.673 s vs 0.284 s (v1-reference) = 2.37x

(untiered) hf download_server_s : 0.310 s vs 0.284 s (v1-reference) = 1.09x

(untiered) hf upload_server_cpu_s : 0.910 s vs 0.680 s (v1-reference) = 1.34x

(untiered) hf upload_server_cpu_s : 0.090 s vs 0.680 s (v1-reference) = 0.13x

Other measurements (medians, not tiered):

- git first_push_s jq git-daemon: 0.547 s
- git first_push_s jq o5s: 0.730 s
- git first_push_s redis git-daemon: 10.731 s
- git first_push_s redis o5s: 12.027 s
- git first_push_s ripgrep git-daemon: 0.542 s
- git first_push_s ripgrep o5s: 0.742 s
- hf download_client_cpu_s  o5s-lfs: 0.687 s
- hf download_client_cpu_s  o5s-xet: 0.452 s
- hf download_s  o5s-lfs: 0.695 s
- hf download_s  o5s-xet: 0.434 s
- hf download_s  v1-reference: 0.293 s
- hf download_server_s  o5s-lfs: 0.673 s
- hf download_server_s  o5s-xet: 0.310 s
- hf download_server_s  v1-reference: 0.284 s
- hf family_added_bytes  o5s-xet: 212.5 MB
- hf family_raw_bytes  source: 538.1 MB
- hf upload_client_cpu_s  o5s-lfs: 0.334 s
- hf upload_client_cpu_s  o5s-xet: 1.628 s
- hf upload_s  o5s-lfs: 0.529 s
- hf upload_s  o5s-xet: 1.340 s
- hf upload_s  v1-reference: 0.255 s
- hf upload_server_cpu_s  o5s-lfs: 0.910 s
- hf upload_server_cpu_s  o5s-xet: 0.090 s
- hf upload_server_cpu_s  v1-reference: 0.680 s
- maven cold_total_s  o5s: 126.964 s
- maven cold_total_s  reposilite: 128.840 s
- maven hot_total_s  o5s: 123.062 s
- maven hot_total_s  reposilite: 119.011 s
- npm cold_total_s  o5s: 30.282 s
- npm cold_total_s  verdaccio: 29.822 s
- npm hot_total_s  o5s: 28.524 s
- npm hot_total_s  verdaccio: 27.740 s
- oci image_set_bytes  source: 135.7 MB
- pypi cold_total_s  devpi: 21.994 s
- pypi cold_total_s  o5s: 20.830 s
- pypi hot_total_s  devpi: 17.225 s
- pypi hot_total_s  o5s: 20.232 s
