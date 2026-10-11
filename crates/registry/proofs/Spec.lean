import O5sRegistry
import Schema
import H5iAppLib
/-!
# What the registry stores

The registry's state as plain lists, and what each write does to it. Every
write is a keyed put (`H5iAppLib.upsert`) or replaces the repository row;
nothing is ever deleted.
-/
open Aeneas Aeneas.Std o5s_registry H5iAppLib

namespace o5s_registry.Spec

/-- One tenant's rows: a repository or an organization directory. -/
structure St where
  repo : Repository
  patches : List PatchRow
  deps : List Dependency
  approvals : List Approval
  checks : List CheckReport
  channels : List Channel
  members : List Member
  paths : List PathKind
  events : List Event
  bodies : List Body
  listings : List Listing

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.repository, s.patches.val, s.dependencies.val, s.approvals.val, s.checks.val, s.channels.val,
    s.members.val, s.paths.val, s.events.val, s.bodies.val, s.listings.val⟩

/-- What one write does. Keys are those of the tables (`schema!`). -/
def applyWrite (s : St) : Write → St
  | .SetRepository r => { s with repo := r }
  | .PutPatch p => { s with patches := upsert (fun y => y.id) p s.patches }
  | .PutDependency d => { s with deps := upsert (fun y => (y.patch, y.dependency)) d s.deps }
  | .PutApproval a => { s with approvals := upsert (fun y => (y.patch, y.reviewer)) a s.approvals }
  | .PutCheck c => { s with checks := upsert (fun y => y.patch) c s.checks }
  | .PutChannel c => { s with channels := upsert (fun y => y.id) c s.channels }
  | .PutMember m => { s with members := upsert (fun y => (y.channel, y.position)) m s.members }
  | .PutPath k => { s with paths := upsert (fun y => y.id) k s.paths }
  | .PutEvent e => { s with events := upsert (fun y => y.seq) e s.events }
  | .PutBody b => { s with bodies := upsert (fun y => y.patch) b s.bodies }
  | .PutListing l => { s with listings := upsert (fun y => y.repo) l s.listings }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-- A fresh tenant. -/
def init : St := ⟨Schema.Repository.zero, [], [], [], [], [], [], [], [], [], []⟩

end o5s_registry.Spec
