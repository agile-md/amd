# agile-md

A tiny **filesystem Kanban** — tasks are markdown files moved between `todo/`,
`doing/` and `done/` directories. A single static binary plus `git`; no runtime,
no daemon, no database.

- **Status is the folder.** A task's column is simply which directory it's in.
- **Git is the audit trail.** Moving a task is a `git mv`, so `git log --follow`
  reconstructs exactly when it started and finished.
- **Tickets come from templates.** Task files are rendered from
  [MiniJinja](https://github.com/mitsuhiko/minijinja) templates, so the format
  is consistent by construction and yours to change.
- **The template is the form.** Leave an argument out in a terminal and `amd`
  asks — including the fields your own template declares.
- **One flexible ticket.** No ticket types to choose between: a `branch-type`
  gives it a branch (entering `todo` puts you on `bugfix/crash-on-save`), and
  without one it's a ticket with nothing to check out.
- **One command, any repo.** Install `amd` once on your PATH; it finds the
  board at `<repository-root>/tasks` from wherever you are.

## Install

Needs **git 2.29 or later** (`git --version`) on your `PATH`.

```bash
curl -fsSL https://raw.githubusercontent.com/mrdoodles/agile-md/v4/install.sh | bash
```

Downloads the prebuilt binary for your platform (Linux/macOS, x86_64/aarch64)
into `/usr/local/bin` when it's writable, otherwise `~/bin`. Override with
`install.sh --dir <dir>`, pin with `--version vX.Y.Z`, or build locally with
`--from-source` (needs [Rust](https://rustup.rs)). From a clone:

```bash
(cd crates/amd-ui-web && trunk build --release)   # the board's pages, first
cargo build --release        # target/release/{amd,amdui}
# a workspace: crates/amd-lib (the library), crates/amd-cli, and the desktop
# board — crates/amd-ui (Tauri), crates/amd-ui-web (Leptos), crates/amd-ui-model
./install.sh                 # builds and installs
```

On Linux, installing from a clone also adds the board to your application
menu: a launcher entry and the agile-md icon in `~/.local/share`, which is
where the dock and top bar take the icon from.

The desktop board needs [Trunk](https://trunkrs.dev) and the
`wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`)
to build its pages, and WebKitGTK on Linux (`libwebkit2gtk-4.1-dev`). For the
command line alone, `cargo build --release -p amd-cli` needs none of them.

Two commands: **`amd`** is the command line and **`amdui`** is the desktop
board. `amd gui` opens the same window as `amdui`.

## Use

In any git repository:

```bash
amd init                          # scaffold tasks/{todo,doing,done} here
amd new "Publish to Marketplace" --branch-type feature --assignee @me
amd board                         # show all columns (the default)
amd todo    1                     # backlog -> todo, and switch to feature/publish-to-marketplace
amd doing   1                     # todo -> doing (plain move, no branch effect)
amd done    1                     # doing -> done
amd todo                          # no ref: list what's in todo/ instead of moving anything
amd show    publish               # print a task (id or slug substring)
amd edit    1                     # open in $EDITOR
amd abandon 1                     # take it off the board, into tasks/archive/ — and
                                   # delete its branch too, if it's still empty
amd mine                          # your todo and doing tickets, across every project on this machine
amd recover                       # tickets deleted or abandoned: who removed each, and when
amd recover 1                     # bring one back into the backlog, exactly as it was
amd renumber                      # two tickets with the same number: shows the fix
amd renumber --apply              # ...and makes it, in one commit
amd link    1 3                   # relate two tickets
amd tag     urgent                # list every ticket carrying a tag, across every column
amd tags                          # the project's tag list, with how many tickets carry each
amd tags add future               # add a tag (saved as FUTURE); `amd tags remove` takes an unused one off
amd new     "Idea" --draft        # a draft: tagged DRAFT, it stays in the backlog
amd ready   1                     # take it out of draft (`amd draft 1` puts it back)
amd todo    --tag urgent          # any listing form narrows to a tag: board, ls, a
                                   # destination verb, or an epic by name
amd lock    1                     # claim a ticket so someone else's edits and moves
                                   # are refused until you release it or it lapses
amd unlock  1                     # release your own lease
amd unlock  1 --force             # break someone else's — a separate, deliberate act
```

`amd` works from any subdirectory — it resolves the board from the repo root.
Set `AMD_DIR` to use a board directory other than `tasks`.

## What's on a ticket

One kind of ticket, with sensible defaults — fill in what applies and leave the
rest:

| Field         | | |
| ------------- | --- | --- |
| `id`          | automatic | `007`, from the board's counter |
| `title`       | required | names the file, and the branch |
| `assignee`    | optional | who's doing it; assignable later |
| `branch-type` | optional | `feature`, `bugfix`, `hotfix`, `release`, `chore` |
| `branch-name` | automatic | `<branch-type>/<title>`, empty without a branch type |
| `parent`      | optional | the ticket this sits under, any depth |
| `related`     | optional | ids of tickets this depends on |
| `tags`        | optional | always UPPERCASE; from the project's tag list if it has one |
| `created`     | automatic | the date |
| `created-by`  | automatic | who made it, `Name <email>`; never changes |

**Who to ask.** `amd show` and the desktop board's editor say who created a
ticket and who last edited it: "created by Priya Shah on 2026-10-02; last
edited by Sam Lee on 2026-10-08". Last edited comes from git history (a move
between columns doesn't count), so an edit made in any editor is credited
to the right person. A ticket made before `created-by` existed is credited
from git, and gets the field the next time it's saved.

**Tags** are always UPPERCASE, so they stand out. A project can keep a tag
list (Admin in the desktop board, or `amd tags`), saved in the tickets
repository's `.settings`. The editor then offers it as a sorted dropdown, and
a tag that isn't on it is refused: "Prevent arbitrary tags" is on by
default. Turn it off (`amd tags only off`) to allow other tags, which `amd
new` then only warns about and the editor shows dashed. A tag can only be removed from
the list once no ticket carries it. Matching ignores case, so a ticket written
before tags were uppercase still matches.

**Drafts.** A ticket tagged **DRAFT** is a draft: it can't leave the
backlog, or go into a sprint, until the tag is taken off. It can still be
edited, assigned, sized and filed into an epic, or abandoned. Make one with
`amd new --draft`, `amd draft <ref>` or "Save as draft" in the desktop
board; `amd ready <ref>` (or the editor's **Mark ready**) takes it out of
draft, and `amd tag DRAFT` lists them. DRAFT is built in: it's allowed even
when only listed tags are, and it can't be removed from the list.

**The branch type is what makes a ticket a piece of code work**, and it's empty
by default. A ticket without one is still a ticket — a rota, an approval, a
renewal — it just has nothing to check out:

```bash
amd new "Renew the certificates"                    # no branch
amd new "Crash on save" --branch-type bugfix -a alex
amd todo 1      # moves it; "no branch type on this ticket"
amd todo 2      # moves it and switches to bugfix/crash-on-save
```

The branch is made in the repository the ticket names (`--repository`, or
`amd set <ref> repository <name>`). On a board in its own `*-tickets`
repository, the layout `amd init <project>` makes, a ticket that wants a
branch but names no repository is refused on entering todo rather than
branching the tickets repository; `--no-branch` moves it anyway.

The branch types are the ones
[conventional-validator](https://github.com/mrdoodles/conventional-validator)
accepts, so a branch made from a ticket passes its check. `AMD_BRANCH_TYPES`
changes the list.

The branch name is written into the ticket at creation, so you can see it — and
edit it — before any work starts. Because the title becomes a branch, it's
**validated when you type it**: a title with nothing sluggable in it is rejected
rather than producing a ticket that can never start.

## Assignees

```bash
amd new "Add login" --assignee alex   # or -a @me for whoever git says you are
amd assign 3 sam                      # assign an existing ticket
amd assign 3                          # unassign it
```

Tickets are meant to be created now and assigned later, so `assignee` starts
empty. It shows on the board as `@sam`, and the desktop board's User filter
narrows to one person. Who *created* a ticket isn't a field — `git log --follow` on the
file already knows.

## Related tickets

Tickets that depend on each other carry a `related` list — empty by default,
holding ids so a link survives a rename or a move between columns:

```bash
amd new "Session store" --related 1 --related add-logout   # ids or slugs
amd link 3 7                                               # link two that exist
amd link 3 7 --one-way                                     # …or just one end
```

The relation is symmetric: linking 3 to 7 records 7 on ticket 3 *and* 3 on
ticket 7, so it reads the same from either end and you can't forget the other
half. Refs are resolved when they're recorded, so a typo is an error rather
than a dangling link found later.

```markdown
related: [001,002]
```

Prose can still point at tickets with `[[NNN-slug]]` wikilinks; `related` is for
the dependency itself.

## Several repositories at once

**The list fills itself.** Every repository you run `amd` in is remembered, so
the boards you actually work on are all there next session — nothing to
maintain:

```bash
amd repos                     # what's been seen so far
amd repos add ../other-repo   # add one you haven't visited yet
amd repos remove other-repo   # by name or by path
```

`amd repos` works anywhere, including outside a repository, and asking what's on
the list doesn't add to it. A removed repository comes back the next time you
work in it; `AMD_NO_REGISTER=1` keeps the list entirely manual, and then removal
sticks.

On the desktop board a checkbox per repository says which are in view, and the
backlog shows one at a time. With more than one board in view each card says
which repository it came from, since ids restart at 001 in every repo.

The registry is a **list of repositories, and nothing else** — one path per line
in `~/.config/agile-md/repos`, written only when a repository is first seen:

```
# Repositories agile-md knows about, one path per line.
/Users/you/code/agile-md
/Users/you/code/other-repo
```

The tickets themselves are never copied into a store. They're markdown in each
repo, they change when you check out a branch or pull someone's work, and an
index of them would be wrong the moment it was written. Reading a board is a
directory listing and a few small files.

## Assignees

```bash
amd new "Add login" --assignee alex   # or -a @me for whoever git says you are
amd assign 3 sam                      # assign an existing ticket
amd assign 3                          # unassign it
```

The name lands in the ticket's frontmatter (`assignee: "sam"`) and shows on the
board as `@sam`, so it moves with the file and is visible in the diff.

## My work, across every project

`amd mine` (and the desktop board's landing page) lists the tickets
assigned to you in todo and doing, across
every project on this machine (everything `amd init`, `amd join` or `amd
register` has registered), highest priority first, then oldest:

```
doing  high     3 pts  acme              017  Fix the basket total rounding
todo   high     2 pts  acme              015  Screen reader labels on the product page
doing  low      1 pts  fantastic-tools   004  Write the returns policy page
```

"You" is git's identity in each project's own repository (else your global
one), matched against that project's users list for the name the team
knows you by. `--sort age` or `--sort points` changes the
order, and `--json` is for scripts. `amd mine --hide <project>` leaves a
project out on this machine (`--unhide` puts it back, `--all` shows
everything); scratch boards under the temporary directory are never
listed. It reads each board as this machine last saw it.

## Bringing in existing work

`amd import tickets <PATH>` copies another board's tickets into this one
(a bare `amd import <PATH>` does the same): each keeps its column, gets a
fresh number, and keeps its parent and related links within the batch;
a ticket with the same title as one already here is tagged `DUPLICATE`.

`amd import documents [PATH]` copies a folder of documents (the current
directory by default) into the project's `{project}-documents`
repository:

```
cd ~/old-notes
amd import documents --project acme --dry-run   # what would be added
amd import documents --project acme             # branch, commit, push, pull request
```

The folder is mirrored as it is (or under `--into <dir>`): every markdown
file, and every picture or other file a document links to. Hidden files
and folders are left out. A file the repository already has is never
overwritten; it's skipped and listed. Documents are reviewed, so the
import goes on a new `chore/import-documents-…` branch from the default
branch, is pushed, and a pull request is opened with `gh` when the
repository is on GitHub (otherwise amd prints what's left to do). Your
clone goes back to the branch it was on. From inside the project,
`--project` isn't needed. `--dry-run` works for both kinds.

## Working together, and getting tickets back

Every change amd makes is committed and pushed, and before each command it
catches up with the remote: it fast-forwards when you're behind, pushes
anything you made offline, and refuses a change on a clone that has
diverged rather than write on stale state.

Nothing a commit has seen is lost. `amd recover` lists every ticket that was
deleted or abandoned, with who removed it and when, and brings any of them
back exactly as it was. If catching up would remove five or more tickets at
once, amd says so (how many, which commits, by whom) and points at `amd
recover`. If someone force-pushes and rewrites the remote's history, amd
names it as that and changes nothing, so your clone, which still holds the
old history, is there to put it back. Branch protection on `main` stops a
force push in the first place.

## Who can change the project's settings

The project's settings (the WIP limit, lock length, tags, admins,
repositories and the rest) live in the tickets repository's `.settings`,
shared with the whole team. The ticket number counter is kept apart, in
`.next-id`, so `.settings` changes only when someone changes a setting.

**What amd does on its own.** The desktop board shows **Admin** only to the
project's admins: people on the `admins` list in `.settings`
(`admins = Alex Morgan <alex@example.com>, sam@example.com`), or with admin
or maintain access to the tickets repository on GitHub (amd asks through the
`gh` CLI when it's installed). That keeps settings out of everyone else's
way, but it isn't a lock: anyone who can push to the tickets repository can
still edit `.settings` by hand.

**Enforcing it on GitHub.** To make GitHub itself refuse settings changes
from anyone but admins, add a **push ruleset** to the tickets repository.
GitHub then rejects any push that touches those files unless the pusher is
on the bypass list, whatever tool they use. Ticket moves and new tickets
don't touch them, so they're unaffected.

1. In the tickets repository: **Settings → Rules → Rulesets → New ruleset →
   New push ruleset**.
2. Add the rule **Restrict file paths** with `.settings` and `.github/**`.
3. Under **Bypass list**, add **Repository admin** and **Maintain**.
4. Set enforcement to **Active** and save.

GitHub only offers push rulesets on **private repositories on a paid plan**.
They don't exist for public repositories at all (GitHub refuses with "Source
public repos cannot have push rules"), and a private repository on the free
plan gets "Upgrade to GitHub Pro" instead. Check GitHub's current rulesets
documentation for which plans include them.

When a push is refused, amd keeps the change in your clone and says the push
failed; nothing is lost. Ask an admin to make the change, or undo yours with
`git reset --hard origin/main` in the tickets repository once you've checked
nothing else is waiting to be pushed.

## The desktop board

`amdui` opens the board in a window, across every registered repository at
once. It's the front end as its own command — launch it from a dock or a
runner, no subcommand to remember — and `amd gui` opens the same window if
you're already in a terminal. Launched from outside a project, it opens the
board it showed last. It is a Tauri window over a Leptos page, and every
change it makes goes through the same library as `amd`, so the two never
disagree.

- **It opens on a landing page**: **My work** (your todo and doing tickets
  across every project on this machine, highest priority first; click one
  to open it on its board) and **Projects** (click one to open its board,
  or **Documents** for its documents; **Hide** leaves it out of My work on
  this machine). **⌂ Home** in a board's header comes back. "You" is git's
  identity in each project's own repository, so a different email per
  organisation works.
- **Board | Documents**: a project's documents beside its board. The left
  lists every markdown file in the project's `{project}-documents`
  repository by folder, titled by its first heading, and the right reads
  one: tables, code, task lists and pictures from the repository. Links
  between documents open in the reader; web links open in your browser.
  Reading only, for now: documents change through pull requests. A
  documents repository the whole organisation shares (standards, the
  handbook) can sit beside it: name it in Admin, or with
  `amd shared-documents <repository>`, and clone it next to the project's
  repositories. Raw HTML in a document is shown as text, never run.
- **A Backlogs column** of sprints, then epics, beside the four columns. Drag
  a card to the next column, or into and out of an epic or sprint; the move is
  a `git mv`, committed and pushed like `amd`'s. Moving into doing
  locks the ticket for you, and moving it out again unlocks it: a ticket is
  locked while it's being worked on.
- **Click a ticket** to edit it: its fields as a form (title, assignee, branch
  type, tags, points, priority), its relationships (parent, children,
  related, and tickets that link to it), each a click away, and its body as
  rich text, with a switch to the raw markdown. Lock, renew, unlock — or break someone else's lock, after
  a confirmation — from the same window.
- **Filters** across the top: repository, person, a search, the sort order,
  and Reset.
- **New ticket**, **+ Sprint**, **+ Epic**; start a sprint from its box.
- **Preferences**, your own settings on this machine, in
  `~/.config/agile-md/config`: the colour scheme (light, dark or the
  system's, with an accent colour), the zoom, and the board to open when
  amdui starts outside a project.
- **Admin**, the project's settings, shared with the team through the
  tickets repository: auto-commit, tags, the WIP limit on doing, how long a
  lock lasts, renaming epics and sprints, and the ticket template (with a
  preview). It's shown to the project's admins: anyone on its `admins` list
  (`admins = Alex Morgan <alex@example.com>, sam@example.com` in the tickets
  `.settings`), or with admin or maintain access to the tickets repository
  on GitHub (asked through the `gh` CLI). When amd can't tell (no list, no
  `gh`), Admin stays open. It's a convenience, not a lock: anyone who can
  push to the tickets repository can still change `.settings`.
- **Zoom** with **Ctrl +** and **Ctrl −**, and **Ctrl 0** back to 100%; the
  size is remembered on this machine (`ui.zoom`). F5 or Ctrl+R refreshes the
  board without losing your filters.

A **sprint** takes only sized tickets — an unsized one would make its point
total a lie. Once **started** it is a commitment: it takes nothing more and
gives nothing back, and its tickets only move on, to todo. An **epic** takes
anything: it is where work goes before anyone has estimated it. The loose
backlog is just the default epic — a ticket in any epic or sprint can move on
to todo, keeping its epic, and sent back from todo it returns to it (unless
that is a sprint that has started).

`cargo build -p amd-cli` gives you the command line alone — no window or web
stack, which is what CI and scripts want.

## Driving it from the command line

Everything the board does is a command, so a script — or an agent — can work a
board without opening a window. `--no-input` makes it never prompt.

```bash
amd new "Checkout revamp" --no-input   # lands in backlog/
amd set 001 points 5                   # points, epic, order, title, priority
amd set 001 priority high              # low (the default), medium or high
amd group epic checkout --description "the checkout flow"
amd group sprint sprint-1 --days 10
amd set 001 epic sprint-1              # refused unless the ticket is sized
amd group start sprint-1               # one way: no route back to pending
amd group                              # tickets and points per epic and sprint
amd group empty checkout               # every ticket in it, back to backlog/ at once
amd group rename checkout payments     # the folder and every ticket's epic
amd todo 001 --no-branch && amd doing 001 && amd done 001
```

The rules live in the board, not the window, so the command line gets them
too: tickets move one column at a time, a sprint refuses an unsized ticket,
and a started sprint takes nothing more and gives nothing back.

`doing` carries a WIP limit: 2 unless changed (`amd wip-limit <n>`,
`AMD_WIP_LIMIT`, or the board's own `.settings`; 0 turns it off). Todo is
never limited, so work can't get stuck there. A move into a full doing is
refused, naming the limit; `--force` overrides it. The limit only ever
gates entry — moving a ticket *out* of doing always works.

`cargo build -p amd-cli` builds this without the desktop board — the whole
API, no window or web stack, which is what CI and scripts want.

## Shell completion

```bash
amd completions bash > /usr/local/etc/bash_completion.d/amd
amd completions zsh  > "${fpath[1]}/_amd"          # then: rm -f ~/.zcompdump; compinit
amd completions fish > ~/.config/fish/completions/amd.fish
```

`amd completions` with no argument works the shell out from `$SHELL`; elvish and
powershell are available too. The script goes to stdout and the install hint to
stderr, so redirecting gives you a clean file.

Completion covers values, not just names — `amd new --branch-type <TAB>` offers
the branch types this board accepts (including anything you added with
`AMD_BRANCH_TYPES`), `-T <TAB>` the templates, and
`amd set <ref> <TAB>` the fields. Scripts are generated from the CLI itself, so
they can't drift from it.

## The board

In a terminal `amd board` draws with [richrs](https://crates.io/crates/richrs) —
tree guides, a colour per column, counts, and ids that open the ticket when
clicked:

```
TODO  (3)
├── [001] Checkout revamp  (feat)
│   └── [002] Guest checkout  (feat)
└── [004] Update the guide  (docs)
```

Piped or redirected output stays plain text (`    [002] Guest checkout  (feat)`,
indented by depth), so `amd board | grep` keeps working and log files don't fill
with escape sequences. `--plain` or `NO_COLOR=1` forces that everywhere.

## Why a directory instead of one big TODO.md?

A single file gets unwieldy once tasks have real content, and every edit is a
merge-conflict magnet. Separate files keep tasks self-describing, diffable and
independently movable, while the folders give an unambiguous board.

## Development

```bash
(cd crates/amd-ui-web && trunk build --release)   # needed before anything builds amd-ui
cargo test                    # unit tests (slugify, frontmatter, templates, the board)
bash tests/test.sh            # end-to-end CLI spec against throwaway git repos
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy -p amd-ui-web -p amd-ui-model --target wasm32-unknown-unknown -- -D warnings
cargo fmt --all --check
```

To work on the board with live reload: `cargo tauri dev -- --no-default-features`
in `crates/amd-ui` (it runs `trunk serve` for you; `--no-default-features`
serves the pages live instead of the ones built into the binary).

For screenshots, `scripts/demo-board.sh [DIR]` builds a demo project: a
fictional shop with every column, epics, a running sprint, assignees,
priorities, points, tags, a draft and locks, all made-up names at
example.com. It keeps its own config in `DIR/config`, so your own boards and
settings are untouched, and prints the command to open it.

`amd` was pure bash through v3; v4 is the same tool in Rust, which is what
buys the templating. The CLI, the board layout and the file format are
unchanged — v3 boards work as-is.
