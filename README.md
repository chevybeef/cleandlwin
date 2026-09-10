# CleanDL for Windows

A classic native Windows application that organises downloaded files using your
CleanDL server's settings. Built with Rust and Win32 common controls; no browser
runtime or separate CLI installation is needed.

## Build and run

Requirements: Windows 10/11 x64, Rust with the MSVC target, and Visual Studio
Build Tools with Desktop development with C++ and a Windows SDK.

```powershell
cargo build --release
.\target\release\cleandlwin.exe
```

The release executable is portable. Copy it to a folder of your choice; settings
are stored separately under `%LOCALAPPDATA%\CleanDLWin`. The application runs
with your normal Windows permissions.

## First run

On first launch, CleanDL offers to import the URL and API key from the CLI's
`%LOCALAPPDATA%\cleandl\Logs` directory if present. This is a one-time copy;
the CLI's files are never updated by this app.

In **Tools > Settings**, enter the server API root (normally
`https://cleandl.chevybeef.com/api/v1`) and a connection key. **Get a connection
key** opens your server's login page. **Test connection** checks the account.
The saved key is protected using Windows DPAPI for the current Windows account.

Use **Help > Demo preview** to try the app without an account or connection.
Demo uses example rules, always disables Apply, and defaults to the Downloads
folder and an example `\\truenas\downloads` destination. To choose different
folders, clear **Use server folder defaults** in the main window.

## Preview and Apply

1. Choose your source and destination, or keep server folder defaults.
2. Select **Preview** (F5). Review each file's action, destination, and status.
3. Select **Apply** (Ctrl+Enter) and confirm the move/recycle counts.
4. Watch the current file's byte progress and overall file count in the status
   bar. The list records outcomes. **Cancel** (Esc) stops work safely.

Preview scans regular files directly in the source folder, without recursion.
Delete rules take priority over ignore rules. Other files use their extension's
ordered filename rules or default destination. Date folders use modification
time in UTC. Existing same-size destinations are skipped, matching the CLI's
behaviour (this is a size check, not a content comparison); other name collisions
get numeric suffixes in the preview.

Apply processes all actionable rows. Files added after preview wait for the next
preview. Changed/unavailable sources and destinations occupied since preview are
reported without overwriting anything. Changing connection or folder settings
invalidates the preview. After Apply or cancellation, preview again to run more
actions.

Moves copy sequentially to an exclusively created final destination, preserve
access/modification times where supported, then remove the source. Sources are
locked against writing and replacement during copying. Failed/cancelled copies
keep the source and attempt to remove their partial destination; cleanup failures
appear in the result and log. A forced process termination can leave a partial
destination; inspect it before retrying. Successfully completed earlier operations
are not rolled back when a run is cancelled.

Delete rules **recycle** files. Windows Shell's permanent-delete fallback is
rejected; unavailable recycling is reported as a failure. No permanent deletion
option is provided. Cancel waits for an in-flight Shell operation or network
request to return (requests have a ten-second timeout).

Server trial-run settings are honoured by default. Settings can force trial-only
mode or explicitly allow Apply after confirmation. Demo always remains trial-only.

## Server settings and offline use

The app uses the same `/api/v1` client API as `cleandl-rs`: account, configuration
version, settings, global rules, and selective extension resolution. Use
**Tools > Manage Rules** to edit rules on the server.

A successful preview saves a separate cache. When the server is unavailable, a
matching cache enables preview and Apply requires explicit cached-settings
confirmation. A different server/key cannot reuse the cache. Authentication
failures stop the operation. Offline extensions without saved mappings are skipped.

With **Use server folder defaults** checked, the folder fields show the matching
cached settings on launch; Preview refreshes them from the server. Local folder
overrides are saved separately and reappear when the checkbox is cleared. An old
override in `settings.json` does not replace the server destination in `cache.json`.

Local files:

| File | Contents |
| --- | --- |
| `settings.json` | Preferences and DPAPI-encrypted key |
| `cache.json` | Last successful server settings and resolved rules |
| `cleandlwin.log` | Preview summaries, outcomes, warnings and optional debug phases |

Open the log using **Tools > Open Log**. Keys are not written to the log.

## Development

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Opt-in Windows desktop smoke tests (briefly open a test window and recycle a
disposable file):

```powershell
cargo test -- --ignored --test-threads=1
```

These also capture the main and settings windows under `target/*-smoke.bmp`.

Tests use temporary folders and mock HTTP servers. Normal automated tests do not
recycle real files. Manual UI validation should use disposable files, including
a file matching a delete rule, and check that it can be restored from the Recycle
Bin. Test UNC destinations using a share accessible to the current account.

The GUI owns native handles on the main thread. Workers send typed events for
phases, copy progress and results; progress updates are throttled. The engine
separates immutable plans from execution. Shell/DPAPI and file-handle operations
are isolated in the platform module.

This first version is an interactive app. It has no scheduler, tray/background
mode, rule editor, installer, or self-updater.

## Licence

MIT. Sorting and API behaviour adapted from `cleandl-rs`, copyright 2026 CleanDL
contributors. See [LICENSE](LICENSE).
