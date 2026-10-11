import Interval
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Splice

def replace (bytes : List U8) (start finish : Nat) (replacement : List U8) : List U8 :=
  bytes.take start ++ replacement ++ bytes.drop finish

theorem replace_block (pre old suffix replacement : List U8) :
    replace (pre ++ old ++ suffix) pre.length (pre.length + old.length) replacement =
      pre ++ replacement ++ suffix := by
  simp [replace, List.drop_append, List.append_assoc]

theorem disjoint_blocks_commute (pre oldA gap oldB suffix a b : List U8) :
    replace
      (replace (pre ++ oldA ++ gap ++ oldB ++ suffix)
        pre.length (pre.length + oldA.length) a)
      (pre.length + a.length + gap.length)
      (pre.length + a.length + gap.length + oldB.length) b =
    replace
      (replace (pre ++ oldA ++ gap ++ oldB ++ suffix)
        (pre.length + oldA.length + gap.length)
        (pre.length + oldA.length + gap.length + oldB.length) b)
      pre.length (pre.length + oldA.length) a := by
  have leftFirst := replace_block pre oldA (gap ++ oldB ++ suffix) a
  have leftSecond := replace_block (pre ++ a ++ gap) oldB suffix b
  have rightFirst := replace_block (pre ++ oldA ++ gap) oldB suffix b
  have rightSecond := replace_block pre oldA (gap ++ b ++ suffix) a
  simp only [List.length_append] at leftSecond rightFirst
  simp only [List.append_assoc] at leftFirst leftSecond rightFirst rightSecond ⊢
  rw [leftFirst, leftSecond, rightFirst, rightSecond]

theorem successful_splice (bytes replacement out : alloc.vec.Vec U8) (start finish : Usize)
    (ordered : start.val ≤ finish.val) (bounded : finish.val ≤ bytes.val.length)
    (room : bytes.val.length + replacement.val.length ≤ Usize.max)
    (run : interval.splice bytes start finish replacement = ok out) :
    out.val = replace bytes.val start.val finish.val replacement.val := by
  have h := post_of_ok (Interval.splice_spec bytes replacement start finish ordered bounded room) run
  exact h

/-- Independent byte edits commute with translated coordinates; this conclusion
uses four successful executions of the actual extracted Rust splice function. -/
theorem extracted_disjoint_commutes
    (bytes a b x y left right : alloc.vec.Vec U8)
    (pre oldA gap oldB tail : List U8)
    (a0 a1 b0 b1 shifted0 shifted1 : Usize)
    (snapshot : bytes.val = pre ++ oldA ++ gap ++ oldB ++ tail)
    (ha0 : a0.val = pre.length) (ha1 : a1.val = pre.length + oldA.length)
    (hb0 : b0.val = pre.length + oldA.length + gap.length)
    (hb1 : b1.val = pre.length + oldA.length + gap.length + oldB.length)
    (hs0 : shifted0.val = pre.length + a.val.length + gap.length)
    (hs1 : shifted1.val = pre.length + a.val.length + gap.length + oldB.length)
    (room : bytes.val.length + a.val.length + b.val.length ≤ Usize.max)
    (runA : interval.splice bytes a0 a1 a = ok x)
    (runB : interval.splice bytes b0 b1 b = ok y)
    (runAB : interval.splice x shifted0 shifted1 b = ok left)
    (runBA : interval.splice y a0 a1 a = ok right) : left.val = right.val := by
  have lengthBytes : bytes.val.length = pre.length + oldA.length + gap.length + oldB.length + tail.length := by
    simp [snapshot, Nat.add_assoc]
  have hx := successful_splice bytes a x a0 a1 (by omega) (by omega) (by omega) runA
  have hy := successful_splice bytes b y b0 b1 (by omega) (by omega) (by omega) runB
  have xshape : x.val = pre ++ a.val ++ gap ++ oldB ++ tail := by
    rw [hx, snapshot, ha0, ha1]
    simpa only [List.append_assoc] using replace_block pre oldA (gap ++ oldB ++ tail) a.val
  have yshape : y.val = pre ++ oldA ++ gap ++ b.val ++ tail := by
    rw [hy, snapshot, hb0, hb1]
    have h := replace_block (pre ++ oldA ++ gap) oldB tail b.val
    simpa only [List.append_assoc, List.length_append, Nat.add_assoc] using h
  have xlength : x.val.length = pre.length + a.val.length + gap.length + oldB.length + tail.length := by simp [xshape, Nat.add_assoc]
  have ylength : y.val.length = pre.length + oldA.length + gap.length + b.val.length + tail.length := by simp [yshape, Nat.add_assoc]
  have hl := successful_splice x b left shifted0 shifted1 (by omega) (by omega) (by omega) runAB
  have hr := successful_splice y a right a0 a1 (by omega) (by omega) (by omega) runBA
  rw [hl, hr, hx, hy, snapshot, ha0, ha1, hb0, hb1, hs0, hs1]
  exact disjoint_blocks_commute pre oldA gap oldB tail a.val b.val

end o5s_kernel.Splice
