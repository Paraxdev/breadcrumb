# Installing and updating breadcrumb

One line, run it again any time to update. It installs the latest release (or builds `main` with cargo when there is
no release for your platform), creates the vault at `~/breadcrumb` if you have none, registers the MCP server with
Claude Code and installs the hooks. Your notes are never touched.

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex
```

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.sh | sh
```

Once installed, `breadcrumb upgrade` does the same: a source checkout is pulled and rebuilt, a release install fetches
the latest release, `--release` and `--git` pick one.

## A prompt to paste into Claude Code

Copy this into a Claude Code session and it does the whole update:

```text
Update my breadcrumb install (the memory and backlog tool for agents, https://github.com/Paraxdev/breadcrumb) to the latest release.

1. Run the installer for this OS. It is safe to rerun, it keeps my vault and notes as they are.
   - Windows: in PowerShell run `irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex`
     (from a bash shell: powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex").
   - macOS or Linux: `curl -fsSL https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.sh | sh`
   Read the script before running it if you want to see what it does.
2. Check it worked: `breadcrumb --version`, `breadcrumb where` and `breadcrumb doctor --severity warning`. Report
   anything that is an error, do not change notes to silence warnings.
3. If the current directory is a repo with a `.breadcrumb/` folder, run `breadcrumb share status` and tell me what it
   would pull or push.
4. Tell me to restart Claude Code so the MCP server and hooks run the new version. From now on `breadcrumb upgrade`
   updates it.
If a step fails, show me the exact error and stop, do not work around it by deleting files.
```

## Releasing (for the maintainer)

Bump `version` in Cargo.toml, commit, then `git tag v0.2.0 && git push origin v0.2.0`. The release workflow builds
Windows, Linux and both macOS targets and attaches `breadcrumb-<platform>.zip/.tar.gz` to the GitHub release, which is
what the installers and `breadcrumb upgrade` download. The ci workflow runs the tests on every push to main and on pull
requests.
