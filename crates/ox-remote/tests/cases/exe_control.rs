use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_remote::{
    CommandError, CommandOutput, DeleteVmRequest, ExeCommand, ExeCommandRunner, ExeControlStore,
    ExeError, ExeSshConfig, RusshExeRunner, SshWorkerConnector, SshWorkerIdentityVerifier, VmSpec,
    VmStatus, WorkerIdentityVerifier, WorkerStoreConnector,
};
use ox_structfs_transport::{HostKeyEnrollment, KnownHosts, RemoteStoreConfig};
use russh::keys::ssh_key::{Algorithm, LineEnding, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use structfs_core_store::{Format, Path, Record, Value, path};

#[derive(Clone)]
struct FakeExe {
    commands: Arc<Mutex<Vec<ExeCommand>>>,
    results: Arc<Mutex<VecDeque<Result<CommandOutput, CommandError>>>>,
}

impl FakeExe {
    fn new(results: Vec<Result<CommandOutput, CommandError>>) -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            results: Arc::new(Mutex::new(results.into())),
        }
    }

    fn commands(&self) -> Vec<ExeCommand> {
        self.commands.lock().unwrap().clone()
    }
}

#[async_trait]
impl ExeCommandRunner for FakeExe {
    async fn run(&self, command: ExeCommand) -> Result<CommandOutput, CommandError> {
        self.commands.lock().unwrap().push(command);
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .expect("fake result")
    }
}

#[derive(Clone, Copy)]
enum SshBehavior {
    Success,
    RejectAuth,
    RejectExec,
    NonzeroExit,
    NoAcknowledgement,
    NoExitStatus,
}

#[derive(Clone)]
struct TestSshServer(SshBehavior);

impl server::Server for TestSshServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        self.clone()
    }
}

impl server::Handler for TestSshServer {
    type Error = russh::Error;
    async fn auth_publickey(&mut self, _: &str, _: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(if matches!(self.0, SshBehavior::RejectAuth) {
            Auth::reject()
        } else {
            Auth::Accept
        })
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if matches!(self.0, SshBehavior::RejectExec) {
            session.channel_failure(channel)?;
            session.close(channel)?;
            return Ok(());
        }
        if !matches!(self.0, SshBehavior::NoAcknowledgement) {
            session.channel_success(channel)?;
        }
        session.data(channel, b"result".to_vec())?;
        session.extended_data(channel, 1, b"diagnostic".to_vec())?;
        if !matches!(self.0, SshBehavior::NoExitStatus) {
            let status = if matches!(self.0, SshBehavior::NonzeroExit) {
                17
            } else {
                0
            };
            session.exit_status_request(channel, status)?;
        }
        session.close(channel)?;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeIdentity {
    accepted: bool,
    checks: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl FakeIdentity {
    fn new(accepted: bool) -> Self {
        Self {
            accepted,
            checks: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl WorkerIdentityVerifier for FakeIdentity {
    async fn verify(
        &self,
        vm: &VmStatus,
        node_id: &str,
        node_attempt_id: &str,
    ) -> Result<bool, ExeError> {
        self.checks.lock().unwrap().push((
            vm.vm_name.clone(),
            node_id.into(),
            node_attempt_id.into(),
        ));
        Ok(self.accepted)
    }
}

async fn run_test_ssh(behavior: SshBehavior) -> Option<Result<CommandOutput, CommandError>> {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let identity_file = root.path().join("id_ed25519");
    let client_key = russh::keys::PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    std::fs::write(
        &identity_file,
        client_key.to_openssh(LineEnding::LF).unwrap().as_bytes(),
    )
    .unwrap();
    std::fs::set_permissions(&identity_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(error) => panic!("bind test SSH listener: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let server_key = russh::keys::PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut server = TestSshServer(behavior);
    let running = server.run_on_socket(
        Arc::new(server::Config {
            auth_rejection_time: std::time::Duration::ZERO,
            auth_rejection_time_initial: Some(std::time::Duration::ZERO),
            keys: vec![server_key],
            ..Default::default()
        }),
        &listener,
    );
    let handle = running.handle();
    let client = async {
        let runner = RusshExeRunner::new(ExeSshConfig {
            host: "127.0.0.1".into(),
            port: address.port(),
            user: "test".into(),
            identity_file,
            known_hosts: KnownHosts::new(
                root.path().join("known_hosts"),
                HostKeyEnrollment::EnrollNew,
            ),
            inactivity_timeout: std::time::Duration::from_secs(5),
            operation_timeout: std::time::Duration::from_secs(5),
        })
        .unwrap()
        .run(ExeCommand::Identity)
        .await;
        handle.shutdown("test complete".into());
        runner
    };
    let (output, server_result) = tokio::join!(client, running);
    server_result.unwrap();
    Some(output)
}

#[tokio::test]
async fn russh_runner_executes_an_authenticated_control_command_end_to_end() {
    let Some(output) = run_test_ssh(SshBehavior::Success).await else {
        return;
    };
    let output = output.unwrap();
    assert_eq!(output.stdout, b"result");
    assert_eq!(output.stderr, b"diagnostic");
}

#[tokio::test]
async fn russh_runner_classifies_protocol_rejections_and_incomplete_results() {
    for (behavior, expected) in [
        (SshBehavior::RejectAuth, "authentication rejected"),
        (SshBehavior::RejectExec, "exec rejected"),
        (SshBehavior::NonzeroExit, "exit status 17"),
        (SshBehavior::NoAcknowledgement, "no exec acknowledgement"),
        (SshBehavior::NoExitStatus, "no exit status"),
    ] {
        let Some(result) = run_test_ssh(behavior).await else {
            return;
        };
        assert!(
            result.unwrap_err().to_string().contains(expected),
            "expected {expected}"
        );
    }
}

#[tokio::test]
async fn production_worker_ssh_adapters_surface_authenticated_transport_rejection() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let identity_file = root.path().join("id_ed25519");
    let client_key = russh::keys::PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    std::fs::write(
        &identity_file,
        client_key.to_openssh(LineEnding::LF).unwrap().as_bytes(),
    )
    .unwrap();
    std::fs::set_permissions(&identity_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("bind test SSH listener: {error}"),
    };
    let address = listener.local_addr().unwrap();
    let server_key = russh::keys::PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut server = TestSshServer(SshBehavior::RejectAuth);
    let running = server.run_on_socket(
        Arc::new(server::Config {
            auth_rejection_time: std::time::Duration::ZERO,
            auth_rejection_time_initial: Some(std::time::Duration::ZERO),
            keys: vec![server_key],
            ..Default::default()
        }),
        &listener,
    );
    let handle = running.handle();
    let client = async {
        let node = ox_inbox::remote_state::RemoteNodeRecord {
            node_id: "n_transport".into(),
            node_attempt_id: "a_transport".into(),
            provider: "exe.dev".into(),
            vm_name: "ox-transport".into(),
            ssh_host: Some("127.0.0.1".into()),
            ssh_port: i64::from(address.port()),
            ssh_user: Some("test".into()),
            ssh_dest: Some("test@127.0.0.1".into()),
            identity_path: identity_file.to_string_lossy().into_owned(),
            known_hosts_path: root
                .path()
                .join("known_hosts")
                .to_string_lossy()
                .into_owned(),
            worker_socket_path: "/tmp/worker.sock".into(),
            desired_state: "active".into(),
            observed_state: "ready".into(),
            cleanup_state: "none".into(),
            image_digest: Some("worker@sha256:abc".into()),
        };
        let connector = SshWorkerConnector {
            enrollment: HostKeyEnrollment::EnrollNew,
            inactivity_timeout: std::time::Duration::from_secs(5),
            remote: RemoteStoreConfig::default(),
        };
        assert!(connector.connect(&node).await.is_err());
        let verifier = SshWorkerIdentityVerifier {
            identity_path: identity_file,
            known_hosts_path: root.path().join("known_hosts"),
            worker_socket_path: "/tmp/worker.sock".into(),
            ssh_port: address.port(),
            enrollment: HostKeyEnrollment::EnrollNew,
            inactivity_timeout: std::time::Duration::from_secs(5),
            remote: RemoteStoreConfig::default(),
        };
        let vm = VmStatus {
            schema_version: 1,
            vm_name: node.vm_name,
            status: "running".into(),
            ssh_dest: "test@127.0.0.1".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_user: Some("test".into()),
        };
        assert!(
            verifier
                .verify(&vm, &node.node_id, &node.node_attempt_id)
                .await
                .is_err()
        );
        handle.shutdown("test complete".into());
    };
    let (_, server_result) = tokio::join!(client, running);
    server_result.unwrap();
}

fn output(json: &str) -> Result<CommandOutput, CommandError> {
    Ok(CommandOutput {
        stdout: json.as_bytes().to_vec(),
        stderr: Vec::new(),
    })
}

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
        comment: "ox remote node".into(),
        tags: vec!["ox".into()],
        integrations: vec!["github".into()],
    }
}

fn found_list() -> &'static str {
    r#"{"vms":[{"vm_name":"ox-deadbeef","status":"running","ssh_dest":"vm+ox-deadbeef@vm.exe.xyz","ssh_host":"vm.exe.xyz","ssh_user":"vm+ox-deadbeef"}]}"#
}

fn store(fake: FakeExe, identity: FakeIdentity) -> ExeControlStore {
    ExeControlStore::new(Arc::new(fake), Arc::new(identity))
}

fn delete_request() -> DeleteVmRequest {
    DeleteVmRequest {
        schema_version: 1,
        deletion_id: "d_789".into(),
        node_id: "n_123".into(),
        node_attempt_id: "na_456".into(),
    }
}

#[tokio::test]
async fn ambiguous_create_reconciles_exact_name_once_and_uses_returned_ssh_fields() {
    let fake = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        Err(CommandError::Ambiguous("timeout after send".into())),
        output(found_list()),
    ]);
    let identity = FakeIdentity::new(true);
    let control = store(fake.clone(), identity.clone());

    let vm = control.create(spec()).await.unwrap();
    assert_eq!(vm.ssh_host, "vm.exe.xyz");
    assert_eq!(vm.ssh_user.as_deref(), Some("vm+ox-deadbeef"));
    assert_eq!(vm.ssh_dest, "vm+ox-deadbeef@vm.exe.xyz");
    let commands = fake.commands();
    assert_eq!(commands.len(), 3);
    assert!(matches!(commands[0], ExeCommand::List { .. }));
    assert!(matches!(commands[1], ExeCommand::Create(_)));
    assert_eq!(
        commands[2],
        ExeCommand::List {
            exact_name: Some("ox-deadbeef".into())
        }
    );
    assert_eq!(identity.checks.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ambiguous_create_absent_never_blindly_retries() {
    let fake = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        Err(CommandError::Ambiguous("lost response".into())),
        output(r#"{"vms":[]}"#),
    ]);
    let control = store(fake.clone(), FakeIdentity::new(true));
    assert!(matches!(
        control.create(spec()).await,
        Err(ExeError::CreateUnresolved)
    ));
    assert_eq!(
        fake.commands()
            .iter()
            .filter(|command| matches!(command, ExeCommand::Create(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn mismatched_attempt_is_neither_adopted_nor_deleted() {
    let create_fake = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        output(r#"{"accepted":true}"#),
        output(found_list()),
    ]);
    let control = store(create_fake.clone(), FakeIdentity::new(false));
    assert!(matches!(
        control.create(spec()).await,
        Err(ExeError::IdentityMismatch)
    ));

    let delete_fake = FakeExe::new(vec![output(found_list())]);
    let control = store(delete_fake.clone(), FakeIdentity::new(false));
    let request = DeleteVmRequest {
        schema_version: 1,
        deletion_id: "d_789".into(),
        node_id: "n_123".into(),
        node_attempt_id: "na_456".into(),
    };
    assert!(matches!(
        control.remove("ox-deadbeef", request).await,
        Err(ExeError::IdentityMismatch)
    ));
    assert!(
        !delete_fake
            .commands()
            .iter()
            .any(|command| matches!(command, ExeCommand::Remove { .. }))
    );
}

#[tokio::test]
async fn ambiguous_delete_proves_absence_after_one_remove() {
    let fake = FakeExe::new(vec![
        output(found_list()),
        Err(CommandError::Ambiguous("timeout after accept".into())),
        output(r#"{"vms":[]}"#),
    ]);
    let control = store(fake.clone(), FakeIdentity::new(true));
    control
        .remove(
            "ox-deadbeef",
            DeleteVmRequest {
                schema_version: 1,
                deletion_id: "d_789".into(),
                node_id: "n_123".into(),
                node_attempt_id: "na_456".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fake.commands()
            .iter()
            .filter(|command| matches!(command, ExeCommand::Remove { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn existing_exact_vm_is_verified_and_adopted_without_create() {
    let fake = FakeExe::new(vec![output(found_list())]);
    let identity = FakeIdentity::new(true);
    let vm = store(fake.clone(), identity.clone())
        .create(spec())
        .await
        .unwrap();
    assert_eq!(vm.vm_name, "ox-deadbeef");
    assert_eq!(fake.commands().len(), 1);
    assert!(matches!(fake.commands()[0], ExeCommand::List { .. }));
    assert_eq!(identity.checks.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn definite_create_rejection_is_not_reconciled_or_retried() {
    let fake = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        Err(CommandError::Rejected("quota".into())),
    ]);
    assert!(matches!(
        store(fake.clone(), FakeIdentity::new(true))
            .create(spec())
            .await,
        Err(ExeError::Rejected(_))
    ));
    assert_eq!(fake.commands().len(), 2);
}

#[tokio::test]
async fn already_absent_delete_is_distinct_and_sends_no_remove() {
    let fake = FakeExe::new(vec![output(r#"{"vms":[]}"#)]);
    let outcome = store(fake.clone(), FakeIdentity::new(true))
        .remove(
            "ox-deadbeef",
            DeleteVmRequest {
                schema_version: 1,
                deletion_id: "d_789".into(),
                node_id: "n_123".into(),
                node_attempt_id: "na_456".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, ox_remote::DeleteOutcome::AlreadyAbsent);
    assert_eq!(fake.commands().len(), 1);
}

#[tokio::test]
async fn mutation_ack_accepts_any_single_json_value_but_rejects_trailing_junk() {
    let scalar = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        output("true"),
        output(found_list()),
    ]);
    store(scalar, FakeIdentity::new(true))
        .create(spec())
        .await
        .unwrap();

    let trailing = FakeExe::new(vec![output(r#"{"vms":[]}"#), output("{} {}")]);
    assert!(matches!(
        store(trailing, FakeIdentity::new(true))
            .create(spec())
            .await,
        Err(ExeError::Malformed(_))
    ));
}

#[tokio::test]
async fn malformed_or_duplicate_exact_provider_output_fails_closed() {
    let malformed = FakeExe::new(vec![output("not-json")]);
    assert!(matches!(
        store(malformed, FakeIdentity::new(true)).list().await,
        Err(ExeError::Malformed(_))
    ));

    let duplicate = FakeExe::new(vec![output(&format!(
        r#"{{"vms":[{},{}]}}"#,
        &found_list()[8..found_list().len() - 2],
        &found_list()[8..found_list().len() - 2]
    ))]);
    assert!(matches!(
        store(duplicate, FakeIdentity::new(true))
            .find_exact("ox-deadbeef")
            .await,
        Err(ExeError::DuplicateExactName)
    ));
}

#[tokio::test]
async fn provider_json_is_bounded_and_never_synthesizes_missing_ssh_host() {
    let oversized = FakeExe::new(vec![Ok(CommandOutput {
        stdout: vec![b' '; 1024 * 1024 + 1],
        stderr: Vec::new(),
    })]);
    assert!(matches!(
        store(oversized, FakeIdentity::new(true)).list().await,
        Err(ExeError::Malformed(_))
    ));

    let missing_host = FakeExe::new(vec![output(
        r#"{"vms":[{"vm_name":"ox-deadbeef","status":"running","ssh_dest":"vm+ox-deadbeef@vm.exe.xyz"}]}"#,
    )]);
    assert!(matches!(
        store(missing_host, FakeIdentity::new(true)).list().await,
        Err(ExeError::Malformed(_))
    ));
}

#[tokio::test]
async fn whoami_projects_only_typed_authenticated_fact() {
    let fake = FakeExe::new(vec![output(
        r#"{"email":"private@example.test","keys":["secret-ish metadata"]}"#,
    )]);
    let identity = store(fake, FakeIdentity::new(true))
        .authenticated_identity()
        .await
        .unwrap();
    assert_eq!(identity.schema_version, 1);
    assert!(identity.authenticated);
}

#[tokio::test]
async fn structfs_routes_expose_only_typed_provider_operations() {
    let fake = FakeExe::new(vec![
        output(r#"{"email":"private@example.test","keys":["secret"]}"#),
        output(found_list()),
        output(found_list()),
    ]);
    let mut control = store(fake, FakeIdentity::new(true));

    let identity = control.read(&path!("identity")).await.unwrap().unwrap();
    let identity: ox_remote::ExeIdentity =
        structfs_serde_store::from_value(identity.as_value().unwrap().clone()).unwrap();
    assert!(identity.authenticated);

    let listed = control.read(&path!("vms")).await.unwrap().unwrap();
    let listed: Vec<VmStatus> =
        structfs_serde_store::from_value(listed.as_value().unwrap().clone()).unwrap();
    assert_eq!(listed.len(), 1);

    let item = ox_remote::vm_path("ox-deadbeef").unwrap();
    let exact = control.read(&item).await.unwrap().unwrap();
    let exact: VmStatus =
        structfs_serde_store::from_value(exact.as_value().unwrap().clone()).unwrap();
    assert_eq!(exact.vm_name, "ox-deadbeef");
    assert!(
        control
            .read(&path!("private/value"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn structfs_mutation_routes_validate_decode_and_return_stable_paths() {
    let create_fake = FakeExe::new(vec![
        output(r#"{"vms":[]}"#),
        output(r#"{"accepted":true}"#),
        output(found_list()),
    ]);
    let mut create = store(create_fake, FakeIdentity::new(true));
    let created = create
        .write(
            &path!("vms"),
            Record::parsed(structfs_serde_store::to_value(&spec()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(created, ox_remote::vm_path("ox-deadbeef").unwrap());
    assert!(
        create
            .write(&path!("vms"), Record::parsed(Value::Null))
            .await
            .is_err()
    );
    assert!(
        create
            .write(&path!("private"), Record::parsed(Value::Null))
            .await
            .is_err()
    );

    let delete_fake = FakeExe::new(vec![
        output(found_list()),
        output(r#"{"accepted":true}"#),
        output(r#"{"vms":[]}"#),
    ]);
    let mut delete = store(delete_fake, FakeIdentity::new(true));
    let target = ox_remote::vm_delete_path("ox-deadbeef").unwrap();
    let deleted = delete
        .write(
            &target,
            Record::parsed(
                structfs_serde_store::to_value(&DeleteVmRequest {
                    schema_version: 1,
                    deletion_id: "delete_route".into(),
                    node_id: "n_123".into(),
                    node_attempt_id: "na_456".into(),
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        deleted,
        Path::parse(&format!(
            "{}/deletions/delete_route",
            ox_remote::vm_path("ox-deadbeef").unwrap()
        ))
        .unwrap()
    );
}

#[tokio::test]
async fn provider_command_validation_rejects_unsafe_or_invalid_arguments() {
    let valid = spec();

    let mut cases = Vec::new();
    let mut invalid = valid.clone();
    invalid.name = "../escape".into();
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.node_id = "node with spaces".into();
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.image = "bad image".into();
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.cpu = 0;
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.comment = "line\nbreak".into();
    cases.push(invalid);
    let mut invalid = valid;
    invalid.tags.push("bad tag".into());
    cases.push(invalid);

    for invalid in cases {
        assert!(
            store(FakeExe::new(Vec::new()), FakeIdentity::new(true))
                .create(invalid)
                .await
                .is_err()
        );
    }
    assert!(
        store(FakeExe::new(Vec::new()), FakeIdentity::new(true))
            .remove(
                "../escape",
                DeleteVmRequest {
                    schema_version: 1,
                    deletion_id: "delete-invalid".into(),
                    node_id: "n_123".into(),
                    node_attempt_id: "na_456".into(),
                },
            )
            .await
            .is_err()
    );
    assert!(ox_remote::decode_vm_component("not-encoded").is_err());
    assert_eq!(
        ox_remote::decode_vm_component(&ox_remote::encode_vm_component("ox-deadbeef")).unwrap(),
        "ox-deadbeef"
    );
}

#[tokio::test]
async fn provider_transport_failures_remain_typed_by_operation_certainty() {
    for error in [
        CommandError::Ambiguous("list may have run".into()),
        CommandError::Unavailable("not connected".into()),
    ] {
        assert!(matches!(
            store(FakeExe::new(vec![Err(error)]), FakeIdentity::new(true))
                .list()
                .await,
            Err(ExeError::Unavailable(_))
        ));
    }
    assert!(matches!(
        store(
            FakeExe::new(vec![
                output(r#"{"vms":[]}"#),
                Err(CommandError::Unavailable("not accepted".into())),
            ]),
            FakeIdentity::new(true),
        )
        .create(spec())
        .await,
        Err(ExeError::Unavailable(_))
    ));
    assert!(matches!(
        store(
            FakeExe::new(vec![Err(CommandError::Rejected("bad query".into()))]),
            FakeIdentity::new(true),
        )
        .authenticated_identity()
        .await,
        Err(ExeError::Rejected(_))
    ));
}

#[tokio::test]
async fn delete_requires_a_valid_identity_and_proven_provider_absence() {
    for mut request in [
        DeleteVmRequest {
            schema_version: 2,
            ..delete_request()
        },
        DeleteVmRequest {
            node_id: "bad node".into(),
            ..delete_request()
        },
        DeleteVmRequest {
            node_attempt_id: String::new(),
            ..delete_request()
        },
        DeleteVmRequest {
            deletion_id: "bad/delete".into(),
            ..delete_request()
        },
    ] {
        assert!(matches!(
            store(FakeExe::new(vec![]), FakeIdentity::new(true))
                .remove("ox-deadbeef", request.clone())
                .await,
            Err(ExeError::Invalid(_))
        ));
        request.schema_version = 1;
    }

    for error in [
        CommandError::Rejected("remove rejected".into()),
        CommandError::Unavailable("remove unavailable".into()),
    ] {
        assert!(matches!(
            store(
                FakeExe::new(vec![output(found_list()), Err(error)]),
                FakeIdentity::new(true),
            )
            .remove("ox-deadbeef", delete_request())
            .await,
            Err(ExeError::Rejected(_) | ExeError::Unavailable(_))
        ));
    }

    let unresolved = FakeExe::new(vec![
        output(found_list()),
        output(r#"{"accepted":true}"#),
        output(found_list()),
    ]);
    assert!(matches!(
        store(unresolved, FakeIdentity::new(true))
            .remove("ox-deadbeef", delete_request())
            .await,
        Err(ExeError::DeleteUnresolved)
    ));

    let malformed_ack = FakeExe::new(vec![output(found_list()), output("not-json")]);
    assert!(matches!(
        store(malformed_ack, FakeIdentity::new(true))
            .remove("ox-deadbeef", delete_request())
            .await,
        Err(ExeError::Malformed(_))
    ));
}

#[tokio::test]
async fn provider_payload_validation_rejects_ambiguous_or_unsafe_fields() {
    let invalid_lists = [
        r#"{"vms":[{"vm_name":"ox-deadbeef","status":"","ssh_dest":"u@h","ssh_host":"h"}]}"#,
        r#"{"vms":[{"vm_name":"ox-deadbeef","status":"running","ssh_dest":"","ssh_host":"h"}]}"#,
        r#"{"vms":[{"vm_name":"ox-deadbeef","status":"running","ssh_dest":"u@h","ssh_host":"bad host"}]}"#,
        r#"{"vms":[{"vm_name":"ox-deadbeef","status":"running","ssh_dest":"u@h","ssh_host":"h","ssh_user":"bad user"}]}"#,
        r#"{"vms":[{"vm_name":"not-ox","status":"running","ssh_dest":"u@h","ssh_host":"h"}]}"#,
    ];
    for payload in invalid_lists {
        assert!(matches!(
            store(FakeExe::new(vec![output(payload)]), FakeIdentity::new(true))
                .list()
                .await,
            Err(ExeError::Invalid(_) | ExeError::Malformed(_))
        ));
    }

    assert!(matches!(
        store(FakeExe::new(vec![output("[]")]), FakeIdentity::new(true))
            .authenticated_identity()
            .await,
        Err(ExeError::Malformed(_))
    ));
}

#[tokio::test]
async fn structfs_routes_fail_closed_for_missing_and_unparsed_records() {
    let mut reader = store(
        FakeExe::new(vec![output(r#"{"vms":[]}"#)]),
        FakeIdentity::new(true),
    );
    let item = ox_remote::vm_path("ox-deadbeef").unwrap();
    assert!(reader.read(&item).await.unwrap().is_none());
    assert!(reader.read(&path!("vms/not_hex")).await.is_err());

    let mut writer = store(FakeExe::new(vec![]), FakeIdentity::new(true));
    assert!(
        writer
            .write(
                &path!("vms"),
                Record::raw(vec![1, 2, 3], Format::OCTET_STREAM),
            )
            .await
            .is_err()
    );
    assert!(
        writer
            .write(
                &ox_remote::vm_delete_path("ox-deadbeef").unwrap(),
                Record::parsed(Value::Null),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn vm_spec_validates_every_resource_and_collection_boundary() {
    let mut cases = Vec::new();
    for mutate in [
        |value: &mut VmSpec| value.schema_version = 2,
        |value: &mut VmSpec| value.cpu = 129,
        |value: &mut VmSpec| value.memory_mib = 255,
        |value: &mut VmSpec| value.memory_mib = 1_048_577,
        |value: &mut VmSpec| value.memory_mib = 1025,
        |value: &mut VmSpec| value.disk_gib = 0,
        |value: &mut VmSpec| value.disk_gib = 65_537,
    ] {
        let mut value = spec();
        mutate(&mut value);
        cases.push(value);
    }
    let mut too_many_tags = spec();
    too_many_tags.tags = vec!["tag".into(); 33];
    cases.push(too_many_tags);
    let mut too_many_integrations = spec();
    too_many_integrations.integrations = vec!["integration".into(); 33];
    cases.push(too_many_integrations);
    let mut invalid_integration = spec();
    invalid_integration.integrations = vec!["bad integration".into()];
    cases.push(invalid_integration);
    let mut long_comment = spec();
    long_comment.comment = "x".repeat(201);
    cases.push(long_comment);

    for invalid in cases {
        assert!(matches!(
            store(FakeExe::new(vec![]), FakeIdentity::new(true))
                .create(invalid)
                .await,
            Err(ExeError::Invalid(_))
        ));
    }

    for component in ["vm_", "vm_0", "vm_zz", "vm_ff", "vm_6e6f742d6f78"] {
        assert!(ox_remote::decode_vm_component(component).is_err());
    }
}
