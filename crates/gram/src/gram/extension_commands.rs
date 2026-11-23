use editor::Editor;
use extension::{ExtensionCommand, InlineCompletionProvider};
use extension_host::ExtensionStore;
use gpui::{Action, App, Context, Window};
use ui::{Button, prelude::*};
use workspace::{Toast, Workspace, notifications::NotificationId};

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
#[action(namespace = lcode)]
struct RunExtensionCommand {
    extension: String,
    command: String,
}

pub fn init(cx: &mut App) {
    command_palette::hide_namespace("copilot", cx);
    command_palette::set_external_command_provider(
        |workspace, cx| {
            commands(workspace, cx)
                .into_iter()
                .map(|(command, action)| (command.title, action.boxed_clone()))
                .collect()
        },
        cx,
    );
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(run_command);
    })
    .detach();
}

fn commands(workspace: &Workspace, cx: &App) -> Vec<(ExtensionCommand, RunExtensionCommand)> {
    let Some(store) = ExtensionStore::try_global(cx) else {
        return Vec::new();
    };
    let language = workspace
        .active_item_as::<Editor>(cx)
        .and_then(|editor| editor.read(cx).active_language_lsp_id(cx));
    store
        .read(cx)
        .wasm_extensions
        .iter()
        .filter(|(_, _, extension)| extension.has_run_command)
        .flat_map(|(_, manifest, _)| {
            manifest.commands.iter().filter_map(|(id, command)| {
                if let Some(required) = &command.language
                    && !language
                        .as_ref()
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(required))
                {
                    return None;
                }
                Some((
                    command.clone(),
                    RunExtensionCommand {
                        extension: manifest.id.to_string(),
                        command: id.to_string(),
                    },
                ))
            })
        })
        .collect()
}

pub(super) fn toolbar_buttons(workspace: &Workspace, cx: &App) -> Vec<Button> {
    commands(workspace, cx)
        .into_iter()
        .filter(|(command, _)| command.editor_toolbar)
        .map(|(command, action)| {
            Button::new(
                format!("extension:{}:{}", action.extension, action.command),
                command.title,
            )
            .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        })
        .collect()
}

fn run_command(
    workspace: &mut Workspace,
    action: &RunExtensionCommand,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(store) = ExtensionStore::try_global(cx) else {
        return;
    };
    let Some((_, manifest, extension)) = store.read(cx).wasm_extensions.iter().find(|(_, manifest, _)| {
        manifest.id.as_ref() == action.extension && manifest.commands.contains_key(action.command.as_str())
    }) else {
        return;
    };
    let path = workspace.active_item_as::<Editor>(cx).and_then(|editor| {
        let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
        let file = buffer.read(cx).file()?;
        if file.is_private() {
            return None;
        }
        file.as_local()?.abs_path(cx).into_os_string().into_string().ok()
    });
    let result = extension.run_command(action.command.clone(), path);
    let copilot = manifest.inline_completion_provider == Some(InlineCompletionProvider::GithubCopilot)
        && manifest.id.as_ref() == "github-copilot";
    cx.spawn_in(window, async move |this, cx| {
        let result = result.await;
        this.update_in(cx, |workspace, window, cx| match result {
            Ok(Some(command)) if copilot => {
                let action: Option<Box<dyn Action>> = match command.as_str() {
                    "sign-in" => Some(Box::new(copilot::SignIn)),
                    "sign-out" => Some(Box::new(copilot::SignOut)),
                    "enable" => Some(Box::new(copilot::Enable)),
                    "disable" => Some(Box::new(copilot::Disable)),
                    "pause" => Some(Box::new(copilot::Snooze)),
                    "resume" => Some(Box::new(copilot::Resume)),
                    "suggest" => Some(Box::new(copilot::Suggest)),
                    "toggle-language" => Some(Box::new(copilot::ToggleLanguage)),
                    _ => None,
                };
                if let Some(action) = action {
                    window.dispatch_action(action, cx);
                }
            }
            Ok(Some(message)) => {
                struct ExtensionCommandOutput;
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<ExtensionCommandOutput>(), message),
                    cx,
                );
            }
            Ok(None) => {}
            Err(error) => {
                struct ExtensionCommandError;
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<ExtensionCommandError>(), error.to_string()),
                    cx,
                );
            }
        })
        .ok();
    })
    .detach();
}

fn copilot_plugin(cx: &App) -> Option<&extension_host::wasm_host::WasmExtension> {
    ExtensionStore::try_global(cx)?
        .read(cx)
        .wasm_extensions
        .iter()
        .find(|(_, manifest, extension)| {
            extension.has_run_command
                && manifest.id.as_ref() == "github-copilot"
                && manifest.inline_completion_provider == Some(InlineCompletionProvider::GithubCopilot)
        })
        .map(|(_, _, extension)| extension)
}

pub fn observe_copilot(cx: &mut App) {
    let Some(store) = ExtensionStore::try_global(cx) else {
        return;
    };
    let sync = |cx: &mut App| {
        let installed = copilot_plugin(cx).is_some();
        if let Some(service) = copilot::Copilot::global(cx) {
            service.update(cx, |service, cx| service.set_plugin_installed(installed, cx));
        }
    };
    sync(cx);
    cx.observe(&store, move |_, cx| sync(cx)).detach();
}

pub fn copilot_release(cx: &mut App) -> gpui::Task<anyhow::Result<copilot::ServerRelease>> {
    let Some(extension) = copilot_plugin(cx) else {
        return gpui::Task::ready(Err(anyhow::anyhow!("GitHub Copilot extension is not installed")));
    };
    let release = extension.run_command("server-release".into(), None);
    cx.background_spawn(async move {
        let release = release
            .await?
            .ok_or_else(|| anyhow::anyhow!("No Copilot release provided"))?;
        Ok(serde_json::from_str(&release)?)
    })
}
