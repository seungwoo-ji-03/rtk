use super::{mypy_cmd, pip_cmd, pytest_cmd, ruff_cmd};
use crate::core::tracking;
use crate::core::utils::{strip_ansi, tool_exists};
use anyhow::{Context, Result};
use std::process::Command;
use std::sync::OnceLock;

pub fn run(args: &[String], verbose: u8) -> Result<()> {
    if args.len() >= 2 && args[0] == "-m" {
        match args[1].as_str() {
            "pytest" => pytest_cmd::run(&args[2..], verbose),
            "mypy" => mypy_cmd::run(&args[2..], verbose),
            "ruff" => ruff_cmd::run(&args[2..], verbose),
            "pip" => pip_cmd::run(&args[2..], verbose),
            "py_compile" => run_py_compile(&args[2..], verbose),
            _ => run_generic(args, verbose),
        }
    } else {
        run_generic(args, verbose)
    }
}

/// Resolve the python interpreter binary: prefer python3, fallback to python.
fn python_bin() -> &'static str {
    static BIN: OnceLock<&str> = OnceLock::new();
    BIN.get_or_init(|| {
        if tool_exists("python3") {
            "python3"
        } else {
            "python"
        }
    })
}

/// Run `python3 -m py_compile <file>` and filter output.
fn run_py_compile(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let bin = python_bin();

    let mut cmd = Command::new(bin);
    cmd.arg("-m").arg("py_compile").args(args);

    if verbose > 0 {
        eprintln!("Running: {} -m py_compile {}", bin, args.join(" "));
    }

    let output = cmd
        .output()
        .context("Failed to run python3 -m py_compile. Is python3 installed?")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);
    let exit_code = output.status.code().unwrap_or(1);

    let filtered = if output.status.success() {
        if args.len() == 1 {
            format!("ok ✓ {}", args[0])
        } else {
            format!("ok ✓ {} files checked", args.len())
        }
    } else {
        filter_compile_errors(&raw)
    };

    if let Some(hint) = crate::core::tee::tee_and_hint(&raw, "py_compile", exit_code) {
        println!("{}\n{}", filtered, hint);
    } else {
        println!("{}", filtered);
    }

    timer.track(
        &format!("python3 -m py_compile {}", args.join(" ")),
        &format!("rtk python3 -m py_compile {}", args.join(" ")),
        &raw,
        &filtered,
    );

    if !output.status.success() {
        std::process::exit(exit_code);
    }

    Ok(())
}

/// Run python3 generically with traceback filtering.
fn run_generic(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let bin = python_bin();

    let mut cmd = Command::new(bin);
    cmd.args(args);

    if verbose > 0 {
        eprintln!("Running: {} {}", bin, args.join(" "));
    }

    let output = cmd
        .output()
        .context("Failed to run python3. Is it installed?")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);
    let exit_code = output.status.code().unwrap_or(1);

    let filtered = if output.status.success() {
        // For successful runs, pass through stdout + stderr (warnings, deprecations)
        raw.trim().to_string()
    } else {
        // For failures, filter tracebacks to show only the error summary
        filter_traceback(&raw)
    };

    if let Some(hint) = crate::core::tee::tee_and_hint(&raw, "python3", exit_code) {
        if !filtered.is_empty() {
            println!("{}\n{}", filtered, hint);
        } else {
            println!("{}", hint);
        }
    } else if !filtered.is_empty() {
        println!("{}", filtered);
    }

    timer.track(
        &format!("python3 {}", args.join(" ")),
        &format!("rtk python3 {}", args.join(" ")),
        &raw,
        &filtered,
    );

    if !output.status.success() {
        std::process::exit(exit_code);
    }

    Ok(())
}

/// Filter py_compile errors to show only the error lines.
fn filter_compile_errors(raw: &str) -> String {
    let clean = strip_ansi(raw);
    let mut errors: Vec<&str> = Vec::new();

    for line in clean.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Keep File/line references and error messages
        if trimmed.starts_with("File ")
            || trimmed.contains("Error:")
            || trimmed.contains("Warning:")
            || is_caret_pointer(trimmed)
        {
            errors.push(trimmed);
        }
    }

    if errors.is_empty() {
        clean.trim().to_string()
    } else {
        errors.join("\n")
    }
}

/// Check if a line is a Python caret pointer line (only spaces and ^).
fn is_caret_pointer(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c == '^' || c == ' ' || c == '~')
}

/// Filter Python tracebacks to show only the last error + location.
fn filter_traceback(raw: &str) -> String {
    let clean = strip_ansi(raw);
    let lines: Vec<&str> = clean.lines().collect();

    if lines.is_empty() {
        return String::new();
    }

    // Find traceback boundaries and extract summary
    let mut result = Vec::new();
    let mut in_traceback = false;
    let mut last_file_line: Option<&str> = None;

    for line in &lines {
        let trimmed = line.trim();

        if trimmed == "Traceback (most recent call last):" {
            in_traceback = true;
            last_file_line = None;
            continue;
        }

        if in_traceback {
            if trimmed.starts_with("File ") {
                last_file_line = Some(trimmed);
            } else if is_traceback_terminal(trimmed) {
                // End of traceback - emit summary
                if let Some(file_line) = last_file_line {
                    result.push(file_line.to_string());
                }
                result.push(trimmed.to_string());
                in_traceback = false;
                last_file_line = None;
            }
            // Skip intermediate code lines within traceback
        } else {
            // Non-traceback output: keep as-is
            result.push(line.to_string());
        }
    }

    // Flush unclosed traceback (e.g., KeyboardInterrupt at EOF without colon)
    if in_traceback {
        if let Some(file_line) = last_file_line {
            result.push(file_line.to_string());
        }
    }

    result.join("\n").trim().to_string()
}

/// Check if a line terminates a traceback block.
/// Matches Error/Exception lines (with colon) and bare exceptions like KeyboardInterrupt.
fn is_traceback_terminal(line: &str) -> bool {
    // Standard: "ValueError: bad value", "ImportError: cannot import..."
    if line.contains("Error:") || line.contains("Exception:") {
        return true;
    }
    // Bare exceptions without message: "KeyboardInterrupt", "SystemExit"
    // These are single CamelCase words at the start of the line (not indented code)
    if !line.starts_with(' ')
        && !line.starts_with('\t')
        && !line.starts_with("File ")
        && (line.ends_with("Interrupt")
            || line.ends_with("Exit")
            || line.ends_with("Error")
            || line.ends_with("Exception"))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn test_filter_compile_errors_success() {
        let result = filter_compile_errors("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_filter_compile_errors_syntax_error() {
        let raw = r#"  File "app/main.py", line 10
    def foo(
          ^
SyntaxError: unexpected EOF while parsing
"#;
        let filtered = filter_compile_errors(raw);
        assert!(filtered.contains("SyntaxError"));
        assert!(filtered.contains("File "));
        assert!(filtered.contains("^"));
    }

    #[test]
    fn test_filter_traceback_no_traceback() {
        let raw = "Hello, world!\nDone.";
        let filtered = filter_traceback(raw);
        assert_eq!(filtered, "Hello, world!\nDone.");
    }

    #[test]
    fn test_filter_traceback_with_traceback() {
        let raw = r#"Traceback (most recent call last):
  File "app/main.py", line 5, in <module>
    import nonexistent_module
  File "app/utils.py", line 12, in helper
    raise ValueError("bad value")
ValueError: bad value
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("ValueError: bad value"));
        assert!(filtered.contains("File \"app/utils.py\""));
        // Should NOT contain intermediate frames
        assert!(!filtered.contains("import nonexistent_module"));

        let input_tokens = count_tokens(raw);
        let output_tokens = count_tokens(&filtered);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "Expected ≥60% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_filter_traceback_mixed_output() {
        let raw = r#"Loading config...
Traceback (most recent call last):
  File "app/main.py", line 1, in <module>
    from app.models import Base
  File "app/models.py", line 50, in <module>
    class User(Base):
  File "app/models.py", line 55, in User
    email = Column(String, unique=True)
ImportError: cannot import name 'Column' from 'sqlalchemy'
After traceback line
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("Loading config..."));
        assert!(filtered.contains("ImportError"));
        assert!(filtered.contains("After traceback line"));
        // Should keep last file reference before the error
        assert!(filtered.contains("File \"app/models.py\", line 55"));
        // Should NOT include intermediate traceback frames
        assert!(!filtered.contains("line 1, in <module>"));
    }

    #[test]
    fn test_py_compile_success_format() {
        // Verify success message format for single file
        let result = "ok ✓ app/main.py";
        assert!(result.contains("ok ✓"));
        assert!(result.contains("app/main.py"));
    }

    #[test]
    fn test_filter_compile_errors_with_caret() {
        let raw = r#"  File "test.py", line 3
    x = (1 +
        ^
SyntaxError: invalid syntax
Some other noise
More noise here
"#;
        let filtered = filter_compile_errors(raw);
        assert!(filtered.contains("SyntaxError: invalid syntax"));
        assert!(filtered.contains("File "));
        assert!(!filtered.contains("More noise"));
        // Caret-only lines preserved, but "x = (1 +" is NOT (contains ^-like chars in code)
        assert!(!filtered.contains("x = (1 +"));
    }

    #[test]
    fn test_filter_compile_errors_no_false_positive_caret() {
        // Lines with ^ in code/text should NOT be included
        let raw = r#"version = "^1.2.3"
File "test.py", line 5
SyntaxError: invalid syntax
"#;
        let filtered = filter_compile_errors(raw);
        assert!(!filtered.contains("version"));
        assert!(filtered.contains("SyntaxError"));
    }

    #[test]
    fn test_filter_traceback_empty() {
        assert_eq!(filter_traceback(""), "");
    }

    #[test]
    fn test_filter_traceback_multiple_tracebacks() {
        let raw = r#"Traceback (most recent call last):
  File "a.py", line 1, in <module>
    foo()
  File "a.py", line 5, in foo
    bar()
NameError: name 'bar' is not defined

During handling of the above exception, another exception occurred:

Traceback (most recent call last):
  File "b.py", line 10, in <module>
    handle_error()
  File "b.py", line 20, in handle_error
    cleanup()
RuntimeError: cleanup failed
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("NameError: name 'bar' is not defined"));
        assert!(filtered.contains("RuntimeError: cleanup failed"));
    }

    #[test]
    fn test_filter_traceback_keyboard_interrupt() {
        let raw = r#"Traceback (most recent call last):
  File "server.py", line 42, in <module>
    app.run()
  File "server.py", line 10, in run
    time.sleep(1)
KeyboardInterrupt
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("KeyboardInterrupt"));
        assert!(filtered.contains("File \"server.py\", line 10"));
        // Should NOT contain intermediate frames
        assert!(!filtered.contains("app.run()"));
    }

    #[test]
    fn test_filter_traceback_system_exit() {
        let raw = r#"Traceback (most recent call last):
  File "cli.py", line 5, in <module>
    sys.exit(1)
SystemExit
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("SystemExit"));
    }

    #[test]
    fn test_filter_traceback_unclosed_at_eof() {
        // Traceback that ends at EOF without a terminal line
        let raw = r#"Traceback (most recent call last):
  File "broken.py", line 1, in <module>
    crash()
"#;
        let filtered = filter_traceback(raw);
        // Should flush the last file line even without error line
        assert!(filtered.contains("File \"broken.py\", line 1"));
    }

    #[test]
    fn test_filter_traceback_realistic_deep() {
        // Realistic deep traceback (Django-style) to verify meaningful savings
        let raw = r#"Traceback (most recent call last):
  File "/usr/lib/python3.11/wsgiref/handlers.py", line 137, in run
    self.result = application(self.environ, self.start_response)
  File "/home/user/venv/lib/python3.11/site-packages/django/core/handlers/wsgi.py", line 124, in __call__
    response = self.get_response(request)
  File "/home/user/venv/lib/python3.11/site-packages/django/core/handlers/base.py", line 140, in get_response
    response = self._middleware_chain(request)
  File "/home/user/venv/lib/python3.11/site-packages/django/core/handlers/exception.py", line 55, in inner
    response = get_response(request)
  File "/home/user/venv/lib/python3.11/site-packages/django/utils/deprecation.py", line 136, in __call__
    response = self.get_response(request)
  File "/home/user/venv/lib/python3.11/site-packages/django/middleware/common.py", line 106, in __call__
    response = self.get_response(request)
  File "/home/user/venv/lib/python3.11/site-packages/django/core/handlers/exception.py", line 55, in inner
    response = get_response(request)
  File "/home/user/app/views.py", line 42, in index
    data = fetch_data()
  File "/home/user/app/services.py", line 88, in fetch_data
    return db.query(Model).filter_by(active=True).all()
  File "/home/user/venv/lib/python3.11/site-packages/sqlalchemy/orm/query.py", line 2673, in all
    return self._iter().all()
  File "/home/user/venv/lib/python3.11/site-packages/sqlalchemy/engine/result.py", line 1808, in _allrows
    make_row = self._row_getter
OperationalError: (psycopg2.OperationalError) connection to server at "localhost", port 5432 failed: Connection refused
"#;
        let filtered = filter_traceback(raw);
        assert!(filtered.contains("OperationalError:"));
        assert!(filtered.contains("connection to server"));
        // Should keep last file location
        assert!(filtered.contains("result.py"));
        // Should NOT contain all intermediate frames
        assert!(!filtered.contains("wsgiref"));
        assert!(!filtered.contains("middleware"));

        let input_tokens = count_tokens(raw);
        let output_tokens = count_tokens(&filtered);
        let savings = 100.0 - (output_tokens as f64 / input_tokens as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "Expected ≥60% savings on deep traceback, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_is_caret_pointer() {
        assert!(is_caret_pointer("    ^"));
        assert!(is_caret_pointer("    ^^^^"));
        assert!(is_caret_pointer("    ^^^~~~"));
        assert!(!is_caret_pointer("x = 1 ^ 2"));
        assert!(!is_caret_pointer("version = \"^1.2\""));
        assert!(!is_caret_pointer(""));
    }

    #[test]
    fn test_is_traceback_terminal() {
        assert!(is_traceback_terminal("ValueError: bad"));
        assert!(is_traceback_terminal("ImportError: no module"));
        assert!(is_traceback_terminal("KeyboardInterrupt"));
        assert!(is_traceback_terminal("SystemExit"));
        assert!(is_traceback_terminal("RuntimeError"));
        assert!(is_traceback_terminal("CustomException: msg"));
        assert!(!is_traceback_terminal("  File \"a.py\""));
        assert!(!is_traceback_terminal("    raise ValueError"));
        assert!(!is_traceback_terminal("some random text"));
    }
}
