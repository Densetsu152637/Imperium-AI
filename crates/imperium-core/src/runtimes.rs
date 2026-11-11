//! `environments.test_runtime`: whether an environment's runtime can be reached, in a sentence
//! the user can act on.

use std::time::Duration;

use imperium_protocol::methods::{EnvironmentRuntime, RuntimeTestResult};
use imperium_tools::runtime::{decode, ssh_target};
use imperium_tools::{ProcessSpec, RunOutput, Runner, ToolError};

const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

fn ok(message: String) -> RuntimeTestResult {
    RuntimeTestResult { ok: true, message }
}

fn failed(message: String) -> RuntimeTestResult {
    RuntimeTestResult { ok: false, message }
}

async fn run(runner: &dyn Runner, spec: ProcessSpec) -> Result<RunOutput, ToolError> {
    runner.run(&spec, None, CHECK_TIMEOUT).await
}

/// Checks `runtime` with `runner`; `windows` says whether WSL can exist here.
pub async fn test(
    runtime: &EnvironmentRuntime,
    runner: &dyn Runner,
    windows: bool,
) -> RuntimeTestResult {
    match runtime {
        EnvironmentRuntime::Local => ok("Tools run on this machine.".into()),
        EnvironmentRuntime::Docker { image, container } => {
            match run(
                runner,
                ProcessSpec::new("docker", ["version", "--format", "{{.Server.Version}}"]),
            )
            .await
            {
                Err(error) => {
                    return failed(format!("Docker is not installed or not on PATH ({error})."));
                }
                Ok(output) if !output.success() => {
                    return failed(format!(
                        "Docker is installed but its engine is not running: {}",
                        output.message()
                    ));
                }
                Ok(_) => {}
            }
            match container {
                Some(container) => {
                    let spec = ProcessSpec::new(
                        "docker",
                        ["inspect", "--format", "{{.State.Running}}", "--", container],
                    );
                    match run(runner, spec).await {
                        Ok(output) if output.success() => {
                            if decode(&output.stdout).trim() == "true" {
                                ok(format!(
                                    "Docker is running and so is container {container}."
                                ))
                            } else {
                                failed(format!(
                                    "Container {container} exists but is not running; start it \
                                     with: docker start {container}"
                                ))
                            }
                        }
                        Ok(output) => failed(format!(
                            "No container named {container}: {}",
                            output.message()
                        )),
                        Err(error) => failed(error.to_string()),
                    }
                }
                None => {
                    let spec = ProcessSpec::new(
                        "docker",
                        ["image", "inspect", "--format", "{{.Id}}", "--", image],
                    );
                    match run(runner, spec).await {
                        Ok(output) if output.success() => {
                            ok(format!("Docker is running and image {image} is present."))
                        }
                        Ok(_) => failed(format!(
                            "Docker is running but image {image} is not present; get it with: \
                             docker pull {image}"
                        )),
                        Err(error) => failed(error.to_string()),
                    }
                }
            }
        }
        EnvironmentRuntime::Wsl { distro } => {
            if !windows {
                return failed("WSL is only available on Windows.".into());
            }
            match run(runner, ProcessSpec::new("wsl.exe", ["--list", "--quiet"])).await {
                Err(error) => failed(format!("WSL is not installed ({error}).")),
                Ok(output) if !output.success() => {
                    failed(format!("WSL did not answer: {}", output.message()))
                }
                Ok(output) => {
                    let listed = decode(&output.stdout);
                    let installed: Vec<&str> = listed
                        .lines()
                        .map(|line| line.trim().trim_matches('\0'))
                        .filter(|line| !line.is_empty())
                        .collect();
                    if installed
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(distro))
                    {
                        ok(format!("WSL distribution {distro} is installed."))
                    } else if installed.is_empty() {
                        failed(format!(
                            "No WSL distributions are installed; install one with: wsl --install \
                             -d {distro}"
                        ))
                    } else {
                        failed(format!(
                            "No WSL distribution named {distro}; installed: {}.",
                            installed.join(", ")
                        ))
                    }
                }
            }
        }
        EnvironmentRuntime::Ssh { host } => {
            let (destination, port) = ssh_target(host);
            let mut args: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-T"]
                .iter()
                .map(|arg| arg.to_string())
                .collect();
            if let Some(port) = port {
                args.extend(["-p".into(), port]);
            }
            args.extend(["--".into(), destination, "true".into()]);
            let spec = ProcessSpec {
                program: "ssh".into(),
                args,
            };
            match run(runner, spec).await {
                Ok(output) if output.success() => ok(format!("Connected to {host} over SSH.")),
                Ok(output) => failed(format!(
                    "Could not connect to {host}: {} (Imperium uses your SSH config, keys, and \
                     agent, and cannot answer password prompts.)",
                    output.message()
                )),
                Err(error) => failed(format!("The ssh client is not available ({error}).")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use imperium_tools::runtime::RunFuture;

    use super::*;

    /// Answers by command line; anything unknown fails to start.
    #[derive(Debug, Default)]
    pub(crate) struct Scripted {
        answers: HashMap<String, (i32, String)>,
        pub ran: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn on(mut self, line: &str, code: i32, stdout: &str) -> Self {
            self.answers.insert(line.into(), (code, stdout.into()));
            self
        }
    }

    impl Runner for Scripted {
        fn run<'a>(
            &'a self,
            spec: &'a ProcessSpec,
            _stdin: Option<&'a [u8]>,
            _timeout: Duration,
        ) -> RunFuture<'a> {
            let line = spec.to_string();
            self.ran.lock().unwrap().push(line.clone());
            let answer = self.answers.get(&line).cloned();
            Box::pin(async move {
                let (code, stdout) =
                    answer.ok_or_else(|| ToolError::Io(format!("could not start {line}")))?;
                Ok(RunOutput {
                    code: Some(code),
                    stdout: stdout.clone().into_bytes(),
                    stderr: if code == 0 {
                        Vec::new()
                    } else {
                        stdout.into_bytes()
                    },
                })
            })
        }
    }

    const VERSION: &str = "docker version --format {{.Server.Version}}";

    fn docker(image: &str, container: Option<&str>) -> EnvironmentRuntime {
        EnvironmentRuntime::Docker {
            image: image.into(),
            container: container.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn docker_needs_the_engine_and_the_image_or_a_running_container() {
        let missing = Scripted::default();
        let result = test(&docker("rust:1", None), &missing, false).await;
        assert!(
            !result.ok && result.message.contains("not installed"),
            "{result:?}"
        );

        let stopped = Scripted::default().on(VERSION, 1, "Cannot connect to the Docker daemon");
        let result = test(&docker("rust:1", None), &stopped, false).await;
        assert!(
            !result.ok && result.message.contains("not running"),
            "{result:?}"
        );

        let no_image = Scripted::default().on(VERSION, 0, "27.0").on(
            "docker image inspect --format {{.Id}} -- rust:1",
            1,
            "No such image",
        );
        let result = test(&docker("rust:1", None), &no_image, false).await;
        assert!(
            !result.ok && result.message.contains("docker pull rust:1"),
            "{result:?}"
        );

        let present = Scripted::default().on(VERSION, 0, "27.0").on(
            "docker image inspect --format {{.Id}} -- rust:1",
            0,
            "sha256:1",
        );
        assert!(test(&docker("rust:1", None), &present, false).await.ok);

        let running = Scripted::default().on(VERSION, 0, "27.0").on(
            "docker inspect --format {{.State.Running}} -- web",
            0,
            "true\n",
        );
        assert!(test(&docker("x", Some("web")), &running, false).await.ok);
        let idle = Scripted::default().on(VERSION, 0, "27.0").on(
            "docker inspect --format {{.State.Running}} -- web",
            0,
            "false\n",
        );
        let result = test(&docker("x", Some("web")), &idle, false).await;
        assert!(
            !result.ok && result.message.contains("docker start web"),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn wsl_needs_windows_and_the_distribution() {
        let wsl = EnvironmentRuntime::Wsl {
            distro: "ubuntu".into(),
        };
        let listed: String = "Ubuntu\r\nDebian\r\n"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .map(char::from)
            .collect();
        // The fake hands back text; build the UTF-16 bytes the way wsl.exe writes them.
        let runner = Scripted::default().on("wsl.exe --list --quiet", 0, &listed);
        let result = test(&wsl, &runner, false).await;
        assert!(!result.ok && result.message.contains("only available on Windows"));
        let result = test(&wsl, &runner, true).await;
        assert!(result.ok, "{result:?}");
        let other = EnvironmentRuntime::Wsl {
            distro: "Arch".into(),
        };
        let result = test(&other, &runner, true).await;
        assert!(
            !result.ok && result.message.contains("Ubuntu, Debian"),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn ssh_connects_without_prompting() {
        let ssh = EnvironmentRuntime::Ssh {
            host: "me@box:2222".into(),
        };
        let line = "ssh -o BatchMode=yes -o ConnectTimeout=10 -T -p 2222 -- me@box true";
        let up = Scripted::default().on(line, 0, "");
        assert!(test(&ssh, &up, false).await.ok);
        let refused = Scripted::default().on(line, 255, "Permission denied (publickey).");
        let result = test(&ssh, &refused, false).await;
        assert!(
            !result.ok && result.message.contains("Permission denied"),
            "{result:?}"
        );
    }
}
