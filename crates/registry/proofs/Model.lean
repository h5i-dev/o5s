import Exact
/-!
# The registry as a pure function

`Model.step` computes the result of every command on a tenant's rows as plain
lists. `Refine.lean` proves the extracted `transition` returns exactly this
whenever it returns at all, so the theorems in `Theorems.lean` are about
`Model.step` only.
-/
open Aeneas Aeneas.Std o5s_registry o5s_registry.Spec o5s_registry.Exact

namespace o5s_registry.Model

/-- A command's result, with writes as a list. -/
abbrev Out := core.result.Result (List Write × Reply) Error

/-- `x + 1`; the commands only use it below `u64::MAX`. -/
def inc (x : U64) : U64 := ⟨x.bv + 1⟩

def MAX : U64 := core.num.U64.MAX

/-- The label `main`. -/
def mainLabel : alloc.vec.Vec U8 :=
  alloc.vec.Vec.from [109#u8, 97#u8, 105#u8, 110#u8] (by have := H5iAppLib.usize_max_ge; simp; omega)

def register (a : Principal) (ls : List Listing) (n : alloc.vec.Vec U8) (repo : U64) : Out :=
  if a.scope ≠ a.org then .Err .WrongScope
  else if a.org.val ≥ REPOSITORY_SCOPE.val then .Err .WrongScope
  else if repo.val < REPOSITORY_SCOPE.val then .Err .WrongScope
  else if !validName n.val then .Err .InvalidName
  else match ls.find? (fun l => decide (l.label.val = n.val)) with
    | some l => if l.repo = repo then .Ok ([], .Registered repo) else .Err .KeyCollision
    | none =>
      if ls.any (fun l => decide (l.repo = repo)) then .Err .KeyCollision
      else .Ok ([.PutListing ⟨repo, n⟩], .Registered repo)

def create (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) : Out :=
  if a.scope.val < REPOSITORY_SCOPE.val then .Err .WrongScope
  else if !validName n.val then .Err .InvalidName
  else if r.created then
    (if r.org = a.org ∧ r.label.val = n.val then .Err .RepositoryExists else .Err .KeyCollision)
  else .Ok ([.SetRepository ⟨a.org, n, true, 0#u64, 1#u64, 0#u64, 1#u64⟩, .PutChannel ⟨1#u64, mainLabel, 0#u64⟩,
    .PutEvent ⟨1#u64, a.user, CREATE, none, none⟩], .Created)

def findChannel (cs : List Channel) (n : alloc.vec.Vec U8) : Option Channel :=
  cs.find? (fun c => decide (c.label.val = n.val))

def fork (u : U64) (s : St) (src n : alloc.vec.Vec U8) : Out :=
  if !validName n.val then .Err .InvalidChannelName
  else match findChannel s.channels n with
  | some _ => .Err .ChannelExists
  | none => match findChannel s.channels src with
    | none => .Err .SourceChannelNotFound
    | some c =>
      if s.repo.channels = MAX then .Err .Exhausted
      else if s.repo.events = MAX then .Err .Exhausted
      else
        let id := inc s.repo.channels
        let i := inc s.repo.events
        .Ok ([.PutChannel { c with id, label := n }] ++
          (s.members.filter (fun m => decide (m.channel = c.id))).map (fun m => .PutMember { m with channel := id }) ++
          [.SetRepository { s.repo with channels := id, events := i }, .PutEvent ⟨i, u, FORK, none, some id⟩],
          .Forked id)

/-- `register_paths`: the paths a proposal adds, and the path counter after them. -/
def registerPaths (known : List PathKind) : List PathUse → List PathKind → U64 →
    core.result.Result (List PathKind × U64) Error
  | [], added, next => .Ok (added, next)
  | u :: us, added, next =>
    let existing : Option PathKind := match known.find? (fun (k : PathKind) => decide (k.path.val = u.path.val)) with
      | none => added.find? (fun (k : PathKind) => decide (k.path.val = u.path.val))
      | some k => some k
    match existing with
    | none =>
      if next = MAX then .Err .Exhausted
      else registerPaths known us (added ++ [({ id := inc next, path := u.path, graph := u.graph } : PathKind)]) (inc next)
    | some k =>
      if k.graph ≠ u.graph then (if k.graph then .Err .GraphDocument else .Err .LegacyDocument)
      else registerPaths known us added next

def propose (u : U64) (s : St) (h : alloc.vec.Vec U8) (deps : List U64) (uses : List PathUse)
    (body : alloc.vec.Vec U8) : Out :=
  if !validHash h.val then .Err .InvalidHash
  else if !deps.all (fun i => s.patches.any (fun p => decide (p.id = i))) then .Err .UnknownDependency
  else match (s.patches.find? (fun p => decide (p.hash.val = h.val))).map (·.id) with
  | some id => .Ok ([], .Proposed id true)
  | none => match registerPaths s.paths uses [] s.repo.paths with
    | .Err e => .Err e
    | .Ok (added, paths) =>
      if s.repo.patches = MAX then .Err .Exhausted
      else if s.repo.events = MAX then .Err .Exhausted
      else
        let id := inc s.repo.patches
        let i := inc s.repo.events
        .Ok ([.PutPatch ⟨id, h, u⟩, .PutBody ⟨id, body⟩] ++ deps.map (fun d => .PutDependency ⟨id, d⟩) ++
          added.map .PutPath ++
          [.SetRepository { s.repo with patches := id, paths, events := i }, .PutEvent ⟨i, u, PROPOSE, some id, none⟩],
          .Proposed id false)

def approve (u : U64) (s : St) (patch : U64) : Out :=
  match s.patches.find? (fun p => decide (p.id = patch)) with
  | none => .Err .PatchNotFound
  | some p =>
    if p.author = u then .Err .IndependentReviewerRequired
    else if s.repo.events = MAX then .Err .Exhausted
    else let i := inc s.repo.events
      .Ok ([.PutApproval ⟨patch, u⟩, .SetRepository { s.repo with events := i },
        .PutEvent ⟨i, u, APPROVE, some patch, none⟩], .Approved patch)

def check (u : U64) (s : St) (patch : U64) (passed : Bool) : Out :=
  match s.patches.find? (fun p => decide (p.id = patch)) with
  | none => .Err .PatchNotFound
  | some p =>
    if p.author = u then .Err .IndependentCheckReporterRequired
    else if s.repo.events = MAX then .Err .Exhausted
    else let i := inc s.repo.events
      .Ok ([.PutCheck ⟨patch, passed, u⟩, .SetRepository { s.repo with events := i },
        .PutEvent ⟨i, u, CHECK, some patch, none⟩], .Checked patch passed)

/-- `o5s_kernel::patch::merge`. -/
def admitK (channel deps : List U64) (approved checks : Bool) (id : U64) :
    core.result.Result (List U64) o5s_kernel.patch.MergeError :=
  if !approved then .Err .Unapproved
  else if !checks then .Err .ChecksFailed
  else if deps.all (fun d => decide (d ∈ channel)) then .Ok (if id ∈ channel then channel else channel ++ [id])
  else .Err .MissingDependency

def mergeError : o5s_kernel.patch.MergeError → Error
  | .Unapproved => .Unapproved
  | .ChecksFailed => .ChecksFailed
  | .MissingDependency => .MissingDependency

/-- An admission batch's state: members so far, channel size, event counter,
admitted count and writes. -/
structure Adm where
  members : List U64
  size : U64
  seq : U64
  admitted : U64
  writes : List Write

def depsOf (s : St) (id : U64) : List U64 := (s.deps.filter (fun d => decide (d.patch = id))).map (·.dependency)
def approvedB (s : St) (id : U64) : Bool := s.approvals.any (fun a => decide (a.patch = id))
def passedB (s : St) (id : U64) : Bool := ((s.checks.find? (fun c => decide (c.patch = id))).map (·.passed)).getD false

def admitOne (u : U64) (s : St) (ch id : U64) (st : Adm) : Option Error × Adm :=
  match s.patches.find? (fun p => decide (p.id = id)) with
  | none => (some .PatchNotFound, st)
  | some _ =>
    match admitK st.members (depsOf s id) (approvedB s id) (passedB s id) id with
    | .Err e => (some (mergeError e), st)
    | .Ok next =>
      if id ∈ st.members then
        if st.seq = MAX then (some .Exhausted, { st with members := next })
        else let i := inc st.seq
          (none, { st with members := next, seq := i, writes := st.writes ++ [.PutEvent ⟨i, u, MERGE, some id, some ch⟩] })
      else if st.size = MAX then (some .Exhausted, { st with members := next })
      else if st.admitted = MAX then (some .Exhausted, { st with members := next })
      else
        let v := st.writes ++ [.PutMember ⟨ch, st.size, id⟩]
        let i := inc st.size
        let i1 := inc st.admitted
        if st.seq = MAX then (some .Exhausted, { members := next, size := i, admitted := i1, writes := v, seq := st.seq })
        else let i2 := inc st.seq
          (none, ⟨next, i, i2, i1, v ++ [.PutEvent ⟨i2, u, MERGE, some id, some ch⟩]⟩)

def admitStep (u : U64) (s : St) (ch : U64) (x : Adm × Option Error) (id : U64) : Adm × Option Error :=
  match x.2 with
  | none => let (f, st) := admitOne u s ch id x.1; (st, f)
  | some e => (x.1, some e)

def merge (u : U64) (s : St) (n : alloc.vec.Vec U8) (ids : List U64) : Out :=
  if !validBatch ids then .Err .InvalidBatch
  else match findChannel s.channels n with
  | none => .Err .ChannelNotFound
  | some c =>
    let st0 : Adm := ⟨(s.members.filter (fun m => decide (m.channel = c.id))).map (·.patch), c.size, s.repo.events, 0#u64, []⟩
    match ids.foldl (admitStep u s c.id) (st0, none) with
    | (st, none) => .Ok (st.writes ++ [.PutChannel { c with size := st.size }, .SetRepository { s.repo with events := st.seq }],
        .Merged c.id st.admitted)
    | (_, some e) => .Err e

def visible (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) : Bool :=
  decide (REPOSITORY_SCOPE.val ≤ a.scope.val) && r.created && decide (r.org = a.org) && decide (r.label.val = n.val)

/-- What `transition` computes. -/
def step (a : Principal) (s : St) : Command → Out
  | .Register n repo => register a s.listings n repo
  | .Create n => create a s.repo n
  | .Fork repo src n => if visible a s.repo repo then fork a.user s src n else .Err .RepositoryNotFound
  | .Propose repo h deps uses body =>
    if visible a s.repo repo then propose a.user s h deps.val uses.val body else .Err .RepositoryNotFound
  | .Approve repo p => if visible a s.repo repo then approve a.user s p else .Err .RepositoryNotFound
  | .Check repo p passed => if visible a s.repo repo then check a.user s p passed else .Err .RepositoryNotFound
  | .Merge repo n ids => if visible a s.repo repo then merge a.user s n ids.val else .Err .RepositoryNotFound
  | _ => .Err .ReadOnly

end o5s_registry.Model
