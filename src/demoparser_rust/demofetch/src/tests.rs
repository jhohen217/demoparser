use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ID: &str = "1-cb038819-b0d0-4471-b25c-0e7468ab1eb1";

fn demo_bytes() -> Vec<u8> {
    b"PBDEMS2\0fixture-payload".to_vec()
}
fn gzip() -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&demo_bytes()).unwrap();
    encoder.finish().unwrap()
}
fn zstd() -> Vec<u8> {
    zstd::stream::encode_all(demo_bytes().as_slice(), 1).unwrap()
}
fn callback() -> Callback {
    Arc::new(|_| {})
}
fn cancel() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn normalizes_room_links_and_legacy_batch_files() {
    let uuid = ID.trim_start_matches("1-");
    let inputs =
        format!("\u{feff}{uuid}\n{ID}, https://www.faceit.com/en/cs2/room/{ID}?x=y\n# comment\n");
    assert_eq!(match_inputs(&inputs).unwrap(), vec![ID]);
    assert!(match_id("https://faceit.com.evil/room/cb038819-b0d0-4471-b25c-0e7468ab1eb1").is_err());
    assert!(match_id("../../x").is_err());
    assert!(match_id(&format!("https://faceit.com/en/players/{ID}")).is_err());
}

#[test]
fn downloads_follow_existing_month_folders_and_match_filenames() {
    let metadata = serde_json::json!({"finished_at":1775445316});
    assert_eq!(
        demo_path(Path::new("D:/"), ID, &metadata, 0, DemoFormat::Gzip).unwrap(),
        PathBuf::from(format!("D:/April26/{ID}.dem.gz"))
    );
    assert_eq!(
        demo_path(Path::new("D:/April26"), ID, &metadata, 1, DemoFormat::Zstd).unwrap(),
        PathBuf::from(format!("D:/April26/{ID}-map2.dem.zst"))
    );
    assert!(demo_path(
        Path::new("D:/"),
        ID,
        &serde_json::json!({}),
        0,
        DemoFormat::Dem
    )
    .is_err());
}

#[test]
fn retry_after_supports_seconds_http_dates_and_elapsed_dates() {
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1000);
    assert_eq!(
        rate_limit::retry_after(Some("120"), now),
        Some(Duration::from_secs(120))
    );
    let date = httpdate::fmt_http_date(now + Duration::from_secs(75));
    assert_eq!(
        rate_limit::retry_after(Some(&date), now),
        Some(Duration::from_secs(75))
    );
    let old = httpdate::fmt_http_date(now - Duration::from_secs(75));
    assert_eq!(
        rate_limit::retry_after(Some(&old), now),
        Some(Duration::ZERO)
    );
    assert_eq!(rate_limit::retry_after(Some("invalid"), now), None);
}

#[tokio::test(start_paused = true)]
async fn shared_gate_paces_clones_and_extends_all_worker_cooldowns() {
    let gate = Arc::new(rate_limit::RateGate::new());
    let worker = gate.clone();
    gate.wait(&cancel()).await.unwrap();
    let start = tokio::time::Instant::now();
    worker.wait(&cancel()).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(100));
    gate.defer(Duration::from_secs(5), true).await.unwrap();
    let start = tokio::time::Instant::now();
    worker.wait(&cancel()).await.unwrap();
    assert!(start.elapsed() >= Duration::from_secs(5));
    gate.defer(Duration::from_secs(3601), true).await.unwrap();
    let canceled = cancel();
    canceled.store(true, Ordering::Relaxed);
    assert!(worker.wait(&canceled).await.is_err());
}

#[test]
fn portable_ini_preserves_paths_and_redacts_credentials() {
    let ini = "\u{feff}; comment\n[FACEIT]\nDataApiKey = secret-data\nDownloadToken = \"secret-token=abc\"\nDownloadDirectory = D:\\Demos\\FACEIT\nConcurrency = 4\n[Parser]\nUnknown=anything";
    let settings = settings::Settings::parse(ini).unwrap();
    assert_eq!(settings.credentials.data_key, "secret-data");
    assert_eq!(settings.credentials.download_token, "secret-token=abc");
    assert_eq!(
        settings.output_directory.unwrap(),
        PathBuf::from("D:\\Demos\\FACEIT")
    );
    assert_eq!(settings.concurrency, Some(4));
    assert!(!format!("{:?}", settings.credentials).contains("secret"));
    assert!(settings::Settings::parse("[FACEIT]\nConcurrency=0").is_err());
    assert!(settings::Settings::parse("[FACEIT]\nConcurrency=17").is_err());
    assert!(settings::Settings::parse("[FACEIT]\nConcurrency=secret")
        .unwrap_err()
        .to_string()
        .find("secret")
        .is_none());
}

#[test]
fn validates_small_archives_and_detects_corruption() {
    let directory = tempfile::tempdir().unwrap();
    for (format, bytes) in [
        (DemoFormat::Dem, demo_bytes()),
        (DemoFormat::Gzip, gzip()),
        (DemoFormat::Zstd, zstd()),
    ] {
        let path = directory.path().join(format.extension());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(validate_demo(&path, format).unwrap().0, bytes.len() as u64);
        if !matches!(format, DemoFormat::Dem) {
            std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
            assert!(validate_demo(&path, format).is_err());
        }
    }
    let path = directory.path().join("error.dem");
    std::fs::write(&path, b"<html>Forbidden</html>").unwrap();
    assert!(validate_demo(&path, DemoFormat::Dem).is_err());
}

type Responses = Vec<(u16, Vec<u8>, Vec<(&'static str, &'static str)>)>;
async fn mock(
    responses: impl FnOnce(&str) -> Responses,
) -> (FaceitClient, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let responses = responses(&base);
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body, headers) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(index) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..index]).to_ascii_lowercase();
                    let length: usize = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse().unwrap())
                        .unwrap_or(0);
                    if request.len() >= index + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            let mut header = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n", body.len());
            for (key, value) in headers {
                header.push_str(&format!("{key}: {value}\r\n"));
            }
            header.push_str("\r\n");
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
        requests
    });
    let mut client = FaceitClient::new(Credentials {
        data_key: "data-only".into(),
        download_token: "downloads-only".into(),
    })
    .unwrap();
    client.data_base = base.clone();
    client.download_endpoint = format!("{base}/sign");
    client.cdn = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    client.retries = 0;
    client.monthly_layout = false;
    (client, server)
}

fn json(value: serde_json::Value) -> (u16, Vec<u8>, Vec<(&'static str, &'static str)>) {
    (200, serde_json::to_vec(&value).unwrap(), vec![])
}

#[tokio::test]
async fn fetches_every_demo_separates_auth_and_reuses_validated_files() {
    let (mut client, server) = mock(|base| {
        let metadata = serde_json::json!({"finished_at":1775445316,"demo_url": ["https://demos.faceit.com/map1.dem.gz", "https://demos.faceit.com/map2.dem.zst"]});
        vec![json(metadata.clone()), json(serde_json::json!({"payload":{"download_url":format!("{base}/cdn1?secret=signed")}})),
            (200, gzip(), vec![]), json(serde_json::json!({"payload":{"download_url":format!("{base}/cdn2?secret=signed")}})),
            (200, zstd(), vec![]), json(metadata)]
    }).await;
    client.monthly_layout = true;
    let directory = tempfile::tempdir().unwrap();
    let first = client
        .download_match(ID, directory.path(), &cancel(), &callback())
        .await;
    assert!(first.error.is_none(), "{:?}", first.error);
    assert_eq!(first.files.len(), 2);
    assert!(first.files.iter().all(|file| !file.reused));
    let second = client
        .download_match(ID, directory.path(), &cancel(), &callback())
        .await;
    assert!(second.error.is_none());
    assert!(second.files.iter().all(|file| file.reused));
    assert_eq!(first.files[0].sha256, second.files[0].sha256);
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert!(requests[0].starts_with(&format!("GET /matches/{ID}")));
    assert!(requests[0].contains("Bearer data-only"));
    assert!(requests[1].starts_with("POST /sign"));
    assert!(requests[1].contains("Bearer downloads-only"));
    assert!(requests[1].contains("\"resource_url\":\"https://demos.faceit.com/map1.dem.gz\""));
    assert!(!requests[2].to_ascii_lowercase().contains("authorization:"));
    assert!(!requests[4].to_ascii_lowercase().contains("authorization:"));
    let report = serde_json::to_string(&first).unwrap();
    assert!(!report.contains("signed"));
    assert!(!report.contains("data-only"));
    assert_eq!(
        first.files[0].path,
        directory.path().join(format!("April26/{ID}.dem.gz"))
    );
    assert_eq!(
        first.files[1].path,
        directory.path().join(format!("April26/{ID}-map2.dem.zst"))
    );
    assert_eq!(
        std::fs::read_dir(directory.path().join("April26"))
            .unwrap()
            .count(),
        2
    );
}

#[tokio::test]
async fn corrupt_transfer_does_not_publish_or_leave_partial_files() {
    let (client, server) = mock(|base| {
        vec![
            json(serde_json::json!({"demo_url": ["https://demos.faceit.com/demo.dem.gz"]})),
            json(
                serde_json::json!({"payload":{"download_url":format!("{base}/bad?token=secret")}}),
            ),
            (200, b"<html>Forbidden</html>".to_vec(), vec![]),
        ]
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let result = client
        .download_match(ID, directory.path(), &cancel(), &callback())
        .await;
    assert!(result.error.is_some());
    assert!(result.files.is_empty());
    assert!(!result.error.unwrap().contains("token=secret"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    server.await.unwrap();
}

#[tokio::test]
async fn rate_limit_retries_but_permission_failure_is_explicit() {
    let (mut client, server) = mock(|_| {
        vec![
            (429, vec![], vec![("Retry-After", "0")]),
            (403, b"sensitive server message".to_vec(), vec![]),
        ]
    })
    .await;
    client.retries = 1;
    let directory = tempfile::tempdir().unwrap();
    let result = client
        .download_match(ID, directory.path(), &cancel(), &callback())
        .await;
    let error = result.error.unwrap();
    assert!(error.contains("403"));
    assert!(error.contains("permission"));
    assert!(!error.contains("sensitive"));
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn cancellation_never_starts_new_network_work() {
    let client = FaceitClient::new(Credentials {
        data_key: "data".into(),
        download_token: "download".into(),
    })
    .unwrap();
    let cancel = cancel();
    cancel.store(true, Ordering::Relaxed);
    let directory = tempfile::tempdir().unwrap();
    let result = client
        .download_match(ID, directory.path(), &cancel, &callback())
        .await;
    assert!(result.error.unwrap().contains("Canceled"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn batch_delivers_completed_files_and_retains_failed_matches() {
    let (client, server) = mock(|base| {
        vec![
            json(serde_json::json!({"demo_url": ["https://demos.faceit.com/demo.dem.gz"]})),
            json(serde_json::json!({"payload":{"download_url":format!("{base}/cdn")}})),
            (200, gzip(), vec![]),
            json(serde_json::json!({"demo_url": []})),
        ]
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    download_batch(
        client,
        vec![ID.into(), "1-038d7563-8f22-4531-a070-25e61d75ef40".into()],
        directory.path().into(),
        1,
        cancel(),
        callback(),
        sender,
    )
    .await
    .unwrap();
    let first = receiver.recv().await.unwrap();
    let second = receiver.recv().await.unwrap();
    assert_eq!(first.files.len(), 1);
    assert!(first.error.is_none());
    assert!(second.files.is_empty());
    assert!(second.error.unwrap().contains("not ready"));
    assert!(receiver.recv().await.is_none());
    server.await.unwrap();
}

#[test]
fn report_updates_replace_atomically_and_preserve_other_files() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("report.json");
    let demo = directory.path().join("retained.dem");
    std::fs::write(&demo, demo_bytes()).unwrap();
    save_report(&report, &serde_json::json!({"completed": 1})).unwrap();
    save_report(&report, &serde_json::json!({"completed": 2})).unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(saved["completed"], 2);
    assert_eq!(std::fs::read(&demo).unwrap(), demo_bytes());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

fn candidates(count: usize) -> Vec<availability::Candidate> {
    (0..count)
        .map(|i| availability::Candidate {
            match_id: format!("match-{i}"),
            finished_at: i as u64 + 1,
            date_is_hint: false,
        })
        .collect()
}

#[test]
fn legacy_queue_dates_are_hints_and_counts_are_not_times() {
    let entries = input_matches(&format!(
        "03-01-26_0100_{ID}\n00-00-00_0001_1-772f32a9-f718-481e-882f-65389a4a7720"
    ))
    .unwrap();
    assert_eq!(entries[0].finished_at, Some(1772323200));
    assert!(entries[0].date_is_hint);
    assert_eq!(entries[1].finished_at, None);
}

#[test]
fn csv_dates_replace_duplicate_day_hints_and_accept_naive_utc() {
    let entries = input_matches(&format!("03-01-26_0100_{ID}\n{ID},2026-03-01T03:45:07.997Z\n1-772f32a9-f718-481e-882f-65389a4a7720,2025-05-10T12:04:52")).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].finished_at, Some(1772336707));
    assert!(!entries[0].date_is_hint);
    assert!(entries[1].finished_at.is_some());
    assert!(!entries[1].date_is_hint);
}

#[test]
fn purging_legacy_queues_preserves_retained_csv_rows() {
    let directory = tempfile::tempdir().unwrap();
    let queue = directory.path().join("matches.txt");
    let retained = "1-772f32a9-f718-481e-882f-65389a4a7720,2025-05-10T12:04:52";
    std::fs::write(&queue, format!("03-01-26_0100_{ID} # gone\n{retained}\n")).unwrap();
    availability::purge_queue(&queue, &[ID.into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(queue).unwrap(),
        format!("# gone\n{retained}\n")
    );
}

#[test]
fn queue_directory_recurses_only_into_ace_text_files() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("March26")).unwrap();
    std::fs::write(directory.path().join("March26/ace.txt"), ID).unwrap();
    std::fs::write(directory.path().join("March26/ACE_matchids_march26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("March26/quad_matchids_march26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("March26/match_ids_march26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("March26/match_filtered_march26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("March26/unapproved_matchids_march26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("config.ini"), "ignored").unwrap();
    let files = cli::queue_files_in(directory.path()).unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|path| path.file_name().unwrap().to_string_lossy().to_lowercase().starts_with("ace")));
}

#[test]
fn queue_directory_does_not_fall_back_to_quad_or_general_lists() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("quad_matchids_april26.txt"), ID).unwrap();
    std::fs::write(directory.path().join("match_ids_april26.txt"), ID).unwrap();
    assert!(cli::queue_files_in(directory.path()).is_err());
}

#[test]
fn explicit_queue_rejects_non_ace_sources() {
    use clap::Parser;
    for name in ["quad_matchids_april26.txt", "match_ids_april26.txt", "queue.txt"] {
        assert!(cli::Args::try_parse_from(["fetch", "--matches-file", name]).is_err());
    }
    for name in ["ace.txt", "ACE_matchids_april26.txt", "ace-0000.txt"] {
        assert!(cli::Args::try_parse_from(["fetch", "--matches-file", name]).is_ok());
    }
}

#[test]
fn mixed_row_cleanup_keeps_original_counts_timestamps_and_line_endings() {
    let directory = tempfile::tempdir().unwrap();
    let queue = directory.path().join("matches.txt");
    let survivor = "03-01-26_0105_1-772f32a9-f718-481e-882f-65389a4a7720";
    let csv = "1-20e90cae-0610-43c3-a2f8-2c13ab819dea,2026-04-01T03:52:13.163Z";
    std::fs::write(&queue, format!("{ID} {survivor}\r\n{csv}\r\n")).unwrap();
    availability::purge_queue(&queue, &[ID.into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(queue).unwrap(),
        format!("{survivor}\r\n{csv}\r\n")
    );
}

#[tokio::test]
async fn warmer_colder_search_narrows_boundary_and_queues_oldest_first() {
    use availability::Availability::*;
    let report = availability::search(
        candidates(1024),
        10,
        64,
        "fixture".into(),
        |candidate| {
            let index = candidate.finished_at - 1;
            async move { Ok(if index < 760 { Unavailable } else { Available }) }
        },
        &callback(),
    )
    .await
    .unwrap();
    assert_eq!(
        report.oldest_confirmed_available.unwrap().match_id,
        "match-760"
    );
    assert_eq!(
        report.queue,
        (760..770).map(|i| format!("match-{i}")).collect::<Vec<_>>()
    );
    // Boundary refinement fills the oldest queue; remaining budget now scouts older exceptions.
    assert_eq!(report.probes.len(), 64);
    assert!(report.budget_exhausted);
    assert!(report.older_unchecked > 0); // sampled boundary, not a false proof about every old row
}

#[tokio::test]
async fn unknown_uploads_and_saved_files_do_not_become_expired_links() {
    use availability::Availability::*;
    let report = availability::search(
        candidates(20),
        5,
        40,
        "fixture".into(),
        |candidate| {
            let index = candidate.finished_at - 1;
            async move {
                Ok(match index {
                    0..=9 => Unavailable,
                    10 => Unknown("not ready".into()),
                    11 => Saved,
                    _ => Available,
                })
            }
        },
        &callback(),
    )
    .await
    .unwrap();
    assert_eq!(
        report.queue,
        (12..17).map(|i| format!("match-{i}")).collect::<Vec<_>>()
    );
    assert!(!report.unavailable.contains(&"match-10".into()));
    assert!(!report.unavailable.contains(&"match-11".into()));
}

#[tokio::test]
async fn unknown_newest_is_not_evidence_that_older_demos_are_gone() {
    use availability::Availability::*;
    let report = availability::search(
        candidates(32),
        3,
        64,
        "fixture".into(),
        |candidate| {
            let index = candidate.finished_at - 1;
            async move {
                Ok(if index == 31 {
                    Unknown("upload pending".into())
                } else if index < 18 {
                    Unavailable
                } else {
                    Available
                })
            }
        },
        &callback(),
    )
    .await
    .unwrap();
    assert_eq!(report.queue, vec!["match-18", "match-19", "match-20"]);
}

#[tokio::test]
async fn nonmonotonic_older_survivor_is_found_without_using_a_fixed_age_cutoff() {
    use availability::Availability::*;
    let report = availability::search(
        candidates(32),
        3,
        64,
        "fixture".into(),
        |candidate| {
            let index = candidate.finished_at - 1;
            async move {
                Ok(if index == 0 || index >= 20 {
                    Available
                } else {
                    Unavailable
                })
            }
        },
        &callback(),
    )
    .await
    .unwrap();
    assert_eq!(
        report.oldest_confirmed_available.unwrap().match_id,
        "match-0"
    );
    assert_eq!(report.queue, vec!["match-0", "match-20", "match-21"]);
}

#[tokio::test]
async fn probe_permission_failure_aborts_search_without_expiry_evidence() {
    let result = availability::search(
        candidates(10),
        3,
        20,
        "fixture".into(),
        |_| async { Err(anyhow::anyhow!("Downloads API permission denied")) },
        &callback(),
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("permission"));
}

#[test]
fn expired_queue_cleanup_preserves_saved_archives_and_other_links() {
    let directory = tempfile::tempdir().unwrap();
    let queue = directory.path().join("queue.txt");
    let valid_id = "1-038d7563-8f22-4531-a070-25e61d75ef40";
    std::fs::write(
        &queue,
        format!("{} # expired\n{valid_id}\n", ID.trim_start_matches("1-")),
    )
    .unwrap();
    let archive = directory.path().join(format!("{ID}-demo1.dem.gz"));
    std::fs::write(&archive, gzip()).unwrap();
    availability::purge_queue(&queue, &[ID.into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(&queue).unwrap(),
        format!("# expired\n{valid_id}\n")
    );
    assert!(archive.is_file());
}

#[tokio::test]
async fn availability_probe_checks_cdn_range_instead_of_trusting_signed_url() {
    let (client, server) = mock(|base| vec![
        json(serde_json::json!({"demo_url":["https://demos.faceit.com/demo.dem.gz"]})),
        json(serde_json::json!({"payload":{"download_url":format!("{base}/expired?signature=private")}})),
        (404, vec![], vec![]),
    ]).await;
    let directory = tempfile::tempdir().unwrap();
    let candidate = availability::Candidate {
        match_id: ID.into(),
        finished_at: 100,
        date_is_hint: false,
    };
    assert_eq!(
        client
            .probe_available(&candidate, directory.path(), &cancel(), &callback())
            .await
            .unwrap(),
        availability::Availability::Unavailable
    );
    let requests = server.await.unwrap();
    assert!(requests[2]
        .to_ascii_lowercase()
        .contains("range: bytes=0-7"));
    assert!(!requests[2].to_ascii_lowercase().contains("authorization:"));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn sign_access_denial_does_not_get_classified_as_expired() {
    let (client, server) = mock(|_| {
        vec![
            json(serde_json::json!({"demo_url":["https://demos.faceit.com/demo.dem.gz"]})),
            (403, vec![], vec![]),
        ]
    })
    .await;
    let directory = tempfile::tempdir().unwrap();
    let candidate = availability::Candidate {
        match_id: ID.into(),
        finished_at: 100,
        date_is_hint: false,
    };
    assert!(client
        .probe_available(&candidate, directory.path(), &cancel(), &callback())
        .await
        .unwrap_err()
        .to_string()
        .contains("permission"));
    server.await.unwrap();
}

#[tokio::test]
async fn cdn_rate_limit_retries_range_without_authorization_or_false_expiry() {
    let (mut client, server) = mock(|base| vec![
        json(serde_json::json!({"demo_url":["https://demos.faceit.com/demo.dem.gz"]})),
        json(serde_json::json!({"payload":{"download_url":format!("{base}/cdn?private=signature")}})),
        (429, vec![], vec![("Retry-After", "0")]),
        (206, gzip()[..8].to_vec(), vec![]),
    ]).await;
    client.retries = 1;
    let directory = tempfile::tempdir().unwrap();
    let candidate = availability::Candidate {
        match_id: ID.into(),
        finished_at: 100,
        date_is_hint: false,
    };
    assert_eq!(
        client
            .probe_available(&candidate, directory.path(), &cancel(), &callback())
            .await
            .unwrap(),
        availability::Availability::Available
    );
    let requests = server.await.unwrap();
    for request in &requests[2..] {
        assert!(request.to_ascii_lowercase().contains("range: bytes=0-7"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
    }
}

#[tokio::test]
async fn remaining_probe_budget_finds_older_exceptions_after_queue_is_full() {
    let report = availability::search(
        candidates(100),
        1,
        100,
        "test".into(),
        |candidate| {
            let index = candidate.finished_at - 1;
            async move {
                Ok(if index == 3 || index >= 90 {
                    availability::Availability::Available
                } else {
                    availability::Availability::Unavailable
                })
            }
        },
        &callback(),
    )
    .await
    .unwrap();
    assert_eq!(report.queue, vec!["match-3"]);
    assert_eq!(report.older_unchecked, 0);
}
