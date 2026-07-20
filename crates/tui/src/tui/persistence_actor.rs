//! Dedicated persistence actor for session save / checkpoint I/O.
//!
//! ## Motivation
//!
//! Before this module, `persist_checkpoint` and `persist_session_snapshot` ran
//! synchronously on the tokio worker thread that drives the TUI event loop.
//! Each call serialised all API messages to JSON, wrote a temp file, and
//! renamed it atomically — blocking keyboard input for the duration.
//! `save_session` additionally called `cleanup_old_sessions`, which listed all
//! session files, parsed metadata from every one, sorted, and deleted the
//! oldest — scaling O(session-bytes + file-count) with every turn.
//!
//! ## Design
//!
//! - **One dedicated tokio task** spawned at TUI startup. All disk I/O moves
//!   to this task. The UI merely `try_send`s a request (non-blocking,
//!   bounded-channel drop) and returns immediately — keystrokes are never
//!   gated on write completion.
//! - **Latest-wins coalescing per session**: when multiple `SaveCheckpoint`,
//!   `SessionSnapshot`, or offline-queue requests pile up before the actor's
//!   next write cycle, only the most recent one per session is written.
//!   Checkpoints and clears are keyed by session id, so concurrent sessions
//!   never coalesce into (or clear) each other's slot.
//! - **Durability reporting**: every write/removal result is collected; a
//!   `FlushAndReport` request drains pending work and replies with the
//!   aggregated results since the last report. Cycles with no listener log
//!   their failures instead of discarding them.
//! - **Unbounded channel** for `try_send` to always succeed; the actor
//!   naturally backpressures via the spawn pool. A few outstanding
//!   `SavedSession` values in the channel (< 1 MB) is negligible pressure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use tokio::sync::{mpsc, oneshot};

use crate::artifacts::ArtifactRecord;
use crate::session_manager::{OfflineQueueState, SavedSession, SessionManager};
use crate::utils::spawn_supervised;

type ArtifactKey = (String, PathBuf);

/// Late artifacts normally live in memory for less than one write cycle. Keep
/// hard bounds anyway: if the session store is unavailable for a long time,
/// exact-output previews and paths must not accumulate without limit.
const MAX_PENDING_ARTIFACT_SESSIONS: usize = 32;
const MAX_PENDING_ARTIFACTS_PER_SESSION: usize = 1_024;
const MAX_TRACKED_ARTIFACT_HIGH_WATERS: usize = 64;

#[derive(Debug)]
struct VersionedArtifact {
    generation: u64,
    record: ArtifactRecord,
}

// ---------------------------------------------------------------------------
// Request type
// ---------------------------------------------------------------------------

/// Persistence work item sent to the actor.
#[derive(Debug)]
pub enum PersistRequest {
    /// Write a crash-recovery checkpoint (in-flight turn state) to the
    /// session's own file (`checkpoints/<session_id>.json`).
    SaveCheckpoint { session: SavedSession },
    /// Write a full session snapshot (completed turn, durable save).
    SessionSnapshot(SavedSession),
    /// Merge one late-arriving exact-output artifact into an already queued
    /// or durable session. Artifact records are monotonic for the actor's
    /// lifetime so a subsequently queued stale snapshot cannot erase them.
    SessionArtifactStored {
        session_id: String,
        artifact: ArtifactRecord,
    },
    /// Write queued/draft offline input for crash recovery.
    OfflineQueue {
        state: OfflineQueueState,
        session_id: Option<String>,
    },
    /// Remove the queued/draft offline input file.
    ClearOfflineQueue,
    /// Remove one session's crash-recovery checkpoint file. Scoped: cannot
    /// remove another session's checkpoint.
    ClearCheckpoint { session_id: String },
    /// Flush all pending work now and report durability results through
    /// `reply`. The report aggregates every write/removal result since the
    /// previous report (including background write cycles) — errors are
    /// collected and surfaced, never discarded.
    FlushAndReport { reply: oneshot::Sender<FlushReport> },
    /// Graceful shutdown — flush pending writes, then exit the actor loop.
    Shutdown,
}

/// Aggregated durability results: how many writes/removals completed and
/// which failed (labelled by what was being persisted, with the I/O error
/// kind).
#[derive(Debug, Default)]
pub struct FlushReport {
    pub completed: usize,
    pub failures: Vec<(String, std::io::ErrorKind)>,
}

impl FlushReport {
    /// Upper bound on retained failure entries when accumulating across
    /// write cycles. Every failure is logged at the cycle it happened, so
    /// dropping older-than-bound entries from the reply loses no evidence.
    const MAX_ACCUMULATED_FAILURES: usize = 256;

    fn merge(&mut self, other: FlushReport) {
        self.completed += other.completed;
        self.failures.extend(other.failures);
        if self.failures.len() > Self::MAX_ACCUMULATED_FAILURES {
            let excess = self.failures.len() - Self::MAX_ACCUMULATED_FAILURES;
            self.failures.drain(..excess);
        }
    }
}

#[derive(Debug)]
enum PendingOfflineQueue {
    Save {
        state: Box<OfflineQueueState>,
        session_id: Option<String>,
    },
    Clear,
}

// ---------------------------------------------------------------------------
// Handle (held by the TUI)
// ---------------------------------------------------------------------------

/// Lightweight handle that the UI holds to queue persistence work.
#[derive(Debug, Clone)]
pub struct PersistActorHandle {
    tx: mpsc::UnboundedSender<PersistRequest>,
}

impl PersistActorHandle {
    /// Queue a persistence request without blocking. If the actor's channel is
    /// closed (shutdown has already happened), return `false`.
    pub fn try_send(&self, request: PersistRequest) -> bool {
        self.tx.send(request).is_ok()
    }
}

// ---------------------------------------------------------------------------
// Global singleton (avoid threading through App)
// ---------------------------------------------------------------------------

static ACTOR_TX: OnceLock<PersistActorHandle> = OnceLock::new();

/// Initialise the global persistence actor handle. Must be called once at
/// startup, before the event loop starts.
pub fn init_actor(handle: PersistActorHandle) {
    let _ = ACTOR_TX.set(handle);
}

/// Queue a persistence request through the global handle. No-op (silently
/// ignored) when the actor hasn't been initialised yet — this can happen in
/// tests or early startup before the actor is ready.
pub fn persist(request: PersistRequest) {
    let _ = try_persist(request);
}

/// Queue persistence and report whether the actor accepted ownership. Work
/// Graph projections use this acknowledgement as their publish boundary.
pub fn try_persist(request: PersistRequest) -> bool {
    ACTOR_TX
        .get()
        .is_some_and(|handle| handle.try_send(request))
}

// ---------------------------------------------------------------------------
// Actor spawn
// ---------------------------------------------------------------------------

/// Spawn the persistence actor task and return a handle for the caller to
/// store and initialise.
///
/// The returned handle should be passed to [`init_actor`] so that the
/// `persist()` free function can reach it from anywhere in the TUI.
pub fn spawn_persistence_actor(
    manager: SessionManager,
) -> (PersistActorHandle, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<PersistRequest>();
    let handle = PersistActorHandle { tx };

    let task = spawn_supervised(
        "persistence-actor",
        std::panic::Location::caller(),
        async move {
            let mut pending = PendingState::default();
            // Durability results from write cycles that no caller has asked
            // about yet; drained into the next `FlushAndReport` reply.
            let mut unreported = FlushReport::default();

            // Flush pending work, log new failures, and fold the cycle's
            // results into the unreported accumulator.
            fn flush_cycle(
                manager: &SessionManager,
                pending: &mut PendingState,
                unreported: &mut FlushReport,
            ) {
                let cycle = flush_inner(manager, pending);
                log_flush_failures(&cycle);
                unreported.merge(cycle);
            }

            loop {
                // Drain everything waiting, keeping only the latest of each kind.
                while let Ok(req) = rx.try_recv() {
                    match pending.absorb(req) {
                        Control::Continue => {}
                        Control::Flush(reply) => {
                            flush_cycle(&manager, &mut pending, &mut unreported);
                            let _ = reply.send(std::mem::take(&mut unreported));
                        }
                        Control::Shutdown => {
                            flush_cycle(&manager, &mut pending, &mut unreported);
                            return;
                        }
                    }
                }

                // Write coalesced work.
                flush_cycle(&manager, &mut pending, &mut unreported);

                // Block until the next request arrives.
                match rx.recv().await {
                    Some(req) => match pending.absorb(req) {
                        Control::Continue => {}
                        Control::Flush(reply) => {
                            flush_cycle(&manager, &mut pending, &mut unreported);
                            let _ = reply.send(std::mem::take(&mut unreported));
                        }
                        Control::Shutdown => {
                            flush_cycle(&manager, &mut pending, &mut unreported);
                            return;
                        }
                    },
                    None => {
                        // Channel closed — final flush and exit.
                        flush_cycle(&manager, &mut pending, &mut unreported);
                        return;
                    }
                }
            }
        },
    );

    (handle, task)
}

/// Coalesced work waiting for the next write cycle.
#[derive(Debug, Default)]
struct PendingState {
    /// Latest-wins per session id. Crash checkpoints are keyed per session
    /// (mirroring `sessions` below) so concurrent sessions can interleave
    /// saves and clears without clobbering each other.
    checkpoints: BTreeMap<String, SavedSession>,
    /// Session ids whose checkpoint file should be removed.
    checkpoint_clears: BTreeSet<String>,
    /// Latest-wins per session id. Coalescing into one global slot can
    /// drop session A when an immediate `/new` queues session B before
    /// the actor drains.
    sessions: BTreeMap<String, SavedSession>,
    /// Validated artifact records not yet covered by a successful durable
    /// session write. Records are versioned so a flush only evicts the exact
    /// generation it made durable; a later arrival can never be cleared by an
    /// older write result.
    artifact_overlays: BTreeMap<String, BTreeMap<ArtifactKey, VersionedArtifact>>,
    /// Actor-local generation assigned to accepted artifacts. This is only a
    /// high-water marker; it contains no artifact content or path.
    next_artifact_generation: u64,
    /// Bounded, non-content receipt of the latest generation made durable for
    /// recently active sessions. Correctness does not depend on retaining an
    /// entry: an evicted session recovers its high water from the durable
    /// session file before any later snapshot is saved.
    durable_artifact_high_waters: BTreeMap<String, u64>,
    /// Rejected late records are quarantined as bounded, non-secret failure
    /// receipts. The rejected record itself is never retained.
    artifact_rejections: Vec<(String, std::io::ErrorKind)>,
    offline_queue: Option<PendingOfflineQueue>,
}

/// What the actor loop should do after absorbing a request.
enum Control {
    Continue,
    Flush(oneshot::Sender<FlushReport>),
    Shutdown,
}

impl PendingState {
    fn absorb(&mut self, req: PersistRequest) -> Control {
        match req {
            PersistRequest::SaveCheckpoint { session } => {
                // Last-writer-wins per session: a fresh checkpoint supersedes
                // a pending clear for the same session so the two never both
                // apply in one drain (which previously cleared then re-wrote
                // the stale checkpoint, undoing the clear).
                let id = session.metadata.id.clone();
                self.checkpoint_clears.remove(&id);
                self.checkpoints.insert(id, session);
            }
            PersistRequest::SessionSnapshot(session) => {
                self.sessions.insert(session.metadata.id.clone(), session);
            }
            PersistRequest::SessionArtifactStored {
                session_id,
                artifact,
            } => {
                self.absorb_artifact(session_id, artifact);
            }
            PersistRequest::OfflineQueue { state, session_id } => {
                self.offline_queue = Some(PendingOfflineQueue::Save {
                    state: Box::new(state),
                    session_id,
                });
            }
            PersistRequest::ClearOfflineQueue => {
                self.offline_queue = Some(PendingOfflineQueue::Clear);
            }
            PersistRequest::ClearCheckpoint { session_id } => {
                // A clear supersedes a pending checkpoint write for the same
                // session only — other sessions' pending work is untouched.
                self.checkpoints.remove(&session_id);
                self.checkpoint_clears.insert(session_id);
            }
            PersistRequest::FlushAndReport { reply } => return Control::Flush(reply),
            PersistRequest::Shutdown => return Control::Shutdown,
        }
        Control::Continue
    }

    fn absorb_artifact(&mut self, session_id: String, artifact: ArtifactRecord) {
        if let Err(error) = validate_late_artifact(&session_id, &artifact) {
            self.reject_artifact(&session_id, error.kind());
            return;
        }

        let key = (artifact.tool_call_id.clone(), artifact.storage_path.clone());
        let starts_new_session = !self.artifact_overlays.contains_key(&session_id);
        if starts_new_session && self.artifact_overlays.len() >= MAX_PENDING_ARTIFACT_SESSIONS {
            self.reject_artifact(&session_id, std::io::ErrorKind::Other);
            return;
        }
        if self
            .artifact_overlays
            .get(&session_id)
            .is_some_and(|overlay| {
                overlay.len() >= MAX_PENDING_ARTIFACTS_PER_SESSION && !overlay.contains_key(&key)
            })
        {
            self.reject_artifact(&session_id, std::io::ErrorKind::Other);
            return;
        }
        let Some(generation) = self.next_artifact_generation.checked_add(1) else {
            self.reject_artifact(&session_id, std::io::ErrorKind::Other);
            return;
        };
        self.next_artifact_generation = generation;
        self.artifact_overlays
            .entry(session_id)
            .or_default()
            .insert(
                key,
                VersionedArtifact {
                    generation,
                    record: artifact,
                },
            );
    }

    fn reject_artifact(&mut self, session_id: &str, kind: std::io::ErrorKind) {
        self.artifact_rejections
            .push((format!("session-artifact-rejected:{session_id}"), kind));
        if self.artifact_rejections.len() > FlushReport::MAX_ACCUMULATED_FAILURES {
            let excess = self.artifact_rejections.len() - FlushReport::MAX_ACCUMULATED_FAILURES;
            self.artifact_rejections.drain(..excess);
        }
    }

    fn overlay_high_water(&self, session_id: &str) -> Option<u64> {
        self.artifact_overlays
            .get(session_id)
            .and_then(|overlay| overlay.values().map(|artifact| artifact.generation).max())
    }

    fn mark_artifact_generation_durable(&mut self, session_id: &str, high_water: u64) {
        let remove_session = self
            .artifact_overlays
            .get_mut(session_id)
            .is_some_and(|overlay| {
                overlay.retain(|_, artifact| artifact.generation > high_water);
                overlay.is_empty()
            });
        if remove_session {
            // Evict previews, tool names, and paths immediately after the
            // durable file becomes the monotonic source of truth.
            self.artifact_overlays.remove(session_id);
        }
        self.durable_artifact_high_waters
            .entry(session_id.to_string())
            .and_modify(|durable| *durable = (*durable).max(high_water))
            .or_insert(high_water);
        while self.durable_artifact_high_waters.len() > MAX_TRACKED_ARTIFACT_HIGH_WATERS {
            let Some(oldest_session) = self
                .durable_artifact_high_waters
                .iter()
                .min_by_key(|(_, generation)| **generation)
                .map(|(session_id, _)| session_id.clone())
            else {
                break;
            };
            self.durable_artifact_high_waters.remove(&oldest_session);
        }
    }
}

/// Write all pending work to disk, draining `pending`. Every write and
/// removal result is collected into the returned [`FlushReport`] — failures
/// are reported, never silently discarded.
fn flush_inner(manager: &SessionManager, pending: &mut PendingState) -> FlushReport {
    let mut report = FlushReport::default();
    report.failures.append(&mut pending.artifact_rejections);
    let mut record = |what: String, result: std::io::Result<()>| match result {
        Ok(()) => report.completed += 1,
        Err(err) => report.failures.push((what, err.kind())),
    };

    for session_id in std::mem::take(&mut pending.checkpoint_clears) {
        record(
            format!("clear-checkpoint:{session_id}"),
            manager.clear_session_checkpoint(&session_id),
        );
    }
    for (session_id, mut session) in std::mem::take(&mut pending.checkpoints) {
        let overlay = pending.artifact_overlays.get(&session_id);
        let result = merge_durable_artifact_high_water(manager, &session_id, &mut session)
            .and_then(|()| {
                merge_artifact_overlay(&mut session, overlay);
                manager.save_checkpoint(&session).map(|_| ())
            });
        record(format!("checkpoint:{session_id}"), result);
    }
    let mut attempted_artifact_sessions = BTreeSet::new();
    for (session_id, mut session) in std::mem::take(&mut pending.sessions) {
        attempted_artifact_sessions.insert(session_id.clone());
        let overlay_high_water = pending.overlay_high_water(&session_id);
        let overlay = pending.artifact_overlays.get(&session_id);
        let result =
            save_session_with_artifact_high_water(manager, &session_id, &mut session, overlay);
        match result {
            Ok(()) => {
                if let Some(high_water) = overlay_high_water {
                    pending.mark_artifact_generation_durable(&session_id, high_water);
                }
                record(format!("session:{session_id}"), Ok(()));
            }
            Err(error) => record(format!("session:{session_id}"), Err(error)),
        }
    }
    let dirty_artifact_sessions = pending
        .artifact_overlays
        .keys()
        .filter(|session_id| !attempted_artifact_sessions.contains(*session_id))
        .cloned()
        .collect::<Vec<_>>();
    for session_id in dirty_artifact_sessions {
        let Some(overlay_high_water) = pending.overlay_high_water(&session_id) else {
            continue;
        };
        let result = merge_artifacts_into_durable_session(
            manager,
            &session_id,
            pending.artifact_overlays.get(&session_id),
        );
        match result {
            Ok(()) => {
                pending.mark_artifact_generation_durable(&session_id, overlay_high_water);
                record(format!("session-artifact:{session_id}"), Ok(()));
            }
            Err(error) => record(format!("session-artifact:{session_id}"), Err(error)),
        }
    }
    if let Some(request) = pending.offline_queue.take() {
        match request {
            PendingOfflineQueue::Save { state, session_id } => record(
                "offline-queue".to_string(),
                manager
                    .save_offline_queue_state(&state, session_id.as_deref())
                    .map(|_| ()),
            ),
            PendingOfflineQueue::Clear => record(
                "clear-offline-queue".to_string(),
                manager.clear_offline_queue_state(),
            ),
        }
    }
    report
}

fn merge_artifact_overlay(
    session: &mut SavedSession,
    overlay: Option<&BTreeMap<ArtifactKey, VersionedArtifact>>,
) {
    let Some(overlay) = overlay else { return };
    for versioned in overlay.values() {
        let artifact = &versioned.record;
        if let Some(existing) = session.artifacts.iter_mut().find(|existing| {
            existing.tool_call_id == artifact.tool_call_id
                && existing.storage_path == artifact.storage_path
        }) {
            *existing = artifact.clone();
        } else {
            session.artifacts.push(artifact.clone());
        }
    }
}

fn merge_artifacts_into_durable_session(
    manager: &SessionManager,
    session_id: &str,
    overlay: Option<&BTreeMap<ArtifactKey, VersionedArtifact>>,
) -> std::io::Result<()> {
    let mut session = manager.load_session(session_id)?;
    if session.metadata.id != session_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "durable session id does not match its persistence key",
        ));
    }
    merge_artifact_overlay(&mut session, overlay);
    manager.save_session(&session).map(|_| ())
}

fn save_session_with_artifact_high_water(
    manager: &SessionManager,
    session_id: &str,
    session: &mut SavedSession,
    overlay: Option<&BTreeMap<ArtifactKey, VersionedArtifact>>,
) -> std::io::Result<()> {
    merge_durable_artifact_high_water(manager, session_id, session)?;
    merge_artifact_overlay(session, overlay);
    manager.save_session(session).map(|_| ())
}

fn merge_durable_artifact_high_water(
    manager: &SessionManager,
    session_id: &str,
    session: &mut SavedSession,
) -> std::io::Result<()> {
    match manager.load_session(session_id) {
        Ok(durable) => {
            if durable.metadata.id != session_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "durable session id does not match its persistence key",
                ));
            }
            // The durable session is the monotonic high water after an
            // in-memory overlay is evicted. Merge it after the incoming
            // snapshot so a late stale snapshot can never erase artifacts,
            // including records whose output file has since become
            // unavailable.
            merge_artifact_records(session, &durable.artifacts);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn merge_artifact_records(session: &mut SavedSession, artifacts: &[ArtifactRecord]) {
    for artifact in artifacts {
        if let Some(existing) = session.artifacts.iter_mut().find(|existing| {
            existing.tool_call_id == artifact.tool_call_id
                && existing.storage_path == artifact.storage_path
        }) {
            *existing = artifact.clone();
        } else {
            session.artifacts.push(artifact.clone());
        }
    }
}

fn validate_late_artifact(session_id: &str, artifact: &ArtifactRecord) -> std::io::Result<()> {
    if artifact.session_id != session_id || artifact.storage_path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "late artifact is not confined to its owning session",
        ));
    }
    // Validate and open once while accepting ownership. Later flushes retain
    // only the already-validated descriptor; loss of the underlying output
    // must not turn a stale snapshot into permission to erase durable
    // metadata.
    crate::artifacts::open_session_artifact_for_read(session_id, &artifact.storage_path)?;
    Ok(())
}

/// Surface flush failures in the log for write cycles that have no caller
/// waiting on a [`FlushReport`].
fn log_flush_failures(report: &FlushReport) {
    for (what, kind) in &report.failures {
        tracing::warn!(
            target: "persistence",
            what = %what,
            error_kind = ?kind,
            "persistence write failed",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::session_manager::{OfflineQueueState, QueuedSessionMessage};

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if predicate() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for persistence actor"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn artifact_record(
        session_id: &str,
        artifact_id: &str,
        tool_call_id: &str,
        storage_path: PathBuf,
        raw: &str,
    ) -> ArtifactRecord {
        ArtifactRecord {
            id: artifact_id.to_string(),
            kind: crate::artifacts::ArtifactKind::ToolOutput,
            session_id: session_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: "run_tests".to_string(),
            success: Some(true),
            created_at: chrono::Utc::now(),
            byte_size: raw.len() as u64,
            preview: raw.lines().next().unwrap_or_default().to_string(),
            storage_path,
        }
    }

    #[tokio::test]
    async fn actor_persists_and_clears_offline_queue_requests() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let queue_path = sessions_dir.join("checkpoints").join("offline_queue.json");
        let (handle, task) = spawn_persistence_actor(manager);

        let state = OfflineQueueState {
            messages: vec![QueuedSessionMessage {
                display: "queued from enter".to_string(),
                skill_instruction: None,
                skill_provenance: None,
            }],
            ..OfflineQueueState::default()
        };

        handle.try_send(PersistRequest::OfflineQueue {
            state,
            session_id: Some("session-A".to_string()),
        });
        wait_until(|| {
            std::fs::read_to_string(&queue_path)
                .is_ok_and(|body| body.contains("queued from enter"))
        })
        .await;

        handle.try_send(PersistRequest::ClearOfflineQueue);
        wait_until(|| !queue_path.exists()).await;
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn shutdown_wait_flushes_queued_session_before_returning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let session = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(session));
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        let loaded = verification_manager
            .load_session(&session_id)
            .expect("shutdown must flush queued session");
        assert_eq!(loaded.metadata.id, session_id);
    }

    #[tokio::test]
    async fn shutdown_flushes_latest_snapshot_for_each_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let mut first = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        first.metadata.title = "Session A".to_string();
        let mut second = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        second.metadata.title = "Session B".to_string();
        let first_id = first.metadata.id.clone();
        let second_id = second.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(first));
        handle.try_send(PersistRequest::SessionSnapshot(second));
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        assert_eq!(
            verification_manager
                .load_session(&first_id)
                .expect("session A flushed")
                .metadata
                .title,
            "Session A"
        );
        assert_eq!(
            verification_manager
                .load_session(&second_id)
                .expect("session B flushed")
                .metadata
                .title,
            "Session B"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn late_artifact_overlay_survives_a_newer_stale_session_snapshot() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let prior = crate::artifacts::set_test_artifact_sessions_root(Some(sessions_dir.clone()));
        struct ArtifactRootReset(Option<PathBuf>);
        impl Drop for ArtifactRootReset {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _reset = ArtifactRootReset(prior);

        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let mut stale = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        stale.metadata.id = "late-artifact-stale-snapshot".to_string();
        let raw = "CW_LATE_STALE_SENTINEL\n    exact spacing\n";
        let artifact_id = "art_late_stale";
        let (_, relative_path) =
            crate::artifacts::write_session_artifact(&stale.metadata.id, artifact_id, raw)
                .expect("write artifact");
        let artifact = ArtifactRecord {
            id: artifact_id.to_string(),
            kind: crate::artifacts::ArtifactKind::ToolOutput,
            session_id: stale.metadata.id.clone(),
            tool_call_id: "call-late-stale".to_string(),
            tool_name: "run_tests".to_string(),
            success: Some(true),
            created_at: chrono::Utc::now(),
            byte_size: raw.len() as u64,
            preview: "CW_LATE_STALE_SENTINEL".to_string(),
            storage_path: relative_path,
        };
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(stale.clone()));
        handle.try_send(PersistRequest::SessionArtifactStored {
            session_id: stale.metadata.id.clone(),
            artifact: artifact.clone(),
        });
        // A full snapshot built before the late event must not win merely
        // because it was enqueued afterward by another persistence path.
        handle.try_send(PersistRequest::SessionSnapshot(stale.clone()));
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        let loaded = verification_manager
            .load_session(&stale.metadata.id)
            .expect("load durable session");
        assert_eq!(loaded.artifacts, vec![artifact]);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn rejected_artifacts_do_not_poison_a_later_valid_artifact() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let prior = crate::artifacts::set_test_artifact_sessions_root(Some(sessions_dir.clone()));
        struct ArtifactRootReset(Option<PathBuf>);
        impl Drop for ArtifactRootReset {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _reset = ArtifactRootReset(prior);

        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let mut stale = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        stale.metadata.id = "late-artifact-quarantine".to_string();
        let missing = artifact_record(
            &stale.metadata.id,
            "art_missing",
            "call-missing",
            crate::artifacts::session_artifact_relative_path("art_missing"),
            "missing",
        );
        let wrong_owner = ArtifactRecord {
            session_id: "different-session".to_string(),
            ..missing.clone()
        };
        let valid_raw = "CW_VALID_AFTER_REJECTED_ARTIFACT\n";
        let (_, valid_path) = crate::artifacts::write_session_artifact(
            &stale.metadata.id,
            "art_valid_after_rejected",
            valid_raw,
        )
        .expect("write valid artifact");
        let valid = artifact_record(
            &stale.metadata.id,
            "art_valid_after_rejected",
            "call-valid-after-rejected",
            valid_path,
            valid_raw,
        );
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SessionSnapshot(stale.clone()));
        handle.try_send(PersistRequest::SessionArtifactStored {
            session_id: stale.metadata.id.clone(),
            artifact: missing,
        });
        handle.try_send(PersistRequest::SessionArtifactStored {
            session_id: stale.metadata.id.clone(),
            artifact: wrong_owner,
        });
        handle.try_send(PersistRequest::SessionArtifactStored {
            session_id: stale.metadata.id.clone(),
            artifact: valid.clone(),
        });
        // Even another pre-artifact snapshot cannot let either rejected
        // record block or erase the independently valid record.
        handle.try_send(PersistRequest::SessionSnapshot(stale));
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report");
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|(what, _)| what.starts_with("session-artifact-rejected:"))
                .count(),
            2,
            "each bad record must be quarantined independently: {:?}",
            report.failures
        );

        let loaded = verification_manager
            .load_session("late-artifact-quarantine")
            .expect("valid artifact session persisted");
        assert_eq!(loaded.artifacts, vec![valid]);
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[test]
    fn durable_high_water_survives_unavailable_output_and_evicts_sensitive_overlay() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let prior = crate::artifacts::set_test_artifact_sessions_root(Some(sessions_dir.clone()));
        struct ArtifactRootReset(Option<PathBuf>);
        impl Drop for ArtifactRootReset {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _reset = ArtifactRootReset(prior);

        let manager = SessionManager::new(sessions_dir).expect("manager");
        let mut stale = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        stale.metadata.id = "durable-artifact-high-water".to_string();
        let raw = "CW_DURABLE_HIGH_WATER\n";
        let (absolute_path, relative_path) = crate::artifacts::write_session_artifact(
            &stale.metadata.id,
            "art_durable_high_water",
            raw,
        )
        .expect("write artifact");
        let artifact = artifact_record(
            &stale.metadata.id,
            "art_durable_high_water",
            "call-durable-high-water",
            relative_path,
            raw,
        );
        let mut pending = PendingState::default();
        pending.absorb(PersistRequest::SessionSnapshot(stale.clone()));
        pending.absorb(PersistRequest::SessionArtifactStored {
            session_id: stale.metadata.id.clone(),
            artifact: artifact.clone(),
        });

        let first = flush_inner(&manager, &mut pending);
        assert!(first.failures.is_empty(), "first flush: {first:?}");
        assert!(
            pending.artifact_overlays.is_empty(),
            "successful durable high water must evict preview/path metadata"
        );
        assert!(
            pending
                .durable_artifact_high_waters
                .contains_key(&stale.metadata.id),
            "non-content high-water receipt should remain"
        );

        std::fs::remove_file(&absolute_path).expect("make prior output unavailable");
        pending.absorb(PersistRequest::SessionSnapshot(stale));
        let second = flush_inner(&manager, &mut pending);
        assert!(second.failures.is_empty(), "stale flush: {second:?}");
        let loaded = manager
            .load_session("durable-artifact-high-water")
            .expect("load monotonic durable session");
        assert_eq!(
            loaded.artifacts,
            vec![artifact],
            "an unavailable output cannot let a stale snapshot roll metadata back"
        );
    }

    #[test]
    fn artifact_generation_eviction_keeps_a_newer_arrival_and_bounds_session_receipts() {
        let mut pending = PendingState::default();
        pending.artifact_overlays.insert(
            "active-session".to_string(),
            BTreeMap::from([
                (
                    ("call-old".to_string(), PathBuf::from("artifacts/old.txt")),
                    VersionedArtifact {
                        generation: 10,
                        record: artifact_record(
                            "active-session",
                            "art_old",
                            "call-old",
                            PathBuf::from("artifacts/old.txt"),
                            "old",
                        ),
                    },
                ),
                (
                    ("call-new".to_string(), PathBuf::from("artifacts/new.txt")),
                    VersionedArtifact {
                        generation: 11,
                        record: artifact_record(
                            "active-session",
                            "art_new",
                            "call-new",
                            PathBuf::from("artifacts/new.txt"),
                            "new",
                        ),
                    },
                ),
            ]),
        );
        pending.mark_artifact_generation_durable("active-session", 10);
        let survivor = pending
            .artifact_overlays
            .get("active-session")
            .expect("newer generation remains");
        assert_eq!(survivor.len(), 1);
        assert_eq!(survivor.values().next().unwrap().generation, 11);

        for generation in 12..(12 + MAX_TRACKED_ARTIFACT_HIGH_WATERS as u64 + 3) {
            pending
                .mark_artifact_generation_durable(&format!("transition-{generation}"), generation);
        }
        assert_eq!(
            pending.durable_artifact_high_waters.len(),
            MAX_TRACKED_ARTIFACT_HIGH_WATERS,
            "session transitions must bound retained non-content receipts"
        );
    }

    #[tokio::test]
    async fn interleaved_checkpoint_saves_and_clears_stay_per_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        let verification_manager = SessionManager::new(sessions_dir).expect("verification manager");
        let first = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let second = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let first_id = first.metadata.id.clone();
        let second_id = second.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        // Interleave: save A, save B, clear A — all coalesced into one drain.
        handle.try_send(PersistRequest::SaveCheckpoint { session: first });
        handle.try_send(PersistRequest::SaveCheckpoint { session: second });
        handle.try_send(PersistRequest::ClearCheckpoint {
            session_id: first_id.clone(),
        });
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");

        assert!(
            verification_manager
                .load_session_checkpoint(&first_id)
                .expect("load first checkpoint")
                .is_none(),
            "cleared session must have no checkpoint file"
        );
        let survivor = verification_manager
            .load_session_checkpoint(&second_id)
            .expect("load second checkpoint")
            .expect("second session's checkpoint must survive an unrelated clear");
        assert_eq!(survivor.metadata.id, second_id);
    }

    #[tokio::test]
    async fn flush_and_report_returns_completed_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir).expect("manager");
        let session = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SaveCheckpoint { session });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");
        // Whether the checkpoint was written by an earlier background cycle
        // or by this flush, the accumulated report must count it and show no
        // failures — and the actor keeps running afterwards.
        assert!(report.completed >= 1, "checkpoint write must be counted");
        assert!(report.failures.is_empty(), "no failures expected");
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }

    #[tokio::test]
    async fn flush_and_report_propagates_write_failures() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions_dir.clone()).expect("manager");
        // Occupy the checkpoints directory path with a regular file so every
        // checkpoint write deterministically fails on all platforms.
        std::fs::write(sessions_dir.join("checkpoints"), b"not a directory")
            .expect("block checkpoints dir");
        let session = crate::session_manager::create_saved_session_with_mode(
            &[],
            "deepseek-v4-pro",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        let session_id = session.metadata.id.clone();
        let (handle, task) = spawn_persistence_actor(manager);

        handle.try_send(PersistRequest::SaveCheckpoint { session });
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle.try_send(PersistRequest::FlushAndReport { reply: reply_tx });
        let report = reply_rx.await.expect("flush report reply");

        assert!(
            report
                .failures
                .iter()
                .any(|(what, _)| what == &format!("checkpoint:{session_id}")),
            "failed checkpoint write must be reported, got: {:?}",
            report.failures
        );
        handle.try_send(PersistRequest::Shutdown);
        task.await.expect("persistence actor join");
    }
}
