//! One embedded, non-listening Dolt SQL process for inspecting a restored copy.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(super) struct Session {
    child: Child,
    input: Option<ChildStdin>,
    values: Receiver<std::result::Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    stderr: File,
    failed: bool,
}
impl Session {
    pub(super) fn start(
        binary: &Path,
        database: &Path,
        home: &Path,
        error_log: &Path,
    ) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let stderr = options.open(error_log)?;
        let mut command = Command::new(binary);
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|k| k.starts_with("DOLT_") || k.starts_with("GIT_"))
            {
                command.env_remove(key);
            }
        }
        let mut child = command
            .args(["sql", "-r", "json"])
            .current_dir(database)
            .env("DOLT_ROOT_PATH", home)
            .env("DOLT_DISABLE_EVENT_FLUSH", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let input = child.stdin.take().context("backup_inspection_pipe")?;
        let output = child.stdout.take().context("backup_inspection_pipe")?;
        let (sender, values) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let mut line = String::new();
                // Pinned Dolt emits one JSON object per result, with escaped text.
                let result = output
                    .by_ref()
                    .take(1024 * 1024 * 1024 + 1)
                    .read_line(&mut line);
                let value = match result {
                    Ok(0) => break,
                    Ok(_) if line.len() > 1024 * 1024 * 1024 => {
                        Err("backup_input_limit: inspection result exceeds 1 GiB".into())
                    }
                    Ok(_) if line.trim().is_empty() => continue,
                    Ok(_) => serde_json::from_str(&line)
                        .map_err(|_| "backup_inspection_failed: invalid JSON framing".into()),
                    Err(_) => Err("backup_inspection_failed: verifier output failed".into()),
                };
                let failed = value.is_err();
                if sender.send(value).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            values,
            reader: Some(reader),
            stderr,
            failed: false,
        })
    }
    pub(super) fn query(&mut self, sql: &str) -> Result<Vec<Value>> {
        ensure!(
            !self.failed,
            "backup_inspection_failed: verifier is no longer usable"
        );
        ensure!(
            ["SELECT ", "SHOW ", "USE "]
                .iter()
                .any(|prefix| sql.starts_with(prefix)),
            "backup_inspection_readonly: unsupported statement"
        );
        let result = (|| {
            let token = uuid::Uuid::new_v4().simple().to_string();
            let input = self.input.as_mut().context("backup_inspection_closed")?;
            writeln!(
                input,
                "{}; SELECT '{}' AS __scrolls_frame;",
                sql.trim_end_matches(';'),
                token
            )?;
            input.flush()?;
            let deadline = Instant::now() + Duration::from_secs(300);
            let mut rows = vec![];
            loop {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .context("backup_inspection_timeout")?;
                let value = self
                    .values
                    .recv_timeout(remaining)
                    .context("backup_inspection_failed: verifier stopped or timed out")?
                    .map_err(anyhow::Error::msg)?;
                let Value::Object(mut object) = value else {
                    bail!("backup_inspection_failed: missing result");
                };
                // Pinned Dolt emits {} for an empty result set.
                if object.is_empty() {
                    continue;
                }
                let Value::Array(values) = object
                    .remove("rows")
                    .context("backup_inspection_failed: missing rows")?
                else {
                    bail!("backup_inspection_failed: invalid rows");
                };
                if values.len() == 1 && values[0]["__scrolls_frame"] == token {
                    ensure!(
                        self.stderr.metadata()?.len() == 0,
                        "backup_inspection_failed: inspect private native-sql.stderr"
                    );
                    return Ok(rows);
                }
                rows.extend(values);
            }
        })();
        if result.is_err() {
            self.failed = true;
            let _ = self.child.kill();
        }
        result
    }
    pub(super) fn finish(&mut self) -> Result<()> {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                self.failed = true;
                self.child.kill()?;
                self.child.wait()?;
                bail!("backup_inspection_timeout: verifier did not stop");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        ensure!(
            status.success() && !self.failed && self.stderr.metadata()?.len() == 0,
            "backup_inspection_failed: verifier exited unsuccessfully"
        );
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
