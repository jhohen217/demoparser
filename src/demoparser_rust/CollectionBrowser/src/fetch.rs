//! FACEIT acquisition panel. Secrets remain in this session, never in browser config or logs.
use crate::{app::BrowserApp, message::Message, style, theme::Theme};
use demoparser::demofetch::{self, cli::Mode, Credentials, FaceitClient};
use demoparser::fetch_pipeline::{FetchReport, FetchRequest};
use iced::futures::SinkExt;
use iced::widget::{button, column, container, row, text, text_input};
use iced::{Command, Element, Length, Subscription};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub struct State {
    pub expanded: bool,
    pub matches: String,
    pub output: String,
    pub data_key: String,
    pub download_token: String,
    pub concurrency: String,
    pub status: String,
    pub job: Option<Job>,
    generation: u64,
}

impl Default for State {
    fn default() -> Self {
        let settings = demofetch::settings::Settings::load(None);
        let status = settings
            .as_ref()
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        let settings = settings.unwrap_or_default();
        Self {
            expanded: false,
            matches: String::new(),
            output: settings
                .output_directory
                .unwrap_or_else(|| "D:\\".into())
                .to_string_lossy()
                .into_owned(),
            data_key: settings.credentials.data_key,
            download_token: settings.credentials.download_token,
            concurrency: settings.concurrency.unwrap_or(3).to_string(),
            status,
            job: None,
            generation: 0,
        }
    }
}

#[derive(Clone)]
pub struct Job {
    request: FetchRequest,
    client: FaceitClient,
    config: Option<demoparser_config::AppConfig>,
    cancel: demofetch::Cancel,
}

#[derive(Clone)]
pub enum FetchMessage {
    Toggle,
    Matches(String),
    Output(String),
    DataKey(String),
    DownloadToken(String),
    Concurrency(String),
    BrowseMatches,
    MatchesLoaded(Result<String, String>),
    BrowseOutput,
    OutputSelected(Option<std::path::PathBuf>),
    Start(Mode),
    Cancel,
    DownloadEvent(demofetch::Event),
    ParserEvent(demoparser::ProgressEvent),
    Finished(Result<FetchReport, String>),
}

impl std::fmt::Debug for FetchMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The parent message is Debug; prevent secrets leaking through debug logging.
        f.write_str(match self {
            Self::DataKey(_) | Self::DownloadToken(_) => "FACEIT credential changed [redacted]",
            _ => "FACEIT acquisition event",
        })
    }
}

fn msg(message: FetchMessage) -> Message {
    Message::Fetch(message)
}

pub fn view(app: &BrowserApp) -> Element<'_, Message, Theme, iced::Renderer> {
    let state = &app.fetch;
    let toggle = button(
        text(if state.expanded {
            "FACEIT downloads ▾"
        } else {
            "FACEIT downloads ▸"
        })
        .size(12),
    )
    .on_press(msg(FetchMessage::Toggle))
    .style(style::Button::ControlPanelLabel(false));
    if !state.expanded {
        return container(toggle).padding(4).into();
    }
    let matches_input = text_input(
        "Match IDs or room URLs, separated by spaces/commas",
        &state.matches,
    )
    .size(12);
    let output_input = text_input("Download folder", &state.output).size(12);
    let data_key = text_input("FACEIT Data API key", &state.data_key)
        .secure(true)
        .size(12);
    let download_token = text_input("Downloads API token", &state.download_token)
        .secure(true)
        .size(12);
    let concurrency = text_input("3", &state.concurrency).size(12).width(50);
    let busy = app.is_parsing;
    let mut file_button =
        button(text("Load ACE file").size(12)).style(style::Button::ControlPanelLabel(false));
    let mut folder_button =
        button(text("Folder...").size(12)).style(style::Button::ControlPanelLabel(false));
    let (matches_input, output_input, data_key, download_token, concurrency) = if !busy {
        file_button = file_button.on_press(msg(FetchMessage::BrowseMatches));
        folder_button = folder_button.on_press(msg(FetchMessage::BrowseOutput));
        (
            matches_input.on_input(|v| msg(FetchMessage::Matches(v))),
            output_input.on_input(|v| msg(FetchMessage::Output(v))),
            data_key.on_input(|v| msg(FetchMessage::DataKey(v))),
            download_token.on_input(|v| msg(FetchMessage::DownloadToken(v))),
            concurrency.on_input(|v| msg(FetchMessage::Concurrency(v))),
        )
    } else {
        (
            matches_input,
            output_input,
            data_key,
            download_token,
            concurrency,
        )
    };
    let inputs = row![matches_input, file_button].spacing(6);
    let mut actions = row![].spacing(8);
    for (label, mode) in [
        ("Download", Mode::Download),
        ("Download + Trim", Mode::Trim),
        ("Download + Trim + Parse", Mode::Parse),
    ] {
        let mut action =
            button(text(label).size(12)).style(style::Button::ControlPanelLabel(false));
        if !busy {
            action = action.on_press(msg(FetchMessage::Start(mode)));
        }
        actions = actions.push(action);
    }
    if state.job.is_some() {
        actions = actions.push(
            button(text("Cancel downloads").size(12))
                .on_press(msg(FetchMessage::Cancel))
                .style(style::Button::ControlPanelLabel(false)),
        );
    }
    container(
        column![
            toggle,
            inputs,
            row![
                output_input,
                folder_button,
                text("Concurrent").size(12),
                concurrency
            ]
            .spacing(6)
            .align_items(iced::Alignment::Center),
            row![data_key, download_token].spacing(6),
            actions,
            text("Credentials stay in this session. Completed demos are kept for retries.")
                .size(11),
            text(&state.status).size(12),
        ]
        .spacing(4),
    )
    .padding(6)
    .width(Length::Fill)
    .style(style::Container::Panel)
    .into()
}

pub fn subscription(app: &BrowserApp) -> Subscription<Message> {
    let Some(job) = app.fetch.job.clone() else {
        return Subscription::none();
    };
    iced::subscription::channel(
        (std::any::TypeId::of::<Job>(), app.fetch.generation),
        100,
        move |output| {
            let job = job.clone();
            async move {
                let mut final_sender = output.clone();
                let output = Arc::new(Mutex::new(output));
                let download_output = output.clone();
                let callback: demofetch::Callback = Arc::new(move |event| {
                    if let Ok(mut sender) = download_output.lock() {
                        let _ = sender.try_send(msg(FetchMessage::DownloadEvent(event)));
                    }
                });
                let result = demoparser::fetch_pipeline::run_fetch(
                    job.request,
                    job.client,
                    job.config,
                    job.cancel,
                    callback,
                    move |event| {
                        if let Ok(mut sender) = output.lock() {
                            let _ = sender.try_send(msg(FetchMessage::ParserEvent(event)));
                        }
                    },
                )
                .await
                .map_err(|error| format!("{error:#}"));
                let _ = final_sender.send(msg(FetchMessage::Finished(result))).await;
                std::future::pending::<std::convert::Infallible>().await
            }
        },
    )
}

pub fn handle(app: &mut BrowserApp, message: FetchMessage) -> Command<Message> {
    match message {
        FetchMessage::Toggle => app.fetch.expanded = !app.fetch.expanded,
        FetchMessage::Matches(value) => app.fetch.matches = value,
        FetchMessage::Output(value) => app.fetch.output = value,
        FetchMessage::DataKey(value) => app.fetch.data_key = value,
        FetchMessage::DownloadToken(value) => app.fetch.download_token = value,
        FetchMessage::Concurrency(value) => app.fetch.concurrency = value,
        FetchMessage::BrowseMatches => {
            return Command::perform(
                async {
                    match rfd::AsyncFileDialog::new()
                        .add_filter("ACE match queues", &["txt"])
                        .pick_file()
                        .await
                    {
                        Some(file) if !demofetch::cli::is_ace_queue(file.path()) =>
                            Err("Choose an ACE queue named ace.txt, ace_*.txt, or ace-*.txt".into()),
                        Some(file) => tokio::fs::read_to_string(file.path())
                            .await
                            .map_err(|error| error.to_string()),
                        None => Ok(String::new()),
                    }
                },
                |result| msg(FetchMessage::MatchesLoaded(result)),
            )
        }
        FetchMessage::MatchesLoaded(Ok(value)) if !value.is_empty() => app.fetch.matches = value,
        FetchMessage::MatchesLoaded(Err(error)) => app.fetch.status = error,
        FetchMessage::MatchesLoaded(_) => {}
        FetchMessage::BrowseOutput => {
            return Command::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|folder| folder.path().to_owned())
                },
                |folder| msg(FetchMessage::OutputSelected(folder)),
            )
        }
        FetchMessage::OutputSelected(Some(path)) => {
            app.fetch.output = path.to_string_lossy().into_owned()
        }
        FetchMessage::OutputSelected(None) => {}
        FetchMessage::Start(mode) => {
            if app.is_parsing {
                return Command::none();
            }
            let setup = (|| -> anyhow::Result<Job> {
                let matches = demofetch::match_inputs(&app.fetch.matches)?;
                if matches.is_empty() {
                    anyhow::bail!("Enter match IDs/URLs or load a batch text file");
                }
                if app.fetch.output.trim().is_empty() {
                    anyhow::bail!("Choose a download folder");
                }
                let concurrency: usize = app.fetch.concurrency.parse()?;
                if !(1..=16).contains(&concurrency) {
                    anyhow::bail!("Use 1–16 concurrent downloads");
                }
                let client = FaceitClient::new(Credentials {
                    data_key: app.fetch.data_key.clone(),
                    download_token: app.fetch.download_token.clone(),
                })?;
                let config = if mode == Mode::Download {
                    None
                } else {
                    let mut config = demoparser_config::AppConfig::load()?;
                    config.paths.parser_output = app.config.parser_output.clone();
                    config.paths.unzip_dir = app.config.unzip_dir.clone();
                    config.paths.ram_unzip = app.config.ram_unzip;
                    config.parser.aces = app.config.aces;
                    config.parser.quads = app.config.quads;
                    config.parser.triples = app.config.triples;
                    config.parser.multi = app.config.multi;
                    config.parser.singles = app.config.singles;
                    config.parser.doubles = app.config.doubles;
                    config.parser.overwrite = app.config.overwrite;
                    config.batch.threads = if app.config.threads > 0 {
                        Some(app.config.threads as usize)
                    } else {
                        None
                    };
                    Some(config)
                };
                Ok(Job {
                    request: FetchRequest {
                        matches,
                        output: app.fetch.output.trim().into(),
                        report_directory: None,
                        concurrency,
                        parse_batch_size: 8,
                        mode,
                    },
                    client,
                    config,
                    cancel: Arc::new(AtomicBool::new(false)),
                })
            })();
            match setup {
                Ok(job) => {
                    let output = job.request.output.clone();
                    if !app
                        .demo_directories
                        .iter()
                        .any(|directory| directory.path == output)
                    {
                        let directory = crate::models::DemoDirectory::new(output);
                        app.demo_directories.push(directory.clone());
                        app.config.demo_directories.push(directory);
                        if let Err(error) = app.config.save() {
                            app.fetch.status =
                                format!("Could not save download directory: {error}");
                            return app.log(app.fetch.status.clone());
                        }
                    }
                    app.fetch.generation += 1;
                    app.parsing_cancel_token = Some(job.cancel.clone());
                    app.fetch.job = Some(job);
                    app.is_parsing = true;
                    app.fetch.status = "Starting FACEIT downloads...".into();
                    return app.log(app.fetch.status.clone());
                }
                Err(error) => {
                    app.fetch.status = format!("{error:#}");
                    return app.log(app.fetch.status.clone());
                }
            }
        }
        FetchMessage::Cancel => {
            if let Some(job) = &app.fetch.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            app.fetch.status = "Canceling; completed demos will be kept...".into();
        }
        FetchMessage::DownloadEvent(event) => match event {
            demofetch::Event::DownloadProgress {
                match_id,
                bytes,
                total_bytes,
            } => {
                app.fetch.status = format!(
                    "{match_id}: {:.1} MiB{}",
                    bytes as f64 / 1048576.0,
                    total_bytes
                        .map(|total| format!(" / {:.1} MiB", total as f64 / 1048576.0))
                        .unwrap_or_default()
                );
            }
            demofetch::Event::MatchCompleted { result } => {
                let status = format!(
                    "{}: {} demos ready{}",
                    result.match_id,
                    result.files.len(),
                    result
                        .error
                        .map(|error| format!("; {error}"))
                        .unwrap_or_default()
                );
                app.fetch.status = status.clone();
                return app.log(status);
            }
            other => return app.log(format!("FACEIT: {other:?}")),
        },
        FetchMessage::ParserEvent(event) => {
            // Each parser batch emits Finished. Only the workflow's Finished event ends the job.
            if let demoparser::ProgressEvent::Finished {
                total_successful,
                total_failed,
                ..
            } = event
            {
                return app.log(format!(
                    "FACEIT parser batch: {total_successful} successful, {total_failed} failed"
                ));
            }
            return crate::handlers::parsing::handle_parsing_event(app, event);
        }
        FetchMessage::Finished(result) => {
            app.fetch.job = None;
            app.is_parsing = false;
            app.parsing_cancel_token = None;
            app.parsing_status.clear();
            app.col_current = 0;
            app.col_total = 0;
            app.tick_current = 0;
            app.tick_total = 0;
            app.batch_progress = 0.0;
            app.total_demos = 0;
            app.processed_demos = 0;
            app.fetch.status = match result {
                Ok(report) => format!(
                    "{}: {} matches, {} parser jobs{}. Report saved in download folder.",
                    if report.canceled {
                        "Canceled"
                    } else {
                        "Finished"
                    },
                    report.downloads.len(),
                    report.processing.len(),
                    if report.has_errors() {
                        "; some jobs failed — rerun to retry"
                    } else {
                        ""
                    }
                ),
                Err(error) => format!("FACEIT job failed: {error}"),
            };
            return Command::batch(vec![
                app.log(app.fetch.status.clone()),
                Command::perform(async {}, |_| Message::RefreshDatabases),
            ]);
        }
    }
    Command::none()
}
