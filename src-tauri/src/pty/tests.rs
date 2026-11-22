//! Ciclo de vida del gestor PTY con procesos reales: creación, escritura y
//! lectura, redimensionado, reenganche, fin natural y limpieza al cerrar.

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use super::*;

enum Seen {
    Out(PtyOutput),
    Exit(PtyExit),
}

fn manager() -> (PtyManager, Receiver<Seen>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let manager = PtyManager::new(move |event| {
        let seen = match event {
            PtyEvent::Output(out) => Seen::Out(out.clone()),
            PtyEvent::Exit(exit) => Seen::Exit(exit.clone()),
        };
        let _ = lock(&tx).send(seen);
    });
    (manager, rx)
}

fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(script);
    cmd
}

/// Acumula salida hasta que aparece `needle`; devuelve también el último `end`.
fn wait_output(rx: &Receiver<Seen>, text: &mut String, needle: &str) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut end = 0;
    while !text.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Seen::Out(out)) => {
                text.push_str(&out.data);
                assert_eq!(out.end, text.len() as u64, "offsets contiguos");
                end = out.end;
            }
            Ok(Seen::Exit(exit)) => panic!("el proceso terminó antes de mostrar «{needle}»: {exit:?}\n{text}"),
            Err(_) => panic!("no apareció «{needle}» en:\n{text}"),
        }
    }
    end
}

fn wait_exit(rx: &Receiver<Seen>) -> PtyExit {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Seen::Exit(exit)) => return exit,
            Ok(Seen::Out(_)) => continue,
            Err(_) => panic!("el proceso no terminó"),
        }
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: la señal 0 solo comprueba si el proceso existe.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn a_session_is_created_written_read_resized_reattached_and_killed() {
    let (pty, rx) = manager();
    let script = "stty size; read line; echo \"eco:$line\"; read x; stty size; read y";
    let first = pty.spawn_command("lane-1", LaneProgram::Shell, sh(script), 80, 24).expect("spawn");
    assert!(!first.reused);
    assert!(first.status.running);
    let pid = first.status.pid.expect("pid");

    let mut text = String::new();
    wait_output(&rx, &mut text, "24 80");
    pty.write("lane-1", "hola ñandú\n").expect("write");
    wait_output(&rx, &mut text, "eco:hola ñandú");

    pty.resize("lane-1", 120, 40).expect("resize");
    pty.write("lane-1", "\n").expect("write");
    let end = wait_output(&rx, &mut text, "40 120");

    // Reenganche: misma sesión, con el historial completo hasta `end`.
    let again = pty.spawn_command("lane-1", LaneProgram::Shell, sh("echo otra"), 100, 30).expect("reuse");
    assert!(again.reused);
    assert_eq!(again.status.session_id, first.status.session_id);
    assert_eq!(again.end, end);
    assert_eq!(again.scrollback, text);
    assert!(pty.status("lane-1").expect("status").running);

    // Limpieza: el proceso muere, se avisa y el carril queda sin sesión.
    assert!(pty.kill("lane-1"));
    let exit = wait_exit(&rx);
    assert_eq!(exit.session_id, first.status.session_id);
    assert!(pty.status("lane-1").is_none());
    assert!(pty.write("lane-1", "x").is_err());
    assert!(!pty.kill("lane-1"), "cerrar dos veces no hace nada");
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "el proceso sigue vivo tras cerrar el terminal");
}

#[test]
fn a_finished_program_keeps_its_output_until_a_new_session_replaces_it() {
    let (pty, rx) = manager();
    let first = pty.spawn_command("lane-2", LaneProgram::Codex, sh("echo fin del agente; exit 3"), 80, 24).expect("spawn");
    let exit = wait_exit(&rx);
    assert_eq!(exit.exit_code, Some(3));

    let status = pty.status("lane-2").expect("status");
    assert!(!status.running);
    assert_eq!(status.exit_code, Some(3));
    assert_eq!(status.program, LaneProgram::Codex);
    // Se puede leer lo que dejó (el lector puede ir unos ms por detrás).
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pty.attach("lane-2").expect("attach").scrollback.contains("fin del agente") {
        assert!(Instant::now() < deadline, "sin historial");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pty.resize("lane-2", 100, 30).is_ok(), "redimensionar uno terminado no falla");

    let second = pty.spawn_command("lane-2", LaneProgram::Shell, sh("read x"), 80, 24).expect("respawn");
    assert!(!second.reused);
    assert_ne!(second.status.session_id, first.status.session_id);
    assert!(second.scrollback.is_empty());
    pty.kill_all();
    assert!(pty.status("lane-2").is_none());
}

#[test]
fn processes_that_ignore_sighup_are_killed_after_the_grace_period() {
    let (pty, rx) = manager();
    let attach = pty
        .spawn_command("lane-3", LaneProgram::Shell, sh("trap '' HUP; echo listo; exec sleep 30"), 80, 24)
        .expect("spawn");
    let pid = attach.status.pid.expect("pid");
    let mut text = String::new();
    wait_output(&rx, &mut text, "listo");
    std::thread::sleep(Duration::from_millis(100));
    assert!(pty.kill("lane-3"));
    let started = Instant::now();
    wait_exit(&rx);
    assert!(started.elapsed() >= Duration::from_millis(1000), "ignoró SIGHUP: hizo falta SIGKILL");
    assert!(!alive(pid));
}

#[test]
fn missing_agents_explain_how_to_install_them_and_installed_ones_run() {
    let dir = std::env::temp_dir().join(format!("nude-pty-bin-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let run = |path: &str| {
        let script = agent_script(LaneProgram::ClaudeCode, Path::new("/usr/bin/true"));
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", path)
            .output()
            .expect("sh");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let missing = run("/usr/bin:/bin");
    assert!(missing.contains("Claude Code no está instalado"), "{missing}");
    assert!(missing.contains("npm install -g @anthropic-ai/claude-code"), "{missing}");
    assert!(missing.contains("«Claude»"), "{missing}");

    let fake = dir.join("claude");
    std::fs::write(&fake, "#!/bin/sh\necho claude falso en marcha\n").expect("fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let installed = run(&format!("{}:/usr/bin:/bin", dir.display()));
    assert!(installed.contains("claude falso en marcha"), "{installed}");
    assert!(!installed.contains("no está instalado"));

    assert!(agent_script(LaneProgram::Codex, Path::new("/opt/homebrew/bin/fish")).starts_with("if command -v codex >/dev/null 2>&1; codex; else;"));
    assert_eq!(agent_script(LaneProgram::Shell, Path::new("/bin/zsh")), "exec '/bin/zsh' -l -i");
    assert_eq!(LaneProgram::from_hint("claude-code"), Some(LaneProgram::ClaudeCode));
    assert_eq!(serde_json::from_str::<LaneProgram>("\"claude\"").expect("alias"), LaneProgram::ClaudeCode);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn split_utf8_sequences_and_long_histories_are_handled() {
    let mut output = Output::default();
    let bytes = "año ✓".as_bytes();
    let (a, b) = bytes.split_at(2); // parte la «ñ»
    assert_eq!(output.decode(a), "a");
    assert_eq!(output.decode(b), "ño ✓");
    assert_eq!(output.decode(&[0xff, b'x']), "\u{FFFD}x");

    for _ in 0..(MAX_SCROLLBACK / 10 + 100) {
        output.append("123456789\n");
    }
    assert!(output.text.len() <= MAX_SCROLLBACK);
    assert!(output.text.starts_with("123456789"), "se corta por líneas");
    assert_eq!(output.end as usize, (MAX_SCROLLBACK / 10 + 100) * 10);
}
