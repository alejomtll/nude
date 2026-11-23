//! Parser tolerante de la respuesta del auditor y ejecución del CLI con
//! binarios falsos (`/bin/sh` en lugar del shell de login).

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::db::models::FileDiff;

const CLEAN: &str = r#"{"summary":"Añade validación de entrada.","verdict":"ready","findings":[]}"#;

#[test]
fn parses_plain_json() {
    let parsed = parse_audit_response(CLEAN).expect("json");
    assert_eq!(parsed.summary, "Añade validación de entrada.");
    assert_eq!(parsed.verdict, AuditVerdict::Ready);
    assert!(parsed.findings.is_empty());
}

#[test]
fn parses_json_wrapped_in_markdown_and_prose() {
    let text = "Claro, aquí tienes la auditoría:\n\n```json\n{\n  \"summary\": \"Cambia el parser.\\nRiesgo medio.\",\n  \"verdict\": \"review\",\n  \"findings\": [\n    {\"severity\": \"medium\", \"file\": \"src/parse.rs\", \"message\": \"No hay test para {vacío}.\"}\n  ]\n}\n```\n\nEspero que te sirva.";
    let parsed = parse_audit_response(text).expect("fenced");
    assert_eq!(parsed.verdict, AuditVerdict::Review);
    assert_eq!(parsed.summary, "Cambia el parser.\nRiesgo medio.");
    assert_eq!(parsed.findings, vec![AuditFinding {
        severity: FindingSeverity::Medium,
        file: Some("src/parse.rs".into()),
        message: "No hay test para {vacío}.".into(),
    }]);
}

#[test]
fn parses_bare_object_with_prose_braces_and_trailing_commas() {
    // Una llave suelta antes, un esquema de ejemplo inválido y comas finales.
    let text = "Formato {\"verdict\": \"ready\" | \"review\"} pedido. Resultado {sin cerrar\n\
                {\"summary\": \"Ok.\", \"verdict\": \"ready\", \"findings\": [{\"severity\": \"low\", \"file\": null, \"message\": \"Detalle.\",},],}\n";
    let parsed = parse_audit_response(text).expect("bare");
    assert_eq!(parsed.verdict, AuditVerdict::Ready);
    assert_eq!(parsed.findings.len(), 1);
    assert_eq!(parsed.findings[0].file, None);
}

#[test]
fn unwraps_claude_json_output_envelope() {
    let inner = format!("```json\n{CLEAN}\n```");
    let envelope = serde_json::json!({ "type": "result", "subtype": "success", "result": inner }).to_string();
    assert_eq!(parse_audit_response(&envelope).expect("envelope").verdict, AuditVerdict::Ready);
}

#[test]
fn normalizes_severities_limits_findings_and_forces_review_on_high() {
    let text = r#"{"resumen":"Muchos cambios.","verdict":"LGTM","findings":[
        {"severity":"low","file":"a","message":"uno"},
        {"severity":"Critical","path":"b","description":"dos"},
        "tres sin estructura",
        {"severity":"baja","file":"c","message":"cuatro"},
        {"severity":"media","file":"d","message":"cinco"},
        {"severity":"high","file":"e","message":"seis"},
        {"severity":"low","file":"f"}
    ]}"#;
    let parsed = parse_audit_response(text).expect("json");
    assert_eq!(parsed.summary, "Muchos cambios.");
    assert_eq!(parsed.verdict, AuditVerdict::Review, "un hallazgo grave obliga a revisar");
    let messages: Vec<&str> = parsed.findings.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(messages, ["dos", "seis", "tres sin estructura", "cinco", "uno"]);
    assert_eq!(parsed.findings[0].file.as_deref(), Some("b"));
    assert_eq!(parsed.findings[2].severity, FindingSeverity::Medium);

    let unknown = parse_audit_response(r#"{"summary":"x","verdict":"quizás"}"#).expect("json");
    assert_eq!(unknown.verdict, AuditVerdict::Review);
    let empty = parse_audit_response(r#"{"verdict":"ready"}"#).expect("json");
    assert_eq!(empty.summary, "Sin observaciones.");
}

#[test]
fn rejects_answers_without_a_report() {
    assert!(parse_audit_response("No puedo ayudar con eso.").is_err());
    assert!(parse_audit_response(r#"{"severity":"low","message":"solo un hallazgo"}"#).is_err());
    let error = parse_audit_response("").expect_err("vacío");
    assert!(error.contains("JSON"), "{error}");
}

fn file(path: &str, change_type: ChangeType, patch: Option<&str>) -> FileDiff {
    FileDiff { file_path: path.into(), change_type, patch_text: patch.map(str::to_owned) }
}

#[test]
fn prompt_lists_files_sensitive_paths_and_truncates_big_diffs() {
    let huge = format!("@@ -1 +1 @@\n{}", "+x\n".repeat(MAX_PROMPT_DIFF));
    let diff = git::LaneDiff {
        base_branch: Some("main".into()),
        base_commit: "abc".into(),
        files: vec![
            file(".env", ChangeType::Added, Some("+TOKEN=1\n")),
            file("logo.png", ChangeType::Modified, None),
            file("src/big.rs", ChangeType::Modified, Some(&huge)),
            file("src/after.rs", ChangeType::Added, Some("+fn a() {}\n")),
        ],
        conflicts: vec!["src/big.rs".into()],
    };
    let prompt = build_prompt("Refactor", "nude/lane-refactor", &diff);
    assert_eq!(prompt.files, 4);
    assert!(prompt.truncated);
    assert!(prompt.text.contains("«Refactor» (rama nude/lane-refactor)"));
    assert!(prompt.text.contains("sobre main"));
    assert!(prompt.text.contains("Archivos sensibles detectados por Nude:\n- .env"));
    assert!(prompt.text.contains("conflicto al fusionar):\n- src/big.rs"));
    assert!(prompt.text.contains("A .env\nM logo.png\nM src/big.rs\nA src/after.rs"));
    assert!(prompt.text.contains("--- logo.png (binario o demasiado grande; sin parche)"));
    assert!(prompt.text.contains("+TOKEN=1"));
    assert!(prompt.text.contains("[… parche de src/big.rs recortado por tamaño]"));
    assert!(prompt.text.contains("JSON válido"));
    assert!(prompt.text.chars().count() < MAX_PROMPT_DIFF + 10_000);
}

/// Carpeta con binarios falsos y una de trabajo, borradas al terminar.
struct FakeBin {
    root: PathBuf,
}

impl FakeBin {
    fn new(tools: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("nude-audit-test-{}", new_id()));
        fs::create_dir_all(root.join("bin")).expect("mkdir");
        fs::create_dir_all(root.join("work")).expect("mkdir");
        fs::write(root.join("work/prompt.txt"), "PROMPT DE PRUEBA").expect("prompt");
        for (name, body) in tools {
            let path = root.join("bin").join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("tool");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        Self { root }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    /// `/bin/sh -c` con solo los binarios falsos y los del sistema en el PATH.
    fn command(&self) -> impl Fn(&str) -> Command + '_ {
        move |script: &str| {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script]);
            cmd.env("PATH", format!("{}:/usr/bin:/bin", self.root.join("bin").display()));
            cmd
        }
    }
}

impl Drop for FakeBin {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn runs_claude_print_mode_read_only_with_the_prompt_on_stdin() {
    let fake = FakeBin::new(&[(
        "claude",
        r#"cat > received.txt; printf '%s\n' "$@" > args.txt
printf 'Aquí está:\n```json\n{"summary":"Ok.","verdict":"ready","findings":[]}\n```\n'"#,
    )]);
    let output = run_cli(AuditorCli::Claude, &fake.work(), Duration::from_secs(10), &fake.command()).expect("run");
    assert_eq!(parse_audit_response(&output).expect("parse").verdict, AuditVerdict::Ready);
    assert_eq!(fs::read_to_string(fake.work().join("received.txt")).expect("stdin"), "PROMPT DE PRUEBA");
    let args = fs::read_to_string(fake.work().join("args.txt")).expect("args");
    assert!(args.starts_with("-p\n"), "{args}");
    assert!(args.contains("--permission-mode\nplan\n"), "{args}");
    assert!(args.contains("Edit,Write"), "{args}");
}

#[test]
fn codex_exec_is_read_only_and_its_last_message_wins_over_logs() {
    let fake = FakeBin::new(&[(
        "codex",
        r#"printf '%s\n' "$@" > args.txt; cat > /dev/null
echo "[codex] thinking… {not json"
printf '%s' '{"summary":"Desde codex.","verdict":"review","findings":[]}' > last.txt"#,
    )]);
    let output = run_cli(AuditorCli::Codex, &fake.work(), Duration::from_secs(10), &fake.command()).expect("run");
    assert_eq!(parse_audit_response(&output).expect("parse").summary, "Desde codex.");
    let args = fs::read_to_string(fake.work().join("args.txt")).expect("args");
    assert!(args.starts_with("exec\n--sandbox\nread-only\n"), "{args}");
}

#[test]
fn missing_clis_fall_back_and_explain_how_to_install() {
    // Auto con un carril de Codex: prueba codex (no está) y luego claude.
    let fake = FakeBin::new(&[("claude", &format!("cat > /dev/null; echo '{CLEAN}'"))]);
    let order = candidates(AuditorCli::Auto, Some("codex"));
    assert_eq!(order, [AuditorCli::Codex, AuditorCli::Claude]);
    let (auditor, parsed) = audit_with(&order, "p", Duration::from_secs(10), &fake.command()).expect("fallback");
    assert_eq!(auditor, AuditorCli::Claude);
    assert_eq!(parsed.verdict, AuditVerdict::Ready);

    let none = FakeBin::new(&[]);
    let error = audit_with(&candidates(AuditorCli::Auto, None), "p", Duration::from_secs(10), &none.command())
        .expect_err("ninguno");
    let message = error.to_string();
    assert!(message.contains("`claude` ni `codex`"), "{message}");
    assert!(message.contains("npm install -g @anthropic-ai/claude-code"), "{message}");
}

#[test]
fn slow_or_failing_clis_return_clean_errors() {
    let fake = FakeBin::new(&[
        ("claude", "cat > /dev/null; sleep 30 & sleep 30"),
        ("codex", "cat > /dev/null; echo 'Error: not logged in' >&2; exit 2"),
    ]);
    let started = Instant::now();
    let error = audit_with(&[AuditorCli::Claude], "p", Duration::from_millis(800), &fake.command()).expect_err("timeout");
    assert!(started.elapsed() < Duration::from_secs(5), "se cancela sin esperar al CLI");
    assert!(error.to_string().contains("tardó más de"), "{error}");

    let error = audit_with(&[AuditorCli::Codex], "p", Duration::from_secs(10), &fake.command()).expect_err("falla");
    let message = error.to_string();
    assert!(message.contains("código 2") && message.contains("not logged in"), "{message}");

    let garbage = FakeBin::new(&[("claude", "cat > /dev/null; echo 'No tengo nada que decir.'")]);
    let error = audit_with(&[AuditorCli::Claude], "p", Duration::from_secs(10), &garbage.command()).expect_err("sin json");
    assert!(error.to_string().starts_with("Claude Code: la respuesta no contenía"), "{error}");
}
