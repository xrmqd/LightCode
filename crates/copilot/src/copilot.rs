mod protocol;

pub use protocol::{Completion, Position, ghost_text, position_at};

use anyhow::{Context as _, Result, anyhow, ensure};
use collections::{BTreeSet, HashMap};
use fs::Fs;
use futures::AsyncReadExt as _;
use gpui::{
    App, AppContext as _, AsyncApp, ClipboardItem, Context, Entity, EntityId, Global, PromptLevel, Subscription, Task,
    Window, actions,
};
use http_client::HttpClient;
use lsp::{LanguageServer, LanguageServerBinary, LanguageServerId, LanguageServerName, Uri};
use parking_lot::Mutex;
use serde_json::{Value, json};
use settings::{Settings, SettingsContent, SettingsStore, update_settings_file};
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const SERVER_VERSION: &str = "1.567.1";
const MAX_ARCHIVE_SIZE: u64 = 150_000_000;
pub const MAX_DOCUMENT_SIZE: usize = 1_000_000;
pub const SNOOZE_DURATION: Duration = Duration::from_secs(10 * 60);

actions!(
    copilot,
    [
        /// Sign into GitHub Copilot.
        SignIn,
        /// Sign out of GitHub Copilot.
        SignOut,
        /// Enable GitHub Copilot suggestions.
        Enable,
        /// Disable GitHub Copilot suggestions.
        Disable,
        /// Pause GitHub Copilot for ten minutes.
        Snooze,
        /// Resume GitHub Copilot suggestions.
        Resume,
        /// Toggle Copilot for the active editor language.
        ToggleLanguage,
        /// Open the action offered by the Copilot server.
        ResolveStatus,
        /// Ask Copilot for a suggestion at the cursor.
        Suggest,
        /// Accept the visible Copilot suggestion.
        Accept,
        /// Show the next Copilot suggestion.
        NextSuggestion,
        /// Show the previous Copilot suggestion.
        PreviousSuggestion,
        /// Dismiss the current Copilot suggestion.
        Dismiss,
    ]
);

macro_rules! rpc {
    ($name:ident, $method:literal, $result:ty) => {
        enum $name {}
        impl lsp::request::Request for $name {
            type Params = Value;
            type Result = $result;
            const METHOD: &'static str = $method;
        }
    };
}

macro_rules! notice {
    ($name:ident, $method:literal) => {
        enum $name {}
        impl lsp::notification::Notification for $name {
            type Params = Value;
            const METHOD: &'static str = $method;
        }
    };
}

rpc!(CheckStatus, "checkStatus", Value);
rpc!(SignInRequest, "signIn", Value);
rpc!(SignOutRequest, "signOut", Value);
rpc!(ExecuteCommand, "workspace/executeCommand", Value);
rpc!(InlineCompletion, "textDocument/inlineCompletion", Value);
notice!(DidChangeStatus, "didChangeStatus");
notice!(DidOpen, "textDocument/didOpen");
notice!(DidChange, "textDocument/didChange");
notice!(DidClose, "textDocument/didClose");
notice!(DidFocus, "textDocument/didFocus");
notice!(DidShowCompletion, "textDocument/didShowCompletion");

#[derive(Clone, Debug, PartialEq, Eq)]
struct CopilotSettings {
    enabled: bool,
    disabled_languages: Vec<String>,
    server_path: Option<PathBuf>,
    debounce_ms: u64,
}

impl Settings for CopilotSettings {
    fn from_settings(content: &SettingsContent) -> Self {
        let settings = content.copilot.clone().unwrap_or_default();
        Self {
            enabled: settings.enabled.unwrap_or(false),
            disabled_languages: settings.disabled_languages.unwrap_or_default(),
            server_path: settings.server_path.map(PathBuf::from),
            debounce_ms: settings.debounce_ms.unwrap_or(300).clamp(100, 2000),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Disabled,
    Starting,
    SignedOut,
    SigningIn,
    Ready,
    Paused,
    Error(String),
}

struct GlobalCopilot(Entity<Copilot>);
impl Global for GlobalCopilot {}

struct Document {
    text: String,
    language: String,
    version: i32,
    workspace: Option<Uri>,
}

pub struct DocumentInput {
    pub uri: Uri,
    pub workspace: Option<Uri>,
    pub language: String,
    pub text: String,
    pub position: Position,
    pub tab_size: u32,
    pub insert_spaces: bool,
}

pub struct Copilot {
    fs: Arc<dyn Fs>,
    http: Arc<dyn HttpClient>,
    settings: CopilotSettings,
    server: Option<Arc<LanguageServer>>,
    documents: HashMap<Uri, Document>,
    owners: HashMap<EntityId, Uri>,
    focused: Option<Uri>,
    authorized: bool,
    starting: bool,
    signing_in: bool,
    paused: bool,
    error: Option<String>,
    server_message: Option<String>,
    server_command: Option<Value>,
    epoch: u64,
    start_task: Option<Task<()>>,
    auth_task: Option<Task<()>>,
    resume_task: Option<Task<()>>,
    _settings_subscription: Subscription,
    _server_handlers: Vec<lsp::Subscription>,
}

pub fn init(fs: Arc<dyn Fs>, http: Arc<dyn HttpClient>, cx: &mut App) {
    CopilotSettings::register(cx);
    let service = cx.new(|cx: &mut Context<Copilot>| {
        let settings = CopilotSettings::get_global(cx).clone();
        let mut service = Copilot {
            fs,
            http,
            settings,
            server: None,
            documents: HashMap::default(),
            owners: HashMap::default(),
            focused: None,
            authorized: false,
            starting: false,
            signing_in: false,
            paused: false,
            error: None,
            server_message: None,
            server_command: None,
            epoch: 0,
            start_task: None,
            auth_task: None,
            resume_task: None,
            _settings_subscription: cx.observe_global::<SettingsStore>(|this, cx| this.reload_settings(cx)),
            _server_handlers: Vec::new(),
        };
        if service.settings.enabled {
            service.start(cx);
        }
        service
    });
    cx.set_global(GlobalCopilot(service));
}

impl Copilot {
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalCopilot>().map(|global| global.0.clone())
    }

    pub fn status(&self) -> Status {
        if !self.settings.enabled {
            Status::Disabled
        } else if self.paused {
            Status::Paused
        } else if self.starting {
            Status::Starting
        } else if self.signing_in {
            Status::SigningIn
        } else if let Some(error) = &self.error {
            Status::Error(error.clone())
        } else if self.authorized {
            Status::Ready
        } else {
            Status::SignedOut
        }
    }

    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn language_enabled(&self, language: &str) -> bool {
        !self
            .settings
            .disabled_languages
            .iter()
            .any(|disabled| disabled.eq_ignore_ascii_case(language))
    }

    pub fn can_complete(&self, language: &str) -> bool {
        self.ready_for_completions() && self.language_enabled(language)
    }

    pub fn ready_for_completions(&self) -> bool {
        self.settings.enabled && self.authorized && !self.paused && !self.signing_in && self.server.is_some()
    }

    pub fn debounce(&self) -> Duration {
        Duration::from_millis(self.settings.debounce_ms)
    }

    pub fn message(&self) -> Option<&str> {
        self.server_message.as_deref()
    }

    pub fn has_status_command(&self) -> bool {
        self.server_command.is_some()
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        update_settings_file(
            self.fs.clone(),
            cx,
            Box::new(move |settings, _| {
                settings.copilot.get_or_insert_with(Default::default).enabled = Some(enabled);
            }),
        );
        self.settings.enabled = enabled;
        if enabled {
            self.start(cx);
        } else {
            self.stop(cx);
        }
    }

    pub fn toggle_language(&mut self, language: String, cx: &mut Context<Self>) {
        if self.language_enabled(&language) {
            self.settings.disabled_languages.push(language);
        } else {
            self.settings
                .disabled_languages
                .retain(|disabled| !disabled.eq_ignore_ascii_case(&language));
        }
        let disabled = self.settings.disabled_languages.clone();
        update_settings_file(
            self.fs.clone(),
            cx,
            Box::new(move |settings, _| {
                settings.copilot.get_or_insert_with(Default::default).disabled_languages = Some(disabled);
            }),
        );
        cx.notify();
    }

    pub fn snooze(&mut self, cx: &mut Context<Self>) {
        if !self.settings.enabled {
            return;
        }
        self.paused = true;
        self.close_documents();
        self.resume_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SNOOZE_DURATION).await;
            this.update(cx, |this, cx| {
                this.paused = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.resume_task.take();
        self.paused = false;
        cx.notify();
    }

    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        let settings = CopilotSettings::get_global(cx).clone();
        if settings == self.settings {
            return;
        }
        let server_changed = self.settings.server_path != settings.server_path;
        let enabled_changed = self.settings.enabled != settings.enabled;
        self.settings = settings;
        if !self.settings.enabled || server_changed {
            self.stop(cx);
        }
        if self.settings.enabled && (enabled_changed || server_changed) {
            self.start(cx);
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.epoch = self.epoch.wrapping_add(1);
        self.start_task.take();
        self.auth_task.take();
        self.resume_task.take();
        self.close_documents();
        self._server_handlers.clear();
        if let Some(server) = self.server.take()
            && let Some(shutdown) = server.shutdown()
        {
            cx.background_spawn(shutdown).detach();
        }
        self.authorized = false;
        self.starting = false;
        self.signing_in = false;
        self.paused = false;
        self.error = None;
        self.server_message = None;
        self.server_command = None;
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if !self.settings.enabled || self.server.is_some() || self.starting {
            return;
        }
        self.starting = true;
        self.error = None;
        let epoch = self.epoch;
        let http = self.http.clone();
        let path = self.settings.server_path.clone();
        self.start_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let install_dir = paths::data_dir().join("copilot");
                let root = install_dir.clone();
                let binary = cx
                    .background_spawn(async move { install_binary(&install_dir, path, http).await })
                    .await?;
                let config = root.join("config");
                smol::fs::create_dir_all(&config).await?;
                let server = LanguageServer::new(
                    Arc::new(Mutex::new(None)),
                    LanguageServerId(usize::MAX),
                    LanguageServerName::new_static("GitHub Copilot"),
                    LanguageServerBinary {
                        path: binary,
                        arguments: vec!["--stdio".into()],
                        env: Some(HashMap::from_iter([(
                            "XDG_CONFIG_HOME".to_owned(),
                            config.to_string_lossy().into_owned(),
                        )])),
                    },
                    &root,
                    None,
                    Some(Arc::new(Mutex::new(BTreeSet::default()))),
                    cx,
                )?;
                let handlers = vec![
                    server.on_notification::<DidChangeStatus, _>({
                        let this = this.clone();
                        move |params, cx| {
                            this.update(cx, |this, cx| {
                                if this.epoch == epoch {
                                    this.on_server_status(params, cx);
                                }
                            })
                            .ok();
                        }
                    }),
                    server.on_request::<lsp::request::ShowDocument, _, _>(|params, cx| {
                        let uri = params.uri.to_string();
                        let success = uri.starts_with("https://") && cx.update(|cx| cx.open_url(&uri)).is_ok();
                        async move { Ok(lsp::ShowDocumentResult { success }) }
                    }),
                    server.on_request::<lsp::request::ShowMessageRequest, _, _>(show_message),
                    server.on_request::<lsp::request::WorkspaceConfiguration, _, _>(|params, _| async move {
                        Ok(params
                            .items
                            .iter()
                            .map(|item| configuration_section(item.section.as_deref()))
                            .collect())
                    }),
                ];
                let mut params = cx.update(|cx| server.default_initialize_params(false, cx))?;
                #[allow(deprecated)]
                {
                    params.root_uri = None;
                }
                params.workspace_folders = Some(Vec::new());
                params.client_info = Some(lsp::ClientInfo {
                    name: "LightCode".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                });
                params
                    .capabilities
                    .window
                    .get_or_insert_with(Default::default)
                    .show_document = Some(lsp::ShowDocumentClientCapabilities { support: true });
                params.initialization_options = Some(json!({
                    "editorInfo": {"name":"LightCode","version":env!("CARGO_PKG_VERSION")},
                    "editorPluginInfo": {"name":"LightCode Copilot","version":env!("CARGO_PKG_VERSION")},
                }));
                let configuration = Arc::new(lsp::DidChangeConfigurationParams {
                    settings: server_configuration(),
                });
                let server = cx
                    .update(|cx| server.initialize(params, configuration.clone(), cx))?
                    .await?;
                server.notify::<lsp::notification::DidChangeConfiguration>((*configuration).clone())?;
                let authorized = check_authorized(&server).await;
                anyhow::Ok((server, handlers, authorized))
            }
            .await;
            this.update(cx, |this, cx| {
                if this.epoch != epoch || !this.settings.enabled {
                    return;
                }
                this.starting = false;
                match result {
                    Ok((server, handlers, authorized)) => {
                        this.server = Some(server);
                        this._server_handlers = handlers;
                        this.authorized = authorized;
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_server_status(&mut self, params: Value, cx: &mut Context<Self>) {
        self.server_message = params
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        self.server_command = params.get("command").filter(|v| v.is_object()).cloned();
        // Status notifications also report transient warnings and ignored files.
        // Do not treat those as loss of the GitHub session.
        if params.get("kind").and_then(Value::as_str) == Some("Error")
            || matches!(
                params.get("status").and_then(Value::as_str),
                Some("NotSignedIn" | "NotAuthorized")
            )
        {
            self.authorized = false;
            self.close_documents();
        }
        cx.notify();
    }

    pub fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_in || self.authorized {
            return;
        }
        if !self.settings.enabled {
            self.set_enabled(true, cx);
        }
        let epoch = self.epoch;
        self.signing_in = true;
        self.error = None;
        self.start(cx);
        self.auth_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let server = loop {
                    let (server, starting, error) = this.update(cx, |this, _| (this.server.clone(), this.starting, this.error.clone()))?;
                    if let Some(server) = server {
                        break server;
                    }
                    if !starting {
                        return Err(anyhow!(error.unwrap_or_else(|| "Copilot server is not running".into())));
                    }
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                };
                if check_authorized(&server).await {
                    return Ok(true);
                }
                let response = server.request::<SignInRequest>(json!({})).await.into_response()?;
                if is_authorized(&response) {
                    return Ok(true);
                }
                let user_code = response.get("userCode").and_then(Value::as_str).context("GitHub did not return a device code")?;
                let command = response.get("command").cloned().context("GitHub did not return a sign-in command")?;
                let answer = cx.update(|window, cx| {
                    let detail = format!("Enter this one-time code on GitHub: {user_code}\n\nUse the GitHub account that has Copilot access.");
                    window.prompt(PromptLevel::Info, "Sign into GitHub Copilot", Some(&detail), &["Copy Code and Open GitHub", "Cancel"], cx)
                })?.await.ok();
                if answer != Some(0) {
                    return Ok(false);
                }
                cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(user_code.to_owned())))?;
                let response = server.request_with_timer::<ExecuteCommand, _>(command, async {
                    smol::Timer::after(Duration::from_secs(10 * 60)).await;
                    "GitHub sign-in timed out".to_owned()
                }).await.into_response()?;
                if is_authorized(&response) || check_authorized(&server).await {
                    Ok(true)
                } else {
                    Err(anyhow!("GitHub has not authorized Copilot for this account. Check your Copilot access and try again."))
                }
            }.await;
            this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.signing_in = false;
                match result {
                    Ok(authorized) => this.authorized |= authorized,
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            }).ok();
        }));
        cx.notify();
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.auth_task.take();
        self.authorized = false;
        self.signing_in = false;
        self.error = None;
        self.close_documents();
        if let Some(server) = self.server.clone() {
            let epoch = self.epoch;
            self.auth_task = Some(cx.spawn(async move |this, cx| {
                let result = server.request::<SignOutRequest>(json!({})).await.into_response();
                this.update(cx, |this, cx| {
                    if this.epoch == epoch {
                        if let Err(error) = result {
                            this.error = Some(format!("Sign out failed: {error:#}"));
                        }
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        cx.notify();
    }

    pub fn resolve_status(&mut self, cx: &mut Context<Self>) {
        if let (Some(server), Some(command)) = (self.server.clone(), self.server_command.clone()) {
            cx.background_spawn(async move {
                server.request::<ExecuteCommand>(command).await.into_response().ok();
            })
            .detach();
        }
    }

    pub fn completions(
        &mut self,
        owner: EntityId,
        input: DocumentInput,
        manual: bool,
        cx: &App,
    ) -> Task<Result<Vec<Completion>>> {
        let Some(server) = self.server.clone().filter(|_| self.can_complete(&input.language)) else {
            return Task::ready(Ok(Vec::new()));
        };
        let version = match self.sync_document(owner, &server, &input) {
            Ok(version) => version,
            Err(error) => return Task::ready(Err(error)),
        };
        let params = json!({
            "textDocument":{"uri":input.uri,"version":version},
            "position":input.position,
            "context":{"triggerKind":if manual {1} else {2}},
            "formattingOptions":{"tabSize":input.tab_size,"insertSpaces":input.insert_spaces},
        });
        cx.background_spawn(async move {
            let result = server.request::<InlineCompletion>(params).await.into_response()?;
            Ok(protocol::completions_from_response(result))
        })
    }

    fn sync_document(&mut self, owner: EntityId, server: &LanguageServer, input: &DocumentInput) -> Result<i32> {
        if self.owners.get(&owner) != Some(&input.uri) {
            self.release_document(owner);
            self.owners.insert(owner, input.uri.clone());
        }
        if self
            .documents
            .get(&input.uri)
            .is_some_and(|doc| doc.language != input.language)
        {
            server.notify::<DidClose>(json!({"textDocument":{"uri":input.uri}}))?;
            self.documents.remove(&input.uri);
        }
        let version = if let Some(document) = self.documents.get_mut(&input.uri) {
            if let Some(change) = protocol::incremental_change(&document.text, &input.text) {
                let version = document
                    .version
                    .checked_add(1)
                    .context("Copilot document version overflow")?;
                server.notify::<DidChange>(json!({
                    "textDocument":{"uri":input.uri,"version":version},
                    "contentChanges":[change],
                }))?;
                document.text.clone_from(&input.text);
                document.version = version;
            }
            document.version
        } else {
            if let Some(workspace) = &input.workspace {
                server.add_workspace_folder(workspace.clone());
            }
            server.notify::<DidOpen>(json!({
                "textDocument":{"uri":input.uri,"languageId":input.language,"version":1,"text":input.text},
            }))?;
            self.documents.insert(
                input.uri.clone(),
                Document {
                    text: input.text.clone(),
                    language: input.language.clone(),
                    version: 1,
                    workspace: input.workspace.clone(),
                },
            );
            1
        };
        if self.focused.as_ref() != Some(&input.uri) {
            server.notify::<DidFocus>(json!({"textDocument":{"uri":input.uri}}))?;
            self.focused = Some(input.uri.clone());
        }
        Ok(version)
    }

    pub fn release_document(&mut self, owner: EntityId) {
        let Some(uri) = self.owners.remove(&owner) else {
            return;
        };
        if self.owners.values().any(|open| open == &uri) {
            return;
        }
        if let Some(doc) = self.documents.remove(&uri)
            && let Some(server) = &self.server
        {
            server.notify::<DidClose>(json!({"textDocument":{"uri":uri}})).ok();
            if let Some(workspace) = doc.workspace
                && !self
                    .documents
                    .values()
                    .any(|doc| doc.workspace.as_ref() == Some(&workspace))
            {
                server.remove_workspace_folder(workspace);
            }
        }
        if self.focused.as_ref() == Some(&uri) {
            self.focused = None;
        }
    }

    fn close_documents(&mut self) {
        for owner in self.owners.keys().copied().collect::<Vec<_>>() {
            self.release_document(owner);
        }
    }

    pub fn shown(&self, completion: &Completion) {
        if let Some(server) = self.server.as_ref().filter(|_| self.authorized && !self.paused) {
            server.notify::<DidShowCompletion>(json!({"item":completion})).ok();
        }
    }

    pub fn accepted(&self, completion: &Completion, cx: &App) {
        if let Some(server) = self.server.clone().filter(|_| self.authorized && !self.paused)
            && let Some(command) = completion.command.clone()
            && command.get("command").and_then(Value::as_str) == Some("github.copilot.didAcceptCompletionItem")
        {
            cx.background_spawn(async move {
                server.request::<ExecuteCommand>(command).await.into_response().ok();
            })
            .detach();
        }
    }
}

fn server_configuration() -> Value {
    json!({"telemetry":{"telemetryLevel":"off"},"http":{"proxyStrictSSL":true}})
}

fn configuration_section(section: Option<&str>) -> Value {
    let mut value = server_configuration();
    if let Some(section) = section {
        for key in section.split('.') {
            value = value.get(key).cloned().unwrap_or(Value::Null);
        }
    }
    value
}

async fn check_authorized(server: &LanguageServer) -> bool {
    server
        .request::<CheckStatus>(json!({"localChecksOnly":false}))
        .await
        .into_response()
        .is_ok_and(|response| is_authorized(&response))
}

fn is_authorized(response: &Value) -> bool {
    matches!(
        response.get("status").and_then(Value::as_str),
        Some("OK" | "AlreadySignedIn")
    )
}

fn show_message(
    params: lsp::ShowMessageRequestParams,
    cx: &mut AsyncApp,
) -> impl Future<Output = Result<Option<lsp::MessageActionItem>>> + 'static + use<> {
    let actions = params.actions.unwrap_or_default();
    let answer = cx.update(|cx| {
        let window = cx.active_window()?;
        window
            .update(cx, |_, window, cx| {
                let labels = actions
                    .iter()
                    .map(|item| item.title.as_str())
                    .chain(["Dismiss"])
                    .collect::<Vec<_>>();
                let level = match params.typ {
                    lsp::MessageType::ERROR => PromptLevel::Critical,
                    lsp::MessageType::WARNING => PromptLevel::Warning,
                    _ => PromptLevel::Info,
                };
                window.prompt(level, "GitHub Copilot", Some(&params.message), &labels, cx)
            })
            .ok()
    });
    async move {
        let Some(answer) = answer? else {
            return Ok(None);
        };
        Ok(answer.await.ok().and_then(|index| actions.get(index).cloned()))
    }
}

async fn install_binary(root: &Path, configured: Option<PathBuf>, http: Arc<dyn HttpClient>) -> Result<PathBuf> {
    smol::fs::create_dir_all(root).await?;
    if let Some(path) = configured {
        ensure!(
            path.is_absolute() && smol::fs::metadata(&path).await.is_ok_and(|m| m.is_file()),
            "The configured Copilot server path must be an absolute path to a file"
        );
        return Ok(path);
    }
    let (platform, checksum) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => (
            "darwin-arm64",
            "a0b6377a22ebf2dca9e80adbb26c7dd61f5b867233050635e0b1ac0868a6bd75",
        ),
        ("macos", "x86_64") => (
            "darwin-x64",
            "935358bdd66ab8b470808debac1c4222e2a1ea733215c599008378e9a9e754dc",
        ),
        _ => anyhow::bail!(
            "Automatic Copilot server installation is available on macOS. Set copilot.server_path to use an installed server."
        ),
    };
    let target = root.join(format!("{SERVER_VERSION}-{platform}"));
    let executable = target.join("copilot-language-server");
    if smol::fs::metadata(&executable).await.is_ok_and(|m| m.is_file()) {
        return Ok(executable);
    }
    let url = format!(
        "https://github.com/github/copilot-language-server-release/releases/download/{SERVER_VERSION}/copilot-language-server-{platform}-{SERVER_VERSION}.zip"
    );
    let mut response = http
        .get(&url, Default::default(), true)
        .await
        .context("Downloading the official GitHub Copilot language server")?;
    ensure!(
        response.status().is_success(),
        "Copilot server download failed: HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    response
        .body_mut()
        .take(MAX_ARCHIVE_SIZE + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() as u64 <= MAX_ARCHIVE_SIZE,
        "Copilot server download exceeded the size limit"
    );
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == checksum,
        "Copilot server archive failed its SHA-256 check"
    );
    let staging = root.join(format!(".{SERVER_VERSION}-{platform}.installing"));
    if staging.exists() {
        smol::fs::remove_dir_all(&staging).await?;
    }
    smol::fs::create_dir(&staging).await?;
    util::archive::extract_zip(&staging, futures::io::Cursor::new(bytes)).await?;
    let binary = staging.join("copilot-language-server");
    ensure!(
        smol::fs::metadata(&binary).await.is_ok_and(|m| m.is_file()),
        "Copilot server executable is missing from the archive"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        smol::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).await?;
    }
    smol::fs::rename(staging, target).await?;
    Ok(executable)
}
