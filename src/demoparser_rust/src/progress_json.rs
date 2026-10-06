//! Machine-readable progress output for host applications.
//!
//! The CLI's normal output is prose and terminal decoration written for a person watching a
//! console. A GUI driving this parser as a child process has to scrape that prose to learn
//! anything, which breaks the first time a message is reworded and can only ever recover the
//! coarse facts the prose happens to state.
//!
//! `--ndjson` switches stdout to one JSON object per line instead: every [`ProgressEvent`] the
//! library emits, including the granular ones the human-facing path discards. The prose path is
//! untouched and remains the default, so nothing that runs this today changes behaviour.

use demoparser::ProgressEvent;
use serde_json::json;

/// Writes one event as a single JSON line on stdout.
///
/// Each line is a complete object with a `type` discriminator, so a reader can dispatch without
/// buffering and a line it does not recognise can be skipped rather than being a parse failure —
/// which is what lets events be added here without breaking existing readers.
pub fn emit(event: &ProgressEvent) {
    let value = match event {
        ProgressEvent::ReplayProgress { filename, phase, completed, total } => json!({
            "type": "replay_progress", "filename": filename, "phase": phase,
            "completed": completed, "total": total,
        }),
        ProgressEvent::PipelineMetrics {
            completed_demos,
            skipped_demos,
            failed_demos,
            elapsed,
            final_sample,
        } => json!({
            "type": "pipeline_metrics", "completed_demos": completed_demos,
            "skipped_demos": skipped_demos, "failed_demos": failed_demos,
            "elapsed": elapsed, "final": final_sample, "unit": "seconds/demo",
            "seconds_per_demo": demoparser::pipeline_metrics::seconds_per_demo(*elapsed, *completed_demos),
        }),
        ProgressEvent::PhaseCompleted { phase, elapsed } => json!({
            "type": "phase_completed", "phase": phase, "elapsed": elapsed,
        }),
        ProgressEvent::Started {
            total_files,
            total_batches,
        } => json!({
            "type": "started",
            "total_files": total_files,
            "total_batches": total_batches,
        }),

        ProgressEvent::BatchStarted {
            batch_num,
            total_batches,
            file_count,
        } => json!({
            "type": "batch_started",
            "batch": batch_num,
            "total_batches": total_batches,
            "file_count": file_count,
        }),

        ProgressEvent::BatchCompleted {
            batch_num,
            total_batches,
            demos_count,
            collections_count,
            elapsed,
        } => json!({
            "type": "batch_completed",
            "batch": batch_num,
            "total_batches": total_batches,
            "demos": demos_count,
            "collections": collections_count,
            "elapsed": elapsed,
        }),

        ProgressEvent::DemoProcessed { filename } => json!({
            "type": "demo_processed",
            "filename": filename,
        }),

        ProgressEvent::UnzippedFile { filename } => json!({
            "type": "unzipped",
            "filename": filename,
        }),

        ProgressEvent::CollectionWritingProgress { completed, total } => json!({
            "type": "collection_writing",
            "completed": completed,
            "total": total,
        }),

        ProgressEvent::TickWritingProgress { completed, total } => json!({
            "type": "tick_writing",
            "completed": completed,
            "total": total,
        }),

        ProgressEvent::Error { path, error } => json!({
            "type": "error",
            "path": path.to_string_lossy(),
            "error": error,
        }),

        ProgressEvent::Finished {
            total_successful,
            total_failed,
            elapsed,
        } => json!({
            "type": "finished",
            "successful": total_successful,
            "failed": total_failed,
            "elapsed": elapsed,
        }),
    };

    println!("{}", value);
}

/// The protocol version and feature list this build supports, as one JSON line.
///
/// `protocol` is bumped when an existing event's shape changes in a way a reader must know about;
/// adding a new event type does not bump it, because readers skip lines they do not recognise.
/// `features` names what can be asked for, so a host can refuse an operation this build would
/// silently ignore rather than perform.
pub fn capabilities() -> String {
    json!({
        "protocol": 1,
        "faceit": {
            "command": "fetch", "modes": ["download", "trim", "parse"],
            "batch_inputs": ["match_ids", "room_urls", "matches_file", "matches_directory", "dated_csv", "legacy_date_count_ids", "nickname"],
            "credentials": "[FACEIT] in config.ini or faceit.ini beside executable",
            "default_download_directory": "D:/",
            "report_directory_flag": "--report-directory",
            "queue_sources": "ACE text files only: ace.txt, ace_*.txt, ace-*.txt (case insensitive)",
            "download_layout": "MonthYY/match-id.dem.gz (or .dem.zst/.dem); additional maps use -mapN",
            "availability_search": {"flag":"--oldest-available", "default_search_days":365, "default_probe_budget":64, "oldest_first":true, "cleanup_flag":"--purge-expired"},
        },
        "features": ["verified-source-cleanup-v1", "s2r-world-entities-v1", "s2r-utility-binary-v2", "s2r-utility-authority-v1", "boolean-flags-preserve-inputs-v1", "legacy-round-checkpoint-prefix-v1", "s2r-weapon-material-inputs-v1", "s2r-input-mask-source-v1", "folder-storage-v2",
                "round-local-replay", "pipeline-seconds-per-demo", "phase-timing", "trim-before-replay", "ndjson", "select", "asset-refresh-only", "canonical-demo-identity", "repair-catalog", "catalog-repair-bulk", "dem-metadata-backfill", "trim", "trim-many", "trim-integrated", "trim-type-filter", "dem-identity-card-v1", "dem-v2-full-round-tail", "s2r-v11-stable-player-indexes", "suppress-shots-v1", "map-retarget-v1", "writer-diagnostics-v1", "faceit-fetch", "faceit-fetch-batch", "faceit-fetch-pipeline", "faceit-portable-ini", "faceit-oldest-available", "faceit-expired-queue-cleanup"],
    })
    .to_string()
}
