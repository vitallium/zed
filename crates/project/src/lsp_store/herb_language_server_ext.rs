use gpui::{App, Entity, Task};
use language::Buffer;
use lsp::LanguageServerName;
use std::ops::Range;
use text::Anchor;

use crate::Project;

pub const HERB_LS_NAME: LanguageServerName = LanguageServerName::new_static("herb");

pub fn toggle_comments(
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    selections: Vec<Range<Anchor>>,
    cx: &mut App,
) -> Task<anyhow::Result<Option<language::Transaction>>> {
    project.update(cx, |project, cx| {
        project.toggle_comments_via_lsp(buffer, selections, cx)
    })
}

pub fn supports_toggle_comments(project: &Project, buffer: &Buffer, cx: &App) -> bool {
    project
        .language_server_id_for_name(buffer, &HERB_LS_NAME, cx)
        .is_some()
    // && project.lsp_store().read(cx).supports_toggle_comments_lsp()
}
