//! Command-line flags of the `transcription-host` binary. Without flags it
//! serves; configuration otherwise comes from `FAIRSPOKEN_HOST_*` variables (`MULTIVOICE_HOST_*` still accepted).

use super::config::{
    default_host_config_path, load_persisted_config, write_persisted_config, HostRuntimeConfig,
    PersistedHostConfig,
};
use super::update::{
    check_feed, describe_update, download_and_install, resolve_channel, ChannelSource,
    UpdateChannel, UpdatePrefs, UpdaterOptions, CHANNEL_ENV, UPDATE_AVAILABLE_EXIT_CODE,
};
use super::SERVER_VERSION;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

pub(super) const USAGE: &str = "\
Usage: transcription-host [OPTION]

Runs the Fairspoken transcription host (configured with FAIRSPOKEN_HOST_*
environment variables; see README.md and PROTOCOL.md).

Options:
  --version                       Print the version and exit
  --check-update [--channel C]    Check for an update; exit 0 when up to date,
                                  10 when an update is available
  --update [--channel C] [--yes]  Download, verify and install the latest
                                  release for the update channel (--channel
                                  also saves C as the channel). Restart the
                                  host afterwards to run it
  --set-update-channel C          Save the update channel (stable or nightly)
  -h, --help                      Show this help";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum HostCommand {
    Serve,
    Version,
    Help,
    CheckUpdate {
        channel: Option<UpdateChannel>,
    },
    Update {
        channel: Option<UpdateChannel>,
        yes: bool,
    },
    SetUpdateChannel(UpdateChannel),
}

fn parse_channel(raw: Option<&str>, flag: &str) -> Result<UpdateChannel, String> {
    let raw = raw.ok_or_else(|| format!("{flag} needs a channel: stable or nightly"))?;
    UpdateChannel::parse(raw).ok_or_else(|| format!("{flag} must be stable or nightly, not {raw}"))
}

fn set_action(current: &mut Option<&'static str>, name: &'static str) -> Result<(), String> {
    if let Some(existing) = current {
        return Err(format!("{existing} and {name} cannot be combined"));
    }
    *current = Some(name);
    Ok(())
}

pub(super) fn parse_args(args: &[String]) -> Result<HostCommand, String> {
    let mut action: Option<&'static str> = None;
    let mut channel = None;
    let mut set_channel = None;
    let mut yes = false;
    let mut iter = args.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        match flag {
            "--version" | "-V" => set_action(&mut action, "--version")?,
            "--help" | "-h" => set_action(&mut action, "--help")?,
            "--check-update" => set_action(&mut action, "--check-update")?,
            "--update" => set_action(&mut action, "--update")?,
            "--set-update-channel" => {
                set_action(&mut action, "--set-update-channel")?;
                let value = inline.or_else(|| iter.next());
                set_channel = Some(parse_channel(value, flag)?);
            }
            "--channel" => {
                let value = inline.or_else(|| iter.next());
                channel = Some(parse_channel(value, flag)?);
            }
            "--yes" | "-y" => yes = true,
            other => return Err(format!("Unknown option: {other}")),
        }
    }
    if channel.is_some() && !matches!(action, Some("--check-update" | "--update")) {
        return Err("--channel only goes with --check-update or --update".to_string());
    }
    if yes && action != Some("--update") {
        return Err("--yes only goes with --update".to_string());
    }
    Ok(match action {
        None => HostCommand::Serve,
        Some("--version") => HostCommand::Version,
        Some("--help") => HostCommand::Help,
        Some("--check-update") => HostCommand::CheckUpdate { channel },
        Some("--update") => HostCommand::Update { channel, yes },
        Some(_) => HostCommand::SetUpdateChannel(
            set_channel.expect("--set-update-channel always parses a channel"),
        ),
    })
}

/// Runs a non-serving command and returns the process exit status.
pub(super) fn run(command: HostCommand) -> Result<i32, String> {
    match command {
        HostCommand::Serve => Ok(0),
        HostCommand::Version => {
            println!("transcription-host {SERVER_VERSION}");
            Ok(0)
        }
        HostCommand::Help => {
            println!("{USAGE}");
            Ok(0)
        }
        HostCommand::SetUpdateChannel(channel) => {
            let path = default_host_config_path();
            save_channel(&path, channel)?;
            println!(
                "Update channel set to {} in {}.",
                channel.as_str(),
                path.display()
            );
            if crate::app_dirs::env_var(CHANNEL_ENV).is_some_and(|value| !value.trim().is_empty()) {
                println!("Note: {CHANNEL_ENV} is set and overrides the saved channel.");
            }
            println!("Restart a running host for it to take effect.");
            Ok(0)
        }
        HostCommand::CheckUpdate { channel } => check_update(channel),
        HostCommand::Update { channel, yes } => update(channel, yes),
    }
}

fn load_prefs(path: &Path) -> Result<UpdatePrefs, String> {
    Ok(load_persisted_config(path)?
        .map(|persisted| persisted.update)
        .unwrap_or_default())
}

fn save_channel(path: &Path, channel: UpdateChannel) -> Result<(), String> {
    let persisted = match load_persisted_config(path)? {
        Some(mut persisted) => {
            persisted.update.update_channel = Some(channel);
            persisted
        }
        // First write: seed the rest from the environment, as the host would.
        None => {
            let config = HostRuntimeConfig::from_env()?;
            PersistedHostConfig {
                max_active_streams: config.max_active_streams,
                max_recording_seconds: config.max_recording_seconds,
                use_gpu: config.use_gpu,
                worker_models: config.worker_models,
                update: UpdatePrefs {
                    update_channel: Some(channel),
                    auto_update: false,
                },
            }
        }
    };
    write_persisted_config(path, &persisted)
}

fn channel_note(source: ChannelSource) -> &'static str {
    match source {
        ChannelSource::Env => "set by FAIRSPOKEN_HOST_UPDATE_CHANNEL",
        ChannelSource::Saved => "saved choice",
        ChannelSource::Version => "from the running version",
    }
}

fn effective_channel(
    one_off: Option<UpdateChannel>,
) -> Result<(UpdateChannel, &'static str), String> {
    if let Some(channel) = one_off {
        return Ok((channel, "--channel"));
    }
    let prefs = load_prefs(&default_host_config_path())?;
    let env = crate::app_dirs::env_var(CHANNEL_ENV);
    let (channel, source) = resolve_channel(env.as_deref(), prefs.update_channel, SERVER_VERSION)?;
    Ok((channel, channel_note(source)))
}

fn check_update(one_off: Option<UpdateChannel>) -> Result<i32, String> {
    let (channel, note) = effective_channel(one_off)?;
    let options = UpdaterOptions::from_env(SERVER_VERSION)?;
    println!("Current version: {SERVER_VERSION}");
    println!("Channel: {} ({note})", channel.as_str());
    match check_feed(&options.feed_base, SERVER_VERSION, channel)? {
        Some(update) => {
            println!("Available: {}", describe_update(&update));
            if let Some(notes) = &update.notes {
                println!("Release notes: {notes}");
            }
            println!("Install it with: transcription-host --update");
            Ok(UPDATE_AVAILABLE_EXIT_CODE)
        }
        None => {
            println!("Available: none (up to date)");
            Ok(0)
        }
    }
}

fn confirm(question: &str) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        return Err("Not a terminal: pass --yes to install without asking".to_string());
    }
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|err| format!("Failed to read the answer: {err}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn update(one_off: Option<UpdateChannel>, yes: bool) -> Result<i32, String> {
    if let Some(channel) = one_off {
        save_channel(&default_host_config_path(), channel)?;
    }
    let (channel, note) = effective_channel(None)?;
    let options = UpdaterOptions::from_env(SERVER_VERSION)?;
    println!("Current version: {SERVER_VERSION}");
    println!("Channel: {} ({note})", channel.as_str());
    let Some(update) = check_feed(&options.feed_base, SERVER_VERSION, channel)? else {
        println!("Already up to date.");
        return Ok(0);
    };
    println!("Available: {}", describe_update(&update));
    if !yes && !confirm(&format!("Install {}?", describe_update(&update)))? {
        println!("Not installed.");
        return Ok(1);
    }
    let mut last_reported = None;
    let report = download_and_install(
        &update,
        &options.public_key,
        &options.exe_path,
        |downloaded, total| {
            let Some(total) = total.filter(|total| *total > 0) else {
                return;
            };
            let decile = (downloaded.min(total) * 10 / total) as u8;
            if last_reported != Some(decile) {
                last_reported = Some(decile);
                eprint!("\rDownloading… {}%", u32::from(decile) * 10);
                if decile == 10 {
                    eprintln!();
                }
            }
        },
    )?;
    println!(
        "Installed Fairspoken host {} at {} (the previous version is kept at {}).",
        report.version,
        report.target.display(),
        report.previous.display()
    );
    println!("Restart the host to run it, e.g.:");
    println!("  systemd: systemctl --user restart fairspoken-transcription-host");
    println!("  launchd: launchctl kickstart -k gui/$(id -u)/ie.fairspoken.transcription-host");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<HostCommand, String> {
        parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_every_command() {
        assert_eq!(parse(&[]), Ok(HostCommand::Serve));
        assert_eq!(parse(&["--version"]), Ok(HostCommand::Version));
        assert_eq!(parse(&["-h"]), Ok(HostCommand::Help));
        assert_eq!(
            parse(&["--check-update"]),
            Ok(HostCommand::CheckUpdate { channel: None })
        );
        assert_eq!(
            parse(&["--check-update", "--channel", "nightly"]),
            Ok(HostCommand::CheckUpdate {
                channel: Some(UpdateChannel::Nightly)
            })
        );
        assert_eq!(
            parse(&["--update", "--channel=stable", "--yes"]),
            Ok(HostCommand::Update {
                channel: Some(UpdateChannel::Stable),
                yes: true
            })
        );
        assert_eq!(
            parse(&["-y", "--update"]),
            Ok(HostCommand::Update {
                channel: None,
                yes: true
            })
        );
        assert_eq!(
            parse(&["--set-update-channel", "Nightly"]),
            Ok(HostCommand::SetUpdateChannel(UpdateChannel::Nightly))
        );
        assert_eq!(
            parse(&["--set-update-channel=stable"]),
            Ok(HostCommand::SetUpdateChannel(UpdateChannel::Stable))
        );
    }

    #[test]
    fn rejects_bad_combinations_and_values() {
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--update", "--check-update"]).is_err());
        assert!(parse(&["--set-update-channel"]).is_err());
        assert!(parse(&["--set-update-channel", "beta"]).is_err());
        assert!(parse(&["--channel", "stable"]).is_err());
        assert!(parse(&["--check-update", "--yes"]).is_err());
        assert!(parse(&["--update", "--channel"]).is_err());
    }

    #[test]
    fn saving_a_channel_keeps_the_rest_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-config.json");
        let model = crate::models::SttModel::Parakeet.model_id();
        std::fs::write(
            &path,
            format!(r#"{{"maxActiveStreams":7,"maxRecordingSeconds":120,"useGpu":false,"workerModels":["{model}"],"autoUpdate":true}}"#),
        )
        .unwrap();
        save_channel(&path, UpdateChannel::Nightly).unwrap();
        let saved = load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(saved.max_active_streams, 7);
        assert_eq!(saved.max_recording_seconds, 120);
        assert!(!saved.use_gpu);
        assert_eq!(saved.update.update_channel, Some(UpdateChannel::Nightly));
        assert!(saved.update.auto_update);
        assert_eq!(load_prefs(&path).unwrap(), saved.update);
    }
}
