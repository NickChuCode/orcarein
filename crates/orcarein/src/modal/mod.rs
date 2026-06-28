//! Self-built multiline vim modal editor replacing rustyline readline on
//! capable terminals (spec: 2026-06-28-orcarein-vim-modal-editor-design).
//! Pure logic in submodules is always compiled & unit-tested; the raw-mode
//! I/O loop lives here behind `tui`.
//!
//! Scaffolded incrementally: types and fields land here ahead of the tasks
//! that consume them (motions, editing, visual, undo, render, I/O loop), so
//! allow dead code module-wide until those tasks wire everything together.
#![allow(dead_code)]

pub mod buffer;
// command/render/clipboard added in later tasks.

/// In-process, non-persisted input history (spec §5/§9). Owned by main.rs.
pub type History = Vec<String>;
