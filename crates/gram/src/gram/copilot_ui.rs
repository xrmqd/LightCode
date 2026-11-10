use copilot::{Copilot, Status};
use editor::Editor;
use gpui::{Corner, Entity, PromptLevel, Subscription, WeakEntity};
use settings::Settings as _;
use ui::{Button, ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::{StatusBarSettings, StatusItemView, Workspace, item::ItemHandle};

pub(super) fn install(workspace: &mut Workspace, cx: &mut Context<Workspace>) -> Option<Entity<CopilotStatusItem>> {
    let service = Copilot::global(cx)?;
    workspace.register_action(|_, _: &copilot::SignIn, window, cx| enable_or_sign_in(true, window, cx));
    workspace.register_action(|_, _: &copilot::Enable, window, cx| enable_or_sign_in(false, window, cx));
    workspace.register_action(|_, _: &copilot::Disable, _, cx| {
        if let Some(service) = Copilot::global(cx) {
            service.update(cx, |service, cx| service.set_enabled(false, cx));
        }
    });
    workspace.register_action(|_, _: &copilot::SignOut, _, cx| {
        if let Some(service) = Copilot::global(cx) {
            service.update(cx, Copilot::sign_out);
        }
    });
    workspace.register_action(|_, _: &copilot::Snooze, _, cx| {
        if let Some(service) = Copilot::global(cx) {
            service.update(cx, Copilot::snooze);
        }
    });
    workspace.register_action(|_, _: &copilot::Resume, _, cx| {
        if let Some(service) = Copilot::global(cx) {
            service.update(cx, Copilot::resume);
        }
    });
    workspace.register_action(|workspace, _: &copilot::ToggleLanguage, _, cx| {
        if let Some(service) = Copilot::global(cx)
            && let Some(language) = active_language(workspace, cx)
        {
            service.update(cx, |service, cx| service.toggle_language(language, cx));
        }
    });
    workspace.register_action(|_, _: &copilot::ResolveStatus, _, cx| {
        if let Some(service) = Copilot::global(cx) {
            service.update(cx, Copilot::resolve_status);
        }
    });
    Some(cx.new(|cx| CopilotStatusItem {
        workspace: workspace.weak_handle(),
        _observe_service: cx.observe(&service, |_, _, cx| cx.notify()),
        service,
    }))
}

fn active_language(workspace: &Workspace, cx: &App) -> Option<String> {
    workspace.active_item_as::<Editor>(cx)?.read(cx).copilot_language(cx)
}

fn enable_or_sign_in(sign_in: bool, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(service) = Copilot::global(cx) else {
        return;
    };
    if service.read(cx).enabled() {
        service.update(cx, |service, cx| {
            if sign_in {
                service.sign_in(window, cx);
            } else {
                service.set_enabled(true, cx);
            }
        });
        return;
    }
    let answer = window.prompt(
        PromptLevel::Info,
        "Enable GitHub Copilot?",
        Some("LightCode will install the official GitHub Copilot component if needed. Copilot sends code context from eligible files to GitHub while suggestions are enabled."),
        &["Enable Copilot", "Cancel"],
        cx,
    );
    cx.spawn_in(window, async move |_, cx| {
        if answer.await.ok() != Some(0) {
            return;
        }
        cx.update(|window, cx| {
            service.update(cx, |service, cx| {
                if sign_in {
                    service.sign_in(window, cx);
                } else {
                    service.set_enabled(true, cx);
                }
            });
        })
        .ok();
    })
    .detach();
}

pub(super) struct CopilotStatusItem {
    service: Entity<Copilot>,
    workspace: WeakEntity<Workspace>,
    _observe_service: Subscription,
}

impl Render for CopilotStatusItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.clone();
        let workspace = self.workspace.clone();
        let status = service.read(cx).status();
        let language_off = workspace
            .upgrade()
            .and_then(|workspace| active_language(workspace.read(cx), cx))
            .is_some_and(|language| !service.read(cx).language_enabled(&language));
        let label = match status {
            Status::Disabled => "Copilot: Off",
            Status::Starting => "Copilot: Starting…",
            Status::SignedOut => "Copilot: Sign In",
            Status::SigningIn => "Copilot: Signing In…",
            Status::Ready if language_off => "Copilot: Language Off",
            Status::Ready => "Copilot",
            Status::Paused => "Copilot: Paused",
            Status::Error(_) => "Copilot: Error",
        };
        let status_bar = StatusBarSettings::get_global(cx);
        let anchor = match status_bar.position {
            settings::StatusBarPosition::Bottom => Corner::BottomRight,
            settings::StatusBarPosition::Top => Corner::TopRight,
        };

        PopoverMenu::new("github-copilot")
            .anchor(anchor)
            .menu(move |window, cx| {
                let copilot = service.read(cx);
                let status = copilot.status();
                let enabled = copilot.enabled();
                let message = copilot.message().map(str::to_owned);
                let status_command = copilot.has_status_command();
                let language_toggle = workspace
                    .upgrade()
                    .and_then(|workspace| active_language(workspace.read(cx), cx))
                    .map(|lang| {
                        let verb = if copilot.language_enabled(&lang) {
                            "Disable"
                        } else {
                            "Enable"
                        };
                        format!("{verb} for {lang}")
                    });
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    menu = menu.header("GitHub Copilot");
                    if let Status::Error(error) = &status {
                        menu = menu.header(error.clone());
                    } else if let Some(message) = message {
                        menu = menu.header(message);
                    }
                    if matches!(status, Status::Disabled | Status::SignedOut | Status::Error(_)) {
                        menu = menu.action("Sign in to GitHub", Box::new(copilot::SignIn));
                    }
                    if !enabled {
                        return menu.action("Enable Copilot", Box::new(copilot::Enable));
                    }
                    if matches!(status, Status::Ready) {
                        menu = menu
                            .action("Trigger Suggestion", Box::new(copilot::Suggest))
                            .action("Pause for 10 Minutes", Box::new(copilot::Snooze));
                    } else if matches!(status, Status::Paused) {
                        menu = menu.action("Resume Suggestions", Box::new(copilot::Resume));
                    }
                    if let Some(label) = language_toggle {
                        menu = menu.action(label, Box::new(copilot::ToggleLanguage));
                    }
                    if status_command {
                        menu = menu.action("View Copilot Status Details", Box::new(copilot::ResolveStatus));
                    }
                    if matches!(status, Status::Ready | Status::Paused) {
                        menu = menu.action("Sign Out", Box::new(copilot::SignOut));
                    }
                    menu.separator().action("Disable Copilot", Box::new(copilot::Disable))
                }))
            })
            .trigger_with_tooltip(
                Button::new("copilot-status", label).label_size(status_bar.icon_size.label_size()),
                |_, cx| {
                    Tooltip::with_meta(
                        "GitHub Copilot",
                        None,
                        "Manage inline suggestions and your GitHub account",
                        cx,
                    )
                },
            )
    }
}

impl StatusItemView for CopilotStatusItem {
    fn set_active_pane_item(&mut self, _: Option<&dyn ItemHandle>, _: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }
}
