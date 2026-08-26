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

/// Bash's `(major, minor)` version, or `None` if bash isn't installed at all -- mirroring
/// `run_bash`'s "shell missing" skip convention for callers that also need to skip on an
/// installed-but-too-old bash. The minor version matters here, not just the major: see
/// `bash_supports_bind_dash_capital_x` below.
fn bash_version() -> Option<(u32, u32)> {
    let output = command::blocking::Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "echo \"${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}\"",
        ])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut fields = stdout.split_whitespace();
    let major = fields.next()?.parse().ok()?;
    let minor = fields.next()?.parse().ok()?;
    Some((major, minor))
}

/// `bind -X` (list `-x` bindings), which this detection depends on entirely, was added in bash
/// 4.3 (NEWS-4.3 item q) -- on anything older it errors out silently here (stderr is redirected
/// away), leaving detection permanently empty. That's a real, accepted limitation of the feature
/// itself on those versions (notably macOS's system bash 3.2; see the PR's "Known limitations"),
/// not something a test workaround should paper over: on such a bash, both of the tests below
/// would either fail (the "tags" case) or pass vacuously without exercising the absent-function
/// branch at all (the "declines" case just happens to expect the same empty result `bind -X`'s
/// absence always produces). Skip both rather than let the latter masquerade as real coverage.
fn bash_supports_bind_dash_capital_x() -> Option<bool> {
    Some(bash_version()? >= (4, 3))
}

/// Regression test for the ctrl-t equivalent of bash's `declare -F __atuin_history` guard on the
/// ctrl-r path: detection must decline (no tag, no interception) when the picker function
/// `warp_run_external_ctrl_t_widget` calls -- `__fzf_select__` -- isn't actually defined, even
/// though `bind -X` reports the wrapper name ("fzf-file-widget") that detection matches against.
/// Without this guard, an fzf version that renamed its picker function would have ctrl-t tagged
/// and intercepted with nothing to invoke, swallowing the key instead of leaving it alone.
#[test]
fn test_bash_ctrl_t_detection_declines_when_picker_function_is_absent() {
    if bash_supports_bind_dash_capital_x() == Some(false) {
        return;
    }
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
    if bash_supports_bind_dash_capital_x() == Some(false) {
        return;
    }
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

fn fish_warp_escape_json_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "function warp_escape_json\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish warp_escape_json function start should exist");
    let end_marker = "\nend\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish warp_escape_json function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

/// Regression test for `set result (commandline | string collect)` above: without `string
/// collect`, a multi-line selection makes that `set`'s own command substitution split it into a
/// list by newline, and the real `warp_escape_json` (used here instead of the plain-join stub the
/// other tests in this section use, since the defect is specifically in how it escapes -- or
/// fails to escape -- what it's given) then quotes that list back down to a single argument by
/// joining with a space instead of preserving the newline as JSON's `\n` escape.
#[test]
fn test_fish_ctrl_r_widget_reports_multiline_selection_with_embedded_newline() {
    let runner = fish_ctrl_r_widget_runner_fn();
    let escape_json = fish_warp_escape_json_fn();
    let script = format!(
        r#"
{escape_json}
function warp_send_json_message
  echo "$argv"
end
set -g _test_commandline_value ''
function commandline
  echo "$_test_commandline_value"
end
function fzf-history-widget
  set -g _test_commandline_value (printf 'echo one\necho two' | string collect)
end
set -g _WARP_EXTERNAL_CTRL_R_WIDGET fzf-history-widget
set -g WARP_SESSION_ID 12345
{runner}
warp_run_external_ctrl_r_widget test-token
"#
    );
    let Some(stdout) = run_fish(&script) else {
        return;
    };
    assert!(
        stdout.contains(r#""buffer": "echo one\necho two""#),
        "{stdout}"
    );
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

/// Writes a NUL-delimited draft file matching [`Input::write_ctrl_t_draft_file`]'s format
/// (`{char_cursor}\0{draft}\0`) directly, rather than going through the Rust writer, so these
/// fish-side decode tests exercise exactly the bytes fish reads without depending on the writer
/// under a separate set of tests.
fn write_decode_test_draft_file(char_cursor: u32, draft: &str) -> std::path::PathBuf {
    let draft_file =
        std::env::temp_dir().join(format!("warp-ctrl-t-decode-test-{}", uuid::Uuid::new_v4()));
    std::fs::write(&draft_file, format!("{char_cursor}\0{draft}\0"))
        .expect("should write test draft file");
    draft_file
}

/// Regression test for the fish decode path (`warp_run_external_ctrl_t_widget` reading back the
/// draft file), not just the Rust file writer: a multiline in-progress command must survive
/// reconstruction intact. An unquoted command substitution splits `cat`'s output into a list by
/// newline, so reconstructing that split without care (see the comment on `warp_ctrl_t_widget`'s
/// reconstruction line) can silently drop embedded newlines back out -- exercising only the write
/// side can never catch that, since the bug is entirely in how fish re-reads what was written.
#[test]
fn test_fish_ctrl_t_draft_decode_preserves_multiline_drafts() {
    let decode_snippet = fish_ctrl_t_draft_decode_snippet();
    let draft_file = write_decode_test_draft_file(8, "echo one\necho two");
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

/// Regression test for a draft whose *last* character is a newline (e.g. a trailing blank line
/// mid-multiline edit): a plain command substitution -- `(command cat -- $draft_file)` --
/// unconditionally strips trailing newline bytes from what it captures before any splitting
/// happens, which is exactly why the decode format is NUL-delimited (see
/// `Input::write_ctrl_t_draft_file`) rather than reconstructed from a newline-split list. The
/// multiline test above only covers an *embedded* newline, which that stripping doesn't touch --
/// this is the case it would silently corrupt.
#[test]
fn test_fish_ctrl_t_draft_decode_preserves_trailing_newline() {
    let decode_snippet = fish_ctrl_t_draft_decode_snippet();
    let draft_file = write_decode_test_draft_file(3, "echo hi\n");
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
    assert!(stdout.contains("char_cursor=[3]"), "{stdout}");
    assert!(stdout.contains("original_line=[echo hi\n]"), "{stdout}");
}

fn fish_ctrl_t_widget_runner_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "function warp_run_external_ctrl_t_widget\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t widget runner function start should exist");
    let end_marker = "\nend\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t widget runner function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

fn fish_ctrl_t_draft_file_path_fn() -> &'static str {
    const FISH_SH: &str = include_str!("../../../app/assets/bundled/bootstrap/fish.sh");
    let start_marker = "function warp_ctrl_t_draft_file_path\n";
    let start = FISH_SH
        .find(start_marker)
        .expect("fish ctrl-t draft file path function start should exist");
    let end_marker = "\nend\n";
    let end = FISH_SH[start..]
        .find(end_marker)
        .expect("fish ctrl-t draft file path function end should exist");
    &FISH_SH[start..start + end + end_marker.len()]
}

/// Builds a script that runs the full `warp_run_external_ctrl_t_widget` (not just the
/// `warp_ctrl_t_widget_result` comparison helper in isolation) against a real draft file, so the
/// `(commandline | string collect)` argument at its `fzf-file-widget` call site is exercised too
/// -- unquoted, a multi-line result there would otherwise expand to multiple arguments, silently
/// truncating that comparison to the result's first line alone. `commandline` is stubbed
/// statefully (supporting the `-r --` and `-C --` forms the widget actually calls, plus a plain
/// read) rather than as a fixed value, since the widget both seeds and reads it back.
fn fish_ctrl_t_widget_test_script(xdg_runtime_dir: &str, widget_body: &str) -> String {
    let runner = fish_ctrl_t_widget_runner_fn();
    let draft_file_path_fn = fish_ctrl_t_draft_file_path_fn();
    let widget_result_fn = fish_ctrl_t_widget_result_fn();
    format!(
        r#"
# Unlike the real warp_escape_json (see fish_warp_escape_json_fn above), this stub doesn't
# actually escape a real newline into JSON's `\n` -- piped through `string collect` purely so
# that leaving one in doesn't itself get re-split by the `set` below that captures this
# function's own output, which would otherwise mask the very truncation these tests exist to
# catch behind an unrelated space-joining artifact of the stub.
function warp_escape_json
  string join \n $argv | string collect
end
function warp_send_json_message
  echo "$argv"
end
set -gx XDG_RUNTIME_DIR '{xdg_runtime_dir}'
{draft_file_path_fn}
{widget_result_fn}
set -g _test_cl_value ''
function commandline
  if test (count $argv) -ge 1; and test "$argv[1]" = '-r'
    set -g _test_cl_value (string join \n -- $argv[3..] | string collect)
    return 0
  end
  if test (count $argv) -ge 1; and test "$argv[1]" = '-C'
    return 0
  end
  echo "$_test_cl_value"
end
function fzf-file-widget
  {widget_body}
end
set -g _WARP_EXTERNAL_CTRL_T_WIDGET fzf-file-widget
set -g WARP_SESSION_ID 12345
{runner}
warp_run_external_ctrl_t_widget test-token
"#
    )
}

/// Writes a NUL-delimited draft file matching [`Input::write_ctrl_t_draft_file`]'s format (see
/// `write_decode_test_draft_file` above) at the path the widget under test will look for.
fn write_ctrl_t_test_draft(xdg_runtime_dir: &std::path::Path, char_cursor: u32, draft: &str) {
    std::fs::create_dir_all(xdg_runtime_dir).expect("should create test XDG_RUNTIME_DIR");
    std::fs::write(
        xdg_runtime_dir.join("warp-ctrl-t-test-token"),
        format!("{char_cursor}\0{draft}\0"),
    )
    .expect("should write test draft file");
}

/// Regression test for the `(commandline | string collect)` argument at the widget's
/// `fzf-file-widget` call site: without `string collect`, a multi-line selection is split by that
/// call's own (unquoted) command substitution into multiple arguments, silently truncating
/// `warp_ctrl_t_widget_result`'s second argument -- and therefore the reported buffer -- to the
/// selection's first line alone.
#[test]
fn test_fish_ctrl_t_widget_reports_full_multiline_change_without_truncation() {
    let xdg_runtime_dir =
        std::env::temp_dir().join(format!("warp-ctrl-t-widget-test-{}", uuid::Uuid::new_v4()));
    write_ctrl_t_test_draft(&xdg_runtime_dir, 10, "echo START\nMIDDLE");
    let script = fish_ctrl_t_widget_test_script(
        &xdg_runtime_dir.display().to_string(),
        "commandline -r -- (printf 'echo START\\nMIDDLE nested.rs ' | string collect)",
    );
    let stdout = run_fish(&script);
    std::fs::remove_dir_all(&xdg_runtime_dir).ok();
    let Some(stdout) = stdout else {
        return;
    };
    assert!(
        stdout.contains("\"buffer\": \"echo START\nMIDDLE nested.rs \""),
        "{stdout}"
    );
}

/// Companion to the test above, for the failure mode the same truncation causes on cancel: a
/// multi-line draft left unchanged gets word-split at the same call site, so
/// `warp_ctrl_t_widget_result` compares the full original line against only its own first line,
/// finds them unequal, and reports that stale first line as if it were a real selection instead
/// of the empty buffer this "unchanged" case is supposed to produce.
#[test]
fn test_fish_ctrl_t_widget_reports_empty_when_multiline_draft_is_left_unchanged() {
    let xdg_runtime_dir =
        std::env::temp_dir().join(format!("warp-ctrl-t-widget-test-{}", uuid::Uuid::new_v4()));
    write_ctrl_t_test_draft(&xdg_runtime_dir, 10, "echo START\nMIDDLE");
    let script =
        fish_ctrl_t_widget_test_script(&xdg_runtime_dir.display().to_string(), "# cancelled");
    let stdout = run_fish(&script);
    std::fs::remove_dir_all(&xdg_runtime_dir).ok();
    let Some(stdout) = stdout else {
        return;
    };
    assert!(stdout.contains(r#""buffer": """#), "{stdout}");
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
