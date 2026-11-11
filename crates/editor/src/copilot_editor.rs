//! Inline Copilot suggestions are display-only inlays until the user accepts them.

use crate::{
    Editor, EditorEvent, SelectionEffects,
    inlays::{Inlay, InlayContent},
};
use copilot::{Completion, Copilot, DocumentInput, ghost_text, position_at};
use gpui::{App, Context, Entity, EntityId, Task, Window};
use language::{Buffer, language_settings::language_settings};
use multi_buffer::{MultiBufferOffset, ToOffset};
use project::InlayId;
use text::Rope;

const GHOST_ID: InlayId = InlayId::Copilot(0);

#[derive(Default)]
pub(super) struct CopilotEditorState {
    generation: u64,
    language: Option<String>,
    request: Option<Task<()>>,
    visible: Option<VisibleSuggestion>,
}

#[derive(Clone, PartialEq, Eq)]
struct SuggestionContext {
    buffer: EntityId,
    version: clock::Global,
    cursor: MultiBufferOffset,
    uri: lsp::Uri,
    language: String,
}

struct VisibleSuggestion {
    target: SuggestionContext,
    items: Vec<(Completion, String)>,
    selected: usize,
}

impl Editor {
    pub(super) fn init_copilot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = Copilot::global(cx) else {
            return;
        };
        if !self.mode.is_full() || self.project.is_none() {
            return;
        }
        let owner = cx.entity_id();
        self._subscriptions.push(cx.on_release(move |_, cx| {
            if let Some(service) = Copilot::global(cx) {
                service.update(cx, |service, _| service.release_document(owner));
            }
        }));
        self._subscriptions
            .push(cx.observe_in(&service, window, |editor, service, _, cx| {
                if editor
                    .copilot
                    .language
                    .as_ref()
                    .is_some_and(|language| !service.read(cx).can_complete(language))
                {
                    editor.dismiss_copilot(cx);
                    let owner = cx.entity_id();
                    service.update(cx, |service, _| service.release_document(owner));
                }
                cx.notify();
            }));
        self._subscriptions.push(cx.subscribe_in(
            &cx.entity(),
            window,
            |editor, _, event: &EditorEvent, window, cx| {
                match event {
                    EditorEvent::BufferEdited => editor.refresh_copilot(false, window, cx),
                    EditorEvent::SelectionsChanged { local: true } => {
                        // Local edits may emit both events. Start the debounce from
                        // the final cursor position after the edit is applied.
                        if editor.copilot.request.is_some() {
                            editor.refresh_copilot(false, window, cx);
                        } else {
                            editor.dismiss_copilot(cx);
                        }
                    }
                    EditorEvent::Blurred => {
                        editor.dismiss_copilot(cx);
                        if let Some(service) = Copilot::global(cx) {
                            let owner = cx.entity_id();
                            service.update(cx, |service, _| service.release_document(owner));
                        }
                    }
                    _ => {}
                }
            },
        ));
    }

    pub fn copilot_language(&self, cx: &App) -> Option<String> {
        let buffer = self.buffer.read(cx).as_singleton()?;
        let language = buffer.read(cx).language()?;
        Some(language.lsp_id())
    }

    fn copilot_context(&self, window: &Window, cx: &App) -> Option<(Entity<Buffer>, SuggestionContext)> {
        if !self.mode.is_full()
            || self.project.is_none()
            || !self.input_enabled
            || self.read_only(cx)
            || !self.focus_handle.is_focused(window)
            || self.ime_transaction.is_some()
            || self.pending_rename.is_some()
            || self.selections.count() != 1
        {
            return None;
        }
        let multi_buffer = self.buffer.read(cx);
        let buffer = multi_buffer.as_singleton()?;
        let buffer_read = buffer.read(cx);
        let file = buffer_read.file()?;
        if file.is_private() || buffer_read.len() > copilot::MAX_DOCUMENT_SIZE {
            return None;
        }
        let language = buffer_read.language()?.lsp_id();
        let path = file.as_local()?.abs_path(cx);
        let uri = lsp::Uri::from_file_path(&path).ok()?;
        let snapshot = multi_buffer.snapshot(cx);
        let selection = self.selections.newest_anchor();
        let cursor = selection.start.to_offset(&snapshot);
        if cursor != selection.end.to_offset(&snapshot) {
            return None;
        }
        let target = SuggestionContext {
            buffer: buffer.entity_id(),
            version: buffer_read.version().clone(),
            cursor,
            uri,
            language,
        };
        Some((buffer, target))
    }

    fn refresh_copilot(&mut self, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_copilot(cx);
        let Some(service) = Copilot::global(cx) else {
            return;
        };
        if !service.read(cx).ready_for_completions() {
            return;
        }
        let Some((_, target)) = self.copilot_context(window, cx) else {
            return;
        };
        if !service.read(cx).can_complete(&target.language) {
            return;
        }
        let debounce = service.read(cx).debounce();
        let generation = self.copilot.generation;
        self.copilot.language = Some(target.language.clone());
        self.copilot.request = Some(cx.spawn_in(window, async move |editor, cx| {
            if !manual {
                cx.background_executor().timer(debounce).await;
            }
            let Ok(Some((request, source))) = editor.update_in(cx, |editor, window, cx| {
                let (buffer, current) = editor.copilot_context(window, cx)?;
                if editor.copilot.generation != generation || current != target {
                    return None;
                }
                let buffer = buffer.read(cx);
                let source = buffer.text();
                let position = position_at(&source, target.cursor.0)?;
                let file = buffer.file()?;
                let workspace = editor
                    .project
                    .as_ref()
                    .and_then(|project| project.read(cx).worktree_for_id(file.worktree_id(cx), cx))
                    .and_then(|tree| lsp::Uri::from_file_path(tree.read(cx).abs_path()).ok());
                let settings = language_settings(buffer.language().map(|lang| lang.name()), buffer.file(), cx);
                let input = DocumentInput {
                    uri: target.uri.clone(),
                    workspace,
                    language: target.language.clone(),
                    text: source.clone(),
                    position,
                    tab_size: settings.tab_size.into(),
                    insert_spaces: !settings.hard_tabs,
                };
                let owner = cx.entity_id();
                Some((
                    service.update(cx, |service, cx| service.completions(owner, input, manual, cx)),
                    source,
                ))
            }) else {
                editor
                    .update(cx, |editor, _| {
                        if editor.copilot.generation == generation {
                            editor.copilot.request.take();
                        }
                    })
                    .ok();
                return;
            };
            let completions = request.await.unwrap_or_default();
            let items = completions
                .into_iter()
                .filter_map(|completion| {
                    let text = ghost_text(&source, target.cursor.0, &completion)?;
                    Some((completion, text))
                })
                .collect::<Vec<_>>();
            editor
                .update_in(cx, |editor, window, cx| {
                    if editor.copilot.generation != generation {
                        return;
                    }
                    editor.copilot.request.take();
                    if items.is_empty()
                        || !service.read(cx).can_complete(&target.language)
                        || editor
                            .copilot_context(window, cx)
                            .is_none_or(|(_, current)| current != target)
                    {
                        return;
                    }
                    editor.copilot.visible = Some(VisibleSuggestion {
                        target,
                        items,
                        selected: 0,
                    });
                    editor.render_copilot(cx);
                })
                .ok();
        }));
    }

    fn render_copilot(&mut self, cx: &mut Context<Self>) {
        let Some(suggestion) = &self.copilot.visible else {
            return;
        };
        let (item, text) = &suggestion.items[suggestion.selected];
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let inlay = Inlay {
            id: GHOST_ID,
            // Right-biased inlays are displayed after the caret at this buffer
            // position. A left-biased inlay would move the visible caret past
            // the suggestion, even though the buffer selection is unchanged.
            position: snapshot.anchor_after(suggestion.target.cursor),
            content: InlayContent::Text(Rope::from(text.as_str())),
        };
        if let Some(service) = Copilot::global(cx) {
            service.read(cx).shown(item);
        }
        self.splice_inlays(&[GHOST_ID], vec![inlay], cx);
    }

    pub(super) fn copilot_visible(&self) -> bool {
        self.copilot.visible.is_some()
    }

    pub(super) fn dismiss_copilot(&mut self, cx: &mut Context<Self>) -> bool {
        self.copilot.generation = self.copilot.generation.wrapping_add(1);
        self.copilot.request.take();
        self.copilot.language.take();
        if self.copilot.visible.take().is_some() {
            self.splice_inlays(&[GHOST_ID], Vec::new(), cx);
            true
        } else {
            false
        }
    }

    pub(super) fn accept_copilot(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(suggestion) = self.copilot.visible.take() else {
            return false;
        };
        self.splice_inlays(&[GHOST_ID], Vec::new(), cx);
        let Some(service) = Copilot::global(cx) else {
            return false;
        };
        if !service.read(cx).can_complete(&suggestion.target.language)
            || self
                .copilot_context(window, cx)
                .is_none_or(|(_, current)| current != suggestion.target)
        {
            return false;
        }
        let (item, text) = &suggestion.items[suggestion.selected];
        let cursor = suggestion.target.cursor;
        let after = MultiBufferOffset(cursor.0 + text.len());
        self.transact(window, cx, |editor, window, cx| {
            editor.buffer.update(cx, |buffer, cx| {
                buffer.edit([(cursor..cursor, text.as_str())], None, cx)
            });
            editor.change_selections(SelectionEffects::default(), window, cx, |selections| {
                selections.select_ranges([after..after]);
            });
        });
        service.read(cx).accepted(item, cx);
        true
    }

    pub(super) fn suggest_copilot_action(&mut self, _: &copilot::Suggest, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_copilot(true, window, cx);
    }

    pub(super) fn accept_copilot_action(&mut self, _: &copilot::Accept, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accept_copilot(window, cx) {
            cx.propagate();
        }
    }

    pub(super) fn dismiss_copilot_action(&mut self, _: &copilot::Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if !self.dismiss_copilot(cx) {
            cx.propagate();
        }
    }

    pub(super) fn next_copilot_action(&mut self, _: &copilot::NextSuggestion, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_copilot(true, cx);
    }

    pub(super) fn previous_copilot_action(
        &mut self,
        _: &copilot::PreviousSuggestion,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_copilot(false, cx);
    }

    fn cycle_copilot(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(suggestion) = &mut self.copilot.visible else {
            cx.propagate();
            return;
        };
        let len = suggestion.items.len();
        suggestion.selected = (suggestion.selected + if forward { 1 } else { len - 1 }) % len;
        self.render_copilot(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{display_map::ToDisplayPoint, editor_tests::init_test, test::editor_test_context::EditorTestContext};
    use gpui::TestAppContext;

    #[gpui::test]
    async fn test_copilot_preview_keeps_caret_before_suggestion(cx: &mut TestAppContext) {
        init_test(cx, |_| {});
        let mut cx = EditorTestContext::new(cx).await;

        for (state, suggestions) in [
            ("ˇ", ["hello", "hello\nworld"]),
            ("fn maˇin() {}", ["ready", "ready\n    // next line"]),
            (r#"let name = "Прˇивет";"#, ["🦀", "🦀\nмир"]),
            ("fn main() {\n    ˇ\n}", ["let value = 1;", "let value = 1;\n    value"]),
        ] {
            cx.set_state(state);
            cx.update_editor(|editor, window, cx| {
                let (_, target) = editor.copilot_context(window, cx).unwrap();
                let snapshot = editor.display_snapshot(cx);
                let cursor = target.cursor;
                let caret = cursor.to_display_point(&snapshot);
                let source = editor.text(cx);
                editor.copilot.visible = Some(VisibleSuggestion {
                    target,
                    items: suggestions
                        .iter()
                        .map(|text| {
                            (
                                Completion {
                                    insert_text: (*text).into(),
                                    range: None,
                                    command: None,
                                    extra: Default::default(),
                                },
                                (*text).into(),
                            )
                        })
                        .collect(),
                    selected: 0,
                });
                editor.render_copilot(cx);

                // Cycling replaces the preview without moving the caret, even
                // when the new suggestion spans several lines.
                for text in suggestions {
                    let mut preview = source.clone();
                    preview.insert_str(cursor.0, text);
                    assert_eq!(editor.display_text(cx), preview);
                    assert_eq!(editor.text(cx), source);
                    let snapshot = editor.display_snapshot(cx);
                    assert_eq!(editor.selections.display_ranges(&snapshot), vec![caret..caret]);
                    assert_eq!(
                        editor.selections.ranges::<MultiBufferOffset>(&snapshot),
                        vec![cursor..cursor]
                    );
                    editor.cycle_copilot(true, cx);
                }

                assert!(editor.dismiss_copilot(cx));
                assert_eq!(editor.display_text(cx), source);
                let snapshot = editor.display_snapshot(cx);
                assert_eq!(editor.selections.display_ranges(&snapshot), vec![caret..caret]);

                editor.insert("x", window, cx);
                let mut edited = source;
                edited.insert_str(cursor.0, "x");
                assert_eq!(editor.text(cx), edited);
                let snapshot = editor.display_snapshot(cx);
                let after = cursor + 1;
                assert_eq!(
                    editor.selections.ranges::<MultiBufferOffset>(&snapshot),
                    vec![after..after]
                );
            });
        }
    }
}
