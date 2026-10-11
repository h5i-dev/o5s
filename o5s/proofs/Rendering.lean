import Interval
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Rendering
abbrev Chunk := Usize × Usize × alloc.vec.Vec U8

def weight (chunks : List Chunk) : Nat := (chunks.map fun c => c.2.2.val.length).sum

def emit (bytes : List U8) (state : List U8 × Nat) (chunk : Chunk) : List U8 × Nat :=
  (state.1 ++ (bytes.drop state.2).take (chunk.1.val - state.2) ++ chunk.2.2.val, chunk.2.1.val)

def WellFormed (bytes : List U8) (chunks : List Chunk) : Prop :=
  (∀ i (h : i < chunks.length), chunks[i].1.val ≤ chunks[i].2.1.val ∧ chunks[i].2.1.val ≤ bytes.length) ∧
  (∀ i (h : i + 1 < chunks.length), chunks[i].2.1.val ≤ chunks[i+1].1.val)

def Inv (bytes : List U8) (chunks : List Chunk) (out : alloc.vec.Vec U8) (cursor : Usize) (i : Nat) : Prop :=
  cursor.val ≤ bytes.length ∧
  (∀ h : i < chunks.length, cursor.val ≤ chunks[i].1.val) ∧
  out.val.length + (bytes.length - cursor.val) + weight (chunks.drop i) ≤ Usize.max

@[step] theorem chunks_loop_spec (bytes : alloc.vec.Vec U8) (chunks : alloc.vec.Vec Chunk)
    (wf : WellFormed bytes.val chunks.val)
    (room : bytes.val.length + weight chunks.val ≤ Usize.max) :
    interval.render_chunks_loop bytes chunks (alloc.vec.Vec.new U8) 0#usize 0#usize ⦃ out =>
      (out.1.val, out.2.val) = chunks.val.foldl (emit bytes.val) ([],0) ∧
      Inv bytes.val chunks.val out.1 out.2 chunks.val.length ⦄ := by
  unfold interval.render_chunks_loop
  apply WP.spec_mono (loop_fold2 chunks.val
    (fun out cursor => (out.val, cursor.val)) (emit bytes.val)
    (Inv bytes.val chunks.val)
    (fun st => interval.render_chunks_loop.body bytes chunks st.1 st.2.1 st.2.2)
    ?_ (alloc.vec.Vec.new U8) 0#usize 0#usize (by simp) ?_)
  · intro out h
    simpa using h
  · intro out cursor i hi hinv
    unfold interval.render_chunks_loop.body
    simp only [Inv] at hinv
    h5i_step
    all_goals try scalar_tac
    all_goals
      have idx : i.val < chunks.val.length := by scalar_tac
      have hb : i2.val ≤ cursor1.val ∧ cursor1.val ≤ bytes.val.length := by
        simpa only [← i2_post] using wf.1 i.val idx
      have hc : cursor.val ≤ i2.val := by
        simpa only [← i2_post] using hinv.2.1 idx
      have hw : weight (chunks.val.drop i.val) = v.val.length + weight (chunks.val.drop (i.val + 1)) := by
        rw [List.drop_eq_getElem_cons idx]
        simp [weight, ← i2_post]
    all_goals try scalar_tac
    refine ⟨⟨idx, ?_⟩, ?_⟩
    · simp [emit, ← i2_post, List.append_assoc]
    · unfold Inv
      refine ⟨hb.2, ?_, ?_⟩
      · intro hn
        simpa only [← i2_post] using wf.2 i.val hn
      · simp only [out2_post, List.length_append, List.length_take, List.length_drop]
        scalar_tac
  · simp [Inv, alloc.vec.Vec.new, weight] at *
    omega

def model (bytes : List U8) (chunks : List Chunk) : List U8 :=
  let state := chunks.foldl (emit bytes) ([],0)
  state.1 ++ bytes.drop state.2

theorem finish (bytes accum : List U8) (cursor : Nat) (state : List U8 × Nat)
    (h : (accum,cursor) = state) :
    accum ++ (bytes.drop cursor).take (bytes.length - cursor) = state.1 ++ bytes.drop state.2 := by
  rw [← h]
  simp [List.take_eq_self_iff]

@[step] theorem render_chunks_spec (bytes : alloc.vec.Vec U8) (chunks : alloc.vec.Vec Chunk)
    (wf : WellFormed bytes.val chunks.val)
    (room : bytes.val.length + weight chunks.val ≤ Usize.max) :
    interval.render_chunks bytes chunks ⦃ out => out.val = model bytes.val chunks.val ⦄ := by
  unfold interval.render_chunks
  h5i_step
  all_goals simp_all [Inv, weight, model]
  apply finish
  assumption

end o5s_kernel.Rendering
