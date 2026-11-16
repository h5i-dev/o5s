# verify/net: proofs about o5s's net kernel

`crates/net-kernel` (front decisions and wire parsers) is extracted by Charon/Aeneas into
`proofs/generated/` and proven in `proofs/*.lean` (h5i-app; see `docs/H5I_PLAN.md`).

```sh
. /mnt/scratch/pico2peta/h5i-env-net.sh        # h5i, charon, aeneas, lean (tool homes under /mnt/scratch/ec2-user)
ln -sfn /mnt/scratch/ec2-user/lake/arith/packages proofs/.lake/packages   # prebuilt Lean packages
h5i app prove        # extract + lake build + gate (no sorry/axiom; standard axioms only)
h5i app extract --check   # CI: the committed Lean is what the kernel extracts to
```

| Function (`parse.rs`) | Theorem (`ParseSpec.lean`) | Says |
|---|---|---|
| `decode_hex(s, n, lower)` | `decode_hex_spec` | equals `hexDec` when `len s = 2n`, else `none`; total (no panic) |
| `pkt_len(b)` | `pkt_len_spec` | equals `pktLen`: exactly four hex digits, big-endian; total |
