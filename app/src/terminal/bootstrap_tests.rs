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
    const FISH_SH: &str = include_str!("../../assets/bundled/bootstrap/fish.sh");
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
    let output = match std::process::Command::new("fish")
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
