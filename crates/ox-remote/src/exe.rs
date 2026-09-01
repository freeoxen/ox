use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_structfs_transport::{KnownHosts, load_private_identity};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use structfs_core_store::{Error as StoreError, Path, Record};
use thiserror::Error;

const MAX_PROVIDER_OUTPUT: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmSpec {
    pub schema_version: u32,
    pub name: String,
    pub node_id: String,
    pub node_attempt_id: String,
    pub image: String,
    pub cpu: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
    pub comment: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub integrations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmStatus {
    pub schema_version: u32,
    pub vm_name: String,
    pub status: String,
    pub ssh_dest: String,
    pub ssh_host: String,
    pub ssh_user: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExeIdentity {
    pub schema_version: u32,
    pub authenticated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeleteVmRequest {
    pub schema_version: u32,
    pub deletion_id: String,
    pub node_id: String,
    pub node_attempt_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteOutcome {
    Deleted,
    AlreadyAbsent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExeCommand {
    Create(VmSpec),
    List { exact_name: Option<String> },
    Remove { name: String },
    Identity,
}

impl ExeCommand {
    fn encode(&self) -> Result<String, ExeError> {
        match self {
            Self::Identity => Ok("whoami --json".into()),
            Self::List { exact_name } => {
                let mut args = vec!["ls".to_owned(), "--json".to_owned()];
                if let Some(name) = exact_name {
                    validate_vm_name(name)?;
                    args.push(name.clone());
                }
                Ok(encode_argv(&args))
            }
            Self::Remove { name } => {
                validate_vm_name(name)?;
                Ok(encode_argv(&["rm".into(), "--json".into(), name.clone()]))
            }
            Self::Create(spec) => {
                spec.validate()?;
                let mut args = vec![
                    "new".into(),
                    "--json".into(),
                    format!("--name={}", spec.name),
                    format!("--image={}", spec.image),
                    format!("--cpu={}", spec.cpu),
                    format!("--memory={}GB", spec.memory_mib / 1024),
                    format!("--disk={}GB", spec.disk_gib),
                    format!("--comment={}", spec.comment),
                    format!("--env=OX_NODE_ID={}", spec.node_id),
                    format!("--env=OX_NODE_ATTEMPT_ID={}", spec.node_attempt_id),
                    format!("--env=OX_WORKER_IMAGE_DIGEST={}", spec.image),
                ];
                for tag in &spec.tags {
                    args.push(format!("--tag={tag}"));
                }
                for integration in &spec.integrations {
                    args.push(format!("--integration={integration}"));
                }
                Ok(encode_argv(&args))
            }
        }
    }
}

impl VmSpec {
    fn validate(&self) -> Result<(), ExeError> {
        if self.schema_version != 1 {
            return Err(ExeError::Invalid("unsupported VmSpec schema".into()));
        }
        validate_vm_name(&self.name)?;
        validate_id("node_id", &self.node_id)?;
        validate_id("node_attempt_id", &self.node_attempt_id)?;
        validate_image(&self.image)?;
        if self.cpu == 0
            || self.cpu > 128
            || self.memory_mib < 256
            || self.memory_mib > 1_048_576
            || self.memory_mib % 1024 != 0
            || self.disk_gib == 0
            || self.disk_gib > 65_536
        {
            return Err(ExeError::Invalid(
                "resource values are out of bounds".into(),
            ));
        }
        validate_text("comment", &self.comment, 200)?;
        if self.tags.len() > 32 || self.integrations.len() > 32 {
            return Err(ExeError::Invalid("too many tags or integrations".into()));
        }
        for tag in &self.tags {
            validate_atom("tag", tag, 64)?;
        }
        for integration in &self.integrations {
            validate_atom("integration", integration, 64)?;
        }
        Ok(())
    }
}

fn validate_vm_name(value: &str) -> Result<(), ExeError> {
    if value.is_empty()
        || value.len() > 63
        || !value.starts_with("ox-")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ExeError::Invalid("invalid deterministic VM name".into()));
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), ExeError> {
    validate_atom(label, value, 128)
}

fn validate_atom(label: &str, value: &str, max: usize) -> Result<(), ExeError> {
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(ExeError::Invalid(format!("invalid {label}")));
    }
    Ok(())
}

fn validate_image(value: &str) -> Result<(), ExeError> {
    if value.is_empty()
        || value.len() > 512
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+/@:-".contains(&byte))
    {
        return Err(ExeError::Invalid("invalid image reference".into()));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, max: usize) -> Result<(), ExeError> {
    if value.len() > max
        || value
            .chars()
            .any(|character| character.is_control() || character == '\0')
    {
        return Err(ExeError::Invalid(format!("invalid {label}")));
    }
    Ok(())
}

fn encode_argv(args: &[String]) -> String {
    args.iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\"'\"'")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CommandError {
    #[error("command outcome is ambiguous: {0}")]
    Ambiguous(String),
    #[error("provider rejected command: {0}")]
    Rejected(String),
    #[error("provider transport failed before command acceptance: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait ExeCommandRunner: Send + Sync {
    async fn run(&self, command: ExeCommand) -> Result<CommandOutput, CommandError>;
}

#[async_trait]
pub trait WorkerIdentityVerifier: Send + Sync {
    async fn verify(
        &self,
        vm: &VmStatus,
        node_id: &str,
        node_attempt_id: &str,
    ) -> Result<bool, ExeError>;
}

#[derive(Debug, Error)]
pub enum ExeError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider response was malformed: {0}")]
    Malformed(String),
    #[error("provider operation was rejected: {0}")]
    Rejected(String),
    #[error("provider create outcome remains ambiguous after exact-name reconciliation")]
    CreateUnresolved,
    #[error("provider delete outcome remains ambiguous after exact-name reconciliation")]
    DeleteUnresolved,
    #[error("node or attempt identity did not match the worker")]
    IdentityMismatch,
    #[error("conflicting provider VMs share the exact deterministic name")]
    DuplicateExactName,
}

#[derive(Clone)]
pub struct ExeControlStore {
    runner: Arc<dyn ExeCommandRunner>,
    identity: Arc<dyn WorkerIdentityVerifier>,
}

impl ExeControlStore {
    pub fn new(
        runner: Arc<dyn ExeCommandRunner>,
        identity: Arc<dyn WorkerIdentityVerifier>,
    ) -> Self {
        Self { runner, identity }
    }

    pub async fn create(&self, spec: VmSpec) -> Result<VmStatus, ExeError> {
        spec.validate()?;
        if let Some(vm) = self.find_exact(&spec.name).await? {
            if !self
                .identity
                .verify(&vm, &spec.node_id, &spec.node_attempt_id)
                .await?
            {
                return Err(ExeError::IdentityMismatch);
            }
            return Ok(vm);
        }
        let create = self.runner.run(ExeCommand::Create(spec.clone())).await;
        match create {
            Ok(output) => require_json_ack(&output.stdout)?,
            Err(CommandError::Ambiguous(_)) => {}
            Err(CommandError::Rejected(error)) => return Err(ExeError::Rejected(error)),
            Err(CommandError::Unavailable(error)) => return Err(ExeError::Unavailable(error)),
        }

        let Some(vm) = self.find_exact(&spec.name).await? else {
            return Err(ExeError::CreateUnresolved);
        };
        if !self
            .identity
            .verify(&vm, &spec.node_id, &spec.node_attempt_id)
            .await?
        {
            return Err(ExeError::IdentityMismatch);
        }
        Ok(vm)
    }

    pub async fn list(&self) -> Result<Vec<VmStatus>, ExeError> {
        let output = self
            .runner
            .run(ExeCommand::List { exact_name: None })
            .await
            .map_err(map_command_error)?;
        parse_list(&output.stdout)
    }

    pub async fn find_exact(&self, name: &str) -> Result<Option<VmStatus>, ExeError> {
        validate_vm_name(name)?;
        let output = self
            .runner
            .run(ExeCommand::List {
                exact_name: Some(name.into()),
            })
            .await
            .map_err(map_command_error)?;
        let exact: Vec<_> = parse_list(&output.stdout)?
            .into_iter()
            .filter(|vm| vm.vm_name == name)
            .collect();
        match exact.len() {
            0 => Ok(None),
            1 => Ok(exact.into_iter().next()),
            _ => Err(ExeError::DuplicateExactName),
        }
    }

    pub async fn remove(
        &self,
        name: &str,
        request: DeleteVmRequest,
    ) -> Result<DeleteOutcome, ExeError> {
        if request.schema_version != 1 {
            return Err(ExeError::Invalid("unsupported delete schema".into()));
        }
        validate_id("node_id", &request.node_id)?;
        validate_id("node_attempt_id", &request.node_attempt_id)?;
        validate_id("deletion_id", &request.deletion_id)?;
        let Some(vm) = self.find_exact(name).await? else {
            return Ok(DeleteOutcome::AlreadyAbsent);
        };
        if !self
            .identity
            .verify(&vm, &request.node_id, &request.node_attempt_id)
            .await?
        {
            return Err(ExeError::IdentityMismatch);
        }

        match self
            .runner
            .run(ExeCommand::Remove { name: name.into() })
            .await
        {
            Ok(output) => require_json_ack(&output.stdout)?,
            Err(CommandError::Ambiguous(_)) => {}
            Err(error) => return Err(map_command_error(error)),
        }
        if self.find_exact(name).await?.is_some() {
            return Err(ExeError::DeleteUnresolved);
        }
        Ok(DeleteOutcome::Deleted)
    }

    pub async fn authenticated_identity(&self) -> Result<ExeIdentity, ExeError> {
        let output = self
            .runner
            .run(ExeCommand::Identity)
            .await
            .map_err(map_command_error)?;
        let value = parse_one_json(&output.stdout)?;
        if !value.is_object() {
            return Err(ExeError::Malformed("whoami JSON must be an object".into()));
        }
        // The provider documents authentication and JSON output, but not a
        // stable whoami field schema. Do not leak or guess provider fields.
        Ok(ExeIdentity {
            schema_version: 1,
            authenticated: true,
        })
    }

    fn store_error(operation: &'static str, error: ExeError) -> StoreError {
        StoreError::store("ExeControlStore", operation, error.to_string())
    }
}

/// StructFS paths cannot contain provider VM punctuation such as `-`. Keep
/// the provider name typed and reversible instead of weakening path grammar.
pub fn vm_path(name: &str) -> Result<Path, ExeError> {
    validate_vm_name(name)?;
    Path::parse(&format!("vms/{}", encode_vm_component(name)))
        .map_err(|error| ExeError::Invalid(error.to_string()))
}

pub fn vm_delete_path(name: &str) -> Result<Path, ExeError> {
    let item = vm_path(name)?;
    Path::parse(&format!("{item}/delete")).map_err(|error| ExeError::Invalid(error.to_string()))
}

pub fn encode_vm_component(name: &str) -> String {
    let mut encoded = String::with_capacity(3 + name.len() * 2);
    encoded.push_str("vm_");
    for byte in name.bytes() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("String writes do not fail");
    }
    encoded
}

pub fn decode_vm_component(component: &str) -> Result<String, ExeError> {
    let hex = component
        .strip_prefix("vm_")
        .ok_or_else(|| ExeError::Invalid("invalid VM path component".into()))?;
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExeError::Invalid("invalid VM path component".into()));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair)
            .map_err(|_| ExeError::Invalid("invalid VM path component".into()))?;
        bytes.push(
            u8::from_str_radix(text, 16)
                .map_err(|_| ExeError::Invalid("invalid VM path component".into()))?,
        );
    }
    let name = String::from_utf8(bytes)
        .map_err(|_| ExeError::Invalid("invalid VM path component".into()))?;
    validate_vm_name(&name)?;
    Ok(name)
}

fn map_command_error(error: CommandError) -> ExeError {
    match error {
        CommandError::Ambiguous(message) | CommandError::Unavailable(message) => {
            ExeError::Unavailable(message)
        }
        CommandError::Rejected(message) => ExeError::Rejected(message),
    }
}

impl AsyncReader for ExeControlStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let this = self.clone();
        let components: Vec<String> = from.iter().cloned().collect();
        Box::pin(async move {
            let value = match components.as_slice() {
                [identity] if identity == "identity" => serde_json::to_value(
                    this.authenticated_identity()
                        .await
                        .map_err(|error| Self::store_error("identity", error))?,
                ),
                [vms] if vms == "vms" => serde_json::to_value(
                    this.list()
                        .await
                        .map_err(|error| Self::store_error("list", error))?,
                ),
                [vms, component] if vms == "vms" => {
                    let name = decode_vm_component(component)
                        .map_err(|error| Self::store_error("list", error))?;
                    let Some(vm) = this
                        .find_exact(&name)
                        .await
                        .map_err(|error| Self::store_error("list", error))?
                    else {
                        return Ok(None);
                    };
                    serde_json::to_value(vm)
                }
                _ => return Ok(None),
            }
            .map_err(|error| {
                StoreError::store("ExeControlStore", "serialize", error.to_string())
            })?;
            Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                value,
            ))))
        })
    }
}

impl AsyncWriter for ExeControlStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let this = self.clone();
        let target = to.clone();
        Box::pin(async move {
            let json = record_json(data)?;
            let components: Vec<String> = target.iter().cloned().collect();
            match components.as_slice() {
                [vms] if vms == "vms" => {
                    let spec: VmSpec = serde_json::from_value(json).map_err(|error| {
                        StoreError::store("ExeControlStore", "create", error.to_string())
                    })?;
                    let vm = this
                        .create(spec)
                        .await
                        .map_err(|error| Self::store_error("create", error))?;
                    vm_path(&vm.vm_name).map_err(|error| Self::store_error("create", error))
                }
                [vms, component, delete] if vms == "vms" && delete == "delete" => {
                    let name = decode_vm_component(component)
                        .map_err(|error| Self::store_error("delete", error))?;
                    let request: DeleteVmRequest =
                        serde_json::from_value(json).map_err(|error| {
                            StoreError::store("ExeControlStore", "delete", error.to_string())
                        })?;
                    let deletion_id = request.deletion_id.clone();
                    this.remove(&name, request)
                        .await
                        .map_err(|error| Self::store_error("delete", error))?;
                    Path::parse(&format!(
                        "vms/{}/deletions/{deletion_id}",
                        encode_vm_component(&name)
                    ))
                    .map_err(StoreError::from)
                }
                _ => Err(StoreError::store(
                    "ExeControlStore",
                    "write",
                    "unsupported path",
                )),
            }
        })
    }
}

fn record_json(record: Record) -> Result<JsonValue, StoreError> {
    let value = record
        .as_value()
        .cloned()
        .ok_or_else(|| StoreError::store("ExeControlStore", "decode", "expected parsed record"))?;
    Ok(structfs_serde_store::value_to_json(value))
}

#[derive(Deserialize)]
struct ProviderList {
    vms: Vec<ProviderVm>,
}

#[derive(Deserialize)]
struct ProviderVm {
    vm_name: String,
    status: String,
    ssh_dest: String,
    ssh_host: String,
    #[serde(default)]
    ssh_user: Option<String>,
}

fn parse_list(bytes: &[u8]) -> Result<Vec<VmStatus>, ExeError> {
    let list: ProviderList = serde_json::from_value(parse_one_json(bytes)?)
        .map_err(|error| ExeError::Malformed(error.to_string()))?;
    list.vms
        .into_iter()
        .map(|vm| {
            validate_provider_vm(&vm)?;
            Ok(VmStatus {
                schema_version: 1,
                vm_name: vm.vm_name,
                status: vm.status,
                ssh_dest: vm.ssh_dest,
                ssh_host: vm.ssh_host,
                ssh_user: vm.ssh_user,
            })
        })
        .collect()
}

fn validate_provider_vm(vm: &ProviderVm) -> Result<(), ExeError> {
    validate_vm_name(&vm.vm_name)?;
    validate_text("provider status", &vm.status, 64)?;
    if vm.status.is_empty() {
        return Err(ExeError::Malformed("provider status is empty".into()));
    }
    validate_text("ssh_dest", &vm.ssh_dest, 512)?;
    if vm.ssh_dest.is_empty()
        || vm.ssh_host.is_empty()
        || vm.ssh_host.len() > 253
        || !vm
            .ssh_host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:_".contains(&byte))
    {
        return Err(ExeError::Malformed(
            "invalid provider SSH destination".into(),
        ));
    }
    if let Some(user) = &vm.ssh_user {
        validate_atom("ssh_user", user, 128)?;
    }
    Ok(())
}

fn parse_one_json(bytes: &[u8]) -> Result<JsonValue, ExeError> {
    if bytes.len() > MAX_PROVIDER_OUTPUT {
        return Err(ExeError::Malformed("provider JSON exceeds limit".into()));
    }
    serde_json::from_slice(bytes).map_err(|error| ExeError::Malformed(error.to_string()))
}

fn require_json_ack(bytes: &[u8]) -> Result<(), ExeError> {
    let _ = parse_one_json(bytes)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ExeSshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity_file: PathBuf,
    pub known_hosts: KnownHosts,
    pub inactivity_timeout: Duration,
    pub operation_timeout: Duration,
}

#[derive(Clone)]
pub struct RusshExeRunner {
    config: ExeSshConfig,
}

impl RusshExeRunner {
    pub fn new(config: ExeSshConfig) -> Result<Self, ExeError> {
        validate_control_config(&config)?;
        Ok(Self { config })
    }
}

#[derive(Clone)]
struct ControlHostVerifier {
    host: String,
    port: u16,
    known_hosts: KnownHosts,
}

#[derive(Debug, Error)]
enum ControlSshError {
    #[error(transparent)]
    Ssh(#[from] russh::Error),
    #[error(transparent)]
    HostKey(#[from] ox_structfs_transport::KnownHostsError),
    #[error("host-key verification task failed: {0}")]
    VerificationTask(String),
}

impl client::Handler for ControlHostVerifier {
    type Error = ControlSshError;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let known_hosts = self.known_hosts.clone();
        let host = self.host.clone();
        let port = self.port;
        let key = key.public_key();
        tokio::task::spawn_blocking(move || known_hosts.verify(&host, port, &key))
            .await
            .map_err(|error| ControlSshError::VerificationTask(error.to_string()))??;
        Ok(true)
    }
}

#[async_trait]
impl ExeCommandRunner for RusshExeRunner {
    async fn run(&self, command: ExeCommand) -> Result<CommandOutput, CommandError> {
        let encoded = command
            .encode()
            .map_err(|error| CommandError::Rejected(error.to_string()))?;
        match tokio::time::timeout(
            self.config.operation_timeout,
            run_control_command(&self.config, encoded),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(CommandError::Ambiguous(
                "exe.dev operation deadline elapsed; reconcile before retry".into(),
            )),
        }
    }
}

async fn run_control_command(
    config: &ExeSshConfig,
    command: String,
) -> Result<CommandOutput, CommandError> {
    let key = load_private_identity(&config.identity_file)
        .map_err(|error| CommandError::Unavailable(error.to_string()))?;
    let handler = ControlHostVerifier {
        host: config.host.clone(),
        port: config.port,
        known_hosts: config.known_hosts.clone(),
    };
    let ssh_config = Arc::new(client::Config {
        inactivity_timeout: Some(config.inactivity_timeout),
        ..Default::default()
    });
    let mut session = client::connect(ssh_config, (config.host.as_str(), config.port), handler)
        .await
        .map_err(|error| CommandError::Unavailable(error.to_string()))?;
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(|error| CommandError::Unavailable(error.to_string()))?
        .flatten();
    let auth = session
        .authenticate_publickey(
            config.user.clone(),
            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
        )
        .await
        .map_err(|error| CommandError::Unavailable(error.to_string()))?;
    if !auth.success() {
        return Err(CommandError::Rejected("SSH authentication rejected".into()));
    }
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|error| CommandError::Unavailable(error.to_string()))?;
    channel
        .exec(true, command)
        .await
        .map_err(|error| CommandError::Unavailable(error.to_string()))?;

    let mut acknowledged = false;
    let mut exit_status = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut output_bytes = 0_usize;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Success => acknowledged = true,
            ChannelMsg::Failure => return Err(CommandError::Rejected("exec rejected".into())),
            ChannelMsg::Data { data } => append_bounded(&mut stdout, &data, &mut output_bytes)?,
            ChannelMsg::ExtendedData { data, .. } => {
                append_bounded(&mut stderr, &data, &mut output_bytes)?
            }
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            _ => {}
        }
    }
    let _ = session
        .disconnect(Disconnect::ByApplication, "", "en")
        .await;
    if !acknowledged {
        return Err(CommandError::Ambiguous("no exec acknowledgement".into()));
    }
    match exit_status {
        Some(0) => Ok(CommandOutput { stdout, stderr }),
        Some(status) => Err(CommandError::Rejected(format!(
            "exit status {status}: {}",
            String::from_utf8_lossy(&stderr)
        ))),
        None => Err(CommandError::Ambiguous("no exit status".into())),
    }
}

fn append_bounded(
    target: &mut Vec<u8>,
    bytes: &[u8],
    output_bytes: &mut usize,
) -> Result<(), CommandError> {
    if output_bytes.saturating_add(bytes.len()) > MAX_PROVIDER_OUTPUT {
        return Err(CommandError::Rejected(
            "provider output exceeds limit".into(),
        ));
    }
    target.extend_from_slice(bytes);
    *output_bytes += bytes.len();
    Ok(())
}

fn validate_control_config(config: &ExeSshConfig) -> Result<(), ExeError> {
    if config.host.is_empty()
        || config.port == 0
        || !config
            .host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:_".contains(&byte))
    {
        return Err(ExeError::Invalid("invalid exe.dev control host".into()));
    }
    validate_atom("control user", &config.user, 128)?;
    if config.inactivity_timeout.is_zero() || config.operation_timeout.is_zero() {
        return Err(ExeError::Invalid("SSH timeouts must be non-zero".into()));
    }
    // The file itself is opened and validated atomically by
    // `load_private_identity` at connection time.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmSpec {
        VmSpec {
            schema_version: 1,
            name: "ox-deadbeef".into(),
            node_id: "n_123".into(),
            node_attempt_id: "na_456".into(),
            image: "ghcr.io/freeoxen/ox@sha256:abc".into(),
            cpu: 2,
            memory_mib: 4096,
            disk_gib: 20,
            comment: "remote node".into(),
            tags: vec!["ox".into()],
            integrations: vec!["github".into()],
        }
    }

    #[test]
    fn encoder_is_allowlisted_and_shell_quotes_every_argument() {
        assert_eq!(
            ExeCommand::List {
                exact_name: Some("ox-deadbeef".into())
            }
            .encode()
            .unwrap(),
            "'ls' '--json' 'ox-deadbeef'"
        );
        let encoded = ExeCommand::Create(spec()).encode().unwrap();
        assert!(encoded.starts_with("'new' '--json' '--name=ox-deadbeef'"));
        assert!(encoded.contains("'--env=OX_NODE_ID=n_123'"));
        assert!(encoded.contains("'--env=OX_NODE_ATTEMPT_ID=na_456'"));
    }

    #[test]
    fn command_values_reject_injection() {
        for bad_name in ["ox-a;id", "ox-a $(id)", "other", "ox-A"] {
            let mut candidate = spec();
            candidate.name = bad_name.into();
            assert!(candidate.validate().is_err());
        }
        for bad in ["github;id", "github$(id)", "git hub", "github`id`"] {
            let mut candidate = spec();
            candidate.integrations = vec![bad.into()];
            assert!(candidate.validate().is_err());
        }
        let mut candidate = spec();
        candidate.image = "image;id".into();
        assert!(candidate.validate().is_err());
    }
}
