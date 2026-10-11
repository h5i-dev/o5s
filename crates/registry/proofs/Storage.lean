import Apply
/-!
# What the store holds

The server stores a write set by running the plan of `sql_writes`, and loads
rows through `decode`. `H5iAppLib.Store` proves, for any schema, that the
database then holds the encoding of the state `applyAll` computes. This file
gives the registry's encoding and table writes, and checks them against
`applyWrite`.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema H5iAppLib H5iAppLib.Sql
  H5iAppLib.Store

namespace o5s_registry.Storage

/-- A state's rows, table by table, numbered like `schema!`. -/
def enc (s : St) : Tables Val
  | 0 => [Repository.row s.repo]
  | 1 => s.patches.map PatchRow.row
  | 2 => s.deps.map Dependency.row
  | 3 => s.approvals.map Approval.row
  | 4 => s.checks.map CheckReport.row
  | 5 => s.channels.map Channel.row
  | 6 => s.members.map Member.row
  | 7 => s.paths.map PathKind.row
  | 8 => s.events.map Event.row
  | 9 => s.bodies.map Body.row
  | 10 => s.listings.map Listing.row
  | _ => []

/-- The table writes of one write. -/
def sqlA : Write → List (AWrite Val)
  | .SetRepository r => [Repository.putA r]
  | .PutPatch p => [PatchRow.putA p]
  | .PutDependency d => [Dependency.putA d]
  | .PutApproval a => [Approval.putA a]
  | .PutCheck c => [CheckReport.putA c]
  | .PutChannel c => [Channel.putA c]
  | .PutMember m => [Member.putA m]
  | .PutPath k => [PathKind.putA k]
  | .PutEvent e => [Event.putA e]
  | .PutBody b => [Body.putA b]
  | .PutListing l => [Listing.putA l]

set_option maxHeartbeats 4000000 in
def app : App St Write Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := applyWrite
  init := init
  IsRow := IsRow
  enc_step := by schema_step [enc, sqlA, applyWrite]
  sql_ok := by schema_ok [sqlA]
  init_ok := by schema_init [enc, init]
  init_rows := by schema_rows [enc, init]

theorem fits : Fits app Snapshot.toSt where
  kl := rfl
  rows := rfl
  enc s := by funext t; cases_table t <;> rfl
  init := by funext t; cases_table t <;> rfl
  nil s t h := by cases_table t <;> first | omega | rfl

theorem sql_fits : SqlFits app := by
  intro w out h
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton] at h ⊢ <;> step* <;> simp_all

/-- The store holds what `apply` computes: the rows of the state `applyAll`
gives for its commits, and a load decodes to that state, up to row order. -/
theorem stored {db : Db Val} {s : St} (h : Served app Snapshot.toSt db s) :
    app.Holds db (enc s) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.toSt snap) s ⦄ :=
  Schema.stored fits sql_fits h

end o5s_registry.Storage
