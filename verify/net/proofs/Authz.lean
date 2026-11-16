import AuthzSpecs
/-!
# Request authorization, proven by the framework

`guarded_transition` (`crates/net-kernel/src/lib.rs`, from `authz_kernel!`) is the access
kernel behind `h5i_app_std::authz::guard_viewed`. `h5i_authz` reads the policy's instances off
its extracted body and instantiates `h5i_app_std.Specs.authz`, which is proven once for every
kernel and policy:

* `guarded_transition.authorized`: every committed command makes only writes `permits` allows
  (a ref move needs write on the name), and discloses only rows that are stored exactly as
  disclosed and `visible` (an object only if some cell reaching it is readable; a ref only if
  readable);
* noninterference: a read-only command's result (`ReadObject`, `ReadRef`), refusals included,
  is the same in any two states with the same view, so nothing about objects or refs the
  caller may not read reaches it. In particular an object the caller may not read and an
  object that does not exist give the same refusal;
* `guarded_transition.read_only_sound`.

Not covered here: that the shell builds the snapshot truthfully (the refs it names and each
object's cells, from core's reachability index; the index's invariant is core's obligation),
and that the store commits the writes the core's name-layer kernel accepts. pico2peta's store
is not PostgreSQL, so h5i's `Storage` obligation does not apply; it is a trusted assumption of
this deployment (docs/NET_PROGRESS.md, "h5i rebuild").
-/
open Aeneas Aeneas.Std Result H5iAppLib pico2peta_net_kernel

h5i_derive_all

h5i_authz pico2peta_net_kernel.guarded_transition
