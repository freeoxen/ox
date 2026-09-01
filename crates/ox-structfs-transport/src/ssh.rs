use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fs2::FileExt;
use russh::ChannelMsg;
use russh::client;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use thiserror::Error;
use zeroize::Zeroize;

use crate::{RemoteStore, RemoteStoreConfig};

const WORKER_PROGRAM: &str = "ox-worker structfs-stdio --socket";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyEnrollment {
    RefuseUnknown,
    EnrollNew,
}

#[derive(Clone, Debug)]
pub struct KnownHosts {
    path: PathBuf,
    enrollment: HostKeyEnrollment,
}

#[derive(Debug, Error)]
pub enum KnownHostsError {
    #[error("invalid SSH host: {0}")]
    InvalidHost(String),
    #[error("unknown SSH host key for {host}:{port}; fingerprint {fingerprint}")]
    Unknown {
        host: String,
        port: u16,
        fingerprint: String,
    },
    #[error("SSH host key changed for {host}:{port}; expected {expected}, received {received}")]
    Changed {
        host: String,
        port: u16,
        expected: String,
        received: String,
    },
    #[error("known-hosts permissions are not user-only: {0}")]
    InsecurePermissions(PathBuf),
    #[error(
        "known-hosts path is not an owned regular file or its parent is not an owned directory: {0}"
    )]
    UnsafePath(PathBuf),
    #[error("known-hosts I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("known-hosts key parsing failed: {0}")]
    Key(String),
}

impl KnownHosts {
    pub fn new(path: impl Into<PathBuf>, enrollment: HostKeyEnrollment) -> Self {
        Self {
            path: path.into(),
            enrollment,
        }
    }

    pub fn path(&self) -> &FsPath {
        &self.path
    }

    /// Verify or explicitly enroll a key while holding an advisory file lock.
    /// The locked re-check makes simultaneous first-use enrollment deterministic:
    /// the same key converges and a different key fails as a change.
    pub fn verify(&self, host: &str, port: u16, key: &PublicKey) -> Result<(), KnownHostsError> {
        validate_host(host, port)?;
        let parent = self.path.parent().ok_or_else(|| {
            KnownHostsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "known-hosts path has no parent",
            ))
        })?;
        prepare_private_parent(parent)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if !metadata.file_type().is_file() || metadata.uid() != effective_uid() {
                return Err(KnownHostsError::UnsafePath(self.path.clone()));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(KnownHostsError::InsecurePermissions(self.path.clone()));
            }
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)?;
        ensure_user_only(&self.path, &file)?;
        file.lock_exclusive()?;

        let result = self.verify_locked(host, port, key, &mut file);
        let unlock = FileExt::unlock(&file);
        result.and_then(|()| unlock.map_err(KnownHostsError::Io))
    }

    fn verify_locked(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
        file: &mut File,
    ) -> Result<(), KnownHostsError> {
        let entries = russh::keys::known_hosts::known_host_keys_path(host, port, &self.path)
            .map_err(|error| KnownHostsError::Key(error.to_string()))?;
        if entries.iter().any(|(_, recorded)| recorded == key) {
            return Ok(());
        }
        if let Some((_, recorded)) = entries.first() {
            return Err(KnownHostsError::Changed {
                host: host.into(),
                port,
                expected: fingerprint(recorded),
                received: fingerprint(key),
            });
        }
        if self.enrollment == HostKeyEnrollment::RefuseUnknown {
            return Err(KnownHostsError::Unknown {
                host: host.into(),
                port,
                fingerprint: fingerprint(key),
            });
        }

        let host_field = if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        };
        let encoded = key
            .to_openssh()
            .map_err(|error| KnownHostsError::Key(error.to_string()))?;
        file.seek(SeekFrom::End(0))?;
        let length = file.stream_position()?;
        if length > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0_u8; 1];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.seek(SeekFrom::End(0))?;
                file.write_all(b"\n")?;
            }
        }
        writeln!(file, "{host_field} {encoded}")?;
        file.sync_data()?;
        Ok(())
    }
}

fn ensure_user_only(path: &FsPath, file: &File) -> Result<(), KnownHostsError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.uid() != effective_uid() {
        return Err(KnownHostsError::UnsafePath(path.to_owned()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(KnownHostsError::InsecurePermissions(path.to_owned()));
    }
    Ok(())
}

fn prepare_private_parent(parent: &FsPath) -> Result<(), KnownHostsError> {
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.uid() != effective_uid() {
                return Err(KnownHostsError::UnsafePath(parent.to_owned()));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(KnownHostsError::InsecurePermissions(parent.to_owned()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(parent) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return prepare_private_parent(parent);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

fn validate_host(host: &str, port: u16) -> Result<(), KnownHostsError> {
    if port == 0
        || host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:_".contains(&byte))
    {
        return Err(KnownHostsError::InvalidHost(host.into()));
    }
    Ok(())
}

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

#[derive(Clone, Debug)]
pub struct WorkerSshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity_file: PathBuf,
    pub known_hosts: KnownHosts,
    pub socket_path: PathBuf,
    pub inactivity_timeout: Duration,
}

impl WorkerSshConfig {
    pub fn validate(&self) -> Result<(), SshConnectError> {
        validate_host(&self.host, self.port)?;
        if self.user.is_empty()
            || self.user.len() > 128
            || !self
                .user
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_+.-".contains(&byte))
        {
            return Err(SshConnectError::InvalidUser(self.user.clone()));
        }
        validate_socket(&self.socket_path)?;
        Ok(())
    }

    fn command(&self) -> Result<String, SshConnectError> {
        self.validate()?;
        Ok(format!("{WORKER_PROGRAM} {}", self.socket_path.display()))
    }
}

fn validate_socket(path: &FsPath) -> Result<(), SshConnectError> {
    let text = path.to_str().ok_or(SshConnectError::InvalidSocketPath)?;
    if !path.is_absolute()
        || text.len() > 4096
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SshConnectError::InvalidSocketPath);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SshConnectError {
    #[error(transparent)]
    HostKey(#[from] KnownHostsError),
    #[error("invalid SSH username: {0}")]
    InvalidUser(String),
    #[error("worker socket must be an absolute path containing only safe path characters")]
    InvalidSocketPath,
    #[error("failed to load SSH identity: {0}")]
    Identity(String),
    #[error("host-key verification task failed: {0}")]
    VerificationTask(String),
    #[error("SSH connection failed: {0}")]
    Ssh(#[from] russh::Error),
    #[error("SSH public-key authentication was rejected")]
    AuthenticationRejected,
    #[error("worker exec request was rejected or closed before acknowledgement")]
    ExecRejected,
}

#[derive(Debug, Error)]
pub enum IdentityFileError {
    #[error("SSH identity I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH identity must be an owned regular file with user-only permissions: {0}")]
    Unsafe(PathBuf),
    #[error("SSH identity exceeds the 1 MiB limit")]
    TooLarge,
    #[error("SSH identity is not valid UTF-8")]
    Utf8,
    #[error("SSH identity could not be decoded: {0}")]
    Decode(String),
}

/// Open a private identity once with `O_NOFOLLOW`, validate the opened file,
/// and decode bounded bytes. This avoids path-validation/reopen races.
pub fn load_private_identity(
    path: &FsPath,
) -> Result<russh::keys::ssh_key::PrivateKey, IdentityFileError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(IdentityFileError::Unsafe(path.to_owned()));
    }
    if metadata.len() > 1024 * 1024 {
        return Err(IdentityFileError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bytes.zeroize();
        return Err(IdentityFileError::TooLarge);
    }
    let mut secret = match String::from_utf8(bytes) {
        Ok(secret) => secret,
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            return Err(IdentityFileError::Utf8);
        }
    };
    let decoded = russh::keys::decode_secret_key(&secret, None)
        .map_err(|error| IdentityFileError::Decode(error.to_string()));
    secret.zeroize();
    decoded
}

#[derive(Clone)]
struct HostVerifier {
    host: String,
    port: u16,
    known_hosts: KnownHosts,
}

impl client::Handler for HostVerifier {
    type Error = SshConnectError;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let known_hosts = self.known_hosts.clone();
        let host = self.host.clone();
        let port = self.port;
        let key = server_key.public_key();
        tokio::task::spawn_blocking(move || known_hosts.verify(&host, port, &key))
            .await
            .map_err(|error| SshConnectError::VerificationTask(error.to_string()))??;
        Ok(true)
    }
}

/// Connect a multiplexed [`RemoteStore`] through one SSH session channel.
///
/// The only channel request issued is one fixed no-PTY exec request. This API
/// exposes no shell, environment, forwarding, subsystem, or arbitrary-command
/// primitive, and the Store layer never retries a write.
pub async fn connect_worker_ssh(
    ssh: WorkerSshConfig,
    remote: RemoteStoreConfig,
) -> Result<RemoteStore, SshConnectError> {
    let command = ssh.command()?;
    let key = load_private_identity(&ssh.identity_file)
        .map_err(|error| SshConnectError::Identity(error.to_string()))?;
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(ssh.inactivity_timeout),
        ..Default::default()
    });
    let handler = HostVerifier {
        host: ssh.host.clone(),
        port: ssh.port,
        known_hosts: ssh.known_hosts,
    };
    let mut session = client::connect(config, (ssh.host.as_str(), ssh.port), handler).await?;
    let hash = session.best_supported_rsa_hash().await?.flatten();
    let auth = session
        .authenticate_publickey(ssh.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await?;
    if !auth.success() {
        return Err(SshConnectError::AuthenticationRejected);
    }
    let mut channel = session.channel_open_session().await?;
    channel.exec(true, command).await?;
    if !matches!(channel.wait().await, Some(ChannelMsg::Success)) {
        return Err(SshConnectError::ExecRejected);
    }
    Ok(RemoteStore::connect(channel.into_stream(), remote))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    #[test]
    fn fixed_worker_command_rejects_shell_metacharacters_and_parent_paths() {
        let base = WorkerSshConfig {
            host: "vm.example".into(),
            port: 22,
            user: "vm+route".into(),
            identity_file: "id".into(),
            known_hosts: KnownHosts::new("known_hosts", HostKeyEnrollment::RefuseUnknown),
            socket_path: "/run/ox/structfs.sock".into(),
            inactivity_timeout: Duration::from_secs(30),
        };
        assert_eq!(
            base.command().unwrap(),
            "ox-worker structfs-stdio --socket /run/ox/structfs.sock"
        );
        for bad in [
            "/run/ox/a;id",
            "/run/ox/a b",
            "/run/ox/$(id)",
            "/run/ox/../root",
            "relative.sock",
        ] {
            let mut config = base.clone();
            config.socket_path = bad.into();
            assert!(matches!(
                config.command(),
                Err(SshConnectError::InvalidSocketPath)
            ));
        }
    }

    #[test]
    fn unknown_requires_explicit_enrollment_and_changed_key_always_fails() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("remote/known_hosts");
        let key_a = PublicKey::from_openssh(KEY_A).unwrap();
        let key_b = PublicKey::from_openssh(KEY_B).unwrap();
        let refuse = KnownHosts::new(&path, HostKeyEnrollment::RefuseUnknown);
        assert!(matches!(
            refuse.verify("vm.example", 22, &key_a),
            Err(KnownHostsError::Unknown { .. })
        ));

        KnownHosts::new(&path, HostKeyEnrollment::EnrollNew)
            .verify("vm.example", 22, &key_a)
            .unwrap();
        refuse.verify("vm.example", 22, &key_a).unwrap();
        assert!(matches!(
            KnownHosts::new(&path, HostKeyEnrollment::EnrollNew).verify("vm.example", 22, &key_b),
            Err(KnownHostsError::Changed { .. })
        ));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn simultaneous_enrollment_rechecks_under_lock() {
        for _ in 0..32 {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("remote/known_hosts");
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let threads: Vec<_> = [KEY_A, KEY_B]
                .into_iter()
                .map(|encoded| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let key = PublicKey::from_openssh(encoded).unwrap();
                        barrier.wait();
                        KnownHosts::new(path, HostKeyEnrollment::EnrollNew).verify(
                            "race.example",
                            2222,
                            &key,
                        )
                    })
                })
                .collect();
            let results: Vec<_> = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|result| matches!(result, Err(KnownHostsError::Changed { .. })))
                    .count(),
                1,
                "unexpected race results: {results:?}"
            );
        }
    }

    #[test]
    fn refuses_shared_parent_and_symlink_without_mutating_permissions() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let key = PublicKey::from_openssh(KEY_A).unwrap();
        let result = KnownHosts::new(shared.join("known_hosts"), HostKeyEnrollment::EnrollNew)
            .verify("vm.example", 22, &key);
        assert!(matches!(
            result,
            Err(KnownHostsError::InsecurePermissions(_))
        ));
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let private = temp.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = private.join("target");
        std::fs::write(&target, "").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = private.join("known_hosts");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            KnownHosts::new(link, HostKeyEnrollment::EnrollNew).verify("vm.example", 22, &key),
            Err(KnownHostsError::UnsafePath(_))
        ));
    }

    #[test]
    fn private_identity_validation_refuses_symlinks_and_open_modes() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let key = temp.path().join("id");
        std::fs::write(&key, "not parsed in this unit test").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_private_identity(&key),
            Err(IdentityFileError::Unsafe(_))
        ));
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            load_private_identity(&key),
            Err(IdentityFileError::Decode(_))
        ));
        let link = temp.path().join("id-link");
        symlink(&key, &link).unwrap();
        assert!(matches!(
            load_private_identity(&link),
            Err(IdentityFileError::Io(_))
        ));
    }
}
