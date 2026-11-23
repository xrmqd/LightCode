use super::register_uri_scheme;
use anyhow::{Context as _, Result};
use gpui::{AppContext as _, AsyncApp, Context, PromptLevel, Window, actions};
use release_channel::ReleaseChannel;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use util::ResultExt;
use workspace::notifications::{DetachAndPromptErr, NotificationId};
use workspace::{Toast, Workspace};

actions!(
    cli,
    [
        /// Installs the LightCode CLI tool to the system PATH.
        InstallCliBinary,
    ]
);

async fn install_script(cx: &AsyncApp) -> Result<PathBuf> {
    let cli_path = cx.update(|cx| cx.path_for_auxiliary_executable("cli"))??;
    let link_path = Path::new("/usr/local/bin/lcode");
    let bin_dir_path = link_path.parent().unwrap();

    let current_link = match smol::fs::symlink_metadata(link_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = smol::fs::read_link(link_path).await?;
            if target == cli_path {
                return Ok(link_path.into());
            }
            anyhow::ensure!(
                target.ends_with("LightCode.app/Contents/MacOS/cli")
                    || target.ends_with("LightCode Dev.app/Contents/MacOS/cli"),
                "{} points to another application",
                link_path.display()
            );
            Some(target)
        }
        Ok(_) => anyhow::bail!("{} already exists and is not a LightCode link", link_path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    if current_link.is_none() && smol::fs::unix::symlink(&cli_path, link_path).await.is_ok() {
        return Ok(link_path.into());
    }

    // Pass the paths as arguments and quote them in AppleScript. Recheck the
    // existing link before changing it after the password prompt.
    let status = smol::process::Command::new("/usr/bin/osascript")
        .args(["-e", r#"
on run argv
    set binDir to quoted form of item 1 of argv
    set sourcePath to quoted form of item 2 of argv
    set targetPath to quoted form of item 3 of argv
    set oldPath to quoted form of item 4 of argv
    do shell script ("if [ -L " & targetPath & " ]; then [ \"$(/usr/bin/readlink " & targetPath & ")\" = " & oldPath & " ] || exit 1; elif [ -e " & targetPath & " ]; then exit 1; fi; /bin/mkdir -p " & binDir & " && /bin/ln -sfn " & sourcePath & " " & targetPath) with administrator privileges
end run
"#])
        .arg(bin_dir_path)
        .arg(&cli_path)
        .arg(link_path)
        .arg(current_link.unwrap_or_default())
    .stdout(smol::process::Stdio::inherit())
        .stderr(smol::process::Stdio::inherit())
        .output()
        .await?
        .status;
    anyhow::ensure!(status.success(), "error running osascript");
    Ok(link_path.into())
}

pub fn install_cli_binary(window: &mut Window, cx: &mut Context<Workspace>) {
    const LINUX_PROMPT_DETAIL: &str = "If you installed LightCode from a release tarball, add ~/.local/bin to your PATH.\n\nIf you installed LightCode from a different source like your package manager, then you may need to create an alias/symlink manually.\n\nCheck that the lcode command is available on your PATH.";

    cx.spawn_in(window, async move |workspace, cx| {
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            let prompt = cx.prompt(
                PromptLevel::Warning,
                "CLI should already be installed",
                Some(LINUX_PROMPT_DETAIL),
                &["Ok"],
            );
            cx.background_spawn(prompt).detach();
            return Ok(());
        }
        let path = install_script(cx.deref()).await.context("error creating CLI symlink")?;

        workspace.update_in(cx, |workspace, _, cx| {
            struct InstalledLcodeCli;

            workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<InstalledLcodeCli>(),
                    format!(
                        "Installed `lcode` to {}. You can launch {} from your terminal.",
                        path.to_string_lossy(),
                        ReleaseChannel::global(cx).display_name()
                    ),
                ),
                cx,
            )
        })?;
        register_uri_scheme(cx).await.log_err();
        Ok(())
    })
    .detach_and_prompt_err("Error installing lcode", window, cx, |_, _, _| None);
}
