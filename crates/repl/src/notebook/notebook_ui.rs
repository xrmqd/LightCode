#![allow(unused, dead_code)]
use crate::ExecutionState;
use crate::kernels::{KernelClient, KernelSpecification, NativeRunningKernel, RemoteRunningKernel, RunningKernel};
use crate::repl_store::ReplStore;
use jupyter_protocol::{ExecuteRequest, InterruptRequest, JupyterMessage, JupyterMessageContent};
use serde_json::{Value, json};
use std::future::Future;
use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result};
use collections::HashMap;
use futures::FutureExt;
use futures::future::Shared;
use gpui::{
    AnyElement, App, Entity, EventEmitter, FocusHandle, Focusable, ListScrollEvent, ListState, Point, Task, actions,
    list, prelude::*,
};
use language::{Language, LanguageRegistry};
use project::{Project, ProjectEntryId, ProjectPath};
use proto::ViewId;
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::item::{ItemEvent, SaveOptions, TabContentParams};
use workspace::searchable::SearchableItemHandle;
use workspace::{Item, ItemHandle, Pane, ProjectItem, ToolbarItemLocation};
use workspace::{ToolbarItemEvent, ToolbarItemView};

use super::{Cell, CellPosition, RenderableCell};

use nbformat::v4::CellId;
use nbformat::v4::Metadata as NotebookMetadata;

actions!(
    notebook,
    [
        /// Runs the selected notebook cell.
        RunSelectedCell,
        InterruptKernel,
        RestartKernel,
        RefreshKernels,
        /// Runs all cells in the notebook.
        RunAll,
        /// Clears all cell outputs.
        ClearOutputs,
        /// Moves the current cell up.
        MoveCellUp,
        /// Moves the current cell down.
        MoveCellDown,
        /// Adds a new markdown cell.
        AddMarkdownBlock,
        /// Adds a new code cell.
        AddCodeBlock,
        DeleteCell,
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = notebook, no_json, no_register)]
pub struct RunCell {
    pub(super) id: String,
}

struct CellExecution {
    cell: CellId,
    request: String,
    reply_ok: Option<bool>,
    idle: bool,
    clear_on_output: bool,
}

pub(crate) const MAX_TEXT_BLOCK_WIDTH: f32 = 9999.0;
pub(crate) const SMALL_SPACING_SIZE: f32 = 8.0;
pub(crate) const MEDIUM_SPACING_SIZE: f32 = 12.0;
pub(crate) const LARGE_SPACING_SIZE: f32 = 16.0;
pub(crate) const GUTTER_WIDTH: f32 = 19.0;
pub(crate) const CODE_BLOCK_INSET: f32 = MEDIUM_SPACING_SIZE;
pub(crate) const CONTROL_SIZE: f32 = 20.0;

pub fn init(cx: &mut App) {
    workspace::register_project_item::<NotebookEditor>(cx);
}

pub struct NotebookEditor {
    languages: Arc<LanguageRegistry>,
    project: Entity<Project>,

    focus_handle: FocusHandle,
    notebook_item: Entity<NotebookItem>,

    remote_id: Option<ViewId>,
    cell_list: ListState,

    selected_cell_index: usize,
    cell_order: Vec<CellId>,
    cell_map: HashMap<CellId, Cell>,
    cell_data: HashMap<CellId, Value>,
    document: Value,
    saved_document: Value,
    disk_contents: String,
    save_task: Option<Shared<Task<std::result::Result<(), Arc<str>>>>>,
    notebook_language: Shared<Task<Option<Arc<Language>>>>,
    kernel_spec: Option<KernelSpecification>,
    kernel: Option<Box<dyn RunningKernel>>,
    kernel_start: Option<Task<()>>,
    kernel_error: Option<String>,
    pending_cells: VecDeque<CellId>,
    execution: Option<CellExecution>,
}

impl NotebookEditor {
    pub fn new(
        project: Entity<Project>,
        notebook_item: Entity<NotebookItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        let languages = project.read(cx).languages().clone();
        let document = notebook_item.read(cx).document.clone();
        let disk_contents = notebook_item.read(cx).disk_contents.clone();
        let cell_data = notebook_item
            .read(cx)
            .notebook
            .cells
            .iter()
            .zip(document["cells"].as_array().expect("parsed notebook cells"))
            .map(|(cell, data)| (cell.id().clone(), data.clone()))
            .collect();

        let notebook_language = notebook_item.read(cx).notebook_language();
        let notebook_language = cx.spawn_in(window, async move |_, _| notebook_language.await).shared();

        let mut cell_order = vec![]; // Vec<CellId>
        let mut cell_map = HashMap::default(); // HashMap<CellId, Cell>

        for (index, cell) in notebook_item.read(cx).notebook.clone().cells.iter().enumerate() {
            let cell_id = cell.id();
            cell_order.push(cell_id.clone());
            cell_map.insert(
                cell_id.clone(),
                Cell::load(cell, &languages, notebook_language.clone(), window, cx),
            );
        }

        let cell_count = cell_order.len();
        let cell_list = ListState::new(cell_count, gpui::ListAlignment::Top, px(1000.));

        let mut this = Self {
            project,
            languages: languages.clone(),
            focus_handle,
            notebook_item,
            remote_id: None,
            cell_list,
            selected_cell_index: 0,
            cell_order,
            cell_map,
            cell_data,
            saved_document: document.clone(),
            document,
            disk_contents,
            save_task: None,
            notebook_language,
            kernel_spec: None,
            kernel: None,
            kernel_start: None,
            kernel_error: None,
            pending_cells: VecDeque::new(),
            execution: None,
        };
        this.observe_cells(cx);
        let store = ReplStore::global(cx);
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let worktree = this.notebook_item.read(cx).project_path.worktree_id;
        store.update(cx, |store, cx| {
            store.refresh_python_kernelspecs(worktree, &this.project, cx).detach();
        });
        this
    }

    fn has_outputs(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.cell_map.values().any(|cell| {
            if let Cell::Code(code_cell) = cell {
                code_cell.read(cx).has_outputs()
            } else {
                false
            }
        })
    }

    fn observe_cells(&self, cx: &mut Context<Self>) {
        for cell in self.cell_map.values() {
            Self::observe_cell(cell, cx);
        }
    }

    fn observe_cell(cell: &Cell, cx: &mut Context<Self>) {
        let editor = match cell {
            Cell::Code(cell) => cell.read(cx).editor(),
            Cell::Markdown(cell) => cell.read(cx).editor(),
            Cell::Raw(_) => return,
        };
        cx.subscribe(&editor, |this, _, event, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                this.did_edit(cx);
            }
        })
        .detach();
    }

    fn did_edit(&self, cx: &mut Context<Self>) {
        cx.emit(ItemEvent::Edit);
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    fn clear_outputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for id in self.cell_order.clone() {
            if matches!(self.cell_map.get(&id), Some(Cell::Code(_))) {
                self.cell_data.get_mut(&id).unwrap()["outputs"] = json!([]);
                self.cell_data.get_mut(&id).unwrap()["execution_count"] = Value::Null;
                self.refresh_cell_output(&id, window, cx);
            }
        }
        self.did_edit(cx);
    }

    fn run_cells(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enqueue(self.cell_order.clone(), window, cx);
    }

    fn enqueue(&mut self, cells: impl IntoIterator<Item = CellId>, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_cells.extend(
            cells
                .into_iter()
                .filter(|id| matches!(self.cell_map.get(id), Some(Cell::Code(_)))),
        );
        if self.pending_cells.is_empty() {
            return;
        }
        if self.kernel.is_none() && self.kernel_start.is_none() {
            self.start_kernel(window, cx);
        }
        self.run_next(window, cx);
    }

    fn available_kernels(&self, cx: &App) -> Vec<KernelSpecification> {
        let worktree = self.notebook_item.read(cx).project_path.worktree_id;
        ReplStore::global(cx)
            .read(cx)
            .kernel_specifications_for_worktree(worktree)
            .cloned()
            .collect()
    }

    fn default_kernel(&self, cx: &App) -> Option<KernelSpecification> {
        let item = self.notebook_item.read(cx);
        let name = item.document["metadata"]["kernelspec"]["name"].as_str();
        let language = item.language_name().unwrap_or_else(|| "python".to_owned());
        let kernels = self.available_kernels(cx);
        kernels
            .iter()
            .find(|spec| name == Some(spec.name().as_ref()))
            .cloned()
            .or_else(|| {
                kernels
                    .into_iter()
                    .find(|spec| spec.language().eq_ignore_ascii_case(&language))
            })
    }

    fn start_kernel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.kernel_spec.clone().or_else(|| self.default_kernel(cx)) else {
            self.on_kernel_error(
                "No matching Jupyter kernel. Install ipykernel in your project environment and refresh kernels.".into(),
                cx,
            );
            return;
        };
        let item = self.notebook_item.read(cx);
        let working_directory = item.path.parent().unwrap_or(&item.path).to_path_buf();
        let fs = self.project.read(cx).fs().clone();
        let client = cx.entity().downgrade();
        let task = match spec.clone() {
            KernelSpecification::Jupyter(local) | KernelSpecification::PythonEnv(local) => {
                NativeRunningKernel::launch(local, cx.entity_id(), working_directory, fs, client, window, cx)
            }
            KernelSpecification::Remote(remote) => {
                RemoteRunningKernel::launch(remote, working_directory, client, window, cx)
            }
        };
        self.kernel_spec = Some(spec);
        self.kernel_error = None;
        self.kernel_start = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.kernel_start.take();
                match result {
                    Ok(kernel) => {
                        this.kernel = Some(kernel);
                        this.run_next(window, cx);
                    }
                    Err(error) => this.on_kernel_error(error.to_string(), cx),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn run_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.execution.is_some() {
            return;
        }
        let Some(kernel) = &self.kernel else { return };
        let mut sender = kernel.request_tx();
        while let Some(id) = self.pending_cells.pop_front() {
            let Some(Cell::Code(cell)) = self.cell_map.get(&id) else {
                continue;
            };
            let code = cell.read(cx).current_source(cx);
            if code.trim().is_empty() {
                continue;
            }
            let message: JupyterMessage = ExecuteRequest {
                code,
                allow_stdin: false,
                ..Default::default()
            }
            .into();
            let request = message.header.msg_id.clone();
            if let Err(error) = sender.try_send(message) {
                self.on_kernel_error(format!("Could not send cell to the kernel: {error}"), cx);
                return;
            }
            let data = self.cell_data.get_mut(&id).unwrap();
            data["outputs"] = json!([]);
            data["execution_count"] = Value::Null;
            self.execution = Some(CellExecution {
                cell: id.clone(),
                request,
                reply_ok: None,
                idle: false,
                clear_on_output: false,
            });
            self.refresh_cell_output(&id, window, cx);
            self.did_edit(cx);
            break;
        }
    }

    fn refresh_cell_output(&self, id: &CellId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Cell::Code(cell)) = self.cell_map.get(id) else {
            return;
        };
        let data = &self.cell_data[id];
        let outputs: Vec<nbformat::v4::Output> = data["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|output| serde_json::from_value(output.clone()).ok())
            .collect();
        let count = data["execution_count"].as_i64().and_then(|count| count.try_into().ok());
        cell.update(cx, |cell, cx| cell.update_results(&outputs, count, window, cx));
    }

    fn stop_kernel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_cells.clear();
        self.execution = None;
        self.kernel_start.take();
        if let Some(mut kernel) = self.kernel.take() {
            kernel.force_shutdown(window, cx).detach();
        }
        self.kernel_error = None;
        cx.notify();
    }

    fn handle_interrupt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_cells.clear();
        if let Some(kernel) = &self.kernel {
            if let Err(error) = kernel.request_tx().try_send(InterruptRequest {}.into()) {
                self.on_kernel_error(error.to_string(), cx);
            }
        } else {
            self.stop_kernel(window, cx);
        }
    }

    fn move_cell(&mut self, direction: isize, cx: &mut Context<Self>) {
        let from = self.selected_cell_index;
        if let Some(to) = from
            .checked_add_signed(direction)
            .filter(|to| *to < self.cell_order.len())
        {
            self.cell_order.swap(from, to);
            self.selected_cell_index = to;
            self.cell_list.reset(self.cell_order.len());
            self.did_edit(cx);
        }
    }

    fn add_cell(&mut self, kind: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = CellId::from(uuid::Uuid::new_v4());
        let mut data = json!({"id": id.as_str(), "cell_type": kind, "metadata": {}, "source": []});
        if kind == "code" {
            data["execution_count"] = Value::Null;
            data["outputs"] = json!([]);
        }
        let Ok(definition) = serde_json::from_value(data.clone()) else {
            return;
        };
        let cell = Cell::load(&definition, &self.languages, self.notebook_language.clone(), window, cx);
        Self::observe_cell(&cell, cx);
        let index = (self.selected_cell_index + 1).min(self.cell_order.len());
        self.cell_order.insert(index, id.clone());
        self.cell_map.insert(id.clone(), cell);
        self.cell_data.insert(id, data);
        self.cell_list.splice(index..index, 1);
        self.set_selected_index(index, true, window, cx);
        self.did_edit(cx);
    }

    fn delete_selected_cell(&mut self, cx: &mut Context<Self>) {
        let index = self.selected_cell_index;
        let Some(id) = self.cell_order.get(index).cloned() else {
            return;
        };
        if self.execution.as_ref().is_some_and(|execution| execution.cell == id) {
            return;
        }
        self.pending_cells.retain(|pending| *pending != id);
        self.cell_order.remove(index);
        self.cell_map.remove(&id);
        self.cell_data.remove(&id);
        self.selected_cell_index = index.min(self.cell_order.len().saturating_sub(1));
        self.cell_list.splice(index..index + 1, 0);
        self.did_edit(cx);
    }

    fn current_document(&self, cx: &App) -> Value {
        let mut document = self.document.clone();
        document["cells"] = Value::Array(
            self.cell_order
                .iter()
                .map(|id| {
                    let mut data = self.cell_data[id].clone();
                    let source = match &self.cell_map[id] {
                        Cell::Code(cell) => Some(cell.read(cx).current_source(cx)),
                        Cell::Markdown(cell) => Some(cell.read(cx).current_source(cx)),
                        Cell::Raw(_) => None,
                    };
                    if let Some(source) = source {
                        super::document::update_source(&mut data, &source);
                    }
                    data
                })
                .collect(),
        );
        document
    }

    fn cell_count(&self) -> usize {
        self.cell_map.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_cell_index
    }

    pub fn set_selected_index(
        &mut self,
        index: usize,
        jump_to_index: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // let previous_index = self.selected_cell_index;
        self.selected_cell_index = index;
        let current_index = self.selected_cell_index;

        // in the future we may have some `on_cell_change` event that we want to fire here

        if jump_to_index {
            self.jump_to_cell(current_index, window, cx);
        }
    }

    pub fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.cell_count();
        if count > 0 {
            let index = self.selected_index();
            let ix = if index == count - 1 { count - 1 } else { index + 1 };
            self.set_selected_index(ix, true, window, cx);
            cx.notify();
        }
    }

    pub fn select_previous(&mut self, _: &menu::SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.cell_count();
        if count > 0 {
            let index = self.selected_index();
            let ix = if index == 0 { 0 } else { index - 1 };
            self.set_selected_index(ix, true, window, cx);
            cx.notify();
        }
    }

    pub fn select_first(&mut self, _: &menu::SelectFirst, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.cell_count();
        if count > 0 {
            self.set_selected_index(0, true, window, cx);
            cx.notify();
        }
    }

    pub fn select_last(&mut self, _: &menu::SelectLast, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.cell_count();
        if count > 0 {
            self.set_selected_index(count - 1, true, window, cx);
            cx.notify();
        }
    }

    fn jump_to_cell(&mut self, index: usize, _window: &mut Window, _cx: &mut Context<Self>) {
        self.cell_list.scroll_to_reveal_item(index);
    }

    fn button_group(window: &mut Window, cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap(DynamicSpacing::Base04.rems(cx))
            .items_center()
            .w(px(CONTROL_SIZE + 4.0))
            .overflow_hidden()
            .rounded(px(5.))
            .bg(cx.theme().colors().title_bar_background)
            .p_px()
            .border_1()
            .border_color(cx.theme().colors().border)
    }

    fn render_notebook_control(
        id: impl Into<SharedString>,
        icon: IconName,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> IconButton {
        let id: ElementId = ElementId::Name(id.into());
        IconButton::new(id, icon).width(px(CONTROL_SIZE))
    }

    fn render_notebook_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_outputs = self.has_outputs(window, cx);
        let kernels = self.available_kernels(cx);
        let selected = self.kernel_spec.clone().or_else(|| self.default_kernel(cx));
        let view = cx.entity().downgrade();
        let tooltip: SharedString = if let Some(error) = &self.kernel_error {
            error.clone().into()
        } else if self.kernel_start.is_some() {
            "Starting Jupyter kernel".into()
        } else if self.execution.is_some() {
            "Jupyter kernel is running".into()
        } else {
            selected
                .as_ref()
                .map(|spec| spec.name())
                .unwrap_or_else(|| "Select Jupyter kernel".into())
        };
        let kernel_menu = PopoverMenu::new("notebook-kernel")
            .trigger(IconButton::new("notebook-kernel", IconName::ReplNeutral).tooltip(Tooltip::text(tooltip)))
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                    for kernel in &kernels {
                        let kernel = kernel.clone();
                        let view = view.clone();
                        menu = menu.entry(kernel.name(), None, move |window, cx| {
                            view.update(cx, |this, cx| {
                                this.stop_kernel(window, cx);
                                this.kernel_spec = Some(kernel.clone());
                                cx.notify();
                            })
                            .ok();
                        });
                    }
                    menu.separator()
                        .action("Interrupt Kernel", Box::new(InterruptKernel))
                        .action("Restart Kernel", Box::new(RestartKernel))
                        .action("Refresh Kernels", Box::new(RefreshKernels))
                }))
            });

        v_flex()
            .max_w(px(CONTROL_SIZE + 4.0))
            .items_center()
            .gap(DynamicSpacing::Base16.rems(cx))
            .justify_between()
            .flex_none()
            .h_full()
            .py(DynamicSpacing::Base12.px(cx))
            .child(
                v_flex()
                    .gap(DynamicSpacing::Base08.rems(cx))
                    .child(
                        Self::button_group(window, cx)
                            .child(
                                Self::render_notebook_control("run-all-cells", IconName::PlayFilled, window, cx)
                                    .tooltip(move |window, cx| Tooltip::for_action("Execute all cells", &RunAll, cx))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(RunAll), cx);
                                    }),
                            )
                            .child(
                                Self::render_notebook_control("clear-all-outputs", IconName::ListX, window, cx)
                                    .disabled(!has_outputs)
                                    .tooltip(move |window, cx| {
                                        Tooltip::for_action("Clear all outputs", &ClearOutputs, cx)
                                    })
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ClearOutputs), cx);
                                    }),
                            ),
                    )
                    .child(
                        Self::button_group(window, cx)
                            .child(
                                Self::render_notebook_control("move-cell-up", IconName::ArrowUp, window, cx)
                                    .tooltip(move |window, cx| Tooltip::for_action("Move cell up", &MoveCellUp, cx))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(MoveCellUp), cx);
                                    }),
                            )
                            .child(
                                Self::render_notebook_control("move-cell-down", IconName::ArrowDown, window, cx)
                                    .tooltip(move |window, cx| Tooltip::for_action("Move cell down", &MoveCellDown, cx))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(MoveCellDown), cx);
                                    }),
                            ),
                    )
                    .child(
                        Self::button_group(window, cx)
                            .child(
                                Self::render_notebook_control("new-markdown-cell", IconName::Plus, window, cx)
                                    .tooltip(move |window, cx| {
                                        Tooltip::for_action("Add markdown block", &AddMarkdownBlock, cx)
                                    })
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(AddMarkdownBlock), cx);
                                    }),
                            )
                            .child(
                                Self::render_notebook_control("new-code-cell", IconName::Code, window, cx)
                                    .tooltip(move |window, cx| Tooltip::for_action("Add code block", &AddCodeBlock, cx))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(AddCodeBlock), cx);
                                    }),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap(DynamicSpacing::Base08.rems(cx))
                    .items_center()
                    .child(
                        Self::render_notebook_control("delete-cell", IconName::Trash, window, cx)
                            .disabled(self.cell_order.is_empty())
                            .tooltip(move |_, cx| Tooltip::for_action("Delete selected cell", &DeleteCell, cx))
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(DeleteCell), cx)),
                    )
                    .child(kernel_menu),
            )
    }

    fn cell_position(&self, index: usize) -> CellPosition {
        match index {
            0 => CellPosition::First,
            index if index == self.cell_count() - 1 => CellPosition::Last,
            _ => CellPosition::Middle,
        }
    }

    fn render_cell(&self, index: usize, cell: &Cell, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cell_position = self.cell_position(index);

        let is_selected = index == self.selected_cell_index;

        match cell {
            Cell::Code(cell) => {
                cell.update(cx, |cell, _cx| {
                    cell.set_selected(is_selected).set_cell_position(cell_position);
                });
                cell.clone().into_any_element()
            }
            Cell::Markdown(cell) => {
                cell.update(cx, |cell, _cx| {
                    cell.set_selected(is_selected).set_cell_position(cell_position);
                });
                cell.clone().into_any_element()
            }
            Cell::Raw(cell) => {
                cell.update(cx, |cell, _cx| {
                    cell.set_selected(is_selected).set_cell_position(cell_position);
                });
                cell.clone().into_any_element()
            }
        }
    }
}

impl Render for NotebookEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kernel_error = self.kernel_error.clone();
        div()
            .key_context("notebook")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, &ClearOutputs, window, cx| this.clear_outputs(window, cx)))
            .on_action(cx.listener(|this, &RunAll, window, cx| this.run_cells(window, cx)))
            .on_action(cx.listener(|this, _: &RunSelectedCell, window, cx| {
                this.enqueue(this.cell_order.get(this.selected_cell_index).cloned(), window, cx);
            }))
            .on_action(cx.listener(|this, action: &RunCell, window, cx| {
                this.enqueue(CellId::new(&action.id).ok(), window, cx);
            }))
            .on_action(cx.listener(|this, _: &InterruptKernel, window, cx| this.handle_interrupt(window, cx)))
            .on_action(cx.listener(|this, _: &RestartKernel, window, cx| {
                this.stop_kernel(window, cx);
                this.start_kernel(window, cx);
            }))
            .on_action(cx.listener(|this, &MoveCellUp, window, cx| this.move_cell(-1, cx)))
            .on_action(cx.listener(|this, &MoveCellDown, window, cx| this.move_cell(1, cx)))
            .on_action(cx.listener(|this, &AddMarkdownBlock, window, cx| this.add_cell("markdown", window, cx)))
            .on_action(cx.listener(|this, &AddCodeBlock, window, cx| this.add_cell("code", window, cx)))
            .on_action(cx.listener(|this, _: &RefreshKernels, _, cx| {
                let worktree = this.notebook_item.read(cx).project_path.worktree_id;
                ReplStore::global(cx).update(cx, |store, cx| {
                    store.refresh_kernelspecs(cx).detach();
                    store.refresh_python_kernelspecs(worktree, &this.project, cx).detach();
                });
            }))
            .on_action(cx.listener(|this, _: &DeleteCell, _, cx| this.delete_selected_cell(cx)))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .flex()
            .items_start()
            .size_full()
            .overflow_hidden()
            .px(DynamicSpacing::Base12.px(cx))
            .gap(DynamicSpacing::Base12.px(cx))
            .bg(cx.theme().colors().tab_bar_background)
            .child(
                v_flex()
                    .id("notebook-cells")
                    .flex_1()
                    .size_full()
                    .overflow_y_scroll()
                    .when_some(kernel_error, |body, error| {
                        body.child(Label::new(error).color(Color::Error))
                    })
                    .child(list(
                        self.cell_list.clone(),
                        cx.processor(|this, ix, window, cx| {
                            this.cell_order
                                .get(ix)
                                .and_then(|cell_id| this.cell_map.get(cell_id))
                                .map(|cell| {
                                    div()
                                        .id(("notebook-cell", ix))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                this.set_selected_index(ix, false, window, cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child(this.render_cell(ix, cell, window, cx))
                                        .into_any_element()
                                })
                                .unwrap_or_else(|| div().into_any())
                        }),
                    ))
                    .size_full(),
            )
            .child(self.render_notebook_controls(window, cx))
    }
}

impl Focusable for NotebookEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// Intended to be a NotebookBuffer
pub struct NotebookItem {
    path: PathBuf,
    project_path: ProjectPath,
    languages: Arc<LanguageRegistry>,
    // Raw notebook data
    notebook: nbformat::v4::Notebook,
    document: Value,
    disk_contents: String,
    // Store our version of the notebook in memory (cell_order, cell_map)
    id: ProjectEntryId,
}

impl project::ProjectItem for NotebookItem {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Entity<Self>>>> {
        let path = path.clone();
        let project = project.clone();
        let fs = project.read(cx).fs().clone();
        let languages = project.read(cx).languages().clone();

        if path.path.extension().unwrap_or_default() == "ipynb" {
            Some(cx.spawn(async move |cx| {
                let abs_path = project
                    .read_with(cx, |project, cx| project.absolute_path(&path, cx))?
                    .with_context(|| format!("finding the absolute path of {path:?}"))?;

                // todo: watch for changes to the file
                let file_content = fs.load(abs_path.as_path()).await?;
                let (notebook, document) = super::document::parse(&file_content)?;

                let id = project
                    .update(cx, |project, cx| {
                        project.entry_for_path(&path, cx).map(|entry| entry.id)
                    })?
                    .context("Entry not found")?;

                cx.new(|_| NotebookItem {
                    path: abs_path,
                    project_path: path,
                    languages,
                    notebook,
                    document,
                    disk_contents: file_content,
                    id,
                })
            }))
        } else {
            None
        }
    }

    fn entry_id(&self, _: &App) -> Option<ProjectEntryId> {
        Some(self.id)
    }

    fn project_path(&self, _: &App) -> Option<ProjectPath> {
        Some(self.project_path.clone())
    }

    fn is_dirty(&self) -> bool {
        false
    }
}

impl NotebookItem {
    pub fn language_name(&self) -> Option<String> {
        self.notebook
            .metadata
            .language_info
            .as_ref()
            .map(|l| l.name.clone())
            .or(self
                .notebook
                .metadata
                .kernelspec
                .as_ref()
                .and_then(|spec| spec.language.clone()))
    }

    pub fn notebook_language(&self) -> impl Future<Output = Option<Arc<Language>>> + use<> {
        let language_name = self.language_name();
        let languages = self.languages.clone();

        async move {
            if let Some(language_name) = language_name {
                languages.language_for_name(&language_name).await.ok()
            } else {
                None
            }
        }
    }
}

impl EventEmitter<ItemEvent> for NotebookEditor {}

impl Item for NotebookEditor {
    type Event = ItemEvent;

    fn to_item_events(event: &ItemEvent, mut emit: impl FnMut(ItemEvent)) {
        emit(*event);
    }

    fn buffer_kind(&self, _: &App) -> workspace::item::ItemBufferKind {
        workspace::item::ItemBufferKind::Singleton
    }

    fn for_each_project_item(&self, cx: &App, f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem)) {
        f(self.notebook_item.entity_id(), self.notebook_item.read(cx))
    }

    fn tab_content(&self, params: TabContentParams, window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or(0), cx))
            .single_line()
            .color(params.text_color())
            .when(params.preview, |this| this.italic())
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let path = &self.notebook_item.read(cx).path;
        let title = path
            .file_name()
            .unwrap_or_else(|| path.as_os_str())
            .to_string_lossy()
            .to_string();
        title.into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(IconName::Book.into())
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    // TODO
    fn pixel_position_of_cursor(&self, _: &App) -> Option<Point<Pixels>> {
        None
    }

    // TODO
    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        None
    }

    fn set_nav_history(&mut self, _: workspace::ItemNavHistory, _window: &mut Window, _: &mut Context<Self>) {
        // TODO
    }

    fn can_save(&self, _cx: &App) -> bool {
        true
    }

    fn save(
        &mut self,
        _options: SaveOptions,
        project: Entity<Project>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let document = self.current_document(cx);
        let previous = self.save_task.clone();
        let fs = project.read(cx).fs().clone();
        let path = self.notebook_item.read(cx).path.clone();
        let task = cx
            .spawn(async move |this, cx| {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                let result = async {
                    let disk_contents = this.read_with(cx, |this, _| this.disk_contents.clone())?;
                    anyhow::ensure!(
                        fs.load(&path).await? == disk_contents,
                        "Notebook changed on disk. Reopen it before saving."
                    );
                    let contents = serde_json::to_string_pretty(&document)? + "\n";
                    fs.atomic_write(path, contents.clone()).await?;
                    this.update(cx, |this, cx| {
                        this.disk_contents = contents;
                        this.saved_document = document;
                        cx.emit(ItemEvent::UpdateTab);
                    })?;
                    anyhow::Ok(())
                }
                .await;
                result.map_err(|error| Arc::<str>::from(error.to_string()))
            })
            .shared();
        self.save_task = Some(task.clone());
        cx.spawn(async move |_, _| task.await.map_err(|error| anyhow::anyhow!("{error}")))
    }

    fn reload(&mut self, _project: Entity<Project>, _window: &mut Window, _cx: &mut Context<Self>) -> Task<Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "Close and reopen the notebook to reload it from disk."
        )))
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.current_document(cx) != self.saved_document
    }
}

// TODO: Implement this to allow us to persist to the database, etc:
// impl SerializableItem for NotebookEditor {}

impl ProjectItem for NotebookEditor {
    type Item = NotebookItem;

    fn for_project_item(
        project: Entity<Project>,
        _: Option<&Pane>,
        item: Entity<Self::Item>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self
    where
        Self: Sized,
    {
        Self::new(project, item, window, cx)
    }
}

impl KernelClient for NotebookEditor {
    fn on_kernel_message(&mut self, message: &JupyterMessage, window: &mut Window, cx: &mut Context<Self>) {
        let Some(execution) = &mut self.execution else { return };
        if message.parent_header.as_ref().map(|header| header.msg_id.as_str()) != Some(execution.request.as_str()) {
            return;
        }
        let id = execution.cell.clone();
        let data = self.cell_data.get_mut(&id).unwrap();
        let output = super::document::execution_output(&message.content);
        match &message.content {
            JupyterMessageContent::ClearOutput(value) => {
                execution.clear_on_output = value.wait;
                if !value.wait {
                    data["outputs"] = json!([]);
                }
            }
            JupyterMessageContent::ExecuteReply(value) => {
                data["execution_count"] = json!(value.execution_count);
                execution.reply_ok = Some(json!(value.status) == "ok");
            }
            JupyterMessageContent::Status(value) => {
                execution.idle = matches!(value.execution_state, ExecutionState::Idle);
            }
            _ if output.is_none() => return,
            _ => {}
        }
        if let Some(output) = output {
            let outputs = data["outputs"].as_array_mut().unwrap();
            if std::mem::take(&mut execution.clear_on_output) {
                outputs.clear();
            }
            outputs.push(output);
        }
        let finished = execution.reply_ok.filter(|_| execution.idle);
        self.refresh_cell_output(&id, window, cx);
        self.did_edit(cx);
        if let Some(success) = finished {
            self.execution = None;
            if success {
                self.run_next(window, cx);
            } else {
                self.pending_cells.clear();
            }
        }
    }

    fn on_kernel_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.kernel_error = Some(error);
        self.kernel_start.take();
        self.kernel.take();
        self.pending_cells.clear();
        self.execution.take();
        cx.notify();
    }
}
