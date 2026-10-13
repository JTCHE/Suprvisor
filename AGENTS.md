# Hygiene

## Global

- Directory shape: a directory holds either files or folders, never both.
  `src/` holds `main.rs` (the Cargo entry) and one folder per module.
  A module that fits in one file still gets its own folder with `mod.rs`,
  and the commands share `src/cmd/`, which holds only files.
- File size: 150 to 300 code lines (blank and comment lines not counted).
  300 is the limit in `src/`, 400 in `tests/`. When a file reaches the limit,
  split it by purpose into a directory module before you add more.
- Function size: 60 lines at most (JPL "Power of Ten", rule 4: one printed
  page per function). Split long functions into named steps.
- Ratchet: a change must not grow a file or function that is already over the
  limit. Touch an over-limit file only to shrink it, or split it first.
- A split or a move changes no behavior and no test value. Do it in its own
  commit, with the full tests green before and after.
  - One source of truth, in code and in UI. A value, a rule or a control that
    shows in two places lives in one module that both import — see # Code.
- Use ASD-STE100 Simplified Technical English in all writing: replies, comments,
  commits, pull requests.
- Do not keep backward compatibility. Delete the old path. Do not add fallbacks,
  shims, or migrations.
- Do not write documentation that a person can get from the code. Write a short
  comment in the code instead.
- Do not record a fact that goes stale: page counts, version numbers, file
  inventories, benchmark tables, audit results. Point to the code that holds it.

## Code

### One source of truth

Each value, shape, and rule lives in one place. Paths, asset shapes, design
tokens, and defaults get one definition that every caller imports. A value
written twice will drift.

The two halves count as one codebase. A rule the Rust side decides — what an
asset path looks like, what a page payload holds — is decided there once, and
the front-end consumes it. Do not re-derive it in TypeScript.

If you find the same value in two files, that is a bug. Fix the duplication
before you fix the symptom.

The same holds for UI. A control that shows in two places is one component:
every menu draws through `MenuList`, every tooltip through `TooltipBox`, every
icon button's name through `Hint`, every page-header button through the classes
in `lib/ui/button.ts`. When a control looks or acts different in one place,
make that place use the shared one. Do not restyle the copy.

### Small modules

Split by purpose, not by size. A module does one thing and says so in its name.
Shared logic goes in a module both callers import — never copied, never
re-implemented.

Orchestration goes in the entry points and the indexes. One-purpose logic goes
in focused modules that they call.

### Dependencies

Prefer a maintained library when it removes more complexity than it adds. Use
what the project already has before you write your own or add a package. Read
the documentation and the types before you decide a library cannot do it.

Weigh a front-end package against the window it runs in. This app ships no
JavaScript runtime and starts in well under a second; a package that costs a
megabyte to draw one control is not worth it.

### Change discipline

- Fix the cause, not the symptom. Before you edit a function, find every caller.
  One guard in the shared function beats a guard in each caller.
- Choose the simplest code that meets the current requirement. Do not add
  configuration, indirection, or abstraction for a need that does not exist yet.
- Grow the system in layers. Start with the smallest version that works from end
  to end, then add on top of a product that already works. Never trade a working
  product for unfinished complexity.
- Make the decision for the long term. Do not accept a stopgap that you plan to
  replace.
- Delete more than you add when you can.
- Use names that explain themselves. `positionX`, not `pX`.
- Write a comment only when the code cannot show the reason. Keep it to one or
  two lines, next to the code it explains. A comment that needs a paragraph to
  defend a workaround means the code is wrong — fix the code.