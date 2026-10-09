//! amd — a tiny filesystem Kanban in markdown (agile-md).
//!
//! Tasks are markdown files that move between `todo/`, `doing/` and `done/` in
//! `<repo-root>/tasks`. Status is the folder; `git mv` is the audit trail; the
//! ticket format is a MiniJinja template you can edit.

mod completions;
mod form;
mod github;
mod init;
mod join;
mod project;
mod render;
mod value;

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::builder::PossibleValuesParser;
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use time::format_description::well_known::Rfc3339;

use agile_md::board::{ABANDON, Board, Column};
use agile_md::create::{Draft, NewTicket};
use agile_md::registry::Registry;
use agile_md::templates::{DEFAULT_TEMPLATE, TEMPLATE_SUFFIX, Templates};
use agile_md::{board_settings, branch, git, task, templates, transition};

const AFTER_HELP: &str = "\
A <REF> is a task id (e.g. 7 or 007) or a unique slug substring. Leave it out
of `amd new` in a terminal and amd fills in a form; leave it out of a
destination verb (see below) and amd lists that destination instead of
moving anything.

One kind of ticket. A branch type — feature, bugfix, hotfix, release,
chore — is what gives it a branch, so `amd todo` on a bugfix titled
\"Crash on save\" creates and switches to bugfix/crash-on-save when it enters
todo/. Leave the type out and there is nothing to check out: the ticket
still moves, the working tree is left alone.

New tickets land in backlog/, priority low. `amd set` sizes, prioritises
(low, medium, high) and files them; `amd group` divides the backlog into
epics and sprints — both optional, and a ticket moves in or out of either
one at a time with `amd set <ref> epic <name>`, or all at once with `amd
group empty <name>`; `amd group rename` renames one and relabels its
tickets. The loose backlog is just the default epic: a ticket in any epic
or sprint can move on to todo, and sent back from todo it returns to its
epic. A sprint takes only sized tickets, and once started it takes nothing
more and gives nothing back — its tickets only move on, to todo. `amd tag
<tag>` lists every ticket carrying it, across every column. Any listing
form — `board`, a bare destination verb, an epic by name — takes
`--tag <tag>` to narrow it to tickets carrying that tag instead.

`doing/` carries a WIP limit (`AMD_WIP_LIMIT`, or `wip-limit` in
`.settings`; `amd wip-limit <n>` sets it) — unset or unparsable defaults to
2, and 0 turns it off. Only doing is limited, so work can't get stuck in
todo. A move into a full doing/ is refused, naming the limit and the
current count; `--force` overrides it. The limit only ever gates entry:
moving a ticket *out* of doing/ is never blocked.

Tickets nest: `--parent <ref>` puts one under another, and the board shows
the tree. Nesting is the parent/child relationship; an epic or sprint is a
folder the backlog is divided into. A ticket can have both.

`amd lock <ref>` claims a ticket's lease for your git identity
(`user.email`), so anyone else's edits or moves on it are refused with a
clear message until it's released or it lapses on its own (8 hours by
default). `amd unlock <ref>` releases your own; `amd unlock <ref> --force`
breaks someone else's, a separate, deliberate act. Entering `doing` claims
a free lease automatically, and leaving it gives yours back: a ticket is
locked while it's being worked on. A lease is advisory, not a guarantee:
it only stops local tools from writing, and a concurrent edit on another
clone still surfaces as a normal git merge conflict.

`amd todo`/`doing`/`done`/`backlog`/`abandon <ref>` move a ticket there,
one column at a time — a ticket can't jump from backlog/ to done/; leave
off <ref> and it lists that destination instead of moving anything.
The same works for any epic by name: `amd checkout 7` files ticket 7 under
the checkout epic, `amd checkout` lists what's in it. Entering `todo` is the
one with a side effect: it creates or switches to the ticket's branch, the
same way `amd start` used to. Abandoning a ticket deletes that branch too,
if it's still empty — nothing lost, nothing left behind.

`amd init <project>` creates a private `<project>-tickets` GitHub repository
(under the GitHub organisation or personal account named <project> — pass
--org to use a different owner), clones it as a child of the current
directory, scaffolds the board inside it, and registers it. Must be run
from a plain directory, never from inside an existing git repository —
`amd init <project> --local-only` scaffolds the board in the repository
you're standing in instead, with no network and nothing created remotely,
the project name still recorded, just without a dedicated tickets repo.
Either way, init records you — git's user.name and user.email — as the
project's first user. Running it again on an existing project adds you if
you're missing, so a project made before that gets its first user too.
<project> cannot be changed once set — every ticket records which
repository it's about using this name, and changing it later means finding
and updating every ticket by hand, on every clone, with no way to do it
atomically. Choose the name you'd want a stranger to see in a year.

`amd join <project>` is how everyone else gets the project: run from a
plain directory, it clones `<project>-tickets` and `<project>-documents`
side by side over plain git (your SSH key is all it needs, no gh token;
--org as for init, AMD_GIT_BASE to clone from somewhere other than
git@github.com:), registers both, and adds you — git's user.name and
user.email — to the project's `users`, pushed straight to the tickets
repository. It refuses if either directory is already there. Unlike
`amd register`, which records a repository already on this machine, join
is the one-off step that puts the project here in the first place.

A project's repositories that aren't on this machine show as (not cloned)
and can't be chosen for a ticket. `amd clone` lists them; `amd clone <name>`
(or `--all`) clones them beside the tickets repository, from wherever the
tickets repository came from (its origin, with the name swapped in), and
registers them.

Several projects can share one folder (agile-md-tickets beside
multi-user-test-tickets). `amd project` lists them and marks the one amd
uses; `amd project use <name>` switches, by writing .current-project in
that folder (this machine only, never committed). From the folder itself,
or a code repository beside the projects, amd follows that file; inside a
tickets repository, that repository's own board always wins. With several
projects and no choice made, amd says so rather than guessing. `amd init`
and `amd join` never switch you: adding a second project keeps the first
one current.

A board with a remote stays in step with it. Before a change, amd fetches
and fast-forwards your clone, so you never number a ticket or take a lease
from a stale copy; a listing does the same at most every 10 seconds. If
someone pushes in the moment before you do, amd replays your change on top
of theirs when it can, and otherwise undoes it, saves what you wrote, and
exits non-zero so you can run it again on the up-to-date board. No network
only warns. AMD_SYNC=0 (or `sync = off` in .settings) turns this off.

Environment:
  AMD_DIR           board directory name under the repository root (default: tasks)
  AMD_BRANCH_TYPES  comma-separated branch types (default: feature, bugfix,
                    hotfix, release, chore)
  AMD_YES           set to 1 to create a missing board without prompting
  AMD_NO_INPUT      set to 1 to never prompt (same as --no-input)
  AMD_NO_BRANCH     set to 1 to never create branches (same as --no-branch)
  NO_COLOR          set to any value for plain output (same as --plain)
  EDITOR            editor used by `amd edit` (default: vi)
  AMD_SYNC          set to 0 to stop catching up with the remote (default: on)
  AMD_TICKET_NUMBERING  local (default) or cloud — cloud needs an agile-md
                    plan, which isn't available yet

AMD_BRANCH_TYPES and EDITOR can also be set in the settings file
(`branch-types`, `editor`) — the environment variable wins when both are set.

Creating a task interactively ends in $EDITOR with the rendered ticket, so
the notes and checklist get filled in there and then (--no-edit to skip).

Tasks are rendered from MiniJinja templates. `amd templates` lists them and
`amd templates eject task` writes an editable copy into <board>/templates/.
A template that uses `extra.<name>` gets asked for it in the form.";

#[derive(Parser, Debug)]
#[command(
    name = "amd",
    version,
    about = "A tiny filesystem Kanban: markdown tasks in todo/, doing/ and done/",
    after_help = AFTER_HELP,
    disable_help_subcommand = false
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Never prompt; missing values are an error instead
    #[arg(long, global = true)]
    no_input: bool,
    /// Plain text output instead of the rich board
    #[arg(long, global = true)]
    plain: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Create or connect this project's dedicated tickets and documents
    /// repositories ({project}-tickets, {project}-documents), recording you
    /// as one of its users
    Init {
        /// The project's declared identity — cannot be changed once set
        #[arg(value_name = "PROJECT")]
        project: String,
        /// Scaffold the board in this repository instead of a dedicated
        /// tickets repository — no network, nothing created remotely
        #[arg(long)]
        local_only: bool,
        /// GitHub org/user to create the tickets repository under
        /// (default: the project name itself)
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
    },
    /// Join an existing project: clone its tickets and documents
    /// repositories here over plain git and add you to its users
    Join {
        /// The project to join, as `amd init` named it
        #[arg(value_name = "PROJECT")]
        project: String,
        /// GitHub org/user the repositories live under
        /// (default: the project name itself)
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
    },
    /// List the projects in this folder and which one amd uses, or switch
    /// with `amd project use <name>`
    Project {
        #[command(subcommand)]
        action: Option<ProjectAction>,
    },
    /// Create a task in backlog/ from a template
    New(NewArgs),
    /// Show all columns (the default command)
    Board {
        /// Only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
    },
    /// List every ticket carrying a tag
    Tag {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// The project's tag list: with no action, every listed tag and how many
    /// tickets carry it. Tags are always UPPERCASE
    Tags {
        #[command(subcommand)]
        action: Option<TagsAction>,
    },
    /// Import tickets or documents. `amd import tickets <PATH>`: a tasks
    /// tree, status kept, ids renumbered, links remapped within the batch,
    /// same-titled tickets flagged DUPLICATE (a bare `amd import <PATH>` is
    /// the same). `amd import documents [PATH]`: a tree of documents (here,
    /// by default) into the project's documents repository, on a new
    /// branch with a pull request
    Import {
        /// `tickets` or `documents`; or, on its own, a tasks tree's path
        #[arg(value_name = "TYPE|PATH")]
        first: String,
        /// The tree to import: for tickets, the directory holding
        /// backlog/todo/doing/done; for documents, any folder (default: the
        /// current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Preview what would happen without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Documents: put the tree under this folder of the documents
        /// repository (default: its root)
        #[arg(long, value_name = "DIR")]
        into: Option<String>,
        /// Documents: the project to import into, when you aren't inside it
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
    },
    /// Move a task into backlog/, or list it with no ref
    Backlog {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
    },
    /// Move a task into todo/, or list it with no ref — entering todo/
    /// creates or switches to the ticket's branch
    Todo {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// Branch to create instead of the one in the ticket
        #[arg(short, long, value_name = "NAME")]
        branch: Option<String>,
        /// Move the task without touching branches
        #[arg(long)]
        no_branch: bool,
        /// With no ref, only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
        /// Move it even if todo/ is already at its WIP limit
        #[arg(long)]
        force: bool,
    },
    /// Move a task into doing/, or list it with no ref
    Doing {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
        /// Move it even if doing/ is already at its WIP limit
        #[arg(long)]
        force: bool,
    },
    /// Move a task into done/, or list it with no ref
    Done {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
    },
    /// Take a ticket off the board into archive/, or list the archive with
    /// no ref
    Abandon {
        #[arg(value_name = "REF")]
        task: Option<String>,
        /// With no ref, only show tasks carrying this tag
        #[arg(short = 't', long = "tag", value_name = "TAG")]
        tag: Option<String>,
    },
    /// Fix tickets that share a number: one keeps it (the first committed),
    /// the others get new numbers. Shows the plan; --apply carries it out
    Renumber {
        /// Carry out the plan, in one commit
        #[arg(long)]
        apply: bool,
        /// The ticket to keep its number (its name, or a unique part of it)
        #[arg(long, value_name = "TICKET")]
        keep: Option<String>,
    },
    /// Bring back tickets that were deleted or abandoned. With no number,
    /// list them: who removed each and when. Restored tickets go to the
    /// backlog exactly as they were, in one commit
    Recover {
        /// Ticket numbers to restore
        #[arg(value_name = "ID")]
        ids: Vec<String>,
        /// Restore every listed ticket (narrowed by --by and --since)
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// Only tickets removed by this person (name or email, any part)
        #[arg(long, value_name = "WHO")]
        by: Option<String>,
        /// Only tickets removed on or after this date (YYYY-MM-DD)
        #[arg(long, value_name = "DATE")]
        since: Option<String>,
    },
    /// Print a task
    Show {
        #[arg(value_name = "REF")]
        task: Option<String>,
    },
    /// Open a task in $EDITOR
    Edit {
        #[arg(value_name = "REF")]
        task: Option<String>,
    },
    /// Assign a ticket: no name picks from the project's users in a
    /// terminal (and unassigns elsewhere)
    Assign {
        #[arg(value_name = "REF")]
        task: String,
        /// Who it belongs to; `@me` is your git user name, empty unassigns
        #[arg(value_name = "WHO", conflicts_with = "clear")]
        who: Option<String>,
        /// Unassign it
        #[arg(long)]
        clear: bool,
    },
    /// Claim a ticket's lease so nobody else's writes land on it
    Lock {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Make a ticket a draft: it's tagged DRAFT and stays in the backlog
    /// (and out of sprints) until `amd ready` takes it out of draft
    Draft {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Take a ticket out of draft, so it can leave the backlog
    Ready {
        #[arg(value_name = "REF")]
        task: String,
    },
    /// Release a ticket's lease
    Unlock {
        #[arg(value_name = "REF")]
        task: String,
        /// Break someone else's lease instead of only releasing your own
        #[arg(long)]
        force: bool,
    },
    /// Set a field on a ticket: points, epic, order, title or priority
    Set {
        #[arg(value_name = "REF")]
        task: String,
        #[arg(
            value_name = "FIELD",
            value_parser = PossibleValuesParser::new(["points", "epic", "order", "title", "priority", "repository"]),
        )]
        field: String,
        /// The new value; empty clears the field
        #[arg(value_name = "VALUE", default_value = "")]
        value: String,
    },
    /// Epics and sprints: the folders the backlog is divided into
    Group {
        #[command(subcommand)]
        command: Option<GroupCmd>,
    },
    /// Set doing/'s WIP limit. 0 turns it off; an absent or unparsable
    /// setting defaults to 2
    WipLimit {
        #[arg(value_name = "N")]
        limit: u32,
    },
    /// The documents repository every project shares, shown in amdui's
    /// Documents beside the project's own. No name says which it is
    SharedDocuments {
        /// The repository's name, as cloned beside the project's
        #[arg(value_name = "REPOSITORY")]
        name: Option<String>,
        /// Name none
        #[arg(long, conflicts_with = "name")]
        clear: bool,
    },
    /// Register a repository — declares its name for the whole project
    /// (shared, git-tracked) and its path on this machine (local only)
    Register {
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// The project to declare it on, when the folder holds several
        /// (default: the board you're in, or the folder's current project
        /// — asked to confirm, since that's a team-wide change)
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
        /// Take the named repository off the project's declared list, for
        /// everyone (and off this machine's registry); same as `amd deregister`
        #[arg(long, value_name = "NAME", conflicts_with = "path")]
        remove: Option<String>,
    },
    /// Clone a repository the project declares but this machine hasn't got,
    /// beside the tickets repository, and register it. No name lists them
    Clone {
        #[arg(value_name = "NAME", conflicts_with = "all")]
        name: Option<String>,
        /// Clone every declared repository that's missing here
        #[arg(long)]
        all: bool,
    },
    /// Take a repository off the project for everyone: off its declared list
    /// (committed and pushed), so it leaves the repository lists unless a
    /// ticket names it, and off this machine. One that's only registered here
    /// just loses that. The clone itself is left alone
    Deregister {
        #[arg(value_name = "NAME")]
        name: String,
        /// The project to take it off, when the folder holds several
        /// (default: the board you're in, or the folder's current project
        /// — asked to confirm, since that's a team-wide change)
        #[arg(long, value_name = "NAME")]
        project: Option<String>,
    },
    /// Link tickets together: both ends get the other in `related`
    Link {
        #[arg(value_name = "REF")]
        task: String,
        #[arg(value_name = "REF", required = true)]
        to: Vec<String>,
        /// Only record the link on the first ticket
        #[arg(long)]
        one_way: bool,
    },
    /// Open the desktop board across every registered repository (same window
    /// as the `amdui` command)
    Gui,
    /// Your work: todo and doing tickets assigned to you, across every
    /// project on this machine, highest priority first. Read as this
    /// machine last saw each board
    Mine {
        /// Include projects you've hidden
        #[arg(long)]
        all: bool,
        /// Order by priority (then oldest), age, or points
        #[arg(long, value_name = "ORDER", default_value = "priority",
              value_parser = PossibleValuesParser::new(["priority", "age", "points"]))]
        sort: String,
        /// Print the list as JSON
        #[arg(long)]
        json: bool,
        /// Leave a project out of your work (by name), on this machine only
        #[arg(long, value_name = "PROJECT", conflicts_with = "unhide")]
        hide: Option<String>,
        /// Put a hidden project back
        #[arg(long, value_name = "PROJECT")]
        unhide: Option<String>,
    },
    /// Print a shell completion script (bash, zsh, fish, …)
    Completions {
        /// Shell to generate for; defaults to $SHELL
        #[arg(value_name = "SHELL")]
        shell: Option<Shell>,
    },
    /// Inspect and customise the task templates
    Templates {
        #[command(subcommand)]
        command: Option<TemplateCmd>,
    },
    /// Show board transitions from git history: date, time, action, id,
    /// title, sha. Reads paths and rename status only, never the commit
    /// message, so it works over history from before this existed too
    Log,
    /// Move a task into an epic, or list it with no ref — checked against
    /// the board's current epics (the four columns and `abandon` are their
    /// own named commands above, and always win if a name collides)
    #[command(external_subcommand)]
    Verb(Vec<String>),
}

#[derive(Args, Debug)]
struct NewArgs {
    /// Task title; omit it in a terminal to fill in the form
    #[arg(value_name = "TITLE")]
    title: Option<String>,
    /// Branch type: feature, bugfix, hotfix, release, chore. Leave it out for
    /// a ticket with no branch
    #[arg(
        long = "branch-type",
        alias = "type",
        value_name = "TYPE",
        value_parser = value::BranchType,
    )]
    branch_type: Option<String>,
    /// The ticket this one sits under (an epic, a story, anything)
    #[arg(short = 'p', long, value_name = "REF")]
    parent: Option<String>,
    /// Who it's assigned to (nobody by default; `@me` is you). Tickets can be
    /// created now and assigned later
    #[arg(short = 'a', long, alias = "owner", value_name = "WHO")]
    assignee: Option<String>,
    /// The repository the work is in — one cloned on this machine. Its
    /// branch is made there on entering todo
    #[arg(short = 'R', long, value_name = "REPO")]
    repository: Option<String>,
    /// Another ticket this one depends on or relates to (repeatable)
    #[arg(short = 'r', long = "related", value_name = "REF")]
    related: Vec<String>,
    /// Tag to add (repeatable)
    #[arg(short = 't', long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    /// Create it as a draft: tagged DRAFT, it stays in the backlog until
    /// `amd ready` takes it out of draft
    #[arg(long)]
    draft: bool,
    /// Template to render the ticket from (default: ticket). A board can add
    /// its own under <board>/templates/
    #[arg(
        short = 'T',
        long = "template",
        alias = "ticket",
        default_value = DEFAULT_TEMPLATE,
        value_name = "TYPE",
        value_parser = value::TicketType,
    )]
    template: String,
    /// Ask for every field, even the ones given as arguments
    #[arg(short, long)]
    interactive: bool,
    /// Extra template variable, available as extra.KEY (repeatable)
    #[arg(short = 's', long = "set", value_name = "KEY=VALUE", value_parser = parse_key_value)]
    set: Vec<(String, String)>,
    /// Open the ticket body in $EDITOR, even when the title came from the CLI
    #[arg(short, long)]
    edit: bool,
    /// Create the ticket from the template without opening an editor
    #[arg(long, conflicts_with = "edit")]
    no_edit: bool,
}

#[derive(Subcommand, Debug)]
enum GroupCmd {
    /// List the epics and sprints in this backlog
    List,
    /// Create an epic
    Epic {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "TEXT", default_value = "")]
        description: String,
    },
    /// Create a sprint
    Sprint {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "TEXT", default_value = "")]
        description: String,
        /// How long it runs
        #[arg(long, value_name = "DAYS", default_value_t = agile_md::group::DEFAULT_DAYS)]
        days: u32,
    },
    /// Start a sprint. There is no way back: a started sprint takes nothing
    /// more and gives nothing back — its tickets only move on, to todo
    Start {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Empty an epic or sprint's folder into the loose backlog at once —
    /// every ticket it holds, no precondition
    Empty {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Delete an epic or sprint outright. Refuses if it still has any
    /// tickets, in any column — `group empty` it first
    Delete {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Rename an epic or sprint: its folder, and every ticket filed in it
    Rename {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(value_name = "NEW_NAME")]
        to: String,
    },
}

#[derive(Subcommand, Debug)]
enum TemplateCmd {
    /// List the available templates (the default)
    List,
    /// Print a template's source
    Show {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Copy a built-in template into <board>/templates/ so you can edit it
    Eject {
        #[arg(default_value = DEFAULT_TEMPLATE, value_name = "NAME")]
        name: String,
        /// Overwrite an existing board template
        #[arg(short, long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    // Behave like a normal Unix tool when the reader goes away — `amd board |
    // head` is an ordinary thing to do. Rust ignores SIGPIPE by default, which
    // turns a closed pipe into a panic on the next print rather than a quiet
    // exit.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("amd: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Subcommand, Debug)]
enum ProjectAction {
    /// Make a project current for this folder (writes .current-project);
    /// with no name, pick one in a terminal
    Use {
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    form::set_no_input(cli.no_input);
    render::set_plain(cli.plain);
    let command = cli.command.unwrap_or(Cmd::Board { tag: None });

    // Completions don't need a board — and often run before there is one.
    if let Cmd::Completions { shell } = command {
        return completions::print(shell, &mut Cli::command());
    }

    // The desktop board draws every registered repository, so it must not
    // insist on standing in one — `amd gui` works from anywhere. The `amdui`
    // binary is the same call without the subcommand.
    if let Cmd::Gui = command {
        return cmd_gui();
    }

    // `init` is the one command that runs without an existing board.
    if let Cmd::Init {
        project,
        local_only,
        org,
    } = command
    {
        return init::run(project, local_only, org);
    }
    if let Cmd::Join { project, org } = command {
        return join::run(project, org);
    }
    // `register` declares a repository for the whole team, so which project
    // it lands on is checked before any board is chosen (ticket 103).
    if let Cmd::Register {
        path,
        project,
        remove,
    } = command
    {
        let board = register_board(path.as_deref(), project.as_deref(), remove.as_deref())?;
        agile_md::project_trust::check(&board)?;
        migrate_settings(&board)?;
        return match remove {
            Some(name) => cmd_unregister(&board, &name),
            None => cmd_register(&board, path.as_deref()),
        };
    }
    // `deregister` can take a repository off the project for everyone, so it
    // gets the same check of which project (ticket 120).
    if let Cmd::Deregister { name, project } = command {
        let board = register_board(None, project.as_deref(), Some(&name))?;
        agile_md::project_trust::check(&board)?;
        migrate_settings(&board)?;
        return cmd_deregister(&board, &name);
    }
    if let Cmd::Mine {
        all,
        sort,
        json,
        hide,
        unhide,
    } = &command
    {
        return cmd_mine(*all, sort, *json, hide.as_deref(), unhide.as_deref());
    }
    // Documents go into the project's documents repository, from wherever
    // they are: no board needed here, and none offered (ticket 079).
    if let Cmd::Import {
        first,
        path,
        dry_run,
        into,
        project,
    } = &command
        && first == "documents"
    {
        return cmd_import_documents(
            path.as_deref(),
            *dry_run,
            into.as_deref(),
            project.as_deref(),
        );
    }
    if let Cmd::Project { action } = command {
        return match action {
            None => project::list(),
            Some(ProjectAction::Use { name }) => project::use_project(name),
        };
    }

    let board = ensure_board()?;
    // Notices if this board's declared project identity has silently
    // changed since amd last saw it — see project_trust.rs.
    agile_md::project_trust::check(&board)?;
    // In step with the remote before anything else (ticket 102): a change
    // never lands on a stale clone, and a listing shows teammates' work.
    for event in agile_md::sync::catch_up(&board, sync_purpose(&command))? {
        match event {
            transition::Event::Info(text) => eprintln!("{text}"),
            transition::Event::Warn(text) => eprintln!("amd: {text}"),
        }
    }
    migrate_settings(&board)?;
    match command {
        Cmd::Init { .. }
        | Cmd::Join { .. }
        | Cmd::Project { .. }
        | Cmd::Mine { .. }
        | Cmd::Completions { .. }
        | Cmd::Gui => {
            unreachable!("handled above")
        }
        Cmd::WipLimit { limit } => cmd_wip_limit(&board, limit),
        Cmd::SharedDocuments { name, clear } => cmd_shared_documents(&board, name, clear),
        Cmd::Register { .. } | Cmd::Deregister { .. } => unreachable!("handled above"),
        Cmd::Clone { name, all } => cmd_clone(&board, name.as_deref(), all),
        Cmd::New(args) => cmd_new(&board, args),
        Cmd::Board { tag } => render::columns(&board, &Column::ALL, tag.as_deref()),
        Cmd::Tag { tag } => render::named(&format!("tag: {tag}"), board.tasks_tagged(&tag)?, None),
        Cmd::Tags { action } => cmd_tags(&board, action),
        Cmd::Import {
            first,
            path,
            dry_run,
            into,
            project,
        } => {
            if into.is_some() || project.is_some() {
                bail!("--into and --project are for `amd import documents`");
            }
            let path = match (first.as_str(), path) {
                ("tickets", Some(path)) => path,
                ("tickets", None) => bail!("usage: amd import tickets <PATH>"),
                (_, None) => PathBuf::from(first),
                (_, Some(_)) => {
                    bail!("amd import takes `tickets <PATH>` or `documents [PATH]`, not {first:?}")
                }
            };
            cmd_import(&board, &path, dry_run)
        }
        Cmd::Backlog { task, tag } => cmd_destination(
            &board,
            Destination::Column(Column::Backlog),
            task,
            tag.as_deref(),
            false,
        ),
        Cmd::Todo {
            task,
            branch,
            no_branch,
            tag,
            force,
        } => match task {
            Some(reference) => {
                let task = board.find(&reference)?;
                cmd_enter_todo(&board, &task, branch, no_branch, force)
            }
            None => render::columns(&board, &[Column::Todo], tag.as_deref()),
        },
        Cmd::Doing { task, tag, force } => cmd_destination(
            &board,
            Destination::Column(Column::Doing),
            task,
            tag.as_deref(),
            force,
        ),
        Cmd::Done { task, tag } => cmd_destination(
            &board,
            Destination::Column(Column::Done),
            task,
            tag.as_deref(),
            false,
        ),
        Cmd::Abandon { task, tag } => {
            cmd_destination(&board, Destination::Abandon, task, tag.as_deref(), false)
        }
        Cmd::Verb(args) => {
            let mut args = args.into_iter();
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: amd <destination> [ref] [--tag TAG]"))?;
            let mut task = None;
            let mut tag = None;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--tag" | "-t" => {
                        let value = args
                            .next()
                            .ok_or_else(|| anyhow::anyhow!("amd {name}: --tag needs a value"))?;
                        tag = Some(value);
                    }
                    _ if task.is_none() => task = Some(arg),
                    other => bail!("amd {name}: unexpected argument '{other}'"),
                }
            }
            let destination = resolve_destination(&board, &name)?;
            cmd_destination(&board, destination, task, tag.as_deref(), false)
        }
        Cmd::Show { task } => {
            let task = resolve(&board, task, "show", &Column::ALL)?;
            let body =
                fs::read(&task.path).with_context(|| format!("reading {}", task.path.display()))?;
            io::stdout().write_all(&body)?;
            if let Some(line) = lock_note(&board, &task) {
                eprintln!("{line}");
            }
            // Who to ask about it (ticket 155), on stderr like the lock note.
            if let Some(line) = agile_md::authorship::summary(&board, &task) {
                eprintln!("{line}");
            }
            Ok(())
        }
        Cmd::Edit { task } => {
            let task = resolve(&board, task, "edit", &Column::ALL)?;
            let (id_display, title) = (task.id_display(), task.title());
            // No separate "did it actually change" check needed: the
            // snapshot diff in with_auto_commit is already empty when the
            // editor exits without saving, so nothing gets committed.
            with_auto_commit(&board, "edit", &id_display, &title, || {
                open_editor(&task.path)
            })
        }
        Cmd::Set { task, field, value } => {
            let task = resolve(&board, Some(task), "set", &Column::ALL)?;
            let (id_display, title) = (task.id_display(), task.title());
            with_auto_commit(&board, "set", &id_display, &title, || {
                match field.as_str() {
                    "points" => {
                        task.set_points(task.hash()?, &value)?;
                        println!("{} sized {}", task.id_display(), display(&value));
                    }
                    // Filing goes through the board, not the ticket: it moves the
                    // file, and the sprint rules live there.
                    "epic" => {
                        let epic = (!value.is_empty()).then_some(value.as_str());
                        board.set_epic(&task, epic)?;
                        println!("{} filed under {}", task.id_display(), display(&value));
                    }
                    "order" => {
                        let rank: f64 = value
                            .parse()
                            .with_context(|| format!("{value} is not a number"))?;
                        task.set_order(task.hash()?, rank)?;
                        println!("{} ranked {rank}", task.id_display());
                    }
                    "title" => {
                        task.set_title(task.hash()?, &value)?;
                        println!("{} retitled", task.id_display());
                    }
                    "priority" => {
                        task.set_priority(task.hash()?, &value)?;
                        println!("{} priority {value}", task.id_display());
                    }
                    "repository" => {
                        task.set_repository(task.hash()?, &value)?;
                        let name = agile_md::task::repo_name(&value);
                        if !name.is_empty() && !board_settings::load(&board).repos().contains(&name)
                        {
                            eprintln!(
                                "amd: {name} isn't declared for this project — `amd register` it there so the team sees it"
                            );
                        }
                        println!("{} repository {}", task.id_display(), display(&name));
                    }
                    other => bail!("unknown field {other}"),
                }
                Ok(())
            })
        }
        Cmd::Group { command } => cmd_group(&board, command.unwrap_or(GroupCmd::List)),
        Cmd::Assign { task, who, clear } => {
            let task = resolve(&board, Some(task), "assign", &Column::ALL)?;
            let (id_display, title) = (task.id_display(), task.title());
            // No name: the picker in a terminal (ticket 089); anywhere else
            // it unassigns, as it always has, so scripts are unchanged.
            let who = match who {
                None if !clear && form::available() => form::assignee(
                    board_settings::load(&board).user_names(),
                    git::config(&board.root, "user.name"),
                    &task.assignee().unwrap_or_default(),
                )?,
                who => resolve_assignee(&board.root, who.as_deref()),
            };
            with_auto_commit(&board, "assign", &id_display, &title, || {
                task.assign(task.hash()?, &who)?;
                match who.is_empty() {
                    true => println!("{} unassigned", task.id_display()),
                    false => println!("{} assigned to {who}", task.id_display()),
                }
                Ok(())
            })?;
            if let Some(warning) = board_settings::unknown_assignee(&board, &who) {
                eprintln!("warning: {warning}");
            }
            Ok(())
        }
        Cmd::Lock { task } => cmd_lock(&board, &task),
        Cmd::Draft { task } => cmd_draft(&board, &task, true),
        Cmd::Ready { task } => cmd_draft(&board, &task, false),
        Cmd::Unlock { task, force } => cmd_unlock(&board, &task, force),
        Cmd::Link { task, to, one_way } => cmd_link(&board, &task, &to, one_way),
        Cmd::Templates { command } => cmd_templates(&board, command.unwrap_or(TemplateCmd::List)),
        Cmd::Log => cmd_log(&board),
        Cmd::Recover {
            ids,
            all,
            by,
            since,
        } => cmd_recover(&board, &ids, all, by.as_deref(), since.as_deref()),
        Cmd::Renumber { apply, keep } => cmd_renumber(&board, apply, keep.as_deref()),
    }
}

/// `amd renumber` (ticket 146): show, or with `--apply` fix, tickets that
/// share a number.
fn cmd_renumber(board: &Board, apply: bool, keep: Option<&str>) -> Result<()> {
    use agile_md::renumber;
    let clashes = renumber::plan(board, keep)?;
    if clashes.is_empty() {
        println!("no two tickets share a number");
        return Ok(());
    }
    for clash in &clashes {
        println!("{:03}: {} keeps it", clash.id, clash.keep.stem);
        for (task, to) in &clash.moves {
            println!("     {} becomes {to:03}", task.stem);
        }
    }
    if !apply {
        println!("(nothing changed; `amd renumber --apply` does this)");
        return Ok(());
    }
    let ids: Vec<String> = clashes.iter().map(|c| format!("{:03}", c.id)).collect();
    let report = with_auto_commit(board, "renumber", &ids.join(", "), "shared numbers", || {
        renumber::apply(board, &clashes)
    })?;
    for (from, to) in &report.renamed {
        println!("renamed {from} -> {to}");
    }
    for (ticket, what) in &report.updated {
        println!("updated {ticket}: {what}");
    }
    if !report.ambiguous.is_empty() {
        eprintln!("check these by hand: they could mean either ticket, so they were left alone");
        for (ticket, what) in &report.ambiguous {
            eprintln!("  {ticket}: {what}");
        }
    }
    Ok(())
}

/// `amd recover` (ticket 144): list tickets removed from the board, or
/// restore some into the backlog in one commit.
fn cmd_recover(
    board: &Board,
    ids: &[String],
    all: bool,
    by: Option<&str>,
    since: Option<&str>,
) -> Result<()> {
    use agile_md::recover;
    if let Some(date) = since
        && !(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-')
    {
        bail!("--since takes a date as YYYY-MM-DD, not '{date}'");
    }
    let listed: Vec<recover::Removed> = recover::removed(board)?
        .into_iter()
        .filter(|r| by.is_none_or(|who| r.by(who)))
        .filter(|r| since.is_none_or(|date| r.when.as_str() >= date))
        .collect();

    if ids.is_empty() && !all {
        if listed.is_empty() {
            println!("nothing to recover: no removed ticket is missing from the board");
        }
        for r in &listed {
            let how = if r.abandoned { "abandoned" } else { "deleted" };
            println!(
                "{}  {}  {how} by {}  {}",
                r.id_display(),
                r.when,
                r.author,
                r.title
            );
        }
        return Ok(());
    }

    let chosen: Vec<recover::Removed> = if all {
        listed
    } else {
        let mut chosen = Vec::new();
        for id in ids {
            let number: u32 = id
                .parse()
                .with_context(|| format!("'{id}' isn't a ticket number"))?;
            match listed.iter().find(|r| r.id == number) {
                Some(r) => chosen.push(r.clone()),
                None => bail!(
                    "{number:03} isn't a removed ticket missing from the board (`amd recover` lists them)"
                ),
            }
        }
        chosen
    };
    if chosen.is_empty() {
        println!("nothing to recover");
        return Ok(());
    }
    let ids: Vec<String> = chosen.iter().map(|r| r.id_display()).collect();
    let detail = if chosen.len() == 1 {
        chosen[0].title.clone()
    } else {
        format!("{} tickets", chosen.len())
    };
    with_auto_commit(board, "recover", &ids.join(", "), &detail, || {
        for r in &chosen {
            recover::restore(board, r)?;
            println!("recovered {} -> backlog/  {}", r.stem, r.title);
        }
        Ok(())
    })
}

/// `amd mine` (ticket 020): your todo and doing tickets across every project
/// on this machine. Works from anywhere: it isn't about one board.
fn cmd_mine(
    all: bool,
    sort: &str,
    json: bool,
    hide: Option<&str>,
    unhide: Option<&str>,
) -> Result<()> {
    use agile_md::my_work::{self, Sort};
    let projects = my_work::projects();
    if let Some(name) = hide.or(unhide) {
        let matches: Vec<_> = projects.iter().filter(|p| p.name == name).collect();
        let project = match matches.as_slice() {
            [one] => *one,
            [] => bail!(
                "no project called '{name}' on this machine (projects: {})",
                projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => bail!(
                "'{name}' is cloned in more than one folder; hide it from the desktop board instead"
            ),
        };
        my_work::set_hidden(&project.root, hide.is_some())?;
        let what = if hide.is_some() {
            "hidden from"
        } else {
            "back in"
        };
        println!("{} is {what} your work on this machine", project.name);
        return Ok(());
    }
    let here = env::current_dir().unwrap_or_default();
    let Some((email, name)) = my_work::me(&here) else {
        bail!("git has no user.email, so amd can't tell which tickets are yours");
    };
    let order = match sort {
        "age" => Sort::Age,
        "points" => Sort::Points,
        _ => Sort::Priority,
    };
    let items = my_work::my_work(&email, name.as_deref(), all, order);
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("nothing in todo or doing is assigned to you");
    }
    for item in &items {
        let points = if item.points.is_empty() {
            "      ".to_string()
        } else {
            format!("{:>2} pts", item.points)
        };
        println!(
            "{:<5}  {:<6}  {}  {:<16}  {}  {}",
            item.column, item.priority, points, item.project, item.id, item.title
        );
    }
    let hidden = projects.iter().filter(|p| p.hidden).count();
    if hidden > 0 && !all {
        eprintln!(
            "{hidden} hidden project{} left out; --all includes {}",
            if hidden == 1 { "" } else { "s" },
            if hidden == 1 { "it" } else { "them" }
        );
    }
    Ok(())
}

/// What `amd show` says about a ticket's lease, on stderr so the ticket on
/// stdout stays exactly the file: the same four cases as amdui's editor
/// (ticket 110). A lapsed lease is said as such, never as unlocked.
fn lock_note(board: &Board, task: &agile_md::task::Task) -> Option<String> {
    let by = task.locked_by()?;
    let until = task.locked_until()?.format(&Rfc3339).ok()?;
    let mine = git::config(board.repo_root(), "user.email").as_deref() == Some(by.as_str());
    Some(match (task.lease_active(), mine) {
        (true, true) => format!("locked by you until {until}"),
        (true, false) => format!("locked by {by} until {until}: only they can change it"),
        (false, true) => {
            format!(
                "your lock lapsed at {until} — `amd lock {}` to keep working",
                task.id_display()
            )
        }
        (false, false) => {
            format!("locked by {by} until {until} (lapsed): anyone can change it now")
        }
    })
}

/// Claim a ticket's lease for the current git identity. Refuses if someone
/// else's active lease is already there — the same `write_checked` gate
/// every other setter goes through, with no special casing needed for take.
fn cmd_lock(board: &Board, reference: &str) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    with_auto_commit(board, "lock", &id_display, &title, || {
        let hours = board_settings::load(board).lease_hours();
        task.take_lease(task.hash()?, hours)?;
        let until = task
            .locked_until()
            .and_then(|until| until.format(&Rfc3339).ok())
            .unwrap_or_default();
        println!("{} locked until {until}", task.id_display());
        Ok(())
    })
}

/// `amd draft` / `amd ready` (ticket 114): add or remove the DRAFT tag. A
/// ticket that's already so says so and changes nothing.
fn cmd_draft(board: &Board, reference: &str, draft: bool) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    let verb = if draft { "draft" } else { "ready" };
    with_auto_commit(board, verb, &id_display, &title, || {
        let changed = task.set_draft(task.hash()?, draft)?;
        let id = task.id_display();
        match (changed, draft) {
            (true, true) => {
                println!("{id} is a draft: it stays in the backlog until `amd ready {id}`")
            }
            (true, false) => println!("{id} is ready: it can leave the backlog"),
            (false, true) => println!("{id} is already a draft"),
            (false, false) => println!("{id} isn't a draft"),
        }
        Ok(())
    })
}

/// Release a ticket's lease — only your own unless `--force` says to break
/// someone else's, a deliberate, separate act rather than a side effect of
/// an ordinary release.
fn cmd_unlock(board: &Board, reference: &str, force: bool) -> Result<()> {
    let task = board.find(reference)?;
    let (id_display, title) = (task.id_display(), task.title());
    with_auto_commit(board, "unlock", &id_display, &title, || {
        let hash = task.hash()?;
        if force {
            task.break_lease(hash)?;
            println!("{} lease broken", task.id_display());
        } else {
            task.release_lease(hash)?;
            println!("{} unlocked", task.id_display());
        }
        Ok(())
    })
}

/// Move a task into todo/ and put the working tree on its branch. The work
/// lives in `agile_md::transition` so every front end does exactly the
/// same; this only prints what it reports.
fn cmd_enter_todo(
    board: &Board,
    task: &task::Task,
    override_branch: Option<String>,
    no_branch: bool,
    force: bool,
) -> Result<()> {
    let outcome = transition::enter_todo(
        board,
        task,
        &transition::TodoOptions {
            force,
            branch: override_branch,
            no_branch,
        },
    )?;
    report(&outcome);
    Ok(())
}

/// Print a transition's events the way the CLI always has: information on
/// stdout, best-effort failures on stderr behind the `amd:` prefix.
fn report(outcome: &transition::Outcome) {
    for event in &outcome.events {
        match event {
            transition::Event::Info(text) => println!("{text}"),
            transition::Event::Warn(text) => eprintln!("amd: {text}"),
        }
    }
}

/// Link tickets together. The relation is symmetric by default — "related"
/// reads the same from either end — so both files are updated unless the
/// caller asks for one direction only.
fn cmd_link(board: &Board, reference: &str, to: &[String], one_way: bool) -> Result<()> {
    let task = board.find(reference)?;
    let id = task.id_display();
    let mut others = Vec::new();
    for other in to {
        let other = board.find(other)?;
        if other.path == task.path {
            bail!("a task can't be related to itself");
        }
        others.push(other);
    }

    let ids: Vec<String> = others.iter().map(|other| other.id_display()).collect();
    // Both ends of the relation are one logical change, so one commit
    // (ticket 054), like `amd group empty`.
    // The message reads `link: 001 -> 002, 003 "<001's title>"`.
    let subject = format!("{id} -> {}", ids.join(", "));
    with_auto_commit(board, "link", &subject, &task.title(), || {
        if task.add_related(task.hash()?, &ids)? {
            println!("{} related: {}", id, ids.join(", "));
        } else {
            println!("{id} was already related to {}", ids.join(", "));
        }
        if one_way {
            return Ok(());
        }
        for other in &others {
            if other.add_related(other.hash()?, std::slice::from_ref(&id))? {
                println!("{} related: {id}", other.id_display());
            }
        }
        Ok(())
    })
}

/// Opens the desktop board: the `amdui` binary installed beside this one,
/// or else the first on `PATH`. The board is its own program (Tauri), so
/// `amd` carries no window stack (ticket 084).
fn cmd_gui() -> Result<()> {
    let beside = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("amdui")))
        .filter(|path| path.is_file());
    let program = beside.unwrap_or_else(|| PathBuf::from("amdui"));
    match Command::new(&program).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("amdui exited with {status}"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => bail!(
            "the desktop board isn't installed: amdui ships with amd — \
             run install.sh again, or build it from a clone (see the README)"
        ),
        Err(err) => Err(err).context("starting amdui"),
    }
}

/// Move a ticket and say so. The wording is the CLI's, not the library's —
/// `Board::move_task` returns where the ticket landed and leaves the reporting
/// to whoever asked for the move; `transition::move_to` does the move, the
/// lease and the auto-commit.
fn move_and_report(board: &Board, task: &task::Task, to: Column, force: bool) -> Result<()> {
    report(&transition::move_to(board, task, to, force)?);
    Ok(())
}

/// Runs `mutate`, then commits and pushes what it changed (see
/// `transition::with_auto_commit`), warning rather than failing if that
/// can't be done.
fn with_auto_commit<T>(
    board: &Board,
    verb: &str,
    subject: &str,
    detail: &str,
    mutate: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let (result, warning) = transition::with_auto_commit(board, verb, subject, detail, mutate)?;
    if let Some(warning) = warning {
        eprintln!("amd: {warning}");
    }
    Ok(result)
}

/// Where a destination verb can send a ticket: one of the four columns,
/// taking it off the board entirely, or filing it under an existing epic.
enum Destination {
    Column(Column),
    Abandon,
    Epic(String),
}

/// Check a destination verb against the board — a column, `abandon`, or the
/// name of an epic that already exists — and say exactly what's valid when
/// it's none of those, since that list is the whole point of the error.
fn resolve_destination(board: &Board, name: &str) -> Result<Destination> {
    if let Some(column) = Column::parse(name) {
        return Ok(Destination::Column(column));
    }
    if name.eq_ignore_ascii_case(ABANDON) {
        return Ok(Destination::Abandon);
    }
    let epics = board.epics_in(Column::Backlog)?;
    if epics.iter().any(|epic| epic == name) {
        return Ok(Destination::Epic(name.to_string()));
    }
    let columns: Vec<String> = Column::ALL.iter().map(|c| c.to_string()).collect();
    let epics_display = if epics.is_empty() {
        "none".to_string()
    } else {
        epics.join(", ")
    };
    bail!(
        "'{name}' is not a column, '{ABANDON}', or an epic on this board\n      (columns: {}; epics: {epics_display})",
        columns.join(", "),
    );
}

/// Move a ticket to a destination, or — with no ref — list what's there,
/// narrowed to `tag` when given. Every destination verb goes through this one
/// function, static columns and dynamic epic names alike, so they can't drift
/// from each other.
fn cmd_destination(
    board: &Board,
    destination: Destination,
    task: Option<String>,
    tag: Option<&str>,
    force: bool,
) -> Result<()> {
    match (destination, task) {
        (Destination::Column(column), Some(reference)) => {
            let task = board.find(&reference)?;
            move_and_report(board, &task, column, force)
        }
        (Destination::Column(column), None) => render::columns(board, &[column], tag),
        (Destination::Abandon, Some(reference)) => {
            let task = board.find(&reference)?;
            let (id_display, title) = (task.id_display(), task.title());
            with_auto_commit(board, "abandon", &id_display, &title, || {
                let name = task.file_name();
                let (_, deleted_branch) = board.archive(&task)?;
                println!("archived {name} -> archive/");
                if let Some(branch) = deleted_branch {
                    println!("deleted {branch} (empty)");
                }
                Ok(())
            })
        }
        (Destination::Abandon, None) => render::named(ABANDON, board.archived()?, tag),
        (Destination::Epic(name), Some(reference)) => {
            let task = board.find(&reference)?;
            let (id_display, title) = (task.id_display(), task.title());
            with_auto_commit(board, "set", &id_display, &title, || {
                board.set_epic(&task, Some(&name))?;
                println!("{} filed under {name}", task.id_display());
                Ok(())
            })
        }
        (Destination::Epic(name), None) => {
            let tasks = board.tasks_in_group(&name)?;
            render::named(&name, tasks, tag)
        }
    }
}

/// An empty value reads better as a word than as nothing at all.
fn display(value: &str) -> String {
    match value.is_empty() {
        true => "(cleared)".to_string(),
        false => value.to_string(),
    }
}

fn cmd_group(board: &Board, command: GroupCmd) -> Result<()> {
    use agile_md::group::{Group, Kind, State};
    match command {
        GroupCmd::List => {
            let groups = board.groups()?;
            if groups.is_empty() {
                println!("(no epics or sprints — add one with: amd group epic <name>)");
                return Ok(());
            }
            for group in groups {
                let tickets = board.tasks_in_group(&group.name)?;
                let points: i64 = tickets
                    .iter()
                    .filter_map(|task| task.points())
                    .filter_map(|p| p.trim().parse::<i64>().ok())
                    .sum();
                let detail = match group.is_sprint() {
                    true => format!("sprint  {}d  {}", group.days, group.state.as_str()),
                    false => "epic".to_string(),
                };
                println!(
                    "{:<24} {detail:<22} {} ticket(s), {points} point(s)",
                    group.name,
                    tickets.len()
                );
            }
            Ok(())
        }
        GroupCmd::Epic { name, description } => {
            with_auto_commit(board, "group-epic", &name, &description, || {
                let group = Group {
                    dir: board.dir(Column::Backlog).join(&name),
                    name: name.clone(),
                    kind: Kind::Epic,
                    description: description.clone(),
                    days: agile_md::group::DEFAULT_DAYS,
                    state: State::Pending,
                };
                board.create_group(&group)?;
                println!("created epic {name}");
                Ok(())
            })
        }
        GroupCmd::Sprint {
            name,
            description,
            days,
        } => with_auto_commit(board, "group-sprint", &name, &description, || {
            let group = Group {
                dir: board.dir(Column::Backlog).join(&name),
                name: name.clone(),
                kind: Kind::Sprint,
                description: description.clone(),
                days,
                state: State::Pending,
            };
            board.create_group(&group)?;
            println!("created sprint {name} ({days} days)");
            Ok(())
        }),
        GroupCmd::Start { name } => with_auto_commit(board, "group-start", &name, "", || {
            let mut group = board.group(&name)?;
            group.start()?;
            println!("{name} started — its tickets are now fixed");
            Ok(())
        }),
        GroupCmd::Empty { name } => with_auto_commit(board, "group-empty", &name, "", || {
            let result = board.empty_group(&name)?;
            println!(
                "{} emptied into backlog/ ({} ticket(s))",
                name,
                result.moved.len()
            );
            for (stem, err) in &result.skipped {
                eprintln!("amd: {stem} not moved: {err}");
            }
            Ok(())
        }),
        GroupCmd::Delete { name } => with_auto_commit(board, "group-delete", &name, "", || {
            board.delete_group(&name)?;
            println!("deleted {name}");
            Ok(())
        }),
        GroupCmd::Rename { name, to } => {
            with_auto_commit(board, "group-rename", &name, &to, || {
                board.rename_group(&name, &to)?;
                println!("renamed {name} -> {}", to.trim());
                Ok(())
            })
        }
    }
}

/// Sets doing/'s WIP limit — the only way to set an explicit value besides
/// `amdui`'s Settings modal or hand-editing `.settings`. `limit` is a `u32`,
/// so clap already refuses a non-numeric argument before this runs; `0` is
/// a valid value and means the limit is turned off.
fn cmd_wip_limit(board: &Board, limit: u32) -> Result<()> {
    // Committed and pushed like a ticket move, so it reaches the whole team
    // (ticket 111).
    with_auto_commit(board, "settings", "wip-limit", &limit.to_string(), || {
        let mut settings = board_settings::load(board);
        settings.set_wip_limit(Column::Doing, Some(limit))?;
        settings.save(board)
    })?;
    match limit {
        0 => println!("doing/'s WIP limit is now off"),
        n => println!("doing/'s WIP limit is now {n}"),
    }
    Ok(())
}

/// Names the documents repository every project shares (ticket 129), or
/// clears it, committed and pushed like any setting; with neither, says
/// which it is.
fn cmd_shared_documents(board: &Board, name: Option<String>, clear: bool) -> Result<()> {
    if name.is_none() && !clear {
        match board_settings::load(board).shared_documents() {
            Some(name) => println!("{name}"),
            None => eprintln!("no shared documents repository (amd shared-documents <name>)"),
        }
        return Ok(());
    }
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let what = name.clone().unwrap_or_else(|| "cleared".to_string());
    with_auto_commit(board, "settings", "shared-documents", &what, || {
        let mut settings = board_settings::load(board);
        settings.set_shared_documents(name.as_deref())?;
        settings.save(board)
    })?;
    match name {
        Some(name) => println!("shared documents: {name}"),
        None => println!("no shared documents repository"),
    }
    Ok(())
}

/// Carries settings still kept in the project's documents repository into
/// the tickets `.settings`, once, committed and pushed (ticket 111). Silent
/// once everything is across.
fn migrate_settings(board: &Board) -> Result<()> {
    let mut moved = Vec::new();
    with_auto_commit(board, "settings", "migrate", "from documents", || {
        moved = board_settings::migrate_from_documents(board)?;
        Ok(())
    })?;
    if !moved.is_empty() {
        let from = board_settings::documents_settings_dir(board)
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "the documents repository".to_string());
        eprintln!(
            "moved {} from {from} into the tickets .settings, where board settings live now — \
             {from} is no longer read; delete it with a documents PR",
            moved.join(", ")
        );
    }
    Ok(())
}

/// Registers a repository, both halves at once: declares its name for the
/// whole project (`board.settings`, shared, git-tracked — everyone sees it
/// listed on their next pull) and its path on this machine
/// (`registry.rs`, local, never shared). `path` defaults to the repository
/// you're standing in, same as the old `amd repos add`. Both halves are
/// idempotent, so registering something already declared/already local is
/// never an error.
fn cmd_register(board: &Board, path: Option<&str>) -> Result<()> {
    let path = match path {
        Some(path) => PathBuf::from(path),
        None => git::repo_root().ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?,
    };
    let canonical = path
        .canonicalize()
        .with_context(|| format!("{} doesn't exist", path.display()))?;
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("{} has no name", canonical.display()))?;

    // The declaration is the team's, so it's committed and pushed (ticket
    // 111); the registry below is this machine's alone.
    with_auto_commit(board, "register", &name, "", || {
        let mut settings = board_settings::load(board);
        settings.declare_repo(&name)?;
        settings.save(board)
    })?;

    let mut registry = Registry::load();
    let added = registry.add(&canonical)?;
    registry.save()?;
    init::warn_if_cloned_twice(&registry, &canonical);

    match added {
        true => println!("registered {name}"),
        false => println!("{name} was already registered on this machine"),
    }
    Ok(())
}

/// `amd tags` (ticket 122): the CLI side of Admin's Tags section.
#[derive(Subcommand, Debug)]
enum TagsAction {
    /// Add a tag to the project's list, in UPPERCASE
    Add {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// Take a tag off the list. Only an unused tag can go: one no ticket on
    /// any of the project's boards carries
    Remove {
        #[arg(value_name = "TAG")]
        tag: String,
    },
    /// Prevent arbitrary tags: allow only listed tags on tickets (`on`, the
    /// default), or any tag with a warning for unlisted ones (`off`)
    Only {
        #[arg(value_name = "on|off")]
        state: String,
    },
}

/// Lists the project's tags with how many tickets on its boards carry each,
/// or changes the list. Every change is committed like any other setting.
fn cmd_tags(board: &Board, action: Option<TagsAction>) -> Result<()> {
    let repos = agile_md::model::load_repos()?;
    let boards: Vec<&Board> = repos.repos.iter().map(|r| &r.board).collect();
    match action {
        None => {
            let settings = board_settings::load(board);
            let listed = settings.tags();
            let counts = agile_md::model::tag_counts(&boards)?;
            if listed.is_empty() {
                println!("no tag list for this project; `amd tags add <TAG>` starts one");
            }
            for tag in &listed {
                let n = counts.get(tag).copied().unwrap_or(0);
                println!("{tag}\t{n}{}", if n == 0 { "\tunused" } else { "" });
            }
            let unlisted: Vec<String> = counts
                .iter()
                .filter(|(tag, _)| !listed.contains(tag))
                .map(|(tag, n)| format!("{tag} ({n})"))
                .collect();
            if !listed.is_empty() && !unlisted.is_empty() {
                println!("in use but not listed: {}", unlisted.join(", "));
            }
            if !listed.is_empty() {
                let only = if settings.only_listed_tags() {
                    "on (arbitrary tags are prevented)"
                } else {
                    "off (arbitrary tags are allowed, with a warning)"
                };
                println!("only listed tags: {only}");
            }
            Ok(())
        }
        Some(TagsAction::Add { tag }) => {
            let mut added = String::new();
            with_auto_commit(board, "tags", "add", &task::normalize_tag(&tag), || {
                let mut settings = board_settings::load(board);
                added = settings.add_tag(&tag)?;
                settings.save(board)
            })?;
            println!("added {added}");
            Ok(())
        }
        Some(TagsAction::Remove { tag }) => {
            let mut removed = String::new();
            with_auto_commit(board, "tags", "remove", &task::normalize_tag(&tag), || {
                removed = agile_md::model::remove_unused_tag(board, &boards, &tag)?;
                Ok(())
            })?;
            println!("removed {removed}");
            Ok(())
        }
        Some(TagsAction::Only { state }) => {
            let on = match state.trim().to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                other => bail!("'{other}': say `on` or `off`"),
            };
            with_auto_commit(
                board,
                "tags",
                "only listed",
                if on { "on" } else { "off" },
                || {
                    let mut settings = board_settings::load(board);
                    settings.set_only_listed_tags(on);
                    settings.save(board)
                },
            )?;
            println!("only listed tags: {}", if on { "on" } else { "off" });
            Ok(())
        }
    }
}

/// `amd register --remove <name>`: takes a repository off the project's
/// declared list for everyone, committed and pushed, and off this machine's
/// registry if it's there. The clone itself is left alone.
fn cmd_unregister(board: &Board, name: &str) -> Result<()> {
    let mut removed = false;
    with_auto_commit(board, "unregister", name, "", || {
        let mut settings = board_settings::load(board);
        removed = settings.undeclare_repo(name);
        if removed {
            settings.save(board)?;
        }
        Ok(())
    })?;
    if !removed {
        bail!("{name} isn't declared for this project");
    }
    let mut registry = Registry::load();
    if registry.remove(name) {
        registry.save()?;
    }
    println!("{name} is no longer part of the project (its clone is untouched)");
    Ok(())
}

/// Which project's board `amd register` declares on (ticket 103). Normally
/// the board you're in, as for any command. But from a repository with no
/// board of its own, in a folder holding several projects, that board is
/// whichever `.current-project` names — easy to get wrong, and the result
/// is a team-wide declaration on the wrong project. So there it asks
/// first, in a terminal, and refuses without one, naming the fixes.
/// `--project` always says outright. `removing` is the repository being
/// taken off (`deregister`, `register --remove`), which the question names.
fn register_board(
    path: Option<&str>,
    project: Option<&str>,
    removing: Option<&str>,
) -> Result<Board> {
    let dir = agile_md::current_project::project_dir()?;
    let projects = agile_md::current_project::projects(&dir);
    if let Some(name) = project {
        let entry = projects.iter().find(|p| p.name == name).ok_or_else(|| {
            let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
            anyhow::anyhow!(
                "no project '{name}' in {} (one of: {})",
                dir.display(),
                names.join(", ")
            )
        })?;
        return Ok(Board::in_repo(&entry.tickets));
    }
    let own_board = Board::locate().is_ok_and(|board| board.root.is_dir());
    if own_board || projects.len() <= 1 {
        return ensure_board();
    }

    let board = ensure_board()?;
    let current = board_settings::load(&board)
        .project()
        .map(str::to_string)
        .unwrap_or_else(|| agile_md::current_project::project_name(board.repo_root()));
    let repo_name = match removing {
        Some(name) => name.to_string(),
        None => {
            let repo = match path {
                Some(path) => PathBuf::from(path),
                None => git::repo_root()
                    .ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?,
            };
            repo.canonicalize()
                .unwrap_or(repo)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    };
    let hint = projects
        .iter()
        .find(|p| p.name != current && repo_name.starts_with(&p.name))
        .map(|p| format!(" — did you mean --project {}?", p.name))
        .unwrap_or_default();
    let (asking, refused, would) = match removing {
        Some(_) => ("Take", "nothing deregistered", "taken off"),
        None => ("Register", "nothing registered", "declared on"),
    };
    if form::available() {
        let preposition = if removing.is_some() { "off" } else { "with" };
        let question = format!(
            "{asking} {repo_name} {preposition} project {current} (this folder's current project){hint}"
        );
        if form::confirm(&question)? {
            return Ok(board);
        }
        bail!("{refused}");
    }
    bail!(
        "{repo_name} would be {would} project '{current}', this folder's current project, \
         and this folder holds {} projects — pass --project <name>, run it from \
         <name>-tickets, or `amd project use <name>` first{hint}",
        projects.len()
    )
}

/// `amd clone [name | --all]` (ticket 108).
fn cmd_clone(board: &Board, name: Option<&str>, all: bool) -> Result<()> {
    let missing = agile_md::clone::missing(board);
    let names: Vec<String> = match (name, all) {
        (Some(name), _) => vec![name.to_string()],
        (None, true) => missing,
        (None, false) => {
            if missing.is_empty() {
                println!("every declared repository is cloned here");
            } else {
                println!("not cloned here (amd clone <name>, or --all):");
                for name in &missing {
                    println!("  {name}");
                }
            }
            return Ok(());
        }
    };
    if names.is_empty() {
        println!("every declared repository is cloned here");
        return Ok(());
    }
    let mut failed = Vec::new();
    for name in &names {
        match agile_md::clone::clone_repo(board, name) {
            Ok(cloned) => {
                println!(
                    "cloned {name} into {} (from {})",
                    cloned.path.display(),
                    cloned.url
                );
                for other in cloned.also_at {
                    eprintln!(
                        "warning: {name} is also cloned at {} — two copies of one repository \
                         drift apart; prefer one",
                        other.display()
                    );
                }
            }
            Err(err) if names.len() == 1 => return Err(err),
            Err(err) => {
                eprintln!("amd: {name}: {err:#}");
                failed.push(name.as_str());
            }
        }
    }
    if !failed.is_empty() {
        bail!("couldn't clone {}", failed.join(", "));
    }
    Ok(())
}

/// Removes only the *local* registration — the project-wide declaration is
/// untouched, so `name` goes back to showing greyed out in `amdui` rather
/// than disappearing for the rest of the team.
/// `amd deregister <name>` (ticket 120). A repository the project declares
/// comes off the project for everyone, exactly as `register --remove`: off the
/// declared list, committed and pushed, so it leaves every teammate's
/// repository filter and fields unless a ticket names it, and off this
/// machine's registry. One that's only registered here (a project's own
/// tickets or documents repository, say) just loses that registration. The
/// clone is never touched.
fn cmd_deregister(board: &Board, name: &str) -> Result<()> {
    if board_settings::load(board)
        .repos()
        .iter()
        .any(|r| r == name)
    {
        return cmd_unregister(board, name);
    }
    let mut registry = Registry::load();
    if !registry.remove(name) {
        bail!("'{name}' is neither declared for this project nor registered on this machine");
    }
    registry.save()?;
    println!("deregistered {name} on this machine");
    Ok(())
}

/// The new-ticket form's repository question: the project's declared
/// repositories that are cloned on this machine, or none. One you haven't got
/// can't be chosen (tickets 101/107), so it's named in the help instead.
fn pick_repository(board: &Board) -> Result<String> {
    let declared = board_settings::load(board).repos();
    if declared.is_empty() {
        return Ok(String::new());
    }
    let (here, missing): (Vec<String>, Vec<String>) = declared
        .into_iter()
        .partition(|name| agile_md::registry::clone_of(&board.root, name).is_some());
    if !missing.is_empty() {
        eprintln!(
            "not cloned on this machine, so not offered: {}",
            missing.join(", ")
        );
    }
    let none = "(none)".to_string();
    let mut options = vec![none.clone()];
    options.extend(here);
    let chosen = form::select("Repository:", options, &none)?;
    Ok(if chosen == none {
        String::new()
    } else {
        chosen
    })
}

/// `@me` is whoever git says you are — the name that ends up in the commits.
fn resolve_assignee(repo: &Path, who: Option<&str>) -> String {
    match who {
        Some("@me") => git::config(repo, "user.name").unwrap_or_else(|| "me".to_string()),
        Some(who) => who.trim().to_string(),
        None => String::new(),
    }
}

/// Whether `command` changes the board (catch up strictly first, refusing a
/// clone that can't be brought up to date) or only looks at it (catch up if
/// it's cheap, and warn rather than refuse). A destination verb or an epic
/// name with a ticket ref moves it; without one it lists.
fn sync_purpose(command: &Cmd) -> agile_md::sync::Purpose {
    use agile_md::sync::Purpose::{Lookup, Read, Write};
    let moves = |task: &Option<String>| if task.is_some() { Write } else { Read };
    match command {
        Cmd::Show { task: Some(_) } => Lookup,
        Cmd::Renumber { apply: false, .. } => Read,
        // Listing what was removed reads; restoring writes.
        Cmd::Recover { ids, all, .. } => {
            if ids.is_empty() && !all {
                Read
            } else {
                Write
            }
        }
        Cmd::Board { .. }
        | Cmd::Tag { .. }
        | Cmd::Tags { action: None }
        | Cmd::Show { .. }
        | Cmd::Log
        | Cmd::Templates { .. }
        | Cmd::Register { .. }
        | Cmd::Clone { .. }
        | Cmd::WipLimit { .. }
        | Cmd::SharedDocuments { .. }
        | Cmd::Group {
            command: None | Some(GroupCmd::List),
        } => Read,
        Cmd::Backlog { task, .. }
        | Cmd::Todo { task, .. }
        | Cmd::Doing { task, .. }
        | Cmd::Done { task, .. }
        | Cmd::Abandon { task, .. } => moves(task),
        Cmd::Verb(args) => {
            let mut rest = args.iter().skip(1);
            let mut has_ref = false;
            while let Some(arg) = rest.next() {
                if arg == "--tag" || arg == "-t" {
                    rest.next();
                } else if !arg.starts_with('-') {
                    has_ref = true;
                }
            }
            if has_ref { Write } else { Read }
        }
        _ => Write,
    }
}

/// Open the board, offering to create one when it isn't there: prompt when
/// interactive, create outright with `AMD_YES=1`, and otherwise fail with a
/// clear message — never block waiting on a pipe.
fn ensure_board() -> Result<Board> {
    let board = Board::discover()?;
    if board.root.is_dir() {
        return Ok(board);
    }
    let create = if std::env::var("AMD_YES").as_deref() == Ok("1") {
        true
    } else if form::available() {
        eprintln!("No task board found at {}", board.root.display());
        form::confirm("Create an empty board here?")?
    } else {
        return Board::open();
    };
    if !create {
        bail!("no board created");
    }
    board.create()?;
    eprintln!("Created board at {}", board.root.display());
    Ok(board)
}

/// Resolve a task reference, or offer a picker when it was left out.
fn resolve(
    board: &Board,
    reference: Option<String>,
    command: &str,
    columns: &[Column],
) -> Result<task::Task> {
    if let Some(reference) = reference {
        return board.find(&reference);
    }
    if !form::available() {
        bail!("usage: amd {command} <ref>");
    }
    let mut choices = Vec::new();
    for column in columns {
        choices.extend(board.tasks_in(*column)?);
    }
    if choices.is_empty() {
        let names: Vec<String> = columns.iter().map(|c| format!("{c}/")).collect();
        bail!("no tasks in {}", names.join(" or "));
    }
    form::select_task(&format!("Which task should amd {command}?"), choices)
}

fn cmd_new(board: &Board, args: NewArgs) -> Result<()> {
    let templates = Templates::load(board)?;
    // A bare `amd new` (or -i) fills the whole form in; otherwise only the
    // fields the chosen template needs and the arguments didn't supply.
    let full_form = args.interactive || args.title.is_none();
    if full_form && !form::available() {
        bail!("usage: amd new \"<title>\" [-t tag ...]");
    }

    // The title first: it's the one thing every ticket has, and it names the
    // file whatever the labels turn out to be.
    let title = match args.title {
        Some(title) if !args.interactive => title,
        title => form::title(title.as_deref())?,
    };
    branch::validate_sluggable(&title)?;

    // A branch type is optional, and empty by default: a ticket only gets a
    // branch when you say what kind of work it is.
    let branch_type = match args.branch_type {
        Some(chosen) => branch::normalise(&chosen),
        None if full_form => branch::normalise(&form::select(
            "Branch type:",
            branch::choices(),
            &branch::NONE.to_string(),
        )?),
        None => String::new(),
    };
    let template = args.template.clone();

    // One parent instead of epic and story: nesting gives both, and any depth
    // past them.
    let parent = match args.parent {
        Some(parent) => Some(parent),
        None if full_form => {
            let answer = form::label(
                "Parent:",
                "",
                board.stems()?,
                "the ticket this one sits under; blank for none",
            )?;
            (!answer.is_empty()).then_some(answer)
        }
        None => None,
    };

    let assignee = match args.assignee {
        Some(who) => resolve_assignee(&board.root, Some(&who)),
        None if full_form => form::assignee(
            board_settings::load(board).user_names(),
            git::config(&board.root, "user.name"),
            "",
        )?,
        None => String::new(),
    };

    let repository = match args.repository {
        Some(name) => name,
        None if full_form => pick_repository(board)?,
        None => String::new(),
    };

    let related = if full_form && args.related.is_empty() {
        form::related("", board.stems()?)?
    } else {
        args.related
    };

    // Offered from the project's tag list when it has one, else from the
    // tags already in use (ticket 122). Saved in uppercase either way.
    let listed = board_settings::load(board).tags();
    let tags = if full_form {
        let known = if listed.is_empty() {
            board.tags()?
        } else {
            listed
        };
        form::tags(&args.tags.join(", "), known)?
    } else {
        args.tags
    };
    // A draft is a ticket carrying DRAFT (ticket 114). The form asks unless
    // `--draft` already said so, or the tags already carry it.
    let mut tags = tags;
    let draft = args.draft
        || tags.iter().any(|t| task::is_reserved_tag(t))
        || (full_form && form::confirm("Save as a draft?")?);
    if draft {
        tags.push(task::DRAFT_TAG.to_string());
    }
    let tags = task::normalize_tags(&tags);
    if let Some(warning) = board_settings::load(board).check_tags(&tags)? {
        eprintln!("amd: {warning}");
    }

    // Fields the template itself asks for: `{{ extra.owner }}` becomes a
    // question, so a template can't reference a value nobody collected.
    let mut extra: BTreeMap<String, String> = args.set.into_iter().collect();
    let required = templates.required_extras(&template)?;
    let missing: Vec<&String> = required
        .iter()
        .filter(|key| !extra.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        if !form::available() {
            let flags: Vec<String> = missing.iter().map(|key| format!("--set {key}=…")).collect();
            bail!("template '{template}' needs {}", flags.join(" "));
        }
        for key in missing {
            let answer = form::text(&format!("{}:", templates::field_label(key)), None, None)?;
            extra.insert(key.clone(), answer);
        }
    }

    let draft = Draft::prepare(
        board,
        &templates,
        NewTicket {
            title,
            assignee,
            repository,
            template,
            branch_type,
            parent,
            related,
            tags,
            extra,
        },
    )?;

    // The form doesn't stop at the metadata: unless told otherwise, it opens
    // the rendered ticket so the notes and checklist are filled in as part of
    // creating it. Nothing is written until the editor exits, so abandoning
    // the edit leaves no half-made task behind.
    let edit = !args.no_edit && form::available() && (args.edit || full_form);
    let body = if edit {
        Some(form::body(&draft.body, OsStr::new(&editor_command()))?)
    } else {
        None
    };

    let note = if draft.branch.is_empty() {
        String::new()
    } else {
        format!(" ({})", draft.branch)
    };
    let name = draft
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Not `with_auto_commit`: that helper takes its commit message eagerly,
    // but a new ticket's id isn't allocated until `draft.write` actually
    // runs (`Board::record_id`), so there's nothing to capture beforehand
    // the way every other verb captures its ticket's existing id_display.
    let auto_commit = board_settings::load(board).auto_commit();
    let before = auto_commit.then(|| git::dirty_paths(board.repo_root()).unwrap_or_default());
    let created = draft.write(board, body)?;
    // The same commit, push and replay-or-undo every other change gets
    // (ticket 102): a clash with a ticket someone else numbered at the same
    // moment is undone here, and fails, rather than reaching the remote.
    let message = format!(
        "new: {} \"{}\"",
        created.task.id_display(),
        created.task.title()
    );
    if let Some(warning) = transition::commit_since(board, before, &message)? {
        eprintln!("amd: {warning}");
    }
    for id in &created.linked {
        println!("{id} now links to {}", created.task.id_display());
    }
    println!("created {}/{name}{note}", Column::Backlog);
    if let Some(warning) = created
        .task
        .assignee()
        .and_then(|who| board_settings::unknown_assignee(board, &who))
    {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn cmd_import(board: &Board, path: &Path, dry_run: bool) -> Result<()> {
    let summary = agile_md::import::import(board, path, dry_run)?;
    for (path, reason) in &summary.skipped {
        eprintln!("skipped {}: {reason}", path.display());
    }
    if summary.imported.is_empty() {
        println!("nothing to import from {}", path.display());
        return Ok(());
    }
    for ticket in &summary.imported {
        let flag = if ticket.duplicate {
            "  [DUPLICATE]"
        } else {
            ""
        };
        println!(
            "{} -> {} \"{}\" ({}){flag}",
            ticket.old_id, ticket.new_id, ticket.title, ticket.column
        );
    }
    let dup = summary.duplicate_count();
    let mut line = format!(
        "{} {} ticket{}",
        if dry_run { "would import" } else { "imported" },
        summary.imported.len(),
        if summary.imported.len() == 1 { "" } else { "s" }
    );
    if dup > 0 {
        line.push_str(&format!(", {dup} flagged DUPLICATE"));
    }
    if !summary.skipped.is_empty() {
        line.push_str(&format!(", {} skipped", summary.skipped.len()));
    }
    if dry_run {
        line.push_str(" — nothing written");
    }
    println!("{line}");
    Ok(())
}

/// `amd import documents [PATH]` (ticket 079): the tree at `path` (here by
/// default) into the project's documents repository, on a new branch,
/// pushed, with a pull request. `--dry-run` only shows the plan.
fn cmd_import_documents(
    path: Option<&Path>,
    dry_run: bool,
    into: Option<&str>,
    project: Option<&str>,
) -> Result<()> {
    let board = match project {
        Some(name) => {
            // Every registered board, scratch ones too (My work leaves
            // those out; naming one here is deliberate).
            let found: Vec<PathBuf> = Registry::load()
                .entries
                .into_iter()
                .filter(|e| e.has_board())
                .filter(|e| agile_md::current_project::project_name(&e.root) == name)
                .map(|e| e.root)
                .collect();
            match found.as_slice() {
                [one] => Board::in_repo(one),
                [] => bail!("no project called {name} on this machine (amd mine lists them)"),
                _ => bail!(
                    "{name} is in more than one folder: {}; run this from inside the one you mean",
                    found
                        .iter()
                        .map(|root| agile_md::registry::project_dir_of(root)
                            .display()
                            .to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        None => Board::discover()
            .context("which project? run this from inside it, or name it with --project <name>")?,
    };
    let own = agile_md::documents::sources(&board)
        .into_iter()
        .find(|s| s.kind == agile_md::documents::Kind::Own)
        .context("this project has no documents repository")?;
    let Some(repo) = own.root else {
        bail!(
            "{} isn't cloned on this machine; clone it beside the project's other repositories first",
            own.name
        );
    };
    let source = match path {
        Some(path) => path.to_path_buf(),
        None => env::current_dir().context("reading the current directory")?,
    };
    let plan = agile_md::import_documents::plan(&source, &repo, into)?;
    for clash in &plan.clashes {
        eprintln!("skipped {clash}: already in {}", own.name);
    }
    if plan.copies.is_empty() {
        println!("nothing to import from {}", plan.source.display());
        return Ok(());
    }
    for (_, to) in &plan.copies {
        println!("{} {to}", if dry_run { "would add" } else { "added" });
    }
    let count = plan.copies.len();
    let files = if count == 1 { "file" } else { "files" };
    if dry_run {
        println!(
            "would import {count} {files} into {} on a new branch — nothing written",
            own.name
        );
        return Ok(());
    }
    let outcome = agile_md::import_documents::apply(&plan)?;
    println!(
        "imported {count} {files} into {} on {} (from {})",
        own.name, outcome.branch, outcome.base
    );
    if let Some(address) = &outcome.pull_request {
        println!("pull request: {address}");
    }
    if let Some(warning) = &outcome.warning {
        eprintln!("amd: {warning}");
    }
    for step in &outcome.still_to_do {
        println!("still to do: {step}");
    }
    eprintln!("{} is back on {}", own.name, outcome.back_on);
    Ok(())
}

/// The board transition log: `date time action id title sha`, one line per
/// file change under the board, oldest first. Reads paths and rename
/// status from `git log --name-status -M`, never the commit message
/// (`%s`), so it's indifferent to whatever wrote the commit — including
/// history from before auto-commit existed, or a hand-written message.
fn cmd_log(board: &Board) -> Result<()> {
    let repo = board.repo_root();
    let board_rel = board
        .root
        .strip_prefix(repo)
        .unwrap_or(Path::new("tasks"))
        .to_path_buf();
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--name-status",
            "-M",
            "--date=format:%Y-%m-%d %H:%M",
            "--pretty=format:>>>%h|%ad",
            "--reverse",
        ])
        .arg("--")
        .arg(&board.root)
        .output()
        .context("running git log")?;
    if !out.status.success() {
        bail!("git log failed");
    }
    let text = String::from_utf8(out.stdout).context("git log produced non-utf8 output")?;

    let mut sha = String::new();
    let mut when = String::new();
    let mut any = false;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix(">>>") {
            let (h, w) = header.split_once('|').unwrap_or((header, ""));
            sha = h.to_string();
            when = w.to_string();
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let Some(status) = fields.next() else {
            continue;
        };
        let (action, path) = match status.as_bytes().first() {
            Some(b'A') => ("add".to_string(), fields.next()),
            Some(b'M') => ("edit".to_string(), fields.next()),
            Some(b'D') => ("remove".to_string(), fields.next()),
            Some(b'R') => {
                let (_old, new) = (fields.next(), fields.next());
                let action = new
                    .map(Path::new)
                    .and_then(|p| p.strip_prefix(&board_rel).ok())
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "move".to_string());
                (action, new)
            }
            _ => continue,
        };
        let Some(path) = path else { continue };
        let title = Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = match title.split_once('-') {
            Some((_, rest)) => rest.replace('-', " "),
            None => title.replace('-', " "),
        };
        let id = Path::new(path)
            .file_stem()
            .and_then(|stem| stem.to_string_lossy().split('-').next().map(str::to_string))
            .unwrap_or_default();
        println!("{when}  {action:<8} {id:>4}  {title}  {sha}");
        any = true;
    }
    if !any {
        println!("(no board history yet)");
    }
    Ok(())
}

fn cmd_templates(board: &Board, command: TemplateCmd) -> Result<()> {
    let templates = Templates::load(board)?;
    match command {
        TemplateCmd::List => {
            let width = templates
                .list()
                .map(|(name, _)| name.len())
                .max()
                .unwrap_or(0);
            for (name, source) in templates.list() {
                println!("{name:width$}  {}", source.describe());
            }
            Ok(())
        }
        TemplateCmd::Show { name } => {
            print!("{}", templates.source_text(&name)?);
            Ok(())
        }
        TemplateCmd::Eject { name, force } => {
            let source = templates::builtin_source(&name)
                .with_context(|| format!("'{name}' is not a built-in template"))?;
            let dir = board.templates_dir();
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let path = dir.join(format!("{name}{TEMPLATE_SUFFIX}"));
            if path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
            }
            fs::write(&path, source).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
            Ok(())
        }
    }
}

/// `$EDITOR`, else the settings file's `editor` key, else `vi` — used by
/// both `amd edit` and the form's body step. GUI-only, so this lives in the
/// CLI rather than amd-lib: the GUI never spawns an editor.
fn editor_command() -> String {
    let config = agile_md::settings::Config::load();
    agile_md::settings::resolved("EDITOR", &config, "editor")
        .filter(|editor| !editor.is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = editor_command();
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("running {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    Ok(())
}

fn parse_key_value(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(format!("expected KEY=VALUE, got '{raw}'")),
    }
}
