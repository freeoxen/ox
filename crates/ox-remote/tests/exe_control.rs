use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ox_remote::{
    CommandError, CommandOutput, DeleteVmRequest, ExeCommand, ExeCommandRunner, ExeControlStore,
    ExeError, VmSpec, VmStatus, WorkerIdentityVerifier,
};

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
