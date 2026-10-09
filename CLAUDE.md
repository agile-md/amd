# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Scope

`agile-md` is a tiny **filesystem Kanban**. It ships two binaries over one
library — **`amd`**, the command line, and **`amdui`**, the desktop board —
which manage markdown task files moved between `backlog/`, `todo/`, `doing/`
and `done/` directories. Board data lives in each *consuming* repo at `<repo-root>/tasks/`;
they're installed globally and operate on whatever repo you're in. Rust +
`git`, shipped as static binaries — no runtime, no daemon, no database.
Design principles: **status is the folder**, **git history is the audit trail**
(moves are `git mv`), tasks are self-describing markdown **rendered from
MiniJinja templates**.

## Layout

A **cargo workspace: one library, two interfaces** (ADR-0004), so the command
line and the desktop board share one implementation of the board and the
boundary is enforced by the dependency graph rather than by review:

```
crates/amd-lib/       the library (`agile_md`) — everything that touches the board
crates/amd-cli/       → binary `amd`    (clap, inquire, the $EDITOR scratch file)
crates/amd-ui/        → binary `amdui`  (Tauri 2: the window and its commands)
crates/amd-ui-web/    the board's page  (Leptos 0.8, CSR, built to wasm by Trunk)
crates/amd-ui-model/  types and pure rules amd-ui and amd-ui-web share (wasm-safe)
```

The library keeps the crate name `agile_md`, so every `use` in the workspace is
unchanged. The desktop board is described under "The desktop board" below.

The terminal board (`crates/amd-tui/`, tickets 080/081) and the Tauri spike
(`crates/amd-tauri/`, ticket 078) were removed in ticket 082: the desktop
board is being rebuilt on Tauri and Leptos (ticket 084), which makes both
redundant. Their history is in git if it is ever wanted again.

- `crates/amd-lib/src/lib.rs` — the library: `board`, `board_settings`,
  `branch`, `create`, `documents`, `git`, `group`, `project_trust`, `registry`,
  `settings`, `task`, `templates`, `model`, `richtext`, `transition` (and `gui` behind
  the feature, pending phase 4). **No `render`** — drawing is a front end's
  business.
- `crates/amd-lib/src/transition.rs` — moving a ticket the way `amd todo`/
  `doing`/`done` do: the move, the lease on entering `doing` (ticket 156), the
  branch switch on entering `todo`, and the auto-commit (`with_auto_commit`).
  Shared by every front end so a move leaves the board, git history and
  working tree in the same state from any of them. **Nothing here
  prints** — a front end that owns the screen can't have the library writing
  over it — so it returns ordered `Event::Info`/`Event::Warn`s, which the CLI
  prints on stdout/stderr (`report()` in `main.rs`). The desktop board's
  `move_ticket` uses `move_to` (lease and commit) but not `enter_todo`: the
  board never switches branches, because `enter_todo` would switch the
  *tickets* repository's branch (ticket 053). It also holds `create_ticket`/
  `edit_ticket`/`save_text` (what `amd new`, a field edit and the board's
  editor do, with the auto-commit).
- `crates/amd-lib/src/richtext.rs` — the line-oriented body rules with no
  drawing in them: `classify`, `continuation`, `toggle`, `spans`. Any front
  end's line editor follows them (the desktop board edits rich text in the
  page instead, through `amd-ui-model::rich`).
- `crates/amd-lib/src/model.rs` — the front-end-free board model: `Card`, `RepoBoard`, `read_lanes`/`read_groups`, `card_visible`/
  `card_in_group`, `rank_between`, `splice_body`, and `load_repos()` (the
  current project's board plus every declared, registered repository). The
  desktop board reads tickets through it, as any other front end should.
- `crates/amd-lib/src/create.rs` — making a ticket, split into `Draft::prepare` (resolve and
  render, touching nothing) and `Draft::write` (file, child link, backlinks).
  That gap is what lets the CLI put the rendered ticket in `$EDITOR` before
  anything is written, while the desktop board writes what it already has.
- `crates/amd-lib/src/group.rs` — epics and sprints: the folders a backlog is divided into.
- `crates/amd-lib/src/settings.rs` — the shared config file, which keeps keys it does not
  recognise so one writer cannot delete another's.
- `crates/amd-lib/src/board_settings.rs` — the board-local settings file
  (`<repo-root>/.settings`, git-tracked, hidden): the `commit` auto-commit
  toggle and the project's `users` (ticket 088 —
  `Name <email>, …`, git's own identity shape, parsed on `>` so a comma in
  a name survives; email is the identity) live here. **So does every other
  board setting** (ticket 111): `project`, `repos`, `wip-limit`,
  `lease-hours`, `ticket-numbering`, `sync`. Ticket 057 had moved some of
  them into `{project}-documents/.settings/` so they'd be PR-reviewed, but
  a change there only reached the team once a documents PR merged, and the
  copies drifted (agile-md's registered repos sat uncommitted in documents
  while its tickets copy was stale). Now `load`/`save` use only the
  tickets file, and a setting change goes out like a ticket move:
  `amd wip-limit`, `amd register`/`register --remove` and amdui's Settings
  all run through the auto-commit path. `migrate_from_documents` (run by
  the CLI after `catch_up`, and by amdui's `change()`) carries a documents
  `.settings/` across once — `repos` unioned, other keys filling gaps,
  tickets winning a clash — commits it, says so, and is silent after.
  `documents_root()` survives only to find that old directory: sibling path
  first, then a project-scoped registry lookup, never a machine-wide name.
- `crates/amd-lib/src/current_project.rs` — several projects in one folder
  (ticket 093). The *project directory* is the folder holding a project's
  repositories (a repo's parent, or the current directory outside any repo);
  `.current-project` there (one line, machine-local, never committed) names
  the project `amd` uses. A candidate's project name is its directory minus
  `-tickets`, else its board's declared `project`, else its directory name.
  `pick()` is what `Board::discover()` calls at both sibling-scan sites;
  `keep_existing()` is what `init`/`join` call so adding a second project
  never switches you (it records the one already there as current). The
  CLI side is `crates/amd-cli/src/project.rs` (`amd project`, `amd project
  use [name]`); `amdui` reads `Board.projects` and calls `use_project`, which
  writes the same file and moves the process to the folder so the file —
  not whichever tickets repo it was launched in — decides.
- `crates/amd-lib/src/project_trust.rs` — a per-machine, trust-on-first-use
  check that a board's declared `project` hasn't changed unexpectedly since
  `amd` last saw it.
- `crates/amd-lib/src/renumber.rs` — `amd renumber [--keep T] [--apply]`
  (ticket 146): tickets sharing a number (`local` numbering can clash when
  clones create apart). The first committed keeps it, judged by commit
  order of the first commit adding that file name anywhere on the board
  (not `git log --follow`, which traces template-alike tickets back to the
  first ticket); `--keep` overrides. Each other one gets the next number:
  `git mv` plus its `id`, and `record_id`. Full `[[stem]]` links are
  rewritten; a bare number in `parent`/`related` only when the moved
  ticket's own `## Children`/`related` proves which ticket it meant, else
  reported as ambiguous and left. Without `--apply` it only shows the plan.
- `crates/amd-lib/src/my_work.rs` — My work (ticket 020): every project
  registered on this machine (`Registry` entries with a board, scratch
  boards under `std::env::temp_dir()` skipped, named by
  `current_project::project_name`), each with its declared repositories'
  boards (`boards_of`, as `load_repos` does for one project); hidden ones
  in your own config (`ui.hidden-projects`, by path). `my_work(email,
  name, …)` gathers todo and doing tickets assigned to you, as git's
  identity in each board's repository (else `email`/`name`): names from each
  project's users list by email, git's `user.name` only where the email
  isn't listed (else one person's git name would match another's tickets).
  Sorted by priority then oldest (`Sort::{Priority, Age, Points}`). Read
  on demand, no cache, no fetching. `amd mine` (CLI, works from anywhere)
  prints it; the desktop board's landing page shows it.
- `crates/amd-lib/src/recover.rs` — `amd recover` (ticket 144): tickets
  removed from the board, found with `git log --diff-filter=D -M` (so a
  move between columns isn't a removal), each read back from its removing
  commit's parent. Listed with who removed it and when; "abandoned" when the
  commit says `abandon` or this machine's archive holds it, else "deleted".
  Only tickets not on the board now. `restore` writes one into the loose
  backlog exactly as it was, keeping its id, and refuses if that number is
  on the board. The CLI filters with `--by` and `--since` and restores
  `<id>...` or `--all` in one auto-commit. A force push that rewrote the
  remote's history is out of its reach (that's 145 and branch protection).
- `crates/amd-lib/src/import.rs` — `amd import <path>`: reads a tasks
  directory tree, treating it as a read-only `Board` (`Board::at`), and
  copies each ticket into the current board — status kept, id renumbered
  against the destination's own counter, `parent`/`related` remapped when
  both ends are part of the same batch, same-titled tickets (within the
  batch or against the destination) tagged `DUPLICATE`. `archive/` isn't a
  destination. A ticket that fails to parse (no frontmatter, unreadable) is
  skipped and reported, not a hard failure for the whole batch. `--dry-run`
  computes and reports everything without writing or spending an id.
- `crates/amd-lib/src/import_documents.rs` — `amd import documents
  [PATH] [--into DIR] [--project NAME] [--dry-run]` (ticket 079; `amd
  import` takes `tickets <PATH>` or `documents [PATH]`, and a bare path is
  still tickets). `plan` mirrors the tree: markdown, plus files a document
  links to or shows inside the tree (a link scan, not a parser), hidden
  files skipped, symlinks not followed; an existing destination is a
  clash, skipped and listed, never overwritten. It refuses the documents
  repository itself, a folder inside it, or one holding it. `apply`
  refuses a clone with uncommitted tracked changes or a detached HEAD,
  fetches, branches `chore/import-documents-<UTC stamp>` from
  `origin/HEAD` (else `origin/<current>`, else the current branch),
  commits, pushes, opens a PR with `gh` only when `origin` is on
  github.com (else prints what's left), and switches back. Handled before
  `ensure_board()` so it never offers to create a board; `--project`
  resolves through the registry (scratch boards included, unlike My
  work), else `Board::discover()`. The target is
  `documents::sources`' own repository.
- `crates/amd-cli/src/main.rs` — the `amd` binary: clap CLI and command handlers; it also owns
  `form.rs`, `value.rs`, `completions.rs` and `init.rs`, which are CLI-only.
- `crates/amd-cli/src/init.rs` — `amd init <project>`: finds or creates
  **two** private GitHub repositories, `<project>-tickets` and
  `<project>-documents` (via `gh`, as siblings under the current
  directory), clones each, scaffolds them and registers both locally.
  Tickets gets the board;
  documents gets `adr/`, `rfc/`, `docs/`, `README.md` and `CHANGELOG.md`
  — no settings (ticket 111: they all live in the tickets `.settings`). Each repository gets its own branch
  protection tier via `github.rs`'s `protect_main()` — tickets: blocks
  force-push/deletion only; documents: additionally requires one approving
  review — both `enforce_admins: true` so the owner can't silently bypass
  it, neither ever sets `require_last_push_approval` (that combination is
  what previously made a solo pusher's own PRs permanently unmergeable
  elsewhere in this org). **This org's repos are private on the free
  plan**, where GitHub's branch-protection API (both the classic endpoint
  and the newer rulesets one) returns `403 Upgrade to GitHub Pro` for
  *any* payload — `protect_main` treats that as `ProtectionOutcome::Unavailable`,
  not an error, and `init` just warns; review on documents is a human
  convention until the org upgrades or the repos go public, not something
  GitHub enforces yet. **Refuses to run from inside an existing git
  repository** — a guard, not an inference: an earlier version tried
  placing the new repos as siblings of whatever repo you happened to be
  standing in, which was a real source of confusion. `--local-only` is the
  command for "inside a repo" — it skips all of the above (tickets board
  only, no documents sibling, no protection) and just scaffolds here, the
  way bare `amd init` always has.
  **Both forms record whoever ran it as a user** (ticket 090,
  `add_first_user`): git's `user.name <user.email>` (`User::from_git`) into
  the tickets `.settings`'s `users`. A brand-new repository's initial
  commit carries it; an existing one (re-running `init` is how a project
  made before 090 gets its first user) gets its own `.settings`-only commit,
  pushed when there's an `origin` (`git::commit_paths` vs
  `commit_paths_locally`). **Before writing, an existing clone is caught
  up** (ticket 094, `ready_to_record`): one with uncommitted edits to
  tracked files is left alone, otherwise `sync::catch_up(Write)`
  fast-forwards it (or pushes its own earlier commits); a diverged clone
  is refused. Either way it's skipped with a warning to fix it and re-run,
  never reset or stashed. A rejected push only warns — never `join`'s
  reset-and-retry, which is safe only in a seconds-old clone, and this may
  be someone's real working copy. No git email warns too; `init` still
  succeeds. `--local-only` records it without committing.
- `crates/amd-cli/src/join.rs` — `amd join <project> [--org]` (ticket 088):
  everyone but the project's creator. Clones `<project>-tickets` and
  `<project>-documents` as siblings with **plain `git clone`**, never `gh`
  — the user's SSH key is the only credential; nothing is created and no
  protection is touched. URL is `{AMD_GIT_BASE}{owner}/{project}-{kind}.git`,
  `AMD_GIT_BASE` defaulting to `git@github.com:` (the test suite points it
  at local bare repos). Refuses inside a git repo, and if either directory
  already exists in any form. Checks the tickets clone has a board and a
  matching `project`, adds git's `user.name`/`user.email` to `users`,
  commits `join: Name <email>` and pushes — **always**, regardless of
  `AMD_COMMIT`. A rejected push is retried (up to 5 attempts, jittered
  backoff) by `fetch` + `reset --hard origin/<branch>` + re-applying the
  change: safe only because the clone is seconds old and its one local
  commit is ours — don't reuse that pattern on a clone with real work in
  it. Any failure before the push lands removes the directories that run
  created, so a retry starts clean. Reuses `init`'s `owner_for` and
  `register`.
- `crates/amd-cli/src/github.rs` — `protect_main(full_name, require_review)`:
  the only thing that shells out to `gh api` rather than plain `gh repo`/
  `git`. Needs the branch to exist first (protection is set after the
  first commit is pushed, not before).
- `crates/amd-ui/` — the `amdui` binary; see "The desktop board" below.
  `amd gui` launches it (`cmd_gui`: the `amdui` beside `amd`, else on
  `PATH`), so `amd` carries no window or web stack at all.
- `crates/amd-lib/src/board.rs` — board discovery (`<repo-root>/${AMD_DIR:-tasks}`), the
  `Column` enum, task lookup, moves, `create` (scaffolding — the interactive
  "create one?" prompt itself is `ensure_board()`, CLI-owned).
- `crates/amd-lib/src/task.rs` — `Task` (path, column, id, stem), frontmatter `meta` lookup,
  `slugify` (transliterates via `deunicode`, so a title in any script still
  makes a plain-ASCII filename and branch; symbols stay separators so an emoji
  doesn't become a word). `points` is fixed sprint-planning Fibonacci
  (`VALID_POINTS`: `1 2 3 5 8 13`), enforced once, in `validate_points`,
  called from both write paths (`set_points`, the CLI's `amd set points`;
  `apply_edit`, the combined field save; `set_text`, the desktop board's
  whole-ticket save) so none can produce an invalid value — empty always
  allowed, clears the field. Not configurable: nobody asked for a board to
  pick its own scale, and a fixed list is what makes the board's points
  field a dropdown (`amd_ui_model::ticket::VALID_POINTS`) rather than free
  text.
- `crates/amd-lib/src/templates.rs` — the MiniJinja `Environment`, built-in templates, board
  overrides, `TaskContext`, `required_extras`.
- `crates/amd-cli/src/form.rs` — every interactive prompt, built on `inquire` (text, select,
  confirm, task picker, label autocomplete).
- `crates/amd-lib/src/branch.rs` — the branch-type taxonomy (`feature`, `bugfix`, `hotfix`,
  `release`, `chore`, overridable with `AMD_BRANCH_TYPES`) and git ref-name
  validation.
- `crates/amd-cli/src/render.rs` — board output: a rich tree (`richrs`) on a
  terminal, plain greppable text everywhere else. It lives in the CLI because
  it is the CLI's output; `richrs` is not an amd-lib dependency.
- `crates/amd-cli/src/completions.rs` — `amd completions [SHELL]`, generated from the clap
  command with `clap_complete`; `$SHELL` detection and install hints.
- `crates/amd-cli/src/value.rs` — `TypedValueParser`s that advertise their possible values, so
  completions and `--help` list them while validation stays ours.
- `crates/amd-lib/src/git.rs` — thin wrappers over the `git` CLI: repo
  discovery and file state (`repo_root`, `is_tracked`, `mv`, `config`),
  branches (`current_branch`, `branch_exists`, `switch_branch`,
  `commits_ahead`, `branch_pushed`, `delete_branch`, `set_head_branch`), and
  a small set for `amd init`'s tickets-repo bootstrap (`remote_url`,
  `commit_all`, `push`) and `amd join`'s clone-and-retry (`clone`, `fetch`,
  `reset_hard`).
- `crates/amd-lib/templates/*.md.jinja` — the built-in templates, `include_str!`'d into the
  binary.
- `install.sh` — downloads the prebuilt `amd-<target>.zip` from the GitHub
  release, or builds from source in a clone (`--from-source`, `--dir`,
  `--version`). Keep the two paths in sync. Both paths install `amdui` as well,
  guarded by `if [ -f ... ]` — `--version` can pin a release made before the
  zip carried both. Every binary goes in through `replace_bin` (ticket 098): copy beside
  it, then `mv` over it, never `cp` onto it — Linux refuses to write into a
  running executable ("Text file busy"), which left an install half done. A
  running one (found via `/proc/*/exe`) gets a "restart it" note.
  **On Linux it also installs amdui's launcher** (`install_launcher`,
  ticket 073): `amdui.desktop` and the icons (`app-icon.svg` plus the 32,
  128 and 256 px PNGs from `crates/amd-ui/icons/`) into
  `${XDG_DATA_HOME:-~/.local/share}`. GNOME's dock and top bar take an app's
  icon from its launcher entry, matched by app id, never from the window,
  so without it amdui shows a generic icon. The app id is `amdui`: Tauri
  leaves GTK's as the program name unless `app.enableGTKAppId` is set,
  which would also make GTK route a second `amdui` to the first process.
  The icons come from the clone, so a release download run outside one
  installs no launcher and says so.
- `tests/test.sh` — assert-based end-to-end suite; builds with cargo and spins
  up temp git repos. Unit tests live next to the code in `#[cfg(test)] mod tests`.
- `.github/workflows/ci.yml` — fmt + clippy + cargo test + tests/test.sh, and
  shellcheck for the shell files.
- `.github/workflows/release.yml` — on a `vX.Y.Z` tag, builds `amd` and
  `amdui` for Linux and macOS, x86_64 and aarch64 (ticket 085): WebKitGTK on
  Linux, the wasm target and Trunk, `trunk build`, then `cargo build -p
  amd-cli -p amd-ui`, a `--version` smoke test, and `amd-<target>.zip`
  holding both. A `release` job then creates the GitHub Release (`gh release
  create --verify-tag --generate-notes`) and attaches every zip, so no
  release is made by hand; a platform whose build failed is left out with a
  warning. amd's own workflow, not `lite-actions/rust-release`, because only
  amdui needs the Tauri steps (decided 2026-10-08). `workflow_dispatch` is a
  dry run: artifacts only, no release. macOS x86_64 is cross-compiled on the
  Apple-silicon runner; the binaries aren't signed or notarised.
- `docs/adr/` — architecture decision records: why the tool is shaped as it
  is, and what each decision costs. `docs/workspace-split.md` is the plan for
  the amd-lib / amd-cli / amd-ui split (ADR-0004).
- `scripts/demo-board.sh [DIR]` — builds a fictional "Acme" project for
  screenshots (ticket 147) through the `amd` CLI itself: tickets, code
  repositories, users, tags, two epics, a started sprint, a draft, every
  column, a documents repository and a shared handbook, locks only in doing, creation dates spread back. Own
  `XDG_CONFIG_HOME` in `DIR/config`; refuses a DIR it didn't make
  (`.agile-md-demo` marker). Shellchecked in CI.
- `README.md`, `LICENSE` (MIT).

## How `amd` works (architecture)

- `Board::locate()` resolves the board as `$(git rev-parse --show-toplevel)/${AMD_DIR:-tasks}`,
  so `amd` works from any subdirectory; it errors if not inside a git repo.
- `Board::discover()` is `locate()` plus **two** fallbacks (ticket 061 added
  the second), used by `ensure_board()` (so `amd register` goes through it
  too, like any other ordinary command — it's only the *path* `amd
  register` defaults to when none is given that still resolves via
  `git::repo_root()`, unchanged, since that specifically means "the repo
  I'm standing in") and the desktop board's `model::load_repos()`. Not used by `amd init
  --local-only`, for the same "repo I'm standing in" reason.
  - **Outside any git repository at all**: looks at the current
    directory's own immediate children for exactly one that's both a git
    repo and has a board of its own (`project_siblings()`) — the layout
    `amd init <project>` creates, a plain parent folder holding
    `{project}-tickets` and `{project}-documents` as siblings. Lets
    `amd`/`amdui` run from that parent directory itself, not only one `cd`
    deeper. Two or more qualifying children **errors**, naming all of
    them, never a guess, same as `find` on an ambiguous ticket reference —
    there's no sane fallback here, `locate()` already failed outright.
    `.current-project` (ticket 093) resolves it when set, and the error
    names `amd project use`.
  - **Inside a git repository that has no board of its own** (ticket 061 —
    `locate()` alone "succeeds" here regardless, since it never checks
    `tasks/` actually exists, which is exactly what stopped this from ever
    running before): checks *that repository's own parent* the same way,
    so standing inside the **code** repo — a sibling of the tickets repo,
    not an ancestor of it — finds the real board next door instead of
    treating the code repo as an empty one of its own. Two or more
    qualifying children here does **not** error, unlike the case above —
    it falls back to `locate()`'s own (possibly nonexistent) answer
    silently, same as finding none at all. A repository's parent is far
    more likely to hold unrelated other repositories than a directory
    purpose-built by `amd init` is — confirmed the hard way: `tests/test.sh`'s
    own scratch boards, all siblings under one shared temp root, made an
    unconditional ambiguity error here break unrelated, pre-existing tests
    that create a fresh repo and expect today's plain "no board yet"
    behaviour. Only a genuinely unambiguous single sibling redirects.
    **Except** (ticket 093): `.current-project` decides when it's set, and
    two or more `*-tickets` siblings with no choice made is the
    plain-directory case's error after all — that's the `init`/`join`
    layout, a folder of projects, where a silent "no board" hid the problem
    (it silently lost the agile-md board from inside `amd/` as soon as a
    second project was initialised beside it). Unrelated scratch boards
    still fall back quietly.
- Tasks are `NNN-slug.md`. `Board::next_id()` takes whichever is higher of the
  counter (its own file, `<repo-root>/.next-id`, ticket 150, so `.settings`
  holds only settings and a rule limiting who may change it (151) never
  stops anyone creating tickets; a board from before still has a `next-id`
  key in `.settings`, or the oldest a `<board>/.next-id` file, and the
  first time a number is handed out the highest of them moves into the
  counter file and the old places are cleared) and one past the highest id
  in the columns. The counter is the
  authority — it remembers ids whose tickets have gone, archived or deleted,
  which no scan can — and the scan is the safety net, so a lost or badly
  merged counter can't hand out an id already in use. `record_id()` runs in
  `Draft::write`, not `prepare`: an abandoned draft leaves no gap. The
  archive isn't scanned; the counter covers it.
- The columns are `backlog`, `todo`, `doing`, `done` (`Column::ALL`, in that
  order). **`amd new` lands in `backlog/`** — new work is raised, then pulled
  into `todo` when someone means to do it. `tests/test.sh` drifted from the
  tool for exactly this reason, so it is worth stating plainly.
- `Board::find(<ref>)` resolves a numeric id or a unique slug substring to one
  task (errors on 0 or >1 matches).
- `Board::move_task()` uses `git mv` for tracked files (history follows the
  rename), else `fs::rename`. It does **not** commit — the user commits the move.
  It returns where the ticket landed and **prints nothing** — `amd` prints
  `moved X -> done/` from `move_and_report`, and the board says it with a card
  that has moved.
- **Destination-as-verb dispatch** (ADR-0006): `amd todo`/`doing`/`done`/
  `backlog`/`abandon <ref>` move a ticket there; leave off `<ref>` and the
  same verb lists that destination instead of moving anything — one command
  name serves both. The same works for any epic by name (`amd checkout 7`
  files it, `amd checkout` lists it). Entering `todo` is the one with a side
  effect: it creates or switches to the ticket's branch — `amd start` doesn't
  exist any more, this replaced it. `amd abandon` also deletes the ticket's
  branch if it's still empty, since a ticket can be abandoned from `todo/`
  before real work starts.
- **Tags are UPPERCASE and can come from a project list** (ticket 122).
  Every writer stores them uppercase (`task::normalize_tag(s)`: `Draft`,
  `apply_edit`, `set_text`), and every match ignores case
  (`Task::has_tag`, `card_visible`), so older lowercase tickets still
  match. The project's list is `tags` in the tickets `.settings`
  (`board_settings::{tags, add_tag, remove_tag}`), with `prevent-arbitrary-tags` (formerly `tags-only`, still read and renamed on the next write; Admin's "Prevent arbitrary tags", **on unless set to
off**, ticket 141) to
  refuse unlisted tags (`check_tags`: a warning, or an error when on).
  `model::remove_unused_tag` refuses a tag any ticket on the project's
  boards carries (`model::tag_counts`, archive excluded), so removing one
  never changes a ticket. CLI: `amd tags [add|remove|only on|off]`, and
  `amd new` warns or refuses. amdui: Admin's Tags section (add form,
  remove-unused dropdown, the switch), the editor's `TagPicker`, and
  `save_ticket` checks the list; `Board.tags`/`tags_only` carry it to the
  page.
- **Drafts** (ticket 114): a ticket tagged `DRAFT` (`task::DRAFT_TAG`,
  `Task::is_draft`/`set_draft`) can't leave the backlog (`Board::move_task`)
  or go into a sprint (`Board::set_epic`); an epic, edits and abandoning are
  fine. DRAFT is one of `task::RESERVED_TAGS`: `check_tags` always allows
  it, `add_tag` and `remove_unused_tag` refuse it, and amdui's `TagUse`
  carries it as `reserved`. CLI: `amd new --draft` (the form asks),
  `amd draft`/`amd ready <ref>`. amdui: a dashed DRAFT badge on the card,
  a Draft banner with Mark ready in the editor, "Save as draft" in New
  ticket, and `amd_ui_model::epic::draft_rule` refuses the drag. Named
  commands win over epic names, so an epic called `draft` or `ready` is
  reached through `amd group`/`amd set epic` instead.
- **Tag filtering**: `amd tag <tag>` lists every ticket carrying `<tag>`,
  across every column, via `Board::tasks_tagged()`/`Task::has_tag()`. Every
  listing form also takes `--tag <tag>` to narrow it down: `amd board`,
  a bare destination verb (`amd todo`, `amd backlog`, …)
  and an epic by name (`amd checkout --tag urgent`) all thread it through to
  `render::columns()`/`render::named()`, which filter before building the
  tree. Moving a ticket (`<ref>` given) ignores `--tag` — it only applies to
  the no-ref, listing form. The `Verb` external-subcommand path (epic names)
  parses `--tag`/`-t` out of its raw args by hand, since clap hands that
  whole tail over as an opaque `Vec<String>`.
- **Leases** (`docs/adr/0007-concurrency-and-locking.md`, currently only
  surviving at `~/develop/github/agile-md/to-sort/old-amd-docs/adr/` —
  `docs/adr/` was never restored into this repo after the git-history reset):
  `locked-by`/`locked-until` frontmatter, stored as an **absolute expiry**
  rather than a duration so every check (`Task::write_checked`,
  `Board::move_task`/`archive`/`set_epic`) is a self-contained
  `now > locked_until` comparison against data already in the file — no
  `Board` handle needed except at the one place a duration is actually
  chosen (`Task::take_lease`, called from `amd lock` and from
  `transition::move_to` on entering `doing`, which both front ends use, both of
  which already hold one). Identity is `git::config(repo, "user.email")` —
  `git.rs`'s `config()` takes an explicit repo path like every other
  function there, not the process's current directory, so a desktop board
  open across several registered repositories resolves each ticket's own
  identity correctly. `take_lease`/`release_lease` need no special-casing:
  both are ordinary `write_checked`-protected writes, and its generic "leased
  by someone else" gate already gives exactly the right semantics for each
  (blocked if someone else holds it; blocked unless *you're* the holder).
  Only `break_lease` (`amd unlock --force`) bypasses that gate — the one
  deliberate act ADR-0007 wants kept separate from an ordinary write.
  **A ticket is locked exactly while it's in doing** (ticket 156; it used
  to be todo or doing): `move_to` takes the lease on entering `doing`, and
  calls `Task::release_if_mine` on leaving it in any direction or on
  landing in `done` or `backlog` (ticket 092), inside the same
  `with_auto_commit` closure, so the move and the release are one commit.
  `Board::archive` does the same before archiving (best-effort; the archive
  is untracked). A lock taken by hand (`amd lock`) on a backlog or todo
  ticket stays until it leaves doing, done or the backlog takes it.
  `release_if_mine` only ever clears the current identity's own lease,
  lapsed or not; someone else's, even lapsed, stays for `unlock --force`.
  **A lapsed lease is said as such** (ticket 110): still recorded, it
  holds nothing, but amdui's `TicketSummary`/`TicketDocument` carry it as
  `lapsed` beside `lock` (which stays "in force" only, so no write check
  changed). The editor says "Your lock lapsed at …" / "Locked by X until
  … (lapsed)", the card badge is dashed, and `amd show` prints the same on
  stderr (`lock_note`), leaving stdout exactly the file.
  `board_settings::lease_hours()` resolves the duration (`AMD_LEASE_HOURS`,
  then the board's own `.settings`, then 8 hours) the same
  `settings::resolved()` precedence every other setting uses.
- `Board::archive()` (`amd abandon`) moves a ticket to `<board>/archive/`,
  which is **not** a column: it's off the board and its contents are
  gitignored. That means `git mv` would refuse the ignored destination and
  forcing it would put the ticket back into the history the `.gitignore`
  exists to keep out, so a tracked ticket is `git rm --cached`'d and then
  moved. Archived ids are never reused — that's the counter's job — because
  reusing one would silently repoint every `parent` and `related` that named
  it.
- `ensure_board()` (`crates/amd-cli/src/main.rs` — CLI-owned, not a `Board`
  method) runs before any board-requiring command: if there's no board, it
  prompts to create one when interactive (`stdin().is_terminal()`),
  auto-creates when `AMD_YES=1`, and otherwise errors (never hangs
  non-interactively).
- Env vars: `AMD_DIR` (board dir name, default `tasks`), `AMD_BRANCH_TYPES`
  (the branch types on offer), `AMD_YES` (force-create), `AMD_NO_INPUT` (never
  prompt), `AMD_NO_BRANCH` (never touch branches), `NO_COLOR`/`--plain`
  (plain board), `EDITOR` (for `amd edit`), `AMD_LEASE_HOURS` (lease
  duration, default 8), `AMD_WIP_LIMIT` (doing's WIP limit — unset or
  unparsable defaults to **2**, not unlimited; `0` turns it off; todo is
  never limited, so `AMD_WIP_TODO` is ignored since ticket 083), `AMD_COMMIT` (auto-commit-and-push,
  unset = **on**; `0`/`off`/`false` turns it off), `AMD_GIT_BASE` (where
  `amd join` clones from, default `git@github.com:`), `AMD_SYNC` (catch up
  with the remote, default on, ticket 102), `AMD_TICKET_NUMBERING`
  (`local`/`cloud`). **`AMD_TYPES` is dead** —
  it was in `--help` long after nothing read it. **`AMD_NO_REGISTER` is also
  gone** (2026-09-26) — registration is a deliberate act (`amd register`)
  again, not something running in the background to opt out of.
  `AMD_BRANCH_TYPES` and `EDITOR` also fall back to the settings file
  (`branch-types`/`editor`) when unset — the env var wins when both are set
  (`settings::resolved()`). `AMD_LEASE_HOURS`/`AMD_WIP_LIMIT`/
  `AMD_COMMIT` follow the same precedence against the *board-local*
  `.settings`'s `lease-hours`/`wip-limit`/`commit` keys instead — team
  decisions, not personal ones. `wip-limit` (2026-09-27) replaced an
  earlier `wip-doing` key entirely.
- **Auto-commit-and-push** (ticket 012, `board_settings::auto_commit`,
  `main.rs`'s `with_auto_commit`) — every ticket-mutating command commits
  (and pushes) its own change, the audit trail a dedicated, board-only
  `{project}-tickets` repository has no other way to accumulate (nothing
  else in it ever changes at the same time a ticket does, so there's no
  natural commit for a move to ride in on). Default **on** — the opposite
  default from every other setting in this file, deliberately: the feature
  is broken until it's active, so off-by-default would ship it in a state
  where the common case needs a setting found first.
  - **The paths committed are computed, never hand-listed.**
    `git::dirty_paths` snapshots the repo before and after the mutation
    runs; `git::changed_paths` diffs the two. This isn't a simplification
    over passing an explicit paths list — it's the fix for a real bug a
    hand-maintained list would have shipped with: `Board::record_id` (via
    `amd new`) also writes the `.next-id` counter, so a new ticket
    genuinely touches two files in one operation, and nobody wrote that
    call site expecting to enumerate the counter too.
  - **The diff has to be signature-based, not presence-based.** An
    untracked file's `git status` is always `??` regardless of content, so
    a path already dirty in the *before* snapshot that changes again by the
    *after* snapshot is invisible to a plain set difference — hit directly
    on a board created with `amd init --local-only` (which never commits),
    where `.settings` is untracked from the start and a second `amd new`
    needs its *second* bump noticed. `dirty_paths` maps each path to a
    modified-time signature instead of returning a bare set, and
    `changed_paths` compares signatures, not just presence.
  - **`git status`'s default collapses a wholly-untracked directory into
    one line.** Also hit directly on an `--local-only` board: without
    `--untracked-files=all`, a brand new ticket inside a never-committed
    `tasks/` never appears as its own path, because git reports the whole
    directory as a single untracked entry.
  - **`commit_paths` skips `add` for a path that no longer exists.**
    `git mv` (used for a tracked rename) already stages both sides of the
    move in the index by itself; re-`add`ing the now-gone old path
    afterward fails with "did not match any files", since there's nothing
    left for `add` to reconcile. The commit's own pathspec still needs
    both paths, gone or not, to record the whole rename — same reason a
    two-commit rename needs both paths on the second commit, not just the
    first.
  - **`changed_paths` drops a rename hop `git` has already forgotten** —
    found live, against the real `agile-md-tickets` repo, not anticipated:
    a ticket already mid-rename (staged, uncommitted) *before* this
    feature's first use on it, then moved again before either side ever
    landed in a commit, collapses in git's own index straight from the
    original committed path to the final one — the intermediate hop never
    existed in any commit, so it stops appearing in `git status` at all.
    Without also requiring the path to still be a key in the `after`
    snapshot, `changed_paths` kept reporting it as "changed" (dirty in
    `before`, simply absent from `after`), and handing that stale path to
    `git commit` failed outright. No data was ever at risk — the file had
    already moved correctly on disk; only the automatic commit failed,
    caught by the same warn-not-fail policy below.
  - **Best-effort, matching `maybe_take_lease`'s own tone**: a commit or
    push failure (locked signing key, no agent, network down, a moved
    remote) warns to stderr and moves on — never fails the command, never
    rolls back. The board change already succeeded on disk; failing at
    that point would misreport a move that actually worked.
  - **Scope**: every verb in `Board::move_task`/`archive`/`set_epic`/
    `Task::assign`/`take_lease`/`release_lease`/`break_lease`, plus every
    `amd group` command including `empty` (one commit for every ticket it
    moves, not one per ticket — a single logical action), and declaring or
    undeclaring a repository (`amd register`, `register --remove`,
    `deregister`). Not in scope: this machine's registry alone (a
    `deregister` of a repository that's only registered here),
    `{project}-documents` (a different repository entirely, PR-reviewed by
    design, never auto-committed).
  - **Sync with the remote** (ticket 102, `crates/amd-lib/src/sync.rs`).
    Before it, nothing fetched or pulled, and a rejected push only warned:
    the multi-user suite (076) proved duplicate ids, a lease claimed twice
    and invisible teammates' tickets. Two halves:
    - **`catch_up(board, Purpose)`**, before a command: the CLI calls it
      once in `run()` (`sync_purpose` classifies each command: a ticket
      ref = Write, a listing = Read); amdui calls it in `change()` (Write)
      and every 30s from the page (`sync_board`, Read). It fetches (a Read
      skips that if amd's last fetch was under 10s ago), then: only behind →
      `merge --ff-only`; only ahead → push the leftovers; diverged → a
      Write refuses with the fix, a Read warns. No remote/upstream, or
      `AMD_SYNC=0`/`sync = off`, does nothing; no network only warns.
    - **amd's fetch never writes `FETCH_HEAD`** (ticket 118,
      `git::fetch`): `--no-write-fetch-head`, with `.git/amd-fetched`
      touched instead as the "fetched recently" timestamp
      (`fetched_within`). `FETCH_HEAD` belongs to `git pull`, which writes
      it and reads it back; amdui's 30s fetch landing mid-pull made the
      pull fail with "Cannot fast-forward to multiple branches" (142 of 200
      pulls in a race loop, 0 after). This is why amd needs **git 2.29+**.
    - **`after_rejected_push`**, from `transition::commit_since` (now
      commit-locally, then push — and `cmd_new` goes through it too): a
      push rejected because the remote moved is replayed with `rebase @{u}`
      when that's clean **and** `Board::duplicate_ids()` is empty; otherwise
      the written files are copied to `$XDG_STATE_HOME/agile-md/recovered/`,
      the commit is dropped (`reset --keep`), the clone fast-forwarded, and
      `ConcurrentChange` returned — an `Err`, so a non-zero exit. Any other
      push failure keeps the old warn-and-keep-local behaviour; the next
      catch-up pushes it. **Note:** two clones bumping `next-id` to the
      *same* value rebase cleanly (identical change), which is exactly why
      the duplicate-id check exists.
    - **Two warnings for a board being trashed** (ticket 145): a
      fast-forward that removes `MASS_REMOVAL` (5) tickets or more still
      happens (it's recoverable), with a warning naming the count, ids,
      commits and authors and pointing at `amd recover`
      (`removed_tickets`). And when commits this clone had from origin
      are no longer in origin's history (`rewritten`: an earlier position
      of the upstream, from its reflog, is in HEAD but not in `@{u}`), a
      force push rewrote it: a Write refuses and a Read warns, naming it as
      such, and nothing is pushed back or reset; the message points at
      `git push --force-with-lease` for a person to decide. Checked only
      when the clone is ahead, so an ordinary divergence never trips it.
    - `tests/test.sh` forces the race deterministically with a one-shot
      `pre-push` hook in clone B that makes clone A push first (`race()`,
      clearing git's per-repo env with `git rev-parse --local-env-vars`).
  - **Ticket numbering** (`crates/amd-lib/src/numbering.rs`): `local` (the
    counter + scan, unchanged, the default) or `cloud` — a stub for a future
    paid plan where a service hands out numbers; it refuses with "needs an
    agile-md plan". `ticket-numbering` lives in the tickets `.settings` like every setting;
    `AMD_TICKET_NUMBERING` overrides. `Board::next_id`/`record_id` dispatch
    through it, so no caller changed.
  - **`amd log`** (`cmd_log`) renders the board's transition history —
    `date time action id title sha` — from `git log --name-status -M`,
    reading paths and rename status only, never the commit message, so it
    works retroactively over history from before this feature existed too.

## Ticket fields (`crates/amd-lib/src/branch.rs`, `templates/ticket.md.jinja`)

- **One ticket, one template.** There are no ticket types any more: `admin` and
  `development` collapsed into fields you either fill in or don't.
- **`branch-type` is what gives a ticket a branch**, and it's empty by default.
  `branch-name` is derived from it and the title at creation and stored, so it
  can be edited before work starts. No branch type means no branch, and
  `amd todo` says so and leaves the working tree alone.
- The values are the **branch** convention (`feature`, `bugfix`, `hotfix`,
  `release`, `chore`), matching mrdoodles/conventional-validator's branch check
  directly — there's no longer a commit-type-to-branch-prefix mapping to keep in
  step. `AMD_BRANCH_TYPES` overrides the list.
- **`priority`** is `low`, `medium` or `high` (`task::PRIORITIES`), `low`
  by default — the built-in template writes it, and `Draft::write` fills it
  in for a custom template that doesn't (`task::ensure_priority`), and
  refuses an invalid one. `Task::priority()` reads a missing one as `low`;
  `amd set <ref> priority <value>` sets it (ticket 083).
- `assignee` says who's doing it, empty by default: tickets are created now and
  assigned later. It was briefly called `owner`; `Task::assignee` still reads
  that key. **Who created it is stored; who last edited it isn't** (ticket
  155, reversing an earlier "no second person field" rule): `created-by:
  "Name <email>"` is written once at creation (`TaskContext.created_by`,
  the built-in template; `Draft::write` fills it for a custom template via
  `authorship::ensure_created_by`) and never changed. "Last edited by" is
  read from git (`authorship::last_edited`: the latest commit changing the
  file, a pure `R100` move not counting), because a stored copy would go
  stale on every edit made outside amd. Names come from the users list by
  email. A ticket from before 155 is credited from the first commit adding
  its file name and gets the field on its next save (`transition::save_text`).
  `amd show` prints `authorship::summary` on stderr; amdui's editor shows
  it under the lock bar (`TicketDocument.authorship`). **Picked from the project's `users`** (ticket
  089): `amd new`'s form and a bare `amd assign <ref>` in a terminal open
  `form::assignee` (type-to-filter: nobody, `@me`, the users, someone
  else); amdui's editor has `AssigneePicker`, fed by `Board.users`/`me`.
  The *name* is stored. A name off the list is allowed with a warning
  (`board_settings::unknown_assignee`; never for a board with no list).
  Without a terminal, bare `amd assign` still clears; `--clear` says so.
- **Frontmatter keys are hyphenated, template variables are not** —
  `branch-type: {{ branch_type | yaml }}`. Jinja would read `branch-type` as a
  subtraction, so the variable has to be `branch_type`; the key is just text.
- `Task` reads the new keys and **falls back to the old ones** (`type`,
  `branch`, `owner`), so boards written before this change still list, enter
  `todo/` and filter correctly. There's no migration command; a ticket picks
  up the new shape when it's rewritten.
- Titles are validated against git's ref rules at creation — the title becomes
  the branch, so a title with nothing sluggable in it is rejected up front.
  `slugify` returns an empty string in that case; callers must reject it.
- **Entering `todo/` is what creates the branch now** (`cmd_enter_todo`,
  `crates/amd-cli/src/main.rs`) — this replaced the old `amd start`, retired
  outright, not kept alongside the new verbs. It takes the branch from
  `--branch`, else the ticket's `branch-name`, else derives one from
  `branch-type` and the title (`--no-branch` skips all of this). **Order
  matters**: `git mv` is staged first so the rename travels with the switch.
  The branch goes in the clone of the ticket's `repository` (ticket 107).
  **A dedicated tickets repository never gets one** (ticket 053,
  `transition::enter_todo`): on a board whose repository is named
  `*-tickets`, a ticket that wants a branch but names no repository is
  refused before the move, naming `amd set <ref> repository` and
  `--no-branch`. A board inside a code repository (`init --local-only`
  there) is that repository and branches as before.

## Board rendering (`crates/amd-cli/src/render.rs`)

- Each column is a **tree**: `forest()` nests tasks by `parent`, ordered by id
  at every level. A child whose parent is in another column stays at the top
  level with a `^NNN` marker — the columns are the board's primary structure.
  `forest()` is a pure function over `(Task, Option<String>)` pairs, so it's
  unit-tested without touching the filesystem; parents are read once per column
  rather than per lookup (each read is a file read).
- Rich (`richrs::Tree`) only when stdout is a terminal and neither `--plain` nor
  `NO_COLOR` is set; otherwise the same tree as indentation, which is what
  `tests/test.sh` asserts on and what pipes into `grep`.
- Ids are **OSC-8 hyperlinks** in the rich path (`TERM=dumb` opts out), so
  clicking one opens the ticket. They are deliberately not in the plain path:
  escape sequences have no business in a pipe. Note this is why the board moved
  off `richrs::Table` — a table measures cell widths and would count the escape
  bytes, misaligning every border. (`richrs` draws the one-shot `amd board`
  printout only; the interactive board is `amdui`.)
- A rendering failure falls back to plain output rather than erroring: never
  let cosmetics stop someone seeing the board.

## Forms (`crates/amd-cli/src/form.rs`)

- **Prompting is always optional.** `form::available()` gates every prompt on
  `stdin` *and* `stdout` being a terminal, plus `--no-input`/`AMD_NO_INPUT=1`.
  Non-interactively a missing value is an error with the usage line — the same
  contract the bash version had, and why the tool never hangs in CI.
- Optional arguments drive it: `amd new` with no title runs the full form
  (title, branch type, assignee, parent, related, tags), `-i` re-asks for what was
  passed, and `show`/`edit` with no `<ref>` open a task picker across every
  column (`resolve()`). Destination verbs (`todo`/`doing`/`done`/`backlog`/
  `abandon`) are different: no `<ref>` lists that destination instead of
  picking — one command name serves both (ADR-0006).
- **The form is derived from the template**: `templates::required_extras()`
  scans the source for `extra.<key>` / `extra["key"]`, and `cmd_new` prompts
  for each one that `--set` didn't supply. Adding a field to a template adds a
  question — there is deliberately no separate field registry to keep in sync.
- `tests/test.sh` exports `AMD_NO_INPUT=1` so the suite stays hermetic when run
  from a terminal; it covers the non-interactive halves of these paths. Drive
  the interactive halves by hand through a pty (`script -q /dev/null amd new`).
- Esc/Ctrl-C map to a plain `cancelled` error, not a panic or a partial task.
- **The form ends in the body.** `form::body()` renders the ticket into a temp
  file, opens `$EDITOR` on it (splitting the command so `code --wait` works)
  and returns what came back; `cmd_new` only writes to the board afterwards.
  A non-zero editor exit or an emptied file creates nothing. Deliberately not
  inquire's `Editor` prompt: that waits for an `e` keypress first, which is the
  stop this was meant to remove. Default on when the form ran or `-e`; off with
  `--no-edit` and whenever prompting isn't available, so scripts and CI never
  spawn an editor. Unit-testable without a TTY — pass `true`/`false` as the
  editor command.

## Templates (the reason this is Rust)

- Built-ins are compiled in via `include_str!` (`ticket`, `board-readme`).
  Anything in `<board>/templates/<name>.md.jinja` overrides or adds to them;
  `amd templates eject <name>` writes an editable copy there.
- The environment is deliberately configured: `UndefinedBehavior::Strict` (a
  typo'd variable is an error, not a blank line), auto-escape **off** (output is
  markdown, not HTML), `keep_trailing_newline`, `trim_blocks`, `lstrip_blocks`.
- Custom `yaml` filter quotes/escapes frontmatter values, so a title containing
  `"` can't corrupt the block. Use it for every frontmatter value.
- `TaskContext` is the contract with templates (`id`, `number`, `title`, `slug`,
  `branch_type`, `branch_name`, `owner`, `parent`, `parent_link`, `related`, `tags`, `created`, `timestamp`, `column`,
  `author`, `email`, `created_by`, `board`, `template`, `extra`). Adding a field is additive;
  renaming one breaks every user template — treat it as a public API.
- Template errors are flattened by `render_error()` (message + line + cause
  chain + MiniJinja debug info); without that only the top line survives.

## Several repositories: declared names + local paths, not one shared list

Two cooperating halves, deliberately kept separate — reworked 2026-09-26
after the old single, per-machine registry predated the two-repo topology
and stacked *every* repository ever worked in, across every unrelated
project, into one `amdui` window:

- **Declared** (`board_settings::repos()`/`declare_repo()`, board-local
  `.settings`, git-tracked): the repository *names* a project spans, shared
  with the whole team. One member adding a repository means everyone sees
  it listed on their next pull. Comma-separated, same shape the global
  settings file's `branch-types` already uses — no new persistence
  mechanism.
- **Registered** (`crates/amd-lib/src/registry.rs`, `$XDG_CONFIG_HOME/agile-md/repos`,
  per-machine, unchanged in shape from before): whether a declared name is
  actually cloned on *this* machine, and where. *Which* names are worth
  asking about always comes from the current project's own declared list,
  never from scanning this file's entries wholesale. **Lookups are scoped
  by project directory** (ticket 095): the same project name can live in
  two folders, so `Registry::find_near(name, project_dir)`/`clone_of(from,
  name)` take the clone in the current project's folder, else the only
  clone of that name anywhere, else nothing (ambiguous → shown as not
  cloned, never a guess). `find(name)` is only for `deregister`. A path
  that no longer exists is skipped on load and so dropped on the next
  save. Registering (`init`, `join`, `register`) a clone of a remote
  that's already registered elsewhere **warns** (`same_remote`: `origin`s
  compared via `git::normalise_remote` — case, scheme, user, port, `.git`,
  SSH host aliases resolved with `ssh -G`; no `origin` falls back to the
  first commit), never refuses. There is deliberately no
  store of tickets in either half: the markdown is the source of truth, it
  changes under you on every checkout and pull, and an index would be
  stale the moment it was written.
- **`amd register [path]`** (defaults to the repo you're in) does both
  halves at once — declares the name for the project, registers the path
  locally — and is the *only* writer now. Registration is deliberate again:
  `registry::remember()`'s run-on-every-command auto-fill is gone, along
  with `AMD_NO_REGISTER` (nothing left to opt out of).
- **Which project `amd register` declares on** (ticket 103,
  `register_board` in `main.rs`): normally the board you're in. From a
  repository with no board of its own, in a folder holding several
  projects, that's whichever `.current-project` names, so it **asks** in a
  terminal ("Register X with project P (this folder's current project)?")
  and **refuses** without one, naming the fixes. It hints `--project Y` when
  the repo's name starts with another project's name. `--project <name>`
  always says outright. `deregister` (and `register --remove`) go through
  the same check, worded "Take X off project P?" (ticket 120).
- **`amd deregister <name>`** (ticket 120, reversing the earlier
  local-only design): a repository the project **declares** comes off the
  project for everyone, exactly as `amd register --remove` (now an alias):
  undeclared in the tickets `.settings`, committed and pushed, and dropped
  from this machine's registry. The repository filter and fields list the
  declared repositories plus any a ticket names (`ticket_repos`), so it
  leaves everyone's lists unless a ticket still names it. A repository
  that's only *registered* here (a project's own tickets or documents
  repository, a scratch board) just loses its registration. Neither is an
  error. The clone is never touched. It takes `--project` like `register`.
- **`amd clone [name | --all]`** (ticket 108, `crates/amd-lib/src/clone.rs`):
  clones a declared repository this machine hasn't got into the project
  directory, from the tickets repo's own `origin` with the last path part
  swapped for the name (`sibling_url` — keeps an SSH host alias, works
  with local bare repos), then registers it. No name lists the missing
  ones. Refuses an undeclared name, one already cloned for this project,
  and a directory in the way. amdui offers the same from the Repository
  filter: a "(not cloned — clone…)" entry opens `CloneRepoDialog`
  (`clone_repository` command); the editor's select stays disabled.
- **There are no repository aliases** (removed in ticket 097). The
  machine-wide `tickets`/`documents` short names ticket 026 added only ever
  pointed at whichever project last ran `init`/`join`, so with several
  projects on one machine they misled. Repositories go by their real
  `{project}-tickets`/`-documents` names; `amd deregister` takes the real
  name. A leftover `$XDG_CONFIG_HOME/agile-md/aliases` file is ignored.
- **`model::load_repos()` cross-references the two.** `Board::discover()`
  finds the current project (a hard error now if none is found — `amdui`
  operates on exactly one project, never a cross-project stack); its own
  board is always `repos[0]`. Each declared name with a registered,
  board-having local entry becomes another `RepoBoard`; each declared name
  *without* one goes into `unregistered` and renders greyed out and inert
  in the `View::Board` repo row — visible, so a teammate who's added a
  repository knows it exists, not silently omitted.
- Ids restart at 001 in every repository, so a `Card` carries which repo it came
  from; the board shows the name whenever more than one is in view, and a move
  goes through *that* repository's `Board`, not the current one. A drag between
  repositories is refused: the ticket would need a new id, not a rename.
- `Registry::load_from`/`save_to` take a path, so tests need no environment
  mutation. The CLI suite sets `XDG_CONFIG_HOME` to a temp dir — `amd
  register` writes to the config directory and a test must never touch the
  user's.

## Completions

- Generated, never checked in: `amd completions <shell>` renders from
  `Cli::command()`, so a new flag is completable the moment it exists. Handled
  before `ensure_board()` — it must work outside a repo, which is exactly when
  people set completion up.
- Value-level completion comes from `crates/amd-cli/src/value.rs`. `TypedValueParser` lets a
  parser advertise `possible_values()` (used by completions and `--help`) while
  `parse_ref()` keeps our own validation and error text — `PossibleValuesParser`
  alone would have replaced "unknown branch type 'x' … set AMD_BRANCH_TYPES"
  with clap's
  wording. `TicketType` advertises the built-ins but accepts any name, since a
  board can add templates.
- `clap`'s `string` feature is on for `Str: From<String>`, which the runtime
  value lists need.
- The script goes to stdout, the install hint to stderr, so a redirect gives a
  clean file. `tests/test.sh` runs `bash -n` over the generated bash script.

## Commands

```bash
(cd crates/amd-ui-web && trunk build --release)   # the board's page, before anything builds amd-ui
cargo test                          # the whole workspace
cargo build -p amd-cli              # the CLI alone: no window or web stack
bash tests/test.sh
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy -p amd-ui-web -p amd-ui-model --target wasm32-unknown-unknown -- -D warnings
cargo fmt --all --check
bash install.sh [--dir DIR] [--version vX.Y.Z] [--from-source]
```

## Coding style

- Edition 2024, `rust-toolchain.toml` pins **stable**, MSRV 1.88 (let-chains).
  CI gates on `cargo fmt --check` + `cargo clippy -D warnings` + tests.
- Errors: `anyhow` with `.with_context()`; `main` prints `amd: {err:#}` and
  exits 1, matching the old bash `die()`.
- Shell out to `git` rather than linking a git library — behaviour is exactly
  what the user would get typing the command.
- No `unwrap()`/`expect()` on anything user input can reach.
- Keep the CLI surface stable: `tests/test.sh` is the spec, and the assertions
  there are the compatibility contract with v3 boards.
- The shell files still follow the repo's bash rules (see the `shell-scripting`
  skill): quote the literal `"done"` (SC1010), `set -euo pipefail`, and
  shellcheck `--severity=warning` clean.

## Versioning & releasing

Cut releases by tag: `git tag -a vX.Y.Z && git push --tags` triggers
`release.yml`, which builds and attaches `amd-<target>.zip`. Force-move the
major tag (`git tag -f -a vN`) and push it, since `install.sh` and the README
`curl` URL point at `raw…/agile-md/vN`. Invocation model by major (breaking
changes):

- **v1** — script vendored per board (`tasks/task`); board = the script's own dir.
- **v2** — global `task` command; board discovered from the repo root; `task init`.
- **v3** — command renamed `task` → **`amd`**; env `TASK_YES`/`TASKS_DIR` →
  `AMD_YES`/`AMD_DIR`.
- **v4** — bash script → **Rust binary**; tasks rendered from MiniJinja
  templates; `inquire` forms when a value is missing; one flexible ticket whose
  `branch-type` decides whether entering `todo/` makes a branch; a desktop
  board. Two binaries over one library: `amd` and `amdui`. Install downloads a
  prebuilt binary instead of copying a script. The v3 CLI and board layout
  still work; v3 task files simply have no labels, so entering `todo/` on one
  leaves the branch alone. **Within v4 itself**, destination-as-verb dispatch
  (ADR-0006) later retired the original `amd start`/`amd back`/`amd rm`
  vocabulary outright in favour of `todo`/`doing`/`done`/`backlog`/`abandon` —
  no new major version, since the ticket file format didn't change.

## Epics and sprints (`crates/amd-lib/src/group.rs`, ADR-0009)

- A group is a directory under `backlog/` with a `_group.md` describing it.
  **Inside `backlog/` the folder decides** which group a ticket is in; outside
  it there are no group directories, so the `epic` frontmatter key is the only
  carrier. `Board::tasks_in_group` treats the two cases separately — do not
  collapse them.
- A sprint's scope is a query over **every column**, not a directory listing. A
  ticket in `doing/` still belongs to the sprint it was committed to. Counting
  `backlog/` alone made a sprint's points fall as work progressed.
- **A started sprint takes nothing more and gives nothing back** (ticket
  083, replacing ADR-0009's "in and out" rule — the desktop board's rules
  were adopted in amd-lib). `Board::set_epic` refuses filing a ticket into
  a started sprint and taking one out of it, in any column; its tickets
  only move on, to todo (`move_task`), and archiving straight out of one is
  refused too. Any group refuses an unsized ticket if it is a sprint.
- **The loose backlog is the default epic.** A ticket in any epic or sprint
  moves straight on to todo (`move_task` from its folder) and keeps its
  `epic` there. Sent back from todo, `Board::home_group` puts it back in the
  group its `epic` names — unless that group is gone, is a started sprint,
  or is a sprint it no longer has points for; then the loose backlog.
- **Moves go one column at a time** (`Column::next_to`, checked first in
  `move_task`): backlog → todo → doing → done and back, never a jump.
- **`Board::rename_group`/`amd group rename <old> <new>`** moves the folder
  (`git mv` when tracked), rewrites `_group.md`'s `name`, and relabels the
  `epic` of every ticket in it, in any column — refused up front, changing
  nothing, if any of them is leased by someone else.
- **Both epics and sprints are optional**, and both stay in scope —
  ticket 032's 2026-09-22 note that dropped sprints (this board was
  human-plus-AI only) was itself reversed 2026-09-25: tickets now live in a
  pushed, git-tracked repository, which makes a real team a near-term case.
  Nothing here changed as a result; sprints were never actually removed
  from the code, only from that ticket's own notes.
- **`Board::empty_group`** moves every ticket a group holds back to the
  loose backlog at once, no precondition — reuses `set_epic(task, None)`
  per ticket rather than a new mechanism, so it inherits the same
  lease/hash checks that setter already has, and skips (rather than
  aborts on) a ticket it can't move. `amd group empty <name>` in the CLI,
  an "empty" button next to a group's "edit" button in `amdui`.
- **The WIP limit** (`board_settings::wip_limit`) gates entry into
  `doing` only (ticket 083: a limit on todo as well could leave work stuck,
  so `wip-todo`/`AMD_WIP_TODO` are ignored) — `Board::move_task` checks the destination column's count
  against the configured limit (env, then `.settings`, same precedence as
  `lease_hours`) before the existing lease check, refusing with
  `WipLimitExceeded` unless `force` is set. Moving a ticket *out* of an
  over-limit column is never blocked, because the check only ever looks at
  where a ticket is going, not where it's leaving from. `move_task`'s
  signature carries `force: bool` now — both its callers (`amd-cli`'s
  `move_and_report`, `amdui`'s `apply()`) pass it through; the GUI always
  passes `false` (no drag-and-drop escape hatch), a blocked drag just sets
  the status line, the same treatment a lease refusal already gets.
  `board_settings::set_wip_limit(column, limit)` is the programmatic
  setter (`amdui`'s Settings modal, and `amd wip-limit <n>` for doing,
  both use it).
  Doing: absent or unparsable defaults to **2** (`DEFAULT_WIP_LIMIT`; it
  was 1 until ticket 083), not unlimited, and `0` is stored *literally* by
  `set_wip_limit` rather than collapsed into clearing, specifically so
  "nobody set it" (→ 2) and "somebody set it to 0" (→ off) stay two
  different states on disk. `set_wip_limit(Column::Todo, …)` is refused.
- **`Board::set_epic` only moves the file inside `backlog/`.** Filing a
  `todo`/`doing`/`done` ticket into a group (or clearing it) tags
  `set_epic_meta` in place and leaves the file exactly where it is — column
  and path both unchanged. Before this branch existed, `set_epic` computed
  its destination unconditionally under `backlog/<name>/`, which would have
  silently relocated a `doing`/`done` ticket back into the backlog; nothing
  called it on a non-backlog task until `amdui`'s membership checkboxes
  (below) needed to, so the bug was latent, not yet shipped-and-observed.
- **`Board::delete_group`/`amd group delete <name>`** removes a group's
  `_group.md` and directory outright. Refused (naming the count) if
  `tasks_in_group` finds any ticket still claiming membership, checked
  across every column the same way `empty_group` already does — not just
  the physical folder — since a committed sprint's tickets can sit in
  `doing`/`done` while the backlog folder itself is empty. `empty` it
  first; `delete` does not imply `empty`.
- **Next round of epic/sprint design, not built yet**: sprint backfill, a
  `Closed` state, and burn-up reporting into the documents repo are
  designed in `agile-md-documents`'
  `rfc/0005-sprint-backfill-closing-and-burn-up-reporting.md` — including
  one question it deliberately leaves open (whether a running sprint's
  `todo` is a gate on the existing column or a nested one). Nothing below
  describes any of that; it isn't shipped.

## The desktop board (`crates/amd-ui*`, ticket 084)

- **Tauri 2 + Leptos 0.8 (CSR), built from the grimartz board** that settled
  the rules ticket 083 put into amd-lib. Three crates: `amd-ui` (the Tauri
  app, binary `amdui`), `amd-ui-web` (the page, compiled to wasm by Trunk:
  `trunk build --release` in `crates/amd-ui-web`) and `amd-ui-model` (the
  types and pure rules both sides share; it compiles to wasm, so it has no
  file, git or settings logic).
- **Every change goes through amd-lib.** `amd-ui/src/lib.rs` holds the
  commands; each opens the project as `amd` does (`boards::Project::open`,
  over `model::load_repos()`) and calls amd-lib: `transition::move_to`
  (lease + commit), `Board::set_epic`/`rename_group`/`create_group`/
  `archive`, `Group::start`, `Task::take_lease`/`release_lease`/
  `break_lease`, `transition::create_ticket`/`save_text`, `board_settings`
  for the WIP limit, lease hours and `commit`, and `Templates` for the
  template editor (`with_source` + `example_context` render a preview).
  Changes are serialised by one mutex. Don't add ticket logic to `amd-ui`;
  add it to amd-lib and call it.
- **The editor saves the whole text** (`transition::save_text` →
  `Task::set_text`): hash-checked against the `version` the editor was
  opened with (`ContentHash` as 16 hex digits, since a page can't hold a
  `u64`), priority/points validated, the lease fields always kept as on
  disk. The page derives `branch-name` from the title with
  `amd_ui_model::slugify`, which must match `agile_md::task::slugify` —
  `amd-ui`'s tests check they agree.
- **The Backlogs column lists compact boxes** (ticket 109): each epic or
  sprint shows only its name and an amount — an epic's ticket count, a
  sprint's total points — and stays a drop target. A click opens it as a
  column of its own beside Backlogs (`OpenEpicColumn`, `Ctx.open_epic`,
  one at a time, × or a second click closes it), holding its cards, the
  sprint's days/state/points line and its Start sprint button.
- **Drags map onto amd-lib's moves** (`move_in`): onto an epic or sprint is
  `set_epic` (from todo, `move_task` back to the backlog first); out of one
  to the loose backlog is `set_epic(None)`; anything else is `move_to`. The
  page previews the same rules (`amd_ui_model::epic::{can_move,
  sprint_rule}`, `wip::WipLimits::admit`) so a refused drop is shown while
  dragging; amd-lib is still the authority. A drop on a full doing asks,
  then moves with `force`.
- **The page is embedded at compile time** (`tauri.conf.json`'s
  `frontendDist` is `../amd-ui-web/dist`), so `trunk build` must run before
  anything compiles `amd-ui` — CI does it first. The `custom-protocol`
  feature is on by default so a plain `cargo build` makes a working board;
  `cargo tauri dev -- --no-default-features` in `crates/amd-ui` serves the
  page live from `trunk serve` instead.
- **It follows the files** (ticket 091). The page asks `board_fingerprint`
  every second: `model::fingerprint` hashes each board file's relative
  path, size and mtime (columns, epic and sprint folders, `.settings`; not
  `archive/`) across every loaded repo. When the fingerprint changes, the
  page calls `ctx.refresh()`: a reload, plus a bump of `ctx.disk_tick`. So
  `amd` in a terminal, an editor, a pull or a checkout all show up within
  about a second, with no watcher thread or dependency. A change seen
  mid-drag waits for the drop. amdui's own writes cause one harmless extra
  reload. An open editor checks its own ticket on each tick
  (`ticket_version`): if it has no unsaved edits it reopens from disk;
  with unsaved edits it shows a "changed on disk" banner (Reload / Keep
  editing); moved or removed gets its own banner. The save's hash check
  still refuses a stale write. The header's ↻, F5 and Ctrl+R all call
  `refresh()`. F5/Ctrl+R are intercepted, because a webview reload would
  throw away filters and sort.
- **The landing page** (ticket 020, `amd-ui-web/src/home.rs`): amdui
  always opens on it (`Ctx.view`: `View::Home`/`View::Board`). The `home`
  command returns `amd_ui_model::Home` (My work from `agile_md::my_work`,
  and the projects); `set_project_hidden` flips `ui.hidden-projects`.
  Opening a project or a work item reuses `set_tasks_dir` (which also
  remembers it as `ui.board`), then `open_ticket`. On Home the header
  hides New ticket, Admin and the filter bar, and ↻ reloads Home; on a
  board, ⌂ Home goes back. It's the shell documents (129), ideas (131) and
  collaboration (138) are meant to join. "You" is git's identity in each
  board's own repository, falling back to the global one.
- **Which board a project opens**: `Board::discover()` from the launch directory; from
  outside any project (a desktop launcher starts in `$HOME`) it goes to the
  directory remembered as `ui.board` in `~/.config/agile-md/config`, which
  Preferences' "Board to open" sets.
- **Preferences vs Admin** (ticket 148): Preferences (`preferences.rs`) is
  personal and per machine (`~/.config/agile-md/config`: colour scheme,
  zoom, board to open), never committed. Admin (`admin.rs`) is the
  project's settings in the tickets `.settings`, committed for the team.
  Header: New ticket | Preferences | Admin.
- **Who sees Admin** (ticket 149, `crates/amd-lib/src/access.rs`): the
  project's `admins` list (`board_settings::admins`, emails or `Name
  <email>`), or admin/maintain on GitHub for the tickets repository's
  `origin` (`gh api repos/<owner>/<repo> --jq .permissions`; amd's git is
  SSH, which can't ask the API). Listed wins; with no list, GitHub decides;
  with no list and no answer (no `gh`, offline, not GitHub) Admin is open,
  with a note. amd-ui asks GitHub once per repository per session
  (`boards::admin_access`) and carries `Board.admin {allowed, note}`; the
  page hides the button and Admin shows the note. A convenience, not
  enforcement: the CLI's settings commands aren't gated. The colour scheme is `ui.theme`/
  `ui.accent` in the same file.
- **Documents** (ticket 129, `crates/amd-lib/src/documents.rs`): Board |
  Documents tabs in a project's header (`View::Documents`), and Documents
  on each landing-page project row. `documents::sources` finds the
  project's `{project}-documents` (sibling first, else the registry's
  `find_near`) and the shared one the tickets `.settings` names
  (`shared-documents`, `amd shared-documents`, Admin's "Shared
  documents"); `list` walks markdown files, skipping dot-folders and not
  following links; `resolve`/`read`/`read_bytes` refuse anything outside
  the repository (`..`, absolute, a symlink out, `.git`) and files over
  `MAX_FILE`. `amd-ui`'s `documents.rs` renders with pulldown-cmark (its
  `html` feature is on only there): raw HTML becomes text, a link to
  another document becomes `#doc:<path>`, web/mail links stay and the page
  hands them to `open_external` (`xdg-open`/`open`, http(s)/mailto only),
  anything else becomes `#`, and relative images are inlined as data URLs
  (the CSP allows `data:` images only). Read only: editing is a branch and
  a PR (138). The demo board has `acme-documents` and a shared
  `acme-handbook`.
- **Releases** build `amdui` too (ticket 085, `release.yml`). `install.sh`'s
  download path checks a downloaded amdui's libraries on Linux
  (`check_board_libraries`, via `ldd`) and names WebKitGTK when it's
  missing; its source build needs Trunk, the wasm target and WebKitGTK, and
  skips the board with a note without them.
- **Zoom** (ticket 024): Ctrl +/−/0 step through `amd_ui_model::zoom::STEPS`
  (50–200%); the page's `zoom_keys` sets `ctx.zoom` at once (so fast presses
  step on) and calls `set_zoom`, which applies Tauri's `WebviewWindow::set_zoom`
  and saves `ui.zoom` in `~/.config/agile-md/config`. `zoom` reapplies it at
  start. A real webview zoom, not CSS `zoom`, so `100vh` layouts still fit.
- WebKitGTK quirks met building it: the CSP needs `'wasm-unsafe-eval'` and
  `connect-src 'self' ipc: http://ipc.localhost`; native `<select>`s draw a
  white box (styled with `appearance-none`); `dragDropEnabled: false` so
  HTML5 drag and drop reaches the page.

## Conventions

- **Feature branch + PR for every real change** — `main` is not pushed to
  directly (the one exception was the initial full-source-tree restore after
  this repo's history was reset). Workflow-file changes need a
  `workflow`-scoped token.
- Co-authored commits use the bot identity, not the Anthropic no-reply:
  `Co-Authored-By: Claude <309050497+MrDClaudeBot@users.noreply.github.com>`.
