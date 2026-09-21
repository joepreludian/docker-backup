//! Wires real adapters to the services and maps results to exit codes.

use std::io::{self, IsTerminal, Write};

use clap::Parser;

use crate::application::backup::BackupService;
use crate::application::doctor::DoctorService;
use crate::application::info::InfoService;
use crate::application::ports::Clock;
use crate::application::restore::RestoreService;
use crate::cli::{Cli, Command};
use crate::domain::error::AppResult;
use crate::infrastructure::clock::SystemClock;
use crate::infrastructure::confirm::TerminalConfirm;
use crate::infrastructure::docker_cli::DockerCli;
use crate::infrastructure::fs_store::FsArchiveStore;
use crate::infrastructure::progress::StderrProgress;
use crate::infrastructure::tools::WhichToolLocator;
use crate::presentation::{OutputFormat, renderer_for};

pub fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return if error.use_stderr() { 2 } else { 0 };
        }
    };

    let format = if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    };
    let color = !cli.json && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = renderer_for(format, color);

    let docker = DockerCli::new(cli.docker_context.clone(), cli.helper_image.clone());
    let store = FsArchiveStore::new();
    let clock = SystemClock;
    let progress = StderrProgress::new();
    let tools = WhichToolLocator;

    // Unlocked handles: `Write` locks per call, so this doesn't hold stdout/stderr
    // for the whole run. `StderrProgress`'s ticker thread writes to stderr every
    // 120ms; holding a lock here for the run's duration would deadlock against it.
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    let result: AppResult<i32> = match &cli.command {
        Command::Backup(args) => {
            let request = args.to_request(clock.now_utc());
            BackupService {
                docker: &docker,
                store: &store,
                progress: &progress,
                clock: &clock,
            }
            .run(&request)
            .and_then(|report| {
                renderer.render_backup(&report, &mut stdout)?;
                Ok(report.exit_code())
            })
        }
        Command::Restore(args) => {
            // Prompts/boxes go to stderr, not stdout, so they get their own color
            // decision based on stderr's tty-ness (not `color`, which is stdout's).
            let prompt_color =
                !cli.json && io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            let confirm = TerminalConfirm::new(
                io::BufReader::new(io::stdin()),
                io::stderr(),
                args.yes,
                io::stdin().is_terminal() && !cli.json,
                prompt_color,
            );
            RestoreService {
                docker: &docker,
                store: &store,
                progress: &progress,
                confirm: &confirm,
            }
            .run(&args.to_request())
            .and_then(|report| {
                renderer.render_restore(&report, &mut stdout)?;
                Ok(report.exit_code())
            })
        }
        Command::Info(args) => InfoService { store: &store }
            .run(&args.source)
            .and_then(|report| {
                renderer.render_info(&report, &mut stdout)?;
                Ok(report.exit_code())
            }),
        Command::Doctor => DoctorService {
            docker: &docker,
            tools: &tools,
        }
        .run()
        .and_then(|report| {
            renderer.render_doctor(&report, &mut stdout)?;
            Ok(report.exit_code())
        }),
    };

    let code = match result {
        Ok(code) => code,
        Err(error) => {
            let _ = renderer.render_error(&error, &mut stdout, &mut stderr);
            error.exit_code()
        }
    };
    let _ = stdout.flush();
    code
}
