import Lean.Meta.Tactic.Simp.RegisterCommand
import Lean.Elab.Tactic.Simp
import Lean.Meta.DiscrTree.Util

/-!
The simp sets the law checker's fixed steps rewrite with (spec section 28), one for the passes that keep an int step
folded and one for those that read it, and `sharp_simp_all`, the `simp_all` they run in. `Sharp/Lean/Law.lean` tags each
lemma with the set of each pass that rewrites with it, so the library builds each set's index once, when it compiles,
where a `simp_all only` that listed the lemmas built each one's again on every call.
-/

/-- The lemmas of the fixed steps' first two passes: the monad's steps, a law's claim and the choices it makes, and the
facts a case holds, split into one fact for each branch and each value. An int step stays folded, so two steps that
compute from the same values stay one term. -/
register_simp_attr sharp_step

/-- The lemmas of the fixed steps' third pass and of their last rewrite of a goal the passes leave open: those of
`sharp_step` and the ones that read an int step as the `Int` it gives. -/
register_simp_attr sharp_full

namespace Sharp

open Lean Meta Elab Tactic Parser.Tactic

/-- `into` with each lemma, definition to unfold and erased lemma of `other`. Each lemma keeps the keys its set built. -/
def joinSimpTheorems (into other : SimpTheorems) : SimpTheorems :=
  let add := fun (thms : SimpTheorems) (thm : SimpTheorem) => thms.addSimpTheorem thm
  let into := other.post.foldValues add (other.pre.foldValues add into)
  { into with
    toUnfold := other.toUnfold.fold (·.insert ·) into.toUnfold
    toUnfoldThms := other.toUnfoldThms.foldl (·.insert ·) into.toUnfoldThms
    erased := other.erased.fold (·.insert ·) into.erased }

/-- `simp_all` with one index: the definitions and lemmas the call names, with the lemmas of each set it names joined in,
and each hypothesis `simp_all` reads added to it. Simp tries the indexes of a call in order and takes the first that
rewrites a term, and `simp_all only` keeps its definitions and hypotheses in an index before every set, so with several
indexes each hypothesis is tried on a term before any lemma of a set. A simp set holds simprocs too, and simp reduces
each term it visits to look it up in each set's simprocs, so the call leaves out each set's simprocs that hold none. -/
syntax (name := sharpSimpAll) "sharp_simp_all" optConfig (discharger)? (&" only")?
  (" [" withoutPosition((simpErase <|> simpLemma),*,?) "]")? : tactic

@[tactic sharpSimpAll] def evalSharpSimpAll : Tactic := fun stx => withMainContext do withSimpDiagnostics do
  let { ctx, simprocs, .. } ← mkSimpContext stx (eraseLocal := true) (kind := .simpAll) (ignoreStarArg := true)
  let ctx := match ctx.simpTheorems.toList with
    | first :: rest => ctx.setSimpTheorems #[rest.foldl joinSimpTheorems first]
    | [] => ctx
  let simprocs := simprocs.filter (!·.simprocNames.isEmpty)
  let (result?, stats) ← simpAll (← getMainGoal) ctx (simprocs := simprocs)
  match result? with
  | none => replaceMainGoal []
  | some mvarId => replaceMainGoal [mvarId]
  return stats.diag

end Sharp
