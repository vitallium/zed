use crate::{
    LocationLink,
    lsp_command::{
        LspCommand, file_path_to_lsp_url, language_server_for_buffer, location_link_from_lsp,
        location_link_from_proto, location_link_to_proto, location_links_from_lsp,
        location_links_from_proto, location_links_to_proto, make_text_document_identifier,
    },
    lsp_store::{LocalLspStore, LspStore},
    make_lsp_text_document_position,
};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use collections::HashMap;
use gpui::{App, AsyncApp, Entity};
use language::{
    Buffer, Transaction, point_to_lsp,
    proto::{deserialize_anchor, deserialize_version, serialize_anchor, serialize_version},
};
use lsp::{AdapterServerCapabilities, LanguageServer, LanguageServerId};
use rpc::proto::{self, PeerId};
use serde::{Deserialize, Serialize};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};
use task::TaskTemplate;
use text::{Anchor, BufferId, PointUtf16, ToPointUtf16};

pub enum LspExtExpandMacro {}

impl lsp::request::Request for LspExtExpandMacro {
    type Params = ExpandMacroParams;
    type Result = Option<ExpandedMacro>;
    const METHOD: &'static str = "rust-analyzer/expandMacro";
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExpandMacroParams {
    pub text_document: lsp::TextDocumentIdentifier,
    pub position: lsp::Position,
}

#[derive(Default, Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExpandedMacro {
    pub name: String,
    pub expansion: String,
}

impl ExpandedMacro {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.expansion.is_empty()
    }
}
#[derive(Debug)]
pub struct ExpandMacro {
    pub position: PointUtf16,
}

#[async_trait(?Send)]
impl LspCommand for ExpandMacro {
    type Response = ExpandedMacro;
    type LspRequest = LspExtExpandMacro;
    type ProtoRequest = proto::LspExtExpandMacro;

    fn display_name(&self) -> &str {
        "Expand macro"
    }

    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<ExpandMacroParams> {
        Ok(ExpandMacroParams {
            text_document: make_text_document_identifier(path)?,
            position: point_to_lsp(self.position),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<ExpandedMacro>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> anyhow::Result<ExpandedMacro> {
        Ok(message
            .map(|message| ExpandedMacro {
                name: message.name,
                expansion: message.expansion,
            })
            .unwrap_or_default())
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LspExtExpandMacro {
        proto::LspExtExpandMacro {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
        }
    }

    async fn from_proto(
        message: Self::ProtoRequest,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        cx: AsyncApp,
    ) -> anyhow::Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        response: ExpandedMacro,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::LspExtExpandMacroResponse {
        proto::LspExtExpandMacroResponse {
            name: response.name,
            expansion: response.expansion,
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LspExtExpandMacroResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> anyhow::Result<ExpandedMacro> {
        Ok(ExpandedMacro {
            name: message.name,
            expansion: message.expansion,
        })
    }

    fn buffer_id_from_proto(message: &proto::LspExtExpandMacro) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

pub enum LspOpenDocs {}

impl lsp::request::Request for LspOpenDocs {
    type Params = OpenDocsParams;
    type Result = Option<DocsUrls>;
    const METHOD: &'static str = "experimental/externalDocs";
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OpenDocsParams {
    pub text_document: lsp::TextDocumentIdentifier,
    pub position: lsp::Position,
}

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DocsUrls {
    pub web: Option<String>,
    pub local: Option<String>,
}

impl DocsUrls {
    pub fn is_empty(&self) -> bool {
        self.web.is_none() && self.local.is_none()
    }
}

#[derive(Debug)]
pub struct OpenDocs {
    pub position: PointUtf16,
}

#[async_trait(?Send)]
impl LspCommand for OpenDocs {
    type Response = DocsUrls;
    type LspRequest = LspOpenDocs;
    type ProtoRequest = proto::LspExtOpenDocs;

    fn display_name(&self) -> &str {
        "Open docs"
    }

    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<OpenDocsParams> {
        let uri = lsp::Uri::from_file_path(path)
            .map_err(|()| anyhow::anyhow!("{path:?} is not a valid URI"))?;
        Ok(OpenDocsParams {
            text_document: lsp::TextDocumentIdentifier { uri },
            position: point_to_lsp(self.position),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<DocsUrls>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> anyhow::Result<DocsUrls> {
        Ok(message
            .map(|message| DocsUrls {
                web: message.web,
                local: message.local,
            })
            .unwrap_or_default())
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LspExtOpenDocs {
        proto::LspExtOpenDocs {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
        }
    }

    async fn from_proto(
        message: Self::ProtoRequest,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        cx: AsyncApp,
    ) -> anyhow::Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        response: DocsUrls,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::LspExtOpenDocsResponse {
        proto::LspExtOpenDocsResponse {
            web: response.web,
            local: response.local,
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LspExtOpenDocsResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> anyhow::Result<DocsUrls> {
        Ok(DocsUrls {
            web: message.web,
            local: message.local,
        })
    }

    fn buffer_id_from_proto(message: &proto::LspExtOpenDocs) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

pub enum LspSwitchSourceHeader {}

impl lsp::request::Request for LspSwitchSourceHeader {
    type Params = SwitchSourceHeaderParams;
    type Result = Option<SwitchSourceHeaderResult>;
    const METHOD: &'static str = "textDocument/switchSourceHeader";
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSourceHeaderParams(lsp::TextDocumentIdentifier);

#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSourceHeaderResult(pub String);

#[derive(Default, Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSourceHeader;

#[derive(Debug)]
pub struct GoToParentModule {
    pub position: PointUtf16,
}

pub struct LspGoToParentModule {}

impl lsp::request::Request for LspGoToParentModule {
    type Params = lsp::TextDocumentPositionParams;
    type Result = Option<Vec<lsp::LocationLink>>;
    const METHOD: &'static str = "experimental/parentModule";
}

#[async_trait(?Send)]
impl LspCommand for SwitchSourceHeader {
    type Response = SwitchSourceHeaderResult;
    type LspRequest = LspSwitchSourceHeader;
    type ProtoRequest = proto::LspExtSwitchSourceHeader;

    fn display_name(&self) -> &str {
        "Switch source header"
    }

    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<SwitchSourceHeaderParams> {
        Ok(SwitchSourceHeaderParams(make_text_document_identifier(
            path,
        )?))
    }

    async fn response_from_lsp(
        self,
        message: Option<SwitchSourceHeaderResult>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> anyhow::Result<SwitchSourceHeaderResult> {
        Ok(message
            .map(|message| SwitchSourceHeaderResult(message.0))
            .unwrap_or_default())
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LspExtSwitchSourceHeader {
        proto::LspExtSwitchSourceHeader {
            project_id,
            buffer_id: buffer.remote_id().into(),
        }
    }

    async fn from_proto(
        _: Self::ProtoRequest,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> anyhow::Result<Self> {
        Ok(Self {})
    }

    fn response_to_proto(
        response: SwitchSourceHeaderResult,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::LspExtSwitchSourceHeaderResponse {
        proto::LspExtSwitchSourceHeaderResponse {
            target_file: response.0,
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LspExtSwitchSourceHeaderResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> anyhow::Result<SwitchSourceHeaderResult> {
        Ok(SwitchSourceHeaderResult(message.target_file))
    }

    fn buffer_id_from_proto(message: &proto::LspExtSwitchSourceHeader) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for GoToParentModule {
    type Response = Vec<LocationLink>;
    type LspRequest = LspGoToParentModule;
    type ProtoRequest = proto::LspExtGoToParentModule;

    fn display_name(&self) -> &str {
        "Go to parent module"
    }

    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::TextDocumentPositionParams> {
        make_lsp_text_document_position(path, self.position)
    }

    async fn response_from_lsp(
        self,
        links: Option<Vec<lsp::LocationLink>>,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        server_id: LanguageServerId,
        cx: AsyncApp,
    ) -> anyhow::Result<Vec<LocationLink>> {
        location_links_from_lsp(
            links.map(lsp::GotoDefinitionResponse::Link),
            lsp_store,
            buffer,
            server_id,
            cx,
        )
        .await
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LspExtGoToParentModule {
        proto::LspExtGoToParentModule {
            project_id,
            buffer_id: buffer.remote_id().to_proto(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
        }
    }

    async fn from_proto(
        request: Self::ProtoRequest,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        cx: AsyncApp,
    ) -> anyhow::Result<Self> {
        let position = request
            .position
            .and_then(deserialize_anchor)
            .context("bad request with bad position")?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        links: Vec<LocationLink>,
        lsp_store: &mut LspStore,
        peer_id: PeerId,
        _: &clock::Global,
        cx: &mut App,
    ) -> proto::LspExtGoToParentModuleResponse {
        proto::LspExtGoToParentModuleResponse {
            links: location_links_to_proto(links, lsp_store, peer_id, cx),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LspExtGoToParentModuleResponse,
        lsp_store: Entity<LspStore>,
        _: Entity<Buffer>,
        cx: AsyncApp,
    ) -> anyhow::Result<Vec<LocationLink>> {
        location_links_from_proto(message.links, lsp_store, cx).await
    }

    fn buffer_id_from_proto(message: &proto::LspExtGoToParentModule) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

// https://rust-analyzer.github.io/book/contributing/lsp-extensions.html#runnables
// Taken from https://github.com/rust-lang/rust-analyzer/blob/a73a37a757a58b43a796d3eb86a1f7dfd0036659/crates/rust-analyzer/src/lsp/ext.rs#L425-L489
pub enum Runnables {}

impl lsp::request::Request for Runnables {
    type Params = RunnablesParams;
    type Result = Vec<Runnable>;
    const METHOD: &'static str = "experimental/runnables";
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RunnablesParams {
    pub text_document: lsp::TextDocumentIdentifier,
    #[serde(default)]
    pub position: Option<lsp::Position>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Runnable {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<lsp::LocationLink>,
    pub kind: RunnableKind,
    pub args: RunnableArgs,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum RunnableArgs {
    Cargo(CargoRunnableArgs),
    Shell(ShellRunnableArgs),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum RunnableKind {
    Cargo,
    Shell,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CargoRunnableArgs {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub environment: HashMap<String, String>,
    pub cwd: PathBuf,
    /// Command to be executed instead of cargo
    #[serde(default)]
    pub override_cargo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    // command, --package and --lib stuff
    #[serde(default)]
    pub cargo_args: Vec<String>,
    // stuff after --
    #[serde(default)]
    pub executable_args: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShellRunnableArgs {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub environment: HashMap<String, String>,
    pub cwd: PathBuf,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug)]
pub struct GetLspRunnables {
    pub buffer_id: BufferId,
    pub position: Option<text::Anchor>,
}

#[derive(Debug, Default)]
pub struct LspRunnables {
    pub runnables: Vec<(Option<LocationLink>, TaskTemplate)>,
}

#[async_trait(?Send)]
impl LspCommand for GetLspRunnables {
    type Response = LspRunnables;
    type LspRequest = Runnables;
    type ProtoRequest = proto::LspExtRunnables;

    fn display_name(&self) -> &str {
        "LSP Runnables"
    }

    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        buffer: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<RunnablesParams> {
        let url = file_path_to_lsp_url(path)?;
        Ok(RunnablesParams {
            text_document: lsp::TextDocumentIdentifier::new(url),
            position: self
                .position
                .map(|anchor| point_to_lsp(anchor.to_point_utf16(&buffer.snapshot()))),
        })
    }

    async fn response_from_lsp(
        self,
        lsp_runnables: Vec<Runnable>,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        server_id: LanguageServerId,
        mut cx: AsyncApp,
    ) -> Result<LspRunnables> {
        let mut runnables = Vec::with_capacity(lsp_runnables.len());

        for runnable in lsp_runnables {
            let location = match runnable.location {
                Some(location) => Some(
                    location_link_from_lsp(location, &lsp_store, &buffer, server_id, &mut cx)
                        .await?,
                ),
                None => None,
            };
            let mut task_template = TaskTemplate::default();
            task_template.label = runnable.label;
            match runnable.args {
                RunnableArgs::Cargo(cargo) => {
                    match cargo.override_cargo {
                        Some(override_cargo) => {
                            let mut override_parts =
                                override_cargo.split(" ").map(|s| s.to_string());
                            task_template.command = override_parts
                                .next()
                                .unwrap_or_else(|| override_cargo.clone());
                            task_template.args.extend(override_parts);
                        }
                        None => task_template.command = "cargo".to_string(),
                    };
                    task_template.env = cargo.environment;
                    task_template.cwd = Some(
                        cargo
                            .workspace_root
                            .unwrap_or(cargo.cwd)
                            .to_string_lossy()
                            .to_string(),
                    );
                    task_template.args.extend(cargo.cargo_args);
                    if !cargo.executable_args.is_empty() {
                        let shell_kind = task_template.shell.shell_kind(cfg!(windows));
                        task_template.args.push("--".to_string());
                        task_template.args.extend(
                            cargo
                                .executable_args
                                .into_iter()
                                // rust-analyzer's doctest data may be smth. like
                                // ```
                                // command: "cargo",
                                // args: [
                                //     "test",
                                //     "--doc",
                                //     "--package",
                                //     "cargo-output-parser",
                                //     "--",
                                //     "X<T>::new",
                                //     "--show-output",
                                // ],
                                // ```
                                // and `X<T>::new` will cause troubles if not escaped properly, as later
                                // the task runs as `$SHELL -i -c "cargo test ..."`.
                                //
                                // We cannot escape all shell arguments unconditionally, as we use this for ssh commands, which may involve paths starting with `~`.
                                // That bit is not auto-expanded when using single quotes.
                                // Escape extra cargo args unconditionally as those are unlikely to contain `~`.
                                .flat_map(|extra_arg| {
                                    shell_kind.try_quote(&extra_arg).map(|s| s.to_string())
                                }),
                        );
                    }
                }
                RunnableArgs::Shell(shell) => {
                    task_template.command = shell.program;
                    task_template.args = shell.args;
                    task_template.env = shell.environment;
                    task_template.cwd = Some(shell.cwd.to_string_lossy().into_owned());
                }
            }

            runnables.push((location, task_template));
        }

        Ok(LspRunnables { runnables })
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LspExtRunnables {
        proto::LspExtRunnables {
            project_id,
            buffer_id: buffer.remote_id().to_proto(),
            position: self.position.as_ref().map(serialize_anchor),
        }
    }

    async fn from_proto(
        message: proto::LspExtRunnables,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Self> {
        let buffer_id = Self::buffer_id_from_proto(&message)?;
        let position = message.position.and_then(deserialize_anchor);
        Ok(Self {
            buffer_id,
            position,
        })
    }

    fn response_to_proto(
        response: LspRunnables,
        lsp_store: &mut LspStore,
        peer_id: PeerId,
        _: &clock::Global,
        cx: &mut App,
    ) -> proto::LspExtRunnablesResponse {
        proto::LspExtRunnablesResponse {
            runnables: response
                .runnables
                .into_iter()
                .map(|(location, task_template)| proto::LspRunnable {
                    location: location
                        .map(|location| location_link_to_proto(location, lsp_store, peer_id, cx)),
                    task_template: serde_json::to_vec(&task_template).unwrap(),
                })
                .collect(),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LspExtRunnablesResponse,
        lsp_store: Entity<LspStore>,
        _: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<LspRunnables> {
        let mut runnables = LspRunnables {
            runnables: Vec::new(),
        };

        for lsp_runnable in message.runnables {
            let location = match lsp_runnable.location {
                Some(location) => {
                    Some(location_link_from_proto(location, lsp_store.clone(), &mut cx).await?)
                }
                None => None,
            };
            let task_template = serde_json::from_slice(&lsp_runnable.task_template)
                .context("deserializing task template from proto")?;
            runnables.runnables.push((location, task_template));
        }

        Ok(runnables)
    }

    fn buffer_id_from_proto(message: &proto::LspExtRunnables) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[derive(Debug)]
pub struct LspExtCancelFlycheck {}

#[derive(Debug)]
pub struct LspExtRunFlycheck {}

#[derive(Debug)]
pub struct LspExtClearFlycheck {}

impl lsp::notification::Notification for LspExtCancelFlycheck {
    type Params = ();
    const METHOD: &'static str = "rust-analyzer/cancelFlycheck";
}

impl lsp::notification::Notification for LspExtRunFlycheck {
    type Params = RunFlycheckParams;
    const METHOD: &'static str = "rust-analyzer/runFlycheck";
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunFlycheckParams {
    pub text_document: Option<lsp::TextDocumentIdentifier>,
}

impl lsp::notification::Notification for LspExtClearFlycheck {
    type Params = ();
    const METHOD: &'static str = "rust-analyzer/clearFlycheck";
}

pub enum LspExtToggleComments {}

impl lsp::request::Request for LspExtToggleComments {
    type Params = ToggleCommentsParams;
    type Result = Option<Vec<lsp::TextEdit>>;
    const METHOD: &'static str = "herb/toggleLineComment";
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ToggleCommentsParams {
    pub text_document: lsp::TextDocumentIdentifier,
    pub ranges: Vec<lsp::Range>,
}

#[derive(Debug)]
pub struct ToggleCommentsCommand {
    pub selections: Vec<Range<Anchor>>,
}

#[async_trait(?Send)]
impl LspCommand for ToggleCommentsCommand {
    type Response = Option<Transaction>;
    type LspRequest = LspExtToggleComments;
    type ProtoRequest = proto::ToggleComments;

    fn display_name(&self) -> &str {
        "Toggle comments"
    }

    fn check_capabilities(&self, _capabilities: AdapterServerCapabilities) -> bool {
        true
    }

    fn to_lsp(
        &self,
        path: &Path,
        buffer: &Buffer,
        _language_server: &Arc<LanguageServer>,
        _cx: &App,
    ) -> Result<ToggleCommentsParams> {
        let ranges = self
            .selections
            .iter()
            .map(|selection| {
                let start = selection.start.to_point_utf16(buffer);
                let end = selection.end.to_point_utf16(buffer);
                lsp::Range {
                    start: point_to_lsp(start),
                    end: point_to_lsp(end),
                }
            })
            .collect();

        Ok(ToggleCommentsParams {
            text_document: make_text_document_identifier(path)?,
            ranges,
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<Vec<lsp::TextEdit>>,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        server_id: LanguageServerId,
        mut cx: AsyncApp,
    ) -> Result<Option<Transaction>> {
        if let Some(edits) = message {
            let (lsp_adapter, lsp_server) =
                language_server_for_buffer(&lsp_store, &buffer, server_id, &mut cx)?;
            LocalLspStore::deserialize_text_edits(
                lsp_store,
                buffer,
                edits,
                true,
                lsp_adapter,
                lsp_server,
                &mut cx,
            )
            .await
        } else {
            Ok(None)
        }
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::ToggleComments {
        let starts = self
            .selections
            .iter()
            .map(|s| serialize_anchor(&s.start))
            .collect();
        let ends = self
            .selections
            .iter()
            .map(|s| serialize_anchor(&s.end))
            .collect();

        proto::ToggleComments {
            project_id,
            buffer_id: buffer.remote_id().into(),
            starts,
            ends,
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::ToggleComments,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;

        let selections = message
            .starts
            .iter()
            .zip(message.ends.iter())
            .map(|(start, end)| {
                let start = deserialize_anchor(start.clone()).context("invalid start anchor")?;
                let end = deserialize_anchor(end.clone()).context("invalid end anchor")?;
                Ok(start..end)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self { selections })
    }

    fn response_to_proto(
        response: Option<Transaction>,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::ToggleCommentsResponse {
        proto::ToggleCommentsResponse {
            transaction: response
                .map(|transaction| language::proto::serialize_transaction(&transaction)),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::ToggleCommentsResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Option<Transaction>> {
        let Some(transaction) = message.transaction else {
            return Ok(None);
        };
        Ok(Some(language::proto::deserialize_transaction(transaction)?))
    }

    fn buffer_id_from_proto(message: &proto::ToggleComments) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}
