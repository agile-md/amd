//! agile-md — a tiny filesystem Kanban in markdown.
//!
//! Tasks are markdown files that move between `todo/`, `doing/` and `done/` in
//! `<repo-root>/tasks`. Status is the folder; `git mv` is the audit trail; the
//! ticket format is a MiniJinja template you can edit.
//!
//! This is `amd-lib`: everything that touches the board lives here, and
//! nothing that knows about a terminal or a window does. Creating a ticket and
//! moving it live here; how that is asked for and how it is drawn belong to
//! the front ends. See docs/adr/0004-one-library-two-interfaces.md.

pub mod access;
pub mod authorship;
pub mod board;
pub mod board_settings;
pub mod branch;
pub mod clone;
pub mod create;
pub mod current_project;
pub mod documents;
pub mod git;
pub mod group;
pub mod import;
pub mod import_documents;
pub mod model;
pub mod my_work;
pub mod numbering;
pub mod project_trust;
pub mod recover;
pub mod registry;
pub mod renumber;
pub mod richtext;
pub mod settings;
pub mod sync;
pub mod task;
pub mod templates;
pub mod transition;
