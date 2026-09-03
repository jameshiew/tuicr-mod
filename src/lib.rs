pub mod app;
pub mod cli;
pub mod comment_vim;
pub mod config;
pub mod editor;
pub mod error;
pub mod handler;
pub mod hash;
pub mod input;
pub mod model;
pub mod output;
pub mod process;
pub mod profile;
pub mod slug;
pub mod syntax;
pub mod terminal_state;
pub mod text_edit;
pub mod theme;
pub mod tuicrignore;
pub mod ui;
pub mod vcs;

pub use error::{Result, TuicrError};
pub use model::{
    AddCommentRequest, Comment, CommentTarget, CommentType, LineRange, LineSide, ReviewSession,
    SessionDiffSource, add_comment_to_session,
};
