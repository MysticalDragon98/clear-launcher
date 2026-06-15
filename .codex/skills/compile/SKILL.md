---
name: compile
description: Apply the current uncommitted changes in clear-launcher.recipe.md to the mc-mods codebase, verify them, and commit both the recipe and resulting implementation. Use when Codex is asked to compile, realize, implement, or apply recipe changes from clear-launcher.recipe.md into working project code.
---

# Compile Recipe Changes

Turn the current recipe edits in `clear-launcher.recipe.md` into working code, then commit the recipe and implementation together.

## Workflow

1. Inspect the repository state with `git status --short`.
2. Read the uncommitted diff for `clear-launcher.recipe.md` with `git diff -- clear-launcher.recipe.md`.
3. Read the relevant existing code before editing. Prefer `rg` and targeted file reads.
4. Translate only the recipe changes into code changes. Preserve unrelated user edits.
5. Keep generated application code inside `**Build Folder**` unless the recipe explicitly changes that constraint.
6. Verify the implemented behavior with the narrowest useful command, test, or manual check available in the repo.
7. Review `git diff` and `git status --short` to confirm only intended changes are included.
8. Commit `clear-launcher.recipe.md` and the implemented changes together with a concise message that describes the recipe behavior applied. Stage only the recipe and files needed for the implementation, and never include unrelated user edits.
9. Report what changed, how it was verified, the commit hash, and any remaining gaps.

## Implementation Notes

- Treat `clear-launcher.recipe.md` as the source of requested behavior and include its current changes in the compile commit. Do not rewrite the recipe beyond existing user edits unless the user asks.
- If the recipe change depends on current machine state, inspect that state directly before coding.
- If the recipe conflicts with existing code or cannot be implemented safely, state the conflict and choose the smallest reasonable path forward.
- Do not revert or overwrite unrelated uncommitted changes.
- If unrelated uncommitted changes are present, leave them unstaged and out of the commit.
