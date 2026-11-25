//! Opens a web address in the user's own browser, for sign-ins to remote MCP servers. Imperium
//! only hands the address over; it never drives the browser or reads anything back from it.

use std::process::Command;

/// The address to open: an `http` or `https` URL without credentials, as the browser should see it.
fn web_address(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| format!("not a web address: {error}"))?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err("only web addresses can be opened".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("an address with a password in it will not be opened".into());
    }
    Ok(parsed.to_string())
}

/// Opens `url` (http or https only) in the default browser.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    let address = web_address(&url)?;
    let mut command = if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else if cfg!(target_os = "macos") {
        Command::new("open")
    } else {
        Command::new("xdg-open")
    };
    // The address is one argument, never parsed by a shell.
    command.arg(address);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not open the browser: {error}"))?;
    // Reaped in the background so the opener does not linger as a zombie process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::web_address;

    #[test]
    fn only_plain_web_addresses_are_opened() {
        assert!(web_address("https://auth.example.com/authorize?x=1&y=2").is_ok());
        assert!(web_address("http://127.0.0.1:8080/a").is_ok());
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "calc.exe",
            "https://user:pw@example.com/",
        ] {
            assert!(web_address(bad).is_err(), "{bad}");
        }
    }
}
