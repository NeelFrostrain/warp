use super::*;

struct TestAssetProvider;

impl AssetProvider for TestAssetProvider {
    fn get(&self, path: &str) -> anyhow::Result<Cow<'_, [u8]>> {
        let content = match path {
            "bundled/bootstrap/bash.sh" => "#include hello_world",
            "bundled/bootstrap/fish.sh" => "# this is a comment\nthis_is_a_command",
            "bundled/bootstrap/zsh.sh" => {
                "asdf\n#include whitespace\n    prepended whitespace\n\n\n"
            }
            "bundled/bootstrap/pwsh.ps1" => {
                r#"# This is a comment
                Write-Output 'Testing some output'
                function test1 {
                    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingInvokeExpression', '', Justification = 'We actually need it')]
                    param([string]$command)
                    Invoke-Expression $command
                }"#
            }
            "hello_world" => "hello world!",
            "whitespace" => "no whitespace\n\n\n yes whitespace!",
            _ => anyhow::bail!("path not found in assets"),
        };
        Ok(Cow::Borrowed(content.as_bytes()))
    }
}

#[test]
fn test_include_directive() {
    assert_eq!(
        decode_script(&script_for_shell(ShellType::Bash, &TestAssetProvider)),
        "hello world!\n"
    );
}

#[test]
fn test_trims_comments() {
    assert_eq!(
        decode_script(&script_for_shell(ShellType::Fish, &TestAssetProvider)),
        "this_is_a_command\n"
    );
}

#[test]
fn test_trims_whitespace() {
    assert_eq!(
        decode_script(&script_for_shell(ShellType::Zsh, &TestAssetProvider)),
        "asdf\nno whitespace\n yes whitespace!\n prepended whitespace\n"
    );
}

#[test]
fn test_trims_powershell_specifics() {
    assert_eq!(
        decode_script(&script_for_shell(ShellType::PowerShell, &TestAssetProvider)),
        " Write-Output 'Testing some output'\n function test1 {\n param([string]$command)\n Invoke-Expression $command\n }\n"
    );
}

fn decode_script(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("should not fail to decode")
}

fn fish_history_wrapper_installer() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "if functions -q fish_should_add_to_history\n  and not functions fish_should_add_to_history";
    let end_marker = "  warp_original_fish_should_add_to_history $argv\nend";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish history wrapper installer start should exist");
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish history wrapper installer end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

fn run_fish(script: &str) -> Option<String> {
    let output = match command::blocking::Command::new("fish")
        .args(["--no-config", "-c", script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("failed to run fish: {error}"),
    };
    assert!(
        output.status.success(),
        "fish exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn test_fish_history_wrapper_accepts_normal_commands_across_resourcing() {
    let installer = fish_history_wrapper_installer();
    let script = format!(
        r#"
{installer}
{installer}
fish_should_add_to_history "echo normal"
echo "normal:$status"
fish_should_add_to_history "warp_run_external_ctrl_r_widget token"
echo "helper:$status"
# The real invocation (see trigger_external_ctrl_r_history_search) is prefixed with a leading
# space, so atuin's own "ignorespace" exclusion also catches it; the wrapper must still reject
# this exact shape too.
fish_should_add_to_history " warp_run_external_ctrl_r_widget token"
echo "helper_leading_space:$status"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("normal:0"), "{stdout}");
    assert!(stdout.contains("helper:1"), "{stdout}");
    assert!(stdout.contains("helper_leading_space:1"), "{stdout}");
}

#[test]
fn test_fish_history_wrapper_preserves_user_hook_across_resourcing() {
    let installer = fish_history_wrapper_installer();
    let script = format!(
        r#"
function fish_should_add_to_history
  string match --quiet -- "user_excluded*" $argv[1]; and return 1
  return 0
end
{installer}
{installer}
fish_should_add_to_history "echo normal"
echo "normal:$status"
fish_should_add_to_history "warp_run_external_ctrl_r_widget token"
echo "helper:$status"
fish_should_add_to_history "user_excluded"
echo "user:$status"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("normal:0"), "{stdout}");
    assert!(stdout.contains("helper:1"), "{stdout}");
    assert!(stdout.contains("user:1"), "{stdout}");
}

/// Regression test for a user/plugin hook defined *between* two sourcings of this bootstrap
/// script (e.g. a plugin loaded after Warp's shell integration, followed by a shell reload or
/// nested fish subshell): the second sourcing must capture that hook rather than discarding it
/// in favor of whatever backup (or accept-everything default) an earlier sourcing installed.
#[test]
fn test_fish_history_wrapper_captures_hook_installed_between_resourcing() {
    let installer = fish_history_wrapper_installer();
    let script = format!(
        r#"
{installer}
function fish_should_add_to_history
  string match --quiet -- "user_excluded*" $argv[1]; and return 1
  return 0
end
{installer}
fish_should_add_to_history "echo normal"
echo "normal:$status"
fish_should_add_to_history "warp_run_external_ctrl_r_widget token"
echo "helper:$status"
fish_should_add_to_history "user_excluded"
echo "user:$status"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("normal:0"), "{stdout}");
    assert!(stdout.contains("helper:1"), "{stdout}");
    assert!(stdout.contains("user:1"), "{stdout}");
}

fn bash_ctrl_t_detection_snippet() -> &'static str {
    const BASH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/bash_body.sh");
    let start_marker = "      _WARP_EXTERNAL_CTRL_T_WIDGET=\"\"\n      warp_ctrl_t_binding=";
    let end_marker = "          fi\n          ;;\n      esac";
    let start = BASH_SH
        .find(start_marker)
        .expect("bash ctrl-t detection snippet start should exist");
    let end = BASH_SH[start..]
        .find(end_marker)
        .expect("bash ctrl-t detection snippet end should exist");
    &BASH_SH[start..start + end + end_marker.len()]
}

fn run_bash(script: &str) -> Option<String> {
    let output = match command::blocking::Command::new("bash")
        .args(["--noprofile", "--norc", "-c", script])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("failed to run bash: {error}"),
    };
    assert!(
        output.status.success(),
        "bash exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Regression test for the ctrl-t equivalent of bash's `declare -F __atuin_history` guard on the
/// ctrl-r path: detection must decline (no tag, no interception) when the picker function
/// `warp_run_external_ctrl_t_widget` calls -- `__fzf_select__` -- isn't actually defined, even
/// though `bind -X` reports the wrapper name ("fzf-file-widget") that detection matches against.
/// Without this guard, an fzf version that renamed its picker function would have ctrl-t tagged
/// and intercepted with nothing to invoke, swallowing the key instead of leaving it alone.
#[test]
fn test_bash_ctrl_t_detection_declines_when_picker_function_is_absent() {
    let detection = bash_ctrl_t_detection_snippet();
    let script = format!(
        r#"
WARP_IN_MSYS2=false
shell_plugins=()
bind -x '"\C-t": fzf-file-widget'
{detection}
printf 'widget=[%s] plugins=[%s]\n' "$_WARP_EXTERNAL_CTRL_T_WIDGET" "${{shell_plugins[*]}}"
"#
    );
    let Some(stdout) = run_bash(&script) else {
        return;
    };
    assert!(stdout.contains("widget=[]"), "{stdout}");
    assert!(!stdout.contains("external_ctrl_t_file"), "{stdout}");
}

#[test]
fn test_bash_ctrl_t_detection_tags_when_picker_function_is_present() {
    let detection = bash_ctrl_t_detection_snippet();
    let script = format!(
        r#"
WARP_IN_MSYS2=false
shell_plugins=()
bind -x '"\C-t": fzf-file-widget'
__fzf_select__() {{ :; }}
{detection}
printf 'widget=[%s] plugins=[%s]\n' "$_WARP_EXTERNAL_CTRL_T_WIDGET" "${{shell_plugins[*]}}"
"#
    );
    let Some(stdout) = run_bash(&script) else {
        return;
    };
    assert!(stdout.contains("widget=[fzf-file-widget]"), "{stdout}");
    assert!(stdout.contains("external_ctrl_t_file"), "{stdout}");
}

fn fish_ctrl_r_widget_runner_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "function warp_run_external_ctrl_r_widget\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-r widget runner function start should exist");
    let end_marker = "\nend\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-r widget runner function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

/// Regression test for `warp_run_external_ctrl_r_widget`'s fzf case: it used to hand-build
/// `FZF_DEFAULT_OPTS` with flags (`--wrap-sign`, `--highlight-line`, `--accept-nth`,
/// `--with-shell`) and call a helper function (`__fzf_defaults`) that don't exist on every fzf
/// shell integration -- confirmed to fail outright with "Unknown command: __fzf_defaults" against
/// a real, still-commonly-packaged fzf 0.44.1 install, with the picker that did appear (fzf
/// falling through to a plain invocation once that command failed) reading raw, unformatted
/// history text as its input. It now delegates entirely to the user's own `fzf-history-widget`
/// instead, so this stubs that widget and the interactive-only `commandline` builtin they both
/// call, to verify the wrapper reports whatever the widget leaves on the commandline without
/// depending on any fzf-version-specific option or helper function existing at all -- the kind of
/// test that would have caught the original defect, rather than merely asserting one flag absent.
fn fish_ctrl_r_widget_test_script(runner: &str, widget_body: &str) -> String {
    format!(
        r#"
function warp_escape_json
  string join \n $argv
end
function warp_send_json_message
  echo "$argv"
end
set -g _test_commandline_value ''
function commandline
  echo "$_test_commandline_value"
end
function fzf-history-widget
  {widget_body}
end
set -g _WARP_EXTERNAL_CTRL_R_WIDGET fzf-history-widget
set -g WARP_SESSION_ID 12345
{runner}
warp_run_external_ctrl_r_widget test-token
"#
    )
}

#[test]
fn test_fish_ctrl_r_widget_reports_fzf_history_widget_selection() {
    let runner = fish_ctrl_r_widget_runner_fn();
    let script = fish_ctrl_r_widget_test_script(
        runner,
        "set -g _test_commandline_value 'echo selected_from_widget'",
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(
        stdout.contains(r#""buffer": "echo selected_from_widget""#),
        "{stdout}"
    );
}

/// `fzf-history-widget` only calls `commandline` on a successful selection, leaving it untouched
/// on cancel -- the wrapper must report that untouched (here: still-empty) state as an empty
/// buffer, matching the existing "nothing selected" convention shared with the plain-path bash/
/// zsh widgets.
#[test]
fn test_fish_ctrl_r_widget_reports_empty_buffer_when_widget_leaves_commandline_untouched() {
    let runner = fish_ctrl_r_widget_runner_fn();
    let script = fish_ctrl_r_widget_test_script(runner, "# cancelled: commandline left as-is");
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains(r#""buffer": """#), "{stdout}");
}

fn fish_ctrl_t_widget_query_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "function warp_external_ctrl_t_widget\n  set -l widget \"\"\n  for binding in (bind \\ct 2>/dev/null)";
    let end_marker = "  test -n \"$widget\"; or return 1\n  echo \"$widget\"\nend";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t widget query function start should exist");
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t widget query function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

fn fish_ctrl_t_detection_snippet() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "set -g _WARP_EXTERNAL_CTRL_T_WIDGET \"\"\n  set -l warp_ctrl_t_widget (warp_external_ctrl_t_widget)\n  switch \"$warp_ctrl_t_widget\"";
    let end_marker = "        set -a shell_plugins external_ctrl_t_file\n      end\n  end";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t detection snippet start should exist");
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t detection snippet end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

fn fish_ctrl_t_widget_result_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    // Locates the function boundary structurally (start of the `function` line to its matching
    // `end` line) rather than by matching the literal body text, so a behavioral mutation to the
    // comparison inside it changes what the test observes instead of breaking extraction itself.
    let start_marker = "function warp_ctrl_t_widget_result\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t widget result function start should exist");
    let end_marker = "\nend\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t widget result function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

#[test]
fn test_fish_ctrl_t_widget_result_is_empty_when_widget_leaves_draft_unchanged() {
    let result_fn = fish_ctrl_t_widget_result_fn();
    let script = format!(
        r#"
{result_fn}
set result (warp_ctrl_t_widget_result 'echo START MIDDLE' 'echo START MIDDLE')
printf 'result=[%s]\n' "$result"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("result=[]"), "{stdout}");
}

#[test]
fn test_fish_ctrl_t_widget_result_preserves_changed_line() {
    let result_fn = fish_ctrl_t_widget_result_fn();
    let script = format!(
        r#"
{result_fn}
set result (warp_ctrl_t_widget_result 'echo START MIDDLE' 'echo START nested.rs MIDDLE')
printf 'result=[%s]\n' "$result"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(
        stdout.contains("result=[echo START nested.rs MIDDLE]"),
        "{stdout}"
    );
}

fn fish_ctrl_t_draft_decode_snippet() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    // Structural, not literal-text, boundaries (see `fish_ctrl_t_widget_result_fn` above) so a
    // behavioral change to the reconstruction logic changes what the test observes.
    let start_marker = "if test -f \"$draft_file\"\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t draft decode snippet start should exist");
    let end_marker = "\n      end\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t draft decode snippet end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

/// Regression test for the fish decode path (`warp_run_external_ctrl_t_widget` reading back the
/// draft file), not just the Rust file writer: a multiline in-progress command must survive
/// reconstruction intact. fish's command substitution splits `cat`'s output into a list by
/// newline, so rejoining it without `string collect` (see the comment on `warp_ctrl_t_widget`'s
/// reconstruction line) silently drops the embedded newline back out -- exercising only the write
/// side can never catch that, since the bug is entirely in how fish re-reads what was written.
#[test]
fn test_fish_ctrl_t_draft_decode_preserves_multiline_drafts() {
    let decode_snippet = fish_ctrl_t_draft_decode_snippet();
    let draft_file =
        std::env::temp_dir().join(format!("warp-ctrl-t-decode-test-{}", uuid::Uuid::new_v4()));
    std::fs::write(&draft_file, "8\necho one\necho two").expect("should write test draft file");
    let draft_file_path = draft_file.display().to_string();
    let script = format!(
        r#"
set -l draft_file '{draft_file_path}'
set -l char_cursor 0
set -l original_line ''
{decode_snippet}
printf 'char_cursor=[%s]\n' "$char_cursor"
printf 'original_line=[%s]\n' "$original_line"
"#
    );
    let stdout = run_fish(&script);
    std::fs::remove_file(&draft_file).ok();
    let Some(stdout) = stdout else {
        return;
    };
    assert!(stdout.contains("char_cursor=[8]"), "{stdout}");
    assert!(
        stdout.contains("original_line=[echo one\necho two]"),
        "{stdout}"
    );
}

/// Regression test for the fish equivalent of bash's picker-function guard: detection must
/// decline (no tag, no interception) when `fzf-file-widget` -- the function
/// `warp_run_external_ctrl_t_widget` now calls directly -- isn't actually defined, even though
/// `bind` reports it as ctrl-t's binding. Without this guard, a rebind to a nonexistent or
/// renamed function would have ctrl-t tagged and intercepted with nothing to invoke, swallowing
/// the key instead of leaving it alone.
#[test]
fn test_fish_ctrl_t_detection_declines_when_picker_function_is_absent() {
    let query_fn = fish_ctrl_t_widget_query_fn();
    let detection = fish_ctrl_t_detection_snippet();
    let script = format!(
        r#"
{query_fn}
bind \ct fzf-file-widget
set -l shell_plugins
{detection}
printf 'widget=[%s] plugins=[%s]\n' "$_WARP_EXTERNAL_CTRL_T_WIDGET" "$shell_plugins"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("widget=[]"), "{stdout}");
    assert!(!stdout.contains("external_ctrl_t_file"), "{stdout}");
}

#[test]
fn test_fish_ctrl_t_detection_tags_when_picker_function_is_present() {
    let query_fn = fish_ctrl_t_widget_query_fn();
    let detection = fish_ctrl_t_detection_snippet();
    let script = format!(
        r#"
{query_fn}
function fzf-file-widget
end
bind \ct fzf-file-widget
set -l shell_plugins
{detection}
printf 'widget=[%s] plugins=[%s]\n' "$_WARP_EXTERNAL_CTRL_T_WIDGET" "$shell_plugins"
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(stdout.contains("widget=[fzf-file-widget]"), "{stdout}");
    assert!(stdout.contains("external_ctrl_t_file"), "{stdout}");
}
