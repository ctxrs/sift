#![cfg(any(unix, windows))]
// Exercise the runner directly while main's CLI integration evolves independently.
#[path = "../src/runner.rs"]
mod runner;

fn record_observation(observation: runner::Observation<'_>) {
    let Some(path) = std::env::var_os("RETOK_TEST_OBSERVATION") else {
        return;
    };
    let stream = |stream: runner::StreamObservation<'_>| {
        serde_json::json!({
            "original": stream.original,
            "compacted": stream.compacted,
            "read_bytes": stream.read_bytes,
            "emitted_bytes": stream.emitted_bytes,
        })
    };
    let value = serde_json::json!({
        "stdout": stream(observation.stdout), "stderr": stream(observation.stderr),
        "duration_ns": observation.duration.as_nanos() as u64, "status": observation.status,
    });
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[cfg(unix)]
mod unix {
    use super::runner;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    const MARKER: &[u8] = b"RUNNER-START\n";

    #[test]
    fn runner_entry() {
        let Ok(args) = std::env::var("RETOK_TEST_ARGV") else {
            return;
        };
        let args: Vec<String> = serde_json::from_str(&args).unwrap();
        std::io::stdout().write_all(MARKER).unwrap();
        std::io::stdout().flush().unwrap();
        let result = runner::run_observed(
            &args.into_iter().map(Into::into).collect::<Vec<_>>(),
            std::env::var_os("RETOK_TEST_RAW").is_some(),
            super::record_observation,
        );
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
                {
                    std::process::exit(0);
                }
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    fn command(args: &[&str], raw: bool) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "unix::runner_entry", "--nocapture"])
            .env("RETOK_TEST_ARGV", serde_json::to_string(args).unwrap())
            .env_remove("RETOK_TEST_RAW")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if raw {
            command.env("RETOK_TEST_RAW", "1");
        }
        command
    }
    fn clean(mut output: Output) -> Output {
        let index = output
            .stdout
            .windows(MARKER.len())
            .position(|w| w == MARKER)
            .unwrap();
        output.stdout.drain(..index + MARKER.len());
        output
    }
    fn output(args: &[&str], raw: bool) -> Output {
        clean(command(args, raw).output().unwrap())
    }
    fn finish(child: &mut Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("runner did not finish");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn start_lines(child: &mut Child) -> BufReader<std::process::ChildStdout> {
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line.as_bytes() == MARKER {
                return reader;
            }
        }
    }

    #[test]
    fn argv_streams_environment_directory_input_and_exit() {
        let cwd = std::env::temp_dir();
        let mut child = command(&["python3", "-c", "import os,sys; print(sys.argv[1]); print(os.environ['RETOK_SYNTHETIC']); print(os.getcwd()); sys.stdout.flush(); sys.stdout.buffer.write(sys.stdin.buffer.read()); sys.stderr.write('separate error\\n'); sys.exit(23)", "$(echo no); * ' spaced"], false)
        .env("RETOK_SYNTHETIC", "inherited").current_dir(&cwd).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"input\0bytes")
            .unwrap();
        let result = clean(child.wait_with_output().unwrap());
        assert_eq!(result.status.code(), Some(23));
        assert_eq!(
            result.stdout,
            format!(
                "$(echo no); * ' spaced\ninherited\n{}\ninput\0bytes",
                cwd.display()
            )
            .as_bytes()
        );
        assert_eq!(result.stderr, b"separate error\n");
    }

    #[test]
    fn missing_permission_empty_and_signal_exit() {
        assert!(runner::run(&[], false).is_err());
        assert_eq!(
            output(&["/retok-synthetic-no-such-program"], false)
                .status
                .code(),
            Some(127)
        );
        assert_eq!(output(&["/"], false).status.code(), Some(126));
        assert_eq!(
            output(&["sh", "-c", "kill -TERM $$"], false).status.code(),
            Some(143)
        );
        assert!(output(&["true"], false).stdout.is_empty());
    }

    #[test]
    fn bounded_complete_outputs_compact_separately_and_raw_is_exact() {
        let line = "synthetic repeated complete diagnostic message\n";
        let args = [
            "python3",
            "-c",
            "import sys; sys.stdout.write('synthetic repeated complete diagnostic message\\n'*300); sys.stderr.write('synthetic repeated complete diagnostic message\\n'*300)",
        ];
        let result = output(&args, false);
        assert!(result.status.success());
        for stream in [&result.stdout, &result.stderr] {
            assert!(stream.len() < line.len() * 300);
            assert_eq!(
                retok::restore(
                    retok::Encoding::TextRunsV1,
                    std::str::from_utf8(stream).unwrap()
                )
                .unwrap(),
                line.repeat(300)
            );
        }
        let raw = output(&args, true);
        assert_eq!(raw.stdout, line.repeat(300).as_bytes());
        assert_eq!(raw.stderr, raw.stdout);
    }

    #[test]
    fn binary_and_large_concurrent_streams_are_exact() {
        let result = output(
            &[
                "python3",
                "-c",
                "import os,threading; b=bytes(range(256))*40000; t=threading.Thread(target=lambda:os.write(2,b)); t.start(); os.write(1,b); t.join()",
            ],
            false,
        );
        let expected: Vec<u8> = (0..256).map(|i| i as u8).collect::<Vec<_>>().repeat(40000);
        assert!(result.status.success());
        assert_eq!(result.stdout, expected);
        assert_eq!(result.stderr, expected);
        let result = output(
            &[
                "python3",
                "-c",
                "import os; os.write(1,b'\\xff'*600); os.write(2,b'\\x80'*600)",
            ],
            false,
        );
        assert_eq!(result.stdout, vec![255; 600]);
        assert_eq!(result.stderr, vec![128; 600]);
    }

    #[test]
    fn pending_prompt_flushes_before_child_can_finish() {
        let mut child = command(&["python3", "-c", "import sys; print('prompt',flush=True); sys.stdin.readline(); print('done',flush=True)"], false).spawn().unwrap();
        let mut reader = start_lines(&mut child);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            tx.send(line).unwrap();
            let mut rest = String::new();
            reader.read_to_string(&mut rest).unwrap();
            rest
        });
        let result = rx.recv_timeout(Duration::from_secs(3));
        if result.is_err() {
            let _ = child.kill();
        }
        assert_eq!(result.unwrap(), "prompt\n");
        assert!(child.try_wait().unwrap().is_none());
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        assert!(finish(&mut child).success());
        assert_eq!(handle.join().unwrap(), "done\n");
    }

    #[test]
    fn terminal_stdin_preserves_output_without_compaction() {
        use std::os::fd::FromRawFd;
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let result = clean(
            command(
                &[
                    "python3",
                    "-c",
                    "import sys; assert sys.stdin.isatty(); print('same diagnostic '*40)",
                ],
                false,
            )
            .stdin(slave)
            .output()
            .unwrap(),
        );
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            format!("{}\n", "same diagnostic ".repeat(40)).as_bytes()
        );
    }

    #[test]
    fn signals_forward_and_closed_consumer_cleans_up_child() {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, 0] {
            let mut child = command(
                &[
                    "python3",
                    "-c",
                    "import os,time; print(os.getpid(),flush=True); time.sleep(60)",
                ],
                false,
            )
            .spawn()
            .unwrap();
            let mut reader = start_lines(&mut child);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let pid: i32 = line.trim().parse().unwrap();
            if signal == 0 {
                drop(reader);
            } else {
                assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
            }
            let status = finish(&mut child);
            if signal != 0 {
                assert_eq!(status.code(), Some(128 + signal));
            }
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child still exists");
        }
    }

    #[test]
    fn size_limit_forces_raw_even_for_valid_compressible_text() {
        let result = output(
            &[
                "python3",
                "-c",
                "import os; b=b'repeated diagnostic\\n'*500000; os.write(1,b); os.write(2,b'last stderr\\n')",
            ],
            false,
        );
        assert!(result.status.success());
        assert_eq!(result.stdout, b"repeated diagnostic\n".repeat(500000));
        assert_eq!(result.stderr, b"last stderr\n");
    }

    #[test]
    fn cancellation_escalates_when_child_ignores_signal() {
        let mut child = command(&["python3", "-c", "import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print(os.getpid(),flush=True); time.sleep(60)"], false).spawn().unwrap();
        let mut reader = start_lines(&mut child);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let pid: i32 = line.trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        assert_eq!(finish(&mut child).code(), Some(143));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn exited_child_does_not_disable_pipe_deadline_or_descendant_cleanup() {
        // The direct child exits immediately; its child retains both pipes.
        let mut child = command(&["python3", "-c", "import os,time; pid=os.fork();\nif pid: os._exit(0)\nprint(os.getpid(),flush=True); time.sleep(60)"], false).spawn().unwrap();
        let mut reader = start_lines(&mut child);
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let _ = tx.send(line);
            reader
        });
        let line = rx.recv_timeout(Duration::from_secs(3));
        if line.is_err() {
            let _ = child.kill();
        }
        let _pid: i32 = line.unwrap().trim().parse().unwrap();
        drop(handle.join().unwrap());
        assert!(finish(&mut child).success());
        // An orphan may briefly remain a zombie until the system reaper collects it.
        #[cfg(target_os = "linux")]
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match std::fs::read_to_string(format!("/proc/{_pid}/stat")) {
                    Err(_) => break,
                    Ok(stat) if stat.split_whitespace().nth(2) == Some("Z") => break,
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => panic!("descendant still running"),
                }
            }
        }
    }

    #[test]
    fn cancellation_survives_output_backpressure() {
        let mut child = command(
            &[
                "python3",
                "-c",
                "import os; print(os.getpid(),flush=True); b=b'x'*65536\nwhile True: os.write(1,b)",
            ],
            false,
        )
        .spawn()
        .unwrap();
        let mut reader = start_lines(&mut child);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let pid: i32 = line.trim().parse().unwrap();
        // Keep the consumer open but deliberately stop draining it.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        assert_eq!(finish(&mut child).code(), Some(143));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        drop(reader);
    }
    fn observed(args: &[&str], raw: bool) -> (Output, serde_json::Value) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "retok-runner-observation-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let output = clean(
            command(args, raw)
                .env("RETOK_TEST_OBSERVATION", &path)
                .output()
                .unwrap(),
        );
        let event = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_file(path).unwrap();
        (output, event)
    }

    #[test]
    fn observer_reports_only_measured_complete_streams_and_exact_bytes() {
        let text = "synthetic complete repeated diagnostic message\n".repeat(300);
        let (output, event) = observed(
            &[
                "python3",
                "-c",
                "import sys; sys.stdout.write('synthetic complete repeated diagnostic message\\n'*300); sys.stderr.write('bad\\n'); sys.exit(23)",
            ],
            false,
        );
        assert_eq!(event["status"], 23);
        assert!(event["duration_ns"].as_u64().unwrap() > 0);
        assert_eq!(event["stdout"]["read_bytes"], text.len());
        assert_eq!(event["stdout"]["emitted_bytes"], output.stdout.len());
        assert_eq!(event["stderr"]["read_bytes"], 4);
        assert_eq!(event["stderr"]["emitted_bytes"], 4);
        assert_eq!(
            event["stdout"]["original"],
            serde_json::json!(text.as_bytes())
        );
        assert_eq!(event["stderr"]["original"], serde_json::json!(b"bad\n"));
        assert!(event["stderr"]["compacted"].is_null());
        let tokenizer = tiktoken_rs::o200k_base().unwrap();
        assert_eq!(
            event["stdout"]["compacted"]["input_tokens"],
            tokenizer.encode_ordinary(&text).len()
        );
        assert_eq!(
            event["stdout"]["compacted"]["output_tokens"],
            tokenizer
                .encode_ordinary(std::str::from_utf8(&output.stdout).unwrap())
                .len()
        );
    }

    #[test]
    fn observer_keeps_binary_streaming_and_inherited_tokens_unmeasured() {
        for (script, raw, size, captured) in [
            ("import os; os.write(1,b'\\xff'*600)", false, 600, true),
            (
                "import os; os.write(1,b'0123456789'*1000000)",
                false,
                10_000_000,
                false,
            ),
            ("print('small')", true, 6, false),
        ] {
            let (output, event) = observed(&["python3", "-c", script], raw);
            assert!(output.status.success());
            assert_eq!(output.stdout.len(), size);
            assert!(event["stdout"]["compacted"].is_null());
            assert_eq!(!event["stdout"]["original"].is_null(), captured);
            if raw {
                assert!(event["stdout"]["read_bytes"].is_null());
                assert!(event["stdout"]["emitted_bytes"].is_null());
            } else {
                assert_eq!(event["stdout"]["read_bytes"], size);
                assert_eq!(event["stdout"]["emitted_bytes"], size);
            }
        }
        let (_, event) = observed(&["/retok-synthetic-missing-program"], false);
        assert_eq!(event["status"], 127);
        assert!(event["stdout"]["read_bytes"].is_null());
        assert!(event["stdout"]["compacted"].is_null());
    }
}

#[cfg(windows)]
mod windows {
    use super::runner;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetStdHandle, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    };

    #[test]
    fn runner_entry() {
        let Ok(args) = std::env::var("RETOK_TEST_ARGV") else {
            return;
        };
        let args: Vec<String> = serde_json::from_str(&args).unwrap();
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let result = runner::run_observed(
            &args,
            std::env::var_os("RETOK_TEST_RAW").is_some(),
            super::record_observation,
        );
        match result {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
                {
                    std::process::exit(0);
                }
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    #[test]
    fn child_entry() {
        let Ok(mode) = std::env::var("RETOK_TEST_CHILD") else {
            return;
        };
        if mode == "detached" {
            // Windows Command inherits other inheritable handles as well as the
            // selected stdio. Null leaf stdio alone would still leak these pipe
            // handles into it, so the captured launcher could not observe EOF.
            for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: these borrowed handles remain open for this helper;
                // only inheritance is disabled before deliberately detaching.
                assert_ne!(
                    unsafe { SetHandleInformation(GetStdHandle(stream), HANDLE_FLAG_INHERIT, 0) },
                    0
                );
            }
            let mut leaf = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows::child_entry", "--nocapture"])
                .env("RETOK_TEST_CHILD", "leaf")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("CHILD {}", leaf.id());
            std::io::stdout().flush().unwrap();
            std::thread::spawn(move || {
                let _ = leaf.wait();
            });
            std::process::exit(0);
        }
        if mode.starts_with("tree") {
            let mut leaf = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows::child_entry", "--nocapture"])
                .env("RETOK_TEST_CHILD", "leaf")
                .spawn()
                .unwrap();
            println!("LEAF {}", leaf.id());
            std::thread::spawn(move || {
                let _ = leaf.wait();
            });
        }
        println!("CHILD {}", std::process::id());
        std::io::stdout().flush().unwrap();
        if mode == "tree-writing" {
            let bytes = b"payload\n".repeat(2048);
            loop {
                std::io::stdout().write_all(&bytes).unwrap();
            }
        }
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    }
    fn command(mode: &str) -> Command {
        let exe = std::env::current_exe().unwrap();
        let args = [
            exe.to_str().unwrap(),
            "--exact",
            "windows::child_entry",
            "--nocapture",
        ];
        let mut command = Command::new(&exe);
        command
            .args(["--exact", "windows::runner_entry", "--nocapture"])
            .env("RETOK_TEST_ARGV", serde_json::to_string(&args).unwrap())
            .env("RETOK_TEST_CHILD", mode)
            .env_remove("RETOK_TEST_RAW")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    fn handles(
        child: &mut Child,
        count: usize,
    ) -> (BufReader<std::process::ChildStdout>, Vec<OwnedHandle>) {
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut handles = Vec::new();
            let mut line = String::new();
            while handles.len() < count {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if let Some(pid) = line.strip_prefix("CHILD ") {
                    let pid = pid.trim().parse().unwrap();
                    let handle =
                        unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
                    assert!(!handle.is_null());
                    handles.push(unsafe { OwnedHandle::from_raw_handle(handle) });
                }
            }
            let _ = tx.send((reader, handles));
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child readiness: {error}");
            }
        }
    }
    fn finish(child: &mut Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("runner did not finish");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn assert_exited(handles: Vec<OwnedHandle>) {
        for handle in handles {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                WAIT_OBJECT_0,
                "orphaned child"
            );
        }
    }
    #[test]
    fn successful_raw_and_captured_launchers_preserve_detached_children() {
        struct Background(OwnedHandle);
        impl Drop for Background {
            fn drop(&mut self) {
                // Always clean up the synthetic background process, even if an
                // assertion or wrapper timeout unwinds this test.
                unsafe {
                    TerminateProcess(self.0.as_raw_handle(), 0);
                    WaitForSingleObject(self.0.as_raw_handle(), 5000);
                }
            }
        }
        for raw in [true, false] {
            let mut command = command("detached");
            if raw {
                command.env("RETOK_TEST_RAW", "1");
            }
            let mut child = command.spawn().unwrap();
            let (_reader, mut processes) = handles(&mut child, 1);
            let background = Background(processes.remove(0));
            eprintln!("waiting for successful launcher (raw={raw})");
            assert!(finish(&mut child).success());
            assert_eq!(
                unsafe { WaitForSingleObject(background.0.as_raw_handle(), 100) },
                WAIT_TIMEOUT,
                "successful launcher killed its detached child (raw={raw})"
            );
            assert_ne!(
                unsafe { TerminateProcess(background.0.as_raw_handle(), 0) },
                0
            );
            assert_eq!(
                unsafe { WaitForSingleObject(background.0.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
        }
    }

    #[test]
    fn job_closes_on_forced_runner_termination() {
        let mut child = command("tree").spawn().unwrap();
        let (_reader, processes) = handles(&mut child, 2);
        child.kill().unwrap();
        finish(&mut child);
        assert_exited(processes);
    }
    #[test]
    fn closed_consumer_on_next_write_kills_child_tree() {
        let mut child = command("tree-writing").spawn().unwrap();
        let (reader, processes) = handles(&mut child, 2);
        drop(reader);
        assert!(finish(&mut child).success());
        assert_exited(processes);
    }
    #[test]
    #[ignore = "native Windows console required: run with --ignored --test-threads=1"]
    fn console_break_preserves_cancellation_status_and_cleans_tree() {
        let mut child = command("tree")
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .unwrap();
        let (_reader, processes) = handles(&mut child, 2);
        assert_ne!(
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
            0
        );
        assert_eq!(finish(&mut child).code(), Some(130));
        assert_exited(processes);
    }
    #[test]
    #[ignore = "native Windows console required: run with --ignored --test-threads=1"]
    fn console_break_cancels_blocked_output_and_cleans_tree() {
        let mut child = command("tree-writing")
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .unwrap();
        let (_reader, processes) = handles(&mut child, 2);
        std::thread::sleep(Duration::from_millis(100));
        assert_ne!(
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) },
            0
        );
        assert_eq!(finish(&mut child).code(), Some(130));
        assert_exited(processes);
    }
}
