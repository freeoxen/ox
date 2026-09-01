//! Headless `ox remote` command surface.
//!
//! The CLI talks only to `RemoteManagerStore` and its local cache through
//! StructFS paths. Provider commands and RuSSH remain edge adapters owned by
//! `ox-remote` and `ox-structfs-transport`.

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use ox_inbox::remote_state::{CachedLedgerEntry, RemoteConversationRecord, RemoteNodeRecord};
use ox_remote::{
    ApprovalRequest, AsyncStorePort, CancelRequest, CreateNodeRequest, DeleteNodeManagerRequest,
    ExeControlStore, ExeSshConfig, MessageRequest, NodeProvisionSpec, PlacementPolicy,
    RemoteManagerConfig, RemoteManagerError, RemoteManagerStore, RusshExeRunner,
    SshWorkerConnector, SshWorkerIdentityVerifier, StartConversationRequest, StorePort,
    SyncStorePort,
};
use ox_structfs_transport::{HostKeyEnrollment, KnownHosts, RemoteStoreConfig};
use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Record, Value, path};
use tokio::io::AsyncBufReadExt as _;

const DEFAULT_WORKER_SOCKET: &str = "/run/ox/worker.sock";

#[derive(Clone, Debug, Args)]
pub struct RemoteArgs {
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true)]
    pub identity: Option<PathBuf>,
    #[arg(long, global = true)]
    pub connect_timeout: Option<String>,
    #[arg(long, global = true)]
    pub operation_timeout: Option<String>,
    /// Explicitly enroll previously unseen provider and worker host keys.
    /// Changed keys are always rejected.
    #[arg(long, global = true)]
    pub accept_new_host_key: bool,
    #[command(subcommand)]
    pub command: RemoteCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum RemoteCommand {
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
    Conversation {
        #[command(subcommand)]
        command: ConversationCommand,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum NodeCommand {
    New(ResourceArgs),
    List,
    Show {
        node: String,
    },
    Drain {
        node: String,
    },
    Delete {
        node: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        delete_id: Option<String>,
    },
    Doctor {
        node: Option<String>,
    },
}

#[derive(Clone, Debug, Args)]
pub struct ResourceArgs {
    #[arg(long)]
    image: Option<String>,
    #[arg(long)]
    cpu: Option<u16>,
    #[arg(long)]
    memory_mib: Option<u32>,
    #[arg(long)]
    disk_gib: Option<u32>,
    #[arg(long)]
    request_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum PlacementArg {
    #[default]
    FreshNode,
    PreferExisting,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ConversationCommand {
    New {
        #[arg(long, conflicts_with_all = ["prompt_file", "stdin"])]
        prompt: Option<String>,
        #[arg(long, conflicts_with_all = ["prompt", "stdin"])]
        prompt_file: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["prompt", "prompt_file"])]
        stdin: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, value_enum)]
        placement: Option<PlacementArg>,
        #[arg(long, conflicts_with = "placement")]
        node: Option<String>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        cpu: Option<u16>,
        #[arg(long)]
        memory_mib: Option<u32>,
        #[arg(long)]
        disk_gib: Option<u32>,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        attach: bool,
    },
    List,
    Show {
        conversation: String,
    },
    Attach {
        conversation: String,
        #[arg(long)]
        from: Option<i64>,
        #[arg(long)]
        read_only: bool,
    },
    Send {
        conversation: String,
        #[arg(long, conflicts_with_all = ["prompt_file", "stdin"])]
        prompt: Option<String>,
        #[arg(long, conflicts_with_all = ["prompt", "stdin"])]
        prompt_file: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["prompt", "prompt_file"])]
        stdin: bool,
        #[arg(long)]
        message_id: Option<String>,
    },
    Logs {
        conversation: String,
        #[arg(long, default_value_t = 0)]
        from: i64,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        jsonl: bool,
        /// Read only the durable local cache without contacting the worker.
        #[arg(long, conflicts_with = "follow")]
        cached: bool,
    },
    Approve {
        conversation: String,
        approval_id: String,
        #[arg(long, conflicts_with = "deny", required_unless_present = "deny")]
        allow: bool,
        #[arg(long, conflicts_with = "allow", required_unless_present = "allow")]
        deny: bool,
    },
    Cancel {
        conversation: String,
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value = "30s")]
        timeout: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        cancel_id: Option<String>,
    },
    Reconcile {
        conversation: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RemoteFileConfig {
    exe: ExeConfig,
    defaults: DefaultsConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExeConfig {
    control_host: String,
    control_port: u16,
    control_user: String,
    worker_image: String,
    identity: Option<PathBuf>,
    provider_known_hosts: Option<PathBuf>,
    worker_known_hosts: Option<PathBuf>,
    worker_socket: PathBuf,
    connect_timeout: String,
    operation_timeout: String,
}

impl Default for ExeConfig {
    fn default() -> Self {
        Self {
            control_host: "exe.dev".into(),
            control_port: 22,
            control_user: std::env::var("USER").unwrap_or_else(|_| "root".into()),
            worker_image: String::new(),
            identity: None,
            provider_known_hosts: None,
            worker_known_hosts: None,
            worker_socket: DEFAULT_WORKER_SOCKET.into(),
            connect_timeout: "10s".into(),
            operation_timeout: "30s".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DefaultsConfig {
    cpu: u16,
    memory_mib: u32,
    disk_gib: u32,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            cpu: 2,
            memory_mib: 4096,
            disk_gib: 20,
        }
    }
}

struct Runtime {
    manager: Option<Arc<dyn StorePort>>,
    local: Arc<dyn StorePort>,
    config: RemoteFileConfig,
}

#[derive(Clone, Debug, Serialize)]
pub struct CliError {
    pub code: i32,
    pub kind: &'static str,
    pub message: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            kind: "validation_failed",
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            kind: "not_found",
            message: message.into(),
        }
    }

    fn persistence(message: impl Into<String>) -> Self {
        Self {
            code: 10,
            kind: "local_persistence",
            message: message.into(),
        }
    }
}

pub fn print_error(error: &CliError, json: bool) {
    if json {
        eprintln!("{}", serde_json::to_string(error).unwrap_or_else(|_| "{\"code\":10,\"kind\":\"local_persistence\",\"message\":\"error serialization failed\"}".into()));
    } else {
        eprintln!("error: {}", sanitize_terminal(&error.message));
    }
}

pub async fn run(args: &RemoteArgs) -> Result<(), CliError> {
    let root = ox_root()?;
    let config = load_config(&root, args)?;
    let runtime = build_runtime(&root, config, args, command_needs_remote(&args.command)).await?;
    match &args.command {
        RemoteCommand::Node { command } => run_node(&runtime, command, args.json).await,
        RemoteCommand::Conversation { command } => {
            run_conversation(&runtime, command, args.json).await
        }
    }
}

async fn build_runtime(
    root: &FsPath,
    config: RemoteFileConfig,
    args: &RemoteArgs,
    needs_remote: bool,
) -> Result<Runtime, CliError> {
    let local: Arc<dyn StorePort> = Arc::new(SyncStorePort::new(
        ox_inbox::InboxStore::open(root)
            .map_err(|error| CliError::persistence(error.to_string()))?,
    ));
    if !needs_remote {
        return Ok(Runtime {
            manager: None,
            local,
            config,
        });
    }
    let identity = args
        .identity
        .clone()
        .or_else(|| config.exe.identity.clone())
        .ok_or_else(|| CliError::auth("remote.exe.identity is required"))?;
    let identity = expand_tilde(identity)?;
    let metadata = std::fs::metadata(&identity)
        .map_err(|error| CliError::auth(format!("SSH identity is unavailable: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(CliError::auth(
                "SSH identity must be a regular user-only file",
            ));
        }
    }
    let enrollment = if args.accept_new_host_key {
        HostKeyEnrollment::EnrollNew
    } else {
        HostKeyEnrollment::RefuseUnknown
    };
    let connect_timeout = parse_duration(
        args.connect_timeout
            .as_deref()
            .unwrap_or(&config.exe.connect_timeout),
    )?;
    let operation_timeout = parse_duration(
        args.operation_timeout
            .as_deref()
            .unwrap_or(&config.exe.operation_timeout),
    )?;
    let remote_dir = root.join("remote");
    let provider_known_hosts = expand_tilde(
        config
            .exe
            .provider_known_hosts
            .clone()
            .unwrap_or_else(|| remote_dir.join("provider_known_hosts")),
    )?;
    let worker_known_hosts = expand_tilde(
        config
            .exe
            .worker_known_hosts
            .clone()
            .unwrap_or_else(|| remote_dir.join("known_hosts")),
    )?;
    let remote_config = RemoteStoreConfig {
        request_timeout: operation_timeout,
        ..RemoteStoreConfig::default()
    };
    let verifier = Arc::new(SshWorkerIdentityVerifier {
        identity_path: identity.clone(),
        known_hosts_path: worker_known_hosts.clone(),
        worker_socket_path: config.exe.worker_socket.clone(),
        ssh_port: 22,
        enrollment,
        inactivity_timeout: connect_timeout,
        remote: remote_config.clone(),
    });
    let runner = Arc::new(
        RusshExeRunner::new(ExeSshConfig {
            host: config.exe.control_host.clone(),
            port: config.exe.control_port,
            user: config.exe.control_user.clone(),
            identity_file: identity.clone(),
            known_hosts: KnownHosts::new(provider_known_hosts, enrollment),
            inactivity_timeout: connect_timeout,
            operation_timeout,
        })
        .map_err(map_exe_error)?,
    );
    let provider: Arc<dyn StorePort> =
        Arc::new(AsyncStorePort::new(ExeControlStore::new(runner, verifier)));
    let connector = Arc::new(SshWorkerConnector {
        enrollment,
        inactivity_timeout: connect_timeout,
        remote: remote_config,
    });
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector,
        RemoteManagerConfig {
            reconciler_id: format!("cli-{}-{}", std::process::id(), uuid::Uuid::new_v4()),
            lease_seconds: 30,
            provider: "exe.dev".into(),
            ssh_port: 22,
            identity_path: identity.to_string_lossy().into_owned(),
            known_hosts_path: worker_known_hosts.to_string_lossy().into_owned(),
            worker_socket_path: config.exe.worker_socket.to_string_lossy().into_owned(),
        },
    )
    .map_err(map_manager_error)?;
    Ok(Runtime {
        manager: Some(Arc::new(AsyncStorePort::new(manager))),
        local,
        config,
    })
}

async fn run_node(runtime: &Runtime, command: &NodeCommand, json: bool) -> Result<(), CliError> {
    match command {
        NodeCommand::New(resources) => {
            let request = CreateNodeRequest {
                schema_version: 1,
                request_id: resources.request_id.clone().unwrap_or_else(new_request_id),
                node: provision_spec(
                    &runtime.config,
                    resources.image.as_ref(),
                    resources.cpu,
                    resources.memory_mib,
                    resources.disk_gib,
                )?,
            };
            let receipt = manager(runtime)?
                .write(&path!("nodes"), parsed(&request)?)
                .await
                .map_err(map_store_error)?;
            let id = receipt
                .iter()
                .last()
                .cloned()
                .ok_or_else(|| CliError::persistence("node receipt was empty"))?;
            let node = resolve_node(runtime, &id).await?;
            print_node(&node, json)
        }
        NodeCommand::List => {
            let nodes = list_nodes(runtime).await?;
            if json {
                print_json(&nodes.iter().map(node_view).collect::<Vec<_>>())
            } else {
                for node in nodes {
                    println!(
                        "{}\t{}\t{}\t{}",
                        sanitize_terminal(&node.node_id),
                        sanitize_terminal(&node.vm_name),
                        sanitize_terminal(&node.desired_state),
                        sanitize_terminal(&node.observed_state)
                    );
                }
                Ok(())
            }
        }
        NodeCommand::Show { node } => print_node(&resolve_node(runtime, node).await?, json),
        NodeCommand::Drain { node } => {
            let node = resolve_node(runtime, node).await?;
            manager(runtime)?
                .write(
                    &Path::parse(&format!("nodes/{}/drain", node.node_id))
                        .map_err(|error| CliError::validation(error.to_string()))?,
                    Record::parsed(Value::Null),
                )
                .await
                .map_err(map_store_error)?;
            print_node(&resolve_node(runtime, &node.node_id).await?, json)
        }
        NodeCommand::Delete {
            node,
            yes,
            force,
            request_id,
            delete_id,
        } => {
            let node = resolve_node(runtime, node).await?;
            if !*yes {
                if !std::io::stdin().is_terminal() {
                    return Err(CliError::validation(
                        "node deletion in noninteractive mode requires --yes",
                    ));
                }
                eprint!(
                    "Delete {} ({})? [y/N] ",
                    sanitize_terminal(&node.node_id),
                    sanitize_terminal(&node.vm_name)
                );
                std::io::stderr()
                    .flush()
                    .map_err(|error| CliError::persistence(error.to_string()))?;
                let mut answer = String::new();
                std::io::stdin()
                    .read_line(&mut answer)
                    .map_err(|error| CliError::persistence(error.to_string()))?;
                if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                    return Err(CliError::validation("node deletion was not confirmed"));
                }
            }
            let delete_id = delete_id
                .clone()
                .unwrap_or_else(|| format!("delete_{}", uuid::Uuid::new_v4().simple()));
            let request_id = request_id
                .clone()
                .unwrap_or_else(|| format!("delete-{delete_id}"));
            let request = DeleteNodeManagerRequest {
                request_id,
                delete_id,
                force: *force,
            };
            manager(runtime)?
                .write(
                    &Path::parse(&format!("nodes/{}/delete", node.node_id))
                        .map_err(|error| CliError::validation(error.to_string()))?,
                    parsed(&request)?,
                )
                .await
                .map_err(map_store_error)?;
            print_node(&resolve_node(runtime, &node.node_id).await?, json)
        }
        NodeCommand::Doctor { node } => {
            validate_image(&runtime.config.exe.worker_image)?;
            let provider = manager(runtime)?
                .read(&path!("doctor/provider"))
                .await
                .map_err(map_store_error)?
                .ok_or_else(|| CliError::unavailable("provider doctor result missing"))?;
            let provider_json = record_json(provider)?;
            if let Some(node) = node {
                let node = resolve_node(runtime, node).await?;
                let path = Path::parse(&format!("nodes/{}/doctor", node.node_id))
                    .map_err(|error| CliError::validation(error.to_string()))?;
                let health = manager(runtime)?
                    .read(&path)
                    .await
                    .map_err(map_store_error)?
                    .ok_or_else(|| CliError::unavailable("worker doctor result missing"))?;
                let result =
                    serde_json::json!({"provider": provider_json, "worker": record_json(health)?});
                if json {
                    print_json(&result)
                } else {
                    println!(
                        "provider: ready\nworker:   ready\nnode:     {}",
                        sanitize_terminal(&node.node_id)
                    );
                    Ok(())
                }
            } else if json {
                print_json(&provider_json)
            } else {
                println!("provider: authenticated\nconfig:   valid\nimage:    pinned");
                Ok(())
            }
        }
    }
}

async fn run_conversation(
    runtime: &Runtime,
    command: &ConversationCommand,
    json: bool,
) -> Result<(), CliError> {
    match command {
        ConversationCommand::New {
            prompt,
            prompt_file,
            stdin,
            title,
            placement,
            node,
            image,
            cpu,
            memory_mib,
            disk_gib,
            request_id,
            attach,
        } => {
            let prompt = read_prompt(prompt.as_deref(), prompt_file.as_deref(), *stdin)?;
            let placement = if let Some(node) = node {
                PlacementPolicy::RequireNode {
                    node_id: resolve_node(runtime, node).await?.node_id,
                }
            } else {
                match placement.unwrap_or_default() {
                    PlacementArg::FreshNode => PlacementPolicy::FreshNode,
                    PlacementArg::PreferExisting => PlacementPolicy::PreferExisting,
                }
            };
            let request = StartConversationRequest {
                schema_version: 1,
                request_id: request_id.clone().unwrap_or_else(new_request_id),
                title: title
                    .clone()
                    .unwrap_or_else(|| "Remote conversation".into()),
                prompt,
                parent_thread_id: None,
                placement,
                node: provision_spec(
                    &runtime.config,
                    image.as_ref(),
                    *cpu,
                    *memory_mib,
                    *disk_gib,
                )?,
            };
            let receipt = manager(runtime)?
                .write(&path!("conversations"), parsed(&request)?)
                .await
                .map_err(map_store_error)?;
            let id = receipt
                .iter()
                .last()
                .cloned()
                .ok_or_else(|| CliError::persistence("conversation receipt was empty"))?;
            let conversation = resolve_conversation(runtime, &id).await?;
            print_conversation(runtime, &conversation, json).await?;
            if *attach {
                follow(runtime, &conversation, 0, None, false, false).await?;
            }
            Ok(())
        }
        ConversationCommand::List => {
            let conversations = list_conversations(runtime).await?;
            if json {
                let mut summaries = Vec::with_capacity(conversations.len());
                for conversation in &conversations {
                    summaries.push(conversation_view(runtime, conversation).await?);
                }
                print_json(&summaries)
            } else {
                for item in conversations {
                    println!(
                        "{}\t{}\t{}\t{}",
                        sanitize_terminal(&item.conversation_id),
                        sanitize_terminal(&item.title),
                        sanitize_terminal(&item.node_id),
                        sanitize_terminal(&item.observed_state)
                    );
                }
                Ok(())
            }
        }
        ConversationCommand::Show { conversation } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            print_conversation(runtime, &conversation, json).await
        }
        ConversationCommand::Send {
            conversation,
            prompt,
            prompt_file,
            stdin,
            message_id,
        } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            let content = read_prompt(prompt.as_deref(), prompt_file.as_deref(), *stdin)?;
            let message_id = message_id
                .clone()
                .unwrap_or_else(|| format!("m_{}", uuid::Uuid::new_v4().simple()));
            let request = MessageRequest {
                request_id: format!("send-{message_id}"),
                message_id,
                content,
            };
            let target = Path::parse(&format!(
                "conversations/{}/messages",
                conversation.conversation_id
            ))
            .map_err(|error| CliError::validation(error.to_string()))?;
            let receipt = manager(runtime)?
                .write(&target, parsed(&request)?)
                .await
                .map_err(map_store_error)?;
            if json {
                print_json(&serde_json::json!({"path": receipt.to_string()}))
            } else {
                println!("accepted: {}", sanitize_terminal(&receipt.to_string()));
                Ok(())
            }
        }
        ConversationCommand::Approve {
            conversation,
            approval_id,
            allow,
            ..
        } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            let decision = if *allow {
                ox_types::Decision::AllowOnce
            } else {
                ox_types::Decision::DenyOnce
            };
            let request = ApprovalRequest {
                request_id: format!("approval-{approval_id}"),
                approval_id: approval_id.clone(),
                decision,
            };
            let target = Path::parse(&format!(
                "conversations/{}/approvals",
                conversation.conversation_id
            ))
            .map_err(|error| CliError::validation(error.to_string()))?;
            let receipt = manager(runtime)?
                .write(&target, parsed(&request)?)
                .await
                .map_err(map_store_error)?;
            if json {
                print_json(
                    &serde_json::json!({"path": receipt.to_string(), "decision": decision.as_str()}),
                )
            } else {
                println!("approval: {}", decision.as_str());
                Ok(())
            }
        }
        ConversationCommand::Cancel {
            conversation,
            wait,
            timeout,
            request_id,
            cancel_id,
        } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            let cancel_id = cancel_id
                .clone()
                .unwrap_or_else(|| format!("cancel_{}", uuid::Uuid::new_v4().simple()));
            let request = CancelRequest {
                request_id: request_id
                    .clone()
                    .unwrap_or_else(|| format!("cancel-{cancel_id}")),
                cancel_id,
                reason: Some("cancelled from ox remote CLI".into()),
            };
            let target = Path::parse(&format!(
                "conversations/{}/cancel",
                conversation.conversation_id
            ))
            .map_err(|error| CliError::validation(error.to_string()))?;
            manager(runtime)?
                .write(&target, parsed(&request)?)
                .await
                .map_err(map_store_error)?;
            if *wait {
                wait_terminal(
                    runtime,
                    &conversation.conversation_id,
                    parse_duration(timeout)?,
                )
                .await?;
            }
            let current = resolve_conversation(runtime, &conversation.conversation_id).await?;
            print_conversation(runtime, &current, json).await
        }
        ConversationCommand::Reconcile { conversation } => {
            if let Some(reference) = conversation {
                let conversation = resolve_conversation(runtime, reference).await?;
                reconcile_conversation(runtime, &conversation.conversation_id).await?;
            } else {
                manager(runtime)?
                    .write(&path!("reconcile"), Record::parsed(Value::Null))
                    .await
                    .map_err(map_store_error)?;
            }
            if json {
                print_json(&serde_json::json!({"reconciled": conversation}))
            } else {
                println!("reconciled");
                Ok(())
            }
        }
        ConversationCommand::Logs {
            conversation,
            from,
            limit,
            follow: follow_logs,
            jsonl,
            cached,
        } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            if *cached {
                print_new_entries(runtime, &conversation, *from, *limit, *jsonl).await?;
                Ok(())
            } else {
                follow(runtime, &conversation, *from, *limit, *follow_logs, *jsonl).await
            }
        }
        ConversationCommand::Attach {
            conversation,
            from,
            read_only,
        } => {
            let conversation = resolve_conversation(runtime, conversation).await?;
            attach(runtime, &conversation, from.unwrap_or(0), *read_only).await
        }
    }
}

async fn attach(
    runtime: &Runtime,
    conversation: &RemoteConversationRecord,
    from: i64,
    read_only: bool,
) -> Result<(), CliError> {
    let mut next = print_new_entries(runtime, conversation, from, None, false).await?;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal.map_err(|error| CliError::persistence(error.to_string()))?; return Ok(()); }
            _ = interval.tick() => {
                reconcile_conversation(runtime, &conversation.conversation_id).await?;
                next = print_new_entries(runtime, conversation, next, None, false).await?;
            }
            line = lines.next_line(), if !read_only => {
                match line.map_err(|error| CliError::persistence(error.to_string()))? {
                    Some(line) if !line.is_empty() => {
                        let message_id = format!("m_{}", uuid::Uuid::new_v4().simple());
                        let request = MessageRequest { request_id: format!("attach-{message_id}"), message_id, content: line };
                        let target = Path::parse(&format!("conversations/{}/messages", conversation.conversation_id)).map_err(|error| CliError::validation(error.to_string()))?;
                        manager(runtime)?.write(&target, parsed(&request)?).await.map_err(map_store_error)?;
                    }
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
        }
    }
}

async fn follow(
    runtime: &Runtime,
    conversation: &RemoteConversationRecord,
    from: i64,
    limit: Option<usize>,
    should_follow: bool,
    jsonl: bool,
) -> Result<(), CliError> {
    let mut next = from;
    loop {
        reconcile_conversation(runtime, &conversation.conversation_id).await?;
        next = print_new_entries(runtime, conversation, next, limit, jsonl).await?;
        if !should_follow {
            return Ok(());
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal.map_err(|error| CliError::persistence(error.to_string()))?; return Ok(()); }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

fn manager(runtime: &Runtime) -> Result<&Arc<dyn StorePort>, CliError> {
    runtime
        .manager
        .as_ref()
        .ok_or_else(|| CliError::persistence("remote manager was not initialized"))
}

fn command_needs_remote(command: &RemoteCommand) -> bool {
    !matches!(
        command,
        RemoteCommand::Node {
            command: NodeCommand::List | NodeCommand::Show { .. },
        } | RemoteCommand::Conversation {
            command: ConversationCommand::List | ConversationCommand::Show { .. },
        } | RemoteCommand::Conversation {
            command: ConversationCommand::Logs { cached: true, .. },
        }
    )
}

async fn print_new_entries(
    runtime: &Runtime,
    conversation: &RemoteConversationRecord,
    from: i64,
    limit: Option<usize>,
    jsonl: bool,
) -> Result<i64, CliError> {
    let mut entries = cached_entries(runtime, &conversation.conversation_id, from).await?;
    if let Some(limit) = limit {
        entries.truncate(limit);
    }
    let mut next = from;
    for entry in entries {
        next = entry.seq.saturating_add(1);
        if jsonl {
            println!(
                "{}",
                serde_json::to_string(&entry)
                    .map_err(|error| CliError::persistence(error.to_string()))?
            );
        } else {
            println!(
                "[{}] {}",
                entry.seq,
                sanitize_terminal(
                    &serde_json::to_string(&entry.msg)
                        .map_err(|error| CliError::persistence(error.to_string()))?
                )
            );
        }
    }
    Ok(next)
}

async fn reconcile_conversation(runtime: &Runtime, id: &str) -> Result<(), CliError> {
    let target = Path::parse(&format!("conversations/{id}/reconcile"))
        .map_err(|error| CliError::validation(error.to_string()))?;
    manager(runtime)?
        .write(
            &target,
            parsed(&serde_json::json!({"request_id": format!("ledger-{id}")}))?,
        )
        .await
        .map_err(map_store_error)?;
    Ok(())
}

async fn cached_entries(
    runtime: &Runtime,
    id: &str,
    from: i64,
) -> Result<Vec<CachedLedgerEntry>, CliError> {
    if from < 0 {
        return Err(CliError::validation("ledger sequence must be non-negative"));
    }
    let base = ox_inbox::remote_state::remote_item_path("conversations", id)
        .map_err(|error| CliError::persistence(error.to_string()))?;
    let target = Path::parse(&format!("{base}/ledger/from/{from}"))
        .map_err(|error| CliError::validation(error.to_string()))?;
    let Some(record) = runtime.local.read(&target).await.map_err(map_store_error)? else {
        return Ok(Vec::new());
    };
    decode(record, "cached ledger")
}

async fn wait_terminal(runtime: &Runtime, id: &str, timeout: Duration) -> Result<(), CliError> {
    let started = tokio::time::Instant::now();
    loop {
        let target = Path::parse(&format!("conversations/{id}/refresh"))
            .map_err(|error| CliError::validation(error.to_string()))?;
        let record = manager(runtime)?
            .read(&target)
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| CliError::not_found("remote conversation disappeared"))?;
        let current: RemoteConversationRecord = decode(record, "conversation refresh")?;
        if is_terminal(&current.observed_state) {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(CliError {
                code: 8,
                kind: "timeout",
                message: "wait timed out while durable work remains active".into(),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn list_nodes(runtime: &Runtime) -> Result<Vec<RemoteNodeRecord>, CliError> {
    let record = runtime
        .local
        .read(&path!("remote/nodes"))
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| CliError::persistence("node listing missing"))?;
    decode(record, "node listing")
}

async fn list_conversations(runtime: &Runtime) -> Result<Vec<RemoteConversationRecord>, CliError> {
    let record = runtime
        .local
        .read(&path!("remote/conversations"))
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| CliError::persistence("conversation listing missing"))?;
    decode(record, "conversation listing")
}

async fn resolve_node(runtime: &Runtime, reference: &str) -> Result<RemoteNodeRecord, CliError> {
    let mut matches: Vec<_> = list_nodes(runtime)
        .await?
        .into_iter()
        .filter(|node| {
            node.node_id == reference
                || node.vm_name == reference
                || node.node_id.starts_with(reference)
        })
        .collect();
    if matches.len() == 1 {
        Ok(matches.remove(0))
    } else if matches.is_empty() {
        Err(CliError::not_found(format!(
            "remote node {reference:?} was not found"
        )))
    } else {
        Err(CliError::not_found(format!(
            "remote node prefix {reference:?} is ambiguous"
        )))
    }
}

async fn resolve_conversation(
    runtime: &Runtime,
    reference: &str,
) -> Result<RemoteConversationRecord, CliError> {
    let mut matches: Vec<_> = list_conversations(runtime)
        .await?
        .into_iter()
        .filter(|item| {
            item.conversation_id == reference
                || item.worker_thread_id.as_deref() == Some(reference)
                || item.conversation_id.starts_with(reference)
        })
        .collect();
    if matches.len() == 1 {
        Ok(matches.remove(0))
    } else if matches.is_empty() {
        Err(CliError::not_found(format!(
            "remote conversation {reference:?} was not found"
        )))
    } else {
        Err(CliError::not_found(format!(
            "remote conversation prefix {reference:?} is ambiguous"
        )))
    }
}

async fn print_conversation(
    runtime: &Runtime,
    conversation: &RemoteConversationRecord,
    json: bool,
) -> Result<(), CliError> {
    let node = resolve_node(runtime, &conversation.node_id).await?;
    let value = conversation_view_with_node(conversation, &node);
    if json {
        print_json(&value)
    } else {
        println!(
            "conversation: {}\nnode:         {}\nvm:           {}\nstate:        {}\nattach:       ox remote conversation attach {}",
            sanitize_terminal(&conversation.conversation_id),
            sanitize_terminal(&node.node_id),
            sanitize_terminal(&node.vm_name),
            sanitize_terminal(&conversation.observed_state),
            sanitize_terminal(&conversation.conversation_id)
        );
        Ok(())
    }
}

async fn conversation_view(
    runtime: &Runtime,
    conversation: &RemoteConversationRecord,
) -> Result<serde_json::Value, CliError> {
    let node = resolve_node(runtime, &conversation.node_id).await?;
    Ok(conversation_view_with_node(conversation, &node))
}

fn conversation_view_with_node(
    conversation: &RemoteConversationRecord,
    node: &RemoteNodeRecord,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "conversation_id": conversation.conversation_id,
        "title": conversation.title,
        "state": conversation.observed_state,
        "desired_state": conversation.desired_state,
        "node": node_view(node),
        "conversation_path": conversation.worker_thread_id.as_ref().map(|id| format!("conversations/{id}")),
        "cleanup_state": conversation.cleanup_state,
    })
}

fn print_node(node: &RemoteNodeRecord, json: bool) -> Result<(), CliError> {
    let value = node_view(node);
    if json {
        print_json(&value)
    } else {
        println!(
            "node:  {}\nvm:    {}\nstate: {} / {}",
            sanitize_terminal(&node.node_id),
            sanitize_terminal(&node.vm_name),
            sanitize_terminal(&node.desired_state),
            sanitize_terminal(&node.observed_state)
        );
        Ok(())
    }
}

fn node_view(node: &RemoteNodeRecord) -> serde_json::Value {
    serde_json::json!({
        "node_id": node.node_id,
        "node_attempt_id": node.node_attempt_id,
        "provider": node.provider,
        "vm_name": node.vm_name,
        "ssh_host": node.ssh_host,
        "ssh_port": node.ssh_port,
        "ssh_user": node.ssh_user,
        "desired_state": node.desired_state,
        "observed_state": node.observed_state,
        "cleanup_state": node.cleanup_state,
        "image_digest": node.image_digest,
    })
}

fn provision_spec(
    config: &RemoteFileConfig,
    image: Option<&String>,
    cpu: Option<u16>,
    memory_mib: Option<u32>,
    disk_gib: Option<u32>,
) -> Result<NodeProvisionSpec, CliError> {
    let image = image
        .cloned()
        .unwrap_or_else(|| config.exe.worker_image.clone());
    validate_image(&image)?;
    let spec = NodeProvisionSpec {
        image,
        cpu: cpu.unwrap_or(config.defaults.cpu),
        memory_mib: memory_mib.unwrap_or(config.defaults.memory_mib),
        disk_gib: disk_gib.unwrap_or(config.defaults.disk_gib),
    };
    if spec.cpu == 0
        || spec.cpu > 128
        || spec.memory_mib < 1024
        || spec.memory_mib % 1024 != 0
        || spec.disk_gib == 0
    {
        return Err(CliError::validation("invalid node resources"));
    }
    Ok(spec)
}

fn read_prompt(
    inline: Option<&str>,
    file: Option<&FsPath>,
    stdin: bool,
) -> Result<String, CliError> {
    let sources = usize::from(inline.is_some()) + usize::from(file.is_some()) + usize::from(stdin);
    if sources != 1 {
        return Err(CliError::validation(
            "exactly one of --prompt, --prompt-file, or --stdin is required",
        ));
    }
    let prompt = if let Some(prompt) = inline {
        prompt.to_owned()
    } else if let Some(path) = file {
        std::fs::read_to_string(path)
            .map_err(|error| CliError::validation(format!("prompt file: {error}")))?
    } else {
        let mut prompt = String::new();
        std::io::stdin()
            .read_to_string(&mut prompt)
            .map_err(|error| CliError::validation(error.to_string()))?;
        prompt
    };
    if prompt.is_empty() || prompt.len() > 4 * 1024 * 1024 {
        return Err(CliError::validation(
            "prompt must contain 1..=4194304 bytes",
        ));
    }
    Ok(prompt)
}

fn load_config(root: &FsPath, args: &RemoteArgs) -> Result<RemoteFileConfig, CliError> {
    let path = std::env::var_os("OX_REMOTE_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("remote.toml"));
    let mut config = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|error| CliError::validation(format!("{}: {error}", path.display())))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RemoteFileConfig::default(),
        Err(error) => {
            return Err(CliError::persistence(format!(
                "{}: {error}",
                path.display()
            )));
        }
    };
    if let Ok(value) = std::env::var("OX_REMOTE__EXE__CONTROL_HOST") {
        config.exe.control_host = value;
    }
    if let Ok(value) = std::env::var("OX_REMOTE__EXE__CONTROL_USER") {
        config.exe.control_user = value;
    }
    if let Ok(value) = std::env::var("OX_REMOTE__EXE__WORKER_IMAGE") {
        config.exe.worker_image = value;
    }
    if let Ok(value) = std::env::var("OX_REMOTE__EXE__IDENTITY") {
        config.exe.identity = Some(value.into());
    }
    if let Some(identity) = &args.identity {
        config.exe.identity = Some(identity.clone());
    }
    Ok(config)
}

fn ox_root() -> Result<PathBuf, CliError> {
    let home = std::env::var_os("HOME").ok_or_else(|| CliError::persistence("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".ox"))
}

fn expand_tilde(path: PathBuf) -> Result<PathBuf, CliError> {
    let text = path.to_string_lossy();
    if text == "~" || text.starts_with("~/") {
        let home =
            std::env::var_os("HOME").ok_or_else(|| CliError::persistence("HOME is not set"))?;
        return Ok(if text == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(&text[2..])
        });
    }
    Ok(path)
}

fn validate_image(value: &str) -> Result<(), CliError> {
    let digest = value
        .rsplit_once("@sha256:")
        .map(|(_, digest)| digest)
        .ok_or_else(|| {
            CliError::validation("worker image must be pinned as name@sha256:<64 hex>")
        })?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CliError::validation(
            "worker image must be pinned as name@sha256:<64 hex>",
        ));
    }
    Ok(())
}

fn parse_duration(value: &str) -> Result<Duration, CliError> {
    let (number, multiplier) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1_u64)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else {
        return Err(CliError::validation("duration must end in ms, s, m, or h"));
    };
    let amount: u64 = number
        .parse()
        .map_err(|_| CliError::validation("duration amount is invalid"))?;
    let millis = amount
        .checked_mul(multiplier)
        .ok_or_else(|| CliError::validation("duration overflow"))?;
    if millis == 0 {
        return Err(CliError::validation("duration must be non-zero"));
    }
    Ok(Duration::from_millis(millis))
}

fn new_request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}

fn parsed<T: Serialize>(value: &T) -> Result<Record, CliError> {
    structfs_serde_store::to_value(value)
        .map(Record::parsed)
        .map_err(|error| CliError::persistence(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(record: Record, label: &str) -> Result<T, CliError> {
    let value = record
        .as_value()
        .cloned()
        .ok_or_else(|| CliError::persistence(format!("{label} was not parsed")))?;
    structfs_serde_store::from_value(value)
        .map_err(|error| CliError::persistence(format!("{label}: {error}")))
}

fn record_json(record: Record) -> Result<serde_json::Value, CliError> {
    record
        .as_value()
        .cloned()
        .map(structfs_serde_store::value_to_json)
        .ok_or_else(|| CliError::persistence("record was not parsed"))
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(|error| CliError::persistence(error.to_string()))?
    );
    Ok(())
}

fn sanitize_terminal(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "completed" | "canceled" | "errored" | "lost")
}

fn map_store_error(error: structfs_core_store::Error) -> CliError {
    classify_error(error.to_string())
}

fn map_manager_error(error: RemoteManagerError) -> CliError {
    classify_error(error.to_string())
}

fn map_exe_error(error: ox_remote::ExeError) -> CliError {
    classify_error(error.to_string())
}

fn classify_error(message: String) -> CliError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("host key")
        || lower.contains("authentication")
        || lower.contains("identity must")
    {
        CliError::auth(message)
    } else if lower.contains("conflict") || lower.contains("mismatch") || lower.contains("lease") {
        CliError {
            code: 6,
            kind: "conflict",
            message,
        }
    } else if lower.contains("unavailable")
        || lower.contains("disconnected")
        || lower.contains("deadline")
        || lower.contains("timeout")
    {
        CliError::unavailable(message)
    } else if lower.contains("unknown") || lower.contains("not found") || lower.contains("missing")
    {
        CliError::not_found(message)
    } else if lower.contains("invalid") || lower.contains("unsupported") {
        CliError::validation(message)
    } else {
        CliError::persistence(message)
    }
}

impl CliError {
    fn auth(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            kind: "unauthenticated",
            message: message.into(),
        }
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: 5,
            kind: "unavailable",
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct RemoteHarness {
        #[command(flatten)]
        remote: RemoteArgs,
    }

    #[test]
    fn pinned_images_and_durations_are_fail_closed() {
        assert!(validate_image("registry/worker@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_image("registry/worker:latest").is_err());
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert!(parse_duration("30").is_err());
    }

    #[test]
    fn terminal_text_cannot_emit_escape_controls() {
        assert_eq!(
            sanitize_terminal("ok\u{1b}[31m\n\t\u{7}bad"),
            "ok�[31m���bad"
        );
    }

    #[test]
    fn command_grammar_parses_nested_headless_operations() {
        let parsed = RemoteHarness::try_parse_from([
            "test",
            "--json",
            "conversation",
            "cancel",
            "c_deadbeef",
            "--cancel-id",
            "cancel_retry",
        ])
        .unwrap();
        assert!(parsed.remote.json);
        assert!(matches!(
            parsed.remote.command,
            RemoteCommand::Conversation {
                command: ConversationCommand::Cancel { .. }
            }
        ));
    }

    #[tokio::test]
    async fn local_list_needs_no_identity_or_worker_image() {
        let root = tempfile::tempdir().unwrap();
        let args = RemoteArgs {
            json: true,
            identity: None,
            connect_timeout: None,
            operation_timeout: None,
            accept_new_host_key: false,
            command: RemoteCommand::Node {
                command: NodeCommand::List,
            },
        };
        let runtime = build_runtime(root.path(), RemoteFileConfig::default(), &args, false)
            .await
            .unwrap();
        assert!(runtime.manager.is_none());
        assert!(list_nodes(&runtime).await.unwrap().is_empty());
    }
}
