use super::*;

/// `system.handshake`: must be the first call on every connection.
#[derive(Debug)]
pub struct Handshake;

impl Method for Handshake {
    const NAME: &'static str = "system.handshake";
    type Params = HandshakeParams;
    type Result = HandshakeResult;
}

/// `system.ping`: a liveness check.
#[derive(Debug)]
pub struct Ping;

impl Method for Ping {
    const NAME: &'static str = "system.ping";
    type Params = PingParams;
    type Result = PingResult;
}

/// `system.shutdown`: stops the daemon gracefully. It replies first, then stops serving; clients
/// see their connection close.
#[derive(Debug)]
pub struct Shutdown;

impl Method for Shutdown {
    const NAME: &'static str = "system.shutdown";
    type Params = ShutdownParams;
    type Result = ShutdownResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownResult {
    /// The daemon's process id, so a caller can wait for it to exit.
    pub pid: u32,
}

/// `system.lease`: registers the calling process as a user of the daemon for as long as this
/// connection stays open. A daemon started by a client exits when no connection holds a lease
/// and no work keeps it up (see `system.lifetime`), so every client that wants the daemon to
/// stay holds one; a daemon started on its own never exits for want of leases. The lease ends
/// when the connection closes, or when the process named by `pid` is gone.
#[derive(Debug)]
pub struct SystemLease;

impl Method for SystemLease {
    const NAME: &'static str = "system.lease";
    type Params = SystemLeaseParams;
    type Result = SystemLeaseResult;
}

/// What kind of client holds a lease on the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LeaseKind {
    /// An `imperium` command.
    Cli,
    /// The desktop app.
    Desktop,
    /// A bridge or integration, such as `imperium mcp-serve`.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemLeaseParams {
    pub kind: LeaseKind,
    /// The client's process id on the daemon's machine; absent for a client elsewhere.
    pub pid: Option<u32>,
    /// What the client is doing, such as `imperium chat`.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemLeaseResult {
    pub id: String,
}

/// `system.lifetime`: how long the daemon stays up, who holds it open, and what work does.
/// With `persistent` it also chooses whether the daemon exits when idle (`false`) or runs until
/// it is stopped (`true`); a daemon that has already decided to exit stays decided.
#[derive(Debug)]
pub struct SystemLifetime;

impl Method for SystemLifetime {
    const NAME: &'static str = "system.lifetime";
    type Params = SystemLifetimeParams;
    type Result = LifetimeStatus;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemLifetimeParams {
    pub persistent: Option<bool>,
}

/// Whether the daemon exits when nothing uses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LifetimeMode {
    /// Exits after a grace period with no lease and no work.
    Shared,
    /// Runs until it is told to stop.
    Persistent,
}

/// The kinds of background work that keep the daemon up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WorkKind {
    /// A turn is running (or waiting for a quiet machine).
    Turn,
    /// An enabled schedule has a run to come, or is running.
    Schedule,
    /// A persistent agent has initiative on.
    Initiative,
    /// GitHub auto-fix or a watched pull request.
    Github,
    /// A long job: a model download, a benchmark, a runtime install or update, a folder move.
    Job,
    /// A request waiting for a paired phone that may approve it.
    Approval,
}

/// A client holding the daemon open, as `system.lifetime` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LeaseInfo {
    pub id: String,
    pub kind: LeaseKind,
    pub pid: Option<u32>,
    pub label: String,
    /// How long the lease has been held.
    #[ts(type = "number")]
    pub held_ms: u64,
}

/// One kind of work holding the daemon up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkInfo {
    pub kind: WorkKind,
    pub count: u32,
    pub detail: String,
}

/// What `system.lifetime` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LifetimeStatus {
    pub mode: LifetimeMode,
    /// How long a shared daemon waits with no lease and no work before it exits.
    #[ts(type = "number")]
    pub grace_ms: u64,
    pub leases: Vec<LeaseInfo>,
    pub work: Vec<WorkInfo>,
    /// How long the daemon has had no lease and no work; absent while it has either.
    #[ts(type = "number | null")]
    pub idle_ms: Option<u64>,
    /// How long until a shared daemon exits; absent while it has a lease or work.
    #[ts(type = "number | null")]
    pub exits_in_ms: Option<u64>,
    /// The daemon has decided to exit and takes no more leases.
    pub exiting: bool,
}

/// Names and versions a program, such as the CLI or the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Peer {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeParams {
    pub client: Peer,
    pub protocol: ProtocolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeResult {
    pub daemon: Peer,
    pub protocol: ProtocolVersion,
    /// The daemon runs as a server (`daemon.server`): the desktop-only features are refused
    /// with `-32010`, so a client hides their controls. False from a daemon before 0.63.
    #[serde(default)]
    pub server: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingResult {
    /// Milliseconds since the daemon started.
    #[ts(type = "number")]
    pub uptime_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_says_server_and_an_older_daemon_reads_as_not_one() {
        let answer = HandshakeResult {
            daemon: Peer {
                name: "imperium-core".into(),
                version: "0".into(),
            },
            protocol: crate::PROTOCOL_VERSION,
            server: true,
        };
        let value = serde_json::to_value(&answer).unwrap();
        assert_eq!(value["server"], true);
        let older: HandshakeResult = serde_json::from_value(serde_json::json!({
            "daemon": {"name": "imperium-core", "version": "0"},
            "protocol": {"major": 0, "minor": 62}
        }))
        .unwrap();
        assert!(!older.server);
    }
}
