use std::{
    env,
    io::{self, IsTerminal, Write},
    path::Path,
    process::{Command, ExitCode},
};

const HELP: &str = "Roamlog build tasks

  cargo xtask                         Interactive menu (help when piped)
  cargo xtask build [--release]        Build desktop app
  cargo xtask run [--release] [-- …]    Run desktop app; -- separates app arguments
  cargo xtask test [FILTER]            Run workspace tests
  cargo xtask test --no-default-features  Run tracking core tests without UI
  cargo xtask check                   Check desktop app
  cargo xtask fmt [--check]           Format workspace, or check formatting
  cargo xtask verify                  Check formatting, core/UI tests, Clippy
  cargo xtask pre-commit              Format, then run all checks
  cargo xtask install-hooks           Enable repository Git pre-commit hook
  cargo xtask android [--release] [--demo]  Build signed ARM64 APK (default: debug)
  cargo xtask ios [--release] [--device]  Build unsigned Roamlog.app (default: simulator)

Build/run/test/check forward remaining arguments to Cargo.
Android needs ANDROID_HOME, ANDROID_NDK_HOME, JDK 17+, Python 3, Bash,
and Rust target aarch64-linux-android. See docs/building.md.
APK output: target/<debug|release>/apk/roamlog[-demo].apk.
iOS needs macOS with Xcode and Rust targets aarch64-apple-ios[-sim].
App output: target/aarch64-apple-ios[-sim]/<debug|release>/Roamlog.app.
No command installs apps or replaces trip data.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(task) = args.next() else {
        return if io::stdin().is_terminal() {
            menu()
        } else {
            println!("{HELP}");
            Ok(())
        };
    };
    dispatch(&task, &args.collect::<Vec<_>>())
}

fn dispatch(task: &str, args: &[String]) -> Result<(), String> {
    match task {
        "build" | "run" | "check" => cargo(&[task, "--locked", "--package", "roamlog"], args),
        "test" => cargo(&["test", "--locked", "--workspace"], args),
        "fmt" => cargo(&["fmt", "--all"], args),
        "verify" if args.is_empty() => {
            cargo(&["fmt", "--all", "--check"], &[])?;
            cargo(
                &[
                    "test",
                    "--locked",
                    "--package",
                    "roamlog",
                    "--no-default-features",
                ],
                &[],
            )?;
            cargo(&["test", "--locked", "--workspace"], &[])?;
            cargo(
                &[
                    "clippy",
                    "--locked",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
                &[],
            )
        }
        "pre-commit" if args.is_empty() => {
            cargo(&["fmt", "--all"], &[])?;
            dispatch("verify", &[])
        }
        "install-hooks" if args.is_empty() => {
            let mut command = Command::new("git");
            command.args(["config", "--local", "core.hooksPath", ".githooks"]);
            execute(command)
        }
        "android" => {
            let (release, demo) = android_options(args)?;
            let mut command = Command::new("bash");
            command
                .arg("android/package.sh")
                .arg(if release { "release" } else { "debug" });
            // Explicit --demo wins over ambient build flags, keeping main/demo predictable.
            command.env("ROAMLOG_DEMO", if demo { "1" } else { "0" });
            execute(command)
        }
        "ios" => {
            let (mut release, mut device) = (false, false);
            for arg in args {
                match arg.as_str() {
                    "--release" => release = true,
                    "--device" => device = true,
                    _ => {
                        return Err(format!(
                            "unsupported iOS argument: {arg}; use --release and/or --device"
                        ));
                    }
                }
            }
            let mut command = Command::new("bash");
            command
                .arg("ios/package.sh")
                .arg(if release { "release" } else { "debug" })
                .arg(if device { "device" } else { "sim" });
            execute(command)
        }
        "help" | "--help" | "-h" if args.is_empty() => {
            println!("{HELP}");
            Ok(())
        }
        _ => Err(format!(
            "unknown task or unsupported arguments: {task}. Run cargo xtask help"
        )),
    }
}

fn android_options(args: &[String]) -> Result<(bool, bool), String> {
    let (mut release, mut demo) = (false, false);
    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            "--demo" => demo = true,
            _ => {
                return Err(format!(
                    "unsupported Android argument: {arg}; use --release and/or --demo"
                ));
            }
        }
    }
    Ok((release, demo))
}

fn cargo(args: &[&str], extra: &[String]) -> Result<(), String> {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(args).args(extra);
    execute(command)
}

fn execute(mut command: Command) -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside workspace");
    command.current_dir(root);
    eprintln!("Running {command:?}");
    let status = command
        .status()
        .map_err(|error| format!("could not start command: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {status}"))
    }
}

fn menu() -> Result<(), String> {
    let choices = [
        ("Run desktop", "run", vec![]),
        ("Build desktop", "build", vec![]),
        ("Build desktop (release)", "build", vec!["--release".into()]),
        ("Run tests", "test", vec![]),
        ("Run all checks", "verify", vec![]),
        ("Format and check before commit", "pre-commit", vec![]),
        (
            "Build Android (release)",
            "android",
            vec!["--release".into()],
        ),
        (
            "Build Android demo (release)",
            "android",
            vec!["--release".into(), "--demo".into()],
        ),
    ];
    println!("Roamlog build tasks");
    for (i, (label, _, _)) in choices.iter().enumerate() {
        println!("  {}. {label}", i + 1);
    }
    print!("Choose task (Enter to exit): ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    if input.trim().is_empty() {
        return Ok(());
    }
    let index = input
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .filter(|&n| n < choices.len())
        .ok_or("Invalid menu choice")?;
    dispatch(choices[index].1, &choices[index].2)
}
