//! The name people know this computer by, for labels other machines show:
//! the transcription host's `/v1/hello` name and the `clientName` a client
//! sends when it pairs with a host.

const MAX_NAME_CHARS: usize = 64;

pub(crate) fn computer_name() -> Option<String> {
    platform_name()
        .map(|raw| clean_name(&raw))
        .filter(|name| !name.is_empty())
}

#[cfg(target_os = "macos")]
fn platform_name() -> Option<String> {
    // The name from System Settings ("Conal's MacBook Pro"), not the
    // `.local` host name.
    command_output("scutil", &["--get", "ComputerName"]).or_else(host_name)
}

#[cfg(windows)]
fn platform_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_name() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(host_name)
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    command_output("hostname", &[])
}

#[cfg(unix)]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
}

fn clean_name(raw: &str) -> String {
    let name = raw.trim();
    let name = name.strip_suffix(".local").unwrap_or(name);
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    name.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_lose_the_mdns_suffix() {
        assert_eq!(clean_name("studio-mac.local\n"), "studio-mac");
        assert_eq!(
            clean_name("  Conal's MacBook Pro \n"),
            "Conal's MacBook Pro"
        );
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME_CHARS);
        assert_eq!(clean_name("\n"), "");
    }

    #[test]
    fn this_machine_has_a_name() {
        assert!(computer_name().is_some_and(|name| !name.is_empty()));
    }
}
